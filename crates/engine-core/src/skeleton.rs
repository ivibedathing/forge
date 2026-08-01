//! Skeletal animation, the CPU half (M30 S0): rigs as data, poses as a pure
//! function of (files, time).
//!
//! `engine-assets` extracts a [`Rig`] out of a glTF file; nothing in this
//! module knows what glTF is, exactly the seam [`crate::mesh::MeshData`]
//! already sits on. What lives here is the part that has to be testable
//! without a GPU and without an asset directory: the sampling.
//!
//! **The split this milestone turns on is CPU skeleton, GPU skin.** A joint
//! palette is a few dozen matrices, so computing it here is free — and
//! *because* it is here, `engine list-joints --time 0.7` can report where
//! every joint actually is, a script can put a torch in a hand, and the whole
//! sampling path is unconditionally testable the way `daylight.rs` is. Skinning
//! the vertices is the other half and belongs on the GPU: posing them here
//! would mint a new `Arc<MeshData>` every frame and defeat M15's upload cache,
//! which is the same argument that put Gerstner waves in the vertex stage.
//!
//! **Rotation is a quaternion here, slerped, shortest-path — the opposite of
//! M9's rule that property clips interpolate Euler degrees component-wise.**
//! Both are right, and the distinction is who wrote the numbers. A property
//! clip's keys were typed by an agent into JSON, where `[0, 360, 0]` is a
//! sentence in a format the agent already knows and must actually spin. A
//! skeletal clip's keys came out of a DCC tool through a specified interchange
//! format: nobody typed them, nobody will read them, and the only correct
//! reading is the glTF specification's.

use std::collections::HashMap;
use std::sync::Arc;

use glam::{Mat4, Quat, Vec3};

use crate::error::Result;

/// The joint-palette ceiling, mirroring `MAX_POINT_LIGHTS` and
/// `MAX_ROAD_KERBS`: the uniform is fixed-size, so a rig with more joints is
/// `too_many_joints` at **validate** time rather than a character that renders
/// correctly up to joint 128 and explodes past it.
///
/// 128 joints packed as three `vec4` rows is 6 KiB against the 16 KiB
/// `max_uniform_buffer_binding_size` that `downlevel_defaults` guarantees.
pub const MAX_JOINTS: usize = 128;

/// A node's local transform, in the decomposed form glTF stores and animates.
///
/// Kept decomposed rather than as a `Mat4` because animation channels target
/// translation, rotation and scale *separately*: a clip that only rotates a
/// joint must leave that joint's authored translation alone, which a composed
/// matrix cannot express.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Trs {
    pub translation: Vec3,
    pub rotation: Quat,
    pub scale: Vec3,
}

impl Default for Trs {
    fn default() -> Self {
        Self {
            translation: Vec3::ZERO,
            rotation: Quat::IDENTITY,
            scale: Vec3::ONE,
        }
    }
}

impl Trs {
    pub fn matrix(&self) -> Mat4 {
        Mat4::from_scale_rotation_translation(self.scale, self.rotation, self.translation)
    }
}

/// One joint of a skin.
#[derive(Debug, Clone, PartialEq)]
pub struct Joint {
    /// The glTF node this joint is. Animation channels target nodes, so this
    /// is what a channel is matched against — and it is why a channel aimed
    /// at a node no skin uses is *ignorable* rather than an error.
    pub node: usize,

    /// The node's name, or a synthesized `joint{index}` when the exporter
    /// wrote none. An unnamed joint is unreachable from `list-joints` and from
    /// a script, so a stable stand-in beats an empty string.
    pub name: String,

    /// The index — in this same `joints` array — of the joint above this one,
    /// or `None` for a root.
    pub parent: Option<usize>,

    /// The node's authored local transform, which sampling overwrites channel
    /// by channel.
    pub rest: Trs,

    /// The skin's `inverseBindMatrices` entry: skin space → this joint's bind
    /// space.
    pub inverse_bind: Mat4,

