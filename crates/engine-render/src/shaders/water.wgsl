// Water surfaces (M18): Gerstner waves in the vertex stage, water optics in the
// fragment stage.
//
// A separate pipeline from `mesh.wgsl` rather than another `Material` branch,
// for two reasons. Water needs things no mesh does — displaced geometry, the
// scene's depth behind the surface, foam — and `mesh.wgsl` is the file the
// repo declares untouchable, because whether the compiler contracts its four
// M4 lines into FMAs depends on the code around them. A new file cannot move a
// pixel in a scene that has no water in it.
//
// Deliberate duplications, and why they are not shared:
//
// - `FrameUniform` is declared here as a *prefix* of the Rust struct, exactly
//   as `sky.wgsl` already does. WGSL has no `#include`, the layout's authority
//   is the Rust `FrameUniform`, and a shared declaration would have to be
//   prepended onto `mesh.wgsl` — see above. The point lights at the end of that
//   struct are the one thing water does not read (`water-design.md` §8).
// - `shadow_lit` is a near-copy of `mesh.wgsl`'s `shadow_factor` for the same
//   reason. It differs where water differs: a water surface is nearly flat, so
//   the slope-scaled bias has nothing to do, and a constant one is enough.
//
// The sky gradient is the exception: `sky_common.wgsl` is prepended to this
// source, because a surface that reflects a *different* sky from the one drawn
// behind it is a worse artifact than one that reflects nothing.

struct WaterUniform {
    // World → clip. Not an MVP: waves displace in world space, so the model
    // transform is applied first and separately.
    view_proj: mat4x4<f32>,
    model: mat4x4<f32>,
    // xyz = shallow color, w = detail strength.
    shallow_detail: vec4<f32>,
    // xyz = deep color, w = depth fade in metres.
    deep_fade: vec4<f32>,
    // xyz = foam color, w = shore foam width in metres.
    foam: vec4<f32>,
    // x = roughness, y = opacity, z = crest foam, w = detail cell size.
    params: vec4<f32>,
    // x = wave count, y = time in seconds, z and w unused.
    clock: vec4<f32>,
    // Two vec4s per wave: (direction.x, direction.z, amplitude, k) and
    // (q, omega, unused, unused). See `pack_waves` on the Rust side.
    waves: array<vec4<f32>, 16>,
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
};

@group(0) @binding(0) var<uniform> surface: WaterUniform;
@group(1) @binding(0) var<uniform> frame: FrameUniform;
@group(2) @binding(0) var shadow_map: texture_depth_2d;
@group(2) @binding(1) var shadow_sampler: sampler_comparison;
// The opaque pass's depth, resolved to a single-sampled R32Float copy. Sampled
// with `textureLoad`, so there is no sampler and nothing filters it. It rides
// in the frame-textures group beside the shadow map since M26 — the two are
// alike in every way that matters to a bind group, and the fourth slot they
// were costing is what a material needs.
@group(2) @binding(2) var scene_depth: texture_2d<f32>;

const PI: f32 = 3.14159265358979;
const TAU: f32 = 6.28318530717959;
// Fresnel reflectance of water at normal incidence (IOR 1.33).
const WATER_F0: f32 = 0.02;

struct VertexOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world: vec3<f32>,
    // Analytic surface normal from the wave derivatives, not the grid's.
    @location(1) normal: vec3<f32>,
    // Horizontal Jacobian determinant: 1 is undisturbed, 0 is a fold. This is
    // where a real wave breaks, which is why it drives the crest foam.
    @location(2) fold: f32,
};

