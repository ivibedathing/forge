# Physics — Design Document (M8)

Companion to `agent-native-engine-design.md`. That document is the source of truth for the
engine; this one covers only the M8 physics system. Where the two conflict, the engine doc wins.

## 1. Vision

Rigid-body physics that an agent can operate entirely through text files and the CLI. The
difficulty is that physics is *temporal*: a scene file describes an instant, but simulation
produces motion, and the agent's medium — JSON in, PNG out — has no native way to see motion.
The design's job is therefore to make **time itself observable and reproducible**: simulation is
deterministic, advances only in fixed steps when explicitly asked, and every interesting moment
can be captured back into the agent's medium — as a baked scene file, a screenshot, or a
step-by-step trace.

Success criterion: the agent adds a `RigidBody` and `Collider` to a cube floating at y=5, runs
`engine simulate scene.json --steps 120 --bake settled.json`, screenshots `settled.json`, and
*sees* the cube resting on the ground plane. Running the same `simulate` twice produces
byte-identical output, so "the cube ends up at y=0.5" is a testable, git-diffable fact — not a
thing that happened once in a window.

## 2. Core Principles

1. **Determinism is the contract.** Same scene file + same step count = identical results, byte
   for byte, across runs and platforms. Everything else here — baking, traces, physics
   regression tests — is only trustworthy because of this. Anything that would trade determinism
   for speed (threading nondeterminism, wall-clock timesteps, platform-varying math) loses.
2. **Simulation state is derived, never authoritative.** The scene JSON is the initial
   conditions; solver internals (contact caches, islands, sleep state) are in-memory and
   disposable. This is how physics coexists with the no-hidden-state invariant (engine doc
   §2.2): any state worth keeping is baked back out as a *valid scene file*, and any baked file
   plus a step count reproduces everything else.
3. **Time advances only when asked.** Headless paths step a fixed timestep an integer number of
   times and never read a clock. The windowed viewer (`engine run-scene`) drives the same fixed
   step through an accumulator; frame pacing may vary there, but the headless path is canonical.
4. **Components are plain data; rapier is an implementation detail.** No rapier types, handles,
   or version-specific enums appear in scene JSON, component schemas, or `engine-core`. Swapping
   the backend must not touch a single scene file.
5. **Same validation, same errors.** Physics fields validate through the existing
   `EngineError` path — file/line/`did_you_mean` and all-errors-at-once — never a second
   reporting channel.
6. **Physics tests need no GPU.** Unlike the render tests, simulation tests run everywhere,
   including bare CI. There is no "skips cleanly when no GPU" caveat here, so the determinism
   and behavior suites are unconditional.

## 3. Backend — rapier3d (settled 2026-07-27)

**`rapier3d` 0.34**, settled deliberately with the user (same process as hecs and JSON; don't
relitigate without raising it). The alternative — `parry3d` for collision detection plus a
hand-written impulse solver — was rejected: stable stacking, friction, and joint constraints are
years of solver engineering, and a homemade solver would make principle #1 *harder*, not easier,
to keep.

What rapier buys: a production solver (islands, sleeping, warm-starting), CCD, sensors,
scene queries (raycasts, shape casts), joints for later, and — decisive for this project — the
**`enhanced-determinism`** feature flag, which makes results reproducible across platforms at
the cost of some SIMD parallelism. At this engine's scale that cost is irrelevant.

What it costs, eyes open:

- **Churn.** Dimforge ships breaking releases; this is the wgpu bet a third time. Mitigation is
  the same as CLAUDE.md's wgpu rule: pin the exact version, and when touching rapier, read the
  API in `~/.cargo/registry/src/*/rapier3d-0.34.0/src/` rather than writing from memory.
  Additionally, a rapier *upgrade may change trajectories* even without API breaks — treat any
  golden-trace diff on upgrade as a breaking change to review, not noise to regenerate blindly.
- **nalgebra.** rapier's math is nalgebra; the engine's is glam. The conversion layer lives at
  the `engine-physics` crate boundary and nowhere else — nalgebra must not leak into
  `engine-core` or scene-facing types.

## 4. Workspace Layout

```
crates/
  engine-physics/    # rapier wrapper: world build, stepping, sync, queries
```

Component *data* (`RigidBody`, `Collider`, the scene-level `physics` block) lives in
**`engine-core`** with the other components, so schema generation, `engine list-components`,
and validation stay in one place. All *logic* lives in `engine-physics` — the engine doc's
"components are plain data, logic lives in systems" split, applied at crate granularity.

`engine-physics` depends on `engine-core`; `engine-cli` depends on `engine-physics`;
`engine-render` is untouched. Nothing in core depends on physics: a scene with no physics
components never constructs a physics world, and deleting the crate leaves rendering whole.

## 5. Scene Format

### Scene-level settings

