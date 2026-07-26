//! Geometry, and the built-in primitives available before the asset pipeline
//! exists.
//!
//! Mesh data lives here rather than in `engine-render` so it stays testable
//! without a GPU and so `engine validate` can resolve asset references without
//! linking wgpu.
//!
//! M3 replaces `from_asset`'s error arm with real glTF loading. Until then a
//! scene that references a file gets a structured "not yet" rather than a
//! silently empty render — an agent should never have to wonder whether a
//! missing object means a bad transform or an unimplemented loader.

use glam::Vec3;

use crate::error::{EngineError, Result};

/// CPU-side geometry, ready to upload.
///
/// Positions and normals are parallel arrays of the same length; `indices`
/// refers into them.
#[derive(Debug, Clone, PartialEq)]
pub struct MeshData {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
}

impl MeshData {
    pub fn vertex_count(&self) -> usize {
        self.positions.len()
    }

    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }
}

/// A primitive the engine can produce without loading anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinMesh {
    Cube,
    Plane,
    Triangle,
}

impl BuiltinMesh {
    /// Prefix marking an asset reference as built-in rather than a file path.
    pub const PREFIX: &'static str = "builtin:";

    /// Every built-in's full asset string, for errors and suggestions.
    pub const ASSETS: &'static [&'static str] =
        &["builtin:cube", "builtin:plane", "builtin:triangle"];

    /// Resolve a `Mesh { asset }` reference.
    ///
    /// Every failure is `asset_not_found` (design doc §5) — one code for an
    /// agent to match on, with the message and `did_you_mean` carrying the
    /// specifics. `engine validate` calls this, so a bad reference fails
    /// validation rather than rendering an incomplete frame; render-time
    /// callers keep it as a backstop.
    pub fn from_asset(asset: &str) -> Result<Self> {
        let Some(name) = asset.strip_prefix(Self::PREFIX) else {
            return Err(EngineError::new(
                "asset_not_found",
                format!(
                    "cannot load {asset:?}: mesh files are not loaded until M3 \
                     (glTF asset pipeline). Use one of {} for now.",
                    Self::ASSETS.join(", ")
                ),
            )
            .field("asset")
            .suggest_from(asset, Self::ASSETS.iter().copied()));
        };

        match name {
            "cube" => Ok(Self::Cube),
            "plane" => Ok(Self::Plane),
            "triangle" => Ok(Self::Triangle),
            _ => Err(EngineError::new(
                "asset_not_found",
                format!("no built-in mesh named {name:?}"),
            )
            .field("asset")
            .suggest_from(asset, Self::ASSETS.iter().copied())),
        }
    }

    pub fn data(self) -> MeshData {
        match self {
            Self::Cube => cube(),
            Self::Plane => plane(),
            Self::Triangle => triangle(),
        }
    }
}

/// Build a quad facing `normal`, wound counter-clockwise seen from outside.
///
/// `u` and `v` must satisfy `cross(u, v) == normal`; that constraint is what
/// makes the winding come out right for every face without a hand-checked
/// vertex table.
fn quad(normal: Vec3, u: Vec3, v: Vec3, mesh: &mut MeshData) {
    debug_assert!(
        (u.cross(v) - normal).length() < 1e-5,
        "cross(u, v) must equal the normal, or the face winds backwards"
    );

    let center = normal * 0.5;
    let (u, v) = (u * 0.5, v * 0.5);
    let base = mesh.positions.len() as u32;

    for corner in [
        center - u - v,
        center + u - v,
        center + u + v,
        center - u + v,
    ] {
        mesh.positions.push(corner.to_array());
        mesh.normals.push(normal.to_array());
    }

    mesh.indices
        .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
}

/// Unit cube centered on the origin, with flat per-face normals.
fn cube() -> MeshData {
    let mut mesh = MeshData {
        positions: Vec::with_capacity(24),
        normals: Vec::with_capacity(24),
        indices: Vec::with_capacity(36),
    };

    let (x, y, z) = (Vec3::X, Vec3::Y, Vec3::Z);
    quad(x, y, z, &mut mesh);
    quad(-x, z, y, &mut mesh);
    quad(y, z, x, &mut mesh);
    quad(-y, x, z, &mut mesh);
    quad(z, x, y, &mut mesh);
    quad(-z, y, x, &mut mesh);

    mesh
}

