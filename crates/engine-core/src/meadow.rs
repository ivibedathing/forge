//! Procedural ground cover with a life cycle (M29).
//!
//! A [`Meadow`] component is a recipe, not a mesh reference — M19's premise
//! again — but it is the first recipe in this engine whose subject **changes
//! shape over time**. A sprout is not a small blade of grass; a dry stalk with a
//! seed head is a third thing again. That collides with the engine's geometry
//! pipeline, which assumes geometry is generated once, `Arc`-cached and uploaded
//! once (M15 keys the renderer's vertex-buffer cache on the `Arc`'s address).
//!
//! # The resolution: two static buffers, and life in the vertex stage
//!
//! This module produces exactly two things per meadow, and **neither ever
//! changes with time**:
//!
//! - a **template** — one plant grown at maximum extent, carrying every organ
//!   any stage of the cycle will ever need;
//! - an **instance buffer** — one 36-byte record per plant, placed once.
//!
//! Everything visible — growth, leaning, colour, the flower opening, the
//! collapse — happens in `meadow.wgsl`'s vertex stage from `ScenePass.time`.
//! This is M18's answer for water, applied to a harder case: water's grid at
//! least kept its topology, and a meadow's plants have to change organs.
//!
//! Shape change is expressed as a **scale animation on parts that are always
//! present in the buffer**: every vertex carries the phase window
//! (`emerge`..`wither`) during which its organ exists, and outside that window
//! the organ scales to zero about its own anchor. Zero-area triangles rasterize
//! nothing, so the cull is free — no second draw, no index rewriting, and no
//! branch that could diverge across a warp.
//!
//! # Determinism
//!
//! One private xorshift, seeded from `Meadow::seed`, drives placement in a fixed
//! order: the template's blades in index order, then the instance grid in row
//! order. The generator and its hash are spelled out in this repo, as
//! `particles.rs`, `tree.rs`, `cloud.rs` and `terrain.rs` spell theirs out,
//! because the sequence is part of what a scene file *means* and may not live
//! somewhere a dependency upgrade can change it.
//!
//! The **reseed hash is a second format contract**, and it lives in the shader
//! rather than here: `hash(plant.seed, generation)` is what gives each plant a
//! fresh position, height and lean every time round the cycle. See
//! `designs/meadow-design.md` §3.
//!
//! [`Meadow`]: crate::components::Meadow

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use glam::{Mat4, Vec2, Vec3};

use crate::components::{Meadow, Terrain};

/// Most life-cycle keyframes one meadow may carry. The table rides a per-draw
/// uniform array, which is fixed-size — beyond this validation rejects the
/// scene rather than silently dropping the end of the cycle. The shape
/// [`MAX_WAVES`](crate::water::MAX_WAVES) and `MAX_POINT_LIGHTS` already use.
pub const MAX_GROWTH_STAGES: usize = 8;

/// Beyond this a meadow is a mistake in a parameter, not a plan — see
/// `meadow_too_complex`. Counted in **triangles**, not plants, because the two
/// knobs that blow it up pull in opposite directions: a sparse field of
/// elaborate plants and a dense field of simple ones cost the same, and only
/// their product is the number that hangs a render.
///
/// A 40 × 40 m field at the default density and template is about 0.9 M, so
/// this ceiling is reachable by accident rather than only by malice.
pub const MAX_MEADOW_TRIANGLES: u64 = 8_000_000;

/// Bounded like `tree.rs`'s and `cloud.rs`'s, and for the same reason:
/// animating a shape parameter mints a new key every step.
const MAX_CACHED_PATCHES: usize = 32;

/// How much of a blade's own width the midrib is raised by. A flat strip has
/// one normal and shades as a painted stripe; the fold is what gives a tuft two
/// tones under one light, and it is the same trick `Tree`'s `blade` leaf uses.
const MIDRIB_FOLD: f32 = 0.35;

/// The golden angle, in degrees — how blades are spun around the tuft, for the
/// reason `Tree::branch_twist` uses it: a whole-number division stacks them into
/// rows the eye picks out immediately.
const GOLDEN_ANGLE: f32 = 137.5;

/// Where in the cycle the flower head opens and closes, and where the seed head
/// that replaces it does. Fixed rather than authored: these are the *geometry's*
/// windows, and the stage table already says what the plant looks like while
/// they are open. Two ways to move the same event is the trap `Water` avoided by
/// having no `size` field beside `Transform.scale`.
const FLOWER_WINDOW: (f32, f32) = (0.42, 0.72);
const SEED_HEAD_WINDOW: (f32, f32) = (0.66, 0.98);