An optional top-level block; every field has a default, so scenes without physics don't change:

```json
{
  "name": "demo_scene",
  "physics": { "gravity": [0.0, -9.81, 0.0], "timestep_hz": 60 },
  "entities": [ ... ]
}
```

`timestep_hz` is an **integer** (default 60), not a float `dt` — `1/60` has no exact JSON
representation, and an integer keeps scene files free of float-precision noise. The engine
computes `dt = 1.0 / hz` once, identically everywhere.

### RigidBody

```json
{ "type": "RigidBody",
  "body": "dynamic",
  "linear_velocity":  [0.0, 0.0, 0.0],
  "angular_velocity": [0.0, 0.0, 0.0],
  "gravity_scale": 1.0,
  "linear_damping": 0.0,
  "angular_damping": 0.0,
  "ccd": false,
  "can_sleep": true }
```

- `body` is `"dynamic" | "kinematic" | "fixed"` (typos get `did_you_mean`). Only `body` is
  required; everything else defaults to the values shown, per the settled component-defaults
  policy.
- `angular_velocity` is in **degrees per second**, matching the settled Euler-degrees decision
  for `Transform.rotation`: the agent that writes `"rotation": [0, 45, 0]` writes
  `"angular_velocity": [0, 90, 0]` for a half-turn per second, in the same units and axis
  order. Conversion to rad/s happens at the rapier boundary, like everything else.

### Collider

```json
{ "type": "Collider",
  "shape": "cuboid", "half_extents": [0.5, 0.5, 0.5],
  "friction": 0.5,
  "restitution": 0.0,
  "density": 1.0,
  "sensor": false,
  "offset": [0.0, 0.0, 0.0] }
```

The shape is an internally-tagged enum on `"shape"`, flattened into the component so the file
stays one flat object — the shape `jq` and an LLM handle best (engine doc §5). Variants and
their fields:

| `shape` | fields |
|---|---|
| `cuboid` | `half_extents: [x, y, z]` |
| `sphere` | `radius` |
| `capsule` | `half_height`, `radius` (Y-axis) |

All plain struct variants, so serde's internally-tagged buffering caveat (CLAUDE.md) stays
satisfied. Mass comes from `density` × shape volume, rapier-style; there is no separate mass
field until something needs one. Mesh-derived colliders (`convex_hull`, `trimesh` from a glTF
asset) are deliberately **not** in M8 — they belong to a follow-up once M3's asset pipeline
settles, and primitives are enough to prove the loop.

**Scale rule:** `Transform.scale` is applied to the collider shape when the physics world is
built — a cube scaled 2× collides 2× big, which is what the agent looking at the screenshot
expects. Nonuniform scale on a `sphere` or `capsule` has no rapier representation and is a
validation error (`nonuniform_scale_on_round_collider`), not a silent approximation.

## 6. Stepping and the hecs ↔ rapier Sync

`engine-physics` exposes one type:

```rust
PhysicsWorld::build(&World, &SceneMeta) -> Result<PhysicsWorld, EngineError>
PhysicsWorld::step(&mut self, &mut World)   // one fixed step, writes back into hecs
```

The world is **built fresh from the scene for every simulate run** — no solver state ever
persists across CLI invocations, so determinism holds by construction rather than by careful
cache invalidation. Entity↔handle maps live inside `PhysicsWorld` and die with it.

Per step, hand-ordered (no scheduler, per the settled hecs decision):

1. Push kinematic bodies' current `Transform` into rapier as kinematic targets.
2. `rapier.step(dt)`.
3. Write back into hecs for dynamic bodies: position and rotation onto `Transform`
   (quaternion → Euler XYZ degrees via glam — lossy as a *representation* but deterministic,
   and normalized to a canonical range so baked files are stable), velocities onto `RigidBody`.

Fixed bodies are never written back. Entities with a `Collider` but no `RigidBody` are static
collision geometry — the common case for ground planes and walls.

## 7. CLI Surface

| Command | Purpose |
|---|---|
| `engine simulate <scene.json> --steps N [--bake out.json] [--trace trace.jsonl]` | Headless: build world, step N times |
| `engine screenshot <scene.json> --steps N ...` | Simulate N steps, then render — physics' edit→see loop |
| `engine raycast <scene.json> --from x,y,z --dir x,y,z [--steps N]` | Scene query, JSON result on stdout |
| `engine run-scene <scene.json>` | Steps physics automatically when physics components are present |

- **`--bake`** writes a *valid scene file*: same schema, `Transform` and `RigidBody` velocities
  updated, every other field byte-preserved with stable key order — the format-preserving rule
  the GUI editor doc's principle #5 states, for the same reason: diff noise corrupts the
  agent's medium. A baked file revalidates clean and can itself be simulated further; baking is
  how *any* moment of simulation becomes agent-visible text.
