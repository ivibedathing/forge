# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Current state

**M0–M23 are done — the v1 roadmap (M0–M10) is complete, plus M11 keyboard input, M12 vehicle wheels, M12 HUD components, M12 collision, M13 particles, M14 breaking objects, M15 frame cost, M16 environment (sky, fog, shadows, MSAA, transparency), M17 fire + point lights, M18 water, M19 procedural trees, M20 procedural clouds, M21 day/night, M22 terrain, and M23 roads** (and most of M1's CLI; M7 at scope E0–E2 + validation panel + --watch).
JSON scenes load into hecs, render headlessly to PNG with PBR lighting, validate with
all-errors-at-once reporting under a formalized CLI contract, reference glTF mesh files, pin
their renders against committed baselines with `engine diff-render`, and open in a GUI editor
that is a live writable *view* onto the file. Verified by 200+ tests including offscreen pixel
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
engine road-centerline <scene.json> [--entity Name]  # where a Road actually went
# Script component: {"type": "Script", "source": "scripts/x.rhai"} — runs fn step(world, step)
# once per fixed step (order: animations → scripts → physics → particles → render)
# Wheel component (M12): raycast-suspension wheel on its own visual entity, chassis by name —
# physics suspends/drives the chassis and writes the wheel's Transform back (steer/spin/bounce)
# ParticleEmitter component (M13): seeded deterministic smoke/sparks — cone spray around local
# -Z, advanced by --steps, rendered as soft alpha-blended billboards; never baked or traced
#   M17 adds the fire fields: "blend": "additive", "radius" (disc emission), "speed_jitter"/
#   "size_jitter"/"lifetime_jitter", "turbulence"+"turbulence_scale", "stretch" — all default
#   to the M13 behaviour, down to which random numbers the emitter draws
# PointLight component (M17): local light, inverse-square windowed at "range", up to 8 per
#   scene; no shadows. Scripts drive any light's intensity/color (world.set_light_intensity /
#   set_light_color), which is what lets a fire light the ground it stands on
# Road component (M23): a circuit, a street, a pass — the entity carries no Mesh
#   and no Material, the component owns one continuous ribbon swept from a polygon
#   of corners ("points" with a "radius"), and asphalt/shoulder/embankment are the
#   same triangles, so there is no ledge to catch a wheel. Markings are painted per
#   pixel from surface coordinates (u across, v along), so lines follow every curve
#   and dashes stay the same length in metres through a hairpin. A Collider with
#   "shape": "trimesh" and no asset takes the road's own geometry
# Tree component (M23): a recipe, not a mesh — seeded procedural trunk/branches/leaves grown
#   on load; carries no Mesh (tree_with_mesh), and the entity's Material is its bark
# Cloud component (M20): a recipe too — a seeded cluster of icosphere lobes, each growing
#   smaller lobes on itself, sized by Transform.scale; carries no Mesh and no Material
#   (cloud_with_mesh), shades through its own clouds.wgsl, and "drift" moves it on the same
#   reproducible clock water uses
# Scene-level "daylight" block (M21): the time of day as one number — sun/moon arc,
#   an eight-keyframe color palette, ambient, sky bands, and a fog *scale*; absent means
#   the pre-M21 engine byte for byte. Scripts read world.time_of_day()/sun_altitude()
# Water component (M18): a body of water — the entity carries no Mesh and no Material, the
#   component owns a tessellated grid ("segments") sized by Transform.scale, Gerstner "waves"
#   displaced in the vertex stage, per-pixel detail ripples, depth-based absorption between
#   "shallow_color" and "deep_color", crest and shore foam; a pure function of (file, time)
# Scene-level "environment" block (M16): {"sky", "sky_zenith"/"sky_horizon"/"sky_ground",
# "fog_density", "shadows", "shadow_distance", "samples"} — all default off, so a scene that
# omits it renders byte-identically to the pre-M16 engine. Material gains "alpha" (flat) and
# "transmission" (Fresnel, keeps specular), which move an entity into the blended pass
engine run-scene <scene.json> [--record-input f]   # windowed viewer + play mode (keyboard reaches
#   scripts); draws an FPS readout top-right — viewer-only, never in a headless render
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
recording driving three clockwise laps on real suspension and parking just past the start line —
pinned by CLI test and `verify/baselines/m11_lap.png`. Both were re-authored in M19 when the
plates became a `Road`: the car carries speed the plate road scrubbed off, so every number moved.

**The circuit is generated** (M15, rebuilt on the `Road` component in M19): `examples/scenes/
make_car_track.py` emits the scene from a closed polygon of 14 named corners (Spa in miniature — La Source's hairpin, the plunge to Eau
Rouge, the climb onto Kemmel, Les Combes at the crest, Rivage, Pouhon, Stavelot at the low
point, Blanchimont, the Bus Stop chicane), ≈546 m round with ≈7.6 m of elevation and grades to
7.5%. Authoring the loop as a *polygon* is what makes closure free: a closed polygon returns
to its first vertex and its exterior angles sum to one turn, so position, heading, and the
height profile all shut without a solver — corners carry `(x, z, radius, height)` and nothing
carries a heading. Two things the polygon can't guarantee refuse to build: a corner radius too big for the edges
feeding it (the *engine* checks this now — `road_corner_does_not_fit`), and a grade too steep
for the car to climb (still the emitter's business, since the engine has no opinion about cars).
Three geometry lessons are baked into the track and are easy to reintroduce by
"simplifying" it:

- **One collider, not two.** Asphalt, verge and embankment are one surface. Road and shoulder
  as two colliders at different heights builds a ledge at the asphalt edge, and a wheel that
  drops off it wedges against the step and stops the car dead. This was a *rule the emitter
  followed* until M19 (a deep box per segment carrying a colliderless slab 3 cm proud of it);
  it is now a property of the `Road` component, which cannot express the two-surface version.
- **The guardrail is continuous.** Posts are spaced 5 m and are 5.4 m long. Dashed barriers let
  the car slip between two of them and fall off the elevated road. They are placed along the
  centerline the engine reports (`engine road-centerline`), not one the emitter re-derives.
- **Radii are sized for the car, not the map.** The layout is Spa at ~1/15 but the car is full
  size, so no corner is under 12 m however tight the real one is.

`make_car_track_lap.py` authors the input timeline the same way it is replayed: a closed loop
that replays the whole timeline from step 0 each round, reads the car's state back out of the
`simulate` report's `hud` (a scratch copy of the scene whose driver pushes one telemetry line
— HUD is output, never input, so it drives identically), and appends the next tenth of a
second of keys. Steering is pure pursuit; the throttle brakes on a `v² = v_corner² + 2ad`
envelope, without which the car reads corners correctly and arrives far too fast anyway.
Regenerating the track means regenerating the timeline and re-blessing the baseline — the two
scripts print the start-line constants `car.rhai` needs.

HUD (M11.6 lines + M12 components, `hud-design.md`): two layers, one render path.
`world.hud(text)` pushes printable-ASCII debug lines, cleared every step — the line HUD is a
pure function of the step that drew it — and `world.state(key, default)` /
`world.set_state(key, value)` is numeric per-run memory on the ScriptHost (replay-
deterministic, reset by a fresh run, deliberately *not* baked — same disposability as
solver caches). Caps 16 lines × 96 chars, runtime error beyond. **M12 adds `HudText` /
`HudRect` components**: screen-anchored (anchor enum + pixel offset measured inward; five
anchors), pixel-sized, schema-validated (size/color/opacity ranges, anchor typos get
`did_you_mean`), needing no Transform and ignoring the camera. Text snaps to integer scales
of the 8×8 font (`size` 16 = 2×), colors are linear RGB like everything else, draw order is
rects-then-texts in file order, and the `world.hud` line panel draws topmost with its
original layout formulas. Rendering is `engine-render/src/hud.rs`: **one** CPU rasterizer
(unit-tested without a GPU) producing a target-sized sRGB straight-alpha canvas that
`SceneRenderer` composites as a sampler-less fullscreen-triangle blit (`ScenePass.hud`) —
`offscreen::render` and the `run-scene` viewer share it, so the played game and the pinned
PNG show the same overlay; an empty HUD draws nothing, keeping every pre-HUD baseline
byte-identical (opaque canvas texels land byte-exact through the sRGB round trip; the
editor viewport deliberately passes `hud: None` — its orbit camera is not the game frame).
Scripts drive components via `world.hud_text`/`set_hud_text` and
`world.hud_rect_size`/`set_hud_rect_size`; changed `HudText.text` / `HudRect.size` bake
under the change-based rule (unlike `world.hud` lines, which are per-step output). The line
HUD stays observable without pixels: `simulate`/`screenshot` report the final step's lines
as `"hud"`, and `--trace` logs `{"step", "hud"}` on every change. Fixture:
`verify/m12_hud.json` + `verify/baselines/m12_hud.png` (all anchors, draw order, opacity,
script-driven counter + growing bar, pinned by CLI test). Demo: `car.rhai` shows a
speedometer plus a lap timer (start-line crossing = z falling past the line on the pit
straight, remembered step-to-step via `world.state`) whose final parked HUD (`LAP 4`,
`LAST 64.37   BEST 64.15`) is pinned by the lap CLI test, plus a `SpeedBar` HudRect gauge
(bottom-left) driven by `set_hud_rect_size`.

Collision (M12): three additions, all opt-in so every pre-M12 trace and baseline is untouched.
**Script contact queries** — `world.touching(name)` / `world.contacts_started(name)` return
entity-name arrays from the touching-state the **previous** physics step left
(`engine_core::contact::ContactState`, applied from `PhysicsWorld::step`'s events after each
step; scripts run before physics, hence the one-step latency). `ContactEvent`/`ContactState`
live in engine-core so engine-script never depends on rapier. **Mesh colliders** — `Collider.
shape` gains `trimesh` and `convex_hull`; geometry comes from `Collider.asset` (`builtin:` or
scene-relative glTF) or, absent that, the entity's own `Mesh.asset` (neither is
`collider_missing_mesh`). Vertices are scaled by `Transform.scale`; a trimesh on a **dynamic**
body is a validation error (`trimesh_on_dynamic_body` — rapier trimeshes are hollow; use
`convex_hull`), and `PhysicsWorld::build` now takes a `&dyn MeshSource`. **Collision layers**
— `Collider.layers` (membership) and `collides_with` (filter) are arrays of free-form layer
names; absent means "everything" (which is why empty arrays are rejected —
`empty_collision_layers`), two colliders interact only if the filter passes **both ways**,
names map to rapier `InteractionGroups` bits sorted-name-deterministically (max 32 distinct
names per scene, `too_many_collision_layers`), and a `collides_with` naming a layer nobody is
a member of warns (`unknown_collision_layer`, with `did_you_mean`). The schemars gotcha
discovered here: a doc comment on an enum **variant** turns the schema from a flat `"enum":
[...]` into oneOf/const, which blinds the validation walk's closed-vocabulary check — keep
`ColliderShapeKind` variants undocumented (a NOTE in components.rs guards this).

