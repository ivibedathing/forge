use super::*;

/// Per-draw shader data. `repr(C)` and 16-byte aligned to match the WGSL
/// `ObjectUniform` struct field for field; scalars ride in the `w` lanes of
/// vec4s so no explicit padding is needed.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct ObjectUniform {
    pub(crate) mvp: [[f32; 4]; 4],
    pub(crate) model: [[f32; 4]; 4],
    pub(crate) normal_matrix: [[f32; 4]; 4],
    pub(crate) albedo_metallic: [f32; 4],
    pub(crate) emissive_roughness: [f32; 4],
    /// x = alpha, y = transmission; z and w are padding.
    pub(crate) surface: [f32; 4],

    /// Terrain shading (M22): x = live layer count (0 for every other draw,
    /// which is the branch that keeps this free), y = texture scale in metres,
    /// z = colour variation, w = bump.
    ///
    /// Appended at the end of the struct, which is the pattern `FrameUniform`
    /// documents for the same reason: every prior field stays at the offset the
    /// shader already reads it from, so the M4 path is untouched by the growth
    /// as well as by the branch.
    pub(crate) terrain: [f32; 4],
    /// x = the terrain's seed; y, z, w padding. `u32` rather than a float lane
    /// because a seed is an exact bit pattern and large ones do not survive f32.
    pub(crate) terrain_seed: [u32; 4],
    /// Fixed-size table, `terrain.x` entries live. Unused slots are zeroed and
    /// never read — the shader loops to the count.
    pub(crate) terrain_layers: [TerrainLayerUniform; MAX_TERRAIN_LAYERS],

    /// Material maps (M26), appended at the end for the reason terrain's
    /// fields were: every field above keeps the offset the shader already reads
    /// it from. xy = uv scale, zw = uv offset.
    pub(crate) map_uv: [f32; 4],
    /// x = which maps are bound, as bits; y = alpha cutoff, z = normal
    /// strength, w = ior.
    pub(crate) map_params: [f32; 4],
    /// x = thickness in metres; yzw = per-channel attenuation.
    pub(crate) map_volume: [f32; 4],
}

/// Which map slots a draw has bound, as the bits `map_params.x` carries.
pub(crate) fn map_bits(textures: &engine_core::texture::MaterialTextures) -> u32 {
    let mut bits = 0;
    if textures.albedo.is_some() {
        bits |= MAP_ALBEDO;
    }
    if textures.orm.is_some() {
        bits |= MAP_ORM;
    }
    if textures.normal.is_some() {
        bits |= MAP_NORMAL;
    }
    if textures.emissive.is_some() {
        bits |= MAP_EMISSIVE;
    }
    bits
}

/// The bits `map_params.x` carries, matching `textured.wgsl`.
pub(crate) const MAP_ALBEDO: u32 = 1;
pub(crate) const MAP_ORM: u32 = 2;
pub(crate) const MAP_NORMAL: u32 = 4;
pub(crate) const MAP_EMISSIVE: u32 = 8;

/// One terrain layer as the object uniform carries it, matching WGSL
/// `TerrainLayer`.
#[repr(C)]
#[derive(Clone, Copy, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct TerrainLayerUniform {
    /// rgb = linear albedo, w = roughness.
    pub(crate) albedo_roughness: [f32; 4],
    /// x, y = world-Y band in metres; z, w = slope band in degrees.
    pub(crate) bands: [f32; 4],
    /// x = height fade in metres, y = boundary jitter, z = slope fade in
    /// degrees; w is padding.
    pub(crate) blend_noise: [f32; 4],
}

