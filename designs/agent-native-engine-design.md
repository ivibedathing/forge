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
   or asset metadata. JSON for scenes, materials, prefabs. Git-diffable
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
- **Serialization:** serde + JSON for scene/asset files (resolved, see §9)
- **Image I/O:** image crate (screenshot export)
- **ECS:** hecs (lightweight, not the full Bevy engine — resolved, see §9)
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
      demo_scene.json
  docs/
    scene-format.md             # the rules the schema cannot express
    component-reference.md      # generated from doc comments
```

Both `docs/` entries above were unbuilt from M0 to M31 and are now real, on the terms this section
already set: `component-reference.md` is **generated** by `engine list-components --markdown` from
the same schema the flagless form publishes, and pinned by `repo_contracts.rs` — a hand-maintained
copy of the schema would violate invariant #7. `scene-format.md` is prose, because what it covers
is what the schema *cannot* say: internal tagging, entity names as addresses, asset paths relative
to the scene file, which components own their own geometry, and the cost of JSON having no
comments. Its worked example is checked by a repo-contract test rather than trusted, since the
example in a format document is the first thing anyone copies.

## 5. Scene File Format (JSON)

Format resolved 2026-07-27: **JSON**, not the RON originally sketched here. See §9.

```json
{
  "name": "demo_scene",
  "entities": [
    {
      "name": "Player",
      "components": [
        { "type": "Transform",
          "position": [0.0, 1.0, 0.0],
          "rotation": [0.0, 0.0, 0.0, 1.0],
          "scale":    [1.0, 1.0, 1.0] },

        { "type": "Mesh", "asset": "meshes/capsule.glb" },

        { "type": "Camera", "fov": 60.0, "near": 0.1, "far": 1000.0, "active": true }
      ]
    },
    {
      "name": "Cube1",
      "components": [
        { "type": "Transform",
          "position": [0.0, 3.0, 0.0],
          "rotation": [0.0, 0.0, 0.0, 1.0],
          "scale":    [1.0, 1.0, 1.0] },

        { "type": "Mesh", "asset": "meshes/cube.glb" },

        { "type": "Material",
          "albedo": [1.0, 0.0, 0.0],
          "metallic": 0.0,
          "roughness": 0.8 }
      ]
    }
  ]
}
```

Design notes:
- Components are **internally tagged** on `"type"`, so a component is one flat object rather than a
  nested single-key wrapper. This is the shape `jq` is pleasant against and the shape an LLM is
  least likely to get wrong.
- Every entity has a stable `name` used for CLI targeting (`engine edit-entity Cube1 --set position=0,5,0`) as a convenience layer over direct text edits.
- Components are plain data; all engine logic lives in systems, not on components.
- Assets are referenced by relative path, never by opaque ID.
- JSON has no comments. Anything a scene needs to say about itself has to be a real, schema'd field
  — this is the accepted cost of the format choice.

## 6. CLI Surface (v1 target)

| Command | Purpose |
|---|---|
| `engine build` | Compile the project, report structured errors |
| `engine validate <scene.json>` | Check scene against component schemas, report structured errors |
| `engine run-scene <scene.json>` | Launch windowed viewer for a scene |
| `engine screenshot <scene.json> --out out.png [--camera Player] [--width 1280 --height 720]` | Headless render to PNG — the key agent feedback tool |
| `engine list-components` | Dump schema of all registered components as JSON |
| `engine diff-render <scene.json> <baseline.png> --out diff.png` | Pixel-diff current render vs a baseline, for regression checks |

All commands exit non-zero on failure and print errors as JSON to stderr:
```json
{"error": "unknown_component", "entity": "Cube1", "component": "Meterial", "did_you_mean": "Material"}
```

## 7. The Agent Feedback Loop

This is the core workflow the whole design serves:

1. Claude Code edits a `.json` scene file or a Rust system/component.
2. Runs `engine validate scene.json` — fast structural check.
3. Runs `engine build` — compiler + engine build errors surface as structured JSON.
4. Runs `engine screenshot scene.json --out /tmp/check.png`.
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
3. **M2 — Scene format + ECS.** JSON scene loading, hecs integration,
   Transform + Mesh + Camera components, schema export.
4. **M3 — Asset pipeline.** glTF mesh loading, basic texture loading.
5. **M4 — Materials + lighting.** PBR-ish material component, one directional
   light, basic Phong or simplified PBR shader.
6. **M5 — Validation + structured errors everywhere.** Make `engine validate`
   and `engine build` genuinely agent-friendly (this is as important as
   rendering features — don't leave it to the end).
7. **M6 — Diff-render / visual regression tooling.**
8. **M7 — GUI editor.** A visual editor as a *view* onto the same text files
   (per §2.3) — never a second source of truth. Edits made in the GUI are
   writes to the scene JSON, so agent and human workflows stay interchangeable.
9. **M8 — Physics.** Rigid bodies and colliders as plain-data components,
   simulated by systems.
10. **M9 — Animation.** Skeletal/keyframe animation, building on the glTF
    asset pipeline from M3.
11. **M10 — Scripting.** Runtime scripting layer — depends on resolving the
    Lua/Rhai vs. compiled-Rust-systems question in §9.

## 9. Open Design Questions (to resolve early, with Claude Code)

### Resolved

**Scene format — JSON** (2026-07-27). The deciding argument was §1's own success criterion: the
agent works "using ordinary bash + file edits," and `jq` is ordinary bash whereas RON has no
equivalent. Secondary: §6's `component-schema.json` validates JSON files natively, so the schema
and the scene are one serialization rather than two, and external tools can validate without a Rust
parser. Third: the primary user is an LLM, which edits JSON more reliably than RON.

Given up: RON's comments and lighter punctuation. RON remains the nicer format to hand-edit; this
engine optimizes for the agent, per §2.

**ECS — `hecs` 0.11** (2026-07-27). Chosen over `bevy_ecs` mainly to limit churn exposure. M0
already lost a build cycle to wgpu's API breaking between major versions; `bevy_ecs` is 0.19,
breaks every Bevy release, and requires a much newer MSRV. Measured: hecs pulls 6 transitive deps
and cold-builds in ~1.2s, `bevy_ecs` pulls 128 and takes ~12.3s. v1's system count doesn't justify
a scheduler.

Given up: `bevy_ecs` change detection, which would make hot reload (below) easier. If hot reload
becomes a priority this is the decision to revisit.

### Still open

- How much of a "runtime scripting" layer to add (Lua/Rhai) vs. everything
  being compiled Rust systems — affects how much an agent can hot-iterate
  without a full rebuild.
- Whether to target a live "hot reload" workflow (re-run without recompiling
  Rust when only scene data changes) — likely high value for agent iteration
  speed. Note this interacts with the ECS choice above.

## 10. Non-Goals (for v1)

- No visual/GUI editor (text files + CLI only, for now).
- No networking/multiplayer.
- No advanced rendering (GI, ray tracing) — a clean forward renderer with
  basic PBR is enough to prove the concept.
- No mobile/console targets — desktop (Windows/Mac/Linux) only.
