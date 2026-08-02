//! Break application (M14): swapping a `Breakable` entity for its fragments.
//!
//! Runs once per fixed step, after physics — the caller collects the step's
//! break decisions ([`PhysicsWorld::take_pending_breaks`]) plus any
//! script-forced names and applies them here, in entity-name order. Fragments
//! are ordinary entities from the moment they spawn: they render, trace, and
//! bake like anything else; nothing downstream has a "debris" case.

use std::collections::HashSet;

use engine_core::components::{
    BodyKind, Breakable, Collider, ColliderShapeKind, Material, Mesh, Name, RigidBody, Shard,
    Transform, HALF_CUBE,
};
use engine_core::Result;
use glam::Vec3;
use hecs::{Entity, EntityBuilder, World};

use crate::{PendingBreak, PhysicsWorld};

/// One applied break: the despawned entity and the fragments that replaced
/// it. The caller uses the pairs to update its own name table (`Scene`
/// keeps one) and to report the break in traces.
#[derive(Debug, Clone)]
pub struct BreakEvent {
    /// The broken (now despawned) entity's name.
    pub entity: String,
    /// The spawned fragment entities, in fragment order.
    pub fragments: Vec<(String, Entity)>,
    /// The burst of dust this break threw off (M44), if its material has one
    /// and it was not turned off: `Parent.dust`, a `ParticleEmitter` entity
    /// that despawns itself once its last particle dies.
    ///
    /// Reported separately from the fragments because it is not one: it has
    /// no body, no collider and no mass, and a caller that gives fragments a
    /// physics presence must not give it one.
    pub dust: Option<(String, Entity)>,
}

