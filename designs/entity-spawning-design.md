# M37 — Entity spawning: a run can grow

`designs/structural-holes.md` §1 opens with the asymmetry: **a script can destroy an entity but
cannot create one, so a run can only ever shrink.** Everything the arena shooter is shaped around
follows from that one sentence — ten drones parked 46 m above the floor waiting for their wave, a
campaign that is four levels because endless would mean authoring endless drones, an end card with
no `RETRY`, a save that restores the campaign and never the arena, and no projectiles anywhere in
the engine.

This milestone closes it. The runtime half was already proven by M14 breaking, which mutates the
entity set mid-run deterministically; what was missing was **the authoring question**, and that is
what most of this document is about.

---

## 1. The shape: a template is a declared entity that is not instantiated

The rule this engine cannot bend is invariant 2 — everything needed to reconstruct a scene lives in
text files on disk. A `world.spawn_entity()` that took geometry, a material and a collider as arguments
would put scene data in a `.rhai`, and the scene file would stop being the whole truth about what
can exist.

So spawning takes the same trade M14 took: **the script names a template the file declares, and the
runtime materializes a copy.** A fragment list is that trade for breaking; `templates` is the
general form.

`templates` is a new top-level block, a sibling of `entities`, `physics`, `environment` and
`daylight`:

```json
{
  "name": "range",
  "entities": [ ... ],
  "templates": [
    {
      "name": "Bullet",
      "limit": 48,
      "components": [
        {"type": "Transform", "scale": [0.08, 0.08, 0.08]},
        {"type": "Mesh", "asset": "builtin:sphere"},
        {"type": "Material", "albedo": [1.0, 0.85, 0.3], "emissive": [1.0, 0.6, 0.1]},
        {"type": "RigidBody", "body": "dynamic", "gravity_scale": 0.0, "ccd": true},
        {"type": "Collider", "shape": "sphere", "radius": 0.08}
      ]
    }
  ]
}
```

A template entry is an `EntityDef` plus a `limit`. It is validated exactly as an entity is — same
schema walk, same ranges, same per-entity cross-component rules — and it is **not** spawned when the
scene loads. Nothing renders it, physics never sees it, `simulate` never traces it. It is a
declaration of a shape that can exist.

**Absent `templates` is the pre-M37 engine exactly**, byte for byte: no block, no spawn queue, no
extra pass, and the sim loop's new code is behind an `is_empty()` that a scene without templates
never leaves. That is the house rule M16 established and every milestone since has kept.

### Rejected: prefab files

`prefabs/*.json` referenced by relative path — the shape `Material.asset` has since M26 — is the
obvious extension and is **deferred, not rejected**. It buys reuse across scenes and costs a loader,
path resolution, and a second file `validate` must reach; none of that is needed to close the hole,
and M26's own history says the inline form should exist first. The field to add later is `asset` on
a template, exclusive with `components`, which is exactly what `Material` does.

### Rejected: spawning a copy of a live entity

`world.spawn_from("SomeExistingEntity")` needs no format change at all, which is its whole appeal.
It is rejected because it re-blesses the workaround: the thing being copied has to *be* in the
scene, lit, colliding and drawn, so authors would keep parking it 46 m up. A template is the
statement "this can exist and does not yet," and no live entity can make that statement.

---

## 2. `world.spawn_entity(template, x, y, z)` → the new entity's name

```rhai
let bullet = world.spawn_entity("Bullet", muzzle_x, muzzle_y, muzzle_z);
if bullet != "" {
    world.set_linear_velocity(bullet, aim_x * 40.0, aim_y * 40.0, aim_z * 40.0);
}
```

Four decisions are packed into that call.

**The engine names the instance, and hands the name back.** `Bullet#1`, `Bullet#2`, … from a
per-template counter that starts at 1 and **never resets or reuses within a run**, even after the
instance despawns. Reuse would make a `--trace` ambiguous — two different objects on the same row
name — and a monotonic counter is one integer of state that is a pure function of how many spawns
happened, so a replay reproduces it exactly. The suffix is `#` for the reason `.frag0` is `.`: the
character is not legal-looking in a hand-authored name, so a derived name is visibly derived.

Letting the *script* choose the name was the alternative. It puts collision-avoidance in every
script that spawns anything, and the first bug is two scripts choosing `bullet`. Returning the name
costs nothing and makes collisions unrepresentable.

