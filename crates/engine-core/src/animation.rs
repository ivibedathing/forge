//! Property-clip animation (M9): the time axis as text.
//!
//! A clip is a JSON file animating schema'd component fields on entities
//! addressed by name. Pose is a pure function of (files, time) — sampling
//! never reads a clock or accumulates state, so `screenshot --time 1.5` is
//! reproducible and diff-render works on animated scenes. Sampled state is
//! written into the ECS world only, never back to disk: the scene file holds
//! the *rest* values.
//!
//! Rotation interpolates **component-wise on Euler degrees**, matching the
//! `Transform.rotation` file format. Deliberate and load-bearing: a
//! 0°→360° key pair actually spins a full turn, where quaternion slerp
//! would treat it as the identity and silently do nothing — the classic
//! failure this engine exists to avoid.

use std::path::Path;

use glam::Vec3;
use hecs::World;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::codes;
use crate::components::AnimationPlayer;
use crate::error::{EngineError, Result};
use crate::lineindex::LineIndex;

/// A property clip file, exactly as it appears on disk.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClipFile {
    pub name: String,
    pub tracks: Vec<Track>,
}

/// One animated property of one entity.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Track {
    /// The target entity's stable name (invariant 4).
    pub entity: String,

    /// `Component.field`, resolved against the same schema
    /// `engine list-components` publishes — a new component's fields are
    /// animatable the day the component exists.
    pub property: String,

    #[serde(default)]
    pub interpolation: Interpolation,

    /// Key times must be strictly increasing. Clip duration is the last key
    /// time — there is no separate duration field to drift out of sync.
    pub keys: Vec<Key>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Interpolation {
    /// Hold each key's value until the next key.
    Step,
    #[default]
    Linear,
    /// Catmull-Rom through the key values; no hand-authored tangents —
    /// tangent arrays are hostile to text editing.
    Cubic,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Key {
    pub time: f32,
    pub value: KeyValue,
}

/// A key's value: a scalar for scalar fields, three components for vector
/// fields. The shape must match the animated field (`type_mismatch`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum KeyValue {
    Scalar(f32),
    Vec3([f32; 3]),
}

impl KeyValue {
    fn as_vec3(self) -> Vec3 {
        match self {
            KeyValue::Scalar(v) => Vec3::splat(v),
            KeyValue::Vec3(v) => Vec3::from_array(v),
        }
    }

    fn is_vec3(self) -> bool {
        matches!(self, KeyValue::Vec3(_))
    }
}

/// The clip duration: the largest last-key time across tracks.
pub fn duration(clip: &ClipFile) -> f32 {
    clip.tracks
        .iter()
        .filter_map(|t| t.keys.last())
        .map(|k| k.time)
        .fold(0.0, f32::max)
}

/// A player's local clip time for scene time `t`: scaled, offset, wrapped
/// when looping, clamped to the final pose when not.
///
/// A **stride-driven** player (M32) substitutes its own accumulated `phase`
/// for scene time and is otherwise identical — one expression, so `speed`,
/// `start_offset` and `looping` mean exactly what they always meant on both
/// clocks. `phase` counts cycles, so it becomes seconds here, where the
/// duration is already in hand. `t` is then unused, which is why a scene
/// rendered with `--time` and no steps shows a stride-driven player wherever
/// its file says it is.
pub fn local_time(player: &AnimationPlayer, clip_duration: f32, t: f32) -> f32 {
    let clock = if player.stride > 0.0 {
        player.phase * clip_duration
    } else {
        t
    };
    let local = clock * player.speed + player.start_offset;
    if clip_duration <= 0.0 {
        return 0.0;
    }
    if player.looping {
        local.rem_euclid(clip_duration)
    } else {
        local.clamp(0.0, clip_duration)
    }
}

/// Sample one track at (already localized) time `t`. Pure; component-wise
/// on the raw numbers.
pub fn sample_track(track: &Track, t: f32) -> Option<KeyValue> {
    let keys = &track.keys;
    let first = keys.first()?;
    if keys.len() == 1 || t <= first.time {
        return Some(first.value);
    }
    let last = keys.last()?;
    if t >= last.time {
        return Some(last.value);
    }

    // The segment [i, i+1] containing t.
    let i = keys.iter().rposition(|k| k.time <= t)?;
    let (a, b) = (&keys[i], &keys[i + 1]);
    let u = (t - a.time) / (b.time - a.time);

    let value = match track.interpolation {
        Interpolation::Step => a.value,
        Interpolation::Linear => {
            let v = a.value.as_vec3().lerp(b.value.as_vec3(), u);
            pack(v, a.value)
        }
        Interpolation::Cubic => {
            // Catmull-Rom with clamped end tangents (endpoints doubled).
            let p0 = keys[i.saturating_sub(1)].value.as_vec3();
            let p1 = a.value.as_vec3();
            let p2 = b.value.as_vec3();
            let p3 = keys[(i + 2).min(keys.len() - 1)].value.as_vec3();
            let u2 = u * u;
            let u3 = u2 * u;
            let v = ((p1 * 2.0)
                + (p2 - p0) * u
                + (p0 * 2.0 - p1 * 5.0 + p2 * 4.0 - p3) * u2
                + (p3 - p0 + (p1 - p2) * 3.0) * u3)
                * 0.5;
            pack(v, a.value)
        }
    };
    Some(value)
}

fn pack(v: Vec3, like: KeyValue) -> KeyValue {
    match like {
        KeyValue::Scalar(_) => KeyValue::Scalar(v.x),
        KeyValue::Vec3(_) => KeyValue::Vec3(v.to_array()),
    }
}

