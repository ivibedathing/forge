# Meadow design (M28)

Ground cover that **grows, seeds, and dies on a loop**: a field of small plants that walk a life
cycle from seed to sprout to green grass to flowering weeds to dry yellow stalks to collapse, and
then start again — each generation landing in a slightly different spot, the way a real meadow
reseeds itself rather than regrowing on the exact same stems.

The cycle is on the scene clock, so it can be sped up: `cycle_length: 3.0` runs a whole generation
in three seconds, which is what lets the showcase tour *show* the system instead of describing it.

## 0. What makes this different from every other recipe component

`Tree`, `Cloud`, `Water`, `Terrain` and `Road` all grow their geometry once and never change it.
A meadow's plants change **shape** continuously — a sprout is not a small blade of grass, it is a
different object, and a dry stalk with a seed head is a third. That collides head-on with the
engine's geometry pipeline, which is built on the assumption that geometry is generated once,
`Arc`-cached, and uploaded once (M15 keys the renderer's vertex-buffer cache on the `Arc`'s
address and evicts after 240 idle frames).

So the whole design is the answer to one question: **how does a thing that changes shape every
frame avoid minting a new mesh every frame?** M18 already answered it for water, and this is that
answer applied to a harder case.

## 1. A meadow is one entity with one component

```json
{ "name": "Field", "components": [
  { "type": "Transform", "position": [0.0, 0.0, 0.0], "scale": [40.0, 1.0, 40.0] },
  { "type": "Meadow", "seed": 7, "density": 24.0, "terrain": "Ground",
    "cycle_length": 3.0, "stagger": 0.25, "wind": 0.4 }
]}
```

`Meadow` owns its geometry, so the entity carries **no** `Mesh` and **no** `Material`, and having
either is `meadow_with_mesh` — the rule `Water`, `Cloud`, `Terrain` and `Road` already follow.
Footprint is `Transform.scale` in XZ, like every other recipe component, so there is no second way
to say how big the field is and the editor's scale gizmo drives the right thing.

Rejected: **`Meadow` as a `Material` on any mesh**, so a non-rectangular field is a glTF outline.
Same objection as water's — the author would have to supply the geometry before seeing a single
plant — but worse, because a meadow's geometry is thousands of *separate* plants and a mesh cannot
say where one ends.

Rejected: **scattering plants as ordinary entities** (one entity per tuft, like the car track's
guardrail posts). 20 000 entities in the JSON is not git-diffable in any useful sense, blows past
what `hecs` iteration and the draw list were sized for, and makes the life cycle a per-entity
script. The whole point of a recipe component is that the file says *what*, not *each*.

## 2. Two static things, and nothing else is uploaded

Per meadow, the CPU produces exactly two buffers, both generated once and both `Arc`-stable:

- **A template** — one plant, grown at maximum extent, in `engine-core/src/meadow.rs`. Cached on
  the template's own field bits, M19/M20's rule.
- **An instance buffer** — one record per plant: world position, yaw, uniform scale, and a `u32`
  seed. Twenty bytes. Static.

Neither ever changes with time. Every bit of life-cycle motion happens in the vertex stage from
`ScenePass.time`, which the renderer already carries for water and cloud drift.

**Instancing, not one big mesh.** A 40 × 40 m field at 24 plants/m² is 38 400 plants; baking them
into one mesh at ~40 triangles each is 1.5 M triangles of vertex data uploaded and held. The
instance buffer for the same field is 768 KB and the template is a few kilobytes. `particles.wgsl`
already draws instanced geometry from a per-instance vertex buffer, so this is a pattern the
renderer has rather than one it needs.

### The template carries its own vertex type

A plant's vertices need more per-vertex data than `MeshData` has: a parameter along the blade, and
the phase window during which the vertex exists at all (§4). So `meadow.rs` defines its own
`MeadowVertex` and the pipeline its own vertex layout.

Rejected: **packing the extra channels into `MeshData`'s `uv` and `normal`.** It fits, barely, and
it is the kind of cleverness that reads as a bug six months later. Rejected harder: **adding
fields to `MeshData`**, which M26 has just finished threading UVs through — every upload path in
the renderer touches that struct, and changing its layout is a bit-exactness risk against every
committed baseline for the sake of one component that does not share the mesh pipeline anyway.

