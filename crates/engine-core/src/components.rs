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
///
/// **One world unit is one metre**, and that is the convention every other
/// number in the format is quoted against: gravity is `-9.81` because a body
/// falls 9.81 m/s², `Tree.height: 6.0` is a six-metre tree, and a `Wheel`
/// 0.35 m in radius belongs under a car 1.7 m wide. Time is seconds and mass
/// is kilograms, so `Collider.density` is kg/m³ — note its default of `1.0`
/// is *not* a plausible material, and a body meant to be pushed by forces
/// wants a real one (the demo car's box chassis carries `350`, which is how
/// 4.3 m³ becomes 1.5 t).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Transform {
    /// World-space `[x, y, z]` **in metres**. +Y is up.
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

    /// Multiplier on the entity's own geometry, per axis. Identity is
    /// `[1, 1, 1]`.
    ///
    /// **Every `builtin:` primitive is one metre across at scale 1**, so on
    /// those this field reads directly as a size in metres: a
    /// `builtin:cube` at `[1.7, 0.7, 3.6]` is a car-sized box, and a
    /// `builtin:sphere` at `[0.9, 0.9, 0.9]` is 0.9 m across — *not* 1.8.
    /// The recipes size the same way (`Terrain`, `Water` and `Meadow` take
    /// their footprint from `scale` in XZ), and it also multiplies
    /// `Collider` dimensions, which is why a cuboid collider matching a
    /// builtin cube is authored as `half_extents: [0.5, 0.5, 0.5]` in
    /// *local* units rather than in metres.
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
///
/// Since M26 a material may instead be a **file**: `{"type": "Material",
/// "asset": "materials/asphalt.json"}` names a JSON document holding these same
/// fields minus the `"type"`. `asset` is exclusive with every other field —
/// setting both is `material_asset_with_fields` — because serde cannot tell an
/// absent field from one written at its default, so a partial override would
/// resolve to something the file does not say. A variant is a second file.
#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Material {
    /// A `materials/*.json` file holding this material, relative to the scene
    /// file (invariant 3). Exclusive with every other field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asset: Option<String>,

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
    /// tinted by its thickness.
    #[schemars(range(min = 0.0, max = 1.0))]
    pub transmission: f32,

    /// An sRGB colour texture, **multiplied** by `albedo` (M26).
    ///
    /// A tint over the map, not a replacement — which means the default
    /// `[0.8, 0.8, 0.8]` darkens an imported texture by 20% unless the file
    /// says `"albedo": [1, 1, 1]` beside the map. `engine import` writes that
    /// explicitly; a hand-authored material has to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub albedo_map: Option<String>,

    /// Occlusion in R, roughness in G, metallic in B — glTF's packing, so an
    /// import is a file copy rather than a channel re-pack. Linear data, never
    /// colour. R multiplies the ambient and sky terms only, never the direct
    /// sun: that is what makes it *ambient* occlusion rather than a second
    /// shadow. G and B multiply `roughness` and `metallic`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub orm_map: Option<String>,

    /// A tangent-space normal map, linear data. The tangent frame is derived
    /// per pixel from screen-space derivatives rather than stored per vertex,
    /// so this works unmodified on `Water`, `Terrain`, `Road`, `Tree` and
    /// `Cloud` geometry, none of which carries tangents.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub normal_map: Option<String>,

    /// An sRGB colour texture multiplied by `emissive`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emissive_map: Option<String>,

    /// Tiling for every map on this material: UV × `uv_scale` + `uv_offset`.
    /// Sampling repeats on both axes, so `[20, 20]` is twenty tiles.
    #[schemars(with = "[f32; 2]")]
    pub uv_scale: Vec2,
    #[schemars(with = "[f32; 2]")]
    pub uv_offset: Vec2,

    /// Above 0, a pixel whose `albedo_map` alpha falls below this is
    /// discarded — and so is its shadow, through a second caster pipeline with
    /// a fragment stage. This is what an alpha-cut leaf needs. Range `[0, 1]`;
    /// `0` (the default) cuts nothing, so the depth-only caster pass every
    /// current scene uses is untouched.
    #[schemars(range(min = 0.0, max = 1.0))]
    pub alpha_cutoff: f32,

    /// Scales the normal map's tangent-space XY. The first thing anyone does
    /// with a normal map is discover it is too strong, and the alternative is
    /// re-authoring the texture.
    #[schemars(range(min = 0.0, max = 8.0))]
    pub normal_strength: f32,

    /// Index of refraction, `1.0` (the default) being no bending at all.
    /// Read only by a transmissive surface, where it refracts the view vector
    /// against the shading normal and offsets the scene-colour sample. Range
    /// `[1, 3]` — glass is 1.5, water 1.33, diamond 2.4.
    #[schemars(range(min = 1.0, max = 3.0))]
    pub ior: f32,

    /// How far light travels inside the surface, in metres. Both the scale of
    /// the refraction offset and the Beer–Lambert path length, so a thick block
    /// of ice is finally greener than a thin one. `0` is the pre-M26 behaviour.
    #[schemars(range(min = 0.0))]
    pub thickness: f32,

    /// What survives that path, per linear-RGB channel: transmitted colour is
    /// scaled by `exp(-(1 - attenuation) * thickness)`. `[1, 1, 1]` absorbs
    /// nothing.
    #[schemars(with = "[f32; 3]", inner(range(min = 0.0, max = 1.0)))]
    pub attenuation: Vec3,
}

impl Material {
    /// Whether this material draws in the blended pass rather than the opaque
    /// one. Exactly the pre-M16 opaque path when both fields sit at their
    /// defaults, which is what keeps every committed baseline bit-exact.
    pub fn is_transparent(&self) -> bool {
        self.alpha < 1.0 || self.transmission > 0.0
    }

    /// Whether this material samples anything — the test that routes a draw to
    /// the textured pipeline variant (M26).
    ///
    /// A material with no maps compiles and draws through the pipeline that
    /// compiles `mesh.wgsl` as it sits on disk, rather than through a textured
    /// pipeline with white textures bound. `x * 1.0` is exact in IEEE-754, but
    /// that was never the risk: the risk is that inserting the multiply changes
    /// the code *around* M16's four untouchable lines, and whether the compiler
    /// contracts `a*b + c` into an FMA depends on exactly that.
    pub fn has_maps(&self) -> bool {
        self.albedo_map.is_some()
            || self.orm_map.is_some()
            || self.normal_map.is_some()
            || self.emissive_map.is_some()
    }

    /// Whether this material bends what is behind it — the gate on the frame's
    /// colour copy. With none in a scene, the pass structure, the attachments
    /// and the load/store ops are byte for byte the pre-M26 ones.
    pub fn refracts(&self) -> bool {
        self.is_transparent() && (self.ior != 1.0 || self.thickness > 0.0)
    }

    /// Every texture reference this material makes, with the colour space its
    /// slot reads it in. **The slot decides the space** — never the file, never
    /// a field — which is what makes the most common texture bug in any engine
    /// unrepresentable.
    pub fn maps(&self) -> impl Iterator<Item = (&str, &str, crate::texture::ColorSpace)> {
        use crate::texture::ColorSpace::{Linear, Srgb};
        [
            ("albedo_map", self.albedo_map.as_deref(), Srgb),
            ("orm_map", self.orm_map.as_deref(), Linear),
            ("normal_map", self.normal_map.as_deref(), Linear),
            ("emissive_map", self.emissive_map.as_deref(), Srgb),
        ]
        .into_iter()
        .filter_map(|(field, asset, space)| asset.map(|asset| (field, asset, space)))
    }
}

/// Written by hand for one reason: a material that names an asset serializes as
/// **only** that reference.
///
/// The fields on such a component are the resolved contents of the file, filled
/// in at load so that everything downstream — the renderer, the editor, a
/// fragment's inherited material — sees a complete material without knowing
/// where it came from. Writing those resolved fields back out beside `asset`
/// would produce a scene that fails its own validation
/// (`material_asset_with_fields`), which is exactly what `engine simulate
/// --bake` would do on a scene with a shared material. So the reference wins,
/// and the file it names stays the single source of truth (invariant 8).
impl Serialize for Material {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct as _;

