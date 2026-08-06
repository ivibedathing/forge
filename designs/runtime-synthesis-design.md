# M50 — Runtime synthesis: a village that builds itself while you watch

M47 made an arrangement a thing this engine can grow. M49 made the arrangement
hold properties adjacency cannot state. Both of them run at the **command
line**, and what they produce is a file: `engine synthesize --write`, then the
scene draws what the file says.

That is the right default and it stays the default. What it cannot do is show
the synthesis *happening*. The showcase tour has carried a `TileGrid` since M47
and it is parked at `x = -46`, outside every camera key the director flies —
present because a repo-contract test requires every component to appear in the
tour, and invisible because a solved layout is a still life. The one system in
the engine whose *process* is the interesting part is the one system the demo
cannot show.

This milestone gives the solver a runtime entry point, driven from a script, and
spends it on the tour: a village that assembles itself block by block while the
camera flies around it.

---

## 1. Scope

| | |
|---|---|
| **R0** | `world.synthesize(entity, x, z, radius[, seed])` — re-solve the blocks meeting a world-space disc, mid-run |
| **R1** | `world.clear_tiles(entity)` — back to the known-good fill, locks kept, nothing solved |
| **R2** | The live grid: tileset, layout and cells held per entity, so a call is CPU-only and touches no file |
| **R3** | The regrow — a new `ResolvedTileGrid` on the entity, which is all the renderer needs |
| **R4** | `simulate` reports `synthesized`; `--trace` logs a `synthesized` event line |
| **R5** | The tour's village station, and the director calls that grow it |
| **R6** | `verify/m50_live_tiles.json` + its script, its baseline, its manifest entry, its CLI tests |

Not in scope, and each named again in §8: the collider following the geometry,
writing the live layout back to disk, synthesis spread across steps, and a
`TileGrid` inside an M37 template.

---

## 2. Why a script call, and why it is queued

Three shapes were considered for what drives a runtime solve.

**A `TileGrid` field — `stream: {radius, interval}` — that the engine acts on.**
The scene says "keep the cells near the camera fresh" and the engine does it
every N steps. It needs no script and it works in `screenshot` and `run-scene`
alike. It was rejected because it makes a component field a *behaviour*: the
grid would have to know which camera is active, how often is often enough, and
what to do in a headless run with no camera at all. Invariant 5 says components
are plain data and all logic lives in systems; a field that means "re-run a
constraint solver near whatever the renderer is pointing at" is not plain data.

**Solve at load from `seed`, no layout file.** This is the alternative
`tile-synthesis-design.md` §3 already rejected, and every reason still holds —
chiefly that "modify this area" needs a previous layout to modify. Runtime
synthesis does not reopen it: the committed layout is still where a run *starts*.

**A curated `world` call, queued and drained by the caller.** This is M37's
`spawn_entity` shape, and it is the one that fits. The script already knows
where the camera is — the tour's director *is* the camera — so the region comes
from the same arithmetic that placed the eye, with no second notion of "near".
A scene that never makes the call takes exactly the pre-M50 path, which is the
house rule.

**Queued rather than immediate**, for `take_breaks`' reasons. A solve that ran
inside the script call would rebuild geometry halfway through a step, so an
entity's drawn shape would depend on whether the line that changed it ran before
or after some other line that read it. Queueing puts the whole rebuild at one
point in the step — after scripts, before physics — where it is one event with
one order. It also keeps the scripting crate free of the solver: the queue
carries a request, and `engine-core` owns what a request means.

The call therefore returns nothing. A script that wants to know what happened
reads the trace, which is where the rest of this engine's answers are.

### The two verbs

```rhai
world.synthesize("Hamlet", x, z, 6.0);        // the component's own seed
world.synthesize("Hamlet", x, z, 6.0, 1234);  // an explicit roll
world.clear_tiles("Hamlet");                  // back to the fill, locks kept
```

`clear_tiles` exists because of M49's do-no-harm rule. A block is accepted when
it does not *increase* the violations blamed on it, which means an already-good
layout re-solved in place stays good and changes little — the village churns but
never *builds*. Clearing to the known-good fill is what `synthesize --reset`
does before it solves, split out as its own verb so a script can clear once and
then grow the grid over as many steps as it likes. Locks survive a clear, for
the reason they survive `--reset`: they are the one thing a reset is not meant
to throw away.

