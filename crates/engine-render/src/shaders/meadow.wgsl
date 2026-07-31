// Meadow: ground cover with a life cycle (M29).
//
// This is the only shader in the engine whose vertex stage does the *modelling*.
// The CPU hands it one plant template and a list of places to put copies of it;
// everything that makes a plant a seed, a sprout, a stand of grass, a flowering
// weed, dry straw or a collapsed stalk happens here, from `time`.
//
// It has to work this way. A plant's shape changes continuously, and the
// engine's geometry cache keys on `Arc` identity (M15) — regenerating a meadow
// on the CPU would mint a new vertex buffer every frame and re-upload tens of
// thousands of plants with it. `water.wgsl` made this trade first, for waves;
// a meadow is the harder version, because a plant does not merely move, it
// changes which *organs* it has.
//
// The trick that makes that fit in a static buffer: the template carries every
// organ any stage will ever need, and each vertex carries the phase window its
// organ lives in. Outside that window the organ scales to zero about its own
// anchor, and zero-area triangles rasterize nothing. So a flower is "not there"
// for two thirds of the cycle without a second draw call, an index rewrite, or
// a branch that could diverge across a warp.
//
// A separate pipeline from `mesh.wgsl`, following `water.wgsl`, `clouds.wgsl`
// and `road.wgsl`, for the reason M16 wrote down: the four lines computing
// `direct`/`ambient`/`base_color` in the mesh shader are pinned byte for byte
// against committed baselines, whether the compiler contracts them into FMAs
// depends on the code around them, and sharing a lighting function between two
// shaders means editing them. Only `sky_common.wgsl` is shared, prepended at
// pipeline build. The lighting below is therefore a deliberate near-copy of
// `road.wgsl`'s.
//
// What is not here: shadow *casting* (a 2048² map cannot resolve a blade of
// grass; what it would record is sub-texel noise that crawls as the ortho box
// slides, which reads as a bug — see §7 of the design doc), and any texture
// maps.

const MAX_GROWTH_STAGES: u32 = 8u;
const MAX_POINT_LIGHTS: u32 = 8u;
const PI: f32 = 3.14159265358979;
const TAU: f32 = 6.28318530717959;

/// Organ tags, matching `engine_core::meadow::ORGAN_*`.
const ORGAN_BLADE: u32 = 0u;
const ORGAN_FLOWER: u32 = 1u;
const ORGAN_SEED_HEAD: u32 = 2u;

/// How dark a plant is at its root relative to its tip.
///
/// This is standing in for the self-shadowing a meadow does not get — grass
/// does not cast into the shadow map, and without *something* darkening the
/// base of every plant a field reads as a carpet of bright spikes pasted onto
/// the ground rather than as vegetation growing out of it. It is the cheapest
/// term in this file and close to the most important.
const ROOT_SHADE: f32 = 0.35;

/// How much sunlight passes *through* a leaf toward the camera.
///
/// A blade of grass is thin enough to be translucent, which is why a meadow
/// with the sun behind it glows. Without this the same field lit from behind is
/// a flat dark mass, and low sun is exactly when a meadow is worth looking at.
const BACKLIGHT: f32 = 0.7;
const BACKLIGHT_FOCUS: f32 = 3.0;

/// Grass is a rough dielectric. A field rather than a constant was considered
/// and dropped: `Meadow` already carries seventeen, and nothing was asking to
/// tune this one.
const ROUGHNESS: f32 = 0.78;

/// Metres per unit of the wind-noise coordinate — the size of a gust.
const GUST_SCALE: f32 = 0.06;

/// How wide the emerge/wither ramp is, in phase. Wide enough that a flower
/// opens rather than pops, narrow enough that it is fully out for most of the
/// window it is authored to occupy.
const ORGAN_FADE: f32 = 0.05;

struct GrowthStageData {
    // x = at, y = height fraction, z = width fraction, w = lean in radians.
    shape: vec4<f32>,
    // rgb = colour at the plant's base, w = sway multiplier.
    color_sway: vec4<f32>,
    // rgb = colour at the plant's tip, w unused.
    tip: vec4<f32>,
};

