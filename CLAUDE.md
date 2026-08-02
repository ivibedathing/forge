# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

**This file is the index, the invariants, and the traps.** Per-system detail lives in
`designs/notes/*.md` — one note per system, holding what building it taught. Read
`designs/agent-native-engine-design.md` before making structural decisions: it is the source of
truth for layout, formats, and build order, and several choices in it are still open (§9).

## How to read this repo's documentation

Four tiers, and knowing which one answers your question saves a search:

1. **This file** — the component vocabulary, the CLI surface, the invariants, the settled
   decisions, the cross-cutting traps, and a sentence per system pointing at its note.
2. **`designs/notes/*.md`** — the detail behind each digest: what the renders changed, which
   constants are load-bearing, what is deliberately absent. **For M4–M25 these notes are the only
   rationale in the working tree**, because those milestones' design docs were deleted once built.
3. **`designs/*.md`** — the milestone design docs that survive (M26 and later, plus the
   cross-cutting ones). These hold the *rejected alternatives*; the notes hold what the build
   learned. `designs/README.md` lists the eighteen deleted docs and the two commands that recover
   one from git history. **Read the original out of history before reversing one of its
   decisions** — that is the case the longer prose was written for.
4. **`docs/*.md`** — the wire contracts (`cli-contract.md`, `error-codes.md`), the generated
   `component-reference.md` (every field, every default, every range), and `scene-format.md`. All
   four are pinned by tests.

## The component vocabulary

Every component the engine has, what it does, and the note with the detail. Field-level truth is
`engine list-components` and `docs/component-reference.md` — those are generated, this is oriented.
**Absent fields *are* the documented defaults**, so `engine inspect` is how you see what a scene
actually says.

| Component | What it does | Note |
|---|---|---|
| `Transform` | Position, XYZ-Euler-degree rotation, and scale in **metres**; the placement every other component is read through. | — |
| `Camera` | A viewpoint with `near`/`far`; aims down its entity's local **−Z**. `--camera Name` picks one. | — |
| `Mesh` | References geometry: a `builtin:` primitive or a `.gltf`/`.glb` path relative to the scene file. | `assets.md` |
| `Material` | Surface appearance — PBR factors, texture maps, alpha, transmission, `ior` — or **is** a `materials/*.json` file via `asset`, which is exclusive with every other field. | `m26-materials.md` |
| `DirectionalLight` | A sun: colour and intensity, aimed down local **−Z**. At most one per scene, and a `daylight` block synthesizes it. | `m04-lighting.md` |
| `AmbientLight` | Uniform fill light. At most one per scene; with `sky` on it becomes hemispheric. | `m04-lighting.md` |
| `PointLight` | A local lamp with a hard `range` horizon; ≤8 per scene, no shadows. | `m17-point-lights.md` |
| `RigidBody` | Makes an entity dynamic, kinematic or fixed; scripts read and write its velocities. | `m08-physics.md` |
| `Buoyancy` | Floats a dynamic body on a named `Water`, sampling its collider in columns so a hull rights itself. | `m41-buoyancy.md` |
| `Collider` | The shape physics sees — cuboid/sphere/capsule/trimesh/convex_hull — plus friction, density (**kg/m³**), and collision layers. | `m08-physics.md` |
| `Script` | Runs `fn step(world, step)` once per fixed step against the curated `world` API. | `m10-scripting.md` |
| `AnimationPlayer` | Plays a property clip (`*.anim.json`) or a glTF skeletal clip (`mesh.glb#Walk`); `stride`/`phase` drive a gait by ground covered. | `m09-animation.md`, `m30-skeletal-animation.md`, `m32-locomotion.md` |
| `FootPlant` | Plants a skinned character's feet on the `Terrain` under it, dropping the hips to reach. | `m32-locomotion.md` |
| `SkinnedCollider` | Hangs simple collision proxies off named joints, re-posed from the rig every step; a part may `fit` its bone. | `m33-skinned-colliders.md`, `m39-ragdolls.md` |
| `Ragdoll` | Hands a skinned character's skeleton to physics — one-way, per entity — and carries the resulting `pose`. | `m39-ragdolls.md` |
| `ParticleEmitter` | A seeded deterministic cone emitter around local **−Z**; M17's fields turn a smoke cone into flame. Since M44 a `duration` makes it a burst, and `despawn_when_done` takes its entity away once the last particle dies. | `m13-particles-and-m17-fire.md`, `m44-break-dust.md` |
| `Breakable` | Lists pre-authored fragments and the impulse that shatters the entity into them. Since M43 it names its `material`, which decides how the pieces behave once they are pieces — and throws that material's `dust`. | `m14-breaking.md`, `m43-fracture.md`, `m44-break-dust.md` |
| `Shard` | A convex piece of a broken thing, as a point set that **owns its geometry** — the hull it draws is the hull it collides with. | `m43-fracture.md` |
| `Wheel` | One raycast-suspension wheel on its own *visual* entity, naming the chassis it drives. | `m11_5-vehicles-and-wheels.md` |
| `Water` | A body of water that **owns its surface** — Gerstner waves, depth colouring, foam, refraction. Since M41 it also carries the fluid's `density`, the one field nothing renders. | `m18-water.md`, `m27-water-refraction.md`, `m41-buoyancy.md` |
| `Terrain` | A height-field patch that **owns its grid**, painted by height/slope layers; the ground everything stands on. Since M42 its `basins` cut authored hollows into the noise. | `m22-terrain.md`, `m42-terrain-basins.md` |
| `Road` | A drivable ribbon from a polygon centerline with corner radii; markings are drawn per pixel. Since M40 it can widen per point, bank, and ride a `Terrain`. | `m23-roads.md`, `m40-road-authoring.md` |
| `Junction` | The patch of asphalt where roads meet, bounded by the mouths of the roads that name it. **Owns its geometry**, and is drawn by the road shader. | `m40-road-authoring.md` |
| `Tree` | A grown tree — bark plus leaves — from a parameter recipe, not a mesh file. | `m19-trees.md` |
| `Cloud` | A cluster of interpenetrating lobes that drifts; **owns its mesh**. | `m20-clouds.md` |
| `Meadow` | Ground cover on a seed→grass→weeds→straw→collapse life cycle, animated entirely in the vertex stage. | `m29-meadows.md` |
| `LightProbeVolume` | A box of baked irradiance probes; replaces the hemispheric fill with one that knows what is above a surface. At most one per scene, and it carries no geometry. | `m35-global-illumination.md` |
| `HudText` | Screen-anchored text at an integer scale of the 8×8 font. | `m11_6-hud.md` |
| `HudRect` | A screen-anchored coloured rectangle — bars, backdrops, gauges. | `m11_6-hud.md` |
| `HudPanel` | Lays its children out in a row, column, or freely; hugs its contents unless sized. | `m31-ui-system.md` |
| `HudImage` | A nine-sliced textured rectangle. **With no `slice` it is all middle band, and the middle band tiles.** | `m31-ui-system.md` |
| `HudInteract` | Makes the HUD element on its own entity hoverable, pressable and clickable — polled, never dispatched. | `m31-ui-system.md` |