`radius` is metres, `x`/`z` are world metres, and the disc clamps to the grid
rather than refusing — M48's rule for `--at`, shared with it rather than
reimplemented beside it. A rectangle entirely off the grid is a runtime error,
not a silent no-op, because "the village did not change" is indistinguishable
from "the village was already right".

---

## 3. The live grid

A `TileGrid` at load resolves two files into a `ResolvedTileGrid` and drops
everything else. A runtime solve needs what was dropped: the expanded tileset,
the compatibility table, the rules, the terraced `Grid`, the current cells and
the locks.

`LiveGrids` holds them, keyed by entity name, and is **built lazily on the first
call for that entity**. A scene that never synthesizes at runtime never reads a
tileset twice and never pays the memory — the same reason M37's `SpawnLedger` is
empty for a scene with no `templates`. After the first call the state is
resident, so a per-step solve reads no file at all, which is what makes the tour
station affordable.

It is not hidden state (invariant 2) for `ResolvedTileGrid`'s reason, one step
further out: the live cells are a pure function of the committed layout, the
tileset, and the sequence of calls the script made — and the script is a file on
disk, run against a fixed clock. Replaying the scene reproduces them exactly.
What is *not* claimed is that the live cells are the file's cells; they are not,
after the first call, and §5 says what that costs.

### Block params come from the layout header

Not from `Params::default()`. This is M49's `--check` trap in a second place: a
layout solved at `block: [4, ∞, 4]` re-solved at runtime with the default `8`
would use a different lattice, so "the blocks meeting this disc" would name
different cells than the file was built from and the borders would not line up
with the seams already in it. The header is the authority, exactly as it is for
`--check`.

---

## 4. What a solve does to the frame

`solid_for` is already cached on a `GridKey` over the cells, so a re-solve that
happened to produce the same arrangement hands back the same `Arc` and the
renderer — which keys its uploads on `Arc::as_ptr` — does not re-upload. A solve
that changed something produces a new `Arc` and one re-upload of the grid's
merged meshes. That is the whole render-side cost, and it is why this milestone
touches no shader, no pipeline and no binding: the draw list is rebuilt from the
world every frame anyway, and a `TileGrid` was already six `RenderItem`s.

The rebuild is skipped entirely when no request was queued, so a step with no
call costs one `is_empty()`.

---

## 5. Physics does not follow, and says so

A `TileGrid` that carries a `Collider` gets a static trimesh at build, merged
across palettes. Re-solving at runtime would make that trimesh a lie.

Rebuilding it is not what it looks like. `insert_entity` — M12's fragment path,
M37's spawn path — takes a `Presence` that deliberately has no generated-surface
arm: its comment says a `Terrain` or a `Road` owns its grid and is not something
a break or a spawn produces. Threading a regenerated surface through it is real
surgery, and on the other side of it sits the rule CLAUDE.md records twice: the
collider set is an input to the broad phase, and removing and re-inserting a
static trimesh mid-run perturbs **every body in the scene**. A feature whose
side effect is "every dynamic body moves a few millimetres, on the steps a
script happens to call it" needs its own answer and its own re-blessing story.

So: **a runtime solve on a grid that has a `Collider` is a runtime error**,
naming the entity and saying to drop the collider or use `engine synthesize`.
Refusing is the only honest option of the three — a stale collider is a village
you fall through, and silently rebuilding it moves bodies at the other end of
the arena for reasons the author cannot see.

The tour's hamlet has carried no `Collider` since M47, deliberately and for this
exact family of reasons, so the station costs the golden traces nothing.

---

## 6. The tour's village station

The tour is where this is spent, and the hamlet has to become visible to spend
it. Three changes:

- **The grid grows** from 7×2×6 cells to 14×2×12 — 28 m by 24 m at the village's
  2 m cell — so it holds a lattice of blocks rather than the single block a
  7×6 grid holds at `block: 8`. A sweep across one block is not a sweep. Blocks
  drop to `[5, ∞, 5]` at overlap 1, which puts nine of them under the camera.
- **A station of its own**, appended after `05 THE WHOLE WORLD` and before the
  way back. Appending is what keeps steps 0–899 arithmetically identical: every
  cue, every eased interpolation and every physics input in the first five
  stations is untouched, and the only thing that moves in a frame rendered at
  step 810 is the HUD counter's denominator. The five existing station timings,
  the boulder's release at 545, the pillar at 600 and the blast at 636 are all
  where they were.
