# Agent ergonomics (M24/M25)

*Relocated verbatim from `CLAUDE.md` when it was compacted to an index. The digest that points here is `CLAUDE.md` §Agent ergonomics.*

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
