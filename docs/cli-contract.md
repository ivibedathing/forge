# The CLI contract

How the `engine` binary talks to whoever runs it. The contract is: **an agent
can operate every command with `jq` and `$?` alone.**

Run it as `bin/engine` — a shim that rebuilds only when a source is newer than
the binary and then execs it, passing arguments through untouched and writing
nothing to stdout. Everything below therefore describes the shim exactly as it
describes the binary. The one addition it makes is at the boundary: a rebuild
that fails to compile is reported as a single `cargo_error` object on stderr
carrying rustc's diagnostic, exit 2 — the invocation-or-environment class,
because the tool itself could not be built, which says nothing about the scene
you asked it to render. `engine build` is where a compile error becomes proper
per-diagnostic NDJSON with file, line, and machine-applicable fixes. Everything below is
pinned by the integration suite in `crates/engine-cli/tests/cli.rs` and the
golden snapshot in `crates/engine-core/tests/validation_corpus.rs`; a change
that breaks a rule here fails the build before it ships.

## Streams

- **stdout** carries exactly one JSON object on success — the command's result
  — and **nothing on failure**.
- **stderr** carries NDJSON: one complete error/warning object per line,
  nothing else, ever. Even an internal panic speaks this protocol (the panic
  hook re-renders it as `internal_panic`, backtrace escaped inside the JSON
  string when `RUST_BACKTRACE` is set).
- `--help`, `--version`, and `engine agent-guide` are documentation, not
  results, and are exempt: human-readable prose, exit 0. `agent-guide` prints
  the markdown orientation an agent needs to work here, which is the one thing
  a caller wants unwrapped — a JSON string holding a 200-line document with
  every newline escaped serves nobody.

## Exit codes

Three, chosen so an agent can branch without parsing:

| Code | Meaning | Examples |
|---|---|---|
| 0 | success (warnings allowed) | valid scene, PNG written |
| 1 | **the input files are at fault** | validation errors, `compile_error`, `scene_unreadable` |
| 2 | **the invocation or environment is at fault** | no GPU adapter, `cargo` missing, bad CLI flags, internal panic |

1 means "edit your files and retry"; 2 means "fix your command or your
machine." Each error code's class is declared in the registry
(`engine-core/src/codes.rs`, human copy `docs/error-codes.md`), so the mapping
is data, not judgment calls scattered through the code.

## The wire format

Every stderr line is one JSON object. `error` is the stable machine-readable
code — parse that, never `message`. All other fields are optional context,
omitted when absent:

| Field | Meaning |
|---|---|
| `message` | human-readable explanation; never parse it |
| `file`, `line`, `column` | where, for humans and editors |
| `path` | a JSON Pointer (`/entities/3/components/0/asset`) into the offending file, for `jq`/programmatic edits |
| `entity`, `component`, `field` | what the error is about |
| `did_you_mean` | the closest known *name* when yours is a near-miss |
| `suggestion` | splice-ready replacement *text* (e.g. a rustc machine-applicable fix) |
| `candidates` | the options an ambiguous choice was between |
| `severity` | present only as `"warning"`; absence means error |

**Codes are API.** Adding one is a feature; renaming or removing one is a
breaking change to every agent script in the wild.

## Warnings

Warnings mark scenes that are *legal but almost certainly wrong* — dead data,
invisible geometry — where the screenshot would just look subtly off. They
ride the same stderr stream with `"severity": "warning"` and full context, the
command exits 0, and the stdout result reports their count.
`engine validate --strict` promotes warnings to errors (exit 1) — the CI mode.

The bar for adding a warning is high: it must indicate a probable authoring
mistake, never a style opinion. An agent that gets nagged learns to ignore the
stream, which destroys the tier's value.

## Every command reports every error

`engine validate`, `engine screenshot`, and `engine run-scene` run the same
validation pipeline and emit the same diagnostics, all at once: **which
command you ran never changes what you learn about a broken scene.** No
command ever reports only the first error.

## Commands

