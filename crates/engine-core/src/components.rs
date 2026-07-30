//! Scene components.
//!
//! Components are plain data (invariant 5) — no methods that do engine work,
//! no references to other entities except by name. Everything here is
//! `Serialize + Deserialize + JsonSchema`, because the JSON Schema published by
//! `engine list-components` is derived from these types and never written by
//! hand (invariant 7).
//!
//! Adding a component means adding one line to the `components!` invocation at
//! the bottom. That macro is what keeps the serialized enum, the name list used
//! for `did_you_mean`, and the spawn logic from drifting apart.

use glam::{Quat, Vec2, Vec3};
use hecs::EntityBuilder;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// An entity's stable identifier (invariant 4).
///
/// Attached as a component so systems can resolve names from the world alone,
/// and mirrored in [`Scene::entity`](crate::scene::Scene::entity) for lookups.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct Name(pub String);

/// Position, orientation, and scale.
///
/// All three fields are optional in JSON; omitting one gives the identity
/// value, so a scene can say `{"type": "Transform", "position": [0, 3, 0]}`
/// without restating a rotation and scale it does not care about.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Transform {
    #[schemars(with = "[f32; 3]")]
    pub position: Vec3,

    /// Euler angles in **degrees**, `[x, y, z]`, applied in X→Y→Z intrinsic
    /// order (glam `EulerRot::XYZ`). Identity is `[0, 0, 0]`.
    ///
    /// Degrees rather than a quaternion is a settled decision (design doc §9):
    /// an agent told to "rotate 45° about Y" writes `[0.0, 45.0, 0.0]`
    /// directly, whereas an unlabeled `[x, y, z, w]` array invites silent
    /// ordering bugs.
    #[schemars(with = "[f32; 3]")]
    pub rotation: Vec3,

    #[schemars(with = "[f32; 3]")]
    pub scale: Vec3,
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            position: Vec3::ZERO,
            rotation: Vec3::ZERO,
            scale: Vec3::ONE,
        }
    }
}

impl Transform {
    /// The rotation as a quaternion — the only place the Euler-degrees file
    /// format is converted, so the order/units convention cannot drift.
    pub fn quat(&self) -> Quat {
        Quat::from_euler(
            glam::EulerRot::XYZ,
            self.rotation.x.to_radians(),
            self.rotation.y.to_radians(),
            self.rotation.z.to_radians(),
        )
    }

    /// Local-to-world matrix.
    pub fn matrix(&self) -> glam::Mat4 {
        glam::Mat4::from_scale_rotation_translation(self.scale, self.quat(), self.position)
    }
}

/// Renderable geometry.
///
/// `asset` is either a `builtin:` primitive (`builtin:cube`, `builtin:cylinder`,
/// `builtin:plane`, `builtin:sphere`, `builtin:triangle`) or a `.gltf`/`.glb`
/// file's relative path, resolved
/// against the directory of the scene file that references it (invariant 3).
/// `engine validate` checks the reference resolves; a file that exists but
/// fails to parse is reported by validation's asset pass and again at render
/// time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Mesh {
    pub asset: String,
}

/// A viewpoint. `engine screenshot --camera <name>` selects one by entity name.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Camera {
    /// Vertical field of view, in degrees. Strictly between 0 and 180.
    #[schemars(extend("exclusiveMinimum" = 0.0, "exclusiveMaximum" = 180.0))]
    pub fov: f32,
    /// Near clip distance. Strictly positive.
    #[schemars(extend("exclusiveMinimum" = 0.0))]
    pub near: f32,
    /// Far clip distance. Strictly positive, and must exceed `near`
    /// (checked cross-field by `engine validate`).
    #[schemars(extend("exclusiveMinimum" = 0.0))]
    pub far: f32,

    /// Marks the camera used when none is named explicitly.
    ///
    /// Exactly one camera in a scene may set this. Zero or several is a
    /// validation error rather than a warning-plus-guess: a deterministic
    /// failure is cheaper for an agent than a nondeterministic success.
    pub active: bool,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            fov: 60.0,
            near: 0.1,
            far: 1000.0,
            active: false,
        }
    }
}

/// Surface appearance, in the metallic/roughness parameterization every
/// mainstream engine and glTF file uses.
///
/// All color fields are **linear** RGB in `[0, 1]` — physical reflectance, not
/// sRGB-encoded screen values. The engine never silently decodes an authored
/// color; the PNG pixel is the lit, sRGB-encoded result, so `albedo: [0.5,
/// 0.5, 0.5]` under full light reads back ≈188, not 128.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Material {
    #[schemars(with = "[f32; 3]", inner(range(min = 0.0, max = 1.0)))]
    pub albedo: Vec3,
    /// `0` = dielectric, `1` = metal. Metals have no diffuse; their specular
    /// is tinted by `albedo`. Range `[0, 1]`.
    #[schemars(range(min = 0.0, max = 1.0))]
    pub metallic: f32,
    /// Perceptual roughness: `0` = mirror-tight highlight, `1` = matte.
    /// Range `[0, 1]`.
    #[schemars(range(min = 0.0, max = 1.0))]
    pub roughness: f32,
    /// Added after lighting, unaffected by any light — "make this visible
    /// regardless of lighting" is a debugging move worth having. Range
    /// `[0, 1]` per component.
    #[schemars(with = "[f32; 3]", inner(range(min = 0.0, max = 1.0)))]
    pub emissive: Vec3,

    /// Uniform opacity: `1` = opaque, `0` = invisible. Range `[0, 1]`.
    ///
    /// A flat blend with no view dependence — the "ghost this object" knob.
    /// Anything below 1 moves the entity out of the opaque pass and into the
    /// sorted blended one, where it tests depth but does not write it.
    #[schemars(range(min = 0.0, max = 1.0))]
    pub alpha: f32,

    /// How much light passes *through* the surface instead of scattering off
    /// it: `0` = opaque, `1` = clear glass. Range `[0, 1]`.
    ///
    /// Unlike [`Material::alpha`] this is view-dependent and keeps the
    /// specular lobe, which is the whole difference between a transparent
    /// object and a faded one: a water surface seen edge-on reflects the sky
    /// and hides its bottom, and seen from overhead it does neither. The
    /// approximation is a Fresnel lerp back toward opaque at grazing angles,
    /// with the diffuse term scaled by `1 - transmission` (light that went
    /// through did not come back). There is no refraction and no
    /// scene-color sampling, so what is behind the surface is not bent or
    /// tinted by its thickness — see `materials-lighting-design.md`.
    #[schemars(range(min = 0.0, max = 1.0))]
    pub transmission: f32,
}

impl Material {
    /// Whether this material draws in the blended pass rather than the opaque
    /// one. Exactly the pre-M16 opaque path when both fields sit at their
    /// defaults, which is what keeps every committed baseline bit-exact.
    pub fn is_transparent(&self) -> bool {
        self.alpha < 1.0 || self.transmission > 0.0
    }
}

impl Default for Material {
    fn default() -> Self {
        Self {
            albedo: Vec3::splat(0.8),
            metallic: 0.0,
            roughness: 0.9,
            emissive: Vec3::ZERO,
            alpha: 1.0,
            transmission: 0.0,
        }
    }
}

/// A sun: parallel light with no falloff.
///
/// The light shines down the entity's local **−Z**, taken from its
/// `Transform` — the same convention the camera uses, so aiming a light is
/// aiming a camera. With no `Transform` the light travels toward −Z
/// (horizontally); a noon sun is `"rotation": [-90, 0, 0]`.
///
/// At most one per scene (`multiple_directional_lights`). A scene with **no**
/// light components at all gets a documented fallback rig (sun + ambient); a
/// scene with any light component gets exactly what it wrote — absent means
/// off.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct DirectionalLight {
    /// Linear RGB chromaticity, each component in `[0, 1]`. Magnitude lives
    /// in `intensity`.
    #[schemars(with = "[f32; 3]", inner(range(min = 0.0, max = 1.0)))]
    pub color: Vec3,
    /// Unitless multiplier, `>= 0`, unbounded above: intensity 2 is twice as
    /// bright, and a white light at 1.0 on a white surface head-on reads
    /// white.
    #[schemars(range(min = 0.0))]
    pub intensity: f32,
}

impl Default for DirectionalLight {
    fn default() -> Self {
        Self {
            color: Vec3::ONE,
            intensity: 1.0,
        }
    }
}

/// A flat, non-directional fill: `albedo * color * intensity`, added to the
/// lit result. Exists because a sun-only scene renders back faces pure black,
/// and a black region in a screenshot tells an agent nothing.
///
/// At most one per scene (`multiple_ambient_lights`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct AmbientLight {
    /// Linear RGB, each component in `[0, 1]`.
    #[schemars(with = "[f32; 3]", inner(range(min = 0.0, max = 1.0)))]
    pub color: Vec3,
    /// `>= 0`.
    #[schemars(range(min = 0.0))]
    pub intensity: f32,
}

impl Default for AmbientLight {
    fn default() -> Self {
        Self {
            color: Vec3::ONE,
            intensity: 0.05,
        }
    }
}

