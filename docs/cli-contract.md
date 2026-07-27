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
engine run-scene <scene.json> [--camera N]
engine build [--check]                       # --check: type-check only, ~half the time
engine list-components                       # scene + component JSON Schemas
engine info                                  # selected GPU adapter
```

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

## Validation against the published schema

`schemas/component-schema.json` (from `engine list-components`) carries the
same numeric range constraints `engine validate` enforces, so third-party
validators (`ajv`, `check-jsonschema`) agree with the engine about the same
file. Only cross-field rules (`Camera.far > near`) and semantic checks
(duplicates, asset existence) go beyond the schema.
