# Animation System — Design Document (M9)

Companion to `agent-native-engine-design.md`. That document is the source of truth for the
engine; this one covers only the M9 animation system. Where the two conflict, the engine doc
wins. Builds on the M3 glTF asset pipeline; interacts with M8 physics and M10 scripting only
at the seams noted in §9.

## 1. Vision

Animation an agent can author as text and *verify without a window*. The existing feedback
loop — edit → validate → build → screenshot → look — is single-frame; animation adds a time
axis, and every design choice below exists to keep that axis inside the same loop rather than
forcing a human to watch a window and say "yes, it moves."

Two problems make animation hostile to agents in conventional engines, and this design is
shaped around both:

1. **Motion is invisible to a screenshot.** Solved by making time an explicit render
   parameter (`--time`) and by contact-sheet rendering (`engine filmstrip`) so one PNG shows
   eight moments of a clip — an image the agent can already view.
2. **Skeletal data lives inside binary glTF.** The no-binary-formats invariant covers scene
   truth, not mesh assets — but a clip the agent cannot enumerate might as well not exist.
   Solved by `engine list-animations`, which dumps every clip in an asset as JSON.

Success criterion: told "make the cube spin," the agent writes a small JSON clip file,
attaches an `AnimationPlayer`, runs `engine validate`, renders a filmstrip, and confirms the
rotation visually — ordinary bash and file edits, no window, no human.

## 2. Core Principles

1. **Pose is a pure function of (files, time).** Sampling never depends on wall clock,
   playback history, or any state not on disk. `pose(scene, t)` is deterministic, so
   `engine screenshot --time 1.5` is reproducible and `engine diff-render` can do visual
   regression on animated scenes.
2. **Sampled state is never written back.** The scene file holds the *rest* values; the pose
   at time `t` is derived, ephemeral, and re-derivable. Writing poses into the scene would
   turn a render into an edit.
3. **Property clips are JSON, authored like scenes.** Any numeric field the component schema
   exposes is animatable by entity name + field path. The clip format gets the same
   treatment as scenes: schema'd, validated all-errors-at-once, `did_you_mean` on names.
4. **Deterministic failure over nondeterministic success.** Two active clips animating the
   same property of the same entity is a validation error, not a silent last-writer-wins —
   the same rationale as the single-`active`-camera rule.
5. **Binary assets must be introspectable.** Anything the engine reads out of a glTF —
   clip names, durations, track targets — is dumpable as JSON from the CLI.

## 3. Two Kinds of Animation

### Property clips (agent-authored)

Standalone JSON files, referenced by relative path (invariant 3), animating schema'd
component fields on entities addressed by name (invariant 4):

```json
{
  "name": "spin",
  "tracks": [
    {
      "entity": "Cube1",
      "property": "Transform.rotation",
      "interpolation": "linear",
      "keys": [
        { "time": 0.0, "value": [0.0, 0.0, 0.0] },
        { "time": 2.0, "value": [0.0, 360.0, 0.0] }
      ]
    }
  ]
}
```

- Suggested location `examples/.../animations/*.anim.json`; the extension is convention,
  not enforcement.
- `property` is `Component.field` — resolved against the same schema
  `engine list-components` publishes, so a new component's fields are animatable the day
  the component exists, and a typo'd path gets a `did_you_mean` from real field names.
- `interpolation` is per-track: `"step"`, `"linear"`, or `"cubic"` (Catmull-Rom through the
  key values; no hand-authored tangents in v1 — tangent arrays are hostile to text editing).
- Key `time`s must be strictly increasing — a violation is a validation error naming the
  offending key index.
- Clip duration is the last key time; there is no separate `duration` field to drift out of
  sync with the keys.

**Rotation interpolates component-wise on Euler degrees**, matching the `Transform.rotation`
file format. This is deliberate and load-bearing: the clip above actually spins a full turn,
whereas quaternion slerp would treat 0°→360° as the identity and do nothing — the classic
silent failure this engine exists to avoid. The agent wrote degrees; interpolation happens
in degrees. Gimbal-sensitive orientation work is what skeletal clips (below) are for.

### Skeletal clips (asset-authored)

Skins, joints, and animation clips loaded from glTF via the M3 pipeline. Authored in DCC
tools, stored binary, addressed by fragment: `meshes/robot.glb#Walk`. Sampling follows the
glTF spec (quaternion slerp for joint rotations); skinning is a joint-palette vertex shader
in `engine-render`. The engine never invents its own skeletal format — glTF *is* the
interchange format, and `engine list-animations` (§5) is what keeps it legible.

## 4. Scene Integration

### The `AnimationPlayer` component

Plain data, like every component:

```json
{ "type": "AnimationPlayer",
  "clip": "animations/spin.anim.json",
  "speed": 1.0,
  "looping": true,
  "start_offset": 0.0 }
```

- `clip` — relative path to a property clip, or `path#ClipName` into a glTF. One field,
  both kinds.
- A skeletal player must live on the entity whose `Mesh` owns the skin. A property-clip
  player may live anywhere — its targets are named inside the clip — but convention is the
  entity it primarily animates.
- Defaults: `speed` 1.0, `looping` true, `start_offset` 0.0, so the minimal player is just
  `{ "type": "AnimationPlayer", "clip": "…" }`.

### Time model

One scene clock `t`, starting at 0. A player's local time is
`local = t * speed + start_offset`, wrapped by clip duration when `looping`, clamped to the
final pose when not. `engine run-scene` advances `t` in real time; `engine screenshot` and
`engine filmstrip` set it explicitly. There is no play/pause/stop runtime state in v1 — a
player in the file is playing, because runtime triggering without hidden state needs the
M10 scripting layer (§9).