@vertex
fn vs_main(@location(0) position: vec3<f32>) -> VertexOut {
    let base = (surface.model * vec4<f32>(position, 1.0)).xyz;

    // Gerstner sum. Each wave moves the surface toward its crests as well as up
    // — that horizontal gather is what sharpens crests and flattens troughs,
    // and it is the whole reason this is not a sum of sines.
    //
    // Derivatives come out of the same sines and cosines, so the normal is
    // exact and costs almost nothing. Finite differences would need three
    // evaluations and would still be wrong at the crests.
    var displaced = base;
    // Partial derivatives of the displaced position with respect to the
    // undisturbed x and z, accumulated alongside.
    var dx = vec3<f32>(1.0, 0.0, 0.0);
    var dz = vec3<f32>(0.0, 0.0, 1.0);

    let count = i32(surface.clock.x);
    let time = surface.clock.y;
    for (var i = 0; i < count; i = i + 1) {
        let a = surface.waves[i * 2];
        let b = surface.waves[i * 2 + 1];
        let direction = a.xy;
        let amplitude = a.z;
        let k = a.w;
        let q = b.x;
        let omega = b.y;

        // Phase from the *undisturbed* position, which is what keeps the sum
        // separable and the derivatives closed-form.
        let phase = k * dot(direction, base.xz) - omega * time;
        let s = sin(phase);
        let c = cos(phase);
        let qa = q * amplitude;
        let ka = k * amplitude;

        displaced = displaced
            + vec3<f32>(qa * direction.x * c, amplitude * s, qa * direction.y * c);

        // d/dx and d/dz of the line above.
        dx = dx
            + vec3<f32>(
                -q * ka * direction.x * direction.x * s,
                ka * direction.x * c,
                -q * ka * direction.x * direction.y * s,
            );
        dz = dz
            + vec3<f32>(
                -q * ka * direction.y * direction.x * s,
                ka * direction.y * c,
                -q * ka * direction.y * direction.y * s,
            );
    }

    var out: VertexOut;
    out.clip = surface.view_proj * vec4<f32>(displaced, 1.0);
    out.world = displaced;
    // cross(dz, dx) — that order is what gives +Y on an undisturbed surface.
    out.normal = normalize(cross(dz, dx));
    out.fold = dx.x * dz.z - dz.x * dx.z;
    return out;
}

/// Slope of the small-scale ripples at a world position.
///
/// Four scrolling sine trains, each shorter, faster and shallower than the one
/// before, rotated by the golden angle so they never line up into a visible
/// grid. Deep-water dispersion (`omega = sqrt(g·k)`) sets the speeds, so the
/// short ripples run over the long ones the way real capillary waves do.
///
/// This perturbs the normal only. It is what puts glitter *between* the grid
/// vertices — per line of code the largest single difference between blue glass
/// and water — and precisely because nothing physical may depend on it, it is
/// free to be a slope field with no height behind it.
///
/// The amplitudes are set so that `detail: 1.0` tilts the surface by at most
/// about 10°, and each layer's slope *decays*. Both matter more than they look:
/// a slope field is a mirror being shaken, so overshooting turns a lake into
/// white noise — and because the layers are all in the same phase somewhere,
/// the worst case is their sum, not their average.
fn ripple_slope(p: vec2<f32>, time: f32, scale: f32) -> vec2<f32> {
    var slope = vec2<f32>(0.0, 0.0);
    var direction = vec2<f32>(0.70710678, 0.70710678);
    var wavelength = max(scale, 1e-3);
    var amplitude = 0.010 * wavelength;
    let c = cos(2.39996323);
    let s = sin(2.39996323);

    for (var i = 0; i < 4; i = i + 1) {
        let k = TAU / wavelength;
        let omega = sqrt(9.81 * k) * 0.35;
        let phase = k * dot(direction, p) - omega * time;
        slope = slope + direction * (amplitude * k * cos(phase));

        direction = vec2<f32>(
            direction.x * c - direction.y * s,
            direction.x * s + direction.y * c,
        );
        wavelength = wavelength * 0.53;
        // Slower than the wavelength shrinks, so the slope per layer falls off.
        amplitude = amplitude * 0.45;
    }
    return slope;
}

