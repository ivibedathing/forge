# Terrain (M22)

*Relocated verbatim from `CLAUDE.md` when it was compacted to an index. The digest that points here is `CLAUDE.md` §Terrain.*

**There is no flat ground in the repo's scenes any more.** Following `Water` exactly, `Terrain` owns
a tessellated unit grid (`segments`, 1..512) sized by `Transform.scale`, so the entity carries **no**
`Mesh` and **no** `Material` (`terrain_with_mesh`). Heights are sampled in **world** XZ, so two
patches sharing a description meet seamlessly; `Transform.scale.y` multiplies the relief.

- **The height field is CPU-side — the opposite of water's choice, for three reasons.** Terrain does
  not animate, so the argument that forced waves onto the GPU does not apply; physics has to stand on
  it (a `trimesh` `Collider` with no `asset` and no `Mesh` borrows the generated surface, which is how
  ground is collidable without a mesh file); and placement has to query it. So there is exactly one
  implementation and **nothing to keep in agreement**. fBm value noise with the integer hash spelled
  out in-repo (a terrain render sits under a baseline, so the hash is a format contract), `warp`
  domain-warping it into ridges and valleys, and the octave sum normalised so `height` means metres
  however many octaves are summed.
- **Mesh normals are written in the patch's local space**, gradient scaled by the patch's own size.
  The renderer transforms normals by the model's inverse-transpose — `diag(1/180, 1, 1/180)` on a
  180 m patch — so a world-space normal arrives crushed to straight up and the landscape lights
  exactly like a plane *and* reports 0° everywhere, silently disabling every slope-selected layer.
  Pinned by `mesh_normals_survive_the_model_transform`.
- **The generative texture system** is `layers` (at most 4), each claiming a band of world height and
  a band of slope. The first is the base coat and each later one **paints over** what is beneath it
  (averaging would leave a rock layer half grass on a cliff it fully claims). Slope does the heavy
  lifting — height alone draws a contour map. Fades are **absolute** (`height_blend` in metres,
  `slope_blend` in degrees) and spent *outside* the band: a scale-free fraction-of-the-band `blend`
  was tried first and bleeds a 13 m fade out of a layer aimed at "above 1.9 m". `noise` jitters each
  boundary out of an iso-line into interlocking fingers, `color_variation` mottles at two scales an
  order of magnitude apart, and `bump` perturbs the normal per pixel with no displacement behind it,
  fading with view distance (water's specular-aliasing lesson). Nothing physical depends on any of
  the texture noise — the collider is the displaced grid and nothing else.
- **`mesh.wgsl` is not edited.** Terrain is lit exactly like a mesh, so it shares the file rather than
  duplicating 200 lines that would drift; but M16's four untouchable lines must reach the compiler
  surrounded by the code they shipped in. Putting the branch inline — those lines textually
  identical, only `albedo`/`roughness` arriving from a function result — **moved one pixel by one
  unit in each of `m16_environment`, `m17_fire` and `m18_water`**, found by the A/B between binaries.
  So the plain pipeline compiles the file as it sits on disk (byte-identical by construction) and a
  second `terrain-pipeline` compiles a variant assembled by `with_terrain`: `shaders/terrain.wgsl`
  inserted plus two **anchored** substitutions that panic at startup if `mesh.wgsl` is reworded.
- **`world.terrain_height(name, x, z)`** is the only terrain call in the script API — read-only,
  returning a world Y a script can assign directly. **Terrain's shape fields are deliberately not
  animatable** (`NOT_ANIMATABLE` in `animation.rs`): a clip driving `height` would regenerate the
  surface every frame and leave hundreds of megabytes of vertex buffers in the renderer's cache, so a
  clip aimed there fails validation with `unknown_property`. Appearance fields animate freely.

**Terrain is the first thing in this engine whose render is not bit-reproducible, and the reason is
not in the engine.** A 200k-triangle ground patch as the *last* draw of an MSAA render pass renders
differently run to run on Metal: one unchanged `showcase_tour.json` rendered 20 times came back as
two or three distinct PNGs, ~24 pixels apart, max channel delta 6, wherever the patch met other
geometry inside a pixel. Everything the CPU hands the GPU is identical run to run, and a *baked*
scene at `--steps 0` flakes too — what varies is which surface wins MSAA samples 1–3. At
`samples: 1`, with `shadows: false`, or with `height: 0` it is stable. **Any draw after the terrain
removes it**, so ground draws **first**, which is also right on its own terms: everything stands *on*
the terrain, so a contact surface exactly coplanar with it should tie in favour of the object, which
is what drawing the object second under `Less` gives.

Residue, recorded rather than hidden: `showcase_646.png`, the blast frame, still comes back as two
images ~100 pixels apart (delta ≤ 18) in the distant tree canopy. It is the only baseline with a
`diff_args` tolerance in `baselines.json` (`--threshold 24`). `showcase_810.png` was first seen to
flake the same way **once** — 29 pixels at a channel delta of 1, along the treeline, clean on the
next three runs — and read at the time as a one-off worth re-running rather than blessing away. The
M31 `draw` split then measured it directly and it is **not** a one-off: 3 distinct renders of 6, on
`main`'s binary as much as the new one (see Verification). Since M29 all six tour frames carry the
same tolerance anyway, so nothing needs re-blessing; what changes is the reading — 810 is in the
class rather than an exception to it.
M27 saw the same signature on **`showcase_450` (22 px) and `showcase_585` (24 px), both at delta 1**,
in one of seven consecutive full sweeps, the other six clean. Same residue, same verdict, no
tolerance: **the whole tour is in this class, not three named frames**, so a failure on any
`showcase_*` at a delta of 1 and a couple of dozen pixels is worth a second sweep before it is worth
debugging. Everything else in the manifest is bit-exact and a failure there is real — the M27
fixtures, which aim at their subject with no terrain in frame, never flaked across ten sweeps.
**M30 measured the rate instead of counting sweeps**, which is the cheaper way to settle one of
these: ten `--steps 585` renders of the *unchanged* tour came back as **three distinct images from
the M30 binary and two from `main`'s** — nondeterministic on both sides, so the sweep is a lossy
instrument and re-running it is a coin flip. When a sweep fails, `md5` N renders of the one frame:
a stable-but-different render is a real change, a different-every-time render is the adapter. **The general rule: fine geometry
against relief under MSAA is where this adapter stops being reproducible, so a new fixture wanting a
hard pin should aim its camera at its subject rather than across a landscape.** Verified by
`engine-render/tests/terrain.rs` (including `a_flat_single_layer_patch_is_exactly_a_painted_plane`,
which pins the shading path against `builtin:plane` at `segments: 1`) and `verify/m22_terrain.json`.
