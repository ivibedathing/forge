# The game shell (M36, `designs/arena-menu-design.md`)

*Relocated verbatim from `CLAUDE.md` when it was compacted to an index. The digest that points here
is `CLAUDE.md` §The systems.*

*The design doc for this milestone is `designs/arena-menu-design.md` — it has the rejected
alternatives; this file has what the build learned.*

The arena shooter asked for a main menu with Settings, Load Game, Save Game and
Quit. **Three of those four are engine work**, because the Rhai sandbox has no
I/O, nothing can close the viewer from a script, and `environment` was read off
the `Scene` at load and never written again. Everything added defaults to the
pre-M36 behaviour, so the sweep came back **41 of 41** and the A/B **34 of 38
comparable artifacts byte-identical** (the four exceptions are tour frames
neither binary reproduces — see Verification; the 39th, this milestone's own
fixture, cannot render under the base binary because it calls `set_shadows`).

- **A save is the whole `world.state` map, written as sorted JSON in
  `<scene dir>/saves/slot<N>.json`.** `world.save` / `world.load` /
  `world.has_save`, slots `0..9`. The shape came out of M32's rule — *ask what
  the bake should contain, and the answer says whether something is state or
  data*: the bake already writes where every body ended up, and this writes the
  memory the bake deliberately drops. So **a load restores the campaign, not
  the arena**: the engine cannot spawn an entity and a broken drone cannot be
  put back, which is why the game offers LOAD GAME on the title card only.
  Sorted keys make a save git-diffable by construction (invariant 1); an empty
  slot reads `false` rather than erroring, because "is there a save?" is a
  menu's first question, while a slot that exists and does not parse *is* an
  error. `load` replaces the map wholesale — a merge leaves keys from the run
  being abandoned, and those bugs surface three levels later.
  **The determinism cost, stated:** a run that calls `world.load` is a function
  of the save file, exactly as a run with `--input` is a function of the
  timeline — a documented input, not hidden state, because it is text on disk
  next to the scene. `world.save` writes headlessly too, deliberately: a call
  that silently did nothing under `screenshot` would be untestable.
- **`world.quit` is a request the caller drains**, following `take_breaks`
  exactly, because what quitting *means* differs by caller: the viewer closes
  its window at the end of the frame that asked, and a headless run stops
  stepping and reports `"quit_at_step": N`. It is **not** a failure — a game
  that ended is not an error. The key is absent unless a script quit, so every
  pre-M36 report is byte-identical, and `simulated_steps` stays what was
  *asked* for so the two together say "you asked for 500 and it ended at 43".
  Gotcha found by its own test: the sim loop counts `1..=steps` and a script is
  handed the 0-based index, so reporting the loop counter names a step one
  later than the one that called `quit`.
- **The `environment` block is writable**: `shadows`/`sky`/`fog`/`samples` plus
  a getter each. `ScriptHost` holds it in an `Rc<RefCell<EnvironmentSettings>>`
  seeded at build and the caller — which owns the `Scene` — assigns it back
  after the step, so `Scene::resolved_at` is untouched and a scene that calls
  no setter assigns an equal value. **Three of the four are per-frame uniforms
  and `samples` is not**: it is baked into every pipeline by `with_samples`
  (M16), so the viewer rebuilds the renderer and its attachments on the step it
  changes and only then. `set_samples` validates 1-or-4 **at the call** (M13's
  `set_particle_rate` rule). **A script-written `environment` is deliberately
  not baked** — whether the player likes shadows is a display preference, not a
  property of the scene, so it persists in the save slot instead; the
  consequence is that a screenshot of a run that changed it is not reproducible
  from the scene file alone.
- **`world.set_animation_clip` is a hard cut**, and that is the design rather
  than a limitation: M9 §8 rejected blending, M30 and M32 restated it, and this
  is the call that makes *a gait change is a different clip* actionable. The
  argument is M30's fragment form, validated against the rigs the host already
  resolved (mistyped clip = located runtime error with `did_you_mean`). It
  **resets `phase` to 0**, because a phase is a fraction of a *cycle* and two
  clips do not share one — carrying it over is M32's `speed` trap in another
  place.
- **`engine ui-layout` takes `--steps N [--input f]`**, added mid-build and
  forced by the menu: seven slots the script labels per screen means which
  slots a card uses is not a property of the file, and M32 refused to ship a
  system whose state no report can reach. It steps against the *requested*
  viewport, not the 960×540 default, since a mouse-driven run is a function of
  the frame (M28 §5).

**Two things the renders changed, both worth knowing before authoring a menu.**
**A hugging column is exactly as wide as its widest *non-stretched* child** —
M31's rule — so a card whose every child stretches sizes itself from its title,
which is fine for `ARENA SHOOTER` and left `SETTINGS`' six rows hanging over
both edges of their own buttons. The column now takes an explicit width and
hugs only its height, which is the half that mattered (the end card still
closes up around its missing buttons). And **a card must be authored as what
its script paints on step 1**: one that grows moves every button in it, and
`ui-layout` at rest then reports a rect a timeline clicks straight through. A
CLI test now asserts the two agree.

Fixture `verify/m36_shell.json` at `--steps 90`: two soldiers, one whose clip
the script cuts to `Run` at step 45, under a shadow the script turns on at
step 30 that the file authors as `false`. **The two soldiers are the
assertion** (M30's fixture logic for the fourth time). Aimed at its subject
with no terrain in frame per M22's rule, and four consecutive renders came back
as one image, so the hard bit-exact pin is measured. Not here: cloud saves,
autosave, a save browser (a script has no clock), restoring a mid-level arena,
and per-joint aim override.