**Recipes own their geometry**, so `Water`, `Terrain`, `Road`, `Junction`, `Cloud` and `Meadow`
carry **no `Mesh` and no `Material`** — authoring one is a validation error. `Tree` and `Shard` are
the exceptions on materials only: a tree's `Material` is its bark, and a shard's is the surface the
thing it broke off was painted. A `LightProbeVolume` carries neither for a different reason: it is
a *region of space* that grows no geometry at all.

**Scene-level blocks**, siblings of `entities`: `physics` (gravity, `timestep_hz`), `environment`
(sky, fog, shadows and their cascades, MSAA — `m16-environment.md`, `m38-shadow-cascades.md`,
**script-writable since M36**), `daylight` (the clock-driven sun, moon and sky palette —
`m21-daylight.md`), and `templates` (entity definitions declared but **not instantiated**, which a
script spawns at runtime — `m37-entity-spawning.md`).

**System order per fixed step**: animations → scripts → physics → particles → render.

## Current state

**M0–M44 are done** — the v1 roadmap (M0–M10) is complete, plus M11 keyboard input, M11.5 vehicle
dynamics, M12 wheels + HUD components + collision, M13 particles, M14 breaking, M15 frame cost,
M16 environment, M17 fire + point lights, M18 water, M19 trees, M20 clouds, M21 day/night,
M22 terrain, M23 roads, M24/M25 agent ergonomics, M26 the material system, M27 water refraction,
M28 the mouse, M29 meadows, M30 skeletal animation, M31 the UI system, M32 locomotion and foot
planting, M33 skinned collider proxies, M34 the metre, M36 the game shell, M37 entity spawning,
M38 shadow cascades, M39 ragdolls, M40 road authoring, M41 buoyancy, M42 terrain basins,
M43 material-aware fracture, M44 the break's dust, and M35 global illumination.
(M7 editor at scope E0–E2 + validation
panel + `--watch`.)

JSON scenes load into hecs, render headlessly to PNG with PBR lighting, validate with
all-errors-at-once reporting under a formalized CLI contract, reference glTF mesh files, pin their
renders against committed baselines with `engine diff-render`, and open in a GUI editor that is a
live writable *view* onto the file. Verified by 200+ tests including offscreen pixel readback and
an end-to-end CLI suite, and by the fixtures in `designs/milestone-verification-scenes.md`
(`verify/m4_lighting.json` diff-renders bit-exactly; `verify/m5_broken.json` is committed **broken**
and must never validate — its failing with all seven planted errors is the pass condition, pinned
by `repo_contracts.rs`). **From M6 on, the standard check's "look at the PNGs" step is
`engine diff-render` against the committed baselines** — each milestone adds its scene's baseline in
the same commit that adds the scene.

What works today:

```
engine validate <scene.json>... [--strict]  # every error at once; multi-file; --strict promotes warnings
engine screenshot <scene.json> --out x.png [--steps N] [--input f] [--time T] [--camera N] [--width W --height H]
engine diff-render <scene.json> <baseline.png> [--steps N] [--input f] [--time T] [--out diff.png] [--threshold N] [--max-diff-percent P]
engine edit <scene.json> [--watch]       # GUI editor; --watch = read-only supervision mode
engine simulate <scene.json> --steps N [--input f] [--bake out.json] [--trace t.jsonl] [--entity N]...
#   the report says where every dynamic body ended up (M25); screenshot/filmstrip report a
#   frame "digest" — mean_luminance, background, coverage — so a black frame is diagnosable
#   without reading the image (M25). A run that spawned anything reports `spawned` (a total,
#   not a live count) and traces `spawned`/`despawned` event lines (M37)
engine raycast <scene.json> --from x,y,z --dir x,y,z [--steps N] [--input f]
engine filmstrip <scene.json> --out strip.png [--start S --end E --frames N --columns C]
engine list-animations <scene-or-clip> [--schema]  # glTF clips too, with their channel targets (M30)
engine list-joints <scene-or-mesh> [--entity Name] [--time T] [--steps N]
#   the rig and where it is (M30); --steps for a pose the simulation reached, and a
#   measured `stride` when the entity has a FootPlant (M32)
engine road-centerline <scene.json> [--entity Name]  # where a Road actually went
#   plus the local width and bank per sample (M40)
engine junction-plan <scene.json> [--entity Name]    # where a Junction's arms met it (M40)
engine list-colliders <scene.json> [--entity Name] [--steps N] [--input f]
#   every collider physics holds — shape, size, world placement — read back out of the
#   built world, so a skinned hitbox nothing renders is still answerable (M33)
engine fracture <scene.json> --entity Name [--material M] [--pieces N] [--seed S]
#                 [--impact x,y,z] [--grain x,y,z] [--threshold T] [--write]
#   break a volume into material-shaped shards and print them as a Breakable;
#   --write splices it in. A command, never a runtime behaviour (M43)
engine fit-colliders <scene.json> [--entity Name] [--shape S] [--write]
#   solve a SkinnedCollider from the skin's vertex weights and print it as JSON;
#   --write splices it into the scene. A command, never a runtime behaviour (M39)
engine ui-layout <scene.json> [--width W --height H] [--entity N]... [--steps N] [--input f]
#   where the UI landed (M31); --steps reports what a script *painted* (M36)
engine terrain-height <scene.json> --at x,z [--entity Name]  # where the ground is (M24)
engine bake-gi <scene.json> [--entity Name] [--out path] [--samples N] [--check]
#   bakes a LightProbeVolume's transfer file; the only query-side command that
#   *writes* into the project, and it reports probes, rays and relocations (M35).
#   --check writes nothing — it recomputes the digest and fails `gi_bake_stale`
#   when the geometry has moved, which is the check `validate` cannot afford
engine gi-probe <scene.json> --at x,y,z [--normal x,y,z] [--time T]
#   the irradiance the renderer would use here, the pre-M35 fallback beside it,
#   the blend weight and how open the sky is — a number, not a picture (M35)
engine water-height <scene.json> --at x,z [--entity N] [--time T] [--steps N]
#   where the water is, and which way it faces (M41); the first query that takes a
#   clock, and the first that can answer "no water here" rather than a height
engine inspect <scene.json> [--entity Name]  # every field resolved, defaults filled in (M24);
#   also reports the scene's `templates` — what it can spawn — with defaults filled in (M37)
engine run-scene <scene.json> [--record-input f]   # windowed viewer + play mode; keyboard AND mouse; FPS readout is viewer-only
engine init [dir] [--force]              # scaffold a project: starter scene + AGENTS.md/CLAUDE.md
engine agent-guide                       # the agent orientation as markdown (a stdout exception)
engine import <model.glb> [--into scene.json] [--textures dir] [--materials dir]  # glTF materials (M26)
engine list-components [--component Name]  # scene + component JSON Schemas (with range constraints)
engine list-components --markdown          # the same vocabulary as prose; generates docs/component-reference.md
engine build [--check]                   # cargo build/check, diagnostics re-emitted as engine errors
engine run                               # M0 triangle (stack proof)
engine info                              # selected GPU adapter as JSON
```