/// Which organ a vertex belongs to. The fragment stage colours blades from the
/// stage table's gradient and heads from their own fields, which is what stops
/// the weed stage from being nothing but taller grass.
pub const ORGAN_BLADE: u32 = 0;
pub const ORGAN_FLOWER: u32 = 1;
pub const ORGAN_SEED_HEAD: u32 = 2;

/// One vertex of a plant template.
///
/// Its own type rather than a [`MeshData`](crate::mesh::MeshData): a plant needs
/// channels no mesh has, and `MeshData`'s layout is threaded through every
/// upload path in the renderer — M26 has just finished adding UVs to it, and
/// changing it again is a bit-exactness risk against every committed baseline
/// for the sake of a component that does not share the mesh pipeline anyway.
#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(C)]
pub struct MeadowVertex {
    /// The plant's centre line, in **unit-height space**: the plant stands 1.0
    /// tall along +Y with its root at the origin, and the shader multiplies this
    /// by the stage's height.
    pub centre: [f32; 3],
    pub normal: [f32; 3],
    /// This vertex's displacement from the centre line, in **metres**.
    ///
    /// Separate from [`centre`](Self::centre) because the two scale by different
    /// things: height by the stage's `height`, girth by its `width`. Folding
    /// them into one position would make a plant that grows taller also grow
    /// proportionally fatter, and would leave `blade_width` — authored in metres
    /// — meaning something different at every stage of the cycle.
    pub offset: [f32; 3],
    /// The point the organ scales about when it emerges or withers, in
    /// unit-height space. A flower must open from the top of its stem, not from
    /// the plant's root.
    pub anchor: [f32; 3],
    /// `[t, emerge, wither]`: the parameter along the plant (0 at the root, 1 at
    /// the tip — it drives the cantilever bend, the colour gradient and the wind
    /// weighting), then the phase window this vertex's organ exists in. `0..1`
    /// is "always", which is what every blade carries.
    pub span: [f32; 3],
    pub organ: u32,
}

/// One plant, placed. World space already — see [`patch_for`].
#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(C)]
pub struct MeadowInstance {
    pub position: [f32; 3],
    /// Metres of plant per unit of template height.
    pub scale: f32,
    pub yaw: f32,
    /// Added to `time / cycle_length`, so plants do not march in lockstep.
    pub phase_offset: f32,
    /// The ground's slope here, `(∂y/∂x, ∂y/∂z)`. The shader moves a plant
    /// within its cell when it reseeds (§3 of the design), and without this the
    /// new generation would sprout at the *old* spot's altitude — on a hillside,
    /// visibly buried or floating. First-order is exact enough: the correction
    /// is over a jitter radius of a few centimetres.
    pub ground_gradient: [f32; 2],
    /// Seeds the per-generation reseed hash in the shader.
    pub seed: u32,
}

/// A meadow's geometry: one template, and where every copy of it stands.
///
/// Both halves in one `Arc`, following `RoadItem`'s `Arc<RoadSurface>`: the
/// instance buffer depends on the entity's transform and on the terrain it
/// stands on, so the two cannot usefully be cached on different keys.
#[derive(Debug, Clone, PartialEq)]
pub struct MeadowPatch {
    pub vertices: Vec<MeadowVertex>,
    pub indices: Vec<u32>,
    pub instances: Vec<MeadowInstance>,
    /// How far a plant may wander from its cell centre when it reseeds, in
    /// metres. Half a cell, so a plant stays in its own cell and the field never
    /// develops bald patches or clumps however many generations pass.
    pub jitter_radius: f32,
}

impl MeadowPatch {
    pub fn triangle_count(&self) -> usize {
        (self.indices.len() / 3) * self.instances.len()
    }
}

/// The ground a meadow stands on, when it named one.
#[derive(Debug, Clone, Copy)]
pub struct Ground<'a> {
    pub terrain: &'a Terrain,
    /// The terrain entity's flattened transform, which is what turns the unit
    /// height field into world metres — see [`crate::terrain::world_height_at`].
    pub transform: &'a crate::components::Transform,
}

thread_local! {
    static PATCH_CACHE: RefCell<HashMap<PatchKey, Arc<MeadowPatch>>> =
        RefCell::new(HashMap::new());
}

/// Build a meadow's geometry, or hand back the copy already built.
///
/// `model` is the entity's flattened transform. Instances come out in **world
/// space**, so the renderer draws them with no model matrix — placement has to
/// consult the terrain, and a plant whose height was sampled from the ground
/// cannot then be pushed around by a transform without leaving the ground.
///
/// Handing back a shared `Arc` is not just an allocation saved: the renderer's
/// upload cache keys on `Arc` identity (M15), so a fresh copy each frame would
/// re-upload every blade of grass in the scene every frame.
pub fn patch_for(meadow: &Meadow, model: Mat4, ground: Option<Ground<'_>>) -> Arc<MeadowPatch> {
    let key = PatchKey::of(meadow, model, ground);
    PATCH_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(hit) = cache.get(&key) {
            return Arc::clone(hit);
        }
        if cache.len() >= MAX_CACHED_PATCHES {
            cache.clear();
        }
        let built = Arc::new(generate(meadow, model, ground));
        cache.insert(key, Arc::clone(&built));
        built
    })
}

