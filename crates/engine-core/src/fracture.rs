//! Material-aware fracture (M43): breaking a box into shards, offline.
//!
//! This is what `engine fracture` runs. It is a **command**, never a runtime
//! behaviour — M14 settled that a `Breakable`'s fragments exist in the text
//! file before the run, and this generates that text. The `engine fit-colliders`
//! precedent (M39): a solver an agent runs when it wants the answer, whose
//! output is ordinary authored data.
//!
//! # One clipper, four seedings
//!
//! Every material is the same algorithm with a different seed distribution and
//! a different metric:
//!
//! 1. Choose seed points in the box.
//! 2. A cell is the set of points closer to its seed than to any other, so it
//!    is the box clipped by the perpendicular bisector of its seed against
//!    every other seed. All planes.
//! 3. A cell's vertices are the intersections of every triple of its planes
//!    that satisfies all of its half-spaces.
//!
//! **The tiling property is load-bearing.** Voronoi cells fill the box and do
//! not overlap. Fragments that overlapped at spawn would be interpenetrating
//! rigid bodies, and rapier resolves interpenetration by pushing them apart
//! hard — a crate that explodes on contact instead of breaking. Any material
//! added here has to keep the property or bring its own guarantee.
//!
//! **Anisotropy comes from an affine metric**, not from perturbing the planes.
//! Measuring distance in a space scaled per axis leaves the bisectors exact
//! bisectors *under that metric*, so the cells still tile — while the shapes
//! stretch. That one trick is the difference between wood and stone: a
//! splinter is a Voronoi cell measured in a space squashed along the grain.
//!
//! # Determinism
//!
//! The xorshift is written out here, seeded from the caller's seed and drawn in
//! a fixed order, for the reason `particles.rs`, `tree.rs` and `cloud.rs` each
//! write theirs out: the sequence is part of what the output *means*, and it
//! may not live somewhere a dependency upgrade can reshape it. Same inputs,
//! same JSON, byte for byte.

use glam::Vec3;

use crate::components::{FractureMaterial, Fragment};
use crate::error::{EngineError, Result};
use crate::{codes, shard};

/// What to fracture, and how.
#[derive(Debug, Clone, Copy)]
pub struct Recipe {
    /// The source volume's half-extents in metres, centred on the entity's
    /// origin — a `Collider`'s cuboid extents, or the mesh's AABB.
    pub half_extents: Vec3,
    pub material: FractureMaterial,
    /// How many pieces to aim for. Cells are never dropped, so this is exact.
    pub pieces: u32,
    pub seed: u32,
    /// Where the thing was struck, in entity-local metres. Materials densify
    /// their seeding here, so the fine debris is where the impact was.
    pub impact: Vec3,
    /// Wood's grain direction; ignored by the other three. Normalized, and a
    /// zero vector falls back to the box's longest axis.
    pub grain: Option<Vec3>,
}

impl Recipe {
    /// The piece count a material breaks into when the caller does not say.
    ///
    /// Glass makes the most and metal the fewest, which is most of what
    /// separates the two at a glance.
    pub fn default_pieces(material: FractureMaterial) -> u32 {
        match material {
            FractureMaterial::Glass => 18,
            FractureMaterial::Wood => 10,
            FractureMaterial::Stone => 12,
            FractureMaterial::Metal => 5,
        }
    }

    /// Where a thing gets hit when the caller does not say: the middle of the
    /// face it is most likely to be struck on — the thin axis for glass (a
    /// pane is hit *through*), the top otherwise (things get dropped).
    pub fn default_impact(half_extents: Vec3, material: FractureMaterial) -> Vec3 {
        if material == FractureMaterial::Glass {
            let thin = thin_axis(half_extents);
            return Vec3::select(axis_mask(thin), half_extents, Vec3::ZERO);
        }
        Vec3::new(0.0, half_extents.y, 0.0)
    }
}

