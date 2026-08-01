# Roads that can build a track (M40)

*Design doc: `designs/road-authoring-design.md` — it holds the rejected alternatives. This holds
what building it taught. The system it extends is `m23-roads.md`; read that first, because every
trap in it still applies.*

M23's five deferred items, all built: junctions, banked cross-sections, per-point width, roads that
follow a `Terrain`, and asphalt grain. **Everything defaults to M23**, and the A/B proved it: 34 of
34 comparable artifacts byte-identical between a `main` binary and this one, `m23_road.png`
included — the fixture that exercises `road.wgsl` end to end.

## The observation the milestone is built on

M23 already separated *where a vertex goes* from *what the shader is told about it*:

```rust
let offset = column * mitre;                     // the position widens
mesh.uvs.push([v[i], column]);                   // `u` does not
```

A mitred sharp vertex widens the cross-section by `1 / cos(turn/2)` while `uv[1]` keeps the nominal
column, so the shader's `|u| > half + shoulder` finds the skirt through a mitre with nothing extra
uploaded. **Per-point width is that same factor, multiplied in**; banking is a *rotation* of the
cross-section frame, which is rigid and changes no arc length at all. Neither one needed a vertex
channel, a uniform, or a shader change. That is why four of the five items are field additions and
only the junction is a new primitive.

The cost, and it is real: **paint scales with the road.** A section at 1.5× width wears a 1.5× wider
edge line, because `u` is one coordinate system and the markings are measured in it. Holding the
shoulder at a constant metre width instead would make `|u| > half + shoulder` a *different number at
different `v`*, which the shader cannot know without a third vertex attribute — a vertex-layout
change on a path flagged ULP-sensitive in four places, to buy a shoulder that stays 1.5 m instead of
becoming 2.6 m. Not worth it.

## What each piece is

- **Per-point width** — `RoadPoint.width: Option<f32>`. Absent is the road's own `width`.
- **Banking** — `RoadPoint.bank: Option<f32>` in degrees, positive raising the driver's **right**
  edge, falling through to `Road.auto_bank` + `Road.auto_bank_radius`. Auto-banking exists because
  **the sign is the hard part**: which way a corner banks is a fact about the winding of a polygon
  being edited beside it, every corner on a clockwise circuit takes a negative number, and one
  corner signed wrong is a circuit that throws the car off there. The engine raises the outside.
- **Terrain following** — `Road.follow_terrain: Option<String>`, plus `follow_smoothing` and
  `follow_blend`. Point `y` becomes a clearance above the ground; `RoadPoint.pin_height` makes one
  point absolute again.
- **Grain** — `Road.grain` / `Road.grain_scale`, a value-noise field in the road's own `(u, v)`.
- **Junctions** — the `Junction` component, a patch bounded by the mouths of the roads that reach
  it.

## One profile, three quantities

M23's `heights()` was a Fritsch–Carlson monotone cubic inlined into one function. Width and bank
want the same curve for the same reason — a linear ramp in width creases the road at every corner,
a linear ramp in bank steps the roll rate where the car is loaded up — so it came out as `Profile`,
with `corner_marks` producing the knots all three share.

**The extraction moved no expression, and that is checkable rather than hopeful**: Rust does not
contract float arithmetic into FMA without an explicit `mul_add`, so unlike this repo's WGSL splices,
CPU code motion here is exact by construction. `m23_road.png` staying bit-exact is the confirmation.

`width_scales` and `bank_angles` both return `Option`, and **`None` is not "all ones"** — the caller
skips the multiply and skips the rotation entirely. A road that authors neither reaches the vertex
arithmetic through the code M23 shipped, rather than through code that multiplies by an exact 1.0.
That is a structural claim about the default path instead of a numerical one, and it is the same
move the shader makes with grain.

## Terrain following, and the thing that had to be measured

The naive version — sample the ground down the centerline, smooth, add the authored clearance —
**punched a hole in the pit lane**. A level cross-section on sloping ground buries its uphill edge,
and this engine does not carve the terrain, so buried means the ground pokes *through* the asphalt.
It showed up as an irregular dark blob in the middle of the 13 m widened section, which is exactly
the class of bug a screenshot finds and a test does not.

**The fix: sample across the road, not only down its middle, and take the highest of the three** —
centre, left edge, right edge, at the local width. The road then rides the highest ground it covers,
which puts the *downhill* edge in the air, which is the failure `skirt` already exists to hide.

Two things follow from this and are worth knowing before changing it:

- **Width has to be profiled before height.** `followed_heights` needs the local cross-section
  extent, so `width_scales` moved ahead of it in `build`. Swapping them back samples the ground at
  the wrong offsets on any road that widens.
- **It gets worse the wider the road is**, which is why per-point width and terrain following had to
  land together rather than in either order.

Still deliberately absent: **carving.** A road writing back into the height field makes the rendered
ground a function of which other entities exist, which is the no-hidden-state invariant in a
costume. `Terrain` owns its grid. The residual is that a road across a sharp ridge can still cut
into it, and the authored answers are more clearance, a pinned point, or more `follow_smoothing`.

The smoothing filter walks at most `MAX_SMOOTHING_TAPS` (256) samples either side — a backstop, not
a policy, so a 100,000-sample road with a 5 km smoothing radius cannot turn a linear filter into a
quadratic one.

## Junctions

An arm names a road and which end of it arrives. The junction reads that road's **finished
`RoadSurface`** — the same geometry the renderer draws and physics builds its trimesh from — so it
cannot disagree with the road it joins about where the road ended, how wide it was, what height it
reached or how far it was banked. That is `engine road-centerline`'s rule applied inside the engine.

