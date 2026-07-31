# Milestone Verification Scenes (M4–M10)

Companion to `agent-native-engine-design.md` §8. For every milestone from M4 onward, this
document defines one small, canonical scene (plus supporting files where the milestone needs
them) that gets **created and run immediately after the milestone is implemented**. Each scene
is designed to do two jobs at once:

1. **Prove the new milestone** — every feature the milestone adds is visible in the scene's
   output (a PNG, a trace, a diff, a structured error).
2. **Regress everything before it** — each scene deliberately exercises the full stack built so
   far (JSON loading → ECS → validation → headless render → readback), so a green run is
   evidence that no earlier milestone broke.

Scene files land in `examples/scenes/verify/` (clips in `examples/scenes/verify/animations/`),
are committed, and are never edited casually afterward — they are regression fixtures. The JSON
in this document is the canonical content; copy it verbatim when the milestone lands.

Component shapes below follow the settled companion designs (`materials-lighting-design.md`,
`physics-design.md`, `animation-system-design.md`, `gui-editor-design.md`). If implementation
forces a shape change, update this document in the same commit.

## The standard check, run after every milestone

Every milestone's verification ends with the same closing sequence, in this order:

```bash
cargo test --workspace                       # the real check, not cargo build
engine build                                 # structured-error path still works
engine list-components > /tmp/cs.json && diff /tmp/cs.json schemas/component-schema.json
                                             # schema regenerated? (repo_contracts.rs enforces
                                             # this in tests too — the diff is the fast check)
for s in examples/scenes/*.json examples/scenes/verify/*.json; do
  engine validate "$s" || echo "REGRESSION: $s"
done                                         # every committed scene still validates
engine screenshot examples/scenes/demo_scene.json --out /tmp/demo.png
engine screenshot examples/scenes/verify/m4_lighting.json --out /tmp/m4.png   # once M4 exists
# ...and LOOK at the PNGs. From M6 onward, replace "look" with diff-render
# against the committed baselines (see M6).
```

Rules that apply throughout:

- **Look at every PNG produced.** "Rendered without crashing" does not distinguish a working
  renderer from a culled mesh (CLAUDE.md: winding bugs are invisible, not loud).
- Every intentionally-broken input must fail with **structured JSON on stderr and a non-zero
  exit** — check both, every time. A broken scene that exits 0 is itself a regression.
- After any component addition: regenerate `schemas/component-schema.json` or
  `repo_contracts.rs` fails.

Note on M3: M3 (glTF assets) is already done, so the M4 scene includes one glTF-meshed entity
(`examples/meshes/pyramid.gltf`, referenced relative to the scene file) — from M4 onward the
standard check regresses the asset pipeline too, and M9's skeletal phase (A2) is unblocked.

---

## M4 — Materials + lighting

**File:** `examples/scenes/verify/m4_lighting.json`

Two identical spheres differing only in roughness (specular response is only visible on curved
geometry — the reason `builtin:sphere` exists), an emissive beacon, the recommended sun+ambient
rig from `materials-lighting-design.md` §3, and the M2-era ground/camera setup so the whole
pre-M4 stack is still in the frame.

```json
{
  "name": "m4_lighting",
  "entities": [
    {
      "name": "Ground",
      "components": [
        { "type": "Transform", "scale": [10.0, 1.0, 10.0] },
        { "type": "Mesh", "asset": "builtin:plane" },
        { "type": "Material", "albedo": [0.35, 0.4, 0.32], "roughness": 0.9 }
      ]
    },
    {
      "name": "SphereRough",
      "components": [
        { "type": "Transform", "position": [-1.5, 1.0, 0.0] },
        { "type": "Mesh", "asset": "builtin:sphere" },
        { "type": "Material", "albedo": [0.8, 0.1, 0.1], "metallic": 0.0, "roughness": 0.9 }
      ]
    },
    {
      "name": "SphereSmooth",
      "components": [
        { "type": "Transform", "position": [1.5, 1.0, 0.0] },
        { "type": "Mesh", "asset": "builtin:sphere" },
        { "type": "Material", "albedo": [0.8, 0.1, 0.1], "metallic": 0.0, "roughness": 0.1 }
      ]
    },
    {
      "name": "Pyramid",
      "components": [
        { "type": "Transform", "position": [0.0, 0.0, -2.0], "rotation": [0.0, 30.0, 0.0] },
        { "type": "Mesh", "asset": "../../meshes/pyramid.gltf" },
        { "type": "Material", "albedo": [0.85, 0.7, 0.2], "metallic": 0.0, "roughness": 0.5 }
      ]
    },
    {
      "name": "Beacon",
      "components": [
        { "type": "Transform", "position": [0.0, 0.25, 2.0], "scale": [0.25, 0.25, 0.25] },
        { "type": "Mesh", "asset": "builtin:cube" },
        { "type": "Material", "albedo": [0.0, 0.0, 0.0], "emissive": [0.0, 1.0, 0.2] }
      ]
    },
    {
      "name": "Sun",
      "components": [
        { "type": "Transform", "rotation": [-50.0, 30.0, 0.0] },
        { "type": "DirectionalLight", "color": [1.0, 1.0, 1.0], "intensity": 1.0 }
      ]
    },
    {
      "name": "Ambient",
      "components": [
        { "type": "AmbientLight", "color": [1.0, 1.0, 1.0], "intensity": 0.05 }
      ]
    },
    {
      "name": "Player",
      "components": [
        { "type": "Transform", "position": [0.0, 2.5, 7.0], "rotation": [-14.0, 0.0, 0.0] },
        { "type": "Camera", "fov": 50.0, "active": true }
      ]
    }
  ]
}
```

**Run after implementing M4:**

```bash
engine validate examples/scenes/verify/m4_lighting.json          # exit 0, no output
engine screenshot examples/scenes/verify/m4_lighting.json --out /tmp/m4_a.png
# LOOK at m4_a.png. Required observations:
#   1. Both spheres show a bright side (upper-left-ish, matching the [-50,30,0] sun) and a
#      dark-but-not-black side (ambient fill is working).
#   2. SphereSmooth has a visibly tighter, hotter specular highlight than SphereRough.
#   3. The Beacon is green despite black albedo (emissive bypasses lighting).
#   4. The Pyramid renders lit — the M3 glTF path feeds the M4 shader (normals survive import).
#   5. Ground, camera framing, builtin meshes all still work — the M0–M3 stack is intact.

# Acceptance-loop step 3 (the actual milestone): move the sun, see the shading move.
jq '(.entities[] | select(.name=="Sun").components[0].rotation) = [-50.0, 210.0, 0.0]' \
  examples/scenes/verify/m4_lighting.json > /tmp/m4_sun_moved.json
engine screenshot /tmp/m4_sun_moved.json --out /tmp/m4_b.png
# LOOK: bright/dark sides of both spheres must have swapped relative to m4_a.png.

# Error path: misspelled component gets did_you_mean.
jq '(.entities[] | select(.name=="Sun").components[1].type) = "DirectionelLight"' \
  examples/scenes/verify/m4_lighting.json > /tmp/m4_typo.json
engine validate /tmp/m4_typo.json
# Expect exit != 0 and stderr JSON containing "did_you_mean": "DirectionalLight" with file/line.
```

Then the standard check. New headless tests from `materials-lighting-design.md` §11 (lit-face
ordering, sun flip, ambient-only, emissive ≈ `srgb_encode(emissive)`, roughness highlight
comparison) must be in `cargo test --workspace` by the time this scene is committed.

**What this regresses:** JSON scene loading, hecs spawn, builtin meshes, glTF import, camera
math, headless readback, validation with line numbers — everything M0–M3 built, now through
the new sRGB target (every old pixel expectation changed; the tests prove they changed
*correctly*).

---

## M5 — Validation hardening

