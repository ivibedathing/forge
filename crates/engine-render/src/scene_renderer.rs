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

use engine_core::components::Camera;
use engine_core::math::{Mat4, Vec3};
use engine_core::mesh::MeshData;
use engine_core::particles::ParticleInstance;
use engine_core::scene::{RenderItem, ResolvedLights};

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
}

/// Per-pass particle data, matching WGSL `ParticleFrame`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ParticleFrameUniform {
    view_proj: [[f32; 4]; 4],
    camera_right: [f32; 4],
    camera_up: [f32; 4],
}

/// One particle billboard as the instance buffer carries it.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ParticleRaw {
    /// xyz = world position, w = half-size.
    pos_size: [f32; 4],
    /// rgb = linear color, a = opacity.
    color: [f32; 4],
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

/// Everything one scene render needs beyond the device and queue.
pub struct ScenePass<'a> {
    pub target: &'a wgpu::TextureView,
    pub depth: &'a wgpu::TextureView,
    pub items: &'a [RenderItem],
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
    pub clear: wgpu::Color,
    /// Screen-space overlay, composited after the mesh pass (M12). Must be
    /// rasterized at the target's dimensions. `None` skips the overlay pass
    /// entirely, so HUD-less scenes render byte-identically to pre-M12.
    pub hud: Option<&'a crate::hud::HudOverlay>,
}

pub struct SceneRenderer {
    pipeline: wgpu::RenderPipeline,
    particle_pipeline: wgpu::RenderPipeline,
    object_layout: wgpu::BindGroupLayout,
    hud_pipeline: wgpu::RenderPipeline,
    hud_layout: wgpu::BindGroupLayout,
    format: wgpu::TextureFormat,

    // Everything below persists across frames; see the module doc.
    meshes: HashMap<usize, CachedMesh>,
    frame_uniform: Uniforms,
    /// Object uniforms for the whole draw list, one per `object_stride` bytes.
    objects: Option<Uniforms>,
    /// Distance between consecutive object uniforms: the struct size rounded
    /// up to the device's dynamic-offset alignment.
    object_stride: u64,
    particle_instances: Option<wgpu::Buffer>,
    particle_uniform: Uniforms,
    /// One cached texture per overlay canvas, reused across frames.
    hud_targets: Vec<HudTarget>,
    frame_index: u64,
}

impl SceneRenderer {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mesh-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/mesh.wgsl").into()),
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

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mesh-pipeline-layout"),
            bind_group_layouts: &[Some(&object_layout), Some(&frame_layout)],
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
            ],
        })];

        let particle_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("particle-pipeline"),
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
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
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
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

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

        Self {
            pipeline,
            particle_pipeline,
            object_layout,
            hud_pipeline,
            hud_layout,
            format,
            meshes: HashMap::new(),
            frame_uniform,
            objects: None,
            object_stride,
            particle_instances: None,
            particle_uniform,
            hud_targets: Vec::new(),
            frame_index: 0,
        }
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

    /// Upload a draw list and render it.
    ///
    /// Geometry uploads once and is reused: `items` carry shared
    /// `Arc<MeshData>`, and this keeps the GPU buffers for each one alive
    /// across frames (see the module doc). Per-frame work is one uniform
    /// write per pass plus the draw calls themselves.
    pub fn draw(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, pass: ScenePass<'_>) {
        let ScenePass {
            target,
            depth,
            items,
            particles,
            view_projection,
            camera_position,
            camera_right,
            camera_up,
            lights,
            clear,
            hud,
        } = pass;

        self.frame_index += 1;

        let frame = FrameUniform {
            camera_pos: camera_position.extend(1.0).to_array(),
            sun_direction: lights.sun_direction.extend(0.0).to_array(),
            sun_color: lights.sun_color.extend(1.0).to_array(),
            ambient: lights.ambient.extend(1.0).to_array(),
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

        // Translucent billboards sort back-to-front (blending is order
        // dependent) and upload as one instance buffer. Distance to the
        // camera stands in for view depth — correct enough for sprites, and
        // `total_cmp` plus a stable sort keeps the order deterministic.
        let particle_count = if particles.is_empty() {
            0
        } else {
            let mut sorted: Vec<&ParticleInstance> = particles.iter().collect();
            sorted.sort_by(|a, b| {
                let da = (a.position - camera_position).length_squared();
                let db = (b.position - camera_position).length_squared();
                db.total_cmp(&da)
            });
            let raw: Vec<ParticleRaw> = sorted
                .iter()
                .map(|p| ParticleRaw {
                    pos_size: p.position.extend(p.size).to_array(),
                    color: p.color.extend(p.alpha).to_array(),
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
            };
            queue.write_buffer(
                &self.particle_uniform.buffer,
                0,
                bytemuck::bytes_of(&uniform),
            );
            raw.len() as u32
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

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("scene-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
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
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(1, &self.frame_uniform.bind_group, &[]);

            if let Some(objects) = &self.objects {
                for (index, key) in keys.iter().enumerate() {
                    let mesh = &self.meshes[key];
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
            }

            // Particles last, over the opaque scene, inside the same pass so
            // they test against the depth the meshes just wrote.
            if particle_count > 0 {
                let instances = self.particle_instances.as_ref().expect("just written");
                pass.set_pipeline(&self.particle_pipeline);
                pass.set_bind_group(0, &self.particle_uniform.bind_group, &[]);
                pass.set_vertex_buffer(0, instances.slice(..));
                pass.draw(0..6, 0..particle_count);
            }
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
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("depth"),
            size: wgpu::Extent3d {
                width: width.max(1),
                height: height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
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
