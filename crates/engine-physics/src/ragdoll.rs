//! Ragdolls (M39) — `designs/ragdoll-design.md`.
//!
//! The pieces of a ragdoll that are pure arithmetic or pure rapier plumbing:
//! the joint each pair of parts gets, and the skeleton read back out of where
//! the bodies ended up. The wiring — when a handoff happens, and where in a
//! step the write-back sits — is in `lib.rs` beside the rest of the world's
//! state, for `skinned.rs`'s reason: the order things happen in a step is the
//! one thing this crate keeps in one place.
//!
//! **This is M33's one-way rule reversed, for one entity, once.** Everywhere
//! else in this engine the pose drives the proxies; here the proxies drive the
//! pose. What keeps invariant 2 intact through the reversal is that the pose
//! goes into a **component field** rather than into this struct — see
//! `engine_core::components::Ragdoll`.

use engine_core::components::RagdollJoint;
use engine_core::skeleton::SkinData;
use glam::{Mat4, Quat, Vec3};
use hecs::Entity;
use rapier3d::prelude::*;

/// One entity's ragdoll, as the physics world holds it.
pub(crate) struct Ragdoll {
    pub(crate) entity: Entity,
    /// Indices into `PhysicsWorld::proxies`, in the component's part order.
    pub(crate) parts: Vec<usize>,
    /// For each entry of `parts`, the entry it hangs from — an index *into
    /// `parts`*, not into the proxy list. `None` for the root.
    pub(crate) parents: Vec<Option<usize>>,
    /// The one part with no proxied ancestor: what the entity's `Transform`
    /// follows once physics owns the character.
    pub(crate) root: usize,
    /// Whether the handoff has happened. Set from `Ragdoll.active` at build and
    /// on the first step that finds the component true; never cleared, because
    /// the handoff is one-way (design §3).
    pub(crate) active: bool,
    /// The joints wiring the parts together, empty until the handoff.
    pub(crate) joints: Vec<ImpulseJointHandle>,
    /// The local pose the clip was showing when the ragdoll fired.
    ///
    /// Genuinely disposable state, `Locomotion::previous`'s category: it is
    /// re-derivable from `Ragdoll.pose`, which is the field that actually
    /// carries the skeleton. It is kept because the joints physics does *not*
    /// drive — a hand, a finger, a jaw — keep these locals for the rest of the
    /// run, and re-deriving them from the component every step would be a name
    /// lookup per joint per step for an answer that cannot change.
    pub(crate) frozen: Vec<engine_core::skeleton::Trs>,
}

/// Where one part sits at the moment of the handoff, and what it rides.
///
/// A struct rather than a tuple because the joint-building below reads four
/// fields off two of these in the same expression, and `(Vec3, Quat, usize,
/// Mat4)` twice over is the shape a swapped pair hides in.
#[derive(Clone)]
pub(crate) struct Placement {
    pub(crate) translation: Vec3,
    pub(crate) rotation: Quat,
    /// Index into the skin's joints.
    pub(crate) joint: usize,
    /// The part's placement inside that joint's frame.
    pub(crate) local: Mat4,
}

impl Placement {
    /// A world point in this body's own frame.
    pub(crate) fn frame(&self, point: Vec3) -> Vec3 {
        self.rotation.inverse() * (point - self.translation)
    }
}

/// The rotation that carries the parent part's body frame onto the child's,
/// **at the rig's rest pose**.
///
/// This is what a joint's limits are measured from, and choosing rest rather
/// than the pose at handoff is deliberate: a knee authored as `[-120, 0]` must
/// mean the same bend whether the character died standing or mid-stride. A
/// frame pair that coincided at handoff would make every ragdoll's limits
/// depend on the frame it fired on, which is precisely the unpredictability
/// this repo refuses elsewhere.
///
/// The entity's own model matrix cancels out of a *relative* rotation, so it is
/// not a parameter: `(M·Gp·Lp)⁻¹·(M·Gc·Lc)` has no `M` left in it.
pub(crate) fn rest_relative(
    rest_globals: &[Mat4],
    parent: (usize, Mat4),
    child: (usize, Mat4),
) -> Quat {
    let frame = |(joint, local): (usize, Mat4)| {
        let global = rest_globals.get(joint).copied().unwrap_or(Mat4::IDENTITY);
        (global * local)
            .to_scale_rotation_translation()
            .1
            .normalize()
    };
    frame(parent).inverse() * frame(child)
}

