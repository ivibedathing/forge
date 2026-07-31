// Texture sampling (M26): the second producer at the surface-resolution seam,
// spliced into the mesh shader at pipeline build by `with_textures`.
//
// `mesh.wgsl` is not edited — see `terrain.wgsl` for the measurement that
// settled why, and `scene_renderer.rs`'s `with_surface` for the mechanism. A
// material with no maps never reaches this file at all: it draws through the
// pipeline that compiles `mesh.wgsl` as it sits on disk, rather than through
// this one with white textures bound. `x * 1.0` is exact in IEEE-754, so that
// is not the reason; the reason is that inserting the multiply changes the code
// *around* the four untouchable lines, and that is what decides whether the
// compiler fuses them.
//
// Group 3 is the material's, which is what the M26 bind-group merge freed up.
// Every slot is always bound, because WGSL binds unconditionally; a slot with
// no map gets a 1×1 white texture and is skipped by the `bound` bits anyway, so
// nothing depends on what is in it.

@group(3) @binding(0) var albedo_map: texture_2d<f32>;
@group(3) @binding(1) var orm_map: texture_2d<f32>;
@group(3) @binding(2) var normal_map: texture_2d<f32>;
@group(3) @binding(3) var emissive_map: texture_2d<f32>;
@group(3) @binding(4) var map_sampler: sampler;

const MAP_ALBEDO: u32 = 1u;
const MAP_ORM: u32 = 2u;
const MAP_NORMAL: u32 = 4u;
const MAP_EMISSIVE: u32 = 8u;

// What the maps say about this pixel, as multipliers on the material's own
// factors. Every field defaults to the identity, so an absent map is the
// material exactly.
struct SampledMaps {
    albedo: vec3<f32>,
    alpha: f32,
    roughness: f32,
    metallic: f32,
    occlusion: f32,
    emissive: vec3<f32>,
};

fn sample_maps(uv: vec2<f32>) -> SampledMaps {
    var out: SampledMaps;
    out.albedo = vec3<f32>(1.0);
    out.alpha = 1.0;
    out.roughness = 1.0;
    out.metallic = 1.0;
    out.occlusion = 1.0;
    out.emissive = vec3<f32>(1.0);

    let bound = u32(object.map_params.x);

    if (bound & MAP_ALBEDO) != 0u {
        // The sampler decodes: the slot uploaded this as an sRGB format, so
        // what arrives here is already linear reflectance like every other
        // colour in the engine.
        let texel = textureSample(albedo_map, map_sampler, uv);
        out.albedo = texel.rgb;
        out.alpha = texel.a;
    }
    if (bound & MAP_ORM) != 0u {
        // glTF's packing: occlusion in R, roughness in G, metallic in B.
        // Linear data, uploaded unencoded — read as sRGB this comes back
        // gamma-decoded and every surface is smoother than it was authored.
        let orm = textureSample(orm_map, map_sampler, uv).rgb;
        out.occlusion = orm.r;
        out.roughness = orm.g;
        out.metallic = orm.b;
    }
    if (bound & MAP_EMISSIVE) != 0u {
        out.emissive = textureSample(emissive_map, map_sampler, uv).rgb;
    }
    return out;
}

/// The shading normal, perturbed by the normal map.
///
/// The tangent frame is derived **per pixel from screen-space derivatives** of
/// position and UV rather than stored per vertex. That is the cheaper choice in
/// this codebase by a distance: it adds nothing to `MeshData`, so no `Arc`
/// changes identity and nothing re-uploads, and it works unmodified on `Water`,
/// `Terrain`, `Road`, `Tree` and `Cloud` geometry, every one of which is
/// generated and would otherwise need a tangent generator of its own. The cost
/// is a noisier frame on low-poly geometry and undefined behaviour on
/// degenerate UVs — acceptable in an engine whose sky is a gradient.
fn perturb_normal(n: vec3<f32>, world_position: vec3<f32>, uv: vec2<f32>) -> vec3<f32> {
    if (u32(object.map_params.x) & MAP_NORMAL) == 0u {
        return n;
    }

    let dp_dx = dpdx(world_position);
    let dp_dy = dpdy(world_position);
    let duv_dx = dpdx(uv);
    let duv_dy = dpdy(uv);

    // Solve for the tangent that carries +U across the surface. The
    // determinant vanishes on a degenerate UV patch — a mesh with no UVs at
    // all, which is every builtin — so fall back to the geometric normal
    // rather than to a NaN.
    let determinant = duv_dx.x * duv_dy.y - duv_dy.x * duv_dx.y;
    if abs(determinant) < 1e-12 {
        return n;
    }
    let tangent = (dp_dx * duv_dy.y - dp_dy * duv_dx.y) / determinant;
    let t = normalize(tangent - n * dot(n, tangent));
    if !all(t == t) {
        return n;
    }
    let b = cross(n, t);

    var tangent_normal = textureSample(normal_map, map_sampler, uv).xyz * 2.0 - 1.0;
    // `normal_strength` scales XY only: scaling Z as well would just rescale
    // the whole vector, which normalizes back to where it started.
    tangent_normal = vec3<f32>(
        tangent_normal.xy * object.map_params.z,
        max(tangent_normal.z, 1e-4),
    );
    return normalize(t * tangent_normal.x + b * tangent_normal.y + n * tangent_normal.z);
}
