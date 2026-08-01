# Validation & the CLI contract (M5)

*Relocated verbatim from `CLAUDE.md` when it was compacted to an index. The digest that points here is `CLAUDE.md` §Validation & the CLI contract.*

The wire contract lives in `docs/cli-contract.md` — stdout is one JSON object on success and empty
on failure, stderr is NDJSON, exit codes split 1 ("your files are at fault") from 2 ("your
invocation/environment is"). Every error code is a const in `engine-core/src/codes.rs` with its exit
class; `docs/error-codes.md` mirrors it and a repo-contract test keeps them in lockstep — **codes
are API**, never rename one casually.

**`validate/` is seven modules**, split out of one 5,716-line file as pure code motion (28 lines
left it: ten `fn` → `pub(super) fn`, nine redundant borrows, one call site, and five rustfmt
re-wraps). `mod.rs` is `validate_source` — now the preamble plus ten named pass calls —
`Cx`/`Checked`/`ComponentSchemas`, and `validate_material_source`. `entity.rs` is the per-entity
walk, `passes.rs` the ten **cross-entity** passes (camera, lights, daylight, point-light budget,
collision layers, wheel, meadow, foot planting, HUD parent, animation), `component.rs`
`check_component`, `walk.rs` the schema walk, `blocks.rs` the `physics`/`environment`/`daylight`
blocks, and `tests.rs` the 2,000-line corpus that lives with the code.

**The passes exist because a name may be authored after its use** — a wheel's chassis, a HUD
element's parent — so anything naming another entity has to wait for every name to exist. The walk
hands them a `SceneFacts` struct rather than sixteen positional values, and that is not tidiness:
four of its fields are `Vec<(String, String)>` and three are `BTreeSet<String>`, so a swapped pair
would type-check and validate the wrong thing. Field-init shorthand in and destructuring by name
out makes the mapping name-identity end to end. (The `point_lights` pass is `point_light_budget`
for the same reason — a function named for its field would shadow it.)

Per-component field checking is **schema-driven**: the walk in `validate/walk.rs` reads the same
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
