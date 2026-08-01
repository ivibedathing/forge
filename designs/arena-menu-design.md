# M36 — A game shell: settings, saves, quit, and a body to hang a gun on

M36 at merge; M35 is the global illumination design and M34 was the metre.

The arena shooter has been the repo's proof that a game can be built out of
what the engine already has. Four things it asked for cannot be:

| ask | why the script cannot do it |
| --- | --- |
| Save Game / Load Game | the Rhai sandbox has no I/O, and `world.state` is per-run and deliberately not baked |
| Quit | nothing can close the viewer from a script |
| Settings → graphics quality | `environment` is read off the `Scene` at load and never written again |
| Settings → difficulty / HUD / lighting | *these it can*, and they need no engine change |

So M36 is three small engine additions and one large content change. Every
addition is **off unless a script calls it**, which is what lets the milestone
land without re-blessing a baseline that has no business moving.

## 1. What is state and what is data — the question that shaped the save

M32 settled this once already: *ask what the bake should contain, and the answer
says whether something is state or data.* A save asks it again, and gets a
different answer, because a save is not a bake.

**A bake is the arena; a save is the campaign.** The bake writes where every
body ended up — it already exists, and it is what `--bake` has done since M10.
What it cannot write is `world.state`, which M12 made per-run on purpose: a
half-finished click, a bullet's remaining life, a muzzle-flash decay. Those are
disposable in exactly the way a solver cache is.

But `score`, `level`, `hp` and the settings are not disposable, and they live in
the same map as the flash decay because the map is the only memory a script has.
So the save is **the whole `world.state` map**, written as sorted JSON, and the
disposability distinction moves from *where the number lives* to *what a load
means*:

**Loading restores the campaign, not the arena.** A save says you were on level
3 with 71 health and 4,200 points. It does not say which drones were broken,
because the engine cannot spawn an entity and a broken drone cannot be put back
(the arena shooter's oldest constraint, and the reason its campaign is four
levels rather than endless). So a load is only meaningful **from the title
screen of a fresh run**, where every drone is intact and level 3 means the ten
drones of level 3 flying down from park altitude. Loading mid-run is refused by
the game rather than by the engine: the menu offers LOAD GAME on the title card
and does not offer it on the pause card.

That is a smaller promise than "save anywhere" and it is the honest one. The
alternative — serialising the arena — is `--bake`, which already works, and
wiring a button to it would mean a script asking the CLI to re-launch itself.

**Rejected: saving to `world.state` alone.** An in-session checkpoint needs no
engine change and survives nothing, which makes SAVE GAME a button that lies
about what it did.

**Rejected: a `SaveGame` component.** Invariant 5 says components are plain
data, and a save slot is not a property of an entity.

## 2. `world.save` / `world.load` / `world.has_save`

```
world.save(slot)      -> true, or a runtime error naming the path
world.load(slot)      -> true if a save was read, false if the slot is empty
world.has_save(slot)  -> bool, no side effect
```

- **The slot is an integer 0..=9**, and anything else is a located runtime
  error with the range in the message. A free-form name would be a path the
  script chooses, and a script choosing a path is the sandbox's whole objection.
- **The file is `<scene dir>/saves/slot<N>.json`**, next to the scene, for
  M10's reason: bake next to the scene, never `/tmp`, because everything in this
  engine resolves relative to the scene file.
- **Keys are written sorted and values are `f64`**, so a save is git-diffable
  by construction (invariant 1) and two saves of the same state are the same
  bytes. There is no binary form and there will not be one.
- **A missing slot is `false`, not an error.** A menu's first question is "is
  there a save?", and making that question cost an error would make every menu
  wrap it in something. A *corrupt* slot is an error — a file that exists and
  does not parse is a bug worth reporting, not an empty slot.
- **`load` replaces the map wholesale** rather than merging. A merge would leave
  keys from the run that is being abandoned, and the bugs that produces are the
  kind that show up three levels later.

**The determinism cost, stated rather than hidden.** Since M10 a run has been a
pure function of (scene, scripts, `--steps`, `--input`). A run that calls
`world.load` is a function of the save file too — exactly as a run with
`--input` is a function of the timeline. That is a *documented input*, not
hidden state (invariant 2), because the file is text on disk next to the scene.
`world.save` writes during headless runs as well as windowed ones, deliberately:
a call that silently did nothing under `screenshot` would be untestable, and the
CLI test that pins the round trip is a headless one. No committed timeline calls
either, so every baseline is untouched.

