//! Headless rendering: render a scene to an image without a window.
//!
//! This is what `engine screenshot` is built on, and per the design doc it is
//! the single most important path in the project — it closes the agent's
//! edit → see loop.

use engine_core::components::Camera;
use engine_core::math::Mat4;
use engine_core::particles::ParticleInstance;
use engine_core::scene::{EnvironmentSettings, HudItems, RenderItem, ResolvedLights, WaterItem};
use engine_core::{EngineError, Result};

use crate::gpu::Gpu;
use crate::hud;
use crate::scene_renderer::{self, SceneRenderer};

/// An RGBA8 image in CPU memory, tightly packed (no row padding).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl Image {
    /// RGBA at (x, y), origin top-left.
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * self.width + x) * 4) as usize;
        [
            self.pixels[i],
            self.pixels[i + 1],
            self.pixels[i + 2],
            self.pixels[i + 3],
        ]
    }
}

/// Render a draw list from a camera into an image.
///
/// The target is `Rgba8UnormSrgb` (an M4 decision that reversed M2's linear
/// choice): lighting math runs in linear space, the hardware performs the
/// sRGB encode, and readback therefore yields sRGB-encoded bytes — which is
/// what a PNG is conventionally assumed to contain. Scene colors stay linear
/// in the file; the PNG pixel is the lit, encoded result.
///
/// `hud` holds the scene's HUD components and `lines` the script debug lines
/// from the last step; both composite over the finished frame through one
/// rasterized overlay, so scenes with nothing to say pay nothing and render
/// byte-identically to the pre-HUD engine.
#[allow(clippy::too_many_arguments)]
pub fn render(
    items: &[RenderItem],
    water: &[WaterItem],
    particles: &[ParticleInstance],
    camera: &Camera,
    camera_model: Mat4,
    lights: ResolvedLights,
    environment: EnvironmentSettings,
    time: f32,
    width: u32,
    height: u32,
    hud: &HudItems,
    lines: &[String],
) -> Result<Image> {
    render_with_adapter(
        items,
        water,
        particles,
        camera,
        camera_model,
        lights,
        environment,
        time,
        width,
        height,
        hud,
        lines,
    )
    .map(|(image, _)| image)
}

/// [`render`], also reporting which adapter drew the image.
///
/// `engine diff-render` carries the adapter name in its report because
/// cross-adapter baseline failures are the expected hard case — the report
/// should include the one fact that diagnoses them.
#[allow(clippy::too_many_arguments)]
pub fn render_with_adapter(
    items: &[RenderItem],
    water: &[WaterItem],
    particles: &[ParticleInstance],
    camera: &Camera,
    camera_model: Mat4,
    lights: ResolvedLights,
    environment: EnvironmentSettings,
    // Scene time in seconds, from the same reproducible clock the rest of the
    // frame came from — read only by water (M18).
    time: f32,
    width: u32,
    height: u32,
    hud: &HudItems,
    lines: &[String],
) -> Result<(Image, String)> {
    let (width, height) = (width.max(1), height.max(1));
    const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

    let instance = Gpu::default_instance();
    let gpu = pollster::block_on(Gpu::new(instance, None))?;
    let adapter = gpu.adapter_info().name;

    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("screenshot-target"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let samples = environment.samples.max(1);
    let depth = scene_renderer::depth_texture_multisampled(&gpu.device, width, height, samples);
    // At one sample the scene draws straight into the readback texture, which
    // is what every pre-MSAA baseline was blessed through.
    let msaa = (samples > 1).then(|| {
        scene_renderer::msaa_color_texture(&gpu.device, FORMAT, width, height, samples)
    });

    let mut renderer = SceneRenderer::with_samples(&gpu.device, FORMAT, samples);
    let view_projection =
        scene_renderer::view_projection(camera, camera_model, width as f32 / height as f32);

    let no_lines = lines.iter().all(|l| l.is_empty());
    let canvas =
        (!(hud.is_empty() && no_lines)).then(|| hud::rasterize(hud, lines, width, height));

    renderer.draw(
        &gpu.device,
        &gpu.queue,
        scene_renderer::ScenePass {
            target: &view,
            msaa: msaa.as_ref(),
            depth: &depth,
            target_size: [width, height],
            items,
            water,
            particles,
            view_projection,
            camera_position: camera_model.w_axis.truncate(),
            camera_right: camera_model.x_axis.truncate(),
            camera_up: camera_model.y_axis.truncate(),
            lights,
            environment,
            time,
            clear: scene_renderer::DEFAULT_CLEAR,
            hud: canvas.as_ref(),
        },
    );

    read_back(&gpu, &texture, width, height).map(|image| (image, adapter))
}

/// Copy a rendered texture into CPU memory, undoing wgpu's row alignment.
///
/// Copy destinations must have rows starting on a
/// `COPY_BYTES_PER_ROW_ALIGNMENT` (256-byte) boundary. Any width that is not a
/// multiple of 64 pixels therefore comes back padded, and the padding has to be
/// stripped — otherwise the image skews progressively, which looks like a
/// camera bug rather than a buffer bug.
fn read_back(gpu: &Gpu, texture: &wgpu::Texture, width: u32, height: u32) -> Result<Image> {
    let unpadded_bytes_per_row = width * 4;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded_bytes_per_row = unpadded_bytes_per_row.div_ceil(align) * align;

    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("screenshot-readback"),
        size: u64::from(padded_bytes_per_row) * u64::from(height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("readback-encoder"),
        });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_bytes_per_row),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit(Some(encoder.finish()));

    let slice = buffer.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    gpu.device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|e| {
            EngineError::new(engine_core::codes::GPU_POLL_FAILED, format!("waiting on the GPU failed: {e}"))
        })?;

    let mapped = slice.get_mapped_range().map_err(|e| {
        EngineError::new(
            engine_core::codes::READBACK_FAILED,
            format!("could not map the readback buffer: {e}"),
        )
    })?;

    let mut pixels = Vec::with_capacity((unpadded_bytes_per_row * height) as usize);
    for row in mapped.chunks_exact(padded_bytes_per_row as usize) {
        pixels.extend_from_slice(&row[..unpadded_bytes_per_row as usize]);
    }

    drop(mapped);
    buffer.unmap();

    Ok(Image {
        width,
        height,
        pixels,
    })
}
