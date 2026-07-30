# Water design (M18)

Oceans, lakes, ponds and canals: what a body of water *is* in a scene file, how it becomes
geometry, and what the shader does with it.

The starting point was the showcase tour's pond, and it is worth being precise about what was
wrong with it, because every decision below is aimed at one of these. It was sixteen
`builtin:cube` tiles that a script moved up and down on a shared sine. Each tile translated as a
rigid block, so **the surface normal was straight up everywhere, at every moment** — no glitter,
no variation, nothing for the sky or the sun to catch. The tile seams were visible as a grid. And
because the tiles were opaque geometry with a low roughness, "deep" and "shallow" did not exist:
the pond was equally dark one centimetre from the bank and in the middle.

## 1. A body of water is one entity with one component

```json
{ "name": "Pond", "components": [
  { "type": "Transform", "position": [15.0, 0.2, 6.0], "scale": [12.4, 1.0, 12.4] },
  { "type": "Water", "segments": 96, "waves": [ … ], "depth_fade": 0.8, "shore_foam": 0.1 }
]}
```

`Water` owns its surface geometry: a tessellated unit grid, identical to `builtin:plane` at
`segments: 1`, sized by `Transform.scale` like any other mesh. The entity therefore carries **no**
`Mesh` and **no** `Material`, and having either is `water_with_mesh` — one surface, one source of
truth (invariant 2). Sixteen tiles pretending to be a pond is the thing this component exists to
delete.

Rejected alternatives:

- **A scene-level `water` block**, like `physics` or `environment`. One global sea level cannot
  describe a lake and a canal at two heights in the same scene, and the tour needs exactly that.
- **`Water` as a material on any mesh.** Tempting, because a non-rectangular lake outline is then
  a glTF file. But `builtin:plane` is two triangles, and a wave needs roughly eight quads per
  wavelength, so in practice every author would have to supply a pre-tessellated mesh before
  seeing a single wave. The component generating its own grid means `{"type": "Water"}` works.
- **Sizing with `size: [x, z]` fields** instead of `Transform.scale`. Two ways to say how big the
  water is, and the editor's scale gizmo would drive the wrong one.

Because waves are evaluated in **world space**, scaling a surface never stretches its waves, and
two water entities at the same height form one continuous surface for free.

## 2. Waves: a Gerstner sum, on the GPU

Each wave carries `direction` (degrees, `0` travelling toward −Z — the engine's forward
convention), `wavelength`, `amplitude`, `steepness` and `speed`. The surface is their sum.

**Gerstner, not sines.** A Gerstner wave moves each surface point *toward* the crests as well as
up, which sharpens crests and flattens troughs. A sum of sines is a rubber sheet — which is what
the sixteen tiles already were, more or less, so replacing them with a sine sum would have been a
smaller version of the same mistake.

**Displacement in the vertex stage, not on the CPU.** This one is not close. A 96 × 96 pond is
9409 vertices and the fixture's lake is 37 249; displacing those in Rust would mint a new
`Arc<MeshData>` every frame, which means a re-upload every frame *and* one entry per frame
accumulating in the renderer's geometry cache (M15 keys that cache on the `Arc`'s address, and
evicts after 240 idle frames). On the GPU the grid uploads once and never moves again.

**Normals from analytic derivatives.** The wave sum's partial derivatives fall out of the same
sines and cosines the displacement needed, so the exact normal is nearly free. Finite differences
would cost three evaluations and still be wrong at the crests, which is precisely where the eye
looks.

**`steepness` sums to at most 1** (`water_waves_self_intersect`). Past that the surface folds
through itself and the crests curl into loops. This is exact rather than a rule of thumb: the
renderer packs Gerstner's `Q` as `steepness / (k · A)`, which makes each wave's contribution to
the horizontal Jacobian equal to its own `steepness`, so the sum reaching 1 is the fold. Most
references divide `Q` by the wave *count* as well; that would make the same file look calmer as
waves were added to it, and would leave the validation rule meaning nothing in particular.

## 3. Detail: a slope field with no height behind it

Four scrolling sine trains, rotated by the golden angle, running at deep-water dispersion speeds,
perturbing the normal and nothing else. Per line of code this is the largest single difference
between blue glass and water, because it is what puts glitter *between* the grid vertices.

Two numbers in it were found by looking at renders, and both are worth keeping:

- The amplitudes are scaled so `detail: 1.0` tilts the surface by at most ~10°, and each layer's
  slope decays. The first attempt overshot by roughly 4× and turned the lake into white noise: a
  slope field is a mirror being shaken, and the layers are all in phase *somewhere*, so the worst
  case is their sum.
