// M0 proof-of-life shader: one hardcoded triangle, no vertex buffers.
//
// Positions come from the vertex index so there is nothing to bind — the point
// of M0 is to confirm the wgpu stack works end to end, not to exercise buffer
// plumbing. Real geometry arrives at M2/M3.
//
// Wound counter-clockwise in clip space, which is wgpu's default front face.
// Getting this right at M0 means backface culling is already proven when real
// meshes land, rather than being debugged for the first time against geometry
// that could be wrong for several other reasons.

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-0.8, -0.6),
        vec2<f32>( 0.8, -0.6),
        vec2<f32>( 0.0,  0.8),
    );
    return vec4<f32>(positions[index], 0.0, 1.0);
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    return vec4<f32>(0.9, 0.2, 0.2, 1.0);
}
