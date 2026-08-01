# Animation (M9)

*Relocated verbatim from `CLAUDE.md` when it was compacted to an index. The digest that points here is `CLAUDE.md` §Animation.*

Property clips; skeletal glTF landed in M30 and shares this clock. Clips are JSON (`*.anim.json`, schema in
`schemas/animation-schema.json`, regenerated via `engine list-animations --schema`), animating
`Component.field` on entities by name. Pose is a pure function of (files, time) — `--time` on
screenshot/diff-render is reproducible down to `cmp`-identical PNGs, and t=loop-period equals t=0
byte-for-byte. **Rotation interpolates component-wise on Euler degrees** so a 0→360 clip actually
spins (quaternion slerp would no-op it) — load-bearing, don't "fix" it. Sampling lives in
`engine-core/src/animation.rs` (step/linear/cubic Catmull-Rom); `set_field` must cover every numeric
schema field — a drift test walks the schema and calls it. System order: animations → physics →
render. The M8×M9 ownership rule is settled: a clip animating the Transform of a **dynamic** body is
`animation_on_dynamic_body` (kinematic is the supported "animation drives, physics follows" case).
Clip-content errors carry the clip file's own file/line; `engine validate` accepts clip files
directly (structural checks only).
