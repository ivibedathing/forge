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

**Scene:** `examples/scenes/car_track.json` — the drivable-car demo itself: a barrier-lined
rectangular circuit with real colliders, a box-chassis car that is a **dynamic RigidBody**
(≈1.5 t from collider density) riding on four `Wheel` components (M12 raycast suspension:
spring/damper per wheel, tire grip, drive and braking at the contact point, wheel visuals
that steer, spin, and compress). `scripts/car.rhai` is only the *driver* — pedals and a
speed-scaled steering wheel via `world.set_engine_force` / `set_brake` / `set_steering` —
plus a spring chase camera (`world.look_at`). **Timeline:**
`examples/scenes/car_track_lap.input.jsonl` — a committed 2 770-step recording (authored by
a closed-loop autopilot driving the real engine chunk-by-chunk) that laps the circuit three
times clockwise on real suspension, brakes, and parks on the start line.

The pass condition is the M11 thesis — *interactive never means unverifiable*:

```bash
engine validate examples/scenes/car_track.json
# Replay the lap headlessly; the car must return to the start line:
engine simulate examples/scenes/car_track.json --steps 2880 \
    --input examples/scenes/car_track_lap.input.jsonl --bake /tmp/lap.json
# → Car parked within 1.5 of the start line [0, 0.82, 9], speed ~0 (CLI test)
engine diff-render examples/scenes/car_track.json \
    examples/scenes/verify/baselines/m11_lap.png --steps 2880 \
    --input examples/scenes/car_track_lap.input.jsonl
# → bit-exact; a recorded drive is a pinnable render like any other pose.
#   The baseline includes the script's HUD overlay (speedometer + lap
#   timer, M11.6): the parked car reads SPEED 0 KM/H, LAP 3,
#   LAST 13.42 / BEST 13.42 — the simulate report carries the same lines
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

(M7's editor column is manual and re-run only when editor code changes; everything else is
scriptable and belongs in CI the day M6's diff-render lands.)
