# M26 — The Material System: Design

Milestone M26. `Material` today is six factors in the object uniform; this document settles what it
becomes when a surface can carry images, when a material can be shared between entities, when
transmission can bend what is behind it, and when a `.glb` can bring its own appearance. Written
2026-07-31, against the post-M25 codebase.

The order of the sections is the order the constraints bind: §3 is a hard limit the current renderer
is already two-thirds of the way into, and it decides more of the design than any aesthetic choice
below it.

## 1. Scope

**In:**

- Texture maps on `Material`: `albedo_map`, `orm_map`, `normal_map`, `emissive_map`, with a
  `uv_scale`/`uv_offset` transform, `alpha_cutoff`, and the sampling/mip/color-space rules that make
  them not alias.
- A texture half of the asset pipeline: `TextureSource` beside `MeshSource`, `Arc`-identity caching
  under M15's rule, CPU mip generation, validation of size and reference at `engine validate` time.
- **Shareable materials**: `Material.asset` referencing a `materials/*.json`, exclusive with inline
  fields (§5).
- **Refraction**: `ior` and `thickness`/`attenuation` on transmissive materials, fed by a scene-color
  copy alongside M18's depth copy.
- **glTF material import**: `engine import`, plus the editor's drag-and-drop routed through it.
- The bind-group reorganization in §3, which is the precondition for all of the above and
  independently unblocks water refraction.

**Out, deferred with reasons in §12:** image-based lighting and prefiltered environment maps,
parallax/displacement mapping, per-material shader graphs, texture compression (BC/ASTC), texture
arrays or atlasing, animated/scrolling textures beyond the static UV transform, decals, subsurface
scattering, clearcoat/sheen/anisotropy lobes, and material overrides layered on a referenced asset
(§5 rejects that one on its merits, not on effort).

**Explicitly not reopened:** consolidating `water.wgsl`, `road.wgsl` and `clouds.wgsl` back into
`mesh.wgsl`. Those files duplicate the lighting body deliberately, for the M16 reason, and a
milestone about materials has no more business editing the four untouchable lines than M23 had
re-blessing terrain's baseline. §2 gets the sharing benefit a different way.

## 2. The seam already exists — name it

M22 discovered the extension point this milestone needs and did not name it. `with_terrain` takes
`mesh.wgsl` and replaces exactly three things: the fragment prologue that reads `albedo`, `metallic`
and `emissive` off the object uniform; the line that computes the shading normal; and the roughness
floor. Everything after those three points — the GGX lobe, the shadow lookup, the sky ambient, the
point-light loop, the fog, the blend — is untouched and shared.

That is a **surface-resolution seam**, and everything this milestone adds is another producer at it:

```
                     ┌─ uniform factors  (today, the file on disk)
    Surface {        ├─ terrain generator (M22, spliced)
      albedo         ├─ texture maps      (M26, spliced)
      metallic       └─ …
      roughness
      normal      }  ──→  the one lighting body, byte for byte the M4/M16 code
      emissive
      alpha
      occlusion
```

So M26 generalizes `with_terrain` into `with_surface(producer)`: the same anchored substitutions,
the same startup assertion that each anchor appears exactly once, one producer per variant. The
generalization is worth doing even though there are only two producers, because the anchors are
currently spelled out as four `const`s inside one function and the second caller is the moment they
become a contract rather than an implementation detail.

**Three properties this preserves, all of them load-bearing:**

1. `mesh.wgsl` on disk stays byte for byte the file it is today. The plain pipeline compiles it
   unmodified, so the default path is bit-exact **by construction rather than by measurement** — the
   distinction M22's comment is at pains to make.
2. A material with no maps compiles to the plain pipeline. Not a textured pipeline with white
   textures bound: `x * 1.0` is exact in IEEE-754, but that was never the risk — the risk is that
   inserting the multiply changes the code *around* the untouchable lines, and whether the compiler
   contracts `a*b + c` into an FMA depends on exactly that. M22 measured this and paid one pixel in
   each of three fixtures for it.
