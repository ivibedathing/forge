# M42 — Terrain basins: authored depressions in the height field

The showcase tour's pond is a sheet of water lying on a field. At its centre the ground is at
−0.24 m and the water plane at −0.04 m, so it is a 20 cm puddle that ends in a straight line where
the `Water` patch's rectangle stops. Nothing holds it in; the ground around it is nowhere higher
than the surface.

That is not an authoring mistake. `Terrain` is pure fBm — `seed`, `feature_scale`, `octaves`,
`persistence`, `warp` — and **the engine has no way to say "the ground dips here."** The m21
fixture gets its "pond in a basin" by building the ground out of four cuboid slabs around a sunken
`PondBed`, which works only because that scene has no `Terrain` at all. On a continuous height
field there is no such trick: lowering the water plane just hides it under the ground.

So water sits on top of the landscape or it does not appear. This milestone adds the one field that
makes a pond possible: a list of circular depressions the height field subtracts.

---

## 1. Scope

| | |
|---|---|
| **B0** | `TerrainBasin` and `Terrain.basins` — floor, wall, depth, in world XZ |
| **B1** | `terrain::height_at` cuts them, so every consumer of the height field follows |
| **B2** | Two validation warnings for the two ways a basin does nothing |
| **B3** | `verify/m42_basins.json` + baseline, and the showcase pond moved into a hollow |

Not in scope, and each named again in §7: mounds, ellipses, per-basin noise, a basin authored on
the `Water` entity, and carving from a `Road`.

---

## 2. The field

```json
{ "type": "Terrain", "seed": 3, "height": 1.2, "feature_scale": 38.0,
  "basins": [
    { "center": [15.0, 6.0], "radius": 4.2, "depth": 1.15, "falloff": 4.6 }
  ] }
```

`radius` is the flat floor. `falloff` is the wall: the metres from the floor's edge out to where
the ground is the untouched fBm again. `depth` is how far the floor drops. A basin's whole
footprint is therefore `radius + falloff`, and the shore — where a water plane meets the ground —
lands somewhere on the wall, chosen by where the plane sits.

```
height_at(x, z) = fbm(x, z) − max over basins of ( depth · w(d) )

        d = |(x, z) − center|
     w(d) = 1                                   d ≤ radius
          = 1 − smoothstep((d − radius)/falloff) radius < d < radius + falloff
          = 0                                   d ≥ radius + falloff
```

`smoothstep(t) = t²(3 − 2t)`, the same one the value noise interpolates with — so the wall meets
the untouched field with a zero derivative and there is no crease at the rim.

**Two numbers rather than one.** A single "radius with a smooth falloff to nothing" gives a dish,
and a dish deep enough to hold water has walls so gentle that the waterline creeps far past where
it was authored. Separating the floor from the wall is what lets an author say "a 4 m pool with a
4.6 m bank" and get it.

### Why world XZ

Everything about `Terrain` is sampled in world XZ — that is what makes two patches sharing a
description meet seamlessly, and what makes moving a patch move it *through* the landscape rather
than dragging its hills along. A basin authored in the patch's local space would break both. It
also means the number an author writes for `center` is the same number they write for the `Water`
entity's `Transform.position`, which is the whole ergonomic point: the two have to agree, so they
should be spelled the same way.

### Why depth is in metres before `Transform.scale.y`

Exactly like `height`, which is documented as the relief at scale 1. `scale.y` multiplies the
relief; a basin is relief, so it scales with it. A basin in post-scale metres would be a second
convention inside one component, and the first thing it would do is make a patch at `scale.y: 2`
disagree with itself about which of its two vertical numbers means metres.

### Why the deepest basin wins, not the sum

Overlapping circles are how anything that is not a circle gets authored here — a lake is three or
four of them, a channel is a line of them. Under a sum, every overlap digs to twice the depth and
the "lake" is a ring of pits. Under `max`, the overlap is the lake's depth and the composition
works. The cost is a gradient discontinuity along the locus where two basins are equally deep: a
faint crease on the wall, under the water in every use this was built for. `max` is also the rule
M22's layers already chose over averaging, for the same reason — a blend of two answers is rarely
either one.

---

## 3. Why this needs no shader, no collider work, and no second implementation

M22's central claim is that terrain has **exactly one height implementation and therefore nothing
to keep in agreement**. This milestone is the first real test of that claim, and it holds: the
basin is subtracted inside `height_at`, and every one of these follows without being told.

| Consumer | Follows because |
|---|---|
| The drawn surface | `build_surface` displaces its grid by `height_at` |
| Its normals | `normal_at` central-differences `height_at`, so the walls light correctly |
| The `trimesh` collider | It *is* the generated surface — a body dropped in a basin lands in it |
| `world.terrain_height` | `world_height_at` |
| `engine terrain-height` | `world_height_at` |
| A `Road` with `follow_terrain` | `world_height_at`, per point per cross-section (M40) |
| A `Meadow` | Places its plants on `height_at` |
| `FootPlant` | Plants against `world_height_at` |

