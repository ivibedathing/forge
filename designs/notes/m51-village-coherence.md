# Closed edges and village coherence (M51)

*Design doc: `designs/tile-edges-design.md` — it holds the diagnosis and the
rejected alternatives. This note holds what building it taught.*

The user's words were "missing walls, or doesn't make sense", and the diagnosis
found four mechanisms: houses truncated at the open grid border, houses cut
open by terrace seams (a floor sat beside street cobble, legal only because
the columns differed in lift), wall runs whose interior flipped sides through
the symmetric `wallrun` socket, and roofs free to rotate per cell. One engine
feature and one tileset rework fixed all four — and the fixing taught more
than the diagnosis did.

## What it moved

| | before | after |
|---|---|---|
| built tiles on the open border (tour) | 17 | 0 |
| buildings straddling a terrace seam | 11 seams | 0 |
| interior flips mid-run | 3 | inexpressible |
| houses | wall mazes and a 60-cell mass history | 4 rectangles, each with a door, 9–16 cells |

## The engine half is small

`TileGrid.edges: "closed"` turns both `Neighbour::Open` cases — the border
*and* the terrace seam — into `Fill { y }`: constrained as the fill of that
layer, street below, air above. It is the WFC literature's **boundary
condition** (UpRoom Games' write-up names it exactly), it rides the layout
header like `offsets`, joins the digest only when closed, and `open` is M47
byte-for-byte. Everything else the milestone did was authoring and two solver
amendments the authoring forced.

## The three things measurement forced

**Enclosure cannot be rejected into existence; it has to be propagated.** The
chiral sockets made a house an atomic thing — and min-entropy collapse almost
never completes an atomic thing by luck. A 16-seed sweep produced at best one
3×3 house and usually bare streets, because any partial building violates and
the attempt that finally settles is the empty one. The answer is the
literature's "fixed tiles draw the floorplan": **a locked floor cell is a
building plot**, and the sockets then *force* a complete house to grow around
it. Weights dropped so spontaneous starts are rare — construction happens
where propagation demands it, which is at the plots.

**A lock poisons the do-no-harm baseline, in both directions.** A plot
sitting in the fill is a one-cell building violating three rules at once, so
under M49's first-non-worsening acceptance the baseline was 6 and a floorless
wall blob rode in under it. Two amendments, both in `synthesize.rs`: the
baseline **skips violations touching a locked cell** (they are the solver's
job, not its allowance) while the candidate counts them all; and acceptance
takes the **best attempt** (fewest violations, early exit at zero) rather
than the first tolerable one. From a clean fill both changes are invisible —
strict is still strict.

**A plot must fit its house inside one block, two cells clear of every seam.**
The sharpest trap: a locked plot in a block's *border ring* forces that block
to grow a wall run it can never terminate — every terminator corner pokes a
run socket into border cobble — so the block contradicts its entire budget
and reverts. It measured as 4 fallbacks and 236 retries with `rejected: []`,
which is the signature: contradictions, not rule rejections. Hamlet-sized
grids now solve as **one block**; the tour's 14×12 uses a 2×2 quadrant
lattice with one plot centred per quadrant, and the runtime build is one call
per quadrant rather than a raster — which also retires the M50 flicker
hazard, since no block is ever re-solved twice in a lap.

## The tileset, in four rules

- **Chirality**: `run_l`/`run_r` on walls and corners. A run keeps its
  interior on one side all the way round; the flip is inexpressible. Cost:
  only convex outlines exist, which for cottages is the look wanted anyway.
- **Ridges**: `ridge` mates only `ridge`, and `roof_gable_end` (same
  geometry, one ridge face swapped for `above`, `rotations: 4`) is the only
  terminator. Every roof is a straight run ≥ 2 with closed thatch ends; the
  chimney's ridge faces put chimneys **on the ridge line**.
- **Four corners**: `region_contains: { tiles: ["corner"], min: 4, max: 4 }`.
  The rule that kills corner-chain warts and fused row-houses at once —
  sockets cannot say "corners join runs through walls", but a rectangle has
  exactly four corners and M49's predicate can say that.
- **A door each**: `region_contains: { tiles: ["wall_door"], min: 1 }`, and
  `region_size` tightened to 9..18 because the smallest enclosure is now 3×3
  by construction.

## Traps

- **`rejected: []` beside high retries means contradictions, not rules.** The
  per-constraint rejection counts only see attempts that *solved*; a block
  whose wave dies in propagation reports nothing. Read the pair together.
- **A single-block plain solve now repairs.** Best-of acceptance plus a
  borderless block means `synthesize` without `--reset` can wave away an
  illegal mass — the M49 note's "a broken layout is never repaired" holds
  only where block borders pin the damage. The reset test pins both halves.
- **`--out` without `--write` writes nothing**, and a stale probe file reads
  as "locks were lost". Half an hour went to a solver bug that did not exist.
- **The old locked cottage was already chiral-legal.** The m47 fixture's
  hand-authored locks happened to use the exact rotation pattern the chiral
  sockets require, so they survived unchanged — worth knowing before
  re-authoring locks that do not need it.

## Verification

The three committed layouts re-solved closed with plots (fallbacks 0
everywhere); both fixture baselines re-pinned bit-exact (three renders, one
hash, terrain in frame at `samples: 1`); the seven tour frames re-blessed and
the tour GI re-baked. Two new engine tests pin `Fill` at the border and the
seam; a solver test pins a closed solve keeping floors off the border; the
reset test now pins the repair split; and the census tests assert 9..18 with
several houses. Full suite green, 55/55 artifacts, clippy silent. No A/B owed:
no shader, pipeline or binding changed — what changed is which arrangements
exist.