**File:** `examples/scenes/verify/m5_broken.json` — the one scene in this document that must
**never validate**. It packs one instance of every error class into a single file to prove the
all-errors-at-once contract: one run reports all of them, each with file/line, each with
`did_you_mean` where a name is close.

```json
{
  "name": "m5_broken",
  "entities": [
    {
      "name": "TypoCube",
      "components": [
        { "type": "Transform", "position": [0.0, 1.0, 0.0] },
        { "type": "Mesh", "asset": "builtin:cube" },
        { "type": "Meterial", "albedo": [0.9, 0.1, 0.1] }
      ]
    },
    {
      "name": "BadRanges",
      "components": [
        { "type": "Transform", "position": [2.0, 1.0, 0.0] },
        { "type": "Mesh", "asset": "builtin:sphere" },
        { "type": "Material", "albedo": [1.5, 0.0, 0.0], "roughness": 1.5 }
      ]
    },
    {
      "name": "MissingAsset",
      "components": [
        { "type": "Transform" },
        { "type": "Mesh", "asset": "meshes/does_not_exist.glb" }
      ]
    },
    {
      "name": "Sun",
      "components": [
        { "type": "Transform", "rotation": [-50.0, 30.0, 0.0] },
        { "type": "DirectionalLight", "colour": [1.0, 1.0, 1.0], "intensity": -2.0 }
      ]
    },
    {
      "name": "CameraA",
      "components": [
        { "type": "Transform", "position": [0.0, 2.0, 7.0] },
        { "type": "Camera", "active": true }
      ]
    },
    {
      "name": "CameraB",
      "components": [
        { "type": "Transform", "position": [0.0, 5.0, 0.0] },
        { "type": "Camera", "active": true }
      ]
    }
  ]
}
```

**Run after implementing M5:**

```bash
engine validate examples/scenes/verify/m5_broken.json 2> /tmp/m5_errors.json; echo "exit=$?"
# Expect exit != 0, and stderr must contain ALL of the following IN ONE RUN:
#   unknown_component  "Meterial"        with did_you_mean "Material"      + line number
#   value_out_of_range for albedo[0]=1.5                                   + line number
#   value_out_of_range for roughness=1.5                                   + line number
#   asset_not_found    for meshes/does_not_exist.glb                       + line number
#   unknown field      "colour"          with did_you_mean "color"         + line number
#   value_out_of_range for intensity=-2.0                                  + line number
#   multiple_active_cameras naming CameraA and CameraB
jq -s 'length' /tmp/m5_errors.json     # errors are parseable JSON, count >= 7

# The positive twin: the M4 scene must still pass with zero output.
engine validate examples/scenes/verify/m4_lighting.json && echo "clean"

# engine build's error path: introduce a deliberate Rust syntax error in a scratch branch or
# stash, run `engine build`, and confirm the compiler diagnostic is re-emitted as one
# structured EngineError JSON object (file/line/message), exit != 0. Revert.
```

Then the standard check. This scene is committed **broken**, so the standard check's
validate-everything loop must special-case it: `verify/m5_broken.json` failing is the pass
condition (invert the check for this one file).

**What this regresses:** the entire validation pipeline — schema pass, semantic pass,
`lineindex` line resolution, `suggest_from` — plus M4's new error codes, all at once.

---

## M6 — Diff-render / visual regression

No new scene: M6's fixture is a **baseline PNG of the M4 scene**, which is exactly the point —
the milestone turns "look at it" into a machine-checkable step, and the M4 scene is the most
feature-dense render we have.

**Files:** `examples/scenes/verify/baselines/m4_lighting.png` (committed, rendered once at a
fixed size on a known-good build).

**Run after implementing M6:**

```bash
# 1. Create the baseline (once, then commit it):
engine screenshot examples/scenes/verify/m4_lighting.json \
  --out examples/scenes/verify/baselines/m4_lighting.png --width 640 --height 360

# 2. Self-diff must pass: identical scene vs its own baseline.
engine diff-render examples/scenes/verify/m4_lighting.json \
  examples/scenes/verify/baselines/m4_lighting.png --out /tmp/m6_selfdiff.png
echo "exit=$?"                                    # 0, and stdout JSON reports ~0 differing pixels

# 3. A real change must fail loudly: recolor one sphere, diff again.
jq '(.entities[] | select(.name=="SphereSmooth").components[2].albedo) = [0.1, 0.1, 0.9]' \
  examples/scenes/verify/m4_lighting.json > /tmp/m6_changed.json
engine diff-render /tmp/m6_changed.json \
  examples/scenes/verify/baselines/m4_lighting.png --out /tmp/m6_diff.png
echo "exit=$?"                                    # != 0, stdout JSON reports the pixel count
# LOOK at m6_diff.png: the highlighted region must be the right sphere and nothing else.

# 4. Odd width: exercises the COPY_BYTES_PER_ROW_ALIGNMENT unpadding path.
engine screenshot examples/scenes/verify/m4_lighting.json --out /tmp/m6_odd.png --width 333 --height 191
```

Then the standard check — and **from this milestone on, the standard check's "look at the
PNGs" step becomes `engine diff-render` against committed baselines** for every verify scene
that has one. Each later milestone adds its scene's baseline in the same commit that adds the
scene.

**What this regresses:** the full render path (any earlier-milestone rendering regression now
fails a diff instead of relying on eyeballs), screenshot sizing/padding, structured output.

---

## M7 — GUI editor

**File:** none new — the editor is a view onto existing files, and verifying it *with the
files other milestones already regress* is itself the test. Use
`examples/scenes/verify/m4_lighting.json` as the working copy (on a scratch branch, since the
test mutates it) plus `verify/m5_broken.json` for the validation panel.

M7 is the one milestone that needs a human at the keyboard for parts of its check (gizmo
drags); everything else stays scriptable.

**Run after implementing M7 (per editor milestone E0–E2, `gui-editor-design.md` §8):**

```bash
git switch -c m7-editor-check
engine edit examples/scenes/verify/m4_lighting.json &   # editor opens: viewport + hierarchy + inspector

# 1. External-edit-wins (E0): with the editor open, move a sphere from the shell.
jq '(.entities[] | select(.name=="SphereSmooth").components[0].position) = [1.5, 3.0, 0.0]' \
  examples/scenes/verify/m4_lighting.json > /tmp/m7.json && mv /tmp/m7.json \
  examples/scenes/verify/m4_lighting.json
# WATCH: the sphere must rise in the viewport within ~1s, no editor interaction needed.

# 2. Write-through + format preservation (E1): in the inspector, set SphereSmooth
#    roughness to 0.3 and press enter. Then:
git diff examples/scenes/verify/m4_lighting.json
# Expect exactly ONE hunk touching ONE line (the roughness value). Any reordering,
# reformatting, or churn elsewhere in the file is a FAILURE of principle #5.

# 3. Gizmo commit (E2): drag SphereRough somewhere with the translate gizmo, release.
git diff examples/scenes/verify/m4_lighting.json
# Expect one additional small hunk on that entity's position only.

# 4. Round-trip test exists and passes (load → save → byte-identical for untouched content):
cargo test -p engine-core formatter

# 5. Validation panel: open the broken scene read-only.
engine edit examples/scenes/verify/m5_broken.json --watch
# WATCH: the validation panel lists the same errors `engine validate` prints — same codes,
# same lines — and writes are disabled.

git switch - && git branch -D m7-editor-check
```

Then the standard check, which now doubles as proof the editor changed no engine behavior:
every scene validates and diff-renders identically to before the editor crate existed
(`engine-editor` must be a pure client — nothing depends on it).

**What this regresses:** scene loading/validation through the same code paths the editor
reuses, plus render (the viewport is `engine-render`); the git-diff assertions catch any
formatter regression that would corrupt the agent's medium.

---

## M8 — Physics

**Files:** `examples/scenes/verify/m8_drop.json`, and after first run the committed golden
trace `examples/scenes/verify/baselines/m8_drop.trace.jsonl`.