```
engine validate <scene.json>... [--strict]   # one or more files; aggregate verdict
engine screenshot <scene.json> --out x.png [--camera N] [--width W --height H]
engine run-scene <scene.json> [--camera N] [--record-input f.input.jsonl]
engine diff-render <scene.json> <baseline.png> [--out diff.png] [--camera N]
                   [--threshold N] [--max-diff-percent P]
engine edit <scene.json> [--watch]           # GUI editor; --watch = read-only
engine simulate <scene.json> --steps N [--input f.input.jsonl]
                [--bake out.json] [--trace t.jsonl] [--entity Name]...
engine raycast <scene.json> --from x,y,z --dir x,y,z [--steps N]
engine filmstrip <scene.json> --out strip.png [--start/--end/--frames/--columns]
engine list-animations <scene-or-clip-or-gltf> [--schema]
engine list-joints <scene-or-gltf> [--entity Name] [--time T] [--clip Name]
engine build [--check]                       # --check: type-check only, ~half the time
engine road-centerline <scene.json> [--entity Name]  # where a Road actually went
engine list-colliders <scene.json> [--entity Name] [--steps N] [--input f]
                                             # every collider physics holds, and where
engine ui-layout <scene.json> [--width W --height H] [--entity Name]...  # where the UI landed
engine terrain-height <scene.json> --at x,z [--entity Name]  # where the ground is
engine inspect <scene.json> [--entity Name]  # every field, defaults filled in
engine list-components [--component Name]    # scene + component JSON Schemas
engine info                                  # selected GPU adapter
engine init [dir] [--force]                  # scaffold a project; refuses a non-empty dir
engine agent-guide                           # the agent orientation, as markdown
```

`engine init` writes a starter scene, a script, and the agent orientation under
the names an agent's tool already reads (`AGENTS.md`, `CLAUDE.md`). Its stdout
object carries `created`, the `files` written, and a `next` array of the
commands to run — enough for an agent to continue without reading this
document. It **refuses a directory that already holds anything** unless
`--force` (`init_target_not_empty`, exit 2), because the names it writes are
exactly the names a project already has.

`engine edit` is a windowed command and exempt from the streams contract
while its window is open; on a fatal failure it still exits with one
`EngineError` line (`editor_failed`, exit 2). Its *writes* are the contract:
every editor action is one formatter splice — one field, one hunk, atomic
rename — and external edits to the open file win within ~250ms. The editor
runs the same validation as `engine validate` and shows the same error
objects.

`engine validate examples/scenes/*.json --strict` is the whole CI story:
diagnostics interleave with per-file `file` fields, and the stdout summary
aggregates (`{"valid": true, "files": 3, "errors": 0, "warnings": 1}`).

On failure the summary line is the *last* stderr record (`validation_failed`
/ `build_failed`), after the per-diagnostic records.

`engine build` re-emits rustc diagnostics as engine errors with `file`/
`line`/`column`, carries rustc's machine-applicable fixes in `suggestion`,
and — when cargo fails without any compiler diagnostic (broken manifest,
resolution failure) — reports `cargo_error` with the tail of cargo's stderr
as the message. If the workspace is too broken for `engine` itself to run,
plain `cargo build` is the documented fallback.

## diff-render

`engine diff-render` renders the scene at exactly the baseline PNG's
dimensions and compares byte-for-byte in the encoded (sRGB) space the PNG is
in. Two knobs: a pixel *differs* when any RGBA channel deviates by more than
`--threshold` (default 0); the comparison *passes* when the percentage of
differing pixels is at most `--max-diff-percent` (default 0 — bit-exact).
Bit-exactness is promised on the same machine/adapter/binary only; baselines
are per-adapter artifacts, and the report's `adapter` field is the first
thing to check when a diff fails on one machine only.

**Stream exception**: the JSON report prints to stdout on *both* pass and
fail — a failing run still tells the agent how much differs
(`diff_pixels`, `max_channel_delta`) and where (`diff_bounds`). On mismatch,
additionally one `render_mismatch` on stderr and exit 1. `max_channel_delta`
makes near-misses self-diagnosing: `1` says "precision noise, raise the
threshold"; `200` says "something actually moved."

`--out diff.png` (written on pass and fail) classifies every pixel: **red**
= violation, **yellow** = nonzero but within threshold, **faded grayscale**
= identical. The formulas are pinned by tests, so diff images compare across
runs.

There is no bless flag: a baseline *is* a screenshot
(`engine screenshot scene.json --out baseline.png`) — both commands drive
the identical offscreen path. Committed baselines live in
`examples/scenes/verify/baselines/`, per-adapter.

## Simulation

