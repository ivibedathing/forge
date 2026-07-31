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

In practice that means: JSON scenes validated against generated schemas, headless PNG rendering
and pixel-diffing against committed baselines, deterministic physics with traceable and bakeable
simulation, property animation, sandboxed Rhai scripting with replayable input timelines, and a
GUI editor that is strictly a live view onto the scene file — all driven from the `engine` CLI.

## The agent feedback loop

The whole design serves this cycle:

1. Edit a `.json` scene file, or a Rust component/system.
2. `engine validate scene.json` — fast structural check against the component schemas.
3. `engine build` — compiler and engine errors surface as structured JSON.
4. `engine screenshot scene.json --out /tmp/check.png` — headless render.
5. **View the PNG.** Claude Code can read images directly.
6. Iterate. No human in the loop.

`engine screenshot` is the single most important command in the project — it is what closes the
loop. `engine diff-render` turns step 5 into a regression test: this scene must look like a
committed baseline, within tolerance (bit-exact by default). Determinism is promised
same-machine and same-adapter, so a baseline is an artifact of the machine that blessed it —
bless your own rather than expecting someone else's to match.

## Install

Rendering needs a GPU with a Vulkan, Metal, or DX12 backend. `engine info` reports
which adapter was selected.

```bash
curl -fsSL https://raw.githubusercontent.com/ivibedathing/forge/main/install.sh | sh
```

That drops the `engine` binary in `~/.local/bin` — no Rust toolchain involved.
Set `FORGE_INSTALL_DIR` to put it somewhere else, or `FORGE_VERSION` to pin a
release. Prebuilt binaries exist for macOS (arm64, x86_64) and Linux x86_64;
Windows x86_64 ships as a zip on the [releases page](https://github.com/ivibedathing/forge/releases).

From source, if you have Rust:

```bash
cargo install --git https://github.com/ivibedathing/forge engine-cli --locked
```

(Not on crates.io: the editor pins egui to a git rev while the released line
still pairs with an older wgpu, and crates.io refuses any crate with a git
dependency.)

## Quick start

```bash
engine init my-scene && cd my-scene
engine validate first.json
engine screenshot first.json --out /tmp/first.png --steps 120
```

Then open `/tmp/first.png`. `engine init` writes a starter scene, a script, and
the agent orientation as both `AGENTS.md` and `CLAUDE.md` — so pointing Claude
Code, Codex, or any agent at that directory is the whole setup. The same
orientation prints from `engine agent-guide`, and `engine list-components`
dumps every component's schema, so an agent can discover the entire scene
format from the binary alone.

## Working on the engine itself

```bash
git clone https://github.com/ivibedathing/forge
cd forge

cargo build
bin/engine validate examples/scenes/demo_scene.json
bin/engine screenshot examples/scenes/demo_scene.json --out /tmp/demo.png

cargo test --workspace
```

`bin/engine` is a development shim, not something an installed copy has — it
runs the CLI without cargo's tax by checking whether any source is
newer than the binary (~0.02s warm), rebuilds only if so, and execs. `cargo run -p
engine-cli --` spends ~8s on freshness checking before every single call, warm
— which is nothing once and is most of a milestone across the hundreds of
validate/screenshot/diff-render calls the loop actually makes. Arguments pass
through untouched, so the contract in `docs/cli-contract.md` describes both.

`bin/verify-baselines` re-diffs every committed baseline against the scene and
flags that produce it, from the manifest in `examples/scenes/verify/baselines.json`:

```bash
bin/verify-baselines                      # check them all, NDJSON out, exit 1 on drift
bin/verify-baselines --filter m19         # one milestone
bin/verify-baselines --bless --filter m19 # re-bless, after an intended change
```

`cargo test --workspace` is the real check, not `cargo build`: the render tests draw offscreen and
assert on actual pixel values, skipping cleanly on machines with no usable GPU.

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
8. **A GUI editor is a view onto the text files** — never a second source of truth.

## Architecture

```
crates/
  engine-core/     ECS, scene graph, validation, structured errors, format-preserving writes
  engine-render/   wgpu renderer, PBR shaders, materials, diff-render comparison
  engine-assets/   glTF mesh + PNG texture loading — the only crate that opens asset files
  engine-physics/  rapier3d integration: deterministic stepping, trace, bake, raycast
  engine-script/   sandboxed Rhai scripting — the per-step `world` API
  engine-editor/   egui GUI editor — a live, writable view onto the scene file
  engine-cli/      the `engine` binary — the primary interface
schemas/           component + animation JSON Schemas, generated from Rust structs
examples/          demo scenes, meshes, scripts, input timelines
docs/              cli-contract.md, error-codes.md
```

**Stack:** Rust · [wgpu](https://github.com/gfx-rs/wgpu) (Vulkan/Metal/DX12) ·
[winit](https://github.com/rust-windowing/winit) · [glam](https://github.com/bitshifter/glam-rs) ·
serde + JSON · [hecs](https://github.com/Ralith/hecs) · [rapier3d](https://rapier.rs) ·
[Rhai](https://rhai.rs) · [egui](https://github.com/emilk/egui) · `image` for PNG export.

## Non-goals

No networking or multiplayer. No advanced rendering (GI, ray tracing) — a clean forward renderer
with basic PBR is enough to prove the concept. Desktop only; no mobile or console targets. The GUI
editor stays deliberately small: it is a convenience view for supervising an agent, not the primary
interface, and features land there only after the CLI can do the same thing headlessly.

`designs/agent-native-engine-design.md` is the source of truth for layout, formats, and build order.
`CLAUDE.md` carries the working notes an agent needs before touching this code.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
