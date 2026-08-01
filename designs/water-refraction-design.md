# Water refraction design (M27)

M18's design opened its list of what water does not do with refraction: "What is behind the
surface is absorbed and tinted, never bent. This is the upgrade M16 already named for transmissive
materials, and water is now its loudest customer." M26 built that upgrade — for `Material`. It could
not reach water, because a `Water` entity carries no `Material` and there is nowhere to put an `ior`.

This milestone closes that. It is a small change on purpose: the machinery M26 built (the opaque
colour copy, the split pass, the producer seam) is reused wholesale, and what is new is one field,
one prelude, and one pipeline variant.

## 1. One field, and it defaults to off

```json
{ "type": "Water", "segments": 96, "ior": 1.33, "waves": [ … ] }
```

`ior` is the only field this milestone adds, `1.0` (no bending at all) is the default, and the
range is `[1, 3]` — the same field, the same default, the same range and the same doc language as
`Material.ior`, because it is the same quantity. Water's physical value is 1.33.

That default is the house rule and it is doing real work here: eleven baselines were blessed before
M16 existed, and every one of them survived that milestone because a scene omitting the block
rendered byte for byte as before. Water is in the same position. `verify/baselines/m18_water.png`
and the showcase tour's pond were blessed against the M18 shader, and a `Water` with no `ior` must
still produce those exact bytes.

Rejected alternatives:

- **A `refraction` strength in `[0, 1]`.** It would default to off just as cleanly, but it is a
  number with no physical meaning that an author would have to tune by looking, and the engine
  already has a field for exactly this quantity on `Material`. Two names for one concept is the
  kind of thing this repo pays for later.
- **`Water` gaining a whole `Material`.** This was settled in M18 and is not reopened here: one
  surface, one source of truth, and `water_with_mesh` exists to enforce it. Water's optics are
  already fully described by its own fields.
- **Making refraction unconditional at water's real IOR.** Physically the most defensible, and it
  is what a renderer with no committed baselines would do. It re-blesses every water baseline in
  the repo to buy an author nothing they could not have written themselves.

## 2. The bend distance is measured, not authored

`Material` scales its refraction offset by an authored `thickness`, because a mesh has no idea how
deep it is. Water does: `water_thickness()` already returns the distance from the surface to
whatever is behind it, along the view ray, and it has since M18 — it is the field that drives
absorption and the shore foam.

So water refracts by the thickness it measures, and **there is no `thickness` field to author**.
This is strictly better than the mesh path rather than merely cheaper: the bed of a pond bends more
where the pond is deep and barely at all at the shoreline, which is what water does, and it falls
out of a number the shader had already computed. It also means the one field this milestone adds
cannot be set inconsistently with anything — there is no second knob to disagree with it.

**But the measurement is not the travel distance, and getting that wrong is what the first
implementation did.** `refraction.wgsl` puts the exit point at `world + refracted * thickness` —
fine for a mesh, whose `thickness` is an authored fudge with no geometry behind it. Water measures
a real quantity: the path length along the **view** ray. The refracted ray is a *different* ray,
and for any `ior > 1` it is always steeper, so travelling the view ray's distance along it lands
well below the bed and the sample comes back from nowhere in particular.

The numbers, from the M27 fixture — a 1.5 m pool seen at 66° from the normal:

| | drop below the surface | lateral displacement |
|---|---|---|
| step by `thickness` | 2.68 m (bed is at 1.50 m) | 2.53 m |
| solve to the bed | 1.50 m | 1.42 m |

A 1.8× overshoot, and it does not read as a bent pool bottom — it reads as the bed diced into
rectangular blocks, because adjacent pixels sample points metres apart. So the variant solves for
the depth instead: the view ray falls `thickness · v.y` to reach the bed, and the refracted ray
reaches that same depth after `drop / -bent.y`. Capped at `thickness`, which is not a fudge but the
`ior >= 1` bound stated as arithmetic — and it makes the whole expression continuous at `ior: 1.0`,
where `refract` is the identity and the travel is exactly `thickness` again.