**The query commands exist because looking at a picture cannot answer where something is.** Reach
for `inspect` (what did you author), `simulate --entity` (where did it end up), `terrain-height`,
`road-centerline`, `junction-plan`, `gi-probe`, `list-joints`, `list-colliders` and `ui-layout`
rather than re-deriving any of them — a generator that re-derives a curve is how two implementations
start disagreeing.

## Traps that cost time

The cross-cutting ones. Per-system traps are in each note.

- **`mesh.wgsl`'s four lighting lines are ULP-sensitive.** The lines computing
  `direct`/`ambient`/`base_color` are the M4 originals and must reach the compiler surrounded by the
  code they shipped in. Terrain, textures, refraction and skinning are therefore **splices**
  (`with_surface`, anchored substitutions asserted to land exactly once), never inline branches.
  Restructuring them into arithmetic *equal on paper* moved one pixel by one ULP — measured three
  separate times. Compiler FMA contraction depends on surrounding code.
- **An anchor with one claimant is a substitution; an anchor that could ever have two is an
  assembly.** `with_surface` splices by sequential `str::replace`, so a second producer claiming an
  anchor is **a silent no-op**, not a merge — the feature renders as if absent. M27 learned this on
  `VERTEX_STAGE`; M35 found it twice more, on `AMBIENT`/`FILL` (texturing's occlusion map plus GI's
  probe lookup) and on `FRAME_TAIL`, where it is worse: two producers appending to one *positional*
  uniform struct make the field order depend on the producer list, so a variant reads the wrong
  offset and renders a plausible wrong picture. Both are now assembled. **Adding the second claimant
  later is not a refactor — it is a bug that already shipped.**
- **A binding number is not a position in a list**, which is the only reason M38 and M35 could both
  land. Both wanted binding 5 of group 2 — the cascade matrices and the first probe plane — and the
  cascade entry is *conditional* on top of that. GI simply starts at 6 and the layout skips 5 when
  there is one cascade. `a_cascaded_surface_inside_a_volume_takes_both` pins the pair.
- **The check that settles a bit-exactness question is an A/B between binaries**, not a diff against
  a baseline: build the CLI at `main` and in the worktree, render the same scenes with both, `cmp`
  the PNGs. The `ab-check` skill is this ritual.
- **This adapter is not bit-reproducible for fine geometry against relief under MSAA.** Terrain
  (M22) and meadows (M29) are the sources; a meadow at `samples: 4` gave six distinct PNGs from six
  renders of an unchanged scene. **A new fixture wanting a hard pin must aim its camera at its
  subject with no terrain in frame, or render at `samples: 1`.** When a sweep fails, `md5` N renders
  of the one frame: stable-but-different is a real change, different-every-time is the adapter.
- **Baselines are per-adapter artifacts**, and for CPU-generated geometry (trees, clouds) **per
  build profile too** — release `sin_cos` moves 3 pixels of `m19_trees.png`. Bless from the debug
  binary that `cargo test` runs.
- **A physics scene is not stable under the addition of a collider anywhere in it.** Dropping one
  5 cm static sphere 200 m from anything moved six bodies by up to 4.4 mm — the collider set is an
  input to the broad phase and float addition is not associative. The determinism promise is per
  *file*: a scene that gains a body re-blesses. **M37 is the sharpest case**: the tour's embers
  moved the *breaking crates* at the other end of the arena, and the diff image is entirely
  somewhere the change is not.
- **A kinematic body has no mass properties, so promoting one to dynamic needs an explicit
  `recompute_mass_properties_from_colliders`.** `Collider::set_density` alone leaves the body at a
  near-zero mass, because mass was meaningless to it until that moment and rapier never computed
  one. The symptom is a ragdoll leaving the scene at 40 m/s from a 6 N·s kick, which sends you to
  read the joints — and the joints are fine (M39).
- **`spawn` is a reserved keyword in Rhai**, which is why the script call is `spawn_entity` (M37).
  The curated engine also has an expression-complexity budget that rejects a six-term string
  concatenation at *compile* time — split it into two statements.
- **Anything a script throws wants `ccd: true`.** The tour's 7 cm embers tunnelled straight through
  the terrain heightfield without it, and a body that leaves the world does so in silence. A
  spawned projectile is the easiest way in this engine to author a body that moves further than its
  own diameter in one step (M37).
- **Grep the `.rhai` files for `set_scale` before believing a scale-space change is complete.** Two
  scripts drive scale every step from a hard-coded constant, so editing the scene file achieved
  nothing and the coals rendered at half size (M34).
- **Four shaders sample the shadow map, and they do not declare the same frame uniform.**
  `mesh.wgsl`, `water.wgsl`, `road.wgsl` and `meadow.wgsl` each carry their own near-copy of the
  lookup, so anything that changes the map's *binding type* changes all four together or fails at
  pipeline creation. And `water.wgsl`'s `FrameUniform` stops at `params`: uniform field offsets are
  positional, so a field appended after `point_lights` is unreachable from water without giving it
  an eight-light array it never reads. **Check all four before appending to a shared uniform** (M38).
- **A road's cross-section widens in the *positions* while `u` stays nominal**, which is how the
  mitre worked since M23 and how per-point width works since M40. The shader's
  `|u| > half + shoulder` therefore finds the skirt with nothing extra uploaded — and the price is
  that **paint scales with the road**: a section at 1.5× width wears a 1.5× wider edge line. Holding
  the shoulder at a constant metre width would need a third vertex channel on a ULP-sensitive path.
- **A road following a `Terrain` samples the ground across its own cross-section, not down its
  middle**, and takes the highest of the three (M40). The naive centerline-only version punches a
  *hole* in a wide road on sloping ground, because the uphill edge ends up buried and the engine
  does not carve terrain. This is why `width_scales` is computed **before** `followed_heights` in
  `road::build`; swapping them back samples the ground at the wrong offsets on any road that widens.
- **A `Water` patch's rectangle must be *wider* than its basin's shoreline, not narrower** (M42).
  Every boundary point of the sheet has to land on ground above its own surface or the water ends
  in a straight cut, and the binding points are the rectangle's **edge midpoints** — they sit
  closest to the basin's centre, where the wall has risen least. The instinct is to shrink the
  sheet to fit the pool, and shrinking it is exactly what exposes the edge. Check it with
  `engine terrain-height` around the rectangle rather than by rendering: the tour's clearance is
  0.20 m at its worst corner, and a first pass left less headroom than the pond's own waves.
- **A junction's shoulder quad across a mouth is degenerate and must stay skipped** (M40): all four
  of its corners lie on the mouth line, so the quad has zero area and a `NaN` normal. `mouth_of` is
  what excludes it, and nothing is lost — the shoulder there is the road's own.