    /// The world transform of everything above a **root** joint that is not
    /// itself a joint — identity for every joint that has a joint parent, and
    /// identity in the common case where the skeleton hangs off the scene
    /// root.
    ///
    /// It is a constant matrix rather than a sampled one because nothing
    /// outside the skin is sampled: a channel targeting a non-joint node is
    /// reported by `list-animations` and ignored, so its node's transform is
    /// whatever the file authored.
    pub ancestor: Mat4,
}

/// A skin: an ordered joint array and the hierarchy over it.
///
/// **The order is the glTF skin's own `joints` order and must not be sorted.**
/// Unlike point lights — whose uniform index must not depend on archetype
/// iteration, hence name-sorting — a joint's index is written into the vertex
/// data as `JOINTS_0`. Reordering here would silently attach every vertex to
/// the wrong bone.
#[derive(Debug, Clone, PartialEq)]
pub struct SkinData {
    pub name: Option<String>,
    pub joints: Vec<Joint>,
}

impl SkinData {
    /// The joint index for a glTF node index, or `None` when that node is not
    /// part of this skin.
    pub fn joint_of_node(&self, node: usize) -> Option<usize> {
        self.joints.iter().position(|joint| joint.node == node)
    }

    /// The joint index for a joint name, for `world.joint_position` and for
    /// `list-joints --entity`.
    pub fn joint_named(&self, name: &str) -> Option<usize> {
        self.joints.iter().position(|joint| joint.name == name)
    }
}

/// Which of a node's transform components a channel drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelProperty {
    Translation,
    Rotation,
    Scale,
    /// Morph-target weights: parsed and reported so an asset does not appear
    /// to carry less than it does, never sampled (out of scope, §1).
    Weights,
}

impl ChannelProperty {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Translation => "translation",
            Self::Rotation => "rotation",
            Self::Scale => "scale",
            Self::Weights => "weights",
        }
    }
}

/// glTF's three sampler interpolations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interpolation {
    Step,
    Linear,
    CubicSpline,
}

impl Interpolation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Step => "step",
            Self::Linear => "linear",
            Self::CubicSpline => "cubicspline",
        }
    }
}

/// A channel's output values, one per key time — except under
/// `CubicSpline`, where glTF stores an in-tangent / value / out-tangent
/// triplet per key and the vectors are three times as long.
#[derive(Debug, Clone, PartialEq)]
pub enum ChannelValues {
    Vec3(Vec<Vec3>),
    Quat(Vec<Quat>),
    /// Morph weights, kept flat: never sampled, only counted.
    Scalar(Vec<f32>),
}

impl ChannelValues {
    fn len(&self) -> usize {
        match self {
            Self::Vec3(v) => v.len(),
            Self::Quat(v) => v.len(),
            Self::Scalar(v) => v.len(),
        }
    }
}

/// One animated property of one node.
#[derive(Debug, Clone, PartialEq)]
pub struct Channel {
    /// The glTF node index this channel targets.
    pub node: usize,
    /// That node's name, for reporting. A channel whose target is outside the
    /// skin is ignored during sampling, and an ignored channel nothing reports
    /// is invisible — so `list-animations` names it.
    pub node_name: Option<String>,
    pub property: ChannelProperty,
    pub interpolation: Interpolation,
    pub times: Vec<f32>,
    pub values: ChannelValues,
}

impl Channel {
    /// Whether the sampler is well enough formed to sample: at least one key,
    /// and the value count glTF requires for the interpolation.
    ///
    /// Malformed channels are dropped rather than erroring, because a rig with
    /// one broken channel still poses; the loader is where a file that cannot
    /// be read at all fails.
    pub fn is_sampleable(&self) -> bool {
        if self.times.is_empty() || matches!(self.property, ChannelProperty::Weights) {
            return false;
        }
        let expected = match self.interpolation {
            Interpolation::CubicSpline => self.times.len() * 3,
            _ => self.times.len(),
        };
        self.values.len() == expected
    }
}

