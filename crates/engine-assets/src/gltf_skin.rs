//! glTF skin and animation extraction (M30 S0).
//!
//! The counterpart to `gltf_mesh.rs`: that module turns a file into geometry,
//! this one turns the same file into a [`Rig`] — the joint hierarchy and the
//! clips. Both stop at plain CPU data, so `engine-core` never learns what glTF
//! is and the sampling in `engine_core::skeleton` stays testable with no asset
//! directory anywhere near it.
//!
//! One skin per file, the first: `Mesh.asset` already means "this whole file
//! as one mesh", and a second skin would need sub-asset addressing nothing has
//! asked for.

use std::collections::HashMap;
use std::path::Path;

use engine_core::error::{EngineError, Result};
use engine_core::skeleton::{
    Channel, ChannelProperty, ChannelValues, Interpolation, Joint, Rig, SkeletalClip, SkinData, Trs,
};
use glam::{Mat4, Quat, Vec3};

/// Read a glTF file's skin and animation clips.
///
/// A file with neither comes back as an empty [`Rig`] rather than an error —
/// every mesh in the repo is one of those, and "does this file have a rig" is
/// a question the caller asks, not a failure.
pub fn load_rig(path: &Path) -> Result<Rig> {
    let display = path.display().to_string();
    let (document, buffers, _images) = gltf::import(path).map_err(|e| {
        EngineError::new(
            engine_core::codes::ASSET_LOAD_FAILED,
            format!("could not load glTF file {display}: {e}"),
        )
        .file(&display)
    })?;

    let skin = document
        .skins()
        .next()
        .map(|skin| load_skin(&skin, &document, &buffers, &display))
        .transpose()?;

    let clips = document
        .animations()
        .enumerate()
        .map(|(index, animation)| load_clip(&animation, index, &buffers))
        .collect();

    Ok(Rig { skin, clips })
}

fn load_skin(
    skin: &gltf::Skin<'_>,
    document: &gltf::Document,
    buffers: &[gltf::buffer::Data],
    display: &str,
) -> Result<SkinData> {
    let joint_nodes: Vec<gltf::Node<'_>> = skin.joints().collect();
    if joint_nodes.is_empty() {
        return Err(EngineError::new(
            engine_core::codes::ASSET_LOAD_FAILED,
            format!("glTF file {display} has a skin with no joints"),
        )
        .file(display));
    }

    let reader = skin.reader(|buffer| buffers.get(buffer.index()).map(|data| &data.0[..]));
    let inverse_binds: Vec<Mat4> = match reader.read_inverse_bind_matrices() {
        Some(matrices) => matrices.map(|m| Mat4::from_cols_array_2d(&m)).collect(),
        // glTF's documented default when the accessor is absent.
        None => vec![Mat4::IDENTITY; joint_nodes.len()],
    };
    if inverse_binds.len() < joint_nodes.len() {
        return Err(EngineError::new(
            engine_core::codes::ASSET_LOAD_FAILED,
            format!(
                "glTF file {display} has {} joints but {} inverse bind matrices",
                joint_nodes.len(),
                inverse_binds.len()
            ),
        )
        .file(display));
    }

    // Node index → its parent node index, walked once over the whole document
    // (glTF stores children, never parents).
    let mut parent_of: HashMap<usize, usize> = HashMap::new();
    for node in document.nodes() {
        for child in node.children() {
            parent_of.insert(child.index(), node.index());
        }
    }

    // Which joint, if any, each node is.
    let joint_of: HashMap<usize, usize> = joint_nodes
        .iter()
        .enumerate()
        .map(|(i, node)| (node.index(), i))
        .collect();

    let node_local: HashMap<usize, Mat4> = document
        .nodes()
        .map(|node| {
            (
                node.index(),
                Mat4::from_cols_array_2d(&node.transform().matrix()),
            )
        })
        .collect();

    let joints = joint_nodes
        .iter()
        .enumerate()
        .map(|(index, node)| {
            // The nearest ancestor that is also a joint, and the product of
            // everything between here and the scene root when there is none.
            let mut parent = None;
            let mut ancestor = Mat4::IDENTITY;
            let mut cursor = node.index();
            while let Some(&up) = parent_of.get(&cursor) {
                if let Some(&joint) = joint_of.get(&up) {
                    parent = Some(joint);
                    break;
                }
                ancestor = node_local.get(&up).copied().unwrap_or(Mat4::IDENTITY) * ancestor;
                cursor = up;
            }

            // `decomposed` hands back translation, rotation, scale — in that
            // order, which is not the order the type's name suggests.
            let (translation, rotation, scale) = node.transform().decomposed();
            Joint {
                node: node.index(),
                name: node
                    .name()
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("joint{index}")),
                parent,
                rest: Trs {
                    translation: Vec3::from_array(translation),
                    rotation: Quat::from_array(rotation),
                    scale: Vec3::from_array(scale),
                },
                inverse_bind: inverse_binds[index],
                ancestor: if parent.is_some() {
                    Mat4::IDENTITY
                } else {
                    ancestor
                },
            }
        })
        .collect();

    Ok(SkinData {
        name: skin.name().map(str::to_string),
        joints,
    })
}