A tumbling cube dropped onto static ground, plus a bouncy sphere — settling, restitution,
static colliders, and the scene-level physics block in one file. Lit with the M4 rig so the
baked-scene screenshot also regresses rendering.

```json
{
  "name": "m8_drop",
  "physics": { "gravity": [0.0, -9.81, 0.0], "timestep_hz": 60 },
  "entities": [
    {
      "name": "Ground",
      "components": [
        { "type": "Transform", "scale": [10.0, 1.0, 10.0] },
        { "type": "Mesh", "asset": "builtin:plane" },
        { "type": "Material", "albedo": [0.35, 0.4, 0.32], "roughness": 0.9 },
        { "type": "Collider", "shape": "cuboid", "half_extents": [5.0, 0.05, 5.0] }
      ]
    },
    {
      "name": "DropCube",
      "components": [
        { "type": "Transform", "position": [0.0, 5.0, 0.0], "rotation": [0.0, 15.0, 10.0] },
        { "type": "Mesh", "asset": "builtin:cube" },
        { "type": "Material", "albedo": [0.9, 0.15, 0.15], "roughness": 0.8 },
        { "type": "RigidBody", "body": "dynamic" },
        { "type": "Collider", "shape": "cuboid", "half_extents": [0.5, 0.5, 0.5] }
      ]
    },
    {
      "name": "BouncyBall",
      "components": [
        { "type": "Transform", "position": [2.0, 4.0, -1.0] },
        { "type": "Mesh", "asset": "builtin:sphere" },
        { "type": "Material", "albedo": [0.2, 0.35, 0.9], "roughness": 0.4 },
        { "type": "RigidBody", "body": "dynamic" },
        { "type": "Collider", "shape": "sphere", "radius": 0.5, "restitution": 0.9 }
      ]
    },
    {
      "name": "Sun",
      "components": [
        { "type": "Transform", "rotation": [-50.0, 30.0, 0.0] },
        { "type": "DirectionalLight" }
      ]
    },
    {
      "name": "Ambient",
      "components": [ { "type": "AmbientLight", "intensity": 0.05 } ]
    },
    {
      "name": "Player",
      "components": [
        { "type": "Transform", "position": [0.0, 3.0, 10.0], "rotation": [-12.0, 0.0, 0.0] },
        { "type": "Camera", "fov": 50.0, "active": true }
      ]
    }
  ]
}
```

**Run after implementing M8 (per phase, `physics-design.md` §11):**

```bash
# M8.0 — data only (no rapier yet): scene validates, schema regenerated.
engine validate examples/scenes/verify/m8_drop.json

# M8.1 — simulation:
engine simulate examples/scenes/verify/m8_drop.json --steps 300 --bake /tmp/m8_settled.json
engine validate /tmp/m8_settled.json                 # a bake is a VALID scene file
jq '.entities[] | select(.name=="DropCube").components[0].position[1]' /tmp/m8_settled.json
# Expect ≈ 0.55 (cube half-extent 0.5 resting on ground top at y=0.05), tolerance ~0.02.

# M8.2 — observability, the edit→simulate→LOOK loop:
engine screenshot examples/scenes/verify/m8_drop.json --steps 0   --out /tmp/m8_t0.png
engine screenshot examples/scenes/verify/m8_drop.json --steps 300 --out /tmp/m8_t300.png
# LOOK: t0 shows cube airborne at y=5; t300 shows it resting on the plane, ball settled.
# Add baselines + diff-render for both once satisfied.

# Determinism is the contract — byte-identical traces:
engine simulate examples/scenes/verify/m8_drop.json --steps 300 --trace /tmp/m8_a.jsonl
engine simulate examples/scenes/verify/m8_drop.json --steps 300 --trace /tmp/m8_b.jsonl
cmp /tmp/m8_a.jsonl /tmp/m8_b.jsonl && echo "deterministic"
cmp /tmp/m8_a.jsonl examples/scenes/verify/baselines/m8_drop.trace.jsonl   # golden (commit on first run)

# Contact events appear in the trace:
grep '"contact"' /tmp/m8_a.jsonl | head -3          # DropCube/Ground contact, started:true

# Bake round-trip: bake at 150, continue 150 more, must equal straight-through 300.
engine simulate examples/scenes/verify/m8_drop.json --steps 150 --bake /tmp/m8_mid.json
engine simulate /tmp/m8_mid.json --steps 150 --bake /tmp/m8_resumed.json
engine simulate examples/scenes/verify/m8_drop.json --steps 300 --bake /tmp/m8_straight.json
diff /tmp/m8_resumed.json /tmp/m8_straight.json && echo "bake round-trip holds"

# M8.3 — queries:
engine raycast examples/scenes/verify/m8_drop.json --from 0,10,0 --dir 0,-1,0 --steps 300
# Expect {"hit": {"entity": "DropCube", ...}} — the cube, not the ground under it.

# Error paths:
#   dynamic body without collider  -> missing_collider
#   "shape": "cubiod"              -> unknown_shape, did_you_mean "cuboid"
#   nonuniform scale on the ball   -> nonuniform_scale_on_round_collider
```

Then the standard check. Physics tests are GPU-free and unconditional — no skip path.

**What this regresses:** validation (new error codes through the same `EngineError` path),
scene round-tripping (bake must byte-preserve untouched fields — the M7 formatter again),
rendering (`--steps` screenshots), and Euler-degree transform conventions (write-back).

---

## M9 — Animation

**Files:** `examples/scenes/verify/m9_spin.json` and
`examples/scenes/verify/animations/spin.anim.json`.

The clip (the canonical 0°→360° spin that quaternion slerp would silently no-op — the exact
failure this design avoids, per `animation-system-design.md` §3):

```json
{
  "name": "spin",
  "tracks": [
    {
      "entity": "SpinCube",
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

The scene — one animated cube next to one static reference cube (so a filmstrip shows motion
against a fixed anchor), M4 lighting rig, standard camera:

```json
{
  "name": "m9_spin",
  "entities": [
    {
      "name": "Ground",
      "components": [
        { "type": "Transform", "scale": [10.0, 1.0, 10.0] },
        { "type": "Mesh", "asset": "builtin:plane" },
        { "type": "Material", "albedo": [0.35, 0.4, 0.32], "roughness": 0.9 }
      ]
    },
    {
      "name": "SpinCube",
      "components": [
        { "type": "Transform", "position": [-1.2, 0.5, 0.0] },
        { "type": "Mesh", "asset": "builtin:cube" },
        { "type": "Material", "albedo": [0.9, 0.15, 0.15], "roughness": 0.8 },
        { "type": "AnimationPlayer", "clip": "animations/spin.anim.json" }
      ]
    },
    {
      "name": "AnchorCube",
      "components": [
        { "type": "Transform", "position": [1.2, 0.5, 0.0], "rotation": [0.0, 45.0, 0.0] },
        { "type": "Mesh", "asset": "builtin:cube" },
        { "type": "Material", "albedo": [0.2, 0.35, 0.9], "roughness": 0.6 }
      ]
    },
    {
      "name": "Sun",
      "components": [
        { "type": "Transform", "rotation": [-50.0, 30.0, 0.0] },
        { "type": "DirectionalLight" }
      ]
    },
    {
      "name": "Ambient",
      "components": [ { "type": "AmbientLight", "intensity": 0.05 } ]
    },
    {
      "name": "Player",
      "components": [
        { "type": "Transform", "position": [0.0, 2.4, 7.0], "rotation": [-14.0, 0.0, 0.0] },
        { "type": "Camera", "fov": 50.0, "active": true }
      ]
    }
  ]
}
```

**Run after implementing M9 (per phase A0–A2, `animation-system-design.md` §7):**

```bash
# A0 — sampling is pure and validated (no rendering involved yet):
engine validate examples/scenes/verify/m9_spin.json      # follows the clip reference
engine validate examples/scenes/verify/animations/spin.anim.json
cargo test -p engine-core animation                      # interpolation, looping, 0→360 spin

