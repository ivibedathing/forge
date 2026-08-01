// Global illumination: the baked irradiance field, sampled (M35).
//
// Four SH-L1 coefficient planes as `Rgba16Float` 3D textures. `Rgba16Float`
// because it is filterable in core WebGPU, and hardware trilinear filtering is
// the entire reason to store the field in a texture rather than a buffer: probe
// interpolation is then free and continuous, and there is no seam between one
// probe cell and the next.
//
// Everything constant over the volume — the authored `AmbientLight`, the two
// live sky bands, and `sky_ambient`'s per-channel normalization — was already
// folded in on the CPU by `gi::evaluate`. What is left here is a fetch and four
// multiply-adds.
//
// The bindings continue group 2, beside the shadow map, the depth copy and the
// colour copy, which is where frame-scoped textures have lived since M26.
//
// `gi_sampler` is bound to the *same* wgpu sampler object as `scene_sampler` —
// linear and clamped on every axis, which is exactly what a probe fetch wants —
// but it needs its own binding rather than sharing binding 4. WGSL rejects two
// module-scope variables at one binding, and `scene_sampler` is declared by
// `refraction.wgsl`, which a GI variant may or may not be composed with. A
// second binding costs nothing; a declaration whose legality depends on which
// other producer is in the list is the kind of coupling this seam exists to
// avoid.

@group(2) @binding(5) var gi_sh0: texture_3d<f32>;
@group(2) @binding(6) var gi_sh1: texture_3d<f32>;
@group(2) @binding(7) var gi_sh2: texture_3d<f32>;
@group(2) @binding(8) var gi_sh3: texture_3d<f32>;
@group(2) @binding(9) var gi_sampler: sampler;

// The gain on the three linear coefficients. Derived, not tuned: it is the
// value that makes an unoccluded probe reconstruct `sky_ambient(n)` exactly.
// `gi::evaluate::LINEAR_GAIN` carries the derivation, and
// `an_unoccluded_probe_reproduces_sky_ambient` is where it is checked.
const GI_LINEAR_GAIN: f32 = 3.0;

// World position → texture coordinate.
//
// Probe [0,0,0] sits at `gi_origin.xyz` and probes are `gi_origin.w` metres
// apart, so probe `i` is at texel centre `(i + 0.5) / count`. Getting this half
// texel wrong shifts the whole field by half a probe, which reads as GI leaking
// through a wall rather than as an offset.
fn gi_texcoord(world_position: vec3<f32>) -> vec3<f32> {
    let cell = (world_position - frame.gi_origin.xyz) / max(frame.gi_origin.w, 1e-6);
    return (cell + vec3<f32>(0.5)) / max(frame.gi_grid.xyz, vec3<f32>(1.0));
}

// How strongly GI applies here: `intensity`, faded to zero over `blend` metres
// at the boundary and zero outside the volume entirely.
//
// A fade rather than a step. It costs nothing in an open scene — an unoccluded
// probe reconstructs the same value the fallback would have given, so the mix
// between them is invisible where there is nothing to occlude. The fade only
// ever shows where GI is doing something.
fn gi_weight(world_position: vec3<f32>) -> f32 {
    if frame.gi_params.y < 0.5 {
        return 0.0;
    }
    let last = max(frame.gi_grid.xyz - vec3<f32>(1.0), vec3<f32>(0.0)) * frame.gi_origin.w;
    let low = world_position - frame.gi_origin.xyz;
    let high = frame.gi_origin.xyz + last - world_position;
    let inside = min(low, high);
    let distance = min(inside.x, min(inside.y, inside.z));
    if distance <= 0.0 {
        return 0.0;
    }
    if frame.gi_params.x <= 0.0 {
        return frame.gi_grid.w;
    }
    return frame.gi_grid.w * clamp(distance / frame.gi_params.x, 0.0, 1.0);
}

// The field reconstructed for a normal.
fn gi_irradiance(world_position: vec3<f32>, n: vec3<f32>) -> vec3<f32> {
    let uvw = gi_texcoord(world_position);
    let c0 = textureSampleLevel(gi_sh0, gi_sampler, uvw, 0.0);
    let c1 = textureSampleLevel(gi_sh1, gi_sampler, uvw, 0.0);
    let c2 = textureSampleLevel(gi_sh2, gi_sampler, uvw, 0.0);
    let c3 = textureSampleLevel(gi_sh3, gi_sampler, uvw, 0.0);
    return c0.rgb + GI_LINEAR_GAIN * (c1.rgb * n.x + c2.rgb * n.y + c3.rgb * n.z);
}

// How much of the sky this point can see, 1 in the open and 0 buried. Free
// beside the fetch above, and what a `Meadow`'s roots will want in G3.
fn gi_openness(world_position: vec3<f32>) -> f32 {
    let uvw = gi_texcoord(world_position);
    return textureSampleLevel(gi_sh0, gi_sampler, uvw, 0.0).a;
}

// What the two fill lines read instead of their constant.
//
// `fallback` is the expression the pre-M35 engine used — `frame.ambient.rgb` on
// the no-sky path, `sky_ambient(n)` on the sky path — so a weight of zero lands
// on it exactly. That is what makes `intensity: 0.0` a one-field A/B against the
// pre-M35 look, and what makes a fragment outside every volume render as it
// always did.
fn gi_fill(world_position: vec3<f32>, n: vec3<f32>, fallback: vec3<f32>) -> vec3<f32> {
    let w = gi_weight(world_position);
    if w <= 0.0 {
        return fallback;
    }
    return mix(fallback, gi_irradiance(world_position, n), w);
}
