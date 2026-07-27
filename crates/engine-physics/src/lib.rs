//! Rigid-body simulation (M8): the rapier wrapper.
//!
//! Everything temporal lives here. The contract is determinism (design §2):
//! the world is built fresh from the scene for every run, `dt` comes from an
//! integer `timestep_hz`, time advances only in explicit fixed steps, and no
//! clock is ever read. rapier is an implementation detail — no rapier type
//! crosses this crate's boundary; scene-facing math is glam (which rapier
//! 0.34's glamx backend shares, same version, so poses convert freely),
//! angles are Euler XYZ **degrees** (the settled file convention).
//!
//! Simulation state is derived, never authoritative: solver internals die
//! with this struct, and anything worth keeping is written back into hecs
//! components, which `--bake` turns into ordinary scene text.

use std::collections::HashMap;
use std::sync::Mutex;

use engine_core::components::{
    BodyKind, Collider as ColliderData, ColliderShapeKind, Name, RigidBody as RigidBodyData,
    Transform,
};
use engine_core::scene::PhysicsSettings;
use engine_core::{codes, EngineError, Result};
use glam::{Quat, Vec3};
use hecs::{Entity, World};
use rapier3d::math::Pose;
use rapier3d::parry::query::DefaultQueryDispatcher;
use rapier3d::prelude::*;

/// One contact begin/end between two named entities — what traces record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContactEvent {
    pub a: String,
    pub b: String,
    pub started: bool,
}

/// A raycast hit, in scene terms.
#[derive(Debug, Clone, PartialEq)]
pub struct RayHit {
    pub entity: String,
    pub point: Vec3,
    pub normal: Vec3,
    pub distance: f32,
}

/// Collects rapier collision events; drained after each step.
#[derive(Default)]
struct EventSink {
    collisions: Mutex<Vec<CollisionEvent>>,
}

impl EventHandler for EventSink {
    fn handle_collision_event(
        &self,
        _bodies: &RigidBodySet,
        _colliders: &ColliderSet,
        event: CollisionEvent,
        _contact_pair: Option<&ContactPair>,
    ) {
        if let Ok(mut collisions) = self.collisions.lock() {
            collisions.push(event);
        }
    }

    fn handle_contact_force_event(
        &self,
        _dt: Real,
        _bodies: &RigidBodySet,
        _colliders: &ColliderSet,
        _contact_pair: &ContactPair,
        _total_force_magnitude: Real,
    ) {
    }
}

pub struct PhysicsWorld {
    pipeline: PhysicsPipeline,
    parameters: IntegrationParameters,
    gravity: Vec3,
    islands: IslandManager,
    broad_phase: BroadPhaseBvh,
    narrow_phase: NarrowPhase,
    bodies: RigidBodySet,
    colliders: ColliderSet,
    impulse_joints: ImpulseJointSet,
    multibody_joints: MultibodyJointSet,
    ccd: CCDSolver,
    events: EventSink,

    /// hecs entity → rapier body, for entities with a `RigidBody` component.
    body_of: HashMap<Entity, RigidBodyHandle>,
    /// Any collider → the entity it came from (bodies and static geometry).
    entity_of_collider: HashMap<ColliderHandle, Entity>,
    /// Entity → stable name, resolved once at build.
    name_of: HashMap<Entity, String>,
}

impl PhysicsWorld {
    /// Whether a world contains any physics component at all — scenes
    /// without physics never construct a physics world.
    pub fn scene_has_physics(world: &World) -> bool {
        world.query::<&RigidBodyData>().iter().next().is_some()
            || world.query::<&ColliderData>().iter().next().is_some()
    }

