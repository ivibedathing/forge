# M35 — Global illumination

Design: `designs/global-illumination-design.md`. That document holds the rejected
alternatives and the reasoning; this note holds what building it changed and the
things a future session will otherwise re-derive.

`LightProbeVolume` is one entity with a `Transform` for bounds and a `bake` path.
The bake stores **transfer** — how much light reaches a probe per unit emitted by
each sky band — so the fill light follows `daylight` without re-baking. Two
commands: `engine bake-gi` writes the file, `engine gi-probe` reports the number.

## The one number the whole milestone rests on

`gi::evaluate::LINEAR_GAIN` is **3.0**, and it is derived rather than tuned.

An unoccluded probe integrates `weight_band(d) · [1, d.x, d.y, d.z]` over a
sphere of directions. `weight_zenith(d)` is `d.y · 0.5 + 0.5`, so the zenith
band's transfer comes out `(0.5, 0, 1/6, 0)` and the ground band is its mirror.
Reconstruction has to give back `mix(ground, zenith, n.y · 0.5 + 0.5)`, whose
zenith half is `0.5 + 0.5·n.y`. Solving `1·0.5 + g·(1/6)·n.y = 0.5 + 0.5·n.y`
gives `g = 3` exactly.

So an open-sky probe reconstructs **`sky_ambient(n)`**, which is what makes
turning GI on unable to change the brightness of an open scene — it can only
redistribute it. Every visible difference is then attributable to geometry, which
is the only way an agent comparing two renders can tell what the feature did.
`an_unoccluded_probe_reproduces_sky_ambient` is where that is checked. **If it
ever fails, do not adjust the gain to make it pass** — the gain is a consequence
of `sky_band_weights`, and a failure means one of those two moved.

## Two sky bands, not three — the design doc was wrong

The design based the basis on `sky_gradient`, which does interpolate three bands.
But `sky_gradient` draws the sky *dome*; the term GI replaces is `sky_ambient`,
and that mixes only `sky_ground` and `sky_zenith`. **`sky_horizon` never appears
in it.**

A third band would let GI produce fill light the pre-M35 engine cannot, so an
open-sky probe would match `sky_ambient` in total energy but not in shape — and
that equality is the guarantee above. The cost is stated and small: a sunset's
horizon colour does not tint GI. It does not tint the ambient fill today either.

Widening `sky_ambient` to three bands would edit one of the four ULP-sensitive
lighting lines, so it is a milestone of its own, not a fix.

## The seam: two anchors that grew a second claimant

Two separate places in the shader assembly turned out to have the same latent
bug, and both were found by trying to add GI rather than by reading:

**`AMBIENT` and `FILL`.** The texture producer already replaced both whole lines
to fold in its occlusion map. `with_surface` applies substitutions by sequential
`str::replace`, so **two producers claiming one anchor is not a merge — it is a
silent no-op for whichever runs second.** A textured surface inside a probe
volume is exactly the case that must compose, and the tour is full of them. So
both lines are now *reassembled* from `FillContribution`s, the way M27 did the
vertex stage. Two kinds of contribution, because they compose differently: an
`occlusion` multiplier is a scale any number of producers may add, while an
`ambient_source` *replaces* where fill comes from and a second one panics.

**`FRAME_TAIL`, which was worse.** Refraction appended `view_proj` there by
substitution; GI needs three more fields. Two producers appending to one
*positional* struct makes the field order a function of the producer list — a
variant listing them the other way reads `gi_origin` where `view_proj` is and
renders a plausible wrong picture. No type catches that. It is now one
unconditional `EXTENDED_FRAME_TAIL` declared by every spliced variant, exactly
as the *object* uniform has been since M22, and refraction's substitution is
gone.

**The rule both cases teach:** an anchor with one claimant is a substitution; an
anchor that could ever have two is an assembly. Adding the second claimant later
is not a refactor, it is a bug that already shipped.

## `Road` and `Meadow` splice separately, and share the mesh's frame tail

Both duplicate `mesh.wgsl`'s lighting (the `water.wgsl` precedent), so neither
goes through `with_surface`. Each takes its own two-line splice in
`with_road_gi` / `with_meadow_gi`, against anchors in `recipe_anchor`.

**Their frame-uniform declaration is byte-identical to `mesh.wgsl`'s**, down to
the comment, so one anchor covers both and the replacement is the same
`EXTENDED_FRAME_TAIL`. That is not tidiness to be refactored away: three shaders
read one buffer, and the moment their declarations diverge one of them is reading
a field at the wrong offset. `the_recipe_shaders_receive_gi_through_their_own_splices`
asserts all three declare the *whole* tail, including the `view_proj` none of
them reads.