3. Terrain does not become a texture consumer in this milestone, so there is no terrain × textured
   variant. Textured terrain layers are a good idea and §12 keeps it; it is not free, and it doubles
   a variant matrix that MSAA already multiplies by two.

## 3. The bind-group budget — the constraint that actually decides things

`gpu.rs` requests `wgpu::Limits::downlevel_defaults()`. Read from the registry rather than from
memory, that gives `max_bind_groups: 4` — and note `Limits::defaults()` is also 4, and so is
WebGPU's own limit, so this is not a floor the engine chose and can casually raise.

Where the four slots go today:

| pipeline | 0 | 1 | 2 | 3 |
|---|---|---|---|---|
| mesh    | object (dynamic offset) | frame | shadow | **free** |
| terrain | object | frame | shadow | **free** |
| road    | object | frame | shadow | road markings |
| water   | water  | frame | shadow | scene depth |

Meshes have one slot left. Roads and water have none. And the wanted work does not fit:

- textured meshes need a material group → takes the last mesh slot;
- **refracting** meshes need scene color as well → a fifth group, over the limit;
- water refraction — the feature CLAUDE.md calls the loudest missing one — needs scene color beside
  its scene depth, and water is already full;
- textured roads (asphalt grain, named in the M23 deferred list) have nowhere to go at all.

So the budget is spent before the milestone starts, and no amount of care in the shader fixes it.

**The change: group 2 stops being "the shadow map" and becomes "frame textures".** Shadow map +
comparison sampler, scene depth + sampler, scene color + sampler — six bindings in one group. Every
one of them is frame-scoped: written once per frame, read by everything, rebuilt only when the
render target resizes. They were three groups only because they arrived in three milestones.

| pipeline | 0 | 1 | 2 | 3 |
|---|---|---|---|---|
| mesh / terrain | object | frame | frame textures | **material** |
| road | object | frame | frame textures | road markings *(→ material, §12)* |
| water | water | frame | frame textures | **free — water refraction lands here** |

Three consequences worth stating plainly:

- **The plain mesh pipeline's shader does not change.** A bind group layout may contain entries the
  shader never references; the reverse is the error. So `mesh.wgsl` keeps declaring `shadow_map` and
  `shadow_sampler` at `@group(2) @binding(0..1)` exactly as it does now, and the two new pairs are
  declared only by the spliced variants that use them. The on-disk file is untouched for the third
  milestone running.
- **This is nonetheless an A/B-check gate, not an assertion.** Growing a layout should not move a
  pixel, and every argument above says it cannot. The repo's own rule is that the check which settles
  a bit-exactness question is an A/B between binaries, and this one runs before anything else in the
  milestone lands — see §10.
- **Roads keep their group 3 in v1** and therefore take no texture maps. That is the one surface
  where it costs least: M23 established that analytic markings beat a texture for anything periodic,
  and the wanted texture there is grain, which §12 keeps. The clean end state is that a road's kerb
  table *is* its material and moves into group 3 beside the maps; folding it into the object uniform
  instead is rejected outright, because the object uniform is one buffer with a shared stride
  addressed by dynamic offset, so a `MAX_ROAD_KERBS` array in it inflates the per-draw slot of every
  cube in every scene.

Two more limits from the same read, both of which have teeth:

- `max_texture_dimension_2d: 2048` under downlevel defaults. **A 4096² texture does not load today.**
  §4 makes that a validation error rather than a device panic.
- `max_sampled_textures_per_shader_stage: 16`. Three frame textures + four material maps = seven,
  comfortable, but it is the ceiling that says "four maps and a packed ORM", not "one map per
  property".

## 4. Textures as assets

### 4.1 The type and the cache

`engine-assets::load_texture` already exists and already normalizes everything to RGBA8; it has been
sitting unused since M3, which is exactly the "awaiting texture-mapped materials" note in CLAUDE.md.
What it lacks is the shape M15 made mandatory:

