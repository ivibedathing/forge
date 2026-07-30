# M4 — Materials & Lighting: Design

Milestone M4 of `agent-native-engine-design.md` §8: "PBR-ish material component, one directional
light, basic Phong or simplified PBR shader." This document settles what that means concretely —
the components, the shading model, the color-space change it forces, and how each piece is
verified. Written 2026-07-27, against the post-M2 codebase.

## 1. Scope

**In:** a `DirectionalLight` component, an `AmbientLight` component, an `emissive` field on
`Material`, a simplified PBR shader (Lambert + GGX Cook-Torrance), the linear→sRGB output switch
that real lighting forces, a `builtin:sphere` primitive so lighting is actually judgeable in a
screenshot, validation for all of the above, and headless pixel tests.

**Out (deferred, see §10):** texture maps of any kind, point/spot lights, shadows, tone mapping /
HDR, image-based lighting.

**Ordering note.** The design doc sequences M3 (assets) before M4, but nothing here depends on M3:
`MeshData` today carries positions/normals/indices and no UVs, so texture-mapped materials are
impossible until M3 adds UVs regardless of what M4 does. Everything below works on builtin
primitives and factor-only materials. M4 can be built before, after, or in parallel with M3; the
one interaction is reserved naming for future texture fields (§10).

## 2. New components

### DirectionalLight

```json
{
  "name": "Sun",
  "components": [
    { "type": "Transform", "rotation": [-50.0, 30.0, 0.0] },
    { "type": "DirectionalLight", "color": [1.0, 1.0, 1.0], "intensity": 1.0 }
  ]
}
```

Fields (all optional, `#[serde(default, deny_unknown_fields)]` like every other component):

- `color: [f32; 3]` — default `[1, 1, 1]`. Linear RGB, each component in `[0, 1]`.
- `intensity: f32` — default `1.0`. Unitless multiplier, `>= 0`, unbounded above. Not lux or
  candela: physical units buy nothing until there is exposure control, and "intensity 2 is twice
  as bright" is the mental model an agent already has. Magnitude lives here, chromaticity lives in
  `color` — that separation is what lets validation keep `color` in `[0, 1]`.

**Direction comes from the entity's `Transform`, not from a field.** The light shines down its
local **−Z**, exactly the convention the camera already uses (looks down −Z). Rationale:

- One orientation convention in the whole engine. An agent that has aimed a camera knows how to
  aim a light.
- A `direction` field would be a second source of truth for orientation on the same entity —
  precisely the class of ambiguity the design doc's invariants exist to prevent.
- Euler-degrees rotation is already the settled, agent-friendly encoding (`components.rs`
  documents why); reusing it beats inventing a vector-normalization contract for one component.

With no `Transform` (or identity rotation) the light travels toward −Z, i.e. horizontally. A noon
sun is `"rotation": [-90.0, 0.0, 0.0]`; the demo values `[-50, 30, 0]` give a pleasant
high-and-off-axis key light. As with the camera, the identity-transform fallback is documented
behavior, not an error.

**At most one per scene.** More than one is the validation error `multiple_directional_lights`,
mirroring `multiple_active_cameras`: a deterministic failure over a silent pick. Zero is allowed
and triggers the fallback rig (§3). Multiple lights are a real future feature, not a rejected one
— see §10.

### AmbientLight

```json
{ "type": "AmbientLight", "color": [1.0, 1.0, 1.0], "intensity": 0.05 }
```

- `color: [f32; 3]` — default `[1, 1, 1]`, components in `[0, 1]`.
- `intensity: f32` — default `0.05`, `>= 0`.

A flat, non-directional fill: `ambient = albedo * color * intensity`, added to the lit result. It
exists because a sun-only scene renders back faces pure black, and a black region in a screenshot
tells the agent nothing (the same argument that put the half-Lambert placeholder into M2's
shader). It is a component on an entity — not a scene-level field — because the entity list is the
only structure a scene file has, and because that gives it a stable `name` for CLI targeting like
everything else.

**At most one per scene** (`multiple_ambient_lights`), zero allowed.

### Material (extended)

`Material` gains one field:

- `emissive: [f32; 3]` — default `[0, 0, 0]`, components in `[0, 1]`. Added to the final color
  after lighting, unaffected by any light. Cheap to implement, and disproportionately useful to an
  agent: "make this object visible regardless of lighting" is a debugging move worth having on day
  one of having lighting at all.

`albedo`, `metallic`, `roughness` keep their existing meanings and defaults; M4 is when the latter
two start doing something. All three color fields are **linear** RGB (§5), `albedo` and `emissive`
in `[0, 1]`, `metallic` and `roughness` in `[0, 1]` (now validated, §7).

