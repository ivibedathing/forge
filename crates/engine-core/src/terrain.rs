//! Terrain surfaces (M22): the height field a [`Terrain`] component stands on,
//! and the displaced grid it becomes.
//!
//! This is deliberately the mirror image of [`water`](crate::water), which keeps
//! **no** Rust copy of its wave formula and evaluates everything in the vertex
//! stage. Three things make the opposite choice right here:
//!
//! * **Terrain does not animate.** The argument that forced water onto the GPU
//!   — a re-displaced grid every frame would mint a fresh `Arc<MeshData>` per
//!   frame and defeat the renderer's geometry cache (M15) — does not apply to a
//!   surface that is a pure function of its own fields. It is generated once,
//!   cached, and uploaded once.
//! * **Physics has to stand on it.** A collider is CPU geometry. Displacing in
//!   the shader would leave the car driving on the undisplaced plane.
//! * **Placement has to query it.** `world.terrain_height` is what keeps a
//!   walking animal's feet on the ground and what snaps authored props onto a
//!   new surface.
//!
//! So there is exactly one height implementation and nothing to keep in
//! agreement with it. Surface *appearance* goes the other way — per pixel, in
//! `engine-render/src/shaders/mesh.wgsl`, mirrored by nothing — which is what
//! licenses detail far finer than this grid. Nothing physical may depend on it.
//!
//! [`Terrain`]: crate::components::Terrain

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use glam::{Vec2, Vec3};

use crate::components::{Terrain, Transform};
use crate::mesh::MeshData;

/// Most layers one terrain may blend. The shader's table is fixed-size, so a
/// scene asking for more is rejected rather than silently losing the extras.
///
/// Four is not arbitrary: height and slope between them describe a base coat, a
/// steep-face material, a low band and a high band, which is the vocabulary this
/// selector actually has. A fifth layer can only overlap one of those.
pub const MAX_TERRAIN_LAYERS: usize = 4;

/// Hash of two lattice coordinates plus a salt, avalanched into the full 32
/// bits.
///
/// Written out here, exactly as the particle turbulence's `hash3` is, so that no
/// dependency upgrade can reshape a hill: a terrain render sits under a
/// `diff-render` baseline, which makes "what does cell (3, −7) hash to" part of
/// the file format rather than an implementation detail.
fn hash2(x: i32, y: i32, salt: u32) -> u32 {
    let mut h = (x as u32).wrapping_mul(0x8DA6_B343)
        ^ (y as u32).wrapping_mul(0xD816_3841)
        ^ salt.wrapping_mul(0x1657_1FA5);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297A_2D39);
    h ^= h >> 15;
    h
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Smooth value noise in `[-1, 1]`: bilinear interpolation between hashed
/// lattice corners with smoothstepped weights, so the field is continuous in its
/// first derivative and a hillside has no creases along the cell boundaries.
///
/// Value rather than gradient noise, for the reason the particle turbulence
/// gives: a third of the arithmetic, and value noise's slight axis alignment
/// disappears under a domain warp and four octaves.
pub fn noise2(p: Vec2, salt: u32) -> f32 {
    let base = p.floor();
    let frac = p - base;
    let (ix, iy) = (base.x as i32, base.y as i32);

    // smoothstep(t) = t²(3 - 2t)
    let w = frac * frac * (Vec2::splat(3.0) - 2.0 * frac);

    let corner = |dx: i32, dy: i32| -> f32 {
        // Hash to [-1, 1].
        (hash2(ix + dx, iy + dy, salt) >> 8) as f32 / 8_388_608.0 - 1.0
    };

    let x0 = lerp(corner(0, 0), corner(1, 0), w.x);
    let x1 = lerp(corner(0, 1), corner(1, 1), w.x);
    lerp(x0, x1, w.y)
}

/// The height field at a world XZ position, in metres above the patch's own Y,
/// before `Transform.scale.y`: the [`relief_at`] noise with every
/// [`TerrainBasin`] cut out of it.
///
/// Sampled in **world** space so that two patches sharing a description meet
/// seamlessly, and so that moving a patch moves it through the landscape rather
/// than carrying its hills along. Basins are authored in the same space for the
/// same reason.
///
/// The no-basin case is a **branch back to M22's expression** rather than a
/// subtraction of zero. The two are numerically identical, but a scene authored
/// before M42 should provably take the code path its baseline was blessed under
/// instead of a new one that happens to agree.
///
/// [`TerrainBasin`]: crate::components::TerrainBasin
pub fn height_at(terrain: &Terrain, x: f32, z: f32) -> f32 {
    let relief = relief_at(terrain, x, z);
    if terrain.basins.is_empty() {
        return relief;
    }
    relief - basin_drop(terrain, x, z)
}