/// A local light that shines in every direction from its entity's position,
/// falling off with distance (M17).
///
/// Unlike the sun, there may be **many** per scene — up to
/// [`MAX_POINT_LIGHTS`], beyond which the scene is invalid
/// (`too_many_point_lights`) rather than silently missing the extras. It has no
/// orientation, so its `Transform.rotation` and `scale` are ignored; only
/// `position` is read. Point lights do not cast shadows (the engine has one
/// shadow map, and it belongs to the sun) and are not attenuated by fog.
///
/// This is the component that lets a fire light the ground around it: its
/// `intensity` and `color` are in the curated script API
/// (`world.light_intensity` / `set_light_intensity` / `light_color` /
/// `set_light_color`), so the same flicker signal that drives a flame's
/// emission rate can drive the light it casts.
///
/// Presence counts as "the scene lit itself": a scene whose only light is a
/// `PointLight` gets no fallback sun, same as with any other light component.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct PointLight {
    /// Linear RGB, each component in `[0, 1]`.
    #[schemars(with = "[f32; 3]", inner(range(min = 0.0, max = 1.0)))]
    pub color: Vec3,

    /// Brightness at one unit of distance. `>= 0`.
    ///
    /// Falloff is inverse-square, so this is the value the surface of a sphere
    /// one metre away receives — which makes `intensity` comparable to
    /// `DirectionalLight.intensity` at exactly that distance and four times
    /// dimmer at two metres. A campfire is a few units; a candle is a fraction.
    #[schemars(range(min = 0.0))]
    pub intensity: f32,

    /// Distance in world units at which the light reaches exactly zero. `> 0`.
    ///
    /// Inverse-square falloff never truly reaches zero, so a range is what
    /// keeps a light local: the physical curve is multiplied by a window that
    /// smoothly closes at `range`. Without it, every light in a scene would
    /// contribute a little to every surface, and a lantern in one room would
    /// lift the black level of the next.
    #[schemars(extend("exclusiveMinimum" = 0.0))]
    pub range: f32,
}

impl Default for PointLight {
    fn default() -> Self {
        Self {
            color: Vec3::ONE,
            intensity: 1.0,
            range: 10.0,
        }
    }
}

/// How many [`PointLight`]s one scene may carry.
///
/// The shader holds them in a fixed-size uniform array and tests every one
/// against every lit fragment, so this is a real cost, not a formality. Eight
/// is enough for a campfire, a couple of lamps, and a muzzle flash; a scene
/// that wants a hundred wants a different technique (clustered or deferred
/// shading), which is a decision to make deliberately rather than by raising
/// this number until the frame time collapses.
pub const MAX_POINT_LIGHTS: usize = 8;

/// How a rigid body participates in simulation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BodyKind {
    /// Fully simulated: forces, gravity, collisions.
    Dynamic,
    /// Moved by writes to its `Transform`; pushes dynamics, is not pushed.
    Kinematic,
    /// Never moves. For platforms that also want a `RigidBody`; bare
    /// `Collider`s without a `RigidBody` are static geometry too.
    Fixed,
}

/// A simulated rigid body (M8). Requires a `Transform`; a **dynamic** body
/// also requires a `Collider` (`missing_collider` — it would fall forever
/// through everything).
///
/// Simulation state is derived, never authoritative: this component is the
/// initial conditions, and `engine simulate --bake` writes the evolved values
/// back as ordinary scene text.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RigidBody {
    pub body: BodyKind,

    #[serde(default)]
    #[schemars(with = "[f32; 3]")]
    pub linear_velocity: Vec3,

    /// **Degrees per second**, `[x, y, z]` — the same units and axis order as
    /// `Transform.rotation`, for the same reason: the agent that writes
    /// `"rotation": [0, 45, 0]` writes `[0, 90, 0]` for a half-turn per
    /// second. Converted to rad/s only at the physics-backend boundary.
    #[serde(default)]
    #[schemars(with = "[f32; 3]")]
    pub angular_velocity: Vec3,

    /// Multiplier on scene gravity. `>= 0`.
    #[serde(default = "one")]
    #[schemars(range(min = 0.0))]
    pub gravity_scale: f32,

    /// `>= 0`.
    #[serde(default)]
    #[schemars(range(min = 0.0))]
    pub linear_damping: f32,

    /// `>= 0`.
    #[serde(default)]
    #[schemars(range(min = 0.0))]
    pub angular_damping: f32,

    /// Continuous collision detection, for small fast bodies that would
    /// otherwise tunnel.
    #[serde(default)]
    pub ccd: bool,

    /// Allow the solver to put this body to sleep once it settles.
    #[serde(default = "yes")]
    pub can_sleep: bool,

    /// Lock rotation around the `[x, y, z]` world axes. A vehicle locks
    /// `[true, false, true]`: yaw stays free for steering while contacts can
    /// no longer pitch or roll it over.
    #[serde(default)]
    pub locked_rotations: [bool; 3],
}

fn one() -> f32 {
    1.0
}

fn yes() -> bool {
    true
}

/// The collision shape kinds `Collider.shape` may name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ColliderShapeKind {
    Cuboid,
    Sphere,
    Capsule,
    // NOTE: variants stay undocumented — a schemars doc comment on a variant
    // turns the schema from a flat "enum" into oneOf/const, which blinds the
    // walk's closed-vocabulary check (unknown_shape). Shape semantics are
    // documented on `Collider`.
    Trimesh,
    ConvexHull,
}

/// Collision geometry (M8). Requires a `Transform`. With no `RigidBody` on
/// the entity, this is static collision geometry — the common case for
/// ground planes and walls.
///
/// One flat object discriminated by `shape` (the shape `jq` and an LLM
/// handle best): `cuboid` uses `half_extents`, `sphere` uses `radius`,
/// `capsule` (Y-axis) uses `half_height` + `radius`; `trimesh` and
/// `convex_hull` take their geometry from `asset`, or from the entity's own
/// `Mesh` when `asset` is absent — the collider matches what the screenshot
/// shows, by construction. Validation enforces which fields each shape
/// requires and forbids — the file format is the contract, the flat Rust
/// struct is how it stays walkable by the schema-driven validator and the
/// editor's generated inspector.
///
/// `Transform.scale` scales the shape when the physics world is built — a
/// cube scaled 2x collides 2x big, which is what the screenshot shows.
/// Nonuniform scale on a round shape has no physics representation and is a
/// validation error, never a silent approximation (mesh shapes scale
/// per-vertex, so nonuniform is fine there).
///
/// Layers: `layers` names the collision layers this collider belongs to,
/// `collides_with` restricts which layers it interacts with. Both absent
/// means "collide with everything" — exactly the pre-layer behavior. Two
/// colliders interact only if each one's `collides_with` (or absence)
/// admits a layer the other belongs to. Layer names are scene-local
/// strings; a scene may use at most 32 distinct names.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Collider {
    pub shape: ColliderShapeKind,

    /// `cuboid` only. Each component `> 0`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<[f32; 3]>")]
    pub half_extents: Option<Vec3>,

    /// `sphere` and `capsule`. `> 0`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub radius: Option<f32>,

    /// `capsule` only: half the cylindrical section's height. `> 0`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub half_height: Option<f32>,

    /// `trimesh` and `convex_hull` only: the mesh whose geometry to collide
    /// as (`builtin:` or a `.gltf`/`.glb` path relative to the scene file).
    /// Absent, the entity's own `Mesh.asset` is used — the common case; a
    /// mesh shape with neither is `collider_missing_mesh`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset: Option<String>,

    /// Collision layers this collider is a member of. Absent = member of
    /// every layer. Empty is an error — omit the field instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layers: Option<Vec<String>>,

    /// Only interact with colliders belonging to these layers. Absent =
    /// interact with everything. Empty is an error — omit the field
    /// instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collides_with: Option<Vec<String>>,

    /// `>= 0`.
    #[serde(default = "half")]
    #[schemars(range(min = 0.0))]
    pub friction: f32,

    /// Bounciness, `[0, 1]`. When two colliders touch, the **larger** of
    /// the two restitutions applies (max-combine), so a bouncy ball bounces
    /// on a plain floor exactly as its own value says.
    #[serde(default)]
    #[schemars(range(min = 0.0, max = 1.0))]
    pub restitution: f32,

    /// Mass comes from `density` x shape volume. `> 0`.
    #[serde(default = "one")]
    #[schemars(extend("exclusiveMinimum" = 0.0))]
    pub density: f32,

    /// Sensors detect overlaps (trace events) but exert no forces.
    #[serde(default)]
    pub sensor: bool,

    /// Local offset of the shape from the entity's transform origin.
    #[serde(default)]
    #[schemars(with = "[f32; 3]")]
    pub offset: Vec3,
}

fn half() -> f32 {
    0.5
}

/// Plays an animation clip against scene time (M9).
///
/// `clip` is a relative path to a property clip (`*.anim.json`); a
/// `path#ClipName` glTF fragment is reserved for skeletal clips (not yet
/// supported). A player in the file is playing — there is no play/pause
/// runtime state, because pose is a pure function of (files, time).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AnimationPlayer {
    pub clip: String,

    /// Time multiplier; local time = `t * speed + start_offset`.
    #[serde(default = "one")]
    pub speed: f32,

    /// Wrap by clip duration; when false, clamp to the final pose.
    #[serde(default = "yes")]
    pub looping: bool,

    #[serde(default)]
    pub start_offset: f32,
}

/// Where a HUD element attaches on screen (M12).
///
/// `offset` is measured **inward** from the anchor: from a right anchor,
/// `offset[0]` runs leftward; from a bottom anchor, `offset[1]` runs upward;
/// from `center` it is the usual +x-right / +y-down applied to the element's
/// center. The anchored point is the element's matching corner (its center
/// for `center`), so `offset: [0, 0]` puts the element flush against its
/// corner at any resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HudAnchor {
    #[default]
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    Center,
}

/// A screen-space text label (M12): one line of the built-in 8×8 pixel font,
/// drawn over the 3D scene after lighting, independent of any camera.
///
/// Needs no `Transform` — placement is `anchor` + `offset` in framebuffer
/// pixels, which is what the agent sees in the PNG. Text is always opaque
/// and never anti-aliased, so a HUD glyph is bit-exact in baselines. Glyphs
/// outside the font's coverage render as a filled box: visibly wrong in the
/// screenshot, never a panic. Draw order is file order, and all text draws
/// over all `HudRect`s.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HudText {
    /// One line; no wrapping. Scripts may rewrite it via
    /// `world.set_hud_text` — an empty string is a legal rest value for a
    /// script-driven readout.
    pub text: String,

    #[serde(default)]
    pub anchor: HudAnchor,

    /// Pixels inward from `anchor` (see [`HudAnchor`]).
    #[serde(default)]
    #[schemars(with = "[f32; 2]")]
    pub offset: Vec2,

    /// Glyph height in pixels, `>= 4`. The 8×8 font renders at integer
    /// scale `max(1, round(size / 8))`, so `16` means exactly 2× glyphs.
    #[serde(default = "hud_text_size")]
    #[schemars(range(min = 4.0))]
    pub size: f32,

    /// Linear RGB in `[0, 1]`, like every color in the engine; encoded to
    /// sRGB when the overlay is rasterized.
    #[serde(default = "white")]
    #[schemars(with = "[f32; 3]", inner(range(min = 0.0, max = 1.0)))]
    pub color: Vec3,
}

