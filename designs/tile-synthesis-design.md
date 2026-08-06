# M47 — Tile synthesis: growing a place from a tileset

Every recipe this engine has grows one *thing*. A `Tree` is a tree, a `Cloud` is a cloud, a `Road`
is a ribbon between the points you gave it. There is no recipe that grows an *arrangement* — and
so every built structure in the repo is placed by hand, box by box. The showcase tour's ruins are
eleven cuboids with hand-written positions; the arena's cover is nine more. Adding a twelfth means
writing a `Transform` and eyeballing a render.

That is the authoring cost this engine exists to remove, and it is the one recipe shape M4–M46
never took: **a recipe whose output is a layout rather than a mesh.**

This milestone adds one, following Paul Merrell's model synthesis as
[Boris the Brave describes it](https://www.boristhebrave.com/2021/10/26/model-synthesis-and-modifying-in-blocks/).
A tileset says which tiles may touch which; a constraint solver fills a grid with tiles that agree;
the grid is drawn as one merged surface. Three things make it this engine's version of that
algorithm rather than a port of someone else's:

- **The tiles are grown, not modelled.** A tile is a list of parametric parts — boxes, wedges,
  cylinders — in the same metres and the same `at`/`size` convention as a `Transform`. A tileset is
  therefore *JSON an agent can write from a prompt*, which is the only form in which "generate a
  medieval village" is a thing this engine can be asked for. No `.glb`, no modelling tool, nothing
  outside the text the agent already edits.
- **The solved layout is a committed text file**, not a runtime roll of the dice. It is the thing
  invariant 2 demands, and it is also the thing you *edit* when you want one cottage moved.
- **The solver works in blocks**, which is what makes editing an area possible at all.

---

## 1. Scope

| | |
|---|---|
| **T0** | `tilesets/*.json` — a new asset file kind: a palette, and tiles as parts plus per-face sockets |
| **T1** | Tile geometry grown from parts; adjacency derived from sockets; rotation by expansion |
| **T2** | `engine list-tiles [--sheet]` — the linter and the contact sheet that make T0 authorable |
| **T3** | `layouts/*.tiles.json` — the solved grid as NDJSON, one row per line, `!` for locked |
| **T4** | The solver: AC-4 propagation, min-entropy collapse, **modifying in blocks** |
| **T5** | `TileGrid`, a geometry recipe; `engine synthesize` with `--region`, `--write`, `--check` |
| **T6** | `tilesets/village.json`, `verify/m47_tiles.json` + baseline, the tour entity |

Not in scope, and each named again in §10: adjacency learned from a sample, part kinds beyond the
three, tilesets other than the village, the editable-WFC similarity picker, transparent palettes,
and blocking in Y.

---

## 2. The tileset

```json
{
  "cell": [2.0, 2.5, 2.0],
  "palette": {
    "timber":  { "albedo": [0.32, 0.22, 0.13], "roughness": 0.75 },
    "plaster": { "albedo": [0.80, 0.77, 0.68], "roughness": 0.95 },
    "thatch":  { "asset": "../materials/thatch.json" }
  },
  "tiles": [
    { "name": "wall", "weight": 4, "rotations": 4,
      "faces": { "nx": "wall", "px": "in_s", "pz": "run_s", "nz": "run_s",
                 "py": "wtop", "ny": "0" },
      "parts": [
        { "kind": "box", "at": [0, 0.09, 0],     "size": [2.0, 0.18, 2.0], "material": "timber" },
        { "kind": "box", "at": [-0.85, 1.25, 0], "size": [0.30, 2.50, 2.0], "material": "plaster" }
      ] }
  ]
}
```

Part coordinates are **cell-local: the origin is the cell's centre in X and Z, and its floor in Y.**
`at` is the part's centre and `size` its full extent, which is the `Transform` convention the rest
of the repo uses — an author who can place a cuboid in a scene can place a part.

### Why parts rather than mesh files

The alternative was `"mesh": "meshes/wall.glb"` per tile, and it is what every existing WFC
implementation does. It was rejected on the milestone's own premise: **a prompt cannot produce a
`.glb`.** An agent asked for "a low-poly medieval village" can write a hundred lines of boxes and
wedges; it cannot model. Making the tile a *recipe* is what puts the whole tileset inside the text
medium the agent already works in — the same argument M19 made for `Tree` over a tree mesh, and
M22 for `Terrain` over a height-map image.

Three part kinds ship: `box`, `wedge`, `cylinder`. That set is not a compromise, it is the span of
the fixture's vocabulary — a doorway is two boxes and a lintel, a roof is two wedges, a column is a
cylinder — and each additional kind costs a schema variant, a validation arm, a vertex-count term,
a winding test and a UV convention. `arch`, `stairs` and `prism` are §10.

`Mesh`-backed parts are not refused on principle and would slot in as a fourth kind; they are simply
not what this milestone is for.

### Sockets, and why plain means symmetric

A face carries a socket string. Two tiles may sit face to face when their touching sockets mate:

| Form | Mates with | For |
|---|---|---|
| `"0"` | `"0"` | Nothing here. Reserved |
| `"x"` | `"x"` | Symmetric — an interface that meets another of its own kind |
| `"x_l"` | `"x_r"` | Half of a mirrored pair |
| `"x_r"` | `"x_l"` | The other half |

Naming the interface, rather than deriving adjacency from the geometry by comparing the vertices on
each face, is the first choice here. The derived version sounds better and is worse: it makes every
socket an approximate float comparison, it cannot express "these two *may* touch but rarely
should", and it silently fuses tiles the author meant to keep apart.

The second choice is a deliberate **departure from the DeBroglie/Tessera convention**, where plain
`x` mates `xf` and symmetry is the suffixed case. Symmetric is what an author means almost every
time — two identical walls in a row meet through faces carrying the same string — so it is the form
that gets no suffix here, and the rarer mirrored pair is the one spelled out. Marking *both* halves
is the other half of the departure: `x` mating `xf` while `x` refuses itself is the part of the
original convention that reads as a bug to everyone who meets it. Nothing in this repo imports a
tileset from elsewhere, so there is no compatibility to keep and only ergonomics to gain.

**Vertical sockets carry the rotation index** unless suffixed `_i`. So a `wall` at rotation 1 does
not stack under a `wall` at rotation 2 by default. This over-constrains, deliberately:
over-constraining has a report — `engine list-tiles` prints every socket's partner count and warns
on orphans — while under-constraining renders a second storey rotated off its ground floor and
says nothing at all.

### Rotation is arithmetic, and it has a test

`rotations` is 1, 2 or 4, and the tileset is **expanded** before anything else runs: each tile
becomes that many tiles, in authored order then rotation order. The engine's Euler convention
carries +X to −Z under +90° about Y, so one step of the face permutation is

```
faces_r1.px = faces_r0.pz     faces_r1.nz = faces_r0.px
faces_r1.nx = faces_r0.nz     faces_r1.pz = faces_r0.nx
```

and `py`/`ny` are unmoved but take a new rotation index. This is four lines of code and it is the
single most dangerous four lines in the milestone, because getting it backwards produces a tileset
that **solves cleanly and renders with every wall facing inward**. So it is pinned by a test that
rotates the tile's *geometry* by the same Euler angle and asserts the rotated mesh's face extents
agree with the permuted sockets — geometry and sockets checked against each other, rather than
either against a hand-written table.

---

## 3. The layout is a file, not a roll

A `TileGrid` names a `layout` file, and renders nothing until `engine synthesize` has written one.
That is the same shape as `LightProbeVolume` and `bake-gi` (M35), including `--check`, the staleness
digest, and the "run the command" wording in the missing-file error. Two commands to first pixels
is a real cost and it is paid on purpose. Three alternatives were considered.

**Solve at load time from `seed`.** The scene stays two lines; nothing has to be committed. It is
also the version where **requirement 3 is impossible**: "modify this area" needs a previous layout
to modify, and there isn't one. It additionally makes every load pay for a constraint solve, and it
puts a several-hundred-millisecond backtracking search on the render path where a contradiction has
nowhere to be reported. Rejected on the first point alone.

**Inline the layout in the scene JSON.** This keeps everything in one file, which invariant 1 likes.
It runs straight into the trap CLAUDE.md records from M43: fourteen spliced shards arrived as one
6,000-character line, and `formatter.rs` had to grow per-element array breaking and `shorten_floats`
to keep the scene git-diffable at all. A 1,000-cell grid is two orders of magnitude past that, and
it would sit in the middle of a file an author edits by hand. Rejected.

**A binary layout.** Refused by invariant 1, and not wanted anyway — the whole point of the file is
that an author opens it and changes `floor@0` to `!wall@2`.

So: NDJSON, header line then one line per grid row, cells as one space-separated string.

```
{"format":"forge-tiles-1","entity":"Village","tileset":"tilesets/village.json","tileset_hash":"7f2a1c9d","inputs_hash":"be03d41a","size":[8,2,6],"seed":11,"block":[4,2,4],"overlap":1,"attempts":10,"fallbacks":0}
{"y":0,"z":0,"row":"cobble@0 cobble@0 wall_corner@0 wall@0 wall@0 wall_corner@1 cobble@0 cobble@0"}
{"y":0,"z":1,"row":"cobble@0 post@0 wall@3 floor@0 floor@0 wall@1 cobble@0 cobble@0"}
{"y":0,"z":2,"row":"cobble@0 cobble@0 !wall_door@3 floor@0 floor@0 wall_window@1 cobble@0 post@0"}
```

The format is `gi/mod.rs`'s bake file, line for line, and it is copied rather than reinvented for
the four reasons that file already earns: a per-line `serde_json` parse gives a real line number on
error; the header is an object `jq` reads; the **line order *is* the layout**, checked explicitly,
because a permuted file parses as valid JSON and renders a wrong world and that is the only cheap
moment to catch it; and `to_text` round-trips byte-identically, so a re-solve at the same seed
leaves the file untouched and the diff empty.

**`!` marks a cell locked.** A locked cell is a hard constraint on every solve, is never re-picked,
and comes back byte-identical from a full re-solve. It is how an author says "I want the door
*there*" and keeps it through everything else changing — the smallest possible version of the
editable-WFC idea, costing one character.

`fallbacks` in the header is the number that matters when a tileset is wrong: it counts blocks that
gave up and took the known-good fill. An over-constrained tileset does not fail loudly, it produces
bland output, and this is the field that says so without reading the picture.

---

## 4. The solver

Grid cells index `x + nx*(z + nz*y)` — x fastest, then z, then y — which is exactly the file's row
order, so the file and the array are the same traversal and there is no place to get a transpose
wrong.

**Propagation is AC-4**, not the naive re-scan: `support[cell][tile][dir]` counts how many tiles
remain in the neighbour in direction `dir` that are compatible with `tile`. Removing a tile
decrements its neighbours' counters and a counter hitting zero removes that tile in turn, so the
work is linear in removals rather than quadratic per pass. This is Merrell's choice and it is the
reason model synthesis scales at all.

**The picker is minimum weighted entropy over the block's interior, with ties broken by a per-cell
hash** rather than by a random draw. That is not a micro-optimisation, it is a format contract:
CLAUDE.md's M46 trap is that a generator's random draws are part of what its output *means*, and
a tie-break that drew would make every committed layout depend on how many ties happened to occur.
Collapse spends **exactly one `rng.unit()` per cell**, weighted over the survivors, and nothing else
in the solver draws.

### Modifying in blocks

The naive version — one wave over the whole grid, restart on contradiction — is what WFC does, and
the article's measurement is that it cannot generate 30×30×10 of a rich tileset at all, because the
probability of a contradiction grows with area. Backtracking scales further and then finds
unsolvable rabbit holes. So:

- The grid is covered by blocks of `block` cells, laid out with stride `block − overlap` in X and Z,
  scanned z-outer, x-inner. Y is taken whole (§10).
- A block's **border** is the ring of already-decided cells outside it. It is a hard constraint,
  and it is what makes the block a small, self-contained problem instead of a slice of a large one.
- **Before block 0, every unlocked cell is set to a known-good fill** — `fill_ground` at `y == 0`,
  `fill_background` above. This is the article's prescription and it removes every "the first block
  has no border" special case: block 0's border is the fill, exactly like block 40's.
- A contradiction aborts **that block only** and retries with a fresh sub-stream, up to `attempts`
  (10). After that the block gives up and `fallbacks` increments. The run always terminates and
  always produces a legal grid.

**The fill is checked legal once, up front:** `fill_ground` must mate with itself in X and Z,
`fill_background` with itself and with `fill_ground` beneath it, and both must satisfy the closed
ends. An unchecked fill is an initial state that is already a contradiction, which surfaces as a
solver bug and is a tileset bug.

**A block that gives up reverts rather than filling** — a correction to the article's prescription,
found by building it. Filling a failed block with the known-good arrangement is right when every
border is that same fill, which is true of the article's first pass and false of every block after
it: a later block's border is an *already solved* neighbour, a wall's interior face say, and the
fill is only known good against itself. Writing it in produced an illegal grid whose diff was
nowhere near the block that failed. Reverting is legal by induction instead — the initial state is
the checked fill, and every block that succeeds leaves the grid legal, so whatever stood in the
block before the attempt is an arrangement the tileset allows.

### The RNG is per block

```rust
fn stream(seed: u32, block: u32, attempt: u32) -> Rng
```

Not one global stream. With a global stream, block N's draws depend on how many draws blocks
0..N−1 happened to consume, so re-solving block N alone is impossible — and re-solving one block
alone is the entire point of §5. The generator is xorshift32, written out in `synthesize.rs` for
the reason `fracture.rs`, `tree.rs`, `cloud.rs` and `particles.rs` each write theirs out: the
sequence is part of what the output means and may not live where a dependency upgrade can reshape
it.

### `--region`

`engine synthesize --region x0,z0,x1,z1` keeps the current layout and re-solves only the blocks
whose interior intersects the region, in the same scan order, with borders read from the live
layout. Everything outside those interiors is untouched, byte for byte, and there is a test that
says so — that test *is* requirement 3.

**One property this does not have, and must not be claimed.** A region solve over exactly one block
does *not* reproduce what a full solve produced there. In a full scan that block's east and south
borders were the known-good fill; in a region solve they are already-solved neighbours. Different
constraints, different answer. That is correct behaviour and the honest statement of it belongs in
the note, because the tempting wrong claim — "regenerating a region is a no-op if nothing changed" —
would be found false by the first person who tried it.

---

## 5. Terrain: terracing, and why the offsets reach the solver

`TileGrid.ground` optionally names a `Terrain`. Each XZ column takes a Y offset

```
off[x][z] = round(world_height_at(column centre) / cell.y) * cell.y
```

**Whole cells, not metres.** A continuous offset is the obvious version and it tears the village
apart: two neighbouring columns at 1.31 m and 1.44 m leave a 13 cm slot down every wall between
them, and no tileset can close it because a tile does not know what its neighbour's offset is.
Snapping to whole cells keeps every face flush and reads as a terraced hillside, which is what a
village on a slope looks like anyway.

The offsets are computed **before the solve** — a pure function of the `Terrain`, the `Transform`
and `cell`, so both the solver and the validator can have them — and they are written into the
layout header, because a layout is only meaningful beside the offsets it was solved against.

**A terrace step is a free edge, and this reverses the obvious design.** The first version sheared
the neighbour relation by the difference in lift, so that grid layer 1 of a low column faced layer
0 of the column one step above it:

```
neighbour((x, y, z), +X) = (x+1, y + off[x][z] − off[x+1][z], z)
```

That is *geometrically* right — those two cells really do touch in world space — and it does not
survive contact with a tileset. What they touch across is a **cut face**: the raised column's ground
layer against the lower column's open air. Constraining it obliges every tileset to carry a socket
for the side of a hill, and the village tileset failed on the first sloped grid it saw, with
`ground@0 refuses air@0 across NegX`. Every flat tileset would stop working the moment it followed
ground.

So columns at different lifts simply do not constrain each other, exactly as the patch's own edges
do not. The lift still moves the geometry — that is what terracing *is* — it just stops being an
adjacency relation. What it costs is a building spanning a step, which a terraced village does not
want: buildings sit on one terrace.

`ground` is optional and its absence means flat. That is the house rule — a scene that does not ask
for the feature takes the code path that predates it.

---

## 6. What this does not touch

**No shader, no pipeline, no binding.** A grid's parts are grouped by palette key and emitted as
**one ordinary `RenderItem` per palette material**, carrying a merged mesh and the entity's model
matrix. `Tree` already emits two items for one entity, so nothing new is needed to allow it. A
six-material village is six draws, cheaper than any per-cell alternative, and the four ULP-sensitive
lines in `mesh.wgsl` are not in the blast radius.

The cost of merging is that a **transparent** palette entry would sort as one blob at the entity
origin, because the blended pass sorts per `RenderItem` by model translation. The village's palette
is opaque and §10 says why fixing it is a separate feature.

**One `Arc` per grid, cached.** The renderer keys its uploads on `Arc::as_ptr`, so handing it a
fresh `Arc` each frame re-uploads the entire village every frame. The merged meshes come out of a
thread-local cache keyed on the resolved inputs, exactly as `shard.rs` and `terrain.rs` do — and
`shard.rs` says outright that this is a correctness-of-performance contract rather than an
optimisation.

**Physics gets the same geometry, merged across palettes**, as a `GeneratedSurface` — the road,
junction and shard route. One correction it forces: `FIX_INTERNAL_EDGES` is currently applied to
road-generated trimeshes only, because a body resting on coplanar triangles eventually takes a
contact normal along an edge and is flung sideways (a ball sat on the M23 road for two seconds and
then left at 4.8 m/s). **A tiled floor is the canonical coplanar case** — every slab is flush with
its neighbours — so `TileGrid` opts in beside `Road`, and the boolean is renamed from `from_road`
to say what it means. `Terrain` still does not opt in; the existing comment says it should
eventually, and doing so moves `m22_terrain.png` by 1339 pixels, which is its own change.

---

## 7. Validation

The tileset validates as a file kind of its own, recognised by shape — a top-level `tiles` and no
`entities`/`tracks` — the way a material file already is. Structural errors are the ordinary
schema-shaped ones. Two analyses are **warnings**, because both describe a tileset that works and
disappoints:

- **`tile_socket_orphaned`** — a socket no other tile carries the mate of. The tile can never be
  placed. This is the single most common thing an author gets wrong, and reporting it is what makes
  the format writable from a prompt at all.
- **`tile_layout_forced`** — a *locked* cell that violates adjacency. The author asserted it; the
  engine draws it and says so.

And one distinction worth its own code: an **unlocked** cell violating adjacency is
`tile_layout_illegal`, an **error**. It means the file was hand-edited into an illegal state or the
solver is broken, and those are worth separating from "the author pinned something odd". That split
is also what lets `engine list-tiles --sheet` — which lays every tile out in a row with everything
locked — pass validation without a special case.

`tile_layout_stale` stays **out of `validate`** and belongs to `synthesize --check`, for the reason
`gi_bake_stale` does: the digest needs the whole tileset read and expanded, which is a cost
`validate` should not pay on every file. A repo-contract test runs the check across every committed
layout, which is where staleness actually gets caught.

---

## 8. Determinism

A layout is a pure function of (tileset bytes, `size`, `seed`, locked cells, column offsets, block
params). The header carries a digest over exactly that tuple and `--check` recomputes it.

What re-blesses in this milestone: the six `showcase_*` frames, because a new component forces an
entity into the tour and there is no allowlist. **The tour's `TileGrid` deliberately carries no
`Collider`** — an entity with neither body nor collider is skipped entirely by the physics build, so
the collider set is unchanged, both golden traces survive, and the tour's `simulate` assertions
survive. That matters because of the rapier rule CLAUDE.md records twice: one 5 cm static sphere
200 m from anything moved six bodies by up to 4.4 mm, and M37's embers moved the breaking crates at
the other end of the arena. The collider is exercised in the fixture, where a re-bless is free.

Nothing else should move, and the check that settles it is the A/B between binaries, not a baseline
diff.

---

## 9. The fixture

`examples/scenes/verify/m47_tiles.json` — an 8×2×6 village on a sloping `Terrain`, at `samples: 1`.
MSAA is off because CLAUDE.md is explicit that fine geometry against relief is not bit-reproducible
on this adapter under it, and a tiled village on a hillside is precisely that case. The frame is
`md5`'d five times before it is pinned; if it is not stable it takes a manifest tolerance and the
test says so where the pin would have gone.

What the picture asserts, item by item:

- a **solved** cottage beside a **locked** one, so a solver change moves one and not the other and
  the diff is diagnostic rather than uniform;
- all three part kinds — box walls, wedge roofs, cylinder posts;
- `wall_corner` at **all four rotations** around one cottage, because a reversed face permutation
  renders the walls facing inward and is unmissable;
- a **terrace step** where the ground falls a whole cell, which is §5 in a picture;
- a sphere at rest on a tiled floor, which is the `FIX_INTERNAL_EDGES` assertion — without the flag
  it does not sit there.

What a picture cannot say goes to unit tests: the rotation permutation against a Euler-rotated mesh,
socket mating symmetry, AC-4 reaching a fixpoint, every solved cell satisfying all six neighbours,
the same seed reproducing the same layout, a region solve touching only its blocks, and an
over-constrained tileset falling back rather than hanging.

---

## 10. Deliberately absent

- **Adjacency learned from a sample grid.** Model synthesis proper infers adjacency from an example
  arrangement, and it is the half of the algorithm this milestone does not build. The reason is the
  premise: authored sockets are what an agent can write from a prompt, while learning needs a
  *second* authoring format for the sample — and the sample would have to be built from tiles that
  already exist, so it cannot be the way a tileset is first created. It is a good later addition for
  tuning weights from a hand-arranged example; it is not the way in.
- **The editable-WFC dirty-cell picker and similarity heuristic.** A second mechanism aimed at the
  requirement `--region` already meets. It minimises churn *within* a re-solved block, and it needs
  possibility sets carried across invocations — a machine of its own. Worth building when someone
  complains that a region re-solve changed more than they wanted.
- **`arch`, `stairs`, `prism`.** Compound shapes. Each is a schema variant, a validation arm, a
  vertex-count term, a winding test and a UV convention, and box+wedge+cylinder spans the fixture.
  They arrive as new part kinds, which is an additive change, not a format change.
- **Dungeon, street and forest tilesets.** Authoring work once the format and the linter exist,
  which is the point of shipping the linter. A forest additionally wants tiles that place real
  `Tree` recipes rather than cone-shaped parts — i.e. tiles that place *props*, which is an M37
  templates question and a genuinely different feature.
- **Transparent palette materials.** The merge sorts as one blob (§6). Fixing it means splitting the
  merge per cell for transparent palettes only, which is a real feature with a real answer owed.
- **Blocking in Y.** Blocks subdivide X and Z; Y is whole. Built structures are one to four layers
  and no fixture needs a third stride. A cave system would.
- **A `TileGrid` in an M37 `template`.** Spawning one means resolving files at runtime, which
  entangles with the still-open hot-reload decision in the design doc's §9. Out until that is
  answered.
- **Carving the terrain to meet the grid.** The grid terraces to the ground; the ground does not
  flatten for the grid. That is M40's road-carving question again and takes M40's answer: `Terrain`
  owns its grid, and a second recipe writing into it makes the ground a function of which other
  entities exist.

---

## 11. Build order

1. `tileset.rs` — the format, expansion, sockets, adjacency, part meshes. Unit tests for the
   rotation permutation first, because everything downstream trusts it.
2. `validate/tileset.rs` and the file-kind route; `engine list-tiles`, with `--sheet` before the
   solver exists, so the village can be authored while §4 is being written.
3. `tilelayout.rs` — parse, `to_text`, the row-order check, `verify_adjacency`.
4. `synthesize.rs` — AC-4, the picker, the block scan, the fallback. Headless, hardest-tested.
5. `TileGrid`, `tilegrid.rs`, the resolve pass, the render items, the physics surface, validation,
   error codes, schema regeneration.
6. `engine synthesize`, `--region`, `--check`.
7. `tilesets/village.json`; `verify/m47_tiles.json`, its layout, its baseline, its manifest entry,
   its CLI tests.
8. The tour entity; re-bless the six frames.
9. `ab-check`, `designs/notes/m47-tile-synthesis.md`, CLAUDE.md.
