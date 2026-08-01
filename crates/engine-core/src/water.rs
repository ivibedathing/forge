//! Water surfaces (M18): the tessellated grid a [`Water`] component draws on,
//! the conventions its waves follow, and — since M41 — the CPU mirror of the
//! wave sum the GPU draws.
//!
//! The surface is still **drawn** entirely in `water.wgsl`'s vertex stage, and
//! that is not going to change: a 96×96 surface is 9409 vertices, and displacing
//! them here would mint a new `Arc<MeshData>` every frame — a per-frame
//! re-upload plus one entry per frame accumulating in the renderer's mesh cache
//! (M15). What M41 adds is not a second way to *draw* water; it is the answer to
//! "how high is the water at (x, z)", which the GPU cannot hand back and which
//! buoyancy, `world.water_height` and `engine water-height` all need.
//!
//! So [`sample_at`] is a deliberate second implementation of one curve — the
//! pattern `CLAUDE.md` warns about, unavoidable here because one side has to run
//! on the GPU. It is held to the shader by a real GPU agreement test
//! (`engine-render/tests/water.rs`), which reads the drawn surface back out of a
//! render and compares it against this file. **Change the wave arithmetic in
//! either place and that test is what tells you the two have drifted.**
//!
//! [`Water`]: crate::components::Water

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use glam::{Mat4, Vec2, Vec3};

use crate::components::{Transform, Water, Wave};
use crate::mesh::MeshData;

/// Most waves one surface may sum. Beyond this the shader has nowhere to put
/// them (the uniform array is fixed-size) and the sum stops reading as water
/// anyway — validation rejects the scene rather than silently dropping waves.
pub const MAX_WAVES: usize = 8;

/// The direction a wave travels, as a unit vector in the XZ plane.
///
/// `degrees` is a yaw about +Y applied to the engine's forward axis, so 0°
/// travels toward **−Z** and 90° toward **−X** — the same "aim it like an
/// entity's local −Z" convention the camera, the lights and the particle cone
/// already use. One function so the shader's packing, validation and the
/// documentation cannot disagree about which way 45° points.
pub fn wave_direction(degrees: f32) -> Vec2 {
    let (sin, cos) = degrees.to_radians().sin_cos();
    Vec2::new(-sin, -cos)
}

/// Most fixed-point steps [`Surface::sample_at`] will take to invert the waves'
/// horizontal gather, and the tolerance in metres it stops early at.
///
/// The iteration contracts by a factor of `Σ steepness` per step (see
/// [`Surface::base_under`]), so a scene at the validator's limit of 1.0 is the
/// only one that reaches the cap — and at exactly 1.0 the surface is folding,
/// which is a shape with no single answer anyway. A typical lake at `Σ ≈ 0.5`
/// is under a micrometre in 20.
pub const MAX_SOLVE_STEPS: usize = 32;
/// See [`MAX_SOLVE_STEPS`]. Metres.
pub const SOLVE_TOLERANCE: f32 = 1e-5;

/// The surface at one world XZ column: where it is, and which way it faces.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WaterSample {
    /// World Y of the surface, in metres — a coordinate a caller can assign to
    /// a position directly, exactly as [`crate::terrain::world_height_at`]
    /// returns one.
    pub height: f32,
    /// Unit surface normal, from the analytic derivatives of the same wave sum.
    ///
    /// This is the **geometry's** normal, and deliberately not the one the
    /// pixels have: M18's per-pixel ripples are a slope field with no height
    /// behind them, and nothing physical may depend on them. A boat sits on the
    /// surface the waves make, not on the one the glitter suggests.
    pub normal: Vec3,
}

/// One wave in the form the shader consumes it, which is not the form the file
/// authors it in.
///
/// The conversion is `water_uniform`'s, and it has to stay `water_uniform`'s:
/// `k = 2π/λ`, `ω = speed·k`, and `Q = steepness/(k·A)`. That last one is what
/// makes each wave's contribution to the horizontal Jacobian equal to its own
/// `steepness`, which is what makes `Σ steepness ≤ 1` exactly the non-folding
/// condition — and, since M41, exactly the condition under which
/// [`Surface::base_under`] converges.
#[derive(Debug, Clone, Copy, Default)]
struct PackedWave {
    direction: Vec2,
    amplitude: f32,
    k: f32,
    q: f32,
    omega: f32,
}

