/// Prepend the shared sky gradient to a shader source.
///
/// WGSL has no `#include` and wgpu has no preprocessor, so the sky pass and
/// the mesh pass share `sky_gradient` by concatenation. They have to share it:
/// the mesh pass reflects the sky off metal and water, and a reflection drawn
/// from a second copy of the curve would drift away from the sky behind it the
/// first time either was touched.
pub(crate) fn with_sky_common(source: &str) -> std::borrow::Cow<'static, str> {
    let mut combined = String::with_capacity(source.len() + 1024);
    combined.push_str(include_str!("../shaders/sky_common.wgsl"));
    combined.push('\n');
    combined.push_str(source);
    std::borrow::Cow::Owned(combined)
}

// ── The surface-resolution seam ──────────────────────────────────────────────
//
// M22 discovered this extension point and did not name it; M26 has a second
// producer at it, so it is named here.
//
// Everything in `mesh.wgsl` after a surface is resolved — the GGX lobe, the
// shadow lookup, the sky ambient, the point-light loop, the fog, the blend — is
// shared, and what varies is only where `albedo`, `metallic`, `roughness`,
// `emissive` and the shading normal come from. A producer supplies those; the
// lighting body never changes.
//
// What a producer may *not* do is edit `mesh.wgsl`: M16's four untouchable lines
// have to reach the compiler surrounded by the code they already shipped in, and
// putting terrain's branch inline moved one pixel by one unit in each of three
// committed fixtures (see `shaders/terrain.wgsl`). So the file on disk stays
// what it was, the plain mesh pipeline compiles it unchanged — byte-identical by
// construction, not by measurement — and a variant is assembled by anchored
// substitution. Every anchor is asserted to appear exactly once: reword
// `mesh.wgsl` and this fails loudly at startup rather than silently rendering
// terrain as flat grey or a texture as untextured.

/// The anchors a producer may replace, as they appear in `mesh.wgsl`.
pub(crate) mod anchor {
    /// The object uniform's last field, where a variant appends its own.
    pub const UNIFORM_TAIL: &str = "    // x = alpha, y = transmission; z and w unused.\n\
                                    \x20   surface: vec4<f32>,\n\
                                    };\n";
    /// The whole vertex stage. Unlike every other anchor this one is not
    /// *replaced* by a producer but **reassembled** from their
    /// [`VertexContribution`](super::VertexContribution)s, because since M27
    /// two producers need it at once: texturing needs a UV attribute the plain
    /// stage does not carry, and skinning needs two more. Whole-stage
    /// replacement worked while exactly one producer did it and does not
    /// survive two — and a textured skinned character is precisely the case
    /// that has to compose. The assembly with no contributions is asserted to
    /// be this string, byte for byte.
    pub const VERTEX_STAGE: &str = "struct VertexOut {\n\
        \x20   @builtin(position) clip_position: vec4<f32>,\n\
        \x20   @location(0) world_position: vec3<f32>,\n\
        \x20   @location(1) normal: vec3<f32>,\n\
        };\n\
        \n\
        @vertex\n\
        fn vs_main(\n\
        \x20   @location(0) position: vec3<f32>,\n\
        \x20   @location(1) normal: vec3<f32>,\n\
        ) -> VertexOut {\n\
        \x20   var out: VertexOut;\n\
        \x20   out.clip_position = object.mvp * vec4<f32>(position, 1.0);\n\
        \x20   out.world_position = (object.model * vec4<f32>(position, 1.0)).xyz;\n\
        \x20   out.normal = (object.normal_matrix * vec4<f32>(normal, 0.0)).xyz;\n\
        \x20   return out;\n\
        }\n";
    /// The fragment prologue: where albedo, metallic and emissive come from.
    pub const PROLOGUE: &str = "    let albedo = object.albedo_metallic.rgb;\n\
                                \x20   let metallic = object.albedo_metallic.w;\n\
                                \x20   let emissive = object.emissive_roughness.rgb;\n";
    pub const NORMAL: &str = "    let n = normalize(in.normal);\n";
    pub const ROUGHNESS: &str = "    let roughness = max(object.emissive_roughness.w, 0.045);\n";
    /// One of M16's four untouchable lines. A variant may replace it —
    /// the plain pipeline still compiles the file as it sits on disk — and
    /// occlusion is the only thing that has ever needed to.
    pub const AMBIENT: &str = "    let ambient = albedo * frame.ambient.rgb;\n";
    /// Its counterpart inside the sky branch.
    pub const FILL: &str = "            fill = albedo * hemisphere;\n";
    /// The frame uniform's tail, where the refraction variant appends the
    /// view-projection it needs to project an exit point.
    pub const FRAME_TAIL: &str = "    point_lights: array<PointLightData, MAX_POINT_LIGHTS>,\n};\n";
    /// Where the fragment's running colour and alpha are declared.
    pub const VARS: &str = "    var color = base_color;\n    var out_alpha = 1.0;\n";
    /// The last line of the fragment stage.
    pub const RETURN: &str =
        "    return vec4<f32>(clamp(color, vec3<f32>(0.0), vec3<f32>(1.0)), out_alpha);\n";
    /// The composite for a transmissive surface — the one place that decides
    /// what is seen *through* it.
    pub const BLENDED: &str = "            color = (lit_diffuse + fill) * out_alpha + lit_specular + reflection + emissive;\n";
}

/// The types the extended object uniform's tail names, declared by every
/// variant because its field *offsets* are positional — a variant that skipped
/// the terrain table would read the material's UV transform where the table is.
pub(crate) const EXTENDED_UNIFORM_TYPES: &str = "const MAX_TERRAIN_LAYERS: u32 = 4u;\n\
    \n\
    // One material a terrain paints itself with, claiming a band of world\n\
    // height and a band of slope (M22).\n\
    struct TerrainLayer {\n\
    \x20   // rgb = linear albedo, w = roughness.\n\
    \x20   albedo_roughness: vec4<f32>,\n\
    \x20   // x, y = world-Y band in metres; z, w = slope band in degrees.\n\
    \x20   bands: vec4<f32>,\n\
    \x20   // x = height fade in metres, y = boundary jitter, z = slope fade\n\
    \x20   // in degrees; w unused.\n\
    \x20   blend_noise: vec4<f32>,\n\
    };\n";

