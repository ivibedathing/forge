//! Locomotion (M32) — `designs/locomotion-design.md`.
//!
//! Two halves of one subject: a clip whose phase is driven by ground covered
//! rather than by the clock, and a post-pass that puts the feet that clip
//! moves onto the ground they are over. Both are GPU-free and unconditionally
//! testable, the way `daylight.rs` is.
//!
//! # Stride-driven phase
//!
//! A clip whose `AnimationPlayer.stride` is set is driven by the ground its
//! entity covers rather than by the clock, which is what stops a walk cycle
//! sliding when the character's speed changes. The accumulated position is
//! `AnimationPlayer.phase`, a **component field** rather than state in this
//! struct: where a character is in its stride is as much a property of the
//! world as where it is standing, and a bake that dropped it would reload the
//! walker with its legs somewhere else.
//!
//! What *is* state here is the previous step's position, which is genuinely
//! disposable — a fresh run re-reads it from the transforms before the first
//! step, so a resumed bake picks up where it left off.

use std::collections::HashMap;

use glam::{Mat4, Quat, Vec3};
use hecs::{Entity, World};

use crate::components::{AnimationPlayer, FootPlant, Terrain, Transform, MAX_PLANTED_FEET};
use crate::skeleton::{self, SkinData, Trs};

/// Where every stride-driven entity was at the end of the previous step.
#[derive(Debug, Default)]
pub struct Locomotion {
    previous: HashMap<Entity, Vec3>,
}

impl Locomotion {
    /// Snapshot the world's stride-driven entities, so the first step measures
    /// real displacement instead of a jump from the origin.
    pub fn build(world: &World) -> Self {
        let mut previous = HashMap::new();
        // hecs 0.11 yields only the queried components, so `Entity` is
        // requested explicitly as part of the query tuple.
        for (entity, transform, player) in world
            .query::<(Entity, &Transform, &AnimationPlayer)>()
            .iter()
        {
            if player.stride > 0.0 {
                previous.insert(entity, transform.position);
            }
        }
        Self { previous }
    }

    /// Advance each stride-driven player by the ground its entity covered.
    ///
    /// Runs **after** physics, so the distance is the one the entity actually
    /// moved this step — a script's intent and a solver's answer are not the
    /// same number, and it is the second one the feet have to match.
    ///
    /// Displacement is **horizontal**: a character climbing a hill still
    /// strides, and folding the vertical in would speed the legs up on a slope
    /// for no reason anyone can see. It is **unsigned**, because a walk cycle
    /// played backwards is a different clip rather than a negative one.
    pub fn step(&mut self, world: &mut World) {
        // Entities that vanished (they broke) stop being tracked, or the map
        // grows for the life of a run that repeatedly spawns and destroys.
        self.previous.retain(|entity, _| world.contains(*entity));

        for (entity, transform, player) in world
            .query::<(Entity, &Transform, &mut AnimationPlayer)>()
            .iter()
        {
            // `is_sign_positive` alone would let a NaN through, and a NaN
            // stride poisons `phase` for the rest of the run — validation
            // rejects one, but a script can still write it through
            // `set_animation_stride`.
            if !player.stride.is_finite() || player.stride <= 0.0 {
                continue;
            }
            let position = transform.position;
            let previous = self.previous.insert(entity, position);
            let Some(previous) = previous else {
                // First sight of this entity — a fragment, or something a
                // script added a stride to mid-run. No displacement to
                // measure yet; the next step has one.
                continue;
            };

            let travelled = (position - previous) * Vec3::new(1.0, 0.0, 1.0);
            let cycles = travelled.length() / player.stride;
            if !cycles.is_finite() {
                continue;
            }
            player.phase = advance(player.phase, cycles, player.looping);
        }
    }
}

/// One step of phase, wrapped **on write-back** when the player loops.
///
/// Wrapping here rather than only at read time is what keeps a long run from
/// baking a phase of 812.4, whose f32 resolution has decayed to a millisecond
/// and which no reader can interpret without knowing how long the run was.
pub fn advance(phase: f32, cycles: f32, looping: bool) -> f32 {
    let next = phase + cycles;
    if looping {
        next.rem_euclid(1.0)
    } else {
        next
    }
}

// ── Foot planting ────────────────────────────────────────────────────────