/// Per-pass shader data, matching WGSL `FrameUniform`. Colors arrive already
/// premultiplied by intensity (`ResolvedLights` does that); `sun_direction` is
/// the direction the light travels.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct FrameUniform {
    pub(crate) camera_pos: [f32; 4],
    pub(crate) sun_direction: [f32; 4],
    pub(crate) sun_color: [f32; 4],
    pub(crate) ambient: [f32; 4],
    pub(crate) inv_view_proj: [[f32; 4]; 4],
    pub(crate) light_view_proj: [[f32; 4]; 4],
    pub(crate) sky_zenith: [f32; 4],
    pub(crate) sky_horizon: [f32; 4],
    pub(crate) sky_ground: [f32; 4],
    /// x = fog density, y = shadows on, z = shadow-map texel size, w = sky on.
    pub(crate) params: [f32; 4],
    /// x = live point-light count; y, z, w are padding.
    ///
    /// A second params vec4 rather than a spare lane in the first: the existing
    /// lanes are all taken, and a uniform struct that grows only at its end
    /// leaves every prior field at the offset the shader already reads it from.
    pub(crate) params2: [f32; 4],
    /// Fixed-size array, `count` entries live. Unused slots are zeroed, which
    /// the shader never reads — it loops to `count`.
    pub(crate) point_lights: [PointLightUniform; MAX_POINT_LIGHTS],
    /// World → clip (M26). Appended after the array, where it leaves every
    /// prior field at the offset the shaders already read it from, and declared
    /// only by the refraction variant — a shader may declare a *prefix* of the
    /// buffer it is bound to, which is why the other five shaders that spell
    /// this struct out did not have to change.
    pub(crate) view_proj: [[f32; 4]; 4],
}

/// One skinned draw's joint palette, matching WGSL `JointPalette` (M30).
///
/// Fixed-size at [`MAX_JOINTS`], the `MAX_POINT_LIGHTS` / `MAX_ROAD_KERBS`
/// idiom: a rig with more joints is `too_many_joints` at *validate* time,
/// before a device exists, rather than a character that renders correctly up to
/// joint 128 and explodes past it. Unused slots are zeroed and never read — no
/// vertex indexes them, because validation refused the file that could.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct JointPaletteUniform {
    /// Three rows per joint. See `skin.wgsl` for why the fourth is not stored.
    pub(crate) joints: [[[f32; 4]; 3]; MAX_JOINTS],
}

impl Default for JointPaletteUniform {
    fn default() -> Self {
        Self {
            joints: [[[0.0; 4]; 3]; MAX_JOINTS],
        }
    }
}

impl JointPaletteUniform {
    /// Pack a CPU palette into the rows the shader reads.
    ///
    /// glam matrices are column-major, so a transpose is what turns columns
    /// into the rows this packs — and the fourth row, which an affine matrix
    /// always leaves at (0, 0, 0, 1), is the one dropped.
    pub(crate) fn from_palette(palette: &[Mat4]) -> Self {
        let mut out = Self::default();
        for (slot, matrix) in out.joints.iter_mut().zip(palette) {
            let rows = matrix.transpose();
            *slot = [
                rows.x_axis.to_array(),
                rows.y_axis.to_array(),
                rows.z_axis.to_array(),
            ];
        }
        out
    }
}

/// One point light as the frame uniform carries it, matching WGSL
/// `PointLightData`.
#[repr(C)]
#[derive(Clone, Copy, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct PointLightUniform {
    /// xyz = world position, w = range in world units.
    pub(crate) position_range: [f32; 4],
    /// rgb = color premultiplied by intensity; w is padding.
    pub(crate) color: [f32; 4],
}

/// Per-surface water data, matching WGSL `WaterUniform` (M18).
///
/// The waves ride in the same uniform as the surface's optics rather than in a
/// storage buffer: eight of them is the component's documented cap, so the
/// array is small, fixed, and costs one write per surface per frame.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct WaterUniform {
    /// World → clip. Waves displace in **world** space, so unlike a mesh this
    /// cannot be a premultiplied MVP.
    pub(crate) view_proj: [[f32; 4]; 4],
    pub(crate) model: [[f32; 4]; 4],
    /// rgb = shallow color, w = detail strength.
    pub(crate) shallow_detail: [f32; 4],
    /// rgb = deep color, w = depth fade in metres.
    pub(crate) deep_fade: [f32; 4],
    /// rgb = foam color, w = shore foam width in metres.
    pub(crate) foam: [f32; 4],
    /// x = roughness, y = opacity, z = crest foam, w = detail cell size.
    pub(crate) params: [f32; 4],
    /// x = wave count, y = time in seconds, z = index of refraction (M27);
    /// w is padding.
    ///
    /// The IOR rides in a slot M18 declared padding so that **one** uniform
    /// layout feeds both water pipelines — the plain shader simply never reads
    /// it, and `water_objects` stays a single buffer.
    pub(crate) clock: [f32; 4],
    /// Two vec4s per wave, [`MAX_WAVES`] of them: `(dir.x, dir.z, amplitude, k)`
    /// then `(q, omega, 0, 0)`. Packed by [`pack_waves`].
    pub(crate) waves: [[f32; 4]; MAX_WAVES * 2],
}

