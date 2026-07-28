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
//! is an ordinary valid scene file. Two deliberate exceptions ride on the
//! host rather than the world: the numeric key/value store behind
//! `world.state`/`world.set_state` (per-run memory — a lap timer's start
//! step — deterministic under replay, reset by a fresh run, and *not*
//! captured by bake, exactly like physics solver caches), and the HUD line
//! list behind `world.hud` (cleared every step, so what is on screen is a
//! pure function of the step that drew it).

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;

use engine_core::components::{
    Breakable, HudRect, HudText, Name, ParticleEmitter, RigidBody, Script, Transform, Wheel,
};
use engine_core::contact::ContactState;
use engine_core::input::{self, InputState};
use engine_core::{codes, EngineError, Result};
use hecs::World;
use rhai::packages::{
    BasicArrayPackage, BasicMathPackage, BasicStringPackage, CorePackage, MoreStringPackage,
    Package,
};
use rhai::{Dynamic, EvalAltResult, Position, Scope, AST};

/// Per-call operation budget: a runaway loop becomes a structured error,
/// which is the deterministic answer to a hang.
const MAX_OPERATIONS: u64 = 1_000_000;

/// HUD caps: enough for a readable overlay, small enough that a runaway
/// loop cannot render the frame unreadable. Exceeding either is a runtime
/// error, not a truncation — deterministic and loud.
const MAX_HUD_LINES: usize = 16;
const MAX_HUD_CHARS: usize = 96;

/// A blast queued by `world.explode`, waiting for the next physics step.
/// Plain numbers so the crate that owns physics can translate — scripting
/// does not depend on the physics crate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct QueuedExplosion {
    pub center: [f32; 3],
    pub radius: f32,
    pub impulse: f32,
}

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
    /// The `world.state` store: per-run, shared by every script (script
    /// order is deterministic, so cross-script reads are too).
    state: Rc<RefCell<HashMap<String, f64>>>,
    /// Breaks queued by `world.break_entity`, drained by the sim loop after
    /// the step via [`ScriptHost::take_breaks`].
    breaks: Rc<RefCell<Vec<String>>>,
    /// Blasts queued by `world.explode`, drained via
    /// [`ScriptHost::take_explosions`].
    explosions: Rc<RefCell<Vec<QueuedExplosion>>>,
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
    /// Who touches whom, as of the previous physics step (M12). Scripts run
    /// before physics, so a hit at physics step N is visible at step N+1 —
    /// the causal order, documented on `ContactState`.
    contacts: Rc<ContactState>,
    /// `world.state` / `world.set_state`: numeric per-run memory.
    state: Rc<RefCell<HashMap<String, f64>>>,
    /// `world.hud`: the lines pushed during this step, in push order.
    hud: Rc<RefCell<Vec<String>>>,
    /// `world.break_entity`: entity names queued to break after this step.
    breaks: Rc<RefCell<Vec<String>>>,
    /// `world.explode`: blasts queued for the next physics step.
    explosions: Rc<RefCell<Vec<QueuedExplosion>>>,
}

impl WorldApi {
    fn entity(&self, name: &str) -> std::result::Result<hecs::Entity, Box<EvalAltResult>> {
        self.names.get(name).copied().ok_or_else(|| {
            Box::new(EvalAltResult::ErrorRuntime(
                format!("no entity named {name:?}").into(),
                Position::NONE,
            ))
        })
    }

    /// Borrow one component mutably for the duration of a closure; `what`
    /// names the component type in the missing-component error.
    fn with_component<C: hecs::Component, T>(
        &mut self,
        name: &str,
        what: &str,
        f: impl FnOnce(&mut C) -> T,
    ) -> std::result::Result<T, Box<EvalAltResult>> {
        let entity = self.entity(name)?;
        let world = self.world.borrow_mut();
        let mut component = world.get::<&mut C>(entity).map_err(|_| {
            Box::new(EvalAltResult::ErrorRuntime(
                format!("entity {name:?} has no {what}").into(),
                Position::NONE,
            ))
        })?;
        Ok(f(&mut component))
    }

    fn with_transform<T>(
        &mut self,
        name: &str,
        f: impl FnOnce(&mut Transform) -> T,
    ) -> std::result::Result<T, Box<EvalAltResult>> {
        self.with_component(name, "Transform", f)
    }

    /// The vehicle path: velocity access needs a `RigidBody`; what a write
    /// means is the physics step's business (dynamic bodies pick it up
    /// before integrating — see `PhysicsWorld::step`).
    fn with_rigid_body<T>(
        &mut self,
        name: &str,
        f: impl FnOnce(&mut RigidBody) -> T,
    ) -> std::result::Result<T, Box<EvalAltResult>> {
        self.with_component(name, "RigidBody", f)
    }

