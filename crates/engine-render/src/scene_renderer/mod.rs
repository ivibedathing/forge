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
//!
//! # Layout
//!
//! This was one 5,989-line file until it was split, and the split was **pure
//! code motion**: not one expression changed, which is the only form of this
//! refactor worth trusting on a file whose arithmetic CLAUDE.md flags as
//! ULP-sensitive in four separate places. The A/B between binaries came back
//! 34 of 36 artifacts byte-identical, the two exceptions being the tour frames
//! this adapter cannot render reproducibly at all.
//!
//! - [`uniforms`] — every `#[repr(C)]` struct the GPU sees, and the functions
//!   that pack a component into one. Field order here is a wire format:
//!   it matches a WGSL struct declaration somewhere, positionally.
//! - [`shaders`] — the WGSL assembly seam. `with_surface` and its `Producer`s
//!   splice `mesh.wgsl` without editing it, which is what keeps M16's four
//!   untouchable lines reaching the compiler surrounded by the code they
//!   shipped in. Its `seam_tests` pin that every substitution actually lands.
//! - [`pipelines`] — `with_samples` and the per-pass pipeline constructors.
//!   The sample count is baked in, so building a pipeline belongs to the
//!   renderer's construction rather than to a frame.
//! - [`resources`] — the caches (mesh, texture, material, meadow) and the
//!   frame-scoped GPU resources: depth, colour copy, HUD targets, and the
//!   grow-in-place buffer helpers.
//! - [`shadow`] — the shadow map and the matrix math behind it, including
//!   `clamp_shadow_elevation`, which is why a sun on the horizon does not cast
//!   shadows of unbounded length.
//!
//! What stayed here is the `SceneRenderer` struct, [`ScenePass`], and the
//! frame itself: `draw` and the five functions it orchestrates.
//!
//! **`draw` splits on the borrow, not on the passes.** `prepare` is the half
//! that needs `&mut self` — it uploads geometry, packs every uniform, and
//! maintains the caches — and it hands the recording half a [`FramePlan`];
//! `record_shadows`, `record_scene` and `record_hud` need only `&self` and an
//! encoder. `prepare_frame_targets` sits between the first two because it
//! both decides the frame's attachments and *allocates* the copies that
//! decision needs, and it must keep its position: the order these run in is
//! the order the GPU sees, and the caster pass runs before anything is shaded
//! because the mesh pass samples what it writes.
//!
//! [`FramePlan`] exists so those two halves can be separate functions without
//! an argument list nobody can check — it is built with field-init shorthand
//! and destructured by name at every use, so a field cannot be wired to the
//! wrong local. That is not tidiness: several of its fields are the same type,
//! and a swapped pair of `Vec<usize>` keys is the one error class here that
//! compiles clean and renders wrong.
//!
//! Submodules are children of this one, so they see its private items without
//! ceremony; what they define is `pub(crate)` and glob-imported back here, so
//! call sites read exactly as they did when everything shared a file.

mod pipelines;
mod resources;
mod shaders;
mod shadow;
mod uniforms;

use pipelines::*;
use resources::*;
use shaders::*;
use shadow::*;
use uniforms::*;

use std::collections::HashMap;
use std::sync::Arc;

use engine_core::components::{Camera, ParticleBlend, Terrain, Water, MAX_POINT_LIGHTS};
use engine_core::math::{Mat4, Vec3};
use engine_core::meadow::MAX_GROWTH_STAGES;
use engine_core::mesh::MeshData;
use engine_core::particles::ParticleInstance;
use engine_core::road::MAX_ROAD_KERBS;
use engine_core::scene::{
    CloudItem, EnvironmentSettings, MeadowItem, RenderItem, ResolvedLights, RoadItem, WaterItem,
};
use engine_core::skeleton::MAX_JOINTS;
use engine_core::terrain::MAX_TERRAIN_LAYERS;
use engine_core::water::MAX_WAVES;

