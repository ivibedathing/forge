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

/// Step a scene's physics `steps` times, optionally writing a JSONL trace.
/// Returns the physics world (for queries) and the total contact count.
pub fn run(
    scene: &mut Scene,
    steps: u32,
    mut trace: Option<&mut dyn Write>,
) -> Result<(PhysicsWorld, u64)> {
    let mut physics = PhysicsWorld::build(&scene.world, &scene.physics)?;
    let trace_names = physics.dynamic_entity_names(&scene.world);
    let mut contacts = 0u64;

    for step in 1..=steps {
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
/// scene file: `Transform` and `RigidBody` velocities updated for dynamic
/// bodies, every other byte preserved.
pub fn bake(
    source: &str,
    scene: &Scene,
    physics: &PhysicsWorld,
    out: &Path,
) -> Result<()> {
    let mut baked = source.to_string();

    for name in physics.dynamic_entity_names(&scene.world) {
        let Some(entity) = scene.entity(&name) else {
            continue;
        };
        let transform = scene
            .world
            .get::<&Transform>(entity)
            .map(|t| *t)
            .unwrap_or_default();
        let body = scene.world.get::<&RigidBody>(entity).map(|b| *b);

        let mut edits = vec![
            SetComponentField {
                entity: name.clone(),
                component: "Transform".into(),
                field: "position".into(),
                value: vec3_json(transform.position),
            },
            SetComponentField {
                entity: name.clone(),
                component: "Transform".into(),
                field: "rotation".into(),
                value: vec3_json(transform.rotation),
            },
        ];
        if let Ok(body) = body {
            edits.push(SetComponentField {
                entity: name.clone(),
                component: "RigidBody".into(),
                field: "linear_velocity".into(),
                value: vec3_json(body.linear_velocity),
            });
            edits.push(SetComponentField {
                entity: name.clone(),
                component: "RigidBody".into(),
                field: "angular_velocity".into(),
                value: vec3_json(body.angular_velocity),
            });
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

    let (physics, contacts) = run(
        &mut scene,
        steps,
        trace_file.as_mut().map(|f| f as &mut dyn Write),
    )?;

    if let Some(bake_path) = &bake_path {
        bake(&source, &scene, &physics, bake_path)?;
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

    let (mut physics, _) = run(&mut scene, steps, None)?;
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