### System ordering

Hand-ordered per the no-scheduler hecs decision: **sample animations → (M8 physics, when it
exists) → render**. Sampling writes component values in the ECS world only — never to disk
(principle 2).

### Validation

`engine validate scene.json` follows `AnimationPlayer` references and reports, all at once,
with file/line via the existing `lineindex` path:

- `asset_not_found` — clip file or glTF missing; `unknown_clip` — bad `#ClipName` fragment,
  `did_you_mean` from the asset's actual clip names.
- `unknown_entity` — a track targets a name not in the scene; `did_you_mean` from scene
  entity names. `unknown_property` — bad field path; suggestions from the component schema.
- `type_mismatch` — key value shape doesn't match the field (e.g. scalar keys on a `[f32;3]`
  field). `unsorted_keys` — non-increasing times.
- `conflicting_tracks` — two active clips animate the same `entity.property` (principle 4),
  naming both players.

`engine validate foo.anim.json` directly checks everything structural (shape, sorted keys,
property paths against the schema); entity-name resolution needs a scene, so standalone
validation reports what it can and says so.

## 5. CLI Surface

| Command | Purpose |
|---|---|
| `engine screenshot <scene> --time 1.5 …` | Existing command, new flag: render the pose at `t` = 1.5s. Default 0.0 — today's behavior, unchanged. |
| `engine filmstrip <scene> --out strip.png [--start 0] [--end 2] [--frames 8] [--columns 4]` | Contact sheet: N frames sampled evenly over [start, end], tiled into one PNG. The agent's primary way to *see motion in a single image view*. Default end = longest clip duration in the scene. |
| `engine list-animations <scene-or-asset>` | Every clip reachable from a scene (or inside one .glb/.anim.json): name, duration, track targets — as JSON. The introspection window into binary glTF (principle 5). |
| `engine diff-render <scene> <baseline> --time T …` | Existing command, new flag: visual regression at a fixed time. |

All of it stays headless, structured-error'd, and deterministic. `filmstrip` reuses the
`screenshot` readback path; per-frame cost is one offscreen render, so an 8-frame strip
should cost roughly 8 screenshots, not a video-encoder dependency.

## 6. Workspace Impact

No new crate. Clip parsing, schemas, sampling, and validation land in `engine-core` (pure,
unit-testable, no GPU); glTF clip/skin extraction in the M3 asset crate; skinning in
`engine-render`; flags and `filmstrip`/`list-animations` in `engine-cli`. The
`AnimationPlayer` component is one new line in the `components!` macro, and
`schemas/component-schema.json` is regenerated as usual — plus a new generated
`schemas/animation-schema.json` for the clip file format, enforced by the same
`repo_contracts.rs` mechanism.

## 7. Milestones

1. **A0 — Clip format + sampling, no rendering.** Clip schema, property-track sampling as a
   pure function in `engine-core`, full validation story (§4). Verified entirely by unit
   tests — interpolation, looping, the 0°→360° spin, conflict detection. Nothing visual yet.
2. **A1 — Time reaches the CLI.** `--time` on `screenshot`/`diff-render`,
   `engine filmstrip`, real-time playback in `run-scene`. The agent loop closes here:
   headless tests assert that a spinning cube renders *differently* at t=0 and t=0.25, and
   identically at t=0 and t=2.0 (loop period) — the pixel-level proof of determinism.
3. **A2 — Skeletal.** glTF skin/joint/clip loading, `engine list-animations`,
   joint-palette GPU skinning. Headless pixel tests on a small rigged asset checked into
   `examples/`.
4. **A3 — Hardening.** Cubic interpolation, multi-clip scenes with disjoint targets,
   filmstrip ergonomics, validation polish from real agent transcripts.

A0 before any rendering mirrors the M5 lesson: the validation and determinism story is
load-bearing, not polish, so it comes first.

## 8. Non-Goals (v1)

- No blend trees, state machines, or crossfades — blending two clips reintroduces exactly
  the ordering nondeterminism principle 4 forbids, and needs design of its own.
- No IK, no root motion, no retargeting, no animation compression.
- No morph targets (revisit with a concrete need; glTF supports them).
- No animation *editing* GUI — the M7 editor's schema-generated inspector will edit an
  `AnimationPlayer` like any component for free, and clip files are for `$EDITOR` and agents.
- No event tracks (footstep sounds, hit frames) — that's scripting's problem (M10).

## 9. Open Questions

- **Runtime triggering.** Playing a clip in response to a game event requires either M10
  scripting or a data-driven trigger format; either way, "what is playing" must stay
  reconstructable from files + inputs, not accumulate as hidden state. Blocked on the §9
  scripting decision in the engine doc — do not design around it prematurely.
- **Animation vs. physics ownership (M8).** An animated transform on a rigid body is a
  contradiction — who wins? Likely answer: it's a validation error unless the body is
  explicitly kinematic. Settle when M8's component design exists, and note M8 and M9 land
  independently — neither should wait on the other.
- **Hot reload of clip files.** The engine-doc hot-reload question applies with extra force
  here: re-running `filmstrip` per edit may be fast enough that live reload adds little for
  the *agent*, even though it's clearly nice for a human in `run-scene`. Measure first.
- **Cubic tangents.** If Catmull-Rom proves too limiting, decide between glTF-style
  in/out tangents (expressive, verbose JSON) or easing keywords (`"ease-in"`, …) — the
  keyword route is friendlier to text authorship and probably wins here.
