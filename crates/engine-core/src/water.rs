//! Water surfaces (M18): the tessellated grid a [`Water`] component draws on,
//! and the conventions its waves follow.
//!
//! There is deliberately **no wave evaluation here**. The Gerstner sum lives in
//! `engine-render/src/shaders/water.wgsl` and runs in the vertex stage, which is
//! what lets the grid upload once and never move again: a 96×96 surface is 9409
//! vertices, and displacing them on the CPU would mint a new `Arc<MeshData>`
//! every frame — a per-frame re-upload plus one entry per frame accumulating in
//! the renderer's mesh cache (M15). The GPU evaluates the same formula for free.
//!
//! What that costs is a CPU answer to "how high is the water at (x, z)", which
//! buoyancy and `world.water_height` will need. That is deferred with its own
//! agreement test rather than solved here by a second copy of the formula.
//!
//! [`Water`]: crate::components::Water

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use glam::Vec2;

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
