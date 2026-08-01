# M33 — Skinned Collider Proxies: Design

Companion to `skeletal-animation-design.md` (M30) and `locomotion-design.md` (M32), and to
`physics-design.md`, whose §12 non-goals this extends. Where any two conflict, the engine doc wins,
then the physics doc, then this one.

M30 §1 said it plainly:

> **Skinned colliders.** A skinned mesh is visual. Physics sees whatever `Collider` the entity
> carries, posed by its `Transform` and nothing else.

**This reverses that one item and nothing else.** A character in this engine can be posed, planted,
queried joint by joint and rendered — and is, to the physics world, a box that never moves an arm.
Nothing can be shot in the head, no swung limb can shove a crate, and the arena shooter's drones
are hit by comparing distances in a script because there was nothing better to compare against.

## 1. Scope

A skinned entity may carry a **`SkinnedCollider`**: a list of simple shapes, each fixed in one
joint's frame, which the physics world re-poses from the rig every fixed step. They are hit by
raycasts, they generate contact events, and they push dynamic bodies.

Not in scope, each with its reason:

- **Ragdolls.** A proxy is *driven by* the pose, never the reverse (§2). Physics taking a skeleton
  over is a different system with a different owner and its own milestone.
- **Standing on things.** A proxy is kinematic, so it holds a character up exactly as much as a
  moving wall holds up the hand that pushes it: not at all. What a character stands on is still its
  own `Collider`, and planting a foot on a crate is still M32's named non-goal.
- **Shapes that change size with the pose.** Only the proxy's *placement* follows the rig. A
  capsule whose length was solved from the posed bone would rebuild a rapier shape every step, and
  a hitbox that quietly resizes is a hitbox nobody can predict from the file. Lengths are authored;
  `engine list-joints` is how you find out what to author.
- **Automatic proxy generation** from the skin's vertex weights. A generated hitbox set is a
  derived artifact with no text form, which is invariant 1 read backwards.
- **Sensors-only, and forces-only.** Both are available: a part is solid by default and
  `sensor: true` makes it a detector, exactly as `Collider.sensor` already means. It is a
  per-*part* field rather than a per-component one, because a sword's blade and its guard want
  opposite answers.
- **Skinned *mesh* colliders** (a trimesh following the skin). The vertices live on the GPU by
  M30's central decision; hauling them back to the CPU per frame is the copy that decision exists
  to avoid.

## 2. The one-way rule, which is the whole design

**The pose drives the proxies. Nothing reads the proxies back into the pose.**

That single sentence is what keeps M30's central claim intact. `engine list-joints --time 0.7` can
answer because nothing had to be simulated to find out where a hand is; a proxy that pushed back on
the skeleton would make the pose a function of the simulation and end that. It is the same
reasoning M32 used to plant feet against a `Terrain` rather than a raycast, arriving one layer
further out: *the skeleton may be read by the physics world, and never written by it.*

So a proxy is a **kinematic position-based** body, re-posed every step from

```
world = entity_model · joint_global(pose)ᵢ · part_offset
```

where `joint_global` comes from `locomotion::posed_globals` — the same seam the render,
`engine list-joints` and `world.joint_position` already go through, so a hitbox cannot disagree
with the picture about where the head is. Planting is included by construction: a `FootPlant`
character's ankle proxies are on the ground because the pose they read is the planted one.

**One body per part, not one body with many colliders.** Colliders on a rapier body share that
body's pose; joints do not share theirs. This is also why proxies cost what they cost — a
fifteen-part humanoid is fifteen kinematic bodies — and why §6 caps the count.

## 3. The component

```json
{
  "type": "SkinnedCollider",
  "parts": [
    { "name": "Head",  "joint": "Head",     "shape": "sphere",  "radius": 0.16, "offset": [0, 0.14, 0] },
    { "name": "Chest", "joint": "Spine",    "shape": "capsule", "radius": 0.18, "half_height": 0.22 },
    { "name": "ShinL", "joint": "Shin.L",   "shape": "cuboid",  "half_extents": [0.07, 0.22, 0.07],
      "offset": [0, -0.2, 0], "rotation": [0, 0, 0] }
  ],
  "layers": ["hitbox"],
  "collides_with": ["bullet"],
  "friction": 0.5,
  "restitution": 0.0
}
```