/// A skeletal clip: glTF's animation, addressed by name.
#[derive(Debug, Clone, PartialEq)]
pub struct SkeletalClip {
    /// The animation's name, or a synthesized `clip{index}` when the exporter
    /// wrote none — a clip is addressed by `path#Name`, so an unnamed one has
    /// to be given a name to be reachable at all.
    pub name: String,
    pub channels: Vec<Channel>,
}

/// Everything skeletal one glTF file carries.
///
/// The engine reads **one** skin per file — the file's first — because
/// `Mesh.asset` already means "this whole file as one mesh" and a second skin
/// would need sub-asset addressing that nothing has asked for. A file with no
/// skin still yields its clips, so `list-animations` can report a glTF whose
/// animation moves whole nodes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Rig {
    pub skin: Option<SkinData>,
    pub clips: Vec<SkeletalClip>,
}

impl Rig {
    pub fn clip_named(&self, name: &str) -> Option<&SkeletalClip> {
        self.clips.iter().find(|clip| clip.name == name)
    }

    pub fn clip_names(&self) -> Vec<&str> {
        self.clips.iter().map(|clip| clip.name.as_str()).collect()
    }
}

/// Loads rigs the way [`crate::mesh::MeshSource`] loads geometry, and with the
/// same `Arc`-sharing contract: repeated loads of one asset hand back the same
/// `Arc`, so a palette computed per frame does not re-parse a `.glb`.
pub trait RigSource {
    fn load_rig(&self, asset: &str) -> Result<Arc<Rig>>;
}

/// What an `AnimationPlayer.clip` string names.
///
/// One field, both kinds of animation — `animation-system-design.md` §4
/// specified the fragment form and nothing used it until M30.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipRef<'a> {
    /// A `*.anim.json` property clip (M9).
    Property(&'a str),
    /// A glTF file and the clip inside it.
    Skeletal { asset: &'a str, clip: &'a str },
}

impl<'a> ClipRef<'a> {
    /// Split a `clip` field. The `#` is the whole rule: a reference with one
    /// is skeletal, one without is a property clip.
    ///
    /// A glTF path with no fragment is deliberately **not** resolved to "the
    /// only clip in the file" — see `clip_needs_fragment`. Splitting is
    /// syntax; that check is validation's.
    pub fn parse(clip: &'a str) -> Self {
        match clip.split_once('#') {
            Some((asset, name)) => ClipRef::Skeletal { asset, clip: name },
            None => ClipRef::Property(clip),
        }
    }

    /// The file half, whichever kind this is.
    pub fn asset(&self) -> &'a str {
        match self {
            ClipRef::Property(path) => path,
            ClipRef::Skeletal { asset, .. } => asset,
        }
    }
}

/// Whether a path names a glTF file, by extension — the test that turns
/// "a clip reference with no `#`" into `clip_needs_fragment` rather than a
/// missing `.anim.json`.
pub fn is_gltf_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".gltf") || lower.ends_with(".glb")
}

/// A clip's duration: the largest key time across its channels.
///
/// M9's rule, kept — there is no separate duration field to drift out of sync
/// with the keys.
pub fn duration(clip: &SkeletalClip) -> f32 {
    clip.channels
        .iter()
        .filter_map(|channel| channel.times.last().copied())
        .fold(0.0, f32::max)
}

/// Where `t` falls in a key sequence: the index of the key at or before it,
/// and how far from there to the next as a 0..1 fraction.
///
/// Before the first key and after the last, the endpoint is held — glTF's
/// rule, and the same clamping M9's property clips use.
fn locate(times: &[f32], t: f32) -> (usize, usize, f32) {
    debug_assert!(!times.is_empty());
    if t <= times[0] || times.len() == 1 {
        return (0, 0, 0.0);
    }
    let last = times.len() - 1;
    if t >= times[last] {
        return (last, last, 0.0);
    }
    // Linear scan: a joint channel has a handful of keys, and a binary search
    // here would be a measurement nobody has taken.
    let mut i = 0;
    while i + 1 < times.len() && times[i + 1] <= t {
        i += 1;
    }
    let span = times[i + 1] - times[i];
    let fraction = if span > 0.0 {
        (t - times[i]) / span
    } else {
        0.0
    };
    (i, i + 1, fraction)
}

