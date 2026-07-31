# UI system — Design Document (M31)

Companion to `agent-native-engine-design.md`, and the successor to
`hud-design.md`. M12 gave the engine two screen-space primitives — a solid
rectangle and a line of pixel-font text — placed by an anchor and a pixel
offset. That was enough to read state out of a running scene. It is not enough
to build a *screen*: a menu, a dialog, an inventory, a title card, anything a
player clicks. This milestone closes that gap: layout, images, and the widgets
a pointer lands on.

Written 2026-07-31, after M26; **revised after M30**, which is when it was
built. The revision is not cosmetic and §7 records it: this document was
drafted before M28, and M28 shipped the pointer it had planned to invent.
Everything below now consumes the landed one rather than building a second.

## 1. The shape of the problem

Three things are wrong with the HUD as a UI system, and all three show up in the
scenes already in the repo.

**Every box is hand-computed.** `arena_shooter.json` positions six `HudText`s
and two `HudRect`s by writing pixel offsets that were derived, by hand, from
glyph widths and from each other. The health bar's background and its fill are
two rects whose offsets must agree; when the bar moved, both moved. Nothing in
the file says they belong together, so nothing can move them together. A dialog
box with a title, a body, and two buttons is a dozen of those relationships, and
authoring it means holding the whole layout in your head and writing down only
its solved output. The file stops explaining the screen.

**There is no image.** The only fill is a flat linear-RGB colour. M26 gave the
engine a texture pipeline, mip chains, and an `(asset, space)` cache; none of it
reaches the overlay. A framed panel, an icon, a health-bar cap, a logo — all of
them are currently a mesh in front of the camera or they do not exist.

**Nothing can be clicked.** M28 put a cursor and three buttons on the
timeline, so the engine knows where the player is pointing — but nothing on
screen knows it is *pointable*. A script can read `world.cursor_x()` and
compare it against numbers it computed itself, which is the hand-computed-box
problem again wearing a different hat. What is missing is the element saying
"I am a hit box", and the engine answering "the pointer is on you".

The constraint that shapes every answer below is the same one that shaped M12:
**an agent builds this UI without ever seeing it move.** It edits JSON, renders
a PNG, and looks. So the layout has to be inspectable as text, the pointer has
to be authorable as text, and the engine has to be willing to say — in a report,
not in an image — where every element ended up.

## 2. Core principles

1. **The existing HUD is the special case, not the legacy case.** Every rule
   here is written so that a scene with no new field renders byte for byte as it
   does today. Not "close enough to re-bless" — identical, by construction, and
   the argument for each rule is given where the rule is. Eleven baselines
   survived M16 this way. Six committed scenes carry HUD components and ten
   baselines render them — `m11_lap`, `m12_hud`, `m16_environment`, both
   `m28_pointer_*`, and the six `showcase_*` — and of those ten only the
   showcase six may move, because §14 re-authors that scene's HUD deliberately.
2. **Layout is a pure function of (file, viewport size).** No incremental
   layout, no dirty flags, no measurement cache that could go stale. Given the
   scene and a width and height, the pixel rectangle of every element is
   determined — which is what makes it a thing the CLI can print.
3. **The pointer is M28's, unchanged.** Not one line of `input.rs` is edited by
   this milestone. "Where is the cursor during step N, and which buttons are
   down" was answered there; this milestone only asks what the cursor is *on*.
4. **Interaction is polled, not dispatched.** Scripts ask `world.clicked(name)`;
   the engine never calls into a script. Reasons in §8.
5. **Hierarchy is by name, in a flat file.** A child names its parent, exactly
   as `Wheel.vehicle` names a chassis. No nested component trees, no second
   entity model, no ordering requirement between the two lines of JSON.
