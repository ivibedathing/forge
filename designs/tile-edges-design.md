# M51 — Closed edges, and a village whose houses hold together

M50 made the tour's hamlet worth flying to, and flying to it is what showed the
buildings: houses open on one whole side, plaster facing the wrong way, floors
running out into the street, roofs a jumble of one-cell pyramids. The user's
words — "missing walls, or doesn't make sense" — and the diagnosis found four
mechanisms, three of them structural:

- **A. Open borders.** 17 built tiles sit on the grid's edge in the tour
  layout. Off-grid is unconstrained (M47's "a village is a window onto a larger
  world"), so a house may put its far wall outside the world.
- **B. Terrace seams.** 11 places where a building straddles a lift step.
  M47 made steps socket-free — right for the *cut face* problem — but nothing
  then stops a building from straddling one, and a straddling building is cut
  open by a cliff. The layout even holds a floor beside street cobble, legal
  only because the columns differ in lift.
- **C. Interior flips.** `wallrun` is symmetric, so `wall@0` (interior +z)
  legally continues into `wall@2` (interior −z): the plaster jumps sides
  mid-run and the room has a hole. Three flips in the committed hamlet.
- **D. Roof incoherence.** A gable's rotation is free per cell, so roofs come
  out as 90°-alternating pyramid scraps rather than ridges.

The literature names the fixes. UpRoom Games' WFC write-up calls A's fix a
*boundary condition* — collapse everything beyond the border as the empty tile,
so interiors cannot leak off the world. Boris the Brave's tips file C under
designing adjacency so the inside/outside labelling always matches across a
join — a handedness the sockets must carry. D is the "tiles that connect to
exactly N others" trick: a ridge socket only a ridge or an end-cap can answer.

## 1. Scope

| | |
|---|---|
| **E0** | `TileGrid.edges: "open" \| "closed"` — closed constrains every free edge (border **and** terrace seam) as the fill pair: street at ground, air above. Default open is M47 byte-for-byte |
| **E1** | The field rides the layout header, the digest (only when closed), `validate`'s mismatch check, and `verify_adjacency` |
| **E2** | The village tileset reworked: chiral `run_l`/`run_r` wall sockets, `ridge` sockets with a `roof_gable_end` cap, the chimney moved onto the ridge, weights and constraint bounds retuned |
| **E3** | All three committed layouts re-solved closed; renders judged; baselines re-blessed |

Not in scope: an inner-corner tile (L-shaped buildings), a hip-roof cap,
multi-storey walls. §5.

## 2. `edges: "closed"`

`Grid::neighbour` is the only place the grid's shape is interpreted, and both
of its `Neighbour::Open` returns — sideways off the patch, and across a lift
step — become `Neighbour::Fill` when the grid is closed. A `Fill` face
constrains against the fill tile of its own layer: `fill_ground` at `y == 0`,
`fill_background` above. The solver folds it into the initial domain exactly as
it folds a border cell; `verify_adjacency` checks it the same way, taking the
fill pair as a parameter.

Treating a terrace seam as *street/air* rather than as hillside is the same
simplification M47 chose, pointed the other way: M47 said "constrain nothing
there", M51's closed mode says "constrain it as the empty street". Both avoid
obliging every tileset to carry a hillside socket. What closed mode buys is
that a wall may stand at a seam (its `out` face mates street) while a floor may
not (its `in` face refuses it) — a building must end before the step, which is
the terraced-village behaviour M47's note wished for but never enforced.

The component field directs the *next* solve; the header records what the file
was solved with, and is what `validate` and the resolve pass read — the
`block`/`overlap` split again. A component flipped to `closed` over an open
layout is `tile_layout_mismatch`, the cheap check, before the digest would
catch it anyway. The digest folds `edges` in **only when closed**, M49's
skip-the-empty-list rule, so no pre-M51 layout goes stale.

## 3. The tileset

**Walls become chiral.** `wall`/`wall_door`/`wall_window` carry
`px: "run_r", nx: "run_l"` (interior at +z); `corner` carries
`px: "run_r", pz: "run_l"`. Since `_l` only mates `_r`, a run keeps its
interior on one side the whole way round, and the flip that cut the hamlet
open stops being expressible. The cost is that only convex outlines exist —
an L-shaped house needs an inner-corner tile this set does not have — which
for a hamlet is a feature: rectangles read as cottages.

**Roofs become runs.** `roof_gable`'s ridge faces become `ridge`; its eaves
stay `above` (air must be able to meet a roof edge-on). `ridge` mates only
`ridge`, so a gable must continue — and a new `roof_gable_end` (same geometry,
`rotations: 4`, one ridge face swapped for `above`) is the only way a run can
stop. Every roof is therefore a straight ridge of ≥ 2 cells with closed thatch
triangles at its ends; the one-cell pyramid stops being expressible. The
chimney's `pz`/`nz` become `ridge` too — a chimney is now a ridge segment,
which is where chimneys go, instead of a tile that could sit anywhere a roof
could.

**Bounds follow.** With enclosure forced, the smallest legal house is 3×3 —
eight wall pieces around one floor — so `region_size` tightens from
`{min: 4, max: 18}` to `{min: 9, max: 18}`, which is arithmetic, not taste
(M49's note: sizing rules is one measurement).

## 4. What re-blesses

The three committed layouts re-solve (`--reset`, closed, new sockets), so:
`m47_tiles.png`, `m50_live_tiles.png`, the seven tour frames, and the tour's
GI bake. Both golden traces survive — the hamlet still has no collider, and
the m47 fixture's collider set changes only through its village's geometry,
which that fixture re-blesses anyway. The m47 fixture's locked cottage is
re-authored to be legal under the chiral sockets — a lock that violates is
only a warning, but a fixture that *asserts* through locks should assert
something true.

## 5. Deliberately absent

- **An inner corner.** Concave outlines (L-, T-, U-shaped buildings) need a
  fourth wall tile whose run turns the other way. Additive; the hamlet reads
  better without it.
- **A hip cap.** A 1-cell roof, closed on all four sides, would want a real
  pyramid part; the wedge pair fakes it badly. With ridge sockets it is simply
  absent, and 2-cell cottages roof as a 2-cell ridge.
- **Hillside sockets.** Closed mode constrains a seam as street/air. A tileset
  that *wants* to build into a hillside — steps, retaining walls — wants the
  sheared-adjacency design M47 §5 rejected, and should get it deliberately.
- **Second storeys.** Wall tops still demand roof or chimney; walls do not
  stack. A `storey` socket is authoring work for a taller tileset.
