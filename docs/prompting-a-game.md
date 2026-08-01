# Prompting for a game

How to ask a coding agent to build a game in this engine.

The short version: **a game is not a prompt, it is a stack of layers, and each
layer is a prompt whose result you can look at before the next one exists.** The
engine is built for that — `validate`, `screenshot`, `simulate`, `list-colliders`
and `ui-layout` all exist so that a layer can be *checked* rather than assumed.
A prompting style that skips the checking throws that away.

This document is about the asking. `AGENTS.md` (also `engine agent-guide`) is
about the engine.

---

## 1. Why one prompt does not work

"Build me a top-down shooter with waves, an inventory, upgrades and a boss" is a
reasonable sentence and a bad prompt. Three things go wrong, every time:

- **You cannot tell which part is broken.** The agent hands back a scene, six
  scripts and a menu. The drones do not move. Is that the steering, the wave
  gate, the pause clock, or a validation error nobody read? You now debug four
  systems at once, and so does the agent.
- **Early guesses get baked into everything downstream.** If the agent decides
  bullets are rigid bodies in minute one, waves, cover and the score all get
  built on that. Changing it later is a rewrite, not an edit.
- **Everything is negotiable, so nothing is settled.** With no fixed floor under
  it, the agent re-litigates decisions it already made, and the tenth prompt
  quietly undoes the third.

Layering fixes all three. Each layer ends with something you can run, look at,
and *freeze*. The next prompt starts from a working thing, not from a plan.

The rule of thumb: **if a prompt's result cannot be verified by one command and
one look, it is two prompts.**

---

## 2. The shape of a good prompt

Three parts. Two of them are the ones people leave out.

```
GOAL         what should be true afterwards
CONSTRAINT   what must not change, or must be reused
VERIFY       the command you will run, and what you expect to see
```

Weak:

> Add enemies.

Strong:

> Add four hovering drones that fly toward the player at 4 m/s and stop 1 m
> short. Do not touch the player script or the arena scene layout — drones go in
> their own entities and their own script. I should be able to run
> `engine simulate arena.json --steps 300 --entity Drone1` and see Drone1's
> position within 2 m of the player's start.

The VERIFY clause is what turns a prompt into a contract. It is also the part
that makes the agent stop and *check*, instead of reporting success from the
fact that the file was written.

**Say what to leave alone.** In a layered build, most of a prompt's value is the
constraint. "Don't change the movement" is worth more than three sentences of
description.

---

## 3. The layers

Build in this order. Each one assumes the ones above it work and are frozen.

| # | Layer | The question it answers |
|---|-------|-------------------------|
| 0 | The loop | Can I see anything at all? |
| 1 | The space | Where does this happen? |
| 2 | The verb | What is the one thing the player *does*? |
| 3 | The opposition | What pushes back? |
| 4 | Feedback | How does the player know what is happening? |
| 5 | Systems | Inventory, health, levelling, economy |
| 6 | Content | Waves, levels, quests, scenarios |
| 7 | The shell | Title, pause, death, next level |
| 8 | Feel | Particles, lighting, sound of the thing |

You can reorder 5 and 6 for some games. You cannot move 2 — the verb comes
before everything that decorates it.

---

### Layer 0 — the loop

Before any game exists, prove the pipeline: a scene renders, and you looked at
it.

> Run `engine init` in `games/roguelike/`, then screenshot the starter scene and
> show me the PNG.

That is the whole prompt. It takes one minute and it catches "no GPU adapter",
"wrong working directory", and "the agent is authoring blind" before any of them
can be mistaken for a game bug.

---

### Layer 1 — the space

One scene file. Ground, walls, a camera, a light. **No gameplay.**

> Make `arena.json`: a flat 40×40 m floor at y=0, four walls 2 m high around it,
> a camera 20 m up looking straight down, and a directional light. Sky and
> shadows on. Screenshot it at `--steps 0` and show me the image.

What to check yourself: is the whole floor in frame, is it lit, does the scale
look right. A camera that misses the arena by 5 m will cost you an hour in
layer 3 if you accept it now.

**Freeze it.** From here, "do not change the arena layout" is a constraint you
repeat.

---

### Layer 2 — the verb

The single action the game is about. Move. Shoot. Jump. Place. Talk. **One.**

> Add a player: a dynamic rigid body capsule at the arena centre, and a script
> `player.rhai` that moves it with WASD at 7 m/s, world-relative. Nothing else —
> no shooting, no camera follow, no HUD. Record a short input timeline and show
> me a filmstrip of the player crossing the arena.

Two things this layer must settle, because everything later depends on them:

- **How the player is represented.** In this engine, a script cannot rotate a
  dynamic body by writing its `Transform`, so the arena shooter splits the
  player into a physics proxy plus separate visual entities placed each step.
  Get that decision made and looked at now.
