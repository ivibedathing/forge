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
| `Buoyancy` | Floats a dynamic body on a named `Water`, sampling its collider in columns so a hull rights itself. | `m40-buoyancy.md` |
| `Collider` | The shape physics sees — cuboid/sphere/capsule/trimesh/convex_hull — plus friction, density (**kg/m³**), and collision layers. | `m08-physics.md` |
| `Script` | Runs `fn step(world, step)` once per fixed step against the curated `world` API. | `m10-scripting.md` |
| `AnimationPlayer` | Plays a property clip (`*.anim.json`) or a glTF skeletal clip (`mesh.glb#Walk`); `stride`/`phase` drive a gait by ground covered. | `m09-animation.md`, `m30-skeletal-animation.md`, `m32-locomotion.md` |
| `FootPlant` | Plants a skinned character's feet on the `Terrain` under it, dropping the hips to reach. | `m32-locomotion.md` |
| `SkinnedCollider` | Hangs simple collision proxies off named joints, re-posed from the rig every step. | `m33-skinned-colliders.md` |
| `ParticleEmitter` | A seeded deterministic cone emitter around local **−Z**; M17's fields turn a smoke cone into flame. | `m13-particles-and-m17-fire.md` |
| `Breakable` | Lists pre-authored fragments and the impulse that shatters the entity into them. | `m14-breaking.md` |
| `Wheel` | One raycast-suspension wheel on its own *visual* entity, naming the chassis it drives. | `m11_5-vehicles-and-wheels.md` |
| `Water` | A body of water that **owns its surface** — Gerstner waves, depth colouring, foam, refraction. Since M40 it also carries the fluid's `density`, the one field nothing renders. | `m18-water.md`, `m27-water-refraction.md`, `m40-buoyancy.md` |
| `Terrain` | A height-field patch that **owns its grid**, painted by height/slope layers; the ground everything stands on. | `m22-terrain.md` |
| `Road` | A drivable ribbon from a polygon centerline with corner radii; markings are drawn per pixel. | `m23-roads.md` |
| `Tree` | A grown tree — bark plus leaves — from a parameter recipe, not a mesh file. | `m19-trees.md` |
| `Cloud` | A cluster of interpenetrating lobes that drifts; **owns its mesh**. | `m20-clouds.md` |
| `Meadow` | Ground cover on a seed→grass→weeds→straw→collapse life cycle, animated entirely in the vertex stage. | `m29-meadows.md` |
| `HudText` | Screen-anchored text at an integer scale of the 8×8 font. | `m11_6-hud.md` |
| `HudRect` | A screen-anchored coloured rectangle — bars, backdrops, gauges. | `m11_6-hud.md` |
| `HudPanel` | Lays its children out in a row, column, or freely; hugs its contents unless sized. | `m31-ui-system.md` |
| `HudImage` | A nine-sliced textured rectangle. **With no `slice` it is all middle band, and the middle band tiles.** | `m31-ui-system.md` |
| `HudInteract` | Makes the HUD element on its own entity hoverable, pressable and clickable — polled, never dispatched. | `m31-ui-system.md` |

**Recipes own their geometry**, so `Water`, `Terrain`, `Road`, `Cloud` and `Meadow` carry **no
`Mesh` and no `Material`** — authoring one is a validation error. A `Tree` is the exception on
materials only: the entity's `Material` is its bark.

**Scene-level blocks**, siblings of `entities`: `physics` (gravity, `timestep_hz`), `environment`
(sky, fog, shadows and their cascades, MSAA — `m16-environment.md`, `m38-shadow-cascades.md`,
**script-writable since M36**), and `daylight` (the clock-driven sun, moon and sky palette —
`m21-daylight.md`).

**System order per fixed step**: animations → scripts → physics → particles → render.

## Current state