/// glTF's cubic spline: Hermite over the value/tangent triplets, with the
/// tangents scaled by the key spacing as the specification requires.
fn cubic<T>(v0: T, out0: T, v1: T, in1: T, span: f32, u: f32) -> T
where
    T: std::ops::Mul<f32, Output = T> + std::ops::Add<Output = T>,
{
    let u2 = u * u;
    let u3 = u2 * u;
    v0 * (2.0 * u3 - 3.0 * u2 + 1.0)
        + out0 * (span * (u3 - 2.0 * u2 + u))
        + v1 * (-2.0 * u3 + 3.0 * u2)
        + in1 * (span * (u3 - u2))
}

/// Sample a `Vec3` channel (translation or scale) at time `t`.
pub fn sample_vec3(channel: &Channel, t: f32) -> Option<Vec3> {
    let ChannelValues::Vec3(values) = &channel.values else {
        return None;
    };
    if !channel.is_sampleable() {
        return None;
    }
    let (a, b, u) = locate(&channel.times, t);
    if a == b {
        // Clamped past an end, or a single key. Returning the key itself
        // rather than interpolating onto itself keeps the held value exactly
        // what the file said.
        return Some(match channel.interpolation {
            Interpolation::CubicSpline => values[a * 3 + 1],
            _ => values[a],
        });
    }
    Some(match channel.interpolation {
        Interpolation::Step => values[a],
        Interpolation::Linear => values[a].lerp(values[b], u),
        Interpolation::CubicSpline => {
            let span = channel.times[b] - channel.times[a];
            cubic(
                values[a * 3 + 1],
                values[a * 3 + 2],
                values[b * 3 + 1],
                values[b * 3],
                span,
                u,
            )
        }
    })
}

/// Sample a rotation channel at time `t`.
///
/// Slerp, shortest path: quaternions `q` and `-q` are the same orientation, so
/// a key pair whose dot product is negative would otherwise spin the long way
/// round. That is a bug visible on one frame in ten, which is why it is pinned
/// by a test rather than eyeballed.
pub fn sample_quat(channel: &Channel, t: f32) -> Option<Quat> {
    let ChannelValues::Quat(values) = &channel.values else {
        return None;
    };
    if !channel.is_sampleable() {
        return None;
    }
    let (a, b, u) = locate(&channel.times, t);
    if a == b {
        return Some(match channel.interpolation {
            Interpolation::CubicSpline => values[a * 3 + 1],
            _ => values[a],
        });
    }
    Some(match channel.interpolation {
        Interpolation::Step => values[a],
        Interpolation::Linear => {
            let from = values[a];
            let mut to = values[b];
            if from.dot(to) < 0.0 {
                to = -to;
            }
            from.slerp(to, u).normalize()
        }
        Interpolation::CubicSpline => {
            // The spec interpolates the components and normalizes after,
            // rather than slerping tangents — so that is what happens here.
            let span = channel.times[b] - channel.times[a];
            let q = cubic(
                values[a * 3 + 1],
                values[a * 3 + 2],
                values[b * 3 + 1],
                values[b * 3],
                span,
                u,
            );
            let normalized = q.normalize();
            if normalized.is_finite() {
                normalized
            } else {
                values[a * 3 + 1]
            }
        }
    })
}

