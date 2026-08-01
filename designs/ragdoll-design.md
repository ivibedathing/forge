# M39 — Ragdolls: Design

Companion to `skinned-collider-design.md` (M33), `locomotion-design.md` (M32) and
`skeletal-animation-design.md` (M30). Where any two conflict, the engine doc wins, then the physics
doc, then this one.

M33 §2 said it in one sentence, and called it the whole design:

> **The pose drives the proxies. Nothing reads the proxies back into the pose.**

**This reverses that sentence, for one entity at a time, permanently, and nothing else.** It is the
third time a character milestone has reversed exactly one earlier claim — M33 reversed M30 §1, M32
removed two fakes M30's own doc admitted to — and the discipline is the same: name the sentence, turn
it over, and say what it costs.

Everything else M33 settled stands. Proxies are still kinematic while a character is animated; a
scene with no `Ragdoll` reaches none of this; the self-filter, the addresses, `list-colliders` and
the layer rules are untouched.

## 1. Scope

A skinned entity carrying a `SkinnedCollider` may also carry a **`Ragdoll`**. While it is inactive
the character animates exactly as it did in M33. When a script fires it, the entity's proxies become
**dynamic** bodies wired together with rapier joints, and the skeleton is driven by where they end
up — for the rest of the run.

Also in scope, both of them M33's named non-goals and both now reachable because a ragdoll needs
them:

- **Proxies that resize with the posed bone**, opt-in per part (§7).
- **A proxy set solved from the skin's vertex weights** — as a command that writes text into the
  scene file, never as a runtime behaviour (§8).

Not in scope, each with its reason:

- **Getting up.** The handoff is one-way (§3). A return path is either an interpolation — which is
  blending, rejected since M9 §8 and leaned on by M30, M32 and this doc — or a hard snap, which is a
  visible pop and can be added later without changing anything here.
- **Partial ragdolls.** An upper body simulated while the legs keep walking is a per-joint partition
  of pose ownership. It is not a weighted blend and so does not reopen M9 §8, but it needs a rule for
  the boundary joint that this milestone does not have to invent to ship a death.
- **Motors, and therefore active ragdolls.** rapier's joint motors are right there and a
  motor-driven ragdoll that resists a hit is the natural next milestone; it is also the thing that
  turns "where does the pose come from" back into a two-owner question.
- **Self-collision inside one ragdoll** (§5). Kept off, deliberately, and the reason is that adjacent
  hitboxes overlap permanently by construction.
- **Ragdolls without a `SkinnedCollider`.** The bodies *are* the proxies (§4). A character with no
  hitboxes has nothing to fall.
- **Skinned *mesh* colliders.** M30's central decision put the vertices on the GPU; that is still
  where they are.

## 2. Where the pose comes from — the question this milestone exists to answer

M33 could hold its one-way rule because the pose stayed a pure function of (files, time). Reversing
the rule looks like it must cost that, and this is the design's whole content: **it does not, because
the pose becomes part of the files.**

`Ragdoll` carries a `pose` field: one local TRS per joint of the rig, written back by the physics
world after every step, exactly as M32 writes `AnimationPlayer.phase` back after every step.
`locomotion::posed_globals_at` consults it **before** it looks at a clip, so the render,
`engine list-joints`, `engine list-colliders` and `world.joint_position` all see the ragdolled
skeleton through the seam they already share, and none of them learns that ragdolls exist.

**M32's rule is what settled it: ask what the bake should contain, and the answer says whether
something is state or data.** A ragdoll halfway to the floor, baked and reloaded, must land in the
same heap. A pose that lived in the `PhysicsWorld` would reload as a character standing up in its
bind pose, and the bake's promise — reload, re-render, bit-exactly — would be false for every corpse
in every scene. So the pose is data, it is in the file, and invariant 2 never comes under strain.

