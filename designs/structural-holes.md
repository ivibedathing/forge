# Structural holes

**Four gaps that block a capability rather than polish one — one of them now closed.**
`CLAUDE.md`'s "Deferred follow-ups, by area" is the full backlog — three dozen items, most of them
a nicer version of something that already works. This document is the short list underneath it:
the places where a scene *cannot express* something, and where the workaround is a demo bending
itself around the engine rather than an author choosing a lesser option.

**The test for being here.** A deferral is a structural hole when at least one of:

- a live demo is shaped by its absence (not merely less pretty for it),
- the workaround is authoring the same thing many times, or hiding it,
- it is an *open* decision rather than a known-shape task — someone has to choose, not just build.

Items that fail all three — shadow cascades, textured terrain layers, road junctions, the UI
font atlas, ragdolls — are real work and are not here. Neither is anything **rejected**: blending
and crossfades (M9 §8, leaned on by M30 and M32), material overrides on a referenced asset
(`material-system-design.md` §5), and a script-settable time of day (M21 — hidden state). Those
are decided, not pending.

---

## 1. Entity spawning — **closed by M37**

**A script can create an entity now.** `designs/entity-spawning-design.md` is the milestone;
`designs/notes/m37-entity-spawning.md` is what building it taught. The shape it took is the one
sketched below — a `templates` block the scene file declares, and
`world.spawn_entity("Bullet", x, y, z)` returning the new entity's name — chosen for the reason the
sketch gave: arbitrary construction from a script would put geometry in a `.rhai` and break
invariant 2.

The original entry is kept below, because what it *predicted* is worth keeping next to what
happened. Three of the four things it said this cost are now merely undone rather than impossible:

- **Projectiles exist.** The arena's twenty-four bullets were entities parked at y = -30 and
  recycled; they are a template now, and regenerating the scene deleted 864 lines of hidden props.
- **`RETRY` and endless waves are now ordinary work, not blocked work.** Nothing has been built:
  the campaign is still four levels and the end card still has no `RETRY`, because putting ten
  broken drones back means a `Drone` template, a wave that spawns from it, and an answer for what a
  mid-level save restores. That is a piece of work with a known shape. It used to be a wall.
- **A save is still the campaign, not the arena**, for the same reason — the save records `level`,
  `score`, `health` and settings, and nothing about which drones were broken.

The four open sub-questions were answered as follows, and the design doc argues each: instance
names are `Template#N` from a per-template counter that never reuses; a template lives **in the
scene**, with prefab files deferred behind the `Material.asset` shape; `inspect` reports a
`templates` array and `validate` runs the full per-entity walk on one; and determinism survives
because every input to a spawn is already ordered — while the collider set moving the broad phase
is exactly as predicted, and a scene that gains spawning re-blesses.

<details>
<summary>The entry as written before M37</summary>

**A script cannot create an entity.** It can destroy one — destruction has exactly one owner and
it is the script — so the asymmetry is the whole problem: a run can only ever shrink.

**What it costs.** `designs/arena-shooter.md` is shaped by this sentence in four separate places:

- Every level's ten drones and its barrels are **in the file from the start**, parked 46 m above
  the arena, flying down when their wave begins. "It is already there, 46 m up" is the engine's
  substitute for spawning.
- The campaign is **four levels rather than endless**, because endless would mean authoring
  endless drones.
- The end card **has no `RETRY` button** — a restart would have to put ten broken drones and four
  broken barrels back, so offering it would be offering something that cannot work.
- **A save is the campaign, not the arena** (M36): it restores level, score, health and settings,
  never which drones were broken. `LOAD GAME` is therefore offered on the title card only.

There are also no projectiles anywhere in the engine, for the same reason.

**Why this is more tractable than it looks: the runtime already does it.** M14 breaking mutates
the entity set mid-run, deterministically, and every hard part is solved and traced:

| Piece | Where |
|---|---|
| Spawn into the live `World`, with physics bodies built | `crates/engine-physics/src/breaking.rs:188` |
| Despawn the parent, guarding double-breaks | `breaking.rs:83`, `:98` |
| Rebuild the script name table after the set changes | `ScriptHost::sync_names`, called at `app.rs:504` and `simulate.rs:223` |
| Splice spawned entities back into a `--bake` as full entities | `crates/engine-cli/src/simulate.rs:691` |