- **A doc comment on an enum *variant* blinds the validation walk's closed-vocabulary check**, and
  since M43 there is a second half to it: **an `Option<T>` of a *named* type publishes
  `anyOf: [{$ref}, {"type": "null"}]`**, not the flat `"type": ["string", "null"]` an optional
  primitive gets. The walk read only the flat form, so every optional enum field in the engine was
  waved through unchecked until `optional_variant` in `walk.rs`. Both symptoms look the same and
  neither looks like a validation bug: the bad value reaches serde and comes back as
  `scene_parse_desync`, the code whose message says "this is an engine bug, not a scene problem".
- **`engine fracture` works in world metres and stores entity-local ones** (M43). A plank authored
  the M34 way — a `builtin:cube` at `scale: [0.6, 0.18, 2.6]` with unit half-extents — has a *cube*
  for its local box, so a generator reading the local box alone finds no grain axis to splinter
  along and no thin axis to shatter through. The multiply-in/divide-out by `Transform.scale` is
  load-bearing, and its absence shows up as wood splintering the wrong way rather than as an error.
- **A generated component is a diffability problem the moment it is large.** Fourteen shards
  spliced through `formatter` arrived as one 6,000-character line — a JSON scene that is no longer
  git-diffable, which is invariant 1 failing quietly. `formatter.rs` now breaks an array of objects
  one element per line and `shorten_floats` trims `serde_json`'s f64 widening of an f32
  (`0.12767969071865082` for a number the engine had seven digits of). Both apply only to shapes no
  pre-M43 caller produces, so every committed splice stayed byte-identical — **check that when
  adding a third**.
- **A particle burst born at a contact point is invisible, and every diagnostic says it works**
  (M44). A contact point is *on* the surface, and at the moment of a break the object is still
  whole, so the particles spawn inside geometry that depth-rejects them: the trace has the spawn,
  the system reports live particles, `instances()` hands the renderer a full list — and the frame
  is empty. Push the burst **out along the face normal** by a fraction of the object's radius, which
  is where dust actually comes from. What isolates this class of bug is rendering the same emitter
  parameters in an empty scene: if it puffs there, the problem is placement, not the pipeline.
- **Dust has to out-contrast the ground, not match the material** (M44). Rock dust's honest colour
  is the colour of the rock, and a grey puff over grey ground at this exposure is nothing at all.
- **Giving an existing `Breakable` a `material` changes what its *neighbours* do**, because M43's
  scatter throws the pieces outward with real momentum. The tour's crate stack went from "the
  boulder shatters one" to "the row goes down in four steps" on that one field, and the knock-on
  was that the later `explode` had nothing left inside its radius. Budget for re-authoring the
  scene around a break, not just the entity that breaks.
- **`engine fracture` writes the material's *solid* density, and a hollow thing is not solid**
  (M43/M44). Wood is 700 kg/m³; the tour's crate is 60, because a crate is mostly air. Conserving
  the parent's mass across shards that tile its full volume is the honest reading and it is the
  wrong one here: the smallest of ten Voronoi cells is ~2 kg, `world.explode` divides its impulse
  by that mass, and the splinter leaves at 60 m/s. Keep the generator's number — M43 already did
  exactly this to the tour's ice pillar, giving a 40 kg/m³ pillar 2500 kg/m³ glass.
- **A fragment inherits its parent's `ccd`, and until the tour's crates went wooden it could not have any.** A thrown shard
  is the tunnelling case this engine makes easiest to author: small, fast, and landing on a
  `trimesh` terrain. The symptom is the silent one — 0.17 m of descent per step against ground at
  −0.39 m, the body at −0.67 m and falling forever. `ccd` is still off unless the *parent* asks,
  so nothing that broke before this changed by a byte.
- **`builtin:cube`'s faces disagree on which way `u` runs, in pairs rather than in axes.** Anything
  strongly directional on a cube draws differently on all four sides. `builtin:plane`'s UVs are not
  the intuitive ones either — fixing both is deferred as its own change with its own A/B (M26).
- **Negated float comparisons in validation are negated so NaN fails.** `!(a > b)` is not
  `a <= b` for NaN; six clippy `#[allow]`s carry written reasons. **Read the reason before deleting
  one.**
- **XYZ Euler clamps the middle angle to ±90°**, so a physics-integrated yaw past that returns as
  the `(±180, θ, ±180)` twin and `rotation[1]` stops being "the yaw". Use `world.forward(name)` for
  heading math (M11.5).
- **Bake next to the scene, not `/tmp`** — asset paths are relative to the scene file, and a baked
  copy elsewhere breaks every one of them.
- **An absent cursor is the centre of the frame** (M28), so "no `--input`" is not the untouched case
  for a scene with anything interactive in the middle.
- **A script's clock is one step behind physics and the render.** A script runs at the time its step
  *begins* at (`step_index · dt`, 0-based); physics and the render get the time it *ends* at. This
  predates M41 and is documented in `simulate.rs`, but water is the first thing in the script API
  where it is **visible**, because it is the first surface that moves — comparing
  `world.water_height` at `--steps N` against `engine water-height` wants `--steps N-1`. Terrain
  never had to care: a height field has no clock.

## Verification

**Run the CLI as `bin/engine`, not `cargo run -p engine-cli --`.** The shim checks whether any source
is newer than the binary (a find, ~0.02s warm), rebuilds only then, and execs; cargo's freshness walk
over this workspace costs ~8s *warm* on every call, which is the difference between a loop worth
running and one worth avoiding. Arguments pass through untouched and stdout stays clean; a rebuild
that fails comes back as one `cargo_error` line on stderr, exit 2. Default profile is **debug**,
matching how baselines are blessed. `ENGINE_PROFILE=release bin/engine …` when you want speed over
comparability.

**`bin/verify-baselines` is "look at the PNGs" as one command.** Every committed baseline is listed in
`examples/scenes/verify/baselines.json` with the scene and flags that reproduce it, and
`repo_contracts.rs::every_committed_baseline_is_listed_in_the_manifest` fails on any baseline missing
from it. NDJSON out, exit 1 on drift, `--filter` to scope, `--bless` to re-bless (from the debug
binary), `--diff-dir` to write diff PNGs, and `--render-to DIR` + `ENGINE=<other binary>` to run the
A/B bit-exactness check as a loop rather than a reconstruction. Both golden traces are checked too,
GPU-free.

**41 of the 47 baselines are pinned by a test.** The six that are not are the six `showcase_*`
frames, deliberately: they are not byte-reproducible on this adapter (measured repeatedly at four to
six distinct images from six renders of an *unchanged* scene, on any binary), so a test asserting
them would fail at random, which is worse than no test. They keep a `diff_args` tolerance of
`--threshold 24 --max-diff-percent 0.02` in the manifest and stay the sweep's job; `cli.rs` says so
where someone would go to add them. The pixel *allowance* is there rather than a wider threshold
because the residual is one or two pixels well outside it, not a haze just over it — 24/0.02 held
for eight consecutive full sweeps. **The other 41 entries carry no `diff_args` at all — they are
bit-exact, and a failure there is real.** `m35_gi.png` joined them in M35: five renders of it gave
one image, so it took a hard pin rather than a tolerance.

