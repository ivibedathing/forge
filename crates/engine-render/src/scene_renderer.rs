//! Renders a scene's draw list.
//!
//! Like [`crate::Renderer`], this draws into any `TextureView` and knows
//! nothing about windows — that is what lets `engine screenshot` reuse it
//! unchanged.
//!
//! GPU resources persist across frames (M15). The renderer originally created
//! every buffer and bind group per call, which is the right shape for
//! `engine screenshot` — render once, exit — but in the viewer it meant
//! reallocating a vertex buffer, an index buffer, a uniform buffer, and a bind
//! group per entity, sixty times a second. A `SceneRenderer` now keeps:
//!
//! - uploaded geometry, keyed on the `Arc<MeshData>` the draw list carries, so
//!   a scene's meshes upload once and stay; entries not drawn for a while are
//!   evicted, and a reloaded asset arrives as a new `Arc` and re-uploads
//! - one object-uniform buffer addressed by dynamic offset, rewritten per
//!   frame in a single `write_buffer` instead of one buffer per entity
//! - the frame, particle, and HUD buffers, grown when they must be and
//!   rewritten in place otherwise
//!
//! None of this changes a rendered pixel: the same data reaches the same
//! pipelines in the same order, which is what keeps every committed baseline
//! bit-exact.

use std::collections::HashMap;
use std::sync::Arc;

use engine_core::components::{Camera, ParticleBlend, Terrain, Water, MAX_POINT_LIGHTS};
use engine_core::math::{Mat4, Vec3};
use engine_core::mesh::MeshData;
use engine_core::particles::ParticleInstance;
use engine_core::scene::{CloudItem, EnvironmentSettings, RenderItem, ResolvedLights, WaterItem};
use engine_core::terrain::MAX_TERRAIN_LAYERS;
use engine_core::water::MAX_WAVES;

pub const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// Per-draw shader data. `repr(C)` and 16-byte aligned to match the WGSL
/// `ObjectUniform` struct field for field; scalars ride in the `w` lanes of
/// vec4s so no explicit padding is needed.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ObjectUniform {
    mvp: [[f32; 4]; 4],
    model: [[f32; 4]; 4],
    normal_matrix: [[f32; 4]; 4],
    albedo_metallic: [f32; 4],
    emissive_roughness: [f32; 4],
    /// x = alpha, y = transmission; z and w are padding.
    surface: [f32; 4],

    /// Terrain shading (M22): x = live layer count (0 for every other draw,
    /// which is the branch that keeps this free), y = texture scale in metres,
    /// z = colour variation, w = bump.
    ///
    /// Appended at the end of the struct, which is the pattern `FrameUniform`
    /// documents for the same reason: every prior field stays at the offset the
    /// shader already reads it from, so the M4 path is untouched by the growth
    /// as well as by the branch.
    terrain: [f32; 4],
    /// x = the terrain's seed; y, z, w padding. `u32` rather than a float lane
    /// because a seed is an exact bit pattern and large ones do not survive f32.
    terrain_seed: [u32; 4],
    /// Fixed-size table, `terrain.x` entries live. Unused slots are zeroed and
    /// never read — the shader loops to the count.
    terrain_layers: [TerrainLayerUniform; MAX_TERRAIN_LAYERS],
}

/// One terrain layer as the object uniform carries it, matching WGSL
/// `TerrainLayer`.
#[repr(C)]
#[derive(Clone, Copy, Default, bytemuck::Pod, bytemuck::Zeroable)]
struct TerrainLayerUniform {
    /// rgb = linear albedo, w = roughness.
    albedo_roughness: [f32; 4],
    /// x, y = world-Y band in metres; z, w = slope band in degrees.
    bands: [f32; 4],
    /// x = height fade in metres, y = boundary jitter, z = slope fade in
    /// degrees; w is padding.
    blend_noise: [f32; 4],
}

/// Per-pass shader data, matching WGSL `FrameUniform`. Colors arrive already
/// premultiplied by intensity (`ResolvedLights` does that); `sun_direction` is
/// the direction the light travels.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FrameUniform {
    camera_pos: [f32; 4],
    sun_direction: [f32; 4],
    sun_color: [f32; 4],
    ambient: [f32; 4],
    inv_view_proj: [[f32; 4]; 4],
    light_view_proj: [[f32; 4]; 4],
    sky_zenith: [f32; 4],
    sky_horizon: [f32; 4],
    sky_ground: [f32; 4],
    /// x = fog density, y = shadows on, z = shadow-map texel size, w = sky on.
    params: [f32; 4],
    /// x = live point-light count; y, z, w are padding.
    ///
    /// A second params vec4 rather than a spare lane in the first: the existing
    /// lanes are all taken, and a uniform struct that grows only at its end
    /// leaves every prior field at the offset the shader already reads it from.
    params2: [f32; 4],
    /// Fixed-size array, `count` entries live. Unused slots are zeroed, which
    /// the shader never reads — it loops to `count`.
    point_lights: [PointLightUniform; MAX_POINT_LIGHTS],
}

/// One point light as the frame uniform carries it, matching WGSL
/// `PointLightData`.
#[repr(C)]
#[derive(Clone, Copy, Default, bytemuck::Pod, bytemuck::Zeroable)]
struct PointLightUniform {
    /// xyz = world position, w = range in world units.
    position_range: [f32; 4],
    /// rgb = color premultiplied by intensity; w is padding.
    color: [f32; 4],
}

/// Per-surface water data, matching WGSL `WaterUniform` (M18).
///
/// The waves ride in the same uniform as the surface's optics rather than in a
/// storage buffer: eight of them is the component's documented cap, so the
/// array is small, fixed, and costs one write per surface per frame.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct WaterUniform {
    /// World → clip. Waves displace in **world** space, so unlike a mesh this
    /// cannot be a premultiplied MVP.
    view_proj: [[f32; 4]; 4],
    model: [[f32; 4]; 4],
    /// rgb = shallow color, w = detail strength.
    shallow_detail: [f32; 4],
    /// rgb = deep color, w = depth fade in metres.
    deep_fade: [f32; 4],
    /// rgb = foam color, w = shore foam width in metres.
    foam: [f32; 4],
    /// x = roughness, y = opacity, z = crest foam, w = detail cell size.
    params: [f32; 4],
    /// x = wave count, y = time in seconds; z and w are padding.
    clock: [f32; 4],
    /// Two vec4s per wave, [`MAX_WAVES`] of them: `(dir.x, dir.z, amplitude, k)`
    /// then `(q, omega, 0, 0)`. Packed by [`pack_waves`].
    waves: [[f32; 4]; MAX_WAVES * 2],
}

/// Per-cloud data, matching WGSL `CloudUniform` (M20).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CloudUniform {
    /// World → clip. Drift displaces in **world** space, so unlike a mesh this
    /// cannot be a premultiplied MVP.
    view_proj: [[f32; 4]; 4],
    model: [[f32; 4]; 4],
    /// Inverse-transpose of `model`: non-uniform scale is the normal case for a
    /// cloud, since that is what makes one wider than it is tall.
    normal_matrix: [[f32; 4]; 4],
    /// rgb = sunlit color, w = density.
    color_density: [f32; 4],
    /// rgb = self-shadowed color, w = feather exponent.
    shade_feather: [f32; 4],
    /// xyz = drift in m/s, w = wrap distance in metres (0 = never wrap).
    drift_wrap: [f32; 4],
    /// x = scene time in seconds; y, z and w are padding.
    params: [f32; 4],
}

/// Per-pass particle data, matching WGSL `ParticleFrame`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ParticleFrameUniform {
    view_proj: [[f32; 4]; 4],
    camera_right: [f32; 4],
    camera_up: [f32; 4],
    /// xyz = camera position, w = fog density.
    camera_pos: [f32; 4],
    fog_color: [f32; 4],
}

/// One particle billboard as the instance buffer carries it.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ParticleRaw {
    /// xyz = world position, w = half-size.
    pos_size: [f32; 4],
    /// rgb = linear color, a = opacity.
    color: [f32; 4],
    /// xyz = world velocity, w = stretch in seconds (0 = a round sprite).
    velocity_stretch: [f32; 4],
}

/// One uploaded mesh, cached across frames.
///
/// Positions and normals share `vertices`; `normals_offset` is where the
/// second slot starts. `_geometry` keeps the source `Arc` alive: the cache is
/// keyed on that allocation's address, and holding a strong reference is what
/// stops a freed mesh's address from being reused by a *different* mesh and
/// silently colliding.
struct CachedMesh {
    _geometry: Arc<MeshData>,
    vertices: wgpu::Buffer,
    normals_offset: u64,
    indices: wgpu::Buffer,
    index_count: u32,
    /// Frame counter at the last draw that used this mesh; entries idle for
    /// [`MESH_CACHE_LIFETIME`] frames are dropped.
    last_used: u64,
}

/// How many frames an unused mesh stays uploaded. Long enough that a scene
/// alternating between two sets of geometry does not re-upload every frame,
/// short enough that editing a scene down to a few entities gives the memory
/// back promptly.
const MESH_CACHE_LIFETIME: u64 = 240;

/// A uniform buffer plus the bind group naming it, recreated only when the
/// buffer has to grow.
struct Uniforms {
    buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    /// Capacity in bytes.
    size: u64,
}

/// The HUD overlay's cached texture. Kept at the largest size any frame has
/// needed so far — canvases are small (they cover only what the HUD touches)
/// and a growing one is rare after the first few frames.
struct HudTarget {
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    placement: wgpu::Buffer,
    width: u32,
    height: u32,
}