- **What the units are.** Speeds in m/s, sizes in metres. One unit is one metre.

If the verb does not feel right, *stop and fix the verb*. A game whose core
action is mushy does not get better when you add an inventory to it.

---

### Layer 3 — the opposition

The thing that makes the verb matter. Enemies, gravity, a timer, a puzzle state.

> Add three drones that hover at 1.5 m and steer toward the player at 4 m/s.
> They do not shoot and cannot be killed yet — this layer is only the movement.
> Keep them in their own script. Verify with
> `engine simulate arena.json --steps 240 --entity Drone1 --entity Drone2`.

Notice the "cannot be killed yet". **Explicitly deferring the obvious next thing
is a prompting technique.** Without it the agent builds hit detection, a health
pool and a death animation, and now you cannot tell whether the steering works.

Then, and only then:

> Now make them killable: three hits from the player's bolt. Do not change the
> steering.

---

### Layer 4 — feedback

The player has to see state. In this engine that is the HUD, and a `HudPanel`
tree the engine lays out for you.

> Add a HUD: health as a bar top-left, ammo as text bottom-right, score
> top-right. Use `HudPanel` for grouping so I am not hand-computing offsets.
> Show me `engine ui-layout arena.json --width 1280 --height 720` and a
> screenshot.

Build feedback *before* the systems it reports on. A health bar with a fake
number in it is five minutes of work and it makes layer 5 debuggable — you can
watch the number instead of reading a trace.

---

### Layer 5 — systems

This is where most people try to one-shot, and it is where layering pays most.

A system is a **state machine plus a place to keep the state**. Prompt each one
separately, and say where the state lives.

#### Inventory

Prompt it in three steps, not one:

> **(a)** Add an inventory model to the player script: 8 slots, each holding an
> item id and a count, kept in `world.state` numeric keys. No UI, no pickups —
> just the model, plus a HUD line printing the contents so I can see it.

> **(b)** Add pickups: five entities on the floor that are removed and added to
> the inventory when the player walks within 1 m. Do not change the model.

> **(c)** Add the inventory panel: a `HudPanel` grid of 8 slots showing the
> counts, toggled with `Tab`. Do not change the model or the pickups.

Three prompts, three things you can look at. One prompt for all three and a
pickup bug looks exactly like a UI bug.

**Say where state lives.** This engine has a hard rule — no hidden state; a
scene reconstructs from text on disk. Per-run memory belongs in `world.state`,
persistent facts belong in component fields. Being explicit about which is which
saves a rewrite:

> Ammo is per-run, so keep it in `world.state`. The weapon the player is holding
> should survive a bake, so make it a field on an entity rather than script
> memory.

#### Health and damage

> Give the player 100 health in `world.state`. Drone contact costs 25 per second
> while touching. Wire the existing health bar to it. Nothing dies yet — at zero
> health, just print `DEAD` to the HUD.

Death, respawn and the death screen are layer 7. Keep them out.

#### Levelling and progression

Progression is three numbers and a curve, and the curve is the part to argue
about in its own prompt:

> Add XP: 10 per drone killed, kept in `world.state`. Level up at 100 XP,
> doubling each level. On level up, add 20 max health and print it to the HUD.
> Show me a run where I reach level 3 and tell me what step each level landed
> on.

Then tune separately:

> The level 2→3 gap feels long. Make the curve 1.6× instead of 2×, and nothing
> else.

**Difficulty is several dials, not one.** The arena shooter scales enemy speed,
hull and contact damage independently, because hull alone makes a level *longer*
where speed makes it *tighter*. Prompt for the dials, then prompt for the
values.

---

### Layer 6 — content

Systems are the rules; content is the arrangement of them. This is waves,
levels, scenarios, quests.

#### The engine rule that shapes all of it

**A scene cannot spawn entities, and a script cannot move a dynamic body by
writing its Transform.** So "create ten enemies when level 2 starts" is not
available. The arena shooter's answer is the pattern to reuse: every level's
enemies are in the file from the start, parked above the arena, and fly in when
their level begins — each level parked four metres above the last, so two
dormant bodies never occupy the same point.

Put that in the prompt, or the agent will discover it the hard way:

> Levels are four contiguous runs of a flat numbering — level *n* owns
> `Drone{(n-1)*10+1}`..`Drone{n*10}`. All forty are in the scene from the start,
> parked above the arena, four metres higher per level. Generate the scene from
> a Python emitter rather than writing forty entities by hand, and have it print
> the level boundaries the script needs.

**Generate content, don't hand-write it.** Anything with more than a dozen
repeated entities wants an emitter script — the arena and the car track are both
generated from a table. Prompt for the generator, not the JSON.

