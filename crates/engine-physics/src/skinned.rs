//! Skinned collider proxies (M33) — `designs/skinned-collider-design.md`.
//!
//! The pieces of a proxy that are pure arithmetic or pure rapier plumbing: how
//! a part's shape is built, where the pose puts it, and the contact filter that
//! stops a character's own hitboxes from fighting its body. The wiring — which
//! proxies exist, and when they are re-posed — is in `lib.rs` beside the rest of
//! the world's state, because the order things happen in a step is the one thing
//! this crate must keep in one place.
//!
//! **The pose drives the proxies and nothing reads them back.** That is the
//! milestone's whole invariant: it is what keeps M30's claim that a pose is a
//! pure function of (files, time) true, and it is why every proxy is a
//! kinematic body rather than a dynamic one.

use engine_core::components::{ColliderPart, ColliderShapeKind};
use glam::{Mat4, Quat, Vec3};
use hecs::Entity;
use rapier3d::math::Pose;
use rapier3d::prelude::*;
use std::collections::HashMap;

/// One built proxy: which joint it rides, what it is called in reports, and
/// the rapier handles it was built as.
pub(crate) struct Proxy {
    /// The entity whose rig this proxy rides — the name every report gives.
    pub(crate) entity: Entity,
    /// Index into the skin's `joints`, resolved once at build. Joint order is
    /// the skin's own (M30 §5), so this index is stable for the run.
    pub(crate) joint: usize,
    /// What reports call this part: `ColliderPart::part_name`.
    pub(crate) part: String,
    /// The part's placement inside the joint's frame, scale already folded in.
    pub(crate) local: Mat4,
    pub(crate) body: RigidBodyHandle,
}

/// The shape a part describes, at a uniform `scale`.
///
/// `None` for the mesh shapes, which `collider_part_shape_unsupported` refuses
/// at validation — reaching it here would be an engine bug, and the caller
/// says so rather than building a silently absent hitbox.
pub(crate) fn part_shape(part: &ColliderPart, scale: f32) -> Option<SharedShape> {
    match part.shape {
        ColliderShapeKind::Sphere => Some(SharedShape::ball(part.radius? * scale)),
        ColliderShapeKind::Cuboid => {
            let half = part.half_extents? * scale;
            Some(SharedShape::cuboid(half.x, half.y, half.z))
        }
        // Along local +Y, which is rapier's axis and `builtin:cylinder`'s; a
        // bone that runs some other way is turned by the part's `rotation`.
        ColliderShapeKind::Capsule => Some(SharedShape::capsule_y(
            part.half_height? * scale,
            part.radius? * scale,
        )),
        ColliderShapeKind::Trimesh | ColliderShapeKind::ConvexHull => None,
    }
}

/// A part's placement inside its joint's frame, with the entity's uniform
/// scale folded in.
///
/// The scale rides here as well as in the shape because `offset` is metres in
/// the joint's frame: a character at scale 2 has a head twice as big *and*
/// twice as far up.
pub(crate) fn part_local(part: &ColliderPart, scale: f32) -> Mat4 {
    Mat4::from_scale_rotation_translation(
        Vec3::ONE,
        Quat::from_euler(
            glam::EulerRot::XYZ,
            part.rotation.x.to_radians(),
            part.rotation.y.to_radians(),
            part.rotation.z.to_radians(),
        ),
        part.offset * scale,
    )
}

/// Where a proxy is this step: the entity's model matrix, the joint's posed
/// global, and the part's own placement.
///
/// The scale in the composed matrix is **dropped**, deliberately: the shape was
/// built at the entity's scale already, and a clip that scales a joint moves
/// the proxy without resizing it. A hitbox that quietly resized with the pose
/// is one nobody can predict from the file — see the design's §1.
pub(crate) fn part_pose(model: Mat4, joint_global: Mat4, local: Mat4) -> Pose {
    let matrix = model * joint_global * local;
    let (_, rotation, translation) = matrix.to_scale_rotation_translation();
    Pose::from_parts(translation, rotation.normalize())
}

/// Rejects contacts between two colliders belonging to one entity.
///
/// A hitbox set overlaps itself permanently — that is what a hitbox set *is* —
/// and it overlaps the character's own `Collider` if it has one, which without
/// this filter is a body that launches itself off its own head. Only proxy
/// colliders carry `ActiveHooks`, so a scene with no `SkinnedCollider` never
/// reaches this function and the solver sees exactly the pairs it always did.
pub(crate) struct SelfFilter<'a> {
    pub(crate) owner: &'a HashMap<ColliderHandle, Entity>,
}

impl SelfFilter<'_> {
    fn same_owner(&self, context: &PairFilterContext) -> bool {
        match (
            self.owner.get(&context.collider1),
            self.owner.get(&context.collider2),
        ) {
            (Some(a), Some(b)) => a == b,
            _ => false,
        }
    }
}

impl PhysicsHooks for SelfFilter<'_> {
    fn filter_contact_pair(&self, context: &PairFilterContext) -> Option<SolverFlags> {
        if self.same_owner(context) {
            return None;
        }
        Some(SolverFlags::COMPUTE_IMPULSES)
    }

    // Sensors take the same rule: a sensor proxy inside its own character
    // would otherwise report an overlap with it every step of the run.
    fn filter_intersection_pair(&self, context: &PairFilterContext) -> bool {
        !self.same_owner(context)
    }
}
