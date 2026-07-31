# Mouse input — Design Document (M28)

Companion to `input-design.md`, which is the source of truth for how input reaches this engine
at all. That document's §7 says, in as many words, **"No mouse, gamepad, or text input"**. This
one reverses exactly the first of those three and nothing else. Written 2026-07-31, after M26.

## 1. Why now

The arena shooter (`designs/arena-shooter.md`) aims with the arrow keys, which is twin-stick on
hardware that has no second stick. Every top-down shooter ever written aims where the player is
pointing, and a menu is a thing you click. Both wants are the same missing primitive: **a point
on the frame, and a button**.

The reason it was a non-goal in M11 is worth restating, because it is the thing this design has
to answer rather than dodge. Keyboard input is a *set of names*, and a set of names is trivially
a text artifact: `{"step": 40, "held": ["KeyW"]}` is legible, hand-authorable, and replays to
the same pixels forever. A cursor is a *continuous coordinate measured in a window*, and windows
are exactly the thing this engine refuses to let gameplay depend on. If the answer had been
"pixels in whatever window you happened to have open", the mouse would have stayed out.

## 2. Principles

These are M11's, unchanged, and every decision below is one of them applied:

1. **Sampled per fixed step, never per event.** A script asks where the cursor was *during step
   N* and whether a button was down. No motion events, no click edges, no double-click timer.
2. **Headless input is a text artifact.** The cursor and the buttons ride the same
   `*.input.jsonl` timeline the keys do, so a mouse-driven play session records, replays,
   traces, screenshots and diff-renders like everything else.
3. **No input means no buttons held and the cursor at the centre of the frame.** Every timeline
   committed before this milestone parses unchanged and means exactly what it meant; every
   baseline is untouched.
4. **Scripts are the only consumer.** The cursor is transient by nature and has no business in
   a scene file (invariant 2), so it never becomes a component.
5. **Unknown names fail structurally, with `did_you_mean`.** `world.mouse("MouseLeftt")` is a
   located runtime error, not a button nobody ever presses.

## 3. The wire format

Two additions to the timeline line, both optional:

```jsonl
{"step": 0,  "held": [],                        "cursor": [0.5, 0.5]}
{"step": 40, "held": ["KeyW"],                  "cursor": [0.62, 0.41]}
{"step": 60, "held": ["KeyW", "MouseLeft"],     "cursor": [0.62, 0.41]}
```

**Buttons live in `held`, beside the keys.** `MouseLeft`, `MouseRight`, `MouseMiddle` join
`KNOWN_KEYS`' sibling `KNOWN_BUTTONS`, and a timeline line accepts either kind. The alternative
— a second `"buttons"` array — was rejected because a keyframe is *one complete snapshot of what
the player is doing*, and splitting it into two arrays means every line that changes a button
must also restate the keys or grow a merge rule. The namespace still splits, but at the query:
`world.key` takes key names only and `world.mouse` takes button names only, each rejecting the
other kind with a suggestion. That way a script that asks for `world.key("MouseLeft")` is told
what it did wrong instead of quietly reading `false` forever.

**The cursor is normalized to the frame, origin top-left, `[0, 1]` on each axis.** Not pixels: a
timeline outlives the window it was recorded in, and a recording made on a 2560×1440 display
that replays as garbage at 960×540 is exactly the failure this engine exists to avoid. Top-left
because that is where the HUD's pixel space starts (`HudAnchor::TopLeft` measures inward from
it) and where window events arrive — `cursor_x() * viewport_width()` is the pixel a menu button
is hit-tested against, with no flip to forget. Values outside `[0, 1]` are clamped at parse: the
pointer left the window, and a keyframe that says `1.4` means "off the right edge", which the
engine reads as the right edge rather than as a ray into nothing.

**An absent `cursor` is the centre of the frame**, not "whatever the previous keyframe said".
Keyframes are complete snapshots in M11 and stay complete here; a carry-over rule would make
line 40 unreadable without line 0. The cost is that a hand-authored timeline which moves the
cursor once must repeat it on every subsequent line, which is what a generator or a recording
does anyway.

## 4. The cursor is a point on the frame; the *ray* is the engine's job

A script that aims at the world does not want a screen coordinate — it wants "the point on the
ground under the pointer". Deriving that in Rhai means an inverse projection, which means the
camera's field of view, its model matrix, and the viewport aspect, none of which the script API
exposes. So the engine resolves it, once per step, in one place:

```
Pointer::resolve(cursor, viewport, camera, camera_model) -> { cursor, viewport, ray }
world.cursor_ground(y)   // where that ray crosses the horizontal plane at height y
```

`Pointer` is computed by the *caller* of `ScriptHost::step` — the same place that already knows
which camera it is about to render through — rather than by the script host reaching into the
world for a camera. That keeps the script host free of camera-selection policy (`--camera`
belongs to the command) and makes the viewer and the headless path provably identical: both call
`scene.camera(name)` and hand the result to the same resolver.

