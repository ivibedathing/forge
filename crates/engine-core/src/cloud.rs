//! Procedural cloud geometry (M20).
//!
//! A [`Cloud`] component is a recipe, not a mesh reference — the M19 premise,
//! applied to the sky. This module turns the recipe into one mesh, CPU-side and
//! GPU-free, so the whole generator unit-tests without an adapter.
//!
//! # The model
//!
//! A cloud is a **cluster of lobes**, and each lobe grows smaller lobes on
//! itself. The base lobes are scattered over a golden-angle spiral in the unit
//! box's footprint, sized so the middle ones are the largest; each generation of
//! children is attached to a random point of its parent's surface, biased
//! upward by `rise`, and buried far enough in that the two surfaces
//! interpenetrate rather than meet. Every lobe's vertices are then displaced
//! radially by a smooth `wobble`, and everything below the base plane is folded
//! onto it by `flatten`.
//!
//! Three of those are the tree's rules transposed, and it is worth being
//! explicit about which:
//!
//! - Children are **seated inside** their parent, exactly as a branch is seated
//!   inside the branch that carries it. There is no CSG union in this engine and
//!   interpenetration costs nothing.
//! - Lobe size **falls off per generation**. Uniform lobes read as popcorn; what
//!   separates cauliflower from a bag of golf balls is that the silhouette has
//!   detail at more than one scale.
//! - The whole thing is grown in a **unit box** and sized by `Transform.scale`,
//!   like a water surface, so there is only one way to say how big a cloud is.
//!
//! What is *not* transposed is the tree's uprighting term. A tree needs one
//! because its trunk is a random walk that compounds; a cloud's lobes are each
//! placed independently from the same origin, so nothing here drifts.
//!
//! # Determinism
//!
//! One private xorshift, seeded from `Cloud::seed`, drives every draw in a fixed
//! order: the base lobes in index order, then each generation of children in
//! index order. The generator and its hash are spelled out in this repo, as
//! `particles.rs` and `tree.rs` spell theirs out, because the sequence is part
//! of what a scene file *means* and may not live somewhere a dependency upgrade
//! can change it.
//!
//! [`Cloud`]: crate::components::Cloud

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use glam::Vec3;

use crate::components::Cloud;
use crate::mesh::MeshData;

/// Grow a cloud's geometry, or hand back the copy already grown.
///
/// Keyed on the component's **geometry** fields alone — `color`, `density`,
/// `drift` and the rest reach the shader as uniforms and cannot change a
/// vertex, so two clouds that differ only in colour share one mesh and one GPU
/// upload. Sharing the `Arc` is not just an allocation saved: the renderer's
/// per-frame upload cache keys on `Arc` identity (M15), so handing back a fresh
/// copy each frame would re-upload every cloud in the sky every frame.
pub fn mesh_for(cloud: &Cloud) -> Arc<MeshData> {
    CLOUD_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(hit) = cache.get(&CloudKey::of(cloud)) {
            return Arc::clone(hit);
        }
        // Animating a shape parameter mints a new key every step, so the cache
        // is bounded rather than an incidental leak — `tree.rs`'s reasoning and
        // its three-line resolution.
        if cache.len() >= MAX_CACHED_CLOUDS {
            cache.clear();
        }
        let grown = Arc::new(generate(cloud));
        cache.insert(CloudKey::of(cloud), Arc::clone(&grown));
        grown
    })
}

/// Grow a cloud's geometry unconditionally. Pure — the cached [`mesh_for`] is
/// the one callers should use.
pub fn generate(cloud: &Cloud) -> MeshData {
    let unit = unit_sphere(cloud.detail);
    let mut builder = Builder {
        cloud,
        rng: seed_state(cloud.seed),
        unit: &unit,
        mesh: MeshData {
            positions: Vec::with_capacity(vertex_count(cloud) as usize),
            normals: Vec::with_capacity(vertex_count(cloud) as usize),
            uvs: Vec::with_capacity(vertex_count(cloud) as usize),
            indices: Vec::new(),
        },
    };

    builder.scatter();
    if cloud.flatten > 0.0 {
        builder.fold_base();
    }
    builder.mesh
}

