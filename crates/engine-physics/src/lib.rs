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
use std::sync::Arc;
use std::sync::Mutex;

use engine_core::components::{
    BodyKind, Breakable, Collider as ColliderData, ColliderShapeKind, Mesh as MeshComponent, Name,
    Ragdoll as RagdollData, RigidBody as RigidBodyData, SkinnedCollider, Terrain as TerrainData,
    Transform, Wheel as WheelData,
};
use engine_core::mesh::{MeshSource, PhysicsAssets};
use engine_core::scene::PhysicsSettings;
use engine_core::skeleton::Rig;
use engine_core::{codes, EngineError, Result};
use glam::{Quat, Vec3};
use hecs::{Entity, World};
use rapier3d::control::{DynamicRayCastVehicleController, WheelTuning};
use rapier3d::math::Pose;
use rapier3d::parry::query::DefaultQueryDispatcher;
use rapier3d::prelude::*;

mod breaking;
pub use breaking::{apply_breaks, BreakEvent};

mod ragdoll;
mod skinned;

/// One contact begin/end between two named entities — what traces record.
/// Shared vocabulary from `engine-core` so scripting can consume contacts
/// without depending on this crate.
pub use engine_core::contact::ContactEvent;

/// A raycast hit, in scene terms.
#[derive(Debug, Clone, PartialEq)]
pub struct RayHit {
    pub entity: String,
    /// The skinned collider proxy that was hit, if the ray hit one (M33) —
    /// which is how a shot to the head is told from a shot to the shin.
    pub part: Option<String>,
    pub point: Vec3,
    pub normal: Vec3,
    pub distance: f32,
}

/// Where one skinned collider proxy is — `engine list-colliders`'s row.
#[derive(Debug, Clone, PartialEq)]
pub struct ProxyPlacement {
    pub entity: String,
    pub part: String,
    pub position: Vec3,
    /// Euler XYZ degrees, the file convention.
    pub rotation: Vec3,
}

/// One collider as the physics world actually holds it — `engine
/// list-colliders`'s row (M33).
///
/// Read back out of rapier rather than re-derived from the components, which
/// is what makes it impossible for the report and the simulation to disagree:
/// `road-centerline` and `ui-layout` exist for the same reason.
#[derive(Debug, Clone, PartialEq)]
pub struct ColliderReport {
    pub entity: String,
    /// The proxy part, when this collider is one (M33). Absent for every
    /// collider an entity's own `Collider` component built.
    pub part: Option<String>,
    /// `sphere`, `cuboid`, `capsule`, `trimesh`, `convex_hull`, or `other`.
    pub shape: &'static str,
    /// `radius` for a sphere, the three half-extents for a cuboid, and
    /// `[half_height, radius]` for a capsule; empty for a mesh shape, whose
    /// size is its geometry.
    pub dimensions: Vec<f32>,
    pub position: Vec3,
    /// Euler XYZ degrees, the file convention.
    pub rotation: Vec3,
    pub sensor: bool,
}

/// A queued blast (M12): applied at the start of the next step's
/// integration, so the explosion moves bodies the same step it fires.
/// Impulse falls off linearly from full strength at the center to zero at
/// `radius`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Explosion {
    pub center: Vec3,
    pub radius: f32,
    pub impulse: f32,
}

/// One break decision a step produced: a thresholded `Breakable` whose
/// threshold was met, with the explosion that did it when one did (fragment
/// kicks need the blast geometry).
#[derive(Debug, Clone, Copy)]
pub struct PendingBreak {
    pub entity: Entity,
    pub kick: Option<Explosion>,
}

/// Collects rapier collision events; drained after each step.
#[derive(Default)]
struct EventSink {
    collisions: Mutex<Vec<CollisionEvent>>,
    /// `(pair, contact impulse)` for pairs that opted into force events —
    /// breakable colliders only (M12). rapier reports force; multiplying by
    /// `dt` here makes the number an impulse, which survives a
    /// `timestep_hz` change.
    impulses: Mutex<Vec<(ColliderHandle, ColliderHandle, f32)>>,
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
        dt: Real,
        _bodies: &RigidBodySet,
        _colliders: &ColliderSet,
        contact_pair: &ContactPair,
        total_force_magnitude: Real,
    ) {
        if let Ok(mut impulses) = self.impulses.lock() {
            impulses.push((
                contact_pair.collider1,
                contact_pair.collider2,
                total_force_magnitude * dt,
            ));
        }
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
    /// Raycast vehicles (M12): one controller per chassis entity that has
    /// `Wheel` components pointing at it, in chassis-name order.
    vehicles: Vec<Vehicle>,
    /// Entities with a thresholded `Breakable` (M14): their colliders opt
    /// into contact-force events, and explosions test them for breaks.
    break_thresholds: HashMap<Entity, f32>,
    /// Blasts queued for the next step.
    queued_explosions: Vec<Explosion>,
    /// Whether the broad-phase BVH is still empty because no step has run.
    /// Only vehicles care: their suspension rays are cast *before* the first
    /// pipeline step (see `step`).
    bvh_cold: bool,
    /// Breaks this step decided on; drained by `take_pending_breaks`.
    pending_breaks: Vec<PendingBreak>,
    /// Skinned collider proxies (M33), in (entity name, part order) order —
    /// deterministic, since a rapier handle's identity follows insertion.
    proxies: Vec<skinned::Proxy>,
    /// The rig behind each proxied entity, resolved once at build: a skin is a
    /// property of the asset and cannot change mid-run, while which clip plays
    /// and what phase it is at are read from the components every step.
    rig_of: HashMap<Entity, Arc<Rig>>,
    /// Proxy colliders → the part name reports address them by. Absent for
    /// every ordinary collider, which is exactly how a report tells them apart.
    part_of_collider: HashMap<ColliderHandle, String>,
    /// Ragdolls (M39), in the proxies' own entity-name order. One per entity
    /// carrying both a `Ragdoll` and a `SkinnedCollider`; inactive until the
    /// handoff, and a scene with none never reaches any of this.
    ragdolls: Vec<ragdoll::Ragdoll>,
    /// Kicks queued by `world.ragdoll_impulse`, applied at the next step
    /// beside the explosions and for the same reason: an impulse applied
    /// before integration moves the body on the step it fires.
    queued_kicks: Vec<(String, String, Vec3)>,
}

/// One wheel awaiting assembly into a `Vehicle`, as `build` collects them:
/// the wheel's own entity name (which is what the sort is by, and so what
/// fixes the controller's wheel indices), its ECS entity, and its component.
type MountedWheel = (String, Entity, WheelData);

/// One raycast vehicle: a chassis body plus its wheels' visual entities,
/// in wheel-entity-name order (the controller's wheel indices follow it).
struct Vehicle {
    controller: DynamicRayCastVehicleController,
    chassis: RigidBodyHandle,
    /// The wheel visual entities, index-aligned with the controller's wheels.
    wheel_entities: Vec<Entity>,
}

impl PhysicsWorld {
    /// Whether a world contains any physics component at all — scenes
    /// without physics never construct a physics world. A `Breakable`
    /// counts (M12): its fragments spawn as dynamic bodies, so a break
    /// needs a physics world even if nothing else does.
    pub fn scene_has_physics(world: &World) -> bool {
        world.query::<&RigidBodyData>().iter().next().is_some()
            || world.query::<&ColliderData>().iter().next().is_some()
            || world.query::<&Breakable>().iter().next().is_some()
            // A proxied character (M33) is usually script-driven and carries
            // no body of its own, so without this a scene whose only physics
            // is a hitbox set would build no world at all.
            || world.query::<&SkinnedCollider>().iter().next().is_some()
    }

    /// Build a fresh physics world from the (already validated) scene world.
    /// `meshes` feeds `trimesh`/`convex_hull` colliders; scenes without mesh
    /// shapes never call it (`BuiltinAssets` is fine for tests).
    ///
    /// `assets` also supplies the rigs behind any `SkinnedCollider` (M33) —
    /// one source rather than two arguments, because the file a proxy's joints
    /// come out of is the entity's own `Mesh.asset`, the same file the mesh
    /// collider would have read.
    pub fn build(
        world: &World,
        settings: &PhysicsSettings,
        meshes: &dyn PhysicsAssets,
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
            vehicles: Vec::new(),
            break_thresholds: HashMap::new(),
            queued_explosions: Vec::new(),
            pending_breaks: Vec::new(),
            bvh_cold: true,
            proxies: Vec::new(),
            rig_of: HashMap::new(),
            part_of_collider: HashMap::new(),
            ragdolls: Vec::new(),
            queued_kicks: Vec::new(),
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
            // Proxy layers share the budget and the bit assignment (M33): a
            // character's "hitbox" and a bullet's "bullet" are names in the
            // same scene-local namespace, and validation counts them together.
            for proxies in world.query::<&SkinnedCollider>().iter() {
                names.extend(proxies.layers.iter().flatten().cloned());
                names.extend(proxies.collides_with.iter().flatten().cloned());
            }
            names
                .into_iter()
                .enumerate()
                .map(|(bit, name)| (name, Group::from_bits_truncate(1 << (bit as u32 % 32))))
                .collect()
        };

        // Every surface an entity *generates* — a Road's ribbon, a Junction's
        // patch — built once here, by entity name, through the same functions
        // the renderer's draw list goes through (M40). Before M40 a road could
        // be regenerated from its own component inside `build_collider`; a road
        // that follows a `Terrain` it names, and a junction bounded by the
        // roads that reach it, both need the rest of the world in view.
        let generated: HashMap<String, GeneratedSurface> = {
            let mut surfaces = HashMap::new();
            for item in engine_core::scene::road_items_of(world) {
                surfaces.insert(
                    item.entity.clone(),
                    GeneratedSurface {
                        kind: if item.junction.is_some() {
                            "Junction"
                        } else {
                            "Road"
                        },
                        mesh: Arc::clone(&item.surface.mesh),
                    },
                );
            }
            surfaces
        };