And **no shader is edited**, which matters more than it sounds: the layer painting is per pixel
from the *world height and slope the vertex carries*, so a basin's floor picks up a low-altitude
layer and its wall picks up a steep-slope layer with no new uniform and no new branch. The four
ULP-sensitive lighting lines in `mesh.wgsl` are not in this milestone's blast radius at all.

The one thing that does need saying explicitly is the **surface cache key**. `GridKey` names every
field that changes the geometry; `basins` joins it as a `Vec<[u32; 4]>` of bit patterns, because
two patches differing only in their basins are different ground and must not share an `Arc`.

---

## 4. Determinism: nothing without a basin moves

`height_at` early-returns the untouched fBm when `basins` is empty — not "subtracts a zero", which
would also be numerically identical, but a branch, so that every scene in the repo that predates
this milestone provably takes the M22 code path expression for expression. The A/B between binaries
is the check that settles it (`ab-check`), and it is a real check here rather than a formality:
generated geometry is one of the two things CLAUDE.md names as needing it.

What re-blesses: `showcase_*` (the tour's pond moves into a hollow, and the ground under six
entities changes, which perturbs the whole collider set — the rapier rule), and nothing else.

---

## 5. Validation

Two warnings, because both failures are silent and both are the mistake an agent actually makes:

- **`terrain_basin_no_effect`** — `depth` is 0, or `radius` and `falloff` are both 0. The basin
  cuts nothing. The likely cause is a half-authored entry.
- **`terrain_basin_outside_patch`** — the basin's footprint does not overlap the patch's own XZ
  extent. The likely cause is a `center` written in the patch's local space, which is the natural
  guess and is wrong; the message says so and gives both rectangles.

Neither is an error. A basin that misses is legal — a scene may share one terrain description
across patches, and M22 already promises world-space sampling exactly so that it can.

There is deliberately **no** check that a `Water` patch sits inside a basin. The engine does not
know that a given pond is meant to be in a given hollow, and inventing that relationship would put
a fifth thing in the "recipes own their geometry" story.

---

## 6. The fixture

`examples/scenes/verify/m42_basins.json`: one patch, three basins — a round pool with water in it,
a dry crater with a hard `falloff: 0` wall, and a pair of overlapping circles that read as one
oblong pond, which is the `max` rule doing its job in a picture. A raft floats on the pool and a
boulder is dropped into the dry crater, so the collider follows the ground in the render rather
than only in a test.

The camera aims down into the ground with no horizon in frame and the scene leaves MSAA off, which
is what keeps it in the bit-exact class rather than the tour's — CLAUDE.md's rule for a new fixture
that wants a hard pin.

---

## 7. Deliberately absent

- **Mounds.** A negative `depth` would raise a hill for free, and the field is called `basins`. A
  component whose name is the opposite of what it can do is how documentation stops matching, and
  the fBm already makes hills. If mounds are wanted, they arrive as their own field or as a renamed
  `features` list with a signed height — a rename with a schema regeneration behind it, not a
  quietly relaxed bound.
- **Ellipses and polygons.** `radius` is a scalar. An elliptical basin makes `falloff` ambiguous —
  metres along which axis? — and overlapping circles under `max` cover the shapes this is for.
- **Per-basin noise on the rim.** The wall is a clean iso-circle, and M22 already learned that a
  clean curve reads as artificial. But the fix belongs at the *field* level (jitter the sampled
  distance by the same detail noise the layers use), not as seven more fields per basin, and it
  wants its own render comparison to tune. Left out rather than guessed at.
- **A basin authored on the `Water` entity**, i.e. a pond that digs its own hole. This is the M40
  road-carving question again and gets M40's answer: `Terrain` owns its grid, and a second entity
  writing into it makes the ground a function of which *other* entities exist — so the collider
  under your feet would depend on a component that is not the terrain's. The basin lives on the
  terrain, where the grid does.
- **Road carving**, for the same reason and with the same shape. This milestone does not make it
  easier or harder; it does establish that an authored subtraction inside `height_at` is a workable
  form, which is one of the two things carving would need.

---

## 8. Build order

1. `TerrainBasin`, `Terrain.basins`, schema regeneration.
2. `height_at` and `GridKey`; the unit tests that pin the floor, the wall, `max` over sum, and the
   empty-list early return.
3. The two validation warnings, their codes, `docs/error-codes.md`.
4. `verify/m42_basins.json`, its baseline, its manifest entry, its CLI test.
5. The showcase pond into its hollow; re-bless the tour.
6. `ab-check`, the note, CLAUDE.md.