/// The opaque pass's depth, copied where the water pass can read it (M18).
///
/// A pass cannot sample the depth attachment it is testing against, so the
/// frame gains a fullscreen copy between the opaque geometry and the water:
/// `Depth32Float` (possibly multisampled) → single-sampled `R32Float`. Water is
/// the only thing that reads it, so it is allocated the first time a scene has
/// any, and resized when the target does.
struct SceneDepth {
    view: wgpu::TextureView,
    bind_group: wgpu::BindGroup,
    width: u32,
    height: u32,
}

impl SceneDepth {
    fn new(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        width: u32,
        height: u32,
    ) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("scene-depth-copy"),
            size: wgpu::Extent3d {
                width: width.max(1),
                height: height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            // R32Float, not a depth format: this is read with `textureLoad` as
            // an ordinary float, and depth formats cannot be sampled that way.
            format: wgpu::TextureFormat::R32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("scene-depth-copy"),
            layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            }],
        });
        Self {
            view,
            bind_group,
            width: width.max(1),
            height: height.max(1),
        }
    }

    /// The copy for a target of this size, reallocated when the size changes.
    /// Exact rather than grow-only: the shader converts pixel coordinates to
    /// UVs with this texture's own dimensions, so a stale larger copy would
    /// read the wrong pixels rather than merely waste memory.
    fn ensure<'a>(
        slot: &'a mut Option<Self>,
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        width: u32,
        height: u32,
    ) -> &'a Self {
        let fits = slot
            .as_ref()
            .is_some_and(|held| held.width == width.max(1) && held.height == height.max(1));
        if !fits {
            *slot = Some(Self::new(device, layout, width, height));
        }
        slot.as_ref().expect("just ensured")
    }
}

/// Resolution of the directional shadow map, in texels on a side.
///
/// Fixed rather than authored: `EnvironmentSettings::shadow_distance` already
/// gives the scene the sharpness knob that matters (it sets how much world
/// these texels are spread over), and a second one would only let a scene ask
/// for 8192² and blame the engine for the memory.
const SHADOW_MAP_SIZE: u32 = 2048;

/// The shadow map and the bind group naming it.
///
/// A 1×1 placeholder stands in when a scene does not cast shadows: WGSL binds
/// the texture unconditionally, but the sampling is behind `params.y`, so
/// nothing ever reads the placeholder's undefined contents. That keeps one
/// mesh pipeline for both cases instead of two shader permutations.
struct ShadowMap {
    view: wgpu::TextureView,
    bind_group: wgpu::BindGroup,
}

impl ShadowMap {
    fn new(device: &wgpu::Device, layout: &wgpu::BindGroupLayout, size: u32) -> Self {
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
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("shadow-sampler"),
            // Linear filtering on a comparison sampler is hardware PCF: each
            // tap already returns a bilinear blend of four depth *tests*, so
            // the 3×3 kernel in the shader is effectively 6×6 for free.
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            compare: Some(wgpu::CompareFunction::LessEqual),
            ..Default::default()
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("shadow-bind-group"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        Self { view, bind_group }
    }
}

/// Everything one scene render needs beyond the device and queue.
pub struct ScenePass<'a> {
    /// Where the finished, single-sampled image lands. With MSAA on this is
    /// the resolve target rather than the thing drawn into.
    pub target: &'a wgpu::TextureView,
    /// The multisampled color attachment, when `environment.samples > 1`.
    /// `None` draws straight into `target`, exactly as before MSAA existed.
    pub msaa: Option<&'a wgpu::TextureView>,
    /// Must match the sample count of the color attachment actually drawn
    /// into — see [`depth_texture_multisampled`].
    pub depth: &'a wgpu::TextureView,
    /// The target's dimensions in pixels. Needed because a `TextureView` cannot
    /// be asked its size, and the water pass allocates a depth copy to match.
    pub target_size: [u32; 2],
    pub items: &'a [RenderItem],
    /// Water surfaces (M18), drawn with the blended geometry and sorted among
    /// it. Pass `&[]` for a scene with no water, which is then rendered by
    /// exactly the passes that existed before water did.
    pub water: &'a [WaterItem],
    /// Clouds (M20), drawn with the blended geometry and sorted among it. Pass
    /// `&[]` for a scene with no clouds, which is then rendered by exactly the
    /// draws that existed before clouds did.
    pub clouds: &'a [CloudItem],
    /// Particle billboards, drawn after the meshes (alpha-blended, depth-read
    /// only). Pass `&[]` when nothing simulates particles.
    pub particles: &'a [ParticleInstance],
    pub view_projection: Mat4,
    /// World-space camera position, for the specular view vector.
    pub camera_position: Vec3,
    /// The camera's world-space right and up axes — the billboard basis.
    /// Only read when `particles` is non-empty.
    pub camera_right: Vec3,
    pub camera_up: Vec3,
    pub lights: ResolvedLights,
    /// How this scene is rendered: sky, fog, shadows, sample count (M16). All
    /// defaults off, in which case this draws exactly what it drew before the
    /// block existed.
    pub environment: EnvironmentSettings,
    /// Scene time in seconds — the reproducible clock, never wall clock: the
    /// `--time` flag if the command took one, otherwise `steps × dt`. Water is
    /// its only consumer, and a frame with no water never reads it, which is
    /// why an unchanged scene renders identically whatever this says.
    pub time: f32,
    /// Used only when `environment.sky` is off; the sky pass overwrites every
    /// pixel it would have set.
    pub clear: wgpu::Color,
    /// Screen-space overlay, composited after the mesh pass (M12). Must be
    /// rasterized at the target's dimensions. `None` skips the overlay pass
    /// entirely, so HUD-less scenes render byte-identically to pre-M12.
    pub hud: Option<&'a crate::hud::HudOverlay>,
}

pub struct SceneRenderer {
    pipeline: wgpu::RenderPipeline,
    /// Same shader as `pipeline`, blended and depth-write-off, for materials
    /// with `alpha < 1` or `transmission > 0`.
    transparent_pipeline: wgpu::RenderPipeline,
    /// `pipeline` with the terrain material generator spliced into its shader
    /// (M22) — see `with_terrain` for why this is a separate module rather than
    /// a branch inside the shared one.
    terrain_pipeline: wgpu::RenderPipeline,
    shadow_pipeline: wgpu::RenderPipeline,
    sky_pipeline: wgpu::RenderPipeline,
    water_pipeline: wgpu::RenderPipeline,
    cloud_pipeline: wgpu::RenderPipeline,
    depth_resolve_pipeline: wgpu::RenderPipeline,
    water_layout: wgpu::BindGroupLayout,
    cloud_layout: wgpu::BindGroupLayout,
    scene_depth_layout: wgpu::BindGroupLayout,
    depth_source_layout: wgpu::BindGroupLayout,
    particle_pipeline: wgpu::RenderPipeline,
    /// Same shader and same instance buffer as `particle_pipeline`, blending
    /// additively — for `ParticleEmitter.blend: "additive"` (fire, sparks).
    additive_particle_pipeline: wgpu::RenderPipeline,
    object_layout: wgpu::BindGroupLayout,
    hud_pipeline: wgpu::RenderPipeline,
    hud_layout: wgpu::BindGroupLayout,
    shadow_layout: wgpu::BindGroupLayout,
    format: wgpu::TextureFormat,
    samples: u32,

    // Everything below persists across frames; see the module doc.
    /// Bound whenever shadows are off. See [`ShadowMap`].
    shadow_placeholder: ShadowMap,
    /// The real map, allocated the first time a scene casts shadows so that
    /// scenes which never do pay nothing for it.
    shadow_map: Option<ShadowMap>,
    meshes: HashMap<usize, CachedMesh>,
    frame_uniform: Uniforms,
    /// Object uniforms for the whole draw list, one per `object_stride` bytes.
    objects: Option<Uniforms>,
    /// Distance between consecutive object uniforms: the struct size rounded
    /// up to the device's dynamic-offset alignment.
    object_stride: u64,
    /// The same arrangement for water surfaces, which carry a different (and
    /// much larger) uniform.
    water_objects: Option<Uniforms>,
    water_stride: u64,
    /// And again for clouds (M20), which need neither the scene depth nor the
    /// shadow map and so carry the smallest uniform of the three.
    cloud_objects: Option<Uniforms>,
    cloud_stride: u64,
    /// The opaque depth copy the water pass reads, allocated on the first frame
    /// that has any water in it and resized with the target.
    scene_depth: Option<SceneDepth>,
    /// Bind group naming the *source* depth attachment for the resolve pass.
    /// Rebuilt whenever the depth view changes, which is every frame in the
    /// viewer (the swapchain hands out a new one) and once in a screenshot.
    depth_source: Option<wgpu::BindGroup>,
    particle_instances: Option<wgpu::Buffer>,
    particle_uniform: Uniforms,
    /// One cached texture per overlay canvas, reused across frames.
    hud_targets: Vec<HudTarget>,
    frame_index: u64,
}

