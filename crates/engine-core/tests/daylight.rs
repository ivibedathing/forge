//! Day and night (M21).
//!
//! Every test here is GPU-free and unconditional — there is no skip path,
//! because the whole system is a pure CPU function by design (M21's design,
//! §1). That is most of the point of building it this way.

use engine_core::daylight::{DaylightKeyframe, DaylightSettings};
use engine_core::math::Vec3;

fn settings() -> DaylightSettings {
    DaylightSettings::default()
}

/// Direction *to* the sun, which is easier to reason about than the travel
/// direction the light actually stores.
fn to_sun(s: &DaylightSettings, hours: f32) -> Vec3 {
    let mut s = s.clone();
    s.time_of_day = hours;
    s.day_length = 0.0;
    -s.evaluate(0.0).light_direction
}

// ---------------------------------------------------------------------------
// The arc
// ---------------------------------------------------------------------------

#[test]
fn noon_puts_the_sun_at_its_maximum_elevation_on_the_noon_bearing() {
    let mut s = settings();
    s.sun_elevation = 62.0;
    s.sun_azimuth = 0.0;
    s.time_of_day = 12.0;

    let day = s.evaluate(0.0);
    assert!(
        (day.sun_altitude - 62.0).abs() < 1e-3,
        "noon altitude was {}, expected the configured 62",
        day.sun_altitude
    );

    // Azimuth zero means the noon sun sits toward −Z, matching the engine's
    // aiming convention.
    let to = to_sun(&s, 12.0);
    assert!(to.x.abs() < 1e-4, "noon sun should have no X: {to:?}");
    assert!(to.z < 0.0, "noon sun should be toward −Z: {to:?}");
}

#[test]
fn the_sun_crosses_the_horizon_at_six_and_eighteen_at_every_elevation() {
    // Refusing to move sunrise with the season is what makes the palette
    // portable — an 18:00 keyframe is *the* sunset keyframe in every scene.
    for elevation in [12.0_f32, 35.0, 62.0, 89.0] {
        let mut s = settings();
        s.sun_elevation = elevation;

        for hour in [6.0_f32, 18.0] {
            s.time_of_day = hour;
            let altitude = s.evaluate(0.0).sun_altitude;
            assert!(
                altitude.abs() < 1e-3,
                "at elevation {elevation} the sun was at {altitude}° at {hour}:00, not on the horizon"
            );
        }
    }
}

#[test]
fn midnight_is_the_negation_of_noon() {
    let mut s = settings();
    s.sun_elevation = 55.0;

    s.time_of_day = 0.0;
    let midnight = s.evaluate(0.0).sun_altitude;
    assert!(
        (midnight + 55.0).abs() < 1e-3,
        "midnight altitude was {midnight}, expected −55"
    );
}

#[test]
fn the_sun_rises_east_of_the_noon_bearing_and_sets_west_of_it() {
    let s = settings();

    let dawn = to_sun(&s, 6.0);
    let dusk = to_sun(&s, 18.0);

    // Which way is east is not arbitrary, it just needs deriving once. A noon
    // sun toward −Z makes −Z south, so +Z is north; facing north in a
    // right-handed Y-up system puts east at cross(+Z, +Y) = −X. So the sun
    // rises toward −X and sets toward +X.
    assert!(
        dawn.x < -0.9,
        "sunrise should be toward −X (east), was {dawn:?}"
    );
    assert!(
        dusk.x > 0.9,
        "sunset should be toward +X (west), was {dusk:?}"
    );
    // Both on the horizon, so no vertical component.
    assert!(dawn.y.abs() < 1e-3 && dusk.y.abs() < 1e-3);
    // ...and they are opposites, which is the part that actually matters.
    assert!((dawn + dusk).length() < 1e-3);
}

#[test]
fn altitude_climbs_monotonically_through_the_morning() {
    let mut s = settings();
    let mut previous = f32::NEG_INFINITY;

    // Every six minutes from midnight to noon.
    for tick in 0..=120 {
        s.time_of_day = tick as f32 * 0.1;
        let altitude = s.evaluate(0.0).sun_altitude;
        assert!(
            altitude > previous - 1e-4,
            "altitude fell from {previous} to {altitude} at hour {}",
            s.time_of_day
        );
        previous = altitude;
    }
}