**Which tour frames flake carries no information; whether one is stable under repetition does.**
Six separate sweeps each picked a different subset of the six, M35's, M36's and M38's A/Bs
included.
Every time, the differing frame had a binary disagreeing with **itself** — which is why the
`md5`-it-N-times step is not optional. Six measurements, six times the answer was the adapter. M35's
is the sharpest: `showcase_585` gave **four distinct images from five renders on each binary**, and
the two populations overlapped.
M40's A/B is the cleanest statement of the rule so far: **34 of 34** comparable artifacts came back
byte-identical between a `main` binary and the milestone's, and the only six excluded were the tour
frames — excluded because the tour *scene* gained four entities in that commit, not because they
flaked.

**Blessing gotcha that cost a sweep here: `--filter` is a substring match, not a regex.**
`--filter "m28|showcase"` matches nothing and blesses nothing, reporting success — run one filter
per artifact family and check the `checked` count in the summary line.

The three repeated rituals are skills in `.claude/skills/`: `verify-baselines`, `ab-check`,
`milestone`. **The measurements behind the rules above** — every flake-rate probe, the
baseline-pinning history, and what the clippy cleanup found — are in
`designs/notes/verification-history.md`. Read it when deciding whether a sweep failure is worth
debugging or worth re-running.

`cargo test --workspace` is the real check, not `cargo build`.
`crates/engine-render/tests/headless_render.rs` renders offscreen and asserts on pixel values,
because "the window opened and did not crash" does not distinguish a working renderer from a culled
triangle or a shader that writes nothing. Those tests skip cleanly (rather than fail) when no GPU is
available.

Backface culling is **on**, and the M0 triangle is wound counter-clockwise in clip space to match
wgpu's default front face. A wrongly-wound triangle renders nothing at all — if geometry is
invisible, suspect winding before suspecting the pipeline.

**A new fixture arrives with the CLI test that diff-renders it**, in the same commit, unless it is
in the tour's nondeterministic class — in which case say so where the test would have gone.

## Dependency versions — check before trusting recall

wgpu moves fast and breaks its API every release. This workspace is on **wgpu 30**, which differs
sharply from the 25-and-earlier APIs most training data describes:

- `Surface::get_current_texture` returns a `CurrentSurfaceTexture` enum, **not** a `Result`.
  Variants include `Suboptimal`, `Occluded`, and `Validation`.
- Presentation is `Queue::present(texture)`, not `SurfaceTexture::present()`.
- Push constants are gone; `PipelineLayoutDescriptor` has `immediate_size: u32`.
- `multiview` is `multiview_mask: Option<NonZeroU32>` on both pipeline and render pass.
- `Instance::new` takes `InstanceDescriptor` **by value**, and it has no `Default` — use
  `InstanceDescriptor::new_without_display_handle_from_env()`.
- `RequestAdapterOptions` requires `apply_limit_buckets`.

When touching wgpu, read the API in `~/.cargo/registry/src/*/wgpu-30.0.0/src/` rather than writing
from memory. winit is pinned to the **0.30** stable line; 0.31 is still beta. rapier3d is pinned
`=0.34.0` (its 0.34 math backend shares our exact glam version, so no conversion layer exists) and
Rhai `=1.25.1`; egui is **git-pinned to a master commit**, because released egui pairs with wgpu 29
— which is also why this workspace cannot publish to crates.io.

## The systems

What each milestone was about, and where its detail is. **The traps live in the notes** — every one
of these systems has at least one constant, convention or "simplification" that will bite a change
made without reading its note first. Paths are under `designs/notes/`.

### Foundations

- **Assets** → `assets.md`. What `Mesh.asset` may reference and how paths resolve.
- **Lighting (M4)** → `m04-lighting.md`. `DirectionalLight` + `AmbientLight` and the GGX
  Cook-Torrance shader; render targets are sRGB.
- **Validation & the CLI contract (M5)** → `m05-validation.md`. Every error at once, schema-driven,
  under a formalized wire contract. **Error codes are API.**
- **Diff-render (M6)** → `m06-diff-render.md`. Pins a render against a committed baseline. Blessing
  is `engine screenshot` — there is no bless flag.
- **Editor (M7)** → `m07-editor.md`. A GUI that is a live writable *view* onto the scene file,
  committing every edit as a splice.

### Simulation

- **Physics (M8)** → `m08-physics.md`. rapier3d with deterministic traces: `RigidBody` + `Collider`
  plus a scene-level `physics` block.
- **Collision (M12)** → also `m08-physics.md`. Script contact queries, mesh colliders, and
  free-form collision layers — all opt-in, so earlier traces are untouched.
- **Animation (M9)** → `m09-animation.md`. Property clips in JSON, animating a component field by
  entity name; the pose is a pure function of (files, time).
- **Scripting (M10)** → `m10-scripting.md`. Rhai `fn step(world, step)` against a curated `world`
  API deliberately small enough to keep runs byte-identical.
- **Particles (M13) and fire (M17)** → `m13-particles-and-m17-fire.md`. Seeded deterministic
  emitters; M17 adds the five fields that make a particle cone read as flame.
- **Breaking (M14)** → `m14-breaking.md`. Pre-authored fragments — no runtime fracture — broken by
  impulse, by a script call, or by an explosion.
- **Material-aware fracture (M43)** → `m43-fracture.md`, design in `designs/fracture-design.md`.
  What M14's fragments are made of: convex `Shard` geometry instead of boxes, generated offline by
  `engine fracture` with a per-material algorithm, and a `Breakable.material` that scatters the
  pieces away from the impact on the material's own speed, spin and surface. Both halves default to
  M14 exactly.
- **The break's dust (M44)** → `m44-break-dust.md`. The burst a material throws off — hanging dust,
  falling sawdust, glitter, sparks — and the `ParticleEmitter` **lifetime** that made it possible:
  `duration` bounds the emission, `despawn_when_done` takes the entity away once the last particle
  dies, and `ParticleSystem::sync` picks up an emitter the world gained (which is also what makes a
  `ParticleEmitter` on an M37 template work at all — it never had).
- **Skinned collider proxies (M33)** → `m33-skinned-colliders.md`. Simple shapes hung off named
  joints and re-posed from the rig each step, so a skinned character can be hit and can push things.
- **Ragdolls (M39)** → `m39-ragdolls.md`. M33's one-way rule reversed for one entity at a time:
  physics takes the skeleton over and hands it back as `Ragdoll.pose`, a **component field** — which
  is how invariant 2 survives and why a corpse baked mid-fall reloads into the same heap. Brings
  `ColliderPart.fit` and `engine fit-colliders` with it.
- **Entity spawning (M37)** → `m37-entity-spawning.md`, design in `designs/entity-spawning-design.md`.
  A `templates` block the script spawns from, so a run can grow rather than only shrink.

### Geometry recipes