impl SceneRenderer {
    /// A renderer for single-sampled targets — the pre-MSAA constructor, kept
    /// because most callers (tests, the editor viewport) have no scene to ask.
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        Self::with_samples(device, format, 1)
    }

    /// A renderer whose scene pipelines are built for `samples`-way MSAA.
    ///
    /// The sample count is baked into every pipeline, so it belongs to the
    /// renderer rather than to a frame: a scene that changes `samples` gets a
    /// new `SceneRenderer`, which is what the viewer's reload path does.
    pub fn with_samples(device: &wgpu::Device, format: wgpu::TextureFormat, samples: u32) -> Self {
        let samples = samples.max(1);
        let multisample = wgpu::MultisampleState {
            count: samples,
            ..Default::default()
        };
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mesh-shader"),
            source: wgpu::ShaderSource::Wgsl(with_sky_common(include_str!("shaders/mesh.wgsl"))),
        });

        let uniform_layout = |label: &str, binding_size: Option<u64>| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some(label),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        // A dynamic offset only makes sense against a binding
                        // smaller than the buffer: the binding is one struct,
                        // the buffer is the whole array of them.
                        has_dynamic_offset: binding_size.is_some(),
                        min_binding_size: binding_size.and_then(std::num::NonZeroU64::new),
                    },
                    count: None,
                }],
            })
        };
        // One buffer holds every entity's uniforms; each draw selects its own
        // with a dynamic offset, so the whole draw list is one bind group and
        // one upload rather than one of each per entity.
        let object_layout = uniform_layout(
            "object-uniforms",
            Some(std::mem::size_of::<ObjectUniform>() as u64),
        );
        let frame_layout = uniform_layout("frame-uniforms", None);

        // Group 2: the shadow map and its comparison sampler. Always present
        // in the layout even when a scene casts no shadows — see `ShadowMap`.
        let shadow_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("shadow-map"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                    count: None,
                },
            ],
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mesh-pipeline-layout"),
            bind_group_layouts: &[
                Some(&object_layout),
                Some(&frame_layout),
                Some(&shadow_layout),
            ],
            immediate_size: 0,
        });

        // Position and normal live in separate buffers so a future mesh with no
        // normals does not need a padded interleaved layout.
        let vertex_layouts = [
            Some(wgpu::VertexBufferLayout {
                array_stride: 12,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &[wgpu::VertexAttribute {
                    offset: 0,
                    shader_location: 0,
                    format: wgpu::VertexFormat::Float32x3,
                }],
            }),
            Some(wgpu::VertexBufferLayout {
                array_stride: 12,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &[wgpu::VertexAttribute {
                    offset: 0,
                    shader_location: 1,
                    format: wgpu::VertexFormat::Float32x3,
                }],
            }),
        ];

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mesh-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &vertex_layouts,
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: Some(wgpu::Face::Back),
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample,
            multiview_mask: None,
            cache: None,
        });

        // The terrain twin of the mesh pipeline (M22): identical in every
        // respect except its shader module, which is `mesh.wgsl` with the
        // generative material spliced in by `with_terrain`.
        //
        // A second pipeline rather than a branch inside one, because the branch
        // was measured and it cost `m16_environment`, `m17_fire` and
        // `m18_water` one pixel each. Compiling the untouched file for
        // everything that is not terrain is the only way to be byte-identical
        // by construction rather than by hoping.
        let terrain_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("terrain-shader"),
            source: wgpu::ShaderSource::Wgsl(with_sky_common(&with_terrain(include_str!(
                "shaders/mesh.wgsl"
            )))),
        });
        let terrain_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("terrain-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &terrain_shader,
                entry_point: Some("vs_main"),
                buffers: &vertex_layouts,
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &terrain_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: Some(wgpu::Face::Back),
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample,
            multiview_mask: None,
            cache: None,
        });

        // The blended twin of the mesh pipeline: same shader, same layout,
        // same geometry. What differs is that it must not write depth (two
        // transparent surfaces have to blend with each other rather than the
        // nearer one masking the farther) and that its blend factors expect
        // the premultiplied color `fs_main` produces for these materials.
        let transparent_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mesh-transparent-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &vertex_layouts,
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: Some(wgpu::Face::Back),
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample,
            multiview_mask: None,
            cache: None,
        });

        let shadow_pipeline =
            Self::shadow_pipeline(device, &object_layout, &frame_layout, &vertex_layouts[..1]);
        let sky_pipeline = Self::sky_pipeline(device, &frame_layout, format, multisample);

        // Water (M18). Its own uniform, its own shader, the mesh pass's frame
        // and shadow bindings, plus the resolved scene depth at group 3.
        let water_layout = uniform_layout(
            "water-uniforms",
            Some(std::mem::size_of::<WaterUniform>() as u64),
        );
        let scene_depth_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("scene-depth"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    // Read with `textureLoad`: no sampler, so nothing filters
                    // depth across a silhouette.
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        });
        let water_pipeline = Self::water_pipeline(
            device,
            &water_layout,
            &frame_layout,
            &shadow_layout,
            &scene_depth_layout,
            // Position only: the grid's stored normals are the flat ones, and
            // the real normal comes from the wave derivatives.
            &vertex_layouts[..1],
            format,
            multisample,
        );
        // Clouds (M20). Its own uniform and shader, the mesh pass's frame
        // binding, and nothing else: no shadow map (the engine has one cascade
        // and it belongs to the ground) and no scene depth (a cloud is not
        // absorbing what is behind it).
        let cloud_layout = uniform_layout(
            "cloud-uniforms",
            Some(std::mem::size_of::<CloudUniform>() as u64),
        );
        let cloud_pipeline = Self::cloud_pipeline(
            device,
            &cloud_layout,
            &frame_layout,
            &vertex_layouts,
            format,
            multisample,
        );

        let (depth_resolve_pipeline, depth_source_layout) =
            Self::depth_resolve_pipeline(device, samples);

        let (hud_pipeline, hud_layout) = Self::hud_pipeline(device, format);

        let particle_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("particle-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/particles.wgsl").into()),
        });
        let particle_layout = uniform_layout("particle-uniforms", None);
        let particle_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("particle-pipeline-layout"),
                bind_group_layouts: &[Some(&particle_layout)],
                immediate_size: 0,
            });

        // One instance per particle; the quad corners come from vertex_index.
        let particle_vertex_layouts = [Some(wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<ParticleRaw>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &[
                wgpu::VertexAttribute {
                    offset: 0,
                    shader_location: 0,
                    format: wgpu::VertexFormat::Float32x4,
                },
                wgpu::VertexAttribute {
                    offset: 16,
                    shader_location: 1,
                    format: wgpu::VertexFormat::Float32x4,
                },
                wgpu::VertexAttribute {
                    offset: 32,
                    shader_location: 2,
                    format: wgpu::VertexFormat::Float32x4,
                },
            ],
        })];

        // Two pipelines, one shader, one instance buffer: the *only* difference
        // is the blend equation, and which particle uses which is a CPU-side
        // partition of the sorted draw list.
        //
        // `ALPHA_BLENDING` is `src·srcA + dst·(1-srcA)` — a sprite hides what
        // it covers. Additive is `src·srcA + dst·1` — it only ever adds light,
        // so a stack of flame sprites climbs toward white and the darkest a
        // flame can make anything is "unchanged". Doing this by shipping
        // premultiplied color through one pipeline (emitting alpha 0 for the
        // additive case) would also work and would save a pipeline, but it
        // would move the multiply by alpha from the blend unit into the shader
        // for *every* particle — and rearranging arithmetic that eleven
        // committed baselines depend on, to save one pipeline object, is the
        // wrong trade. Alpha-blended particles keep the exact pipeline they had.
        let particle_pipeline_for = |label: &str, blend: wgpu::BlendState| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&particle_pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &particle_shader,
                    entry_point: Some("vs_main"),
                    buffers: &particle_vertex_layouts,
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &particle_shader,
                    entry_point: Some("fs_main"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(blend),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                // Billboards always face the camera; culling would be a no-op at
                // best and a winding trap at worst.
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    cull_mode: None,
                    ..Default::default()
                },
                // Depth-test against the meshes but never write: translucent
                // sprites must not occlude each other (they are sorted and
                // blended instead).
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(false),
                    depth_compare: Some(wgpu::CompareFunction::Less),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample,
                multiview_mask: None,
                cache: None,
            })
        };
        let particle_pipeline =
            particle_pipeline_for("particle-pipeline", wgpu::BlendState::ALPHA_BLENDING);
        let additive_particle_pipeline = particle_pipeline_for(
            "particle-pipeline-additive",
            wgpu::BlendState {
                color: wgpu::BlendComponent {
                    src_factor: wgpu::BlendFactor::SrcAlpha,
                    dst_factor: wgpu::BlendFactor::One,
                    operation: wgpu::BlendOperation::Add,
                },
                // The scene target is opaque, so nothing reads this back; keep
                // it saturating rather than leaving it at whatever the default
                // would imply.
                alpha: wgpu::BlendComponent {
                    src_factor: wgpu::BlendFactor::One,
                    dst_factor: wgpu::BlendFactor::One,
                    operation: wgpu::BlendOperation::Add,
                },
            },
        );

        let frame_uniform = Uniforms::new(
            device,
            &frame_layout,
            "frame-uniform",
            std::mem::size_of::<FrameUniform>() as u64,
            None,
        );
        let particle_uniform = Uniforms::new(
            device,
            &particle_layout,
            "particle-frame-uniform",
            std::mem::size_of::<ParticleFrameUniform>() as u64,
            None,
        );

        // Dynamic offsets must land on the device's uniform alignment, so the
        // per-object stride is the struct size rounded up to it.
        let alignment = device.limits().min_uniform_buffer_offset_alignment as u64;
        let object_stride =
            std::mem::size_of::<ObjectUniform>().next_multiple_of(alignment as usize) as u64;

        let shadow_placeholder = ShadowMap::new(device, &shadow_layout, 1);

        let water_stride =
            std::mem::size_of::<WaterUniform>().next_multiple_of(alignment as usize) as u64;
        let cloud_stride =
            std::mem::size_of::<CloudUniform>().next_multiple_of(alignment as usize) as u64;

        Self {
            pipeline,
            transparent_pipeline,
            terrain_pipeline,
            shadow_pipeline,
            sky_pipeline,
            water_pipeline,
            cloud_pipeline,
            depth_resolve_pipeline,
            water_layout,
            cloud_layout,
            scene_depth_layout,
            depth_source_layout,
            particle_pipeline,
            additive_particle_pipeline,
            object_layout,
            hud_pipeline,
            hud_layout,
            shadow_layout,
            format,
            samples,
            shadow_placeholder,
            shadow_map: None,
            meshes: HashMap::new(),
            frame_uniform,
            objects: None,
            object_stride,
            water_objects: None,
            water_stride,
            cloud_objects: None,
            cloud_stride,
            scene_depth: None,
            depth_source: None,
            particle_instances: None,
            particle_uniform,
            hud_targets: Vec::new(),
            frame_index: 0,
        }
    }

    /// The depth-only caster pass (M16). No fragment stage and no color
    /// target: the rasterizer writing depth is the whole point.
    ///
    /// Culling is inverted relative to the mesh pass. Recording the *back* of
    /// each caster moves the stored depth away from the lit surface by the
    /// thickness of the object, which is a far better peeling margin than any
    /// constant bias, and it costs nothing.
    fn shadow_pipeline(
        device: &wgpu::Device,
        object_layout: &wgpu::BindGroupLayout,
        frame_layout: &wgpu::BindGroupLayout,
        vertex_layouts: &[Option<wgpu::VertexBufferLayout<'_>>],
    ) -> wgpu::RenderPipeline {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("shadow-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/shadow.wgsl").into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("shadow-pipeline-layout"),
            bind_group_layouts: &[Some(object_layout), Some(frame_layout)],
            immediate_size: 0,
        });

        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("shadow-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: vertex_layouts,
                compilation_options: Default::default(),
            },
            fragment: None,
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: Some(wgpu::Face::Front),
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: Default::default(),
                // A slope-scaled hardware bias on top of the shader's, which
                // is what keeps large ground-facing polygons from self-
                // shadowing in bands.
                bias: wgpu::DepthBiasState {
                    constant: 2,
                    slope_scale: 2.0,
                    clamp: 0.0,
                },
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        })
    }

    /// The procedural sky (M16): one fullscreen triangle, drawn before the
    /// meshes with the depth test passing always and depth writes off, so
    /// every mesh that follows simply covers it.
    fn sky_pipeline(
        device: &wgpu::Device,
        frame_layout: &wgpu::BindGroupLayout,
        format: wgpu::TextureFormat,
        multisample: wgpu::MultisampleState,
    ) -> wgpu::RenderPipeline {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("sky-shader"),
            source: wgpu::ShaderSource::Wgsl(with_sky_common(include_str!("shaders/sky.wgsl"))),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("sky-pipeline-layout"),
            bind_group_layouts: &[Some(frame_layout)],
            immediate_size: 0,
        });

        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("sky-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Always),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample,
            multiview_mask: None,
            cache: None,
        })
    }

    /// The water pass (M18): the blended twin of the mesh pipeline in how it
    /// composites, and nothing like it in what it draws.
    ///
    /// Two departures worth naming. It is **not culled**, because a water
    /// surface is a single sheet with no inside: back-face culling would delete
    /// it the moment a camera dipped below the waterline, and the fragment
    /// shader flips the normal toward the viewer instead. And like the
    /// transparent mesh pipeline it tests depth without writing it — two water
    /// surfaces at different heights have to blend, and a surface that wrote
    /// depth would also occlude the particles of its own spray.
    #[allow(clippy::too_many_arguments)]
    fn water_pipeline(
        device: &wgpu::Device,
        water_layout: &wgpu::BindGroupLayout,
        frame_layout: &wgpu::BindGroupLayout,
        shadow_layout: &wgpu::BindGroupLayout,
        scene_depth_layout: &wgpu::BindGroupLayout,
        vertex_layouts: &[Option<wgpu::VertexBufferLayout<'_>>],
        format: wgpu::TextureFormat,
        multisample: wgpu::MultisampleState,
    ) -> wgpu::RenderPipeline {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("water-shader"),
            source: wgpu::ShaderSource::Wgsl(with_sky_common(include_str!("shaders/water.wgsl"))),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("water-pipeline-layout"),
            bind_group_layouts: &[
                Some(water_layout),
                Some(frame_layout),
                Some(shadow_layout),
                Some(scene_depth_layout),
            ],
            immediate_size: 0,
        });

        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("water-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: vertex_layouts,
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample,
            multiview_mask: None,
            cache: None,
        })
    }

    /// The cloud pass (M20): blended like the water pass, and culled like
    /// neither of the others.
    ///
    /// **Culling is off**, and that is load-bearing twice over. A cloud has no
    /// inside, so back-face culling would delete it the instant the camera flew
    /// into one; and the far wall of every lobe is what the near wall is being
    /// blended *over*, which is the accumulation standing in for thickness.
    ///
    /// Depth is tested but never written, like every other blended thing here.
    /// Two clouds have to blend rather than the nearer one masking the farther,
    /// and a cloud that wrote depth would occlude the sky's own reflection in
    /// the water below it.
    fn cloud_pipeline(
        device: &wgpu::Device,
        cloud_layout: &wgpu::BindGroupLayout,
        frame_layout: &wgpu::BindGroupLayout,
        vertex_layouts: &[Option<wgpu::VertexBufferLayout<'_>>],
        format: wgpu::TextureFormat,
        multisample: wgpu::MultisampleState,
    ) -> wgpu::RenderPipeline {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("cloud-shader"),
            source: wgpu::ShaderSource::Wgsl(with_sky_common(include_str!("shaders/clouds.wgsl"))),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("cloud-pipeline-layout"),
            bind_group_layouts: &[Some(cloud_layout), Some(frame_layout)],
            immediate_size: 0,
        });

        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("cloud-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: vertex_layouts,
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample,
            multiview_mask: None,
            cache: None,
        })
    }

    /// The depth copy pass (M18): one fullscreen triangle turning the opaque
    /// pass's depth attachment into something the water shader can read.
    ///
    /// The source's binding type must match its sample count, and the shader
    /// text is patched accordingly — which is fine here because the sample count
    /// is baked into the renderer already (`with_samples`).
    fn depth_resolve_pipeline(
        device: &wgpu::Device,
        samples: u32,
    ) -> (wgpu::RenderPipeline, wgpu::BindGroupLayout) {
        let multisampled = samples > 1;
        let source_type = if multisampled {
            "texture_depth_multisampled_2d"
        } else {
            "texture_depth_2d"
        };
        let source = include_str!("shaders/depth_resolve.wgsl")
            .replace("SOURCE_TEXTURE_TYPE", source_type);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("depth-resolve-shader"),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        let source_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("depth-resolve-source"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Depth,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled,
                },
                count: None,
            }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("depth-resolve-layout"),
            bind_group_layouts: &[Some(&source_layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("depth-resolve-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::R32Float,
                    blend: None,
                    write_mask: wgpu::ColorWrites::RED,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            // The copy target is single-sampled however many samples the scene
            // draws with, so this pass is never multisampled.
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        (pipeline, source_layout)
    }

    /// The HUD overlay blit (M12): fullscreen triangle, no vertex buffers, no
    /// sampler (`textureLoad` — the canvas is 1:1 with target pixels, so
    /// nothing filters a glyph edge), straight-alpha blend over the lit scene.
    /// The canvas covers only the region the HUD touches, so the fetch is
    /// offset by that region's corner and a scissor rect bounds the triangle.
    fn hud_pipeline(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
    ) -> (wgpu::RenderPipeline, wgpu::BindGroupLayout) {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("hud-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/hud.wgsl").into()),
        });

        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("hud-overlay"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("hud-pipeline-layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("hud-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    // Straight alpha: the canvas stores unpremultiplied color,
                    // so alpha 1 replaces the scene byte exactly and alpha 0
                    // leaves it exactly.
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        (pipeline, layout)
    }

    pub fn format(&self) -> wgpu::TextureFormat {
        self.format
    }

    /// The MSAA sample count baked into this renderer's pipelines. A caller
    /// whose scene now asks for a different one has to build a new renderer.
    pub fn samples(&self) -> u32 {
        self.samples
    }

    /// Upload a draw list and render it.
    ///
    /// Geometry uploads once and is reused: `items` carry shared
    /// `Arc<MeshData>`, and this keeps the GPU buffers for each one alive
    /// across frames (see the module doc). Per-frame work is one uniform
    /// write per pass plus the draw calls themselves.
    pub fn draw(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, pass: ScenePass<'_>) {
        let ScenePass {
            target,
            msaa,
            depth,
            target_size,
            items,
            water,
            clouds,
            particles,
            view_projection,
            camera_position,
            camera_right,
            camera_up,
            lights,
            environment,
            time,
            clear,
            hud,
        } = pass;

        self.frame_index += 1;

        // Shadows need a real map; allocate it the first time any scene asks.
        if environment.shadows && self.shadow_map.is_none() {
            self.shadow_map = Some(ShadowMap::new(
                device,
                &self.shadow_layout,
                SHADOW_MAP_SIZE,
            ));
        }
        let light_view_proj = if environment.shadows {
            light_view_projection(
                lights.sun_direction,
                camera_position,
                view_projection,
                environment.shadow_distance,
                SHADOW_MAP_SIZE,
            )
        } else {
            Mat4::IDENTITY
        };

        // Point lights pack into the fixed-size array in the order
        // `ResolvedLights` produced (entity-name order). Validation caps the
        // count, so `take` here is a belt-and-braces bound on an already-valid
        // scene rather than a silent truncation policy.
        let mut point_lights = [PointLightUniform::default(); MAX_POINT_LIGHTS];
        let point_light_count = lights.live_points().len();
        for (slot, light) in point_lights.iter_mut().zip(lights.live_points()) {
            *slot = PointLightUniform {
                position_range: light.position.extend(light.range).to_array(),
                color: light.color.extend(0.0).to_array(),
            };
        }

        let frame = FrameUniform {
            camera_pos: camera_position.extend(1.0).to_array(),
            sun_direction: lights.sun_direction.extend(0.0).to_array(),
            sun_color: lights.sun_color.extend(1.0).to_array(),
            ambient: lights.ambient.extend(1.0).to_array(),
            inv_view_proj: view_projection.inverse().to_cols_array_2d(),
            light_view_proj: light_view_proj.to_cols_array_2d(),
            sky_zenith: environment.sky_zenith.extend(1.0).to_array(),
            sky_horizon: environment.sky_horizon.extend(1.0).to_array(),
            sky_ground: environment.sky_ground.extend(1.0).to_array(),
            params: [
                environment.fog_density,
                if environment.shadows { 1.0 } else { 0.0 },
                1.0 / SHADOW_MAP_SIZE as f32,
                if environment.sky { 1.0 } else { 0.0 },
            ],
            params2: [point_light_count as f32, 0.0, 0.0, 0.0],
            point_lights,
        };
        queue.write_buffer(&self.frame_uniform.buffer, 0, bytemuck::bytes_of(&frame));

        // Geometry first: anything new joins the cache, anything already there
        // is just touched. `keys` then addresses the cache during the pass
        // without hashing an `Arc` pointer twice.
        let keys: Vec<usize> = items
            .iter()
            .map(|item| self.upload_mesh(device, &item.mesh))
            .collect();

        // Every entity's uniforms in one buffer, one write, addressed during
        // the pass by dynamic offset.
        let stride = self.object_stride as usize;
        let mut object_bytes = vec![0u8; stride * items.len()];
        for (index, item) in items.iter().enumerate() {
            let material = &item.material;
            let uniform = ObjectUniform {
                mvp: (view_projection * item.model).to_cols_array_2d(),
                model: item.model.to_cols_array_2d(),
                normal_matrix: item.model.inverse().transpose().to_cols_array_2d(),
                albedo_metallic: material.albedo.extend(material.metallic).to_array(),
                emissive_roughness: material.emissive.extend(material.roughness).to_array(),
                surface: [material.alpha, material.transmission, 0.0, 0.0],
                terrain: match &item.terrain {
                    // Zero layers is the "not terrain" signal, and it is the
                    // only thing the shader tests: every mesh drawn since M4
                    // lands here and never executes a line of the terrain path.
                    Some(t) => [
                        t.layers.len().min(MAX_TERRAIN_LAYERS) as f32,
                        t.texture_scale,
                        t.color_variation,
                        t.bump,
                    ],
                    None => [0.0; 4],
                },
                terrain_seed: [item.terrain.as_ref().map_or(0, |t| t.seed), 0, 0, 0],
                terrain_layers: terrain_layers(item.terrain.as_ref()),
            };
            let at = index * stride;
            object_bytes[at..at + std::mem::size_of::<ObjectUniform>()]
                .copy_from_slice(bytemuck::bytes_of(&uniform));
        }
        if !object_bytes.is_empty() {
            let objects = Uniforms::ensure(
                &mut self.objects,
                device,
                &self.object_layout,
                "object-uniforms",
                object_bytes.len() as u64,
                Some(std::mem::size_of::<ObjectUniform>() as u64),
            );
            queue.write_buffer(&objects.buffer, 0, &object_bytes);
        }

        // Water surfaces: their grids join the same geometry cache (one upload
        // per tessellation for the life of the run, since `surface_grid` hands
        // back the same `Arc` every frame), and their uniforms the same
        // one-buffer-addressed-by-dynamic-offset arrangement.
        let water_keys: Vec<usize> = water
            .iter()
            .map(|item| self.upload_mesh(device, &item.mesh))
            .collect();
        if !water.is_empty() {
            let stride = self.water_stride as usize;
            let mut water_bytes = vec![0u8; stride * water.len()];
            for (index, item) in water.iter().enumerate() {
                let uniform = water_uniform(item, view_projection, time);
                let at = index * stride;
                water_bytes[at..at + std::mem::size_of::<WaterUniform>()]
                    .copy_from_slice(bytemuck::bytes_of(&uniform));
            }
            let objects = Uniforms::ensure(
                &mut self.water_objects,
                device,
                &self.water_layout,
                "water-uniforms",
                water_bytes.len() as u64,
                Some(std::mem::size_of::<WaterUniform>() as u64),
            );
            queue.write_buffer(&objects.buffer, 0, &water_bytes);
        }

        // Clouds: the same arrangement again. Their lobe clusters join the
        // geometry cache (one upload per distinct cloud for the life of the
        // run, since `cloud::mesh_for` hands back the same `Arc` every frame —
        // drift is a shader-side translation precisely so that stays true).
        let cloud_keys: Vec<usize> = clouds
            .iter()
            .map(|item| self.upload_mesh(device, &item.mesh))
            .collect();
        if !clouds.is_empty() {
            let stride = self.cloud_stride as usize;
            let mut cloud_bytes = vec![0u8; stride * clouds.len()];
            for (index, item) in clouds.iter().enumerate() {
                let uniform = cloud_uniform(item, view_projection, time);
                let at = index * stride;
                cloud_bytes[at..at + std::mem::size_of::<CloudUniform>()]
                    .copy_from_slice(bytemuck::bytes_of(&uniform));
            }
            let objects = Uniforms::ensure(
                &mut self.cloud_objects,
                device,
                &self.cloud_layout,
                "cloud-uniforms",
                cloud_bytes.len() as u64,
                Some(std::mem::size_of::<CloudUniform>() as u64),
            );
            queue.write_buffer(&objects.buffer, 0, &cloud_bytes);
        }

        // Split the draw list by blend mode. Opaque keeps file order (it is
        // depth-tested, so order does not matter and stability is worth more);
        // everything blended sorts back-to-front, because blending does not
        // commute. The tiebreak on entity name keeps two surfaces at the same
        // distance in an order that does not depend on how the world happened
        // to iterate.
        //
        // Water sorts in the *same* list as the transparent meshes rather than
        // in a pass of its own: an ice block floating in a pond is transparent
        // geometry inside a water surface, and two separate passes would fix
        // which of the two always draws over the other.
        let opaque: Vec<usize> = (0..items.len())
            .filter(|&i| !items[i].material.is_transparent())
            .collect();
        let mut blended: Vec<Blended> = (0..items.len())
            .filter(|&i| items[i].material.is_transparent())
            .map(Blended::Mesh)
            .chain((0..water.len()).map(Blended::Water))
            .chain((0..clouds.len()).map(Blended::Cloud))
            .collect();
        let sort_key = |entry: &Blended| -> (f32, &str) {
            match *entry {
                Blended::Mesh(i) => (
                    (items[i].model.w_axis.truncate() - camera_position).length_squared(),
                    items[i].entity.as_str(),
                ),
                // A surface's centre stands in for the whole sheet, which is
                // the same approximation the meshes use and is wrong in the
                // same way: two overlapping *large* transparent things sort by
                // their origins, not per pixel.
                Blended::Water(i) => (
                    (water[i].model.w_axis.truncate() - camera_position).length_squared(),
                    water[i].entity.as_str(),
                ),
                // Clouds sort by their origin like everything else here, and a
                // cloud is large, so two overlapping ones sort as wholes rather
                // than per pixel. That is the same approximation the meshes and
                // the water make, and clouds are the case where it is most
                // forgiving: two of them at similar distance are both nearly
                // the same colour.
                Blended::Cloud(i) => (
                    (clouds[i].model.w_axis.truncate() - camera_position).length_squared(),
                    clouds[i].entity.as_str(),
                ),
            }
        };
        blended.sort_by(|a, b| {
            let (da, na) = sort_key(a);
            let (db, nb) = sort_key(b);
            db.total_cmp(&da).then_with(|| na.cmp(nb))
        });

        // Translucent billboards sort back-to-front (blending is order
        // dependent) and upload as one instance buffer. Distance to the
        // camera stands in for view depth — correct enough for sprites, and
        // `total_cmp` plus a stable sort keeps the order deterministic.
        //
        // Additive sprites then move to the back of the buffer as one
        // contiguous run, so the pass is two draws over one buffer rather than
        // a pipeline switch per sprite. `sort_by_key` is stable, so each group
        // keeps the back-to-front order the distance sort just gave it — which
        // additive blending does not need (it commutes) but alpha does.
        let (particle_count, alpha_particles) = if particles.is_empty() {
            (0, 0)
        } else {
            let mut sorted: Vec<&ParticleInstance> = particles.iter().collect();
            sorted.sort_by(|a, b| {
                let da = (a.position - camera_position).length_squared();
                let db = (b.position - camera_position).length_squared();
                db.total_cmp(&da)
            });
            sorted.sort_by_key(|p| p.blend == ParticleBlend::Additive);
            let alpha_particles = sorted
                .iter()
                .take_while(|p| p.blend != ParticleBlend::Additive)
                .count() as u32;
            let raw: Vec<ParticleRaw> = sorted
                .iter()
                .map(|p| ParticleRaw {
                    pos_size: p.position.extend(p.size).to_array(),
                    color: p.color.extend(p.alpha).to_array(),
                    velocity_stretch: p.velocity.extend(p.stretch).to_array(),
                })
                .collect();

            let bytes: &[u8] = bytemuck::cast_slice(&raw);
            let buffer = grow_buffer(
                &mut self.particle_instances,
                device,
                "particle-instances",
                wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                bytes.len() as u64,
            );
            queue.write_buffer(buffer, 0, bytes);

            let uniform = ParticleFrameUniform {
                view_proj: view_projection.to_cols_array_2d(),
                camera_right: camera_right.normalize_or_zero().extend(0.0).to_array(),
                camera_up: camera_up.normalize_or_zero().extend(0.0).to_array(),
                camera_pos: camera_position.extend(environment.fog_density).to_array(),
                fog_color: environment.sky_horizon.extend(1.0).to_array(),
            };
            queue.write_buffer(
                &self.particle_uniform.buffer,
                0,
                bytemuck::bytes_of(&uniform),
            );
            (raw.len() as u32, alpha_particles)
        };

        // The overlay canvas covers only the pixels the HUD touches; upload it
        // into the top-left corner of the (cached, at-least-that-big) texture
        // and tell the shader where those pixels belong on screen.
        let hud_canvases: &[crate::hud::HudCanvas] = hud.map_or(&[], |hud| &hud.canvases);
        let hud_canvases: Vec<&crate::hud::HudCanvas> = hud_canvases
            .iter()
            .filter(|canvas| !canvas.is_empty())
            .collect();
        for (index, canvas) in hud_canvases.iter().enumerate() {
            let hud_target = HudTarget::ensure(
                &mut self.hud_targets,
                index,
                device,
                &self.hud_layout,
                canvas.width,
                canvas.height,
            );
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &hud_target.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &canvas.pixels,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    // `write_texture` takes tight rows; the 256-byte alignment
                    // rule is buffer↔texture copies only.
                    bytes_per_row: Some(canvas.width * 4),
                    rows_per_image: None,
                },
                wgpu::Extent3d {
                    width: canvas.width,
                    height: canvas.height,
                    depth_or_array_layers: 1,
                },
            );
            queue.write_buffer(
                &hud_target.placement,
                0,
                bytemuck::cast_slice(&[canvas.origin_x as i32, canvas.origin_y as i32, 0, 0]),
            );
        }

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("scene-encoder"),
        });

        // The caster pass, before anything is shaded: the mesh pass samples
        // what it writes. Only opaque geometry casts — a transparent surface
        // that shadowed as if it were solid would be worse than one that does
        // not shadow at all.
        if environment.shadows {
            let shadow_map = self.shadow_map.as_ref().expect("allocated above");
            let mut shadow_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("shadow-pass"),
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &shadow_map.view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            if let Some(objects) = &self.objects {
                shadow_pass.set_pipeline(&self.shadow_pipeline);
                shadow_pass.set_bind_group(1, &self.frame_uniform.bind_group, &[]);
                for &index in &opaque {
                    let mesh = &self.meshes[&keys[index]];
                    shadow_pass.set_bind_group(
                        0,
                        &objects.bind_group,
                        &[(index as u64 * self.object_stride) as u32],
                    );
                    shadow_pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                    shadow_pass
                        .set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                    shadow_pass.draw_indexed(0..mesh.index_count, 0, 0..1);
                }
            }
        }

        // With MSAA the multisampled texture is what gets drawn into and
        // `target` receives the resolve; without it, `target` is drawn into
        // directly, exactly as it always was.
        let (color_view, resolve_target) = match msaa {
            Some(msaa) => (msaa, Some(target)),
            None => (target, None),
        };

        // Water splits the frame in two: the opaque geometry has to be finished
        // and its depth readable before the water can absorb what is behind it.
        // A scene with no water keeps the single pass it always had — same
        // attachments, same load and store ops, same draws — which is what
        // keeps every baseline blessed before this milestone bit-exact.
        let water_present = !water.is_empty();
        if water_present {
            SceneDepth::ensure(
                &mut self.scene_depth,
                device,
                &self.scene_depth_layout,
                target_size[0],
                target_size[1],
            );
            // The source view changes every frame in the viewer (a new
            // swapchain-sized depth texture on resize) and cannot be compared
            // for identity, so this bind group is rebuilt rather than cached.
            // It is one small allocation per frame, against the per-entity
            // churn M15 removed.
            self.depth_source = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("depth-resolve-source"),
                layout: &self.depth_source_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(depth),
                }],
            }));
        }

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("scene-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: color_view,
                    // With water the resolve happens at the end of the *water*
                    // pass instead, so the multisampled color survives to be
                    // drawn into again.
                    resolve_target: if water_present { None } else { resolve_target },
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(clear),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: if water_present {
                            wgpu::StoreOp::Store
                        } else {
                            wgpu::StoreOp::Discard
                        },
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            // The sky first, filling every pixel the geometry will not.
            if environment.sky {
                pass.set_pipeline(&self.sky_pipeline);
                pass.set_bind_group(0, &self.frame_uniform.bind_group, &[]);
                pass.draw(0..3, 0..1);
            }

            let shadows = match (environment.shadows, &self.shadow_map) {
                (true, Some(map)) => map,
                _ => &self.shadow_placeholder,
            };

            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(1, &self.frame_uniform.bind_group, &[]);
            pass.set_bind_group(2, &shadows.bind_group, &[]);

            if let Some(objects) = &self.objects {
                let draw = |pass: &mut wgpu::RenderPass<'_>, index: usize| {
                    let mesh = &self.meshes[&keys[index]];
                    pass.set_bind_group(
                        0,
                        &objects.bind_group,
                        &[(index as u64 * self.object_stride) as u32],
                    );
                    pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                    pass.set_vertex_buffer(1, mesh.vertices.slice(mesh.normals_offset..));
                    pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..mesh.index_count, 0, 0..1);
                };

                // Terrain draws in one run at the end, so the pipeline switches
                // at most once a frame rather than once an entity. Both
                // pipelines write depth and neither blends, so ordering within
                // the opaque pass cannot change a pixel — and a scene with no
                // terrain never leaves `self.pipeline`.
                for &index in &opaque {
                    if items[index].terrain.is_none() {
                        draw(&mut pass, index);
                    }
                }
                let mut switched = false;
                for &index in &opaque {
                    if items[index].terrain.is_some() {
                        if !switched {
                            pass.set_pipeline(&self.terrain_pipeline);
                            switched = true;
                        }
                        draw(&mut pass, index);
                    }
                }
                if switched {
                    pass.set_pipeline(&self.pipeline);
                }
            }

            // Blended geometry after every opaque surface has written depth, so
            // it is occluded by what is in front of it, and back-to-front among
            // itself. With water in the scene this waits for the second pass,
            // where the depth behind the water is readable.
            if !water_present {
                self.draw_blended(&mut pass, &blended, &keys, &water_keys, &cloud_keys, shadows);
                self.draw_particles(&mut pass, particle_count, alpha_particles);
            }
        }

        if water_present {
            let scene_depth = self.scene_depth.as_ref().expect("ensured above");
            {
                let mut resolve = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("depth-resolve-pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &scene_depth.view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            // Every pixel is written, so the load is discarded
                            // rather than cleared.
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                        depth_slice: None,
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                resolve.set_pipeline(&self.depth_resolve_pipeline);
                resolve.set_bind_group(0, self.depth_source.as_ref().expect("ensured above"), &[]);
                resolve.draw(0..3, 0..1);
            }

            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("water-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: color_view,
                    resolve_target,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            let shadows = match (environment.shadows, &self.shadow_map) {
                (true, Some(map)) => map,
                _ => &self.shadow_placeholder,
            };
            self.draw_blended(&mut pass, &blended, &keys, &water_keys, &cloud_keys, shadows);
            self.draw_particles(&mut pass, particle_count, alpha_particles);
        }

        if !hud_canvases.is_empty() {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("hud-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.hud_pipeline);
            for (index, canvas) in hud_canvases.iter().enumerate() {
                pass.set_bind_group(0, &self.hud_targets[index].bind_group, &[]);
                // The fullscreen triangle would otherwise shade the whole
                // frame; outside this canvas there is nothing to composite.
                pass.set_scissor_rect(
                    canvas.origin_x,
                    canvas.origin_y,
                    canvas.width,
                    canvas.height,
                );
                pass.draw(0..3, 0..1);
            }
        }

        queue.submit(Some(encoder.finish()));

        let frame_index = self.frame_index;
        self.meshes
            .retain(|_, mesh| frame_index - mesh.last_used < MESH_CACHE_LIFETIME);
    }

    /// Draw the blended list — transparent meshes and water surfaces
    /// interleaved, back-to-front.
    ///
    /// Switching between the pipelines re-binds their groups, because the three
    /// pipeline layouts differ (water has a fourth group, clouds have only
    /// two) and a pipeline change with an incompatible layout invalidates what
    /// was bound. Tracking the previous kind keeps that to one switch per run
    /// of same-kind items.
    fn draw_blended(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        blended: &[Blended],
        keys: &[usize],
        water_keys: &[usize],
        cloud_keys: &[usize],
        shadows: &ShadowMap,
    ) {
        // 0 = transparent mesh, 1 = water, 2 = cloud.
        let mut current: Option<u8> = None;
        for entry in blended {
            match *entry {
                Blended::Mesh(index) => {
                    let Some(objects) = &self.objects else { continue };
                    if current != Some(0) {
                        pass.set_pipeline(&self.transparent_pipeline);
                        pass.set_bind_group(1, &self.frame_uniform.bind_group, &[]);
                        pass.set_bind_group(2, &shadows.bind_group, &[]);
                        current = Some(0);
                    }
                    let mesh = &self.meshes[&keys[index]];
                    pass.set_bind_group(
                        0,
                        &objects.bind_group,
                        &[(index as u64 * self.object_stride) as u32],
                    );
                    pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                    pass.set_vertex_buffer(1, mesh.vertices.slice(mesh.normals_offset..));
                    pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..mesh.index_count, 0, 0..1);
                }
                Blended::Water(index) => {
                    let (Some(surfaces), Some(scene_depth)) =
                        (&self.water_objects, &self.scene_depth)
                    else {
                        continue;
                    };
                    if current != Some(1) {
                        pass.set_pipeline(&self.water_pipeline);
                        pass.set_bind_group(1, &self.frame_uniform.bind_group, &[]);
                        pass.set_bind_group(2, &shadows.bind_group, &[]);
                        pass.set_bind_group(3, &scene_depth.bind_group, &[]);
                        current = Some(1);
                    }
                    let mesh = &self.meshes[&water_keys[index]];
                    pass.set_bind_group(
                        0,
                        &surfaces.bind_group,
                        &[(index as u64 * self.water_stride) as u32],
                    );
                    // One vertex buffer: the wave derivatives are the normal.
                    pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                    pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..mesh.index_count, 0, 0..1);
                }
                Blended::Cloud(index) => {
                    let Some(objects) = &self.cloud_objects else { continue };
                    if current != Some(2) {
                        pass.set_pipeline(&self.cloud_pipeline);
                        pass.set_bind_group(1, &self.frame_uniform.bind_group, &[]);
                        current = Some(2);
                    }
                    let mesh = &self.meshes[&cloud_keys[index]];
                    pass.set_bind_group(
                        0,
                        &objects.bind_group,
                        &[(index as u64 * self.cloud_stride) as u32],
                    );
                    pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                    pass.set_vertex_buffer(1, mesh.vertices.slice(mesh.normals_offset..));
                    pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..mesh.index_count, 0, 0..1);
                }
            }
        }
    }

    /// Draw the particle billboards, last of everything: they test against the
    /// depth the meshes wrote and blend over whatever is already there,
    /// including the water.
    fn draw_particles(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        particle_count: u32,
        alpha_particles: u32,
    ) {
        if particle_count == 0 {
            return;
        }
        let instances = self.particle_instances.as_ref().expect("just written");
        pass.set_bind_group(0, &self.particle_uniform.bind_group, &[]);
        pass.set_vertex_buffer(0, instances.slice(..));
        // Alpha first, additive after: a flame reads as glowing *through* the
        // smoke above it, which is what firelight scattering in that smoke
        // actually looks like. A scene with no additive emitter issues exactly
        // the one draw it always did.
        if alpha_particles > 0 {
            pass.set_pipeline(&self.particle_pipeline);
            pass.draw(0..6, 0..alpha_particles);
        }
        if particle_count > alpha_particles {
            pass.set_pipeline(&self.additive_particle_pipeline);
            pass.draw(0..6, alpha_particles..particle_count);
        }
    }

    /// Upload `geometry` if this is the first time it has been seen, and
    /// return its cache key (the shared allocation's address).
    fn upload_mesh(&mut self, device: &wgpu::Device, geometry: &Arc<MeshData>) -> usize {
        let key = Arc::as_ptr(geometry) as usize;
        let frame_index = self.frame_index;
        self.meshes
            .entry(key)
            .and_modify(|mesh| mesh.last_used = frame_index)
            .or_insert_with(|| {
                // Positions and normals share one buffer, positions first, so
                // a single allocation serves both vertex slots.
                let mut vertex_bytes =
                    Vec::with_capacity((geometry.positions.len() + geometry.normals.len()) * 12);
                vertex_bytes.extend_from_slice(bytemuck::cast_slice(&geometry.positions));
                let normals_offset = vertex_bytes.len() as u64;
                vertex_bytes.extend_from_slice(bytemuck::cast_slice(&geometry.normals));

                CachedMesh {
                    _geometry: Arc::clone(geometry),
                    vertices: buffer_with(
                        device,
                        "mesh-vertices",
                        wgpu::BufferUsages::VERTEX,
                        &vertex_bytes,
                    ),
                    normals_offset,
                    indices: buffer_with(
                        device,
                        "mesh-indices",
                        wgpu::BufferUsages::INDEX,
                        bytemuck::cast_slice(&geometry.indices),
                    ),
                    index_count: geometry.indices.len() as u32,
                    last_used: frame_index,
                }
            });
        key
    }

    /// How many meshes are currently uploaded — the cache's observable
    /// behavior, for tests.
    pub fn cached_mesh_count(&self) -> usize {
        self.meshes.len()
    }
}