What this buys, stated as the claim to verify: **`engine list-joints --steps 200` and the render at
step 200 agree about a ragdolled character**, for the same reason they agreed about a planted one.

What it costs, stated plainly:

- **`--time T` alone stops being a complete answer** for a ragdolled entity — it reports the pose the
  file carries rather than a function of `T`, because there is no longer a clip playing. That is
  M32's `--steps` precedent one step further out: a stride-driven pose was already not a function of
  time alone.
- **The scene file gets bigger when it is baked.** Thirteen joints is about ninety numbers. A
  hundred-joint rig is the ceiling `MAX_JOINTS` already sets, and it is a bake, which is a machine
  artifact.

### Rotation in `pose` is a quaternion, and that is the trap avoided

Every other rotation an agent types into a scene file is XYZ Euler degrees, and this one is not.
`CLAUDE.md` names the reason under Traps: **XYZ Euler clamps the middle angle to ±90°**, so a
physics-integrated orientation past that comes back as the `(±180, θ, ±180)` twin. A ragdoll's joints
go past it in the first second. M30 already drew this line for skeletal clips — the distinction is
*who wrote the numbers* — and an engine-written pose is on the same side of it as a DCC tool's.

So `pose` entries are `{"joint": "Head", "translation": [x,y,z], "rotation": [x,y,z,w]}`, `w` last,
matching glTF and rapier both. `scale` rides along only when it is not 1: a ragdoll does not scale
bones, so what is in the field is whatever the clip had at the moment of handoff.

## 3. The handoff is one-way, and it is an edit to a component

A script calls `world.ragdoll("Walker")`. That sets `Ragdoll.active` — a plain bool in the file — and
the next physics step does the work: proxies switch body type, joints are created, the entity's own
collider is disabled, and the first `pose` is written back.

- **`active` is authorable.** A scene may ship with `"active": true` and its character is a corpse
  from step 0. That is what makes a fixture possible without a script, and it is the same courtesy
  every other component extends.
- **Firing twice is a no-op**, and so is firing on an entity already active from the file. The
  script call is idempotent because the state it sets is a bool, not an event.
- **There is no `world.unragdoll`.** One-way, per §1.
- **The clip keeps playing, and nothing reads it.** `AnimationPlayer` is left alone rather than
  removed: removing a component mid-run is a structure edit the bake would have to describe, and an
  animation whose output nobody consults costs a clip sample per frame that the pose check now skips
  anyway. A character that ragdolls and is later un-ragdolled by some future milestone finds its
  clip where it left it.

## 4. The bodies are the proxies

The authored `SkinnedCollider` parts become the ragdoll's rigid bodies. One shape list in the file,
and **the hitbox you shot is the body that falls** — which is the property that makes a hit react
where it landed, and the reason this beat a separate `Ragdoll.bones` list.

- **`set_body_type` rather than a rebuild.** The bodies, colliders, handles, layer masks and the
  `entity_of_collider` / `part_of_collider` maps all already exist and stay valid, so a handoff
  perturbs the collider *set* not at all — which matters more here than tidiness, because
  `CLAUDE.md`'s trap says the collider set is an input to the broad phase and a scene that gains a
  body re-blesses. A ragdoll gains none.
- **Mass comes from the shape's volume and `Ragdoll.density`** (kg/m³, `Collider.density`'s unit,
  defaulting to 985 — a shade under water, which is roughly what a person is). Authoring a mass per
  part was rejected for the reason M8 rejected it on `Collider`: a density is one number that stays
  right when a shape is resized, and a mass is fifteen numbers that quietly stop matching the
  hitboxes they belong to.
- **The joint graph is derived from the skeleton, not authored.** Part B's parent is the part riding
  the *nearest ancestor joint of B's joint that also carries a part* — so an eleven-part humanoid
  wires itself, and a part list that skips the spine still connects the head to the pelvis. Nothing
  in the file repeats what the rig already says (invariant: assets are referenced, never restated).