#[test]
fn sun_azimuth_rotates_the_whole_arc_without_changing_altitude() {
    let mut turned = settings();
    turned.sun_azimuth = 90.0;
    let straight = settings();

    for hour in [3.0_f32, 6.0, 9.0, 12.0, 15.0, 21.0] {
        let mut a = straight.clone();
        a.time_of_day = hour;
        let mut b = turned.clone();
        b.time_of_day = hour;
        assert!(
            (a.evaluate(0.0).sun_altitude - b.evaluate(0.0).sun_altitude).abs() < 1e-4,
            "azimuth changed the altitude at {hour}:00"
        );
    }

    // A quarter turn swings the noon bearing from −Z round to +X. The sun is
    // 62° up, so its whole horizontal component is only cos(62°) ≈ 0.47 —
    // what identifies the bearing is that all of it is in X and none in Z.
    let to = to_sun(&turned, 12.0);
    assert!(
        to.x > 0.4,
        "noon sun should have turned toward +X, was {to:?}"
    );
    assert!(
        to.z.abs() < 1e-4,
        "noon sun should have left the Z axis, was {to:?}"
    );
}

#[test]
fn the_light_direction_is_normalized_travel_all_day() {
    let mut s = settings();
    for tick in 0..240 {
        s.time_of_day = tick as f32 * 0.1;
        let dir = s.evaluate(0.0).light_direction;
        assert!(
            (dir.length() - 1.0).abs() < 1e-4,
            "light direction was not normalized at hour {}: {dir:?}",
            s.time_of_day
        );
    }
}

// ---------------------------------------------------------------------------
// The clock
// ---------------------------------------------------------------------------

#[test]
fn day_length_zero_is_genuinely_frozen() {
    let mut s = settings();
    s.time_of_day = 6.5;
    s.day_length = 0.0;

    let reference = s.evaluate(0.0);
    for time in [0.0_f32, 1.0, 60.0, 3600.0, 86_400.0] {
        assert_eq!(
            s.evaluate(time),
            reference,
            "a frozen day moved at time {time}"
        );
        assert!((s.hours_at(time) - 6.5).abs() < 1e-6);
    }
}

#[test]
fn a_cycling_day_returns_to_where_it_started() {
    let mut s = settings();
    s.time_of_day = 5.0;
    s.day_length = 24.0;

    for time in [0.0_f32, 3.5, 11.0, 19.25] {
        let a = s.evaluate(time);
        let b = s.evaluate(time + 24.0);
        assert!(
            (a.hours - b.hours).abs() < 1e-3,
            "t and t+day_length disagreed: {} vs {}",
            a.hours,
            b.hours
        );
        assert!((a.sun_altitude - b.sun_altitude).abs() < 1e-3);
    }
}

#[test]
fn day_length_twentyfour_makes_an_hour_a_second() {
    // The unit the fixtures use, because it reads itself: `--time 6.5` is
    // half past six in the morning.
    let mut s = settings();
    s.time_of_day = 0.0;
    s.day_length = 24.0;

    assert!((s.hours_at(6.5) - 6.5).abs() < 1e-5);
    assert!((s.hours_at(18.0) - 18.0).abs() < 1e-5);
    // And it wraps rather than running off the end of the day.
    assert!((s.hours_at(26.0) - 2.0).abs() < 1e-4);
}

#[test]
fn the_clock_wraps_across_midnight_rather_than_clamping() {
    let mut s = settings();
    s.time_of_day = 23.0;
    s.day_length = 24.0;

    assert!((s.hours_at(0.5) - 23.5).abs() < 1e-4);
    assert!(
        (s.hours_at(1.5) - 0.5).abs() < 1e-4,
        "did not wrap past midnight"
    );
    assert!((s.hours_at(2.5) - 1.5).abs() < 1e-4);
}

// ---------------------------------------------------------------------------
// The palette
// ---------------------------------------------------------------------------

