# M32 — Locomotion and Foot Planting: Design

Companion to `skeletal-animation-design.md`, which shipped the rig and named these two as the
things it faked. Where any two conflict, `agent-native-engine-design.md` wins, then the skeletal
doc, then this one.

M30's showcase `Walker` walks. It also **slides**, because the clip plays at one cycle a second
however fast the script carries the entity, and it **floats and sinks**, because the entity rides
the terrain by its root while the terrain under each foot is somewhere else. Both are named as
fakes in `designs/showcase-tour.md`. This milestone removes them, and adds nothing else.

## 1. Scope

- **Locomotion**: a clip whose phase advances with **ground covered** rather than with the clock, so
  the feet stop sliding at any speed the character is driven at.
- **Foot planting**: a two-bone IK pass that puts each named ankle on the ground under it, drops the
  hips when a leg cannot reach, and tilts the sole to the slope.

Not in scope, each with a reason:

- **Blending and crossfades.** Still rejected, not deferred (`skeletal-animation-design.md` §1,
  M9 §8). Locomotion is the usual reason engines reach for a blend tree — *walk fading into run* —
  and this milestone deliberately does the other thing: one clip, played at the rate the ground
  demands. A gait change is a different `AnimationPlayer.clip`, authored as an edit.
- **Root motion.** The inverse of this design: the clip drives the entity instead of the entity
  driving the clip. It would take ownership of `Transform.position` away from scripts and physics,
  and there is no third owner in this engine. See §9.
- **Planting on physics colliders.** The ground here is a `Terrain`, sampled through M22's one
  implementation. A raycast against the physics world would make the pose a function of simulation
  state rather than of files — see §5, which is the sharpest boundary in this document.
- **Arm/hand IK, look-at, and pole targets as authored entities.** Two-bone IK is a general solver
  and this milestone points it at feet only. `FootPlant` names what it does.
- **Gait phase-matching between two characters**, procedural stepping, and toe joints.

## 2. Locomotion: the phase is a field in the file, not state in the process

The obvious implementation of "advance the clip with distance" is an accumulator in the sim loop —
the shape `ParticleSystem` already has. It is the wrong one here, and the reason is invariant 2.

A particle's position is genuinely disposable: nothing in the file describes it, `--time` never
creates one, and a baked scene is expected to come back with no particles alive. **Where a
character is in its stride is not like that.** It is exactly as much a property of the world as
where the character is standing, and if it lives only in the process then a baked scene reloads
with the walker's legs somewhere else and the bake's promise — *a baked scene re-renders
bit-exactly* — is false for any scene with a walking character in it.

So the accumulated clip time is a component field:

```json
{ "type": "AnimationPlayer", "clip": "meshes/walker.glb#Walk", "stride": 1.62, "phase": 0.41 }
```

- **`stride`** is the metres of ground one **cycle** of this clip covers. `0.0` is the default and
  means *the clock drives the clip*, which is M30 exactly — so no committed baseline moves.
- **`phase`** is the player's own clock, in seconds, and it substitutes for scene time in the
  arithmetic already there. `local_time` was `t * speed + start_offset`; it becomes
  `phase * speed + start_offset` when `stride > 0`, wrapped or clamped by `looping` as before. One
  expression, one set of rules, and `speed` keeps meaning "play this faster" on both paths.

Per fixed step, after physics has moved everything, the locomotion system measures each
stride-driven entity's **horizontal** displacement since the previous step and advances its `phase`
by `distance / stride` cycles — `× duration` to land in seconds. Horizontal, because a character
climbing a hill still strides; unsigned, because a walk cycle played backwards is a different clip,
not a negative one.

What this buys, all of it for free rather than by additional work:

- `engine inspect` shows the phase; the editor's inspector edits it; `engine simulate --bake` splices
  it under the change-based rule that already covers `Transform` and `RigidBody`.
- **No signature changes anywhere in the render path.** `Scene::palette_for` reads the component it
  was already reading. The alternative — threading a phase table through `render_items_at` — touches
  five call sites that have no rig and cannot have one, which is the argument M30 used to give the
  palette its own entry point rather than a parameter.
- A script that wants a different locomotion rule writes `world.set_animation_phase(name, t)` and
  gets the whole mechanism, because the field is ordinary component data (invariant 5).

