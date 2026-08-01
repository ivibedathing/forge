# Trees (M19)

*Relocated verbatim from `CLAUDE.md` when it was compacted to an index. The digest that points here is `CLAUDE.md` §Trees.*

The `Tree` component is a **recipe, not a mesh reference**: `engine-core/src/tree.rs` grows it into
two meshes — bark (drawn with the entity's own `Material`) and leaves (drawn with
`Tree::leaf_material`, from `leaf_color`/`leaf_roughness`) — so one entity emits two `RenderItem`s
under one name, and `unused_material` knows a tree's Material is its bark. A branch is a polyline
that wanders: each of `segments` steps adds a random `crook` and a tube of `sides` faces is swept
along it, tapering on a **power curve** (`t^1.6`, since linear draws a carrot), with a quadratic root
flare over the bottom fifth of the trunk. Children attach past `branch_start`, spun by `branch_twist`
per point (137.5°, the golden angle — a whole-number division stacks branches into visible rows) and
started 70% inside the parent's radius so the tubes interpenetrate instead of needing a union. Ring
orientation is carried by **parallel transport**; rebuilding a perpendicular from a world axis spins
the tube wherever a branch aligns with it. Leaves are `blade` (a midrib with two wings folded down,
emitted twice for both faces — the fold is what gives a canopy texture when the engine has no leaf
textures), `cluster` (a stretched octahedron, for conifer sprays), or `none`.

**Three model rules came out of looking at renders, and all three are now multi-seed tests**:
`whorl` applies to the **trunk only** (compounding it is quadratic — a plausible spruce hit 175,898
vertices — and botanically wrong); `tropism` applies at **depth > 0 only** and clamps against
overshoot (at depth 0 it is unstable: a degree of crook gives a negative tropism something to
amplify, and the first pine grew sideways); and the trunk gives back 30% of its accumulated lean
every segment, because **a random walk with nothing pulling on it drifts**.

Determinism is the M13 discipline again — one private xorshift written out in-repo — except that
jitter helpers always consume a draw even at `jitter: 0`, since no tree baseline predates any tree
field. `tree::vertex_count` is exact (not an estimate) and validation refuses anything over
`MAX_TREE_VERTICES` (100k) with `tree_too_complex` before allocating, because a hung render with no
output is the worst failure an agent loop can hit. `meshes_for` caches on the component's **exact
field bits** (26 words, compared not hashed) and must return the same `Arc` — M15's upload cache keys
on `Arc` identity. There is no species enum: a species is a set of parameters, tabulated in the
design doc.

**Tree baselines are per build profile as well as per adapter** — a release build's `sin_cos`
routines move 3 pixels of `m19_trees.png` by one channel step (measured; Rust does not contract
floats, so this is libm, not FMA), so bless from the debug binary `cargo test` runs. Every pre-tree
fixture is profile-insensitive; the constraint arrives with CPU-generated geometry.
