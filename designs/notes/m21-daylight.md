# Day and night (M21)

*Relocated verbatim from `CLAUDE.md` when it was compacted to an index. The digest that points here is `CLAUDE.md` §Day and night.*

**It is a pure CPU function, and that is the whole design.** `engine-core/src/daylight.rs` maps
`(DaylightSettings, time) -> Daylight`, and `scene::apply_daylight` folds that onto the
`ResolvedLights` and `EnvironmentSettings` the renderer was going to receive anyway. **No WGSL
changed, no new uniform, no new pass — `SceneRenderer::draw` takes exactly the types it took
before.** So M16's untouchable four lines cannot be tripped, the whole system is GPU-free and
unconditionally testable, and everything downstream tracks for free: shadows follow the sun because
they always followed the `DirectionalLight`, fog recolors at sunset because fog *is* `sky_horizon`,
and water reflects a dusk sky because `water.wgsl` already reflects whatever the sky uniform says.

`daylight` is a **top-level sibling of `physics` and `environment`**, not a field inside it — it is
clock-driven and it *produces* environment values, and a `Vec` palette inside `EnvironmentSettings`
would cost that type its `Copy` and put a clone in the per-frame path. It rides the same clock water
does, and **`day_length: 0` (the default) freezes the day**: most scenes want a dial, not motion, and
a frozen day is reproducible with no `--time` at all. `day_length: 24.0` makes an hour a second.

- **The arc** is artistic with a physical shape: `sun_elevation` (noon altitude) and `sun_azimuth`
  (noon bearing) replace latitude, date, and axial tilt. **Sunrise is 06:00 and sunset 18:00 at every
  elevation** — refusing to move them with the season is what makes an 18:00 keyframe *the* sunset
  keyframe in every scene. A noon sun toward −Z makes −Z south, so the sun **rises toward −X and sets
  toward +X**.
- **The moon** rides the same arc twelve hours out of phase with its own elevation, color, and
  intensity. There is still one directional light: **it *is* the dominant body**, with no summing.
  The bodies swap where their luminances are equal, so brightness is continuous by construction and
  only hue and direction shift. Summing instead would send an orange sunset from the moon's side of
  the sky for all of twilight; crossfading the direction would aim the light where neither body is.
  The handoff's invisibility is a **property of the palette**, and a test walks the day at one-minute
  resolution asserting exactly two swaps, each under 0.08 luminance.
- **The palette** is eight keyframes, all nine fields required (a half-specified keyframe fading to
  black is worse than an error), interpolated linearly in linear RGB and **wrapping across
  midnight**. **The noon keyframe is exactly the M16 clear-day defaults**, so the model and every
  hand-authored scene agree at the one hour anyone can check. Sun intensity lives in the table rather
  than falling out of `sin(altitude)` because a sunset's redness and its dimness are one decision.
  **Fog is a `fog_scale` multiplier on the authored `fog_density`**, never an absolute — a scene with
  `fog_density: 0` stays clear all day.
- **Ownership.** `drives_sun` (default on) synthesizes the sun, and an authored `DirectionalLight`
  beside it is `daylight_and_directional_light`. `drives_sky` (default on) computes the three bands
  **and the ambient** (ambient *is* the sky's contribution, which is why M16 gates hemispheric ambient
  on `sky`); authoring either anyway is the `daylight_overrides_sky` warning, naming the fix.
- **The horizon-sun shadow bug**, which day/night is the first thing to reach: a sun on the horizon
  casts shadows of unbounded length and one just below it casts them *upward*.
  `clamp_shadow_elevation` in `scene_renderer/shadow.rs` floors the direction used for the **shadow matrix**
  at 5° while the lighting direction keeps going. Above 5° it returns its input unchanged, which is
  why it costs every pre-M21 baseline nothing.
- **Scripts** get exactly two read-only getters, `world.time_of_day()` and `world.sun_altitude()`,
  evaluated once per step from `step * dt`. There is deliberately **no setter**: a script-settable
  clock is hidden state (invariant 2). Asking a scene with no `daylight` block for the time is a
  runtime error, not a plausible noon.

Fixture `verify/m21_daylight.json` + **five baselines from one file** at `--steps
120/390/720/1110/1320` (02:00, 06:30, noon, 18:30, 22:00) — `--steps` and not `--time` because the
lamp is script-driven. Bless from the **debug** binary (the fixture has trees). Not here: a sun disc
or a directional horizon glow (the natural next commit, in `sky_common.wgsl` on its own branch after
the untouchable lines), stars, clouds, real astronomy, moon shadows, and script-driven
`Material.emissive`.
