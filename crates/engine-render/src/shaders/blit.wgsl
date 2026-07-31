// The opaque-colour copy, blitted back over the frame (M26).
//
// Needed on one path only: a scene that refracts with MSAA *off* has nowhere to
// resolve from, so the opaque pass draws straight into the copy and this puts it
// back where the blended pass expects to find it. With MSAA the copy is the
// opaque pass's resolve target and the multisampled attachment still holds
// everything, so this pass never runs.
//
// `textureLoad`, not a sampler: the copy is 1:1 with the target's pixels, and
// filtering a pixel against itself is a way to lose one.

// The frame-textures group, bound whole; this reads only the colour copy.
@group(0) @binding(3) var scene_color: texture_2d<f32>;

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    // One oversized triangle covering the viewport — the standard trick, and
    // the same one the sky and HUD passes use.
    let x = f32(i32(index) / 2) * 4.0 - 1.0;
    let y = f32(i32(index) & 1) * 4.0 - 1.0;
    return vec4<f32>(x, y, 0.0, 1.0);
}

@fragment
fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    return textureLoad(scene_color, vec2<i32>(position.xy), 0);
}