fn hud_text_size() -> f32 {
    16.0
}

fn white() -> Vec3 {
    Vec3::ONE
}

/// A screen-space solid rectangle (M12): the primitive behind health bars,
/// speed bars, and backdrops. Drawn before all `HudText`, file order within
/// rects.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HudRect {
    #[serde(default)]
    pub anchor: HudAnchor,

    /// Pixels inward from `anchor` (see [`HudAnchor`]).
    #[serde(default)]
    #[schemars(with = "[f32; 2]")]
    pub offset: Vec2,

    /// `[width, height]` in pixels, each `>= 0` — zero is legal so a
    /// script-driven bar can be empty. Scripts resize via
    /// `world.set_hud_rect_size`.
    #[schemars(with = "[f32; 2]", inner(range(min = 0.0)))]
    pub size: Vec2,

    /// Linear RGB in `[0, 1]`.
    #[serde(default = "white")]
    #[schemars(with = "[f32; 3]", inner(range(min = 0.0, max = 1.0)))]
    pub color: Vec3,

    /// `[0, 1]`; `1` (the default) replaces the pixel exactly, fractions
    /// alpha-blend on the GPU (deterministic per adapter, like every
    /// baseline).
    #[serde(default = "one")]
    #[schemars(range(min = 0.0, max = 1.0))]
    pub opacity: f32,
}

/// One piece a `Breakable` entity shatters into (M14).
///
/// `mesh` follows `Mesh.asset` rules (builtin or relative glTF path).
/// `offset`/`rotation`/`scale` place the fragment relative to the parent
/// entity, so the assembled fragments overlay the unbroken model.
/// `half_extents` is the fragment's cuboid collider in fragment-local units —
/// `scale` scales it, exactly as `Transform.scale` scales a `Collider`.
/// Cuboid-only fragment colliders are deliberate v1 scope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Fragment {
    pub mesh: String,

    /// Position relative to the parent entity's origin, in parent-local
    /// units (the parent's `Transform.scale` applies).
    #[serde(default)]
    #[schemars(with = "[f32; 3]")]
    pub offset: Vec3,

    /// Euler angles in **degrees**, `[x, y, z]`, XYZ order — the same
    /// convention as `Transform.rotation`.
    #[serde(default)]
    #[schemars(with = "[f32; 3]")]
    pub rotation: Vec3,

    #[serde(default = "ones")]
    #[schemars(with = "[f32; 3]")]
    pub scale: Vec3,

    /// Cuboid collider half-extents. The default matches `builtin:cube`, so
    /// a fragment that is a scaled builtin cube needs no collider authoring.
    #[serde(default = "half_cube")]
    #[schemars(with = "[f32; 3]")]
    pub half_extents: Vec3,

    /// Fragment mass comes from `density` x collider volume. `> 0`.
    #[serde(default = "one")]
    #[schemars(extend("exclusiveMinimum" = 0.0))]
    pub density: f32,
}

fn ones() -> Vec3 {
    Vec3::ONE
}

fn half_cube() -> Vec3 {
    Vec3::splat(0.5)
}

/// Breaks into pre-authored fragments (M14) — on a hard enough collision,
/// inside an explosion, or when a script calls `world.break_entity`.
///
/// On break the entity is replaced, after that step's physics, by one
/// dynamic-body entity per fragment (`Parent.frag0`, `Parent.frag1`, …),
/// each inheriting the parent's `Material` and motion. Fragments are
/// ordinary entities afterwards: they render, trace, and bake like anything
/// else. See `breaking-design.md`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Breakable {
    /// What the entity becomes. At least one.
    #[schemars(length(min = 1))]
    pub fragments: Vec<Fragment>,

    /// Contact impulse, in kg·m/s (≈ mass x closing speed), at or above
    /// which a collision breaks this entity. **Absent means collisions
    /// never break it** — only scripts and explosions do. Impulse rather
    /// than force so the number survives a `timestep_hz` change. `> 0`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("exclusiveMinimum" = 0.0))]
    pub impulse_threshold: Option<f32>,
}

/// Gameplay logic as data (M10): a Rhai script run once per fixed step.
///
/// `source` is a relative `.rhai` path defining `fn step(world, step)`.
/// Scripts mutate the world through a small registered API and never invent
/// state of their own — baked output after a scripted run is an ordinary
/// scene file. See `scripting-design.md`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Script {
    pub source: String,
}

/// One raycast-suspension wheel of a vehicle (M12).
///
/// Sits on its own entity — the wheel's visual — and names its chassis in
/// `vehicle` (an entity with a **dynamic** `RigidBody` + `Collider`). The
/// engine groups every wheel naming the same chassis into one raycast
/// vehicle: each wheel casts a ray down its suspension, a spring/damper
/// pushes the chassis, and a tire friction model drives and grips at the
/// contact point. The wheel entity's own `Transform` is **written by
/// physics** every step — suspension compression, steering yaw, and axle
/// spin — so screenshots show wheels that steer, roll, and bounce. Author
/// its rest pose; simulation owns it afterwards. A wheel entity must not
/// carry its own `RigidBody` or `Collider` (`wheel_with_physics`) — the
/// chassis owns all collision.
///
/// Control fields (`engine_force`, `brake`, `steering`) are runtime inputs
/// the same way `RigidBody.linear_velocity` is: scripts write them via
/// `world.set_engine_force(...)` and friends, and physics reads them each
/// step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Wheel {
    /// Name of the chassis entity this wheel belongs to. Must be a
    /// *different* entity with a dynamic `RigidBody` and a `Collider`.
    pub vehicle: String,

    /// Suspension attachment point in the chassis's local frame, in meters
    /// (rotated with the chassis, **not** multiplied by its
    /// `Transform.scale`). The suspension ray starts here and points down
    /// the chassis's local −Y.
    #[serde(default)]
    #[schemars(with = "[f32; 3]")]
    pub offset: Vec3,

    /// Wheel radius in meters. `> 0`.
    #[serde(default = "default_wheel_radius")]
    #[schemars(extend("exclusiveMinimum" = 0.0))]
    pub radius: f32,

    /// Suspension spring rest length in meters: how far below `offset` the
    /// wheel center hangs when the spring is neither compressed nor
    /// stretched. `>= 0`.
    #[serde(default = "default_suspension_rest_length")]
    #[schemars(range(min = 0.0))]
    pub suspension_rest_length: f32,

    /// Spring stiffness **per kilogram of chassis mass** (the backend
    /// multiplies by mass, so the same value suspends a light or heavy
    /// chassis identically). Higher = stiffer. `> 0`. Static sag per wheel
    /// is roughly `9.81 / (4 * stiffness)` meters — keep
    /// `suspension_travel` above that.
    #[serde(default = "default_suspension_stiffness")]
    #[schemars(extend("exclusiveMinimum" = 0.0))]
    pub suspension_stiffness: f32,

    /// Damping while the spring compresses, per kilogram of chassis mass.
    /// `sqrt(stiffness)` is critical damping; ~0.5× that is a comfortable
    /// car. `>= 0`.
    #[serde(default = "default_suspension_compression")]
    #[schemars(range(min = 0.0))]
    pub suspension_compression: f32,

    /// Damping while the spring extends (rebound), per kilogram of chassis
    /// mass. Usually a little higher than `suspension_compression`. `>= 0`.
    #[serde(default = "default_suspension_damping")]
    #[schemars(range(min = 0.0))]
    pub suspension_damping: f32,

    /// Maximum travel from rest length, in meters, both directions. `>= 0`.
    #[serde(default = "default_suspension_travel")]
    #[schemars(range(min = 0.0))]
    pub suspension_travel: f32,

    /// Hard cap on the suspension force, in newtons (not mass-scaled).
    /// `>= 0`.
    #[serde(default = "default_max_suspension_force")]
    #[schemars(range(min = 0.0))]
    pub max_suspension_force: f32,

    /// Tire traction: how much forward/braking impulse the contact patch
    /// transmits before it slips, as a multiple of the suspension load.
    /// Larger = grippier; too large flips the vehicle under hard braking.
    /// `>= 0`.
    #[serde(default = "default_friction_slip")]
    #[schemars(range(min = 0.0))]
    pub friction_slip: f32,

    /// Multiplier on the tire's sideways grip. `1.0` = full lateral
    /// friction; lower values let the vehicle drift. `>= 0`.
    #[serde(default = "one")]
    #[schemars(range(min = 0.0))]
    pub side_friction_stiffness: f32,

    /// Drive force along the wheel's rolling direction, in newtons.
    /// Positive is the chassis's forward (−Z); negative reverses. A runtime
    /// input scripts write each step (`world.set_engine_force`).
    #[serde(default)]
    pub engine_force: f32,

    /// Braking strength, `>= 0`. A runtime input scripts write each step
    /// (`world.set_brake`).
    #[serde(default)]
    #[schemars(range(min = 0.0))]
    pub brake: f32,

    /// Steering angle in **degrees** about the chassis's up axis; positive
    /// steers left. A runtime input scripts write each step
    /// (`world.set_steering`).
    #[serde(default)]
    pub steering: f32,
}

fn default_wheel_radius() -> f32 {
    0.3
}
fn default_suspension_rest_length() -> f32 {
    0.3
}
fn default_suspension_stiffness() -> f32 {
    24.0
}
fn default_suspension_compression() -> f32 {
    2.5
}
fn default_suspension_damping() -> f32 {
    3.5
}
fn default_suspension_travel() -> f32 {
    0.25
}
fn default_max_suspension_force() -> f32 {
    30000.0
}
fn default_friction_slip() -> f32 {
    10.5
}

