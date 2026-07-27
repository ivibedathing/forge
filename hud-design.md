# HUD design (M12)

Screen-space overlay for the agent-native engine: text labels and solid
rectangles, authored as ordinary components in the scene file, rendered
identically by `engine screenshot`, `engine diff-render`, and
`engine run-scene`, and drivable from Rhai scripts. The HUD is how a running
scene tells the agent (and the player) about state that has no 3D shape —
speed, lap counters, debug readouts.

Everything here follows the invariants: components are plain data, validated
by the generated schema; there is no runtime-only HUD state (what the file
says plus the script system's deterministic evolution *is* the HUD); a HUD
never depends on the camera.

## 1. Components

Two components, screen-anchored, sized in **framebuffer pixels**. Pixels
rather than normalized coordinates because they are what an agent sees in the
PNG: "the bar is 200px wide" survives a `jq` round trip and an
`engine diff-render` bounds report; a normalized value would re-raster text
at every resolution and make baselines resolution-relative. Anchors keep
HUDs usable across resolutions without normalized math.

```json
{ "type": "HudText", "text": "SPEED", "anchor": "top_left",
  "offset": [16, 16], "size": 16, "color": [1.0, 1.0, 1.0] }

{ "type": "HudRect", "anchor": "top_left", "offset": [16, 40],
  "size": [200, 12], "color": [0.2, 0.9, 0.3], "opacity": 1.0 }
```

- `anchor`: `top_left` (default) | `top_right` | `bottom_left` |
  `bottom_right` | `center`. `offset` is in pixels **inward** from the
  anchor: from a right anchor, `offset[0]` measures leftward; from a bottom
  anchor, `offset[1]` measures upward; from `center` it is the usual
  +x-right / +y-down applied to the element's center. The anchored point is
  the element's matching corner (its center for `center`).
- `HudText.size` is the glyph height in pixels. The built-in font is an 8×8
  pixel font, so rendering snaps to an integer scale factor
  `max(1, round(size / 8))` — authored `size: 16` means exactly 2× glyphs.
  Schema range: `size >= 4` (anything smaller than scale 1 is a lie).
- `HudRect.size` is `[width, height]` in pixels, each `>= 0` — zero is legal
  so a script-driven bar can be empty.
- Colors are **linear RGB in [0, 1]** like every other color in the engine,
  encoded to sRGB when the overlay is rasterized. `HudRect.opacity` is
  `[0, 1]`, default 1; `HudText` is always opaque (pixel font, no
  anti-aliasing — deliberately, for bit-exact baselines).
- A HUD entity needs no `Transform`; HUD components ignore one if present.

Draw order: all `HudRect`s in file order, then all `HudText`s in file order
— text always reads over bars, and within a class the file is the z-order.
No z field until something needs it.

## 2. Font

The built-in font is the public-domain 8×8 bitmap font (`font8x8` crate —
pure Rust arrays, no binary asset in the repo, no rasterizer dependency).
Glyphs outside its coverage render as a filled 8×8 box: visibly wrong in the
screenshot, never a panic. Text width is `len * 8 * scale`; no kerning, no
wrapping — a HUD line is one line.

This is deliberately a debug-quality font. The HUD's job is machine-legible
state readout; typography is out of scope until a real need shows up.

## 3. Rendering

The overlay is rasterized on the **CPU** into an RGBA8 buffer at framebuffer
size (`engine-render/src/hud.rs`, pure function of the HUD component list +
dimensions — unit-testable with no GPU, same philosophy as `diff.rs`), then
uploaded and composited by a single alpha-blended full-screen triangle pass
after the mesh pass. One small pipeline (`shaders/hud.wgsl`), no per-glyph
GPU work, and the windowed viewer, the editor viewport, and the offscreen
path share it by construction because they share `SceneRenderer`.

Compositing math: the canvas holds sRGB-encoded bytes; opaque pixels
(alpha 255) replace the destination byte exactly, alpha-0 pixels leave it
exactly, so text and default rects cannot introduce cross-run wobble.
Fractional `opacity` blends on the GPU and is deterministic per adapter —
the same promise every baseline already carries.

## 4. Scripts

The curated `world` API grows HUD accessors, runtime-erroring (exit 1,
`script_runtime_error`, `did_you_mean` on the entity name) like the
transform accessors when the entity or component is missing:

- `world.hud_text(name)` / `world.set_hud_text(name, text)`
- `world.hud_rect_size(name)` (returns `[w, h]`) /
  `world.set_hud_rect_size(name, w, h)`

That is enough for a speedometer readout and a speed bar. Color/offset
setters wait for a need.

Bake: HUD fields evolved by scripts follow the change-based rule — a
`HudText.text` or `HudRect.size` differing from the file's rest value is
spliced back like a moved `Transform`.

## 5. Verification

- `verify/m12_hud.json` + `verify/baselines/m12_hud.png`: every anchor, a
  glyph-coverage line, overlapping rect/text draw order, an opacity rect,
  and a script that writes a step counter into a `HudText` and stretches a
  `HudRect` — rendered at a fixed `--steps`, pinned by `engine diff-render`.
- CPU rasterizer unit tests (no GPU): anchor math at all five anchors,
  glyph bitmap exactness, scale snapping, rect bounds, draw order,
  out-of-bounds clipping.
- The car demo gains a speed readout + bar (`car_track.json` + `car.rhai`);
  `verify/baselines/m11_lap.png` is re-blessed in the same commit (the input
  timeline and physics are untouched — pixels change only where the HUD
  draws).
- Validation corpus grows HUD cases (bad anchor with `did_you_mean` via the
  schema enum, out-of-range size/opacity, unknown field).