/// Apply this step's breaks: the physics world's pending decisions plus
/// `forced` names (script `world.break_entity` calls). Deterministic:
/// candidates apply in entity-name order, fragments in authored order.
/// A forced name that no longer exists or has no `Breakable` is skipped —
/// scripts validate at call time, and the entity may have broken earlier
/// the same step.
pub fn apply_breaks(
    world: &mut World,
    physics: &mut PhysicsWorld,
    forced: &[String],
) -> Result<Vec<BreakEvent>> {
    let mut pending = physics.take_pending_breaks();

    if !forced.is_empty() {
        for name in forced {
            let found = world
                .query::<(Entity, &Name)>()
                .iter()
                .find(|(_, n)| &n.0 == name)
                .map(|(entity, _)| entity);
            if let Some(entity) = found {
                if !pending.iter().any(|p| p.entity == entity) {
                    pending.push(PendingBreak {
                        entity,
                        kick: None,
                        // A scripted break has no geometry to scatter from.
                        impact: None,
                    });
                }
            }
        }
    }
    if pending.is_empty() {
        return Ok(Vec::new());
    }

    // Name order regardless of how the causes interleaved. The sort is
    // stable and physics pushed explosion-caused entries first, so a
    // duplicate keeps its kick through the dedup.
    pending.sort_by_key(|p| {
        world
            .get::<&Name>(p.entity)
            .map(|n| n.0.clone())
            .unwrap_or_default()
    });
    pending.dedup_by_key(|p| p.entity);

    let mut taken_names: HashSet<String> = world
        .query::<&Name>()
        .iter()
        .map(|name| name.0.clone())
        .collect();

    let mut events = Vec::new();
    for pending_break in pending {
        let entity = pending_break.entity;
        let Ok(name) = world.get::<&Name>(entity).map(|n| n.0.clone()) else {
            continue; // already despawned this step
        };
        let Ok(breakable) = world.get::<&Breakable>(entity).map(|b| (*b).clone()) else {
            continue; // forced name without a Breakable
        };
        let transform = world
            .get::<&Transform>(entity)
            .map(|t| *t)
            .unwrap_or_default();
        let material = world.get::<&Material>(entity).map(|m| (*m).clone()).ok();
        // `ccd` is inherited rather than fixed off, because a fragment is the
        // body in this engine most likely to need it: it is thrown, it is
        // smaller than the thing it came off, and a small fast convex hull
        // against a `trimesh` terrain is the tunnelling case. The tour's
        // wooden crates are what found it — a shard sailing at 8 m/s went
        // through the ground on the way down and fell forever in silence.
        // Inheriting keeps every scene that predates it byte-identical: nothing
        // that breaks today sets `ccd` on the parent.
        let (linear, angular_degrees, ccd) = world
            .get::<&RigidBody>(entity)
            .map(|b| (b.linear_velocity, b.angular_velocity, b.ccd))
            .unwrap_or((Vec3::ZERO, Vec3::ZERO, false));

        let _ = world.despawn(entity);
        physics.remove_entity(entity);
        taken_names.remove(&name);

        let parent_rotation = transform.quat();
        let angular_radians = angular_degrees * (std::f32::consts::PI / 180.0);

        // What it is made of, and so how its pieces behave once they are
        // pieces (M43). `None` is M14: the parent's motion and nothing else.
        let behaviour = breakable.material.map(|m| m.behaviour());

        let mut fragments = Vec::with_capacity(breakable.fragments.len());
        for (i, fragment) in breakable.fragments.iter().enumerate() {
            let mut fragment_name = format!("{name}.frag{i}");
            let mut attempt = 2;
            while taken_names.contains(&fragment_name) {
                fragment_name = format!("{name}.frag{i}_{attempt}");
                attempt += 1;
            }
            taken_names.insert(fragment_name.clone());

            let position =
                transform.position + parent_rotation * (fragment.offset * transform.scale);
            let rotation = parent_rotation
                * glam::Quat::from_euler(
                    glam::EulerRot::XYZ,
                    fragment.rotation.x.to_radians(),
                    fragment.rotation.y.to_radians(),
                    fragment.rotation.z.to_radians(),
                );
            let fragment_transform = Transform {
                position,
                rotation: crate::quat_to_euler_degrees(rotation),
                scale: transform.scale * fragment.scale,
            };

            // A shard carries its own geometry and collides with its own hull;
            // a mesh fragment is M14's box. Exactly one of the two, which
            // validation is what guarantees — a fragment with neither is
            // skipped rather than spawned as an invisible collider.
            let shard = fragment.points.as_ref().map(|points| Shard {
                points: points.clone(),
            });
            if shard.is_none() && fragment.mesh.is_none() {
                continue;
            }

            // Rigid-body kinematics: a point on a spinning parent moves at
            // v + ω x r, so fragments fly apart the way the parent moved.
            let mut velocity = linear + angular_radians.cross(position - transform.position);
            let mut spin = angular_degrees;

            // Mass, for the explosion's momentum kick: a shard's hull volume,
            // or the box M14 assumed. `scale` is a length per axis, so volume
            // scales by their product.
            let scaled = fragment_transform.scale;
            let volume = match &shard {
                Some(shard) => {
                    engine_core::shard::volume(&shard.points)
                        * (scaled.x * scaled.y * scaled.z).abs()
                }
                None => {
                    let extents = fragment.half_extents.unwrap_or(HALF_CUBE) * scaled;
                    8.0 * (extents.x * extents.y * extents.z).abs()
                }
            };
            let mass = fragment.density * volume;

            if let Some(explosion) = pending_break.kick {
                let delta = position - explosion.center;
                let distance = delta.length();
                if distance < explosion.radius {
                    let magnitude = explosion.impulse * (1.0 - distance / explosion.radius);
                    let direction = if distance > 1e-6 {
                        delta / distance
                    } else {
                        Vec3::Y
                    };
                    if mass > 0.0 {
                        velocity += direction * (magnitude / mass);
                    }
                }
            }

            // The material's scatter (M43): pieces thrown away from where the
            // thing was struck, at a speed the material chooses and the hit's
            // severity scales. Absent material, absent impact, or a scripted
            // break with no impact at all, and none of this runs — which is
            // what keeps every pre-M43 scene byte-identical.
            if let (Some(behaviour), Some(impact)) = (behaviour, pending_break.impact) {
                let jitter = jitter_of(i);
                let away = position - impact.point;
                let mut direction = if away.length() > 1e-4 {
                    away.normalize()
                } else {
                    // Struck dead on its own centroid: throw it up and out
                    // rather than dividing by nothing.
                    (Vec3::Y + jitter * 0.5).normalize_or(Vec3::Y)
                };
                // Nothing is thrown *downward*. A crate hit from above has
                // every fragment below the contact point, so the honest
                // "away from the impact" direction is into the floor — where
                // that half of a real break's energy does go, and where this
                // engine's version of it would only become penetration for
                // the solver to push back out. What a break shows is the
                // sideways half, so the vertical component is floored here.
                direction.y = direction.y.max(MIN_LIFT);
                let direction = direction.normalize();
                // The jitter is a quarter of the direction so a scatter still
                // reads as radial — this is spray, not randomness.
                let sprayed = (direction + jitter * 0.25).normalize_or(direction);
                let speed = behaviour.burst_speed * impact.severity;
                velocity += sprayed * speed;
                spin += jitter * (behaviour.spin * impact.severity);
            }

            let body = RigidBody {
                body: BodyKind::Dynamic,
                linear_velocity: velocity,
                angular_velocity: spin,
                gravity_scale: 1.0,
                linear_damping: 0.0,
                angular_damping: 0.0,
                ccd,
                can_sleep: true,
                locked_rotations: [false; 3],
            };
            let collider = Collider {
                shape: match shard {
                    Some(_) => ColliderShapeKind::ConvexHull,
                    None => ColliderShapeKind::Cuboid,
                },
                half_extents: shard
                    .is_none()
                    .then(|| fragment.half_extents.unwrap_or(HALF_CUBE)),
                radius: None,
                half_height: None,
                asset: None,
                friction: behaviour.map_or(0.5, |b| b.friction),
                restitution: behaviour.map_or(0.0, |b| b.restitution),
                density: fragment.density,
                sensor: false,
                offset: Vec3::ZERO,
                layers: None,
                collides_with: None,
            };

            let mut builder = EntityBuilder::new();
            builder.add(Name(fragment_name.clone()));
            builder.add(fragment_transform);
            match (&shard, &fragment.mesh) {
                (Some(shard), _) => builder.add(shard.clone()),
                (None, Some(asset)) => builder.add(Mesh {
                    asset: asset.clone(),
                }),
                (None, None) => unreachable!("a fragment with no geometry was skipped above"),
            };
            if let Some(material) = &material {
                builder.add(material.clone());
            }
            builder.add(body);
            builder.add(collider.clone());
            let spawned = world.spawn(builder.build());
            physics.insert_entity(
                spawned,
                &crate::Presence {
                    name: &fragment_name,
                    transform: &fragment_transform,
                    body: Some(&body),
                    collider: Some(&collider),
                    break_threshold: None,
                    // A mesh fragment's collider is a cuboid built from
                    // `fragment.half_extents` and carries no layers, so
                    // neither the mesh source nor the layer table can reach it
                    // — which is what makes M37 widening this call unable to
                    // move a single breaking baseline. A shard's comes from
                    // its own points, which is the one thing here that does
                    // need something passed in.
                    entity_mesh: None,
                    shard: shard.as_ref(),
                    meshes: &engine_core::mesh::BuiltinAssets,
                },
            )?;
            fragments.push((fragment_name, spawned));
        }

        // The burst (M44), after the fragments so its name cannot take one of
        // theirs. It is an entity like any other from the moment it spawns —
        // it just has no body, so nothing tells physics about it.
        let dust = match breakable.material.filter(|_| breakable.dust) {
            Some(material) => {
                let mut dust_name = format!("{name}.dust");
                let mut attempt = 2;
                while taken_names.contains(&dust_name) {
                    dust_name = format!("{name}.dust{attempt}");
                    attempt += 1;
                }
                taken_names.insert(dust_name.clone());

                // How big the thing that broke was, from the fragments it
                // became: the furthest one's offset is the object's radius,
                // and it needs no `Collider` to be there and no second
                // opinion about a size the file already states.
                let radius = breakable
                    .fragments
                    .iter()
                    .map(|fragment| (fragment.offset * transform.scale).length())
                    .fold(0.0f32, f32::max);

                let mut emitter = material.dust(radius);
                // Seeded from the entity's own name, so two crates of the same
                // material do not throw the identical puff, and re-running the
                // same scene throws the same one.
                emitter.seed = name_seed(&name);

                // **Outside the surface that was struck, not at the contact
                // point.** A contact point sits *on* the object, so a burst
                // born there is inside the silhouette of a thing that has not
                // come apart yet, and every particle of it is depth-rejected
                // by the very geometry it is supposed to be coming off — a
                // puff that the sim produces, the renderer receives, and the
                // frame does not show. Pushing it out along the face normal
                // (which for a contact is the direction from the centre) is
                // both what makes it visible and what a break actually does:
                // dust is ejected *away* from the impact face.
                let position = match pending_break.impact {
                    Some(impact) => {
                        let outward = (impact.point - transform.position)
                            .normalize_or(Vec3::Y);
                        impact.point + outward * (radius * 0.45)
                    }
                    // A scripted break has no impact, so the burst is the
                    // whole object's: centred on it, and `radius` spreads it.
                    None => transform.position,
                };
                let dust_transform = Transform {
                    position,
                    rotation: Vec3::ZERO,
                    scale: Vec3::ONE,
                };

                let mut builder = EntityBuilder::new();
                builder.add(Name(dust_name.clone()));
                builder.add(dust_transform);
                builder.add(emitter);
                Some((dust_name, world.spawn(builder.build())))
            }
            None => None,
        };

        events.push(BreakEvent {
            entity: name,
            fragments,
            dust,
        });
    }

    Ok(events)
}

