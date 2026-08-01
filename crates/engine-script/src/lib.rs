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

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use std::sync::Arc;

use engine_core::components::{
    AmbientLight, AnimationPlayer, Breakable, DirectionalLight, HudImage, HudPanel, HudRect,
    HudText, Mesh, Name, ParticleEmitter, PointLight, RigidBody, Script, Terrain, Transform, Wheel,
};
use engine_core::contact::ContactState;
use engine_core::daylight::{Daylight, DaylightSettings};
use engine_core::input::{self, InputState, Pointer};
use engine_core::scene::EnvironmentSettings;
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

/// A located-nowhere runtime error. Rhai fills the position in from the call
/// site, which is what makes these point at the script line rather than at the
/// engine.
fn runtime(message: String) -> Box<EvalAltResult> {
    Box::new(EvalAltResult::ErrorRuntime(message.into(), Position::NONE))
}

/// Which asset does this entity draw? The first link of the chain both the
/// joint queries (M30) and the clip check (M36) walk, shared so the sentence a
/// script sees when it asks a rig question of an entity that has no mesh stays
/// one sentence rather than two copies of it.
///
/// Only the first link: what the two do with the asset diverges deliberately.
/// `joint_world` looks the rig up straight away, while `check_clip` has to
/// compare the asset against the one the clip names *first*, so that a clip out
/// of the wrong file reports the mismatch rather than "carries no skin".
fn rig_asset(
    world: &hecs::World,
    entity: hecs::Entity,
    name: &str,
) -> std::result::Result<String, Box<EvalAltResult>> {
    world
        .get::<&Mesh>(entity)
        .map(|mesh| mesh.asset.clone())
        .map_err(|_| runtime(format!("entity {name:?} has no Mesh, so it has no rig")))
}

/// Where save slots live, relative to the scene file (M36).
const SAVE_DIR: &str = "saves";

/// How many slots there are. Ten is arbitrary and finite on purpose: the slot
/// is an *index*, not a name, because a script naming its own file is exactly
/// what the sandbox exists to prevent.
const SAVE_SLOTS: i64 = 10;

/// Validate a slot index and turn it into its path.
fn slot_path(dir: &Path, slot: i64) -> std::result::Result<PathBuf, Box<EvalAltResult>> {
    if !(0..SAVE_SLOTS).contains(&slot) {
        return Err(runtime(format!(
            "save slot must be 0..{}, got {slot}",
            SAVE_SLOTS - 1
        )));
    }
    Ok(dir.join(format!("slot{slot}.json")))
}

/// Write the `world.state` map to a slot.
///
/// A `BTreeMap` rather than the `HashMap` it comes from, so keys are sorted
/// and two saves of one state are the same bytes — invariant 1's "git-diffable
/// by construction" applied to a file the engine writes rather than one an
/// agent does.
fn write_slot(
    path: &Path,
    state: &HashMap<String, f64>,
) -> std::result::Result<(), Box<EvalAltResult>> {
    let sorted: BTreeMap<&str, f64> = state.iter().map(|(k, v)| (k.as_str(), *v)).collect();
    let body = serde_json::to_string_pretty(&sorted)
        .map_err(|e| runtime(format!("could not encode save: {e}")))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| runtime(format!("could not create {}: {e}", parent.display())))?;
    }
    std::fs::write(path, body + "\n")
        .map_err(|e| runtime(format!("could not write {}: {e}", path.display())))
}

/// Read a slot back, or `None` when it does not exist.
///
/// A missing slot is `None` and not an error because "is there a save?" is a
/// menu's first question; a slot that exists and does not parse *is* an error,
/// because that is a bug rather than an empty slot.
#[allow(clippy::type_complexity)]
fn read_slot(path: &Path) -> std::result::Result<Option<HashMap<String, f64>>, Box<EvalAltResult>> {
    let body = match std::fs::read_to_string(path) {
        Ok(body) => body,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(runtime(format!("could not read {}: {e}", path.display()))),
    };
    let parsed: BTreeMap<String, f64> = serde_json::from_str(&body)
        .map_err(|e| runtime(format!("{} is not a valid save: {e}", path.display())))?;
    Ok(Some(parsed.into_iter().collect()))
}

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
    /// The scene's day/night block (M21), or `None`. Held as *settings* and
    /// evaluated once per step from the step number, so scripts read the same
    /// clock the renderer does and a replay reads it identically.
    daylight: Option<DaylightSettings>,
    /// The rig behind every skinned mesh the scene references (M30), resolved
    /// once at build. Rigs are shared and immutable, so holding them costs a
    /// pointer each and spares `world.joint_position` a file read per call.
    rigs: Rc<HashMap<String, Arc<engine_core::skeleton::Rig>>>,
    /// Set by `world.quit` (M36), drained by the caller via
    /// [`ScriptHost::quit_requested`] — the `take_breaks` pattern, for the same
    /// reason: what quitting *means* differs between the viewer (close the
    /// window) and a headless run (stop stepping and report it), and neither
    /// belongs in the script host.
    quit: Rc<Cell<bool>>,
    /// The scene's `environment` block, seeded at build and writable from
    /// scripts since M36. The caller owns the `Scene`, so it reads this back
    /// after each step via [`ScriptHost::environment`] and assigns
    /// `scene.environment`; `Scene::resolved_at` is untouched.
    environment: Rc<RefCell<EnvironmentSettings>>,
    /// Where `world.save`/`world.load` put their slots: `<scene dir>/saves`.
    /// Next to the scene for M10's reason — everything in this engine resolves
    /// relative to the scene file, and a save in `/tmp` is a save nobody can
    /// commit.
    save_dir: Rc<PathBuf>,
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
    /// Where the pointer points during this step (M28): the cursor, the frame
    /// it was measured in, and the ray through it. Resolved by the caller
    /// from the camera it is about to render through.
    pointer: Pointer,
    /// What the pointer is doing to the overlay this step (M31): hover, an
    /// in-flight press, and the click it turned into. Resolved by the caller
    /// against the layout for the frame it is about to render, *before*
    /// scripts run — so `world.clicked` is a question about this step, not a
    /// callback the engine fires into a script.
    interaction: Rc<engine_core::ui::Interaction>,
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
    /// The day evaluated at this step, or `None` when the scene has no
    /// `daylight` block — in which case asking for the time is a runtime
    /// error rather than a made-up noon.
    daylight: Option<Daylight>,
    /// Scene time at this step, in seconds: `step * dt`, the same reproducible
    /// clock the renderer poses a rig on. Evaluated once per step, so two
    /// `world.joint_position` calls in one step cannot disagree.
    time: f32,
    /// The scene's rigs (M30), by `Mesh.asset`.
    rigs: Rc<HashMap<String, Arc<engine_core::skeleton::Rig>>>,
    /// `world.quit` (M36): a request the caller drains after the step.
    quit: Rc<Cell<bool>>,
    /// `world.set_shadows` and friends (M36): the live `environment` block.
    environment: Rc<RefCell<EnvironmentSettings>>,
    /// `world.save`/`world.load` (M36): where the slots live.
    save_dir: Rc<PathBuf>,
}

impl WorldApi {
    /// The day at this step, or a located runtime error.
    ///
    /// Asking a scene with no `daylight` block what time it is gets an error
    /// rather than a plausible noon: a script that wants the time in a scene
    /// that has no clock is a bug, and inventing one hides it until the
    /// lamps come on at the wrong moment.
    fn day(&self) -> std::result::Result<Daylight, Box<EvalAltResult>> {
        self.daylight.ok_or_else(|| {
            Box::new(EvalAltResult::ErrorRuntime(
                "this scene has no \"daylight\" block, so it has no time of day; \
                 add one to the scene file to use world.time_of_day()"
                    .into(),
                Position::NONE,
            ))
        })
    }

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

    /// The `offset` of whichever HUD component the entity carries (M28).
    ///
    /// `offset` means the same thing on a `HudText` and a `HudRect` — pixels
    /// inward from the anchor — so, like `with_light`, the API takes the name
    /// and does not make the author remember which kind it is.
    fn with_hud_offset<T>(
        &mut self,
        name: &str,
        f: impl FnOnce(&mut glam::Vec2) -> T,
    ) -> std::result::Result<T, Box<EvalAltResult>> {
        let entity = self.entity(name)?;
        let world = self.world.borrow_mut();
        if let Ok(mut text) = world.get::<&mut HudText>(entity) {
            return Ok(f(&mut text.offset));
        }
        if let Ok(mut rect) = world.get::<&mut HudRect>(entity) {
            return Ok(f(&mut rect.offset));
        }
        if let Ok(mut panel) = world.get::<&mut HudPanel>(entity) {
            return Ok(f(&mut panel.offset));
        }
        if let Ok(mut image) = world.get::<&mut HudImage>(entity) {
            return Ok(f(&mut image.offset));
        }
        Err(Box::new(EvalAltResult::ErrorRuntime(
            format!("entity {name:?} has no HudText, HudRect, HudPanel or HudImage").into(),
            Position::NONE,
        )))
    }

    /// Reach whichever HUD element the entity carries (M31).
    ///
    /// `with_hud_offset`'s generalization: `visible`, `color`, `opacity` and
    /// `size` mean the same thing on all four components, so the API takes a
    /// name and does not make the author remember which kind it is. The order
    /// is arbitrary but fixed — an entity carrying two HUD elements is
    /// `duplicate_component` only if they are the same type, and a panel with
    /// a text on it is a legal, if unusual, thing to have written.
    fn with_hud_element<T>(
        &mut self,
        name: &str,
        call: &str,
        panel: impl FnOnce(&mut HudPanel) -> T,
        rect: impl FnOnce(&mut HudRect) -> T,
        image: impl FnOnce(&mut HudImage) -> T,
        text: impl FnOnce(&mut HudText) -> T,
    ) -> std::result::Result<T, Box<EvalAltResult>> {
        let entity = self.entity(name)?;
        let world = self.world.borrow_mut();
        if let Ok(mut c) = world.get::<&mut HudPanel>(entity) {
            return Ok(panel(&mut c));
        }
        if let Ok(mut c) = world.get::<&mut HudRect>(entity) {
            return Ok(rect(&mut c));
        }
        if let Ok(mut c) = world.get::<&mut HudImage>(entity) {
            return Ok(image(&mut c));
        }
        if let Ok(mut c) = world.get::<&mut HudText>(entity) {
            return Ok(text(&mut c));
        }
        Err(Box::new(EvalAltResult::ErrorRuntime(
            format!(
                "world.{call} needs a HUD element on entity {name:?}, which has no \
                 HudPanel, HudRect, HudImage or HudText"
            )
            .into(),
            Position::NONE,
        )))
    }