/// Vertices this cloud would generate, computed from the parameters alone —
/// what validation checks before anything is allocated.
///
/// Exact rather than an estimate, so the budget error can name a real number.
pub fn vertex_count(cloud: &Cloud) -> u64 {
    lobe_count(cloud).saturating_mul(sphere_vertices(cloud.detail))
}

/// Lobes across every generation: `lobes × (1 + c + c² + … + c^levels)`.
pub fn lobe_count(cloud: &Cloud) -> u64 {
    let children = cloud.children as u64;
    let mut per_base = 0u64;
    let mut generation = 1u64;
    for _ in 0..=cloud.levels {
        per_base = per_base.saturating_add(generation);
        generation = generation.saturating_mul(children);
    }
    (cloud.lobes as u64).saturating_mul(per_base)
}

/// Vertices of one icosphere at `detail` subdivisions: `10 · 4^d + 2`.
fn sphere_vertices(detail: u32) -> u64 {
    10u64.saturating_mul(4u64.saturating_pow(detail.min(8))) + 2
}

/// Beyond this a cloud is a mistake in a parameter, not a plan — see
/// `cloud_too_complex`. `lobes: 32, levels: 3, children: 8` is 18,720 lobes,
/// so this ceiling is reachable by accident rather than only by malice.
pub const MAX_CLOUD_VERTICES: u64 = 100_000;

const MAX_CACHED_CLOUDS: usize = 256;

/// How far below the base plane a fully flattened cloud's lobes sink, as a
/// fraction of their radius. The fold then cuts them off there, which is what
/// makes the base a disc instead of a row of tangent spheres.
const BASE_SINK: f32 = 0.6;
/// How much of a child's radius is buried under its parent's surface.
const CHILD_OVERLAP: f32 = 0.45;
/// How much smaller the outermost base lobes are than the middle ones. A
/// cluster of equal spheres has no profile; this is what domes it.
const EDGE_FALLOFF: f32 = 0.45;
/// The golden angle, in degrees — the spiral the base lobes are placed on, for
/// the reason `Tree::branch_twist` uses it: a whole-number division stacks
/// them into rows the eye picks out immediately.
const GOLDEN_ANGLE: f32 = 137.5;
/// The cloud's base, in the unit box the geometry is grown in.
const BASE_PLANE: f32 = -0.5;
/// How many lobe *diameters* the height profile may lift the middle of the
/// cluster above its rim. Above roughly 1 the rings stop overlapping.
const DOME_STACK: f32 = 0.8;
/// How far each lobe's normal is bent from its own centre toward the cloud's.
/// See [`Builder::emit_lobe`] — this is what makes a cluster shade as one body.
const BODY_NORMAL: f32 = 0.55;

// ── the generator ──────────────────────────────────────────────────────────

struct Builder<'a> {
    cloud: &'a Cloud,
    rng: u32,
    unit: &'a MeshData,
    mesh: MeshData,
}

