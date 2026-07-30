# Terrain design (M19)

Ground, and what it is made of. The starting point is the same kind of embarrassment M18 started
from, and it is worth being precise about it because every decision below is aimed at one of these.

The showcase tour's ground is `builtin:plane` scaled to 200 × 200 with `albedo: [0.13, 0.19, 0.11]`.
That is **two triangles and one colour**. It is perfectly flat, so the sun lands on all 40 000 m² at
exactly the same angle and the whole surface is one value; nothing casts a shadow across it that
isn't a prop; there is no horizon line because there is no relief; and at any camera height the eye
reads it instantly as a backdrop rather than as a place. The tour's own doc calls the sky faked and
the animals faked. The ground was worse than faked — it was *absent*, a coloured floor holding
props up.

Three things are missing and they are not the same problem:

1. **Relief.** Ground is not flat. This is geometry, and physics has to agree with it.
2. **Material variation at every scale.** Real ground is a different colour at the top of a bank
   than in the hollow, on the steep face than on the flat, and it is mottled *within* each of those
   at a metre's scale. The engine has no texture mapping at all — `engine-assets` decodes PNGs and
   nothing samples them — so this has to be generated, and generating it is better anyway (§3).
3. **Somewhere to stand.** Everything currently at `y = 0` is standing on the flat floor. If ground
   acquires relief and nothing else moves, the whole tour is buried or floating.

## 1. A patch of ground is one entity with one component

```json
{ "name": "Ground", "components": [
  { "type": "Transform", "scale": [200.0, 1.0, 200.0] },
  { "type": "Terrain", "segments": 192, "seed": 7, "height": 2.4, "feature_scale": 46.0,
    "layers": [ … ] },
  { "type": "Collider", "shape": "trimesh", "friction": 0.9, "layers": ["ground"] }
]}
```

`Terrain` owns its surface geometry, exactly as `Water` does: a tessellated unit grid sized by
`Transform.scale`, so the entity carries **no** `Mesh` and **no** `Material`, and having either is
`terrain_with_mesh`. This is M18's rule and it is settled — one surface, one source of truth
(invariant 2).

Heights are sampled in **world** XZ, like water's waves, so two terrain entities with the same
fields meet seamlessly and moving a patch moves it *through* the field rather than dragging its
hills along. `Transform.scale.y` multiplies the displacement, as it would for any mesh; `height` is
what you get at `scale.y = 1`.

Rejected: a scene-level `terrain` block (one global ground cannot be an island and a seabed at once,
and `physics`/`environment` are settings, not geometry), and `Terrain` as a `Material` on any mesh
(the same argument that killed it for water — a plane is two triangles, so every author would have
to supply a pre-tessellated mesh before seeing a single hill).

## 2. Height is evaluated on the CPU — the opposite of water, for one reason

Water displaces in the vertex stage and keeps **no** Rust copy of the formula; `water-design.md` §6
defers a CPU evaluator, with the honest note that adding one now would mean a second implementation
with nothing checking it. Terrain does the reverse, and the reason is not taste:

- **Terrain does not animate.** A water grid must be re-displaced every frame, which is exactly why
  a CPU pass was untenable — it would mint an `Arc<MeshData>` per frame and defeat M15's geometry
  cache. Terrain's surface is a pure function of its fields: generate once, `Arc`-cache, upload
  once, never touch again. The argument that forced water onto the GPU does not apply.
- **Physics has to stand on it.** A collider is CPU geometry. GPU displacement would leave the car
  driving on the undisplaced plane, and no amount of shader work fixes that.
- **Placement has to query it.** `world.terrain_height(name, x, z)` is what lets a script keep a
  deer's feet on the ground, and it is what snaps the tour's props onto the new surface (§6).

So there is exactly one height implementation, in Rust, and the renderer consumes its output as
ordinary vertices. **No agreement test is needed because there is nothing to agree with** — which is
strictly better than water's position, not a compromise with it.

The field itself is fBm value noise over a domain-warped plane:

- **Value noise, and the integer hash is spelled out in-repo**, following M17's turbulence exactly.
  A scene's terrain is under a `diff-render` baseline, so "what does noise cell (3, −7) hash to" is
  a **format contract**; a dependency upgrade must not be able to reshape a hill.
- **Domain warp** (`warp`, off by default) displaces the sample point by another noise lookup before
  the octaves are summed. Two lines, and it is the single biggest difference between "fBm" and
  "landscape": plain fBm is isotropic blobs, and warping shears them into ridges and valleys that
  read as though water once ran over them.
- `octaves` / `persistence` are the usual knobs, normalised so the sum stays in `[-1, 1]` and
  `height` therefore means metres of displacement regardless of how many octaves are summed. Adding
  an octave must add *detail*, not *altitude* — the same argument that made water's `Q` packing
  divide by steepness rather than by wave count.
