# Agent-Native 3D Engine — Design Document

## 1. Vision

Build a 3D game engine designed from day one to be operated by an AI coding
agent (Claude Code) as its primary "user," not a human using a GUI editor.
Instead of bolting an MCP integration onto an existing engine, the engine's
native representation — scenes, assets, build process, and feedback loop —
is text-first, CLI-driven, and machine-legible, so an agent can read, edit,
build, run, and *see the result* of its own changes without any custom
tooling glue.

Success criterion: Claude Code should be able to say "add a red cube 2 units
above the player" and be able to (a) find the right file, (b) make a valid
edit, (c) build, (d) render a screenshot, and (e) confirm visually — all
using ordinary bash + file edits, no bespoke integration required.

## 2. Core Principles

1. **Text-first, human/agent-readable formats everywhere.** No binary scene
   or asset metadata. RON or JSON for scenes, materials, prefabs. Git-diffable
   by construction.
2. **No hidden state.** Everything the engine needs to reconstruct a scene
   lives in text files on disk. No editor-only in-memory state, no opaque
   GUIDs without a lookup table in the repo.
3. **CLI is the primary interface.** The engine is a library + a CLI tool.
   A GUI editor may come later, but it's a *view* onto the same text files,
   never a second source of truth.
4. **Headless render is a first-class feature.** `engine screenshot` renders
   a scene to a PNG from the command line. This is the single most important
   feature for closing the agent's edit→build→see loop.
5. **Structured, deterministic errors.** All build/validation/shader errors
   are printed as JSON to stderr with file/line/field info — parseable by
   an agent, not just readable by a human.
6. **Schema-driven components.** Every ECS component has a schema (derived
   from Rust structs via serde). Scene files are validated against this
   automatically, so the agent gets clear errors instead of silent failures.
7. **Small, composable CLI commands over one monolithic tool.** Each does one
   thing well and returns clean exit codes + structured output.

## 3. Tech Stack

- **Language:** Rust
- **Graphics API:** wgpu (targets Vulkan/Metal/DX12, cross-platform)
- **Windowing:** winit
- **Math:** glam
- **Serialization:** serde + RON (or JSON) for scene/asset files
- **Image I/O:** image crate (screenshot export)
- **ECS:** hecs or bevy_ecs (lightweight, not the full Bevy engine)
- **Build system:** Cargo (workspace with multiple crates, see below)

## 4. Workspace Layout

```
engine/
  Cargo.toml                  # workspace root
  crates/
    engine-core/               # ECS, scene graph, math re-exports
    engine-render/             # wgpu renderer, shaders, materials
    engine-assets/              # asset loading (meshes, textures), asset schema
    engine-cli/                 # the `engine` binary: build/run/screenshot/validate
  schemas/
    component-schema.json       # generated JSON Schema for all components
  examples/
    scenes/
      demo_scene.ron
  docs/
    scene-format.md
    component-reference.md      # auto-generated from doc comments
```

## 5. Scene File Format (example, RON)

```ron
(
    name: "demo_scene",
    entities: [
        (
            name: "Player",
            components: [
                Transform(position: (0.0, 1.0, 0.0), rotation: (0,0,0,1), scale: (1,1,1)),
                Mesh(asset: "meshes/capsule.glb"),
                Camera(fov: 60.0, near: 0.1, far: 1000.0, active: true),
            ],
        ),
        (
            name: "Cube1",
            components: [
                Transform(position: (0.0, 3.0, 0.0), rotation: (0,0,0,1), scale: (1,1,1)),
                Mesh(asset: "meshes/cube.glb"),
                Material(albedo: (1.0, 0.0, 0.0), metallic: 0.0, roughness: 0.8),
            ],
        ),
    ],
)
```

Design notes:
- Every entity has a stable `name` used for CLI targeting (`engine edit-entity Cube1 --set position=0,5,0`) as a convenience layer over direct text edits.
- Components are plain data; all engine logic lives in systems, not on components.
- Assets are referenced by relative path, never by opaque ID.

## 6. CLI Surface (v1 target)

| Command | Purpose |
|---|---|
| `engine build` | Compile the project, report structured errors |
| `engine validate <scene.ron>` | Check scene against component schemas, report structured errors |
| `engine run-scene <scene.ron>` | Launch windowed viewer for a scene |
| `engine screenshot <scene.ron> --out out.png [--camera Player] [--width 1280 --height 720]` | Headless render to PNG — the key agent feedback tool |
| `engine list-components` | Dump schema of all registered components as JSON |
| `engine diff-render <scene.ron> <baseline.png> --out diff.png` | Pixel-diff current render vs a baseline, for regression checks |

All commands exit non-zero on failure and print errors as JSON to stderr:
```json
{"error": "unknown_component", "entity": "Cube1", "component": "Meterial", "did_you_mean": "Material"}
```

## 7. The Agent Feedback Loop

This is the core workflow the whole design serves:

1. Claude Code edits a `.ron` scene file or a Rust system/component.
2. Runs `engine validate scene.ron` — fast structural check.
3. Runs `engine build` — compiler + engine build errors surface as structured JSON.
4. Runs `engine screenshot scene.ron --out /tmp/check.png`.
5. Views the PNG directly (Claude Code/Claude can view images).
6. Iterates based on what it sees, no human in the loop required.

Optional: `engine diff-render` lets the agent set up simple visual regression
tests ("this scene should always look like baseline.png ± tolerance") that
can run in CI.

## 8. Milestones (suggested build order)

1. **M0 — Window + triangle.** wgpu + winit boilerplate, clear color, one
   hardcoded triangle. Confirms the graphics stack works end to end.
2. **M1 — CLI skeleton.** `engine build`, `engine run-scene`, `engine screenshot`
   (even against a hardcoded scene). Establish the JSON error convention early.
3. **M2 — Scene format + ECS.** RON scene loading, hecs/bevy_ecs integration,
   Transform + Mesh + Camera components, schema export.
4. **M3 — Asset pipeline.** glTF mesh loading, basic texture loading.
5. **M4 — Materials + lighting.** PBR-ish material component, one directional
   light, basic Phong or simplified PBR shader.
6. **M5 — Validation + structured errors everywhere.** Make `engine validate`
   and `engine build` genuinely agent-friendly (this is as important as
   rendering features — don't leave it to the end).
7. **M6 — Diff-render / visual regression tooling.**
8. **M7+ — Physics, animation, scripting, editor UI (optional, later).**

## 9. Open Design Questions (to resolve early, with Claude Code)

- RON vs JSON for scene files (RON is more Rust-native and readable; JSON is
  more universally tooling-friendly — worth deciding before M2).
- ECS crate choice: `hecs` (minimal, you write more) vs `bevy_ecs` (more
  batteries, heavier dependency, pulls in some Bevy conventions).
- How much of a "runtime scripting" layer to add (Lua/Rhai) vs. everything
  being compiled Rust systems — affects how much an agent can hot-iterate
  without a full rebuild.
- Whether to target a live "hot reload" workflow (re-run without recompiling
  Rust when only scene data changes) — likely high value for agent iteration
  speed.

## 10. Non-Goals (for v1)

- No visual/GUI editor (text files + CLI only, for now).
- No networking/multiplayer.
- No advanced rendering (GI, ray tracing) — a clean forward renderer with
  basic PBR is enough to prove the concept.
- No mobile/console targets — desktop (Windows/Mac/Linux) only.