        if let Some(asset) = &self.asset {
            let mut out = serializer.serialize_struct("Material", 1)?;
            out.serialize_field("asset", asset)?;
            return out.end();
        }

        let maps = [
            ("albedo_map", &self.albedo_map),
            ("orm_map", &self.orm_map),
            ("normal_map", &self.normal_map),
            ("emissive_map", &self.emissive_map),
        ];
        let present = maps.iter().filter(|(_, map)| map.is_some()).count();

        let mut out = serializer.serialize_struct("Material", 13 + present)?;
        out.serialize_field("albedo", &self.albedo)?;
        out.serialize_field("metallic", &self.metallic)?;
        out.serialize_field("roughness", &self.roughness)?;
        out.serialize_field("emissive", &self.emissive)?;
        out.serialize_field("alpha", &self.alpha)?;
        out.serialize_field("transmission", &self.transmission)?;
        for (field, map) in maps {
            if let Some(map) = map {
                out.serialize_field(field, map)?;
            }
        }
        out.serialize_field("uv_scale", &self.uv_scale)?;
        out.serialize_field("uv_offset", &self.uv_offset)?;
        out.serialize_field("alpha_cutoff", &self.alpha_cutoff)?;
        out.serialize_field("normal_strength", &self.normal_strength)?;
        out.serialize_field("ior", &self.ior)?;
        out.serialize_field("thickness", &self.thickness)?;
        out.serialize_field("attenuation", &self.attenuation)?;
        out.end()
    }
}

impl Default for Material {
    fn default() -> Self {
        Self {
            asset: None,
            albedo: Vec3::splat(0.8),
            metallic: 0.0,
            roughness: 0.9,
            emissive: Vec3::ZERO,
            alpha: 1.0,
            transmission: 0.0,
            albedo_map: None,
            orm_map: None,
            normal_map: None,
            emissive_map: None,
            uv_scale: Vec2::ONE,
            uv_offset: Vec2::ZERO,
            alpha_cutoff: 0.0,
            normal_strength: 1.0,
            ior: 1.0,
            thickness: 0.0,
            attenuation: Vec3::ONE,
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

/// Plays an animation clip against scene time (M9), or against ground
/// covered (M32).
///
/// `clip` is a relative path to a property clip (`*.anim.json`), or a
/// `path#ClipName` glTF fragment naming a skeletal clip in the entity's own
/// mesh file (M30). A player in the file is playing — there is no play/pause
/// runtime state.
///
/// The clock is scene time by default, which keeps the pose a pure function
/// of (files, time). Setting `stride` swaps that clock for **distance
/// travelled**, and `phase` is where the clip has got to: still a field in
/// the file, so nothing moves into hidden state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AnimationPlayer {
    pub clip: String,

    /// Time multiplier; local time = `t * speed + start_offset`, or
    /// `phase * speed + start_offset` when `stride` is set.
    #[serde(default = "one")]
    pub speed: f32,

    /// Wrap by clip duration; when false, clamp to the final pose.
    #[serde(default = "yes")]
    pub looping: bool,

    #[serde(default)]
    pub start_offset: f32,

    /// Metres of ground one **cycle** of this clip covers (M32).
    ///
    /// `0` — the default — is the M9 behaviour: scene time drives the clip.
    /// Above zero the clip is driven by the entity's horizontal displacement
    /// instead, advancing `distance / stride` cycles per fixed step, which is
    /// what stops a walk cycle sliding when the character's speed changes.
    /// `engine list-joints` measures the right value off the clip itself.
    #[serde(default)]
    #[schemars(range(min = 0.0))]
    pub stride: f32,

    /// How far this player has got, in **cycles** of its clip, when `stride`
    /// drives it (M32). Ignored otherwise.
    ///
    /// Cycles rather than seconds so the locomotion system needs no clip
    /// duration to advance it: one step covering `d` metres adds `d / stride`,
    /// and nothing has to open the clip file to know that. The engine writes
    /// it back every fixed step and the change-based bake splices it, so where
    /// a character is in its stride survives a bake the same way where it is
    /// standing does. A looping player's phase is reduced into `[0, 1)` as it
    /// is stored.
    #[serde(default)]
    #[schemars(range(min = 0.0))]
    pub phase: f32,
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

/// How a [`HudPanel`] arranges its children (M31).
///
/// NOTE (schemars): variants carry no doc comments on purpose. A doc comment
/// on a *variant* turns the generated schema from a flat `"enum": [...]` into
/// oneOf/const, which blinds the validation walk's closed-vocabulary check —
/// the same trap `ColliderShapeKind` documents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HudLayout {
    #[default]
    Free,
    Row,
    Column,
}

/// Cross-axis alignment of a [`HudPanel`]'s children (M31). See [`HudLayout`]
/// for why the variants are undocumented.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HudAlign {
    #[default]
    Start,
    Center,
    End,
}

/// Horizontal alignment of text within its own box (M31). See [`HudLayout`]
/// for why the variants are undocumented.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HudTextAlign {
    #[default]
    Left,
    Center,
    Right,
}

/// A screen-space text label (M12): lines of the built-in 8×8 pixel font,
/// drawn over the 3D scene after lighting, independent of any camera.
///
/// Needs no `Transform` — placement is `anchor` + `offset` in framebuffer
/// pixels, which is what the agent sees in the PNG. Text is always opaque
/// and never anti-aliased, so a HUD glyph is bit-exact in baselines. Glyphs
/// outside the font's coverage render as a filled box: visibly wrong in the
/// screenshot, never a panic.
///
/// M31 adds `parent`, `visible` and `stretch` (shared by every element in the
/// family), plus `align`, `wrap` and `line_gap`. Every one defaults to the M12
/// behaviour: no parent means a child of the viewport placed by exactly the
/// M12 anchor arithmetic, and `wrap: 0` means the single unwrapped line it has
/// always been.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HudText {
    /// The label. `\n` breaks a line explicitly; `wrap` breaks it
    /// automatically. Scripts may rewrite it via `world.set_hud_text` — an
    /// empty string is a legal rest value for a script-driven readout.
    pub text: String,

    #[serde(default)]
    pub anchor: HudAnchor,

    /// Pixels inward from `anchor` (see [`HudAnchor`]). Inside a `row` or
    /// `column` parent this is a nudge on top of the computed position rather
    /// than the position itself.
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

    /// The name of an entity carrying a [`HudPanel`] to place this inside.
    /// Absent (the default) means a child of the viewport.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,

    /// Drawn and hit-testable when true (the default). Hiding a panel hides
    /// its whole subtree — one boolean is how a menu opens and closes.
    #[serde(default = "yes")]
    pub visible: bool,

    /// Fill the parent's content box on `[x, y]`, ignoring this element's own
    /// size on that axis. Two booleans rather than a `"fill"` string in a
    /// numeric field, which would break the schema-driven walk.
    #[serde(default)]
    pub stretch: [bool; 2],

    /// Alignment of each line within the text's own box, which differs from
    /// the box only when the text is stretched or wrapped.
    #[serde(default)]
    pub align: HudTextAlign,

    /// Wrap width in pixels; `0` (the default) is no wrapping. Breaks on
    /// spaces — a word longer than `wrap` overflows rather than splitting,
    /// since a mid-word break in a fixed-width font reads as corruption.
    #[serde(default)]
    #[schemars(range(min = 0.0))]
    pub wrap: f32,

    /// Extra pixels between lines, on top of the glyph cell.
    #[serde(default)]
    #[schemars(range(min = 0.0))]
    pub line_gap: f32,
}

fn hud_text_size() -> f32 {
    16.0
}

fn white() -> Vec3 {
    Vec3::ONE
}

/// A screen-space solid rectangle (M12): the primitive behind health bars,
/// speed bars, and backdrops. Drawn with the panels and images, before all
/// `HudText`, file order within the class.
///
/// M31 adds only the shared `parent`/`visible`/`stretch`; a rect stays the
/// flat script-driven bar it has always been.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HudRect {
    #[serde(default)]
    pub anchor: HudAnchor,

    /// Pixels inward from `anchor` (see [`HudAnchor`]). Inside a `row` or
    /// `column` parent this is a nudge on top of the computed position.
    #[serde(default)]
    #[schemars(with = "[f32; 2]")]
    pub offset: Vec2,

    /// `[width, height]` in pixels, each `>= 0` — zero is legal so a
    /// script-driven bar can be empty. Scripts resize via
    /// `world.set_hud_rect_size` or `world.set_hud_size`. Ignored on an axis
    /// where `stretch` is true.
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

    /// The name of an entity carrying a [`HudPanel`] to place this inside.
    /// Absent (the default) means a child of the viewport.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,

    /// Drawn and hit-testable when true (the default).
    #[serde(default = "yes")]
    pub visible: bool,

    /// Fill the parent's content box on `[x, y]`, ignoring `size` on that
    /// axis — the full-screen dim backdrop, and the bar spanning a column.
    #[serde(default)]
    pub stretch: [bool; 2],
}

