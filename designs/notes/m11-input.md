# Input (M11)

*Relocated verbatim from `CLAUDE.md` when it was compacted to an index. The digest that points here is `CLAUDE.md` §Input.*

Keyboard input sampled per fixed step on the shared integer clock — scripts ask
`world.key("ArrowUp")` (unknown names are runtime errors with `did_you_mean`; key names are the
curated W3C-code allowlist in `engine_core::input::KNOWN_KEYS`). Live keys exist only in
`engine run-scene`; headlessly, input is an `*.input.jsonl` timeline (sparse keyframes of the
complete held set, in effect from their 0-based `step` until the next line; strictly increasing)
replayed via `--input` on simulate/screenshot/diff-render/raycast — same timeline, byte-identical
results, and no `--input` means no keys held, so all pre-M11 traces/baselines are untouched.
`run-scene --record-input` writes a timeline whenever the held set changes: record a play session
once, regression-test it forever. `world.look_at(name, x, y, z)` aims an entity's local −Z with a
level horizon (pitch+yaw through the XYZ Euler order would roll — that's why it exists); the viewer
re-resolves the camera transform every frame so scripts can drive a chase camera.