        // Deterministic build order: hecs iteration order is stable for a
        // freshly spawned world, and every simulate run spawns fresh.
        for (entity, name, transform, body, collider, mesh, terrain, breakable) in world
            .query::<(
                Entity,
                &Name,
                &Transform,
                Option<&RigidBodyData>,
                Option<&ColliderData>,
                Option<&MeshComponent>,
                Option<&TerrainData>,
                Option<&Breakable>,
            )>()
            .iter()
        {
            if body.is_none() && collider.is_none() {
                continue;
            }
            physics.name_of.insert(entity, name.0.clone());

            // A thresholded Breakable with a collider watches for breaking
            // impacts; its collider opts into contact-force events below.
            let break_threshold = breakable.and_then(|b| b.impulse_threshold);
            if let Some(threshold) = break_threshold {
                if collider.is_some() {
                    physics.break_thresholds.insert(entity, threshold);
                }
            }

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
                    terrain,
                    generated.get(&physics.name_of[&entity]),
                    meshes,
                    &layer_bits,
                    break_threshold.is_some(),
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

        // ── Vehicles (M12): group Wheel components by chassis name ────
        // Deterministic build order: chassis sorted by name, wheels within a
        // vehicle sorted by their entity name; the controller's wheel
        // indices follow that order.
        let mut wheels_by_chassis: Vec<(String, Vec<MountedWheel>)> = Vec::new();
        for (entity, name, wheel) in world.query::<(Entity, &Name, &WheelData)>().iter() {
            match wheels_by_chassis
                .iter_mut()
                .find(|(c, _)| *c == wheel.vehicle)
            {
                Some((_, list)) => list.push((name.0.clone(), entity, wheel.clone())),
                None => wheels_by_chassis.push((
                    wheel.vehicle.clone(),
                    vec![(name.0.clone(), entity, wheel.clone())],
                )),
            }
        }
        wheels_by_chassis.sort_by(|a, b| a.0.cmp(&b.0));

        for (chassis_name, mut wheel_list) in wheels_by_chassis {
            wheel_list.sort_by(|a, b| a.0.cmp(&b.0));

            let chassis_handle = physics
                .body_of
                .iter()
                .find(|(entity, _)| physics.name_of.get(entity) == Some(&chassis_name))
                .map(|(_, &handle)| handle)
                .ok_or_else(|| {
                    // Validation guarantees the chassis exists with a dynamic
                    // body; reaching this is an engine bug.
                    EngineError::new(
                        codes::SCENE_PARSE_DESYNC,
                        format!(
                            "Wheel components name vehicle {chassis_name:?} but no such \
                             rigid body was built; this survived validation, which is an \
                             engine bug"
                        ),
                    )
                    .entity(&chassis_name)
                })?;

            let mut controller = DynamicRayCastVehicleController::new(chassis_handle);
            // File conventions: up is +Y, forward is the chassis's local −Z
            // (the camera/light convention). The controller has no sign on
            // its forward axis; +Z with the axle on +X makes the drive
            // direction `normal × axle = −Z`, so positive `engine_force`
            // pushes the chassis forward. See the sign notes on `Wheel`.
            controller.index_up_axis = 1;
            controller.index_forward_axis = 2;

            let mut wheel_entities = Vec::with_capacity(wheel_list.len());
            for (_, entity, wheel) in wheel_list {
                let tuning = WheelTuning {
                    suspension_stiffness: wheel.suspension_stiffness,
                    suspension_compression: wheel.suspension_compression,
                    suspension_damping: wheel.suspension_damping,
                    max_suspension_travel: wheel.suspension_travel,
                    side_friction_stiffness: wheel.side_friction_stiffness,
                    friction_slip: wheel.friction_slip,
                    max_suspension_force: wheel.max_suspension_force,
                };
                controller.add_wheel(
                    wheel.offset,
                    -Vector::Y,
                    Vector::X,
                    wheel.suspension_rest_length,
                    wheel.radius,
                    &tuning,
                );
                wheel_entities.push(entity);
            }

            physics.vehicles.push(Vehicle {
                controller,
                chassis: chassis_handle,
                wheel_entities,
            });
        }

        // ── Skinned collider proxies (M33) ────────────────────────────
        //
        // Entity-name sorted, then in the component's own part order, so a
        // rapier handle's identity — which follows insertion — is a function
        // of the file and nothing else.
        let mut proxied: Vec<(String, Entity, SkinnedCollider, String, Transform)> = Vec::new();
        for (entity, name, transform, proxies, mesh) in world
            .query::<(
                Entity,
                &Name,
                &Transform,
                &SkinnedCollider,
                Option<&MeshComponent>,
            )>()
            .iter()
        {
            // No `Mesh` is `skinned_collider_without_skin` at validation; a
            // world built past it has nothing to ride and skips quietly.
            let Some(mesh) = mesh else { continue };
            proxied.push((
                name.0.clone(),
                entity,
                proxies.clone(),
                mesh.asset.clone(),
                *transform,
            ));
        }
        proxied.sort_by(|a, b| a.0.cmp(&b.0));

        for (name, entity, proxies, asset, transform) in proxied {
            let rig = meshes.load_rig(&asset)?;
            let Some(skin) = &rig.skin else {
                // Validation refuses this (`skinned_collider_without_skin`),
                // so reaching it means the walk and the loader disagree.
                return Err(EngineError::new(
                    codes::SCENE_PARSE_DESYNC,
                    format!(
                        "entity {name:?} has a SkinnedCollider but its mesh {asset:?} \
                         carries no skin; this survived validation, which is an engine bug"
                    ),
                )
                .entity(&name));
            };
            physics.name_of.insert(entity, name.clone());
            // Uniform by validation, so any axis is *the* scale.
            let scale = transform.scale.x;
            let model = transform.matrix();
            // For a ragdoll reloaded out of a bake this is the *ragdoll's* pose
            // — `posed_globals_at` reads `Ragdoll.pose` before it resolves a
            // clip — so a corpse's proxies are built where the corpse is
            // lying, with no special case here. That is what makes the bake
            // round-trip work.
            let globals = engine_core::locomotion::posed_globals_at(world, entity, &rig, Some(0.0));
            let first_part = physics.proxies.len();

            for part in &proxies.parts {
                let Some(joint) = skin.joint_named(&part.joint) else {
                    return Err(EngineError::new(
                        codes::SCENE_PARSE_DESYNC,
                        format!(
                            "the SkinnedCollider on {name:?} rides joint {:?}, which the \
                             rig in {asset:?} does not have; this survived validation, \
                             which is an engine bug",
                            part.joint
                        ),
                    )
                    .entity(&name));
                };
                let Some(shape) = skinned::part_shape(part, scale) else {
                    return Err(EngineError::new(
                        codes::SCENE_PARSE_DESYNC,
                        format!(
                            "part {:?} of the SkinnedCollider on {name:?} names a mesh \
                             shape, which a proxy cannot be; this survived validation, \
                             which is an engine bug",
                            part.part_name()
                        ),
                    )
                    .entity(&name));
                };

                let local = skinned::part_local(part, scale);
                let pose = skinned::part_pose(
                    model,
                    globals.get(joint).copied().unwrap_or(glam::Mat4::IDENTITY),
                    local,
                );

                // Kinematic, and that is the milestone's whole invariant: the
                // pose drives the proxy and nothing writes back to the pose.
                let body = physics.bodies.insert(
                    RigidBodyBuilder::kinematic_position_based()
                        .pose(pose)
                        .build(),
                );

                let built = ColliderBuilder::new(shape)
                    .friction(proxies.friction)
                    .restitution(proxies.restitution)
                    .restitution_combine_rule(CoefficientCombineRule::Max)
                    .sensor(part.sensor)
                    .collision_groups(InteractionGroups::new(
                        group_mask(proxies.layers.as_deref(), &layer_bits),
                        group_mask(proxies.collides_with.as_deref(), &layer_bits),
                        InteractionTestMode::And,
                    ))
                    // Only proxies carry hooks, so `SelfFilter` is unreachable
                    // for a scene without one and the solver sees exactly the
                    // pairs it always did.
                    .active_hooks(
                        ActiveHooks::FILTER_CONTACT_PAIRS | ActiveHooks::FILTER_INTERSECTION_PAIR,
                    )
                    // rapier reports neither kinematic-vs-fixed nor
                    // kinematic-vs-kinematic contacts by default (M10 hit the
                    // first of those), and "did the sword touch the shield" is
                    // the second — the question proxies exist to answer.
                    .active_collision_types(ActiveCollisionTypes::all())
                    .active_events(ActiveEvents::COLLISION_EVENTS)
                    .build();
                let collider =
                    physics
                        .colliders
                        .insert_with_parent(built, body, &mut physics.bodies);

                // Reports name the *entity*, never the proxy (design §5), so a
                // proxy collider maps to its owner exactly as any other does;
                // the part rides alongside.
                physics.entity_of_collider.insert(collider, entity);
                physics
                    .part_of_collider
                    .insert(collider, part.part_name().to_string());
                physics.proxies.push(skinned::Proxy {
                    entity,
                    joint,
                    part: part.part_name().to_string(),
                    local,
                    body,
                    fit: part.fit.map(|_| skinned::Fit {
                        radius: part.radius.unwrap_or(0.0) * scale,
                        half_length: match part.shape {
                            engine_core::components::ColliderShapeKind::Cuboid => {
                                part.half_extents.map(|h| h.y * scale).unwrap_or_default()
                            }
                            _ => part.half_height.unwrap_or_default() * scale,
                        },
                        half_extents: part.half_extents.map(|h| h * scale),
                    }),
                });
            }

            // The ragdoll (M39), if this character has one. Recorded after its
            // parts so the indices are the range this entity just wrote, and
            // the graph comes from the shared `engine_core::ragdoll` so that
            // validation and the simulation cannot disagree about which part
            // hangs from which.
            if world.get::<&RagdollData>(entity).is_ok() {
                let parents = engine_core::ragdoll::parent_parts(skin, &proxies.parts);
                // `ragdoll_disconnected_parts` refuses more than one root, and
                // `ragdoll_without_proxies` refuses none at all; a world built
                // past either takes the first, which is a character that
                // simulates oddly rather than one that panics.
                let root = engine_core::ragdoll::roots(&parents)
                    .first()
                    .copied()
                    .unwrap_or(0);
                physics.ragdolls.push(ragdoll::Ragdoll {
                    entity,
                    parts: (first_part..physics.proxies.len()).collect(),
                    parents,
                    root,
                    active: false,
                    joints: Vec::new(),
                    frozen: Vec::new(),
                });
            }
            physics.rig_of.insert(entity, rig);
        }

        // A scene that ships `"active": true` is a corpse from step 0, which is
        // what lets a fixture exist without a script — and what a bake reloads
        // as. Done after the loop so every proxy handle is in place.
        for index in 0..physics.ragdolls.len() {
            let entity = physics.ragdolls[index].entity;
            let active = world
                .get::<&RagdollData>(entity)
                .map(|r| r.active)
                .unwrap_or(false);
            if active {
                physics.activate_ragdoll(world, index, 0.0);
            }
        }

        Ok(physics)
    }