/// How far the basins lower the ground at a world XZ position, in metres before
/// `Transform.scale.y` — the **deepest** basin covering the point, never the sum
/// of them. `TerrainBasin`'s own docs carry the argument.
fn basin_drop(terrain: &Terrain, x: f32, z: f32) -> f32 {
    let mut drop = 0.0f32;
    for basin in &terrain.basins {
        let dx = x - basin.center[0];
        let dz = z - basin.center[1];
        let distance = (dx * dx + dz * dz).sqrt();

        let weight = if distance <= basin.radius {
            1.0
        } else if basin.falloff > 0.0 {
            let t = ((distance - basin.radius) / basin.falloff).clamp(0.0, 1.0);
            // 1 − smoothstep(t), the interpolant `noise2` already uses, so the
            // wall leaves the floor and meets the untouched field with a zero
            // derivative at both ends.
            1.0 - t * t * (3.0 - 2.0 * t)
        } else {
            // A zero-width wall: inside is floor, outside is untouched. The
            // discontinuity is the authored one.
            0.0
        };

        drop = drop.max(basin.depth * weight);
    }
    drop
}

/// The fBm relief alone (M22), before any basin is cut — the height field as it
/// stood through M41, kept as one unedited expression so that the no-basin path
/// is the one every existing baseline was blessed under.
///
/// The sum is normalised by the total amplitude of its octaves, which is what
/// makes [`Terrain::height`] mean metres regardless of `octaves` and
/// `persistence`: adding an octave must add detail, not altitude. (The same
/// argument that made water's `Q` divide by steepness rather than by wave
/// count.)
///
/// A patch at `height: 0` returns 0 here and is still basined — flat ground with
/// a pond in it is a legitimate thing to ask for.
fn relief_at(terrain: &Terrain, x: f32, z: f32) -> f32 {
    if terrain.height == 0.0 {
        return 0.0;
    }

    let scale = terrain.feature_scale.max(1e-4);
    let mut p = Vec2::new(x, z) / scale;

    // Domain warp: drag the sample point sideways by a lookup of the same field
    // before summing. Two salts so the two offsets are decorrelated; the 0.37
    // shifts keep them off the lattice they are sampling.
    if terrain.warp > 0.0 {
        let warp = Vec2::new(
            noise2(p + Vec2::new(0.37, 1.73), terrain.seed ^ 0x9E37_79B9),
            noise2(p + Vec2::new(-1.21, 0.59), terrain.seed ^ 0x85EB_CA6B),
        );
        p += warp * terrain.warp;
    }

    let mut sum = 0.0;
    let mut amplitude = 1.0;
    let mut total = 0.0;
    let mut frequency = 1.0;
    for octave in 0..terrain.octaves.clamp(1, 8) {
        sum += amplitude * noise2(p * frequency, terrain.seed.wrapping_add(octave));
        total += amplitude;
        amplitude *= terrain.persistence;
        frequency *= 2.0;
    }

    terrain.height * sum / total.max(1e-6)
}

/// [`height_at`] placed in the world by the patch's own `Transform`: the world
/// Y a caller can assign to a position directly.
///
/// The one composition of "the field says this much relief" with "the patch
/// sits here and is this tall". Everything that answers *where the ground is*
/// goes through this function — the script API's `world.terrain_height`,
/// [`Scene::terrain_height`](crate::scene::Scene::terrain_height), and
/// `engine terrain-height` — because M22's central claim is that terrain has
/// exactly one implementation and therefore nothing to keep in agreement, and
/// two callers each adding `position.y + scale.y * …` for themselves is how
/// that claim quietly stops being true.
pub fn world_height_at(terrain: &Terrain, transform: &Transform, x: f32, z: f32) -> f32 {
    transform.position.y + transform.scale.y * height_at(terrain, x, z)
}

