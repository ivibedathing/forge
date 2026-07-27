//! The winit application handler for windowed viewing.
//!
//! One app, two paint modes: the M0 triangle (`engine run`) and a loaded scene
//! (`engine run-scene`). The split lives here rather than in two app structs
//! because everything except the per-frame draw — window creation, resize,
//! error plumbing — is identical.

use std::io::Write;
use std::sync::Arc;
use std::time::Instant;

use engine_core::components::Camera;
use engine_core::input::InputState;
use engine_core::math::Mat4;
use engine_core::scene::RenderItem;
use engine_core::{codes, EngineError, Result};
use engine_render::scene_renderer::{self, ScenePass, SceneRenderer};
use engine_render::{Frame, Renderer, WindowTarget};
use winit::{
    application::ApplicationHandler,
    event::{ElementState, WindowEvent},
    event_loop::ActiveEventLoop,
    keyboard::PhysicalKey,
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

/// Live playback for the windowed viewer: animation sampling and/or
/// fixed-step physics over one owned scene.
pub struct Simulation {
    pub scene: engine_core::Scene,
    pub physics: Option<engine_physics::PhysicsWorld>,
    pub players: Vec<engine_core::animation::LoadedPlayer>,
    pub scripts: Option<engine_script::ScriptHost>,
    pub assets: engine_assets::AssetServer,
    /// `--camera` from the invocation, so the per-frame camera re-resolve
    /// follows the same entity the session started on.
    pub camera_name: Option<String>,
    /// The keys held right now, fed by window events and sampled at each
    /// fixed step — scripts never see a between-steps edge.
    pub held: InputState,
    pub recorder: Option<InputRecorder>,
    pub accumulator: f32,
    pub t: f32,
    pub step_index: u64,
    pub last: Option<Instant>,
    /// What the most recent script step put on screen; empty between HUD
    /// pushes and for scripts that never call `world.hud`.
    pub hud_lines: Vec<String>,
}

/// `--record-input`: writes one timeline line whenever the held set changes,
/// flushed per line so an aborted session still leaves a valid replay file.
pub struct InputRecorder {
    file: std::fs::File,
    last: Option<InputState>,
}

impl InputRecorder {
    pub fn create(path: &std::path::Path) -> Result<Self> {
        let file = std::fs::File::create(path).map_err(|e| {
            EngineError::new(
                codes::SCENE_WRITE_FAILED,
                format!("could not create input recording: {e}"),
            )
            .file(path.display().to_string())
        })?;
        Ok(Self { file, last: None })
    }

    /// Record `held` as the set in effect from `step` onward, if it changed.
    fn sample(&mut self, step: u64, held: &InputState) -> Result<()> {
        if self.last.as_ref() == Some(held) {
            return Ok(());
        }
        // An initial empty set is the file's implicit starting state; only
        // record it as a line when it is a *return* to empty.
        if self.last.is_none() && held.is_empty() {
            return Ok(());
        }
        writeln!(self.file, "{}", held.timeline_line(step))
            .and_then(|()| self.file.flush())
            .map_err(|e| {
                EngineError::new(
                    codes::SCENE_WRITE_FAILED,
                    format!("could not write input recording: {e}"),
                )
            })?;
        self.last = Some(held.clone());
        Ok(())
    }
}

/// GPU-side state, created once the window exists.
enum Paint {
    Triangle(Renderer),
    Scene {
        renderer: SceneRenderer,
        depth: wgpu::TextureView,
        hud: engine_render::hud::HudRenderer,
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
                Paint::Scene {
                    renderer,
                    depth,
                    hud,
                },
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
                    sim.t += elapsed;

                    // System order: sample animations → physics → render.
                    if !sim.players.is_empty() {
                        engine_core::animation::apply_all(
                            &mut sim.scene,
                            &sim.players,
                            sim.t,
                        );
                    }
                    if sim.physics.is_some() || sim.scripts.is_some() || sim.recorder.is_some()
                    {
                        sim.accumulator += elapsed;
                        while sim.accumulator >= dt {
                            if let Some(recorder) = &mut sim.recorder {
                                if let Err(e) = recorder.sample(sim.step_index, &sim.held) {
                                    self.error = Some(e);
                                    break;
                                }
                            }
                            if let Some(scripts) = &sim.scripts {
                                // A failing script ends the session with a
                                // structured error, like any render failure.
                                match scripts.step(
                                    &mut sim.scene.world,
                                    sim.step_index,
                                    &sim.held,
                                ) {
                                    Ok(lines) => sim.hud_lines = lines,
                                    Err(e) => {
                                        self.error = Some(e);
                                        break;
                                    }
                                }
                            }
                            if let Some(physics) = &mut sim.physics {
                                physics.step(&mut sim.scene.world);
                            }
                            sim.step_index += 1;
                            sim.accumulator -= dt;
                        }
                    }
                    if let Ok(fresh) = sim.scene.render_items(&sim.assets) {
                        *items = fresh;
                    }
                    // Scripts may drive the camera entity (a chase camera);
                    // follow it rather than the pose captured at load.
                    if let Ok((fresh_camera, fresh_transform)) =
                        sim.scene.camera(sim.camera_name.as_deref())
                    {
                        *camera = fresh_camera;
                        *camera_model = fresh_transform.matrix();
                    }
                }
                let (width, height) = target.size();
                let view_projection = scene_renderer::view_projection(
                    camera,
                    *camera_model,
                    width as f32 / height as f32,
                );
                let hud_lines: &[String] = simulation
                    .as_ref()
                    .map_or(&[], |sim| sim.hud_lines.as_slice());
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
                    // The same overlay the screenshot path composites — the
                    // played game and the pinned PNG say the same thing.
                    hud.draw(device, queue, view, width, height, hud_lines);
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
                    hud: engine_render::hud::HudRenderer::new(
                        &target.gpu.device,
                        target.format(),
                    ),
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

            // The play mode: hardware key codes become the held set the next
            // fixed step samples. Keys outside the engine's allowlist are
            // dropped inside `press`.
            WindowEvent::KeyboardInput { event: key, .. } => {
                if let Content::Scene {
                    simulation: Some(sim),
                    ..
                } = &mut self.content
                {
                    if let PhysicalKey::Code(code) = key.physical_key {
                        let name = format!("{code:?}");
                        match key.state {
                            ElementState::Pressed => sim.held.press(&name),
                            ElementState::Released => sim.held.release(&name),
                        }
                    }
                }
            }

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

#[cfg(test)]
mod tests {
    use engine_core::input::is_known_key;
    use winit::keyboard::KeyCode;

    /// The viewer turns hardware codes into key names via `Debug` — this is
    /// only sound while winit's `KeyCode` Debug names match the W3C code
    /// names in `KNOWN_KEYS`. A winit upgrade that renames them must fail
    /// here, not silently produce a keyboard that types nothing.
    #[test]
    fn winit_keycode_debug_names_match_the_allowlist() {
        for (code, name) in [
            (KeyCode::ArrowUp, "ArrowUp"),
            (KeyCode::ArrowDown, "ArrowDown"),
            (KeyCode::ArrowLeft, "ArrowLeft"),
            (KeyCode::ArrowRight, "ArrowRight"),
            (KeyCode::KeyW, "KeyW"),
            (KeyCode::Space, "Space"),
            (KeyCode::ShiftLeft, "ShiftLeft"),
        ] {
            assert_eq!(format!("{code:?}"), name);
            assert!(is_known_key(name));
        }
    }
}
