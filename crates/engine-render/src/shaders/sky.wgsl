// Procedural sky (M16): a three-band gradient with the sun in it.
//
// Drawn first, as a fullscreen triangle with depth writes off and the depth
// test always passing, so the meshes that follow overwrite it wherever they
// are. It replaces the flat clear color for scenes that opt in with
// `environment.sky`; scenes that do not never create this pipeline's pass and
// keep the clear color they always had.
//
// The gradient is evaluated per pixel from a world-space view ray rather than
// per vertex, because the horizon has to stay a horizon: interpolating three
// corners of a triangle across a wide field of view bends it visibly.

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

@group(0) @binding(0) var<uniform> frame: FrameUniform;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) ndc: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VsOut {
    // One oversized triangle covering the whole viewport — no vertex buffer,
    // and no seam down the middle the way two quad triangles have.
    var corners = array<vec2<f32>, 3>(
        vec2(-1.0, -1.0),
        vec2(3.0, -1.0),
        vec2(-1.0, 3.0),
    );
    let corner = corners[index];

    var out: VsOut;
    out.clip = vec4<f32>(corner, 1.0, 1.0);
    out.ndc = corner;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Unproject the far plane to get this pixel's world-space view ray.
    let far = frame.inv_view_proj * vec4<f32>(in.ndc, 1.0, 1.0);
    let direction = normalize(far.xyz / far.w - frame.camera_pos.xyz);

    // The gradient itself lives in `sky_common.wgsl`, prepended to this
    // source, because the mesh pass reflects the same sky off metal and water.
    var color = sky_gradient(
        direction,
        frame.sky_zenith.rgb,
        frame.sky_horizon.rgb,
        frame.sky_ground.rgb,
    );

    // The sun, where the directional light comes from. Two lobes: a wide
    // atmospheric glow and the disc itself. Both scale with the light's own
    // color and intensity, so an unlit scene's sky has no sun in it and a
    // sunset's is orange without anything else being said.
    let toward_sun = max(dot(direction, -normalize(frame.sun_direction.xyz)), 0.0);
    let glow = pow(toward_sun, 8.0) * 0.12;
    let disc = smoothstep(0.9995, 0.9999, toward_sun);
    color = color + frame.sun_color.rgb * (glow + disc * 6.0);

    return vec4<f32>(clamp(color, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