# A1 — time reaches the CLI; the pixel-level determinism proof:
engine screenshot examples/scenes/verify/m9_spin.json --time 0.0  --out /tmp/m9_t0.png
engine screenshot examples/scenes/verify/m9_spin.json --time 0.25 --out /tmp/m9_t025.png
engine screenshot examples/scenes/verify/m9_spin.json --time 2.0  --out /tmp/m9_t2.png
cmp /tmp/m9_t0.png /tmp/m9_t025.png && echo "FAIL: cube did not move"     # must DIFFER (45° yaw)
cmp /tmp/m9_t0.png /tmp/m9_t2.png   && echo "loop period exact"           # must be IDENTICAL

engine filmstrip examples/scenes/verify/m9_spin.json --out /tmp/m9_strip.png --frames 8 --columns 4
# LOOK at m9_strip.png: SpinCube visibly rotates across the 8 tiles; AnchorCube is identical
# in every tile (nothing animates what the clip doesn't target).

engine list-animations examples/scenes/verify/m9_spin.json
# Expect JSON: clip "spin", duration 2.0, one track targeting SpinCube Transform.rotation.

# Visual regression gains a time axis (M6 + M9 composing):
engine diff-render examples/scenes/verify/m9_spin.json \
  examples/scenes/verify/baselines/m9_t025.png --time 0.25 --out /tmp/m9_diff.png

# Error paths (all-at-once, file/line, did_you_mean):
#   track entity "SpinCub"                  -> unknown_entity, did_you_mean "SpinCube"
#   property "Transform.rotaton"            -> unknown_property, did_you_mean "rotation"
#   keys with non-increasing times          -> unsorted_keys naming the key index
#   two players animating SpinCube rotation -> conflicting_tracks naming both

# A2 — skeletal (builds on the M3 glTF pipeline, which is done): check a small rigged .glb
# into examples/meshes/, then:
#   engine list-animations examples/meshes/rig.glb      # clips enumerable from binary
#   engine filmstrip on a scene playing rig.glb#<Clip>  # skinned motion visible in one PNG
```

Then the standard check. If M8 is already in, also run one combined scene check: an
`AnimationPlayer` targeting a `dynamic` rigid body must fail validation (or whatever rule the
open ownership question in `animation-system-design.md` §9 settled to — settle it before this
milestone closes, and encode the answer as a test here).

**What this regresses:** validation across file references (scene → clip), the screenshot
path (now parameterized by time), diff-render, Euler-degree semantics (the 0→360 behavior),
and determinism end to end — `cmp` on PNGs is the strictest check in this whole document.

---

## M10 — Scripting

> **Resolved 2026-07-27: Rhai** (settled with the user; see `scripting-design.md`). Scripts
> are `.rhai` files defining `fn step(world, step)`, run once per fixed step, before physics.

**Files:** `examples/scenes/verify/m10_script.json`,
`examples/scenes/verify/scripts/elevator.rhai`.

The scene: a script-driven platform that rises 2 units over the first 120 steps, with a
sensor collider at the top that the trace must record — scripting observed through the two
channels that already exist (screenshots and traces) rather than any new one.

```json
{
  "name": "m10_script",
  "physics": { "timestep_hz": 60 },
  "entities": [
    {
      "name": "Ground",
      "components": [
        { "type": "Transform", "scale": [10.0, 1.0, 10.0] },
        { "type": "Mesh", "asset": "builtin:plane" },
        { "type": "Material", "albedo": [0.35, 0.4, 0.32], "roughness": 0.9 }
      ]
    },
    {
      "name": "Elevator",
      "components": [
        { "type": "Transform", "position": [0.0, 0.25, 0.0], "scale": [1.5, 0.25, 1.5] },
        { "type": "Mesh", "asset": "builtin:cube" },
        { "type": "Material", "albedo": [0.8, 0.6, 0.1], "roughness": 0.5 },
        { "type": "RigidBody", "body": "kinematic" },
        { "type": "Collider", "shape": "cuboid", "half_extents": [0.5, 0.5, 0.5] },
        { "type": "Script", "source": "scripts/elevator.rhai" }
      ]
    },
    {
      "name": "TopSensor",
      "components": [
        { "type": "Transform", "position": [0.0, 2.5, 0.0] },
        { "type": "Collider", "shape": "cuboid", "half_extents": [1.0, 0.25, 1.0], "sensor": true }
      ]
    },
    {
      "name": "Sun",
      "components": [
        { "type": "Transform", "rotation": [-50.0, 30.0, 0.0] },
        { "type": "DirectionalLight" }
      ]
    },
    {
      "name": "Ambient",
      "components": [ { "type": "AmbientLight", "intensity": 0.05 } ]
    },
    {
      "name": "Player",
      "components": [
        { "type": "Transform", "position": [0.0, 2.4, 8.0], "rotation": [-12.0, 0.0, 0.0] },
        { "type": "Camera", "fov": 50.0, "active": true }
      ]
    }
  ]
}
```

The script (the elevator is kinematic so its collider rides the scripted Transform through
physics, which is what makes the sensor contact traceable):

```rhai
fn step(world, step) {
    if step < 120 {
        let p = world.position("Elevator");
        world.set_position("Elevator", p[0], p[1] + 2.0 / 120.0, p[2]);
    }
}
```

**Run after implementing M10:**

```bash
engine validate examples/scenes/verify/m10_script.json    # script path resolved like any asset

# Scripted motion is deterministic and screenshot-visible (the M8 loop, driven by a script):
engine screenshot examples/scenes/verify/m10_script.json --steps 0   --out /tmp/m10_t0.png
engine screenshot examples/scenes/verify/m10_script.json --steps 120 --out /tmp/m10_t120.png
# LOOK: elevator on the ground at step 0, up at y≈2.25 at step 120, inside the sensor region.

# The script's effect is trace-observable, and determinism still holds with scripts running:
engine simulate examples/scenes/verify/m10_script.json --steps 150 --trace /tmp/m10_a.jsonl
engine simulate examples/scenes/verify/m10_script.json --steps 150 --trace /tmp/m10_b.jsonl
cmp /tmp/m10_a.jsonl /tmp/m10_b.jsonl && echo "still deterministic with scripts"
grep '"contact": \["Elevator", "TopSensor"\]' /tmp/m10_a.jsonl    # sensor event recorded

# Baked output stays schema-valid — scripts mutate the world, never invent hidden state:
engine simulate examples/scenes/verify/m10_script.json --steps 120 --bake /tmp/m10_baked.json
engine validate /tmp/m10_baked.json
jq '.entities[] | select(.name=="Elevator").components[0].position[1]' /tmp/m10_baked.json  # ≈2.25

