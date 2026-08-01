# Entity spawning (M37)

Design: `designs/entity-spawning-design.md`. It closes §1 of `designs/structural-holes.md` — the
asymmetry where a script could destroy an entity but not create one, so a run could only ever
shrink.

A `templates` block declares entity definitions the scene never instantiates;
`world.spawn_entity("Bullet", x, y, z)` materializes one and returns its name, and
`world.despawn_entity(name)` takes it back out. **The runtime half was already built** — M14
breaking mutates the entity set mid-run — so almost all of the milestone's thinking went into the
authoring question the design doc answers, and almost all of its *debugging* went into the four
things below.

## The trap that cost the most: `spawn` is a reserved keyword in Rhai

The first script written against this API failed to compile with `'spawn' is a reserved keyword`.
Rhai reserves a set of words for future language features and `spawn` is one of them, so the
function is **`spawn_entity`**, which also matches the existing `break_entity`. `despawn_entity`
follows it for symmetry rather than necessity — `despawn` is not reserved.

A second Rhai limit bit twice while writing fixtures: **the curated engine's expression-complexity
budget rejects long concatenation chains**. `world.hud("a" + b + "c" + d + "e" + f)` with six terms
is `Expression exceeds maximum complexity` at compile time, not a runtime cost. Split it into two
`world.hud` calls or two statements.

## The spawn is immediate in the ECS and deferred in physics

The entity exists in `hecs` before `spawn_entity` returns, so the very next line can set its
velocity through the ordinary API. What it does not have yet is a rapier body: those are inserted
by the caller **between the script step and the physics step**, which is what makes
`set_linear_velocity` on the line after a spawn arrive as the body's *initial* velocity rather than
a correction one step later. Despawns go the other way — queued and applied beside the breaks,
after physics, so the entity writes its final position into the trace before it disappears.

Two consequences worth keeping:

- **The name table needed an overlay.** `WorldApi.names` is an immutable `Rc<HashMap>` shared
  across scripts and rebuilt once per entity-set change; a live spawn cannot rebuild it mid-step.
  Spawned names land in a second `Rc<RefCell<HashMap>>` consulted *after* it and cleared by
  `sync_names`. Two maps rather than one mutable one on purpose: every lookup in every pre-M37
  script takes the first, and making that path pay a `RefCell` borrow would be a cost on scenes
  that spawn nothing.
- **`PhysicsWorld` had to keep its collision-layer table.** It was a local in `build`, dropped at
  the end, and `insert_entity` passed `&HashMap::new()` — harmless for M14 fragments, whose
  colliders always carry `layers: None`, and silently wrong for a spawned bullet whose `"bullet"`
  layer would have meant no bit at all. The table is a field now, and template colliders join the
  bit assignment at build so a layer only a spawned collider mentions still means the same bit as
  the wall that filters on it.

## A baked scene is an ordinary scene, which the counter has to know

`simulate --bake` already spliced runtime-spawned entities back in as full entities (M14 needed
it), so a resumed run opens a file that already contains `Bullet#7`. A counter starting at 1 would
mint a second `Bullet#1` and put two entities under one name — invariant 4 broken at runtime, with
no validation error anywhere because the file was fine when it loaded.

`SpawnLedger::new` therefore reads the world: any `Template#N` already present advances that
template's counter past it and counts as live. **This is why the ledger takes a `&World` rather
than just the templates**, and it is the reason bake-resume — the per-step control interface the
demo timelines are authored with — still works on a scene that spawns.

## `ccd` is not optional for anything you throw

The showcase tour's embers are 7 cm spheres launched at 3.5 m/s. Without `ccd` they tunnelled
through the terrain heightfield and fell forever; the tour's own
`the_showcase_tour_runs_fifteen_deterministic_seconds` caught it, because that test asserts no body
ends below y = -1. A body moving further than its own diameter in one step is the condition, and a
spawned projectile is the easiest way in this engine to author one — the pre-authored props that
existed before M37 were all large, slow, or both.

## What it costs, measured

**The A/B was clean and the tour still re-blessed, and both facts matter.** Rendering every
manifest scene with a binary built at `main` and with this one gave **34 of 34 comparable artifacts
byte-identical** — the code moves no pixel. The seven the reference binary could not render are the
two scenes that now use `templates`, which it rejects, exactly as it should.

The six `showcase_*` frames still had to be re-blessed, and the diff image says why: the changed
pixels are the *breaking crates* at the other end of the arena, not the embers. Adding dynamic
bodies changed the broad phase, and float addition is not associative — `CLAUDE.md`'s "a physics
scene is not stable under the addition of a collider anywhere in it", in its sharpest form yet.
`designs/entity-spawning-design.md` §6 predicted this before the change was made; it is worth
predicting again rather than debugging.

## What the arena shooter got

The demo's twenty-four bullets were entities parked at y = -30 and recycled — the pre-authored-and-
hidden shape the whole scene was built out of. Regenerating it with a `Bullet` template **deleted
864 lines** and added 34.

What did *not* change is that bullets are still not physics: the script flies them and swept-segment
tests them against drone centres, because a 46 m/s round covers 0.77 m per step and because a hail
of fire must not disturb the arena. That was a design decision, not a workaround, and M37 does not
undo it. The *pool of state slots* survives too — `world.state` holds numbers and not lists, so a
fixed set of slots is still how a script tracks several bolts at once. What a slot holds changed:
the instance index of a bullet that exists, rather than the index of one that always did.

One consequence that is easy to miss: **a save is a snapshot of the state map, bullet slots
included**, and the bullets those slots named are gone on the next run. `topdown_shooter.rhai`
clears the slots on load, or the flight loop asks where `Bullet#7` is and gets a runtime error.
That is the M37 half of "a save is the campaign, not the arena".

## Deliberately absent

- **Prefab files** (`prefabs/*.json`), deferred with the field name already chosen: `asset` on a
  template, exclusive with `components`, exactly as `Material` does it since M26.
- **`Script`, `Camera` and the three light components inside a template.** Each has a scene-level
  budget that validation checks, and a spawn must not be able to make a valid scene invalid at step
  40. `PointLight` is the one that will be asked for — a muzzle flash wants a light — and reversing
  it needs a runtime answer to the ≤8 budget, which is a design rather than an oversight.
- **Spawning with a parent, or relative to another entity.** `spawn_entity` takes world
  coordinates; muzzle-relative is `world.forward()` plus arithmetic.
- **Endless waves, `RETRY`, and restoring a mid-level arena.** All three are now ordinary work
  rather than blocked work, and none of them is built. See `designs/structural-holes.md` §1.

## Verification

`examples/scenes/verify/m37_spawn.json` + `scripts/m37_spawn.rhai` at `--steps 120`, pinned
bit-exactly by `cli.rs::the_spawn_fixture_matches_its_baseline`. **The arc of five shots is the
assertion**: nothing in the file draws a sphere, so every ball in the frame was spawned, aimed,
simulated and reaped. A spawn that silently did nothing, arrived a step late, or never reached
physics all render as a frame with no spheres in it.

Six renders of the fixture gave one image, so it takes a hard pin — it has no `Terrain` and no
`Meadow` in frame, which is the condition `CLAUDE.md` gives for that.
`cli.rs::simulate_reports_traces_and_bakes_what_a_run_spawned` covers the half a picture cannot
answer: the report's `spawned` total, the trace's `spawned`/`despawned` events, and that a name is
never reused.
