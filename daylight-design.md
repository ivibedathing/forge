# Day and night (M21)

The engine has a sun, an ambient term, a three-band gradient sky, distance fog tied to the sky's
horizon color, one shadow map, and a reproducible clock. Every one of those is authored as a
constant. A scene is fixed at whatever hour its author typed, and moving it to dusk means editing
six colors by hand and keeping them consistent with each other — the sun's warmth against the
horizon's, the ambient against the zenith, the fog against the haze.

Day/night is the system that makes those six numbers **one number**: the time of day.

## 1. It is a CPU function, not a renderer feature

The whole system is a pure function

```
(DaylightSettings, time) -> Daylight
```

in `engine-core/src/daylight.rs`, folded onto a scene's lights and environment by
`scene::apply_daylight`. A `Daylight` is nothing but values the engine already had fields for: a
directional light, an ambient term, three sky band colors, and a fog multiplier.

**No WGSL changed. No new uniform, no new pass, no new binding. `SceneRenderer::draw` takes exactly
the types it took before.**

That is the central decision and it buys three things at once:

- **It cannot break M16's bit-exactness.** The four lines in `mesh.wgsl` computing
  `direct`/`ambient`/`base_color` are declared untouchable, and the FMA-contraction hazard means
  even an equal-on-paper rewrite can move a pixel. A feature that adds no shader code cannot trip
  that wire. Verified, not assumed — see §8.
- **It is testable without a GPU**, unconditionally, like `particles.rs` and `tree.rs`. The sun's
  arc, the palette interpolation, the horizon handoff, and the wrap across midnight are all
  ordinary unit tests with no skip path.
- **Everything downstream tracks for free.** Shadows follow the sun because they always followed
  the `DirectionalLight`. Fog changes color at sunset because fog *is* `sky_horizon`. Water
  reflects a dusk sky because `water.wgsl` already reflects whatever the sky uniform says. Sky
  reflections off metal already share `sky_common.wgsl`. Not one of those needed a line.

The cost is stated plainly in §9: with no directional term in the sky, a sunset reddens the whole
sky evenly and there is no sun disc.

## 2. The block

`daylight` is a **top-level sibling of `physics` and `environment`**, not a field inside
`environment`.

```json
"daylight": {
  "time_of_day": 16.3,
  "day_length": 300.0,
  "sun_elevation": 54.0,
  "sun_azimuth": 28.0,
  "moon_elevation": 40.0,
  "moon_color": [0.55, 0.66, 0.95],
  "moon_intensity": 0.06,
  "drives_sun": true,
  "drives_sky": true
}
```

Two reasons it sits outside `environment`. Conceptually, `environment` is a set of static render
settings and this is a **clock-driven system** — closer to `physics`, and it *produces* environment
values rather than being one. Mechanically, `EnvironmentSettings` is `Copy` because lights resolve
every frame in the viewer; an optional keyframe palette is a `Vec`, and hanging it there would cost
that type its `Copy` and put a clone in the per-frame path. Keeping the palette in its own struct
that the renderer never sees leaves both `EnvironmentSettings` and `ResolvedLights` exactly as they
were.

**Absent `daylight` block means the pre-M21 engine, byte for byte.** `apply_daylight(None, ..)`
returns its inputs untouched — literally the same values flowing through the same code. That is the
M16 contract repeated, and it is not a convenience: nineteen baselines existed and none of them had
any business moving because a feature they do not use was added.

## 3. The clock

Water settled this and daylight inherits it: `time` is `--time T` when given, otherwise
`steps / timestep_hz` (`scene_time` in the CLI), and the viewer uses whole fixed steps since load.
A scene with water *and* daylight has one clock, not two.

```
hours = time_of_day                                     when day_length == 0
hours = (time_of_day + time * 24 / day_length) mod 24   otherwise
```

`time_of_day` is hours in `[0, 24)` — `6.5` is half past six, and an agent writing that needs no
conversion table. When the day is cycling it means the hour at scene time zero. `day_length` is
**seconds of scene time per full 24-hour cycle**, and it defaults to `0`.

**Frozen is the default, and that is deliberate.** Most scenes want a dial — "render this at golden
hour" — not motion, and a frozen day means a screenshot is reproducible from the file with no
`--time` at all. Cycling is one field on top. It also makes the units legible in fixtures:
`day_length: 24.0` is a one-second hour, so step 390 at 60 Hz is 6:30 in the morning and the
fixture reads itself.

