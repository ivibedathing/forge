# Material-aware fracture (M43)

*Design doc: `designs/fracture-design.md` — it holds the rejected alternatives. This note holds what
building it taught.*

M14 gave breaking; M43 gave it a material. A `Breakable` now says whether it is `glass`, `wood`,
`stone` or `metal`, a fragment may be a **convex shard** instead of a box, and `engine fracture`
generates those shards offline with a per-material algorithm. Every part of it defaults to M14: a
`Breakable` with no `material` and no `points` reaches none of the new code.

## The three pieces

**`Shard`** — a convex point set that **owns its geometry**, so no `Mesh` (that is
`shard_with_mesh`) but a `Material`, which is `Tree`'s bark exception. The hull is recomputed from
the points at load, flat-shaded, cached by `Arc` like a cloud's mesh. It rides the ordinary mesh
draw list the way a `Tree` does, so shadows, fog, point lights, picking and the editor's selection
came free and **no shader and no pipeline changed**. Physics reaches the same hull through the
`generated` seam `Road` and `Junction` already use, which is what makes the drawn shard and the
collided shard the same solid.

**`Fragment.points`** — exclusive with `mesh` (`fragment_geometry`), and exclusive with
`half_extents` too, because a shard's collider *is* its hull and a half-extent beside it describes a
shape physics never builds.

**`Breakable.material`** — drives what happens *after* the break: fragments scatter away from the
impact point at a per-material speed scaled by how far past the threshold the hit was, tumble in
proportion, and spawn on the material's friction and restitution instead of M14's `0.5 / 0.0`.

## What the renders decided

- **Nothing is thrown downward.** The honest "away from the impact" direction for a crate hit from
  above points *into the floor* for every fragment, because they are all below the contact point.
  The first version drove the whole break into the ground, where the solver pushed it back out. The
  vertical component is floored at `MIN_LIFT` (0.2 of the scatter speed): what a break shows is the
  sideways half, and the downward half is where a real break's energy goes anyway.
- **Glass ≈ 6× stone.** `burst_speed` 3.0 against 0.5 is what makes the fixture legible at a
  glance — slivers across the floor next to a block whose chunks merely parted. A pinned test
  measures the ratio rather than trusting the constants.
- **Debris is the fine tail of the same distribution**, not a second system. Stone and glass
  densify their seeding near the impact, so the cells there are small; that *is* the rubble. There
  is no dust — see the design doc §7 for why an engine-spawned `ParticleEmitter` has nowhere good
  to die.

## The generator

One clipper, four seedings. A cell is the box clipped by the perpendicular bisectors of its seed
against every other seed; its corners are the triple-plane intersections that satisfy every
half-space. Brute force over triples, deliberately — a cell has ≤ 6 + n planes, and an exhaustive
search has no merge order to get subtly wrong. `shard.rs`'s hull is the same argument at the same
scale.

**The tiling property is load-bearing.** Voronoi cells fill the box and do not overlap; fragments
that overlapped at spawn would be interpenetrating rigid bodies, and rapier resolves that by pushing
them apart hard — a crate that explodes on contact instead of breaking. A test measures it directly:
the fragments' volumes sum to the source box within 1%.

**Anisotropy is an affine metric, never a perturbed plane.** Measuring distance in a space scaled
per axis leaves the bisectors exact bisectors *under that metric*, so the tiling survives while the
shapes stretch. Wood squashes distance along the grain by 4× and gets splinters; metal by 2× and
gets plates. Perturbing the plane normals directly — the first instinct for "torn" — breaks the
tiling and is the one thing this module may not do.

## Traps

- **The generator fractures in world metres and stores local ones.** A plank authored the M34 way —
  a `builtin:cube` at `scale: [0.6, 0.18, 2.6]` with unit half-extents — has a **cube** for its
  local box, so a generator reading only the local box finds no grain axis to splinter along and no
  thin axis to shatter through. `engine fracture` multiplies by `Transform.scale` going in and
  divides going out. Nothing about this is visible in a render; it shows up as wood that splinters
  the wrong way.
- **`--write` cannot bootstrap a scene that does not validate**, because the command validates
  first. That is why `--threshold` exists: an entity with no `Breakable` yet has no threshold to
  keep, and an empty `"fragments": []` placeholder is itself a validation error.
- **A doc comment on an enum *variant* blinds the closed-vocabulary check.** `ParticleBlend` carries
  the note; `FractureMaterial` re-learned it. Measured: `"material": "wud"` reported *nothing* until
  the four variant comments moved up into the enum's own doc.
- **An `Option<T>` of a named type publishes `anyOf: [{$ref}, {"type": "null"}]`**, not the flat
  `"type": ["string", "null"]` an optional primitive gets — and the walk read only the flat form,
  so every optional enum field in the engine was waved through unchecked. `optional_variant` in
  `walk.rs` is the fix. The symptom was a bad material reaching serde and coming back as
  `scene_parse_desync`, the code that means "engine bug, not your scene".
- **A generated component is a git-diffability problem the moment it is large.** Fourteen shards
  spliced as one 6,000-character line is a JSON scene format that is no longer diffable
  (invariant 1). `formatter.rs` grew a block form — an array of objects breaks one element per
  line — and `shorten_floats`, because `serde_json` widens an `f32` to `f64` and prints
  `0.12767969071865082` for a number the engine only ever had seven digits of. Both apply only to
  values no pre-M43 caller produces, so every committed splice is byte-identical.

## What the tour's failure was, and was not

Adding one `Shard` and re-fracturing the ice pillar moved a crate fragment into the pond basin M42
had just dug, and `the_showcase_tour_runs_fifteen_deterministic_seconds` failed with "it fell
through the world" at y = −1.67. It had not: `engine terrain-height` says the ground there is at
−1.94, so the fragment was resting on it. The test's floor was a flat `y > -1.0` written when the
lowest ground in the tour was near zero, and M42's basin outran it. The constant now sits below the
deepest ground with a comment saying so — a body that actually loses contact at step 600 is past
−100 m by step 900, so the check has three orders of magnitude of room and needed none of its
precision.

The general lesson is the one CLAUDE.md already states — **a physics scene is not stable under the
addition of a collider anywhere in it** — with a corollary: an assertion tuned to where bodies
*happened* to land is a test that a later milestone's terrain can invalidate without touching
anything it names.

## Verification

`verify/m43_fracture.json`: four slabs, four materials, four dropped weights, rendered at step 55.
Bit-reproducible (three renders, one image) because the camera holds no terrain and the scene
renders at `samples: 1` — so it is pinned by `cli.rs` with no tolerance, like the other 39.

The generator's tests are properties rather than pixels: the cells tile the box, every cell bounds a
volume and stays inside the source, the piece count is exact, the seed reproduces, wood runs along
its grain, and glass spans the pane and breaks finer at the impact.
