# Materials (M26, `designs/material-system-design.md`)

*Relocated verbatim from `CLAUDE.md` when it was compacted to an index. The digest that points here is `CLAUDE.md` §Materials.*

*The design doc for this milestone is `designs/material-system-design.md` — it has the rejected
alternatives; this file has what the build learned.*

`Material` gains texture maps, a file form, and refraction. **Every added field defaults to the
pre-M26 behaviour** — no maps, an identity UV transform, no alpha cut, `ior: 1.0`, `thickness: 0.0` —
which is what let the milestone land with every committed baseline untouched except the six the
showcase tour's own edit re-blessed.

- **The bind-group budget decided the shape.** `downlevel_defaults` caps `max_bind_groups` at 4, and
  three were spent on frame-scoped textures that arrived in three milestones. Group 2 is now **frame
  textures** — shadow map + comparison sampler, depth copy, colour copy + sampler — which gives
  meshes a material slot at 3 and frees water's. Two bind groups are built from it, differing only in
  whether the colour copy is bound: on the refracting path the opaque pass is *drawing into* that
  copy, and a texture cannot be an attachment and a resource in one pass.
- **`with_surface(producers)` is M22's splice, named and generalized.** Terrain, textures and
  refraction are `Producer`s — a prelude plus anchored substitutions against `mesh.wgsl` — composed
  because a textured surface can also refract. Every anchor is asserted to appear exactly once, and
  `every_producer_actually_replaces_what_it_claims` pins that each substitution *landed*: a splice
  that silently did nothing renders the feature as if it were absent, which is the failure mode
  hardest to see. One **shared extended object-uniform tail** goes into every variant, because
  uniform field offsets are positional.
- **Colour space is a property of the slot**, never the file and never a field: `albedo_map` and
  `emissive_map` decode, `orm_map` and `normal_map` do not. It also decides how the mip chain was
  filtered — averaging sRGB-encoded bytes darkens every level — so `TextureSource` keys its cache on
  `(asset, space)`. Mips are generated on the CPU by a box filter written out in-repo, for the reason
  every generator here is: a render sits under a baseline, so the filter is a format contract.
  `texture_too_large` (2048 a side, the device limit) fires from `validate`, before a device exists.
- **`Material.asset`** names a `materials/*.json` and is **exclusive with every other field**
  (`material_asset_with_fields`), checked against the raw JSON rather than the parsed component: every
  field has a serde default, so the parse cannot tell an override from someone spelling out the
  default. A material file's own texture references are relative to **it**, rebased onto the scene
  once at load — that is what makes one shareable. `Material` has a **hand-written `Serialize`** that
  emits only the reference when `asset` is set, so a baked scene points at the file instead of
  inlining a copy that would fail its own validation.
- **Tangent frames are derived per pixel** from screen-space derivatives, so `Water`, `Terrain`,
  `Road`, `Tree` and `Cloud` take normal maps with no tangent generator each and no `MeshData` change
  (no `Arc` changes identity, nothing re-uploads).
- **`alpha_cutoff` cuts the shadow too**, through a second caster pipeline with a fragment stage —
  `shadow.wgsl` has none, and a leaf that cuts its pixels but not its shadow casts the silhouette of
  its own quad. That caster is `cull_mode: None`: **the solid caster is front-face culled**, so a flat
  single-sided card facing the sun is culled out of the shadow map entirely. Worth knowing before
  debugging a missing shadow.
- **Refraction is a third blended pipeline, not a branch in the second, and that was measured.**
  Compiling the refraction variant for every transparent draw moved one pixel of `m16_environment` by
  one channel step — M22's lesson repeating. The transmitted background is added **after fog**: the
  copy was already fogged at its own depth, and fogging it again turned the tour's ice into a pale
  slab. The colour copy is gated like M18's depth copy, so a scene with nothing refracting renders
  the pre-M26 pass structure exactly.
- **`engine import`** writes a glTF's materials out as files and its embedded images as PNGs (deduped
  by an in-repo FNV-1a of their pixels — it decides file names). The editor's drag-and-drop calls it
  rather than reimplementing it. Occlusion is the lossy spot — glTF allows a different image from
  metallic-roughness while `orm_map` packs them — so a repack warns. Re-importing refreshes the files
  and leaves the scene alone.

**Two traps written down.** An unwritten 1×1 placeholder rendered as a stable magenta that looked
exactly like a mip-chain bug and was chased as one; placeholders are written now. And
**`builtin:plane`'s UVs are not the intuitive ones** — `quad(+Y, +Z, +X)` puts `u` along local +Z and
`v` along +X, so a texture's "left half" lands on the top of an upright card. Fixing the builtins'
layout is deferred as its own change with its own A/B.

Fixture `verify/m26_materials.json` + baseline, aimed at its subject with no terrain in frame per
M22's rule, so it carries a hard bit-exact pin. Textures are generated by
`examples/textures/make_textures.py`; the import fixture by `examples/meshes/make_textured_quad.py`.
Not here: IBL and prefiltered probes, parallax, decals, texture compression, stored tangents,
anisotropic filtering (pinned at 1 — a per-adapter quality knob is where reproducibility dies),
textured terrain layers, and textured roads.
