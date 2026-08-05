//! The `engine` binary.
//!
//! Every command exits non-zero on failure and prints structured JSON errors
//! to stderr, one per line. Machine-facing success output goes to stdout as
//! JSON too. The full contract — streams, exit codes, the wire format — is
//! `docs/cli-contract.md`; the short version is that an agent can operate
//! every command with `jq` and `$?` alone. Nothing in this binary should ever
//! `panic!` on a user-reachable path, and if something does anyway, the panic
//! hook keeps even that inside the protocol.

mod app;
mod build;
mod scaffold;
mod simulate;

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use engine_core::{codes, EngineError, Result, Scene};
use engine_render::Gpu;
use winit::event_loop::{ControlFlow, EventLoop};

use crate::app::{Content, SceneContent, ViewerApp};

#[derive(Parser)]
#[command(
    name = "engine",
    about = "Agent-native 3D engine",
    version,
    disable_help_subcommand = true
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Open a window and render the M0 triangle (no scene; stack proof).
    Run {
        #[arg(long, default_value_t = 1280)]
        width: u32,
        #[arg(long, default_value_t = 720)]
        height: u32,
    },

    /// Open a window and render a scene. The keyboard drives scripts that
    /// call world.key(); --record-input turns a play session into a
    /// replayable timeline.
    RunScene {
        scene: PathBuf,
        /// Render from this entity's camera instead of the active one.
        #[arg(long)]
        camera: Option<String>,
        #[arg(long, default_value_t = 1280)]
        width: u32,
        #[arg(long, default_value_t = 720)]
        height: u32,
        /// Record the held-key timeline to this .input.jsonl file — replay
        /// it headlessly with --input on simulate/screenshot/diff-render.
        #[arg(long)]
        record_input: Option<PathBuf>,
    },

    /// Render a scene headlessly to a PNG — the agent's eyes.
    Screenshot {
        scene: PathBuf,
        #[arg(long)]
        out: PathBuf,
        /// Simulate this many physics steps first — edit, simulate, LOOK.
        #[arg(long, default_value_t = 0)]
        steps: u32,
        /// Replay this .input.jsonl timeline while stepping.
        #[arg(long)]
        input: Option<PathBuf>,
        /// Render the animated pose at this scene time (seconds).
        #[arg(long, default_value_t = 0.0, allow_hyphen_values = true)]
        time: f32,
        /// Render from this entity's camera instead of the active one.
        #[arg(long)]
        camera: Option<String>,
        #[arg(long, default_value_t = 1280)]
        width: u32,
        #[arg(long, default_value_t = 720)]
        height: u32,
    },

    /// Render a scene and compare it against a baseline PNG.
    ///
    /// The scene renders at exactly the baseline's dimensions. Defaults are
    /// bit-exact — right for same-machine regression checks; cross-adapter
    /// comparisons start at --threshold 3 --max-diff-percent 0.1 and tighten
    /// using the report's numbers. Bless baselines with `engine screenshot`.
    DiffRender {
        scene: PathBuf,
        baseline: PathBuf,
        /// Simulate this many physics steps before rendering — how baked
        /// moments of simulation get visual regression coverage.
        #[arg(long, default_value_t = 0)]
        steps: u32,
        /// Replay this .input.jsonl timeline while stepping.
        #[arg(long)]
        input: Option<PathBuf>,
        /// Compare the animated pose at this scene time (seconds).
        #[arg(long, default_value_t = 0.0, allow_hyphen_values = true)]
        time: f32,
        /// Write the visual diff here (red: violation, yellow: within
        /// threshold, faded gray: identical). Written on pass and fail.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Render from this entity's camera instead of the active one.
        #[arg(long)]
        camera: Option<String>,
        /// Per-channel byte tolerance before a pixel counts as differing.
        #[arg(long, default_value_t = 0)]
        threshold: u8,
        /// Allowed percentage of differing pixels.
        #[arg(long, default_value_t = 0.0)]
        max_diff_percent: f64,
    },

    /// Open the GUI editor: a live, writable view onto the scene file.
    Edit {
        scene: PathBuf,
        /// Read-only supervision mode: full viewport and inspection, writes
        /// disabled.
        #[arg(long)]
        watch: bool,
        /// Write one screenshot of the editor and exit (agent verification).
        #[arg(long, hide = true)]
        self_screenshot: Option<PathBuf>,
        /// Delay before the self-screenshot, in milliseconds.
        #[arg(long, hide = true, default_value_t = 1500)]
        self_screenshot_after_ms: u64,
    },

    /// Contact sheet: N animation frames sampled evenly over a time range,
    /// tiled into one PNG — how motion becomes visible in a single image.
    Filmstrip {
        scene: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value_t = 0.0, allow_hyphen_values = true)]
        start: f32,
        /// Defaults to the longest clip duration in the scene.
        #[arg(long, allow_hyphen_values = true)]
        end: Option<f32>,
        #[arg(long, default_value_t = 8)]
        frames: u32,
        #[arg(long, default_value_t = 4)]
        columns: u32,
        /// Per-tile size.
        #[arg(long, default_value_t = 320)]
        width: u32,
        #[arg(long, default_value_t = 180)]
        height: u32,
        /// Render from this entity's camera instead of the active one.
        #[arg(long)]
        camera: Option<String>,
    },

    /// Every animation clip reachable from a scene, a clip file, or a glTF.
    ListAnimations {
        /// A scene file, a .anim.json clip file, or a .gltf/.glb.
        path: Option<PathBuf>,
        /// Print the clip-file JSON Schema instead.
        #[arg(long)]
        schema: bool,
    },

    /// The skeleton of a rigged glTF, as JSON: every joint's name, parent,
    /// index and rest transform.
    ///
    /// With --time, each joint's *posed* world transform at that moment —
    /// which is the thing a filmstrip cannot tell you. A contact sheet shows
    /// that something moved; only this says the hand reached the doorknob.
    ListJoints {
        /// A scene file, or a .gltf/.glb directly.
        path: PathBuf,
        /// Which skinned entity, when a scene has more than one.
        #[arg(long)]
        entity: Option<String>,
        /// Pose the rig at this scene time instead of reporting the rest pose.
        #[arg(long, allow_hyphen_values = true)]
        time: Option<f32>,
        /// Run the scene this many fixed steps first, then report where the
        /// rig ended up.
        ///
        /// Needed by anything the *simulation* moves — a stride-driven clip's
        /// phase is advanced by the ground its entity covers (M32), so `--time`
        /// alone reports the pose the file was authored at rather than the one
        /// the run reached. Absent, nothing is stepped and the report stays the
        /// pure function of (files, time) it has always been.
        #[arg(long, default_value_t = 0)]
        steps: u32,
        /// Which clip to pose with, when reading a .gltf/.glb directly. In a
        /// scene the entity's AnimationPlayer already says.
        #[arg(long)]
        clip: Option<String>,
    },

    /// Step physics headlessly: build the world, advance N fixed steps.
    ///
    /// --bake writes the settled state back out as a valid scene file with
    /// every untouched byte preserved; --trace writes JSONL (one line per
    /// dynamic body per step, plus contact events) — the greppable,
    /// committable record of what happened.
    Simulate {
        scene: PathBuf,
        #[arg(long)]
        steps: u32,
        /// Replay this .input.jsonl timeline while stepping.
        #[arg(long)]
        input: Option<PathBuf>,
        #[arg(long)]
        bake: Option<PathBuf>,
        #[arg(long)]
        trace: Option<PathBuf>,
        /// Report this entity's final state instead of every dynamic body's.
        /// Repeatable, and reaches entities no trace enumerates — a scripted
        /// kinematic platform, a camera a chase script drives.
        #[arg(long)]
        entity: Vec<String>,
    },

    /// Cast a ray into the (optionally pre-simulated) scene; JSON result.
    Raycast {
        scene: PathBuf,
        /// Ray origin as x,y,z
        #[arg(long, allow_hyphen_values = true)]
        from: String,
        /// Ray direction as x,y,z (need not be normalized)
        #[arg(long, allow_hyphen_values = true)]
        dir: String,
        /// Simulate this many steps before casting.
        #[arg(long, default_value_t = 0)]
        steps: u32,
        /// Replay this .input.jsonl timeline while stepping.
        #[arg(long)]
        input: Option<PathBuf>,
    },

    /// Check scenes against the component schemas; report every error.
    Validate {
        #[arg(required = true, num_args = 1..)]
        scenes: Vec<PathBuf>,
        /// Treat warnings as errors (the CI mode).
        #[arg(long)]
        strict: bool,
    },

    /// Print a Road's sampled centerline: where the road is, which way it
    /// faces, and how far along that is.
    ///
    /// What anything *furnishing* a road needs — guardrails, signs, start
    /// lights. Publishing it is what stops a generator from re-implementing
    /// the sampler and drifting out of agreement with the ribbon it decorates.
    RoadCenterline {
        scene: PathBuf,
        /// Which road, when the scene has more than one. Defaults to the only
        /// one; with several, naming one is required.
        #[arg(long)]
        entity: Option<String>,
    },

    /// Print where a Junction's arms actually met it (M40).
    ///
    /// A junction is bounded by the *mouths* of the roads that reach it, and
    /// the author's job is to end each road near the crossing — so the question
    /// that matters is where those mouths turned out to be. A screenshot cannot
    /// tell a road that stopped 8 m short from one that arrived at the wrong
    /// angle, and both produce a patch that looks wrong in the same way.
    ///
    /// `road-centerline`'s reason, one primitive over.
    JunctionPlan {
        scene: PathBuf,
        /// Which junction, when the scene has more than one.
        #[arg(long)]
        entity: Option<String>,
    },

    /// Print every collider the physics world holds: shape, size, and where
    /// it actually is (M33).
    ///
    /// The command skinned collider proxies are really for. A hitbox riding a
    /// joint is invisible in a render and derived from a pose, so "is the head
    /// where I think it is" had no answer at all; this reads the answer back
    /// out of rapier rather than re-deriving it, so the report cannot drift
    /// from the simulation — `road-centerline`'s argument, applied to physics.
    ///
    /// Component-authored colliders are listed too, because "where are the
    /// colliders" is the question and half an answer is worse than none.
    ListColliders {
        scene: PathBuf,
        /// Narrow to one entity's colliders.
        #[arg(long)]
        entity: Option<String>,
        /// Step the simulation first. A proxy follows a pose, and a
        /// stride-driven pose is what the *run* reached rather than a function
        /// of the file — the reason `list-joints` grew the same flag in M32.
        #[arg(long, default_value_t = 0)]
        steps: u32,
        /// Replay an input timeline while stepping.
        #[arg(long)]
        input: Option<PathBuf>,
    },

    /// Solve a `SkinnedCollider` from a skinned mesh's vertex weights (M39).
    ///
    /// M33 refused generating a proxy set at runtime — "a derived artifact with
    /// no text form, which is invariant 1 read backwards" — and this is the
    /// answer to that rather than an overruling of it. The generator runs when
    /// an author asks it to, its output is JSON they read and edit, and
    /// `--write` splices it into the scene file so the file still says
    /// everything. Nothing at load time or step time consults a vertex weight.
    ///
    /// Deliberately not folded into `engine import`: a proxy set is a *choice*
    /// about a character, and every imported rig arriving with hitboxes nobody
    /// asked for is a scene file that got bigger for no reason.
    FitColliders {
        scene: PathBuf,
        /// Narrow to one entity. Absent, every skinned entity in the scene.
        #[arg(long)]
        entity: Option<String>,
        /// The shape to fit. `cuboid` is the bucket's bounding box exactly,
        /// with no axis to guess, which is why it is the default. `capsule`
        /// suits a rig whose bones are long — on a stubby one the dominant
        /// axis is a near-tie and the result is a capsule that is nearly a
        /// sphere, correct and useless.
        #[arg(long, default_value = "cuboid")]
        shape: String,
        /// Splice the result into the scene file, replacing any
        /// `SkinnedCollider` already on the entity. Without it, the component
        /// is printed and the file is untouched.
        #[arg(long)]
        write: bool,
    },

    /// Break an entity's volume into material-shaped shards (M43).
    ///
    /// M14 settled that a `Breakable`'s fragments exist in the text file before
    /// the run, and its design doc named this as the way out: "a future
    /// `engine fracture` CLI could *generate* this JSON offline without
    /// changing the runtime". Nothing at load time or break time fractures
    /// anything — this writes fragments, and the runtime spawns what it reads.
    ///
    /// The volume is the entity's cuboid `Collider` if it has one, else its
    /// mesh's bounding box. A source shape that is not a box fractures its box,
    /// which is stated rather than hidden: a fractured sphere would otherwise
    /// silently gain corners.
    Fracture {
        scene: PathBuf,
        /// The entity to break up.
        #[arg(long)]
        entity: String,
        /// What it is made of: `glass`, `wood`, `stone` or `metal`. Absent, the
        /// entity's existing `Breakable.material`, and failing that `stone`.
        #[arg(long)]
        material: Option<String>,
        /// How many pieces. Absent, what the material breaks into by default —
        /// glass the most, metal the fewest.
        #[arg(long)]
        pieces: Option<u32>,
        /// The fracture's random seed. The same seed reproduces the same
        /// shards byte for byte.
        #[arg(long, default_value_t = 1)]
        seed: u32,
        /// Where it was struck as x,y,z, in entity-local metres — materials
        /// break finer here. Absent, the middle of the face it would most
        /// likely be hit on.
        #[arg(long, allow_hyphen_values = true)]
        impact: Option<String>,
        /// Wood's grain direction as x,y,z, in entity-local space. Ignored by
        /// the other three materials; absent, the volume's longest axis.
        #[arg(long, allow_hyphen_values = true)]
        grain: Option<String>,
        /// The contact impulse that breaks it, in kg·m/s. Absent, the entity
        /// keeps the `impulse_threshold` it already had — and an entity with
        /// no `Breakable` yet gets none, which means script-and-explosion-only
        /// by M14's rule.
        #[arg(long)]
        threshold: Option<f32>,
        /// Splice the result into the scene file, replacing any `Breakable`
        /// already on the entity and keeping its `impulse_threshold`. Without
        /// it, the component is printed and the file is untouched.
        #[arg(long)]
        write: bool,
    },

    /// Print where every HUD element ends up on screen (M31).
    ///
    /// This is the command the UI system is really for. An agent authoring a
    /// menu cannot see it move; what it needs is the answer to "where did the
    /// Start button end up", so it can write a timeline cursor that lands on
    /// it. `engine road-centerline` publishes the samples a ribbon was built
    /// from for exactly this reason, and the failure it prevents is the same:
    /// a caller re-deriving geometry the engine already computed, and the two
    /// drifting.
    ///
    /// A pure function of (file, viewport) at rest by default, like `engine
    /// inspect`. `--steps` runs the simulation first, which M36 added for
    /// M32's reason: a menu a script *paints* — labelling seven slots per
    /// screen and hiding the rest — has a layout that is not a property of the
    /// file, and a system whose state no report can reach is what M30 §6 says
    /// not to build. The cursor is still never part of the question.
    UiLayout {
        scene: PathBuf,
        /// The framebuffer to lay out against. A pixel-authored UI is
        /// resolution-dependent by construction, so the size is part of the
        /// question — these default to the same 960×540 `simulate` hit-tests
        /// against.
        #[arg(long)]
        width: Option<u32>,
        #[arg(long)]
        height: Option<u32>,
        /// Narrow to named elements; repeatable. Unknown names are reported
        /// all at once.
        #[arg(long)]
        entity: Vec<String>,
        /// Report the layout the run *reached* rather than the authored one.
        #[arg(long, default_value_t = 0)]
        steps: u32,
        /// The input timeline to replay while stepping, as on `simulate`.
        #[arg(long)]
        input: Option<PathBuf>,
    },

    /// Bake a `LightProbeVolume`'s transfer to a file beside the scene (M35).
    ///
    /// The only command besides `import` that *writes* into the project, and
    /// like `import` it writes a file rather than mutating the scene. Rays are
    /// cast against render geometry, not colliders — the tour's trees carry no
    /// `Collider`, so asking physics what stood in the way would find a
    /// landscape with no trees on it.
    ///
    /// CPU-only and deterministic: the same scene bakes to the same bytes on
    /// any machine, which is a stronger promise than any render here makes.
    BakeGi {
        scene: PathBuf,
        /// Which volume. A scene may have only one, so this is the usual
        /// spelling-check argument rather than a choice.
        #[arg(long)]
        entity: Option<String>,
        /// Where to write it. Defaults to the `bake` path the component names,
        /// resolved relative to the scene file — which is the only path that
        /// makes the scene load afterwards.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Rays per probe. More is less noise and a slower bake; the value is
        /// recorded in the file, because two sample counts are two artifacts.
        #[arg(long)]
        samples: Option<u32>,
        /// Recompute the bake's `inputs_hash` and compare it against the
        /// committed file, without writing anything. Exits non-zero when the
        /// scene has moved since the bake.
        ///
        /// This is the geometry-level staleness check `validate` cannot afford:
        /// it needs the scene's whole triangle set — 504,970 of them for the
        /// showcase tour, 0.86s against `validate`'s 0.17s, and growing with
        /// the geometry while `validate` does not — so it is an explicit
        /// command rather than part of the fast gate.
        #[arg(long)]
        check: bool,
    },

    /// Ask what irradiance the renderer would use at a point and normal (M35).
    ///
    /// `terrain-height`'s argument applied to light: anything asking "why is
    /// this dark" needs the number, not a PNG. Reports which volume answered
    /// and how occluded the probes around the point are.
    GiProbe {
        scene: PathBuf,
        /// World position as x,y,z — all three, unlike `terrain-height`.
        #[arg(long, allow_hyphen_values = true)]
        at: String,
        /// Surface normal to evaluate along. Defaults to straight up.
        #[arg(long, allow_hyphen_values = true)]
        normal: Option<String>,
        /// Scene time in hours, for a scene with a `daylight` block. GI is
        /// baked as *transfer*, so the answer moves with the sky — which is
        /// the whole point, and is only checkable if this flag exists.
        #[arg(long)]
        time: Option<f32>,
    },

    /// Ask a terrain patch how high the ground is at a world XZ position.
    ///
    /// The same sampler `world.terrain_height` answers with, so a prop placed
    /// from the shell and one placed from a script land on the same ground.
    /// Needs no `Collider`: this is the height *field*, not a raycast, so a
    /// patch authored purely for looks answers too.
    TerrainHeight {
        scene: PathBuf,
        /// World position as x,z — the height is what is being asked for.
        #[arg(long, allow_hyphen_values = true)]
        at: String,
        /// Which patch, when the scene has more than one. Defaults to the only
        /// one; with several, naming one is required.
        #[arg(long)]
        entity: Option<String>,
    },

    /// Ask a water surface how high it is at a world XZ position, and which way
    /// it faces.
    ///
    /// The same evaluator `world.water_height` and buoyancy answer with, so a
    /// prop placed from the shell floats at the height the physics agrees with.
    /// Unlike `terrain-height` this takes a clock — water moves — and it can
    /// report *no water here*, because a patch has edges.
    WaterHeight {
        scene: PathBuf,
        /// World position as x,z — the height is what is being asked for.
        #[arg(long, allow_hyphen_values = true)]
        at: String,
        /// Which surface, when the scene has more than one. Defaults to the
        /// only one; with several, naming one is required.
        #[arg(long)]
        entity: Option<String>,
        /// Seconds of scene time. Overrides `--steps` when given, exactly as on
        /// `screenshot`.
        #[arg(long, default_value_t = 0.0)]
        time: f32,
        /// Fixed steps to convert into scene time, when `--time` is absent.
        #[arg(long, default_value_t = 0)]
        steps: u32,
    },

    /// Print entities with every component field resolved — defaults filled
    /// in, as the engine actually built them.
    ///
    /// Reading the scene file is not the same thing: absent fields *are* the
    /// documented defaults, so a `Material` that writes only `albedo` leaves
    /// four values unstated. This is the scene at rest — what you authored, not
    /// what happens when it runs.
    Inspect {
        scene: PathBuf,
        /// One entity by name. Absent, every entity, name-sorted.
        #[arg(long)]
        entity: Option<String>,
    },

    /// Import a glTF model's materials as engine material files, writing any
    /// embedded textures out as PNGs beside them.
    ///
    /// The materials in a `.glb` are already parsed and thrown away by the mesh
    /// loader; this keeps them. Embedded images are written out because a
    /// binary asset referenced by index is what invariants 1 and 3 both exist to
    /// prevent — an import has to produce an ordinary, diffable, hand-editable
    /// scene. One `materials/*.json` per glTF material, referenced rather than
    /// inlined, because a model routinely has several primitives sharing one.
    ///
    /// With `--into`, one entity is spliced into that scene: its `Mesh` naming
    /// the model and its `Material` naming the first imported material file.
    Import {
        /// The `.gltf` or `.glb` to import.
        model: PathBuf,
        /// A scene to splice an entity into. Absent, the files are written
        /// beside the model and nothing is edited.
        #[arg(long)]
        into: Option<PathBuf>,
        /// Where written textures go, relative to the scene (or the model).
        #[arg(long, default_value = "textures")]
        textures: String,
        /// Where written material files go.
        #[arg(long, default_value = "materials")]
        materials: String,
    },

    /// Scaffold a new project: a starter scene, a script, and the agent
    /// orientation under the names Claude Code and Codex already read.
    Init {
        /// Where to scaffold. Defaults to the current directory.
        #[arg(default_value = ".")]
        dir: PathBuf,
        /// Write even if the directory already holds files, overwriting any
        /// scaffolded name that collides.
        #[arg(long)]
        force: bool,
    },

    /// Print the agent orientation: the loop, the CLI contract, the scene
    /// format, and the conventions that are easy to get wrong.
    ///
    /// Documentation rather than a result, so it prints markdown to stdout
    /// like `--help` does — see docs/cli-contract.md.
    AgentGuide,

    /// Solve a TileGrid's layout by model synthesis, in overlapping blocks
    /// (M47).
    ///
    /// Writes beside the scene by default, because asset paths resolve
    /// relative to the scene file — the `bake-gi` rule, for its reason.
    ///
    /// `--region` re-solves only the blocks that overlap a rectangle of cells,
    /// reading everything outside them as fixed border. That is how an area is
    /// changed without re-rolling the world. A cell written `!name@rotation` is
    /// locked and survives every solve, including a full one.
    Synthesize {
        /// The scene holding the grid.
        scene: PathBuf,
        /// Which `TileGrid` — required only when the scene has more than one.
        #[arg(long)]
        entity: Option<String>,
        /// Override the component's seed for this solve.
        #[arg(long)]
        seed: Option<u32>,
        /// `x0,z0,x1,z1` inclusive, **in cells**: re-solve only the blocks
        /// whose interior meets this rectangle. Metres would make the flag's
        /// meaning depend on Transform.scale.
        #[arg(long, allow_hyphen_values = true)]
        region: Option<String>,
        /// Block extent in cells, `x,z`. Y is always taken whole.
        #[arg(long)]
        block: Option<String>,
        /// Cells two neighbouring blocks share, at least 1.
        #[arg(long)]
        overlap: Option<u32>,
        /// Retries a block gets before it gives up and keeps what stood there.
        #[arg(long)]
        attempts: Option<u32>,
        /// Write somewhere other than the component's `layout`.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Write the layout. Without it the report is printed and nothing on
        /// disk changes.
        #[arg(long)]
        write: bool,
        /// Recompute the digest and fail `tile_layout_stale` when the scene has
        /// moved under the layout. Writes nothing.
        #[arg(long)]
        check: bool,
    },

    /// Print a tileset's tiles: every rotation, its six sockets, how many
    /// tiles may sit across each face, and what it grows (M47).
    ///
    /// A socket graph is a constraint problem rather than a description, so it
    /// is the half of a tileset that is hard to author blind. `partners: 0` is
    /// a tile that can never be placed that side, and `engine validate` reports
    /// the same thing as `tile_socket_orphaned`.
    ListTiles {
        /// The tileset file.
        tileset: PathBuf,
    },

    /// Print the component and scene JSON Schemas.
    ListComponents {
        /// One component's schema instead of the whole vocabulary — the
        /// selection out of the `oneOf` that would otherwise be a `jq`
        /// expression. Carries the `$defs` it references, so it resolves on
        /// its own.
        #[arg(long)]
        component: Option<String>,
        /// Print the component reference as markdown instead of JSON Schema —
        /// the human-readable half of the same vocabulary, and how
        /// `docs/component-reference.md` is generated. A documented stdout
        /// exception, beside `agent-guide`.
        #[arg(long, conflicts_with = "component")]
        markdown: bool,
    },

    /// Compile the workspace, re-emitting rustc diagnostics as engine errors.
    Build {
        /// Type-check only (cargo check): the same errors in half the time.
        #[arg(long)]
        check: bool,
    },

    /// Print the selected GPU adapter as JSON.
    Info,

    /// Panic on purpose — exists so tests can prove the panic hook keeps
    /// even a crash inside the JSON protocol. Debug builds only.
    #[cfg(debug_assertions)]
    #[command(hide = true)]
    DebugPanic,
}