    /// Insert one entity's physics presence — shared by the initial build
    /// and by fragment spawning (M12). Entities with neither a body nor a
    /// collider have no presence and are skipped. A `break_threshold` opts
    /// the entity's collider into contact-force events.
    pub fn insert_entity(
        &mut self,
        entity: Entity,
        name: &str,
        transform: &Transform,
        body: Option<&RigidBodyData>,
        collider: Option<&ColliderData>,
        break_threshold: Option<f32>,
    ) -> Result<()> {
        if body.is_none() && collider.is_none() {
            return Ok(());
        }
        self.name_of.insert(entity, name.to_string());
        if let Some(threshold) = break_threshold {
            if collider.is_some() {
                self.break_thresholds.insert(entity, threshold);
            }
        }

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
            let handle = self.bodies.insert(built);
            self.body_of.insert(entity, handle);
            self.written_velocities
                .insert(entity, (body.linear_velocity, body.angular_velocity));
            handle
        });

        if let Some(collider) = collider {
            // Spawned entities (fragments) carry cuboid colliders with no
            // mesh shapes, no terrain, no roads and no layers, so builtin
            // meshes and an empty layer table cover every caller.
            let built = build_collider(
                collider,
                transform,
                name,
                None,
                None,
                None,
                &engine_core::mesh::BuiltinAssets,
                &HashMap::new(),
                break_threshold.is_some(),
            )?;
            let handle = match body_handle {
                Some(body_handle) => {
                    self.colliders
                        .insert_with_parent(built, body_handle, &mut self.bodies)
                }
                None => {
                    // Static geometry: place the collider in world space
                    // directly; no body needed.
                    let mut built = built;
                    built.set_position(position * built.position());
                    self.colliders.insert(built)
                }
            };
            self.entity_of_collider.insert(handle, entity);
        }

        Ok(())
    }

    /// Remove an entity's physics presence entirely (M12 breaks): body,
    /// colliders (attached or static), and every map entry.
    pub fn remove_entity(&mut self, entity: Entity) {
        if let Some(handle) = self.body_of.remove(&entity) {
            self.bodies.remove(
                handle,
                &mut self.islands,
                &mut self.colliders,
                &mut self.impulse_joints,
                &mut self.multibody_joints,
                true,
            );
        }
        let orphans: Vec<ColliderHandle> = self
            .entity_of_collider
            .iter()
            .filter(|(_, e)| **e == entity)
            .map(|(handle, _)| *handle)
            .collect();
        for handle in orphans {
            if self.colliders.get(handle).is_some() {
                self.colliders
                    .remove(handle, &mut self.islands, &mut self.bodies, true);
            }
            self.entity_of_collider.remove(&handle);
        }
        self.written_velocities.remove(&entity);
        self.name_of.remove(&entity);
        self.break_thresholds.remove(&entity);
    }

    /// Queue a blast for the next step (M12). Scripts call this through
    /// `world.explode`; it is applied before integration, so the blast
    /// moves bodies the same step it fires.
    pub fn queue_explosion(&mut self, explosion: Explosion) {
        self.queued_explosions.push(explosion);
    }

    /// Queue a kick to one ragdoll part, by the entity name and the part name
    /// `world.touching_parts` already returns (M39 §9).
    ///
    /// Addressed by name rather than by handle because a script has names and
    /// nothing else — the same reason `queue_explosion` takes a point.
    pub fn queue_ragdoll_impulse(&mut self, entity: &str, part: &str, impulse: Vec3) {
        self.queued_kicks
            .push((entity.to_string(), part.to_string(), impulse));
    }

    /// The break decisions the last step made, sorted by entity name and
    /// deduplicated (first cause wins, so an explosion's kick survives).
    /// Callers apply them via [`apply_breaks`](crate::apply_breaks).
    pub fn take_pending_breaks(&mut self) -> Vec<PendingBreak> {
        let mut breaks = std::mem::take(&mut self.pending_breaks);
        breaks.sort_by(|a, b| {
            self.name_of
                .get(&a.entity)
                .cmp(&self.name_of.get(&b.entity))
        });
        breaks.dedup_by_key(|b| b.entity);
        breaks
    }

    /// Advance one fixed step and write the results back into hecs. Returns
    /// the contact events the step produced, in deterministic order.
    /// One fixed step. `time` is the **scene** time this step ends at —
    /// `steps · dt`, the same number the render would draw with — and is what
    /// skinned collider proxies sample the pose at (M33).
    ///
    /// It is a parameter rather than a counter this struct keeps, so that no
    /// caller can quietly drift from the clock the picture uses; a world with
    /// no proxies ignores it entirely, which is why every pre-M33 golden trace
    /// is untouched whatever is passed.
    pub fn step(&mut self, world: &mut World, time: f32) -> Vec<ContactEvent> {
        // 0. Ragdoll handoffs (M39). A script sets `Ragdoll.active` and this is
        //    where it takes effect — before the proxies are posed, because from
        //    this step on they are not followers. M10's ordering, and M12's
        //    one-step latency for the same reason: scripts run before physics.
        for index in 0..self.ragdolls.len() {
            if self.ragdolls[index].active {
                continue;
            }
            let entity = self.ragdolls[index].entity;
            let active = world
                .get::<&RagdollData>(entity)
                .map(|r| r.active)
                .unwrap_or(false);
            if active {
                self.activate_ragdoll(world, index, time);
            }
        }

        // 0.5. Proxies follow the pose the render will draw at the end of this
        //    step, which is exactly what `set_next_kinematic_position` means:
        //    rapier interpolates from where the body is to where it is told it
        //    will be. Nothing here reads a proxy back into the skeleton —
        //    except for a ragdolled character, which this skips entirely
        //    because its proxies are now the thing being read.
        self.pose_proxies(world, time);

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

        // 1.5. Queued blasts (M14): a radial, linearly-falling-off impulse
        //      to every dynamic body in range, applied before integration
        //      so the blast moves bodies the same step it fires. Impulses
        //      to distinct bodies commute, so map order cannot matter;
        //      multiple blasts apply in queue order.
        let explosions = std::mem::take(&mut self.queued_explosions);
        for explosion in &explosions {
            for &handle in self.body_of.values() {
                let Some(body) = self.bodies.get_mut(handle) else {
                    continue;
                };
                if !body.is_dynamic() {
                    continue;
                }
                let delta = body.translation() - explosion.center;
                let distance = delta.length();
                if distance >= explosion.radius {
                    continue;
                }
                let direction = if distance > 1e-6 {
                    delta / distance
                } else {
                    Vec3::Y
                };
                let falloff = 1.0 - distance / explosion.radius;
                body.apply_impulse(direction * (explosion.impulse * falloff), true);
            }

            // Threshold checks read entity positions from hecs, so static
            // breakables (a wall with no RigidBody) break too.
            for (&entity, &threshold) in &self.break_thresholds {
                let Ok(transform) = world.get::<&Transform>(entity) else {
                    continue;
                };
                let distance = (transform.position - explosion.center).length();
                if distance >= explosion.radius {
                    continue;
                }
                if explosion.impulse * (1.0 - distance / explosion.radius) >= threshold {
                    self.pending_breaks.push(PendingBreak {
                        entity,
                        kick: Some(*explosion),
                    });
                }
            }
        }

        // 1.6. Ragdoll kicks (M39): an impulse to one named hitbox, applied
        //      before integration so the head snaps back on the step the shot
        //      landed. Impulses to distinct bodies commute, so queue order is
        //      the only order that can matter and it is the call order.
        let kicks = std::mem::take(&mut self.queued_kicks);
        for (entity, part, impulse) in kicks {
            let Some(proxy) = self.proxies.iter().find(|proxy| {
                proxy.part == part
                    && self
                        .name_of
                        .get(&proxy.entity)
                        .is_some_and(|n| *n == entity)
            }) else {
                continue;
            };
            if let Some(body) = self.bodies.get_mut(proxy.body) {
                // Only a dynamic body takes an impulse; the script layer
                // already refuses a character that has not ragdolled, so
                // reaching this with a kinematic proxy means the two disagree.
                if body.is_dynamic() {
                    body.apply_impulse(impulse, true);
                }
            }
        }

        // 2. Vehicles (M12): push script-written controls into each
        //    controller, then let it cast its suspension rays and apply
        //    spring, drive, brake, and tire impulses to the chassis. Runs
        //    after the velocity sync so suspension sees script-written
        //    velocities, and before the solver integrates the impulses.
        let dt = self.parameters.dt;

        // On the very first step the BVH is still empty — it is built inside
        // `pipeline.step`, which has not run yet — so the suspension rays
        // below would find no ground. Build it on a *scratch copy* rather
        // than on `self.broad_phase`: a broad-phase update reports each new
        // collider pair exactly once, and `NarrowPhase::register_pairs` is
        // private to rapier, so priming the real one here would swallow the
        // pairs of everything already resting in contact at load. Those
        // bodies would then never touch anything again and would fall
        // straight through the floor — with no error, in a scene that
        // validates.
        let mut cold_bvh = None;
        if self.bvh_cold && !self.vehicles.is_empty() {
            let mut scratch = self.broad_phase.clone();
            let modified: Vec<ColliderHandle> =
                self.colliders.iter().map(|(handle, _)| handle).collect();
            scratch.update(
                &self.parameters,
                &self.colliders,
                &self.bodies,
                &modified,
                &[],
                &mut Vec::new(),
            );
            cold_bvh = Some(scratch);
        }
        self.bvh_cold = false;

        for vehicle in &mut self.vehicles {
            let mut any_drive = false;
            for (index, &entity) in vehicle.wheel_entities.iter().enumerate() {
                let Ok(wheel) = world.get::<&WheelData>(entity) else {
                    continue;
                };
                let state = &mut vehicle.controller.wheels_mut()[index];
                state.engine_force = wheel.engine_force;
                state.brake = wheel.brake;
                state.steering = wheel.steering.to_radians();
                any_drive |= wheel.engine_force != 0.0 || wheel.brake != 0.0;
            }
            // The controller only wakes the chassis for *positive* engine
            // force; reverse and braking must not act on a sleeping body
            // either, so wake it for any nonzero control ourselves.
            if any_drive {
                if let Some(body) = self.bodies.get_mut(vehicle.chassis) {
                    body.wake_up(true);
                }
            }

            // Suspension rays must not hit the chassis itself (they start
            // inside its collider) and must not ride on sensors.
            let filter = QueryFilter::default()
                .exclude_rigid_body(vehicle.chassis)
                .exclude_sensors();
            let bvh = match cold_bvh.as_mut() {
                Some(scratch) => scratch,
                None => &mut self.broad_phase,
            };
            let queries = bvh.as_query_pipeline_mut(
                &DefaultQueryDispatcher,
                &mut self.bodies,
                &mut self.colliders,
                filter,
            );
            vehicle.controller.update_vehicle(dt, queries);
        }

        // 3. One fixed step.
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
            &skinned::SelfFilter {
                owner: &self.entity_of_collider,
            },
            &self.events,
        );

        // 3.5. Ragdolls read their skeleton back out of where the bodies ended
        //      up (M39) — the one place in this engine where physics writes a
        //      pose, and it writes it into a component rather than into this
        //      struct. Before the transform write-back below, which a ragdolled
        //      entity's own (now disabled) body no longer takes part in.
        if !self.ragdolls.is_empty() {
            self.write_back_ragdolls(world);
        }

        // 4. Write back into hecs for dynamic bodies: the scene components
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

        // 5. Pose the wheel visuals from the *post-step* chassis pose:
        //    suspension compression drops the wheel down its ray, steering
        //    yaws it, and the accumulated axle spin rolls it. The trailing
        //    Z-90° maps the builtin cylinder's Y axis onto the axle.
        for vehicle in &self.vehicles {
            let Some(chassis) = self.bodies.get(vehicle.chassis) else {
                continue;
            };
            let chassis_position = chassis.translation();
            let chassis_rotation = *chassis.rotation();

            for (index, &entity) in vehicle.wheel_entities.iter().enumerate() {
                let state = &vehicle.controller.wheels()[index];
                let offset = match world.get::<&WheelData>(entity) {
                    Ok(wheel) => wheel.offset,
                    Err(_) => continue,
                };
                let length = state.raycast_info().suspension_length;
                let center = chassis_position + chassis_rotation * (offset - Vec3::Y * length);
                let pose = chassis_rotation
                    * Quat::from_rotation_y(state.steering)
                    * Quat::from_rotation_x(state.rotation)
                    * Quat::from_rotation_z(std::f32::consts::FRAC_PI_2);
                if let Ok(mut transform) = world.get::<&mut Transform>(entity) {
                    transform.position = center;
                    transform.rotation = quat_to_euler_degrees(pose);
                }
            }
        }

        // 6. Contact-impulse breaks (M14): the step's peak contact impulse
        //    per thresholded breakable, compared against its threshold.
        //    Peak (not sum): one hard hit breaks a crate, resting contact
        //    under gravity accumulating over steps must not.
        let impulses = match self.events.impulses.lock() {
            Ok(mut impulses) => std::mem::take(&mut *impulses),
            Err(_) => Vec::new(),
        };
        let mut peak: HashMap<Entity, f32> = HashMap::new();
        for (h1, h2, impulse) in impulses {
            for handle in [h1, h2] {
                let Some(&entity) = self.entity_of_collider.get(&handle) else {
                    continue;
                };
                if self.break_thresholds.contains_key(&entity) {
                    let slot = peak.entry(entity).or_insert(0.0);
                    *slot = slot.max(impulse);
                }
            }
        }
        for (entity, impulse) in peak {
            if impulse >= self.break_thresholds[&entity] {
                self.pending_breaks
                    .push(PendingBreak { entity, kick: None });
            }
        }

        self.drain_events()
    }

    /// Move every proxy onto the pose its entity has at scene time `time`.
    ///
    /// The pose comes from `locomotion::posed_globals_at` — the one seam the
    /// render, `engine list-joints` and `world.joint_position` already share —
    /// so a hitbox cannot end up somewhere the character visibly is not, and a
    /// planted character's ankle proxies are on the ground because the pose
    /// they read is the planted one.
    ///
    /// One pose per *entity*, not per part: a fifteen-part humanoid samples its
    /// clip once and every part reads the joint it rides out of the result.
    ///
    /// **A stride-driven character's proxies lag its render by one step**, and
    /// that is causal rather than a defect: `AnimationPlayer.phase` is advanced
    /// by the ground the entity *covered*, which is not known until physics has
    /// run, so the pose this step can be told to move toward is the one the
    /// previous step's phase describes. It is M12's contact latency again — the
    /// same shape, the same reason — and it is worth a millimetre or two on a
    /// walking character. A clock-driven clip has no lag at all: `time` is the
    /// end of this step, which is exactly what the render will draw.
    fn pose_proxies(&mut self, world: &World, time: f32) {
        if self.proxies.is_empty() {
            return;
        }
        // Entities physics has taken the skeleton of (M39): their proxies are
        // dynamic now, and telling a dynamic body where it "will be" would
        // teleport a corpse back onto the pose it fell out of, every step.
        let ragdolled: std::collections::HashSet<Entity> = self
            .ragdolls
            .iter()
            .filter(|r| r.active)
            .map(|r| r.entity)
            .collect();

        let mut posed: HashMap<Entity, (glam::Mat4, Vec<glam::Mat4>)> = HashMap::new();
        let mut refits: Vec<(usize, f32)> = Vec::new();
        for (index, proxy) in self.proxies.iter().enumerate() {
            if ragdolled.contains(&proxy.entity) {
                continue;
            }
            let entry = match posed.entry(proxy.entity) {
                std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
                std::collections::hash_map::Entry::Vacant(slot) => {
                    let Some(rig) = self.rig_of.get(&proxy.entity) else {
                        continue;
                    };
                    let model = world
                        .get::<&Transform>(proxy.entity)
                        .map(|t| t.matrix())
                        .unwrap_or(glam::Mat4::IDENTITY);
                    let globals = engine_core::locomotion::posed_globals_at(
                        world,
                        proxy.entity,
                        rig,
                        Some(time),
                    );
                    slot.insert((model, globals))
                }
            };
            let (model, globals) = &*entry;
            let Some(&global) = globals.get(proxy.joint) else {
                continue;
            };
            let pose = skinned::part_pose(*model, global, proxy.local);

            // A `fit: "bone"` part takes its length from the posed rig (M39
            // §7). Measured here, applied below: the shape swap needs
            // `&mut self.colliders` while this loop holds `&self.proxies`, and
            // a second pass is cheaper than cloning the proxy list.
            if let Some(fit) = &proxy.fit {
                if let Some(rig) = self.rig_of.get(&proxy.entity) {
                    if let Some(skin) = &rig.skin {
                        if let Some(half) =
                            ragdoll::fitted_half_length(skin, globals, proxy.joint, fit.radius)
                        {
                            if (half - fit.half_length).abs() > ragdoll::FIT_EPSILON {
                                refits.push((index, half));
                            }
                        }
                    }
                }
            }

            if let Some(body) = self.bodies.get_mut(proxy.body) {
                body.set_next_kinematic_position(pose);
            }
        }

        // The rebuilds this step decided on. A rig whose clips animate rotation
        // only — every clip in this repo — leaves this empty after the first
        // step, so the feature costs one comparison per part per step on the
        // scenes that use it and nothing at all on the scenes that do not.
        for (index, half) in refits {
            let proxy = &self.proxies[index];
            let Some(fit) = &proxy.fit else { continue };
            let shape = match fit.half_extents {
                Some(half_extents) => SharedShape::cuboid(half_extents.x, half, half_extents.z),
                None => SharedShape::capsule_y(half, fit.radius),
            };
            let proxy_body = proxy.body;
            let handles: Vec<ColliderHandle> = self
                .bodies
                .get(proxy_body)
                .map(|b| b.colliders().to_vec())
                .unwrap_or_default();
            for handle in handles {
                if let Some(collider) = self.colliders.get_mut(handle) {
                    collider.set_shape(shape.clone());
                }
            }
            if let Some(fit) = &mut self.proxies[index].fit {
                fit.half_length = half;
            }
        }
    }

    /// Hand a character's skeleton to physics (M39), once and for the rest of
    /// the run.
    ///
    /// The proxies stop being kinematic followers and become dynamic bodies
    /// wired together with joints. **`set_body_type` rather than a rebuild**:
    /// every handle, layer mask and report mapping stays valid, so the collider
    /// *set* does not change — which matters more here than tidiness, because
    /// that set is an input to rapier's broad phase and a scene that gains a
    /// body re-blesses every baseline it has.
    fn activate_ragdoll(&mut self, world: &World, index: usize, time: f32) {
        let entity = self.ragdolls[index].entity;
        if self.ragdolls[index].active {
            return;
        }
        let (Some(rig), Ok(component)) =
            (self.rig_of.get(&entity), world.get::<&RagdollData>(entity))
        else {
            return;
        };
        let Some(skin) = rig.skin.clone() else { return };
        let rig = rig.clone();

        let transform = world
            .get::<&Transform>(entity)
            .map(|t| *t)
            .unwrap_or_default();
        let model = transform.matrix();
        // The pose the character was *drawn* at this step: the bodies start
        // exactly where the picture had them, so nothing snaps on the frame a
        // ragdoll fires.
        let globals = engine_core::locomotion::posed_globals_at(world, entity, &rig, Some(time));
        let rest = engine_core::skeleton::joint_globals(&skin, None, 0.0);

        // A corpse that stops dead reads as a bug. A character with a body of
        // its own hands its velocity to every part, so a runner's ragdoll keeps
        // going — the arcade half of the milestone, and one line of it.
        let inherited = world
            .get::<&RigidBodyData>(entity)
            .map(|b| b.linear_velocity)
            .unwrap_or(Vec3::ZERO);

        // ── The bodies ────────────────────────────────────────────────
        let parts = self.ragdolls[index].parts.clone();
        let mut placement: Vec<Option<ragdoll::Placement>> = vec![None; parts.len()];
        for (slot, &part) in parts.iter().enumerate() {
            let proxy = &self.proxies[part];
            let global = globals
                .get(proxy.joint)
                .copied()
                .unwrap_or(glam::Mat4::IDENTITY);
            let pose = skinned::part_pose(model, global, proxy.local);
            placement[slot] = Some(ragdoll::Placement {
                translation: pose.translation,
                rotation: pose.rotation,
                joint: proxy.joint,
                local: proxy.local,
            });

            if let Some(body) = self.bodies.get_mut(proxy.body) {
                body.set_body_type(RigidBodyType::Dynamic, true);
                body.set_position(pose, true);
                body.set_linvel(inherited, true);
                body.set_angvel(Vec3::ZERO, true);
                body.set_linear_damping(component.linear_damping);
                body.set_angular_damping(component.angular_damping);
            }
            // rapier's own volume, not a second implementation of it: a mass
            // this crate computed and a mass rapier computed are exactly the
            // two answers a generator is warned against having.
            let proxy_body = proxy.body;
            let handles: Vec<ColliderHandle> = self
                .bodies
                .get(proxy_body)
                .map(|b| b.colliders().to_vec())
                .unwrap_or_default();
            for handle in handles {
                if let Some(collider) = self.colliders.get_mut(handle) {
                    collider.set_density(component.density);
                }
            }
            // **Explicitly, and this is not optional.** A collider's density
            // only reaches its body when the body's mass properties are
            // recomputed, and for a body that has been *kinematic* since it
            // was inserted that never happened — mass is meaningless to a
            // kinematic body, so rapier never needed it. Leaving it out gives
            // every part a near-zero mass, and the symptom is spectacular: the
            // fixture's ragdoll left the scene at about 40 m/s from a 6 N·s
            // kick, and nothing about the joints or the limits was wrong.
            let colliders = &self.colliders;
            if let Some(body) = self.bodies.get_mut(proxy_body) {
                body.recompute_mass_properties_from_colliders(colliders);
            }
        }

        // ── The joints ────────────────────────────────────────────────
        let overrides = |joint: &str| component.joints.iter().find(|o| o.joint == joint);
        let mut joints = Vec::new();
        for (slot, &part) in parts.iter().enumerate() {
            let Some(parent_slot) = self.ragdolls[index].parents.get(slot).copied().flatten()
            else {
                continue;
            };
            let (Some(child), Some(parent)) = (
                placement[slot].as_ref().cloned(),
                placement[parent_slot].as_ref().cloned(),
            ) else {
                continue;
            };

            // Anchored at the child *joint's* origin — the anatomical joint,
            // not either capsule's centre — so an elbow hinges where an elbow
            // is.
            let anchor = (model
                * globals
                    .get(child.joint)
                    .copied()
                    .unwrap_or(glam::Mat4::IDENTITY))
            .w_axis
            .truncate();
            let name = skin.joints[child.joint].name.clone();
            let joint = ragdoll::joint_between(
                parent.frame(anchor),
                child.frame(anchor),
                ragdoll::rest_relative(
                    &rest,
                    (parent.joint, parent.local),
                    (child.joint, child.local),
                ),
                overrides(&name),
                component.limit,
            );
            let parent_body = self.proxies[parts[parent_slot]].body;
            joints.push(self.impulse_joints.insert(
                parent_body,
                self.proxies[part].body,
                joint,
                true,
            ));
        }

        // ── The character's own body steps aside ──────────────────────
        //
        // A capsule left enabled holds its own corpse off the floor, which is
        // the most likely symptom of getting this wrong and reads as a bug in
        // the joints rather than as a collider nobody turned off.
        let own: Vec<ColliderHandle> = self
            .entity_of_collider
            .iter()
            .filter(|(handle, &owner)| {
                owner == entity && !self.part_of_collider.contains_key(*handle)
            })
            .map(|(handle, _)| *handle)
            .collect();
        for handle in own {
            if let Some(collider) = self.colliders.get_mut(handle) {
                collider.set_enabled(false);
            }
        }
        if let Some(&handle) = self.body_of.get(&entity) {
            if let Some(body) = self.bodies.get_mut(handle) {
                body.set_enabled(false);
            }
        }

        self.ragdolls[index].joints = joints;
        self.ragdolls[index].frozen = engine_core::ragdoll::locals_from_globals(&skin, &globals);
        self.ragdolls[index].active = true;
    }

    /// Read every active ragdoll's skeleton out of its bodies and write it into
    /// the scene (M39 §2).
    ///
    /// **This is M33's arrow reversed, and the write lands in a component.**
    /// `Ragdoll.pose` is a field of the file, exactly as `AnimationPlayer.phase`
    /// is, so a corpse baked mid-fall reloads into the same heap and every
    /// reader — the render, `list-joints`, `list-colliders`,
    /// `world.joint_position` — sees it through the seam it already used.
    ///
    /// The entity's own `Transform` follows the **root** part, so
    /// `Transform.position` keeps meaning "where the character is": culling, a
    /// script's distance check and `simulate --entity` would otherwise all be
    /// wrong about something plainly visible somewhere else. Its rotation and
    /// scale are left alone — the orientation is in the pose, where the
    /// skeleton is.
    fn write_back_ragdolls(&mut self, world: &mut World) {
        for index in 0..self.ragdolls.len() {
            if !self.ragdolls[index].active {
                continue;
            }
            let entity = self.ragdolls[index].entity;
            let Some(skin) = self.rig_of.get(&entity).and_then(|r| r.skin.clone()) else {
                continue;
            };

            // The new model matrix first: every joint global is derived through
            // its inverse, so solving before the root has moved would put the
            // whole skeleton one step behind the body it hangs on.
            let root_body =
                self.proxies[self.ragdolls[index].parts[self.ragdolls[index].root]].body;
            let Some(root) = self.bodies.get(root_body) else {
                continue;
            };
            let root_translation = root.translation();
            let model = {
                let mut transform = match world.get::<&mut Transform>(entity) {
                    Ok(mut t) => {
                        t.position = root_translation;
                        *t
                    }
                    Err(_) => Transform::default(),
                };
                transform.position = root_translation;
                transform.matrix()
            };
            let to_skin = model.inverse();

            let mut solved: Vec<Option<glam::Mat4>> = vec![None; skin.joints.len()];
            for &part in &self.ragdolls[index].parts {
                let proxy = &self.proxies[part];
                let Some(body) = self.bodies.get(proxy.body) else {
                    continue;
                };
                let world_pose =
                    glam::Mat4::from_rotation_translation(*body.rotation(), body.translation());
                // `B = M · G · L`, so `G = M⁻¹ · B · L⁻¹` — `part_pose`'s
                // arithmetic, run backwards, which is the whole reversal in one
                // line.
                if let Some(slot) = solved.get_mut(proxy.joint) {
                    *slot = Some(to_skin * world_pose * proxy.local.inverse());
                }
            }

            let pose =
                engine_core::ragdoll::solve_pose(&skin, &self.ragdolls[index].frozen, &solved);
            if let Ok(mut component) = world.get::<&mut RagdollData>(entity) {
                component.pose = Some(engine_core::ragdoll::pose_field(&skin, &pose));
            }
        }
    }

    /// Every proxy's world placement right now — the pose `engine
    /// list-colliders` reports and the solver sees, read back out of rapier
    /// rather than re-derived, so the report cannot drift from the simulation.
    pub fn proxy_placements(&self) -> Vec<ProxyPlacement> {
        let mut placements: Vec<ProxyPlacement> = self
            .proxies
            .iter()
            .filter_map(|proxy| {
                let body = self.bodies.get(proxy.body)?;
                Some(ProxyPlacement {
                    entity: self.name_of.get(&proxy.entity)?.clone(),
                    part: proxy.part.clone(),
                    position: body.translation(),
                    rotation: quat_to_euler_degrees(*body.rotation()),
                })
            })
            .collect();
        // Name-sorted, M24's contract for reports.
        placements.sort_by(|a, b| (&a.entity, &a.part).cmp(&(&b.entity, &b.part)));
        placements
    }

    /// Every collider in the world, name-sorted — what `engine list-colliders`
    /// prints.
    ///
    /// Includes the component-authored ones as well as the proxies, because
    /// "where are the colliders" is the question and answering half of it is
    /// the kind of gap this repo writes down instead of shipping.
    pub fn collider_report(&self) -> Vec<ColliderReport> {
        let mut rows: Vec<ColliderReport> = self
            .colliders
            .iter()
            .filter_map(|(handle, collider)| {
                let entity = self.entity_of_collider.get(&handle)?;
                let (shape, dimensions) = describe_shape(collider.shape());
                Some(ColliderReport {
                    entity: self.name_of.get(entity)?.clone(),
                    part: self.part_of_collider.get(&handle).cloned(),
                    shape,
                    dimensions,
                    position: collider.translation(),
                    rotation: quat_to_euler_degrees(collider.rotation()),
                    sensor: collider.is_sensor(),
                })
            })
            .collect();
        // Name-sorted, M24's contract for reports; the part orders within an
        // entity, so a humanoid's rows read the same every run.
        rows.sort_by(|a, b| (&a.entity, &a.part).cmp(&(&b.entity, &b.part)));
        rows
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
                // The part, when the collider was a proxy (M33) — carried
                // beside the entity name rather than folded into it, so a
                // pre-M33 reader of this event is unchanged.
                let mut a_part = self.part_of_collider.get(&h1).cloned();
                let mut b_part = self.part_of_collider.get(&h2).cloned();
                if (&b, &b_part) < (&a, &a_part) {
                    std::mem::swap(&mut a, &mut b);
                    std::mem::swap(&mut a_part, &mut b_part);
                }
                Some(ContactEvent {
                    a,
                    b,
                    a_part,
                    b_part,
                    started,
                })
            })
            .collect();
        events.sort_by(|x, y| {
            (&x.a, &x.b, &x.a_part, &x.b_part, x.started)
                .cmp(&(&y.a, &y.b, &y.a_part, &y.b_part, y.started))
        });
        events
    }

    /// Names of dynamic bodies, sorted — the stable row order for traces.
    pub fn dynamic_entity_names(&self, world: &World) -> Vec<String> {
        let mut names: Vec<String> = self
            .body_of
            .iter()
            .filter(|(_, &handle)| self.bodies.get(handle).is_some_and(RigidBody::is_dynamic))
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
    ///
    /// **Destructive — call this only when no further `step` follows.** A
    /// broad-phase update reports each newly overlapping collider pair
    /// exactly once, into an event list the narrow phase is supposed to
    /// consume; rapier keeps `NarrowPhase::register_pairs` private, so the
    /// events this drops are gone for good and the pairs never become
    /// contacts. The `--steps 0` query path is the only safe caller.
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
            part: self.part_of_collider.get(&handle).cloned(),
            point: ray.point_at(intersection.time_of_impact),
            normal: intersection.normal,
            distance: intersection.time_of_impact,
        })
    }
}