**The wrap is on write-back, not only on read.** A looping player's `phase` is reduced into
`[0, duration)` as it is stored, so a ten-minute run does not bake a `phase` of 812.4 whose f32
resolution has decayed to a millisecond, and the number in the file is always the one a reader can
interpret without knowing how long the run was.

### Why not just drive `speed` from a script

`world.set_animation_speed` would be one line and is the first thing to try. It does not work, and
the reason is worth writing down because it is invisible until you look at a filmstrip: `local_time`
is `t * speed`, so changing `speed` at t = 10 from 1.0 to 2.0 does not make the clip run faster from
where it was — it **teleports the phase from 10 to 20**. Every acceleration is a pop. Phase
continuity under a changing rate is exactly what an integral is, and that is the whole reason this
milestone has a stored `phase` at all.

## 3. Measuring `stride`, rather than tuning it

`stride` is the one number the author has to get right, and getting it wrong *is* the foot slide the
milestone exists to remove. Tuning it by eye against a filmstrip is what M30 already did.

So `engine list-joints` reports it. For a scene entity with both a skeletal clip and a `FootPlant`
(which is where the foot joints are named), the rig's report carries:

```json
"stride": { "measured": 1.618, "feet": ["Foot.L", "Foot.R"], "samples": 64 }
```

The measurement makes no assumption about gait. At each of `samples` moments in the cycle, the
**lowest** of the named feet is the planted one; the ground the body covers over that interval is
the negative of that foot's displacement in the entity's own frame. Summing over the cycle gives
metres per cycle for a biped, a quadruped, or anything else whose feet take turns. A hop with every
foot leaving the ground at once measures the airborne interval as zero travel, which is correct.

**The engine reports the number; the file carries it.** Computing `stride` implicitly at render time
was rejected: the measurement is an algorithm, and an algorithm that silently sets the clip rate is
a format contract in a place that does not need one — a refinement to the sampling would move every
walking character in every committed baseline. A number in a file cannot do that.

## 4. Foot planting: a post-pass on the globals, in skin space

The solver runs between `joint_globals` and `palette`, on the skin-space matrices, and is a pure
function of (skin, clip, time, entity transform, terrain). No history, no iteration count that
depends on the previous frame, nothing accumulated.

```
FootPlant {
  feet: [ { ankle: "Foot.L", chain: 2, sole: 0.06 }, … ],   // ≤ MAX_PLANTED_FEET (4)
  ground: "Ground",        // an entity with a Terrain — required, see §5
  hips: "Hips",            // optional: the joint lowered when a leg cannot reach
  max_drop: 0.5,           // metres the target may fall below the animated ankle
  max_lift: 0.5,           // and rise above it
  align: 30.0              // degrees the sole may tilt to the slope; 0 disables it
}
```

Per foot, with `M` the entity's model matrix:

1. **The target.** The animated ankle's world position `A`; its **authored clearance** `A.y`
   minus the entity's own floor plane (local y = 0, carried into the world by the model matrix),
   re-based onto the terrain under the foot's XZ, with `sole` as the closest the ankle may come to
   that terrain: `terrain + max(clearance, sole)`, clamped into `[A.y - max_drop, A.y + max_lift]`.
   On ground that matches the authoring plane this is a no-op, which is the property that keeps the
   solver from fighting the animator: the original formulation — snap to `terrain + sole` whenever
   within the clamps — dragged the swing foot along the floor for its whole arc and crouched the
   hips 18 cm (against the clip's authored 4 cm) so both legs could reach at once. The clamps stay,
   because planting is a *correction*, and a correction with no ceiling is a different animation.
2. **Into skin space.** The target is mapped back through `M⁻¹` and the whole solve happens in the
   skin's own units. Solving in world space and mapping rotations back would have to undo the
   entity's scale on every quaternion; solving in skin space costs one inverse per entity per frame
   and cannot get that wrong. (A non-uniformly scaled character is therefore not supported here, and
   `foot_plant_non_uniform_scale` says so rather than bending the leg by a factor.)
3. **The two-bone solve.** `chain: 2` means the ankle's parent (knee) and grandparent (hip) rotate.
   Lengths come from the **posed** pose, not the bind pose, so a clip that scales a limb still
   solves. The bend plane is preserved from the current knee offset; when the leg is straight enough
   that the offset is degenerate, the entity's forward (`M`'s local −Z, the engine's aiming
   convention everywhere else) picks the plane, so a knee never snaps sideways on the one frame the
   leg passes through straight.
