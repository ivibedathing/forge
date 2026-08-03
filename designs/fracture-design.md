# Material-aware fracture (M43)

M14 gave the engine breaking: a `Breakable` lists pre-authored fragments, and a hard enough hit,
an explosion or a script swaps the entity for them. What it did not give is *material*. Every
breakable in this repo is a box that becomes smaller boxes, because a fragment's collider is a
cuboid and its mesh is whatever primitive the author names — and because nothing in the component
says whether the thing breaking is a pane of glass or a granite block, the two behave identically
once broken.

This milestone makes a break say which material broke. Glass shatters into radial slivers, wood
splits into splinters along its grain, stone breaks into chunky irregular blocks, and metal tears
into a few large plates — and once broken, the pieces scatter *away from the impact* at a speed
and with a spin the material chooses, on friction and restitution the material chooses.

Numbered M43 because a parallel session holds M42.

## 1. What M14 settled, and what this changes

The deleted `breaking-design.md` §1 is explicit, and it is worth quoting because this milestone
does not reverse it:

> Pre-authored fragments (not runtime Voronoi) is the settled decision: fragments exist in the
> text file, validate against the schema, and produce byte-identical runs. **A future
> `engine fracture` CLI could *generate* this JSON offline without changing the runtime.**

That is exactly what M43 builds. The fracture algorithms run in a **command**, never at break
time; what reaches the runtime is still a list of fragments sitting in the scene file, still
validated against the schema, still byte-identical from run to run. `engine fit-colliders` (M39)
is the precedent — a solver an agent runs when it wants the answer, whose output is ordinary
authored data.

What M43 *does* reverse is the narrower one, also §1: **"Cuboid-only fragment colliders are
deliberate v1 scope: shards are boxes to the solver."** A shard is now a convex polyhedron, to the
solver and to the renderer both.

## 2. The three additions

### 2.1 `Fragment.points` — a shard instead of a box

```json
{"type": "Breakable", "material": "stone", "impulse_threshold": 40.0, "fragments": [
  {"points": [[-0.5, -0.5, -0.5], [0.11, -0.5, -0.42], [-0.5, 0.06, -0.5],
              [0.09, 0.14, -0.5], [-0.5, -0.5, 0.22], [0.13, -0.5, 0.18]],
   "density": 2400.0},
  ...
]}
```

`points` is a convex point set in **parent-local metres**, and it is exclusive with `mesh`:
exactly one of the two, enforced by validation (`fragment_geometry`). A `points` fragment ignores
`half_extents` — its collider *is* its geometry — and authoring one alongside `points` is the same
error, because a half-extent that disagrees with the hull is a lie about what physics will see.

`offset`, `rotation` and `scale` still apply, and still mean what they meant: the generator writes
shards already positioned in the parent's frame, so it emits no offset at all, but a hand-authored
shard can be placed like any other fragment.

Points, not faces. The faces are recoverable from the points — the renderer takes the hull, and
rapier takes the hull of the same set — and storing both would be two sources of truth that can
disagree. Storing points alone makes it *impossible* for the drawn shard and the collided shard to
be different shapes.

### 2.2 `Shard` — the component a broken piece carries

A fragment spawned from `points` needs somewhere to keep them, or a baked post-break scene could
not reload into the same world (invariant 2). That is a component:

```json
{"type": "Shard", "points": [[...], ...]}
```

`Shard` **owns its geometry** — the `Water`/`Terrain`/`Cloud`/`Meadow` rule — so an entity with one
carries no `Mesh`, and authoring both is a validation error. It is `Tree`'s exception on materials:
the entity's own `Material` is the shard's surface, because a shard of a painted crate is painted.

A `Shard` is an ordinary authorable component, not a runtime-only artifact. Rubble on the ground is
a pile of them, and that is how the showcase tour gets one without waiting for something to break.

Geometry is the convex hull of `points`, **flat-shaded**: every face gets its own vertices and one
normal. Shards read as shards because their facets are sharp — smoothing them across the hull would
make gravel look like river pebbles.

It rides the ordinary mesh draw list, exactly as `Tree` does. Shadows, fog, MSAA, point lights,
picking, and the editor's selection all come for free, and no new pipeline exists. Physics reaches
the same hull through the `generated` seam `Road` and `Junction` already use, so a `Collider` of
shape `convex_hull` with no `asset` on a `Shard` entity collides with the shape that is drawn.

### 2.3 `Breakable.material` — what the pieces do once they are pieces

```json
{"type": "Breakable", "material": "glass", "impulse_threshold": 6.0, "fragments": [...]}
```

One of `glass`, `wood`, `stone`, `metal`. **Absent is M14 unchanged** — the house rule — so every
existing scene, trace and baseline is untouched by this milestone, and a `Breakable` that never
names a material never reaches a line of new code.

