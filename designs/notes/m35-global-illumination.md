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

## GI and M38's cascades wanted the same binding

Both milestones were built in parallel off the same base, and both claimed
**binding 5 of group 2** — M38 for the cascade matrix buffer, M35 for the first
probe plane. Merging them was a rename and nothing more, because **a binding
number is not a position in a list**: GI starts at 6, the layout skips 5 at one
cascade, and WGSL resolves each declaration by its own index. The cascade entry
being *conditional* costs GI nothing for the same reason.

The two shader transforms compose in one direction only —
`with_sky_common(with_cascades(<gi-composed source>, cascades))` — because
`with_cascades` rewrites the shadow lookup and GI rewrites `AMBIENT`/`FILL`,
which are disjoint. `a_cascaded_surface_inside_a_volume_takes_both` asserts all
five receivers carry both declarations rather than trusting that reading.

## A scene has at most one volume

The design allowed several (smallest spacing wins, name-sorted on a tie) and
§7's own arithmetic assumes one field: four 3D textures and one placement in the
frame uniform, "group 2 goes from five bindings to nine". A second volume costs
four more bindings per volume, or giving up the hardware trilinear filtering that
is the entire reason the field is a texture rather than a buffer.

G2 shipped that gap as a *warning* — several volumes validated, one rendered.
The decision taken after the merge closed it the other way: **a second
`LightProbeVolume` is `multiple_light_probe_volumes`, an error.** The precedent
is `DirectionalLight` and `AmbientLight`, which are at-most-one *errors* already
and for exactly this reason — the renderer holds one field, so the second one is
not a lesser version of the effect, it is a component that does nothing.

The warning was the worse half of the trade in a way worth writing down: a
warning is only read by someone who suspects a problem, and the failure here has
no symptom. An author who adds an interior volume and sees no change in the
landscape around it cannot tell that from a bad bake. The error is read by
everyone, because `validate` is the gate.

What it cost to make it an error: three lines in `passes::lights` (the loop that
was already raising the other two), one error code, and the deletion of a
warning pass. What it *bought* is a simplification downstream —
`evaluate::rendered_volume` no longer resolves between volumes, `gi-probe` no
longer explains which one answered, and `bake-gi` dropped its multi-target loop
and its `--out`-names-one-file guard onto `sole_entity_with`, the helper
`water-height` and `road-centerline` already share. **The rule that lets a
command be simple is the rule enforced at `validate`, not the one documented in
a note.**

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
- **The tour**, measured after merging M37–M43 in: GI touches **48–74% of every
  station's pixels**, and the largest channel delta on five of the six is **14 to
  18** — under the manifest's threshold of 24. Only `showcase_90`, the frame
  looking into the forest, puts pixels over it: 118 of 230,400, max delta 32.
  That shape is §3.1 working rather than a weak volume. An unoccluded probe
  reconstructs `sky_ambient` exactly, so open ground *cannot* move much; what
  moves is what has something above it, and the canopy frame is the one with a
  lot of that. A version of this feature that changed the open frames would be
  the version that had a bug.

## Traps

- **The bake is a build artifact that lives in the repo.** `examples/scenes/gi/`
  and `examples/scenes/verify/gi/` are committed, and
  `every_asset_a_committed_scene_references_is_committed` fails if a scene names
  a bake that is not tracked. A scene edited after its bake fails `validate` with
  `gi_bake_stale`, which is the cheapest place to catch it — but see the gap
  below.
- **`validate` does not catch geometry-level staleness — `bake-gi --check`
  does.** `gi_bake_stale` at `validate` compares the component's `spacing`,
  `bounces` and derived grid against the header, which catches an edited
  *component*. It does not recompute `inputs_hash`, because that needs the
  scene's whole triangle set: 504,970 triangles for the tour, **0.86 s against
  `validate`'s 0.17 s**, and rising with every triangle added while `validate`
  stays flat. So the geometry check is a command — `engine bake-gi <scene>
  --check`, which collects, hashes, compares and writes nothing — with
  `every_committed_gi_bake_matches_its_scene` standing over every committed bake
  with no allowlist.

  **This is not hypothetical — the M37–M43 merge walked straight into it.** The
  tour gained an entity's worth of geometry per milestone, every one of the 1,050
  probes moved on the re-bake, and `inputs_hash` went from `852eb35b022c82d1` to
  `9f833d7ff5a93b23`. `validate` was green the whole time. The re-bake happened
  because a human knew the scene had moved, which is exactly the thing a gate is
  supposed to replace — and the contract test is that gate now, so the same merge
  would fail `cargo test` rather than ship.

  **What this still does not catch**: an author who edits geometry, renders, and
  never runs the tests. That is the accepted cost of keeping `validate` at
  0.17 s, and it is why `--check` is worth running by hand after moving anything
  in a scene with a volume.
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
