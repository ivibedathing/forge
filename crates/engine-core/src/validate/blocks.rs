//! The scene-level blocks: `physics`, `environment` (M16) and `daylight`
//! (M21). Hand-validated rather than schema-driven — they are not components,
//! so the component walk never sees them.

use serde_json::Value;

use crate::codes;
use crate::error::EngineError;

use super::Cx;

/// Validate the scene-level `physics` block by hand (the top-level walk is
/// hand-written; the block is small enough to keep it that way).
pub(super) fn check_physics_block(cx: &Cx<'_>, physics: &Value, errors: &mut Vec<EngineError>) {
    let Some(object) = physics.as_object() else {
        errors.push(cx.wrong_type("physics", "object", physics, "/physics"));
        return;
    };

    for key in object.keys() {
        if key != "gravity" && key != "timestep_hz" {
            errors.push(
                cx.err(
                    codes::UNKNOWN_FIELD,
                    format!("the physics block has no field {key:?}"),
                    &format!("/physics/{key}"),
                )
                .field(key)
                .suggest_from(key, ["gravity", "timestep_hz"]),
            );
        }
    }

    if let Some(gravity) = object.get("gravity") {
        match gravity.as_array() {
            Some(items) if items.len() == 3 && items.iter().all(Value::is_number) => {}
            _ => errors.push(
                cx.wrong_type("gravity", "array", gravity, "/physics/gravity")
                    .field("gravity"),
            ),
        }
    }

    if let Some(hz) = object.get("timestep_hz") {
        let valid = hz.as_u64().is_some_and(|v| v >= 1);
        if !valid {
            errors.push(
                cx.err(
                    codes::INVALID_PHYSICS_VALUE,
                    format!("physics.timestep_hz is {hz}; it must be an integer of at least 1"),
                    "/physics/timestep_hz",
                )
                .field("timestep_hz"),
            );
        }
    }
}

/// Validate the scene-level `environment` block (M16), hand-written like
/// [`check_physics_block`] and for the same reason.
pub(super) fn check_environment_block(
    cx: &Cx<'_>,
    environment: &Value,
    errors: &mut Vec<EngineError>,
) {
    const COLORS: [&str; 3] = ["sky_zenith", "sky_horizon", "sky_ground"];
    const FLAGS: [&str; 2] = ["sky", "shadows"];
    const KNOWN: [&str; 8] = [
        "sky",
        "sky_zenith",
        "sky_horizon",
        "sky_ground",
        "fog_density",
        "shadows",
        "shadow_distance",
        "samples",
    ];

    let Some(object) = environment.as_object() else {
        errors.push(cx.wrong_type("environment", "object", environment, "/environment"));
        return;
    };

    for key in object.keys() {
        if !KNOWN.contains(&key.as_str()) {
            errors.push(
                cx.err(
                    codes::UNKNOWN_FIELD,
                    format!("the environment block has no field {key:?}"),
                    &format!("/environment/{key}"),
                )
                .field(key)
                .suggest_from(key, KNOWN),
            );
        }
    }

    for name in FLAGS {
        if let Some(value) = object.get(name) {
            if !value.is_boolean() {
                errors.push(
                    cx.wrong_type(name, "boolean", value, &format!("/environment/{name}"))
                        .field(name),
                );
            }
        }
    }

    for name in COLORS {
        if let Some(value) = object.get(name) {
            let ok = value
                .as_array()
                .is_some_and(|items| items.len() == 3 && items.iter().all(Value::is_number));
            if !ok {
                errors.push(
                    cx.wrong_type(name, "array", value, &format!("/environment/{name}"))
                        .field(name),
                );
            }
        }
    }

    if let Some(density) = object.get("fog_density") {
        let valid = density.as_f64().is_some_and(|v| v >= 0.0 && v.is_finite());
        if !valid {
            errors.push(
                cx.err(
                    codes::INVALID_ENVIRONMENT_VALUE,
                    format!("environment.fog_density is {density}; it must be a number >= 0"),
                    "/environment/fog_density",
                )
                .field("fog_density"),
            );
        }
    }

    if let Some(distance) = object.get("shadow_distance") {
        let valid = distance.as_f64().is_some_and(|v| v > 0.0 && v.is_finite());
        if !valid {
            errors.push(
                cx.err(
                    codes::INVALID_ENVIRONMENT_VALUE,
                    format!(
                        "environment.shadow_distance is {distance}; it must be a number greater than 0"
                    ),
                    "/environment/shadow_distance",
                )
                .field("shadow_distance"),
            );
        }
    }

    // 1 or 4 and nothing between: every other count would need its own set of
    // pipelines, and a scene asking for 2 should be told so rather than
    // silently rounded to something it did not write.
    if let Some(samples) = object.get("samples") {
        let valid = samples.as_u64().is_some_and(|v| v == 1 || v == 4);
        if !valid {
            errors.push(
                cx.err(
                    codes::INVALID_ENVIRONMENT_VALUE,
                    format!("environment.samples is {samples}; it must be 1 or 4"),
                    "/environment/samples",
                )
                .field("samples"),
            );
        }
    }
}