- **`parts` is a list, `FootPlant.feet`'s shape**, and each part is the flat per-shape struct
  `Collider` already is (`shape` plus the fields that shape needs, checked semantically — M8's
  idiom, deliberately not a tagged union, because the validation walk and the editor's generated
  widgets both read flat structs best).
- **Shapes are `sphere`, `capsule` and `cuboid`** — `Collider`'s own vocabulary,
  reused rather than re-spelled, so `ColliderShapeKind` and the schema walk's
  closed-vocabulary check both come for free. `trimesh` and `convex_hull` are refused rather than
  quietly allowed: they exist to describe a specific mesh, and a proxy exists precisely because the
  specific mesh is on the GPU.
- **`offset` and `rotation` are in the joint's frame**, metres and Euler degrees, so a part is
  authored against the bone rather than against the world. A capsule's axis is **local +Y**
  (rapier's, and `builtin:cylinder`'s), which `rotation` is there to change.
- **`name` defaults to the joint's name** and is what reports call the part. It is unique within
  the component (`duplicate_collider_part`), because a report that names two things the same is
  worse than no report.
- **`layers` / `collides_with` / `friction` / `restitution` sit on the component, not the part.**
  Every part of one character wants the same filtering — "bullets hit hitboxes" is a statement
  about the character — and per-part layers would be four more strings per part to keep in
  agreement. A part that wants its own filter is a second `SkinnedCollider`… which the
  one-component-per-type rule forbids, and that is the honest cost, recorded here rather than
  designed around.
- **The entity's own `Collider` and `RigidBody` are untouched and optional.** A character with a
  capsule body keeps it; a script-driven character with no body at all still gets proxies, which is
  why `scene_has_physics` learns about this component.
- **`Transform.scale` scales the parts**, and a non-uniform scale is refused
  (`skinned_collider_non_uniform_scale`) for M32's reason: there is no honest way to put a sphere
  through it.

## 4. Self-collision, and how it costs pre-M33 scenes nothing

A character's fifteen proxies overlap each other permanently — that is what a hitbox set is — and
they overlap the entity's own `Collider` if it has one. Left alone this produces a permanent storm
of contacts and, against the owner's own dynamic body, a character that launches itself.

Kinematic bodies do not collide with each other by default, so proxy-vs-proxy is free. Proxy versus
its **owner's** body is not, and the fix is rapier's contact-pair hook: `PhysicsWorld` gains a
`PhysicsHooks` implementation that rejects a pair when both colliders trace back to the same
entity. **Only proxy colliders set `ActiveHooks::FILTER_CONTACT_PAIRS`**, so the hook is never
invoked for a scene without this component and the solver sees the same pairs it always did — the
M16 discipline applied to physics: a new feature is a branch nothing else can reach.

Contact *events* between kinematic proxies and fixed geometry, and between two characters' proxies,
are opted in with `ActiveCollisionTypes` on proxy colliders only (rapier skips both by default —
M10 hit the same wall with kinematic-vs-fixed). "Did the sword touch the shield" is the question
proxies exist to answer, and it is a kinematic-kinematic pair.

## 5. Reporting: an address is not an entity name

Invariant 4 says entities have stable names and commands target them by name. A proxy is not an
entity — it has no `Transform`, it is not in the trace's dynamic-body rows, and nothing can be
baked about it — so **the reports never put a proxy where an entity name goes**.

- **`ContactEvent` keeps `a` and `b` as entity names** and gains `a_part` / `b_part`. So
  `world.touching("Bullet")` still answers `["Walker"]`, every pre-M33 script reads unchanged, and
  the trace's `"contact": [a, b]` line is byte-identical until a part is involved (a `"parts"` key
  joins it only then).
- **`engine raycast` reports `"part": "Head"` beside `"entity": "Walker"`**, and only when the hit
  was a proxy.