```rust
pub trait TextureSource {
    fn load_texture(&self, asset: &str) -> Result<Arc<TextureData>>;
}
```

`Arc`, and the **same `Arc` for the same asset** — M15's upload cache keys on `Arc` identity, and a
`TextureSource` that mints a fresh `Arc` per call re-uploads every texture every frame. `AssetServer`
grows a second `HashMap` beside its mesh cache and gains the trait; `BuiltinAssets` gains a
`builtin:white` 1×1 for tests and for the "declared but missing" path. This is the mesh contract
transposed, deliberately, so there is one rule to learn.

### 4.2 Color space is a property of the slot, not of the file

A PNG does not say whether its bytes are an sRGB-encoded color or linear data. Albedo and emissive
maps are colors and must be uploaded `Rgba8UnormSrgb` so the hardware decodes them; ORM and normal
maps are **data** and must be uploaded `Rgba8Unorm`, or roughness comes back gamma-decoded and every
surface is smoother than it was authored.

So the *slot* decides the format, never the file and never a field. `albedo_map` and `emissive_map`
are sRGB; `orm_map` and `normal_map` are linear. There is nothing to configure, which is the point:
this is the single most common texture bug in any engine, and the only reliable fix is to make it
unrepresentable.

This also sits correctly with M4's sRGB decision: scene colors are linear reflectance and the render
target encodes on write. A texture is just another way to spell a linear reflectance, and the
hardware sampler does the decode for free.

### 4.3 Mipmaps are required, and generated on the CPU

This engine has learned the same lesson twice already. Water's detail normals fade with view distance
because without the fade "sub-pixel ripples alias into sparkle that reads as broken rather than as
low quality". Terrain's per-pixel bump does the same. A texture minified without mips is that bug in
its original form, and it will be at its worst on exactly the surfaces this milestone is for — a
tiled asphalt or bark texture seen down a 546 m circuit.

Generated **on the CPU at load**, by a box filter written out in-repo, for the reason every generator
in this repo is written out in-repo: a render sits under a committed baseline, so the filter is a
format contract, and "the `image` crate changed its resampler" must not be able to surface as a
renderer regression. It also keeps the whole path GPU-free and unit-testable, like `particles.rs`,
`tree.rs`, `cloud.rs` and `terrain.rs` — a GPU blit-chain would be faster to run and impossible to
test on CI, which "proves the GPU-free half only".

Non-power-of-two sources are allowed and mip down by integer halving with the odd dimension rounded
up, which is what every box-filter chain does; the alternative — refusing NPOT — buys nothing on any
backend the engine targets.

### 4.4 Samplers, tiling, and the one knob that could move pixels

One sampler per distinct configuration, not one per texture, against the 16-sampler ceiling. Default
address mode is **repeat** on both axes, because tiling is what a material texture is for, and
`ClampToEdge` would make `uv_scale: 20` draw one stretched copy surrounded by smeared border pixels.

`anisotropy_clamp` is the knob to be careful with. It measurably improves exactly the grazing-angle
case this milestone cares about, and it is also a per-adapter quality setting — the thing this repo
has repeatedly found to be where reproducibility goes to die. **v1 pins it at 1** (off) so that a
baseline is a function of the scene and not of the driver's filtering quality, and §12 keeps raising
it as a scene-level `environment` knob, which is where a quality dial belongs and where it can
default to off.

### 4.5 Validation, at validate time

Texture references resolve exactly like `Mesh.asset` and like M14's fragment meshes: the reference
check (existence, extension, absolute-path rejection) in `engine-core`, the decode in
`engine-assets`, and `engine validate` runs both passes, so a corrupt PNG fails validation rather
than the screenshot.

New codes, all additive:

- `texture_too_large` — over `max_texture_dimension_2d`, reported with the actual and the limit.
  This is `tree_too_complex`'s precedent applied: refuse before allocating, because "a hung render
  with no output is the worst failure an agent loop can hit", and a device-limit panic is worse
  still.
