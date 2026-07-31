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
use engine_core::road::MAX_ROAD_KERBS;
use engine_core::scene::{
    CloudItem, EnvironmentSettings, RenderItem, ResolvedLights, RoadItem, WaterItem,
};
use engine_core::skeleton::MAX_JOINTS;
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

    /// Material maps (M26), appended at the end for the reason terrain's
    /// fields were: every field above keeps the offset the shader already reads
    /// it from. xy = uv scale, zw = uv offset.
    map_uv: [f32; 4],
    /// x = which maps are bound, as bits; y = alpha cutoff, z = normal
    /// strength, w = ior.
    map_params: [f32; 4],
    /// x = thickness in metres; yzw = per-channel attenuation.
    map_volume: [f32; 4],
}

/// Which map slots a draw has bound, as the bits `map_params.x` carries.
fn map_bits(textures: &engine_core::texture::MaterialTextures) -> u32 {
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
const MAP_ALBEDO: u32 = 1;
const MAP_ORM: u32 = 2;
const MAP_NORMAL: u32 = 4;
const MAP_EMISSIVE: u32 = 8;

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
    /// World → clip (M26). Appended after the array, where it leaves every
    /// prior field at the offset the shaders already read it from, and declared
    /// only by the refraction variant — a shader may declare a *prefix* of the
    /// buffer it is bound to, which is why the other five shaders that spell
    /// this struct out did not have to change.
    view_proj: [[f32; 4]; 4],
}

/// One skinned draw's joint palette, matching WGSL `JointPalette` (M27).
///
/// Fixed-size at [`MAX_JOINTS`], the `MAX_POINT_LIGHTS` / `MAX_ROAD_KERBS`
/// idiom: a rig with more joints is `too_many_joints` at *validate* time,
/// before a device exists, rather than a character that renders correctly up to
/// joint 128 and explodes past it. Unused slots are zeroed and never read — no
/// vertex indexes them, because validation refused the file that could.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct JointPaletteUniform {
    /// Three rows per joint. See `skin.wgsl` for why the fourth is not stored.
    joints: [[[f32; 4]; 3]; MAX_JOINTS],
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
    fn from_palette(palette: &[Mat4]) -> Self {
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

/// Per-road shader data, matching WGSL `RoadUniform` (M23).
///
/// What a road *is* rides in the ordinary `ObjectUniform` beside this — model
/// matrix, asphalt colour, roughness — so a road casts shadows through the
/// unchanged shadow pipeline. This carries only what markings need.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct RoadUniform {
    /// x = half the asphalt width, y = shoulder width, z = total length,
    /// w = dash period in metres (0 = a solid centre line).
    metrics: [f32; 4],
    /// rgb = paint colour, w = edge-line width.
    paint: [f32; 4],
    /// x = edge inset, y = centre-line width, z = dash duty, w = start-line width.
    lines: [f32; 4],
    /// rgb = the red half of a kerb, w = kerb width.
    kerb: [f32; 4],
    /// rgb = shoulder colour, w = live kerb-span count.
    shoulder: [f32; 4],
    /// rgb = embankment colour, w = 1 when a start line is painted.
    bank: [f32; 4],
    /// x = where that line is, in metres along the centerline; rest padding.
    start: [f32; 4],
    /// `(start_v, end_v, side, stripe)` per kerbed corner. Unused slots are
    /// zeroed and never read — the shader loops to the count.
    kerbs: [[f32; 4]; MAX_ROAD_KERBS],
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
    /// Where the UVs start in the same buffer. Uploaded for every mesh since
    /// M23, because a road's markings are painted from surface coordinates it
    /// carries there — before that nothing on the GPU read a UV at all.
    uvs_offset: u64,
    /// Where the skinning influences start in the same buffer (M27), for a
    /// skinned mesh. `None` for everything else — which is every mesh
    /// committed before M27 — so no existing vertex buffer grew by a byte.
    skin_offsets: Option<SkinOffsets>,
    indices: wgpu::Buffer,
    index_count: u32,
    /// Frame counter at the last draw that used this mesh; entries idle for
    /// [`MESH_CACHE_LIFETIME`] frames are dropped.
    last_used: u64,
}

/// Where a skinned mesh's two extra vertex slots start in its shared buffer.
#[derive(Clone, Copy)]
struct SkinOffsets {
    joints: u64,
    weights: u64,
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
///
/// Since M26 the view rides in the shared frame-textures group rather than in a
/// group of its own — see [`FrameTextures`].
struct SceneDepth {
    view: wgpu::TextureView,
    width: u32,
    height: u32,
}

impl SceneDepth {
    fn new(device: &wgpu::Device, width: u32, height: u32) -> Self {
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
        Self {
            view,
            width: width.max(1),
            height: height.max(1),
        }
    }

    /// The copy for a target of this size, reallocated when the size changes.
    /// Exact rather than grow-only: the shader converts pixel coordinates to
    /// UVs with this texture's own dimensions, so a stale larger copy would
    /// read the wrong pixels rather than merely waste memory.
    ///
    /// Returns whether it reallocated, which is what tells the frame-textures
    /// bind group that it has to be rebuilt.
    fn ensure(
        slot: &mut Option<Self>,
        device: &wgpu::Device,
        width: u32,
        height: u32,
    ) -> bool {
        let fits = slot
            .as_ref()
            .is_some_and(|held| held.width == width.max(1) && held.height == height.max(1));
        if !fits {
            *slot = Some(Self::new(device, width, height));
        }
        !fits
    }
}

/// The opaque pass's colour, copied where a refracting surface can read it
/// (M26). Allocated the first time a scene refracts anything and resized with
/// the target, exactly like [`SceneDepth`], and for the same reason: a pass
/// cannot sample the colour attachment it is drawing into.
struct SceneColor {
    view: wgpu::TextureView,
    width: u32,
    height: u32,
}

impl SceneColor {
    fn new(device: &wgpu::Device, format: wgpu::TextureFormat, width: u32, height: u32) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("scene-color-copy"),
            size: wgpu::Extent3d {
                width: width.max(1),
                height: height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Self {
            view,
            width: width.max(1),
            height: height.max(1),
        }
    }

    fn ensure(
        slot: &mut Option<Self>,
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        width: u32,
        height: u32,
    ) -> bool {
        let fits = slot
            .as_ref()
            .is_some_and(|held| held.width == width.max(1) && held.height == height.max(1));
        if !fits {
            *slot = Some(Self::new(device, format, width, height));
        }
        !fits
    }
}

/// Group 2 for every lit pipeline: the shadow map, the depth copy, and the
/// colour copy, with their samplers (M26).
///
/// They were three groups only because they arrived in three milestones, and
/// three of four slots is what the budget could not afford — see
/// `designs/material-system-design.md` §3. Every one of them is frame-scoped:
/// written once per frame, read by everything, rebuilt only when the render
/// target resizes or a scene starts casting shadows.
///
/// The bind group is cached against `key` so the steady-state frame rebuilds
/// nothing; a 1×1 placeholder stands in for each texture the frame does not
/// have, since WGSL binds unconditionally while the reads sit behind a branch.
struct FrameTextures {
    bind_group: wgpu::BindGroup,
    /// The same group with the **colour copy left out** — a 1×1 placeholder in
    /// its slot — for the opaque pass.
    ///
    /// Not an optimisation: on the refracting path the opaque pass is *drawing
    /// into* that copy (directly without MSAA, as a resolve target with it), and
    /// a texture cannot be a colour attachment and a bound resource in the same
    /// pass. Nothing in the opaque pass reads scene colour, so leaving it out
    /// costs nothing and the two groups share every other binding.
    opaque_bind_group: wgpu::BindGroup,
    key: FrameTextureKey,
}

/// What a cached [`FrameTextures`] was built from. The sizes are the copies'
/// own dimensions, so a resize invalidates the group by construction.
#[derive(Clone, Copy, PartialEq, Eq)]
struct FrameTextureKey {
    shadows: bool,
    depth: Option<(u32, u32)>,
    color: Option<(u32, u32)>,
}

/// Resolution of the directional shadow map, in texels on a side.
///
/// Fixed rather than authored: `EnvironmentSettings::shadow_distance` already
/// gives the scene the sharpness knob that matters (it sets how much world
/// these texels are spread over), and a second one would only let a scene ask
/// for 8192² and blame the engine for the memory.
const SHADOW_MAP_SIZE: u32 = 2048;

/// The shadow map.
///
/// A 1×1 placeholder stands in when a scene does not cast shadows: WGSL binds
/// the texture unconditionally, but the sampling is behind `params.y`, so
/// nothing ever reads the placeholder's undefined contents. That keeps one
/// mesh pipeline for both cases instead of two shader permutations.
struct ShadowMap {
    view: wgpu::TextureView,
}

impl ShadowMap {
    fn new(device: &wgpu::Device, size: u32) -> Self {
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

/// One uploaded texture with its mip chain, cached across frames.
///
/// `_source` keeps the `TextureData` alive for exactly the reason `CachedMesh`
/// keeps its geometry: the cache is keyed on that allocation's address, and a
/// strong reference is what stops a freed texture's address from being reused
/// by a *different* one and silently colliding.
struct CachedTexture {
    _source: Arc<engine_core::texture::TextureData>,
    view: wgpu::TextureView,
    last_used: u64,
}

/// A material's four map slots as one bind group, cached on the identities of
/// the textures in it.
///
/// Keyed on the `Arc` addresses rather than on the entity, because two entities
/// sharing a `materials/*.json` share its pixels and should share its bind
/// group — which is most of the point of shareable materials.
struct CachedMaterial {
    bind_group: wgpu::BindGroup,
    last_used: u64,
}

/// The 1×1 white bound in a material slot with no map.
///
/// **Written, not merely allocated.** The reads sit behind the `map_params`
/// bits so nothing should ever sample it — but "should" is doing work there,
/// and an unwritten texture's contents are whatever the allocator last had.
/// Leaving it undefined cost an afternoon: a slot that was in fact being bound
/// rendered as a stable magenta that looked exactly like a mip-chain bug.
fn white_texture(device: &wgpu::Device, queue: &wgpu::Queue) -> wgpu::TextureView {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("material-white"),
        size: wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &[255u8, 255, 255, 255],
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(4),
            rows_per_image: None,
        },
        wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
    );
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

/// A 1×1 texture of `format`, bound wherever a frame does not have the real
/// thing. Never read: every sampler of it sits behind a branch that is off.
fn placeholder_texture(
    device: &wgpu::Device,
    label: &str,
    format: wgpu::TextureFormat,
) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        })
        .create_view(&wgpu::TextureViewDescriptor::default())
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
    /// Roads (M23), drawn with the opaque geometry and casting shadows like it.
    /// Pass `&[]` for a scene with no road, which then issues exactly the draws
    /// it did before roads existed.
    pub roads: &'a [RoadItem],
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

/// Everything a skinned draw needs, built the **first time a frame has one**
/// (M27) and kept for the life of the renderer.
///
/// Lazy for the reason the shadow map, the 1×1 white texture and the colour
/// copy are: a scene with no skinned mesh pays nothing, and "nothing" here is
/// six shader compilations at startup — which every `engine screenshot` in this
/// repo but one would otherwise pay on every invocation.
///
/// The variants mirror the unskinned ones exactly, so routing a skinned draw is
/// the same decision with the same inputs; anything else would be a second
/// place for "which pipeline does this material want" to disagree with itself.
struct SkinnedPipelines {
    opaque: wgpu::RenderPipeline,
    textured: wgpu::RenderPipeline,
    transparent: wgpu::RenderPipeline,
    textured_transparent: wgpu::RenderPipeline,
    refractive: wgpu::RenderPipeline,
    shadow: wgpu::RenderPipeline,
    shadow_cutout: wgpu::RenderPipeline,
}

/// Which optional vertex slots and bind groups one skinned pipeline declares.
///
/// Two variants' worth of difference, spelled out rather than inferred: a
/// vertex-buffer slot bound in the wrong order is a character that renders as
/// noise, and there is no way to see from the noise which slot was wrong.
#[derive(Clone, Copy)]
struct SkinnedInputs {
    /// Whether the stage reads a normal. The solid caster does not.
    normal: bool,
    /// The material's maps and the group they bind at — 3 in the mesh passes,
    /// 2 in the cut-out caster, which has no frame textures to read because it
    /// *is* what writes one of them.
    material: Option<([usize; 4], u32)>,
}

impl SkinnedInputs {
    const CASTER: Self = Self {
        normal: false,
        material: None,
    };
    const LIT: Self = Self {
        normal: true,
        material: None,
    };
    fn textured(key: [usize; 4]) -> Self {
        Self {
            normal: true,
            material: Some((key, 3)),
        }
    }
    fn cutout_caster(key: [usize; 4]) -> Self {
        Self {
            normal: true,
            material: Some((key, 2)),
        }
    }
}