4. **Applied as local edits, then rebuilt.** Each solved joint's new *local* transform is
   `parent_globalᵢ⁻¹ · Δ · parent_globalᵢ · localᵢ`, and the hierarchy walk runs once more. Editing
   globals in place would leave every descendant — the ankle under the knee, the toe under the ankle
   — carrying its old parent, which is the classic symptom of a detached foot.
5. **Sole alignment** rotates the ankle so its local down meets the terrain normal, clamped to
   `align` degrees. The normal is `terrain::normal_at`, the same field the ground mesh was built
   from, so a foot cannot disagree with the surface it is standing on.

**The hips drop first.** If a leg cannot reach its target even fully extended, the deficit is real
and no amount of knee straightening fixes it; the standard answer is to lower the pelvis by the
largest deficit across feet and solve the legs after. Absent `hips`, the deficit is simply clamped —
a character with one foot in a hole plants the other and stretches, which reads as wrong but is
bounded and is the author's signal to name a hips joint.

## 5. The ground is a `Terrain`, and that is a purity decision

M30's central claim is that **the pose is a pure function of (files, time)**. `list-joints --time
0.7` can say where a hand is precisely because nothing had to be simulated to find out.

A foot planted by raycasting the physics world would end that. The answer would depend on where
every dynamic body happened to be, which depends on the run, which means `list-joints --time` could
no longer answer at all and the render would stop being reproducible from the file. So planting
samples `terrain::world_height_at` — M22's single implementation, the one the collider, the mesh,
`engine terrain-height` and `world.terrain_height` all already share — and `ground` is a **required**
name validated to be an entity carrying a `Terrain` (`foot_plant_ground_not_found`,
`foot_plant_ground_not_terrain`). The `Wheel.vehicle` precedent: a component may name another
entity, and the name is checked.

The cost is stated plainly: **a character cannot plant a foot on a crate.** That is a real
limitation and the honest place for it is a named non-goal, not a raycast that quietly makes every
pose depend on the simulation.

## 6. Validation

New codes, all refusing before a device or a frame exists:

| code | what it catches |
| --- | --- |
| `foot_plant_without_skin` | a `FootPlant` on an entity whose mesh carries no skin |
| `foot_plant_ground_not_found` | `ground` names nothing |
| `foot_plant_ground_not_terrain` | `ground` names an entity with no `Terrain` |
| `unknown_joint` | an `ankle` or `hips` the rig does not have — with `did_you_mean`, matching the script runtime's joint errors |
| `foot_plant_chain_too_long` | `chain` reaches past the rig's root |
| `too_many_planted_feet` | more than `MAX_PLANTED_FEET` |
| `foot_plant_non_uniform_scale` | the entity's `Transform.scale` is not uniform (§4.2) |
| `animation_stride_without_transform` | `stride > 0` on an entity with no `Transform` to measure displacement of |

Ranges (`sole`, `max_drop`, `max_lift`, `align`, `chain`, `stride`, `phase`) are `#[schemars]`
attributes, so the schema-driven walk enforces them without a hand-written check.

## 7. Verification

- **Fixture** `verify/m32_locomotion.json`: two copies of `rigged_walker.gltf` on a **sloped**
  terrain patch, one with `FootPlant` and one without. The two walkers are the assertion, M30's
  fixture logic reused — they share a file, a mesh and a material, so anything that made both wrong
  would leave them identical, and only real planting puts one pair of feet on the slope while the
  other pair floats and sinks.
- **`samples: 1`**, and it is deliberate: the fixture needs terrain in frame, which M22's rule says
  costs a hard bit-exact pin at `samples: 4`. M29 hit the same wall with meadows and settled it the
  same way. Measured, not assumed — four consecutive renders must be one image.
- **The claim proved without a pixel**, which is the half M30 cared most about: a CLI test walks the
  planted rig with `engine list-joints --entity … --time …` and asserts each ankle's world Y equals
  `terrain-height(x, z) + sole` to within a millimetre, on a slope, at several times. Nothing about
  that reads an image.
