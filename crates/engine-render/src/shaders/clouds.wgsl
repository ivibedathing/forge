// Clouds (M20): lobe geometry in, three cheap stand-ins for multiple
// scattering out.
//
// A separate pipeline from `mesh.wgsl` rather than another `Material` branch,
// following `water.wgsl` and for the same two reasons. A cloud needs shading no
// surface does — wrapped diffuse, a forward-scattering lobe, an alpha that
// falls off toward the silhouette — and `mesh.wgsl` is the file the repo
// declares untouchable, because whether the compiler contracts its four M4
// lines into FMAs depends on the code around them. A new file cannot move a
// pixel in a scene that has no clouds in it.
//
// Deliberate duplications, and why they are not shared:
//
// - `FrameUniform` is declared here as a *prefix* of the Rust struct, exactly as
//   `sky.wgsl` and `water.wgsl` already do. WGSL has no `#include`, the layout's
//   authority is the Rust `FrameUniform`, and a shared declaration would have to
//   be prepended onto `mesh.wgsl` — see above. The point lights at the end of
//   that struct are one of the things a cloud does not read.
// - The fog term is a near-copy of `mesh.wgsl`'s, for the same reason.
//
// The sky gradient is the exception: `sky_common.wgsl` is prepended to this
// source, because a cloud's underside is lit by the sky and that has to be the
// same sky drawn behind it.
//
// What is *not* here: shadows (the engine has one cascade and it is fitted to
// the camera, not to a cloud at altitude), point lights, and any notion of
// volume. Overlapping lobes do not write depth, so their alpha accumulates —
// which is a poor man's optical depth, and roughly the right poor man's.

struct CloudUniform {
    // World → clip. Not an MVP: `drift` displaces in world space, so the model
    // transform is applied first and separately.
    view_proj: mat4x4<f32>,
    model: mat4x4<f32>,
    // Inverse-transpose of `model`. Non-uniform scale is the normal case for a
    // cloud — `[24, 12, 24]` is what makes a cumulus wider than it is tall —
    // so the normals have to survive it.
    normal_matrix: mat4x4<f32>,
    // xyz = sunlit colour, w = density.
    color_density: vec4<f32>,
    // xyz = self-shadowed colour, w = feather exponent.
    shade_feather: vec4<f32>,
    // xyz = drift in metres per second, w = wrap distance (0 = never wrap).
    drift_wrap: vec4<f32>,
    // x = scene time in seconds; y, z, w unused.
    params: vec4<f32>,
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

@group(0) @binding(0) var<uniform> cloud: CloudUniform;
@group(1) @binding(0) var<uniform> frame: FrameUniform;

/// How much brighter the silver lining gets over the cloud's own tone. Tuned
/// against a backlit cumulus: high enough to read as a rim, low enough that a
/// cloud between the camera and the sun does not turn into a light source.
const SILVER: f32 = 0.55;
/// How tightly the silver lining hugs the direction of the sun.
const SILVER_FOCUS: f32 = 8.0;
/// Fraction of the sunlight that reaches the shadowed side of a cloud by
/// scattering through it rather than by arriving directly.
const THROUGH_SCATTER: f32 = 0.3;

struct VertexOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) normal: vec3<f32>,
};