/// A screen-space container that lays its children out (M31).
///
/// This is the component that removes hand-computed pixel offsets. Children
/// name it in their `parent`; `layout` decides whether they are stacked in a
/// `row`, a `column`, or placed `free` by their own anchors relative to this
/// panel's content box.
///
/// **Absent `width`/`height` means hug contents** — the panel is exactly its
/// children's extent plus `padding`. That is the default because it is the
/// case that makes a dialog authorable: the box follows the text instead of
/// the text being fitted to a box someone solved by hand.
///
/// `opacity` defaults to **0**, so a bare `HudPanel` is an invisible layout
/// group; set it and the same component is the dialog's backdrop. One
/// component rather than a container plus a rect whose size would have to be
/// kept in agreement with it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HudPanel {
    #[serde(default)]
    pub anchor: HudAnchor,

    /// Pixels inward from `anchor` (see [`HudAnchor`]).
    #[serde(default)]
    #[schemars(with = "[f32; 2]")]
    pub offset: Vec2,

    #[serde(default)]
    pub layout: HudLayout,

    /// Uniform inset in pixels between this panel's edge and its content box.
    /// Per-side padding is the obvious next field and costs nothing to add
    /// later; M12's "no z field until something needs it" applies.
    #[serde(default)]
    #[schemars(range(min = 0.0))]
    pub padding: f32,

    /// Pixels between children along the main axis of a `row`/`column`.
    #[serde(default)]
    #[schemars(range(min = 0.0))]
    pub gap: f32,

    /// Cross-axis alignment of children in a `row`/`column`.
    #[serde(default)]
    pub align: HudAlign,

    /// Fixed width in pixels. Absent hugs the children.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0.0))]
    pub width: Option<f32>,

    /// Fixed height in pixels. Absent hugs the children.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0.0))]
    pub height: Option<f32>,

    /// Background colour, linear RGB in `[0, 1]`. Only visible at
    /// `opacity > 0`.
    #[serde(default = "white")]
    #[schemars(with = "[f32; 3]", inner(range(min = 0.0, max = 1.0)))]
    pub color: Vec3,

    /// `[0, 1]`, defaulting to **0** — an invisible layout group.
    #[serde(default)]
    #[schemars(range(min = 0.0, max = 1.0))]
    pub opacity: f32,

    /// The name of another [`HudPanel`] entity to nest inside.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,

    /// Drawn and hit-testable when true (the default). A hidden panel hides
    /// its whole subtree.
    #[serde(default = "yes")]
    pub visible: bool,

    /// Fill the parent's content box on `[x, y]`, ignoring `width`/`height`
    /// and hug sizing on that axis.
    #[serde(default)]
    pub stretch: [bool; 2],
}

/// A screen-space textured rectangle (M31): icons, logos, framed panels.
///
/// `texture` is a PNG relative to the scene file, loaded through the same
/// `TextureSource` and `(asset, space)` cache the material system uses, in
/// sRGB — so `texture_too_large` fires from `validate`, before a device
/// exists. Only the base level is read: the overlay draws at most one
/// destination pixel per texel band and never minifies below it, so a mip
/// level selection would have no correct answer at this scale.
///
/// Sampling is **nearest-neighbour**, written out in `engine-core` like every
/// other generator here — a render sits under a baseline, so the filter is a
/// format contract, and nearest is exactly reproducible where a bilinear
/// filter is a float-rounding question.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HudImage {
    /// A `.png` path relative to the scene file (invariant 3).
    pub texture: String,

    #[serde(default)]
    pub anchor: HudAnchor,

    /// Pixels inward from `anchor` (see [`HudAnchor`]).
    #[serde(default)]
    #[schemars(with = "[f32; 2]")]
    pub offset: Vec2,

    /// `[width, height]` in destination pixels. Ignored on an axis where
    /// `stretch` is true.
    #[schemars(with = "[f32; 2]", inner(range(min = 0.0)))]
    pub size: Vec2,

    /// Nine-slice insets in **source** pixels, `[left, top, right, bottom]`.
    /// The default `[0, 0, 0, 0]` is a plain stretch. Corners are copied
    /// 1:1, edges tile along their axis and the centre tiles both ways —
    /// tiling rather than stretching, because tiling at nearest is exact
    /// where stretching at nearest is a moiré pattern.
    #[serde(default)]
    #[schemars(inner(range(min = 0.0)))]
    pub slice: [f32; 4],

    /// Multiplies the decoded texel in linear space, so one grey frame
    /// texture serves a red panel and a blue one — the material system's
    /// authoring rule for `albedo_map`, here for the same reason.
    #[serde(default = "white")]
    #[schemars(with = "[f32; 3]", inner(range(min = 0.0, max = 1.0)))]
    pub tint: Vec3,

    /// `[0, 1]`, multiplied onto the texture's own alpha.
    #[serde(default = "one")]
    #[schemars(range(min = 0.0, max = 1.0))]
    pub opacity: f32,

    /// The name of an entity carrying a [`HudPanel`] to place this inside.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,

    /// Drawn and hit-testable when true (the default).
    #[serde(default = "yes")]
    pub visible: bool,

    /// Fill the parent's content box on `[x, y]`, ignoring `size`.
    #[serde(default)]
    pub stretch: [bool; 2],
}

/// Makes the HUD element on its own entity clickable (M31).
///
/// Carries no geometry: the hit box is that element's laid-out rectangle. An
/// entity with a `HudInteract` and no `HudPanel`/`HudRect`/`HudImage`/
/// `HudText` is `hud_interact_without_element`.
///
/// A separate component rather than an `interactive: true` flag on each of
/// four components, because the flag would be four fields that must stay in
/// agreement, and because the tints belong next to it.
///
/// The tints multiply the element's own colour (clamped to `[0, 1]` after
/// multiplying) and default to `[1, 1, 1]` — no change — so adding a
/// `HudInteract` moves no pixel until a cursor arrives. They exist so the
/// ordinary case, a button that lights up under the pointer, needs no script
/// at all; anything richer is a script writing colours.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HudInteract {
    /// Colour multiplier while the cursor is over this element. Unbounded
    /// above — a hover tint brightens.
    #[serde(default = "white")]
    #[schemars(with = "[f32; 3]", inner(range(min = 0.0)))]
    pub hover_tint: Vec3,

    /// Colour multiplier while a button is held down on this element.
    #[serde(default = "white")]
    #[schemars(with = "[f32; 3]", inner(range(min = 0.0)))]
    pub press_tint: Vec3,

    /// Excluded from hit-testing when true, so it never hovers, presses or
    /// clicks — and never blocks what is under it either.
    #[serde(default)]
    pub disabled: bool,
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
/// else.
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
/// scene file.
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
            roughness: self.leaf_roughness,
            ..Material::default()
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
/// a pond is not displaced by the ripples above it.
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

    /// Index of refraction, `1.0` (the default) being no bending at all — the
    /// pre-M27 surface, which absorbs and tints what is behind it but never
    /// moves it. Range `[1, 3]`; water is 1.33.
    ///
    /// Unlike [`Material::ior`] this needs no companion `thickness`: the shader
    /// already measures how far the view ray travels through the body to reach
    /// the bed, so the bend scales with the water's own depth and a pond bends
    /// its bed most where it is deepest. It also cannot change how *deep* the
    /// water looks — absorption stays [`Water::depth_fade`]'s job — so it can
    /// be turned on in a tuned scene without re-tuning it.
    #[schemars(range(min = 1.0, max = 3.0))]
    pub ior: f32,

    /// Density of the fluid in kg/m³, `> 0`. Fresh water is 1000 (the default),
    /// sea water about 1025.
    ///
    /// The **only** field here that nothing renders. It is what a [`Buoyancy`]
    /// body weighs the water it displaces against, and it lives on the lake
    /// rather than on the boat because it is a property of the fluid: two hulls
    /// in one pond disagreeing about how dense the water is would not be a knob,
    /// it would be a bug. The authoring knob for "this floats higher" already
    /// exists and is [`Collider::density`], in the same unit.
    #[schemars(extend("exclusiveMinimum" = 0.0))]
    pub density: f32,
}

