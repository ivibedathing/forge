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
use engine_core::{codes, EngineError, Result, Scene};
use engine_physics::PhysicsWorld;
use glam::Vec3;
use serde_json::{json, Value};

fn vec3_json(v: Vec3) -> Value {
    Value::Array(v.to_array().into_iter().map(number_from_f32).collect())
}

/// Step a scene `steps` times — scripts then physics, per the fixed system
/// order — optionally writing a JSONL trace. Returns the physics world (for
/// queries) and the total contact count.
pub fn run(
    scene: &mut Scene,
    scene_path: &Path,
    steps: u32,
    mut trace: Option<&mut dyn Write>,
) -> Result<(PhysicsWorld, u64)> {
    let scripts =
        engine_script::ScriptHost::build(&scene.world, scene_path, scene.physics.timestep_hz)?;
    let mut physics = PhysicsWorld::build(&scene.world, &scene.physics)?;
    let trace_names = physics.dynamic_entity_names(&scene.world);
    let mut contacts = 0u64;

    for step in 1..=steps {
        if let Some(scripts) = &scripts {
            scripts.step(&mut scene.world, u64::from(step) - 1)?;
        }
        let events = physics.step(&mut scene.world);

        if let Some(trace) = trace.as_deref_mut() {
            for name in &trace_names {
                let Some(entity) = scene.entity(name) else {
                    continue;
                };
                let transform = scene
                    .world
                    .get::<&Transform>(entity)
                    .map(|t| *t)
                    .unwrap_or_default();
                let body = scene
                    .world
                    .get::<&RigidBody>(entity)
                    .map(|b| *b)
                    .ok();
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
        }
        contacts += events.len() as u64;
    }

    Ok((physics, contacts))
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
/// any `Transform` or `RigidBody` field that differs from the file's rest
/// value gets spliced — which captures dynamic bodies, script-driven
/// kinematics, and plain script-moved entities uniformly, without touching
/// entities nothing moved.
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
            continue;
        };

        let mut edits: Vec<SetComponentField> = Vec::new();
        let edit = |field: &str, component: &str, value: Vec3| SetComponentField {
            entity: def.name.clone(),
            component: component.into(),
            field: field.into(),
            value: vec3_json(value),
        };

        if def.components.iter().any(|c| matches!(c, ComponentData::Transform(_))) {
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
        if def.components.iter().any(|c| matches!(c, ComponentData::RigidBody(_))) {
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
                    edits.push(edit("linear_velocity", "RigidBody", current.linear_velocity));
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
        for edit in edits {
            baked = formatter::apply_set_component_field(&baked, &edit)?;
        }
    }

    formatter::write_atomic(out, &baked)
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
    bake_path: Option<PathBuf>,
    trace_path: Option<PathBuf>,
) -> Result<()> {
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

    let (_physics, contacts) = run(
        &mut scene,
        &scene_path,
        steps,
        trace_file.as_mut().map(|f| f as &mut dyn Write),
    )?;

    if let Some(bake_path) = &bake_path {
        bake(&source, &scene, bake_path)?;
    }

    let mut result = json!({
        "simulated_steps": steps,
        "timestep_hz": scene.physics.timestep_hz,
        "contacts": contacts,
    });
    if let Some(path) = &bake_path {
        result["baked"] = json!(path.display().to_string());
    }
    if let Some(path) = &trace_path {
        result["trace"] = json!(path.display().to_string());
    }
    println!("{result}");
    Ok(())
}

/// The `engine raycast` command.
pub fn raycast_command(
    scene_path: PathBuf,
    from: String,
    direction: String,
    steps: u32,
) -> Result<()> {
    let from = parse_vec3(&from)?;
    let direction = parse_vec3(&direction)?;

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

    let (mut physics, _) = run(&mut scene, &scene_path, steps, None)?;
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