/// The palette buffer and the group-0 bind group naming it beside the object
/// uniforms.
///
/// Rebuilt when either buffer is reallocated, which the recorded capacities
/// detect: a bind group holds its buffers by identity, and `Uniforms::ensure`
/// mints a new one whenever the draw list outgrows the old.
struct SkinnedObjects {
    palette: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    palette_size: u64,
    objects_size: u64,
}

pub struct SceneRenderer {
    pipeline: wgpu::RenderPipeline,
    /// Same shader as `pipeline`, blended and depth-write-off, for materials
    /// with `alpha < 1` or `transmission > 0`.
    transparent_pipeline: wgpu::RenderPipeline,
    /// The same again with refraction spliced in (M26), for the materials that
    /// bend what is behind them. A third pipeline rather than a branch in the
    /// second — see its construction for the pixel that decided that.
    refractive_pipeline: wgpu::RenderPipeline,
    /// `pipeline` with the terrain material generator spliced into its shader
    /// (M22) — see `with_surface` for why this is a separate module rather than
    /// a branch inside the shared one.
    terrain_pipeline: wgpu::RenderPipeline,
    /// `pipeline` and `transparent_pipeline` with texture sampling spliced in
    /// (M26), for materials that carry any map.
    textured_pipeline: wgpu::RenderPipeline,
    textured_transparent_pipeline: wgpu::RenderPipeline,
    /// Group 3 for a textured draw: four maps and their sampler.
    material_layout: wgpu::BindGroupLayout,
    map_sampler: wgpu::Sampler,
    shadow_pipeline: wgpu::RenderPipeline,
    /// The caster pass for `alpha_cutoff` materials (M26): the same depth-only
    /// pass with the smallest fragment stage that can `discard`.
    shadow_cutout_pipeline: wgpu::RenderPipeline,
    sky_pipeline: wgpu::RenderPipeline,
    water_pipeline: wgpu::RenderPipeline,
    cloud_pipeline: wgpu::RenderPipeline,
    /// Roads (M23): opaque, shadow-casting, and the only pipeline that reads a
    /// vertex UV — a road's markings are painted from surface coordinates.
    road_pipeline: wgpu::RenderPipeline,
    road_layout: wgpu::BindGroupLayout,
    depth_resolve_pipeline: wgpu::RenderPipeline,
    /// Puts the opaque colour copy back over the frame — see `blit.wgsl` for
    /// the one path that needs it.
    blit_pipeline: wgpu::RenderPipeline,
    water_layout: wgpu::BindGroupLayout,
    cloud_layout: wgpu::BindGroupLayout,
    depth_source_layout: wgpu::BindGroupLayout,
    particle_pipeline: wgpu::RenderPipeline,
    /// Same shader and same instance buffer as `particle_pipeline`, blending
    /// additively — for `ParticleEmitter.blend: "additive"` (fire, sparks).
    additive_particle_pipeline: wgpu::RenderPipeline,
    object_layout: wgpu::BindGroupLayout,
    /// Group 1 everywhere. Kept because the skinned pipelines are built
    /// lazily, long after the constructor that made it.
    frame_layout: wgpu::BindGroupLayout,
    /// Group 0 for a skinned draw (M27): the object uniform at binding 0, as
    /// everywhere else, plus the joint palette at binding 1 with its own
    /// dynamic offset. Built up front because it is cheap and the lazily-built
    /// pipelines need it; nothing binds it until a skinned draw appears.
    skinned_object_layout: wgpu::BindGroupLayout,
    hud_pipeline: wgpu::RenderPipeline,
    hud_layout: wgpu::BindGroupLayout,
    /// Group 2 everywhere: shadow map, depth copy, colour copy. See
    /// [`FrameTextures`].
    frame_textures_layout: wgpu::BindGroupLayout,
    /// The comparison sampler for the shadow map and the linear one for the
    /// colour copy. Created once — one sampler per configuration, not one per
    /// texture.
    shadow_sampler: wgpu::Sampler,
    scene_sampler: wgpu::Sampler,
    format: wgpu::TextureFormat,
    samples: u32,

    // Everything below persists across frames; see the module doc.
    /// Bound whenever shadows are off. See [`ShadowMap`].
    shadow_placeholder: ShadowMap,
    /// Bound whenever the frame has no depth or colour copy.
    depth_placeholder: wgpu::TextureView,
    color_placeholder: wgpu::TextureView,
    /// The cached group-2 binding, rebuilt only when one of its textures is.
    frame_textures: Option<FrameTextures>,
    /// The real map, allocated the first time a scene casts shadows so that
    /// scenes which never do pay nothing for it.
    shadow_map: Option<ShadowMap>,
    meshes: HashMap<usize, CachedMesh>,
    /// Uploaded material maps, keyed on the `Arc<TextureData>` identity — M15's
    /// rule, which is why `TextureSource` must hand back the same `Arc`.
    textures: HashMap<usize, CachedTexture>,
    /// Material bind groups, keyed on the four slots' texture identities.
    materials: HashMap<[usize; 4], CachedMaterial>,
    /// The 1×1 white bound in a slot with no map, made on the first textured
    /// frame — a scene with no maps never allocates it.
    white_texture: Option<wgpu::TextureView>,
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
    /// And again for roads, whose marking parameters and kerb spans are far too
    /// big to ride in the object uniform every mesh shares.
    road_objects: Option<Uniforms>,
    road_stride: u64,
    /// The skinned set (M27), absent until a frame has a skinned draw.
    skinned: Option<SkinnedPipelines>,
    skinned_objects: Option<SkinnedObjects>,
    /// Distance between consecutive joint palettes: 6 KiB rounded up to the
    /// device's dynamic-offset alignment, which on every adapter this runs on
    /// leaves it exactly 6 KiB.
    palette_stride: u64,
    /// The opaque depth copy the water pass reads, allocated on the first frame
    /// that has any water in it and resized with the target.
    scene_depth: Option<SceneDepth>,
    /// The opaque colour copy a refracting surface reads (M26), allocated on
    /// the first frame that has one and resized with the target.
    scene_color: Option<SceneColor>,
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