/// How a particle's color combines with what is already on screen.
///
/// NOTE: leave these variants undocumented. A doc comment on an enum *variant*
/// makes schemars emit oneOf/const instead of a flat `"enum": [...]`, which
/// blinds the validation walk's closed-vocabulary check — the same trap
/// [`ColliderShapeKind`] carries a note about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum ParticleBlend {
    #[default]
    Alpha,
    Additive,
}

/// A deterministic particle emitter (M13): smoke, sparks, dust — and, with the
/// M17 fields, fire.
///
/// Particles spray from the entity's position into a cone around its local
/// **−Z** — the same aiming convention the camera and lights use, so a rising
/// smoke plume is `"rotation": [90, 0, 0]`. Each particle is born with
/// `start_*` values and reaches its `end_*` values as it dies; the billboard
/// renders as a soft unlit disc, alpha-blended over the scene.
///
/// Particles are simulation state on the fixed step clock, advanced by
/// `--steps` exactly like physics (`--time` poses animations, it does not
/// advance particles). The state is derived and disposable — never baked,
/// never traced — and the `seed` makes it reproducible: same file, same
/// steps, same particles, so screenshots of smoke diff-render bit-exactly.
///
/// # The fire fields (M17)
///
/// A cone of identical sprites reads as a sparkler, not a flame. Five fields
/// fix that, and **every one of them defaults to the M13 behaviour** — a
/// pre-M17 emitter is byte-identical, down to which random numbers it draws:
/// `radius` spreads emission over a fire bed, the three `*_jitter` fields
/// break the lockstep of a population born identical, `turbulence` makes
/// flames lick, `stretch` turns round puffs into motion-aligned tongues and
/// ember streaks, and `blend: "additive"` makes overlapping flame get
/// *brighter* instead of merely more opaque, which is the single biggest
/// difference between fire and orange smoke.
///
/// **Random draws are skipped, not defaulted, when a field is off.** Every
/// jitter field consumes the emitter's RNG only when it is non-zero, in the
/// documented order (direction → disc → speed → size → lifetime →
/// turbulence). That is what keeps every baseline blessed before M17 exact,
/// and it is the same discipline as not consuming the RNG on a capped spawn.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ParticleEmitter {
    /// Particles spawned per second. `>= 0`; 0 stops emission (existing
    /// particles live out their lifetime).
    #[schemars(range(min = 0.0))]
    pub rate: f32,

    /// Seconds each particle lives. `> 0`.
    #[schemars(extend("exclusiveMinimum" = 0.0))]
    pub lifetime: f32,

    /// Initial speed in units/second along the sampled cone direction. `>= 0`.
    #[schemars(range(min = 0.0))]
    pub speed: f32,

    /// Cone half-angle in degrees around local −Z. `[0, 180]`: 0 is a beam,
    /// 90 a hemisphere, 180 a full sphere.
    #[schemars(range(min = 0.0, max = 180.0))]
    pub spread: f32,

    /// World-space acceleration applied to every live particle, units/s².
    /// Particles ignore physics; this is where buoyancy (`[0, 1, 0]` for
    /// smoke) or gravity (`[0, -9.81, 0]` for sparks) comes from.
    #[schemars(with = "[f32; 3]")]
    pub acceleration: Vec3,

    /// Velocity damping per second. `>= 0`; 0 means none.
    #[schemars(range(min = 0.0))]
    pub drag: f32,

    /// Billboard half-size in world units at birth. `> 0`.
    #[schemars(extend("exclusiveMinimum" = 0.0))]
    pub start_size: f32,

    /// Billboard half-size at death. `> 0`; larger than `start_size` grows
    /// (smoke), smaller shrinks (sparks).
    #[schemars(extend("exclusiveMinimum" = 0.0))]
    pub end_size: f32,

    /// Linear RGB at birth, each component in `[0, 1]`. Unlit.
    #[schemars(with = "[f32; 3]", inner(range(min = 0.0, max = 1.0)))]
    pub start_color: Vec3,

    /// Linear RGB at death, each component in `[0, 1]`.
    #[schemars(with = "[f32; 3]", inner(range(min = 0.0, max = 1.0)))]
    pub end_color: Vec3,

    /// Opacity at birth, `[0, 1]`.
    #[schemars(range(min = 0.0, max = 1.0))]
    pub start_alpha: f32,

    /// Opacity at death, `[0, 1]`. The default 0 fades particles out.
    #[schemars(range(min = 0.0, max = 1.0))]
    pub end_alpha: f32,

    /// Cap on live particles; spawns beyond it are dropped (deterministically).
    /// `[1, 65536]`.
    #[schemars(range(min = 1, max = 65536))]
    pub max_particles: u32,

    /// Seed for the emitter's random spray directions. Same seed, same steps →
    /// identical particles; give two otherwise-identical emitters different
    /// seeds so they don't emit in lockstep.
    pub seed: u32,

    /// How the billboard combines with the scene behind it. `"alpha"` (default)
    /// occludes what it covers; `"additive"` adds light to it, so overlapping
    /// sprites brighten toward white and a thin one is nearly invisible.
    ///
    /// Fire, sparks, and magic are additive; smoke, dust, and spray are not.
    /// Additive particles draw after every alpha-blended one, so a flame glows
    /// *through* the smoke above it rather than being occluded by it — the
    /// approximation is deliberate and it is what a real fire looks like,
    /// since that smoke is genuinely lit from below.
    pub blend: ParticleBlend,

    /// Radius of the disc particles are born on, in world units, centred on the
    /// entity and lying in the plane **perpendicular to the aim** (local XY).
    /// `>= 0`; 0 (default) emits from the single point.
    ///
    /// A campfire is a bed of coals, not a nozzle: emitting a flame from one
    /// point gives a cone with a visible apex, and no amount of `spread` hides
    /// it. Like `Wheel.offset`, this is rotated by the entity's rotation but
    /// **not** scaled by its `Transform.scale`.
    #[schemars(range(min = 0.0))]
    pub radius: f32,

    /// Fraction by which each particle's launch speed varies from `speed`,
    /// `[0, 1]`: 0.3 means every particle draws uniformly from ±30%.
    #[schemars(range(min = 0.0, max = 1.0))]
    pub speed_jitter: f32,

    /// Fraction by which each particle's size varies from `start_size` /
    /// `end_size`, `[0, 1]`. One multiplier scales both, so a particle keeps
    /// its authored growth curve and only its scale changes.
    #[schemars(range(min = 0.0, max = 1.0))]
    pub size_jitter: f32,

    /// Fraction by which each particle's lifespan varies from `lifetime`,
    /// `[0, 1)`. The most valuable of the three for fire: identical lifespans
    /// put the whole population's fade at one height, which draws a flat top
    /// on the flame, and varying them is what makes it ragged.
    ///
    /// A particle's lifespan is fixed **at birth**, so animating `lifetime`
    /// mid-run retimes new particles and leaves live ones alone.
    #[schemars(range(min = 0.0, max = 0.99))]
    pub lifetime_jitter: f32,

    /// Strength of a swirling world-space acceleration field, units/s².
    /// `>= 0`; 0 (default) means none.
    ///
    /// This is what separates a flame from a jet of orange dots: hot gas is
    /// unstable and curls. The field is smooth value noise sampled along each
    /// particle's own path (see `turbulence_scale`), so a particle wanders
    /// coherently rather than vibrating — and it is generated by an integer
    /// hash specified in this repo, so it is exactly as reproducible as the
    /// spray directions.
    #[schemars(range(min = 0.0))]
    pub turbulence: f32,

    /// Size in world units of one cell of the turbulence field. `> 0`.
    ///
    /// Small values curl the flame tightly (candle), large ones sway the whole
    /// plume (bonfire in wind). Roughly: a particle's direction changes over
    /// this distance travelled.
    #[schemars(extend("exclusiveMinimum" = 0.0))]
    pub turbulence_scale: f32,

    /// Seconds of travel to stretch the billboard along its own velocity.
    /// `>= 0`; 0 (default) keeps sprites round.
    ///
    /// The sprite grows along its direction of motion by the distance the
    /// particle covers in this many seconds, so the same value stretches a
    /// fast ember into a streak and leaves slow smoke nearly circular — which
    /// is what a camera does. Flame tongues want a little (~0.05), sparks want
    /// a lot (~0.2).
    #[schemars(range(min = 0.0))]
    pub stretch: f32,
}

impl Default for ParticleEmitter {
    fn default() -> Self {
        Self {
            rate: 10.0,
            lifetime: 2.0,
            speed: 1.0,
            spread: 15.0,
            acceleration: Vec3::ZERO,
            drag: 0.0,
            start_size: 0.2,
            end_size: 0.2,
            start_color: Vec3::splat(0.7),
            end_color: Vec3::splat(0.7),
            start_alpha: 1.0,
            end_alpha: 0.0,
            max_particles: 1024,
            seed: 0,
            // Every M17 field defaults to the M13 behaviour: no disc, no
            // jitter, no turbulence, round sprites, alpha blending.
            blend: ParticleBlend::Alpha,
            radius: 0.0,
            speed_jitter: 0.0,
            size_jitter: 0.0,
            lifetime_jitter: 0.0,
            turbulence: 0.0,
            turbulence_scale: 1.0,
            stretch: 0.0,
        }
    }
}

/// What hangs on a tree's outermost branches.
///
/// NOTE: leave these variants undocumented, like [`ParticleBlend`]'s — a doc
/// comment on a variant turns the schema into oneOf/const and blinds the
/// validation walk's closed-vocabulary check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum TreeLeaf {
    #[default]
    Blade,
    Cluster,
    None,
}