6. **The engine publishes what it laid out.** M23 learned this on roads: a
   generator that re-derives the curve is how two implementations start
   disagreeing about where the road is. `engine road-centerline` exists so
   nothing has to. `engine ui-layout` is the same command for the same reason —
   anything that wants to click a button needs the rectangle the engine put it
   in, not one it recomputed.

## 3. The component family

The family stays named `Hud*`. `UiPanel`/`UiText` would read better, and the
cost of getting there is renaming two components that four committed scenes and
five baselines depend on, plus every `world.set_hud_text` call in three scripts,
to buy a better word. Rejected. The name is now a slight misnomer — a pause menu
is not a heads-up display — and that is cheaper than the migration. `HudRect`
was always the primitive behind a panel background anyway.

Three components are new; the two that exist grow fields, all defaulted to
today's behaviour.

### `HudPanel` — the container

```json
{ "type": "HudPanel", "anchor": "center", "offset": [0, 0],
  "layout": "column", "padding": 12, "gap": 8, "align": "center",
  "color": [0.05, 0.06, 0.08], "opacity": 0.85 }
```

- `layout`: `free` (default) | `row` | `column`.
- `padding` (pixels, uniform), `gap` (pixels between children on the main
  axis), `align`: `start` (default) | `center` | `end` on the cross axis.
- `width` / `height`: optional pixel sizes. **Absent means hug contents** — the
  panel is exactly its children's extent plus padding. This is the field that
  removes hand-computed boxes, so it is the default.
- `color` + `opacity`: the panel's own background. `opacity` defaults to **0**,
  so a bare `HudPanel` is an invisible layout group; set it and the same
  component is the dialog's backdrop. One component, not a container plus a
  rect whose size has to be kept in agreement with it.

A uniform scalar `padding` rather than four sides: per-side padding is the
obvious next field and costs nothing to add later, and M12's "no z field until
something needs it" applies. Same for margins, which do not exist — `gap` and
`offset` cover what they would.

### `HudImage` — a textured rectangle

```json
{ "type": "HudImage", "texture": "textures/panel_frame.png",
  "anchor": "center", "size": [320, 180], "slice": [12, 12, 12, 12],
  "tint": [1.0, 1.0, 1.0], "opacity": 1.0 }
```

`texture` is a PNG path relative to the scene file, loaded through M26's
`TextureSource` in `ColorSpace::Srgb` — same loader, same `(asset, space)`
cache, same `texture_too_large` check firing from `validate` before a device
exists. Only the base level is read; the overlay never minifies (§6).

`slice` is a nine-slice inset in **source** pixels, `[left, top, right,
bottom]`, default `[0,0,0,0]` = plain stretch. `tint` multiplies the decoded
texel in linear space, so one grey frame texture serves a red panel and a blue
one — M26's authoring rule for `albedo_map`, repeated here for the same reason.

### `HudInteract` — the hit box

```json
{ "type": "HudInteract", "hover_tint": [1.2, 1.2, 1.2],
  "press_tint": [0.7, 0.7, 0.7], "disabled": false }
```

Carries no geometry. It makes the element **on its own entity** clickable, using
that element's laid-out rectangle as the hit box; an entity with a
`HudInteract` and no `HudPanel`/`HudRect`/`HudImage`/`HudText` is
`hud_interact_without_element`. A separate component rather than an
`interactive: true` flag on each of four components, because the flag would have
to be four fields that must stay in agreement, and because the tints belong next
to it.

The tints are multipliers on the element's own colour (clamped to `[0,1]` after
multiplying), default `[1,1,1]` — no change, so adding `HudInteract` to an
element moves no pixel until a cursor arrives. They exist so that the ordinary
case, a button that lights up under the pointer, needs no script at all.
Anything richer is a script writing colours through the API in §9.

### Fields added to `HudText` and `HudRect`

Shared by every element in the family, `HudPanel` and `HudImage` included:

- `parent`: the name of an entity carrying a `HudPanel`. Absent (the default)
  means the element is a child of the viewport.
