# Assets

*Relocated verbatim from `CLAUDE.md` when it was compacted to an index. The digest that points here is `CLAUDE.md` §Assets.*

`Mesh.asset` is `builtin:cube` / `builtin:cylinder` / `builtin:plane` / `builtin:sphere` /
`builtin:triangle`, or a `.gltf`/`.glb` path relative to the scene file. **Every builtin but the
triangle is one metre across at scale 1** (M34), so `Transform.scale` reads as a size in metres and
a `Collider` matching one is always `half_extents: [0.5, 0.5, 0.5]` or `radius: 0.5`. Reference checks (existence,
extension, absolute-path rejection) live in `engine-core/src/mesh.rs` (`MeshAsset::resolve`); actual
file parsing lives in `engine-assets` — the only crate that opens asset files (glTF meshes plus
PNG→RGBA8 textures, the latter awaiting texture-mapped materials). `engine validate` runs both
passes, so a corrupt glTF fails validation, not just the screenshot. `Scene::render_items` takes a
`MeshSource`: `AssetServer::for_scene` in the CLI, `BuiltinAssets` in GPU-less tests.
`examples/meshes/pyramid.gltf` is generated text glTF (embedded base64 buffer), flat-shaded,
CCW-wound.