        // The same group 0 with the joint palette beside it (M27). A second
        // *layout*, not a second group index: `downlevel_defaults` caps
        // `max_bind_groups` at 4 and M26 spent the fourth, so the palette rides
        // in group 0 under a layout the skinned pipelines alone use — which
        // costs the plain pipelines nothing, because they keep theirs.
        let skinned_object_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("skinned-object-uniforms"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: true,
                            min_binding_size: std::num::NonZeroU64::new(std::mem::size_of::<
                                ObjectUniform,
                            >(
                            )
                                as u64),
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        // Vertex only: the palette moves vertices and nothing
                        // in a fragment stage has ever asked where a joint is.
                        visibility: wgpu::ShaderStages::VERTEX,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: true,
                            min_binding_size: std::num::NonZeroU64::new(std::mem::size_of::<
                                JointPaletteUniform,
                            >(
                            )
                                as u64),
                        },
                        count: None,
                    },
                ],
            });

        // Group 2: the frame's textures — the shadow map and its comparison
        // sampler, the opaque depth copy, and the opaque colour copy with the
        // sampler that reads it (M26). Every entry is always present in the
        // layout even when the frame has none of them: a bind group layout may
        // contain entries the shader never references, and the reverse is the
        // error, so `mesh.wgsl` keeps declaring only bindings 0 and 1 and stays
        // the file it has been since M4.
        //
        // Merging these was a bind-group-budget decision, not a tidiness one:
        // `downlevel_defaults` caps `max_bind_groups` at 4, and three of them
        // spent on frame-scoped textures left nowhere for a material.
        let frame_textures_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("frame-textures"),
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
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            // Read with `textureLoad`: no sampler, so nothing
                            // filters depth across a silhouette.
                            sample_type: wgpu::TextureSampleType::Float { filterable: false },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 3,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 4,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                ],
            });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mesh-pipeline-layout"),
            bind_group_layouts: &[
                Some(&object_layout),
                Some(&frame_layout),
                Some(&frame_textures_layout),
            ],
            immediate_size: 0,
        });

        // Position, normal and UV live in separate buffers so a mesh with no
        // normals does not need a padded interleaved layout. Only the road
        // pipeline binds the third — it is where a road's surface coordinates
        // travel.
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
            Some(wgpu::VertexBufferLayout {
                array_stride: 8,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &[wgpu::VertexAttribute {
                    offset: 0,
                    shader_location: 2,
                    format: wgpu::VertexFormat::Float32x2,
                }],
            }),
        ];

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mesh-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &vertex_layouts[..2],
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

        // Group 3 for a textured mesh (M26): the four maps and one sampler.
        // Every slot is always present in the layout, with a 1×1 white bound
        // where a material has no map, because WGSL binds unconditionally and
        // the reads sit behind the `map_params` bits.
        let material_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("material-maps"),
            entries: &[0u32, 1, 2, 3]
                .map(|binding| wgpu::BindGroupLayoutEntry {
                    binding,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                })
                .into_iter()
                .chain([wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                }])
                .collect::<Vec<_>>(),
        });
        let map_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("material-sampler"),
            // Repeat on both axes, because tiling is what a material texture is
            // for: `ClampToEdge` would make `uv_scale: [20, 20]` draw one
            // stretched copy surrounded by smeared border pixels.
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            // Anisotropy is pinned at 1 (off) in v1. It measurably improves
            // exactly the grazing-angle tiling this milestone is for, and it is
            // also a per-adapter *quality* setting — which is where this repo
            // has repeatedly found reproducibility to die. A baseline should be
            // a function of the scene, not of the driver's filtering.
            anisotropy_clamp: 1,
            ..Default::default()
        });

        let textured_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("textured-pipeline-layout"),
            bind_group_layouts: &[
                Some(&object_layout),
                Some(&frame_layout),
                Some(&frame_textures_layout),
                Some(&material_layout),
            ],
            immediate_size: 0,
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
            source: wgpu::ShaderSource::Wgsl(with_sky_common(&with_terrain())),
        });
        let terrain_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("terrain-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &terrain_shader,
                entry_point: Some("vs_main"),
                buffers: &vertex_layouts[..2],
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

        // The textured twins (M26). Same states as the plain and blended mesh
        // pipelines exactly — a textured surface is not a differently-composited
        // surface, it is a differently-*resolved* one — with a fourth bind group
        // for the maps and a third vertex slot for the UVs they are read at.
        let textured_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("textured-shader"),
            source: wgpu::ShaderSource::Wgsl(with_sky_common(&with_textures())),
        });
        let textured_blended_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("textured-blended-shader"),
            source: wgpu::ShaderSource::Wgsl(with_sky_common(&with_textures_and_refraction())),
        });
        let textured_pipeline_for = |label: &str,
                                     module: &wgpu::ShaderModule,
                                     blend: wgpu::BlendState,
                                     depth_write: bool| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&textured_layout),
                vertex: wgpu::VertexState {
                    module,
                    entry_point: Some("vs_main"),
                    buffers: &vertex_layouts,
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module,
                    entry_point: Some("fs_main"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(blend),
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
                    depth_write_enabled: Some(depth_write),
                    depth_compare: Some(wgpu::CompareFunction::Less),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample,
                multiview_mask: None,
                cache: None,
            })
        };
        let textured_pipeline = textured_pipeline_for(
            "textured-pipeline",
            &textured_shader,
            wgpu::BlendState::REPLACE,
            true,
        );
        let textured_transparent_pipeline = textured_pipeline_for(
            "textured-transparent-pipeline",
            &textured_blended_shader,
            wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING,
            false,
        );

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
                buffers: &vertex_layouts[..2],
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

        // Refraction is a *third* blended pipeline rather than a branch inside
        // the second, and that was measured, not assumed: compiling the
        // refraction variant for every transparent draw moved one pixel of
        // `m16_environment.png` by one channel step — M22's lesson repeating on
        // the one fixture that has transmissive geometry. The added branch is
        // never taken by a surface with `ior: 1.0` and `thickness: 0.0`, and it
        // still changes the code the compiler sees around M16's untouchable
        // lines, which is exactly what the rule is about.
        //
        // So a material that does not refract keeps the pipeline it had, whose
        // module is `mesh.wgsl` as it sits on disk, and only a material that
        // asks to bend light pays for a second shader.
        let refractive_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mesh-refractive-shader"),
            source: wgpu::ShaderSource::Wgsl(with_sky_common(&with_refraction())),
        });
        let refractive_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mesh-refractive-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &refractive_shader,
                entry_point: Some("vs_main"),
                buffers: &vertex_layouts[..2],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &refractive_shader,
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
        let shadow_cutout_pipeline = Self::shadow_cutout_pipeline(
            device,
            &object_layout,
            &frame_layout,
            &material_layout,
            &vertex_layouts,
        );
        let sky_pipeline = Self::sky_pipeline(device, &frame_layout, format, multisample);

        // Water (M18). Its own uniform, its own shader, and the mesh pass's
        // frame and frame-texture bindings — the depth it absorbs against
        // arrives in group 2 with the shadow map since M26, which is what frees
        // its group 3.
        let water_layout = uniform_layout(
            "water-uniforms",
            Some(std::mem::size_of::<WaterUniform>() as u64),
        );
        let water_pipeline = Self::water_pipeline(
            device,
            &water_layout,
            &frame_layout,
            &frame_textures_layout,
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
            &vertex_layouts[..2],
            format,
            multisample,
        );

        // Roads (M23): the opaque twin of the mesh pipeline — its own uniform
        // at group 3, the mesh pass's object, frame and shadow bindings, and
        // the one pipeline in the engine that reads a UV.
        let road_layout = uniform_layout(
            "road-uniforms",
            Some(std::mem::size_of::<RoadUniform>() as u64),
        );
        let road_pipeline = Self::road_pipeline(
            device,
            &object_layout,
            &frame_layout,
            &frame_textures_layout,
            &road_layout,
            &vertex_layouts,
            format,
            multisample,
        );

        let (depth_resolve_pipeline, depth_source_layout) =
            Self::depth_resolve_pipeline(device, samples);

        let blit_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("blit-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/blit.wgsl").into()),
        });
        let blit_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("blit-pipeline-layout"),
            bind_group_layouts: &[Some(&frame_textures_layout)],
            immediate_size: 0,
        });
        let blit_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("blit-pipeline"),
            layout: Some(&blit_layout),
            vertex: wgpu::VertexState {
                module: &blit_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &blit_shader,
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
            // This path only exists when MSAA is off, so the blit is never
            // multisampled.
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

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

        let shadow_placeholder = ShadowMap::new(device, 1);
        let depth_placeholder =
            placeholder_texture(device, "scene-depth-placeholder", wgpu::TextureFormat::R32Float);
        let color_placeholder = placeholder_texture(device, "scene-color-placeholder", format);
        let shadow_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("shadow-sampler"),
            // Linear filtering on a comparison sampler is hardware PCF: each
            // tap already returns a bilinear blend of four depth *tests*, so
            // the 3×3 kernel in the shader is effectively 6×6 for free.
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            compare: Some(wgpu::CompareFunction::LessEqual),
            ..Default::default()
        });
        let scene_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("scene-color-sampler"),
            // Clamped, because a refraction offset that runs off the frame has
            // no data to read and the honest failure is a stretched edge.
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let water_stride =
            std::mem::size_of::<WaterUniform>().next_multiple_of(alignment as usize) as u64;
        let cloud_stride =
            std::mem::size_of::<CloudUniform>().next_multiple_of(alignment as usize) as u64;
        let road_stride =
            std::mem::size_of::<RoadUniform>().next_multiple_of(alignment as usize) as u64;
        let palette_stride =
            std::mem::size_of::<JointPaletteUniform>().next_multiple_of(alignment as usize) as u64;

        Self {
            pipeline,
            transparent_pipeline,
            refractive_pipeline,
            terrain_pipeline,
            textured_pipeline,
            textured_transparent_pipeline,
            material_layout,
            map_sampler,
            shadow_pipeline,
            shadow_cutout_pipeline,
            sky_pipeline,
            water_pipeline,
            cloud_pipeline,
            road_pipeline,
            road_layout,
            road_objects: None,
            road_stride,
            skinned: None,
            skinned_objects: None,
            palette_stride,
            depth_resolve_pipeline,
            blit_pipeline,
            water_layout,
            cloud_layout,
            depth_source_layout,
            particle_pipeline,
            additive_particle_pipeline,
            object_layout,
            frame_layout,
            skinned_object_layout,
            hud_pipeline,
            hud_layout,
            frame_textures_layout,
            shadow_sampler,
            scene_sampler,
            format,
            samples,
            shadow_placeholder,
            depth_placeholder,
            color_placeholder,
            frame_textures: None,
            shadow_map: None,
            meshes: HashMap::new(),
            textures: HashMap::new(),
            materials: HashMap::new(),
            white_texture: None,
            frame_uniform,
            objects: None,
            object_stride,
            water_objects: None,
            water_stride,
            cloud_objects: None,
            cloud_stride,
            scene_depth: None,
            scene_color: None,
            depth_source: None,
            particle_instances: None,
            particle_uniform,
            hud_targets: Vec::new(),
            frame_index: 0,
        }
    }

    /// Build the skinned pipeline set, once, on the first frame that has a
    /// skinned draw (M27).
    ///
    /// Six shader modules, which is why this is lazy rather than part of the
    /// constructor: every `engine screenshot` in this repo but one has no
    /// skinned mesh in it and should not pay for compiling them. The precedent
    /// is the shadow map, the 1×1 white texture, and the colour copy — all
    /// allocated by the first frame that needs them.
    fn build_skinned(&self, device: &wgpu::Device) -> SkinnedPipelines {
        let multisample = wgpu::MultisampleState {
            count: self.samples,
            ..Default::default()
        };
        let module = |label: &str, source: std::borrow::Cow<'static, str>| {
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(label),
                source: wgpu::ShaderSource::Wgsl(with_sky_common(&source)),
            })
        };
        let plain = module("skinned-shader", with_surface(&[skin_producer()]));
        let textured = module(
            "skinned-textured-shader",
            with_surface(&[skin_producer(), texture_producer()]),
        );
        let refractive = module(
            "skinned-refractive-shader",
            with_surface(&[skin_producer(), refraction_producer()]),
        );
        let textured_blended = module(
            "skinned-textured-blended-shader",
            with_surface(&[skin_producer(), texture_producer(), refraction_producer()]),
        );

        // Position, normal, UV, joints, weights. The joints arrive as
        // `Uint16x4` — a 16-bit index is what glTF writes and what 128 joints
        // need — and land in the shader as `vec4<u32>`.
        let joints = wgpu::VertexBufferLayout {
            array_stride: 8,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[wgpu::VertexAttribute {
                offset: 0,
                shader_location: 3,
                format: wgpu::VertexFormat::Uint16x4,
            }],
        };
        let weights = wgpu::VertexBufferLayout {
            array_stride: 16,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[wgpu::VertexAttribute {
                offset: 0,
                shader_location: 4,
                format: wgpu::VertexFormat::Float32x4,
            }],
        };
        let position = wgpu::VertexBufferLayout {
            array_stride: 12,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[wgpu::VertexAttribute {
                offset: 0,
                shader_location: 0,
                format: wgpu::VertexFormat::Float32x3,
            }],
        };
        let normal = wgpu::VertexBufferLayout {
            array_stride: 12,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[wgpu::VertexAttribute {
                offset: 0,
                shader_location: 1,
                format: wgpu::VertexFormat::Float32x3,
            }],
        };
        let uv = wgpu::VertexBufferLayout {
            array_stride: 8,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[wgpu::VertexAttribute {
                offset: 0,
                shader_location: 2,
                format: wgpu::VertexFormat::Float32x2,
            }],
        };

        // Untextured skinning skips the UV slot entirely rather than binding a
        // padded one: the shader does not declare `@location(2)`, and a layout
        // that provides an attribute the stage never reads is a mismatch worth
        // not relying on.
        let plain_layouts = [
            Some(position.clone()),
            Some(normal.clone()),
            Some(joints.clone()),
            Some(weights.clone()),
        ];
        let textured_layouts = [
            Some(position.clone()),
            Some(normal.clone()),
            Some(uv.clone()),
            Some(joints.clone()),
            Some(weights.clone()),
        ];
        let caster_layouts = [Some(position), Some(joints), Some(weights)];

        let mesh_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("skinned-pipeline-layout"),
            bind_group_layouts: &[
                Some(&self.skinned_object_layout),
                Some(&self.frame_layout),
                Some(&self.frame_textures_layout),
            ],
            immediate_size: 0,
        });
        let material_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("skinned-textured-pipeline-layout"),
                bind_group_layouts: &[
                    Some(&self.skinned_object_layout),
                    Some(&self.frame_layout),
                    Some(&self.frame_textures_layout),
                    Some(&self.material_layout),
                ],
                immediate_size: 0,
            });

        let format = self.format;
        let mesh_pipeline = |label: &str,
                             module: &wgpu::ShaderModule,
                             layout: &wgpu::PipelineLayout,
                             buffers: &[Option<wgpu::VertexBufferLayout<'_>>],
                             blend: wgpu::BlendState,
                             depth_write: bool| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(layout),
                vertex: wgpu::VertexState {
                    module,
                    entry_point: Some("vs_main"),
                    buffers,
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module,
                    entry_point: Some("fs_main"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(blend),
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
                    depth_write_enabled: Some(depth_write),
                    depth_compare: Some(wgpu::CompareFunction::Less),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample,
                multiview_mask: None,
                cache: None,
            })
        };

        let caster = |label: &str,
                      module: &wgpu::ShaderModule,
                      layout: &wgpu::PipelineLayout,
                      buffers: &[Option<wgpu::VertexBufferLayout<'_>>],
                      fragment: Option<wgpu::FragmentState<'_>>| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(layout),
                vertex: wgpu::VertexState {
                    module,
                    entry_point: Some("vs_main"),
                    buffers,
                    compilation_options: Default::default(),
                },
                fragment,
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    // Front-face culled, exactly like the unskinned casters:
                    // the map should record each caster's far side.
                    cull_mode: Some(wgpu::Face::Front),
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(true),
                    depth_compare: Some(wgpu::CompareFunction::Less),
                    stencil: Default::default(),
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
        };

        let shadow_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("skinned-shadow-shader"),
            source: wgpu::ShaderSource::Wgsl(skinned_shadow().into()),
        });
        let shadow_cutout_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("skinned-shadow-cutout-shader"),
            source: wgpu::ShaderSource::Wgsl(skinned_shadow_cutout().into()),
        });
        let shadow_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("skinned-shadow-pipeline-layout"),
            bind_group_layouts: &[Some(&self.skinned_object_layout), Some(&self.frame_layout)],
            immediate_size: 0,
        });
        let shadow_cutout_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("skinned-shadow-cutout-pipeline-layout"),
            bind_group_layouts: &[
                Some(&self.skinned_object_layout),
                Some(&self.frame_layout),
                // The material group at 2, not 3: this pipeline has no frame
                // textures to read — it *is* what writes one of them.
                Some(&self.material_layout),
            ],
            immediate_size: 0,
        });

        SkinnedPipelines {
            opaque: mesh_pipeline(
                "skinned-pipeline",
                &plain,
                &mesh_layout,
                &plain_layouts,
                wgpu::BlendState::REPLACE,
                true,
            ),
            textured: mesh_pipeline(
                "skinned-textured-pipeline",
                &textured,
                &material_pipeline_layout,
                &textured_layouts,
                wgpu::BlendState::REPLACE,
                true,
            ),
            transparent: mesh_pipeline(
                "skinned-transparent-pipeline",
                &plain,
                &mesh_layout,
                &plain_layouts,
                wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING,
                false,
            ),
            textured_transparent: mesh_pipeline(
                "skinned-textured-transparent-pipeline",
                &textured_blended,
                &material_pipeline_layout,
                &textured_layouts,
                wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING,
                false,
            ),
            refractive: mesh_pipeline(
                "skinned-refractive-pipeline",
                &refractive,
                &mesh_layout,
                &plain_layouts,
                wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING,
                false,
            ),
            shadow: caster(
                "skinned-shadow-pipeline",
                &shadow_module,
                &shadow_layout,
                &caster_layouts,
                None,
            ),
            shadow_cutout: caster(
                "skinned-shadow-cutout-pipeline",
                &shadow_cutout_module,
                &shadow_cutout_layout,
                &textured_layouts,
                Some(wgpu::FragmentState {
                    module: &shadow_cutout_module,
                    entry_point: Some("fs_main"),
                    targets: &[],
                    compilation_options: Default::default(),
                }),
            ),
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

    /// The caster pass for alpha-cut materials (M26).
    ///
    /// Identical to [`Self::shadow_pipeline`] in every state that matters —
    /// front-face culled, the same slope-scaled bias, no colour target — and
    /// different only in having a fragment stage at all. Two pipelines rather
    /// than one with a branch, so the depth-only pass every current scene casts
    /// through is the one it always was.
    fn shadow_cutout_pipeline(
        device: &wgpu::Device,
        object_layout: &wgpu::BindGroupLayout,
        frame_layout: &wgpu::BindGroupLayout,
        material_layout: &wgpu::BindGroupLayout,
        vertex_layouts: &[Option<wgpu::VertexBufferLayout<'_>>],
    ) -> wgpu::RenderPipeline {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("shadow-cutout-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/shadow_cutout.wgsl").into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("shadow-cutout-pipeline-layout"),
            bind_group_layouts: &[
                Some(object_layout),
                Some(frame_layout),
                Some(material_layout),
            ],
            immediate_size: 0,
        });

        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("shadow-cutout-pipeline"),
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
                targets: &[],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                // A cut-out card is a *sheet*: culling its front faces the way
                // solid casters are culled would delete it from the map
                // entirely whenever the sun is on its front side, and the
                // peeling margin that trick buys is meaningless on geometry
                // with no thickness.
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: Default::default(),
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
        frame_textures_layout: &wgpu::BindGroupLayout,
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
                Some(frame_textures_layout),
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

    /// The road pass (M23): the mesh pipeline's opaque twin, with a fourth
    /// bind group for the marking parameters and a third vertex slot for the
    /// surface coordinates they are painted in.
    ///
    /// Everything else matches the mesh pipeline exactly — back-face culled,
    /// depth-tested and depth-writing, `REPLACE` blending — because a road is
    /// ordinary opaque geometry. It is a separate pipeline for the shader's
    /// sake, not the state's.
    #[allow(clippy::too_many_arguments)]
    fn road_pipeline(
        device: &wgpu::Device,
        object_layout: &wgpu::BindGroupLayout,
        frame_layout: &wgpu::BindGroupLayout,
        frame_textures_layout: &wgpu::BindGroupLayout,
        road_layout: &wgpu::BindGroupLayout,
        vertex_layouts: &[Option<wgpu::VertexBufferLayout<'_>>],
        format: wgpu::TextureFormat,
        multisample: wgpu::MultisampleState,
    ) -> wgpu::RenderPipeline {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("road-shader"),
            source: wgpu::ShaderSource::Wgsl(with_sky_common(include_str!("shaders/road.wgsl"))),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("road-pipeline-layout"),
            bind_group_layouts: &[
                Some(object_layout),
                Some(frame_layout),
                Some(frame_textures_layout),
                Some(road_layout),
            ],
            immediate_size: 0,
        });

        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("road-pipeline"),
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
            roads,
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
            self.shadow_map = Some(ShadowMap::new(device, SHADOW_MAP_SIZE));
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
            view_proj: view_projection.to_cols_array_2d(),
        };
        queue.write_buffer(&self.frame_uniform.buffer, 0, bytemuck::bytes_of(&frame));

        // Geometry first: anything new joins the cache, anything already there
        // is just touched. `keys` then addresses the cache during the pass
        // without hashing an `Arc` pointer twice.
        let keys: Vec<usize> = items
            .iter()
            .map(|item| self.upload_mesh(device, &item.mesh))
            .collect();

        // Roads join the same geometry cache as everything else; their ribbons
        // are `Arc`-cached per geometry, so a road that is not being edited
        // uploads once for the life of the run.
        let road_keys: Vec<usize> = roads
            .iter()
            .map(|item| self.upload_mesh(device, &item.surface.mesh))
            .collect();

        // Material maps (M26), on the same terms: uploaded once per distinct
        // texture, and one bind group per distinct *set* of them — so two
        // entities sharing a `materials/*.json` share both.
        let material_keys: Vec<Option<[usize; 4]>> = items
            .iter()
            .map(|item| self.ensure_material(device, queue, &item.textures))
            .collect();
        // Which casters have to test their alpha. `alpha_cutoff` alone is not
        // enough: with no albedo map there is no alpha to test, and such a
        // material casts through the ordinary depth-only pipeline.
        let cutout: Vec<bool> = items
            .iter()
            .map(|item| item.material.alpha_cutoff > 0.0 && item.textures.albedo.is_some())
            .collect();

        // Every entity's uniforms in one buffer, one write, addressed during
        // the pass by dynamic offset.
        //
        // Roads ride at the end of the same array rather than in one of their
        // own. That is what lets a road cast a shadow through the *unchanged*
        // shadow pipeline, which reads nothing but this struct's model matrix.
        let stride = self.object_stride as usize;
        let mut object_bytes = vec![0u8; stride * (items.len() + roads.len())];
        let mut write_object = |index: usize, uniform: &ObjectUniform| {
            let at = index * stride;
            object_bytes[at..at + std::mem::size_of::<ObjectUniform>()]
                .copy_from_slice(bytemuck::bytes_of(uniform));
        };
        for (index, item) in items.iter().enumerate() {
            let material = &item.material;
            write_object(
                index,
                &ObjectUniform {
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
                    map_uv: [
                        material.uv_scale.x,
                        material.uv_scale.y,
                        material.uv_offset.x,
                        material.uv_offset.y,
                    ],
                    map_params: [
                        map_bits(&item.textures) as f32,
                        material.alpha_cutoff,
                        material.normal_strength,
                        material.ior,
                    ],
                    map_volume: [
                        material.thickness,
                        material.attenuation.x,
                        material.attenuation.y,
                        material.attenuation.z,
                    ],
                },
            );
        }
        for (index, item) in roads.iter().enumerate() {
            let road = &item.road;
            write_object(
                items.len() + index,
                &ObjectUniform {
                    mvp: (view_projection * item.model).to_cols_array_2d(),
                    model: item.model.to_cols_array_2d(),
                    normal_matrix: item.model.inverse().transpose().to_cols_array_2d(),
                    // A road is a dielectric; its markings are painted in the
                    // fragment stage over this asphalt colour.
                    albedo_metallic: road.color.extend(0.0).to_array(),
                    emissive_roughness: Vec3::ZERO.extend(road.roughness).to_array(),
                    surface: [1.0, 0.0, 0.0, 0.0],
                    // A road is never terrain: zero layers keeps it off that
                    // path in the shader, exactly like every ordinary mesh.
                    terrain: [0.0; 4],
                    terrain_seed: [0; 4],
                    terrain_layers: terrain_layers(None),
                    // A road takes no maps in v1: its group 3 is its markings.
                    map_uv: [1.0, 1.0, 0.0, 0.0],
                    map_params: [0.0, 0.0, 1.0, 1.0],
                    map_volume: [0.0, 1.0, 1.0, 1.0],
                },
            );
        }
        drop(write_object);
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

        // Joint palettes (M27): one slot per skinned draw in one buffer,
        // addressed during the pass by dynamic offset — the arrangement water,
        // clouds and roads already use, and the reason the palette could ride
        // in group 0 beside the object uniform rather than needing a fifth bind
        // group the device does not have.
        //
        // `skin_slots[i]` is where item `i`'s palette landed, or `None` for
        // everything that is not a skinned mesh — which is every draw in this
        // repo but one, and the test that keeps them on the pipelines that
        // compile `mesh.wgsl` as it sits on disk.
        let mut skin_slots: Vec<Option<usize>> = vec![None; items.len()];
        let mut palettes: Vec<JointPaletteUniform> = Vec::new();
        for (index, item) in items.iter().enumerate() {
            if item.joints.is_empty() || !item.mesh.is_skinned() {
                continue;
            }
            skin_slots[index] = Some(palettes.len());
            palettes.push(JointPaletteUniform::from_palette(&item.joints));
        }
        if !palettes.is_empty() {
            if self.skinned.is_none() {
                self.skinned = Some(self.build_skinned(device));
            }
            let stride = self.palette_stride as usize;
            let mut palette_bytes = vec![0u8; stride * palettes.len()];
            for (index, palette) in palettes.iter().enumerate() {
                let at = index * stride;
                palette_bytes[at..at + std::mem::size_of::<JointPaletteUniform>()]
                    .copy_from_slice(bytemuck::bytes_of(palette));
            }
            let objects = self.objects.as_ref().expect("skinned draws write objects");
            SkinnedObjects::ensure(
                &mut self.skinned_objects,
                device,
                &self.skinned_object_layout,
                objects,
                palette_bytes.len() as u64,
            );
            let skinned = self.skinned_objects.as_ref().expect("just ensured");
            queue.write_buffer(&skinned.palette, 0, &palette_bytes);
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

        // Road marking parameters and kerb spans, the same
        // one-buffer-addressed-by-dynamic-offset arrangement.
        if !roads.is_empty() {
            let stride = self.road_stride as usize;
            let mut road_bytes = vec![0u8; stride * roads.len()];
            for (index, item) in roads.iter().enumerate() {
                let uniform = road_uniform(item);
                let at = index * stride;
                road_bytes[at..at + std::mem::size_of::<RoadUniform>()]
                    .copy_from_slice(bytemuck::bytes_of(&uniform));
            }
            let objects = Uniforms::ensure(
                &mut self.road_objects,
                device,
                &self.road_layout,
                "road-uniforms",
                road_bytes.len() as u64,
                Some(std::mem::size_of::<RoadUniform>() as u64),
            );
            queue.write_buffer(&objects.buffer, 0, &road_bytes);
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
                let cast = |pass: &mut wgpu::RenderPass<'_>, object: usize, mesh: &CachedMesh| {
                    pass.set_bind_group(
                        0,
                        &objects.bind_group,
                        &[(object as u64 * self.object_stride) as u32],
                    );
                    pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                    pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..mesh.index_count, 0, 0..1);
                };
                for &index in &opaque {
                    if cutout[index] || skin_slots[index].is_some() {
                        continue;
                    }
                    cast(&mut shadow_pass, index, &self.meshes[&keys[index]]);
                }
                // Cut-out casters after the solid ones, in one run: the switch
                // costs one pipeline change a frame, and a scene with no
                // `alpha_cutoff` never enters this loop.
                let mut switched = false;
                for &index in &opaque {
                    if !cutout[index] || skin_slots[index].is_some() {
                        continue;
                    }
                    let Some(key) = material_keys[index] else { continue };
                    if !switched {
                        shadow_pass.set_pipeline(&self.shadow_cutout_pipeline);
                        shadow_pass.set_bind_group(1, &self.frame_uniform.bind_group, &[]);
                        switched = true;
                    }
                    let mesh = &self.meshes[&keys[index]];
                    shadow_pass.set_bind_group(
                        0,
                        &objects.bind_group,
                        &[(index as u64 * self.object_stride) as u32],
                    );
                    shadow_pass.set_bind_group(2, &self.materials[&key].bind_group, &[]);
                    shadow_pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                    shadow_pass.set_vertex_buffer(1, mesh.vertices.slice(mesh.normals_offset..));
                    shadow_pass.set_vertex_buffer(2, mesh.vertices.slice(mesh.uvs_offset..));
                    shadow_pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                    shadow_pass.draw_indexed(0..mesh.index_count, 0, 0..1);
                }
                // Skinned casters (M27), in their own two runs. Without them a
                // walking character casts its **rest pose** — a wrongness that
                // reads as a renderer bug and is actually a missing pipeline.
                if let (Some(skinned), Some(skin_objects)) = (&self.skinned, &self.skinned_objects)
                {
                    let mut solid = false;
                    for &index in &opaque {
                        let Some(slot) = skin_slots[index] else {
                            continue;
                        };
                        if cutout[index] {
                            continue;
                        }
                        if !solid {
                            shadow_pass.set_pipeline(&skinned.shadow);
                            shadow_pass.set_bind_group(1, &self.frame_uniform.bind_group, &[]);
                            solid = true;
                        }
                        self.draw_skinned(
                            &mut shadow_pass,
                            skin_objects,
                            keys[index],
                            index,
                            slot,
                            SkinnedInputs::CASTER,
                        );
                    }
                    let mut cut = false;
                    for &index in &opaque {
                        let Some(slot) = skin_slots[index] else {
                            continue;
                        };
                        if !cutout[index] {
                            continue;
                        }
                        let Some(key) = material_keys[index] else {
                            continue;
                        };
                        if !cut {
                            shadow_pass.set_pipeline(&skinned.shadow_cutout);
                            shadow_pass.set_bind_group(1, &self.frame_uniform.bind_group, &[]);
                            cut = true;
                        }
                        self.draw_skinned(
                            &mut shadow_pass,
                            skin_objects,
                            keys[index],
                            index,
                            slot,
                            SkinnedInputs::cutout_caster(key),
                        );
                    }
                    switched |= solid || cut;
                }
                if switched {
                    shadow_pass.set_pipeline(&self.shadow_pipeline);
                }
                // Roads cast too — an embankment's shadow across the valley
                // below it is most of what makes an elevated road read as
                // elevated. The pipeline is unchanged: it reads only the model
                // matrix, which is why roads share the object uniform array.
                for index in 0..roads.len() {
                    cast(
                        &mut shadow_pass,
                        items.len() + index,
                        &self.meshes[&road_keys[index]],
                    );
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
        // Refraction splits the frame the same way and for the same reason: a
        // pass cannot sample the colour attachment it is drawing into (M26).
        // Gated on the scene actually refracting something, so with none the
        // pass structure, the attachments and the load/store ops are byte for
        // byte the pre-M26 ones.
        let refracting: Vec<bool> = items.iter().map(|item| item.material.refracts()).collect();
        let refraction_present = refracting.iter().any(|&yes| yes);
        let split_pass = water_present || refraction_present;
        if refraction_present {
            SceneColor::ensure(
                &mut self.scene_color,
                device,
                self.format,
                target_size[0],
                target_size[1],
            );
        }
        if water_present {
            SceneDepth::ensure(&mut self.scene_depth, device, target_size[0], target_size[1]);
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

        // Group 2 for every lit pipeline, rebuilt only when one of its textures
        // was — allocating the shadow map, or a resize reallocating a copy.
        self.ensure_frame_textures(device, environment.shadows);
        let held = self.frame_textures.as_ref().expect("just ensured");
        let frame_textures = &held.bind_group;
        let opaque_frame_textures = &held.opaque_bind_group;

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("scene-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    // Refraction with no MSAA has nowhere to resolve *from*, so
                    // the opaque geometry is drawn straight into the copy and
                    // blitted across before the blended pass. With MSAA the
                    // multisampled attachment stays the target and the copy is
                    // its resolve, which costs nothing extra: the resolve was
                    // going to happen anyway, only later.
                    view: match (refraction_present, msaa) {
                        (true, None) => &self.scene_color.as_ref().expect("ensured above").view,
                        _ => color_view,
                    },
                    // With a split pass the resolve happens at the end of the
                    // *second* pass instead, so the multisampled color survives
                    // to be drawn into again.
                    resolve_target: match (refraction_present, msaa) {
                        (true, Some(_)) => {
                            Some(&self.scene_color.as_ref().expect("ensured above").view)
                        }
                        _ if split_pass => None,
                        _ => resolve_target,
                    },
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
                        store: if split_pass {
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

            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(1, &self.frame_uniform.bind_group, &[]);
            pass.set_bind_group(2, opaque_frame_textures, &[]);

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

                // Terrain draws in one run at the **front** of the opaque pass,
                // so the pipeline switches at most once a frame rather than
                // once an entity, and a scene with no terrain never leaves
                // `self.pipeline` — the first loop does nothing and the second
                // is the pre-terrain code exactly.
                //
                // Front and not back, and this is load-bearing: a 200k-triangle
                // ground patch as the *last* draw of an MSAA render pass
                // renders **nondeterministically** on Metal (measured on an
                // M3 Pro: 2–3 distinct images over 20 runs of one unchanged
                // file, ~24 pixels apart, wherever the patch meets other
                // geometry within a pixel). The shadow map and the depth copy
                // come back bit-identical run to run, so what varies is which
                // surface wins MSAA samples 1–3 in the pass's final draw. Any
                // draw after it removes the variance — drawing the ground
                // first, splitting its index range in two, or even re-drawing
                // one small mesh behind it all render the same bytes every
                // time. Ground first is the one of those that is also right on
                // its own terms: everything in a scene stands *on* the terrain,
                // so a contact surface exactly coplanar with it should tie in
                // favour of the object, which is what drawing the object second
                // under `Less` gives. Reproduce with `bin/verify-baselines
                // --filter showcase` run repeatedly, or by rendering one file
                // 20 times and `md5`-ing the PNGs.
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
                for &index in &opaque {
                    if items[index].terrain.is_none()
                        && material_keys[index].is_none()
                        && skin_slots[index].is_none()
                    {
                        draw(&mut pass, index);
                    }
                }

                // Textured draws last in the opaque run, in one group, for the
                // reason terrain draws first in one group: the pipeline
                // switches at most once a frame rather than once an entity. A
                // scene with no maps never enters this loop and therefore never
                // leaves `self.pipeline` — which is what keeps every baseline
                // blessed before M26 issuing exactly the draws it always did.
                let mut textured = false;
                for &index in &opaque {
                    let Some(key) = material_keys[index] else { continue };
                    if skin_slots[index].is_some() {
                        continue;
                    }
                    if !textured {
                        pass.set_pipeline(&self.textured_pipeline);
                        textured = true;
                    }
                    let mesh = &self.meshes[&keys[index]];
                    pass.set_bind_group(
                        0,
                        &objects.bind_group,
                        &[(index as u64 * self.object_stride) as u32],
                    );
                    pass.set_bind_group(3, &self.materials[&key].bind_group, &[]);
                    pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                    pass.set_vertex_buffer(1, mesh.vertices.slice(mesh.normals_offset..));
                    pass.set_vertex_buffer(2, mesh.vertices.slice(mesh.uvs_offset..));
                    pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..mesh.index_count, 0, 0..1);
                }
                // And the skinned draws (M27), each in its own run for the
                // reason terrain and the textured meshes get one: the pipeline
                // switches at most once a frame. A scene with no skinned mesh
                // never enters either loop, and its `self.skinned` was never
                // built.
                if let (Some(skinned), Some(objects)) = (&self.skinned, &self.skinned_objects) {
                    let mut plain = false;
                    for &index in &opaque {
                        let Some(slot) = skin_slots[index] else {
                            continue;
                        };
                        if material_keys[index].is_some() {
                            continue;
                        }
                        if !plain {
                            pass.set_pipeline(&skinned.opaque);
                            plain = true;
                        }
                        self.draw_skinned(
                            &mut pass,
                            objects,
                            keys[index],
                            index,
                            slot,
                            SkinnedInputs::LIT,
                        );
                    }
                    let mut mapped = false;
                    for &index in &opaque {
                        let Some(slot) = skin_slots[index] else {
                            continue;
                        };
                        let Some(key) = material_keys[index] else {
                            continue;
                        };
                        if !mapped {
                            pass.set_pipeline(&skinned.textured);
                            mapped = true;
                        }
                        self.draw_skinned(
                            &mut pass,
                            objects,
                            keys[index],
                            index,
                            slot,
                            SkinnedInputs::textured(key),
                        );
                    }
                }
                // Nothing after this assumes a bound pipeline: the road block
                // and `draw_blended` each set their own.
                let _ = textured;
            }

            // Roads, still in the opaque pass: they write depth like any solid
            // surface, so water absorbs against them and particles sort against
            // them correctly.
            if let (false, Some(objects), Some(surfaces)) =
                (roads.is_empty(), &self.objects, &self.road_objects)
            {
                pass.set_pipeline(&self.road_pipeline);
                pass.set_bind_group(1, &self.frame_uniform.bind_group, &[]);
                pass.set_bind_group(2, opaque_frame_textures, &[]);
                for (index, _) in roads.iter().enumerate() {
                    let mesh = &self.meshes[&road_keys[index]];
                    pass.set_bind_group(
                        0,
                        &objects.bind_group,
                        &[((items.len() + index) as u64 * self.object_stride) as u32],
                    );
                    pass.set_bind_group(
                        3,
                        &surfaces.bind_group,
                        &[(index as u64 * self.road_stride) as u32],
                    );
                    pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                    pass.set_vertex_buffer(1, mesh.vertices.slice(mesh.normals_offset..));
                    pass.set_vertex_buffer(2, mesh.vertices.slice(mesh.uvs_offset..));
                    pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..mesh.index_count, 0, 0..1);
                }
                // Nothing after this assumes a bound pipeline: `draw_blended`
                // sets its own for the first item it draws.
            }

            // Blended geometry after every opaque surface has written depth, so
            // it is occluded by what is in front of it, and back-to-front among
            // itself. With water or refraction in the scene this waits for the
            // second pass, where the copies behind it are readable.
            if !split_pass {
                self.draw_blended(
                    &mut pass,
                    &blended,
                    &keys,
                    &water_keys,
                    &cloud_keys,
                    &material_keys,
                    &refracting,
                    &skin_slots,
                    opaque_frame_textures,
                );
                self.draw_particles(&mut pass, particle_count, alpha_particles);
            }
        }

        if split_pass {
            // The opaque colour is in the copy but not on the frame, on the one
            // path where the copy *was* the attachment.
            if refraction_present && msaa.is_none() {
                let mut blit = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("scene-color-blit"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: color_view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(clear),
                            store: wgpu::StoreOp::Store,
                        },
                        depth_slice: None,
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                blit.set_pipeline(&self.blit_pipeline);
                blit.set_bind_group(0, frame_textures, &[]);
                blit.draw(0..3, 0..1);
            }

            if water_present {
                let scene_depth = self.scene_depth.as_ref().expect("ensured above");
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
                label: Some("blended-pass"),
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

            self.draw_blended(
                &mut pass,
                &blended,
                &keys,
                &water_keys,
                &cloud_keys,
                &material_keys,
                &refracting,
                &skin_slots,
                frame_textures,
            );
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
        self.materials
            .retain(|_, material| frame_index - material.last_used < MESH_CACHE_LIFETIME);
        self.textures
            .retain(|_, texture| frame_index - texture.last_used < MESH_CACHE_LIFETIME);
    }

    /// Build group 2 — the shadow map, the depth copy, the colour copy — if it
    /// is not already the one this frame needs.
    ///
    /// The key is the identity of what it holds, so a steady-state frame
    /// rebuilds nothing: the group changes when a scene starts casting shadows,
    /// when a copy is first allocated, and when the target resizes.
    fn ensure_frame_textures(&mut self, device: &wgpu::Device, shadows: bool) {
        let shadows = shadows && self.shadow_map.is_some();
        let key = FrameTextureKey {
            shadows,
            depth: self.scene_depth.as_ref().map(|d| (d.width, d.height)),
            color: self.scene_color.as_ref().map(|c| (c.width, c.height)),
        };
        if self.frame_textures.as_ref().is_some_and(|held| held.key == key) {
            return;
        }

        let shadow_view = match (shadows, &self.shadow_map) {
            (true, Some(map)) => &map.view,
            _ => &self.shadow_placeholder.view,
        };
        let depth_view = self
            .scene_depth
            .as_ref()
            .map_or(&self.depth_placeholder, |copy| &copy.view);
        let color_view = self
            .scene_color
            .as_ref()
            .map_or(&self.color_placeholder, |copy| &copy.view);

        let group = |label, colour: &wgpu::TextureView| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout: &self.frame_textures_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(shadow_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.shadow_sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(depth_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(colour),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::Sampler(&self.scene_sampler),
                    },
                ],
            })
        };
        self.frame_textures = Some(FrameTextures {
            bind_group: group("frame-textures", color_view),
            opaque_bind_group: group("frame-textures-opaque", &self.color_placeholder),
            key,
        });
    }

    /// Draw the blended list — transparent meshes and water surfaces
    /// interleaved, back-to-front.
    ///
    /// Switching between the pipelines re-binds their groups, because the
    /// pipeline layouts differ (clouds have only two groups) and a pipeline
    /// change with an incompatible layout invalidates what was bound. Tracking
    /// the previous kind keeps that to one switch per run of same-kind items.
    /// One skinned draw: group 0 carries both dynamic offsets, and the mesh
    /// binds its two extra vertex slots.
    ///
    /// `inputs` says which of the optional slots this pipeline declares: the
    /// untextured variants declare no `@location(2)`, so binding a UV buffer
    /// for them would be providing an attribute nothing reads, and the solid
    /// caster reads position alone.
    fn draw_skinned(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        objects: &SkinnedObjects,
        mesh_key: usize,
        index: usize,
        slot: usize,
        inputs: SkinnedInputs,
    ) {
        let mesh = &self.meshes[&mesh_key];
        let Some(skin) = mesh.skin_offsets else {
            // A palette with no influences behind it cannot skin anything;
            // `skin_slots` already refuses to build one, so this is belt and
            // braces rather than a live path.
            return;
        };
        pass.set_bind_group(
            0,
            &objects.bind_group,
            &[
                (index as u64 * self.object_stride) as u32,
                (slot as u64 * self.palette_stride) as u32,
            ],
        );
        pass.set_vertex_buffer(0, mesh.vertices.slice(..));
        let mut next = 1;
        if inputs.normal {
            pass.set_vertex_buffer(next, mesh.vertices.slice(mesh.normals_offset..));
            next += 1;
        }
        if let Some((key, group)) = inputs.material {
            pass.set_bind_group(group, &self.materials[&key].bind_group, &[]);
            pass.set_vertex_buffer(next, mesh.vertices.slice(mesh.uvs_offset..));
            next += 1;
        }
        pass.set_vertex_buffer(next, mesh.vertices.slice(skin.joints..));
        pass.set_vertex_buffer(next + 1, mesh.vertices.slice(skin.weights..));
        pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..mesh.index_count, 0, 0..1);
    }

    fn draw_blended(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        blended: &[Blended],
        keys: &[usize],
        water_keys: &[usize],
        cloud_keys: &[usize],
        material_keys: &[Option<[usize; 4]>],
        refracting: &[bool],
        skin_slots: &[Option<usize>],
        frame_textures: &wgpu::BindGroup,
    ) {
        // 0 = transparent mesh, 1 = water, 2 = cloud, 3 = textured transparent
        // mesh.
        let mut current: Option<u8> = None;
        for entry in blended {
            match *entry {
                Blended::Mesh(index) => {
                    let Some(objects) = &self.objects else { continue };
                    let material = material_keys[index];
                    // 0 = plain, 3 = textured, 4 = plain refracting. A textured
                    // material refracts through its own pipeline, which carries
                    // both producers: there is no baseline to protect on that
                    // path, so it does not need splitting the way the plain one
                    // did. 5, 6 and 7 are the skinned twins of the three, which
                    // exist so that a transparent skinned surface is a
                    // *transparent skinned surface* rather than a silently
                    // rest-posed one.
                    let skinned = skin_slots[index];
                    let kind = match (material.is_some(), refracting[index], skinned.is_some()) {
                        (true, _, false) => 3,
                        (false, true, false) => 4,
                        (false, false, false) => 0,
                        (true, _, true) => 5,
                        (false, true, true) => 6,
                        (false, false, true) => 7,
                    };
                    if current != Some(kind) {
                        let Some(pipeline) = (match kind {
                            3 => Some(&self.textured_transparent_pipeline),
                            4 => Some(&self.refractive_pipeline),
                            0 => Some(&self.transparent_pipeline),
                            _ => self.skinned.as_ref().map(|s| match kind {
                                5 => &s.textured_transparent,
                                6 => &s.refractive,
                                _ => &s.transparent,
                            }),
                        }) else {
                            continue;
                        };
                        pass.set_pipeline(pipeline);
                        pass.set_bind_group(1, &self.frame_uniform.bind_group, &[]);
                        pass.set_bind_group(2, frame_textures, &[]);
                        current = Some(kind);
                    }
                    if let Some(slot) = skinned {
                        let Some(skin_objects) = &self.skinned_objects else {
                            continue;
                        };
                        self.draw_skinned(
                            pass,
                            skin_objects,
                            keys[index],
                            index,
                            slot,
                            match material {
                                Some(key) => SkinnedInputs::textured(key),
                                None => SkinnedInputs::LIT,
                            },
                        );
                        continue;
                    }
                    let mesh = &self.meshes[&keys[index]];
                    pass.set_bind_group(
                        0,
                        &objects.bind_group,
                        &[(index as u64 * self.object_stride) as u32],
                    );
                    pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                    pass.set_vertex_buffer(1, mesh.vertices.slice(mesh.normals_offset..));
                    if let Some(key) = material {
                        pass.set_bind_group(3, &self.materials[&key].bind_group, &[]);
                        pass.set_vertex_buffer(2, mesh.vertices.slice(mesh.uvs_offset..));
                    }
                    pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..mesh.index_count, 0, 0..1);
                }
                Blended::Water(index) => {
                    let Some(surfaces) = &self.water_objects else {
                        continue;
                    };
                    if current != Some(1) {
                        pass.set_pipeline(&self.water_pipeline);
                        pass.set_bind_group(1, &self.frame_uniform.bind_group, &[]);
                        pass.set_bind_group(2, frame_textures, &[]);
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

    /// Upload a material map with its whole mip chain if this is the first time
    /// it has been seen, and return its cache key.
    ///
    /// The chain was generated on the CPU at load — see
    /// `engine_core::texture::TextureData` for why — so this is a write per
    /// level and no blit chain. The **format is decided by the colour space the
    /// slot loaded it in**, which is the one place that decision becomes a GPU
    /// fact: sRGB for a colour map so the sampler decodes, linear for data.
    fn upload_texture(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        data: &Arc<engine_core::texture::TextureData>,
    ) -> usize {
        let key = Arc::as_ptr(data) as usize;
        let frame_index = self.frame_index;
        self.textures
            .entry(key)
            .and_modify(|texture| texture.last_used = frame_index)
            .or_insert_with(|| {
                let format = match data.space {
                    engine_core::texture::ColorSpace::Srgb => wgpu::TextureFormat::Rgba8UnormSrgb,
                    engine_core::texture::ColorSpace::Linear => wgpu::TextureFormat::Rgba8Unorm,
                };
                let texture = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("material-map"),
                    size: wgpu::Extent3d {
                        width: data.width,
                        height: data.height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: data.mips.len() as u32,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                });
                for (level, pixels) in data.mips.iter().enumerate() {
                    let (width, height) = data.level_size(level);
                    queue.write_texture(
                        wgpu::TexelCopyTextureInfo {
                            texture: &texture,
                            mip_level: level as u32,
                            origin: wgpu::Origin3d::ZERO,
                            aspect: wgpu::TextureAspect::All,
                        },
                        pixels,
                        wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(width * 4),
                            rows_per_image: None,
                        },
                        wgpu::Extent3d {
                            width,
                            height,
                            depth_or_array_layers: 1,
                        },
                    );
                }
                CachedTexture {
                    _source: Arc::clone(data),
                    view: texture.create_view(&wgpu::TextureViewDescriptor::default()),
                    last_used: frame_index,
                }
            });
        key
    }

    /// The bind group for one material's maps, uploading anything new.
    ///
    /// Returns `None` for a material with no maps at all — such a draw goes to
    /// the plain pipeline, which has no group 3 to bind.
    fn ensure_material(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        maps: &engine_core::texture::MaterialTextures,
    ) -> Option<[usize; 4]> {
        if !maps.any() {
            return None;
        }
        let slots = [&maps.albedo, &maps.orm, &maps.normal, &maps.emissive];
        let key = slots.map(|slot| match slot {
            Some(data) => self.upload_texture(device, queue, data),
            None => 0,
        });

        let frame_index = self.frame_index;
        if let Some(held) = self.materials.get_mut(&key) {
            held.last_used = frame_index;
            return Some(key);
        }

        if self.white_texture.is_none() {
            self.white_texture = Some(white_texture(device, queue));
        }
        let white = self.white_texture.as_ref().expect("just made");
        let views: Vec<&wgpu::TextureView> = key
            .iter()
            .map(|slot| match self.textures.get(slot) {
                Some(texture) => &texture.view,
                None => white,
            })
            .collect();
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("material-maps"),
            layout: &self.material_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(views[0]),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(views[1]),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(views[2]),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(views[3]),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::Sampler(&self.map_sampler),
                },
            ],
        });
        self.materials.insert(
            key,
            CachedMaterial {
                bind_group,
                last_used: frame_index,
            },
        );
        Some(key)
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
                // Positions, normals and UVs share one buffer in that order, so
                // a single allocation serves all three vertex slots.
                let mut vertex_bytes = Vec::with_capacity(
                    (geometry.positions.len() + geometry.normals.len()) * 12
                        + geometry.positions.len() * 8,
                );
                vertex_bytes.extend_from_slice(bytemuck::cast_slice(&geometry.positions));
                let normals_offset = vertex_bytes.len() as u64;
                vertex_bytes.extend_from_slice(bytemuck::cast_slice(&geometry.normals));
                let uvs_offset = vertex_bytes.len() as u64;
                vertex_bytes.extend_from_slice(bytemuck::cast_slice(&geometry.uvs));
                // A glTF mesh may carry no UVs at all; pad so the slot is
                // always bindable rather than making the layout conditional.
                let missing = geometry.positions.len().saturating_sub(geometry.uvs.len());
                vertex_bytes.resize(vertex_bytes.len() + missing * 8, 0);

                // The two skinning slots, appended only for a skinned mesh
                // (M27) — which is what keeps every buffer committed before it
                // byte-identical to what it always was.
                let skin_offsets = geometry.is_skinned().then(|| {
                    let joints = vertex_bytes.len() as u64;
                    vertex_bytes.extend_from_slice(bytemuck::cast_slice(&geometry.joint_indices));
                    let weights = vertex_bytes.len() as u64;
                    vertex_bytes.extend_from_slice(bytemuck::cast_slice(&geometry.joint_weights));
                    SkinOffsets { joints, weights }
                });

                CachedMesh {
                    _geometry: Arc::clone(geometry),
                    vertices: buffer_with(
                        device,
                        "mesh-vertices",
                        wgpu::BufferUsages::VERTEX,
                        &vertex_bytes,
                    ),
                    normals_offset,
                    uvs_offset,
                    skin_offsets,
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

impl SkinnedObjects {
    /// Make sure the palette buffer holds `size` bytes and that the group-0
    /// bind group names it beside the current object buffer.
    ///
    /// Rebuilt only when one of the two buffers was reallocated, which the
    /// recorded capacities detect: a bind group holds its buffers by identity,
    /// so a draw list that outgrew the object buffer would otherwise keep
    /// binding the freed one.
    fn ensure(
        slot: &mut Option<Self>,
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        objects: &Uniforms,
        size: u64,
    ) {
        let fits = slot
            .as_ref()
            .is_some_and(|held| held.palette_size >= size && held.objects_size == objects.size);
        if fits {
            return;
        }

        let palette = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("joint-palettes"),
            size: size.max(std::mem::size_of::<JointPaletteUniform>() as u64),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group =
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("skinned-object-uniforms"),
                layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: &objects.buffer,
                            offset: 0,
                            size: std::num::NonZeroU64::new(
                                std::mem::size_of::<ObjectUniform>() as u64
                            ),
                        }),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: &palette,
                            offset: 0,
                            size: std::num::NonZeroU64::new(
                                std::mem::size_of::<JointPaletteUniform>() as u64,
                            ),
                        }),
                    },
                ],
            });
        *slot = Some(Self {
            palette_size: size,
            objects_size: objects.size,
            palette,
            bind_group,
        });
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