/// A procedurally generated tree (M19): trunk, recursive branches, and leaves,
/// grown from a `seed` into geometry the renderer draws like any other mesh.
///
/// The tree is built in entity-local space with its **base at the origin,
/// growing along +Y**, so `"position": [x, 0, z]` plants it on flat ground and
/// `Transform.scale` sizes the whole thing. It replaces the entity's `Mesh`
/// rather than accompanying one (`tree_with_mesh`): the component *is* the
/// geometry.
///
/// # Two draws, two materials
///
/// A tree needs bark and foliage, and one `Material` cannot be both. The
/// entity's own `Material` is the **bark**; the leaves get [`Tree::leaf_color`]
/// and [`Tree::leaf_roughness`], and are always opaque and non-metallic. That
/// last part is a constraint, not an oversight: every leaf of one tree is a
/// single mesh, and the blended pass sorts whole draws, so translucent leaves
/// could not be sorted against each other and would visibly z-fight.
///
/// # Randomness
///
/// Everything jittered is drawn from a private xorshift seeded by `seed`,
/// consumed in a fixed order (a branch draws its own wander, then recurses into
/// its children in index order, then scatters its leaves), so two trees that
/// differ only in `seed` are different trees and the *same* tree is the same
/// mesh forever — which is what lets a forest sit under a pinned baseline.
/// `jitter` is the master dial for how much variation there is at all; `0`
/// grows a rigid diagram of a tree.
///
/// # Cost
///
/// Vertices scale as `(branches · whorl)^levels · segments · sides`, and a
/// tree that would blow past the budget is a validation error
/// (`tree_too_complex`) rather than a hang. `levels: 2` with the default
/// branching is ~1k vertices; `levels: 3` is ~6k. Generation happens once per
/// distinct parameter set and is cached, so a forest of nine trees is nine
/// meshes no matter how many frames render.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Tree {
    /// Seeds every random draw. Two trees with the same parameters and
    /// different seeds are different trees; the same seed always regrows the
    /// same tree.
    pub seed: u32,

    /// Trunk length in meters, before `Transform.scale`. `> 0`.
    #[schemars(extend("exclusiveMinimum" = 0.0))]
    pub height: f32,

    /// Trunk radius at the ground, in meters, before `Transform.scale`. `> 0`.
    #[schemars(extend("exclusiveMinimum" = 0.0))]
    pub trunk_radius: f32,

    /// How many generations of branches hang off the trunk. `0` is a bare
    /// pole; `2` reads as a tree; `3` is a good hero tree; `4` is expensive.
    /// `[0, 4]`.
    #[schemars(range(min = 0, max = 4))]
    pub levels: u32,

    /// Attachment points spaced along each parent branch. `[0, 16]`.
    #[schemars(range(min = 0, max = 16))]
    pub branches: u32,

    /// Branches emitted at each attachment point **on the trunk**, spread
    /// evenly around it. `1` (default) gives the alternate, spiralling
    /// arrangement of most broadleaf trees; `4`–`5` gives the whorls of a
    /// conifer, where a ring of limbs leaves the trunk at one height.
    ///
    /// Deeper levels are always alternate, which is both what a real conifer's
    /// limbs carry and what keeps the geometry finite — a whorl compounding at
    /// every level multiplies the tree by itself. `[1, 8]`.
    #[schemars(range(min = 1, max = 8))]
    pub whorl: u32,

    /// Angle in degrees between a child branch and its parent at the
    /// attachment. Small angles sweep up (poplar), large ones reach out
    /// (oak) or hang down (spruce, with negative `tropism`). `[0, 180]`.
    #[schemars(range(min = 0.0, max = 180.0))]
    pub branch_angle: f32,

    /// Degrees of rotation around the parent between successive attachment
    /// points. The default is the golden angle, which is what real phyllotaxis
    /// converges on and what stops branches from stacking into visible rows.
    pub branch_twist: f32,

    /// Fraction of a branch's length that carries no children — the bare
    /// trunk under the crown. `[0, 1)`.
    #[schemars(range(min = 0.0, max = 0.99))]
    pub branch_start: f32,

    /// Child length as a fraction of its parent's. `(0, 2]`.
    #[schemars(extend("exclusiveMinimum" = 0.0), range(max = 2.0))]
    pub length_ratio: f32,

    /// How much shorter children get toward the parent's tip, `[0, 1]`: `0`
    /// makes every child the same length (a round crown), `0.8` makes the top
    /// ones stubs (the cone of a conifer).
    #[schemars(range(min = 0.0, max = 1.0))]
    pub length_falloff: f32,

    /// Child radius as a fraction of the parent's radius where it attaches.
    /// `(0, 1]`.
    #[schemars(extend("exclusiveMinimum" = 0.0), range(max = 1.0))]
    pub radius_ratio: f32,

    /// Radius at a branch's tip as a fraction of its base. `[0, 1]`; small
    /// values give the sharp taper of a young shoot.
    #[schemars(range(min = 0.0, max = 1.0))]
    pub taper: f32,

    /// Extra radius at the very foot of the trunk, as a fraction: `0.4` makes
    /// the base 40% wider than the trunk above it. Root flare is most of what
    /// says "grown" rather than "placed", and it is at eye level. `>= 0`.
    #[schemars(range(min = 0.0))]
    pub flare: f32,

    /// Degrees of random wander per meter of branch. This is the gnarl: `0`
    /// grows perfectly straight poles, `8` is a healthy tree, `25` is an old
    /// olive. `>= 0`.
    #[schemars(range(min = 0.0))]
    pub crook: f32,

    /// Degrees per meter that a branch curves toward the sky as it grows.
    /// Positive lifts tips toward the light (most trees), negative lets
    /// gravity droop them (spruce, willow). This is what makes branches curve
    /// rather than point.
    ///
    /// It does not apply to the trunk, whose line is [`Tree::crook`] alone: a
    /// negative tropism on the trunk is unstable — a degree of crook tips it
    /// off vertical and the bend then compounds until the tree grows sideways.
    pub tropism: f32,

    /// How much every jittered quantity — branch length, radius, angle, leaf
    /// placement — varies, as a fraction. `[0, 1)`; `0` is a diagram.
    #[schemars(range(min = 0.0, max = 0.99))]
    pub jitter: f32,

    /// Radial sides of a branch tube. `[3, 16]`; `5`–`6` is plenty, since bark
    /// silhouettes read from the branching, not the cross-section.
    #[schemars(range(min = 3, max = 16))]
    pub sides: u32,

    /// Lengthwise segments per branch — how finely a branch can curve. `[1, 16]`.
    #[schemars(range(min = 1, max = 16))]
    pub segments: u32,

    /// What hangs on the outermost branches: `"blade"` (a folded leaf, the
    /// default), `"cluster"` (a foliage blob — cheaper per unit of cover, and
    /// what conifer sprays and distant trees want), or `"none"` for a bare
    /// winter or dead tree.
    pub leaf: TreeLeaf,

    /// Length of one leaf, or diameter of one cluster, in meters. `> 0`.
    #[schemars(extend("exclusiveMinimum" = 0.0))]
    pub leaf_size: f32,

    /// Leaves scattered along each outermost branch. `[0, 64]`; `0` is
    /// equivalent to `"leaf": "none"`.
    #[schemars(range(min = 0, max = 64))]
    pub leaves_per_branch: u32,

    /// Foliage albedo, linear RGB in `[0, 1]` — the leaves' half of the tree's
    /// appearance, the entity's `Material` being the bark's.
    #[schemars(with = "[f32; 3]", inner(range(min = 0.0, max = 1.0)))]
    pub leaf_color: Vec3,

    /// Foliage roughness, `[0, 1]`. Leaves are waxy, not matte: a little
    /// specular is what separates them from felt.
    #[schemars(range(min = 0.0, max = 1.0))]
    pub leaf_roughness: f32,
}

impl Default for Tree {
    fn default() -> Self {
        Self {
            seed: 0,
            height: 6.0,
            trunk_radius: 0.16,
            levels: 2,
            branches: 5,
            whorl: 1,
            branch_angle: 48.0,
            // The golden angle: 360° / φ².
            branch_twist: 137.5,
            branch_start: 0.35,
            length_ratio: 0.62,
            length_falloff: 0.35,
            radius_ratio: 0.6,
            taper: 0.12,
            flare: 0.4,
            crook: 8.0,
            tropism: 8.0,
            jitter: 0.25,
            sides: 6,
            segments: 5,
            leaf: TreeLeaf::Blade,
            leaf_size: 0.3,
            leaves_per_branch: 6,
            leaf_color: Vec3::new(0.09, 0.26, 0.08),
            leaf_roughness: 0.75,
        }
    }
}

/// One travelling wave in a [`Water`] surface's sum.
///
/// Gerstner rather than a sine: a Gerstner wave moves each point of the surface
/// *toward* the crests as well as up, which sharpens crests and flattens
/// troughs. A sum of sines is a rubber sheet, and no amount of tuning it makes
/// it read as water.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Wave {
    /// Heading the wave travels, in degrees: a yaw about +Y applied to the
    /// engine's forward axis, so `0` travels toward **−Z** and `90` toward
    /// **−X**. The same convention as aiming a camera, a light or a particle
    /// cone.
    pub direction: f32,

    /// Crest-to-crest distance in metres. `> 0`.
    ///
    /// This is the field that sets the *scale* of the water: 40 m swells read
    /// as ocean, 2 m chop as a lake, 0.4 m as a puddle. It must also be
    /// comfortably larger than one grid quad, or the surface cannot represent
    /// the wave — see [`Water::segments`].
    #[schemars(extend("exclusiveMinimum" = 0.0))]
    pub wavelength: f32,

    /// Half the crest-to-trough height, in metres. `>= 0`.
    #[schemars(range(min = 0.0))]
    pub amplitude: f32,

    /// How far the surface is pulled toward the crests, `[0, 1]`: 0 is a plain
    /// sine, 1 is a cusp.
    ///
    /// The **sum** over every wave must not exceed 1
    /// (`water_waves_self_intersect`). Past that the surface folds through
    /// itself and the crests curl into loops — a well-known Gerstner failure,
    /// and cheaper to refuse than to debug from a screenshot.
    #[schemars(range(min = 0.0, max = 1.0))]
    pub steepness: f32,

    /// Crest travel speed in metres/second. `>= 0`; reverse a wave with
    /// `direction`, not a negative speed.
    ///
    /// Real deep-water waves are dispersive (`c = sqrt(g·λ/2π)`, so long waves
    /// outrun short ones), and authoring speed per wave rather than deriving it
    /// keeps a scene free to be wrong about that on purpose — a lake's chop
    /// looks better slightly slower than physics says.
    #[schemars(range(min = 0.0))]
    pub speed: f32,
}

