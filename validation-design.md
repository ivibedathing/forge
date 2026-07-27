# M5 — Validation & Structured Errors: Design

Milestone M5 of `agent-native-engine-design.md` §8: "Make `engine validate` and `engine build`
genuinely agent-friendly — this is as important as rendering features." This document settles what
that means concretely. Written 2026-07-27, against the post-M2 codebase (M4 is designed in
`materials-lighting-design.md` but not yet implemented; §4 notes the one interaction).

M5 is unusual among the milestones: M0–M2 already built much of it. All-errors-at-once validation
with file/line via `lineindex`, `EngineError` with `did_you_mean`, and `engine build` re-emitting
rustc diagnostics all exist and are tested. So this design is a gap analysis and a hardening plan,
not a greenfield spec. The framing question for every item: *what can still surprise an agent that
is parsing stderr and branching on exit codes?*

## 1. Scope

**In:** a formalized error contract (streams, exit codes, severity, JSON-pointer paths, a
registered error-code vocabulary); schema-driven per-component validation replacing the fragile
serde-message parsing; numeric range constraints that live in the published JSON Schema and are
enforced by `engine validate`; new semantic checks (duplicate components, camera ranges) and a
warnings tier with `--strict`; full-scene error reporting from *every* scene-consuming command,
not just `validate`; `engine build` hardening (`cargo` failures without diagnostics, rustc
suggestions, `--check`); a panic hook so even a bug never prints an unparseable error; multi-file
`validate`; a CLI contract document; and end-to-end CLI tests that pin all of it.