Both new components are one line each in the `components!` macro, which keeps the schema, the
`did_you_mean` name list, and spawning in sync by construction. After adding them:
`engine list-components > schemas/component-schema.json`, or `repo_contracts.rs` fails.

## 3. Defaults when a scene has no lights

**Rule: if a scene contains zero light components (no `DirectionalLight`, no `AmbientLight`), the
renderer applies a documented fallback rig. If it contains at least one light component, exactly
what is written applies — absent means off.**

The fallback rig is: white directional light, intensity 1.0, from the same bearing as M2's
hardcoded placeholder (arriving from `normalize((0.4, 1.0, 0.6))`), plus white ambient at
intensity 0.15.

Why a fallback at all: every existing scene has no light entities, and a lighting milestone whose
first observable effect is turning every screenshot black is the worst possible outcome for the
edit→see loop. Why the sharp all-or-nothing rule instead of per-slot defaults ("no ambient → small
default ambient"): one rule with no interaction matrix. The rejected alternative — always
defaulting ambient unless overridden — makes "sun plus pitch-black shadows" inexpressible without
knowing to write an explicit zero-intensity `AmbientLight`, and hidden contributions an agent
didn't write are exactly the "hidden state" invariant 2 bans. The cost: adding only a Sun gives
black back faces. The demo scene models the fix (include both a Sun and an Ambient entity), and
the component reference documents it.

This is the same shape as the existing precedent: a camera without a transform sits at the origin
— a documented, deterministic default rather than an error or a guess.

## 4. Shading model: simplified PBR, not Phong

The design doc left "basic Phong or simplified PBR" open. **Simplified PBR** — and the decision is
mostly already made by M2: `Material` ships `metallic` and `roughness` in the published schema,
which is the metallic/roughness parameterization every mainstream engine and every glTF file uses
(relevant the moment M3 lands). Phong's `shininess` has no honest mapping from those fields, and
metallic/roughness is the vocabulary an LLM already knows. The full model is ~40 lines of WGSL.

Per fragment, with `N` the normal, `V` toward the camera, `L` toward the light:

- **Diffuse:** Lambert. `F0 = mix(vec3(0.04), albedo, metallic)`; the diffuse term is scaled by
  `(1 - metallic)` so pure metals have no diffuse.
- **Specular:** Cook-Torrance with GGX normal distribution, Smith height-correlated visibility,
  and Schlick Fresnel. `alpha = roughness²` (perceptual roughness convention), with `roughness`
  clamped to a floor of `0.045` in the shader so a scene that writes `0.0` gets a very tight
  highlight instead of NaN/Inf.
- **Intensity convention:** the Lambertian `1/π` is folded into the light (the usual punctual-light
  convention), so a white light at `intensity: 1.0` hitting an `albedo: [1,1,1]` surface head-on
  yields ≈1.0, not ≈0.32. Predictability for the agent beats radiometric purity: "intensity 1 on a
  white surface reads white" is a rule you can verify in a screenshot.
- **Ambient:** `albedo * ambient_color * ambient_intensity`, added flat.
- **Emissive:** added last, unlit.
- **Output:** `clamp(color, 0.0, 1.0)`, then sRGB encoding via the target format (§5). No tone
  mapping in M4 — clamping is deterministic, trivial to reason about when writing pixel
  assertions, and blown-out highlights are a visually legible artifact. Reinhard/ACES becomes
  worth revisiting only if HDR workflows appear (§10).

## 5. Color space — the decision M4 forces

Lighting math must run in linear space; that is not negotiable if `roughness` and `metallic` are
to look like they do everywhere else. The question is what happens at the output, and it
invalidates an M2 decision: `offscreen.rs` deliberately uses `Rgba8Unorm` so that "albedo 0.5
comes back as byte 128." That identity died the moment M2's placeholder diffuse multiplied the
albedo; M4 buries it.

**Decision: render targets become sRGB.** The offscreen target switches to `Rgba8UnormSrgb`, and
the windowed viewer prefers an sRGB surface format. The hardware does the encode; readback of an
sRGB texture yields sRGB-encoded bytes, which is exactly what a PNG is conventionally assumed to
contain — so screenshots become *more* correct in image viewers, not less. The rejected
alternative, manual `pow(c, 1/2.2)` in the shader, is an approximation of the real sRGB curve and
breaks any future blending.

**Scene colors are linear, with no hidden decode.** `albedo: [0.5, 0.5, 0.5]` means 50% linear
reflectance; the engine never silently reinterprets authored values as sRGB the way art-pipeline
engines do. One less invisible transform between the file and the pixel (invariant 2), at the
cost that mid-gray in a file is not mid-gray on screen — `0.5` reflectance under full light
encodes to byte ≈188. The component reference must state this plainly: *scene colors are physical
reflectance; the PNG pixel is the lit, sRGB-encoded result.*

Consequence for tests: every existing pixel assertion in `headless_render.rs` changes value. Test
helpers get an `srgb_encode(f32) -> u8` function so expectations are computed, not eyeballed.

## 6. Renderer changes

Extraction stays plain data, GPU code stays query-free — the M2 split that keeps everything
testable headlessly:

- `RenderItem.albedo: Vec3` becomes `RenderItem.material: Material` (the whole struct).
- New extraction on `Scene`: `lights() -> LightRig`, where `LightRig` is plain data holding the
  optional sun (component + world-space travel direction, i.e. `transform.quat() * -Z`) and the
  optional ambient. `LightRig::resolved()` applies §3's fallback rule and returns concrete values.
  Multiple-light detection lives in validation, not here; extraction takes what the (already
  validated) world contains.
- `ScenePass` gains the resolved light values and the camera's world position (translation of the
  camera's model matrix — needed for the specular view vector).

Uniform layout — group 0 stays per-object, a new group 1 is per-pass:

```
group(0) ObjectUniform:  mvp: mat4, model: mat4, normal_matrix: mat4,
                         albedo_metallic: vec4,  // rgb + metallic in w
                         emissive_roughness: vec4 // rgb + roughness in w
group(1) FrameUniform:   camera_pos: vec4,
                         sun_direction: vec4,     // xyz = direction light travels
                         sun_color: vec4,         // rgb premultiplied by intensity
                         ambient: vec4            // rgb premultiplied by intensity
```

Scalars ride in the `w` lanes of vec4s so the struct needs no padding fields to satisfy WGSL
alignment. `model` is new in the object uniform because the fragment shader now needs world-space
position; the vertex shader gains a `world_position` output. The sun uniform stores the direction
light *travels*; the shader negates it for `L`. The fallback rig and a real sun are
indistinguishable at this layer — a scene with no lights uploads the fallback values, nothing in
WGSL branches on "is there a light."

`mesh.wgsl`'s placeholder fragment shader is replaced wholesale; its header comment already
promises exactly that.

## 7. Validation additions

All new checks are semantic-pass checks with file/line via `lineindex`, reported all-at-once by
`engine validate`, one `EngineError` each:

- `multiple_directional_lights`, `multiple_ambient_lights` — same shape as
  `multiple_active_cameras`, naming the offending entities.
- `value_out_of_range` — one code for every numeric-range violation, with `field`, the offending
  value, and the allowed range in the message: `metallic`, `roughness` in `[0, 1]`; `albedo`,
  `emissive`, and both light `color`s component-wise in `[0, 1]`; both `intensity` fields `>= 0`.
  An error rather than a silent clamp: a clamp is the failure mode where the agent edits a value,
  re-renders, and nothing changes.

Unknown fields and misspelled component names (`"DirectionelLight"` → `did_you_mean:
"DirectionalLight"`) already fall out of `deny_unknown_fields` and the macro-generated name list —
no new work, but worth a test asserting it.

## 8. `builtin:sphere`

A flat-shaded cube under a directional light is three constant-colored quads — it proves N·L runs,
and nothing else. Roughness, metallic, and Fresnel are only *visible* on curved geometry, and
"visible in a screenshot" is this engine's whole verification story. So M4 adds `builtin:sphere`
to `BuiltinMesh`: a UV sphere, 32 segments × 16 rings, unit radius, smooth normals (= normalized
positions, which is the property that makes a sphere the ideal lighting probe). One new enum
variant, one entry in `ASSETS`, mesh-construction tests matching the cube's (counts, normal
lengths, winding — backface culling is on, and §"Verification" of CLAUDE.md documents why winding
bugs are invisible rather than loud).

