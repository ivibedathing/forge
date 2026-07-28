// HUD overlay blit (M12): composite the CPU-rasterized overlay canvas over
// the lit scene. One fullscreen triangle, no vertex buffers, no sampler —
// `textureLoad` at the fragment's own pixel coordinate is an exact 1:1 fetch
// with no filtering to smear a glyph edge.
//
// The canvas covers only the pixels the HUD actually touches (M15), so the
// fetch is offset by that region's top-left corner and a scissor rect keeps
// the triangle from producing fragments outside it. Everything outside the
// region is transparent by construction, and blending a transparent texel is
// a no-op, so scissoring it away is bit-identical to compositing it.
//
// The canvas is sRGB-encoded straight-alpha; the texture is Rgba8UnormSrgb,
// so `textureLoad` hands us linear values and the pipeline's straight-alpha
// blend (src-alpha / one-minus-src-alpha) composites in linear space. An
// alpha-1 texel therefore lands byte-identical to the canvas byte, and an
// alpha-0 texel leaves the scene byte untouched — the bit-exactness the
// baselines rely on.

struct Overlay {
    // Where the canvas' (0, 0) texel sits in the render target. zw unused.
    origin: vec4<i32>,
};

@group(0) @binding(0) var overlay: texture_2d<f32>;
@group(0) @binding(1) var<uniform> placement: Overlay;

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    // (0,0) (2,0) (0,2) → clip (-1,-1) (3,-1) (-1,3): one CCW triangle
    // covering the screen.
    let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(corner * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    return textureLoad(overlay, vec2<i32>(position.xy) - placement.origin.xy, 0);
}