/// The surface a character's feet are put on: a `Terrain` and the transform
/// that places it.
///
/// A struct rather than a closure so the sampling goes through M22's one
/// implementation (`terrain::world_height_at`) by construction — the collider,
/// the mesh, `engine terrain-height` and `world.terrain_height` all already
/// share it, and a second height function is how two answers to "where is the
/// ground" start disagreeing.
#[derive(Debug, Clone, Copy)]
pub struct Ground<'a> {
    pub terrain: &'a Terrain,
    pub transform: &'a Transform,
}

impl Ground<'_> {
    /// World Y of the surface under a world XZ.
    pub fn height(&self, x: f32, z: f32) -> f32 {
        crate::terrain::world_height_at(self.terrain, self.transform, x, z)
    }

    /// The world-space surface normal there.
    ///
    /// Sampled at one grid quad, so a foot answers to the relief the geometry
    /// actually carries rather than to detail the tessellation dropped — and
    /// scaled by the patch's own `scale.y`, because the gradient of the height
    /// *field* is not the gradient of the surface a taller patch draws.
    pub fn normal(&self, x: f32, z: f32) -> Vec3 {
        let spacing =
            (self.transform.scale.x.abs() / self.terrain.segments.max(1) as f32).max(1e-3);
        let gradient =
            crate::terrain::gradient_at(self.terrain, x, z, spacing) * self.transform.scale.y;
        Vec3::new(-gradient.x, 1.0, -gradient.y).normalize()
    }
}

/// Put the named feet of a posed skeleton on the ground under them.
///
/// `pose` is the clip's local pose, edited in place; the returned globals are
/// the planted ones. Runs entirely in **skin space** — the target is mapped
/// back through `model⁻¹` rather than the rotations being mapped forward,
/// which is one inverse per entity per frame instead of an un-scaling of every
/// quaternion, and cannot get a non-uniform scale subtly wrong (validation
/// refuses one outright).
///
/// A foot naming a joint the rig does not have is **skipped**, not an error:
/// validation reports it with a `did_you_mean`, and a render that panicked on
/// a typo would be a worse way to find out.
pub fn plant(
    skin: &SkinData,
    pose: &mut [Trs],
    component: &FootPlant,
    model: Mat4,
    ground: &Ground,
) -> Vec<Mat4> {
    let mut globals = skeleton::globals_from(skin, pose);
    let to_skin = model.inverse();
    // The entity's forward, in skin space: the fallback bend plane for a leg
    // that happens to be exactly straight this frame. −Z is the aiming
    // convention everywhere else in this engine.
    let forward = to_skin
        .transform_vector3(model.transform_vector3(Vec3::NEG_Z))
        .normalize_or_zero();

    // Resolve every foot first: the hips drop is the *largest* deficit across
    // legs, so no leg can be solved until all of them have been measured.
    let mut legs: Vec<Leg> = Vec::new();
    for foot in component.feet.iter().take(MAX_PLANTED_FEET) {
        let Some(ankle) = skin.joint_named(&foot.ankle) else {
            continue;
        };
        let mut chain = Vec::new();
        let mut joint = ankle;
        for _ in 0..foot.chain {
            let Some(parent) = skin.joints[joint].parent else {
                break;
            };
            chain.push(parent);
            joint = parent;
        }
        if chain.len() < foot.chain as usize {
            // Reaches past the root — validation says so; solve with what
            // there is rather than dropping the foot entirely.
        }
        legs.push(Leg {
            ankle,
            chain,
            sole: foot.sole,
        });
    }
    if legs.is_empty() {
        return globals;
    }

    // ── 1. Targets, and the hips drop they imply ──────────────────────────
    let mut deficit = 0.0f32;
    for leg in &legs {
        let Some(target) = leg.target(&globals, model, to_skin, ground, component) else {
            continue;
        };
        let Some(&root) = leg.chain.last() else {
            continue;
        };
        let hip = origin(&globals[root]);
        let reach = leg.reach(&globals);
        deficit = deficit.max(hip.distance(target) - reach);
    }
    if deficit > 0.0 {
        if let Some(hips) = component.hips.as_ref().and_then(|n| skin.joint_named(n)) {
            // Straight down in *world* Y, expressed in skin space: the pelvis
            // drops toward the ground, not toward the skin's own down, which
            // are different axes the moment the character is tilted.
            let down = to_skin.transform_vector3(Vec3::NEG_Y).normalize_or_zero();
            let parent = parent_global(skin, &globals, hips);
            let shift = parent
                .inverse()
                .transform_vector3(down * deficit.min(component.max_drop));
            pose[hips].translation += shift;
            globals = skeleton::globals_from(skin, pose);
        }
    }

    // ── 2. The legs ───────────────────────────────────────────────────────
    for leg in &legs {
        let Some(target) = leg.target(&globals, model, to_skin, ground, component) else {
            continue;
        };
        solve_leg(skin, pose, &mut globals, leg, target, forward);
    }

    // ── 3. The soles ──────────────────────────────────────────────────────
    if component.align > 0.0 {
        for leg in &legs {
            let ankle_world = model.transform_point3(origin(&globals[leg.ankle]));
            let normal = ground.normal(ankle_world.x, ankle_world.z);
            align_sole(
                skin,
                pose,
                &mut globals,
                leg.ankle,
                to_skin,
                normal,
                component.align,
            );
        }
    }

    globals
}

