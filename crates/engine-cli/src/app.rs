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
        /// The scene's water surfaces (M18). Refreshed per frame when a
        /// simulation runs — a script can move a surface — and static
        /// otherwise, exactly like `items`.
        water: Vec<engine_core::scene::WaterItem>,
        /// The scene's clouds (M20), refreshed on the same terms. Their
        /// drift runs on the simulated clock below, so a viewer session and
        /// a screenshot at the same step show the same sky.
        clouds: Vec<engine_core::scene::CloudItem>,
        /// The scene's roads (M23). Refreshed with `water` for the same
        /// reason: a script can move or repaint one.
        roads: Vec<engine_core::scene::RoadItem>,
        /// The scene's meadows (M29). Refreshed with `roads` for the same
        /// reason: a script can change what a field is made of between steps.
        meadows: Vec<engine_core::scene::MeadowItem>,
        camera: Camera,
        camera_model: Mat4,
        lights: engine_core::scene::ResolvedLights,
        /// The scene's sky, fog, shadow and MSAA settings. The viewer honors
        /// them so that what you fly around in is what `engine screenshot`
        /// pins — the frame rate differs, the picture must not.
        environment: engine_core::scene::EnvironmentSettings,
        /// The scene's day/night block, or `None`. Kept as *settings* rather
        /// than as resolved values because the viewer re-folds it every frame
        /// against the fixed-step clock — a cycling day has to actually move.
        daylight: Option<engine_core::daylight::DaylightSettings>,
        /// The scene's HUD components; refreshed per frame when a simulation
        /// runs (clips and scripts can drive them), static otherwise.
        hud_items: engine_core::scene::HudItems,
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
    pub particles: engine_core::particles::ParticleSystem,
    pub players: Vec<engine_core::animation::LoadedPlayer>,
    pub scripts: Option<engine_script::ScriptHost>,
    pub assets: engine_assets::AssetServer,
    /// `--camera` from the invocation, so the per-frame camera re-resolve
    /// follows the same entity the session started on.
    pub camera_name: Option<String>,
    /// The keys held right now, fed by window events and sampled at each
    /// fixed step — scripts never see a between-steps edge.
    pub held: InputState,
    /// Touching-state from the previous physics step, for script contact
    /// queries — same one-step latency as the headless path.
    pub contacts: engine_core::contact::ContactState,
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

/// The scene's HUD with the frame-rate readout added in the top-right corner:
/// white text on its own dim plate, sized to the string.
///
/// It rides the ordinary HUD components rather than a private drawing path,
/// so it composites through the same rasterizer as everything else and needs
/// no rendering code of its own. Appending puts it last within each class,
/// which draws it over a scene's own HUD; the script debug-line panel is
/// top-left and never collides with it.
fn with_fps_readout(hud: &engine_core::scene::HudItems, fps: &str) -> engine_core::scene::HudItems {
    use engine_core::components::{HudAnchor, HudRect, HudText};
    use engine_core::math::{Vec2, Vec3};

    /// Font cell at the readout's text size — `HudText.size` snaps to integer
    /// multiples of the 8×8 font, so 16 is exactly 2×.
    const SIZE: f32 = 16.0;
    /// Gap between the plate and the text inside it.
    const PAD: f32 = 4.0;
    /// Gap between the plate and the frame's corner.
    const INSET: f32 = 6.0;

    let mut hud = hud.clone();
    let text_width = fps.chars().count() as f32 * SIZE;
    hud.rects.push(HudRect {
        anchor: HudAnchor::TopRight,
        offset: Vec2::splat(INSET),
        size: Vec2::new(text_width + 2.0 * PAD, SIZE + 2.0 * PAD),
        color: Vec3::new(0.01, 0.015, 0.02),
        opacity: 0.55,
    });
    hud.texts.push(HudText {
        text: fps.to_string(),
        anchor: HudAnchor::TopRight,
        offset: Vec2::splat(INSET + PAD),
        size: SIZE,
        color: Vec3::ONE,
    });
    hud
}

/// GPU-side state, created once the window exists.
enum Paint {
    Triangle(Renderer),
    Scene {
        renderer: SceneRenderer,
        depth: wgpu::TextureView,
        /// The multisampled color attachment, when the scene asks for MSAA.
        /// Recreated with the depth buffer on every resize.
        msaa: Option<wgpu::TextureView>,
    },
}

/// Frames actually presented per second, averaged over a short window.
///
/// Wall-clock, and therefore viewer-only: it measures how fast this machine is
/// drawing right now, which is exactly what a headless render must never
/// depend on. `engine screenshot` and `engine diff-render` never see it, so
/// baselines stay reproducible.
struct FpsMeter {
    window_start: Option<Instant>,
    frames: u32,
    /// The last computed value; held between refreshes so the number on
    /// screen is readable rather than flickering every frame.
    shown: Option<f32>,
}

impl FpsMeter {
    /// How long to average over. Short enough to react to a stutter, long
    /// enough that the digits sit still.
    const WINDOW: f32 = 0.5;