## 3. `world.quit`

```
world.quit()
```

Sets a flag the caller drains, following `take_breaks` and `take_explosions`
exactly — the script host queues, the sim loop acts. What acting means differs
by caller and both are documented:

- **`run-scene`** closes the window at the end of the step.
- **Headless** stops stepping and reports `"quit_at_step": N` on the `simulate`
  report. It does not fail: a game that ended is not an error, and a run that
  quit at step 40 of a requested 200 renders the frame it reached.

**Rejected: `world.quit()` as a runtime error headlessly.** Quitting is what the
button does; making the same call mean "end the game" in one place and "you have
a bug" in another is the split M28 refused for the pointer.

## 4. The `environment` block becomes writable

`world.set_shadows`, `world.set_fog`, `world.set_sky`, `world.set_samples`, and
a getter for each. This is the milestone's one genuinely awkward addition, and
the awkwardness is worth writing down.

**Three of the four are per-frame uniforms and the fourth is not.** `shadows`,
`fog_density` and `sky` reach the shader through `FrameUniform` and cost
nothing to change between frames. `samples` is baked into every pipeline by
`SceneRenderer::with_samples` (M16), so changing it means **rebuilding the
renderer**. The viewer does exactly that, on the step the value changes and only
then; the headless paths build a renderer per invocation anyway and read the
value once.

**The write-back rides the existing seam.** `ScriptHost` holds the settings in
an `Rc<RefCell<EnvironmentSettings>>` seeded from the scene at build, and the
caller — which owns the `Scene` — reads it back after the step and assigns
`scene.environment`. So `Scene::resolved_at` is untouched, `daylight` still
computes what `drives_sky` says it computes, and a scene whose scripts never
call a setter assigns a value equal to the one already there.

**`samples` is validated at the call**, not at the next render: `1` or `4`,
anything else is a located script error. That is M13's rule for
`set_particle_rate` — a bad value is a located script error rather than a baked
file that fails its own validation.

**A script-written `environment` is deliberately not baked**, and this is the
one place M36 owes §1's own question an answer. Ask what the bake should
contain: a bake reconstructs *the scene*, and whether the player likes shadows
is not a property of the scene — it is a display preference, in the same class
as `world.state`'s press capture and for the same reason. So it persists in the
**save slot** (settings are `world.state` keys, so the save carries them for
free) and not in the scene file. The consequence, stated rather than
discovered: a scene baked mid-run reloads with the `environment` block it was
authored with, not the one the settings screen left. That is the intended
answer, but it does mean a *screenshot* of a run that changed the setting is
not reproducible from the scene file alone — which is why nothing in the repo
pins one except this milestone's own fixture, whose script re-derives the
setting from the step number every step.

**Rejected: a `quality` enum.** One knob that means four things is a knob whose
meaning changes when a fifth is added, and the four fields already exist and are
already documented.

**Not writable: `sky_zenith`/`sky_horizon`/`sky_ground` and `shadow_distance`.**
Nothing asked for them, `sky_horizon` *is* the fog colour so writing it has a
second effect that wants its own thought, and every field added here is one more
thing a script can leave in a state the file does not describe.

## 5. `world.set_animation_clip`

The player has an idle and a run, and switching between them needs a setter that
did not exist. It is a **hard cut**, and that is the design rather than a
limitation: M9 §8 rejected blending, M30 restated the rejection, and M32
restated it again — *a gait change here is a different clip.* This is the call
that makes that sentence actionable.

The argument is the fragment form M30 specified: `meshes/soldier.gltf#Run`. It
is validated at the call against the rigs the host already resolved, so a
mistyped clip is a located runtime error with `did_you_mean`, matching
`world.key`. Changing the clip **resets `phase` to 0**, because a phase is a
fraction of a cycle and two clips do not share a cycle; carrying it over is the
`speed` trap M32 documented, in another place.

## 6. The player is a rig now

`examples/meshes/make_rigged_soldier.py` generates a 17-joint humanoid the way
`make_rigged_walker.py` generates the tour's, as text glTF with an embedded
base64 buffer — generated rather than committed binary for invariant 1's reason
and for M19's: a render sits under a baseline, so the geometry is a format
contract.

- **Two clips, `Idle` and `Run`**, and `Run` carries a measured `stride` so M32
  drives the legs off ground covered rather than off the clock. That is the
  whole reason the player stops sliding, and `engine list-joints --steps N`
  measures the number rather than anyone guessing it.
