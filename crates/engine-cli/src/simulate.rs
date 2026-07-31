//! `engine simulate` and friends: stepping, traces, and baking.
//!
//! Time becomes observable text here (physics design §1): a trace is JSONL an
//! agent greps, a bake is a valid scene file whose untouched bytes are
//! byte-preserved through the M7 formatter — the same splice discipline the
//! editor uses, for the same reason.

use std::io::Write;
use std::path::{Path, PathBuf};

use engine_core::components::{RigidBody, Transform};
use engine_core::formatter::{self, number_from_f32, SetComponentField};
use engine_core::input::{InputState, InputTimeline, Pointer, Viewport};
use engine_core::particles::ParticleSystem;
use engine_core::{codes, EngineError, Result, Scene};
use engine_physics::PhysicsWorld;
use engine_script::ScriptHost;
use glam::Vec3;
use serde_json::{json, Value};

fn vec3_json(v: Vec3) -> Value {
    Value::Array(v.to_array().into_iter().map(number_from_f32).collect())
}

fn vec2_json(v: glam::Vec2) -> Value {
    Value::Array(v.to_array().into_iter().map(number_from_f32).collect())
}

/// The button `world.hovered`/`pressed`/`clicked` answer for (M31).
const PRIMARY: &str = "MouseLeft";

