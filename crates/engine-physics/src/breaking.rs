//! Break application (M14): swapping a `Breakable` entity for its fragments.
//!
//! Runs once per fixed step, after physics — the caller collects the step's
//! break decisions ([`PhysicsWorld::take_pending_breaks`]) plus any
//! script-forced names and applies them here, in entity-name order. Fragments
//! are ordinary entities from the moment they spawn: they render, trace, and
//! bake like anything else; nothing downstream has a "debris" case.

use std::collections::HashSet;

use engine_core::components::{
    BodyKind, Breakable, Collider, ColliderShapeKind, Material, Mesh, Name, RigidBody, Transform,
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
                    pending.push(PendingBreak { entity, kick: None });
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
        let (linear, angular_degrees) = world
            .get::<&RigidBody>(entity)
            .map(|b| (b.linear_velocity, b.angular_velocity))
            .unwrap_or((Vec3::ZERO, Vec3::ZERO));

        let _ = world.despawn(entity);
        physics.remove_entity(entity);
        taken_names.remove(&name);

        let parent_rotation = transform.quat();
        let angular_radians = angular_degrees * (std::f32::consts::PI / 180.0);

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

            // Rigid-body kinematics: a point on a spinning parent moves at
            // v + ω x r, so fragments fly apart the way the parent moved.
            let mut velocity = linear + angular_radians.cross(position - transform.position);
            if let Some(explosion) = pending_break.kick {
                let delta = position - explosion.center;
                let distance = delta.length();
                if distance < explosion.radius {
                    let magnitude = explosion.impulse * (1.0 - distance / explosion.radius);
                    let extents = fragment.half_extents * fragment_transform.scale;
                    let mass = fragment.density * 8.0 * (extents.x * extents.y * extents.z).abs();
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

            let body = RigidBody {
                body: BodyKind::Dynamic,
                linear_velocity: velocity,
                angular_velocity: angular_degrees,
                gravity_scale: 1.0,
                linear_damping: 0.0,
                angular_damping: 0.0,
                ccd: false,
                can_sleep: true,
                locked_rotations: [false; 3],
            };
            let collider = Collider {
                shape: ColliderShapeKind::Cuboid,
                half_extents: Some(fragment.half_extents),
                radius: None,
                half_height: None,
                asset: None,
                friction: 0.5,
                restitution: 0.0,
                density: fragment.density,
                sensor: false,
                offset: Vec3::ZERO,
                layers: None,
                collides_with: None,
            };

            let mut builder = EntityBuilder::new();
            builder.add(Name(fragment_name.clone()));
            builder.add(fragment_transform);
            builder.add(Mesh {
                asset: fragment.mesh.clone(),
            });
            if let Some(material) = &material {
                builder.add(material.clone());
            }
            builder.add(body);
            builder.add(collider.clone());
            let spawned = world.spawn(builder.build());
            physics.insert_entity(
                spawned,
                &fragment_name,
                &fragment_transform,
                Some(&body),
                Some(&collider),
                None,
            )?;
            fragments.push((fragment_name, spawned));
        }

        events.push(BreakEvent {
            entity: name,
            fragments,
        });
    }

    Ok(events)
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
            physics.step(&mut scene.world);
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
        physics.step(&mut scene.world);
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
}
