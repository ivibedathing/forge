# Forge

**A 3D engine whose primary user is an AI coding agent, not a human in a GUI editor.**

Every mainstream engine assumes a person clicking around a viewport. Scene state lives in binary
files, errors are prose in a log panel, and "see what changed" means launching an editor. An agent
can't do any of that — so integrations bolt an MCP server onto the side and hope the engine's
internals leak enough to be useful.

Forge inverts the tradeoff. Scenes are JSON, errors are structured JSON on stderr, and rendering a
frame is one headless CLI command that writes a PNG. An agent edits a text file, validates it,
builds, renders, **looks at the image**, and iterates — using only ordinary bash and file edits,
with no bespoke integration layer.

Wherever machine-legibility and GUI convenience conflict, machine-legibility wins.

---

## Status: M0–M8 complete

JSON scenes load into a hecs ECS, render headlessly to PNG with PBR lighting, validate with
all-errors-at-once reporting under a formalized CLI contract, reference glTF mesh files, pin their
renders against committed baselines, simulate rigid-body physics deterministically, and open in a
GUI editor that is a live writable *view* onto the file. Verified by 170+ tests, including
offscreen pixel readback and an end-to-end CLI suite. Next up: animation (M9).

What works today:

```bash
engine validate <scene.json>... [--strict]  # every error at once; multi-file; --strict promotes warnings
engine screenshot <scene.json> --out x.png [--steps N] [--camera Name] [--width W --height H]
engine diff-render <scene.json> <baseline.png> [--steps N] [--out diff.png] [--threshold N] [--max-diff-percent P]
engine edit <scene.json> [--watch]       # GUI editor; --watch = read-only supervision mode
engine simulate <scene.json> --steps N [--bake out.json] [--trace t.jsonl]
engine raycast <scene.json> --from x,y,z --dir x,y,z [--steps N]
engine run-scene <scene.json>            # windowed scene viewer
engine list-components                   # scene + component JSON Schemas (with range constraints)
engine build [--check]                   # cargo build/check, diagnostics re-emitted as engine errors
engine run                               # M0 triangle (stack proof)
engine info                              # selected GPU adapter as JSON
```

## Quick start

Requires a recent stable Rust toolchain and a GPU with a Vulkan, Metal, or DX12 backend.

```bash
git clone https://github.com/ivibedathing/forge
cd forge

cargo build
cargo run -p engine-cli -- validate examples/scenes/demo_scene.json
cargo run -p engine-cli -- screenshot examples/scenes/demo_scene.json --out /tmp/demo.png

cargo test --workspace
```

`cargo test --workspace` is the real check, not `cargo build`. The tests in
`crates/engine-render/tests/headless_render.rs` render offscreen and assert on actual pixel values,
because "the window opened and did not crash" does not distinguish a working renderer from a culled
triangle or a shader that writes nothing. They skip cleanly rather than fail on machines with no
usable GPU; physics tests are GPU-free and always run.

## Starting the GUI editor

```bash
cargo run -p engine-cli -- edit examples/scenes/demo_scene.json

# or, once the binary is built:
target/debug/engine edit examples/scenes/demo_scene.json

# read-only supervision mode — full viewport and inspection, writes disabled:
engine edit examples/scenes/demo_scene.json --watch
```