/// Pack one road's marking parameters for the shader (M23).
///
/// The road's geometry, dash period and kerb spans were all decided when the
/// ribbon was generated — this only copies them into the uniform, which is why
/// a road that is not being edited costs one small write per frame however long
/// it is.
fn road_uniform(item: &RoadItem) -> RoadUniform {
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

// ── The surface-resolution seam ──────────────────────────────────────────────
//
// M22 discovered this extension point and did not name it; M26 has a second
// producer at it, so it is named here.
//
// Everything in `mesh.wgsl` after a surface is resolved — the GGX lobe, the
// shadow lookup, the sky ambient, the point-light loop, the fog, the blend — is
// shared, and what varies is only where `albedo`, `metallic`, `roughness`,
// `emissive` and the shading normal come from. A producer supplies those; the
// lighting body never changes.
//
// What a producer may *not* do is edit `mesh.wgsl`: M16's four untouchable lines
// have to reach the compiler surrounded by the code they already shipped in, and
// putting terrain's branch inline moved one pixel by one unit in each of three
// committed fixtures (see `shaders/terrain.wgsl`). So the file on disk stays
// what it was, the plain mesh pipeline compiles it unchanged — byte-identical by
// construction, not by measurement — and a variant is assembled by anchored
// substitution. Every anchor is asserted to appear exactly once: reword
// `mesh.wgsl` and this fails loudly at startup rather than silently rendering
// terrain as flat grey or a texture as untextured.

/// The anchors a producer may replace, as they appear in `mesh.wgsl`.
mod anchor {
    /// The object uniform's last field, where a variant appends its own.
    pub const UNIFORM_TAIL: &str = "    // x = alpha, y = transmission; z and w unused.\n\
                                    \x20   surface: vec4<f32>,\n\
                                    };\n";
    /// The whole vertex stage. Unlike every other anchor this one is not
    /// *replaced* by a producer but **reassembled** from their
    /// [`VertexContribution`](super::VertexContribution)s, because since M27
    /// two producers need it at once: texturing needs a UV attribute the plain
    /// stage does not carry, and skinning needs two more. Whole-stage
    /// replacement worked while exactly one producer did it and does not
    /// survive two — and a textured skinned character is precisely the case
    /// that has to compose. The assembly with no contributions is asserted to
    /// be this string, byte for byte.
    pub const VERTEX_STAGE: &str = "struct VertexOut {\n\
        \x20   @builtin(position) clip_position: vec4<f32>,\n\
        \x20   @location(0) world_position: vec3<f32>,\n\
        \x20   @location(1) normal: vec3<f32>,\n\
        };\n\
        \n\
        @vertex\n\
        fn vs_main(\n\
        \x20   @location(0) position: vec3<f32>,\n\
        \x20   @location(1) normal: vec3<f32>,\n\
        ) -> VertexOut {\n\
        \x20   var out: VertexOut;\n\
        \x20   out.clip_position = object.mvp * vec4<f32>(position, 1.0);\n\
        \x20   out.world_position = (object.model * vec4<f32>(position, 1.0)).xyz;\n\
        \x20   out.normal = (object.normal_matrix * vec4<f32>(normal, 0.0)).xyz;\n\
        \x20   return out;\n\
        }\n";
    /// The fragment prologue: where albedo, metallic and emissive come from.
    pub const PROLOGUE: &str = "    let albedo = object.albedo_metallic.rgb;\n\
                                \x20   let metallic = object.albedo_metallic.w;\n\
                                \x20   let emissive = object.emissive_roughness.rgb;\n";
    pub const NORMAL: &str = "    let n = normalize(in.normal);\n";
    pub const ROUGHNESS: &str = "    let roughness = max(object.emissive_roughness.w, 0.045);\n";
    /// One of M16's four untouchable lines. A variant may replace it —
    /// the plain pipeline still compiles the file as it sits on disk — and
    /// occlusion is the only thing that has ever needed to.
    pub const AMBIENT: &str = "    let ambient = albedo * frame.ambient.rgb;\n";
    /// Its counterpart inside the sky branch.
    pub const FILL: &str = "            fill = albedo * hemisphere;\n";
    /// The frame uniform's tail, where the refraction variant appends the
    /// view-projection it needs to project an exit point.
    pub const FRAME_TAIL: &str =
        "    point_lights: array<PointLightData, MAX_POINT_LIGHTS>,\n};\n";
    /// Where the fragment's running colour and alpha are declared.
    pub const VARS: &str = "    var color = base_color;\n    var out_alpha = 1.0;\n";
    /// The last line of the fragment stage.
    pub const RETURN: &str =
        "    return vec4<f32>(clamp(color, vec3<f32>(0.0), vec3<f32>(1.0)), out_alpha);\n";
    /// The composite for a transmissive surface — the one place that decides
    /// what is seen *through* it.
    pub const BLENDED: &str = "            color = (lit_diffuse + fill) * out_alpha + lit_specular + reflection + emissive;\n";
}

/// The types the extended object uniform's tail names, declared by every
/// variant because its field *offsets* are positional — a variant that skipped
/// the terrain table would read the material's UV transform where the table is.
const EXTENDED_UNIFORM_TYPES: &str = "const MAX_TERRAIN_LAYERS: u32 = 4u;\n\
    \n\
    // One material a terrain paints itself with, claiming a band of world\n\
    // height and a band of slope (M22).\n\
    struct TerrainLayer {\n\
    \x20   // rgb = linear albedo, w = roughness.\n\
    \x20   albedo_roughness: vec4<f32>,\n\
    \x20   // x, y = world-Y band in metres; z, w = slope band in degrees.\n\
    \x20   bands: vec4<f32>,\n\
    \x20   // x = height fade in metres, y = boundary jitter, z = slope fade\n\
    \x20   // in degrees; w unused.\n\
    \x20   blend_noise: vec4<f32>,\n\
    };\n";

/// The extended object uniform every spliced variant declares.
///
/// One tail rather than one per producer, because uniform field offsets are
/// positional: a variant that declared only *its* fields would read terrain's
/// layer table where the material's UV transform is. Each producer uses what it
/// needs and ignores the rest; the plain pipeline declares none of it and reads
/// the shorter struct out of the same buffer, which is legal precisely because
/// every field it does read is at the offset it always was.
const EXTENDED_UNIFORM_TAIL: &str = "    // x = alpha, y = transmission; z and w unused.\n\
    \x20   surface: vec4<f32>,\n\
    \x20   // Terrain (M22), appended at the end so every field above keeps\n\
    \x20   // the offset the shader already reads it from. x = live layer\n\
    \x20   // count, y = texture scale in metres, z = colour variation,\n\
    \x20   // w = bump.\n\
    \x20   terrain: vec4<f32>,\n\
    \x20   // x = the terrain's seed; y, z, w unused.\n\
    \x20   terrain_seed: vec4<u32>,\n\
    \x20   terrain_layers: array<TerrainLayer, MAX_TERRAIN_LAYERS>,\n\
    \x20   // Material maps (M26): xy = uv scale, zw = uv offset.\n\
    \x20   map_uv: vec4<f32>,\n\
    \x20   // x = which maps are bound (bit 0 albedo, 1 orm, 2 normal,\n\
    \x20   // 3 emissive), y = alpha cutoff, z = normal strength, w = ior.\n\
    \x20   map_params: vec4<f32>,\n\
    \x20   // x = thickness in metres; yzw = per-channel attenuation.\n\
    \x20   map_volume: vec4<f32>,\n\
    };\n";

/// Assemble a variant of the mesh shader: the shared lighting body, with one
/// producer spliced in at the surface-resolution seam.
///
/// `prelude` goes ahead of the object uniform (a producer's own declarations
/// often reference its fields); `substitutions` are (anchor, replacement) pairs.
fn with_surface(producers: &[Producer]) -> std::borrow::Cow<'static, str> {
    let source = include_str!("shaders/mesh.wgsl");

    let mut out = source.to_string();
    let substitutions = || producers.iter().flat_map(|p| p.substitutions.iter());
    for (anchor, _) in substitutions().chain([
        &(anchor::UNIFORM_TAIL, EXTENDED_UNIFORM_TAIL),
        // Not in any producer's substitution list — it is reassembled rather
        // than replaced — but just as load-bearing, so it is asserted here.
        &(anchor::VERTEX_STAGE, ""),
    ]) {
        assert_eq!(
            source.matches(anchor).count(),
            1,
            "mesh.wgsl no longer contains this anchor exactly once, so a spliced \
             pipeline variant would compile as if its feature were absent:\n{anchor}"
        );
    }

    out = out.replace(anchor::UNIFORM_TAIL, EXTENDED_UNIFORM_TAIL);
    let preludes: String = producers
        .iter()
        .map(|p| p.prelude)
        .collect::<Vec<_>>()
        .join("\n");
    out = out.replace(
        "struct ObjectUniform {",
        &format!("{EXTENDED_UNIFORM_TYPES}\n{preludes}\nstruct ObjectUniform {{"),
    );
    out = out.replace(
        anchor::VERTEX_STAGE,
        &vertex_stage(producers.iter().filter_map(|p| p.vertex.as_ref())),
    );
    for (anchor, replacement) in substitutions() {
        out = out.replace(anchor, replacement);
    }

    std::borrow::Cow::Owned(out)
}

