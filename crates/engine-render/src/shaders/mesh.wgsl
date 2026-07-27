// Mesh shading for M4: simplified PBR.
//
// Lambert diffuse + GGX Cook-Torrance specular, one directional light, a flat
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

struct ObjectUniform {
    mvp: mat4x4<f32>,
    model: mat4x4<f32>,
    // Inverse-transpose of the model matrix, so normals survive non-uniform
    // scale (the ground plane in the demo scene is scaled 10x on two axes).
    normal_matrix: mat4x4<f32>,
    // Scalars ride in the w lanes so the struct needs no padding fields.
    albedo_metallic: vec4<f32>,
    emissive_roughness: vec4<f32>,
};

struct FrameUniform {
    camera_pos: vec4<f32>,
    sun_direction: vec4<f32>,
    sun_color: vec4<f32>,
    ambient: vec4<f32>,
};

@group(0) @binding(0) var<uniform> object: ObjectUniform;
@group(1) @binding(0) var<uniform> frame: FrameUniform;

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

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    let albedo = object.albedo_metallic.rgb;
    let metallic = object.albedo_metallic.w;
    let emissive = object.emissive_roughness.rgb;
    // Floor keeps alpha^2 out of the denominator's danger zone: a scene that
    // writes roughness 0.0 gets a very tight highlight, not NaN.
    let roughness = max(object.emissive_roughness.w, 0.045);

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

    let direct = (diffuse + specular) * frame.sun_color.rgb * n_dot_l;
    let ambient = albedo * frame.ambient.rgb;

    // Clamp, no tone mapping: deterministic, trivial to write pixel
    // assertions against, and blown highlights are a legible artifact.
    let color = clamp(direct + ambient + emissive, vec3<f32>(0.0), vec3<f32>(1.0));
    return vec4<f32>(color, 1.0);
}