# Error path: a script runtime error must surface as structured JSON naming the script
# file and line, exit != 0 — not a panic, not a silent no-op.
```

Then the standard check, now at full depth: every verify scene from M4 through M10 validates,
renders, diffs against its baseline, and simulates deterministically.

**What this regresses:** everything — scripting is the last layer, and this scene alone
touches scene loading, validation, lighting, rendering, physics stepping, traces, baking, and
structured errors in one run.

---

## M11 — Input (keyboard, replayable)

**Scene:** `examples/scenes/car_track.json` — the drivable-car demo itself: a ≈546 m
barrier-lined circuit that climbs and drops through ≈7.6 m of elevation on real colliders
(Spa in miniature — La Source, Eau Rouge, the Kemmel climb, Les Combes, Rivage, Pouhon,
Stavelot, Blanchimont, the Bus Stop chicane), and a box-chassis car that is a **dynamic
RigidBody** (≈1.5 t from collider density) riding on four `Wheel` components (M12 raycast
suspension: spring/damper per wheel, tire grip, drive and braking at the contact point, wheel
visuals that steer, spin, and compress). `scripts/car.rhai` is only the *driver* — pedals and
a speed-scaled steering wheel via `world.set_engine_force` / `set_brake` / `set_steering` —
plus a spring chase camera (`world.look_at`) that now tracks the car's *height* as well, since
the circuit no longer lies flat.

The scene is **generated**, by `examples/scenes/make_car_track.py` from a closed polygon of
named corners; `examples/scenes/make_car_track_lap.py` then drives it to author the timeline.
Regenerating either means regenerating the other and re-blessing the baseline. **Timeline:**
`examples/scenes/car_track_lap.input.jsonl` — a committed recording (authored by a closed-loop
autopilot driving the real engine, replaying from step 0 every round so the recording cannot
drift from the drive that made it) that laps the circuit three times clockwise on real
suspension, brakes, and parks just past the start line.

The pass condition is the M11 thesis — *interactive never means unverifiable*:

```bash
engine validate examples/scenes/car_track.json
# Replay the drive headlessly; the car must come back to the start line:
engine simulate examples/scenes/car_track.json --steps 11634 \
    --input examples/scenes/car_track_lap.input.jsonl --bake /tmp/lap.json
# → Car stopped a few meters past the line at [-62.8, ~6.4, -43.6], speed 0.
#   The same replay is sampled mid-drive at two places (CLI test): out east
#   above y=7 near the crest (step 1800), and down at Stavelot below y=2.5
#   (step 6600) — one
#   recording that climbs and descends is what makes the elevation real.
engine diff-render examples/scenes/car_track.json \
    examples/scenes/verify/baselines/m11_lap.png --steps 11634 \
    --input examples/scenes/car_track_lap.input.jsonl
# → bit-exact; a recorded drive is a pinnable render like any other pose.
#   The baseline includes the script's HUD overlay (speedometer + lap
#   timer, M11.6): the parked car reads SPEED 0 KM/H, LAP 4,
#   LAST 63.70 / BEST 59.47 — the simulate report carries the same lines
#   as "hud", so the timing is also asserted without a GPU
engine run-scene examples/scenes/car_track.json   # the playable version
```

A broken timeline (typo'd key, junk line, non-increasing steps) must fail with every error at
once — `unknown_key` + `did_you_mean`, `input_parse_error`, `unsorted_input_steps` — and the
M8 golden trace must still match: no `--input` means no keys held, byte-for-byte.

**What this regresses:** the whole input path (timeline parse → `world.key` → script → bake /
render), `world.look_at`, and the determinism promise extended over recorded input.

---

## M12 — HUD components: `verify/m12_hud.json`

Screen-anchored `HudText` + `HudRect` components (hud-design.md), rendered by the same
rasterizer/overlay pass as the M11.6 `world.hud` lines. The fixture covers every anchor, a
glyph-coverage line, rect-under-text draw order, a fractional-opacity panel, and a script
(`verify/scripts/m12_hud.rhai`) that writes the step counter into a `HudText` and stretches a
`HudRect` one pixel per step.

```bash
engine validate examples/scenes/verify/m12_hud.json
engine diff-render examples/scenes/verify/m12_hud.json     examples/scenes/verify/baselines/m12_hud.png --steps 60
# → bit-exact: "M12 HUD" top-left on its translucent panel, coverage line
#   top-right, BL/BR corner labels, "+" dead center, STEP 60, and the green
#   bar at 100 of 160 px (40 rest + 60 steps)
engine simulate examples/scenes/verify/m12_hud.json --steps 60 --bake /tmp/m12.json
# → the baked file reads "STEP 60" and "size": [100.0, 10.0] — script-driven
#   HUD state bakes under the change-based rule (CLI test)
```

The car demo carries the applied version: a `SpeedBar` HudRect gauge (bottom-left) driven by
`world.set_hud_rect_size` from the same speed the `world.hud` readout shows —
`verify/baselines/m11_lap.png` includes it (re-blessed with M12; timeline and physics
untouched, the parked bar is empty over its backdrop).

**What this regresses:** the component overlay (anchor math, draw order, opacity, glyph
rendering), the schema validation of HUD components (anchor enum with `did_you_mean`, size and
color ranges), the M12 script accessors, and HUD-field bake.

---

## M13 — Particles

**Scene:** `examples/scenes/verify/m13_smoke.json` — a campfire: gray ground plane, a small
emissive orange ember cube, and a `ParticleEmitter` on an entity rotated `[90, 0, 0]` so its
local −Z (the same axis cameras and lights aim down) points up. The emitter is authored as
smoke: 24 particles/sec, 3 s lifetime, a 12° cone, gentle buoyancy plus a slight crosswind
(`acceleration: [0.15, 0.5, 0]`), drag, sprites that grow from 0.12 to 0.55 half-size while
fading from α 0.85 to 0, and a fixed `seed`.

Particles are simulation state, not pose: they exist only after `--steps` (never `--time`),
they are never baked or traced (disposable, like solver caches), and the seeded per-emitter
RNG makes every run of the same file + steps byte-identical — which is what lets a
stochastic-looking effect live under a diff-render baseline.

```bash
engine validate examples/scenes/verify/m13_smoke.json
# Bless once (this is also the "look at it" step — the plume must rise from
# the ember, widen, drift with the wind, and fade out at the top):
engine screenshot examples/scenes/verify/m13_smoke.json \
    --out examples/scenes/verify/baselines/m13_smoke.png --steps 180
# From then on:
engine diff-render examples/scenes/verify/m13_smoke.json \
    examples/scenes/verify/baselines/m13_smoke.png --steps 180
# → bit-exact (same machine/adapter, the standard M6 caveat)
```

A `--steps 0` screenshot of the same scene shows no particles at all — the emitter at rest
draws nothing, so adding one to a scene never disturbs that scene's unstepped baseline.
Integer fields (`seed`, `max_particles`) validate like everything else: `"seed": 1.5` or
`-1` is `invalid_field_type`, `"max_particles": 0` is `value_out_of_range`, all located.

The car demo carries the applied version: an `Exhaust` emitter that `scripts/car.rhai` parks
at the tailpipe every step, from the same `world.forward` heading the driver already uses.
Because particles are world-space once spawned, the trail is left *behind* a moving car —
which is the visible proof that the emitter's position is sampled per step, not per render:

```bash
engine diff-render examples/scenes/car_track.json \
    examples/scenes/verify/baselines/m11_lap.png --steps 11634 \
    --input examples/scenes/car_track_lap.input.jsonl
# → bit-exact, smoke and all: a stochastic effect on a recorded drive is
#   still a pinnable render. m11_lap.png was re-blessed when the exhaust
#   landed; the timeline, the physics, and the HUD strings did not move,
#   because particles never feed back into simulation.
```

Emission answers to gameplay through `world.set_particle_rate`: the car's `SkidLeft` /
`SkidRight` emitters rest at `"rate": 0.0` and are driven off chassis sideslip (with a
1 m/s deadband, so suspension jitter is not a skid) plus a braking-lockup term, which is
why the tires smoke in the corners and go silent on the straights.

```bash
engine simulate examples/scenes/car_track.json --steps 550 \
    --input examples/scenes/car_track_lap.input.jsonl --bake /tmp/skid.json