/// One producer at the seam: declarations to put ahead of the object uniform,
/// its addition to the vertex stage, and the anchored substitutions that route
/// the fragment stage through it.
///
/// Composable, because a textured surface can also refract and can also be
/// skinned. Composition is what keeps the variant matrix from being a matrix of
/// hand-written shaders.
struct Producer {
    prelude: &'static str,
    /// What this producer adds to the vertex stage, if anything. Terrain and
    /// refraction add nothing: they resolve a surface per pixel.
    vertex: Option<VertexContribution>,
    substitutions: Vec<(&'static str, &'static str)>,
}

/// One producer's addition to the vertex stage.
///
/// Every field is a fragment of WGSL pasted into a fixed skeleton, and an
/// all-empty set of contributions assembles to [`anchor::VERTEX_STAGE`]
/// verbatim — which is the property that keeps the plain pipeline compiling
/// `mesh.wgsl` as it sits on disk. `producers_compose_without_a_hand_written_stage`
/// pins it.
#[derive(Default)]
struct VertexContribution {
    /// Extra `vs_main` parameters: whole `    @location(n) name: T,\n` lines.
    attributes: &'static str,
    /// Extra `VertexOut` fields, in the same shape.
    varyings: &'static str,
    /// Statements run before the position and normal are transformed.
    body: &'static str,
    /// The expression transformed in place of the `position` attribute — how a
    /// producer moves a vertex. At most one producer may set it.
    position: Option<&'static str>,
    /// And in place of `normal`, for the same reason and under the same rule.
    normal: Option<&'static str>,
    /// Statements run after `out` is filled, writing the extra varyings.
    tail: &'static str,
}