`engine filmstrip` samples in seconds of time, so a filmstrip across a cycling day is one PNG of the
entire cycle (§8).

## 4. The sun's arc

Artistic, with a physically-shaped path. Two parameters replace latitude, longitude, date, and
axial tilt:

- `sun_elevation` — degrees above the horizon the sun reaches at noon. `90` is overhead, `20` is a
  winter afternoon that never really gets going.
- `sun_azimuth` — compass bearing of the noon sun, rotating the whole arc about Y. `0` puts noon
  toward −Z, matching the engine's aiming convention.

With `h = (hours − 12) / 12 · π` (zero at noon, ±π at midnight) and `e` the max elevation:

```
altitude = asin(sin(e) · cos(h))
azimuth  = sun_azimuth + atan2(sin(h), cos(h) · sin(e))
to_sun   = (cos(altitude)·sin(azimuth), sin(altitude), −cos(altitude)·cos(azimuth))
```

That is a great circle tilted so its peak is `e`: noon gives altitude `e` at the noon bearing,
midnight gives its negation on the opposite bearing. The `DirectionalLight`'s stored value is travel
direction, so it is `−to_sun`.

**Which way is east is worth deriving once rather than rediscovering.** A noon sun toward −Z makes
−Z south, so +Z is north; facing north in a right-handed Y-up system puts east at
`cross(+Z, +Y) = −X`. The sun rises toward **−X** and sets toward **+X**. (Getting this backwards
cost two test failures before the reasoning was written down.)

**Sunrise is 06:00 and sunset is 18:00, always, at every elevation.** Real astronomy moves them with
the season; refusing to is what makes the palette portable — a keyframe at 18:00 is *the* sunset
keyframe in every scene, rather than a color that lands at the wrong moment when someone edits
`sun_elevation`.

### The moon, and the handoff

The moon rides the same arc offset by twelve hours, with its own `moon_elevation` so it is not a
mechanical anti-sun, and its own `moon_color` / `moon_intensity`. There is still exactly **one**
directional light, which is what keeps the single shadow map sufficient — the twelve-hour offset
means the two are never meaningfully up together.

**The light *is* the dominant body — direction, color, and intensity, with no summing of the two.**
The bodies swap where their luminances are equal, so the light's brightness does not jump at the
crossover; only its hue and its direction change, and both do so at the moment when there is least
light to notice it by.

Two alternatives were considered and are worse. *Summing* the two would keep an orange sunset
arriving from the moon's side of the sky for the whole of twilight. *Crossfading the direction*
would aim the light at a patch of sky where neither body is, for the same stretch.

The sun needs no fade-in window: the palette author already fades it, which is what `sun_intensity`
is for. The moon gets a `smoothstep` across ±6° of altitude so it does not pop on when it crosses
the horizon.

**The handoff's invisibility is a property of the palette, and it is tested as one.**
`the_direction_switch_happens_while_there_is_almost_no_light_to_notice_it_by` walks the day at
one-minute resolution, asserts exactly two body swaps, and fails if either happens while the light
carries more than 0.08 luminance. Brighten the dusk keyframes and that test fails — which is exactly
when it should.

### The shadow-fitting problem, and where it is fixed

A sun on the horizon casts shadows of unbounded length, and one a hair below it casts them *upward*
— the ground shadowing itself from beneath. Neither is a precision failure a bias or a better ortho
fit could solve; the geometry really is that shape. Before M21 no scene ever reached those angles
(every shadow-casting fixture in the repo aims its sun 24°–33° up, measured); day/night reaches them
twice a day.

So `clamp_shadow_elevation` in `scene_renderer.rs` pushes the direction used to build the **shadow
matrix** down to at least 5° below horizontal, while the direction that *lights* the scene keeps
going. It is a lie, told when direct light is nearly gone and the shadows it would have cast are far
too long and faint to read. It lives in the renderer rather than the scene format because an author
should not have to know the renderer has a floor — and above 5° it returns its input unchanged,
which is why it costs every pre-M21 baseline nothing.

## 5. The palette