fn load_clip(
    animation: &gltf::Animation<'_>,
    index: usize,
    buffers: &[gltf::buffer::Data],
) -> SkeletalClip {
    let channels = animation
        .channels()
        .filter_map(|channel| load_channel(&channel, buffers))
        .collect();

    SkeletalClip {
        // A clip is addressed by `path#Name`, so an unnamed one has to be
        // given a name to be reachable at all.
        name: animation
            .name()
            .map(str::to_string)
            .unwrap_or_else(|| format!("clip{index}")),
        channels,
    }
}

fn load_channel(
    channel: &gltf::animation::Channel<'_>,
    buffers: &[gltf::buffer::Data],
) -> Option<Channel> {
    use gltf::animation::util::ReadOutputs;
    use gltf::animation::{Interpolation as GltfInterpolation, Property};

    let reader = channel.reader(|buffer| buffers.get(buffer.index()).map(|data| &data.0[..]));
    let times: Vec<f32> = reader.read_inputs()?.collect();
    let outputs = reader.read_outputs()?;

    let (property, values) = match outputs {
        ReadOutputs::Translations(iter) => (
            ChannelProperty::Translation,
            ChannelValues::Vec3(iter.map(Vec3::from_array).collect()),
        ),
        ReadOutputs::Scales(iter) => (
            ChannelProperty::Scale,
            ChannelValues::Vec3(iter.map(Vec3::from_array).collect()),
        ),
        ReadOutputs::Rotations(rotations) => (
            ChannelProperty::Rotation,
            ChannelValues::Quat(rotations.into_f32().map(Quat::from_array).collect()),
        ),
        ReadOutputs::MorphTargetWeights(weights) => (
            ChannelProperty::Weights,
            ChannelValues::Scalar(weights.into_f32().collect()),
        ),
    };

    // `Property` and the outputs always agree in a well-formed file; trusting
    // the outputs is what makes the match above exhaustive without a second
    // arm for the disagreement.
    debug_assert!(matches!(
        (channel.target().property(), property),
        (Property::Translation, ChannelProperty::Translation)
            | (Property::Rotation, ChannelProperty::Rotation)
            | (Property::Scale, ChannelProperty::Scale)
            | (Property::MorphTargetWeights, ChannelProperty::Weights)
    ));

    Some(Channel {
        node: channel.target().node().index(),
        node_name: channel.target().node().name().map(str::to_string),
        property,
        interpolation: match channel.sampler().interpolation() {
            GltfInterpolation::Step => Interpolation::Step,
            GltfInterpolation::Linear => Interpolation::Linear,
            GltfInterpolation::CubicSpline => Interpolation::CubicSpline,
        },
        times,
        values,
    })
}
