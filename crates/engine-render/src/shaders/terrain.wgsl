// Terrain shading (M19): the generative material system, spliced into the mesh
// shader at pipeline build.
//
// **`mesh.wgsl` is not edited, and that is the whole point.** Terrain is an
// ordinary opaque lit surface — the same GGX lobe, the same PCF shadow lookup,
// the same hemispheric sky ambient, the same point lights, the same fog — and a
// second copy of two hundred lines of that, drifting apart from the first, is
// exactly what water and the point lights were careful to avoid duplicating for
// no reason. But M16 declares the four lines computing `direct`/`ambient`/
// `base_color` untouchable: they must reach the compiler in the code they
// already shipped in, because whether it may contract `a * b + c` into an FMA
// depends on the surrounding code, and an FMA carries more intermediate
// precision than the pair it replaces.
//
// This was not a theoretical worry. Putting the branch inline in `fs_main` —
// leaving those four lines textually identical, and only feeding them `albedo`
// and `roughness` from a function result instead of a uniform load — moved
// exactly one pixel by exactly one unit in each of `m16_environment`,
// `m17_fire` and `m18_water`. Found by the check this repo trusts: build the
// CLI at `main` and here, render every fixture with both, `cmp` the PNGs.
//
// So the terrain pipeline compiles a *variant* of the mesh shader, assembled by
// `with_terrain` in `scene_renderer.rs`: this file's declarations are inserted,
// and the fragment prologue is rewritten to resolve the surface through
// `terrain_surface`. The non-terrain pipeline still compiles `mesh.wgsl` exactly
// as it is on disk, so its output is byte-identical by construction rather than
// by measurement. The precedent is `sky_common.wgsl`, which is likewise
// concatenated rather than copied.
//
// Nothing on the CPU mirrors any of this. The collider is the displaced grid and
// nothing else, which is exactly what licenses per-pixel detail far finer than
// that grid.

const MAX_TERRAIN_LAYERS: u32 = 4u;

// One material a terrain paints itself with, claiming a band of world height
// and a band of slope.
struct TerrainLayer {
    // rgb = linear albedo, w = roughness.
    albedo_roughness: vec4<f32>,
    // x, y = world-Y band in metres; z, w = slope band in degrees.
    bands: vec4<f32>,
    // x = height fade in metres, y = boundary jitter, z = slope fade in
    // degrees; w unused.
    blend_noise: vec4<f32>,
};

// Hash of two lattice coordinates plus a salt. The same constants the height
// field uses in `engine-core/src/terrain.rs`, though the two fields are
// independent: relief is CPU and shared with physics, texture is per pixel and
// shared with nothing.
fn terrain_hash2(x: i32, y: i32, salt: u32) -> u32 {
    var h = u32(x) * 0x8DA6B343u ^ u32(y) * 0xD8163841u ^ salt * 0x16571FA5u;
    h = h ^ (h >> 15u);
    h = h * 0x2C1B3C6Du;
    h = h ^ (h >> 12u);
    h = h * 0x297A2D39u;
    h = h ^ (h >> 15u);
    return h;
}

// Smooth value noise in [-1, 1], smoothstepped so the field has no creases.
fn terrain_noise2(p: vec2<f32>, salt: u32) -> f32 {
    let base = floor(p);
    let frac = p - base;
    let ix = i32(base.x);
    let iy = i32(base.y);
    let w = frac * frac * (vec2<f32>(3.0) - 2.0 * frac);

    let c00 = f32(terrain_hash2(ix, iy, salt) >> 8u) / 8388608.0 - 1.0;
    let c10 = f32(terrain_hash2(ix + 1, iy, salt) >> 8u) / 8388608.0 - 1.0;
    let c01 = f32(terrain_hash2(ix, iy + 1, salt) >> 8u) / 8388608.0 - 1.0;
    let c11 = f32(terrain_hash2(ix + 1, iy + 1, salt) >> 8u) / 8388608.0 - 1.0;

    return mix(mix(c00, c10, w.x), mix(c01, c11, w.x), w.y);
}

// Two octaves, normalised to [-1, 1]. Two rather than four because this field
// is only ever read at pixel scale: a third octave lands below one pixel at any
// camera distance that matters and costs four more hashes to be invisible.
fn terrain_fbm(p: vec2<f32>, salt: u32) -> f32 {
    return (terrain_noise2(p, salt) + 0.5 * terrain_noise2(p * 2.0, salt + 1u)) / 1.5;
}

