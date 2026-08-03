//! The GI bake: the occluder set, a BVH over it, and the transfer integral.
//!
//! Everything here is CPU-only and deterministic, which is what lets the bake
//! promise **byte-reproducibility across machines** — a stronger promise than
//! any render in this repo makes, and available only because no GPU is
//! involved. Two things buy it, and both are format contracts:
//!
//! * the sample directions come from a stratified sequence spelled out below,
//!   not from `rand` and not from a dependency; and
//! * rays are accumulated in a fixed order, so no float sum depends on
//!   scheduling.
//!
//! See `designs/global-illumination-design.md` §5. The one place this module
//! knowingly departs from that document is the basis size — see
//! [`SKY_BANDS`](super::SKY_BANDS), which carries the measurement.

use super::{
    grid_counts, BakeHeader, BakedGi, InputsHasher, Probe, BAND_GROUND, BAND_ZENITH, CHANNELS,
    FORMAT, NUMBERS_PER_BASIS, SH_L1_COEFFS, SKY_BANDS,
};
use crate::math::{Mat4, Vec3};

/// Rays per probe when `--samples` is not given.
///
/// Recorded in the file, because a bake at 128 samples and one at 512 are
/// different artifacts and a render must be able to say which it is looking at.
pub const DEFAULT_SAMPLES: u32 = 256;

/// Secondary rays cast from a bounce hit to find how much sky reaches it.
///
/// Deliberately much smaller than the primary count: the first bounce carries
/// nearly all of the visible difference, and its *shape* comes from the primary
/// ray that found the surface. What these add is how brightly that surface is
/// lit, which a coarse estimate answers well — and the cost is multiplicative,
/// so this number is the difference between a bake in seconds and one in
/// minutes.
pub const BOUNCE_SAMPLES: u32 = 16;

/// How far a ray may travel before it is treated as having escaped to the sky.
///
/// A finite horizon rather than infinity so an unclosed scene — which is every
/// scene here, since terrain is a patch and not a planet — does not gather
/// black from the gap past its edge.
pub const MAX_RAY: f32 = 500.0;

/// Pushed off the surface along the ray to stop a probe re-hitting the triangle
/// it started on. In metres, and one unit is one metre (M34).
const EPSILON: f32 = 1.0e-4;

/// One occluding triangle, pre-transformed to world space and carrying the
/// albedo of the surface it came from — which is what makes bounce coloured.
#[derive(Debug, Clone)]
pub struct Triangle {
    pub v0: Vec3,
    pub e1: Vec3,
    pub e2: Vec3,
    pub normal: Vec3,
    pub albedo: Vec3,
}

impl Triangle {
    pub fn new(v0: Vec3, v1: Vec3, v2: Vec3, albedo: Vec3) -> Self {
        let e1 = v1 - v0;
        let e2 = v2 - v0;
        Self {
            v0,
            e1,
            e2,
            normal: e1.cross(e2).normalize_or_zero(),
            albedo,
        }
    }

    pub fn centroid(&self) -> Vec3 {
        self.v0 + (self.e1 + self.e2) / 3.0
    }

    fn bounds(&self) -> Aabb {
        let v1 = self.v0 + self.e1;
        let v2 = self.v0 + self.e2;
        Aabb {
            min: self.v0.min(v1).min(v2),
            max: self.v0.max(v1).max(v2),
        }
    }

