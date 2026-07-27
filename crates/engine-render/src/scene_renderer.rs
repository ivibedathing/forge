//! Renders a scene's draw list.
//!
//! Like [`crate::Renderer`], this draws into any `TextureView` and knows
//! nothing about windows — that is what lets `engine screenshot` reuse it
//! unchanged.

use engine_core::components::Camera;
use engine_core::math::{Mat4, Vec3};
use engine_core::scene::{RenderItem, ResolvedLights};
use wgpu::util::DeviceExt as _;

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

/// One uploaded draw item.
///
/// Positions and normals share `vertices`; `normals_offset` is where the
/// second slot starts.
struct GpuItem {
    vertices: wgpu::Buffer,
    normals_offset: u64,
    indices: wgpu::Buffer,
    index_count: u32,
    bind_group: wgpu::BindGroup,
}

/// Everything one scene render needs beyond the device and queue.
pub struct ScenePass<'a> {
    pub target: &'a wgpu::TextureView,
    pub depth: &'a wgpu::TextureView,
    pub items: &'a [RenderItem],
    pub view_projection: Mat4,
    /// World-space camera position, for the specular view vector.
    pub camera_position: Vec3,
    pub lights: ResolvedLights,
    pub clear: wgpu::Color,
}

pub struct SceneRenderer {
    pipeline: wgpu::RenderPipeline,
    object_layout: wgpu::BindGroupLayout,
    frame_layout: wgpu::BindGroupLayout,
    format: wgpu::TextureFormat,
}

impl SceneRenderer {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mesh-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/mesh.wgsl").into()),
        });

        let uniform_layout = |label: &str| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some(label),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            })
        };
        let object_layout = uniform_layout("object-uniforms");
        let frame_layout = uniform_layout("frame-uniforms");

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

        Self {
            pipeline,
            object_layout,
            frame_layout,
            format,
        }
    }

    pub fn format(&self) -> wgpu::TextureFormat {
        self.format
    }

    /// Upload a draw list and render it.
    ///
    /// Buffers are created per call rather than cached: a screenshot renders
    /// once and exits, and caching would be dead weight until there is a
    /// persistent viewer to benefit from it.
    pub fn draw(&self, device: &wgpu::Device, queue: &wgpu::Queue, pass: ScenePass<'_>) {
        let ScenePass {
            target,
            depth,
            items,
            view_projection,
            camera_position,
            lights,
            clear,
        } = pass;

        let frame = FrameUniform {
            camera_pos: camera_position.extend(1.0).to_array(),
            sun_direction: lights.sun_direction.extend(0.0).to_array(),
            sun_color: lights.sun_color.extend(1.0).to_array(),
            ambient: lights.ambient.extend(1.0).to_array(),
        };
        let frame_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("frame-uniform"),
            contents: bytemuck::bytes_of(&frame),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let frame_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("frame-bind-group"),
            layout: &self.frame_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: frame_buffer.as_entire_binding(),
            }],
        });

        let uploaded: Vec<GpuItem> = items
            .iter()
            .map(|item| self.upload(device, item, view_projection))
            .collect();

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
            pass.set_bind_group(1, &frame_bind_group, &[]);

            for item in &uploaded {
                pass.set_bind_group(0, &item.bind_group, &[]);
                pass.set_vertex_buffer(0, item.vertices.slice(..));
                pass.set_vertex_buffer(1, item.normals_slice());
                pass.set_index_buffer(item.indices.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..item.index_count, 0, 0..1);
            }
        }

        queue.submit(Some(encoder.finish()));
    }

    fn upload(&self, device: &wgpu::Device, item: &RenderItem, view_projection: Mat4) -> GpuItem {
        // Positions and normals share one buffer, positions first, so a single
        // allocation serves both vertex slots.
        let mut vertex_bytes =
            Vec::with_capacity((item.mesh.positions.len() + item.mesh.normals.len()) * 12);
        vertex_bytes.extend_from_slice(bytemuck::cast_slice(&item.mesh.positions));
        let normals_offset = vertex_bytes.len() as u64;
        vertex_bytes.extend_from_slice(bytemuck::cast_slice(&item.mesh.normals));

        let vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("mesh-vertices"),
            contents: &vertex_bytes,
            usage: wgpu::BufferUsages::VERTEX,
        });

        let indices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("mesh-indices"),
            contents: bytemuck::cast_slice(&item.mesh.indices),
            usage: wgpu::BufferUsages::INDEX,
        });

        let material = &item.material;
        let uniform = ObjectUniform {
            mvp: (view_projection * item.model).to_cols_array_2d(),
            model: item.model.to_cols_array_2d(),
            normal_matrix: item.model.inverse().transpose().to_cols_array_2d(),
            albedo_metallic: material.albedo.extend(material.metallic).to_array(),
            emissive_roughness: material.emissive.extend(material.roughness).to_array(),
        };

        let uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("object-uniform"),
            contents: bytemuck::bytes_of(&uniform),
            usage: wgpu::BufferUsages::UNIFORM,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("object-bind-group"),
            layout: &self.object_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });

        GpuItem {
            vertices,
            indices,
            index_count: item.mesh.indices.len() as u32,
            bind_group,
            normals_offset,
        }
    }
}

impl GpuItem {
    fn normals_slice(&self) -> wgpu::BufferSlice<'_> {
        self.vertices.slice(self.normals_offset..)
    }
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
