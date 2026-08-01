# Editor (M7, `crates/engine-editor`)

*Relocated verbatim from `CLAUDE.md` when it was compacted to an index. The digest that points here is `CLAUDE.md` §Editor.*

egui is **git-pinned to a master commit** (released egui pairs with wgpu 29; swap to the 0.36
release when it ships). The scene file stays the single source of truth: the editor polls the file
(250 ms) and reloads on any external change; every editor action commits through
`engine-core/src/formatter.rs` — a *splice*, not a serialize, so a one-field edit is one hunk on one
line and untouched content is byte-identical by construction (`cargo test -p engine-core formatter`
pins this). Commits rebase onto a fresh read by entity `name` + component `type` (never index); a
vanished target drops the edit with a status-bar notice.

Inspector widgets are generated from the component schema (a new component is editable the day it
exists); only arrays-of-numbers route to the vec3 widget. The validation panel shows the same
`EngineError`s the CLI emits, click-to-select. Viewport = `SceneRenderer` into an offscreen texture
(same pipeline as `engine screenshot`), orbit camera (right-drag; shift/middle = pan, scroll =
zoom), CPU ray picking, hand-rolled transform gizmos — `W`/`R`/`S` switch translate/rotate/scale,
world axes map straight to `Transform` field components, preview in memory, one write on release.
The viewport shows scenes **at rest** (no particles until the fixed clock advances) and passes
`hud: None` — its orbit camera is not the game frame.

Structure edits: drag-and-drop import (`import.rs`) references a dropped `.glb`/`.gltf` in place or
copies it to `meshes/`; a `.blend` is converted to `.glb` by running Blender headlessly (`$BLENDER`
→ `PATH` → macOS app bundle; absent = `blender_not_found`) on a worker thread, then one entity is
spliced in via `formatter::apply_add_entity` (the Blender-gated test skips cleanly when Blender is
missing). A "+ add" menu splices `builtin:` primitives the same way, its entries generated from
`BuiltinMesh::ASSETS`. The inspector adds and removes components via `apply_add_component` /
`apply_remove_component` — absent fields *are* the documented defaults. `[0, 1]` RGB triples get a
linear-RGB color-picker swatch committing one write per picker session. Hidden flag
`--self-screenshot <png> [--self-screenshot-after-ms N]` renders the editor and exits — the agent's
way to *look at* the editor. `RenderItem` carries an `entity: String` for picking/selection.
