# Meadows (M29, `designs/meadow-design.md`)

*Relocated verbatim from `CLAUDE.md` when it was compacted to an index. The digest that points here is `CLAUDE.md` §Meadows.*

*The design doc for this milestone is `designs/meadow-design.md` — it has the rejected
alternatives; this file has what the build learned.*

`Meadow` is ground cover with a **life cycle**: seed → sprout → grass → flowering weeds → dry straw →
collapse → seed, on the scene clock, so `cycle_length: 3.0` runs a whole generation in three seconds.
A recipe like `Tree`/`Cloud`, so the entity carries **no** `Mesh` and **no** `Material`
(`meadow_with_mesh`), sized by `Transform.scale` in XZ.

**It is the first recipe here whose subject changes shape over time**, and the whole design is the
answer to how that avoids minting a mesh per frame (M15 keys the upload cache on `Arc` identity).
Two static buffers per meadow — a **template** (one plant at maximum extent) and an **instance
buffer** (36 bytes a plant) — and everything visible happens in `meadow.wgsl`'s vertex stage from
`ScenePass.time`. Water's M18 trade, on a harder case: water kept its topology, a plant has to change
*organs*.

- **Shape change is a scale animation on parts that are always in the buffer.** Every vertex carries
  the phase window (`emerge`..`wither`) its organ lives in; outside it the organ scales to zero about
  its own anchor and its triangles rasterize nothing. No second draw, no index rewrite, no divergent
  branch. The template therefore holds the union of every stage's organs — blades, a flower head, a
  seed head — at all times.
- **`generation = floor(progress)` is what makes the cycle regrowth rather than a loop.**
  `hash(plant.seed, generation)` in the shader re-draws each plant's position within its cell, its
  height, lean and heading every time round, so the dead stalk and the sprout replacing it are not
  collinear. One integer hash, **no state anywhere** — the render stays a pure function of (file,
  time). The reseed hash is a **format contract** and is spelled out in the shader for the reason
  every generator here spells its own out.
- **`cycle_length: 0` (the default) freezes the field** at `phase`, exactly as `daylight.day_length:
  0` freezes the day. `stagger` desyncs plants; `0` marches the field in lockstep, `1` shows every
  stage at once and so never appears to change — the default is near the low end because a real
  meadow browns together.
- **`MeadowVertex` carries `centre` and `offset` separately**, and that is not tidiness: height scales
  by the stage's `height`, girth by its `width`. One combined position would make a taller plant
  proportionally fatter and leave `blade_width` — authored in metres — meaning something different at
  every stage.
- **The cache key covers the transform *and* the terrain.** Instances are placed in **world space**
  (altitude sampled through M22's `terrain::world_height_at`, the one implementation), so keying on
  the component's own fields would leave a moved meadow, or a re-shaped terrain under a still one,
  with grass floating at the old ground's height. `terrain_moves_rebuild_the_patch` pins it. Each
  instance also carries the ground's **gradient**, so a plant that reseeds a few centimetres away
  lands at the new spot's altitude rather than the old one's.
- **Every cell draws its full set of random numbers whether or not the slope test keeps its plant** —
  otherwise raising a hill at one corner reshuffles the grass at the other. M17's
  "defaulted fields consume no randomness", generalized.
- Rendering is `shaders/meadow.wgsl`, a new **instanced** pipeline duplicating `mesh.wgsl`'s lighting
  with `sky_common.wgsl` prepended (the `water`/`road`/`clouds` precedent, M16's reason). Opaque,
  depth-writing, drawn last in the opaque run. **`cull_mode: None`** with the normal flipped toward
  the viewer — a blade is a single-sided strip and half of every tuft faces away. **Grass receives
  shadows and casts none**: one 2048² cascade cannot resolve a blade, and what it would record is
  sub-texel noise that crawls. `ROOT_SHADE` (darkening toward the root) is what stands in for the
  missing self-shadow, and `BACKLIGHT` is what makes a field lit from behind glow.
- Budget is **`MAX_MEADOW_TRIANGLES` (8M), counted in triangles** rather than plants, because only the
  product of plant count and template complexity hangs a render. Measured: 1.3M draws in 0.19 s,
  7.1M in 3.6 s (debug). Geometry fields are in `NOT_ANIMATABLE`, `stagger` included — a plant's phase
  offset is drawn once, at placement.

**M29 is where this adapter's reproducibility limit gets sharper, and the two artifacts settle it
oppositely.** A meadow at `samples: 4` is *not* byte-reproducible: six renders of the unchanged
fixture came back as six distinct PNGs (1874 px, delta 69). At `samples: 1` eight renders are one
image. **Relief is not required** — the fixture's ground is `height: 0.0`, a flat patch — so M22's
rule is really "enough sub-pixel geometry", and a meadow is the densest source of it in the engine.
So `verify/m29_meadow.json` renders at **`samples: 1`** and keeps a hard bit-exact pin, while **all
six showcase baselines now carry `"diff_args": ["--threshold", "24"]`** (the tour is stable without
the meadow — 8/8 identical — and the meadow is visible in every frame, worst drift 203 px / delta 20).
That is a real loss of five bit-exact pins, recorded rather than hidden: **a new fixture wanting a
hard pin on ground cover must render it at `samples: 1`.**

Four authoring rules came out of looking at renders: blades must be **thin** (2 cm is a real
measurement and renders as ribbons; 7 mm at higher density reads as grass), **every blade arches**
including the central one (`reach`'s `+ 0.55` — a rigid vertical wire up each tuft read as wheat),
heads are **stretched spikelets** not beads, and the flower colour sits **near the plant's** or it
scatters as dots. Not here: trampling and thatch (both need history, and history is hidden state), a
spatial wave across the field, textured or alpha-cut blades, slope-aligned plants, and LOD.
