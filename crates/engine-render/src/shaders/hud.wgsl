// HUD overlay blit (M12): composite the CPU-rasterized overlay canvas over
// the lit scene. One fullscreen triangle, no vertex buffers, no sampler —
// the canvas is target-sized, so `textureLoad` at the fragment's own pixel
// coordinate is an exact 1:1 fetch with no filtering to smear a glyph edge.
//
// The canvas is sRGB-encoded straight-alpha; the texture is Rgba8UnormSrgb,
// so `textureLoad` hands us linear values and the pipeline's straight-alpha
// blend (src-alpha / one-minus-src-alpha) composites in linear space. An
// alpha-1 texel therefore lands byte-identical to the canvas byte, and an
// alpha-0 texel leaves the scene byte untouched — the bit-exactness the
// baselines rely on.

@group(0) @binding(0) var overlay: texture_2d<f32>;

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    // (0,0) (2,0) (0,2) → clip (-1,-1) (3,-1) (-1,3): one CCW triangle
    // covering the screen.
    let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(corner * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    return textureLoad(overlay, vec2<i32>(position.xy), 0);
}
