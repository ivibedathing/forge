// Water refraction (M27): what `refraction.wgsl` is to `mesh.wgsl`, for the
// water pipeline.
//
// Spliced into `water.wgsl` by `with_water_refraction` and compiled only into
// the refractive water pipeline, so a surface at the default `ior: 1.0` still
// takes the M18 shader as it sits on disk. That is M22's and M26's lesson
// rather than tidiness: splicing a feature inline into a shared shader moves
// pixels in scenes that do not use it, because whether the compiler contracts
// `a*b + c` into an FMA depends on the code around it.
//
// Not shared with `refraction.wgsl`, and the three differences are the reason:
//
// - It projects the exit point with `surface.view_proj`, out of `WaterUniform`.
//   A mesh's object uniform carries a premultiplied MVP and cannot supply one,
//   which is why the mesh variant has to append `view_proj` to `FrameUniform`;
//   water's carries world → clip already, because waves displace in world
//   space. So `FrameUniform` is untouched by this milestone.
// - The bend distance is *measured*, not authored. `water_thickness` has
//   returned the view ray's path length through the body since M18, so there
//   is no `thickness` field to set and a pond bends its bed most where it is
//   deepest.
// - It validates the sample against the depth copy. See `refracted_bed`.
//
// What survives the trip is *not* recomputed here: water grades `shallow_color`
// to `deep_color` against the same thickness and drives its opacity off that
// curve, so the amount of bed reaching the camera is `1 - out_alpha` — the
// number the blend unit was already using. Refraction moves where the bed is
// read from, not how much of it comes back.

@group(2) @binding(3) var scene_color: texture_2d<f32>;
@group(2) @binding(4) var scene_sampler: sampler;

/// The opaque frame behind this fragment, bent by the surface.
///
/// The exit point is the fragment's own position pushed along the refracted
/// direction by the measured thickness — the thin-surface approximation — and
/// projected with the same view-projection the surface was drawn with, so the
/// offset is the screen-space displacement of what is behind.
///
/// **The sample is rejected when it lands in front of the water.** A
/// screen-space offset can reach a pixel nearer than the refracting surface,
/// and when it does, whatever is standing in the water smears sideways into it.
/// The mesh path lives with this (`refraction.wgsl` names it), because the ice
/// it was built for is a block in mid-air. Water cannot: a pond is bounded by a
/// shoreline on every side, that shoreline is always in front of some part of
/// the surface, and it is exactly where the eye goes — `shore_foam` exists
/// because of it. The check costs one `textureLoad` from a copy water already
/// has bound, and it is per pixel, so a surface half of whose refracted samples
/// are valid keeps the half that are.
fn refracted_bed(
    clip: vec4<f32>,
    world: vec3<f32>,
    v: vec3<f32>,
    n: vec3<f32>,
    ior: f32,
    thickness: f32,
) -> vec3<f32> {
    let size = vec2<i32>(textureDimensions(scene_depth));
    let extent = vec2<f32>(size);

    // Where the blend unit would have read: this fragment's own pixel. The
    // fallback, and the same texel convention `water_thickness` uses.
    let own = clamp(vec2<i32>(clip.xy), vec2<i32>(0), size - vec2<i32>(1));
    let straight = (vec2<f32>(own) + vec2<f32>(0.5)) / extent;

    let direction = refract(-v, n, 1.0 / max(ior, 1e-3));
    // Total internal reflection returns the zero vector; there is nothing
    // behind the surface to sample along it, so read straight through.
    let bent = select(direction, -v, dot(direction, direction) < 1e-6);

    // **The exit point is solved to the bed's depth, not stepped along the
    // refracted ray by the view ray's path length.** `refraction.wgsl` does the
    // latter because a mesh has no idea how deep it is and its `thickness` is
    // authored. Water measures, and the two are not interchangeable: the
    // refracted ray is always *steeper* than the view ray, so travelling the
    // view ray's distance along it lands well below the bed. Measured on the
    // M27 fixture — a 1.5 m pool seen at 66° from the normal — that overshoots
    // by 1.18 m and displaces the sample 2.53 m sideways instead of 1.42 m,
    // which does not read as a bent pool bottom but as scrambled blocks.
    //
    // The view ray falls `thickness * v.y` to reach the bed, so the refracted
    // ray reaches that same depth after `drop / -bent.y`. Capped at `thickness`
    // because the refracted ray cannot be shallower than the view ray for any
    // `ior >= 1` — which also makes the whole expression continuous at 1.0,
    // where `refract` is the identity and the travel is exactly `thickness`.
    let drop = thickness * v.y;
    let descent = -bent.y;
    let solved = min(drop / max(descent, 1e-4), thickness);
    // Both guards are the camera at or below the waterline, where there is no
    // "down to the bed" to solve for and the plain step is what is left.
    let usable = v.y > 1e-4 && descent > 1e-4;
    let travel = select(thickness, solved, usable);
    let exit = world + bent * travel;

    let exit_clip = surface.view_proj * vec4<f32>(exit, 1.0);
    let ndc = exit_clip.xy / max(abs(exit_clip.w), 1e-4) * sign(exit_clip.w);
    // Clamped to the frame: a refraction that reads outside it has no data to
    // read, and a slightly wrong edge is a more honest failure than a black
    // smear.
    let offset = clamp(
        vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5),
        vec2<f32>(0.0),
        vec2<f32>(1.0),
    );

    // Depth is 0 at the near plane under this projection, so a smaller value
    // than the fragment's own is geometry in front of the water. Sky reads
    // 1.0 and is always behind.
    let probe = clamp(vec2<i32>(offset * extent), vec2<i32>(0), size - vec2<i32>(1));
    let behind = textureLoad(scene_depth, probe, 0).r;
    let uv = select(straight, offset, behind >= clip.z);

    return textureSampleLevel(scene_color, scene_sampler, uv, 0.0).rgb;
}