    /// Build a fresh physics world from the (already validated) scene world.
    pub fn build(world: &World, settings: &PhysicsSettings) -> Result<Self> {
        let mut physics = Self {
            pipeline: PhysicsPipeline::new(),
            parameters: IntegrationParameters {
                dt: 1.0 / settings.timestep_hz.max(1) as Real,
                ..IntegrationParameters::default()
            },
            gravity: settings.gravity,
            islands: IslandManager::new(),
            broad_phase: BroadPhaseBvh::new(),
            narrow_phase: NarrowPhase::new(),
            bodies: RigidBodySet::new(),
            colliders: ColliderSet::new(),
            impulse_joints: ImpulseJointSet::new(),
            multibody_joints: MultibodyJointSet::new(),
            ccd: CCDSolver::new(),
            events: EventSink::default(),
            body_of: HashMap::new(),
            entity_of_collider: HashMap::new(),
            name_of: HashMap::new(),
        };

        // Deterministic build order: hecs iteration order is stable for a
        // freshly spawned world, and every simulate run spawns fresh.
        for (entity, name, transform, body, collider) in world
            .query::<(
                Entity,
                &Name,
                &Transform,
                Option<&RigidBodyData>,
                Option<&ColliderData>,
            )>()
            .iter()
        {
            if body.is_none() && collider.is_none() {
                continue;
            }
            physics.name_of.insert(entity, name.0.clone());

            let position = pose_of(transform);

            let body_handle = body.map(|body| {
                let builder = match body.body {
                    BodyKind::Dynamic => RigidBodyBuilder::dynamic(),
                    BodyKind::Kinematic => RigidBodyBuilder::kinematic_position_based(),
                    BodyKind::Fixed => RigidBodyBuilder::fixed(),
                };
                let built = builder
                    .pose(position)
                    .linvel(body.linear_velocity)
                    .angvel(degrees_to_radians(body.angular_velocity))
                    .gravity_scale(body.gravity_scale)
                    .linear_damping(body.linear_damping)
                    .angular_damping(body.angular_damping)
                    .ccd_enabled(body.ccd)
                    .can_sleep(body.can_sleep)
                    .build();
                let handle = physics.bodies.insert(built);
                physics.body_of.insert(entity, handle);
                handle
            });

            if let Some(collider) = collider {
                let built = build_collider(collider, transform, &physics.name_of[&entity])?;
                let handle = match body_handle {
                    Some(body_handle) => physics.colliders.insert_with_parent(
                        built,
                        body_handle,
                        &mut physics.bodies,
                    ),
                    None => {
                        // Static geometry: place the collider in world space
                        // directly; no body needed.
                        let mut built = built;
                        built.set_position(position * built.position());
                        physics.colliders.insert(built)
                    }
                };
                physics.entity_of_collider.insert(handle, entity);
            }
        }

        Ok(physics)
    }

    /// Advance one fixed step and write the results back into hecs. Returns
    /// the contact events the step produced, in deterministic order.
    pub fn step(&mut self, world: &mut World) -> Vec<ContactEvent> {
        // 1. Kinematic bodies follow whatever the world says their
        //    Transform is now.
        for (&entity, &handle) in &self.body_of {
            let Some(body) = self.bodies.get_mut(handle) else {
                continue;
            };
            if body.is_kinematic() {
                if let Ok(transform) = world.get::<&Transform>(entity) {
                    body.set_next_kinematic_position(pose_of(&transform));
                }
            }
        }

        // 2. One fixed step.
        self.pipeline.step(
            self.gravity,
            &self.parameters,
            &mut self.islands,
            &mut self.broad_phase,
            &mut self.narrow_phase,
            &mut self.bodies,
            &mut self.colliders,
            &mut self.impulse_joints,
            &mut self.multibody_joints,
            &mut self.ccd,
            &(),
            &self.events,
        );

        // 3. Write back into hecs for dynamic bodies: the scene components
        //    are the only state anyone else ever sees.
        for (&entity, &handle) in &self.body_of {
            let Some(body) = self.bodies.get(handle) else {
                continue;
            };
            if !body.is_dynamic() {
                continue;
            }

            if let Ok(mut transform) = world.get::<&mut Transform>(entity) {
                transform.position = body.translation();
                transform.rotation = quat_to_euler_degrees(*body.rotation());
            }
            if let Ok(mut rigid_body) = world.get::<&mut RigidBodyData>(entity) {
                rigid_body.linear_velocity = body.linvel();
                rigid_body.angular_velocity = body.angvel() * (180.0 / std::f32::consts::PI);
            }
        }

        self.drain_events()
    }