/// The scene-level `daylight` block (M21), hand-validated like `physics` and
/// `environment` rather than walked from the schema.
///
/// `environment` comes in so the `daylight_overrides_sky` warning can see
/// whether the scene also authored sky colors that nothing will read.
pub(super) fn check_daylight_block(
    cx: &Cx<'_>,
    daylight: &Value,
    environment: Option<&Value>,
    errors: &mut Vec<EngineError>,
) {
    const FLAGS: [&str; 2] = ["drives_sun", "drives_sky"];
    const KNOWN: [&str; 10] = [
        "time_of_day",
        "day_length",
        "sun_elevation",
        "sun_azimuth",
        "moon_elevation",
        "moon_color",
        "moon_intensity",
        "drives_sun",
        "drives_sky",
        "palette",
    ];

    let Some(object) = daylight.as_object() else {
        errors.push(cx.wrong_type("daylight", "object", daylight, "/daylight"));
        return;
    };

    for key in object.keys() {
        if !KNOWN.contains(&key.as_str()) {
            errors.push(
                cx.err(
                    codes::UNKNOWN_FIELD,
                    format!("the daylight block has no field {key:?}"),
                    &format!("/daylight/{key}"),
                )
                .field(key)
                .suggest_from(key, KNOWN),
            );
        }
    }

    for name in FLAGS {
        if let Some(value) = object.get(name) {
            if !value.is_boolean() {
                errors.push(
                    cx.wrong_type(name, "boolean", value, &format!("/daylight/{name}"))
                        .field(name),
                );
            }
        }
    }

    // (field, low, high, low is exclusive, high is exclusive, prose)
    let ranges: [(&str, f64, f64, bool, bool, &str); 6] = [
        ("time_of_day", 0.0, 24.0, false, true, "hours in [0, 24)"),
        (
            "day_length",
            0.0,
            f64::INFINITY,
            false,
            false,
            "a number >= 0",
        ),
        (
            "sun_elevation",
            0.0,
            90.0,
            true,
            false,
            "degrees in (0, 90]",
        ),
        (
            "sun_azimuth",
            f64::NEG_INFINITY,
            f64::INFINITY,
            false,
            false,
            "a finite number of degrees",
        ),
        (
            "moon_elevation",
            0.0,
            90.0,
            true,
            false,
            "degrees in (0, 90]",
        ),
        (
            "moon_intensity",
            0.0,
            f64::INFINITY,
            false,
            false,
            "a number >= 0",
        ),
    ];

    for (name, low, high, low_exclusive, high_exclusive, prose) in ranges {
        let Some(value) = object.get(name) else {
            continue;
        };
        let ok = value.as_f64().is_some_and(|v| {
            v.is_finite()
                && (if low_exclusive { v > low } else { v >= low })
                && (if high_exclusive { v < high } else { v <= high })
        });
        if !ok {
            errors.push(
                cx.err(
                    codes::INVALID_DAYLIGHT_VALUE,
                    format!("daylight.{name} is {value}; it must be {prose}"),
                    &format!("/daylight/{name}"),
                )
                .field(name),
            );
        }
    }

    if let Some(color) = object.get("moon_color") {
        check_daylight_color(
            cx,
            color,
            "moon_color",
            "/daylight/moon_color",
            true,
            errors,
        );
    }

    if let Some(palette) = object.get("palette") {
        check_daylight_palette(cx, palette, errors);
    }

    // A scene that authored sky bands and left `drives_sky` on has written
    // values nothing will ever read — the `unused_material` precedent, and the
    // fix (`drives_sky: false`) goes in the message.
    let drives_sky = object
        .get("drives_sky")
        .is_none_or(|v| v.as_bool().unwrap_or(true));
    if drives_sky {
        let authored: Vec<&str> = environment
            .and_then(Value::as_object)
            .map(|env| {
                ["sky_zenith", "sky_horizon", "sky_ground"]
                    .into_iter()
                    .filter(|band| env.contains_key(*band))
                    .collect()
            })
            .unwrap_or_default();

        if !authored.is_empty() {
            errors.push(
                cx.err(
                    codes::DAYLIGHT_OVERRIDES_SKY,
                    format!(
                        "daylight computes the sky, so environment.{} {} never read; \
                         set daylight.drives_sky to false to keep the authored colors",
                        authored.join(", environment."),
                        if authored.len() == 1 { "is" } else { "are" },
                    ),
                    "/daylight/drives_sky",
                )
                .field("drives_sky")
                .candidates(authored)
                .warning(),
            );
        }
    }
}