# → SkidLeft bakes "rate": 60.0 under braking — a script-driven component
#   field bakes change-based, like a velocity or a gauge width. The same
#   bake at a straight-line step carries no rate edit at all, because the
#   gate wrote back the file's own 0.0. Particle *state* is in neither file.
```

A rate the schema forbids (negative, NaN, or too large for f32) is a `script_runtime_error`
at the call — not a file that bakes and then fails `validate`.

**What this regresses:** the particle step (spawn credit, cone sampling, integrate,
age-out), start→end interpolation, billboard rendering (camera-facing quads, soft-disc
falloff, alpha blending, back-to-front sort), the system order (scripts → physics →
particles → render), schema-driven validation of the first integer component fields, and
the script emission-rate accessor with its bake path.

---

## M14 — Breaking objects (pre-authored fragments)

**Scene:** `examples/scenes/verify/m14_break.json` — a breakable crate (four pre-authored
quarter-cube fragments, `impulse_threshold: 5.0`) standing on the ground with a heavy ball
falling on it. The ball smashes the crate at step 52; the fragments scatter and settle.

The pass condition is the M12 thesis — *a break is data, not an effect*:

```bash
engine validate examples/scenes/verify/m14_break.json
engine simulate examples/scenes/verify/m14_break.json --steps 300 --trace /tmp/m14.jsonl
# → the trace holds one {"broke": "Crate", "fragments": ["Crate.frag0", ...]} line at
#   step 52, and the fragment rows join from step 53; byte-identical to the committed
#   golden examples/scenes/verify/baselines/m14_break.trace.jsonl
engine diff-render examples/scenes/verify/m14_break.json \
    examples/scenes/verify/baselines/m14_break.png --steps 300
# → bit-exact: scattered fragments are a pinnable render like any other pose
engine simulate examples/scenes/verify/m14_break.json --steps 300 --bake /tmp/m14_baked.json
engine validate /tmp/m14_baked.json
# → the baked file has no "Crate" entity and four Crate.frag* entities with full state;
#   rendering it at --steps 0 equals the live scene at --steps 300, bit for bit (CLI test)

# Script triggers (CLI test `scripts_break_and_explode`): world.break_entity(name) breaks a
# threshold-less crate on demand; world.explode(x,y,z,radius,impulse) breaks a thresholded
# one in range and kicks its fragments outward. Both replay deterministically.

# Error path: {"type": "Breakable", "impulse_threshold": 5.0, ...} on an entity with no
# Collider must fail validation with breakable_without_collider, exit 1.
```

A scene with no `Breakable` must behave exactly as before M14 — the M8 golden trace and
every committed baseline stay byte-identical (force events only exist on breakable
colliders; the break phase without candidates is a no-op).

**What this regresses:** contact-event plumbing, the validation walk (arrays of objects),
trace shape, bake's new structural splices (`apply_remove_entity` / `apply_add_entity`),
and the script→physics queue boundary.

---

## M16 — Environment: sky, fog, shadows, MSAA, transparency

**Scene:** `examples/scenes/verify/m16_environment.json` — static, deliberately: every M16
feature is a property of one *frame*, so nothing here needs a clock. A lit ground plane with two
casters and a metal sphere, two walls receding to 62 m, a transmissive pool over a visible floor
with a box half-sunk in it, and a flat-`alpha` panel off to one side.

```bash
engine validate examples/scenes/verify/m16_environment.json
engine diff-render examples/scenes/verify/m16_environment.json \
    examples/scenes/verify/baselines/m16_environment.png
# → bit-exact (CLI test `the_m16_environment_fixture_pins_sky_fog_shadows_and_glass`)

# Error paths — each its own code, none of them silently accepted:
#   "samples": 2          → invalid_environment_value  (1 or 4, never rounded)
#   "fog_density": -1     → invalid_environment_value
#   "shadow_distance": 0  → invalid_environment_value
#   "shadow": true        → unknown_field, did_you_mean "shadows"
#   "sky": "yes"          → invalid_field_type
```

**The pass condition is as much about what did *not* change.** Every field of the `environment`
block defaults to off, and a scene without the block must render byte for byte as it did before
the block existed:

```bash
# Pre-M16 baselines, unchanged and not re-blessed:
engine diff-render examples/scenes/verify/m4_lighting.json \
    examples/scenes/verify/baselines/m4_lighting.png
engine diff-render examples/scenes/verify/m12_hud.json \
    examples/scenes/verify/baselines/m12_hud.png --steps 60
engine diff-render examples/scenes/verify/m13_smoke.json \
    examples/scenes/verify/baselines/m13_smoke.png --steps 180
# → 0 diff_pixels each. The showcase tour's six *are* re-blessed, because that scene opts in.
```

That property is fragile in a way worth writing down: it is not enough for the default path to be
an *equivalent* expression, it has to be the *same* expression. Whether the compiler may contract
`a*b + c` into an FMA depends on the surrounding code, and an FMA carries more intermediate
precision than the pair it replaces — restructuring the M4 lines in `mesh.wgsl` into arithmetic
that was equal on paper moved `m12_hud.png` by one ULP in one pixel. The lines computing
`direct` / `ambient` / `base_color` are therefore left exactly as M4 wrote them, ahead of every
M16 branch.

**What this regresses:** the shadow pass and its texel-snapped ortho fit, the shared sky gradient
concatenated into two shaders, the MSAA resolve (and the HUD staying single-sampled on the
resolved target, so glyphs are still pixel-exact), the sorted blended pass and its premultiplied
output, the scene-level block's hand-written validator, and the bit-exactness of every scene that
opts into none of it. Pixel-level coverage lives in `engine-render/tests/environment.rs`.

---

## M17 — fire and point lights

**Scene:** `examples/scenes/verify/m17_fire.json` + `verify/scripts/m17_fire.rhai` — a night
campfire seen from 3.6 m: stone ring, three logs, an emissive coal bed, five emitters (a white-hot
`FireBase`, the `Fire` body, breakaway `FireTongues`, alpha-blended `Smoke`, streaked `Embers`) and
a `FireLight` the script flickers. The scene is deliberately dark — a 0.16-intensity moon and a
0.035 ambient — so that what the point light does is unambiguous.

```
engine validate examples/scenes/verify/m17_fire.json
engine diff-render examples/scenes/verify/m17_fire.json \
    examples/scenes/verify/baselines/m17_fire.png --steps 240
#   pinned by cli.rs::the_m17_fire_fixture_pins_additive_flame_and_firelight

# The bake half: a script-driven light is scene state, and a baked fire is a scene again.
engine simulate examples/scenes/verify/m17_fire.json --steps 240 \
    --bake examples/scenes/verify/m17_baked.json
engine validate examples/scenes/verify/m17_baked.json
```

**The check that matters most is not in this list.** Every M17 field defaults to the pre-M17
behaviour, so the pass condition includes *twelve baselines that must not move*. A diff against a
committed baseline is the wrong instrument for that — a baseline can have drifted for unrelated
reasons, and during this milestone one had (`m14_break.png` was already one pixel off on `main`).
The right instrument is an **A/B between binaries**:

```bash
# Build the CLI at main and in the worktree, render the same scenes with both, cmp the bytes.
for spec in "verify/m4_lighting.json" "verify/m8_drop.json --steps 300" \
            "verify/m13_smoke.json --steps 180" "verify/m16_environment.json" \
            "showcase_tour.json --steps 270 --width 640 --height 360" ; do
  main/target/release/engine     screenshot $spec --out /tmp/a.png
  worktree/target/release/engine screenshot $spec --out /tmp/b.png
  cmp /tmp/a.png /tmp/b.png || echo "REGRESSION: $spec"