Particles (M13): the `ParticleEmitter` component is a seeded deterministic emitter — cone
spray around the entity's local **−Z** (the camera/light aiming convention: rising smoke is
`"rotation": [90, 0, 0]`), spawn rate via a credit accumulator, per-particle world-space
acceleration/drag, and start→end interpolation of half-size, linear-RGB color, and alpha
over each particle's lifetime. Simulation is GPU-free in `engine-core/src/particles.rs`: a
private per-emitter xorshift32 RNG (fully specified in-repo so dependency upgrades can't
change sequences; splitmix-finalizer seeding, RNG *not* consumed on capped spawns), emitters
stepped in name order — same file + `--steps` → byte-identical pixels, which is what lets
smoke live under a diff-render baseline (`verify/m13_smoke.json` /
`baselines/m13_smoke.png`, blessed at `--steps 180`). Particle state is simulation state:
created only by `--steps` (never `--time`), never baked or traced (disposable like solver
caches), and a `--steps 0` render draws nothing, so pre-M13 baselines are untouched. System
order: animations → scripts → physics → **particles** → render (an emitter riding a dynamic
body trails where the body actually went). Rendering is `shaders/particles.wgsl`:
camera-facing instanced quads with a `(1−d)²` soft-disc falloff, alpha-blended
(depth-tested against meshes, depth-write off), CPU-sorted back-to-front by camera distance
with `total_cmp`. `seed`/`max_particles` are the first **integer** component fields — the
schema walk gained a first-class `"integer"` arm (a float, negative, or out-of-u32 value
where a u32 belongs is `invalid_field_type`; a below-minimum integer is
`value_out_of_range`); without it, walk/serde disagreement on these fields would fire
`scene_parse_desync`. The editor viewport shows scenes at rest — no particles until the
fixed clock advances. The car demo carries the applied version: an `Exhaust` emitter that
`car.rhai` parks at the tailpipe each step (rear bumper, offset to the car's right, from
the same `world.forward` heading the driver already computes) — particles are world-space
once spawned, so a moving car leaves a trail behind it rather than dragging a plume along.
It cost `verify/baselines/m11_lap.png` a re-bless; the timeline, physics, and the pinned
HUD strings are untouched, because particles never feed back into simulation.
`rate` is the one emitter parameter scripts drive — `world.particle_rate` /
`set_particle_rate`, the only particle field in the curated API, since the component is
re-read every step. It bakes change-based like a velocity or a gauge width, and the setter
rejects negative/NaN/f32-overflowing values **at the call** so a bad rate is a located
script error rather than a baked file that fails `validate`. Rate 0 pauses emission without
touching live particles (they live out their lifetime), which is what makes gating cheap:
`car.rhai` runs `SkidLeft`/`SkidRight` emitters at the rear contact patches off chassis
sideslip (lateral velocity, 1 m/s deadband so suspension jitter is not a skid) plus a
braking-lockup term, so the tires smoke in corners and under braking and are silent on the
straights. Those two emitters rest at `"rate": 0.0` and the parked car is not sliding, so
they cost the parked baseline nothing on their own — the circuit rebuild is what
re-blessed `m11_lap.png`. Both are emitted by `make_car_track.py` and follow the car's
*height*, like the exhaust: a contact patch pinned to a fixed altitude smokes from inside
the hill on a circuit that climbs.