#[test]
fn the_noon_keyframe_is_exactly_the_m16_clear_day_defaults() {
    // The one hour where the daylight model and every hand-authored scene in
    // the repo can be checked against each other.
    let mut s = settings();
    s.time_of_day = 12.0;
    let day = s.evaluate(0.0);

    assert_eq!(day.sky_zenith, Vec3::new(0.19, 0.34, 0.62));
    assert_eq!(day.sky_horizon, Vec3::new(0.62, 0.71, 0.82));
    assert_eq!(day.sky_ground, Vec3::new(0.16, 0.16, 0.17));
    assert_eq!(day.fog_scale, 1.0);
}

#[test]
fn the_palette_interpolates_across_midnight_instead_of_racing_backwards() {
    // A two-keyframe palette with a red 22:00 and a green 02:00: the small
    // hours must run red → green, not sprint the long way round through the
    // whole day.
    let palette = vec![
        keyframe(2.0, Vec3::new(0.0, 1.0, 0.0)),
        keyframe(22.0, Vec3::new(1.0, 0.0, 0.0)),
    ];
    let mut s = settings();
    s.palette = Some(palette);
    s.drives_sky = true;

    // Midnight is the halfway point of the 22:00 → 02:00 span.
    s.time_of_day = 0.0;
    let midnight = s.evaluate(0.0);
    assert!(
        (midnight.sky_zenith.x - 0.5).abs() < 1e-3 && (midnight.sky_zenith.y - 0.5).abs() < 1e-3,
        "midnight should be halfway between the two keyframes, was {:?}",
        midnight.sky_zenith
    );

    // A quarter of the way in, at 23:00, it should still be mostly red.
    s.time_of_day = 23.0;
    let late = s.evaluate(0.0);
    assert!(
        late.sky_zenith.x > late.sky_zenith.y,
        "23:00 should still be mostly red, was {:?}",
        late.sky_zenith
    );
}

#[test]
fn a_keyframe_hour_reproduces_that_keyframe_exactly() {
    let s = settings();
    for key in DaylightSettings::builtin_palette() {
        let mut at = s.clone();
        at.time_of_day = key.hour;
        let day = at.evaluate(0.0);
        assert_eq!(
            day.sky_zenith, key.sky_zenith,
            "landing on keyframe {} did not reproduce it",
            key.hour
        );
        assert_eq!(day.fog_scale, key.fog_scale);
    }
}

#[test]
fn the_palette_is_sorted_and_in_range() {
    // The builtin table is the one palette validation never sees, so it
    // checks itself.
    let palette = DaylightSettings::builtin_palette();
    assert!(palette.len() >= 2);
    for pair in palette.windows(2) {
        assert!(
            pair[0].hour < pair[1].hour,
            "builtin palette is out of order at {} / {}",
            pair[0].hour,
            pair[1].hour
        );
    }
    for key in palette {
        assert!((0.0..24.0).contains(&key.hour));
        assert!(key.sun_intensity >= 0.0 && key.ambient_intensity >= 0.0);
        assert!(key.fog_scale >= 0.0);
    }
}

#[test]
fn fog_is_a_scale_so_a_clear_scene_stays_clear_all_day() {
    // The reason the palette carries a multiplier and not a density: a
    // daylight block must never switch fog on in a scene that never asked
    // for any.
    let s = settings();
    for tick in 0..240 {
        let mut at = s.clone();
        at.time_of_day = tick as f32 * 0.1;
        assert!(
            at.evaluate(0.0).fog_scale.is_finite() && at.evaluate(0.0).fog_scale >= 0.0,
            "fog scale went out of range at hour {}",
            at.time_of_day
        );
    }
    // The scaling itself is Scene::resolved_at's job; what this pins is that
    // the multiplier is always a sane finite number to multiply by.
    let mut noon = s.clone();
    noon.time_of_day = 12.0;
    assert_eq!(
        noon.evaluate(0.0).fog_scale,
        1.0,
        "noon should not alter fog"
    );
}

// ---------------------------------------------------------------------------
// The sun/moon handoff — the one genuinely delicate part
// ---------------------------------------------------------------------------