/// Every joint's local transform at time `t`: the rest pose with each
/// channel that targets a joint of this skin applied over it.
///
/// A `None` clip is the rest pose — which is what an entity with a skinned
/// mesh and no player renders, and what `list-joints` reports without
/// `--time`.
pub fn local_pose(skin: &SkinData, clip: Option<&SkeletalClip>, t: f32) -> Vec<Trs> {
    let mut pose: Vec<Trs> = skin.joints.iter().map(|joint| joint.rest).collect();

    let Some(clip) = clip else { return pose };

    // Node → joint once, rather than a scan per channel: a rig with 128 joints
    // and 384 channels would otherwise be quadratic for no reason.
    let index: HashMap<usize, usize> = skin
        .joints
        .iter()
        .enumerate()
        .map(|(i, joint)| (joint.node, i))
        .collect();

    for channel in &clip.channels {
        let Some(&joint) = index.get(&channel.node) else {
            continue; // Outside the skin: reported by `list-animations`, ignored here.
        };
        match channel.property {
            ChannelProperty::Translation => {
                if let Some(v) = sample_vec3(channel, t) {
                    pose[joint].translation = v;
                }
            }
            ChannelProperty::Rotation => {
                if let Some(q) = sample_quat(channel, t) {
                    pose[joint].rotation = q;
                }
            }
            ChannelProperty::Scale => {
                if let Some(v) = sample_vec3(channel, t) {
                    pose[joint].scale = v;
                }
            }
            ChannelProperty::Weights => {}
        }
    }

    pose
}

/// Every joint's transform in **skin space** at time `t` — the local pose
/// composed down the hierarchy.
///
/// This is what `engine list-joints --time` reports and what a script asks for
/// a hand's position: multiply by the entity's own `Transform` to reach the
/// world. glTF says the transform of the node referencing a skinned mesh is
/// ignored, so the engine's `Transform` on the entity is what places the
/// character — never a node transform out of the file.
pub fn joint_globals(skin: &SkinData, clip: Option<&SkeletalClip>, t: f32) -> Vec<Mat4> {
    globals_from(skin, &local_pose(skin, clip, t))
}

/// The hierarchy walk on its own, over a local pose someone else produced.
///
/// Separate from [`joint_globals`] because foot planting (M32) edits locals and
/// has to re-derive globals two or three times per frame — and the walk
/// resolves parents rather than assuming an order, which is a property worth
/// having in exactly one place.
pub fn globals_from(skin: &SkinData, pose: &[Trs]) -> Vec<Mat4> {
    let mut globals = vec![Mat4::IDENTITY; skin.joints.len()];

    // Parents before children. The loader emits joints in the skin's own
    // order, which glTF does not require to be topological, so this resolves
    // rather than assumes.
    let mut resolved = vec![false; skin.joints.len()];
    let mut remaining = skin.joints.len();
    while remaining > 0 {
        let before = remaining;
        for i in 0..skin.joints.len() {
            if resolved[i] {
                continue;
            }
            let local = pose[i].matrix();
            globals[i] = match skin.joints[i].parent {
                None => skin.joints[i].ancestor * local,
                Some(parent) if resolved[parent] => globals[parent] * local,
                Some(_) => continue,
            };
            resolved[i] = true;
            remaining -= 1;
        }
        if remaining == before {
            // A parent cycle: the file is malformed. Leaving the rest at
            // identity is a visibly collapsed rig, which beats hanging.
            break;
        }
    }

    globals
}

/// The joint palette the vertex stage multiplies by: skin space → posed skin
/// space, per joint.
///
/// `world = entity_model · Σ wᵢ · palette[jᵢ] · position`, which is why the
/// entity's `Transform` is *not* folded in here.
pub fn palette(skin: &SkinData, clip: Option<&SkeletalClip>, t: f32) -> Vec<Mat4> {
    joint_globals(skin, clip, t)
        .into_iter()
        .zip(&skin.joints)
        .map(|(global, joint)| global * joint.inverse_bind)
        .collect()
}

