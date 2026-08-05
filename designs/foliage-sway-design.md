# M46 — Foliage sway

Leaves and branches that move. A `Tree` since M19 is a recipe that grows two
static meshes and stands perfectly still forever, which is the single loudest
"this is a render, not a place" cue left in an outdoor scene: the grass moves
(M29), the water moves (M18), the clouds drift (M20), the sun crosses the sky
(M21), and the trees under all of it are furniture.

## 1. What it is

Four fields on `Tree`, mirroring `Meadow`'s wind vocabulary word for word:

| Field | Unit | Default | What it does |
|---|---|---|---|
| `wind` | degrees | `2.5` | How far the outermost twigs lean at full gust. |
| `wind_speed` | m/s | `3.0` | How fast a gust travels across the scene. |
| `wind_direction` | degrees | `0.0` | Which way it blows; `0` toward −Z, the engine's forward. |
| `flutter` | degrees | `9.0` | How far a single leaf beats about its own attachment. |

`Meadow` already names three of those four, with the same meanings and the same
sign conventions, so a scene that authors a breeze authors it once and the two
systems agree about what it is. The fourth has no meadow counterpart because a
blade of grass is the whole plant, while a leaf is a small thing hanging off a
big one and moves on its own account.

Motion is a **pure function of (files, time)**. No physics, no state carried
between steps, no wind velocity field anyone can query: `engine screenshot
--time 4.5` renders the same bytes today and next year. This is the same trade
`Water` and `Meadow` made and it is what keeps a moving tree inside a
`diff-render` baseline.

## 2. Default on, which is a departure

The house rule is that new behaviour defaults to off — M16 added sky, fog,
shadows, MSAA and transparency without re-blessing one of eleven baselines,
because a scene that omits the block renders byte for byte as it did.

This milestone breaks that rule deliberately, on the user's call: a tree that
does not move is *wrong*, not merely plainer, and the fields exist to tune the
wrongness away rather than to opt into the right thing. The cost is bounded and
was paid up front — every committed baseline with a tree in it re-blesses in the
milestone commit, and that is four files.

What survives the departure is the **opt-out**: `wind: 0` with `flutter: 0`
routes the draw onto the pipelines that compile `mesh.wgsl` as it sits on disk,
uploads no sway attribute, and renders the M19 tree byte for byte. That is
asserted, not assumed (`a_windless_tree_takes_the_plain_pipeline`).

## 3. Where the motion is computed

**In the vertex stage, from a per-vertex weight the generator authors.**

The alternatives, and why not:

- **Regenerate the mesh on the CPU each step.** This is where M19 would push it,
  since `tree.rs` already owns the geometry. It is fatal for two reasons that
  compound: `meshes_for` caches on the component's exact field bits and returns a
  shared `Arc`, and the renderer's upload cache keys on that `Arc`'s *identity*
  (M15) — so a tree whose geometry changed every step would mint a new buffer
  and re-upload every vertex of every tree in the scene, every frame. `Meadow`
  faced exactly this and moved its whole life cycle into the vertex stage; a tree
  is the easier version of the same problem, because a tree does not change which
  organs it has.
- **A bone per branch.** The engine has a skinning path (M30) that could carry
  this, and a rigged tree is what an offline pipeline would export. It needs a
  skeleton per tree, a palette upload per tree per frame, and 128-joint budgets
  a `levels: 3` tree blows through. The sway needed here is one smooth field, not
  an articulated pose.
- **Rotate the whole entity.** Free, and instantly readable as wrong: a tree that
  rocks rigidly about its base is a mast, and its trunk moves as much as its
  twigs.

The vertex stage needs one number per vertex that the CPU is in a far better
position to know than the shader is: **how much this point of the tree moves**.
The generator knows it exactly — it knows the recursion depth, where along its
branch a vertex sits, and which branch carries it. A shader would have to guess
it from position, and every guess is wrong somewhere: height alone flaps the top
of the trunk, radius alone flaps the trunk's tapered tip, distance-from-axis
pins a drooping willow's tips exactly where they should move most.

## 4. The sway channel

`MeshData` grows one optional channel, on `joint_indices`/`joint_weights`'
precedent and under their rule — **empty for every mesh that is not foliage**,
so no committed vertex buffer and no committed vertex layout changed when it
arrived:

```rust
/// x = sway weight, y = flutter phase in turns.
pub sway: Vec<[f32; 2]>,
```

