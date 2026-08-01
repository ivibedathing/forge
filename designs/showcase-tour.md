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

The tour shows M29's `Meadow` too: a 24 × 14 m field of ground cover in front of station 04, on a
`cycle_length: 5.0` so three whole generations pass during the fifteen seconds — green at step 585,
flowering at 646. It is the tour's clearest demonstration that a recipe component can be a function
of the clock rather than a fixed shape.

**It also cost the tour its bit-exact pins.** A meadow at `samples: 4` is not byte-reproducible on
this adapter (see `meadow-design.md` §9), the tour is `samples: 4`, and the field is visible in all
six frames — removing the entity changes 875–3649 pixels in each. The tour without it renders
identically eight runs running; with it, frames move by up to 203 pixels at delta 20. So all six
`showcase_*` baselines now carry `"diff_args": ["--threshold", "24", "--max-diff-percent", "0.02"]`,
where before M29 only `showcase_646` had a tolerance at all. The strict pin on the meadow system
lives in `verify/m29_meadow.json` instead, which renders at `samples: 1` for exactly this reason.

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
stations. `scripts/tour_director.rhai` holds the whole timeline: seven camera
keys, seven aim points, the station captions, and the three timed events the
breaking station is built around. The dolly runs the full segment while the
aim stays pinned on the current station for its first two thirds, so each
station is *looked at* rather than driven past, and the camera never stops
moving.