/// The most pieces one call will fracture something into. Past this the cells
/// are smaller than the shards' own point budget can describe, and a scene
/// file gains a thousand lines of numbers nobody will read.
pub const MAX_PIECES: u32 = 48;

/// Fracture a box into fragments, ready to serialize into a `Breakable`.
///
/// Fragments come back in seed order, each with its points **centred on its
/// own centroid** and its `offset` placing it back where it came from. That
/// keeps a fragment's `Transform.position` the fragment's actual position once
/// it spawns, which is what `engine simulate --entity` reports and what the
/// scatter aims from.
pub fn fracture(recipe: &Recipe) -> Result<Vec<Fragment>> {
    let half = recipe.half_extents;
    if !(half.x > 0.0 && half.y > 0.0 && half.z > 0.0) {
        return Err(EngineError::new(
            codes::INVALID_SHAPE_DIMENSION,
            format!(
                "cannot fracture a volume with half-extents [{}, {}, {}]; every axis \
                 must be greater than 0",
                half.x, half.y, half.z
            ),
        ));
    }
    if recipe.pieces == 0 || recipe.pieces > MAX_PIECES {
        return Err(EngineError::new(
            codes::VALUE_OUT_OF_RANGE,
            format!(
                "{} pieces is outside the range a fracture produces (1 to {MAX_PIECES})",
                recipe.pieces
            ),
        ));
    }

    let mut rng = Rng::new(recipe.seed);
    let (seeds, metric) = seed(recipe, &mut rng);
    let density = recipe.material.behaviour().density;

    // The box, as six outward half-spaces.
    let walls: Vec<Plane> = [
        (Vec3::X, half.x),
        (Vec3::NEG_X, half.x),
        (Vec3::Y, half.y),
        (Vec3::NEG_Y, half.y),
        (Vec3::Z, half.z),
        (Vec3::NEG_Z, half.z),
    ]
    .into_iter()
    .map(|(normal, offset)| Plane { normal, offset })
    .collect();

    let mut fragments = Vec::with_capacity(seeds.len());
    for (index, seed_point) in seeds.iter().enumerate() {
        let mut planes = walls.clone();
        for (other, neighbour) in seeds.iter().enumerate() {
            if other != index {
                planes.push(bisector(*seed_point, *neighbour, metric));
            }
        }

        let corners = corners_of(&planes);
        if corners.len() < 4 {
            // A Voronoi cell always contains its own seed, so this means two
            // seeds landed on top of each other — the generator's problem to
            // report, not something to paper over with a hole in the solid.
            return Err(EngineError::new(
                codes::FRACTURE_FAILED,
                format!(
                    "fracture cell {index} came out flat, which means two seeds \
                     coincided; try a different --seed"
                ),
            ));
        }
        if corners.len() > shard::MAX_SHARD_POINTS {
            return Err(EngineError::new(
                codes::FRACTURE_FAILED,
                format!(
                    "fracture cell {index} has {} corners, more than a shard may carry \
                     ({}); fracture into fewer pieces",
                    corners.len(),
                    shard::MAX_SHARD_POINTS
                ),
            ));
        }

        let centre = shard::centroid(&corners);
        fragments.push(Fragment {
            mesh: None,
            points: Some(corners.iter().map(|p| *p - centre).collect()),
            offset: centre,
            rotation: Vec3::ZERO,
            scale: Vec3::ONE,
            half_extents: None,
            density,
        });
    }

    Ok(fragments)
}

/// One outward half-space: `normal · p <= offset` is inside.
#[derive(Debug, Clone, Copy)]
struct Plane {
    normal: Vec3,
    offset: f32,
}

