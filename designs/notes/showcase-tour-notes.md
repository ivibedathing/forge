# Showcase tour (`designs/showcase-tour.md`)

*Relocated verbatim from `CLAUDE.md` when it was compacted to an index. The digest that points here is `CLAUDE.md` §Showcase tour.*

*The design doc for this milestone is `designs/showcase-tour.md` — it has the rejected
alternatives; this file has what the build learned.*

`examples/scenes/showcase_tour.json` is a 15-second (900-step) camera move through five 180-step
stations — forest / campfire / water+ice / breaking / wide — with every system running at once, plus
four scripts (`scripts/tour_{director,wildlife,effects,truck}.rhai`) and seven 640×360 baselines
(per-adapter, checked by hand with `diff-render`, not by a CLI test). The seventh, `showcase_1150`,
is on the lap rather than in the fifteen seconds: it is the village mid-build (M50).

**The camera path is a closed cycle, not a timeline that ends.** Eight legs over nine keys (the
ninth is the first again), read through `p = step % 1440`, so past step 900 the camera flies west to
the hamlet, watches it build itself (M50), and comes home on a 24-second lap — the director used to
clamp its station index, which replayed the finale's own three seconds forever while the world went
on moving. **Nothing resets on a lap**: breaks stay one-shot, so station 04 later shows a debris
field, and `day_length: 300` means lap two is dusk. The village is the exception and deliberately
so — it is cleared and re-solved every lap, from a seed that advances with the *step* rather than
the lap position, so it comes out different each time.

The first lap is *arithmetically* the pre-loop one (`step % 1440` is the identity below 1440, and
the time bar picks a numerator and denominator rather than scaling a fraction). **`total` stays
900** through M50 for the same reason: the tour proper is still five stations in fifteen seconds
and the village is on the way home, so the HUD counter's denominator — the one thing a new leg
could have moved in an already-blessed frame — does not move.

**The village needs two legs, not one.** The aim only swings to a new subject at 62% of a leg, so a
village *reached* in one leg is a village already finished by the time it is looked at. Leg 5 is the
approach and leg 6 is the crossing, and the crossing is the only leg in the tour whose two aim keys
are the same point — a constant aim is what keeps the hamlet centred for the three seconds it takes
to solve. Rhai's
function-expression depth budget is **16 in a debug build**, which is why the director spells
sub-expressions into `let`s instead of nesting one more paren.

**Its growth contract is a test**: `repo_contracts.rs::showcase_tour_uses_every_component_the_engine_has`
fails on any schema component the tour does not use, so a new component's commit adds an entity here
— there is no allowlist, deliberately. `showcase_tour_uses_every_scene_block_the_format_has` sits
beside it, since `daylight` is a block the component walk could never have seen missing. M21 put the
first hole in the component contract's premise, because `drives_sun` makes `DirectionalLight` a
validation error and two components stopped being addable; that exemption is *computed* from the
validation rule, not listed, so it evaporates if the tour stops driving the sun. **M30 put the second
hole in it, with a different shape**: skeletal animation adds no component at all (a skin is a
property of the asset), so a contract keyed on components can never notice the system exists — M21's
hole is an exemption the contract computes, this one is a system it was never able to see. The tour
carries a rigged `Walker` anyway, because "every system running at once" is the claim.

The tour shows M26 too: the ice **refracts** — `ior: 1.31` with a thickness that scales with the
block and a faint blue-green `attenuation` — and **every `Mesh` in it is textured**, from the four
new map sets `examples/textures/make_textures.py` generates beside the granite: fissured `bark` on
the nine trees and the campfire logs, a framed-panel `crate`, `plate_normal` + `plate_orm` panelling
the truck, and `tread_normal` on its wheels. Granite serves the monolith, the boulder, the fire pit
and the breaking pad at four `uv_scale`s. Four authoring rules came out of it:

- **The maps are near-neutral and bright, and the `Material` carries the hue**, because `albedo_map`
  is *multiplied* by `albedo` — a map with its own strong colour can only be tinted toward black, so
  one bark file could not serve an oak and a birch both. This is why `granite.png` was already grey.
- **Seven of the nine trees share `examples/materials/bark.json`**, the tour's use of `Material.asset`.
  Birch and the dead snag stay inline: same maps, different tint, and `asset` is exclusive with every
  other field so a shared file cannot be tinted per entity.