impl Default for Wave {
    fn default() -> Self {
        Self {
            direction: 0.0,
            wavelength: 4.0,
            amplitude: 0.06,
            steepness: 0.4,
            speed: 1.2,
        }
    }
}

impl Tree {
    /// The leaves' surface, assembled from the foliage fields. Opaque and
    /// non-metallic by construction — see the type's note on why leaves cannot
    /// be transparent.
    pub fn leaf_material(&self) -> Material {
        Material {
            albedo: self.leaf_color,
            metallic: 0.0,
            roughness: self.leaf_roughness,
            emissive: Vec3::ZERO,
            alpha: 1.0,
            transmission: 0.0,
        }
    }
}

/// A body of water: an ocean, a lake, a pond, a canal.
///
/// The entity owns its own surface geometry — a tessellated unit grid sized by
/// `Transform.scale`, exactly like a scaled `builtin:plane` — so a `Water`
/// entity carries **no** `Mesh` and no `Material` (`water_with_mesh`). One
/// surface is one entity: sixteen tiles pretending to be a pond is what this
/// component exists to delete, and their seams are visible in any screenshot.
///
/// Waves are evaluated in **world space** in the vertex stage, which has two
/// consequences worth knowing: scaling a surface never stretches its waves, and
/// two adjacent water entities at the same height share one continuous surface
/// for free.
///
/// Shading is water-specific rather than a `Material`: sky reflection with a
/// Fresnel-weighted view term, absorption of what is behind the surface with
/// depth (`shallow_color` → `deep_color`), and foam where the water meets
/// geometry or folds at a crest. What it does *not* do is refract — the bed of
/// a pond is not displaced by the ripples above it (`water-design.md` §8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Water {
    /// Quads per axis across the surface. `[1, 512]`.
    ///
    /// This is the resolution the *waves* are drawn at, and it is the one field
    /// worth thinking about: a wave needs roughly eight quads per wavelength to
    /// look like a wave rather than a fold, so a 14 m pond carrying 2 m chop
    /// wants ~64, and a 200 m ocean carrying 3 m chop cannot be drawn by any
    /// grid this component will generate. Detail *normals* are per pixel and
    /// cost nothing, which is why the glitter survives a coarse grid even
    /// though the silhouette does not.
    #[schemars(range(min = 1, max = 512))]
    pub segments: u32,

    /// The waves summed to shape the surface, at most
    /// [`MAX_WAVES`](crate::water::MAX_WAVES) of them. Empty (the default)
    /// leaves the surface flat: a mirror, which is what a sheltered pond at
    /// dawn actually looks like.
    #[schemars(length(max = 8))]
    pub waves: Vec<Wave>,

    /// Strength of the per-pixel ripple normals, `[0, 1]`.
    ///
    /// Small-scale roughness the grid is far too coarse to carry as geometry,
    /// perturbing the normal and nothing else. Per line of code this is the
    /// biggest single difference between "blue glass" and "water", because it
    /// is what breaks the sun and the sky into glitter between the vertices.
    /// Nothing physical may depend on it — no buoyancy, no collision.
    #[schemars(range(min = 0.0, max = 1.0))]
    pub detail: f32,

    /// Size of one ripple cell in metres. `> 0`. Around 0.5 reads as wind
    /// texture on a lake; 3 or more as a swell the grid did not resolve.
    #[schemars(extend("exclusiveMinimum" = 0.0))]
    pub detail_scale: f32,

    /// Surface roughness, `[0, 1]`, meaning what `Material.roughness` means.
    ///
    /// Water is smooth, but not 0: a mirror-tight sun highlight is a single
    /// blown-out pixel that aliases as the camera moves, and the reflected sky
    /// carries most of the look anyway.
    #[schemars(range(min = 0.0, max = 1.0))]
    pub roughness: f32,

    /// Linear RGB of water one `depth_fade` deep or less — the colour at the
    /// shoreline. Each component `[0, 1]`.
    #[schemars(with = "[f32; 3]", inner(range(min = 0.0, max = 1.0)))]
    pub shallow_color: Vec3,

    /// Linear RGB of water far deeper than `depth_fade`. Each component
    /// `[0, 1]`.
    #[schemars(with = "[f32; 3]", inner(range(min = 0.0, max = 1.0)))]
    pub deep_color: Vec3,

    /// Metres of water the view has to pass through to reach `deep_color` and
    /// full `opacity`. `> 0`.
    ///
    /// Beer-Lambert absorption against the depth of whatever is behind the
    /// surface, so the same water is clear at the edge of a pond and opaque in
    /// the middle — which is most of how a surface reads as *deep* rather than
    /// as a coloured pane. A clear alpine lake is 6 or more; a silty pond is
    /// under 1.
    #[schemars(extend("exclusiveMinimum" = 0.0))]
    pub depth_fade: f32,

    /// How opaque deep water becomes, `[0, 1]`. 1 hides its bed completely
    /// however clear the shallows are.
    #[schemars(range(min = 0.0, max = 1.0))]
    pub opacity: f32,

    /// Foam on the crests, `[0, 1]`; 0 (the default) is off.
    ///
    /// Driven by the Gerstner Jacobian — where the surface pinches toward
    /// folding, which is exactly where a real wave breaks — so it appears only
    /// on steep waves and needs no second noise field to place it.
    #[schemars(range(min = 0.0, max = 1.0))]
    pub crest_foam: f32,

    /// Width in metres of the foam line where the surface meets geometry.
    /// `>= 0`; 0 (the default) is off.
    ///
    /// This is the shoreline, and it is also the waterline on anything standing
    /// in the water: it comes from the depth behind the surface, so a boat, an
    /// ice block and the bank all get one without being marked up.
    #[schemars(range(min = 0.0))]
    pub shore_foam: f32,

    /// Linear RGB of both foam kinds, each component `[0, 1]`. Foam is
    /// scattered light, so it is opaque where it appears.
    #[schemars(with = "[f32; 3]", inner(range(min = 0.0, max = 1.0)))]
    pub foam_color: Vec3,
}

impl Default for Water {
    fn default() -> Self {
        Self {
            segments: 64,
            // Absent means off, as everywhere else in the engine: a bare
            // `{"type": "Water"}` is a flat, clear, reflective surface, and
            // every feature past that is asked for by name.
            waves: Vec::new(),
            detail: 0.5,
            detail_scale: 0.6,
            roughness: 0.06,
            shallow_color: Vec3::new(0.09, 0.20, 0.21),
            deep_color: Vec3::new(0.01, 0.05, 0.08),
            depth_fade: 2.5,
            opacity: 0.94,
            crest_foam: 0.0,
            shore_foam: 0.0,
            foam_color: Vec3::new(0.86, 0.90, 0.92),
        }
    }
}