/// The joint holding one part to its parent.
///
/// Anchored at the **child joint's origin** — the anatomical joint, not either
/// shape's centre — expressed in each body's own frame, so an elbow hinges
/// where an elbow is rather than where the middle of the forearm capsule
/// happens to sit.
///
/// The default is a spherical joint whose swing and twist are both limited:
/// `limit` degrees about the two swing axes, half that about the twist axis.
/// A single equal limit on all three reads as a bag of shapes, and the real
/// asymmetry — a neck twists less than it nods — is what makes a ragdoll read
/// as a body. An override replaces it with a hinge, which is what a knee and
/// an elbow are.
pub(crate) fn joint_between(
    anchor_parent: Vec3,
    anchor_child: Vec3,
    rest: Quat,
    override_data: Option<&RagdollJoint>,
    default_limit: f32,
) -> GenericJoint {
    match override_data.and_then(|o| o.hinge.map(|axis| (o, axis))) {
        Some((data, axis)) => {
            // The hinge turns about `axis` in the child part's frame; the
            // parent's frame is the same axis carried back through the rest
            // rotation, so the two frames coincide at rest and the range is
            // measured from there.
            let axis = axis.normalize_or(Vec3::X);
            let to_axis = Quat::from_rotation_arc(Vec3::X, axis);
            let range = data.range.unwrap_or([-default_limit, default_limit]);
            // `GenericJointBuilder` rather than `RevoluteJointBuilder`, which
            // takes one axis "expressed in the local-space of both
            // rigid-bodies" — true only when the two bodies share an
            // orientation, and two bones never do. A frame pair says it once
            // per body and carries the rest rotation between them.
            GenericJointBuilder::new(JointAxesMask::LOCKED_REVOLUTE_AXES)
                .local_frame1(Pose::from_parts(anchor_parent, rest * to_axis))
                .local_frame2(Pose::from_parts(anchor_child, to_axis))
                .limits(
                    JointAxis::AngX,
                    [range[0].to_radians(), range[1].to_radians()],
                )
                .build()
        }
        None => {
            let limit = override_data
                .and_then(|o| o.limit)
                .unwrap_or(default_limit)
                .to_radians();
            SphericalJointBuilder::new()
                .local_frame1(Pose::from_parts(anchor_parent, rest))
                .local_frame2(Pose::from_parts(anchor_child, Quat::IDENTITY))
                .limits(JointAxis::AngX, [-limit * 0.5, limit * 0.5])
                .limits(JointAxis::AngY, [-limit, limit])
                .limits(JointAxis::AngZ, [-limit, limit])
                .build()
                .into()
        }
    }
}

/// The length a `fit: "bone"` part takes from the posed rig (M39 §7).
///
/// The distance from the part's joint to that joint's **first child**, less the
/// part's radius, so a capsule spans the bone rather than overhanging it. A
/// joint with no child — a hand, a head — has no bone to measure and keeps
/// whatever was authored, which is the honest answer rather than a guess.
///
/// Measured in the joint's own frame, so the entity's scale is applied by the
/// caller exactly as it is for an authored length.
pub(crate) fn fitted_half_length(
    skin: &SkinData,
    globals: &[Mat4],
    joint: usize,
    radius: f32,
) -> Option<f32> {
    let child = skin.joints.iter().position(|j| j.parent == Some(joint))?;
    let here = globals.get(joint)?.to_scale_rotation_translation().2;
    let there = globals.get(child)?.to_scale_rotation_translation().2;
    let half = here.distance(there) * 0.5 - radius;
    // A bone shorter than its own radius is a joint pair the author has
    // already got wrong; a non-positive half-height is a shape rapier refuses,
    // so the authored value stands and `list-colliders` shows it.
    (half > 1e-4).then_some(half)
}

/// How far a fitted length must move before the shape is rebuilt.
///
/// Half a millimetre. A rig whose clips animate rotation only — which is every
/// clip in this repo — never crosses it after the first step, so the cost of
/// the feature on the scenes that use it is one comparison per part per step
/// rather than a rapier shape allocation.
pub(crate) const FIT_EPSILON: f32 = 5e-4;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rest_rotation_does_not_depend_on_where_the_character_stands() {
        // The relative rotation between two parts is what a joint's limits are
        // measured from, and the entity's own placement must cancel out of it
        // — otherwise a ragdoll's knee would bend differently depending on
        // which way the character was facing when it died.
        let globals = vec![
            Mat4::from_rotation_y(0.3),
            Mat4::from_rotation_y(0.3) * Mat4::from_rotation_x(0.8),
        ];
        let a = rest_relative(&globals, (0, Mat4::IDENTITY), (1, Mat4::IDENTITY));

        let moved: Vec<Mat4> = globals
            .iter()
            .map(|g| Mat4::from_translation(Vec3::new(17.0, 0.0, -4.0)) * *g)
            .collect();
        let b = rest_relative(&moved, (0, Mat4::IDENTITY), (1, Mat4::IDENTITY));
        assert!(a.abs_diff_eq(b, 1e-6), "{a} vs {b}");
    }

    #[test]
    fn a_fitted_capsule_spans_the_bone_less_its_radius() {
        let skin = SkinData {
            name: None,
            joints: vec![
                engine_core::skeleton::Joint {
                    node: 0,
                    name: "Thigh".into(),
                    parent: None,
                    rest: Default::default(),
                    inverse_bind: Mat4::IDENTITY,
                    ancestor: Mat4::IDENTITY,
                },
                engine_core::skeleton::Joint {
                    node: 1,
                    name: "Shin".into(),
                    parent: Some(0),
                    rest: Default::default(),
                    inverse_bind: Mat4::IDENTITY,
                    ancestor: Mat4::IDENTITY,
                },
            ],
        };
        let globals = vec![
            Mat4::IDENTITY,
            Mat4::from_translation(Vec3::new(0.0, -0.8, 0.0)),
        ];
        // 0.8 m of bone, 0.1 m radius: half of 0.8 is 0.4, less the radius.
        let half = fitted_half_length(&skin, &globals, 0, 0.1).unwrap();
        assert!((half - 0.3).abs() < 1e-5, "got {half}");

        // A tip joint has no bone to measure and says so rather than guessing.
        assert!(fitted_half_length(&skin, &globals, 1, 0.1).is_none());
    }
}
