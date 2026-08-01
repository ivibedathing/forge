//! Ragdolls (M39) — `designs/ragdoll-design.md`.
//!
//! The parts of a ragdoll that are pure arithmetic over the rig: which proxy
//! hangs off which, and the conversion between a skeleton's local pose and the
//! `Ragdoll.pose` field that carries it in the file. The simulation itself is
//! in `engine-physics`, beside the rest of the world's state, because the order
//! things happen in a step is the one thing that crate keeps in one place.
//!
//! Everything here is GPU-free and unconditionally testable, the way
//! `locomotion.rs` and `daylight.rs` are — and it is shared rather than
//! duplicated because **validation and the simulation must agree about the
//! joint graph**. A validator that computed a different parent from the one
//! rapier wires would pass a scene that comes apart at the first step.

use glam::{Quat, Vec3};

use crate::components::{ColliderPart, JointPose};
use crate::skeleton::{SkinData, Trs};

/// For each part, the index of the part it hangs from — or `None` when it is a
/// root.
///
/// **The graph is derived from the skeleton, never authored.** A part's parent
/// is the part riding the *nearest ancestor joint that also carries a part*, so
/// an eleven-part humanoid wires itself and a part list that skips the spine
/// still connects the head to the pelvis. Nothing in the scene file repeats
/// what the rig already says.
///
/// A part whose joint the rig does not have (`unknown_joint` refuses one at
/// validation) is treated as a root here rather than panicking: this function
/// runs inside the validator that is trying to report the typo.
pub fn parent_parts(skin: &SkinData, parts: &[ColliderPart]) -> Vec<Option<usize>> {
    // Joint index → the part riding it. Built once; the alternative is a
    // linear scan per ancestor per part, which is quadratic in a place that
    // runs on every world build.
    let mut part_of_joint: Vec<Option<usize>> = vec![None; skin.joints.len()];
    for (i, part) in parts.iter().enumerate() {
        if let Some(joint) = skin.joint_named(&part.joint) {
            // First writer wins, so a duplicate-jointed part set has a stable
            // graph. `duplicate_collider_part` only refuses duplicate *names*.
            part_of_joint[joint].get_or_insert(i);
        }
    }

    parts
        .iter()
        .map(|part| {
            let joint = skin.joint_named(&part.joint)?;
            let mut walk = skin.joints[joint].parent;
            while let Some(ancestor) = walk {
                if let Some(owner) = part_of_joint[ancestor] {
                    return Some(owner);
                }
                walk = skin.joints[ancestor].parent;
            }
            None
        })
        .collect()
}

/// The parts with no proxied ancestor.
///
/// Exactly one is a ragdoll. Two is a ragdoll in two pieces, which
/// `ragdoll_disconnected_parts` refuses at validation rather than at the first
/// step — rapier would simulate it perfectly happily, as two.
pub fn roots(parents: &[Option<usize>]) -> Vec<usize> {
    parents
        .iter()
        .enumerate()
        .filter_map(|(i, parent)| parent.is_none().then_some(i))
        .collect()
}

/// A skeleton's local pose as the field the scene file carries.
///
/// One entry per joint, in the skin's own joint order — which is the order
/// everything else in this engine reads a rig in, and not an accident: a
/// name-keyed list that lost an entry would silently pose that joint at its
/// rest transform, and one that gained a duplicate would depend on which won.
pub fn pose_field(skin: &SkinData, pose: &[Trs]) -> Vec<JointPose> {
    skin.joints
        .iter()
        .enumerate()
        .map(|(i, joint)| {
            let trs = pose.get(i).copied().unwrap_or(joint.rest);
            JointPose {
                joint: joint.name.clone(),
                translation: trs.translation,
                rotation: trs.rotation.to_array(),
                // Omitted when it is 1, which is every joint of every rig this
                // repo has: a ragdoll does not scale bones, so carrying three
                // 1.0s per joint would be ninety numbers of noise in a bake.
                scale: (trs.scale != Vec3::ONE).then_some(trs.scale),
            }
        })
        .collect()
}

/// The field read back into a local pose.
///
/// Matched **by name**, not by position, because the field is text an author
/// may have edited and a rig is a file that may have been re-exported. A joint
/// the field does not mention keeps its rest transform; an entry naming a joint
/// the rig does not have is ignored. Neither is an error: a pose is a report of
/// where a run got to, and refusing to draw a character because a bake and a
/// mesh drifted apart is a worse failure than drawing it at rest.
pub fn pose_from_field(skin: &SkinData, field: &[JointPose]) -> Vec<Trs> {
    let mut pose: Vec<Trs> = skin.joints.iter().map(|joint| joint.rest).collect();
    for entry in field {
        let Some(i) = skin.joint_named(&entry.joint) else {
            continue;
        };
        pose[i] = Trs {
            translation: entry.translation,
            // `from_array` takes `[x, y, z, w]`, which is the order the field
            // documents and the order glTF and rapier both use.
            rotation: Quat::from_array(entry.rotation).normalize(),
            scale: entry.scale.unwrap_or(Vec3::ONE),
        };
    }
    pose
}