/// The perpendicular bisector of `here` and `there`, measured in a space
/// scaled by `metric` — the plane every point closer to `here` lies behind.
///
/// In the scaled space the bisector is `(b-a) · p' = (|b|²-|a|²)/2`. A point
/// maps as `p' = p * metric`, so the same plane in real space has normal
/// `(b-a) * metric` and the same offset. That is the whole anisotropy trick:
/// still an exact bisector, so the cells still tile.
fn bisector(here: Vec3, there: Vec3, metric: Vec3) -> Plane {
    let a = here * metric;
    let b = there * metric;
    let normal = (b - a) * metric;
    let offset = (b.length_squared() - a.length_squared()) * 0.5;
    let length = normal.length().max(1e-9);
    Plane {
        normal: normal / length,
        offset: offset / length,
    }
}

/// Every corner of the solid a set of half-spaces bounds: the intersection of
/// each triple of planes that satisfies all of them.
///
/// Brute force over triples for `shard.rs`'s reason — the counts are tiny, and
/// an exhaustive search has no merge order to get subtly wrong.
fn corners_of(planes: &[Plane]) -> Vec<Vec3> {
    // Generous next to the plane arithmetic: three planes meeting at a shallow
    // angle put a corner a few ulps outside the neighbour that also touches it.
    const INSIDE: f32 = 1e-4;
    const WELD: f32 = 1e-5;

    let mut corners: Vec<Vec3> = Vec::new();
    for i in 0..planes.len() {
        for j in (i + 1)..planes.len() {
            for k in (j + 1)..planes.len() {
                let Some(point) = meet(planes[i], planes[j], planes[k]) else {
                    continue;
                };
                if planes
                    .iter()
                    .all(|plane| plane.normal.dot(point) <= plane.offset + INSIDE)
                    && !corners.iter().any(|kept| kept.distance(point) < WELD)
                {
                    corners.push(point);
                }
            }
        }
    }
    corners
}

/// Where three planes meet, by Cramer's rule. `None` when they do not meet in
/// a point — two of them parallel, or all three sharing a line.
fn meet(a: Plane, b: Plane, c: Plane) -> Option<Vec3> {
    let determinant = a.normal.dot(b.normal.cross(c.normal));
    if determinant.abs() < 1e-7 {
        return None;
    }
    Some(
        (a.offset * b.normal.cross(c.normal)
            + b.offset * c.normal.cross(a.normal)
            + c.offset * a.normal.cross(b.normal))
            / determinant,
    )
}

