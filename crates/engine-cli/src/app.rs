//! The winit application handler for windowed viewing.
//!
//! One app, two paint modes: the M0 triangle (`engine run`) and a loaded scene
//! (`engine run-scene`). The split lives here rather than in two app structs
//! because everything except the per-frame draw — window creation, resize,
//! error plumbing — is identical.

use std::sync::Arc;
use std::time::Instant;

use engine_core::components::Camera;
use engine_core::math::Mat4;
use engine_core::scene::RenderItem;
use engine_core::{EngineError, Result};
use engine_render::scene_renderer::{self, ScenePass, SceneRenderer};
use engine_render::{Frame, Renderer, WindowTarget};
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::ActiveEventLoop,
    window::{Window, WindowId},
};

/// What to render, decided before the window exists.
pub enum Content {
    /// The M0 proof-of-life triangle.
    Triangle,
    /// A loaded scene, flattened to a draw list.
    Scene {
        items: Vec<RenderItem>,
        camera: Camera,
        camera_model: Mat4,
        lights: engine_core::scene::ResolvedLights,
        /// Present when the scene has physics components: the viewer drives
        /// the same fixed step through a wall-clock accumulator (the
        /// headless path stays canonical; frame pacing may vary here).
        simulation: Option<Simulation>,
    },
}

/// Live physics for the windowed viewer.
pub struct Simulation {
    pub scene: engine_core::Scene,
    pub physics: engine_physics::PhysicsWorld,
    pub assets: engine_assets::AssetServer,
    pub accumulator: f32,
    pub last: Option<Instant>,
}

/// GPU-side state, created once the window exists.
enum Paint {
    Triangle(Renderer),
    Scene {
        renderer: SceneRenderer,
        depth: wgpu::TextureView,
    },
}

pub struct ViewerApp {
    title: String,
    width: u32,
    height: u32,
    content: Content,

    // Populated on `resumed`, the only point at which winit guarantees a
    // window can be created. On desktop this fires once at startup.
    window: Option<Arc<Window>>,
    target: Option<WindowTarget>,
    paint: Option<Paint>,

    /// Set when a frame fails unrecoverably; drained by `into_result` so the
    /// process can exit non-zero with structured JSON.
    error: Option<EngineError>,
}

impl ViewerApp {
    pub fn new(title: impl Into<String>, width: u32, height: u32, content: Content) -> Self {
        Self {
            title: title.into(),
            width,
            height,
            content,
            window: None,
            target: None,
            paint: None,
            error: None,
        }
    }

    /// The error that ended the loop, if any.
    pub fn into_result(self) -> Result<()> {
        match self.error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Record a fatal error and ask the loop to unwind.
    fn fail(&mut self, event_loop: &ActiveEventLoop, error: EngineError) {
        self.error = Some(error);
        event_loop.exit();
    }

    fn redraw(&mut self) -> Result<()> {
        let (Some(target), Some(paint)) = (self.target.as_mut(), self.paint.as_mut()) else {
            return Ok(());
        };

        match (paint, &mut self.content) {
            (Paint::Triangle(renderer), _) => target.render_with(|device, queue, view| {
                renderer.draw(
                    device,
                    queue,
                    Frame {
                        view,
                        clear: scene_renderer::DEFAULT_CLEAR,
                    },
                );
            }),

            (
                Paint::Scene { renderer, depth },
                Content::Scene {
                    items,
                    camera,
                    camera_model,
                    lights,
                    simulation,
                },
            ) => {
                if let Some(sim) = simulation {
                    let now = Instant::now();
                    let dt = 1.0 / sim.scene.physics.timestep_hz.max(1) as f32;
                    let elapsed = sim
                        .last
                        .map(|last| (now - last).as_secs_f32())
                        .unwrap_or(dt)
                        .min(0.25);
                    sim.last = Some(now);
                    sim.accumulator += elapsed;
                    while sim.accumulator >= dt {
                        sim.physics.step(&mut sim.scene.world);
                        sim.accumulator -= dt;
                    }
                    if let Ok(fresh) = sim.scene.render_items(&sim.assets) {
                        *items = fresh;
                    }
                }
                let (width, height) = target.size();
                let view_projection = scene_renderer::view_projection(
                    camera,
                    *camera_model,
                    width as f32 / height as f32,
                );
                target.render_with(|device, queue, view| {
                    renderer.draw(
                        device,
                        queue,
                        ScenePass {
                            target: view,
                            depth,
                            items,
                            view_projection,
                            camera_position: camera_model.w_axis.truncate(),
                            lights: *lights,
                            clear: scene_renderer::DEFAULT_CLEAR,
                        },
                    );
                })
            }

            // Scene paint is only ever built from scene content.
            (Paint::Scene { .. }, Content::Triangle) => unreachable!(),
        }
    }
}

impl ApplicationHandler for ViewerApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        let attrs = Window::default_attributes()
            .with_title(&self.title)
            .with_inner_size(winit::dpi::LogicalSize::new(self.width, self.height));

        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                return self.fail(
                    event_loop,
                    EngineError::new(
                        engine_core::codes::WINDOW_CREATION_FAILED,
                        format!("could not create a window: {e}"),
                    ),
                );
            }
        };

        let size = window.inner_size();
        let target = match WindowTarget::new(window.clone(), size.width, size.height) {
            Ok(target) => target,
            Err(e) => return self.fail(event_loop, e),
        };

        let paint = match self.content {
            Content::Triangle => {
                Paint::Triangle(Renderer::new(&target.gpu.device, target.format()))
            }
            Content::Scene { .. } => {
                let (width, height) = target.size();
                Paint::Scene {
                    renderer: SceneRenderer::new(&target.gpu.device, target.format()),
                    depth: scene_renderer::depth_texture(&target.gpu.device, width, height),
                }
            }
        };

        self.window = Some(window);
        self.target = Some(target);
        self.paint = Some(paint);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),

            WindowEvent::Resized(size) => {
                if let Some(target) = self.target.as_mut() {
                    target.resize(size.width, size.height);
                    // The depth buffer must track the swapchain size exactly.
                    if let Some(Paint::Scene { depth, .. }) = self.paint.as_mut() {
                        *depth = scene_renderer::depth_texture(
                            &target.gpu.device,
                            size.width,
                            size.height,
                        );
                    }
                }
            }

            WindowEvent::RedrawRequested => {
                if let Err(e) = self.redraw() {
                    return self.fail(event_loop, e);
                }
                // Keep drawing; there is no simulation to pace yet.
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
            }

            _ => {}
        }
    }
}