`Water` and `Cloud` deliberately do not receive GI — water's fill is dominated by
its sky reflection, and a cloud has no inside.

## One volume reaches the GPU

The design allows several volumes (smallest spacing wins, name-sorted on a tie)
and §7's own arithmetic assumes one field: four 3D textures and one placement in
the frame uniform, "group 2 goes from five bindings to nine". A second volume
costs four more bindings per volume, or giving up the hardware trilinear
filtering that is the entire reason the field is a texture rather than a buffer.

So: **`bake-gi` bakes every volume, `gi-probe` answers from whichever contains
the point, the renderer draws the finest, and `multiple_gi_volumes` warns at
`validate`.** The warning exists because the failure is invisible — an author who
adds an interior volume and sees no change in the landscape around it has no way
to tell that from a bad bake.

## What was measured, and what the numbers were

- **`intensity: 0.0` renders byte-identically to a scene with no volume at all**,
  across two *different compiled pipelines* — the GI variant with its extra
  bindings and uniform fields, against `mesh.wgsl` untouched. That is stronger
  than a source assertion and it is what
  `gi_at_zero_intensity_is_byte_identical_to_no_gi_at_all` pins.
- **The shader and `IrradianceField::sample` agree to 0/255** at both points of
  `the_shader_and_the_cpu_evaluator_agree`. The tolerance in the test is 3
  because half precision and a driver's trilinear filter are per-adapter; the
  measured delta here is zero.
- **The A/B: 38 of 39 artifacts byte-identical** after G2, the 39th being
  `showcase_585` disagreeing with *itself* — four distinct images from five
  renders on each binary, with the two populations overlapping. Sixth time in
  this repo's history that the answer was the adapter.
- **The tour**: station 01 (forest) changes 10.9% of its pixels; the other five
  stations change under 1% each. That is §3.1 working, not a weak volume — open
  ground *must* barely move, and the frame with a canopy is the one that does.

## Traps

- **The bake is a build artifact that lives in the repo.** `examples/scenes/gi/`
  and `examples/scenes/verify/gi/` are committed, and
  `every_asset_a_committed_scene_references_is_committed` fails if a scene names
  a bake that is not tracked. A scene edited after its bake fails `validate` with
  `gi_bake_stale`, which is the cheapest place to catch it — but see the gap
  below.
- **Geometry-level staleness is not checked.** `gi_bake_stale` compares the
  component's `spacing`, `bounces` and derived grid against the header. It does
  **not** recompute `inputs_hash`, because that needs the scene's whole triangle
  set — half a million for the tour — and `validate` is the ~0.02 s gate every
  other command runs first. So moving a wall and re-rendering silently keeps the
  old bounce. `bake-gi` writes the hash and the format carries it; nothing reads
  it back yet.
- **A probe exactly on the volume's boundary has weight zero.** `blend` fades
  inward from each face and `weight()` returns 0 at distance 0, so a floor lying
  exactly on the volume's bottom face receives no GI at all. Size the volume to
  contain the ground, not to rest on it. This cost a debugging pass on the
  agreement test.
- **`bake-gi` must tolerate its own error.** A scene naming a bake that does not
  exist fails `validate` with `gi_bake_missing` — including when the command
  being run is the one that would create it. `Scene::from_source_ignoring` exists
  for exactly this, and `load_scene_for_bake` names the three codes it tolerates.
  A fix that filters errors *after* `Scene::from_source` does not work, because
  that function re-validates internally.
- **A ceiling seals the sky out.** The fixture deliberately has none, against the
  design's wording: the light source here is the sky, so a closed box renders
  black and demonstrates nothing. The overhang over one sphere does the
  sheltering.
- **The bake file is large and `spacing` is the lever.** The fixture at
  `spacing: 0.5` was 592 KB; at `1.0` it is 88 KB with the same picture and the
  same assertions holding at a 9x ratio. Halving `spacing` multiplies the file by
  eight. Pick it from a render.
- **Line order in the bake file *is* its layout.** `BakedGi::parse` verifies each
  probe's coordinate against the position its line implies (x fastest, then y,
  then z), because both the fold and the texture upload index by line rather than
  searching for a coordinate. A permuted file would otherwise parse, carry the
  right probe count, and light the scene from the wrong places.
