// HUD overlay: one textured quad, alpha-blended over the finished frame.
// `rect` is (left, top, width, height) in NDC; NDC y points up, so the quad
// grows downward from `top`.

struct HudRect {
    rect: vec4<f32>,
}

@group(0) @binding(0) var panel: texture_2d<f32>;
@group(0) @binding(1) var panel_sampler: sampler;
@group(0) @binding(2) var<uniform> hud: HudRect;

struct VsOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VsOut {
    // Two triangles over the unit square, wound counter-clockwise in NDC.
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 1.0),
    );
    let corner = corners[index];
    var out: VsOut;
    out.position = vec4<f32>(
        hud.rect.x + corner.x * hud.rect.z,
        hud.rect.y - corner.y * hud.rect.w,
        0.0,
        1.0,
    );
    out.uv = corner;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return textureSample(panel, panel_sampler, in.uv);
}