    /// Light access (M17), across all three light components.
    ///
    /// `color` and `intensity` mean the same thing on a `PointLight`, a
    /// `DirectionalLight`, and an `AmbientLight`, so the script API takes a
    /// light by name and does not make the author remember which kind it is —
    /// `world.set_light_intensity("Campfire", x)` and
    /// `world.set_light_intensity("Sun", x)` are the same call. Point lights
    /// come first because they are the ones a flicker drives.
    fn with_light<T>(
        &mut self,
        name: &str,
        point: impl FnOnce(&mut PointLight) -> T,
        directional: impl FnOnce(&mut DirectionalLight) -> T,
        ambient: impl FnOnce(&mut AmbientLight) -> T,
    ) -> std::result::Result<T, Box<EvalAltResult>> {
        let entity = self.entity(name)?;
        let world = self.world.borrow_mut();
        if let Ok(mut light) = world.get::<&mut PointLight>(entity) {
            return Ok(point(&mut light));
        }
        if let Ok(mut light) = world.get::<&mut DirectionalLight>(entity) {
            return Ok(directional(&mut light));
        }
        if let Ok(mut light) = world.get::<&mut AmbientLight>(entity) {
            return Ok(ambient(&mut light));
        }
        Err(Box::new(EvalAltResult::ErrorRuntime(
            format!("entity {name:?} has no PointLight, DirectionalLight, or AmbientLight").into(),
            Position::NONE,
        )))
    }

    /// The height of a terrain patch at a world XZ position, in world metres
    /// (M22).
    ///
    /// Needs the patch's `Transform` as well as its `Terrain`, so it reads the
    /// world directly rather than going through `with_component`: the answer
    /// includes the entity's own Y and `Transform.scale.y`, which makes it a
    /// coordinate a script can assign straight to a position. That is the whole
    /// point — an animal walking a parametric loop over rolling ground needs to
    /// ask where the ground *is* every step, and before this there was no way to
    /// ask.
    fn terrain_height_at(
        &mut self,
        name: &str,
        x: f32,
        z: f32,
    ) -> std::result::Result<f32, Box<EvalAltResult>> {
        let entity = self.entity(name)?;
        let world = self.world.borrow();
        let terrain = world.get::<&Terrain>(entity).map_err(|_| {
            Box::new(EvalAltResult::ErrorRuntime(
                format!("entity {name:?} has no Terrain").into(),
                Position::NONE,
            ))
        })?;
        let transform = world
            .get::<&Transform>(entity)
            .map(|t| *t)
            .unwrap_or_default();
        Ok(engine_core::terrain::world_height_at(
            &terrain, &transform, x, z,
        ))
    }

    /// Where one joint of a skinned entity is right now, as a **world**
    /// matrix (M30).
    ///
    /// Read-only, and deliberately so: there is no setter, for M21's reason —
    /// a script-settable joint is hidden state (invariant 2), and the pose has
    /// to stay a function of (files, time). What a script wants is not to move
    /// a bone but to put something *where a bone is*, and that is an ordinary
    /// `set_position` on an ordinary entity, visible in the trace and baked by
    /// the change-based rule like anything else.
    ///
    /// This composes the same `skeleton::joint_globals` the renderer's palette
    /// comes from, at the same scene time, with the entity's own `Transform`
    /// read *live* — so a chase-camera script that has already moved the
    /// character this step gets the hand where it now is, not where it was.
    fn joint_world(
        &mut self,
        name: &str,
        joint: &str,
    ) -> std::result::Result<glam::Mat4, Box<EvalAltResult>> {
        use engine_core::skeleton::{self, ClipRef};

        let entity = self.entity(name)?;
        let world = self.world.borrow();

        let asset = rig_asset(&world, entity, name)?;
        let rig = self.rigs.get(&asset).ok_or_else(|| {
            runtime(format!(
                "entity {name:?} references {asset:?}, which carries no skin"
            ))
        })?;
        let skin = rig.skin.as_ref().ok_or_else(|| {
            runtime(format!(
                "entity {name:?} references {asset:?}, which carries no skin"
            ))
        })?;

        let index = skin.joint_named(joint).ok_or_else(|| {
            // Named joints are a closed set per rig, so a typo can be caught
            // exactly — the `world.key` treatment.
            let mut message = format!("{asset:?} has no joint named {joint:?}");
            if let Some(near) = engine_core::error::closest_match(
                joint,
                skin.joints.iter().map(|j| j.name.as_str()),
            ) {
                message.push_str(&format!(" (did you mean {near:?}?)"));
            }
            runtime(message)
        })?;

        let player = world
            .get::<&AnimationPlayer>(entity)
            .ok()
            .map(|p| (*p).clone());
        let clip = player.as_ref().and_then(|p| match ClipRef::parse(&p.clip) {
            ClipRef::Skeletal { clip, .. } => rig.clip_named(clip),
            ClipRef::Property(_) => None,
        });
        let local = match (&player, clip) {
            (Some(player), Some(clip)) => {
                engine_core::animation::local_time(player, skeleton::duration(clip), self.time)
            }
            _ => 0.0,
        };

        let transform = world
            .get::<&Transform>(entity)
            .map(|t| *t)
            .unwrap_or_default();
        // Through M32's shared seam, so a prop hung off a *planted* foot lands
        // where the render draws that foot rather than where the clip alone
        // put it.
        let globals = engine_core::locomotion::posed_globals(&world, entity, skin, clip, local);
        Ok(transform.matrix() * globals[index])
    }

