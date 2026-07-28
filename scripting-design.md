# Scripting — Design Document (M10)

Companion to `agent-native-engine-design.md`. That document is the source of truth for the
engine; this one covers only the M10 scripting system. Written 2026-07-27, after M0–M9.

## 1. The §9 decision — Rhai (settled 2026-07-27)

**Rhai 1.25, pinned exactly**, settled deliberately with the user (same process as hecs, JSON,
and rapier; don't relitigate without raising it). The alternatives, and why they lost:

- **Lua (mlua)** — the industry standard, and the language LLMs know best. Costs that decided
  it: a vendored C dependency (against the pure-Rust supply-chain posture every other bet here
  has kept), and determinism that requires actively fighting the language (unordered `pairs`,
  a GC with observable timing). LLM familiarity with Rhai's Rust/JS-like syntax is good enough
  that the gap doesn't pay for a C toolchain in the build.
- **Compiled-Rust systems only** — trivially deterministic, zero new surface, but per-scene
  behavior would require an engine rebuild per iteration and scenes could not carry logic as
  data. The agent's edit→see loop is the product; seconds of `cargo build` per script tweak
  loses to milliseconds of re-reading a text file.

What Rhai buys: pure Rust (no C, no new supply chain), an embeddable engine with a curated
API surface (nothing exists in a script's world unless we register it — sandboxing by
construction), operation limits (a runaway loop becomes a structured error, not a hang), and
scripts-as-data in the same medium as everything else.

## 2. Core Principles

1. **Scripts mutate the world, never invent state.** A script reads and writes component
   fields through the registered API; anything it computes beyond that dies at the end of the
   step. Baked output after a scripted run is an ordinary valid scene file — the M8 bake
   machinery doesn't know scripts exist.
2. **Deterministic stepping, shared clock.** Scripts run once per fixed step, driven by the
   same integer step count as M8 physics. No wall clock, no randomness, no I/O in the script
   API — `simulate --steps N` twice is byte-identical, scripts and all.
3. **System order is fixed:** sample animations → run scripts → step physics → render. Scripts
   see the animated pose and can drive kinematic bodies; physics resolves what they did.
4. **Same validation, same errors.** A script that doesn't parse fails `engine validate` with
   the script's own file/line. A runtime error is a structured `EngineError`, exit non-zero —
   never a panic, never a silent no-op.
5. **The API is the contract.** Everything a script can touch is registered explicitly and
   documented; the surface is small on purpose and grows deliberately.

## 3. The `Script` component

```json
{ "type": "Script", "source": "scripts/elevator.rhai" }
```

`source` is a relative path (invariant 3), `.rhai` extension enforced like mesh extensions.
One script per component; a scene may have many scripted entities. Scripts run in entity
declaration order (deterministic), each with its own compiled AST and its own scope — scripts
do not share state.

## 4. Execution model

Each script file must define:

```rhai
fn step(world, step) {
    // step: integer step index, 0-based; dt is world.dt()
    if step < 120 {
        let p = world.position("Elevator");
        world.set_position("Elevator", p[0], p[1] + 2.0 / 120.0, p[2]);
    }
}
```

- The AST compiles once per run (and at validate time); `step` is called once per fixed step.
- A missing `step` function is a validation error (`script_missing_step_fn`), not a silent
  no-op.
- Operation limit per call (1,000,000 ops) — exceeding it is `script_runtime_error`, the
  deterministic answer to an infinite loop.
- The Rhai time/eval/IO built-ins are not registered; the engine's curated `world` API is the
  entire universe.

### The `world` API (v1, deliberately small)

| Call | Meaning |
|---|---|
| `world.dt()` | fixed timestep seconds (`1.0 / timestep_hz`) |
| `world.position(name)` / `world.set_position(name, x, y, z)` | `Transform.position` |
| `world.rotation(name)` / `world.set_rotation(name, x, y, z)` | `Transform.rotation`, Euler degrees |
| `world.scale(name)` / `world.set_scale(name, x, y, z)` | `Transform.scale` |
| `world.key(name)` | (M11) `true` if the key is held during this step; unknown key names are runtime errors with a `did you mean` |
| `world.forward(name)` | (M11) the entity's world-space forward (local −Z) as `[x, y, z]` — use this, never yaw math on `rotation[1]`: XYZ Euler clamps the middle angle to ±90°, so physics-integrated yaws past that come back as the `(±180, θ, ±180)` twin |
| `world.linear_velocity(name)` / `world.set_linear_velocity(name, x, y, z)` | (M11) `RigidBody.linear_velocity`, m/s; a write to a dynamic body reaches the solver before the next step — the vehicle primitive |
| `world.angular_velocity(name)` / `world.set_angular_velocity(name, x, y, z)` | (M11) `RigidBody.angular_velocity`, **degrees/sec** (file convention) |
| `world.look_at(name, x, y, z)` | (M11) aim the entity's local −Z at a point with a level horizon — the chase-camera primitive (composing pitch and yaw through the XYZ Euler order rolls the horizon; this computes the decomposition correctly) |
| `world.state(key, default)` / `world.set_state(key, value)` | numeric per-run memory on the host (a lap timer's start step): replay-deterministic, reset by a fresh run, deliberately **not** captured by bake — the same disposability as physics solver caches |
| `world.hud(text)` | push one printable-ASCII overlay line for *this step only* (the list clears every step, so the HUD is a pure function of the step that drew it); composited onto screenshots, diff-renders, and the run-scene window, mirrored into simulate traces/reports. Caps: 16 lines × 96 chars, runtime error beyond |
| `world.hud_text(name)` / `world.set_hud_text(name, text)` | (M12) `HudText.text` — the component readout; unlike `world.hud` lines this is scene state, so it bakes under the change-based rule |
| `world.hud_rect_size(name)` (returns `[w, h]`) / `world.set_hud_rect_size(name, w, h)` | (M12) `HudRect.size` in pixels — the gauge-bar primitive; bakes like any component field |
| `world.touching(name)` | (M12) names of entities the entity's collider is in contact with, as an array of strings — the touching-state left by the **previous** physics step (system order is scripts → physics, so a contact at physics step N is script-visible at step N+1) |
| `world.contacts_started(name)` | (M12) the subset of `touching` that began on the previous physics step — the "on hit" edge trigger; empty again the step after |

Getters return `[x, y, z]` arrays. A name that resolves to no entity, or an entity without a
`Transform`, raises a script runtime error naming the entity — deterministic failure over
silent no-op. Material access and spawning are deferred (§7); velocity access arrived with
M11's car, contact queries with M12's collision work.
Input semantics (key names, the timeline file `--input` replays) live in `input-design.md`.

## 5. Workspace

`crates/engine-script` (rhai wrapper; `ScriptHost::build` + `ScriptHost::step`), component
data in `engine-core`, wiring in `engine-cli`'s simulate path — the same split as physics.
Nothing else depends on the script crate; deleting it leaves the engine whole.

## 6. Validation and errors

- `Script.source`: relative path, exists, `.rhai` extension — the mesh-asset checks, reused.
- Scene validation compiles every referenced script: `script_parse_error` carries the script
  file's path and rhai's line/column. A missing `step` fn is `script_missing_step_fn`.
- Runtime: `script_runtime_error` with script file, line where available, and the entity whose
  Script component ran.

## 7. Non-Goals (v1)

- No event callbacks (`on_contact`, `on_message`) — contact *observation* exists via traces;
  reacting needs an event-delivery design of its own.
- No inter-script communication, no spawning/despawning entities, no asset access.
- No hot reload (the engine-doc question stays open); `simulate`/`screenshot` re-read scripts
  every run, which is the agent loop.
- No `Script` fields beyond `source` (no inline code — files are the medium).
