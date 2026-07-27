//! glTF mesh loading.
//!
//! A `Mesh.asset` path names a whole glTF file, and loading it means "give me
//! that file's geometry as one mesh": every triangle primitive in the file's
//! default scene, with node transforms baked into the vertices. That is the
//! agent-legible semantic — what you see in a glTF viewer is what the entity
//! renders — and it postpones any notion of sub-asset addressing until
//! something needs it.
//!
//! glTF's conventions match the engine's (right-handed, +Y up, counter-
//! clockwise front faces), so vertices pass through untransformed except for
//! the node hierarchy.

use std::path::Path;

use engine_core::error::{EngineError, Result};
use engine_core::mesh::MeshData;
use glam::{Mat3, Mat4, Vec3};

/// Load every triangle primitive of a `.gltf`/`.glb` file into one mesh.
pub fn load_gltf(path: &Path) -> Result<MeshData> {
    let display = path.display().to_string();

    let (document, buffers, _images) = gltf::import(path).map_err(|e| {
        EngineError::new(
            engine_core::codes::ASSET_LOAD_FAILED,
            format!("could not load glTF file {display}: {e}"),
        )
        .file(&display)
    })?;

    let mut mesh = MeshData {
        positions: Vec::new(),
        normals: Vec::new(),
        uvs: Vec::new(),
        indices: Vec::new(),
    };

    // The default scene, or the first one — glTF files from every mainstream
    // exporter have at least one. A file with none gets its meshes loaded
    // with identity transforms rather than an error: deterministic, and the
    // geometry is all the engine wants from the file anyway.
    let scene = document.default_scene().or_else(|| document.scenes().next());
    match scene {
        Some(scene) => {
            for node in scene.nodes() {
                load_node(&node, Mat4::IDENTITY, &buffers, &mut mesh, &display)?;
            }
        }
        None => {
            for gltf_mesh in document.meshes() {
                load_mesh(&gltf_mesh, Mat4::IDENTITY, &buffers, &mut mesh, &display)?;
            }
        }
    }

    if mesh.indices.is_empty() {
        return Err(EngineError::new(
            engine_core::codes::ASSET_UNSUPPORTED,
            format!("glTF file {display} contains no triangle geometry"),
        )
        .file(&display));
    }

    Ok(mesh)
}

fn load_node(
    node: &gltf::Node<'_>,
    parent: Mat4,
    buffers: &[gltf::buffer::Data],
    mesh: &mut MeshData,
    display: &str,
) -> Result<()> {
    let transform = parent * Mat4::from_cols_array_2d(&node.transform().matrix());

    if let Some(gltf_mesh) = node.mesh() {
        load_mesh(&gltf_mesh, transform, buffers, mesh, display)?;
    }

    for child in node.children() {
        load_node(&child, transform, buffers, mesh, display)?;
    }

    Ok(())
}

fn load_mesh(
    gltf_mesh: &gltf::Mesh<'_>,
    transform: Mat4,
    buffers: &[gltf::buffer::Data],
    mesh: &mut MeshData,
    display: &str,
) -> Result<()> {
    for primitive in gltf_mesh.primitives() {
        load_primitive(&primitive, transform, buffers, mesh, display)?;
    }
    Ok(())
}

fn load_primitive(
    primitive: &gltf::Primitive<'_>,
    transform: Mat4,
    buffers: &[gltf::buffer::Data],
    mesh: &mut MeshData,
    display: &str,
) -> Result<()> {
    if primitive.mode() != gltf::mesh::Mode::Triangles {
        return Err(EngineError::new(
            engine_core::codes::ASSET_UNSUPPORTED,
            format!(
                "glTF file {display} has a {:?} primitive; only Triangles are supported",
                primitive.mode()
            ),
        )
        .file(display));
    }

    let reader = primitive.reader(|buffer| buffers.get(buffer.index()).map(|data| &data.0[..]));

    let Some(position_reader) = reader.read_positions() else {
        return Err(EngineError::new(
            engine_core::codes::ASSET_UNSUPPORTED,
            format!("glTF file {display} has a primitive with no POSITION attribute"),
        )
        .file(display));
    };
    let positions: Vec<Vec3> = position_reader.map(Vec3::from_array).collect();

    let base = mesh.positions.len() as u32;

    let indices: Vec<u32> = match reader.read_indices() {
        Some(indices) => indices.into_u32().collect(),
        // Unindexed geometry: consecutive vertices form triangles.
        None => (0..positions.len() as u32).collect(),
    };

    // Normals transform by the inverse-transpose so non-uniform node scales
    // do not shear the lighting. Missing normals are reconstructed from the
    // faces rather than rejected — flat-exported files are common and an
    // agent can do nothing about them from the scene file.
    let normal_matrix = Mat3::from_mat4(transform).inverse().transpose();
    let normals: Vec<Vec3> = match reader.read_normals() {
        Some(normal_reader) => normal_reader.map(Vec3::from_array).collect(),
        None => smooth_normals(&positions, &indices),
    };

    if normals.len() != positions.len() {
        return Err(EngineError::new(
            engine_core::codes::ASSET_LOAD_FAILED,
            format!(
                "glTF file {display} has {} positions but {} normals in one primitive",
                positions.len(),
                normals.len()
            ),
        )
        .file(display));
    }

    let uvs: Vec<[f32; 2]> = match reader.read_tex_coords(0) {
        Some(uv_reader) => uv_reader.into_f32().collect(),
        None => vec![[0.0, 0.0]; positions.len()],
    };
    if uvs.len() != positions.len() {
        return Err(EngineError::new(
            engine_core::codes::ASSET_LOAD_FAILED,
            format!(
                "glTF file {display} has {} positions but {} texture coordinates in one primitive",
                positions.len(),
                uvs.len()
            ),
        )
        .file(display));
    }

    for position in &positions {
        mesh.positions
            .push(transform.transform_point3(*position).to_array());
    }
    for normal in &normals {
        mesh.normals
            .push((normal_matrix * *normal).normalize_or_zero().to_array());
    }
    mesh.uvs.extend(uvs);
    mesh.indices.extend(indices.iter().map(|i| base + i));

    Ok(())
}

/// Reconstruct per-vertex normals by averaging the geometric normal of every
/// face a vertex participates in.
fn smooth_normals(positions: &[Vec3], indices: &[u32]) -> Vec<Vec3> {
    let mut normals = vec![Vec3::ZERO; positions.len()];

    for triangle in indices.chunks_exact(3) {
        let [a, b, c] = [
            positions[triangle[0] as usize],
            positions[triangle[1] as usize],
            positions[triangle[2] as usize],
        ];
        // Unnormalized: the cross product's magnitude weights the average by
        // face area, which is the standard choice.
        let face = (b - a).cross(c - a);
        for &index in triangle {
            normals[index as usize] += face;
        }
    }

    for normal in &mut normals {
        *normal = normal.normalize_or(Vec3::Y);
    }
    normals
}
