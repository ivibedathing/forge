# Scripting (M10, `crates/engine-script`)

*Relocated verbatim from `CLAUDE.md` when it was compacted to an index. The digest that points here is `CLAUDE.md` §Scripting.*

**Rhai pinned =1.25.1** — settled (Lua lost on the C dependency
and determinism friction, compiled-Rust-only lost on rebuild-per-iteration). Scripts define
`fn step(world, step)`; the curated `world` API is the entire universe — no time, no I/O, no
randomness, 1M-operation budget per call, so traces stay byte-identical with scripts running. Script
parse errors fail `engine validate` with the script's file/line; runtime errors are
`script_runtime_error`, exit 1, world intact. Bake is change-based: any `Transform`/`RigidBody` field
differing from the file's rest value is spliced — which is how script-driven kinematics land in baked
files. Kinematic-vs-fixed contact events are opted in via `ActiveCollisionTypes` (rapier skips them
by default). Bake next to the scene, not /tmp — relative paths.