/// How lit this point is by the sun: 1 fully, 0 fully shadowed.
///
/// 3×3 PCF over the same map the mesh pass samples. A water surface is nearly
/// flat and nearly horizontal, so unlike `mesh.wgsl` there is no slope to scale
/// the bias against — but the ripple normals do tilt `n_dot_l` around, so the
/// bias is generous rather than tight: a shadow that leaks a centimetre onto
/// water is invisible, and acne on a mirror is not.
fn shadow_lit(world: vec3<f32>) -> f32 {
    let light_clip = frame.light_view_proj * vec4<f32>(world, 1.0);
    let projected = light_clip.xyz / light_clip.w;
    if projected.z > 1.0 || projected.z < 0.0 {
        return 1.0;
    }
    let inset = max(abs(projected.x), abs(projected.y));
    if inset > 1.0 {
        return 1.0;
    }

    let uv = vec2<f32>(projected.x * 0.5 + 0.5, 0.5 - projected.y * 0.5);
    let reference = projected.z - 0.0015;
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

/// Distance the view ray travels through the water before it hits whatever was
/// drawn behind the surface.
///
/// This is the field that makes water read as *deep* rather than as a coloured
/// pane: the same absorption curve run over it gives a clear shoreline, an
/// opaque middle, a waterline on anything standing in it, and — because it is
/// measured along the ray — more colour at grazing angles, which is what a
/// lake actually does.
///
/// Nothing behind the surface at all (the sky) returns a large number rather
/// than zero: water against the horizon is the deepest water in the frame.
fn water_thickness(clip: vec4<f32>, world: vec3<f32>) -> f32 {
    let coord = vec2<i32>(clip.xy);
    let size = vec2<i32>(textureDimensions(scene_depth));
    if coord.x < 0 || coord.y < 0 || coord.x >= size.x || coord.y >= size.y {
        return 1.0e4;
    }
    let raw = textureLoad(scene_depth, coord, 0).r;
    if raw >= 1.0 {
        return 1.0e4;
    }

    let uv = (vec2<f32>(coord) + vec2<f32>(0.5, 0.5)) / vec2<f32>(size);
    let ndc = vec3<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, raw);
    let behind = frame.inv_view_proj * vec4<f32>(ndc, 1.0);
    return length(behind.xyz / behind.w - world);
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    let shallow = surface.shallow_detail.rgb;
    let detail = surface.shallow_detail.w;
    let deep = surface.deep_fade.rgb;
    let depth_fade = max(surface.deep_fade.w, 1e-4);
    let foam_color = surface.foam.rgb;
    let shore_foam = surface.foam.w;
    let roughness = max(surface.params.x, 0.02);
    let opacity = surface.params.y;
    let crest_foam = surface.params.z;
    let detail_scale = surface.params.w;
    let time = surface.clock.y;

    let v = normalize(frame.camera_pos.xyz - in.world);

    // Double-sided: the pipeline does not cull, so a surface seen from
    // underneath gets the normal that faces the viewer. Without this, water
    // vanishes the moment a camera dips below it.
    //
    // The *upward* normal is kept as well, and the two are used for different
    // things: reflections and Fresnel belong to the side you are looking at,
    // but the sun lands on the top of the water whichever side you are on. A
    // body lit through the flipped normal is unlit from below, which renders a
    // sunlit lake as a black ceiling.
    let up_normal = normalize(in.normal);
    var n = up_normal;
    if dot(n, v) < 0.0 {
        n = -n;
    }
    // Ripples, in the surface's own tangent frame. Water is close enough to
    // horizontal that the world axes are a fine basis, and staying in world
    // space keeps the ripples continuous across two adjacent surfaces.
    //
    // Faded with distance, which is not a nicety: a half-metre ripple seen from
    // 80 m is far smaller than a pixel, so its normal varies wildly *within* one
    // pixel and the surface dissolves into white sparkle — the classic specular
    // aliasing failure, and the one thing that makes water read as broken
    // rather than as low quality. Fading the slopes is the cheap half of the
    // real fix (the other half is widening the roughness with distance, which
    // needs mip-mapped normals this shader does not have).
    let view_distance = length(frame.camera_pos.xyz - in.world);
    if detail > 0.0 {
        let fade = 1.0 / (1.0 + view_distance * 0.04);
        let slope = ripple_slope(in.world.xz, time, detail_scale) * (detail * fade);
        n = normalize(n + vec3<f32>(-slope.x, 0.0, -slope.y));
    }

    let l = normalize(-frame.sun_direction.xyz);
    let h = normalize(v + l);
    let n_dot_l = max(dot(n, l), 0.0);
    // How lit the water *body* is: the top face's angle to the sun, so the
    // colour of the water is the same whether you are above it or under it.
    let body_dot_l = max(dot(up_normal, l), 0.0);
    let n_dot_v = max(dot(n, v), 1e-4);
    let n_dot_h = max(dot(n, h), 0.0);
    let v_dot_h = max(dot(v, h), 0.0);

    var shade = 1.0;
    if frame.params.y > 0.5 {
        shade = shadow_lit(in.world);
    }

    // GGX for the sun's own reflection — the glitter path. Nothing is shared
    // with the mesh shader's copy on purpose (see the header).
    let alpha = roughness * roughness;
    let alpha2 = alpha * alpha;
    let d_denom = n_dot_h * n_dot_h * (alpha2 - 1.0) + 1.0;
    let d = alpha2 / (PI * d_denom * d_denom);
    let ggx_v = n_dot_l * sqrt(n_dot_v * n_dot_v * (1.0 - alpha2) + alpha2);
    let ggx_l = n_dot_v * sqrt(n_dot_l * n_dot_l * (1.0 - alpha2) + alpha2);
    let visibility = 0.5 / max(ggx_v + ggx_l, 1e-5);
    let fresnel_h = WATER_F0 + (1.0 - WATER_F0) * pow(1.0 - v_dot_h, 5.0);
    let sun_specular = d * visibility * fresnel_h * frame.sun_color.rgb * n_dot_l * shade;

    // The reflection, weighted by view-angle Fresnel: 2% straight down, most of
    // the sky at a grazing angle. This is the single strongest cue that a
    // surface is water, and it is why water in a scene with no sky looks like
    // dark plastic — there is nothing defensible to reflect, so the term is
    // gated exactly as the mesh pass gates its own.
    let view_fresnel = WATER_F0 + (1.0 - WATER_F0) * pow(1.0 - n_dot_v, 5.0);
    var reflection = vec3<f32>(0.0);
    var fill = frame.ambient.rgb;
    if frame.params.w > 0.5 {
        let up = n.y * 0.5 + 0.5;
        let hemisphere = mix(frame.sky_ground.rgb, frame.sky_zenith.rgb, up);
        let mean = max((frame.sky_ground.rgb + frame.sky_zenith.rgb) * 0.5, vec3<f32>(1e-4));
        fill = frame.ambient.rgb * (hemisphere / mean);

        let mirror = sky_gradient(
            reflect(-v, n),
            frame.sky_zenith.rgb,
            frame.sky_horizon.rgb,
            frame.sky_ground.rgb,
        );
        // Rough water gathers the sky over a cone, smooth water mirrors one
        // direction: the same roughness lerp the mesh pass uses, for the same
        // reason (it is the cheapest honest stand-in for a prefiltered
        // environment map).
        let sharpness = (1.0 - roughness) * (1.0 - roughness);
        reflection = mix(hemisphere, mirror, sharpness) * view_fresnel;
    }

    // Absorption along the view ray through the water body.
    let thickness = water_thickness(in.clip, in.world);
    let absorbed = 1.0 - exp(-thickness / depth_fade);
    let body = mix(shallow, deep, absorbed);
    let body_lit = body * (frame.sun_color.rgb * body_dot_l * shade + fill);
    let body_alpha = opacity * absorbed;

    // Premultiplied, like the mesh shader's transparent path: the reflection
    // and the sun highlight are light leaving the *surface*, so they must
    // survive being blended at a low alpha instead of being scaled down with
    // the water body behind them.
    let transmitted = 1.0 - view_fresnel;
    var color = reflection + sun_specular + body_lit * transmitted * body_alpha;
    var out_alpha = clamp(view_fresnel + transmitted * body_alpha, 0.0, 1.0);

    // Foam, from two independent signals: the surface folding at a crest, and
    // the surface meeting geometry. The second one is why a shoreline, an ice
    // block and a boat all get a waterline without being marked up — they are
    // all just something close behind the water.
    var foam_amount = 0.0;
    if crest_foam > 0.0 {
        // Tight thresholds: the Jacobian dips below 1 across most of a wave,
        // and foam belongs on the last stretch before a fold, not on the whole
        // windward face.
        foam_amount = crest_foam * smoothstep(0.5, 0.12, in.fold);
    }
    if shore_foam > 0.0 {
        let shore = 1.0 - smoothstep(0.0, shore_foam, thickness);
        // Squared: a linear ramp puts foam halfway across the shallows, and
        // real foam crowds the last few centimetres.
        foam_amount = max(foam_amount, shore * shore);
    }
    if foam_amount > 0.0 {
        let foam_lit = foam_color * (frame.sun_color.rgb * n_dot_l * shade + fill);
        color = mix(color, foam_lit, foam_amount);
        out_alpha = mix(out_alpha, 1.0, foam_amount);
    }

    // Fog last, so it takes the highlights with it, and against the
    // premultiplied convention — the same form as `mesh.wgsl`.
    if frame.params.x > 0.0 {
        let amount = clamp(1.0 - exp(-pow(view_distance * frame.params.x, 2.0)), 0.0, 1.0);
        color = mix(color, frame.sky_horizon.rgb * out_alpha, amount);
    }

    return vec4<f32>(clamp(color, vec3<f32>(0.0), vec3<f32>(1.0)), out_alpha);
}