/// Makes a dynamic body float on a named [`Water`] surface (M41).
///
/// Archimedes, sampled: the body's collider is divided into columns, each column
/// is asked how deep it sits under the wave above it, and each pushes up with
/// the weight of the water it displaces. Because the pushes land at their own
/// columns rather than at the centre of mass, a hull that rolls has more of
/// itself submerged on the low side and rights itself — the pitch and roll come
/// out of the same sum as the lift, with nothing modelling them separately.
///
/// **Absent, nothing floats**, which is the pre-M41 engine exactly. The
/// component needs a `RigidBody` that is dynamic and a `Collider` to have a
/// shape at all, and validation says so rather than letting a scene author a
/// component that silently does nothing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Buoyancy {
    /// The [`Water`] entity this body floats on, by name. Required.
    ///
    /// Named rather than found by overlap, for [`Meadow::terrain`]'s reason: one
    /// implementation of "where is the surface", pointed at explicitly. A scene
    /// with two ponds has to say which one, and a scene with one still says it,
    /// so the file records what the physics did.
    pub water: String,

    /// Columns per axis across the body's footprint, `[1, 4]`. Default 2, so a
    /// hull is sampled at four points.
    ///
    /// The fidelity knob, and it decides whether the body can **turn**: at 1
    /// there is a single upward push through the middle and a raft cannot right
    /// itself or ride a slope, because a force through the centre of mass makes
    /// no torque. At 2 each quarter of the hull feels its own wave, which is
    /// what makes a boat pitch into a swell instead of hovering over it. Past
    /// that the returns fall off quickly — 3 and 4 are for a long hull spanning
    /// several wavelengths.
    #[schemars(range(min = 1, max = 4))]
    pub samples: u32,

    /// Linear damping in 1/s applied **in proportion to how submerged the body
    /// is**, `>= 0`.
    ///
    /// Added on top of [`RigidBody::linear_damping`], not replacing it: that
    /// field is the body's drag in air, and this is the water's. Water drag is
    /// not a property of the boat, which is exactly why it cannot be authored on
    /// the `RigidBody` — a hull thrown clear of the pond has to stop being
    /// damped the moment it leaves, and a half-submerged one is dragged half as
    /// hard.
    #[schemars(range(min = 0.0))]
    pub drag: f32,

    /// Angular damping in 1/s, scaled by submersion exactly as [`drag`] is.
    /// `>= 0`.
    ///
    /// Usually wants to be the larger of the two: water stops a hull from
    /// spinning far more effectively than it stops it from drifting, and a boat
    /// that rolls for twenty seconds after a wave reads as weightless.
    ///
    /// [`drag`]: Buoyancy::drag
    #[schemars(range(min = 0.0))]
    pub angular_drag: f32,
}

impl Default for Buoyancy {
    fn default() -> Self {
        Self {
            // No sensible default: which water a boat floats on is not
            // guessable, and validation requires it. Empty is what an author
            // omitting it gets, and what the error message is about.
            water: String::new(),
            samples: 2,
            drag: 1.0,
            angular_drag: 2.0,
        }
    }
}

impl Water {
    /// Whether this surface bends what is behind it, and so needs the opaque
    /// colour copy and the refracting pipeline variant (M27).
    ///
    /// The default `1.0` is exactly "no", which is what lets every water
    /// baseline blessed before this milestone keep compiling the M18 shader.
    pub fn refracts(&self) -> bool {
        self.ior != 1.0
    }
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
            ior: 1.0,
            // Fresh water. Nothing reads this unless something floats, so it
            // costs a pre-M41 scene nothing to have gained it.
            density: 1000.0,
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

/// One authored point on a road's centerline.
///
/// The road is a **polygon with corner radii**, not a spline: a closed polygon
/// returns to its own first vertex and its exterior angles sum to exactly one
/// turn, so position and heading close without solving anything. Nothing here
/// carries a heading, deliberately — a heading is derived, and a stored one can
/// be edited into disagreeing with the points on either side of it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct RoadPoint {
    /// Where the centerline passes, in the entity's local space. `y` is the
    /// road *surface* height there; the profile between points is a monotone
    /// cubic through these, so the grade turns over smoothly and never
    /// overshoots an authored height.
    #[schemars(with = "[f32; 3]")]
    pub position: Vec3,

    /// Radius of the arc rounding this corner, in metres. `>= 0`.
    ///
    /// `0` is a sharp vertex — mitred, which is exactly right for a point that
    /// is not really a turn (a start line partway along a straight) and wrong
    /// for one that is: past
    /// [`MAX_SHARP_TURN_DEGREES`](crate::road::MAX_SHARP_TURN_DEGREES) of turn
    /// the mitre folds back through the road and validation refuses it.
    ///
    /// The two arcs meeting on one edge have to fit on it, which is the other
    /// thing a polygon cannot guarantee (`road_corner_does_not_fit`).
    #[schemars(range(min = 0.0))]
    pub radius: f32,
}

impl Default for RoadPoint {
    fn default() -> Self {
        Self {
            position: Vec3::ZERO,
            radius: 0.0,
        }
    }
}

/// What is painted on a road, and where.
///
/// Every marking is computed per pixel from the road's surface coordinates —
/// `u`, metres from the centerline across the road, and `v`, metres along it —
/// rather than built as geometry laid on the asphalt. That is what makes a line
/// follow every curve and grade for free, keeps a dash the same length in
/// metres through a hairpin as on a straight, and means paint can never
/// z-fight: it is not a surface on a surface, it is the same pixel shaded
/// differently.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct RoadMarkings {
    /// Linear RGB of the paint — the edge lines, the centre line, the start
    /// line, and the white half of a kerb. Each component `[0, 1]`.
    #[schemars(with = "[f32; 3]", inner(range(min = 0.0, max = 1.0)))]
    pub color: Vec3,

    /// Width of the line down each edge of the asphalt, in metres. `0` is no
    /// edge line. `>= 0`.
    #[schemars(range(min = 0.0))]
    pub edge_width: f32,

    /// Gap between the asphalt's edge and the outside of the edge line, in
    /// metres. `>= 0`.
    #[schemars(range(min = 0.0))]
    pub edge_inset: f32,

    /// Width of the centre line, in metres. `0` (the default) is no centre
    /// line, which is what a race circuit wants. `>= 0`.
    #[schemars(range(min = 0.0))]
    pub center_width: f32,

    /// Length of one painted dash in the centre line, in metres. `0` makes it
    /// solid. `>= 0`.
    ///
    /// On a **closed** road the dash period is snapped to a whole number of
    /// repeats around the lap, so the pattern meets itself exactly at the seam
    /// instead of leaving a short dash there. The ratio of dash to gap is what
    /// is preserved, not their absolute lengths.
    #[schemars(range(min = 0.0))]
    pub center_dash: f32,

    /// Unpainted gap between dashes, in metres. `>= 0`.
    #[schemars(range(min = 0.0))]
    pub center_gap: f32,

    /// Corners with a radius at or under this get a kerb on the **inside** of
    /// the turn, in metres. `0` (the default) is no kerbs. `>= 0`.
    ///
    /// Which corners are tight enough, and which side of the road is the inside
    /// of the turn, are facts about the plan-view geometry that per-pixel code
    /// cannot know, so they are computed once and handed to the shader as spans
    /// — at most [`MAX_ROAD_KERBS`](crate::road::MAX_ROAD_KERBS) of them.
    #[schemars(range(min = 0.0))]
    pub kerb_max_radius: f32,

    /// How far a kerb reaches out from the asphalt edge, in metres. `>= 0`.
    #[schemars(range(min = 0.0))]
    pub kerb_width: f32,

    /// Length of one red or white kerb stripe, in metres. `> 0`.
    ///
    /// Fitted per corner to a whole number of stripes, so a kerb begins and
    /// ends on a stripe boundary rather than on a sliver.
    #[schemars(extend("exclusiveMinimum" = 0.0))]
    pub kerb_stripe: f32,

    /// Linear RGB of the red half of a kerb; the other half is `color`. Each
    /// component `[0, 1]`.
    #[schemars(with = "[f32; 3]", inner(range(min = 0.0, max = 1.0)))]
    pub kerb_color: Vec3,

    /// Paint a line across the road.
    ///
    /// Painted rather than built, which matters more here than anywhere else: a
    /// start line with any real height is a wall the car is parked against at
    /// step 0.
    pub start_line: bool,

    /// Where that line goes: metres along the centerline from the road's first
    /// point. `>= 0`, and past the end of a closed road it wraps.
    ///
    /// A position rather than "wherever the polygon starts", because the two
    /// are different jobs: the polygon's first point is a *corner*, chosen by
    /// the shape of the circuit, and a start line belongs partway down a
    /// straight. Splitting a straight with an extra point to move the line
    /// would be geometry surgery in service of paint — and on a short straight
    /// it fails outright, because the neighbouring corner's arc already covers
    /// the place the line wanted to be. `engine road-centerline` is how a
    /// generator turns "here, in world space" into this number.
    #[schemars(range(min = 0.0))]
    pub start_line_at: f32,

    /// Width of that line along the road, in metres. `>= 0`.
    #[schemars(range(min = 0.0))]
    pub start_line_width: f32,
}