- **Normals come from central differences of the same function**, not from averaging triangle
  normals. Two extra height samples per vertex against a neighbourhood walk, and the result is the
  smooth field's true normal rather than the tessellation's.
- **Those normals are written in the patch's local space**, and this is the one place the geometry
  will silently lie to you. The renderer transforms every normal by the model matrix's
  inverse-transpose; for a patch scaled 180× across and 1× up that is `diag(1/180, 1, 1/180)`, which
  crushes a world-space normal flat. The result is a landscape with real relief that lights exactly
  like a plane — and, because every pixel then reports 0°, slope-selected layers that silently never
  appear. Scaling the gradient by the patch's own size on the way out inverts it exactly, for any
  scale. `mesh_normals_survive_the_model_transform` pins it.

## 3. The generative texture system

This is the part with no precedent in the engine, and it is the part the milestone is named for.

`Terrain.layers` is an ordered list of at most four materials, each claiming a **band of height and
a band of slope**:

```json
{ "albedo": [0.10, 0.16, 0.07], "roughness": 0.95,
  "height_range": [1.9, 40.0], "height_blend": 1.4,
  "slope_range": [0.0, 22.0], "slope_blend": 8.0, "noise": 0.5 }
```

The first layer is the base coat: it paints everywhere, and each later layer paints *over* what is
beneath it wherever its bands say it does. That is the whole model, and four decisions make it read
as ground rather than as a contour map.

Painting rather than averaging the weights matters more than it sounds. Under an average, a rock
layer that fully claims a cliff face still comes out half grass, and adding a fourth layer quietly
dilutes the other three; under painting, a layer's own weight is exactly how much of it you see.

- **Slope is a first-class selector, not a decoration.** Height alone gives you stripes — a
  topographic map, and unmistakably so as soon as the camera moves. What actually distinguishes
  rock from grass in the world is that soil cannot cling to a steep face, so `slope_range` is the
  selector that does the heavy lifting and `height_range` is the one that adds bands of climate on
  top. A layer that omits both applies everywhere and is the base coat.
- **Band edges are soft, in the band's own units.** `height_blend` is in metres and `slope_blend` in
  degrees, and the fade is spent *outside* the band, so a layer covers what it names at full strength
  and falls off beyond each edge. Both halves of that were learned by looking at renders. A single
  scale-free `blend`, as a fraction of the band's width, was the first design and is a trap: a layer
  aimed at "above 1.9 m" with a generous top end gets a fade *thirteen metres* wide, bleeds far below
  where it was pointed, and washes out everything under it. And fading *inward* — the other obvious
  reading of a range — makes every band weakest exactly where the author aimed it, and a base coat
  written `slope_range: [0, 90]` would vanish on flat ground and on cliffs alike.
- **The bands are tested against a *jittered* coordinate.** The boundary between two layers, drawn
  honestly, is an iso-line of a smooth function — which is to say a clean sweeping curve, and the
  eye reads clean curves as artificial faster than it reads anything else here. Each layer's `noise`
  perturbs the height and slope it *thinks* it is at, by a fraction of its own fade width, so the
  boundary breaks up into interlocking fingers at two scales. This is one multiply-add and it is the
  difference between "procedural" as a compliment and as an insult.
- **Macro mottling, applied to the blended result.** `color_variation` modulates the final albedo
  with the low-frequency noise. Even a single-layer terrain stops being one flat colour, which is
  the failure the current ground demonstrates most clearly.

Plus `bump`: a per-pixel normal perturbation from the gradient of the fine noise, with **no
displacement behind it**. This is water's detail slope field and it earns its place for the same
reason — it puts variation *between* the vertices, where a 192² grid over 200 m has one vertex per
metre and the eye is standing much closer than that. Water's second lesson is taken with it: **the
perturbation fades with view distance**, because sub-pixel normal detail aliases into sparkle that
reads as broken rather than as low quality.

Nothing physical may depend on the texture noise or on `bump` — the collider is the displaced grid
and nothing else — which is exactly what licenses per-pixel detail that no CPU code mirrors. The
split is clean: **height is shared with physics and lives in Rust; appearance is per-pixel and lives
in WGSL.**

Why not sample an image? Because invariant 1 wants no binary assets, invariant 3 wants no opaque
references, and a generated field is diffable, seedable, and infinitely large at zero bytes. The
engine having no texture mapping made this decision, but it is the decision it would have made
anyway — the alternative for an agent-operated engine is asking an agent to author a PNG.

## 4. Terrain rides the mesh pipeline, and mesh.wgsl gains one branch

Water got its own shader and duplicated the shadow lookup and `FrameUniform`, following `sky.wgsl`.
M17 duplicated the GGX terms into `evaluate_point_light` rather than share them. Both were right,
and terrain must go the other way:

