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

    /// Run every script's `step` for step index `step`. The world is moved
    /// into the scripts' reach for the duration and moved back out even on
    /// error, so a failing script never swallows the ECS.
    pub fn step(&self, world: &mut World, step: u64) -> Result<()> {
        let api = WorldApi {
            world: Rc::new(RefCell::new(std::mem::take(world))),
            names: Rc::clone(&self.names),
            dt: self.dt,
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
            host.step(&mut scene.world, step).unwrap();
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
        let error = host.step(&mut scene.world, 0).unwrap_err();
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
        let error = host.step(&mut scene.world, 0).unwrap_err();
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
    fn no_time_or_io_exists_in_the_sandbox() {
        let dir = temp_dir("sandbox");
        let (mut scene, path) = scene_with_script(
            &dir,
            r#"fn step(world, step) { timestamp(); }"#,
        );
        let host = ScriptHost::build(&scene.world, &path, 60).unwrap().unwrap();
        let error = host.step(&mut scene.world, 0).unwrap_err();
        assert_eq!(
            error.error, "script_runtime_error",
            "timestamp() must not exist: {error:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
