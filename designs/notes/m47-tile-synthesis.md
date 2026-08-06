# Tile synthesis (M47)

*Design doc: `designs/tile-synthesis-design.md` — it holds the rejected alternatives. This note
holds what building it taught.*

The first recipe whose output is an **arrangement** rather than a mesh. A tileset says which tiles
may touch which, a constraint solver fills a grid with tiles that agree, and the grid draws as one
merged surface per palette material.

## The four pieces

| Piece | Where | What it is |
|---|---|---|
| The tileset | `tileset.rs`, `validate/tileset.rs` | A palette and a list of tiles, each a handful of parametric parts plus a socket per face. Rotation expansion, socket mating, the adjacency bitsets, and the three primitives. |
| The layout | `tilelayout.rs` | The solved grid as NDJSON, and `Grid`, which is the only place the grid's shape is interpreted. |
| The solver | `synthesize.rs` | AC-4 propagation, min-weighted-entropy collapse, the overlapping block scan. No scene, no files, no GPU. |
| The component | `components.rs`, `tilegrid.rs`, `scene.rs` | `TileGrid`, its merged geometry, and the resolve pass that grows it at load. |

Plus `engine synthesize` and `engine list-tiles`, and `examples/tilesets/village.json`.

## What the renders decided

**Four passes over the tileset, none of them driven by the schema.** The palette was too bright to
read at this exposure; the ground slabs floated over the terrace steps until they grew plinths;
`corner` outweighed `wall` and produced zigzag chains that enclosed nothing; and the first camera
was so high the frame was entirely roof. None of that is visible in a test — `cargo test` was green
through all four — and all of it is visible in the first screenshot.

**The terrace lift rounds up, not to nearest.** This is the note's sharpest lesson. Splitting the
difference is the obvious rule and it buries half the village: a cell whose ground sits above its
own floor has the hillside coming through the flagstones. What actually surfaced it was the *ball*
— dropped on the plaza it landed at `y = 0.62` and rolled away, and `engine terrain-height --at
-6,5` said `0.344` where the deck was at `0.12`. The ball had been resting on terrain the whole
time. Clearing the ground instead leaves a gap under the low cells, and a gap is something a tile
can fill: the village tileset fills it with a stone plinth one cell deep, which is what a hillside
village has anyway.

**A terrace step is a free edge.** The first version sheared the neighbour relation by the columns'
difference in lift, so grid layer 1 of a low column faced layer 0 of the column one step above it.
That is geometrically right — those cells really do touch — and it does not survive contact with a
tileset, because what they touch across is a **cut face**: the raised column's ground against the
lower column's open air. The village tileset failed on the first sloped grid it saw, with
`ground@0 refuses air@0 across NegX`. Constraining it would oblige every tileset to carry a socket
for the side of a hill, so every flat tileset would stop working the moment it followed ground.
Columns at different lifts now simply do not constrain each other. The lift still moves the
geometry; it just stopped being an adjacency relation.

## Traps

- **The initial AC-4 seeding must queue each removal once, not once per starved face.** Pushing
  inside the per-face loop queued a tile as many times as it had unsupported directions, and
  `propagate` decrements a neighbour's counters *per stack entry* — so the duplicates drove supports
  below zero, removed legal tiles, and turned solvable blocks into contradictions. It presented as a
  tileset that looked over-constrained: four fallbacks on a 12×2×10 grid that should have had none.
  The fix is a second pass; the symptom is `fallbacks > 0` on a tileset you believe is fine.
- **A failed block must revert, not take the fill.** The article's fallback — fill the block with
  the known-good arrangement — is right when every border is that same fill, which is true of the
  first pass and false of every block after it: a later block's border is an *already solved*
  neighbour, and the fill is only known good against itself. Writing it in produced an illegal grid
  whose diff was nowhere near the block that failed. Reverting is legal by induction instead.
- **A `BTreeMap` field publishes `additionalProperties` and no `properties`**, so the schema walk
  reported every palette key as `unknown_field`. No component has a map field, which is why the walk
  never needed the arm before — and why adding one is safe.
- **`--out` must redirect the write only.** It had also redirected where the *prior* layout was read
  from, so solving into a scratch file lost every lock. The prior belongs to the scene.
- **A violation is `forced` when *either* side is locked.** Pinning a floor out in the open breaks
  the cobble around it too, and reporting those neighbours as `tile_layout_illegal` points the
  author at cells they did not write.
- **The rotation permutation is the most dangerous four lines in the milestone.** Reversed, a
  tileset solves cleanly and renders with every wall facing inward. It is pinned by a test that
  rotates the *geometry* by the same Euler angle and checks the two agree, rather than against a
  hand-written table — which would only pin the table.
- **An orphaned-socket report must name the *authored* face, not the expanded one.** The face a
  socket lands on moves with the turn, so one authoring mistake shows up on `px` at one rotation and
  `nz` at the next. Keying the dedup on the expanded face reported it four times and pointed at
  three faces the author never wrote.
- **A rotationally expanded tile is usually its own partner** — `px` at rotation 0 meets `nx` at
  rotation 2 — so the orphan case that survives expansion is a *vertical* face, which does not turn.
  Any test for the orphan report has to reach for one.

## What this did not touch

**No shader, no pipeline, no binding.** A grid's parts group by palette key and emit one ordinary
`RenderItem` each, which `Tree` already does for its bark and its leaves. A six-material village is
six draws, and `mesh.wgsl`'s four ULP-sensitive lines were never in the blast radius. The A/B
against `main` is what says so rather than the baseline sweep.

**The tour's `TileGrid` carries no `Collider`, deliberately.** An entity with neither body nor
collider is skipped entirely by the physics build, so the collider set is unchanged, both golden
traces pass and the tour's `simulate` assertions are unmoved — which matters because of the rapier
rule CLAUDE.md records twice. The collider is exercised in the fixture, where a re-bless is free.

**One thing it did touch, unavoidably:** `from_road` in `engine-physics` is now
`merge_internal_edges`, and `TileGrid` opts in beside `Road` and `Junction`. Not a tidy-up — a tiled
floor is the canonical coplanar-triangle case, since every slab is flush with its neighbours, and
that is the contact bug that threw a ball off the M23 road at 4.8 m/s. `Terrain` still does not opt
in; the comment there has said it should since M23, and doing so moves `m22_terrain.png` by 1339
pixels.

**The tour's GI bake re-ran.** `collect_occluders` walks the draw list, so a new component in the
tour changes the input digest whether or not the geometry is inside the probe volume. After the
re-bake all six tour frames came back byte-identical to the blessed set.

## Verification

`verify/m47_tiles.json` + `m47_tiles.png`, pinned bit-exactly at `samples: 1` — five renders, one
image, with terrain in frame. Ten CLI tests and forty-two unit tests across the four modules. The
sweep is 53 artifacts, all passing, including both golden traces.

The property that *is* the milestone is a test:
`a_region_re_solve_leaves_the_rest_of_the_village_alone`.
