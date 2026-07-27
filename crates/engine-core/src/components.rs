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
}

impl Default for Material {
    fn default() -> Self {
        Self {
            albedo: Vec3::splat(0.8),
            metallic: 0.0,
            roughness: 0.9,
            emissive: Vec3::ZERO,
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
}

/// Collision geometry (M8). Requires a `Transform`. With no `RigidBody` on
/// the entity, this is static collision geometry — the common case for
/// ground planes and walls.
///
/// One flat object discriminated by `shape` (the shape `jq` and an LLM
/// handle best): `cuboid` uses `half_extents`, `sphere` uses `radius`,
/// `capsule` (Y-axis) uses `half_height` + `radius`. Validation enforces
/// which fields each shape requires and forbids — the file format is the
/// contract, the flat Rust struct is how it stays walkable by the
/// schema-driven validator and the editor's generated inspector.
///
/// `Transform.scale` scales the shape when the physics world is built — a
/// cube scaled 2x collides 2x big, which is what the screenshot shows.
/// Nonuniform scale on a round shape has no physics representation and is a
/// validation error, never a silent approximation.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
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
    RigidBody,
    Collider,
    AnimationPlayer,
    Script,
    HudText,
    HudRect,
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
                "RigidBody",
                "Collider",
                "AnimationPlayer",
                "Script",
                "HudText",
                "HudRect"
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
