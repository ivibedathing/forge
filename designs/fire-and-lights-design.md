# Fire and point lights (M17)

Two halves of one problem. The showcase tour's campfire was a cone of identical
orange sprites, and the tour's own design doc named the gap: *"the blast (no
light — nothing can drive a light from a script)"*, with **script-driven lights**
listed as one of the two upgrades that would move the scene most. This milestone
does both — makes the fire look like fire, and makes it light the ground it
stands on.

## 1. Why the old fire did not read as fire

Five separate reasons, and only one of them is about color:

1. **Every particle was identical at birth.** One emitter spawned particles with
   the same speed, size, and lifespan, so the population moved in visible
   lockstep and died at one height — a flat-topped cone.
2. **They were alpha-blended.** Overlapping flame got *more opaque*, not
   brighter. Fire is the archetypal emissive phenomenon; alpha blending can
   render orange smoke and nothing else.
3. **They rose in straight lines.** Hot gas is unstable and curls; a straight
   cone reads as a nozzle.
4. **They were round.** A moving ember covers more than its own width during an
   exposure, which is why real embers photograph as streaks.
5. **They came from a point.** A campfire is a bed of coals, and no amount of
   cone angle hides a single apex.

So the emitter grew five fields, one per reason, plus a component for the light.

## 2. The new emitter fields

| Field | What it fixes | Default |
|---|---|---|
| `blend: "alpha" \| "additive"` | overlapping flame brightens | `"alpha"` |
| `radius` | emission over a fire bed, not a point | `0` |
| `speed_jitter` / `size_jitter` / `lifetime_jitter` | the lockstep population | `0` |
| `turbulence` + `turbulence_scale` | flames lick instead of jetting | `0` / `1` |
| `stretch` | motion-aligned tongues and ember streaks | `0` |

**Every default is the pre-M17 behaviour**, and that is load-bearing rather than
polite. Twelve baselines were blessed before any of this existed, and the way
the property breaks is silent — see §5.

### Random draws are skipped, not defaulted

The order in which an emitter consumes its RNG is part of the file format's
contract: **direction → disc → speed → size → lifetime → turbulence**. Each
step is skipped *entirely* when its field is zero, so an emitter that opted into
nothing draws exactly the numbers it drew in M13, in the same order. Defaulting
a draw instead of skipping it (`jitter(rng, 0.0)` returning 1.0) would be
equivalent arithmetically and would still shift every subsequent draw, changing
every committed particle baseline. This is the same discipline M13 already
applied to capped spawns.

`particles.rs::defaulted_fire_fields_consume_no_randomness` pins it by
construction: an emitter with all five fields written out at their defaults must
produce instances equal to one that omits them.

### Turbulence is a field, not a shove

`turbulence` adds an acceleration sampled from smooth value noise at the
particle's own position (divided by `turbulence_scale`) plus a per-particle
offset drawn at birth. Three things about that:

- **Smooth, not per-step random.** An independent random shove each step makes a
  particle vibrate in place; a smooth field makes it arc. The lattice weights go
  through smoothstep so the field is C¹ and a particle crossing a cell boundary
  does not kink. `turbulence_is_smooth_along_a_path` pins this.
- **Per-particle offset.** Without it every particle follows the same streamline
  and the plume grows one shared braid.
- **Value noise, not Perlin.** A third of the arithmetic, and once three
  channels are combined into a vector and integrated along a path, the
  difference is invisible. The integer hash is written out in
  `engine-core/src/particles.rs` for the same reason the xorshift is: a
  dependency upgrade must not be able to change what a scene looks like.

It is also **not divergence-free** — a true curl field would be. Nothing here
conserves mass, and what the eye reads as fire is coherent lateral wander.

### Additive blending is a second pipeline, not a clever shader

`ALPHA_BLENDING` is `src·srcA + dst·(1−srcA)`; additive is `src·srcA + dst·1`.
One shader, one instance buffer, two pipelines; the sorted draw list is
partitioned on the CPU with a stable `sort_by_key`, so each group keeps its
back-to-front order and the pass is two draws rather than a pipeline switch per
sprite.

The tempting alternative is one pipeline with premultiplied output — emit
`(rgb·a, a)` for alpha and `(rgb·a, 0)` for additive under
`(One, OneMinusSrcAlpha)`. It works, and it saves a pipeline object. It was
rejected because it moves the multiply by alpha out of the blend unit and into
the shader **for every particle**, including the ones under existing baselines.
Rearranging arithmetic that a dozen committed PNGs depend on, to save one
pipeline object, is the wrong trade.

**Additive sprites draw after every alpha-blended one**, regardless of depth.
So a flame glows *through* the smoke above it. That is an approximation — a real
depth-correct compositor would let the smoke occlude it — and it is the right
one here, because the smoke above a fire genuinely is lit from below.

### Stretch is measured in seconds

`stretch` elongates the billboard along its velocity by the distance the
particle covers in that many seconds. Same number stretches a fast ember into a
streak and leaves slow smoke nearly circular — which is what a camera does. The
elongation is along the velocity's *screen-space* projection, so a particle
flying at the camera stays round instead of collapsing to a line.

## 3. `PointLight`

A local light with a position and no orientation. Many per scene, up to
`MAX_POINT_LIGHTS` (8) — the ninth is `too_many_point_lights`, an error rather
than a light that silently never shines, because an agent that placed nine and
sees eight has no way to tell which one was dropped.

- **Falloff** is inverse-square windowed by `(1 − (d/r)⁴)²`. Inverse-square is
  the physics; the window is what makes the light *local*, and it matters more
  than it looks — without a hard horizon every light contributes a little to
  every surface, and a lantern in one room lifts the black level of the next.
  `range_is_a_hard_horizon` asserts a fragment past `range` is byte-identical to
  one with no light at all.
