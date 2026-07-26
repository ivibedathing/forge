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

## Status: M0

The workspace builds and renders a triangle, verified by offscreen pixel readback. Scene loading,
the ECS, and `engine screenshot` are next. **Most of the CLI described below does not exist yet** —
what's listed under [Roadmap](#roadmap) is the plan, not a changelog.

What works today:

```bash
engine run [--width 1280 --height 720]   # windowed viewer, M0 triangle — no scene loading yet
engine info                              # selected GPU adapter, as JSON
```

## Quick start

Requires a recent stable Rust toolchain and a GPU with a Vulkan, Metal, or DX12 backend.

```bash
git clone https://github.com/ivibedathing/forge
cd forge

cargo build
cargo run -p engine-cli -- info
cargo run -p engine-cli -- run

cargo test --workspace
```

`cargo test --workspace` is the real check, not `cargo build`. The tests in
`crates/engine-render/tests/headless_render.rs` render offscreen and assert on actual pixel values,
because "the window opened and did not crash" does not distinguish a working renderer from a culled
triangle or a shader that writes nothing. They skip cleanly rather than fail on machines with no
usable GPU.

## The agent feedback loop

The whole design serves this cycle:

1. Edit a `.json` scene file, or a Rust component/system.
2. `engine validate scene.json` — fast structural check against the component schemas.
3. `engine build` — compiler and engine errors surface as structured JSON.
4. `engine screenshot scene.json --out /tmp/check.png` — headless render.
5. **View the PNG.** Claude Code can read images directly.
6. Iterate. No human in the loop.

`engine diff-render` turns step 5 into a regression test: this scene should always look like
`baseline.png`, within tolerance, in CI.

`engine screenshot` is the single most important command in the project — it is what closes the
loop. Everything else is in service of it.

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

        { "type": "Mesh", "asset": "meshes/cube.glb" },

        { "type": "Material", "albedo": [1.0, 0.0, 0.0], "metallic": 0.0, "roughness": 0.8 }
      ]
    }
  ]
}
```

Accepted cost: JSON has no comments. Anything a scene needs to say about itself has to be a real,
schema'd field.

## Structured errors

Errors are JSON on stderr with a non-zero exit code — parseable, not just readable:

```json
{"error": "unknown_component", "entity": "Cube1", "component": "Meterial", "did_you_mean": "Material"}
```

`error` is a stable snake_case code to match on; `message` is prose you should never parse. Optional
context (`file`, `line`, `entity`, `component`, `field`, `did_you_mean`) is flattened into the same
flat object and omitted when absent. Near-miss names get a `did_you_mean` by Levenshtein distance,
because an agent that writes `Meterial` should be told the answer rather than left to guess.

This convention exists as of M0, before there was much to report, because it is far cheaper to
establish now than to retrofit across a codebase that has grown its own ad-hoc error prose.

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
8. **A GUI editor, if it ever exists, is a view onto the text files** — never a second source of
   truth.

## Architecture

```
crates/
  engine-core/     ECS, scene graph, structured errors, math re-exports
  engine-render/   wgpu renderer, shaders, materials
  engine-assets/   mesh/texture loading, asset schema        (lands at M3)
  engine-cli/      the `engine` binary — the primary interface
schemas/
  component-schema.json    generated from Rust structs, never hand-written
examples/scenes/*.json
docs/component-reference.md    generated from doc comments
```

`engine-core` must not depend on the renderer or on any windowing library: headless tooling
(`engine validate`, `engine list-components`) links only that crate and has to stay usable on a
machine with no GPU. `gpu` and `renderer` know nothing about windows, so headless rendering is a
first-class path rather than a special case.

**Stack:** Rust · [wgpu](https://github.com/gfx-rs/wgpu) 30 (Vulkan/Metal/DX12) ·
[winit](https://github.com/rust-windowing/winit) 0.30 · [glam](https://github.com/bitshifter/glam-rs) ·
serde + JSON · [hecs](https://github.com/Ralith/hecs) · `image` for PNG export.

hecs over `bevy_ecs` was a deliberate churn tradeoff: `bevy_ecs` breaks every Bevy release, and this
project already spent a build cycle on wgpu's API churn. Full reasoning, including what it gives up,
is in the design doc §9.

## CLI surface (v1 target)

| Command | Purpose |
|---|---|
| `engine build` | Compile; report structured errors |
| `engine validate <scene.json>` | Schema-check a scene |
| `engine run-scene <scene.json>` | Windowed viewer |
| `engine screenshot <scene.json> --out out.png [--camera Player] [--width W --height H]` | Headless render to PNG — the key agent feedback tool |
| `engine list-components` | Dump all component schemas as JSON |
| `engine diff-render <scene.json> <baseline.png> --out diff.png` | Pixel-diff against a baseline |

## Roadmap

- [x] **M0** — Window + triangle. Proves the graphics stack end to end.
- [ ] **M1** — CLI skeleton: `build`, `run-scene`, `screenshot`. Establish the JSON error convention.
- [ ] **M2** — JSON scene loading, hecs integration, Transform/Mesh/Camera, schema export.
- [ ] **M3** — Asset pipeline: glTF meshes, textures.
- [ ] **M4** — Materials + lighting: a PBR-ish material component, one directional light.
- [ ] **M5** — Validation hardening. Deliberately *not* last-priority work — structured validation
      is as load-bearing here as rendering features.
- [ ] **M6** — Diff-render / visual regression tooling.

## Non-goals for v1

No GUI editor. No networking or multiplayer. No advanced rendering (GI, ray tracing) — a clean
forward renderer with basic PBR is enough to prove the concept. Desktop only; no mobile or console
targets.

## Open questions

Deliberately unsettled, and interacting with each other:

- Runtime scripting (Lua/Rhai) vs. compiled-Rust systems only — this bounds how much an agent can
  iterate on without a full rebuild.
- Whether to support hot reload of scene data without a Rust rebuild. Likely high value for agent
  iteration speed, and the one argument strong enough to reopen the ECS choice.

`agent-native-engine-design.md` is the source of truth for layout, formats, and build order.
`CLAUDE.md` carries the working notes an agent needs before touching this code.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