- **Exactly one root part**, the one with no proxied ancestor. Two roots is a ragdoll in two pieces,
  and `ragdoll_disconnected_parts` refuses it at validation rather than at the first step.
- **Joints are spherical with cone limits by default**, `Ragdoll.limit` degrees (45 by default),
  anchored at the child joint's origin expressed in each body's frame. A per-joint entry overrides
  the limit, or replaces the cone with a **hinge** — `{"joint": "Knee.L", "hinge": [1,0,0], "range":
  [-120, 0]}` — which is the difference between a ragdoll that reads as a body and one that reads as
  a bag. This is the "as real as possible" half; the "as arcade as possible" half is that both
  numbers are in the file and an author tunes them without touching Rust.
- **The entity's own `Collider` is disabled on handoff**, and its `RigidBody` stops being synced from
  the `Transform`. A character capsule left enabled holds its own corpse off the floor, which is the
  most likely symptom of getting this wrong and reads as a bug in the joints.

### The entity's `Transform` follows the root

The mesh is drawn at `model · joint_global · vertex`, so a pose alone could carry a corpse across the
room while `Transform.position` sat where the character died. Everything that asks where an entity is
— `simulate --entity`, culling, `world.position`, a script's distance check — would then be wrong
about a thing plainly visible somewhere else.

So **physics writes the entity's `Transform` from the root part's body**, and the pose is expressed
relative to it exactly as it always was. `Transform.position` keeps meaning "where the character is".
The alternative — pinning the transform and letting the pose carry everything — was rejected on that
one sentence.

## 5. Self-collision stays off, and it is a choice rather than an omission

M33's `SelfFilter` rejects every contact pair whose two colliders trace back to one entity, and a
ragdoll keeps it. A hitbox set overlaps itself permanently — a forearm proxy is inside an upper-arm
proxy at every elbow angle — so turning self-collision on means either authoring shapes that never
touch, which no hitbox set does, or accepting a permanent contact storm at every joint, which is how
a ragdoll explodes at the first step.

Limbs therefore pass through each other. That is the standard arcade answer, it is what the settling
behaviour below depends on, and the honest cost is that a corpse can end up with an arm inside its
chest. Recorded here rather than discovered later.

## 6. Settling, and what keeps a ragdoll from jittering forever

`Ragdoll.linear_damping` / `angular_damping` (0.05 and 0.6 by default) are on the bodies, and
rapier's own sleeping does the rest: once every body in the set is below the sleep threshold the
solver stops integrating them, the pose stops changing, and the write-back sees no movement. A
settled corpse costs nothing per step and bakes as a fixed pose.

The damping defaults are the arcade dial. They are deliberately higher than a physically-honest
value: a real body tumbles longer than a game wants to watch, and the number that fixes it is in the
file.

## 7. Proxies that resize with the posed bone

M33 refused this outright: "a hitbox that quietly resizes is a hitbox nobody can predict from the
file." That objection is answered rather than overruled, in three parts.

- **It is opt-in per part.** `ColliderPart.fit: "bone"`, absent by default, and a part without it
  behaves exactly as it did in M33 — same shape, same size, same rapier handle, byte for byte.
- **The size is a function of the rig, and the rig is in the file.** A `fit: "bone"` capsule takes
  its `half_height` from the posed distance between its joint and the joint's first child, less the
  radius; a cuboid takes its Y half-extent the same way. The authored `half_height` becomes the value
  used when the joint has no child, which is what a hand or a head has.
- **What it actually is, is askable.** `engine list-colliders` reports dimensions read back out of
  rapier, not re-derived — so "what size is that hitbox at step 200" was already a question with a
  command, and that is what makes a resizing hitbox predictable in the sense M33 wanted.

**The shape is rebuilt only when the length moves past `FIT_EPSILON` (0.5 mm).** A rig whose clips
animate rotation only — which is every clip in this repo — never rebuilds a shape after the first
step, so the cost of the feature on the scenes that have it is one comparison per part per step.