impl Builder<'_> {
    /// Place the base lobes and recurse into their children.
    fn scatter(&mut self) {
        let cloud = self.cloud;
        let lobes = cloud.lobes.max(1);
        let base_radius = (cloud.lobe_size * 0.5).max(1e-4);

        for index in 0..lobes {
            // A sunflower spiral over the footprint: `sqrt` spaces the rings so
            // the lobes cover the disc evenly instead of crowding the middle,
            // and index 0 lands dead centre, so a one-lobe cloud is a sphere at
            // the origin rather than a sphere pushed off to one side.
            let ring = (index as f32 / lobes as f32).sqrt();
            let angle = (GOLDEN_ANGLE * index as f32).to_radians();
            let spread = (0.5 - base_radius).max(0.0);

            // Middle lobes are the largest, which is half of what domes the
            // cluster. Nothing in the model knows what a cumulus is; this,
            // the height profile below, and `rise` are what draw one.
            let radius = base_radius * (1.0 - EDGE_FALLOFF * ring) * self.jitter_multiplier();
            let offset = spread * ring * self.jitter_multiplier();
            let x = angle.cos() * offset;
            let z = angle.sin() * offset;

            // Height is a *profile*, not a scatter: central lobes ride high,
            // rim lobes rest on the floor, and the seed does not enter. That is
            // deliberate — a random vertical scatter makes the difference
            // between a cloud and a smear a lottery the author cannot see they
            // are playing, which is the lesson `tree.rs` paid for with its
            // uprighting term.
            //
            // The rise is capped at [`DOME_STACK`] lobe diameters, and that cap
            // is what holds the cluster together: let the profile reach the top
            // of the box and the middle lobe floats clear of the ring around
            // it, which renders as a ball hovering over a wreath rather than as
            // a cloud. Consecutive rings have to *overlap*, so the whole
            // vertical span is bounded by the lobes' own size, and how far the
            // cluster fills a tall box is set by `lobe_size` — not by stretching
            // a fixed number of lobes to reach.
            //
            // `flatten` bends the profile, so more of the cluster crowds the
            // base plane; the fold afterwards is what makes that base flat.
            let low = BASE_PLANE + radius * (1.0 - BASE_SINK);
            let high = (low + DOME_STACK * base_radius * 2.0).min(0.5 - radius).max(low);
            let profile = (1.0 - ring).powf(1.0 + cloud.flatten);
            let y = lerp(low, high, profile) + self.jitter_signed() * (high - low) * 0.25;

            self.grow(0, Vec3::new(x, y, z), radius);
        }
    }

    /// Emit one lobe and everything piled on it.
    fn grow(&mut self, depth: u32, center: Vec3, radius: f32) {
        self.emit_lobe(center, radius);
        if depth >= self.cloud.levels || self.cloud.children == 0 {
            return;
        }

        let cloud = self.cloud;
        for _ in 0..cloud.children {
            // A random point of the parent's surface, pulled toward the sky by
            // `rise`. A cumulus is a convection cell: the detail is on top,
            // where the air is still rising, and the underside is smooth.
            // Scattering children isotropically makes a sea urchin.
            let direction = self.direction().normalize_or(Vec3::Y);
            let direction = (direction + Vec3::Y * cloud.rise * 2.0).normalize_or(Vec3::Y);

            let child_radius = (radius * cloud.lobe_ratio * self.jitter_multiplier()).max(1e-4);
            // Buried by a fraction of its own radius, so the two surfaces
            // interpenetrate. Invisible from outside, and far cheaper than the
            // union this engine does not have.
            let child_center = center + direction * (radius - child_radius * CHILD_OVERLAP);

            self.grow(depth + 1, child_center, child_radius);
        }
    }

    /// Append one lobe: the shared icosphere, scaled, displaced by `wobble`,
    /// and moved into place.
    fn emit_lobe(&mut self, center: Vec3, radius: f32) {
        // Three phases per lobe, so no two lobes wear the same dents even at
        // the same radius. Drawn whether or not `wobble` is on, keeping the
        // sequence independent of the parameter values (see the module doc).
        let phase = Vec3::new(
            self.unit_draw() * std::f32::consts::TAU,
            self.unit_draw() * std::f32::consts::TAU,
            self.unit_draw() * std::f32::consts::TAU,
        );
        let wobble = self.cloud.wobble;
        let base = self.mesh.positions.len() as u32;

        for (position, uv) in self.unit.positions.iter().zip(&self.unit.uvs) {
            let direction = Vec3::from_array(*position);
            let scale = 1.0 + wobble * dent(direction, phase);
            let vertex = center + direction * (radius * scale);
            self.mesh.positions.push(vertex.to_array());

            // Not the lobe's own radial normal, but that normal bent toward the
            // *cloud's*. This is the difference between a cloud and a pile of
            // spheres, and it is a shading fix rather than a geometry one for a
            // reason: light entering a cloud scatters through the whole body
            // before it leaves, so the underside of a lobe on top of the cloud
            // is not lit like the underside of a lobe sitting on its own. Pure
            // per-lobe normals draw every lobe as a separate ball with its own
            // terminator, which is exactly what the first render showed.
            //
            // The lobe normal is kept at `1 - BODY_NORMAL` so the relief does
            // not vanish; at 1.0 the cloud shades as a smooth blob and the
            // cauliflower silhouette stops being legible in the shading.
            let body = vertex.normalize_or(direction);
            let normal = direction.lerp(body, BODY_NORMAL).normalize_or(direction);
            self.mesh.normals.push(normal.to_array());
            self.mesh.uvs.push(*uv);
        }
        for index in &self.unit.indices {
            self.mesh.indices.push(base + index);
        }
    }

    /// Fold everything below the base plane onto it.
    ///
    /// A fold, not a clip: cutting the lobes would leave an open shell whose
    /// inside is visible the moment the camera rises above the base.
    fn fold_base(&mut self) {
        let flatten = self.cloud.flatten;
        for position in &mut self.mesh.positions {
            if position[1] < BASE_PLANE {
                position[1] = lerp(position[1], BASE_PLANE, flatten);
            }
        }
    }

    // ── randomness ─────────────────────────────────────────────────────

    fn unit_draw(&mut self) -> f32 {
        unit(&mut self.rng)
    }

    /// A multiplier in `[1 - jitter, 1 + jitter]`. Always consumes exactly one
    /// draw, even at `jitter: 0` — no cloud baseline predates this field, so
    /// keeping the draw sequence independent of the parameters is the simpler
    /// contract, exactly as it is for trees.
    fn jitter_multiplier(&mut self) -> f32 {
        1.0 + (self.unit_draw() * 2.0 - 1.0) * self.cloud.jitter
    }

    /// A signed offset in `[-jitter, jitter]`, for quantities that are added
    /// rather than scaled.
    fn jitter_signed(&mut self) -> f32 {
        (self.unit_draw() * 2.0 - 1.0) * self.cloud.jitter
    }

    /// A direction drawn uniformly over the sphere. Two draws, always.
    fn direction(&mut self) -> Vec3 {
        // Uniform in cos(theta), or the poles get crowded — which on a cloud
        // shows up as children stacking on the tops of their parents.
        let cos_theta = self.unit_draw() * 2.0 - 1.0;
        let azimuth = self.unit_draw() * std::f32::consts::TAU;
        let sin_theta = (1.0 - cos_theta * cos_theta).max(0.0).sqrt();
        Vec3::new(
            sin_theta * azimuth.cos(),
            cos_theta,
            sin_theta * azimuth.sin(),
        )
    }
}