- **The director grows the village.** At the station's first step it calls
  `clear_tiles`, and then it sweeps `synthesize` across the grid in step with
  the dolly, so the arrangement settles behind the camera as it travels. The
  seed advances with the sweep, so a second lap builds a different village on
  the same ground — which is the honest demonstration of what a solver is, and
  a still frame of a fixed layout is not.

What this costs in blessing: the six `showcase_*` frames, which are the
tolerance class already, plus the tour's GI bake, because `collect_occluders`
walks the draw list and the hamlet's footprint changed at load. Both golden
traces and every `simulate` assertion survive, because nothing in the tour
gained a collider or a body.

---

## 7. The fixture

`verify/m50_live_tiles.json` — a village on flat ground, `samples: 1`, with a
script that clears the grid at step 0 and then synthesizes a widening disc.
Flat, because CLAUDE.md is explicit that fine geometry against relief is not
bit-reproducible under MSAA on this adapter, and a tiled village on a hillside
is that case exactly; the terracing is M47's fixture's job and it stays there.

The baseline is rendered at a step **after** several solves have landed, so it
asserts the whole chain: the queue drained, the region resolved from metres, the
solver ran on the live cells, and the regrown geometry reached the draw list. A
frame rendered at step 0 would pass with the feature deleted.

What a picture cannot say goes to tests:

- the same scene at the same `--steps` gives byte-identical cells twice, which
  is the determinism claim;
- a `clear_tiles` leaves locked cells alone;
- a disc that meets no cell is an error, not a silent no-op;
- a runtime solve on a grid with a `Collider` is refused, with the code;
- a scene that never calls either verb loads and renders exactly as it did
  before this milestone, with no tileset read a second time;
- the region a disc resolves to matches what `engine synthesize --at x,z
  --radius r` resolves the same disc to — the shared helper, pinned rather than
  assumed, because two implementations of one mapping is how M40's road and its
  query started disagreeing.

---

## 8. Deliberately absent

- **The collider following the geometry.** §5. It is the one thing a caller will
  want next — a village you can walk in — and it wants an answer to the broad
  phase perturbation before it wants code.
- **Writing the live layout back.** A run that ends does not save what it grew.
  Adding it means deciding whether a *screenshot* writes into the project, which
  `bake-gi` and `synthesize` both answer with "only when asked, by a command
  whose name says so". A `world.save`-style call could carry it later; the
  M36 save file is where it would go, not the layout file.
- **A solve spread across steps.** One call is one solve, synchronously, inside
  the step. The tour's grid solves a few blocks in low single-digit
  milliseconds, so slicing it would buy nothing measurable and would cost the
  property that a step is a step. A 60×60 grid would want it, and would want a
  budget in cells rather than in time, since time is not a thing a fixed step
  may read.
- **A `TileGrid` in an M37 `template`.** Unchanged from M47: spawning one means
  resolving files at runtime, which entangles with the still-open hot-reload
  decision. Runtime *re-solving* of a grid the scene already declares does not
  touch that question — the files were resolved at load.
- **`world.synthesize` reporting what it did.** The call returns nothing (§2).
  Returning a block count would mean solving inside the script call, and the
  trace already answers it.

---

## 9. Build order

1. The shared world→cell region helper, moved into `engine-core` and pinned
   against the CLI's own use of it, before anything depends on two copies.
2. `LiveGrids` in `engine-core`: lazy load, `clear`, `resynthesize`, the
   `ResolvedTileGrid` swap. Unit-tested headlessly, which is where the solver
   already lives.
3. The script queue and the two verbs; `take_synthesis` beside `take_spawns`.
4. Draining in `simulate.rs` and in the viewer's loop, the trace line, the
   `synthesized` count in the report.
5. The refusal for a grid with a `Collider`, and its error code.
6. `verify/m50_live_tiles.json`, its script, its baseline, its manifest entry,
   its CLI tests.
7. The tour: the bigger hamlet, the station, the director's sweep. Re-solve the
   committed layout at the new size, re-bake the GI, re-bless the six frames.
8. `designs/notes/m50-runtime-synthesis.md`, CLAUDE.md, `docs/` where the script
   API is listed.