Breaking (M14, design in `breaking-design.md`): `Breakable` lists **pre-authored fragments**
(mesh ref + local placement + cuboid `half_extents` + `density` — no runtime fracture, the
settled decision) and breaks three ways: collision (`impulse_threshold` in kg·m/s — rapier
contact *force* × dt at the event boundary, **peak** per step not sum, and force events are
enabled only on breakable colliders so no-Breakable scenes are byte-identical to pre-M14),
`world.break_entity(name)` (validated at call time, queued on the ScriptHost, drained by the
sim loop), and `world.explode(x,y,z,radius,impulse)` (radial impulse, linear falloff,
applied inside `step()` before integration; thresholded breakables in range break with a
kick). Breaks apply after physics in entity-name order (`engine-physics/src/breaking.rs`):
despawn parent, spawn `Parent.fragN` (suffix-deduped) as dynamic bodies inheriting
v + ω×r, then `Scene::refresh_names` + `ScriptHost::sync_names` — fragments are ordinary
entities everywhere downstream. Trace rows **re-enumerate dynamic bodies every step** (sorted,
so unchanged scenes trace identically) plus `{"step", "broke", "fragments"}` lines; bake
extends change-based to structure via `formatter::apply_remove_entity` (new) +
`apply_add_entity` with `ComponentData::collect_from` — a baked post-break scene revalidates
and re-renders **bit-exactly** (pinned by CLI test; fixture `verify/m14_break.json`, golden
trace + PNG in `verify/baselines/`). Validation: the schema walk now recurses into objects
and arrays-of-objects (open-ended `minItems` reports as `value_out_of_range`, keeping the
walk/serde agreement); fragment `mesh` refs resolve like `Mesh.asset` in both passes;
`impulse_threshold` without a `Collider` is `breakable_without_collider`. The editor inspector
routes only arrays-of-numbers to the vec3 widget. A threshold-less `Breakable` is
script/explosion-only by design.