- **`PlayerBody`/`PlayerHead` are gone.** They were two boxes standing in for a
  person; one skinned mesh replaces both. `PlayerGun` stays but becomes a
  weapon model hung off the right hand.
- **The body turns to the aim, all of it.** A twin-stick game usually turns the
  legs to movement and the torso to the aim, which is a per-joint override, and
  M30 is explicit that joints are read-only for the reason M21 gave about the
  clock: a script-settable joint is hidden state. So `world.look_at` aims the
  whole model at the cursor, which is what a top-down shooter reads as anyway.
- **No `FootPlant`.** M32 plants against a `Terrain`, and the arena floor is a
  box — deliberately, since a body sliding on a trimesh hits M23's internal-edge
  bug. The floor is flat and level, so the model sits at a constant height and
  the thing `FootPlant` fixes does not arise. This is M32's stated cost ("a
  character cannot stand on a crate") reached from the other side.
- **No `SkinnedCollider`, in the end.** The draft called for five parts, and
  building it changed the answer: proxies are *kinematic* bodies, so eleven of
  them would shove the drones around in a game whose contact rule is tuned
  against a capsule — and M33 measured that adding a collider anywhere in a
  rapier scene perturbs every body in it. What stops a drone is still the
  player's own capsule, which is exactly what M33 says a proxy does not do
  ("a proxy holds a character up as much as a moving wall holds up the hand
  pushing it"). Recorded here rather than quietly dropped.

## 7. Three weapons

`examples/meshes/make_weapons.py` emits `weapon_pistol.gltf`,
`weapon_rifle.gltf` and `weapon_shotgun.gltf` — static meshes, not rigged.

**They hang off the right hand through `world.joint_position`**, which is M30's
sanctioned pattern stated in its own design: *hanging a prop off a hand is then
an ordinary `set_position`.* This milestone is the first thing in the repo to
actually do it.

**The unused two park below the floor**, at `y = -30`, which is where a spent
bullet already goes. A `Mesh` has no visibility flag and adding one would be a
component change to solve a script's problem; parking is the idiom the drones,
the barrels and the bullet pool all use.

**Spread is a fixed pattern, not a random one.** The shotgun throws five pellets
at fixed angles and the rifle walks a fixed three-shot cone. A script has no
randomness (M10, and it is load-bearing: a recorded timeline replays to the same
pixels), so a random cone was never available — and a fixed one is *better* for
a demo timeline, since the same click kills the same drone every replay.

| weapon | interval | damage | magazine | reload | pattern |
| --- | --- | --- | --- | --- | --- |
| pistol | 0.20 s | 34 | 12 | 1.1 s | one bolt, no spread |
| rifle | 0.09 s | 24 | 30 | 1.6 s | one bolt, ±1.5° walking cone |
| shotgun | 0.62 s | 21 × 5 | 6 | 1.9 s | five pellets at ±10° |

The pistol is exactly the gun that shipped (0.13 s and 34 damage became 0.20 s
and 34 — the interval is the one number that moved, so the rifle has somewhere
to be faster). The bullet pool grows from 14 to 24, because a shotgun puts five
bolts in the air on one click and a rifle sustains twelve.

## 8. The menu is a state machine now

Five screens where there were three, and the button that was one `MenuButton` is
a column of seven slots the script labels per screen. An empty label hides a
slot and a hidden element leaves the flow entirely (M31), so each card is
exactly as tall as the screen it is showing — the same property that already
made the end card close up around its missing button.

| screen | buttons |
| --- | --- |
| title | PLAY · SETTINGS · LOAD GAME · QUIT |
| pause | RESUME · SAVE GAME · SETTINGS · QUIT |
| settings | six rows, each cycling its value, then BACK |
| level cleared | NEXT LEVEL |
| end | none |

**Settings are `world.state` keys, which is what makes them saveable for free.**
They ride the same map the campaign does, so `world.save` carries them without
knowing they exist.

| row | values | reaches |
| --- | --- | --- |
| DIFFICULTY | NORMAL / HARD / BRUTAL | the three drone dials, multiplied |
| CROSSHAIR | ON / OFF | `set_hud_visible` |
| HUD | FULL / MINIMAL / OFF | `set_hud_visible` on four groups |
| LIGHTING | DAY / DUSK / NIGHT | `set_light_intensity` on sun and lamps |
| SHADOWS | ON / OFF | `world.set_shadows` |
| QUALITY | LOW / HIGH | `world.set_samples` + `world.set_fog` |

**LOAD GAME is on the title card only**, for §1's reason. **QUIT is `world.quit`
on both**, and under the recorded demo timeline it is never pressed.

## 8.5. `engine ui-layout --steps`, which the build added

Not in the draft, and forced by §8. The menu is seven slots the game script
*labels per screen*, so which of them a card uses — and therefore how tall the
card is and where its buttons are — is not a property of the file. `ui-layout`
reported the layout at rest and there was no way to ask for any other, which is
precisely what M32 refused to ship: *a system whose state no report can reach is
what M30 §6 says not to build.* So it takes `--steps N [--input f]` on M32's
`list-joints` precedent, and the demo director uses it to find the `NEXT LEVEL`
button on a card that is a different height from the title card.

It steps against **the requested viewport** rather than the documented
960×540 default, because a mouse-driven script's clicks are a function of the
frame (M28 §5) and reporting a layout the run could not have produced would be
worse than not reporting one.

## 8.6. Two things the renders changed

Both are in `designs/arena-shooter.md` at length; recorded here because they are
what the milestone actually cost.

- **A hugging column is as wide as its title.** M31's rule that a stretched
  child contributes nothing to a hugging parent means a card whose every child
  stretches sizes itself from the one that does not. `ARENA SHOOTER` is wide
  enough; `SETTINGS` is eight glyphs, and six rows of text hung out over both
  edges of their own buttons. The column now takes an explicit width and hugs
  only its height — which is the half that mattered, since it is what makes the
  end card close up around its missing buttons.
- **The title card has to be authored as what the script paints.** Otherwise
  the card grows on step 1, every button in it moves, and a demo timeline
  aiming at the rect `ui-layout` reports clicks through empty space. There is a
  CLI test asserting the rest layout and the painted layout agree.

## 9. Verification

- **Fixture `verify/m36_shell.json`** at `--steps 90` — no timeline needed in
  the end, since both additions are step-gated rather than input-driven. Its
  script turns `shadows` on at step 30 (the file authors `false`) and cuts
  `Cut` from `Idle` to `Run` at step 45 while `Held` never changes, so the
  render pins the two engine additions that have pixels behind them. **The two
  soldiers are the assertion** — M30's fixture logic for the fourth time: they
  share a file, a mesh and a material, so anything that made both wrong would
  leave them identical. Aimed at its subject with no terrain in frame (M22's
  rule), and measured rather than assumed — four consecutive renders came back
  as one image — so it takes a hard bit-exact pin, and it arrives with the CLI
  test that diff-renders it.
- **CLI tests** for the three that have no pixels: a save round trip (save,
  read the JSON back, load it in a second run, assert the state came back), an
  empty slot returning false, an out-of-range slot erroring, and `world.quit`
  stopping a headless run early with `quit_at_step` on the report.
- **The A/B**, because a writable `environment` reaches the render path. The
  claim is that a scene calling no setter renders byte-identically, and only an
  A/B between binaries can prove it. **Result: 34 of 38 comparable artifacts
  byte-identical.** The four exceptions are `showcase_90`, `585`, `646` and
  `810`, and the `md5`-it-N-times probe settled all four the way it has settled
  them three times before — each has a binary that disagrees with *itself*
  across four renders of the unchanged scene (585 at 3-of-4 new and 4-of-4
  base; 646 at 3-of-4 new; 810 at 3-of-4 on both; 90 at 2-of-4 base). The
  39th entry, this milestone's own fixture, cannot render under the base binary
  at all — it calls `set_shadows` — which is the expected exclusion. The
  ordinary sweep separately reported **41 of 41 passing**, every pre-M36
  artifact at zero differing pixels.
- **The arena is still not a fixture.** No baseline, no CLI test — it is a demo,
  as `designs/arena-shooter.md` has said since it was written. The demo timeline
  is regenerated because the scene changed.

## 10. Not here

Cloud saves, autosave, and more than ten slots. A save browser with timestamps
(a script has no clock). Restoring a mid-level arena, which wants entity
spawning. Weapon pickups — the campaign's "it is already there" trick would work
for them, and nothing asked. Per-joint aim override (§6). Sound, of which the
engine has none, and which is what a settings screen is usually for.