## 3. The clock, and the two numbers that come off it

```
progress   = time / cycle_length + offset(plant)
phase      = fract(progress)     // where in the life cycle, [0, 1)
generation = floor(progress)     // which generation, an integer
```

`phase` drives everything visible. **`generation` is what makes the meadow reseed rather than
regrow**, and it is the cheapest good idea in this design: `hash(plant.seed, generation)` in the
vertex shader gives each plant a fresh set of random numbers every time round the loop, so its
position within its own cell, its height, its lean and its yaw all shift a little each generation.
Seeds fall *near* the parent, not on it. The dead stalk and the new sprout are not collinear, and
that is most of what sells the cycle as regrowth instead of as an animation loop.

It costs one integer hash per vertex and **no state anywhere** — invariant 2 holds, the render
stays a pure function of (file, time), and the meadow sits under a `diff-render` baseline.

**`cycle_length: 0` freezes the meadow** at `phase`, and that is the default. M21's `day_length: 0`
for the same reason: most scenes want a dial, not motion, and a frozen field is reproducible with
no `--time` at all. `phase: 0.45` — mature green — is the frozen default, so `{"type": "Meadow"}`
alone puts a working field of grass in a scene.

**`stagger`** is how far plants desync: `0` marches the whole field in lockstep, `1` spreads phase
offsets uniformly so every stage is present at every moment. A real meadow is nearer the first —
it browns together, with variation — so the default is `0.25`. The showcase turns it down further,
because a field that always shows all six stages at once never appears to *change*.

## 4. Stages: a keyframe table, and geometry that emerges and withers

The life cycle is an array of keyframes over `phase`, interpolated linearly and **wrapping across
phase 1 → 0**, exactly as M21's palette wraps across midnight. Each keyframe carries all seven
fields — `at`, `height`, `width`, `lean`, `sway`, `color`, `tip_color` — and a half-specified
keyframe is an error rather than a fade to black, which is the lesson M21 wrote down.

| phase | stage | what the keyframe says |
|-------|-------|------------------------|
| 0.00 | seed | height ~0, the plant is not there |
| 0.08 | sprout | short, wide-ish, bright yellow-green, floppy |
| 0.30 | green grass | full height, saturated green, soft sway |
| 0.55 | weeds | tallest, coarser, flower heads out, olive |
| 0.75 | dry | straw yellow at the tip, still standing, stiff |
| 0.90 | collapse | leaning hard, grey-brown, height falling |
| 1.00 | seed | back to nothing, and `generation` has ticked |

`color`/`tip_color` are a gradient along the plant, because senescence runs tip-downward in real
grass and a single flat colour per stage looks painted. `sway` per stage is what separates green
grass that flows from dry stalks that stand — it is also nearly free, since the wind term is
already there (§5).

At most `MAX_GROWTH_STAGES` (8) keyframes; the table rides a per-draw uniform array, the shape
`MAX_WAVES` and `MAX_POINT_LIGHTS` already use. `at` must be strictly increasing in `[0, 1)`
(`meadow_stages_invalid`, mirroring `daylight_palette_invalid`).

### Different shapes, one static mesh

Scaling a blade cannot turn it into a flower head. So the template contains **the union of every
stage's organs** — blades, a flower head, a seed head — and each vertex carries `emerge` and
`wither`, the phase window during which its organ exists. Outside that window the organ scales to
zero *about its attachment point*, collapsing its triangles to zero area. Degenerate triangles
rasterize nothing, so the cull is free and needs no second draw call, no index rewriting, and no
branch that could diverge across a warp.

This is the mechanism the whole component rests on: **shape change is a scale animation on parts
that are always present in the buffer.**

## 5. Wind, and why it comes almost free

The vertex stage already has `time`, the plant's world position and a parameter along the blade, so
wind is a smooth value-noise bend sampled at `dot(world_xz, wind_direction) − wind_speed · time`.
Sampling against a *travelling* coordinate is what makes gusts cross the field as visible waves
rather than making every plant shimmer in place independently.