/// One scene entity whose `Mesh` turns out to carry a skin, together with the
/// clip its `AnimationPlayer` selects.
pub struct SkinnedEntity {
    pub name: String,
    /// The hecs handle, so a caller can reach the entity's other components —
    /// which is what `engine list-joints` needs to pose the rig through M32's
    /// shared seam rather than re-deriving the pose beside it.
    pub entity: hecs::Entity,
    /// The entity's `Mesh.asset` — the file the skin came out of.
    pub asset: String,
    pub rig: Arc<Rig>,
    /// The player's clip, when it has a skeletal one. `None` is a skinned
    /// mesh at rest, which is a legitimate thing to author and to report.
    pub clip: Option<String>,
    pub player: Option<crate::components::AnimationPlayer>,
    /// The entity's own `Transform`, which is what places the character:
    /// glTF says the transform of the node referencing a skinned mesh is
    /// ignored, so nothing out of the file competes with this.
    pub transform: crate::components::Transform,
}

impl SkinnedEntity {
    /// The clip this entity plays, resolved out of its rig.
    pub fn selected_clip(&self) -> Option<&SkeletalClip> {
        self.clip
            .as_deref()
            .and_then(|name| self.rig.clip_named(name))
    }

    /// Scene time `t` mapped through the player's `speed`, `start_offset` and
    /// `looping` — the same arithmetic property clips use, so one clock drives
    /// both kinds of animation.
    pub fn local_time(&self, t: f32) -> f32 {
        match (&self.player, self.selected_clip()) {
            (Some(player), Some(clip)) => crate::animation::local_time(player, duration(clip), t),
            _ => t,
        }
    }
}