/// Apply the pose at scene time `t` for one player+clip pair into the world.
/// Values land on components only (principle 2: derived, ephemeral).
/// Unresolvable targets are skipped — validation already rejected them for
/// scenes that reach this point.
pub fn apply(
    world: &mut World,
    by_name: &dyn Fn(&str) -> Option<hecs::Entity>,
    player: &AnimationPlayer,
    clip: &ClipFile,
    t: f32,
) {
    let local = local_time(player, duration(clip), t);
    for track in &clip.tracks {
        let Some(value) = sample_track(track, local) else {
            continue;
        };
        let Some((component, field)) = track.property.split_once('.') else {
            continue;
        };
        let Some(entity) = by_name(&track.entity) else {
            continue;
        };
        set_field(world, entity, component, field, value);
    }
}

/// Write one sampled value onto a component field. Returns false when no
/// such animatable field exists — the coverage test below walks the schema
/// to prove every numeric field has an arm here, so validation ("that
/// property exists") and application ("we can set it") cannot drift.
pub fn set_field(
    world: &mut World,
    entity: hecs::Entity,
    component: &str,
    field: &str,
    value: KeyValue,
) -> bool {
    use crate::components::*;

    let v3 = value.as_vec3();
    let scalar = v3.x;

    match component {
        "Transform" => {
            let Ok(mut c) = world.get::<&mut Transform>(entity) else {
                return false;
            };
            match field {
                "position" => c.position = v3,
                "rotation" => c.rotation = v3,
                "scale" => c.scale = v3,
                _ => return false,
            }
        }
        "Material" => {
            let Ok(mut c) = world.get::<&mut Material>(entity) else {
                return false;
            };
            match field {
                "albedo" => c.albedo = v3,
                "metallic" => c.metallic = scalar,
                "roughness" => c.roughness = scalar,
                "emissive" => c.emissive = v3,
                "alpha" => c.alpha = scalar,
                "transmission" => c.transmission = scalar,
                // M26. The maps themselves are paths, so they never arrive
                // here; `uv_scale`/`uv_offset` are in `NOT_ANIMATABLE`.
                "alpha_cutoff" => c.alpha_cutoff = scalar,
                "normal_strength" => c.normal_strength = scalar,
                "ior" => c.ior = scalar,
                "thickness" => c.thickness = scalar,
                "attenuation" => c.attenuation = v3,
                _ => return false,
            }
        }
        "Camera" => {
            let Ok(mut c) = world.get::<&mut Camera>(entity) else {
                return false;
            };
            match field {
                "fov" => c.fov = scalar,
                "near" => c.near = scalar,
                "far" => c.far = scalar,
                _ => return false,
            }
        }
        "DirectionalLight" => {
            let Ok(mut c) = world.get::<&mut DirectionalLight>(entity) else {
                return false;
            };
            match field {
                "color" => c.color = v3,
                "intensity" => c.intensity = scalar,
                _ => return false,
            }
        }
        "AmbientLight" => {
            let Ok(mut c) = world.get::<&mut AmbientLight>(entity) else {
                return false;
            };
            match field {
                "color" => c.color = v3,
                "intensity" => c.intensity = scalar,
                _ => return false,
            }
        }
        "PointLight" => {
            let Ok(mut c) = world.get::<&mut PointLight>(entity) else {
                return false;
            };
            match field {
                "color" => c.color = v3,
                "intensity" => c.intensity = scalar,
                "range" => c.range = scalar,
                _ => return false,
            }
        }
        "RigidBody" => {
            let Ok(mut c) = world.get::<&mut RigidBody>(entity) else {
                return false;
            };
            match field {
                "linear_velocity" => c.linear_velocity = v3,
                "angular_velocity" => c.angular_velocity = v3,
                "gravity_scale" => c.gravity_scale = scalar,
                "linear_damping" => c.linear_damping = scalar,
                "angular_damping" => c.angular_damping = scalar,
                _ => return false,
            }
        }
        "Collider" => {
            let Ok(mut c) = world.get::<&mut Collider>(entity) else {
                return false;
            };
            match field {
                "friction" => c.friction = scalar,
                "restitution" => c.restitution = scalar,
                "density" => c.density = scalar,
                "offset" => c.offset = v3,
                "radius" => c.radius = Some(scalar),
                "half_height" => c.half_height = Some(scalar),
                "half_extents" => c.half_extents = Some(v3),
                _ => return false,
            }
        }
        "HudText" => {
            let Ok(mut c) = world.get::<&mut HudText>(entity) else {
                return false;
            };
            match field {
                "offset" => c.offset = v3.truncate(),
                "size" => c.size = scalar,
                "color" => c.color = v3,
                // M31's layout fields animate like any other number: layout is
                // a pure function recomputed per frame, so a clip driving
                // `wrap` costs a re-measure and nothing else. This is the
                // opposite of `Terrain`'s shape fields, which are in
                // `NOT_ANIMATABLE` because they would regenerate a mesh.
                "wrap" => c.wrap = scalar,
                "line_gap" => c.line_gap = scalar,
                _ => return false,
            }
        }
        "HudRect" => {
            let Ok(mut c) = world.get::<&mut HudRect>(entity) else {
                return false;
            };
            match field {
                "offset" => c.offset = v3.truncate(),
                "size" => c.size = v3.truncate(),
                "color" => c.color = v3,
                "opacity" => c.opacity = scalar,
                _ => return false,
            }
        }
        "HudPanel" => {
            let Ok(mut c) = world.get::<&mut HudPanel>(entity) else {
                return false;
            };
            match field {
                "offset" => c.offset = v3.truncate(),
                "padding" => c.padding = scalar,
                "gap" => c.gap = scalar,
                // `width`/`height` are `Option<f32>`: a clip sets a size,
                // which is exactly "stop hugging and be this big".
                "width" => c.width = Some(scalar),
                "height" => c.height = Some(scalar),
                "color" => c.color = v3,
                "opacity" => c.opacity = scalar,
                _ => return false,
            }
        }
        "HudImage" => {
            let Ok(mut c) = world.get::<&mut HudImage>(entity) else {
                return false;
            };
            match field {
                "offset" => c.offset = v3.truncate(),
                "size" => c.size = v3.truncate(),
                "tint" => c.tint = v3,
                "opacity" => c.opacity = scalar,
                // `slice` is four numbers, not three: a clip that drove it
                // through the vec3 path would silently drop the fourth, so it
                // is in `NOT_ANIMATABLE` instead.
                _ => return false,
            }
        }
        "HudInteract" => {
            let Ok(mut c) = world.get::<&mut HudInteract>(entity) else {
                return false;
            };
            match field {
                "hover_tint" => c.hover_tint = v3,
                "press_tint" => c.press_tint = v3,
                _ => return false,
            }
        }
        "Breakable" => {
            let Ok(mut c) = world.get::<&mut Breakable>(entity) else {
                return false;
            };
            match field {
                "impulse_threshold" => c.impulse_threshold = Some(scalar),
                _ => return false,
            }
        }
        // M32's planting limits animate freely, and that is the intended way
        // to *stop* planting: a character that jumps drives `max_drop` and
        // `max_lift` to zero for the airborne frames, and its feet keep
        // whatever the clip gives them. The foot list itself is structure, not
        // a number, so it never arrives here.
        "FootPlant" => {
            let Ok(mut c) = world.get::<&mut FootPlant>(entity) else {
                return false;
            };
            match field {
                "max_drop" => c.max_drop = scalar,
                "max_lift" => c.max_lift = scalar,
                "align" => c.align = scalar,
                _ => return false,
            }
        }
        "AnimationPlayer" => {
            let Ok(mut c) = world.get::<&mut AnimationPlayer>(entity) else {
                return false;
            };
            match field {
                "speed" => c.speed = scalar,
                "start_offset" => c.start_offset = scalar,
                _ => return false,
            }
        }
        "Wheel" => {
            let Ok(mut c) = world.get::<&mut Wheel>(entity) else {
                return false;
            };
            match field {
                "offset" => c.offset = v3,
                "radius" => c.radius = scalar,
                "suspension_rest_length" => c.suspension_rest_length = scalar,
                "suspension_stiffness" => c.suspension_stiffness = scalar,
                "suspension_compression" => c.suspension_compression = scalar,
                "suspension_damping" => c.suspension_damping = scalar,
                "suspension_travel" => c.suspension_travel = scalar,
                "max_suspension_force" => c.max_suspension_force = scalar,
                "friction_slip" => c.friction_slip = scalar,
                "side_friction_stiffness" => c.side_friction_stiffness = scalar,
                "engine_force" => c.engine_force = scalar,
                "brake" => c.brake = scalar,
                "steering" => c.steering = scalar,
                _ => return false,
            }
        }
        "ParticleEmitter" => {
            let Ok(mut c) = world.get::<&mut ParticleEmitter>(entity) else {
                return false;
            };
            match field {
                "rate" => c.rate = scalar,
                "lifetime" => c.lifetime = scalar,
                "speed" => c.speed = scalar,
                "spread" => c.spread = scalar,
                "acceleration" => c.acceleration = v3,
                "drag" => c.drag = scalar,
                "start_size" => c.start_size = scalar,
                "end_size" => c.end_size = scalar,
                "start_color" => c.start_color = v3,
                "end_color" => c.end_color = v3,
                "start_alpha" => c.start_alpha = scalar,
                "end_alpha" => c.end_alpha = scalar,
                // M17. `blend` is absent on purpose: it is a string enum, and
                // this function animates numbers.
                "radius" => c.radius = scalar,
                "speed_jitter" => c.speed_jitter = scalar,
                "size_jitter" => c.size_jitter = scalar,
                "lifetime_jitter" => c.lifetime_jitter = scalar,
                "turbulence" => c.turbulence = scalar,
                "turbulence_scale" => c.turbulence_scale = scalar,
                "stretch" => c.stretch = scalar,
                _ => return false,
            }
        }
        // A clip on a tree parameter regrows the tree's mesh every step it
        // changes — legal, deterministic, and not free. `seed`, `levels`,
        // `branches`, `whorl`, `sides`, `segments`, `leaves_per_branch`, and
        // `leaf` are absent for the usual reason: integers and string enums
        // are not numbers this function can interpolate into.
        "Tree" => {
            let Ok(mut c) = world.get::<&mut Tree>(entity) else {
                return false;
            };
            match field {
                "height" => c.height = scalar,
                "trunk_radius" => c.trunk_radius = scalar,
                "branch_angle" => c.branch_angle = scalar,
                "branch_twist" => c.branch_twist = scalar,
                "branch_start" => c.branch_start = scalar,
                "length_ratio" => c.length_ratio = scalar,
                "length_falloff" => c.length_falloff = scalar,
                "radius_ratio" => c.radius_ratio = scalar,
                "taper" => c.taper = scalar,
                "flare" => c.flare = scalar,
                "crook" => c.crook = scalar,
                "tropism" => c.tropism = scalar,
                "jitter" => c.jitter = scalar,
                "leaf_size" => c.leaf_size = scalar,
                "leaf_color" => c.leaf_color = v3,
                "leaf_roughness" => c.leaf_roughness = scalar,
                _ => return false,
            }
        }
        "Water" => {
            let Ok(mut c) = world.get::<&mut Water>(entity) else {
                return false;
            };
            match field {
                // `segments` and `waves` are absent on purpose. The first is an
                // integer (this function animates numbers, and retessellating
                // per frame would defeat the cached grid); the second is an
                // array of objects, which is a shape clips cannot express — a
                // rising sea is a clip on `detail` and `crest_foam`, or a
                // script writing the surface's fields.
                "detail" => c.detail = scalar,
                "detail_scale" => c.detail_scale = scalar,
                "roughness" => c.roughness = scalar,
                "shallow_color" => c.shallow_color = v3,
                "deep_color" => c.deep_color = v3,
                "depth_fade" => c.depth_fade = scalar,
                "opacity" => c.opacity = scalar,
                "crest_foam" => c.crest_foam = scalar,
                "shore_foam" => c.shore_foam = scalar,
                "foam_color" => c.foam_color = v3,
                // Animatable, unlike `Terrain`'s shape fields: the IOR is a
                // uniform the shader reads, so a clip on it regenerates
                // nothing. It does switch pipelines the step it leaves 1.0,
                // which is a pipeline lookup and not a rebuild.
                "ior" => c.ior = scalar,
                _ => return false,
            }
        }
        // A clip on a cloud's *shape* regrows its lobes every step it changes,
        // which is legal, deterministic and not free — the shading fields below
        // are uniforms and cost nothing. `seed`, `lobes`, `levels`, `children`
        // and `detail` are absent for the usual reason: integers are not
        // numbers this function can interpolate into.
        "Cloud" => {
            let Ok(mut c) = world.get::<&mut Cloud>(entity) else {
                return false;
            };
            match field {
                "lobe_size" => c.lobe_size = scalar,
                "lobe_ratio" => c.lobe_ratio = scalar,
                "flatten" => c.flatten = scalar,
                "rise" => c.rise = scalar,
                "wobble" => c.wobble = scalar,
                "jitter" => c.jitter = scalar,
                "density" => c.density = scalar,
                "feather" => c.feather = scalar,
                "color" => c.color = v3,
                "shade_color" => c.shade_color = v3,
                "drift" => c.drift = v3,
                "drift_wrap" => c.drift_wrap = scalar,
                _ => return false,
            }
        }
        "Terrain" => {
            let Ok(mut c) = world.get::<&mut Terrain>(entity) else {
                return false;
            };
            match field {
                // Appearance only. The shape fields are listed in
                // `NOT_ANIMATABLE` and never reach here — regenerating the
                // surface every frame is not something a clip should be able to
                // ask for by accident.
                "texture_scale" => c.texture_scale = scalar,
                "color_variation" => c.color_variation = scalar,
                "bump" => c.bump = scalar,
                _ => return false,
            }
        }
        "Meadow" => {
            let Ok(mut c) = world.get::<&mut Meadow>(entity) else {
                return false;
            };
            match field {
                // Uniforms only — every one of these reaches the shader without
                // moving a vertex or replacing an instance. The placement and
                // template fields are in `NOT_ANIMATABLE` and never reach here.
                "cycle_length" => c.cycle_length = scalar,
                "phase" => c.phase = scalar,
                "wind" => c.wind = scalar,
                "wind_speed" => c.wind_speed = scalar,
                "wind_direction" => c.wind_direction = scalar,
                "flower_color" => c.flower_color = v3,
                _ => return false,
            }
        }
        "Road" => {
            let Ok(mut c) = world.get::<&mut Road>(entity) else {
                return false;
            };
            match field {
                // Appearance only, for terrain's reason: a road's *shape*
                // fields are in `NOT_ANIMATABLE`. `points`, `closed` and
                // `markings` never arrive here at all — an array of objects, a
                // bool and a nested object are shapes a numeric clip cannot
                // express, the same as `Water.waves`.
                //
                // Repainting a road *is* free: these four are read per pixel.
                "roughness" => c.roughness = scalar,
                "color" => c.color = v3,
                "shoulder_color" => c.shoulder_color = v3,
                "bank_color" => c.bank_color = v3,
                // Grain is read per pixel too (M40), so it repaints for free —
                // a road that dries out over a scene is a clip on `grain`.
                "grain" => c.grain = scalar,
                _ => return false,
            }
        }
        _ => return false,
    }
    true
}