struct MeadowUniform {
    // World → clip. There is no model matrix: instances are placed in world
    // space, because their altitude came off the terrain and a transform
    // applied afterwards would lift them off it.
    view_proj: mat4x4<f32>,
    // x = scene time in seconds, y = cycle length in seconds (0 = frozen),
    // z = base phase, w = live stage count.
    clock: vec4<f32>,
    // xy = unit wind direction in XZ, z = wind strength in radians,
    // w = gust travel speed in metres per second.
    wind: vec4<f32>,
    // rgb = flower colour, w = reseed jitter radius in metres.
    flower: vec4<f32>,
    stages: array<GrowthStageData, MAX_GROWTH_STAGES>,
};

struct PointLightData {
    position_range: vec4<f32>,
    color: vec4<f32>,
};

struct FrameUniform {
    camera_pos: vec4<f32>,
    sun_direction: vec4<f32>,
    sun_color: vec4<f32>,
    ambient: vec4<f32>,
    inv_view_proj: mat4x4<f32>,
    light_view_proj: mat4x4<f32>,
    sky_zenith: vec4<f32>,
    sky_horizon: vec4<f32>,
    sky_ground: vec4<f32>,
    // x = fog density, y = shadows on, z = shadow-map texel size, w = sky on.
    params: vec4<f32>,
    // x = live point-light count.
    params2: vec4<f32>,
    point_lights: array<PointLightData, MAX_POINT_LIGHTS>,
};

@group(0) @binding(0) var<uniform> meadow: MeadowUniform;
@group(1) @binding(0) var<uniform> frame: FrameUniform;
@group(2) @binding(0) var shadow_map: texture_depth_2d;
@group(2) @binding(1) var shadow_sampler: sampler_comparison;

struct VertexOut {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    // rgb = this vertex's own albedo, a = the parameter along the plant.
    @location(2) tint: vec4<f32>,
};

// ── the reseed hash ────────────────────────────────────────────────────────
//
// A format contract, spelled out here rather than pulled from anywhere, exactly
// as `particles.rs`, `tree.rs`, `cloud.rs`, `terrain.rs` and `meadow.rs` spell
// theirs out: a meadow render sits under a `diff-render` baseline, so what these
// bits do is part of what a scene file means.

fn hash_u32(value: u32) -> u32 {
    var h = value;
    h = h ^ (h >> 16u);
    h = h * 0x7FEB352Du;
    h = h ^ (h >> 15u);
    h = h * 0x846CA68Bu;
    h = h ^ (h >> 16u);
    return h;
}

/// A draw in `[0, 1)` from a seed and a salt.
fn rand01(seed: u32, salt: u32) -> f32 {
    return f32(hash_u32(seed ^ hash_u32(salt)) >> 8u) / 16777216.0;
}

/// Smooth 1-D value noise. Smooth because per-step randomness makes a blade
/// *vibrate* rather than sway — M17's turbulence learned this and it is the
/// same mistake available here.
fn value_noise(x: f32) -> f32 {
    let cell = floor(x);
    let f = x - cell;
    let smoothed = f * f * (3.0 - 2.0 * f);
    let index = bitcast<u32>(i32(cell));
    let a = rand01(index, 0x9E3779B9u);
    let b = rand01(index + 1u, 0x9E3779B9u);
    return mix(a, b, smoothed);
}

