# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

**This file is the index, the invariants, and the traps.** Per-system detail lives in
`designs/notes/*.md` — one note per system, holding what building it taught. Read
`designs/agent-native-engine-design.md` before making structural decisions: it is the source of
truth for layout, formats, and build order, and several choices in it are still open (§9).

## How to read this repo's documentation

Four tiers, and knowing which one answers your question saves a search:

1. **This file** — the component vocabulary, the CLI surface, the invariants, the settled
   decisions, the cross-cutting traps, and a digest of every system with a pointer to its note.
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
| `Collider` | The shape physics sees — cuboid/sphere/capsule/trimesh/convex_hull — plus friction, density (**kg/m³**), and collision layers. | `m08-physics.md` |
| `Script` | Runs `fn step(world, step)` once per fixed step against the curated `world` API. | `m10-scripting.md` |
| `AnimationPlayer` | Plays a property clip (`*.anim.json`) or a glTF skeletal clip (`mesh.glb#Walk`); `stride`/`phase` drive a gait by ground covered. | `m09-animation.md`, `m30-skeletal-animation.md`, `m32-locomotion.md` |
| `FootPlant` | Plants a skinned character's feet on the `Terrain` under it, dropping the hips to reach. | `m32-locomotion.md` |
| `SkinnedCollider` | Hangs simple collision proxies off named joints, re-posed from the rig every step. | `m33-skinned-colliders.md` |
| `ParticleEmitter` | A seeded deterministic cone emitter around local **−Z**; M17's fields turn a smoke cone into flame. | `m13-particles-and-m17-fire.md` |
| `Breakable` | Lists pre-authored fragments and the impulse that shatters the entity into them. | `m14-breaking.md` |
| `Wheel` | One raycast-suspension wheel on its own *visual* entity, naming the chassis it drives. | `m11_5-vehicles-and-wheels.md` |
| `Water` | A body of water that **owns its surface** — Gerstner waves, depth colouring, foam, refraction. | `m18-water.md`, `m27-water-refraction.md` |
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
(sky, fog, shadows, MSAA — `m16-environment.md`), and `daylight` (the clock-driven sun, moon and
sky palette — `m21-daylight.md`).

**System order per fixed step**: animations → scripts → physics → particles → render.

## Current state