- The slopes fade with view distance. A half-metre ripple seen from 80 m is far smaller than a
  pixel, so its normal varies wildly within one pixel and the surface dissolves into sparkle —
  the classic specular aliasing failure, and the one that reads as *broken* rather than as low
  quality. Fading is the cheap half of the fix; the other half wants mip-mapped normals, which
  this shader does not have.

Nothing physical may depend on the detail normals — no buoyancy, no collision — which is exactly
what licenses a slope field with no surface behind it.

## 4. The clock

Water is a pure function of `(file, time)`, and `time` is always the reproducible clock, never
wall time: `--time T` when a command was given one, otherwise `steps / timestep_hz`. In the viewer
it is whole fixed steps taken since load, so flying around a lake for a minute and screenshotting
the same step number gives the same waves. (The M15 FPS readout remains the one wall-clock thing
in the engine, and it is viewer-only for this reason.)

That is what lets a water render sit under a `diff-render` baseline at all, and the M18 CLI test
pins it from both directions: `--steps 120` at 60 Hz and `--time 2.0` produce identical bytes, and
the same instant asked for twice does too.

Each wave is periodic at `wavelength / speed`, so unlike an animation clip there is no single loop
period where `t = T` equals `t = 0` unless the speeds are commensurate. Nothing depends on there
being one.

## 5. The frame gains a pass, but only when there is water

Absorption with depth and shoreline foam both need the depth of whatever is behind the surface,
and a pass cannot sample the depth attachment it is testing against. So a scene with water renders
as: **opaque geometry (depth stored) → depth copy → water and transparency → particles.** The copy
is one fullscreen triangle into a single-sampled `R32Float`, `textureLoad` per pixel; with MSAA it
reads sample 0, because absorption over metres does not care which sample of a pixel it measured.

A scene with **no** water keeps the exact single pass it had before — same attachments, same load
and store ops, same draws. That is not tidiness, it is the invariant: M18 arrived after seventeen
milestones of committed baselines, and not one of them may move. Verified the way this repo has
learned to verify it — an A/B between binaries built at `main` and here, rendering fifteen
scene/step combinations and `cmp`-ing the PNGs, all byte-identical — plus a test that renders a
water-free scene at two different times and requires the bytes to match.

Water sorts into the **same** back-to-front list as transparent meshes rather than into a pass of
its own, because an ice floe sitting in a pond is transparent geometry inside a water surface, and
two separate passes would fix which of them always draws over the other.

## 6. What is not here

- **Refraction.** What is behind the surface is absorbed and tinted, never bent. This is the
  upgrade M16 already named for transmissive materials, and water is now its loudest customer.
- **Reflections of the scene.** The surface reflects the sky gradient (shared with `sky.wgsl`
  through `sky_common.wgsl`, so it cannot drift from the sky drawn behind it) and the sun. The
  trees standing next to a lake are not in it. Planar reflections — a second scene render from the
  mirrored camera — are the obvious next step and are deterministic, so nothing about the baseline
  discipline stands in the way.
- **A CPU wave evaluator**, and therefore no `world.water_height(x, z)` and no buoyancy. The
  formula lives in WGSL only. When a boat needs to float, the Rust mirror comes with an agreement
  test against the GPU, and `water.rs` is where it goes; adding it now would mean a second
  implementation with nothing checking it.
- **Point lights on water.** The mesh pass evaluates them; the water pass does not. A local light
  glinting off a surface wants its own specular treatment, and the tour's one point light is a
  campfire two stations away from the pond.
- **Caustics, wet shorelines, spray from the water itself.** The tour's waterfall spray is a
  `ParticleEmitter` a script drives, as it was.

## 7. Authoring notes

- **Give water a sky.** The reflection term is gated on `environment.sky`, exactly as the mesh
  pass gates its own: with no sky there is nothing defensible to reflect, and water without a
  reflection looks like dark plastic.
- **`segments` is the wave resolution.** Roughly eight quads per wavelength; the detail normals
  are per pixel and do not care. A 200 m ocean carrying 3 m chop is not something this grid can
  draw.
- **The bed matters as much as the water.** Absorption grades between `shallow_color` and
  `deep_color` against what is *behind* the surface, so a bed at albedo 0.02 gives the clearest
  water in the world nothing to reveal. Retuning the tour's pond was half water fields and half
  the sand under it.
- **`shore_foam` is measured along the view ray**, so it appears where the water is genuinely
  thin, and a shallow pond seen at a grazing angle does not turn into a foam sheet.
