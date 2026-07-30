# Breaking objects (M14)

A model breaks into pieces when hit hard enough, when caught in an explosion, or when a
script says so. Everything stays inside the established contracts: pre-authored fragments in
scene JSON (no runtime mesh fracture), deterministic traces, baked files that reconstruct the
post-break world exactly.

## 1. The component

```json
{
  "type": "Breakable",
  "impulse_threshold": 8.0,
  "fragments": [
    { "mesh": "builtin:cube", "offset": [-0.25, 0.25, 0.0], "scale": [0.5, 0.5, 0.5] },
    { "mesh": "meshes/crate_shard.glb", "offset": [0.25, -0.25, 0.0],
      "rotation": [0.0, 30.0, 0.0], "half_extents": [0.2, 0.2, 0.2] }
  ]
}
```

- `fragments` (required, at least one): what the entity becomes. Each fragment is a mesh
  reference (same rules as `Mesh.asset`) plus a local placement: `offset` (default origin),
  `rotation` (Euler degrees, default identity), `scale` (default 1) — all relative to the
  parent entity, so the assembled fragments overlay the unbroken model. `half_extents`
  (default `[0.5, 0.5, 0.5]`, matching `builtin:cube`) is the fragment's cuboid collider in
  fragment-local units — `scale` scales it, exactly like `Transform.scale` scales a
  `Collider`. `density` (default 1, `> 0`) sets fragment mass. Cuboid-only fragment
  colliders are deliberate v1 scope: shards are boxes to the solver.
- `impulse_threshold` (optional, `> 0`): the contact impulse, in kg·m/s (≈ mass × closing
  speed), at or above which a collision breaks the entity. **Absent means collisions never
  break it** — it breaks only by script or explosion. Impulse rather than force so the
  number survives a `timestep_hz` change (rapier reports force; the engine multiplies by dt
  at the event boundary).

Pre-authored fragments (not runtime Voronoi) is the settled decision: fragments exist in the
text file, validate against the schema, and produce byte-identical runs. A future
`engine fracture` CLI could *generate* this JSON offline without changing the runtime.

## 2. What a break does

Breaking replaces the entity with its fragments, at the end of the fixed step, after
physics:

1. The entity is despawned — from hecs, from the physics world, from the name table.
2. Each fragment spawns as a full entity: `Name` (`Parent.frag0`, `Parent.frag1`, …,
   suffix-deduped if an authored entity already claims the name), `Transform` (parent
   transform composed with the fragment placement), `Mesh`, the parent's `Material` (copied
   if present), a **dynamic** `RigidBody`, and a cuboid `Collider` (friction/restitution
   defaults, the fragment's `density`).
3. Fragments inherit the parent's motion: linear velocity + angular velocity × lever arm,
   plus a radial kick when the break came from an explosion. A broken wall keeps flying
   apart the way it was moving.

Fragment entities are ordinary entities. They render, they trace, they bake, scripts can
push them around by name. There is no "debris" special case anywhere downstream.

## 3. Triggers

- **Collision**: colliders on entities with a thresholded `Breakable` opt into rapier
  contact-force events; the step's peak contact impulse per entity is compared against
  `impulse_threshold` after the step. Any body kind can break — a fixed wall breaks when a
  truck hits it.
- **Script**: `world.break_entity(name)` queues a break, applied after this step's physics.
  Unknown name or an entity with no `Breakable` is a runtime error at call time
  (deterministic failure over a silent no-op).
- **Explosion**: `world.explode(x, y, z, radius, impulse)` queues a blast. At the next
  physics step, every dynamic body within `radius` gets a radial impulse falling off
  linearly to zero at `radius` (applied before integration, so the blast moves things the
  same step); every thresholded `Breakable` within radius whose falloff impulse meets its
  threshold breaks, and its fragments get the same radial kick.

Multiple triggers in one step resolve deterministically: breaks apply once, in entity-name
order.

## 4. Determinism, traces, bake

- No `Breakable` in the scene → zero behavior change; all pre-M14 traces and baselines are
  untouched (force events are only enabled on breakable colliders, and the break phase is a
  no-op without candidates).
- A break appears in the trace as `{"step": N, "broke": "Crate", "fragments": [...]}`, and
  fragment rows join the per-step position lines from the step after their spawn (trace rows
  re-enumerate dynamic bodies each step, sorted by name — pre-existing scenes enumerate
  identically every step).
- Bake extends the change-based rule to structure: a file entity that no longer exists in
  the world is spliced out (`apply_remove_entity`); a world entity not in the file (a
  fragment) is spliced in as a full entity with its current state. A baked post-break scene
  reloads into exactly the post-break world — no hidden state.

## 5. Validation

- `fragments` empty or missing, wrong field shapes, unknown fragment fields: schema-driven
  (the walk now recurses into arrays of objects — previously they fell through to the serde
  gate, which would have misreported as `scene_parse_desync`).
- Every fragment `mesh` reference is resolved like `Mesh.asset` (existence, extension,
  relative path), and the asset pass parses file-backed fragment meshes.
- `impulse_threshold` on an entity with no `Collider` is `breakable_without_collider`
  (error: nothing can ever hit it, so the threshold is dead — script/explosion-only
  breakables just omit the threshold).
