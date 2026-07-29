// Mesh shading for M4: simplified PBR. Extended in M16 with shadows, fog,
// sky-tinted ambient, and transparency.
//
// Lambert diffuse + GGX Cook-Torrance specular, one directional light, an
// ambient term, and unlit emissive. All math runs in linear space; the render
// target is an sRGB format, so the hardware performs the output encoding —
// there is deliberately no pow(1/2.2) approximation here.
//
// Conventions (documented in materials-lighting-design.md):
// - `sun_direction` is the direction the light TRAVELS; L = -sun_direction.
// - The Lambertian 1/pi is folded into the light (punctual-light convention):
//   a white light at intensity 1.0 hitting a white surface head-on reads
//   white. Predictability in a screenshot beats radiometric purity.
// - Colors are premultiplied by intensity on the CPU; nothing here branches
//   on "is there a light" — a lightless scene uploads the fallback rig.
//
// **Every M16 addition sits behind a branch that is off by default**, and on
// the default path the baseline expression is not merely equivalent but
// untouched. That is what let shadows, fog and blending land without moving a
// pixel in any scene that did not ask for them — see `EnvironmentSettings`.

struct ObjectUniform {
    mvp: mat4x4<f32>,
    model: mat4x4<f32>,
    // Inverse-transpose of the model matrix, so normals survive non-uniform
    // scale (the ground plane in the demo scene is scaled 10x on two axes).
    normal_matrix: mat4x4<f32>,
    // Scalars ride in the w lanes so the struct needs no padding fields.
    albedo_metallic: vec4<f32>,
    emissive_roughness: vec4<f32>,
    // x = alpha, y = transmission; z and w unused.
    surface: vec4<f32>,
};

struct FrameUniform {
    camera_pos: vec4<f32>,
    sun_direction: vec4<f32>,
    sun_color: vec4<f32>,
    ambient: vec4<f32>,
    // Clip → world, for the sky pass's per-pixel view ray.
    inv_view_proj: mat4x4<f32>,
    // World → the sun's orthographic clip space, for shadow lookups.
    light_view_proj: mat4x4<f32>,
    sky_zenith: vec4<f32>,
    sky_horizon: vec4<f32>,
    sky_ground: vec4<f32>,
    // x = fog density, y = shadows on, z = shadow-map texel size, w = sky on.
    params: vec4<f32>,
};

@group(0) @binding(0) var<uniform> object: ObjectUniform;
@group(1) @binding(0) var<uniform> frame: FrameUniform;
@group(2) @binding(0) var shadow_map: texture_depth_2d;
@group(2) @binding(1) var shadow_sampler: sampler_comparison;

struct VertexOut {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_position: vec3<f32>,
    @location(1) normal: vec3<f32>,
};

@vertex
fn vs_main(
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
) -> VertexOut {
    var out: VertexOut;
    out.clip_position = object.mvp * vec4<f32>(position, 1.0);
    out.world_position = (object.model * vec4<f32>(position, 1.0)).xyz;
    out.normal = (object.normal_matrix * vec4<f32>(normal, 0.0)).xyz;
    return out;
}

const PI: f32 = 3.14159265358979;