/// The least upward a scattered fragment may be thrown, as a fraction of its
/// scatter speed — see where it is applied.
const MIN_LIFT: f32 = 0.2;

/// A seed from an entity's name (M44), so a break's dust is that break's and
/// re-running the scene throws the same one.
///
/// FNV-1a, written out here for the reason every other generator in this repo
/// writes its own out: the sequence is part of what a run *means*, and a
/// `DefaultHasher` is explicitly allowed to change between Rust releases.
fn name_seed(name: &str) -> u32 {
    let mut hash: u32 = 0x811C_9DC5;
    for byte in name.as_bytes() {
        hash ^= *byte as u32;
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

/// A fragment's own spray direction, in `[-1, 1]` per axis (M43).
///
/// A hash of the fragment's index rather than a running RNG, deliberately: it
/// has no state to carry across a break, it does not care what order the
/// fragments were visited in, and fragment 3 of a crate gets the same spray
/// however many crates broke first. The constants are the usual xorshift
/// scramble, written out here for the reason `particles.rs` and `tree.rs`
/// write theirs out — the sequence is part of what a scene *means*.
fn jitter_of(index: usize) -> Vec3 {
    let mut state = (index as u32).wrapping_mul(0x9E37_79B9) | 1;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        // [0, 1) → [-1, 1)
        (state as f32 / u32::MAX as f32) * 2.0 - 1.0
    };
    Vec3::new(next(), next(), next())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Explosion;
    use engine_core::scene::PhysicsSettings;
    use engine_core::Scene;

    /// A static breakable crate on the ground, a heavy ball above it.
    const CRATE_DROP: &str = r#"{
      "name": "break",
      "entities": [
        {"name": "Ground", "components": [
          {"type": "Transform"},
          {"type": "Collider", "shape": "cuboid", "half_extents": [10.0, 0.05, 10.0]}
        ]},
        {"name": "Crate", "components": [
          {"type": "Transform", "position": [0.0, 0.55, 0.0]},
          {"type": "Mesh", "asset": "builtin:cube"},
          {"type": "Material", "albedo": [0.8, 0.5, 0.2]},
          {"type": "Collider", "shape": "cuboid", "half_extents": [0.5, 0.5, 0.5]},
          {"type": "Breakable", "impulse_threshold": 5.0, "fragments": [
            {"mesh": "builtin:cube", "offset": [-0.25, -0.25, 0.0],
             "scale": [0.5, 0.5, 0.5], "half_extents": [0.25, 0.25, 0.25]},
            {"mesh": "builtin:cube", "offset": [0.25, 0.25, 0.0],
             "scale": [0.5, 0.5, 0.5], "half_extents": [0.25, 0.25, 0.25]}
          ]}
        ]},
        {"name": "Ball", "components": [
          {"type": "Transform", "position": [0.0, 5.0, 0.0]},
          {"type": "RigidBody", "body": "dynamic"},
          {"type": "Collider", "shape": "sphere", "radius": 0.4, "density": 20.0}
        ]}
      ]
    }"#;

    fn scene(source: &str) -> (Scene, PhysicsWorld) {
        let mut scene = Scene::from_source(source, "test.json").unwrap();
        let physics = PhysicsWorld::build(
            &scene.world,
            &PhysicsSettings::default(),
            &engine_core::mesh::BuiltinAssets,
            &scene.templates,
        )
        .unwrap();
        let _ = &mut scene;
        (scene, physics)
    }

    fn step_and_break(
        scene: &mut Scene,
        physics: &mut PhysicsWorld,
        steps: u32,
    ) -> Vec<BreakEvent> {
        let mut events = Vec::new();
        for _ in 0..steps {
            physics.step(&mut scene.world, 0.0);
            events.extend(apply_breaks(&mut scene.world, physics, &[]).unwrap());
        }
        events
    }

    fn find(scene: &Scene, name: &str) -> Option<Entity> {
        scene
            .world
            .query::<(Entity, &Name)>()
            .iter()
            .find(|(_, n)| n.0 == name)
            .map(|(entity, _)| entity)
    }

    #[test]
    fn a_hard_impact_breaks_the_crate_into_fragments() {
        let (mut scene, mut physics) = scene(CRATE_DROP);
        let events = step_and_break(&mut scene, &mut physics, 300);

        assert_eq!(events.len(), 1, "exactly one break: {events:?}");
        assert_eq!(events[0].entity, "Crate");
        assert_eq!(
            events[0]
                .fragments
                .iter()
                .map(|(n, _)| n.as_str())
                .collect::<Vec<_>>(),
            ["Crate.frag0", "Crate.frag1"]
        );

        assert!(find(&scene, "Crate").is_none(), "the crate is gone");
        for (name, _) in &events[0].fragments {
            let entity = find(&scene, name).expect("fragment exists");
            let body = scene.world.get::<&RigidBody>(entity).unwrap();
            assert_eq!(body.body, BodyKind::Dynamic);
            let material = scene.world.get::<&Material>(entity).unwrap();
            assert_eq!(
                material.albedo,
                Vec3::new(0.8, 0.5, 0.2),
                "parent material copied"
            );
            let transform = scene.world.get::<&Transform>(entity).unwrap();
            assert!(
                transform.position.y > 0.0 && transform.position.y < 1.5,
                "fragments settle near the ground: {:?}",
                transform.position
            );
        }
    }

    #[test]
    fn a_soft_touch_stays_whole() {
        let gentle = CRATE_DROP
            .replace(
                "\"impulse_threshold\": 5.0",
                "\"impulse_threshold\": 10000.0",
            )
            .replace("[0.0, 5.0, 0.0]", "[0.0, 1.6, 0.0]");
        let (mut scene, mut physics) = scene(&gentle);
        let events = step_and_break(&mut scene, &mut physics, 300);
        assert!(
            events.is_empty(),
            "nothing reaches 10000 kg·m/s: {events:?}"
        );
        assert!(find(&scene, "Crate").is_some());
    }

    #[test]
    fn forced_breaks_inherit_the_parent_velocity() {
        let source = CRATE_DROP.replace(
            r#"{"type": "Collider", "shape": "cuboid", "half_extents": [0.5, 0.5, 0.5]},"#,
            r#"{"type": "RigidBody", "body": "dynamic", "linear_velocity": [3.0, 0.0, 0.0]},
               {"type": "Collider", "shape": "cuboid", "half_extents": [0.5, 0.5, 0.5]},"#,
        );
        let (mut scene, mut physics) = scene(&source);
        let events = apply_breaks(&mut scene.world, &mut physics, &["Crate".to_string()]).unwrap();
        assert_eq!(events.len(), 1);
        for (name, _) in &events[0].fragments {
            let entity = find(&scene, name).unwrap();
            let body = scene.world.get::<&RigidBody>(entity).unwrap();
            assert_eq!(
                body.linear_velocity,
                Vec3::new(3.0, 0.0, 0.0),
                "{name} inherits the parent's motion"
            );
        }
    }

    #[test]
    fn a_forced_break_of_a_non_breakable_is_skipped() {
        let (mut scene, mut physics) = scene(CRATE_DROP);
        let events = apply_breaks(&mut scene.world, &mut physics, &["Ball".to_string()]).unwrap();
        assert!(events.is_empty());
        assert!(find(&scene, "Ball").is_some());
    }

    #[test]
    fn an_explosion_breaks_and_kicks_fragments_outward() {
        let (mut scene, mut physics) = scene(CRATE_DROP);
        // Blast center south of the crate: fragments must fly north (−Z).
        physics.queue_explosion(Explosion {
            center: Vec3::new(0.0, 0.55, 2.0),
            radius: 5.0,
            impulse: 50.0,
        });
        physics.step(&mut scene.world, 0.0);
        let events = apply_breaks(&mut scene.world, &mut physics, &[]).unwrap();

        assert_eq!(events.len(), 1, "the blast breaks the crate: {events:?}");
        assert_eq!(events[0].entity, "Crate");
        for (name, _) in &events[0].fragments {
            let entity = find(&scene, name).unwrap();
            let body = scene.world.get::<&RigidBody>(entity).unwrap();
            assert!(
                body.linear_velocity.z < -1.0,
                "{name} should be kicked away from the blast: {:?}",
                body.linear_velocity
            );
        }

        // The dynamic ball in range was pushed upward and away too.
        let ball = find(&scene, "Ball").unwrap();
        let ball_velocity = scene.world.get::<&RigidBody>(ball).unwrap().linear_velocity;
        // Near the blast's edge the falloff makes the push small; the sign
        // (away from the center, enough lift to beat one step of gravity)
        // is the property under test.
        assert!(
            ball_velocity.z < -0.05 && ball_velocity.y > -0.1,
            "the blast pushes the ball up and away: {ball_velocity:?}"
        );
    }

    #[test]
    fn fragment_names_dodge_authored_entities() {
        let source = CRATE_DROP.replace(
            r#"{"name": "Ball", "components": ["#,
            r#"{"name": "Crate.frag0", "components": [{"type": "Transform"}]},
               {"name": "Ball", "components": ["#,
        );
        let (mut scene, mut physics) = scene(&source);
        let events = apply_breaks(&mut scene.world, &mut physics, &["Crate".to_string()]).unwrap();
        assert_eq!(
            events[0]
                .fragments
                .iter()
                .map(|(n, _)| n.as_str())
                .collect::<Vec<_>>(),
            ["Crate.frag0_2", "Crate.frag1"],
            "a taken name gets a deterministic suffix"
        );
    }

    #[test]
    fn breaking_is_deterministic() {
        let run = || {
            let (mut scene, mut physics) = scene(CRATE_DROP);
            step_and_break(&mut scene, &mut physics, 300);
            let mut positions: Vec<(String, [f32; 3])> = scene
                .world
                .query::<(&Name, &Transform)>()
                .iter()
                .map(|(name, transform)| (name.0.clone(), transform.position.to_array()))
                .collect();
            positions.sort_by(|a, b| a.0.cmp(&b.0));
            positions
        };
        assert_eq!(run(), run());
    }

    /// A fragment is thrown, and it is smaller than the thing it came off,
    /// which makes it the body most likely to cross a `trimesh` terrain
    /// between two steps. It inherits the parent's `ccd` rather than being
    /// fixed off, so a scene that needs it can say so — and one that does not
    /// mention it is byte-identical to every break before this.
    #[test]
    fn a_fragment_inherits_the_ccd_of_what_it_broke_off() {
        let ccd_of = |body: &str| -> Vec<bool> {
            let source = CRATE_DROP.replace(
                r#"{"type": "Collider", "shape": "cuboid", "half_extents": [0.5, 0.5, 0.5]},"#,
                &format!(
                    r#"{body}{{"type": "Collider", "shape": "cuboid", "half_extents": [0.5, 0.5, 0.5]}},"#
                ),
            );
            let (mut scene, mut physics) = scene(&source);
            let events =
                apply_breaks(&mut scene.world, &mut physics, &["Crate".to_string()]).unwrap();
            events[0]
                .fragments
                .iter()
                .map(|(_, entity)| scene.world.get::<&RigidBody>(*entity).unwrap().ccd)
                .collect()
        };

        assert_eq!(
            ccd_of(r#"{"type": "RigidBody", "body": "dynamic", "ccd": true},"#),
            [true, true],
            "a parent that asks for continuous collision passes it to its pieces"
        );
        assert_eq!(
            ccd_of(r#"{"type": "RigidBody", "body": "dynamic"},"#),
            [false, false],
            "and the default stays off, which is every scene that predates it"
        );
        assert_eq!(
            ccd_of(""),
            [false, false],
            "a parent with no body at all has no ccd to inherit"
        );
    }

    // ── Material-aware fracture (M43) ──────────────────────────────────

    /// A tetrahedral shard, as `engine fracture` would write one.
    const SHARD_POINTS: &str = "[[-0.25,-0.25,-0.25],[0.25,-0.25,-0.25],\
                                [0.0,0.25,-0.25],[0.0,0.0,0.25]]";

    /// The crate, but its fragments are shards and it knows what it is made of.
    fn shard_crate(material: &str) -> String {
        CRATE_DROP.replace(
            r#""impulse_threshold": 5.0, "fragments": [
            {"mesh": "builtin:cube", "offset": [-0.25, -0.25, 0.0],
             "scale": [0.5, 0.5, 0.5], "half_extents": [0.25, 0.25, 0.25]},
            {"mesh": "builtin:cube", "offset": [0.25, 0.25, 0.0],
             "scale": [0.5, 0.5, 0.5], "half_extents": [0.25, 0.25, 0.25]}
          ]"#,
            &format!(
                r#""impulse_threshold": 5.0, "material": "{material}", "fragments": [
            {{"points": {SHARD_POINTS}, "offset": [-0.25, -0.25, 0.0]}},
            {{"points": {SHARD_POINTS}, "offset": [0.25, 0.25, 0.0]}}
          ]"#
            ),
        )
    }

    #[test]
    fn a_shard_fragment_spawns_with_its_own_hull() {
        let source = shard_crate("stone");
        let (mut scene, mut physics) = scene(&source);
        let events = apply_breaks(&mut scene.world, &mut physics, &["Crate".to_string()]).unwrap();

        assert_eq!(events[0].fragments.len(), 2);
        for (name, entity) in &events[0].fragments {
            let shard = scene
                .world
                .get::<&Shard>(*entity)
                .unwrap_or_else(|_| panic!("{name} carries its geometry"));
            assert_eq!(shard.points.len(), 4, "the authored points, verbatim");
            assert!(
                scene.world.get::<&Mesh>(*entity).is_err(),
                "{name} owns its geometry, so it has no Mesh"
            );

            let collider = scene.world.get::<&Collider>(*entity).unwrap();
            assert_eq!(collider.shape, ColliderShapeKind::ConvexHull);
            assert_eq!(
                collider.half_extents, None,
                "a shard's collider is its hull, not a box around it"
            );
        }

        // And physics agrees: the hull reached rapier, so the shard is a
        // convex polyhedron in the world rather than a missing collider.
        let shapes: Vec<&str> = physics
            .collider_report()
            .into_iter()
            .filter(|c| c.entity.starts_with("Crate.frag"))
            .map(|c| c.shape)
            .collect();
        assert_eq!(shapes, ["convex_hull", "convex_hull"], "{shapes:?}");
    }

    #[test]
    fn a_material_throws_its_fragments_away_from_the_impact() {
        // The ball lands on top, so the pieces must come off it: the scatter
        // is aimed from the contact point, not from the parent's centre.
        let (mut scene, mut physics) = scene(&shard_crate("glass"));
        let mut events = Vec::new();
        for _ in 0..300 {
            physics.step(&mut scene.world, 0.0);
            events.extend(apply_breaks(&mut scene.world, &mut physics, &[]).unwrap());
            if !events.is_empty() {
                break;
            }
        }
        assert_eq!(events.len(), 1, "the ball breaks the crate: {events:?}");

        let mut thrown = 0;
        for (name, entity) in &events[0].fragments {
            let body = scene.world.get::<&RigidBody>(*entity).unwrap();
            assert!(
                body.linear_velocity.length() > 1.0,
                "{name} should have been thrown, not dropped: {:?}",
                body.linear_velocity
            );
            assert!(
                body.angular_velocity.length() > 1.0,
                "{name} should be tumbling: {:?}",
                body.angular_velocity
            );
            assert!(
                body.linear_velocity.y > 0.0,
                "{name} was thrown into the floor: {:?}",
                body.linear_velocity
            );
            thrown += 1;
        }
        assert_eq!(thrown, 2, "both pieces came off the hit");
    }

    #[test]
    fn a_material_is_the_only_thing_that_scatters() {
        // The M14 path, untouched: without a material the fragments take the
        // parent's motion and nothing else. A static crate's is zero, so this
        // is the sharpest possible statement of "pre-M43 scenes are
        // byte-identical".
        let (mut scene, mut physics) = scene(CRATE_DROP);
        let events = apply_breaks(&mut scene.world, &mut physics, &["Crate".to_string()]).unwrap();
        for (name, entity) in &events[0].fragments {
            let body = scene.world.get::<&RigidBody>(*entity).unwrap();
            assert_eq!(body.linear_velocity, Vec3::ZERO, "{name} was not thrown");
            assert_eq!(body.angular_velocity, Vec3::ZERO, "{name} is not spinning");
            let collider = scene.world.get::<&Collider>(*entity).unwrap();
            assert_eq!(collider.friction, 0.5, "M14's fragment surface");
            assert_eq!(collider.restitution, 0.0);
        }
    }

    #[test]
    fn a_scripted_break_has_nothing_to_scatter_from() {
        // `world.break_entity` names no impact point, and inventing one would
        // put energy into the scene that nothing in the file accounts for.
        let (mut scene, mut physics) = scene(&shard_crate("glass"));
        let events = apply_breaks(&mut scene.world, &mut physics, &["Crate".to_string()]).unwrap();
        for (name, entity) in &events[0].fragments {
            let body = scene.world.get::<&RigidBody>(*entity).unwrap();
            assert_eq!(
                body.linear_velocity,
                Vec3::ZERO,
                "{name} moved without an impact"
            );
        }
    }

    #[test]
    fn each_material_lays_its_own_surface_on_the_pieces() {
        for (material, friction, restitution) in [
            ("glass", 0.2, 0.1),
            ("wood", 0.6, 0.05),
            ("stone", 0.8, 0.0),
            ("metal", 0.4, 0.02),
        ] {
            let (mut scene, mut physics) = scene(&shard_crate(material));
            let events =
                apply_breaks(&mut scene.world, &mut physics, &["Crate".to_string()]).unwrap();
            let (_, entity) = events[0].fragments[0];
            let collider = scene.world.get::<&Collider>(entity).unwrap();
            assert_eq!(collider.friction, friction, "{material} friction");
            assert_eq!(collider.restitution, restitution, "{material} restitution");
        }
    }

    #[test]
    fn glass_throws_its_shards_further_than_stone_drops_them() {
        // The claim the whole material model rests on, measured rather than
        // asserted in a comment.
        let speed_of = |material: &str| {
            let (mut scene, mut physics) = scene(&shard_crate(material));
            let mut events = Vec::new();
            for _ in 0..300 {
                physics.step(&mut scene.world, 0.0);
                events.extend(apply_breaks(&mut scene.world, &mut physics, &[]).unwrap());
                if !events.is_empty() {
                    break;
                }
            }
            let speeds: Vec<f32> = events[0]
                .fragments
                .iter()
                .map(|(_, e)| {
                    scene
                        .world
                        .get::<&RigidBody>(*e)
                        .unwrap()
                        .linear_velocity
                        .length()
                })
                .collect();
            speeds.iter().sum::<f32>() / speeds.len() as f32
        };
        let glass = speed_of("glass");
        let stone = speed_of("stone");
        assert!(
            glass > stone * 3.0,
            "glass left at {glass} m/s and stone at {stone} m/s"
        );
    }

    // ── The break's dust (M44) ─────────────────────────────────────────

    #[test]
    fn a_material_throws_a_burst_that_dies_on_its_own() {
        let (mut scene, mut physics) = scene(&shard_crate("stone"));
        let events = apply_breaks(&mut scene.world, &mut physics, &["Crate".to_string()]).unwrap();

        let (dust_name, dust) = events[0].dust.clone().expect("stone puffs");
        assert_eq!(dust_name, "Crate.dust");
        let emitter = scene
            .world
            .get::<&engine_core::components::ParticleEmitter>(dust)
            .expect("the burst is an emitter");
        assert!(emitter.duration.is_some(), "a burst is not a fountain");
        assert!(emitter.despawn_when_done, "and it clears up after itself");
        // No body, no collider: a puff is not a fragment, and physics must
        // never have been told about it.
        assert!(scene.world.get::<&RigidBody>(dust).is_err());
        assert!(scene.world.get::<&Collider>(dust).is_err());
        drop(emitter);

        // It emits, then stops, then goes.
        let mut particles = engine_core::particles::ParticleSystem::build(&scene.world);
        let dt = 1.0 / 60.0;
        for _ in 0..8 {
            particles.step(&scene.world, dt);
        }
        let peak = particles.live_particles();
        assert!(peak > 0, "the burst produced nothing");
        assert!(
            particles.finished(&scene.world).is_empty(),
            "it is not done while it still has particles"
        );

        // Long enough for every particle to live out its lifetime.
        for _ in 0..200 {
            particles.step(&scene.world, dt);
        }
        assert_eq!(particles.live_particles(), 0, "every particle died");
        assert_eq!(
            particles.finished(&scene.world),
            vec![dust],
            "and the spent emitter asks to be reaped"
        );
    }

    #[test]
    fn dust_can_be_turned_off_and_needs_a_material() {
        // Off by request.
        let quiet = shard_crate("stone").replace(
            r#""material": "stone""#,
            r#""material": "stone", "dust": false"#,
        );
        let (mut quiet_scene, mut quiet_physics) = scene(&quiet);
        let events = apply_breaks(
            &mut quiet_scene.world,
            &mut quiet_physics,
            &["Crate".to_string()],
        )
        .unwrap();
        assert!(events[0].dust.is_none(), "asked for no dust, got none");

        // And there is no generic dust: a `Breakable` with no material is the
        // M14 path start to finish, which is what keeps every pre-M43 scene
        // rendering as it did.
        let (mut plain, mut plain_physics) = scene(CRATE_DROP);
        let events = apply_breaks(
            &mut plain.world,
            &mut plain_physics,
            &["Crate".to_string()],
        )
        .unwrap();
        assert!(events[0].dust.is_none(), "no material, no dust");
    }

    #[test]
    fn each_material_throws_its_own_burst() {
        // Not one puff recoloured: the four differ in what a viewer would
        // actually name them by — how long they last, how fast they leave,
        // and whether they are lit.
        use engine_core::components::ParticleBlend;
        let of = |material: &str| {
            let (mut scene, mut physics) = scene(&shard_crate(material));
            let events =
                apply_breaks(&mut scene.world, &mut physics, &["Crate".to_string()]).unwrap();
            let (_, dust) = events[0].dust.clone().expect("puffs");
            let emitter = *scene
                .world
                .get::<&engine_core::components::ParticleEmitter>(dust)
                .unwrap();
            emitter
        };
        let (stone, wood, glass, metal) = (of("stone"), of("wood"), of("glass"), of("metal"));

        assert!(
            stone.lifetime > glass.lifetime * 2.0,
            "rock dust hangs and glitter does not: {} vs {}",
            stone.lifetime,
            glass.lifetime
        );
        assert!(
            metal.speed > stone.speed * 2.0,
            "sparks fly and dust drifts: {} vs {}",
            metal.speed,
            stone.speed
        );
        assert!(stone.acceleration.y > 0.0, "dust rises");
        assert!(wood.acceleration.y < 0.0, "sawdust falls");
        assert_eq!(glass.blend, ParticleBlend::Additive, "glitter catches light");
        assert_eq!(metal.blend, ParticleBlend::Additive, "so do sparks");
        assert_eq!(stone.blend, ParticleBlend::Alpha, "dust occludes");
    }

    #[test]
    fn the_burst_is_seeded_by_the_entity_that_broke() {
        // Two identical crates must not throw the identical puff, and the
        // same crate must throw the same one every run.
        let source = shard_crate("stone").replace(
            r#"{"name": "Ball", "components": ["#,
            r#"{"name": "Crate2", "components": [
              {"type": "Transform", "position": [3.0, 0.55, 0.0]},
              {"type": "Collider", "shape": "cuboid", "half_extents": [0.5, 0.5, 0.5]},
              {"type": "Breakable", "material": "stone", "fragments": [
                {"points": [[-0.25,-0.25,-0.25],[0.25,-0.25,-0.25],
                            [0.0,0.25,-0.25],[0.0,0.0,0.25]]}
              ]}
            ]},
            {"name": "Ball", "components": ["#,
        );
        let seeds = || {
            let (mut scene, mut physics) = scene(&source);
            let events = apply_breaks(
                &mut scene.world,
                &mut physics,
                &["Crate".to_string(), "Crate2".to_string()],
            )
            .unwrap();
            events
                .iter()
                .filter_map(|event| event.dust.as_ref())
                .map(|(_, dust)| {
                    scene
                        .world
                        .get::<&engine_core::components::ParticleEmitter>(*dust)
                        .unwrap()
                        .seed
                })
                .collect::<Vec<u32>>()
        };
        let first = seeds();
        assert_eq!(first.len(), 2);
        assert_ne!(first[0], first[1], "two crates, two puffs");
        assert_eq!(first, seeds(), "and the same run throws the same ones");
    }

    #[test]
    fn the_burst_comes_off_the_struck_face() {
        // Outside the surface, not at the contact point. A burst born *on*
        // the object is inside the silhouette of something that has not come
        // apart yet, and every particle of it is depth-rejected by the
        // geometry it is supposed to be coming off — the sim produces it, the
        // renderer receives it, and the frame does not show it. Measured, so
        // the fix cannot be undone by a refactor that looks equivalent.
        let (mut scene, mut physics) = scene(&shard_crate("stone"));
        let mut events = Vec::new();
        for _ in 0..300 {
            physics.step(&mut scene.world, 0.0);
            events.extend(apply_breaks(&mut scene.world, &mut physics, &[]).unwrap());
            if !events.is_empty() {
                break;
            }
        }
        let (_, dust) = events[0].dust.clone().expect("puffs");
        let at = scene.world.get::<&Transform>(dust).unwrap().position;
        // The ball falls onto the top of the crate, whose centre is y = 0.55
        // and whose top is y = 1.05.
        assert!(
            at.y > 1.05,
            "the burst should sit clear of the face it came off: {at:?}"
        );
    }

    #[test]
    fn a_shard_break_is_deterministic() {
        let run = || {
            let (mut scene, mut physics) = scene(&shard_crate("wood"));
            step_and_break(&mut scene, &mut physics, 300);
            let mut positions: Vec<(String, [f32; 3])> = scene
                .world
                .query::<(&Name, &Transform)>()
                .iter()
                .map(|(name, transform)| (name.0.clone(), transform.position.to_array()))
                .collect();
            positions.sort_by(|a, b| a.0.cmp(&b.0));
            positions
        };
        assert_eq!(run(), run());
    }
}
