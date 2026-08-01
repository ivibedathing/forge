//! Day and night (M21): the time of day as a pure function of the clock.
//!
//! The whole system is
//!
//! ```text
//! (DaylightSettings, time) -> Daylight
//! ```
//!
//! and a [`Daylight`] is nothing but values the engine already had fields for:
//! a directional light, an ambient term, three sky band colors, and a fog
//! multiplier. Nothing here reaches the GPU on its own — the caller folds a
//! `Daylight` into the [`ResolvedLights`](crate::scene::ResolvedLights) and
//! [`EnvironmentSettings`](crate::scene::EnvironmentSettings) it was going to
//! upload anyway.
//!
//! That is the central decision of the milestone (M21's design, §1) and
//! it is what makes this module GPU-free and unconditionally testable: no
//! shader changed, so M16's bit-exactness rules cannot be tripped, and every
//! rule in here is an ordinary unit test with no skip path.

use glam::Vec3;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// How far above and below the horizon the moon fades in, in degrees.
///
/// A private constant rather than a scene field: it is the width of twilight,
/// not something a scene has an opinion about, and every field the format does
/// not have is a field an agent cannot get wrong.
const HORIZON_SOFTNESS_DEGREES: f32 = 6.0;

/// The scene-level `daylight` block — a sibling of `physics` and
/// `environment`, not a field inside either.
///
/// It sits outside `environment` for two reasons (design §2). Conceptually
/// this is a *clock-driven system*, closer to `physics`, and it **produces**
/// environment values rather than being one. Mechanically, `EnvironmentSettings`
/// is `Copy` because lights resolve every frame in the viewer; an optional
/// keyframe palette is a `Vec`, and hanging it there would cost that type its
/// `Copy` and put a clone in the per-frame path.
///
/// **A scene with no `daylight` block renders exactly as it did before this
/// module existed, byte for byte.** That is the M16 contract repeated, and it
/// is why nineteen existing baselines did not move.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct DaylightSettings {
    /// Hours in `[0, 24)` — `6.5` is half past six in the morning. When the
    /// day is cycling this is the hour at scene time zero.
    #[schemars(range(min = 0.0, max = 24.0))]
    pub time_of_day: f32,

    /// Seconds of scene time per full 24-hour cycle. `0` (the default) freezes
    /// the day, which is what most scenes want: a dial, not motion, and a
    /// screenshot reproducible from the file with no `--time` at all.
    ///
    /// `24.0` makes an hour a second, so `--time 6.5` is 6:30 in the morning.
    /// That is the unit the fixtures use because it reads itself.
    #[schemars(range(min = 0.0))]
    pub day_length: f32,

    /// Degrees above the horizon the sun reaches at noon. `90` is overhead,
    /// `20` is a winter afternoon that never really gets going.
    ///
    /// This one number replaces latitude, longitude, date, and axial tilt.
    #[schemars(extend("exclusiveMinimum" = 0.0), range(max = 90.0))]
    pub sun_elevation: f32,

    /// Compass bearing of the noon sun in degrees, rotating the whole arc
    /// about Y. `0` puts noon toward −Z, matching the engine's aiming
    /// convention. Unbounded — it wraps.
    pub sun_azimuth: f32,

    /// Degrees above the horizon the moon reaches at its highest. It rides the
    /// same arc as the sun offset by twelve hours; giving it its own elevation
    /// is what keeps it from being a mechanical anti-sun.
    #[schemars(extend("exclusiveMinimum" = 0.0), range(max = 90.0))]
    pub moon_elevation: f32,

    /// Linear RGB chromaticity of moonlight. Magnitude lives in
    /// `moon_intensity`, like every other light in the engine.
    #[schemars(with = "[f32; 3]", inner(range(min = 0.0, max = 1.0)))]
    pub moon_color: Vec3,

    /// Brightness of moonlight. The default is deliberately tiny: a moonlit
    /// scene should read as *navigable*, not as a dim afternoon.
    #[schemars(range(min = 0.0))]
    pub moon_intensity: f32,

    /// Synthesize the sun's direction, color, and intensity.
    ///
    /// With this on (the default) a scene needs **no** `DirectionalLight`
    /// entity, and authoring one anyway is `daylight_and_directional_light`.
    /// Two owners of one sun is what invariant 8 exists to prevent: a rotation
    /// in a text file that is silently ignored, or silently overwritten, is a
    /// value that does not mean what it says.
    ///
    /// Set it `false` and daylight paints the sky, ambient, and fog only,
    /// leaving a hand-aimed `DirectionalLight` its job. That is the artist
    /// scene, and it is one field.
    pub drives_sun: bool,

    /// Compute `environment.sky_zenith` / `sky_horizon` / `sky_ground` **and
    /// the ambient term** from the palette.
    ///
    /// Ambient rides with the sky rather than with the sun because it *is*
    /// the sky's contribution — which is why M16 gates hemispheric ambient on
    /// `sky` in the first place. A scene that authored non-default band colors
    /// or its own `AmbientLight` and leaves this on gets the
    /// `daylight_overrides_sky` warning.
    pub drives_sky: bool,

    /// The color table over the day. Absent means [`DaylightSettings::builtin_palette`].
    ///
    /// Every field of a keyframe is required: a half-specified keyframe
    /// silently interpolating toward black is a worse failure than a
    /// validation error.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub palette: Option<Vec<DaylightKeyframe>>,
}

