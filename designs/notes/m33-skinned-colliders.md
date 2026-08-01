# Skinned collider proxies (M33, `designs/skinned-collider-design.md`)

*Relocated verbatim from `CLAUDE.md` when it was compacted to an index. The digest that points here is `CLAUDE.md` §Skinned collider proxies.*

*The design doc for this milestone is `designs/skinned-collider-design.md` — it has the rejected
alternatives; this file has what the build learned.*

M30 §1 said a skinned mesh is visual and physics sees only the entity's own `Collider`. **This
reverses that one item and nothing else**: a `SkinnedCollider` lists simple shapes, each fixed in
one joint's frame, and the physics world re-poses them from the rig every fixed step. They are hit
by raycasts, they report contacts, and they push dynamic bodies.

- **The pose drives the proxies and nothing reads them back.** That one sentence is the design.
  A proxy is a **kinematic** body, posed from `locomotion::posed_globals_at` — the same seam the
  render, `engine list-joints` and `world.joint_position` go through, so a hitbox cannot disagree
  with the picture about where a head is, and a `FootPlant` character's ankle proxies are on the
  ground for free. Physics reading the skeleton is what keeps M30's "the pose is a pure function of
  (files, time)" true; physics *writing* it would be a ragdoll, which is a different milestone.
  **A proxy therefore holds a character up exactly as much as a moving wall holds up the hand
  pushing it** — what a character stands on is still its own `Collider`.
- **One body per part** (colliders on one rapier body share its pose; joints do not), `sphere` /
  `capsule` / `cuboid` only — `Collider`'s own vocabulary, and a mesh shape is refused
  (`collider_part_shape_unsupported`) because a proxy exists precisely because the skinned mesh is
  on the GPU. `offset`/`rotation` are in the **joint's** frame (capsule axis local +Y);
  `Transform.scale` scales the parts and a non-uniform one is refused. Shapes never resize with the
  pose: only the placement follows the rig.
- **Self-collision is a contact-pair hook keyed on the owning entity**, and only proxy colliders set
  `ActiveHooks`, so a scene without one reaches none of it. Proxies also opt into
  `ActiveCollisionTypes::all()` — kinematic-vs-fixed and kinematic-vs-kinematic are both off by
  default in rapier, and "did the sword touch the shield" is the second of those.
- **An address is not an entity name.** `ContactEvent` keeps `a`/`b` as entity names and gains
  `a_part`/`b_part`, so every pre-M33 script and both golden traces are untouched and a trace line
  grows a `"parts"` key only when a proxy is involved. `engine raycast` reports `"part"` beside
  `"entity"`, and scripts get `world.touching_parts` / `contacts_started_parts` returning
  **addresses** (`Walker/Head`) — engine-produced, never accepted back, slash-separated because
  entity names already contain dots (`Crate.frag0`). With no proxies in a scene those two calls
  return exactly what `touching`/`contacts_started` do.
- **`engine list-colliders`** is the milestone's legibility half and reports *every* collider,
  component-authored and skinned alike, read back out of rapier rather than re-derived —
  `road-centerline`'s argument applied to physics. `--steps N` for M32's reason.
- **`PhysicsWorld::build` takes `&dyn PhysicsAssets`** (M30's `AssetSource` trick again: one
  supertrait, every existing caller unchanged) and **`step` takes the scene time** — passed, not
  counted internally, so no caller can drift from the clock the render uses. The pose is sampled at
  the step's **end**, `steps · dt`, the time the render draws after those steps.

**Two things measured, both worth knowing before debugging one of them.** First, **a physics scene
is not stable under the addition of a collider anywhere in it**: the tour's `Walker` gained five
proxies that touch nothing, and 24 of the tour's 26 dynamic bodies moved, two frames by ~1900
pixels. Dropping *one 5 cm static sphere 200 m from anything* into the unchanged tour moves six
bodies by up to 4.4 mm — the collider set is an input to the broad phase, its traversal fixes the
order contacts reach the solver, and float addition is not associative. So the determinism promise
is per *file*: a scene that gains a body re-blesses, and three runs of the edited tour are still
byte-identical to each other. The A/B said **31 of 31** comparable artifacts byte-identical, the
seven exclusions being the new fixture and the six tour frames this scene edit re-blessed.
Second, **a stride-driven character's proxies lag its render by one step** (1.9 mm at the hips on
the fixture): `phase` is advanced by ground *covered*, which physics cannot know until it has run,
so the pose a proxy can be aimed at is the previous step's. M12's contact latency, in another
place — causal, not a defect, and the CLI test that compares `list-colliders` with `list-joints`
states the residue rather than hiding it.

Fixture `verify/m33_proxies.json` at `--steps 150`: two copies of `rigged_walker.gltf` walking into
two identical crates, one carrying an eleven-part proxy set. **The two walkers are the assertion**
(M30's fixture logic for the third time) — one bulldozes its crate 1.3 m, the other walks straight
through its own, which is visible in the render and readable out of `simulate` without one. Aimed
at its subject with no terrain in frame per M22's rule, so it carries a hard bit-exact pin (four
consecutive renders were one image), and a CLI test diff-renders it. Not here, deliberately:
ragdolls, standing on a proxy, shapes solved from the posed bone, automatic proxy generation from
vertex weights, and skinned *mesh* colliders.
