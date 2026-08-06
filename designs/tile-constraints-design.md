# M49 — Tile constraints: the properties adjacency cannot state

M47 generates villages. It does not generate *buildings*, and the numbers say so
more clearly than any render:

| | ground cells | built | built regions | open regions | floors |
|---|---|---|---|---|---|
| `verify/m47_tiles.json` | 80 | 60 | **one region of 60** | 19, **and an orphan of 1** | 19 |
| the tour's `Hamlet` | 42 | 24 | 23, **and a lone cell of 1** | 16, **and two orphans of 1** | **0** |

Three defects, each measured rather than judged:

- **The village is one structure.** Sixty of eighty ground cells form a single
  4-connected mass — 75% of the grid, walls running into walls without ever
  closing. It reads as a fortress wall, not as a row of cottages.
- **The hamlet encloses nothing.** Twenty-four wall and corner pieces, **zero**
  `floor` cells. Every one of them is a wall with no room behind it.
- **Both strand cells.** A one-cell courtyard walled in on all four sides; a
  single wall standing on its own in the open.

None of this is a tuning failure. I retuned the village tileset's weights four
times during M47 and the mass got denser, not more building-shaped.

---

## 1. Why sockets cannot fix this

A socket is a statement about **one interface**. The properties above are
statements about a **region** — how far a mass of walls extends, whether a run
closes, whether anything is inside it. Constraint propagation over face
adjacency has no representation for either.

The village's own tileset shows where it runs out. Two `wall` tiles back to
back, facing away from each other, is a legal 2-cell "building" with no interior:

```
wall@0 : px=wallrun  nx=wallrun  pz=in   nz=out
wall@2 : px=wallrun  nx=wallrun  pz=out  nz=in     (the same tile, turned twice)
```

`wall@0`'s `pz` is `in`, and `wall@2`'s `nz` is `in`. Both are symmetric, so
they mate — a wall's interior side is satisfied by *another wall's interior
side*. There are four such interfaces in the committed layout.

The obvious repair is to make the interior socket a mirrored pair, so a wall's
inward face demands a floor's outward face rather than another wall's. It does
not work, and the reason is worth writing down because it is the general shape
of the problem: **a `floor` must mate both walls and other floors**, and a face
carries one socket. Give `floor` an `in_r` and walls an `in_l` and floors stop
tiling against each other. Give everything `in` and the degenerate case returns.
There is no assignment of one socket per face that separates them.

`corner` is worse: its four faces are `wallrun`, `wallrun`, `out`, `out`. It
never faces an interior at all, so nothing in the vocabulary can make a corner
imply a room.

---

## 2. Scope

| | |
|---|---|
| **C0** | A `constraints` array in the tileset: one shape, four predicates, over a named set of tiles |
| **C1** | Evaluation over the ground layer's 4-connected regions |
| **C2** | Post-solve rejection driving M47's existing per-block retry |
| **C3** | Validation of the constraints themselves, and a report saying which one rejected what |
| **C4** | The village tileset constrained, both committed layouts re-solved, the fixture and tour re-blessed |

Not in scope, and each argued in §7: constraints inside propagation, hierarchical
generation, constraints on the upper layers, and per-grid overrides.

---

## 3. The shape

One type, several optional predicates, over a set of tiles named by their
**authored** names — four rotations of a wall are one kind of wall to whoever is
writing this.

```json
"constraints": [
  {
    "name": "a building is a building",
    "tiles": ["wall", "wall_door", "wall_window", "corner", "floor"],
    "region_size": { "min": 6, "max": 22 },
    "region_contains": { "tiles": ["floor"], "min": 1 }
  },
  {
    "name": "the street reaches everywhere",
    "tiles": ["cobble", "post"],
    "regions": { "max": 1 }
  },
  {
    "name": "a roofscape, not a chimney farm",
    "tiles": ["chimney"],
    "count": { "max": 3 }
  }
]
```

| Predicate | Reads | Fixes |
|---|---|---|
| `count` | how many cells of this set exist | runaway or absent tiles |
| `regions` | how many connected regions they form | orphaned pockets; `max: 1` is "connected" |
| `region_size` | the size of each region | **the 60-cell mass**, and the lone standing wall |
| `region_contains` | what each region must hold | **the hamlet's zero floors** |

Each is `{min, max}` with both optional. `regions: {max: 1}` *is* the
connectivity constraint the milestone is named for; it does not need its own
kind, and having one shape means one schema type, one validation arm and one
evaluator.

**Why the authored name and not the expansion.** A constraint that had to list
`wall@0, wall@1, wall@2, wall@3` would be four times as long and would silently
stop covering a tile whose `rotations` changed.

### Where they live

**In the tileset**, beside the tiles. A constraint here is a statement about
what this *vocabulary* is for — "these pieces make cottages, not curtain walls"
— and it should travel with the tileset to every scene that names it, exactly as
the sockets do. Per-grid overrides are §7.

---

## 4. How they are enforced: rejection, not propagation

**A block that violates a constraint is re-rolled.** M47 already solves each
block up to `attempts` times with a fresh sub-stream, and already reverts a
block that never succeeds. Constraints hook into that: after a block's wave
collapses, evaluate; on a violation, treat it exactly as a contradiction.