impl Uniforms {
    /// `binding_size` is the size of one binding when the buffer holds an
    /// array addressed by dynamic offset; `None` binds the whole buffer.
    fn new(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        label: &str,
        size: u64,
        binding_size: Option<u64>,
    ) -> Self {
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: size.max(wgpu::COPY_BUFFER_ALIGNMENT),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(label),
            layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &buffer,
                    offset: 0,
                    size: binding_size.and_then(std::num::NonZeroU64::new),
                }),
            }],
        });
        Self {
            buffer,
            bind_group,
            size,
        }
    }

    /// The uniforms at `slot`, allocated or grown to hold `size` bytes. Only
    /// a growth reallocates — a steady-state frame reuses everything.
    fn ensure<'a>(
        slot: &'a mut Option<Self>,
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        label: &str,
        size: u64,
        binding_size: Option<u64>,
    ) -> &'a Self {
        if slot.as_ref().is_none_or(|held| held.size < size) {
            *slot = Some(Self::new(device, layout, label, size, binding_size));
        }
        slot.as_ref().expect("just ensured")
    }
}

impl HudTarget {
    /// The overlay texture, allocated or grown to hold a `width × height`
    /// canvas. Canvases shrink and grow with the HUD's content, so the
    /// texture keeps the largest size seen and writes smaller canvases into
    /// its corner; the shader only ever reads the written region.
    fn ensure<'a>(
        targets: &'a mut Vec<Self>,
        index: usize,
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        width: u32,
        height: u32,
    ) -> &'a Self {
        let held = targets.get(index);
        let fits = held.is_some_and(|held| held.width >= width && held.height >= height);
        if !fits {
            let (width, height) = match held {
                Some(held) => (held.width.max(width), held.height.max(height)),
                None => (width, height),
            };
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("hud-overlay"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                // sRGB like the render target: `textureLoad` decodes, the
                // target re-encodes, and the round trip is byte-exact.
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let placement = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("hud-placement"),
                size: 16,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("hud-bind-group"),
                layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(
                            &texture.create_view(&wgpu::TextureViewDescriptor::default()),
                        ),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: placement.as_entire_binding(),
                    },
                ],
            });
            let target = Self {
                texture,
                bind_group,
                placement,
                width,
                height,
            };
            match targets.get_mut(index) {
                Some(slot) => *slot = target,
                // Canvas counts only grow by one at a time as the HUD gains
                // separated elements, so the index is always the next slot.
                None => targets.insert(index.min(targets.len()), target),
            }
        }
        &targets[index]
    }
}