impl Default for DaylightSettings {
    fn default() -> Self {
        Self {
            time_of_day: 12.0,
            day_length: 0.0,
            sun_elevation: 62.0,
            sun_azimuth: 0.0,
            moon_elevation: 48.0,
            moon_color: Vec3::new(0.55, 0.66, 0.95),
            moon_intensity: 0.06,
            drives_sun: true,
            drives_sky: true,
            palette: None,
        }
    }
}

/// One entry in the day's color table.
///
/// The palette carries the **sun's** color and intensity, not the directional
/// light's in general — the moon has its own two fields on
/// [`DaylightSettings`], because it does not meaningfully change color through
/// a night and would otherwise need repeating in every night keyframe.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DaylightKeyframe {
    /// Hours in `[0, 24)`.
    #[schemars(range(min = 0.0, max = 24.0))]
    pub hour: f32,

    /// Linear RGB chromaticity of direct sunlight at this hour.
    #[schemars(with = "[f32; 3]", inner(range(min = 0.0, max = 1.0)))]
    pub sun_color: Vec3,

    /// Brightness of direct sunlight at this hour.
    ///
    /// It lives in the table rather than falling out of `sin(altitude)`
    /// because a sunset's redness and its dimness are one artistic decision,
    /// and splitting them across a physical falloff and a color curve means
    /// retuning a dusk in two places that then disagree.
    ///
    /// **Keep it near zero at the horizon hours.** The sun/moon handoff
    /// (§ [`Daylight`]) switches the light's direction where the two bodies
    /// contribute equally, and how visible that switch is *is* the sun's
    /// intensity there. A test pins it.
    #[schemars(range(min = 0.0))]
    pub sun_intensity: f32,

    /// Linear RGB chromaticity of the ambient fill.
    #[schemars(with = "[f32; 3]", inner(range(min = 0.0, max = 1.0)))]
    pub ambient_color: Vec3,

    /// Brightness of the ambient fill.
    #[schemars(range(min = 0.0))]
    pub ambient_intensity: f32,

    /// Sky color straight overhead. Unclamped above 1, like
    /// `environment.sky_zenith`: a sky is a light source, and clamping it to
    /// reflectance range makes noon look like dusk.
    #[schemars(with = "[f32; 3]", inner(range(min = 0.0)))]
    pub sky_zenith: Vec3,

    /// Sky color at the horizon — which is also the fog color, since
    /// `environment` has one field for both.
    #[schemars(with = "[f32; 3]", inner(range(min = 0.0)))]
    pub sky_horizon: Vec3,

    /// Color below the horizon.
    #[schemars(with = "[f32; 3]", inner(range(min = 0.0)))]
    pub sky_ground: Vec3,

    /// Multiplier on the scene's authored `environment.fog_density`.
    ///
    /// A **scale**, not a density, deliberately (design §5): a scene with
    /// `fog_density: 0` stays clear all day however misty the palette's dawn
    /// is, and a scene that wants fog authors it once and gets the dawn
    /// thickening for free. An absolute density here would mean a daylight
    /// block silently switching fog on in a scene that never asked for any.
    #[schemars(range(min = 0.0))]
    pub fog_scale: f32,
}

