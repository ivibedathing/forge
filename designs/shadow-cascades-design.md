# M38: Shadow cascades

M16 shipped one shadow map, and the note said so in three words — "One cascade only." This is the
milestone that removes that sentence, and the one cloud shadows have been waiting on: a shadow
that covers the sky needs a map that covers the world, and one 2048² map cannot be both.

## 1. What is actually wrong today

`environment.shadow_distance` is a single knob trading area against sharpness, and every outdoor
scene in this repo is on the wrong side of it. The map is 2048² regardless, so the texel size is
`shadow_distance / 2048`:

| `shadow_distance` | texel | what it looks like |
|---|---|---|
| 40 m | 2.0 cm | crisp; contact shadows read |
| 60 m (the default) | 2.9 cm | fine |
| 240 m | 11.7 cm | a walking character's shadow is four texels wide |
| 600 m | 29 cm | shadows are blocks |

The showcase tour and the car circuit both want hundreds of metres of shadowed world *and* a
character-scale shadow under the thing the camera is looking at. There is no single value that
gives both, which is the definition of the problem cascades solve: spend the same texels
non-uniformly, most of them near the camera.

## 2. The shape of the answer

`environment.shadow_cascades`, an integer 1–4, default **1**.

At 1 the engine renders exactly what it renders today — the same matrix, the same texture, the
same shader source, byte for byte. That is not a nice-to-have; it is the constraint the whole
design is arranged around, for M16's own reason (§7).

At *n*, the sun renders *n* maps of the same 2048² resolution into an array texture, each fitted
to a nested slab of the view, and every receiver samples the tightest one that contains the point.

### 2.1 The cascades are nested, not sliced

The usual cascaded-shadow-map construction slices the view frustum into disjoint depth ranges —
`[0, d0]`, `[d0, d1]`, `[d1, d2]` — and fits a box to each slice. This engine does something
simpler: cascade *i* covers `[0, d_i]`, all of them starting at the camera, each one containing
the one inside it.

The reason is that `light_view_projection` already computes exactly this box, and it takes the far
distance as its only parameter:

```rust
light_view_projection(sun, camera_position, view_projection, shadow_distance, SHADOW_MAP_SIZE)
```

Calling it once per cascade with `d_i` in place of `shadow_distance` is the entire CPU-side change.
Nothing about the fit, the texel snapping, the pulled-back eye or the horizon clamp is re-derived,
which matters more than the wasted overlap: those four details each cost a debugging session in
M16 and M21, and a second implementation of them is how two fits start disagreeing.

**The property that falls out of nesting is the one that makes the milestone safe**: the outermost
cascade is fitted with `d_{n-1} = shadow_distance`, so it is *the same matrix today's single map
uses*. At `shadow_cascades: 1` there is only that one, and the call is character-identical to the
one on `main`.

The cost is overlap — cascade 2 re-renders everything cascade 1 and 0 already have. Measured as
draw calls it is `n ×` the caster pass, which §6 accepts explicitly.

### 2.2 The splits are geometric, ratio one third

`d_i = shadow_distance × (1/3)^(n - 1 - i)`.

Each cascade is a third the extent of the one outside it, so its texels are three times finer.
For `shadow_distance: 240` at three cascades: 26.7 m, 80 m, 240 m — 1.3 cm, 3.9 cm and 11.7 cm
texels, against 11.7 cm everywhere today.

One third rather than a tuned lambda because it is a number a scene author can reason about
without a second field to tune: *each level is 3× sharper and covers 3× less*. The practical split
scheme (a blend of logarithmic and uniform spacing, parameterized by lambda) is the standard
alternative and is rejected here for the reason `SHADOW_MAP_SIZE` is not a scene field — it is a
second knob whose only honest description is "try values until the render looks right", and the
knob that matters (`shadow_distance`) already exists.

### 2.3 Selection is containment, not view depth