Each owns its geometry, so the entity carries **no `Mesh` and no `Material`**.

- **Water (M18)** → `m18-water.md`. A body of water with Gerstner waves displaced in the vertex
  stage, depth colouring and shore foam.
- **Water refraction (M27)** → `m27-water-refraction.md`. One `ior` field bending what is seen
  through the surface, defaulting to no bending.
- **The wave evaluator and buoyancy (M41)** → `m41-buoyancy.md`, design in
  `designs/buoyancy-design.md`. The Gerstner sum mirrored on the CPU so `engine water-height`,
  `world.water_height` and a floating `Buoyancy` body can all ask where the surface is — held to the
  shader by a GPU agreement test that reads the drawn surface back out of a render.
- **Trees (M19)** → `m19-trees.md`. A grown tree — bark plus leaves — from a parameter recipe rather
  than a mesh file.
- **Clouds (M20)** → `m20-clouds.md`. Drifting clusters of interpenetrating lobes.
- **Terrain (M22)** → `m22-terrain.md`. A CPU height-field patch painted by height and slope layers;
  the ground everything else stands on.
- **Terrain basins (M42)** → `m42-terrain-basins.md`, design in `designs/terrain-basins-design.md`.
  `Terrain.basins` — circular hollows in world XZ, subtracted inside `height_at`, so the render, the
  `trimesh` collider, roads, meadows, foot planting and every query follow from the one
  implementation and **no shader is edited**. The only way to say "the ground dips *here*", and
  therefore the first way to put a pond somewhere the ground holds it.
- **Roads (M23)** → `m23-roads.md`. A drivable ribbon from a polygon centerline with corner radii,
  its markings drawn per pixel.
- **Road authoring (M40)** → `m40-road-authoring.md`, design in `designs/road-authoring-design.md`.
  Per-point width, banking the engine signs itself, roads that ride a `Terrain`, asphalt grain, and
  `Junction` — the patch a ribbon cannot be. Every one of them defaults to M23.
- **Meadows (M29)** → `m29-meadows.md`. Ground cover on a seed→grass→weeds→straw→collapse life
  cycle, animated entirely in the vertex stage.

### Environment and time

- **Environment (M16)** → `m16-environment.md`. Sky, fog, shadows, MSAA and transparency through one
  `environment` block. Every one of them defaults to off.
- **Shadow cascades (M38)** → `m38-shadow-cascades.md`. `shadow_cascades` renders the sun's map
  more than once, over **nested** slabs of the view, so the outermost cascade *is* M16's map and
  the default of 1 is M16 unchanged. Four shaders sample that map and all four splice together.
- **Point lights (M17)** → `m17-point-lights.md`. Local lamps with a hard `range` horizon, ≤8 per
  scene, no shadows.
- **Day and night (M21)** → `m21-daylight.md`. A pure CPU function mapping the clock to sun, moon,
  sky and fog — no shader changed to add it.
- **Global illumination (M35)** → `m35-global-illumination.md`, design in
  `designs/global-illumination-design.md`. `LightProbeVolume` bakes **transfer** rather than
  radiance, so the bounce follows `daylight` without re-baking; an unoccluded probe reconstructs
  `sky_ambient` exactly, which is why turning GI on cannot change an open scene's brightness. Two
  sky bands, not the three the design assumed.
- **Frame cost (M15)** → `m15-frame-cost.md`. The optimisation pass that took the viewer from ~34 ms
  a frame to ~0.9 ms, none of which moved a pixel.

### Materials

- **Materials (M26)** → `m26-materials.md`. Texture maps, shareable `materials/*.json` files, and
  refraction. Every added field defaults to the pre-M26 behaviour.

### Characters

- **Skeletal animation (M30)** → `m30-skeletal-animation.md`. CPU skeleton, GPU skin, glTF clips
  named by fragment (`meshes/robot.glb#Walk`). No new component — a skin is a property of the asset.
- **Locomotion and foot planting (M32)** → `m32-locomotion.md`. A walk cycle driven by ground
  covered rather than by the clock, with the feet planted on the terrain under them.

### Input and UI

- **Input (M11)** → `m11-input.md`. Keyboard sampled per fixed step, replayable headlessly as an
  `*.input.jsonl` timeline.
- **The mouse (M28)** → `m28-mouse.md`. Buttons on the same `held` set as keys, plus a cursor
  expressed as a **fraction of the frame**; the engine resolves the pointer ray.
- **HUD (M11.6 + M12)** → `m11_6-hud.md`. Per-step debug lines and screen-anchored `HudText` /
  `HudRect` components, sharing one CPU rasterizer.
- **The UI system (M31)** → `m31-ui-system.md`. Layout, nine-sliced images, and widgets a pointer
  lands on — the three things needed to build a *screen* rather than read state off one.

### Vehicles

- **Vehicle dynamics (M11.5) and wheels (M12)** → `m11_5-vehicles-and-wheels.md`. Raycast-suspension
  wheels on a dynamic chassis, with the pedals and steering driven by scripts.
- **The car demo and its circuit** → `car-demo.md`. A generated Spa-in-miniature circuit and the
  committed lap recording that regression-tests it.

### Agent ergonomics and units

- **Agent ergonomics (M24/M25)** → `m24-m25-agent-ergonomics.md`. The query commands (`inspect`,
  `terrain-height`, per-entity `simulate`) and the frame `digest`, so a render can be diagnosed
  without reading the image.
- **One unit is one metre (M34)** → `m34-one-unit-is-one-metre.md`. Made `builtin:sphere` fit the
  unit extent like the other primitives, and added the warning that catches a collider disagreeing
  with the mesh it stands in for.

### The demos and shipping

- **Showcase tour** → `showcase-tour-notes.md`, design in `designs/showcase-tour.md`. A 900-step
  camera move with every system running at once. **A test fails on any component the tour does not
  use**, so a new component's commit adds an entity here.
- **The game shell (M36)** → `m36-game-shell.md`, design in `designs/arena-menu-design.md`. Saves,
  a quit request, a script-writable `environment` block, and clip cutting — the three of the arena's
  four menu items that turned out to be engine work rather than script work.
- **Arena shooter** → `designs/arena-shooter.md`. The other live demo, and the worked example of the
  M31 UI system — a five-screen shell since M36, with a rigged player carrying three weapons.
- **Distribution** → `distribution-notes.md`, design in `designs/distribution-design.md`. Prebuilt
  binaries on a `v*` tag, `install.sh`, and `engine init` scaffolding a project.

## What this project is

An agent-native 3D engine in Rust: a game engine whose primary "user" is an AI coding agent rather
than a human in a GUI editor. The design constraint driving every other decision is the agent
feedback loop — edit a text file → validate → build → render a PNG headlessly → *look at it* →
iterate — using only ordinary bash and file edits, with no bespoke integration layer.

This inverts the usual engine tradeoff: machine-legibility beats GUI convenience wherever the two
conflict.

## Architecture

Cargo workspace (design doc §4), dependency order bottom-up:

- `crates/engine-core` — ECS, scene graph, math re-exports (glam)
- `crates/engine-render` — wgpu renderer, shaders, materials
- `crates/engine-assets` — mesh/texture loading, asset schema
- `crates/engine-cli` — the `engine` binary; the primary interface

Plus `engine-physics`, `engine-script`, `engine-editor`.

**`engine-render/src/scene_renderer/` is six modules**, split out of one 5,989-line file as **pure
code motion** — not one expression changed, which is the only form that refactor is safe in on a
path this file flags as ULP-sensitive in four places (the A/B: 34 of 36 artifacts byte-identical,
the two exceptions being the tour frames this adapter cannot reproduce at all). `uniforms.rs` is
every `#[repr(C)]` struct the GPU sees plus the functions that pack a component into one — **field
order there is a wire format**, matching a WGSL declaration positionally. `shaders.rs` is the WGSL
assembly seam (`with_surface`, the `Producer`s, the anchors, and the `seam_tests` that pin every
substitution actually landing). `pipelines.rs` is `with_samples` and the per-pass constructors;
`resources.rs` the caches and frame-scoped GPU resources; `shadow.rs` the shadow map and its
matrix math. Submodules are children and see the parent's private items; what they define is
`pub(crate)` and glob-imported back, so call sites read as they did when it was one file.

`mod.rs` keeps `SceneRenderer`, `ScenePass`, and the frame. **`draw` splits on the borrow, not on
the passes**: `prepare` is the `&mut self` half (uploads, uniform packing, cache maintenance) and
hands `record_shadows`/`record_scene`/`record_hud` — which need only `&self` and an encoder — a
`FramePlan`. `prepare_frame_targets` sits between the first two and **must keep that position**: it
both decides the frame's attachments and allocates the copies that decision needs, and the order
these run in is the order the GPU sees. `FramePlan` is built with field-init shorthand and
destructured by name everywhere, which is load-bearing rather than tidy — several of its fields are
the same type, and **a swapped pair of `Vec<usize>` keys is the one error class here that compiles
clean and renders wrong**. The split moved no expression: the only edits inside the moved bodies
were 25 `&x` → `x` borrows, forced because destructuring a reference yields references, and each
one is compiler-checked.

`engine-core/src/validate/` is likewise seven modules split as pure code motion — see
`designs/notes/m05-validation.md`, including why the passes take a named `SceneFacts` struct rather
than sixteen positional values.

Supporting: `schemas/component-schema.json` (generated, not hand-written), `examples/scenes/*.json`,
and `docs/`, which holds four documents. `cli-contract.md` is the wire contract and `error-codes.md`
mirrors `codes.rs`, both pinned by repo-contract tests. **`component-reference.md` is generated** —
`engine list-components --markdown` renders the same schema the flagless form publishes, and
`checked_in_component_reference_matches_the_code` fails when the committed file is stale, so it can
never become a second source of truth (invariant 7). **`scene-format.md` is prose**, covering what
the schema cannot say: internal tagging, entity names as addresses, asset paths relative to the
*scene file*, which components own their own geometry, and the cost of JSON having no comments. Its
worked example is fenced with `<!-- validated -->` and checked by a test rather than trusted — the
example in a format document is the first thing anyone copies, and the test caught two invented
`RigidBody` fields the hour it was written.

Stack: Rust + wgpu 30 (Vulkan/Metal/DX12) + winit 0.30 + glam + serde/JSON + hecs + `image` for PNG
export.

## Non-negotiable invariants

These are what make the engine agent-operable. Violating one breaks the core premise, so raise it
with the user rather than working around it:

1. **No binary scene or asset-metadata formats.** Scenes, materials, and prefabs are JSON and
   git-diffable by construction.
2. **No hidden state.** Everything needed to reconstruct a scene lives in text files on disk. No
   editor-only in-memory state; no opaque GUIDs without an in-repo lookup table.
3. **Assets are referenced by relative path, never by opaque ID.**
4. **Entities have stable `name` fields** — CLI commands and agent edits target them by name.
5. **Components are plain data.** All logic lives in systems.
6. **Errors are structured JSON on stderr, with a non-zero exit code.** Include file/line/field and a
   `did_you_mean` when a name is close to a known one:
   ```json
   {"error": "unknown_component", "entity": "Cube1", "component": "Meterial", "did_you_mean": "Material"}
   ```
   Implemented as `EngineError` in `crates/engine-core/src/error.rs`; use it rather than inventing a
   second error type. Optional context is boxed to keep the struct small, since it rides in every
   `Result` including the per-frame render path — reach for `EngineError::context()` to read it back.
   `suggest_from` fills `did_you_mean` by Levenshtein distance.
7. **Component schemas are derived from Rust structs via serde**, never maintained by hand, and scene
   files are validated against them.
8. **A GUI editor, if it ever exists, is a view onto the text files** — never a second source of
   truth.

## Settled decisions

Resolved deliberately with the user; don't relitigate without raising it.

**Scene format: JSON**, not the RON the design doc sketched. The agent loop is specified as "ordinary
bash," and `jq` is ordinary bash while RON has no equivalent. Invariant #7 wants scenes validated
against `schemas/component-schema.json` — with JSON the schema and the file are the same
serialization. And the primary user is an LLM, which edits JSON more reliably than RON. Accepted
cost: **JSON has no comments**, so anything a scene needs to say about itself must be a real field.

Components are **internally tagged** with `"type"`:

```json
{ "type": "Transform", "position": [0.0, 3.0, 0.0], "scale": [1.0, 1.0, 1.0] }
```

Note that serde's internally-tagged representation buffers during deserialization and rejects newtype
variants over non-struct types — keep components as plain structs and this stays fine.

**ECS: `hecs`** (0.11), not `bevy_ecs`. Primarily churn, not performance: `bevy_ecs` breaks every
Bevy release, and this project already spent a build cycle on wgpu's API churn — a second
fast-moving dependency at the core of the data model is the same bet twice. hecs is a small stable
API at MSRV 1.65 with 6 transitive deps and a ~1.2s cold build, against 128 deps and ~12.3s for
`bevy_ecs`. v1 has too few systems to need a scheduler; write system ordering by hand. What this
gives up: `bevy_ecs` change detection would have helped with hot reload — the one argument that could
reverse this.

**Runtime scripting: Rhai** (M10).

**Blending, crossfades and animation state machines stay rejected**, not deferred — M9 §8's
reasoning, which M30 and M32 both lean on. Blending reintroduces exactly the nondeterminism that
made two clips on one property an error. A gait change is a different `clip`.

## Open decisions — ask, don't assume

Still unsettled (design doc §9). If a task forces one, surface it rather than picking silently:

- Whether to support hot reload of scene data without a Rust rebuild

## Build order and remaining work

M0 window+triangle → M1 CLI skeleton + JSON error convention → M2 JSON scenes + ECS → M3 glTF/texture
assets → M4 materials + lighting → M5 validation hardening → M6 diff-render → M7 GUI editor (E0–E2) →
M8 physics → M9 animation (A0–A1) → M10 scripting — **the roadmap is complete.** Each milestone from
M4 on ends by running its fixture from `designs/milestone-verification-scenes.md`.