/// A global pose turned back into locals — the pose the clip was showing at
/// the moment a ragdoll fired.
///
/// `posed_globals` returns globals because that is what a palette, a report and
/// a proxy all want; a handoff wants the locals underneath them, because the
/// joints physics does *not* take over keep theirs for the rest of the run.
/// Deriving them here rather than threading a second return value out of the
/// planting solver keeps M32's one seam single.
pub fn locals_from_globals(skin: &SkinData, globals: &[glam::Mat4]) -> Vec<Trs> {
    skin.joints
        .iter()
        .enumerate()
        .map(|(i, joint)| {
            let Some(&global) = globals.get(i) else {
                return joint.rest;
            };
            let parent = match joint.parent {
                None => joint.ancestor,
                Some(parent) => globals.get(parent).copied().unwrap_or(glam::Mat4::IDENTITY),
            };
            let (scale, rotation, translation) =
                (parent.inverse() * global).to_scale_rotation_translation();
            Trs {
                translation,
                rotation: rotation.normalize(),
                scale,
            }
        })
        .collect()
}

/// The local pose implied by the joints physics solved, with every other joint
/// left where it was.
///
/// `solved` holds a skin-space global for each joint that has a body under it;
/// `frozen` is the local pose to keep for the joints that do not — the pose the
/// clip had at the moment of handoff, which is what a hand or a finger keeps
/// doing for the rest of the run.
///
/// **A ragdoll does not scale bones.** The scale in `frozen` survives untouched
/// rather than being read out of the decomposition: a solved global divided by
/// a parent that carries scale decomposes to a number that is arithmetically
/// right and physically meaningless, and a bone that quietly grew is the kind
/// of wrongness that reads as a skinning bug.
///
/// Parents are resolved rather than assumed, `globals_from`'s walk and its
/// reason — glTF does not require the joint array to be topological.
pub fn solve_pose(skin: &SkinData, frozen: &[Trs], solved: &[Option<glam::Mat4>]) -> Vec<Trs> {
    let n = skin.joints.len();
    let mut locals: Vec<Trs> = (0..n)
        .map(|i| frozen.get(i).copied().unwrap_or(skin.joints[i].rest))
        .collect();
    let mut globals = vec![glam::Mat4::IDENTITY; n];
    let mut resolved = vec![false; n];
    let mut remaining = n;

    while remaining > 0 {
        let before = remaining;
        for i in 0..n {
            if resolved[i] {
                continue;
            }
            let parent_global = match skin.joints[i].parent {
                None => skin.joints[i].ancestor,
                Some(parent) if resolved[parent] => globals[parent],
                Some(_) => continue,
            };
            match solved.get(i).copied().flatten() {
                Some(global) => {
                    globals[i] = global;
                    let local = parent_global.inverse() * global;
                    let (_, rotation, translation) = local.to_scale_rotation_translation();
                    locals[i].translation = translation;
                    locals[i].rotation = rotation.normalize();
                }
                None => globals[i] = parent_global * locals[i].matrix(),
            }
            resolved[i] = true;
            remaining -= 1;
        }
        if remaining == before {
            // A parent cycle: `globals_from` leaves the rest at identity for
            // the same malformed file, and so does this.
            break;
        }
    }

    locals
}

/// How few vertices a joint may own and still get a proxy.
///
/// A joint that a handful of stray vertices leak onto — a weight-painting
/// artefact, which every rig has — would otherwise fit a hitbox around three
/// points and hand the author a shape they have to notice and delete.
pub const MIN_FITTED_VERTICES: usize = 8;