/// The extended object uniform every spliced variant declares.
///
/// One tail rather than one per producer, because uniform field offsets are
/// positional: a variant that declared only *its* fields would read terrain's
/// layer table where the material's UV transform is. Each producer uses what it
/// needs and ignores the rest; the plain pipeline declares none of it and reads
/// the shorter struct out of the same buffer, which is legal precisely because
/// every field it does read is at the offset it always was.
pub(crate) const EXTENDED_UNIFORM_TAIL: &str =
    "    // x = alpha, y = transmission; z and w unused.\n\
    \x20   surface: vec4<f32>,\n\
    \x20   // Terrain (M22), appended at the end so every field above keeps\n\
    \x20   // the offset the shader already reads it from. x = live layer\n\
    \x20   // count, y = texture scale in metres, z = colour variation,\n\
    \x20   // w = bump.\n\
    \x20   terrain: vec4<f32>,\n\
    \x20   // x = the terrain's seed; y, z, w unused.\n\
    \x20   terrain_seed: vec4<u32>,\n\
    \x20   terrain_layers: array<TerrainLayer, MAX_TERRAIN_LAYERS>,\n\
    \x20   // Material maps (M26): xy = uv scale, zw = uv offset.\n\
    \x20   map_uv: vec4<f32>,\n\
    \x20   // x = which maps are bound (bit 0 albedo, 1 orm, 2 normal,\n\
    \x20   // 3 emissive), y = alpha cutoff, z = normal strength, w = ior.\n\
    \x20   map_params: vec4<f32>,\n\
    \x20   // x = thickness in metres; yzw = per-channel attenuation.\n\
    \x20   map_volume: vec4<f32>,\n\
    };\n";

/// Assemble a variant of the mesh shader: the shared lighting body, with one
/// producer spliced in at the surface-resolution seam.
///
/// `prelude` goes ahead of the object uniform (a producer's own declarations
/// often reference its fields); `substitutions` are (anchor, replacement) pairs.
pub(crate) fn with_surface(producers: &[Producer]) -> std::borrow::Cow<'static, str> {
    let source = include_str!("../shaders/mesh.wgsl");

    let mut out = source.to_string();
    let substitutions = || producers.iter().flat_map(|p| p.substitutions.iter());
    for (anchor, _) in substitutions().chain([
        &(anchor::UNIFORM_TAIL, EXTENDED_UNIFORM_TAIL),
        // Not in any producer's substitution list — it is reassembled rather
        // than replaced — but just as load-bearing, so it is asserted here.
        &(anchor::VERTEX_STAGE, ""),
    ]) {
        assert_eq!(
            source.matches(anchor).count(),
            1,
            "mesh.wgsl no longer contains this anchor exactly once, so a spliced \
             pipeline variant would compile as if its feature were absent:\n{anchor}"
        );
    }

    out = out.replace(anchor::UNIFORM_TAIL, EXTENDED_UNIFORM_TAIL);
    let preludes: String = producers
        .iter()
        .map(|p| p.prelude)
        .collect::<Vec<_>>()
        .join("\n");
    out = out.replace(
        "struct ObjectUniform {",
        &format!("{EXTENDED_UNIFORM_TYPES}\n{preludes}\nstruct ObjectUniform {{"),
    );
    out = out.replace(
        anchor::VERTEX_STAGE,
        &vertex_stage(producers.iter().filter_map(|p| p.vertex.as_ref())),
    );
    for (anchor, replacement) in substitutions() {
        out = out.replace(anchor, replacement);
    }

    std::borrow::Cow::Owned(out)
}

/// One producer at the seam: declarations to put ahead of the object uniform,
/// its addition to the vertex stage, and the anchored substitutions that route
/// the fragment stage through it.
///
/// Composable, because a textured surface can also refract and can also be
/// skinned. Composition is what keeps the variant matrix from being a matrix of
/// hand-written shaders.
pub(crate) struct Producer {
    pub(crate) prelude: &'static str,
    /// What this producer adds to the vertex stage, if anything. Terrain and
    /// refraction add nothing: they resolve a surface per pixel.
    pub(crate) vertex: Option<VertexContribution>,
    pub(crate) substitutions: Vec<(&'static str, &'static str)>,
}

/// One producer's addition to the vertex stage.
///
/// Every field is a fragment of WGSL pasted into a fixed skeleton, and an
/// all-empty set of contributions assembles to [`anchor::VERTEX_STAGE`]
/// verbatim — which is the property that keeps the plain pipeline compiling
/// `mesh.wgsl` as it sits on disk. `producers_compose_without_a_hand_written_stage`
/// pins it.
#[derive(Default)]
pub(crate) struct VertexContribution {
    /// Extra `vs_main` parameters: whole `    @location(n) name: T,\n` lines.
    pub(crate) attributes: &'static str,
    /// Extra `VertexOut` fields, in the same shape.
    pub(crate) varyings: &'static str,
    /// Statements run before the position and normal are transformed.
    pub(crate) body: &'static str,
    /// The expression transformed in place of the `position` attribute — how a
    /// producer moves a vertex. At most one producer may set it.
    pub(crate) position: Option<&'static str>,
    /// And in place of `normal`, for the same reason and under the same rule.
    pub(crate) normal: Option<&'static str>,
    /// Statements run after `out` is filled, writing the extra varyings.
    pub(crate) tail: &'static str,
}