impl PackedWave {
    fn new(wave: &Wave) -> Self {
        let k = std::f32::consts::TAU / wave.wavelength.max(1e-4);
        // A wave with no amplitude has no crests to gather toward, so its Q is
        // 0 rather than a division by zero — `water_uniform`'s guard, and the
        // two must agree on it or a zero-amplitude wave reads as NaN on one
        // side and as nothing on the other.
        let q = if wave.amplitude > 0.0 {
            wave.steepness / (k * wave.amplitude)
        } else {
            0.0
        };
        Self {
            direction: wave_direction(wave.direction),
            amplitude: wave.amplitude,
            k,
            q,
            omega: wave.speed * k,
        }
    }
}

/// One water surface, prepared for querying: the waves packed as the shader
/// packs them, and the patch's placement.
///
/// Built once and asked many times, because that is the shape of every real
/// caller — buoyancy samples one hull at four columns every fixed step, and
/// re-deriving `k`, `ω` and `Q` for each of them is arithmetic nobody needs to
/// repeat. One-shot callers want the free [`sample_at`] instead.
#[derive(Debug, Clone)]
pub struct Surface {
    waves: [PackedWave; MAX_WAVES],
    count: usize,
    /// The patch's `Transform` as a matrix: the unit grid's placement, and so
    /// where the *undisturbed* surface is before any wave moves it.
    model: Mat4,
}

impl Surface {
    /// Prepare a water entity's surface for querying.
    pub fn new(water: &Water, transform: &Transform) -> Self {
        let mut waves = [PackedWave::default(); MAX_WAVES];
        let count = water.waves.len().min(MAX_WAVES);
        for (slot, wave) in water.waves.iter().take(count).enumerate() {
            waves[slot] = PackedWave::new(wave);
        }
        Self {
            waves,
            count,
            model: transform.matrix(),
        }
    }

    /// How far the waves gather a point sitting at world XZ `base` — the
    /// horizontal half of the Gerstner displacement, on its own.
    ///
    /// Split out because inverting *this* is the whole difficulty of asking a
    /// Gerstner surface a vertical question (see the module docs and
    /// `designs/buoyancy-design.md` §3).
    fn gather(&self, base: Vec2, time: f32) -> Vec2 {
        let mut offset = Vec2::ZERO;
        for wave in &self.waves[..self.count] {
            let phase = wave.k * wave.direction.dot(base) - wave.omega * time;
            offset += wave.direction * (wave.q * wave.amplitude * phase.cos());
        }
        offset
    }

    /// The **undisturbed** XZ position whose displaced position stands over
    /// `query` — the inverse of [`gather`](Self::gather).
    ///
    /// The shader is told a base point and computes where it lands; every
    /// caller here has the opposite question, because a boat is at an XZ and
    /// wants the water above it. Gerstner waves move the surface toward their
    /// crests as well as up, so the surface point over `(x, z)` did not start
    /// at `(x, z)` — at `steepness 0.4` on a 4 m wave the crest has travelled a
    /// quarter of a metre sideways, which is a quarter of a boat.
    ///
    /// Solved by fixed-point iteration, which converges **exactly when the
    /// scene validates**: the gather's Jacobian has spectral radius bounded by
    /// `Σ steepness`, the same sum `water_waves_self_intersect` refuses to let
    /// exceed 1. That is not luck. A surface that folds is a surface with two
    /// answers to "how high is the water here", so the rule that keeps the
    /// render from curling into loops is the rule that makes this query
    /// well-posed at all.
    fn base_under(&self, query: Vec2, time: f32) -> Vec2 {
        let mut base = query;
        for _ in 0..MAX_SOLVE_STEPS {
            let next = query - self.gather(base, time);
            let converged = (next - base).abs().max_element() <= SOLVE_TOLERANCE;
            base = next;
            if converged {
                break;
            }
        }
        base
    }