`engine simulate` advances a fixed timestep (`physics.timestep_hz`, integer)
an explicit number of steps and never reads a clock: same scene + same steps
= byte-identical results, pinned by committed golden traces. `--trace`
writes JSONL — one line per dynamic body per step plus contact events
(`{"step": 30, "contact": ["A", "B"], "started": true}`), plus one
`{"step": N, "hud": [...]}` line whenever the scripts' HUD text changes
(a lap crossing is a greppable trace event; script-free traces are
unchanged) — the greppable record agents assert on. The `simulate` success
object carries the final step's HUD as `"hud": [...]` when non-empty, as
does `screenshot`'s. `--bake` writes a *valid scene file* with
`Transform`/`RigidBody` velocities updated and every untouched byte
preserved (the M7 formatter). A bake is a representation checkpoint, not a
solver snapshot: resuming from a bake agrees with the straight-through run
to ~1e-4 (quantization + disposable solver caches), not byte-for-byte.
`--steps N` on `screenshot` and `diff-render` is the edit → simulate → LOOK
loop; `engine raycast` answers spatial questions in JSON
(`{"hit": {"entity", "point", "normal", "distance"}}` or `{"hit": null}`).

The `simulate` report carries **`entities`**: where everything ended up, so
reading one final position needs neither a trace file nor a bake. Each row is
the trace's row — `entity`, `position`, `rotation`, and `linear_velocity` when
the entity has a `RigidBody` — and the default membership is the trace's too:
the dynamic bodies, re-enumerated after the run (so fragments are in and a
broken parent is out), **name-sorted**. The sort is a contract, not cosmetics:
it is what makes an unchanged scene report identically instead of in archetype
order. `--entity NAME` (repeatable) narrows, and reaches entities no trace
enumerates — a fixed floor, a scripted kinematic platform, a camera a chase
script drives. Unknown names are reported all at once (`entity_not_found` with
`did_you_mean`), like every other diagnostic here. The trace and bake formats
are untouched, and so are the committed golden traces.

## Animation

Pose is a pure function of (files, time): `--time T` on `screenshot` and
`diff-render` renders the animated pose at scene time T, reproducibly —
equal times give byte-identical PNGs. `engine filmstrip` tiles N frames over
a time range into one contact-sheet PNG (default range: the longest clip in
the scene). `engine list-animations` dumps every clip reachable from a scene
(or a single `.anim.json`, or a `.gltf`/`.glb`) as JSON — name, duration,
track or channel targets — and `--schema` prints the clip-file JSON Schema.
`engine list-joints` does the same for a rig: every joint's name, parent,
index and rest transform, plus its **posed world transform** under `--time`.
That is the half a filmstrip cannot give you — a contact sheet shows that
something moved, never that the hand reached the doorknob. `engine validate` accepts clip
files directly (structural checks; entity-name resolution needs a scene and
happens when validating the scene). Ordering everywhere: sample animations →
physics (`--steps`) → render.

## Scripting

`Script` components run `fn step(world, step)` from a `.rhai` file once per
fixed step, before physics. The registered `world` API is the script's
entire universe (no time, no I/O, no randomness; 1M-op budget), so
determinism holds with scripts running. Parse failures surface in
`engine validate` with the script's own file/line (`script_parse_error`,
`script_missing_step_fn`); runtime failures are `script_runtime_error` with
file, line, and the owning entity, exit 1. Baking captures script-driven
state: any Transform/RigidBody field that differs from the file's rest value
is written back, everything else byte-preserved.

Two script facilities live on the host rather than in the world:
`world.state(key, default)` / `world.set_state(key, value)` is a numeric
per-run store (replay-deterministic; reset by a fresh run; *not* captured by
bake, exactly like solver caches), and `world.hud(text)` pushes printable-
ASCII overlay lines — cleared every step, so the HUD is a pure function of
the step that drew it, composited identically onto `screenshot`/
`diff-render` output and the `run-scene` window (caps: 16 lines × 96 chars,
exceeding either is a runtime error).

The `run-scene` window additionally draws a frame-rate readout in its
top-right corner. It is wall-clock, so it exists only there: no headless
command renders it, and nothing reproducible depends on it.

## Breaking