/// How lit this point is by the sun: 1 fully, 0 fully shadowed.
///
/// 3×3 PCF over a single orthographic map. Two things hold off the classic
/// artifacts: a slope-scaled depth bias (a surface nearly edge-on to the sun
/// spans many depths within one texel, and a flat bias either acnes it or
/// lifts the contact shadow off the ground), and a fade to fully lit at the
/// edge of the map, so its boundary is a gradient rather than a straight line
/// ruled across the world.
fn shadow_factor(world_position: vec3<f32>, n_dot_l: f32) -> f32 {
    let light_clip = frame.light_view_proj * vec4<f32>(world_position, 1.0);
    let projected = light_clip.xyz / light_clip.w;

    // Outside the light's depth range or its box: nothing was rendered there
    // to be occluded by, so treat it as lit.
    if projected.z > 1.0 || projected.z < 0.0 {
        return 1.0;
    }
    let inset = max(abs(projected.x), abs(projected.y));
    if inset > 1.0 {
        return 1.0;
    }

    // Clip xy is [-1, 1] with +Y up; texture uv is [0, 1] with +V down.
    let uv = vec2<f32>(projected.x * 0.5 + 0.5, 0.5 - projected.y * 0.5);

    let slope = sqrt(max(1.0 - n_dot_l * n_dot_l, 0.0)) / max(n_dot_l, 0.05);
    let bias = clamp(0.0006 * slope, 0.0004, 0.006);
    let reference = projected.z - bias;

    let texel = frame.params.z;
    var sum = 0.0;
    for (var y = -1; y <= 1; y = y + 1) {
        for (var x = -1; x <= 1; x = x + 1) {
            let offset = vec2<f32>(f32(x), f32(y)) * texel;
            // `...CompareLevel` rather than `...Compare`: this runs inside an
            // if, and the implicit-derivative form is only valid in uniform
            // control flow.
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

/// Ambient light modulated by the sky hemisphere.
///
/// Without this, every surface facing away from the sun is one dead flat
/// color, which is a large part of why untextured geometry reads as cheap:
/// real fill light arrives blue from the sky above and dark and warm from the
/// ground below, and that gradient across a curved surface is most of what
/// makes it look lit rather than painted.
///
/// The normalization is **per channel**, against the mean of the two bands,
/// so a surface facing the average of ground and zenith receives exactly the
/// authored `AmbientLight` — this is a *modulation* of the ambient color, not
/// a replacement for it. Normalizing against mean *luminance* instead is the
/// obvious alternative and is wrong in practice: a saturated blue sky then
/// multiplies the blue channel by three, every up-facing surface in the scene
/// turns blue-gray, and `AmbientLight.color` stops predicting anything.
fn sky_ambient(n: vec3<f32>) -> vec3<f32> {
    let up = n.y * 0.5 + 0.5;
    let env = mix(frame.sky_ground.rgb, frame.sky_zenith.rgb, up);
    let mean = max((frame.sky_ground.rgb + frame.sky_zenith.rgb) * 0.5, vec3<f32>(1e-4));
    return frame.ambient.rgb * (env / mean);
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    let albedo = object.albedo_metallic.rgb;
    let metallic = object.albedo_metallic.w;
    let emissive = object.emissive_roughness.rgb;
    // Floor keeps alpha^2 out of the denominator's danger zone: a scene that
    // writes roughness 0.0 gets a very tight highlight, not NaN.
    let roughness = max(object.emissive_roughness.w, 0.045);
    let surface_alpha = object.surface.x;
    let transmission = object.surface.y;

    let n = normalize(in.normal);
    let v = normalize(frame.camera_pos.xyz - in.world_position);
    let l = normalize(-frame.sun_direction.xyz);
    let h = normalize(v + l);

    let n_dot_l = max(dot(n, l), 0.0);
    let n_dot_v = max(dot(n, v), 1e-4);
    let n_dot_h = max(dot(n, h), 0.0);
    let v_dot_h = max(dot(v, h), 0.0);

    // GGX normal distribution; alpha = roughness^2 (perceptual convention).
    let alpha = roughness * roughness;
    let alpha2 = alpha * alpha;
    let d_denom = n_dot_h * n_dot_h * (alpha2 - 1.0) + 1.0;
    let d = alpha2 / (PI * d_denom * d_denom);

    // Smith height-correlated visibility (already includes the 1/(4 NdotL
    // NdotV) of the Cook-Torrance denominator).
    let ggx_v = n_dot_l * sqrt(n_dot_v * n_dot_v * (1.0 - alpha2) + alpha2);
    let ggx_l = n_dot_v * sqrt(n_dot_l * n_dot_l * (1.0 - alpha2) + alpha2);
    let visibility = 0.5 / max(ggx_v + ggx_l, 1e-5);

    // Schlick Fresnel; dielectrics reflect 4%, metals reflect tinted albedo.
    let f0 = mix(vec3<f32>(0.04), albedo, metallic);
    let fresnel = f0 + (vec3<f32>(1.0) - f0) * pow(1.0 - v_dot_h, 5.0);

    let specular = d * visibility * fresnel;
    let diffuse = albedo * (1.0 - metallic);

    // ── The M4 path, untouched ────────────────────────────────────────────
    //
    // These four lines are byte for byte what the engine shipped before any
    // of M16 existed, computed unconditionally from immutable bindings and
    // ahead of every branch below. That is stricter than it looks: an
    // *equivalent* expression is not enough, because whether the compiler may
    // contract `a * b + c` into an FMA depends on the code around it, and a
    // fused multiply-add carries more intermediate precision than the pair it
    // replaces. Restructuring these lines — even into arithmetic that is
    // equal on paper — moved a committed baseline by one unit in the last
    // place of one pixel. Leave them alone.
    let direct = (diffuse + specular) * frame.sun_color.rgb * n_dot_l;
    let ambient = albedo * frame.ambient.rgb;
    let base_color = direct + ambient + emissive;

    let shadowed = frame.params.y > 0.5;
    let lit_sky = frame.params.w > 0.5;
    let blended = surface_alpha < 1.0 || transmission > 0.0;

    var color = base_color;
    var out_alpha = 1.0;

    // Everything M16 added shares one branch, so a scene that opted into none
    // of it never leaves the path above.
    if shadowed || lit_sky || blended {
        var shade = 1.0;
        if shadowed {
            shade = shadow_factor(in.world_position, n_dot_l);
        }

        // Light that passed through the surface did not scatter back off it.
        let body = diffuse * (1.0 - transmission);

        // Reflected sky. Without it the only thing a surface can reflect is
        // the sun, which leaves polished metal and water looking like dark
        // plastic: a lake at a grazing angle is *mostly* reflected sky, and
        // it is the reflection, not the transparency, that makes water read
        // as water. Off unless the scene draws a sky, since with no sky there
        // is nothing defensible to reflect.
        //
        // Kept apart from the ambient term rather than folded into it because
        // a reflection happens *at* the surface: like the specular lobe it
        // must not be attenuated when the surface is blended at low alpha.
        var fill = ambient;
        var reflection = vec3<f32>(0.0);
        if lit_sky {
            let hemisphere = sky_ambient(n);
            fill = albedo * hemisphere;

            let mirror = sky_gradient(
                reflect(-v, n),
                frame.sky_zenith.rgb,
                frame.sky_horizon.rgb,
                frame.sky_ground.rgb,
            );
            // A rough surface gathers the sky over a wide cone, which
            // averages out to the hemispheric term; a smooth one mirrors a
            // single direction. Interpolating between the two by roughness is
            // the cheapest honest stand-in for a prefiltered environment map.
            let sharpness = (1.0 - roughness) * (1.0 - roughness);
            let environment = mix(hemisphere, mirror, sharpness);
            // Schlick on the view angle rather than the half vector — but
            // capped at `1 - roughness` instead of at 1 (the standard
            // roughness-aware form). Plain Schlick sends *every* surface to
            // mirror reflectance at grazing incidence, and a ground plane is
            // seen at grazing incidence nearly everywhere, so uncapped it
            // turns matte terrain into a sheet of sky. Only a smooth surface
            // keeps the full grazing rise, which is the one place it belongs.
            let ceiling = max(vec3<f32>(1.0 - roughness), f0);
            let view_fresnel = f0 + (ceiling - f0) * pow(1.0 - n_dot_v, 5.0);
            reflection = environment * view_fresnel;
        }

        let lit_diffuse = body * frame.sun_color.rgb * n_dot_l * shade;
        let lit_specular = specular * frame.sun_color.rgb * n_dot_l * shade;

        if blended {
            // View-angle Fresnel: a transmissive surface seen edge-on
            // reflects instead of transmitting, which is the entire reason
            // water reads as water. `alpha` on its own is deliberately flat
            // and view-independent.
            let view_fresnel = 0.04 + 0.96 * pow(1.0 - n_dot_v, 5.0);
            let clarity = transmission * (1.0 - view_fresnel);
            out_alpha = clamp(surface_alpha * (1.0 - clarity), 0.0, 1.0);

            // Premultiplied output: the reflected highlight has to survive
            // being blended at low alpha, or a clear surface loses its
            // specular exactly where the reflection should be strongest.
            color = (lit_diffuse + fill) * out_alpha + lit_specular + reflection + emissive;
        } else {
            color = lit_diffuse + lit_specular + fill + reflection + emissive;
        }
    }

    // Fog last, so it takes emissive and specular with it — a distant fire
    // should sink into the haze like everything else. Premultiplied color
    // fogs toward the fog color scaled by alpha, keeping the blend consistent.
    if frame.params.x > 0.0 {
        let distance = length(in.world_position - frame.camera_pos.xyz);
        let amount = clamp(1.0 - exp(-pow(distance * frame.params.x, 2.0)), 0.0, 1.0);
        color = mix(color, frame.sky_horizon.rgb * out_alpha, amount);
    }

    // Clamp, no tone mapping: deterministic, trivial to write pixel
    // assertions against, and blown highlights are a legible artifact.
    return vec4<f32>(clamp(color, vec3<f32>(0.0), vec3<f32>(1.0)), out_alpha);
}