/// The life-cycle table at `phase`, interpolated and **wrapping** from the last
/// keyframe round to the first — so the collapse keyframe fades back into the
/// seed keyframe without anyone authoring phase 1.0 as a copy of phase 0.0.
fn sample_stage(phase: f32) -> GrowthStageData {
    let count = max(u32(meadow.clock.w), 1u);

    // The last keyframe at or before `phase`. If there is none — a table whose
    // first `at` is above 0 — this stays at the final keyframe, which is the
    // wrap and is correct.
    var lower = count - 1u;
    for (var i = 0u; i < MAX_GROWTH_STAGES; i = i + 1u) {
        if i >= count {
            break;
        }
        if meadow.stages[i].shape.x <= phase {
            lower = i;
        }
    }
    let upper = (lower + 1u) % count;

    var span = meadow.stages[upper].shape.x - meadow.stages[lower].shape.x;
    if span <= 0.0 {
        span = span + 1.0;
    }
    var local = phase - meadow.stages[lower].shape.x;
    if local < 0.0 {
        local = local + 1.0;
    }
    let t = clamp(local / max(span, 1e-5), 0.0, 1.0);

    var out: GrowthStageData;
    out.shape = mix(meadow.stages[lower].shape, meadow.stages[upper].shape, t);
    out.color_sway = mix(meadow.stages[lower].color_sway, meadow.stages[upper].color_sway, t);
    out.tip = mix(meadow.stages[lower].tip, meadow.stages[upper].tip, t);
    return out;
}

/// How much of an organ exists at `phase`. `[0, 1]` covering the whole cycle is
/// "always", which is what every blade carries.
fn organ_scale(phase: f32, emerge: f32, wither: f32) -> f32 {
    if emerge <= 0.0 && wither >= 1.0 {
        return 1.0;
    }
    let opening = smoothstep(emerge, emerge + ORGAN_FADE, phase);
    let closing = 1.0 - smoothstep(wither - ORGAN_FADE, wither, phase);
    return opening * closing;
}