impl Default for RoadMarkings {
    fn default() -> Self {
        Self {
            color: Vec3::new(0.88, 0.88, 0.88),
            edge_width: 0.14,
            edge_inset: 0.10,
            // Absent means off, as everywhere else: a bare road gets edge lines
            // and nothing else, and every marking past that is asked for.
            center_width: 0.0,
            center_dash: 3.0,
            center_gap: 6.0,
            kerb_max_radius: 0.0,
            kerb_width: 0.9,
            kerb_stripe: 1.4,
            kerb_color: Vec3::new(0.80, 0.10, 0.08),
            start_line: false,
            start_line_at: 0.0,
            start_line_width: 0.7,
        }
    }
}

/// A road: a circuit, a street, a mountain pass.
///
/// The entity owns its surface geometry — one continuous ribbon generated from
/// the centerline — so a `Road` entity carries **no** `Mesh` and no `Material`
/// (`road_with_mesh`), the same rule [`Water`] follows and for the same reason.
///
/// Asphalt, shoulders and the embankment skirt are all the **same** triangles.
/// That is not a saving, it is the point: road and shoulder as two surfaces at
/// slightly different heights build a ledge along the asphalt edge, and a wheel
/// that drops off it wedges against the step and stops the car dead. There is
/// no seam between segments either, because consecutive cross-sections share
/// their vertices.
///
/// Physics reads the same mesh: a `Collider` with `"shape": "trimesh"` on a
/// road entity needs no `asset` and no `Mesh`, because the road is the
/// geometry. Friction and collision layers stay on the `Collider`, where every
/// other surface in the engine keeps them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Road {
    /// The centerline, corner by corner, in the order they are driven. At
    /// least two points; a closed road needs at least three.
    #[schemars(length(max = 256))]
    pub points: Vec<RoadPoint>,

    /// Join the last point back to the first. A closed road is a circuit: the
    /// polygon's exterior angles sum to one turn, so it shuts without a solver.
    pub closed: bool,

    /// Width of the asphalt, edge to edge, in metres. `> 0`.
    #[schemars(extend("exclusiveMinimum" = 0.0))]
    pub width: f32,

    /// Drivable shoulder each side of the asphalt, in metres. `>= 0`.
    ///
    /// Part of the same surface, not a second one — see the note above about
    /// ledges.
    #[schemars(range(min = 0.0))]
    pub shoulder: f32,

    /// How far the embankment drops below the road's outer edge, in metres.
    /// `>= 0`.
    ///
    /// This is what stops an elevated road from floating. Set it deeper than
    /// the road ever climbs and it simply disappears under the ground plane
    /// wherever the road is low.
    #[schemars(range(min = 0.0))]
    pub skirt: f32,

    /// Longest a straight segment may be before the road is cut again, in
    /// metres. `>= 0.25`.
    #[schemars(range(min = 0.25))]
    pub segment_length: f32,

    /// Most degrees of arc one segment may cover through a corner. `>= 0.5`.
    ///
    /// This is the resolution knob that matters: a corner cut every 5° is
    /// smooth to drive and to look at, and the cost is linear in the road's
    /// length rather than quadratic like a grid's.
    #[schemars(range(min = 0.5))]
    pub segment_angle: f32,

    /// Linear RGB of the asphalt. Each component `[0, 1]`.
    #[schemars(with = "[f32; 3]", inner(range(min = 0.0, max = 1.0)))]
    pub color: Vec3,

    /// Surface roughness, `[0, 1]`, meaning what `Material.roughness` means.
    /// Asphalt is nearly matte; wet asphalt is not.
    #[schemars(range(min = 0.0, max = 1.0))]
    pub roughness: f32,

    /// Linear RGB of the shoulder each side of the asphalt. Each component
    /// `[0, 1]`.
    #[schemars(with = "[f32; 3]", inner(range(min = 0.0, max = 1.0)))]
    pub shoulder_color: Vec3,

    /// Linear RGB of the embankment below the shoulder. Each component
    /// `[0, 1]`.
    #[schemars(with = "[f32; 3]", inner(range(min = 0.0, max = 1.0)))]
    pub bank_color: Vec3,

    /// What is painted on it.
    pub markings: RoadMarkings,
}

impl Default for Road {
    fn default() -> Self {
        Self {
            // A 20 m straight running down the engine's forward axis, so a bare
            // `{"type": "Road"}` is a road — the same courtesy `{"type":
            // "Water"}` does, and the difference between adding the component
            // in the editor and seeing a road, or adding it and seeing
            // `road_too_few_points`.
            points: vec![
                RoadPoint {
                    position: Vec3::ZERO,
                    radius: 0.0,
                },
                RoadPoint {
                    position: Vec3::new(0.0, 0.0, -20.0),
                    radius: 0.0,
                },
            ],
            closed: false,
            width: 7.0,
            shoulder: 1.5,
            skirt: 0.6,
            segment_length: 2.0,
            segment_angle: 5.0,
            color: Vec3::new(0.09, 0.09, 0.10),
            roughness: 0.92,
            shoulder_color: Vec3::new(0.17, 0.20, 0.14),
            bank_color: Vec3::new(0.20, 0.17, 0.13),
            markings: RoadMarkings::default(),
        }
    }
}

/// One keyframe of a [`Meadow`]'s life cycle.
///
/// All seven fields are required. A half-specified keyframe is an error rather
/// than a fade to black — M21's palette wrote that rule down, and a meadow's
/// table is read the same way: linearly interpolated, and **wrapping** from the
/// last keyframe back round to the first, so the cycle closes without anyone
/// having to author phase 1.0 as a copy of phase 0.0.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GrowthStage {
    /// Where in the cycle this keyframe sits, `[0, 1)`. Strictly increasing
    /// down the table (`meadow_stages_invalid`).
    #[schemars(range(min = 0.0, max = 0.999))]
    pub at: f32,

    /// Plant height as a fraction of [`Meadow::height`], `>= 0`. `0` is a plant
    /// that is not there, which is what the seed keyframe says.
    #[schemars(range(min = 0.0))]
    pub height: f32,

    /// Blade width as a fraction of [`Meadow::blade_width`], `>= 0`.
    #[schemars(range(min = 0.0))]
    pub width: f32,

    /// Degrees the plant leans from vertical at its tip. The bend is a
    /// cantilever — the tip leans this far and the root not at all — so a large
    /// value at the end of the cycle is the plant collapsing rather than
    /// tipping over rigidly. `[0, 90]`.
    #[schemars(range(min = 0.0, max = 90.0))]
    pub lean: f32,

    /// How much of [`Meadow::wind`] reaches the plant at this stage, `>= 0`.
    ///
    /// This is what separates green grass that flows from dry stalks that
    /// stand, and it is nearly free — the wind term is already in the vertex
    /// stage.
    #[schemars(range(min = 0.0))]
    pub sway: f32,

    /// Linear RGB at the plant's base, each component `[0, 1]`.
    #[schemars(with = "[f32; 3]", inner(range(min = 0.0, max = 1.0)))]
    pub color: Vec3,

    /// Linear RGB at the plant's tip, each component `[0, 1]`.
    ///
    /// Two colours rather than one because senescence runs tip-downward in real
    /// grass: a stand going over turns straw at the top while its base is still
    /// green, and a single flat colour per stage looks painted on.
    #[schemars(with = "[f32; 3]", inner(range(min = 0.0, max = 1.0)))]
    pub tip_color: Vec3,
}