#[test]
fn the_moon_takes_over_after_dusk_and_hands_back_before_dawn() {
    let s = settings();

    for (hour, expect_sun) in [
        (0.0_f32, false),
        (3.0, false),
        (8.0, true),
        (12.0, true),
        (16.0, true),
        (22.0, false),
    ] {
        let mut at = s.clone();
        at.time_of_day = hour;
        let day = at.evaluate(0.0);
        assert_eq!(
            day.sun_is_dominant, expect_sun,
            "at {hour}:00 the dominant body was wrong (sun_is_dominant = {})",
            day.sun_is_dominant
        );
    }
}

#[test]
fn the_light_never_jumps_in_brightness_across_the_whole_day() {
    // The handoff swaps bodies where their luminances are equal, so the
    // magnitude is continuous by construction. This walks the day at one-minute
    // resolution and refuses any step that changes brightness abruptly.
    let mut s = settings();
    s.time_of_day = 0.0;
    s.day_length = 24.0;

    let lum = |c: Vec3| 0.2126 * c.x + 0.7152 * c.y + 0.0722 * c.z;

    let mut previous = lum(s.evaluate(0.0).light_color);
    for minute in 1..=(24 * 60) {
        let now = lum(s.evaluate(minute as f32 / 60.0).light_color);
        assert!(
            (now - previous).abs() < 0.02,
            "light brightness jumped from {previous} to {now} at minute {minute}"
        );
        previous = now;
    }
}

#[test]
fn the_direction_switch_happens_while_there_is_almost_no_light_to_notice_it_by() {
    // The direction *does* pop — the design says so, because crossfading it
    // would aim the light at a patch of sky where nothing is. What makes that
    // acceptable is that it happens in near-darkness, and that is a property
    // of the palette keeping `sun_intensity` near zero at the horizon hours.
    // If someone retunes the palette and brightens dusk, this fails, which is
    // exactly when it should.
    let mut s = settings();
    s.time_of_day = 0.0;
    s.day_length = 24.0;

    let lum = |c: Vec3| 0.2126 * c.x + 0.7152 * c.y + 0.0722 * c.z;

    let mut switches = 0;
    let mut previous = s.evaluate(0.0);
    for minute in 1..=(24 * 60) {
        let now = s.evaluate(minute as f32 / 60.0);
        if now.sun_is_dominant != previous.sun_is_dominant {
            switches += 1;
            let brightness = lum(now.light_color).max(lum(previous.light_color));
            assert!(
                brightness < 0.08,
                "the light switched bodies at minute {minute} while carrying \
                 luminance {brightness} — bright enough to see the direction pop"
            );
        }
        previous = now;
    }

    assert_eq!(switches, 2, "expected exactly one handoff each way per day");
}

#[test]
fn a_daylight_scene_is_never_pitch_black() {
    // Moonlight plus the night ambient must leave something to see by: a
    // black frame tells an agent nothing, which is the same argument
    // `AmbientLight` exists for.
    let mut s = settings();
    for tick in 0..240 {
        s.time_of_day = tick as f32 * 0.1;
        let day = s.evaluate(0.0);
        let total = day.light_color + day.ambient;
        assert!(
            total.max_element() > 0.01,
            "the scene went black at hour {}: light {:?} ambient {:?}",
            s.time_of_day,
            day.light_color,
            day.ambient
        );
    }
}

#[test]
fn moon_intensity_zero_leaves_the_night_lit_only_by_ambient() {
    let mut s = settings();
    s.moon_intensity = 0.0;
    s.time_of_day = 1.0;

    let day = s.evaluate(0.0);
    assert_eq!(
        day.light_color,
        Vec3::ZERO,
        "a moonless night has no direct light"
    );
    assert!(
        day.ambient.max_element() > 0.0,
        "ambient should still be there"
    );
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// A keyframe that varies only in `hour` and the sky bands, for palette tests.
fn keyframe(hour: f32, color: Vec3) -> DaylightKeyframe {
    DaylightKeyframe {
        hour,
        sun_color: Vec3::ONE,
        sun_intensity: 1.0,
        ambient_color: Vec3::ONE,
        ambient_intensity: 0.2,
        sky_zenith: color,
        sky_horizon: color,
        sky_ground: color,
        fog_scale: 1.0,
    }
}
