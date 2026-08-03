# M40 — Roads that can build a track

M23 shipped a road that is one ribbon: a polygon of corners with radii, one width, one flat
cross-section, heights authored per point, and markings painted per pixel. Everything in
`CLAUDE.md`'s "Roads (after M23)" deferred list is a thing you hit the moment you try to *lay out a
circuit* rather than demonstrate one:

| Deferred item | What it blocks |
|---|---|
| Junctions | Two roads meeting is a hole in the asphalt. `make_car_track.py` cannot build a pit lane, a slip road, or a crossroads. |
| Banked cross-sections | Every corner is flat. A car at speed slides off a corner a real circuit would hold it through, and the fix today is "drive slower". |
| Per-point width | One width per road. A pit lane, a widening run-off, a narrowing bridge each need a *second road entity* whose edges do not line up with the first. |
| Roads that follow a `Terrain` | A road over M22 terrain is authored by reading `engine terrain-height` at every corner by hand and pasting the numbers back. Move the terrain's seed and every one of them is wrong. |
| Asphalt grain | The surface is one flat colour, so a wide road reads as a painted plane and there is no scale cue between 5 m away and 50. |

This milestone builds all five. The order below is the order they were built in, and it is not
arbitrary: per-point width and banking both change the cross-section, terrain-following changes the
height profile that the cross-section rides on, and junctions consume all three through the roads
they join.