done
```

**What this regresses:** disc emission and the three jitter draws (including that each is *skipped*,
not defaulted, when off — the RNG draw order is a format contract), the in-repo noise field and its
smoothness, per-particle lifespan and size fixed at birth, the additive pipeline and the
alpha-then-additive draw order, velocity-stretched billboards, the point-light array and its
name-ordering, windowed inverse-square falloff reaching exactly zero at `range`, the script light
API across all three light components, and change-based baking of `intensity` and `color`. Pixel
coverage is `engine-render/tests/point_lights.rs` (six tests) and the M17 half of
`engine-render/tests/particles.rs` (five); the GPU-free simulation half is in
`engine-core/src/particles.rs`.

---

## M18 — Water: `verify/m18_water.json`

A lake over a bed that slopes from a shoreline down to four metres, with a post, a boulder and a
transmissive ice floe standing in it. One scene exercises everything the milestone added: Gerstner
waves displacing the grid in the vertex stage, per-pixel ripple normals, absorption between
`shallow_color` and `deep_color` against the depth behind the surface, crest foam where the wave
folds, shore foam where the water thins over the bed and around each object, sky reflection with a
Fresnel weight, and shadows landing on the surface.

```bash
cd examples/scenes
engine validate verify/m18_water.json
engine diff-render verify/m18_water.json verify/baselines/m18_water.png --steps 120
```

**Pass condition:** `"pass": true` with `diff_pixels: 0`, and the same for `--time 2.0` in place of
`--steps 120` — 120 steps at the scene's 60 Hz *is* two seconds, and the renderer has one clock.
`--steps 0` must **not** match: water at rest is a different picture, which is what proves the two
agreements above are not both trivially true. All three are pinned by
`the_m18_water_fixture_pins_waves_depth_and_foam` in the CLI suite.

**What this regresses:** the two-pass frame and its depth copy (including the MSAA path, since the
fixture asks for `samples: 4`), the wave packing (`Q = steepness/(k·A)` — get it wrong and the
surface either flattens or folds), analytic normals, the ripple slope field and its distance fade,
absorption and both foams, the sorted blended list shared with transparent meshes, and the whole
`--time` / `--steps` clock rule.

**The bit-exactness half is separate and matters more.** Water is opt-in per scene, and seventeen
milestones of baselines were blessed before it existed. Use the A/B between binaries as in M17, not
a diff against the baselines:

```bash
cd examples/scenes    # one scene dir; asset paths are scene-relative
for spec in "verify/m4_lighting.json" "verify/m8_drop.json --steps 100" \
            "verify/m13_smoke.json --steps 180" "verify/m14_break.json --steps 240" \
            "verify/m16_environment.json" "verify/m17_fire.json --steps 240" \
            "car_track.json --steps 300 --input car_track_lap.input.jsonl" ; do
  main/target/release/engine     screenshot $spec --out /tmp/a.png --width 400 --height 240
  worktree/target/release/engine screenshot $spec --out /tmp/b.png --width 400 --height 240
  cmp /tmp/a.png /tmp/b.png || echo "REGRESSION: $spec"
done
```

Fifteen scene/step combinations were byte-identical when this landed. The renderer-side guard is
`a_scene_with_no_water_is_untouched_by_the_water_pass` in `engine-render/tests/water.rs`, which
renders a water-free scene at two different times and requires identical bytes — nothing but water
may read the clock.

---

## M19 — trees

**Scene:** `examples/scenes/verify/m19_trees.json` — six trees on a lit ground plane, static (no
steps, no scripts): two broadleaves differing **only in `seed`**, a whorled conifer, a leafless
snag, a one-meter scrub, and a `Diagram` tree with `jitter`, `crook`, `tropism` and `flare` all
zeroed. The twins are the point of the fixture — they are the same species and visibly different
individuals, which is the property the milestone exists to add, and it has to survive under a
bit-exact baseline.

```
engine validate examples/scenes/verify/m19_trees.json
engine diff-render examples/scenes/verify/m19_trees.json \
    examples/scenes/verify/baselines/m19_trees.png
#   pinned by cli.rs::the_m19_tree_fixture_pins_seeded_procedural_growth

# The authoring loop: change one field, look at it. The Diagram tree is where a
# parameter's effect is visible on its own, because nothing else is moving.
engine screenshot examples/scenes/verify/m19_trees.json --out /tmp/trees.png --width 960 --height 540
```

**Two failures are part of the pass condition**, both no-GPU and both in the same CLI test:

```
# A Tree *is* the entity's geometry; a Mesh beside it is a second opinion.
{"type":"Tree"} + {"type":"Mesh","asset":"builtin:cube"}   → tree_with_mesh, exit 1

# Branching is exponential, so a plausible edit can ask for a billion vertices.
{"type":"Tree","levels":4,"branches":12,"sides":16,"segments":12} → tree_too_complex, exit 1
```

The second is the one worth re-running by hand after any change to `tree::vertex_count`: it is
computed from the parameters *before anything is allocated*, and it has to be the exact count
rather than an estimate (`vertex_count_predicts_what_generation_produces` walks six configurations
against real generation). A hung render with no output is the worst failure an agent loop can hit.

**What this regresses:** the tube sweep and its winding (a wrongly-wound tree renders as nothing
at all, so `every_wall_triangle_faces_outward` checks each wall face against the axis it was swept
around), parallel transport of the ring frame, power-curve taper, the root flare, golden-angle
phyllotaxis, the three stability rules discovered by rendering — whorls are trunk-only, tropism is
branch-only, and the trunk's random walk has a restoring term (each with its own multi-seed test),
double-sided flat-shaded leaves, the exact-bits mesh cache and its `Arc` identity contract, and
schema-driven validation of 24 new fields. The GPU-free half is 12 tests in
`engine-core/src/tree.rs`; the showcase forest is the applied version.

**Bless from a debug build.** Unlike every earlier fixture, this one is sensitive to the build
profile: procedural geometry does enough `sin_cos` work that a release build's libm routines move
three pixels of `m19_trees.png` and one of `showcase_90.png` by one channel step (see
`tree-design.md` §4 — it is measured, and it is not FMA). The committed baselines are blessed with
the binary `cargo test` runs, so the pinned test is exact; a release build checking them by hand
sees those pixels.

**Also re-blessed here:** all six `showcase_*` baselines, since station 01's twelve
cylinder-and-sphere entities became nine `Tree`s. No other baseline moved — an A/B between the
`main` binary and the worktree's over all sixteen pre-M19 scene/step combinations was
byte-identical, which is the check that actually settles it.

---

## M21 — day and night

**Scene:** `examples/scenes/verify/m21_daylight.json` — a pond in a basin, three trees, a boulder
and a wall for shadow shapes, and a lamp post whose `PointLight` a script raises off
`world.sun_altitude()`. `day_length: 24.0`, so an hour is a second and step `hour × 60` at 60 Hz is
that hour.

**The point of the fixture is that there is one file and five pictures.** The day is a pure function
of the clock, so nothing is authored per time of day.

```
engine validate examples/scenes/verify/m21_daylight.json

# 02:00, 06:30, noon, 18:30, 22:00 — all from the one scene
engine diff-render examples/scenes/verify/m21_daylight.json \
    examples/scenes/verify/baselines/m21_daylight_0200.png --steps 120
engine diff-render examples/scenes/verify/m21_daylight.json \
    examples/scenes/verify/baselines/m21_daylight_0630.png --steps 390
engine diff-render examples/scenes/verify/m21_daylight.json \
    examples/scenes/verify/baselines/m21_daylight_1200.png --steps 720
engine diff-render examples/scenes/verify/m21_daylight.json \
    examples/scenes/verify/baselines/m21_daylight_1830.png --steps 1110
engine diff-render examples/scenes/verify/m21_daylight.json \
    examples/scenes/verify/baselines/m21_daylight_2200.png --steps 1320
#   pinned by cli.rs::the_m21_daylight_fixture_pins_a_whole_day_from_one_file

# The whole cycle on one sheet — the fastest way to *look at* a day.
engine filmstrip examples/scenes/verify/m21_daylight.json \
    --out /tmp/day.png --start 0 --end 24 --frames 8 --columns 4