Why it matters at all, given that most clips do not stretch bones: a **ragdoll** does. A `fit` part
is how a proxy set generated for a rest pose keeps fitting a character whose joints the solver is
moving, and it is why this item shipped with ragdolls rather than as polish on M33.

## 8. A proxy set from vertex weights: `engine fit-colliders`

M33's refusal here was sharper — "a generated hitbox set is a derived artifact with no text form,
which is invariant 1 read backwards" — and it is correct about runtime generation. So this is not
runtime generation.

```
engine fit-colliders <scene.json> [--entity Name] [--shape capsule|cuboid|sphere] [--write]
```

It loads the skin, assigns every vertex to the joint holding its **largest** weight, and fits a shape
to each bucket's extents in that joint's own frame. It prints a complete `SkinnedCollider` component
as JSON on stdout. With `--write` it splices that component into the scene file, the editor's splice
discipline (M7), leaving every other byte of the file alone.

**The scene file still says everything.** The generator runs when an author asks it to, its output is
text a human can edit afterwards, and nothing at load time or step time consults a vertex weight. A
regenerated set that differs from the committed one shows up as a diff, which is the property
invariant 1 is protecting.

Rejected: doing this inside `engine import`. A proxy set is a *choice* about a character, and folding
it into the importer would mean every imported rig arrives with hitboxes nobody asked for, in a
scene file that got bigger for it.

## 9. Scripts

- **`world.ragdoll(name)`** — fire it. Idempotent, one-way, takes effect on the physics step that
  follows the script (M10's ordering, and M12's one-step latency for the same reason).
- **`world.is_ragdoll(name)`** — a bool, so a script can stop steering a corpse.
- **`world.ragdoll_impulse(name, part, x, y, z)`** — a kick to one hitbox, addressed by the part name
  `world.touching_parts` already returns. This is the arcade half of the milestone in one call: the
  drone you shot in the head snaps its head back. Refused with a located error on an entity that is
  not ragdolled, because the alternative is an impulse that silently does nothing.

No setter for the pose. A script-written joint is hidden state (M30's rule, M21's before it), and
`ragdoll_impulse` is the sanctioned way to move one — through the solver, where the joint limits still
apply.

## 10. Validation

| code | what it catches |
| --- | --- |
| `ragdoll_without_proxies` | `Ragdoll` on an entity with no `SkinnedCollider` — the bodies *are* the proxies |
| `ragdoll_disconnected_parts` | more than one root part: a ragdoll in two pieces |
| `ragdoll_unknown_joint` | a `joints` entry naming a joint no part rides, with `did_you_mean` |
| `ragdoll_duplicate_joint` | two overrides for one joint |
| `ragdoll_bad_hinge` | a `hinge` axis of zero length, or a `range` whose min exceeds its max |
| `collider_part_fit_unsupported` | `fit: "bone"` on a sphere — a sphere has no length to solve |

Ranges (`density > 0`, `limit` in `[0, 180]`, damping `>= 0`) are `#[schemars]` attributes, so the
schema-driven walk enforces them with no hand-written check. `unknown_joint` from M32/M33 still
covers the parts themselves.

## 11. What changes signature, and what deliberately does not

- **`PhysicsWorld::step` already takes the scene time and the world** — the write-back needs a
  `&mut World`, which it already has. No signature moves for the ragdoll itself.
- **`locomotion::posed_globals_at` grows one early return**, reading `Ragdoll.pose` before it resolves
  a clip. Every caller is unchanged, which is the entire benefit of M33 having put clip selection in
  one place; had the three copies still existed, a ragdoll would have posed correctly in the render
  and stood up in `list-joints`.
