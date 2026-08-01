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
    /// One of M16's four untouchable lines.
    ///
    /// Like [`VERTEX_STAGE`], and since M35 for the same reason, this is not
    /// *replaced* by a producer but **reassembled** from their
    /// [`FillContribution`](super::FillContribution)s: texturing multiplies it
    /// by an occlusion map and GI replaces the constant sky term with a probe
    /// lookup, and a textured surface inside a probe volume is precisely the
    /// case that has to compose. `with_surface` applies substitutions by
    /// sequential `str::replace`, so two producers claiming one anchor is not a
    /// merge — it is a silent no-op for whichever runs second, which renders the
    /// feature as if it were absent. The assembly with no contributions is
    /// asserted to be this string, byte for byte.
    pub const AMBIENT: &str = "    let ambient = albedo * frame.ambient.rgb;\n";
    /// Its counterpart inside the sky branch, reassembled under the same rule.
    pub const FILL: &str = "            fill = albedo * hemisphere;\n";
    /// The frame uniform's tail, replaced wholesale by
    /// [`EXTENDED_FRAME_TAIL`](super::EXTENDED_FRAME_TAIL).
    ///
    /// Unconditional since M35, and for [`EXTENDED_UNIFORM_TAIL`]'s reason
    /// rather than a tidiness one: this was a *substitution* while refraction
    /// was the only producer that appended a field, and GI needs three more.
    /// Two producers appending to one positional struct would make the field
    /// order a function of the producer list — so a variant that happened to
    /// list them the other way would read `gi_origin` where `view_proj` is and
    /// render a plausible wrong picture. One tail, declared by every spliced
    /// variant, removes the error class instead of documenting it.
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

/// The extended frame uniform every spliced variant declares.
///
/// [`EXTENDED_UNIFORM_TAIL`]'s twin for group 1, and it exists for the same
/// reason: uniform field offsets are positional. `mesh.wgsl` on disk stops at
/// `point_lights` and every field beyond it is invisible to the plain pipeline,
/// which is legal — a shader may declare a prefix of the buffer it is bound to.
/// What is *not* safe is letting each producer append its own fields, because
/// then the offsets depend on the order the producers were listed in.
pub(crate) const EXTENDED_FRAME_TAIL: &str =
    "    point_lights: array<PointLightData, MAX_POINT_LIGHTS>,\n\
    \x20   // World → clip (M26): the refraction variant projects an exit point\n\
    \x20   // with it.\n\
    \x20   view_proj: mat4x4<f32>,\n\
    \x20   // The bound probe volume (M35). xyz = the world position of probe\n\
    \x20   // [0,0,0], w = metres between probes.\n\
    \x20   gi_origin: vec4<f32>,\n\
    \x20   // xyz = probes per axis, w = intensity.\n\
    \x20   gi_grid: vec4<f32>,\n\
    \x20   // x = edge blend in metres, y = 1 when a field is bound.\n\
    \x20   gi_params: vec4<f32>,\n\
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
        &(anchor::FRAME_TAIL, EXTENDED_FRAME_TAIL),
        // Not in any producer's substitution list — they are reassembled rather
        // than replaced — but just as load-bearing, so they are asserted here.
        &(anchor::VERTEX_STAGE, ""),
        &(anchor::AMBIENT, ""),
        &(anchor::FILL, ""),
    ]) {
        assert_eq!(
            source.matches(anchor).count(),
            1,
            "mesh.wgsl no longer contains this anchor exactly once, so a spliced \
             pipeline variant would compile as if its feature were absent:\n{anchor}"
        );
    }

    out = out.replace(anchor::UNIFORM_TAIL, EXTENDED_UNIFORM_TAIL);
    out = out.replace(anchor::FRAME_TAIL, EXTENDED_FRAME_TAIL);
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
    let (ambient, fill) = fill_lines(producers.iter().filter_map(|p| p.fill.as_ref()));
    out = out.replace(anchor::AMBIENT, &ambient);
    out = out.replace(anchor::FILL, &fill);
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
    /// How this producer changes the fill light, if at all (M35). Reassembled
    /// rather than substituted — see [`anchor::AMBIENT`].
    pub(crate) fill: Option<FillContribution>,
    pub(crate) substitutions: Vec<(&'static str, &'static str)>,
}