/// Numeric fields that are nonetheless not animatable, and why.
///
/// A terrain's *shape* is generated on the CPU and cached as one `Arc<MeshData>`
/// per distinct patch, which the renderer in turn keys its uploaded vertex
/// buffers on (M15). A clip driving one of these would mint a new surface every
/// frame: a full regeneration (a 256² patch is 330 000 noise evaluations), a new
/// GPU upload, and — because the renderer holds an idle entry for 240 frames —
/// hundreds of megabytes of vertex buffers for a scene that looks like it is
/// merely undulating.
///
/// Refusing is better than paying that quietly. Because `field_shape` is the one
/// gate, a clip aimed here fails validation with `unknown_property` and a
/// `did_you_mean` naming the fields that *do* animate, rather than silently
/// doing nothing — which is exactly what the drift test in this module exists to
/// prevent. Terrain's appearance (`texture_scale`, `color_variation`, `bump`)
/// animates freely; it costs nothing but a uniform.
const NOT_ANIMATABLE: &[(&str, &str)] = &[
    ("Terrain", "height"),
    ("Terrain", "feature_scale"),
    ("Terrain", "persistence"),
    ("Terrain", "warp"),
    // A road's shape is generated and `Arc`-cached exactly like a terrain
    // patch's, so animating one of these would mint a new ribbon — and a new
    // GPU upload, and a new trimesh collider — every frame it changed.
    ("Road", "width"),
    ("Road", "shoulder"),
    ("Road", "skirt"),
    ("Road", "segment_length"),
    ("Road", "segment_angle"),
    // M40's shape fields, in here for the same reason: all four are in the
    // road's cache key, so a clip on one regenerates and re-uploads the whole
    // ribbon every frame it changes. `grain_scale` joins them not because it
    // moves a vertex — it does not — but because a road's grain cell size
    // changing per frame is a shimmer, not an animation.
    ("Road", "auto_bank"),
    ("Road", "auto_bank_radius"),
    ("Road", "follow_smoothing"),
    ("Road", "follow_blend"),
    ("Road", "grain_scale"),
    // A junction's patch is generated from the roads that reach it and cached
    // the same way, and its colours are the only fields a clip could touch
    // without rebuilding it — but a `Junction` is not reachable by name from a
    // clip's `component` field yet, so the whole component is listed rather
    // than half of it.
    ("Junction", "flare"),
    ("Junction", "corner_segments"),
    ("Junction", "shoulder"),
    ("Junction", "skirt"),
    ("Junction", "roughness"),
    ("Junction", "grain"),
    ("Junction", "grain_scale"),
    ("Junction", "color"),
    ("Junction", "shoulder_color"),
    ("Junction", "bank_color"),
    // A meadow's placement and its plant template are generated and
    // `Arc`-cached the same way, and these are exactly the fields its cache key
    // covers — animating one would rebuild the template and re-scatter every
    // plant in the field every frame it changed. `stagger` is in here for the
    // same reason and it is the one that looks animatable: a plant's phase
    // offset is drawn once, at placement, and baked into the instance buffer.
    ("Meadow", "density"),
    ("Meadow", "height"),
    ("Meadow", "blade_width"),
    ("Meadow", "splay"),
    ("Meadow", "head_size"),
    ("Meadow", "size_jitter"),
    ("Meadow", "stagger"),
    ("Meadow", "max_slope"),
    // A different reason, and the only one of its kind: a clip's values are
    // scalars and 3-vectors, and these are 2-vectors, which the format cannot
    // spell. Scrolling UVs is the feature that would want them and it is
    // deferred with the reproducible clock it needs — see the material design
    // doc's §12.
    ("Material", "uv_scale"),
    ("Material", "uv_offset"),
    // A nine-slice inset is *four* numbers, one per edge, and a clip's values
    // are scalars and 3-vectors. Driving it through the vector path would
    // silently drop the bottom inset, which renders as a frame that has lost
    // one edge — so it is refused rather than three-quarters supported.
    ("HudImage", "slice"),
    // A third reason, and the only fields whose animation would be circular:
    // these two *are* a clip's clock (M32). `phase` is written by the
    // locomotion system every fixed step, so a clip driving it would fight
    // that system for the same number every frame, and a clip driving its own
    // player's phase would sample itself. `stride` selects which clock runs at
    // all; flipping it mid-clip teleports the pose, which is the discontinuity
    // the stored phase exists to remove. Scripts set both, where the write is
    // explicit and shows up in the trace.
    ("AnimationPlayer", "stride"),
    ("AnimationPlayer", "phase"),
    // A fourth reason (M33): rapier reads a collider's material once, when the
    // physics world is built, so a clip driving these would animate a number
    // nothing reads again — a silent no-op, which is the failure this table
    // exists to turn into an error. The same is true of every dimension inside
    // `parts`, which the format cannot address at all.
    ("SkinnedCollider", "friction"),
    ("SkinnedCollider", "restitution"),
    // M33's reason again, one milestone on (M39): every one of these is read
    // exactly once, at the handoff, and written into rapier bodies and joints
    // that never consult the component again. A clip driving `limit` would
    // animate a number nothing reads — the silent no-op this table exists to
    // turn into an error. `density` is the sharpest case: it sets each part's
    // mass when the ragdoll fires, and a corpse whose mass changed every frame
    // would be a solver bug rather than an effect.
    ("Ragdoll", "density"),
    ("Ragdoll", "limit"),
    ("Ragdoll", "linear_damping"),
    ("Ragdoll", "angular_damping"),
];

