# Locomotion and foot planting (M32, `designs/locomotion-design.md`)

*Relocated verbatim from `CLAUDE.md` when it was compacted to an index. The digest that points here is `CLAUDE.md` §Locomotion and foot planting.*

*The design doc for this milestone is `designs/locomotion-design.md` — it has the rejected
alternatives; this file has what the build learned.*

M30's walker slid and floated, and its own design doc said so. This removes those two fakes and
nothing else. **Everything defaults to the pre-M32 behaviour** — `stride: 0` is the clock driving
the clip, and a scene with no `FootPlant` poses exactly as it did — so the A/B said **30 of 30**
comparable artifacts byte-identical, the seven exclusions being the new fixture and the six tour
frames the `Walker`'s own edit re-blessed.

- **The clip's phase is a field in the file, not state in the process, and the *bake* is what
  settled that.** The obvious implementation is an accumulator in the sim loop beside
  `ParticleSystem`; a particle is genuinely disposable and a half-finished stride is not, so a baked
  scene that dropped it would reload the walker with its legs somewhere else and the bake's promise
  — reload, re-render, bit-exactly — would be false for every walking character. So
  `AnimationPlayer` gains `stride` (metres of ground one **cycle** covers) and `phase`, the engine
  writes `phase` back after physics every fixed step, and the change-based bake splices it.
  **Nothing in the render path changed signature**: `palette_for` was already reading the component.
  The general rule this yields — *ask what the bake should contain, and the answer says whether
  something is state or data.*
- **`phase` counts cycles, not seconds**, so the locomotion system needs no clip duration and never
  opens a glTF; `local_time` converts where the duration is already in hand. It wraps into `[0, 1)`
  **on write-back**, so a long run does not bake a number whose f32 resolution has decayed.
- **Driving `AnimationPlayer.speed` from a script does not work**, and the reason is invisible until
  you look at a filmstrip: `local_time` is `t * speed`, so changing `speed` at t=10 from 1 to 2
  teleports the phase from 10 to 20. Every acceleration is a pop. Phase continuity under a changing
  rate *is* an integral, which is the whole reason a stored `phase` exists.
- **`engine list-joints` measures `stride` off the clip** (`stride.measured`, over
  `STRIDE_SAMPLES` = 64 moments, on an entity that also has a `FootPlant` so the feet are named). It
  assumes nothing about gait: over each interval the **lowest** foot is the planted one and the
  ground covered is how far it travelled in the skeleton's frame — biped, quadruped, or a hop, which
  correctly measures zero. Computing it implicitly at render time was rejected: the measurement is
  an algorithm, and an algorithm that silently set the clip rate would be a format contract, so a
  refinement would move every walking character in every baseline. **The tour's walker covers
  1.5894 m per cycle** (1.6408 before the rig's humanoid rework), against the 0.884 m/s it was being carried at — a foot travelling 0.76 m/s
  backwards through every stance, which is the fake as a number.
- **`list-joints --steps N`**, and the fixture's own test is what forced it. A stride-driven pose is
  *not* a pure function of (files, time) — its phase is what the run reached — so `--time` alone
  reports the authored pose. The first version of the slip test baked to `/tmp` to reach the stepped
  world and broke every relative asset path in the baked file (the trap this file already warns
  about). A system whose state no report can reach is what M30 §6 says not to build.
- **`FootPlant` plants against a `Terrain`, and that is a purity decision.** Raycasting the physics
  world would make the pose a function of the simulation, which is exactly what lets `list-joints
  --time` answer at all. So `ground` names an entity with a `Terrain`, checked
  (`foot_plant_ground_not_found` / `_not_terrain`), and sampling goes through M22's one
  implementation. **The stated cost: a character cannot stand on a crate.**
- **Planting re-bases the clip's authored clearance onto the terrain; it does not snap feet to the
  ground.** The target is `terrain_under_foot + max(clearance, sole)`, where clearance is the
  ankle's height above the entity's own floor plane — so on ground that matches the authoring plane
  the whole pass is a no-op, and `sole` is the closest the ankle may ever come to the surface. As
  shipped, M32 instead snapped every foot within `max_drop` of the ground *to* the ground, and
  `max_drop`'s default (0.5 m) is taller than any walk clip's swing arc: the swing foot shuffled
  along the floor (peak 2 cm against the authored 33 cm), the hips-drop pass crouched the walker
  18 cm so both extended legs could reach at once, and the result read as a bouncing crouch-shuffle
  — the "jitter" was the solver fighting the animator every frame. Two intermediate fixes were
  measured and rejected on the way to this one: a clearance-based plant *weight* band (full below
  5% of reach, released by 25%) still pulled the half-planted trailing foot down at toe-off, where
  the leg is near full extension and every centimetre comes out of the pelvis; narrowing the band
  and scaling the deficit by the weight still left a ±5 mm hip wiggle, because heel-strike and
  heel-off sit at the *same* clearance with opposite intent — height alone cannot tell them apart,
  so any height-triggered snap oscillates. Re-basing sidesteps the classification entirely. The
  flat-ground fixture `verify/m32_walk_side.json` is the oracle: its hips deviate 0.0 mm from the
  clip's own curve, and `planting_on_flat_ground_leaves_the_swing_foot_to_the_animator` pins swing
  height, stance height and hip height as numbers.
- **The solve runs in skin space**, mapping the target back through `model⁻¹` rather than mapping
  every quaternion forward — one inverse per entity per frame, and it cannot get a scale subtly
  wrong (`foot_plant_non_uniform_scale` refuses one outright). Two lessons from writing it: the
  knee's side must be chosen by **nearness to where the clip put it**, never by a fixed sign (a sign
  flips the joint the first frame a leg passes through straight), and results must be written back
  as **local** transforms with the hierarchy re-walked — editing globals detaches everything below,
  and the symptom is a foot that reaches the ground with nothing joining it to the knee. The hips
  drop first, by the largest deficit across legs; absent a `hips` joint the deficit is clamped and
  one leg stretches, which is bounded and reads as wrong.