Colors come from a keyframe table over the day, not from a scattering model. Each keyframe carries
`hour`, `sun_color`, `sun_intensity`, `ambient_color`, `ambient_intensity`, `sky_zenith`,
`sky_horizon`, `sky_ground`, and `fog_scale`. **All nine are required**: a half-specified keyframe
silently interpolating toward black is a worse failure than being told to finish it.

Interpolated linearly in hours and **wrapping across midnight**, so a 21:00 keyframe and a 00:00
keyframe interpolate through the small hours rather than racing backwards through the day.
Interpolation is in linear RGB, because every color in this engine is linear and light adds
linearly; lerping a sunset through sRGB would darken its middle.

The built-in table is eight keyframes — deep night, astronomical dawn, sunrise, golden morning,
noon, golden evening, sunset, dusk — and a scene may author its own. **The noon keyframe is exactly
the M16 clear-day defaults** (`[0.19, 0.34, 0.62]` / `[0.62, 0.71, 0.82]` / `[0.16, 0.16, 0.17]`,
`fog_scale` 1.0), so a daylight scene at noon looks like a hand-authored scene does today: the two
models agree at the one hour anyone can check against existing work.

The golden keyframes at 06:42 and 17:18 carry far more `sun_intensity` (0.72) than the horizon
keyframes at 06:00 and 18:00 (0.16). That asymmetry is not timidity — the sun at the horizon really
is dim and red, and it is also what keeps the direction handoff invisible.

Why the sun's *intensity* lives in the table rather than falling out of `sin(altitude)`: a sunset's
redness and its dimness are one artistic decision, and splitting them across a physical falloff and
a color curve means retuning a dusk in two places that then disagree.

**Fog is a scale, not a density.** `fog_scale` multiplies the scene's authored
`environment.fog_density`. A scene with `fog_density: 0` therefore stays clear all day however misty
the palette's dawn is, and a scene that wants fog authors it once and gets a dawn thickening for
free. An absolute density here would mean a daylight block silently switching fog on in a scene that
never asked for any. The showcase tour is the applied case: adding daylight meant dividing its
authored density by the palette's ≈1.7 at those hours — one number retuned, not a curve.

## 6. Ownership, and what daylight overrides

`drives_sun` (default `true`) — daylight synthesizes the sun's direction, color, and intensity. A
scene needs **no** `DirectionalLight` entity, and authoring one anyway is a validation error
(`daylight_and_directional_light`). Two owners of one sun is exactly what invariant 8 exists to
prevent: an entity whose Transform rotation is silently ignored, or silently overwritten, is a value
in a text file that does not mean what it says. The escape hatch is `drives_sun: false` — daylight
then paints the sky and fog only, and a hand-aimed `DirectionalLight` keeps its job.

`drives_sky` (default `true`) — the three band colors **and the ambient term** are computed. Ambient
rides with the sky rather than with the sun because it *is* the sky's contribution, which is why M16
gates hemispheric ambient on `sky` in the first place. A scene that authored non-default band colors
or its own `AmbientLight` and leaves this on gets the `daylight_overrides_sky` **warning** — the
`unused_material` precedent, with `drives_sky: false` named in the message as the fix.

Nothing about daylight is baked or traced. It is a pure function of the clock, disposable in exactly
the sense solver caches and particle state are — a baked scene carries the `daylight` block and
reproduces the same sky. A script that dims a lamp based on the hour bakes that lamp's intensity
change under the existing change-based rule, unchanged.

## 7. Scripts

Two read-only getters, and that is the entire surface:

- `world.time_of_day()` → hours as a float
- `world.sun_altitude()` → degrees, negative when the sun is down

`sun_altitude` is derivable from the first, but only by reimplementing §4's arc in Rhai, and "turn
the lamps on when the sun is down" is *the* use case. With those two, street lights, shop signs, and
a campfire that burns brighter after dusk are ordinary `set_light_intensity` calls — which is why
there is no `auto_on` field on `PointLight`, and why day/night needed no new script API for lights
at all.

Both are evaluated **once per step** on the `ScriptHost` from the same `step * dt` the renderer
uses, so two calls in one step cannot disagree and a replay reads the clock identically.

Asking a scene with no `daylight` block for the time is a **runtime error**, not a plausible noon: a
script that wants the time in a scene that has no clock is a bug, and inventing one hides it until
the lamps come on at the wrong moment.

