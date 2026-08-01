# HUD (M11.6 lines + M12 components)

*Relocated verbatim from `CLAUDE.md` when it was compacted to an index. The digest that points here is `CLAUDE.md` §HUD.*

Two layers, one render path. `world.hud(text)` pushes printable-ASCII debug lines, cleared every step
— the line HUD is a pure function of the step that drew it — and `world.state(key, default)` /
`world.set_state(key, value)` is numeric per-run memory on the ScriptHost (replay-deterministic,
reset by a fresh run, deliberately *not* baked — same disposability as solver caches). Caps 16 lines
× 96 chars, runtime error beyond.

**`HudText` / `HudRect` components** are screen-anchored (anchor enum + pixel offset measured inward;
five anchors), pixel-sized, schema-validated (size/color/opacity ranges, anchor typos get
`did_you_mean`), need no Transform and ignore the camera. Text snaps to integer scales of the 8×8
font (`size` 16 = 2×), colors are linear RGB, draw order is rects-then-texts in file order, and the
`world.hud` line panel draws topmost with its original layout formulas.

Rendering is `engine-render/src/hud.rs`: **one** CPU rasterizer (unit-tested without a GPU) producing
a target-sized sRGB straight-alpha canvas that `SceneRenderer` composites as a sampler-less
fullscreen-triangle blit (`ScenePass.hud`) — `offscreen::render` and the `run-scene` viewer share it,
so the played game and the pinned PNG show the same overlay; an empty HUD draws nothing, keeping
every pre-HUD baseline byte-identical. Scripts drive components via `world.hud_text`/`set_hud_text`
and `world.hud_rect_size`/`set_hud_rect_size`; changed `HudText.text` / `HudRect.size` bake under the
change-based rule (unlike `world.hud` lines, which are per-step output). The line HUD stays
observable without pixels: `simulate`/`screenshot` report the final step's lines as `"hud"`, and
`--trace` logs `{"step", "hud"}` on every change. Fixture: `verify/m12_hud.json`. `car.rhai` shows
the applied version — speedometer, lap timer (start-line crossing remembered step-to-step via
`world.state`), and a `SpeedBar` HudRect gauge.