Four things the build taught:

- **Corners are transformed, not widths.** `resolve` maps the five points of the mouth (centre and
  four corners) through the road's model matrix and back through the junction's inverse. Carrying a
  scalar half-width across two transforms instead would be wrong for any placement with a scale, and
  subtly wrong for a yaw.
- **Orientation is measured, not derived.** `signed_area` decides whether the boundary loop needs
  reversing before the fan is emitted. Deriving it means reasoning about which way `atan2(z, x)`
  runs in a Y-up right-handed space, getting it wrong builds a patch that is **invisible under
  back-face culling**, and "the junction is invisible" and "the junction was never built" look
  identical in a screenshot. `CLAUDE.md` says to suspect winding first; this measures it instead.
- **The shoulder quad across a mouth is degenerate, and has to be skipped.** At a mouth the inner
  ring is the road's asphalt corners and the outer ring is its shoulder corners — all four on one
  line. The quad between them is a zero-area sliver whose normal is `NaN`, which is what the
  first `the_patch_faces_up` run reported. `mouth_of` tracks which ring vertices belong to which
  arm's mouth so those quads (and the skirt quads spanning them) are never emitted. Nothing is
  missing: the shoulder they would have covered is the road's own.
- **A junction is drawn by `road.wgsl`, unchanged.** It emits the same `u`/`v` convention, and
  `junction::as_road` synthesizes the `Road` the uniform is packed from — this width, this shoulder,
  these colours, every marking off. `u` is quoted against the **mean** of the arms' half-widths, so
  the asphalt/shoulder boundary lands exactly on the ring that is the boundary and `u` stays in
  metres, which is what keeps the shader's `fwidth` antialiasing meaning what it means on a road.

The junction does **not** trim its roads. It would be the tidier result and it inverts the ownership
every recipe here follows — a road's geometry would become a function of which junctions name it,
and `engine inspect` on the road would stop predicting the road. The author ends each road at the
mouth, and because the patch stretches to whatever the mouths are, "roughly there" is enough.

## Grain, and why it is behind a branch

`if road.start.y > 0.0 { … }` rather than `albedo * (1.0 + amount)` with `amount == 0`. The four
lighting lines below it are the ones this repo pins byte for byte, and arithmetic added ahead of
them can change how the compiler contracts them even when it is arithmetically inert — measured
three separate times on `mesh.wgsl`. A uniform branch costs nothing on any GPU this runs on, and it
makes "grain off is M23" structural. The roughness push is guarded the same way, which is why
turning `let roughness` into `var roughness` cost nothing: the A/B confirmed it.

The hash is **integer**, not `fract(sin(dot(p, k)) * 43758.5)`. `sin` of a large argument is where
two GPUs disagree first, and this repo's habit is that a generator sitting under a baseline writes
its own sequence out (M19's forests, M29's meadows). Grain perturbs albedo and roughness only,
never the shading normal: normal-perturbed grain sparkles under a moving camera at exactly the
frequencies a fixed-step deterministic renderer should not be producing.

## Where things moved

- **`road::surface` takes a model matrix and an optional `Ground` now.** Both are ignored — and
  kept out of the cache key — unless the road actually follows a terrain, so a road with absolute
  heights caches exactly as it did before M40 and an animated road transform still reuses one
  upload.
- **`scene::road_items_of` and `junction_items_of` are free functions on `&World`.** Physics builds
  trimesh colliders straight off a `World` and, since M40, cannot rebuild a road from its own
  component — a road may name a terrain, and a junction is a function of other entities. So
  `build_collider` takes a **pre-resolved** `GeneratedSurface` instead of `Option<&Road>`, and
  physics now reads the identical `Arc` the renderer draws. That is a simplification, not just a
  plumbing change: terrain resolution used to exist twice.
- **`RoadItem.junction: Option<Junction>`** marks a draw that is a patch rather than a ribbon. Both
  ride one draw list because both go through the road pipeline; `road-centerline` filters on this,
  because a patch has no centerline to publish and counting one would make "the scene has 3 roads"
  wrong.
- **`FIX_INTERNAL_EDGES` now covers junction patches too** — the same coplanar-triangle contact bug
  is waiting on a patch, and a car crossing a junction is exactly the case that finds it.

## New CLI surface

- `engine junction-plan <scene> [--entity N]` — where each arm met the patch, how wide it was, and
  its `reach` from the centre. A set of similar reaches is a tidy junction; one much larger is the
  road that stopped short. A screenshot cannot tell that from an arm arriving at the wrong angle.
- `engine road-centerline` gains `width` and `bank` (degrees) per sample, so a generator placing a
  car on a banked corner does not re-derive the roll from the polygon.

## Verification

`verify/m40_track.json` at `--steps 180`, pinned by
`cli.rs::the_m40_track_fixture_pins_banking_width_and_a_junction` — a circuit that cannot be
authored without all five: banked corners, a pit lane widening 7 m → 13 m → 7 m, three roads riding
an M22 terrain, a T junction where the pit lane meets the paddock road, and grain on. `samples: 1`,
per the trap about this adapter and fine geometry against relief under MSAA.

The physics half pins the claim a picture cannot make: **the banking has the right sign.** A ball
dropped on the west straight approaching a right-hand corner must roll toward the *inside* and stay
on the asphalt. Banked the other way it rolls off the outside.

The tour gained `ServiceSpur` / `ServiceNorth` / `ServiceSouth` / `ServiceJunction` and
auto-banking + grain on `RingRoad`, so all six `showcase_*` frames re-blessed — expected, since the
scene gained four entities. Everything else stayed bit-exact.