fn main() {
    install_panic_hook();

    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => exit_with_clap_error(error),
    };

    let result = match cli.command {
        Command::Run { width, height } => run_triangle(width, height),
        Command::RunScene {
            scene,
            camera,
            width,
            height,
            record_input,
        } => run_scene(scene, camera.as_deref(), width, height, record_input),
        Command::Screenshot {
            scene,
            out,
            steps,
            input,
            time,
            camera,
            width,
            height,
        } => screenshot(
            scene,
            out,
            steps,
            input,
            time,
            camera.as_deref(),
            width,
            height,
        ),
        Command::DiffRender {
            scene,
            baseline,
            steps,
            input,
            time,
            out,
            camera,
            threshold,
            max_diff_percent,
        } => diff_render(
            scene,
            baseline,
            steps,
            input,
            time,
            out,
            camera.as_deref(),
            threshold,
            max_diff_percent,
        ),
        Command::Filmstrip {
            scene,
            out,
            start,
            end,
            frames,
            columns,
            width,
            height,
            camera,
        } => filmstrip(
            scene,
            out,
            start,
            end,
            frames,
            columns,
            width,
            height,
            camera.as_deref(),
        ),
        Command::ListAnimations { path, schema } => list_animations(path, schema),
        Command::ListJoints {
            path,
            entity,
            time,
            steps,
            clip,
        } => list_joints(path, entity, time, steps, clip),
        Command::Edit {
            scene,
            watch,
            self_screenshot,
            self_screenshot_after_ms,
        } => engine_editor::run(engine_editor::EditorOptions {
            scene,
            watch_only: watch,
            screenshot: self_screenshot,
            screenshot_after_ms: self_screenshot_after_ms,
        }),
        Command::Simulate {
            scene,
            steps,
            input,
            bake,
            trace,
            entity,
        } => simulate::simulate_command(scene, steps, input, bake, trace, entity),
        Command::Raycast {
            scene,
            from,
            dir,
            steps,
            input,
        } => simulate::raycast_command(scene, from, dir, steps, input),
        Command::Validate { scenes, strict } => validate(&scenes, strict),
        Command::RoadCenterline { scene, entity } => road_centerline(scene, entity),
        Command::JunctionPlan { scene, entity } => junction_plan(scene, entity),
        Command::ListColliders {
            scene,
            entity,
            steps,
            input,
        } => list_colliders(scene, entity, steps, input),
        Command::FitColliders {
            scene,
            entity,
            shape,
            write,
        } => fit_colliders(scene, entity, shape, write),
        Command::Fracture {
            scene,
            entity,
            material,
            pieces,
            seed,
            impact,
            grain,
            threshold,
            write,
        } => fracture(FractureArgs {
            scene,
            entity,
            material,
            pieces,
            seed,
            impact,
            grain,
            threshold,
            write,
        }),
        Command::UiLayout {
            scene,
            width,
            height,
            entity,
            steps,
            input,
        } => ui_layout(scene, width, height, entity, steps, input),
        Command::BakeGi {
            scene,
            entity,
            out,
            samples,
            check,
        } => bake_gi(scene, entity, out, samples, check),
        Command::GiProbe {
            scene,
            at,
            normal,
            time,
        } => gi_probe(scene, at, normal, time),
        Command::TerrainHeight { scene, at, entity } => terrain_height(scene, at, entity),
        Command::WaterHeight {
            scene,
            at,
            entity,
            time,
            steps,
        } => water_height(scene, at, entity, time, steps),
        Command::Inspect { scene, entity } => inspect(scene, entity),
        Command::Import {
            model,
            into,
            textures,
            materials,
        } => import(model, into, textures, materials),
        Command::Init { dir, force } => scaffold::init(dir, force),
        Command::AgentGuide => {
            print!("{}", scaffold::AGENT_GUIDE);
            Ok(())
        }
        Command::Synthesize {
            scene,
            entity,
            seed,
            region,
            block,
            overlap,
            attempts,
            out,
            write,
            check,
        } => synthesize(SynthesizeArgs {
            scene,
            entity,
            seed,
            region,
            block,
            overlap,
            attempts,
            out,
            write,
            check,
        }),
        Command::ListTiles { tileset } => list_tiles(tileset),
        Command::ListComponents {
            component,
            markdown,
        } => list_components(component, markdown),
        Command::Build { check } => build::build(check),
        Command::Info => info(),
        #[cfg(debug_assertions)]
        Command::DebugPanic => panic!("deliberate panic from the hidden debug-panic subcommand"),
    };

    if let Err(error) = result {
        error.emit();
        std::process::exit(error.exit_code());
    }
}

/// Even a bug must speak the protocol: any panic prints one `EngineError`
/// line and exits 2. With `RUST_BACKTRACE` set, the backtrace rides inside
/// the JSON string — escaped, so the one-object-per-line guarantee holds
/// even then.
fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let payload: &str = if let Some(s) = info.payload().downcast_ref::<&str>() {
            s
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s
        } else {
            "non-string panic payload"
        };

        let mut message = format!(
            "internal panic: {payload}; this is an engine bug — the scene and \
             invocation may be fine"
        );
        if std::env::var("RUST_BACKTRACE").is_ok_and(|v| v != "0") {
            message = format!(
                "{message}\nbacktrace:\n{}",
                std::backtrace::Backtrace::force_capture()
            );
        }

        let mut error = EngineError::new(codes::INTERNAL_PANIC, message);
        if let Some(location) = info.location() {
            error = error
                .file(location.file())
                .line(location.line())
                .column(location.column());
        }
        error.emit();
        std::process::exit(2);
    }));
}

/// clap parse failures are the agent path too — a typo'd flag is exactly the
/// mistake an agent makes — so they get JSON like everything else, exit 2.
/// `--help`/`--version` are documentation, not errors, and stay prose.
fn exit_with_clap_error(error: clap::Error) -> ! {
    use clap::error::{ContextKind, ContextValue, ErrorKind};

    if matches!(
        error.kind(),
        ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
    ) {
        let _ = error.print();
        std::process::exit(0);
    }

    // The first line of clap's rendering ("error: unrecognized subcommand
    // 'scrnshot'") is the summary; the rest is usage prose.
    let rendered = error.render().to_string();
    let message = rendered
        .lines()
        .next()
        .unwrap_or("invalid invocation")
        .trim_start_matches("error: ")
        .to_string();

    let mut engine_error = EngineError::new(codes::INVALID_INVOCATION, message);

    // clap already computed the near-miss; surface it the same way scene
    // validation does.
    let suggestion = [
        ContextKind::SuggestedSubcommand,
        ContextKind::SuggestedArg,
        ContextKind::ValidValue,
    ]
    .iter()
    .find_map(|kind| match error.get(*kind) {
        Some(ContextValue::String(s)) => Some(s.clone()),
        Some(ContextValue::Strings(s)) => s.first().cloned(),
        _ => None,
    });
    if let Some(suggestion) = suggestion {
        engine_error = engine_error.did_you_mean(suggestion);
    }

    engine_error.emit();
    std::process::exit(2);
}

/// One scene's full validation report: every structural, semantic, and asset
/// diagnostic, emitted to stderr in file order.
pub(crate) struct SceneReport {
    pub(crate) source: Option<String>,
    pub(crate) errors: usize,
    pub(crate) warnings: usize,
}

/// Run the whole validation pipeline on one file and emit every diagnostic.
/// Every scene-consuming command calls this, so which command you ran never
/// changes what you learn about a broken scene — the diagnostics are
/// byte-identical to `engine validate`'s.
pub(crate) fn report_scene_diagnostics(path: &PathBuf) -> SceneReport {
    let display = path.display().to_string();
    let source = match std::fs::read_to_string(path) {
        Ok(source) => source,
        Err(e) => {
            EngineError::new(
                codes::SCENE_UNREADABLE,
                format!("could not read scene: {e}"),
            )
            .file(&display)
            .emit();
            return SceneReport {
                source: None,
                errors: 1,
                warnings: 0,
            };
        }
    };

    // Structural pass first; the asset pass assumes a well-formed scene, and
    // mixing "your JSON is wrong" with "your glTF is corrupt" in one report
    // would double-report every reference the structural pass rejected.
    let mut diagnostics = engine_core::validate::validate_source(&source, &display);
    if diagnostics.iter().all(EngineError::is_warning) {
        diagnostics.extend(engine_assets::validate_scene_assets(&source, &display));
    }
    if diagnostics.iter().all(EngineError::is_warning) {
        diagnostics.extend(engine_script::validate_scene_scripts(&source, &display));
    }

    let mut errors = 0;
    let mut warnings = 0;
    for diagnostic in &diagnostics {
        if diagnostic.is_warning() {
            warnings += 1;
        } else {
            errors += 1;
        }
        diagnostic.emit();
    }

    SceneReport {
        source: Some(source),
        errors,
        warnings,
    }
}

/// The `Material` component's field names, read from the published schema so
/// this cannot drift as fields are added.
///
/// Used to recognise a `materials/*.json` by its shape rather than by its
/// filename — the same choice `validate` already makes for clip files, and for
/// the same reason: an agent should not have to learn a naming convention to
/// get its file checked. One known field is enough, so a file with a *typo'd*
/// field is still validated as a material and told which field is wrong,
/// instead of being validated as a scene and told it has no entities.
fn material_fields() -> &'static [String] {
    use std::sync::OnceLock;
    static FIELDS: OnceLock<Vec<String>> = OnceLock::new();
    FIELDS.get_or_init(|| {
        let schema = engine_core::schema::component_schema();
        schema["oneOf"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|v| v["properties"]["type"]["const"] == "Material")
            .and_then(|v| v["properties"].as_object())
            .into_iter()
            .flatten()
            .map(|(name, _)| name.clone())
            .filter(|name| name != "type" && name != "asset")
            .collect()
    })
}

/// `engine validate` — every diagnostic for every file, then an aggregate
/// verdict. `--strict` promotes warnings to errors, for CI.
fn validate(scenes: &[PathBuf], strict: bool) -> Result<()> {
    let mut errors = 0usize;
    let mut warnings = 0usize;

    for path in scenes {
        // A clip file validates as a clip ("tracks", no "entities"): the
        // same all-at-once contract, structural checks only — entity-name
        // resolution needs a scene. A material file (M26) is routed the same
        // way, and is recognised the same way: by shape, not by filename, so
        // `materials/asphalt.json` and `asphalt.material.json` both work.
        let parsed = std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());
        let is_clip = parsed
            .as_ref()
            .is_some_and(|v| v.get("tracks").is_some() && v.get("entities").is_none());
        let is_material = parsed.as_ref().is_some_and(|v| {
            v.get("entities").is_none()
                && v.get("tracks").is_none()
                && v.as_object()
                    .is_some_and(|o| o.keys().any(|k| material_fields().iter().any(|f| f == k)))
        });
        // A tileset file (M47) is recognised the same way, by shape: a
        // top-level "tiles" and neither of the other two kinds' keys.
        let is_tileset = parsed
            .as_ref()
            .is_some_and(engine_core::tileset::Tileset::looks_like);
        if is_clip || is_material || is_tileset {
            let display = path.display().to_string();
            let source = std::fs::read_to_string(path).unwrap_or_default();
            let diagnostics = if is_clip {
                engine_core::animation::validate_clip_source(&source, &display)
            } else if is_tileset {
                engine_core::validate::validate_tileset_source(&source, &display)
            } else {
                engine_core::validate::validate_material_source(&source, &display)
            };
            for diagnostic in &diagnostics {
                diagnostic.emit();
            }
            // A tileset is the first non-scene kind that reports warnings, so
            // it is the first that has to split the count — `--strict` is what
            // decides whether an orphaned socket ends the process.
            errors += diagnostics.iter().filter(|d| !d.is_warning()).count();
            warnings += diagnostics.iter().filter(|d| d.is_warning()).count();
            continue;
        }
        let report = report_scene_diagnostics(path);
        errors += report.errors;
        warnings += report.warnings;
    }

    let files = scenes.len();
    if errors > 0 || (strict && warnings > 0) {
        let strict_note = if errors == 0 {
            "; --strict treats warnings as errors"
        } else {
            ""
        };
        return Err(EngineError::new(
            codes::VALIDATION_FAILED,
            format!(
                "{errors} error(s) and {warnings} warning(s) across {files} file(s){strict_note}"
            ),
        ));
    }

    println!(
        "{}",
        serde_json::json!({
            "valid": true,
            "files": files,
            "errors": 0,
            "warnings": warnings,
        })
    );
    Ok(())
}