**M0–M36, M38 and M40 are done** — the v1 roadmap (M0–M10) is complete, plus M11 keyboard input, M11.5 vehicle
dynamics, M12 wheels + HUD components + collision, M13 particles, M14 breaking, M15 frame cost,
M16 environment, M17 fire + point lights, M18 water, M19 trees, M20 clouds, M21 day/night,
M22 terrain, M23 roads, M24/M25 agent ergonomics, M26 the material system, M27 water refraction,
M28 the mouse, M29 meadows, M30 skeletal animation, M31 the UI system, M32 locomotion and foot
planting, M33 skinned collider proxies, M34 the metre, M36 the game shell, M38 shadow cascades,
M40 buoyancy. (M35 is a design doc only — global illumination, not built. M7 editor at scope
E0–E2 + validation panel + `--watch`.)

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
#   without reading the image (M25)
engine raycast <scene.json> --from x,y,z --dir x,y,z [--steps N] [--input f]
engine filmstrip <scene.json> --out strip.png [--start S --end E --frames N --columns C]
engine list-animations <scene-or-clip> [--schema]  # glTF clips too, with their channel targets (M30)
engine list-joints <scene-or-mesh> [--entity Name] [--time T] [--steps N]
#   the rig and where it is (M30); --steps for a pose the simulation reached, and a
#   measured `stride` when the entity has a FootPlant (M32)
engine road-centerline <scene.json> [--entity Name]  # where a Road actually went
engine list-colliders <scene.json> [--entity Name] [--steps N] [--input f]
#   every collider physics holds — shape, size, world placement — read back out of the
#   built world, so a skinned hitbox nothing renders is still answerable (M33)
engine ui-layout <scene.json> [--width W --height H] [--entity N]... [--steps N] [--input f]
#   where the UI landed (M31); --steps reports what a script *painted* (M36)
engine terrain-height <scene.json> --at x,z [--entity Name]  # where the ground is (M24)
engine water-height <scene.json> --at x,z [--entity N] [--time T] [--steps N]
#   where the water is, and which way it faces (M40); the first query that takes a
#   clock, and the first that can answer "no water here" rather than a height
engine inspect <scene.json> [--entity Name]  # every field resolved, defaults filled in (M24)
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
`road-centerline`, `list-joints`, `list-colliders` and `ui-layout` rather than re-deriving any of
them — a generator that re-derives a curve is how two implementations start disagreeing.

## Traps that cost time

The cross-cutting ones. Per-system traps are in each note.

- **`mesh.wgsl`'s four lighting lines are ULP-sensitive.** The lines computing
  `direct`/`ambient`/`base_color` are the M4 originals and must reach the compiler surrounded by the
  code they shipped in. Terrain, textures, refraction and skinning are therefore **splices**
  (`with_surface`, anchored substitutions asserted to land exactly once), never inline branches.
  Restructuring them into arithmetic *equal on paper* moved one pixel by one ULP — measured three
  separate times. Compiler FMA contraction depends on surrounding code.
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
  *file*: a scene that gains a body re-blesses.
- **Grep the `.rhai` files for `set_scale` before believing a scale-space change is complete.** Two
  scripts drive scale every step from a hard-coded constant, so editing the scene file achieved
  nothing and the coals rendered at half size (M34).
- **Four shaders sample the shadow map, and they do not declare the same frame uniform.**
  `mesh.wgsl`, `water.wgsl`, `road.wgsl` and `meadow.wgsl` each carry their own near-copy of the
  lookup, so anything that changes the map's *binding type* changes all four together or fails at
  pipeline creation. And `water.wgsl`'s `FrameUniform` stops at `params`: uniform field offsets are
  positional, so a field appended after `point_lights` is unreachable from water without giving it
  an eight-light array it never reads. **Check all four before appending to a shared uniform** (M38).
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
  predates M40 and is documented in `simulate.rs`, but water is the first thing in the script API
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

**34 of the 40 baselines are pinned by a test.** The six that are not are the six `showcase_*`
frames, deliberately: they are not byte-reproducible on this adapter (measured repeatedly at four to
six distinct images from six renders of an *unchanged* scene, on any binary), so a test asserting
them would fail at random, which is worse than no test. They keep a `diff_args` tolerance of
`--threshold 24 --max-diff-percent 0.02` in the manifest and stay the sweep's job; `cli.rs` says so
where someone would go to add them. The pixel *allowance* is there rather than a wider threshold
because the residual is one or two pixels well outside it, not a haze just over it — 24/0.02 held
for eight consecutive full sweeps. **The other 31 entries carry no `diff_args` at all — they are
bit-exact, and a failure there is real.**