/// Whether a field is vector-shaped in the published schema (3-element
/// array). Used by clip validation for `type_mismatch`.
fn field_shape(schema: &serde_json::Value, component: &str, field: &str) -> Option<bool> {
    if NOT_ANIMATABLE.contains(&(component, field)) {
        return None;
    }

    let variant = schema["oneOf"]
        .as_array()?
        .iter()
        .find(|v| v["properties"]["type"]["const"] == component)?;
    let property = &variant["properties"][field];
    let property = match property["$ref"]
        .as_str()
        .and_then(|r| r.strip_prefix("#/$defs/"))
    {
        Some(name) => &schema["$defs"][name],
        None => property,
    };
    let is_array = property["type"] == "array"
        || property["type"]
            .as_array()
            .is_some_and(|t| t.iter().any(|x| x == "array"));
    // Only arrays *of numbers* animate: a `[bool; 3]` like
    // `RigidBody.locked_rotations` is configuration, not a pose.
    let items_numeric = property["items"]["type"] == "number"
        || property["items"]["type"]
            .as_array()
            .is_some_and(|t| t.iter().any(|x| x == "number"));
    let is_number = property["type"] == "number"
        || property["type"]
            .as_array()
            .is_some_and(|t| t.iter().any(|x| x == "number"));
    if (is_array && items_numeric) || is_number {
        Some(is_array)
    } else {
        None // Not a numeric/vector field: not animatable.
    }
}

