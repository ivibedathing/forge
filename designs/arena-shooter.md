# Arena Shooter

A top-down third-person shooter, built entirely out of what the engine already
has — no engine code was changed to make it. It is three files:

| file | what it is |
| --- | --- |
| `examples/scenes/arena_shooter.json` | the scene: arena, cast, HUD, lighting |
| `examples/scenes/scripts/topdown_shooter.rhai` | the game |
| `examples/scenes/make_arena.py` | emits the scene from a table of positions |
| `examples/meshes/make_rigged_soldier.py` | the player rig: 17 joints, `Idle` + `Run` |
| `examples/meshes/make_weapons.py` | the three weapon meshes |

Plus `examples/scenes/arena_shooter_demo.input.jsonl`, a canned 15-second run
for headless rendering, and the M26 material set described under
[Surfaces](#surfaces).

## Playing it

```
bin/engine run-scene examples/scenes/arena_shooter.json
```

```
WASD    move        world-relative, the way a top-down game moves
Mouse   aim         the gun points at the ground under the cursor
Click   fire
1 2 3   pistol / rifle / shotgun
R       reload
Esc     pause
```

The run opens on a main menu and pauses to one — a `HudPanel` tree the engine
lays out and hit-tests, described under [The menu](#the-menu) below. It has
four screens now: PLAY, SETTINGS, LOAD GAME and QUIT on the title card, and
RESUME, SAVE GAME, SETTINGS and QUIT on the pause card.

Three waves of hovering drones home in on you: four, then three, then three.
Three bolts kill one, or shoot one of the four red barrels and the blast takes
everything within 7.5 m — and chains into any other barrel in range. Contact
costs 26 health per second.

Clear all ten and the card offers **NEXT LEVEL**. There are four, and each one
is faster (+14% closing speed), tougher (+33% hull — three bolts to a kill, then
four, five and six), harder-hitting (+25% contact damage) and worth more (+50%
score) than the one before it, and each
drops in fewer new barrels than the last: four, three, two, one. A barrel you
did not shoot stays live into the next level, so saving one is worth something.
Clearing a level hands back 45 health, and the score carries. Run out of health, or clear the fourth level,
and the card says so and stops — see [The campaign](#the-campaign).

Headlessly, with the recorded run:

```
bin/engine screenshot examples/scenes/arena_shooter.json --out /tmp/f.png \
    --steps 470 --input examples/scenes/arena_shooter_demo.input.jsonl \
    --width 960 --height 540
```

Same file, same steps, same input → the same bytes. There is no randomness in
the script, so the whole run is a pure function of those inputs — **plus the
frame size, which the mouse added**: a cursor is a fraction of the frame and
the ray through it depends on the aspect, so render the demo at 16:9 or it
aims somewhere else (`designs/mouse-input-design.md` §5).

The canned demo clicks PLAY, then fights: it clears level 1, presses NEXT
LEVEL, and fights on into level 2. It is a choreography, not an AI — see
[Regenerating the demo](#regenerating-the-demo).

## How it is put together

**`Player` is a physics proxy with no mesh.** A dynamic rigid body owns its own
`Transform`, so a script cannot turn one to face the aim. `PlayerModel` is a
separate visual entity the script places from the proxy's position every step
and turns with `world.look_at` — which means the aim never needs an angle at
all, only a point to look at. This is M12's wheel pattern, one milestone's
worth of precedent applied to a person instead of a car.

**The player is a rig** (M36), where a cylinder and a sphere used to stand in
for a person: seventeen joints out of `examples/meshes/rigged_soldier.gltf`,
with `Idle` and `Run`. Three things about it are worth knowing.

- **Its arms do not swing**, unlike the tour walker's. `HandR` is forward of
  the chest in the bind pose and stays there through `Run`, so the legs do the
  running and the arms are a rigid brace. A gun on the end of a swinging arm
  points somewhere new every frame, and the script's aim would stop describing
  the picture.
- **The clip is a hard cut**, `world.set_animation_clip` between `Idle` and
  `Run`. Blending is a standing non-goal (M9 §8, restated by M30 and M32), so a
  change of gait is a change of clip — and the two clips differ in *shape*
  rather than in rate, which is what makes the cut worth having.
- **`Run` carries a measured `stride` of 2.7664 m** (M32), so the legs are
  paced by the ground the player actually covers and strafing into a wall does
  not moonwalk. The number came out of `engine list-joints`, not out of the
  swing angle; `make_rigged_soldier.py` prints the command that re-measures it.

**The whole body turns to the aim, legs included.** A twin-stick game usually
turns the legs to movement and the torso to the aim, and that is a per-joint
override — which M30 does not have, deliberately (a script-settable joint is
hidden state, M21's reason). A top-down shooter reads as aiming with the whole
body anyway.

**There is no `FootPlant` and no `SkinnedCollider` on the player**, and both
omissions are decisions. `FootPlant` plants against a `Terrain` and this floor
is a box for the two reasons above; the floor is flat and level, so what it
fixes does not arise. Proxies would be eleven kinematic bodies shoving the
drones around in a game tuned against a capsule, and M33 measured that adding a
collider anywhere perturbs every body in the scene — the capsule is what stops
a drone, and that is enough.

**Three weapons, hung off the hand.** `world.joint_position("PlayerModel",
"HandR")` places the held one every step and `look_at` aims it, which is M30's
sanctioned way to hold a prop and the first use of it in the repo. The other
two park at `y = -30`, where a spent bullet already goes: a `Mesh` has no
visibility flag, and adding one would be a component change to solve a script's
problem.

| weapon | interval | damage | magazine | reload | pattern |
| --- | --- | --- | --- | --- | --- |
| pistol | 0.20 s | 34 | 12 | 1.1 s | one bolt, no spread |
| rifle | 0.09 s | 24 | 30 | 1.6 s | one bolt, a walking cone |
| shotgun | 0.62 s | 21 × 5 | 6 | 1.9 s | five pellets at ±10° |

The pistol is the gun that shipped with one number moved — 0.13 s between shots
became 0.20, so the rifle has somewhere to be faster — which keeps the opening
fight the one that has always been here. **Spread is a fixed pattern, never a
random one**: a script has no randomness at all (M10, and it is load-bearing —
a recorded timeline has to replay to the same pixels), so a random cone was
never available, and a fixed one is better for a demo because the same click
kills the same drone every replay. Damage rides on the *bolt* rather than on
the weapon, so switching guns does not retroactively re-arm rounds in flight.
The bullet pool grew from 14 to 24 for the shotgun's five-at-once.

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
dormant drone is parked above the arena and simply held at zero velocity; when
its wave starts, the same steering code flies it down and in. A script cannot
move a dynamic body by writing its `Transform`, so "spawn it later" was never
available — "it is already there, 46 m up" is. [The campaign](#the-campaign)
is that same sentence applied one level up, which is why it is four levels
rather than endless.

**The arena is a plateau in a valley.** The `Terrain` patch is scenery only: it
carries no collider, and it sits low enough (`position.y = -7`, `height: 6`)
that its highest possible point is a metre *below* the fighting floor. Terrain
heights run `[0, height]`, so a patch placed at y=0 with 16 m of relief buries
whatever is standing on it — which is exactly what the first render of this
scene showed. Two further reasons the floor is a flat box and not terrain: a
body sliding on a trimesh hits the internal-edge bug M23 documents, and a
top-down shooter wants a floor whose geometry never argues with movement.

**Grass grows at the foot of the plateau, and only there.** Four `Meadow`
strips (M29) ring the arena on the `Landscape` patch. It is a ring rather than
a field because the camera looks *down* from 20 m and the only ground it ever
sees past the walls is the band immediately outside them — grass further out is
plants nobody renders. Density is the number that decides whether this is
scenery or a stall: the component counts plants per square metre of footprint,
so a strip is `size_x * size_z * density` plants, and M29's budget is per entity
and counted in triangles. Measured here: 64 triangles a plant, ~20k plants in a
long strip and ~12k in a short one, about 4M triangles over the four — and no
measurable cost to the frame, since a meadow is two static buffers and a vertex
shader. There is no water, and the reason is in the terrain: the basins near
enough to see are barely a metre deep across twenty-six, so a `Water` surface
laid in one reads as a pale puddle draped over a slope rather than as a pond.
Making one would mean reshaping `Landscape`, which is a change to the whole
scene for something at the edge of the frame.

**The aim is a point, not an angle.** `world.cursor_ground(player_y)` returns
where the pointer's ray meets the plane the player stands on, and the aim is
just the direction from the player to that point — normalized, with a 0.7 m
deadzone so a cursor sitting on the player does not spin the gun on noise.
There is no slew and that is deliberate: the arrow keys had one because a held
key is a *rate* and something has to integrate it, while a cursor is already a
position. Easing toward it would be lag with no gameplay behind it.

**A paused game is `gdt = 0`, not a branch.** Everything that moves reads a
game clock that is `world.dt()` while playing and zero otherwise, so the menu
freezes bullets, cooldowns, reloads, drones and the camera by arithmetic. The
one thing that needs a line of its own is the player's velocity: the blend
toward the target speed is a no-op at `gdt = 0`, so a pause would leave the
player coasting. Wrapping the whole body in `if playing { … }` was not
available anyway — see the depth note below.

**Two nested loops, no more.** Rhai's *function* expression-depth limit is **16
in a debug build** (32 in release), and blocks are expensive against it —
measured on this engine, three nested blocks inside `fn step` is the ceiling, and
a `let x = world.something(...)` read survives only two. `bin/engine` is a debug
binary, so a script that nests one loop deeper compiles fine under a release
build and fails to *parse* under the one this repo actually uses. That is why
every inner loop in the script lives in its own function — a function body starts
the budget over — and why subexpressions are pulled out into locals rather than
nested. It reads like an over-cautious style rule and is not one.

## The campaign

Four levels, and the shape of them is decided by one engine rule: **a scene
cannot spawn entities, and a script cannot move a dynamic body by writing its
Transform.** So a level's enemies cannot be created when it starts, and a dead
one cannot be picked up and put back. What is left is the trick the dormant
waves have used since the beginning — *it is already there, 46 m up* — applied
one level higher.

Every level's ten drones and its barrels are in the file from the start, parked
above the arena, and fly or drop in when their level begins. That is why the
campaign is four levels rather than endless: the number is in the scene, and
`make_arena.py` writes it.

**Levels are contiguous runs of a flat numbering.** Level *n* owns
`Drone{(n-1)*10+1}`..`Drone{n*10}`, and its barrels are the next run of a list
that shortens as the campaign goes on. So the script gets from a level to its
drones with `base = (level - 1) * 10`, and everything downstream — the hit test,
the blast radius, the wave counter — keeps working in indices local to the
level. The generator prints the boundaries; they are the only coupling.

**Drones are scoped to their level and barrels are not.** Every barrel up to and
including this level's stays armed, because one left standing from last level
looks exactly like a live one, and an inert twin of a thing that explodes is the
worst answer available. It also means saving a barrel is worth something.

**Each level parks four metres above the one before it**, and that is not
tidiness. The levels reuse each other's positions — the drone ring is the same
ten points turned a quarter turn per level, and a level's barrels are the first
N of one list — so a shared park altitude puts two dynamic bodies inside each
other and physics spends the whole run shoving them apart. The first run of
this showed parked drones metres out of position before their level had begun.

**A barrel has `gravity_scale: 0` and is driven down by the script.** It has to
be: a barrel waiting three levels out would otherwise sag out of the sky, and
even at rest nothing would hold it against the blast that opens its neighbour.
`settle_barrels` drives it down at a speed that eases off over the last metre
and then holds it at zero velocity — a drop is a velocity or it is nothing.

**Difficulty is three dials, not one, and two of them are pinned to something
real.** Hull alone makes a level *longer*; speed and contact damage make it
*tighter*. Hull steps so the bolts-to-a-kill count lands on 3, 4, 5, 6 rather
than on a fraction — a magazine that runs dry in a different place each level
for no visible reason is worse than a harder one. Closing speed stays under the
player's own 7.2 m/s at level 4 (6.3), because a drone that outruns the player
cannot be kited and the arena stops being a place to move through. Score scales
with the dials, so a level-4 kill is worth more than a level-1 one, and each
level brings fewer new barrels because cover that never runs out is not a
difficulty curve. Every step is a multiple of
`level - 1`, so **level 1 is arithmetically the game that shipped** — the
identity, not a tuned approximation of it.

**Mode 4 is the new state**, and it is the difference between a card that
reports and a card that offers. Mode 3 is now only the two ends nothing follows:
the player died, or the fourth level is clear. Both still have no button, and
now for a reason that survives the campaign — either one wants forty broken
drones and ten spent barrels back.

## The menu

Five screens, and since M31 they are a component tree the engine lays out and
hit-tests rather than rectangles the script computed and tested itself.

| screen | buttons | how it ends |
| --- | --- | --- |
| title | `PLAY` · `SETTINGS` · `LOAD GAME` · `QUIT` | a button |
| pause | `RESUME` · `SAVE GAME` · `SETTINGS` · `QUIT` | a button, or Esc again |
| settings | six rows that cycle their value, then `BACK` | `BACK` |
| level cleared | `NEXT LEVEL` | the button |
| end | none | nothing — see below |

The card is a `HudPanel` anchored `center`, holding a nine-sliced `HudImage`
stretched over it and a `column` panel of title, line and **seven button
slots** (M36). The script labels the slots a screen uses and blanks the rest;
an empty label hides a slot, and a hidden element leaves the flow entirely, so
each card is exactly as tall as the screen it is showing. Nothing in the scene
knows what a screen is. Seven, because the settings screen is the widest.

**The column takes an explicit width and hugs only its height**, and the reason
is a bug that took a render to see. M31's rule is that a stretched child
contributes nothing to a hugging parent — and every child here stretches except
the title, so a hugging column is exactly as wide as its *title*. That is fine
for `ARENA SHOOTER` and wrong for `SETTINGS`, whose eight glyphs left six rows
of text hanging out over both edges of their own buttons. Fixing the width
costs the cards their three different *widths* and keeps the property that
actually mattered: the height still hugs, so the end card still closes up
around its missing buttons.

**The title card is authored to be exactly what the script paints on step 1**,
which is load-bearing rather than tidy. A card that grows when the script first
paints it moves every button in it, and `engine ui-layout` reports the *rest*
layout — so a demo timeline aiming at the rect the engine reports would click
through empty space one step later. Found exactly that way; there is now a CLI
test asserting the two layouts agree.

**What the script kept is which words are on screen.** What it lost is a
`menu_rect` it both drew and hit-tested, an `inside` of four comparisons, and a
centring multiply per string — `len * 40.0` for a 40-pixel title, which is the
8×8 font's advance restated in a script that had no other reason to know it.
`show_menu` is now six setters and `hide_menu` is two.

**The button is a `HudInteract`, so the engine decides what a click is.** The
hit box is the panel's laid-out rectangle; `world.clicked("MenuButton")` is
true for exactly one step; and the hover and press states are `hover_tint` and
`press_tint` multiplying the panel's own colour. The old menu faked hover by
putting brackets around the label (`[ PLAY ]` versus `  PLAY  `) because the
script had no colour to set — that is gone, and so is the label churn.

This is stricter than what it replaced, in one way worth knowing: the old test
was a press edge anywhere inside a rectangle, and M31's is a **press capture** —
press and release must both land on the element. A timeline that presses on the
button and releases somewhere else no longer clicks, which is why
`make_arena_demo.py` writes the release at the button too.

**Hiding is `visible` now**, one field on every kind of element, where the old
script hid
a text by emptying its string and a rect by sizing it to zero — two spellings of
one idea, and neither of them sayable in the scene file. The play HUD is
authored *hidden*, so `screenshot --steps 0` shows the title screen and the file
says what the first frame is; the script shows it when the run starts. Hiding a
panel hides its subtree, which is what makes closing the menu two calls.

**The end card has no button, and the card closes up around it.** A restart
would have to put ten broken drones and four broken barrels back, and the engine
cannot spawn entities — so a cleared arena stays cleared, and offering `RETRY`
would be offering something that cannot work. An empty label hides the button
panel, a hidden element leaves the flow entirely, and the card is shorter by
exactly the gap.

**There is no full-frame dim behind the card, and that is a performance fix.**
The menu used to darken the whole screen with a stretched `HudRect`, which
defeats M15's central optimisation: the CPU HUD rasterizer fills only the pixels
HUD elements actually cover, and one element covering everything puts it back to
filling a window-sized canvas every frame. Measured on this scene, six frames at
1920×1080: **13.1 s with the veil against 6.7 s without** — about a second of CPU
per frame in a debug build, which is what `bin/engine` runs. The viewer steps
physics through a wall-clock accumulator, so a frame that slow asks for sixty
steps on the next one and the game stops responding to input altogether. The
card carries its own dark backdrop instead (a `HudPanel` *is* its own backdrop,
which is why this costs no extra component), and the same six frames now take
5.7 s. **A full-screen HUD element is the one shape this engine's overlay cannot
afford**; anything wanting a real dim wants the renderer to draw solid rects on
the GPU, which is an engine change with its own A/B.

**One recorded timeline still clicks the button at any frame size**, though
not for the reason the old menu gave. The card is centre-anchored and its
contents are pixel-sized, so its rectangle moves with the frame: the button
spans 0.554–0.635 of the height at 960×540 and 0.58–0.70 at 640×360. The demo
clicks the centre of the rectangle the engine reports at 960×540, which is
inside both — checked by replaying the same timeline at 640×360, where the run
starts exactly as it does at the authored size. What does give way below about
700 pixels wide is the top row: `SCORE` and `WAVE` are 24-pixel text anchored to
opposite corners and they meet in the middle. That is the pixel HUD's own limit
rather than the layout's, and the demo is authored at 960×540.

**The layout is answerable without running the game**, which the hand-rolled
version was not: `engine ui-layout examples/scenes/arena_shooter.json --width
960 --height 540` reports every rectangle, and `--entity MenuBtn1` reports the
one the demo has to click. That is where `make_arena_demo.py` gets it.

**And answerable *while* running it, since M36.** `ui-layout --steps N
[--input f]` replays first and reports the layout the run reached — M32's
`list-joints --steps` argument applied to a menu, for the same reason: which
slots a screen uses is what the script painted, not what the file says, and the
`NEXT LEVEL` card is a different height from the title card so its button is
somewhere else. The demo director asks for it rather than reusing the title
screen's rect.

## Settings, saves and quitting

The three things M36 added to the engine, and what the game does with them.

**Settings are ordinary `world.state` keys**, which is what makes them saveable
for free: a save is the whole map, so it carries them without knowing they
exist. Every default is the value the scene file authors, so a run that never
opens the settings screen is arithmetically the game that shipped.

| row | values | reaches |
| --- | --- | --- |
| DIFFICULTY | NORMAL / HARD / BRUTAL | the three drone dials |
| CROSSHAIR | ON / OFF | `set_hud_visible` |
| HUD | FULL / MINIMAL / OFF | `set_hud_visible` on four groups |
| LIGHTS | OFF / LOW / FULL | `set_light_intensity` on the four floodlights |
| SHADOWS | ON / OFF | `world.set_shadows` |
| QUALITY | LOW / HIGH | `world.set_samples` + `world.set_fog` |

**Difficulty moves hull and contact damage hard and closing speed barely**, and
that asymmetry is the one number here worth defending. At level 4 the drones
already close at 6.3 m/s against the player's 7.2, so a difficulty that scaled
speed like it scales hull would put them *ahead* of the player and the arena
would stop being a place to move through. 6% a step keeps BRUTAL level 4 at
7.05 — still, just, kiteable.

**LIGHTS drives the floodlights and not the sun**, because the arena's sun is
synthesized by its `daylight` block and M21 deliberately has no clock setter (a
script-settable time of day is hidden state). Nothing here reverses that.

**A save is the campaign, not the arena.** It restores level, score, health and
the settings; it does not restore which drones were broken, because the engine
cannot spawn an entity and a broken drone cannot be put back — the arena
shooter's oldest constraint, and the reason its campaign is four levels rather
than endless. So **LOAD GAME is offered on the title card only**, where every
drone is still intact and "level 3" means level 3's ten flying down from park
altitude. Loading mid-run is refused by the game rather than by the engine.

One subtlety the first run of it found: a load replaces the whole state map,
`mode` included — and the save was taken from a pause screen, so it says
*paused*. The script forces the run to start playing on the level that came
back, which is three lines and would otherwise load you into a menu you cannot
see behind.

**QUIT is `world.quit`** on both cards. Under the recorded demo timeline it is
never pressed.

**Two things the play HUD gained from the same components.** The health readout
is a `column` of label over gauge, the gauge a panel whose `padding` *is* its
bezel — three hand-kept numbers (a 224-wide back, a fill inset 3 px into it at
218, a label offset 30 px above both) collapsed into one authored width. And the
fill fades green through amber to red with `set_hud_color`, as the ammo counter
does when the magazine is nearly out; a wave announcement is one `HudText` the
script turns on for a second and a half.

**One authoring trap, found by rendering it.** The reticle is `ui_icon.png`
drawn at its own 16 px, and it has to be: a `HudImage` with no `slice` is all
middle band, and the middle band **tiles**. Asking for 32 px of a 16 px icon
does not draw it twice as large, it draws four of them — which is exactly what
the first render showed, a 2×2 of rings where the crosshair should be.

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

Deliberately still flat: the bullets and the drone fragments. They are
stand-ins the way the tour's critters are. The player and its weapons take the
tour's pressed-steel maps (`plate_normal` + `plate_orm`), which makes the player
the repo's second **skinned × textured** draw after the tour's walker.

## Regenerating the demo

`make_arena_demo.py` writes `arena_shooter_demo.input.jsonl`. It exists
because the mouse made the old approach impossible: fourteen hand-written
lines could say "hold ArrowUp for two seconds", but nobody can write down
which *pixel* is on a drone at step 431. So the timeline is authored the way
`make_car_track_lap.py` authors the car's lap — a closed loop that replays
what it has written so far, asks `engine simulate` where everything ended up,
projects the nearest live drone onto the frame, and appends the next tenth of
a second:

```
python3 examples/scenes/make_arena_demo.py
```

Three things about it. The projection is **a second implementation of the
inverse of `world.cursor_ground`**, which this repo normally refuses; it is
taken deliberately, because the alternative is a demo that shoots at nothing,
and it is checked by the outcome the script prints (a drifted projection kills
no drones) rather than by an agreement test. And **it finds out what it killed
from the engine's own error**: `--entity Drone03` on a drone that has already
broken is `entity_not_found` with the name in it, so the director drops that
name and asks again.

And **where the PLAY button is comes from `engine ui-layout`**, not from a pair
of hand-tuned fractions. It used to have to be hand-tuned: the menu was
rectangles the game script computed at run time, so nothing outside that script
could say where the button was. Now the director asks for `MenuButton`'s
rectangle at the frame it is authoring for and clicks its centre — the same
crossing between a pixel report and a fractional timeline that M31's own CLI
test pins.

**The director plays the campaign, and it reads the level boundary out of the
same signal it reads a kill from.** A broken drone leaves the `simulate` report
entirely; when every one of a level's ten has left it and none of them is still
up at park altitude, the level is over, the card is up, and the button that was
PLAY is now NEXT LEVEL — so the director presses it in the same place and starts
watching the next ten names. It then waits half a second for them to fly down,
because a director aiming at an arena whose drones are still 50 m up wanders off
to the middle of the floor and shoots at nothing.

## Regenerating the scene

`make_arena.py` exists for the same reason `make_car_track.py` does: a bullet
pool is fourteen entities differing only in a name, and a drone is nine lines of
JSON repeated forty times now that the campaign wants four levels of them.
Everything worth tuning is a table at the top of the file.

```
python3 examples/scenes/make_arena.py
```

It prints the constants the script has to agree with: the level count, drones
per level, bullets, barrels per level, the arena half-width, the two HUD numbers
— and, level by level, which drone and barrel names each one owns. Those are the
only coupling between the two files; change a table and check them.

The emitted JSON is a normal, hand-editable scene; the engine never runs the
generator. This is a convenience, not a second source of truth.

## Not here

No baseline is committed for this scene and no CLI test pins it: it is a demo,
not a verification fixture, and it is not in `verify/baselines.json`. Also
absent, in rough order of how much each would add: cover that stops bullets,
enemies that shoot back, a restart or an endless mode (both want entities the
file does not carry — the campaign is four levels because four levels' worth of
drones are authored), drone variety, a walking enemy off M30's rig, weapon
pickups (the campaign's "it is already there" trick would work for them and
nothing asked), more than one save slot, and sound — of which the engine has
none. The menu is deliberately still 8×8 text:
a bitmap-font atlas is the sanctioned next step for that everywhere in the repo,
not something to solve once here.