@vertex
fn vs_main(
    // The template. `centre` is the plant's centre line in unit-height space;
    // `offset` is this vertex's displacement from it, in metres. They scale
    // differently — height by the stage's height, girth by its width — which is
    // the whole reason they are separate attributes.
    @location(0) centre: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) offset: vec3<f32>,
    @location(3) anchor: vec3<f32>,
    // x = parameter along the plant, y = emerge, z = wither.
    @location(4) span: vec3<f32>,
    @location(5) organ: u32,
    // The instance. xyz = world position, w = metres of plant per unit height.
    @location(6) pos_scale: vec4<f32>,
    // x = yaw, y = phase offset, zw = the ground's slope here.
    @location(7) instance: vec4<f32>,
    @location(8) seed: u32,
) -> VertexOut {
    // ── where in the cycle this plant is ──────────────────────────────────
    //
    // `cycle_length == 0` freezes the field at the base phase, the way
    // `daylight.day_length: 0` freezes the day. The plant's own offset still
    // applies, so a frozen meadow has variety rather than being one plant
    // repeated.
    var progress = meadow.clock.z + instance.y;
    if meadow.clock.y > 0.0 {
        progress = progress + meadow.clock.x / meadow.clock.y;
    }
    let phase = fract(progress);
    let generation = bitcast<u32>(i32(floor(progress)));

    // ── the reseed ────────────────────────────────────────────────────────
    //
    // This is what makes the cycle regrowth rather than an animation loop.
    // Hashing the plant's seed *with the generation number* gives it a fresh
    // position, height, lean and heading every time round, so the stalk that
    // died and the sprout that replaces it are not on the same spot — which is
    // what a meadow reseeding itself actually looks like. It costs one integer
    // hash and no state anywhere.
    let reseed = hash_u32(seed ^ hash_u32(generation));
    let jitter = meadow.flower.w;
    let drift = vec2<f32>(
        (rand01(reseed, 1u) * 2.0 - 1.0) * jitter,
        (rand01(reseed, 2u) * 2.0 - 1.0) * jitter,
    );
    let height_mul = 0.75 + rand01(reseed, 3u) * 0.5;
    let yaw = instance.x + rand01(reseed, 4u) * TAU;
    let lean_mul = 0.75 + rand01(reseed, 5u) * 0.5;

    // The new spot is a few centimetres from the old one, and on a hillside
    // that is a few centimetres of altitude too. First order off the ground's
    // own gradient — the alternative is grass that buries itself uphill.
    let root = vec3<f32>(
        pos_scale.x + drift.x,
        pos_scale.y + instance.z * drift.x + instance.w * drift.y,
        pos_scale.z + drift.y,
    );

    // ── the stage ─────────────────────────────────────────────────────────
    let stage = sample_stage(phase);
    let plant_height = pos_scale.w * stage.shape.y * height_mul;
    let width = stage.shape.z;
    let grown = organ_scale(phase, span.y, span.z);

    // Grow the organ out of its own anchor: a blade rises from the root, a
    // flower opens at the top of its stem. Scaling everything about the plant's
    // origin instead would slide the flower down the stalk as it opened.
    let anchored = anchor * plant_height;
    let centred = anchored + (centre * plant_height - anchored) * grown;
    var local = centred + offset * (width * grown);
    var local_normal = normal;

    // ── heading ───────────────────────────────────────────────────────────
    let cos_yaw = cos(yaw);
    let sin_yaw = sin(yaw);
    local = vec3<f32>(
        local.x * cos_yaw - local.z * sin_yaw,
        local.y,
        local.x * sin_yaw + local.z * cos_yaw,
    );
    local_normal = vec3<f32>(
        local_normal.x * cos_yaw - local_normal.z * sin_yaw,
        local_normal.y,
        local_normal.x * sin_yaw + local_normal.z * cos_yaw,
    );

    // ── the bend: lean and wind, as one cantilever ────────────────────────
    //
    // Both are the same deformation — a rotation about the root whose angle
    // grows with the parameter along the plant, so the plant *curves* instead
    // of hinging at the ground like a felled tree. The stage's `lean` is the
    // steady part (and at the end of the cycle it is the collapse); the wind is
    // the moving part.
    //
    // Gusts are sampled against a coordinate that travels with them, which is
    // what makes wind cross a field as a visible wave. Sampling at the plant's
    // own position with time added instead makes every plant shimmer
    // independently, which reads as noise rather than as weather.
    let bend_dir = meadow.wind.xy;
    let travel = dot(vec2<f32>(root.x, root.z), bend_dir) * GUST_SCALE
        - meadow.clock.x * meadow.wind.w * GUST_SCALE;
    let gust = value_noise(travel) * 0.65 + value_noise(travel * 2.7 + 11.0) * 0.35;
    let sway = meadow.wind.z * stage.color_sway.w * (0.35 + 0.65 * gust);
    let theta = (stage.shape.w * lean_mul + sway) * span.x;

    let bend_sin = sin(theta);
    let bend_cos = cos(theta);
    let flat = vec2<f32>(local.x, local.z);
    let along = dot(flat, bend_dir);
    let perp = flat - bend_dir * along;
    let bent_along = along * bend_cos + local.y * bend_sin;
    let bent_flat = perp + bend_dir * bent_along;
    let bent = vec3<f32>(bent_flat.x, local.y * bend_cos - along * bend_sin, bent_flat.y);

    // The same rotation on the normal, so a leaning blade catches the light
    // from where it is actually facing.
    let n_flat = vec2<f32>(local_normal.x, local_normal.z);
    let n_along = dot(n_flat, bend_dir);
    let n_perp = n_flat - bend_dir * n_along;
    let n_bent_along = n_along * bend_cos + local_normal.y * bend_sin;
    let n_bent_flat = n_perp + bend_dir * n_bent_along;
    let bent_normal = vec3<f32>(
        n_bent_flat.x,
        local_normal.y * bend_cos - n_along * bend_sin,
        n_bent_flat.y,
    );

    // ── out ───────────────────────────────────────────────────────────────
    let world = root + bent;

    // Colour is decided here rather than in the fragment stage because the
    // stage table lookup is per plant, not per pixel: a loop over the keyframes
    // in the fragment stage would run it once for every pixel of every blade to
    // reach the same answer for all of them.
    var albedo = mix(stage.color_sway.rgb, stage.tip.rgb, span.x);
    if organ == ORGAN_FLOWER {
        albedo = meadow.flower.rgb;
    } else if organ == ORGAN_SEED_HEAD {
        albedo = stage.tip.rgb;
    }

    var out: VertexOut;
    out.clip_position = meadow.view_proj * vec4<f32>(world, 1.0);
    out.world_position = world;
    out.normal = bent_normal;
    out.tint = vec4<f32>(albedo, span.x);
    return out;
}