The editor is a **view onto the scene file, never a second source of truth** (invariant #8). The
JSON file on disk stays authoritative:

- The editor polls the file (250ms) and reloads on any external change — edit the JSON in your
  own editor, or let an agent edit it, and the viewport follows.
- Every editor action writes through a format-preserving splice, not a re-serialize: a one-field
  edit is one hunk on one line, and untouched content stays byte-identical. Editor diffs are
  reviewable diffs.
- Writes target entities by `name` and components by `type`, never by index, so they rebase
  cleanly onto concurrent external edits.

In the viewport: right-drag orbits, shift- or middle-drag pans, scroll zooms, left-click picks an
entity. Transform gizmos — `W` move, `R` rotate, `S` scale — preview in memory and commit one
write on release, each editing its `Transform` field component-wise. The inspector is
generated from the component schema, so a new component is editable the day it exists, and the
validation panel shows the same structured errors the CLI emits, click-to-select.

For agents: the hidden flag `--self-screenshot <png> [--self-screenshot-after-ms N]` renders the
editor UI to a PNG and exits — the way an agent *looks at* the editor itself.

## The agent feedback loop

The whole design serves this cycle:

1. Edit a `.json` scene file, or a Rust component/system.
2. `engine validate scene.json` — fast structural check against the component schemas.
3. `engine build` — compiler and engine errors surface as structured JSON.
4. `engine screenshot scene.json --out /tmp/check.png` — headless render.
5. **View the PNG.** Claude Code can read images directly.
6. Iterate. No human in the loop.

`engine diff-render` turns step 5 into a regression test: this scene must look like
`baseline.png`, within tolerance (bit-exact by default), in CI. Blessing a new baseline is just
`engine screenshot` — no separate bless flag, deliberately. Baselines are per-adapter artifacts;
determinism is promised same-machine/same-adapter, and the report carries the adapter name.

`engine screenshot` is the single most important command in the project — it is what closes the
loop. Everything else is in service of it. Its `--steps N` flag runs physics first, so the same
command is also the eyes for simulation: edit, simulate, **look**.

## Physics

Rigid-body physics via [rapier3d](https://rapier.rs) with `enhanced-determinism`: `RigidBody`
(dynamic/kinematic/fixed) and `Collider` (cuboid/sphere/capsule) components, plus an optional
scene-level `physics` block (`gravity`, integer `timestep_hz`). Everything is observable from the
command line:

- `engine simulate --steps N` steps headlessly; `--trace` writes one JSON line per step per body,
  and `--bake` writes a plain scene JSON of the final state — resumable, diffable, renderable.
- `engine raycast` casts a ray (optionally after N steps) and reports the hit as JSON.
- Same file + same steps → byte-identical traces, pinned by a committed golden trace.

## Scene format

Scenes are JSON. Not RON, not a binary blob — JSON, because the success criterion says the agent
works with "ordinary bash," and `jq` is ordinary bash. The same serialization is both the file and
the schema, so third-party tooling can validate scenes without a Rust parser.

Components are **internally tagged** on `"type"`, so a component is one flat object rather than a
nested single-key wrapper — the shape `jq` is pleasant against and the shape an LLM is least likely
to get wrong.

```json
{
  "name": "demo_scene",
  "entities": [
    {
      "name": "Cube1",
      "components": [
        { "type": "Transform",
          "position": [0.0, 3.0, 0.0],
          "rotation": [0.0, 0.0, 0.0, 1.0],
          "scale":    [1.0, 1.0, 1.0] },

        { "type": "Mesh", "asset": "builtin:cube" },

        { "type": "Material", "albedo": [1.0, 0.0, 0.0], "metallic": 0.0, "roughness": 0.8 }
      ]
    }
  ]
}
```

`Mesh.asset` is a builtin (`builtin:cube`, `builtin:plane`, `builtin:sphere`, `builtin:triangle`)
or a `.gltf`/`.glb` path relative to the scene file. Lighting comes from `DirectionalLight` and
`AmbientLight` components (a scene with no light components gets a documented fallback rig);
materials are GGX Cook–Torrance PBR with `albedo`, `metallic`, `roughness`, `emissive`.

Accepted cost: JSON has no comments. Anything a scene needs to say about itself has to be a real,
schema'd field.

## Structured errors

Errors are JSON on stderr with a non-zero exit code — parseable, not just readable:

```json
{"error": "unknown_component", "entity": "Cube1", "component": "Meterial", "did_you_mean": "Material"}
```

`error` is a stable snake_case code to match on; `message` is prose you should never parse. Optional
context (`file`, `line`, `path` as a JSON Pointer for `jq`, `entity`, `component`, `field`,
`did_you_mean`) is flattened into the same flat object and omitted when absent. Near-miss names get
a `did_you_mean` by Levenshtein distance, because an agent that writes `Meterial` should be told
the answer rather than left to guess.

The full wire contract lives in [`docs/cli-contract.md`](docs/cli-contract.md): stdout is one JSON
object on success and empty on failure, stderr is NDJSON with *every* error at once, and exit
codes split 1 ("your files are at fault") from 2 ("your invocation or environment is"). Warnings
ride the same stream with `"severity": "warning"` and exit 0 unless `--strict`. Every code is
enumerated in [`docs/error-codes.md`](docs/error-codes.md) and pinned by repo-contract tests —
**codes are API**. Even a panic reports as structured NDJSON (`internal_panic`, exit 2).

## Invariants

These are what make the engine agent-operable. Breaking one breaks the premise.

1. **No binary scene or asset-metadata formats.** Scenes, materials, and prefabs are JSON and
   git-diffable by construction.
2. **No hidden state.** Everything needed to reconstruct a scene lives in text files on disk. No
   editor-only in-memory state; no opaque GUIDs without an in-repo lookup table.
3. **Assets are referenced by relative path**, never by opaque ID.
4. **Entities have stable `name` fields** — CLI commands and agent edits target them by name.
5. **Components are plain data.** All logic lives in systems.
6. **Errors are structured JSON on stderr**, with a non-zero exit code.
7. **Component schemas are derived from Rust structs via serde**, never hand-maintained, and scenes
   are validated against them.
8. **A GUI editor is a view onto the text files** — never a second source of truth. The M7 editor
   is this invariant made concrete.

## Architecture

```
crates/
  engine-core/     ECS, scene graph, validation, structured errors, format-preserving writes
  engine-render/   wgpu renderer, PBR shaders, materials, diff-render comparison
  engine-assets/   glTF mesh + PNG texture loading — the only crate that opens asset files
  engine-physics/  rapier3d integration: deterministic stepping, trace, bake, raycast
  engine-editor/   egui GUI editor — a live, writable view onto the scene file
  engine-cli/      the `engine` binary — the primary interface
schemas/
  component-schema.json    generated from Rust structs (`engine list-components`), never hand-written
examples/scenes/*.json     demo scenes + verification fixtures with committed baselines
docs/                      cli-contract.md, error-codes.md
```

`engine-core` must not depend on the renderer or on any windowing library: headless tooling
(`engine validate`, `engine list-components`) links only that crate and has to stay usable on a
machine with no GPU. `gpu` and `renderer` know nothing about windows, so headless rendering is a
first-class path rather than a special case.

**Stack:** Rust · [wgpu](https://github.com/gfx-rs/wgpu) 30 (Vulkan/Metal/DX12) ·
[winit](https://github.com/rust-windowing/winit) 0.30 · [glam](https://github.com/bitshifter/glam-rs) ·
serde + JSON · [hecs](https://github.com/Ralith/hecs) ·
[rapier3d](https://rapier.rs) · [egui](https://github.com/emilk/egui) · `image` for PNG export.

hecs over `bevy_ecs` was a deliberate churn tradeoff: `bevy_ecs` breaks every Bevy release, and this
project already spent a build cycle on wgpu's API churn. Full reasoning, including what it gives up,
is in the design doc §9.

## CLI surface

| Command | Purpose |
|---|---|
| `engine validate <scene.json>... [--strict]` | Schema + semantic check; every error at once |
| `engine screenshot <scene.json> --out out.png` | Headless render to PNG — the key agent feedback tool |
| `engine diff-render <scene.json> <baseline.png>` | Pixel-diff against a committed baseline; CI visual regression |
| `engine edit <scene.json> [--watch]` | GUI editor; `--watch` is read-only supervision |
| `engine simulate <scene.json> --steps N [--bake out.json] [--trace t.jsonl]` | Headless deterministic physics |
| `engine raycast <scene.json> --from x,y,z --dir x,y,z` | Scene query as JSON |
| `engine run-scene <scene.json>` | Windowed viewer (steps physics in real time) |
| `engine list-components` | Dump scene + component JSON Schemas |
| `engine build [--check]` | Compile; rustc diagnostics as structured errors |
| `engine info` | Selected GPU adapter as JSON |

## Roadmap

- [x] **M0** — Window + triangle. Proves the graphics stack end to end.
- [x] **M1** — CLI skeleton: `build`, `screenshot`, `info`. The JSON error convention.
- [x] **M2** — JSON scene loading, hecs integration, Transform/Mesh/Camera, schema export.
- [x] **M3** — Asset pipeline: glTF meshes, textures.
- [x] **M4** — Materials + lighting: PBR materials, directional + ambient lights, sRGB pipeline.
- [x] **M5** — Validation hardening: schema-driven field checks, warnings tier, the formalized
      CLI contract (`docs/cli-contract.md`).
- [x] **M6** — Diff-render / visual regression against committed baselines.
- [x] **M7** — GUI editor (scope E0–E2): viewport, picking, gizmo, schema-driven inspector,
      validation panel, `--watch`.
- [x] **M8** — Rigid-body physics: deterministic simulate/trace/bake/raycast.
- [ ] **M9** — Animation.

## Non-goals for v1

No networking or multiplayer. No advanced rendering (GI, ray tracing) — a clean forward renderer
with basic PBR is enough to prove the concept. Desktop only; no mobile or console targets. The GUI
editor stays deliberately small: it is a convenience view for supervising an agent, not the primary
interface, and features land there only after the CLI can do the same thing headlessly.

## Open questions

Deliberately unsettled, and interacting with each other:

- Runtime scripting (Lua/Rhai) vs. compiled-Rust systems only — this bounds how much an agent can
  iterate on without a full rebuild.
- Whether to support hot reload of scene data without a Rust rebuild. Likely high value for agent
  iteration speed, and the one argument strong enough to reopen the ECS choice. (The editor's
  250ms file polling is a first taste of what this buys.)

`agent-native-engine-design.md` is the source of truth for layout, formats, and build order.
`CLAUDE.md` carries the working notes an agent needs before touching this code.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