- `material_asset_with_fields` — §5.
- `material_asset_not_found` / decode failures reuse `asset_not_found` / `asset_load_failed`, which
  already carry file and line.

`invalid_field_type` and `value_out_of_range` cover the rest through the schema walk, since every new
field is a string path, a `[f32; 2]`, or a ranged scalar. Codes are API; nothing is renamed.

## 5. Material identity: inline, or a file, never half of each

`Material` gains `asset`, mirroring `Mesh.asset` — a scene-relative path to a `materials/*.json`
holding one material object. Invariant 3 is satisfied by construction: a relative path, no opaque ID,
no lookup table.

**`asset` is exclusive with every other field.** A `Material` that names an asset and also sets
`roughness` is `material_asset_with_fields`, with the message naming both.

That is the decision most likely to be re-litigated, so the reasoning is worth having in writing.
Layered overrides — "the asphalt, but tinted" — are genuinely useful and every mature engine has
them. They are rejected here for a specific, mechanical reason: **serde cannot distinguish an absent
field from one written at its default value.** Every field on `Material` has a default and is
`#[serde(default)]`; given `{"asset": "asphalt.json", "roughness": 0.9}` there is no way to know
whether 0.9 is an override or someone spelling out the default, so the resolved material depends on
information the file does not contain. That breaks the property M24 leaned on — absent fields *are*
the documented defaults — and it breaks the rule that the file predicts the scene. Making every
field `Option<T>` when `asset` is present would fix it and would also mean the component's schema
changes shape depending on another field's presence, which the schema-driven validation walk and the
editor's generated widgets both read directly.

The single-owner shape is also the house style: `daylight` and an authored `DirectionalLight` are a
validation error because two owners of one sun is what invariant 8 exists to prevent. A material file
and a field on top of it is the same shape of mistake. Materials are small JSON; a variant is a
second file, and `engine inspect` will tell you what either one resolves to.

The file's contents are the component's fields minus `"type"`, so the schema published by
`engine list-components --component Material` describes both forms and there is nothing new to learn.
`engine validate` accepts a material file directly, the way M9 made it accept clip files.

`unused_material` keeps working and gets slightly smarter: a `materials/*.json` that no scene entity
references is not warnable at scene scope (it may be shared by other scenes), so the warning stays
where it is — on an entity carrying a `Material` that nothing draws.

## 6. The material component after M26

```json
{
  "type": "Material",
  "albedo": [0.8, 0.8, 0.8],
  "metallic": 0.0,
  "roughness": 0.9,
  "emissive": [0.0, 0.0, 0.0],
  "alpha": 1.0,
  "transmission": 0.0,

  "albedo_map": "textures/bark_albedo.png",
  "orm_map": "textures/bark_orm.png",
  "normal_map": "textures/bark_normal.png",
  "emissive_map": null,
  "uv_scale": [1.0, 1.0],
  "uv_offset": [0.0, 0.0],
  "alpha_cutoff": 0.0,
  "normal_strength": 1.0,

  "ior": 1.0,
  "thickness": 0.0,
  "attenuation": [1.0, 1.0, 1.0]
}
```

Every added field defaults to the pre-M26 behaviour: no maps, an identity UV transform, no alpha
cut, `ior: 1.0` (no bending), `thickness: 0.0` (no absorption). A scene that omits all of them
renders on the plain pipeline, which is the file on disk. This is the M16 rule — "every one of them
defaults to off, and that is the design, not a convenience" — and it is what lets thirty committed
baselines survive the milestone.

The naming follows what M4 reserved for exactly this moment: a texture-valued field is the factor's
name plus `_map`, and it **multiplies** its factor rather than replacing it, so `albedo` keeps
working as a tint over `albedo_map` and the default `[0.8, 0.8, 0.8]` does not silently darken every
imported texture — §11 covers the one wrinkle there.

Three fields deserve their reasoning:

- **`orm_map` packs occlusion, roughness and metallic into R, G, B**, which is glTF's
  `occlusionTexture` + `metallicRoughnessTexture` convention. Adopting it rather than taking three
  separate maps means glTF import is a file copy rather than a channel re-pack (§9), it is what every
  authoring tool already exports, and it costs two texture slots and two samplers per material
  against the ceiling in §3. Occlusion multiplies the ambient and sky terms only — never the direct
  sun, which is what makes it "ambient occlusion" rather than a second shadow.
- **`alpha_cutoff`** exists for alpha-cut foliage, which CLAUDE.md names as one of the two things
  that would most improve the forest. It has a consequence beyond the mesh pass: a cut-out leaf must
  cut its **shadow** too, and `shadow.wgsl` deliberately has no fragment stage. So a second shadow
  pipeline, with a fragment stage that samples the albedo map's alpha and discards, is part of this
  milestone — used only by materials with `alpha_cutoff > 0`, leaving the depth-only pipeline that
  every current scene uses untouched.
- **`normal_strength`** is a plain scale on the tangent-space XY, because the first thing anyone does
  with a normal map is discover it is too strong, and the alternative is re-authoring the texture.

### Tangents: derived, not stored

Normal mapping needs a tangent frame and `MeshData` carries positions, normals, UVs and indices —
no tangents. The frame is derived **per pixel from screen-space derivatives** of position and UV,
rather than stored per vertex.

This is the cheaper choice by a distance in this codebase specifically: it adds nothing to
`MeshData`, so no `Arc` changes identity, nothing re-uploads, no glTF loader change, and — the part
that matters — it works unmodified for `Water`, `Terrain`, `Road`, `Tree` and `Cloud`, which generate
their geometry and would each need a tangent generator of their own. The cost is a slightly noisier
frame on low-poly geometry and undefined behaviour on degenerate UVs, both of which are acceptable at
the quality level of an engine whose sky is a gradient. §12 keeps stored tangents for when a
normal-mapped hero asset makes the difference visible.

## 7. Refraction

`transmission` today is a Fresnel lerp with the diffuse scaled by `1 - transmission`, and the M4 doc
names the gap precisely: "no refraction and no scene-color sampling … a thick block of ice is exactly
as clear as a thin one". M26 closes it with two fields and one new frame texture.

**The pass structure is M18's, extended by one copy.** Water already forced the split, for the same
reason refraction needs it — a pass cannot sample its own attachments:

```
opaque  →  copy depth (R32Float)  +  copy color (Rgba8UnormSrgb)  →  water & blended  →  particles
```

The color copy is gated the way `water_present` gates the depth copy: it happens only if the scene
contains a material with `ior != 1.0` or `thickness > 0.0`, or a refracting water surface. With none,
the pass structure, the attachments, and the load/store ops are byte for byte the pre-M26 ones.

**Refraction is a branch, not a pipeline variant, and that falls out of M16's structure.** Anything
transmissive already fails `blended == false` and has already left the default path through the
combined `if shadowed || lit_sky || blended`. So the bending and the absorption go inside a branch
that only transmissive surfaces reach, and the untouchable lines never see them. This is a real
dividend from M16's discipline and worth noticing: the feature most likely to disturb the shader is
the one the existing branch structure already isolates.

The model is the standard thin-surface screen-space approximation:

- `ior` (1.0–3.0, default **1.0 = no bending**) refracts the view vector against the shading normal;
  the resulting direction is projected to screen space, scaled by `thickness`, and used to offset the
  scene-color sample.
- `thickness` (default 0) is both the offset scale and the Beer–Lambert path length: transmitted
  color is attenuated by `exp(-(1 - attenuation) * thickness)`, so a thick block of ice is finally
  greener than a thin one, and `attenuation` is a linear-RGB color like every other color in the
  engine.
- The sample is clamped to the frame, and falls back to the un-refracted sample at the edges. A
  refraction that reads outside the frame has no data to read — the honest failure is a slightly wrong
  edge, not a black smear.