**The spawn is immediate in the world and deferred in physics.** The entity exists in `hecs` before
`world.spawn_entity` returns, so the very next line can set its velocity, its rotation, its material — the
whole existing script API works on it with no new call. What it does *not* have yet is a rapier
body: those are inserted by the caller between the script step and the physics step, so a bullet
moves on the first step it exists rather than hanging in the air for one frame. That split is why
`ScriptHost` grows a `take_spawns()` drain next to `take_explosions()` rather than reaching into
physics itself — the scripting crate does not depend on the physics crate, and M13 and M14 both
declined to make it.

**The name table has an overlay.** `WorldApi.names` is an immutable `Rc<HashMap>` shared across
scripts, rebuilt once per entity-set change by `sync_names`. A live spawn cannot rebuild it
mid-step, so spawns land in a second `Rc<RefCell<HashMap>>` consulted after it, folded in and
cleared by the caller's `sync_names` after the step. Two maps rather than one mutable map because
the fast path — every lookup in every pre-M37 script — must not take a `RefCell` borrow it never
needed.

**Position is an argument because a template has no place.** Everything else about a spawned entity
comes from the template; where it goes cannot, because "somewhere else" is the entire point. The
three floats overwrite `Transform.position` and nothing else, so a template's `rotation` and `scale`
survive.

### `limit`, and what happens at it

`limit` is the maximum number of **live** instances of that template — spawned minus despawned —
and defaults to `64`. Spawning at the limit **spawns nothing and returns the empty string.**

This is the one place the milestone declines M10's usual "deterministic failure over a silent
no-op." A located error would be correct if hitting the cap were a bug, and it is not: a gun that
fires faster than its bullets expire is an ordinary game, and crashing the run is a worse answer
than not firing. The refusal is not silent either — it is a value the script reads, which is what
the `if bullet != ""` above is for. `world.spawn_count("Bullet")` answers the same question before
the call, for a script that would rather gate than check.

The cap exists at all because an uncapped spawn in a fixed-step loop is a footgun with a physics
bill: every instance is a collider, the collider set is an input to the broad phase, and a script
with an off-by-one spawns 900 of them and reports the sim as "slow." A number in the file is the
agent-legible form of that limit — `engine inspect` shows it, and a scene that wants ten thousand
bullets says so in the scene rather than discovering a hard-coded ceiling.

---

## 3. `world.despawn_entity(name)` — the symmetric half

Spawning without despawning turns `limit` into a countdown to a dead gun, so the two ship together.

```rhai
if world.position(bullet)[1] < 0.0 { world.despawn_entity(bullet); }
```

Queued, not immediate — applied at the same point in the step that breaks are, **after physics and
before the next step's scripts**, for M14's reason: the entity writes its final position into the
trace before it disappears, and nothing that runs later in this step finds a name that vanished
mid-step.

It works on any entity, authored or spawned. That is deliberate — the arena wants a drone gone
without shattering it, and `break_entity` cannot express that. One thing is refused: **despawning an
entity that carries a `Script`.** Scripts are compiled once at build and run from their ASTs, so a
script whose owner no longer exists keeps running against a name that is gone; that is the exact
class of bug that produces a runtime error two hundred steps later with no way back to its cause.
A located error at the call is the M10 treatment.

---

## 4. What a template may not contain

Five components are a validation error inside a template, each because spawning one would break a
rule the scene format already validates:

| Forbidden | Why |
|---|---|
| `Script` | Compiled at build from the world's `Script` components. A spawned one would never run, and silently-never-runs is worse than refused. |
| `Camera` | "At most one active camera" is a validated scene property. A spawn could make a valid scene invalid at step 40. |
| `DirectionalLight` | At most one per scene, same reason. |
| `AmbientLight` | At most one per scene, same reason. |
| `PointLight` | The ≤8 budget is a hard renderer limit (`MAX_POINT_LIGHTS`), and overflowing it drops lights silently rather than failing. |

The shape of the rule is what matters more than the list: **a template may not contain a component
whose scene-level budget validation could then be violated by a spawn.** `PointLight` is the one
that will be asked for — a muzzle flash wants a light — and reversing it needs a runtime answer to
the budget (an eviction policy, or a validated per-template reservation), which is a design, not an
oversight. It is listed in §9.

Everything else is allowed, including the geometry recipes. A spawned `Tree` or `Cloud` costs a CPU
mesh generation at spawn time and is a strange thing to want, but nothing about it is *wrong*, and
forbidding it would be inventing a rule to save a paragraph.

Cross-entity references from a template — a `Wheel`'s chassis, a `Meadow`'s or a `FootPlant`'s
terrain, a HUD element's parent — resolve against the **scene's** entity names, not other templates.
A spawned thing attaches to something that exists.

