//! Rhai scripting (M10): gameplay logic as data.
//!
//! Scripts run once per fixed step — the same integer clock as physics — in
//! the fixed system order *animations → scripts → physics → render*. The
//! engine registers a deliberately small `world` API and nothing else: no
//! time, no I/O, no randomness, so `simulate --steps N` stays byte-identical
//! with scripts running (determinism is the contract, M8 §2).
//!
//! Scripts mutate component fields and never invent state: whatever they
//! compute beyond their writes dies at the end of the step, and baked output
//! is an ordinary valid scene file.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;

use engine_core::components::{Name, Script, Transform};
use engine_core::input::{self, InputState};
use engine_core::{codes, EngineError, Result};
use hecs::World;
use rhai::packages::{BasicArrayPackage, BasicMathPackage, CorePackage, Package};
use rhai::{Dynamic, EvalAltResult, Position, Scope, AST};

/// Per-call operation budget: a runaway loop becomes a structured error,
/// which is the deterministic answer to a hang.
const MAX_OPERATIONS: u64 = 1_000_000;

struct CompiledScript {
    /// The entity the `Script` component sits on — error context.
    owner: String,
    /// The script file, as displayed in errors.
    file: String,
    ast: AST,
}

/// The scripting host for one run: compiled ASTs plus the curated engine.
pub struct ScriptHost {
    engine: rhai::Engine,
    scripts: Vec<CompiledScript>,
    names: Rc<HashMap<String, hecs::Entity>>,
    dt: f32,
}

/// What scripts see: the world, entity names, and the fixed timestep.
/// Cloned freely (it is two `Rc`s); the world inside is moved in for the
/// duration of one step and moved back out after.
#[derive(Clone)]
struct WorldApi {
    world: Rc<RefCell<World>>,
    names: Rc<HashMap<String, hecs::Entity>>,
    dt: f32,
    /// The keys held during the current step (M11). Empty unless the caller
    /// has input to offer, so runs without `--input` behave exactly as they
    /// did before input existed.
    input: Rc<InputState>,
}

impl WorldApi {
    fn with_transform<T>(
        &mut self,
        name: &str,
        f: impl FnOnce(&mut Transform) -> T,
    ) -> std::result::Result<T, Box<EvalAltResult>> {
        let entity = *self.names.get(name).ok_or_else(|| {
            Box::new(EvalAltResult::ErrorRuntime(
                format!("no entity named {name:?}").into(),
                Position::NONE,
            ))
        })?;
        let world = self.world.borrow_mut();
        let mut transform = world.get::<&mut Transform>(entity).map_err(|_| {
            Box::new(EvalAltResult::ErrorRuntime(
                format!("entity {name:?} has no Transform").into(),
                Position::NONE,
            ))
        })?;
        Ok(f(&mut transform))
    }
}

fn vec3_array(v: glam::Vec3) -> rhai::Array {
    vec![
        Dynamic::from_float(f64::from(v.x)),
        Dynamic::from_float(f64::from(v.y)),
        Dynamic::from_float(f64::from(v.z)),
    ]
}

