# Frame cost (M15)

*Relocated verbatim from `CLAUDE.md` when it was compacted to an index. The digest that points here is `CLAUDE.md` §Frame cost.*

The viewer was slow for reasons that had **nothing to do with particles** — measured on an M3 Pro at
2560×1440, the smoke costs ~0 ms/frame even with the camera inside the plume, while the frame was
spending ~29 ms in `hud::rasterize` and ~4 ms rebuilding GPU resources. Three fixes, none of which
moves a pixel:

1. **The HUD rasterizes only what it covers** — elements are measured, overlapping ones grouped, and
   each group gets a canvas at its bounding box blitted under a scissor rect (`HudOverlay` /
   `HudCanvas { origin_x, origin_y, .. }`, `shaders/hud.wgsl` takes the origin as a uniform);
   overlapping elements still accumulate in one linear-space buffer and quantize once, so stacked
   translucency is untouched.
2. **GPU resources persist across frames** — `SceneRenderer::draw` takes `&mut self` and keeps
   uploaded geometry (keyed on the `Arc<MeshData>` identity, evicted after 240 idle frames), one
   object-uniform buffer addressed by dynamic offset instead of a buffer + bind group per entity, and
   grown-in-place frame/particle/HUD buffers.
3. **`MeshSource::load_mesh` returns `Arc<MeshData>`** and implementations must return the *same*
   `Arc` for one asset — that is both the end of the per-frame deep copy in `Scene::render_items` and
   the cache key in (2); a reloaded file mints a new `Arc` and re-uploads.

`particles.wgsl` also discards fragments whose final alpha is exactly 0, which is bit-identical
because `src·0 + dst·1` is `dst`. Net: ~34 ms → ~0.9 ms per frame in release, ~173 ms → ~2.2 ms in
debug. **The viewer draws an FPS readout** (`app.rs::with_fps_readout`, averaged over 0.5 s) — it
rides ordinary `HudText`/`HudRect` components appended to the scene's own HUD, and headless renders
never see it, so nothing reproducible depends on how fast this machine drew.

## The rasterizer M15 made small but not cheap

The showcase tour ran at ~100 fps in a debug viewer, and **45% of the frame was `hud::rasterize`** —
5.35 ms of an 11.84 ms frame to paint a 348×89 card and an 88×8 label, 31,676 pixels, **0.86% of the
window**. M15 fixed the *area* the HUD rasterizes and left the per-pixel cost alone, which stopped
mattering right up until it was the only CPU work left: every other phase of the frame is now a
tenth of a millisecond (`render_items_at` 0.13 ms, the recipe item lists 0.10 ms, the GI fold
0.35 ms), because M15's caches all still work.

Four changes, none of which moves a pixel — 47 of 47 committed artifacts came back byte-identical in
an A/B against a `main` binary, **including all six tour frames, which normally flake**:

1. **`decode_srgb` is a 256-entry table.** Its whole domain is a byte, so the table *is* the
   function — same expression, same inputs, same bits. `draw_image` was calling it three times per
   texel it sampled.
2. **`Region::spans` replaces per-pixel `Region::index`.** One intersection of the element box with
   the region and the target, then a row base — instead of an `Option<usize>` per pixel, unwrapped
   and discarded. Same pixels, same order.
3. **A nine-slice's source *column* is resolved once per column, not once per pixel.** It does not
   depend on the row, and a card frame has the same few hundred columns all the way down.
4. **The encode carries the previous pixel's bytes across a run of bit-identical pixels**, and
   writes into a pre-sized buffer instead of growing it four bytes at a time. A HUD canvas is mostly
   runs — a flat panel band, the inside of a glyph, the transparent margin no element reached — and
   the encode is three `powf`s a pixel.

Measured at 2560×1440 with `cargo run -p engine-cli --example frame_bench`:

| | before | after |
|---|---|---|
| `hud::rasterize`, debug | 5.35 ms | **1.75 ms** |
| `hud::rasterize`, release | 0.90 ms | **0.15 ms** |
| whole frame, debug | 11.84 ms | **7.9 ms** |
| whole frame, release | 5.46 ms | **4.4 ms** |

In the viewer that is a median **~100 → ~240 fps** in debug on an M3 Pro, measured by alternating the
two rasterizers within one session rather than across two. The gap between that 2.4× and the bench's
1.5× is the caveat in `frame_bench`'s own header: the bench drains the GPU every frame and a window
pipelines, so the viewer tracks `max(cpu, gpu)` — the tour was CPU-bound and is now **GPU-bound**, at
the 4.2 ms of fill this scene costs at 4× MSAA.

Three things the measurement settled that are worth keeping:

- **`powf` was 65% of the release rasterize and only 20% of the debug one.** The rest of debug's cost
  was the unoptimized loop around it — `Option`s, calls that do not inline, per-pixel capacity
  checks. Optimising a CPU path for the profile the agent loop actually runs (`bin/engine` is debug
  by default) is not the same job as optimising it for release, and only the debug profile says
  which is which. `#[inline(always)]` on the leaf helpers is honoured at `-O0`.
- **The GPU cost has no single owner.** Removing meadows, trees, clouds, water or roads one at a
  time each saved ≤0.35 ms of 4.24 ms; MSAA 4→1 saved 1.0 ms and shadows-off 0.4 ms. It is
  distributed fill, not a hog, and there is nothing to fix there short of culling.
- **The simulation is not the problem and never was**: 900 steps of the tour — four Rhai scripts,
  physics, particles — is 0.67 ms/step in release and 2.6 ms/step in debug.

`crates/engine-cli/examples/frame_bench.rs` is how all of the above was measured, kept because the
next person to ask "why is this slow" should not have to rebuild it. It is an example rather than a
subcommand for the same reason the FPS readout is viewer-only: nothing reproducible may depend on
how fast this machine drew.