/// One entry in the back-to-front blended list: an index into the draw list's
/// transparent meshes, its water surfaces, or its clouds.
#[derive(Clone, Copy)]
enum Blended {
    Mesh(usize),
    Water(usize),
    Cloud(usize),
}

/// Pack one cloud's shading parameters for the cloud shader (M20).
///
/// Everything here is a straight copy out of the component. The only thing
/// worth naming is what is *absent*: no time is folded into the model matrix,
/// because `drift` is applied in the vertex stage instead — which is what keeps
/// `Scene::cloud_items` a pure function of the file and the grown mesh's `Arc`
/// stable across frames, so the renderer uploads each cloud once.
fn cloud_uniform(item: &CloudItem, view_projection: Mat4, time: f32) -> CloudUniform {
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

/// Pack a terrain's layer table for the mesh shader (M22), zeroed for every
/// other draw.
///
/// Slope arrives in degrees and stays in degrees: the shader compares it against
/// an angle it derives with `acos`, and keeping the file's unit all the way to
/// the comparison is what makes `slope_range: [30, 90]` mean what it reads as.
fn terrain_layers(terrain: Option<&Terrain>) -> [TerrainLayerUniform; MAX_TERRAIN_LAYERS] {
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
        slot.blend_noise = [
            layer.height_blend,
            layer.noise,
            layer.slope_blend,
            0.0,
        ];
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
fn water_uniform(item: &WaterItem, view_projection: Mat4, time: f32) -> WaterUniform {
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
        clock: [count as f32, time, 0.0, 0.0],
        waves,
    }
}

/// Prepend the shared sky gradient to a shader source.
///
/// WGSL has no `#include` and wgpu has no preprocessor, so the sky pass and
/// the mesh pass share `sky_gradient` by concatenation. They have to share it:
/// the mesh pass reflects the sky off metal and water, and a reflection drawn
/// from a second copy of the curve would drift away from the sky behind it the
/// first time either was touched.
fn with_sky_common(source: &str) -> std::borrow::Cow<'static, str> {
    let mut combined = String::with_capacity(source.len() + 1024);
    combined.push_str(include_str!("shaders/sky_common.wgsl"));
    combined.push('\n');
    combined.push_str(source);
    std::borrow::Cow::Owned(combined)
}