The receiver does not compute a view-space depth and compare it against split distances. It
projects the world position into each cascade in turn and takes the first one that contains it.

Nested boxes make this correct by construction: the first containing cascade is also the smallest,
therefore the sharpest. And it needs no split distances in the frame uniform, no near/far
reconstruction in the fragment stage, and no agreement between a CPU-side split table and a
GPU-side comparison — three places a cascaded implementation classically goes wrong.

### 2.4 The seam between cascades is M16's edge fade, reused

`shadow_factor` already ends:

```wgsl
return mix(sum / 9.0, 1.0, smoothstep(0.85, 1.0, inset));
```

— the last 15% of the map fades to fully lit, so its boundary is a gradient rather than a line
ruled across the world. With cascades that same fade becomes a fade **to the next cascade**, and
only the outermost still fades to lit:

```wgsl
if this is the last cascade { return mix(here, 1.0, fade); }
return mix(here, next_cascade, fade);
```

Same constant, same concept, and today's behaviour is literally the `n = 1` case of it. A hard
switch would show: the two cascades agree on where the shadow is but not on how soft it is (3×
the texel size is 3× the PCF footprint), and an abrupt change in penumbra width along a curve
across the ground reads as a bug.

## 3. Where the data lives

### 3.1 The caster: n bind groups over one buffer, and no shader change at all

`shadow.wgsl` and its three siblings read `frame.light_view_proj`. Rather than teach them about
cascades, the frame uniform is written **once per cascade** into one buffer at aligned offsets,
and cascade *i*'s caster pass binds a bind group whose `BufferBinding` starts at offset *i*.

The four caster shaders are untouched. The `frame_layout` bind group layout is untouched. The only
change in `record_shadows` is a loop and which bind group goes into slot 1.

The alternatives were worse in a way worth recording, because both are what a reader would reach
for first:

- **A dynamic offset on group 1.** Requires `has_dynamic_offset: true` on a layout every pipeline
  in the engine shares, and therefore an offset argument at every `set_bind_group(1, …)` call site.
  A layout change for the benefit of four draws.
- **A new bind group for the caster.** There isn't one. `downlevel_defaults` caps `max_bind_groups`
  at 4 and M26 spent the fourth (`pipelines.rs` says so where the skinned object layout is built).

### 3.2 The receiver: the matrices get a binding of their own

`CascadeUniform { view_proj: array<mat4x4<f32>, 4> }` at `@group(2) @binding(5)`, beside the
shadow map it describes.

Appending it to `FrameUniform` instead — M26's move for `view_proj`, and the first thing to try —
does not survive `water.wgsl`. Uniform field offsets are positional, so a field after
`point_lights` is only reachable by a shader that declares `point_lights`, and water's copy of the
struct stops at `params`. Water would have had to grow a `PointLightData` struct and an eight-light
array it never reads in order to reach one matrix. The frame-textures group is where the shadow
map already lives, its layout already differs between the two cascade modes, and an entry present
only in the cascaded one costs the default path nothing.

The cascade *count* is not in the uniform at all — it is a `const CASCADE_COUNT: u32 = n;` emitted
into the spliced shader, because the pipelines already know it at build time (§4).

### 3.3 The map becomes an array texture

`n` layers of 2048² `Depth32Float`. Each caster pass renders into one layer's view; the receivers
sample a `texture_depth_2d_array`. At four cascades that is 64 MB of shadow map, which is why the
field stops at 4 and why the default stays 1.

## 4. Why this is a pipeline-build-time decision

`texture_depth_2d` and `texture_depth_2d_array` are different binding types, so the frame-textures
bind group layout depends on the cascade count, so the pipelines do.

This is `with_samples`'s situation exactly, and it takes `with_samples`'s answer: the count is
baked into the renderer at construction, and changing it builds a new `SceneRenderer` — which M36
already does every time a script writes `environment.samples`. `SceneRenderer::configured(device,
format, samples, cascades)` is the constructor both go through; `with_samples` keeps its signature
and passes 1.