    fn drain_events(&mut self) -> Vec<ContactEvent> {
        let mut collisions = match self.events.collisions.lock() {
            Ok(mut collisions) => std::mem::take(&mut *collisions),
            Err(_) => Vec::new(),
        };
        // rapier's event order within a step is not documented as stable;
        // sorting pins the trace format deterministically.
        let mut events: Vec<ContactEvent> = collisions
            .drain(..)
            .filter_map(|event| {
                let (h1, h2, started) = match event {
                    CollisionEvent::Started(h1, h2, _) => (h1, h2, true),
                    CollisionEvent::Stopped(h1, h2, _) => (h1, h2, false),
                };
                let mut a = self.name_of.get(self.entity_of_collider.get(&h1)?)?.clone();
                let mut b = self.name_of.get(self.entity_of_collider.get(&h2)?)?.clone();
                if b < a {
                    std::mem::swap(&mut a, &mut b);
                }
                Some(ContactEvent { a, b, started })
            })
            .collect();
        events.sort_by(|x, y| (&x.a, &x.b, x.started).cmp(&(&y.a, &y.b, y.started)));
        events
    }

    /// Names of dynamic bodies, sorted — the stable row order for traces.
    pub fn dynamic_entity_names(&self, world: &World) -> Vec<String> {
        let mut names: Vec<String> = self
            .body_of
            .iter()
            .filter(|(_, &handle)| {
                self.bodies.get(handle).is_some_and(RigidBody::is_dynamic)
            })
            .filter_map(|(&entity, _)| world.get::<&Name>(entity).ok().map(|n| n.0.clone()))
            .collect();
        names.sort();
        names
    }

    /// Whether the named body is currently asleep.
    pub fn is_sleeping(&self, world: &World, name: &str) -> bool {
        self.body_of.iter().any(|(&entity, &handle)| {
            world.get::<&Name>(entity).is_ok_and(|n| n.0 == name)
                && self.bodies.get(handle).is_some_and(RigidBody::is_sleeping)
        })
    }

    /// Make scene queries valid before any step has run: the broad-phase
    /// BVH is normally maintained inside `step`, so a freshly built world
    /// (`--steps 0`) has an empty tree until this runs.
    pub fn refresh_queries(&mut self) {
        let modified: Vec<ColliderHandle> =
            self.colliders.iter().map(|(handle, _)| handle).collect();
        let mut events = Vec::new();
        self.broad_phase.update(
            &self.parameters,
            &self.colliders,
            &self.bodies,
            &modified,
            &[],
            &mut events,
        );
    }

    /// Cast a ray; the closest hit wins. `direction` need not be normalized.
    pub fn raycast(&self, from: Vec3, direction: Vec3) -> Option<RayHit> {
        let direction = direction.normalize_or_zero();
        if direction == Vec3::ZERO {
            return None;
        }

        let pipeline = self.broad_phase.as_query_pipeline(
            &DefaultQueryDispatcher,
            &self.bodies,
            &self.colliders,
            QueryFilter::default(),
        );
        let ray = Ray::new(from, direction);
        let (handle, intersection) = pipeline.cast_ray_and_get_normal(&ray, Real::MAX, true)?;

        let entity = self.entity_of_collider.get(&handle)?;
        Some(RayHit {
            entity: self.name_of.get(entity)?.clone(),
            point: ray.point_at(intersection.time_of_impact),
            normal: intersection.normal,
            distance: intersection.time_of_impact,
        })
    }
}

/// Collider shape with `Transform.scale` applied (validation already
/// rejected nonuniform scale on round shapes).
fn build_collider(
    collider: &ColliderData,
    transform: &Transform,
    entity: &str,
) -> Result<rapier3d::geometry::Collider> {
    let scale = transform.scale;

    let builder = match collider.shape {
        ColliderShapeKind::Cuboid => {
            let he = collider
                .half_extents
                .ok_or_else(|| shape_bug(entity, "cuboid collider without half_extents"))?;
            ColliderBuilder::cuboid(
                (he.x * scale.x).abs(),
                (he.y * scale.y).abs(),
                (he.z * scale.z).abs(),
            )
        }
        ColliderShapeKind::Sphere => {
            let radius = collider
                .radius
                .ok_or_else(|| shape_bug(entity, "sphere collider without radius"))?;
            ColliderBuilder::ball((radius * scale.x).abs())
        }
        ColliderShapeKind::Capsule => {
            let radius = collider
                .radius
                .ok_or_else(|| shape_bug(entity, "capsule collider without radius"))?;
            let half_height = collider
                .half_height
                .ok_or_else(|| shape_bug(entity, "capsule collider without half_height"))?;
            ColliderBuilder::capsule_y((half_height * scale.x).abs(), (radius * scale.x).abs())
        }
    };

    Ok(builder
        .translation(collider.offset * scale)
        .friction(collider.friction)
        .restitution(collider.restitution)
        // Pairwise restitution combines by MAX, not rapier's default
        // average: a restitution-1.0 ball on a plain ground should bounce
        // like the file says, not at half strength because the ground never
        // declared an opinion. The bouncier surface wins, which matches
        // intuition and keeps single-component authoring predictable.
        .restitution_combine_rule(CoefficientCombineRule::Max)
        .density(collider.density)
        .sensor(collider.sensor)
        // Contact begin/end feeds the trace; every collider participates.
        .active_events(ActiveEvents::COLLISION_EVENTS)
        .build())
}