/// The mesh shader with terrain's generative material spliced in (M22).
///
/// Terrain is lit exactly like a mesh — the same GGX lobe, shadow lookup, sky
/// ambient, point lights and fog — so it shares `mesh.wgsl` rather than
/// duplicating two hundred lines that would then have to stay in lockstep
/// forever. What it may not do is *edit* that file: M16's four untouchable lines
/// have to reach the compiler surrounded by the code they already shipped in,
/// and putting the branch inline moved one pixel by one unit in each of three
/// committed fixtures (see `shaders/terrain.wgsl`).
///
/// So the file on disk stays the pre-M22 one, the plain mesh pipeline compiles
/// it unchanged — byte-identical by construction, not by measurement — and this
/// builds the terrain variant by two anchored substitutions. Both anchors are
/// asserted: if `mesh.wgsl` is ever reworded, this fails loudly at startup
/// rather than silently rendering terrain as flat grey.
fn with_terrain(source: &str) -> std::borrow::Cow<'static, str> {
    // 1. The object uniform grows the layer table at its end, where it leaves
    //    every prior field at the offset the shader already reads it from.
    const UNIFORM_TAIL: &str = "    // x = alpha, y = transmission; z and w unused.\n\
                                \x20   surface: vec4<f32>,\n\
                                };\n";
    // 2. The fragment prologue resolves its surface through the generator
    //    instead of reading the material directly.
    const PROLOGUE: &str = "    let albedo = object.albedo_metallic.rgb;\n\
                            \x20   let metallic = object.albedo_metallic.w;\n\
                            \x20   let emissive = object.emissive_roughness.rgb;\n";
    const NORMAL: &str = "    let n = normalize(in.normal);\n";
    const ROUGHNESS: &str = "    let roughness = max(object.emissive_roughness.w, 0.045);\n";

    let mut out = source.to_string();
    for (what, anchor) in [
        ("the object uniform's tail", UNIFORM_TAIL),
        ("the fragment prologue", PROLOGUE),
        ("the surface normal", NORMAL),
        ("the roughness floor", ROUGHNESS),
    ] {
        assert_eq!(
            source.matches(anchor).count(),
            1,
            "mesh.wgsl no longer contains {what} exactly once; \
             with_terrain splices against it and must be updated with it"
        );
    }

    out = out.replace(
        UNIFORM_TAIL,
        "    // x = alpha, y = transmission; z and w unused.\n\
         \x20   surface: vec4<f32>,\n\
         \x20   // Terrain (M22), appended at the end so every field above keeps\n\
         \x20   // the offset the shader already reads it from. x = live layer\n\
         \x20   // count, y = texture scale in metres, z = colour variation,\n\
         \x20   // w = bump.\n\
         \x20   terrain: vec4<f32>,\n\
         \x20   // x = the terrain's seed; y, z, w unused.\n\
         \x20   terrain_seed: vec4<u32>,\n\
         \x20   terrain_layers: array<TerrainLayer, MAX_TERRAIN_LAYERS>,\n\
         };\n",
    );

    // The generator's own declarations go ahead of the uniform that now holds
    // its layer table.
    out = out.replace(
        "struct ObjectUniform {",
        &format!(
            "{}\nstruct ObjectUniform {{",
            include_str!("shaders/terrain.wgsl")
        ),
    );

    out = out.replace(
        PROLOGUE,
        "    let generated = terrain_surface(\n\
         \x20       in.world_position,\n\
         \x20       normalize(in.normal),\n\
         \x20       length(frame.camera_pos.xyz - in.world_position),\n\
         \x20       object.albedo_metallic.rgb,\n\
         \x20       object.emissive_roughness.w,\n\
         \x20   );\n\
         \x20   let albedo = generated.albedo;\n\
         \x20   let metallic = object.albedo_metallic.w;\n\
         \x20   let emissive = object.emissive_roughness.rgb;\n",
    );
    out = out.replace(NORMAL, "    let n = generated.normal;\n");
    out = out.replace(
        ROUGHNESS,
        "    let roughness = max(generated.roughness, 0.045);\n",
    );

    std::borrow::Cow::Owned(out)
}