#### Quests

A quest is: a trigger, a set of steps with completion conditions, and a reward.
The interesting question is where it is *written*, and you should decide that
rather than let it be decided:

> Put quests in their own JSON file, `quests.json`, as a list of
> `{ id, title, steps: [{ description, kind, target, count }], reward }`. The
> script reads it and tracks progress in `world.state` keyed by quest id. Start
> with one quest — "kill 5 drones" — and a HUD line showing progress. No quest
> log UI yet.

Then, separately:

> Add a second quest that only unlocks when the first is complete, and make the
> HUD show the active quest's current step.

And later still:

> Add the quest log panel, `J` to toggle, listing every quest with its steps
> ticked or not.

Three prompts. Data model, then chaining, then UI — in that order, because a
quest log built over an unfinished data model gets rebuilt.

**Dialogue and triggers** are the same shape: data in a file, a small state
machine in the script, UI last.

---

### Layer 7 — the shell

Title screen, pause, death, next level, restart. This is genuinely last, and
doing it earlier is a common mistake — a menu wraps the game, so it wants the
game to exist.

> Add a title screen, a pause menu on `Esc`, and a game-over card. Use
> `HudPanel` hug sizing so each card sizes to its own content. Author the play
> HUD hidden so that `--steps 0` renders the title screen. Pause should freeze
> everything by making the game clock zero, not by branching around the update.

That last sentence is worth stealing. "Pause is `dt = 0`" is one line of
arithmetic; "pause is an `if` around the whole step function" is a bug farm.

---

### Layer 8 — feel

Particles, point lights, screen shake, day/night, materials. Cheap to add, easy
to look at, and safe last because none of it changes behaviour.

> Add a muzzle flash particle burst and a point light on each explosion that
> fades over 0.3 s. Do not change any gameplay numbers.

---

## 4. Prompting patterns that work here

**One verb per prompt.** "Add X" or "change X", not "add X and also fix Y".

**Name the file.** "in `player.rhai`" removes an entire class of surprise.

**Give the verification.** "Show me `engine simulate … --entity Drone1`" or
"screenshot at `--steps 300` and show me the image". The engine's whole design
is the check step; use it in the prompt.

**Ask for the number, not the vibe.** "The drones feel fast" produces a guess;
"drones close at 4 m/s and the player moves at 7.2 — tell me the closing speed
after your change" produces a decision.

**Freeze what works.** Say "don't change the arena / the movement / the HUD
layout" explicitly. Agents refactor helpfully.

**Ask for the trade-off out loud.** "Tell me what this costs before you build
it" catches things like *bullets that are not physics bodies cannot be stopped
by cover* — a real limitation worth knowing before, not after.

**Let it push back.** "If this conflicts with something already built, say so
instead of working around it."

---

## 5. Anti-patterns

| Anti-pattern | Why it hurts | Instead |
|---|---|---|
| The mega-prompt | Nothing is isolatable when it breaks | One layer per prompt |
| "Make it fun" | Not a change anyone can make | Name the dial and the number |
| Systems before the verb | Inventory on top of mushy movement | Verb first, always |
| UI before the model | The panel gets rebuilt when the model settles | Model, then UI |
| Hand-written repeated content | Forty entities nobody can edit | An emitter script |
| Accepting "done" without an image | Authoring blind is the one mistake the engine is shaped to prevent | Ask for the PNG |
| Tuning while building | You cannot tell a balance problem from a bug | Build, freeze, then tune |

---

## 6. A worked sequence

Twelve prompts, in order, for a small top-down shooter. Each one is a session
that ends with something you looked at.

```
 1  engine init, screenshot the starter scene, show me the PNG
 2  arena.json: 40×20 floor, four walls, top-down camera, sun. Screenshot.
 3  player: physics proxy + visual body, WASD at 7 m/s. Filmstrip.
 4  camera follows the player with a 0.2 s lag. Filmstrip.
 5  aiming: gun looks at cursor_ground. Screenshot with a cursor in the timeline.
 6  shooting: pooled bolts, 46 m/s, no physics. Filmstrip.
 7  three drones that steer at the player. Not killable. simulate --entity.
 8  drones take three hits and break. simulate + screenshot.
 9  HUD: health bar, ammo, score. ui-layout + screenshot.
10  health and contact damage. Print DEAD at zero, nothing more.
11  waves: ten drones in three waves, all parked above the arena from the start.
12  title / pause / game-over cards, play HUD authored hidden.
```

Then, and only then: levels, upgrades, an inventory, quests — each on the same
three-prompt shape of *model, then behaviour, then UI*.

---

## 7. The one-line summary

Ask for **one layer**, name **what not to touch**, and say **how you will check
it**. Repeat twelve times. That is a game.