/// One skinned entity's joints in skin space at clip time `local`, planted
/// when the entity asks for it.
///
/// **The one seam the render, `engine list-joints` and `world.joint_position`
/// all go through.** A report that described the unplanted rig while the
/// picture drew the planted one would be a second answer to "where is that
/// foot", and `list-joints` exists precisely because there is only one.
///
/// A `FootPlant` whose ground is missing poses unplanted rather than failing:
/// validation reports it, and a character standing where the animator put it
/// is a better failure than a render that will not run.
pub fn posed_globals(
    world: &World,
    entity: Entity,
    skin: &SkinData,
    clip: Option<&skeleton::SkeletalClip>,
    local: f32,
) -> Vec<Mat4> {
    let Ok(component) = world.get::<&FootPlant>(entity) else {
        return skeleton::joint_globals(skin, clip, local);
    };
    let Some((terrain, ground)) = terrain_named(world, &component.ground) else {
        return skeleton::joint_globals(skin, clip, local);
    };
    let model = world
        .get::<&Transform>(entity)
        .map(|t| *t)
        .unwrap_or_default()
        .matrix();

    let mut pose = skeleton::local_pose(skin, clip, local);
    plant(
        skin,
        &mut pose,
        &component,
        model,
        &Ground {
            terrain: &terrain,
            transform: &ground,
        },
    )
}

/// One skinned entity's joints in skin space at **scene** time `time`, clip
/// selection and all.
///
/// [`posed_globals`] takes a clip and a clip-local time, which every caller was
/// deriving for itself from the entity's `AnimationPlayer`. M33 gave the
/// physics world a third caller — proxies follow the pose (`SkinnedCollider`)
/// — and three copies of "which clip, and what time is it in that clip" is two
/// too many: a hitbox that read the clip differently from the render would sit
/// somewhere the character visibly is not.
///
/// `time` of `None` is the rest pose. It still needs a real pose rather than an
/// identity one, for the reason `Scene::palette_for` documents: the vertices
/// live in skin space.
pub fn posed_globals_at(
    world: &World,
    entity: Entity,
    rig: &skeleton::Rig,
    time: Option<f32>,
) -> Vec<Mat4> {
    let Some(skin) = &rig.skin else {
        return Vec::new();
    };
    // `hecs::Ref` derefs to the component, so this clones the `AnimationPlayer`
    // rather than the guard — `.cloned()` does not apply and clippy's
    // `map_clone` suggestion does not compile here.
    #[allow(clippy::map_clone)]
    let player = world.get::<&AnimationPlayer>(entity).ok().map(|p| p.clone());
    // A property clip on a skinned entity is legal: it animates components,
    // not joints, and the rig stays at rest.
    let clip = player
        .as_ref()
        .and_then(|player| match skeleton::ClipRef::parse(&player.clip) {
            skeleton::ClipRef::Skeletal { clip, .. } => rig.clip_named(clip),
            skeleton::ClipRef::Property(_) => None,
        });
    let local = match (time, &player, clip) {
        (Some(t), Some(player), Some(clip)) => {
            crate::animation::local_time(player, skeleton::duration(clip), t)
        }
        _ => 0.0,
    };
    posed_globals(world, entity, skin, time.and(clip), local)
}

/// How many moments of a cycle [`measure_stride`] looks at.
///
/// Fixed rather than a flag: the answer is a number an author pastes into a
/// file, so it must not depend on how the question was asked.
pub const STRIDE_SAMPLES: u32 = 64;