/// Every entity in a scene whose mesh carries a skin, name-sorted.
///
/// Name-sorted is a contract, not cosmetics — M24's rule for reports.
/// Entities whose mesh has no skin are simply absent: "does this file have a
/// rig" is a question, not a failure, so `list-joints` on an unrigged scene is
/// an empty list rather than an error.
pub fn skinned_entities(scene: &crate::Scene, rigs: &dyn RigSource) -> Result<Vec<SkinnedEntity>> {
    let mut names: Vec<String> = scene.names().map(str::to_string).collect();
    names.sort();

    let mut found = Vec::new();
    for name in names {
        let Some(entity) = scene.entity(&name) else {
            continue;
        };
        let Ok(mesh) = scene.world.get::<&crate::components::Mesh>(entity) else {
            continue;
        };
        let rig = rigs.load_rig(&mesh.asset)?;
        if rig.skin.is_none() {
            continue;
        }

        let player = scene
            .world
            .get::<&crate::components::AnimationPlayer>(entity)
            .ok()
            .map(|player| (*player).clone());
        let clip = player.as_ref().and_then(|player| {
            match ClipRef::parse(&player.clip) {
                ClipRef::Skeletal { clip, .. } => Some(clip.to_string()),
                // A property clip on a skinned entity is legal and animates
                // components, not joints; the rig stays at rest.
                ClipRef::Property(_) => None,
            }
        });
        let transform = scene
            .world
            .get::<&crate::components::Transform>(entity)
            .map(|t| *t)
            .unwrap_or_default();

        found.push(SkinnedEntity {
            name,
            entity,
            asset: mesh.asset.clone(),
            rig,
            clip,
            player,
            transform,
        });
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arm() -> SkinData {
        // Root at the origin, elbow one unit up, hand one unit above that.
        SkinData {
            name: Some("Arm".into()),
            joints: vec![
                Joint {
                    node: 1,
                    name: "Shoulder".into(),
                    parent: None,
                    rest: Trs::default(),
                    inverse_bind: Mat4::IDENTITY,
                    ancestor: Mat4::IDENTITY,
                },
                Joint {
                    node: 2,
                    name: "Elbow".into(),
                    parent: Some(0),
                    rest: Trs {
                        translation: Vec3::Y,
                        ..Trs::default()
                    },
                    inverse_bind: Mat4::from_translation(-Vec3::Y),
                    ancestor: Mat4::IDENTITY,
                },
                Joint {
                    node: 3,
                    name: "Hand".into(),
                    parent: Some(1),
                    rest: Trs {
                        translation: Vec3::Y,
                        ..Trs::default()
                    },
                    inverse_bind: Mat4::from_translation(-2.0 * Vec3::Y),
                    ancestor: Mat4::IDENTITY,
                },
            ],
        }
    }

    fn rotation_channel(node: usize, keys: Vec<(f32, Quat)>) -> Channel {
        Channel {
            node,
            node_name: None,
            property: ChannelProperty::Rotation,
            interpolation: Interpolation::Linear,
            times: keys.iter().map(|(t, _)| *t).collect(),
            values: ChannelValues::Quat(keys.iter().map(|(_, q)| *q).collect()),
        }
    }

    #[test]
    fn a_rest_pose_stacks_the_hierarchy() {
        let skin = arm();
        let globals = joint_globals(&skin, None, 0.0);
        assert_eq!(globals[0].transform_point3(Vec3::ZERO), Vec3::ZERO);
        assert_eq!(globals[1].transform_point3(Vec3::ZERO), Vec3::Y);
        assert_eq!(globals[2].transform_point3(Vec3::ZERO), 2.0 * Vec3::Y);
    }

    #[test]
    fn a_parent_rotation_carries_its_children() {
        // Bend the shoulder 90° about +X: the hand, two units up the chain,
        // swings to -Z.
        let skin = arm();
        let clip = SkeletalClip {
            name: "Bend".into(),
            channels: vec![rotation_channel(
                1,
                vec![
                    (0.0, Quat::IDENTITY),
                    (1.0, Quat::from_rotation_x(std::f32::consts::FRAC_PI_2)),
                ],
            )],
        };
        let hand = joint_globals(&skin, Some(&clip), 1.0)[2].transform_point3(Vec3::ZERO);
        assert!(
            (hand - Vec3::new(0.0, 0.0, 2.0)).length() < 1e-5,
            "hand ended at {hand}"
        );
    }

    #[test]
    fn the_rest_pose_and_time_zero_agree() {
        let skin = arm();
        let clip = SkeletalClip {
            name: "Bend".into(),
            channels: vec![rotation_channel(
                1,
                vec![
                    (0.0, Quat::IDENTITY),
                    (1.0, Quat::from_rotation_x(std::f32::consts::FRAC_PI_2)),
                ],
            )],
        };
        assert_eq!(
            joint_globals(&skin, None, 0.0),
            joint_globals(&skin, Some(&clip), 0.0)
        );
    }

    #[test]
    fn slerp_takes_the_shortest_path() {
        // 170° apart, with the second key negated — the same orientation,
        // written the long way round. Without the dot-product flip the
        // midpoint lands 180° away from where it belongs.
        let far = Quat::from_rotation_y(170f32.to_radians());
        let channel = rotation_channel(1, vec![(0.0, Quat::IDENTITY), (1.0, -far)]);
        let mid = sample_quat(&channel, 0.5).unwrap();
        let expected = Quat::from_rotation_y(85f32.to_radians());
        assert!(
            mid.dot(expected).abs() > 0.9999,
            "midpoint {mid:?} is not the 85° rotation"
        );
    }

    #[test]
    fn keys_clamp_outside_their_range() {
        let channel = rotation_channel(
            1,
            vec![
                (1.0, Quat::IDENTITY),
                (2.0, Quat::from_rotation_y(std::f32::consts::FRAC_PI_2)),
            ],
        );
        assert_eq!(sample_quat(&channel, -5.0).unwrap(), Quat::IDENTITY);
        assert_eq!(
            sample_quat(&channel, 99.0).unwrap(),
            Quat::from_rotation_y(std::f32::consts::FRAC_PI_2)
        );
    }

    #[test]
    fn step_holds_until_the_next_key() {
        let mut channel = rotation_channel(
            1,
            vec![
                (0.0, Quat::IDENTITY),
                (1.0, Quat::from_rotation_y(std::f32::consts::FRAC_PI_2)),
            ],
        );
        channel.interpolation = Interpolation::Step;
        assert_eq!(sample_quat(&channel, 0.999).unwrap(), Quat::IDENTITY);
    }

    #[test]
    fn a_cubic_channel_passes_through_its_keys() {
        let channel = Channel {
            node: 1,
            node_name: None,
            property: ChannelProperty::Translation,
            interpolation: Interpolation::CubicSpline,
            times: vec![0.0, 1.0],
            values: ChannelValues::Vec3(vec![
                // in-tangent, value, out-tangent per key.
                Vec3::ZERO,
                Vec3::ZERO,
                Vec3::ZERO,
                Vec3::ZERO,
                Vec3::new(0.0, 4.0, 0.0),
                Vec3::ZERO,
            ]),
        };
        assert_eq!(sample_vec3(&channel, 0.0).unwrap(), Vec3::ZERO);
        assert_eq!(
            sample_vec3(&channel, 1.0).unwrap(),
            Vec3::new(0.0, 4.0, 0.0)
        );
        // Flat tangents at both ends: the midpoint is the smoothstep half.
        assert_eq!(
            sample_vec3(&channel, 0.5).unwrap(),
            Vec3::new(0.0, 2.0, 0.0)
        );
    }

    #[test]
    fn a_channel_outside_the_skin_is_ignored_not_fatal() {
        let skin = arm();
        let clip = SkeletalClip {
            name: "Elsewhere".into(),
            channels: vec![rotation_channel(
                // Node 99 is in the file but in no skin.
                99,
                vec![
                    (0.0, Quat::IDENTITY),
                    (1.0, Quat::from_rotation_x(std::f32::consts::FRAC_PI_2)),
                ],
            )],
        };
        assert_eq!(
            joint_globals(&skin, Some(&clip), 1.0),
            joint_globals(&skin, None, 0.0)
        );
    }

    #[test]
    fn the_palette_is_identity_at_the_bind_pose() {
        // Rest pose == bind pose in this rig, so every palette entry is the
        // identity — the property that makes a skinned mesh with no clip
        // render exactly where its vertices sit.
        let skin = arm();
        for matrix in palette(&skin, None, 0.0) {
            assert!(
                (matrix - Mat4::IDENTITY).abs_diff_eq(Mat4::ZERO, 1e-6),
                "{matrix} is not the identity"
            );
        }
    }

    #[test]
    fn a_clip_reference_splits_on_the_hash() {
        assert_eq!(
            ClipRef::parse("meshes/robot.glb#Walk"),
            ClipRef::Skeletal {
                asset: "meshes/robot.glb",
                clip: "Walk"
            }
        );
        assert_eq!(
            ClipRef::parse("animations/spin.anim.json"),
            ClipRef::Property("animations/spin.anim.json")
        );
    }

    #[test]
    fn duration_is_the_largest_key_time() {
        let clip = SkeletalClip {
            name: "Walk".into(),
            channels: vec![
                rotation_channel(1, vec![(0.0, Quat::IDENTITY), (0.5, Quat::IDENTITY)]),
                rotation_channel(2, vec![(0.0, Quat::IDENTITY), (1.25, Quat::IDENTITY)]),
            ],
        };
        assert_eq!(duration(&clip), 1.25);
    }

    #[test]
    fn joints_resolve_even_when_a_child_precedes_its_parent() {
        // glTF does not require the joints array to be topologically sorted.
        let skin = SkinData {
            name: None,
            joints: vec![
                Joint {
                    node: 2,
                    name: "Child".into(),
                    parent: Some(1),
                    rest: Trs {
                        translation: Vec3::Y,
                        ..Trs::default()
                    },
                    inverse_bind: Mat4::IDENTITY,
                    ancestor: Mat4::IDENTITY,
                },
                Joint {
                    node: 1,
                    name: "Parent".into(),
                    parent: None,
                    rest: Trs {
                        translation: Vec3::X,
                        ..Trs::default()
                    },
                    inverse_bind: Mat4::IDENTITY,
                    ancestor: Mat4::IDENTITY,
                },
            ],
        };
        let globals = joint_globals(&skin, None, 0.0);
        assert_eq!(
            globals[0].transform_point3(Vec3::ZERO),
            Vec3::new(1.0, 1.0, 0.0)
        );
    }
}
