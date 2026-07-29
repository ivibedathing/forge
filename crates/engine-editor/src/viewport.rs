//! The 3D viewport: the engine's own `SceneRenderer` drawing into an
//! offscreen texture that egui displays as an image.
//!
//! Rendering offscreen (rather than inside egui's render pass) keeps the
//! scene pass identical to `engine screenshot`'s — same pipeline, same depth
//! buffer, same clear — so what the editor shows is what the engine renders,
//! not an editor approximation (principle #7).

use engine_core::scene::{RenderItem, ResolvedLights};
use engine_render::scene_renderer::{self, ScenePass, SceneRenderer};
use glam::{Mat4, Vec3};

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

struct Target {
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
    depth: wgpu::TextureView,
    id: egui::TextureId,
    width: u32,
    height: u32,
}

pub struct ViewportRenderer {
    renderer: SceneRenderer,
    target: Option<Target>,
}

impl ViewportRenderer {
    pub fn new(render_state: &egui_wgpu::RenderState) -> Self {
        Self {
            renderer: SceneRenderer::new(&render_state.device, FORMAT),
            target: None,
        }
    }

    /// Render the draw list and return the egui texture to show.
    #[allow(clippy::too_many_arguments)]
    pub fn paint(
        &mut self,
        render_state: &egui_wgpu::RenderState,
        width: u32,
        height: u32,
        items: &[RenderItem],
        view_projection: Mat4,
        camera_position: Vec3,
        lights: ResolvedLights,
        environment: engine_core::scene::EnvironmentSettings,
    ) -> egui::TextureId {
        let (width, height) = (width.max(1), height.max(1));

        if self
            .target
            .as_ref()
            .is_none_or(|t| t.width != width || t.height != height)
        {
            if let Some(old) = self.target.take() {
                render_state.renderer.write().free_texture(&old.id);
            }

            let texture = render_state.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("editor-viewport"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let depth = scene_renderer::depth_texture(&render_state.device, width, height);
            let id = render_state.renderer.write().register_native_texture(
                &render_state.device,
                &view,
                wgpu::FilterMode::Linear,
            );

            self.target = Some(Target {
                _texture: texture,
                view,
                depth,
                id,
                width,
                height,
            });
        }

        let target = self.target.as_ref().expect("just ensured");
        self.renderer.draw(
            &render_state.device,
            &render_state.queue,
            ScenePass {
                target: &target.view,
                msaa: None,
                depth: &target.depth,
                items,
                // The editor shows the scene at rest; particles only exist
                // once the fixed clock advances, so there are none to draw.
                particles: &[],
                view_projection,
                camera_position,
                camera_right: Vec3::X,
                camera_up: Vec3::Y,
                lights,
                environment,
                clear: scene_renderer::DEFAULT_CLEAR,
                // The orbit-camera viewport is not the game camera's frame;
                // screen-anchored HUD elements would be misleading here, so
                // the editor leaves the overlay off. `engine screenshot` is
                // where the HUD is verified.
                hud: None,
            },
        );
        target.id
    }
}

impl ViewportRenderer {
    /// Read the current viewport texture back to CPU RGBA8 — the agent
    /// verification path (`--self-screenshot`). Mirrors `offscreen.rs`'s
    /// readback, including the row-alignment unpadding.
    pub fn read_back(
        &self,
        render_state: &egui_wgpu::RenderState,
    ) -> Option<(u32, u32, Vec<u8>)> {
        let target = self.target.as_ref()?;
        let (width, height) = (target.width, target.height);

        let unpadded = width * 4;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded = unpadded.div_ceil(align) * align;

        let buffer = render_state.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("viewport-readback"),
            size: u64::from(padded) * u64::from(height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder = render_state
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("viewport-readback-encoder"),
            });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &target._texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        render_state.queue.submit(Some(encoder.finish()));

        let slice = buffer.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        render_state
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .ok()?;

        let mapped = slice.get_mapped_range().ok()?;
        let mut pixels = Vec::with_capacity((unpadded * height) as usize);
        for row in mapped.chunks_exact(padded as usize) {
            pixels.extend_from_slice(&row[..unpadded as usize]);
        }
        drop(mapped);
        buffer.unmap();

        Some((width, height, pixels))
    }
}

/// The editor grid: thin cuboids on the ground plane, editor-side overlay
/// data that never touches the scene file. X and Z axes get their gizmo
/// colors, every fifth line is a brighter major line, and the rest are
/// quiet gray.
pub fn grid_items() -> Vec<RenderItem> {
    use engine_core::components::Material;
    use engine_core::mesh::BuiltinMesh;

    let mut items = Vec::new();
    let half = 20i32;
    let length = (half * 2) as f32;
    // Every grid line is the same cube: share one allocation so the renderer
    // uploads the geometry once rather than once per line.
    let cube = std::sync::Arc::new(BuiltinMesh::Cube.data());
    let mut line = |scale: Vec3, position: Vec3, albedo: Vec3| {
        items.push(RenderItem {
            entity: String::new(),
            mesh: std::sync::Arc::clone(&cube),
            model: Mat4::from_scale_rotation_translation(
                scale,
                glam::Quat::IDENTITY,
                position,
            ),
            material: Material {
                albedo,
                metallic: 0.0,
                roughness: 1.0,
                emissive: albedo * 0.15,
                ..Material::default()
            },
        });
    };

    let quiet = Vec3::splat(0.32);
    let major = Vec3::splat(0.52);
    for i in -half..=half {
        let offset = i as f32;
        let (thickness, color_x, color_z) = if i == 0 {
            (0.02, Vec3::new(0.8, 0.25, 0.25), Vec3::new(0.25, 0.4, 0.85))
        } else if i % 5 == 0 {
            (0.012, major, major)
        } else {
            (0.006, quiet, quiet)
        };
        // Line along X at z = offset.
        line(
            Vec3::new(length, thickness, thickness),
            Vec3::new(0.0, 0.0, offset),
            color_x,
        );
        // Line along Z at x = offset.
        line(
            Vec3::new(thickness, thickness, length),
            Vec3::new(offset, 0.0, 0.0),
            color_z,
        );
    }
    items
}