    /// Does `clip` name something this entity can actually play (M36)?
    ///
    /// Checked at the call rather than at the next render, because
    /// `AnimationPlayer.clip` bakes: a bad value has to be a located script
    /// error and not a scene file that fails its own validation. The rules are
    /// M30's, reached from the other side — the fragment form is required, the
    /// asset it names has to be the one on this entity's `Mesh`, and the clip
    /// has to exist in it.
    fn check_clip(
        &mut self,
        name: &str,
        clip: &str,
    ) -> std::result::Result<(), Box<EvalAltResult>> {
        use engine_core::skeleton::ClipRef;

        let entity = self.entity(name)?;
        let world = self.world.borrow();

        // A property clip has no rig to check against — M9 owns those, and its
        // own validation does. Only the skeletal form is checkable here.
        let ClipRef::Skeletal { asset, clip: want } = ClipRef::parse(clip) else {
            return Ok(());
        };

        let mesh = rig_asset(&world, entity, name)?;
        if mesh != asset {
            // M30's `skeletal_player_mesh_mismatch`, as a runtime error: a clip
            // out of a file the entity does not draw would silently never play.
            return Err(runtime(format!(
                "entity {name:?} draws {mesh:?}, so it cannot play a clip from {asset:?}"
            )));
        }
        let rig = self
            .rigs
            .get(&mesh)
            .ok_or_else(|| runtime(format!("{mesh:?} carries no skin")))?;
        if rig.clip_named(want).is_some() {
            return Ok(());
        }
        let mut message = format!("{mesh:?} has no clip named {want:?}");
        if let Some(near) = engine_core::error::closest_match(want, rig.clip_names()) {
            message.push_str(&format!(" (did you mean {near:?}?)"));
        }
        Err(runtime(message))
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

    // Daylight (M21): two read-only getters, and that is the whole surface.
    //
    // There is deliberately no setter. A script-settable clock would be
    // hidden state — the scene would stop being reconstructible from its
    // text, which is invariant 2. "Sleep until dawn" is a real want and is
    // named in the design doc's "what is not here" rather than smuggled in.
    engine.register_fn(
        "time_of_day",
        |w: &mut WorldApi| -> std::result::Result<f64, Box<EvalAltResult>> {
            w.day().map(|day| f64::from(day.hours))
        },
    );
    // Derivable from `time_of_day` only by reimplementing the sun's arc in
    // Rhai, and "turn the lamps on when the sun is down" is *the* use case —
    // which is why this is a second function rather than a documented formula.
    engine.register_fn(
        "sun_altitude",
        |w: &mut WorldApi| -> std::result::Result<f64, Box<EvalAltResult>> {
            w.day().map(|day| f64::from(day.sun_altitude))
        },
    );

    engine.register_fn(
        "key",
        |w: &mut WorldApi, name: &str| -> std::result::Result<bool, Box<EvalAltResult>> {
            if !input::is_known_key(name) {
                // Deterministic failure over a silently-never-pressed key.
                let mut message = format!("{name:?} names no known key");
                if input::is_known_button(name) {
                    // The two namespaces share the held set but not the
                    // query, so name the call that would have worked rather
                    // than the nearest key to a mouse button.
                    message.push_str(&format!(
                        " (it is a mouse button — did you mean world.mouse({name:?})?)"
                    ));
                } else if let Some(suggestion) = input::closest_key(name) {
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

    // The mouse (M28). Buttons ride the same held set the keys do, but the
    // namespace splits here: `key` takes key names and `mouse` takes button
    // names, each rejecting the other kind with a suggestion. A script that
    // asks `world.key("MouseLeft")` is told what it did wrong instead of
    // reading `false` forever.
    engine.register_fn(
        "mouse",
        |w: &mut WorldApi, name: &str| -> std::result::Result<bool, Box<EvalAltResult>> {
            if !input::is_known_button(name) {
                let mut message = format!("{name:?} names no known mouse button");
                if input::is_known_key(name) {
                    message.push_str(&format!(
                        " (it is a key — did you mean world.key({name:?})?)"
                    ));
                } else if let Some(suggestion) = input::closest_input(name) {
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
    engine.register_fn("cursor_x", |w: &mut WorldApi| f64::from(w.pointer.cursor.x));
    engine.register_fn("cursor_y", |w: &mut WorldApi| f64::from(w.pointer.cursor.y));
    // The frame in pixels, so a script can put the cursor in HUD coordinates:
    // `cursor_x() * viewport_width()` is the pixel a menu button is
    // hit-tested against, with no flip to get wrong.
    engine.register_fn("viewport_width", |w: &mut WorldApi| {
        w.pointer.viewport[0] as i64
    });
    engine.register_fn("viewport_height", |w: &mut WorldApi| {
        w.pointer.viewport[1] as i64
    });
    // Where the cursor's ray meets the horizontal plane at `y` — the call a
    // top-down game aims with, and the reason the engine resolves the ray
    // rather than exposing a projection matrix to Rhai.
    engine.register_fn(
        "cursor_ground",
        |w: &mut WorldApi, y: f64| -> std::result::Result<rhai::Array, Box<EvalAltResult>> {
            // No camera, no direction. The M21 precedent: a script asking
            // where the pointer is in a scene with no view is a bug, and
            // inventing an answer hides it until something aims at nothing.
            w.pointer.ground(y as f32).map(vec3_array).ok_or_else(|| {
                Box::new(EvalAltResult::ErrorRuntime(
                    "this scene has no camera to point through, so the cursor has no \
                     direction; give one entity a Camera component (or pass --camera)"
                        .into(),
                    Position::NONE,
                ))
            })
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

    // The same two questions at proxy resolution (M33). The answers are
    // **addresses** — `Walker/Head` — which are engine-produced and never
    // accepted back: a proxy is not an entity, so `world.set_position` would
    // rightly reject one. With no `SkinnedCollider` in the scene these return
    // exactly what the two above do, which is what makes them safe to reach
    // for in a script that may or may not be shooting at a character.
    engine.register_fn(
        "touching_parts",
        |w: &mut WorldApi, name: &str| -> std::result::Result<rhai::Array, Box<EvalAltResult>> {
            w.entity(engine_core::contact::owner_of(name))?;
            Ok(w.contacts
                .touching_parts(name)
                .into_iter()
                .map(Dynamic::from)
                .collect())
        },
    );
    engine.register_fn(
        "contacts_started_parts",
        |w: &mut WorldApi, name: &str| -> std::result::Result<rhai::Array, Box<EvalAltResult>> {
            w.entity(engine_core::contact::owner_of(name))?;
            Ok(w.contacts
                .started_parts_with(name)
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
    engine.register_fn(
        "state",
        |w: &mut WorldApi, key: &str, default: f64| -> f64 {
            w.state.borrow().get(key).copied().unwrap_or(default)
        },
    );
    engine.register_fn(
        "state",
        |w: &mut WorldApi, key: &str, default: i64| -> f64 {
            w.state.borrow().get(key).copied().unwrap_or(default as f64)
        },
    );
    engine.register_fn("set_state", |w: &mut WorldApi, key: &str, value: f64| {
        w.state.borrow_mut().insert(key.to_string(), value);
    });
    engine.register_fn("set_state", |w: &mut WorldApi, key: &str, value: i64| {
        w.state.borrow_mut().insert(key.to_string(), value as f64);
    });

    // Saves (M36). The whole `world.state` map, written as sorted JSON next to
    // the scene. What a save *is* comes out of M32's rule — ask what should
    // survive, and the answer says whether something is state or data: the bake
    // already writes where every body ended up, and this writes the memory the
    // bake deliberately drops. A load therefore restores the campaign and not
    // the arena, since the engine cannot spawn an entity and a broken drone
    // cannot come back. That is the game's problem to state, not the engine's:
    // here it is simply a map in and a map out.
    engine.register_fn(
        "save",
        |w: &mut WorldApi, slot: i64| -> std::result::Result<bool, Box<EvalAltResult>> {
            let path = slot_path(&w.save_dir, slot)?;
            write_slot(&path, &w.state.borrow())?;
            Ok(true)
        },
    );
    engine.register_fn(
        "load",
        |w: &mut WorldApi, slot: i64| -> std::result::Result<bool, Box<EvalAltResult>> {
            let path = slot_path(&w.save_dir, slot)?;
            let Some(loaded) = read_slot(&path)? else {
                return Ok(false);
            };
            // Replaced wholesale rather than merged: a merge leaves keys from
            // the run being abandoned, and what that produces is a bug three
            // levels later rather than a wrong number now.
            *w.state.borrow_mut() = loaded;
            Ok(true)
        },
    );
    engine.register_fn(
        "has_save",
        |w: &mut WorldApi, slot: i64| -> std::result::Result<bool, Box<EvalAltResult>> {
            let path = slot_path(&w.save_dir, slot)?;
            Ok(path.exists())
        },
    );

    // Quitting (M36). Queued like a break, for the same reason: what it means
    // is the caller's business. The viewer closes its window; a headless run
    // stops stepping and reports the step it stopped on.
    engine.register_fn("quit", |w: &mut WorldApi| {
        w.quit.set(true);
    });

    // The `environment` block, writable since M36 — the settings screen the
    // arena shooter wanted, and the one place where a script reaches something
    // that was read off the file at load and never again.
    //
    // Every setter is a no-op that writes the value it already holds unless a
    // script calls it, which is what keeps a scene touching none of them
    // byte-identical: the caller assigns back a value equal to the one it had.
    engine.register_fn("shadows", |w: &mut WorldApi| w.environment.borrow().shadows);
    engine.register_fn("set_shadows", |w: &mut WorldApi, on: bool| {
        w.environment.borrow_mut().shadows = on;
    });
    engine.register_fn("sky", |w: &mut WorldApi| w.environment.borrow().sky);
    engine.register_fn("set_sky", |w: &mut WorldApi, on: bool| {
        w.environment.borrow_mut().sky = on;
    });
    engine.register_fn("fog", |w: &mut WorldApi| {
        f64::from(w.environment.borrow().fog_density)
    });
    engine.register_fn(
        "set_fog",
        |w: &mut WorldApi, density: f64| -> std::result::Result<(), Box<EvalAltResult>> {
            // Negated so NaN fails, like every other numeric guard in this
            // file — `density >= 0.0` is false for NaN and would let one
            // through into a uniform.
            #[allow(clippy::neg_cmp_op_on_partial_ord)]
            if !(density >= 0.0) || density > f64::from(f32::MAX) {
                return Err(runtime(format!(
                    "fog density must be zero or positive, got {density}"
                )));
            }
            w.environment.borrow_mut().fog_density = density as f32;
            Ok(())
        },
    );
    engine.register_fn("samples", |w: &mut WorldApi| {
        w.environment.borrow().samples as i64
    });
    engine.register_fn(
        "set_samples",
        |w: &mut WorldApi, samples: i64| -> std::result::Result<(), Box<EvalAltResult>> {
            // Validated at the call, M13's rule for `set_particle_rate`: a bad
            // value is a located script error rather than a baked file that
            // fails its own validation. The vocabulary is the schema's — 1 or
            // 4, and nothing silently rounds.
            if samples != 1 && samples != 4 {
                return Err(runtime(format!("samples must be 1 or 4, got {samples}")));
            }
            w.environment.borrow_mut().samples = samples as u32;
            Ok(())
        },
    );

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
            // `!(radius > 0.0)` and not `radius <= 0.0`: a NaN radius has to
            // be a located script error, not an explosion of unknown size.
            #[allow(clippy::neg_cmp_op_on_partial_ord)]
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
        |w: &mut WorldApi, name: &str, force: f64| -> std::result::Result<(), Box<EvalAltResult>> {
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
        |w: &mut WorldApi, name: &str, force: f64| -> std::result::Result<(), Box<EvalAltResult>> {
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
                let (rx, ry, rz) = glam::Quat::from_mat3(&glam::Mat3::from_cols(right, up, back))
                    .to_euler(glam::EulerRot::XYZ);
                t.rotation = glam::Vec3::new(rx.to_degrees(), ry.to_degrees(), rz.to_degrees());
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
        |w: &mut WorldApi, name: &str, text: &str| -> std::result::Result<(), Box<EvalAltResult>> {
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
    // Moving a HUD element (M28), on either kind. A HUD that can be resized
    // and re-worded but not *moved* cannot draw a crosshair, which is the
    // minimum feedback a pointing device needs. Offsets are pixels inward
    // from the element's own anchor, exactly as the component documents them.
    engine.register_fn(
        "hud_offset",
        |w: &mut WorldApi, name: &str| -> std::result::Result<rhai::Array, Box<EvalAltResult>> {
            w.with_hud_offset(name, |offset| {
                vec![
                    Dynamic::from_float(f64::from(offset.x)),
                    Dynamic::from_float(f64::from(offset.y)),
                ]
            })
        },
    );
    engine.register_fn(
        "set_hud_offset",
        |w: &mut WorldApi,
         name: &str,
         x: f64,
         y: f64|
         -> std::result::Result<(), Box<EvalAltResult>> {
            let offset = glam::Vec2::new(x as f32, y as f32);
            if !offset.is_finite() {
                return Err(Box::new(EvalAltResult::ErrorRuntime(
                    format!("HUD offset must be finite, got [{x}, {y}]").into(),
                    Position::NONE,
                )));
            }
            w.with_hud_offset(name, |target| *target = offset)
        },
    );

    // ── Interaction (M31) ──────────────────────────────────────────────
    //
    // Polled, never dispatched: the engine does not call into a script. A
    // scene has many `Script` entities, and an `on_click` field would have to
    // say which one owns the handler — a second addressing scheme for
    // something already addressed by entity name — plus a dispatch-order rule
    // and mid-step reentrancy. `world.key` set this shape in M11 for the same
    // reason: a button that runs code is a *binding*, and bindings are game
    // logic, and game logic lives in scripts.
    //
    // All three take an entity name and answer for the element on it; a name
    // that is not an element is simply never hovered, which is deliberate —
    // asking about the wrong entity is a logic bug in the script, not a
    // resource error, and the polled shape has nowhere to report it that is
    // not "false".
    engine.register_fn("hovered", |w: &mut WorldApi, name: &str| {
        w.interaction.hovered(name)
    });
    engine.register_fn("pressed", |w: &mut WorldApi, name: &str| {
        w.interaction.pressed(name)
    });
    engine.register_fn("clicked", |w: &mut WorldApi, name: &str| {
        w.interaction.clicked(name)
    });

    // ── HUD element fields (M31) ───────────────────────────────────────
    //
    // `visible` is the one that matters: it is how a menu opens and closes,
    // one boolean on one panel, hiding the whole subtree. All of these bake
    // change-based like every other script-driven field, so a run that opened
    // a menu bakes a scene with the menu open.
    engine.register_fn(
        "hud_visible",
        |w: &mut WorldApi, name: &str| -> std::result::Result<bool, Box<EvalAltResult>> {
            w.with_hud_element(
                name,
                "hud_visible",
                |p| p.visible,
                |r| r.visible,
                |i| i.visible,
                |t| t.visible,
            )
        },
    );
    engine.register_fn(
        "set_hud_visible",
        |w: &mut WorldApi,
         name: &str,
         visible: bool|
         -> std::result::Result<(), Box<EvalAltResult>> {
            w.with_hud_element(
                name,
                "set_hud_visible",
                |p| p.visible = visible,
                |r| r.visible = visible,
                |i| i.visible = visible,
                |t| t.visible = visible,
            )
        },
    );

    // Colour reads and writes the field that *means* colour on each kind — a
    // `HudImage` has no `color`, and its `tint` is the multiplier that plays
    // the same role, so one call reaches all four.
    engine.register_fn(
        "hud_color",
        |w: &mut WorldApi, name: &str| -> std::result::Result<rhai::Array, Box<EvalAltResult>> {
            let color = w.with_hud_element(
                name,
                "hud_color",
                |p| p.color,
                |r| r.color,
                |i| i.tint,
                |t| t.color,
            )?;
            Ok(vec3_array(color))
        },
    );
    engine.register_fn(
        "set_hud_color",
        |w: &mut WorldApi,
         name: &str,
         r: f64,
         g: f64,
         b: f64|
         -> std::result::Result<(), Box<EvalAltResult>> {
            // Clamped rather than rejected, following M17's split for lights:
            // a colour outside [0, 1] has an obvious nearest legal answer, so
            // it is clamped, where a NaN opacity does not and is an error.
            let color = glam::Vec3::new(r as f32, g as f32, b as f32);
            let color = if color.is_finite() {
                color.clamp(glam::Vec3::ZERO, glam::Vec3::ONE)
            } else {
                return Err(Box::new(EvalAltResult::ErrorRuntime(
                    format!("HUD color must be finite, got [{r}, {g}, {b}]").into(),
                    Position::NONE,
                )));
            };
            w.with_hud_element(
                name,
                "set_hud_color",
                |p| p.color = color,
                |x| x.color = color,
                |i| i.tint = color,
                |t| t.color = color,
            )
        },
    );

    engine.register_fn(
        "hud_opacity",
        |w: &mut WorldApi, name: &str| -> std::result::Result<f64, Box<EvalAltResult>> {
            let opacity = w.with_hud_element(
                name,
                "hud_opacity",
                |p| p.opacity,
                |r| r.opacity,
                |i| i.opacity,
                // A `HudText` is always opaque — the 8×8 font is bit-exact in
                // baselines precisely because a glyph pixel is fully on or
                // fully off — so it reads as 1 and cannot be dimmed.
                |_| 1.0,
            )?;
            Ok(f64::from(opacity))
        },
    );
    engine.register_fn(
        "set_hud_opacity",
        |w: &mut WorldApi,
         name: &str,
         opacity: f64|
         -> std::result::Result<(), Box<EvalAltResult>> {
            let value = opacity as f32;
            if !value.is_finite() {
                return Err(Box::new(EvalAltResult::ErrorRuntime(
                    format!("HUD opacity must be finite, got {opacity}").into(),
                    Position::NONE,
                )));
            }
            let value = value.clamp(0.0, 1.0);
            w.with_hud_element(
                name,
                "set_hud_opacity",
                |p| p.opacity = value,
                |r| r.opacity = value,
                |i| i.opacity = value,
                |_| {},
            )
        },
    );

    // `hud_size` generalizes M12's `set_hud_rect_size` to panels and images.
    // The older name stays, because three committed scripts call it.
    //
    // A panel's absent `width`/`height` is hug sizing, so reading one back
    // reports 0 rather than the laid-out box: layout is a function of the
    // viewport, and this call has no viewport. `engine ui-layout` is where the
    // resolved rectangle lives, and a script that needs it should be told to
    // ask there rather than given a number that is right at one resolution.
    engine.register_fn(
        "hud_size",
        |w: &mut WorldApi, name: &str| -> std::result::Result<rhai::Array, Box<EvalAltResult>> {
            let size = w.with_hud_element(
                name,
                "hud_size",
                |p| glam::Vec2::new(p.width.unwrap_or(0.0), p.height.unwrap_or(0.0)),
                |r| r.size,
                |i| i.size,
                |t| glam::Vec2::new(0.0, t.size),
            )?;
            Ok(vec![
                Dynamic::from_float(f64::from(size.x)),
                Dynamic::from_float(f64::from(size.y)),
            ])
        },
    );
    engine.register_fn(
        "set_hud_size",
        |w: &mut WorldApi,
         name: &str,
         width: f64,
         height: f64|
         -> std::result::Result<(), Box<EvalAltResult>> {
            let size = glam::Vec2::new(width as f32, height as f32);
            if !size.is_finite() || size.x < 0.0 || size.y < 0.0 {
                return Err(Box::new(EvalAltResult::ErrorRuntime(
                    format!("HUD size must be finite and non-negative, got [{width}, {height}]")
                        .into(),
                    Position::NONE,
                )));
            }
            w.with_hud_element(
                name,
                "set_hud_size",
                |p| {
                    p.width = Some(size.x);
                    p.height = Some(size.y);
                },
                |r| r.size = size,
                |i| i.size = size,
                // Setting a text's "size" is its glyph height, the one number
                // it has; the width follows from the string.
                |t| t.size = size.y,
            )
        },
    );

    // Terrain (M22): read-only, and the only terrain field the API exposes.
    // A patch's shape is a function of its authored fields, so there is nothing
    // meaningful to write here — what a script needs is the answer to "where is
    // the ground under this point", which is what keeps feet, wheels and props
    // on a surface that is no longer flat anywhere.
    engine.register_fn(
        "terrain_height",
        |w: &mut WorldApi,
         name: &str,
         x: f64,
         z: f64|
         -> std::result::Result<f64, Box<EvalAltResult>> {
            w.terrain_height_at(name, x as f32, z as f32).map(f64::from)
        },
    );

    // Joints (M30): read-only, and the only two skeletal calls the API
    // exposes. There is deliberately no setter — a script-settable joint is
    // hidden state (invariant 2) and the pose must stay a function of (files,
    // time). Attaching a prop to a hand is then one line of ordinary script:
    //
    //     let p = world.joint_position("Robot", "Hand.R");
    //     world.set_position("Torch", p[0], p[1], p[2]);
    //
    // which bakes change-based like every other script-driven transform, needs
    // no new component, and shows up in the trace.
    engine.register_fn(
        "joint_position",
        |w: &mut WorldApi,
         name: &str,
         joint: &str|
         -> std::result::Result<rhai::Array, Box<EvalAltResult>> {
            let world = w.joint_world(name, joint)?;
            let p = world.w_axis.truncate();
            Ok(vec![
                Dynamic::from(f64::from(p.x)),
                Dynamic::from(f64::from(p.y)),
                Dynamic::from(f64::from(p.z)),
            ])
        },
    );
    // Position *and* rotation, for aiming as well as placing — the rotation as
    // XYZ Euler degrees, the file's convention, so it can be written straight
    // back through `set_rotation`. Six numbers in one array rather than two
    // calls, because two calls would pose the rig twice.
    engine.register_fn(
        "joint_transform",
        |w: &mut WorldApi,
         name: &str,
         joint: &str|
         -> std::result::Result<rhai::Array, Box<EvalAltResult>> {
            let world = w.joint_world(name, joint)?;
            let (_, rotation, translation) = world.to_scale_rotation_translation();
            let (x, y, z) = rotation.to_euler(glam::EulerRot::XYZ);
            Ok([
                translation.x,
                translation.y,
                translation.z,
                x.to_degrees(),
                y.to_degrees(),
                z.to_degrees(),
            ]
            .iter()
            .map(|v| Dynamic::from(f64::from(*v)))
            .collect())
        },
    );

    // Locomotion (M32): where the clip has got to, and how much ground a
    // cycle of it covers. Unlike the joints above these *are* settable, and
    // the distinction is where the number lives — a joint is derived from the
    // clip and would be hidden state, while `phase` and `stride` are ordinary
    // component fields that the file carries and the bake splices. A game with
    // its own idea of locomotion (a phase that freezes mid-air, a gait that
    // changes with terrain) drives it through these rather than through a
    // second system in the engine.
    engine.register_fn(
        "animation_phase",
        |w: &mut WorldApi, name: &str| -> std::result::Result<f64, Box<EvalAltResult>> {
            w.with_component::<AnimationPlayer, _>(name, "AnimationPlayer", |p| f64::from(p.phase))
        },
    );
    engine.register_fn(
        "set_animation_phase",
        |w: &mut WorldApi, name: &str, phase: f64| -> std::result::Result<(), Box<EvalAltResult>> {
            // Validated at the call, like `set_particle_rate` and for the same
            // reason: this field bakes, so a bad value has to be a located
            // script error rather than a scene file that no longer validates.
            let stored = phase as f32;
            if !stored.is_finite() || stored < 0.0 {
                return Err(Box::new(EvalAltResult::ErrorRuntime(
                    format!("animation phase must be a finite number >= 0, got {phase}").into(),
                    Position::NONE,
                )));
            }
            w.with_component::<AnimationPlayer, _>(name, "AnimationPlayer", |p| {
                p.phase = stored;
            })
        },
    );
    engine.register_fn(
        "animation_stride",
        |w: &mut WorldApi, name: &str| -> std::result::Result<f64, Box<EvalAltResult>> {
            w.with_component::<AnimationPlayer, _>(name, "AnimationPlayer", |p| f64::from(p.stride))
        },
    );
    engine.register_fn(
        "set_animation_stride",
        |w: &mut WorldApi,
         name: &str,
         stride: f64|
         -> std::result::Result<(), Box<EvalAltResult>> {
            let stored = stride as f32;
            if !stored.is_finite() || stored < 0.0 {
                return Err(Box::new(EvalAltResult::ErrorRuntime(
                    format!("animation stride must be a finite number >= 0, got {stride}").into(),
                    Position::NONE,
                )));
            }
            w.with_component::<AnimationPlayer, _>(name, "AnimationPlayer", |p| {
                p.stride = stored;
            })
        },
    );
    // Switching clips (M36). A **hard cut**, and that is the design rather than
    // a limitation: M9 §8 rejected blending, M30 restated the rejection, and
    // M32 restated it again — *a gait change here is a different clip*. This is
    // the call that makes that sentence actionable, and a game with an idle and
    // a run is the thing that wanted it.
    //
    // The clip is validated against the rig the host already resolved, so a
    // typo is a located runtime error with `did_you_mean` — the `world.key`
    // treatment, which M30 gave joint names for the same reason.
    engine.register_fn(
        "set_animation_clip",
        |w: &mut WorldApi, name: &str, clip: &str| -> std::result::Result<(), Box<EvalAltResult>> {
            // The unchanged case first, and it is the overwhelmingly common
            // one: a script asks for the gait it wants every step and switches
            // a handful of times in a run. Answering it costs one component
            // read — the validation walk and the two allocations below belong
            // to the step that actually cuts, not to the 3,000 that do not.
            if w.with_component::<AnimationPlayer, _>(name, "AnimationPlayer", |p| p.clip == clip)?
            {
                return Ok(());
            }
            w.check_clip(name, clip)?;
            let stored = clip.to_string();
            w.with_component::<AnimationPlayer, _>(name, "AnimationPlayer", |p| {
                p.clip = stored;
                // Reset, never carried over: a phase is a fraction of a *cycle*
                // and two clips do not share one, so carrying it is M32's
                // `speed` trap in another place — the pose teleports on the
                // step the gait changes.
                p.phase = 0.0;
            })
        },
    );
    engine.register_fn(
        "animation_clip",
        |w: &mut WorldApi, name: &str| -> std::result::Result<String, Box<EvalAltResult>> {
            w.with_component::<AnimationPlayer, _>(name, "AnimationPlayer", |p| p.clip.clone())
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
            w.with_component::<ParticleEmitter, _>(name, "ParticleEmitter", |e| f64::from(e.rate))
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

    // Lights (M17): the piece that makes a scripted effect light its
    // surroundings instead of only glowing. A campfire's flicker is one signal,
    // and it should drive the flame's emission rate, the ember core's size, and
    // the light the fire casts — before this, the third was impossible and the
    // showcase tour said so in its own design doc.
    //
    // Both fields are validated at the call, like `set_particle_rate` and for
    // the same reason: intensity and color are baked change-based, so a bad
    // value must be a located script error rather than a scene file that no
    // longer validates.
    engine.register_fn(
        "light_intensity",
        |w: &mut WorldApi, name: &str| -> std::result::Result<f64, Box<EvalAltResult>> {
            w.with_light(
                name,
                |l| f64::from(l.intensity),
                |l| f64::from(l.intensity),
                |l| f64::from(l.intensity),
            )
        },
    );
    engine.register_fn(
        "set_light_intensity",
        |w: &mut WorldApi,
         name: &str,
         intensity: f64|
         -> std::result::Result<(), Box<EvalAltResult>> {
            let stored = intensity as f32;
            if !stored.is_finite() || stored < 0.0 {
                return Err(Box::new(EvalAltResult::ErrorRuntime(
                    format!("light intensity must be a finite number >= 0, got {intensity}").into(),
                    Position::NONE,
                )));
            }
            w.with_light(
                name,
                |l| l.intensity = stored,
                |l| l.intensity = stored,
                |l| l.intensity = stored,
            )
        },
    );
    engine.register_fn(
        "light_color",
        |w: &mut WorldApi, name: &str| -> std::result::Result<rhai::Array, Box<EvalAltResult>> {
            w.with_light(
                name,
                |l| vec3_array(l.color),
                |l| vec3_array(l.color),
                |l| vec3_array(l.color),
            )
        },
    );
    engine.register_fn(
        "set_light_color",
        |w: &mut WorldApi,
         name: &str,
         r: f64,
         g: f64,
         b: f64|
         -> std::result::Result<(), Box<EvalAltResult>> {
            // Light colors are `[0, 1]` in the schema on all three components,
            // so unlike intensity this clamps rather than erroring: a flicker
            // that computes 1.02 has not made a mistake worth halting a run
            // over, and the alternative is every author writing the same
            // min/max around every write.
            let color = glam::Vec3::new(r as f32, g as f32, b as f32);
            if !color.is_finite() {
                return Err(Box::new(EvalAltResult::ErrorRuntime(
                    format!("light color must be finite, got [{r}, {g}, {b}]").into(),
                    Position::NONE,
                )));
            }
            let color = color.clamp(glam::Vec3::ZERO, glam::Vec3::ONE);
            w.with_light(
                name,
                |l| l.color = color,
                |l| l.color = color,
                |l| l.color = color,
            )
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
        daylight: Option<DaylightSettings>,
        environment: EnvironmentSettings,
        rigs: &dyn engine_core::skeleton::RigSource,
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

        // The rig behind each distinct `Mesh.asset` in the scene, resolved
        // once. Only skinned files are kept: `world.joint_position` is the
        // only caller, and holding an empty `Rig` for every cube would make
        // "does this entity have a rig" a lookup that always succeeds.
        let mut skins: HashMap<String, Arc<engine_core::skeleton::Rig>> = HashMap::new();
        for (_, mesh) in world.query::<(hecs::Entity, &Mesh)>().iter() {
            if skins.contains_key(&mesh.asset) {
                continue;
            }
            let rig = rigs.load_rig(&mesh.asset)?;
            if rig.skin.is_some() {
                skins.insert(mesh.asset.clone(), rig);
            }
        }

        Ok(Some(Self {
            engine,
            scripts,
            names: name_table(world),
            dt: 1.0 / timestep_hz.max(1) as f32,
            state: Rc::new(RefCell::new(HashMap::new())),
            breaks: Rc::new(RefCell::new(Vec::new())),
            explosions: Rc::new(RefCell::new(Vec::new())),
            daylight,
            rigs: Rc::new(skins),
            quit: Rc::new(Cell::new(false)),
            environment: Rc::new(RefCell::new(environment)),
            save_dir: Rc::new(base_dir.join(SAVE_DIR)),
        }))
    }

    /// Did a script call `world.quit` (M36)?
    ///
    /// A read, not a drain: quitting is terminal, so a caller that asks twice
    /// in one frame must get the same answer both times. What it *does* about
    /// it is the caller's business — the viewer closes the window, a headless
    /// run stops stepping and says so on the report.
    pub fn quit_requested(&self) -> bool {
        self.quit.get()
    }

    /// The `environment` block as scripts have left it (M36).
    ///
    /// Equal to what was passed to [`ScriptHost::build`] unless a script called
    /// one of the setters, which is what keeps a scene that touches none of
    /// them byte-identical to the pre-M36 engine.
    pub fn environment(&self) -> EnvironmentSettings {
        *self.environment.borrow()
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
    /// held-key set for the duration of the step, `pointer` as where the
    /// mouse pointed during it, and `contacts` as the touching-state left by
    /// the previous physics step. The world is moved
    /// into the scripts' reach for the duration and moved back out even on
    /// error, so a failing script never swallows the ECS. Returns the HUD
    /// lines the step pushed — this step's alone; the list starts empty
    /// every step.
    pub fn step(
        &self,
        world: &mut World,
        step: u64,
        input: &InputState,
        pointer: &Pointer,
        interaction: &engine_core::ui::Interaction,
        contacts: &ContactState,
    ) -> Result<Vec<String>> {
        // The same reproducible clock the renderer uses: whole fixed steps
        // converted to seconds. Evaluated once per step rather than per call,
        // so two `world.time_of_day()` calls in one step cannot disagree.
        let daylight = self
            .daylight
            .as_ref()
            .map(|settings| settings.evaluate(step as f32 * self.dt));

        let api = WorldApi {
            world: Rc::new(RefCell::new(std::mem::take(world))),
            names: Rc::clone(&self.names),
            dt: self.dt,
            daylight,
            time: step as f32 * self.dt,
            rigs: Rc::clone(&self.rigs),
            input: Rc::new(input.clone()),
            pointer: *pointer,
            interaction: Rc::new(interaction.clone()),
            contacts: Rc::new(contacts.clone()),
            state: Rc::clone(&self.state),
            hud: Rc::new(RefCell::new(Vec::new())),
            breaks: Rc::clone(&self.breaks),
            explosions: Rc::clone(&self.explosions),
            quit: Rc::clone(&self.quit),
            environment: Rc::clone(&self.environment),
            save_dir: Rc::clone(&self.save_dir),
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
                                format!("script {display} defines no `fn step(world, step)`"),
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

    /// The default host every test but one wants: 60 Hz, no daylight, a
    /// default `environment`, builtin assets. The two `unwrap`s are the
    /// fixture's own assertion — a scene with a `Script` on it builds a host,
    /// and a test whose script does not compile should fail here rather than
    /// silently step nothing. The exception is the test that seeds an
    /// *authored* `environment`, which spells `ScriptHost::build` out because
    /// the seed is the thing it is asserting on.
    fn host_for(scene: &Scene, path: &Path) -> ScriptHost {
        ScriptHost::build(
            &scene.world,
            path,
            60,
            None,
            Default::default(),
            &engine_core::mesh::BuiltinAssets,
        )
        .unwrap()
        .unwrap()
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
        let host = host_for(&scene, &path);
        for step in 0..150 {
            host.step(
                &mut scene.world,
                step,
                &InputState::default(),
                &Pointer::default(),
                &engine_core::ui::Interaction::default(),
                &ContactState::default(),
            )
            .unwrap();
        }

        let entity = scene.entity("Mover").unwrap();
        let y = scene.world.get::<&Transform>(entity).unwrap().position.y;
        assert!(
            (y - 2.25).abs() < 1e-4,
            "elevator should stop at 2.25, is at {y}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_runtime_error_is_structured_and_restores_the_world() {
        let dir = temp_dir("boom");
        let (mut scene, path) = scene_with_script(
            &dir,
            r#"fn step(world, step) { world.position("Nobody"); }"#,
        );
        let host = host_for(&scene, &path);
        let error = host
            .step(
                &mut scene.world,
                0,
                &InputState::default(),
                &Pointer::default(),
                &engine_core::ui::Interaction::default(),
                &ContactState::default(),
            )
            .unwrap_err();
        assert_eq!(error.error, "script_runtime_error");
        assert!(error.message.contains("Nobody"), "{}", error.message);
        assert_eq!(error.context().unwrap().entity.as_deref(), Some("Mover"));

        // The world survived the failure.
        assert!(scene.entity("Mover").is_some());
        assert!(scene
            .world
            .get::<&Transform>(scene.entity("Mover").unwrap())
            .is_ok());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_infinite_loop_hits_the_operation_budget() {
        let dir = temp_dir("spin");
        let (mut scene, path) = scene_with_script(
            &dir,
            r#"fn step(world, step) { let x = 0; loop { x += 1; } }"#,
        );
        let host = host_for(&scene, &path);
        let error = host
            .step(
                &mut scene.world,
                0,
                &InputState::default(),
                &Pointer::default(),
                &engine_core::ui::Interaction::default(),
                &ContactState::default(),
            )
            .unwrap_err();
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
        let errors =
            validate_scene_scripts(scene_json, &dir.join("scene.json").display().to_string());
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].error, "script_parse_error");
        assert!(errors[0].context().unwrap().line.is_some());

        std::fs::write(dir.join("scripts/test.rhai"), "fn stpe(world, step) {}").unwrap();
        let errors =
            validate_scene_scripts(scene_json, &dir.join("scene.json").display().to_string());
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
        let host = host_for(&scene, &path);
        let entity = scene.entity("Mover").unwrap();

        host.step(
            &mut scene.world,
            0,
            &InputState::default(),
            &Pointer::default(),
            &engine_core::ui::Interaction::default(),
            &ContactState::default(),
        )
        .unwrap();
        let r = scene.world.get::<&Transform>(entity).unwrap().rotation;
        assert!(
            r.abs_diff_eq(glam::Vec3::ZERO, 1e-4),
            "straight ahead is identity: {r}"
        );

        host.step(
            &mut scene.world,
            1,
            &InputState::default(),
            &Pointer::default(),
            &engine_core::ui::Interaction::default(),
            &ContactState::default(),
        )
        .unwrap();
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
        assert!(
            forward.abs_diff_eq(glam::Vec3::X, 1e-4),
            "forward is {forward}"
        );
        let up = rotation * glam::Vec3::Y;
        assert!(up.abs_diff_eq(glam::Vec3::Y, 1e-4), "up is {up}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// M28: the mouse reaches scripts as a button predicate, a cursor, and a
    /// point on the ground — and the two namespaces stay apart.
    #[test]
    fn scripts_read_the_mouse_and_aim_at_the_ground_under_the_cursor() {
        let dir = temp_dir("mouse");
        let (mut scene, path) = scene_with_script(
            &dir,
            r#"fn step(world, step) {
                if world.mouse("MouseLeft") {
                    let g = world.cursor_ground(0.0);
                    world.set_position("Mover", g[0], g[1], g[2]);
                }
                world.set_state("cx", world.cursor_x());
                world.set_state("vw", world.viewport_width());
            }"#,
        );
        let host = host_for(&scene, &path);
        let entity = scene.entity("Mover").unwrap();

        let mut held = InputState::default();
        held.set_cursor(glam::Vec2::new(0.5, 0.5));
        // 10 m up, looking straight down: the centre of the frame is the
        // point directly below, whatever the fov.
        let camera = engine_core::components::Camera::default();
        let model = glam::Mat4::from_rotation_translation(
            glam::Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2),
            glam::Vec3::new(2.0, 10.0, -3.0),
        );
        let viewport = engine_core::input::Viewport::new(800, 400, None);
        let pointer = Pointer::resolve(&held, &viewport, Some((camera, model)));

        // Nothing held: the cursor is read, but nothing is aimed.
        let before = scene.world.get::<&Transform>(entity).unwrap().position;
        host.step(
            &mut scene.world,
            0,
            &held,
            &pointer,
            &engine_core::ui::Interaction::default(),
            &ContactState::default(),
        )
        .unwrap();
        let resting = scene.world.get::<&Transform>(entity).unwrap().position;
        assert_eq!(resting, before);

        held.press("MouseLeft");
        let pointer = Pointer::resolve(&held, &viewport, Some((camera, model)));
        host.step(
            &mut scene.world,
            1,
            &held,
            &pointer,
            &engine_core::ui::Interaction::default(),
            &ContactState::default(),
        )
        .unwrap();
        let aimed = scene.world.get::<&Transform>(entity).unwrap().position;
        assert!(
            (aimed - glam::Vec3::new(2.0, 0.0, -3.0)).length() < 1e-3,
            "the ground under the centre of the frame is under the camera: {aimed}"
        );

        // A button asked for as a key — and a key asked for as a button —
        // are located errors naming the call that would have worked.
        for (source, wrong) in [
            (
                r#"fn step(world, step) { world.key("MouseLeft"); }"#,
                "MouseLeft",
            ),
            (
                r#"fn step(world, step) { world.mouse("MouseLef"); }"#,
                "MouseLef",
            ),
        ] {
            let (mut scene, path) = scene_with_script(&dir, source);
            let host = host_for(&scene, &path);
            let error = host
                .step(
                    &mut scene.world,
                    0,
                    &InputState::default(),
                    &Pointer::default(),
                    &engine_core::ui::Interaction::default(),
                    &ContactState::default(),
                )
                .unwrap_err();
            assert_eq!(error.error, "script_runtime_error");
            assert!(
                error.message.contains(wrong) && error.message.contains("did you mean"),
                "{}",
                error.message
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A scene with no camera has no direction to point in, and says so —
    /// the M21 precedent for `world.time_of_day()` without a `daylight`
    /// block.
    #[test]
    fn asking_where_the_cursor_points_with_no_camera_is_an_error() {
        let dir = temp_dir("nocamera");
        let (mut scene, path) = scene_with_script(
            &dir,
            r#"fn step(world, step) { world.cursor_ground(0.0); }"#,
        );
        let host = host_for(&scene, &path);
        let error = host
            .step(
                &mut scene.world,
                0,
                &InputState::default(),
                &Pointer::default(),
                &engine_core::ui::Interaction::default(),
                &ContactState::default(),
            )
            .unwrap_err();
        assert_eq!(error.error, "script_runtime_error");
        assert!(error.message.contains("no camera"), "{}", error.message);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A HUD that can be resized and re-worded but not moved cannot draw a
    /// crosshair (M28).
    #[test]
    fn scripts_move_hud_elements_of_either_kind() {
        let dir = temp_dir("hudoffset");
        std::fs::create_dir_all(dir.join("scripts")).unwrap();
        std::fs::write(
            dir.join("scripts/test.rhai"),
            r#"fn step(world, step) {
                world.set_hud_offset("Cross", world.cursor_x() * 200.0, world.cursor_y() * 100.0);
                let o = world.hud_offset("Cross");
                world.set_hud_offset("Label", o[0], o[1] + 8.0);
            }"#,
        )
        .unwrap();
        let scene_json = r#"{"name":"s","entities":[
            {"name":"Cross","components":[
                {"type":"HudRect","size":[8.0,8.0]},
                {"type":"Script","source":"scripts/test.rhai"}
            ]},
            {"name":"Label","components":[{"type":"HudText","text":"x"}]}
        ]}"#;
        let scene_path = dir.join("scene.json");
        std::fs::write(&scene_path, scene_json).unwrap();
        let mut scene = Scene::from_source(scene_json, &scene_path.display().to_string()).unwrap();
        let host = host_for(&scene, &scene_path);

        let mut held = InputState::default();
        held.set_cursor(glam::Vec2::new(0.25, 0.5));
        let pointer = Pointer::resolve(&held, &engine_core::input::Viewport::DEFAULT, None);
        host.step(
            &mut scene.world,
            0,
            &held,
            &pointer,
            &engine_core::ui::Interaction::default(),
            &ContactState::default(),
        )
        .unwrap();

        let rect = scene
            .world
            .get::<&HudRect>(scene.entity("Cross").unwrap())
            .unwrap()
            .offset;
        assert_eq!(rect, glam::Vec2::new(50.0, 50.0));
        let text = scene
            .world
            .get::<&HudText>(scene.entity("Label").unwrap())
            .unwrap()
            .offset;
        assert_eq!(text, glam::Vec2::new(50.0, 58.0));
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
        let host = host_for(&scene, &path);

        let mut held = InputState::default();
        held.press("ArrowUp");
        host.step(
            &mut scene.world,
            0,
            &held,
            &Pointer::default(),
            &engine_core::ui::Interaction::default(),
            &ContactState::default(),
        )
        .unwrap();
        host.step(
            &mut scene.world,
            1,
            &InputState::default(),
            &Pointer::default(),
            &engine_core::ui::Interaction::default(),
            &ContactState::default(),
        )
        .unwrap();

        let entity = scene.entity("Mover").unwrap();
        let x = scene.world.get::<&Transform>(entity).unwrap().position.x;
        assert!((x - 1.0).abs() < 1e-6, "only the held step moves: x = {x}");

        let (mut scene, path) =
            scene_with_script(&dir, r#"fn step(world, step) { world.key("ArowUp"); }"#);
        let host = host_for(&scene, &path);
        let error = host
            .step(
                &mut scene.world,
                0,
                &InputState::default(),
                &Pointer::default(),
                &engine_core::ui::Interaction::default(),
                &ContactState::default(),
            )
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
        let host = host_for(&scene, &path);
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
            (
                glam::Vec3::new(0.0, 90.0, 0.0),
                glam::Vec3::new(-1.0, 0.0, 0.0),
            ),
            (glam::Vec3::new(0.0, 150.0, 0.0), yaw150),
            (glam::Vec3::new(-180.0, 30.0, -180.0), yaw150), // twin of yaw 150
        ] {
            scene.world.get::<&mut Transform>(entity).unwrap().rotation = rotation;
            host.step(
                &mut scene.world,
                0,
                &InputState::default(),
                &Pointer::default(),
                &engine_core::ui::Interaction::default(),
                &ContactState::default(),
            )
            .unwrap();
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
        let mut scene = Scene::from_source(scene_json, &scene_path.display().to_string()).unwrap();
        let host = host_for(&scene, &scene_path);
        host.step(
            &mut scene.world,
            0,
            &InputState::default(),
            &Pointer::default(),
            &engine_core::ui::Interaction::default(),
            &ContactState::default(),
        )
        .unwrap();

        let entity = scene.entity("Car").unwrap();
        let body = *scene
            .world
            .get::<&engine_core::components::RigidBody>(entity)
            .unwrap();
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
        let mut scene = Scene::from_source(scene_json, &scene_path.display().to_string()).unwrap();
        let host = host_for(&scene, &scene_path);
        host.step(
            &mut scene.world,
            0,
            &InputState::default(),
            &Pointer::default(),
            &engine_core::ui::Interaction::default(),
            &ContactState::default(),
        )
        .unwrap();
        host.step(
            &mut scene.world,
            1,
            &InputState::default(),
            &Pointer::default(),
            &engine_core::ui::Interaction::default(),
            &ContactState::default(),
        )
        .unwrap();

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
        assert_eq!(
            rear.brake, 4.0,
            "brake accumulated across two steps via readback"
        );
        let front = wheel("WheelFL");
        assert_eq!(front.steering, 12.5, "steering set (and readback saw 900)");

        // A wheel call against an entity with no Wheel is structured.
        let (mut scene, path) = scene_with_script(
            &dir,
            r#"fn step(world, step) { world.set_engine_force("Mover", 1.0); }"#,
        );
        let host = host_for(&scene, &path);
        let error = host
            .step(
                &mut scene.world,
                0,
                &InputState::default(),
                &Pointer::default(),
                &engine_core::ui::Interaction::default(),
                &ContactState::default(),
            )
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
        let host = host_for(&scene, &path);
        let error = host
            .step(
                &mut scene.world,
                0,
                &InputState::default(),
                &Pointer::default(),
                &engine_core::ui::Interaction::default(),
                &ContactState::default(),
            )
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
        let host = host_for(&scene, &path);
        let entity = scene.entity("Mover").unwrap();
        let y_of = |scene: &Scene| scene.world.get::<&Transform>(entity).unwrap().position.y;

        let mut contacts = ContactState::default();
        contacts.apply(&[ContactEvent::new("Cam", "Mover", true)]);
        host.step(
            &mut scene.world,
            0,
            &InputState::default(),
            &Pointer::default(),
            &engine_core::ui::Interaction::default(),
            &contacts,
        )
        .unwrap();
        assert_eq!(y_of(&scene), 7.0, "touching + started + name all visible");

        // Next step: still touching, no longer freshly started.
        contacts.apply(&[]);
        host.step(
            &mut scene.world,
            1,
            &InputState::default(),
            &Pointer::default(),
            &engine_core::ui::Interaction::default(),
            &contacts,
        )
        .unwrap();
        assert_eq!(y_of(&scene), 5.0, "started clears, touching persists");

        contacts.apply(&[ContactEvent::new("Cam", "Mover", false)]);
        host.step(
            &mut scene.world,
            2,
            &InputState::default(),
            &Pointer::default(),
            &engine_core::ui::Interaction::default(),
            &contacts,
        )
        .unwrap();
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
        let host = host_for(&scene, &path);
        let hud = host
            .step(
                &mut scene.world,
                0,
                &InputState::default(),
                &Pointer::default(),
                &engine_core::ui::Interaction::default(),
                &ContactState::default(),
            )
            .unwrap();
        assert_eq!(hud, vec!["SPEED 42 KM/H".to_string(), "LAP 1".to_string()]);
        // The next step pushes nothing, so the HUD is empty — not sticky.
        let hud = host
            .step(
                &mut scene.world,
                1,
                &InputState::default(),
                &Pointer::default(),
                &engine_core::ui::Interaction::default(),
                &ContactState::default(),
            )
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
        let host = host_for(&scene, &path);
        let error = host
            .step(
                &mut scene.world,
                0,
                &InputState::default(),
                &Pointer::default(),
                &engine_core::ui::Interaction::default(),
                &ContactState::default(),
            )
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
        let host = host_for(&scene, &path);
        let error = host
            .step(
                &mut scene.world,
                0,
                &InputState::default(),
                &Pointer::default(),
                &engine_core::ui::Interaction::default(),
                &ContactState::default(),
            )
            .unwrap_err();
        assert_eq!(error.error, "script_runtime_error");
        assert!(
            error.message.contains("at most 16 lines"),
            "{}",
            error.message
        );

        let (mut scene, path) =
            scene_with_script(&dir, "fn step(world, step) { world.hud(\"caf\u{e9}\"); }");
        let host = host_for(&scene, &path);
        let error = host
            .step(
                &mut scene.world,
                0,
                &InputState::default(),
                &Pointer::default(),
                &engine_core::ui::Interaction::default(),
                &ContactState::default(),
            )
            .unwrap_err();
        assert!(
            error.message.contains("printable ASCII"),
            "{}",
            error.message
        );
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
        let mut scene = Scene::from_source(scene_json, &scene_path.display().to_string()).unwrap();
        let host = host_for(&scene, &scene_path);
        host.step(
            &mut scene.world,
            0,
            &InputState::default(),
            &Pointer::default(),
            &engine_core::ui::Interaction::default(),
            &ContactState::default(),
        )
        .unwrap();

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

        // M31's generalization reaches the same two through one call, and a
        // panel and an image besides.
        let dir = temp_dir("hudelem");
        std::fs::create_dir_all(dir.join("scripts")).unwrap();
        std::fs::write(
            dir.join("scripts/test.rhai"),
            r#"fn step(world, step) {
                world.set_hud_visible("Menu", false);
                world.set_hud_color("Menu", 0.1, 0.2, 0.3);
                world.set_hud_opacity("Menu", 0.75);
                world.set_hud_size("Bar", 10.0, 4.0);
                // Clamped, not rejected: an out-of-range colour has an
                // obvious nearest legal answer (M17's split for lights).
                world.set_hud_color("Bar", 5.0, -1.0, 0.5);
            }"#,
        )
        .unwrap();
        let scene_json = r#"{"name":"s","entities":[
            {"name":"Menu","components":[
                {"type":"HudPanel","layout":"column"},
                {"type":"Script","source":"scripts/test.rhai"}
            ]},
            {"name":"Bar","components":[
                {"type":"HudRect","size":[50.0,8.0],"parent":"Menu"}
            ]}
        ]}"#;
        let scene_path = dir.join("scene.json");
        std::fs::write(&scene_path, scene_json).unwrap();
        let mut scene = Scene::from_source(scene_json, &scene_path.display().to_string()).unwrap();
        let host = host_for(&scene, &scene_path);
        host.step(
            &mut scene.world,
            0,
            &InputState::default(),
            &Pointer::default(),
            &engine_core::ui::Interaction::default(),
            &ContactState::default(),
        )
        .unwrap();

        let menu = scene.entity("Menu").unwrap();
        let panel = scene
            .world
            .get::<&engine_core::components::HudPanel>(menu)
            .unwrap();
        assert!(!panel.visible, "one boolean closes a menu");
        assert_eq!(panel.color, glam::Vec3::new(0.1, 0.2, 0.3));
        assert_eq!(panel.opacity, 0.75);
        drop(panel);

        let bar = scene
            .world
            .get::<&HudRect>(scene.entity("Bar").unwrap())
            .unwrap();
        assert_eq!(bar.size, glam::Vec2::new(10.0, 4.0));
        assert_eq!(bar.color, glam::Vec3::new(1.0, 0.0, 0.5), "clamped");

        // Missing components are structured errors, like every accessor.
        let (mut scene, path) =
            scene_with_script(&dir, r#"fn step(world, step) { world.hud_text("Mover"); }"#);
        let host = host_for(&scene, &path);
        let error = host
            .step(
                &mut scene.world,
                0,
                &InputState::default(),
                &Pointer::default(),
                &engine_core::ui::Interaction::default(),
                &ContactState::default(),
            )
            .unwrap_err();
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
        let host = host_for(&scene, &scene_path);
        let rate_now = |scene: &Scene| {
            scene
                .world
                .get::<&ParticleEmitter>(scene.entity("Puff").unwrap())
                .unwrap()
                .rate
        };

        host.step(
            &mut scene.world,
            0,
            &InputState::default(),
            &Pointer::default(),
            &engine_core::ui::Interaction::default(),
            &ContactState::default(),
        )
        .unwrap();
        assert_eq!(
            rate_now(&scene),
            0.0,
            "the script must be able to shut emission off"
        );
        host.step(
            &mut scene.world,
            2,
            &InputState::default(),
            &Pointer::default(),
            &engine_core::ui::Interaction::default(),
            &ContactState::default(),
        )
        .unwrap();
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
            let host = host_for(&scene, &path);
            let error = host
                .step(
                    &mut scene.world,
                    0,
                    &InputState::default(),
                    &Pointer::default(),
                    &engine_core::ui::Interaction::default(),
                    &ContactState::default(),
                )
                .unwrap_err();
            assert_eq!(error.error, "script_runtime_error", "{bad}");
            assert!(
                error.message.contains("finite number >= 0"),
                "{bad}: {}",
                error.message
            );
        }

        // Missing components are structured errors, like every accessor.
        let (mut scene, path) = scene_with_script(
            &dir,
            r#"fn step(world, step) { world.particle_rate("Mover"); }"#,
        );
        let host = host_for(&scene, &path);
        let error = host
            .step(
                &mut scene.world,
                0,
                &InputState::default(),
                &Pointer::default(),
                &engine_core::ui::Interaction::default(),
                &ContactState::default(),
            )
            .unwrap_err();
        assert!(
            error.message.contains("no ParticleEmitter"),
            "{}",
            error.message
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn light_accessors_reach_every_kind_of_light() {
        // One pair of accessors for all three light components, because
        // `intensity` and `color` mean the same thing on each: a script author
        // should not have to remember which kind a name refers to.
        let dir = temp_dir("lights");
        std::fs::create_dir_all(dir.join("scripts")).unwrap();
        std::fs::write(
            dir.join("scripts/test.rhai"),
            r#"fn step(world, step) {
                // Read the authored value, then write a multiple of it — proving
                // both directions on each component.
                world.set_light_intensity("Fire", world.light_intensity("Fire") * 2.0);
                world.set_light_color("Fire", 1.0, 0.5, 0.25);
                world.set_light_intensity("Sun", 0.25);
                world.set_light_intensity("Fill", world.light_intensity("Fill") * 3.0);
                // Clamped, not an error: a flicker that overshoots 1.0 has not
                // made a mistake worth halting the run for.
                world.set_light_color("Fill", 1.5, -0.5, 0.5);
            }"#,
        )
        .unwrap();
        let scene_json = r#"{"name":"s","entities":[
            {"name":"Fire","components":[
                {"type":"Transform","position":[0.0,1.0,0.0]},
                {"type":"PointLight","intensity":2.0,"range":5.0},
                {"type":"Script","source":"scripts/test.rhai"}
            ]},
            {"name":"Sun","components":[{"type":"DirectionalLight","intensity":1.0}]},
            {"name":"Fill","components":[{"type":"AmbientLight","intensity":0.1}]}
        ]}"#;
        let scene_path = dir.join("scene.json");
        std::fs::write(&scene_path, scene_json).unwrap();
        let mut scene = Scene::from_source(scene_json, &scene_path.display().to_string()).unwrap();
        let host = host_for(&scene, &scene_path);
        host.step(
            &mut scene.world,
            0,
            &InputState::default(),
            &Pointer::default(),
            &engine_core::ui::Interaction::default(),
            &ContactState::default(),
        )
        .unwrap();

        let fire = scene
            .world
            .get::<&PointLight>(scene.entity("Fire").unwrap())
            .unwrap()
            .clone();
        assert_eq!(fire.intensity, 4.0);
        assert_eq!(fire.color, glam::Vec3::new(1.0, 0.5, 0.25));
        assert_eq!(
            scene
                .world
                .get::<&DirectionalLight>(scene.entity("Sun").unwrap())
                .unwrap()
                .intensity,
            0.25
        );
        let fill = scene
            .world
            .get::<&AmbientLight>(scene.entity("Fill").unwrap())
            .unwrap()
            .clone();
        assert_eq!(fill.intensity, 0.3);
        assert_eq!(
            fill.color,
            glam::Vec3::new(1.0, 0.0, 0.5),
            "out-of-range light colors clamp into the schema's [0, 1]"
        );

        // A negative or non-finite intensity would bake into a file that fails
        // validation, so it is a located script error — same rule as
        // `set_particle_rate`.
        for bad in ["-1.0", "0.0/0.0", "1e300"] {
            let script =
                format!("fn step(world, step) {{ world.set_light_intensity(\"Fire\", {bad}); }}");
            std::fs::write(dir.join("scripts/test.rhai"), &script).unwrap();
            let mut scene =
                Scene::from_source(scene_json, &scene_path.display().to_string()).unwrap();
            let host = host_for(&scene, &scene_path);
            let error = host
                .step(
                    &mut scene.world,
                    0,
                    &InputState::default(),
                    &Pointer::default(),
                    &engine_core::ui::Interaction::default(),
                    &ContactState::default(),
                )
                .unwrap_err();
            assert_eq!(error.error, "script_runtime_error", "{bad}");
            assert!(
                error.message.contains("finite number >= 0"),
                "{bad}: {}",
                error.message
            );
        }

        // An entity with no light at all names all three kinds in its error, so
        // the fix is obvious from the message.
        std::fs::write(
            dir.join("scripts/test.rhai"),
            r#"fn step(world, step) { world.light_intensity("Nothing"); }"#,
        )
        .unwrap();
        let scene_json = r#"{"name":"s","entities":[
            {"name":"Nothing","components":[
                {"type":"Transform"},
                {"type":"Script","source":"scripts/test.rhai"}
            ]}
        ]}"#;
        std::fs::write(&scene_path, scene_json).unwrap();
        let mut scene = Scene::from_source(scene_json, &scene_path.display().to_string()).unwrap();
        let host = host_for(&scene, &scene_path);
        let error = host
            .step(
                &mut scene.world,
                0,
                &InputState::default(),
                &Pointer::default(),
                &engine_core::ui::Interaction::default(),
                &ContactState::default(),
            )
            .unwrap_err();
        assert!(
            error.message.contains("PointLight")
                && error.message.contains("DirectionalLight")
                && error.message.contains("AmbientLight"),
            "{}",
            error.message
        );
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
        let host = host_for(&scene, &path);
        for step in 0..8 {
            host.step(
                &mut scene.world,
                step,
                &InputState::default(),
                &Pointer::default(),
                &engine_core::ui::Interaction::default(),
                &ContactState::default(),
            )
            .unwrap();
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
        let host = host_for(&scene, &path);

        host.step(
            &mut scene.world,
            0,
            &InputState::default(),
            &Pointer::default(),
            &engine_core::ui::Interaction::default(),
            &ContactState::default(),
        )
        .unwrap();
        assert_eq!(host.take_breaks(), vec!["Crate".to_string()]);
        assert!(host.take_breaks().is_empty(), "draining drains");

        let error = host
            .step(
                &mut scene.world,
                1,
                &InputState::default(),
                &Pointer::default(),
                &engine_core::ui::Interaction::default(),
                &ContactState::default(),
            )
            .unwrap_err();
        assert!(error.message.contains("no Breakable"), "{}", error.message);

        let error = host
            .step(
                &mut scene.world,
                2,
                &InputState::default(),
                &Pointer::default(),
                &engine_core::ui::Interaction::default(),
                &ContactState::default(),
            )
            .unwrap_err();
        assert!(
            error.message.contains("no entity named"),
            "{}",
            error.message
        );
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
        let host = host_for(&scene, &path);

        host.step(
            &mut scene.world,
            0,
            &InputState::default(),
            &Pointer::default(),
            &engine_core::ui::Interaction::default(),
            &ContactState::default(),
        )
        .unwrap();
        assert_eq!(
            host.take_explosions(),
            vec![QueuedExplosion {
                center: [1.0, 2.0, 3.0],
                radius: 5.0,
                impulse: 20.0
            }]
        );

        let error = host
            .step(
                &mut scene.world,
                1,
                &InputState::default(),
                &Pointer::default(),
                &engine_core::ui::Interaction::default(),
                &ContactState::default(),
            )
            .unwrap_err();
        assert!(
            error.message.contains("radius must be positive"),
            "{}",
            error.message
        );

        let error = host
            .step(
                &mut scene.world,
                2,
                &InputState::default(),
                &Pointer::default(),
                &engine_core::ui::Interaction::default(),
                &ContactState::default(),
            )
            .unwrap_err();
        assert!(
            error.message.contains("cannot be negative"),
            "{}",
            error.message
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn sync_names_lets_scripts_reach_spawned_entities() {
        let dir = temp_dir("sync");
        let (mut scene, path) = scene_with_script(
            &dir,
            r#"fn step(world, step) { world.set_position("Fragment", 1.0, 2.0, 3.0); }"#,
        );
        let mut host = host_for(&scene, &path);

        // Before the spawn the name is unknown — a runtime error.
        let error = host
            .step(
                &mut scene.world,
                0,
                &InputState::default(),
                &Pointer::default(),
                &engine_core::ui::Interaction::default(),
                &ContactState::default(),
            )
            .unwrap_err();
        assert!(
            error.message.contains("no entity named"),
            "{}",
            error.message
        );

        let spawned = scene
            .world
            .spawn((Name("Fragment".to_string()), Transform::default()));
        host.sync_names(&scene.world);
        host.step(
            &mut scene.world,
            1,
            &InputState::default(),
            &Pointer::default(),
            &engine_core::ui::Interaction::default(),
            &ContactState::default(),
        )
        .unwrap();
        let p = scene.world.get::<&Transform>(spawned).unwrap().position;
        assert_eq!(p, glam::Vec3::new(1.0, 2.0, 3.0));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn no_time_or_io_exists_in_the_sandbox() {
        let dir = temp_dir("sandbox");
        let (mut scene, path) = scene_with_script(&dir, r#"fn step(world, step) { timestamp(); }"#);
        let host = host_for(&scene, &path);
        let error = host
            .step(
                &mut scene.world,
                0,
                &InputState::default(),
                &Pointer::default(),
                &engine_core::ui::Interaction::default(),
                &ContactState::default(),
            )
            .unwrap_err();
        assert_eq!(
            error.error, "script_runtime_error",
            "timestamp() must not exist: {error:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Drive one script for `steps` steps with the default everything.
    fn run_steps(host: &ScriptHost, scene: &mut Scene, steps: u64) {
        for step in 0..steps {
            host.step(
                &mut scene.world,
                step,
                &InputState::default(),
                &Pointer::default(),
                &engine_core::ui::Interaction::default(),
                &ContactState::default(),
            )
            .unwrap();
        }
    }

    /// M36. The round trip is the whole promise: what one run saved, a
    /// *different* run loads. Two hosts rather than one, because a single
    /// host's `world.state` would return the right answer even if the file
    /// were never written.
    #[test]
    fn a_save_written_by_one_run_is_read_by_the_next() {
        let dir = temp_dir("save");
        let (mut scene, path) = scene_with_script(
            &dir,
            r#"fn step(world, step) {
                if step == 0 { world.set_state("score", 4200.0); }
                if step == 0 { world.set_state("level", 3.0); }
                if step == 1 { let ok = world.save(2); }
            }"#,
        );
        run_steps(&host_for(&scene, &path), &mut scene, 2);

        // Sorted keys and a plain map: a save is git-diffable by construction,
        // which is invariant 1 applied to a file the engine writes.
        let written = std::fs::read_to_string(dir.join("saves/slot2.json")).unwrap();
        assert!(
            written.find("\"level\"").unwrap() < written.find("\"score\"").unwrap(),
            "keys must be sorted: {written}"
        );

        let (mut fresh, path) = scene_with_script(
            &dir,
            r#"fn step(world, step) {
                if step == 0 { world.load(2); }
                if step == 1 {
                    let s = world.state("score", 0.0);
                    world.set_position("Mover", s, world.state("level", 0.0), 0.0);
                }
            }"#,
        );
        run_steps(&host_for(&fresh, &path), &mut fresh, 2);
        let entity = fresh.entity("Mover").unwrap();
        let p = fresh.world.get::<&Transform>(entity).unwrap().position;
        assert_eq!((p.x, p.y), (4200.0, 3.0), "the save did not come back");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// An empty slot is `false`, not an error — "is there a save?" is a menu's
    /// first question, and making it cost an error makes every menu wrap it.
    /// An out-of-range slot *is* an error, because a script choosing its own
    /// path is what the sandbox exists to prevent.
    #[test]
    fn an_empty_slot_is_false_and_an_impossible_slot_is_an_error() {
        let dir = temp_dir("slots");
        let (mut scene, path) = scene_with_script(
            &dir,
            r#"fn step(world, step) {
                let found = world.load(7);
                let there = world.has_save(7);
                if !found && !there { world.set_position("Mover", 1.0, 1.0, 1.0); }
            }"#,
        );
        run_steps(&host_for(&scene, &path), &mut scene, 1);
        let entity = scene.entity("Mover").unwrap();
        let p = scene.world.get::<&Transform>(entity).unwrap().position;
        assert_eq!(p.x, 1.0, "an absent slot must read as absent, not fail");

        let (mut scene, path) =
            scene_with_script(&dir, r#"fn step(world, step) { world.save(10); }"#);
        let host = host_for(&scene, &path);
        let error = host
            .step(
                &mut scene.world,
                0,
                &InputState::default(),
                &Pointer::default(),
                &engine_core::ui::Interaction::default(),
                &ContactState::default(),
            )
            .unwrap_err();
        assert_eq!(error.error, "script_runtime_error");
        assert!(
            error.message.contains("0..9"),
            "the message must name the range: {}",
            error.message
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// M36. `quit` is a request the caller drains, not an error and not a
    /// world change — the `take_breaks` shape, because what quitting *means*
    /// differs between the viewer and a headless run.
    #[test]
    fn quit_is_a_request_the_caller_reads() {
        let dir = temp_dir("quit");
        let (mut scene, path) = scene_with_script(
            &dir,
            r#"fn step(world, step) { if step == 3 { world.quit(); } }"#,
        );
        let host = host_for(&scene, &path);
        run_steps(&host, &mut scene, 3);
        assert!(!host.quit_requested(), "nothing asked to quit yet");
        run_steps(&host, &mut scene, 4);
        assert!(host.quit_requested());
        // Terminal, so asking twice must answer twice — a caller that checks
        // it in two places must not race itself.
        assert!(host.quit_requested());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// M36. A scene that calls no setter must leave the block exactly as it
    /// was — that equality is what keeps every pre-M36 baseline byte-identical,
    /// since the caller assigns this back unconditionally.
    #[test]
    fn an_untouched_environment_block_comes_back_unchanged() {
        let dir = temp_dir("env-untouched");
        let (mut scene, path) =
            scene_with_script(&dir, r#"fn step(world, step) { let d = world.dt(); }"#);
        let authored = EnvironmentSettings {
            sky: true,
            fog_density: 0.004,
            samples: 4,
            ..Default::default()
        };
        let host = ScriptHost::build(
            &scene.world,
            &path,
            60,
            None,
            authored,
            &engine_core::mesh::BuiltinAssets,
        )
        .unwrap()
        .unwrap();
        run_steps(&host, &mut scene, 5);
        assert_eq!(host.environment(), authored);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn environment_setters_reach_the_block_and_reject_impossible_values() {
        let dir = temp_dir("env-write");
        let (mut scene, path) = scene_with_script(
            &dir,
            r#"fn step(world, step) {
                if step == 0 { world.set_shadows(true); }
                if step == 0 { world.set_samples(4); }
                if step == 1 { world.set_fog(0.02); }
                if step == 2 { world.set_sky(!world.sky()); }
            }"#,
        );
        let host = host_for(&scene, &path);
        run_steps(&host, &mut scene, 3);
        let env = host.environment();
        assert!(env.shadows && env.sky);
        assert_eq!(env.samples, 4);
        assert!((env.fog_density - 0.02).abs() < 1e-6);

        // The vocabulary is the schema's — 1 or 4, and nothing silently
        // rounds, for M13's reason: this value ends up in a scene file.
        let (mut scene, path) =
            scene_with_script(&dir, r#"fn step(world, step) { world.set_samples(2); }"#);
        let host = host_for(&scene, &path);
        let error = host
            .step(
                &mut scene.world,
                0,
                &InputState::default(),
                &Pointer::default(),
                &engine_core::ui::Interaction::default(),
                &ContactState::default(),
            )
            .unwrap_err();
        assert!(
            error.message.contains("must be 1 or 4"),
            "{}",
            error.message
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