- **`builtin:cube`'s faces disagree on which way `u` runs** — and they disagree **in pairs, not in
  axes**: `mesh.rs` builds them as `quad(+X, Y, Z)` / `quad(−X, Z, Y)` / `quad(+Z, X, Y)` /
  `quad(−Z, Y, X)`, so `u` is vertical on +X and *horizontal* on −X. Anything strongly directional on
  a cube therefore draws differently on all four sides, and a box's tiling is a property of the
  **face** you care about rather than of the box (the arena shooter's four perimeter walls carry four
  different `uv_scale`s for exactly this reason — see `designs/arena-shooter.md`, whose
  title/pause/end menu was **rebuilt on M31**: a `HudPanel` tree the engine lays out and a
  `HudInteract` it hit-tests, where M28's version was rectangles the script computed, centred by
  multiplying a string's length by the glyph advance, and hover-highlighted by putting brackets
  round the label. What that bought is worth reading as a worked example of the UI system —
  hug-sized cards that are three different sizes for three screens, `visible` in place of the
  empty-string/zero-size pair, a play HUD authored *hidden* so `--steps 0` is the title screen, and
  a demo director that asks `engine ui-layout` where the button is instead of hard-coding the
  fraction. **M36 turned that menu into a five-screen shell** — Settings, Save, Load and Quit on a
  column of seven labelled slots — put a seventeen-joint rig where a cylinder and a sphere used to
  stand in for the player, and gave it three weapons hung off `HandR` through `world.joint_position`
  (M30's sanctioned prop pattern, and the first use of it in the repo). See
  `designs/notes/m36-game-shell.md`. It also carries the one trap in `HudImage`: with no `slice` an image is all middle band
  and the middle band **tiles**, so an icon must be drawn at its source size — 32 px of a 16 px
  reticle is four reticles. Its demo timeline is authored by a closed-loop director,
  `make_arena_demo.py`, because nobody can hand-write which *pixel* is on a drone at step 431, and
  M31's **press capture** means that director now writes the release on the button too. Four
  `Meadow` strips ring the plateau since the same pass, and it now runs a **four-level campaign**
  whose shape is dictated by two engine rules — a scene cannot spawn entities, and a script cannot
  move a *dynamic* body by writing its Transform — so every level's drones and barrels are authored
  parked above the arena and fly or drop in when their level starts, each level four metres above
  the last because they reuse each other's positions and a shared park altitude has physics shoving
  two bodies apart all run. **The performance bug it fixed is worth knowing outside the arena: a
  full-frame `HudRect` defeats M15's scissored HUD rasterizer** and puts the frame back to filling a
  window-sized CPU canvas — measured at 1920×1080, six frames went 13.1 s with a full-screen menu
  veil against 5.7 s with a card-sized backdrop, which in a debug viewer stepping physics through a
  wall-clock accumulator reads as a game that has stopped responding).
  The crate texture is a *framed* panel with a centre batten for that reason: a border is invariant
  under it. `Tree` tubes are the well-behaved case (`u` around the ring, `v` along the branch), which
  is also why bark fissures must vary in `u` — transposed, a trunk wears tyre tread.
- **The ice is deliberately unmapped**: refraction is what station 03 is showing and a frost normal
  map competes for the same pixels. So are the critters and the beacon, which are stand-ins.

Those edits are why the six showcase baselines were re-blessed — the sweep confirmed the other 25
held bit-exactly, since no engine code was touched.

**M30 adds the `Walker`** to station 01: sixteen joints out of `examples/meshes/rigged_walker.gltf`
playing a one-second `Walk`, carried around a circuit by `tour_wildlife.rhai` while the clip does the
legs — the milestone's division of labour, since no script ever touches a joint. It is the repo's
first **skinned × textured** draw (`plate_normal` + `plate_orm`, the truck's maps), which is the
composition the vertex-stage seam was rebuilt for. (The station-02 `Soldier` is the second — see
below.) It stands *between* the station-01 camera and the
trees deliberately: a two-metre figure thirty metres back behind a canopy is a pale smudge. Five of
the six showcase baselines were re-blessed for it and `showcase_450` was **byte-identical** — station
03's camera is aimed the other way, which is the cheap confirmation that one added entity changed
only the frames it is in. **M32 unfaked two of the three the tour doc names**: the stride is
now driven by the ground the walker covers (`stride`, the number `list-joints` measures off
the clip) and its feet are planted on the terrain by a `FootPlant`. **M33 gave it five collision
proxies**, which is the tour's use of `SkinnedCollider` — and re-blessed all six baselines for a
reason worth reading in that section: the walker touches nothing, and adding bodies to a rapier
world perturbs every other body in it anyway. Still faked: `Idle` is in the
file but never crossfaded to, because a crossfade is the nondeterminism M9 refused.

Station 04 fires all three `Breakable` triggers in one run (collision at ~585, `break_entity` at 601,
`explode` at 636). What is real: particles, physics, fragments, the ice (real
`Material.transmission`), the campfire (layered additive flame, turbulent smoke, streaked embers, and
a `PointLight` the effects script flickers off the same signal that drives the emission rates), the
pond (one `Water` entity where sixteen script-bobbed cube tiles used to be), the forest (nine `Tree`
components where twelve cylinder-and-sphere entities used to be), and four `Cloud`s — the tour's
cameras are all ground-level and aimed *down*, so the clouds ride the horizon rather than filling the
sky. Still faked and named as such in the doc: animals (scaled spheres on parametric loops) and the
sky (a gradient, not scattering). The blast at station 04 emits no light, which is a wiring job rather
than a missing feature. The pond **refracts** since M27 (`ior: 1.33` — `Water` carries its own, having no
`Material` to put one on), which is what re-blessed the six baselines a second time; **alpha-cut
leaves** are the last flat surface in frame, and they need `Tree::leaf_material` to grow map fields
— an engine change, not authoring.

**Building it found a physics bug** now fixed and regression-tested: priming the broad-phase BVH
before the first step (vehicle worlds did this so wheel rays hit ground on step 0) consumed the pair
events, and rapier's `NarrowPhase::register_pairs` is private — so every collider **already resting in
contact at load** silently lost its contacts and fell through the world forever. Bodies *dropped*
from a height were unaffected, which is why every earlier fixture missed it. The first-step BVH now
goes on a scratch clone (`bvh_cold`).

**The crates became wood (M43/M44).** Station 04's five crates carried M14's four cube fragments
until now; they are `material: "wood"` with ten generated `Shard` fragments each, so the boulder
splinters them along the grain and each break throws M44's sawdust. Three things the change taught,
none of them about shards:

- **The three-trigger story does not survive a material.** Wood's scatter carries Crate1's shards
  into Crate2 hard enough to break it, and the row goes down in four steps — leaving the blast at
  636 with nothing standing inside its radius. There is no threshold-and-force pair that fixes it:
  the window where debris does not chain-break is above the window where the blast still can.
  `Crate6` is the answer, off the boulder's line and inside the blast — see `designs/showcase-tour.md`.
- **Fragment mass is a scene-tuning input, not a physical constant.** `engine fracture` writes the
  material's real density (wood 700 kg/m³) and the crates' own collider is 60 kg/m³, because a crate
  is mostly air. Conserving the crate's 60 kg across shards that tile its full volume is the
  physically honest choice and it is *unplayable* here: the smallest of ten Voronoi cells is ~2 kg,
  the tour's `explode` divides a 210 impulse by that mass, and the splinter leaves at 60 m/s and
  clears the terrain. The generator's density is what the scene keeps — which is also what M43 did
  to the ice pillar in this same file, giving a 40 kg/m³ pillar 2500 kg/m³ glass.
- **A thrown fragment wants CCD, and could not have it.** `breaking.rs` hard-coded `ccd: false` on
  every fragment. A shard sailing at 8 m/s went *through* the terrain on the way down — descending
  0.17 m a step against a `trimesh`, resting height −0.39 m, and the body at −0.67 m and falling
  forever in silence. Fragments now inherit the parent's `ccd`, which is off for every scene that
  does not say otherwise, so both golden traces and all 41 pinned baselines stayed byte-identical.

**The campfire soldier.** Station 02 carries the arena shooter's seventeen-joint
`rigged_soldier.gltf` at the fire pit's rim — 2.4 m from the fire's centre, facing it, playing its
upper-body `Idle` — so the tour's second skinned × textured draw is also its one *point-lit* one.
It is deliberately the minimal rigged entity: `Transform`, `Mesh`, `Material`, `AnimationPlayer`,
and nothing physics can see. A `SkinnedCollider` would have re-blessed every physics artifact in
the file (adding any collider perturbs every rapier body — the M33 cost, and this entity has
nothing to touch), and a `FootPlant` would plant feet that `Idle` never moves; standing at the
height `engine terrain-height` reports is the whole grounding story. The change re-blessed the
five tour frames the figure is visible in (at the fire in 270, a distant sliver in 450, 585, 646
and 810 — 585's sliver alone was over the 0.02% allowance) plus the GI bake for its triangles,
and left `showcase_90`, both golden traces and all 41 bit-exact baselines untouched — which is
the collider-free claim, measured.

**The runner.** The same rig again, lapping the campfire on a 6 m circle carried by
`tour_wildlife.rhai` in the walker's set-position-and-look-at pattern. The one number that
matters: it is carried at **4.46 m/s because that is the speed the `Run` clip covers ground at**
— stride 2.7664 m per 0.62 s cycle, measured with `engine list-joints --entity Runner`, and a
stride-driven gait carried at any other speed plays the right footfalls at the wrong ground
speed. It has `FootPlant` (sole 0.07, the soldier ankle height) and a stride like the walker but
no colliders like the idle soldier — so it, too, moved no physics artifact. Its circle was
cleared against the fire pit (4.1 m), the idle soldier (3.6 m), and the `RingRoad` (5.9 m) with
`road-centerline` queries. This time all six tour frames re-blessed: the runner is visible in
every one of them, `showcase_90` included, because station 01's camera catches the far side of
its lap at the frame's left edge. Note for the next filmstrip user: `filmstrip`'s window is the
pure render *clock* — scripts do not step — so a script-carried entity sits at its authored
transform in every tile; use repeated `screenshot --steps N` instead.
