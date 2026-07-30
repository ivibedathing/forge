# Trees (M19)

The showcase tour's forest was twelve entities: six `builtin:cylinder` trunks
with six `builtin:sphere` crowns parked on top of them. It read as a diagram of
a tree rather than a tree, and worse, all six read as the *same* diagram — the
only thing separating one from the next was its `Transform`.

This milestone replaces that with a `Tree` component: a recipe the engine grows
into geometry, seeded so that every instance of one species is a different
individual, and deterministic so that a forest can still sit under a
`diff-render` baseline.

## 1. Why the old trees did not read as trees

Four reasons, and none of them is polygon count:

1. **No taper.** A cylinder is the same thickness at the crown as at the
   ground. Every real trunk sheds radius as it sheds branches, and the eye
   reads a constant-radius vertical as a pole, a pipe, or a lamppost — never as
   wood.
2. **No curve.** A trunk that is exactly straight and exactly vertical is a
   manufactured object. Real ones wander and come back.
3. **No branching.** The silhouette of a tree *is* its branching. A sphere on a
   stick has the outline of a lollipop, and no amount of foliage color fixes an
   outline.
4. **No root flare.** Where a trunk meets the ground it swells into a buttress.
   Without it the tree looks placed on the ground rather than grown out of it —
   this is the cheapest of the four to fix and one of the most visible.

So the component is built out of exactly those four things, plus one dial
(`jitter`) that makes every instance different.

## 2. The model

A branch is a **polyline that wanders**, and a tube swept along it.

```
for each of `segments` steps:
    rotate the growth direction by a random crook       (± `crook` deg/m)
    if trunk:  rotate a fixed fraction of the lean back toward +Y
    if branch: rotate toward ±Y by `tropism` deg/m, never overshooting
    step forward
sweep a tube of `sides` faces along the result, tapering base → tip
attach `branches` children past `branch_start`, spun by `branch_twist` each
recurse `levels` deep; the outermost generation carries leaves
```

Four details in there are load-bearing:

- **Taper is a power curve**, not a straight line: `lerp(r, r·taper,
  t^1.6)`. A trunk keeps its thickness through the bare part and thins fast
  once it is branching. Interpolating linearly draws a carrot.
- **Parallel transport**, not a world-space up vector, carries the ring
  orientation from one node to the next. Rebuilding a perpendicular from a
  fixed axis makes the tube spin wherever the branch aligns with that axis.
- **`branch_twist` defaults to 137.5°**, the golden angle — which is what real
  phyllotaxis converges on, and which is why a whole-number division (say 90°)
  looks artificial: successive branches stack into visible rows.
- **Children start just inside the parent's surface** (70% of its radius). The
  tubes interpenetrate. That is invisible from outside and far cheaper than a
  real CSG union, and it is why no join has a seam or a gap.

This is deliberately not Weber–Penn. There are no per-level parameter arrays,
no splits, no pruning envelope, no bark ridges. What is here is the four things
in §1 plus randomness — the smallest model that produces something you would
call a tree.

### Leaves

Two shapes, chosen by `leaf`:

| `leaf` | Geometry | For |
|---|---|---|
| `"blade"` (default) | midrib + two wings folded down along it, doubled | broadleaves, scrub |
| `"cluster"` | octahedron with radial normals, stretched along the shoot | conifer sprays, distant trees |
| `"none"` | nothing | dead snags, winter, authoring diagrams |

**The fold is the whole point of the blade.** A flat card lit by one sun is one
value of green, and a canopy of flat cards flickers between "all lit" and "all
dark" as the camera moves. Two wings at a dihedral catch the light at different
angles, so the canopy gets texture out of shading alone — which matters here
because the engine has no alpha-cut leaf textures to get it from. Blades are
flat-shaded and emitted **twice with opposite winding**: backface culling is on,
and a leaf has two sides.

Leaves are their own mesh with their own material (`Tree::leaf_material`, built
from `leaf_color` / `leaf_roughness`), because bark and foliage cannot share
one. The entity's `Material` is the bark — which is also why `unused_material`
does not fire on a tree that has no `Mesh`.

## 3. Three rules discovered by looking at renders

