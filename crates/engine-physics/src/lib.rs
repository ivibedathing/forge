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

use std::collections::{BTreeSet, HashMap};
use std::sync::Mutex;

use engine_core::components::{
    BodyKind, Collider as ColliderData, ColliderShapeKind, Mesh as MeshComponent, Name,
    RigidBody as RigidBodyData, Transform,
};
use engine_core::mesh::MeshSource;
use engine_core::scene::PhysicsSettings;
use engine_core::{codes, EngineError, Result};
use glam::{Quat, Vec3};
use hecs::{Entity, World};
use rapier3d::math::Pose;
use rapier3d::parry::query::DefaultQueryDispatcher;
use rapier3d::prelude::*;

/// One contact begin/end between two named entities — what traces record.
/// Shared vocabulary from `engine-core` so scripting can consume contacts
/// without depending on this crate.
pub use engine_core::contact::ContactEvent;

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
    /// The `(linear, angular-degrees)` velocities this world last wrote into
    /// each dynamic body's component (build values initially). A component
    /// that differs was written by a script and gets pushed into rapier
    /// before the next step. Push-only-on-change keeps the deg↔rad float
    /// round-trip out of untouched runs, so golden traces stay golden.
    written_velocities: HashMap<Entity, (Vec3, Vec3)>,
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
    /// `meshes` feeds `trimesh`/`convex_hull` colliders; scenes without mesh
    /// shapes never call it (`BuiltinAssets` is fine for tests).
    pub fn build(
        world: &World,
        settings: &PhysicsSettings,
        meshes: &dyn MeshSource,
    ) -> Result<Self> {
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
            written_velocities: HashMap::new(),
        };

        // Collision layers (M12): map each distinct name to one bit of
        // rapier's 32-bit interaction groups. BTreeSet iteration makes the
        // name → bit assignment deterministic; validation capped the count.
        let layer_bits: HashMap<String, Group> = {
            let mut names = BTreeSet::new();
            for collider in world.query::<&ColliderData>().iter() {
                names.extend(collider.layers.iter().flatten().cloned());
                names.extend(collider.collides_with.iter().flatten().cloned());
            }
            names
                .into_iter()
                .enumerate()
                .map(|(bit, name)| (name, Group::from_bits_truncate(1 << (bit as u32 % 32))))
                .collect()
        };

        // Deterministic build order: hecs iteration order is stable for a
        // freshly spawned world, and every simulate run spawns fresh.
        for (entity, name, transform, body, collider, mesh) in world
            .query::<(
                Entity,
                &Name,
                &Transform,
                Option<&RigidBodyData>,
                Option<&ColliderData>,
                Option<&MeshComponent>,
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
                let mut locked = LockedAxes::empty();
                for (axis, flag) in [
                    LockedAxes::ROTATION_LOCKED_X,
                    LockedAxes::ROTATION_LOCKED_Y,
                    LockedAxes::ROTATION_LOCKED_Z,
                ]
                .into_iter()
                .zip(body.locked_rotations)
                {
                    if flag {
                        locked |= axis;
                    }
                }
                let built = builder
                    .pose(position)
                    .linvel(body.linear_velocity)
                    .angvel(degrees_to_radians(body.angular_velocity))
                    .gravity_scale(body.gravity_scale)
                    .linear_damping(body.linear_damping)
                    .angular_damping(body.angular_damping)
                    .ccd_enabled(body.ccd)
                    .can_sleep(body.can_sleep)
                    .locked_axes(locked)
                    .build();
                let handle = physics.bodies.insert(built);
                physics.body_of.insert(entity, handle);
                physics
                    .written_velocities
                    .insert(entity, (body.linear_velocity, body.angular_velocity));
                handle
            });

            if let Some(collider) = collider {
                let built = build_collider(
                    collider,
                    transform,
                    &physics.name_of[&entity],
                    mesh.map(|m| m.asset.as_str()),
                    meshes,
                    &layer_bits,
                )?;
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
        //    Transform is now; dynamic bodies pick up script-written
        //    velocities (M11): a component velocity differing from the value
        //    this world last wrote back means a script changed it.
        for (&entity, &handle) in &self.body_of {
            let Some(body) = self.bodies.get_mut(handle) else {
                continue;
            };
            if body.is_kinematic() {
                if let Ok(transform) = world.get::<&Transform>(entity) {
                    body.set_next_kinematic_position(pose_of(&transform));
                }
            } else if body.is_dynamic() {
                if let Ok(component) = world.get::<&RigidBodyData>(entity) {
                    let current = (component.linear_velocity, component.angular_velocity);
                    if self.written_velocities.get(&entity) != Some(&current) {
                        body.set_linvel(current.0, true);
                        body.set_angvel(degrees_to_radians(current.1), true);
                        self.written_velocities.insert(entity, current);
                    }
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
                self.written_velocities.insert(
                    entity,
                    (rigid_body.linear_velocity, rigid_body.angular_velocity),
                );
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
/// rejected nonuniform scale on round shapes; mesh shapes scale per-vertex,
/// so any scale is representable).
fn build_collider(
    collider: &ColliderData,
    transform: &Transform,
    entity: &str,
    entity_mesh: Option<&str>,
    meshes: &dyn MeshSource,
    layer_bits: &HashMap<String, Group>,
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
        ColliderShapeKind::Trimesh | ColliderShapeKind::ConvexHull => {
            let asset = collider
                .asset
                .as_deref()
                .or(entity_mesh)
                .ok_or_else(|| shape_bug(entity, "mesh collider with no asset in reach"))?;
            let mesh = meshes.load_mesh(asset).map_err(|e| e.entity(entity))?;
            let vertices: Vec<Vec3> = mesh
                .positions
                .iter()
                .map(|p| Vec3::from_array(*p) * scale)
                .collect();

            match collider.shape {
                ColliderShapeKind::Trimesh => {
                    let indices: Vec<[u32; 3]> = mesh
                        .indices
                        .chunks_exact(3)
                        .map(|t| [t[0], t[1], t[2]])
                        .collect();
                    ColliderBuilder::trimesh(vertices, indices).map_err(|e| {
                        EngineError::new(
                            codes::INVALID_SHAPE_DIMENSION,
                            format!(
                                "mesh {asset:?} on entity {entity:?} does not form a \
                                 usable trimesh collider: {e:?}"
                            ),
                        )
                        .entity(entity)
                        .component("Collider")
                        .field("asset")
                    })?
                }
                _ => ColliderBuilder::convex_hull(&vertices).ok_or_else(|| {
                    EngineError::new(
                        codes::INVALID_SHAPE_DIMENSION,
                        format!(
                            "the vertices of mesh {asset:?} on entity {entity:?} \
                             form no valid convex hull"
                        ),
                    )
                    .entity(entity)
                    .component("Collider")
                    .field("asset")
                })?,
            }
        }
    };

    // Layers → interaction groups: absent fields mean "everything", which is
    // rapier's default, so layer-free scenes build byte-identical worlds.
    let groups = InteractionGroups::new(
        group_mask(collider.layers.as_deref(), layer_bits),
        group_mask(collider.collides_with.as_deref(), layer_bits),
        InteractionTestMode::And,
    );

    Ok(builder
        .collision_groups(groups)
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
        // Kinematic-vs-fixed pairs are opted in explicitly: rapier skips
        // them by default, but a scripted kinematic platform crossing a
        // static sensor is exactly what M10 traces need to see.
        .active_events(ActiveEvents::COLLISION_EVENTS)
        .active_collision_types(
            ActiveCollisionTypes::default() | ActiveCollisionTypes::KINEMATIC_FIXED,
        )
        .build())
}

/// A layer list as a rapier group mask. `None` (field absent) is ALL —
/// the pre-layer behavior; every name has a bit because the map was built
/// from the union of all names in the scene.
fn group_mask(layers: Option<&[String]>, layer_bits: &HashMap<String, Group>) -> Group {
    match layers {
        None => Group::ALL,
        Some(names) => names
            .iter()
            .filter_map(|name| layer_bits.get(name))
            .fold(Group::NONE, |mask, bit| mask | *bit),
    }
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
    use engine_core::mesh::BuiltinAssets;
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
        let mut physics = PhysicsWorld::build(&scene.world, &settings, &BuiltinAssets).unwrap();
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
            let mut physics = PhysicsWorld::build(&scene.world, &settings, &BuiltinAssets).unwrap();
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
        let mut physics = PhysicsWorld::build(&scene.world, &settings, &BuiltinAssets).unwrap();

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

    /// A script writing `RigidBody.linear_velocity` between steps must reach
    /// rapier (the M11 vehicle contract), and a *reverted* write must too —
    /// the cache compares against what physics wrote, not history.
    #[test]
    fn script_written_velocities_reach_the_solver() {
        let source = r#"{
          "name": "push",
          "entities": [
            {"name": "Ground", "components": [
              {"type": "Transform"},
              {"type": "Collider", "shape": "cuboid", "half_extents": [50.0, 0.05, 50.0]}
            ]},
            {"name": "Car", "components": [
              {"type": "Transform", "position": [0.0, 0.55, 0.0]},
              {"type": "RigidBody", "body": "dynamic", "locked_rotations": [true, false, true]},
              {"type": "Collider", "shape": "cuboid", "half_extents": [0.5, 0.5, 0.5], "friction": 0.0}
            ]}
          ]
        }"#;
        let mut scene = Scene::from_source(source, "test.json").unwrap();
        let settings = PhysicsSettings::default();
        let mut physics = PhysicsWorld::build(&scene.world, &settings, &BuiltinAssets).unwrap();
        let entity = scene.entity("Car").unwrap();

        // Settle, then "throttle": write a forward velocity like a script.
        for _ in 0..30 {
            physics.step(&mut scene.world);
        }
        let x_before = position_of(&scene, "Car").x;
        scene
            .world
            .get::<&mut RigidBodyData>(entity)
            .unwrap()
            .linear_velocity = Vec3::new(5.0, 0.0, 0.0);
        for _ in 0..60 {
            physics.step(&mut scene.world);
        }
        let moved = position_of(&scene, "Car").x - x_before;
        assert!(
            moved > 3.0,
            "a script-written velocity must move the body (moved {moved})"
        );
    }

    /// Untouched components must NOT be pushed back into rapier: the write
    /// path stays silent unless a script actually wrote, or the deg↔rad
    /// round-trip would perturb every trace.
    #[test]
    fn untouched_velocities_do_not_perturb_the_golden_path() {
        let (scene_a, _, _) = simulate(DROP, 200);
        let (scene_b, _, _) = simulate(DROP, 200);
        assert_eq!(
            position_of(&scene_a, "Cube"),
            position_of(&scene_b, "Cube"),
            "byte-identical runs"
        );
    }

    /// `locked_rotations` holds the axis fixed even under off-center impact.
    #[test]
    fn locked_rotations_pin_pitch_and_roll() {
        let source = r#"{
          "name": "locked",
          "entities": [
            {"name": "Ground", "components": [
              {"type": "Transform"},
              {"type": "Collider", "shape": "cuboid", "half_extents": [50.0, 0.05, 50.0]}
            ]},
            {"name": "Wall", "components": [
              {"type": "Transform", "position": [3.0, 0.6, 0.0]},
              {"type": "Collider", "shape": "cuboid", "half_extents": [0.2, 0.5, 2.0]}
            ]},
            {"name": "Car", "components": [
              {"type": "Transform", "position": [0.0, 0.75, 0.0]},
              {"type": "RigidBody", "body": "dynamic",
               "linear_velocity": [8.0, 0.0, 0.0],
               "locked_rotations": [true, false, true]},
              {"type": "Collider", "shape": "cuboid", "half_extents": [0.5, 0.7, 0.5]}
            ]}
          ]
        }"#;
        let mut scene = Scene::from_source(source, "test.json").unwrap();
        let settings = PhysicsSettings::default();
        let mut physics = PhysicsWorld::build(&scene.world, &settings, &BuiltinAssets).unwrap();
        for _ in 0..120 {
            physics.step(&mut scene.world);
        }
        let entity = scene.entity("Car").unwrap();
        let rotation = scene.world.get::<&Transform>(entity).unwrap().rotation;
        assert!(
            rotation.x.abs() < 1e-3 && rotation.z.abs() < 1e-3,
            "pitch/roll must stay locked through a wall hit: {rotation}"
        );
    }

    // ── Collision (M12): layers and mesh shapes ────────────────────────

    /// Two spheres over one layered ground: the one whose `collides_with`
    /// names the ground's layer lands; the one filtered elsewhere falls
    /// straight through. Filtering is mutual (AND), so the ground's absent
    /// fields ("everything") do not resurrect the filtered pair.
    #[test]
    fn collision_layers_filter_who_collides() {
        let source = r#"{
          "name": "layers",
          "entities": [
            {"name": "Ground", "components": [
              {"type": "Transform"},
              {"type": "Collider", "shape": "cuboid", "half_extents": [5.0, 0.05, 5.0],
               "layers": ["ground"]}
            ]},
            {"name": "Lands", "components": [
              {"type": "Transform", "position": [1.0, 2.0, 0.0]},
              {"type": "RigidBody", "body": "dynamic"},
              {"type": "Collider", "shape": "sphere", "radius": 0.5,
               "collides_with": ["ground"]}
            ]},
            {"name": "Ghost", "components": [
              {"type": "Transform", "position": [-1.0, 2.0, 0.0]},
              {"type": "RigidBody", "body": "dynamic"},
              {"type": "Collider", "shape": "sphere", "radius": 0.5,
               "collides_with": ["debris"]}
            ]}
          ]
        }"#;
        let (scene, _, events) = simulate(source, 240);

        let lands = position_of(&scene, "Lands");
        assert!(
            (lands.y - 0.55).abs() < 0.02,
            "the ground-filtered sphere must land at ≈0.55, is at {lands:?}"
        );
        let ghost = position_of(&scene, "Ghost");
        assert!(
            ghost.y < -2.0,
            "the debris-filtered sphere must fall through, is at {ghost:?}"
        );
        assert!(
            events.iter().all(|e| !(e.a == "Ghost" || e.b == "Ghost")),
            "a filtered pair must produce no contact events: {events:?}"
        );
    }

    /// A trimesh collider borrowing the entity's own `Mesh`: the plane's two
    /// triangles, scaled by the transform, carry a resting body — and the
    /// landing shows up as an ordinary contact event.
    #[test]
    fn trimesh_colliders_borrow_the_entity_mesh() {
        let source = r#"{
          "name": "trimesh",
          "entities": [
            {"name": "Track", "components": [
              {"type": "Transform", "scale": [10.0, 1.0, 10.0]},
              {"type": "Mesh", "asset": "builtin:plane"},
              {"type": "Collider", "shape": "trimesh"}
            ]},
            {"name": "Ball", "components": [
              {"type": "Transform", "position": [2.0, 3.0, 2.0]},
              {"type": "RigidBody", "body": "dynamic"},
              {"type": "Collider", "shape": "sphere", "radius": 0.5}
            ]}
          ]
        }"#;
        let (scene, _, events) = simulate(source, 300);

        let ball = position_of(&scene, "Ball");
        assert!(
            (ball.y - 0.5).abs() < 0.02,
            "the ball must rest on the trimesh plane at ≈0.5, is at {ball:?}"
        );
        assert!(
            events.iter().any(|e| e.a == "Ball" && e.b == "Track" && e.started),
            "the landing must appear as a contact event: {events:?}"
        );
    }

    /// A dynamic body with a convex-hull collider (the hull of the unit cube
    /// is the unit cube) rests exactly where a cuboid collider would.
    #[test]
    fn convex_hull_colliders_carry_dynamic_bodies() {
        let source = r#"{
          "name": "hull",
          "entities": [
            {"name": "Ground", "components": [
              {"type": "Transform"},
              {"type": "Collider", "shape": "cuboid", "half_extents": [5.0, 0.05, 5.0]}
            ]},
            {"name": "Crate", "components": [
              {"type": "Transform", "position": [0.0, 3.0, 0.0]},
              {"type": "RigidBody", "body": "dynamic"},
              {"type": "Collider", "shape": "convex_hull", "asset": "builtin:cube"}
            ]}
          ]
        }"#;
        let (scene, _, _) = simulate(source, 300);

        let y = position_of(&scene, "Crate").y;
        // Ground top 0.05 + unit-cube half-height 0.5.
        assert!(
            (y - 0.55).abs() < 0.02,
            "the hulled crate must rest at ≈0.55, is at {y}"
        );
    }
}

