# The UI system (M31, `designs/ui-system-design.md`)

*Relocated verbatim from `CLAUDE.md` when it was compacted to an index. The digest that points here is `CLAUDE.md` §The UI system.*

*The design doc for this milestone is `designs/ui-system-design.md` — it has the rejected
alternatives; this file has what the build learned.*

M12's two screen-space primitives were enough to *read* state out of a running scene and not enough
to build a **screen**. M31 adds the three things that were missing — layout, images, and widgets a
pointer lands on — and adds no input code at all, because M28 had already shipped the pointer.

**The design doc was drafted before M28 and planned to invent its own** (pixel cursors, a per-
keyframe `viewport`, an `input_viewport_mismatch` error). §7 is now a record of that reversal rather
than a specification: a timeline outlives the window it was recorded in, so M28's *fraction* is
right, and the draft's error code would have reported that failure rather than removed it. The
concern was real but belongs to the **layout** — which is what `engine ui-layout` answers.

- **`HudPanel` is the component that removes hand-computed offsets.** `layout` is `free`/`row`/
  `column`; **absent `width`/`height` means hug contents**, which is the default because it is the
  case that makes a dialog authorable. `opacity` defaults to **0**, so a bare panel is an invisible
  layout *group* and the same component is also the dialog's backdrop — one component, not a
  container plus a rect whose size has to be kept in agreement with it.
- **Hierarchy is a `parent` name in a flat file** (the `Wheel.vehicle` precedent), shared with
  `visible` and `stretch` by all four elements. Nothing structural guarantees it resolves, so five
  codes do: `hud_parent_not_found`, `hud_parent_not_panel`, `hud_parent_cycle` (the message names
  the ring), `hud_nesting_too_deep` (16), `hud_interact_without_element`. Cycles are a *validation*
  error rather than a layout guard — `tree_too_complex`'s argument — though `Structure::resolve`
  still roots an offending node so layout terminates on a scene that reached it anyway.
- **Two properties make the restructure free rather than merely cheap, and both are asserted rather
  than measured.** `ui::anchored` is M12's expression verbatim, so an unparented element resolves
  through arithmetic that is textually the same; and the new draw order (depth-first, a panel before
  its children, siblings by `(class, file order)`) **collapses** to "rects then texts, each in file
  order" when nothing names a parent. There is no arrangement of pre-M31 components the two rules
  sort differently. The A/B said **29 of 29** artifacts byte-identical between binaries.
- **Flow order is file order; draw order is `(class, file order)`.** Two orderings of one sibling
  set, and conflating them is a bug with a confusing symptom: the class sort exists so text reads
  over the backgrounds it sits on, and running a *column* in that order stacks every button above
  every label however the file reads. This cost a fixture render to find.
- **Layout runs in f32 end to end and rounds once per element, at emission.** Rounding per level
  would let a nested element drift a pixel per level. Hidden elements leave the flow entirely (they
  take no space) and a stretched child contributes nothing to a hugging parent — both so hug sizing
  cannot be circular. `stretch` on a `row`/`column`'s **main** axis is ignored, since distributing
  leftover space is flex-grow and that is a named non-goal.
- **`HudImage` samples nearest-neighbour, written out in-repo** for the reason every generator here
  is: a render sits under a baseline, so the filter is a format contract. Nine-slice copies corners
  1:1 and **tiles** edges and centre — tiling at nearest is exact where stretching at nearest is a
  moiré pattern. Only the base mip is read (the overlay never minifies below one destination pixel
  per texel band). `hud_image_slice_too_large` lives in **engine-assets**, not engine-core, because
  comparing an inset against its source needs the PNG decoded — the `texture_too_large` division.
- **Interaction is polled, never dispatched**: `world.hovered`/`pressed`/`clicked`. An `on_click`
  field would need a second addressing scheme for which `Script` owns the handler, a dispatch-order
  rule, and mid-step reentrancy; `world.key` set this shape in M11 because a button that runs code
  is a *binding*, and bindings are game logic. The **press capture** is the one thing a polled API
  cannot derive for itself, so the engine keeps it — runtime state of `world.state`'s kind:
  replay-deterministic, reset by a fresh run, **not baked**, since a half-finished click is not a
  property of the scene. Press-inside/release-outside does not click; `clicked` is true for exactly
  one step. **`MouseLeft` alone drives the widget model**, the other two staying raw.
- **Tints are applied to the extracted tree just before drawing, not inside the rasterizer** — the
  renderer has no business knowing what a pointer is, and `hud::rasterize` stays a pure function of
  (tree, lines, size). The `[1, 1, 1]` defaults make `apply_tints` a no-op for any scene with no
  cursor over an interactive element, which is why adding a `HudInteract` moves no pixel.
- **Hit-testing runs before scripts and is *not* gated on a scene having a `Script`** — hover and
  press tints are a property of the components, so a menu that lights up needs no script. It is
  gated on there being an overlay at all, so a scene without one takes the pre-M31 path exactly.
- **`engine ui-layout <scene> [--width W --height H] [--entity N]...`** is the command the milestone
  is really for: `road-centerline`'s argument applied to buttons. It reports the same rectangles the
  rasterizer draws from and the hit test uses, name-sorted, at rest (no `--steps`, no cursor) — and
  a CLI test closes the loop by turning a reported rect into the cursor *fraction* that hits it,
  which is the one place the pixel report and the fractional timeline have to agree.

**A trap worth knowing before authoring a fixture**: M28 defines an absent cursor as the **centre of
the frame**, so "no `--input`" is not the untouched case if anything interactive sits in the middle —
`verify/m31_ui.json`'s first button does, which is why its untouched-render test compares against
`--steps 0` instead.

Fixture `verify/m31_ui.json` + timeline at `--steps 30`, 640×360: a stretched dim backdrop, a
nine-sliced frame, a hugging column of title, wrapped centred body text, and two buttons — with the
**second held down**, the state hardest to reach and the one nothing else pins. Aimed at its subject
with no terrain in frame per M22's rule, so it carries a hard bit-exact pin (four consecutive renders
identical). Not here, deliberately: TTF text (a bitmap-font atlas is the sanctioned next step, and
the 8×8 font's fully-on-or-off glyph pixels are what the whole verification story rests on), pointer
lock and scroll, text input and focus, percentage sizes and flex-grow, rounded corners and shadows
(all anti-aliasing, which is where the CPU rasterizer's bit-exactness lives), and world-space UI.