## 9. Demo scene, schema, and the acceptance loop

`examples/scenes/demo_scene.json` gains a `Sun` (Transform + DirectionalLight), an `Ambient`
entity, and a sphere entity with a mid-roughness non-metal material next to an existing cube — so
the checked-in demo exercises every M4 feature and models the recommended sun+ambient rig.
`schemas/component-schema.json` is regenerated.

**Acceptance criterion** — the §1 success loop of the design doc, run end to end:

1. Add a `Sun` entity to a scene by editing JSON. `engine validate` passes.
2. `engine screenshot` → the sphere shows a bright side, a dark side, and a specular highlight.
3. Edit the sun's `rotation`, re-screenshot → the shading visibly moves accordingly.
4. Misspell a field → structured error with line number and `did_you_mean`.

Step 3 as experienced by an agent looking at two PNGs is the milestone.

## 10. Deferred, with reserved shapes

- **Texture maps** (blocked on M3 UVs): reserve the naming convention now — texture-valued fields
  are the factor field's name plus `_map` (`albedo_map`, `roughness_map`), each an optional
  relative path per invariant 3, each multiplying/replacing its factor. Reserving the names costs
  nothing and prevents a schema break later.
- **Multiple / point / spot lights:** the component union and validation make adding them cheap;
  the uniform layout would grow a light array. Not needed to prove the concept.