- `visible`: `true` by default. A hidden element draws nothing and cannot be
  hit; a hidden panel hides its whole subtree. This is how a menu opens and
  closes — one boolean, script-settable, bakeable under the change-based rule.
- `stretch`: `[bool, bool]`, default `[false, false]`. On an axis where it is
  true the element fills the parent's content box on that axis, ignoring its own
  size. This is the full-screen dim backdrop, and the button that spans a
  column's width. It is two booleans rather than a `"fill"` string in a numeric
  field, because a union-typed field breaks the schema-driven validation walk
  and the editor's generated widgets.

`HudText` additionally gains `align` (`left` default | `center` | `right`,
within its own box, which only differs from the box when the text is stretched
or wrapped), `wrap` (a pixel width, `0` = no wrapping = today), and `line_gap`
(pixels between lines, default 0). `text` may now contain `\n`. `HudRect` gains
nothing beyond the shared fields; it stays the flat script-driven bar it is.

## 4. Layout

Layout runs on the CPU in `engine-core`, as a pure function
`layout(&HudTree, width, height) -> Vec<PlacedElement>`, unit-testable with no
GPU and no renderer — the same split `hud.rs` and `diff.rs` already have.

**Resolution order.** Children are measured bottom-up (a hugging panel needs its
children's sizes), then placed top-down (a child's position needs its parent's
box). Text measures as `chars * 8 * scale` per line by the existing formula;
wrapping breaks on spaces, and a single word longer than `wrap` overflows rather
than splitting — a mid-word break in a fixed-width font reads as corruption.

**Free layout** places each child by its own `anchor`/`offset` **relative to the
parent's content box**, using M12's inward-offset rule unchanged. With no
parent, the content box is the viewport, so an unparented element is placed by
exactly the arithmetic in `anchored_box` today.

**Row and column** stack children along the main axis in tree order, separated
by `gap`, aligned on the cross axis by `align`. A child's `anchor` is ignored
here and its `offset` is applied as a nudge on top of the computed position —
useful, harmless, and it means a child moved into a panel does not silently jump
because its anchor stopped meaning anything.

**Cycles and depth.** `parent` is checked for cycles (`hud_parent_cycle`,
naming the whole ring) and for a nesting depth cap of 16
(`hud_nesting_too_deep`). Both are validation errors, not runtime guards — a
hung layout with no output is the worst failure an agent loop can hit, which is
the same argument `tree_too_complex` makes.

**Rounding** happens once, at the end, per element: positions and sizes are
computed in f32 and rounded to whole pixels exactly where `anchored_box` rounds
today. Rounding at each level of the tree instead would let a nested element
drift by a pixel per level.

### Draw order, and why it does not move anything

Today's rule: all `HudRect`s in file order, then all `HudText`s in file order.
`m12_hud.json` pins overlapping rect-under-text ordering deliberately.

The new rule: **depth-first over the tree; a panel draws its own background
before its children; among siblings, order by (class, file order)** where
`HudPanel`/`HudRect`/`HudImage` are class 0 and `HudText` is class 1.

In a scene where nothing names a parent, every element is a sibling at the root,
so the order collapses to (class, file order) — which is the old rule verbatim.
That is the whole compatibility argument, and it is structural rather than
empirical: there is no arrangement of pre-M31 components that the two rules
order differently. The `world.hud` debug-line panel still draws last, over
everything.

## 5. Text

Text stays the public-domain 8×8 pixel font at integer scales, unfiltered. That
is a deliberate hold. Real typography means a TTF rasterizer, which means
anti-aliased coverage from a third-party crate — and a coverage value that
differs by one between crate versions moves every baseline with text in it,
which is most of them. The engine's whole verification story rests on glyph
pixels being fully on or fully off.