@vertex
fn vs_main(
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
) -> VertexOut {
    // Drift is a rigid translation evaluated from the scene clock, so the mesh
    // itself never changes and its `Arc` identity — which the renderer's upload
    // cache keys on — survives every frame. A cloud that *evolved* would be a
    // new mesh per frame, which is the same trade `tree-design.md` refuses for
    // wind.
    var offset = cloud.drift_wrap.xyz * cloud.params.x;
    let wrap = cloud.drift_wrap.w;
    if wrap > 0.0 {
        // Into [-wrap/2, wrap/2). The offset is uniform over the whole cloud,
        // so wrapping here cannot tear it — it teleports as one piece.
        offset = offset - floor(offset / wrap + vec3<f32>(0.5)) * wrap;
    }

    var out: VertexOut;
    let world = (cloud.model * vec4<f32>(position, 1.0)).xyz + offset;
    out.clip = cloud.view_proj * vec4<f32>(world, 1.0);
    out.world = world;
    out.normal = (cloud.normal_matrix * vec4<f32>(normal, 0.0)).xyz;
    return out;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    let lit_color = cloud.color_density.rgb;
    let density = cloud.color_density.w;
    let shade_color = cloud.shade_feather.rgb;
    let feather = cloud.shade_feather.w;

    let n = normalize(in.normal);
    let v = normalize(frame.camera_pos.xyz - in.world);
    let l = normalize(-frame.sun_direction.xyz);

    // Wrapped diffuse: `dot(n,l)·0.5 + 0.5` rather than `max(dot(n,l), 0)`.
    // Light entering a cloud scatters many times before it leaves, so the lit
    // side is nearly flat and the shadowed side is bright and blue — a Lambert
    // term instead puts a hard terminator across it and turns half of every
    // cloud black, which is the single loudest way a blob fails to read as one.
    //
    // The normal is *not* flipped toward the viewer the way `water.wgsl` flips
    // its own. Culling is off here, so the far wall of each lobe is drawn too,
    // and it should shade as the far wall: seen through the near one, away from
    // the sun, darker. Flipping would light both walls identically and flatten
    // the lobe.
    // Left linear rather than raised to a power. Sharpening this curve is the
    // obvious way to get more contrast out of a cloud that renders too flat,
    // and it is the wrong knob: squaring it drives the shadowed side toward
    // rock, because the same term also scales how much sunlight reaches there.
    let scatter = dot(n, l) * 0.5 + 0.5;
    let tone = mix(shade_color, lit_color, scatter);

    // Ambient. With a sky the fill is the hemisphere the normal faces, which is
    // what puts the ground's colour under a cloud and the zenith's on top of
    // it; normalized per channel against the two bands' mean, exactly as
    // `mesh.wgsl` and `water.wgsl` do it, so `AmbientLight` keeps meaning what
    // it says and only the *balance* tracks the normal.
    var fill = frame.ambient.rgb;
    if frame.params.w > 0.5 {
        let up = n.y * 0.5 + 0.5;
        let hemisphere = mix(frame.sky_ground.rgb, frame.sky_zenith.rgb, up);
        let mean = max((frame.sky_ground.rgb + frame.sky_zenith.rgb) * 0.5, vec3<f32>(1e-4));
        fill = frame.ambient.rgb * (hemisphere / mean);
    }
    // The sun does not reach the shadowed side — that is what shadowed means —
    // but a cloud is not opaque either, and some fraction of the sunlight
    // arrives there having scattered through the body. Without that term the
    // underside of every cloud goes to whatever the sky alone can light, which
    // is far darker than any real cloud; with the sun applied in full instead,
    // an author's `intensity: 2.4` saturates a white cloud everywhere and the
    // shading disappears entirely. Both were rendered; this is between them.
    let reaching = mix(THROUGH_SCATTER, 1.0, scatter);
    var color = tone * (frame.sun_color.rgb * reaching + fill);

    // How square-on this pixel's surface is to the camera. Both the edge fade
    // and the silver lining hang off it, and they are the same observation from
    // two sides: a cloud is thin where you see it edge-on.
    let facing = abs(dot(n, v));

    // Forward scattering — the silver lining. Looking toward the sun means the
    // view direction (`-v`) points along `l`. Concentrated at the edges, where
    // the cloud is thin enough for sunlight to make it through, which is why a
    // backlit cloud is the most legible cloud there is.
    let toward_sun = max(dot(-v, l), 0.0);
    color = color + frame.sun_color.rgb * (pow(toward_sun, SILVER_FOCUS) * (1.0 - facing) * SILVER);

    // The silhouette of a cloud is where it thins out, not where its geometry
    // stops. This fade is doing two jobs: it softens the outer edge, and it
    // hides the boundary *between* two interpenetrating lobes, since each of
    // them vanishes exactly where its own surface turns away from the camera.
    // Without it a cluster of lobes reads as a bag of marbles.
    //
    // The curve is `1 - (1 - facing)^feather`, not `facing^feather`, and the
    // difference is the whole look. A plain power fades *proportionally*: a
    // surface tilted 60° from the camera is already two thirds transparent, so
    // a cloud seen from below — where every underside is tilted — goes
    // translucent all over and its own interior lobes show through it as pale
    // outlines. This curve keeps the body opaque and spends its whole range in
    // the last few degrees before the silhouette, which is where a real cloud
    // actually thins out.
    let alpha = clamp(density * (1.0 - pow(1.0 - facing, feather)), 0.0, 1.0);

    // Fog last, so it takes the silver lining with it, and against the
    // premultiplied convention — the same form as `mesh.wgsl` and `water.wgsl`.
    // A cloud at 400 m fading into the horizon colour is aerial perspective,
    // and it is free.
    if frame.params.x > 0.0 {
        let distance = length(frame.camera_pos.xyz - in.world);
        let amount = clamp(1.0 - exp(-pow(distance * frame.params.x, 2.0)), 0.0, 1.0);
        color = mix(color, frame.sky_horizon.rgb, amount);
    }

    // Premultiplied, like every other blended path in the engine.
    return vec4<f32>(clamp(color, vec3<f32>(0.0), vec3<f32>(1.0)) * alpha, alpha);
}