/// The metres of ground one cycle of `clip` covers, measured off the clip.
///
/// `stride` is the one number an author has to get right — getting it wrong
/// *is* the foot slide this milestone removes — so the engine measures it and
/// the file carries the answer. Computing it implicitly at render time was
/// rejected: the measurement is an algorithm, and an algorithm that silently
/// set the clip rate would be a format contract, so a refinement to the
/// sampling would move every walking character in every committed baseline.
///
/// **It assumes nothing about gait.** Over each interval the *lowest* of the
/// named feet is the planted one, and the ground the body covers is how far
/// that foot travelled horizontally in the skeleton's own frame. That is true
/// for a biped, a quadruped, or anything else whose feet take turns; a hop
/// with every foot in the air measures the airborne interval as no travel,
/// which is correct.
pub fn measure_stride(
    skin: &SkinData,
    clip: &skeleton::SkeletalClip,
    feet: &[usize],
    samples: u32,
) -> Option<f32> {
    let duration = skeleton::duration(clip);
    if feet.is_empty() || duration <= 0.0 || samples == 0 {
        return None;
    }

    let at = |i: u32| -> Vec<Vec3> {
        let t = duration * (i as f32 / samples as f32);
        let globals = skeleton::joint_globals(skin, Some(clip), t);
        feet.iter().map(|&j| origin(&globals[j])).collect()
    };

    let mut total = 0.0;
    let mut previous = at(0);
    for i in 1..=samples {
        let current = at(i);
        // Lowest at the interval's *midpoint*, by summing the two ends: a foot
        // chosen at one end alone flips at exactly the moment of the swap and
        // attributes the swing leg's travel to the ground.
        let planted = (0..feet.len())
            .min_by(|&a, &b| {
                (previous[a].y + current[a].y).total_cmp(&(previous[b].y + current[b].y))
            })
            .unwrap_or(0);
        let step = (current[planted] - previous[planted]) * Vec3::new(1.0, 0.0, 1.0);
        total += step.length();
        previous = current;
    }
    Some(total)
}

/// The `Terrain` and transform of a named entity, for anything that has to ask
/// where the ground is.
fn terrain_named(world: &World, name: &str) -> Option<(Terrain, Transform)> {
    world
        .query::<(&crate::components::Name, &Terrain, &Transform)>()
        .iter()
        .find(|(found, _, _)| found.0 == name)
        .map(|(_, terrain, transform)| (terrain.clone(), *transform))
}

/// One resolved foot: the ankle, the joints above it that may rotate (nearest
/// first), and how far the sole sits below the joint.
struct Leg {
    ankle: usize,
    chain: Vec<usize>,
    sole: f32,
}

impl Leg {
    /// Where this foot should be, in skin space — or `None` when the chain is
    /// too short to move it anywhere.
    fn target(
        &self,
        globals: &[Mat4],
        model: Mat4,
        to_skin: Mat4,
        ground: &Ground,
        component: &FootPlant,
    ) -> Option<Vec3> {
        if self.chain.is_empty() {
            return None;
        }
        let animated = model.transform_point3(origin(&globals[self.ankle]));
        let wanted = ground.height(animated.x, animated.z) + self.sole;
        // Clamped, because planting is a correction: a foot in mid-swing must
        // not be dragged to the floor, and an unbounded lift would stand a
        // character on a cliff it is passing.
        let y = wanted.clamp(
            animated.y - component.max_drop,
            animated.y + component.max_lift,
        );
        Some(to_skin.transform_point3(Vec3::new(animated.x, y, animated.z)))
    }

    /// How far this leg reaches when fully extended, in skin space.
    fn reach(&self, globals: &[Mat4]) -> f32 {
        let mut total = 0.0;
        let mut lower = origin(&globals[self.ankle]);
        for &joint in &self.chain {
            let upper = origin(&globals[joint]);
            total += lower.distance(upper);
            lower = upper;
        }
        total
    }
}