The ray formula is the inverse of `scene_renderer::view_projection`, and the two are pinned
against each other by a test in `engine-render` (which is the only crate that can see both): a
cursor projected back through the renderer's own matrix must land where it started, at the
centre and at all four corners. Writing the inverse in `engine-core` rather than sharing code
with the renderer is the `water.wgsl`/`mesh.wgsl` precedent — engine-core cannot depend on
engine-render, and an agreement test is what keeps two spellings of one transform honest.

**`cursor_ground` on a scene with no camera is a runtime error**, in the shape M21 settled for
`world.time_of_day()` on a scene with no `daylight` block: a script asking where the pointer is
in a scene that has no view is a bug, and inventing an answer hides it. A ray that runs parallel
to the plane or away from it is *not* an error — it returns the point at
`Pointer::MAX_GROUND_DISTANCE` (500 m) along the ray's horizontal projection, so a camera tipped
at the horizon degrades to "very far away" instead of to a NaN that lands in a `Transform`.

## 5. What this makes resolution-dependent, stated rather than discovered

The ray through a cursor depends on the viewport **aspect**. So a mouse-driven run is a function
of `(scene, steps, timeline, width, height)` where a keyboard-driven one was a function of the
first three. This is not new in kind — `HudText` and `HudRect` are pixel-sized, so a HUD has
always been laid out against the frame — but it is new in reaching *gameplay*, and it is why:

- `screenshot` and `diff-render` pass their real frame size (diff-render's is the baseline's own
  dimensions, which it already reads before rendering), so a fixture pinned by a baseline is
  pinned at the size it was blessed at.
- `simulate` and `raycast` render nothing and have no size to pass. They use
  `Viewport::DEFAULT`, **960×540**, a documented constant rather than an accident. A generator
  that drives a mouse-aiming script through `simulate` and then screenshots the result at some
  other aspect will aim differently in the two runs — that is the honest consequence, and the
  fix is to screenshot at 960×540 or to add `--width/--height` to `simulate` later.
- The viewer uses the live window size and re-resolves it on every resize, so dragging a window
  wider widens the view rather than moving the crosshair.

## 6. The script API

| Call | Meaning |
|---|---|
| `world.mouse(name)` | `true` if button `name` is down during this step |
| `world.cursor_x()` / `world.cursor_y()` | `[0, 1]` across the frame, origin top-left |
| `world.viewport_width()` / `world.viewport_height()` | the frame in pixels — what turns the cursor into HUD pixels |
| `world.cursor_ground(y)` | `[x, y, z]`: where the cursor's ray meets the horizontal plane at height `y` |
| `world.hud_offset(name)` / `world.set_hud_offset(name, x, y)` | read/move a `HudText` or `HudRect` — how a crosshair follows the cursor |

`set_hud_offset` is the one addition that is not about the mouse as such. A HUD that can be
resized (`set_hud_rect_size`, M12) and re-worded (`set_hud_text`) but not *moved* cannot draw a
crosshair, and a crosshair is the minimum feedback a pointing device needs. It bakes under the
change-based rule like every other script-driven component field.

There is deliberately **no** `world.set_cursor(...)`, no cursor-visibility control, and no
pointer capture. All three write to the window, and the window is the one thing in this engine
that a headless run does not have; a script that moves the pointer would replay differently
under `--input` than it played, which is the whole property this design is protecting.

## 7. Recording

`run-scene --record-input` writes a line whenever the *sampled* input changes, where the cursor
is sampled **quantized to three decimals** (about one pixel across a 960-wide frame). Without
quantization every sub-pixel tremor of a hand on a mouse is a keyframe and a fifteen-second
session records a line per step; with it, a still hand records nothing. Three decimals is the
same reasoning M25 applied to the frame digest — a number an agent reads or commits has to be
quantized against the noise in how it is produced — and it is a *format* decision, since the
quantized value is what the file says and therefore what replays.

A mouse session still records far more lines than a keyboard one. That is inherent: a cursor is
a continuously varying input and its timeline is a motion capture. The arena shooter's canned
demo is 14 lines with keys and ~80 with a mouse, which is still a file a human can read.

## 8. The viewer

`WindowEvent::CursorMoved` gives a position in physical pixels; dividing by the window's inner
size gives the normalized cursor, clamped. `WindowEvent::MouseInput` maps
`MouseButton::{Left, Right, Middle}` onto the three button names; other buttons are dropped the
way keys outside the allowlist already are. `WindowEvent::CursorLeft` is deliberately *not*
handled — the cursor keeps its last position when it leaves the window, because a menu button
under a pointer that slid out of frame should not appear to be clicked at the centre of the
screen.

## 9. Not here

- **No scroll wheel and no relative motion.** Relative motion is what a first-person mouselook
  needs, and it also needs pointer capture, a sensitivity convention, and a story for what a
  headless replay does with an uncaptured pointer. That is its own milestone.
- **No click edges.** `world.mouse` is a held-state predicate, exactly like `world.key`; a
  script that wants "the frame the button went down" compares against what it stored in
  `world.state` last step. Two menus in this repo do it in three lines.
- **No mouse in the editor.** The editor already has one, through egui; this is the *player's*
  mouse.
- **No cursor in `filmstrip`.** It samples animation time, not steps, and takes no `--input`.