/// Assemble the vertex stage from its contributions, in producer order.
///
/// Ordered, and the order is the meaning: skinning moves the vertex and
/// texturing then passes that position through untouched, so a producer list
/// of `[skin, texture]` is a skinned textured surface rather than two
/// half-applied ones.
pub(crate) fn vertex_stage<'a>(
    contributions: impl Iterator<Item = &'a VertexContribution> + Clone,
) -> String {
    let concat = |field: fn(&VertexContribution) -> &'static str| -> String {
        contributions.clone().map(field).collect()
    };
    // A second producer moving the vertex would silently win or silently lose
    // depending on iteration order; neither is a thing to discover from a
    // render.
    let one = |what: &str, field: fn(&VertexContribution) -> Option<&'static str>| {
        let mut found = contributions.clone().filter_map(field);
        let first = found.next();
        assert!(
            found.next().is_none(),
            "two producers both claim to compute the vertex {what}; the stage \
             can only transform one expression"
        );
        first
    };

    let varyings = concat(|c| c.varyings);
    let attributes = concat(|c| c.attributes);
    let body = concat(|c| c.body);
    let tail = concat(|c| c.tail);
    let position = one("position", |c| c.position).unwrap_or("position");
    let normal = one("normal", |c| c.normal).unwrap_or("normal");

    format!(
        "struct VertexOut {{\n\
         \x20   @builtin(position) clip_position: vec4<f32>,\n\
         \x20   @location(0) world_position: vec3<f32>,\n\
         \x20   @location(1) normal: vec3<f32>,\n\
         {varyings}}};\n\
         \n\
         @vertex\n\
         fn vs_main(\n\
         \x20   @location(0) position: vec3<f32>,\n\
         \x20   @location(1) normal: vec3<f32>,\n\
         {attributes}) -> VertexOut {{\n\
         \x20   var out: VertexOut;\n\
         {body}\x20   out.clip_position = object.mvp * vec4<f32>({position}, 1.0);\n\
         \x20   out.world_position = (object.model * vec4<f32>({position}, 1.0)).xyz;\n\
         \x20   out.normal = (object.normal_matrix * vec4<f32>({normal}, 0.0)).xyz;\n\
         {tail}\x20   return out;\n\
         }}\n"
    )
}

/// The terrain producer (M22): a generative material in place of the uniform's.
pub(crate) fn terrain_producer() -> Producer {
    Producer {
        prelude: include_str!("../shaders/terrain.wgsl"),
        // Terrain resolves its material per pixel from the world position the
        // plain stage already interpolates, so it adds nothing to the vertex
        // stage.
        vertex: None,
        substitutions: vec![
            (
                anchor::PROLOGUE,
                "    let generated = terrain_surface(\n\
                 \x20       in.world_position,\n\
                 \x20       normalize(in.normal),\n\
                 \x20       length(frame.camera_pos.xyz - in.world_position),\n\
                 \x20       object.albedo_metallic.rgb,\n\
                 \x20       object.emissive_roughness.w,\n\
                 \x20   );\n\
                 \x20   let albedo = generated.albedo;\n\
                 \x20   let metallic = object.albedo_metallic.w;\n\
                 \x20   let emissive = object.emissive_roughness.rgb;\n",
            ),
            (anchor::NORMAL, "    let n = generated.normal;\n"),
            (
                anchor::ROUGHNESS,
                "    let roughness = max(generated.roughness, 0.045);\n",
            ),
        ],
    }
}

/// The texture producer (M26).
///
/// The second producer at the seam, and the one that proves the seam is worth
/// naming: it needs a vertex attribute the plain stage does not carry, and it
/// touches the occlusion half of the ambient terms, and it still shares every
/// line of the lighting body.
pub(crate) fn texture_producer() -> Producer {
    Producer {
        prelude: include_str!("../shaders/textured.wgsl"),
        // A UV attribute the plain stage does not carry, interpolated to the
        // fragment stage under the material's own scale and offset.
        vertex: Some(VertexContribution {
            attributes: "    @location(2) uv: vec2<f32>,\n",
            varyings: "    @location(2) uv: vec2<f32>,\n",
            tail: "    out.uv = uv * object.map_uv.xy + object.map_uv.zw;\n",
            ..VertexContribution::default()
        }),
        substitutions: vec![
            (
                anchor::PROLOGUE,
                "    let sampled = sample_maps(in.uv);\n\
                 \x20   // A cut pixel leaves before anything is lit — and its\n\
                 \x20   // shadow leaves through the caster pipeline that runs\n\
                 \x20   // this same test.\n\
                 \x20   if object.map_params.y > 0.0 && sampled.alpha < object.map_params.y {\n\
                 \x20       discard;\n\
                 \x20   }\n\
                 \x20   let albedo = object.albedo_metallic.rgb * sampled.albedo;\n\
                 \x20   let metallic = object.albedo_metallic.w * sampled.metallic;\n\
                 \x20   let emissive = object.emissive_roughness.rgb * sampled.emissive;\n",
            ),
            (
                anchor::NORMAL,
                "    let n = perturb_normal(\n\
                 \x20       normalize(in.normal),\n\
                 \x20       in.world_position,\n\
                 \x20       in.uv,\n\
                 \x20   );\n",
            ),
            (
                anchor::ROUGHNESS,
                "    let roughness = max(object.emissive_roughness.w * sampled.roughness, 0.045);\n",
            ),
            // Occlusion multiplies the ambient terms and **never** the direct
            // sun: that is the whole difference between ambient occlusion and a
            // second shadow map.
            (
                anchor::AMBIENT,
                "    let ambient = albedo * frame.ambient.rgb * sampled.occlusion;\n",
            ),
            (
                anchor::FILL,
                "            fill = albedo * hemisphere * sampled.occlusion;\n",
            ),
        ],
    }
}