/// Load a scene for rendering, reporting *all* validation errors first.
fn load_scene(path: &PathBuf) -> Result<Scene> {
    let report = report_scene_diagnostics(path);
    let display = path.display().to_string();

    if report.errors > 0 {
        return Err(EngineError::new(
            codes::VALIDATION_FAILED,
            format!("{} error(s) in {display}", report.errors),
        )
        .file(&display));
    }

    let source = report.source.unwrap_or_default();
    Scene::from_source(&source, &display).map_err(|mut errors| {
        // Validation was clean, so this is the desync backstop; emit any
        // surplus records and surface the last as the command's result.
        let last = errors.pop().unwrap_or_else(|| {
            EngineError::new(
                codes::SCENE_PARSE_DESYNC,
                "scene failed to load after clean validation",
            )
            .file(&display)
        });
        for error in errors {
            error.emit();
        }
        last
    })
}

/// Load a scene for `bake-gi`, tolerating the three errors the bake exists to
/// clear.
///
/// `report_scene_diagnostics`' contract is that which command you ran never
/// changes what you learn about a broken scene, and this is the one deliberate
/// exception: a scene whose bake is missing, stale or malformed is *exactly*
/// the scene `bake-gi` is for, so refusing it would make the command unable to
/// fix the only problem it addresses. Nothing else is tolerated — a scene with
/// a bad mesh reference still fails here, because the bake would ray-trace
/// geometry it could not load.
///
/// Found the hard way: the first end-to-end run refused to bake a brand-new
/// volume because that volume had no bake yet.
fn load_scene_for_bake(path: &PathBuf) -> Result<Scene> {
    const TOLERATED: &[&str] = &[
        codes::GI_BAKE_MISSING,
        codes::GI_BAKE_STALE,
        codes::GI_BAKE_MALFORMED,
    ];

    let display = path.display().to_string();
    let source = std::fs::read_to_string(path).map_err(|e| {
        EngineError::new(
            codes::SCENE_UNREADABLE,
            format!("could not read scene: {e}"),
        )
        .file(&display)
    })?;

    let mut diagnostics = engine_core::validate::validate_source(&source, &display);
    diagnostics.retain(|d| !TOLERATED.contains(&d.error));
    if diagnostics.iter().all(EngineError::is_warning) {
        diagnostics.extend(engine_assets::validate_scene_assets(&source, &display));
    }

    let mut errors = 0;
    for diagnostic in &diagnostics {
        if !diagnostic.is_warning() {
            errors += 1;
        }
        diagnostic.emit();
    }
    if errors > 0 {
        return Err(EngineError::new(
            codes::VALIDATION_FAILED,
            format!("{errors} error(s) in {display}"),
        )
        .file(&display));
    }

    Scene::from_source_ignoring(&source, &display, TOLERATED).map_err(|mut errors| {
        let last = errors.pop().unwrap_or_else(|| {
            EngineError::new(
                codes::SCENE_PARSE_DESYNC,
                "scene failed to load after clean validation",
            )
            .file(&display)
        });
        for error in errors {
            error.emit();
        }
        last
    })
}

/// `engine list-colliders` — every collider the physics world holds (M33).
///
/// Read out of the built world rather than out of the components: a skinned
/// collider proxy has no `Transform` to inspect, its placement is derived from
/// a pose, and a second implementation of that derivation is exactly what
/// `road-centerline` and `ui-layout` exist to prevent.
///
/// `--steps` runs the simulation first, for M32's reason: a stride-driven pose
/// is what the run reached, not a function of the file, so a report that never
/// stepped would describe a character standing still.
fn list_colliders(
    scene_path: PathBuf,
    entity: Option<String>,
    steps: u32,
    input_path: Option<PathBuf>,
) -> Result<()> {
    let display = scene_path.display().to_string();
    let mut scene = load_scene(&scene_path)?;
    let input = simulate::load_input(input_path.as_deref())?;

    let physics = simulate::run(
        &mut scene,
        &scene_path,
        steps,
        input.as_ref(),
        &engine_core::input::Viewport::DEFAULT,
        None,
    )?
    .physics;

    let rows = physics.collider_report();
    if let Some(wanted) = &entity {
        if !rows.iter().any(|row| &row.entity == wanted) {
            return Err(EngineError::new(
                codes::ENTITY_NOT_FOUND,
                format!("no entity named {wanted:?} with a collider in {display}"),
            )
            .entity(wanted)
            .file(&display)
            .suggest_from(wanted, rows.iter().map(|row| row.entity.as_str())));
        }
    }

    let colliders: Vec<serde_json::Value> = rows
        .iter()
        .filter(|row| entity.as_ref().is_none_or(|wanted| &row.entity == wanted))
        .map(|row| {
            let mut record = serde_json::json!({
                "entity": row.entity,
                "shape": row.shape,
                "dimensions": row.dimensions,
                "position": [row.position.x, row.position.y, row.position.z],
                "rotation": [row.rotation.x, row.rotation.y, row.rotation.z],
                "sensor": row.sensor,
            });
            // Only proxies carry a part, so an ordinary collider's row is the
            // shape it would have had if this command had existed since M8.
            if let Some(part) = &row.part {
                record["part"] = serde_json::json!(part);
            }
            record
        })
        .collect();

    println!(
        "{}",
        serde_json::json!({ "steps": steps, "colliders": colliders })
    );
    Ok(())
}

/// `engine fracture`'s arguments — a named struct rather than eight
/// positional ones, for `Presence`'s reason: three of them are
/// `Option<String>` and a swapped pair would type-check and fracture the wrong
/// thing.
struct FractureArgs {
    scene: PathBuf,
    entity: String,
    material: Option<String>,
    pieces: Option<u32>,
    seed: u32,
    impact: Option<String>,
    grain: Option<String>,
    threshold: Option<f32>,
    write: bool,
}

/// `engine fracture` — break an entity's volume into material-shaped shards
/// (M43).
///
/// The generator is a command and never a runtime behaviour, which is M14's
/// decision kept rather than overruled: what reaches the engine is a list of
/// fragments in the scene file, validated against the schema, identical from
/// run to run. `engine fit-colliders` (M39) is the same shape — a solver an
/// author runs, whose output is ordinary authored data.
fn fracture(args: FractureArgs) -> Result<()> {
    let FractureArgs {
        scene: scene_path,
        entity,
        material,
        pieces,
        seed,
        impact,
        grain,
        threshold,
        write,
    } = args;
    let display = scene_path.display().to_string();

    // The *file*, not the instantiated world: this reads and writes component
    // definitions, and `Scene` has already spent them into hecs.
    let source = std::fs::read_to_string(&scene_path).map_err(|e| {
        EngineError::new(
            codes::SCENE_UNREADABLE,
            format!("cannot read {display}: {e}"),
        )
        .file(&display)
    })?;
    load_scene(&scene_path)?;
    let scene: engine_core::SceneFile = serde_json::from_str(&source).map_err(|e| {
        EngineError::new(
            codes::SCENE_PARSE_DESYNC,
            format!("{display} passed validation but failed to parse: {e}"),
        )
        .file(&display)
    })?;

    let target = scene
        .entities
        .iter()
        .find(|e| e.name == entity)
        .ok_or_else(|| {
            EngineError::new(
                codes::UNKNOWN_ENTITY,
                format!("no entity named {entity:?} in {display}"),
            )
            .file(&display)
            .suggest_from(&entity, scene.entities.iter().map(|e| e.name.as_str()))
        })?;

    let existing = target.components.iter().find_map(|c| match c {
        engine_core::components::ComponentData::Breakable(b) => Some(b),
        _ => None,
    });

    // The material is the flag, else what the entity already says it is made
    // of, else stone — the one of the four that reads as "a solid object".
    let material = match &material {
        Some(name) => engine_core::components::FractureMaterial::parse(name).ok_or_else(|| {
            EngineError::new(
                codes::UNKNOWN_FIELD,
                format!("{name:?} is not a material this fractures"),
            )
            .file(&display)
            .suggest_from(
                name,
                engine_core::components::FractureMaterial::NAMES
                    .iter()
                    .copied(),
            )
        })?,
        None => existing
            .and_then(|b| b.material)
            .unwrap_or(engine_core::components::FractureMaterial::Stone),
    };

    // The fracture runs in **world** metres and the shards come back in local
    // ones. The entity's own scale is what separates the two, and it has to be
    // in this arithmetic rather than left out: a plank authored as a
    // `builtin:cube` scaled to [0.6, 0.18, 2.6] has a *cube* for its local box,
    // and a generator that only saw the local box would find no grain axis to
    // splinter along and no thin axis to shatter through.
    let (half_extents, scale) = source_volume(target, &scene_path, &display, &entity)?;
    let world_half = half_extents * scale.abs();
    let recipe = engine_core::fracture::Recipe {
        half_extents: world_half,
        material,
        pieces: pieces.unwrap_or_else(|| engine_core::fracture::Recipe::default_pieces(material)),
        seed,
        impact: match &impact {
            Some(text) => simulate::parse_vec3(text)? * scale.abs(),
            None => engine_core::fracture::Recipe::default_impact(world_half, material),
        },
        grain: match &grain {
            Some(text) => Some(simulate::parse_vec3(text)?),
            None => None,
        },
    };
    let mut fragments =
        engine_core::fracture::fracture(&recipe).map_err(|e| e.file(&display).entity(&entity))?;

    // Measured before the unscale, so the report is in metres — "how big are
    // the pieces" is a question about the world, not about local units.
    let volumes: Vec<f32> = fragments
        .iter()
        .map(|f| engine_core::shard::volume(f.points.as_deref().unwrap_or_default()))
        .collect();

    // Back into the entity's units, where a fragment's points and offset both
    // live — `Transform.scale` multiplies them again at spawn.
    let unscale = glam::Vec3::ONE / scale.abs();
    for fragment in &mut fragments {
        fragment.offset *= unscale;
        if let Some(points) = &mut fragment.points {
            for point in points {
                *point *= unscale;
            }
        }
    }

    // The threshold is a decision about the object rather than about its
    // shards, so a refracture keeps it — `fit-colliders` keeps a character's
    // layers for the same reason. `--threshold` is how a first fracture gives
    // one to an entity that had no `Breakable` at all.
    let component = engine_core::components::Breakable {
        fragments,
        impulse_threshold: threshold.or(existing.and_then(|b| b.impulse_threshold)),
        material: Some(material),
        // A refracture keeps whether the object puffs, for the threshold's
        // reason: it is a decision about the object, not about its shards.
        dust: existing.is_none_or(|b| b.dust),
    };

    if write {
        let mut encoded = serde_json::to_value(&component.fragments).map_err(|e| {
            EngineError::new(
                codes::SCENE_PARSE_DESYNC,
                format!("a generated fragment did not encode: {e}"),
            )
        })?;
        // Seven digits, not seventeen: these came from f32s, and a scene file
        // full of `0.12767969071865082` is precision nobody wrote.
        engine_core::formatter::shorten_floats(&mut encoded);
        let mut fields = vec![
            ("fragments".to_string(), encoded),
            ("material".to_string(), serde_json::json!(material.as_str())),
        ];
        if let Some(threshold) = component.impulse_threshold {
            fields.push((
                "impulse_threshold".to_string(),
                serde_json::json!(threshold),
            ));
        }
        let edited = engine_core::formatter::apply_replace_component(
            &source,
            &engine_core::formatter::ReplaceComponent {
                entity: entity.clone(),
                component: "Breakable".into(),
                fields,
            },
        )?;
        engine_core::formatter::write_atomic(&scene_path, &edited)?;
    }

    // The report answers "what did it do" without reading the shards: how big
    // the pieces are, in metres.
    let total: f32 = volumes.iter().sum();
    println!(
        "{}",
        serde_json::json!({
            "scene": display,
            "entity": entity,
            "material": material.as_str(),
            "seed": seed,
            "source_half_extents": half_extents.to_array(),
            "world_half_extents": world_half.to_array(),
            "impact": recipe.impact.to_array(),
            "pieces": component.fragments.len(),
            "volume": total,
            "smallest": volumes.iter().copied().fold(f32::MAX, f32::min),
            "largest": volumes.iter().copied().fold(0.0, f32::max),
            "written": write,
            "component": serde_json::to_value(&component).unwrap_or_default(),
        })
    );
    Ok(())
}

/// The box `engine fracture` breaks up: the entity's cuboid `Collider` if it
/// has one, else its mesh's bounding box.
///
/// The collider comes first deliberately. It is what physics already believes
/// the object is, so the shards replace exactly the volume that was being hit
/// — a mesh AABB that disagreed with the collider would fracture into a solid
/// a different size from the one that broke.
fn source_volume(
    entity: &engine_core::scene::EntityDef,
    scene_path: &Path,
    display: &str,
    name: &str,
) -> Result<(glam::Vec3, glam::Vec3)> {
    use engine_core::components::{ColliderShapeKind, ComponentData};

    let mut scale = glam::Vec3::ONE;
    let mut collider = None;
    let mut mesh = None;
    for component in &entity.components {
        match component {
            ComponentData::Transform(t) => scale = t.scale,
            ComponentData::Collider(c) => collider = Some(c),
            ComponentData::Mesh(m) => mesh = Some(m.asset.clone()),
            _ => {}
        }
    }

    if let Some(collider) = collider {
        if collider.shape == ColliderShapeKind::Cuboid {
            if let Some(half) = collider.half_extents {
                return Ok((half, scale));
            }
        }
    }

    let Some(asset) = mesh else {
        return Err(EngineError::new(
            codes::COLLIDER_MISSING_MESH,
            format!(
                "entity {name:?} has neither a cuboid Collider nor a Mesh, so there is \
                 no volume to fracture"
            ),
        )
        .file(display)
        .entity(name));
    };

    let base_dir = scene_path.parent().unwrap_or(Path::new("."));
    let data = match engine_core::mesh::MeshAsset::resolve(&asset, base_dir)? {
        engine_core::mesh::MeshAsset::Builtin(builtin) => builtin.data(),
        engine_core::mesh::MeshAsset::File(path) => engine_assets::load_gltf(&path)?,
    };
    let mut low = glam::Vec3::splat(f32::MAX);
    let mut high = glam::Vec3::splat(f32::MIN);
    for position in &data.positions {
        let point = glam::Vec3::from_array(*position);
        low = low.min(point);
        high = high.max(point);
    }
    if !(high.cmpgt(low).all()) {
        return Err(EngineError::new(
            codes::INVALID_SHAPE_DIMENSION,
            format!("the mesh on entity {name:?} has no volume to fracture"),
        )
        .file(display)
        .entity(name));
    }
    // The AABB's half-extents, centred: a mesh whose origin is not its middle
    // still fractures into a box around its own geometry.
    Ok(((high - low) * 0.5, scale))
}