/// A buffer holding `contents`, created once and never rewritten — the shape
/// mesh geometry wants.
fn buffer_with(
    device: &wgpu::Device,
    label: &str,
    usage: wgpu::BufferUsages,
    contents: &[u8],
) -> wgpu::Buffer {
    use wgpu::util::DeviceExt as _;
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(label),
        contents,
        usage,
    })
}

/// The buffer at `slot`, allocated or grown to hold `size` bytes. Growth
/// doubles, so a particle system that ramps up settles after a few frames
/// instead of reallocating on every new particle.
fn grow_buffer<'a>(
    slot: &'a mut Option<wgpu::Buffer>,
    device: &wgpu::Device,
    label: &str,
    usage: wgpu::BufferUsages,
    size: u64,
) -> &'a wgpu::Buffer {
    if slot.as_ref().is_none_or(|held| held.size() < size) {
        let capacity = slot
            .as_ref()
            .map_or(size, |held| (held.size() * 2).max(size))
            .max(wgpu::COPY_BUFFER_ALIGNMENT);
        *slot = Some(device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: capacity,
            usage,
            mapped_at_creation: false,
        }));
    }
    slot.as_ref().expect("just ensured")
}

/// Build the view-projection matrix for a camera.
///
/// The camera looks down its local -Z with +Y up, the usual right-handed
/// convention. `glam::Mat4::perspective_rh` produces a 0..1 depth range, which
/// is what wgpu expects — `perspective_rh_gl` would silently halve the usable
/// depth precision.
pub fn view_projection(camera: &Camera, camera_model: Mat4, aspect: f32) -> Mat4 {
    // `directx` is glam's name for the DirectX/WebGPU convention: Z in [0, 1]
    // and Y-up. The `vulkan` module is also [0, 1] but Y-down, which would
    // render the image upside down.
    let projection = glam::camera::rh::proj::directx::perspective(
        camera.fov.to_radians(),
        aspect.max(f32::EPSILON),
        camera.near,
        camera.far,
    );
    projection * camera_model.inverse()
}

