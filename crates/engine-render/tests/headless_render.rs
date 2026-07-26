//! Renders the M0 triangle to an offscreen texture and inspects the pixels.
//!
//! "The window opened and did not crash" is not evidence that anything was
//! drawn — a culled triangle, a broken pipeline, or a shader that writes
//! nothing all look identical from outside. This test reads the framebuffer
//! back and asserts on actual colors.
//!
//! It also exercises the render-to-arbitrary-target seam that `engine
//! screenshot` (M1) is built on, so that path stays honest as the renderer
//! grows.

use engine_render::{Frame, Gpu, Renderer};

const SIZE: u32 = 256;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// Clear color, chosen to be unmistakably distinct from the triangle.
const CLEAR: wgpu::Color = wgpu::Color {
    r: 0.0,
    g: 0.0,
    b: 0.0,
    a: 1.0,
};

struct Rendered {
    pixels: Vec<u8>,
}

impl Rendered {
    /// RGBA at (x, y), origin top-left.
    fn at(&self, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * SIZE + x) * 4) as usize;
        [
            self.pixels[i],
            self.pixels[i + 1],
            self.pixels[i + 2],
            self.pixels[i + 3],
        ]
    }
}

/// Render one frame offscreen and read it back, or `None` if this machine has
/// no usable GPU (CI runners frequently do not).
fn render_offscreen() -> Option<Rendered> {
    let instance = Gpu::default_instance();
    let gpu = match pollster::block_on(Gpu::new(instance, None)) {
        Ok(gpu) => gpu,
        Err(e) => {
            eprintln!("skipping: no usable GPU on this machine ({e})");
            return None;
        }
    };

    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("headless-target"),
        size: wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
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

    let renderer = Renderer::new(&gpu.device, FORMAT);
    renderer.draw(
        &gpu.device,
        &gpu.queue,
        Frame {
            view: &view,
            clear: CLEAR,
        },
    );

    // Rows in a texture-to-buffer copy must start on a 256-byte boundary. At
    // 256px RGBA that is already 1024 bytes, so no padding is needed here —
    // but M1's screenshot takes arbitrary sizes and will have to handle it.
    let bytes_per_row = SIZE * 4;
    assert_eq!(bytes_per_row % wgpu::COPY_BYTES_PER_ROW_ALIGNMENT, 0);

    let readback = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("headless-readback"),
        size: u64::from(bytes_per_row * SIZE),
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
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(SIZE),
            },
        },
        wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit(Some(encoder.finish()));

    let slice = readback.slice(..);
    slice.map_async(wgpu::MapMode::Read, |r| {
        r.expect("mapping the readback buffer failed");
    });
    gpu.device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("polling the device failed");

    let pixels = slice
        .get_mapped_range()
        .expect("readback buffer was not mapped")
        .to_vec();
    readback.unmap();

    Some(Rendered { pixels })
}

#[test]
fn draws_a_triangle_not_an_empty_frame() {
    let Some(frame) = render_offscreen() else {
        return;
    };

    // Dead center is inside the triangle. The shader writes (0.9, 0.2, 0.2).
    let [r, g, b, a] = frame.at(SIZE / 2, SIZE / 2);
    assert!(
        r > 200 && g < 80 && b < 80 && a == 255,
        "center pixel should be the triangle's red, got {:?}",
        [r, g, b, a]
    );
}

#[test]
fn leaves_the_background_clear() {
    let Some(frame) = render_offscreen() else {
        return;
    };

    // The triangle spans x -0.8..0.8, y -0.6..0.8 in clip space, so all four
    // corners fall outside it and must still show the clear color.
    for (x, y) in [(1, 1), (SIZE - 2, 1), (1, SIZE - 2), (SIZE - 2, SIZE - 2)] {
        let [r, g, b, _] = frame.at(x, y);
        assert!(
            r < 20 && g < 20 && b < 20,
            "corner ({x}, {y}) should be the clear color, got {:?}",
            [r, g, b]
        );
    }
}

#[test]
fn survives_the_front_face_winding_convention() {
    let Some(frame) = render_offscreen() else {
        return;
    };

    // Backface culling is on. If the triangle's winding were wrong it would be
    // discarded entirely and every pixel would be the clear color — which the
    // center-pixel test would catch, but this states the intent directly so a
    // future winding change fails with an obvious name.
    let lit = (0..SIZE)
        .step_by(8)
        .flat_map(|y| (0..SIZE).step_by(8).map(move |x| (x, y)))
        .filter(|&(x, y)| frame.at(x, y)[0] > 200)
        .count();

    assert!(
        lit > 100,
        "expected a substantial lit area; only {lit} sampled pixels were red \
         (a wrongly-wound triangle gets culled and lights nothing)"
    );
}