    /// The wheel path (M12): drive/brake/steer access needs a `Wheel`;
    /// physics reads the fields into its vehicle controller each step.
    fn with_wheel<T>(
        &mut self,
        name: &str,
        f: impl FnOnce(&mut Wheel) -> T,
    ) -> std::result::Result<T, Box<EvalAltResult>> {
        let entity = self.entity(name)?;
        let world = self.world.borrow_mut();
        let mut wheel = world.get::<&mut Wheel>(entity).map_err(|_| {
            Box::new(EvalAltResult::ErrorRuntime(
                format!("entity {name:?} has no Wheel").into(),
                Position::NONE,
            ))
        })?;
        Ok(f(&mut wheel))
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
    // String building (concat, to_string, pad) so scripts can compose HUD
    // text. Pure functions only — still no time, no I/O, no randomness.
    engine.register_global_module(BasicStringPackage::new().as_shared_module());
    engine.register_global_module(MoreStringPackage::new().as_shared_module());
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

    // Contact queries (M12): what the previous physics step left touching.
    // The entity must exist (a typo'd name is an error, not silence), but
    // needs no particular component — contacts are keyed by name.
    engine.register_fn(
        "touching",
        |w: &mut WorldApi, name: &str| -> std::result::Result<rhai::Array, Box<EvalAltResult>> {
            w.entity(name)?;
            Ok(w.contacts
                .touching(name)
                .into_iter()
                .map(Dynamic::from)
                .collect())
        },
    );
    engine.register_fn(
        "contacts_started",
        |w: &mut WorldApi, name: &str| -> std::result::Result<rhai::Array, Box<EvalAltResult>> {
            w.entity(name)?;
            Ok(w.contacts
                .started_with(name)
                .into_iter()
                .map(Dynamic::from)
                .collect())
        },
    );

    engine.register_fn(
        "hud",
        |w: &mut WorldApi, text: &str| -> std::result::Result<(), Box<EvalAltResult>> {
            let mut hud = w.hud.borrow_mut();
            if hud.len() >= MAX_HUD_LINES {
                return Err(Box::new(EvalAltResult::ErrorRuntime(
                    format!("the HUD holds at most {MAX_HUD_LINES} lines per step").into(),
                    Position::NONE,
                )));
            }
            if text.chars().count() > MAX_HUD_CHARS {
                return Err(Box::new(EvalAltResult::ErrorRuntime(
                    format!("a HUD line holds at most {MAX_HUD_CHARS} characters").into(),
                    Position::NONE,
                )));
            }
            if let Some(bad) = text.chars().find(|c| !('\x20'..'\x7f').contains(c)) {
                return Err(Box::new(EvalAltResult::ErrorRuntime(
                    format!("HUD text is printable ASCII only, got {bad:?}").into(),
                    Position::NONE,
                )));
            }
            hud.push(text.to_string());
            Ok(())
        },
    );
    engine.register_fn("state", |w: &mut WorldApi, key: &str, default: f64| -> f64 {
        w.state.borrow().get(key).copied().unwrap_or(default)
    });
    engine.register_fn("state", |w: &mut WorldApi, key: &str, default: i64| -> f64 {
        w.state.borrow().get(key).copied().unwrap_or(default as f64)
    });
    engine.register_fn("set_state", |w: &mut WorldApi, key: &str, value: f64| {
        w.state.borrow_mut().insert(key.to_string(), value);
    });
    engine.register_fn("set_state", |w: &mut WorldApi, key: &str, value: i64| {
        w.state.borrow_mut().insert(key.to_string(), value as f64);
    });

    engine.register_fn(
        "break_entity",
        |w: &mut WorldApi, name: &str| -> std::result::Result<(), Box<EvalAltResult>> {
            // Validated at call time (unknown name, nothing to break into):
            // deterministic failure over a silent no-op. The break itself
            // applies after this step's physics.
            let entity = w.entity(name)?;
            if w.world.borrow().get::<&Breakable>(entity).is_err() {
                return Err(Box::new(EvalAltResult::ErrorRuntime(
                    format!("entity {name:?} has no Breakable").into(),
                    Position::NONE,
                )));
            }
            w.breaks.borrow_mut().push(name.to_string());
            Ok(())
        },
    );
    engine.register_fn(
        "explode",
        |w: &mut WorldApi,
         x: f64,
         y: f64,
         z: f64,
         radius: f64,
         impulse: f64|
         -> std::result::Result<(), Box<EvalAltResult>> {
            if !(radius > 0.0) {
                return Err(Box::new(EvalAltResult::ErrorRuntime(
                    format!("explosion radius must be positive, got {radius}").into(),
                    Position::NONE,
                )));
            }
            if impulse < 0.0 {
                return Err(Box::new(EvalAltResult::ErrorRuntime(
                    format!("explosion impulse cannot be negative, got {impulse}").into(),
                    Position::NONE,
                )));
            }
            w.explosions.borrow_mut().push(QueuedExplosion {
                center: [x as f32, y as f32, z as f32],
                radius: radius as f32,
                impulse: impulse as f32,
            });
            Ok(())
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
        "forward",
        |w: &mut WorldApi, name: &str| -> std::result::Result<rhai::Array, Box<EvalAltResult>> {
            w.with_transform(name, |t| {
                // The entity's world-space -Z. Scripts must use this rather
                // than treating rotation[1] as "the yaw": XYZ Euler restricts
                // the middle angle to ±90°, so a physics-integrated yaw past
                // that comes back as the twin (±180, θ, ±180) and naive yaw
                // math silently goes wrong.
                let rotation = glam::Quat::from_euler(
                    glam::EulerRot::XYZ,
                    t.rotation.x.to_radians(),
                    t.rotation.y.to_radians(),
                    t.rotation.z.to_radians(),
                );
                vec3_array(rotation * -glam::Vec3::Z)
            })
        },
    );
    engine.register_fn(
        "linear_velocity",
        |w: &mut WorldApi, name: &str| -> std::result::Result<rhai::Array, Box<EvalAltResult>> {
            w.with_rigid_body(name, |b| vec3_array(b.linear_velocity))
        },
    );
    engine.register_fn(
        "set_linear_velocity",
        |w: &mut WorldApi,
         name: &str,
         x: f64,
         y: f64,
         z: f64|
         -> std::result::Result<(), Box<EvalAltResult>> {
            w.with_rigid_body(name, |b| {
                b.linear_velocity = glam::Vec3::new(x as f32, y as f32, z as f32);
            })
        },
    );
    engine.register_fn(
        "angular_velocity",
        |w: &mut WorldApi, name: &str| -> std::result::Result<rhai::Array, Box<EvalAltResult>> {
            w.with_rigid_body(name, |b| vec3_array(b.angular_velocity))
        },
    );
    engine.register_fn(
        "set_angular_velocity",
        |w: &mut WorldApi,
         name: &str,
         x: f64,
         y: f64,
         z: f64|
         -> std::result::Result<(), Box<EvalAltResult>> {
            w.with_rigid_body(name, |b| {
                b.angular_velocity = glam::Vec3::new(x as f32, y as f32, z as f32);
            })
        },
    );
    // Wheel controls (M12): pedals and steering wheel. Physics reads these
    // into its raycast-vehicle controller before every step.
    engine.register_fn(
        "engine_force",
        |w: &mut WorldApi, name: &str| -> std::result::Result<f64, Box<EvalAltResult>> {
            w.with_wheel(name, |wheel| f64::from(wheel.engine_force))
        },
    );
    engine.register_fn(
        "set_engine_force",
        |w: &mut WorldApi,
         name: &str,
         force: f64|
         -> std::result::Result<(), Box<EvalAltResult>> {
            w.with_wheel(name, |wheel| wheel.engine_force = force as f32)
        },
    );
    engine.register_fn(
        "brake",
        |w: &mut WorldApi, name: &str| -> std::result::Result<f64, Box<EvalAltResult>> {
            w.with_wheel(name, |wheel| f64::from(wheel.brake))
        },
    );
    engine.register_fn(
        "set_brake",
        |w: &mut WorldApi,
         name: &str,
         force: f64|
         -> std::result::Result<(), Box<EvalAltResult>> {
            w.with_wheel(name, |wheel| wheel.brake = force as f32)
        },
    );
    engine.register_fn(
        "steering",
        |w: &mut WorldApi, name: &str| -> std::result::Result<f64, Box<EvalAltResult>> {
            w.with_wheel(name, |wheel| f64::from(wheel.steering))
        },
    );
    engine.register_fn(
        "set_steering",
        |w: &mut WorldApi,
         name: &str,
         degrees: f64|
         -> std::result::Result<(), Box<EvalAltResult>> {
            w.with_wheel(name, |wheel| wheel.steering = degrees as f32)
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
    // HUD components (M12): scripts drive on-screen text and gauge bars the
    // same way they drive Transforms — the elements are ordinary components,
    // so a script write is visible to screenshot, bake, and the viewer alike.
    engine.register_fn(
        "hud_text",
        |w: &mut WorldApi, name: &str| -> std::result::Result<String, Box<EvalAltResult>> {
            w.with_component::<HudText, _>(name, "HudText", |t| t.text.clone())
        },
    );
    engine.register_fn(
        "set_hud_text",
        |w: &mut WorldApi,
         name: &str,
         text: &str|
         -> std::result::Result<(), Box<EvalAltResult>> {
            let text = text.to_string();
            w.with_component::<HudText, _>(name, "HudText", move |t| t.text = text)
        },
    );
    engine.register_fn(
        "hud_rect_size",
        |w: &mut WorldApi, name: &str| -> std::result::Result<rhai::Array, Box<EvalAltResult>> {
            w.with_component::<HudRect, _>(name, "HudRect", |r| {
                vec![
                    Dynamic::from_float(f64::from(r.size.x)),
                    Dynamic::from_float(f64::from(r.size.y)),
                ]
            })
        },
    );
    engine.register_fn(
        "set_hud_rect_size",
        |w: &mut WorldApi,
         name: &str,
         width: f64,
         height: f64|
         -> std::result::Result<(), Box<EvalAltResult>> {
            w.with_component::<HudRect, _>(name, "HudRect", |r| {
                r.size = glam::Vec2::new(width as f32, height as f32);
            })
        },
    );

    // Emission rate (M13): the one particle parameter a script drives, so
    // effects can answer to gameplay — a skidding tire smokes, a healthy
    // engine does not. `ParticleEmitter` re-reads `rate` every step, so the
    // write takes effect on the same step; particle *state* stays untouched,
    // which keeps rate 0 a pause (live particles live out their lifetime)
    // rather than a reset.
    engine.register_fn(
        "particle_rate",
        |w: &mut WorldApi, name: &str| -> std::result::Result<f64, Box<EvalAltResult>> {
            w.with_component::<ParticleEmitter, _>(name, "ParticleEmitter", |e| {
                f64::from(e.rate)
            })
        },
    );
    engine.register_fn(
        "set_particle_rate",
        |w: &mut WorldApi, name: &str, rate: f64| -> std::result::Result<(), Box<EvalAltResult>> {
            // The schema says `rate >= 0`, and bake writes this field back
            // into a scene file that must revalidate. Rejecting the bad
            // value here makes that a located script error on the step it
            // happened, not `value_out_of_range` on a file nobody hand-wrote.
            // The test is on the *stored* f32: NaN would poison the spawn
            // credit, and a finite f64 too big for f32 (1e300) overflows to
            // an infinity that serializes as JSON `null`.
            let stored = rate as f32;
            if !stored.is_finite() || stored < 0.0 {
                return Err(Box::new(EvalAltResult::ErrorRuntime(
                    format!("particle rate must be a finite number >= 0, got {rate}").into(),
                    Position::NONE,
                )));
            }
            w.with_component::<ParticleEmitter, _>(name, "ParticleEmitter", |e| {
                e.rate = stored;
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

fn name_table(world: &World) -> Rc<HashMap<String, hecs::Entity>> {
    Rc::new(
        world
            .query::<(hecs::Entity, &Name)>()
            .iter()
            .map(|(entity, name)| (name.0.clone(), entity))
            .collect(),
    )
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

        Ok(Some(Self {
            engine,
            scripts,
            names: name_table(world),
            dt: 1.0 / timestep_hz.max(1) as f32,
            state: Rc::new(RefCell::new(HashMap::new())),
            breaks: Rc::new(RefCell::new(Vec::new())),
            explosions: Rc::new(RefCell::new(Vec::new())),
        }))
    }

    /// Rebuild the name table from the world. Call after anything changes
    /// the entity set — a break despawns the parent and spawns fragments,
    /// and scripts address entities by name.
    pub fn sync_names(&mut self, world: &World) {
        self.names = name_table(world);
    }

    /// Drain the breaks scripts queued this step, in call order.
    pub fn take_breaks(&self) -> Vec<String> {
        self.breaks.borrow_mut().drain(..).collect()
    }

    /// Drain the explosions scripts queued this step, in call order.
    pub fn take_explosions(&self) -> Vec<QueuedExplosion> {
        self.explosions.borrow_mut().drain(..).collect()
    }

    /// Run every script's `step` for step index `step`, with `input` as the
    /// held-key set for the duration of the step and `contacts` as the
    /// touching-state left by the previous physics step. The world is moved
    /// into the scripts' reach for the duration and moved back out even on
    /// error, so a failing script never swallows the ECS. Returns the HUD
    /// lines the step pushed — this step's alone; the list starts empty
    /// every step.
    pub fn step(
        &self,
        world: &mut World,
        step: u64,
        input: &InputState,
        contacts: &ContactState,
    ) -> Result<Vec<String>> {
        let api = WorldApi {
            world: Rc::new(RefCell::new(std::mem::take(world))),
            names: Rc::clone(&self.names),
            dt: self.dt,
            input: Rc::new(input.clone()),
            contacts: Rc::new(contacts.clone()),
            state: Rc::clone(&self.state),
            hud: Rc::new(RefCell::new(Vec::new())),
            breaks: Rc::clone(&self.breaks),
            explosions: Rc::clone(&self.explosions),
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

        let hud = match Rc::try_unwrap(api.hud) {
            Ok(cell) => cell.into_inner(),
            Err(shared) => shared.borrow().clone(),
        };

        match failure {
            Some(error) => Err(error),
            None => Ok(hud),
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
            host.step(&mut scene.world, step, &InputState::default(), &ContactState::default()).unwrap();
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
        let error = host.step(&mut scene.world, 0, &InputState::default(), &ContactState::default()).unwrap_err();
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
        let error = host.step(&mut scene.world, 0, &InputState::default(), &ContactState::default()).unwrap_err();
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

        host.step(&mut scene.world, 0, &InputState::default(), &ContactState::default()).unwrap();
        let r = scene.world.get::<&Transform>(entity).unwrap().rotation;
        assert!(r.abs_diff_eq(glam::Vec3::ZERO, 1e-4), "straight ahead is identity: {r}");

        host.step(&mut scene.world, 1, &InputState::default(), &ContactState::default()).unwrap();
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
        host.step(&mut scene.world, 0, &held, &ContactState::default()).unwrap();
        host.step(&mut scene.world, 1, &InputState::default(), &ContactState::default()).unwrap();

        let entity = scene.entity("Mover").unwrap();
        let x = scene.world.get::<&Transform>(entity).unwrap().position.x;
        assert!((x - 1.0).abs() < 1e-6, "only the held step moves: x = {x}");

        let (mut scene, path) = scene_with_script(
            &dir,
            r#"fn step(world, step) { world.key("ArowUp"); }"#,
        );
        let host = ScriptHost::build(&scene.world, &path, 60).unwrap().unwrap();
        let error = host
            .step(&mut scene.world, 0, &InputState::default(), &ContactState::default())
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
    fn forward_is_representation_independent() {
        let dir = temp_dir("fwd");
        let (mut scene, path) = scene_with_script(
            &dir,
            r#"fn step(world, step) {
                let f = world.forward("Mover");
                world.set_position("Mover", f[0], f[1], f[2]);
            }"#,
        );
        let host = ScriptHost::build(&scene.world, &path, 60).unwrap().unwrap();
        let entity = scene.entity("Mover").unwrap();

        // Physics writes yaws past ±90° as the gimbal twin (±180, θ, ±180),
        // where (−180, θ, −180) ≡ plain yaw 180−θ. `forward` must read both
        // representations correctly — this is the reason it exists.
        let yaw150 = glam::Vec3::new(
            -(150f32.to_radians().sin()),
            0.0,
            -(150f32.to_radians().cos()),
        );
        for (rotation, expected) in [
            (glam::Vec3::new(0.0, 90.0, 0.0), glam::Vec3::new(-1.0, 0.0, 0.0)),
            (glam::Vec3::new(0.0, 150.0, 0.0), yaw150),
            (glam::Vec3::new(-180.0, 30.0, -180.0), yaw150), // twin of yaw 150
        ] {
            scene.world.get::<&mut Transform>(entity).unwrap().rotation = rotation;
            host.step(&mut scene.world, 0, &InputState::default(), &ContactState::default()).unwrap();
            let p = scene.world.get::<&Transform>(entity).unwrap().position;
            assert!(
                (p - expected).length() < 1e-4,
                "rotation {rotation} gave forward {p}, expected {expected}"
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn velocity_access_reads_and_writes_the_rigid_body() {
        let dir = temp_dir("vel");
        std::fs::create_dir_all(dir.join("scripts")).unwrap();
        std::fs::write(
            dir.join("scripts/test.rhai"),
            r#"fn step(world, step) {
                let v = world.linear_velocity("Car");
                world.set_linear_velocity("Car", v[0] + 2.0, v[1], v[2]);
                world.set_angular_velocity("Car", 0.0, 45.0, 0.0);
            }"#,
        )
        .unwrap();
        let scene_json = r#"{"name":"s","entities":[
            {"name":"Car","components":[
                {"type":"Transform"},
                {"type":"RigidBody","body":"dynamic","linear_velocity":[1.0,0.0,0.0]},
                {"type":"Collider","shape":"cuboid","half_extents":[0.5,0.5,0.5]},
                {"type":"Script","source":"scripts/test.rhai"}
            ]}
        ]}"#;
        let scene_path = dir.join("scene.json");
        std::fs::write(&scene_path, scene_json).unwrap();
        let mut scene =
            Scene::from_source(scene_json, &scene_path.display().to_string()).unwrap();
        let host = ScriptHost::build(&scene.world, &scene_path, 60).unwrap().unwrap();
        host.step(&mut scene.world, 0, &InputState::default(), &ContactState::default()).unwrap();

        let entity = scene.entity("Car").unwrap();
        let body = *scene.world.get::<&engine_core::components::RigidBody>(entity).unwrap();
        assert_eq!(body.linear_velocity, glam::Vec3::new(3.0, 0.0, 0.0));
        assert_eq!(body.angular_velocity, glam::Vec3::new(0.0, 45.0, 0.0));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn wheel_controls_read_and_write_the_wheel_component() {
        let dir = temp_dir("wheel");
        std::fs::create_dir_all(dir.join("scripts")).unwrap();
        std::fs::write(
            dir.join("scripts/test.rhai"),
            r#"fn step(world, step) {
                world.set_engine_force("WheelBL", 900.0);
                world.set_brake("WheelBL", world.brake("WheelBL") + 2.0);
                world.set_steering("WheelFL", 12.5);
                if world.engine_force("WheelBL") != 900.0 {
                    world.set_steering("WheelFL", -1.0); // readback failed
                }
            }"#,
        )
        .unwrap();
        let scene_json = r#"{"name":"s","entities":[
            {"name":"Car","components":[
                {"type":"Transform"},
                {"type":"RigidBody","body":"dynamic"},
                {"type":"Collider","shape":"cuboid","half_extents":[1.0,0.5,2.0]},
                {"type":"Script","source":"scripts/test.rhai"}
            ]},
            {"name":"WheelBL","components":[
                {"type":"Transform"},
                {"type":"Wheel","vehicle":"Car","offset":[-0.8,0.0,1.2]}
            ]},
            {"name":"WheelFL","components":[
                {"type":"Transform"},
                {"type":"Wheel","vehicle":"Car","offset":[-0.8,0.0,-1.2]}
            ]}
        ]}"#;
        let scene_path = dir.join("scene.json");
        std::fs::write(&scene_path, scene_json).unwrap();
        let mut scene =
            Scene::from_source(scene_json, &scene_path.display().to_string()).unwrap();
        let host = ScriptHost::build(&scene.world, &scene_path, 60).unwrap().unwrap();
        host.step(&mut scene.world, 0, &InputState::default(), &ContactState::default()).unwrap();
        host.step(&mut scene.world, 1, &InputState::default(), &ContactState::default()).unwrap();

        let wheel = |name: &str| {
            let entity = scene.entity(name).unwrap();
            scene
                .world
                .get::<&engine_core::components::Wheel>(entity)
                .unwrap()
                .clone()
        };
        let rear = wheel("WheelBL");
        assert_eq!(rear.engine_force, 900.0);
        assert_eq!(rear.brake, 4.0, "brake accumulated across two steps via readback");
        let front = wheel("WheelFL");
        assert_eq!(front.steering, 12.5, "steering set (and readback saw 900)");

        // A wheel call against an entity with no Wheel is structured.
        let (mut scene, path) = scene_with_script(
            &dir,
            r#"fn step(world, step) { world.set_engine_force("Mover", 1.0); }"#,
        );
        let host = ScriptHost::build(&scene.world, &path, 60).unwrap().unwrap();
        let error = host
            .step(&mut scene.world, 0, &InputState::default(), &ContactState::default())
            .unwrap_err();
        assert_eq!(error.error, "script_runtime_error");
        assert!(error.message.contains("no Wheel"), "{}", error.message);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn velocity_access_without_a_rigid_body_is_a_structured_error() {
        let dir = temp_dir("novel");
        let (mut scene, path) = scene_with_script(
            &dir,
            r#"fn step(world, step) { world.linear_velocity("Mover"); }"#,
        );
        let host = ScriptHost::build(&scene.world, &path, 60).unwrap().unwrap();
        let error = host
            .step(&mut scene.world, 0, &InputState::default(), &ContactState::default())
            .unwrap_err();
        assert_eq!(error.error, "script_runtime_error");
        assert!(error.message.contains("no RigidBody"), "{}", error.message);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn contact_queries_read_the_previous_steps_state() {
        use engine_core::contact::ContactEvent;

        let dir = temp_dir("contacts");
        let (mut scene, path) = scene_with_script(
            &dir,
            r#"fn step(world, step) {
                let y = 0.0;
                if world.touching("Mover").len() > 0 { y += 1.0; }
                if world.contacts_started("Mover").len() > 0 { y += 2.0; }
                let names = world.touching("Mover");
                if names.len() > 0 && names[0] == "Cam" { y += 4.0; }
                world.set_position("Mover", 0.0, y, 0.0);
            }"#,
        );
        let host = ScriptHost::build(&scene.world, &path, 60).unwrap().unwrap();
        let entity = scene.entity("Mover").unwrap();
        let y_of = |scene: &Scene| scene.world.get::<&Transform>(entity).unwrap().position.y;

        let mut contacts = ContactState::default();
        contacts.apply(&[ContactEvent {
            a: "Cam".into(),
            b: "Mover".into(),
            started: true,
        }]);
        host.step(&mut scene.world, 0, &InputState::default(), &contacts).unwrap();
        assert_eq!(y_of(&scene), 7.0, "touching + started + name all visible");

        // Next step: still touching, no longer freshly started.
        contacts.apply(&[]);
        host.step(&mut scene.world, 1, &InputState::default(), &contacts).unwrap();
        assert_eq!(y_of(&scene), 5.0, "started clears, touching persists");

        contacts.apply(&[ContactEvent {
            a: "Cam".into(),
            b: "Mover".into(),
            started: false,
        }]);
        host.step(&mut scene.world, 2, &InputState::default(), &contacts).unwrap();
        assert_eq!(y_of(&scene), 0.0, "an ended contact disappears");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn hud_lines_are_returned_per_step_and_do_not_accumulate() {
        let dir = temp_dir("hud");
        let (mut scene, path) = scene_with_script(
            &dir,
            r#"fn step(world, step) {
                if step == 0 {
                    world.hud("SPEED 42 KM/H");
                    world.hud("LAP 1");
                }
            }"#,
        );
        let host = ScriptHost::build(&scene.world, &path, 60).unwrap().unwrap();
        let hud = host
            .step(&mut scene.world, 0, &InputState::default(), &ContactState::default())
            .unwrap();
        assert_eq!(hud, vec!["SPEED 42 KM/H".to_string(), "LAP 1".to_string()]);
        // The next step pushes nothing, so the HUD is empty — not sticky.
        let hud = host
            .step(&mut scene.world, 1, &InputState::default(), &ContactState::default())
            .unwrap();
        assert!(hud.is_empty(), "{hud:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn contact_queries_on_unknown_entities_are_structured_errors() {
        let dir = temp_dir("contacts-nobody");
        let (mut scene, path) = scene_with_script(
            &dir,
            r#"fn step(world, step) { world.touching("Nobody"); }"#,
        );
        let host = ScriptHost::build(&scene.world, &path, 60).unwrap().unwrap();
        let error = host
            .step(&mut scene.world, 0, &InputState::default(), &ContactState::default())
            .unwrap_err();
        assert_eq!(error.error, "script_runtime_error");
        assert!(error.message.contains("Nobody"), "{}", error.message);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn hud_caps_and_non_ascii_are_structured_errors() {
        let dir = temp_dir("hudcap");
        let (mut scene, path) = scene_with_script(
            &dir,
            r#"fn step(world, step) {
                let i = 0;
                while i < 17 { world.hud("line"); i += 1; }
            }"#,
        );
        let host = ScriptHost::build(&scene.world, &path, 60).unwrap().unwrap();
        let error = host.step(&mut scene.world, 0, &InputState::default(), &ContactState::default()).unwrap_err();
        assert_eq!(error.error, "script_runtime_error");
        assert!(error.message.contains("at most 16 lines"), "{}", error.message);

        let (mut scene, path) = scene_with_script(
            &dir,
            "fn step(world, step) { world.hud(\"caf\u{e9}\"); }",
        );
        let host = ScriptHost::build(&scene.world, &path, 60).unwrap().unwrap();
        let error = host.step(&mut scene.world, 0, &InputState::default(), &ContactState::default()).unwrap_err();
        assert!(error.message.contains("printable ASCII"), "{}", error.message);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn hud_component_accessors_read_and_write_text_and_rect_size() {
        let dir = temp_dir("hudcomp");
        std::fs::create_dir_all(dir.join("scripts")).unwrap();
        std::fs::write(
            dir.join("scripts/test.rhai"),
            r#"fn step(world, step) {
                let old = world.hud_text("Speedo");
                world.set_hud_text("Speedo", old + " KM/H");
                let s = world.hud_rect_size("Bar");
                world.set_hud_rect_size("Bar", s[0] * 2.0, s[1]);
            }"#,
        )
        .unwrap();
        let scene_json = r#"{"name":"s","entities":[
            {"name":"Speedo","components":[
                {"type":"HudText","text":"42"},
                {"type":"Script","source":"scripts/test.rhai"}
            ]},
            {"name":"Bar","components":[
                {"type":"HudRect","size":[50.0,8.0]}
            ]}
        ]}"#;
        let scene_path = dir.join("scene.json");
        std::fs::write(&scene_path, scene_json).unwrap();
        let mut scene =
            Scene::from_source(scene_json, &scene_path.display().to_string()).unwrap();
        let host = ScriptHost::build(&scene.world, &scene_path, 60).unwrap().unwrap();
        host.step(&mut scene.world, 0, &InputState::default(), &ContactState::default()).unwrap();

        let text = scene
            .world
            .get::<&HudText>(scene.entity("Speedo").unwrap())
            .unwrap()
            .text
            .clone();
        assert_eq!(text, "42 KM/H");
        let size = scene
            .world
            .get::<&HudRect>(scene.entity("Bar").unwrap())
            .unwrap()
            .size;
        assert_eq!(size, glam::Vec2::new(100.0, 8.0));

        // Missing components are structured errors, like every accessor.
        let (mut scene, path) = scene_with_script(
            &dir,
            r#"fn step(world, step) { world.hud_text("Mover"); }"#,
        );
        let host = ScriptHost::build(&scene.world, &path, 60).unwrap().unwrap();
        let error = host.step(&mut scene.world, 0, &InputState::default(), &ContactState::default()).unwrap_err();
        assert_eq!(error.error, "script_runtime_error");
        assert!(error.message.contains("no HudText"), "{}", error.message);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn particle_rate_accessor_reads_writes_and_rejects_bad_values() {
        let dir = temp_dir("particlerate");
        std::fs::create_dir_all(dir.join("scripts")).unwrap();
        std::fs::write(
            dir.join("scripts/test.rhai"),
            r#"fn step(world, step) {
                // Gate the effect on gameplay: smoke only while "skidding".
                let idle = world.particle_rate("Puff");
                world.set_particle_rate("Puff", if step >= 2 { idle * 4.0 } else { 0.0 });
            }"#,
        )
        .unwrap();
        let scene_json = r#"{"name":"s","entities":[
            {"name":"Puff","components":[
                {"type":"Transform"},
                {"type":"ParticleEmitter","rate":25.0},
                {"type":"Script","source":"scripts/test.rhai"}
            ]}
        ]}"#;
        let scene_path = dir.join("scene.json");
        std::fs::write(&scene_path, scene_json).unwrap();
        let mut scene = Scene::from_source(scene_json, &scene_path.display().to_string()).unwrap();
        let host = ScriptHost::build(&scene.world, &scene_path, 60).unwrap().unwrap();
        let rate_now = |scene: &Scene| {
            scene
                .world
                .get::<&ParticleEmitter>(scene.entity("Puff").unwrap())
                .unwrap()
                .rate
        };

        host.step(&mut scene.world, 0, &InputState::default(), &ContactState::default()).unwrap();
        assert_eq!(rate_now(&scene), 0.0, "the script must be able to shut emission off");
        host.step(&mut scene.world, 2, &InputState::default(), &ContactState::default()).unwrap();
        // The getter read the *live* 0.0 the previous step wrote, not the
        // file's 25.0 — the component is the single source of truth.
        assert_eq!(rate_now(&scene), 0.0, "the getter must see the live value");

        // A negative rate would bake into a file that fails validation, and
        // NaN would poison the spawn credit: both are located script errors.
        for bad in ["-1.0", "0.0/0.0", "1e300"] {
            let (mut scene, path) = scene_with_emitter(
                &dir,
                &format!("fn step(world, step) {{ world.set_particle_rate(\"Puff\", {bad}); }}"),
            );
            let host = ScriptHost::build(&scene.world, &path, 60).unwrap().unwrap();
            let error = host
                .step(&mut scene.world, 0, &InputState::default(), &ContactState::default())
                .unwrap_err();
            assert_eq!(error.error, "script_runtime_error", "{bad}");
            assert!(error.message.contains("finite number >= 0"), "{bad}: {}", error.message);
        }

        // Missing components are structured errors, like every accessor.
        let (mut scene, path) = scene_with_script(
            &dir,
            r#"fn step(world, step) { world.particle_rate("Mover"); }"#,
        );
        let host = ScriptHost::build(&scene.world, &path, 60).unwrap().unwrap();
        let error = host
            .step(&mut scene.world, 0, &InputState::default(), &ContactState::default())
            .unwrap_err();
        assert!(error.message.contains("no ParticleEmitter"), "{}", error.message);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A one-entity scene whose emitter the given script drives.
    fn scene_with_emitter(dir: &Path, script: &str) -> (Scene, std::path::PathBuf) {
        std::fs::create_dir_all(dir.join("scripts")).unwrap();
        std::fs::write(dir.join("scripts/emitter.rhai"), script).unwrap();
        let scene_json = r#"{"name":"s","entities":[
            {"name":"Puff","components":[
                {"type":"Transform"},
                {"type":"ParticleEmitter","rate":25.0},
                {"type":"Script","source":"scripts/emitter.rhai"}
            ]}
        ]}"#;
        let scene_path = dir.join("emitter_scene.json");
        std::fs::write(&scene_path, scene_json).unwrap();
        let scene = Scene::from_source(scene_json, &scene_path.display().to_string()).unwrap();
        (scene, scene_path)
    }

    #[test]
    fn state_persists_across_steps_and_defaults_when_unset() {
        let dir = temp_dir("state");
        let (mut scene, path) = scene_with_script(
            &dir,
            r#"fn step(world, step) {
                let laps = world.state("laps", 0);
                if step == 5 { world.set_state("laps", laps + 1.0); }
                world.set_position("Mover", laps, 0.0, world.state("missing", -7.5));
            }"#,
        );
        let host = ScriptHost::build(&scene.world, &path, 60).unwrap().unwrap();
        for step in 0..8 {
            host.step(&mut scene.world, step, &InputState::default(), &ContactState::default()).unwrap();
        }
        let entity = scene.entity("Mover").unwrap();
        let p = scene.world.get::<&Transform>(entity).unwrap().position;
        // Step 7 read the value step 5 wrote; the unset key fell back.
        assert_eq!(p.x, 1.0, "state write must persist: {p}");
        assert_eq!(p.z, -7.5, "unset state must yield the default: {p}");
        std::fs::remove_dir_all(&dir).ok();
    }

    fn breakable_scene(dir: &Path, script: &str) -> (Scene, std::path::PathBuf) {
        std::fs::create_dir_all(dir.join("scripts")).unwrap();
        std::fs::write(dir.join("scripts/test.rhai"), script).unwrap();
        let scene_json = r#"{"name":"s","entities":[
            {"name":"Crate","components":[
                {"type":"Transform","position":[0.0,0.5,0.0]},
                {"type":"Mesh","asset":"builtin:cube"},
                {"type":"Collider","shape":"cuboid","half_extents":[0.5,0.5,0.5]},
                {"type":"Breakable","fragments":[{"mesh":"builtin:cube"}]},
                {"type":"Script","source":"scripts/test.rhai"}
            ]},
            {"name":"Solid","components":[{"type":"Transform"}]}
        ]}"#;
        let scene_path = dir.join("scene.json");
        std::fs::write(&scene_path, scene_json).unwrap();
        let scene = Scene::from_source(scene_json, &scene_path.display().to_string()).unwrap();
        (scene, scene_path)
    }

    #[test]
    fn break_entity_queues_and_validates_at_call_time() {
        let dir = temp_dir("break");
        let (mut scene, path) = breakable_scene(
            &dir,
            r#"fn step(world, step) {
                if step == 0 { world.break_entity("Crate"); }
                if step == 1 { world.break_entity("Solid"); }
                if step == 2 { world.break_entity("Ghost"); }
            }"#,
        );
        let host = ScriptHost::build(&scene.world, &path, 60).unwrap().unwrap();

        host.step(&mut scene.world, 0, &InputState::default(), &ContactState::default()).unwrap();
        assert_eq!(host.take_breaks(), vec!["Crate".to_string()]);
        assert!(host.take_breaks().is_empty(), "draining drains");

        let error = host.step(&mut scene.world, 1, &InputState::default(), &ContactState::default()).unwrap_err();
        assert!(error.message.contains("no Breakable"), "{}", error.message);

        let error = host.step(&mut scene.world, 2, &InputState::default(), &ContactState::default()).unwrap_err();
        assert!(error.message.contains("no entity named"), "{}", error.message);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn explode_queues_a_blast_and_rejects_a_bad_radius() {
        let dir = temp_dir("explode");
        let (mut scene, path) = breakable_scene(
            &dir,
            r#"fn step(world, step) {
                if step == 0 { world.explode(1.0, 2.0, 3.0, 5.0, 20.0); }
                if step == 1 { world.explode(0.0, 0.0, 0.0, 0.0, 1.0); }
                if step == 2 { world.explode(0.0, 0.0, 0.0, 1.0, -1.0); }
            }"#,
        );
        let host = ScriptHost::build(&scene.world, &path, 60).unwrap().unwrap();

        host.step(&mut scene.world, 0, &InputState::default(), &ContactState::default()).unwrap();
        assert_eq!(
            host.take_explosions(),
            vec![QueuedExplosion { center: [1.0, 2.0, 3.0], radius: 5.0, impulse: 20.0 }]
        );

        let error = host.step(&mut scene.world, 1, &InputState::default(), &ContactState::default()).unwrap_err();
        assert!(error.message.contains("radius must be positive"), "{}", error.message);

        let error = host.step(&mut scene.world, 2, &InputState::default(), &ContactState::default()).unwrap_err();
        assert!(error.message.contains("cannot be negative"), "{}", error.message);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn sync_names_lets_scripts_reach_spawned_entities() {
        let dir = temp_dir("sync");
        let (mut scene, path) = scene_with_script(
            &dir,
            r#"fn step(world, step) { world.set_position("Fragment", 1.0, 2.0, 3.0); }"#,
        );
        let mut host = ScriptHost::build(&scene.world, &path, 60).unwrap().unwrap();

        // Before the spawn the name is unknown — a runtime error.
        let error = host.step(&mut scene.world, 0, &InputState::default(), &ContactState::default()).unwrap_err();
        assert!(error.message.contains("no entity named"), "{}", error.message);

        let spawned = scene
            .world
            .spawn((Name("Fragment".to_string()), Transform::default()));
        host.sync_names(&scene.world);
        host.step(&mut scene.world, 1, &InputState::default(), &ContactState::default()).unwrap();
        let p = scene.world.get::<&Transform>(spawned).unwrap().position;
        assert_eq!(p, glam::Vec3::new(1.0, 2.0, 3.0));
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
        let error = host.step(&mut scene.world, 0, &InputState::default(), &ContactState::default()).unwrap_err();
        assert_eq!(
            error.error, "script_runtime_error",
            "timestamp() must not exist: {error:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