**M0–M34 are done** — the v1 roadmap (M0–M10) is complete, plus M11 keyboard input, M11.5 vehicle
dynamics, M12 wheels + HUD components + collision, M13 particles, M14 breaking, M15 frame cost,
M16 environment, M17 fire + point lights, M18 water, M19 trees, M20 clouds, M21 day/night,
M22 terrain, M23 roads, M24/M25 agent ergonomics, M26 the material system, M27 water refraction,
M28 the mouse, M29 meadows, M30 skeletal animation, M31 the UI system, M32 locomotion and foot
planting, M33 skinned collider proxies, M34 the metre. (M7 editor at scope E0–E2 + validation
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
engine ui-layout <scene.json> [--width W --height H] [--entity N]...  # where the UI landed (M31)
engine terrain-height <scene.json> --at x,z [--entity Name]  # where the ground is (M24)
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

**32 of the 38 baselines are pinned by a test.** The six that are not are the six `showcase_*`
frames, deliberately: they are not byte-reproducible on this adapter (measured repeatedly at four to
six distinct images from six renders of an *unchanged* scene, on any binary), so a test asserting
them would fail at random, which is worse than no test. They keep a `diff_args` tolerance of
`--threshold 24 --max-diff-percent 0.02` in the manifest and stay the sweep's job; `cli.rs` says so
where someone would go to add them. The pixel *allowance* is there rather than a wider threshold
because the residual is one or two pixels well outside it, not a haze just over it — 24/0.02 held
for eight consecutive full sweeps. **The other 30 entries carry no `diff_args` at all — they are
bit-exact, and a failure there is real.**

**Which tour frames flake carries no information; whether one is stable under repetition does.**
Three sweeps each picked a different subset of the six. Both binaries disagree with themselves on
the same frames, which is why the `md5`-it-N-times step is not optional — a two-artifact A/B failure
looks damning and has twice meant nothing.

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

The digest and the trap for each; the note has the rest. All paths are under `designs/notes/`.

### Foundations

- **Assets** → `assets.md`. `Mesh.asset` is a `builtin:` primitive or a `.gltf`/`.glb` path relative
  to the scene file. **Every builtin but the triangle is one metre across at scale 1**, so
  `Transform.scale` reads as a size in metres and a `Collider` matching one is always
  `half_extents: [0.5, 0.5, 0.5]` or `radius: 0.5`. Reference checks live in
  `engine-core/src/mesh.rs`; parsing lives in `engine-assets`, the only crate that opens asset files.
- **Lighting (M4)** → `m04-lighting.md`. GGX Cook-Torrance in `mesh.wgsl`. Lights aim down local
  **−Z** like the camera. A scene with *zero* light components gets a documented fallback rig; any
  light component means "absent is off". Render targets are **sRGB**, so pixel tests compute
  expectations through a `srgb_encode` helper — never eyeball byte values.
- **Validation & the CLI contract (M5)** → `m05-validation.md`. stdout is one JSON object on success
  and empty on failure; stderr is NDJSON; exit 1 is "your files" and 2 is "your invocation". **Error
  codes are API** — never rename one casually. Per-field checking is schema-driven from the same
  schema `engine list-components` publishes, with serde as a final gate. The ten cross-entity passes
  exist because a name may be authored after its use. Regenerate
  `schemas/component-schema.json` after touching any component.
- **Diff-render (M6)** → `m06-diff-render.md`. Pure comparison in `engine-render/src/diff.rs`, no
  GPU. Renders at the baseline's dimensions — re-bless to resize. Blessing is `engine screenshot`;
  there is no bless flag, deliberately.
- **Editor (M7)** → `m07-editor.md`. The scene file stays the single source of truth: the editor
  polls it and every action commits through `formatter.rs` as a **splice**, so a one-field edit is
  one hunk and untouched content is byte-identical by construction. Inspector widgets are generated
  from the component schema, so a new component is editable the day it exists.

### Simulation

- **Physics (M8)** → `m08-physics.md`. rapier3d with `enhanced-determinism`; same file + steps →
  byte-identical traces. `Transform.scale` scales collider shapes; angular velocity is degrees/sec
  at the file boundary. `--steps 0` queries need `refresh_queries()`, which is **documented
  destructive**. Bake round-trip is state-equal within ~1e-4, deliberately not byte-equal.
- **Collision (M12)** → in `m08-physics.md`. Contact queries carry one step of latency by
  construction (scripts run before physics). A trimesh on a **dynamic** body is an error — rapier
  trimeshes are hollow, use `convex_hull`. Layers interact only if the filter passes **both ways**.
- **Animation (M9)** → `m09-animation.md`. Property clips in JSON; pose is a pure function of
  (files, time), so `--time` is reproducible to a byte. **Rotation interpolates component-wise on
  Euler degrees** so a 0→360 clip actually spins — load-bearing, don't "fix" it to slerp. A clip on
  a **dynamic** body's Transform is an error; kinematic is the supported case.
- **Scripting (M10)** → `m10-scripting.md`. Rhai; `fn step(world, step)`. The curated `world` API is
  the entire universe — no time, no I/O, no randomness, 1M ops per call, so traces stay
  byte-identical with scripts running. Bake is **change-based**: any field differing from the file's
  rest value is spliced.
- **Particles (M13) and fire (M17)** → `m13-particles-and-m17-fire.md`. Seeded deterministic
  emitters, GPU-free simulation, xorshift written out in-repo so dependency upgrades cannot change
  sequences. **The random draw order is a format contract**, and each step is *skipped*, not
  defaulted, when its field is zero — a defaulted draw would move every particle baseline. Particle
  state is simulation state: created only by `--steps`, never baked, never traced.
- **Breaking (M14)** → `m14-breaking.md`. Pre-authored fragments, no runtime fracture. Breaks via
  collision impulse, `world.break_entity`, or `world.explode`. Fragments are ordinary entities
  everywhere downstream, and a baked post-break scene revalidates and re-renders bit-exactly.
- **Skinned collider proxies (M33)** → `m33-skinned-colliders.md`. **The pose drives the proxies and
  nothing reads them back** — that one sentence is the design. Kinematic bodies posed from the same
  seam the render and `list-joints` use, so a hitbox cannot disagree with the picture about where a
  head is. A proxy holds a character up as much as a moving wall holds up the hand pushing it. A
  stride-driven character's proxies lag its render by one step, causally.

### Geometry recipes

Each owns its geometry, so the entity carries no `Mesh` and no `Material`.

- **Water (M18)** → `m18-water.md`. Gerstner waves displaced in the **vertex stage** — CPU
  displacement would mint a new `Arc<MeshData>` every frame and defeat M15's cache. Sum of steepness
  ≤ 1 is exactly the non-folding condition. Waves evaluate in **world space**, so two water entities
  at one height form one continuous surface. A water scene gains a depth-copy pass. No CPU wave
  evaluator, so no buoyancy yet.
- **Water refraction (M27)** → `m27-water-refraction.md`. One field, `ior`, defaulting to no
  bending. **The exit point is solved to the bed's depth, not stepped along the refracted ray** —
  stepping dices the bed into rectangular blocks. Refraction moves *where* the bed is read from, not
  how much comes back, so it drops into a tuned scene without re-tuning it. It is only visible in
  water you can see through, over a pattern laid *across* the view direction.
- **Trees (M19)** → `m19-trees.md`. A recipe growing bark + leaf meshes, so one entity emits two
  `RenderItem`s under one name. Branch rings are carried by **parallel transport**. Three model
  rules came out of looking at renders: `whorl` is trunk-only, `tropism` is depth>0 only, and the
  trunk gives back 30% of its lean per segment because a random walk with nothing pulling on it
  drifts. There is no species enum — a species is a set of parameters.
- **Clouds (M20)** → `m20-clouds.md`. Golden-angle spiral of interpenetrating lobes. Vertex normals
  are bent 55% toward the cloud's centre, or every lobe draws its own terminator and the cluster
  reads as a bag of marbles. Culling is **off** for this pipeline alone — a cloud has no inside and
  would vanish the moment a camera entered one.
- **Terrain (M22)** → `m22-terrain.md`. The height field is **CPU-side** — the opposite of water's
  choice, because physics must stand on it and placement must query it, so there is exactly one
  implementation and nothing to keep in agreement. **Mesh normals are written in patch-local
  space**; a world-space normal arrives crushed flat and silently disables every slope-selected
  layer. Ground draws **first**. Shape fields are `NOT_ANIMATABLE`.
- **Roads (M23)** → `m23-roads.md`. Centerline is a **polygon with corner radii**, not a spline, so
  position and heading close without solving anything. **One collider for the whole ribbon** — the
  two-surface version builds a ledge that stops a car dead. Markings are computed per pixel from two
  surface coordinates, so they follow every curve and grade for free and cannot z-fight.
  `FIX_INTERNAL_EDGES` on a road's trimesh and only there.
- **Meadows (M29)** → `m29-meadows.md`. The first recipe whose subject changes shape over time, and
  the whole design is how that avoids minting a mesh per frame: two static buffers per meadow, and
  everything visible happens in the vertex stage. **`generation = floor(progress)`** with a reseed
  hash makes the cycle regrowth rather than a loop, with no state anywhere.

### Environment and time

- **Environment (M16)** → `m16-environment.md`. Sky, fog, shadows, MSAA and transparency through one
  `environment` block. **Every one defaults to off, and that is the design** — eleven baselines
  predate it and none was re-blessed. The shadow box's centre is **snapped to whole texels** or
  every edge crawls as the camera moves; casters are front-face culled as a peeling margin.
  `sky_horizon` **is** the fog colour, one field, so the two cannot disagree.
- **Point lights (M17)** → `m17-point-lights.md`. Local lights, ≤8, windowed inverse-square so past
  `range` the contribution is byte-identical to no light at all — without a hard horizon a lantern
  in one room lifts the black level of the next. Contributions are **added to the finished colour**,
  so a lamp can never darken a pixel, and a test walks every pixel to prove it.
- **Day and night (M21)** → `m21-daylight.md`. **A pure CPU function, and that is the whole design**
  — no WGSL changed, no new uniform, no new pass, so everything downstream tracks for free. Sunrise
  is 06:00 and sunset 18:00 at every elevation, deliberately. There is one directional light and it
  **is** the dominant body, swapping where luminances are equal so brightness is continuous.
  `day_length: 0` freezes the day, which is what most scenes want.
- **Frame cost (M15)** → `m15-frame-cost.md`. The viewer was slow for reasons that had nothing to do
  with particles: ~29 ms in HUD rasterization. The HUD now rasterizes only what it covers, GPU
  resources persist across frames, and `load_mesh` returns `Arc<MeshData>` — **implementations must
  return the same `Arc` for one asset**, which is both the end of a per-frame deep copy and the
  cache key. Net ~34 ms → ~0.9 ms in release.

### Materials

- **Materials (M26)** → `m26-materials.md`. Texture maps, a file form, refraction. **Every added
  field defaults to the pre-M26 behaviour.** The bind-group budget decided the shape (4 groups,
  three already spent). **Colour space is a property of the slot**, never the file — which also
  decides how the mip chain was filtered. `Material.asset` is exclusive with every other field,
  checked against raw JSON because serde defaults cannot tell an override from a spelled-out
  default. `alpha_cutoff` cuts the shadow too, through a second caster pipeline. Tangent frames are
  derived per pixel, so every recipe takes a normal map with no tangent generator.

### Characters

- **Skeletal animation (M30)** → `m30-skeletal-animation.md`. **CPU skeleton, GPU skin, and both
  halves are forced.** No new component — a skin is a property of the asset, so `AnimationPlayer.clip`
  takes `meshes/robot.glb#Walk`, and the fragment is **required** even when the file has one clip.
  Rotation is a **quaternion, slerped** here — the opposite of M9's rule, and the distinction is who
  wrote the numbers. **A skinned primitive loads unbaked**: glTF says the referencing node's
  transform is ignored, and this is the single most likely thing to be "simplified" back into a bug.
  Joint order is the skin's own and must not be sorted.
- **Locomotion and foot planting (M32)** → `m32-locomotion.md`. **The clip's phase is a field in the
  file, not state in the process, and the bake is what settled that** — ask what the bake should
  contain and the answer says whether something is state or data. Driving `speed` from a script does
  not work: `local_time` is `t * speed`, so every acceleration is a pop; phase continuity under a
  changing rate is an integral. `FootPlant` plants against a `Terrain`, not the physics world, to
  keep the pose a pure function of (files, time) — the stated cost is that a character cannot stand
  on a crate.

### Input and UI

- **Input (M11)** → `m11-input.md`. Keyboard sampled per fixed step on the shared integer clock.
  Headlessly, input is an `*.input.jsonl` timeline of sparse keyframes; no `--input` means no keys
  held, so every pre-M11 artifact is untouched. `run-scene --record-input` turns one play session
  into a permanent regression test.
- **The mouse (M28)** → `m28-mouse.md`. Buttons ride the same `held` set the keys do; the cursor is
  a **fraction of the frame**, because a timeline outlives the window it was recorded in. The engine
  computes the *ray* — `Pointer::resolve` is called by whoever already knows the camera, so the
  viewer and the headless path provably agree. A mouse-driven run is a function of frame size, which
  no earlier input was.
- **HUD (M11.6 + M12)** → `m11_6-hud.md`. Two layers, one CPU rasterizer, unit-tested without a GPU.
  `world.hud(text)` lines are cleared every step, so the line HUD is a pure function of the step that
  drew it; `HudText`/`HudRect` components are screen-anchored and bake change-based. An empty HUD
  draws nothing, keeping every pre-HUD baseline byte-identical. `simulate` reports the final step's
  lines, so the HUD is readable without pixels.
- **The UI system (M31)** → `m31-ui-system.md`. `HudPanel` removes hand-computed offsets; absent
  width/height means **hug contents**; `opacity` defaults to 0 so a bare panel is an invisible
  layout group *and* a dialog backdrop. **Flow order is file order; draw order is `(class, file
  order)`** — conflating them stacks every button above every label. Interaction is **polled, never
  dispatched**, because a button that runs code is a binding and bindings are game logic. Hit
  testing runs before scripts and is not gated on a scene having one.

### Vehicles

- **Vehicle dynamics (M11.5) and wheels (M12)** → `m11_5-vehicles-and-wheels.md`. A `Wheel` is
  raycast suspension on its own *visual* entity naming a chassis — no `RigidBody` of its own.
  Positive `engine_force` is forward, positive `steering` is left, suspension stiffness is **per kg
  of chassis mass**. Tire caveat: lateral grip is a velocity damper, so the sum of
  `0.2·side_friction_stiffness` over the wheels above 1 over-corrects and glues the car to its line.
- **The car demo and its generated circuit** → `car-demo.md`. The circuit is *generated* from a
  closed polygon of 14 corners, which is what makes closure free — a closed polygon's exterior
  angles sum to one turn, so position, heading and elevation all shut without a solver. Three
  geometry lessons are baked in and easy to reintroduce by "simplifying": one collider not two, a
  continuous guardrail, and radii sized for the car rather than the map.

### Agent ergonomics and units

- **Agent ergonomics (M24/M25)** → `m24-m25-agent-ergonomics.md`. Negative coordinates parse;
  `terrain-height`, `inspect`, per-entity `simulate` reporting, and the frame `digest`. **The digest
  is quantized to three decimals**, or an adapter that renders terrain ~24 pixels differently run to
  run turns a diagnostic into phantom diffs. Nothing may pin the digest — `diff-render` pins renders.
  Output-shape rule this settled: **schemas pretty-print, reports do not.**
- **One unit is one metre (M34)** → `m34-one-unit-is-one-metre.md`. The convention was already true
  everywhere except `builtin:sphere`, which was radius 1. The damage was in **collider pairing** —
  five of six sphere-plus-collider pairs in the repo were wrong, the scaffolded starter scene
  included, which shipped the defect to every new project. `collider_mesh_size_mismatch` is the
  warning that would have caught all six, and it is a warning because a proxy collider is ordinary
  authoring. **`Collider.density` is kg/m³ and its `1.0` default is not a plausible material.**

### The demos and shipping

- **Showcase tour** → `showcase-tour-notes.md`, design in `designs/showcase-tour.md`. A 900-step
  camera move through five stations with every system running at once. **Its growth contract is a
  test**: `showcase_tour_uses_every_component_the_engine_has` fails on any schema component the tour
  does not use, so a new component's commit adds an entity here — there is no allowlist,
  deliberately. The camera path is a closed cycle, not a timeline that ends.
- **Arena shooter** → `designs/arena-shooter.md`. The other live demo, and the worked example of the
  M31 UI system. **A full-frame `HudRect` defeats M15's scissored rasterizer** — 13.1 s vs 5.7 s for
  six frames at 1920×1080, which in a debug viewer reads as a game that has stopped responding.
- **Distribution** → `distribution-notes.md`, design in `designs/distribution-design.md`. Prebuilt
  binaries on a `v*` tag; `install.sh` verifies checksums; Linux builds on the *oldest* supported
  Ubuntu because the artifact's glibc floor is whatever built it. **CI proves the GPU-free half
  only** — baselines are a local, per-adapter check. The pinned car drive is a per-platform artifact
  and skips off aarch64 macOS. `engine init` scaffolds a project; **its scene sits at the project
  root**, because asset paths resolve relative to the scene file.

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

Deferred follow-ups, by area:

- **Editor**: E3 (structure edits), E4 (undo); picking against the *posed* mesh (CPU ray picking
  hits the rest pose).
- **M5-era**: `--fix`, watch mode.
- **Rendering**: planar reflections, shadow cascades (which is also what cloud shadows need),
  shadows from point lights, spot lights, a light on the tour's explosion, a sky-dome cloud layer
  for cirrus and overcast, tree LOD and wind. **Alpha-cut leaves are a missing feature**, not an
  authoring job: `Tree::leaf_material` synthesizes a `Material` from `leaf_color`/`leaf_roughness`
  alone, so leaf maps mean new `Tree` fields, a schema regeneration, and a validation pass.
- **Water**: a CPU wave evaluator and therefore buoyancy.
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