/// Ground cover that grows, seeds and dies on a loop: grass, weeds, wildflowers
/// and the dry stand they turn into.
///
/// A recipe rather than a mesh reference, like [`Tree`] and [`Cloud`] — the
/// engine grows one plant and scatters copies of it over the footprint
/// `Transform.scale` gives, so a `Meadow` entity carries **no** `Mesh` and
/// **no** `Material` (`meadow_with_mesh`).
///
/// **It is the first recipe in this engine whose subject changes shape over
/// time.** A sprout is not a small blade of grass. The resolution is that the
/// geometry is static and the *life cycle lives in the vertex stage*: the plant
/// is built once carrying every organ any stage will need, and each organ scales
/// to nothing outside the phase window it belongs to. See
/// `designs/meadow-design.md`.
///
/// The cycle runs on the scene clock, so it can be sped up:
/// [`cycle_length`](Meadow::cycle_length) `: 3.0` runs a whole generation in
/// three seconds. **`0` — the default — freezes the field** at
/// [`phase`](Meadow::phase), the way `daylight.day_length: 0` freezes the day:
/// most scenes want a dial, not motion, and a frozen field is reproducible with
/// no `--time` at all.
///
/// Every generation **reseeds** rather than regrowing: a plant's position within
/// its own cell, its height and its lean all shift a little each time round, so
/// the dead stalk and the sprout that replaces it are not collinear. That costs
/// one integer hash in the shader and no state anywhere.
///
/// A meadow is scenery: no `Collider`, no shadow cast (a 2048² map cannot
/// resolve a blade of grass, and what it would record is noise that crawls),
/// and no `PointLight` contribution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Meadow {
    /// Seeds placement and the template's blades. Two meadows with the same
    /// parameters and different seeds are different fields.
    ///
    /// The generator's xorshift and the shader's reseed hash are both written
    /// out in this repo, so a given seed means the same field across dependency
    /// upgrades — a meadow render sits under a `diff-render` baseline, which
    /// makes both a format contract.
    pub seed: u32,

    /// Plants per square metre of footprint, `>= 0`.
    ///
    /// The footprint is `Transform.scale` in XZ, so the plant count is
    /// `density × area`, rounded up to a square grid. `0` is an empty field,
    /// which is a legitimate thing to animate toward.
    #[schemars(range(min = 0.0))]
    pub density: f32,

    /// Height of a fully grown plant in metres, `> 0`. `Transform.scale.y`
    /// multiplies it, the way it multiplies a [`Terrain`]'s relief.
    #[schemars(extend("exclusiveMinimum" = 0.0))]
    pub height: f32,

    /// Blades in one plant. `[1, 12]`; `3`–`5` reads as a tuft.
    #[schemars(range(min = 1, max = 12))]
    pub blades: u32,

    /// Lengthwise segments per blade — how finely a blade can curve as it leans
    /// and bends in the wind. `[1, 8]`.
    ///
    /// Together with [`blades`](Meadow::blades) this sets the per-plant
    /// triangle count, and the product with the plant count is what
    /// `meadow_too_complex` bounds.
    #[schemars(range(min = 1, max = 8))]
    pub segments: u32,

    /// Width of a blade at its base, in metres, `> 0`.
    #[schemars(extend("exclusiveMinimum" = 0.0))]
    pub blade_width: f32,

    /// Degrees the outermost blades splay from vertical, `[0, 90]`. Blade 0
    /// stays near upright, which is what gives a tuft a centre rather than a
    /// hole.
    #[schemars(range(min = 0.0, max = 90.0))]
    pub splay: f32,

    /// Size of the flower and seed heads, in metres, `>= 0`. `0` grows a plant
    /// that never flowers.
    #[schemars(range(min = 0.0))]
    pub head_size: f32,

    /// How much plant height varies between plants, as a fraction, `[0, 1)`.
    /// `0` is a lawn.
    #[schemars(range(min = 0.0, max = 0.99))]
    pub size_jitter: f32,

    /// Seconds one full life cycle takes, `>= 0`.
    ///
    /// **`0` freezes the field** at [`phase`](Meadow::phase) — the default, and
    /// `daylight.day_length: 0`'s reasoning exactly.
    #[schemars(range(min = 0.0))]
    pub cycle_length: f32,

    /// Where the cycle starts, `[0, 1)` — and where a frozen field sits. The
    /// default is mature green, so `{"type": "Meadow"}` alone puts a working
    /// field of grass in a scene.
    #[schemars(range(min = 0.0, max = 0.999))]
    pub phase: f32,

    /// How far plants desync from each other, `[0, 1]`.
    ///
    /// `0` marches the whole field in lockstep; `1` spreads offsets across the
    /// whole cycle, so every stage is present at every moment and the field
    /// never appears to change. A real meadow browns together with variation,
    /// which is why the default is near the low end.
    #[schemars(range(min = 0.0, max = 1.0))]
    pub stagger: f32,

    /// How far the wind bends a plant at full sway, in degrees, `>= 0`. `0` is
    /// still air.
    #[schemars(range(min = 0.0, max = 90.0))]
    pub wind: f32,

    /// How fast gusts travel across the field, in metres per second, `>= 0`.
    ///
    /// Gusts are a travelling wave, not a per-plant shimmer: sampling the noise
    /// against a moving coordinate is what makes wind cross a meadow visibly.
    #[schemars(range(min = 0.0))]
    pub wind_speed: f32,

    /// Which way the wind blows, in degrees — `0` toward −Z, the engine's
    /// forward convention, shared with `Water`'s wave directions.
    pub wind_direction: f32,

    /// Steepest ground grass will grow on, in degrees, `[0, 90]`. Plants on
    /// steeper ground are dropped; `90` keeps everything.
    #[schemars(range(min = 0.0, max = 90.0))]
    pub max_slope: f32,

    /// The [`Terrain`] entity this meadow stands on, by name.
    ///
    /// Each plant's altitude is sampled from that patch through the same
    /// function `world.terrain_height` and `engine terrain-height` call, so
    /// there is one implementation of "where is the ground" and nothing to keep
    /// in agreement. Absent, the field is flat at the entity's own Y.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terrain: Option<String>,

    /// Linear RGB of the flower head, each component `[0, 1]`. What stops the
    /// weed stage from being nothing but taller grass.
    #[schemars(with = "[f32; 3]", inner(range(min = 0.0, max = 1.0)))]
    pub flower_color: Vec3,

    /// The life cycle, as keyframes over `phase`. At least two, at most
    /// [`MAX_GROWTH_STAGES`](crate::meadow::MAX_GROWTH_STAGES), strictly
    /// increasing in `at`.
    ///
    /// The default is the full seed → sprout → grass → weeds → dry → collapse
    /// cycle, so a meadow is worth looking at before anything is authored.
    pub stages: Vec<GrowthStage>,
}

impl Default for Meadow {
    fn default() -> Self {
        Self {
            seed: 0,
            density: 24.0,
            height: 0.45,
            blades: 5,
            segments: 4,
            blade_width: 0.007,
            splay: 54.0,
            head_size: 0.018,
            size_jitter: 0.35,
            cycle_length: 0.0,
            phase: 0.42,
            stagger: 0.25,
            wind: 9.0,
            wind_speed: 3.5,
            wind_direction: 0.0,
            max_slope: 38.0,
            terrain: None,
            flower_color: Vec3::new(0.40, 0.37, 0.17),
            stages: Meadow::default_stages(),
        }
    }
}

