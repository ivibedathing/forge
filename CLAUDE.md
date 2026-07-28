# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Current state

**M0–M12 are done — the v1 roadmap (M0–M10) is complete, plus M11 keyboard input and M12 vehicle wheels** (and most of M1's CLI; M7 at scope E0–E2 + validation panel + --watch).
JSON scenes load into hecs, render headlessly to PNG with PBR lighting, validate with
all-errors-at-once reporting under a formalized CLI contract, reference glTF mesh files, pin
their renders against committed baselines with `engine diff-render`, and open in a GUI editor
that is a live writable *view* onto the file. Verified by 170+ tests including offscreen pixel
readback and an end-to-end CLI suite, and by the verification fixtures from
`milestone-verification-scenes.md` (`verify/m4_lighting.json` diff-renders bit-exactly against
`verify/baselines/m4_lighting.png`; `verify/m5_broken.json` is committed **broken** and must
never validate — its failing with all seven planted errors is the pass condition, pinned by
`repo_contracts.rs`). **From M6 on, the standard check's "look at the PNGs" step is
`engine diff-render` against the committed baselines** — each later milestone adds its scene's
baseline in the same commit that adds the scene.

What works today:

```
engine validate <scene.json>... [--strict]  # every error at once; multi-file; --strict promotes warnings
engine screenshot <scene.json> --out x.png [--steps N] [--input f] [--time T] [--camera N] [--width W --height H]
engine diff-render <scene.json> <baseline.png> [--steps N] [--input f] [--time T] [--out diff.png] [--threshold N] [--max-diff-percent P]
engine edit <scene.json> [--watch]       # GUI editor; --watch = read-only supervision mode
engine simulate <scene.json> --steps N [--input f] [--bake out.json] [--trace t.jsonl]
engine raycast <scene.json> --from x,y,z --dir x,y,z [--steps N] [--input f]
engine filmstrip <scene.json> --out strip.png [--start S --end E --frames N --columns C]
engine list-animations <scene-or-clip> [--schema]
# Script component: {"type": "Script", "source": "scripts/x.rhai"} — runs fn step(world, step)
# once per fixed step (order: animations → scripts → physics → render)
# Wheel component (M12): raycast-suspension wheel on its own visual entity, chassis by name —
# physics suspends/drives the chassis and writes the wheel's Transform back (steer/spin/bounce)
engine run-scene <scene.json> [--record-input f]   # windowed viewer + play mode (keyboard reaches scripts)
engine list-components                   # scene + component JSON Schemas (with range constraints)
engine build [--check]                   # cargo build/check, diagnostics re-emitted as engine errors
engine run                               # M0 triangle (stack proof)
engine info                              # selected GPU adapter as JSON
```

Editor (M7, `crates/engine-editor`): egui **git-pinned to a master commit** (see the workspace
Cargo.toml comment — released egui pairs with wgpu 29; swap to the 0.36 release when it ships).
The scene file stays the single source of truth: the editor polls the file (250ms) and reloads
on any external change; every editor action commits through
`engine-core/src/formatter.rs` — a *splice*, not a serialize, so a one-field edit is one hunk on
one line and untouched content is byte-identical by construction (`cargo test -p engine-core
formatter` pins this). Commits rebase onto a fresh read by entity `name` + component `type`
(never index); a vanished target drops the edit with a status-bar notice. Inspector widgets are
generated from the component schema (a new component is editable the day it exists); the
validation panel shows the same `EngineError`s the CLI emits, click-to-select. Viewport =
`SceneRenderer` into an offscreen texture (same pipeline as `engine screenshot`), orbit camera
(right-drag; shift/middle = pan, scroll = zoom), CPU ray picking, hand-rolled transform gizmos
— `W` translate / `R` rotate / `S` scale switch modes, world axes mapping straight to
`Transform` field components (the X ring adds degrees to `rotation[0]`, etc.), preview in
memory, one write on release. Drag-and-drop import (`engine-editor/src/import.rs`): a dropped
`.glb`/`.gltf` is referenced in place or copied to `meshes/` beside the scene; a `.blend` is
converted to `.glb` by running Blender headlessly (`$BLENDER` → `PATH` → macOS app bundle;
absent Blender = `blender_not_found`), then one entity (`Transform` + `Mesh`, name deduped
from the file stem) is spliced in via `formatter::apply_add_entity` — the first structure
edit, E3's opening move. Conversion runs on a worker thread; the Blender-gated end-to-end
test skips cleanly when Blender is missing. A "+ add" menu beside the entities heading
splices a `builtin:` primitive the same way (Transform at origin + Mesh, name deduped from
the primitive) — its entries are generated from `BuiltinMesh::ASSETS`, so a new builtin
appears in the menu for free. The inspector adds and removes components: "+ add
component" lists the schema types the entity lacks and splices `{ "type": X }` (absent
fields *are* the documented defaults) via `formatter::apply_add_component`; a header ❌
removes one via `apply_remove_component` — both rebase by entity name + component type like
every mutation. `[0, 1]` RGB triples (`albedo`, `emissive`, light `color`) get a linear-RGB
color-picker swatch that commits one write per picker session. Hidden flag `--self-screenshot <png>
[--self-screenshot-after-ms N]` renders the editor and exits — the agent's way to *look at* the
editor. `RenderItem` gained an `entity: String` field for picking/selection.