/// The radial displacement of one lobe's surface at `direction`.
///
/// A product of three sines: smooth everywhere (so the lobe stays a lobe rather
/// than acquiring noise), bounded to `[-1, 1]` by construction, and a pure
/// function of the direction — which is what keeps the icosphere's shared
/// vertices agreeing with each other and the surface closed.
fn dent(direction: Vec3, phase: Vec3) -> f32 {
    (direction.x * 3.7 + phase.x).sin()
        * (direction.y * 4.3 + phase.y).sin()
        * (direction.z * 3.1 + phase.z).sin()
}

/// Written `(1-t)·a + t·b` rather than `a + (b-a)·t` so that both endpoints are
/// *exact*: `t = 1` must give `b` to the bit, or a fully flattened cloud does
/// not quite rest on its base plane and a test that says it does is a test that
/// depends on the seed.
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a * (1.0 - t) + b * t
}

// ── the unit lobe ──────────────────────────────────────────────────────────

/// A unit icosphere at `detail` subdivisions, shared across every lobe of every
/// cloud in the process.
///
/// Icosphere rather than the UV sphere `builtin:sphere` generates: a UV sphere
/// pinches at its poles, and a cloud lobe is seen from every angle with nothing
/// to hide the pinch. (`builtin:sphere` is the lighting probe, and its layout is
/// right for that job.)
fn unit_sphere(detail: u32) -> Arc<MeshData> {
    SPHERE_CACHE.with(|cache| {
        Arc::clone(
            cache
                .borrow_mut()
                .entry(detail)
                .or_insert_with(|| Arc::new(build_sphere(detail))),
        )
    })
}

