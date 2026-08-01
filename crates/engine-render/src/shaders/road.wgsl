// Road shading and markings (M23).
//
// The geometry is one continuous ribbon — asphalt, shoulders and the
// embankment skirt are the same triangles — and every marking on it is
// computed here, per pixel, from two surface coordinates the vertex stage
// carries in the UVs:
//
//   u (uv.y) : signed metres from the centerline, along the cross-section,
//              positive to the driver's right. `|u| > half + shoulder` is the
//              skirt, whatever the profile does.
//   v (uv.x) : metres travelled along the centerline.
//
// Painting rather than building is what makes a marking follow the curve and
// the grade for free: a line is a band in `u`, so it bends with the road; a
// dash is periodic in `v`, so it is the same length in metres through a hairpin
// as on a straight. And paint cannot z-fight, because it is not a surface
// sitting on a surface — it is the same pixel, shaded differently.
//
// The lighting below re-derives what `mesh.wgsl` already contains. That is the
// precedent `water.wgsl` set and the reason is the same: M16 pinned the four
// lines computing `direct`/`ambient`/`base_color` in the mesh shader byte for
// byte against committed baselines, and restructuring arithmetic that is equal
// on paper has already moved one by a ULP, because FMA contraction depends on
// surrounding code. Sharing a function between the two shaders means editing
// those lines. Only `sky_common.wgsl` is shared, prepended at pipeline build,
// so the sky a wet road reflects cannot drift from the sky drawn behind it.

struct ObjectUniform {
    mvp: mat4x4<f32>,
    model: mat4x4<f32>,
    normal_matrix: mat4x4<f32>,
    // rgb = asphalt colour, w = metallic (always 0 for a road).
    albedo_metallic: vec4<f32>,
    // rgb = emissive, w = roughness.
    emissive_roughness: vec4<f32>,
    surface: vec4<f32>,
};

struct PointLightData {
    position_range: vec4<f32>,
    color: vec4<f32>,
};

const MAX_POINT_LIGHTS: u32 = 8u;
const MAX_ROAD_KERBS: u32 = 32u;

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

struct RoadUniform {
    // x = half the asphalt width, y = shoulder width, z = total road length,
    // w = centre-line dash period (0 = solid).
    metrics: vec4<f32>,
    // rgb = paint colour, w = edge-line width.
    paint: vec4<f32>,
    // x = edge inset, y = centre-line width, z = dash duty, w = start-line width.
    lines: vec4<f32>,
    // rgb = the red half of a kerb, w = kerb width.
    kerb: vec4<f32>,
    // rgb = shoulder colour, w = number of kerb spans.
    shoulder: vec4<f32>,
    // rgb = embankment colour, w = 1 when a start line is painted.
    bank: vec4<f32>,
    // x = where that line is, in metres along the centerline; y = grain amount
    // (0 = off), z = grain cell size in metres; w unused.
    start: vec4<f32>,
    // (start_v, end_v, side, stripe) per kerbed corner. `side` is +1 for the
    // driver's right — the inside of the turn, which only the plan-view
    // geometry knows, so the CPU decides it.
    kerbs: array<vec4<f32>, MAX_ROAD_KERBS>,
};

@group(0) @binding(0) var<uniform> object: ObjectUniform;
@group(1) @binding(0) var<uniform> frame: FrameUniform;
@group(2) @binding(0) var shadow_map: texture_depth_2d;
@group(2) @binding(1) var shadow_sampler: sampler_comparison;
@group(3) @binding(0) var<uniform> road: RoadUniform;

struct VertexOut {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    // x = v (metres along), y = u (metres across).
    @location(2) surface: vec2<f32>,
};

@vertex
fn vs_main(
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
) -> VertexOut {
    var out: VertexOut;
    out.clip_position = object.mvp * vec4<f32>(position, 1.0);
    out.world_position = (object.model * vec4<f32>(position, 1.0)).xyz;
    out.normal = (object.normal_matrix * vec4<f32>(normal, 0.0)).xyz;
    out.surface = uv;
    return out;
}