/// Step a scene `steps` times — scripts, physics, then particles, per the
/// fixed system order — optionally replaying an input timeline and writing a
/// JSONL trace. No timeline means no keys held, which keeps every pre-input
/// trace and baseline byte-identical.
///
/// `view` is the frame the timeline's cursor is measured in (M28): the real
/// one for commands that render, [`Viewport::DEFAULT`] for the ones that do
/// not. It is what turns a cursor into a world ray, so a mouse-driven scene
/// is a function of it — see `designs/mouse-input-design.md` §5.
pub fn run(
    scene: &mut Scene,
    scene_path: &Path,
    steps: u32,
    input: Option<&InputTimeline>,
    view: &Viewport,
    mut trace: Option<&mut dyn Write>,
) -> Result<StepRun> {
    let assets = engine_assets::AssetServer::for_scene(scene_path);
    let mut scripts = engine_script::ScriptHost::build(
        &scene.world,
        scene_path,
        scene.physics.timestep_hz,
        scene.daylight.clone(),
        &assets,
    )?;
    let mut physics = PhysicsWorld::build(&scene.world, &scene.physics, &assets)?;
    let mut particles = ParticleSystem::build(&scene.world);
    let dt = 1.0 / scene.physics.timestep_hz.max(1) as f32;
    let mut contacts = 0u64;
    let no_keys = InputState::default();
    // What scripts see at step N is the touching-state physics left at step
    // N-1 — the causal order under animations → scripts → physics.
    let mut contact_state = engine_core::contact::ContactState::default();
    let mut hud: Vec<String> = Vec::new();
    let mut traced_hud: Vec<String> = Vec::new();
    // What the pointer is doing to the overlay (M31). Runtime state of the
    // same kind as `world.state` and the contact state: replay-deterministic,
    // reset by a fresh run, never baked.
    let mut interaction = engine_core::ui::Interaction::default();

    for step in 1..=steps {
        if let Some(scripts) = &scripts {
            let step_index = u64::from(step) - 1;
            let held = input.map_or(&no_keys, |t| t.held_at(step_index));
            // The pointer is resolved against the camera *this* step will be
            // scripted with, through the same `Scene::camera` selection the
            // render makes — so what a script aims at is what the picture
            // shows, and the viewer runs this identical resolution.
            let pointer = Pointer::resolve(
                held,
                view,
                scene
                    .camera(view.camera.as_deref())
                    .ok()
                    .map(|(camera, transform)| (camera, transform.matrix())),
            );
            // Hit-testing runs before scripts, against the layout for the
            // frame this command is rendering — `view` is the real size for
            // commands that render and `Viewport::DEFAULT` for the ones that
            // do not (M28's rule, which M31 inherits rather than revisits).
            // The cursor is a *fraction*, so this is where it becomes pixels.
            let frame = glam::Vec2::new(view.width as f32, view.height as f32);
            let tree = scene.hud_tree(&assets);
            if !tree.is_empty() {
                let layout = engine_core::ui::layout(&tree, view.width, view.height);
                // `MouseLeft` alone drives the widget model. The other two
                // buttons stay available raw through `world.mouse`, because a
                // right-click is a context action in every UI ever written and
                // making it press a button would be a surprise with no way to
                // opt out of it.
                interaction.update(
                    &tree,
                    &layout,
                    pointer.cursor * frame,
                    held.is_held(PRIMARY),
                );
            }
            hud = scripts.step(
                &mut scene.world,
                step_index,
                held,
                &pointer,
                &interaction,
                &contact_state,
            )?;
            for blast in scripts.take_explosions() {
                physics.queue_explosion(engine_physics::Explosion {
                    center: Vec3::from(blast.center),
                    radius: blast.radius,
                    impulse: blast.impulse,
                });
            }
        }
        let events = physics.step(&mut scene.world);
        contact_state.apply(&events);
        // Particles read the post-physics world, so an emitter riding a
        // dynamic body trails where the body actually went this step.
        particles.step(&scene.world, dt);

        if let Some(trace) = trace.as_deref_mut() {
            // Re-enumerated every step: a broken entity's row disappears the
            // step after its break, and fragment rows join then. Sorted, so
            // scenes where nothing breaks trace identically every step.
            let trace_names = physics.dynamic_entity_names(&scene.world);
            for name in &trace_names {
                let Some(entity) = scene.entity(name) else {
                    continue;
                };
                let transform = scene
                    .world
                    .get::<&Transform>(entity)
                    .map(|t| *t)
                    .unwrap_or_default();
                let body = scene.world.get::<&RigidBody>(entity).map(|b| *b).ok();
                let mut line = json!({
                    "step": step,
                    "entity": name,
                    "position": vec3_json(transform.position),
                    "rotation": vec3_json(transform.rotation),
                });
                if let Some(body) = body {
                    line["linear_velocity"] = vec3_json(body.linear_velocity);
                }
                write_line(trace, &line)?;
            }
            for event in &events {
                write_line(
                    trace,
                    &json!({
                        "step": step,
                        "contact": [event.a, event.b],
                        "started": event.started,
                    }),
                )?;
            }
            // The HUD is part of the observable record: one line whenever it
            // changes, so a lap crossing is a greppable trace event. Script-
            // free scenes never emit one and pre-HUD traces stay byte-exact.
            if hud != traced_hud {
                write_line(trace, &json!({ "step": step, "hud": hud }))?;
                traced_hud = hud.clone();
            }
        }
        contacts += events.len() as u64;

        // Breaks apply after physics, before the next step's scripts: the
        // broken entity traced its final position above, and its fragments
        // enter the rows from the next step.
        let forced = scripts
            .as_ref()
            .map(ScriptHost::take_breaks)
            .unwrap_or_default();
        let broke = engine_physics::apply_breaks(&mut scene.world, &mut physics, &forced)?;
        if !broke.is_empty() {
            scene.refresh_names();
            if let Some(scripts) = &mut scripts {
                scripts.sync_names(&scene.world);
            }
            if let Some(trace) = trace.as_deref_mut() {
                for event in &broke {
                    let fragments: Vec<&str> = event
                        .fragments
                        .iter()
                        .map(|(name, _)| name.as_str())
                        .collect();
                    write_line(
                        trace,
                        &json!({ "step": step, "broke": event.entity, "fragments": fragments }),
                    )?;
                }
            }
        }
    }

    Ok(StepRun {
        physics,
        particles,
        contacts,
        hud,
        interaction,
    })
}

/// What stepping a scene produced: the physics world (for queries), the
/// particle system (for rendering), the total contact count, the HUD
/// lines the final step's scripts pushed, and where the pointer left the
/// overlay.
pub struct StepRun {
    pub physics: PhysicsWorld,
    pub particles: ParticleSystem,
    pub contacts: u64,
    pub hud: Vec<String>,
    /// The final step's hover and press state (M31), so the render that
    /// follows can tint the element under the cursor. Carried out of the run
    /// rather than recomputed, because recomputing it would need the cursor
    /// again and is one more place the two could disagree.
    pub interaction: engine_core::ui::Interaction,
}