/// Build the curated engine: core language + arrays + math, and the `world`
/// API. Nothing else exists — no time, no eval, no files.
fn curated_engine() -> rhai::Engine {
    let mut engine = rhai::Engine::new_raw();
    engine.register_global_module(CorePackage::new().as_shared_module());
    engine.register_global_module(BasicArrayPackage::new().as_shared_module());
    engine.register_global_module(BasicMathPackage::new().as_shared_module());
    engine.set_max_operations(MAX_OPERATIONS);

    engine.register_type_with_name::<WorldApi>("World");
    engine.register_fn("dt", |w: &mut WorldApi| f64::from(w.dt));

    engine.register_fn(
        "key",
        |w: &mut WorldApi, name: &str| -> std::result::Result<bool, Box<EvalAltResult>> {
            if !input::is_known_key(name) {
                // Deterministic failure over a silently-never-pressed key.
                let mut message = format!("{name:?} names no known key");
                if let Some(suggestion) = input::closest_key(name) {
                    message.push_str(&format!(" (did you mean {suggestion:?}?)"));
                }
                return Err(Box::new(EvalAltResult::ErrorRuntime(
                    message.into(),
                    Position::NONE,
                )));
            }
            Ok(w.input.is_held(name))
        },
    );

    engine.register_fn(
        "position",
        |w: &mut WorldApi, name: &str| -> std::result::Result<rhai::Array, Box<EvalAltResult>> {
            w.with_transform(name, |t| vec3_array(t.position))
        },
    );
    engine.register_fn(
        "set_position",
        |w: &mut WorldApi,
         name: &str,
         x: f64,
         y: f64,
         z: f64|
         -> std::result::Result<(), Box<EvalAltResult>> {
            w.with_transform(name, |t| {
                t.position = glam::Vec3::new(x as f32, y as f32, z as f32);
            })
        },
    );
    engine.register_fn(
        "rotation",
        |w: &mut WorldApi, name: &str| -> std::result::Result<rhai::Array, Box<EvalAltResult>> {
            w.with_transform(name, |t| vec3_array(t.rotation))
        },
    );
    engine.register_fn(
        "set_rotation",
        |w: &mut WorldApi,
         name: &str,
         x: f64,
         y: f64,
         z: f64|
         -> std::result::Result<(), Box<EvalAltResult>> {
            w.with_transform(name, |t| {
                t.rotation = glam::Vec3::new(x as f32, y as f32, z as f32);
            })
        },
    );
    engine.register_fn(
        "look_at",
        |w: &mut WorldApi,
         name: &str,
         x: f64,
         y: f64,
         z: f64|
         -> std::result::Result<(), Box<EvalAltResult>> {
            w.with_transform(name, |t| {
                let forward = glam::Vec3::new(x as f32, y as f32, z as f32) - t.position;
                if forward.length_squared() < 1e-12 {
                    return; // aiming at yourself is a no-op, not an error
                }
                // Camera convention: an entity faces its local -Z, so +Z is
                // the vector *away* from the target; +Y stays as close to
                // world-up as the aim allows (a level horizon — the reason
                // this exists, since composing pitch and yaw through the
                // XYZ Euler order introduces roll).
                let back = -forward.normalize();
                let right = glam::Vec3::Y.cross(back);
                let right = if right.length_squared() < 1e-9 {
                    glam::Vec3::X // straight up or down: any right works
                } else {
                    right.normalize()
                };
                let up = back.cross(right);
                let (rx, ry, rz) = glam::Quat::from_mat3(&glam::Mat3::from_cols(
                    right, up, back,
                ))
                .to_euler(glam::EulerRot::XYZ);
                t.rotation =
                    glam::Vec3::new(rx.to_degrees(), ry.to_degrees(), rz.to_degrees());
            })
        },
    );
    engine.register_fn(
        "scale",
        |w: &mut WorldApi, name: &str| -> std::result::Result<rhai::Array, Box<EvalAltResult>> {
            w.with_transform(name, |t| vec3_array(t.scale))
        },
    );
    engine.register_fn(
        "set_scale",
        |w: &mut WorldApi,
         name: &str,
         x: f64,
         y: f64,
         z: f64|
         -> std::result::Result<(), Box<EvalAltResult>> {
            w.with_transform(name, |t| {
                t.scale = glam::Vec3::new(x as f32, y as f32, z as f32);
            })
        },
    );

    engine
}

fn parse_error(file: &str, owner: &str, e: &rhai::ParseError) -> EngineError {
    let mut error = EngineError::new(
        codes::SCRIPT_PARSE_ERROR,
        format!("script does not compile: {}", e.0),
    )
    .file(file)
    .entity(owner);
    if let Some(line) = e.1.line() {
        error = error.line(line as u32);
    }
    if let Some(column) = e.1.position() {
        error = error.column(column as u32);
    }
    error
}