- **Scripts get one new call, `world.touching_parts(name)`**, returning **addresses** of the form
  `Walker/Head` — the owner, a slash, the part. The slash is deliberate: entity names already
  contain dots (`Crate.frag0`), and an address must be unmistakably not one. With no proxies in the
  scene the call returns exactly what `world.touching` does, which is the property that makes it
  safe to reach for.
- **`engine list-colliders <scene> [--entity N] [--steps N]`** is the milestone's legibility half,
  and it reports **every collider the physics world actually contains** — component-authored and
  skinned alike — with its shape, dimensions and world placement, name-sorted. Reading it out of
  rapier's own collider set rather than re-deriving it is what makes it impossible for the report
  and the simulation to disagree, which is the failure `road-centerline` and `ui-layout` were both
  built to prevent. `--steps N` is M32's precedent: a posed proxy is not a function of the file
  alone, so the report has to be able to reach the world a run arrived at.

The claim this closes: **after N steps, `list-colliders --steps N` and `list-joints --steps N`
agree about where a part is**, which is one CLI test and no pixels.

## 6. Validation

New codes, all refusing before a device or a step exists:

| code | what it catches |
| --- | --- |
| `skinned_collider_without_skin` | the component on an entity whose mesh carries no skin |
| `unknown_joint` | a `joint` the rig does not have — reused from M32, with `did_you_mean` |
| `duplicate_collider_part` | two parts with one `name` |
| `too_many_collider_parts` | more than `MAX_COLLIDER_PARTS` (32) on one entity |
| `collider_part_shape_unsupported` | `trimesh` / `convex_hull` on a part |
| `skinned_collider_non_uniform_scale` | the entity's `Transform.scale` is not uniform |

Per-shape dimension checks (`radius` on a sphere, `half_extents` on a box) reuse `Collider`'s
existing semantic pass rather than growing a second copy of it, and ranges are `#[schemars]`
attributes so the schema-driven walk enforces them with no hand-written check. Collision-layer
rules (`empty_collision_layers`, `too_many_collision_layers`, `unknown_collision_layer`) are M12's
and apply unchanged — the layer bit assignment simply learns to look at this component too.

## 7. What changes signature, and what deliberately does not

- **`PhysicsWorld::build` takes a `&dyn RigSource`** beside its `&dyn MeshSource`, and caches the
  `Arc<Rig>` of every entity with proxies. Rigs are a property of the asset and cannot change
  mid-run; which clip plays and what phase it is at are read from the components every step.
- **`PhysicsWorld::step` takes the scene time.** A clock-driven pose needs it (a stride-driven one
  does not — `phase` is in the file), and it is passed rather than counted internally so that no
  caller can drift from the clock the render uses. The pose is sampled at the **end** of the step
  being taken, `steps · dt`, which is exactly the time the render shows after those steps: that is
  what makes §5's agreement claim true, and it is also what `set_next_kinematic_position` means.
  Every existing call site passes a time it already has; the churn is mechanical and the compiler
  finds all of it, which is the point of not hiding it behind a counter.
- **Nothing in the render path changes at all.** No shader, no pipeline, no uniform, no
  `RenderItem`. Proxies are invisible. The A/B between binaries must therefore come back
  **byte-identical on every committed artifact, including the tour**, and anything less is a bug in
  this milestone rather than an adapter story.
