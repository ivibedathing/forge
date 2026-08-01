use super::*;

/// Resolution of the directional shadow map, in texels on a side.
///
/// Fixed rather than authored: `EnvironmentSettings::shadow_distance` already
/// gives the scene the sharpness knob that matters (it sets how much world
/// these texels are spread over), and a second one would only let a scene ask
/// for 8192² and blame the engine for the memory.
pub(crate) const SHADOW_MAP_SIZE: u32 = 2048;

/// The most cascades a scene may ask for, and the length of the frame uniform's
/// matrix array (M38).
///
/// Four rather than a larger number because a cascade is a whole 2048² depth
/// layer *and* a whole pass over the scene's casters: the ceiling is 64 MB and
/// four caster passes, and a scene that wants more wants a different feature.
pub(crate) const MAX_SHADOW_CASCADES: u32 = 4;

/// Each cascade covers a third the extent of the one outside it, so its texels
/// are three times finer (M38 §2.2).
///
/// A fixed ratio rather than an authored lambda for [`SHADOW_MAP_SIZE`]'s
/// reason: the knob that matters is `shadow_distance`, and a second one whose
/// only honest description is "try values until it looks right" is not a scene
/// field. Three is the number that makes the rule sayable — *each level is 3×
/// sharper and covers 3× less*.
const CASCADE_RATIO: f32 = 1.0 / 3.0;

/// How far each cascade reaches, innermost first.
///
/// The cascades are **nested**, all of them starting at the camera, rather than
/// slicing the view into disjoint depth ranges. Two things follow, and both are
/// load-bearing:
///
/// - The last entry is exactly `shadow_distance`, so the outermost cascade is
///   fitted with the matrix M16's single map has always used — which is why a
///   scene at `shadow_cascades: 1` renders what it rendered before M38 rather
///   than something equal to it.
/// - Nested boxes are ordered by size, so the first cascade that *contains* a
///   point is also the sharpest one that does. The receiver needs no split
///   distances and no view-depth reconstruction to choose (§2.3).
///
/// The cost is overlap: every caster inside cascade 0 is drawn again in
/// cascades 1 and 2. See §6.
pub(crate) fn cascade_distances(cascades: u32, shadow_distance: f32) -> Vec<f32> {
    let count = cascades.clamp(1, MAX_SHADOW_CASCADES);
    (0..count)
        .map(|i| shadow_distance * CASCADE_RATIO.powi((count - 1 - i) as i32))
        .collect()
}

/// The shadow map: one 2048² depth layer per cascade.
///
/// A 1×1 placeholder stands in when a scene does not cast shadows: WGSL binds
/// the texture unconditionally, but the sampling is behind `params.y`, so
/// nothing ever reads the placeholder's undefined contents. That keeps one
/// mesh pipeline for both cases instead of two shader permutations.
pub(crate) struct ShadowMap {
    /// The map as the receivers sample it — a plain `D2` view at one cascade,
    /// a `D2Array` beyond. The binding *type* differs between the two, which is
    /// why the cascade count is baked into the pipelines (M38 §4).
    pub(crate) view: wgpu::TextureView,
    /// One view per cascade, each the depth attachment of that cascade's caster
    /// pass.
    pub(crate) cascades: Vec<wgpu::TextureView>,
}

impl ShadowMap {
    pub(crate) fn new(device: &wgpu::Device, size: u32, cascades: u32) -> Self {
        let cascades = cascades.clamp(1, MAX_SHADOW_CASCADES);
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("shadow-map"),
            size: wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: cascades,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        // At one cascade both views are the default descriptor M16 used — the
        // same view it always made, not an equivalent one. Beyond one the map
        // is sampled as an array and rendered a layer at a time.
        if cascades == 1 {
            return Self {
                view: texture.create_view(&wgpu::TextureViewDescriptor::default()),
                cascades: vec![texture.create_view(&wgpu::TextureViewDescriptor::default())],
            };
        }
        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let cascades = (0..cascades)
            .map(|layer| {
                texture.create_view(&wgpu::TextureViewDescriptor {
                    label: Some("shadow-cascade"),
                    dimension: Some(wgpu::TextureViewDimension::D2),
                    base_array_layer: layer,
                    array_layer_count: Some(1),
                    ..Default::default()
                })
            })
            .collect();
        Self { view, cascades }
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

#[cfg(test)]
mod cascade_tests {
    use super::*;

    /// The property the whole design rests on (M38 §2.1): the outermost cascade
    /// is fitted to `shadow_distance` itself, so a scene at one cascade calls
    /// `light_view_projection` with exactly the argument M16 gave it.
    #[test]
    fn the_outermost_cascade_is_the_map_m16_rendered() {
        for cascades in 1..=MAX_SHADOW_CASCADES {
            let distances = cascade_distances(cascades, 240.0);
            assert_eq!(distances.len(), cascades as usize);
            assert_eq!(*distances.last().unwrap(), 240.0);
        }
        assert_eq!(cascade_distances(1, 60.0), vec![60.0]);
    }

    /// Nested and ascending, which is what makes "the first cascade that
    /// contains the point is the sharpest that does" true rather than hopeful.
    #[test]
    fn cascades_nest_from_the_camera_outward() {
        let distances = cascade_distances(4, 240.0);
        for pair in distances.windows(2) {
            assert!(pair[0] < pair[1], "{distances:?} is not ascending");
        }
        // Three times sharper per level, which is the sentence the field's
        // documentation makes to a scene author.
        for pair in distances.windows(2) {
            assert!((pair[1] / pair[0] - 3.0).abs() < 1e-4, "{distances:?}");
        }
    }

    /// A count outside the range is clamped rather than panicking: validation
    /// refuses the file that could get here, and a renderer that indexed past
    /// its texture's layers would fault instead of complaining.
    #[test]
    fn an_out_of_range_count_is_clamped() {
        assert_eq!(cascade_distances(0, 90.0), vec![90.0]);
        assert_eq!(
            cascade_distances(99, 90.0).len(),
            MAX_SHADOW_CASCADES as usize
        );
    }
}