/// One producer's change to the ambient and sky-fill terms.
///
/// Two kinds, because the two compose differently. An `occlusion` multiplier is
/// a *scale* on whatever the fill turned out to be, and any number of producers
/// may contribute one — they multiply. An `irradiance` expression *replaces*
/// where the fill comes from, and at most one producer may contribute it, for
/// [`VertexContribution::position`]'s reason: a second would silently win or
/// lose on iteration order.
///
/// An all-empty set assembles to [`anchor::AMBIENT`] and [`anchor::FILL`]
/// verbatim, which is what keeps the plain pipeline compiling `mesh.wgsl` as it
/// sits on disk — and what keeps M16's untouchable lines reaching the compiler
/// surrounded by the code they shipped in.
#[derive(Default)]
pub(crate) struct FillContribution {
    /// A factor the ambient terms are multiplied by — an occlusion map, say.
    /// Written as the WGSL that follows a `*`, e.g. `"sampled.occlusion"`.
    ///
    /// Multiplies the *ambient* terms and **never** the direct sun: that is the
    /// whole difference between ambient occlusion and a second shadow map.
    pub(crate) occlusion: Option<&'static str>,
    /// The expression the ambient term reads instead of `frame.ambient.rgb`.
    /// At most one producer may set it.
    pub(crate) ambient_source: Option<&'static str>,
    /// The expression the sky branch reads instead of `hemisphere`, under the
    /// same rule.
    pub(crate) hemisphere_source: Option<&'static str>,
}

/// Assemble the two fill lines from their contributions, in producer order.
///
/// Returns `(ambient, fill)` — the replacements for [`anchor::AMBIENT`] and
/// [`anchor::FILL`]. With no contributions each is its anchor byte for byte,
/// which `producers_compose_without_a_hand_written_fill` pins.
pub(crate) fn fill_lines<'a>(
    contributions: impl Iterator<Item = &'a FillContribution> + Clone,
) -> (String, String) {
    // A second producer deciding where the fill comes from would silently win
    // or lose depending on iteration order; neither is a thing to discover from
    // a render. `vertex_stage`'s rule, for its reason.
    let one = |what: &str, field: fn(&FillContribution) -> Option<&'static str>| {
        let mut found = contributions.clone().filter_map(field);
        let first = found.next();
        assert!(
            found.next().is_none(),
            "two producers both claim to compute the {what}; the fill can only \
             come from one place"
        );
        first
    };

    // Multipliers concatenate in producer order, so the arithmetic a variant
    // compiles is a function of the producer list and nothing else.
    let multipliers: String = contributions
        .clone()
        .filter_map(|c| c.occlusion)
        .map(|m| format!(" * {m}"))
        .collect();

    let ambient_source = one("ambient source", |c| c.ambient_source).unwrap_or("frame.ambient.rgb");
    let hemisphere_source =
        one("hemisphere source", |c| c.hemisphere_source).unwrap_or("hemisphere");

    (
        format!("    let ambient = albedo * {ambient_source}{multipliers};\n"),
        format!("            fill = albedo * {hemisphere_source}{multipliers};\n"),
    )
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
        // Terrain shades itself per pixel but takes the fill light every
        // other opaque surface takes — it is an ordinary lit surface that
        // happens to compute its own albedo.
        fill: None,
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
        // Occlusion multiplies the ambient terms and **never** the direct sun:
        // that is the whole difference between ambient occlusion and a second
        // shadow map. A multiplier rather than a replacement, so it composes
        // with GI deciding where the fill comes from — before M35 this was a
        // whole-line substitution, and a second claimant would have silently
        // erased it.
        fill: Some(FillContribution {
            occlusion: Some("sampled.occlusion"),
            ..FillContribution::default()
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
        // Refraction bends what is seen *through* a surface; it does not
        // change what lights the surface itself.
        fill: None,
        substitutions: vec![
            // `view_proj` is no longer appended here: since M35 the frame tail
            // is one unconditional extension (`EXTENDED_FRAME_TAIL`), because a
            // second producer with its own fields would have made the offsets a
            // function of the producer order. This variant reads the field; it
            // no longer declares it.
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

/// The global illumination producer (M35), and the first to contribute nothing
/// but a [`FillContribution`].
///
/// It replaces no line and adds no vertex work. Both of the lines it changes are
/// *reassembled* around it, which is what lets a textured surface inside a probe
/// volume take its occlusion map and its baked irradiance at once — the case
/// that made whole-line replacement untenable and the reason `AMBIENT` and
/// `FILL` stopped being substitutions.
///
/// `gi_fill` takes the pre-M35 expression as its `fallback` argument, so a
/// fragment outside every volume, or one in a scene whose `intensity` is `0.0`,
/// evaluates to exactly what it evaluated to before this producer existed. Not
/// approximately: the early return is on the *expression*, not on a lerp
/// weight.
pub(crate) fn gi_producer() -> Producer {
    Producer {
        prelude: include_str!("../shaders/gi.wgsl"),
        // GI is a lookup at a point, resolved per pixel. Nothing moves.
        vertex: None,
        fill: Some(FillContribution {
            // No occlusion multiplier: GI does not *scale* the fill, it decides
            // where the fill comes from. A surface with a baked ambient
            // occlusion map still multiplies its own map on top, which is why
            // the two kinds of contribution are kept apart.
            occlusion: None,
            ambient_source: Some("gi_fill(in.world_position, n, frame.ambient.rgb)"),
            hemisphere_source: Some("gi_fill(in.world_position, n, hemisphere)"),
        }),
        substitutions: Vec::new(),
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
        // A skinned surface takes the same fill as any other; the skin moved
        // the vertex and stopped there.
        fill: None,
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

/// Assemble a variant, with the GI producer appended when the renderer was
/// built for a scene that has a probe volume.
///
/// **Last in the list, always.** Producer order is the compiled arithmetic:
/// appending GI leaves the multiplier string exactly what it was, so a textured
/// surface's line grows a new *source* and keeps its `* sampled.occlusion` in
/// the position it has held since M26. Inserting GI first would reorder a float
/// product, and this file is ULP-sensitive in four places.
pub(crate) fn with_gi(mut producers: Vec<Producer>, gi: bool) -> std::borrow::Cow<'static, str> {
    if gi {
        producers.push(gi_producer());
    }
    with_surface(&producers)
}

/// The plain mesh shader: `mesh.wgsl` exactly as it sits on disk when the scene
/// has no probe volume.
///
/// The `false` arm is the whole M16 house rule in one expression — a scene that
/// opted into nothing compiles the untouched file, byte-identical by
/// construction rather than by measurement. Every baseline in the repo is blessed
/// through it.
pub(crate) fn plain_mesh(gi: bool) -> std::borrow::Cow<'static, str> {
    if gi {
        with_gi(Vec::new(), true)
    } else {
        std::borrow::Cow::Borrowed(include_str!("../shaders/mesh.wgsl"))
    }
}

/// The mesh shader with terrain's generative material spliced in (M22).
pub(crate) fn with_terrain(gi: bool) -> std::borrow::Cow<'static, str> {
    with_gi(vec![terrain_producer()], gi)
}

/// The mesh shader with texture sampling spliced in (M26).
pub(crate) fn with_textures(gi: bool) -> std::borrow::Cow<'static, str> {
    with_gi(vec![texture_producer()], gi)
}

/// The blended twins, which also refract.
pub(crate) fn with_refraction(gi: bool) -> std::borrow::Cow<'static, str> {
    with_gi(vec![refraction_producer()], gi)
}