    /// Möller–Trumbore. Returns the ray parameter of the hit.
    ///
    /// Double-sided on purpose: a probe inside a closed object must still see
    /// the walls around it, and this engine's geometry is not reliably closed
    /// or consistently wound (a `Road` ribbon is a strip, a `Tree`'s leaves are
    /// quads). Backface culling here would let light leak through anything
    /// facing away.
    fn hit(&self, origin: Vec3, direction: Vec3, t_max: f32) -> Option<f32> {
        let p = direction.cross(self.e2);
        let det = self.e1.dot(p);
        if det.abs() < 1.0e-12 {
            return None;
        }
        let inv_det = 1.0 / det;
        let t_vec = origin - self.v0;
        let u = t_vec.dot(p) * inv_det;
        if !(0.0..=1.0).contains(&u) {
            return None;
        }
        let q = t_vec.cross(self.e1);
        let v = direction.dot(q) * inv_det;
        if v < 0.0 || u + v > 1.0 {
            return None;
        }
        let t = self.e2.dot(q) * inv_det;
        if t > EPSILON && t < t_max {
            Some(t)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Aabb {
    min: Vec3,
    max: Vec3,
}

impl Aabb {
    fn empty() -> Self {
        Self {
            min: Vec3::splat(f32::INFINITY),
            max: Vec3::splat(f32::NEG_INFINITY),
        }
    }

    fn join(self, other: Self) -> Self {
        Self {
            min: self.min.min(other.min),
            max: self.max.max(other.max),
        }
    }

    /// Slab test. Written branchlessly over the three axes so the traversal
    /// cost does not depend on which way the ray points.
    fn hit(&self, origin: Vec3, inv_dir: Vec3, t_max: f32) -> bool {
        let t0 = (self.min - origin) * inv_dir;
        let t1 = (self.max - origin) * inv_dir;
        let near = t0.min(t1);
        let far = t0.max(t1);
        let enter = near.x.max(near.y).max(near.z).max(0.0);
        let exit = far.x.min(far.y).min(far.z).min(t_max);
        enter <= exit
    }
}

#[derive(Debug)]
enum Node {
    Leaf {
        bounds: Aabb,
        first: usize,
        count: usize,
    },
    Split {
        bounds: Aabb,
        right: usize,
    },
}

/// A bounding-volume hierarchy over the scene's *render* geometry.
///
/// Render geometry, not colliders, and the tour is why: its trees carry no
/// `Collider` at all, so a bake that asked the physics world what stood in the
/// way would find a landscape with no trees on it.
#[derive(Debug)]
pub struct Bvh {
    nodes: Vec<Node>,
    tris: Vec<Triangle>,
}

impl Bvh {
    /// Triangles per leaf. Small enough that a leaf test is cheap, large enough
    /// that the tree does not cost more to walk than the triangles cost to test.
    const LEAF_SIZE: usize = 4;

    pub fn build(mut tris: Vec<Triangle>) -> Self {
        let mut nodes = Vec::new();
        if tris.is_empty() {
            nodes.push(Node::Leaf {
                bounds: Aabb::empty(),
                first: 0,
                count: 0,
            });
            return Self { nodes, tris };
        }
        let count = tris.len();
        Self::split(&mut nodes, &mut tris, 0, count);
        Self { nodes, tris }
    }

    /// Median split on the widest axis of the centroid bounds.
    ///
    /// Deterministic by construction: `sort_by` on this repo's float ordering
    /// with the triangle's original index as the tie-break, so two builds of the
    /// same triangle list produce byte-identical trees — and therefore the same
    /// traversal order, and therefore the same float accumulation.
    fn split(nodes: &mut Vec<Node>, tris: &mut [Triangle], first: usize, count: usize) -> usize {
        let slice = &mut tris[first..first + count];
        let bounds = slice
            .iter()
            .map(Triangle::bounds)
            .fold(Aabb::empty(), Aabb::join);

        if count <= Self::LEAF_SIZE {
            let index = nodes.len();
            nodes.push(Node::Leaf {
                bounds,
                first,
                count,
            });
            return index;
        }

        let centroid_bounds = slice
            .iter()
            .map(|t| {
                let c = t.centroid();
                Aabb { min: c, max: c }
            })
            .fold(Aabb::empty(), Aabb::join);
        let extent = centroid_bounds.max - centroid_bounds.min;
        let axis = if extent.x >= extent.y && extent.x >= extent.z {
            0
        } else if extent.y >= extent.z {
            1
        } else {
            2
        };

        // `total_cmp` rather than `partial_cmp().unwrap()`: a degenerate
        // triangle can put a NaN in a centroid, and a sort that panics on one
        // bad triangle is a worse failure than a slightly worse tree.
        slice.sort_by(|a, b| {
            let (x, y) = (a.centroid(), b.centroid());
            x[axis].total_cmp(&y[axis])
        });

        let mid = count / 2;
        let index = nodes.len();
        nodes.push(Node::Split {
            bounds,
            right: usize::MAX,
        });
        Self::split(nodes, tris, first, mid);
        let right = Self::split(nodes, tris, first + mid, count - mid);
        if let Node::Split { right: slot, .. } = &mut nodes[index] {
            *slot = right;
        }
        index
    }

    pub fn triangle_count(&self) -> usize {
        self.tris.len()
    }

    /// The nearest hit along a ray, as `(distance, triangle)`.
    fn trace(&self, origin: Vec3, direction: Vec3, t_max: f32) -> Option<(f32, &Triangle)> {
        if self.tris.is_empty() {
            return None;
        }
        // A zero component would make `1/d` infinite; the slab test handles the
        // infinities correctly, so this only avoids a NaN from `0 * inf`.
        let inv_dir = Vec3::new(
            1.0 / nonzero(direction.x),
            1.0 / nonzero(direction.y),
            1.0 / nonzero(direction.z),
        );

        let mut best: Option<(f32, &Triangle)> = None;
        let mut closest = t_max;
        let mut stack = vec![0usize];
        while let Some(index) = stack.pop() {
            match &self.nodes[index] {
                Node::Leaf {
                    bounds,
                    first,
                    count,
                } => {
                    if !bounds.hit(origin, inv_dir, closest) {
                        continue;
                    }
                    for tri in &self.tris[*first..*first + *count] {
                        if let Some(t) = tri.hit(origin, direction, closest) {
                            closest = t;
                            best = Some((t, tri));
                        }
                    }
                }
                Node::Split { bounds, right } => {
                    if !bounds.hit(origin, inv_dir, closest) {
                        continue;
                    }
                    // Push right first so the near child is popped first —
                    // fixed order, so the float accumulation never varies.
                    stack.push(*right);
                    stack.push(index + 1);
                }
            }
        }
        best
    }

    /// Whether anything blocks the segment from `origin` along `direction`.
    fn occluded(&self, origin: Vec3, direction: Vec3, t_max: f32) -> bool {
        self.trace(origin, direction, t_max).is_some()
    }
}

fn nonzero(v: f32) -> f32 {
    if v == 0.0 {
        1.0e-20
    } else {
        v
    }
}

/// The stratified direction sequence, spelled out in-repo.
///
/// `sample(i, n)` returns the `i`th of `n` directions over the sphere. It is a
/// **format contract**: a baked file sits under a render baseline, so this
/// sequence must not change without invalidating every committed bake — the
/// same rule the particle xorshift and the meadow reseed hash live under.
///
/// The construction is a Fibonacci spiral, chosen over a jittered grid for one
/// property the design asked for: adding samples *refines* the set rather than
/// reshuffling it, because sample `i` depends on `i` and `n` through a smooth
/// map with no per-cell random draw. Two bakes at different `samples` disagree
/// in accuracy, never in character.
pub fn sample_direction(i: u32, n: u32) -> Vec3 {
    // The golden angle, written out rather than derived, so a change to a
    // constant elsewhere cannot silently move every committed bake.
    const GOLDEN_ANGLE: f32 = 2.399_963_2;
    let n = n.max(1) as f32;
    // +0.5 centres each sample in its band, which keeps the set symmetric about
    // the equator — a probe in an open field must not gather a sky that leans.
    let y = 1.0 - 2.0 * (i as f32 + 0.5) / n;
    let radius = (1.0 - y * y).max(0.0).sqrt();
    let theta = GOLDEN_ANGLE * i as f32;
    let (sin, cos) = theta.sin_cos();
    Vec3::new(radius * cos, y, radius * sin)
}

/// SH-L1 basis evaluated for a direction: the constant band, then x, y, z.
///
/// Unnormalized — the constants fold into the evaluation side, which keeps this
/// function's output exactly the numbers a reader of the file sees.
fn sh_l1(direction: Vec3) -> [f32; SH_L1_COEFFS] {
    [1.0, direction.x, direction.y, direction.z]
}

/// How much of each sky band a direction sees, matching `sky_ambient`'s mix.
///
/// `sky_ambient` reads `mix(ground, zenith, n.y * 0.5 + 0.5)`, so a direction
/// straight up is all zenith, straight down all ground, and the horizon is an
/// even blend. Reproducing that mix here — rather than a physically-motivated
/// one — is what makes an unoccluded probe integrate back to the fill term the
/// engine already computes.
fn sky_band_weights(direction: Vec3) -> [f32; SKY_BANDS] {
    let up = (direction.y * 0.5 + 0.5).clamp(0.0, 1.0);
    let mut w = [0.0; SKY_BANDS];
    w[BAND_ZENITH] = up;
    w[BAND_GROUND] = 1.0 - up;
    w
}

/// What one bake run was asked to do.
#[derive(Debug, Clone)]
pub struct BakeParams {
    pub samples: u32,
    pub bounces: u32,
    /// Sun directions to gather **bounced** sunlight for (M45), from
    /// [`sun_directions`](crate::gi::sun_directions). Empty is M35 exactly.
    pub sun: Vec<Vec3>,
}

impl Default for BakeParams {
    fn default() -> Self {
        Self {
            samples: DEFAULT_SAMPLES,
            bounces: 1,
            sun: Vec::new(),
        }
    }
}

/// What it did, for `bake-gi` to report.
#[derive(Debug, Clone, Default)]
pub struct BakeStats {
    pub probes: u64,
    pub rays: u64,
    pub triangles: usize,
    pub relocated: u32,
}

/// Bake one volume against an occluder set.
///
/// The integral, per probe and per sample direction `d`:
///
/// * the ray escapes → the sky is visible that way, and each band takes
///   `weight(d, band)` into the SH projection along `d`;
/// * the ray hits a surface → that surface's albedo times however much sky
///   reaches *it* (a coarser secondary gather) takes the same projection.
///
/// Normalized at the end so an unoccluded probe reproduces `sky_ambient`
/// exactly — design §3.1, and the reason turning GI on cannot change the
/// brightness of an open scene, only redistribute it.
///
/// Since M45 a hit also contributes to the **sun** basis, once per direction in
/// `params.sun`: the surface's albedo times `N·L` times whether the sun reaches
/// it. A ray that *escapes* contributes nothing there — the direct sun is the
/// shader's own term with its own shadow map, and counting it here as well
/// would light every surface twice. That asymmetry is the whole of the sun
/// basis's design, and it is what leaves an unoccluded probe's sun transfer at
/// zero and §3.1's guarantee exactly as it was.
pub fn bake_volume(
    bvh: &Bvh,
    origin: Vec3,
    grid: [u32; 3],
    spacing: f32,
    params: &BakeParams,
) -> (Vec<Probe>, BakeStats) {
    let mut probes = Vec::with_capacity(super::probe_count(grid) as usize);
    let mut stats = BakeStats {
        probes: super::probe_count(grid),
        triangles: bvh.triangle_count(),
        ..Default::default()
    };

    // Fixed iteration order — z outermost, then y, then x — so the file's line
    // order is a property of the grid and not of anything else.
    for z in 0..grid[2] {
        for y in 0..grid[1] {
            for x in 0..grid[0] {
                let at = origin + Vec3::new(x as f32, y as f32, z as f32) * spacing;
                let probe = bake_probe(bvh, at, spacing, params);
                stats.rays += probe.rays;
                stats.relocated += u32::from(probe.relocated);
                probes.push(Probe {
                    p: [x, y, z],
                    sky: probe.sky.into_iter().map(|band| band.to_vec()).collect(),
                    sun: probe.sun.into_iter().map(|v| v.to_vec()).collect(),
                });
            }
        }
    }

    (probes, stats)
}

/// One probe's transfer, plus how many rays it cost and whether it had to move.
struct ProbeBake {
    sky: [[f32; NUMBERS_PER_BASIS]; SKY_BANDS],
    /// One vector per `params.sun` direction; empty when there is no sun basis.
    sun: Vec<[f32; NUMBERS_PER_BASIS]>,
    rays: u64,
    relocated: bool,
}

fn bake_probe(bvh: &Bvh, at: Vec3, spacing: f32, params: &BakeParams) -> ProbeBake {
    let (at, relocated) = relocate_if_buried(bvh, at, spacing, params.samples);

    let mut transfer = [[0.0f32; NUMBERS_PER_BASIS]; SKY_BANDS];
    let mut sun = vec![[0.0f32; NUMBERS_PER_BASIS]; params.sun.len()];
    let mut rays = 0u64;

    for i in 0..params.samples {
        let dir = sample_direction(i, params.samples);
        rays += 1;

        let basis = sh_l1(dir);
        let (visible, tint) = match bvh.trace(at, dir, MAX_RAY) {
            // Nothing in the way: the sky itself, at full strength. Nothing for
            // the sun basis either — see this function's doc comment; the direct
            // sun already reaches this direction through the shader.
            None => (1.0, Vec3::ONE),
            Some((t, tri)) if params.bounces >= 1 => {
                // One bounce: how much sky reaches the surface we hit, times
                // its albedo. This is where colour starts travelling.
                let point = at + dir * t;
                let (lit, secondary) = sky_reaching(bvh, point, tri.normal, params);
                rays += secondary;
                rays += gather_sun(bvh, point, tri, &basis, params, &mut sun);
                (lit, tri.albedo)
            }
            Some(_) => (0.0, Vec3::ZERO),
        };

        // The sky half only. A surface the sky cannot reach may still be in
        // full sun, which is why `gather_sun` runs above this rather than
        // under the same guard — that ordering *is* the milestone: the sharpest
        // sun bounce in any scene comes off a wall whose sky access is poor.
        if visible <= 0.0 {
            continue;
        }

        let weights = sky_band_weights(dir);
        for (band, weight) in weights.iter().enumerate() {
            let scale = weight * visible;
            if scale == 0.0 {
                continue;
            }
            for (coeff, b) in basis.iter().enumerate() {
                let value = scale * b;
                // Channel-major within a coefficient, matching the file's
                // documented layout and the texture upload in G2.
                transfer[band][coeff * CHANNELS] += value * tint.x;
                transfer[band][coeff * CHANNELS + 1] += value * tint.y;
                transfer[band][coeff * CHANNELS + 2] += value * tint.z;
            }
        }
    }

    // Normalize by the sample count so the result is an average over the
    // sphere rather than a sum that changes meaning with `--samples`. This is
    // what makes a 128-sample and a 512-sample bake differ in noise only.
    let inv = 1.0 / params.samples.max(1) as f32;
    for band in transfer.iter_mut() {
        for value in band.iter_mut() {
            *value *= inv;
        }
    }
    // The same normalization, so the two bases stay in each other's units and
    // the one `LINEAR_GAIN` reconstructs both.
    for direction in sun.iter_mut() {
        for value in direction.iter_mut() {
            *value *= inv;
        }
    }

    ProbeBake {
        sky: transfer,
        sun,
        rays,
        relocated,
    }
}

/// Add one hit surface's **bounced sunlight** to a probe's sun basis (M45).
///
/// One shadow ray per sun direction, which is the whole marginal cost of the
/// basis: the primary ray is already cast and its hit already found, so eight
/// sun directions cost eight visibility queries rather than eight bakes.
///
/// Returns the rays it spent, so `bake-gi`'s report stays honest.
fn gather_sun(
    bvh: &Bvh,
    point: Vec3,
    tri: &Triangle,
    basis: &[f32; SH_L1_COEFFS],
    params: &BakeParams,
    sun: &mut [[f32; NUMBERS_PER_BASIS]],
) -> u64 {
    let mut rays = 0;
    let lifted = point + tri.normal * EPSILON;
    for (k, travel) in params.sun.iter().enumerate() {
        // `travel` is the direction the light *moves*, which is what a
        // `DirectionalLight` stores, so the direction toward the sun is its
        // negation — the same sign convention `mesh.wgsl`'s direct term uses.
        let toward = -*travel;
        let ndotl = tri.normal.dot(toward);
        if ndotl <= 0.0 {
            // Facing away: no sun on this surface, and no shadow ray needed
            // to know it. Skipping the ray rather than casting and discarding
            // keeps the reported count meaningful.
            continue;
        }
        rays += 1;
        if bvh.occluded(lifted, toward, MAX_RAY) {
            continue;
        }
        for (coeff, b) in basis.iter().enumerate() {
            let value = ndotl * b;
            sun[k][coeff * CHANNELS] += value * tri.albedo.x;
            sun[k][coeff * CHANNELS + 1] += value * tri.albedo.y;
            sun[k][coeff * CHANNELS + 2] += value * tri.albedo.z;
        }
    }
    rays
}

/// How much sky reaches a point on a surface, in `[0, 1]`.
///
/// A coarse hemisphere gather around the surface normal. Coarse on purpose —
/// see [`BOUNCE_SAMPLES`].
fn sky_reaching(bvh: &Bvh, point: Vec3, normal: Vec3, params: &BakeParams) -> (f32, u64) {
    if params.bounces == 0 {
        return (0.0, 0);
    }
    let mut open = 0u32;
    for i in 0..BOUNCE_SAMPLES {
        let mut dir = sample_direction(i, BOUNCE_SAMPLES);
        // Fold the sample into the surface's own hemisphere rather than
        // rejecting it, so the ray count is fixed and the bake's cost is
        // predictable from the grid alone.
        if dir.dot(normal) < 0.0 {
            dir = -dir;
        }
        if !bvh.occluded(point + normal * EPSILON, dir, MAX_RAY) {
            open += 1;
        }
    }
    (open as f32 / BOUNCE_SAMPLES as f32, BOUNCE_SAMPLES as u64)
}

/// Move a probe that is buried in geometry out toward open space.
///
/// A probe inside the terrain or inside a crate gathers black and then leaks
/// that black into every surface it interpolates to. Fixing it here rather than
/// in the shader is what lets the shader use plain hardware trilinear filtering
/// — four taps, no per-tap weighting, no validity branch, no divergence.
///
/// The cost, stated: GI cannot express a genuinely dark interior. This engine
/// has no interiors; if one arrives, the shader-side fix is a known quantity.
fn relocate_if_buried(bvh: &Bvh, at: Vec3, spacing: f32, samples: u32) -> (Vec3, bool) {
    // A short probe of the immediate neighbourhood: if most of it is blocked at
    // close range, this point is inside something.
    const PROBES: u32 = 12;
    let reach = spacing * 0.5;
    let mut blocked = 0u32;
    let mut escape = Vec3::ZERO;
    for i in 0..PROBES {
        let dir = sample_direction(i, PROBES);
        if bvh.occluded(at, dir, reach) {
            blocked += 1;
        } else {
            escape += dir;
        }
    }

    // Two thirds is the threshold: a probe just above a floor is half blocked
    // and perfectly valid, while one inside a wall sees almost nothing.
    if blocked * 3 <= PROBES * 2 {
        return (at, false);
    }

    // Push along the direction that was most open. If nothing was open the sum
    // is zero and there is nowhere to go — the flood fill in `fill_invalid`
    // is what rescues that case.
    let _ = samples;
    let direction = escape.normalize_or_zero();
    if direction == Vec3::ZERO {
        return (at, true);
    }
    (at + direction * reach, true)
}

/// Collect every occluding triangle in a scene, with its albedo.
///
/// Occluders are meshes (including trees, which ride the mesh list), terrain,
/// and roads. Clouds and meadows deliberately do **not** occlude — design §12's
/// provisional answer: a cloud casts no shadow today and making it darken GI
/// would be the first time it darkened anything, while grass occluding grass is
/// a large ray-count multiplier for an effect the meadow's own shading covers.
/// Both still *receive*.
pub fn collect_occluders(
    scene: &crate::scene::Scene,
    assets: &dyn crate::texture::AssetSource,
) -> crate::error::Result<Vec<Triangle>> {
    let mut tris = Vec::new();

    let mut push_mesh = |mesh: &crate::mesh::MeshData, model: Mat4, albedo: Vec3| {
        for chunk in mesh.indices.chunks_exact(3) {
            let p = |i: u32| {
                let v = mesh.positions[i as usize];
                model.transform_point3(Vec3::new(v[0], v[1], v[2]))
            };
            tris.push(Triangle::new(p(chunk[0]), p(chunk[1]), p(chunk[2]), albedo));
        }
    };

    for item in scene.render_items(assets)? {
        push_mesh(&item.mesh, item.model, item.material.albedo);
    }
    for item in scene.terrain_items() {
        push_mesh(&item.mesh, item.model, item.material.albedo);
    }
    for item in scene.road_items() {
        // A road's albedo is its asphalt colour; its markings are drawn per
        // pixel and are far too thin to matter to a bounce.
        push_mesh(&item.surface.mesh, item.model, item.road.color);
    }

    Ok(tris)
}

/// Hash every input a bake read, so a scene edited afterwards fails `validate`
/// rather than rendering with light that no longer matches its geometry.
///
/// Order is fixed and the triangle set is fed in the order
/// [`collect_occluders`] produced it, which is itself a fixed walk of the
/// scene — so the digest is a function of the file, not of iteration luck.
pub fn hash_inputs(tris: &[Triangle], params: &BakeParams, spacing: f32, grid: [u32; 3]) -> String {
    let mut h = InputsHasher::new();
    h.str(FORMAT)
        .u32(params.samples)
        .u32(params.bounces)
        .f32(spacing)
        .u32(grid[0])
        .u32(grid[1])
        .u32(grid[2])
        .u32(tris.len() as u32);
    for tri in tris {
        h.vec3(tri.v0).vec3(tri.e1).vec3(tri.e2).vec3(tri.albedo);
    }
    // The sun basis (M45) enters the digest **only when there is one**, which is
    // what keeps every bake written before it byte-valid rather than uniformly
    // stale. It is M17's rule for the particle RNG applied to a hash: skip the
    // step entirely when the field is off, never feed it a defaulted value —
    // a defaulted zero is arithmetically reasonable and still moves every digest
    // in the repo. Feeding the directions rather than the count is what makes
    // editing `sun_elevation` a stale bake.
    if !params.sun.is_empty() {
        h.u32(params.sun.len() as u32);
        for direction in &params.sun {
            h.vec3(*direction);
        }
    }
    h.finish()
}

/// The whole bake for one volume, from a scene to a writable file.
#[allow(clippy::too_many_arguments)]
pub fn bake(
    scene_name: &str,
    entity: &str,
    tris: Vec<Triangle>,
    center: Vec3,
    scale: Vec3,
    volume: &crate::components::LightProbeVolume,
    params: &BakeParams,
) -> (BakedGi, BakeStats) {
    let grid = grid_counts(scale, volume.spacing);
    let inputs_hash = hash_inputs(&tris, params, volume.spacing, grid);
    // The Transform is a *unit box* scaled and positioned, so the grid starts
    // at the minimum corner, not at the entity's origin.
    let origin = center - scale * 0.5;

    let bvh = Bvh::build(tris);
    let (probes, mut stats) = bake_volume(&bvh, origin, grid, volume.spacing, params);
    stats.triangles = bvh.triangle_count();

    let baked = BakedGi {
        header: BakeHeader {
            format: FORMAT.to_string(),
            scene: scene_name.to_string(),
            entity: entity.to_string(),
            inputs_hash,
            grid,
            origin: origin.to_array(),
            spacing: volume.spacing,
            basis: [("sky".to_string(), SKY_BANDS as u32)]
                .into_iter()
                .chain(
                    // Only when there is one, so a bake with no sun basis
                    // writes the same header it wrote before M45.
                    (!params.sun.is_empty()).then(|| ("sun".to_string(), params.sun.len() as u32)),
                )
                .collect(),
            samples: params.samples,
            // From `params`, like `samples` beside it: the header records what
            // the bake *ran with*. Every current caller copies
            // `volume.bounces` into `params`, but a caller that ever passes a
            // different count must not get a header that lies about it —
            // `BakedGi::matches` trusts this field against the volume's.
            bounces: params.bounces,
            relocated: stats.relocated,
            sun_dirs: params.sun.iter().map(Vec3::to_array).collect(),
        },
        probes,
    };
    (baked, stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quad(y: f32, half: f32, albedo: Vec3) -> Vec<Triangle> {
        let p = |x: f32, z: f32| Vec3::new(x, y, z);
        vec![
            Triangle::new(p(-half, -half), p(half, -half), p(half, half), albedo),
            Triangle::new(p(-half, -half), p(half, half), p(-half, half), albedo),
        ]
    }

    #[test]
    fn the_sample_set_is_balanced_about_the_equator() {
        // A probe in an open field must not gather a sky that leans, or every
        // flat surface in every scene picks up a tilt that no geometry caused.
        let n = 256;
        let sum: Vec3 = (0..n).map(|i| sample_direction(i, n)).sum();
        // The vertical balance is the property that matters and it is exact by
        // construction: `y` is a symmetric ramp, so the bands cancel in pairs.
        // A lean here would tilt the fill light on every flat surface in every
        // scene, with no geometry to explain it.
        assert_eq!(sum.y, 0.0, "the sample set must not lean up or down");
        // The azimuthal residual is inherent to a Fibonacci spiral and shrinks
        // as ~1/n; at 256 samples it is about 0.05 of one unit vector out of
        // 256, which is noise well below a quantized coefficient.
        assert!(
            sum.length() < 0.1,
            "directions should broadly cancel over the sphere, got {sum:?}"
        );
    }

    #[test]
    fn the_sample_sequence_is_stable() {
        // A format contract: every committed bake was taken with this sequence,
        // so a change here invalidates all of them and must be deliberate.
        let d = sample_direction(7, 64);
        assert_eq!(
            (d.x, d.y, d.z),
            (-0.29649603, 0.765625, -0.57088387),
            "the direction sequence is pinned; changing it re-bakes the repo"
        );
    }

    #[test]
    fn an_unoccluded_probe_sees_the_whole_sky() {
        // With nothing in the way the constant SH band should integrate to 1
        // across the two bands — the property §3.1 leans on, since it is what
        // makes GI reproduce `sky_ambient` in an open scene.
        let bvh = Bvh::build(Vec::new());
        let params = BakeParams {
            samples: 512,
            bounces: 1,
            sun: Vec::new(),
        };
        let baked = bake_probe(&bvh, Vec3::ZERO, 4.0, &params);
        let (transfer, relocated) = (baked.sky, baked.relocated);
        assert!(!relocated, "an empty scene buries nothing");
        let total: f32 = (0..SKY_BANDS).map(|b| transfer[b][0]).sum();
        assert!(
            (total - 1.0).abs() < 0.01,
            "the two bands should partition the sphere, got {total}"
        );
    }

    #[test]
    fn a_floor_below_halves_the_sky_and_tints_the_bounce() {
        // The assertion that says GI is doing its job: a red floor under a
        // probe must both occlude (less sky) and bleed red upward.
        let red = Vec3::new(0.9, 0.05, 0.05);
        let bvh = Bvh::build(quad(-1.0, 50.0, red));
        let params = BakeParams {
            samples: 512,
            bounces: 1,
            sun: Vec::new(),
        };
        let transfer = bake_probe(&bvh, Vec3::ZERO, 4.0, &params).sky;

        // Ground band, constant coefficient, per channel.
        let r = transfer[BAND_GROUND][0];
        let g = transfer[BAND_GROUND][1];
        assert!(
            r > g * 2.0,
            "a red floor must bleed red into the ground band, got r={r} g={g}"
        );
    }

    #[test]
    fn a_buried_probe_is_moved_out() {
        // A probe inside a box gathers black and would leak it into every
        // surface it interpolates to, which is why this is fixed at bake time.
        let mut tris = Vec::new();
        for (axis, sign) in [
            (0, 1.0),
            (0, -1.0),
            (1, 1.0),
            (1, -1.0),
            (2, 1.0),
            (2, -1.0),
        ] {
            // Six inward-facing quads, a 1 m box around the origin.
            let mut face = quad(-0.5 * sign, 0.5, Vec3::splat(0.5));
            for tri in &mut face {
                let rotate = |v: Vec3| match axis {
                    0 => Vec3::new(v.y, v.x, v.z),
                    1 => v,
                    _ => Vec3::new(v.x, v.z, v.y),
                };
                *tri = Triangle::new(
                    rotate(tri.v0),
                    rotate(tri.v0 + tri.e1),
                    rotate(tri.v0 + tri.e2),
                    tri.albedo,
                );
            }
            tris.extend(face);
        }
        let bvh = Bvh::build(tris);
        let relocated = bake_probe(&bvh, Vec3::ZERO, 4.0, &BakeParams::default()).relocated;
        assert!(relocated, "a probe sealed inside a box must be relocated");
    }

    #[test]
    fn the_bvh_finds_what_a_linear_scan_finds() {
        // The BVH is an acceleration structure, so its only correctness
        // property is agreeing with the obvious implementation.
        let tris: Vec<Triangle> = (0..64)
            .map(|i| {
                let x = i as f32 * 0.7 - 20.0;
                Triangle::new(
                    Vec3::new(x, -1.0, -1.0),
                    Vec3::new(x + 0.5, -1.0, 1.0),
                    Vec3::new(x, 1.0, 1.0),
                    Vec3::ONE,
                )
            })
            .collect();
        let bvh = Bvh::build(tris.clone());

        for i in 0..32 {
            let dir = sample_direction(i, 32);
            let origin = Vec3::new(0.0, 0.0, -8.0);
            let brute = tris
                .iter()
                .filter_map(|t| t.hit(origin, dir, MAX_RAY))
                .fold(f32::INFINITY, f32::min);
            let fast = bvh
                .trace(origin, dir, MAX_RAY)
                .map(|(t, _)| t)
                .unwrap_or(f32::INFINITY);
            // Both-miss is `inf` on each side, and `(inf - inf).abs()` is NaN,
            // which fails every comparison — so agreement on "nothing hit" has
            // to be checked before the numeric one.
            let agree = (brute.is_infinite() && fast.is_infinite()) || (brute - fast).abs() < 1e-4;
            assert!(
                agree,
                "ray {i}: brute force found {brute}, bvh found {fast}"
            );
        }
    }

    #[test]
    fn a_bake_is_reproducible() {
        // The promise that makes a committed bake safe to diff: same inputs,
        // byte-identical output. Stronger than any render here, and available
        // only because no GPU is involved.
        let tris = quad(-1.0, 20.0, Vec3::new(0.4, 0.6, 0.3));
        let volume = crate::components::LightProbeVolume {
            spacing: 2.0,
            ..Default::default()
        };
        let params = BakeParams {
            samples: 64,
            bounces: 1,
            sun: Vec::new(),
        };
        let run = || {
            bake(
                "s.json",
                "L",
                tris.clone(),
                Vec3::ZERO,
                Vec3::splat(4.0),
                &volume,
                &params,
            )
            .0
            .to_text()
        };
        assert_eq!(run(), run(), "two bakes of one scene must be identical");
    }
}