This is the whole mechanism, and it is chosen over checking constraints *inside*
propagation for one reason: an in-propagation connectivity check has to decide,
for every candidate tile in every cell, whether choosing it could disconnect a
region that is still half-undecided. That is a real algorithm (DeBroglie has
one) and it is a milestone by itself. Rejection is twenty lines against
machinery that exists.

### Rejection is *do no harm*, not *be perfect*

Written first as strict rejection — any violation re-rolls the block — and that
does not converge. Measured: **every block failed every attempt**, at every
block size from 3×3 to 8×8 and at 20 and 60 attempts, all 380 of them.

The reason is the blame rule below meeting an already-violating layout. A grid
whose walls form one 60-cell mass has that violation blamed on *every* block
that touches it, and no block can fix it because most of the mass lies outside
anything it can change. Strict rejection therefore rejects for ever.

So a block is asked not to **increase** the violations blamed on it. Starting
from the known-good fill there are none, so a fresh solve is exactly strict; and
a re-solve of a broken layout can improve or hold rather than falling back to
nothing.

The corollary is the flag: **an existing violating layout is never repaired**,
only kept from worsening, so adding constraints to a tileset changes nothing
until `synthesize --reset` throws the old layout away and solves from the fill
with the locks still in it. Without that, the feature silently does nothing to
the very layouts it was built for.

**What it costs** is rejection sampling's usual problem: if a constraint is
rarely satisfied by chance, every block exhausts its attempts and the grid
degrades. Measured on the village: about **thirty attempts a block**, and a
fully clean solve on roughly one seed in eight — which is why the default
budget moves from M47's ten to sixty when a tileset carries rules. Three things
make the rest safe rather than silent:

1. A failed block **reverts** (M47's rule), so the result is still a legal grid
   rather than garbage.
2. `fallbacks` already counts it, and this milestone adds *which constraint*
   rejected how often — so "the village came out bland" has a named cause.
3. `attempts` is authored, and a constrained tileset can raise it.

If the measured hit rate turns out to be bad, the escape is the cheap half of
in-propagation checking — a `count` maximum can ban a tile the moment the cap is
reached — which is a later addition, not a redesign.

### A block is only blamed for what it touched

Regions do not respect block boundaries. A mass that straddles the border
between block 3 and block 4 would fail block 4 forever, because most of it is
outside anything block 4 can change.

So: constraints are evaluated over **the whole grid** as it would stand if the
block were accepted, but a violation only counts when the offending region
**intersects the block's own interior**. A block is judged on the consequences
of its own choices and not on its neighbours'.

---

## 5. What is evaluated, and over what

The **ground layer only** (`y == 0`), 4-connected in XZ.

Buildings, streets and courtyards are all ground-plane properties. Extending
regions through the upper layers would make a roof part of its building's
region, which changes every size bound into a function of how many storeys the
grid has — and the grid's storeys are a scene decision, while the constraint is
a tileset one. §7 keeps the door open.

A terrace step does not break a region. The lift moves geometry, and two columns
at different lifts are still neighbours on the ground plan; making a step split
a building would mean a village on a slope could never satisfy a size bound.

---

## 6. Determinism

A constraint changes **which** solutions are accepted, never the order they are
generated in. A block's `n`th attempt draws exactly the numbers it drew before;
rejection only decides whether that attempt is kept. So a tileset with no
`constraints` array is byte-for-byte the M47 solver, and every committed layout
that predates this milestone re-solves unchanged — checked, not assumed.

What re-blesses: `m47_tiles.png` and the tour frames, because the two committed
layouts are re-solved *with* constraints and the villages change. That is the
milestone's visible output, and the region census is how it is measured rather
than judged.

---

## 7. Deliberately absent

- **Constraints inside propagation.** The real version, and a milestone of its
  own (§4). Rejection reuses machinery that exists and its failure mode is
  reported rather than silent. Worth building when the measured retry rate says
  rejection is the bottleneck.
- **Hierarchical generation** — pick building footprints on a coarse grid, then
  fill each. It is how you get *composed* towns rather than plausible ones, and
  it is a different generator, not a constraint. This milestone is what makes
  its absence survivable.
- **Constraints on the upper layers** (§5), and 3D regions.
- **Per-grid overrides.** A `TileGrid` cannot currently relax its tileset's
  constraints. It should eventually — an author wanting one enormous keep out of
  the village vocabulary has no way to say so — but the field belongs beside the
  answer to whether it merges with or replaces the tileset's list, and there is
  no second consumer yet to answer it against.
- **Repair instead of rejection.** Nudging a failed block toward legality is
  cleverer, is not deterministic in any obvious way, and would need its own
  argument about which cell to change.

---

## 8. Build order

1. `Constraint` in `tileset.rs`, its schema, and validation of its own bounds.
2. `constraints.rs`: region labelling and the four predicates, unit-tested
   against hand-built grids.
3. The solver hook, and `fallbacks` gaining a per-constraint breakdown.
4. The village tileset's constraints, authored against the three measured
   defects, then re-solved and **looked at**.
5. Re-bless; re-run the region census and put the before/after in the note.
6. Tests, `docs/`, `CLAUDE.md`.