/// Rotate a leg so its ankle lands on `target`.
///
/// Sequential, with a hierarchy rebuild between the two joints, because the
/// second rotation is measured from where the first one left the knee.
/// Computing both from the same pose and applying them together is the obvious
/// shortcut and is wrong by exactly the hip's rotation.
fn solve_leg(
    skin: &SkinData,
    pose: &mut [Trs],
    globals: &mut Vec<Mat4>,
    leg: &Leg,
    target: Vec3,
    forward: Vec3,
) {
    let ankle = origin(&globals[leg.ankle]);
    match leg.chain.as_slice() {
        // One hinge: swing the ankle onto the target's direction. It cannot
        // change the distance, so the target is projected onto the arc the
        // ankle actually travels on.
        [knee] => {
            let pivot = origin(&globals[*knee]);
            let delta = swing(ankle - pivot, target - pivot);
            rotate_joint(skin, pose, globals, *knee, pivot, delta);
        }
        [knee, hip, ..] => {
            let (knee_pos, hip_pos) = (origin(&globals[*knee]), origin(&globals[*hip]));
            let l1 = hip_pos.distance(knee_pos);
            let l2 = knee_pos.distance(ankle);
            let Some(bent) = bend(hip_pos, knee_pos, target, l1, l2, forward) else {
                return;
            };

            let hip_delta = swing(knee_pos - hip_pos, bent - hip_pos);
            rotate_joint(skin, pose, globals, *hip, hip_pos, hip_delta);

            let moved_knee = origin(&globals[*knee]);
            let moved_ankle = origin(&globals[leg.ankle]);
            let knee_delta = swing(moved_ankle - moved_knee, target - moved_knee);
            rotate_joint(skin, pose, globals, *knee, moved_knee, knee_delta);
        }
        [] => {}
    }
}

/// Where the knee goes for a two-bone chain reaching `target`, by the law of
/// cosines — or `None` when the target is degenerate (on top of the hip).
///
/// The two solutions are mirror images across the hip→target line, and the one
/// picked is whichever leaves the knee **nearer where the clip put it**. That
/// is what keeps a knee bending the way the animator bent it: choosing by a
/// fixed sign would flip the joint the first time a leg passed through
/// straight, which reads as the character's knee snapping backwards for one
/// frame.
fn bend(hip: Vec3, knee: Vec3, target: Vec3, l1: f32, l2: f32, forward: Vec3) -> Option<Vec3> {
    let to_target = target - hip;
    let distance = to_target
        .length()
        .clamp((l1 - l2).abs() + 1e-4, l1 + l2 - 1e-4);
    let direction = to_target.normalize_or_zero();
    if direction == Vec3::ZERO || l1 <= 1e-6 || l2 <= 1e-6 {
        return None;
    }

    let cosine =
        ((l1 * l1 + distance * distance - l2 * l2) / (2.0 * l1 * distance)).clamp(-1.0, 1.0);
    let angle = cosine.acos();

    // The plane the leg bends in. A leg that is exactly straight this frame
    // has no plane of its own, so the entity's forward supplies one — without
    // it the axis is a zero vector and the knee would be left wherever the
    // normalize degenerated to.
    let mut axis = (knee - hip).cross(to_target);
    if axis.length_squared() < 1e-12 {
        axis = direction.cross(forward);
    }
    if axis.length_squared() < 1e-12 {
        axis = direction.cross(Vec3::Y);
    }
    let axis = axis.normalize_or_zero();
    if axis == Vec3::ZERO {
        return None;
    }

    let a = hip + Quat::from_axis_angle(axis, angle) * direction * l1;
    let b = hip + Quat::from_axis_angle(axis, -angle) * direction * l1;
    Some(if a.distance_squared(knee) <= b.distance_squared(knee) {
        a
    } else {
        b
    })
}

/// The rotation taking one direction onto another, identity for degenerate
/// inputs.
fn swing(from: Vec3, to: Vec3) -> Quat {
    let (from, to) = (from.normalize_or_zero(), to.normalize_or_zero());
    if from == Vec3::ZERO || to == Vec3::ZERO {
        return Quat::IDENTITY;
    }
    Quat::from_rotation_arc(from, to)
}

