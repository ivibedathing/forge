# The CLI contract

How the `engine` binary talks to whoever runs it. The contract is: **an agent
can operate every command with `jq` and `$?` alone.** Everything below is
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
- `--help` and `--version` are documentation, not results, and are exempt:
  human-readable prose, exit 0.

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
                [--bake out.json] [--trace t.jsonl]
engine raycast <scene.json> --from x,y,z --dir x,y,z [--steps N]
engine filmstrip <scene.json> --out strip.png [--start/--end/--frames/--columns]
engine list-animations <scene-or-clip> [--schema]
engine build [--check]                       # --check: type-check only, ~half the time
engine list-components                       # scene + component JSON Schemas
engine info                                  # selected GPU adapter
```

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
(`{"step": 30, "contact": ["A", "B"], "started": true}`) — the greppable
record agents assert on. `--bake` writes a *valid scene file* with
`Transform`/`RigidBody` velocities updated and every untouched byte
preserved (the M7 formatter). A bake is a representation checkpoint, not a
solver snapshot: resuming from a bake agrees with the straight-through run
to ~1e-4 (quantization + disposable solver caches), not byte-for-byte.
`--steps N` on `screenshot` and `diff-render` is the edit → simulate → LOOK
loop; `engine raycast` answers spatial questions in JSON
(`{"hit": {"entity", "point", "normal", "distance"}}` or `{"hit": null}`).

## Animation

Pose is a pure function of (files, time): `--time T` on `screenshot` and
`diff-render` renders the animated pose at scene time T, reproducibly —
equal times give byte-identical PNGs. `engine filmstrip` tiles N frames over
a time range into one contact-sheet PNG (default range: the longest clip in
the scene). `engine list-animations` dumps every clip reachable from a scene
(or a single `.anim.json`) as JSON — name, duration, track targets — and
`--schema` prints the clip-file JSON Schema. `engine validate` accepts clip
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

## Input

Keyboard input is sampled per fixed step — scripts ask `world.key("ArrowUp")`
— and exists headlessly only as an `*.input.jsonl` timeline: sparse JSONL
keyframes of the *complete* held set, each in effect from its `step` (0-based)
until the next line, nothing held before the first:

```jsonl
{"step": 0, "held": ["ArrowUp"]}
{"step": 120, "held": ["ArrowUp", "ArrowLeft"]}
{"step": 300, "held": []}
```

Key names are the W3C `KeyboardEvent.code` values from a curated allowlist
(arrows, `KeyA`–`KeyZ`, `Digit0`–`Digit9`, `Space`, `Enter`, shift/control);
an unknown name is `unknown_key` with `did_you_mean`, malformed lines are
`input_parse_error`, non-increasing steps are `unsorted_input_steps` — every
error at once, with the timeline's file/line.

`--input <f>` on `simulate` / `screenshot` / `diff-render` / `raycast`
replays a timeline while stepping; the same timeline twice is byte-identical
(the golden-trace promise extends to input), and no `--input` means no keys
held. `engine run-scene` is the play mode — the keyboard feeds the same
held-set live, and `--record-input <f>` writes a timeline line whenever the
held set changes, so a human play session becomes a committable artifact that
`--input` replays exactly: record once, regression-test forever.

`schemas/component-schema.json` (from `engine list-components`) carries the
same numeric range constraints `engine validate` enforces, so third-party
validators (`ajv`, `check-jsonschema`) agree with the engine about the same
file. Only cross-field rules (`Camera.far > near`) and semantic checks
(duplicates, asset existence) go beyond the schema.