/// Assemble the vertex stage from its contributions, in producer order.
///
/// Ordered, and the order is the meaning: skinning moves the vertex and
/// texturing then passes that position through untouched, so a producer list
/// of `[skin, texture]` is a skinned textured surface rather than two
/// half-applied ones.
fn vertex_stage<'a>(contributions: impl Iterator<Item = &'a VertexContribution> + Clone) -> String {
    let concat = |field: fn(&VertexContribution) -> &'static str| -> String {
        contributions.clone().map(field).collect()
    };
    // A second producer moving the vertex would silently win or silently lose
    // depending on iteration order; neither is a thing to discover from a
    // render.
    let one = |what: &str, field: fn(&VertexContribution) -> Option<&'static str>| {
        let mut found = contributions.clone().filter_map(field);
        let first = found.next();
        assert!(
            found.next().is_none(),
            "two producers both claim to compute the vertex {what}; the stage \
             can only transform one expression"
        );
        first
    };

    let varyings = concat(|c| c.varyings);
    let attributes = concat(|c| c.attributes);
    let body = concat(|c| c.body);
    let tail = concat(|c| c.tail);
    let position = one("position", |c| c.position).unwrap_or("position");
    let normal = one("normal", |c| c.normal).unwrap_or("normal");

    format!(
        "struct VertexOut {{\n\
         \x20   @builtin(position) clip_position: vec4<f32>,\n\
         \x20   @location(0) world_position: vec3<f32>,\n\
         \x20   @location(1) normal: vec3<f32>,\n\
         {varyings}}};\n\
         \n\
         @vertex\n\
         fn vs_main(\n\
         \x20   @location(0) position: vec3<f32>,\n\
         \x20   @location(1) normal: vec3<f32>,\n\
         {attributes}) -> VertexOut {{\n\
         \x20   var out: VertexOut;\n\
         {body}\x20   out.clip_position = object.mvp * vec4<f32>({position}, 1.0);\n\
         \x20   out.world_position = (object.model * vec4<f32>({position}, 1.0)).xyz;\n\
         \x20   out.normal = (object.normal_matrix * vec4<f32>({normal}, 0.0)).xyz;\n\
         {tail}\x20   return out;\n\
         }}\n"
    )
}