/// `engine fit-colliders` — solve a proxy set from a skin's vertex weights
/// (M39 §8).
///
/// M33 refused runtime generation, correctly: a hitbox set the engine invented
/// each load would be a derived artifact with no text form, which is invariant
/// 1 read backwards. This is the same computation with the invariant intact —
/// it runs when an author asks, it prints JSON they can edit, and `--write`
/// splices it through `formatter`, the editor's own commit path, so every other
/// byte of the scene file is untouched.
///
/// An existing `SkinnedCollider` keeps its `layers`, `collides_with`, `friction`
/// and `restitution`: those are decisions about the character, and only the
/// shapes are what this solves.
fn fit_colliders(
    scene_path: PathBuf,
    entity: Option<String>,
    shape: String,
    write: bool,
) -> Result<()> {
    let display = scene_path.display().to_string();
    let shape_kind: engine_core::components::ColliderShapeKind =
        serde_json::from_value(serde_json::Value::String(shape.clone())).map_err(|_| {
            EngineError::new(
                codes::UNKNOWN_SHAPE,
                format!("{shape:?} is not a shape a proxy may be"),
            )
            .file(&display)
            .suggest_from(&shape, ["capsule", "cuboid", "sphere"])
        })?;
    if matches!(
        shape_kind,
        engine_core::components::ColliderShapeKind::Trimesh
            | engine_core::components::ColliderShapeKind::ConvexHull
    ) {
        return Err(EngineError::new(
            codes::COLLIDER_PART_SHAPE_UNSUPPORTED,
            format!(
                "{shape:?} describes one specific mesh, and a proxy exists precisely \
                 because the skinned mesh is on the GPU; fit \"capsule\", \"cuboid\" \
                 or \"sphere\""
            ),
        )
        .file(&display));
    }

    // The *file*, not the instantiated world: this reads and writes component
    // definitions, and `Scene` has already spent them into hecs.
    let source = std::fs::read_to_string(&scene_path).map_err(|e| {
        EngineError::new(
            codes::SCENE_UNREADABLE,
            format!("cannot read {display}: {e}"),
        )
        .file(&display)
    })?;
    load_scene(&scene_path)?;
    let scene: engine_core::SceneFile = serde_json::from_str(&source).map_err(|e| {
        EngineError::new(
            codes::SCENE_PARSE_DESYNC,
            format!("{display} passed validation but failed to parse: {e}"),
        )
        .file(&display)
    })?;
    let base_dir = scene_path.parent().unwrap_or(Path::new("."));

    // Entity order is the file's, which is what makes a regenerated set diff
    // cleanly against the committed one.
    let mut fitted: Vec<(String, Vec<engine_core::components::ColliderPart>)> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    for entry in &scene.entities {
        if entity.as_ref().is_some_and(|wanted| &entry.name != wanted) {
            continue;
        }
        let asset = entry.components.iter().find_map(|c| match c {
            engine_core::components::ComponentData::Mesh(mesh) => Some(mesh.asset.clone()),
            _ => None,
        });
        let Some(asset) = asset else { continue };
        let Ok(engine_core::mesh::MeshAsset::File(path)) =
            engine_core::mesh::MeshAsset::resolve(&asset, base_dir)
        else {
            continue;
        };
        let rig = engine_assets::load_rig(&path)?;
        let Some(skin) = rig.skin.as_ref() else {
            skipped.push(entry.name.clone());
            continue;
        };
        let mesh = engine_assets::load_gltf(&path)?;
        if mesh.joint_weights.is_empty() {
            skipped.push(entry.name.clone());
            continue;
        }
        fitted.push((
            entry.name.clone(),
            engine_core::ragdoll::fit_parts(skin, &mesh, shape_kind),
        ));
    }

    if fitted.is_empty() {
        let wanted = entity.as_deref().unwrap_or("any entity");
        return Err(EngineError::new(
            codes::MESH_HAS_NO_SKIN,
            format!(
                "no skinned mesh to fit in {display} for {wanted}; \
                 `engine list-joints` says which entities carry a rig"
            ),
        )
        .file(&display));
    }

    if write {
        for (name, parts) in &fitted {
            // Re-read per entity: each splice is committed before the next
            // one is rebased onto it, the editor's own commit shape.
            let source = std::fs::read_to_string(&scene_path).map_err(|e| {
                EngineError::new(
                    codes::SCENE_UNREADABLE,
                    format!("cannot read {display} to edit it: {e}"),
                )
                .file(&display)
            })?;
            // Layers, friction and restitution are statements about the
            // character rather than about its shapes, so a refit keeps them.
            let kept: Vec<(String, serde_json::Value)> = scene
                .entities
                .iter()
                .find(|e| &e.name == name)
                .and_then(|e| {
                    e.components.iter().find_map(|c| match c {
                        engine_core::components::ComponentData::SkinnedCollider(s) => Some(s),
                        _ => None,
                    })
                })
                .map(|existing| {
                    let mut kept = Vec::new();
                    if let Some(layers) = &existing.layers {
                        kept.push(("layers".into(), serde_json::json!(layers)));
                    }
                    if let Some(with) = &existing.collides_with {
                        kept.push(("collides_with".into(), serde_json::json!(with)));
                    }
                    kept.push(("friction".into(), serde_json::json!(existing.friction)));
                    kept.push((
                        "restitution".into(),
                        serde_json::json!(existing.restitution),
                    ));
                    kept
                })
                .unwrap_or_default();

            let mut encoded = serde_json::to_value(parts).map_err(|e| {
                EngineError::new(
                    codes::SCENE_PARSE_DESYNC,
                    format!("a fitted part did not encode: {e}"),
                )
            })?;
            // Seven digits, not seventeen: these came from f32s, and a scene
            // file full of `0.12767969071865082` is precision nobody wrote —
            // the fracture `--write` path does the same.
            engine_core::formatter::shorten_floats(&mut encoded);
            let mut fields = vec![("parts".to_string(), encoded)];
            fields.extend(kept);
            let edited = engine_core::formatter::apply_replace_component(
                &source,
                &engine_core::formatter::ReplaceComponent {
                    entity: name.clone(),
                    component: "SkinnedCollider".into(),
                    fields,
                },
            )?;
            engine_core::formatter::write_atomic(&scene_path, &edited)?;
        }
    }

    let entities: Vec<serde_json::Value> = fitted
        .iter()
        .map(|(name, parts)| {
            serde_json::json!({
                "entity": name,
                "component": { "type": "SkinnedCollider", "parts": parts },
            })
        })
        .collect();
    println!(
        "{}",
        serde_json::json!({
            "scene": display,
            "shape": shape,
            "written": write,
            // A rig whose vertices carry no weights is reported rather than
            // silently absent: "it fitted nothing" and "there was nothing to
            // fit" are different answers and an agent needs to tell them apart.
            "skipped": skipped,
            "entities": entities,
        })
    );
    Ok(())
}