**The weight** is a compliance that accumulates down the branch hierarchy. A
branch entering at weight `w0` reaches `w0 + (1 - w0) * share` at its tip, and
interpolates between the two along `t^BEND_CURVE`:

- the trunk runs `0 → TRUNK_SHARE` (0.12), so its foot is pinned and its top
  moves a little, which is what a trunk does;
- every deeper branch runs from wherever its parent had got to at the attachment
  point to `w0 + (1 - w0) * BRANCH_SHARE` (0.55), so each generation gives back
  roughly half the remaining distance to 1 and depth alone makes twigs the
  loosest thing in the tree;
- a leaf takes the weight of its attachment point **uniformly over all of its
  vertices**, so the bend translates it rigidly instead of stretching it.

Continuity at a join is by construction — a child starts at exactly the weight
its parent has where it attaches — which is why the canopy bends as one surface
rather than as a set of pieces sliding against each other.

**The phase** is per leaf and free: `roll / τ`, a quantity `emit_leaves` has
already drawn from the RNG for the leaf's spin about its own midrib. Drawing a
fresh random number would have been the obvious thing and would have **reshaped
every tree in the repo**, because the draw sequence is what a `seed` means here.
Bark carries phase 0; it does not flutter.

## 5. The shader

`foliage.wgsl` is a prelude spliced in by a `foliage_producer()`, at the seam
M27 built for skinning and for its reason: the four lighting lines in
`mesh.wgsl` are ULP-sensitive, and a variant that moves a vertex has no business
near them. The producer sets `VertexContribution::position`, which is the field
skinning uses and which at most one producer may claim — a tree is never
skinned, and the assertion in `vertex_stage` is what says so out loud.

Two terms, in this order:

1. **Bend.** A rotation of the vertex about the tree's local origin, by
   `wind · gust · weight` radians, about the horizontal axis perpendicular to
   `wind_direction`. A rotation and not a translation, because a translation
   stretches a branch as it displaces it. The trunk's foot is at weight 0 and
   therefore exactly fixed.
2. **Flutter.** `flutter_metres · sin(τ · (t · f + phase))` along the vertex's
   own normal, and **leaf draws only** — the bark item uploads a flutter
   amplitude of zero. A leaf beating along its normal is a leaf turning its face
   to the wind, which is the motion the eye actually reads at this distance; a
   translation in a fixed direction reads as jitter.

`gust` is `value_noise` over a coordinate that travels with the wind, the same
two-octave sum `meadow.wgsl` uses, at the same `GUST_SCALE` — so a meadow and
the trees above it gust *together*, which is the entire reason to copy a
constant rather than pick a fresh one.

The flutter frequency is a shader constant scaled by `wind_speed` rather than a
fifth field: a stiffer breeze beats leaves faster, nobody was asking to tune the
two apart, and `Tree` already carries twenty-six fields.

## 6. The shadow pass has to move too

A caster that does not apply the same displacement writes a static tree into the
shadow map, and the mismatch between a moved surface and its own unmoved depth
is self-shadow acne that crawls — a far louder artifact than the motion is a
feature. So the foliage set carries its own two casters, spliced into
`shadow.wgsl` and `shadow_cutout.wgsl` exactly as `skinned_shadow()` splices
them, and **the solid caster grows a normal attribute it does not otherwise
need**, because flutter is along the normal and the two stages must agree to the
bit.

## 7. What is not in it

- **Transparent bark.** The foliage variants cover the opaque and textured
  colour passes and the two casters. A `Tree` whose `Material` is transparent or
  refractive draws through the ordinary blended path and does not sway; leaves
  are opaque by construction (`Tree::leaf_material`), so this can only ever
  affect the bark half. `tree_sway_needs_opaque_bark` is a **warning** rather
  than an error, because the tree is still a correct tree — it is the trap of a
  feature that renders as if absent, converted into a line of output.
- **Wind as a scene-level block.** Tempting, and wrong at this size: `Meadow`
  already authors its own, and a scene-level wind that the two systems read
  would be a fourth cross-cutting block whose absence has to mean something. Two
  components naming the same fields is the cheaper agreement.
- **Anything the wind pushes.** No force on a `RigidBody`, no drag, nothing a
  script can query. The motion is a rendering fact, like a meadow's.
- **Collision.** `SkinnedCollider`'s question — a swaying branch's collider — is
  not asked here. Nothing in this engine collides with a tree's canopy today.
