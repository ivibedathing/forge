//! The renderer's frame-to-frame resource reuse (M15).
//!
//! A viewer redraws the same scene sixty times a second, and the engine used
//! to rebuild every GPU buffer and bind group each of those times. These tests
//! pin the behavior that replaced it: geometry uploads once and is reused,
//! entities sharing a mesh share the upload, and geometry that stops being
//! drawn is eventually released.
//!
//! Like the other render tests, every test skips cleanly without a GPU.

use std::sync::Arc;

use engine_core::components::Material;
use engine_core::math::{Mat4, Vec3};
use engine_core::mesh::{BuiltinMesh, MeshData};
use engine_core::scene::{LightRig, RenderItem};
use engine_render::scene_renderer::{self, ScenePass, SceneRenderer};
use engine_render::Gpu;

const SIZE: u32 = 64;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

fn gpu() -> Option<Gpu> {
    match pollster::block_on(Gpu::new(Gpu::default_instance(), None)) {
        Ok(gpu) => Some(gpu),
        Err(_) => {
            eprintln!("skipping: no usable GPU on this machine");
            None
        }
    }
}

fn item(mesh: &Arc<MeshData>, x: f32) -> RenderItem {
    RenderItem {
        entity: format!("Cube{x}"),
        mesh: Arc::clone(mesh),
        model: Mat4::from_translation(Vec3::new(x, 0.0, -4.0)),
        material: Material::default(),
    }
}

/// Draw `items` `frames` times through one renderer, as a viewer would.
fn draw_frames(gpu: &Gpu, renderer: &mut SceneRenderer, items: &[RenderItem], frames: u32) {
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("reuse-target"),
        size: wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let depth = scene_renderer::depth_texture(&gpu.device, SIZE, SIZE);
    let lights = LightRig {
        sun: None,
        ambient: None,
    }
    .resolved();

    for _ in 0..frames {
        renderer.draw(
            &gpu.device,
            &gpu.queue,
            ScenePass {
                target: &view,
                msaa: None,
                depth: &depth,
                items,
                particles: &[],
                view_projection: Mat4::IDENTITY,
                camera_position: Vec3::ZERO,
                camera_right: Vec3::X,
                camera_up: Vec3::Y,
                lights,
                environment: Default::default(),
                clear: scene_renderer::DEFAULT_CLEAR,
                hud: None,
            },
        );
        gpu.device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("frame should complete");
    }
}

#[test]
fn geometry_uploads_once_however_many_frames_and_entities_share_it() {
    let Some(gpu) = gpu() else { return };
    let mut renderer = SceneRenderer::new(&gpu.device, FORMAT);

    // Twenty entities, one shared mesh — the shape a scene of repeated props
    // has, and the reason the cache is keyed on the geometry rather than the
    // entity.
    let cube = Arc::new(BuiltinMesh::Cube.data());
    let items: Vec<RenderItem> = (0..20).map(|i| item(&cube, i as f32)).collect();

    draw_frames(&gpu, &mut renderer, &items, 30);
    assert_eq!(
        renderer.cached_mesh_count(),
        1,
        "one mesh drawn thirty times by twenty entities is one upload"
    );

    // A second distinct mesh is a second upload, and only one.
    let sphere = Arc::new(BuiltinMesh::Sphere.data());
    let mixed: Vec<RenderItem> = items.iter().cloned().chain([item(&sphere, 5.0)]).collect();
    draw_frames(&gpu, &mut renderer, &mixed, 10);
    assert_eq!(renderer.cached_mesh_count(), 2);
}

#[test]
fn reloaded_geometry_replaces_the_cached_upload() {
    let Some(gpu) = gpu() else { return };
    let mut renderer = SceneRenderer::new(&gpu.device, FORMAT);

    // An asset reload hands out a new `Arc` — the cache must treat it as new
    // geometry rather than serving the stale upload, or an edited mesh would
    // never appear.
    let first = Arc::new(BuiltinMesh::Cube.data());
    draw_frames(&gpu, &mut renderer, &[item(&first, 0.0)], 2);
    let reloaded = Arc::new(BuiltinMesh::Cube.data());
    draw_frames(&gpu, &mut renderer, &[item(&reloaded, 0.0)], 2);
    assert_eq!(
        renderer.cached_mesh_count(),
        2,
        "the reloaded mesh uploads; the old entry lingers only until eviction"
    );

    // Keep drawing only the reloaded one: the stale entry is released.
    draw_frames(&gpu, &mut renderer, &[item(&reloaded, 0.0)], 300);
    assert_eq!(renderer.cached_mesh_count(), 1, "idle geometry is evicted");
}