Each of these came out of a PNG that was wrong in a way the unit tests were
happy with, and each is now pinned by a test that would have caught it.

### Whorls are a trunk property

`whorl` puts *n* branches at the same height instead of one — a spruce's ring of
limbs. Applying it at every level is both botanically wrong (the shoots on those
limbs are ordinary alternate ones) and quadratic: `whorl: 5` at three levels is
5 children per node, then 25, then 125. The first conifer authored against the
old rule came to 175,898 vertices and tripped `tree_too_complex`, which is how
the design gap was found. Now `whorl` applies at depth 0 only.
*(`whorls_are_a_trunk_property`)*

### Tropism is a branch behaviour

`tropism` bends growth toward the sky (positive) or lets gravity pull it down
(negative). Applied to the trunk it is **unstable**: one degree of crook tips
the trunk off vertical, a negative tropism bends it further off, and the error
compounds every segment. The first pine grew short and sideways. Tropism now
applies at `depth > 0` only, rotates toward `sign(tropism)·Y`, and clamps
against overshoot so a branch already pointing at the target stops rather than
swinging past it. *(`a_drooping_tropism_does_not_topple_the_trunk` — which also
checks the branches still droop, so the fix did not cost the feature.)*

### A random walk needs something pulling on it

Even without tropism, `crook` on the trunk is a random walk, and a random walk
drifts. At `crook: 18` a six-meter trunk could end up growing sideways, and
which seeds did that was pure luck — the difference between a usable seed and a
broken one was invisible in the parameters. Real trunks wander *around* vertical
because gravitropism keeps returning them to it, so the trunk now gives back a
fixed fraction (30%) of whatever lean it has accumulated, every segment. That
bounds the wander without straightening it, and it is what makes one seed as
good as another. *(`the_trunk_stays_near_vertical_however_gnarled`, swept over
40 seeds at `crook: 25`.)*

The general lesson: **every random walk in the generator needs a restoring
term**, or its quality is a lottery the author cannot see they are playing.

## 4. Determinism

One private xorshift32, seeded from `Tree::seed` through the same splitmix
finalizer the particle system uses, drives every draw — in a fixed order: a
branch draws its own segment wander, recurses into each child in index order,
then scatters its leaves if it is outermost.

The generator and its hash are **written out in this repo**, duplicated from
`particles.rs` rather than shared with it, for the reason M13 established: the
sequence is part of what a scene file *means*, so it may not live in a
dependency where an upgrade could change what a forest looks like.

Unlike the particle emitter, the jitter helpers always consume exactly one draw
even at `jitter: 0`. M13/M17 had the opposite rule (skip the draw, don't
default it) because twelve particle baselines predated those fields. No tree
baseline predates any tree field, so the simpler contract — draw sequence
independent of parameter values — is the one worth holding here.

### The one limit: baselines are per build profile, not just per adapter

The RNG is exact. The geometry it drives is not *bit*-identical across
optimisation levels: a debug build and a release build of the same commit grow
trees whose vertices differ in the last place, which reaches the frame as **3
pixels of `m19_trees.png` at one channel step**, and 1 pixel of
`showcase_90.png`. Measured, not assumed — `tree::generate` hashed in both
profiles diverges for a jittered oak and agrees exactly for the `Diagram` tree,
which is the one that turns the randomness off.

Rust does not contract or reassociate float arithmetic, so this is not the
usual FMA story. What is left is the transcendentals: a branch's every rotation
is `Quat::from_axis_angle`, which is `sin_cos`, and the optimiser is free to
reach a different (still correctly-rounded-ish) libm routine than the debug
build does. No amount of care in this module fixes that short of writing `sin`
and `cos` out in-repo the way the RNG is written out, which is not worth it for
one ULP.

So the committed baselines are blessed from the **debug** binary — the profile
`cargo test` runs, so the pinned CLI test is exact. A release build checking
them by hand sees the handful of pixels above. Every fixture that predates
trees is profile-insensitive and unaffected; this constraint arrives with
CPU-generated geometry and belongs to it.

## 5. Cost, and the budget error