/// The refraction producer (M26), for the blended pipelines.
///
/// It replaces exactly one line — the composite for a transmissive surface —
/// and appends one field to the frame uniform. A surface with `ior: 1.0` and
/// `thickness: 0.0` takes the `else` and lands on the pre-M26 expression
/// unchanged, which is what lets the blended pipelines compile this variant
/// without moving the ice in any committed fixture.
pub(crate) fn refraction_producer() -> Producer {
    Producer {
        prelude: include_str!("../shaders/refraction.wgsl"),
        // Refraction is entirely a fragment-stage decision: it bends a view
        // ray, it does not move a vertex.
        vertex: None,
        substitutions: vec![
            (
                anchor::FRAME_TAIL,
                "    point_lights: array<PointLightData, MAX_POINT_LIGHTS>,\n\
                 \x20   // World → clip (M26): the refraction variant projects an\n\
                 \x20   // exit point with it. Appended at the end, so every field\n\
                 \x20   // above keeps the offset the other shaders read it from.\n\
                 \x20   view_proj: mat4x4<f32>,\n\
                 };\n",
            ),
            (
                anchor::VARS,
                "    var color = base_color;\n\
                 \x20   var out_alpha = 1.0;\n\
                 \x20   // What a refracting surface lets through, held out of the\n\
                 \x20   // running colour until after fog. The copy it comes from\n\
                 \x20   // was already fogged at its own depth when the opaque pass\n\
                 \x20   // drew it, and fogging it a second time here is what turns\n\
                 \x20   // clear ice into a pale slab.\n\
                 \x20   var transmitted = vec3<f32>(0.0);\n",
            ),
            (
                anchor::BLENDED,
                "            if object.map_params.w != 1.0 || object.map_volume.x > 0.0 {\n\
                 \x20               // The frame behind this surface, bent and\n\
                 \x20               // absorbed. Taken from the copy rather than\n\
                 \x20               // left to the blend unit, which is the whole\n\
                 \x20               // difference: a blend can only show what is\n\
                 \x20               // straight behind, and refraction is about\n\
                 \x20               // what is *not*.\n\
                 \x20               let uv = refracted_uv(\n\
                 \x20                   in.world_position,\n\
                 \x20                   v,\n\
                 \x20                   n,\n\
                 \x20                   object.map_params.w,\n\
                 \x20                   object.map_volume.x,\n\
                 \x20               );\n\
                 \x20               transmitted = absorbed(\n\
                 \x20                   textureSampleLevel(scene_color, scene_sampler, uv, 0.0).rgb,\n\
                 \x20                   object.map_volume.yzw,\n\
                 \x20                   object.map_volume.x,\n\
                 \x20               ) * (1.0 - out_alpha);\n\
                 \x20           }\n\
                 \x20           color = (lit_diffuse + fill) * out_alpha + lit_specular + reflection + emissive;\n",
            ),
            (
                anchor::RETURN,
                "    // The transmitted background last, and the surface opaque\n\
                 \x20   // once it carries it: the blend unit must not add the\n\
                 \x20   // framebuffer's *un*-refracted version on top.\n\
                 \x20   if any(transmitted > vec3<f32>(0.0)) {\n\
                 \x20       color = color + transmitted;\n\
                 \x20       out_alpha = 1.0;\n\
                 \x20   }\n\
                 \x20   return vec4<f32>(clamp(color, vec3<f32>(0.0), vec3<f32>(1.0)), out_alpha);\n",
            ),
        ],
    }
}

/// The skinning producer (M27).
///
/// The only producer so far that *moves* a vertex, which is why the vertex
/// stage had to become an assembly: it needs two attributes the plain stage
/// does not carry, and so does texturing, and a rigged character is precisely
/// the thing that wants both.
pub(crate) fn skin_producer() -> Producer {
    Producer {
        prelude: include_str!("../shaders/skin.wgsl"),
        vertex: Some(VertexContribution {
            attributes: "    @location(3) joint_indices: vec4<u32>,\n\
                         \x20   @location(4) joint_weights: vec4<f32>,\n",
            body:
                "    let skinned = skin_vertex(position, normal, joint_indices, joint_weights);\n",
            position: Some("skinned.position"),
            normal: Some("skinned.normal"),
            ..VertexContribution::default()
        }),
        // Nothing in the fragment stage: a skinned surface is shaded exactly
        // like any other, which is the point of doing this in the vertex
        // stage at all.
        substitutions: Vec::new(),
    }
}

/// The two shadow casters, skinned (M27).
///
/// `shadow.wgsl` reads nothing but the object uniform's model matrix, so a
/// walking character would otherwise cast its rest pose — a wrongness that
/// reads as a renderer bug and is actually a missing pipeline. Spliced rather
/// than copied, for `with_surface`'s reason: two more hand-maintained shadow
/// shaders are two more things to drift out of step with the pass they belong
/// to.
///
/// Note for whoever debugs a missing shadow here: the solid caster is
/// **front-face culled** (M16's peeling margin), which applies to characters
/// as much as to M26's single-sided cards.
pub(crate) fn with_skinned_caster(source: &'static str, anchor: &str, replacement: &str) -> String {
    assert_eq!(
        source.matches(anchor).count(),
        1,
        "the shadow caster no longer contains this anchor exactly once, so the \
         skinned variant would compile as if skinning were absent:\n{anchor}"
    );
    format!(
        "{}\n{}",
        include_str!("../shaders/skin.wgsl"),
        source.replace(anchor, replacement)
    )
}

pub(crate) fn skinned_shadow() -> String {
    with_skinned_caster(
        include_str!("../shaders/shadow.wgsl"),
        "fn vs_main(@location(0) position: vec3<f32>) -> @builtin(position) vec4<f32> {\n\
         \x20   return frame.light_view_proj * object.model * vec4<f32>(position, 1.0);\n\
         }\n",
        "fn vs_main(\n\
         \x20   @location(0) position: vec3<f32>,\n\
         \x20   @location(3) joint_indices: vec4<u32>,\n\
         \x20   @location(4) joint_weights: vec4<f32>,\n\
         ) -> @builtin(position) vec4<f32> {\n\
         \x20   let skinned = skin_vertex(position, vec3<f32>(0.0, 1.0, 0.0), joint_indices, joint_weights);\n\
         \x20   return frame.light_view_proj * object.model * vec4<f32>(skinned.position, 1.0);\n\
         }\n",
    )
}