**There is no setter.** A script-settable clock is hidden state — the scene would stop being
reconstructible from its text, which is invariant 2. "Sleep until dawn" is a real want and is named
in §9 rather than smuggled in.

## 8. Verification

`engine-core/tests/daylight.rs` — 21 tests, GPU-free and unconditional: the arc's endpoints and
noon, east/west, the midnight wrap in both the clock and the palette, monotonic morning altitude,
`day_length: 0` being genuinely frozen, `t` and `t + day_length` agreeing, the noon keyframe matching
M16, the handoff's continuity and its brightness bound, and that a daylight scene is never pitch
black. Validation adds four more in `validate.rs` and two at the CLI level.

Fixture `verify/m21_daylight.json` — a pond in a basin, three trees casting real shadows, a boulder
and a wall for shadow shapes, and a lamp post whose `PointLight` starts at intensity 0 and is raised
by `scripts/m21_lamp.rhai` off `world.sun_altitude()`. `day_length: 24.0`, so an hour is a second.

Five baselines at `--steps 120 / 390 / 720 / 1110 / 1320` — 02:00, 06:30, noon, 18:30, 22:00 — from
**one scene file**, pinned by `the_m21_daylight_fixture_pins_a_whole_day_from_one_file`, which also
asserts that 02:00 does *not* match the noon baseline (otherwise all five would pass for the trivial
reason that the clock is ignored).

They use `--steps` rather than `--time` because the lamp is script-driven and scripts run on the
step loop; a `--time` render never steps. Baselines are blessed from the **debug** binary, per M19's
rule — the fixture has trees, and CPU-generated geometry makes baselines profile-sensitive.

`engine filmstrip verify/m21_daylight.json --out day.png --start 0 --end 24 --frames 8 --columns 4`
renders the whole cycle on one sheet. It is deliberately **not** committed: `diff-render` renders a
scene at a baseline's dimensions and cannot reproduce a filmstrip, so a committed one would be the
only baseline in the repo that nothing could check. The command is the artifact. (It also shows the
lamp dark, because filmstrip does not step and therefore does not run scripts.)

**The bit-exactness of the existing baselines was checked the way this repo has learned to check
it** — an A/B between binaries built at the merge base and here, not a diff against baselines.
15 scenes × 5 step counts = **75 combinations, all byte-identical**, covering shadows, fire, water,
trees, breaking, HUD, physics, the car track, and the showcase tour. That is what confirms both the
absent-block path and the shadow-elevation clamp are inert.

## 9. What is not here

- **No sun disc and no directional horizon glow.** The sky has no term that knows where the sun is,
  so a sunset reddens the whole sky evenly. This is the honest cost of §1 and it is the natural next
  commit: `sky_common.wgsl` gains a sun-direction term added on its own branch after the untouchable
  lines, following the M17 point-light precedent, with its own A/B check.
- **No stars, no moon disc, no clouds.**
- **No astronomical accuracy.** No latitude, longitude, or date; sunrise is always at six.
- **No scattering, no aerial perspective.** The sky is still a gradient.
- **No script-settable time**, and therefore no "sleep until dawn" — that is state, and it needs an
  answer about where it is written down before it needs an implementation.
- **No shadows from the moon as a second map.** There is one shadow map and one directional light;
  the moon is that light.
- **No weather.** `fog_scale` is a time-of-day curve, not a forecast.
- **Scripts cannot drive `Material.emissive`.** The fixture's lamp wants its *globe* to light up as
  well as cast light; the curated API can set a light's intensity but not a material's emissive, so
  the globe carries a constant warm emissive instead. This is a gap day/night surfaced rather than
  created, and it is small — the light API's shape is already the template.

### One thing this milestone broke, and what it means

`showcase_tour_uses_every_component_the_engine_has` failed on the commit that added daylight to the
tour, because `daylight` is the first feature that makes two components **forbidden** rather than
merely optional: with `drives_sun`, a `DirectionalLight` is a validation error. The contract's
premise — that one scene can use everything at once — has a hole in it now.

The fix computes the exemption from the same rule validation enforces, so it evaporates by itself if
the tour ever sets `drives_sun: false`, and it asserts the *converse* too (a driven sun means the
scene must not author one). A second contract,
`showcase_tour_uses_every_scene_block_the_format_has`, closes the related gap the component walk
could never have seen: `daylight` is a block, not a component, so nothing would have noticed the
tour omitting it.