/// A linear-RGB triple in a hand-validated block. `clamped` distinguishes a
/// chromaticity (`[0, 1]`) from a sky band, which is a light source and is
/// deliberately unbounded above.
fn check_daylight_color(
    cx: &Cx<'_>,
    value: &Value,
    field: &str,
    path: &str,
    clamped: bool,
    errors: &mut Vec<EngineError>,
) {
    let Some(items) = value.as_array().filter(|items| items.len() == 3) else {
        errors.push(cx.wrong_type(field, "array", value, path).field(field));
        return;
    };

    for (channel, item) in items.iter().enumerate() {
        let ok = item
            .as_f64()
            .is_some_and(|v| v.is_finite() && v >= 0.0 && (!clamped || v <= 1.0));
        if !ok {
            errors.push(
                cx.err(
                    codes::INVALID_DAYLIGHT_VALUE,
                    format!(
                        "{field}[{channel}] is {item}; it must be a number in {}",
                        if clamped { "[0, 1]" } else { "[0, ∞)" }
                    ),
                    &format!("{path}/{channel}"),
                )
                .field(field),
            );
        }
    }
}

/// The palette table: at least two keyframes, strictly increasing hours, and
/// every field of every keyframe present.
///
/// Requiring all nine fields is deliberate — a half-specified keyframe
/// silently interpolating toward black is a worse failure than being told to
/// finish it.
fn check_daylight_palette(cx: &Cx<'_>, palette: &Value, errors: &mut Vec<EngineError>) {
    const COLORS: [&str; 4] = ["sun_color", "ambient_color", "sky_zenith", "sky_ground"];
    const REQUIRED: [&str; 9] = [
        "hour",
        "sun_color",
        "sun_intensity",
        "ambient_color",
        "ambient_intensity",
        "sky_zenith",
        "sky_horizon",
        "sky_ground",
        "fog_scale",
    ];

    let Some(keys) = palette.as_array() else {
        errors.push(cx.wrong_type("palette", "array", palette, "/daylight/palette"));
        return;
    };

    if keys.len() < 2 {
        errors.push(
            cx.err(
                codes::DAYLIGHT_PALETTE_INVALID,
                format!(
                    "daylight.palette holds {} keyframe(s); it needs at least 2, \
                     because a day is interpolated between them",
                    keys.len()
                ),
                "/daylight/palette",
            )
            .field("palette"),
        );
        return;
    }

    let mut previous_hour: Option<f64> = None;

    for (index, key) in keys.iter().enumerate() {
        let key_path = format!("/daylight/palette/{index}");

        let Some(object) = key.as_object() else {
            errors.push(cx.wrong_type("palette", "object", key, &key_path));
            continue;
        };

        for name in REQUIRED {
            if !object.contains_key(name) {
                errors.push(
                    cx.err(
                        codes::MISSING_FIELD,
                        format!("daylight palette keyframe {index} has no {name:?}"),
                        &key_path,
                    )
                    .field(name),
                );
            }
        }

        for key_name in object.keys() {
            if !REQUIRED.contains(&key_name.as_str()) {
                errors.push(
                    cx.err(
                        codes::UNKNOWN_FIELD,
                        format!("a daylight palette keyframe has no field {key_name:?}"),
                        &format!("{key_path}/{key_name}"),
                    )
                    .field(key_name)
                    .suggest_from(key_name, REQUIRED),
                );
            }
        }

        for name in COLORS.into_iter().chain(["sky_horizon"]) {
            if let Some(color) = object.get(name) {
                // Sky bands are light sources and are unbounded above; the
                // sun and ambient carry their magnitude in an intensity, so
                // their colors are chromaticities in [0, 1].
                let clamped = name == "sun_color" || name == "ambient_color";
                check_daylight_color(
                    cx,
                    color,
                    name,
                    &format!("{key_path}/{name}"),
                    clamped,
                    errors,
                );
            }
        }

        for name in ["sun_intensity", "ambient_intensity", "fog_scale"] {
            if let Some(value) = object.get(name) {
                let ok = value.as_f64().is_some_and(|v| v.is_finite() && v >= 0.0);
                if !ok {
                    errors.push(
                        cx.err(
                            codes::INVALID_DAYLIGHT_VALUE,
                            format!("palette keyframe {index}: {name} is {value}; it must be >= 0"),
                            &format!("{key_path}/{name}"),
                        )
                        .field(name),
                    );
                }
            }
        }

        let Some(hour) = object.get("hour") else {
            continue;
        };
        let Some(hour) = hour
            .as_f64()
            .filter(|v| v.is_finite() && (0.0..24.0).contains(v))
        else {
            errors.push(
                cx.err(
                    codes::INVALID_DAYLIGHT_VALUE,
                    format!("palette keyframe {index}: hour is {hour}; it must be in [0, 24)"),
                    &format!("{key_path}/hour"),
                )
                .field("hour"),
            );
            continue;
        };

        // Sorted, because the table wraps: an unsorted palette has no
        // well-defined "next keyframe" and would interpolate backwards
        // through the day rather than failing.
        if let Some(previous) = previous_hour {
            if hour <= previous {
                errors.push(
                    cx.err(
                        codes::DAYLIGHT_PALETTE_INVALID,
                        format!(
                            "palette keyframe {index} is at hour {hour}, not after the \
                             previous keyframe's {previous}; hours must strictly increase"
                        ),
                        &format!("{key_path}/hour"),
                    )
                    .field("hour"),
                );
            }
        }
        previous_hour = Some(hour);
    }
}