- **The trace and bake formats are unchanged** for scenes without proxies, and a proxy contributes
  nothing to a bake even in scenes with them: its placement is derived, and derived state is what
  the bake leaves out (M32's rule — ask what the bake should contain).

## 8. Verification

- **Fixture** `verify/m33_proxies.json`: two copies of `rigged_walker.gltf` walking the
  same clip, one carrying a `SkinnedCollider`, each with a light dynamic crate standing at chest
  height in its path. **The two walkers are the assertion** — M30's fixture logic for the third
  time — since they share a file, a mesh, a clip and a crate, so anything that made both wrong
  would leave them identical. Only real proxies knock one crate aside while the other stands
  untouched, and that difference is visible in the render *and* readable out of `simulate`'s
  `entities` array without an image.
- **No terrain in frame**, per M22's rule, so the baseline is a hard bit-exact pin, measured by
  four consecutive renders rather than assumed.
- **The claim proved without a pixel**, three ways: the crate's displacement out of `simulate`;
  `list-colliders --steps N` agreeing with `list-joints --steps N` on a part's world position to a
  millimetre; and a raycast at head height that reports `"part": "Head"` while the same ray at the
  unproxied walker reports nothing.
- **A regression test for the self-filter**: the proxied walker's own body must not accumulate
  velocity from its own hitboxes over a run that would show it immediately.
- **The tour** gets the component on its `Walker`, because the growth contract has no allowlist.
  The six showcase baselines were expected to come back unchanged — the walker circles nothing it
  can touch — and they did not; §10 records what that measured instead.

## 9. Build order

1. **P1 — the component.** `SkinnedCollider`, `MAX_COLLIDER_PARTS`, validation, schema
   regeneration. Nothing behaves differently yet.
2. **P2 — the physics.** Build proxies, pose them each step, the self-filter hook, the
   `build`/`step` signatures. Unit-tested GPU-free in `engine-physics`.
3. **P3 — the reports.** `ContactEvent` parts, the trace key, `raycast --part`,
   `world.touching_parts`, `engine list-colliders`.
4. **P4 — the fixture, the tour, the sweep.** Baseline, CLI tests, A/B against `main`, CLAUDE.md,
   and a §10 written from what building it taught.

## 10. What building it actually taught

- **A physics scene is not stable under the addition of a collider — anywhere in it.** Adding the
  `Walker`'s five proxies moved 24 of the tour's 26 dynamic bodies, the far-away `Boulder` included,
  and two showcase frames by ~1900 pixels. The walker touches none of them, so the obvious reading
  ("the hitboxes hit something") is wrong. Measured directly: dropping **one 5 cm static sphere
  200 m from anything** into an otherwise unchanged tour moves six bodies by up to 4.4 mm. The
  collider set is an input to rapier's broad phase, its traversal fixes the order contacts reach the
  solver, and float addition is not associative. So a scene that gains a body re-blesses, and the
  determinism promise is per *file*, never across an edit to one — which the re-run confirmed:
  three `simulate` runs of the edited tour are byte-identical to each other.
- **The A/B still said 31 of 31**, and the two facts sit together without contradiction: every scene
  whose *file* did not change renders identically, and the seven exclusions are the new fixture and
  the six tour frames this branch edited — about which an A/B can say nothing by construction.
- **A stride-driven character's proxies lag its render by one step, and that is causal.**
  `AnimationPlayer.phase` is advanced by the ground the entity *covered*, which is not known until
  physics has run, so what physics can aim a proxy at is the previous step's phase. It measures
  1.9 mm at the hips on the fixture's walker. M12's contact latency is the same shape and was
  settled the same way — write it down as the causal order rather than chase it — and the CLI test
  that compares `list-colliders` with `list-joints` states the residue rather than tolerating it
  silently. A clock-driven clip has no lag at all.
- **`build` needed the rigs, not just the meshes, and the seam already existed.** M30 had made
  `AssetSource` one supertrait over three so the draw list took one parameter; the physics world
  wants exactly two of those three, so `PhysicsAssets` is the same trick again and every existing
  caller of `PhysicsWorld::build` compiles unchanged. The `step` signature was the opposite call —
  the scene clock is passed explicitly and the compiler found all eighteen call sites, which is what
  a counter hidden inside the struct would have silently got wrong in the viewer.
- **Clip selection was being derived in three places before this milestone put it in one.**
  `Scene::palette_for` had it, `list-joints` reached it through the same path, and the physics world
  would have been the third copy. `locomotion::posed_globals_at` is now the single seam, and the
  reason it matters is not tidiness: a proxy that resolved a clip differently from the render would
  sit somewhere the character visibly is not, and no test in this repo would have caught it.