**Three limitations, named rather than discovered later:** what is refracted is the *opaque* frame,
so a transparent object cannot refract another transparent object behind it (M18's depth copy has
exactly this limitation and it has not hurt); the offset is screen-space, so refraction through a
strongly curved surface is an approximation, not a ray; and sorting stays per-object by origin
distance, so two interpenetrating transmissive objects can still blend in the wrong order.

Water gets refraction from the same scene-color texture, in the group-3 slot §3 frees up. It is the
same shader work and the design doc for water already names the missing piece; whether it lands in
M26 or immediately after is a scheduling call, not a design one.

## 8. What this does to the pipeline matrix

Pipelines are built per `samples` value, so every variant costs two. The mesh family goes from two
variants (plain, terrain) to four:

| variant | when | surface producer |
|---|---|---|
| plain | no maps, no cutout | the file on disk, unmodified |
| textured | any `*_map` set | spliced texture producer |
| terrain | `Terrain` component | spliced terrain producer (M22, unchanged) |
| shadow-cutout | `alpha_cutoff > 0` | shadow pass + fragment discard |

Four is acceptable. The reason to write the matrix down is that it is the thing that grows
quadratically if a later milestone adds producers carelessly — textured terrain is producer × producer
— and §2's `with_surface` generalization is what keeps the growth in one place where it can be seen.

## 9. glTF material import

`gltf::import` already returns `(document, buffers, images)` and `gltf_mesh.rs` discards the third
with a `_images` binding. The material data this milestone wants is therefore *already being parsed
and thrown away*, which makes import much closer to plumbing than to a feature.

**`engine import <file.glb> [--into <scene.json>] [--textures <dir>]`** is a new CLI command, and the
editor's drag-and-drop calls it rather than reimplementing it. That direction matters: the agent is
the primary user, and an import path reachable only by dropping a file on a GUI is exactly the
"bespoke integration layer" the project exists to avoid. The editor's existing `import.rs` already
splices `Transform` + `Mesh` via `formatter::apply_add_entity`; it gains a `Material` in the same
splice.

Mapping, which is close to 1:1 by §6's design:

| glTF | engine |
|---|---|
| `baseColorFactor` / `baseColorTexture` | `albedo` / `albedo_map` |
| `metallicFactor`, `roughnessFactor` | `metallic`, `roughness` |
| `metallicRoughnessTexture` + `occlusionTexture` | `orm_map` |
| `normalTexture` (+ `scale`) | `normal_map` (+ `normal_strength`) |
| `emissiveFactor` / `emissiveTexture` | `emissive` / `emissive_map` |
| `alphaMode: MASK` + `alphaCutoff` | `alpha_cutoff` |
| `alphaMode: BLEND` | `alpha` |
| `KHR_materials_transmission`, `_ior`, `_volume` | `transmission`, `ior`, `thickness`/`attenuation` |

Two rules keep it inside the invariants:

- **Embedded images are written out as PNG files**, next to the scene under `--textures` (default
  `textures/`), named deterministically from the glTF material and slot names, deduped by content
  hash so two materials sharing a map share the file. A GLB's images live in a binary buffer, and
  leaving them there would be a binary asset referenced by index — invariants 1 and 3 both. Writing
  them out is what makes the import result an ordinary, diffable, hand-editable scene.
- **Import writes a `materials/*.json` per glTF material and references it**, rather than inlining.
  A glTF model routinely has several primitives sharing one material, which is precisely the case §5
  added file references for.

Occlusion is the one lossy spot: glTF allows the occlusion texture to be a different image from the
metallic-roughness one, while `orm_map` packs them. When they differ, import repacks R from one and
GB from the other and says so on stderr as a warning; when the occlusion texture is absent, R is
filled with 1.0.

## 10. Verification

The order is the milestone skill's, with one addition at the front.