The upgrade that keeps that property is a **bitmap font atlas**: a PNG plus a
small in-repo JSON describing glyph cells and advances, sampled nearest like
every other overlay texture. It needs no new dependency and no float. It is
sketched here and deferred (§13) only because this milestone is already large;
when it lands it is a `font` field on `HudText` whose absence is the 8×8 font.

## 6. Images

The overlay rasterizes on the CPU, so `HudImage` samples on the CPU, and the
sampler is **nearest-neighbour**, written out in `engine-core` like every other
generator in this repo. Three reasons, in order of weight: a render sits under a
baseline, so the filter is a format contract and cannot be a dependency's
business; nearest is exactly reproducible on every machine, where a bilinear
filter is a float-rounding question; and nine-slice exists precisely so that a
frame's corners are never scaled, which removes most of what filtering would
have been for.

Nine-slice cuts the source into nine regions by `slice`; corners are copied at
1:1 when the destination allows and clamped when it does not, edges tile along
their axis, the centre tiles both ways. Tiling rather than stretching, because
tiling at nearest is exact and stretching at nearest is a moiré pattern. A
`slice` whose insets exceed the source dimensions is
`hud_image_slice_too_large`, reported from `validate` with the source size in
the message.

Sampling reads the base mip only. The overlay draws at most one destination
pixel per texel band and never minifies below it, so a mip chain would only
introduce a level-selection decision with no correct answer at this scale.

## 7. The pointer — already built, and not the way this document planned

This section is kept as a record of a reversal, because the reasoning that
replaced it is worth more than the paragraph it replaced.

**As drafted**, this milestone was going to put the cursor on the timeline in
**framebuffer pixels**, with each keyframe declaring a `viewport: [w, h]`, a
separate `"buttons": ["Left"]` array, and an `input_viewport_mismatch` error
(exit 2) when a cursor-bearing timeline was replayed into a framebuffer of a
different size. The argument was that a pixel-authored UI is
resolution-dependent by construction, so a normalized cursor would produce
silent misclicks.

**M28 shipped the pointer first, and chose the opposite on every point**
(`designs/mouse-input-design.md`): the cursor is a **fraction of the frame**,
origin top-left; buttons ride the *same* `held` set the keys do, as
`MouseLeft`/`MouseRight`/`MouseMiddle`; an absent cursor is the **centre** of
the frame, so every pre-M28 timeline parses unchanged; and no keyframe declares
a viewport, because a fraction does not need one.

**M28 is right, and not merely first.** A timeline outlives the window it was
recorded in — that is the whole reason input is a text artifact — and a
recording made at 2560×1440 that replays as garbage at 960×540 is exactly the
failure this engine exists to avoid. The draft's `input_viewport_mismatch`
would have *reported* that failure rather than removing it, and the cost is a
timeline pinned forever to one window size. Splitting `held` in two was the
weaker half of the same idea: a keyframe is one complete snapshot of what the
player is doing, and two arrays means every line that changes a button must
restate the keys or grow a merge rule.

What the draft got right is that **the concern is real** — it just belongs to
the *layout*, not to the timeline. A cursor at 0.9 of the frame is on a
right-anchored button at every size; a cursor over a 132×26 plate is not, and
M28's own CLI test already documents exactly that (960×540 misses the arena
fixture's plate that 640×360 hits). So the resolution-dependence lives where it
is visible and queryable: `engine ui-layout --width W --height H` reports the
rectangle, and a UI that must survive resizing is authored with `stretch` and
hug sizing rather than with pixel offsets. That is the same answer §1 gives for
every other hand-computed box.

**So this milestone edits no input code at all.** It consumes
`input::Pointer`'s cursor, multiplies by the frame it is laying out against, and
hit-tests. Scripts keep `world.mouse`, `cursor_x`/`cursor_y`,
`viewport_width`/`viewport_height` and `cursor_ground` exactly as M28 defined
them; `world.cursor()` as a *pixel* accessor is not added, because two spellings
of one quantity in different units is how a script starts disagreeing with the
engine about where the pointer is.