When present it drives three things at break time:

- **Scatter from the impact.** M14's fragments inherit the parent's rigid motion (`v + ω × r`) and
  nothing else, so a crate hit by a truck comes apart in place and then falls. With a material, each
  fragment also takes a velocity along the direction from the **impact point** to its own centroid,
  scaled by how far past the threshold the hit was. Glass throws its slivers hard, stone barely
  moves — a granite block breaks and its chunks *drop*.
- **Spin.** A fragment thrown off-centre tumbles. Angular velocity is proportional to the scatter
  speed and to the fragment's offset from the impact axis, jittered per fragment.
- **Surface.** M14 spawns every fragment with `friction: 0.5, restitution: 0.0`. A material
  supplies its own: glass slides and tinks, stone grips, metal is in between.

The impact point is the contact point for a collision break, the blast centre for an explosion, and
**absent for `world.break_entity`** — a script-forced break has no geometry to scatter from, so it
gets the inherited motion alone. That is a real asymmetry and it is deliberate: inventing an impact
point for a scripted break would put energy into the scene that nothing in the file accounts for.

Density is *not* on the material. It is per fragment, in the file, because the generator already
writes the material's density into every fragment it emits and a hand-edited fragment must be able
to disagree.

## 3. `engine fracture` — the generator

```
engine fracture <scene.json> --entity Name [--material stone] [--pieces N] [--seed S]
                             [--impact x,y,z] [--grain x,y,z] [--write]
```

Prints a `Breakable` component as JSON; `--write` splices it into the scene, replacing the entity's
existing `Breakable` and **preserving its `impulse_threshold`** if it had one. The `--write` shape
is `fit-colliders`': a command whose output is ordinary authored data an agent can read, edit, and
diff.

The volume it fractures is the entity's `Collider` half-extents when it has a cuboid one, else the
AABB of its mesh. A non-box source shape fractures its box — stated plainly rather than hidden,
because a fractured sphere would otherwise silently gain corners. `--impact` defaults to the top
face's centre, which is where a dropped thing gets hit.

### 3.1 One clipper, four seedings

Every material is the same algorithm with a different seed distribution and a different metric:

1. Choose `n` **seed points** in the box.
2. A cell is the set of points closer to its seed than to any other — so each cell is the box
   clipped by the perpendicular bisector of its seed against every other seed. All planes.
3. A cell's vertices are the intersections of every triple of its planes that satisfies all of its
   half-spaces. Brute force over triples: with ≤ 6 + n planes and n ≤ 32 this is a few thousand
   3×3 solves, in a command that runs once.
4. Those vertices *are* the fragment's `points`.

**The tiling property is load-bearing.** Voronoi cells partition the box exactly: they fill it and
they do not overlap. Fragments that overlapped at spawn would be interpenetrating rigid bodies, and
rapier resolves interpenetration by pushing them apart hard — a crate that explodes on contact
instead of breaking. Any future material must keep the property or find another way to guarantee
non-overlap.

**Anisotropy comes from an affine metric**, not from perturbing the planes. Measuring distance in a
space scaled by `m = (mx, my, mz)` turns the bisectors into planes that are still exact bisectors
under that metric, so the cells still tile — while the *shapes* stretch. That single trick is what
separates wood from stone:

| Material | Seeding | Metric | Reads as |
|---|---|---|---|
| `stone` | uniform in the box, densified toward the impact | isotropic | chunky irregular blocks, finer where it was hit |
| `glass` | polar around the impact: angular sectors × geometric rings, no seed variation through the thickness | isotropic | radial slivers and concentric rings spanning the pane, dense at the impact |
| `wood` | uniform in the cross-section perpendicular to the grain, plus a few jittered cross-cuts | stretched hard along the grain | long splinters with ragged ends |
| `metal` | few seeds, biased toward the impact | moderately stretched along the box's longest axis | a handful of large torn plates |

Glass gets no seed variation across the thin axis on purpose: a shattered pane's shards go all the
way through it. Wood's cross-cuts are what stop it from being an infinite bundle of full-length
matchsticks — real wood splits long *and* breaks short, and the ends are where it looks torn.

**Debris is the fine tail of the same distribution**, not a second system. Stone and glass densify
their seeding near the impact, which produces small cells there and large ones away from it, which
is what rubble is. There is no dust — see §7.

### 3.2 Determinism

The generator's xorshift is written out in this repo, seeded from `--seed`, drawn in a fixed order,
for the reason `particles.rs`, `tree.rs` and `cloud.rs` each spell theirs out: the sequence is part
of what the output *means*, and it may not live somewhere a dependency upgrade can reshape it.
Same scene, same flags → the same JSON, byte for byte.

