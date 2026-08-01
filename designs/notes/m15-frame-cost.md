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