/// This material's seed points and the metric its distances are measured in.
fn seed(recipe: &Recipe, rng: &mut Rng) -> (Vec<Vec3>, Vec3) {
    let half = recipe.half_extents;
    let count = recipe.pieces as usize;
    let impact = recipe.impact.clamp(-half, half);

    match recipe.material {
        // Chunky irregular blocks, finer where it was hit. Uniform scatter,
        // with the early seeds pulled toward the impact — the pull falls off
        // over the list, so the fine tail *is* the debris and there is no
        // second system producing it.
        FractureMaterial::Stone => {
            let seeds = (0..count)
                .map(|i| {
                    let loose = Vec3::new(
                        rng.range(-half.x, half.x),
                        rng.range(-half.y, half.y),
                        rng.range(-half.z, half.z),
                    );
                    let pull = (1.0 - i as f32 / count as f32).powi(2) * 0.7;
                    loose.lerp(impact, pull)
                })
                .collect();
            (seeds, Vec3::ONE)
        }

        // Radial slivers and concentric rings, dense at the impact. Every seed
        // sits at the same depth through the pane, so the bisectors are all
        // perpendicular to it and the shards go all the way through — which is
        // what a shattered pane does.
        FractureMaterial::Glass => {
            let thin = thin_axis(half);
            let (u_axis, v_axis) = pane_axes(thin);
            let reach = (half * (Vec3::ONE - axis_mask_f32(thin))).length();
            let sectors = ((count as f32 * 1.6).sqrt().ceil() as usize).max(3);
            let rings = count.div_ceil(sectors).max(1);

            let mut seeds = Vec::with_capacity(count);
            for i in 0..count {
                let ring = i / sectors;
                let sector = i % sectors;
                // Rings grow faster than linearly, so the cells near the
                // impact are small and the outer ones are long slivers.
                let radius = reach
                    * ((ring + 1) as f32 / rings as f32).powf(1.8)
                    * (0.75 + 0.5 * rng.unit());
                // Staggered by the golden ratio so the cracks of one ring do
                // not line up with the next and draw spokes.
                let angle = std::f32::consts::TAU
                    * ((sector as f32 / sectors as f32) + 0.618 * ring as f32)
                    + 0.3 * rng.signed();
                let offset = u_axis * (radius * angle.cos()) + v_axis * (radius * angle.sin());
                let flat = Vec3::select(axis_mask(thin), impact, Vec3::ZERO);
                seeds.push((flat + offset).clamp(-half, half));
            }
            (seeds, Vec3::ONE)
        }

        // Long splinters with ragged ends. The metric squashes distance along
        // the grain, so a cell reaches far up the plank before a neighbour
        // takes over; the cross-cut stations are what stop it being an
        // infinite bundle of full-length matchsticks.
        FractureMaterial::Wood => {
            let grain = recipe
                .grain
                .filter(|g| g.length() > 1e-6)
                .map(|g| g.normalize())
                .unwrap_or_else(|| axis_vector(long_axis(half)));
            let stations = if count >= 8 { 3 } else { 2 };
            let across = count.div_ceil(stations).max(1);

            let mut seeds = Vec::with_capacity(count);
            for i in 0..count {
                let station = i / across;
                let loose = Vec3::new(
                    rng.range(-half.x, half.x),
                    rng.range(-half.y, half.y),
                    rng.range(-half.z, half.z),
                );
                // Flatten the seed onto the cross-section through the impact,
                // then push it back along the grain to its own station. The
                // jitter is what makes the break ends ragged rather than a
                // clean saw cut across every splinter at once.
                let along = grain.dot(loose);
                let across_grain = loose - grain * along;
                let reach = grain.dot(half.abs());
                let step = (station as f32 + 0.5) / stations as f32 * 2.0 - 1.0;
                let seated = reach * (step + 0.35 * rng.signed() / stations as f32);
                seeds.push((across_grain + grain * seated).clamp(-half, half));
            }
            // Distance along the grain counts for a quarter, so cells stretch
            // four times as far that way before a neighbour wins.
            let metric = Vec3::ONE - grain.abs() * 0.75;
            (seeds, metric)
        }

        // A handful of large torn plates, pulled toward the impact and
        // stretched along the longest axis — metal parts, it does not shatter.
        FractureMaterial::Metal => {
            let long = axis_vector(long_axis(half));
            let seeds = (0..count)
                .map(|i| {
                    let loose = Vec3::new(
                        rng.range(-half.x, half.x),
                        rng.range(-half.y, half.y),
                        rng.range(-half.z, half.z),
                    );
                    let pull = (1.0 - i as f32 / count as f32) * 0.4;
                    loose.lerp(impact, pull)
                })
                .collect();
            (seeds, Vec3::ONE - long.abs() * 0.5)
        }
    }
}

/// The index of the box's shortest axis — a pane's thickness.
fn thin_axis(half: Vec3) -> usize {
    let h = half.to_array();
    (0..3).fold(0, |best, i| if h[i] < h[best] { i } else { best })
}

/// The index of the box's longest axis — a plank's length.
fn long_axis(half: Vec3) -> usize {
    let h = half.to_array();
    (0..3).fold(0, |best, i| if h[i] > h[best] { i } else { best })
}

fn axis_vector(axis: usize) -> Vec3 {
    [Vec3::X, Vec3::Y, Vec3::Z][axis]
}

fn axis_mask(axis: usize) -> glam::BVec3 {
    glam::BVec3::new(axis == 0, axis == 1, axis == 2)
}

fn axis_mask_f32(axis: usize) -> Vec3 {
    axis_vector(axis)
}