Frame cost (M15): the viewer was slow for reasons that had **nothing to do with particles** —
measured on an M3 Pro at 2560×1440, the smoke costs ~0 ms/frame even with the camera inside the
plume, while the frame was spending ~29 ms in `hud::rasterize` and ~4 ms rebuilding GPU
resources. Three fixes, none of which moves a pixel (all five baselines re-diffed bit-exactly):
**(1) the HUD rasterizes only what it covers** — elements are measured, overlapping ones grouped,
and each group gets a canvas at its bounding box blitted under a scissor rect (`HudOverlay` /
`HudCanvas { origin_x, origin_y, .. }`, `shaders/hud.wgsl` takes the origin as a uniform);
overlapping elements still accumulate in one linear-space buffer and quantize once, so stacked
translucency is untouched. **(2) GPU resources persist across frames** — `SceneRenderer::draw`
takes `&mut self` and keeps uploaded geometry (keyed on the `Arc<MeshData>` identity, evicted
after 240 idle frames), one object-uniform buffer addressed by dynamic offset instead of a
buffer + bind group per entity, and grown-in-place frame/particle/HUD buffers. **(3)**
`MeshSource::load_mesh` returns `Arc<MeshData>` and implementations must return the *same* `Arc`
for one asset — that is both the end of the per-frame deep copy in `Scene::render_items` and the
cache key in (2); a reloaded file mints a new `Arc` and re-uploads. `particles.wgsl` also
discards fragments whose final alpha is exactly 0 (the disc's corners, fully faded particles),
which is bit-identical because `src·0 + dst·1` is `dst`. Net: ~34 ms → ~0.9 ms per frame in
release, ~173 ms → ~2.2 ms in debug. **The viewer draws an FPS readout** in the top-right
corner (`app.rs::with_fps_readout`, averaged over 0.5 s) — wall-clock and therefore viewer-only:
it rides ordinary `HudText`/`HudRect` components appended to the scene's own HUD, and headless
renders never see it, so nothing reproducible depends on how fast this machine drew.

Environment: sky, fog, shadows, MSAA, transparency (M16). Five renderer features, all reached
through **one scene-level `environment` block** (`EnvironmentSettings` in `engine-core/src/
scene.rs`, hand-validated like `physics` by `check_environment_block`, new code
`invalid_environment_value`) plus two new `Material` fields. **Every one of them defaults to off,
and that is the design, not a convenience**: eleven baselines were blessed before any of this
existed and not one had to be re-blessed — a scene with no `environment` block renders byte for
byte as it did before the block did. Fields: `sky` + `sky_zenith`/`sky_horizon`/`sky_ground`,
`fog_density`, `shadows` + `shadow_distance`, `samples` (1 or 4; anything else is a validation
error rather than a silent round). `sky_horizon` **is** the fog color — one field, so it cannot be
set inconsistently with the sky it fades into.

- **Shadows** are a single directional map (2048², `shadow.wgsl`, depth-only, no fragment stage,
  reusing the mesh pass's object+frame uniforms so casting costs no extra upload). The ortho box is
  fitted along the camera's view direction, and its center is **snapped to whole texels** — without
  that, moving the camera slides the sampling grid across the world and every shadow edge crawls,
  which reads as a bug rather than as low resolution. Casters are drawn **front-face-culled** so
  the map records each caster's far side, which is a better peeling margin than any constant bias.
  3×3 PCF over a `LessEqual` comparison sampler with linear filtering (hardware PCF, so each tap is
  already a bilinear blend of four tests), slope-scaled bias, and a fade to lit at the box edge.
  Transparent geometry does not cast. One cascade only — no crisp-near-*and*-far.
- **Sky** is a fullscreen triangle drawn first with `depth_compare: Always` and depth writes off,
  evaluated per pixel from an unprojected view ray (per-vertex would visibly bend the horizon).
  The gradient lives in `shaders/sky_common.wgsl` and is **concatenated onto both `sky.wgsl` and
  `mesh.wgsl`** at pipeline build (`with_sky_common`) — WGSL has no `#include`, and the mesh pass
  reflects this exact sky off metal and water, so a second copy of the curve would drift.
- **Reflected sky and hemispheric ambient**, both gated on `sky`. Ambient is modulated by a
  ground↔zenith lerp normalized **per channel** against the two bands' mean, so `AmbientLight`
  keeps meaning what it says and only the color *balance* tracks the normal. Normalizing against
  mean *luminance* instead is the obvious alternative and is wrong: a saturated sky then triples
  the blue channel and every up-facing surface goes blue-grey. The specular environment term uses
  **roughness-capped Schlick** (`max(1 - roughness, f0)`, not 1) — uncapped, grazing Fresnel turns
  matte ground into a sheet of sky, since a ground plane is seen at grazing incidence nearly
  everywhere.
- **MSAA** is `samples` on the scene pipelines plus a resolve; the HUD pass stays single-sampled on
  the resolved target, so glyphs are still pixel-exact. `SceneRenderer::with_samples` bakes the
  count into the pipelines, so it belongs to the renderer, not the frame.
- **Transparency** is `Material.alpha` (flat, view-independent — the "ghost this" knob) and
  `Material.transmission` (view-dependent, keeps the specular lobe, scales diffuse by
  `1 - transmission`). `Material::is_transparent` routes those into a second blended pass, sorted
  back-to-front with an entity-name tiebreak, depth-tested but not depth-writing, and the shader
  emits **premultiplied** color for them so a clear surface keeps its highlight and its sky
  reflection instead of losing them to a low alpha. No refraction and no scene-color sampling.

**The bit-exactness of the default path is load-bearing and fragile.** The four lines computing
`direct`/`ambient`/`base_color` in `mesh.wgsl` are the M4 originals, computed from immutable
bindings ahead of every M16 branch, and every new feature hangs off one combined `if`. That is
stricter than "an equivalent expression" on purpose: whether the compiler may contract `a*b + c`
into an FMA depends on the code around it, and an FMA carries more intermediate precision than the
pair it replaces. Restructuring those lines into arithmetic that is *equal on paper* moved
`m12_hud.png` by one ULP in one pixel. Leave them alone. Verified by
`engine-render/tests/environment.rs` (six GPU-skipping pixel tests: shadowing only ever darkens,
sky runs blue upward, fog grows with distance, blending shows what is behind, MSAA leaves covered
interiors exact, and an absent block equals an all-defaults one) and fixture
`verify/m16_environment.json` + baseline, pinned by a CLI test.

Fire and point lights (M17, design in `fire-and-lights-design.md`). Two halves of one problem: the
tour's campfire was a cone of identical orange sprites, and its own doc named script-driven lights
as the upgrade that would move the scene most.

**`ParticleEmitter` gains five fire fields**, each fixing one reason a particle cone does not read
as flame: `blend: "additive"` (overlapping flame *brightens* — alpha blending can only render
orange smoke), `radius` (a disc of coals instead of a single apex), `speed_jitter`/`size_jitter`/
`lifetime_jitter` (a population born identical dies at one height, drawing a flat top on the
flame), `turbulence`+`turbulence_scale` (hot gas curls), and `stretch` (a moving ember covers more
than its width during an exposure). **Every default is the M13 behaviour, down to which random
numbers the emitter draws**: the draw order is a format contract — direction → disc → speed → size
→ lifetime → turbulence — and each step is *skipped*, not defaulted, when its field is zero, since
a defaulted draw would shift every subsequent one and move every particle baseline
(`defaulted_fire_fields_consume_no_randomness` pins it by construction). Turbulence is smooth value
noise sampled along each particle's own path plus a per-particle offset drawn at birth — smooth
because per-step randomness makes a particle *vibrate* rather than arc, per-particle because
otherwise every particle follows one shared braid; the integer hash is spelled out in-repo like the
xorshift, so no dependency upgrade can change what a scene looks like. A particle's lifespan and
size scale are now fixed **at birth**, so retiming an emitter mid-run affects new particles only.
Additive is a **second pipeline** over the same shader and instance buffer (the sorted list is
stable-partitioned on the CPU), *not* one premultiplied pipeline — that alternative works and saves
a pipeline object, but it moves the multiply by alpha into the shader for every particle including
the ones under existing baselines. Additive sprites draw after *all* alpha ones regardless of
depth, so a flame glows through the smoke above it, which is what firelight scattering in that
smoke looks like. `stretch` is in **seconds** of travel and elongates along the velocity's
*screen-space* projection, so a particle flying at the camera stays round.

**`PointLight`** is a local light — position only, no orientation, many per scene up to
`MAX_POINT_LIGHTS` (8, beyond which `too_many_point_lights` rather than a light that silently never
shines). Inverse-square falloff windowed by `(1 − (d/r)⁴)²`: the window is what makes a light
*local*, and past `range` the contribution is byte-identical to no light at all — without a hard
horizon a lantern in one room lifts the black level of the next. `intensity` is brightness at one
unit of distance. No shadows (the engine has one shadow map and it belongs to the sun). Lights are
ordered by entity name, since the uniform array is fixed-size and an index must not depend on
archetype iteration; a `PointLight` counts as lighting the scene, so a campfire-only scene stays
dark outside its firelight. Contributions are **added to the finished color** on their own branch
after every M16 feature — firelight is *extra* light, and
`a_point_light_is_extra_light_not_replacement_light` walks every pixel of a sunlit scene to prove
adding a lamp never darkens one. Scripts reach any light by name through
`world.light_intensity`/`set_light_intensity`/`light_color`/`set_light_color` (all three light
components — the fields mean the same thing on each); intensity errors on negative/NaN/overflow at
the call, color *clamps* to `[0, 1]`, and both bake change-based for all three kinds.

**Two places here are deliberately more repetitive than they look.** `evaluate_point_light` in
`mesh.wgsl` re-derives the GGX terms instead of sharing a function with the sun path, and
`particles.wgsl` writes the un-stretched quad expansion out twice rather than lerping. Both are
guarding the M16 ULP sensitivity: factoring them would rewrite the four lines M16 declares
untouchable, and "equal on paper" is not enough when FMA contraction depends on surrounding code.
The check that actually settles a bit-exactness question is an **A/B between binaries**, not a
diff against a baseline: build the CLI at `main` and in the worktree, render the same scenes with
both, `cmp` the PNGs. Doing that here found a 1-pixel `m14_break.png` diff that turned out to be
**pre-existing drift on `main`** — all 19 scene/step combinations were byte-identical between the
two builds. Fixture `verify/m17_fire.json` + `scripts/m17_fire.rhai` + baseline at `--steps 240`,
pinned by a CLI test that also bakes and revalidates the run.

Trees (M19, design in `tree-design.md`). The `Tree` component is a **recipe, not a mesh
reference**: `engine-core/src/tree.rs` grows it into two meshes — bark (drawn with the entity's own
`Material`) and leaves (drawn with `Tree::leaf_material`, from `leaf_color`/`leaf_roughness`) — so
one entity emits two `RenderItem`s under one name, and `unused_material` knows a tree's Material is
its bark. A branch is a polyline that wanders: each of `segments` steps adds a random `crook` and
a tube of `sides` faces is swept along it, tapering on a **power curve** (`t^1.6`, since linear
draws a carrot), with a quadratic root flare over the bottom fifth of the trunk. Children attach
past `branch_start`, spun by `branch_twist` per point (137.5°, the golden angle — a whole-number
division stacks branches into visible rows) and started 70% inside the parent's radius so the
tubes interpenetrate instead of needing a union. Ring orientation is carried by **parallel
transport**; rebuilding a perpendicular from a world axis spins the tube wherever a branch aligns
with it. Leaves are `blade` (a midrib with two wings folded down, emitted twice for both faces —
the fold is what gives a canopy texture when the engine has no leaf textures), `cluster` (a
stretched octahedron, for conifer sprays), or `none`. **Three model rules came out of looking at
renders, and all three are now multi-seed tests**: `whorl` applies to the **trunk only** (compounding
it is quadratic — a plausible spruce hit 175,898 vertices — and botanically wrong); `tropism`
applies at **depth > 0 only** and clamps against overshoot (at depth 0 it is unstable: a degree of
crook gives a negative tropism something to amplify, and the first pine grew sideways); and the
trunk gives back 30% of its accumulated lean every segment, because **a random walk with nothing
pulling on it drifts** and which seeds toppled was pure luck. Determinism is the M13 discipline
again — one private xorshift written out in-repo so no dependency upgrade can reshape a forest —
except that jitter helpers always consume a draw even at `jitter: 0`, since no tree baseline
predates any tree field. `tree::vertex_count` is exact (not an estimate) and validation refuses
anything over `MAX_TREE_VERTICES` (100k) with `tree_too_complex` before allocating, because a hung
render with no output is the worst failure an agent loop can hit. `meshes_for` caches on the
component's **exact field bits** (26 words, compared not hashed) and must return the same `Arc` —
M15's upload cache keys on `Arc` identity. There is no species enum: a species is a set of
parameters, tabulated in the design doc. Fixture `verify/m19_trees.json` + baseline (twins
differing only in `seed`, plus a zero-randomness `Diagram` tree), pinned by a CLI test that also
pins `tree_with_mesh` and `tree_too_complex`. **Tree baselines are per build profile as well as
per adapter** — a release build's `sin_cos` routines move 3 pixels of `m19_trees.png` by one
channel step (measured; Rust does not contract floats, so this is libm, not FMA), so bless from
the debug binary `cargo test` runs. Every pre-tree fixture is profile-insensitive; the constraint
arrives with CPU-generated geometry.

Water (M18, design in `water-design.md`). The tour's pond was sixteen `builtin:cube` tiles a script
bobbed on a shared sine: every tile translated rigidly, so the surface normal was straight up
everywhere at every moment, there was nothing for the sun or sky to catch, the seams showed, and
"deep" and "shallow" did not exist. The `Water` component replaces all of that.

**A body of water is one entity with one component.** `Water` owns its surface geometry — a
tessellated unit grid (`segments`, 1..512, identical to `builtin:plane` at `segments: 1`, generated
and `Arc`-cached in `engine-core/src/water.rs`) sized by `Transform.scale` — so the entity carries
**no** `Mesh` and **no** `Material`, and having either is `water_with_mesh`. Waves are evaluated in
**world space**, so scaling never stretches them and two water entities at the same height form one
continuous surface. `Scene::water_items` returns name-sorted `WaterItem`s and needs no `MeshSource`.

- **Gerstner waves, displaced in the vertex stage** (`shaders/water.wgsl`), with normals from the
  analytic derivatives of the same sum. CPU displacement was never close: a 192² grid is 37k
  vertices and would mint a new `Arc<MeshData>` every frame, defeating M15's geometry cache, which
  keys on that `Arc`'s identity. `Q` is packed as `steepness / (k · A)`, which makes each wave's
  contribution to the horizontal Jacobian equal to its own `steepness` — so **sum of steepness ≤ 1
  is exactly the non-folding condition**, enforced as `water_waves_self_intersect` with the
  arithmetic in the message. Dividing `Q` by the wave count (as most references do) would make the
  same file calmer as waves were added and leave the rule meaning nothing.
- **Detail is a slope field with no height behind it**: four golden-angle-rotated sine trains at
  deep-water dispersion speeds, perturbing the normal only. Two numbers in it were found by looking
  at renders and are load-bearing — the base amplitude (`0.010 · wavelength`; the first attempt was
  ~4× steeper and rendered white noise, since the layers are in phase *somewhere* and a slope field
  is a shaken mirror) and the **fade with view distance**, without which sub-pixel ripples alias
  into sparkle that reads as broken rather than as low quality. Nothing physical may depend on these
  normals, which is what licenses a slope field with no surface under it.
- **The frame gains a pass, but only when there is water.** Absorption and shore foam need the depth
  behind the surface and a pass cannot sample its own depth attachment, so a water scene renders as
  opaque (depth stored) → depth copy (`shaders/depth_resolve.wgsl`, one fullscreen triangle into a
  single-sampled `R32Float`, `textureLoad`, sample 0 under MSAA) → water and transparency →
  particles. `water_present` gates the split: with no water the pass structure, attachments, and
  load/store ops are the exact pre-M18 ones. Water sorts into the **same** back-to-front `Blended`
  list as transparent meshes, because an ice floe in a pond is transparent geometry *inside* a
  water surface and two passes would fix which always draws over the other.
- **The clock.** Water is a pure function of (file, `time`), and `time` is the reproducible clock:
  `--time T` when given, otherwise `steps / timestep_hz` (`scene_time` in the CLI); the viewer uses
  whole fixed steps since load. That is what lets water sit under a `diff-render` baseline, and the
  M18 CLI test pins it from both directions (`--steps 120` at 60 Hz and `--time 2.0` are the same
  bytes; a run with neither must *not* match).
- **`mesh.wgsl` is untouched.** `water.wgsl` duplicates `FrameUniform` and the shadow lookup rather
  than sharing them, following the `sky.wgsl` precedent, because of the M16 ULP sensitivity above;
  only `sky_common.wgsl` is shared, prepended at pipeline build, so the reflected sky cannot drift
  from the sky drawn behind it. The body is lit with the **up** normal while the view-facing normal
  drives reflection, Fresnel, and specular — conflating them made water black from below (caught by
  `a_surface_is_visible_from_underneath`).

Not here, deliberately: refraction (what is behind is absorbed and tinted, never bent), scene
reflections (sky and sun only), a CPU wave evaluator and therefore no buoyancy (`water.rs` is where
the Rust mirror goes, with an agreement test, when a boat needs to float), and point lights on
water. Verified by `engine-render/tests/water.rs` (six GPU-skipping pixel tests, including
`a_scene_with_no_water_is_untouched_by_the_water_pass`) and fixture `verify/m18_water.json` +
baseline at `--steps 120`, pinned by a CLI test. The bit-exactness of the seventeen earlier
baselines was checked the way this repo has learned to check it — an A/B between binaries built at
`main` and here, fifteen scene/step combinations, all byte-identical.

Terrain (M22, design in `terrain-design.md`). Ground was `builtin:plane` scaled to 200 m with one
albedo — two triangles and one colour, so the sun struck all 40 000 m² at the same angle and the eye
read it as a backdrop rather than a place. The `Terrain` component replaces it, and **there is no
flat ground in the repo's scenes any more**.

**A patch of ground is one entity with one component**, following `Water` exactly: `Terrain` owns a
tessellated unit grid (`segments`, 1..512) sized by `Transform.scale`, so the entity carries **no**
`Mesh` and **no** `Material` (`terrain_with_mesh`). Heights are sampled in **world** XZ, so two
patches sharing a description meet seamlessly; `Transform.scale.y` multiplies the relief.

- **The height field is CPU-side — the opposite of water's choice, for three reasons.** Terrain does
  not animate, so the argument that forced waves onto the GPU (a new `Arc<MeshData>` every frame,
  defeating M15's geometry cache) does not apply; physics has to stand on it (a `trimesh` `Collider`
  with no `asset` and no `Mesh` borrows the generated surface, which is how ground is collidable
  without a mesh file); and placement has to query it. So there is exactly one implementation and
  **nothing to keep in agreement**, which is strictly better than water's position. fBm value noise
  with the integer hash spelled out in-repo like M17's turbulence (a terrain render sits under a
  baseline, so the hash is a format contract), `warp` domain-warping it into ridges and valleys, and
  the octave sum normalised so `height` means metres however many octaves are summed.
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
  boundary out of an iso-line into interlocking fingers, `color_variation` mottles the result at two
  scales an order of magnitude apart, and `bump` perturbs the normal per pixel with no displacement
  behind it, fading with view distance (water's specular-aliasing lesson). Nothing physical depends
  on any of the texture noise — the collider is the displaced grid and nothing else.
- **`mesh.wgsl` is not edited.** Terrain is lit exactly like a mesh, so it shares the file rather
  than duplicating 200 lines of GGX/PCF/ambient/point-light/fog that would drift; but M16's four
  untouchable lines must reach the compiler surrounded by the code they shipped in. Putting the
  branch inline — those lines textually identical, only `albedo`/`roughness` arriving from a function
  result — **moved one pixel by one unit in each of `m16_environment`, `m17_fire` and `m18_water`**,
  found by the A/B between binaries. So the plain pipeline compiles the file as it sits on disk
  (byte-identical by construction) and a second `terrain-pipeline` compiles a variant assembled by
  `with_terrain`: `shaders/terrain.wgsl` inserted plus two **anchored** substitutions that panic at
  startup if `mesh.wgsl` is reworded. Terrain draws in one run at the end of the opaque pass, so the
  pipeline switches once a frame.
- **`world.terrain_height(name, x, z)`** is the only terrain call in the script API — read-only,
  returning a world Y a script can assign directly. It is what keeps the tour's critters on the
  ground and the truck's exhaust and skid smoke from firing out of a hillside. **Terrain's shape
  fields are deliberately not animatable** (`NOT_ANIMATABLE` in `animation.rs`): a clip driving
  `height` would regenerate the surface every frame and leave hundreds of megabytes of vertex
  buffers in the renderer's cache, so a clip aimed there fails validation with `unknown_property`
  rather than doing it quietly. Appearance fields animate freely.

Verified by `engine-render/tests/terrain.rs` (six GPU-skipping pixel tests, including
`a_flat_single_layer_patch_is_exactly_a_painted_plane`, which pins the shading path against
`builtin:plane` at `segments: 1`) and fixture `verify/m22_terrain.json` + baseline at `--steps 120`,
pinned by a CLI test that also proves the dropped sphere lands *on* the ground. The eighteen earlier
scene/step combinations were A/B'd against `main` and are byte-identical.
Clouds (M20, design in `cloud-design.md`). The `Cloud` component is M19's premise applied to the
sky: a **recipe, not a mesh reference**. `engine-core/src/cloud.rs` grows it into one mesh — a
golden-angle spiral of icosphere lobes over the footprint, each growing `children` smaller lobes
biased upward by `rise`, buried 45% of their own radius so the surfaces interpenetrate (M19's join,
for M19's reason), radially displaced by `wobble`, and folded onto a base plane by `flatten`. The
entity carries **no `Mesh` and no `Material`** (`cloud_with_mesh`) and is sized by `Transform.scale`
like a water surface; non-uniform scale is the normal case, which is what oblates the lobes.
Determinism, the exact `vertex_count` + `cloud_too_complex` budget (100k), and `Arc`-identity
caching are M19's, transposed — except that the cache key covers the **eleven geometry fields
only**, since colour and density are uniforms that cannot move a vertex. **Cloud baselines are per
build profile as well as per adapter**, like tree baselines: bless from the debug binary.

Rendering is `shaders/clouds.wgsl`, a new pipeline (not a `Material` branch) duplicating
`FrameUniform` and the fog term rather than touching `mesh.wgsl`, with `sky_common.wgsl` prepended
so a cloud's underside is lit by the sky drawn behind it. Clouds join the existing back-to-front
`Blended` list beside water and transparent meshes, depth-tested but **not** depth-writing, so
overlapping lobes accumulate alpha as a stand-in for optical depth; culling is **off** for this
pipeline alone, because a cloud has no inside and would vanish the moment a camera entered one.
`drift` (m/s) is applied in the **vertex stage** from `ScenePass.time` — not folded into the model
matrix — which is what keeps `Scene::cloud_items` a pure function of the file and the grown mesh's
`Arc` stable across frames; the shape never evolves with time, for the reason trees have no wind.
No shadows cast (one cascade, fitted to the camera, does not reach a cloud at altitude), no point
lights, no volumetrics.

**Four things the renders changed, and all four are easy to reintroduce by "simplifying" it:**
vertex normals are bent **55% from each lobe's centre toward the cloud's** (`BODY_NORMAL`), without
which every lobe draws its own terminator and the cluster reads as a bag of marbles; the height
profile's rise is capped at 0.8 lobe *diameters* (`DOME_STACK`), without which the middle lobe
floats clear of the ring around it — the consequence being that how far a cloud fills a tall box is
set by `lobe_size`, not by stretching a fixed lobe count to reach; alpha is
`density · (1 - (1 - facing)^feather)` and **not** `facing^feather`, since the proportional form
turns a cloud seen from below translucent all over (this inverted `feather`'s sense: higher is now
*crisper*); and the sun reaches the shadowed side at a `THROUGH_SCATTER` fraction (0.3) with the
diffuse curve left **linear**, because applying it in full saturates a white cloud everywhere and
sharpening the curve instead — the obvious fix — turns a storm cloud into grey rock. Fixture
`verify/m20_clouds.json` + baseline at `--steps 120`, pinned by a CLI test that also pins
`cloud_with_mesh` and `cloud_too_complex`; seven GPU-skipping pixel tests in
`engine-render/tests/clouds.rs`. The no-pixel-moved claim was settled the usual way — an A/B
between binaries, twenty scene/step combinations against `main` including all five hours of the
M21 daylight fixture, all byte-identical.

Day and night (M21, design in `daylight-design.md`). Every scene was pinned at whatever hour its
author typed, and moving one to dusk meant hand-editing six colors and keeping them consistent.
`daylight` makes them one number.

**It is a pure CPU function, and that is the whole design.** `engine-core/src/daylight.rs` maps
`(DaylightSettings, time) -> Daylight`, and `scene::apply_daylight` folds that onto the
`ResolvedLights` and `EnvironmentSettings` the renderer was going to receive anyway. **No WGSL
changed, no new uniform, no new pass — `SceneRenderer::draw` takes exactly the types it took
before.** So M16's untouchable four lines in `mesh.wgsl` cannot be tripped, the whole system is
GPU-free and unconditionally testable like `particles.rs` and `tree.rs`, and everything downstream
tracks for free: shadows follow the sun because they always followed the `DirectionalLight`, fog
recolors at sunset because fog *is* `sky_horizon`, and water reflects a dusk sky because
`water.wgsl` already reflects whatever the sky uniform says.

`daylight` is a **top-level sibling of `physics` and `environment`**, not a field inside it — it is
clock-driven and it *produces* environment values, and a `Vec` palette inside `EnvironmentSettings`
would cost that type its `Copy` and put a clone in the per-frame path. It rides the same clock water
does (`--time T`, else `steps / timestep_hz`), and **`day_length: 0` (the default) freezes the day**:
most scenes want a dial, not motion, and a frozen day is reproducible with no `--time` at all.
`day_length: 24.0` makes an hour a second, which is what the fixture uses.

- **The arc** is artistic with a physical shape: `sun_elevation` (noon altitude) and `sun_azimuth`
  (noon bearing) replace latitude, date, and axial tilt. **Sunrise is 06:00 and sunset 18:00 at
  every elevation** — refusing to move them with the season is what makes an 18:00 keyframe *the*
  sunset keyframe in every scene. Derived once so it is not rediscovered: a noon sun toward −Z makes
  −Z south, so the sun **rises toward −X and sets toward +X**.
- **The moon** rides the same arc twelve hours out of phase with its own elevation, color, and
  intensity. There is still one directional light: **it *is* the dominant body**, direction, color
  and intensity, with no summing. The bodies swap where their luminances are equal, so brightness is
  continuous by construction and only hue and direction shift — at the moment there is least light
  to notice it by. Summing instead would send an orange sunset from the moon's side of the sky for
  all of twilight; crossfading the direction would aim the light where neither body is. The
  handoff's invisibility is a **property of the palette**, and a test walks the day at one-minute
  resolution asserting exactly two swaps, each under 0.08 luminance — brighten the dusk keyframes
  and it fails, correctly.
- **The palette** is eight keyframes, all nine fields required (a half-specified keyframe fading to
  black is worse than an error), interpolated linearly in linear RGB and **wrapping across
  midnight**. **The noon keyframe is exactly the M16 clear-day defaults**, so the model and every
  hand-authored scene agree at the one hour anyone can check. Sun intensity lives in the table
  rather than falling out of `sin(altitude)` because a sunset's redness and its dimness are one
  decision. **Fog is a `fog_scale` multiplier on the authored `fog_density`**, never an absolute —
  a scene with `fog_density: 0` stays clear all day, and a daylight block can never switch fog on in
  a scene that never asked for any.
- **Ownership.** `drives_sun` (default on) synthesizes the sun, and an authored `DirectionalLight`
  beside it is `daylight_and_directional_light` — two owners of one sun is what invariant 8 exists
  to prevent. `drives_sky` (default on) computes the three bands **and the ambient** (ambient *is*
  the sky's contribution, which is why M16 gates hemispheric ambient on `sky`); authoring either
  anyway is the `daylight_overrides_sky` warning, `unused_material`-style, naming the fix.
- **The horizon-sun shadow bug**, which day/night is the first thing to reach: a sun on the horizon
  casts shadows of unbounded length and one just below it casts them *upward*. `clamp_shadow_
  elevation` in `scene_renderer.rs` floors the direction used for the **shadow matrix** at 5° while
  the lighting direction keeps going. Above 5° it returns its input unchanged, which is why it costs
  every pre-M21 baseline nothing (measured: every shadow-casting fixture aims its sun 24°–33° up).
- **Scripts** get exactly two read-only getters, `world.time_of_day()` and `world.sun_altitude()`,
  evaluated once per step from `step * dt`. Turning them into light is the M17 API unchanged — which
  is why there is no `auto_on` field on `PointLight`. There is deliberately **no setter**: a
  script-settable clock is hidden state (invariant 2), so "sleep until dawn" is named as not-done
  rather than smuggled in. Asking a scene with no `daylight` block for the time is a runtime error,
  not a plausible noon.

Fixture `verify/m21_daylight.json` + `scripts/m21_lamp.rhai` and **five baselines from one file** at
`--steps 120/390/720/1110/1320` (02:00, 06:30, noon, 18:30, 22:00), pinned by a CLI test that also
asserts 02:00 does *not* match the noon baseline. `--steps` and not `--time` because the lamp is
script-driven and scripts run on the step loop. Bless from the **debug** binary (M19's rule — the
fixture has trees). Not here: a sun disc or a directional horizon glow (so a sunset reddens the whole
sky evenly — the natural next commit, in `sky_common.wgsl` on its own branch after the untouchable
lines), stars, clouds, real astronomy, moon shadows, and script-driven `Material.emissive` — which
the fixture's lamp globe wanted and is a gap this milestone surfaced rather than created.
**The A/B settled the no-pixel-moved claim**: merge-base binary vs the worktree's, 15 scenes × 5
step counts = 75 combinations, all byte-identical.

Roads (M23, design in `road-design.md`). The car demo's circuit was **207 `builtin:cube`
plates**: a deep earth box per segment whose top face was the drivable surface, a thin
colliderless asphalt slab 9 cm proud of it so the road read as road, kerb cubes on the tight
corners, and one painted start line. Consecutive rectangles cannot tile a curve, so every corner
joint left a wedge of verge showing through — papered over by overlapping the slabs 1.1 m and
lifting them clear of their neighbours (`ASPHALT_OVERLAP`, `SKIN`, both constants that existed
only to hide the fact that the road was not a surface). The plates had to overlap 35 cm so a
suspension ray could not fall through the crack between two of them. The `Road` component
replaces all of it with one entity.

**A road is one entity with one component.** `Road` owns its geometry, so the entity carries
**no** `Mesh` and **no** `Material` (`road_with_mesh`) — water's rule, one milestone later. The
centerline is authored as a **polygon with corner radii** (`points`, each a `position` and a
`radius`), not a spline: a closed polygon returns to its own first vertex and its exterior angles
sum to one turn, so position *and* heading close without solving anything, and nothing in the file
carries a heading. `radius: 0` is a sharp vertex, mitred with the standard `1 / cos(turn/2)`
widening; past `MAX_SHARP_TURN_DEGREES` the mitre folds and validation says so
(`road_corner_needs_radius`), as it does when two arcs need more of the edge between them than it
has (`road_corner_does_not_fit`). Elevation rides on the points and is interpolated by arc length
with a **monotone cubic** (Fritsch–Carlson), not linearly and not Catmull-Rom: linear ramps break
the grade at every corner — a bump the car feels exactly where it is loaded up — and plain
Catmull-Rom smooths that but overshoots, so a road authored to reach 6 m crests at 6.4 and the
file stops predicting the scene.

- **One collider, and it is the whole ribbon.** Asphalt, shoulders and the embankment skirt are
  the same triangles, so the ledge that stopped the car dead on the plate road is now structurally
  impossible rather than merely remembered. A `Collider` with `"shape": "trimesh"` on a road entity
  needs no `asset` and no `Mesh` — the road *is* the geometry — while friction and layers stay on
  the `Collider`, where every other surface keeps them.
- **`FIX_INTERNAL_EDGES` on a road's own trimesh, and only there.** Without it a body resting on a
  triangle mesh eventually contacts an edge *between* two coplanar triangles, takes a contact normal
  along that edge instead of off the surface, and is flung sideways: a ball parked on the M23
  fixture sat still for two seconds and then left the road at 4.8 m/s. Scoping it to road-generated
  geometry is deliberate — switching it on for *every* trimesh moves `verify/baselines/
  m22_terrain.png` by 1339 pixels, because the ball in that fixture then rests ~20 cm away, and a
  milestone about roads has no business re-blessing terrain's baseline. **Terrain has the same
  latent bug** and should take the same flag as its own change, with its own re-blessed baseline;
  that is a finding of this milestone, not a decision it was entitled to make.
- **Markings are drawn, not built.** Every marking — edge lines, centre line, kerbs, start line —
  is computed per pixel in `shaders/road.wgsl` from two surface coordinates the vertex stage
  carries in the mesh's UVs (which the renderer had never uploaded before and now does for every
  mesh): `u`, signed metres from the centerline *along the cross-section*, so `|u| > width/2 +
  shoulder` is exactly "on the skirt"; and `v`, metres along the centerline. A line is a band in
  `u`, so it follows every curve and grade for free; a dash is periodic in `v`, so it is the same
  length in metres through a hairpin as on a straight. Paint cannot z-fight, because it is not a
  surface on a surface — it is the same pixel shaded differently. Anti-aliasing is a **clamped**
  `fwidth`; unclamped, a road seen at a grazing angle from 200 m dissolves into grey.
- **Two things the CPU decides**, because per-pixel code cannot: **kerb spans** (which corners are
  under `markings.kerb_max_radius`, and which side is the *inside* of the turn) ride in a
  fixed-size uniform array, `MAX_ROAD_KERBS` of them, beyond which `too_many_road_kerbs` rather
  than a silently dropped kerb; and **period fitting** — on a closed road the dash period is
  snapped to `total / round(total / period)` and each kerb's stripe to its span, so patterns close
  on themselves instead of leaving a sliver at the seam. Kerbs are *painted*, not raised: a real
  kerb is a step, and a step is the thing the whole design says must not exist on the drivable
  surface.
- **`markings.start_line_at`** places the start line by arc length rather than at `v = 0`. The
  obvious alternative — split the straight with a radius-0 point so the road *begins* at the line —
  fails on the demo circuit and taught the rule: La Source is a 110° turn on a 14 m radius, its arc
  reaches 20 m back down a 34 m straight, and a sharp vertex 19 m along would sit inside it. The
  road refuses that, correctly.
- **`road.wgsl` duplicates `mesh.wgsl`'s lighting**, following the `water.wgsl` precedent and for
  the same reason: M16's four pinned lines cannot be shared without editing them. Only
  `sky_common.wgsl` is prepended, so a reflected sky cannot drift from the sky drawn behind it.
- **`engine road-centerline`** publishes the samples the ribbon was built from — world position,
  heading and `v` per point. Anything placed *along* a road needs them, and a generator
  re-deriving them is how two implementations of one curve start disagreeing about where the road
  is. `make_car_track.py` is the worked example: it writes the road, asks the engine where it went,
  and writes the scene again with the guardrail and the car on it.

A scene with no `Road` uploads no road uniforms and issues the draws it always did, and the A/B
proves it: every manifest entry rendered by a binary built at `main` and by this one, **20 of 20
comparable artifacts byte-identical** (the other eight are the three scenes this milestone edited
to use a `Road`, which the older binary cannot load at all — the input changed, so they say nothing
about the engine). Verified by
`engine-render/tests/road.rs` (seven GPU-skipping pixel tests, including that paint lands where
`u` says, that a dash is not a solid line, that kerbs appear only on corners that asked and only
on the inside, and that a road-less scene is untouched) plus `engine-core`'s geometry tests, and
fixture `verify/m23_road.json` + baseline at `--steps 180`, pinned by a CLI test that also drops a
ball on the road and requires it to *stay where it lands*.

Showcase tour (`showcase-tour.md`): `examples/scenes/showcase_tour.json` is a 15-second (900-step)
camera move through five 180-step stations — forest / campfire / water+ice / breaking / wide —
with every system running at once, plus four scripts (`scripts/tour_{director,wildlife,effects,
truck}.rhai`) and six 640×360 baselines (`verify/baselines/showcase_*.png`, per-adapter, checked
by hand with `diff-render`, not by a CLI test). **Its growth contract is a test**:
`repo_contracts.rs::showcase_tour_uses_every_component_the_engine_has` fails on any schema
component the tour does not use, so a new component's commit adds an entity here — there is no
allowlist, deliberately. M21 added `showcase_tour_uses_every_scene_block_the_format_has` beside it,
since `daylight` is a block and the component walk could never have seen it missing; M21 also put
the first hole in the component contract's premise, because `drives_sun` makes `DirectionalLight` a
validation error and two components stopped being addable. That exemption is *computed* from the
validation rule, not listed, so it evaporates if the tour stops driving the sun. Station 04 fires all three `Breakable` triggers in one run (collision at
~585, `break_entity` at 601, `explode` at 636). What is real: particles, physics, fragments, the ice
(real `Material.transmission`, not an opaque stand-in), since M17 the campfire — layered additive
flame, turbulent smoke, streaked embers, and a `PointLight` the tour's effects script flickers off
the same signal that drives the emission rates — since M18 the pond, one `Water` entity where
sixteen script-bobbed cube tiles used to be (the wave loop is gone from `tour_effects.rhai`, and the
`PondBed` under it was retuned at the same time: absorption has nothing to reveal over a black bed)
— and since M19 the forest, nine `Tree` components (two oaks, a birch, three spruces, a snag, two
scrubs) where twelve cylinder-and-sphere entities used to be, which cost all six showcase baselines
a re-bless and no other baseline anything. M20 adds four `Cloud` entities and re-blessed the same
six: the tour's cameras are all ground-level and aimed *down* at their subjects, so the clouds ride
the horizon rather than filling the sky — visible at stations 02 and 03 and absent from 01. A
sky-facing beat, or the sky-dome cloud layer of `cloud-design.md` §9, is what would change that. What is still faked and named as such in the doc:
animals (scaled spheres on parametric loops) and the sky (a gradient, not scattering). The blast at
station 04 still emits no light, which is now a wiring job rather than a missing feature.
Refraction is the upgrade that would move this scene most now, and water is its loudest customer;
textured bark and alpha-cut leaves are what would move the forest most, and both need the same
missing feature.
**Building it found a physics bug** now fixed and regression-tested: priming the broad-phase BVH
before the first step (vehicle worlds did this so wheel rays hit ground on step 0) consumed the
pair events, and rapier's `NarrowPhase::register_pairs` is private — so every collider **already
resting in contact at load** silently lost its contacts and fell through the world forever. Bodies
*dropped* from a height were unaffected, which is why every earlier fixture missed it. The
first-step BVH now goes on a scratch clone (`bvh_cold` in engine-physics); `refresh_queries` is
documented destructive, with the `--steps 0` query path its only safe caller.

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

**Run the CLI as `bin/engine`, not `cargo run -p engine-cli --`.** The shim checks whether any
source is newer than the binary (a find, ~0.02s warm), rebuilds only then, and execs; cargo's freshness
walk over this workspace costs ~8s *warm* on every call, which is the difference between a loop
worth running and one worth avoiding. Arguments pass through untouched and stdout stays clean, so
`docs/cli-contract.md` describes the shim exactly as it describes the binary; a rebuild that fails
comes back as one `cargo_error` line on stderr, exit 2. Default profile is **debug**, matching how
baselines are blessed. `ENGINE_PROFILE=release bin/engine …` when you want speed over comparability.

**`bin/verify-baselines` is "look at the PNGs" as one command.** Every committed baseline is listed
in `examples/scenes/verify/baselines.json` with the scene and flags that reproduce it — a mapping
that previously lived only in prose here, in `milestone-verification-scenes.md`, and hardcoded in
`cli.rs`, so the full sweep and the A/B check were reassembled by hand each time.
`repo_contracts.rs::every_committed_baseline_is_listed_in_the_manifest` fails on any baseline
missing from it. NDJSON out, exit 1 on drift, `--filter` to scope, `--bless` to re-bless (from the
debug binary), `--diff-dir` to write diff PNGs, and `--render-to DIR` + `ENGINE=<other binary>` to
run the A/B bit-exactness check as a loop rather than a reconstruction. Both golden traces are
checked too, GPU-free. Cataloguing this surfaced that **15 of the 26 baselines are pinned by no
test at all** (`m4_lighting`, both `m8_drop`, `m9_t025`, both `m10`, `m11_lap`, `m13_smoke`,
`m14_break`, and all six `showcase_*`) — the sweep is their only check, and its first run found
`m14_break.png` still carrying the 1-pixel drift M17 recorded as pre-existing. `m11_lap.png` is
the one to be careful about when reading older notes: the lap CLI test pins the *drive* — the
positions, the elevation, the parked HUD strings — and names the PNG in a comment, but nothing
diff-renders it. The three repeated rituals are
skills in `.claude/skills/`: `verify-baselines`, `ab-check`, `milestone`.

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
GPU skinning, the M5-era deferrals (--fix, watch mode), and — after M16/M17/M18/M19/M20 — refraction
and scene-color sampling for transmissive materials (water's loudest missing feature), planar
reflections, shadow cascades (which is also what cloud shadows need), shadows from point lights,
spot lights, a CPU wave evaluator and buoyancy, a light on the tour's explosion (~~lights a script
can drive~~ landed in M17), a sky-dome cloud layer for cirrus and overcast, and texture-mapped
materials — bark and alpha-cut leaves are the same missing feature — plus tree LOD and wind. After
M23: road junctions (two roads crossing wants a patch primitive, not a ribbon), banked
cross-sections, per-point road width for a pit lane merging out of a straight, roads that follow a
`Terrain` instead of carrying their own heights, and textures for asphalt grain (analytic markings
beat a texture for anything periodic, but grain is not periodic).
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