/// Collider shape with `Transform.scale` applied (validation already
/// rejected nonuniform scale on round shapes; mesh shapes scale per-vertex,
/// so any scale is representable). `force_events` opts the collider into
/// contact-force events — thresholded breakables only, so scenes without
/// breakables run the exact event path they always did.
///
/// Nine parameters, and they are nine independent lookups the caller has
/// already done — the component, its transform, and the four places a shape's
/// geometry can come from (its own asset, the entity's Mesh, a Terrain, or a
/// surface the entity generates). Bundling them into a struct would build that
/// struct per collider at scene load and hide which sources a given shape
/// actually consults, which is the whole subtlety of this function.
/// A surface an entity generates rather than loads, resolved before the
/// collider loop — see `build_collider`'s `generated` parameter.
struct GeneratedSurface {
    /// What generated it, for the error message naming where geometry failed.
    kind: &'static str,
    mesh: Arc<engine_core::mesh::MeshData>,
}

#[allow(clippy::too_many_arguments)]
fn build_collider(
    collider: &ColliderData,
    transform: &Transform,
    entity: &str,
    entity_mesh: Option<&str>,
    terrain: Option<&TerrainData>,
    // `road` is the entity's Road when it has one: a mesh-shaped collider with
    // no asset, no Mesh and no Terrain takes the road's generated ribbon (M23).
    // The surface this entity *generates*, when it has one: a `Road`'s ribbon
    // or a `Junction`'s patch, already built. Pre-resolved rather than rebuilt
    // here because since M40 both are functions of more than their own
    // component — a road may follow a `Terrain` it names, and a junction is
    // bounded by the roads that reach it — and physics reading the same
    // `Arc` the renderer draws is what keeps the surface driven and the
    // surface drawn from drifting apart.
    generated: Option<&GeneratedSurface>,
    meshes: &dyn MeshSource,
    layer_bits: &HashMap<String, Group>,
    force_events: bool,
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
            // Geometry comes from the explicit asset, else the entity's own
            // Mesh, else a surface the entity *generates*: its Terrain (M22) or
            // its Road (M23). Those last two are how ground and asphalt become
            // collidable without a mesh file duplicating what the renderer
            // already draws — and, for a road, they are what makes the surface
            // driven and the surface drawn impossible to author apart.
            let (asset, mesh, from_road) = match (
                collider.asset.as_deref().or(entity_mesh),
                terrain,
                generated,
            ) {
                (Some(asset), _, _) => (
                    asset.to_string(),
                    meshes.load_mesh(asset).map_err(|e| e.entity(entity))?,
                    false,
                ),
                (None, Some(terrain), _) => (
                    "the entity's Terrain".to_string(),
                    engine_core::terrain::surface_grid(
                        terrain,
                        glam::Vec2::new(transform.position.x, transform.position.z),
                        glam::Vec2::new(scale.x, scale.z),
                    ),
                    false,
                ),
                (None, None, Some(generated)) => (
                    format!("the {} on {entity:?}", generated.kind),
                    std::sync::Arc::clone(&generated.mesh),
                    // A junction's patch is road-generated geometry too:
                    // the same coplanar-triangle contact bug is waiting on
                    // it, and a car crossing a junction is exactly the case
                    // that finds it.
                    true,
                ),
                (None, None, None) => {
                    return Err(shape_bug(entity, "mesh collider with no asset in reach"))
                }
            };
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
                    // `FIX_INTERNAL_EDGES` on a road's own surface (M23), and
                    // this is not a tuning preference. Without it a body
                    // resting on a triangle mesh eventually contacts an edge
                    // *between* two coplanar triangles, takes a contact normal
                    // along that edge rather than off the surface, and is flung
                    // sideways — a ball parked on the M23 fixture sat still for
                    // two seconds and then left the road at 4.8 m/s. The flag
                    // implies `MERGE_DUPLICATE_VERTICES`, which also welds the
                    // crease vertices a road's surface and skirt do not share.
                    //
                    // **Only** road-generated geometry, deliberately. Every
                    // other trimesh keeps the flags it has had since M12,
                    // because turning this on for all of them moves an existing
                    // baseline: the ball in `verify/m22_terrain.json` comes to
                    // rest ~20 cm away, which is 1339 pixels of a fixture this
                    // milestone has no business touching. Terrain has the same
                    // latent bug and should probably take the same flag — as
                    // its own change, with its own re-blessed baseline.
                    let flags = if from_road {
                        rapier3d::geometry::TriMeshFlags::FIX_INTERNAL_EDGES
                    } else {
                        rapier3d::geometry::TriMeshFlags::empty()
                    };
                    ColliderBuilder::trimesh_with_flags(vertices, indices, flags).map_err(|e| {
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
    let events = if force_events {
        ActiveEvents::COLLISION_EVENTS | ActiveEvents::CONTACT_FORCE_EVENTS
    } else {
        ActiveEvents::COLLISION_EVENTS
    };

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
        .active_events(events)
        // Report every contact force (the engine compares impulses against
        // the break threshold itself, in one place).
        .contact_force_event_threshold(0.0)
        .active_collision_types(
            ActiveCollisionTypes::default() | ActiveCollisionTypes::KINEMATIC_FIXED,
        )
        .build())
}

/// A built shape as the name and numbers a report prints (M33).
///
/// The scene's vocabulary, not parry's: what an author typed into `shape` is
/// what comes back, so a report row can be compared against the file that
/// produced it. Anything the engine cannot build from a scene is `other`
/// rather than a panic — this is a report, and a report that crashes on an
/// unexpected input is worse than one that says it does not know.
fn describe_shape(shape: &dyn Shape) -> (&'static str, Vec<f32>) {
    match shape.as_typed_shape() {
        TypedShape::Ball(ball) => ("sphere", vec![ball.radius]),
        TypedShape::Cuboid(cuboid) => (
            "cuboid",
            vec![
                cuboid.half_extents.x,
                cuboid.half_extents.y,
                cuboid.half_extents.z,
            ],
        ),
        TypedShape::Capsule(capsule) => (
            "capsule",
            // Half the segment's length is the `half_height` the file names.
            vec![capsule.segment.length() * 0.5, capsule.radius],
        ),
        TypedShape::TriMesh(_) => ("trimesh", Vec::new()),
        TypedShape::ConvexPolyhedron(_) => ("convex_hull", Vec::new()),
        _ => ("other", Vec::new()),
    }
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
pub(crate) fn quat_to_euler_degrees(q: Quat) -> Vec3 {
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
            all_events.extend(physics.step(&mut scene.world, 0.0));
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
                physics.step(&mut scene.world, 0.0);
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
        assert!(
            low < 1.0,
            "restitution 0.0 should not meaningfully bounce, apex {low}"
        );
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
        physics.step(&mut scene.world, 0.0);
        physics.step(&mut scene.world, 0.0);

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
        assert_eq!(
            hit.entity, "Cube",
            "the cube sits on the ground, so it is hit first"
        );
        assert!(
            (hit.point.y - 1.05).abs() < 0.03,
            "top of the settled cube, got {hit:?}"
        );
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
        assert!(
            (y - 1.05).abs() < 0.02,
            "scaled cube should rest at ≈1.05, is at {y}"
        );
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
            physics.step(&mut scene.world, 0.0);
        }
        let x_before = position_of(&scene, "Car").x;
        scene
            .world
            .get::<&mut RigidBodyData>(entity)
            .unwrap()
            .linear_velocity = Vec3::new(5.0, 0.0, 0.0);
        for _ in 0..60 {
            physics.step(&mut scene.world, 0.0);
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

    /// A four-wheeled box on flat ground: the suspension must hold the
    /// chassis up (no wheel ray reaches its rest state with the chassis
    /// resting on its collider), and the wheel visuals must hang below
    /// their attachment points.
    const CAR: &str = r#"{
      "name": "car",
      "entities": [
        {"name": "Ground", "components": [
          {"type": "Transform"},
          {"type": "Collider", "shape": "cuboid", "half_extents": [50.0, 0.5, 50.0],
           "offset": [0.0, -0.5, 0.0]}
        ]},
        {"name": "Chassis", "components": [
          {"type": "Transform", "position": [0.0, 0.8, 0.0]},
          {"type": "RigidBody", "body": "dynamic", "can_sleep": false},
          {"type": "Collider", "shape": "cuboid", "half_extents": [0.8, 0.25, 1.6],
           "density": 120.0}
        ]},
        {"name": "WheelBL", "components": [
          {"type": "Transform"},
          {"type": "Wheel", "vehicle": "Chassis", "offset": [-0.8, -0.1, 1.3],
           "radius": 0.3, "suspension_rest_length": 0.4}
        ]},
        {"name": "WheelBR", "components": [
          {"type": "Transform"},
          {"type": "Wheel", "vehicle": "Chassis", "offset": [0.8, -0.1, 1.3],
           "radius": 0.3, "suspension_rest_length": 0.4}
        ]},
        {"name": "WheelFL", "components": [
          {"type": "Transform"},
          {"type": "Wheel", "vehicle": "Chassis", "offset": [-0.8, -0.1, -1.3],
           "radius": 0.3, "suspension_rest_length": 0.4}
        ]},
        {"name": "WheelFR", "components": [
          {"type": "Transform"},
          {"type": "Wheel", "vehicle": "Chassis", "offset": [0.8, -0.1, -1.3],
           "radius": 0.3, "suspension_rest_length": 0.4}
        ]}
      ]
    }"#;

    #[test]
    fn suspension_holds_the_chassis_off_the_ground() {
        let (scene, _, _) = simulate(CAR, 300);
        let y = position_of(&scene, "Chassis").y;
        // Wheel bottom at rest: chassis_y - 0.1 (offset) - rest 0.4 - radius
        // 0.3 touches ground at 0 → chassis_y ≈ 0.8 minus static sag
        // (9.81 / (4 * 24) ≈ 0.10). Riding on springs means noticeably
        // above the bottomed-out height (0.25) and below the unsagged 0.8.
        assert!(
            y > 0.45 && y < 0.78,
            "chassis should settle on its springs below 0.8, is at {y}"
        );

        // Wheel visuals hang below their attachment points by ray length,
        // i.e. between rest-length-compressed and full droop.
        let wheel = position_of(&scene, "WheelFL");
        assert!(
            wheel.y < y - 0.1 && wheel.y > 0.0,
            "wheel visual should sit between chassis and ground, is at {}",
            wheel.y
        );
    }

    /// A vehicle in the world must not cost every *other* body its contacts.
    ///
    /// The first step's suspension rays need a broad-phase BVH, which is
    /// otherwise built inside `pipeline.step`. Priming it on the real broad
    /// phase looked free and was not: an update reports each new collider
    /// pair exactly once, into events the narrow phase never got, so anything
    /// already resting in contact when the scene loaded — a crate on the
    /// ground, a stack, a wall — silently fell through the world for the rest
    /// of the run. Nothing errored; the scene validated; only the pixels were
    /// wrong. A body dropped from a height was *not* affected, which is why
    /// every fixture missed it.
    #[test]
    fn a_vehicle_does_not_break_contacts_for_bodies_resting_at_load() {
        // The box starts flush on the ground — touching, not falling.
        let scene = CAR.replace(
            r#"{"name": "WheelBL", "components": ["#,
            r#"{"name": "Crate", "components": [
          {"type": "Transform", "position": [8.0, 0.5, 0.0]},
          {"type": "RigidBody", "body": "dynamic"},
          {"type": "Collider", "shape": "cuboid", "half_extents": [0.5, 0.5, 0.5],
           "density": 60.0}
        ]},
        {"name": "WheelBL", "components": ["#,
        );

        let (settled, _, _) = simulate(&scene, 120);
        let y = position_of(&settled, "Crate").y;
        assert!(
            (y - 0.5).abs() < 0.05,
            "the crate rested on the ground at load and must still be there, is at {y}"
        );
    }

    #[test]
    fn engine_force_drives_the_chassis_forward() {
        let mut scene = Scene::from_source(CAR, "t.json").unwrap();
        let settings = PhysicsSettings::default();
        let mut physics = PhysicsWorld::build(&scene.world, &settings, &BuiltinAssets).unwrap();

        // Settle onto the springs first, then floor it.
        for _ in 0..120 {
            physics.step(&mut scene.world, 0.0);
        }
        let start = position_of(&scene, "Chassis");
        for name in ["WheelBL", "WheelBR"] {
            let entity = scene.entity(name).unwrap();
            scene
                .world
                .get::<&mut WheelData>(entity)
                .unwrap()
                .engine_force = 1500.0;
        }
        for _ in 0..120 {
            physics.step(&mut scene.world, 0.0);
        }
        let moved = start - position_of(&scene, "Chassis");
        // Positive engine force drives the chassis's local −Z.
        assert!(
            moved.z > 2.0,
            "rear-wheel drive should move the chassis toward −Z, moved {moved:?}"
        );
        assert!(
            moved.x.abs() < 0.3,
            "straight wheels should not veer, moved {moved:?}"
        );

        // The wheel visual must actually roll about its axle.
        let entity = scene.entity("WheelBL").unwrap();
        let rotation = scene.world.get::<&Transform>(entity).unwrap().rotation;
        let spun = rotation.x.abs() + rotation.z.abs();
        assert!(
            spun > 1.0,
            "a driving wheel's visual should spin: {rotation}"
        );
    }

    #[test]
    fn steering_turns_the_vehicle_left() {
        // Pitch/roll locked to isolate the yaw response: an unloaded 300 kg
        // box at full steer simply rolls over, which is physics working but
        // not what this test is about.
        let source = CAR.replace(
            r#""body": "dynamic", "can_sleep": false"#,
            r#""body": "dynamic", "can_sleep": false,
               "locked_rotations": [true, false, true]"#,
        );
        let mut scene = Scene::from_source(&source, "t.json").unwrap();
        let settings = PhysicsSettings::default();
        let mut physics = PhysicsWorld::build(&scene.world, &settings, &BuiltinAssets).unwrap();
        for _ in 0..120 {
            physics.step(&mut scene.world, 0.0);
        }
        for name in ["WheelBL", "WheelBR"] {
            let entity = scene.entity(name).unwrap();
            scene
                .world
                .get::<&mut WheelData>(entity)
                .unwrap()
                .engine_force = 600.0;
        }
        for name in ["WheelFL", "WheelFR"] {
            let entity = scene.entity(name).unwrap();
            scene.world.get::<&mut WheelData>(entity).unwrap().steering = 15.0;
        }
        for _ in 0..120 {
            physics.step(&mut scene.world, 0.0);
        }
        let position = position_of(&scene, "Chassis");
        // Starting toward −Z, positive steering curves the path toward −X.
        assert!(
            position.x < -0.5,
            "positive steering must curve the path left (−X), ended at {position:?}"
        );
        assert!(
            position.z < -1.0,
            "the car should still be moving forward while turning: {position:?}"
        );
    }

    #[test]
    fn brakes_stop_a_rolling_vehicle() {
        let mut scene = Scene::from_source(CAR, "t.json").unwrap();
        let settings = PhysicsSettings::default();
        let mut physics = PhysicsWorld::build(&scene.world, &settings, &BuiltinAssets).unwrap();
        for _ in 0..120 {
            physics.step(&mut scene.world, 0.0);
        }
        let drive = |world: &mut hecs::World, scene_entity: hecs::Entity, force: f32| {
            world
                .get::<&mut WheelData>(scene_entity)
                .unwrap()
                .engine_force = force;
        };
        let bl = scene.entity("WheelBL").unwrap();
        let br = scene.entity("WheelBR").unwrap();
        drive(&mut scene.world, bl, 1500.0);
        drive(&mut scene.world, br, 1500.0);
        for _ in 0..120 {
            physics.step(&mut scene.world, 0.0);
        }
        drive(&mut scene.world, bl, 0.0);
        drive(&mut scene.world, br, 0.0);
        for name in ["WheelBL", "WheelBR", "WheelFL", "WheelFR"] {
            let entity = scene.entity(name).unwrap();
            scene.world.get::<&mut WheelData>(entity).unwrap().brake = 15.0;
        }
        for _ in 0..240 {
            physics.step(&mut scene.world, 0.0);
        }
        let entity = scene.entity("Chassis").unwrap();
        let speed = scene
            .world
            .get::<&RigidBodyData>(entity)
            .unwrap()
            .linear_velocity
            .length();
        assert!(
            speed < 0.2,
            "braked vehicle should stop, still moving at {speed}"
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
            physics.step(&mut scene.world, 0.0);
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
            events
                .iter()
                .any(|e| e.a == "Ball" && e.b == "Track" && e.started),
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

    // ── Skinned collider proxies (M33) ────────────────────────────────
    //
    // A two-joint rig built in code rather than read from a file: these tests
    // are about what the physics world does with a pose, and a glTF in the
    // middle would only add a parser to what they can fail on.

    /// `Root` at the origin, `Head` one metre above it, and a clip that swings
    /// the head to +Z by rotating the root 90° about +X.
    fn stick_rig() -> Arc<Rig> {
        use engine_core::skeleton::{
            Channel, ChannelProperty, ChannelValues, Interpolation, Joint, SkeletalClip, SkinData,
            Trs,
        };
        let skin = SkinData {
            name: Some("Stick".into()),
            joints: vec![
                Joint {
                    node: 0,
                    name: "Root".into(),
                    parent: None,
                    rest: Trs::default(),
                    inverse_bind: glam::Mat4::IDENTITY,
                    ancestor: glam::Mat4::IDENTITY,
                },
                Joint {
                    node: 1,
                    name: "Head".into(),
                    parent: Some(0),
                    rest: Trs {
                        translation: Vec3::Y,
                        ..Trs::default()
                    },
                    inverse_bind: glam::Mat4::from_translation(-Vec3::Y),
                    ancestor: glam::Mat4::IDENTITY,
                },
            ],
        };
        Arc::new(Rig {
            skin: Some(skin),
            clips: vec![SkeletalClip {
                name: "Swing".into(),
                channels: vec![Channel {
                    node: 0,
                    node_name: None,
                    property: ChannelProperty::Rotation,
                    interpolation: Interpolation::Linear,
                    times: vec![0.0, 1.0],
                    values: ChannelValues::Quat(vec![
                        Quat::IDENTITY,
                        Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
                    ]),
                }],
            }],
        })
    }

    /// `BuiltinAssets` for geometry, one hand-built rig for anything else.
    struct StickAssets;

    impl MeshSource for StickAssets {
        fn load_mesh(&self, asset: &str) -> Result<Arc<engine_core::mesh::MeshData>> {
            BuiltinAssets.load_mesh(asset)
        }
    }

    impl engine_core::skeleton::RigSource for StickAssets {
        fn load_rig(&self, _asset: &str) -> Result<Arc<Rig>> {
            Ok(stick_rig())
        }
    }

    /// A real rigged file, so the reference checks in `Scene::from_source`
    /// pass; `StickAssets` hands back the two-joint rig above whatever the
    /// path says, because what these tests are about is the physics world's
    /// use of a pose rather than the loader's reading of a file.
    fn stick_scene(extra: &str, clip: &str) -> String {
        format!(
            r#"{{
              "name": "proxies",
              "entities": [
                {{"name": "Walker", "components": [
                  {{"type": "Transform", "position": [0.0, 0.0, 0.0]}},
                  {{"type": "Mesh", "asset": "../../examples/meshes/rigged_arm.gltf"}},
                  {{"type": "AnimationPlayer", "clip": "../../examples/meshes/rigged_arm.gltf#{clip}", "looping": false}},
                  {{"type": "SkinnedCollider", "parts": [
                    {{"joint": "Head", "shape": "sphere", "radius": 0.25}}
                  ]}}
                ]}}
                {extra}
              ]
            }}"#
        )
    }

    fn stick_world(source: &str) -> (Scene, PhysicsWorld) {
        let scene = Scene::from_source(source, "test.json").unwrap();
        let settings = PhysicsSettings::default();
        let physics = PhysicsWorld::build(&scene.world, &settings, &StickAssets).unwrap();
        (scene, physics)
    }

    #[test]
    fn a_proxy_rides_the_joint_it_names() {
        let (mut scene, mut physics) = stick_world(&stick_scene("", "Swing"));

        // At rest the head is one metre up, and the proxy is on it.
        let at_rest = physics.proxy_placements();
        assert_eq!(at_rest.len(), 1);
        assert_eq!(at_rest[0].entity, "Walker");
        assert_eq!(at_rest[0].part, "Head");
        assert!(
            (at_rest[0].position - Vec3::Y).length() < 1e-4,
            "the proxy starts at {}",
            at_rest[0].position
        );

        // One second in, the clip has swung the root 90° about +X, carrying
        // the head to +Z — and the proxy with it.
        physics.step(&mut scene.world, 1.0);
        let swung = physics.proxy_placements();
        assert!(
            (swung[0].position - Vec3::Z).length() < 1e-3,
            "the proxy must follow the pose to +Z, is at {}",
            swung[0].position
        );
    }

    #[test]
    fn a_proxy_carries_the_entity_transform() {
        // The same swing, on a character standing ten metres out and turned
        // 180°: the proxy must compose the model matrix, not just the pose.
        let source = stick_scene("", "Swing").replace(
            r#"{"type": "Transform", "position": [0.0, 0.0, 0.0]}"#,
            r#"{"type": "Transform", "position": [10.0, 0.0, 0.0], "rotation": [0.0, 180.0, 0.0]}"#,
        );
        let (mut scene, mut physics) = stick_world(&source);
        physics.step(&mut scene.world, 1.0);

        let position = physics.proxy_placements()[0].position;
        let expected = Vec3::new(10.0, 0.0, -1.0);
        assert!(
            (position - expected).length() < 1e-3,
            "expected {expected}, got {position}"
        );
    }

    #[test]
    fn a_proxy_pushes_a_dynamic_body_and_reports_the_part() {
        // A crate resting where the head swings to. The proxy is kinematic,
        // so the crate is pushed and the character is not.
        let crate_entity = r#",
                {"name": "Crate", "components": [
                  {"type": "Transform", "position": [0.0, 0.3, 1.0]},
                  {"type": "RigidBody", "body": "dynamic", "gravity_scale": 0.0},
                  {"type": "Collider", "shape": "cuboid", "half_extents": [0.25, 0.25, 0.25]}
                ]}"#;
        let (mut scene, mut physics) = stick_world(&stick_scene(crate_entity, "Swing"));

        let mut events = Vec::new();
        for step in 1..=60 {
            events.extend(physics.step(&mut scene.world, step as f32 / 60.0));
        }

        let crate_z = position_of(&scene, "Crate").z;
        assert!(
            crate_z > 1.05,
            "the swinging proxy must shove the crate along +Z, it is at {crate_z}"
        );

        // The contact names the *entity* — a proxy is not an entity — with the
        // part beside it (design §5).
        let hit = events
            .iter()
            .find(|e| e.started && (e.a == "Crate" || e.b == "Crate"))
            .expect("the crate must report a contact");
        assert_eq!(hit.a, "Crate");
        assert_eq!(hit.b, "Walker");
        assert_eq!(hit.b_part.as_deref(), Some("Head"));
        assert_eq!(hit.address_b(), "Walker/Head");
    }

    #[test]
    fn a_character_does_not_collide_with_its_own_proxies() {
        // A body on the character itself, inside its own hitbox. Without the
        // self-filter the pair resolves and the character launches.
        let source = stick_scene("", "Swing").replace(
            r#"{"type": "Transform", "position": [0.0, 0.0, 0.0]}"#,
            r#"{"type": "Transform", "position": [0.0, 1.0, 0.0]},
                  {"type": "RigidBody", "body": "dynamic", "gravity_scale": 0.0},
                  {"type": "Collider", "shape": "sphere", "radius": 0.5}"#,
        );
        let (mut scene, mut physics) = stick_world(&source);
        for step in 1..=30 {
            physics.step(&mut scene.world, step as f32 / 60.0);
        }

        let entity = scene.entity("Walker").unwrap();
        let velocity = scene
            .world
            .get::<&RigidBodyData>(entity)
            .unwrap()
            .linear_velocity;
        assert!(
            velocity.length() < 1e-3,
            "a character must not be pushed by its own hitboxes, it is moving at {velocity}"
        );
    }

    #[test]
    fn a_raycast_names_the_part_it_hit() {
        let (mut scene, mut physics) = stick_world(&stick_scene("", "Swing"));
        physics.step(&mut scene.world, 0.0);

        let hit = physics
            .raycast(Vec3::new(-5.0, 1.0, 0.0), Vec3::X)
            .expect("the ray must reach the head proxy");
        assert_eq!(hit.entity, "Walker");
        assert_eq!(hit.part.as_deref(), Some("Head"));
    }

    #[test]
    fn a_scene_with_no_proxies_builds_none() {
        // The default path, asserted rather than assumed: a world with no
        // `SkinnedCollider` has nothing to pose and nothing to filter.
        let (_, physics) = {
            let scene = Scene::from_source(DROP, "test.json").unwrap();
            let settings = PhysicsSettings::default();
            let physics = PhysicsWorld::build(&scene.world, &settings, &BuiltinAssets).unwrap();
            (scene, physics)
        };
        assert!(physics.proxy_placements().is_empty());
    }
}