A `Breakable` component lists pre-authored fragments; the entity shatters
into them on a hard enough collision (`impulse_threshold`, kg·m/s — absent
means collisions never break it), on `world.break_entity(name)` (queued,
applied after the step's physics; unknown name or no `Breakable` is a
runtime error at call time), or inside `world.explode(x, y, z, radius,
impulse)` (radial impulse with linear falloff; thresholded breakables in
range whose falloff impulse meets their threshold break too). Fragments are
ordinary entities (`Parent.frag0`, …, suffix-deduped) — dynamic bodies that
inherit the parent's motion — so they render, trace, and bake with no
special casing. The trace records each break as
`{"step": N, "broke": "Crate", "fragments": [...]}`; per-step rows
re-enumerate, so fragment rows join from the step after the break (scenes
where nothing breaks trace identically to pre-M14). Bake extends its
change-based rule to structure: a broken file entity is spliced out, its
fragments spliced in with full state, and the baked scene reloads into
exactly the post-break world — pinned bit-exact by CLI test. A thresholded
`Breakable` with no `Collider` is `breakable_without_collider` at validation.

## Input

Keyboard and mouse input is sampled per fixed step — scripts ask
`world.key("ArrowUp")` and `world.mouse("MouseLeft")` — and exists headlessly
only as an `*.input.jsonl` timeline: sparse JSONL keyframes of the *complete*
held set, each in effect from its `step` (0-based) until the next line,
nothing held before the first:

```jsonl
{"step": 0, "held": ["ArrowUp"]}
{"step": 120, "held": ["ArrowUp", "ArrowLeft"], "cursor": [0.62, 0.41]}
{"step": 300, "held": ["MouseLeft"], "cursor": [0.62, 0.41]}
```

Key names are the W3C `KeyboardEvent.code` values from a curated allowlist
(arrows, `KeyA`–`KeyZ`, `Digit0`–`Digit9`, `Space`, `Enter`, `Escape`,
shift/control); the three mouse buttons are `MouseLeft`, `MouseRight` and
`MouseMiddle`, and they ride the same `held` array, since a keyframe is one
complete snapshot of what the player is doing. An unknown name is
`unknown_key` with `did_you_mean`, malformed lines are `input_parse_error`,
non-increasing steps are `unsorted_input_steps` — every error at once, with
the timeline's file/line.

**`cursor` is optional and is a fraction of the frame**, `[x, y]` in `[0, 1]`
with the origin at the top-left corner — not pixels, because a timeline
outlives the window it was recorded in. Values outside the range clamp to the
edge; an **absent `cursor` is the centre of the frame** (`[0.5, 0.5]`), so
every pre-M28 timeline parses unchanged and means what it always meant.
Recorded cursors are quantized to three decimals, which is what the file says
and therefore what replays.

The ray through the cursor depends on the frame's **aspect**, so a
mouse-driven run is a function of `--width`/`--height` as well as of the
scene, the steps and the timeline. `screenshot` uses its own frame,
`diff-render` uses the baseline's, and `simulate`/`raycast` — which render
nothing — use a documented default of **960×540**. See
`designs/mouse-input-design.md` §5.

`--input <f>` on `simulate` / `screenshot` / `diff-render` / `raycast`
replays a timeline while stepping; the same timeline twice is byte-identical
(the golden-trace promise extends to input), and no `--input` means no keys
held. `engine run-scene` is the play mode — the keyboard feeds the same
held-set live, and `--record-input <f>` writes a timeline line whenever the
held set changes, so a human play session becomes a committable artifact that
`--input` replays exactly: record once, regression-test forever.

`engine road-centerline` prints one object: the road's `entity`, `length`,
`width`, `shoulder`, `closed`, and its sampled `points` — each with a world
`position`, a unit `forward` in the XZ plane, and `v`, the metres along the
centerline that the road's markings are painted in. It exists because a `Road`
generates its geometry from a polygon of corners, and anything placed *along*
that road (a guardrail, a sign, a start line) needs the samples the ribbon was
actually built from; re-deriving them in a generator is how two implementations
of one curve begin to disagree about where the road is. `examples/scenes/
make_car_track.py` is the worked example: it writes the road, asks where it
went, and writes the scene again with the barriers on it. With no `--entity`
the scene must contain exactly one road; naming one that is not there is
`entity_not_found` with a `did_you_mean`.