fn build_sphere(detail: u32) -> MeshData {
    // The icosahedron's twelve vertices: the corners of three mutually
    // perpendicular golden rectangles.
    let phi = (1.0 + 5.0f32.sqrt()) * 0.5;
    let mut positions: Vec<Vec3> = Vec::with_capacity(12);
    for (a, b) in [(-1.0f32, phi), (1.0, phi), (-1.0, -phi), (1.0, -phi)] {
        positions.push(Vec3::new(a, b, 0.0));
        positions.push(Vec3::new(0.0, a, b));
        positions.push(Vec3::new(b, 0.0, a));
    }
    for position in &mut positions {
        *position = position.normalize();
    }

    // Faces, derived rather than tabulated. On a unit icosahedron every edge is
    // the same length and the next-nearest vertex is 60% further away, so a
    // face is exactly a mutually-adjacent triple — which is checkable, unlike
    // twenty transcribed index triples. Wound counter-clockwise seen from
    // outside.
    let mut indices: Vec<[u32; 3]> = Vec::new();
    for a in 0..positions.len() as u32 {
        for b in (a + 1)..positions.len() as u32 {
            for c in (b + 1)..positions.len() as u32 {
                let (pa, pb, pc) = (
                    positions[a as usize],
                    positions[b as usize],
                    positions[c as usize],
                );
                // On a unit icosahedron every edge is the same length, so a
                // face is any triple that is mutually adjacent. Deriving the
                // faces beats tabulating twenty triples nobody can proofread.
                let edge = 2.0 / (1.0 + phi * phi).sqrt();
                let close = |u: Vec3, v: Vec3| (u - v).length() < edge * 1.05;
                if close(pa, pb) && close(pb, pc) && close(pa, pc) {
                    let normal = (pb - pa).cross(pc - pa);
                    if normal.dot(pa + pb + pc) > 0.0 {
                        indices.push([a, b, c]);
                    } else {
                        indices.push([a, c, b]);
                    }
                }
            }
        }
    }

    // Subdivide: every edge gains a midpoint, shared between the two faces that
    // own it — which is what makes the count exactly `10·4^d + 2` and the
    // surface closed and smooth-shaded.
    for _ in 0..detail {
        let mut midpoints: HashMap<(u32, u32), u32> = HashMap::new();
        let mut next: Vec<[u32; 3]> = Vec::with_capacity(indices.len() * 4);
        for [a, b, c] in indices {
            let mut midpoint = |u: u32, v: u32, positions: &mut Vec<Vec3>| -> u32 {
                let key = (u.min(v), u.max(v));
                *midpoints.entry(key).or_insert_with(|| {
                    let point =
                        ((positions[u as usize] + positions[v as usize]) * 0.5).normalize();
                    positions.push(point);
                    positions.len() as u32 - 1
                })
            };
            let ab = midpoint(a, b, &mut positions);
            let bc = midpoint(b, c, &mut positions);
            let ca = midpoint(c, a, &mut positions);
            next.extend_from_slice(&[[a, ab, ca], [b, bc, ab], [c, ca, bc], [ab, bc, ca]]);
        }
        indices = next;
    }

    let mut mesh = MeshData {
        positions: Vec::with_capacity(positions.len()),
        normals: Vec::with_capacity(positions.len()),
        uvs: Vec::with_capacity(positions.len()),
        indices: indices.into_iter().flatten().collect(),
    };
    for point in positions {
        mesh.positions.push(point.to_array());
        mesh.normals.push(point.to_array());
        // A spherical projection. Nothing samples a texture on a cloud yet;
        // `MeshData` requires the channel, and leaving it degenerate would be a
        // trap for whatever does.
        mesh.uvs.push([
            point.z.atan2(point.x) / std::f32::consts::TAU + 0.5,
            point.y * 0.5 + 0.5,
        ]);
    }
    mesh
}