/// Per-cloud data, matching WGSL `CloudUniform` (M20).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct CloudUniform {
    /// World → clip. Drift displaces in **world** space, so unlike a mesh this
    /// cannot be a premultiplied MVP.
    pub(crate) view_proj: [[f32; 4]; 4],
    pub(crate) model: [[f32; 4]; 4],
    /// Inverse-transpose of `model`: non-uniform scale is the normal case for a
    /// cloud, since that is what makes one wider than it is tall.
    pub(crate) normal_matrix: [[f32; 4]; 4],
    /// rgb = sunlit color, w = density.
    pub(crate) color_density: [f32; 4],
    /// rgb = self-shadowed color, w = feather exponent.
    pub(crate) shade_feather: [f32; 4],
    /// xyz = drift in m/s, w = wrap distance in metres (0 = never wrap).
    pub(crate) drift_wrap: [f32; 4],
    /// x = scene time in seconds; y, z and w are padding.
    pub(crate) params: [f32; 4],
}

/// Per-road shader data, matching WGSL `RoadUniform` (M23).
///
/// What a road *is* rides in the ordinary `ObjectUniform` beside this — model
/// matrix, asphalt colour, roughness — so a road casts shadows through the
/// unchanged shadow pipeline. This carries only what markings need.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct RoadUniform {
    /// x = half the asphalt width, y = shoulder width, z = total length,
    /// w = dash period in metres (0 = a solid centre line).
    pub(crate) metrics: [f32; 4],
    /// rgb = paint colour, w = edge-line width.
    pub(crate) paint: [f32; 4],
    /// x = edge inset, y = centre-line width, z = dash duty, w = start-line width.
    pub(crate) lines: [f32; 4],
    /// rgb = the red half of a kerb, w = kerb width.
    pub(crate) kerb: [f32; 4],
    /// rgb = shoulder colour, w = live kerb-span count.
    pub(crate) shoulder: [f32; 4],
    /// rgb = embankment colour, w = 1 when a start line is painted.
    pub(crate) bank: [f32; 4],
    /// x = where that line is, in metres along the centerline; rest padding.
    pub(crate) start: [f32; 4],
    /// `(start_v, end_v, side, stripe)` per kerbed corner. Unused slots are
    /// zeroed and never read — the shader loops to the count.
    pub(crate) kerbs: [[f32; 4]; MAX_ROAD_KERBS],
}

/// One life-cycle keyframe as the shader reads it, matching WGSL
/// `GrowthStageData` (M29).
#[repr(C)]
#[derive(Clone, Copy, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct GrowthStageData {
    /// x = at, y = height fraction, z = width fraction, w = lean in **radians**
    /// (the file authors degrees; the conversion happens once, here).
    pub(crate) shape: [f32; 4],
    /// rgb = colour at the plant's base, w = sway multiplier.
    pub(crate) color_sway: [f32; 4],
    /// rgb = colour at the plant's tip, w padding.
    pub(crate) tip: [f32; 4],
}

/// Per-meadow shader data, matching WGSL `MeadowUniform` (M29).
///
/// No model matrix, unlike every other per-object uniform here: a meadow's
/// instances are placed in world space, because their altitude came off the
/// terrain and a transform applied afterwards would lift them back off it.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct MeadowUniform {
    pub(crate) view_proj: [[f32; 4]; 4],
    /// x = scene time in seconds, y = cycle length in seconds (0 = frozen),
    /// z = base phase, w = live stage count.
    pub(crate) clock: [f32; 4],
    /// xy = unit wind direction in XZ, z = wind strength in radians,
    /// w = gust travel speed in m/s.
    pub(crate) wind: [f32; 4],
    /// rgb = flower colour, w = reseed jitter radius in metres.
    pub(crate) flower: [f32; 4],
    /// Unused slots are zeroed and never read — the shader loops to the count.
    pub(crate) stages: [GrowthStageData; MAX_GROWTH_STAGES],
}

/// Per-pass particle data, matching WGSL `ParticleFrame`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct ParticleFrameUniform {
    pub(crate) view_proj: [[f32; 4]; 4],
    pub(crate) camera_right: [f32; 4],
    pub(crate) camera_up: [f32; 4],
    /// xyz = camera position, w = fog density.
    pub(crate) camera_pos: [f32; 4],
    pub(crate) fog_color: [f32; 4],
}

