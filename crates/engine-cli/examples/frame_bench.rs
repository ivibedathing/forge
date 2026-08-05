//! What one *viewer* frame of a scene costs, phase by phase.
//!
//! An example rather than a subcommand, because it answers a question about
//! *this machine* and nothing reproducible may depend on it — the same reason
//! the viewer's FPS readout is viewer-only (M15). It mirrors `app.rs::redraw`'s
//! render half: the per-frame item rebuilds, the GI fold, the HUD rasterize and
//! `draw`, against a persistent `SceneRenderer` and offscreen attachments.
//!
//! Two things it does deliberately, and one caveat:
//!
//! - **It warms up first.** The geometry caches, the texture uploads and the
//!   driver's pipeline compilation are load costs, not frame costs; the first
//!   frame of the tour is ~1 s of Metal shader compilation and averaging it in
//!   hides everything else.
//! - **It caches the GI bake like the viewer does**, folding it per frame but
//!   reading the file once. Calling `field_for_scene` per frame instead re-reads
//!   an NDJSON of probes sixty times a second and reports the parser as the
//!   frame cost.
//! - **It drains the GPU every frame**, so `gpu wait` is real GPU time and the
//!   total is CPU + GPU *serialized*. A window pipelines the two, so the viewer's
//!   frame rate tracks `max(cpu, gpu)` and this total is a lower bound on it.
//!
//! `cargo run --release -p engine-cli --example frame_bench -- <scene> [frames] [width] [height]`
//!
//! A 1280×720 window on a Retina display is a 2560×1440 surface, which is the
//! default here and the size the numbers in `designs/notes/m15-frame-cost.md`
//! were taken at.

use std::path::PathBuf;
use std::time::Instant;

fn main() {
    let mut args = std::env::args().skip(1);
    let scene_path = PathBuf::from(
        args.next()
            .expect("usage: frame_bench <scene> [frames] [w] [h]"),
    );
    let frames: u32 = args.next().map_or(120, |a| a.parse().unwrap());
    let width: u32 = args.next().map_or(2560, |a| a.parse().unwrap());
    let height: u32 = args.next().map_or(1440, |a| a.parse().unwrap());

    let text = std::fs::read_to_string(&scene_path).expect("read scene");
    let scene = engine_core::scene::Scene::from_source(&text, &scene_path.display().to_string())
        .expect("parse scene");
    let assets = engine_assets::AssetServer::for_scene(&scene_path);
    let base = scene_path.parent().unwrap_or(std::path::Path::new(""));

    let (camera, camera_transform) = scene.camera(None).expect("camera");
    let camera_model = camera_transform.matrix();

    const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
    let instance = engine_render::gpu::Gpu::default_instance();
    let gpu = pollster::block_on(engine_render::gpu::Gpu::new(instance, None)).expect("gpu");

    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("bench-target"),
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

    let (lights0, env0) = scene.resolved_at(0.0);
    let samples = env0.samples.max(1);
    let depth = engine_render::scene_renderer::depth_texture_multisampled(
        &gpu.device,
        width,
        height,
        samples,
    );
    let msaa = (samples > 1).then(|| {
        engine_render::scene_renderer::msaa_color_texture(
            &gpu.device,
            FORMAT,
            width,
            height,
            samples,
        )
    });
    // Cached exactly as the viewer caches it: the file is read once and only
    // the fold runs per frame.
    let baked = engine_core::gi::evaluate::load_for_scene(&scene, base);
    let gi0 = baked
        .as_ref()
        .map(|(volume, baked)| engine_core::gi::evaluate(baked, volume, &lights0, &env0));
    let mut renderer = engine_render::scene_renderer::SceneRenderer::configured(
        &gpu.device,
        FORMAT,
        samples,
        env0.shadow_cascades,
        gi0.is_some(),
    );

    let mut t = [0u128; 8];
    const PHASES: [&str; 8] = [
        "render_items_at",
        "water/cloud/road/meadow",
        "hud_tree",
        "gi::evaluate",
        "hud::rasterize",
        "draw(record+submit)",
        "gpu wait",
        "TOTAL",
    ];

    // Twenty warm-up frames first: the geometry caches, the texture uploads and
    // the driver's pipeline compilation are load costs, not frame costs, and
    // averaging them in hides the steady state this is measuring.
    const WARMUP: u32 = 20;
    for frame in 0..frames + WARMUP {
        if frame == WARMUP {
            t = [0u128; 8];
        }
        let time = frame as f32 / 60.0;
        let whole = Instant::now();

        let mark = Instant::now();
        let items = scene.render_items_at(&assets, Some(time)).expect("items");
        t[0] += mark.elapsed().as_nanos();

        let mark = Instant::now();
        let water = scene.water_items();
        let clouds = scene.cloud_items();
        let roads = scene.road_items();
        let meadows = scene.meadow_items();
        t[1] += mark.elapsed().as_nanos();

        let mark = Instant::now();
        let hud = scene.hud_tree(&assets);
        t[2] += mark.elapsed().as_nanos();

        let (lights, environment) = scene.resolved_at(time);

        let mark = Instant::now();
        let gi = baked
            .as_ref()
            .map(|(volume, baked)| engine_core::gi::evaluate(baked, volume, &lights, &environment));
        t[3] += mark.elapsed().as_nanos();

        let mark = Instant::now();
        let canvas =
            (!hud.is_empty()).then(|| engine_render::hud::rasterize(&hud, &[], width, height));
        if frame == WARMUP {
            if let Some(overlay) = &canvas {
                for c in &overlay.canvases {
                    eprintln!(
                        "hud canvas {}x{} at ({},{}) = {} px",
                        c.width,
                        c.height,
                        c.origin_x,
                        c.origin_y,
                        c.width * c.height
                    );
                }
            }
        }
        t[4] += mark.elapsed().as_nanos();

        let view_projection = engine_render::scene_renderer::view_projection(
            &camera,
            camera_model,
            width as f32 / height as f32,
        );

        let mark = Instant::now();
        renderer.draw(
            &gpu.device,
            &gpu.queue,
            engine_render::scene_renderer::ScenePass {
                target: &view,
                msaa: msaa.as_ref(),
                depth: &depth,
                target_size: [width, height],
                items: &items,
                water: &water,
                clouds: &clouds,
                roads: &roads,
                meadows: &meadows,
                particles: &[],
                view_projection,
                camera_position: camera_model.w_axis.truncate(),
                camera_right: camera_model.x_axis.truncate(),
                camera_up: camera_model.y_axis.truncate(),
                lights,
                environment,
                time,
                clear: engine_render::scene_renderer::DEFAULT_CLEAR,
                hud: canvas.as_ref(),
                gi: gi.as_ref(),
            },
        );
        t[5] += mark.elapsed().as_nanos();

        let mark = Instant::now();
        let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
        t[6] += mark.elapsed().as_nanos();

        t[7] += whole.elapsed().as_nanos();
    }

    println!(
        "{} frames at {width}x{height}, samples={samples} (after {WARMUP} warm-up frames)",
        frames
    );
    for (name, total) in PHASES.iter().zip(t) {
        let ms = total as f64 / 1.0e6 / frames as f64;
        println!("{ms:9.3} ms  {name}");
    }
}