pub const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// Everything one scene render needs beyond the device and queue.
#[derive(Clone, Copy)]
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
    /// Meadows (M29), drawn with the opaque geometry after the roads. Pass
    /// `&[]` for a scene with no ground cover, which then issues exactly the
    /// draws it did before meadows existed.
    pub meadows: &'a [MeadowItem],
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
    /// The scene's baked irradiance field, folded against this frame's lighting
    /// (M35). `None` for a scene with no `LightProbeVolume`, which is then lit
    /// by exactly the expressions that lit it before GI existed.
    ///
    /// Folded by the *caller* rather than here, because the fold is a pure CPU
    /// function of (bake file, sky, ambient) and `engine gi-probe` has to be
    /// able to run it without a GPU. M21's arrangement: the model is CPU, and
    /// the GPU only ever reads its output.
    pub gi: Option<&'a engine_core::gi::IrradianceField>,
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
    /// The same with refraction spliced in (M27), used only by a surface whose
    /// `ior` is not 1. Chosen per surface, so an unrefracting pond in a scene
    /// that also holds a refracting one still compiles to the M18 shader.
    refractive_water_pipeline: wgpu::RenderPipeline,
    cloud_pipeline: wgpu::RenderPipeline,
    /// Roads (M23): opaque, shadow-casting, and the only pipeline that reads a
    /// vertex UV — a road's markings are painted from surface coordinates.
    road_pipeline: wgpu::RenderPipeline,
    road_layout: wgpu::BindGroupLayout,
    /// Meadows (M29): opaque, instanced, double-sided, and the only pipeline
    /// whose vertex stage decides what the geometry *is*.
    meadow_pipeline: wgpu::RenderPipeline,
    meadow_layout: wgpu::BindGroupLayout,
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
    /// Group 0 for a skinned draw (M30): the object uniform at binding 0, as
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
    /// Whether the mesh pipelines were compiled with the GI producer (M35).
    /// Baked into the shaders, so it belongs to the renderer rather than to a
    /// frame — `samples`' rule, for the same reason.
    gi: bool,

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
    /// Uploaded meadows, keyed on the `Arc<MeadowPatch>` identity — the mesh
    /// cache's rule, for the same reason.
    meadow_meshes: HashMap<usize, CachedMeadow>,
    /// Uploaded material maps, keyed on the `Arc<TextureData>` identity — M15's
    /// rule, which is why `TextureSource` must hand back the same `Arc`.
    textures: HashMap<usize, CachedTexture>,
    /// Material bind groups, keyed on the four slots' texture identities.
    materials: HashMap<[usize; 4], CachedMaterial>,
    /// The 1×1 white bound in a slot with no map, made on the first textured
    /// frame — a scene with no maps never allocates it.
    white_texture: Option<wgpu::TextureView>,
    /// The uploaded irradiance field (M35), allocated the first frame a volume
    /// arrives. `None` in every scene without one, which is nearly all of them.
    gi_field: Option<GiTextures>,
    /// Bound at the four GI slots whenever `gi_field` is `None`.
    gi_placeholder: wgpu::TextureView,
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
    /// The skinned set (M30), absent until a frame has a skinned draw.
    skinned: Option<SkinnedPipelines>,
    skinned_objects: Option<SkinnedObjects>,
    /// Distance between consecutive joint palettes: 6 KiB rounded up to the
    /// device's dynamic-offset alignment, which on every adapter this runs on
    /// leaves it exactly 6 KiB.
    palette_stride: u64,
    /// And again for meadows, whose life-cycle table is the largest uniform of
    /// the four.
    meadow_objects: Option<Uniforms>,
    meadow_stride: u64,
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

