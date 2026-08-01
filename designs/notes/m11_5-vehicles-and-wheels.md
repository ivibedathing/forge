# Vehicle dynamics (M11.5) and wheels (M12)

*Relocated verbatim from `CLAUDE.md` when it was compacted to an index. The digest that points here is `CLAUDE.md` §Vehicle dynamics.*

Scripts read/write `RigidBody` velocities — `world.linear_velocity`/`set_linear_velocity` (m/s) and
the `angular_velocity` pair (deg/s) — and `PhysicsWorld::step` pushes a dynamic body's component
velocity into rapier **only when it differs from what physics last wrote back** (cache in
`written_velocities`), so the deg↔rad round-trip never touches untouched runs and the M8 golden trace
stays byte-identical. `RigidBody.locked_rotations: [bool; 3]` maps to rapier `LockedAxes`.
**`world.forward(name)` is required for heading math**: XYZ Euler clamps the middle angle to ±90°, so
physics-integrated yaws past that come back as the `(±180, θ, ±180)` twin and `rotation[1]` stops
being "the yaw" (this cost a debugging session; the twin is also why `animation.rs::field_shape` only
treats arrays *of numbers* as animatable).

The `Wheel` component is one raycast-suspension wheel — it sits on its own *visual* entity (Transform
+ cylinder Mesh, **no** RigidBody/Collider of its own, enforced by `wheel_with_physics`) and names
its chassis in `vehicle` (a different entity with a dynamic RigidBody + Collider;
`wheel_vehicle_not_found` / `wheel_vehicle_invalid`). engine-physics groups wheels by chassis name
(both levels name-sorted for determinism) into rapier `DynamicRayCastVehicleController`s.

- Conventions: up +Y, forward −Z via `index_forward_axis = 2` + axle +X (drive direction is
  `normal × axle = −Z`, so positive `engine_force` is forward); positive `steering` (degrees) steers
  **left**. Suspension stiffness is **per kg of chassis mass** (static sag ≈ `9.81/(4·stiffness)`).
  `Wheel.offset` is chassis-local meters, rotated but **not** scaled by `Transform.scale`.
- Control fields (`engine_force`/`brake`/`steering`) are runtime inputs like `RigidBody` velocities:
  scripts write them (`world.set_engine_force`/`set_brake`/`set_steering` + getters), physics reads
  them each step and wakes the chassis itself (rapier only wakes on *positive* engine force).
- Physics writes each wheel entity's Transform back every step — post-step chassis pose + ray length
  + steer yaw + accumulated spin, ×`Qz(90°)` mapping the builtin cylinder's Y axis onto the axle — so
  wheels visibly bounce, steer, and roll in screenshots. Vehicle worlds call `refresh_queries()` at
  build; vehicle-free worlds skip everything, keeping M8 golden.
- **Tire model caveats** (bullet port): lateral grip is a velocity damper — side impulse =
  `0.2 · side_friction_stiffness · lateral_vel · effective_mass` per wheel per step, so the sum of
  `0.2·side_friction_stiffness` over wheels is the fraction of lateral velocity removed per step (>1
  over-corrects and glues the car); `friction_slip` is the skid clamp as a multiple of suspension
  load — its 10.5 default never saturates and a large sideslip then wipes all momentum; ≈1.0 gives a
  physical μ≈0.9 tire that slides instead of sticking.
