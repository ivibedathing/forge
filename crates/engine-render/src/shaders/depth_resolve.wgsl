// Depth resolve (M18): copy the opaque pass's depth into a texture something
// can read.
//
// The water pass needs the depth of whatever is behind the surface — that one
// number gives it absorption with depth, a shoreline, and a waterline on
// anything standing in the water. It cannot read the depth buffer directly:
// that buffer is bound as the pass's own depth attachment, and no API lets a
// pass sample the attachment it is testing against.
//
// So the frame becomes: opaque pass (stores depth) → this (depth → R32Float) →
// water and transparency (test against the depth buffer, sample the copy). One
// fullscreen triangle, `textureLoad` per pixel, no sampler and no filtering.
//
// `SOURCE_MULTISAMPLED` is patched in when the pipeline is built, because the
// binding type has to match the attachment's sample count and the renderer
// already knows it: with MSAA this reads **sample 0** rather than resolving.
// Absorption over metres does not care which sample of a pixel it measured
// from, and a real min-depth resolve would cost more than the whole pass.

@group(0) @binding(0) var source: SOURCE_TEXTURE_TYPE;

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    var corners = array<vec2<f32>, 3>(
        vec2(-1.0, -1.0),
        vec2(3.0, -1.0),
        vec2(-1.0, 3.0),
    );
    return vec4<f32>(corners[index], 0.0, 1.0);
}

@fragment
fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) f32 {
    // Both depth texture types return a plain f32 here, and both take the same
    // third argument — mip level 0 for the plain one, sample 0 for the
    // multisampled one — so one body serves both variants.
    return textureLoad(source, vec2<i32>(position.xy), 0);
}