/// Everything `prepare` computed that the recording half then reads.
///
/// This exists so the two halves can be separate functions without an argument
/// list nobody can check. Construction is field-init shorthand and every
/// consumer destructures by name, so a field cannot be wired to the wrong
/// local — which matters because several of these are the same type, and a
/// swapped pair of `Vec<usize>` keys would compile clean and render wrong.
struct FramePlan<'a> {
    opaque: Vec<usize>,
    blended: Vec<Blended>,
    keys: Vec<usize>,
    material_keys: Vec<Option<[usize; 4]>>,
    skin_slots: Vec<Option<usize>>,
    cutout: Vec<bool>,
    road_keys: Vec<usize>,
    water_keys: Vec<usize>,
    cloud_keys: Vec<usize>,
    meadow_keys: Vec<usize>,
    particle_count: u32,
    alpha_particles: u32,
    hud_canvases: Vec<&'a crate::hud::HudCanvas>,
}

/// The frame's attachment choices and pass-split decisions.
///
/// Computed after the caster pass and before the scene pass, in that order,
/// because it also *allocates*: the colour copy, the depth copy and the
/// depth-resolve bind group. Keeping that order is what keeps the recorded
/// command stream identical to the one this was split out of.
struct FrameTargets<'a> {
    color_view: &'a wgpu::TextureView,
    resolve_target: Option<&'a wgpu::TextureView>,
    water_present: bool,
    refracting: Vec<bool>,
    refracting_water: Vec<bool>,
    refraction_present: bool,
    split_pass: bool,
}

