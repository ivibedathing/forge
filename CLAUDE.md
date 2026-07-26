# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Current state

**M0 is done.** The Cargo workspace exists and renders a triangle, verified by pixel readback.
Everything after M0 in the milestone list is unbuilt — most of the CLI table below does not exist
yet.

What works today:

```
engine run [--width 1280 --height 720]   # windowed viewer, M0 triangle. No scene loading yet.
engine info                              # selected GPU adapter as JSON
```

Read `agent-native-engine-design.md` before making structural decisions; it is the source of truth
for layout, formats, and build order, and several choices in it are still open (§9).

## Dependency versions — check before trusting recall

wgpu moves fast and breaks its API every release. This workspace is on **wgpu 30**, which differs
sharply from the 25-and-earlier APIs most training data describes:

- `Surface::get_current_texture` returns a `CurrentSurfaceTexture` enum, **not** a `Result`.
  Variants include `Suboptimal`, `Occluded`, and `Validation`.
- Presentation is `Queue::present(texture)`, not `SurfaceTexture::present()`.
- Push constants are gone; `PipelineLayoutDescriptor` has `immediate_size: u32`.
- `multiview` is `multiview_mask: Option<NonZeroU32>` on both pipeline and render pass.
- `Instance::new` takes `InstanceDescriptor` **by value**, and it has no `Default` — use
  `InstanceDescriptor::new_without_display_handle_from_env()`.
- `RequestAdapterOptions` requires `apply_limit_buckets`.

When touching wgpu, read the API in `~/.cargo/registry/src/*/wgpu-30.0.0/src/` rather than writing
from memory. winit is pinned to the **0.30** stable line; 0.31 is still beta.

## Verification

`cargo test --workspace` is the real check, not `cargo build`. `crates/engine-render/tests/
headless_render.rs` renders offscreen and asserts on pixel values, because "the window opened and
did not crash" does not distinguish a working renderer from a culled triangle or a shader that
writes nothing. Those tests skip cleanly (rather than fail) when no GPU is available.

Backface culling is **on**, and the M0 triangle is wound counter-clockwise in clip space to match
wgpu's default front face. A wrongly-wound triangle renders nothing at all — if geometry is
invisible, suspect winding before suspecting the pipeline.

## What this project is

An agent-native 3D engine in Rust: a game engine whose primary "user" is an AI coding agent rather
than a human in a GUI editor. The design constraint driving every other decision is the agent
feedback loop — edit a text file → validate → build → render a PNG headlessly → *look at it* →
iterate — using only ordinary bash and file edits, with no bespoke integration layer.

This inverts the usual engine tradeoff: machine-legibility beats GUI convenience wherever the two
conflict.

## Architecture

Planned Cargo workspace (design doc §4), dependency order bottom-up:

- `crates/engine-core` — ECS, scene graph, math re-exports (glam)
- `crates/engine-render` — wgpu renderer, shaders, materials
- `crates/engine-assets` — mesh/texture loading, asset schema
- `crates/engine-cli` — the `engine` binary; the primary interface

Supporting: `schemas/component-schema.json` (generated, not hand-written), `examples/scenes/*.ron`,
`docs/component-reference.md` (generated from doc comments).

Stack: Rust + wgpu (Vulkan/Metal/DX12) + winit + glam + serde/RON + `image` for PNG export.
ECS crate is `hecs` or `bevy_ecs` — **undecided**, see below.

## Non-negotiable invariants

These are what make the engine agent-operable. Violating one breaks the core premise, so raise it
with the user rather than working around it:

1. **No binary scene or asset-metadata formats.** Scenes, materials, and prefabs are RON/JSON and
   git-diffable by construction.
2. **No hidden state.** Everything needed to reconstruct a scene lives in text files on disk. No
   editor-only in-memory state; no opaque GUIDs without an in-repo lookup table.
3. **Assets are referenced by relative path, never by opaque ID.**
4. **Entities have stable `name` fields** — CLI commands and agent edits target them by name.
5. **Components are plain data.** All logic lives in systems.
6. **Errors are structured JSON on stderr, with a non-zero exit code.** Include file/line/field and
   a `did_you_mean` when a name is close to a known one:
   ```json
   {"error": "unknown_component", "entity": "Cube1", "component": "Meterial", "did_you_mean": "Material"}
   ```
   Implemented as `EngineError` in `crates/engine-core/src/error.rs`; use it rather than inventing
   a second error type. Optional context is boxed to keep the struct small, since it rides in every
   `Result` including the per-frame render path — reach for `EngineError::context()` to read it
   back. `suggest_from` fills `did_you_mean` by Levenshtein distance.
7. **Component schemas are derived from Rust structs via serde**, never maintained by hand, and
   scene files are validated against them.
8. **A GUI editor, if it ever exists, is a view onto the text files** — never a second source of
   truth.

## Commands (target CLI — mostly not yet present, see "Current state")

```
engine build                                    # compile; structured errors
engine validate <scene.ron>                     # schema-check a scene
engine run-scene <scene.ron>                    # windowed viewer
engine screenshot <scene.ron> --out out.png [--camera Player] [--width 1280 --height 720]
engine list-components                          # dump all component schemas as JSON
engine diff-render <scene.ron> <baseline.png> --out diff.png
```

`engine screenshot` is the single most important command in the project — it is what closes the
agent's edit→see loop. Prioritize it accordingly; keep it headless and keep it fast.

Standard Cargo commands apply once the workspace exists (`cargo build`, `cargo test`,
`cargo test -p engine-core <test_name>` for a single test).

## Build order

Follow the milestones in design doc §8: ~~M0 window+triangle~~ (done) → M1 CLI skeleton + JSON
error convention → M2 RON scenes + ECS → M3 glTF/texture assets → M4 materials + lighting → M5
validation hardening → M6 diff-render. M5 is deliberately *not* last-priority work; structured
validation is as load-bearing as rendering features here.

M1's `engine screenshot` is mostly plumbing that already exists: `Renderer::draw` takes any
`TextureView`, `Gpu::new` takes an optional surface, and the readback path (texture → buffer →
pixels) is written in `tests/headless_render.rs`. Lifting that into the CLI is the work. One thing
the test dodges: it uses a 256px-wide target so rows are already 256-byte aligned. Arbitrary
`--width` values need real `COPY_BYTES_PER_ROW_ALIGNMENT` padding and unpadding.

## Open decisions — ask, don't assume

Design doc §9 lists choices the user wants to settle deliberately. If a task forces one, surface it
rather than picking silently:

- RON vs JSON for scene files (decide before M2)
- `hecs` vs `bevy_ecs`
- Runtime scripting (Lua/Rhai) vs compiled-Rust systems only
- Whether to support hot reload of scene data without a Rust rebuild

## Out of scope for v1

GUI editor, networking/multiplayer, advanced rendering (GI, ray tracing), mobile/console targets.
Desktop only.