/// The slope at a world XZ position: `(∂y/∂x, ∂y/∂z)` in metres per metre,
/// from central differences of [`height_at`].
///
/// `spacing` is the sampling step in metres — one grid quad when this is used
/// for the mesh, so the answer describes the relief the geometry actually
/// carries rather than detail the tessellation dropped.
pub fn gradient_at(terrain: &Terrain, x: f32, z: f32, spacing: f32) -> Vec2 {
    let h = spacing.max(1e-3);
    Vec2::new(
        height_at(terrain, x + h, z) - height_at(terrain, x - h, z),
        height_at(terrain, x, z + h) - height_at(terrain, x, z - h),
    ) / (2.0 * h)
}

/// The world-space surface normal at a world XZ position.
///
/// The smooth field's true normal rather than the tessellation's, which is why
/// the mesh uses this instead of averaging triangle normals: a grid coarse
/// enough to be cheap still shades as though it were not.
pub fn normal_at(terrain: &Terrain, x: f32, z: f32, spacing: f32) -> Vec3 {
    let gradient = gradient_at(terrain, x, z, spacing);
    Vec3::new(-gradient.x, 1.0, -gradient.y).normalize()
}

/// What a generated surface is keyed on: every field that changes the geometry,
/// plus where in the world it sits.
///
/// The world XZ placement belongs in the key because the field is sampled in
/// world space — two patches with identical fields at different positions are
/// different pieces of ground. `f32` bit patterns rather than the floats
/// themselves, so the key can be hashed at all; two NaNs never collide, and a
/// NaN in a terrain field is a validation error long before it reaches here.
#[derive(PartialEq, Eq, Hash)]
struct GridKey {
    segments: u32,
    seed: u32,
    height: u32,
    feature_scale: u32,
    octaves: u32,
    persistence: u32,
    warp: u32,
    origin_x: u32,
    origin_z: u32,
    size_x: u32,
    size_z: u32,
    /// `(center.x, center.z, radius, depth, falloff)` per basin, as bit
    /// patterns and in the authored order. Two patches differing only in their
    /// basins are different ground, and sharing an `Arc` between them would
    /// hand the second one the first one's hole.
    basins: Vec<[u32; 5]>,
}

thread_local! {
    /// One surface per distinct patch per thread.
    ///
    /// The `MeshSource` contract's "same asset, same `Arc`" rule applies for the
    /// reason M15 documents: the renderer keys its uploaded vertex buffers on
    /// this pointer, so a stable `Arc` is the difference between uploading
    /// 74 000 triangles once and uploading them every frame.
    static SURFACE_CACHE: RefCell<HashMap<GridKey, Arc<MeshData>>> = RefCell::new(HashMap::new());
}

/// The displaced surface for a patch, in the entity's local space.
///
/// `origin` is the entity's world XZ position and `size` its world XZ extent
/// (`Transform.scale`), which together decide where in the height field this
/// patch is cut from. Positions come back in the same unit-grid local space
/// `builtin:plane` uses — `[-0.5, 0.5]` in X and Z, so `Transform` sizes it like
/// any other mesh — with **local Y in metres**, unscaled, so `Transform.scale.y`
/// multiplies the relief exactly as it would for a loaded mesh.
pub fn surface_grid(terrain: &Terrain, origin: Vec2, size: Vec2) -> Arc<MeshData> {
    let key = GridKey {
        segments: terrain.segments,
        seed: terrain.seed,
        height: terrain.height.to_bits(),
        feature_scale: terrain.feature_scale.to_bits(),
        octaves: terrain.octaves,
        persistence: terrain.persistence.to_bits(),
        warp: terrain.warp.to_bits(),
        origin_x: origin.x.to_bits(),
        origin_z: origin.y.to_bits(),
        size_x: size.x.to_bits(),
        size_z: size.y.to_bits(),
        basins: terrain
            .basins
            .iter()
            .map(|b| {
                [
                    b.center[0].to_bits(),
                    b.center[1].to_bits(),
                    b.radius.to_bits(),
                    b.depth.to_bits(),
                    b.falloff.to_bits(),
                ]
            })
            .collect(),
    };

    SURFACE_CACHE.with(|cache| {
        Arc::clone(
            cache
                .borrow_mut()
                .entry(key)
                .or_insert_with(|| Arc::new(build_surface(terrain, origin, size))),
        )
    })
}