pub(crate) fn skinned_shadow_cutout() -> String {
    with_skinned_caster(
        include_str!("../shaders/shadow_cutout.wgsl"),
        "    out.clip = frame.light_view_proj * object.model * vec4<f32>(position, 1.0);\n",
        "    let skinned = skin_vertex(position, normal, joint_indices, joint_weights);\n\
         \x20   out.clip = frame.light_view_proj * object.model * vec4<f32>(skinned.position, 1.0);\n",
    )
    .replace(
        "    @location(2) uv: vec2<f32>,\n) -> VertexOut {",
        "    @location(2) uv: vec2<f32>,\n\
         \x20   @location(3) joint_indices: vec4<u32>,\n\
         \x20   @location(4) joint_weights: vec4<f32>,\n\
         ) -> VertexOut {",
    )
}

/// The mesh shader with terrain's generative material spliced in (M22).
pub(crate) fn with_terrain() -> std::borrow::Cow<'static, str> {
    with_surface(&[terrain_producer()])
}

/// The mesh shader with texture sampling spliced in (M26).
pub(crate) fn with_textures() -> std::borrow::Cow<'static, str> {
    with_surface(&[texture_producer()])
}

/// The blended twins, which also refract.
pub(crate) fn with_refraction() -> std::borrow::Cow<'static, str> {
    with_surface(&[refraction_producer()])
}

pub(crate) fn with_textures_and_refraction() -> std::borrow::Cow<'static, str> {
    with_surface(&[texture_producer(), refraction_producer()])
}

/// The anchors `with_water_refraction` splices against — existing lines of
/// `water.wgsl`, exactly as `anchor` holds existing lines of `mesh.wgsl`.
///
/// The file itself is **not edited by this milestone, including its comments**:
/// the plain pipeline compiles it as it sits on disk, byte-identical by
/// construction, and the substitution that claims each anchor is where the
/// variant's version of that text lives.
pub(crate) mod water_anchor {
    /// The depth copy's binding, and the last of the group-2 declarations —
    /// where the variant's own colour-copy bindings go in after it.
    pub const BINDINGS: &str = "@group(2) @binding(2) var scene_depth: texture_2d<f32>;\n";
    /// The clock, whose `z` M18 declared padding and M27 fills with the IOR.
    /// A comment-only substitution, and it earns its place: the disk file keeps
    /// describing the pipeline that actually compiles it.
    pub const CLOCK: &str = "    // x = wave count, y = time in seconds, z and w unused.\n\
                             \x20   clock: vec4<f32>,\n";
    /// Where the path length through the body is measured. The bend is scaled
    /// by it, so the sample is taken here rather than at the composite.
    pub const THICKNESS: &str = "    // Absorption along the view ray through the water body.\n\
         \x20   let thickness = water_thickness(in.clip, in.world);\n";
    /// The last line of the fragment stage.
    pub const RETURN: &str =
        "    return vec4<f32>(clamp(color, vec3<f32>(0.0), vec3<f32>(1.0)), out_alpha);\n";
}

/// The water shader with refraction spliced in (M27).
///
/// Same discipline as [`with_surface`]: every anchor must appear exactly once,
/// because a splice that silently did nothing renders the feature as if it were
/// absent — the failure mode hardest to see, since the scene still draws.
pub(crate) fn with_water_refraction() -> std::borrow::Cow<'static, str> {
    let source = include_str!("../shaders/water.wgsl");
    let substitutions = [
        (
            water_anchor::BINDINGS,
            concat!(
                "@group(2) @binding(2) var scene_depth: texture_2d<f32>;\n\n",
                include_str!("../shaders/water_refraction.wgsl"),
            ),
        ),
        (
            water_anchor::CLOCK,
            "    // x = wave count, y = time in seconds, z = index of refraction\n\
             \x20   // (M27), w unused.\n\
             \x20   clock: vec4<f32>,\n",
        ),
        (
            water_anchor::THICKNESS,
            "    // Absorption along the view ray through the water body.\n\
             \x20   let thickness = water_thickness(in.clip, in.world);\n\
             \x20   // What is behind the surface, bent (M27). Sampled here,\n\
             \x20   // where the measured thickness that scales the bend is in\n\
             \x20   // hand, and held out of the running colour until after fog:\n\
             \x20   // the copy was already fogged at its own depth when the\n\
             \x20   // opaque pass drew it, and fogging it twice is what turns\n\
             \x20   // clear water into a pale slab.\n\
             \x20   let bed = refracted_bed(\n\
             \x20       in.clip,\n\
             \x20       in.world,\n\
             \x20       v,\n\
             \x20       n,\n\
             \x20       surface.clock.z,\n\
             \x20       thickness,\n\
             \x20   );\n",
        ),
        (
            water_anchor::RETURN,
            "    // The bed last, and the surface opaque once it carries it: the\n\
             \x20   // blend unit must not add the framebuffer's un-refracted\n\
             \x20   // version on top. `1 - out_alpha` is exactly what that blend\n\
             \x20   // would have admitted, which is the whole claim — refraction\n\
             \x20   // moves where the bed is read from, not how much of it comes\n\
             \x20   // back, so turning it on cannot change how deep the water\n\
             \x20   // looks. Foam has already driven `out_alpha` toward 1 where\n\
             \x20   // it is opaque, and you cannot see through foam.\n\
             \x20   color = color + bed * (1.0 - out_alpha);\n\
             \x20   out_alpha = 1.0;\n\
             \x20   return vec4<f32>(clamp(color, vec3<f32>(0.0), vec3<f32>(1.0)), out_alpha);\n",
        ),
    ];

    let mut out = source.to_string();
    for (anchor, replacement) in substitutions {
        assert_eq!(
            source.matches(anchor).count(),
            1,
            "water.wgsl no longer contains this anchor exactly once, so the \
             refracting pipeline would compile as if refraction were absent:\n{anchor}"
        );
        out = out.replace(anchor, replacement);
    }
    std::borrow::Cow::Owned(out)
}

