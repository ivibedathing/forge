//! The HUD overlay: script-authored text lines drawn over a rendered frame.
//!
//! Scripts push plain ASCII lines through `world.hud(...)` each step; this
//! module turns the last step's lines into pixels. Rasterization is pure CPU
//! (an embedded public-domain 8x8 bitmap font, integer-scaled — unit-testable
//! without a GPU); compositing is one alpha-blended textured quad, drawn by
//! both the headless screenshot path and the windowed viewer so what the
//! agent pins in a baseline is what the player sees.

use wgpu::util::DeviceExt as _;

use crate::offscreen::Image;

/// Font glyphs are 8x8; drawn at this integer scale.
const GLYPH: u32 = 8;
const SCALE: u32 = 2;
/// Padding between the text block and the panel edge, in pixels.
const PAD: u32 = 8;
/// Vertical gap between lines, in pixels.
const LINE_GAP: u32 = 6;
/// Panel offset from the top-left corner of the frame.
const MARGIN: u32 = 10;

/// Translucent dark panel behind the text, sRGB-encoded straight alpha.
const PANEL: [u8; 4] = [10, 12, 16, 200];
const TEXT: [u8; 4] = [255, 255, 255, 255];

/// Rasterize HUD lines onto their backing panel. `None` when there is
/// nothing to draw. Pure CPU and deterministic by construction.
pub fn rasterize(lines: &[String]) -> Option<Image> {
    let longest = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    if lines.is_empty() || longest == 0 {
        return None;
    }

    let cell = GLYPH * SCALE;
    let width = longest as u32 * cell + 2 * PAD;
    let height = lines.len() as u32 * cell + (lines.len() as u32 - 1) * LINE_GAP + 2 * PAD;

    let mut pixels = Vec::with_capacity((width * height * 4) as usize);
    for _ in 0..width * height {
        pixels.extend_from_slice(&PANEL);
    }
    let mut image = Image {
        width,
        height,
        pixels,
    };

    for (row, line) in lines.iter().enumerate() {
        let top = PAD + row as u32 * (cell + LINE_GAP);
        for (col, ch) in line.chars().enumerate() {
            // The script API enforces printable ASCII; anything else that
            // reaches us renders as '?' rather than a hole.
            let index = if (0x20..0x7f).contains(&(ch as u32)) {
                ch as usize
            } else {
                b'?' as usize
            };
            let glyph = font8x8::legacy::BASIC_LEGACY[index];
            let left = PAD + col as u32 * cell;
            for (y, bits) in glyph.iter().enumerate() {
                for x in 0..8u32 {
                    // font8x8 packs rows LSB-leftmost.
                    if bits & (1 << x) == 0 {
                        continue;
                    }
                    for sy in 0..SCALE {
                        for sx in 0..SCALE {
                            let px = left + x * SCALE + sx;
                            let py = top + y as u32 * SCALE + sy;
                            let i = ((py * width + px) * 4) as usize;
                            image.pixels[i..i + 4].copy_from_slice(&TEXT);
                        }
                    }
                }
            }
        }
    }

    Some(image)
}

/// Shader-side quad placement, NDC top-left plus size.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct HudRect {
    rect: [f32; 4],
}

/// Draws the rasterized panel into any render target, over whatever the
/// scene pass left there.
pub struct HudRenderer {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
}

impl HudRenderer {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("hud-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/hud.wgsl").into()),
        });

        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("hud-bind-layout"),
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
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::VERTEX,
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
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("hud-sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        Self {
            pipeline,
            layout,
            sampler,
        }
    }

    /// Composite `lines` over `target`. A no-op when there is nothing to say,
    /// so HUD-free scenes render byte-identically to an engine without HUDs.
    pub fn draw(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target: &wgpu::TextureView,
        target_width: u32,
        target_height: u32,
        lines: &[String],
    ) {
        let Some(panel) = rasterize(lines) else {
            return;
        };

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("hud-panel"),
            size: wgpu::Extent3d {
                width: panel.width,
                height: panel.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            // sRGB so the authored panel bytes survive the sample → encode
            // round trip over opaque pixels.
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
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
            &panel.pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(panel.width * 4),
                rows_per_image: Some(panel.height),
            },
            wgpu::Extent3d {
                width: panel.width,
                height: panel.height,
                depth_or_array_layers: 1,
            },
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        // Pixel rect → NDC top-left rect (NDC y points up, so height is
        // subtracted in the shader).
        let (tw, th) = (target_width.max(1) as f32, target_height.max(1) as f32);
        let rect = HudRect {
            rect: [
                MARGIN as f32 / tw * 2.0 - 1.0,
                1.0 - MARGIN as f32 / th * 2.0,
                panel.width as f32 / tw * 2.0,
                panel.height as f32 / th * 2.0,
            ],
        };
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("hud-rect"),
            contents: bytemuck::bytes_of(&rect),
            usage: wgpu::BufferUsages::UNIFORM,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("hud-bind-group"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: uniform.as_entire_binding(),
                },
            ],
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("hud-encoder"),
        });
        {
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
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..6, 0..1);
        }
        queue.submit(Some(encoder.finish()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_draws_nothing() {
        assert!(rasterize(&[]).is_none());
        assert!(rasterize(&[String::new()]).is_none());
    }

    #[test]
    fn panel_size_follows_the_text_block() {
        let image = rasterize(&["ABCD".into(), "A".into()]).unwrap();
        // 4 chars * 16px + 2*8 padding = 80; 2 lines * 16 + 1 gap * 6 + 16 = 54.
        assert_eq!((image.width, image.height), (80, 54));
    }

    #[test]
    fn glyphs_land_as_white_pixels_on_the_panel() {
        let image = rasterize(&["A".into()]).unwrap();
        // Corners are bare panel.
        assert_eq!(image.pixel(0, 0), PANEL);
        assert_eq!(image.pixel(image.width - 1, image.height - 1), PANEL);
        // Somewhere inside the glyph cell there is text; 'A' is not blank.
        let cell = (PAD..PAD + GLYPH * SCALE)
            .flat_map(|y| (PAD..PAD + GLYPH * SCALE).map(move |x| (x, y)));
        assert!(cell.clone().any(|(x, y)| image.pixel(x, y) == TEXT));
        // And a space renders as bare panel everywhere.
        let space = rasterize(&[" ".into()]).unwrap();
        assert!((0..space.height)
            .flat_map(|y| (0..space.width).map(move |x| (x, y)))
            .all(|(x, y)| space.pixel(x, y) == PANEL));
    }

    #[test]
    fn rasterization_is_deterministic() {
        let a = rasterize(&["SPEED 42 KM/H".into()]).unwrap();
        let b = rasterize(&["SPEED 42 KM/H".into()]).unwrap();
        assert_eq!(a, b);
    }
}
