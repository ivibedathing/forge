# One unit is one metre (M34)

*Relocated verbatim from `CLAUDE.md` when it was compacted to an index. The digest that points here is `CLAUDE.md` §One unit is one metre.*

**The convention was already true everywhere except one primitive, and that one exception had
quietly produced wrong content in six committed scenes.** Free fall measures `-9.810 m/s` after a
second and `-19.620` after two, a body at 10 m/s covers 10.000 m in one, every length-bearing field
in all 26 component schemas is documented in metres, a `Terrain` at `scale: 100` spans exactly ±50,
and glTF loads 1:1 (the rigged walker measures 1.58 m to the head). None of that changed. What
changed is `builtin:sphere`.

- **`builtin:sphere` was radius 1 — two metres across — while the cube, cylinder and plane were all
  one.** The cylinder's own doc comment stated the rule it broke: *"like the cube it fits the unit
  extent and `Transform.scale` means the same thing on both."* It now has radius 0.5, and
  `every_modelling_primitive_fits_the_unit_extent` pins the rule for all four by measuring the
  generated vertices. `builtin:triangle` stays 1.6 × 1.4 and is exempt — it is the M0 stack proof,
  not a modelling shape.
- **The damage was in the collider pairing, and it is why this was worth a milestone rather than a
  doc fix.** `Collider` dimensions are in the entity's own units and `Transform.scale` multiplies
  them too, so a collider authored to match the drawn ball had to be written at *twice* its visible
  radius — and nothing said so. Five of the six sphere-plus-collider pairs in the repo were wrong:
  `m8_drop`'s ball rested with its bottom 0.45 m under the floor, `m14_break`'s and `m23_road`'s the
  same, `m22_terrain`'s `Dropped` had a radius written as a world measurement that scale multiplied
  a second time (0.7 authored, 0.49 simulated, 0.7 drawn), and **the starter scene `engine init`
  scaffolds shipped the defect to every new project**. Only the tour's `Boulder` was right, and only
  because its author happened to write `radius: 1.0`.
- **Three of the four fixtures needed no edit at all, and that is the tell.** Where the author had
  treated the collider as the source of truth, halving the mesh made the two agree on its own —
  which is why **both golden traces are byte-identical** and `m23_road`'s "the ball must stay where
  it lands" test still passes untouched. Only the render moved. The other spheres had their `scale`
  **doubled**, which is bit-exact rather than merely close: doubling a decimal literal and halving a
  mesh are both exact in binary floating point, and the products round identically, so 34 of 39
  artifacts came back at zero pixels against baselines rendered by an older binary — the A/B claim,
  for free, out of the ordinary sweep.
- **The one thing that broke that reasoning was a script.** `m17_fire.rhai` and `tour_effects.rhai`
  both call `world.set_scale` on their emissive coal bed every step from a hard-coded constant, so
  doubling the *authored* scale achieved nothing and the coals rendered at half size. A blanket edit
  over scene files cannot see this; the constants in both scripts were doubled too. **Grep the
  `.rhai` files for `set_scale` before believing any scale-space change is complete.**
- **`collider_mesh_size_mismatch` is the check that would have caught all six**, and it is a
  warning rather than an error because a proxy collider is ordinary authoring. It compares a
  `sphere` or `cuboid` collider against the builtin mesh on the same entity, both in world units,
  and fires past a deliberately loose `COLLIDER_SIZE_TOLERANCE` of 1.25 — sized to catch the factor
  of two and the scale-applied-twice case while staying silent on an inset hull, a thick collider
  under a flat `builtin:plane` (axes the mesh is flat in are skipped), and a cuboid standing in for
  a sphere, which the tour's critters author and whose bounding box is exactly right.
  `no_committed_scene_disagrees_with_itself_about_how_big_something_is` sweeps every committed
  scene, the scaffolded starter included, so the six cannot come back.
- **It immediately found a seventh, in code merged the same day.** M33's `m33_proxies.json` draws
  both crates as `builtin:cube` at scale 1 — one metre — over a collider half a metre across, so
  its own baseline shows a walker standing *inside* a crate. The fix keeps M33's physics exactly
  (`scale` 0.5 with `half_extents` 0.5 leaves the world half-extent at 0.25, and mass comes from
  the collider, so `simulate` is byte-identical); only the drawn size moved, and the crate's
  authored `y: 0.25` confirms half a metre was always the intent. **That is the argument for the
  warning in one example**: this class is invisible in review, survives a fixture with a hard
  bit-exact pin, and reads as a renderer bug when anyone finally looks.
- **`BuiltinMesh::half_extents()` is what validation compares against, and it is written by hand**,
  so `declared_half_extents_match_the_geometry` measures it against the generated vertices. It is
  the reach from the origin rather than half the span, because the M0 triangle is not centred.
- Documented where it was silent: `Transform.position` and `Transform.scale` had **no doc comment at
  all** — `rotation` spent five lines on its degrees convention while the field that defines the
  length unit said nothing. Both now carry it, which reaches `engine list-components` and
  `docs/component-reference.md` for free. `AGENTS.md` and `docs/scene-format.md` gained the
  builtin-extent rule and the collider-units corollary. Also written down: **`Collider.density` is
  kg/m³ and its `1.0` default is not a plausible material** — a default 1 m³ cube masses 1 kg, which
  is invisible under gravity (mass-independent) and three orders of magnitude off for anything
  force-driven; the demo car carries `350` to reach 1.5 t.

Not here: changing `builtin:triangle`, and fixing `builtin:plane`/`builtin:cube`'s UV layout, which
M26 already deferred as its own change with its own A/B.

**Numbering**: two parallel sessions were holding `m33-gi` and `m33-skinned-colliders` when this
started. Skinned colliders landed as M33 while this was building, so this took M34 on the way in —
the repo's rule that the later session renumbers, applied at merge rather than at branch. A
global-illumination branch is still out there and is the next number.