Cell order is seed order, and seed order is the draw order. A fragment's index in the file is
therefore stable across regenerations with the same seed, which keeps `Crate.frag7` the same shard
run to run.

## 4. Validation

- `fragment_geometry` — a fragment with both `mesh` and `points`, or with neither, or with
  `half_extents` alongside `points`.
- `shard_degenerate` — fewer than four points, or points that are coplanar, collinear or
  coincident, on either a `Fragment.points` or a `Shard`. Caught at validate time rather than at
  the hull, because a degenerate shard renders as nothing and collides as nothing, which is the
  hardest possible failure to read off a picture.
- `shard_with_mesh` — a `Shard` entity that also carries a `Mesh`, the recipe rule the other five
  recipes already enforce.
- Point count is capped in the schema (64 per shard), which bounds the hull's cost and keeps a
  generated scene file legible.

`material` needs no validation beyond the enum: every value is meaningful on every `Breakable`,
including one whose fragments are M14 boxes. A wooden crate that breaks into boxes still scatters
like wood.

## 5. What does not change

- A scene with no `material` and no `points` runs byte-identically to before. The contact-point
  capture added to the event sink collects data the solver never reads.
- Fragment naming, ordering, the three triggers, the trace lines, and the bake splice are all M14's
  and are untouched. A `Shard` bakes out as a component like any other.
- No shader changes. No new pipeline. Shards are mesh draws.

## 6. Verification

`examples/scenes/verify/m43_fracture.json`: four slabs — glass, wood, stone, metal — each with a
weight dropped on it, rendered after the impact so the frame shows four different break patterns
side by side. No terrain in frame and `samples: 1`, per the adapter trap: a fixture that wants a
hard pin may not aim at fine geometry against relief under MSAA.

The generator gets unit tests that are properties rather than pixels — cells tile the box (total
volume within epsilon of the source), no two cells overlap, every cell is non-degenerate, the same
seed reproduces the same points — because "the shards look right" is a render's job and "the shards
are a valid partition" is a test's.

The showcase tour gains a `Shard` entity (rubble beside the breaking crates), which the
component-coverage test requires. That adds colliders to the tour, so the tour's frames re-bless:
the collider set is an input to the broad phase and a scene that gains a body is a scene that
re-blesses (the M37 precedent, where embers moved crates at the other end of the arena).

## 7. Rejected, and why

**Runtime fracture at break time.** Even seeded and deterministic, it would mean the scene file no
longer says what a crate breaks into — you would have to run the engine to find out. M14's decision
holds for M14's reason, and the generator being a command is what lets an agent look at the shards,
edit one, and diff the result.

**Generated glTF shard files.** The alternative the user was offered: `engine fracture` writes
`meshes/crate_shard_00.glb` and fragments reference them by path, leaving the runtime untouched. It
loses the diff — a shard becomes an opaque binary — and it puts N files per breakable object next to
the scene. Inline points keep invariant 1 whole.

**Storing hull faces beside the points.** Redundant, and redundancy here means the renderer and
rapier can disagree about the same shard. The hull is computed once and shared.

**Dust and debris particles at the break.** ~~A `ParticleEmitter` spawned by the engine has nowhere
good to die: particle state is derived and never baked, so an expired emitter left in the world
re-puffs when its scene is reloaded, and a scene that breaks twenty crates accumulates twenty dead
emitters in its bake. It is also already authorable — an M37 template plus a script line — so it is
not blocked, just not the engine's. The visual gap is covered by the fine tail of §3.1.~~

**Built in M44** — see `designs/notes/m44-break-dust.md`. The objection was the emitter's *death*,
and the answer was to give it one: `ParticleEmitter.duration` bounds the emission and
`despawn_when_done` takes the entity out once its last particle dies, so nothing accumulates and a
bake taken after the puff contains no trace of it. The residual — a bake taken *mid*-puff reloads
and puffs again — is the same thing the tour's fire already does, and it is the accepted price of
particle state being derived rather than baked.

**Denting metal instead of breaking it.** Plastic deformation is per-step mesh mutation, and this
engine's geometry is a pure function of the file. A metal panel that permanently changed shape
would be hidden state (invariant 2) in the most literal way.

**Shards that break again.** A shard could carry its own `Breakable` and shatter further — glass
does this in reality. Deferred: it needs a depth rule, and every level multiplies the collider set,
which is the one thing this engine's determinism is most sensitive to.

**Per-material `impulse_threshold` defaults.** Tempting — glass is weak, stone is strong — but the
threshold depends on the object's mass and size as much as its material, and a default that is
wrong for the object is worse than a number the author had to think about once.