**Which tour frames flake carries no information; whether one is stable under repetition does.**
Five separate sweeps each picked a different subset of the six, M36's and M38's A/Bs included.
Every time, the differing frame had a binary disagreeing with **itself** — which is why the
`md5`-it-N-times step is not optional. Five measurements, five times the answer was the adapter.

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
- **Skinned collider proxies (M33)** → `m33-skinned-colliders.md`. Simple shapes hung off named
  joints and re-posed from the rig each step, so a skinned character can be hit and can push things.

### Geometry recipes

Each owns its geometry, so the entity carries **no `Mesh` and no `Material`**.

- **Water (M18)** → `m18-water.md`. A body of water with Gerstner waves displaced in the vertex
  stage, depth colouring and shore foam.
- **Water refraction (M27)** → `m27-water-refraction.md`. One `ior` field bending what is seen
  through the surface, defaulting to no bending.
- **The wave evaluator and buoyancy (M40)** → `m40-buoyancy.md`, design in
  `designs/buoyancy-design.md`. The Gerstner sum mirrored on the CPU so `engine water-height`,
  `world.water_height` and a floating `Buoyancy` body can all ask where the surface is — held to the
  shader by a GPU agreement test that reads the drawn surface back out of a render.
- **Trees (M19)** → `m19-trees.md`. A grown tree — bark plus leaves — from a parameter recipe rather
  than a mesh file.
- **Clouds (M20)** → `m20-clouds.md`. Drifting clusters of interpenetrating lobes.
- **Terrain (M22)** → `m22-terrain.md`. A CPU height-field patch painted by height and slope layers;
  the ground everything else stands on.
- **Roads (M23)** → `m23-roads.md`. A drivable ribbon from a polygon centerline with corner radii,
  its markings drawn per pixel.
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

**The three that block a capability rather than polish one** — entity spawning, hot reload and
alpha-cut leaves — are pulled out into `designs/structural-holes.md`, with what each one costs a live
demo today. (The fourth, a CPU wave evaluator, was M40.) The rest, by area:

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
- **Water** (after M40): wave-driven drift (a Gerstner wave's orbital velocity would carry a float
  along with it, and wants its own answer to whether a raft eventually crosses the pond), drag on a
  submerged swimmer as distinct from a floating hull, and waves that respond to the body — which the
  purity of (file, time) currently forbids, and which is what the CPU/GPU agreement rests on.
- **Roads** (after M23): junctions (two roads crossing wants a patch primitive, not a ribbon),
  banked cross-sections, per-point road width, roads that follow a `Terrain`, and asphalt grain.
- **Characters** (after M30/M32/M33): ragdolls (physics writing the skeleton, which is the one-way
  rule reversed and wants its own answer to where the pose then comes from), proxies that resize
  with the posed bone, generating a proxy set from vertex weights, planting against arbitrary
  colliders, arm and hand IK with authored pole targets, toe joints.
- **UI** (after M31): a bitmap-font atlas (the sanctioned path to better text — a PNG plus an
  in-repo JSON of glyph cells, sampled nearest, no new dependency and no float, arriving as a `font`
  field whose absence is the 8×8 font), pointer lock and scroll, text input and focus, per-side
  padding, and world-space UI (a health bar over an enemy's head is a *projection* question and
  wants `world.project(x, y, z)`).
- **Game shell** (after M36): more than one save slot and a save browser (which wants a clock a
  script does not have), autosave, restoring a mid-level arena (which wants entity spawning, the
  arena shooter's oldest constraint), and a per-joint aim override so a twin-stick character can
  turn its torso without its legs — the one item here that would **reverse** a settled decision
  rather than extend one.
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

GUI editor, networking/multiplayer, advanced rendering (GI, ray tracing), mobile/console targets.
Desktop only.