```

**`--steps`, not `--time`:** the lamp is script-driven, and scripts run on the step loop. A
`--time` render never steps, so it renders the right sky under a dark lamp. The filmstrip has the
same limitation by design, and is therefore not a committed baseline — it is also not something
`diff-render` could check, since that renders a scene at a baseline's dimensions.

**What this regresses:** the sun/moon arc and its east/west sense, the wrapping palette, the clock
(frozen at `day_length: 0`, cycling otherwise, `t` and `t + day_length` identical), the
dominant-body handoff and its brightness bound, the shadow-elevation clamp that keeps a horizon sun
from casting shadows upward, and the two ownership rules — `daylight_and_directional_light` as an
error, `daylight_overrides_sky` as a warning. 21 GPU-free tests in `engine-core/tests/daylight.rs`,
four in `validate.rs`, two at the CLI.

**Bless from a debug build**, for M19's reason: the fixture has trees.

**Also re-blessed here:** all six `showcase_*` baselines, since the tour's hand-aimed `Sun` and
`Sky` entities became a `daylight` block. No other baseline moved — an A/B between the merge-base
binary and the worktree's over 15 scenes × 5 step counts (75 combinations) was byte-identical,
which is the check that actually settles it.
---

## M23 — Roads: `verify/m23_road.json`

A closed circuit as one `Road` entity, over grass, with a ball dropped on it. One scene exercises
everything the milestone added: the ribbon swept from a polygon of corners, the monotone-cubic
height profile, asphalt/shoulder/embankment as one surface, edge lines and a dashed centre line
painted from the road's own surface coordinates, kerbs on the two corners tight enough to ask for
them, a start line placed by arc length, sun shadows landing on and cast by the road, and a trimesh
collider that is the same triangles that are drawn.

```bash
cd examples/scenes
engine validate verify/m23_road.json
engine simulate verify/m23_road.json --steps 180 --bake /tmp/rest.json
engine diff-render verify/m23_road.json verify/baselines/m23_road.png --steps 180
engine road-centerline verify/m23_road.json | head -c 200
```

**Pass condition:** `"pass": true` with `diff_pixels: 0`, and the baked `Ball` resting at
y ≈ 0.9 within half a metre of where it was dropped. The second half is the one that matters more:
a body that lands on a triangle mesh and then departs sideways at 5 m/s is what an unfixed internal
edge looks like, and it is silent in a screenshot. Both are pinned by
`the_m23_road_fixture_pins_markings_and_a_drivable_surface` in the CLI suite.

**What this regresses:** the corner fillets and the closed ring, the surface coordinates (`u`
across, `v` along) and every marking painted from them, kerb spans and side selection, dash-period
fitting, the road pipeline's place in the opaque pass, roads in the shadow pass through the
unchanged shadow pipeline, the UV upload added to the mesh cache for every mesh, and
`FIX_INTERNAL_EDGES` on trimesh colliders.

**The bit-exactness half** is the A/B between binaries, as in M17 and M18 — a road-less scene must
render byte for byte as it did, which includes every scene with a trimesh collider in it, since
`FIX_INTERNAL_EDGES` is scoped to road geometry precisely so that stays true.

---

## M26 — Materials: `verify/m26_materials.json`

A row of spheres spanning metallic and roughness with and without maps, a checkered floor and cube
for tiling and mips, a normal-mapped sphere whose material lives in `materials/dented.json`, a
cut-out foliage card casting a **cut** shadow, and a refracting glass block over the patterned
floor. The textures are generated by `examples/textures/make_textures.py`, committed, and described
in it.

Per M22's rule the camera **aims at its subject rather than across a landscape** — there is no
terrain in the frame — so this fixture carries a hard bit-exact pin rather than a tolerance. Four
consecutive renders come back `md5`-identical.

What it covers: the textured pipeline variant and the `with_surface` seam it is spliced at, colour
space by slot (the ORM sphere's roughness sweep is visibly a sweep, which it would not be if the map
were decoded as sRGB), the CPU mip chain (the floor at a grazing angle), `uv_scale` tiling, per-pixel
derived tangent frames, `Material.asset` resolution, `alpha_cutoff` in both the mesh pass and the
second caster pipeline, and refraction with Beer–Lambert attenuation.

**The bit-exactness half** is the A/B between binaries, as in M17, M18 and M23: 22 of 22 committed
scenes this milestone did not edit render byte for byte as they did at `main`. The seven not compared
are the ones whose *inputs* changed — the six showcase frames, whose scene now uses M26 fields, and
this fixture, which the `main` binary cannot parse.

---

## M27 — Water refraction: `verify/m27_water_refraction.json`

A clear pool over a bed of dark bars crossed by a red and a blue rail, with two posts and a boulder
standing through the surface. The bed is a *grid* on purpose: refraction is a displacement, so a
uniform bed cannot show it, and the displacement runs along the view direction — bars laid across
that axis move, bars laid along it barely do.

**Two baselines from one file**, via a second camera:

```
engine screenshot verify/m27_water_refraction.json --steps 120 --out m27_water_refraction.png
engine screenshot verify/m27_water_refraction.json --steps 120 --camera CameraGrazing \
    --out m27_water_grazing.png
```

- `Camera` looks down at the pool at 24°, where the grid is what refraction acts on. This is the
  frame that goes visibly wrong if the exit point is stepped along the refracted ray by the view
  ray's path length instead of solved to the bed's depth — the bars dice into rectangular blocks.
- `CameraGrazing` looks across at 8°, where the boulder and posts stand *in* the water. This is the
  framing the depth-validated sample exists for: dropping the check moves ~22k pixels of it by up
  to 99. On the overhead camera it moves **zero**, which is why the second camera is here at all.

Per M22's rule both cameras aim at the subject with no terrain in frame, so both carry hard
bit-exact pins rather than a tolerance; four consecutive sweeps came back at zero differing pixels.
Pinned by `cli.rs::the_m27_water_refraction_fixture_pins_a_bent_bed_and_a_clean_waterline`, which
also drops `ior` back to its default and requires the baseline to *stop* matching — a splice that
silently did nothing would otherwise pass every other assertion.

What it covers: the spliced refracting-water pipeline and its four anchors against `water.wgsl`,
the bed-depth solve, the depth-validated sample, the IOR riding in `clock.z`, and the
colour-copy/split-pass gate extended from `Material::refracts()` to `Water::refracts()`.

**The bit-exactness half** is the A/B between binaries: every committed scene this milestone did not
edit renders byte for byte as it did at `main`. Not compared are the ones whose *inputs* changed —
the six showcase frames, whose pond now carries an `ior`, and this fixture, which the `main` binary
cannot parse.

---

## Cumulative matrix

What must be green after each milestone lands (columns are the checks, ⬤ = required):

| After | m4 scene renders + acceptance loop | m5_broken fails correctly | baselines diff-clean | editor coexistence | m8 sim deterministic | m9 time-axis PNGs | m10 script run | `cargo test --workspace` |
|---|---|---|---|---|---|---|---|---|
| M4 | ⬤ | | | | | | | ⬤ |
| M5 | ⬤ | ⬤ | | | | | | ⬤ |
| M6 | ⬤ | ⬤ | ⬤ | | | | | ⬤ |
| M7 | ⬤ | ⬤ | ⬤ | ⬤ | | | | ⬤ |
| M8 | ⬤ | ⬤ | ⬤ | | ⬤ | | | ⬤ |
| M9 | ⬤ | ⬤ | ⬤ | | ⬤ | ⬤ | | ⬤ |
| M10 | ⬤ | ⬤ | ⬤ | | ⬤ | ⬤ | ⬤ | ⬤ |
| M17 | ⬤ | ⬤ | ⬤ | | ⬤ | ⬤ | ⬤ | ⬤ |
| M19 | ⬤ | ⬤ | ⬤ | | ⬤ | ⬤ | ⬤ | ⬤ |
| M23 | ⬤ | ⬤ | ⬤ | | ⬤ | ⬤ | ⬤ | ⬤ |
| M26 | ⬤ | ⬤ | ⬤ | | ⬤ | ⬤ | ⬤ | ⬤ |
| M27 | ⬤ | ⬤ | ⬤ | | ⬤ | ⬤ | ⬤ | ⬤ |

(M7's editor column is manual and re-run only when editor code changes; everything else is
scriptable and belongs in CI the day M6's diff-render lands.)