/// One particle billboard as the instance buffer carries it.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct ParticleRaw {
    /// xyz = world position, w = half-size.
    pub(crate) pos_size: [f32; 4],
    /// rgb = linear color, a = opacity.
    pub(crate) color: [f32; 4],
    /// xyz = world velocity, w = stretch in seconds (0 = a round sprite).
    pub(crate) velocity_stretch: [f32; 4],
}

/// `MeadowVertex` with the derives the GPU path needs. Field for field with
/// `engine_core::meadow::MeadowVertex`, and the vertex layout in
/// [`SceneRenderer::meadow_pipeline`] is written against this.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct MeadowVertexRaw {
    pub(crate) centre: [f32; 3],
    pub(crate) normal: [f32; 3],
    pub(crate) offset: [f32; 3],
    pub(crate) anchor: [f32; 3],
    pub(crate) span: [f32; 3],
    pub(crate) organ: u32,
}

/// `MeadowInstance`, repacked into the vec4 lanes the shader reads.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct MeadowInstanceRaw {
    /// xyz = world position, w = metres of plant per unit of template height.
    pub(crate) pos_scale: [f32; 4],
    /// x = yaw, y = phase offset, zw = the ground's slope here.
    pub(crate) params: [f32; 4],
    pub(crate) seed: u32,
}

/// Pack one cloud's shading parameters for the cloud shader (M20).
///
/// Everything here is a straight copy out of the component. The only thing
/// worth naming is what is *absent*: no time is folded into the model matrix,
/// because `drift` is applied in the vertex stage instead — which is what keeps
/// `Scene::cloud_items` a pure function of the file and the grown mesh's `Arc`
/// stable across frames, so the renderer uploads each cloud once.
pub(crate) fn cloud_uniform(item: &CloudItem, view_projection: Mat4, time: f32) -> CloudUniform {
    let c = &item.cloud;
    CloudUniform {
        view_proj: view_projection.to_cols_array_2d(),
        model: item.model.to_cols_array_2d(),
        normal_matrix: item.model.inverse().transpose().to_cols_array_2d(),
        color_density: c.color.extend(c.density).to_array(),
        shade_feather: c.shade_color.extend(c.feather).to_array(),
        drift_wrap: c.drift.extend(c.drift_wrap).to_array(),
        params: [time, 0.0, 0.0, 0.0],
    }
}

/// Pack a meadow's clock, wind and life-cycle table for the shader (M29).
///
/// The two conversions that happen here rather than in the shader are the file's
/// units meeting the maths: `lean` and `wind` are authored in degrees and used
/// as rotation angles, and `wind_direction` is a heading in degrees that becomes
/// a unit XZ vector through [`wave_direction`](engine_core::water::wave_direction)
/// — the *same* function `Water`'s waves use, so "0° travels toward −Z" cannot
/// come to mean two different things in two components.
pub(crate) fn meadow_uniform(item: &MeadowItem, view_projection: Mat4, time: f32) -> MeadowUniform {
    let m = &item.meadow;
    let mut stages = [GrowthStageData::default(); MAX_GROWTH_STAGES];
    for (slot, stage) in stages
        .iter_mut()
        .zip(m.stages.iter().take(MAX_GROWTH_STAGES))
    {
        *slot = GrowthStageData {
            shape: [stage.at, stage.height, stage.width, stage.lean.to_radians()],
            color_sway: stage.color.extend(stage.sway).to_array(),
            tip: stage.tip_color.extend(0.0).to_array(),
        };
    }

    let direction = engine_core::water::wave_direction(m.wind_direction);
    MeadowUniform {
        view_proj: view_projection.to_cols_array_2d(),
        clock: [
            time,
            m.cycle_length,
            m.phase,
            m.stages.len().min(MAX_GROWTH_STAGES) as f32,
        ],
        wind: [direction.x, direction.y, m.wind.to_radians(), m.wind_speed],
        flower: m.flower_color.extend(item.patch.jitter_radius).to_array(),
        stages,
    }
}