The seventh key is the first one again, because the path is a **closed cycle**
of six legs and not five legs that stop — see [past the fifteen
seconds](#past-the-fifteen-seconds).

| steps | station | what is on screen | systems |
|-------|---------|-------------------|---------|
| 0–179 | 01 forest | nine procedural trees — two oaks, a birch, three spruces, a dead snag, two scrubs — under fissured bark, four critters running loops, a rigged figure walking a circuit in front of them, the granite monolith, its animated beacon | `Tree`, `Mesh` (builtin + glTF), **skeletal animation** (13 joints, textured), `Material` (`albedo_map`, `normal_map`, `orm_map`, `Material.asset`), `DirectionalLight`, `AmbientLight`, `AnimationPlayer`, `Script` |
| 180–359 | 02 campfire | layered additive flame, turbulent smoke, streaked embers, and firelight pooling on the grass | `ParticleEmitter` ×5 (additive, disc emission, jitter, turbulence, stretch), `PointLight`, `Material.emissive`, script-driven `rate` + `intensity` + `color` |
| 360–539 | 03 water and ice | a pond with real waves and a foam rim, a waterfall into a plunge pool, ice shelf, blocks, spire, frost | `Water` (Gerstner waves, depth absorption, foam), `Material.transmission`, `ParticleEmitter` ×3 |
| 540–719 | 04 breaking | a granite boulder rolls into a stack of planked crates, an ice pillar is broken by name, a blast finishes the rest | `RigidBody`, `Collider`, `Breakable`, `world.break_entity`, `world.explode` |
| 720–899 | 05 the whole world | high wide arc over all of it, debris settled, truck still running | `Wheel` ×4 (tread `normal_map`), `Material.orm_map`, the `HudPanel` station card, the camera |
| 900–1079 | 06 the way back | the descent home, over the burning fire and the debris field, and then all of the above again | the loop |

Running underneath all five, from the `environment` block rather than from any
component: a gradient sky with the sun in it, distance fog, sun shadows from
everything opaque, 4× MSAA, and sky reflected off the metal and the water.

The forest is nine `Tree` components and no meshes (M19). Each is a seed and a
species recipe — broadleaf, conifer, snag, scrub — so the two oaks are the same
species and visibly different individuals, which is the thing twelve
cylinder-and-sphere entities could not do however they were placed. The station
is framed around what the component gives for free: the snag is bare structure
where the branching is legible, the spruces stand behind at 7–8.6 m so the
whorled limbs read against the sky, and the scrubs are one-meter trees at the
front to show that the model scales rather than special-casing a bush.

The truck patrols a 27 m ring for the whole fifteen seconds, so no station is
a still life; `scripts/tour_truck.rhai` is a cruise-control autopilot on the
same raycast suspension the playable car demo uses. Since M23 that ring is a
**road** rather than a line on the grass: `RingRoad` is one `Road` entity whose
twelve corners round almost the whole way into their edges, so the polygon *is*
the circle the truck was already driving — asphalt, shoulders, edge lines and a
dashed centre line fitted to close on itself, and a trimesh collider that is the
same triangles you see. It is also what turns the wide shot from objects on a
lawn into a place. The critters
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

### Past the fifteen seconds

The tour is fifteen seconds long and the world it is touring is not. In
`run-scene` the clock keeps going, and it used to go somewhere silly: the
director clamped its station index at the last one, so `local` swept 0→1
forever and the camera replayed the finale's own three-second leg on repeat
while the fire burned, the truck drove and the daylight kept warming. The
symptom was the camera; the cause was that the key path had an end.

So it does not have one. The path is a cycle of six legs over seven keys —
the seventh being the first again — and `p = step % 1080` is the lap position
the whole director reads instead of the step. Leg 5 is the flight home:
900–1079 descends from the wide finale over the burning fire and the debris
field back to the forest key, and then the five stations run again.

Three properties are load-bearing:

- **The first lap is arithmetically untouched.** For `step < 1080` the
  modulo is the identity, so every expression a committed baseline was
  blessed from is the same expression evaluated on the same integer. The six
  showcase baselines diff at zero pixels across this change, which is how it
  was checked. The same care is why the time bar picks a *numerator and
  denominator* rather than scaling a fraction — `320 * x / n` and
  `320 * (x / n)` are not the same float.
- **Nothing resets.** A lap is a camera move, not a replay: the crates stay
  broken, the fragments stay where they settled, the daylight keeps
  advancing (`day_length: 300`, so lap two is dusk and lap three is night),
  and `Breakable` stays one-shot. The tour is not a loop of a film; it is a
  camera that keeps going round a world that keeps running. Station 04 on a
  later lap shows a debris field rather than a stack of crates, and that is
  the honest thing for it to show.
- **The leg is captioned like a station**, "06 THE WAY BACK / NOTHING RESET .
  THE WORLD KEPT RUNNING", because an uncaptioned three seconds reads as the
  narration having broken. The HUD line switches from `TOUR 900/900` to
  `TOUR LAP 2`, which is also what makes the lap visible to `simulate` with
  no pixels involved — `the_showcase_tour_keeps_touring_past_its_fifteen_seconds`
  pins exactly that plus the camera's return to its opening key.

The second lap has no committed baseline and should not get one. What is
worth pinning is that the camera *moves on*; what it happens to see two laps
in is a function of a world that has been running for forty-five seconds.

## The growth contract

**Every component the engine has must appear in this scene.**
`repo_contracts.rs::showcase_tour_uses_every_component_the_engine_has` reads
the generated schema's component list and the scene's component types and
fails on any name in the first that is missing from the second. It is meant
to be the test that breaks on the commit that adds a component, and the fix
is to add an entity that uses it — there is no allowlist, because an
allowlist is how a contract like this quietly stops meaning anything.

**And every scene-level block.**
`repo_contracts.rs::showcase_tour_uses_every_scene_block_the_format_has` is the
same contract one level up, added in M21 because `daylight` is a top-level
block rather than a component and the component walk would never have noticed
it missing.

M21 also put the first hole in the component contract's premise. `daylight`
with `drives_sun` makes a `DirectionalLight` a *validation error*, so the tour
cannot carry one — two components stopped being addable. The exemption is
computed from that same validation rule rather than declared as a list, so it
disappears by itself if the tour ever stops driving the sun, and the test
asserts the converse too. If a future feature makes another component
mutually exclusive, extend it the same way: derive, never enumerate.

**M30 put the second hole in it, and it has a different shape from M21's.**
Skeletal animation adds no component at all — a skin is a property of the
*asset*, and `AnimationPlayer.clip` gained a fragment form rather than a
`Skeleton` component — so the walk that a component contract keys on can never
notice that the system exists. M21's hole is an exemption the contract
computes; this one is a system the contract was never able to see. The tour
carries a rigged character anyway, because "every system running at once" is
the tour's claim and a contract that cannot see a system does not weaken it.

**M31 is the case the contract works best on**, and worth recording as the
counterexample to the two holes above: the UI system adds three components,
the test failed on the commit that added them, and the fix improved the scene
rather than decorating it. The tour's lower third *was* the problem the UI
design opens with — a 352×92 `HudRect` whose size had been solved by hand to
fit four labels and a gauge, each positioned by an offset derived from the
others, with nothing in the file saying they belonged together. It is now an
invisible `HudPanel` hugging its contents, a nine-sliced `HudImage` stretched
over exactly that box, a `row` pairing the station icon with the station name,
and — the part that reads best — the `TimeBar` fill *inside* its
`TimeBarBack` track instead of beside it at an offset that has to match. The
director script is untouched: same entity names, same `set_hud_text` and
`set_hud_rect_size` calls.

The `HudInteract` rides on the card's frame, so it brightens under the pointer
in `run-scene` and costs the baselines nothing — an absent cursor is the
*centre* of the frame (M28) and the card is bottom-left. That is luck rather
than law, and the UI fixture is the scene where it goes the other way.

When a system is bigger than one component — a renderer feature, a shader
path, a whole subsystem — it does not trip that test, so add it here by hand
and say so in the table above. Another station is cheap, with one thing to
remember since the path closed: insert its key into `eyes` and `aims` *before*
the wrap entry (they end with a copy of the first key, and it has to stay
last), its caption into `titles` and `systems` before the way-back one, and
then set `total` and `cycle` — `seg` times the station count, and `seg` times
one more than that. Keeping the run at fifteen seconds instead means shrinking
`seg`, which re-blesses all six baselines; growing the tour does not.

## What is honest and what is faked

Worth being plain about, because a showcase that quietly overclaims is worse
than no showcase:

- **Fire, smoke, sparks, spray, frost, blast, dust** are real: `ParticleEmitter`
  with scripted rates. Nothing is faked here.
- **The time of day is real** as of M21. The tour has no `DirectionalLight` and
  no `AmbientLight` at all: its `daylight` block synthesizes the sun, the
  ambient, and the sky from one number, and `day_length: 300.0` drifts the
  15-second run from 16:18 to 17:38 — late afternoon into golden hour, so the
  light warms and the shadows lengthen while the camera moves. What is faked is
  unchanged and now more visible: the sky is still a gradient with no idea where
  the sun is, so the sunset reddens all of it evenly and there is no disc.
- **The campfire casts real light** as of M17. `FireLight` is a `PointLight`, and
  `tour_effects.rhai` drives its intensity, color, and height from the same
  flicker-times-breath signal that drives the flame's emission rates and the coal
  bed's size — so the pool of firelight on the grass breathes with the flame
  instead of sitting at a fixed radius. The flame itself is five emitters: a
  white-hot `FireBase`, the `Fire` body, breakaway `FireTongues`, alpha-blended
  `FireSmoke`, and additive streaked `Sparks`. What is still missing: the light
  casts no shadows, so the logs do not throw one outward across the pit.
- **The road** is real as of M23: `RingRoad` is generated from a polygon of
  corners, its markings are painted per pixel from the road's own surface
  coordinates (so they bend with it and cannot z-fight), and the truck drives on
  the same triangles that are drawn. Still missing: junctions — a second road
  crossing this one would be a visible seam, because a crossing wants a patch
  primitive and not a ribbon.
- **Breaking** is real physics on pre-authored fragments — no runtime fracture,
  which is the settled M14 scope.
- **Water** is real as of M18: one `Water` entity where sixteen cube tiles used
  to be. It has its own waves (three Gerstner components plus per-pixel ripple
  normals), it absorbs with depth between `shallow_color` and `deep_color` so
  the middle of the pond is darker than its rim, it foams where the surface
  meets the bed and where a crest pinches, and it reflects the sky with a
  Fresnel weight. The sixteen tiles were the clearest fake in the tour: each one
  translated rigidly, so the surface normal pointed straight up everywhere and
  nothing could catch the light, and their seams were visible in every
  screenshot. Still missing: refraction — the bed shows through undistorted —
  and the trees beside the pond are not reflected in it, only the sky is.
- **Ice** is a pale dielectric at roughness 0.05–0.10 with transmission
  0.55–0.66, and the floating blocks are sorted into the same back-to-front
  list as the water they sit in. As of M26 it **refracts** — `ior: 1.31` with a
  `thickness` that scales with each block and a faint blue-green `attenuation`,
  so a thick block is not as clear as a thin one. No subsurface scattering, and
  the ice carries no texture maps deliberately: refraction is what this station
  is showing and a frost normal map competes with it for the same pixels.
- **The trees are real geometry** as of M19 — swept tubes on wandering
  polylines, recursively branched, with taper and a root flare, and a seed per
  tree so no two are the same individual — and as of the M26 material pass they
  have **surface**: bark is an `albedo_map` and a `normal_map` of fissures, and
  seven of the nine trees share one `materials/bark.json` file. The leaves are
  what is still flat: a leaf is a folded blade shaded by its own geometry, and
  `Tree::leaf_material` has no map fields to hang an alpha-cut leaf card off,
  so giving them one is an engine change and not an authoring job. No wind, no
  LOD, and no collision — you can walk the truck through a trunk.
- **Everything else with a `Mesh` is textured too**, and the interesting part is
  what the maps are *not*. Bark, crate, granite and tread all serve more than
  one entity at more than one colour, so each map is near-neutral and bright and
  the material's `albedo` carries the hue: `albedo_map` is **multiplied** by
  `albedo`, so a map with its own strong colour can only be tinted toward black
  and one bark file could not serve both an oak and a birch. The truck is the
  one entity that texture is *only* relief and reflectance — `plate_normal` for
  the panel seams and their rivets, `plate_orm` scuffing the paint's roughness,
  and the red left where the file says it. Untextured on purpose: the critters
  and the beacon (stand-ins with nothing to be a surface of), the ice, and
  `Terrain` and `Road`, whose own texture systems are generative and whose map
  support M26 did not build.
- **`builtin:cube`'s faces do not agree on which way `u` runs** — it is vertical
  on ±X and horizontal on ±Z — which is why the crates are a *framed* panel with
  a centre batten rather than plain boards: a border is invariant under that,
  and boards that change direction between the side of a crate and its end are
  what a real crate does anyway. Anything strongly directional on a cube draws
  one thing on two faces and another on the other two. `Tree` tubes are the
  well-behaved case: `u` runs around the ring and `v` along the branch, so a
  fissure is simply something that varies fast in `u`.
- **The animals** are scaled spheres on parametric loops. There is no
  navigation, no steering behaviour, no state machine — scripts have no
  randomness by design, so the variety is sums of sines.
- **The walker is really skinned** as of M30 — thirteen joints, a one-second
  `Walk` clip out of `rigged_walker.gltf`, posed on the CPU and applied to the
  vertices on the GPU, casting a shadow that walks with it because the skinned
  caster is its own pipeline. It is also the tour's only **skinned *and*
  textured** draw, which is the composition M30 rebuilt the vertex-stage seam
  for: `plate_normal` and `plate_orm` panel it, the same two maps the truck
  wears. It is in the frame for stations 01, 02, 04 and 05 (the water station's
  camera is aimed the other way), and it is placed *between* the station-01
  camera and the trees on purpose — a two-metre figure thirty metres back and
  behind a canopy is a pale smudge, and the point of putting it there is that
  the legs are legibly legs.

  Two of the three things faked about it are gone as of **M32**.
  `tour_wildlife.rhai` still carries it around a circle — the M30 division of
  labour, and correct — but the clip is now driven by the ground it covers
  (`stride: 1.6408`, measured off the clip by `engine list-joints`) rather than
  played at a rate tuned to match, so retuning the circuit cannot make the feet
  skate. And a `FootPlant` puts each foot on the hillside instead of leaving
  the root on the terrain and the feet wherever the clip put them, which on a
  slope is inside the hill. The measurement is the part worth keeping: the walk
  covers 1.64 m a cycle and the tour was carrying it at 0.88 m/s, so every
  stance foot had been sliding backwards at 0.76 m/s — a number nobody had
  because nothing could measure it.

  **M33** adds the third thing it was missing, though nothing in the tour is
  aimed at it: five `SkinnedCollider` proxies — head, chest, hips and two legs —
  so the walker is something the physics world can see rather than a pose that
  passes through everything. It walks a circle that meets nothing, and the
  entry is here because the growth contract has no allowlist. The interesting
  part was the cost: adding those five kinematic bodies moved 24 of the tour's
  26 dynamic bodies and re-blessed all six baselines, because a rapier world's
  results depend on its whole collider set and not only on what touches what.

  What is still faked: no blending and no state machine, which is M9's
  rejection standing. `Idle` is in the file and the tour never crossfades to
  it, because a crossfade is exactly the nondeterminism that made two clips on
  one property a validation error.
- **The blast** still has no light — but as of M17 that is a wiring job rather
  than a missing feature. A `PointLight` at the crate pile, pulsed for a dozen
  steps from `tour_director.rhai` (which already fires the explosion), would do
  it; the fireball is particles either way, which is the better answer for the
  bulk of the effect.
- **The clouds are real geometry** as of M20 — four `Cloud` entities, seeded
  clusters of lobes growing smaller lobes on themselves, with flat cumulus
  bases and a slow `drift` on the scene clock. What they are not is
  *positioned to be seen*: every station's camera sits at head height and aims
  **down** at its subject, so the clouds ride the horizon — a corner of one at
  station 02, a sliver above the title bar at 03, and nothing at all at 01.
  They are placed to be found rather than to dominate, and a sky-facing beat is
  what would change that.
- **The sky behind them is still a gradient, not a simulation.** Three bands
  and a sun disc — no scattering model, no time of day, and no high cloud (the
  M20 component makes objects, not a dome). Surfaces reflect the gradient,
  which is what makes the metal and the water read as they do, and they do not
  reflect the clouds: a cumulus over the pond is not in the pond.
- **One shadow map, one cascade.** It follows the camera and covers 72 m; past
  that, shadows fade to lit rather than ending on a line ruled across the
  ground. Crisp shadows near the camera *and* shadows out to the horizon would
  need cascades, which M16 does not have.

Refraction is the upgrade that would move this scene most now, and the water is
its loudest customer — the ice took it in M26 and the pond still cannot, since
`Water` has no `Material` to put an `ior` on. For the forest it is alpha-cut
leaves: the bark is textured and the canopy is the last flat surface in the
frame. For the sky it is the cloud *layer* M20 deferred:
overcast and cirrus belong to the dome, would ride into
the water reflection for free through `sky_common.wgsl`, and unlike the cloud
objects would be visible from a camera that never looks up.

## Measuring frames

The viewer draws an FPS readout in the top-right (`run-scene`, averaged over
0.5 s, wall-clock and therefore viewer-only — headless renders never see it):

```
engine run-scene examples/scenes/showcase_tour.json
```

The tour runs its fifteen seconds and then keeps touring: the camera flies
home and takes the stations round again on an eighteen-second lap, while the
truck keeps circling, the fire keeps burning and the day keeps setting. Breaks
are one-shot, so a later lap finds a debris field where the crate stack was.

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