// ── The cascade seam (M38) ───────────────────────────────────────────────────
//
// Four shaders sample the shadow map — `mesh.wgsl`, `water.wgsl`, `road.wgsl`
// and `meadow.wgsl` — each with its own near-copy of the lookup. All four have
// to change together: a bind group layout declaring a `D2Array` texture against
// a shader declaring `texture_depth_2d` is a pipeline-creation error, not a
// rendering difference.
//
// They change the way everything else on this path changes — by anchored
// substitution, so that at one cascade the file that reaches the compiler is
// the file on disk. See CLAUDE.md's first trap for why that is not a stylistic
// preference.

/// The shadow-map bindings, identical in all four receivers.
const SHADOW_BINDINGS: &str = "@group(2) @binding(0) var shadow_map: texture_depth_2d;\n\
                               @group(2) @binding(1) var shadow_sampler: sampler_comparison;\n";

/// What they become, plus the cascade matrices and the live count.
fn cascade_bindings(cascades: u32) -> String {
    format!(
        "@group(2) @binding(0) var shadow_map: texture_depth_2d_array;\n\
         @group(2) @binding(1) var shadow_sampler: sampler_comparison;\n\
         \n\
         // The sun's nested cascades (M38), innermost first. The pipelines know\n\
         // the live count at build time, so it is a constant here rather than a\n\
         // uniform lane that could disagree with the texture's layer count.\n\
         const CASCADE_COUNT: u32 = {cascades}u;\n\
         \n\
         struct CascadeUniform {{\n\
         \x20   view_proj: array<mat4x4<f32>, {max}>,\n\
         }};\n\
         \n\
         @group(2) @binding(5) var<uniform> cascade: CascadeUniform;\n",
        max = super::MAX_SHADOW_CASCADES,
    )
}

/// One cascade's 3×3 PCF — M16's loop with a layer index, shared by all four
/// receivers because all four were already running the same nine taps.
const CASCADE_SAMPLE: &str = "\
/// One cascade's 3×3 PCF, and how far into that cascade's map the point sits.\n\
///\n\
/// `.x` is the lit fraction; `.y` is the inset, and a value above 1 means this\n\
/// cascade does not hold the point at all, so the caller moves outward.\n\
fn cascade_sample(index: u32, world_position: vec3<f32>, bias: f32) -> vec2<f32> {\n\
\x20   let light_clip = cascade.view_proj[index] * vec4<f32>(world_position, 1.0);\n\
\x20   let projected = light_clip.xyz / light_clip.w;\n\
\x20   if projected.z > 1.0 || projected.z < 0.0 {\n\
\x20       return vec2<f32>(1.0, 2.0);\n\
\x20   }\n\
\x20   let inset = max(abs(projected.x), abs(projected.y));\n\
\x20   if inset > 1.0 {\n\
\x20       return vec2<f32>(1.0, 2.0);\n\
\x20   }\n\
\n\
\x20   // Clip xy is [-1, 1] with +Y up; texture uv is [0, 1] with +V down.\n\
\x20   let uv = vec2<f32>(projected.x * 0.5 + 0.5, 0.5 - projected.y * 0.5);\n\
\x20   let reference = projected.z - bias;\n\
\n\
\x20   let texel = frame.params.z;\n\
\x20   var sum = 0.0;\n\
\x20   for (var y = -1; y <= 1; y = y + 1) {\n\
\x20       for (var x = -1; x <= 1; x = x + 1) {\n\
\x20           let offset = vec2<f32>(f32(x), f32(y)) * texel;\n\
\x20           // `...CompareLevel` rather than `...Compare`: this runs inside an\n\
\x20           // if, and the implicit-derivative form is only valid in uniform\n\
\x20           // control flow.\n\
\x20           sum = sum + textureSampleCompareLevel(\n\
\x20               shadow_map,\n\
\x20               shadow_sampler,\n\
\x20               uv + offset,\n\
\x20               index,\n\
\x20               reference,\n\
\x20           );\n\
\x20       }\n\
\x20   }\n\
\n\
\x20   return vec2<f32>(sum / 9.0, inset);\n\
}\n";

/// The cascaded lookup, in place of a receiver's single-map one.
///
/// `bias` is the receiver's own — water's is a flat constant and the other
/// three scale theirs by slope — because unifying them would change what a
/// water shadow looks like today, and at one cascade this milestone changes
/// nothing.
fn cascaded_lookup(name: &str, params: &str, world: &str, bias: &str) -> String {
    format!(
        "{CASCADE_SAMPLE}\n\
         /// How lit this point is by the sun: 1 fully, 0 fully shadowed.\n\
         ///\n\
         /// The cascades are nested, so the first that contains the point is\n\
         /// also the sharpest that does: the search runs outward and stops.\n\
         ///\n\
         /// M16's fade across the last 15% of a map becomes a fade *to the next\n\
         /// cascade*, and only the outermost still fades to lit — which is what\n\
         /// it did when it was the only one. A hard switch would show: two\n\
         /// cascades agree on where a shadow is but not on how soft it is, and\n\
         /// a penumbra that changes width along a curve across the ground reads\n\
         /// as a bug.\n\
         fn {name}({params}) -> f32 {{\n\
         {bias}\
         \x20   for (var c = 0u; c < CASCADE_COUNT; c = c + 1u) {{\n\
         \x20       let here = cascade_sample(c, {world}, bias);\n\
         \x20       if here.y > 1.0 {{\n\
         \x20           continue;\n\
         \x20       }}\n\
         \x20       let fade = smoothstep(0.85, 1.0, here.y);\n\
         \x20       if c + 1u == CASCADE_COUNT {{\n\
         \x20           return mix(here.x, 1.0, fade);\n\
         \x20       }}\n\
         \x20       if fade <= 0.0 {{\n\
         \x20           return here.x;\n\
         \x20       }}\n\
         \x20       let next = cascade_sample(c + 1u, {world}, bias);\n\
         \x20       if next.y > 1.0 {{\n\
         \x20           return mix(here.x, 1.0, fade);\n\
         \x20       }}\n\
         \x20       return mix(here.x, next.x, fade);\n\
         \x20   }}\n\
         \x20   return 1.0;\n\
         }}\n",
    )
}