- **`--trace`** emits JSONL, one line per dynamic body per step —
  `{"step": 12, "entity": "Cube1", "position": [...], "rotation": [...], "linear_velocity": [...]}`
  — plus contact begin/end events (`{"step": 30, "contact": ["Cube1", "Ground"], "started": true}`).
  Traces are what agents grep and assert on ("did the cube ever touch the sensor?"), and
  committed traces are physics' golden baselines, the way baseline PNGs serve `diff-render`.
  Sensor colliders exist in M8 *only* as trace events; reacting to them at runtime is M10
  scripting's problem.
- **`engine screenshot --steps N`** is the single most important line in this table — it extends
  the engine's core command (engine doc §2.4) into the time dimension: edit → simulate → *look*.
- **`engine raycast`** answers spatial questions in JSON
  (`{"hit": {"entity": "Ground", "point": [...], "normal": [...], "distance": 4.5}}` or
  `{"hit": null}`) — the agent's substitute for clicking in a viewport.

All commands keep the standard contract: structured JSON errors on stderr, non-zero exit.

## 8. Determinism, Concretely

- Fixed `dt` from `timestep_hz`; integer step counts; no clock reads anywhere headless.
- `rapier3d` with `enhanced-determinism` enabled unconditionally — cross-platform IEEE-754
  reproducibility, no per-platform goldens.
- Version pinned exactly; upgrades reviewed against golden traces (§3).
- Enforced by test, not by hope: the suite runs the same scene twice in one process and asserts
  the traces are byte-identical, and compares against a committed golden trace to catch
  cross-machine or cross-version drift.

## 9. Validation

New `EngineError` cases, all through the existing path (all-at-once, file/line via
`lineindex.rs`, `suggest_from` for names):

- `unknown_shape` — `"shape": "cubiod"` → `did_you_mean: "cuboid"`; same for `body` kinds.
- `invalid_shape_dimension` — non-positive `radius`, `half_extents`, `half_height`.
- `nonuniform_scale_on_round_collider` — per the §5 scale rule.
- `invalid_physics_value` — negative `density`, `damping`, or `gravity_scale` out of sense;
  `friction < 0`; `restitution` outside `[0, 1]`; `timestep_hz < 1`.
- `missing_collider` — a **dynamic** `RigidBody` with no `Collider` on the same entity. It
  would fall forever through everything; that is a mistake essentially always, so it is an
  error, not a warning (the error convention has no warning channel, deliberately).
- `missing_transform` — a `RigidBody` or `Collider` on an entity with no `Transform`.

After adding the components: `engine list-components > schemas/component-schema.json`, enforced
as ever by `repo_contracts.rs`.

## 10. Testing

All headless, all GPU-free, all unconditional (§2.6):

- **Settling:** cube dropped from y=5 onto a fixed ground plane rests at `y ≈ half_extent`
  within tolerance after N steps; velocity ≈ 0; `can_sleep` body reports sleeping.
- **Restitution:** `restitution: 1.0` ball bounces back to (near) drop height; `0.0` doesn't.
- **Determinism:** twice-run byte-identical traces + committed golden (§8).
- **Bake round-trip:** baked scene revalidates clean; simulating the bake for 0 steps changes
  nothing; bake-then-simulate equals simulate-straight-through for the same total steps.
- **Queries:** raycast down from above the cube hits the cube, not the plane; miss returns
  `hit: null`.
- **Validation:** each §9 error case fires with correct file/line/`did_you_mean`.

## 11. Build Order Within M8

1. **M8.0 — Data.** Components + scene `physics` block in `engine-core`, validation cases,
   schema regeneration. No rapier dependency yet; `validate` and `list-components` fully work.
2. **M8.1 — Simulation.** `engine-physics` crate, `PhysicsWorld` build/step/write-back,
   `engine simulate --steps --bake`, settling + determinism tests.
3. **M8.2 — Observability.** `--trace` with contact events, `engine screenshot --steps`,
   golden-trace test, bake round-trip tests.
4. **M8.3 — Queries + window.** `engine raycast`, physics stepping in `engine run-scene`.

M8.0 and M8.1 are the milestone's load-bearing halves; M8.2 is what makes it *agent-native*
rather than merely present. Don't ship M8 without traces.

## 12. Non-Goals (for M8)

- **Joints** — rapier has them, but joints need entity-to-entity references in scene JSON, and
  that reference format deserves its own deliberate design pass. Deferred, not rejected.
- **Mesh colliders** (`convex_hull`/`trimesh`) — after M3's asset pipeline settles (§5).
- **Runtime reactions to collisions** — sensors emit trace events only; gameplay responses are
  M10 scripting.
- **Character controller, vehicles, soft bodies, cloth, fluids.**
- **f64 simulation, multithreaded stepping** — both trade against §8 for scale this engine
  doesn't have.