/// Validate a clip file's contents, all errors at once, with file/line.
/// Structural checks only; entity-name resolution needs a scene and lives
/// in scene validation.
pub fn validate_clip_source(source: &str, path: &str) -> Vec<EngineError> {
    let mut errors = Vec::new();

    let root: serde_json::Value = match serde_json::from_str(source) {
        Ok(value) => value,
        Err(e) => {
            return vec![EngineError::new(codes::INVALID_JSON, e.to_string())
                .file(path)
                .line(e.line() as u32)
                .column(e.column() as u32)];
        }
    };
    let index = LineIndex::new(source);
    let schema = crate::schema::component_schema();

    let located = |mut error: EngineError, json_path: &str| -> EngineError {
        error = error.file(path).path(json_path);
        match index.line_of_or_parent(json_path) {
            Some(line) => error.line(line),
            None => error,
        }
    };

    // Structure first, through serde on the typed ClipFile — but collect
    // per-track errors ourselves so one bad track doesn't hide the rest.
    let Some(tracks) = root["tracks"].as_array() else {
        errors.push(located(
            EngineError::new(codes::MISSING_FIELD, "a clip requires a \"tracks\" array")
                .field("tracks"),
            "",
        ));
        return errors;
    };
    if root["name"].as_str().is_none() {
        errors.push(located(
            EngineError::new(codes::MISSING_FIELD, "a clip requires a \"name\" field")
                .field("name"),
            "",
        ));
    }

    for (track_index, track_value) in tracks.iter().enumerate() {
        let track_path = format!("/tracks/{track_index}");
        let track: Track = match serde_json::from_value(track_value.clone()) {
            Ok(track) => track,
            Err(e) => {
                errors.push(located(
                    EngineError::new(
                        codes::INVALID_FIELD_TYPE,
                        format!("track {track_index} is malformed: {e}"),
                    ),
                    &track_path,
                ));
                continue;
            }
        };

        // Property path against the schema.
        let shape = track
            .property
            .split_once('.')
            .and_then(|(component, field)| field_shape(&schema, component, field));
        match (track.property.split_once('.'), shape) {
            (None, _) => {
                errors.push(located(
                    EngineError::new(
                        codes::UNKNOWN_PROPERTY,
                        format!("property {:?} is not \"Component.field\"", track.property),
                    )
                    .field("property"),
                    &format!("{track_path}/property"),
                ));
            }
            (Some((component, field)), None) => {
                let suggestions = animatable_properties(&schema);
                errors.push(located(
                    EngineError::new(
                        codes::UNKNOWN_PROPERTY,
                        format!("{component:?} has no animatable field {field:?}"),
                    )
                    .field("property")
                    .suggest_from(&track.property, suggestions.iter().map(String::as_str)),
                    &format!("{track_path}/property"),
                ));
            }
            (Some(_), Some(wants_vec3)) => {
                for (key_index, key) in track.keys.iter().enumerate() {
                    if key.value.is_vec3() != wants_vec3 {
                        let (want, got) = if wants_vec3 {
                            ("a [x, y, z] array", "a scalar")
                        } else {
                            ("a scalar", "a [x, y, z] array")
                        };
                        errors.push(located(
                            EngineError::new(
                                codes::TYPE_MISMATCH,
                                format!(
                                    "key {key_index} of {:?} needs {want}, found {got}",
                                    track.property
                                ),
                            )
                            .field("value"),
                            &format!("{track_path}/keys/{key_index}/value"),
                        ));
                    }
                }
            }
        }

        // Strictly increasing key times, naming the offending index.
        for (key_index, pair) in track.keys.windows(2).enumerate() {
            if pair[1].time <= pair[0].time {
                errors.push(located(
                    EngineError::new(
                        codes::UNSORTED_KEYS,
                        format!(
                            "key {} (time {}) does not come after key {} (time {}); \
                             key times must be strictly increasing",
                            key_index + 1,
                            pair[1].time,
                            key_index,
                            pair[0].time
                        ),
                    )
                    .field("time"),
                    &format!("{track_path}/keys/{}/time", key_index + 1),
                ));
            }
        }

        if track.keys.is_empty() {
            errors.push(located(
                EngineError::new(
                    codes::MISSING_FIELD,
                    format!("track {track_index} has no keys"),
                )
                .field("keys"),
                &track_path,
            ));
        }
    }

    errors
}

