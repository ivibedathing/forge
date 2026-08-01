# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

Full rationale for each system lives in `designs/*.md`; this file is the index plus the list of
things that cost time. Read `designs/agent-native-engine-design.md` before making structural
decisions — it is the source of truth for layout, formats, and build order, and several choices in
it are still open (§9).

## Current state

**M0–M31 are done** — the v1 roadmap (M0–M10) is complete, plus M11 keyboard input, M11.5 vehicle
dynamics, M12 wheels + HUD components + collision, M13 particles, M14 breaking, M15 frame cost,
M16 environment, M17 fire + point lights, M18 water, M19 trees, M20 clouds, M21 day/night,
M22 terrain, M23 roads, M24/M25 agent ergonomics, M26 the material system, M27 water refraction,
M28 the mouse, M29 meadows, M30 skeletal animation, M31 the UI system. (M7 editor at scope E0–E2 +
validation panel + `--watch`.)

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
engine list-joints <scene-or-mesh> [--entity Name] [--time T]  # the rig, and where it is (M30)
engine road-centerline <scene.json> [--entity Name]  # where a Road actually went
engine ui-layout <scene.json> [--width W --height H] [--entity N]...  # where the UI landed (M31)
engine terrain-height <scene.json> --at x,z [--entity Name]  # where the ground is (M24)
engine inspect <scene.json> [--entity Name]  # every field resolved, defaults filled in (M24)
engine run-scene <scene.json> [--record-input f]   # windowed viewer + play mode; keyboard AND mouse; FPS readout is viewer-only
engine init [dir] [--force]              # scaffold a project: starter scene + AGENTS.md/CLAUDE.md
engine agent-guide                       # the agent orientation as markdown (a stdout exception)
engine import <model.glb> [--into scene.json] [--textures dir] [--materials dir]  # glTF materials (M26)
engine list-components [--component Name]  # scene + component JSON Schemas (with range constraints)
engine build [--check]                   # cargo build/check, diagnostics re-emitted as engine errors
engine run                               # M0 triangle (stack proof)
engine info                              # selected GPU adapter as JSON
```

Component quick-reference (details below): `Material` carries texture maps, or *is* a
`materials/*.json` file (M26). `Script` runs `fn step(world, step)` once per fixed step
(order: animations → scripts → physics → particles → render). `Wheel` is raycast suspension on its
own visual entity naming a chassis. `ParticleEmitter` is a seeded deterministic emitter (M13 smoke,
M17 fire fields). `PointLight` is a local light, ≤8 per scene. `Water`, `Tree`, `Cloud`, `Terrain`,
`Road` and `Meadow` are **recipes, not mesh references** — each owns its geometry and so carries no
`Mesh` and no `Material` (a `Tree` is the exception on materials: the entity's `Material` is its
bark). `Meadow` is ground cover on a seed→grass→weeds→straw→collapse life cycle (M29).
`HudPanel` lays its children out and `HudImage` is a nine-sliced textured rectangle; `HudInteract`
makes the element on its own entity clickable (M31).
Scene-level blocks: `physics`, `environment` (M16), `daylight` (M21).

## Editor (M7, `crates/engine-editor`)

egui is **git-pinned to a master commit** (released egui pairs with wgpu 29; swap to the 0.36
release when it ships). The scene file stays the single source of truth: the editor polls the file
(250 ms) and reloads on any external change; every editor action commits through
`engine-core/src/formatter.rs` — a *splice*, not a serialize, so a one-field edit is one hunk on one
line and untouched content is byte-identical by construction (`cargo test -p engine-core formatter`
pins this). Commits rebase onto a fresh read by entity `name` + component `type` (never index); a
vanished target drops the edit with a status-bar notice.

Inspector widgets are generated from the component schema (a new component is editable the day it
exists); only arrays-of-numbers route to the vec3 widget. The validation panel shows the same
`EngineError`s the CLI emits, click-to-select. Viewport = `SceneRenderer` into an offscreen texture
(same pipeline as `engine screenshot`), orbit camera (right-drag; shift/middle = pan, scroll =
zoom), CPU ray picking, hand-rolled transform gizmos — `W`/`R`/`S` switch translate/rotate/scale,
world axes map straight to `Transform` field components, preview in memory, one write on release.
The viewport shows scenes **at rest** (no particles until the fixed clock advances) and passes
`hud: None` — its orbit camera is not the game frame.

Structure edits: drag-and-drop import (`import.rs`) references a dropped `.glb`/`.gltf` in place or
copies it to `meshes/`; a `.blend` is converted to `.glb` by running Blender headlessly (`$BLENDER`
→ `PATH` → macOS app bundle; absent = `blender_not_found`) on a worker thread, then one entity is
spliced in via `formatter::apply_add_entity` (the Blender-gated test skips cleanly when Blender is
missing). A "+ add" menu splices `builtin:` primitives the same way, its entries generated from
`BuiltinMesh::ASSETS`. The inspector adds and removes components via `apply_add_component` /
`apply_remove_component` — absent fields *are* the documented defaults. `[0, 1]` RGB triples get a
linear-RGB color-picker swatch committing one write per picker session. Hidden flag
`--self-screenshot <png> [--self-screenshot-after-ms N]` renders the editor and exits — the agent's
way to *look at* the editor. `RenderItem` carries an `entity: String` for picking/selection.

## Diff-render (M6)

The pure comparison lives in `engine-render/src/diff.rs` (no GPU — unit-testable everywhere); the
CLI decodes the baseline, renders at the baseline's dimensions (no `--width`/`--height`; re-bless to
resize), and reports pass/fail with `diff_pixels`, `max_channel_delta`, and `diff_bounds`. Defaults
are bit-exact; determinism is promised same-machine/same-adapter only, so **baselines are
per-adapter artifacts** and the report carries the adapter name. The diff PNG's three pixel classes
(red violation / yellow within-threshold / faded-gray identical) are pinned formulas — see
`docs/cli-contract.md`. Blessing is `engine screenshot` — no separate bless flag, deliberately. The
report prints on both pass and fail (a documented stdout exception).

## Assets

`Mesh.asset` is `builtin:cube` / `builtin:cylinder` / `builtin:plane` / `builtin:sphere` /
`builtin:triangle`, or a `.gltf`/`.glb` path relative to the scene file. Reference checks (existence,
extension, absolute-path rejection) live in `engine-core/src/mesh.rs` (`MeshAsset::resolve`); actual
file parsing lives in `engine-assets` — the only crate that opens asset files (glTF meshes plus
PNG→RGBA8 textures, the latter awaiting texture-mapped materials). `engine validate` runs both
passes, so a corrupt glTF fails validation, not just the screenshot. `Scene::render_items` takes a
`MeshSource`: `AssetServer::for_scene` in the CLI, `BuiltinAssets` in GPU-less tests.
`examples/meshes/pyramid.gltf` is generated text glTF (embedded base64 buffer), flat-shaded,
CCW-wound.

## Lighting (M4)

`DirectionalLight` + `AmbientLight` components (at most one each per scene, validated),
`Material.emissive`, and a GGX Cook-Torrance shader in `engine-render/src/shaders/mesh.wgsl`. Lights
aim down their entity's local **−Z** like the camera; a scene with *zero* light components gets the
documented fallback rig (`LightRig::resolved`), while any light component means "absent is off".
Render targets are **sRGB** (`Rgba8UnormSrgb`): scene colors are linear reflectance, the hardware
encodes on write, and pixel tests compute expectations via the `srgb_encode` helper in
`engine-render/tests/lighting.rs` — never eyeball byte values. Line numbers on semantic errors come
from `engine-core/src/lineindex.rs` (serde_json discards spans).

## Validation & the CLI contract (M5)

The wire contract lives in `docs/cli-contract.md` — stdout is one JSON object on success and empty
on failure, stderr is NDJSON, exit codes split 1 ("your files are at fault") from 2 ("your
invocation/environment is"). Every error code is a const in `engine-core/src/codes.rs` with its exit
class; `docs/error-codes.md` mirrors it and a repo-contract test keeps them in lockstep — **codes
are API**, never rename one casually.

Per-component field checking is **schema-driven**: the walk in `validate.rs` reads the same
schemars-generated schema `engine list-components` publishes (unknown/missing fields, JSON types,
`minimum`/`exclusiveMinimum`-style ranges authored as `#[schemars(...)]` attributes), then serde
parses the clean component as a final gate — `scene_parse_desync` firing means the walk and the
parser drifted, and the corpus tests in `engine-core/tests/validation_corpus.rs` exist to catch that
before an agent does. The walk recurses into objects and arrays-of-objects (open-ended `minItems`
reports as `value_out_of_range`) and has a first-class `"integer"` arm (a float, negative, or
out-of-u32 value where a u32 belongs is `invalid_field_type`; below-minimum is `value_out_of_range`).

Errors carry `path` (a JSON Pointer for `jq`) next to `line`; warnings (`unused_material`,
`zero_scale`) ride the same stream with `"severity": "warning"` and exit 0 unless `--strict`.
Cross-field checks (`Camera.far > near`) and `duplicate_component` are semantic checks beyond the
schema. `Scene::from_source` errors with `Vec<EngineError>`, so screenshot/run-scene report
byte-identical diagnostics to `validate`. A panic hook keeps even a crash inside the NDJSON protocol
(`internal_panic`, exit 2), and clap failures are re-rendered as `invalid_invocation` JSON with
clap's own `did_you_mean`. The checked-in `schemas/component-schema.json` is enforced by
`repo_contracts.rs` — regenerate with `engine list-components > schemas/component-schema.json` after
touching any component, including its range attributes.

**schemars gotcha**: a doc comment on an enum **variant** turns the schema from a flat `"enum":
[...]` into oneOf/const, which blinds the validation walk's closed-vocabulary check — keep
`ColliderShapeKind` variants undocumented (a NOTE in components.rs guards this).

## Physics (M8, `crates/engine-physics`)

**rapier3d pinned =0.34.0** with `enhanced-determinism` — and note rapier 0.34 switched to a
glam-based math backend sharing our exact glam version, so no conversion layer exists; read the API
in the registry when touching it, and treat any golden-trace diff on a rapier upgrade as a breaking
change to review.

`RigidBody` (dynamic/kinematic/fixed) + `Collider` (flat struct, per-shape fields enforced
semantically) + optional scene-level `physics` block (`gravity`, integer `timestep_hz`).
`Transform.scale` scales collider shapes (the fixture's ground collider is authored in *local* units
for this reason); restitution combines by **max**. Angular velocity is degrees/sec (file
convention), converted at the rapier boundary. Determinism: same file + steps → byte-identical
traces, pinned by golden `verify/baselines/m8_drop.trace.jsonl`. **Bake round-trip is state-equal
within ~1e-4, deliberately not byte-equal**: baking quantizes to Euler-degree f32 text and drops
solver caches (disposable by design). `--steps 0` scene queries need `PhysicsWorld::refresh_queries()`
(the broad-phase BVH is otherwise only built inside `step`) — it is **documented destructive**, with
the `--steps 0` query path its only safe caller. The windowed viewer steps the same fixed dt through
a wall-clock accumulator; headless is canonical. Physics tests are GPU-free and unconditional.

**Collision (M12)**, all opt-in so pre-M12 traces and baselines are untouched:
- **Script contact queries** — `world.touching(name)` / `world.contacts_started(name)` return entity
  names from the touching-state the **previous** physics step left (scripts run before physics,
  hence the one-step latency). `ContactEvent`/`ContactState` live in engine-core so engine-script
  never depends on rapier.
- **Mesh colliders** — `shape` gains `trimesh` and `convex_hull`; geometry comes from
  `Collider.asset` or, absent that, the entity's own `Mesh.asset` (neither is
  `collider_missing_mesh`). Vertices scale by `Transform.scale`; a trimesh on a **dynamic** body is
  `trimesh_on_dynamic_body` (rapier trimeshes are hollow; use `convex_hull`). `PhysicsWorld::build`
  takes a `&dyn MeshSource`.
- **Collision layers** — `layers` (membership) and `collides_with` (filter), free-form names; absent
  means "everything" (which is why empty arrays are rejected — `empty_collision_layers`), two
  colliders interact only if the filter passes **both ways**, names map to rapier
  `InteractionGroups` bits sorted-name-deterministically (max 32, `too_many_collision_layers`), and
  a `collides_with` naming a layer nobody is a member of warns `unknown_collision_layer`.

## Animation (M9)

Property clips; skeletal glTF landed in M30 and shares this clock. Clips are JSON (`*.anim.json`, schema in
`schemas/animation-schema.json`, regenerated via `engine list-animations --schema`), animating
`Component.field` on entities by name. Pose is a pure function of (files, time) — `--time` on
screenshot/diff-render is reproducible down to `cmp`-identical PNGs, and t=loop-period equals t=0
byte-for-byte. **Rotation interpolates component-wise on Euler degrees** so a 0→360 clip actually
spins (quaternion slerp would no-op it) — load-bearing, don't "fix" it. Sampling lives in
`engine-core/src/animation.rs` (step/linear/cubic Catmull-Rom); `set_field` must cover every numeric
schema field — a drift test walks the schema and calls it. System order: animations → physics →
render. The M8×M9 ownership rule is settled: a clip animating the Transform of a **dynamic** body is
`animation_on_dynamic_body` (kinematic is the supported "animation drives, physics follows" case).
Clip-content errors carry the clip file's own file/line; `engine validate` accepts clip files
directly (structural checks only).

## Scripting (M10, `crates/engine-script`)

**Rhai pinned =1.25.1** — settled (see `designs/scripting-design.md` §1; Lua lost on the C dependency
and determinism friction, compiled-Rust-only lost on rebuild-per-iteration). Scripts define
`fn step(world, step)`; the curated `world` API is the entire universe — no time, no I/O, no
randomness, 1M-operation budget per call, so traces stay byte-identical with scripts running. Script
parse errors fail `engine validate` with the script's file/line; runtime errors are
`script_runtime_error`, exit 1, world intact. Bake is change-based: any `Transform`/`RigidBody` field
differing from the file's rest value is spliced — which is how script-driven kinematics land in baked
files. Kinematic-vs-fixed contact events are opted in via `ActiveCollisionTypes` (rapier skips them
by default). Bake next to the scene, not /tmp — relative paths.

## The mouse (M28, `designs/mouse-input-design.md`)

M11's §7 said "no mouse"; this reverses that one item and nothing else. **Buttons ride the same
`held` set the keys do** (`MouseLeft`/`MouseRight`/`MouseMiddle`, own allowlist so `world.key` and
`world.mouse` each reject the other kind *naming the call that would have worked*), and the cursor
is a `"cursor": [x, y]` **fraction of the frame**, origin top-left — not pixels, because a timeline
outlives the window it was recorded in. **An absent `cursor` is the centre of the frame**, so every
pre-M28 timeline parses unchanged; recorded cursors quantize to three decimals (`CURSOR_SCALE`,
written as a scale and not a step of 0.001, or the file says `0.41300002`).

- **The cursor is a point on the frame; the *ray* is the engine's job.** `input::Pointer::resolve`
  is computed by the **caller** of `ScriptHost::step` — the code that already knows which camera it
  is about to render through — so the script host holds no camera-selection policy and the viewer
  and the headless path provably agree. Scripts get `world.mouse`, `cursor_x`/`cursor_y`,
  `viewport_width`/`viewport_height`, and `cursor_ground(y)`; a scene with no camera makes
  `cursor_ground` a **runtime error** (M21's precedent for `time_of_day` without a `daylight`
  block), while a ray that never meets the plane degrades to `MAX_GROUND_DISTANCE` rather than NaN.
- **The ray is the inverse of `scene_renderer::view_projection`, written out longhand in
  engine-core**, which cannot depend on engine-render — so `engine-render/tests/pointer.rs` is the
  agreement test (project a cursor's ray back through the renderer's own matrix; it must land where
  it started, at the centre and all four corners, at several distances and two aspects).
- **A mouse-driven run is a function of the frame size**, which no earlier input was. `screenshot`
  passes its own size, `diff-render` the baseline's, and `simulate`/`raycast` — which render
  nothing — `Viewport::DEFAULT`, **960×540**. Same aspect ⇒ same ray, so `simulate` and a 16:9
  screenshot aim identically; a *pixel-sized* HUD hit test is another matter and the M28 CLI test
  documents exactly that (960×540 misses the arena fixture's 132×26 plate that 640×360 hits).
- **`set_hud_offset` / `hud_offset`** (either HUD component, offsets mean the same on both) is the
  one non-mouse addition: a HUD that could be resized and re-worded but not *moved* cannot draw a
  crosshair. It bakes change-based like every other script-driven field.
- The viewer maps `CursorMoved` against the window's inner size and drops buttons outside the three;
  **`CursorLeft` is deliberately unhandled** — a pointer that slid out of frame must not read as a
  click at the centre of the screen. The recorder compares **quantized** states, so a still hand
  records nothing, and its "an initial empty set is implicit" rule now compares against the whole
  default state, or the first mouse movement of a session (which happens before any button) is lost.

Fixture `verify/m28_pointer.json` + timeline, **two baselines from one file** (`--steps 40` and
`--steps 80`). Not here: scroll wheel, relative motion / pointer capture (which is what a
first-person mouselook needs, and it wants its own milestone), click edges (`world.state`, two
lines), and cursor visibility control.

## Input (M11, `designs/input-design.md`)

Keyboard input sampled per fixed step on the shared integer clock — scripts ask
`world.key("ArrowUp")` (unknown names are runtime errors with `did_you_mean`; key names are the
curated W3C-code allowlist in `engine_core::input::KNOWN_KEYS`). Live keys exist only in
`engine run-scene`; headlessly, input is an `*.input.jsonl` timeline (sparse keyframes of the
complete held set, in effect from their 0-based `step` until the next line; strictly increasing)
replayed via `--input` on simulate/screenshot/diff-render/raycast — same timeline, byte-identical
results, and no `--input` means no keys held, so all pre-M11 traces/baselines are untouched.
`run-scene --record-input` writes a timeline whenever the held set changes: record a play session
once, regression-test it forever. `world.look_at(name, x, y, z)` aims an entity's local −Z with a
level horizon (pitch+yaw through the XYZ Euler order would roll — that's why it exists); the viewer
re-resolves the camera transform every frame so scripts can drive a chase camera.

## Vehicle dynamics (M11.5) and wheels (M12)

Scripts read/write `RigidBody` velocities — `world.linear_velocity`/`set_linear_velocity` (m/s) and
the `angular_velocity` pair (deg/s) — and `PhysicsWorld::step` pushes a dynamic body's component
velocity into rapier **only when it differs from what physics last wrote back** (cache in
`written_velocities`), so the deg↔rad round-trip never touches untouched runs and the M8 golden trace
stays byte-identical. `RigidBody.locked_rotations: [bool; 3]` maps to rapier `LockedAxes`.
**`world.forward(name)` is required for heading math**: XYZ Euler clamps the middle angle to ±90°, so
physics-integrated yaws past that come back as the `(±180, θ, ±180)` twin and `rotation[1]` stops
being "the yaw" (this cost a debugging session; the twin is also why `animation.rs::field_shape` only
treats arrays *of numbers* as animatable).

The `Wheel` component is one raycast-suspension wheel — it sits on its own *visual* entity (Transform
+ cylinder Mesh, **no** RigidBody/Collider of its own, enforced by `wheel_with_physics`) and names
its chassis in `vehicle` (a different entity with a dynamic RigidBody + Collider;
`wheel_vehicle_not_found` / `wheel_vehicle_invalid`). engine-physics groups wheels by chassis name
(both levels name-sorted for determinism) into rapier `DynamicRayCastVehicleController`s.

- Conventions: up +Y, forward −Z via `index_forward_axis = 2` + axle +X (drive direction is
  `normal × axle = −Z`, so positive `engine_force` is forward); positive `steering` (degrees) steers
  **left**. Suspension stiffness is **per kg of chassis mass** (static sag ≈ `9.81/(4·stiffness)`).
  `Wheel.offset` is chassis-local meters, rotated but **not** scaled by `Transform.scale`.
- Control fields (`engine_force`/`brake`/`steering`) are runtime inputs like `RigidBody` velocities:
  scripts write them (`world.set_engine_force`/`set_brake`/`set_steering` + getters), physics reads
  them each step and wakes the chassis itself (rapier only wakes on *positive* engine force).
- Physics writes each wheel entity's Transform back every step — post-step chassis pose + ray length
  + steer yaw + accumulated spin, ×`Qz(90°)` mapping the builtin cylinder's Y axis onto the axle — so
  wheels visibly bounce, steer, and roll in screenshots. Vehicle worlds call `refresh_queries()` at
  build; vehicle-free worlds skip everything, keeping M8 golden.
- **Tire model caveats** (bullet port): lateral grip is a velocity damper — side impulse =
  `0.2 · side_friction_stiffness · lateral_vel · effective_mass` per wheel per step, so the sum of
  `0.2·side_friction_stiffness` over wheels is the fraction of lateral velocity removed per step (>1
  over-corrects and glues the car); `friction_slip` is the skid clamp as a multiple of suspension
  load — its 10.5 default never saturates and a large sideslip then wipes all momentum; ≈1.0 gives a
  physical μ≈0.9 tire that slides instead of sticking.

## The car demo and its generated circuit

`examples/scenes/car_track.json` — box chassis (`builtin:cube`, ≈1.5 t via density) + four cylinder
wheels; `scripts/car.rhai` is only the *driver* (pedals, speed-scaled steering wheel with finite slew
rate, low-gear torque boost below 8 m/s so full-lock corners don't stall against front-tire slip
drag, chase camera). `car_track_lap.input.jsonl` is a committed recording driving three clockwise
laps and parking just past the start line — pinned by CLI test and `verify/baselines/m11_lap.png`.
Both were re-authored in M23 when the plates became a `Road`.

**The circuit is generated** (M15, rebuilt on `Road` in M23): `examples/scenes/make_car_track.py`
emits the scene from a closed polygon of 14 named corners (Spa in miniature), ≈546 m round with
≈7.6 m of elevation and grades to 7.5%. Authoring the loop as a *polygon* is what makes closure free:
a closed polygon returns to its first vertex and its exterior angles sum to one turn, so position,
heading, and the height profile all shut without a solver — corners carry `(x, z, radius, height)`
and nothing carries a heading. Two things the polygon can't guarantee refuse to build: a corner
radius too big for the edges feeding it (the *engine* checks this — `road_corner_does_not_fit`) and a
grade too steep for the car to climb (the emitter's business). Three geometry lessons are baked in
and easy to reintroduce by "simplifying":

- **One collider, not two.** Road and shoulder as two colliders at different heights builds a ledge
  at the asphalt edge, and a wheel that drops off it wedges against the step and stops the car dead.
  This is now a property of the `Road` component, which cannot express the two-surface version.
- **The guardrail is continuous.** Posts are spaced 5 m and are 5.4 m long; dashed barriers let the
  car slip between two and fall off the elevated road. They are placed along the centerline the
  engine reports (`engine road-centerline`), not one the emitter re-derives.
- **Radii are sized for the car, not the map.** The layout is Spa at ~1/15 but the car is full size,
  so no corner is under 12 m however tight the real one is.

`make_car_track_lap.py` authors the input timeline the same way it is replayed: a closed loop that
replays the whole timeline from step 0 each round, reads the car's state back out of the `simulate`
report's `hud` (a scratch copy of the scene whose driver pushes one telemetry line — HUD is output,
never input, so it drives identically), and appends the next tenth of a second of keys. Steering is
pure pursuit; the throttle brakes on a `v² = v_corner² + 2ad` envelope, without which the car reads
corners correctly and arrives far too fast anyway. Regenerating the track means regenerating the
timeline and re-blessing the baseline — both scripts print the start-line constants `car.rhai` needs.

**The circuit stands in weather**: it now carries `{"sky": true, "fog_density": 0.0012,
"shadows": true, "shadow_distance": 70.0, "samples": 4}`, keeping the hand-tuned `Sun`;
`make_car_track.py` also scatters **58 `Tree`s** by dart-throwing (rejecting any candidate within
`TREE_CLEARANCE` of the *road's own* reported centerline, so the treeline re-fits itself when the
corners move) and rings the track with **six `Cloud`s**. Three things are load-bearing. **No tree
carries a `Collider`** — they are scenery the car reaches only through a guardrail, and a
colliderless forest is what keeps the drive, the timeline, and the lap test's pinned HUD strings
(`LAP 4`, `LAST 63.70   BEST 59.47`) the ones the bare circuit had. **The clouds ring the circuit
rather than sitting over it** because `TopCam` looks down from ~270 m, so a cloud over the infield
hides the infield. **Placement is a hand-rolled LCG in the script**, because the forest is committed
scene data and `random` reshuffling under a Python upgrade would surface as a baseline diff that
looks like a renderer bug. This is also a data point on M22's MSAA caveat — 58 trees at `samples: 4`
against a *flat* ground plane rendered byte-identically 6 runs running, so it is relief, not fine
geometry alone, that costs this adapter its determinism.

## HUD (M11.6 lines + M12 components, `designs/hud-design.md`)

Two layers, one render path. `world.hud(text)` pushes printable-ASCII debug lines, cleared every step
— the line HUD is a pure function of the step that drew it — and `world.state(key, default)` /
`world.set_state(key, value)` is numeric per-run memory on the ScriptHost (replay-deterministic,
reset by a fresh run, deliberately *not* baked — same disposability as solver caches). Caps 16 lines
× 96 chars, runtime error beyond.

**`HudText` / `HudRect` components** are screen-anchored (anchor enum + pixel offset measured inward;
five anchors), pixel-sized, schema-validated (size/color/opacity ranges, anchor typos get
`did_you_mean`), need no Transform and ignore the camera. Text snaps to integer scales of the 8×8
font (`size` 16 = 2×), colors are linear RGB, draw order is rects-then-texts in file order, and the
`world.hud` line panel draws topmost with its original layout formulas.

Rendering is `engine-render/src/hud.rs`: **one** CPU rasterizer (unit-tested without a GPU) producing
a target-sized sRGB straight-alpha canvas that `SceneRenderer` composites as a sampler-less
fullscreen-triangle blit (`ScenePass.hud`) — `offscreen::render` and the `run-scene` viewer share it,
so the played game and the pinned PNG show the same overlay; an empty HUD draws nothing, keeping
every pre-HUD baseline byte-identical. Scripts drive components via `world.hud_text`/`set_hud_text`
and `world.hud_rect_size`/`set_hud_rect_size`; changed `HudText.text` / `HudRect.size` bake under the
change-based rule (unlike `world.hud` lines, which are per-step output). The line HUD stays
observable without pixels: `simulate`/`screenshot` report the final step's lines as `"hud"`, and
`--trace` logs `{"step", "hud"}` on every change. Fixture: `verify/m12_hud.json`. `car.rhai` shows
the applied version — speedometer, lap timer (start-line crossing remembered step-to-step via
`world.state`), and a `SpeedBar` HudRect gauge.

## Particles (M13) and fire (M17)

The `ParticleEmitter` component is a seeded deterministic emitter — cone spray around the entity's
local **−Z** (the camera/light aiming convention: rising smoke is `"rotation": [90, 0, 0]`), spawn
rate via a credit accumulator, per-particle world-space acceleration/drag, and start→end
interpolation of half-size, linear-RGB color, and alpha over each particle's lifetime.

Simulation is GPU-free in `engine-core/src/particles.rs`: a private per-emitter xorshift32 RNG
**fully specified in-repo so dependency upgrades can't change sequences** (splitmix-finalizer
seeding, RNG *not* consumed on capped spawns), emitters stepped in name order — same file +
`--steps` → byte-identical pixels, which is what lets smoke live under a diff-render baseline
(`verify/m13_smoke.json`). Particle state is simulation state: created only by `--steps` (never
`--time`), never baked or traced, and a `--steps 0` render draws nothing. System order: animations →
scripts → physics → **particles** → render (an emitter riding a dynamic body trails where the body
actually went). Rendering is `shaders/particles.wgsl`: camera-facing instanced quads with a `(1−d)²`
soft-disc falloff, alpha-blended (depth-tested against meshes, depth-write off), CPU-sorted
back-to-front by camera distance with `total_cmp`.

`rate` is the one emitter parameter scripts drive — `world.particle_rate` / `set_particle_rate` — and
the setter rejects negative/NaN/f32-overflowing values **at the call** so a bad rate is a located
script error rather than a baked file that fails `validate`. It bakes change-based. Rate 0 pauses
emission without touching live particles, which is what makes gating cheap: `car.rhai` runs
`SkidLeft`/`SkidRight` at the rear contact patches off chassis sideslip (1 m/s deadband so
suspension jitter is not a skid) plus a braking-lockup term, and parks an `Exhaust` emitter at the
tailpipe each step (particles are world-space once spawned, so a moving car leaves a trail behind it
rather than dragging a plume along). All three follow the car's *height* — a contact patch pinned to
a fixed altitude smokes from inside the hill on a circuit that climbs.

**M17's five fire fields** (`designs/fire-and-lights-design.md`), each fixing one reason a particle
cone does not read as flame: `blend: "additive"` (overlapping flame *brightens*; alpha blending can
only render orange smoke), `radius` (a disc of coals instead of a single apex),
`speed_jitter`/`size_jitter`/`lifetime_jitter` (a population born identical dies at one height,
drawing a flat top), `turbulence`+`turbulence_scale`, and `stretch`. **Every default is the M13
behaviour, down to which random numbers the emitter draws**: the draw order is a format contract —
direction → disc → speed → size → lifetime → turbulence — and each step is *skipped*, not defaulted,
when its field is zero, since a defaulted draw would shift every subsequent one and move every
particle baseline (`defaulted_fire_fields_consume_no_randomness` pins it by construction).
Turbulence is smooth value noise sampled along each particle's own path plus a per-particle offset
drawn at birth — smooth because per-step randomness makes a particle *vibrate* rather than arc,
per-particle because otherwise every particle follows one shared braid; the integer hash is spelled
out in-repo like the xorshift. A particle's lifespan and size scale are fixed **at birth**. Additive
is a **second pipeline** over the same shader and instance buffer (the sorted list is
stable-partitioned on the CPU), *not* one premultiplied pipeline — that alternative moves the
multiply by alpha into the shader for every particle including the ones under existing baselines.
Additive sprites draw after *all* alpha ones regardless of depth, which is what firelight scattering
in smoke looks like. `stretch` is in **seconds** of travel and elongates along the velocity's
*screen-space* projection, so a particle flying at the camera stays round.

## Breaking (M14, `designs/breaking-design.md`)

`Breakable` lists **pre-authored fragments** (mesh ref + local placement + cuboid `half_extents` +
`density` — no runtime fracture, the settled decision) and breaks three ways: collision
(`impulse_threshold` in kg·m/s — rapier contact *force* × dt at the event boundary, **peak** per step
not sum, and force events are enabled only on breakable colliders so no-Breakable scenes are
byte-identical to pre-M14), `world.break_entity(name)` (validated at call time, queued on the
ScriptHost, drained by the sim loop), and `world.explode(x,y,z,radius,impulse)` (radial impulse,
linear falloff, applied inside `step()` before integration).

Breaks apply after physics in entity-name order (`engine-physics/src/breaking.rs`): despawn parent,
spawn `Parent.fragN` (suffix-deduped) as dynamic bodies inheriting v + ω×r, then
`Scene::refresh_names` + `ScriptHost::sync_names` — fragments are ordinary entities everywhere
downstream. Trace rows **re-enumerate dynamic bodies every step** (sorted, so unchanged scenes trace
identically) plus `{"step", "broke", "fragments"}` lines; bake extends change-based to structure via
`formatter::apply_remove_entity` + `apply_add_entity` with `ComponentData::collect_from` — a baked
post-break scene revalidates and re-renders **bit-exactly**. Fragment `mesh` refs resolve like
`Mesh.asset` in both passes; `impulse_threshold` without a `Collider` is `breakable_without_collider`.
A threshold-less `Breakable` is script/explosion-only by design.

## Frame cost (M15)

The viewer was slow for reasons that had **nothing to do with particles** — measured on an M3 Pro at
2560×1440, the smoke costs ~0 ms/frame even with the camera inside the plume, while the frame was
spending ~29 ms in `hud::rasterize` and ~4 ms rebuilding GPU resources. Three fixes, none of which
moves a pixel:

1. **The HUD rasterizes only what it covers** — elements are measured, overlapping ones grouped, and
   each group gets a canvas at its bounding box blitted under a scissor rect (`HudOverlay` /
   `HudCanvas { origin_x, origin_y, .. }`, `shaders/hud.wgsl` takes the origin as a uniform);
   overlapping elements still accumulate in one linear-space buffer and quantize once, so stacked
   translucency is untouched.
2. **GPU resources persist across frames** — `SceneRenderer::draw` takes `&mut self` and keeps
   uploaded geometry (keyed on the `Arc<MeshData>` identity, evicted after 240 idle frames), one
   object-uniform buffer addressed by dynamic offset instead of a buffer + bind group per entity, and
   grown-in-place frame/particle/HUD buffers.
3. **`MeshSource::load_mesh` returns `Arc<MeshData>`** and implementations must return the *same*
   `Arc` for one asset — that is both the end of the per-frame deep copy in `Scene::render_items` and
   the cache key in (2); a reloaded file mints a new `Arc` and re-uploads.

`particles.wgsl` also discards fragments whose final alpha is exactly 0, which is bit-identical
because `src·0 + dst·1` is `dst`. Net: ~34 ms → ~0.9 ms per frame in release, ~173 ms → ~2.2 ms in
debug. **The viewer draws an FPS readout** (`app.rs::with_fps_readout`, averaged over 0.5 s) — it
rides ordinary `HudText`/`HudRect` components appended to the scene's own HUD, and headless renders
never see it, so nothing reproducible depends on how fast this machine drew.

## Environment: sky, fog, shadows, MSAA, transparency (M16)

Five renderer features reached through **one scene-level `environment` block**
(`EnvironmentSettings`, hand-validated like `physics` by `check_environment_block`, code
`invalid_environment_value`) plus two new `Material` fields. **Every one of them defaults to off,
and that is the design**: eleven baselines were blessed before any of this existed and not one had
to be re-blessed. Fields: `sky` + `sky_zenith`/`sky_horizon`/`sky_ground`, `fog_density`, `shadows` +
`shadow_distance`, `samples` (1 or 4; anything else is a validation error rather than a silent
round). `sky_horizon` **is** the fog color — one field, so it cannot be set inconsistently with the
sky it fades into.

- **Shadows** are a single directional map (2048², `shadow.wgsl`, depth-only, no fragment stage,
  reusing the mesh pass's object+frame uniforms). The ortho box is fitted along the camera's view
  direction, and its center is **snapped to whole texels** — without that, moving the camera slides
  the sampling grid across the world and every shadow edge crawls, which reads as a bug rather than
  as low resolution. Casters are drawn **front-face-culled** so the map records each caster's far
  side, a better peeling margin than any constant bias. 3×3 PCF over a `LessEqual` comparison sampler
  with linear filtering, slope-scaled bias, and a fade to lit at the box edge. Transparent geometry
  does not cast. One cascade only.
- **Sky** is a fullscreen triangle drawn first with `depth_compare: Always` and depth writes off,
  evaluated per pixel from an unprojected view ray (per-vertex would visibly bend the horizon). The
  gradient lives in `shaders/sky_common.wgsl` and is **concatenated onto both `sky.wgsl` and
  `mesh.wgsl`** at pipeline build (`with_sky_common`) — WGSL has no `#include`, and the mesh pass
  reflects this exact sky off metal and water, so a second copy of the curve would drift.
- **Reflected sky and hemispheric ambient**, both gated on `sky`. Ambient is modulated by a
  ground↔zenith lerp normalized **per channel** against the two bands' mean, so `AmbientLight` keeps
  meaning what it says and only the color *balance* tracks the normal; normalizing against mean
  *luminance* instead is the obvious alternative and is wrong (a saturated sky then triples the blue
  channel and every up-facing surface goes blue-grey). The specular environment term uses
  **roughness-capped Schlick** (`max(1 - roughness, f0)`, not 1) — uncapped, grazing Fresnel turns
  matte ground into a sheet of sky.
- **MSAA** is `samples` on the scene pipelines plus a resolve; the HUD pass stays single-sampled on
  the resolved target, so glyphs are still pixel-exact. `SceneRenderer::with_samples` bakes the count
  into the pipelines, so it belongs to the renderer, not the frame.
- **Transparency** is `Material.alpha` (flat, view-independent — the "ghost this" knob) and
  `Material.transmission` (view-dependent, keeps the specular lobe, scales diffuse by
  `1 - transmission`). `Material::is_transparent` routes those into a second blended pass, sorted
  back-to-front with an entity-name tiebreak, depth-tested but not depth-writing, and the shader
  emits **premultiplied** color for them so a clear surface keeps its highlight and its sky
  reflection. No refraction and no scene-color sampling.

**The bit-exactness of the default path is load-bearing and fragile.** The four lines computing
`direct`/`ambient`/`base_color` in `mesh.wgsl` are the M4 originals, computed from immutable bindings
ahead of every M16 branch, and every new feature hangs off one combined `if`. That is stricter than
"an equivalent expression" on purpose: whether the compiler may contract `a*b + c` into an FMA
depends on the code around it, and an FMA carries more intermediate precision than the pair it
replaces. Restructuring those lines into arithmetic that is *equal on paper* moved `m12_hud.png` by
one ULP in one pixel. Leave them alone. Verified by `engine-render/tests/environment.rs` and fixture
`verify/m16_environment.json`.

**The check that settles a bit-exactness question is an A/B between binaries**, not a diff against a
baseline: build the CLI at `main` and in the worktree, render the same scenes with both, `cmp` the
PNGs.

## Point lights (M17)

`PointLight` is a local light — position only, no orientation, many per scene up to
`MAX_POINT_LIGHTS` (8, beyond which `too_many_point_lights` rather than a light that silently never
shines). Inverse-square falloff windowed by `(1 − (d/r)⁴)²`: the window is what makes a light
*local*, and past `range` the contribution is byte-identical to no light at all — without a hard
horizon a lantern in one room lifts the black level of the next. `intensity` is brightness at one
unit of distance. No shadows (the engine has one shadow map and it belongs to the sun). Lights are
ordered by entity name, since the uniform array is fixed-size and an index must not depend on
archetype iteration; a `PointLight` counts as lighting the scene. Contributions are **added to the
finished color** on their own branch after every M16 feature — firelight is *extra* light, and
`a_point_light_is_extra_light_not_replacement_light` walks every pixel of a sunlit scene to prove
adding a lamp never darkens one. Scripts reach any light by name through `world.light_intensity` /
`set_light_intensity` / `light_color` / `set_light_color` (all three light components — the fields
mean the same thing on each); intensity errors on negative/NaN/overflow at the call, color *clamps*
to `[0, 1]`, and both bake change-based.

**Two places here are deliberately more repetitive than they look**: `evaluate_point_light` in
`mesh.wgsl` re-derives the GGX terms instead of sharing a function with the sun path, and
`particles.wgsl` writes the un-stretched quad expansion out twice rather than lerping. Both guard the
M16 ULP sensitivity — factoring them would rewrite the four untouchable lines.

## Water (M18, `designs/water-design.md`)

**A body of water is one entity with one component.** `Water` owns its surface geometry — a
tessellated unit grid (`segments`, 1..512, identical to `builtin:plane` at `segments: 1`, generated
and `Arc`-cached in `engine-core/src/water.rs`) sized by `Transform.scale` — so the entity carries
**no** `Mesh` and **no** `Material` (`water_with_mesh`). Waves are evaluated in **world space**, so
scaling never stretches them and two water entities at the same height form one continuous surface.
`Scene::water_items` returns name-sorted `WaterItem`s and needs no `MeshSource`.

- **Gerstner waves, displaced in the vertex stage** (`shaders/water.wgsl`), with normals from the
  analytic derivatives of the same sum. CPU displacement was never close: a 192² grid is 37k vertices
  and would mint a new `Arc<MeshData>` every frame, defeating M15's geometry cache. `Q` is packed as
  `steepness / (k · A)`, which makes each wave's contribution to the horizontal Jacobian equal to its
  own `steepness` — so **sum of steepness ≤ 1 is exactly the non-folding condition**, enforced as
  `water_waves_self_intersect` with the arithmetic in the message. Dividing `Q` by the wave count (as
  most references do) would make the same file calmer as waves were added.
- **Detail is a slope field with no height behind it**: four golden-angle-rotated sine trains at
  deep-water dispersion speeds, perturbing the normal only. Two numbers in it are load-bearing — the
  base amplitude (`0.010 · wavelength`; the first attempt was ~4× steeper and rendered white noise,
  since the layers are in phase *somewhere* and a slope field is a shaken mirror) and the **fade with
  view distance**, without which sub-pixel ripples alias into sparkle that reads as broken. Nothing
  physical may depend on these normals.
- **The frame gains a pass, but only when there is water.** Absorption and shore foam need the depth
  behind the surface and a pass cannot sample its own depth attachment, so a water scene renders as
  opaque (depth stored) → depth copy (`shaders/depth_resolve.wgsl`, one fullscreen triangle into a
  single-sampled `R32Float`, `textureLoad`, sample 0 under MSAA) → water and transparency →
  particles. `water_present` gates the split: with no water the pass structure, attachments, and
  load/store ops are the exact pre-M18 ones. Water sorts into the **same** back-to-front `Blended`
  list as transparent meshes, because an ice floe in a pond is transparent geometry *inside* a water
  surface and two passes would fix which always draws over the other.
- **The clock.** Water is a pure function of (file, `time`): `--time T` when given, otherwise
  `steps / timestep_hz` (`scene_time` in the CLI); the viewer uses whole fixed steps since load.
  That is what lets water sit under a `diff-render` baseline.
- **`mesh.wgsl` is untouched.** `water.wgsl` duplicates `FrameUniform` and the shadow lookup rather
  than sharing them (the `sky.wgsl` precedent, the M16 reason); only `sky_common.wgsl` is shared. The
  body is lit with the **up** normal while the view-facing normal drives reflection, Fresnel, and
  specular — conflating them made water black from below.

Not here, deliberately: scene reflections (sky and sun only), a CPU wave evaluator and therefore no
buoyancy (`water.rs` is where the Rust mirror goes, with an agreement test, when a boat needs to
float), and point lights on water. Refraction landed in M27 (below).
Fixture `verify/m18_water.json` at `--steps 120`.

## Water refraction (M27, `designs/water-refraction-design.md`)

`Water` gains **one field, `ior`**, defaulting to `1.0` (no bending) — so every committed baseline
survived the milestone untouched except the six the showcase tour's own edit re-blessed, and the
sweep confirmed the other 27 bit-exact. `Water::refracts()` is `ior != 1.0`, and it joins
`Material::refracts()` in the disjunction that allocates M26's opaque colour copy and splits the
pass, so a scene with neither still renders the pre-M26 pass structure exactly.

- **Three things `Material` needs that water does not.** No `thickness`: `water_thickness()` has
  measured the view ray's path through the body since M18, so the bend scales with the water's own
  depth. No `attenuation`: water already grades `shallow_color`→`deep_color` off that same
  thickness, and the bed that reaches the camera is `1 - out_alpha`, the number the blend unit was
  already using. **Refraction moves where the bed is read from, not how much of it comes back** —
  which means turning `ior` on cannot change how deep the water looks, and it can go into a tuned
  scene without re-tuning it. And no `FrameUniform` change: the exit point projects with
  `surface.view_proj` out of `WaterUniform`, which water carries because waves displace in world
  space.
- **The exit point is solved to the bed's depth, not stepped along the refracted ray by the view
  ray's path length.** This is the milestone's one real trap. `refraction.wgsl` steps, correctly,
  because a mesh's `thickness` is an authored fudge; water measures a real quantity along a
  *different* ray, and the refracted ray is always steeper for `ior > 1`. Measured on the fixture —
  1.5 m pool at 66° from the normal — stepping overshoots the bed by 1.18 m and displaces the
  sample 2.53 m instead of 1.42 m, which renders as the bed **diced into rectangular blocks**, not
  as a bent pool bottom. The travel is capped at `thickness`, which is the `ior >= 1` bound as
  arithmetic and makes the expression continuous at 1.0.
- **The sample is validated against the depth copy** and falls back to the unrefracted one when it
  lands in front of the water. The mesh path skips this (its ice is a block in mid-air); water
  cannot, because a pond is bounded by a shoreline and by things standing in it. It costs one
  `textureLoad` from a copy water already has bound. **It was measured before it was believed**: on
  the fixture's overhead camera it changes *zero* pixels and was nearly deleted as dead code; at a
  grazing 8° it changes ~22k by up to 99, smearing the boulder's silhouette across the water. Hence
  the fixture's second camera.
- **`water.wgsl` is not edited, including its comments.** The plain pipeline compiles it as it sits
  on disk and a second `refractive-water-pipeline` compiles a variant assembled by
  `with_water_refraction` (M22/M26's splice, four anchors, each asserted to appear exactly once).
  The pipeline is chosen **per surface**, so an unrefracting pond beside a refracting one still
  gets the M18 shader. The IOR rides in `clock.z`, a slot M18 declared padding, which is what keeps
  one uniform layout feeding both pipelines.
- **Authoring: refraction is only visible in water you can see through, and needs a pattern under
  it.** A displacement of a uniform field is invisible by construction, and the displacement runs
  *along* the view direction — so a bed pattern parallel to that axis barely moves (the first
  render test split the bed left/right and saw 236 pixels change; bars laid across it see
  thousands). The tour's pond is silty over a 0.2 m bed and `ior` still moves ~30k pixels of
  `showcase_450`, because a grazing camera makes the path long even in a puddle.

Fixture `verify/m27_water_refraction.json` at `--steps 120`, **two baselines from one file** via a
second camera (`--camera CameraGrazing`) — the overhead one pins the bend, the grazing one pins the
clean waterline. Both are hard bit-exact pins with no tolerance, which M22's rule allows because the
fixture aims at its subject with no terrain in frame; four consecutive sweeps came back at zero.
Not here: refracting another transparent surface (the copy is the *opaque* frame, so the ice
floating in a pond is not in what the pond bends), chromatic dispersion, and planar reflections —
still the other half of a water surface, and still missing.

## Trees (M19, `designs/tree-design.md`)

The `Tree` component is a **recipe, not a mesh reference**: `engine-core/src/tree.rs` grows it into
two meshes — bark (drawn with the entity's own `Material`) and leaves (drawn with
`Tree::leaf_material`, from `leaf_color`/`leaf_roughness`) — so one entity emits two `RenderItem`s
under one name, and `unused_material` knows a tree's Material is its bark. A branch is a polyline
that wanders: each of `segments` steps adds a random `crook` and a tube of `sides` faces is swept
along it, tapering on a **power curve** (`t^1.6`, since linear draws a carrot), with a quadratic root
flare over the bottom fifth of the trunk. Children attach past `branch_start`, spun by `branch_twist`
per point (137.5°, the golden angle — a whole-number division stacks branches into visible rows) and
started 70% inside the parent's radius so the tubes interpenetrate instead of needing a union. Ring
orientation is carried by **parallel transport**; rebuilding a perpendicular from a world axis spins
the tube wherever a branch aligns with it. Leaves are `blade` (a midrib with two wings folded down,
emitted twice for both faces — the fold is what gives a canopy texture when the engine has no leaf
textures), `cluster` (a stretched octahedron, for conifer sprays), or `none`.

**Three model rules came out of looking at renders, and all three are now multi-seed tests**:
`whorl` applies to the **trunk only** (compounding it is quadratic — a plausible spruce hit 175,898
vertices — and botanically wrong); `tropism` applies at **depth > 0 only** and clamps against
overshoot (at depth 0 it is unstable: a degree of crook gives a negative tropism something to
amplify, and the first pine grew sideways); and the trunk gives back 30% of its accumulated lean
every segment, because **a random walk with nothing pulling on it drifts**.

Determinism is the M13 discipline again — one private xorshift written out in-repo — except that
jitter helpers always consume a draw even at `jitter: 0`, since no tree baseline predates any tree
field. `tree::vertex_count` is exact (not an estimate) and validation refuses anything over
`MAX_TREE_VERTICES` (100k) with `tree_too_complex` before allocating, because a hung render with no
output is the worst failure an agent loop can hit. `meshes_for` caches on the component's **exact
field bits** (26 words, compared not hashed) and must return the same `Arc` — M15's upload cache keys
on `Arc` identity. There is no species enum: a species is a set of parameters, tabulated in the
design doc.

**Tree baselines are per build profile as well as per adapter** — a release build's `sin_cos`
routines move 3 pixels of `m19_trees.png` by one channel step (measured; Rust does not contract
floats, so this is libm, not FMA), so bless from the debug binary `cargo test` runs. Every pre-tree
fixture is profile-insensitive; the constraint arrives with CPU-generated geometry.

## Clouds (M20, `designs/cloud-design.md`)

M19's premise applied to the sky. `engine-core/src/cloud.rs` grows one mesh — a golden-angle spiral
of icosphere lobes over the footprint, each growing `children` smaller lobes biased upward by `rise`,
buried 45% of their own radius so the surfaces interpenetrate (M19's join, for M19's reason),
radially displaced by `wobble`, and folded onto a base plane by `flatten`. The entity carries **no
`Mesh` and no `Material`** (`cloud_with_mesh`) and is sized by `Transform.scale`; non-uniform scale
is the normal case, which is what oblates the lobes. Determinism, the exact `vertex_count` +
`cloud_too_complex` budget (100k), and `Arc`-identity caching are M19's — except the cache key covers
the **eleven geometry fields only**, since colour and density are uniforms that cannot move a vertex.
**Cloud baselines are per build profile as well as per adapter**; bless from the debug binary.

Rendering is `shaders/clouds.wgsl`, a new pipeline (not a `Material` branch) duplicating
`FrameUniform` and the fog term rather than touching `mesh.wgsl`, with `sky_common.wgsl` prepended so
a cloud's underside is lit by the sky drawn behind it. Clouds join the existing back-to-front
`Blended` list beside water and transparent meshes, depth-tested but **not** depth-writing, so
overlapping lobes accumulate alpha as a stand-in for optical depth; culling is **off** for this
pipeline alone, because a cloud has no inside and would vanish the moment a camera entered one.
`drift` (m/s) is applied in the **vertex stage** from `ScenePass.time` — not folded into the model
matrix — which keeps `Scene::cloud_items` a pure function of the file and the grown mesh's `Arc`
stable across frames; the shape never evolves with time. No shadows cast, no point lights, no
volumetrics.

**Four things the renders changed, all easy to reintroduce by "simplifying"**: vertex normals are
bent **55% from each lobe's centre toward the cloud's** (`BODY_NORMAL`), without which every lobe
draws its own terminator and the cluster reads as a bag of marbles; the height profile's rise is
capped at 0.8 lobe *diameters* (`DOME_STACK`), without which the middle lobe floats clear of the ring
around it — the consequence being that how far a cloud fills a tall box is set by `lobe_size`, not by
stretching a fixed lobe count; alpha is `density · (1 - (1 - facing)^feather)` and **not**
`facing^feather`, since the proportional form turns a cloud seen from below translucent all over
(this inverted `feather`'s sense: higher is now *crisper*); and the sun reaches the shadowed side at
a `THROUGH_SCATTER` fraction (0.3) with the diffuse curve left **linear**, because applying it in
full saturates a white cloud everywhere and sharpening the curve instead turns a storm cloud into
grey rock.

## Day and night (M21, `designs/daylight-design.md`)

**It is a pure CPU function, and that is the whole design.** `engine-core/src/daylight.rs` maps
`(DaylightSettings, time) -> Daylight`, and `scene::apply_daylight` folds that onto the
`ResolvedLights` and `EnvironmentSettings` the renderer was going to receive anyway. **No WGSL
changed, no new uniform, no new pass — `SceneRenderer::draw` takes exactly the types it took
before.** So M16's untouchable four lines cannot be tripped, the whole system is GPU-free and
unconditionally testable, and everything downstream tracks for free: shadows follow the sun because
they always followed the `DirectionalLight`, fog recolors at sunset because fog *is* `sky_horizon`,
and water reflects a dusk sky because `water.wgsl` already reflects whatever the sky uniform says.

`daylight` is a **top-level sibling of `physics` and `environment`**, not a field inside it — it is
clock-driven and it *produces* environment values, and a `Vec` palette inside `EnvironmentSettings`
would cost that type its `Copy` and put a clone in the per-frame path. It rides the same clock water
does, and **`day_length: 0` (the default) freezes the day**: most scenes want a dial, not motion, and
a frozen day is reproducible with no `--time` at all. `day_length: 24.0` makes an hour a second.

- **The arc** is artistic with a physical shape: `sun_elevation` (noon altitude) and `sun_azimuth`
  (noon bearing) replace latitude, date, and axial tilt. **Sunrise is 06:00 and sunset 18:00 at every
  elevation** — refusing to move them with the season is what makes an 18:00 keyframe *the* sunset
  keyframe in every scene. A noon sun toward −Z makes −Z south, so the sun **rises toward −X and sets
  toward +X**.
- **The moon** rides the same arc twelve hours out of phase with its own elevation, color, and
  intensity. There is still one directional light: **it *is* the dominant body**, with no summing.
  The bodies swap where their luminances are equal, so brightness is continuous by construction and
  only hue and direction shift. Summing instead would send an orange sunset from the moon's side of
  the sky for all of twilight; crossfading the direction would aim the light where neither body is.
  The handoff's invisibility is a **property of the palette**, and a test walks the day at one-minute
  resolution asserting exactly two swaps, each under 0.08 luminance.
- **The palette** is eight keyframes, all nine fields required (a half-specified keyframe fading to
  black is worse than an error), interpolated linearly in linear RGB and **wrapping across
  midnight**. **The noon keyframe is exactly the M16 clear-day defaults**, so the model and every
  hand-authored scene agree at the one hour anyone can check. Sun intensity lives in the table rather
  than falling out of `sin(altitude)` because a sunset's redness and its dimness are one decision.
  **Fog is a `fog_scale` multiplier on the authored `fog_density`**, never an absolute — a scene with
  `fog_density: 0` stays clear all day.
- **Ownership.** `drives_sun` (default on) synthesizes the sun, and an authored `DirectionalLight`
  beside it is `daylight_and_directional_light`. `drives_sky` (default on) computes the three bands
  **and the ambient** (ambient *is* the sky's contribution, which is why M16 gates hemispheric ambient
  on `sky`); authoring either anyway is the `daylight_overrides_sky` warning, naming the fix.
- **The horizon-sun shadow bug**, which day/night is the first thing to reach: a sun on the horizon
  casts shadows of unbounded length and one just below it casts them *upward*.
  `clamp_shadow_elevation` in `scene_renderer.rs` floors the direction used for the **shadow matrix**
  at 5° while the lighting direction keeps going. Above 5° it returns its input unchanged, which is
  why it costs every pre-M21 baseline nothing.
- **Scripts** get exactly two read-only getters, `world.time_of_day()` and `world.sun_altitude()`,
  evaluated once per step from `step * dt`. There is deliberately **no setter**: a script-settable
  clock is hidden state (invariant 2). Asking a scene with no `daylight` block for the time is a
  runtime error, not a plausible noon.

Fixture `verify/m21_daylight.json` + **five baselines from one file** at `--steps
120/390/720/1110/1320` (02:00, 06:30, noon, 18:30, 22:00) — `--steps` and not `--time` because the
lamp is script-driven. Bless from the **debug** binary (the fixture has trees). Not here: a sun disc
or a directional horizon glow (the natural next commit, in `sky_common.wgsl` on its own branch after
the untouchable lines), stars, clouds, real astronomy, moon shadows, and script-driven
`Material.emissive`.

## Terrain (M22, `designs/terrain-design.md`)

**There is no flat ground in the repo's scenes any more.** Following `Water` exactly, `Terrain` owns
a tessellated unit grid (`segments`, 1..512) sized by `Transform.scale`, so the entity carries **no**
`Mesh` and **no** `Material` (`terrain_with_mesh`). Heights are sampled in **world** XZ, so two
patches sharing a description meet seamlessly; `Transform.scale.y` multiplies the relief.

- **The height field is CPU-side — the opposite of water's choice, for three reasons.** Terrain does
  not animate, so the argument that forced waves onto the GPU does not apply; physics has to stand on
  it (a `trimesh` `Collider` with no `asset` and no `Mesh` borrows the generated surface, which is how
  ground is collidable without a mesh file); and placement has to query it. So there is exactly one
  implementation and **nothing to keep in agreement**. fBm value noise with the integer hash spelled
  out in-repo (a terrain render sits under a baseline, so the hash is a format contract), `warp`
  domain-warping it into ridges and valleys, and the octave sum normalised so `height` means metres
  however many octaves are summed.
- **Mesh normals are written in the patch's local space**, gradient scaled by the patch's own size.
  The renderer transforms normals by the model's inverse-transpose — `diag(1/180, 1, 1/180)` on a
  180 m patch — so a world-space normal arrives crushed to straight up and the landscape lights
  exactly like a plane *and* reports 0° everywhere, silently disabling every slope-selected layer.
  Pinned by `mesh_normals_survive_the_model_transform`.
- **The generative texture system** is `layers` (at most 4), each claiming a band of world height and
  a band of slope. The first is the base coat and each later one **paints over** what is beneath it
  (averaging would leave a rock layer half grass on a cliff it fully claims). Slope does the heavy
  lifting — height alone draws a contour map. Fades are **absolute** (`height_blend` in metres,
  `slope_blend` in degrees) and spent *outside* the band: a scale-free fraction-of-the-band `blend`
  was tried first and bleeds a 13 m fade out of a layer aimed at "above 1.9 m". `noise` jitters each
  boundary out of an iso-line into interlocking fingers, `color_variation` mottles at two scales an
  order of magnitude apart, and `bump` perturbs the normal per pixel with no displacement behind it,
  fading with view distance (water's specular-aliasing lesson). Nothing physical depends on any of
  the texture noise — the collider is the displaced grid and nothing else.
- **`mesh.wgsl` is not edited.** Terrain is lit exactly like a mesh, so it shares the file rather than
  duplicating 200 lines that would drift; but M16's four untouchable lines must reach the compiler
  surrounded by the code they shipped in. Putting the branch inline — those lines textually
  identical, only `albedo`/`roughness` arriving from a function result — **moved one pixel by one
  unit in each of `m16_environment`, `m17_fire` and `m18_water`**, found by the A/B between binaries.
  So the plain pipeline compiles the file as it sits on disk (byte-identical by construction) and a
  second `terrain-pipeline` compiles a variant assembled by `with_terrain`: `shaders/terrain.wgsl`
  inserted plus two **anchored** substitutions that panic at startup if `mesh.wgsl` is reworded.
- **`world.terrain_height(name, x, z)`** is the only terrain call in the script API — read-only,
  returning a world Y a script can assign directly. **Terrain's shape fields are deliberately not
  animatable** (`NOT_ANIMATABLE` in `animation.rs`): a clip driving `height` would regenerate the
  surface every frame and leave hundreds of megabytes of vertex buffers in the renderer's cache, so a
  clip aimed there fails validation with `unknown_property`. Appearance fields animate freely.

**Terrain is the first thing in this engine whose render is not bit-reproducible, and the reason is
not in the engine.** A 200k-triangle ground patch as the *last* draw of an MSAA render pass renders
differently run to run on Metal: one unchanged `showcase_tour.json` rendered 20 times came back as
two or three distinct PNGs, ~24 pixels apart, max channel delta 6, wherever the patch met other
geometry inside a pixel. Everything the CPU hands the GPU is identical run to run, and a *baked*
scene at `--steps 0` flakes too — what varies is which surface wins MSAA samples 1–3. At
`samples: 1`, with `shadows: false`, or with `height: 0` it is stable. **Any draw after the terrain
removes it**, so ground draws **first**, which is also right on its own terms: everything stands *on*
the terrain, so a contact surface exactly coplanar with it should tie in favour of the object, which
is what drawing the object second under `Less` gives.

Residue, recorded rather than hidden: `showcase_646.png`, the blast frame, still comes back as two
images ~100 pixels apart (delta ≤ 18) in the distant tree canopy. It is the only baseline with a
`diff_args` tolerance in `baselines.json` (`--threshold 24`). `showcase_810.png` has since been
seen to flake the same way **once** — 29 pixels at a channel delta of 1, along the treeline, clean on
the next three runs — so it is the same residue and not a second bug; it is left without a tolerance
deliberately, since one flake in four sweeps is worth re-running rather than blessing away.
M27 saw the same signature on **`showcase_450` (22 px) and `showcase_585` (24 px), both at delta 1**,
in one of seven consecutive full sweeps, the other six clean. Same residue, same verdict, no
tolerance: **the whole tour is in this class, not three named frames**, so a failure on any
`showcase_*` at a delta of 1 and a couple of dozen pixels is worth a second sweep before it is worth
debugging. Everything else in the manifest is bit-exact and a failure there is real — the M27
fixtures, which aim at their subject with no terrain in frame, never flaked across ten sweeps.
**M30 measured the rate instead of counting sweeps**, which is the cheaper way to settle one of
these: ten `--steps 585` renders of the *unchanged* tour came back as **three distinct images from
the M30 binary and two from `main`'s** — nondeterministic on both sides, so the sweep is a lossy
instrument and re-running it is a coin flip. When a sweep fails, `md5` N renders of the one frame:
a stable-but-different render is a real change, a different-every-time render is the adapter. **The general rule: fine geometry
against relief under MSAA is where this adapter stops being reproducible, so a new fixture wanting a
hard pin should aim its camera at its subject rather than across a landscape.** Verified by
`engine-render/tests/terrain.rs` (including `a_flat_single_layer_patch_is_exactly_a_painted_plane`,
which pins the shading path against `builtin:plane` at `segments: 1`) and `verify/m22_terrain.json`.

## Roads (M23, `designs/road-design.md`)

The car demo's circuit was **207 `builtin:cube` plates** whose overlapping slabs and constants existed
only to hide the fact that the road was not a surface. `Road` replaces all of it with one entity, and
the entity carries **no** `Mesh` and **no** `Material` (`road_with_mesh`).

The centerline is authored as a **polygon with corner radii** (`points`, each a `position` and a
`radius`), not a spline: a closed polygon returns to its own first vertex and its exterior angles sum
to one turn, so position *and* heading close without solving anything, and nothing in the file
carries a heading. `radius: 0` is a sharp vertex, mitred with the standard `1 / cos(turn/2)`
widening; past `MAX_SHARP_TURN_DEGREES` the mitre folds and validation says so
(`road_corner_needs_radius`), as it does when two arcs need more of the edge between them than it has
(`road_corner_does_not_fit`). Elevation rides on the points and is interpolated by arc length with a
**monotone cubic** (Fritsch–Carlson), not linearly and not Catmull-Rom: linear ramps break the grade
at every corner — a bump the car feels exactly where it is loaded up — and plain Catmull-Rom
overshoots, so a road authored to reach 6 m crests at 6.4 and the file stops predicting the scene.

- **One collider, and it is the whole ribbon.** Asphalt, shoulders and the embankment skirt are the
  same triangles, so the ledge that stopped the car dead on the plate road is now structurally
  impossible. A `Collider` with `"shape": "trimesh"` on a road entity needs no `asset` and no `Mesh`
  — the road *is* the geometry — while friction and layers stay on the `Collider`.
- **`FIX_INTERNAL_EDGES` on a road's own trimesh, and only there.** Without it a body resting on a
  triangle mesh eventually contacts an edge *between* two coplanar triangles, takes a contact normal
  along that edge instead of off the surface, and is flung sideways: a ball parked on the M23 fixture
  sat still for two seconds and then left the road at 4.8 m/s. Switching it on for *every* trimesh
  moves `verify/baselines/m22_terrain.png` by 1339 pixels. **Terrain has the same latent bug** and
  should take the same flag as its own change, with its own re-blessed baseline.
- **Markings are drawn, not built.** Every marking is computed per pixel in `shaders/road.wgsl` from
  two surface coordinates the vertex stage carries in the mesh's UVs (which the renderer had never
  uploaded before and now does for every mesh): `u`, signed metres from the centerline *along the
  cross-section*, so `|u| > width/2 + shoulder` is exactly "on the skirt"; and `v`, metres along the
  centerline. A line is a band in `u`, so it follows every curve and grade for free; a dash is
  periodic in `v`, so it is the same length in metres through a hairpin as on a straight. Paint
  cannot z-fight, because it is the same pixel shaded differently. Anti-aliasing is a **clamped**
  `fwidth`; unclamped, a road seen at a grazing angle from 200 m dissolves into grey.
- **Two things the CPU decides**, because per-pixel code cannot: **kerb spans** (which corners are
  under `markings.kerb_max_radius`, and which side is the *inside*) ride in a fixed-size uniform
  array, `MAX_ROAD_KERBS` of them, beyond which `too_many_road_kerbs`; and **period fitting** — on a
  closed road the dash period is snapped to `total / round(total / period)` and each kerb's stripe to
  its span, so patterns close on themselves. Kerbs are *painted*, not raised: a real kerb is a step,
  and a step is the thing the whole design says must not exist on the drivable surface.
- **`markings.start_line_at`** places the start line by arc length rather than at `v = 0`. The obvious
  alternative — split the straight with a radius-0 point — fails on the demo circuit: La Source is a
  110° turn on a 14 m radius, its arc reaches 20 m back down a 34 m straight, and a sharp vertex 19 m
  along would sit inside it. The road refuses that, correctly.
- **`road.wgsl` duplicates `mesh.wgsl`'s lighting**, following the `water.wgsl` precedent and for the
  same reason; only `sky_common.wgsl` is prepended.
- **`engine road-centerline`** publishes the samples the ribbon was built from — world position,
  heading and `v` per point. Anything placed *along* a road needs them, and a generator re-deriving
  them is how two implementations of one curve start disagreeing about where the road is.
  `make_car_track.py` is the worked example: write the road, ask the engine where it went, write the
  scene again with the guardrail and the car on it.
- **Roads draw last in the opaque pass**, after the terrain run M22 moved to the front. That puts a
  road where M22 measured this adapter to be unreliable, so it was checked rather than assumed: five
  consecutive sweeps of the six tour frames came back with **zero** differing pixels every time,
  `showcase_646` included. A road ribbon is a few thousand triangles against terrain's 200k; if a
  future road scene starts flaking, M22's fix is to give the pass something to draw afterwards.

Fixture `verify/m23_road.json` at `--steps 180`, pinned by a CLI test that also drops a ball on the
road and requires it to *stay where it lands*.

## Agent ergonomics (M24/M25, `designs/agent-ergonomics-design.md`)

The README claims *discover by looking, verify by querying*; this is the querying half catching up.
No component, renderer or physics code was touched, and `bin/verify-baselines` reported 30 of 30
unchanged both times.

- **Negative coordinates parse.** `raycast --from -6,20,6` used to be `unexpected argument '-6'`.
  `allow_hyphen_values` is now on the *class*: `raycast --from`/`--dir`, `terrain-height --at`,
  `screenshot`/`diff-render --time`, `filmstrip --start`/`--end`. Teaching the guide to write
  `--from=` was rejected: a workaround documents a defect instead of removing it.
- **`engine terrain-height <scene> --at x,z [--entity N]`** reports `{entity, x, z, height}` — the
  world Y a caller assigns straight to a position. It **needs no `Collider`**, which is what separates
  it from a downward raycast (that asks where the *collider* is). M22's one-implementation claim is
  now enforced by a function: `terrain::world_height_at` composes the field with the patch's
  transform, and the script API, `Scene::terrain_height` and the CLI all call it.
- **`engine inspect <scene> [--entity N]`** prints each entity's components with **every field filled
  in**, plus its resolved transform, name-sorted. Absent fields *are* the documented defaults, so the
  file under-specifies the entity by design (writing this milestone's test, the author guessed
  `Material.roughness` was 0.5; it is 0.9). Resolution goes through `ComponentData::collect_from` and
  the ordinary serde impls, never a re-derivation in the CLI. It is a pure function of the file **at
  rest** — no `--steps`, so `inspect` answers "what did you author" and `simulate` answers "what
  happened".
- **`engine list-components --component <Name>`** lifts one schema out of the `oneOf` (unknown name =
  `unknown_component_query`, exit 1, with `did_you_mean`). Without the flag the output is
  **byte-identical** to `schemas/component-schema.json`, and a repo-contract test says so. The trap: a
  lifted variant keeps `#/$defs/...` pointers into the document it came from, so the referenced
  definitions are collected **transitively** and carried along. Reshaping the top-level output to key
  schemas by name was rejected — it breaks the schema file, the validation walk, the editor's widget
  generation, and any agent script in the wild, to save one `jq` selector.
- **`simulate` says where everything ended up.** The new `entities` array **is the trace's rows**:
  same fields (`position`, `rotation`, `linear_velocity` when there is a `RigidBody`), same omissions
  (no angular velocity, no scale), and the same membership rule — the dynamic bodies re-enumerated
  after the run. **Name-sorted is a contract, not cosmetics.** `--entity NAME` (repeatable) narrows
  *and* reaches what no trace enumerates: a fixed floor, a scripted kinematic platform, a chase
  camera. Unknown names are reported all at once. The trace format, the bake format, and both golden
  traces are untouched.
- **`screenshot`/`filmstrip` report a frame `digest`**: `mean_luminance`, `background` (the most
  common exact color, as sRGB bytes), and `coverage` (the fraction that is anything else).
  `entities_drawn` catches "nothing loaded" and cannot catch **"nothing is in the frame"** — a camera
  aimed past the scene renders a perfectly correct empty picture, and `coverage: 0.0` is that,
  without the image read. Luminance is over the **encoded** bytes, since the question is whether the
  PNG looks black. "Background" is the frame's *mode* rather than the clear color, which is what keeps
  it meaningful under a sky gradient; ties break toward the numerically smallest color.
- **The digest is quantized to three decimals, and that is the load-bearing part.** This adapter
  renders a terrain frame ~24 pixels differently run to run; at full precision the mean would differ
  in its low digits between two runs of an unchanged scene, turning a diagnostic into phantom diffs.
  The measured worst case moves it by ~3e-5 against a 1e-3 step. **Nothing may pin the digest** —
  `diff-render` pins renders, bit-exactly and with a diff image showing where.

Output-shape rule this settled, for the next command that prints something: **schemas pretty-print,
reports do not.**

## Materials (M26, `designs/material-system-design.md`)

`Material` gains texture maps, a file form, and refraction. **Every added field defaults to the
pre-M26 behaviour** — no maps, an identity UV transform, no alpha cut, `ior: 1.0`, `thickness: 0.0` —
which is what let the milestone land with every committed baseline untouched except the six the
showcase tour's own edit re-blessed.

- **The bind-group budget decided the shape.** `downlevel_defaults` caps `max_bind_groups` at 4, and
  three were spent on frame-scoped textures that arrived in three milestones. Group 2 is now **frame
  textures** — shadow map + comparison sampler, depth copy, colour copy + sampler — which gives
  meshes a material slot at 3 and frees water's. Two bind groups are built from it, differing only in
  whether the colour copy is bound: on the refracting path the opaque pass is *drawing into* that
  copy, and a texture cannot be an attachment and a resource in one pass.
- **`with_surface(producers)` is M22's splice, named and generalized.** Terrain, textures and
  refraction are `Producer`s — a prelude plus anchored substitutions against `mesh.wgsl` — composed
  because a textured surface can also refract. Every anchor is asserted to appear exactly once, and
  `every_producer_actually_replaces_what_it_claims` pins that each substitution *landed*: a splice
  that silently did nothing renders the feature as if it were absent, which is the failure mode
  hardest to see. One **shared extended object-uniform tail** goes into every variant, because
  uniform field offsets are positional.
- **Colour space is a property of the slot**, never the file and never a field: `albedo_map` and
  `emissive_map` decode, `orm_map` and `normal_map` do not. It also decides how the mip chain was
  filtered — averaging sRGB-encoded bytes darkens every level — so `TextureSource` keys its cache on
  `(asset, space)`. Mips are generated on the CPU by a box filter written out in-repo, for the reason
  every generator here is: a render sits under a baseline, so the filter is a format contract.
  `texture_too_large` (2048 a side, the device limit) fires from `validate`, before a device exists.
- **`Material.asset`** names a `materials/*.json` and is **exclusive with every other field**
  (`material_asset_with_fields`), checked against the raw JSON rather than the parsed component: every
  field has a serde default, so the parse cannot tell an override from someone spelling out the
  default. A material file's own texture references are relative to **it**, rebased onto the scene
  once at load — that is what makes one shareable. `Material` has a **hand-written `Serialize`** that
  emits only the reference when `asset` is set, so a baked scene points at the file instead of
  inlining a copy that would fail its own validation.
- **Tangent frames are derived per pixel** from screen-space derivatives, so `Water`, `Terrain`,
  `Road`, `Tree` and `Cloud` take normal maps with no tangent generator each and no `MeshData` change
  (no `Arc` changes identity, nothing re-uploads).
- **`alpha_cutoff` cuts the shadow too**, through a second caster pipeline with a fragment stage —
  `shadow.wgsl` has none, and a leaf that cuts its pixels but not its shadow casts the silhouette of
  its own quad. That caster is `cull_mode: None`: **the solid caster is front-face culled**, so a flat
  single-sided card facing the sun is culled out of the shadow map entirely. Worth knowing before
  debugging a missing shadow.
- **Refraction is a third blended pipeline, not a branch in the second, and that was measured.**
  Compiling the refraction variant for every transparent draw moved one pixel of `m16_environment` by
  one channel step — M22's lesson repeating. The transmitted background is added **after fog**: the
  copy was already fogged at its own depth, and fogging it again turned the tour's ice into a pale
  slab. The colour copy is gated like M18's depth copy, so a scene with nothing refracting renders
  the pre-M26 pass structure exactly.
- **`engine import`** writes a glTF's materials out as files and its embedded images as PNGs (deduped
  by an in-repo FNV-1a of their pixels — it decides file names). The editor's drag-and-drop calls it
  rather than reimplementing it. Occlusion is the lossy spot — glTF allows a different image from
  metallic-roughness while `orm_map` packs them — so a repack warns. Re-importing refreshes the files
  and leaves the scene alone.

**Two traps written down.** An unwritten 1×1 placeholder rendered as a stable magenta that looked
exactly like a mip-chain bug and was chased as one; placeholders are written now. And
**`builtin:plane`'s UVs are not the intuitive ones** — `quad(+Y, +Z, +X)` puts `u` along local +Z and
`v` along +X, so a texture's "left half" lands on the top of an upright card. Fixing the builtins'
layout is deferred as its own change with its own A/B.

Fixture `verify/m26_materials.json` + baseline, aimed at its subject with no terrain in frame per
M22's rule, so it carries a hard bit-exact pin. Textures are generated by
`examples/textures/make_textures.py`; the import fixture by `examples/meshes/make_textured_quad.py`.
Not here: IBL and prefiltered probes, parallax, decals, texture compression, stored tangents,
anisotropic filtering (pinned at 1 — a per-adapter quality knob is where reproducibility dies),
textured terrain layers, and textured roads.

## Meadows (M29, `designs/meadow-design.md`)

`Meadow` is ground cover with a **life cycle**: seed → sprout → grass → flowering weeds → dry straw →
collapse → seed, on the scene clock, so `cycle_length: 3.0` runs a whole generation in three seconds.
A recipe like `Tree`/`Cloud`, so the entity carries **no** `Mesh` and **no** `Material`
(`meadow_with_mesh`), sized by `Transform.scale` in XZ.

**It is the first recipe here whose subject changes shape over time**, and the whole design is the
answer to how that avoids minting a mesh per frame (M15 keys the upload cache on `Arc` identity).
Two static buffers per meadow — a **template** (one plant at maximum extent) and an **instance
buffer** (36 bytes a plant) — and everything visible happens in `meadow.wgsl`'s vertex stage from
`ScenePass.time`. Water's M18 trade, on a harder case: water kept its topology, a plant has to change
*organs*.

- **Shape change is a scale animation on parts that are always in the buffer.** Every vertex carries
  the phase window (`emerge`..`wither`) its organ lives in; outside it the organ scales to zero about
  its own anchor and its triangles rasterize nothing. No second draw, no index rewrite, no divergent
  branch. The template therefore holds the union of every stage's organs — blades, a flower head, a
  seed head — at all times.
- **`generation = floor(progress)` is what makes the cycle regrowth rather than a loop.**
  `hash(plant.seed, generation)` in the shader re-draws each plant's position within its cell, its
  height, lean and heading every time round, so the dead stalk and the sprout replacing it are not
  collinear. One integer hash, **no state anywhere** — the render stays a pure function of (file,
  time). The reseed hash is a **format contract** and is spelled out in the shader for the reason
  every generator here spells its own out.
- **`cycle_length: 0` (the default) freezes the field** at `phase`, exactly as `daylight.day_length:
  0` freezes the day. `stagger` desyncs plants; `0` marches the field in lockstep, `1` shows every
  stage at once and so never appears to change — the default is near the low end because a real
  meadow browns together.
- **`MeadowVertex` carries `centre` and `offset` separately**, and that is not tidiness: height scales
  by the stage's `height`, girth by its `width`. One combined position would make a taller plant
  proportionally fatter and leave `blade_width` — authored in metres — meaning something different at
  every stage.
- **The cache key covers the transform *and* the terrain.** Instances are placed in **world space**
  (altitude sampled through M22's `terrain::world_height_at`, the one implementation), so keying on
  the component's own fields would leave a moved meadow, or a re-shaped terrain under a still one,
  with grass floating at the old ground's height. `terrain_moves_rebuild_the_patch` pins it. Each
  instance also carries the ground's **gradient**, so a plant that reseeds a few centimetres away
  lands at the new spot's altitude rather than the old one's.
- **Every cell draws its full set of random numbers whether or not the slope test keeps its plant** —
  otherwise raising a hill at one corner reshuffles the grass at the other. M17's
  "defaulted fields consume no randomness", generalized.
- Rendering is `shaders/meadow.wgsl`, a new **instanced** pipeline duplicating `mesh.wgsl`'s lighting
  with `sky_common.wgsl` prepended (the `water`/`road`/`clouds` precedent, M16's reason). Opaque,
  depth-writing, drawn last in the opaque run. **`cull_mode: None`** with the normal flipped toward
  the viewer — a blade is a single-sided strip and half of every tuft faces away. **Grass receives
  shadows and casts none**: one 2048² cascade cannot resolve a blade, and what it would record is
  sub-texel noise that crawls. `ROOT_SHADE` (darkening toward the root) is what stands in for the
  missing self-shadow, and `BACKLIGHT` is what makes a field lit from behind glow.
- Budget is **`MAX_MEADOW_TRIANGLES` (8M), counted in triangles** rather than plants, because only the
  product of plant count and template complexity hangs a render. Measured: 1.3M draws in 0.19 s,
  7.1M in 3.6 s (debug). Geometry fields are in `NOT_ANIMATABLE`, `stagger` included — a plant's phase
  offset is drawn once, at placement.

**M29 is where this adapter's reproducibility limit gets sharper, and the two artifacts settle it
oppositely.** A meadow at `samples: 4` is *not* byte-reproducible: six renders of the unchanged
fixture came back as six distinct PNGs (1874 px, delta 69). At `samples: 1` eight renders are one
image. **Relief is not required** — the fixture's ground is `height: 0.0`, a flat patch — so M22's
rule is really "enough sub-pixel geometry", and a meadow is the densest source of it in the engine.
So `verify/m29_meadow.json` renders at **`samples: 1`** and keeps a hard bit-exact pin, while **all
six showcase baselines now carry `"diff_args": ["--threshold", "24"]`** (the tour is stable without
the meadow — 8/8 identical — and the meadow is visible in every frame, worst drift 203 px / delta 20).
That is a real loss of five bit-exact pins, recorded rather than hidden: **a new fixture wanting a
hard pin on ground cover must render it at `samples: 1`.**

Four authoring rules came out of looking at renders: blades must be **thin** (2 cm is a real
measurement and renders as ribbons; 7 mm at higher density reads as grass), **every blade arches**
including the central one (`reach`'s `+ 0.55` — a rigid vertical wire up each tuft read as wheat),
heads are **stretched spikelets** not beads, and the flower colour sits **near the plant's** or it
scatters as dots. Not here: trampling and thatch (both need history, and history is hidden state), a
spatial wave across the field, textured or alpha-cut blades, slope-aligned plants, and LOD.
## Skeletal animation (M30, `designs/skeletal-animation-design.md`)

**CPU skeleton, GPU skin, and both halves of that sentence are forced.** Skinning cannot happen on
the CPU: posing vertices there mints a new `Arc<MeshData>` every frame and defeats M15's upload
cache (M18's argument, same answer). The skeleton cannot happen on the GPU without costing the
milestone its point — a joint palette is a few dozen matrices, and *because* they exist on the CPU
`engine list-joints --time 0.7` can say where every joint went, a script can put a torch in a hand,
and the whole sampling path is GPU-free and unconditionally testable the way `daylight.rs` is.

- **No new component.** `AnimationPlayer.clip` gains the fragment form `meshes/robot.glb#Walk` that
  `animation-system-design.md` §4 specified and nothing had used. A skin is a property of the
  *asset*, and `Mesh.asset` already names it; a `Skeleton` component would be a second source of
  truth for what the file contains. The fragment is **required** even when the file has one clip —
  defaulting is friendly right up until someone exports a second one and which clip plays changes
  silently. Ownership rules, all validation errors: `skeletal_player_mesh_mismatch`,
  `clip_needs_fragment`, `mesh_has_no_skin`, `unknown_clip` (with `did_you_mean`), and
  `too_many_joints` past `MAX_JOINTS` (128) — refused before a device exists rather than a rig that
  renders correctly up to joint 128.
- **Rotation is a quaternion here, slerped, shortest-path — the opposite of M9's rule**, and the
  distinction is *who wrote the numbers*. A property clip's keys were typed by an agent into JSON
  where `[0, 360, 0]` is a sentence that must actually spin; a skeletal clip's came out of a DCC
  tool through a specified format where the only correct reading is the spec's. Don't "unify" them.
- **A skinned primitive loads unbaked.** glTF says the transform of the node referencing a skinned
  mesh is *ignored* — the palette already speaks skin space — while `gltf_mesh.rs` bakes node
  transforms for static geometry, which is right for that and exactly wrong here. This is the single
  most likely thing to be "simplified" back into a bug; the symptom is a character posed correctly
  in the wrong place, or one that doubles its own root transform. `JOINTS_1` (a fifth influence) is
  **refused**, not dropped: a dropped influence is a wrist that collapses under rotation.
- **The palette rides group 0 at binding 1** with its own dynamic offset — `downlevel_defaults` caps
  `max_bind_groups` at 4 and M26 spent the fourth on materials, so there is nowhere else. Packed as
  **three `vec4` rows, not `mat4x4`**: a joint matrix's fourth column is always `(0,0,0,1)` and
  storing it wastes a quarter of the 16 KiB budget. **Joint order is the skin's own `joints` order
  and must not be sorted** — unlike point lights, a joint's index is written into the vertex data.
- **The vertex stage is assembled from producer contributions**, not replaced wholesale. Texturing
  needs a UV the plain stage does not carry and skinning needs two more attributes, and a rigged
  character is precisely the thing that wants both — whole-stage replacement worked while exactly
  one producer did it. A `VertexContribution` names attributes, varyings, statements, and at most
  one expression transformed in place of `position`; `an_unassisted_vertex_stage_is_the_one_in_the_file`
  asserts the empty assembly equals `mesh.wgsl`'s stage **character for character**, which is what
  keeps M16's four untouchable lines reachable. The A/B said 29 of 29 committed render artifacts
  byte-identical.
- **The skinned pipelines are built lazily**, on the first frame that has a skinned draw — six
  shader modules is a real startup cost and one scene in this repo has a rig. Same precedent as the
  shadow map, the 1×1 white texture and the colour copy.
- **A skinned caster is its own pipeline**, because `shadow.wgsl` reads nothing but the model matrix
  and a walking character would otherwise cast its **rest pose** — a wrongness that reads as a
  renderer bug and is a missing pipeline. Both casters are skinned (solid and M26's alpha-cutout).
  The solid one is front-face culled, M16's peeling margin, which applies to characters too.
- **Scripts get two read-only getters and no setter**: `world.joint_position(entity, joint)` and
  `world.joint_transform(entity, joint)` (position plus XYZ Euler degrees, six numbers in one call
  so the rig is posed once). M21's reason — a script-settable joint is hidden state (invariant 2)
  and the pose must stay a function of (files, time). Hanging a prop off a hand is then an ordinary
  `set_position`, which bakes change-based and shows up in the trace. A mistyped joint is a located
  runtime error with `did_you_mean`, matching `world.key`.
- **The rest pose still needs a palette.** `render_items(assets)` is the rest pose and
  `render_items_at(assets, Some(t))` is posed; the tempting shortcut of an identity palette collapses
  any rig whose rest pose is not exactly its bind pose, since the vertices live in skin space.

Fixture `verify/m30_skeletal.json` at `--time 0.4`: two copies of `examples/meshes/rigged_arm.gltf`,
one playing `Wave`. **The two arms are the assertion** — they share a file, a mesh and a material, so
anything that made both wrong would leave them identical; only real skinning makes one bend and the
other stand, and the bent one's shadow bends with it. **Measured rather than assumed** (§9 warned it
might go the other way): this baseline is *not* per-build-profile, unlike trees and clouds — three
joints of slerp is not enough libm to reach a pixel, and a hundred-joint rig may not inherit that.
Test assets are generated text glTF like `pyramid.gltf`: `make_rigged_arm.py` (3 joints, the fixture)
and `make_rigged_walker.py` (13 joints in a branching tree, `Walk` + `Idle`, UVs — the tour's
character, and the only **skinned × textured** draw in the repo).

Not here, deliberately: blending, crossfades and state machines (M9 §8's rejection still standing —
blending reintroduces exactly the nondeterminism that made two clips on one property an error), IK
and root motion, retargeting, morph targets, skinned colliders (a skinned mesh is visual; physics
sees whatever `Collider` the entity carries, posed by its `Transform`), per-joint attachment
components, and editor picking against the posed mesh — CPU ray picking hits the rest pose.

## The UI system (M31, `designs/ui-system-design.md`)

M12's two screen-space primitives were enough to *read* state out of a running scene and not enough
to build a **screen**. M31 adds the three things that were missing — layout, images, and widgets a
pointer lands on — and adds no input code at all, because M28 had already shipped the pointer.

**The design doc was drafted before M28 and planned to invent its own** (pixel cursors, a per-
keyframe `viewport`, an `input_viewport_mismatch` error). §7 is now a record of that reversal rather
than a specification: a timeline outlives the window it was recorded in, so M28's *fraction* is
right, and the draft's error code would have reported that failure rather than removed it. The
concern was real but belongs to the **layout** — which is what `engine ui-layout` answers.

- **`HudPanel` is the component that removes hand-computed offsets.** `layout` is `free`/`row`/
  `column`; **absent `width`/`height` means hug contents**, which is the default because it is the
  case that makes a dialog authorable. `opacity` defaults to **0**, so a bare panel is an invisible
  layout *group* and the same component is also the dialog's backdrop — one component, not a
  container plus a rect whose size has to be kept in agreement with it.
- **Hierarchy is a `parent` name in a flat file** (the `Wheel.vehicle` precedent), shared with
  `visible` and `stretch` by all four elements. Nothing structural guarantees it resolves, so five
  codes do: `hud_parent_not_found`, `hud_parent_not_panel`, `hud_parent_cycle` (the message names
  the ring), `hud_nesting_too_deep` (16), `hud_interact_without_element`. Cycles are a *validation*
  error rather than a layout guard — `tree_too_complex`'s argument — though `Structure::resolve`
  still roots an offending node so layout terminates on a scene that reached it anyway.
- **Two properties make the restructure free rather than merely cheap, and both are asserted rather
  than measured.** `ui::anchored` is M12's expression verbatim, so an unparented element resolves
  through arithmetic that is textually the same; and the new draw order (depth-first, a panel before
  its children, siblings by `(class, file order)`) **collapses** to "rects then texts, each in file
  order" when nothing names a parent. There is no arrangement of pre-M31 components the two rules
  sort differently. The A/B said **29 of 29** artifacts byte-identical between binaries.
- **Flow order is file order; draw order is `(class, file order)`.** Two orderings of one sibling
  set, and conflating them is a bug with a confusing symptom: the class sort exists so text reads
  over the backgrounds it sits on, and running a *column* in that order stacks every button above
  every label however the file reads. This cost a fixture render to find.
- **Layout runs in f32 end to end and rounds once per element, at emission.** Rounding per level
  would let a nested element drift a pixel per level. Hidden elements leave the flow entirely (they
  take no space) and a stretched child contributes nothing to a hugging parent — both so hug sizing
  cannot be circular. `stretch` on a `row`/`column`'s **main** axis is ignored, since distributing
  leftover space is flex-grow and that is a named non-goal.
- **`HudImage` samples nearest-neighbour, written out in-repo** for the reason every generator here
  is: a render sits under a baseline, so the filter is a format contract. Nine-slice copies corners
  1:1 and **tiles** edges and centre — tiling at nearest is exact where stretching at nearest is a
  moiré pattern. Only the base mip is read (the overlay never minifies below one destination pixel
  per texel band). `hud_image_slice_too_large` lives in **engine-assets**, not engine-core, because
  comparing an inset against its source needs the PNG decoded — the `texture_too_large` division.
- **Interaction is polled, never dispatched**: `world.hovered`/`pressed`/`clicked`. An `on_click`
  field would need a second addressing scheme for which `Script` owns the handler, a dispatch-order
  rule, and mid-step reentrancy; `world.key` set this shape in M11 because a button that runs code
  is a *binding*, and bindings are game logic. The **press capture** is the one thing a polled API
  cannot derive for itself, so the engine keeps it — runtime state of `world.state`'s kind:
  replay-deterministic, reset by a fresh run, **not baked**, since a half-finished click is not a
  property of the scene. Press-inside/release-outside does not click; `clicked` is true for exactly
  one step. **`MouseLeft` alone drives the widget model**, the other two staying raw.
- **Tints are applied to the extracted tree just before drawing, not inside the rasterizer** — the
  renderer has no business knowing what a pointer is, and `hud::rasterize` stays a pure function of
  (tree, lines, size). The `[1, 1, 1]` defaults make `apply_tints` a no-op for any scene with no
  cursor over an interactive element, which is why adding a `HudInteract` moves no pixel.
- **Hit-testing runs before scripts and is *not* gated on a scene having a `Script`** — hover and
  press tints are a property of the components, so a menu that lights up needs no script. It is
  gated on there being an overlay at all, so a scene without one takes the pre-M31 path exactly.
- **`engine ui-layout <scene> [--width W --height H] [--entity N]...`** is the command the milestone
  is really for: `road-centerline`'s argument applied to buttons. It reports the same rectangles the
  rasterizer draws from and the hit test uses, name-sorted, at rest (no `--steps`, no cursor) — and
  a CLI test closes the loop by turning a reported rect into the cursor *fraction* that hits it,
  which is the one place the pixel report and the fractional timeline have to agree.

**A trap worth knowing before authoring a fixture**: M28 defines an absent cursor as the **centre of
the frame**, so "no `--input`" is not the untouched case if anything interactive sits in the middle —
`verify/m31_ui.json`'s first button does, which is why its untouched-render test compares against
`--steps 0` instead.

Fixture `verify/m31_ui.json` + timeline at `--steps 30`, 640×360: a stretched dim backdrop, a
nine-sliced frame, a hugging column of title, wrapped centred body text, and two buttons — with the
**second held down**, the state hardest to reach and the one nothing else pins. Aimed at its subject
with no terrain in frame per M22's rule, so it carries a hard bit-exact pin (four consecutive renders
identical). Not here, deliberately: TTF text (a bitmap-font atlas is the sanctioned next step, and
the 8×8 font's fully-on-or-off glyph pixels are what the whole verification story rests on), pointer
lock and scroll, text input and focus, percentage sizes and flex-grow, rounded corners and shadows
(all anti-aliasing, which is where the CPU rasterizer's bit-exactness lives), and world-space UI.


## Showcase tour (`designs/showcase-tour.md`)

`examples/scenes/showcase_tour.json` is a 15-second (900-step) camera move through five 180-step
stations — forest / campfire / water+ice / breaking / wide — with every system running at once, plus
four scripts (`scripts/tour_{director,wildlife,effects,truck}.rhai`) and six 640×360 baselines
(per-adapter, checked by hand with `diff-render`, not by a CLI test).

**The camera path is a closed cycle, not a timeline that ends.** Six legs over seven keys (the
seventh is the first again), read through `p = step % 1080`, so past step 900 leg 5 flies the camera
home from the wide finale and the five stations come round again on an 18-second lap — the director
used to clamp its station index, which replayed the finale's own three seconds forever while the
world went on moving. **Nothing resets on a lap**: breaks stay one-shot, so station 04 later shows a
debris field, and `day_length: 300` means lap two is dusk. The first lap is *arithmetically* the
pre-loop one (`step % 1080` is the identity below 1080, and the time bar picks a numerator and
denominator rather than scaling a fraction), so all six baselines diff at zero pixels. Rhai's
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
  different `uv_scale`s for exactly this reason — see `designs/arena-shooter.md`, which M28 also gave
  a mouse-driven title/pause/end menu built from ordinary `HudText`/`HudRect`: the layout is measured
  from the **centre** of the frame so one recorded timeline clicks the same button at any window
  size, hiding an element is a zero size or an empty string, and its demo timeline is now authored by
  a closed-loop director, `make_arena_demo.py`, because nobody can hand-write which *pixel* is on a
  drone at step 431).
  The crate texture is a *framed* panel with a centre batten for that reason: a border is invariant
  under it. `Tree` tubes are the well-behaved case (`u` around the ring, `v` along the branch), which
  is also why bark fissures must vary in `u` — transposed, a trunk wears tyre tread.
- **The ice is deliberately unmapped**: refraction is what station 03 is showing and a frost normal
  map competes for the same pixels. So are the critters and the beacon, which are stand-ins.

Those edits are why the six showcase baselines were re-blessed — the sweep confirmed the other 25
held bit-exactly, since no engine code was touched.

**M30 adds the `Walker`** to station 01: thirteen joints out of `examples/meshes/rigged_walker.gltf`
playing a one-second `Walk`, carried around a circuit by `tour_wildlife.rhai` while the clip does the
legs — the milestone's division of labour, since no script ever touches a joint. It is the repo's
only **skinned × textured** draw (`plate_normal` + `plate_orm`, the truck's maps), which is the
composition the vertex-stage seam was rebuilt for. It stands *between* the station-01 camera and the
trees deliberately: a two-metre figure thirty metres back behind a canopy is a pale smudge. Five of
the six showcase baselines were re-blessed for it and `showcase_450` was **byte-identical** — station
03's camera is aimed the other way, which is the cheap confirmation that one added entity changed
only the frames it is in. Faked and named as such in the tour doc: the stride is hand-tuned to the
speed rather than driven by it, there is no foot IK, and `Idle` is in the file but never crossfaded
to, because a crossfade is the nondeterminism M9 refused.

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

## Distribution (`designs/distribution-design.md`)

- **Prebuilt binaries.** `.github/workflows/release.yml` builds `engine` natively on four runners
  (macos-14/13, ubuntu-22.04, windows-2022) on a `v*` tag and uploads tarballs plus `SHA256SUMS`;
  `install.sh` (POSIX sh, `curl | sh`) resolves the latest tag, verifies the checksum, and drops the
  binary in `~/.local/bin`. Linux is built on the *oldest* supported Ubuntu deliberately — the
  artifact's glibc floor is whatever runner built it. `.github/workflows/ci.yml` runs fmt, clippy and
  the workspace tests; the render tests skip there for want of an adapter, so **CI proves the GPU-free
  half only** and baselines stay a local, per-adapter check. **crates.io is closed to this workspace**:
  `engine-editor` pins egui to a git rev, cargo refuses to publish anything with a git dependency, and
  `engine-cli` depends on the editor — so `publish = false` is a *consequence*, and
  `cargo install --git` is the toolchain path until egui 0.36 lets that pin become a version.
- **`engine init [dir]`** scaffolds a project, because the binary alone is not enough: an agent in an
  empty directory has no way to know the loop is the point. It writes `AGENTS.md` (Codex/Cursor/Amp)
  and `CLAUDE.md` (an `@AGENTS.md` import so there is one source of truth), a starter scene, and a
  script. **The scene sits at the project root, not under `scenes/`** — asset paths resolve relative
  to the *scene file*, so a nested scene reaches its own scripts through `../scripts/`, which is the
  first thing anyone copying the layout gets wrong. It refuses a non-empty directory
  (`init_target_not_empty`, exit 2) unless `--force`. Files are `include_str!`'d, so a `curl | sh`
  install with no checkout carries all of them.
- **`engine agent-guide`** prints that same `AGENTS.md` text — the binary is self-describing, so
  `--help` + `agent-guide` + `list-components` is a complete onboarding with no repo. It is
  **markdown on stdout**, a documented exception beside `--help`/`--version` in
  `docs/cli-contract.md`. A CLI test asserts `init`'s `AGENTS.md` is byte-identical to
  `agent-guide`'s output, so the two cannot drift. The guide is written for someone *using* the
  engine, which is the opposite audience from this file — keeping it accurate is part of adding a
  component, the same way the showcase tour is.

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

**25 of the 36 baselines are pinned by no test at all** — the sweep is their only check. The eleven a
test actually diff-renders and asserts *matching* are `m12_hud`, `m16_environment`, `m17_fire`,
`m18_water`, `m19_trees`, `m20_clouds`, `m21_daylight_1200`, `m22_terrain`, `m23_road`,
`m30_skeletal` and `m31_ui`; everything else — `m4_lighting`, both `m8_drop`, `m9_t025`, both `m10`,
`m11_lap`, `m13_smoke`, `m14_break`, the other four `m21_daylight_*`, `m26_materials`, both `m27_*`,
both `m28_pointer_*`, `m29_meadow`, and all six `showcase_*` — rides on `bin/verify-baselines` alone.
**This ratio has been getting worse, not better**: it was 16 of 35 when last counted, and M26/M28
each added fixtures whose baselines no test asserts. Two entries mislead if skimmed.
`m27_water_refraction.png` *is* named by a CLI test, but only in the **negative** direction (with
`ior` back at 1.0 the baseline must *not* match), which pins that refraction is load-bearing and
does not pin the render. And `m11_lap.png`: the lap CLI test pins the *drive* (positions, elevation,
parked HUD strings) and names the PNG in a comment, but nothing diff-renders it. A sweep failure that will not reproduce twice in a row is worth suspecting before it
is worth debugging: since M29 **all six `showcase_*` frames** carry a `diff_args` tolerance of
`--threshold 24 --max-diff-percent 0.02`, because a meadow at `samples: 4` is not byte-reproducible
on this adapter and the tour has one in every frame (M22 had already given `showcase_646` a
threshold for its own reason). The pixel *allowance* is there rather than a wider threshold because
the residual is one or two pixels well outside it, not a haze just over it — 24/0.02 held for eight
consecutive full sweeps where `--threshold 40` alone would have been a looser claim. The other 30
entries carry no `diff_args` at all — they are bit-exact, and a failure there is real.

**M31 measured the tour's flake rate directly**, which is the cheap way to settle one of these: with
the *unchanged* pre-M31 scene, `showcase_585` came back as **5 distinct images from 6 renders** on
the M31 binary and **3 from 6** on `main`'s. A frame that disagrees with itself on both sides of a
change is the adapter; `cmp`-ing one render against one render would have called that a regression.

The clippy cleanup re-measured it on **two** frames and got the same answer, which is worth knowing
before anyone reads an A/B result as a regression: `showcase_585` came back **6 distinct of 6** on
the new binary and **5 of 6** on `main`'s, and `showcase_646` **3 of 6** and **4 of 6**. Its A/B
found exactly those two frames differing out of 36 artifacts — and neither binary agrees with
itself on either, so the difference is the adapter and not the change. **This is the reason the
`md5`-it-N-times step is not optional**: a two-artifact A/B failure looks damning and here meant
nothing.

**Blessing gotcha that cost a sweep here: `--filter` is a substring match, not a regex.**
`--filter "m28|showcase"` matches nothing and blesses nothing, reporting success — run one filter
per artifact family and check the `checked` count in the summary line.

The three repeated rituals are skills in `.claude/skills/`: `verify-baselines`, `ab-check`,
`milestone`.

`cargo test --workspace` is the real check, not `cargo build`.
`crates/engine-render/tests/headless_render.rs` renders offscreen and asserts on pixel values,
because "the window opened and did not crash" does not distinguish a working renderer from a culled
triangle or a shader that writes nothing. Those tests skip cleanly (rather than fail) when no GPU is
available.

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

Cargo workspace (design doc §4), dependency order bottom-up:

- `crates/engine-core` — ECS, scene graph, math re-exports (glam)
- `crates/engine-render` — wgpu renderer, shaders, materials
- `crates/engine-assets` — mesh/texture loading, asset schema
- `crates/engine-cli` — the `engine` binary; the primary interface

Plus `engine-physics`, `engine-script`, `engine-editor`. Supporting:
`schemas/component-schema.json` (generated, not hand-written), `examples/scenes/*.json`, and
`docs/` — which holds `cli-contract.md` and `error-codes.md` and **nothing else**. The design doc's
§4 sketch also lists `docs/component-reference.md` and `docs/scene-format.md`; neither was ever
built, and the component reference today is `engine list-components` plus the doc comments it
carries into the schema. Don't cite either file as if it exists.

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

**Runtime scripting: Rhai** (M10, `designs/scripting-design.md`).

## Open decisions — ask, don't assume

Still unsettled (design doc §9). If a task forces one, surface it rather than picking silently:

- Whether to support hot reload of scene data without a Rust rebuild

## Build order and remaining work

M0 window+triangle → M1 CLI skeleton + JSON error convention → M2 JSON scenes + ECS → M3 glTF/texture
assets → M4 materials + lighting → M5 validation hardening → M6 diff-render → M7 GUI editor (E0–E2) →
M8 physics → M9 animation (A0–A1) → M10 scripting — **the roadmap is complete.** Each milestone from
M4 on ends by running its fixture from `designs/milestone-verification-scenes.md`.

Deferred follow-ups: editor E3 (structure edits) / E4 (undo), the
M5-era deferrals (`--fix`, watch mode), and — after M16–M20 — planar reflections, shadow cascades (which
is also what cloud shadows need), shadows from point lights, spot lights, a CPU wave evaluator and
buoyancy, a light on the tour's explosion, a sky-dome cloud layer for cirrus and overcast, and
tree LOD and wind. (Refraction and texture-mapped materials landed in M26, and the showcase tour's
bark is authored from them. **Alpha-cut leaves are still a missing feature**, not an authoring job:
`Tree::leaf_material` synthesizes a `Material` from `leaf_color`/`leaf_roughness` alone, so leaf maps
and an `alpha_cutoff` mean new `Tree` fields, a schema regeneration, and a validation pass.) After M23: road junctions (two roads crossing wants a patch primitive, not a ribbon), banked
cross-sections, per-point road width, roads that follow a `Terrain` instead of carrying their own
heights, and textures for asphalt grain (analytic markings beat a texture for anything periodic, but
grain is not periodic). After M30: foot IK (the tour's walker rides the terrain by its root, which
is not the same as planting on it), a locomotion system that drives clip rate from ground speed
instead of leaving the stride tuned by hand, skinned collider proxies, and editor picking against
the posed mesh. **Blending stays rejected**, not deferred — see the design's §1. After M31: a
bitmap-font atlas (the sanctioned path to better text — a PNG plus an in-repo JSON of glyph cells,
sampled nearest, no new dependency and no float, arriving as a `font` field whose absence is the 8×8
font), pointer lock and scroll, text input and focus, per-side padding, and world-space UI (a health
bar over an enemy's head is a *projection* question and wants `world.project(x, y, z)`).

**Housekeeping the M31 audit turned up and did not do**, in the order they are worth doing:

- **`scene_renderer.rs` has outgrown its file** — 5,977 lines, of which `SceneRenderer::with_samples`
  is ~880 (pipeline construction) and `SceneRenderer::draw` is ~1,150. `validate.rs` is 5,539 with
  `validate_source` at ~1,400. Splitting them is the one real structural debt in the workspace, and
  `draw` is exactly the ULP-sensitive path this file keeps warning about — so it wants its own
  change with its own A/B between binaries, never a drive-by while doing something else.
- **25 of the 36 baselines are pinned by no test** (see Verification). Each new fixture has been
  adding to that pile; a CLI test that diff-renders the fixture is cheap and is what makes a
  baseline survive someone who does not run the sweep.
- **`docs/scene-format.md` and `docs/component-reference.md`** are sketched in the design doc §4 and
  were never written. If either lands it must be generated and pinned like `error-codes.md`.

**The clippy warnings are cleared and CI's clippy step is blocking.** Six of the twenty-eight were
not bugs to fix but the lint being wrong, and they carry a local `#[allow]` with the reason —
**read it before deleting one**. Five are the `!(a > b)` comparisons in `validate.rs` and
`engine-script`, written negated *precisely so NaN fails*; clippy's suggested `a <= b` is false for
NaN, so "fixing" them would let a NaN far plane, collider dimension, meadow stage or explosion
radius validate clean. The sixth is `drop(write_object)` in `scene_renderer.rs`, which releases the
closure's mutable borrow of `object_bytes` — deleting it does not compile. Four
`too_many_arguments` allows carry their own rationale (a nine-field keyframe constructor, a
recursive validation walk threading a JSON location, a collider builder naming four geometry
sources, and eleven index-aligned slices on the blended draw path). One genuine defect fell out of
it: the editor cloned `ResolvedLights` under a comment claiming M17 had made it non-`Copy`, when
M17 had deliberately kept it `Copy` with a fixed-size point-light array — the comment asserted the
opposite of the design it cited.

## Out of scope for v1

GUI editor, networking/multiplayer, advanced rendering (GI, ray tracing), mobile/console targets.
Desktop only.
