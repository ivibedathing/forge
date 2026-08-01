# Environment: sky, fog, shadows, MSAA, transparency (M16)

*Relocated verbatim from `CLAUDE.md` when it was compacted to an index. The digest that points here is `CLAUDE.md` §Environment: sky, fog, shadows, MSAA, transparency.*

Five renderer features reached through **one scene-level `environment` block**
(`EnvironmentSettings`, hand-validated like `physics` by `check_environment_block`, code
`invalid_environment_value`) plus two new `Material` fields. **Every one of them defaults to off,
and that is the design**: eleven baselines were blessed before any of this existed and not one had
to be re-blessed. Fields: `sky` + `sky_zenith`/`sky_horizon`/`sky_ground`, `fog_density`, `shadows` +
`shadow_distance`, `samples` (1 or 4; anything else is a validation error rather than a silent
round). `sky_horizon` **is** the fog color — one field, so it cannot be set inconsistently with the
sky it fades into.

- **Shadows** are a single directional map (2048², `shadow.wgsl`, depth-only, no fragment stage,
  reusing the mesh pass's object+frame uniforms). The ortho box is fitted along the camera's view
  direction, and its center is **snapped to whole texels** — without that, moving the camera slides
  the sampling grid across the world and every shadow edge crawls, which reads as a bug rather than
  as low resolution. Casters are drawn **front-face-culled** so the map records each caster's far
  side, a better peeling margin than any constant bias. 3×3 PCF over a `LessEqual` comparison sampler
  with linear filtering, slope-scaled bias, and a fade to lit at the box edge. Transparent geometry
  does not cast. One cascade only.
- **Sky** is a fullscreen triangle drawn first with `depth_compare: Always` and depth writes off,
  evaluated per pixel from an unprojected view ray (per-vertex would visibly bend the horizon). The
  gradient lives in `shaders/sky_common.wgsl` and is **concatenated onto both `sky.wgsl` and
  `mesh.wgsl`** at pipeline build (`with_sky_common`) — WGSL has no `#include`, and the mesh pass
  reflects this exact sky off metal and water, so a second copy of the curve would drift.
- **Reflected sky and hemispheric ambient**, both gated on `sky`. Ambient is modulated by a
  ground↔zenith lerp normalized **per channel** against the two bands' mean, so `AmbientLight` keeps
  meaning what it says and only the color *balance* tracks the normal; normalizing against mean
  *luminance* instead is the obvious alternative and is wrong (a saturated sky then triples the blue
  channel and every up-facing surface goes blue-grey). The specular environment term uses
  **roughness-capped Schlick** (`max(1 - roughness, f0)`, not 1) — uncapped, grazing Fresnel turns
  matte ground into a sheet of sky.
- **MSAA** is `samples` on the scene pipelines plus a resolve; the HUD pass stays single-sampled on
  the resolved target, so glyphs are still pixel-exact. `SceneRenderer::with_samples` bakes the count
  into the pipelines, so it belongs to the renderer, not the frame.
- **Transparency** is `Material.alpha` (flat, view-independent — the "ghost this" knob) and
  `Material.transmission` (view-dependent, keeps the specular lobe, scales diffuse by
  `1 - transmission`). `Material::is_transparent` routes those into a second blended pass, sorted
  back-to-front with an entity-name tiebreak, depth-tested but not depth-writing, and the shader
  emits **premultiplied** color for them so a clear surface keeps its highlight and its sky
  reflection. No refraction and no scene-color sampling.

**The bit-exactness of the default path is load-bearing and fragile.** The four lines computing
`direct`/`ambient`/`base_color` in `mesh.wgsl` are the M4 originals, computed from immutable bindings
ahead of every M16 branch, and every new feature hangs off one combined `if`. That is stricter than
"an equivalent expression" on purpose: whether the compiler may contract `a*b + c` into an FMA
depends on the code around it, and an FMA carries more intermediate precision than the pair it
replaces. Restructuring those lines into arithmetic that is *equal on paper* moved `m12_hud.png` by
one ULP in one pixel. Leave them alone. Verified by `engine-render/tests/environment.rs` and fixture
`verify/m16_environment.json`.

**The check that settles a bit-exactness question is an A/B between binaries**, not a diff against a
baseline: build the CLI at `main` and in the worktree, render the same scenes with both, `cmp` the
PNGs.