fn write_line(trace: &mut dyn Write, line: &Value) -> Result<()> {
    writeln!(trace, "{line}").map_err(|e| {
        EngineError::new(
            codes::SCENE_WRITE_FAILED,
            format!("could not write trace: {e}"),
        )
    })
}

/// Write the simulated state back into the original source text as a valid
/// scene file, every untouched byte preserved. The rule is change-based:
/// any `Transform`, `RigidBody`, or HUD-component field that differs from
/// the file's rest value gets spliced — which captures dynamic bodies,
/// script-driven kinematics, and script-driven HUD state uniformly, without
/// touching entities nothing moved. The rule extends to structure: a file
/// entity that no longer exists in the world (it broke) is spliced out, and
/// a world entity not in the file (a fragment) is spliced in with its full
/// current state, so a baked post-break scene reloads into exactly the
/// post-break world.
pub fn bake(source: &str, scene: &Scene, out: &Path) -> Result<()> {
    let file: engine_core::SceneFile = serde_json::from_str(source).map_err(|e| {
        EngineError::new(
            codes::SCENE_PARSE_DESYNC,
            format!("bake input no longer parses: {e}"),
        )
    })?;
    use engine_core::components::ComponentData;

    let mut baked = source.to_string();
    for def in &file.entities {
        let Some(entity) = scene.entity(&def.name) else {
            // Gone from the world: it broke into fragments. Splice it out.
            baked = formatter::apply_remove_entity(
                &baked,
                &formatter::RemoveEntity {
                    entity: def.name.clone(),
                },
            )?;
            continue;
        };

        let mut edits: Vec<SetComponentField> = Vec::new();
        let field_edit = |field: &str, component: &str, value: Value| SetComponentField {
            entity: def.name.clone(),
            component: component.into(),
            field: field.into(),
            value,
        };
        let edit = |field: &str, component: &str, value: Vec3| {
            field_edit(field, component, vec3_json(value))
        };

        if def
            .components
            .iter()
            .any(|c| matches!(c, ComponentData::Transform(_)))
        {
            if let Ok(current) = scene.world.get::<&Transform>(entity) {
                let rest = def
                    .components
                    .iter()
                    .find_map(|c| match c {
                        ComponentData::Transform(t) => Some(*t),
                        _ => None,
                    })
                    .unwrap_or_default();
                if current.position != rest.position {
                    edits.push(edit("position", "Transform", current.position));
                }
                if current.rotation != rest.rotation {
                    edits.push(edit("rotation", "Transform", current.rotation));
                }
                if current.scale != rest.scale {
                    edits.push(edit("scale", "Transform", current.scale));
                }
            }
        }
        if def
            .components
            .iter()
            .any(|c| matches!(c, ComponentData::RigidBody(_)))
        {
            if let Ok(current) = scene.world.get::<&RigidBody>(entity) {
                let rest = def
                    .components
                    .iter()
                    .find_map(|c| match c {
                        ComponentData::RigidBody(b) => Some(*b),
                        _ => None,
                    })
                    .expect("guarded above");
                if current.linear_velocity != rest.linear_velocity {
                    edits.push(edit(
                        "linear_velocity",
                        "RigidBody",
                        current.linear_velocity,
                    ));
                }
                if current.angular_velocity != rest.angular_velocity {
                    edits.push(edit(
                        "angular_velocity",
                        "RigidBody",
                        current.angular_velocity,
                    ));
                }
            }
        }
        // Script-driven HUD state is scene state like any other: a changed
        // readout or gauge width lands in the baked file.
        if def
            .components
            .iter()
            .any(|c| matches!(c, ComponentData::HudText(_)))
        {
            if let Ok(current) = scene.world.get::<&engine_core::components::HudText>(entity) {
                let rest = def
                    .components
                    .iter()
                    .find_map(|c| match c {
                        ComponentData::HudText(t) => Some(t.clone()),
                        _ => None,
                    })
                    .expect("guarded above");
                if current.text != rest.text {
                    edits.push(field_edit(
                        "text",
                        "HudText",
                        Value::String(current.text.clone()),
                    ));
                }
                // A moved element is script-driven HUD state like a re-worded
                // one (M28): a crosshair the run left somewhere reopens
                // there.
                if current.offset != rest.offset {
                    edits.push(field_edit("offset", "HudText", vec2_json(current.offset)));
                }
                if current.visible != rest.visible {
                    edits.push(field_edit(
                        "visible",
                        "HudText",
                        Value::Bool(current.visible),
                    ));
                }
                if current.color != rest.color {
                    edits.push(field_edit("color", "HudText", vec3_json(current.color)));
                }
            }
        }
        if def
            .components
            .iter()
            .any(|c| matches!(c, ComponentData::HudRect(_)))
        {
            if let Ok(current) = scene.world.get::<&engine_core::components::HudRect>(entity) {
                let rest = def
                    .components
                    .iter()
                    .find_map(|c| match c {
                        ComponentData::HudRect(r) => Some(r.clone()),
                        _ => None,
                    })
                    .expect("guarded above");
                if current.size != rest.size {
                    edits.push(field_edit("size", "HudRect", vec2_json(current.size)));
                }
                if current.offset != rest.offset {
                    edits.push(field_edit("offset", "HudRect", vec2_json(current.offset)));
                }
                if current.visible != rest.visible {
                    edits.push(field_edit(
                        "visible",
                        "HudRect",
                        Value::Bool(current.visible),
                    ));
                }
                if current.color != rest.color {
                    edits.push(field_edit("color", "HudRect", vec3_json(current.color)));
                }
                if current.opacity != rest.opacity {
                    edits.push(field_edit(
                        "opacity",
                        "HudRect",
                        number_from_f32(current.opacity),
                    ));
                }
            }
        }
        // M31's containers and images bake on the same change-based rule: a
        // run that opened a menu bakes a scene with the menu open. `visible`
        // is the one that carries the most, since it is how a menu opens and
        // closes at all.
        if def
            .components
            .iter()
            .any(|c| matches!(c, ComponentData::HudPanel(_)))
        {
            if let Ok(current) = scene
                .world
                .get::<&engine_core::components::HudPanel>(entity)
            {
                let rest = def
                    .components
                    .iter()
                    .find_map(|c| match c {
                        ComponentData::HudPanel(p) => Some(p.clone()),
                        _ => None,
                    })
                    .expect("guarded above");
                if current.visible != rest.visible {
                    edits.push(field_edit(
                        "visible",
                        "HudPanel",
                        Value::Bool(current.visible),
                    ));
                }
                if current.offset != rest.offset {
                    edits.push(field_edit("offset", "HudPanel", vec2_json(current.offset)));
                }
                if current.color != rest.color {
                    edits.push(field_edit("color", "HudPanel", vec3_json(current.color)));
                }
                if current.opacity != rest.opacity {
                    edits.push(field_edit(
                        "opacity",
                        "HudPanel",
                        number_from_f32(current.opacity),
                    ));
                }
                // A panel sized by a script has stopped hugging, and the baked
                // file has to say so or reloading it would re-hug.
                if current.width != rest.width {
                    if let Some(width) = current.width {
                        edits.push(field_edit("width", "HudPanel", number_from_f32(width)));
                    }
                }
                if current.height != rest.height {
                    if let Some(height) = current.height {
                        edits.push(field_edit("height", "HudPanel", number_from_f32(height)));
                    }
                }
            }
        }
        if def
            .components
            .iter()
            .any(|c| matches!(c, ComponentData::HudImage(_)))
        {
            if let Ok(current) = scene
                .world
                .get::<&engine_core::components::HudImage>(entity)
            {
                let rest = def
                    .components
                    .iter()
                    .find_map(|c| match c {
                        ComponentData::HudImage(i) => Some(i.clone()),
                        _ => None,
                    })
                    .expect("guarded above");
                if current.visible != rest.visible {
                    edits.push(field_edit(
                        "visible",
                        "HudImage",
                        Value::Bool(current.visible),
                    ));
                }
                if current.offset != rest.offset {
                    edits.push(field_edit("offset", "HudImage", vec2_json(current.offset)));
                }
                if current.size != rest.size {
                    edits.push(field_edit("size", "HudImage", vec2_json(current.size)));
                }
                if current.tint != rest.tint {
                    edits.push(field_edit("tint", "HudImage", vec3_json(current.tint)));
                }
                if current.opacity != rest.opacity {
                    edits.push(field_edit(
                        "opacity",
                        "HudImage",
                        number_from_f32(current.opacity),
                    ));
                }
            }
        }
        // A script-driven emission rate is scene state too. The particles
        // themselves are not baked (they are disposable simulation state,
        // like solver caches) — but `rate` is an authored component field
        // that a script changed, so it bakes under the same change-based
        // rule as a velocity or a gauge width.
        if def
            .components
            .iter()
            .any(|c| matches!(c, ComponentData::ParticleEmitter(_)))
        {
            if let Ok(current) = scene
                .world
                .get::<&engine_core::components::ParticleEmitter>(entity)
            {
                let rest = def
                    .components
                    .iter()
                    .find_map(|c| match c {
                        ComponentData::ParticleEmitter(e) => Some(*e),
                        _ => None,
                    })
                    .expect("guarded above");
                if current.rate != rest.rate {
                    edits.push(field_edit(
                        "rate",
                        "ParticleEmitter",
                        number_from_f32(current.rate),
                    ));
                }
            }
        }
        // Script-driven light state (M17), same rule again. A flickering
        // campfire's light is at some intensity when the run stops, and that is
        // the intensity a baked scene has to reopen at, or the resumed scene is
        // lit differently from the one that was saved.
        macro_rules! bake_light {
            ($variant:ident, $ty:ty) => {
                if def
                    .components
                    .iter()
                    .any(|c| matches!(c, ComponentData::$variant(_)))
                {
                    if let Ok(current) = scene.world.get::<&$ty>(entity) {
                        let rest = def
                            .components
                            .iter()
                            .find_map(|c| match c {
                                ComponentData::$variant(l) => Some(*l),
                                _ => None,
                            })
                            .expect("guarded above");
                        if current.intensity != rest.intensity {
                            edits.push(field_edit(
                                "intensity",
                                stringify!($variant),
                                number_from_f32(current.intensity),
                            ));
                        }
                        if current.color != rest.color {
                            edits.push(edit("color", stringify!($variant), current.color));
                        }
                    }
                }
            };
        }
        bake_light!(PointLight, engine_core::components::PointLight);
        bake_light!(DirectionalLight, engine_core::components::DirectionalLight);
        bake_light!(AmbientLight, engine_core::components::AmbientLight);
        for edit in edits {
            baked = formatter::apply_set_component_field(&baked, &edit)?;
        }
    }

    // Entities the run spawned (fragments): splice each in as a full entity
    // with its current state, in name order for a deterministic file.
    let file_names: std::collections::HashSet<&str> =
        file.entities.iter().map(|d| d.name.as_str()).collect();
    let mut spawned: Vec<(String, engine_core::hecs::Entity)> = scene
        .world
        .query::<(engine_core::hecs::Entity, &engine_core::components::Name)>()
        .iter()
        .filter(|(_, name)| !file_names.contains(name.0.as_str()))
        .map(|(entity, name)| (name.0.clone(), entity))
        .collect();
    spawned.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, entity) in spawned {
        let components = ComponentData::collect_from(&scene.world, entity)
            .iter()
            .map(|component| {
                let kind = component.name().to_string();
                let mut value = serde_json::to_value(component).map_err(|e| {
                    EngineError::new(
                        codes::OUTPUT_SERIALIZATION_FAILED,
                        format!("could not serialize a {kind} for bake: {e}"),
                    )
                })?;
                clean_numbers(&mut value);
                let fields: Vec<(String, Value)> = value
                    .as_object()
                    .map(|object| {
                        object
                            .iter()
                            .filter(|(key, _)| key.as_str() != "type")
                            .map(|(key, field)| (key.clone(), field.clone()))
                            .collect()
                    })
                    .unwrap_or_default();
                Ok((kind, fields))
            })
            .collect::<Result<Vec<_>>>()?;
        baked = formatter::apply_add_entity(&baked, &formatter::AddEntity { name, components })?;
    }

    formatter::write_atomic(out, &baked)
}

