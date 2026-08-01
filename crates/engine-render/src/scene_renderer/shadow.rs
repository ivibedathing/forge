use super::*;

/// Resolution of the directional shadow map, in texels on a side.
///
/// Fixed rather than authored: `EnvironmentSettings::shadow_distance` already
/// gives the scene the sharpness knob that matters (it sets how much world
/// these texels are spread over), and a second one would only let a scene ask
/// for 8192² and blame the engine for the memory.
pub(crate) const SHADOW_MAP_SIZE: u32 = 2048;

/// The shadow map.
///
/// A 1×1 placeholder stands in when a scene does not cast shadows: WGSL binds
/// the texture unconditionally, but the sampling is behind `params.y`, so
/// nothing ever reads the placeholder's undefined contents. That keeps one
/// mesh pipeline for both cases instead of two shader permutations.
pub(crate) struct ShadowMap {
    pub(crate) view: wgpu::TextureView,
}

impl ShadowMap {
    pub(crate) fn new(device: &wgpu::Device, size: u32) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("shadow-map"),
            size: wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Self { view }
    }
}

/// The world-space direction the camera looks, recovered from its
/// view-projection.
///
/// Taken from the matrix rather than from `ScenePass::camera_right`/`_up`
/// because those are documented as meaningful only when there are particles,
/// and the shadow box has to be fitted for every scene that casts.
pub(crate) fn camera_forward(view_projection: Mat4) -> Vec3 {
    let inverse = view_projection.inverse();
    let unproject = |z: f32| {
        let p = inverse * glam::Vec4::new(0.0, 0.0, z, 1.0);
        p.truncate() / p.w
    };
    (unproject(1.0) - unproject(0.0)).normalize_or_zero()
}

/// How close to the horizon the *shadow* direction is allowed to get, in
/// degrees. Not a scene field, and not applied to the lighting direction.
///
/// See [`clamp_shadow_elevation`].
pub(crate) const MIN_SHADOW_ELEVATION_DEGREES: f32 = 5.0;

/// Push a light direction down to at least [`MIN_SHADOW_ELEVATION_DEGREES`]
/// below horizontal, for shadow-map fitting only.
///
/// A sun on the horizon casts shadows of unbounded length, and one a hair
/// below it casts them *upward* — the ground shadowing itself from beneath.
/// Neither is a problem the ortho fit or a depth bias can solve, because
/// neither is a precision failure: the geometry really is that shape. M21's
/// day/night system reaches those angles twice a day, where before M21 no
/// scene ever did (every shadow-casting fixture in the repo aims its sun
/// 24°–33° up).
///
/// So the shadow direction stops descending near the horizon while the
/// direction that *lights* the scene keeps going. It is a lie, and it is told
/// at the moment when direct light is nearly gone and the shadows it would
/// have cast are far too long and faint to read. Doing it here rather than in
/// the scene format keeps it out of the file: an author should not have to
/// know the renderer has a floor.
///
/// Above the floor this returns its input unchanged, which is why it costs
/// every pre-M21 baseline nothing.
pub(crate) fn clamp_shadow_elevation(travel: Vec3) -> Vec3 {
    // `travel` points the way the light goes, so a descending sun has
    // negative Y and "elevation" is `-travel.y`.
    let floor = MIN_SHADOW_ELEVATION_DEGREES.to_radians().sin();
    if travel.y <= -floor {
        return travel;
    }

    let horizontal = Vec3::new(travel.x, 0.0, travel.z);
    let Some(bearing) = horizontal.try_normalize() else {
        // Straight up or straight down: there is no bearing to preserve, and
        // straight down is already past the floor.
        return Vec3::NEG_Y;
    };

    let elevation = MIN_SHADOW_ELEVATION_DEGREES.to_radians();
    (bearing * elevation.cos() - Vec3::Y * elevation.sin()).normalize()
}

/// Fit the sun's orthographic frustum around the part of the world the camera
/// can see, and return world → light clip.
///
/// The box is a `shadow_distance`-long slab starting at the camera and
/// following its view direction, which is the cheapest thing that keeps the
/// texels where the viewer is looking. Two details are load-bearing:
///
/// - **The center is snapped to whole texels.** Without it, moving the camera
///   slides the shadow map's sampling grid continuously across the world and
///   every shadow edge crawls and fizzes — the artifact reads as a rendering
///   bug rather than as low resolution, and it is far more visible in motion
///   than the resolution itself.
/// - **The eye is pulled well back** along the light, so that casters above
///   the slab (the showcase tour's monolith, a truck on a rise) are inside the
///   depth range and can shadow the ground they should.
pub(crate) fn light_view_projection(
    sun_direction: Vec3,
    camera_position: Vec3,
    view_projection: Mat4,
    shadow_distance: f32,
    map_size: u32,
) -> Mat4 {
    let radius = (shadow_distance * 0.5).max(0.5);
    let center = camera_position + camera_forward(view_projection) * radius;

    let travel = if sun_direction.length_squared() > 1e-12 {
        clamp_shadow_elevation(sun_direction.normalize())
    } else {
        Vec3::NEG_Y
    };
    // `up` only has to be non-parallel to the light; a sun directly overhead
    // would make the usual +Y degenerate.
    let up = if travel.y.abs() > 0.99 {
        Vec3::Z
    } else {
        Vec3::Y
    };

    let orientation = glam::camera::rh::view::look_to_mat4(Vec3::ZERO, travel, up);
    let texel = 2.0 * radius / map_size as f32;
    let in_light_space = orientation.transform_point3(center);
    let snapped = Vec3::new(
        (in_light_space.x / texel).round() * texel,
        (in_light_space.y / texel).round() * texel,
        in_light_space.z,
    );
    let center = orientation.inverse().transform_point3(snapped);

    let depth = radius * 4.0 + 50.0;
    let eye = center - travel * (depth * 0.5);
    let view = glam::camera::rh::view::look_to_mat4(eye, travel, up);
    let projection =
        glam::camera::rh::proj::directx::orthographic(-radius, radius, -radius, radius, 0.1, depth);
    projection * view
}
