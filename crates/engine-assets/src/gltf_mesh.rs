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
//!
//! **A skinned primitive is the one exception, and it is load-bearing (M30).**
//! glTF says the transform of the node referencing a skinned mesh is *ignored*
//! — joint matrices are already expressed in the skin's space — so a skinned
//! primitive loads **unbaked**: its vertices must stay in skin space for the
//! joint palette to mean anything, and the engine's own `Transform` on the
//! entity is what places the character. Baking the node transform in anyway is
//! the single most likely thing here to be "simplified" back into a bug; the
//! symptom is a character that renders in the right pose at the wrong place,
//! or one that doubles its own root transform.

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
        ..MeshData::default()
    };

    // The default scene, or the first one — glTF files from every mainstream
    // exporter have at least one. A file with none gets its meshes loaded
    // with identity transforms rather than an error: deterministic, and the
    // geometry is all the engine wants from the file anyway.
    let scene = document
        .default_scene()
        .or_else(|| document.scenes().next());
    match scene {
        Some(scene) => {
            for node in scene.nodes() {
                load_node(&node, Mat4::IDENTITY, &buffers, &mut mesh, &display)?;
            }
        }
        None => {
            for gltf_mesh in document.meshes() {
                load_mesh(
                    &gltf_mesh,
                    Mat4::IDENTITY,
                    false,
                    &buffers,
                    &mut mesh,
                    &display,
                )?;
            }
        }
    }

    // Influences are written for **every** primitive or for none: a file that
    // mixes a skinned primitive with a static one would otherwise leave the
    // two arrays shorter than the positions they parallel, and the shader would
    // read one primitive's influences against another's vertices. Every
    // primitive appends its own — all-zero for a static one, which `skin.wgsl`
    // reads as "leave this vertex alone" — and a file where nothing was skinned
    // gives them back, so an unskinned mesh is `is_skinned() == false` and
    // uploads exactly the buffers it always did.
    if mesh.joint_weights.iter().all(|w| w == &[0.0; 4]) {
        mesh.joint_indices.clear();
        mesh.joint_weights.clear();
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
        // A node that references a skin draws its mesh in skin space, so
        // nothing above it is baked in — see the module doc. The subtree below
        // it keeps accumulating normally: a skinned node's *children* are
        // ordinary nodes placed by the hierarchy.
        let skinned = node.skin().is_some();
        let placement = if skinned { Mat4::IDENTITY } else { transform };
        load_mesh(&gltf_mesh, placement, skinned, buffers, mesh, display)?;
    }

    for child in node.children() {
        load_node(&child, transform, buffers, mesh, display)?;
    }

    Ok(())
}

fn load_mesh(
    gltf_mesh: &gltf::Mesh<'_>,
    transform: Mat4,
    skinned: bool,
    buffers: &[gltf::buffer::Data],
    mesh: &mut MeshData,
    display: &str,
) -> Result<()> {
    for primitive in gltf_mesh.primitives() {
        load_primitive(&primitive, transform, skinned, buffers, mesh, display)?;
    }
    Ok(())
}

fn load_primitive(
    primitive: &gltf::Primitive<'_>,
    transform: Mat4,
    skinned: bool,
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

    // Skinning influences (M30). `JOINTS_1` is a fifth-through-eighth
    // influence per vertex, which the palette's four-wide vertex attribute
    // cannot carry: refused rather than dropped, because a dropped influence
    // shows up as a wrist that collapses under rotation and is a very hard
    // thing to trace back to the loader.
    if primitive
        .get(&gltf::Semantic::Joints(1))
        .or_else(|| primitive.get(&gltf::Semantic::Weights(1)))
        .is_some()
    {
        return Err(EngineError::new(
            engine_core::codes::ASSET_UNSUPPORTED,
            format!(
                "glTF file {display} has a primitive with more than four skinning \
                 influences per vertex (JOINTS_1); the engine skins with four. \
                 Limit the influences in the exporter."
            ),
        )
        .file(display));
    }

    let joint_indices: Vec<[u16; 4]> = match reader.read_joints(0) {
        Some(joints) => joints.into_u16().collect(),
        None => Vec::new(),
    };
    let joint_weights: Vec<[f32; 4]> = match reader.read_weights(0) {
        Some(weights) => weights.into_f32().collect(),
        None => Vec::new(),
    };
    if skinned && (joint_indices.len() != positions.len() || joint_weights.len() != positions.len())
    {
        return Err(EngineError::new(
            engine_core::codes::ASSET_LOAD_FAILED,
            format!(
                "glTF file {display} has {} positions but {} joint indices and {} \
                 weights in one skinned primitive",
                positions.len(),
                joint_indices.len(),
                joint_weights.len()
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

    // One influence per vertex from every primitive, so the arrays stay
    // parallel to the positions — see `load_gltf`, which gives them back when
    // nothing in the file turned out to be skinned.
    if skinned {
        mesh.joint_indices.extend(joint_indices);
        mesh.joint_weights.extend(joint_weights);
    } else {
        mesh.joint_indices
            .extend(std::iter::repeat_n([0u16; 4], positions.len()));
        mesh.joint_weights
            .extend(std::iter::repeat_n([0.0f32; 4], positions.len()));
    }

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