/// The slope-scaled bias `mesh.wgsl`, `road.wgsl` and `meadow.wgsl` share.
const SLOPE_BIAS: &str = "\
\x20   let slope = sqrt(max(1.0 - n_dot_l * n_dot_l, 0.0)) / max(n_dot_l, 0.05);\n\
\x20   let bias = clamp(0.0006 * slope, 0.0004, 0.006);\n";

/// Replace a whole function — and the doc comment above it — in a shader.
///
/// Anchoring on the signature rather than on the forty lines under it, for the
/// reason the anchors exist at all: an anchor that has to be kept character-
/// identical to a body someone might reasonably reformat is an anchor that will
/// silently stop matching. The signature is the part that cannot change without
/// the call sites changing too.
///
/// The doc comment goes with it because it would otherwise be describing the
/// wrong function: `mesh.wgsl`'s says "over a single orthographic map", which is
/// exactly what the cascaded variant is not.
fn replace_function(source: &str, signature: &str, replacement: &str) -> String {
    assert_eq!(
        source.matches(signature).count(),
        1,
        "a shadow receiver no longer contains this signature exactly once, so the \
         cascaded pipeline would compile as if cascades were absent:\n{signature}"
    );
    // The signature sits in column zero, so this is already a line start.
    let mut start = source.find(signature).expect("asserted above");
    // Back up over the contiguous `///` block introducing it.
    while start > 0 {
        let previous = source[..start - 1].rfind('\n').map_or(0, |at| at + 1);
        if !source[previous..start].trim_start().starts_with("///") {
            break;
        }
        start = previous;
    }

    let body = &source[start..];
    let end = start
        + body
            .find("\n}\n")
            .expect("a WGSL function ends with a brace in column zero")
        + 3;
    format!("{}{replacement}{}", &source[..start], &source[end..])
}

/// A receiver shader with the cascaded lookup spliced in (M38).
///
/// At one cascade this returns the source untouched — not an equivalent
/// rearrangement of it, the same `&str` — which is what makes every committed
/// baseline safe by construction rather than by measurement.
pub(crate) fn with_cascades(source: &str, cascades: u32) -> std::borrow::Cow<'_, str> {
    if cascades <= 1 {
        return std::borrow::Cow::Borrowed(source);
    }
    assert_eq!(
        source.matches(SHADOW_BINDINGS).count(),
        1,
        "a shadow receiver no longer declares the shadow map exactly as the other \
         three do, so its cascaded variant would bind an array texture to a \
         2D sampler:\n{SHADOW_BINDINGS}"
    );
    let out = source.replace(SHADOW_BINDINGS, &cascade_bindings(cascades));

    // Water's lookup takes no normal — a wave surface is nearly flat, so M18
    // gave it a flat generous bias instead of a slope-scaled one.
    let (signature, replacement) = if out.contains("fn shadow_lit(world: vec3<f32>) -> f32 {") {
        (
            "fn shadow_lit(world: vec3<f32>) -> f32 {",
            cascaded_lookup(
                "shadow_lit",
                "world: vec3<f32>",
                "world",
                "    let bias = 0.0015;\n",
            ),
        )
    } else {
        (
            "fn shadow_factor(world_position: vec3<f32>, n_dot_l: f32) -> f32 {",
            cascaded_lookup(
                "shadow_factor",
                "world_position: vec3<f32>, n_dot_l: f32",
                "world_position",
                SLOPE_BIAS,
            ),
        )
    };
    std::borrow::Cow::Owned(replace_function(&out, signature, &replacement))
}

#[cfg(test)]
mod seam_tests {
    /// The anchors are asserted at pipeline build, but only for *presence*.
    /// This pins that each producer's substitution actually landed — a splice
    /// that silently did nothing renders the feature as if it were absent,
    /// which is the failure mode hardest to spot in a render.
    #[test]
    fn every_producer_actually_replaces_what_it_claims() {
        let terrain = super::with_terrain();
        assert!(terrain.contains("let albedo = generated.albedo;"));
        assert!(terrain.contains("let n = generated.normal;"));
        assert!(terrain.contains("terrain_layers: array<TerrainLayer"));

        let textured = super::with_textures();
        for expected in [
            "let sampled = sample_maps(in.uv);",
            "sampled.metallic",
            "sampled.roughness",
            "sampled.occlusion",
            "let n = perturb_normal(",
            "@location(2) uv: vec2<f32>,",
            "map_params: vec4<f32>,",
        ] {
            assert!(
                textured.contains(expected),
                "textured splice lost {expected:?}"
            );
        }
        // And the shared lighting body is still the one from the file.
        assert!(textured.contains("let base_color = direct + ambient + emissive;"));
    }

    /// The property the whole vertex-stage assembly rests on (M27): with no
    /// contributions it reproduces the stage `mesh.wgsl` ships, byte for byte.
    ///
    /// Without this the refactor would be a rewrite of the one mechanism that
    /// guards M16's four untouchable lines, verified by hoping.
    #[test]
    fn an_unassisted_vertex_stage_is_the_one_in_the_file() {
        assert_eq!(
            super::vertex_stage(std::iter::empty()),
            super::anchor::VERTEX_STAGE,
        );
        // And a producer that adds nothing to the vertex stage leaves it the
        // file's, which is what kept M22's and M26's committed baselines from
        // moving when the assembly replaced whole-stage substitution.
        for variant in [super::with_terrain(), super::with_refraction()] {
            assert!(
                variant.contains(super::anchor::VERTEX_STAGE),
                "a producer with no vertex contribution must leave the stage alone"
            );
        }
    }