This is the planar-bed approximation. It is exact when the bed under a pixel is level and degrades
gracefully when it is not, which is the right trade for a shader that already has the bed's depth
and cannot afford to march for its shape.

## 3. Absorption stays water's own model

`Material` absorbs the transmitted background with Beer–Lambert over an authored `attenuation`.
Water does not, and gains no `attenuation` field.

Water already has an absorption model, and a better-specified one: it grades `shallow_color` to
`deep_color` by `1 - exp(-thickness / depth_fade)` and drives `opacity` off the same curve. The
amount of bed that survives to the camera is exactly `1 - out_alpha`, which is the number the blend
unit was already using. Refraction changes **where the bed is read from, not how much of it comes
back.** Adding a second absorption curve on top would give one surface two ways to tint what is
behind it, and an author tuning `depth_fade` would be fighting whichever one they were not editing.

The practical consequence: turning `ior` on cannot change how *deep* a body of water looks. It can
only move what is under it. That is a property worth having, because it means the field can be
added to a tuned scene without re-tuning it.

## 4. Sampling is depth-validated, and that is not optional here

A screen-space refraction offset can read a pixel that is *in front of* the refracting surface.
When it does, whatever is standing in the water smears sideways into it. The mesh path lives with
this — `refraction.wgsl` names it among its three limitations and it has not hurt, because the ice
at station 03 is a block in mid-air with nothing in front of it.

Water cannot live with it. A pond is bounded by a shoreline on every side, and the shoreline is
*always* in front of some part of the surface: the bank at the far edge of the tour's pond is a few
pixels above the water it borders, and an unvalidated offset drags the bank down into the water in
exactly the frames the pond is being looked at. `shore_foam` exists because that boundary is where
the eye goes.

So the water variant reads the depth copy a second time, at the refracted coordinate, and **falls
back to the unrefracted sample when the pixel it found is nearer than the water surface**. Water
already has the depth copy bound — it is what `water_thickness` reads — so this costs one
`textureLoad` and no new binding, no new uniform and no new pass. The fallback is per pixel, so a
surface half of whose refracted samples are valid keeps the half that are.

This is the one place water's refraction is *more* correct than the mesh's rather than merely
adapted, and it is worth doing here rather than in `refraction.wgsl` for both: the mesh path has no
depth copy bound on the refracting pipeline, so giving it the same check is a binding change, a
separate milestone, and its own A/B.

## 5. A second pipeline, and `water.wgsl` is not edited

M22 measured this and M26 measured it again: splicing a feature *inline* into a shared shader moves
pixels in scenes that do not use the feature, because whether the compiler contracts `a*b + c` into
an FMA depends on the code around it. Putting terrain's branch inline moved one pixel in each of
three fixtures; compiling the refraction variant for every transparent draw moved one pixel of
`m16_environment`.

So the plain water pipeline compiles `shaders/water.wgsl` **as it sits on disk, byte-identical by
construction**, and a second `refractive-water-pipeline` compiles a variant assembled by
`with_water_refraction`: `shaders/water_refraction.wgsl` prepended, plus anchored substitutions.
Every anchor is asserted to appear exactly once and every substitution is asserted to land, the
same discipline `with_surface` already runs — a splice that silently did nothing renders the
feature as if it were absent, which is the failure mode hardest to see.

A water entity at `ior: 1.0` takes the plain pipeline. Not a branch inside the variant: the
*pipeline* is chosen per surface, so an unrefracting pond in a scene that also contains a
refracting one still compiles to the M18 shader.

**The file is not edited at all, including its comments.** `clock.z` was declared padding in M18
and this milestone puts the IOR there, which would ordinarily earn a comment fix — but the
substitution that claims the anchor rewrites that comment for the variant, so the disk file keeps
describing the pipeline that actually compiles it, and the question of whether naga's tokenizer
truly discards comments never has to be answered.

Using `clock.z` rather than extending `WaterUniform` is what keeps **one** uniform layout and
therefore one `water_objects` buffer feeding both pipelines. The plain shader never reads the slot.

## 6. What it reads, and what it does not touch