/// The terrain producer (M22): a generative material in place of the uniform's.
fn terrain_producer() -> Producer {
    Producer {
        prelude: include_str!("shaders/terrain.wgsl"),
        // Terrain resolves its material per pixel from the world position the
        // plain stage already interpolates, so it adds nothing to the vertex
        // stage.
        vertex: None,
        substitutions: vec![
            (
                anchor::PROLOGUE,
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
            ),
            (anchor::NORMAL, "    let n = generated.normal;\n"),
            (
                anchor::ROUGHNESS,
                "    let roughness = max(generated.roughness, 0.045);\n",
            ),
        ],
    }
}

/// The texture producer (M26).
///
/// The second producer at the seam, and the one that proves the seam is worth
/// naming: it needs a vertex attribute the plain stage does not carry, and it
/// touches the occlusion half of the ambient terms, and it still shares every
/// line of the lighting body.
fn texture_producer() -> Producer {
    Producer {
        prelude: include_str!("shaders/textured.wgsl"),
        // A UV attribute the plain stage does not carry, interpolated to the
        // fragment stage under the material's own scale and offset.
        vertex: Some(VertexContribution {
            attributes: "    @location(2) uv: vec2<f32>,\n",
            varyings: "    @location(2) uv: vec2<f32>,\n",
            tail: "    out.uv = uv * object.map_uv.xy + object.map_uv.zw;\n",
            ..VertexContribution::default()
        }),
        substitutions: vec![
            (
                anchor::PROLOGUE,
                "    let sampled = sample_maps(in.uv);\n\
                 \x20   // A cut pixel leaves before anything is lit — and its\n\
                 \x20   // shadow leaves through the caster pipeline that runs\n\
                 \x20   // this same test.\n\
                 \x20   if object.map_params.y > 0.0 && sampled.alpha < object.map_params.y {\n\
                 \x20       discard;\n\
                 \x20   }\n\
                 \x20   let albedo = object.albedo_metallic.rgb * sampled.albedo;\n\
                 \x20   let metallic = object.albedo_metallic.w * sampled.metallic;\n\
                 \x20   let emissive = object.emissive_roughness.rgb * sampled.emissive;\n",
            ),
            (
                anchor::NORMAL,
                "    let n = perturb_normal(\n\
                 \x20       normalize(in.normal),\n\
                 \x20       in.world_position,\n\
                 \x20       in.uv,\n\
                 \x20   );\n",
            ),
            (
                anchor::ROUGHNESS,
                "    let roughness = max(object.emissive_roughness.w * sampled.roughness, 0.045);\n",
            ),
            // Occlusion multiplies the ambient terms and **never** the direct
            // sun: that is the whole difference between ambient occlusion and a
            // second shadow map.
            (
                anchor::AMBIENT,
                "    let ambient = albedo * frame.ambient.rgb * sampled.occlusion;\n",
            ),
            (
                anchor::FILL,
                "            fill = albedo * hemisphere * sampled.occlusion;\n",
            ),
        ],
    }
}