/// Rewrite every number through the f32-shortest text path, so serialized
/// components bake as `0.1` rather than the f64-widened
/// `0.10000000149011612`. Lossless: every component numeric field is f32.
fn clean_numbers(value: &mut Value) {
    match value {
        Value::Number(n) => {
            if let Some(f) = n.as_f64() {
                *value = number_from_f32(f as f32);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(clean_numbers),
        Value::Object(map) => map.values_mut().for_each(clean_numbers),
        _ => {}
    }
}

/// Load an `--input` timeline, if one was given. Every timeline error is
/// emitted; the last one becomes the command's result, matching the
/// all-errors-at-once scene-loading pattern.
pub fn load_input(path: Option<&Path>) -> Result<Option<InputTimeline>> {
    let Some(path) = path else {
        return Ok(None);
    };
    InputTimeline::load(path).map(Some).map_err(|mut errors| {
        let last = errors.pop().expect("timeline errors are never empty");
        for error in errors {
            error.emit();
        }
        last
    })
}

/// Parse an `x,y,z` CLI argument.
pub fn parse_vec3(text: &str) -> Result<Vec3> {
    let parts: Vec<f32> = text
        .split(',')
        .map(|p| p.trim().parse::<f32>())
        .collect::<std::result::Result<_, _>>()
        .map_err(|e| {
            EngineError::new(
                codes::INVALID_INVOCATION,
                format!("expected x,y,z numbers, got {text:?} ({e})"),
            )
        })?;
    if parts.len() != 3 {
        return Err(EngineError::new(
            codes::INVALID_INVOCATION,
            format!("expected exactly three comma-separated numbers, got {text:?}"),
        ));
    }
    Ok(Vec3::new(parts[0], parts[1], parts[2]))
}

/// The `engine simulate` command.
pub fn simulate_command(
    scene_path: PathBuf,
    steps: u32,
    input_path: Option<PathBuf>,
    bake_path: Option<PathBuf>,
    trace_path: Option<PathBuf>,
    requested: Vec<String>,
) -> Result<()> {
    let input = load_input(input_path.as_deref())?;
    let report = crate::report_scene_diagnostics(&scene_path);
    let display = scene_path.display().to_string();
    if report.errors > 0 {
        return Err(EngineError::new(
            codes::VALIDATION_FAILED,
            format!("{} error(s) in {display}", report.errors),
        )
        .file(&display));
    }
    let source = report.source.unwrap_or_default();
    let mut scene = Scene::from_source(&source, &display)
        .map_err(|mut errors| errors.pop().expect("non-empty"))?;

    let mut trace_file = match &trace_path {
        Some(path) => Some(std::fs::File::create(path).map_err(|e| {
            EngineError::new(
                codes::SCENE_WRITE_FAILED,
                format!("could not create trace file: {e}"),
            )
            .file(path.display().to_string())
        })?),
        None => None,
    };

    let outcome = run(
        &mut scene,
        &scene_path,
        steps,
        input.as_ref(),
        // `simulate` renders nothing and so has no frame of its own; a
        // mouse-driven script sees the documented default (M28 §5).
        &Viewport::DEFAULT,
        trace_file.as_mut().map(|f| f as &mut dyn Write),
    )?;

    if let Some(bake_path) = &bake_path {
        bake(&source, &scene, bake_path)?;
    }

    let entities = final_states(&scene, &outcome.physics, &requested, &display)?;

    let mut result = json!({
        "simulated_steps": steps,
        "timestep_hz": scene.physics.timestep_hz,
        "contacts": outcome.contacts,
        "entities": entities,
    });
    if !outcome.hud.is_empty() {
        // The final step's HUD, so an agent reads the lap timer without a
        // trace file.
        result["hud"] = json!(outcome.hud);
    }
    if let Some(path) = &bake_path {
        result["baked"] = json!(path.display().to_string());
    }
    if let Some(path) = &trace_path {
        result["trace"] = json!(path.display().to_string());
    }
    println!("{result}");
    Ok(())
}

/// Where everything ended up, for the `simulate` report (M25).
///
/// The report used to be `{contacts, simulated_steps, timestep_hz}` — three
/// numbers, none of which is where anything is. Learning that a body landed at
/// y = 1.2 meant writing a trace (125 lines and 17.8 KB for a 120-step run) and
/// parsing its tail, or baking a whole scene file and reading a `Transform` back
/// out; the M22 CLI test does the second to assert one number. The data was
/// already here.
///
/// **The rows are the trace's rows.** Same fields, same name-sorted order, same
/// rule for which entities appear by default — the dynamic bodies, re-enumerated
/// after the run, so fragments are in and a broken parent is out. An agent that
/// can read one can read the other, and the sort is not cosmetic: it is what
/// makes an unchanged scene report identically instead of in whatever order
/// hecs laid out its archetypes.
///
/// `--entity NAME` narrows to named entities and reaches ones the trace does
/// not enumerate at all — a scripted kinematic platform, a camera a chase
/// script is driving — which is the case `--trace` cannot serve today.
fn final_states(
    scene: &Scene,
    physics: &PhysicsWorld,
    requested: &[String],
    display: &str,
) -> Result<Vec<Value>> {
    let names: Vec<String> = if requested.is_empty() {
        physics.dynamic_entity_names(&scene.world)
    } else {
        // Every unknown name at once, the way validation reports: an agent
        // fixing three typos should learn all three from one run.
        let unknown: Vec<&String> = requested
            .iter()
            .filter(|name| scene.entity(name).is_none())
            .collect();
        if let Some(last) = unknown.last() {
            for name in &unknown[..unknown.len() - 1] {
                EngineError::new(codes::ENTITY_NOT_FOUND, format!("no entity named {name:?}"))
                    .entity(*name)
                    .file(display)
                    .suggest_from(name, scene.names())
                    .emit();
            }
            return Err(EngineError::new(
                codes::ENTITY_NOT_FOUND,
                format!("no entity named {last:?}"),
            )
            .entity(*last)
            .file(display)
            .suggest_from(last, scene.names()));
        }
        let mut names: Vec<String> = requested.to_vec();
        names.sort();
        names.dedup();
        names
    };

    Ok(names
        .iter()
        .filter_map(|name| {
            let entity = scene.entity(name)?;
            let transform = scene
                .world
                .get::<&Transform>(entity)
                .map(|t| *t)
                .unwrap_or_default();
            let mut state = json!({
                "entity": name,
                "position": vec3_json(transform.position),
                "rotation": vec3_json(transform.rotation),
            });
            // Exactly the trace's fields, including its omissions: no
            // angular velocity, no scale. Parity is the contract — one shape
            // to learn — and a field is cheap to add later and breaking to
            // remove.
            if let Ok(body) = scene.world.get::<&RigidBody>(entity) {
                state["linear_velocity"] = vec3_json(body.linear_velocity);
            }
            Some(state)
        })
        .collect())
}

/// The `engine raycast` command.
pub fn raycast_command(
    scene_path: PathBuf,
    from: String,
    direction: String,
    steps: u32,
    input_path: Option<PathBuf>,
) -> Result<()> {
    let from = parse_vec3(&from)?;
    let direction = parse_vec3(&direction)?;
    let input = load_input(input_path.as_deref())?;

    let report = crate::report_scene_diagnostics(&scene_path);
    let display = scene_path.display().to_string();
    if report.errors > 0 {
        return Err(EngineError::new(
            codes::VALIDATION_FAILED,
            format!("{} error(s) in {display}", report.errors),
        )
        .file(&display));
    }
    let source = report.source.unwrap_or_default();
    let mut scene = Scene::from_source(&source, &display)
        .map_err(|mut errors| errors.pop().expect("non-empty"))?;

    let mut physics = run(
        &mut scene,
        &scene_path,
        steps,
        input.as_ref(),
        &Viewport::DEFAULT,
        None,
    )?
    .physics;
    physics.refresh_queries();

    let result = match physics.raycast(from, direction) {
        Some(hit) => json!({
            "hit": {
                "entity": hit.entity,
                "point": vec3_json(hit.point),
                "normal": vec3_json(hit.normal),
                "distance": number_from_f32(hit.distance),
            }
        }),
        None => json!({ "hit": null }),
    };
    println!("{result}");
    Ok(())
}
