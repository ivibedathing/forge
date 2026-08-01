# Physics (M8, `crates/engine-physics`)

*Relocated verbatim from `CLAUDE.md` when it was compacted to an index. The digest that points here is `CLAUDE.md` §Physics.*

**rapier3d pinned =0.34.0** with `enhanced-determinism` — and note rapier 0.34 switched to a
glam-based math backend sharing our exact glam version, so no conversion layer exists; read the API
in the registry when touching it, and treat any golden-trace diff on a rapier upgrade as a breaking
change to review.

`RigidBody` (dynamic/kinematic/fixed) + `Collider` (flat struct, per-shape fields enforced
semantically) + optional scene-level `physics` block (`gravity`, integer `timestep_hz`).
`Transform.scale` scales collider shapes (the fixture's ground collider is authored in *local* units
for this reason); restitution combines by **max**. Angular velocity is degrees/sec (file
convention), converted at the rapier boundary. Determinism: same file + steps → byte-identical
traces, pinned by golden `verify/baselines/m8_drop.trace.jsonl`. **Bake round-trip is state-equal
within ~1e-4, deliberately not byte-equal**: baking quantizes to Euler-degree f32 text and drops
solver caches (disposable by design). `--steps 0` scene queries need `PhysicsWorld::refresh_queries()`
(the broad-phase BVH is otherwise only built inside `step`) — it is **documented destructive**, with
the `--steps 0` query path its only safe caller. The windowed viewer steps the same fixed dt through
a wall-clock accumulator; headless is canonical. Physics tests are GPU-free and unconditional.

**Collision (M12)**, all opt-in so pre-M12 traces and baselines are untouched:
- **Script contact queries** — `world.touching(name)` / `world.contacts_started(name)` return entity
  names from the touching-state the **previous** physics step left (scripts run before physics,
  hence the one-step latency). `ContactEvent`/`ContactState` live in engine-core so engine-script
  never depends on rapier.
- **Mesh colliders** — `shape` gains `trimesh` and `convex_hull`; geometry comes from
  `Collider.asset` or, absent that, the entity's own `Mesh.asset` (neither is
  `collider_missing_mesh`). Vertices scale by `Transform.scale`; a trimesh on a **dynamic** body is
  `trimesh_on_dynamic_body` (rapier trimeshes are hollow; use `convex_hull`). `PhysicsWorld::build`
  takes a `&dyn MeshSource`.
- **Collision layers** — `layers` (membership) and `collides_with` (filter), free-form names; absent
  means "everything" (which is why empty arrays are rejected — `empty_collision_layers`), two
  colliders interact only if the filter passes **both ways**, names map to rapier
  `InteractionGroups` bits sorted-name-deterministically (max 32, `too_many_collision_layers`), and
  a `collides_with` naming a layer nobody is a member of warns `unknown_collision_layer`.