/// The two axes spanning a pane whose thickness runs along `thin`.
fn pane_axes(thin: usize) -> (Vec3, Vec3) {
    match thin {
        0 => (Vec3::Y, Vec3::Z),
        1 => (Vec3::X, Vec3::Z),
        _ => (Vec3::X, Vec3::Y),
    }
}

/// The generator's private xorshift — see the module docs on why it is spelled
/// out here rather than pulled from a crate.
struct Rng(u32);

impl Rng {
    fn new(seed: u32) -> Self {
        // A zero state is a fixed point of xorshift, so it can never be one.
        Self(seed.wrapping_mul(0x9E37_79B9) | 1)
    }

    /// The next raw draw, in `[0, 1)`.
    fn unit(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0 as f32 / u32::MAX as f32
    }

    /// The next draw in `[-1, 1)`.
    fn signed(&mut self) -> f32 {
        self.unit() * 2.0 - 1.0
    }

    /// The next draw in `[low, high)`.
    fn range(&mut self, low: f32, high: f32) -> f32 {
        low + (high - low) * self.unit()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recipe(material: FractureMaterial, half: Vec3) -> Recipe {
        Recipe {
            half_extents: half,
            material,
            pieces: Recipe::default_pieces(material),
            seed: 7,
            impact: Recipe::default_impact(half, material),
            grain: None,
        }
    }

    /// A fragment's points, put back where they came from.
    fn placed(fragment: &Fragment) -> Vec<Vec3> {
        fragment
            .points
            .as_ref()
            .expect("a generated fragment is a shard")
            .iter()
            .map(|p| *p + fragment.offset)
            .collect()
    }

    const EVERY: [FractureMaterial; 4] = [
        FractureMaterial::Glass,
        FractureMaterial::Wood,
        FractureMaterial::Stone,
        FractureMaterial::Metal,
    ];

    #[test]
    fn every_material_fills_the_box_it_broke() {
        // The tiling property, and the one that matters most: fragments that
        // overlapped would be interpenetrating bodies at spawn, and rapier
        // pushes those apart hard enough to look like an explosion.
        let half = Vec3::new(0.5, 0.3, 0.8);
        let box_volume = 8.0 * half.x * half.y * half.z;
        for material in EVERY {
            let fragments = fracture(&recipe(material, half)).expect("fractures");
            let total: f32 = fragments.iter().map(|f| shard::volume(&placed(f))).sum();
            assert!(
                (total - box_volume).abs() < box_volume * 0.01,
                "{material:?}: {} fragments hold {total} m³ of a {box_volume} m³ box",
                fragments.len()
            );
        }
    }

    #[test]
    fn every_fragment_is_a_solid_inside_the_box() {
        let half = Vec3::new(0.6, 0.4, 0.5);
        for material in EVERY {
            for (i, fragment) in fracture(&recipe(material, half))
                .expect("fractures")
                .iter()
                .enumerate()
            {
                let points = placed(fragment);
                assert!(
                    shard::hull(&points).is_some(),
                    "{material:?} fragment {i} bounds no volume"
                );
                assert!(
                    points.len() <= shard::MAX_SHARD_POINTS,
                    "{material:?} fragment {i} has {} points",
                    points.len()
                );
                for point in &points {
                    assert!(
                        point.abs().cmple(half + Vec3::splat(1e-3)).all(),
                        "{material:?} fragment {i} reaches {point:?}, outside the box"
                    );
                }
            }
        }
    }

    #[test]
    fn the_piece_count_is_exact() {
        for material in EVERY {
            for pieces in [1, 3, 12, 24] {
                let mut wanted = recipe(material, Vec3::new(0.5, 0.5, 0.5));
                wanted.pieces = pieces;
                assert_eq!(
                    fracture(&wanted).expect("fractures").len(),
                    pieces as usize,
                    "{material:?} at {pieces} pieces"
                );
            }
        }
    }

    #[test]
    fn the_same_seed_reproduces_the_same_shards() {
        let recipe = recipe(FractureMaterial::Stone, Vec3::splat(0.5));
        assert_eq!(fracture(&recipe).unwrap(), fracture(&recipe).unwrap());

        let mut other = recipe;
        other.seed = 8;
        assert_ne!(
            fracture(&recipe).unwrap(),
            fracture(&other).unwrap(),
            "a different seed is a different break"
        );
    }

    #[test]
    fn wood_splinters_along_its_grain() {
        // A plank, long in Z. Its splinters should each be longer along the
        // grain than they are wide — which is the whole claim "wood" makes.
        let half = Vec3::new(0.4, 0.1, 1.5);
        let fragments = fracture(&recipe(FractureMaterial::Wood, half)).expect("fractures");
        let elongated = fragments
            .iter()
            .filter(|fragment| {
                let points = placed(fragment);
                let span = |axis: fn(&Vec3) -> f32| {
                    let values: Vec<f32> = points.iter().map(axis).collect();
                    values.iter().copied().fold(f32::MIN, f32::max)
                        - values.iter().copied().fold(f32::MAX, f32::min)
                };
                span(|p| p.z) > span(|p| p.x)
            })
            .count();
        assert!(
            elongated * 2 > fragments.len(),
            "only {elongated} of {} splinters run along the grain",
            fragments.len()
        );
    }

    #[test]
    fn glass_shards_reach_through_the_pane() {
        // A pane thin in Y: every shard must span the full thickness, because
        // that is what a shattered pane does — no shard is a flake off a face.
        let half = Vec3::new(0.8, 0.02, 0.8);
        for fragment in fracture(&recipe(FractureMaterial::Glass, half)).expect("fractures") {
            let points = placed(&fragment);
            let low = points.iter().map(|p| p.y).fold(f32::MAX, f32::min);
            let high = points.iter().map(|p| p.y).fold(f32::MIN, f32::max);
            assert!(
                (high - low) > half.y * 1.9,
                "a shard spans {} of the pane's {} thickness",
                high - low,
                half.y * 2.0
            );
        }
    }

    #[test]
    fn glass_breaks_finer_at_the_impact() {
        // Densifying the seeding near the impact is where the debris comes
        // from, so it is worth a test rather than a comment: the shards near
        // where it was hit are smaller than the ones at the rim.
        let half = Vec3::new(1.0, 0.02, 1.0);
        let mut recipe = recipe(FractureMaterial::Glass, half);
        recipe.impact = Vec3::new(0.0, 0.02, 0.0);
        let fragments = fracture(&recipe).expect("fractures");

        let mut by_distance: Vec<(f32, f32)> = fragments
            .iter()
            .map(|f| {
                let flat = Vec3::new(f.offset.x, 0.0, f.offset.z);
                (flat.length(), shard::volume(&placed(f)))
            })
            .collect();
        by_distance.sort_by(|a, b| a.0.total_cmp(&b.0));
        let half_way = by_distance.len() / 2;
        let near: f32 = by_distance[..half_way].iter().map(|(_, v)| v).sum();
        let far: f32 = by_distance[half_way..].iter().map(|(_, v)| v).sum();
        assert!(
            near < far,
            "the inner shards hold {near} m³ and the outer ones {far} m³"
        );
    }

    #[test]
    fn a_flat_or_empty_source_is_an_error_rather_than_a_panic() {
        let mut flat = recipe(FractureMaterial::Stone, Vec3::new(0.5, 0.0, 0.5));
        assert!(fracture(&flat).is_err(), "a box with no thickness");

        flat.half_extents = Vec3::splat(0.5);
        flat.pieces = MAX_PIECES + 1;
        assert!(fracture(&flat).is_err(), "past the piece ceiling");

        flat.pieces = 0;
        assert!(fracture(&flat).is_err(), "no pieces at all");
    }
}