The consequence that matters: **at one cascade every pipeline compiles the shader source that sits
on disk**, unmodified, exactly as it does on `main`.

## 5. The splice

Four shaders sample the shadow map — `mesh.wgsl`, `water.wgsl`, `road.wgsl` and `meadow.wgsl` —
each with its own near-copy of the lookup (M18 wrote water's; M22 and M29 copied mesh's). All four
must change together: a bind group layout declaring a `D2Array` texture and a shader declaring
`texture_depth_2d` is a pipeline-creation error, not a rendering difference.

They change by **anchored substitution**, the mechanism `with_surface` and `with_water_refraction`
already use, for the reason CLAUDE.md gives at the top of its trap list: `mesh.wgsl`'s four
lighting lines must reach the compiler surrounded by the code they shipped in. Three substitutions
per file:

1. the two group-2 declarations → the array-texture forms,
2. the frame uniform's tail → the same tail plus `cascade_view_proj`,
3. the whole `shadow_factor` / `shadow_lit` function → its cascaded version.

Every anchor is asserted to appear exactly once at pipeline build, and a `seam_tests` case pins
that each substitution actually landed. A splice that silently does nothing renders the feature as
absent, which is the failure mode hardest to see — the scene still draws.

Water keeps its own flat `0.0015` bias and its own function name; the other three share mesh's
slope-scaled bias. The generator takes both as parameters rather than unifying them, because
unifying them would change what a water shadow looks like today and this milestone changes nothing
at one cascade.

## 6. What it costs

- **The caster pass runs *n* times.** Every opaque caster is drawn once per cascade. There is no
  per-cascade culling: the engine has no spatial structure to cull against, and adding one is a
  larger change than this milestone. A scene at four cascades pays four times M16's shadow cost.
- **`n × 16 MB` of depth texture**, allocated the first time a scene casts.
- **The receiver samples up to two cascades** in the fade band, one elsewhere — so the fragment
  cost is at most 2× M16's 9 taps, and only within 15% of a cascade edge.

None of it is paid by a scene at the default.

## 7. What is deliberately not here

- **Per-cascade resolution.** All cascades are 2048². A finer inner map and a coarser outer one is
  the obvious refinement and it needs either a second texture or a texture array with unequal
  layers (which does not exist). Deferred.
- **Per-cascade `shadow_distance` authoring.** The splits are derived, not written. A scene that
  wants a specific inner distance can set `shadow_distance` and count backwards; a scene that wants
  four independent distances wants a different feature.
- **Cascade selection by view depth.** §2.3.
- **Stabilization beyond M16's texel snapping.** Each cascade snaps in its own light space at its
  own texel size, which is what M16 does and is why its shadows do not crawl. Whether the *outer*
  cascades crawl visibly at 3× texel size is a question for the render, not for this document.
- **Cloud shadows.** This is their prerequisite, not their implementation. A `Cloud` casting into
  the outermost cascade is its own milestone: clouds are transparent, and M16 decided transparent
  geometry does not cast.
- **Point-light and spot-light shadows.** Different problem, different map, still on the list.

## 8. Verification

- `verify/m38_shadow_cascades.json`: a long `shadow_distance` with casters at 5 m, 30 m and 120 m
  from the camera, `shadow_cascades: 3`, and **`samples: 1` with no `Terrain` in frame** — the
  fixture is meant to be a hard pin, and CLAUDE.md's trap list says fine geometry against relief
  under MSAA is not reproducible on this adapter.
- A unit test on `cascade_distances`: nested, ascending, and the outermost is exactly
  `shadow_distance` — the property §2.1 rests on.
- A `seam_tests` case per spliced file.
- The A/B against `main`: **every committed baseline must be byte-identical**, because every one of
  them is at the default. That is the check this design is arranged to pass, and if it fails the
  arrangement is what failed.