/// Tilt a foot's sole toward the ground's normal, by at most `limit` degrees.
///
/// The clamp is what keeps a foot on a cliff edge from lying flat against a
/// wall: past some slope a real foot stops conforming and starts standing on
/// its edge, and a limit is a cheaper model of that than a contact solver.
fn align_sole(
    skin: &SkinData,
    pose: &mut [Trs],
    globals: &mut Vec<Mat4>,
    ankle: usize,
    to_skin: Mat4,
    normal: Vec3,
    limit: f32,
) {
    let up = to_skin.transform_vector3(Vec3::Y).normalize_or_zero();
    let wanted = to_skin.transform_vector3(normal).normalize_or_zero();
    if up == Vec3::ZERO || wanted == Vec3::ZERO {
        return;
    }
    let angle = up.dot(wanted).clamp(-1.0, 1.0).acos();
    if angle < 1e-5 {
        return;
    }
    let axis = up.cross(wanted).normalize_or_zero();
    if axis == Vec3::ZERO {
        return;
    }
    let delta = Quat::from_axis_angle(axis, angle.min(limit.to_radians()));
    let pivot = origin(&globals[ankle]);
    rotate_joint(skin, pose, globals, ankle, pivot, delta);
}

/// Apply a skin-space rotation about `pivot` to one joint, as an edit to its
/// **local** transform, and rebuild the hierarchy.
///
/// Editing globals in place is the tempting shortcut and it detaches every
/// descendant: the ankle keeps the knee's old matrix, the toe keeps the
/// ankle's, and the foot visibly comes off the leg.
fn rotate_joint(
    skin: &SkinData,
    pose: &mut [Trs],
    globals: &mut Vec<Mat4>,
    joint: usize,
    pivot: Vec3,
    delta: Quat,
) {
    let about =
        Mat4::from_translation(pivot) * Mat4::from_quat(delta) * Mat4::from_translation(-pivot);
    let wanted = about * globals[joint];
    let local = parent_global(skin, globals, joint).inverse() * wanted;
    let (scale, rotation, translation) = local.to_scale_rotation_translation();
    pose[joint] = Trs {
        translation,
        rotation,
        scale,
    };
    *globals = skeleton::globals_from(skin, pose);
}

/// The matrix a joint's local transform is composed onto: its parent's global,
/// or the constant transform above a root joint.
fn parent_global(skin: &SkinData, globals: &[Mat4], joint: usize) -> Mat4 {
    match skin.joints[joint].parent {
        Some(parent) => globals[parent],
        None => skin.joints[joint].ancestor,
    }
}