/// A cloud: a cumulus, a raft of stratocumulus, a storm anvil, a torn wisp.
///
/// A recipe rather than a mesh reference, like [`Tree`] — the engine grows it
/// into a cluster of lobes, each of which grows smaller lobes on itself, seeded
/// so two clouds with the same parameters and different seeds are different
/// clouds. The entity owns that geometry, sized by `Transform.scale` like a
/// water surface, so a `Cloud` entity carries **no** `Mesh` and **no**
/// `Material` (`cloud_with_mesh`). A cloud is not a GGX surface: what a
/// `Material` describes, `color` / `shade_color` / `density` / `feather`
/// describe instead.
///
/// Non-uniform scale is the normal case, not an edge case — `scale: [24, 12,
/// 24]` is what makes a cumulus wider than it is tall, and it oblates the lobes
/// with it.
///
/// Shading is three cheap stand-ins for multiple scattering, none of which is
/// volumetric: wrapped diffuse between `shade_color` and `color`, a forward-
/// scattering silver lining when the camera looks toward the sun, and an alpha
/// that fades toward each lobe's own silhouette. Clouds do not cast shadows
/// (the engine has one shadow cascade and it is fitted to the camera, not to a
/// cloud at altitude) and are not lit by `PointLight`s.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Cloud {
    /// Seeds every random draw. Two clouds with the same parameters and
    /// different seeds are different clouds; the same seed always regrows the
    /// same cloud.
    pub seed: u32,

    /// Lobes in the base cluster, spread over the footprint on a golden-angle
    /// spiral. `[1, 32]`; a handful reads as one cumulus, a dozen or more as a
    /// raft.
    #[schemars(range(min = 1, max = 32))]
    pub lobes: u32,

    /// Generations of smaller lobes piled on the base ones. `[0, 3]`: `0` is a
    /// cluster of plain spheres, `2` reads as cauliflower, `3` is expensive.
    #[schemars(range(min = 0, max = 3))]
    pub levels: u32,

    /// Lobes grown on each lobe of the previous generation. `[0, 8]`.
    #[schemars(range(min = 0, max = 8))]
    pub children: u32,

    /// Diameter of a base lobe as a fraction of the cloud's own size, `(0, 1]`.
    /// Large values give a few fat billows, small ones a curdled texture.
    #[schemars(extend("exclusiveMinimum" = 0.0), range(max = 1.0))]
    pub lobe_size: f32,

    /// Child lobe radius as a fraction of its parent's, `(0, 1]`. This is the
    /// dial that makes the silhouette detailed at more than one scale — at 1
    /// every lobe is the same size and the cloud reads as popcorn.
    #[schemars(extend("exclusiveMinimum" = 0.0), range(max = 1.0))]
    pub lobe_ratio: f32,

    /// How much the cloud sits on a flat base, `[0, 1]`. `0` is a puffball with
    /// lobes scattered through its whole box; `1` seats every lobe on the base
    /// plane and folds what hangs below onto it.
    ///
    /// A cumulus has a flat bottom because condensation begins at one altitude,
    /// which is why every fair-weather cloud in a field shares a base — it is
    /// the cheapest of this component's cues and one of the most legible.
    #[schemars(range(min = 0.0, max = 1.0))]
    pub flatten: f32,

    /// How strongly child lobes are biased toward the sky, `[0, 1]`. A cumulus
    /// is a convection cell: its detail is on top, where the air is still
    /// rising, and its underside is smooth. At `0` children scatter in every
    /// direction and the cloud reads as a sea urchin.
    #[schemars(range(min = 0.0, max = 1.0))]
    pub rise: f32,

    /// Smooth radial distortion of each lobe, as a fraction of its radius.
    /// `[0, 1)`; a little is what stops a lobe from reading as a ball.
    #[schemars(range(min = 0.0, max = 0.99))]
    pub wobble: f32,

    /// How much every jittered quantity — lobe radius, placement, child size —
    /// varies, as a fraction. `[0, 1)`; `0` is a diagram.
    #[schemars(range(min = 0.0, max = 0.99))]
    pub jitter: f32,

    /// Icosphere subdivisions per lobe: 12, 42, 162 or 642 vertices. `[0, 3]`.
    /// This is the quality dial, and `2` is plenty for anything at cloud
    /// distance.
    #[schemars(range(min = 0, max = 3))]
    pub detail: u32,

    /// How opaque the cloud is where it is thickest, `[0, 1]`. Lobes do not
    /// write depth, so overlapping ones accumulate — which is a cheap stand-in
    /// for optical depth, and why a wisp wants a much lower value than a storm.
    #[schemars(range(min = 0.0, max = 1.0))]
    pub density: f32,

    /// How crisp the cloud's edges are, `[0, 8]`. Alpha follows
    /// `1 - (1 - facing)^feather` as the surface turns away from the camera, so
    /// **higher is crisper** and low values are wispy: 1 fades the whole
    /// surface proportionally, 3 keeps the body opaque and thins only the last
    /// few degrees before the silhouette.
    ///
    /// It is doing two jobs. A real cloud's silhouette is where it thins out,
    /// not where its geometry stops — and the same fade is what hides the
    /// boundaries *between* two interpenetrating lobes, since each of them
    /// vanishes exactly where its surface turns away.
    #[schemars(range(min = 0.0, max = 8.0))]
    pub feather: f32,

    /// Linear RGB of the sunlit side. Each component `[0, 1]`.
    #[schemars(with = "[f32; 3]", inner(range(min = 0.0, max = 1.0)))]
    pub color: Vec3,

    /// Linear RGB of the self-shadowed side, each component `[0, 1]`.
    ///
    /// Blue-grey rather than grey by default, and that is the point: the
    /// underside of a cloud is lit by the sky above it, not by the sun it is
    /// hiding from. Darkening this toward slate is most of what makes a storm.
    #[schemars(with = "[f32; 3]", inner(range(min = 0.0, max = 1.0)))]
    pub shade_color: Vec3,

    /// World-space metres per second the cloud travels, evaluated against the
    /// scene clock (`--time`, else `steps / timestep_hz`) — so a drifting sky
    /// is as reproducible as a wave, and a script is not needed to move it.
    ///
    /// The cloud's *shape* never changes with time. Regenerating lobes per
    /// frame would mint a new mesh every frame and defeat the renderer's upload
    /// cache; in this engine, generated geometry is made once.
    #[schemars(with = "[f32; 3]")]
    pub drift: Vec3,

    /// Metres after which a drifting cloud recycles to where it started. `>= 0`;
    /// `0` (the default) lets it drift away for good.
    ///
    /// Wrapping *teleports* the cloud, so it wants to be wider than the view or
    /// far enough out that fog has already eaten it before it jumps.
    #[schemars(range(min = 0.0))]
    pub drift_wrap: f32,
}

impl Default for Cloud {
    fn default() -> Self {
        Self {
            seed: 0,
            lobes: 6,
            levels: 2,
            children: 3,
            lobe_size: 0.42,
            lobe_ratio: 0.55,
            // Absent is off, as everywhere else in the engine: a bare
            // `{"type": "Cloud"}` is a puffball, and a flat base is asked for
            // by name.
            flatten: 0.0,
            rise: 0.35,
            wobble: 0.12,
            jitter: 0.3,
            detail: 2,
            density: 0.9,
            feather: 3.0,
            color: Vec3::new(1.0, 0.98, 0.95),
            shade_color: Vec3::new(0.42, 0.46, 0.58),
            drift: Vec3::ZERO,
            drift_wrap: 0.0,
        }
    }
}

/// A patch of ground: displaced terrain with a procedurally shaded surface
/// (M22).
///
/// The entity carries **no** [`Mesh`] and **no** [`Material`] — `Terrain` owns
/// both, like [`Water`] — and having either is `terrain_with_mesh`. Geometry is
/// a tessellated unit grid sized by `Transform.scale`, displaced by an fBm
/// height field; `Transform.scale.y` multiplies that displacement, so [`height`]
/// is what you get at scale 1.
///
/// Heights are sampled in **world** XZ, so two patches with the same fields meet
/// seamlessly and moving one moves it *through* the field rather than dragging
/// its hills along.
///
/// Unlike water's waves, the height field is evaluated on the **CPU**: terrain
/// does not animate, so the surface is generated once and cached, and there is
/// exactly one implementation for the renderer, the collider (a `trimesh`
/// `Collider` with no asset uses this surface) and
/// `world.terrain_height(name, x, z)` to share. Appearance is the opposite —
/// per-pixel, in the shader, mirrored by nothing, which is what licenses detail
/// far finer than the grid.
///
/// [`height`]: Terrain::height
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Terrain {
    /// Quads per axis across the patch. `[1, 512]`.
    ///
    /// The resolution the *relief* is drawn and collided at. What matters is
    /// metres per quad against [`feature_scale`](Terrain::feature_scale): a
    /// 200 m patch at 192 has one vertex per metre, which resolves a 40 m hill
    /// comfortably and a 3 m hummock not at all. Surface *detail* is per pixel
    /// and does not care.
    #[schemars(range(min = 1, max = 512))]
    pub segments: u32,

    /// Chooses the landscape. Any change reshapes every hill.
    ///
    /// The noise hash is written out in this crate rather than pulled from a
    /// dependency, so a given seed means the same terrain across upgrades — a
    /// terrain render sits under a `diff-render` baseline, which makes this a
    /// format contract.
    pub seed: u32,

    /// Metres of displacement at full amplitude, `>= 0`.
    ///
    /// The field is normalised to `[-1, 1]` before scaling, so this is the peak
    /// above (and below) the entity's own Y, and **adding octaves adds detail
    /// rather than altitude**. 0 is a flat patch, which is a legitimate thing to
    /// ask for and is still shaded by the layer system.
    #[schemars(range(min = 0.0))]
    pub height: f32,

    /// Metres across one cell of the largest noise octave. `> 0`.
    ///
    /// The size of the big rolling features, and the field that decides whether
    /// a patch reads as dunes, as pasture or as foothills. Well under
    /// `segments`-worth of quads and the grid cannot resolve it; far larger than
    /// the patch and the ground reads as a tilted plane.
    #[schemars(extend("exclusiveMinimum" = 0.0))]
    pub feature_scale: f32,

    /// How many octaves of noise are summed. `[1, 8]`.
    ///
    /// Each octave halves the feature size and scales its amplitude by
    /// [`persistence`](Terrain::persistence). Past about 5 the added detail is
    /// finer than the grid can carry and only costs generation time.
    #[schemars(range(min = 1, max = 8))]
    pub octaves: u32,

    /// Amplitude multiplier per octave, `[0, 1]`.
    ///
    /// Low values give smooth swells; near 1 gives a rough, noisy surface with
    /// no clear large-scale shape. 0.5 is the usual landscape.
    #[schemars(range(min = 0.0, max = 1.0))]
    pub persistence: f32,

    /// Domain warp: how far the field is dragged sideways before it is summed,
    /// as a fraction of [`feature_scale`](Terrain::feature_scale). `[0, 2]`;
    /// 0 (the default) is off.
    ///
    /// Two lines of arithmetic, and the largest single difference between "fBm"
    /// and "landscape". Unwarped fBm is isotropic blobs; warping shears them
    /// into ridges and valleys that read as though water once ran over them.
    /// Past ~1 the surface starts to look smeared.
    #[schemars(range(min = 0.0, max = 2.0))]
    pub warp: f32,

    /// The materials the surface is painted with, at most
    /// [`MAX_TERRAIN_LAYERS`](crate::terrain::MAX_TERRAIN_LAYERS), blended by
    /// height and slope.
    ///
    /// Empty (the default) paints the whole surface with
    /// [`TerrainLayer`]'s own defaults, so a bare `{"type": "Terrain"}` is a
    /// plausible grassy patch rather than an error or a blank.
    #[schemars(length(max = 4))]
    pub layers: Vec<TerrainLayer>,

    /// Metres across one cell of the surface-detail noise. `> 0`.
    ///
    /// The scale of the mottling within a layer and of the fingers along the
    /// boundary between two — the *texture*, as opposed to the relief. Around a
    /// few metres reads as ground cover seen from standing height.
    #[schemars(extend("exclusiveMinimum" = 0.0))]
    pub texture_scale: f32,

    /// How strongly the detail noise modulates the blended albedo, `[0, 1]`.
    ///
    /// The cure for the one-flat-colour look: even a single-layer terrain stops
    /// being a sheet of paint. Past ~0.5 the ground reads as camouflage.
    #[schemars(range(min = 0.0, max = 1.0))]
    pub color_variation: f32,

    /// Per-pixel normal perturbation from the detail noise, `[0, 1]`.
    ///
    /// Bumpiness with no displacement behind it — nothing physical may depend on
    /// it, which is what allows detail far finer than the grid or the collider.
    /// It fades with view distance, because sub-pixel normal variation aliases
    /// into sparkle that reads as broken rather than as low quality (the lesson
    /// water's detail ripples already paid for).
    #[schemars(range(min = 0.0, max = 1.0))]
    pub bump: f32,
}