// ── randomness ─────────────────────────────────────────────────────────────

/// The same splitmix-style finalizer and xorshift the particle system and the
/// tree generator use, duplicated for the reason they duplicate it: the
/// sequence is part of what a scene file means.
fn seed_state(seed: u32) -> u32 {
    let mut z = seed.wrapping_add(0x9E37_79B9);
    z = (z ^ (z >> 16)).wrapping_mul(0x21F0_AAAD);
    z = (z ^ (z >> 15)).wrapping_mul(0x735A_2D97);
    z ^= z >> 15;
    if z == 0 {
        0x9E37_79B9
    } else {
        z
    }
}

fn next(state: &mut u32) -> u32 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *state = x;
    x
}

/// Uniform in `[0, 1)`, from the generator's top 24 bits.
fn unit(state: &mut u32) -> f32 {
    (next(state) >> 8) as f32 / 16_777_216.0
}

// ── the cache ──────────────────────────────────────────────────────────────

/// A [`Cloud`]'s **geometry** fields, exactly. Exact rather than hashed: a hash
/// collision would silently draw the wrong cloud, and the array is 11 words.
///
/// `color`, `shade_color`, `density`, `feather`, `drift` and `drift_wrap` are
/// deliberately absent — they reach the shader as uniforms and cannot move a
/// vertex, so a white cloud and a storm-grey one of the same shape share one
/// upload.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct CloudKey([u32; 11]);

impl CloudKey {
    fn of(cloud: &Cloud) -> Self {
        Self([
            cloud.seed,
            cloud.lobes,
            cloud.levels,
            cloud.children,
            cloud.detail,
            cloud.lobe_size.to_bits(),
            cloud.lobe_ratio.to_bits(),
            cloud.flatten.to_bits(),
            cloud.rise.to_bits(),
            cloud.wobble.to_bits(),
            cloud.jitter.to_bits(),
        ])
    }
}