/// A proxy set solved from the skin's vertex weights (M39 §8).
///
/// **This is not a runtime behaviour and must never become one.** M33 refused
/// automatic generation as "a derived artifact with no text form, which is
/// invariant 1 read backwards", and it was right about that; the answer is that
/// this runs when an author asks it to, via `engine fit-colliders`, and its
/// output is JSON they edit and commit. Nothing at load time or step time
/// consults a vertex weight.
///
/// Each vertex is assigned to the joint holding its **largest** weight — not
/// split across four, which would fit every hitbox around a blend region and
/// make each one too big — and a shape is fitted to that bucket's extent in the
/// joint's own bind frame.
pub fn fit_parts(
    skin: &SkinData,
    mesh: &crate::mesh::MeshData,
    shape: crate::components::ColliderShapeKind,
) -> Vec<ColliderPart> {
    use crate::components::ColliderShapeKind::{Capsule, Cuboid};

    let mut bounds: Vec<Option<(Vec3, Vec3)>> = vec![None; skin.joints.len()];
    let mut counts = vec![0usize; skin.joints.len()];

    for (index, position) in mesh.positions.iter().enumerate() {
        let (Some(joints), Some(weights)) = (
            mesh.joint_indices.get(index),
            mesh.joint_weights.get(index),
        ) else {
            break;
        };
        let Some(slot) = (0..4)
            .filter(|&i| weights[i] > 0.0)
            .max_by(|&a, &b| weights[a].total_cmp(&weights[b]))
        else {
            continue;
        };
        let joint = joints[slot] as usize;
        let Some(entry) = bounds.get_mut(joint) else {
            continue;
        };

        // The vertices live in skin space (M30's central decision), and
        // `inverse_bind` is exactly the map from there into this joint's bind
        // frame — so the extent measured here is the one an `offset` in the
        // joint's frame addresses.
        let local = skin.joints[joint]
            .inverse_bind
            .transform_point3(Vec3::from(*position));
        *entry = Some(match *entry {
            None => (local, local),
            Some((min, max)) => (min.min(local), max.max(local)),
        });
        counts[joint] += 1;
    }

    let mut parts = Vec::new();
    // The skin's own joint order, so a regenerated set diffs cleanly against
    // the committed one rather than reshuffling.
    for (joint, entry) in bounds.iter().enumerate() {
        let Some((min, max)) = entry else { continue };
        if counts[joint] < MIN_FITTED_VERTICES {
            continue;
        }
        let centre = (*min + *max) * 0.5;
        let half = ((*max - *min) * 0.5).max(Vec3::splat(1e-3));

        let mut part = ColliderPart {
            joint: skin.joints[joint].name.clone(),
            shape,
            name: None,
            half_extents: None,
            radius: None,
            half_height: None,
            offset: centre,
            rotation: Vec3::ZERO,
            sensor: false,
            fit: None,
        };
        match shape {
            Cuboid => part.half_extents = Some(half),
            Capsule => {
                // The longest axis is the bone; the other two are how thick the
                // limb is, averaged because a capsule has one radius and
                // picking the larger would sink the shape into the mesh.
                let axis = dominant_axis(half);
                let others: Vec<f32> = (0..3)
                    .filter(|&i| i != axis)
                    .map(|i| half[i])
                    .collect();
                let radius = ((others[0] + others[1]) * 0.5).max(1e-3);
                part.radius = Some(radius);
                part.half_height = Some((half[axis] - radius).max(1e-3));
                // A capsule's axis is local +Y, so a bone running along X or Z
                // needs the part turned onto it.
                part.rotation = match axis {
                    0 => Vec3::new(0.0, 0.0, -90.0),
                    2 => Vec3::new(90.0, 0.0, 0.0),
                    _ => Vec3::ZERO,
                };
            }
            // A sphere, and the mesh shapes a part may not be — for which the
            // largest half-extent is the only answer that encloses the bucket.
            _ => part.radius = Some(half.max_element()),
        }
        parts.push(part);
    }
    parts
}

