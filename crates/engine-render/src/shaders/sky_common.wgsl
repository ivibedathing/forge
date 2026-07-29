// The sky gradient, shared by the sky pass and the mesh pass (M16).
//
// WGSL has no `#include`, so this file is concatenated onto the front of both
// shader sources when their pipelines are built. It exists because the two
// passes *must* agree: the mesh shader reflects the sky off metal and water,
// and a reflection that does not match the sky above it is a worse artifact
// than no reflection at all. Sharing the source is the only way to keep them
// in step through a later change to the curve.
//
// It takes the three band colors as parameters rather than reading a uniform,
// so it makes no assumption about either pass's bindings.

fn sky_gradient(
    direction: vec3<f32>,
    zenith: vec3<f32>,
    horizon: vec3<f32>,
    ground: vec3<f32>,
) -> vec3<f32> {
    // Above the horizon the gradient runs horizon → zenith, below it runs
    // horizon → ground. Both are eased: a linear ramp in `direction.y` puts
    // its whole transition overhead and leaves a hard band at eye level.
    let height = direction.y;
    if height >= 0.0 {
        return mix(horizon, zenith, pow(height, 0.42));
    }
    return mix(horizon, ground, pow(-height, 0.55));
}
