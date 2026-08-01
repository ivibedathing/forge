# Point lights (M17)

*Relocated verbatim from `CLAUDE.md` when it was compacted to an index. The digest that points here is `CLAUDE.md` §Point lights.*

`PointLight` is a local light — position only, no orientation, many per scene up to
`MAX_POINT_LIGHTS` (8, beyond which `too_many_point_lights` rather than a light that silently never
shines). Inverse-square falloff windowed by `(1 − (d/r)⁴)²`: the window is what makes a light
*local*, and past `range` the contribution is byte-identical to no light at all — without a hard
horizon a lantern in one room lifts the black level of the next. `intensity` is brightness at one
unit of distance. No shadows (the engine has one shadow map and it belongs to the sun). Lights are
ordered by entity name, since the uniform array is fixed-size and an index must not depend on
archetype iteration; a `PointLight` counts as lighting the scene. Contributions are **added to the
finished color** on their own branch after every M16 feature — firelight is *extra* light, and
`a_point_light_is_extra_light_not_replacement_light` walks every pixel of a sunlit scene to prove
adding a lamp never darkens one. Scripts reach any light by name through `world.light_intensity` /
`set_light_intensity` / `light_color` / `set_light_color` (all three light components — the fields
mean the same thing on each); intensity errors on negative/NaN/overflow at the call, color *clamps*
to `[0, 1]`, and both bake change-based.

**Two places here are deliberately more repetitive than they look**: `evaluate_point_light` in
`mesh.wgsl` re-derives the GGX terms instead of sharing a function with the sun path, and
`particles.wgsl` writes the un-stretched quad expansion out twice rather than lerping. Both guard the
M16 ULP sensitivity — factoring them would rewrite the four untouchable lines.