`engine list-colliders` prints `steps` and one `colliders` array, name-sorted:
each row's `entity`, `shape` (`sphere`/`cuboid`/`capsule`/`trimesh`/
`convex_hull`/`other`), its `dimensions` in the file's own terms (a sphere's
radius; a cuboid's three half-extents; a capsule's half-height and radius; a
mesh shape's is its geometry, so the array is empty), world `position` and
`rotation` (Euler XYZ degrees), and `sensor`. A skinned collider proxy (M33)
carries a `part` as well — the name it reports under, which no other row has.

The rows are read back out of the built physics world rather than re-derived
from the components, which is what makes it impossible for the report and the
simulation to disagree: `road-centerline`'s argument, applied to physics. That
matters most for a proxy, whose placement comes from a *pose* and which appears
in no render at all. `--steps N` runs the simulation first, because a
stride-driven pose is what the run reached rather than a function of the file —
`list-joints` grew the same flag in M32 for the same reason.

## The render digest

`screenshot` and `filmstrip` report a `digest` of the frame they wrote:

```json
"digest": {"mean_luminance": 0.405, "background": [63, 69, 85], "coverage": 0.523}
```

`entities_drawn` catches "nothing loaded". It does not catch **"nothing is in
the frame"** — a camera aimed past the scene submits the same geometry and
renders a perfectly correct empty picture — which is the most common bad render
and the one whose diagnosis otherwise costs an image read. `background` is the
most common exact color in the frame (the clear color, or the sky) as sRGB
bytes; `coverage` is the fraction of pixels that are something else, so
`coverage: 0.0` means nothing reached the frame. `mean_luminance` is over the
**encoded** bytes the PNG carries, not linearized — the question is whether the
image looks black, and the image is the encoded one.

**A diagnostic, never a pin.** `diff-render` is what pins a render, bit-exactly
and with a diff image showing where. The digest's numbers are quantized to three
decimals precisely so that this adapter's MSAA nondeterminism (M22: ~24 pixels
of a terrain frame, run to run) cannot move a reported digit and turn a
diagnostic into a phantom diff. Read the image when the digest says something is
there; skip it when the digest says the frame is empty. There is deliberately no
hash: a hash would invite comparing two renders by number, which is the job
`diff-render` already does properly.

## Asking the engine instead of reconstructing the answer

Four small queries whose answers the engine already holds. None of them adds
state between invocations, and none changes an existing output.

**Signed numbers parse.** Every argument that takes a vector or a signed
scalar — `raycast --from`/`--dir`, `terrain-height --at`, `screenshot`/
`diff-render --time`, `filmstrip --start`/`--end` — accepts a leading minus
without the `--from=-6,20,6` workaround. Roughly half the coordinates in a
centered scene are negative, so this was constant, and the failure named the
argument rather than the cause.

`engine terrain-height <scene> --at x,z` reports
`{"entity", "x", "z", "height"}`: the world Y of a `Terrain` patch's height
field, which is a coordinate a caller can assign to a position directly.
Placement is the most common operation on terrain — it is what keeps a tree
from floating and an emitter from firing out of a hillside. It is the **same
sampler `world.terrain_height` answers with**, and it needs no `Collider`,
which is what separates it from a downward raycast: that asks where the
*collider* is, and a patch authored for looks has none. `--entity` picks among
several patches, defaulting to the only one; the `road-centerline` convention.

`engine inspect <scene> [--entity Name]` prints each entity's resolved
components — **every field, defaults filled in** — plus its resolved transform,
name-sorted. Reading the JSON is not the same thing: absent fields *are* the
documented defaults, so a `Material` writing only `albedo` leaves four values
unstated. The components are serialized from what the engine actually built, so
this cannot describe a scene the renderer does not have. It is a pure function
of the file **at rest**: no `--steps`, because "what did you author" and "what
happened when it ran" are different questions and `simulate` owns the second.

`engine list-components --component <Name>` prints one component's schema
instead of the `oneOf` over all of them, carrying the `$defs` that variant
references so the printed document resolves on its own. Without the flag the
output is byte-identical to what it always was — it *is*
`schemas/component-schema.json`. An unknown name is `unknown_component_query`
(exit 1) with a `did_you_mean`.

`schemas/component-schema.json` (from `engine list-components`) carries the
same numeric range constraints `engine validate` enforces, so third-party
validators (`ajv`, `check-jsonschema`) agree with the engine about the same
file. Only cross-field rules (`Camera.far > near`) and semantic checks
(duplicates, asset existence) go beyond the schema.