fn shadow_factor(world_position: vec3<f32>, n_dot_l: f32) -> f32 {
    let light_clip = frame.light_view_proj * vec4<f32>(world_position, 1.0);
    let projected = light_clip.xyz / light_clip.w;

    if projected.z > 1.0 || projected.z < 0.0 {
        return 1.0;
    }
    let inset = max(abs(projected.x), abs(projected.y));
    if inset > 1.0 {
        return 1.0;
    }

    let uv = vec2<f32>(projected.x * 0.5 + 0.5, 0.5 - projected.y * 0.5);
    let slope = sqrt(max(1.0 - n_dot_l * n_dot_l, 0.0)) / max(n_dot_l, 0.05);
    let bias = clamp(0.0006 * slope, 0.0004, 0.006);
    let reference = projected.z - bias;

    let texel = frame.params.z;
    var sum = 0.0;
    for (var y = -1; y <= 1; y = y + 1) {
        for (var x = -1; x <= 1; x = x + 1) {
            let offset = vec2<f32>(f32(x), f32(y)) * texel;
            sum = sum + textureSampleCompareLevel(
                shadow_map,
                shadow_sampler,
                uv + offset,
                reference,
            );
        }
    }

    return mix(sum / 9.0, 1.0, smoothstep(0.85, 1.0, inset));
}

fn sky_ambient(n: vec3<f32>) -> vec3<f32> {
    let up = n.y * 0.5 + 0.5;
    let env = mix(frame.sky_ground.rgb, frame.sky_zenith.rgb, up);
    let mean = max((frame.sky_ground.rgb + frame.sky_zenith.rgb) * 0.5, vec3<f32>(1e-4));
    return frame.ambient.rgb * (env / mean);
}

struct LightSample {
    diffuse: vec3<f32>,
    specular: vec3<f32>,
};

fn point_light_falloff(distance: f32, range: f32) -> f32 {
    let ratio = clamp(distance / range, 0.0, 1.0);
    let ratio4 = ratio * ratio * ratio * ratio;
    let window = 1.0 - ratio4;
    return (window * window) / max(distance * distance, 1e-4);
}