The consequence for hit-testing is one rule: **the frame a UI is hit-tested
against is the frame the command is rendering**, which M28 already settled —
`screenshot` passes its own size, `diff-render` the baseline's, and
`simulate`/`raycast` `Viewport::DEFAULT` (960×540). A `simulate` run and a 16:9
screenshot therefore agree on which button the cursor is over whenever the
button is placed proportionally, and disagree when it is placed in pixels, which
is a property of the scene and is reported by `ui-layout` rather than hidden.

## 8. Interaction

Hit-testing runs once per fixed step, **before scripts**, against the layout for
that step's viewport — the cursor being M28's fraction multiplied by that
frame. Candidates are entities carrying a visible, non-disabled `HudInteract`;
the hit is the **last one in draw order** whose rectangle contains the cursor,
so topmost wins and a modal panel with a `HudInteract` swallows clicks to what
is under it, while one without is click-through. That makes "does this menu
block the game" an authored property rather than an accident.

State per element, derived from the pointer and the previous step:

| Call | True when |
|---|---|
| `world.hovered(name)` | the cursor is over it this step |
| `world.pressed(name)` | a button went down over it and is still held |
| `world.clicked(name)` | the button was released this step, over the same element it was pressed on |

The press-capture (which element owns the in-flight press) is one entry of
runtime state on the UI system, of the same kind as `world.state` and rapier's
contact state: replay-deterministic, reset by a fresh run, and deliberately
**not baked** — a half-finished click is not a property of the scene. `clicked`
is edge-shaped and true for exactly one step, which is the one thing a polled
API cannot derive for itself without knowing the capture.

**Why polling and not callbacks.** An `on_click: "start_game"` field was the
obvious alternative and is rejected on three counts. A scene has many `Script`
entities and the field would have to say which one owns the handler, which is a
second addressing scheme for something the engine already addresses by entity
name. Dispatch order between a handler and the ordinary `step` function becomes
a rule someone has to remember, and mid-step reentrancy becomes a thing that can
happen. And `world.key` set the precedent in M11 §6 for exactly this shape:
"one predicate, not an axis/action abstraction — bindings are game logic, and
game logic lives in scripts." A button that runs code is a binding.

Scripts already have the raw pointer for aiming and for anything the widget
model does not cover — `world.mouse`, `cursor_x`/`cursor_y`,
`viewport_width`/`viewport_height`, `cursor_ground` — and this milestone adds
nothing there (§7). No screen-to-world picking beyond M28's ground plane:
`engine raycast` is where that question is answered, and a second answer in the
script API is how two of them start disagreeing.

## 9. Script API

Additions to the curated `world`, all runtime-erroring on a missing entity or
component with `did_you_mean`, like every accessor since M12:

| Call | Meaning |
|---|---|
| `world.hovered(n)` / `pressed(n)` / `clicked(n)` | interaction state (§8) |
| `world.hud_visible(n)` / `set_hud_visible(n, b)` | show/hide an element or a subtree |
| `world.hud_color(n)` / `set_hud_color(n, r, g, b)` | colour of any HUD element |
| `world.hud_opacity(n)` / `set_hud_opacity(n, o)` | opacity of any element |
| `world.hud_size(n)` / `set_hud_size(n, w, h)` | generalizes `set_hud_rect_size` to panels and images |

`set_hud_rect_size` stays, as the name three committed scripts already call, and
so does M28's `hud_offset` / `set_hud_offset` pair — which this document's draft
listed as new, having been written before M28 added it to draw a crosshair. The
raw pointer accessors are M28's and are not restated here (§7).

Colour setters *clamp* to `[0,1]` and opacity errors on NaN/overflow at the
call, following M17's split for lights: a clamp for a value with an obvious
nearest legal answer, an error for one without.

All of these bake under the change-based rule — a `visible`, `color`, `size` or
`offset` differing from the file's rest value is spliced back like a moved
`Transform`. Hover and press state do not bake, per §8.