/// Build a meadow's geometry unconditionally. Pure — the cached [`patch_for`] is
/// the one callers should use.
pub fn generate(meadow: &Meadow, model: Mat4, ground: Option<Ground<'_>>) -> MeadowPatch {
    let (vertices, indices) = template(meadow);
    let (instances, jitter_radius) = scatter(meadow, model, ground);
    MeadowPatch {
        vertices,
        indices,
        instances,
        jitter_radius,
    }
}

/// Triangles this meadow would draw, from the parameters and the footprint
/// alone — what validation checks before anything is allocated.
///
/// Exact rather than an estimate, so the budget error can name a real number.
/// The footprint comes from `Transform.scale`, which is why this takes it: a
/// meadow's cost is `density × area × template`, and two of those three are not
/// in the component.
pub fn triangle_count(meadow: &Meadow, scale_x: f32, scale_z: f32) -> u64 {
    plant_count(meadow, scale_x, scale_z).saturating_mul(template_triangles(meadow))
}

/// Plants this meadow would place: `density × area`, on a square grid.
pub fn plant_count(meadow: &Meadow, scale_x: f32, scale_z: f32) -> u64 {
    let area = (scale_x.abs() as f64) * (scale_z.abs() as f64);
    if !area.is_finite() || area <= 0.0 || meadow.density <= 0.0 {
        return 0;
    }
    let wanted = area * meadow.density as f64;
    if !wanted.is_finite() || wanted <= 0.0 {
        return 0;
    }
    // A square grid, one plant per cell — see `scatter`. Rounding up to a whole
    // grid is what makes the count exact rather than approximately `density ×
    // area`, and validation has to agree with the generator to the plant.
    let side = (wanted.sqrt().ceil() as u64).max(1);
    side.saturating_mul(side)
}

/// Triangles in one plant.
fn template_triangles(meadow: &Meadow) -> u64 {
    let blades = meadow.blades.max(1) as u64;
    let segments = meadow.segments.max(1) as u64;
    // Two quads per segment — the midrib splits every rung in half.
    let blade_triangles = blades.saturating_mul(segments).saturating_mul(4);
    // A flower head and a seed head, each an octahedron.
    blade_triangles.saturating_add(16)
}

// ── the template ───────────────────────────────────────────────────────────

fn template(meadow: &Meadow) -> (Vec<MeadowVertex>, Vec<u32>) {
    let blades = meadow.blades.max(1);
    let segments = meadow.segments.max(1);
    let mut rng = seed_state(meadow.seed ^ 0x5EED_1EAF);
    let mut vertices = Vec::new();
    let mut indices = Vec::new();

    for blade in 0..blades {
        // The golden angle round the tuft, so blades never stack into a fan the
        // eye reads as a single flat card.
        let yaw = (GOLDEN_ANGLE * blade as f32).to_radians() + unit(&mut rng) * 0.4;
        // Outer blades splay further. Blade 0 stays near vertical, which is what
        // gives a tuft a centre rather than a hole.
        let reach = if blades > 1 {
            blade as f32 / (blades - 1) as f32
        } else {
            0.0
        };
        let splay = meadow.splay.to_radians() * reach * (0.7 + 0.6 * unit(&mut rng));
        // Outer blades are the shorter ones. A *signed* draw here would let a
        // blade come out longer than the template is tall, and the template's
        // unit height is what makes `Meadow.height` mean metres.
        let length = 1.0 - 0.25 * reach * unit_draw(&mut rng);

        // A tuft thickens as it grows: the outer blades arrive after the first
        // ones. Cheap, and it is most of the difference between a sprout and a
        // small clump of mature grass.
        let emerge = 0.04 + 0.14 * reach;

        emit_blade(
            &mut vertices,
            &mut indices,
            BladeSpec {
                yaw,
                splay,
                length,
                half_width: meadow.blade_width * 0.5,
                segments,
                emerge,
            },
        );
    }

    // The heads ride the tuft's centre line, at the tip of an upright blade.
    let flower_base = Vec3::new(0.0, 0.82, 0.0);
    // Elongated, not round. A ball at the top of a stem reads as a bead
    // threaded on it — grass flowers and seeds both grow as *spikelets*, and
    // stretching the same octahedron along the stem is the whole difference
    // between the two readings for no extra geometry.
    emit_head(
        &mut vertices,
        &mut indices,
        flower_base,
        meadow.head_size,
        1.9,
        FLOWER_WINDOW,
        ORGAN_FLOWER,
    );
    let seed_base = Vec3::new(0.0, 0.78, 0.0);
    emit_head(
        &mut vertices,
        &mut indices,
        seed_base,
        meadow.head_size * 0.8,
        2.8,
        SEED_HEAD_WINDOW,
        ORGAN_SEED_HEAD,
    );

    (vertices, indices)
}