Diff-render (M6): the pure comparison lives in `engine-render/src/diff.rs` (no GPU — unit-tests
everywhere); the CLI decodes the baseline, renders at the baseline's dimensions (no
--width/--height; re-bless to resize), and reports pass/fail with `diff_pixels`,
`max_channel_delta`, and `diff_bounds`. Defaults are bit-exact; determinism is promised
same-machine/same-adapter only, so baselines are per-adapter artifacts and the report carries
the adapter name. The diff PNG's three pixel classes (red violation / yellow within-threshold /
faded-gray identical) are pinned formulas — see `docs/cli-contract.md`. Blessing is
`engine screenshot` — no separate bless flag, deliberately. The report prints on both pass and
fail (a documented stdout exception).

`Mesh.asset` is `builtin:cube` / `builtin:cylinder` / `builtin:plane` / `builtin:sphere` /
`builtin:triangle`, or a `.gltf`/`.glb` path relative to the scene file. Reference checks (existence, extension,
absolute-path rejection) live in `engine-core/src/mesh.rs` (`MeshAsset::resolve`); actual file
parsing lives in `engine-assets` (the only crate that opens asset files — glTF meshes plus
PNG→RGBA8 textures, the latter awaiting texture-mapped materials). `engine validate` runs both
passes, so a corrupt glTF fails validation, not just the screenshot. `Scene::render_items` takes
a `MeshSource`: `AssetServer::for_scene` in the CLI, `BuiltinAssets` in GPU-less tests.
`examples/meshes/pyramid.gltf` is generated text glTF (embedded base64 buffer) — flat-shaded,
CCW-wound, the file the example scene and asset tests load.

Lighting (M4): `DirectionalLight` + `AmbientLight` components (at most one each per scene,
validated), `Material.emissive`, and a GGX Cook-Torrance shader in
`engine-render/src/shaders/mesh.wgsl`. Lights aim down their entity's local **−Z** like the
camera; a scene with *zero* light components gets the documented fallback rig
(`LightRig::resolved` in `engine-core/src/scene.rs`), while any light component means "absent is
off". Render targets are **sRGB** (`Rgba8UnormSrgb`): scene colors are linear reflectance, the
hardware encodes on write, and pixel tests compute expectations via the `srgb_encode` helper in
`engine-render/tests/lighting.rs` — never eyeball byte values. Line numbers on semantic errors
come from `engine-core/src/lineindex.rs` (path → line lookup; serde_json discards spans).

