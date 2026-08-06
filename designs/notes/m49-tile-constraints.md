# Tile constraints (M49)

*Design doc: `designs/tile-constraints-design.md` — it holds the rejected
alternatives. This note holds what building it taught.*

M47 generated villages but not *buildings*. M49 adds the properties face
adjacency cannot state, as one constraint type with four predicates, enforced by
rejecting a block that breaks them.

## What it moved

Measured before and after with the same script, on the same seeds:

| | built cells | built regions | open regions | floors |
|---|---|---|---|---|
| m47 village, before | 60 | **one region of 60** | 19, **and an orphan of 1** | 19 |
| m47 village, after | 36 | 10, 11, 15 | **one region of 44** | 11 |
| tour hamlet, before | 24 | 23, **and a lone cell of 1** | 16, **and two orphans** | **0** |
| tour hamlet, after | 32 | 16, 16 | **one region of 10** | 14 |

The hamlet is the sharper case: twenty-four wall and corner pieces enclosing
nothing at all became two cottages with fourteen rooms between them.

## The three things measurement forced

**Strict rejection does not converge.** The first version re-rolled a block on
any violation, and *every block failed every attempt* — 380 of them, across
block sizes 3×3 to 8×8 and budgets of 20 and 60. The cause is the blame rule
meeting an already-violating layout: a 60-cell mass is blamed on every block
that touches it, and no block can fix it because most of the mass lies outside
anything it can change. A block is now asked not to **increase** the violations
blamed on it. From the fill there are none, so a fresh solve is exactly strict.

**Which made `--reset` necessary.** Do-no-harm's corollary is that a layout
which already breaks a rule is never repaired, only kept from worsening — so
adding constraints to a tileset silently did nothing to the tour's hamlet, the
exact layout the feature existed for. `--reset` solves from the known-good fill
with the locks still in it. It also turned out to make the M47 fixture
reproducible by the CLI rather than by a scratch Python script, which is
strictly better: `synthesize --reset --write` now reproduces the committed
layout byte for byte, and a test says so.

**Weights are not a substitute.** Before finding the convergence bug I swept the
open-ground weight over 11, 25, 45 and 80 looking for a hit rate. Every run
reported *identical* retries — the budget was saturated, every attempt failing
regardless — which is the signal that a measurement is not measuring what you
think. A saturated counter looks like a flat response.

## Traps

- **A violation a block cannot avoid is a no-op, not a fallback.** Do-no-harm
  accepts anything that does not make matters worse, so a globally impossible
  rule leaves the village alone instead of re-rolling it into the fill. The
  intuition runs the other way, which is why it has a test.
- **`--check` must read the block params from the layout header**, not from the
  CLI defaults. An M47 bug this milestone surfaced: any layout solved at
  other-than-default blocks reported stale for ever. `check_bake`'s treatment of
  `samples` is the shape.
- **The digest must skip an empty constraint list.** Feeding a zero length in
  still changes the hash, which would have marked every pre-M49 layout stale for
  a field it never had. The house rule that let M16 add five features without
  re-blessing anything.
- **Regions ignore the terrace lift.** `Grid::neighbour` treats a step as a free
  edge, which is right for sockets and wrong here: a building does not stop
  being one building because it stands on two levels. `constraints.rs` walks
  plain XZ adjacency for that reason, and a test pins it.
- **A test comparing a region solve against a *different* full solve measures
  two starting states, not locality.** M48 made `--out` redirect the write only,
  so the prior always comes from the committed layout; the comparison has to be
  against that layout.
- **The committed layout is the product of `--reset`, so a plain re-solve does
  not reproduce it.** A block handed a different starting state finds a
  different valid answer — the same non-idempotence M47 documents for
  `--region`, and what the deferred similarity picker would remove.

## Authoring, in practice

The village's rules are two:

```json
{ "tiles": ["wall", "wall_door", "wall_window", "corner", "floor"],
  "region_size": { "min": 4, "max": 18 },
  "region_contains": { "tiles": ["floor"], "min": 1 } }
{ "tiles": ["cobble", "post"], "regions": { "max": 1 } }
```

Sizing them took one measurement rather than four render passes, which is the
difference from M47: `region_size` is in cells and a 3×3 cottage is nine of
them, so the bound is arithmetic rather than taste. What still takes iteration
is the *seed* — about one in eight gives a solve with no fallbacks at all, and
the fixture moved from seed 7 to 41 to get one.

## Verification

Eight unit tests over hand-drawn grids (`.` open, `#` wall, `o` floor — a
picture is the clearest way to state a region property), seven CLI tests, and
the before/after census as two of them. The fixture and two tour frames
re-blessed; the tour's GI re-baked, after which all six frames came back
byte-identical.

No A/B is owed: no shader, no pipeline and no geometry generation changed. What
changed is which arrangement the solver accepts.