Bend amplitude goes as `t²` along the blade — a cantilever, and more importantly the thing that
stops a blade from translating rigidly. The pond that `Water` replaced was sixteen tiles that each
moved as a rigid block, and rigid motion is exactly as unconvincing on a blade of grass as it was
on a pond tile.

The noise hash is spelled out in-repo, like M13's xorshift, M17's turbulence hash and M22's fBm,
and for the same reason: a meadow render sits under a baseline, so the hash is a format contract.

## 6. Standing on the ground

`terrain` names a `Terrain` entity, and each plant's Y is sampled at generation time through
`terrain::world_height_at` — M22's single implementation, which is the point of it having been
extracted into a function. Absent, the field is flat at the meadow's own Y.
`meadow_terrain_not_found` / `meadow_terrain_invalid` mirror `wheel_vehicle_not_found` /
`wheel_vehicle_invalid`.

`max_slope` (degrees) drops plants on ground steeper than grass grows on, using
`terrain::gradient_at`. Plants stand **vertical** regardless of slope — real grass grows toward
light, not normal to the hillside — and aligning to the surface normal is deferred rather than
made a field.

**The cache key is the trap here.** The instance buffer depends on another entity's fields *and*
its transform, so keying it on the `Meadow`'s own bits alone would leave a moved or re-shaped
terrain with grass floating in the air. The key is `(meadow bits, terrain bits, terrain transform
bits)`, and a test moves the terrain and asserts the buffer is rebuilt.

No `Collider`. Grass is not collidable, for the same reason the car track's 58 trees carry no
collider: it is scenery, and giving thousands of tufts colliders would be both a physics cost and a
car that mysteriously slows in the rough.

## 7. Rendering

A new pipeline, `shaders/meadow.wgsl`, duplicating `mesh.wgsl`'s lighting with `sky_common.wgsl`
prepended — the `water.wgsl` / `road.wgsl` / `clouds.wgsl` precedent, for the reason M16 wrote
down: the four untouchable lines in `mesh.wgsl` must reach the compiler surrounded by the code they
shipped in.

Rejected: **an M26 `with_surface` producer.** Producers splice the *fragment* path of `mesh.wgsl`;
a meadow needs a wholly different vertex stage, a different vertex layout and instancing. M26 also
measured that merely compiling an extra variant for existing draws moved a pixel of
`m16_environment`.

- **Opaque pass, drawn after terrain.** Grass is opaque geometry; no sorting, no blending.
- **`cull_mode: None`** with the normal flipped toward the viewer, so a blade is visible from both
  sides without emitting both faces. Halves the template — the `clouds.wgsl` precedent.
- **Grass receives shadows and does not cast them.** A single 2048² directional map cannot resolve a
  blade of grass; what it would record is sub-texel noise that crawls as the ortho box slides, which
  reads as a bug. It would also cost a second full draw of every plant. What replaces the missing
  self-shadow is **root darkening baked into the shading** — a fixed falloff toward `t = 0`, which
  is the standard trick and is most of what makes grass sit *in* the ground rather than on it.

## 8. What is not animatable, and what scripts get

`density`, `plants`, `blades`, `segments`, `seed`, `terrain` and the template's shape fields go in
`animation.rs`'s `NOT_ANIMATABLE` — M22's rule exactly. A clip driving `density` would rebuild the
instance buffer every frame and leave hundreds of megabytes in the renderer's cache; a clip aimed
there fails validation with `unknown_property`. Stage colours, `wind` and `sway` animate freely.

Scripts get **nothing** in v1 — specifically no phase setter. M21 refused a settable clock because
a script-driven clock is hidden state (invariant 2), and the same argument applies unchanged.

## 9. Reproducibility: what was predicted, and what was measured

The prediction was that a meadow would walk straight into M22's finding — *fine geometry against
relief under MSAA is where this adapter stops being byte-reproducible*. It did, and harder than
expected. The measurements, all on an Apple M3 Pro, debug build:

- **`samples: 4` is not reproducible with a meadow in frame.** Six renders of an unchanged fixture
  came back as **six distinct PNGs**, 1874 pixels apart, max channel delta 69.
- **`samples: 1` is.** Eight renders, one image. Shadows on or off makes no difference; the sample
  count is the whole variable.
- **Relief is not required, which sharpens M22's rule.** The fixture's ground is `height: 0.0` — a
  flat patch — and it still flakes at four samples. M22 needed a 200k-triangle relief patch to
  provoke this; a meadow provokes it on a plane. **The rule is not "fine geometry against relief",
  it is "enough sub-pixel geometry", and a meadow is the engine's densest source of it.**
- **The showcase tour is stable without the meadow and not with it.** Eight consecutive renders of
  the tour minus the `Meadow` entity at step 90 came back identical; with it, the same frame moves.
  The meadow is visible in all six tour frames (removing it changes 875–3649 pixels each), so all
  six are affected. Worst observed drift: 203 pixels, max channel delta 20.

So the two artifacts take opposite settlements, and the split is the design:

- **The fixture is `samples: 1` and carries a hard bit-exact pin.** `verify/m28_meadow.json` at
  `--time 0.7`, aimed at its subject with no horizon in frame (M26's rule). Giving up MSAA costs a
  verification fixture some anti-aliasing and buys back the only strict check this system has.
- **The six showcase baselines take `"diff_args": ["--threshold", "24"]`** — the tolerance M22
  already chose for `showcase_646`, now measured to cover the meadow's drift with margin. Nine of
  ten sweeps passed clean before the tolerance and the last six consecutively after it.

That is a real loss and it is worth naming: before M28, five of the tour's six frames were bit-exact
pins. **The next fixture that needs a hard pin on ground cover must render it at `samples: 1`.**

## 10. What the renders changed

Four things came out of looking at PNGs rather than out of tests passing, and all four are easy to
reintroduce by "simplifying":

- **Blades are thin.** The first pass authored `blade_width: 0.02` — two centimetres, which is a
  real measurement for a real blade of grass and rendered as a field of ribbons. At 7 mm with a
  higher density the same field reads as grass. Width and count trade off, and the eye reads count.
- **Every blade arches, including the middle one.** The splay was originally `blade_index /
  (blades - 1)`, which is exactly 0 for blade 0 — a rigid vertical wire up the centre of every
  tuft, and the field read as wheat. The `+ 0.55` offset in `reach` is what fixed it; inner blades
  now arch less rather than not at all.
- **Heads are spikelets, not beads.** A round octahedron at the top of a stem reads as a bead
  threaded onto it. Stretching the same eight triangles along the stem (×1.9 for the flower, ×2.8
  for the seed head) is the whole difference, for no extra geometry.
- **The maps' colours are near the plant's.** A flower colour with its own strong yellow popped as
  a field of scattered dots rather than as a flowering meadow; pulling it toward the tip colour put
  it back in the plant.

And one the *tests* changed: a blade's random length used a signed draw, so a blade could come out
25% longer than the template is tall — which quietly broke the claim that `Meadow.height` is metres.
`the_template_is_a_unit_tall_plant_with_every_organ_on_it` caught it.

## 11. Not in this milestone

Deliberately deferred, and named so the next session does not think they were missed:

- **Trampling** — grass flattened where a car or a body passed. It needs history, and history is
  hidden state.
- **Thatch** — dead matter accumulating between generations. Same objection.
- **A spatial cycle wave**, so yellowing crosses the field as a front rather than as scattered
  variation. One line (offset by `dot(world_xz, wave_dir)`), held back only to keep the field count
  down for a first cut.
- **Textured or alpha-cut blades.** M26's maps do not reach this pipeline yet, and the same gap
  exists for `Tree::leaf_material`; they should be closed together.
- **Coupling the cycle to `daylight`**, so a generation is a season rather than a number of
  seconds. Both ride the same `time`, so a scene can already tune them to agree.
- **Slope-aligned plants**, **shrubs and undergrowth**, and **LOD** — a meadow currently draws every
  plant at full detail however far away it is.