fn build_surface(terrain: &Terrain, origin: Vec2, size: Vec2) -> MeshData {
    let segments = terrain.segments.clamp(1, 512);
    let n = segments as usize;
    let step = 1.0 / segments as f32;
    let vertices = (n + 1) * (n + 1);

    // Metres per quad, for the normal's sampling step: the normal then describes
    // the relief this grid can actually represent.
    let spacing = size.abs().max_element() * step;

    let mut mesh = MeshData {
        positions: Vec::with_capacity(vertices),
        normals: Vec::with_capacity(vertices),
        uvs: Vec::with_capacity(vertices),
        indices: Vec::with_capacity(n * n * 6),
        ..MeshData::default()
    };

    for i in 0..=n {
        for j in 0..=n {
            let local_x = -0.5 + i as f32 * step;
            let local_z = -0.5 + j as f32 * step;
            let world_x = origin.x + local_x * size.x;
            let world_z = origin.y + local_z * size.y;

            mesh.positions
                .push([local_x, height_at(terrain, world_x, world_z), local_z]);

            // **Local** normals, not world ones. The renderer transforms every
            // normal by the model matrix's inverse-transpose, which for a patch
            // scaled 180× across and 1× up is `diag(1/180, 1, 1/180)` — so a
            // world normal handed over here comes out crushed to straight up,
            // and the whole landscape lights as though it were flat. (It also
            // silently disables slope-selected layers, since every pixel then
            // reports 0°.)
            //
            // Undoing that in advance means scaling the gradient by the
            // patch's own size: `(-∂y/∂x · sx, 1, -∂y/∂z · sz)` maps back to the
            // true world normal for any scale, `Transform.scale.y` included.
            let gradient = gradient_at(terrain, world_x, world_z, spacing);
            mesh.normals.push(
                Vec3::new(-gradient.x * size.x, 1.0, -gradient.y * size.y)
                    .normalize()
                    .to_array(),
            );
            // u along Z, v along X — `builtin:plane`'s convention, shared with
            // the water grid.
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
            // Counter-clockwise seen from above, so the surface survives the
            // engine's back-face culling — same winding as `quad()` and the
            // water grid.
            mesh.indices.extend_from_slice(&[a, b, c, a, c, d]);
        }
    }

    mesh
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::TerrainBasin;
    use crate::mesh::BuiltinMesh;

    fn rolling() -> Terrain {
        Terrain {
            segments: 32,
            seed: 11,
            height: 3.0,
            feature_scale: 20.0,
            octaves: 4,
            persistence: 0.5,
            ..Terrain::default()
        }
    }

    #[test]
    fn a_flat_patch_is_the_builtin_plane() {
        // `height: 0` must give geometry identical to the plane it replaces,
        // which is what lets a scene adopt the component without moving a pixel
        // until it asks for relief.
        let flat = Terrain {
            segments: 1,
            height: 0.0,
            ..Terrain::default()
        };
        let grid = surface_grid(&flat, Vec2::ZERO, Vec2::ONE);
        let plane = BuiltinMesh::Plane.data();

        assert_eq!(grid.triangle_count(), plane.triangle_count());
        for position in &grid.positions {
            assert_eq!(position[1], 0.0, "flat terrain displaced a vertex");
        }
        for normal in &grid.normals {
            assert_eq!(normal, &[0.0, 1.0, 0.0], "flat terrain tilted a normal");
        }
    }

    #[test]
    fn height_stays_within_the_authored_amplitude() {
        // The normalisation is what makes `height` mean metres, so this is the
        // property that keeps octaves from quietly raising the mountains.
        let terrain = rolling();
        let (mut lo, mut hi) = (f32::MAX, f32::MIN);
        for i in 0..200 {
            for j in 0..200 {
                let h = height_at(&terrain, i as f32 * 0.7 - 70.0, j as f32 * 0.7 - 70.0);
                lo = lo.min(h);
                hi = hi.max(h);
            }
        }
        assert!(lo >= -terrain.height && hi <= terrain.height, "{lo}..{hi}");
        // ...and that it is actually used: a field that never leaves ±0.1 m
        // would pass the bound above and still be flat ground.
        assert!(
            hi - lo > terrain.height,
            "terrain barely varies: {lo}..{hi}"
        );
    }

    #[test]
    fn octaves_add_detail_not_altitude() {
        let sample = |octaves| {
            let terrain = Terrain {
                octaves,
                ..rolling()
            };
            let mut hi: f32 = 0.0;
            for i in 0..300 {
                hi = hi.max(height_at(&terrain, i as f32 * 0.9 - 135.0, 4.0).abs());
            }
            hi
        };
        // Not a fixed value — the point is that eight octaves do not tower over
        // two, which is what an unnormalised sum would do.
        assert!(
            sample(8) < sample(2) * 1.35,
            "{} vs {}",
            sample(8),
            sample(2)
        );
    }

    #[test]
    fn the_field_is_continuous() {
        // A hillside with a crease in it is the failure a non-smoothstepped
        // interpolation gives, and it is very visible under a low sun. Walk a
        // line and require consecutive samples to stay close.
        let terrain = rolling();
        let mut previous = height_at(&terrain, -30.0, 2.5);
        for i in 1..4000 {
            let x = -30.0 + i as f32 * 0.02;
            let h = height_at(&terrain, x, 2.5);
            assert!(
                (h - previous).abs() < 0.05,
                "jump of {} at x={x}",
                (h - previous).abs()
            );
            previous = h;
        }
    }

    #[test]
    fn normals_follow_the_slope() {
        let terrain = rolling();
        for i in 0..50 {
            let x = i as f32 * 1.3 - 30.0;
            let n = normal_at(&terrain, x, 7.0, 0.5);
            assert!(n.y > 0.0, "normal points into the ground at x={x}: {n}");
            assert!((n.length() - 1.0).abs() < 1e-4, "unnormalised: {n}");

            // Uphill in +X must tilt the normal toward -X, and vice versa.
            let slope = height_at(&terrain, x + 0.5, 7.0) - height_at(&terrain, x - 0.5, 7.0);
            if slope.abs() > 0.05 {
                assert!(
                    n.x * slope < 0.0,
                    "normal leans the wrong way at x={x}: slope {slope}, n {n}"
                );
            }
        }
    }

    #[test]
    fn patches_are_sampled_in_world_space() {
        // Two patches of the same landscape side by side must agree along the
        // seam, which is the whole reason the field is world-space.
        let terrain = rolling();
        let left = surface_grid(&terrain, Vec2::new(-5.0, 0.0), Vec2::splat(10.0));
        let right = surface_grid(&terrain, Vec2::new(5.0, 0.0), Vec2::splat(10.0));

        // Left patch's +X edge is local x = +0.5; right patch's -X edge is -0.5.
        // Both sit at world x = 0, so their heights must match exactly.
        let edge = |mesh: &MeshData, local_x: f32| -> Vec<f32> {
            mesh.positions
                .iter()
                .filter(|p| (p[0] - local_x).abs() < 1e-6)
                .map(|p| p[1])
                .collect()
        };
        let a = edge(&left, 0.5);
        let b = edge(&right, -0.5);
        assert_eq!(a.len(), 33);
        assert_eq!(a, b, "the seam between two patches does not line up");
    }

    #[test]
    fn mesh_normals_survive_the_model_transform() {
        // The bug this pins cost a debugging session and was invisible in the
        // geometry: a patch's mesh normals are consumed *through* the model
        // matrix's inverse-transpose, so on a 180 m patch a world-space normal
        // arrives at the shader crushed to (0, 1, 0). The landscape then lights
        // as though it were flat and every slope-selected layer silently
        // reports 0° — a shading bug with no wrong pixel to point at.
        let terrain = rolling();
        let size = glam::Vec2::new(180.0, 140.0);
        let grid = surface_grid(&terrain, Vec2::ZERO, size);
        let model = glam::Mat4::from_scale(Vec3::new(size.x, 1.0, size.y));
        let normal_matrix = glam::Mat3::from_mat4(model.inverse().transpose());

        let mut checked = 0;
        for (position, normal) in grid.positions.iter().zip(&grid.normals) {
            let world_x = position[0] * size.x;
            let world_z = position[2] * size.y;
            let expected = normal_at(&terrain, world_x, world_z, size.max_element() / 32.0);
            let actual = (normal_matrix * Vec3::from_array(*normal)).normalize();
            assert!(
                actual.distance(expected) < 1e-3,
                "at ({world_x}, {world_z}): got {actual}, want {expected}"
            );
            checked += 1;
        }
        assert_eq!(checked, 33 * 33);
    }

    #[test]
    fn one_surface_per_patch_is_shared() {
        // The renderer's geometry cache keys on this pointer; a fresh `Arc` per
        // frame would re-upload the whole surface every frame.
        let terrain = rolling();
        assert!(Arc::ptr_eq(
            &surface_grid(&terrain, Vec2::ZERO, Vec2::splat(50.0)),
            &surface_grid(&terrain, Vec2::ZERO, Vec2::splat(50.0))
        ));
        // ...and moving the patch is a different piece of ground.
        assert!(!Arc::ptr_eq(
            &surface_grid(&terrain, Vec2::ZERO, Vec2::splat(50.0)),
            &surface_grid(&terrain, Vec2::new(1.0, 0.0), Vec2::splat(50.0))
        ));
    }

    #[test]
    fn every_triangle_faces_up() {
        let terrain = rolling();
        let grid = surface_grid(&terrain, Vec2::ZERO, Vec2::splat(40.0));
        for triangle in grid.indices.chunks_exact(3) {
            let p = |i: u32| {
                let v = grid.positions[i as usize];
                // Undo the aspect: local XZ spans 1 unit over 40 m, so a hill
                // is enormously steep in local space. Winding is what is being
                // checked, so compare in world proportions.
                Vec3::new(v[0] * 40.0, v[1], v[2] * 40.0)
            };
            let (a, b, c) = (p(triangle[0]), p(triangle[1]), p(triangle[2]));
            let normal = (b - a).cross(c - a).normalize();
            assert!(normal.y > 0.0, "triangle {triangle:?} winds downward");
        }
    }

    #[test]
    fn a_different_seed_is_a_different_landscape() {
        let a = rolling();
        let b = Terrain {
            seed: 12,
            ..a.clone()
        };
        let differing = (0..200)
            .filter(|i| {
                let x = *i as f32 * 0.6;
                (height_at(&a, x, 3.0) - height_at(&b, x, 3.0)).abs() > 0.01
            })
            .count();
        assert!(differing > 180, "seeds barely differ: {differing}/200");
    }

    // ── Basins (M42) ─────────────────────────────────────────────────────

    fn basin(center: [f32; 2], radius: f32, depth: f32, falloff: f32) -> TerrainBasin {
        TerrainBasin {
            center,
            radius,
            depth,
            falloff,
        }
    }

    #[test]
    fn no_basins_is_the_untouched_field() {
        // The M22 path, bit for bit. This is the property every pre-M42
        // baseline in the repo rests on, so it is worth an assertion rather
        // than an argument.
        let plain = rolling();
        let with_empty_list = Terrain {
            basins: Vec::new(),
            ..plain.clone()
        };
        for i in 0..500 {
            let (x, z) = (i as f32 * 0.31 - 40.0, i as f32 * 0.17 - 20.0);
            assert_eq!(
                height_at(&plain, x, z).to_bits(),
                height_at(&with_empty_list, x, z).to_bits()
            );
        }
    }

    #[test]
    fn a_basin_lowers_its_floor_by_exactly_its_depth() {
        let plain = rolling();
        let dug = Terrain {
            basins: vec![basin([6.0, -3.0], 4.0, 2.5, 3.0)],
            ..plain.clone()
        };

        // Anywhere on the floor: the full depth, not a fraction of it.
        for (x, z) in [(6.0, -3.0), (9.9, -3.0), (6.0, 0.9), (3.0, -5.0)] {
            let drop = height_at(&plain, x, z) - height_at(&dug, x, z);
            assert!(
                (drop - 2.5).abs() < 1e-5,
                "floor at ({x}, {z}) dropped {drop}, want 2.5"
            );
        }

        // Beyond the wall: untouched, to the bit.
        for (x, z) in [(14.0, -3.0), (6.0, 12.0), (-30.0, 40.0)] {
            assert_eq!(
                height_at(&plain, x, z).to_bits(),
                height_at(&dug, x, z).to_bits(),
                "the field moved {} m outside the basin at ({x}, {z})",
                height_at(&plain, x, z) - height_at(&dug, x, z)
            );
        }
    }

    #[test]
    fn the_wall_is_monotone_and_meets_the_ground_smoothly() {
        // A basin is a shape an author places by eye and then puts water in, so
        // the wall has to climb without a step and arrive at the surrounding
        // field without a crease — the same thing `the_field_is_continuous`
        // asks of the noise.
        let flat = Terrain {
            height: 0.0,
            basins: vec![basin([0.0, 0.0], 2.0, 1.6, 5.0)],
            ..Terrain::default()
        };

        let mut previous = height_at(&flat, 0.0, 0.0);
        let mut gradients = Vec::new();
        for i in 1..=1400 {
            let x = i as f32 * 0.01;
            let h = height_at(&flat, x, 0.0);
            assert!(h >= previous - 1e-6, "the wall dips at x={x}");
            assert!(h <= 1e-6, "the wall rises above the field at x={x}: {h}");
            gradients.push((h - previous) / 0.01);
            previous = h;
        }

        // The wall spans (2, 7]; the rim is the last sample inside it. A
        // smoothstep leaves with a zero derivative, a linear ramp does not.
        let rim = gradients[(7.0 / 0.01) as usize - 2];
        assert!(rim.abs() < 0.02, "the rim has a crease: dy/dx {rim}");
        assert!(
            gradients.iter().cloned().fold(0.0, f32::max) > 0.3,
            "the wall never actually climbs"
        );
    }

    #[test]
    fn a_zero_falloff_is_a_vertical_wall() {
        let flat = Terrain {
            height: 0.0,
            basins: vec![basin([0.0, 0.0], 3.0, 2.0, 0.0)],
            ..Terrain::default()
        };
        assert_eq!(height_at(&flat, 2.999, 0.0), -2.0);
        assert_eq!(height_at(&flat, 3.001, 0.0), 0.0);
    }

    #[test]
    fn overlapping_basins_take_the_deepest_not_the_sum() {
        // The rule that lets a lake be authored as three circles. Under a sum
        // the overlap digs to 2.4 m and the "lake" is a ring of pits.
        let flat = Terrain {
            height: 0.0,
            basins: vec![
                basin([-2.0, 0.0], 3.0, 1.2, 2.0),
                basin([2.0, 0.0], 3.0, 1.2, 2.0),
            ],
            ..Terrain::default()
        };
        // (0, 0) is inside both floors.
        assert_eq!(height_at(&flat, 0.0, 0.0), -1.2);

        // And "deepest", not "first" or "last": a shallow basin laid over a
        // deep one must not fill it in.
        let nested = Terrain {
            height: 0.0,
            basins: vec![
                basin([0.0, 0.0], 5.0, 3.0, 1.0),
                basin([0.0, 0.0], 5.0, 0.5, 1.0),
            ],
            ..Terrain::default()
        };
        assert_eq!(height_at(&nested, 0.0, 0.0), -3.0);
    }

    #[test]
    fn a_basin_survives_a_flat_patch() {
        // `height: 0` early-returns before the noise is ever summed, which is
        // the one place a basin could have been dropped on the floor.
        let flat = Terrain {
            height: 0.0,
            basins: vec![basin([0.0, 0.0], 1.0, 0.75, 2.0)],
            ..Terrain::default()
        };
        assert_eq!(height_at(&flat, 0.0, 0.0), -0.75);
    }

    #[test]
    fn the_collider_surface_follows_the_basin() {
        // The generated grid *is* the trimesh collider, so this is the
        // assertion that a body dropped into a pond lands in it rather than on
        // the plain the noise would have given.
        let terrain = Terrain {
            segments: 64,
            basins: vec![basin([0.0, 0.0], 6.0, 2.0, 6.0)],
            ..rolling()
        };
        let size = Vec2::splat(60.0);
        let grid = surface_grid(&terrain, Vec2::ZERO, size);

        let mut floor_vertices = 0;
        for position in &grid.positions {
            let (x, z) = (position[0] * size.x, position[2] * size.y);
            assert!(
                (position[1] - height_at(&terrain, x, z)).abs() < 1e-6,
                "the grid disagrees with the height field at ({x}, {z})"
            );
            if x * x + z * z < 36.0 {
                floor_vertices += 1;
            }
        }
        assert!(
            floor_vertices > 10,
            "the floor is not resolved: {floor_vertices}"
        );
    }

    #[test]
    fn basins_are_in_the_surface_cache_key() {
        // Two patches differing only in their basins are different ground;
        // sharing an `Arc` would hand the second one the first one's hole.
        let plain = rolling();
        let dug = Terrain {
            basins: vec![basin([0.0, 0.0], 4.0, 1.0, 4.0)],
            ..plain.clone()
        };
        let deeper = Terrain {
            basins: vec![basin([0.0, 0.0], 4.0, 2.0, 4.0)],
            ..plain.clone()
        };
        let grid = |t: &Terrain| surface_grid(t, Vec2::ZERO, Vec2::splat(50.0));

        assert!(!Arc::ptr_eq(&grid(&plain), &grid(&dug)));
        assert!(!Arc::ptr_eq(&grid(&dug), &grid(&deeper)));
        assert!(Arc::ptr_eq(&grid(&dug), &grid(&dug)));
    }
}