impl Default for Terrain {
    fn default() -> Self {
        Self {
            segments: 128,
            seed: 0,
            height: 2.0,
            feature_scale: 40.0,
            octaves: 4,
            persistence: 0.5,
            warp: 0.0,
            layers: Vec::new(),
            texture_scale: 4.0,
            color_variation: 0.25,
            bump: 0.3,
        }
    }
}

/// One material a [`Terrain`] paints itself with, claiming a band of height and
/// a band of slope.
///
/// A pixel's surface is the weighted average of every layer whose bands it falls
/// inside; a layer that leaves both at their defaults covers everything and is
/// the base coat.
///
/// **Slope does the heavy lifting.** Height alone gives horizontal stripes — a
/// contour map, and unmistakably so once the camera moves. What separates rock
/// from grass in the world is that soil cannot cling to a steep face, so
/// `slope_range` is the selector that reads as real and `height_range` adds
/// bands of climate on top of it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct TerrainLayer {
    /// Linear RGB, each component `[0, 1]` — `Material.albedo` for this band.
    #[schemars(with = "[f32; 3]", inner(range(min = 0.0, max = 1.0)))]
    pub albedo: Vec3,

    /// Perceptual roughness, `[0, 1]`, meaning what `Material.roughness` means.
    /// Wet rock and packed sand are smoother than grass, and the difference
    /// shows wherever the sun is low.
    #[schemars(range(min = 0.0, max = 1.0))]
    pub roughness: f32,

    /// World-space Y band this layer covers, in metres, `[min, max]`.
    ///
    /// World Y, not a fraction of [`Terrain::height`], so a layer's altitude
    /// does not shift when the relief is retuned — and so that two patches at
    /// different elevations can share one description of what grows where.
    #[schemars(with = "[f32; 2]")]
    pub height_range: [f32; 2],

    /// Surface-angle band this layer covers, in degrees from horizontal,
    /// `[min, max]`. 0 is flat ground, 90 a vertical face.
    #[schemars(with = "[f32; 2]", inner(range(min = 0.0, max = 90.0)))]
    pub slope_range: [f32; 2],

    /// Metres over which this layer fades out beyond each end of
    /// `height_range`. `>= 0`; 0 is a hard edge, which reads as a cut line and
    /// is almost never what ground does.
    ///
    /// Absolute, not a fraction of the band — that was tried first and is a
    /// trap. A fraction means a wide band gets a wide fade, so a layer written
    /// as "above 1.9 m" with a generous top end bleeds ten metres *below* where
    /// it was aimed and washes out everything under it. In metres, what the
    /// author writes is what the surface does.
    #[schemars(range(min = 0.0))]
    pub height_blend: f32,

    /// Degrees over which this layer fades out beyond each end of
    /// `slope_range`. `>= 0`.
    ///
    /// Separate from [`height_blend`](TerrainLayer::height_blend) because the
    /// two bands are in different units and one number cannot be honest about
    /// both.
    #[schemars(range(min = 0.0))]
    pub slope_blend: f32,

    /// How much the detail noise jitters this layer's boundary, `[0, 1]`, as a
    /// fraction of its own fade widths.
    ///
    /// Drawn honestly, the boundary between two layers is an iso-line of a
    /// smooth function — a clean sweeping curve, and the eye reads clean curves
    /// as artificial faster than it reads anything else here. Jittering the
    /// height and slope the layer *thinks* it is at breaks the boundary into
    /// interlocking fingers at two scales, for one multiply-add.
    #[schemars(range(min = 0.0, max = 1.0))]
    pub noise: f32,
}

impl Default for TerrainLayer {
    fn default() -> Self {
        Self {
            // A plausible dry grass, so `{"type": "Terrain"}` on its own draws
            // ground rather than the 0.8 grey a missing material would give.
            albedo: Vec3::new(0.13, 0.17, 0.09),
            roughness: 0.95,
            // Bands that cover everything: absent means "applies here", so the
            // first layer an author writes is the base coat without having to
            // say so.
            height_range: [-1000.0, 1000.0],
            slope_range: [0.0, 90.0],
            height_blend: 0.5,
            slope_blend: 8.0,
            noise: 0.5,
        }
    }
}

/// Defines the serialized component union alongside everything that must stay
/// in step with it.
///
/// The name list feeds `did_you_mean` suggestions and the spawn arm feeds
/// hecs; generating all three from one list is what stops a new component from
/// being loadable but unsuggestable, or schema'd but never spawned.
macro_rules! components {
    ($($variant:ident),* $(,)?) => {
        /// One component as it appears in a scene file.
        ///
        /// Internally tagged on `"type"`, so each component is a flat object
        /// rather than a single-key wrapper — the shape `jq` is pleasant
        /// against and the shape an LLM is least likely to get wrong.
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
        #[serde(tag = "type")]
        pub enum ComponentData {
            $($variant($variant),)*
        }

        impl ComponentData {
            /// Every known component name, for error messages and suggestions.
            pub const NAMES: &'static [&'static str] = &[$(stringify!($variant)),*];

            /// This component's `"type"` value.
            pub fn name(&self) -> &'static str {
                match self {
                    $(Self::$variant(_) => stringify!($variant),)*
                }
            }

            /// Attach to an entity under construction.
            pub fn add_to(self, builder: &mut EntityBuilder) {
                match self {
                    $(Self::$variant(component) => { builder.add(component); })*
                }
            }

            /// Every component the entity currently holds, in declaration
            /// order — how bake serializes an entity the run spawned (a
            /// break's fragments) back into scene JSON.
            pub fn collect_from(world: &hecs::World, entity: hecs::Entity) -> Vec<ComponentData> {
                let mut components = Vec::new();
                $(
                    if let Ok(c) = world.get::<&$variant>(entity) {
                        components.push(Self::$variant((*c).clone()));
                    }
                )*
                components
            }
        }
    };
}

components!(
    Transform,
    Mesh,
    Camera,
    Material,
    DirectionalLight,
    AmbientLight,
    PointLight,
    RigidBody,
    Collider,
    Breakable,
    AnimationPlayer,
    Script,
    Wheel,
    HudText,
    HudRect,
    ParticleEmitter,
    Tree,
    Water,
    Cloud,
    Terrain,
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transform_omits_default_fields() {
        let t: Transform = serde_json::from_str(r#"{"position": [0.0, 3.0, 0.0]}"#).unwrap();
        assert_eq!(t.position, Vec3::new(0.0, 3.0, 0.0));
        assert_eq!(t.rotation, Vec3::ZERO);
        assert_eq!(t.scale, Vec3::ONE, "omitted scale must default to 1, not 0");
    }

    #[test]
    fn rotation_is_euler_degrees_xyz() {
        // Pins the file-format convention (design doc §5): degrees, X→Y→Z
        // intrinsic. +90° about Y carries +X to -Z in a right-handed Y-up
        // space; if someone swaps the order or forgets to_radians, this fails.
        let t: Transform = serde_json::from_str(r#"{"rotation": [0.0, 90.0, 0.0]}"#).unwrap();
        let rotated = t.quat().mul_vec3(Vec3::X);
        assert!(
            (rotated - Vec3::NEG_Z).length() < 1e-5,
            "+X should rotate to -Z, got {rotated:?}"
        );
    }

    #[test]
    fn components_are_internally_tagged() {
        let json = r#"{"type": "Mesh", "asset": "builtin:cube"}"#;
        let c: ComponentData = serde_json::from_str(json).unwrap();
        assert_eq!(
            c,
            ComponentData::Mesh(Mesh {
                asset: "builtin:cube".into()
            })
        );
        assert_eq!(c.name(), "Mesh");
    }

    #[test]
    fn round_trips_through_json() {
        let original = ComponentData::Transform(Transform {
            position: Vec3::new(1.0, 2.0, 3.0),
            ..Default::default()
        });
        let json = serde_json::to_string(&original).unwrap();
        assert_eq!(
            serde_json::from_str::<ComponentData>(&json).unwrap(),
            original
        );
    }

    #[test]
    fn rejects_unknown_fields() {
        // A typo'd field must not be silently dropped — that is the failure
        // mode where an agent "fixes" something and nothing changes. This also
        // pins that `deny_unknown_fields` survives internal tagging, which is a
        // known serde sharp edge.
        let err = serde_json::from_str::<ComponentData>(
            r#"{"type": "Transform", "postion": [0.0, 1.0, 0.0]}"#,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("postion"),
            "expected the unknown field to be named, got: {err}"
        );
    }

    #[test]
    fn every_component_name_is_listed() {
        // The macro generates NAMES, so this checks the macro rather than a
        // hand-written list — it catches a variant added without a name.
        assert_eq!(
            ComponentData::NAMES,
            &[
                "Transform",
                "Mesh",
                "Camera",
                "Material",
                "DirectionalLight",
                "AmbientLight",
                "PointLight",
                "RigidBody",
                "Collider",
                "Breakable",
                "AnimationPlayer",
                "Script",
                "Wheel",
                "HudText",
                "HudRect",
                "ParticleEmitter",
                "Tree",
                "Water",
                "Cloud",
                "Terrain"
            ]
        );
    }

    #[test]
    fn light_defaults_match_the_design() {
        let sun: DirectionalLight = serde_json::from_str("{}").unwrap();
        assert_eq!(sun.color, Vec3::ONE);
        assert_eq!(sun.intensity, 1.0);

        let ambient: AmbientLight = serde_json::from_str("{}").unwrap();
        assert_eq!(ambient.color, Vec3::ONE);
        assert_eq!(ambient.intensity, 0.05);

        let material: Material = serde_json::from_str("{}").unwrap();
        assert_eq!(material.emissive, Vec3::ZERO, "emissive defaults to off");
    }

    #[test]
    fn lights_round_trip_through_json() {
        let original = ComponentData::DirectionalLight(DirectionalLight {
            color: Vec3::new(1.0, 0.9, 0.8),
            intensity: 2.5,
        });
        let json = serde_json::to_string(&original).unwrap();
        assert_eq!(
            serde_json::from_str::<ComponentData>(&json).unwrap(),
            original
        );
    }
}
