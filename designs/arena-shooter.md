# Arena Shooter

A top-down third-person shooter, built entirely out of what the engine already
has — no engine code was changed to make it. It is three files:

| file | what it is |
| --- | --- |
| `examples/scenes/arena_shooter.json` | the scene: arena, cast, HUD, lighting |
| `examples/scenes/scripts/topdown_shooter.rhai` | the game |
| `examples/scenes/make_arena.py` | emits the scene from a table of positions |

Plus `examples/scenes/arena_shooter_demo.input.jsonl`, a canned 15-second run
for headless rendering, and the M26 material set described under
[Surfaces](#surfaces).

## Playing it

```
bin/engine run-scene examples/scenes/arena_shooter.json
```

```
WASD    move        world-relative, the way a top-down game moves
Arrows  aim         independent of movement — twin-stick, on a keyboard
Space   fire
R       reload
```

Three waves of hovering drones home in on you: four, then three, then three.
Three bolts kill one, or shoot one of the four red barrels and the blast takes
everything within 7.5 m — and chains into any other barrel in range. Contact
costs 26 health per second. Clear all ten and the banner says so; run out of
health and it says the other thing.

Headlessly, with the recorded run:

```
bin/engine screenshot examples/scenes/arena_shooter.json --out /tmp/f.png \
    --steps 470 --input examples/scenes/arena_shooter_demo.input.jsonl \
    --width 960 --height 540
```

Same file, same steps, same input → the same bytes. There is no randomness in
the script, so the whole run is a pure function of those three inputs. (The
canned demo is a fixed key timeline, not an AI: it drives, shoots, blows up two
barrels, and eventually gets cornered and killed. That is the point of it — it
exercises every system in one render.)

## How it is put together

**`Player` is a physics proxy with no mesh.** A dynamic rigid body owns its own
`Transform`, so a script cannot turn one to face the aim. `PlayerBody`,
`PlayerHead` and `PlayerGun` are separate visual entities the script places from
the proxy's position every step, and the gun is turned with `world.look_at` —
which means the aim never needs an angle at all, only a point to look at. This
is M12's wheel pattern, one milestone's worth of precedent applied to a person
instead of a car.

**Bullets are not physics.** Fourteen mesh-only entities (no `RigidBody`, no
`Collider`) are recycled by the script and flown at 46 m/s. Hit detection is a
swept **segment**-to-centre test, not a point test: a bolt covers 0.77 m per
fixed step, so a point test would put it on one side of a drone in one frame and
the far side in the next. Keeping them out of rapier is also what makes a
sustained burst free — a hail of fire cannot disturb the arena it flies through,
and the pool never allocates.

The cost is stated rather than hidden: **bolts pass over cover**. Crates and
barriers stop bodies, not bullets. Making cover block fire needs the arena's
blocking boxes to exist somewhere the script can see them, and duplicating them
into the script would be a second source of truth for where the walls are.

**Every `Breakable` here is script-only** — none carries an `impulse_threshold`.
That is deliberate: the script tracks what is alive in `world.state`, and a
barrel that physics opened on its own would leave an alive flag pointing at an
entity that no longer exists, so the next `world.position` on it is a runtime
error. One owner of destruction, and it is the script.

**Drones hover** (`gravity_scale: 0`), which is what makes waves cheap. A
dormant drone is parked at y=46 and simply held at zero velocity; when its wave
starts, the same steering code flies it down and in. A script cannot move a
dynamic body by writing its `Transform`, so "spawn it later" was never
available — "it is already there, 46 m up" is.

**The arena is a plateau in a valley.** The `Terrain` patch is scenery only: it
carries no collider, and it sits low enough (`position.y = -7`, `height: 6`)
that its highest possible point is a metre *below* the fighting floor. Terrain
heights run `[0, height]`, so a patch placed at y=0 with 16 m of relief buries
whatever is standing on it — which is exactly what the first render of this
scene showed. Two further reasons the floor is a flat box and not terrain: a
body sliding on a trimesh hits the internal-edge bug M23 documents, and a
top-down shooter wants a floor whose geometry never argues with movement.

**Two nested loops, no more.** Rhai's *function* expression-depth limit is **16
in a debug build** (32 in release), and blocks are expensive against it —
measured on this engine, three nested blocks inside `fn step` is the ceiling, and
a `let x = world.something(...)` read survives only two. `bin/engine` is a debug
binary, so a script that nests one loop deeper compiles fine under a release
build and fails to *parse* under the one this repo actually uses. That is why
every inner loop in the script lives in its own function — a function body starts
the budget over — and why subexpressions are pulled out into locals rather than
nested. It reads like an over-cautious style rule and is not one.

## Surfaces

The arena shipped in flat colour. It is textured now, entirely with M26's
`Material` — again with no engine change. Four map sets were added to
`examples/textures/make_textures.py` beside the tour's, and three of the tour's
own are reused:

| surface | maps | why |
| --- | --- | --- |
| floor, floor paint | `deck` + `deck_normal` + `deck_orm` | a jointed poured slab; by far the largest thing on screen |
| perimeter walls, lamp posts, player, drone hulls | `plate_normal` + `plate_orm` | the tour's pressed steel, at four tilings |
| barriers | `concrete` + `concrete_normal` | cast concrete, chamfered and aggregated |
| barrels | `barrel` + `barrel_normal` | a hazard-striped drum |
| drone lens | `drone_eye` as `emissive_map` | the glow was the whole cube; now it is a lens |
| crates | `examples/materials/crate_wood.json` | the tour's crate boards, shared by eleven entities |
| trees | `examples/materials/bark.json` | the tour's bark, shared outright |

Five things came out of authoring it.

**A cube's faces disagree about `u` in pairs, not in axes.** The tour recorded
this as "vertical on ±X, horizontal on ±Z", which is half of it. `mesh.rs`
builds the six faces as `quad(+X, Y, Z)`, `quad(−X, Z, Y)`, `quad(+Z, X, Y)`,
`quad(−Z, Y, X)` — so the two faces *within* each pair are transposed against
each other as well. `u` is vertical on +X and horizontal on −X. That is why the
four perimeter walls carry four different `uv_scale`s: each is chosen for the
one face that points into the arena, and each of those four is a different face
of the cube. `side_uv` in `make_arena.py` takes the metres each of a *named
face's* own axes spans, rather than the box's dimensions, precisely so this
cannot be got wrong silently — a transposed wall tiles 39 panels up 3.2 m and
one panel along 62, which is unmistakable but only after you look.

**The floor's `uv_scale` swaps its arguments.** `builtin:cube`'s +Y face is
`quad(+Y, +Z, +X)`, so `u` runs along local +Z and `v` along +X. Passing
`(size_x, size_z)` straight through draws stretched bands on every floor
marking. `top_uv` does the swap once, and the paint decals reach it through the
same helper, so a 56 × 2.6 m lane tiles at the same 3.1 m as the concrete
around it.

**The barrels are the one map that carries its own colour.** The tour's rule is
that maps stay near-neutral because `albedo_map` is *multiplied* by `albedo`, so
a coloured map can only be tinted toward black. A hazard band is exactly the
case that rule cannot serve: black-and-yellow chevrons are a colour contrast,
and multiplying a bright band by a red barrel yields two shades of red, which is
not a warning stripe. So `barrel.png` owns its hue, the material tints it
near-white, and the file stays private to the barrels — nothing else can reuse
it, which is the cost of the exception, stated rather than discovered later.

**The eleven crates share a material file and the six barriers cannot.**
`Material.asset` is exclusive with every other field, so a shared file cannot be
tuned per entity. The crates are all the same wood at the same tiling and share
`crate_wood.json`; the barriers each need their own `uv_scale`, because a 12 m
one and an 11 m one are long along different axes. Same rule, opposite answer,
and it is the rule that decides — not a judgement about how alike they look.

**The paint takes the deck's normal map and not its albedo.** Paint follows the
slab it was rolled onto, and a marking is a separate box: its joints could never
line up with the floor's underneath. Relief at matching scale reads as one
surface; a second copy of the grime would read as two.

Deliberately still flat: the player's head, the bullets, and the drone
fragments. They are stand-ins the way the tour's critters are.

## Regenerating the scene

`make_arena.py` exists for the same reason `make_car_track.py` does: a bullet
pool is fourteen entities differing only in a name, and a drone is nine lines of
JSON repeated ten times. Everything worth tuning is a table at the top of the
file.

```
python3 examples/scenes/make_arena.py
```

It prints the constants the script has to agree with (drone count, bullet count,
barrel count, arena half-width, and which drone indices belong to which wave).
Those five numbers are the only coupling between the two files — change a table
and check them.

The emitted JSON is a normal, hand-editable scene; the engine never runs the
generator. This is a convenience, not a second source of truth.

## Not here

No baseline is committed for this scene and no CLI test pins it: it is a demo,
not a verification fixture, and it is not in `verify/baselines.json`. Also
absent, in rough order of how much each would add: cover that stops bullets,
enemies that shoot back, a restart (the engine cannot spawn entities, so a
cleared arena stays cleared), drone variety, and sound — of which the engine has
none.