    /// Two producers both needing the vertex stage is what forced the assembly
    /// (M27 §7): a rigged character is precisely the thing that also wants an
    /// albedo map, so the two must compose rather than compete.
    #[test]
    fn producers_compose_without_a_hand_written_stage() {
        let both = super::with_surface(&[super::skin_producer(), super::texture_producer()]);

        for expected in [
            // Both attributes reached `vs_main`…
            "    @location(2) uv: vec2<f32>,\n",
            "    @location(3) joint_indices: vec4<u32>,\n",
            "    @location(4) joint_weights: vec4<f32>,\n",
            // …and the skinned position is what gets transformed, once.
            "out.clip_position = object.mvp * vec4<f32>(skinned.position, 1.0);",
            "out.world_position = (object.model * vec4<f32>(skinned.position, 1.0)).xyz;",
            "out.normal = (object.normal_matrix * vec4<f32>(skinned.normal, 0.0)).xyz;",
            "out.uv = uv * object.map_uv.xy + object.map_uv.zw;",
            // …and the fragment stage still samples maps.
            "let sampled = sample_maps(in.uv);",
        ] {
            assert!(both.contains(expected), "composed stage lost {expected:?}");
        }
        assert!(
            !both.contains("object.mvp * vec4<f32>(position, 1.0)"),
            "the unskinned position must not survive alongside the skinned one"
        );
    }

    /// The joint palette reaches the GPU as three *rows*, and glam matrices are
    /// stored as columns (M27).
    ///
    /// A transpose dropped here is the kind of bug that renders — a character
    /// appears, moves when the clip moves it, and is inside out — so it is
    /// pinned by arithmetic rather than by looking.
    #[test]
    fn a_palette_entry_packs_as_rows_not_columns() {
        let matrix = glam::Mat4::from_scale_rotation_translation(
            glam::Vec3::new(1.0, 2.0, 3.0),
            glam::Quat::from_rotation_y(0.7),
            glam::Vec3::new(4.0, 5.0, 6.0),
        );
        let packed = crate::scene_renderer::uniforms::JointPaletteUniform::from_palette(&[matrix]);

        // The translation is the *fourth element of each row*, which is where
        // the shader's `dot(row, vec4(position, 1.0))` picks it up. Under a
        // column packing it would be the first three elements of row 3, which
        // is not even stored.
        assert_eq!(
            [
                packed.joints[0][0][3],
                packed.joints[0][1][3],
                packed.joints[0][2][3]
            ],
            [4.0, 5.0, 6.0],
        );
        // And the whole transform agrees with the matrix, for a point that is
        // not the origin.
        let point = glam::Vec3::new(0.3, -1.1, 2.0);
        let expected = matrix.transform_point3(point);
        let homogeneous = point.extend(1.0);
        let by_rows = glam::Vec3::new(
            glam::Vec4::from_array(packed.joints[0][0]).dot(homogeneous),
            glam::Vec4::from_array(packed.joints[0][1]).dot(homogeneous),
            glam::Vec4::from_array(packed.joints[0][2]).dot(homogeneous),
        );
        assert!(
            (by_rows - expected).length() < 1e-5,
            "packed rows transform to {by_rows:?}, the matrix to {expected:?}"
        );
    }

    /// One cascade must return the file itself, not a rearrangement of it
    /// (M38 §4). This is the property every committed baseline rests on, and
    /// the cheapest place to catch a splice that started firing unconditionally.
    #[test]
    fn one_cascade_leaves_every_receiver_exactly_as_it_sits_on_disk() {
        for source in [
            include_str!("../shaders/mesh.wgsl"),
            include_str!("../shaders/water.wgsl"),
            include_str!("../shaders/road.wgsl"),
            include_str!("../shaders/meadow.wgsl"),
        ] {
            assert_eq!(super::with_cascades(source, 1), source);
        }
    }

    /// And beyond one, that each substitution actually landed — a splice that
    /// silently did nothing would bind an array texture to a shader sampling a
    /// 2D one, which fails at pipeline creation rather than in a render, but
    /// only on the machine that runs it.
    #[test]
    fn the_cascaded_receivers_sample_an_array() {
        for (name, source) in [
            ("mesh", include_str!("../shaders/mesh.wgsl")),
            ("water", include_str!("../shaders/water.wgsl")),
            ("road", include_str!("../shaders/road.wgsl")),
            ("meadow", include_str!("../shaders/meadow.wgsl")),
        ] {
            let cascaded = super::with_cascades(source, 3);
            for expected in [
                "var shadow_map: texture_depth_2d_array;",
                "const CASCADE_COUNT: u32 = 3u;",
                "@group(2) @binding(5) var<uniform> cascade: CascadeUniform;",
                "fn cascade_sample(index: u32",
                "cascade.view_proj[index]",
            ] {
                assert!(
                    cascaded.contains(expected),
                    "{name}'s cascaded splice lost {expected:?}"
                );
            }
            // The single-map lookup is gone rather than shadowed by the new one.
            assert!(
                !cascaded.contains("frame.light_view_proj * vec4<f32>(world"),
                "{name} still holds its single-map projection"
            );
            // And the receiver keeps its own bias: water's is flat, the rest
            // scale theirs by slope.
            let slope = cascaded.contains("let slope = sqrt(max(1.0 - n_dot_l");
            assert_eq!(slope, name != "water", "{name} took the wrong bias");
        }
    }

    /// Every slot past the rig's joint count is zero, and nothing indexes them
    /// — validation refused the file that could (`too_many_joints`).
    #[test]
    fn unused_palette_slots_are_zeroed() {
        let packed = crate::scene_renderer::uniforms::JointPaletteUniform::from_palette(&[
            glam::Mat4::IDENTITY,
        ]);
        assert_eq!(packed.joints[1], [[0.0; 4]; 3]);
        assert_eq!(
            packed.joints[engine_core::skeleton::MAX_JOINTS - 1],
            [[0.0; 4]; 3]
        );
    }
}