/// Create the depth texture for a target of this size.
pub fn depth_texture(device: &wgpu::Device, width: u32, height: u32) -> wgpu::TextureView {
    depth_texture_multisampled(device, width, height, 1)
}

/// The depth texture for a target of this size at `samples`-way MSAA.
///
/// A render pass requires every attachment to agree on sample count, so this
/// has to match whatever color attachment is actually drawn into — the
/// multisampled one when MSAA is on, not the resolve target.
pub fn depth_texture_multisampled(
    device: &wgpu::Device,
    width: u32,
    height: u32,
    samples: u32,
) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("depth"),
            size: wgpu::Extent3d {
                width: width.max(1),
                height: height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: samples.max(1),
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            // TEXTURE_BINDING because the water pass copies this depth into a
            // sampleable texture (M18). Declaring the usage costs a frame with
            // no water nothing — the copy pass only runs when there is water to
            // absorb with — and it means no caller has to know whether the
            // scene it is about to draw has any.
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        })
        .create_view(&wgpu::TextureViewDescriptor::default())
}

/// The multisampled color attachment the scene pass draws into when MSAA is
/// on. Never read back or sampled — it only ever resolves into the real
/// target — so it wants no `COPY_SRC` or `TEXTURE_BINDING`.
pub fn msaa_color_texture(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    width: u32,
    height: u32,
    samples: u32,
) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("msaa-color"),
            size: wgpu::Extent3d {
                width: width.max(1),
                height: height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: samples.max(1),
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        })
        .create_view(&wgpu::TextureViewDescriptor::default())
}

/// The world-space direction the camera looks, recovered from its
/// view-projection.
///
/// Taken from the matrix rather than from `ScenePass::camera_right`/`_up`
/// because those are documented as meaningful only when there are particles,
/// and the shadow box has to be fitted for every scene that casts.
fn camera_forward(view_projection: Mat4) -> Vec3 {
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
const MIN_SHADOW_ELEVATION_DEGREES: f32 = 5.0;

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
fn clamp_shadow_elevation(travel: Vec3) -> Vec3 {
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
fn light_view_projection(
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
    let projection = glam::camera::rh::proj::directx::orthographic(
        -radius, radius, -radius, radius, 0.1, depth,
    );
    projection * view
}

/// Default clear color — a neutral dark backdrop that neither of the demo
/// scene's materials could be confused with.
pub const DEFAULT_CLEAR: wgpu::Color = wgpu::Color {
    r: 0.05,
    g: 0.06,
    b: 0.09,
    a: 1.0,
};

/// Convert a scene albedo into a clear color, for callers that want one.
pub fn color_from(v: Vec3) -> wgpu::Color {
    wgpu::Color {
        r: v.x as f64,
        g: v.y as f64,
        b: v.z as f64,
        a: 1.0,
    }
}
