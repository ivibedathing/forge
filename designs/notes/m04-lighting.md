# Lighting (M4)

*Relocated verbatim from `CLAUDE.md` when it was compacted to an index. The digest that points here is `CLAUDE.md` §Lighting.*

`DirectionalLight` + `AmbientLight` components (at most one each per scene, validated),
`Material.emissive`, and a GGX Cook-Torrance shader in `engine-render/src/shaders/mesh.wgsl`. Lights
aim down their entity's local **−Z** like the camera; a scene with *zero* light components gets the
documented fallback rig (`LightRig::resolved`), while any light component means "absent is off".
Render targets are **sRGB** (`Rgba8UnormSrgb`): scene colors are linear reflectance, the hardware
encodes on write, and pixel tests compute expectations via the `srgb_encode` helper in
`engine-render/tests/lighting.rs` — never eyeball byte values. Line numbers on semantic errors come
from `engine-core/src/lineindex.rs` (serde_json discards spans).