pub(crate) fn with_textures_and_refraction(gi: bool) -> std::borrow::Cow<'static, str> {
    with_gi(vec![texture_producer(), refraction_producer()], gi)
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

#[cfg(test)]
mod seam_tests {
    /// The anchors are asserted at pipeline build, but only for *presence*.
    /// This pins that each producer's substitution actually landed — a splice
    /// that silently did nothing renders the feature as if it were absent,
    /// which is the failure mode hardest to spot in a render.
    #[test]
    fn every_producer_actually_replaces_what_it_claims() {
        let terrain = super::with_terrain(false);
        assert!(terrain.contains("let albedo = generated.albedo;"));
        assert!(terrain.contains("let n = generated.normal;"));
        assert!(terrain.contains("terrain_layers: array<TerrainLayer"));

        let textured = super::with_textures(false);
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
        for variant in [super::with_terrain(false), super::with_refraction(false)] {
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

    /// The empty fill assembly must be M16's two lines byte for byte (M35).
    ///
    /// This is the property the whole reassembly rests on: those lines are two
    /// of the four CLAUDE.md flags as ULP-sensitive, and they have to reach the
    /// compiler surrounded by the code they shipped in. If this drifts by a
    /// space, every variant that does *not* use GI starts compiling different
    /// arithmetic — and the A/B that would catch it runs once a milestone, not
    /// once a build.
    #[test]
    fn the_empty_fill_assembly_is_the_anchor_byte_for_byte() {
        let (ambient, fill) = super::fill_lines(std::iter::empty());
        assert_eq!(ambient, super::anchor::AMBIENT);
        assert_eq!(fill, super::anchor::FILL);
    }

    /// And with only the texture producer it must be exactly what that producer
    /// used to substitute, or M26's occlusion silently changes shape.
    #[test]
    fn the_texture_only_fill_assembly_matches_the_pre_m35_substitution() {
        let texture = super::texture_producer();
        let (ambient, fill) = super::fill_lines(texture.fill.iter());
        assert_eq!(
            ambient,
            "    let ambient = albedo * frame.ambient.rgb * sampled.occlusion;\n"
        );
        assert_eq!(
            fill,
            "            fill = albedo * hemisphere * sampled.occlusion;\n"
        );
    }

    /// Two producers on the fill is the case that forced the assembly, and the
    /// one `str::replace` could not express: an occlusion map *and* a probe
    /// lookup have to both survive.
    #[test]
    fn an_occluder_and_an_irradiance_source_compose() {
        let occlusion = super::FillContribution {
            occlusion: Some("sampled.occlusion"),
            ..Default::default()
        };
        let gi = super::FillContribution {
            ambient_source: Some("gi_ambient"),
            hemisphere_source: Some("gi_hemisphere"),
            ..Default::default()
        };
        let (ambient, fill) = super::fill_lines([&occlusion, &gi].into_iter());
        assert_eq!(
            ambient,
            "    let ambient = albedo * gi_ambient * sampled.occlusion;\n"
        );
        assert_eq!(
            fill,
            "            fill = albedo * gi_hemisphere * sampled.occlusion;\n"
        );
    }

    /// Two producers claiming to *be* the fill is the error that must be loud.
    #[test]
    #[should_panic(expected = "the fill can only come from one place")]
    fn two_irradiance_sources_are_refused() {
        let a = super::FillContribution {
            ambient_source: Some("one"),
            ..Default::default()
        };
        let b = super::FillContribution {
            ambient_source: Some("two"),
            ..Default::default()
        };
        super::fill_lines([&a, &b].into_iter());
    }

    /// The real producer, composed with the real texture producer — the case
    /// the tour is full of, and the one that decided the seam's shape.
    #[test]
    fn a_textured_surface_inside_a_volume_takes_both() {
        let both = super::with_gi(vec![super::texture_producer()], true);

        // The occlusion map survived a second claimant…
        assert!(both.contains(
            "    let ambient = albedo * gi_fill(in.world_position, n, frame.ambient.rgb) \
             * sampled.occlusion;\n"
        ));
        assert!(both.contains(
            "            fill = albedo * gi_fill(in.world_position, n, hemisphere) \
             * sampled.occlusion;\n"
        ));
        // …the field is declared…
        assert!(both.contains("@group(2) @binding(5) var gi_sh0: texture_3d<f32>;"));
        // …and the shared lighting body is still the file's.
        assert!(both.contains("let base_color = direct + ambient + emissive;"));
    }

    /// Every spliced variant declares the *whole* frame tail, and the file on
    /// disk still declares the short one (M35).
    ///
    /// The failure this prevents compiles cleanly and renders a plausible wrong
    /// picture: uniform field offsets are positional, so a variant that declared
    /// only some of the tail would read `gi_origin` where `view_proj` is. It is
    /// the same error class as a swapped pair of `Vec<usize>` keys in `FramePlan`
    /// — no type catches it and no assertion fires.
    #[test]
    fn every_spliced_variant_declares_the_whole_frame_tail() {
        for variant in [
            super::with_terrain(false),
            super::with_textures(false),
            super::with_refraction(false),
            super::with_textures_and_refraction(true),
            super::plain_mesh(true),
        ] {
            assert!(
                variant.contains("view_proj: mat4x4<f32>,")
                    && variant.contains("gi_origin: vec4<f32>,")
                    && variant.contains("gi_grid: vec4<f32>,")
                    && variant.contains("gi_params: vec4<f32>,"),
                "a spliced variant declared only part of the frame uniform's tail"
            );
            assert!(
                !variant.contains(super::anchor::FRAME_TAIL),
                "the short tail must not survive beside the long one"
            );
        }
        // And the untouched file keeps the short tail, which is what lets the
        // plain pipeline compile `mesh.wgsl` as it sits on disk.
        let plain = super::plain_mesh(false);
        assert!(plain.contains(super::anchor::FRAME_TAIL));
        assert!(!plain.contains("gi_origin"));
        assert_eq!(plain, include_str!("../shaders/mesh.wgsl"));
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