// How strongly a layer claims a value, given the band it wants and how far
// beyond each edge it fades.
//
// The fade is spent **outside** the band, not inside it: a layer covers the
// range it names at full strength and falls off beyond each edge, so a base
// coat written as `slope_range: [0, 90]` is genuinely full strength on flat
// ground and on a cliff. Fading inward instead would make every band weakest
// exactly where the author aimed it.
//
// `fade` is in the band's own units — metres for height, degrees for slope.
// A fraction of the band's width was the first attempt and is a trap: a wide
// band then gets a wide fade, and a layer aimed at "above 1.9 m" bleeds ten
// metres below itself and washes out everything beneath.
//
// `jitter` displaces the value being tested by a fraction of that fade, which
// is what breaks the boundary out of an iso-line — a clean sweeping curve, and
// the most artificial-looking thing this system can draw — and into
// interlocking fingers.
fn terrain_band(value: f32, low: f32, high: f32, fade: f32, jitter: f32) -> f32 {
    let width = max(fade, 1e-4);
    let v = value + jitter * width;
    return smoothstep(low - width, low, v) * (1.0 - smoothstep(high, high + width, v));
}

struct TerrainSurface {
    albedo: vec3<f32>,
    roughness: f32,
    normal: vec3<f32>,
};

// A terrain pixel's material and normal. Returns what it was handed when this
// draw is not terrain, which is every draw the engine made before M19.
fn terrain_surface(
    world_position: vec3<f32>,
    n: vec3<f32>,
    view_distance: f32,
    base_albedo: vec3<f32>,
    base_roughness: f32,
) -> TerrainSurface {
    var out: TerrainSurface;
    out.albedo = base_albedo;
    out.roughness = base_roughness;
    out.normal = n;

    let count = u32(object.terrain.x);
    if count == 0u {
        return out;
    }

    let scale = max(object.terrain.y, 1e-3);
    let seed = object.terrain_seed.x;
    let p = world_position.xz / scale;

    // Two scales of noise, an order of magnitude apart. The coarse one is
    // patch-sized — the drift in colour across a field that reads as ground
    // having a history — and the fine one is what the eye resolves standing on
    // it. They have to be far apart: a first attempt put them at 4× and the
    // result was one texture at one scale, which is the flat look this is
    // meant to cure.
    let coarse = terrain_fbm(p * 0.08, seed ^ 0x51ED27u);
    let fine = terrain_fbm(p, seed ^ 0x9E3779u);
    let mottle = coarse * 0.65 + fine * 0.35;

    // Slope in degrees from horizontal, the unit the file authors it in.
    let slope = degrees(acos(clamp(n.y, -1.0, 1.0)));
    let height = world_position.y;

    // Layer 0 is the base coat — it paints everywhere, and its own bands are
    // what the layers above it fade against. Each later layer paints *over*
    // what is beneath it wherever its bands say it does. Painting rather than
    // averaging is what lets a rock layer fully claim a cliff face instead of
    // settling for half of it.
    var albedo = object.terrain_layers[0].albedo_roughness.rgb;
    var roughness = object.terrain_layers[0].albedo_roughness.w;
    for (var i = 1u; i < MAX_TERRAIN_LAYERS; i = i + 1u) {
        if i >= count {
            break;
        }
        let layer = object.terrain_layers[i];
        let jitter = layer.blend_noise.y * mottle;
        let weight = terrain_band(height, layer.bands.x, layer.bands.y, layer.blend_noise.x, jitter)
            * terrain_band(slope, layer.bands.z, layer.bands.w, layer.blend_noise.z, jitter);
        albedo = mix(albedo, layer.albedo_roughness.rgb, weight);
        roughness = mix(roughness, layer.albedo_roughness.w, weight);
    }

    out.albedo = clamp(
        albedo * (1.0 + object.terrain.z * mottle),
        vec3<f32>(0.0),
        vec3<f32>(1.0),
    );
    out.roughness = roughness;

    // Bumpiness with no displacement behind it: the gradient of the fine noise,
    // tilting the normal only.
    //
    // It fades out with view distance, and that is not an optimisation — a
    // ripple far smaller than a pixel varies wildly inside that pixel, and the
    // surface dissolves into sparkle that reads as *broken* rather than as low
    // quality. Water's detail ripples paid for this lesson already.
    if object.terrain.w > 0.0 {
        let fade = 1.0 - smoothstep(scale * 10.0, scale * 40.0, view_distance);
        if fade > 0.0 {
            let salt = seed ^ 0x27D4EBu;
            let d = 0.5;
            let gx = terrain_fbm(p + vec2<f32>(d, 0.0), salt)
                - terrain_fbm(p - vec2<f32>(d, 0.0), salt);
            let gz = terrain_fbm(p + vec2<f32>(0.0, d), salt)
                - terrain_fbm(p - vec2<f32>(0.0, d), salt);
            out.normal = normalize(
                n + vec3<f32>(-gx, 0.0, -gz) * object.terrain.w * fade,
            );
        }
    }

    return out;
}