**The two that block a capability rather than polish one** — hot reload and alpha-cut leaves — are
pulled out into `designs/structural-holes.md`, with what each one costs a live demo today. (Of the
original four, entity spawning was M37 and a CPU wave evaluator was M41.) The rest, by area:

- **Editor**: E3 (structure edits), E4 (undo); picking against the *posed* mesh (CPU ray picking
  hits the rest pose).
- **M5-era**: `--fix`, watch mode.
- **Rendering**: planar reflections, cloud shadows (M38 was their prerequisite; a `Cloud` casting
  wants M16's "transparent geometry does not cast" answered), per-cascade resolution, shadows from
  point lights, spot lights, a light on the tour's explosion, a sky-dome cloud layer for cirrus and
  overcast, tree LOD and wind. The showcase tour still renders at one cascade, deliberately —
  see `m38-shadow-cascades.md`. **Alpha-cut leaves are a missing feature**, not an
  authoring job: `Tree::leaf_material` synthesizes a `Material` from `leaf_color`/`leaf_roughness`
  alone, so leaf maps mean new `Tree` fields, a schema regeneration, and a validation pass.
- **GI** (after M35): **bounced sunlight** — the largest deferral and the one a viewer notices, since
  it is what makes a coloured wall tint its neighbour under a *sun* rather than only under the sky;
  the design's §5.3 has the mechanism written. Also specular GI (a prefiltered radiance cube in the
  same volume — that is what IBL means here), point-light and emissive bounce (transfer is linear in
  intensity, so a per-light basis vector would be *exact* for a flickering campfire), dynamic
  occluders, `Water`/`Cloud` receivers, and **more than one volume** — an interior at finer spacing
  than the landscape around it, which is what the design's §4 originally allowed and what
  `multiple_light_probe_volumes` now refuses; it wants an answer to four more bindings per volume,
  not a CPU resolution rule. Geometry-level staleness is **no longer deferred** — it is
  `bake-gi --check` plus `every_committed_gi_bake_matches_its_scene`, kept out of `validate`
  because the digest needs the scene's whole triangle set (0.86 s on the tour against 0.17 s).
- **Water** (after M41): wave-driven drift (a Gerstner wave's orbital velocity would carry a float
  along with it, and wants its own answer to whether a raft eventually crosses the pond), drag on a
  submerged swimmer as distinct from a floating hull, and waves that respond to the body — which the
  purity of (file, time) currently forbids, and which is what the CPU/GPU agreement rests on.
- **Terrain** (after M42): mounds (a signed `depth`, which is a rename of `basins` rather than a
  relaxed bound), elliptical and polygonal basins, and rim noise — a basin's wall is a clean
  iso-circle, and M22 already learned that a clean curve reads as artificial; the fix belongs at
  the field level, not as seven more fields per basin.
- **Roads** (after M40, which built all five of M23's deferred items): **carving** — a road cutting
  a shelf into the `Terrain` it follows, which M40 rejected because `Terrain` owns its grid and a
  second recipe mutating it makes the ground a function of which other entities exist; it needs its
  own answer to where the height field then lives. M42 does not change that answer, but it does
  establish that an authored subtraction inside `height_at` works and that everything downstream
  follows — one of the two things carving needs. Also junction markings (stop bars, turn arrows —
  they want a lane model), roads whose *shoulder* width is authored apart from the asphalt (which
  wants the third vertex channel M40 declined to add), per-point `segment_length`, and pinned
  heights closer together than `follow_blend`, which today warn rather than compose.
- **Characters** (after M30/M32/M33/M39): planting against arbitrary colliders rather than only a
  `Terrain`, arm and hand IK with authored pole targets, and toe joints — the three M39 left for the
  IK milestone. Also getting up from a ragdoll (a return path is a blend, still rejected, or a hard
  snap), partial ragdolls (a per-joint *partition* of pose ownership, which needs a rule for the
  boundary joint), motors and therefore active ragdolls, self-collision inside one ragdoll, and
  proxies generated from vertex weights *for a `Ragdoll` specifically* — `engine fit-colliders` fits
  hitboxes, not a mass distribution.
- **UI** (after M31): a bitmap-font atlas (the sanctioned path to better text — a PNG plus an
  in-repo JSON of glyph cells, sampled nearest, no new dependency and no float, arriving as a `font`
  field whose absence is the 8×8 font), pointer lock and scroll, text input and focus, per-side
  padding, and world-space UI (a health bar over an enemy's head is a *projection* question and
  wants `world.project(x, y, z)`).
- **Game shell** (after M36): more than one save slot and a save browser (which wants a clock a
  script does not have), autosave, and a per-joint aim override so a twin-stick character can turn
  its torso without its legs — the one item here that would **reverse** a settled decision rather
  than extend one.
- **Spawning** (after M37): prefab files (`prefabs/*.json`, deferred behind an `asset` field on a
  template, exactly as `Material` does it), `PointLight` inside a template (which wants a runtime
  answer to the ≤8 budget), a `Script` inside one (runtime compilation, entangled with hot reload),
  and spawning relative to another entity. Downstream of those, in the arena: endless waves, a
  working `RETRY`, and a save that restores a mid-level arena — all now ordinary work rather than
  blocked work, and none of them built.
- **Breaking** (after M43/M44): shards that break again (needs a depth rule, and each level
  multiplies the collider set), a fracture source that is not a box, metal that dents instead of
  parting (per-step mesh mutation, which the purity of geometry-from-file forbids), per-material
  `impulse_threshold` defaults, and a *decal* where something broke — the one thing a burst cannot
  do, since particles all die. (Dust itself was M44; `designs/fracture-design.md` §7 records the
  reversal and what answered its objection.)
- **Deferred with an A/B attached**: fixing `builtin:plane`/`builtin:cube`'s UV layout, and changing
  `builtin:triangle`.

**The M31 audit's housekeeping is done**: `scene_renderer.rs` and `validate.rs` are split, the
clippy warnings are cleared and their CI step is blocking, every reproducible baseline has a test,
and both `docs/` files the design doc sketched now exist.

**Clippy is blocking in CI.** Six of the twenty-eight cleared warnings were the lint being wrong and
carry a local `#[allow]` with the reason — **read it before deleting one** (see Traps). Note the
step has to *compile*, not merely warn: `approx_constant` and friends are deny-by-default, so a
single one aborts the run for the whole crate and every later lint in it goes unreported. That
happened once, and `continue-on-error` hid it for the length of a milestone.

## Out of scope for v1

GUI editor, networking/multiplayer, ray tracing, mobile/console targets. Desktop only.

**GI came back in scope and was built (M35)** — the design doc reversed that half of this line and
nothing else, on M28's precedent (which reversed M11's "no mouse"). Ray tracing stays out, and
`designs/global-illumination-design.md` §2 rejects the ray-traced approach on its own merits rather
than on this line's authority.