## 10. `engine ui-layout`

```
engine ui-layout <scene.json> [--width W --height H] [--entity Name]...
```

Reports, name-sorted, every HUD element's resolved rectangle in framebuffer
pixels, its parent, its visibility, and whether it is interactive:

```json
{"viewport": [1280, 720], "elements": [
  {"entity": "StartButton", "kind": "HudPanel", "parent": "MenuColumn",
   "rect": [552, 372, 176, 40], "visible": true, "interactive": true}]}
```

This is the command the milestone is really for. An agent authoring a menu
cannot see it move; what it needs is the answer to "where did the Start button
end up", so it can write `{"step": 120, "cursor": [640, 392], "buttons":
["Left"]}` and know the click lands. `engine road-centerline` publishes the
samples the ribbon was built from for exactly this reason, and the failure it
prevents — a generator re-deriving geometry the engine already computed, and the
two drifting — is the same failure. A report, so it does not pretty-print
(M24's output-shape rule).

It is a pure function of (file, viewport) at rest, like `engine inspect`: no
`--steps`, no `--input`, no cursor. What a script has since done to the layout
is `simulate`'s question.

## 11. Validation

New codes, all `Input` class (exit 1) unless noted:

| Code | Fires when |
|---|---|
| `hud_parent_not_found` | `parent` names no entity — with `did_you_mean` |
| `hud_parent_not_panel` | `parent` names an entity with no `HudPanel` |
| `hud_parent_cycle` | the parent chain loops; the message names the ring |
| `hud_nesting_too_deep` | more than 16 levels |
| `hud_interact_without_element` | `HudInteract` with nothing to hit |
| `hud_image_slice_too_large` | nine-slice insets exceed the source image |

Two codes the draft listed are **not** here: `unknown_mouse_button` shipped in
M28 as part of the held-set allowlist, and `input_viewport_mismatch` does not
exist because a fractional cursor cannot mismatch a framebuffer (§7).

Everything else — field types, ranges, unknown fields, enum typos on `layout`,
`align` and `anchor` — comes free from the schema-driven walk, which is the
point of that walk. `padding`, `gap`, `line_gap` and `wrap` carry
`#[schemars(range(min = 0.0))]`; tints are unbounded above (a hover tint
brightens) and floored at 0.

`unused_material`-style warnings are not extended here: an element with no
parent and no visible pixels is a legitimate work-in-progress, and warning about
it would fire on every hidden menu in the repo.

## 12. Rendering

Almost nothing changes below the layout layer, which is the design working.

`Scene::hud_items` grows into `Scene::hud_tree` — the same name-sorted plain-data
extraction, plus each element's parent link and the resolved image handles from
`TextureSource`. `hud::rasterize` keeps its signature shape (a pure function of
content and dimensions) and gains two element kinds to draw; the existing
measure → cluster → draw-per-cluster machinery from M15 is unchanged, because
the clustering only ever cared about pixel boxes and layout produces pixel
boxes. Panels and images are rect-shaped, so they cluster like rects.

The GPU side is untouched: one sampler-less blit per canvas through
`shaders/hud.wgsl`, exactly as M15 left it. No new pipeline, no new bind group —
which matters, because M26 spent the last of the four `downlevel_defaults` bind
groups. Image texels are sampled on the CPU and land in the same canvas as
everything else.

**The editor still passes no overlay.** Its orbit camera is not the game frame,
and a screen-anchored UI drawn over a viewport with a different aspect ratio
would mislead about the very thing this milestone makes checkable.
`engine screenshot` at a stated `--width`/`--height` is where a UI is verified,
and `engine ui-layout` is where it is queried.

## 13. Non-goals

Named, so the next session does not have to guess whether they were forgotten:

- **TTF text.** §5; the bitmap-font atlas is the sanctioned next step.
- **Pointer lock, relative mouse, scroll wheel.** Mouselook needs sub-step
  deltas and a capture model; it is its own design.
- **Text input, focus, keyboard navigation.** A text field needs a caret,
  selection, an IME story and a character-level input stream, none of which the
  W3C-code allowlist can express.
- **Percentage sizes, flex-grow, wrapping rows, grids.** `stretch` covers the
  full-bleed cases and hug covers the rest; anything past that should arrive
  with a scene that needs it.
- **Rounded corners, gradients, shadows, blur.** All of them are
  anti-aliasing at the pixel level, which is where the CPU rasterizer's
  bit-exactness lives.
- **Animation of UI fields by clips.** Clips animate `Component.field` already
  and will reach these fields for free where the field is numeric; nothing
  special is being built for it.
- **World-space UI** (a health bar over an enemy's head). That is a projection
  question — screen position from a world position — and it belongs to the
  script API as `world.project(x, y, z)` some other milestone.
- **A second UI in the editor.** §12.

## 14. Verification

- **`verify/m31_ui.json` + `verify/m31_ui.input.jsonl` + a baseline.** A menu:
  a stretched dim backdrop, a nine-sliced framed panel hugging a column of a
  title, two lines of wrapped body text, and two buttons with hover and press
  tints, over a small 3D scene so compositing is exercised. The timeline moves
  the cursor onto the second button and presses it, so the blessed frame shows a
  pressed button — the state hardest to reach and the one nothing else pins.
  Rendered at a fixed `--steps` and an explicit `--width`/`--height`, pinned by
  `engine diff-render`; the cursor being a *fraction* (§7), the timeline needs
  no viewport of its own, but the render size is still part of what the baseline
  means and is recorded in the manifest. Aimed at its subject with no terrain in
  frame, per M22's rule, so it carries a hard bit-exact pin.
- **Layout unit tests, GPU-free**: hug sizing bottom-up, row/column/free
  placement, `align` on the cross axis, `stretch` on each axis independently,
  nested rounding, wrapping including the over-long word, and the draw-order
  collapse — a scene with no parents ordered by the new rule must equal the old
  rule's order, asserted directly rather than through a render.
- **Rasterizer unit tests**: nine-slice at exact, undersized and oversized
  destinations; nearest sampling exactness; tint multiplication in linear space.
- **Interaction unit tests, GPU-free**: press-inside/release-outside does not
  click; press-inside/release-inside does, for exactly one step; topmost wins;
  a disabled or hidden element is not a candidate; a panel with `HudInteract`
  blocks and one without does not.
- **`engine ui-layout`** against the fixture, including an element whose
  reported rect is then used to derive the cursor *fraction* that hits it — the
  loop the command exists to close, closed in a test, and the one place the
  pixel report and the fractional timeline have to agree.
- **`showcase_tour.json` gains the new components**, per
  `repo_contracts.rs::showcase_tour_uses_every_component_the_engine_has`. A
  station card — a nine-sliced `HudPanel` hugging a `HudText` station name and a
  `HudImage` icon, driven by the director script that already knows which
  station is on screen — replaces the four hand-positioned `HudText`s. Six
  showcase baselines re-bless in that commit; nothing else may move.
- **`bin/verify-baselines`** clean on all 33 committed baselines other than
  those six, and an **A/B between binaries** on the pre-M31 scenes: the overlay
  path is being restructured under scenes whose HUD pixels are pinned, and a
  baseline diff cannot prove no pixel moved when six of the baselines are
  expected to move.

## 15. What this milestone would let someone build

The measure of it: `arena_shooter.json`'s HUD becomes two panels — a column for
score and wave, a row for the health bar and ammo — with no pixel offsets in it
except the two margins; the arena gains a start screen and a death screen that
are `visible` toggles on two panels; and the demo timeline gains four lines that
click through them. None of that requires an engine change after this one.