/// The day evaluated at one instant: values ready to fold into the lights and
/// the environment the renderer was going to receive anyway.
///
/// `Copy`, for the same reason `ResolvedLights` is: the viewer evaluates this
/// every frame and a heap allocation per frame would buy nothing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Daylight {
    /// Hours in `[0, 24)` after applying `day_length`.
    pub hours: f32,

    /// The **sun's** altitude in degrees, negative when it is down. Reported
    /// separately from the light because scripts ask about the sun even when
    /// the moon is the one lighting the scene.
    pub sun_altitude: f32,

    /// Travel direction of the dominant body, normalized — the value a
    /// `DirectionalLight`'s transform would have produced.
    pub light_direction: Vec3,

    /// Color of the dominant body, already premultiplied by its intensity.
    pub light_color: Vec3,

    /// Ambient fill, already premultiplied by its intensity.
    pub ambient: Vec3,

    pub sky_zenith: Vec3,
    pub sky_horizon: Vec3,
    pub sky_ground: Vec3,

    /// Multiplier on the scene's authored fog density.
    pub fog_scale: f32,

    /// True when the sun is the dominant body. Diagnostic; the renderer does
    /// not branch on it.
    pub sun_is_dominant: bool,
}

/// Where a celestial body is: altitude and azimuth in **radians**.
#[derive(Debug, Clone, Copy, PartialEq)]
struct SkyPosition {
    altitude: f32,
    azimuth: f32,
}

impl SkyPosition {
    /// Direction **to** the body in world space.
    ///
    /// Y-up, and azimuth zero points toward −Z, matching the engine's aiming
    /// convention (a camera and a light both look down local −Z).
    ///
    /// Which way that makes east is worth deriving once rather than
    /// rediscovering: a noon sun toward −Z makes −Z south, so +Z is north, and
    /// facing north in a right-handed Y-up system puts east at
    /// `cross(+Z, +Y) = −X`. **The sun rises toward −X and sets toward +X.**
    fn to_body(self) -> Vec3 {
        let (sin_alt, cos_alt) = self.altitude.sin_cos();
        let (sin_az, cos_az) = self.azimuth.sin_cos();
        Vec3::new(cos_alt * sin_az, sin_alt, -cos_alt * cos_az)
    }

    /// Travel direction of the light *from* the body — what a
    /// `DirectionalLight` stores.
    fn travel(self) -> Vec3 {
        -self.to_body()
    }
}

/// The arc a body traces across the sky.
///
/// A great circle tilted so its peak is `max_elevation`: noon gives that
/// altitude at the noon bearing, midnight gives its negation on the opposite
/// bearing, and the body crosses the horizon rising 90° east of the noon
/// bearing.
///
/// **Sunrise is 06:00 and sunset is 18:00 at every elevation**, and refusing
/// to move them with the season is what makes the palette portable: a keyframe
/// at 18:00 is *the* sunset keyframe in every scene, rather than a color that
/// lands at the wrong moment when someone edits `sun_elevation`.
fn arc(hours: f32, max_elevation_degrees: f32, noon_azimuth_degrees: f32) -> SkyPosition {
    // Hour angle: zero at noon, ±π at midnight.
    let h = (hours - 12.0) / 12.0 * std::f32::consts::PI;
    let e = max_elevation_degrees.to_radians();
    let (sin_h, cos_h) = h.sin_cos();
    let (sin_e, _) = e.sin_cos();

    SkyPosition {
        altitude: (sin_e * cos_h).clamp(-1.0, 1.0).asin(),
        azimuth: noon_azimuth_degrees.to_radians() + sin_h.atan2(cos_h * sin_e),
    }
}

/// The classic smooth Hermite step, `0` at or below `edge0` and `1` at or
/// above `edge1`.
fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    if edge1 <= edge0 {
        return if x < edge0 { 0.0 } else { 1.0 };
    }
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Rec. 709 luminance — used only to decide which body is brighter, never to
/// produce a color.
fn luminance(c: Vec3) -> f32 {
    0.2126 * c.x + 0.7152 * c.y + 0.0722 * c.z
}