/// Validation guarantees per-shape fields; reaching this is an engine bug,
/// reported as one rather than panicking.
fn shape_bug(entity: &str, what: &str) -> EngineError {
    EngineError::new(
        codes::SCENE_PARSE_DESYNC,
        format!("{what} on entity {entity:?} survived validation; this is an engine bug"),
    )
    .entity(entity)
}

fn pose_of(transform: &Transform) -> Pose {
    Pose::from_parts(transform.position, transform.quat())
}

fn degrees_to_radians(v: Vec3) -> Vec3 {
    v * (std::f32::consts::PI / 180.0)
}

/// Quaternion → Euler XYZ degrees, the file representation. Lossy as a
/// *representation* but deterministic, and canonicalized (−0 becomes 0) so
/// baked files are stable.
fn quat_to_euler_degrees(q: Quat) -> Vec3 {
    let (x, y, z) = q.to_euler(glam::EulerRot::XYZ);
    let canonical = |r: f32| {
        let degrees = r.to_degrees();
        if degrees == 0.0 {
            0.0
        } else {
            degrees
        }
    };
    Vec3::new(canonical(x), canonical(y), canonical(z))
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_core::Scene;

    const DROP: &str = r#"{
      "name": "drop",
      "entities": [
        {"name": "Ground", "components": [
          {"type": "Transform"},
          {"type": "Collider", "shape": "cuboid", "half_extents": [5.0, 0.05, 5.0]}
        ]},
        {"name": "Cube", "components": [
          {"type": "Transform", "position": [0.0, 5.0, 0.0]},
          {"type": "RigidBody", "body": "dynamic"},
          {"type": "Collider", "shape": "cuboid", "half_extents": [0.5, 0.5, 0.5]}
        ]}
      ]
    }"#;

    fn simulate(source: &str, steps: u32) -> (Scene, PhysicsWorld, Vec<ContactEvent>) {
        let mut scene = Scene::from_source(source, "test.json").unwrap();
        let settings = PhysicsSettings::default();
        let mut physics = PhysicsWorld::build(&scene.world, &settings).unwrap();
        let mut all_events = Vec::new();
        for _ in 0..steps {
            all_events.extend(physics.step(&mut scene.world));
        }
        (scene, physics, all_events)
    }

    fn position_of(scene: &Scene, name: &str) -> Vec3 {
        let entity = scene.entity(name).unwrap();
        scene.world.get::<&Transform>(entity).unwrap().position
    }

    #[test]
    fn a_dropped_cube_settles_on_the_ground() {
        let (scene, physics, events) = simulate(DROP, 300);

        let position = position_of(&scene, "Cube");
        // Ground top at 0.05, cube half-extent 0.5 → rest ≈ 0.55.
        assert!(
            (position.y - 0.55).abs() < 0.02,
            "cube should rest at ≈0.55, is at {position:?}"
        );

        let entity = scene.entity("Cube").unwrap();
        let body = scene.world.get::<&RigidBodyData>(entity).unwrap();
        assert!(
            body.linear_velocity.length() < 0.01,
            "settled cube should be still, moving at {:?}",
            body.linear_velocity
        );
        assert!(
            physics.is_sleeping(&scene.world, "Cube"),
            "a can_sleep body at rest reports sleeping"
        );
        assert!(
            events
                .iter()
                .any(|e| e.a == "Cube" && e.b == "Ground" && e.started),
            "the landing must appear as a contact event: {events:?}"
        );
    }

    #[test]
    fn restitution_controls_the_bounce() {
        let bouncy = DROP.replace(
            r#""shape": "cuboid", "half_extents": [0.5, 0.5, 0.5]"#,
            r#""shape": "sphere", "radius": 0.5, "restitution": 1.0"#,
        );
        let dead = DROP.replace(
            r#""shape": "cuboid", "half_extents": [0.5, 0.5, 0.5]"#,
            r#""shape": "sphere", "radius": 0.5, "restitution": 0.0"#,
        );

        // Track the apex reached after the first upward motion.
        let apex = |source: &str| -> f32 {
            let mut scene = Scene::from_source(source, "t.json").unwrap();
            let settings = PhysicsSettings::default();
            let mut physics = PhysicsWorld::build(&scene.world, &settings).unwrap();
            let mut bounced = false;
            let mut apex: f32 = 0.0;
            let mut previous_y = 5.0f32;
            for _ in 0..400 {
                physics.step(&mut scene.world);
                let y = position_of(&scene, "Cube").y;
                if y > previous_y {
                    bounced = true;
                }
                if bounced {
                    apex = apex.max(y);
                }
                previous_y = y;
            }
            apex
        };

        let high = apex(&bouncy);
        let low = apex(&dead);
        assert!(
            high > 3.0,
            "restitution 1.0 should bounce back most of the way, apex {high}"
        );
        assert!(low < 1.0, "restitution 0.0 should not meaningfully bounce, apex {low}");
    }

    #[test]
    fn fixed_bodies_and_static_colliders_never_move() {
        let (scene, _, _) = simulate(DROP, 120);
        assert_eq!(position_of(&scene, "Ground"), Vec3::ZERO);
    }

    #[test]
    fn kinematic_bodies_follow_their_transform() {
        let source = r#"{"name":"k","entities":[
            {"name":"Mover","components":[
                {"type":"Transform","position":[0.0,1.0,0.0]},
                {"type":"RigidBody","body":"kinematic"},
                {"type":"Collider","shape":"cuboid","half_extents":[0.5,0.5,0.5]}
            ]}
        ]}"#;
        let mut scene = Scene::from_source(source, "t.json").unwrap();
        let settings = PhysicsSettings::default();
        let mut physics = PhysicsWorld::build(&scene.world, &settings).unwrap();

        // Move the transform externally; the body must follow, not fight.
        let entity = scene.entity("Mover").unwrap();
        scene.world.get::<&mut Transform>(entity).unwrap().position = Vec3::new(3.0, 1.0, 0.0);
        physics.step(&mut scene.world);
        physics.step(&mut scene.world);

        assert_eq!(position_of(&scene, "Mover"), Vec3::new(3.0, 1.0, 0.0));
    }

    #[test]
    fn raycast_works_before_any_step() {
        let (_, mut physics, _) = simulate(DROP, 0);
        physics.refresh_queries();
        let hit = physics
            .raycast(Vec3::new(0.0, 10.0, 0.0), Vec3::NEG_Y)
            .expect("a freshly built world must be queryable");
        assert_eq!(hit.entity, "Cube");
    }

    #[test]
    fn raycast_hits_the_nearest_body() {
        let (_, physics, _) = simulate(DROP, 300);
        let hit = physics
            .raycast(Vec3::new(0.0, 10.0, 0.0), Vec3::NEG_Y)
            .expect("straight down must hit something");
        assert_eq!(hit.entity, "Cube", "the cube sits on the ground, so it is hit first");
        assert!((hit.point.y - 1.05).abs() < 0.03, "top of the settled cube, got {hit:?}");
        assert!(hit.normal.y > 0.9);

        let miss = physics.raycast(Vec3::new(50.0, 10.0, 0.0), Vec3::NEG_Y);
        assert!(miss.is_none());
    }

    #[test]
    fn simulation_is_deterministic_within_a_process() {
        let states = |steps| {
            let (scene, _, events) = simulate(DROP, steps);
            (position_of(&scene, "Cube").to_array(), events.len())
        };
        assert_eq!(states(180), states(180));
    }

    #[test]
    fn scale_makes_colliders_bigger() {
        let scaled = DROP.replace(
            r#""position": [0.0, 5.0, 0.0]"#,
            r#""position": [0.0, 5.0, 0.0], "scale": [2.0, 2.0, 2.0]"#,
        );
        let (scene, _, _) = simulate(&scaled, 300);
        let y = position_of(&scene, "Cube").y;
        // Scaled cube has half-extent 1.0 → rests at 0.05 + 1.0.
        assert!((y - 1.05).abs() < 0.02, "scaled cube should rest at ≈1.05, is at {y}");
    }
}