impl Meadow {
    /// The default life cycle: six keyframes from bare ground back to bare
    /// ground, in linear RGB.
    ///
    /// The table wraps, so there is no phase-1.0 entry — the collapse keyframe
    /// interpolates round to the seed keyframe on its own.
    pub fn default_stages() -> Vec<GrowthStage> {
        vec![
            // Seed: nothing above ground.
            GrowthStage {
                at: 0.0,
                height: 0.0,
                width: 0.5,
                lean: 0.0,
                sway: 0.0,
                color: Vec3::new(0.13, 0.10, 0.05),
                tip_color: Vec3::new(0.16, 0.13, 0.07),
            },
            // Sprout: short, soft, and the bright yellow-green of new growth.
            GrowthStage {
                at: 0.09,
                height: 0.16,
                width: 0.7,
                lean: 10.0,
                sway: 0.5,
                color: Vec3::new(0.16, 0.34, 0.07),
                tip_color: Vec3::new(0.31, 0.55, 0.13),
            },
            // Grass: full height, saturated, moving.
            GrowthStage {
                at: 0.31,
                height: 0.86,
                width: 1.0,
                lean: 17.0,
                sway: 1.0,
                color: Vec3::new(0.06, 0.19, 0.04),
                tip_color: Vec3::new(0.17, 0.40, 0.09),
            },
            // Weeds: the tallest and coarsest it gets, flower heads open,
            // colour drifting olive.
            GrowthStage {
                at: 0.53,
                height: 1.0,
                width: 1.05,
                lean: 21.0,
                sway: 0.9,
                color: Vec3::new(0.09, 0.20, 0.05),
                tip_color: Vec3::new(0.30, 0.36, 0.10),
            },
            // Dry: standing straw, stiff, seed heads out.
            GrowthStage {
                at: 0.76,
                height: 0.95,
                width: 0.92,
                lean: 27.0,
                sway: 0.4,
                color: Vec3::new(0.22, 0.18, 0.07),
                tip_color: Vec3::new(0.62, 0.51, 0.19),
            },
            // Collapse: gone over, grey-brown, on its way back into the ground.
            GrowthStage {
                at: 0.91,
                height: 0.62,
                width: 0.8,
                lean: 71.0,
                sway: 0.25,
                color: Vec3::new(0.14, 0.11, 0.06),
                tip_color: Vec3::new(0.26, 0.21, 0.11),
            },
        ]
    }
}

/// The most feet one solver run plants, and the ceiling on `FootPlant.feet`.
///
/// Bounded for the reason `MAX_POINT_LIGHTS` and `MAX_ROAD_KERBS` are: a fixed
/// small number is a budget an agent can be told about, and the alternative to
/// refusing the fifth foot is a rig that plants four and silently ignores the
/// rest. Four covers a quadruped, which is the most legs anything in this
/// engine has.
pub const MAX_PLANTED_FEET: usize = 4;

/// One foot of a [`FootPlant`]: which joint it is, and how far up the chain
/// the solver may rotate to reach the ground.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlantedFoot {
    /// The ankle joint — the end of the chain, the thing put on the ground.
    pub ankle: String,

    /// How many joints **above** the ankle rotate to reach the target. `2` is
    /// the ordinary leg (knee and hip) and is what the two-bone solve means;
    /// `1` bends a single hinge and is what a stubby prop leg wants.
    #[serde(default = "two")]
    #[schemars(range(min = 1, max = 2))]
    pub chain: u32,

    /// Metres from the ankle joint down to the bottom of the foot, so the sole
    /// meets the ground rather than the joint being buried in it.
    #[serde(default)]
    #[schemars(range(min = 0.0))]
    pub sole: f32,
}

fn two() -> u32 {
    2
}

/// Puts a skinned character's feet on the terrain under them (M32).
///
/// A post-pass over the posed skeleton: each named ankle is moved to the
/// ground beneath where the clip put it, the joints above it rotate to follow
/// (two-bone IK), the hips drop when a leg cannot reach, and the sole tilts to
/// the slope. It runs wherever the pose does, so `engine list-joints --time`
/// reports the planted rig and the render draws it — one answer, not two.
///
/// The ground is a `Terrain` entity named in `ground`, and that is a purity
/// decision rather than a convenience: planting against the *physics* world
/// would make the pose a function of the simulation, and the pose being a pure
/// function of (files, time) is what lets `list-joints` answer at all. The cost
/// is that a character cannot stand on a crate — see
/// `designs/locomotion-design.md` §5.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FootPlant {
    /// The feet to plant, at most [`MAX_PLANTED_FEET`].
    pub feet: Vec<PlantedFoot>,

    /// The entity carrying the `Terrain` these feet stand on.
    pub ground: String,

    /// The joint lowered when a leg cannot reach its target — the pelvis, in
    /// a humanoid. Absent, the deficit is simply clamped: one foot plants and
    /// the other stretches, which is bounded but reads as wrong.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hips: Option<String>,

    /// How far below the animated ankle a target may be, in metres. This is
    /// what keeps a foot in mid-swing from being dragged to the floor:
    /// planting is a *correction*, and a correction with no ceiling is a
    /// different animation.
    #[serde(default = "half")]
    #[schemars(range(min = 0.0))]
    pub max_drop: f32,

    /// And how far above it.
    #[serde(default = "half")]
    #[schemars(range(min = 0.0))]
    pub max_lift: f32,

    /// Degrees the sole may tilt to meet the ground's normal. `0` leaves the
    /// foot's animated orientation alone, which is right for a character that
    /// only ever walks on the flat.
    #[serde(default = "thirty")]
    #[schemars(range(min = 0.0, max = 90.0))]
    pub align: f32,
}

fn thirty() -> f32 {
    30.0
}

/// The most proxies one skinned entity may carry, and the ceiling on
/// [`SkinnedCollider::parts`].
///
/// Bounded for `MAX_PLANTED_FEET`'s reason, at a number sized to the job: a
/// humanoid hitbox set is head, torso, pelvis, two arms in two pieces each,
/// two legs in two pieces each, hands and feet — fifteen. Thirty-two leaves
/// room for a detailed rig and still refuses the runaway case, which here is a
/// proxy per joint on a rig that has a hundred of them.
pub const MAX_COLLIDER_PARTS: usize = 32;

/// One proxy of a [`SkinnedCollider`]: a simple shape fixed in one joint's
/// frame (M33).
///
/// Flat and discriminated by `shape`, exactly as [`Collider`] is and for the
/// same reasons — the schema-driven walk and the editor's generated inspector
/// both read flat structs, and `jq` and an LLM both write them. `cuboid`,
/// `sphere` and `capsule` only: `trimesh` and `convex_hull` describe a
/// specific mesh, and a proxy exists precisely because that mesh lives on the
/// GPU where physics cannot reach it (`collider_part_shape_unsupported`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ColliderPart {
    /// The joint this proxy rides. Its posed world transform, times `offset`
    /// and `rotation`, is where the shape is each step.
    pub joint: String,

    pub shape: ColliderShapeKind,

    /// What reports call this part. Absent, the joint's name — which is right
    /// for the ordinary one-proxy-per-joint set and wrong the moment a limb
    /// takes two, hence the field. Unique within the component.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

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

    /// Metres from the joint's origin, **in the joint's own frame**, so a
    /// proxy is authored against the bone rather than against the world.
    #[serde(default)]
    #[schemars(with = "[f32; 3]")]
    pub offset: Vec3,

    /// Euler degrees in the joint's frame. A capsule's axis is local **+Y**
    /// (rapier's, and `builtin:cylinder`'s), and this is what turns it onto a
    /// bone that runs some other way.
    #[serde(default)]
    #[schemars(with = "[f32; 3]")]
    pub rotation: Vec3,

    /// Sensors detect overlaps but exert no forces — `Collider.sensor`,
    /// meaning the same thing. Per part rather than per component, because a
    /// sword's blade and its guard want opposite answers.
    #[serde(default)]
    pub sensor: bool,

    /// Solve this part's length from the posed bone instead of holding the
    /// authored one (M39). Absent — the default, and every pre-M39 part — is
    /// M33's rule exactly: only the placement follows the rig, never the size.
    ///
    /// `"bone"` takes a `capsule`'s `half_height`, or a `cuboid`'s Y
    /// half-extent, from the posed distance between this part's joint and that
    /// joint's first child, so a proxy set fitted to a rest pose keeps fitting
    /// a skeleton the solver is moving. A joint with no child keeps the
    /// authored value, which is what a hand or a head has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fit: Option<ColliderFit>,
}

