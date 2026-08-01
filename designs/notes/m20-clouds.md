# Clouds (M20)

*Relocated verbatim from `CLAUDE.md` when it was compacted to an index. The digest that points here is `CLAUDE.md` §Clouds.*

M19's premise applied to the sky. `engine-core/src/cloud.rs` grows one mesh — a golden-angle spiral
of icosphere lobes over the footprint, each growing `children` smaller lobes biased upward by `rise`,
buried 45% of their own radius so the surfaces interpenetrate (M19's join, for M19's reason),
radially displaced by `wobble`, and folded onto a base plane by `flatten`. The entity carries **no
`Mesh` and no `Material`** (`cloud_with_mesh`) and is sized by `Transform.scale`; non-uniform scale
is the normal case, which is what oblates the lobes. Determinism, the exact `vertex_count` +
`cloud_too_complex` budget (100k), and `Arc`-identity caching are M19's — except the cache key covers
the **eleven geometry fields only**, since colour and density are uniforms that cannot move a vertex.
**Cloud baselines are per build profile as well as per adapter**; bless from the debug binary.

Rendering is `shaders/clouds.wgsl`, a new pipeline (not a `Material` branch) duplicating
`FrameUniform` and the fog term rather than touching `mesh.wgsl`, with `sky_common.wgsl` prepended so
a cloud's underside is lit by the sky drawn behind it. Clouds join the existing back-to-front
`Blended` list beside water and transparent meshes, depth-tested but **not** depth-writing, so
overlapping lobes accumulate alpha as a stand-in for optical depth; culling is **off** for this
pipeline alone, because a cloud has no inside and would vanish the moment a camera entered one.
`drift` (m/s) is applied in the **vertex stage** from `ScenePass.time` — not folded into the model
matrix — which keeps `Scene::cloud_items` a pure function of the file and the grown mesh's `Arc`
stable across frames; the shape never evolves with time. No shadows cast, no point lights, no
volumetrics.

**Four things the renders changed, all easy to reintroduce by "simplifying"**: vertex normals are
bent **55% from each lobe's centre toward the cloud's** (`BODY_NORMAL`), without which every lobe
draws its own terminator and the cluster reads as a bag of marbles; the height profile's rise is
capped at 0.8 lobe *diameters* (`DOME_STACK`), without which the middle lobe floats clear of the ring
around it — the consequence being that how far a cloud fills a tall box is set by `lobe_size`, not by
stretching a fixed lobe count; alpha is `density · (1 - (1 - facing)^feather)` and **not**
`facing^feather`, since the proportional form turns a cloud seen from below translucent all over
(this inverted `feather`'s sense: higher is now *crisper*); and the sun reaches the shadowed side at
a `THROUGH_SCATTER` fraction (0.3) with the diffuse curve left **linear**, because applying it in
full saturates a white cloud everywhere and sharpening the curve instead turns a storm cloud into
grey rock.