/// Every `Component.field` the schema exposes as numeric — the suggestion
/// pool for `unknown_property`.
pub fn animatable_properties(schema: &serde_json::Value) -> Vec<String> {
    let mut properties = Vec::new();
    let Some(variants) = schema["oneOf"].as_array() else {
        return properties;
    };
    for variant in variants {
        let Some(component) = variant["properties"]["type"]["const"].as_str() else {
            continue;
        };
        let Some(fields) = variant["properties"].as_object() else {
            continue;
        };
        for field in fields.keys() {
            if field == "type" {
                continue;
            }
            if field_shape(schema, component, field).is_some() {
                properties.push(format!("{component}.{field}"));
            }
        }
    }
    properties
}

/// A player paired with its loaded clip, ready to sample.
pub struct LoadedPlayer {
    pub player: AnimationPlayer,
    pub clip: ClipFile,
}

/// Load every `AnimationPlayer`'s clip, resolved relative to the scene file.
pub fn load_players(scene: &crate::Scene, scene_path: &Path) -> Result<Vec<LoadedPlayer>> {
    let base_dir = scene_path.parent().unwrap_or(Path::new(""));
    let mut players = Vec::new();
    for player in scene.world.query::<&AnimationPlayer>().iter() {
        // Skeletal references (`path#Clip`) name a rig inside a glTF, not a
        // property clip: they are sampled by `engine_core::skeleton` against
        // the asset, and there is nothing here to write into a component.
        if matches!(
            crate::skeleton::ClipRef::parse(&player.clip),
            crate::skeleton::ClipRef::Skeletal { .. }
        ) {
            continue;
        }
        let clip = load_clip(&base_dir.join(&player.clip))?;
        players.push(LoadedPlayer {
            player: player.clone(),
            clip,
        });
    }
    Ok(players)
}