struct BladeSpec {
    yaw: f32,
    splay: f32,
    length: f32,
    half_width: f32,
    segments: u32,
    emerge: f32,
}

/// One blade: a tapering strip of `segments` rungs, folded along its midrib.
///
/// The blade is built **straight-ish** — only its static splay is baked in.
/// Every other bend (the stage's `lean`, the wind) is applied in the vertex
/// stage, because those change with time and baking them here would put the
/// whole component back to minting a mesh per frame.
fn emit_blade(vertices: &mut Vec<MeadowVertex>, indices: &mut Vec<u32>, spec: BladeSpec) {
    let (sin_yaw, cos_yaw) = spec.yaw.sin_cos();
    // The blade's own frame: `out` is the direction it splays toward, `across`
    // is the width axis, perpendicular to both `out` and up.
    let out = Vec3::new(sin_yaw, 0.0, cos_yaw);
    let across = Vec3::new(cos_yaw, 0.0, -sin_yaw);

    let rungs = spec.segments + 1;
    let base = vertices.len() as u32;

    for rung in 0..rungs {
        let t = rung as f32 / spec.segments as f32;
        // A cantilever, applied here for the static splay and again in the
        // shader for everything that moves: the angle grows with t², so the
        // blade curves rather than hinging at its root.
        let angle = spec.splay * t * t;
        let (sin_a, cos_a) = angle.sin_cos();
        let along = spec.length * t;
        let centre = Vec3::Y * (along * cos_a) + out * (along * sin_a);

        // Widest at the base, tapering to a point. `sqrt` keeps the blade broad
        // most of the way up and then closes it quickly, which is a grass blade;
        // a linear taper draws a triangle.
        let width = spec.half_width * (1.0 - t).sqrt();
        // The fold, and the normal the fold implies. `up_local` is the blade's
        // face direction after the splay rotation.
        let up_local = (Vec3::Y * (-sin_a) + out * cos_a).normalize_or(out);
        let rise = width * MIDRIB_FOLD;

        // Each wing's normal tilts away from the midrib, which is the whole
        // point of folding: one light gives the tuft two tones.
        let wing =
            |sign: f32| (up_local * 1.0 + across * (sign * MIDRIB_FOLD)).normalize_or(Vec3::Y);

        for (offset, normal) in [
            (across * -width, wing(-1.0)),
            (up_local * rise, up_local),
            (across * width, wing(1.0)),
        ] {
            vertices.push(MeadowVertex {
                centre: centre.to_array(),
                normal: normal.to_array(),
                offset: offset.to_array(),
                // A blade grows out of the root.
                anchor: [0.0, 0.0, 0.0],
                span: [t, spec.emerge, 1.0],
                organ: ORGAN_BLADE,
            });
        }
    }

    for segment in 0..spec.segments {
        let a = base + segment * 3;
        let b = a + 3;
        // Left wing, then right wing. Counter-clockwise seen from the face side;
        // the pipeline draws double-sided anyway (`cull_mode: None`, so a blade
        // is visible from behind without emitting both faces), but a strip that
        // only accidentally faces outward would be a trap for anything else that
        // ever draws it.
        indices.extend_from_slice(&[a, b, a + 1, a + 1, b, b + 1]);
        indices.extend_from_slice(&[a + 1, b + 1, a + 2, a + 2, b + 1, b + 2]);
    }
}

