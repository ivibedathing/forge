// Mesh shading for M2.
//
// The lighting here is a single hardcoded directional term, not a material
// model — M4 replaces it with real lights and PBR. It exists because a flat
// unlit cube renders as a featureless hexagon, which tells an agent looking at
// a screenshot nothing about whether its transform was right.

struct Uniforms {
    mvp: mat4x4<f32>,
    // Inverse-transpose of the model matrix, so normals survive non-uniform
    // scale (the ground plane in the demo scene is scaled 8x on two axes).
    normal_matrix: mat4x4<f32>,
    albedo: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;

struct VertexOut {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) normal: vec3<f32>,
};

@vertex
fn vs_main(
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
) -> VertexOut {
    var out: VertexOut;
    out.clip_position = u.mvp * vec4<f32>(position, 1.0);
    out.normal = (u.normal_matrix * vec4<f32>(normal, 0.0)).xyz;
    return out;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    let n = normalize(in.normal);
    let light_direction = normalize(vec3<f32>(0.4, 1.0, 0.6));

    // Half-Lambert-ish: remap N·L into 0.25..1 so faces turned away from the
    // light stay legible instead of going black. Placeholder until M4.
    let diffuse = max(dot(n, light_direction), 0.0) * 0.75 + 0.25;

    return vec4<f32>(u.albedo.rgb * diffuse, 1.0);
}