- **`intensity` is brightness at one unit of distance**, which makes it directly
  comparable to `DirectionalLight.intensity` at exactly that distance.
- **No shadows.** The engine has one shadow map and it belongs to the sun. For a
  campfire this is nearly free: the fire sits in the open, and the thing that
  would cast into its light (the logs) is also the thing that is brightest, so
  the missing occlusion reads as coals glowing.
- **Ordered by entity name.** The uniform array is fixed-size and a light's index
  in it must not depend on archetype iteration.
- **A `PointLight` counts as lighting the scene**, so a campfire-only scene stays
  dark outside its firelight instead of getting the fallback sun.

Point-light contributions are **added to the finished color**, on their own
branch after every M16 feature. Firelight is *extra* light: a scene keeps its
sun, its ambient, and its sky reflection whether or not a fire is burning.
`a_point_light_is_extra_light_not_replacement_light` walks every pixel of a
sunlit scene and asserts adding a lamp never darkens one.

The shader **re-derives** the GGX terms rather than sharing a function with the
sun path. That duplication is deliberate; see §5.

## 4. Script control

`world.light_intensity` / `set_light_intensity` / `light_color` /
`set_light_color`, and they work on **all three** light components — `intensity`
and `color` mean the same thing on a `PointLight`, a `DirectionalLight`, and an
`AmbientLight`, so a script author should not have to remember which kind a name
refers to. An entity with none of them gets an error naming all three.

Validation is at the call, like `set_particle_rate`, because both fields bake
change-based and a bad value must be a located script error rather than a scene
file that no longer validates. Intensity **errors** on negative/NaN/overflow;
color **clamps** to `[0, 1]` — a flicker that computes 1.02 has not made a
mistake worth halting a run over, and the alternative is every author writing
the same `min`/`max` around every write.

Both fields bake under the existing change-based rule, for all three light
kinds. A flickering campfire is at *some* intensity when the run stops, and a
resumed scene has to reopen lit the way it was saved.

## 5. What is fragile here, and why the code looks redundant

Two places in this milestone are deliberately more repetitive than they need to
be, and both are guarding the same property.

**In `mesh.wgsl`**, `evaluate_point_light` re-derives the GGX distribution,
visibility, and Fresnel that `fs_main` already computes for the sun. Factoring
them into one function both lights call would rewrite the four lines M16's
comment block declares untouchable — and whether the compiler may contract
`a*b + c` into an FMA depends on the code around it, so an *equivalent*
expression is not good enough.

**In `particles.wgsl`**, the un-stretched quad expansion is written out twice —
once in the `speed ≈ 0` fallback and once in the `stretch == 0` branch — rather
than being lerped into the stretched form.

Both looked like paranoia until a one-pixel diff showed up in `m14_break.png`
mid-milestone, in a scene with no particles and no point lights. It turned out
to be **pre-existing drift on `main`, not a regression** — but finding that out
took a bisect, and the reason a bisect was even necessary is that the ULP
sensitivity M16 documented is real.

The check that settled it is worth repeating rather than re-deriving: build the
CLI at `main` and in the worktree, render the same scenes with both, and `cmp`
the PNGs. That is a direct A/B, independent of whatever a committed baseline has
drifted to:

```bash
for spec in "verify/m4_lighting.json" "verify/m13_smoke.json --steps 180" ...; do
  main/engine     screenshot $spec --out /tmp/a.png
  worktree/engine screenshot $spec --out /tmp/b.png
  cmp /tmp/a.png /tmp/b.png
done
```

All 19 scene/step combinations came back byte-identical, including every
particle-heavy one.

## 6. The fixtures

`verify/m17_fire.json` + `scripts/m17_fire.rhai` is a night campfire at 3.6 m:
stone ring, three logs, an emissive coal bed, and five emitters — a white-hot
`FireBase`, the `Fire` body, breakaway `FireTongues`, alpha-blended `Smoke`, and
streaked `Embers` — under a `FireLight` the script flickers. Baseline blessed at
`--steps 240`, pinned by
`cli.rs::the_m17_fire_fixture_pins_additive_flame_and_firelight`, which also
bakes the run and revalidates it.

The tour's campfire station is the same construction scaled to a 3 m pit and
seen from 12 m in daylight, and all six `showcase_*.png` baselines were
re-blessed for it. Three tuning lessons from getting there, all of which look
obvious in hindsight and none of which were:

- **Additive saturation is about *area*, not brightness.** The first attempt put
  95 sprites/s of half-size 0.20 at alpha 0.42 inside a 0.30 disc. They summed
  past 1 across the whole disc and rendered as a flat orange lid over the pit.
  A hot core needs *few, small, faint* sprites and lets the overlap build the
  brightness.
- **Stretch is a spice.** `stretch: 0.22` on bright embers at speed 3.2 turned
  the campfire into a firework — long radial rays out of a point. Embers want
  ~0.05; the flame body wants ~0.03.
- **A point light 0.4 m above a surface blows it out**, whatever its intensity,
  because that is what inverse-square does. Raise the light into the flame body
  and darken the ash rather than fighting it with intensity.

## 7. Not done

- **No shadows from point lights**, and no second cascade for the sun.
- **No light on the explosion.** The blast at station 04 still emits no light;
  now that scripts can drive a light, a fixture-mounted `PointLight` pulsed for
  a few steps would work — it just is not wired up.
- **No spot lights.** A cone with an orientation is the obvious next component,
  and the aiming convention (local −Z) is already settled by every other
  directional thing in the engine.
- **No emissive-particle light injection.** The fire's light is an explicit
  component a script drives, not something derived from the particles. That is
  the honest design under invariant 5 (components are plain data, logic in
  systems), and it also means the flame and the light can disagree if an author
  drives only one of them.