impl DaylightSettings {
    /// The built-in color table: eight keyframes from deep night through a
    /// golden morning to a golden evening and back.
    ///
    /// **The noon keyframe is exactly the M16 clear-day defaults**, so a
    /// daylight scene at noon looks like a hand-authored scene does today —
    /// the two models agree at the one hour anyone can check against existing
    /// work.
    ///
    /// Note how small `sun_intensity` is at 06:00 and 18:00 against the golden
    /// hours either side of them. That is not timidity: the sun at the horizon
    /// really is dim and red, and it is also what keeps the sun/moon direction
    /// handoff invisible.
    pub fn builtin_palette() -> &'static [DaylightKeyframe] {
        // Nine parameters because a keyframe *has* nine fields, all of them
        // required (a half-specified keyframe fading to black is worse than an
        // error). This is a positional constructor for a literal table below;
        // taking the struct itself would mean spelling out eight field names
        // per row and turn a readable palette into three screens of noise.
        #[allow(clippy::too_many_arguments)]
        const fn kf(
            hour: f32,
            sun_color: [f32; 3],
            sun_intensity: f32,
            ambient_color: [f32; 3],
            ambient_intensity: f32,
            sky_zenith: [f32; 3],
            sky_horizon: [f32; 3],
            sky_ground: [f32; 3],
            fog_scale: f32,
        ) -> DaylightKeyframe {
            DaylightKeyframe {
                hour,
                sun_color: Vec3::new(sun_color[0], sun_color[1], sun_color[2]),
                sun_intensity,
                ambient_color: Vec3::new(ambient_color[0], ambient_color[1], ambient_color[2]),
                ambient_intensity,
                sky_zenith: Vec3::new(sky_zenith[0], sky_zenith[1], sky_zenith[2]),
                sky_horizon: Vec3::new(sky_horizon[0], sky_horizon[1], sky_horizon[2]),
                sky_ground: Vec3::new(sky_ground[0], sky_ground[1], sky_ground[2]),
                fog_scale,
            }
        }

        // Deep night, astronomical dawn, sunrise, golden morning, noon, golden
        // evening, sunset, dusk — then the wrap back to deep night.
        static PALETTE: [DaylightKeyframe; 8] = [
            kf(
                0.0,
                [0.50, 0.60, 0.90],
                0.0,
                [0.28, 0.34, 0.52],
                0.045,
                [0.008, 0.013, 0.030],
                [0.020, 0.030, 0.055],
                [0.006, 0.007, 0.012],
                1.4,
            ),
            kf(
                4.5,
                [0.62, 0.58, 0.72],
                0.0,
                [0.30, 0.34, 0.50],
                0.070,
                [0.020, 0.035, 0.080],
                [0.075, 0.070, 0.105],
                [0.012, 0.013, 0.020],
                1.9,
            ),
            kf(
                6.0,
                [1.00, 0.34, 0.12],
                0.16,
                [0.45, 0.42, 0.52],
                0.155,
                [0.075, 0.130, 0.290],
                [0.640, 0.340, 0.230],
                [0.045, 0.040, 0.045],
                2.4,
            ),
            kf(
                6.7,
                [1.00, 0.62, 0.30],
                0.72,
                [0.52, 0.52, 0.64],
                0.185,
                [0.105, 0.190, 0.410],
                [0.660, 0.510, 0.430],
                [0.080, 0.075, 0.080],
                1.8,
            ),
            kf(
                12.0,
                [1.00, 0.98, 0.94],
                1.35,
                [0.60, 0.68, 0.85],
                0.220,
                [0.190, 0.340, 0.620],
                [0.620, 0.710, 0.820],
                [0.160, 0.160, 0.170],
                1.0,
            ),
            kf(
                17.3,
                [1.00, 0.62, 0.28],
                0.72,
                [0.52, 0.50, 0.60],
                0.180,
                [0.100, 0.180, 0.400],
                [0.680, 0.480, 0.380],
                [0.078, 0.070, 0.074],
                1.8,
            ),
            kf(
                18.0,
                [1.00, 0.32, 0.10],
                0.16,
                [0.48, 0.40, 0.46],
                0.150,
                [0.070, 0.120, 0.280],
                [0.720, 0.320, 0.190],
                [0.048, 0.040, 0.042],
                2.2,
            ),
            kf(
                19.5,
                [0.55, 0.42, 0.60],
                0.0,
                [0.33, 0.36, 0.54],
                0.075,
                [0.022, 0.038, 0.090],
                [0.090, 0.075, 0.115],
                [0.013, 0.014, 0.022],
                1.8,
            ),
        ];

