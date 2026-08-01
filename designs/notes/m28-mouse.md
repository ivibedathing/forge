# The mouse (M28, `designs/mouse-input-design.md`)

*Relocated verbatim from `CLAUDE.md` when it was compacted to an index. The digest that points here is `CLAUDE.md` §The mouse.*

*The design doc for this milestone is `designs/mouse-input-design.md` — it has the rejected
alternatives; this file has what the build learned.*

M11's §7 said "no mouse"; this reverses that one item and nothing else. **Buttons ride the same
`held` set the keys do** (`MouseLeft`/`MouseRight`/`MouseMiddle`, own allowlist so `world.key` and
`world.mouse` each reject the other kind *naming the call that would have worked*), and the cursor
is a `"cursor": [x, y]` **fraction of the frame**, origin top-left — not pixels, because a timeline
outlives the window it was recorded in. **An absent `cursor` is the centre of the frame**, so every
pre-M28 timeline parses unchanged; recorded cursors quantize to three decimals (`CURSOR_SCALE`,
written as a scale and not a step of 0.001, or the file says `0.41300002`).

- **The cursor is a point on the frame; the *ray* is the engine's job.** `input::Pointer::resolve`
  is computed by the **caller** of `ScriptHost::step` — the code that already knows which camera it
  is about to render through — so the script host holds no camera-selection policy and the viewer
  and the headless path provably agree. Scripts get `world.mouse`, `cursor_x`/`cursor_y`,
  `viewport_width`/`viewport_height`, and `cursor_ground(y)`; a scene with no camera makes
  `cursor_ground` a **runtime error** (M21's precedent for `time_of_day` without a `daylight`
  block), while a ray that never meets the plane degrades to `MAX_GROUND_DISTANCE` rather than NaN.
- **The ray is the inverse of `scene_renderer::view_projection`, written out longhand in
  engine-core**, which cannot depend on engine-render — so `engine-render/tests/pointer.rs` is the
  agreement test (project a cursor's ray back through the renderer's own matrix; it must land where
  it started, at the centre and all four corners, at several distances and two aspects).
- **A mouse-driven run is a function of the frame size**, which no earlier input was. `screenshot`
  passes its own size, `diff-render` the baseline's, and `simulate`/`raycast` — which render
  nothing — `Viewport::DEFAULT`, **960×540**. Same aspect ⇒ same ray, so `simulate` and a 16:9
  screenshot aim identically; a *pixel-sized* HUD hit test is another matter and the M28 CLI test
  documents exactly that (960×540 misses the arena fixture's 132×26 plate that 640×360 hits).
- **`set_hud_offset` / `hud_offset`** (either HUD component, offsets mean the same on both) is the
  one non-mouse addition: a HUD that could be resized and re-worded but not *moved* cannot draw a
  crosshair. It bakes change-based like every other script-driven field.
- The viewer maps `CursorMoved` against the window's inner size and drops buttons outside the three;
  **`CursorLeft` is deliberately unhandled** — a pointer that slid out of frame must not read as a
  click at the centre of the screen. The recorder compares **quantized** states, so a still hand
  records nothing, and its "an initial empty set is implicit" rule now compares against the whole
  default state, or the first mouse movement of a session (which happens before any button) is lost.

Fixture `verify/m28_pointer.json` + timeline, **two baselines from one file** (`--steps 40` and
`--steps 80`). Not here: scroll wheel, relative motion / pointer capture (which is what a
first-person mouselook needs, and it wants its own milestone), click edges (`world.state`, two
lines), and cursor visibility control.