**Everything here defaults to the M23 behaviour**, which is the house rule (M16 §"Default new
behaviour to off"). A scene that authors none of the new fields generates the same vertices, and
`verify/baselines/m23_road.png` is expected to stay bit-exact — that expectation is the A/B this
milestone ends on, not a hope.

## 1. The cross-section is scaled and rolled, and `u` does not move

The load-bearing observation, and the reason per-point width and banking are cheap rather than
structural: **M23 already separates the cross-section's shape from its coordinate**.

```rust
for column in columns {
    let offset = column * mitre;                    // where the vertex goes
    mesh.positions.push((*center + right * offset).to_array());
    mesh.uvs.push([v[i], column]);                  // what the shader is told
}
```

A mitred sharp vertex widens the *positions* by `1 / cos(turn/2)` while `uv[1]` keeps the nominal
column. The shader's `|u| > half + shoulder` test therefore stays true through a mitre without the
mitre being uploaded anywhere. Per-point width is the same move with a second factor, and banking
is a rotation of `right`, which is rigid and so changes no arc length at all.

### Per-point width

`RoadPoint.width: Option<f32>` — the asphalt width in metres at this point. Absent is the road's
`width`, so every existing file is unchanged. The authored values become knots on the same
monotone-cubic profile the heights already use (§2), evaluated per sample, and enter the build as
one scale factor on the whole cross-section:

```rust
let offset = column * mitre * width_scale[i];       // width_scale = local_width / road.width
mesh.uvs.push([v[i], column]);                      // unchanged
```

**The whole cross-section scales, shoulder included.** This is a real decision with a real cost, and
the alternative was considered and rejected: scaling only the asphalt and holding the shoulder at a
constant metre width means `u` measures *different* things at different `v`, so `|u| > half +
shoulder` stops being a fixed number and the shader can no longer find the skirt without a
per-vertex channel it does not have. `MeshData` carries positions, normals and one `vec2` of UVs;
adding a third vertex channel to the road pipeline is a vertex-layout change on a path four separate
places in this repo flag as ULP-sensitive, to buy a shoulder that stays 1.5 m instead of becoming
2.6 m under a road that doubled in width. Not worth it.

The visible consequence to document, because someone will notice it: **paint scales with the road.**
A section at 1.5× width gets a 1.5× wider edge line. On the widths a track actually uses (7 m to
12 m) that is 0.14 m of paint becoming 0.24 m, which reads as correct rather than as a bug, and it
is the honest consequence of markings being measured in the same coordinate the road is.

### Banking

Two ways in, because the two authoring situations are different.

**`RoadPoint.bank: Option<f32>`** — degrees, explicit. Positive raises the driver's **right** edge.
Absent falls through to auto-banking.

**`Road.auto_bank: f32`** (degrees, default `0` = off) with **`Road.auto_bank_radius: f32`**
(metres, default `20`). A corner of radius `r` banks by

```
auto_bank * auto_bank_radius / max(r, auto_bank_radius)
```

capped at `auto_bank` for anything tighter than the reference and falling off as `1/r` for anything
wider — and **the engine picks the sign**, raising the outside of the turn. That last part is the
whole point of the feature. The sign of an explicit bank is a fact about which way the road turns
*there*, which the author has to work out from the winding of a polygon they are also editing; every
corner on a clockwise circuit takes a negative number and every corner on an anticlockwise one takes
a positive, and getting it wrong builds a track that throws the car off at every corner. Two fields
and no signs is what "author a circuit with ease" means here.

Rejected: computing the bank from a target speed (`atan(v²/gR)`, the physically ideal angle). It is
the right formula and the wrong input — a road does not know what will drive on it, the answer for a
kart and an F1 car differ by 20°, and it would put a velocity in a geometry component. `auto_bank`
is a shape knob and says so.

Bank enters the build as a roll of the cross-section frame about the local heading:

```rust
let right = rotate_about(right_horizontal, along, bank_radians);
```

Rigid, so `u` — cross-section arc length — is untouched, every marking stays where it was, and the
normals fall out of the existing `rights[i].cross(along)` with no special case. The skirt still
drops **vertically** from the banked outer edge, because an embankment is a slope to the ground and
not a continuation of the road.

Banking a straight is possible (author `bank` explicitly on a straight point) and the profile
interpolates to it through the same monotone cubic, so a corner's bank rolls in over the approach
instead of appearing at the tangent point.

## 2. One profile, three quantities

M23's `heights()` is a Fritsch–Carlson monotone cubic over knots pinned at the middle of each
corner's arc, with a wrap for closed roads. Width and bank want exactly the same treatment and for
exactly the same reasons — a linear ramp in width puts a crease down the road at every corner, and a
linear ramp in bank breaks the roll rate where the car is loaded up — so the interpolator is
extracted as `Profile`, and height, width and bank each build one.

**The extraction is pure code motion, expression for expression.** Rust does not contract float
expressions into FMA without an explicit `f32::mul_add`, so unlike the WGSL splices this refactor is
safe by construction; the A/B at the end of this milestone confirms it rather than assuming it.

Monotone matters differently for each: for height it stops a road authored to 6 m cresting at 6.4
(M23's original reason), for width it stops a road authored between 7 m and 12 m bulging to 12.6
somewhere in the middle, and for bank it stops a corner overshooting into a roll the author never
asked for.

## 3. Following a terrain

`Road.follow_terrain: Option<String>` names a `Terrain` entity, the way `Meadow.terrain` already
does, and resolves through the same `Ground { terrain, transform }` pair and the same
`terrain::world_height_at`. Absent — the default — is M23 exactly.

When it is set:

- **`position.y` becomes a clearance above the ground**, not an absolute height. `0` sits the road
  on the terrain; a few centimetres lifts it clear.
- The terrain is sampled at **every centerline sample**, not only at the authored points. Sampling
  per point and interpolating between them was the third option on the table and it is the one that
  floats a road over a dip halfway down a long straight, which is precisely the case the feature
  exists for.
- That raw profile is **smoothed along the road** over `Road.follow_smoothing` metres (default
  `12`), because terrain is noise and a road that reproduces it is undrivable. The filter is a
  symmetric box over arc length, wrapping on a closed road so the seam is not a step.
- The authored `y` values still ride on top as a clearance profile through the same monotone cubic,
  so a road can lift clear of the ground for part of its length.

**Pinned points.** `RoadPoint.pin_height: bool` (default `false`) makes that point's `y` an absolute
height again — a bridge deck, a junction that has to meet another road's fixed level. The pin is
applied as a *local correction*: the difference between the pin and the followed profile at that
point, faded out over `Road.follow_blend` metres (default `30`) with a smoothstep, summed over
pins. So the road reaches each pinned height exactly and returns to hugging the ground on either
side, and with no pins the correction is identically zero.

Corrections sum rather than compose, which means two pins closer together than `follow_blend` pull
on each other and neither is reached exactly. That is a real limitation, it is what "blend" means,
and validation warns (`road_pins_overlap`) rather than leaving it to be discovered from a
screenshot.

### What this deliberately does not do

**It does not carve the terrain.** The alternative — the road writing back into the height field to
flatten a shelf under itself — was considered and rejected: `Terrain` owns its grid, and a second
recipe mutating it makes the rendered ground a function of which other entities happen to exist,
which is the "no hidden state" invariant in a different costume. The height field stays the one
source of truth for where the ground is.

The accepted cost: **where the smoothed road passes below the real terrain, the terrain pokes
through it.** A road across a sharp ridge will cut into the ridge. The answers are more clearance
(`position.y`), a pinned point, or a wider `follow_smoothing` — all of them things the author can
see and change in the file. Carving remains the honest fix and stays on the deferred list, now with
a note saying what it would have to answer.

The skirt handles the opposite case for free: where the road stands above the ground, the
embankment already drops to meet it, which is what `skirt` was for.

## 4. Junctions

Two roads meeting is the one item here that is a new primitive rather than a new field, exactly as
the deferred list predicted: a ribbon is swept along a curve, and a junction is a *patch* bounded by
the mouths of the roads that reach it.

```json
{
  "type": "Junction",
  "arms": [
    { "road": "MainStreet", "end": "end" },
    { "road": "SideStreet", "end": "start" },
    { "road": "PitEntry",   "end": "start" }
  ],
  "flare": 1.0,
  "corner_segments": 6,
  "shoulder": 1.5,
  "color": [0.09, 0.09, 0.10],
  "roughness": 0.92
}
```

**An arm names a road and which end of it arrives.** The junction reads that road's finished
surface — the same `RoadSurface` the renderer and the physics trimesh are built from, so a junction
cannot disagree with the road it joins about where the road ended, how wide it was there, what
height it reached or how far it was banked. This is the `road-centerline` rule applied inside the
engine: nothing re-derives a curve someone else already built.

Geometry, from the arms:

1. Each arm contributes a **mouth**: a position, a heading pointing *into* the junction, a
   half-width (asphalt and shoulder, including the local width scale), a height and a bank. All of
   it read off the road's terminal cross-section.
2. Arms are **sorted by bearing** about the centroid of the mouths, so the patch is built in
   rotational order whatever order the file lists them in.
3. Between each pair of adjacent arms, the boundary runs from one mouth's corner to the next
   mouth's corner along a **quadratic Bézier through the intersection of the two mouth edges** —
   the standard corner flare, and always defined even when the edges are near-parallel, where it
   degenerates to the chord. `flare` (0..1, default 1) pulls the control point back toward that
   chord; `corner_segments` is how finely the curve is cut.
4. The asphalt is a **fan from the centroid**; the shoulder is a ring offset outward; the skirt
   drops vertically from the shoulder's outer edge. Interior height is the inverse-distance blend of
   the arm heights, so a junction between roads at different levels ramps rather than steps.

**A junction is drawn by the road shader, unchanged.** It emits `u`/`v` on the same convention — `u`
is `0` at the centroid, `half` at the asphalt boundary, `half + shoulder` at the shoulder's outer
edge, and past that on the skirt — so asphalt, shoulder and embankment colour themselves through
exactly the code that colours a road, and `RoadItem` carries a *synthesized* `Road` whose markings
are all off. No new shader, no new pipeline, no new draw path, and nothing added to the fragment
shader whose lighting M23 duplicated deliberately.

Rejected: painting markings across the junction. Real junction markings (stop bars, turn arrows,
give-way triangles) are per-arm and per-lane, they want a lane model the engine does not have, and
half of them are decals rather than paint. A junction is asphalt, and paint on it is the next
milestone's problem.

Rejected: the junction trimming the roads that reach it. It would be the tidiest result — author a
crossroads, get a crossroads — and it inverts the ownership every recipe in this engine follows. A
road's geometry would become a function of which junctions happen to name it, so `engine inspect` on
the road would stop predicting the road, and two junctions naming the same end would fight. **The
author ends each road at the junction's mouth**, and because the patch stretches to whatever the
mouths actually are, "roughly there" is good enough — an arm that stops 2 m short simply makes the
patch 2 m longer on that side. How far each mouth actually
landed is what `engine junction-plan` publishes as `reach`, which is where that question belongs:
validation sees components, not the built patch, and a "far arm" warning derived from authored
points alone would be wrong the moment either entity carried a transform.

The `Junction` entity owns its geometry, so it carries **no `Mesh` and no `Material`**
(`junction_with_mesh`), and a `Collider` with `"shape": "trimesh"` on it takes the patch — the same
contract `Road` has, and for the same reason: the surface driven and the surface drawn must not be
authorable apart.

## 5. Asphalt grain

`Road.grain: f32` (0..1, default `0` = off) and `Road.grain_scale: f32` (metres, default `0.35`),
carried onto junctions too. A value-noise field in the road's own `(u, v)` — so it follows the curve
and the grade like every other road marking — perturbing albedo and roughness slightly.

Two things it is not:

- **Not a texture.** A texture on a road wants UVs in a tiling space, and the road's UVs are metres
  along and across, which is a coordinate the grain wants and a tiling atlas does not. Procedural
  also keeps the road's "no asset files" property: a `Road` is still a recipe.
- **Not a normal perturbation.** Grain that tilts the shading normal makes a road sparkle under a
  moving camera at exactly the frequencies a fixed-step deterministic renderer should not be
  producing. Albedo and roughness only.

**The default path takes a branch, not a multiply.** `if road.grain > 0.0 { ... }` around the whole
term rather than `albedo * (1.0 + amount)` with `amount == 0`, because the four lighting lines below
it are the ULP-sensitive ones and arithmetic added ahead of them can change how the compiler
contracts them even when it is arithmetically inert. A uniform branch costs nothing on any GPU this
runs on, and it makes "grain off is M23" a structural claim rather than a numerical one — which the
A/B then checks anyway.

## 6. What is verified

- `verify/m40_track.json` + baseline — a circuit that uses all five: banked corners via `auto_bank`,
  a pit lane that widens through `RoadPoint.width`, the whole thing following an M22 terrain, a
  junction where the pit lane rejoins, and grain on. Rendered at `samples: 1` with the camera on the
  junction, per the trap about this adapter and fine geometry against relief under MSAA.
- A CLI test that diff-renders it, and drops a ball on the banked corner and requires it to **stay
  on the road** — M23's fixture test with the banking as the new thing it proves.
- `engine road-centerline` gains `width` and `bank` per sample, so a generator placing a car on a
  banked corner does not re-derive the roll.
- `engine junction-plan <scene> [--entity N]` publishes the mouths a junction resolved — where each
  arm arrived, how wide, and its `reach` from the patch's centre — because "the patch looks wrong"
  is otherwise a question only a screenshot can answer, and it is the same reason `road-centerline`
  exists.
- The A/B: `m23_road.png`, `car_demo` and the tour, from a `main` binary and this one. Grain off,
  no per-point width, no bank, no terrain: byte-identical, or the milestone has a bug.
