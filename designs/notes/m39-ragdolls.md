# Ragdolls (M39, `designs/ragdoll-design.md`)

*The design doc for this milestone is `designs/ragdoll-design.md` — it has the rejected
alternatives; this file has what the build learned.*

M33 §2 said "the pose drives the proxies, nothing reads them back" and called it the whole design.
**This reverses that sentence for one entity, once, permanently, and nothing else.** A skinned
entity carrying a `SkinnedCollider` may also carry a `Ragdoll`; when a script fires it, the proxies
become dynamic bodies wired together with rapier joints and the skeleton becomes a report of where
they ended up.

- **The pose goes into a component field, and that is the entire answer to "where does the pose come
  from".** `Ragdoll.pose` is one local TRS per joint, written back after every step exactly as M32
  writes `AnimationPlayer.phase` back — so physics writing a skeleton costs invariant 2 nothing, and
  `list-joints --steps` still answers. **M32's rule is what settled it: ask what the bake should
  contain.** A corpse baked mid-fall has to reload into the same heap, and a pose living in
  `PhysicsWorld` would reload as a character standing in its bind pose. It works, and it is pinned:
  the fixture baked at step 75 and re-rendered at **`--steps 0`** is byte-identical to the live
  scene's step-75 baseline.
- **`rotation` in `pose` is a quaternion, `w` last — the one rotation in this format that is not
  XYZ Euler degrees.** The Traps entry is the reason: Euler clamps the middle angle to ±90°, and a
  ragdoll's joints pass it in the first second. M30 drew the same line for skeletal clips, and an
  engine-written pose is on the same side of it as a DCC tool's.
- **The seam is `locomotion::posed_globals`, the inner one — not `posed_globals_at`.** Putting the
  early return one layer out was *measured* to be wrong: `list-joints --steps` reaches
  `Scene::posed_globals` directly and cheerfully reported a corpse standing up while the render drew
  it in a heap. One function further in, and the render's palette, `list-joints`, `list-colliders`
  and `world.joint_position` all get it with no edit. M33's collapsing of clip selection into one
  place is what made a ragdoll need no new reader at all.
- **`set_body_type`, not a rebuild, and the collider set therefore does not change.** Every handle,
  layer mask and report mapping survives the handoff. That matters more than tidiness here, because
  CLAUDE.md's trap says the collider set is an input to rapier's broad phase — so unlike M33, which
  re-blessed six tour frames for adding five proxies, **M39 re-blessed nothing**: the tour's inert
  `Ragdoll` creates no body until it fires, and all six showcase frames came back at zero diff
  pixels.
- **The joint graph is derived from the skeleton and shared with the validator.** A part's parent is
  the part riding the nearest ancestor joint that also carries one, so an eleven-part humanoid wires
  itself. `engine_core::ragdoll::parent_parts` is the single implementation because a validator that
  computed a different parent from the one rapier wires would pass a scene that comes apart at the
  first step. More than one root is `ragdoll_disconnected_parts`.
- **Joint limits are measured from the *rest* pose, not from the pose at handoff.** A knee authored
  as `[-115, 0]` has to mean the same bend whether the character died standing or mid-stride, and a
  frame pair that coincided at handoff would make every ragdoll's limits depend on the frame it
  fired on. The entity's model matrix cancels out of a relative rotation, so `rest_relative` does not
  take one.
- **`GenericJointBuilder`, not `RevoluteJointBuilder`, for a hinge.** rapier's revolute builder takes
  one axis "expressed in the local-space of both rigid-bodies", which is true only when the two
  bodies share an orientation — and two bones never do.

**Nothing in the render path changed**, so the A/B said **34 of 34** comparable artifacts
byte-identical. The seven exclusions are the new fixture and the six tour frames, and here they are
excluded for an unusual reason: the base binary does not know the `Ragdoll` component, so it refuses
those scenes at *validation* rather than rendering them differently. The tour is covered better than
an A/B could anyway — `bin/verify-baselines --filter showcase` on this branch came back at **zero
diff pixels on all six**, which is the direct measurement that an inert `Ragdoll` costs a scene
nothing.

