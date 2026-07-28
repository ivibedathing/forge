// Particle billboards (M13): soft unlit discs, alpha-blended over the scene.
//
// One instance per particle; the six vertices of the quad are generated from
// vertex_index, expanded along the camera's right/up axes so every sprite
// faces the viewer. Colors are linear — the sRGB render target encodes on
// write, exactly like the mesh shader's output path.

struct ParticleFrame {
    view_proj: mat4x4<f32>,
    // xyz = normalized camera basis vectors; w unused.
    camera_right: vec4<f32>,
    camera_up: vec4<f32>,
};

@group(0) @binding(0) var<uniform> frame: ParticleFrame;

struct VsIn {
    @builtin(vertex_index) index: u32,
    // xyz = world position, w = billboard half-size in world units.
    @location(0) pos_size: vec4<f32>,
    // rgb = linear color, a = opacity.
    @location(1) color: vec4<f32>,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    // The corner in [-1, 1]² — the fragment stage's distance field.
    @location(0) corner: vec2<f32>,
    @location(1) color: vec4<f32>,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var corners = array<vec2<f32>, 6>(
        vec2(-1.0, -1.0), vec2(1.0, -1.0), vec2(1.0, 1.0),
        vec2(-1.0, -1.0), vec2(1.0, 1.0), vec2(-1.0, 1.0),
    );
    let corner = corners[in.index];
    let world = in.pos_size.xyz
        + (frame.camera_right.xyz * corner.x + frame.camera_up.xyz * corner.y) * in.pos_size.w;

    var out: VsOut;
    out.clip = frame.view_proj * vec4(world, 1.0);
    out.corner = corner;
    out.color = in.color;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Quadratic falloff from the center: a soft puff rather than a hard
    // circle, and exactly zero at the quad edge so sprites never show seams.
    let d = length(in.corner);
    let fade = clamp(1.0 - d, 0.0, 1.0);
    let alpha = in.color.a * fade * fade;
    // The corners outside the disc — over a fifth of every sprite — and any
    // particle faded to nothing contribute alpha 0, and `src * 0 + dst * 1`
    // leaves the destination byte for byte. Dropping those fragments is
    // therefore bit-identical to blending them, and smoke is drawn
    // back-to-front over large parts of the screen, so it is the cheapest
    // fill this pass can save.
    if alpha == 0.0 {
        discard;
    }
    return vec4(in.color.rgb, alpha);
}