Branching is exponential, so a plausible-looking edit can ask for a billion
vertices: `levels: 4, branches: 12` is 22,621 branches before leaves.
`tree::vertex_count` computes the exact total from the parameters alone —
exact, not an estimate, so the error can name a real number — and validation
refuses anything over `MAX_TREE_VERTICES` (100,000) with `tree_too_complex`
before a single allocation. A hung render with no output is the worst failure
mode an agent loop can hit; this makes it a located error instead.

The other new code, `tree_with_mesh`, is the same reasoning applied to
authoring: a `Tree` **is** the entity's geometry, so a `Mesh` beside it would be
a second opinion about what the entity looks like.

## 6. Caching, and why it is not hidden state

`tree::meshes_for` caches generated geometry keyed on the component's **exact
field bits** (26 words, compared not hashed — a hash collision would silently
draw the wrong tree). Two entities with identical parameters share one mesh and
one GPU upload; one changed field is a different tree.

Sharing the `Arc` is not just an allocation saved. `MeshSource`'s M15 contract
and the renderer's per-frame upload cache both key on `Arc` **identity**, so
handing back a fresh copy each frame would re-upload every tree in the scene,
every frame.

The cache clears wholesale at 256 entries. Animating a tree parameter mints a
new key every step, so the bound is what keeps it from being an incidental leak.
It is not hidden state in the sense invariant 2 forbids, any more than
`mesh.rs`'s builtin cache is: generated geometry is a pure function of the
component, so nothing in the cache can differ from what the file says.

## 7. Species recipes

There is no species enum — a species is a set of parameters, and putting them in
Rust would mean a new tree needs a rebuild. These are the four that the fixture
and the showcase forest are built from; copy one and change the seed.

| | Broadleaf | Conifer | Dead snag | Scrub |
|---|---|---|---|---|
| `levels` | 3 | 1 | 3 | 2 |
| `branches` | 5 | 13 | 3 | 5 |
| `whorl` | 1 | 5 | 1 | 1 |
| `branch_angle` | 58 | 95 | 46 | 62 |
| `branch_start` | 0.30 | 0.08 | 0.34 | 0.10 |
| `length_ratio` | 0.66 | 0.30 | 0.58 | 0.70 |
| `length_falloff` | 0.30 | 0.88 | 0.35 | 0.35 |
| `tropism` | 16 | −16 | 14 | 20 |
| `crook` | 11 | 6 | 13 | 14 |
| `taper` | 0.10 | 0.08 | 0.05 | 0.12 |
| `leaf` | blade | cluster | none | blade |

What each of the three shape parameters is doing:

- **The conifer** is one level of many whorled limbs starting near the ground,
  angled past horizontal (95°) and drooping (`tropism: -16`), with a steep
  `length_falloff` so the limbs shorten toward the top. That falloff is what
  draws the cone; nothing about the model knows what a cone is.
- **The dead snag** is the broadleaf with `leaf: "none"`, fewer and thinner
  branches, and more crook. Bare structure is unforgiving, which makes it the
  best thing to look at when tuning the woody parameters.
- **The scrub** is a one-meter tree. Everything scales, so a bush is not a
  separate system.

`verify/m18_trees.json` carries all four plus a `Diagram` tree with `jitter`,
`crook`, `tropism` and `flare` all zeroed — the authoring reference, where the
seed stops mattering and each parameter's effect can be seen on its own.

## 8. What this is not

- **No bark texture, no leaf texture.** The engine has neither; the fold in the
  blade exists precisely to work without them.
- **No wind.** Every parameter is animatable through `AnimationPlayer` and the
  geometry regenerates, but that regenerates the whole tree per frame — fine for
  a growth shot, not a way to move leaves.
- **No collision.** A `Tree` has no `Collider`; the geometry is not a
  `MeshSource` asset, so `Collider.asset` cannot reach it. A tree that should be
  solid gets a capsule authored beside it.
- **No LOD.** A forest of them is a forest of full-detail meshes. `sides`,
  `segments` and `leaves_per_branch` are the manual dial.
- **No splits, no pruning envelope, no per-level parameter arrays.** The
  Weber–Penn features that were left out, in the order they would be worth
  adding back.