/// A flower or seed head: an octahedron, stretched along the stem by `stretch`.
///
/// Eight triangles is not much of a flower, and it does not need to be — it is
/// three or four pixels tall at the distance a meadow is seen from, and what it
/// has to do is put a *different colour* at the top of the plant during the
/// stages that have one.
fn emit_head(
    vertices: &mut Vec<MeadowVertex>,
    indices: &mut Vec<u32>,
    base_point: Vec3,
    size: f32,
    stretch: f32,
    window: (f32, f32),
    organ: u32,
) {
    let base = vertices.len() as u32;
    let half = size * 0.5;
    // The head's whole extent rides in `offset`, in metres — `base_point` is
    // where it attaches to the stem, in unit-height space. Because `centre` and
    // `anchor` are then the same point, the emerge/wither scaling acts on the
    // offsets alone: the head opens *at* the top of the stem instead of sliding
    // up it.
    let hub = Vec3::Y * (half * stretch);
    let offsets = [
        hub + Vec3::Y * (half * stretch),
        hub - Vec3::Y * (half * stretch),
        hub + Vec3::X * half,
        hub - Vec3::X * half,
        hub + Vec3::Z * half,
        hub - Vec3::Z * half,
    ];

    for offset in offsets {
        vertices.push(MeadowVertex {
            centre: base_point.to_array(),
            normal: (offset - hub).normalize_or(Vec3::Y).to_array(),
            offset: offset.to_array(),
            anchor: base_point.to_array(),
            // The head rides at the plant's tip, so it takes the tip's bend and
            // the tip's share of the wind.
            span: [1.0, window.0, window.1],
            organ,
        });
    }

    // Top fan, then bottom fan.
    const FACES: [[u32; 3]; 8] = [
        [0, 2, 4],
        [0, 4, 3],
        [0, 3, 5],
        [0, 5, 2],
        [1, 4, 2],
        [1, 3, 4],
        [1, 5, 3],
        [1, 2, 5],
    ];
    for face in FACES {
        indices.extend_from_slice(&[base + face[0], base + face[1], base + face[2]]);
    }
}

// ── placement ──────────────────────────────────────────────────────────────

/// Place the plants, in world space.
///
/// A **jittered grid**, not a free scatter: one plant per cell, offset within
/// it. Uniform random placement clumps — it leaves bald patches next to knots of
/// four, and at a meadow's densities that reads as a bug in the generator rather
/// than as nature. Stratifying costs nothing and is also what gives the reseed
/// jitter a bound to stay inside (§3), so the field cannot drift into clumps
/// over many generations either.
fn scatter(meadow: &Meadow, model: Mat4, ground: Option<Ground<'_>>) -> (Vec<MeadowInstance>, f32) {
    let scale_x = model.x_axis.truncate().length();
    let scale_z = model.z_axis.truncate().length();
    let plants = plant_count(meadow, scale_x, scale_z);
    if plants == 0 {
        return (Vec::new(), 0.0);
    }
    let side = (plants as f64).sqrt().round() as u32;
    let side = side.max(1);

    // Plant height in metres. `Transform.scale.y` multiplies it, the way it
    // multiplies a `Terrain`'s relief — so a meadow can be flattened without
    // rewriting every stage's height.
    let height_scale = model.y_axis.truncate().length().max(0.0);
    let cell = 1.0 / side as f32;
    let jitter_radius = cell * 0.5 * scale_x.max(scale_z);
    let max_slope = meadow.max_slope.to_radians().tan();

    let mut rng = seed_state(meadow.seed);
    let mut instances = Vec::with_capacity(plants as usize);

    for row in 0..side {
        for column in 0..side {
            // Draw every random number for every cell, whether or not the plant
            // survives the slope test. A draw skipped on rejection would make
            // the whole rest of the field depend on the terrain under one cell,
            // so raising a hill at one corner would reshuffle the grass at the
            // other — the trap M17's "defaulted fields consume no randomness"
            // rule is the general form of.
            let jitter_u = unit(&mut rng);
            let jitter_v = unit(&mut rng);
            let yaw = unit_draw(&mut rng) * std::f32::consts::TAU;
            let size = 1.0 + unit(&mut rng) * meadow.size_jitter;
            let phase_offset = unit_draw(&mut rng) * meadow.stagger;
            let seed = next_u32(&mut rng);

            // The cell's centre in the unit square, plus a half-cell jitter.
            let local = Vec3::new(
                -0.5 + (column as f32 + 0.5) * cell + jitter_u * cell * 0.5,
                0.0,
                -0.5 + (row as f32 + 0.5) * cell + jitter_v * cell * 0.5,
            );
            let world = model.transform_point3(local);

            let (y, gradient) = match ground {
                Some(ground) => {
                    let height = crate::terrain::world_height_at(
                        ground.terrain,
                        ground.transform,
                        world.x,
                        world.z,
                    );
                    // One grid quad of the terrain is the right sampling step —
                    // the slope the *geometry* carries, not detail the
                    // tessellation dropped. `gradient_at` is M22's, and reusing
                    // it is the point of it having been extracted.
                    let spacing =
                        ground.transform.scale.x.abs() / ground.terrain.segments.max(1) as f32;
                    let slope = crate::terrain::gradient_at(
                        ground.terrain,
                        world.x,
                        world.z,
                        spacing.max(1e-3),
                    ) * ground.transform.scale.y;
                    (height, slope)
                }
                None => (world.y, Vec2::ZERO),
            };

            // Grass does not grow on a cliff. `>=` so `max_slope: 0` is "flat
            // ground only" rather than "nothing at all".
            if gradient.length() > max_slope && meadow.max_slope < 90.0 {
                continue;
            }

            instances.push(MeadowInstance {
                position: [world.x, y, world.z],
                scale: meadow.height * size * height_scale,
                yaw,
                phase_offset,
                ground_gradient: gradient.to_array(),
                seed,
            });
        }
    }

    (instances, jitter_radius)
}