fn dominant_axis(half: Vec3) -> usize {
    let a = half.to_array();
    (0..3).max_by(|&i, &j| a[i].total_cmp(&a[j])).unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::ColliderShapeKind;
    use glam::Mat4;

    fn joint(name: &str, parent: Option<usize>) -> crate::skeleton::Joint {
        crate::skeleton::Joint {
            node: 0,
            name: name.to_string(),
            parent,
            rest: Trs::default(),
            inverse_bind: Mat4::IDENTITY,
            ancestor: Mat4::IDENTITY,
        }
    }

    /// Hips → Spine → Head, and Hips → Thigh → Shin.
    fn humanoid() -> SkinData {
        SkinData {
            name: None,
            joints: vec![
                joint("Hips", None),
                joint("Spine", Some(0)),
                joint("Head", Some(1)),
                joint("Thigh", Some(0)),
                joint("Shin", Some(3)),
            ],
        }
    }

    fn part(joint: &str) -> ColliderPart {
        ColliderPart {
            joint: joint.to_string(),
            shape: ColliderShapeKind::Sphere,
            name: None,
            half_extents: None,
            radius: Some(0.1),
            half_height: None,
            offset: Vec3::ZERO,
            rotation: Vec3::ZERO,
            sensor: false,
            fit: None,
        }
    }

    #[test]
    fn a_part_hangs_from_the_nearest_proxied_ancestor() {
        let skin = humanoid();
        // Deliberately skips Spine: the head must still reach the hips.
        let parts = [part("Hips"), part("Head"), part("Shin")];
        let parents = parent_parts(&skin, &parts);
        assert_eq!(parents, vec![None, Some(0), Some(0)]);
        assert_eq!(roots(&parents), vec![0]);
    }

    #[test]
    fn a_part_set_that_skips_the_root_has_two_roots() {
        let skin = humanoid();
        // No part on Hips, so Head and Shin never meet — a ragdoll in two
        // pieces, which `ragdoll_disconnected_parts` is the report for.
        let parts = [part("Head"), part("Shin")];
        let parents = parent_parts(&skin, &parts);
        assert_eq!(roots(&parents), vec![0, 1]);
    }

    #[test]
    fn an_unknown_joint_does_not_panic_the_validator() {
        let skin = humanoid();
        let parts = [part("Hips"), part("Elbow")];
        assert_eq!(parent_parts(&skin, &parts), vec![None, None]);
    }

    #[test]
    fn a_pose_survives_the_round_trip_through_the_field() {
        let skin = humanoid();
        let mut pose: Vec<Trs> = skin.joints.iter().map(|j| j.rest).collect();
        pose[2].rotation = Quat::from_rotation_x(0.7);
        pose[2].translation = Vec3::new(0.0, 1.5, 0.25);
        pose[4].scale = Vec3::splat(1.5);

        let field = pose_field(&skin, &pose);
        assert_eq!(field.len(), skin.joints.len());
        assert!(field[0].scale.is_none(), "an unscaled joint omits its scale");
        assert_eq!(field[4].scale, Some(Vec3::splat(1.5)));

        let back = pose_from_field(&skin, &field);
        for (i, (a, b)) in pose.iter().zip(&back).enumerate() {
            assert!(
                (a.translation - b.translation).length() < 1e-6
                    && a.rotation.abs_diff_eq(b.rotation, 1e-6)
                    && (a.scale - b.scale).length() < 1e-6,
                "joint {i} did not survive: {a:?} vs {b:?}"
            );
        }
    }

    #[test]
    fn a_solved_joint_lands_exactly_where_physics_put_it() {
        let skin = humanoid();
        let frozen: Vec<Trs> = skin.joints.iter().map(|j| j.rest).collect();

        // Physics moved the shin — and only the shin. The local that comes
        // back must reproduce that global exactly once the hierarchy is walked
        // forward again, which is the round trip the render depends on.
        let target = Mat4::from_rotation_translation(
            Quat::from_rotation_z(0.9),
            Vec3::new(0.3, -0.8, 0.1),
        );
        let mut solved = vec![None; skin.joints.len()];
        solved[4] = Some(target);

        let pose = solve_pose(&skin, &frozen, &solved);
        let globals = crate::skeleton::globals_from(&skin, &pose);
        let (_, r, t) = globals[4].to_scale_rotation_translation();
        let (_, want_r, want_t) = target.to_scale_rotation_translation();
        assert!((t - want_t).length() < 1e-5, "{t} vs {want_t}");
        assert!(r.abs_diff_eq(want_r, 1e-5), "{r} vs {want_r}");

        // And nothing else moved: an unsolved joint keeps the local it was
        // handed, which is what a finger does for the rest of the run.
        assert_eq!(pose[2], frozen[2]);
    }

    #[test]
    fn a_solved_joint_does_not_rescale_its_bone() {
        let skin = humanoid();
        let mut frozen: Vec<Trs> = skin.joints.iter().map(|j| j.rest).collect();
        frozen[4].scale = Vec3::splat(2.0);
        // A parent carrying scale is what makes the naive decomposition wrong.
        frozen[3].scale = Vec3::splat(3.0);

        let mut solved = vec![None; skin.joints.len()];
        solved[4] = Some(Mat4::from_translation(Vec3::new(0.0, -0.5, 0.0)));

        let pose = solve_pose(&skin, &frozen, &solved);
        assert_eq!(
            pose[4].scale,
            Vec3::splat(2.0),
            "a ragdoll must not resize the bone it is posing"
        );
    }

    #[test]
    fn the_field_is_matched_by_name_not_by_position() {
        let skin = humanoid();
        // One entry, out of order, naming a joint the rig does have — plus one
        // it does not. The named joint moves; nothing else does.
        let field = vec![
            JointPose {
                joint: "Ghost".into(),
                translation: Vec3::splat(9.0),
                rotation: Quat::IDENTITY.to_array(),
                scale: None,
            },
            JointPose {
                joint: "Shin".into(),
                translation: Vec3::new(0.0, -0.4, 0.0),
                rotation: Quat::IDENTITY.to_array(),
                scale: None,
            },
        ];
        let pose = pose_from_field(&skin, &field);
        assert_eq!(pose[4].translation, Vec3::new(0.0, -0.4, 0.0));
        assert_eq!(pose[0].translation, Vec3::ZERO);
    }
}