Validation & the CLI contract (M5): the wire contract lives in `docs/cli-contract.md` — stdout
is one JSON object on success and empty on failure, stderr is NDJSON, exit codes split 1 ("your
files are at fault") from 2 ("your invocation/environment is"). Every error code is a const in
`engine-core/src/codes.rs` with its exit class; `docs/error-codes.md` mirrors it and a
repo-contract test keeps them in lockstep — **codes are API**, never rename one casually.
Per-component field checking is **schema-driven**: the walk in `validate.rs` reads the same
schemars-generated schema `engine list-components` publishes (unknown/missing fields, JSON
types, `minimum`/`exclusiveMinimum`-style ranges authored as `#[schemars(...)]` attributes on
the component structs), then serde parses the clean component as a final gate —
`scene_parse_desync` firing means the walk and the parser drifted, and the corpus tests in
`engine-core/tests/validation_corpus.rs` (agreement, robustness, golden kitchen-sink snapshot)
exist to catch that before an agent does. Errors carry `path` (a JSON Pointer for `jq`) next to
`line`; warnings (`unused_material`, `zero_scale`) ride the same stream with
`"severity": "warning"` and exit 0 unless `--strict`. Cross-field checks (`Camera.far > near`)
and `duplicate_component` are semantic checks beyond the schema. `Scene::from_source` errors
with `Vec<EngineError>` — screenshot/run-scene report byte-identical diagnostics to `validate`.
A panic hook keeps even a crash inside the NDJSON protocol (`internal_panic`, exit 2), and clap
failures are re-rendered as `invalid_invocation` JSON with clap's own `did_you_mean`. The
checked-in `schemas/component-schema.json` is enforced by `engine-core/tests/repo_contracts.rs`
— regenerate with `engine list-components > schemas/component-schema.json` after touching any
component, including its range attributes.

Physics (M8, `crates/engine-physics`): **rapier3d pinned =0.34.0** with `enhanced-determinism`
— and note rapier 0.34 switched to a glam-based math backend (glamx) sharing our exact glam
version, so no conversion layer exists; read the API in the registry when touching it, and treat
any golden-trace diff on a rapier upgrade as a breaking change to review. `RigidBody` (body:
dynamic/kinematic/fixed) + `Collider` (flat struct, `shape` cuboid/sphere/capsule with per-shape
fields enforced semantically) + optional scene-level `physics` block (`gravity`,
integer `timestep_hz`). `Transform.scale` scales collider shapes (the fixture's ground collider
is authored in *local* units for this reason); restitution combines by **max**, documented on
the component. Angular velocity is degrees/sec (file convention), converted at the rapier
boundary. Determinism: same file + steps → byte-identical traces, pinned by the committed
golden `verify/baselines/m8_drop.trace.jsonl` and CLI tests. **Bake round-trip is
state-equal within ~1e-4, deliberately not byte-equal**: baking quantizes to Euler-degree f32
text and drops solver caches (disposable by design), so a resumed run drifts by float ulps —
the CLI test pins the tolerance property. `--steps 0` scene queries need
`PhysicsWorld::refresh_queries()` (the broad-phase BVH is otherwise only built inside `step`).
The windowed viewer steps the same fixed dt through a wall-clock accumulator; headless is
canonical. Physics tests are GPU-free and unconditional — no skip path.

Animation (M9, property clips; skeletal glTF deferred like editor E3/E4): clips are JSON
(`*.anim.json`, schema in `schemas/animation-schema.json`, regenerated via
`engine list-animations --schema`), animating `Component.field` on entities by name. Pose is a
pure function of (files, time) — `--time` on screenshot/diff-render is reproducible down to
`cmp`-identical PNGs, and t=loop-period equals t=0 byte-for-byte (pinned by CLI test).
**Rotation interpolates component-wise on Euler degrees** so a 0→360 clip actually spins
(quaternion slerp would no-op it) — load-bearing, don't "fix" it. Sampling lives in
`engine-core/src/animation.rs` (step/linear/cubic Catmull-Rom); `set_field` must cover every
numeric schema field — a drift test walks the schema and calls it. System order: sample
animations → physics → render. The M8×M9 ownership rule is settled: a clip animating the
Transform of a **dynamic** body is `animation_on_dynamic_body` (kinematic is the supported
"animation drives, physics follows" case). Clip-content errors carry the clip file's own
file/line; `engine validate` accepts clip files directly (structural checks only).

Scripting (M10, `crates/engine-script`): **Rhai pinned =1.25.1** — the §9 decision is settled
(see `scripting-design.md` §1; Lua lost on the C dependency and determinism friction,
compiled-Rust-only lost on rebuild-per-iteration). Scripts define `fn step(world, step)`; the
curated `world` API (dt / position / rotation / scale get+set) is the entire universe — no
time, no I/O, no randomness, 1M-operation budget per call, so traces stay byte-identical with
scripts running. Script parse errors fail `engine validate` with the script's file/line;
runtime errors are `script_runtime_error`, exit 1, world intact. Bake is change-based: any
`Transform`/`RigidBody` field differing from the file's rest value is spliced — which is how
script-driven kinematics land in baked files. Kinematic-vs-fixed contact events are opted in
via `ActiveCollisionTypes` (rapier skips them by default; a scripted platform crossing a
static sensor needs them). Bake next to the scene, not /tmp — relative paths.

Input (M11, `input-design.md`): keyboard input sampled per fixed step on the shared integer
clock — scripts ask `world.key("ArrowUp")` (unknown names are runtime errors with
`did_you_mean`). Live keys exist only in `engine run-scene`; headlessly, input is an
`*.input.jsonl` timeline (sparse keyframes of the complete held set, in effect from their
0-based `step` until the next line; strictly increasing steps) replayed via `--input` on
simulate/screenshot/diff-render/raycast — same timeline, byte-identical results, and no
`--input` means no keys held, so all pre-M11 traces/baselines are untouched.
`run-scene --record-input` writes a timeline whenever the held set changes: record a play
session once, regression-test it forever. Key names are the curated W3C-code allowlist in
`engine_core::input::KNOWN_KEYS`. `world.look_at(name, x, y, z)` aims an entity's local −Z
with a level horizon (pitch+yaw through the XYZ Euler order would roll — that's why it
exists); the viewer re-resolves the camera transform every frame so scripts can drive a chase
camera, and the headless commands already resolved it after stepping.

Vehicle dynamics (M11.5): scripts read/write `RigidBody` velocities — `world.linear_velocity`
/ `set_linear_velocity` (m/s) and `angular_velocity` pair (deg/s) — and `PhysicsWorld::step`
pushes a dynamic body's component velocity into rapier **only when it differs from what
physics last wrote back** (cache in `written_velocities`), so the deg↔rad round-trip never
touches untouched runs and the M8 golden trace stays byte-identical. `RigidBody.
locked_rotations: [bool; 3]` maps to rapier `LockedAxes`. `world.forward(name)` returns the
entity's world -Z — **required** for heading math:
XYZ Euler clamps the middle angle to ±90°, so physics-integrated yaws past that come back as
the `(±180, θ, ±180)` twin and `rotation[1]` stops being "the yaw" (this bug cost a debugging
session; the twin is also why `animation.rs::field_shape` only treats arrays *of numbers* as
animatable).

Wheels (M12): the `Wheel` component is one raycast-suspension wheel — it sits on its own
*visual* entity (Transform + cylinder Mesh, **no** RigidBody/Collider of its own, enforced by
`wheel_with_physics`) and names its chassis in `vehicle` (a different entity with a dynamic
RigidBody + Collider; `wheel_vehicle_not_found` / `wheel_vehicle_invalid`). engine-physics
groups wheels by chassis name (both levels name-sorted for determinism) into rapier
`DynamicRayCastVehicleController`s: suspension spring/damper per wheel (stiffness is **per kg
of chassis mass**; static sag ≈ `9.81/(4·stiffness)`), drive/brake/steer at the contact
point. Conventions: up +Y, forward −Z via `index_forward_axis = 2` + axle +X (drive direction
is `normal × axle = −Z`, so positive `engine_force` is forward); positive `steering` (degrees)
steers **left**. `Wheel.offset` is chassis-local meters, rotated but **not** scaled by
`Transform.scale`. Control fields (`engine_force`/`brake`/`steering`) are runtime inputs like
`RigidBody` velocities: scripts write them (`world.set_engine_force/set_brake/set_steering`
+ getters), physics reads them each step and wakes the chassis itself (rapier only wakes on
*positive* engine force). Physics writes each wheel entity's Transform back every step —
post-step chassis pose + ray length + steer yaw + accumulated spin, ×`Qz(90°)` mapping the
builtin cylinder's Y axis onto the axle — so wheels visibly bounce, steer, and roll in
screenshots. Vehicle worlds call `refresh_queries()` at build (suspension rays run before the
first pipeline step builds the BVH); vehicle-free worlds skip everything, keeping M8 golden.
**Tire model caveats** (bullet port): lateral grip is a velocity damper — side impulse =
`0.2 · side_friction_stiffness · lateral_vel · effective_mass` per wheel per step, so the sum
of `0.2·side_friction_stiffness` over wheels is the fraction of lateral velocity removed per
step (>1 over-corrects and glues the car); `friction_slip` is the skid clamp as a multiple of
suspension load — its 10.5 default never saturates and a large sideslip then wipes all
momentum; ≈1.0 gives a physical μ≈0.9 tire that slides instead of sticking. Demo:
`examples/scenes/car_track.json` — box chassis (`builtin:cube`, ≈1.5 t via density) + four
cylinder wheels; `scripts/car.rhai` is now only the *driver* (pedals, speed-scaled steering
wheel with finite slew rate, low-gear torque boost below 8 m/s so full-lock corners don't
stall against front-tire slip drag, chase camera). `car_track_lap.input.jsonl` is a committed
recording (closed-loop autopilot: simulate a 10-step chunk from step 0 → bake → read state →
next keys) driving three clockwise laps on real suspension and parking on the start line —
pinned by CLI test and `verify/baselines/m11_lap.png`.

Read `agent-native-engine-design.md` before making structural decisions; it is the source of truth
for layout, formats, and build order, and several choices in it are still open (§9).

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
from memory. winit is pinned to the **0.30** stable line; 0.31 is still beta.

## Verification

`cargo test --workspace` is the real check, not `cargo build`. `crates/engine-render/tests/
headless_render.rs` renders offscreen and asserts on pixel values, because "the window opened and
did not crash" does not distinguish a working renderer from a culled triangle or a shader that
writes nothing. Those tests skip cleanly (rather than fail) when no GPU is available.

Backface culling is **on**, and the M0 triangle is wound counter-clockwise in clip space to match
wgpu's default front face. A wrongly-wound triangle renders nothing at all — if geometry is
invisible, suspect winding before suspecting the pipeline.

## What this project is

An agent-native 3D engine in Rust: a game engine whose primary "user" is an AI coding agent rather
than a human in a GUI editor. The design constraint driving every other decision is the agent
feedback loop — edit a text file → validate → build → render a PNG headlessly → *look at it* →
iterate — using only ordinary bash and file edits, with no bespoke integration layer.

This inverts the usual engine tradeoff: machine-legibility beats GUI convenience wherever the two
conflict.

## Architecture

Planned Cargo workspace (design doc §4), dependency order bottom-up:

- `crates/engine-core` — ECS, scene graph, math re-exports (glam)
- `crates/engine-render` — wgpu renderer, shaders, materials
- `crates/engine-assets` — mesh/texture loading, asset schema
- `crates/engine-cli` — the `engine` binary; the primary interface

Supporting: `schemas/component-schema.json` (generated, not hand-written), `examples/scenes/*.json`,
`docs/component-reference.md` (generated from doc comments).

Stack: Rust + wgpu 30 (Vulkan/Metal/DX12) + winit 0.30 + glam + serde/JSON + hecs + `image` for
PNG export.

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
6. **Errors are structured JSON on stderr, with a non-zero exit code.** Include file/line/field and
   a `did_you_mean` when a name is close to a known one:
   ```json
   {"error": "unknown_component", "entity": "Cube1", "component": "Meterial", "did_you_mean": "Material"}
   ```
   Implemented as `EngineError` in `crates/engine-core/src/error.rs`; use it rather than inventing
   a second error type. Optional context is boxed to keep the struct small, since it rides in every
   `Result` including the per-frame render path — reach for `EngineError::context()` to read it
   back. `suggest_from` fills `did_you_mean` by Levenshtein distance.
7. **Component schemas are derived from Rust structs via serde**, never maintained by hand, and
   scene files are validated against them.
8. **A GUI editor, if it ever exists, is a view onto the text files** — never a second source of
   truth.

## Commands (target CLI — mostly not yet present, see "Current state")

```
engine build                                     # compile; structured errors
engine validate <scene.json>                     # schema-check a scene
engine run-scene <scene.json>                    # windowed viewer
engine screenshot <scene.json> --out out.png [--camera Player] [--width 1280 --height 720]
engine list-components                           # dump all component schemas as JSON
engine diff-render <scene.json> <baseline.png> --out diff.png
```

`engine screenshot` is the single most important command in the project — it is what closes the
agent's edit→see loop. Prioritize it accordingly; keep it headless and keep it fast.

Standard Cargo commands: `cargo build`, `cargo test --workspace`, and
`cargo test -p engine-core <test_name>` for a single test.

## Build order

Follow the milestones in design doc §8: ~~M0 window+triangle~~ → ~~M1 CLI skeleton + JSON
error convention~~ → ~~M2 JSON scenes + ECS~~ → ~~M3 glTF/texture assets~~ → ~~M4 materials +
lighting~~ → ~~M5 validation hardening~~ → ~~M6 diff-render~~ → ~~M7 GUI editor (E0–E2)~~ →
~~M8 physics~~ → ~~M9 animation (A0–A1)~~ → ~~M10 scripting~~ — **the roadmap is complete.**
Remaining deferred follow-ups: editor E3 (structure edits) / E4 (undo), M9-A2 skeletal glTF +
GPU skinning, and the M5-era deferrals (--fix, watch mode).
Each milestone from M4 on ends by running its fixture from `milestone-verification-scenes.md`.

M1's `engine screenshot` is mostly plumbing that already exists: `Renderer::draw` takes any
`TextureView`, `Gpu::new` takes an optional surface, and the readback path (texture → buffer →
pixels) is written in `tests/headless_render.rs`. Lifting that into the CLI is the work. One thing
the test dodges: it uses a 256px-wide target so rows are already 256-byte aligned. Arbitrary
`--width` values need real `COPY_BYTES_PER_ROW_ALIGNMENT` padding and unpadding.

## Settled decisions

Resolved deliberately with the user; don't relitigate without raising it.

**Scene format: JSON**, not the RON the design doc sketched. The agent loop is specified as
"ordinary bash," and `jq` is ordinary bash while RON has no equivalent. Invariant #7 wants scenes
validated against `schemas/component-schema.json` — with JSON the schema and the file are the same
serialization, so third-party tooling can validate too. And the primary user is an LLM, which edits
JSON more reliably than RON. Accepted cost: **JSON has no comments**, so anything a scene needs to
say about itself must be a real field, not a `//`.

Components are **internally tagged** with `"type"`:

```json
{ "type": "Transform", "position": [0.0, 3.0, 0.0], "scale": [1.0, 1.0, 1.0] }
```

Note that serde's internally-tagged representation buffers during deserialization and rejects
newtype variants over non-struct types — keep components as plain structs and this stays fine.

**ECS: `hecs`** (0.11), not `bevy_ecs`. Primarily churn, not performance: `bevy_ecs` is 0.19 and
breaks every Bevy release, and this project already spent a build cycle on wgpu's API churn — a
second fast-moving dependency at the core of the data model is the same bet twice. hecs is a small
stable API at MSRV 1.65 with 6 transitive deps and a ~1.2s cold build, against 128 deps and ~12.3s
for `bevy_ecs`. v1 has too few systems to need a scheduler; write system ordering by hand.

What this gives up: `bevy_ecs` change detection would have helped with hot reload. If hot reload
becomes a priority, that tradeoff is worth revisiting — it is the one argument that could reverse
this.

## Open decisions — ask, don't assume

Still unsettled (design doc §9). If a task forces one, surface it rather than picking silently:

- ~~Runtime scripting~~ — settled: Rhai (M10, `scripting-design.md`)
- Whether to support hot reload of scene data without a Rust rebuild

## Out of scope for v1

GUI editor, networking/multiplayer, advanced rendering (GI, ray tracing), mobile/console targets.
Desktop only.