/// `engine road-centerline` — publish a road's sampled centerline (M23).
///
/// The road's geometry is generated from its polygon of corners, and anything
/// placed *along* it — a guardrail, a sign, a start light — needs the same
/// samples the ribbon was built from. Re-deriving them in a generator is how
/// two implementations of one curve start disagreeing about where the road is,
/// so the engine publishes the one it actually used.
///
/// The transform is applied, so the positions are world space.
fn road_centerline(scene_path: PathBuf, entity: Option<String>) -> Result<()> {
    let scene = load_scene(&scene_path)?;
    // Junction patches ride in the same draw list (M40) and are not roads:
    // a patch has no centerline to publish, and counting them here would make
    // "the scene has 3 roads" wrong.
    let roads: Vec<_> = scene
        .road_items()
        .into_iter()
        .filter(|item| item.junction.is_none())
        .collect();

    let road = match (&entity, roads.len()) {
        (Some(name), _) => roads
            .iter()
            .find(|item| item.entity == *name)
            .ok_or_else(|| {
                EngineError::new(
                    codes::ENTITY_NOT_FOUND,
                    format!("no entity named {name:?} with a Road component"),
                )
                .entity(name)
                .file(scene_path.display().to_string())
                .suggest_from(name, roads.iter().map(|item| item.entity.as_str()))
            })?,
        (None, 1) => &roads[0],
        (None, 0) => {
            return Err(EngineError::new(
                codes::MISSING_COMPONENT,
                "scene has no entity with a Road component",
            )
            .file(scene_path.display().to_string()))
        }
        (None, _) => {
            return Err(EngineError::new(
                codes::MISSING_COMPONENT,
                format!(
                    "scene has {} roads ({}); name one with --entity",
                    roads.len(),
                    roads
                        .iter()
                        .map(|item| item.entity.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            )
            .file(scene_path.display().to_string()))
        }
    };

    let points: Vec<serde_json::Value> = road
        .surface
        .centerline
        .iter()
        .map(|point| {
            let world = road.model.transform_point3(point.position);
            serde_json::json!({
                "position": [world.x, world.y, world.z],
                "forward": [point.direction.x, point.direction.y],
                "v": point.v,
                // M40: the cross-section, so a car placed on a banked corner
                // does not have to re-derive the roll from the polygon.
                "width": point.width,
                "bank": point.bank.to_degrees(),
            })
        })
        .collect();

    println!(
        "{}",
        serde_json::json!({
            "entity": road.entity,
            "length": road.surface.length,
            "width": road.road.width,
            "shoulder": road.road.shoulder,
            "closed": road.road.closed,
            "points": points,
        })
    );
    Ok(())
}

/// `engine junction-plan <scene> [--entity N]` — where a junction's arms met it
/// (M40).
///
/// The transform is applied, so the positions are world space, matching
/// `road-centerline`. `reach` is how far each mouth sits from the patch's
/// centre: a set of similar reaches is a tidy junction, and one much larger
/// than the rest is the road that stopped short.
fn junction_plan(scene_path: PathBuf, entity: Option<String>) -> Result<()> {
    let scene = load_scene(&scene_path)?;
    let junctions = scene.junction_items();

    let junction = match (&entity, junctions.len()) {
        (Some(name), _) => junctions
            .iter()
            .find(|item| item.entity == *name)
            .ok_or_else(|| {
                EngineError::new(
                    codes::ENTITY_NOT_FOUND,
                    format!("no entity named {name:?} with a Junction component"),
                )
                .entity(name)
                .file(scene_path.display().to_string())
                .suggest_from(name, junctions.iter().map(|item| item.entity.as_str()))
            })?,
        (None, 1) => &junctions[0],
        (None, 0) => {
            return Err(EngineError::new(
                codes::MISSING_COMPONENT,
                "scene has no entity with a Junction component",
            )
            .file(scene_path.display().to_string()))
        }
        (None, _) => {
            return Err(EngineError::new(
                codes::MISSING_COMPONENT,
                format!(
                    "scene has {} junctions ({}); name one with --entity",
                    junctions.len(),
                    junctions
                        .iter()
                        .map(|item| item.entity.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            )
            .file(scene_path.display().to_string()))
        }
    };

    let arms: Vec<serde_json::Value> = junction
        .surface
        .mouths
        .iter()
        .map(|mouth| {
            let world = junction.model.transform_point3(mouth.center);
            serde_json::json!({
                "road": mouth.road,
                "position": [world.x, world.y, world.z],
                "into": [mouth.into.x, mouth.into.y],
                "width": mouth.half_asphalt * 2.0,
                "half_total": mouth.half_total,
                "reach": mouth.reach,
            })
        })
        .collect();

    println!(
        "{}",
        serde_json::json!({
            "entity": junction.entity,
            "width": junction.surface.half_asphalt * 2.0,
            "shoulder": junction.surface.shoulder,
            "triangles": junction.surface.mesh.triangle_count(),
            "arms": arms,
        })
    );
    Ok(())
}

/// `engine ui-layout <scene> [--width W --height H] [--entity N]...` (M31).
///
/// Reports the rectangle the layout engine puts every HUD element in — the
/// same function `hud::rasterize` draws from and the same one the hit test
/// uses, so the report cannot disagree with the picture.
///
/// **Name-sorted**, following `simulate --entity`'s contract rather than draw
/// order: a report is read by name, and draw order is already visible in the
/// render. `depth` and `parent` carry the tree for anything that wants it.
fn ui_layout(
    scene_path: PathBuf,
    width: Option<u32>,
    height: Option<u32>,
    entities: Vec<String>,
    steps: u32,
    input_path: Option<PathBuf>,
) -> Result<()> {
    let mut scene = load_scene(&scene_path)?;
    let assets = engine_assets::AssetServer::for_scene(&scene_path);
    let view = engine_core::input::Viewport::DEFAULT;
    let (width, height) = (width.unwrap_or(view.width), height.unwrap_or(view.height));

    // Stepping first, when asked (M36) — the `list-joints --steps` shape, for
    // the same reason. The run is stepped against *this* viewport rather than
    // the documented default, because a mouse-driven script's clicks are a
    // function of the frame (M28 §5) and reporting a layout the run could not
    // have produced would be worse than not reporting one.
    if steps > 0 {
        let input = simulate::load_input(input_path.as_deref())?;
        // No camera: `ui-layout` has no `--camera`, and a HUD is screen-space
        // anyway — what the viewport is carrying here is the frame the script's
        // clicks resolve against.
        let stepping = engine_core::input::Viewport::new(width, height, None);
        simulate::run(
            &mut scene,
            &scene_path,
            steps,
            input.as_ref(),
            &stepping,
            None,
        )?;
    }

    let tree = scene.hud_tree(&assets);
    let layout = engine_core::ui::layout(&tree, width, height);

    // Unknown names all at once, the M25 rule — an agent fixing one typo at a
    // time is an agent running the command four times.
    if !entities.is_empty() {
        let unknown: Vec<&String> = entities
            .iter()
            .filter(|name| tree.index_of(name).is_none())
            .collect();
        if !unknown.is_empty() {
            let known: Vec<&str> = tree.nodes.iter().map(|n| n.entity.as_str()).collect();
            let mut error = EngineError::new(
                codes::ENTITY_NOT_FOUND,
                format!(
                    "no HUD element on {}",
                    unknown
                        .iter()
                        .map(|n| format!("{n:?}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            )
            .file(scene_path.display().to_string());
            if let Some(first) = unknown.first() {
                error = error.entity(first.as_str()).suggest_from(first, known);
            }
            return Err(error);
        }
    }

    let mut elements: Vec<serde_json::Value> = layout
        .placed
        .iter()
        .filter(|placed| entities.is_empty() || entities.contains(&tree.nodes[placed.node].entity))
        .map(|placed| {
            let node = &tree.nodes[placed.node];
            serde_json::json!({
                "entity": node.entity,
                "kind": node.kind.name(),
                "parent": placed.parent,
                "depth": placed.depth,
                "rect": [
                    placed.rect.x,
                    placed.rect.y,
                    placed.rect.width,
                    placed.rect.height,
                ],
                "visible": placed.visible,
                "interactive": node
                    .interact
                    .as_ref()
                    .is_some_and(|interact| !interact.disabled),
            })
        })
        .collect();
    elements.sort_by(|a, b| a["entity"].as_str().cmp(&b["entity"].as_str()));

    println!(
        "{}",
        serde_json::json!({
            "viewport": [width, height],
            "elements": elements,
        })
    );
    Ok(())
}

/// `engine synthesize`'s flags, as a named struct for the reason
/// [`FractureArgs`] is one: several are `Option<String>` and a swapped pair
/// would type-check and solve the wrong thing.
struct SynthesizeArgs {
    scene: PathBuf,
    entity: Option<String>,
    seed: Option<u32>,
    region: Option<String>,
    block: Option<String>,
    overlap: Option<u32>,
    attempts: Option<u32>,
    out: Option<PathBuf>,
    write: bool,
    check: bool,
}

/// Everything a solve needs, read off the scene once.
struct GridInputs {
    name: String,
    component: engine_core::components::TileGrid,
    tileset: engine_core::tileset::Tileset,
    tiles: Vec<engine_core::tileset::ExpandedTile>,
    compat: engine_core::tileset::Compat,
    grid: engine_core::tilelayout::Grid,
    target: PathBuf,
}

/// `engine synthesize <scene> [--entity NAME] [--region ...] [--write]
/// [--check]` (M47).
///
/// The `bake-gi` shape, and for its reasons: a solve is expensive, its output
/// is an artifact the scene names, and the artifact is what a later edit edits.
/// `--check` is the same command with the writing taken out — it recomputes the
/// digest and compares — and it lives here rather than in `validate` because
/// the digest reads the whole tileset and the scene's terrain, which is a cost
/// the fast gate should not pay on every file.
fn synthesize(args: SynthesizeArgs) -> Result<()> {
    if args.check && args.out.is_some() {
        return Err(EngineError::new(
            codes::INVALID_INVOCATION,
            "--check writes nothing, so --out has nothing to name",
        )
        .file(args.scene.display().to_string()));
    }
    if args.check && args.write {
        return Err(EngineError::new(
            codes::INVALID_INVOCATION,
            "--check writes nothing; drop --write to check, or --check to solve",
        )
        .file(args.scene.display().to_string()));
    }

    let inputs = read_grid(&args)?;
    let params = solve_params(&args, &inputs)?;
    let seed = args.seed.unwrap_or(inputs.component.seed);

    // The prior layout, when there is one. Locked cells and `--region` both
    // read from it, and both are meaningless without it.
    let prior = std::fs::read_to_string(&inputs.target)
        .ok()
        .and_then(|text| engine_core::tilelayout::TileLayout::parse(&text).ok())
        .filter(|layout| layout.header.size == inputs.component.size);

    let digest = inputs_digest(&inputs, seed, params, prior.as_ref());
    if args.check {
        return check_layout(&args, &inputs, &digest);
    }

    let (prior_cells, locked) = match &prior {
        Some(layout) => {
            let cells = layout.resolve(&inputs.tiles).map_err(|unknown| {
                EngineError::new(
                    codes::UNKNOWN_TILE,
                    format!(
                        "the layout at {} places tiles the tileset no longer defines: {}; \
                         delete it to solve from scratch",
                        inputs.target.display(),
                        unknown.join(", ")
                    ),
                )
                .entity(&inputs.name)
                .file(args.scene.display().to_string())
            })?;
            let locked: Vec<bool> = layout.cells.iter().map(|c| c.locked).collect();
            (Some(cells), locked)
        }
        None => (None, vec![false; inputs.grid.cell_count()]),
    };

    if args.region.is_some() && prior_cells.is_none() {
        return Err(EngineError::new(
            codes::INVALID_INVOCATION,
            format!(
                "--region re-solves part of an existing layout, and there is none at {}; \
                 run `engine synthesize --write` once first",
                inputs.target.display()
            ),
        )
        .entity(&inputs.name)
        .file(args.scene.display().to_string()));
    }

    let (fill_ground, fill_background) =
        engine_core::tilegrid::fill_indices(&inputs.component, &inputs.tileset, &inputs.tiles)
            .map_err(|name| {
                EngineError::new(
                    codes::UNKNOWN_TILE,
                    format!("the tileset defines no tile named {name:?}"),
                )
                .entity(&inputs.name)
                .file(args.scene.display().to_string())
            })?;

    let region = parse_region(args.region.as_deref(), &inputs)?;
    let outcome = engine_core::synthesize::synthesize(&engine_core::synthesize::Request {
        grid: &inputs.grid,
        tiles: &inputs.tiles,
        compat: &inputs.compat,
        seed,
        params,
        fill_ground,
        fill_background,
        prior: prior_cells.as_deref(),
        locked: &locked,
        region,
    })
    .map_err(|e| e.entity(&inputs.name).file(args.scene.display().to_string()))?;

    let layout = engine_core::tilelayout::TileLayout {
        header: engine_core::tilelayout::LayoutHeader {
            format: engine_core::tilelayout::FORMAT.to_string(),
            scene: args
                .scene
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            entity: inputs.name.clone(),
            tileset: inputs.component.tileset.clone(),
            tileset_hash: engine_core::tileset::digest(&inputs.tileset),
            inputs_hash: digest.clone(),
            size: inputs.component.size,
            seed,
            block: params.block,
            overlap: params.overlap,
            attempts: params.attempts,
            fallbacks: outcome.fallbacks,
            offsets: inputs.grid.offsets.clone(),
        },
        cells: outcome
            .cells
            .iter()
            .enumerate()
            .map(|(cell, tile)| {
                engine_core::tilelayout::Cell::new(
                    inputs.tiles[*tile].token.clone(),
                    locked.get(cell).copied().unwrap_or(false),
                )
            })
            .collect(),
    };

    if args.write {
        if let Some(parent) = inputs.target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                EngineError::new(
                    codes::SCENE_WRITE_FAILED,
                    format!("could not create {}: {e}", parent.display()),
                )
            })?;
        }
        engine_core::formatter::write_atomic(&inputs.target, &layout.to_text())?;
    }

    let placed = layout
        .cells
        .iter()
        .filter(|c| !c.token.is_empty())
        .count();
    println!(
        "{}",
        serde_json::json!({
            "scene": args.scene.display().to_string(),
            "entity": inputs.name,
            "layout": inputs.target.display().to_string(),
            "tileset": inputs.component.tileset,
            "seed": seed,
            "size": inputs.component.size,
            "cells": placed,
            "locked": locked.iter().filter(|l| **l).count(),
            "terraced": !inputs.grid.offsets.is_empty(),
            "blocks": outcome.blocks,
            "solved": outcome.solved,
            "retries": outcome.retries,
            // Blocks that used up their retries and kept what stood there. The
            // number that says a tileset is over-constrained without reading
            // the picture.
            "fallbacks": outcome.fallbacks,
            "vertices": engine_core::tilegrid::vertex_count(
                &inputs.tileset,
                &inputs.tiles,
                &outcome.cells,
            ),
            "inputs_hash": digest,
            "written": args.write,
        })
    );
    Ok(())
}

/// Read the scene, resolve the grid's two files, and derive its terracing.
fn read_grid(args: &SynthesizeArgs) -> Result<GridInputs> {
    // A grid with no layout yet is exactly the case this command exists to
    // fix, so its own absence must not stop the scene loading — the
    // `load_scene_for_bake` trade.
    let scene = load_scene_for_synthesis(&args.scene)?;
    let base_dir = args.scene.parent().unwrap_or(Path::new("")).to_path_buf();
    let name = sole_entity_with::<engine_core::components::TileGrid>(
        &scene,
        &args.scene,
        &args.entity,
        "TileGrid",
        "tile grids",
    )?;
    let handle = scene.entity(&name).expect("named above");
    let component = (*scene
        .world
        .get::<&engine_core::components::TileGrid>(handle)
        .expect("filtered above"))
    .clone();

    let tileset_path = engine_core::tileset::resolve_tileset(&component.tileset, &base_dir)
        .map_err(|e| e.entity(&name).file(args.scene.display().to_string()))?;
    let tileset = engine_core::tileset::load_tileset(&tileset_path)?;
    let tiles = engine_core::tileset::expand(&tileset).map_err(|e| e.entity(&name))?;
    let compat = engine_core::tileset::Compat::build(&tiles);

    let grid = engine_core::tilelayout::Grid {
        size: component.size,
        offsets: scene.tile_grid_offsets(&name, &component, tileset.cell),
    };
    let target = match &args.out {
        Some(path) => path.clone(),
        None => base_dir.join(&component.layout),
    };

    Ok(GridInputs {
        name,
        component,
        tileset,
        tiles,
        compat,
        grid,
        target,
    })
}

/// Load a scene whose grid may have no layout yet, and whose layout may be
/// stale — the two states this command is run to leave.
fn load_scene_for_synthesis(path: &PathBuf) -> Result<Scene> {
    const TOLERATED: &[&str] = &[
        codes::TILE_LAYOUT_MISSING,
        codes::TILE_LAYOUT_STALE,
        codes::TILE_LAYOUT_MALFORMED,
        codes::TILE_LAYOUT_MISMATCH,
        codes::TILE_LAYOUT_ILLEGAL,
        codes::UNKNOWN_TILE,
        // A grid that cannot be built yet is the whole reason to run this.
        codes::GI_BAKE_MISSING,
        codes::GI_BAKE_STALE,
    ];

    let display = path.display().to_string();
    let source = std::fs::read_to_string(path).map_err(|e| {
        EngineError::new(
            codes::SCENE_UNREADABLE,
            format!("could not read scene: {e}"),
        )
        .file(&display)
    })?;

    let mut diagnostics = engine_core::validate::validate_source(&source, &display);
    diagnostics.retain(|d| !TOLERATED.contains(&d.error));
    let fatal: Vec<EngineError> = diagnostics
        .into_iter()
        .filter(|d| !d.is_warning())
        .collect();
    if !fatal.is_empty() {
        for error in &fatal {
            error.emit();
        }
        return Err(EngineError::new(
            codes::VALIDATION_FAILED,
            format!("{} is not valid; fix it before synthesizing", display),
        )
        .file(&display));
    }

    Scene::from_source_ignoring(&source, &display, TOLERATED).map_err(|errors| {
        for error in errors.iter().skip(1) {
            error.emit();
        }
        errors.into_iter().next().expect("non-empty")
    })
}

fn solve_params(
    args: &SynthesizeArgs,
    inputs: &GridInputs,
) -> Result<engine_core::synthesize::Params> {
    let mut params = engine_core::synthesize::Params::default();
    if let Some(text) = &args.block {
        let pair = parse_pair(text, "--block")?;
        params.block = [pair.0, u32::MAX, pair.1];
    }
    if let Some(overlap) = args.overlap {
        params.overlap = overlap.max(1);
    }
    if let Some(attempts) = args.attempts {
        params.attempts = attempts.max(1);
    }
    let _ = inputs;
    Ok(params)
}

fn parse_pair(text: &str, flag: &str) -> Result<(u32, u32)> {
    let parts: Vec<&str> = text.split(',').collect();
    let parsed: Option<Vec<u32>> = (parts.len() == 2)
        .then(|| parts.iter().map(|p| p.trim().parse::<u32>().ok()).collect())
        .flatten();
    match parsed {
        Some(values) if values[0] > 0 && values[1] > 0 => Ok((values[0], values[1])),
        _ => Err(EngineError::new(
            codes::INVALID_INVOCATION,
            format!("{flag} takes two positive whole numbers, as x,z — found {text:?}"),
        )),
    }
}

fn parse_region(text: Option<&str>, inputs: &GridInputs) -> Result<Option<[u32; 4]>> {
    let Some(text) = text else {
        return Ok(None);
    };
    let parts: Vec<&str> = text.split(',').collect();
    let parsed: Option<Vec<u32>> = (parts.len() == 4)
        .then(|| parts.iter().map(|p| p.trim().parse::<u32>().ok()).collect())
        .flatten();
    let Some(values) = parsed else {
        return Err(EngineError::new(
            codes::INVALID_INVOCATION,
            format!("--region takes four whole cell indices, as x0,z0,x1,z1 — found {text:?}"),
        ));
    };
    let [x0, z0, x1, z1] = [values[0], values[1], values[2], values[3]];
    if x0 > x1 || z0 > z1 {
        return Err(EngineError::new(
            codes::INVALID_INVOCATION,
            format!("--region {text:?} runs backwards; it is x0,z0,x1,z1 inclusive"),
        ));
    }
    let [nx, _, nz] = inputs.component.size;
    if x1 >= nx || z1 >= nz {
        return Err(EngineError::new(
            codes::INVALID_INVOCATION,
            format!(
                "--region {text:?} reaches past the grid, which is {nx} by {nz} cells \
                 (indices are 0-based and inclusive)"
            ),
        ));
    }
    Ok(Some([x0, z0, x1, z1]))
}

/// Everything a layout is a function of.
///
/// The locked set is in here because a lock is an *input* to the solve: pinning
/// a cell and re-running produces a different world, and a digest that ignored
/// it would call the old file current.
fn inputs_digest(
    inputs: &GridInputs,
    seed: u32,
    params: engine_core::synthesize::Params,
    prior: Option<&engine_core::tilelayout::TileLayout>,
) -> String {
    let mut h = engine_core::gi::InputsHasher::new();
    h.str(&engine_core::tileset::digest(&inputs.tileset));
    h.str(&inputs.component.tileset);
    h.str(&inputs.component.fill_ground);
    h.str(&inputs.component.fill_background);
    for axis in inputs.component.size {
        h.u32(axis);
    }
    h.u32(seed);
    for axis in params.block {
        h.u32(axis);
    }
    h.u32(params.overlap).u32(params.attempts);
    h.u32(inputs.grid.offsets.len() as u32);
    for offset in &inputs.grid.offsets {
        h.u32(*offset as u32);
    }
    // Which cells are pinned, not what they hold: what they hold is the
    // layout's own content, and hashing it would make the digest a checksum of
    // the file rather than of its inputs.
    if let Some(layout) = prior {
        for (index, cell) in layout.cells.iter().enumerate() {
            if cell.locked {
                h.u32(index as u32).str(&cell.token);
            }
        }
    }
    h.finish()
}

/// The staleness half, mirroring `check_bake`.
fn check_layout(args: &SynthesizeArgs, inputs: &GridInputs, digest: &str) -> Result<()> {
    let text = std::fs::read_to_string(&inputs.target).map_err(|e| {
        EngineError::new(
            codes::TILE_LAYOUT_MISSING,
            format!(
                "could not read {}: {e}; run `engine synthesize --write`",
                inputs.target.display()
            ),
        )
        .entity(&inputs.name)
        .file(args.scene.display().to_string())
    })?;
    let layout = engine_core::tilelayout::TileLayout::parse(&text).map_err(|bad| {
        EngineError::new(
            codes::TILE_LAYOUT_MALFORMED,
            format!("{}: {bad}", inputs.target.display()),
        )
        .entity(&inputs.name)
        .file(args.scene.display().to_string())
    })?;

    if layout.header.inputs_hash != digest {
        return Err(EngineError::new(
            codes::TILE_LAYOUT_STALE,
            format!(
                "{} was solved from different inputs (file {}, scene now {}); \
                 re-run `engine synthesize --write`",
                inputs.target.display(),
                layout.header.inputs_hash,
                digest
            ),
        )
        .entity(&inputs.name)
        .file(args.scene.display().to_string()));
    }

    println!(
        "{}",
        serde_json::json!({
            "scene": args.scene.display().to_string(),
            "entity": inputs.name,
            "layout": inputs.target.display().to_string(),
            "current": true,
            "cells": layout.cells.len(),
            "locked": layout.cells.iter().filter(|c| c.locked).count(),
            "fallbacks": layout.header.fallbacks,
            "inputs_hash": digest,
        })
    );
    Ok(())
}

/// `engine list-tiles <tileset.json>` — what a tileset actually contains
/// (M47).
///
/// The `list-*` family's job on a file kind whose most important property is
/// invisible: which tiles may sit against which. A tileset is written from a
/// prompt, and the socket graph is the part that comes out wrong — so this
/// prints the expansion (one entry per tile per rotation, in the order the
/// solver indexes them), the six resolved sockets, and **how many tiles may sit
/// across each face**. A `0` there is a tile that can never be placed that
/// side, and it is the difference between "the village came out empty" and a
/// one-character fix.
fn list_tiles(path: PathBuf) -> Result<()> {
    let tileset = engine_core::tileset::load_tileset(&path)?;
    let expanded = engine_core::tileset::expand(&tileset)?;
    let compat = engine_core::tileset::Compat::build(&expanded);

    let tiles: Vec<serde_json::Value> = expanded
        .iter()
        .enumerate()
        .map(|(index, tile)| {
            use engine_core::tileset::Face;
            let faces: serde_json::Map<String, serde_json::Value> = Face::ALL
                .into_iter()
                .map(|face| {
                    let socket = &tile.faces[face.index()];
                    (
                        Face::KEYS[face.index()].to_string(),
                        serde_json::json!({
                            "socket": socket.socket.name(),
                            "turn": socket.turn,
                            "partners": compat.partners(face.index(), index),
                        }),
                    )
                })
                .collect();
            serde_json::json!({
                "index": index,
                "token": tile.token,
                "name": tileset.tiles[tile.tile].name,
                "rotation": tile.rotation,
                "weight": tile.weight,
                "parts": tileset.tiles[tile.tile].parts.len(),
                "vertices": engine_core::tileset::tile_vertex_count(&tileset, tile),
                "faces": faces,
            })
        })
        .collect();

    println!(
        "{}",
        serde_json::json!({
            "tileset": path.display().to_string(),
            "cell": tileset.cell.to_array(),
            "palette": tileset.palette.keys().collect::<Vec<_>>(),
            "authored": tileset.tiles.len(),
            "expanded": expanded.len(),
            "tiles": tiles,
        })
    );
    Ok(())
}

/// `engine list-components [--component NAME]` — the component vocabulary
/// (M24).
///
/// Without the flag this is byte-identical to what it always printed: the
/// checked-in `schemas/component-schema.json` is that output, a repo-contract
/// test enforces it, and the validation walk and the editor's widget generator
/// both read the same document.
fn list_components(component: Option<String>, markdown: bool) -> Result<()> {
    if markdown {
        print!("{}", engine_core::schema::component_reference());
        return Ok(());
    }

    let Some(name) = component else {
        print!("{}", engine_core::schema::canonical_json());
        return Ok(());
    };

    let schema = engine_core::schema::component_schema_named(&name).ok_or_else(|| {
        EngineError::new(
            codes::UNKNOWN_COMPONENT_QUERY,
            format!("no component named {name:?}"),
        )
        .component(&name)
        .suggest_from(
            &name,
            engine_core::components::ComponentData::NAMES
                .iter()
                .copied(),
        )
    })?;

    println!(
        "{}",
        serde_json::to_string_pretty(&schema).map_err(|e| {
            EngineError::new(
                codes::OUTPUT_SERIALIZATION_FAILED,
                format!("could not serialize the component schema: {e}"),
            )
        })?
    );
    Ok(())
}

/// `engine terrain-height <scene> --at x,z [--entity NAME]` — where the ground
/// is (M24).
///
/// Placement is the most common operation on terrain, and until this the only
/// route from outside a script was a downward `raycast`: it needs the trick,
/// and it needs the patch to carry a `Collider`, so a patch authored for looks
/// could not be asked at all. This asks the height *field*, which is the thing
/// that decides where a tree's roots go.
///
/// Goes through [`Scene::terrain_height`], which goes through
/// `terrain::world_height_at`, which is what `world.terrain_height` answers
/// with — one sampler, per M22's one-implementation rule.
fn terrain_height(scene_path: PathBuf, at: String, entity: Option<String>) -> Result<()> {
    let (x, z) = parse_xz(&at)?;
    let scene = load_scene(&scene_path)?;

    let name = sole_entity_with::<engine_core::components::Terrain>(
        &scene,
        &scene_path,
        &entity,
        "Terrain",
        "terrain patches",
    )?;

    let height = scene
        .terrain_height(&name, x, z)
        .expect("the name came from a Terrain query");

    println!(
        "{}",
        serde_json::json!({
            "entity": name,
            "x": x,
            "z": z,
            "height": height,
        })
    );
    Ok(())
}

/// Which entity a "where is the surface" query is about: the one the caller
/// named, or the only candidate, and an error rather than a guess.
///
/// The `road-centerline` convention, factored out when `water-height` became
/// the third command to want it. Naming the candidates in the ambiguous case is
/// the part worth keeping — an agent that has to re-read the scene file to find
/// out what it could have written has been sent back to the picture.
fn sole_entity_with<C: engine_core::hecs::Component>(
    scene: &engine_core::Scene,
    scene_path: &Path,
    requested: &Option<String>,
    component: &str,
    plural: &str,
) -> Result<String> {
    let mut candidates: Vec<String> = scene
        .names()
        .filter(|name| {
            scene
                .entity(name)
                .is_some_and(|entity| scene.world.get::<&C>(entity).is_ok())
        })
        .map(str::to_string)
        .collect();
    candidates.sort();

    match (requested, candidates.len()) {
        (Some(requested), _) => candidates
            .iter()
            .find(|candidate| *candidate == requested)
            .cloned()
            .ok_or_else(|| {
                EngineError::new(
                    codes::ENTITY_NOT_FOUND,
                    format!("no entity named {requested:?} with a {component} component"),
                )
                .entity(requested)
                .file(scene_path.display().to_string())
                .suggest_from(requested, candidates.iter().map(String::as_str))
            }),
        (None, 1) => Ok(candidates.remove(0)),
        (None, 0) => Err(EngineError::new(
            codes::MISSING_COMPONENT,
            format!("scene has no entity with a {component} component"),
        )
        .file(scene_path.display().to_string())),
        (None, count) => Err(EngineError::new(
            codes::MISSING_COMPONENT,
            format!(
                "scene has {count} {plural} ({}); name one with --entity",
                candidates.join(", ")
            ),
        )
        .file(scene_path.display().to_string())),
    }
}

/// Ask a water surface where it is at a world XZ position (M41).
///
/// `terrain-height`'s twin, and different from it in exactly the two ways water
/// is different from ground. It takes a **clock**, because the surface moves —
/// resolved by `scene_time`, so the number printed is the number the render at
/// that frame drew and the number a buoyant body felt. And it can answer *no
/// water here*, because a patch is a bounded body: a column past the edge
/// reports `"water": false` with no height rather than a confident 0.0.
///
/// The normal rides along because it costs nothing — it falls out of the same
/// derivatives — and because a script sitting a boat *on* a wave needs it, so
/// asking for it later would mean a second command.
fn water_height(
    scene_path: PathBuf,
    at: String,
    entity: Option<String>,
    time: f32,
    steps: u32,
) -> Result<()> {
    let (x, z) = parse_xz(&at)?;
    let scene = load_scene(&scene_path)?;
    let name = sole_entity_with::<engine_core::components::Water>(
        &scene,
        &scene_path,
        &entity,
        "Water",
        "water surfaces",
    )?;
    let seconds = scene_time(time, steps, &scene);

    let report = match scene.water_sample(&name, x, z, seconds) {
        Some(sample) => serde_json::json!({
            "entity": name,
            "x": x,
            "z": z,
            "time": seconds,
            "water": true,
            "height": sample.height,
            "normal": [sample.normal.x, sample.normal.y, sample.normal.z],
        }),
        None => serde_json::json!({
            "entity": name,
            "x": x,
            "z": z,
            "time": seconds,
            "water": false,
        }),
    };
    println!("{report}");
    Ok(())
}

/// An entity's `Transform`, or the default when it has none.
///
/// Inlined here rather than reaching for `Scene`'s private accessor: a volume
/// with no `Transform` is already a validation error, so the default is only
/// ever reached on a scene that was told it is broken.
fn transform_of(
    scene: &Scene,
    entity: engine_core::hecs::Entity,
) -> engine_core::components::Transform {
    scene
        .world
        .get::<&engine_core::components::Transform>(entity)
        .map(|t| *t)
        .unwrap_or_default()
}

/// The scene's irradiance field for one frame, or `None` when it has no volume.
///
/// A one-line wrapper so that every render path in this file resolves GI the
/// same way. Three of them do — `screenshot`, `diff-render` and `filmstrip` —
/// and a fourth that folded the field differently is exactly how a baseline and
/// the picture it pins start to disagree.
fn gi_field(
    scene: &Scene,
    scene_path: &Path,
    lights: &engine_core::scene::ResolvedLights,
    environment: &engine_core::scene::EnvironmentSettings,
) -> Option<engine_core::gi::IrradianceField> {
    let base = scene_path.parent().unwrap_or(Path::new(""));
    engine_core::gi::evaluate::field_for_scene(scene, base, lights, environment)
}

/// The scene's `LightProbeVolume`, or `None` when it has none.
///
/// A scene may hold at most one — `multiple_light_probe_volumes` since the M35
/// decisions — so this is a lookup rather than a resolution rule. `None` is not
/// an error here: `gi-probe` answers "outside every volume" for a scene without
/// one, which is a truthful answer and more useful than a refusal.
fn probe_volume(scene: &Scene) -> Option<String> {
    scene
        .names()
        .find(|name| {
            scene.entity(name).is_some_and(|entity| {
                scene
                    .world
                    .get::<&engine_core::components::LightProbeVolume>(entity)
                    .is_ok()
            })
        })
        .map(str::to_string)
}

/// `engine bake-gi <scene> [--entity NAME] [--out PATH] [--samples N] [--check]`
/// (M35).
///
/// Writes beside the scene by default, because asset paths resolve relative to
/// the scene file and a bake written to `/tmp` is a bake the scene cannot load
/// — the trap CLAUDE.md records for `simulate --bake`.
///
/// `--check` is the same command with the writing taken out: it collects the
/// occluders, recomputes the digest, and compares. That is the whole
/// geometry-level staleness check, and it lives here rather than in `validate`
/// because it needs the scene's entire triangle set — 0.86s for the showcase
/// tour against `validate`'s 0.17s, five times the fast gate on the largest
/// scene in the repo and rising with every triangle added to it.
fn bake_gi(
    scene_path: PathBuf,
    entity: Option<String>,
    out: Option<PathBuf>,
    samples: Option<u32>,
    check: bool,
) -> Result<()> {
    if check && out.is_some() {
        return Err(EngineError::new(
            codes::INVALID_INVOCATION,
            "--check writes nothing, so --out has nothing to name",
        )
        .file(scene_path.display().to_string()));
    }

    let scene = load_scene_for_bake(&scene_path)?;
    let base_dir = scene_path.parent().unwrap_or(Path::new("")).to_path_buf();
    let name = sole_entity_with::<engine_core::components::LightProbeVolume>(
        &scene,
        &scene_path,
        &entity,
        "LightProbeVolume",
        "light probe volumes",
    )?;

    let handle = scene.entity(&name).expect("named above");
    let volume = scene
        .world
        .get::<&engine_core::components::LightProbeVolume>(handle)
        .expect("filtered above")
        .clone();
    let transform = transform_of(&scene, handle);
    let target = match &out {
        Some(path) => path.clone(),
        None => base_dir.join(&volume.bake),
    };

    let assets = engine_assets::AssetServer::for_scene(&scene_path);
    let tris = engine_core::gi::bake::collect_occluders(&scene, &assets)?;
    // Which suns this bake covers (M45) — the scene's arc, or its one static
    // direction, or none at all. Resolved from the scene rather than from the
    // component, because a `daylight` block is what turns one direction into a
    // set.
    let sun = engine_core::gi::sun_directions(&scene, &volume);

    if check {
        return check_bake(
            &scene_path,
            &name,
            &volume,
            &transform,
            &target,
            &tris,
            &sun,
            samples,
        );
    }

    let params = engine_core::gi::bake::BakeParams {
        samples: samples.unwrap_or(engine_core::gi::bake::DEFAULT_SAMPLES),
        bounces: volume.bounces,
        sun,
    };

    let (baked, stats) = engine_core::gi::bake::bake(
        &scene_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        &name,
        tris,
        transform.position,
        transform.scale,
        &volume,
        &params,
    );

    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            EngineError::new(
                codes::SCENE_WRITE_FAILED,
                format!("could not create {}: {e}", parent.display()),
            )
        })?;
    }
    std::fs::write(&target, baked.to_text()).map_err(|e| {
        EngineError::new(
            codes::SCENE_WRITE_FAILED,
            format!("could not write {}: {e}", target.display()),
        )
    })?;

    let report = serde_json::json!({
        "entity": name,
        "out": target.display().to_string(),
        "grid": baked.header.grid,
        "probes": stats.probes,
        "rays": stats.rays,
        "triangles": stats.triangles,
        "relocated": stats.relocated,
        "samples": params.samples,
        "bounces": params.bounces,
        "sun_directions": params.sun.len(),
        "inputs_hash": baked.header.inputs_hash,
    });
    println!("{}", serde_json::json!({ "baked": [report] }));
    Ok(())
}

/// `bake-gi --check`: recompute the digest and compare, writing nothing.
///
/// The samples count comes from the **file** unless the caller names one,
/// because rays per probe is a bake-time choice and a bake taken at 512 is not
/// stale merely for having been taken at 512. Everything else is read from the
/// scene as it stands now — spacing, bounces, the grid the transform implies,
/// and the triangles — so every edit that would change the light is caught.
///
/// Eight arguments because a staleness check is a comparison against eight
/// independent inputs; bundling them into a struct that exists for one call
/// site would hide which ones the digest actually reads.
#[allow(clippy::too_many_arguments)]
fn check_bake(
    scene_path: &Path,
    name: &str,
    volume: &engine_core::components::LightProbeVolume,
    transform: &engine_core::components::Transform,
    target: &Path,
    tris: &[engine_core::gi::bake::Triangle],
    sun: &[engine_core::math::Vec3],
    samples: Option<u32>,
) -> Result<()> {
    let text = std::fs::read_to_string(target).map_err(|e| {
        EngineError::new(
            codes::GI_BAKE_MISSING,
            format!(
                "could not read {}: {e}; run `engine bake-gi`",
                target.display()
            ),
        )
        .entity(name)
        .file(scene_path.display().to_string())
    })?;
    let baked = engine_core::gi::BakedGi::parse(&text).map_err(|bad| {
        EngineError::new(
            codes::GI_BAKE_MALFORMED,
            format!("{}: {bad}", target.display()),
        )
        .entity(name)
        .file(scene_path.display().to_string())
    })?;

    let params = engine_core::gi::bake::BakeParams {
        samples: samples.unwrap_or(baked.header.samples),
        bounces: volume.bounces,
        sun: sun.to_vec(),
    };
    let grid = engine_core::gi::grid_counts(transform.scale, volume.spacing);
    let current = engine_core::gi::bake::hash_inputs(tris, &params, volume.spacing, grid);

    if current != baked.header.inputs_hash {
        return Err(EngineError::new(
            codes::GI_BAKE_STALE,
            format!(
                "{} was baked from different inputs (file {}, scene now {}); \
                 re-run `engine bake-gi`",
                target.display(),
                baked.header.inputs_hash,
                current
            ),
        )
        .entity(name)
        .file(scene_path.display().to_string()));
    }

    println!(
        "{}",
        serde_json::json!({
            "entity": name,
            "bake": target.display().to_string(),
            "current": true,
            "triangles": tris.len(),
            "inputs_hash": current,
        })
    );
    Ok(())
}

/// `engine gi-probe <scene> --at x,y,z [--normal x,y,z]` (M35).
fn gi_probe(
    scene_path: PathBuf,
    at: String,
    normal: Option<String>,
    time: Option<f32>,
) -> Result<()> {
    let point = parse_vec3_arg(&at, "at")?;
    let normal = match &normal {
        Some(text) => parse_vec3_arg(text, "normal")?.normalize_or_zero(),
        None => engine_core::math::Vec3::Y,
    };

    let scene = load_scene(&scene_path)?;
    let base_dir = scene_path.parent().unwrap_or(Path::new("")).to_path_buf();

    // The scene's one volume, and only if the point is inside it. The bounds
    // test is what stays interesting now that there is nothing to resolve
    // between: "outside" is the case an agent hits when a probe reads like the
    // fallback and it wants to know why.
    let best = probe_volume(&scene).and_then(|name| {
        let handle = scene.entity(&name).expect("listed above");
        let volume = (*scene
            .world
            .get::<&engine_core::components::LightProbeVolume>(handle)
            .expect("filtered above"))
        .clone();
        let transform = transform_of(&scene, handle);
        let half = transform.scale * 0.5;
        let min = transform.position - half;
        let max = transform.position + half;
        if point.cmplt(min).any() || point.cmpgt(max).any() {
            return None;
        }
        Some((name, volume, transform))
    });

    let Some((name, volume, transform)) = best else {
        // Outside every volume is not an error: it is the documented fallback,
        // and saying so is more useful than refusing to answer.
        println!(
            "{}",
            serde_json::json!({
                "at": point.to_array(),
                "normal": normal.to_array(),
                "volume": serde_json::Value::Null,
                "note": "outside every LightProbeVolume; the renderer falls back to sky_ambient",
            })
        );
        return Ok(());
    };

    let file = base_dir.join(&volume.bake);
    let text = std::fs::read_to_string(&file).map_err(|e| {
        EngineError::new(
            codes::GI_BAKE_MISSING,
            format!(
                "could not read {}: {e}; run `engine bake-gi`",
                file.display()
            ),
        )
        .entity(&name)
        .file(scene_path.display().to_string())
    })?;
    let baked = engine_core::gi::BakedGi::parse(&text).map_err(|bad| {
        EngineError::new(
            codes::GI_BAKE_MALFORMED,
            format!("{}: {bad}", file.display()),
        )
        .entity(&name)
        .file(scene_path.display().to_string())
    })?;

    let origin = transform.position - transform.scale * 0.5;
    let cell = ((point - origin) / volume.spacing).max(engine_core::math::Vec3::ZERO);

    // The same fold the renderer runs, against the same clock — this is the
    // number the shader would read, not a second model of it. `--time` picks the
    // sky, because under `daylight` the answer moves with it: that is what
    // baking transfer rather than radiance buys.
    let (lights, environment) = scene.resolved_at(time.unwrap_or(0.0));
    let field = engine_core::gi::evaluate(&baked, &volume, &lights, &environment);
    let irradiance = field.sample(point, normal);
    let fallback = fallback_fill(normal, &lights, &environment);
    let weight = field.weight(point);

    // How much of that came off a sunlit surface (M45), by folding the same
    // bake a second time with the sun basis struck out and subtracting. A
    // difference rather than a separate evaluation path, so the number reported
    // is provably the one in `irradiance` and cannot drift from it — the
    // question an author asks here is "is this pink because of the red wall",
    // and a second model of the fold could answer it wrongly while agreeing
    // everywhere else.
    let sun_bounce = if baked.header.sun_dirs.is_empty() {
        engine_core::math::Vec3::ZERO
    } else {
        let mut without = baked.clone();
        without.header.sun_dirs.clear();
        for probe in &mut without.probes {
            probe.sun.clear();
        }
        irradiance
            - engine_core::gi::evaluate(&without, &volume, &lights, &environment)
                .sample(point, normal)
    };

    println!(
        "{}",
        serde_json::json!({
            "at": point.to_array(),
            "normal": normal.to_array(),
            "volume": name,
            "bake": volume.bake,
            "grid": baked.header.grid,
            "origin": baked.header.origin,
            "spacing": volume.spacing,
            "intensity": volume.intensity,
            "cell": cell.to_array(),
            "relocated_probes": baked.header.relocated,
            "samples": baked.header.samples,
            // What the renderer uses here, and what it would have used
            // without GI. Two numbers rather than one, because "is this
            // dark" is only answerable against what it is being compared to.
            "irradiance": irradiance.to_array(),
            "fallback": fallback.to_array(),
            // The share of `irradiance` that bounced off a sunlit surface, and
            // how many sun directions the bake covers. Zero on a bake with no
            // sun basis, and zero at night whatever the bake holds.
            "sun_bounce": sun_bounce.to_array(),
            "sun_directions": baked.header.sun_dirs.len(),
            // `intensity` faded at the volume's edge; the renderer mixes the
            // two above by exactly this.
            "weight": weight,
            "openness": field.openness(point),
        })
    );
    Ok(())
}

/// The fill a fragment would receive with no GI — `sky_ambient(n)` when the
/// scene draws a sky, the flat `AmbientLight` when it does not.
///
/// Transcribed from `mesh.wgsl` rather than shared with it, which is the same
/// bargain `list-colliders` and `terrain-height` make: the shader's copy is the
/// one that draws and this one is what a report can print. The equality between
/// them is what `an_unoccluded_probe_reproduces_sky_ambient` pins.
fn fallback_fill(
    normal: engine_core::math::Vec3,
    lights: &engine_core::scene::ResolvedLights,
    environment: &engine_core::scene::EnvironmentSettings,
) -> engine_core::math::Vec3 {
    if !environment.sky {
        return lights.ambient;
    }
    let up = normal.y * 0.5 + 0.5;
    let env = environment.sky_ground + (environment.sky_zenith - environment.sky_ground) * up;
    let mean = ((environment.sky_ground + environment.sky_zenith) * 0.5)
        .max(engine_core::math::Vec3::splat(1.0e-4));
    lights.ambient * (env / mean)
}

/// `--at x,y,z` / `--normal x,y,z`: three numbers, unlike `terrain-height`'s
/// two — a light field is a function of position *and* direction.
fn parse_vec3_arg(text: &str, flag: &str) -> Result<engine_core::math::Vec3> {
    let parts: Vec<f32> = text
        .split(',')
        .map(|part| part.trim().parse::<f32>())
        .collect::<std::result::Result<_, _>>()
        .map_err(|e| {
            EngineError::new(
                codes::INVALID_INVOCATION,
                format!("--{flag} expected x,y,z numbers, got {text:?} ({e})"),
            )
        })?;
    if parts.len() != 3 {
        return Err(EngineError::new(
            codes::INVALID_INVOCATION,
            format!("--{flag} expected exactly three comma-separated numbers, got {text:?}"),
        ));
    }
    Ok(engine_core::math::Vec3::new(parts[0], parts[1], parts[2]))
}

/// `--at x,z`: two numbers, unlike `raycast`'s three. The height is what is
/// being asked for, so passing a Y would be passing an answer in.
fn parse_xz(text: &str) -> Result<(f32, f32)> {
    let parts: Vec<f32> = text
        .split(',')
        .map(|part| part.trim().parse::<f32>())
        .collect::<std::result::Result<_, _>>()
        .map_err(|e| {
            EngineError::new(
                codes::INVALID_INVOCATION,
                format!("expected x,z numbers, got {text:?} ({e})"),
            )
        })?;
    if parts.len() != 2 {
        return Err(EngineError::new(
            codes::INVALID_INVOCATION,
            format!(
                "expected exactly two comma-separated numbers (x,z), got {text:?}; \
                 the height is what terrain-height answers"
            ),
        ));
    }
    Ok((parts[0], parts[1]))
}

/// `engine inspect <scene> [--entity NAME]` — the entity as the engine holds
/// it (M24).
///
/// The components come back through `ComponentData::collect_from` and are
/// serialized by the same serde impls that read them, so every default is the
/// one the engine is actually using. Re-deriving defaults here is how `inspect`
/// would start describing a scene the renderer does not have.
///
/// A pure function of the file at rest — no `--steps`. "What did you author"
/// and "what happened when it ran" are different questions, and `simulate` owns
/// the second one.
/// `engine import` — a glTF model's materials, as files the engine reads.
fn import(
    model: PathBuf,
    into: Option<PathBuf>,
    textures: String,
    materials: String,
) -> Result<()> {
    if !model.is_file() {
        return Err(EngineError::new(
            codes::ASSET_NOT_FOUND,
            format!("no model file at {}", model.display()),
        )
        .file(model.display().to_string()));
    }

    // Paths come out relative to whatever will reference them: the scene when
    // there is one, the model's own directory otherwise. That is the same rule
    // every other asset reference in the engine follows.
    let root = match &into {
        Some(scene) => scene.parent().unwrap_or(Path::new("")).to_path_buf(),
        None => model.parent().unwrap_or(Path::new("")).to_path_buf(),
    };
    let imported = engine_assets::import_materials(&model, &root, &textures, &materials)?;

    for warning in &imported.warnings {
        EngineError::new(codes::IMPORT_FAILED, warning.clone())
            .warning()
            .emit();
    }

    // The model itself, as the scene would reference it: relative to the scene,
    // like every other asset. An import that leaves the model where it is (this
    // one) does not copy it — the editor's drag-and-drop is what copies.
    let mesh_asset = match &into {
        Some(scene) => relative_to(&model, scene.parent().unwrap_or(Path::new("")))
            .unwrap_or_else(|| model.display().to_string()),
        None => model
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| model.display().to_string()),
    };

    let entity_name = model
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Imported".to_string());

    // A model outside the scene's own tree still resolves — the reference is
    // relative and validation accepts it — but it is a path that breaks the
    // moment the project moves, which is the thing invariant 3 is protecting.
    let mut warnings = imported.warnings.clone();
    if mesh_asset.starts_with("..") {
        let warning = format!(
            "{mesh_asset:?} climbs out of the scene's directory; copy the model \
             under the project to keep the scene portable"
        );
        EngineError::new(codes::IMPORT_FAILED, warning.clone())
            .warning()
            .emit();
        warnings.push(warning);
    }

    let mut entity = None;
    if let Some(scene) = &into {
        let source = std::fs::read_to_string(scene).map_err(|e| {
            EngineError::new(
                codes::SCENE_UNREADABLE,
                format!("could not read {}: {e}", scene.display()),
            )
            .file(scene.display().to_string())
        })?;

        let mut components = vec![
            ("Transform".to_string(), vec![]),
            (
                "Mesh".to_string(),
                vec![(
                    "asset".to_string(),
                    serde_json::Value::String(mesh_asset.clone()),
                )],
            ),
        ];
        // The first material, because one entity draws one mesh with one
        // material and a glTF file with several is a scene the importer cannot
        // invent an entity split for. The rest are on disk to be referenced by
        // hand — which is the whole reason they are files.
        if let Some(first) = imported.materials.first() {
            components.push((
                "Material".to_string(),
                vec![(
                    "asset".to_string(),
                    serde_json::Value::String(first.clone()),
                )],
            ));
        }

        // Re-importing refreshes the files and leaves the scene alone, rather
        // than failing on the duplicate name or quietly adding a second copy.
        // That is what makes `engine import` safe to run again after the model
        // changes, which is the case an agent actually hits.
        let exists = serde_json::from_str::<serde_json::Value>(&source)
            .ok()
            .and_then(|v| v["entities"].as_array().cloned())
            .is_some_and(|entities| entities.iter().any(|e| e["name"] == entity_name.as_str()));
        if exists {
            let warning = format!(
                "{} already has an entity named {entity_name:?}; its material and \
                 texture files were refreshed and the scene left alone. Copy the \
                 entity to place a second one.",
                scene.display()
            );
            EngineError::new(codes::IMPORT_FAILED, warning.clone())
                .warning()
                .emit();
            warnings.push(warning);
        } else {
            let edit = engine_core::formatter::AddEntity {
                name: entity_name.clone(),
                components,
            };
            let updated = engine_core::formatter::apply_add_entity(&source, &edit)?;
            // Atomic like every other scene write: the editor may be watching
            // this file, and a rename never shows it a half-written scene.
            engine_core::formatter::write_atomic(scene, &updated)?;
            entity = Some(entity_name);
        }
    }

    let report = serde_json::json!({
        "model": model.display().to_string(),
        "materials": imported.materials,
        "textures": imported.textures,
        "entity": entity,
        "warnings": warnings.len(),
    });
    println!("{report}");
    Ok(())
}

/// `path` as seen from `base`, when one is inside the other.
///
/// Deliberately lexical and deliberately narrow: it walks up from `base` only
/// as far as a shared prefix, and gives up rather than guessing when the two
/// have none. A path that cannot be made relative is reported as it was given,
/// which validation will then reject as absolute — a legible failure rather
/// than a silently wrong reference.
fn relative_to(path: &Path, base: &Path) -> Option<String> {
    let path = path.canonicalize().ok()?;
    let base = base
        .canonicalize()
        .ok()
        .or_else(|| std::env::current_dir().ok())?;
    let mut up = Vec::new();
    let mut candidate = base.as_path();
    loop {
        if let Ok(rest) = path.strip_prefix(candidate) {
            let mut out = up.join("/");
            if !out.is_empty() {
                out.push('/');
            }
            out.push_str(&rest.to_string_lossy());
            return Some(out);
        }
        candidate = candidate.parent()?;
        up.push("..");
    }
}

fn inspect(scene_path: PathBuf, entity: Option<String>) -> Result<()> {
    let scene = load_scene(&scene_path)?;

    let mut names: Vec<String> = scene.names().map(str::to_string).collect();
    names.sort();

    if let Some(requested) = &entity {
        if !names.iter().any(|name| name == requested) {
            return Err(EngineError::new(
                codes::ENTITY_NOT_FOUND,
                format!("no entity named {requested:?}"),
            )
            .entity(requested)
            .file(scene_path.display().to_string())
            .suggest_from(requested, names.iter().map(String::as_str)));
        }
        names.retain(|name| name == requested);
    }

    let entities: Vec<serde_json::Value> = names
        .iter()
        .map(|name| {
            let handle = scene.entity(name).expect("the name came from the scene");
            let components =
                engine_core::components::ComponentData::collect_from(&scene.world, handle);
            let transform = components
                .iter()
                .find_map(|component| match component {
                    engine_core::components::ComponentData::Transform(t) => Some(*t),
                    _ => None,
                })
                .unwrap_or_default();
            serde_json::json!({
                "name": name,
                // There is no parenting in this engine, so an entity's world
                // transform is its own — reported anyway, and filled with the
                // identity for an entity that carries no Transform at all,
                // because that is the placement everything downstream uses.
                "transform": {
                    "position": [transform.position.x, transform.position.y, transform.position.z],
                    "rotation": [transform.rotation.x, transform.rotation.y, transform.rotation.z],
                    "scale": [transform.scale.x, transform.scale.y, transform.scale.z],
                },
                "components": components,
            })
        })
        .collect();

    // What the scene can spawn (M37), resolved the same way: absent fields are
    // the documented defaults, and the point of `inspect` is that you see them
    // rather than infer them. Reported only when the scene has templates, and
    // suppressed entirely under `--entity`, which asks about one *entity* —
    // a template is not one, and answering with both would make the report's
    // shape depend on something the question did not mention.
    let templates: Vec<serde_json::Value> = if entity.is_some() {
        Vec::new()
    } else {
        let mut templates: Vec<&engine_core::scene::TemplateDef> = scene.templates.iter().collect();
        templates.sort_by(|a, b| a.name.cmp(&b.name));
        templates
            .into_iter()
            .map(|template| {
                serde_json::json!({
                    "name": template.name,
                    "limit": template.limit,
                    "components": template.components,
                })
            })
            .collect()
    };

    // Compact, like every other report — `raycast`, `road-centerline`,
    // `simulate`. Only the schema commands pretty-print, because a schema is
    // read and a report is piped through `jq`.
    let mut report = serde_json::json!({
        "scene": scene.name,
        "entities": entities,
    });
    if !templates.is_empty() {
        report["templates"] = serde_json::Value::Array(templates);
    }
    println!("{report}");
    Ok(())
}

/// `engine diff-render` — render the scene at the baseline's dimensions and
/// compare. The report goes to stdout on *both* pass and fail (a documented
/// exception to "nothing on stdout on failure": a failing run still tells
/// the agent exactly how much differs and where); on mismatch, additionally
/// `render_mismatch` on stderr and exit 1.
#[allow(clippy::too_many_arguments)]
fn diff_render(
    scene_path: PathBuf,
    baseline_path: PathBuf,
    steps: u32,
    input_path: Option<PathBuf>,
    time: f32,
    out: Option<PathBuf>,
    camera_name: Option<&str>,
    threshold: u8,
    max_diff_percent: f64,
) -> Result<()> {
    // Baseline first: it is cheap, needs no GPU, and defines the render size.
    let baseline = load_baseline(&baseline_path)?;
    let input = simulate::load_input(input_path.as_deref())?;

    let mut scene = load_scene(&scene_path)?;
    let players = engine_core::animation::load_players(&scene, &scene_path)?;
    if !players.is_empty() {
        engine_core::animation::apply_all(&mut scene, &players, time);
    }
    let (particles, hud, interaction) = if steps > 0 {
        // The cursor is a fraction of the frame, and the frame here is the
        // baseline's own dimensions — the same ones this render uses, so a
        // mouse-driven fixture is pinned at the size it was blessed at.
        let view = engine_core::input::Viewport::new(baseline.width, baseline.height, camera_name);
        let outcome = simulate::run(&mut scene, &scene_path, steps, input.as_ref(), &view, None)?;
        (
            outcome.particles.instances(&scene.world),
            outcome.hud,
            outcome.interaction,
        )
    } else {
        (
            Vec::new(),
            Vec::new(),
            engine_core::ui::Interaction::default(),
        )
    };
    let (camera, camera_transform) = scene.camera(camera_name)?;
    let assets = engine_assets::AssetServer::for_scene(&scene_path);
    let render_time = scene_time(time, steps, &scene);
    let items = scene.render_items_at(&assets, Some(render_time))?;
    let (lights, environment) = scene.resolved_at(render_time);
    let gi = gi_field(&scene, &scene_path, &lights, &environment);

    let (actual, adapter) = engine_render::offscreen::render_with_adapter(
        &items,
        &scene.water_items(),
        &scene.cloud_items(),
        &scene.road_items(),
        &scene.meadow_items(),
        &particles,
        &camera,
        camera_transform.matrix(),
        lights,
        environment,
        render_time,
        baseline.width,
        baseline.height,
        &tinted_hud(&scene, &assets, &interaction),
        &hud,
        gi.as_ref(),
    )?;

    let (stats, diff_image) = engine_render::diff::diff(&actual, &baseline, threshold)?;

    if let Some(out) = &out {
        // Written on pass too: an all-faded image is itself legible
        // confirmation that nothing moved.
        let png =
            image::RgbaImage::from_raw(diff_image.width, diff_image.height, diff_image.pixels)
                .expect("diff() returns exactly width*height*4 bytes");
        png.save(out).map_err(|e| {
            EngineError::new(codes::PNG_WRITE_FAILED, format!("could not write PNG: {e}"))
                .file(out.display().to_string())
        })?;
    }

    let pass = stats.passes(max_diff_percent);
    let mut report = serde_json::json!({
        "pass": pass,
        "width": actual.width,
        "height": actual.height,
        "diff_pixels": stats.diff_pixels,
        "diff_percent": stats.diff_percent(),
        "max_channel_delta": stats.max_channel_delta,
        "threshold": threshold,
        "max_diff_percent": max_diff_percent,
        "adapter": adapter,
    });
    if let Some(bounds) = stats.bounds {
        report["diff_bounds"] = serde_json::json!({
            "min_x": bounds.min_x,
            "min_y": bounds.min_y,
            "max_x": bounds.max_x,
            "max_y": bounds.max_y,
        });
    }
    if let Some(out) = &out {
        report["diff_image"] = serde_json::json!(out.display().to_string());
    }
    println!("{report}");

    if pass {
        Ok(())
    } else {
        Err(EngineError::new(
            codes::RENDER_MISMATCH,
            format!(
                "{} of {} pixels ({:.3}%) differ from {} (threshold {threshold}, max allowed {max_diff_percent}%)",
                stats.diff_pixels,
                stats.total_pixels,
                stats.diff_percent(),
                baseline_path.display(),
            ),
        )
        .file(baseline_path.display().to_string()))
    }
}

/// Decode a baseline PNG into the comparison image format.
/// The scene clock in seconds, as water reads it (M18).
///
/// Water is the first thing in the engine that is a pure function of *time* and
/// yet belongs to the rendered frame rather than to a clip, so it needs one rule
/// covering both ways a command can say when "now" is:
///
/// - `--time T` names an instant directly, the way it does for animation. It
///   wins when given, so `screenshot --time 2.5` and `diff-render --time 2.5`
///   pin the same wave state down to identical bytes, and a `filmstrip` walking
///   time shows the waves moving.
/// - otherwise `--steps N` has advanced the fixed clock by `N/hz` seconds, and
///   the water is where that much simulated time put it — the same clock the
///   physics, the scripts and the particles in that frame ran on.
///
/// Neither is wall clock, and nothing else in the frame is: this is what keeps
/// a water render reproducible from the file plus its flags (invariant 2). A
/// command that passes both flags gets the explicit `--time`, which is the only
/// combination where the two answers differ.
fn scene_time(time: f32, steps: u32, scene: &engine_core::Scene) -> f32 {
    if time > 0.0 {
        return time;
    }
    steps as f32 / scene.physics.timestep_hz.max(1) as f32
}

/// The scene's overlay with the pointer's hover and press tints applied (M31).
///
/// Applied here, between extraction and the render, rather than inside the
/// rasterizer: the renderer has no business knowing what a pointer is, and
/// `hud::rasterize` stays a pure function of (tree, lines, size). With no
/// cursor over anything — every scene with no `HudInteract`, and every one
/// rendered at `--steps 0` — every tint is `[1, 1, 1]` and `apply_tints` is a
/// no-op, which is why no pre-M31 baseline can move through this path.
fn tinted_hud(
    scene: &engine_core::Scene,
    assets: &engine_assets::AssetServer,
    interaction: &engine_core::ui::Interaction,
) -> engine_core::ui::HudTree {
    let mut tree = scene.hud_tree(assets);
    interaction.apply_tints(&mut tree);
    tree
}

fn load_baseline(path: &std::path::Path) -> Result<engine_render::Image> {
    let display = path.display().to_string();

    let bytes = std::fs::read(path).map_err(|e| {
        // Not `asset_not_found`: baselines are not scene assets, and
        // overloading that code would muddy what it means to validation.
        let code = if e.kind() == std::io::ErrorKind::NotFound {
            codes::BASELINE_NOT_FOUND
        } else {
            codes::BASELINE_INVALID
        };
        EngineError::new(code, format!("could not read baseline {display}: {e}")).file(&display)
    })?;

    let decoded = image::load_from_memory(&bytes).map_err(|e| {
        EngineError::new(
            codes::BASELINE_INVALID,
            format!("could not decode baseline {display} as an image: {e}"),
        )
        .file(&display)
    })?;

    let rgba = decoded.to_rgba8();
    let (width, height) = rgba.dimensions();
    if width == 0 || height == 0 {
        return Err(EngineError::new(
            codes::BASELINE_INVALID,
            format!("baseline {display} has zero dimensions"),
        )
        .file(&display));
    }

    Ok(engine_render::Image {
        width,
        height,
        pixels: rgba.into_raw(),
    })
}

/// The render digest as it appears in a report (M25).
///
/// One shape for both commands, so an agent that learns to read
/// `screenshot`'s reads `filmstrip`'s. Deliberately *not* a pin: see
/// `engine_render::digest` for why the numbers are quantized and why there is
/// no hash here.
fn digest_json(digest: &engine_render::digest::Digest) -> serde_json::Value {
    serde_json::json!({
        "mean_luminance": digest.mean_luminance,
        "background": digest.background,
        "coverage": digest.coverage,
    })
}

/// `engine filmstrip` — one PNG, many moments.
#[allow(clippy::too_many_arguments)]
fn filmstrip(
    scene_path: PathBuf,
    out: PathBuf,
    start: f32,
    end: Option<f32>,
    frames: u32,
    columns: u32,
    tile_width: u32,
    tile_height: u32,
    camera_name: Option<&str>,
) -> Result<()> {
    let mut scene = load_scene(&scene_path)?;
    let players = engine_core::animation::load_players(&scene, &scene_path)?;
    let end = end
        .unwrap_or_else(|| start + engine_core::animation::longest_duration(&players).max(0.001));

    let frames = frames.max(1);
    let columns = columns.max(1).min(frames);
    let rows = frames.div_ceil(columns);
    let (camera, camera_transform) = scene.camera(camera_name)?;
    let assets = engine_assets::AssetServer::for_scene(&scene_path);

    let mut sheet = image::RgbaImage::new(tile_width * columns, tile_height * rows);
    for frame in 0..frames {
        let t = if frames == 1 {
            start
        } else {
            start + (end - start) * frame as f32 / (frames - 1) as f32
        };
        engine_core::animation::apply_all(&mut scene, &players, t);
        let items = scene.render_items_at(&assets, Some(t))?;
        let (lights, environment) = scene.resolved_at(t);
        // Filmstrip samples animation time only; particles advance with
        // --steps, which filmstrip does not take, so none are drawn. Water is
        // a pure function of time rather than of stepping, so it *does* move
        // across the strip — a filmstrip of a lake is a filmstrip of its waves.
        // Daylight is a pure function of time for the same reason, which is
        // what makes `--start 0 --end 24` a whole day on one sheet.
        let gi = gi_field(&scene, &scene_path, &lights, &environment);
        let rendered = engine_render::offscreen::render_with_adapter(
            &items,
            &scene.water_items(),
            &scene.cloud_items(),
            &scene.road_items(),
            &scene.meadow_items(),
            &[],
            &camera,
            camera_transform.matrix(),
            lights,
            environment,
            t,
            tile_width,
            tile_height,
            &scene.hud_tree(&assets),
            &[],
            gi.as_ref(),
        )
        .map(|(image, _)| image)?;
        let tile = image::RgbaImage::from_raw(rendered.width, rendered.height, rendered.pixels)
            .expect("offscreen render returns exactly width*height*4 bytes");
        let x = (frame % columns) * tile_width;
        let y = (frame / columns) * tile_height;
        image::imageops::replace(&mut sheet, &tile, i64::from(x), i64::from(y));
    }

    // The digest is of the whole contact sheet, which is what was written —
    // a strip whose frames are all black says so in one number.
    let digest = engine_render::digest::of(&engine_render::Image {
        width: sheet.width(),
        height: sheet.height(),
        pixels: sheet.as_raw().clone(),
    });

    sheet.save(&out).map_err(|e| {
        EngineError::new(codes::PNG_WRITE_FAILED, format!("could not write PNG: {e}"))
            .file(out.display().to_string())
    })?;

    println!(
        "{}",
        serde_json::json!({
            "written": out.display().to_string(),
            "digest": digest_json(&digest),
            "frames": frames,
            "start": start,
            "end": end,
            "columns": columns,
            "tile_width": tile_width,
            "tile_height": tile_height,
        })
    );
    Ok(())
}

/// `engine list-animations` — the introspection window (design principle 5).
fn list_animations(path: Option<PathBuf>, schema: bool) -> Result<()> {
    if schema {
        print!("{}", engine_core::schema::canonical_animation_json());
        return Ok(());
    }
    let Some(path) = path else {
        return Err(EngineError::new(
            codes::INVALID_INVOCATION,
            "list-animations needs a scene or clip file (or --schema)",
        ));
    };

    let display = path.display().to_string();

    // A glTF asked about directly (M30). Sniffing the extension rather than
    // the contents, because a `.glb` is not text and a `.gltf` is JSON that
    // would otherwise fall through to the scene parser.
    if engine_core::skeleton::is_gltf_path(&display) {
        let rig = engine_assets::load_rig(&path)?;
        println!(
            "{}",
            serde_json::json!({ "clips": gltf_clip_reports(&rig, &display) })
        );
        return Ok(());
    }

    let source = std::fs::read_to_string(&path).map_err(|e| {
        EngineError::new(codes::SCENE_UNREADABLE, format!("could not read: {e}")).file(&display)
    })?;
    let sniff: serde_json::Value = serde_json::from_str(&source).unwrap_or_default();

    let clip_report = |clip: &engine_core::animation::ClipFile, source_path: &str| {
        serde_json::json!({
            "name": clip.name,
            "source": source_path,
            // Named since M30, when a second kind arrived: a caller reading
            // `tracks` versus `channels` should not have to guess which.
            "kind": "property",
            "duration": engine_core::animation::duration(clip) as f64,
            "tracks": clip.tracks.iter().map(|t| serde_json::json!({
                "entity": t.entity,
                "property": t.property,
                "interpolation": format!("{:?}", t.interpolation).to_lowercase(),
                "keys": t.keys.len(),
            })).collect::<Vec<_>>(),
        })
    };

    let clips: Vec<serde_json::Value> = if sniff.get("tracks").is_some() {
        // A clip file directly.
        let clip: engine_core::animation::ClipFile =
            serde_json::from_str(&source).map_err(|e| {
                EngineError::new(
                    codes::ASSET_LOAD_FAILED,
                    format!("clip does not parse: {e}"),
                )
                .file(&display)
            })?;
        vec![clip_report(&clip, &display)]
    } else {
        // A scene: every player's clip, property and skeletal alike — one
        // command answers "what animates here" whichever kind it is.
        let scene = load_scene(&path)?;
        let players = engine_core::animation::load_players(&scene, &path)?;
        let mut clips: Vec<serde_json::Value> = players
            .iter()
            .map(|p| clip_report(&p.clip, &p.player.clip))
            .collect();

        let assets = engine_assets::AssetServer::for_scene(&path);
        for skinned in engine_core::skeleton::skinned_entities(&scene, &assets)? {
            let Some(clip) = skinned.selected_clip() else {
                continue;
            };
            clips.push(skeletal_clip_report(
                clip,
                &format!("{}#{}", skinned.asset, clip.name),
                Some(&skinned.name),
                skinned.rig.skin.as_ref(),
            ));
        }
        clips
    };

    println!("{}", serde_json::json!({ "clips": clips }));
    Ok(())
}

/// Every clip in a glTF, reported the way a property clip is — same shape, so
/// one `jq` expression reads both.
fn gltf_clip_reports(rig: &engine_core::skeleton::Rig, display: &str) -> Vec<serde_json::Value> {
    rig.clips
        .iter()
        .map(|clip| {
            skeletal_clip_report(
                clip,
                &format!("{display}#{}", clip.name),
                None,
                rig.skin.as_ref(),
            )
        })
        .collect()
}

/// One skeletal clip as JSON.
///
/// Each channel carries the `joint` it drives — **null when it targets a node
/// the skin does not use**, which is exactly the case glTF allows and sampling
/// ignores. An ignored channel nothing reports is invisible; an ignored
/// channel the CLI names is a fact about the asset. `sampled` is the same
/// judgement stated outright, so a caller need not re-derive it.
fn skeletal_clip_report(
    clip: &engine_core::skeleton::SkeletalClip,
    source: &str,
    entity: Option<&str>,
    skin: Option<&engine_core::skeleton::SkinData>,
) -> serde_json::Value {
    let channels: Vec<serde_json::Value> = clip
        .channels
        .iter()
        .map(|channel| {
            let joint = skin.and_then(|skin| skin.joint_of_node(channel.node));
            serde_json::json!({
                "node": channel.node,
                "node_name": channel.node_name,
                "joint": joint,
                "property": channel.property.as_str(),
                "interpolation": channel.interpolation.as_str(),
                "keys": channel.times.len(),
                "sampled": joint.is_some() && channel.is_sampleable(),
            })
        })
        .collect();

    let mut report = serde_json::json!({
        "name": clip.name,
        "source": source,
        "kind": "skeletal",
        "duration": engine_core::skeleton::duration(clip) as f64,
        "channels": channels,
    });
    if let Some(entity) = entity {
        report["entity"] = serde_json::Value::String(entity.to_string());
    }
    report
}

/// `engine list-joints` — the command that makes M30 agent-native rather than
/// merely present.
///
/// A filmstrip shows that *something* moved; it never shows that the hand
/// reached the doorknob. This does, and it needs no `Collider` and no GPU,
/// which is what separates it from every other way of asking where something
/// is.
fn list_joints(
    path: PathBuf,
    entity: Option<String>,
    time: Option<f32>,
    steps: u32,
    clip: Option<String>,
) -> Result<()> {
    let display = path.display().to_string();

    // A `.gltf`/`.glb` asked about directly reports the rig; a scene reports
    // every skinned entity's, or one with `--entity`.
    let rigs: Vec<serde_json::Value> = if engine_core::skeleton::is_gltf_path(&display) {
        let rig = engine_assets::load_rig(&path)?;
        let Some(skin) = rig.skin.as_ref() else {
            return Err(EngineError::new(
                codes::MESH_HAS_NO_SKIN,
                format!("glTF file {display:?} carries no skin"),
            )
            .file(&display));
        };
        let selected = match &clip {
            Some(name) => Some(rig.clip_named(name).ok_or_else(|| {
                EngineError::new(
                    codes::UNKNOWN_CLIP,
                    format!("glTF file {display:?} has no animation named {name:?}"),
                )
                .file(&display)
                .suggest_from(name, rig.clip_names())
            })?),
            None => None,
        };
        // No player, so scene time and clip time are the same thing — and no
        // scene, so no `FootPlant` and nothing to plant against.
        let globals = engine_core::skeleton::joint_globals(
            skin,
            selected.filter(|_| time.is_some()),
            time.unwrap_or(0.0),
        );
        vec![joint_report(
            &display,
            None,
            skin,
            selected,
            time,
            time,
            engine_core::components::Transform::default(),
            globals,
            None,
        )]
    } else {
        let mut scene = load_scene(&path)?;
        // Stepping first, when asked: a stride-driven player's phase lives in
        // the world the run leaves behind, so a report that never stepped
        // would describe the file rather than the run.
        let time = if steps > 0 {
            simulate::run(
                &mut scene,
                &path,
                steps,
                None,
                &engine_core::input::Viewport::DEFAULT,
                None,
            )?;
            Some(scene_time(time.unwrap_or(0.0), steps, &scene))
        } else {
            time
        };
        let assets = engine_assets::AssetServer::for_scene(&path);
        let skinned = engine_core::skeleton::skinned_entities(&scene, &assets)?;

        if let Some(wanted) = &entity {
            let found = skinned.iter().find(|s| &s.name == wanted).ok_or_else(|| {
                EngineError::new(
                    codes::UNKNOWN_ENTITY,
                    format!("no skinned entity named {wanted:?} in {display}"),
                )
                .file(&display)
                .entity(wanted)
                .suggest_from(wanted, skinned.iter().map(|s| s.name.as_str()))
            })?;
            vec![entity_joint_report(&scene, found, time)]
        } else {
            skinned
                .iter()
                .map(|found| entity_joint_report(&scene, found, time))
                .collect()
        }
    };

    println!("{}", serde_json::json!({ "rigs": rigs }));
    Ok(())
}

fn entity_joint_report(
    scene: &engine_core::Scene,
    skinned: &engine_core::skeleton::SkinnedEntity,
    time: Option<f32>,
) -> serde_json::Value {
    let skin = skinned
        .rig
        .skin
        .as_ref()
        .expect("skinned_entities filtered");
    let clip = skinned.selected_clip();
    // The player's speed, offset and looping map scene time onto clip time, so
    // `--time` here means what it means everywhere else — and the report
    // carries both, because "why is the pose the same at 0 and at 1" is
    // answered by the wrap, not by the joints.
    let clip_time = time.map(|t| skinned.local_time(t));
    // Through the same seam the renderer poses with (M32), so a planted foot
    // is reported where it is drawn.
    let globals = scene.posed_globals(
        skinned.entity,
        skin,
        clip.filter(|_| clip_time.is_some()),
        clip_time.unwrap_or(0.0),
    );

    // The stride this clip actually covers, when the entity names its feet.
    // This is the number `AnimationPlayer.stride` wants, and measuring it is
    // the alternative to tuning it against a filmstrip.
    let stride = clip.and_then(|clip| {
        let plant = scene.foot_plant_of(skinned.entity)?;
        let feet: Vec<usize> = plant
            .feet
            .iter()
            .filter_map(|foot| skin.joint_named(&foot.ankle))
            .collect();
        let metres = engine_core::locomotion::measure_stride(
            skin,
            clip,
            &feet,
            engine_core::locomotion::STRIDE_SAMPLES,
        )?;
        let names: Vec<&str> = plant.feet.iter().map(|f| f.ankle.as_str()).collect();
        Some(serde_json::json!({
            "measured": metres as f64,
            "feet": names,
            "samples": engine_core::locomotion::STRIDE_SAMPLES,
        }))
    });

    joint_report(
        &skinned.asset,
        Some(&skinned.name),
        skin,
        clip,
        time,
        clip_time,
        skinned.transform,
        globals,
        stride,
    )
}

/// One rig as JSON: the joints in the skin's own order, each with its index.
///
/// Order is the skin's, never sorted — a joint's index is written into the
/// vertex data, so it is a fact about the asset rather than a presentation
/// choice, and carrying `index` is how the report says so.
#[allow(clippy::too_many_arguments)]
fn joint_report(
    asset: &str,
    entity: Option<&str>,
    skin: &engine_core::skeleton::SkinData,
    clip: Option<&engine_core::skeleton::SkeletalClip>,
    time: Option<f32>,
    clip_time: Option<f32>,
    transform: engine_core::components::Transform,
    // Posed by the caller — without `--time`, the rest pose; with it, the pose
    // at that moment, planted when the entity asks for it (M32).
    globals: Vec<glam::Mat4>,
    stride: Option<serde_json::Value>,
) -> serde_json::Value {
    // glTF ignores the skinned mesh node's own transform, so the entity's
    // `Transform` is what puts the rig in the world — and world coordinates
    // are what a caller assigns to something they want in that hand.
    let model = transform.matrix();

    let joints: Vec<serde_json::Value> = skin
        .joints
        .iter()
        .zip(&globals)
        .enumerate()
        .map(|(index, (joint, global))| {
            let world = model * *global;
            let (scale, rotation, translation) = world.to_scale_rotation_translation();
            serde_json::json!({
                "index": index,
                "name": joint.name,
                "parent": joint.parent,
                "parent_name": joint.parent.map(|p| skin.joints[p].name.clone()),
                "rest": {
                    "position": joint.rest.translation.to_array(),
                    "rotation": joint.rest.rotation.to_array(),
                    "scale": joint.rest.scale.to_array(),
                },
                "world": {
                    "position": translation.to_array(),
                    "rotation": rotation.to_array(),
                    "scale": scale.to_array(),
                },
            })
        })
        .collect();

    let mut report = serde_json::json!({
        "asset": asset,
        "skin": skin.name,
        "joint_count": skin.joints.len(),
        "joints": joints,
    });
    if let Some(entity) = entity {
        report["entity"] = serde_json::Value::String(entity.to_string());
    }
    if let Some(clip) = clip {
        report["clip"] = serde_json::Value::String(clip.name.clone());
    }
    if let Some(time) = time {
        report["time"] = serde_json::json!(time as f64);
    }
    if let Some(clip_time) = clip_time {
        report["clip_time"] = serde_json::json!(clip_time as f64);
    }
    if let Some(stride) = stride {
        report["stride"] = stride;
    }
    report
}

#[allow(clippy::too_many_arguments)]
fn screenshot(
    scene_path: PathBuf,
    out: PathBuf,
    steps: u32,
    input_path: Option<PathBuf>,
    time: f32,
    camera_name: Option<&str>,
    width: u32,
    height: u32,
) -> Result<()> {
    let input = simulate::load_input(input_path.as_deref())?;
    let mut scene = load_scene(&scene_path)?;
    // System order: sample animations, then physics and particles, then render.
    let players = engine_core::animation::load_players(&scene, &scene_path)?;
    if !players.is_empty() {
        engine_core::animation::apply_all(&mut scene, &players, time);
    }
    let (particles, hud, interaction) = if steps > 0 {
        // The frame the cursor is measured in is the one about to be
        // rendered (M28).
        let view = engine_core::input::Viewport::new(width, height, camera_name);
        let outcome = simulate::run(&mut scene, &scene_path, steps, input.as_ref(), &view, None)?;
        (
            outcome.particles.instances(&scene.world),
            outcome.hud,
            outcome.interaction,
        )
    } else {
        (
            Vec::new(),
            Vec::new(),
            engine_core::ui::Interaction::default(),
        )
    };
    let (camera, camera_transform) = scene.camera(camera_name)?;
    let assets = engine_assets::AssetServer::for_scene(&scene_path);
    let render_time = scene_time(time, steps, &scene);
    let items = scene.render_items_at(&assets, Some(render_time))?;
    let drawn = items.len();
    let (lights, environment) = scene.resolved_at(render_time);
    let gi = gi_field(&scene, &scene_path, &lights, &environment);

    let image = engine_render::offscreen::render_with_adapter(
        &items,
        &scene.water_items(),
        &scene.cloud_items(),
        &scene.road_items(),
        &scene.meadow_items(),
        &particles,
        &camera,
        camera_transform.matrix(),
        lights,
        environment,
        render_time,
        width,
        height,
        &tinted_hud(&scene, &assets, &interaction),
        &hud,
        gi.as_ref(),
    )
    .map(|(image, _)| image)?;

    // Between rendering and encoding, while the frame is resident: one pass
    // over a buffer that is already there (M25). Nothing about the render
    // changes — this is a read.
    let digest = engine_render::digest::of(&image);

    let png = image::RgbaImage::from_raw(image.width, image.height, image.pixels)
        .expect("offscreen::render returns exactly width*height*4 bytes");
    png.save(&out).map_err(|e| {
        EngineError::new(codes::PNG_WRITE_FAILED, format!("could not write PNG: {e}"))
            .file(out.display().to_string())
    })?;

    let mut report = serde_json::json!({
        "written": out.display().to_string(),
        "width": image.width,
        "height": image.height,
        "scene": scene.name,
        "entities_drawn": drawn,
        "digest": digest_json(&digest),
    });
    if !hud.is_empty() {
        report["hud"] = serde_json::json!(hud);
    }
    println!("{report}");
    Ok(())
}

fn run_scene(
    scene_path: PathBuf,
    camera_name: Option<&str>,
    width: u32,
    height: u32,
    record_input: Option<PathBuf>,
) -> Result<()> {
    let scene = load_scene(&scene_path)?;
    let (camera, camera_transform) = scene.camera(camera_name)?;
    let assets = engine_assets::AssetServer::for_scene(&scene_path);
    let items = scene.render_items(&assets)?;
    let water = scene.water_items();
    let clouds = scene.cloud_items();
    // The *base* values: the viewer folds daylight onto these every frame
    // against its own fixed-step clock, so what it stores is the scene as
    // authored, not the scene at one instant.
    let roads = scene.road_items();
    let meadows = scene.meadow_items();
    let lights = scene.lights().resolved();
    let environment = scene.environment;
    let daylight = scene.daylight.clone();
    let hud_items = scene.hud_tree(&assets);
    // Read once, folded per frame: the file is a build artifact, the fold is
    // what the clock moves.
    let gi = engine_core::gi::evaluate::load_for_scene(
        &scene,
        scene_path.parent().unwrap_or(Path::new("")),
    );
    let title = format!("engine — {}", scene.name);

    // Physics and animated scenes come alive in the viewer; static scenes
    // stay static (unless a recording was asked for, which needs the step
    // clock running to have steps to record against).
    let players = engine_core::animation::load_players(&scene, &scene_path)?;
    let scripts = engine_script::ScriptHost::build(
        &scene.world,
        &scene_path,
        scene.physics.timestep_hz,
        scene.daylight.clone(),
        scene.environment,
        &assets,
        &scene.templates,
    )?;
    let has_physics =
        engine_physics::PhysicsWorld::scene_has_physics(&scene.world, &scene.templates);
    let has_emitters = engine_core::particles::ParticleSystem::scene_has_emitters(&scene.world);
    let simulation = if has_physics
        || has_emitters
        || !players.is_empty()
        || scripts.is_some()
        || record_input.is_some()
    {
        let physics = if has_physics {
            Some(engine_physics::PhysicsWorld::build(
                &scene.world,
                &scene.physics,
                &assets,
                &scene.templates,
            )?)
        } else {
            None
        };
        let recorder = record_input
            .as_deref()
            .map(crate::app::InputRecorder::create)
            .transpose()?;
        let particles = engine_core::particles::ParticleSystem::build(&scene.world);
        // Snapshots where every stride-driven entity starts, exactly as the
        // headless loop does — step 1 measures real displacement rather than
        // a jump from the origin (M32).
        let locomotion = engine_core::locomotion::Locomotion::build(&scene.world);
        Some(crate::app::Simulation {
            scene,
            physics,
            particles,
            players,
            locomotion,
            scripts,
            assets,
            camera_name: camera_name.map(String::from),
            held: engine_core::input::InputState::default(),
            contacts: engine_core::contact::ContactState::default(),
            interaction: engine_core::ui::Interaction::default(),
            recorder,
            accumulator: 0.0,
            t: 0.0,
            step_index: 0,
            last: None,
            hud_lines: Vec::new(),
        })
    } else {
        None
    };

    run_app(ViewerApp::new(
        title,
        width,
        height,
        Content::Scene(Box::new(SceneContent {
            items,
            water,
            clouds,
            roads,
            meadows,
            camera,
            camera_model: camera_transform.matrix(),
            lights,
            environment,
            daylight,
            hud_items,
            gi,
            simulation,
        })),
    ))
}

fn run_triangle(width: u32, height: u32) -> Result<()> {
    run_app(ViewerApp::new(
        "engine — M0",
        width,
        height,
        Content::Triangle,
    ))
}

fn run_app(mut app: ViewerApp) -> Result<()> {
    let event_loop = EventLoop::new().map_err(|e| {
        EngineError::new(
            codes::EVENT_LOOP_CREATION_FAILED,
            format!("could not create an event loop: {e}"),
        )
    })?;

    // Poll rather than Wait: the viewer redraws continuously, and this is the
    // mode a real-time loop will want anyway.
    event_loop.set_control_flow(ControlFlow::Poll);

    event_loop.run_app(&mut app).map_err(|e| {
        EngineError::new(
            codes::EVENT_LOOP_FAILED,
            format!("the event loop failed: {e}"),
        )
    })?;

    // A render error inside the loop exits it cleanly; surface it here.
    app.into_result()
}

fn info() -> Result<()> {
    let instance = Gpu::default_instance();
    let gpu = pollster::block_on(Gpu::new(instance, None))?;
    let adapter = gpu.adapter_info();

    let json = serde_json::json!({
        "name": adapter.name,
        "backend": adapter.backend.to_string(),
        "device_type": format!("{:?}", adapter.device_type),
        "driver": adapter.driver,
        "driver_info": adapter.driver_info,
    });

    println!(
        "{}",
        serde_json::to_string_pretty(&json).map_err(|e| {
            EngineError::new(
                codes::OUTPUT_SERIALIZATION_FAILED,
                format!("could not serialize adapter info: {e}"),
            )
        })?
    );

    Ok(())
}