So the missing piece is not the machinery. It is **the authoring question**: a break spawns
*pre-authored* fragments — the shapes are in the scene file, and the runtime only materializes
them. The likely shape of a spawn API is the same trade (spawn a named template that the file
declares, so the scene still says everything that can exist) rather than arbitrary construction
from script, which would put geometry in a `.rhai` and violate invariant 2.

**The open sub-questions**, none of which the breaking path had to answer: what names spawned
entities get and whether they collide (fragments get a derived name; ten thousand bullets need a
scheme), whether a template lives in the scene or its own file, what `inspect` and `validate`
say about something that does not exist yet, and how the determinism promise survives — the
collider set is an input to the broad phase (see `CLAUDE.md`'s Traps), so a spawn perturbs
every body in the scene by construction.

</details>

---

## 2. Hot reload

**The only still-open question in the design doc's §9**, and the one item here that is a decision
before it is a task.

Reloading scene data without a Rust rebuild is called out in §9 as "likely high value for agent
iteration speed" — and the agent feedback loop is the constraint that justifies every other
decision in this engine. It is also the one thing that could **reverse a settled decision**: hecs
was chosen over `bevy_ecs` primarily to limit churn exposure, and what that gave up was change
detection, "the one argument that could reverse this" (`CLAUDE.md`, Settled decisions).

**What already exists and what it implies.** `engine edit --watch` is a read-only supervision
mode over a scene file, and the editor is a live writable *view* onto the file (invariant 8), so
file-change plumbing is not the gap. The gap is what a *running simulation* does with a changed
file: adopt the edit and lose determinism from the step it landed on, or restart from step 0 and
re-run — which is cheap here, and may well be the right answer given that a run is a pure
function of (files, inputs, steps). **Deciding that it is the answer would close §9 without
building anything**, which is worth considering before treating this as a milestone.

Raise it rather than picking silently — `CLAUDE.md`'s "Open decisions — ask, don't assume".

---

## 3. Alpha-cut leaves

**A missing feature, not an authoring job.** `Tree::leaf_material`
(`crates/engine-core/src/components.rs:1865`) synthesizes the canopy surface from `leaf_color` and
`leaf_roughness` alone — "opaque and non-metallic by construction". Nothing an author writes can
reach it: a `Tree`'s own `Material` is its **bark**, and the leaves have no addressable surface.

So a leaf texture with a cut-out mask means new `Tree` fields, a schema regeneration, and a
validation pass — the ordinary shape of a component change, but a change to the component, which
is why no amount of scene editing gets there.

**Everything downstream is already built.** M26 shipped `alpha_cutoff` and the cut-out shadow
pipeline; the canopy is already emitted twice for both faces (`m19-trees.md`), which is the fold
that gives it texture *because* the engine has no leaf cut-out. This is a small, known-shape hole
that happens to gate how every outdoor scene reads — which is the only reason it outranks the
rendering backlog it otherwise belongs in.

---

## 4. A CPU wave evaluator, and therefore buoyancy

**Nothing can float.** Gerstner waves are displaced in the vertex stage (`shaders/water.wgsl`),
with normals from the analytic derivatives of the same sum. CPU displacement "was never close" —
a 192² grid is 37k vertices a step — so the surface exists only on the GPU, and no Rust code can
answer *where is the water at (x, z)*.

`m18-water.md` names both the absence and its remedy: `water.rs` is where the Rust mirror goes,
**with an agreement test**, when a boat needs one. That test is the interesting constraint — two
implementations of one curve is exactly the pattern `CLAUDE.md` warns about under the query
commands ("a generator that re-derives a curve is how two implementations start disagreeing"),
and here the duplication is unavoidable because one side must run on the GPU.

**Why it is structural rather than deferred polish.** It is the same shape as `terrain-height`,
which exists: the ground is queryable, so things stand on it, `FootPlant` works, and an agent can
ask where the ground is without reading a picture. Water has no equivalent, so it is scenery.
Buoyancy is the visible payoff, but the query command is the real one.

---

## Suggested order

1. ~~**Entity spawning.**~~ Built as M37, and the prediction held: the runtime half was already
   proven by M14, and what the milestone actually spent its time on was the authoring question.
2. **Hot reload**, as a *decision* first. It may cost a paragraph rather than a milestone.
3. **Alpha-cut leaves.** Small, known shape, disproportionate visual return.
4. **The wave evaluator.** Wants a real consumer — a boat, a buoy, something in the tour — before
   the duplicated-curve cost is worth paying.

Outside this list, the strongest rendering pick remains **shadow cascades**: one cascade is the
current limit (`m16-environment.md`), and cloud shadows come free with it.