**Before anything else lands: the A/B check on the bind-group merge alone.** §3 changes a layout that
every existing pipeline uses, and the argument that a layout cannot move a pixel is an argument, not
a measurement. Build the CLI at `main` and in the worktree with *only* the group-2 merge applied and
run `bin/verify-baselines --render-to` against both across all 30 manifest entries. If that is not
byte-identical, the whole shape of §3 is wrong and it is much cheaper to know before four features
are stacked on it.

Then, per stage:

- `bin/engine validate examples/scenes/*.json --strict`
- `cargo test --workspace`
- `bin/verify-baselines` — 30 of 30 unchanged is the standing claim; anything else is a bug in this
  milestone until proven otherwise.
- The `ab-check` skill again at the end, because the milestone touches shaders, the pass structure
  and the asset path.

GPU-free tests, which is where most of the confidence should come from:

- Mip chain: a known image box-filters to known values; odd dimensions round up; the chain ends at
  1×1; the filter is pinned by a golden vector, since it is a format contract (§4.3).
- `TextureSource` returns the identical `Arc` for repeat loads of one asset — the M15 rule, asserted
  rather than assumed.
- Color-space assignment is per slot: a table test that `albedo_map`/`emissive_map` request an sRGB
  format and `orm_map`/`normal_map` a linear one. This is the §4.2 bug, and it is invisible in any
  test that does not assert on the format.
- `Material.asset` resolution: a referenced file's fields land; `material_asset_with_fields` fires on
  any inline field beside `asset`; a missing file reports with the *scene's* line, a malformed one
  with the material file's own line (M9's clip-error precedent).
- `texture_too_large` fires from `validate`, before any device exists.
- Schema round-trips and defaults for every new field; the `set_field` drift test picks up the new
  numeric fields for free and will fail if `animation.rs` is not updated — texture *paths* are not
  animatable, and the UV transform, `alpha_cutoff`, `ior` and `thickness` are.
- glTF import: a fixture `.glb` with an embedded texture imports to a scene that `validate`s, with
  the image written out and referenced relatively; re-importing is idempotent.

GPU pixel tests, in `engine-render/tests/materials.rs`, all skipping cleanly without an adapter:

- A material with no maps renders identically to the same material through the textured pipeline with
  white maps — the sanity check that the producer seam is neutral. (Identical, not bit-identical: the
  point of §2 is that we do not have to claim the latter.)
- An albedo map with two known halves puts the right color on the right half of a quad — the UV
  orientation pin, which is the thing that is silently upside-down forever if nothing asserts it.
- `uv_scale: [2, 2]` produces four tiles, not one.
- A roughness gradient in `orm_map`'s G channel produces a highlight that tightens across the
  surface — the check that would fail loudly if §4.2 were wrong.
- A normal map perturbs shading on a flat quad lit from a known angle.
- `alpha_cutoff` removes pixels **and** removes their shadow.
- Refraction: a textured backdrop seen through an `ior: 1.5` slab is displaced relative to the same
  slab at `ior: 1.0`; `thickness` with a colored `attenuation` tints it; a scene with no refracting
  material runs the pre-M26 pass structure (the `a_scene_with_no_water_is_untouched_by_the_water_pass`
  precedent, transposed).

Fixture: `verify/m26_materials.json` + baseline — a row of spheres spanning metallic and roughness
with and without maps, a cut-out foliage card casting a cut shadow, and a refracting slab over a
patterned floor. Per §12's own warning about M22, the camera **aims at its subject and not across a
landscape**: no terrain in the frame, so the fixture can carry a hard bit-exact pin rather than a
tolerance. Blessed from the debug binary, listed in `baselines.json`, in the same commit.

And the step none of the above replaces: render it and **look at it**. Every model rule in the tree
and cloud systems came out of looking at a render, and a material system is more subject to that than
either.

## 11. Risks, and the things most likely to go wrong