impl SceneRenderer {
    /// A renderer for single-sampled targets — the pre-MSAA constructor, kept
    /// because most callers (tests, the editor viewport) have no scene to ask.
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        Self::with_samples(device, format, 1)
    }

    pub fn format(&self) -> wgpu::TextureFormat {
        self.format
    }

    /// The MSAA sample count baked into this renderer's pipelines. A caller
    /// whose scene now asks for a different one has to build a new renderer.
    pub fn samples(&self) -> u32 {
        self.samples
    }

    /// Whether this renderer's mesh pipelines carry the GI producer (M35).
    /// `samples()`'s twin, and a caller whose scene has gained or lost a
    /// `LightProbeVolume` has to build a new renderer for the same reason.
    pub fn gi_enabled(&self) -> bool {
        self.gi
    }

    /// Upload a draw list and render it.
    ///
    /// Geometry uploads once and is reused: `items` carry shared
    /// `Arc<MeshData>`, and this keeps the GPU buffers for each one alive
    /// across frames (see the module doc). Per-frame work is one uniform
    /// write per pass plus the draw calls themselves.
    /// Record and submit one frame.
    ///
    /// Two halves, and the split is the borrow: `prepare` needs `&mut self` to
    /// upload geometry, pack uniforms and maintain the caches, and hands the
    /// recording half a [`FramePlan`]; the recorders need only `&self` and an
    /// encoder. The order below is the order the GPU sees it, and it is
    /// load-bearing — the caster pass runs before anything is shaded because
    /// the mesh pass samples what it writes.
    pub fn draw(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, pass: ScenePass<'_>) {
        self.frame_index += 1;

        let plan = self.prepare(device, queue, &pass);

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("scene-encoder"),
        });

        self.record_shadows(&mut encoder, &pass, &plan);
        let targets = self.prepare_frame_targets(device, &pass);
        self.record_scene(&mut encoder, &pass, &plan, &targets);
        self.record_hud(&mut encoder, &pass, &plan);

        queue.submit(Some(encoder.finish()));

        let frame_index = self.frame_index;
        self.meshes
            .retain(|_, mesh| frame_index - mesh.last_used < MESH_CACHE_LIFETIME);
        self.materials
            .retain(|_, material| frame_index - material.last_used < MESH_CACHE_LIFETIME);
        self.textures
            .retain(|_, texture| frame_index - texture.last_used < MESH_CACHE_LIFETIME);
    }

    /// Upload, pack and sort everything this frame needs, and return what the
    /// recording half reads back out. The `&mut self` half of [`Self::draw`].
    fn prepare<'a>(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pass: &ScenePass<'a>,
    ) -> FramePlan<'a> {
        let ScenePass {
            items,
            water,
            clouds,
            roads,
            meadows,
            particles,
            view_projection,
            camera_position,
            camera_right,
            camera_up,
            lights,
            environment,
            time,
            hud,
            gi,
            ..
        } = *pass;

        // Allocate the field's planes if this volume's grid is new, and write
        // this frame's fold into them. The textures are reallocated only when
        // the grid changes; the contents are rewritten every frame, because
        // under `daylight` the sky moves every frame and the fold is what
        // carries that into GI.
        if let Some(field) = gi {
            if GiTextures::ensure(&mut self.gi_field, device, field.grid) {
                // New planes mean new view identities, so group 2 no longer
                // names the right textures. Dropping it is what makes
                // `ensure_frame_textures` rebuild.
                self.frame_textures = None;
            }
            self.gi_field
                .as_ref()
                .expect("just ensured")
                .upload(queue, field);
        }

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
            gi_origin: match gi {
                Some(field) => field.origin.extend(field.spacing).to_array(),
                None => [0.0; 4],
            },
            gi_grid: match gi {
                Some(field) => [
                    field.grid[0] as f32,
                    field.grid[1] as f32,
                    field.grid[2] as f32,
                    field.intensity,
                ],
                None => [0.0; 4],
            },
            // The `1.0` is the only thing that turns GI on in a shader that was
            // compiled with it: a scene whose pipelines carry the producer but
            // whose field failed to arrive renders the fallback, rather than
            // sampling a placeholder and going black.
            gi_params: [
                gi.map_or(0.0, |f| f.blend),
                f32::from(gi.is_some()),
                0.0,
                0.0,
            ],
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
        // Not a no-op, whatever clippy's `drop_non_drop` says: the closure
        // holds a mutable borrow of `object_bytes`, and this is what releases
        // it so the buffer can be read below. Deleting the line does not
        // compile.
        #[allow(clippy::drop_non_drop)]
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

        // Meadows: the same arrangement once more. Their templates and instance
        // buffers join a cache of their own, keyed on `Arc<MeadowPatch>`
        // identity, so a field that is not being edited uploads once for the
        // life of the run — the life cycle is a uniform and a vertex-stage
        // evaluation, never new geometry.
        let meadow_keys: Vec<usize> = meadows
            .iter()
            .map(|item| self.upload_meadow(device, &item.patch))
            .collect();
        if !meadows.is_empty() {
            let stride = self.meadow_stride as usize;
            let mut meadow_bytes = vec![0u8; stride * meadows.len()];
            for (index, item) in meadows.iter().enumerate() {
                let uniform = meadow_uniform(item, view_projection, time);
                let at = index * stride;
                meadow_bytes[at..at + std::mem::size_of::<MeadowUniform>()]
                    .copy_from_slice(bytemuck::bytes_of(&uniform));
            }
            let objects = Uniforms::ensure(
                &mut self.meadow_objects,
                device,
                &self.meadow_layout,
                "meadow-uniforms",
                meadow_bytes.len() as u64,
                Some(std::mem::size_of::<MeadowUniform>() as u64),
            );
            queue.write_buffer(&objects.buffer, 0, &meadow_bytes);
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

        FramePlan {
            opaque,
            blended,
            keys,
            material_keys,
            skin_slots,
            cutout,
            road_keys,
            water_keys,
            cloud_keys,
            meadow_keys,
            particle_count,
            alpha_particles,
            hud_canvases,
        }
    }

    /// The caster pass, before anything is shaded.
    fn record_shadows(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        pass: &ScenePass<'_>,
        plan: &FramePlan<'_>,
    ) {
        let ScenePass {
            items,
            roads,
            environment,
            ..
        } = *pass;
        let FramePlan {
            opaque,
            keys,
            material_keys,
            skin_slots,
            cutout,
            road_keys,
            ..
        } = plan;

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
                for &index in opaque {
                    if cutout[index] || skin_slots[index].is_some() {
                        continue;
                    }
                    cast(&mut shadow_pass, index, &self.meshes[&keys[index]]);
                }
                // Cut-out casters after the solid ones, in one run: the switch
                // costs one pipeline change a frame, and a scene with no
                // `alpha_cutoff` never enters this loop.
                let mut switched = false;
                for &index in opaque {
                    if !cutout[index] || skin_slots[index].is_some() {
                        continue;
                    }
                    let Some(key) = material_keys[index] else {
                        continue;
                    };
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
                    for &index in opaque {
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
                    for &index in opaque {
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
                for (index, road_key) in road_keys.iter().enumerate().take(roads.len()) {
                    cast(
                        &mut shadow_pass,
                        items.len() + index,
                        &self.meshes[road_key],
                    );
                }
            }
        }
    }

    /// Choose the frame's attachments and decide whether the pass splits,
    /// allocating the copies that decision needs.
    fn prepare_frame_targets<'a>(
        &mut self,
        device: &wgpu::Device,
        pass: &ScenePass<'a>,
    ) -> FrameTargets<'a> {
        let ScenePass {
            target,
            msaa,
            depth,
            target_size,
            items,
            water,
            environment,
            ..
        } = *pass;

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
        // Water refracts through the same copy since M27, so it joins the
        // disjunction rather than getting a second one: a scene with neither a
        // refracting material nor a refracting surface still renders the
        // pre-M26 pass structure exactly.
        let refracting_water: Vec<bool> = water.iter().map(|item| item.water.refracts()).collect();
        let refraction_present =
            refracting.iter().any(|&yes| yes) || refracting_water.iter().any(|&yes| yes);
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
            SceneDepth::ensure(
                &mut self.scene_depth,
                device,
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

        // Group 2 for every lit pipeline, rebuilt only when one of its textures
        // was — allocating the shadow map, or a resize reallocating a copy.
        self.ensure_frame_textures(device, environment.shadows);

        FrameTargets {
            color_view,
            resolve_target,
            water_present,
            refracting,
            refracting_water,
            refraction_present,
            split_pass,
        }
    }

    /// The scene pass: sky, opaque geometry, the depth and colour copies, the
    /// back-to-front blended run, and particles.
    fn record_scene(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        pass: &ScenePass<'_>,
        plan: &FramePlan<'_>,
        targets: &FrameTargets<'_>,
    ) {
        let ScenePass {
            msaa,
            depth,
            items,
            roads,
            meadows,
            environment,
            clear,
            ..
        } = *pass;
        let FramePlan {
            opaque,
            blended,
            keys,
            material_keys,
            skin_slots,
            road_keys,
            water_keys,
            cloud_keys,
            meadow_keys,
            particle_count,
            alpha_particles,
            ..
        } = plan;
        let FrameTargets {
            color_view,
            resolve_target,
            water_present,
            refracting,
            refracting_water,
            refraction_present,
            split_pass,
            ..
        } = targets;
        // Destructuring a reference yields references; the `Copy` scalars
        // are taken by value so the body below reads as it always did.
        let (particle_count, alpha_particles) = (*particle_count, *alpha_particles);
        let (color_view, resolve_target) = (*color_view, *resolve_target);
        let (water_present, refraction_present, split_pass) =
            (*water_present, *refraction_present, *split_pass);

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
                for &index in opaque {
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
                for &index in opaque {
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
                for &index in opaque {
                    let Some(key) = material_keys[index] else {
                        continue;
                    };
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
                    for &index in opaque {
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
                    for &index in opaque {
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

            // Meadows, last in the opaque run. They write depth like the grass
            // they are: a plant standing in front of a rock occludes it, water
            // absorbs against the field, and particles sort against it.
            //
            // Drawing *after* the terrain is not merely tidy — M22 measured that
            // this adapter renders a big relief patch differently run to run
            // when it is the last thing in an MSAA pass, and that any draw after
            // it removes the flake. A meadow is also the finest geometry this
            // engine has ever put against that same relief, so which way this
            // actually falls is measured per fixture rather than assumed; see
            // the design doc's §9.
            if let Some(uniforms) = &self.meadow_objects {
                if !meadows.is_empty() {
                    pass.set_pipeline(&self.meadow_pipeline);
                    pass.set_bind_group(1, &self.frame_uniform.bind_group, &[]);
                    pass.set_bind_group(2, opaque_frame_textures, &[]);
                    for (index, _) in meadows.iter().enumerate() {
                        let meadow = &self.meadow_meshes[&meadow_keys[index]];
                        if meadow.instance_count == 0 {
                            continue;
                        }
                        pass.set_bind_group(
                            0,
                            &uniforms.bind_group,
                            &[(index as u64 * self.meadow_stride) as u32],
                        );
                        pass.set_vertex_buffer(0, meadow.vertices.slice(..));
                        pass.set_vertex_buffer(1, meadow.instances.slice(..));
                        pass.set_index_buffer(meadow.indices.slice(..), wgpu::IndexFormat::Uint32);
                        pass.draw_indexed(0..meadow.index_count, 0, 0..meadow.instance_count);
                    }
                    // Nothing after this assumes a bound pipeline: `draw_blended`
                    // sets its own for the first item it draws.
                }
            }

            // Blended geometry after every opaque surface has written depth, so
            // it is occluded by what is in front of it, and back-to-front among
            // itself. With water or refraction in the scene this waits for the
            // second pass, where the copies behind it are readable.
            if !split_pass {
                self.draw_blended(
                    &mut pass,
                    blended,
                    keys,
                    water_keys,
                    cloud_keys,
                    material_keys,
                    refracting,
                    skin_slots,
                    refracting_water,
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
                blended,
                keys,
                water_keys,
                cloud_keys,
                material_keys,
                refracting,
                skin_slots,
                refracting_water,
                frame_textures,
            );
            self.draw_particles(&mut pass, particle_count, alpha_particles);
        }
    }

    /// The overlay, composited last onto the resolved target.
    fn record_hud(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        pass: &ScenePass<'_>,
        plan: &FramePlan<'_>,
    ) {
        let ScenePass { target, .. } = *pass;
        let FramePlan { hud_canvases, .. } = plan;

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
            gi: self.gi_field.as_ref().map(|f| f.grid),
        };
        if self
            .frame_textures
            .as_ref()
            .is_some_and(|held| held.key == key)
        {
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
        // Four views either way: the field's planes when a volume is resident,
        // the 1x1x1 stand-in otherwise. WGSL binds unconditionally, and a
        // variant that never reads these still needs them present.
        let gi_views: [&wgpu::TextureView; engine_core::gi::SH_L1_COEFFS] =
            match self.gi_field.as_ref() {
                Some(field) => std::array::from_fn(|i| &field.views[i]),
                None => std::array::from_fn(|_| &self.gi_placeholder),
            };

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
                ]
                .into_iter()
                .chain(
                    gi_views
                        .iter()
                        .enumerate()
                        .map(|(i, view)| wgpu::BindGroupEntry {
                            binding: 5 + i as u32,
                            resource: wgpu::BindingResource::TextureView(view),
                        }),
                )
                .chain([wgpu::BindGroupEntry {
                    binding: 9,
                    // The same sampler object as binding 4 under a second name:
                    // linear and clamped on every axis is what both a refraction
                    // offset and a probe fetch want. See `gi.wgsl` for why it
                    // cannot simply share the binding.
                    resource: wgpu::BindingResource::Sampler(&self.scene_sampler),
                }])
                .collect::<Vec<_>>(),
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

    /// Eleven parameters, and clippy would rather they were a struct. They are
    /// nine parallel slices indexed by the *same* `Blended` index, built once
    /// per frame by `draw` and read here in one pass. A struct of borrows would
    /// be built and destructured for a single call on the hot path, and the
    /// index-alignment invariant — the one thing that can actually go wrong
    /// here — would be no better expressed for it.
    #[allow(clippy::too_many_arguments)]
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
        refracting_water: &[bool],
        frame_textures: &wgpu::BindGroup,
    ) {
        // 0 = transparent mesh, 1 = water, 2 = cloud, 3 = textured transparent
        // mesh, 5 = refracting water.
        let mut current: Option<u8> = None;
        for entry in blended {
            match *entry {
                Blended::Mesh(index) => {
                    let Some(objects) = &self.objects else {
                        continue;
                    };
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
                    // Per surface (M27), not per scene: a pond at the default
                    // `ior: 1.0` keeps the M18 shader even when another body of
                    // water in the same frame refracts.
                    let kind = if refracting_water[index] { 5 } else { 1 };
                    if current != Some(kind) {
                        pass.set_pipeline(match kind {
                            5 => &self.refractive_water_pipeline,
                            _ => &self.water_pipeline,
                        });
                        pass.set_bind_group(1, &self.frame_uniform.bind_group, &[]);
                        pass.set_bind_group(2, frame_textures, &[]);
                        current = Some(kind);
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
                    let Some(objects) = &self.cloud_objects else {
                        continue;
                    };
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

    /// Upload a meadow if this is the first time it has been seen, and return
    /// its cache key (the shared allocation's address).
    ///
    /// All three buffers are static for the life of the patch — the life cycle
    /// runs in the vertex stage — so a meadow that is not being edited uploads
    /// once for the life of the run, however many generations pass in front of
    /// the camera.
    fn upload_meadow(
        &mut self,
        device: &wgpu::Device,
        patch: &Arc<engine_core::meadow::MeadowPatch>,
    ) -> usize {
        let key = Arc::as_ptr(patch) as usize;
        let frame_index = self.frame_index;
        self.meadow_meshes
            .entry(key)
            .and_modify(|meadow| meadow.last_used = frame_index)
            .or_insert_with(|| {
                // engine-core carries no `bytemuck` (it is the GPU-free crate),
                // so the POD mirrors live here and the conversion happens once,
                // at upload.
                let vertices: Vec<MeadowVertexRaw> = patch
                    .vertices
                    .iter()
                    .map(|v| MeadowVertexRaw {
                        centre: v.centre,
                        normal: v.normal,
                        offset: v.offset,
                        anchor: v.anchor,
                        span: v.span,
                        organ: v.organ,
                    })
                    .collect();
                let instances: Vec<MeadowInstanceRaw> = patch
                    .instances
                    .iter()
                    .map(|i| MeadowInstanceRaw {
                        pos_scale: [i.position[0], i.position[1], i.position[2], i.scale],
                        params: [
                            i.yaw,
                            i.phase_offset,
                            i.ground_gradient[0],
                            i.ground_gradient[1],
                        ],
                        seed: i.seed,
                    })
                    .collect();

                CachedMeadow {
                    _patch: Arc::clone(patch),
                    vertices: buffer_with(
                        device,
                        "meadow-template",
                        wgpu::BufferUsages::VERTEX,
                        bytemuck::cast_slice(&vertices),
                    ),
                    indices: buffer_with(
                        device,
                        "meadow-indices",
                        wgpu::BufferUsages::INDEX,
                        bytemuck::cast_slice(&patch.indices),
                    ),
                    instances: buffer_with(
                        device,
                        "meadow-instances",
                        wgpu::BufferUsages::VERTEX,
                        bytemuck::cast_slice(&instances),
                    ),
                    index_count: patch.indices.len() as u32,
                    instance_count: patch.instances.len() as u32,
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

/// One entry in the back-to-front blended list: an index into the draw list's
/// transparent meshes, its water surfaces, or its clouds.
#[derive(Clone, Copy)]
enum Blended {
    Mesh(usize),
    Water(usize),
    Cloud(usize),
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
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
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