    /// Where the unit grid's `(u, v)` lands under a world XZ point, or `None`
    /// when the patch is edge-on and has no answer.
    ///
    /// A 2×2 solve against the model matrix's X and Z columns rather than
    /// `position.y + …`, so a **rotated** water entity is an ordinary case
    /// instead of a silently wrong one.
    fn grid_coords(&self, base: Vec2) -> Option<Vec2> {
        let (x_axis, z_axis, origin) = (self.model.x_axis, self.model.z_axis, self.model.w_axis);
        let determinant = x_axis.x * z_axis.z - z_axis.x * x_axis.z;

        // The determinant *is* the Y component of the patch's own normal
        // (`x_axis × z_axis`), so testing it against that normal's length is
        // testing whether the surface is edge-on — the only shape with no
        // answer here. The comparison has to be relative: an absolute epsilon
        // is a fixed tolerance on a quantity that scales with the patch's area,
        // and it lets a 20 m pond stood on its side through while rejecting a
        // legitimate 1 cm puddle.
        //
        // Negated so a NaN fails rather than passes — the house convention from
        // validation, and reachable here through a NaN scale.
        let area = x_axis.truncate().cross(z_axis.truncate()).length();
        // Negated deliberately: `!(a > b)` is not `a <= b` when either side is
        // NaN, and a NaN here (reachable through a NaN scale) must fail into
        // "no answer" rather than pass into an invented one.
        #[allow(clippy::neg_cmp_op_on_partial_ord)]
        if !(determinant.abs() > 1e-6 * area) {
            return None;
        }
        let (dx, dz) = (base.x - origin.x, base.y - origin.z);
        Some(Vec2::new(
            (dx * z_axis.z - dz * z_axis.x) / determinant,
            (dz * x_axis.x - dx * x_axis.z) / determinant,
        ))
    }

    /// The surface over a world XZ position at `time` seconds, or `None` when
    /// that column is **outside the patch**.
    ///
    /// Outside is a different answer from "the water is at 0.0", and the
    /// difference is exactly what a hull drifting off the edge of a pond needs:
    /// water that ends is water a boat falls out of.
    pub fn sample_at(&self, x: f32, z: f32, time: f32) -> Option<WaterSample> {
        let base = self.base_under(Vec2::new(x, z), time);
        let grid = self.grid_coords(base)?;
        if grid.x.abs() > 0.5 || grid.y.abs() > 0.5 {
            return None;
        }

        // The rest plane's height under the base point. Read through the model
        // matrix rather than taken from `position.y`, so a rotated or non-
        // uniformly scaled patch answers about the surface it actually draws.
        let rest = self.model.transform_point3(Vec3::new(grid.x, 0.0, grid.y));

        // The vertical half of the displacement, and the normal from the same
        // sines and cosines — the shader's arithmetic, in the shader's order.
        let mut height = 0.0;
        let mut dx = Vec3::X;
        let mut dz = Vec3::Z;
        for wave in &self.waves[..self.count] {
            let phase = wave.k * wave.direction.dot(base) - wave.omega * time;
            let (sin, cos) = phase.sin_cos();
            let ka = wave.k * wave.amplitude;
            let qka = wave.q * ka;

            height += wave.amplitude * sin;
            dx += Vec3::new(
                -qka * wave.direction.x * wave.direction.x * sin,
                ka * wave.direction.x * cos,
                -qka * wave.direction.x * wave.direction.y * sin,
            );
            dz += Vec3::new(
                -qka * wave.direction.y * wave.direction.x * sin,
                ka * wave.direction.y * cos,
                -qka * wave.direction.y * wave.direction.y * sin,
            );
        }

        Some(WaterSample {
            height: rest.y + height,
            // `cross(dz, dx)`, in that order: what gives +Y on flat water.
            normal: dz.cross(dx).normalize(),
        })
    }
}

/// The surface of one water entity over a world XZ position, at `time` seconds
/// — what `world.water_height`, [`Scene::water_sample`] and
/// `engine water-height` all resolve through (M41).
///
/// `None` when the column is outside the patch. The terrain twin
/// ([`crate::terrain::world_height_at`]) cannot fail because a height field is
/// defined everywhere its formula is; a water patch is a bounded body, and
/// pretending otherwise is how a boat floats on dry land.
///
/// Prefer [`Surface`] when asking repeatedly about one entity — this packs the
/// waves afresh on every call.
///
/// [`Scene::water_sample`]: crate::scene::Scene::water_sample
pub fn sample_at(
    water: &Water,
    transform: &Transform,
    x: f32,
    z: f32,
    time: f32,
) -> Option<WaterSample> {
    Surface::new(water, transform).sample_at(x, z, time)
}

thread_local! {
    /// One grid per tessellation per thread. The `MeshSource` contract's "same
    /// asset, same `Arc`" rule applies here for a sharper reason than sharing
    /// an allocation: the renderer keys its uploaded vertex buffers on this
    /// pointer, so a stable `Arc` is the difference between uploading the
    /// surface once and uploading it every frame.
    static GRID_CACHE: RefCell<HashMap<u32, Arc<MeshData>>> = RefCell::new(HashMap::new());
}