**The one that cost the debugging session, and it is not about ragdolls.** The fixture's corpse left
the scene at about **40 m/s** from a 6 N·s kick. Nothing was wrong with the joints, the limits, the
anchors or the frames: **a collider's density only reaches its body when the body's mass properties
are recomputed, and for a body that has been kinematic since it was inserted that never happened.**
Mass is meaningless to a kinematic body, so rapier never needed it, and `Collider::set_density`
alone left every part at a near-zero mass. The fix is one explicit
`RigidBody::recompute_mass_properties_from_colliders` per part at the handoff. **Any milestone that
promotes a kinematic body to dynamic inherits this**, and the symptom is spectacular enough to send
you looking at the joints first.

**Two of M33's named non-goals came with it**, because a ragdoll needs both:

- **`ColliderPart.fit: "bone"`** solves a capsule's `half_height` (or a cuboid's Y half-extent) from
  the posed distance to the joint's first child. Opt-in, absent by default, so every pre-M39 part is
  byte-for-byte what it was. M33's objection — "a hitbox that quietly resizes is one nobody can
  predict from the file" — is answered rather than overruled: `list-colliders` reports the size read
  back out of rapier, so what it *is* stays a question with a command. The shape is rebuilt only past
  `FIT_EPSILON` (0.5 mm), so a rig whose clips animate rotation only — every clip in this repo —
  crosses it once and never again. **Fitting freezes at the handoff**: a ragdoll's pose comes *from*
  the proxies, so fitting a proxy to that pose would be circular.
- **`engine fit-colliders`** solves a whole proxy set from the skin's vertex weights, assigning each
  vertex to the joint holding its largest weight. M33 refused *runtime* generation as "a derived
  artifact with no text form, which is invariant 1 read backwards" and was right; this is the same
  computation as a command whose output is JSON an author edits, with `--write` splicing through the
  editor's own `formatter` path and keeping the existing `layers`/`friction`/`restitution`. Nothing
  at load time or step time consults a vertex weight. **`cuboid` is the default** because the
  bucket's bounding box needs no axis guessed; `capsule` on a stubby rig picks a near-tied dominant
  axis and returns a capsule that is nearly a sphere — correct and useless.

Scripts get `world.ragdoll`, `world.is_ragdoll` and `world.ragdoll_impulse(name, part, x, y, z)`.
The first two are an ordinary component read and write, so the handoff bakes like any other state;
the third is queued like an explosion and refused with a located error on a character that has not
ragdolled, because an impulse to a kinematic proxy is a call that appears to work and does nothing.
**There is no pose setter** — M30's rule, that a script-written joint is hidden state — so moving one
goes through the solver, where the limits still apply. `world.is_ragdoll` is not decoration: the
fixture's script uses it to *stop carrying* the character, and a script that kept driving
`set_position` would drag the corpse along the floor by its pelvis, which reads as a ragdoll that
never fired.

Fixture `verify/m39_ragdoll.json` at `--steps 75`: two copies of `rigged_walker.gltf` with identical
eleven-part proxy sets, one carrying a `Ragdoll` the script fires at step 40. **The two walkers are
the assertion** (M30's fixture logic for the fourth time) — they share a file, a mesh, a clip and a
proxy set, so anything that made both wrong would leave them identical. **Step 75 rather than the end
of the run**: a settled corpse is a flat pile, and the frame where the milestone is legible is the
one still falling. No terrain in frame per M22's rule, so it carries a hard bit-exact pin measured by
four consecutive identical renders. Four claims are pinned without a pixel: the ragdoll's root falls
and its twin's does not; `list-colliders --steps` and `list-joints --steps` agree on the hips to a
millimetre — **M33's test with its arrow reversed, and exactly rather than approximately, because a
ragdolled pose has no stride latency**; the bake round-trips bit-exactly; and a `fit: "bone"` thigh
reports 0.12 where its authored twin shin reports the file's 0.15.

Not here, deliberately: getting up (a return path is either a blend, still rejected, or a hard snap
that can be added later without changing anything here), partial ragdolls (an upper body simulated
while the legs walk is a per-joint *partition* rather than a weighted blend, so it does not reopen
M9 §8 — but the boundary joint needs an owner), motors and therefore active ragdolls that resist a
hit, self-collision inside one ragdoll (adjacent hitboxes overlap permanently by construction, so
turning it on is a contact storm at every joint), and ragdolls without a `SkinnedCollider`.