- ~~**Shadows:**~~ **done in M16** — a single directional map, exactly the follow-up sketched
  here. The prediction held: unshadowed objects really were visually floating, and grounding them
  is the largest readability change the renderer has had.
- ~~**IBL:**~~ **partly done in M16** — there is no image-based lighting and no prefiltered
  environment map, but when a scene draws a sky, surfaces reflect that sky's gradient and the
  ambient term is modulated by the sky hemisphere. That covers what IBL was wanted for here
  (metal and water that do not read as dark plastic) without an asset pipeline.
- **Tone mapping / HDR emissive:** still deferred, and clamping still has not demonstrably hurt.
  Note that M16 strengthens the case rather than weakening it: sky colors are deliberately
  unclamped above 1 because a sky is a light source, so there is more range being thrown away at
  the end than there was.

## 10a. Transparency (M16)

`Material` gained two fields, and they are deliberately *two*:

- `alpha` is a flat, view-independent blend — the "ghost this object" knob, the one to reach for
  to see through something while debugging.
- `transmission` is view-dependent and keeps the specular lobe. It scales diffuse by
  `1 - transmission` (light that went through did not scatter back off the surface) and lerps
  opacity back toward opaque at grazing angles through Fresnel. That Fresnel behavior is the whole
  difference between a transparent object and a merely faded one: water seen edge-on reflects the
  sky and hides its bottom, and seen from overhead it does neither.

Anything with `alpha < 1` or `transmission > 0` draws in a second pass, sorted back-to-front,
depth-tested but not depth-writing. The shader emits **premultiplied** color for those materials
so the specular highlight and the reflected sky survive being blended at low alpha — attenuating
them along with the diffuse is the obvious implementation, and it loses the reflection exactly
where a clear surface should be at its most reflective.

Not done, and worth naming: **no refraction and no scene-color sampling.** What is behind a
transmissive surface is neither bent nor tinted by how much material it passed through, so a thick
block of ice is exactly as clear as a thin one. Sorting is per object by origin distance, so two
interpenetrating transparent objects can still blend in the wrong order.

## 11. Test plan

`engine-core` (no GPU):

- Direction convention pin (analogous to the existing Euler pin test): rotation `[-90, 0, 0]`
  makes the light travel `(0, -1, 0)` — straight down.
- `LightRig`: extraction of sun + ambient; fallback applies with zero light components; fallback
  does **not** apply when any single light component exists (the §3 rule, both halves).
- Component defaults and JSON round-trips for both new components; `emissive` default.
- Validation: each new error code fires with correct file/line; `value_out_of_range` for a
  representative field of each kind; misspelled component name suggests the right one.
- Sphere mesh invariants (§8).
- `repo_contracts.rs` forces the schema regeneration.

`engine-render/tests/headless_render.rs` (skips cleanly without a GPU, as today):

- Sun at a known angle on a cube: the lit face's pixels are brighter than the unlit face's.
- Rotate that sun 180°: the ordering flips. (Step 3 of the acceptance loop, as a regression test.)
- Ambient-only scene: uniformly lit, not black, all visible faces equal.
- Emissive material with an explicit zero-intensity light (which disables the fallback rig):
  pixels ≈ `srgb_encode(emissive)`.
- Sphere, roughness 0.1 vs 0.9: the smooth sphere's brightest pixel is brighter (tighter, hotter
  highlight) — a coarse but stable assertion that specular responds to roughness.
- Updated expectations everywhere else via `srgb_encode` (§5).

## 12. Build order within M4

Each step leaves the workspace green:

1. `engine-core`: new components + `Material.emissive`, `LightRig` extraction, validation, tests;
   regenerate the schema.
2. `builtin:sphere` + tests.
3. `engine-render`: uniform restructure, new `mesh.wgsl`, sRGB target switch, updated headless
   tests. (The one step that touches wgpu — per CLAUDE.md, read the wgpu 30 API from the registry
   source, not from memory.)
4. Demo scene update; render it; **look at it.**