The variant reads `scene_color` and `scene_sampler` at group 2 bindings 3 and 4 — the frame-textures
group M26 reorganised, which water was already bound to for the shadow map and the depth copy. The
slots exist, the layout is unchanged, and the blended pass is already handed the bind group whose
colour copy is the real one.

It projects the exit point with **`surface.view_proj`**, out of `WaterUniform`, not with
`frame.view_proj`. The mesh variant appends `view_proj` to `FrameUniform` because a mesh's uniform
carries a premultiplied MVP and cannot supply it; water's carries world→clip already, because waves
displace in world space. So `FrameUniform` is untouched by this milestone, and with it every other
shader that declares a prefix of it.

The colour copy is now allocated when a scene contains **either** a refracting material or a
refracting water surface, and the split pass follows the same disjunction. A scene with neither
renders the pre-M26 pass structure exactly, which is the condition
`a_scene_with_no_refraction_is_untouched_by_the_colour_copy` already pins.

## 7. Composite order

The transmitted bed is held out of the running colour until after fog, for M26's reason: the copy
was already fogged at its own depth when the opaque pass drew it, and fogging it a second time is
what turned the tour's ice into a pale slab. So the variant adds it at the return, and sets
`out_alpha` to 1 — the surface now carries its own background, and the blend unit must not add the
framebuffer's unrefracted version on top of it.

Foam composites *before* that, unchanged. Foam is scattered light on the surface and it already
drives `out_alpha` toward 1; where it is opaque, `1 - out_alpha` is 0 and no bed is admitted, which
is correct — you cannot see through foam.

## 8. What is still not here

- **Refraction through the surface from below.** The bed is what is behind the water along the view
  ray whichever side you are on, so a camera under the surface refracts the sky-side frame by the
  same rule. It is not wrong, it is just not the air/water interface run backwards.
- **Water refracting another transparent surface.** The copy is the *opaque* frame. The ice at
  station 03 floating in a pond does not appear in what the pond bends. M18's depth copy has had
  exactly this limitation since it shipped.
- **Chromatic dispersion.** One offset for all three channels.
- **Planar reflections**, which M18 named as missing and this milestone does not touch.
  Refraction and reflection are the two halves of a water surface and only one of them is here.
- **A CPU mirror of any of this.** Nothing physical depends on the refracted sample, exactly as
  nothing physical depends on the detail normals.

## 9. What building it found

- **The exit point has to be solved, not stepped** — §2. This is the whole implementation risk of
  the milestone, it is invisible in a code review, and it renders as an obviously broken picture
  rather than a subtly wrong one, which is the good kind of bug.
- **A pattern under the water is what makes refraction testable at all**, and it has to run
  *across* the view direction. The refracted ray's horizontal component points the same way the
  view ray's does — away from the camera — so the displacement is along the view axis. The first
  version of `refraction_displaces_what_is_behind_the_surface` split the bed left/right, parallel
  to that axis, and saw 236 pixels change; bars laid across it see thousands. The M27 fixture's bed
  is a grid for the same reason: bars one way, two coloured cross-bars the other.
- **The depth check was measured before it was believed.** On the fixture's overhead camera it
  changes exactly zero pixels — the solved exit point is well-behaved enough that nothing reaches
  in front of the surface. It was nearly deleted as dead code on that evidence. At a grazing 8°
  it changes ~22k pixels by up to 99, dragging the boulder's silhouette and the blue cross-bar out
  across the water. Hence the fixture's **second camera**: a feature whose guard only fires in
  framings the fixture does not have is a guard nobody will keep.
- **Refraction is only visible in water you can see through.** The tour's pond is authored silty
  (`depth_fade: 0.8`) over a bed 0.2 m down, and the expectation going in was that `ior` would be
  decorative there. It moves ~30k pixels of `showcase_450`, because at a grazing camera the *path*
  through even a shallow pond is long. The pond was briefly retuned clearer to "make it visible"
  and that edit was reverted: §3 says turning `ior` on must not change how deep the water looks,
  and reaching for `depth_fade` to show off a different feature is exactly the coupling §3 refuses.
