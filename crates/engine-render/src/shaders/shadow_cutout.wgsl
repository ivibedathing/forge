// Shadow-map depth pass for alpha-cut materials (M26).
//
// `shadow.wgsl` deliberately has no fragment stage — the rasterizer writing
// depth is its entire output — and that is exactly why a cut-out leaf needs a
// second pipeline. A card that discards its transparent pixels in the mesh pass
// and not in the caster pass casts the silhouette of the *quad it was drawn
// on*, which is worse than casting nothing: the geometry says leaf and the
// shadow says rectangle.
//
// So this is `shadow.wgsl` plus the smallest fragment stage that can `discard`:
// one sample of the albedo map's alpha against `alpha_cutoff`. Used only by
// materials with `alpha_cutoff > 0`, which leaves the depth-only pipeline every
// current scene casts through completely untouched.
//
// The uniform is declared out to `map_uv` because field offsets are positional
// — the same reason `with_surface` splices one shared tail into every mesh
// variant. Everything past what this reads is left off the end, which is legal:
// a shader may declare a *prefix* of the buffer it is bound to.

struct TerrainLayer {
    albedo_roughness: vec4<f32>,
    bands: vec4<f32>,
    blend_noise: vec4<f32>,
};

struct ObjectUniform {
    mvp: mat4x4<f32>,
    model: mat4x4<f32>,
    normal_matrix: mat4x4<f32>,
    albedo_metallic: vec4<f32>,
    emissive_roughness: vec4<f32>,
    surface: vec4<f32>,
    terrain: vec4<f32>,
    terrain_seed: vec4<u32>,
    terrain_layers: array<TerrainLayer, 4>,
    // xy = uv scale, zw = uv offset.
    map_uv: vec4<f32>,
    // x = which maps are bound, y = alpha cutoff, z = normal strength, w = ior.
    map_params: vec4<f32>,
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
// The material group, bound at 2 rather than 3: this pipeline has no frame
// textures to read (it *is* the thing that writes one of them).
@group(2) @binding(0) var albedo_map: texture_2d<f32>;
@group(2) @binding(4) var map_sampler: sampler;

const MAP_ALBEDO: u32 = 1u;

struct VertexOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
) -> VertexOut {
    var out: VertexOut;
    out.clip = frame.light_view_proj * object.model * vec4<f32>(position, 1.0);
    out.uv = uv * object.map_uv.xy + object.map_uv.zw;
    return out;
}

@fragment
fn fs_main(in: VertexOut) {
    // No albedo map means no alpha to test, and this pipeline is only selected
    // for `alpha_cutoff > 0`; the guard is there so a material that sets a
    // cutoff and forgets the map casts its geometry rather than nothing.
    if (u32(object.map_params.x) & MAP_ALBEDO) == 0u {
        return;
    }
    if textureSample(albedo_map, map_sampler, in.uv).a < object.map_params.y {
        discard;
    }
}