- **The viewer shipped without the locomotion step, and every walker slid in `run-scene`.**
  `app.rs`'s fixed-step loop mirrors `simulate.rs` system for system — its comments insist the two
  "must not diverge" — but `Locomotion` was only ever built and stepped in the headless loop, so in
  the window the phase stayed 0 forever: the script carried the root while the skeleton held its
  frame-0 pose, and the same scene animated correctly under `screenshot --steps`. The fix is the
  mirror: `Simulation` carries a `Locomotion` built at construction, stepped after physics and
  before particles, **unconditionally** (a walker carried by a script alone has no physics), and
  the step-loop gate gains `!locomotion.is_empty()` so a scene with nothing but a stride-driven
  walker still runs the loop. The general lesson: when a loop's correctness rule is "mirror the
  other loop", a *new system* added to one is invisible to every diff of the other — the headless
  A/B, the baselines and all 965 tests were green while the window was wrong, because nothing
  automated looks at the window.
- Scripts get `world.animation_phase`/`set_animation_phase` and the `stride` pair — **settable**,
  unlike M30's joint getters, and the distinction is where the number lives: a joint is derived and
  would be hidden state, while these are ordinary component fields the file carries. A game with its
  own locomotion rule drives it through them rather than through a second system in the engine.
  `FootPlant.max_drop`/`max_lift`/`align` animate freely, which is how a jump *stops* planting;
  `stride` and `phase` are in `NOT_ANIMATABLE` because a clip driving its own clock is circular.

Fixture `verify/m32_locomotion.json` at `--steps 45`: two copies of `rigged_walker.gltf` crossing one
slope, one with a `FootPlant`. **The two walkers are the assertion**, M30's fixture logic — they
share a file, a mesh and a clip, so anything that made both wrong would leave them identical. It
renders at **`samples: 1`** because it needs terrain in frame (M22's rule, M29's answer), and four
consecutive renders came back as one image, so the hard pin is measured. Both claims are also pinned
**without a pixel**: a CLI test asserts each planted ankle's world Y equals `terrain-height(x, z) +
sole` to a millimetre at four moments, and another measures foot **slip** — under a centimetre over
two steps stride-driven, seven times that with the clock driving the same clip.

**The tour is not where this is visible.** Station 01's walker is thirty metres back and mostly
behind the lower-third card: the whole change moves **147 pixels** of `showcase_90` and nothing
outside the existing tolerance on the other five. All six were re-blessed anyway, since the scene
changed. Not here, deliberately: blending and crossfades (**still rejected**, and locomotion is the
usual reason engines reach for a blend tree — a gait change here is a different `clip`), root motion
(the inverse of this design; it would take `Transform.position` from scripts and physics, and there
is no third owner), planting on physics colliders, arm/hand IK and authored pole targets, and toe
joints.
