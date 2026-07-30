# The showcase tour

`examples/scenes/showcase_tour.json` is a fifteen-second camera move around a
world where everything the engine can do is happening at once. It is a demo,
and it is also a standing test: the scene every new system has to appear in,
and the scene the frame budget is measured against.

Run it:

```
engine run-scene   examples/scenes/showcase_tour.json     # watch it, with the FPS readout
engine screenshot  examples/scenes/showcase_tour.json --out f.png --steps 646
engine simulate    examples/scenes/showcase_tour.json --steps 900
engine diff-render examples/scenes/showcase_tour.json \
    examples/scenes/verify/baselines/showcase_646.png --steps 646
```

## The shape of it

The scene opens with an `environment` block, which is where every M16
rendering feature is turned on:

```json
"environment": {
  "sky": true,
  "sky_zenith": [0.10, 0.24, 0.58],
  "sky_horizon": [0.40, 0.54, 0.72],
  "sky_ground": [0.09, 0.11, 0.09],
  "fog_density": 0.0045,
  "shadows": true,
  "shadow_distance": 72.0,
  "samples": 4
}
```

Every field of that block defaults to off, which is the reason M16 landed
without re-blessing a single baseline outside this scene: a file that says
nothing about its environment renders byte for byte as it did before the block
existed. `sky_horizon` doubles as the fog color deliberately — fog that does
not match the sky it fades into reads as a grey wall, and one field cannot be
set inconsistently with itself.

900 steps at 60 Hz — exactly fifteen seconds — split into five 180-step
stations. `scripts/tour_director.rhai` holds the whole timeline: six camera
keys, six aim points, the station captions, and the three timed events the
breaking station is built around. The dolly runs the full segment while the
aim stays pinned on the current station for its first two thirds, so each
station is *looked at* rather than driven past, and the camera never stops
moving.

| steps | station | what is on screen | systems |
|-------|---------|-------------------|---------|
| 0–179 | 01 forest | nine procedural trees — two oaks, a birch, three spruces, a dead snag, two scrubs — four critters running loops, the glTF monolith, its animated beacon | `Tree`, `Mesh` (builtin + glTF), `Material`, `DirectionalLight`, `AmbientLight`, `AnimationPlayer`, `Script` |
| 180–359 | 02 campfire | layered additive flame, turbulent smoke, streaked embers, and firelight pooling on the grass | `ParticleEmitter` ×5 (additive, disc emission, jitter, turbulence, stretch), `PointLight`, `Material.emissive`, script-driven `rate` + `intensity` + `color` |
| 360–539 | 03 water and ice | a pond of sixteen tiles on a travelling wave, a waterfall into a plunge pool, ice shelf, blocks, spire, frost | low-roughness metallic PBR, `ParticleEmitter` ×3, scripted transforms |
| 540–719 | 04 breaking | a boulder rolls into a crate stack, an ice pillar is broken by name, a blast finishes the rest | `RigidBody`, `Collider`, `Breakable`, `world.break_entity`, `world.explode` |
| 720–899 | 05 the whole world | high wide arc over all of it, debris settled, truck still running | `Wheel` ×4, `HudText`, `HudRect`, the camera |

Running underneath all five, from the `environment` block rather than from any
component: a gradient sky with the sun in it, distance fog, sun shadows from
everything opaque, 4× MSAA, and sky reflected off the metal and the water.

The forest is nine `Tree` components and no meshes (M18). Each is a seed and a
species recipe — broadleaf, conifer, snag, scrub — so the two oaks are the same
species and visibly different individuals, which is the thing twelve
cylinder-and-sphere entities could not do however they were placed. The station
is framed around what the component gives for free: the snag is bare structure
where the branching is legible, the spruces stand behind at 7–8.6 m so the
whorled limbs read against the sky, and the scrubs are one-meter trees at the
front to show that the model scales rather than special-casing a bush.

The truck patrols a 27 m ring for the whole fifteen seconds, so no station is
a still life; `scripts/tour_truck.rhai` is a cruise-control autopilot on the
same raycast suspension the playable car demo uses. The critters
(`tour_wildlife.rhai`) are kinematic bodies on a `critter` collision layer
that only collides with `ground` — they share a world with 24 loose fragments
and cannot touch one.

### The three ways to break something

Station 04 is the one place all of `Breakable`'s triggers fire in one shot,
in the order the design doc lists them:

- **step 545** the director gives `Boulder` 13 m/s of velocity; it reaches the
  stack around step 585 and the contact impulse clears the threshold.
- **step 600** `world.break_entity("IcePillar")` — a break with no collision
  behind it at all. It lands on step 601, the step after the call.
- **step 636** `world.explode(-1, 0.9, 15, 6.5, 210)` takes whatever the
  boulder left, and a full-sphere emitter runs at 900/s for twelve steps
  behind it.

Which crate each trigger claims is float-level detail that moves between
optimisation levels; the CLI test pins the *sequence*, not the casualties.

## The growth contract

**Every component the engine has must appear in this scene.**
`repo_contracts.rs::showcase_tour_uses_every_component_the_engine_has` reads
the generated schema's component list and the scene's component types and
fails on any name in the first that is missing from the second. It is meant
to be the test that breaks on the commit that adds a component, and the fix
is to add an entity that uses it — there is no allowlist, because an
allowlist is how a contract like this quietly stops meaning anything.

When a system is bigger than one component — a renderer feature, a shader
path, a whole subsystem — it does not trip that test, so add it here by hand
and say so in the table above. A sixth station is cheap: extend `eyes`,
`aims`, `titles` and `systems` in `tour_director.rhai` by one entry each and
change `seg` so the run still totals fifteen seconds.

## What is honest and what is faked