        &PALETTE
    }

    /// The palette this scene uses.
    pub fn palette(&self) -> &[DaylightKeyframe] {
        match &self.palette {
            Some(custom) => custom,
            None => Self::builtin_palette(),
        }
    }

    /// The hour of the day at `time` seconds of scene time.
    ///
    /// `time` is the reproducible clock water already rides: `--time T` when
    /// given, otherwise `steps / timestep_hz`. A scene with water *and*
    /// daylight has one clock, not two.
    pub fn hours_at(&self, time: f32) -> f32 {
        if self.day_length <= 0.0 || !self.day_length.is_finite() {
            return self.time_of_day.rem_euclid(24.0);
        }
        (self.time_of_day + time * 24.0 / self.day_length).rem_euclid(24.0)
    }

    /// Evaluate the whole day at `time` seconds of scene time.
    ///
    /// # The sun/moon handoff
    ///
    /// There is exactly **one** directional light, which is what keeps the
    /// single shadow map sufficient. The light *is* the dominant body —
    /// direction, color, and intensity, with no summing of the two.
    ///
    /// That is what makes it continuous. The bodies swap where their
    /// luminances are equal, so the light's brightness does not jump at the
    /// crossover; only its hue and its direction change, and both do so at the
    /// moment when there is least light to notice it by. Summing the two
    /// instead would keep an orange sunset arriving from the moon's side of
    /// the sky for the whole of twilight, and crossfading the *direction*
    /// would aim the light at a patch of sky where nothing is.
    pub fn evaluate(&self, time: f32) -> Daylight {
        let hours = self.hours_at(time);
        let key = sample_palette(self.palette(), hours);

        let sun = arc(hours, self.sun_elevation, self.sun_azimuth);
        // The moon rides the same arc half a day out of phase.
        let moon = arc(hours + 12.0, self.moon_elevation, self.sun_azimuth);

        let sun_color = key.sun_color * key.sun_intensity;

        // The moon fades in across the horizon rather than popping on when it
        // crosses it. The sun needs no such window: the palette author already
        // fades it, which is what `sun_intensity` is for.
        let moon_weight = smoothstep(
            -HORIZON_SOFTNESS_DEGREES.to_radians(),
            HORIZON_SOFTNESS_DEGREES.to_radians(),
            moon.altitude,
        );
        let moon_color = self.moon_color * self.moon_intensity * moon_weight;

        let sun_is_dominant = luminance(sun_color) >= luminance(moon_color);
        let (light_direction, light_color) = if sun_is_dominant {
            (sun.travel(), sun_color)
        } else {
            (moon.travel(), moon_color)
        };

        Daylight {
            hours,
            sun_altitude: sun.altitude.to_degrees(),
            light_direction,
            light_color,
            ambient: key.ambient_color * key.ambient_intensity,
            sky_zenith: key.sky_zenith,
            sky_horizon: key.sky_horizon,
            sky_ground: key.sky_ground,
            fog_scale: key.fog_scale,
            sun_is_dominant,
        }
    }
}

/// Interpolate the palette at `hours`, **wrapping across midnight**.
///
/// A 21:00 keyframe and a 00:00 keyframe interpolate through the small hours
/// rather than racing backwards through the day. Interpolation is linear in
/// linear RGB, because every color in this engine is linear and light adds
/// linearly — lerping a sunset through sRGB would darken its middle.
///
/// The table is required to be sorted and to hold at least two keyframes
/// (`daylight_palette_invalid`), so the only degenerate case reachable here is
/// a palette that never loaded.
fn sample_palette(palette: &[DaylightKeyframe], hours: f32) -> DaylightKeyframe {
    match palette.len() {
        0 => *DaylightSettings::builtin_palette()
            .first()
            .expect("the builtin palette is never empty"),
        1 => palette[0],
        _ => {
            // The last keyframe at or before `hours`; with none, the final
            // keyframe of the previous day.
            let index = palette
                .iter()
                .rposition(|k| k.hour <= hours)
                .unwrap_or(palette.len() - 1);
            let prev = palette[index];
            let next = palette[(index + 1) % palette.len()];

            let span = (next.hour - prev.hour).rem_euclid(24.0);
            let t = if span <= 0.0 {
                0.0
            } else {
                ((hours - prev.hour).rem_euclid(24.0) / span).clamp(0.0, 1.0)
            };

            DaylightKeyframe {
                hour: hours,
                sun_color: prev.sun_color.lerp(next.sun_color, t),
                sun_intensity: prev.sun_intensity + (next.sun_intensity - prev.sun_intensity) * t,
                ambient_color: prev.ambient_color.lerp(next.ambient_color, t),
                ambient_intensity: prev.ambient_intensity
                    + (next.ambient_intensity - prev.ambient_intensity) * t,
                sky_zenith: prev.sky_zenith.lerp(next.sky_zenith, t),
                sky_horizon: prev.sky_horizon.lerp(next.sky_horizon, t),
                sky_ground: prev.sky_ground.lerp(next.sky_ground, t),
                fog_scale: prev.fog_scale + (next.fog_scale - prev.fog_scale) * t,
            }
        }
    }
}
