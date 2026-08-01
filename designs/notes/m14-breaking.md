# Breaking (M14)

*Relocated verbatim from `CLAUDE.md` when it was compacted to an index. The digest that points here is `CLAUDE.md` §Breaking.*

`Breakable` lists **pre-authored fragments** (mesh ref + local placement + cuboid `half_extents` +
`density` — no runtime fracture, the settled decision) and breaks three ways: collision
(`impulse_threshold` in kg·m/s — rapier contact *force* × dt at the event boundary, **peak** per step
not sum, and force events are enabled only on breakable colliders so no-Breakable scenes are
byte-identical to pre-M14), `world.break_entity(name)` (validated at call time, queued on the
ScriptHost, drained by the sim loop), and `world.explode(x,y,z,radius,impulse)` (radial impulse,
linear falloff, applied inside `step()` before integration).

Breaks apply after physics in entity-name order (`engine-physics/src/breaking.rs`): despawn parent,
spawn `Parent.fragN` (suffix-deduped) as dynamic bodies inheriting v + ω×r, then
`Scene::refresh_names` + `ScriptHost::sync_names` — fragments are ordinary entities everywhere
downstream. Trace rows **re-enumerate dynamic bodies every step** (sorted, so unchanged scenes trace
identically) plus `{"step", "broke", "fragments"}` lines; bake extends change-based to structure via
`formatter::apply_remove_entity` + `apply_add_entity` with `ComponentData::collect_from` — a baked
post-break scene revalidates and re-renders **bit-exactly**. Fragment `mesh` refs resolve like
`Mesh.asset` in both passes; `impulse_threshold` without a `Collider` is `breakable_without_collider`.
A threshold-less `Breakable` is script/explosion-only by design.