**Out (deferred, see §11):** autofix (`--fix`), watch mode, an LSP server, validating asset file
*contents* (M3's job), `diff-render` (M6).

## 2. Gap analysis

What exists is described in CLAUDE.md's "Current state." What an agent can still trip over:

1. **Per-component field errors are built by parsing serde's English messages.**
   `component_field_error` in `validate.rs` string-matches `"unknown field \``…" — documented in
   its own comment as an unstable interface that degrades on format changes. Degrading means the
   agent loses the field name, the line, and the suggestion exactly when it needs them.
2. **No value-range checking, and the published schema carries no ranges.** `fov: 0`, `near:
   -1.0`, `far: 0.05` with `near: 0.1`, `scale: [0,0,0]` — all validate today and render garbage
   or nothing. "Renders nothing with no error" is the worst outcome this engine can produce.
3. **A duplicated component silently overwrites.** Two `Transform`s on one entity: hecs's
   `EntityBuilder::add` replaces the first, so the file shows two and the world contains one —
   whichever came last. Hidden state, invariant 2, in miniature.
4. **Only `engine validate` reports everything.** `screenshot` and `run-scene` call
   `Scene::from_source`, which returns the *first* validation error. An agent iterating via
   screenshots gets a drip-feed of one error per run.
5. **The wire contract is informal.** Exit code is always 1; nothing distinguishes "your scene is
   wrong" from "there is no GPU." Error codes are stable by intention but no registry enforces it
   — a rename or a near-duplicate (`asset_not_found` vs. `asset_missing`) would ship silently.
   Errors carry file/line but not the JSON path, so a *programmatic* fix (`jq 'setpath(...)'`)
   requires re-deriving what the validator already knew.
6. **`engine build` drops information.** A failure with no compiler diagnostics — broken
   `Cargo.toml`, dependency resolution — yields `build_failed` with zero detail, because only
   stdout's message stream is read. rustc's machine-applicable suggestions (the exact replacement
   text) are discarded.
7. **A panic is an unparseable error.** `main.rs` promises no panics on user-reachable paths;
   nothing enforces the promise, and a violated promise prints a raw backtrace.
8. **The CLI contract has no tests.** `engine-cli` has zero integration tests; every stream/exit
   code behavior above is pinned only by convention.

## 3. The error contract, formalized

This section becomes `docs/cli-contract.md` almost verbatim. The contract is: an agent can operate
every command with `jq` and `$?` alone.

**Streams.** stdout carries exactly one JSON object on success — the command's result — and
nothing on failure. stderr carries NDJSON: one complete `EngineError` object per line, nothing
else, ever. (Human-facing `--help`/`--version` are documentation, not results, and are exempt.)

**Exit codes.** Three, chosen so an agent can branch without parsing:

| Code | Meaning | Examples |
|---|---|---|
| 0 | success (warnings allowed) | valid scene, PNG written |
| 1 | **the input files are at fault** | validation errors, `compile_error`, `scene_unreadable` |
| 2 | **the invocation or environment is at fault** | no GPU adapter, `cargo` missing, unwritable `--out`, bad CLI flags, internal panic |

The 1/2 split is the actionable one: 1 means "edit your files and retry," 2 means "fix your
command or your machine." Each error code's class is declared in the registry (below), so the
mapping is data, not scattered judgment calls. clap usage errors already exit 2, which lands on
the right side of this split.

**Wire-format additions** — all additive; existing fields never change meaning:

- `path` — a JSON Pointer (`/entities/3/components/0/asset`) into the offending file. The
  validator already computes exactly this string for the line lookup and then throws it away;
  M5 keeps it. `line` remains for humans and editors; `path` is for `jq`/programmatic edits.
- `severity` — present only as `"warning"`; absence means error. Warnings ride the same NDJSON
  stream with the same context fields (§6).
- `suggestion` — concrete replacement *text* (from rustc's machine-applicable fixes), as opposed
  to `did_you_mean`, which is a known *name*. Kept separate because an agent applies them
  differently: `did_you_mean` corrects an identifier, `suggestion` is splice-ready source.

**The error-code registry.** A new `engine-core/src/codes.rs` declares every code as a `const`
alongside its exit-code class and one-line description; all `EngineError::new` call sites use the
consts. `docs/error-codes.md` is the human-facing table, and a repo-contract test (same pattern as
`repo_contracts.rs` for the schema) asserts the source constants and the doc rows match one-to-one
in both directions. **Stability policy, stated in the doc: codes are API.** Adding one is a
feature; renaming or removing one is a breaking change to every agent script in the wild.

## 4. Schema-driven component validation

The centerpiece. Today `validate.rs` hand-walks scene structure but delegates per-component field
checking to `serde_json::from_value`, then reverse-engineers serde's prose (gap 1). M5 inverts
this: **the per-component walk is driven by the schemars-generated schema** — the same
`schema_for!(ComponentData)` that `engine list-components` publishes.

For each component object, look up its variant in the schema's `oneOf` (already discriminated by
`type`, pinned by tests in `schema.rs`) and check, in our own code with our own messages:

- unknown keys against `properties` → `unknown_field` + `suggest_from` the property names
- `required` entries absent → `missing_field`
- JSON type per property (`type`, `minItems`/`maxItems` for the vec3 arrays) → `invalid_field_type`
- `minimum` / `maximum` / `exclusiveMinimum` / `exclusiveMaximum` → `value_out_of_range`

Every check knows its JSON path (so line + `path` come free via `Cx`) and produces exactly the
error shape §3 specifies. serde then parses the already-clean component as the final gate — if the
walk passes and serde still rejects, that is the existing `scene_parse_desync` bug signal, and a
corpus test (§10) asserts the two agree in both directions.

**Why this and not the alternatives:**

- *Keep parsing serde messages* — rejected; it is the fragility being removed, and its failure
  mode (silent degradation to a context-free message) is invisible until an agent hits it.
- *The `jsonschema` crate as the validator* — rejected. Its errors are generic ("1 is not of type
  string" with a schema path), carry no `did_you_mean`, and translating them back into our shape
  is more code than walking the four checks ourselves over a schema we generate and pin with
  tests. A dependency that still requires a translation layer buys nothing.
- *Macro-generated field metadata* (extend `components!`) — rejected; it creates a second
  metadata channel that can drift from the published schema. The schema **is** the metadata.

**Range constraints are authored on the structs** via `#[schemars(...)]` attributes
(`range(min = …, max = …)` and exclusive variants), so they flow into the published
`component-schema.json` and third-party validation (ajv, `check-jsonschema`) agrees with
`engine validate` about the same file. This upgrades invariant 7 from "the schema is derived, not
hand-written" to "the schema is derived *and enforced* — validation and publication cannot
disagree." `repo_contracts.rs` already forces the regeneration.

Initial constraint set:

| Field | Constraint |
|---|---|
| `Camera.fov` | exclusive (0, 180) degrees |
| `Camera.near` | > 0 |
| `Camera.far` | > 0 (cross-field check below) |
| `Material.metallic`, `Material.roughness` | [0, 1] |
| `Material.albedo` components | [0, 1] |

**Cross-field checks stay hand-written** — JSON Schema cannot express `far > near` without
contortions. It is a semantic check like `multiple_active_cameras`, emitted as
`value_out_of_range` on `far` with a message naming `near`'s value.

**Interaction with M4:** `materials-lighting-design.md` §7 specifies `value_out_of_range` for the
light components as one-off checks. Whichever milestone lands second rebases on the other: if M5
is first, M4's constraints become schema attributes (one line each); if M4 is first, M5 migrates
its hand checks into the mechanism. The error code and wire shape are identical either way, so
nothing downstream notices.

## 5. New semantic checks (errors)

- `duplicate_component` — the same `type` twice on one entity (gap 3). An error, not a warning:
  the file and the world would disagree, which is the hidden-state failure invariant 2 exists to
  prevent. Points at the second occurrence, names the entity.
- `value_out_of_range` for `far <= near` (§4).

The existing check set (duplicate names, multiple active cameras, unresolvable assets, unknown
fields/components) is unchanged.

## 6. Warnings — legal but almost certainly wrong

Some scenes are valid and still wrong in ways the renderer cannot flag: the screenshot just looks
subtly off, and the agent burns iterations discovering why. That is what a warnings tier is for.
Initial set, deliberately small:

- `unused_material` — a `Material` on an entity with no `Mesh`. Dead data; probably a mistake in
  which entity was edited.
- `zero_scale` — any `Transform.scale` component equal to 0. Renders as invisible or degenerate
  geometry with no error; the classic "I edited the file and nothing changed."

Semantics: warnings go to stderr as NDJSON with `"severity": "warning"` and full context
(file/line/path/entity), the command **exits 0**, and the stdout result object reports counts
(`"warnings": 2`). `engine validate --strict` promotes warnings to errors (exit 1) — the CI mode.
The bar for adding a warning is high and documented in `cli-contract.md`: it must indicate a
probable authoring mistake, never a style opinion. An agent that gets nagged learns to ignore the
stream.

Not warnings, deliberately: a mesh without a `Transform` (documented origin default, same as the
camera), an empty `entities` array (rendering the background is legal), an entity with no
components (a named placeholder is a reasonable authoring state).

## 7. Every command reports every error

`Scene::from_source` changes signature: the error side becomes `Vec<EngineError>` (never empty).
`screenshot` and `run-scene` emit all of them and exit 1 — byte-identical diagnostics to running
`engine validate` first. The rule, stated in the contract doc: **which command you ran never
changes what you learn about a broken scene.** `Scene::load`'s single-error convenience shape
disappears; the CLI is the only caller and it wants the vector. This is a deliberate breaking
change to `engine-core`'s public API, made now while the only consumer is in-tree.

## 8. `engine build` hardening

- **Capture stderr.** On failure with zero compiler diagnostics (broken manifest, resolution
  failure, ICE), emit `cargo_error` carrying the tail of cargo's stderr as the message —
  never again a bare `build_failed` with no explanation (gap 6).
- **Suggestions.** From the primary diagnostic's children, extract machine-applicable
  `suggested_replacement`s into the new `suggestion` field (§3). rustc's fixes are precisely the
  splice-ready text an agent wants.
- **`engine build --check`** — `cargo check` under the same error convention. Type errors surface
  in roughly half the time; the edit→validate→check loop tightens accordingly, and full `build`
  is only needed before actually rendering.

## 9. No unparseable exits

- **Panic hook.** `main` installs `std::panic::set_hook`: any panic prints one
  `{"error": "internal_panic", …}` line (message + panic location in `file`/`line`) and the
  process exits 2. With `RUST_BACKTRACE=1` the backtrace is embedded, escaped, in the message —
  the NDJSON guarantee holds even then. This converts "the promise that main.rs never panics"
  into "a broken promise still speaks the protocol."
- **clap errors.** `--help`/`--version` stay human-readable (documentation, not errors). Actual
  parse failures — unknown subcommand, unknown flag, missing required arg — are re-rendered as
  `invalid_invocation` JSON (exit 2), with `did_you_mean` populated from clap's own suggestion
  machinery. A typo'd flag is exactly the agent path, and today it gets prose.
- **`engine validate` takes one or more files.** Diagnostics interleave with correct per-file
  `file` fields; the stdout summary aggregates (`{"valid": false, "files": 3, "errors": 7,
  "warnings": 1}`). This makes `engine validate examples/scenes/*.json` the whole CI story.

## 10. Test plan

`engine-core` (no GPU):

- Every new code fires with `file`, `line`, **and `path`** — extend the existing
  `every_semantic_error_carries_file_and_line` sweep to assert `path` too.
- Range checks: representative field per constraint kind (min, max, exclusive, cross-field).
- `duplicate_component`; warnings emit with `severity` and do not affect validity; `--strict`
  logic (promotion happens in the CLI, but the classification lives in core).
- **Walk/serde agreement corpus:** for a corpus of scenes (valid and broken), schema-walk-clean ⟺
  serde-parses. Catches drift in either direction — the property that replaces the old message
  parsing's tests.
- **Robustness corpus:** garbage inputs — deep nesting, `1e999`, wrong types at every level,
  empty file, BOM, unicode entity names — validator returns errors and never panics, and every
  returned error still carries file + line.
- **Golden kitchen-sink:** one scene exercising every error code, its full NDJSON output
  (including line numbers) pinned as a snapshot. Any wire-format drift fails loudly.
- Registry contract test: `codes.rs` ⟷ `docs/error-codes.md`, both directions (§3).

`engine-cli` (new integration suite, `assert_cmd`, no GPU needed for validate/build paths):

- Exit codes 0/1/2 land per the §3 table; stdout is exactly one parseable JSON object on success
  and empty on failure; every stderr line parses as JSON individually.
- Multi-file validate; `--strict` flips exit 0→1 on a warnings-only scene.
- Build-diagnostic translation is unit-tested against captured `cargo --message-format=json`
  fixture lines (including a suggestion-bearing one) rather than by compiling a throwaway crate —
  invoking real cargo in tests is slow and network-adjacent.
- Panic hook: a debug-build-only hidden subcommand panics on purpose; the test asserts one JSON
  line and exit 2.

## 11. Deferred

- **`--fix` autofix** — `did_you_mean` + `path` + `suggestion` make mechanical fixes computable;
  applying them is a separate feature with its own dry-run/UX questions. The M5 fields are
  deliberately sufficient for a future `--fix` to be pure plumbing.
- **Watch mode / LSP** — interactive-loop features; they belong with the hot-reload decision
  still open in design doc §9.
- **Asset content validation** (malformed glTF, oversized textures) — M3 owns asset loading; M5
  only guarantees the error *shape* those checks will use. Same for rejecting `..` in asset paths
  once real file paths become legal.
- **Machine-readable progress events** (`{"event": "compiling", …}`) — nothing consumes them yet.

## 12. Build order within M5

Each step leaves the workspace green:

1. `codes.rs` registry + `docs/error-codes.md` + contract test — mechanical, and every later step
   names its codes against it.
2. Wire additions (`path`, `severity`, `suggestion` in `ErrorContext`; exit-code classes) + the
   panic hook + `docs/cli-contract.md` first draft.
3. Range attributes on components (regenerate `schemas/component-schema.json`); the schema-driven
   walk replacing `component_field_error`; new semantic checks + warnings; the agreement and
   robustness corpora.
4. `Scene::from_source` → `Vec<EngineError>`; screenshot/run-scene report everything.
5. `engine build` hardening, multi-file validate, `--strict`, clap re-rendering.
6. CLI integration suite + golden snapshot; finalize `cli-contract.md` against what actually
   ships.