// ── the cache key ──────────────────────────────────────────────────────────

/// Everything the geometry depends on, compared bit for bit.
///
/// **The transform and the terrain are in here, and that is the load-bearing
/// part.** A meadow's instances are placed in world space against the ground it
/// stands on, so keying on the component's own fields alone would leave a moved
/// meadow — or a re-shaped terrain under a still one — with its grass floating
/// in the air at the old altitude. `terrain_moves_rebuild_the_patch` pins it.
#[derive(PartialEq, Eq, Hash)]
struct PatchKey {
    meadow: [u32; 12],
    model: [u32; 16],
    ground: Option<Box<[u32]>>,
}

impl PatchKey {
    fn of(meadow: &Meadow, model: Mat4, ground: Option<Ground<'_>>) -> Self {
        Self {
            meadow: [
                meadow.seed,
                meadow.blades,
                meadow.segments,
                meadow.density.to_bits(),
                meadow.height.to_bits(),
                meadow.blade_width.to_bits(),
                meadow.splay.to_bits(),
                meadow.head_size.to_bits(),
                meadow.size_jitter.to_bits(),
                meadow.stagger.to_bits(),
                meadow.max_slope.to_bits(),
                // `cycle_length`, `phase`, `wind` and every stage colour reach
                // the shader as uniforms and cannot move a vertex, so two
                // meadows differing only in those share one upload — `cloud.rs`'s
                // rule.
                0,
            ],
            model: model.to_cols_array().map(f32::to_bits),
            ground: ground.map(|ground| {
                let terrain = ground.terrain;
                let transform = ground.transform;
                vec![
                    terrain.seed,
                    terrain.segments,
                    terrain.octaves,
                    terrain.height.to_bits(),
                    terrain.feature_scale.to_bits(),
                    terrain.warp.to_bits(),
                    terrain.persistence.to_bits(),
                    transform.position.x.to_bits(),
                    transform.position.y.to_bits(),
                    transform.position.z.to_bits(),
                    transform.scale.x.to_bits(),
                    transform.scale.y.to_bits(),
                    transform.scale.z.to_bits(),
                ]
                .into_boxed_slice()
            }),
        }
    }
}

// ── the generator's randomness ─────────────────────────────────────────────
//
// One xorshift32, written out here rather than pulled from a crate, because a
// meadow render sits under a baseline and the sequence is therefore a format
// contract — see the module docs.

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

fn next_u32(state: &mut u32) -> u32 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *state = x;
    x
}

/// A draw in `[0, 1)`.
fn unit_draw(state: &mut u32) -> f32 {
    (next_u32(state) >> 8) as f32 / (1u32 << 24) as f32
}