/// The longest clip duration among loaded players — `filmstrip`'s default
/// time range.
pub fn longest_duration(players: &[LoadedPlayer]) -> f32 {
    players
        .iter()
        .map(|p| duration(&p.clip))
        .fold(0.0, f32::max)
}

/// Apply every player's pose at scene time `t`. The system-ordering slot is
/// "sample animations" — callers run physics after, render last.
pub fn apply_all(scene: &mut crate::Scene, players: &[LoadedPlayer], t: f32) {
    let lookup: std::collections::HashMap<String, hecs::Entity> = scene
        .names()
        .map(str::to_string)
        .filter_map(|n| scene.entity(&n).map(|e| (n, e)))
        .collect();
    let by_name = move |name: &str| lookup.get(name).copied();
    for loaded in players {
        apply(&mut scene.world, &by_name, &loaded.player, &loaded.clip, t);
    }
}

/// Read and structurally validate a clip file from disk.
pub fn load_clip(path: &Path) -> Result<ClipFile> {
    let display = path.display().to_string();
    let source = std::fs::read_to_string(path).map_err(|e| {
        EngineError::new(
            codes::ASSET_NOT_FOUND,
            format!("could not read clip {display}: {e}"),
        )
        .file(&display)
    })?;
    serde_json::from_str(&source).map_err(|e| {
        EngineError::new(
            codes::ASSET_LOAD_FAILED,
            format!("clip {display} does not parse: {e}"),
        )
        .file(&display)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Scene;

    fn spin_track(interpolation: Interpolation) -> Track {
        Track {
            entity: "Cube".into(),
            property: "Transform.rotation".into(),
            interpolation,
            keys: vec![
                Key {
                    time: 0.0,
                    value: KeyValue::Vec3([0.0, 0.0, 0.0]),
                },
                Key {
                    time: 2.0,
                    value: KeyValue::Vec3([0.0, 360.0, 0.0]),
                },
            ],
        }
    }

    #[test]
    fn linear_interpolation_hits_the_midpoint() {
        let track = spin_track(Interpolation::Linear);
        assert_eq!(
            sample_track(&track, 1.0),
            Some(KeyValue::Vec3([0.0, 180.0, 0.0]))
        );
        assert_eq!(
            sample_track(&track, 0.5),
            Some(KeyValue::Vec3([0.0, 90.0, 0.0]))
        );
    }

    #[test]
    fn the_zero_to_360_spin_actually_spins() {
        // The design's load-bearing case: quaternion slerp would no-op this.
        let track = spin_track(Interpolation::Linear);
        let quarter = sample_track(&track, 0.5).unwrap();
        assert_eq!(quarter, KeyValue::Vec3([0.0, 90.0, 0.0]));
        let full = sample_track(&track, 2.0).unwrap();
        assert_eq!(full, KeyValue::Vec3([0.0, 360.0, 0.0]));
    }

    #[test]
    fn step_holds_until_the_next_key() {
        let track = spin_track(Interpolation::Step);
        assert_eq!(
            sample_track(&track, 1.999),
            Some(KeyValue::Vec3([0.0, 0.0, 0.0]))
        );
        assert_eq!(
            sample_track(&track, 2.0),
            Some(KeyValue::Vec3([0.0, 360.0, 0.0]))
        );
    }

    #[test]
    fn cubic_passes_through_the_keys_and_stays_smooth() {
        let track = Track {
            entity: "X".into(),
            property: "Transform.position".into(),
            interpolation: Interpolation::Cubic,
            keys: vec![
                Key {
                    time: 0.0,
                    value: KeyValue::Vec3([0.0, 0.0, 0.0]),
                },
                Key {
                    time: 1.0,
                    value: KeyValue::Vec3([1.0, 2.0, 0.0]),
                },
                Key {
                    time: 2.0,
                    value: KeyValue::Vec3([2.0, 0.0, 0.0]),
                },
            ],
        };
        // Passes exactly through keys…
        assert_eq!(
            sample_track(&track, 1.0),
            Some(KeyValue::Vec3([1.0, 2.0, 0.0]))
        );
        // …and overshoots nowhere near the ends (clamped tangents).
        let KeyValue::Vec3(v) = sample_track(&track, 0.5).unwrap() else {
            panic!()
        };
        assert!(v[1] > 0.0 && v[1] < 2.0, "{v:?}");
    }

    #[test]
    fn sampling_clamps_outside_the_key_range() {
        let track = spin_track(Interpolation::Linear);
        assert_eq!(
            sample_track(&track, -1.0),
            Some(KeyValue::Vec3([0.0, 0.0, 0.0]))
        );
        assert_eq!(
            sample_track(&track, 99.0),
            Some(KeyValue::Vec3([0.0, 360.0, 0.0]))
        );
    }

    #[test]
    fn looping_wraps_and_non_looping_clamps() {
        let player = AnimationPlayer {
            clip: "x".into(),
            speed: 1.0,
            looping: true,
            start_offset: 0.0,
            stride: 0.0,
            phase: 0.0,
        };
        assert_eq!(local_time(&player, 2.0, 2.0), 0.0, "loop period lands on 0");
        assert_eq!(local_time(&player, 2.0, 2.5), 0.5);

        let once = AnimationPlayer {
            looping: false,
            ..player.clone()
        };
        assert_eq!(
            local_time(&once, 2.0, 5.0),
            2.0,
            "clamped to the final pose"
        );
    }

    #[test]
    fn speed_and_offset_shape_local_time() {
        let player = AnimationPlayer {
            clip: "x".into(),
            speed: 2.0,
            looping: true,
            start_offset: 0.5,
            stride: 0.0,
            phase: 0.0,
        };
        assert_eq!(local_time(&player, 10.0, 1.0), 2.5);
    }

    #[test]
    fn apply_writes_the_pose_into_the_world() {
        let source = r#"{"name":"s","entities":[
            {"name":"Cube","components":[
                {"type":"Transform","position":[0.0,0.5,0.0]},
                {"type":"Mesh","asset":"builtin:cube"}
            ]}
        ]}"#;
        let mut scene = Scene::from_source(source, "t.json").unwrap();
        let player = AnimationPlayer {
            clip: "spin".into(),
            speed: 1.0,
            looping: true,
            start_offset: 0.0,
            stride: 0.0,
            phase: 0.0,
        };
        let clip = ClipFile {
            name: "spin".into(),
            tracks: vec![spin_track(Interpolation::Linear)],
        };

        let entity = scene.entity("Cube").unwrap();
        let lookup = {
            let map: std::collections::HashMap<String, hecs::Entity> =
                [("Cube".to_string(), entity)].into();
            move |name: &str| map.get(name).copied()
        };
        apply(&mut scene.world, &lookup, &player, &clip, 0.5);

        let transform = scene
            .world
            .get::<&crate::components::Transform>(entity)
            .unwrap();
        assert_eq!(transform.rotation, Vec3::new(0.0, 90.0, 0.0));
        assert_eq!(
            transform.position,
            Vec3::new(0.0, 0.5, 0.0),
            "untargeted fields keep their rest values"
        );
    }

    #[test]
    fn every_numeric_schema_field_has_a_setter_arm() {
        // The drift test: validation says "that property exists" from the
        // schema; application must be able to set every one of them, or a
        // validated clip could silently do nothing.
        let source = r#"{"name":"s","entities":[
            {"name":"E","components":[
                {"type":"Transform"},
                {"type":"Mesh","asset":"builtin:cube"},
                {"type":"Material"},
                {"type":"Camera"},
                {"type":"DirectionalLight"},
                {"type":"AmbientLight"},
                {"type":"PointLight"},
                {"type":"RigidBody","body":"kinematic"},
                {"type":"Collider","shape":"sphere","radius":0.5},
                {"type":"Breakable","fragments":[{"mesh":"builtin:cube"}]},
                {"type":"AnimationPlayer","clip":"x"},
                {"type":"Wheel","vehicle":"E"},
                {"type":"HudText","text":"x"},
                {"type":"HudRect","size":[1.0,1.0]},
                {"type":"HudPanel"},
                {"type":"HudImage","texture":"x.png","size":[1.0,1.0]},
                {"type":"HudInteract"},
                {"type":"ParticleEmitter"},
                {"type":"Tree"},
                {"type":"Water"},
                {"type":"Cloud"},
                {"type":"Terrain"},
                {"type":"Road"},
                {"type":"Meadow"},
                {"type":"FootPlant","feet":[{"ankle":"Foot.L"}],"ground":"Ground"}
            ]}
        ]}"#;
        // Not a *valid* scene (missing collider transform rules etc. are
        // irrelevant here); spawn directly through the parsed file.
        let file: crate::SceneFile = serde_json::from_str(source).unwrap();
        let mut scene = Scene::instantiate(file);
        let entity = scene.entity("E").unwrap();

        let schema = crate::schema::component_schema();
        for property in animatable_properties(&schema) {
            let (component, field) = property.split_once('.').unwrap();
            let ok = set_field(
                &mut scene.world,
                entity,
                component,
                field,
                KeyValue::Vec3([0.1, 0.2, 0.3]),
            );
            assert!(ok, "schema exposes {property} but set_field cannot set it");
        }
    }

    #[test]
    fn clip_validation_reports_everything_at_once() {
        let source = r#"{
  "name": "broken",
  "tracks": [
    { "entity": "Cube", "property": "Transform.rotaton",
      "keys": [ { "time": 0.0, "value": [0.0, 0.0, 0.0] } ] },
    { "entity": "Cube", "property": "Transform.rotation",
      "keys": [ { "time": 1.0, "value": [0.0, 0.0, 0.0] },
                { "time": 0.5, "value": 3.0 } ] }
  ]
}"#;
        let errors = validate_clip_source(source, "broken.anim.json");
        let codes: Vec<&str> = errors.iter().map(|e| e.error).collect();
        assert!(codes.contains(&"unknown_property"), "{codes:?}");
        assert!(codes.contains(&"unsorted_keys"), "{codes:?}");
        assert!(codes.contains(&"type_mismatch"), "{codes:?}");

        let property = errors
            .iter()
            .find(|e| e.error == "unknown_property")
            .unwrap();
        assert_eq!(
            property.context().unwrap().did_you_mean.as_deref(),
            Some("Transform.rotation")
        );
        for error in &errors {
            let context = error.context().unwrap();
            assert!(context.line.is_some(), "{}", error.to_json());
            assert_eq!(context.file.as_deref(), Some("broken.anim.json"));
        }
    }

    #[test]
    fn a_valid_clip_validates_clean() {
        let source = r#"{
  "name": "spin",
  "tracks": [
    { "entity": "SpinCube", "property": "Transform.rotation",
      "interpolation": "linear",
      "keys": [ { "time": 0.0, "value": [0.0, 0.0, 0.0] },
                { "time": 2.0, "value": [0.0, 360.0, 0.0] } ] }
  ]
}"#;
        let errors = validate_clip_source(source, "spin.anim.json");
        assert!(errors.is_empty(), "{errors:?}");
    }
}