impl ScriptHost {
    /// Compile every `Script` in the world. `None` when the scene has no
    /// scripts — script-free scenes pay nothing.
    pub fn build(
        world: &World,
        scene_path: &Path,
        timestep_hz: u32,
    ) -> Result<Option<Self>> {
        let base_dir = scene_path.parent().unwrap_or(Path::new(""));
        let engine = curated_engine();

        let mut scripts = Vec::new();
        for (name, script) in world.query::<(&Name, &Script)>().iter() {
            let path = base_dir.join(&script.source);
            let file = path.display().to_string();
            let source = std::fs::read_to_string(&path).map_err(|e| {
                EngineError::new(
                    codes::ASSET_NOT_FOUND,
                    format!("could not read script {file}: {e}"),
                )
                .file(&file)
                .entity(&name.0)
            })?;

            let ast = engine
                .compile(&source)
                .map_err(|e| parse_error(&file, &name.0, &e))?;

            if !ast.iter_functions().any(|f| f.name == "step") {
                return Err(EngineError::new(
                    codes::SCRIPT_MISSING_STEP_FN,
                    format!("script {file} defines no `fn step(world, step)`"),
                )
                .file(&file)
                .entity(&name.0));
            }

            scripts.push(CompiledScript {
                owner: name.0.clone(),
                file,
                ast,
            });
        }

        if scripts.is_empty() {
            return Ok(None);
        }

        // Deterministic script order: sort by owning entity name.
        scripts.sort_by(|a, b| a.owner.cmp(&b.owner));

        let names: HashMap<String, hecs::Entity> = world
            .query::<(hecs::Entity, &Name)>()
            .iter()
            .map(|(entity, name)| (name.0.clone(), entity))
            .collect();

        Ok(Some(Self {
            engine,
            scripts,
            names: Rc::new(names),
            dt: 1.0 / timestep_hz.max(1) as f32,
        }))
    }

    /// Run every script's `step` for step index `step`, with `input` as the
    /// held-key set for the duration of the step. The world is moved into
    /// the scripts' reach for the duration and moved back out even on
    /// error, so a failing script never swallows the ECS.
    pub fn step(&self, world: &mut World, step: u64, input: &InputState) -> Result<()> {
        let api = WorldApi {
            world: Rc::new(RefCell::new(std::mem::take(world))),
            names: Rc::clone(&self.names),
            dt: self.dt,
            input: Rc::new(input.clone()),
        };

        let mut failure: Option<EngineError> = None;
        for script in &self.scripts {
            let result = self.engine.call_fn::<()>(
                &mut Scope::new(),
                &script.ast,
                "step",
                (api.clone(), step as i64),
            );
            if let Err(e) = result {
                let mut error = EngineError::new(
                    codes::SCRIPT_RUNTIME_ERROR,
                    format!("script failed at step {step}: {e}"),
                )
                .file(&script.file)
                .entity(&script.owner);
                if let Some(line) = e.position().line() {
                    error = error.line(line as u32);
                }
                failure = Some(error);
                break;
            }
        }

        let inner = match Rc::try_unwrap(api.world) {
            Ok(cell) => cell.into_inner(),
            // A script cannot retain the API past its call, but degrade
            // rather than panic if that invariant ever breaks.
            Err(shared) => std::mem::take(&mut *shared.borrow_mut()),
        };
        *world = inner;

        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

/// The script pass of `engine validate`: does every referenced script
/// compile and define `step`? Mirrors the asset pass — expects a scene that
/// already passed structural validation.
pub fn validate_scene_scripts(source: &str, path: &str) -> Vec<EngineError> {
    let Ok(file) = serde_json::from_str::<engine_core::SceneFile>(source) else {
        return Vec::new();
    };

    let base_dir = Path::new(path).parent().unwrap_or(Path::new(""));
    let engine = curated_engine();
    let mut errors = Vec::new();

    for entity in &file.entities {
        for component in &entity.components {
            let engine_core::components::ComponentData::Script(script) = component else {
                continue;
            };
            let script_path = base_dir.join(&script.source);
            let display = script_path.display().to_string();
            let Ok(script_source) = std::fs::read_to_string(&script_path) else {
                continue; // asset_not_found already reported structurally.
            };

            match engine.compile(&script_source) {
                Err(e) => errors.push(parse_error(&display, &entity.name, &e)),
                Ok(ast) => {
                    if !ast.iter_functions().any(|f| f.name == "step") {
                        errors.push(
                            EngineError::new(
                                codes::SCRIPT_MISSING_STEP_FN,
                                format!(
                                    "script {display} defines no `fn step(world, step)`"
                                ),
                            )
                            .file(&display)
                            .entity(&entity.name),
                        );
                    }
                }
            }
        }
    }

    errors
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_core::Scene;

    fn scene_with_script(dir: &Path, script: &str) -> (Scene, std::path::PathBuf) {
        std::fs::create_dir_all(dir.join("scripts")).unwrap();
        std::fs::write(dir.join("scripts/test.rhai"), script).unwrap();
        let scene_json = r#"{"name":"s","entities":[
            {"name":"Mover","components":[
                {"type":"Transform","position":[0.0,0.25,0.0]},
                {"type":"Mesh","asset":"builtin:cube"},
                {"type":"Script","source":"scripts/test.rhai"}
            ]},
            {"name":"Cam","components":[{"type":"Camera","active":true}]}
        ]}"#;
        let scene_path = dir.join("scene.json");
        std::fs::write(&scene_path, scene_json).unwrap();
        let scene = Scene::from_source(scene_json, &scene_path.display().to_string()).unwrap();
        (scene, scene_path)
    }

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("engine-script-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn scripts_move_entities_deterministically() {
        let dir = temp_dir("move");
        let (mut scene, path) = scene_with_script(
            &dir,
            r#"fn step(world, step) {
                if step < 120 {
                    let p = world.position("Mover");
                    world.set_position("Mover", p[0], p[1] + 2.0 / 120.0, p[2]);
                }
            }"#,
        );
        let host = ScriptHost::build(&scene.world, &path, 60).unwrap().unwrap();
        for step in 0..150 {
            host.step(&mut scene.world, step, &InputState::default()).unwrap();
        }