    fn new() -> Self {
        Self {
            window_start: None,
            frames: 0,
            shown: None,
        }
    }

    /// Count a presented frame and return what the readout should say.
    fn tick(&mut self) -> String {
        let now = Instant::now();
        let start = *self.window_start.get_or_insert(now);
        self.frames += 1;

        let elapsed = (now - start).as_secs_f32();
        if elapsed >= Self::WINDOW {
            self.shown = Some(self.frames as f32 / elapsed);
            self.window_start = Some(now);
            self.frames = 0;
        }

        match self.shown {
            // Right-aligned in three columns so the readout does not jitter
            // between 99 and 100 fps.
            Some(fps) => format!("FPS {:>3}", fps.round() as u32),
            None => "FPS  --".to_string(),
        }
    }
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

    fps: FpsMeter,

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
            fps: FpsMeter::new(),
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
        // Sampled before the borrows below, and only for scene content: the
        // triangle viewer is a stack proof, not a game frame.
        let fps = matches!(self.content, Content::Scene { .. }).then(|| self.fps.tick());

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
                    msaa,
                },
                Content::Scene {
                    items,
                    water,
                    clouds,
                    roads,
                    meadows,
                    camera,
                    camera_model,
                    lights,
                    environment,
                    daylight,
                    hud_items,
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
                        engine_core::animation::apply_all(&mut sim.scene, &sim.players, sim.t);
                    }
                    if sim.physics.is_some()
                        || sim.scripts.is_some()
                        || sim.recorder.is_some()
                        || !sim.particles.is_empty()
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
                                    &sim.contacts,
                                ) {
                                    Ok(lines) => sim.hud_lines = lines,
                                    Err(e) => {
                                        self.error = Some(e);
                                        break;
                                    }
                                }
                                if let Some(physics) = &mut sim.physics {
                                    for blast in scripts.take_explosions() {
                                        physics.queue_explosion(engine_physics::Explosion {
                                            center: blast.center.into(),
                                            radius: blast.radius,
                                            impulse: blast.impulse,
                                        });
                                    }
                                }
                            }
                            if let Some(physics) = &mut sim.physics {
                                let events = physics.step(&mut sim.scene.world);
                                sim.contacts.apply(&events);
                                // Breaks apply after physics, exactly as in
                                // the headless loop — played and simulated
                                // runs must not diverge.
                                let forced = sim
                                    .scripts
                                    .as_ref()
                                    .map(engine_script::ScriptHost::take_breaks)
                                    .unwrap_or_default();
                                match engine_physics::apply_breaks(
                                    &mut sim.scene.world,
                                    physics,
                                    &forced,
                                ) {
                                    Ok(broke) if !broke.is_empty() => {
                                        sim.scene.refresh_names();
                                        if let Some(scripts) = &mut sim.scripts {
                                            scripts.sync_names(&sim.scene.world);
                                        }
                                    }
                                    Ok(_) => {}
                                    Err(e) => {
                                        self.error = Some(e);
                                        break;
                                    }
                                }
                            }
                            sim.particles.step(&sim.scene.world, dt);
                            sim.step_index += 1;
                            sim.accumulator -= dt;
                        }
                    }
                    if let Ok(fresh) = sim.scene.render_items(&sim.assets) {
                        *items = fresh;
                    }
                    *water = sim.scene.water_items();
                    *clouds = sim.scene.cloud_items();
                    *roads = sim.scene.road_items();
                    *meadows = sim.scene.meadow_items();
                    *hud_items = sim.scene.hud_items();
                    // Scripts may drive the camera entity (a chase camera);
                    // follow it rather than the pose captured at load.
                    if let Ok((fresh_camera, fresh_transform)) =
                        sim.scene.camera(sim.camera_name.as_deref())
                    {
                        *camera = fresh_camera;
                        *camera_model = fresh_transform.matrix();
                    }
                }
                let particles = simulation
                    .as_ref()
                    .map(|sim| sim.particles.instances(&sim.scene.world))
                    .unwrap_or_default();
                // Whole fixed steps, converted to seconds — the reproducible
                // clock. A scene with nothing to simulate has no clock at all
                // and its water sits at its t = 0 pose.
                let simulated_time = simulation
                    .as_ref()
                    .map(|sim| sim.step_index as f32 / sim.scene.physics.timestep_hz.max(1) as f32)
                    .unwrap_or(0.0);
                // Daylight runs on that same clock, for the same reason water
                // does: the viewer must show what a screenshot at this step
                // number would show. Re-folded every frame so a cycling day
                // moves, and a no-op when the scene has no daylight block.
                let (lights, environment) = engine_core::scene::apply_daylight(
                    daylight.as_ref(),
                    simulated_time,
                    *lights,
                    *environment,
                );
                let (width, height) = target.size();
                let view_projection = scene_renderer::view_projection(
                    camera,
                    *camera_model,
                    width as f32 / height as f32,
                );
                let hud_lines: &[String] = simulation
                    .as_ref()
                    .map_or(&[], |sim| sim.hud_lines.as_slice());
                // The same overlay the screenshot path composites — the
                // played game and the pinned PNG say the same thing — plus
                // the viewer-only frame-rate readout on top.
                let overlay = fps
                    .map(|fps| with_fps_readout(hud_items, &fps))
                    .unwrap_or_else(|| hud_items.clone());
                let no_lines = hud_lines.iter().all(|l| l.is_empty());
                let canvas = (!(overlay.is_empty() && no_lines))
                    .then(|| engine_render::hud::rasterize(&overlay, hud_lines, width, height));
                target.render_with(|device, queue, view| {
                    renderer.draw(
                        device,
                        queue,
                        ScenePass {
                            target: view,
                            msaa: msaa.as_ref(),
                            depth,
                            target_size: [width, height],
                            items,
                            water,
                            clouds,
                            roads,
                            meadows,
                            particles: &particles,
                            view_projection,
                            camera_position: camera_model.w_axis.truncate(),
                            camera_right: camera_model.x_axis.truncate(),
                            camera_up: camera_model.y_axis.truncate(),
                            lights,
                            environment,
                            // The viewer's water runs on the *simulated* clock,
                            // not on wall time: whole fixed steps taken since
                            // load. Flying around a lake for a minute and then
                            // screenshotting the same step number gives the same
                            // waves, which is the property the FPS readout is
                            // deliberately excluded from.
                            time: simulated_time,
                            clear: scene_renderer::DEFAULT_CLEAR,
                            hud: canvas.as_ref(),
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
            Content::Scene { environment, .. } => {
                let (width, height) = target.size();
                let samples = environment.samples.max(1);
                Paint::Scene {
                    renderer: SceneRenderer::with_samples(
                        &target.gpu.device,
                        target.format(),
                        samples,
                    ),
                    depth: scene_renderer::depth_texture_multisampled(
                        &target.gpu.device,
                        width,
                        height,
                        samples,
                    ),
                    msaa: (samples > 1).then(|| {
                        scene_renderer::msaa_color_texture(
                            &target.gpu.device,
                            target.format(),
                            width,
                            height,
                            samples,
                        )
                    }),
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
                    // The depth buffer — and the multisampled color target,
                    // when there is one — must track the swapchain size
                    // exactly.
                    if let Some(Paint::Scene {
                        depth,
                        msaa,
                        renderer,
                    }) = self.paint.as_mut()
                    {
                        let samples = renderer.samples();
                        *depth = scene_renderer::depth_texture_multisampled(
                            &target.gpu.device,
                            size.width,
                            size.height,
                            samples,
                        );
                        *msaa = (samples > 1).then(|| {
                            scene_renderer::msaa_color_texture(
                                &target.gpu.device,
                                renderer.format(),
                                size.width,
                                size.height,
                                samples,
                            )
                        });
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

    /// The readout sits in the top-right corner, on its own plate, clear of
    /// everything else: nothing is drawn in the other three corners, and a
    /// scene's own HUD survives underneath it.
    #[test]
    fn the_fps_readout_occupies_only_the_top_right_corner() {
        use engine_core::components::{HudAnchor, HudRect};
        use engine_core::math::{Vec2, Vec3};
        use engine_core::scene::HudItems;

        let (width, height) = (400u32, 200u32);
        let scene_hud = HudItems {
            rects: vec![HudRect {
                anchor: HudAnchor::BottomLeft,
                offset: Vec2::splat(10.0),
                size: Vec2::new(60.0, 12.0),
                color: Vec3::ONE,
                opacity: 1.0,
            }],
            texts: vec![],
        };

        let overlay = super::with_fps_readout(&scene_hud, "FPS  60");
        let canvas = engine_render::hud::rasterize(&overlay, &[], width, height);

        let lit: Vec<(u32, u32)> = (0..height)
            .flat_map(|y| (0..width).map(move |x| (x, y)))
            .filter(|&(x, y)| canvas.pixel(x, y)[3] > 0)
            .collect();
        assert!(!lit.is_empty());

        // The scene's own bar is still there, bottom-left.
        assert!(lit.contains(&(10, height - 11)), "scene HUD survives");

        // Everything else is in the top-right corner: 7 glyphs at 2× plus
        // padding, 6px in from the corner.
        let readout: Vec<(u32, u32)> = lit
            .iter()
            .copied()
            .filter(|&(_, y)| y < height / 2)
            .collect();
        assert!(readout.iter().all(|&(x, y)| {
            x >= width - (6 + 7 * 16 + 8) && x < width - 6 && (6..6 + 24).contains(&y)
        }));
        // Flush against the plate's right edge, and nothing in the left half.
        assert!(readout.iter().any(|&(x, _)| x == width - 7));
        assert!(!readout.iter().any(|&(x, _)| x < width / 2));

        // The readout is its own canvas: it does not drag the bottom-left bar
        // into one screen-sized rasterization.
        assert!(
            canvas.covered_pixels() < (width * height / 4) as usize,
            "corner elements must not merge into one big canvas: {} px",
            canvas.covered_pixels()
        );
    }
}
