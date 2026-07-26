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

use glam::{Quat, Vec3};
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

/// Renderable geometry, referenced by relative path (invariant 3).
///
/// Until M3 lands glTF loading, only the `builtin:` primitives resolve; see
/// [`crate::mesh::BuiltinMesh`]. A real path is accepted by the schema and by
/// `engine validate` — it is a valid scene — but fails at render time with a
/// structured error rather than silently drawing nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Mesh {
    pub asset: String,
}

/// A viewpoint. `engine screenshot --camera <name>` selects one by entity name.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Camera {
    /// Vertical field of view, in degrees.
    pub fov: f32,
    pub near: f32,
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

/// Surface appearance. Consumed properly at M4; at M2 only `albedo` is used,
/// as a flat unlit color.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Material {
    #[schemars(with = "[f32; 3]")]
    pub albedo: Vec3,
    pub metallic: f32,
    pub roughness: f32,
}

impl Default for Material {
    fn default() -> Self {
        Self {
            albedo: Vec3::splat(0.8),
            metallic: 0.0,
            roughness: 0.9,
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
        }
    };
}

components!(Transform, Mesh, Camera, Material);

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
            &["Transform", "Mesh", "Camera", "Material"]
        );
    }
}