/// Unit quad in the XZ plane, facing up.
fn plane() -> MeshData {
    let mut mesh = MeshData {
        positions: Vec::with_capacity(4),
        normals: Vec::with_capacity(4),
        indices: Vec::with_capacity(6),
    };
    // Offset back to the origin: `quad` pushes the face out along its normal.
    quad(Vec3::Y, Vec3::Z, Vec3::X, &mut mesh);
    for position in &mut mesh.positions {
        position[1] -= 0.5;
    }
    mesh
}

/// The M0 triangle, kept as a primitive so the oldest render path stays
/// reachable from a scene file.
fn triangle() -> MeshData {
    MeshData {
        positions: vec![[-0.8, -0.6, 0.0], [0.8, -0.6, 0.0], [0.0, 0.8, 0.0]],
        normals: vec![[0.0, 0.0, 1.0]; 3],
        indices: vec![0, 1, 2],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cube_has_flat_shaded_faces() {
        let cube = BuiltinMesh::Cube.data();
        assert_eq!(cube.vertex_count(), 24, "4 verts per face, not shared");
        assert_eq!(cube.triangle_count(), 12);
        assert_eq!(cube.normals.len(), cube.positions.len());
    }

    #[test]
    fn cube_faces_all_wind_outward() {
        // If any face were wound backwards it would be culled and the cube
        // would render with a hole. Check the geometric normal of each triangle
        // agrees with the direction from the cube's center.
        let cube = BuiltinMesh::Cube.data();

        for triangle in cube.indices.chunks(3) {
            let [a, b, c] = [
                Vec3::from_array(cube.positions[triangle[0] as usize]),
                Vec3::from_array(cube.positions[triangle[1] as usize]),
                Vec3::from_array(cube.positions[triangle[2] as usize]),
            ];

            let geometric = (b - a).cross(c - a).normalize();
            let outward = ((a + b + c) / 3.0).normalize();

            assert!(
                geometric.dot(outward) > 0.5,
                "a face winds inward: normal {geometric:?} vs outward {outward:?}"
            );
        }
    }

    #[test]
    fn cube_spans_the_unit_extent() {
        let cube = BuiltinMesh::Cube.data();
        for axis in 0..3 {
            let min = cube
                .positions
                .iter()
                .map(|p| p[axis])
                .fold(f32::MAX, f32::min);
            let max = cube
                .positions
                .iter()
                .map(|p| p[axis])
                .fold(f32::MIN, f32::max);
            assert_eq!((min, max), (-0.5, 0.5));
        }
    }

    #[test]
    fn plane_sits_on_the_origin() {
        let plane = BuiltinMesh::Plane.data();
        assert!(plane.positions.iter().all(|p| p[1] == 0.0));
        assert_eq!(plane.normals[0], [0.0, 1.0, 0.0]);
    }

    #[test]
    fn resolves_builtin_assets() {
        assert_eq!(
            BuiltinMesh::from_asset("builtin:cube").unwrap(),
            BuiltinMesh::Cube
        );
    }

    #[test]
    fn explains_that_file_assets_arrive_at_m3() {
        let err = BuiltinMesh::from_asset("meshes/cube.glb").unwrap_err();
        assert_eq!(err.error, "asset_not_found");
        assert!(
            err.message.contains("M3"),
            "the error should say when this will work: {}",
            err.message
        );
    }

    #[test]
    fn suggests_a_near_miss_builtin() {
        let err = BuiltinMesh::from_asset("builtin:cuve").unwrap_err();
        assert_eq!(err.error, "asset_not_found");
        assert_eq!(
            err.context().unwrap().did_you_mean.as_deref(),
            Some("builtin:cube")
        );
    }
}
