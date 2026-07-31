// Refraction (M26): the third producer at the surface-resolution seam, and the
// only one that does not resolve a surface — it decides what is *behind* one.
//
// Spliced into the mesh shader by `with_refraction`, and used only by the
// blended pipelines: anything transmissive has already failed `blended == false`
// and left the default path through M16's combined `if`, so the bending and the
// absorption sit inside a branch the untouchable four lines never reach. That is
// a real dividend from M16's discipline — the feature most likely to disturb the
// shader is the one the existing branch structure already isolates.
//
// What is refracted is the **opaque** frame, copied where a pass that is not
// drawing into it can read it. Three limitations follow and are named here
// rather than discovered later: a transparent object cannot refract another
// transparent object behind it (M18's depth copy has exactly this limitation and
// it has not hurt); the offset is screen-space, so refraction through a strongly
// curved surface is an approximation and not a ray; and sorting stays per-object
// by origin distance, so two interpenetrating transmissive objects can still
// blend in the wrong order.

@group(2) @binding(3) var scene_color: texture_2d<f32>;
@group(2) @binding(4) var scene_sampler: sampler;

/// Where on the copied frame this fragment's refracted view ray comes out.
///
/// The exit point is the fragment's own position pushed along the refracted
/// direction by `thickness` — the thin-surface approximation — and then
/// projected with the same view-projection the geometry was drawn with, so the
/// offset is exactly the screen-space displacement of what is behind. Clamped
/// to the frame: a refraction that reads outside it has no data to read, and a
/// slightly wrong edge is a more honest failure than a black smear.
fn refracted_uv(world_position: vec3<f32>, v: vec3<f32>, n: vec3<f32>, ior: f32, thickness: f32) -> vec2<f32> {
    let direction = refract(-v, n, 1.0 / max(ior, 1e-3));
    // Total internal reflection returns the zero vector; there is nothing
    // behind the surface to sample along it, so read straight through.
    let bent = select(direction, -v, dot(direction, direction) < 1e-6);
    let exit = world_position + bent * thickness;

    let clip = frame.view_proj * vec4<f32>(exit, 1.0);
    let ndc = clip.xy / max(abs(clip.w), 1e-4) * sign(clip.w);
    return clamp(
        vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5),
        vec2<f32>(0.0),
        vec2<f32>(1.0),
    );
}

/// What survives the trip through the body, per linear-RGB channel.
///
/// Beer–Lambert on the authored `attenuation`, so a thick block of ice is
/// finally greener than a thin one — the gap `materials-lighting-design.md`
/// named when it said "a thick block of ice is exactly as clear as a thin one".
fn absorbed(colour: vec3<f32>, attenuation: vec3<f32>, thickness: f32) -> vec3<f32> {
    return colour * exp(-(vec3<f32>(1.0) - attenuation) * thickness);
}