- **Foot slip, measured**: with `stride` set from the measured value, the planted ankle's world XZ
  must move less than a centimetre between consecutive steps while it is the lower foot. That is the
  milestone's actual subject as a number.
- **The default path is untouched**: `stride: 0` and no `FootPlant` is M30's arithmetic, so the A/B
  between binaries must come back byte-identical on every committed artifact except the tour frames
  the `Walker`'s own edit re-blesses.

## 8. Build order

1. **S1 — locomotion.** `stride`/`phase`, `local_time`, the sim-loop system, the bake, the script
   getter and setter, validation, unit and CLI tests. Nothing visual, and nothing renders
   differently until a scene sets `stride`.
2. **S2 — planting.** `FootPlant`, the solver, hips drop, alignment, `list-joints` reporting the
   planted pose and the measured stride, validation.
3. **S3 — the fixture, the tour, the sweep.** The `Walker` gets both, five tour baselines re-bless,
   and CLAUDE.md and §9 below are written from what building it taught.

## 9. What building it actually taught

- **The purity question had a better answer than "make it run state".** The first sketch put the
  accumulated phase in the sim loop beside `ParticleSystem`, because that is the shape this repo
  already has for per-run state. Following it through to the bake is what killed it: particles are
  *supposed* to vanish from a baked file and a half-finished stride is not, so the field version is
  both more honest and — because `palette_for` was already reading the component — strictly less
  code. **Nothing in the render path changed signature.** The rule that generalizes: ask what the
  bake should contain, and the answer says whether something is state or data.
- **`phase` counts cycles, not seconds, and that fell out of a dependency rather than a
  preference.** Seconds would have made the locomotion system need each clip's duration, which means
  reaching an `AssetSource` from the sim loop to open a glTF per entity per step. Cycles need
  nothing: `distance / stride`. `local_time` already had the duration in hand, so the conversion
  happens in the one place it is free.
- **A stride-driven rig was unqueryable, and the fixture's own test is what found it.** `list-joints`
  had `--time` and nothing else, which is correct for a pose that is a pure function of (files,
  time) — and a stride-driven pose is not one, because its phase is what the *run* reached. The
  first version of the slip test baked to `/tmp` to get at the stepped world, which broke every
  relative asset path in the baked file (the trap CLAUDE.md already warns about). `list-joints
  --steps N` is the fix, and it belongs to the milestone rather than beside it: a system whose state
  no report can reach is exactly what §6 of the skeletal design says not to build.
- **The knee's side must be chosen by *nearness*, not by a sign.** The two law-of-cosines solutions
  are mirror images across the hip→target line. Picking one by a fixed sign works until a leg passes
  through straight, at which point the axis flips and the knee snaps backwards for a frame. Picking
  whichever solution leaves the knee nearer where the clip put it is one line and cannot flip.
- **Editing globals instead of locals detaches everything below the joint.** The symptom is a foot
  that reaches the ground with nothing joining it to the knee, and it is instant to write by
  accident because the solve *is* in global space. The fix is to convert each result back through
  the parent's global and re-run the hierarchy walk, which is why `skeleton::globals_from` exists
  as a public function now. `the_leg_stays_attached_at_every_joint` asserts the bone lengths.
- **Measuring the stride made the tour's number falsifiable.** The walker's `Walk` covers
  **1.6408 m** per cycle, and the tour was carrying it at 0.884 m/s while playing the clip at one
  cycle a second — a foot travelling 0.76 m/s backwards through every stance. That is the fake the
  tour doc named, as a number, which nobody had because nobody could measure it.
- **The tour is not where this milestone is visible**, and that is worth knowing before chasing a
  baseline. Station 01's walker is thirty metres back and mostly behind the lower-third card: the
  whole change moves **147 pixels** of `showcase_90` and nothing outside the tolerance on the other
  five frames. The fixture is the proof; the tour is the growth contract.
- **`samples: 1` again.** The fixture needs terrain in frame, so M22's rule applies and M29's answer
  applies with it. Four consecutive renders came back as one image, so the hard pin is measured
  rather than hoped for.