/// How a proxy's size is decided (M39). See [`ColliderPart::fit`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ColliderFit {
    // NOTE: undocumented for `ColliderShapeKind`'s reason — a schemars doc
    // comment on a variant turns a flat "enum" into oneOf/const and blinds the
    // walk's closed-vocabulary check.
    Bone,
}

impl ColliderPart {
    /// What reports call this part: its `name`, or the joint it rides.
    pub fn part_name(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.joint)
    }
}

/// Collision proxies that follow a skinned character's pose (M33).
///
/// M30 said a skinned mesh is visual and physics sees only the entity's own
/// `Collider`; this is the one item of that reversed. Each part is re-posed
/// every fixed step from the same joint globals the render and
/// `engine list-joints` use, so a hitbox cannot disagree with the picture
/// about where a head is.
///
/// **The pose drives the proxies and nothing reads them back** — they are
/// kinematic, so they are hit, they push dynamic bodies, and they report
/// contacts, but they never move a joint. That is what keeps M30's claim that
/// the pose is a pure function of (files, time) true, and it is why a proxy
/// holds a character up exactly as much as a moving wall holds up the hand
/// pushing it: not at all. What a character stands on is still its own
/// `Collider`.
///
/// Layers, friction and restitution sit here rather than on each part: "bullets
/// hit hitboxes" is a statement about the character, and per-part copies would
/// be four more strings to keep in agreement per part.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SkinnedCollider {
    /// The proxies, at most [`MAX_COLLIDER_PARTS`].
    pub parts: Vec<ColliderPart>,

    /// Collision layers every part belongs to. Absent = every layer, exactly
    /// as on `Collider`. Empty is an error — omit the field instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layers: Option<Vec<String>>,

    /// Only interact with colliders belonging to these layers. Absent =
    /// interact with everything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collides_with: Option<Vec<String>>,

    /// `>= 0`.
    #[serde(default = "half")]
    #[schemars(range(min = 0.0))]
    pub friction: f32,

    /// Bounciness, `[0, 1]`, max-combined like `Collider.restitution`.
    #[serde(default)]
    #[schemars(range(min = 0.0, max = 1.0))]
    pub restitution: f32,
}

/// One joint's local placement while a [`Ragdoll`] owns the skeleton (M39).
///
/// **`rotation` is a quaternion, `w` last — the one rotation in this format
/// that is not XYZ Euler degrees.** `CLAUDE.md` names the reason under Traps:
/// XYZ Euler clamps the middle angle to ±90°, so an orientation the solver
/// integrated past that comes back as the `(±180, θ, ±180)` twin, and a
/// ragdoll's joints go past it in the first second. M30 drew the same line for
/// skeletal clips, and the distinction is *who wrote the numbers*: these are
/// the engine's, like a DCC tool's, not an agent's.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct JointPose {
    /// The joint this places. Every joint of the rig appears exactly once.
    pub joint: String,

    /// Metres, in the parent joint's frame.
    #[serde(default)]
    #[schemars(with = "[f32; 3]")]
    pub translation: Vec3,

    /// `[x, y, z, w]`, glTF's order and rapier's.
    #[serde(default = "identity_quat")]
    pub rotation: [f32; 4],

    /// Carried only when it is not 1: a ragdoll does not scale bones, so this
    /// is whatever the clip had at the moment of handoff.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<[f32; 3]>")]
    pub scale: Option<Vec3>,
}

fn identity_quat() -> [f32; 4] {
    [0.0, 0.0, 0.0, 1.0]
}

/// A per-joint override of [`Ragdoll`]'s default cone (M39).
///
/// A ragdoll whose every joint is a 45° cone reads as a bag. A knee that only
/// bends one way, and only backwards, is what makes it read as a body — and
/// both numbers being in the file is what makes it tunable without touching
/// Rust.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RagdollJoint {
    /// The joint to override. Some part of the `SkinnedCollider` must ride it,
    /// and it must not be the root — a root part has no joint to constrain.
    pub joint: String,

    /// Half-angle of the cone, in degrees. Ignored when `hinge` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0.0, max = 180.0))]
    pub limit: Option<f32>,

    /// Turn this joint into a hinge about this axis in the **child** part's
    /// local frame, instead of a cone. An elbow or a knee.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<[f32; 3]>")]
    pub hinge: Option<Vec3>,

    /// The hinge's travel in degrees, `[min, max]`. `hinge` only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<[f32; 2]>,
}

/// Physics driving a skinned character's skeleton (M39).
///
/// M33 said the pose drives the proxies and nothing reads them back, and called
/// that its whole design. **This reverses that sentence for one entity, once,
/// permanently.** When `active` turns true the entity's `SkinnedCollider`
/// proxies stop being kinematic followers and become dynamic bodies wired
/// together with rapier joints; from that step on the skeleton is a report of
/// where they ended up.
///
/// **The pose stays in the file, which is why invariant 2 survives the
/// reversal.** `pose` is written back after every step, exactly as M32 writes
/// `AnimationPlayer.phase` back, and `locomotion::posed_globals_at` reads it
/// before it looks at a clip — so the render, `engine list-joints`,
/// `engine list-colliders` and `world.joint_position` all see the ragdolled
/// skeleton through the seam they already shared. M32's rule is what settled
/// it: a ragdoll halfway to the floor, baked and reloaded, has to land in the
/// same heap, and a pose living in the physics world would reload standing up.
///
/// The bodies **are** the proxies, so the hitbox that was shot is the body that
/// falls, and the collider set does not change on handoff — which matters
/// because that set is an input to rapier's broad phase.
///
/// See `designs/ragdoll-design.md`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Ragdoll {
    /// Whether physics owns the skeleton. A scene may ship this true and its
    /// character is a corpse from step 0; `world.ragdoll(name)` sets it, and
    /// nothing clears it — the handoff is one-way (design §3).
    #[serde(default)]
    pub active: bool,

    /// kg/m³, `Collider.density`'s unit. Each part's mass is its shape's
    /// volume times this. Defaults to a shade under water, which is roughly
    /// what a person is.
    #[serde(default = "default_ragdoll_density")]
    #[schemars(range(min = 0.0))]
    pub density: f32,

    /// Half-angle in degrees of the cone every joint gets unless `joints`
    /// overrides it.
    #[serde(default = "default_ragdoll_limit")]
    #[schemars(range(min = 0.0, max = 180.0))]
    pub limit: f32,

    /// Velocity damping on every part. Deliberately above a physically honest
    /// value: a real body tumbles for longer than a game wants to watch, and
    /// this is the dial that fixes it.
    #[serde(default = "default_ragdoll_linear_damping")]
    #[schemars(range(min = 0.0))]
    pub linear_damping: f32,

    /// The same, for spin — and the one that stops a corpse pinwheeling.
    #[serde(default = "default_ragdoll_angular_damping")]
    #[schemars(range(min = 0.0))]
    pub angular_damping: f32,

    /// Joints that want something other than the default cone.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub joints: Vec<RagdollJoint>,

    /// The skeleton, once physics owns it: one entry per joint of the rig,
    /// written by the engine after every step. Absent until the handoff.
    ///
    /// It is in the file rather than in the physics world because that is what
    /// makes a baked ragdoll reload into the same heap — see the type's docs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pose: Option<Vec<JointPose>>,
}

fn default_ragdoll_density() -> f32 {
    985.0
}

fn default_ragdoll_limit() -> f32 {
    45.0
}

fn default_ragdoll_linear_damping() -> f32 {
    0.05
}

fn default_ragdoll_angular_damping() -> f32 {
    0.6
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
    HudPanel,
    HudImage,
    HudInteract,
    ParticleEmitter,
    Tree,
    Water,
    Cloud,
    Terrain,
    Road,
    Meadow,
    FootPlant,
    SkinnedCollider,
    Ragdoll,
    Buoyancy,
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
                "HudPanel",
                "HudImage",
                "HudInteract",
                "ParticleEmitter",
                "Tree",
                "Water",
                "Cloud",
                "Terrain",
                "Road",
                "Meadow",
                "FootPlant",
                "SkinnedCollider",
                "Ragdoll",
                "Buoyancy"
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