Worth being plain about, because a showcase that quietly overclaims is worse
than no showcase:

- **Fire, smoke, sparks, spray, frost, blast, dust** are real: `ParticleEmitter`
  with scripted rates. Nothing is faked here.
- **The campfire casts real light** as of M17. `FireLight` is a `PointLight`, and
  `tour_effects.rhai` drives its intensity, color, and height from the same
  flicker-times-breath signal that drives the flame's emission rates and the coal
  bed's size — so the pool of firelight on the grass breathes with the flame
  instead of sitting at a fixed radius. The flame itself is five emitters: a
  white-hot `FireBase`, the `Fire` body, breakaway `FireTongues`, alpha-blended
  `FireSmoke`, and additive streaked `Sparks`. What is still missing: the light
  casts no shadows, so the logs do not throw one outward across the pit.
- **Breaking** is real physics on pre-authored fragments — no runtime fracture,
  which is the settled M14 scope.
- **Water** is sixteen tiles that a script moves on a sine wave, and since M16
  it is genuinely transmissive: `Material.transmission` at 0.82, so the pond
  bed and anything in it show through, and the Fresnel term turns the far edge
  reflective while the near edge stays clear. Still missing: refraction (the
  bed shows through undistorted) and absorption with depth, so the water is
  equally clear however deep it is. The tiles now **abut instead of
  overlapping** — an overlap is invisible between opaque tiles and reads as a
  grid of seams between transmissive ones, because pixels in the overlap are
  blended through two surfaces.
- **Ice** is a pale dielectric at roughness 0.05–0.10 with transmission
  0.55–0.66. No subsurface scattering and no tinting by thickness, so a thick
  block is exactly as clear as a thin one.
- **The trees are real geometry** as of M18 — swept tubes on wandering
  polylines, recursively branched, with taper and a root flare, and a seed per
  tree so no two are the same individual. What is missing is surface: there is
  no bark texture and no leaf texture (the engine has neither), so bark is a
  flat brown dielectric and a leaf is a folded blade that gets its variation
  from shading alone. No wind, no LOD, and no collision — you can walk the
  truck through a trunk.
- **The animals** are scaled spheres on parametric loops. There is no
  navigation, no steering behaviour, no state machine — scripts have no
  randomness by design, so the variety is sums of sines.
- **The blast** still has no light — but as of M17 that is a wiring job rather
  than a missing feature. A `PointLight` at the crate pile, pulsed for a dozen
  steps from `tour_director.rhai` (which already fires the explosion), would do
  it; the fireball is particles either way, which is the better answer for the
  bulk of the effect.
- **The sky is a gradient, not a simulation.** Three bands and a sun disc — no
  clouds, no scattering model, no time of day. Surfaces reflect it, which is
  what makes the metal and the water read as they do, but nothing else in the
  world knows it is there beyond the hemispheric tint it puts on
  `AmbientLight`.
- **One shadow map, one cascade.** It follows the camera and covers 72 m; past
  that, shadows fade to lit rather than ending on a line ruled across the
  ground. Crisp shadows near the camera *and* shadows out to the horizon would
  need cascades, which M16 does not have.

Refraction is the upgrade that would move this scene most now; textured bark
and alpha-cut leaves are what would move the forest most, and both are the same
missing feature — the renderer has no texture-mapped materials yet.

## Measuring frames

The viewer draws an FPS readout in the top-right (`run-scene`, averaged over
0.5 s, wall-clock and therefore viewer-only — headless renders never see it):

```
engine run-scene examples/scenes/showcase_tour.json
```

The tour runs its fifteen seconds and then keeps going: the camera parks at
the last key, the truck keeps circling, the fire keeps burning. Breaks are
one-shot, so it does not loop.

Two headless numbers are worth watching alongside it, because they separate
simulation cost from frame cost:

```
time engine simulate   examples/scenes/showcase_tour.json --steps 900     # sim only, no GPU
time engine screenshot examples/scenes/showcase_tour.json --out /tmp/f.png --steps 900
```

## Baselines

Six frames are committed under `examples/scenes/verify/baselines/`:
`showcase_90` (forest), `_270` (campfire), `_450` (water), `_585` (the moment
before impact), `_646` (the fireball), `_810` (the wide finale) — one per
station plus the two beats of the break. They are 640×360 and blessed with
`engine screenshot`, like every other baseline in the repo.

They are **per-adapter artifacts** and are deliberately not pinned by a CLI
test: check them with `engine diff-render` on the machine that blessed them,
and re-bless when the framing changes. The CPU-deterministic properties —
byte-identical traces across runs, the break sequence, the final HUD line,
and that nothing ever falls out of the world — are what the CLI tests pin.

## Why the floor check is in the tests

`the_showcase_tour_runs_fifteen_deterministic_seconds` asserts that no body
ends the run below y = -1. That looks like belt and braces; it is there
because building this scene found a real bug that nothing else in the repo
could see.

Priming the broad-phase BVH before the first step — which vehicle worlds did,
so wheel rays would find ground on step 0 — consumed the broad phase's
collider-pair events, and rapier keeps `NarrowPhase::register_pairs` private,
so those pairs never became contacts. Anything already *resting* in contact
when the scene loaded fell through the world for the rest of the run. A body
dropped from a height was unaffected, which is exactly why every existing
fixture missed it: they all drop things. The tour stacks them.

The fix builds that first-step BVH on a scratch copy
(`engine-physics/src/lib.rs`, `bvh_cold`), and
`a_vehicle_does_not_break_contacts_for_bodies_resting_at_load` is the unit
test. `PhysicsWorld::refresh_queries` is now documented as destructive: the
`--steps 0` query path is its only safe caller.