**Water shades differently — terrain does not.** Terrain is an opaque lit surface, identical to a
mesh in every respect except where its albedo and roughness come from. A `terrain.wgsl` would have
to reimplement the GGX lobe, the PCF shadow lookup, the hemispheric sky ambient, the roughness-capped
environment reflection, the point lights and the fog — roughly two hundred lines that must then stay
in lockstep forever, in a scene where terrain is drawn *next to* meshes and any drift is visible
side by side. The point-light case is not hypothetical: the tour's campfire lights the ground it
stands on (M17's stated goal), and a terrain shader that forgot point lights would silently take
that away.

So `Terrain` becomes a `RenderItem` like any other — generated geometry, model matrix, and a
material — and picks up shadows, fog, MSAA, sky ambient, point lights, sorting and editor picking
for free and permanently. `fs_main` resolves its surface through one function call whose non-terrain
path returns the material it was handed, and the terrain parameters ride the **object** uniform,
grown at its end (the pattern `FrameUniform` already documents: a struct that grows only at its end
leaves every prior field where the shader already reads it).

This touches the shader M16 declares ULP-sensitive, so it was settled by the check this repo has
learned to trust: **an A/B between binaries built at `main` and here**, rendering every committed
fixture at its blessed step count and `cmp`-ing the PNGs.

**The A/B rejected the obvious implementation.** Putting the branch inline in `fs_main` — leaving
the four M4 lines textually identical, and changing only where `albedo` and `roughness` come from —
moved exactly one pixel by exactly one unit in each of `m16_environment`, `m17_fire` and
`m18_water`. All three are sky-lit scenes, so the drift is in the environment branch, and it is the
FMA-contraction hazard M16 warned about arriving exactly as described: same values, different
surrounding code, different generated arithmetic.

So `mesh.wgsl` is **not edited at all**. The plain mesh pipeline compiles the file as it sits on
disk, which makes its output byte-identical by construction rather than by measurement, and the
terrain pipeline compiles a *variant* assembled at build time by `with_terrain`: the declarations in
`shaders/terrain.wgsl` are inserted and the fragment prologue is rewritten by two anchored
substitutions, both asserted so a reworded `mesh.wgsl` fails loudly at startup instead of silently
rendering terrain as grey. The precedent is `sky_common.wgsl`, likewise concatenated rather than
copied. One lighting implementation, two compilations of it, and eighteen scene/step combinations
verified byte-identical against `main`.

The cost is one extra pipeline object and a pipeline switch — once per frame, since terrain draws in
one run at the end of the opaque pass. Both pipelines write depth and neither blends, so where in
that pass they draw cannot change a pixel.

## 5. Collision: the trimesh path already exists

A `Collider` with `shape: "trimesh"` and no `asset` currently falls back to the entity's own
`Mesh.asset`. Terrain extends that fallback by one step: no `asset`, no `Mesh`, but a `Terrain` —
use the generated surface. Vertices are scaled by `Transform.scale` by the code that is already
there, and the existing `trimesh_on_dynamic_body` rule keeps anyone from trying to throw a hill.

No rapier `HeightField`, though it is the obvious fit and is faster. The trimesh path is written,
tested and already used by the tour, and a heightfield would be a second geometry representation to
keep in step with the mesh the renderer draws — for a static collider whose cost is paid once at
build. If terrain ever gets large enough that the BVH hurts, that is the moment to reach for it.

## 6. Standing on it

Everything the tour places at `y = 0` has to move, and the placement has to come from the same
height function the renderer and the collider use — a Python mirror of the noise would be a second
implementation of the thing §2 exists to avoid.

The engine can already do this with no new CLI surface: a scratch Rhai script sets each prop's
position to `world.terrain_height(...)`, and `engine simulate --steps 1 --bake` splices the results
back into the scene file under M10's change-based bake rule. Props land on the ground by running the
engine, not by trusting a spreadsheet.

For anything that *moves* over the ground, the script queries every step — which is what the tour's
wildlife and truck scripts now do.

## 7. What is not here

- **Erosion.** Hydraulic or thermal erosion is what turns fBm into landscape that has a history, and
  it is a simulation over the height field, not a formula. It is also the natural next milestone.
- **Holes, caves, overhangs.** A height field is a function of (x, z) and cannot express any of
  them.
- **Layer masks an author paints**, and any per-vertex or per-entity override of the generated
  material. Everything is a function of the fields.
- **Splat maps, texture arrays, triplanar image sampling.** All want the texture mapping the engine
  does not have. `bump` is a normal perturbation, not a normal map.
- **LOD.** One tessellation, uniform across the patch. A 200 m ground at `segments: 192` is 74 000
  triangles and the geometry cache uploads it once; the moment terrain wants to be a kilometre
  across, this is the first thing that has to change.
- **Vegetation scattering.** Placing trees by density and slope is the obvious companion feature and
  is a separate component.
- **Terrain on water's absorption path.** A shore reads correctly because the water surface fades
  against whatever depth is behind it, and terrain is behind it like any other opaque geometry.