- **Nothing in the render path changes at all.** No shader, no pipeline, no uniform, no `RenderItem` —
  a ragdolled character is an ordinary skinned draw with different numbers in its palette. So the A/B
  between binaries must come back **byte-identical on every committed artifact**, and anything less
  is a bug in this milestone rather than an adapter story.
- **The trace format is unchanged.** A ragdoll's bodies are proxies, proxies are not entities, and
  the trace's rows are entities. What moves in the trace is the character's own `Transform`, which is
  a row it already had.

## 12. Verification

- **Fixture** `verify/m39_ragdoll.json`: two copies of `rigged_walker.gltf` with identical proxy sets,
  walking into an identical obstacle, one carrying a `Ragdoll` that a script fires on contact. **The
  two walkers are the assertion** — M30's fixture logic for the fourth time — since they share a
  file, a mesh, a clip, a proxy set and an obstacle, so anything that made both wrong would leave
  them identical. Only a working ragdoll drops one into a heap while the other walks on.
- **No terrain in frame**, per M22's rule, so the baseline is a hard bit-exact pin — measured by
  repeated renders rather than assumed.
- **The claim proved without a pixel**, four ways: the ragdolled walker's `Transform` falls and the
  other's does not, out of `simulate --entity`; `list-colliders --steps N` and `list-joints --steps N`
  agree on a part's world position to a millimetre, *now with physics as the source*, which is the
  M33 test with its arrow reversed; a bake at mid-fall reloads to the same pose; and a `fit: "bone"`
  part's reported dimensions track the bone while a plain part's do not.
- **The tour** gets a `Ragdoll` on a character, because the growth contract has no allowlist. The six
  showcase frames re-bless — a scene edit, per M33 §10.

## 13. Build order

1. **P1 — the components.** `Ragdoll`, `ColliderPart.fit`, validation, schema regeneration, the
   generated component reference. Nothing behaves differently yet.
2. **P2 — the physics.** Handoff, the joint graph, the write-back, the transform follow, damping and
   sleeping. Unit-tested GPU-free in `engine-physics`.
3. **P3 — the pose seam and the scripts.** `posed_globals_at`'s early return, `world.ragdoll` and its
   two companions, `fit: "bone"` resizing.
4. **P4 — `engine fit-colliders`**, with `--write` splicing.
5. **P5 — the fixture, the tour, the sweep.** Baseline, CLI tests, A/B against `main`, CLAUDE.md, and
   a §14 written from what building it taught.

## 14. What building it actually taught

The full account is `designs/notes/m39-ragdolls.md`. The four that changed a decision:

- **The seam is `posed_globals`, the inner one.** §11 said `posed_globals_at` and §11 was wrong:
  `engine list-joints --steps` reaches `Scene::posed_globals` directly, so a ragdoll whose early
  return sat one layer out drew a corpse in the render and reported it standing up. Measured, not
  reasoned about.
- **A kinematic body's mass properties are never computed, so promoting one to dynamic and setting a
  collider density is not enough.** The fixture's corpse left the scene at 40 m/s from a 6 N·s kick
  and nothing about the joints was wrong. One explicit
  `recompute_mass_properties_from_colliders` per part fixes it. This is the milestone's most
  transferable finding and it is not about ragdolls at all.
- **§4's `set_body_type` bet paid more than predicted.** Because the collider set does not change on
  a handoff, the tour's inert `Ragdoll` re-blessed **nothing** — all six showcase frames came back at
  zero diff pixels, where M33's five added proxies had moved 24 of 26 dynamic bodies.
- **`cuboid` is `fit-colliders`'s default, not `capsule`.** §8 assumed capsules; on a rig whose bones
  are stubby the dominant axis is a near-tie and the fit returns a capsule that is nearly a sphere.
  A bounding box guesses no axis.

One scope note: §12's fixture fires the ragdoll at a fixed step rather than on contact, and renders
at step 75 rather than at the end of the run. A settled corpse is a flat pile; the frame the
milestone is legible in is the one still falling.