thread_local! {
    /// Generated geometry is a pure function of the component, so a
    /// process-local cache is not hidden state (invariant 2) any more than
    /// `mesh.rs`'s builtin cache is.
    static CLOUD_CACHE: RefCell<HashMap<CloudKey, Arc<MeshData>>> = RefCell::new(HashMap::new());
    /// One icosphere per subdivision level, shared by every lobe.
    static SPHERE_CACHE: RefCell<HashMap<u32, Arc<MeshData>>> = RefCell::new(HashMap::new());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn puffball() -> Cloud {
        Cloud {
            lobes: 1,
            levels: 0,
            children: 0,
            lobe_size: 1.0,
            wobble: 0.0,
            jitter: 0.0,
            flatten: 0.0,
            ..Cloud::default()
        }
    }

    #[test]
    fn an_icosphere_has_the_vertex_count_the_budget_predicts() {
        // `10·4^d + 2` is what `vertex_count` promises, and validation refuses a
        // cloud on that number before anything is allocated.
        for (detail, expected) in [(0, 12), (1, 42), (2, 162), (3, 642)] {
            let sphere = unit_sphere(detail);
            assert_eq!(sphere.positions.len(), expected, "detail {detail}");
            assert_eq!(sphere.indices.len() / 3, 20 * 4usize.pow(detail));
            assert_eq!(sphere_vertices(detail), expected as u64);
        }
    }

    #[test]
    fn every_lobe_triangle_faces_outward() {
        // Backface culling is on for the mesh passes and off for this one, but
        // a lobe wound inside-out would still shade its far wall as its near
        // one. Volume alone can hide a mesh that is inverted in one place and
        // compensating elsewhere, so check each face against its own centre.
        let sphere = unit_sphere(2);
        for triangle in sphere.indices.chunks_exact(3) {
            let p = |i: u32| Vec3::from_array(sphere.positions[i as usize]);
            let (a, b, c) = (p(triangle[0]), p(triangle[1]), p(triangle[2]));
            let face = (b - a).cross(c - a);
            assert!(
                face.dot(a + b + c) > 0.0,
                "triangle {triangle:?} winds inward"
            );
        }
    }

    #[test]
    fn a_single_lobe_is_a_unit_sphere_scaled_by_lobe_size() {
        // The degenerate case has to be exact, because it is the one an author
        // reaches for to see what a single parameter does: index 0 of the
        // spiral is the centre, and the height profile puts the centre lobe at
        // the top of the space its own radius leaves it — which for a lobe
        // filling the box is the origin.
        let cloud = puffball();
        let mesh = generate(&cloud);
        let expected = cloud.lobe_size * 0.5;
        for position in &mesh.positions {
            let radius = Vec3::from_array(*position).length();
            assert!(
                (radius - expected).abs() < 1e-5,
                "vertex at radius {radius}, expected {expected}"
            );
        }
    }

    #[test]
    fn vertex_count_predicts_what_generation_produces() {
        for cloud in [
            Cloud::default(),
            Cloud { levels: 0, ..Cloud::default() },
            Cloud { children: 0, ..Cloud::default() },
            Cloud { lobes: 1, levels: 3, children: 2, detail: 1, ..Cloud::default() },
            Cloud { detail: 0, lobes: 12, ..Cloud::default() },
        ] {
            let mesh = generate(&cloud);
            assert_eq!(
                vertex_count(&cloud),
                mesh.positions.len() as u64,
                "for {cloud:?}"
            );
        }
    }

    #[test]
    fn the_same_cloud_grows_the_same_mesh_and_a_new_seed_does_not() {
        let cloud = Cloud::default();
        let first = generate(&cloud);
        let again = generate(&cloud);
        assert_eq!(first, again, "generation is a pure function of the component");

        let other = generate(&Cloud { seed: 1, ..cloud.clone() });
        assert_eq!(
            first.positions.len(),
            other.positions.len(),
            "same parameters, same vertex budget"
        );
        assert_ne!(
            first.positions, other.positions,
            "a different seed must grow a different cloud"
        );
    }

    #[test]
    fn no_jitter_and_no_wobble_leaves_only_the_children_to_the_seed() {
        // The authoring reference. The base cluster is a *profile* — spiral
        // placement, size falloff, height curve — so with `jitter` and `wobble`
        // off it is fully determined and the seed stops mattering, which is
        // what the fixture's `Diagram` entity relies on.
        let diagram = Cloud {
            levels: 0,
            children: 0,
            jitter: 0.0,
            wobble: 0.0,
            ..Cloud::default()
        };
        let a = generate(&diagram);
        let b = generate(&Cloud { seed: 99, ..diagram.clone() });
        assert_eq!(a.positions, b.positions, "no jitter, no wobble: no randomness");

        // Children are the exception, and deliberately so: where a lobe
        // attaches is a direction draw with no parameter to turn off. A diagram
        // cloud is a diagram of the base cluster.
        let piled = Cloud { levels: 1, children: 3, ..diagram };
        assert_ne!(
            generate(&piled).positions,
            generate(&Cloud { seed: 99, ..piled }).positions,
            "children must still vary with the seed"
        );
    }

    #[test]
    fn flattening_puts_a_floor_under_the_cloud() {
        // The flat base is one of the four cues in `cloud-design.md` §1, and it
        // is the one a unit test can actually check.
        let flat = generate(&Cloud { flatten: 1.0, ..Cloud::default() });
        let lowest = flat.positions.iter().map(|p| p[1]).fold(f32::MAX, f32::min);
        assert!(
            (lowest - BASE_PLANE).abs() < 1e-6,
            "a fully flattened cloud must rest on its base plane, got {lowest}"
        );
        // And it is a *floor*, not a squash: the cloud still has height.
        let highest = flat.positions.iter().map(|p| p[1]).fold(f32::MIN, f32::max);
        assert!(highest > 0.0, "the flattened cloud collapsed: top at {highest}");

        // Checked across seeds, because one seed proves nothing about a scatter.
        for seed in 0..16 {
            let cloud = generate(&Cloud { seed, flatten: 1.0, ..Cloud::default() });
            let lowest = cloud.positions.iter().map(|p| p[1]).fold(f32::MAX, f32::min);
            assert!(lowest >= BASE_PLANE - 1e-5, "seed {seed} leaked below the base");
        }
    }

    #[test]
    fn a_cloud_stays_inside_a_plausible_envelope() {
        // Catches the class of bug where a child chain walks away from its
        // cluster — visible as a wart floating beside the cloud, and easy to
        // miss in a thumbnail.
        for seed in 0..24 {
            let cloud = Cloud { seed, levels: 3, ..Cloud::default() };
            let mesh = generate(&cloud);
            let reach = mesh
                .positions
                .iter()
                .map(|p| Vec3::from_array(*p).length())
                .fold(0.0f32, f32::max);
            assert!(reach < 1.4, "seed {seed} reached {reach} outside the unit box");
        }
    }

    #[test]
    fn children_pile_upward() {
        // `rise` is what separates a convection cell from a sea urchin. With it
        // at 1 the crown must sit clear of where the base lobes topped out.
        let base = Cloud { levels: 0, children: 0, flatten: 1.0, ..Cloud::default() };
        let risen = Cloud { levels: 2, children: 4, rise: 1.0, ..base.clone() };
        let top = |c: &Cloud| {
            generate(c)
                .positions
                .iter()
                .map(|p| p[1])
                .fold(f32::MIN, f32::max)
        };
        assert!(
            top(&risen) > top(&base) + 0.05,
            "children did not pile up: {} vs {}",
            top(&risen),
            top(&base)
        );
    }

    #[test]
    fn the_cache_hands_back_one_arc_per_distinct_shape() {
        // The renderer's upload cache keys on `Arc` identity; a fresh copy per
        // frame would re-upload every cloud in the sky every frame.
        let cloud = Cloud { seed: 4242, ..Cloud::default() };
        assert!(Arc::ptr_eq(&mesh_for(&cloud), &mesh_for(&cloud)));
        assert!(!Arc::ptr_eq(
            &mesh_for(&cloud),
            &mesh_for(&Cloud { seed: 4243, ..cloud.clone() })
        ));

        // Shading is a uniform, not a vertex: two clouds that differ only in
        // colour must share one mesh and one upload.
        let repainted = Cloud {
            color: Vec3::new(0.2, 0.2, 0.3),
            density: 0.4,
            drift: Vec3::new(3.0, 0.0, 0.0),
            ..cloud.clone()
        };
        assert!(Arc::ptr_eq(&mesh_for(&cloud), &mesh_for(&repainted)));
    }

    #[test]
    fn the_budget_catches_a_plausible_typo() {
        // The point of the exact count: `levels: 3, children: 8` is one
        // keystroke from `levels: 2` and is 18,720 lobes.
        let runaway = Cloud {
            lobes: 32,
            levels: 3,
            children: 8,
            detail: 2,
            ..Cloud::default()
        };
        assert!(vertex_count(&runaway) > MAX_CLOUD_VERTICES);
        assert!(vertex_count(&Cloud::default()) < MAX_CLOUD_VERTICES);
    }
}