/// A draw in `[-1, 1)`.
fn unit(state: &mut u32) -> f32 {
    unit_draw(state) * 2.0 - 1.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::Transform;

    fn field() -> Meadow {
        Meadow {
            seed: 4,
            density: 9.0,
            ..Default::default()
        }
    }

    /// A 10 × 10 patch, so a plant count is easy to reason about by hand.
    fn model() -> Mat4 {
        Mat4::from_scale(Vec3::new(10.0, 1.0, 10.0))
    }

    #[test]
    fn the_count_validation_refuses_is_the_count_the_generator_places() {
        // `meadow_too_complex` fires before anything is allocated, so its
        // arithmetic has to agree with the generator's to the plant — an
        // estimate would let a scene through that then blows the budget, or
        // refuse one that fits.
        for density in [0.5, 9.0, 40.0, 137.0] {
            let meadow = Meadow { density, ..field() };
            let patch = generate(&meadow, model(), None);
            assert_eq!(
                plant_count(&meadow, 10.0, 10.0) as usize,
                patch.instances.len(),
                "density {density}"
            );
            assert_eq!(
                triangle_count(&meadow, 10.0, 10.0) as usize,
                patch.triangle_count(),
            );
        }
    }

    #[test]
    fn a_zero_density_field_is_empty_rather_than_an_error() {
        let meadow = Meadow {
            density: 0.0,
            ..field()
        };
        assert_eq!(plant_count(&meadow, 10.0, 10.0), 0);
        assert!(generate(&meadow, model(), None).instances.is_empty());
    }

    #[test]
    fn the_same_seed_grows_the_same_field() {
        let meadow = field();
        assert_eq!(
            generate(&meadow, model(), None),
            generate(&meadow, model(), None)
        );

        let other = Meadow {
            seed: 5,
            ..meadow.clone()
        };
        assert_ne!(
            generate(&meadow, model(), None).instances,
            generate(&other, model(), None).instances,
            "a different seed must be a different field"
        );
    }

    #[test]
    fn one_patch_per_description_is_shared() {
        // The renderer's upload cache keys on this pointer (M15); a fresh `Arc`
        // per frame would re-upload every blade of grass in the scene.
        let meadow = field();
        assert!(Arc::ptr_eq(
            &patch_for(&meadow, model(), None),
            &patch_for(&meadow, model(), None)
        ));
    }

    #[test]
    fn moving_a_meadow_rebuilds_it() {
        // Instances are world-space, so the transform is part of what the
        // geometry *is* — not, as with every other recipe component, something
        // the renderer applies afterwards.
        let meadow = field();
        let moved = Mat4::from_translation(Vec3::new(3.0, 0.0, 0.0)) * model();
        let here = patch_for(&meadow, model(), None);
        let there = patch_for(&meadow, moved, None);
        assert!(!Arc::ptr_eq(&here, &there));
        assert_ne!(here.instances[0].position, there.instances[0].position);
    }

    #[test]
    fn terrain_moves_rebuild_the_patch() {
        // The trap the cache key exists for: a meadow's plants stand at
        // altitudes sampled from another entity, so keying on the meadow's own
        // fields alone would leave a re-shaped or moved terrain with grass
        // floating at the old ground's height.
        let meadow = Meadow {
            terrain: Some("Ground".into()),
            ..field()
        };
        let terrain = Terrain {
            height: 4.0,
            ..Default::default()
        };
        let flat = Transform::default();
        let lifted = Transform {
            position: Vec3::new(0.0, 7.0, 0.0),
            ..Default::default()
        };

        let low = patch_for(
            &meadow,
            model(),
            Some(Ground {
                terrain: &terrain,
                transform: &flat,
            }),
        );
        let high = patch_for(
            &meadow,
            model(),
            Some(Ground {
                terrain: &terrain,
                transform: &lifted,
            }),
        );
        assert!(!Arc::ptr_eq(&low, &high));
        assert!(
            (high.instances[0].position[1] - low.instances[0].position[1] - 7.0).abs() < 1e-3,
            "the whole field should rise with the ground it stands on"
        );

        // And re-shaping the terrain under a still meadow counts too.
        let rougher = Terrain {
            height: 9.0,
            ..terrain.clone()
        };
        let reshaped = patch_for(
            &meadow,
            model(),
            Some(Ground {
                terrain: &rougher,
                transform: &flat,
            }),
        );
        assert!(!Arc::ptr_eq(&low, &reshaped));
    }

    #[test]
    fn plants_stand_on_the_terrain_they_name() {
        let meadow = Meadow {
            terrain: Some("Ground".into()),
            max_slope: 90.0,
            ..field()
        };
        let terrain = Terrain {
            height: 3.0,
            feature_scale: 12.0,
            ..Default::default()
        };
        let transform = Transform {
            scale: Vec3::new(40.0, 1.0, 40.0),
            ..Default::default()
        };
        let patch = generate(
            &meadow,
            model(),
            Some(Ground {
                terrain: &terrain,
                transform: &transform,
            }),
        );

        assert!(!patch.instances.is_empty());
        for plant in &patch.instances {
            let expected = crate::terrain::world_height_at(
                &terrain,
                &transform,
                plant.position[0],
                plant.position[2],
            );
            assert!(
                (plant.position[1] - expected).abs() < 1e-4,
                "a plant at {:?} sits at {} but the ground is at {expected}",
                plant.position,
                plant.position[1]
            );
        }
        // A meadow on relief must actually vary in height, or the test above
        // would pass just as well against a flat field.
        let highest = patch
            .instances
            .iter()
            .map(|p| p.position[1])
            .fold(f32::MIN, f32::max);
        let lowest = patch
            .instances
            .iter()
            .map(|p| p.position[1])
            .fold(f32::MAX, f32::min);
        assert!(highest - lowest > 0.5);
    }

    #[test]
    fn grass_does_not_grow_on_a_cliff() {
        let terrain = Terrain {
            height: 40.0,
            feature_scale: 8.0,
            ..Default::default()
        };
        let transform = Transform {
            scale: Vec3::new(30.0, 1.0, 30.0),
            ..Default::default()
        };
        let ground = Ground {
            terrain: &terrain,
            transform: &transform,
        };

        let steep = generate(
            &Meadow {
                terrain: Some("Ground".into()),
                max_slope: 90.0,
                ..field()
            },
            model(),
            Some(ground),
        );
        let gentle = generate(
            &Meadow {
                terrain: Some("Ground".into()),
                max_slope: 10.0,
                ..field()
            },
            model(),
            Some(ground),
        );
        assert!(
            gentle.instances.len() < steep.instances.len(),
            "a low max_slope must drop the plants on the steep ground"
        );
    }

    #[test]
    fn the_slope_test_does_not_reshuffle_the_rest_of_the_field() {
        // Every cell draws its full set of random numbers whether or not its
        // plant survives, so raising a hill under one corner cannot move the
        // grass at another. M17's "defaulted fields consume no randomness",
        // generalized.
        let meadow = Meadow {
            terrain: Some("Ground".into()),
            max_slope: 20.0,
            ..field()
        };
        let terrain = Terrain {
            height: 25.0,
            feature_scale: 9.0,
            ..Default::default()
        };
        let transform = Transform {
            scale: Vec3::new(30.0, 1.0, 30.0),
            ..Default::default()
        };
        let culled = generate(
            &meadow,
            model(),
            Some(Ground {
                terrain: &terrain,
                transform: &transform,
            }),
        );
        let everything = generate(
            &Meadow {
                max_slope: 90.0,
                ..meadow.clone()
            },
            model(),
            Some(Ground {
                terrain: &terrain,
                transform: &transform,
            }),
        );

        assert!(culled.instances.len() < everything.instances.len());
        // Every survivor appears in the uncut field with identical parameters.
        for plant in &culled.instances {
            assert!(
                everything.instances.contains(plant),
                "the slope test changed a plant it did not remove: {plant:?}"
            );
        }
    }

    #[test]
    fn the_template_is_a_unit_tall_plant_with_every_organ_on_it() {
        let (vertices, indices) = template(&field());
        assert!(!indices.is_empty());
        assert_eq!(indices.len() % 3, 0);
        for index in &indices {
            assert!((*index as usize) < vertices.len());
        }

        // Unit-height space: the shader multiplies `centre` by the stage's
        // height in metres, so a template taller than 1 would make `height`
        // mean something other than metres.
        let tallest = vertices
            .iter()
            .map(|v| v.centre[1])
            .fold(f32::MIN, f32::max);
        assert!((0.5..=1.0).contains(&tallest), "tallest centre {tallest}");
        assert!(vertices.iter().all(|v| v.centre[1] >= -1e-6));

        // Every organ the cycle needs is in the buffer, always.
        for organ in [ORGAN_BLADE, ORGAN_FLOWER, ORGAN_SEED_HEAD] {
            assert!(vertices.iter().any(|v| v.organ == organ), "organ {organ}");
        }
        // A blade exists for the whole cycle and scales from the root; a head
        // has a window and opens at its own attachment point.
        for v in &vertices {
            if v.organ == ORGAN_BLADE {
                assert_eq!(v.span[2], 1.0);
                assert_eq!(v.anchor, [0.0, 0.0, 0.0]);
            } else {
                assert!(v.span[1] > 0.0 && v.span[2] < 1.0);
                assert!(v.anchor[1] > 0.0, "a head must open at the top of a stem");
            }
        }
    }

    #[test]
    fn the_flower_and_the_seed_head_do_not_share_the_cycle() {
        // They are the same stem at two stages, so an overlap would render both
        // at once — a plant flowering and seeding in the same frame.
        assert!(FLOWER_WINDOW.0 < SEED_HEAD_WINDOW.0);
        assert!(SEED_HEAD_WINDOW.1 > FLOWER_WINDOW.1);
    }

    #[test]
    fn the_default_life_cycle_is_a_closed_loop() {
        let stages = Meadow::default().stages;
        assert!(stages.len() >= 2 && stages.len() <= MAX_GROWTH_STAGES);
        for pair in stages.windows(2) {
            assert!(pair[1].at > pair[0].at, "stages must strictly increase");
        }
        assert!((0.0..1.0).contains(&stages[0].at));
        assert!(stages.last().unwrap().at < 1.0);
        // It starts and ends at nothing: the table wraps, so the last keyframe
        // fades back into the first, and a cycle that ended tall would snap.
        assert_eq!(stages[0].height, 0.0);
        assert!(stages.last().unwrap().height < 0.7);
    }

    #[test]
    fn colour_and_clock_fields_do_not_rebuild_the_geometry() {
        // They reach the shader as uniforms and cannot move a vertex, so two
        // meadows differing only in those share one upload — `cloud.rs`'s rule.
        let meadow = field();
        let recoloured = Meadow {
            cycle_length: 12.0,
            phase: 0.9,
            wind: 40.0,
            wind_speed: 9.0,
            wind_direction: 180.0,
            flower_color: Vec3::new(0.9, 0.1, 0.1),
            stages: vec![],
            ..meadow.clone()
        };
        assert!(Arc::ptr_eq(
            &patch_for(&meadow, model(), None),
            &patch_for(&recoloured, model(), None)
        ));
    }
}