---

## 5. Validation, `inspect`, and what a not-yet-existing thing reports

Templates take the whole per-entity walk: unknown fields, JSON types, ranges, unknown components,
duplicate components, and every per-entity cross-component rule (`Water` forbids `Mesh`, a round
collider forbids nonuniform scale, and the rest). What they do **not** take are the cross-*entity*
budget passes — one camera, one sun, eight point lights — because the forbidden set above
guarantees a template contributes to none of them.

Five new error codes, all `Input` class:

- `template_not_object`, `missing_template_name`, `empty_template_name` — the entity codes' twins.
  Separate codes rather than reuse, because a machine branching on `missing_entity_name` should not
  have to re-read `path` to learn it was a template.
- `duplicate_template_name` — two templates share a name, **or** a template shares a name with an
  entity. One address space: `world.spawn_entity("Drone")` and `world.set_position("Drone", …)` must never
  be able to mean two different things.
- `template_forbidden_component` — §4, with the reason in the message.

`engine inspect` grows a `templates` array beside `entities`, every field resolved and defaults
filled in, exactly as entities are shown — so the answer to "what can this scene spawn, and what
will it look like" is a query rather than a read of the raw file. `engine validate` needs no new
flag. `engine simulate`'s report gains a `spawned` count, **omitted when zero**, so every report a
pre-M37 scene produces is byte-identical.

`--bake` needed nothing. It already splices runtime-spawned entities in as full entities
(`simulate.rs:691`) and removes ones that vanished, because M14 needed both; a baked scene therefore
comes back with its live bullets as ordinary entities and its `templates` block intact.

---

## 6. Determinism

The promise is unchanged and is per *file*: same scene, same inputs, same step count, same bytes.

Spawning does not weaken it, because every input to it is already deterministic — script order is
entity-name order, calls within a script are program order, the counter is a pure function of the
spawn count, and physics insertion happens in call order at a fixed point in the step. A replay of
the same `--input` timeline spawns the same instances with the same names in the same order.

What it *does* is make the scene a different scene. `CLAUDE.md`'s trap stands and this is its
sharpest case: **the collider set is an input to the broad phase and float addition is not
associative**, so the first spawned bullet perturbs every body in the arena by construction. A scene
that gains spawning re-blesses; that is not a regression, it is the same rule that says dropping one
5 cm sphere 200 m away moves six bodies by 4.4 mm.

---

## 7. Where it lands in the step

```
animations → scripts → [spawns enter physics] → physics → particles → [despawns, breaks] → render
```

Two new points, both chosen by an existing precedent:

- **Spawns enter physics right after the script step**, beside `take_explosions`, so a bullet moves
  on the step it was fired.
- **Despawns apply beside breaks**, after physics, so the last thing an entity does is write its
  final position into the trace.

The viewer (`app.rs`) and the headless loop (`simulate.rs`) both do this, in the same order, for
M28's reason: the two paths must be provably identical or a recorded input stops reproducing what
the window did.

---

## 8. Verification

- `verify/m37_spawn.json` + `m37_spawn.rhai`: a launcher that spawns, aims and despawns, run at a
  fixed step count against a committed baseline, with the CLI test that diff-renders it. It aims at
  its subject with **no terrain in frame** and renders at `samples: 1`, per the adapter rule.
- The showcase tour gains a `templates` block, because
  `showcase_tour_uses_every_scene_block_the_format_has` fails on block-level growth by design (M21
  added that test for exactly this).
- The arena shooter's weapons fire **real spawned projectiles** instead of hitscan — the demo the
  hole was costing, and the thing that proves the API is usable rather than merely present.
- No shader changed and no render path changed, so the pixel claim is `bin/verify-baselines` plus
  an A/B on the scenes that do not spawn. Scenes that do spawn re-bless, per §6.

## 9. Not here

- **Prefab files** (`prefabs/*.json`), §1 — the `Material.asset` shape, deferred with its field name
  already chosen.
- **`PointLight` in a template**, §4 — wants a runtime answer to the ≤8 budget.
- **`Script` in a template** — wants runtime compilation, which is a different milestone and is
  entangled with the still-open hot-reload question.
- **Spawning with a parent, or a spawn transform relative to another entity.** `world.spawn_entity` takes
  world coordinates; a muzzle-relative spawn is `world.forward()` plus arithmetic today.
- **Restoring a mid-level arena from a save**, which is what `CLAUDE.md`'s game-shell list wanted
  spawning for. It is now *possible* and is still its own piece of work: a save would have to record
  which drones were broken and which spawns were live.
