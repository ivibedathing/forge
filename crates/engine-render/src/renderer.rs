//! The renderer proper.
//!
//! `Renderer::draw` takes a `TextureView` and does not know or care whether it
//! came from a swapchain or from an offscreen texture. That seam is the whole
//! reason `engine screenshot` (M1) is cheap to add: headless rendering is the
//! same code path with a different target, not a parallel implementation.

/// What to draw into and how.
pub struct Frame<'a> {
    pub view: &'a wgpu::TextureView,
    pub clear: wgpu::Color,
}

pub struct Renderer {
    pipeline: wgpu::RenderPipeline,
    /// Format this pipeline was built for; a target with a different format
    /// needs a different pipeline.
    format: wgpu::TextureFormat,
}

impl Renderer {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("triangle-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/triangle.wgsl").into()),
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("triangle-pipeline-layout"),
            bind_group_layouts: &[],
            // wgpu 30's replacement for push constants. Nothing to push yet.
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("triangle-pipeline"),
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
                cull_mode: Some(wgpu::Face::Back),
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        Self { pipeline, format }
    }

    pub fn format(&self) -> wgpu::TextureFormat {
        self.format
    }

    /// Record and submit one frame.
    pub fn draw(&self, device: &wgpu::Device, queue: &wgpu::Queue, frame: Frame<'_>) {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("frame-encoder"),
        });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("main-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: frame.view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(frame.clear),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            pass.set_pipeline(&self.pipeline);
            pass.draw(0..3, 0..1);
        }

        queue.submit(Some(encoder.finish()));
    }
}