        let entity = scene.entity("Mover").unwrap();
        let y = scene.world.get::<&Transform>(entity).unwrap().position.y;
        assert!((y - 2.25).abs() < 1e-4, "elevator should stop at 2.25, is at {y}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_runtime_error_is_structured_and_restores_the_world() {
        let dir = temp_dir("boom");
        let (mut scene, path) = scene_with_script(
            &dir,
            r#"fn step(world, step) { world.position("Nobody"); }"#,
        );
        let host = ScriptHost::build(&scene.world, &path, 60).unwrap().unwrap();
        let error = host.step(&mut scene.world, 0, &InputState::default()).unwrap_err();
        assert_eq!(error.error, "script_runtime_error");
        assert!(error.message.contains("Nobody"), "{}", error.message);
        assert_eq!(error.context().unwrap().entity.as_deref(), Some("Mover"));

        // The world survived the failure.
        assert!(scene.entity("Mover").is_some());
        assert!(scene.world.get::<&Transform>(scene.entity("Mover").unwrap()).is_ok());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_infinite_loop_hits_the_operation_budget() {
        let dir = temp_dir("spin");
        let (mut scene, path) = scene_with_script(
            &dir,
            r#"fn step(world, step) { let x = 0; loop { x += 1; } }"#,
        );
        let host = ScriptHost::build(&scene.world, &path, 60).unwrap().unwrap();
        let error = host.step(&mut scene.world, 0, &InputState::default()).unwrap_err();
        assert_eq!(error.error, "script_runtime_error");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn parse_and_missing_step_errors_carry_the_script_file() {
        let dir = temp_dir("parse");
        std::fs::create_dir_all(dir.join("scripts")).unwrap();
        std::fs::write(dir.join("scripts/test.rhai"), "fn step(world step) {}").unwrap();
        let scene_json = r#"{"name":"s","entities":[
            {"name":"Mover","components":[
                {"type":"Transform"},
                {"type":"Script","source":"scripts/test.rhai"}
            ]}
        ]}"#;
        std::fs::write(dir.join("scene.json"), scene_json).unwrap();
        let errors = validate_scene_scripts(
            scene_json,
            &dir.join("scene.json").display().to_string(),
        );
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].error, "script_parse_error");
        assert!(errors[0].context().unwrap().line.is_some());

        std::fs::write(dir.join("scripts/test.rhai"), "fn stpe(world, step) {}").unwrap();
        let errors = validate_scene_scripts(
            scene_json,
            &dir.join("scene.json").display().to_string(),
        );
        assert_eq!(errors[0].error, "script_missing_step_fn");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn look_at_aims_the_local_minus_z_with_a_level_horizon() {
        let dir = temp_dir("lookat");
        let (mut scene, path) = scene_with_script(
            &dir,
            r#"fn step(world, step) {
                if step == 0 { world.look_at("Mover", 0.0, 0.25, -5.0); }
                if step == 1 { world.look_at("Mover", 5.0, 0.25, 0.0); }
            }"#,
        );
        let host = ScriptHost::build(&scene.world, &path, 60).unwrap().unwrap();
        let entity = scene.entity("Mover").unwrap();

        host.step(&mut scene.world, 0, &InputState::default()).unwrap();
        let r = scene.world.get::<&Transform>(entity).unwrap().rotation;
        assert!(r.abs_diff_eq(glam::Vec3::ZERO, 1e-4), "straight ahead is identity: {r}");

        host.step(&mut scene.world, 1, &InputState::default()).unwrap();
        let t = *scene.world.get::<&Transform>(entity).unwrap();
        // Facing +X from the origin: forward (-Z rotated) must be +X, and
        // the entity's up must stay world-up (no roll).
        let rotation = glam::Quat::from_euler(
            glam::EulerRot::XYZ,
            t.rotation.x.to_radians(),
            t.rotation.y.to_radians(),
            t.rotation.z.to_radians(),
        );
        let forward = rotation * -glam::Vec3::Z;
        assert!(forward.abs_diff_eq(glam::Vec3::X, 1e-4), "forward is {forward}");
        let up = rotation * glam::Vec3::Y;
        assert!(up.abs_diff_eq(glam::Vec3::Y, 1e-4), "up is {up}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn scripts_read_held_keys_and_typos_fail_with_a_suggestion() {
        let dir = temp_dir("keys");
        let (mut scene, path) = scene_with_script(
            &dir,
            r#"fn step(world, step) {
                if world.key("ArrowUp") {
                    let p = world.position("Mover");
                    world.set_position("Mover", p[0] + 1.0, p[1], p[2]);
                }
            }"#,
        );
        let host = ScriptHost::build(&scene.world, &path, 60).unwrap().unwrap();

        let mut held = InputState::default();
        held.press("ArrowUp");
        host.step(&mut scene.world, 0, &held).unwrap();
        host.step(&mut scene.world, 1, &InputState::default()).unwrap();

        let entity = scene.entity("Mover").unwrap();
        let x = scene.world.get::<&Transform>(entity).unwrap().position.x;
        assert!((x - 1.0).abs() < 1e-6, "only the held step moves: x = {x}");

        let (mut scene, path) = scene_with_script(
            &dir,
            r#"fn step(world, step) { world.key("ArowUp"); }"#,
        );
        let host = ScriptHost::build(&scene.world, &path, 60).unwrap().unwrap();
        let error = host
            .step(&mut scene.world, 0, &InputState::default())
            .unwrap_err();
        assert_eq!(error.error, "script_runtime_error");
        assert!(
            error.message.contains("did you mean \"ArrowUp\""),
            "{}",
            error.message
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn no_time_or_io_exists_in_the_sandbox() {
        let dir = temp_dir("sandbox");
        let (mut scene, path) = scene_with_script(
            &dir,
            r#"fn step(world, step) { timestamp(); }"#,
        );
        let host = ScriptHost::build(&scene.world, &path, 60).unwrap().unwrap();
        let error = host.step(&mut scene.world, 0, &InputState::default()).unwrap_err();
        assert_eq!(
            error.error, "script_runtime_error",
            "timestamp() must not exist: {error:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