const PI: f32 = 3.14159265358979;

/// Coverage of the band `[low, high]` by a pixel at `x`, softened by one
/// pixel's worth of the coordinate.
///
/// `width` is a clamped `fwidth`. The clamp is load-bearing: a road seen at a
/// grazing angle 200 m away has enormous derivatives, and unclamped every
/// marking dissolves into a uniform grey haze rather than fading out.
fn band(x: f32, low: f32, high: f32, width: f32) -> f32 {
    let soft = max(width, 1e-5);
    return smoothstep(low - soft, low + soft, x) * (1.0 - smoothstep(high - soft, high + soft, x));
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

/// A hash of one integer grain cell, in `[0, 1)`.
///
/// Integer arithmetic rather than the usual `fract(sin(dot(p, k)) * 43758.5)`:
/// `sin` of a large argument is where two GPUs disagree first, and this repo's
/// house rule is that a generator sitting under a baseline writes its sequence
/// out rather than borrowing one whose precision it does not control (M19's
/// forests, M29's meadows). Integer ops are exact on every backend.
fn grain_hash(cell: vec2<f32>) -> f32 {
    let i = vec2<u32>(bitcast<u32>(i32(cell.x)), bitcast<u32>(i32(cell.y)));
    var h = i.x * 374761393u + i.y * 668265263u;
    h = (h ^ (h >> 13u)) * 1274126177u;
    h = h ^ (h >> 16u);
    return f32(h) * (1.0 / 4294967296.0);
}

/// Smooth value noise over the road's own surface coordinates.
fn grain_noise(p: vec2<f32>) -> f32 {
    let cell = floor(p);
    let f = p - cell;
    let w = f * f * (3.0 - 2.0 * f);
    let a = grain_hash(cell);
    let b = grain_hash(cell + vec2<f32>(1.0, 0.0));
    let c = grain_hash(cell + vec2<f32>(0.0, 1.0));
    let d = grain_hash(cell + vec2<f32>(1.0, 1.0));
    return mix(mix(a, b, w.x), mix(c, d, w.x), w.y);
}

/// What is painted at this point on the road: a colour and how much of the
/// pixel it covers, plus how much of that is glossier than asphalt.
struct Surface {
    albedo: vec3<f32>,
    /// How marked this pixel is, 0..1 — paint and kerbs both. Drives the
    /// roughness blend, so markings catch the sun the way fresh paint does.
    marked: f32,
    /// Grain's push on the roughness, and **exactly zero when grain is off**
    /// (M40) — the fragment stage branches on that rather than adding it, so
    /// the default path reaches the compiler as the arithmetic M23 shipped.
    roughness_bias: f32,
};

fn road_surface(v: f32, u: f32) -> Surface {
    let half = road.metrics.x;
    let shoulder_width = road.metrics.y;
    let total = road.metrics.z;
    let outer = half + shoulder_width;

    // One pixel's worth of each coordinate, clamped — see `band`.
    let du = clamp(fwidth(u), 0.0008, 0.5);
    let dv = clamp(fwidth(v), 0.0008, 0.5);
    let across = abs(u);

    var out: Surface;
    out.marked = 0.0;
    out.roughness_bias = 0.0;

    // The three surfaces, blended over a pixel rather than stepped, so the
    // asphalt edge does not crawl when the camera moves.
    let on_shoulder = smoothstep(half - du, half + du, across);
    let on_bank = smoothstep(outer - du, outer + du, across);
    out.albedo = mix(object.albedo_metallic.rgb, road.shoulder.rgb, on_shoulder);
    out.albedo = mix(out.albedo, road.bank.rgb, on_bank);

    // Asphalt grain (M40), before any paint so a line stays a clean line —
    // grain is the surface it is painted on, not something on top of it.
    //
    // The whole term is behind a branch rather than multiplied by an amount
    // that happens to be zero. The four lighting lines below are the ones this
    // repo pins byte for byte, and arithmetic added ahead of them can change
    // how the compiler contracts them even when it is arithmetically inert —
    // measured three separate times on `mesh.wgsl`. A uniform branch costs
    // nothing and makes "grain off is M23" structural rather than numerical.
    if road.start.y > 0.0 {
        let cell = max(road.start.z, 1e-3);
        let p = vec2<f32>(v, u) / cell;
        // Two octaves: the coarse one is the aggregate, the fine one the chips.
        let noise = grain_noise(p) * 0.65 + grain_noise(p * 2.7 + vec2<f32>(11.3, 5.1)) * 0.35;
        let signed = noise - 0.5;
        out.albedo = out.albedo * (1.0 + signed * road.start.y * 0.55);
        // Rougher in the hollows, smoother on the polished aggregate. This is
        // most of what stops a wide road reading as a painted plane when the
        // sun is low.
        out.roughness_bias = signed * road.start.y * 0.16;
    }

    // Paint only reaches the asphalt and its immediate edge.
    let paintable = 1.0 - on_bank;

    var paint = 0.0;

    // Edge lines, inset from the asphalt edge by `edge_inset`.
    if road.paint.w > 0.0 {
        let line_outer = half - road.lines.x;
        let line_inner = line_outer - road.paint.w;
        paint = max(paint, band(across, line_inner, line_outer, du));
    }

    // The centre line, dashed by arc length. On a closed road the period was
    // fitted so a whole number of dashes covers the lap, which is why the
    // pattern meets itself exactly at the seam.
    if road.lines.y > 0.0 {
        var along = 1.0;
        if road.metrics.w > 0.0 {
            let period = road.metrics.w;
            let phase = fract(v / period);
            let soft = clamp(dv / period, 1e-5, 0.5);
            along = band(phase, 0.0, road.lines.z, soft);
            // A dash straddling the wrap of `fract` would otherwise be cut in
            // half; catch its far end as it comes back round.
            along = max(along, band(phase - 1.0, 0.0, road.lines.z, soft));
        }
        paint = max(paint, band(across, 0.0, road.lines.y * 0.5, du) * along);
    }

    // The start line, across the road wherever it was placed. The distance is
    // measured the short way round, so a line at v = 0 is also a line at
    // v = total — the same place on a closed road.
    if road.bank.w > 0.5 && road.lines.w > 0.0 {
        let offset = abs(v - road.start.x);
        let along = min(offset, max(total - offset, 0.0));
        paint = max(paint, band(along, 0.0, road.lines.w * 0.5, dv) * (1.0 - on_shoulder));
    }

    paint = paint * paintable;
    out.albedo = mix(out.albedo, road.paint.rgb, paint);
    out.marked = paint;

    // Kerbs last, so they cover the edge line rather than being striped by it.
    // A kerb straddles the asphalt edge: a quarter of its width inside, the
    // rest out on the shoulder, which is where a real one sits.
    let kerb_width = road.kerb.w;
    let count = u32(road.shoulder.w);
    if kerb_width > 0.0 && count > 0u {
        let inner = half - kerb_width * 0.25;
        let outer_edge = inner + kerb_width;
        let across_band = band(across, inner, outer_edge, du);
        if across_band > 0.0 {
            for (var i = 0u; i < MAX_ROAD_KERBS; i = i + 1u) {
                if i >= count {
                    break;
                }
                let span = road.kerbs[i];
                // Only the inside of the turn is kerbed.
                if sign(u) != span.z {
                    continue;
                }
                let along = band(v, span.x, span.y, dv);
                if along <= 0.0 {
                    continue;
                }
                // Alternating stripes, fitted so a whole number covers the
                // corner: a kerb begins and ends on a stripe boundary.
                let stripe = max(span.w, 1e-4);
                let index = floor((v - span.x) / stripe);
                let red = step(0.5, fract(index * 0.5));
                let color = mix(road.paint.rgb, road.kerb.rgb, red);
                let coverage = across_band * along * paintable;
                out.albedo = mix(out.albedo, color, coverage);
                out.marked = max(out.marked, coverage);
            }
        }
    }

    return out;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    let painted = road_surface(in.surface.x, in.surface.y);
    let albedo = painted.albedo;
    let metallic = object.albedo_metallic.w;
    let emissive = object.emissive_roughness.rgb;
    // Paint and kerbs are glossier than the asphalt around them, which is most
    // of what makes fresh markings read as painted rather than as recoloured
    // road when the sun is low.
    var roughness = max(mix(object.emissive_roughness.w, 0.55, painted.marked), 0.045);
    // Grain's push, applied only when there is grain — see `Surface`.
    if painted.roughness_bias != 0.0 {
        roughness = clamp(roughness + painted.roughness_bias, 0.045, 1.0);
    }

    let n = normalize(in.normal);
    let v = normalize(frame.camera_pos.xyz - in.world_position);
    let l = normalize(-frame.sun_direction.xyz);
    let h = normalize(v + l);

    let n_dot_l = max(dot(n, l), 0.0);
    let n_dot_v = max(dot(n, v), 1e-4);
    let n_dot_h = max(dot(n, h), 0.0);
    let v_dot_h = max(dot(v, h), 0.0);

    let alpha = roughness * roughness;
    let alpha2 = alpha * alpha;
    let d_denom = n_dot_h * n_dot_h * (alpha2 - 1.0) + 1.0;
    let d = alpha2 / (PI * d_denom * d_denom);

    let ggx_v = n_dot_l * sqrt(n_dot_v * n_dot_v * (1.0 - alpha2) + alpha2);
    let ggx_l = n_dot_v * sqrt(n_dot_l * n_dot_l * (1.0 - alpha2) + alpha2);
    let visibility = 0.5 / max(ggx_v + ggx_l, 1e-5);

    let f0 = mix(vec3<f32>(0.04), albedo, metallic);
    let fresnel = f0 + (vec3<f32>(1.0) - f0) * pow(1.0 - v_dot_h, 5.0);

    let specular = d * visibility * fresnel;
    let diffuse = albedo * (1.0 - metallic);

    var shade = 1.0;
    if frame.params.y > 0.5 {
        shade = shadow_factor(in.world_position, n_dot_l);
    }

    var fill = albedo * frame.ambient.rgb;
    var reflection = vec3<f32>(0.0);
    if frame.params.w > 0.5 {
        let hemisphere = sky_ambient(n);
        fill = albedo * hemisphere;

        let mirror = sky_gradient(
            reflect(-v, n),
            frame.sky_zenith.rgb,
            frame.sky_horizon.rgb,
            frame.sky_ground.rgb,
        );
        let sharpness = (1.0 - roughness) * (1.0 - roughness);
        let environment = mix(hemisphere, mirror, sharpness);
        // Roughness-capped Schlick, for M16's reason: a road is seen at
        // grazing incidence nearly everywhere, and uncapped Fresnel turns dry
        // asphalt into a sheet of sky.
        let ceiling = max(vec3<f32>(1.0 - roughness), f0);
        let view_fresnel = f0 + (ceiling - f0) * pow(1.0 - n_dot_v, 5.0);
        reflection = environment * view_fresnel;
    }

    var color = (diffuse + specular) * frame.sun_color.rgb * n_dot_l * shade
        + fill
        + reflection
        + emissive;

    let point_count = u32(frame.params2.x);
    if point_count > 0u {
        var point_diffuse = vec3<f32>(0.0);
        var point_specular = vec3<f32>(0.0);
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
                diffuse,
                f0,
                roughness,
            );
            point_diffuse = point_diffuse + sample.diffuse;
            point_specular = point_specular + sample.specular;
        }
        color = color + point_diffuse + point_specular;
    }

    if frame.params.x > 0.0 {
        let distance = length(in.world_position - frame.camera_pos.xyz);
        let amount = clamp(1.0 - exp(-pow(distance * frame.params.x, 2.0)), 0.0, 1.0);
        color = mix(color, frame.sky_horizon.rgb, amount);
    }

    return vec4<f32>(clamp(color, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