fn evaluate_point_light(
    light: PointLightData,
    world_position: vec3<f32>,
    n: vec3<f32>,
    v: vec3<f32>,
    n_dot_v: f32,
    diffuse_color: vec3<f32>,
    f0: vec3<f32>,
    roughness: f32,
) -> LightSample {
    var out: LightSample;
    out.diffuse = vec3<f32>(0.0);
    out.specular = vec3<f32>(0.0);

    let to_light = light.position_range.xyz - world_position;
    let distance = length(to_light);
    let range = light.position_range.w;
    if distance >= range {
        return out;
    }

    let l = to_light / max(distance, 1e-4);
    let n_dot_l = max(dot(n, l), 0.0);
    if n_dot_l <= 0.0 {
        return out;
    }

    let h = normalize(v + l);
    let n_dot_h = max(dot(n, h), 0.0);
    let v_dot_h = max(dot(v, h), 0.0);

    let alpha = roughness * roughness;
    let alpha2 = alpha * alpha;
    let d_denom = n_dot_h * n_dot_h * (alpha2 - 1.0) + 1.0;
    let d = alpha2 / (PI * d_denom * d_denom);

    let ggx_v = n_dot_l * sqrt(n_dot_v * n_dot_v * (1.0 - alpha2) + alpha2);
    let ggx_l = n_dot_v * sqrt(n_dot_l * n_dot_l * (1.0 - alpha2) + alpha2);
    let visibility = 0.5 / max(ggx_v + ggx_l, 1e-5);

    let fresnel = f0 + (vec3<f32>(1.0) - f0) * pow(1.0 - v_dot_h, 5.0);
    let radiance = light.color.rgb * point_light_falloff(distance, range) * n_dot_l;

    out.diffuse = diffuse_color * radiance;
    out.specular = d * visibility * fresnel * radiance;
    return out;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    let t = in.tint.a;
    // The root darkening (see `ROOT_SHADE`). Applied to the albedo rather than
    // to the finished colour so it darkens the ambient fill too — the base of a
    // plant is occluded from the sky as much as from the sun.
    let albedo = in.tint.rgb * mix(ROOT_SHADE, 1.0, t);

    let v = normalize(frame.camera_pos.xyz - in.world_position);
    // Culling is off for this pipeline — a blade is a single-sided strip and
    // half of every tuft faces away — so the normal is flipped toward the
    // viewer. `clouds.wgsl` deliberately does *not* do this, because a cloud's
    // far wall should shade as a far wall; a leaf has no inside, so here the
    // two faces are the same surface and should light identically.
    var n = normalize(in.normal);
    if dot(n, v) < 0.0 {
        n = -n;
    }

    let l = normalize(-frame.sun_direction.xyz);
    let h = normalize(v + l);

    let n_dot_l = max(dot(n, l), 0.0);
    let n_dot_v = max(dot(n, v), 1e-4);
    let n_dot_h = max(dot(n, h), 0.0);
    let v_dot_h = max(dot(v, h), 0.0);

    let alpha = ROUGHNESS * ROUGHNESS;
    let alpha2 = alpha * alpha;
    let d_denom = n_dot_h * n_dot_h * (alpha2 - 1.0) + 1.0;
    let d = alpha2 / (PI * d_denom * d_denom);

    let ggx_v = n_dot_l * sqrt(n_dot_v * n_dot_v * (1.0 - alpha2) + alpha2);
    let ggx_l = n_dot_v * sqrt(n_dot_l * n_dot_l * (1.0 - alpha2) + alpha2);
    let visibility = 0.5 / max(ggx_v + ggx_l, 1e-5);

    let f0 = vec3<f32>(0.04);
    let fresnel = f0 + (vec3<f32>(1.0) - f0) * pow(1.0 - v_dot_h, 5.0);

    let specular = d * visibility * fresnel;

    var shade = 1.0;
    if frame.params.y > 0.5 {
        shade = shadow_factor(in.world_position, n_dot_l);
    }

    var fill = albedo * frame.ambient.rgb;
    if frame.params.w > 0.5 {
        fill = albedo * sky_ambient(n);
    }

    var color = (albedo + specular) * frame.sun_color.rgb * n_dot_l * shade + fill;

    // Transmission. A leaf held up to the sun glows, and a meadow is thousands
    // of leaves held up to the sun. Gated on the shadow term, because a blade
    // standing in someone else's shadow has no sunlight to pass through it.
    let through = pow(max(dot(-v, l), 0.0), BACKLIGHT_FOCUS);
    color = color + albedo * frame.sun_color.rgb * (through * BACKLIGHT * shade);

    let point_count = u32(frame.params2.x);
    if point_count > 0u {
        for (var i = 0u; i < MAX_POINT_LIGHTS; i = i + 1u) {
            if i >= point_count {
                break;
            }
            let sample = evaluate_point_light(
                frame.point_lights[i],
                in.world_position,
                n,
                v,
                n_dot_v,
                albedo,
                f0,
                ROUGHNESS,
            );
            color = color + sample.diffuse + sample.specular;
        }
    }

    if frame.params.x > 0.0 {
        let distance = length(in.world_position - frame.camera_pos.xyz);
        let amount = clamp(1.0 - exp(-pow(distance * frame.params.x, 2.0)), 0.0, 1.0);
        color = mix(color, frame.sky_horizon.rgb, amount);
    }

    return vec4<f32>(clamp(color, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