- **The `albedo` tint over `albedo_map`.** `albedo` defaults to `[0.8, 0.8, 0.8]`, so a texture
  multiplied by the default factor is 20% darker than the artist's file, and an imported model looks
  subtly wrong for a reason nobody guesses. Options are to default the factor to white when a map is
  present (rejected: state-dependent defaults are exactly what §5 refuses), or to have `engine import`
  write `albedo: [1, 1, 1]` explicitly alongside a map (accepted — the importer knows, and the file
  then says so). A hand-authored material still has to be told; the field docs say it and
  `engine inspect` shows it.
- **The group-2 merge is the highest-variance item** and is sequenced first (§10) for that reason.
- **`max_texture_dimension_2d: 2048`** will be hit by the first real texture anyone downloads. The
  error is good, but the fix an agent wants is "downscale it for me", and that is a plausible
  `engine import --max-size` follow-up rather than something to leave as a wall.
- **Pipeline variant growth** (§8) is the long-term one. Four is fine; the milestone after the one
  that adds textured terrain and textured roads is where it stops being fine, and the answer then is
  a uniform-flag ubershader for everything *except* the plain path, which must stay the file on disk.
- **Anisotropy off (§4.4) will look worse than the competition** on tiled ground at grazing angles,
  and someone will want to turn it on. It belongs in `environment` beside `samples`, defaulting off,
  where the existing per-adapter baseline rules already apply.

## 12. Deferred, with the shapes they would take

- **Textured terrain layers** — each `Terrain` layer taking an albedo/ORM map instead of a flat color.
  The natural next step, and the one that makes the ground read as ground; needs the producer ×
  producer variant §8 warns about, which is the real work.
- **Textured roads / asphalt grain** — road markings move from group 3 into the material group (§3),
  which is the tidy end state anyway.
- **Stored tangents** on `MeshData`, when derived frames become the visible limit (§6).
- **Anisotropic filtering** as an `environment` knob, default off (§4.4).
- **IBL and prefiltered environment maps.** M16 already covers what IBL was wanted for here — metal
  and water that do not read as dark plastic — with the sky gradient; a real prefiltered probe is what
  a *reflective* material wants and it is a bigger asset-pipeline question than this milestone.
- **Texture compression (BC7/ASTC)** — a real pipeline concern at real asset sizes, and a
  transcoder is a dependency with a format contract attached. Not until something is actually slow.
- **Parallax / displacement**, **decals**, **clearcoat / sheen / anisotropy lobes**,
  **subsurface scattering**, **animated UV scroll** (the static transform covers tiling; scrolling
  wants the reproducible clock, which is a one-line change when someone wants a conveyor belt).
- **Material overrides on a referenced asset** — rejected on the merits in §5, not deferred for
  effort. Reopening it means solving the absent-versus-default problem first.

## 13. Build order

Each step leaves the workspace green, and the first two are separable enough to land on their own.

1. **The group-2 merge, alone.** No new features. `bin/verify-baselines` and the A/B check from §10.
   This is the step that either validates §3 or sends it back.
2. **Textures as assets:** `TextureSource`, the `Arc` cache, CPU mip generation, color-space-by-slot,
   validation and `texture_too_large`. All GPU-free, all testable, nothing rendered yet.
3. **`Material.asset`** and the material-file format, with `validate` accepting one directly. Also
   GPU-free, and it makes step 4's fixture much easier to author.
4. **`with_surface` + the textured variant:** the schema fields, the material bind group, the sampling
   in a spliced producer. `albedo_map` first, then ORM, then normal. Fixture and baseline here.
5. **`alpha_cutoff`** and the cut-out shadow pipeline.
6. **Refraction:** the scene-color copy, the gate, `ior`/`thickness`/`attenuation`, and water's
   refraction in the freed slot.
7. **`engine import`**, then the editor's drag-and-drop routed through it.
8. The showcase tour takes a textured entity — `repo_contracts.rs` does not force it, since no new
   *component* is added, which is worth noting as a small hole in that contract's premise: this
   milestone adds capability without adding a component, and the tour's growth test cannot see that.
   Add it anyway, and consider whether the contract should grow a field-level notion.
9. CLAUDE.md, and this document's §12 updated with whatever the renders taught.