/// The unit surface, tessellated into `segments`×`segments` quads.
///
/// Geometry matches `builtin:plane` exactly at `segments == 1`: 1×1 in XZ,
/// centred on the origin, lying at y = 0, normals +Y, wound counter-clockwise
/// seen from above. `Transform.scale` sizes it like any other mesh — a pond is
/// a scaled unit surface, so there is no second way to say how big the water
/// is, and the waves are evaluated in world space, so scaling never stretches
/// them.
pub fn surface_grid(segments: u32) -> Arc<MeshData> {
    let segments = segments.max(1);
    GRID_CACHE.with(|cache| {
        Arc::clone(
            cache
                .borrow_mut()
                .entry(segments)
                .or_insert_with(|| Arc::new(build_grid(segments))),
        )
    })
}

fn build_grid(segments: u32) -> MeshData {
    let n = segments as usize;
    let step = 1.0 / segments as f32;
    let vertices = (n + 1) * (n + 1);

    let mut mesh = MeshData {
        positions: Vec::with_capacity(vertices),
        normals: Vec::with_capacity(vertices),
        uvs: Vec::with_capacity(vertices),
        indices: Vec::with_capacity(n * n * 6),
        ..MeshData::default()
    };

    for i in 0..=n {
        for j in 0..=n {
            let x = -0.5 + i as f32 * step;
            let z = -0.5 + j as f32 * step;
            mesh.positions.push([x, 0.0, z]);
            mesh.normals.push([0.0, 1.0, 0.0]);
            // u along Z, v along X — `builtin:plane`'s convention.
            mesh.uvs.push([j as f32 * step, i as f32 * step]);
        }
    }

    let index = |i: usize, j: usize| (i * (n + 1) + j) as u32;
    for i in 0..n {
        for j in 0..n {
            let (a, b, c, d) = (
                index(i, j),
                index(i, j + 1),
                index(i + 1, j + 1),
                index(i + 1, j),
            );
            // Same corner order as `quad()`: counter-clockwise from above, so
            // the surface survives the engine's back-face culling. (The water
            // pipeline draws double-sided anyway — see `water.wgsl` — but a
            // grid that only accidentally faces up would be a trap for anything
            // else that ever draws it.)
            mesh.indices.extend_from_slice(&[a, b, c, a, c, d]);
        }
    }

    mesh
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::BuiltinMesh;
    use glam::Vec3;

    #[test]
    fn one_segment_is_the_builtin_plane() {
        let grid = surface_grid(1);
        let plane = BuiltinMesh::Plane.data();

        // Same surface, so the same triangles cover the same area — vertex
        // order differs (the grid walks a lattice), so compare geometry.
        assert_eq!(grid.triangle_count(), plane.triangle_count());
        let extent = |mesh: &MeshData| {
            mesh.positions.iter().fold(
                (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)),
                |(min, max), p| {
                    let p = Vec3::from_array(*p);
                    (min.min(p), max.max(p))
                },
            )
        };
        assert_eq!(extent(&grid), extent(&plane));
    }

    #[test]
    fn every_triangle_faces_up() {
        let grid = surface_grid(5);
        for triangle in grid.indices.chunks_exact(3) {
            let p = |i: u32| Vec3::from_array(grid.positions[i as usize]);
            let (a, b, c) = (p(triangle[0]), p(triangle[1]), p(triangle[2]));
            let normal = (b - a).cross(c - a).normalize();
            assert!(
                normal.dot(Vec3::Y) > 0.99,
                "triangle {triangle:?} winds downward: normal {normal}"
            );
        }
    }

    #[test]
    fn tessellation_scales_with_segments() {
        assert_eq!(surface_grid(4).triangle_count(), 32);
        assert_eq!(surface_grid(4).vertex_count(), 25);
        assert_eq!(surface_grid(64).triangle_count(), 64 * 64 * 2);
    }

    #[test]
    fn one_grid_per_tessellation_is_shared() {
        // The renderer's geometry cache keys on this pointer; a fresh `Arc` per
        // frame would re-upload the whole surface every frame.
        assert!(Arc::ptr_eq(&surface_grid(32), &surface_grid(32)));
        assert!(!Arc::ptr_eq(&surface_grid(32), &surface_grid(33)));
    }

    /// A pond 20 m across, centred on the origin at y = 2, with `waves`.
    fn pond(waves: Vec<Wave>) -> (Water, Transform) {
        (
            Water {
                waves,
                ..Water::default()
            },
            Transform {
                position: Vec3::new(0.0, 2.0, 0.0),
                scale: Vec3::new(20.0, 1.0, 20.0),
                ..Transform::default()
            },
        )
    }

    #[test]
    fn flat_water_is_the_patch_it_sits_on() {
        let (water, transform) = pond(Vec::new());
        let sample = sample_at(&water, &transform, 3.0, -7.0, 12.5).expect("inside the patch");
        assert_eq!(sample.height, 2.0);
        assert!((sample.normal - Vec3::Y).length() < 1e-6);
    }

    #[test]
    fn outside_the_patch_is_none_rather_than_zero() {
        // The whole point of the `Option`: a hull that drifts off the edge of a
        // pond must fall out of the water, not float on the plane the pond
        // would have had if it were infinite.
        let (water, transform) = pond(Vec::new());
        assert!(sample_at(&water, &transform, 9.9, 0.0, 0.0).is_some());
        assert!(sample_at(&water, &transform, 10.1, 0.0, 0.0).is_none());
        assert!(sample_at(&water, &transform, 0.0, -10.1, 0.0).is_none());
    }

    #[test]
    fn a_wave_without_steepness_is_exactly_its_sine() {
        // `steepness: 0` switches the horizontal gather off, so the surface is
        // a plain travelling sine and the answer has a closed form to check
        // against — which pins the direction convention, `k = 2π/λ` and
        // `ω = speed·k` independently of the fixed-point solve.
        let wave = Wave {
            direction: 0.0,
            wavelength: 8.0,
            amplitude: 0.5,
            steepness: 0.0,
            speed: 2.0,
        };
        let (water, transform) = pond(vec![wave]);
        let k = std::f32::consts::TAU / 8.0;

        for (x, z, time) in [(0.0, 0.0, 0.0), (1.5, -3.0, 4.25), (-6.0, 7.0, 11.0)] {
            // 0° travels toward −Z, so the phase runs along −z.
            let expected = 2.0 + 0.5 * (k * -z - 2.0 * k * time).sin();
            let sample = sample_at(&water, &transform, x, z, time).expect("inside the patch");
            assert!(
                (sample.height - expected).abs() < 1e-5,
                "at ({x}, {z}, t={time}): {} vs {expected}",
                sample.height
            );
        }
    }

    #[test]
    fn the_solve_inverts_the_displacement_the_shader_applies() {
        // The load-bearing test for `base_under`. Displace a known base point
        // forward exactly as the vertex stage does, then ask the evaluator
        // about the XZ it landed on: it must recover that base point's height,
        // which it can only do by undoing the horizontal gather.
        let waves = vec![
            Wave {
                direction: 20.0,
                wavelength: 6.0,
                amplitude: 0.45,
                steepness: 0.5,
                speed: 1.4,
            },
            Wave {
                direction: 115.0,
                wavelength: 2.5,
                amplitude: 0.12,
                steepness: 0.35,
                speed: 0.9,
            },
        ];
        let (water, transform) = pond(waves.clone());
        let packed: Vec<PackedWave> = waves.iter().map(PackedWave::new).collect();

        for (bx, bz, time) in [(0.0, 0.0, 0.0), (2.5, -1.25, 3.75), (-4.0, 3.5, 9.5)] {
            let base = Vec2::new(bx, bz);
            let (mut gather, mut rise) = (Vec2::ZERO, 0.0);
            for wave in &packed {
                let phase = wave.k * wave.direction.dot(base) - wave.omega * time;
                gather += wave.direction * (wave.q * wave.amplitude * phase.cos());
                rise += wave.amplitude * phase.sin();
            }
            let landed = base + gather;

            let sample =
                sample_at(&water, &transform, landed.x, landed.y, time).expect("inside the patch");
            assert!(
                (sample.height - (2.0 + rise)).abs() < 1e-4,
                "displaced ({bx}, {bz}) to {landed}: got {}, want {}",
                sample.height,
                2.0 + rise
            );
        }
    }

    #[test]
    fn the_normal_leans_away_from_a_crest() {
        // A travelling wave tilts the surface everywhere except at its own
        // crests and troughs, and the normal must lean *along the direction of
        // travel* — the sign that `cross(dz, dx)` is the right order.
        let (water, transform) = pond(vec![Wave {
            direction: 0.0,
            wavelength: 8.0,
            amplitude: 0.5,
            steepness: 0.4,
            speed: 0.0,
        }]);
        // 0° travels toward −Z, so at t = 0 the crest is at z = −2 (phase π/2)
        // and the surface climbs toward it from z = 0.
        let sample = sample_at(&water, &transform, 0.0, 0.0, 0.0).expect("inside the patch");
        assert!(
            sample.normal.y > 0.9,
            "normal {} points down",
            sample.normal
        );
        assert!(
            sample.normal.z > 0.05,
            "normal {} does not lean out of the rising face",
            sample.normal
        );
        assert!((sample.normal.length() - 1.0).abs() < 1e-5);
    }

    #[test]
    fn a_rotated_patch_is_an_ordinary_case() {
        // The 2×2 solve exists for this: yaw the pond 90° and its footprint
        // still has to be the same square, since a square rotated about its
        // centre is itself. A `position.y + …` implementation passes the height
        // check here and fails the footprint one on a non-square patch.
        let (water, mut transform) = pond(Vec::new());
        transform.scale = Vec3::new(20.0, 1.0, 6.0);
        transform.rotation = Vec3::new(0.0, 90.0, 0.0);

        // Long axis now runs along Z, short along X.
        assert!(sample_at(&water, &transform, 0.0, 9.5, 0.0).is_some());
        assert!(sample_at(&water, &transform, 0.0, 10.5, 0.0).is_none());
        assert!(sample_at(&water, &transform, 3.5, 0.0, 0.0).is_none());
        assert_eq!(
            sample_at(&water, &transform, 0.0, 0.0, 0.0).map(|s| s.height),
            Some(2.0)
        );
    }

    #[test]
    fn an_edge_on_patch_has_no_answer() {
        // A surface stood on its side covers no XZ column at all. Returning a
        // number here would be inventing one.
        let (water, mut transform) = pond(Vec::new());
        transform.rotation = Vec3::new(90.0, 0.0, 0.0);
        assert!(sample_at(&water, &transform, 0.0, 0.0, 0.0).is_none());
    }

    #[test]
    fn the_surface_is_a_pure_function_of_its_inputs() {
        // Same file, same clock, same answer — the promise every `--time`
        // render and every physics step already leans on.
        let (water, transform) = pond(vec![
            Wave::default(),
            Wave {
                direction: 70.0,
                ..Wave::default()
            },
        ]);
        let surface = Surface::new(&water, &transform);
        for _ in 0..8 {
            assert_eq!(
                surface.sample_at(1.5, -2.5, 6.25),
                sample_at(&water, &transform, 1.5, -2.5, 6.25)
            );
        }
        // And it does move: a wave at a nonzero speed is somewhere else later.
        assert_ne!(
            surface.sample_at(1.5, -2.5, 6.25).map(|s| s.height),
            surface.sample_at(1.5, -2.5, 6.75).map(|s| s.height)
        );
    }

    #[test]
    fn the_solve_converges_at_the_validator_s_steepness_limit() {
        // `Σ steepness == 1` is exactly the bound `water_waves_self_intersect`
        // enforces and exactly the bound the fixed point contracts under. This
        // is the worst case a scene can legally reach: it must still land
        // somewhere sane rather than oscillate or diverge.
        let waves = (0..4)
            .map(|i| Wave {
                direction: i as f32 * 47.0,
                wavelength: 3.0 + i as f32,
                amplitude: 0.3,
                steepness: 0.25,
                speed: 1.0,
            })
            .collect();
        let (water, transform) = pond(waves);
        let surface = Surface::new(&water, &transform);
        for step in 0..40 {
            let time = step as f32 * 0.25;
            let sample = surface.sample_at(2.0, -3.0, time).expect("inside");
            assert!(sample.height.is_finite(), "diverged at t={time}");
            // Four 0.3 m waves cannot sum past 1.2 m either side of the plane.
            assert!(
                (sample.height - 2.0).abs() <= 1.2,
                "t={time}: {} is outside the waves' own range",
                sample.height
            );
        }
    }

    #[test]
    fn wave_directions_follow_the_forward_axis_convention() {
        let close = |a: Vec2, b: Vec2| (a - b).length() < 1e-6;
        assert!(close(wave_direction(0.0), Vec2::new(0.0, -1.0)));
        assert!(close(wave_direction(90.0), Vec2::new(-1.0, 0.0)));
        assert!(close(wave_direction(180.0), Vec2::new(0.0, 1.0)));
        // Every direction is a unit vector, so amplitude means metres
        // regardless of heading.
        for degrees in [-135.0, -33.0, 17.0, 260.0, 1000.0] {
            assert!((wave_direction(degrees).length() - 1.0).abs() < 1e-6);
        }
    }
}
