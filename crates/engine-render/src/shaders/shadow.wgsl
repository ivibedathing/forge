// Shadow-map depth pass (M16).
//
// Vertex only: the pass has no color attachment, so the depth the rasterizer
// writes is the entire output. It reuses the mesh pass's object and frame
// uniforms unchanged — the model matrix is already there and the sun's
// view-projection rides in the frame — which is why casting shadows needs no
// second upload of anything.
//
// Front faces are culled here rather than back faces (see the pipeline): what
// the map should record is the far side of each caster, which pushes the
// depth comparison away from the lit surface and keeps shadow acne off the
// faces the camera can actually see.

struct ObjectUniform {
    mvp: mat4x4<f32>,
    model: mat4x4<f32>,
    normal_matrix: mat4x4<f32>,
    albedo_metallic: vec4<f32>,
    emissive_roughness: vec4<f32>,
    surface: vec4<f32>,
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
    params: vec4<f32>,
};

@group(0) @binding(0) var<uniform> object: ObjectUniform;
@group(1) @binding(0) var<uniform> frame: FrameUniform;

@vertex
fn vs_main(@location(0) position: vec3<f32>) -> @builtin(position) vec4<f32> {
    return frame.light_view_proj * object.model * vec4<f32>(position, 1.0);
}