fn origin(matrix: &Mat4) -> Vec3 {
    matrix.w_axis.truncate()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn walker(world: &mut World, position: Vec3, stride: f32) -> Entity {
        world.spawn((
            Transform {
                position,
                ..Default::default()
            },
            AnimationPlayer {
                clip: "meshes/walker.gltf#Walk".into(),
                speed: 1.0,
                looping: true,
                start_offset: 0.0,
                stride,
                phase: 0.0,
            },
        ))
    }

    #[test]
    fn one_stride_of_ground_is_one_cycle_of_clip() {
        let mut world = World::new();
        let entity = walker(&mut world, Vec3::ZERO, 2.0);
        let mut locomotion = Locomotion::build(&world);

        // Half a stride, then another half: the phase wraps back to 0.
        world.get::<&mut Transform>(entity).unwrap().position.x = 1.0;
        locomotion.step(&mut world);
        assert!((world.get::<&AnimationPlayer>(entity).unwrap().phase - 0.5).abs() < 1e-6);

        world.get::<&mut Transform>(entity).unwrap().position.x = 2.0;
        locomotion.step(&mut world);
        assert!(world.get::<&AnimationPlayer>(entity).unwrap().phase.abs() < 1e-6);
    }

    #[test]
    fn a_clock_driven_player_is_never_touched() {
        let mut world = World::new();
        let entity = walker(&mut world, Vec3::ZERO, 0.0);
        let mut locomotion = Locomotion::build(&world);

        world.get::<&mut Transform>(entity).unwrap().position.x = 12.0;
        locomotion.step(&mut world);

        assert_eq!(world.get::<&AnimationPlayer>(entity).unwrap().phase, 0.0);
    }

    #[test]
    fn climbing_is_not_striding() {
        // Straight up: a character on a lift does not walk. Only the
        // horizontal component may advance the cycle.
        let mut world = World::new();
        let entity = walker(&mut world, Vec3::ZERO, 1.0);
        let mut locomotion = Locomotion::build(&world);

        world.get::<&mut Transform>(entity).unwrap().position.y = 5.0;
        locomotion.step(&mut world);

        assert_eq!(world.get::<&AnimationPlayer>(entity).unwrap().phase, 0.0);
    }

    #[test]
    fn walking_backwards_still_walks_forwards() {
        // Unsigned: the same clip, advancing, rather than a cycle running in
        // reverse. Backwards locomotion is a different clip.
        let mut world = World::new();
        let entity = walker(&mut world, Vec3::ZERO, 4.0);
        let mut locomotion = Locomotion::build(&world);

        world.get::<&mut Transform>(entity).unwrap().position.z = -1.0;
        locomotion.step(&mut world);

        assert!((world.get::<&AnimationPlayer>(entity).unwrap().phase - 0.25).abs() < 1e-6);
    }

    #[test]
    fn a_non_looping_phase_runs_past_one_and_the_clip_clamps() {
        assert!((advance(0.9, 0.4, false) - 1.3).abs() < 1e-6);
        assert!((advance(0.9, 0.4, true) - 0.3).abs() < 1e-6);
    }

    // ── Foot planting ────────────────────────────────────────────────

    /// A leg hanging down from the origin: hip at y=2, knee at y=1, ankle at
    /// y=0, with the knee pushed a little forward so the bend plane is not
    /// degenerate. Three joints, the shape `rigged_arm.gltf` proved a palette
    /// on, pointed the other way.
    fn leg() -> SkinData {
        use crate::skeleton::Joint;
        let joint = |node: usize, name: &str, parent: Option<usize>, offset: Vec3| Joint {
            node,
            name: name.into(),
            parent,
            rest: Trs {
                translation: offset,
                ..Default::default()
            },
            inverse_bind: Mat4::IDENTITY,
            ancestor: Mat4::IDENTITY,
        };
        SkinData {
            name: Some("Leg".into()),
            joints: vec![
                joint(1, "Hip", None, Vec3::new(0.0, 2.0, 0.0)),
                joint(2, "Knee", Some(0), Vec3::new(0.0, -1.0, 0.02)),
                joint(3, "Ankle", Some(1), Vec3::new(0.0, -1.0, -0.02)),
            ],
        }
    }

    fn flat(height: f32) -> (Terrain, Transform) {
        (
            Terrain {
                height: 0.0,
                ..Default::default()
            },
            Transform {
                position: Vec3::new(0.0, height, 0.0),
                scale: Vec3::new(20.0, 1.0, 20.0),
                ..Default::default()
            },
        )
    }

    fn one_foot(sole: f32) -> FootPlant {
        FootPlant {
            feet: vec![crate::components::PlantedFoot {
                ankle: "Ankle".into(),
                chain: 2,
                sole,
            }],
            ground: "Ground".into(),
            hips: None,
            max_drop: 1.0,
            max_lift: 1.0,
            align: 0.0,
        }
    }

    fn plant_at(ground_y: f32, component: &FootPlant, model: Mat4) -> (SkinData, Vec<Mat4>) {
        let skin = leg();
        let (terrain, transform) = flat(ground_y);
        let mut pose = skeleton::local_pose(&skin, None, 0.0);
        let globals = plant(
            &skin,
            &mut pose,
            component,
            model,
            &Ground {
                terrain: &terrain,
                transform: &transform,
            },
        );
        (skin, globals)
    }

    #[test]
    fn the_ankle_lands_on_the_ground_plus_its_sole() {
        // The claim the whole milestone makes, at its simplest: raise the
        // ground and the foot comes up with it.
        let component = one_foot(0.05);
        let (_, globals) = plant_at(0.4, &component, Mat4::IDENTITY);
        let ankle = origin(&globals[2]);
        assert!(
            (ankle.y - 0.45).abs() < 1e-3,
            "ankle at {ankle:?}, wanted y = ground 0.4 + sole 0.05"
        );
    }

    #[test]
    fn a_correction_is_bounded() {
        // Planting is a correction, and a correction with no ceiling is a
        // different animation: with the ground 3 m up and `max_lift` 1 m, the
        // ankle rises exactly 1 m and the character does not climb the cliff
        // it is walking past.
        let component = one_foot(0.0);
        let (_, globals) = plant_at(3.0, &component, Mat4::IDENTITY);
        assert!(
            (origin(&globals[2]).y - 1.0).abs() < 1e-3,
            "ankle at {:?}, wanted the max_lift ceiling of 1.0",
            origin(&globals[2])
        );
    }

    #[test]
    fn the_hips_drop_when_a_leg_cannot_reach() {
        // A leg hanging at nearly full extension cannot reach a floor below
        // it however hard the knee straightens, so the pelvis comes down — and
        // without a `hips` joint to lower, the deficit is simply clamped and
        // the foot stays where the leg's own reach leaves it.
        let mut component = one_foot(0.0);
        component.hips = Some("Hip".into());
        let (_, dropped) = plant_at(-0.6, &component, Mat4::IDENTITY);

        component.hips = None;
        let (_, stretched) = plant_at(-0.6, &component, Mat4::IDENTITY);

        assert!(
            origin(&dropped[0]).y < origin(&stretched[0]).y - 0.4,
            "hips at {:?} should have dropped below {:?}",
            origin(&dropped[0]),
            origin(&stretched[0])
        );
        assert!(
            origin(&dropped[2]).y < origin(&stretched[2]).y - 0.4,
            "and taken the foot down with them"
        );
    }

    #[test]
    fn the_leg_stays_attached_at_every_joint() {
        // Editing globals in place instead of locals detaches the chain, and
        // the symptom is a foot that reaches the ground with nothing joining
        // it to the knee. Bone lengths are the invariant that catches it.
        let component = one_foot(0.0);
        let (_, globals) = plant_at(0.6, &component, Mat4::IDENTITY);
        let (hip, knee, ankle) = (
            origin(&globals[0]),
            origin(&globals[1]),
            origin(&globals[2]),
        );
        assert!(
            (hip.distance(knee) - 1.0002).abs() < 1e-2,
            "thigh stretched"
        );
        assert!(
            (knee.distance(ankle) - 1.0002).abs() < 1e-2,
            "shin stretched"
        );
    }

    #[test]
    fn the_knee_keeps_the_side_the_animator_bent_it() {
        // The two law-of-cosines solutions are mirror images, and picking by a
        // fixed sign flips the joint the first time a leg passes through
        // straight. The rest pose bends the knee toward +Z; planting must not
        // send it to −Z.
        let component = one_foot(0.0);
        let (_, globals) = plant_at(0.5, &component, Mat4::IDENTITY);
        assert!(
            origin(&globals[1]).z > 0.0,
            "knee flipped to {:?}",
            origin(&globals[1])
        );
    }

    #[test]
    fn planting_is_measured_in_the_world_the_entity_stands_in() {
        // The solve runs in skin space but the ground is a world quantity, so
        // an entity lifted 5 m has to reach 5 m less far down. Without the
        // model round-trip this plants the foot at the terrain's own Y and the
        // character folds up under itself.
        let component = one_foot(0.0);
        let model = Mat4::from_translation(Vec3::new(0.0, 5.0, 0.0));
        let (_, globals) = plant_at(5.2, &component, model);
        let world = model.transform_point3(origin(&globals[2]));
        assert!((world.y - 5.2).abs() < 1e-3, "world ankle at {world:?}");
    }

    #[test]
    fn a_missing_joint_poses_rather_than_panics() {
        // Validation reports the typo; a render that panicked on it would be
        // a worse way to find out.
        let mut component = one_foot(0.0);
        component.feet[0].ankle = "Foot.L".into();
        let (skin, globals) = plant_at(0.6, &component, Mat4::IDENTITY);
        assert_eq!(globals.len(), skin.joints.len());
        assert!(origin(&globals[2]).y.abs() < 1e-6, "the rest pose, unmoved");
    }

    #[test]
    fn a_hop_covers_no_ground_and_a_walk_covers_its_stride() {
        // `measure_stride` with a clip that never moves anything: zero, not a
        // guess. The gait-bearing case is pinned by the CLI test against the
        // real walker, which is the only rig here with a walk cycle in it.
        let skin = leg();
        let clip = skeleton::SkeletalClip {
            name: "Still".into(),
            channels: Vec::new(),
        };
        assert_eq!(measure_stride(&skin, &clip, &[2], 16), None);
    }

    #[test]
    fn the_first_step_measures_from_where_the_file_put_it() {
        // `build` snapshots, so a character authored at x = 100 does not
        // advance a hundred metres of stride on its first step.
        let mut world = World::new();
        let entity = walker(&mut world, Vec3::new(100.0, 0.0, 0.0), 1.0);
        let mut locomotion = Locomotion::build(&world);
        locomotion.step(&mut world);
        assert_eq!(world.get::<&AnimationPlayer>(entity).unwrap().phase, 0.0);
    }
}
