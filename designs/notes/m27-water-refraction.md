# Water refraction (M27, `designs/water-refraction-design.md`)

*Relocated verbatim from `CLAUDE.md` when it was compacted to an index. The digest that points here is `CLAUDE.md` §Water refraction.*

*The design doc for this milestone is `designs/water-refraction-design.md` — it has the rejected
alternatives; this file has what the build learned.*

`Water` gains **one field, `ior`**, defaulting to `1.0` (no bending) — so every committed baseline
survived the milestone untouched except the six the showcase tour's own edit re-blessed, and the
sweep confirmed the other 27 bit-exact. `Water::refracts()` is `ior != 1.0`, and it joins
`Material::refracts()` in the disjunction that allocates M26's opaque colour copy and splits the
pass, so a scene with neither still renders the pre-M26 pass structure exactly.

- **Three things `Material` needs that water does not.** No `thickness`: `water_thickness()` has
  measured the view ray's path through the body since M18, so the bend scales with the water's own
  depth. No `attenuation`: water already grades `shallow_color`→`deep_color` off that same
  thickness, and the bed that reaches the camera is `1 - out_alpha`, the number the blend unit was
  already using. **Refraction moves where the bed is read from, not how much of it comes back** —
  which means turning `ior` on cannot change how deep the water looks, and it can go into a tuned
  scene without re-tuning it. And no `FrameUniform` change: the exit point projects with
  `surface.view_proj` out of `WaterUniform`, which water carries because waves displace in world
  space.
- **The exit point is solved to the bed's depth, not stepped along the refracted ray by the view
  ray's path length.** This is the milestone's one real trap. `refraction.wgsl` steps, correctly,
  because a mesh's `thickness` is an authored fudge; water measures a real quantity along a
  *different* ray, and the refracted ray is always steeper for `ior > 1`. Measured on the fixture —
  1.5 m pool at 66° from the normal — stepping overshoots the bed by 1.18 m and displaces the
  sample 2.53 m instead of 1.42 m, which renders as the bed **diced into rectangular blocks**, not
  as a bent pool bottom. The travel is capped at `thickness`, which is the `ior >= 1` bound as
  arithmetic and makes the expression continuous at 1.0.
- **The sample is validated against the depth copy** and falls back to the unrefracted one when it
  lands in front of the water. The mesh path skips this (its ice is a block in mid-air); water
  cannot, because a pond is bounded by a shoreline and by things standing in it. It costs one
  `textureLoad` from a copy water already has bound. **It was measured before it was believed**: on
  the fixture's overhead camera it changes *zero* pixels and was nearly deleted as dead code; at a
  grazing 8° it changes ~22k by up to 99, smearing the boulder's silhouette across the water. Hence
  the fixture's second camera.
- **`water.wgsl` is not edited, including its comments.** The plain pipeline compiles it as it sits
  on disk and a second `refractive-water-pipeline` compiles a variant assembled by
  `with_water_refraction` (M22/M26's splice, four anchors, each asserted to appear exactly once).
  The pipeline is chosen **per surface**, so an unrefracting pond beside a refracting one still
  gets the M18 shader. The IOR rides in `clock.z`, a slot M18 declared padding, which is what keeps
  one uniform layout feeding both pipelines.
- **Authoring: refraction is only visible in water you can see through, and needs a pattern under
  it.** A displacement of a uniform field is invisible by construction, and the displacement runs
  *along* the view direction — so a bed pattern parallel to that axis barely moves (the first
  render test split the bed left/right and saw 236 pixels change; bars laid across it see
  thousands). The tour's pond is silty over a 0.2 m bed and `ior` still moves ~30k pixels of
  `showcase_450`, because a grazing camera makes the path long even in a puddle.

Fixture `verify/m27_water_refraction.json` at `--steps 120`, **two baselines from one file** via a
second camera (`--camera CameraGrazing`) — the overhead one pins the bend, the grazing one pins the
clean waterline. Both are hard bit-exact pins with no tolerance, which M22's rule allows because the
fixture aims at its subject with no terrain in frame; four consecutive sweeps came back at zero.
Not here: refracting another transparent surface (the copy is the *opaque* frame, so the ice
floating in a pond is not in what the pond bends), chromatic dispersion, and planar reflections —
still the other half of a water surface, and still missing.
