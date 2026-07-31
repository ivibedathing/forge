# Input — Design Document (M11)

Companion to `agent-native-engine-design.md`. That document is the source of truth for the
engine; this one covers only the M11 input system — keyboard input for interactive games,
designed so that *interactive* never means *unverifiable*. Written 2026-07-27, after M0–M10.

## 1. The shape of the problem

A game needs a player; the agent building the game has no hands. Every prior milestone kept the
loop closed — edit a text file, render, *look* — and input is where that loop traditionally
breaks: a behavior that only exists while someone holds an arrow key cannot be validated,
diff-rendered, or traced. The design answer is the same one the engine always gives: **make it
a text file.** Live keys exist only in the windowed viewer; everywhere else, input is a
committed timeline that replays deterministically.

## 2. Core principles

1. **Input is sampled per fixed step, never per event.** Scripts see "is this key held during
   step N" — the same integer clock as physics and scripting. No event queue, no key-repeat
   semantics, no wall-clock edges.
2. **Headless input is a text artifact.** `--input <file.input.jsonl>` feeds the same held-set
   the viewer's keyboard would; `simulate --steps N --input f` twice is byte-identical,
   and a screenshot after a recorded drive is a pinnable baseline.
3. **No input means no keys held.** Every existing command, trace, and baseline is unchanged
   by this milestone; the golden M8 trace stays golden.
4. **Scripts are the only consumer.** Input reaches gameplay through the curated `world` API
   (`world.key("ArrowUp")`), never through a component — key state is transient by nature and
   has no business in a scene file (invariant 2 cuts both ways).
5. **Unknown keys fail structurally, with `did_you_mean`.** A typo'd key name in a timeline is
   a validation-class error; in a script it is a `script_runtime_error` naming the close match
   — deterministic failure over a silently-never-pressed key.

## 3. Key names

The canonical names are the W3C `KeyboardEvent.code` values (which are also winit's `KeyCode`
names): `ArrowUp` `ArrowDown` `ArrowLeft` `ArrowRight`, `KeyA`–`KeyZ`, `Digit0`–`Digit9`,
`Space`, `Enter`, `ShiftLeft` `ShiftRight`, `ControlLeft` `ControlRight`. The set is a curated
allowlist (`engine_core::input::KNOWN_KEYS`); keys outside it don't exist, in files or in the
viewer. Layout-independent physical codes, so WASD is WASD on AZERTY hardware too.

## 4. The input timeline file

JSONL, conventionally `*.input.jsonl` — the same medium as traces. Each line is a keyframe of
the *complete* held set, taking effect at `step` (0-based, the script/physics step index) and
holding until the next line; before the first line nothing is held:

```jsonl
{"step": 0, "held": ["ArrowUp"]}
{"step": 120, "held": ["ArrowUp", "ArrowLeft"]}
{"step": 300, "held": []}
```

Complete sets rather than press/release deltas: any prefix of the file is valid, any line is
legible alone, and an agent authoring "drive forward then turn" writes exactly two lines.
Steps must be strictly increasing (`unsorted_input_steps`); malformed lines are
`input_parse_error`; an unknown key name is `unknown_key` with `did_you_mean`. All errors
carry the timeline file and line number, every error at once, per the M5 contract.

## 5. Command surface

- `engine simulate|screenshot|diff-render|raycast … --input <f>` — replay the timeline while
  stepping. Combined with `--steps`, this is the whole verification story: trace a drive,
  screenshot its end state, pin it with diff-render.
- `engine run-scene <scene> [--record-input <f>]` — the play mode. The keyboard drives the
  held set; `--record-input` writes a timeline line whenever the held set changes, so a human
  play session becomes a committable, replayable artifact. Replay it headlessly and the same
  scripts do the same thing: record once, regression-test forever.
- The viewer re-resolves the camera transform from the live scene every frame, so a script may
  drive a chase camera; the headless commands already resolve the camera after stepping.

## 6. The `world` API addition

| Call | Meaning |
|---|---|
| `world.key(name)` | `true` if `name` is held during the current step |

One predicate, not an axis/action abstraction — bindings, dead zones, and input mapping are
game logic, and game logic lives in scripts.

## 7. Non-goals (v1)

- **The mouse landed in M28** — see `designs/mouse-input-design.md`, which reverses the first
  item below and nothing else: buttons ride the same `held` set, the cursor is a fraction of
  the frame carried in the same timeline, and every principle in §2 above is unchanged.
- No mouse, gamepad, or text input; no key-repeat or pressed/released edge queries (a script
  that needs edges compares against what it did last step via world state).
- No input in `filmstrip` (it samples animation time, not steps) or the editor (the editor
  edits; the viewer plays).
- No binding/action-map layer — `world.key` is the primitive; maps are scripts' business.
