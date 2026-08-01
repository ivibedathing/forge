# Shadow cascades (M38)

*Design doc: `designs/shadow-cascades-design.md`, which holds the rejected alternatives. This note
holds what building it taught.*

`environment.shadow_cascades`, an integer 1–4, default **1**. At 1 the engine renders exactly what
M16 rendered — same matrix, same texture, same shader source, byte for byte, verified by an A/B
against `main` over all 40 committed baselines. At *n* the sun renders *n* nested 2048² maps into
an array texture and every receiver samples the tightest one holding the point.

## The three decisions that made it small

**The cascades are nested, not sliced.** Cascade *i* covers `[0, d_i]` from the camera, each box
containing the one inside it, rather than the textbook `[d_{i-1}, d_i]` slices. That means
`light_view_projection` is called once per cascade with `d_i` in place of `shadow_distance` and
**nothing about the fit is re-derived** — not the texel snapping, not the pulled-back eye, not
M21's horizon clamp, each of which cost a debugging session when it was written. The property that
falls out is the one the milestone rests on: `d_{n-1}` *is* `shadow_distance`, so the outermost
cascade is M16's map and one cascade is M16's call.

**Selection is containment, not view depth.** Nested boxes are ordered by size, so the first
cascade that contains a point is also the sharpest that does — the receiver projects into each in
turn and stops. No split table in the frame uniform, no near/far reconstruction in the fragment
stage, and no CPU-side distances that must agree with a GPU-side comparison.

**The seam is M16's edge fade, reused.** `shadow_factor` already faded to fully lit across the last
15% of the map (`smoothstep(0.85, 1.0, inset)`) so its boundary was a gradient rather than a line
ruled across the world. With cascades that same fade becomes a fade *to the next cascade*, and only
the outermost still fades to lit. One constant, one concept, and today's behaviour is the `n = 1`
case of it.

## Traps

- **`water.wgsl`'s `FrameUniform` stops at `params`.** The cascade matrices were going to be
  appended to the frame uniform, on the terms M26 appended `view_proj` — and they cannot be, because
  uniform field offsets are positional and a field after `point_lights` is only reachable by a
  shader that declares `point_lights`. Water would have grown a `PointLightData` struct and an
  eight-light array it never reads in order to reach one matrix. They live at
  `@group(2) @binding(5)` instead, beside the map they address. **Check all four receivers before
  appending anything to a shared uniform** — they do not all declare the same prefix.
- **The caster shaders never learned about cascades**, and should not. `shadow.wgsl` and its three
  siblings read `frame.light_view_proj`; the frame uniform is written once per cascade into one
  buffer at aligned offsets, and cascade *i*'s pass binds a group naming its own slice. The two
  alternatives are both worse and both are what a reader reaches for first: a dynamic offset on
  group 1 changes a layout every pipeline shares, and a new bind group does not exist —
  `downlevel_defaults` caps `max_bind_groups` at 4 and M26 spent the fourth.
- **Four receivers sample the shadow map**, not one: `mesh.wgsl`, `water.wgsl`, `road.wgsl` and
  `meadow.wgsl`, each with its own near-copy of the lookup. They must change *together*, because a
  layout declaring a `D2Array` against a shader declaring `texture_depth_2d` is a pipeline-creation
  failure — it does not render wrong, it fails to build, and only on the machine that runs it.
- **The splice anchors on the function signature, not its body.** `replace_function` finds the
  signature, backs up over the `///` block above it, and replaces to the closing brace in column
  zero. Anchoring on forty lines of body would be an anchor someone reformats and silently breaks;
  the doc comment goes with it because `mesh.wgsl`'s says "over a single orthographic map", which is
  exactly what the cascaded variant is not.
- **`ShadowMap::new` returns early at one cascade** with the `TextureViewDescriptor::default()`
  M16 used, for both the sampled view and the single layer view. An explicit `D2` descriptor would
  be equivalent and was not worth the argument.

## What it costs

The caster pass runs once per cascade, over every opaque caster, with no per-cascade culling —
the engine has no spatial structure to cull against. A scene at four cascades pays four times M16's
caster cost and 64 MB of depth texture. A scene at the default pays neither.

The receiver samples two cascades inside a fade band and one everywhere else, so at most 18 taps
against M16's 9, and only within 15% of a cascade edge.

## Verification

`verify/m38_shadow_cascades.json`: a fence receding to 168 m under three cascades at
`shadow_distance: 240`, so **one object spans all three** and the sharpness gradient runs along it
rather than between three separate props. Flat ground, no `Terrain` in frame, `samples: 1` — per
CLAUDE.md's reproducibility rule — so it carries a hard bit-exact pin; three consecutive renders
came back as one image, measured rather than assumed.

Rendering the same file at `shadow_cascades: 1` is the comparison worth looking at: the near post's
shadow goes from a crisp line to a grey smear, which is the 11.7 cm texel a single 240 m map can
afford.

**The showcase tour deliberately stays at one cascade.** It would benefit — it is exactly the
long-`shadow_distance` outdoor scene the feature is for — but its six frames are the ones this
adapter cannot reproduce, and re-blessing them to demonstrate a field would trade a measured
tolerance for an unmeasured one. Turning it on there is its own change, with its own sweep.
