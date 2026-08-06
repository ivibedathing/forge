# Runtime synthesis (M50)

*The tour choreography this note describes — the raster sweep and its
spacing — was revised by M51, which replaced it with one call per block over
locked building plots. The mechanism (the verbs, the queue, `LiveGrids`, the
refusals) is unchanged; read `m51-village-coherence.md` for why the sweep
went.*

*Design doc: `designs/runtime-synthesis-design.md` — it holds the rejected
alternatives. This note holds what building it taught.*

M47 solves a grid at the command line and commits the answer; M49 gave the
answer properties. Both are still-lifes. M50 adds a runtime entry point — two
curated `world` calls — and spends it on the tour, which now flies west to a
hamlet and watches it build itself out of bare cobble.

## The three pieces

| Piece | Where | What it is |
|---|---|---|
| The verbs | `engine-script/src/lib.rs` | `world.synthesize(entity, x, z, radius[, seed])` and `world.clear_tiles(entity)`, queued and drained like `spawn_entity` |
| The live grid | `tilelive.rs` | Tileset, rules, cells and locks held per entity, lazily; the solve; the `ResolvedTileGrid` swap |
| The mapping | `tilegrid.rs::region_around` | World metres → cells, now shared by the CLI's `--at` and the runtime disc |

Plus `engine tile-grid --steps`, a `synthesized` count on the `simulate` report,
a `synthesized` trace line, and the tour's two village legs.

## What the renders decided

**A disc cannot sweep a grid.** The first tour sweep ran a line of discs west to
east at the radius that looked right, and built the middle third of the village
only — because `radius` is one number for both axes, and a disc wide enough in z
to reach the far side of a 24 m grid is wide enough in x to re-solve all of it.
Every trace line said `region: [.., 5, .., 6]`, the same two z rows, fourteen
times. The fix is a **raster** — fifteen positions, z outer and x inner, the
solver's own scan order — and it is what makes the hamlet build row by row
rather than in a stripe. What found it was the trace, not the picture: the
picture just looked sparse.

**Passing over a block twice grows a roof mass.** The sweep before that used a
4 m disc every sixth step, so each block was re-solved five or six times. The
census went from the committed layout's 13 floors and 103 open cells to **3
floors and 53** — one solid raft of roofs with no street in it, which is exactly
the failure M49 was built to fix. The cause is that do-no-harm counts violations
rather than measuring them: once a single over-size built region exists, adding
to it does not increase the count, so every later pass is free to extend it.
**One pass per block** keeps a village with rooms, and the tour's spacing is
that number rather than a taste.

This is the sharpest thing the milestone learned, and it generalises past tiles:
a rejection rule that counts is not a rule that bounds.

## Traps

- **A script's `step` is 0-based**, so the fixture's first version ran a solve at
  step 0 and *then* cleared at step 1, wiping it. The `synthesized` count caught
  it — 10 requests where the script has 9 — and the picture did not, because a
  village built from a cleared grid and a village built from a wiped one look
  equally plausible. This is `m41-buoyancy.md`'s clock note in a new place.
- **The block params must come from the layout header.** A layout solved at
  `block: [5, ∞, 5]` re-solved at runtime on `Params::default()`'s 8 would read
  borders at seams the file was never built against. M49 records the same trap
  for `--check`; this is its third instance, and `check_bake`'s `samples` is the
  shape all three take.
- **Absolute asset paths are refused, and correctly.** The first version of the
  CLI tests' scratch-scene helper named the village tileset by absolute path so
  the scratch dir would not need a copy. `validate` rejected it with
  `asset_path_not_relative` — invariant 3, doing its job. The helper copies the
  tileset in beside the scene now, which is what any real scene does.
- **`region_around` had to move before anything used it twice.** The CLI's
  `--at`/`--around` mapping and the runtime disc are the same function, and the
  first thing built in this milestone was moving it into `engine-core` with the
  two callers pinned against each other. M40's road and its centreline query are
  what that habit comes from.

## Physics does not follow, deliberately

A `TileGrid` carrying a `Collider` is **refused** at the call, with
`tile_grid_collides`. Rebuilding a static trimesh mid-run means removing and
re-inserting a collider, which perturbs the broad phase and moves every body in
the scene — the rule CLAUDE.md records twice, and a feature whose side effect is
"every crate shifts on the steps a script happens to call this" needs its own
answer. A stale collider is worse: a village you fall through.

The tour's hamlet has carried no `Collider` since M47, for this family of
reasons, so the two village legs cost the golden traces nothing and both pass
untouched.

## The tour

Two legs rather than one, and that is a framing constraint rather than a
flourish: the director's aim only swings to a new subject at 62% of a leg, so a
village reached in one leg is a village already finished by the time it is
looked at. Leg 5 approaches it (the aim arrives at ~p 1012, on bare cobble); leg
6 crosses it with **both aim keys the same point** — the only leg in the tour
whose aim is constant — so the hamlet stays centred for the three seconds it
takes to solve. The lap is 1440 steps.

`total` stays **900**. The tour proper is still five stations in fifteen
seconds and the village is on the way home, which is honest and also keeps the
HUD counter's denominator — the one thing a new leg could have moved in an
already-blessed frame — where it was.

What re-blessed anyway: four of the six tour frames, because the hamlet grew
from 7×6 to 14×12 cells and is visible from the arena at that size; and the
tour's GI bake, because `collect_occluders` walks the draw list. `showcase_1150`
joins the manifest as the village leg's frame, in the tolerance class with the
other six and with no test, for the documented reason.

## Verification

`verify/m50_live_tiles.json` + its `.rhai` + `m50_live_tiles.png`, pinned
**bit-exactly** — five renders gave one image, which is what `samples: 1` on
flat ground buys. The frame is rendered at step 70, after nine requests have
landed, so a build with the feature deleted renders bare cobble and fails by
19% of the frame.

Eight CLI tests and five unit tests over `region_around`. The two that matter
most are `a_runtime_solve_is_the_same_twice` (determinism, which the baseline
rests on) and `a_runtime_solve_leaves_the_committed_layout_behind` — without the
second, the first passes on a build where `world.synthesize` does nothing.

No A/B is owed: no shader, no pipeline, no binding and no geometry *generation*
changed. What changed is when `solid_for` is called.