/// Pack a terrain's layer table for the mesh shader (M22), zeroed for every
/// other draw.
///
/// Slope arrives in degrees and stays in degrees: the shader compares it against
/// an angle it derives with `acos`, and keeping the file's unit all the way to
/// the comparison is what makes `slope_range: [30, 90]` mean what it reads as.
pub(crate) fn terrain_layers(
    terrain: Option<&Terrain>,
) -> [TerrainLayerUniform; MAX_TERRAIN_LAYERS] {
    let mut layers = [TerrainLayerUniform::default(); MAX_TERRAIN_LAYERS];
    let Some(terrain) = terrain else {
        return layers;
    };

    for (slot, layer) in terrain
        .layers
        .iter()
        .take(MAX_TERRAIN_LAYERS)
        .zip(layers.iter_mut())
        .map(|(layer, slot)| (slot, layer))
    {
        slot.albedo_roughness = layer.albedo.extend(layer.roughness).to_array();
        slot.bands = [
            layer.height_range[0],
            layer.height_range[1],
            layer.slope_range[0],
            layer.slope_range[1],
        ];
        slot.blend_noise = [layer.height_blend, layer.noise, layer.slope_blend, 0.0];
    }

    layers
}

/// Pack one surface's shading parameters and waves for the water shader.
///
/// The wave packing is where the file's units become the shader's: `wavelength`
/// becomes the wavenumber `k = 2π/λ`, `speed` becomes the angular frequency
/// `ω = speed·k`, and `steepness` becomes Gerstner's `Q = steepness/(k·A)`.
///
/// That last conversion is the one worth stating, because it is what makes the
/// validation rule true: with `Q` scaled this way, the horizontal Jacobian
/// contributed by a wave is exactly its `steepness`, so a total steepness of 1
/// is precisely the point where the surface starts folding through itself.
/// Dividing by the wave *count* as well — the form most references give — would
/// leave the same file looking calmer as waves were added to it.
pub(crate) fn water_uniform(item: &WaterItem, view_projection: Mat4, time: f32) -> WaterUniform {
    let w: &Water = &item.water;
    let mut waves = [[0.0f32; 4]; MAX_WAVES * 2];
    let count = w.waves.len().min(MAX_WAVES);

    for (slot, wave) in w.waves.iter().take(count).enumerate() {
        let direction = engine_core::water::wave_direction(wave.direction);
        let k = std::f32::consts::TAU / wave.wavelength.max(1e-4);
        // A wave with no amplitude has no crests to gather toward, so its Q is
        // 0 rather than a division by zero.
        let q = if wave.amplitude > 0.0 {
            wave.steepness / (k * wave.amplitude)
        } else {
            0.0
        };
        waves[slot * 2] = [direction.x, direction.y, wave.amplitude, k];
        waves[slot * 2 + 1] = [q, wave.speed * k, 0.0, 0.0];
    }

    WaterUniform {
        view_proj: view_projection.to_cols_array_2d(),
        model: item.model.to_cols_array_2d(),
        shallow_detail: w.shallow_color.extend(w.detail).to_array(),
        deep_fade: w.deep_color.extend(w.depth_fade).to_array(),
        foam: w.foam_color.extend(w.shore_foam).to_array(),
        params: [w.roughness, w.opacity, w.crest_foam, w.detail_scale],
        clock: [count as f32, time, w.ior, 0.0],
        waves,
    }
}

/// Pack one road's marking parameters for the shader (M23).
///
/// The road's geometry, dash period and kerb spans were all decided when the
/// ribbon was generated — this only copies them into the uniform, which is why
/// a road that is not being edited costs one small write per frame however long
/// it is.
pub(crate) fn road_uniform(item: &RoadItem) -> RoadUniform {
    let road = &item.road;
    let markings = &road.markings;
    let surface = &item.surface;

    let mut kerbs = [[0.0f32; 4]; MAX_ROAD_KERBS];
    let count = surface.kerbs.len().min(MAX_ROAD_KERBS);
    for (slot, span) in surface.kerbs.iter().take(count).enumerate() {
        kerbs[slot] = [span.start, span.end, span.side, span.stripe];
    }

    RoadUniform {
        metrics: [
            road.width / 2.0,
            road.shoulder,
            surface.length,
            surface.dash_period,
        ],
        paint: markings.color.extend(markings.edge_width).to_array(),
        lines: [
            markings.edge_inset,
            markings.center_width,
            surface.dash_duty,
            markings.start_line_width,
        ],
        kerb: markings.kerb_color.extend(markings.kerb_width).to_array(),
        shoulder: road.shoulder_color.extend(count as f32).to_array(),
        bank: road
            .bank_color
            .extend(if markings.start_line { 1.0 } else { 0.0 })
            .to_array(),
        start: [markings.start_line_at, 0.0, 0.0, 0.0],
        kerbs,
    }
}