/// The refraction producer (M26), for the blended pipelines.
///
/// It replaces exactly one line — the composite for a transmissive surface —
/// and appends one field to the frame uniform. A surface with `ior: 1.0` and
/// `thickness: 0.0` takes the `else` and lands on the pre-M26 expression
/// unchanged, which is what lets the blended pipelines compile this variant
/// without moving the ice in any committed fixture.
fn refraction_producer() -> Producer {
    Producer {
        prelude: include_str!("shaders/refraction.wgsl"),
        // Refraction is entirely a fragment-stage decision: it bends a view
        // ray, it does not move a vertex.
        vertex: None,
        substitutions: vec![
            (
                anchor::FRAME_TAIL,
                "    point_lights: array<PointLightData, MAX_POINT_LIGHTS>,\n\
                 \x20   // World → clip (M26): the refraction variant projects an\n\
                 \x20   // exit point with it. Appended at the end, so every field\n\
                 \x20   // above keeps the offset the other shaders read it from.\n\
                 \x20   view_proj: mat4x4<f32>,\n\
                 };\n",
            ),
            (
                anchor::VARS,
                "    var color = base_color;\n\
                 \x20   var out_alpha = 1.0;\n\
                 \x20   // What a refracting surface lets through, held out of the\n\
                 \x20   // running colour until after fog. The copy it comes from\n\
                 \x20   // was already fogged at its own depth when the opaque pass\n\
                 \x20   // drew it, and fogging it a second time here is what turns\n\
                 \x20   // clear ice into a pale slab.\n\
                 \x20   var transmitted = vec3<f32>(0.0);\n",
            ),
            (
                anchor::BLENDED,
                "            if object.map_params.w != 1.0 || object.map_volume.x > 0.0 {\n\
                 \x20               // The frame behind this surface, bent and\n\
                 \x20               // absorbed. Taken from the copy rather than\n\
                 \x20               // left to the blend unit, which is the whole\n\
                 \x20               // difference: a blend can only show what is\n\
                 \x20               // straight behind, and refraction is about\n\
                 \x20               // what is *not*.\n\
                 \x20               let uv = refracted_uv(\n\
                 \x20                   in.world_position,\n\
                 \x20                   v,\n\
                 \x20                   n,\n\
                 \x20                   object.map_params.w,\n\
                 \x20                   object.map_volume.x,\n\
                 \x20               );\n\
                 \x20               transmitted = absorbed(\n\
                 \x20                   textureSampleLevel(scene_color, scene_sampler, uv, 0.0).rgb,\n\
                 \x20                   object.map_volume.yzw,\n\
                 \x20                   object.map_volume.x,\n\
                 \x20               ) * (1.0 - out_alpha);\n\
                 \x20           }\n\
                 \x20           color = (lit_diffuse + fill) * out_alpha + lit_specular + reflection + emissive;\n",
            ),
            (
                anchor::RETURN,
                "    // The transmitted background last, and the surface opaque\n\
                 \x20   // once it carries it: the blend unit must not add the\n\
                 \x20   // framebuffer's *un*-refracted version on top.\n\
                 \x20   if any(transmitted > vec3<f32>(0.0)) {\n\
                 \x20       color = color + transmitted;\n\
                 \x20       out_alpha = 1.0;\n\
                 \x20   }\n\
                 \x20   return vec4<f32>(clamp(color, vec3<f32>(0.0), vec3<f32>(1.0)), out_alpha);\n",
            ),
        ],
    }
}

/// The skinning producer (M27).
///
/// The only producer so far that *moves* a vertex, which is why the vertex
/// stage had to become an assembly: it needs two attributes the plain stage
/// does not carry, and so does texturing, and a rigged character is precisely
/// the thing that wants both.
fn skin_producer() -> Producer {
    Producer {
        prelude: include_str!("shaders/skin.wgsl"),
        vertex: Some(VertexContribution {
            attributes: "    @location(3) joint_indices: vec4<u32>,\n\
                         \x20   @location(4) joint_weights: vec4<f32>,\n",
            body:
                "    let skinned = skin_vertex(position, normal, joint_indices, joint_weights);\n",
            position: Some("skinned.position"),
            normal: Some("skinned.normal"),
            ..VertexContribution::default()
        }),
        // Nothing in the fragment stage: a skinned surface is shaded exactly
        // like any other, which is the point of doing this in the vertex
        // stage at all.
        substitutions: Vec::new(),
    }
}

/// The two shadow casters, skinned (M27).
///
/// `shadow.wgsl` reads nothing but the object uniform's model matrix, so a
/// walking character would otherwise cast its rest pose — a wrongness that
/// reads as a renderer bug and is actually a missing pipeline. Spliced rather
/// than copied, for `with_surface`'s reason: two more hand-maintained shadow
/// shaders are two more things to drift out of step with the pass they belong
/// to.
///
/// Note for whoever debugs a missing shadow here: the solid caster is
/// **front-face culled** (M16's peeling margin), which applies to characters
/// as much as to M26's single-sided cards.
fn with_skinned_caster(source: &'static str, anchor: &str, replacement: &str) -> String {
    assert_eq!(
        source.matches(anchor).count(),
        1,
        "the shadow caster no longer contains this anchor exactly once, so the \
         skinned variant would compile as if skinning were absent:\n{anchor}"
    );
    format!(
        "{}\n{}",
        include_str!("shaders/skin.wgsl"),
        source.replace(anchor, replacement)
    )
}

fn skinned_shadow() -> String {
    with_skinned_caster(
        include_str!("shaders/shadow.wgsl"),
        "fn vs_main(@location(0) position: vec3<f32>) -> @builtin(position) vec4<f32> {\n\
         \x20   return frame.light_view_proj * object.model * vec4<f32>(position, 1.0);\n\
         }\n",
        "fn vs_main(\n\
         \x20   @location(0) position: vec3<f32>,\n\
         \x20   @location(3) joint_indices: vec4<u32>,\n\
         \x20   @location(4) joint_weights: vec4<f32>,\n\
         ) -> @builtin(position) vec4<f32> {\n\
         \x20   let skinned = skin_vertex(position, vec3<f32>(0.0, 1.0, 0.0), joint_indices, joint_weights);\n\
         \x20   return frame.light_view_proj * object.model * vec4<f32>(skinned.position, 1.0);\n\
         }\n",
    )
}

fn skinned_shadow_cutout() -> String {
    with_skinned_caster(
        include_str!("shaders/shadow_cutout.wgsl"),
        "    out.clip = frame.light_view_proj * object.model * vec4<f32>(position, 1.0);\n",
        "    let skinned = skin_vertex(position, normal, joint_indices, joint_weights);\n\
         \x20   out.clip = frame.light_view_proj * object.model * vec4<f32>(skinned.position, 1.0);\n",
    )
    .replace(
        "    @location(2) uv: vec2<f32>,\n) -> VertexOut {",
        "    @location(2) uv: vec2<f32>,\n\
         \x20   @location(3) joint_indices: vec4<u32>,\n\
         \x20   @location(4) joint_weights: vec4<f32>,\n\
         ) -> VertexOut {",
    )
}

/// The mesh shader with terrain's generative material spliced in (M22).
fn with_terrain() -> std::borrow::Cow<'static, str> {
    with_surface(&[terrain_producer()])
}

/// The mesh shader with texture sampling spliced in (M26).
fn with_textures() -> std::borrow::Cow<'static, str> {
    with_surface(&[texture_producer()])
}

/// The blended twins, which also refract.
fn with_refraction() -> std::borrow::Cow<'static, str> {
    with_surface(&[refraction_producer()])
}

fn with_textures_and_refraction() -> std::borrow::Cow<'static, str> {
    with_surface(&[texture_producer(), refraction_producer()])
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

#[cfg(test)]
mod seam_tests {
    /// The anchors are asserted at pipeline build, but only for *presence*.
    /// This pins that each producer's substitution actually landed — a splice
    /// that silently did nothing renders the feature as if it were absent,
    /// which is the failure mode hardest to spot in a render.
    #[test]
    fn every_producer_actually_replaces_what_it_claims() {
        let terrain = super::with_terrain();
        assert!(terrain.contains("let albedo = generated.albedo;"));
        assert!(terrain.contains("let n = generated.normal;"));
        assert!(terrain.contains("terrain_layers: array<TerrainLayer"));

        let textured = super::with_textures();
        for expected in [
            "let sampled = sample_maps(in.uv);",
            "sampled.metallic",
            "sampled.roughness",
            "sampled.occlusion",
            "let n = perturb_normal(",
            "@location(2) uv: vec2<f32>,",
            "map_params: vec4<f32>,",
        ] {
            assert!(textured.contains(expected), "textured splice lost {expected:?}");
        }
        // And the shared lighting body is still the one from the file.
        assert!(textured.contains("let base_color = direct + ambient + emissive;"));
    }

    /// The property the whole vertex-stage assembly rests on (M27): with no
    /// contributions it reproduces the stage `mesh.wgsl` ships, byte for byte.
    ///
    /// Without this the refactor would be a rewrite of the one mechanism that
    /// guards M16's four untouchable lines, verified by hoping.
    #[test]
    fn an_unassisted_vertex_stage_is_the_one_in_the_file() {
        assert_eq!(
            super::vertex_stage(std::iter::empty()),
            super::anchor::VERTEX_STAGE,
        );
        // And a producer that adds nothing to the vertex stage leaves it the
        // file's, which is what kept M22's and M26's committed baselines from
        // moving when the assembly replaced whole-stage substitution.
        for variant in [super::with_terrain(), super::with_refraction()] {
            assert!(
                variant.contains(super::anchor::VERTEX_STAGE),
                "a producer with no vertex contribution must leave the stage alone"
            );
        }
    }

    /// Two producers both needing the vertex stage is what forced the assembly
    /// (M27 §7): a rigged character is precisely the thing that also wants an
    /// albedo map, so the two must compose rather than compete.
    #[test]
    fn producers_compose_without_a_hand_written_stage() {
        let both = super::with_surface(&[super::skin_producer(), super::texture_producer()]);

        for expected in [
            // Both attributes reached `vs_main`…
            "    @location(2) uv: vec2<f32>,\n",
            "    @location(3) joint_indices: vec4<u32>,\n",
            "    @location(4) joint_weights: vec4<f32>,\n",
            // …and the skinned position is what gets transformed, once.
            "out.clip_position = object.mvp * vec4<f32>(skinned.position, 1.0);",
            "out.world_position = (object.model * vec4<f32>(skinned.position, 1.0)).xyz;",
            "out.normal = (object.normal_matrix * vec4<f32>(skinned.normal, 0.0)).xyz;",
            "out.uv = uv * object.map_uv.xy + object.map_uv.zw;",
            // …and the fragment stage still samples maps.
            "let sampled = sample_maps(in.uv);",
        ] {
            assert!(both.contains(expected), "composed stage lost {expected:?}");
        }
        assert!(
            !both.contains("object.mvp * vec4<f32>(position, 1.0)"),
            "the unskinned position must not survive alongside the skinned one"
        );
    }

    /// The joint palette reaches the GPU as three *rows*, and glam matrices are
    /// stored as columns (M27).
    ///
    /// A transpose dropped here is the kind of bug that renders — a character
    /// appears, moves when the clip moves it, and is inside out — so it is
    /// pinned by arithmetic rather than by looking.
    #[test]
    fn a_palette_entry_packs_as_rows_not_columns() {
        let matrix = glam::Mat4::from_scale_rotation_translation(
            glam::Vec3::new(1.0, 2.0, 3.0),
            glam::Quat::from_rotation_y(0.7),
            glam::Vec3::new(4.0, 5.0, 6.0),
        );
        let packed = super::JointPaletteUniform::from_palette(&[matrix]);

        // The translation is the *fourth element of each row*, which is where
        // the shader's `dot(row, vec4(position, 1.0))` picks it up. Under a
        // column packing it would be the first three elements of row 3, which
        // is not even stored.
        assert_eq!(
            [
                packed.joints[0][0][3],
                packed.joints[0][1][3],
                packed.joints[0][2][3]
            ],
            [4.0, 5.0, 6.0],
        );
        // And the whole transform agrees with the matrix, for a point that is
        // not the origin.
        let point = glam::Vec3::new(0.3, -1.1, 2.0);
        let expected = matrix.transform_point3(point);
        let homogeneous = point.extend(1.0);
        let by_rows = glam::Vec3::new(
            glam::Vec4::from_array(packed.joints[0][0]).dot(homogeneous),
            glam::Vec4::from_array(packed.joints[0][1]).dot(homogeneous),
            glam::Vec4::from_array(packed.joints[0][2]).dot(homogeneous),
        );
        assert!(
            (by_rows - expected).length() < 1e-5,
            "packed rows transform to {by_rows:?}, the matrix to {expected:?}"
        );
    }

    /// Every slot past the rig's joint count is zero, and nothing indexes them
    /// — validation refused the file that could (`too_many_joints`).
    #[test]
    fn unused_palette_slots_are_zeroed() {
        let packed = super::JointPaletteUniform::from_palette(&[glam::Mat4::IDENTITY]);
        assert_eq!(packed.joints[1], [[0.0; 4]; 3]);
        assert_eq!(packed.joints[super::MAX_JOINTS - 1], [[0.0; 4]; 3]);
    }
}
