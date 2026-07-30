// Particle billboards (M13): soft unlit discs, alpha-blended over the scene.
//
// One instance per particle; the six vertices of the quad are generated from
// vertex_index, expanded along the camera's right/up axes so every sprite
// faces the viewer. Colors are linear — the sRGB render target encodes on
// write, exactly like the mesh shader's output path.

struct ParticleFrame {
    view_proj: mat4x4<f32>,
    // xyz = normalized camera basis vectors; w unused.
    camera_right: vec4<f32>,
    camera_up: vec4<f32>,
    // xyz = world-space camera position, w = fog density (0 disables fog).
    camera_pos: vec4<f32>,
    // rgb = fog color, which is the sky's horizon color; a unused.
    fog_color: vec4<f32>,
};

@group(0) @binding(0) var<uniform> frame: ParticleFrame;

struct VsIn {
    @builtin(vertex_index) index: u32,
    // xyz = world position, w = billboard half-size in world units.
    @location(0) pos_size: vec4<f32>,
    // rgb = linear color, a = opacity.
    @location(1) color: vec4<f32>,
    // xyz = world velocity, w = stretch in seconds (0 = a round sprite).
    @location(2) velocity_stretch: vec4<f32>,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    // The corner in [-1, 1]² — the fragment stage's distance field.
    @location(0) corner: vec2<f32>,
    @location(1) color: vec4<f32>,
    // Distance from the camera to the sprite's center. Per-vertex is plenty:
    // a billboard is flat and small, so fog across one is effectively
    // constant.
    @location(2) view_distance: f32,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var corners = array<vec2<f32>, 6>(
        vec2(-1.0, -1.0), vec2(1.0, -1.0), vec2(1.0, 1.0),
        vec2(-1.0, -1.0), vec2(1.0, 1.0), vec2(-1.0, 1.0),
    );
    let corner = corners[in.index];

    // Velocity stretching (M17). A round sprite is a poor ember: a real one
    // draws a streak, because it moves further during the exposure than its own
    // width. So the quad is elongated along the *screen-space* projection of
    // the velocity — screen-space, because a particle flying at the camera has
    // no visible direction of travel and must stay round rather than collapsing
    // to a line.
    //
    // The `stretch == 0` branch is the pre-M17 expression, character for
    // character. Folding both cases into one lerp would be tidier and would
    // also mean every previously-blessed sprite goes through new arithmetic;
    // this is the same discipline mesh.wgsl follows for its M4 lines.
    var world: vec3<f32>;
    let stretch = in.velocity_stretch.w;
    if stretch > 0.0 {
        // The velocity in the camera's 2D basis.
        let planar = vec2<f32>(
            dot(in.velocity_stretch.xyz, frame.camera_right.xyz),
            dot(in.velocity_stretch.xyz, frame.camera_up.xyz),
        );
        let speed = length(planar);
        if speed > 1e-4 {
            let along = planar / speed;
            let across = vec2<f32>(-along.y, along.x);
            // Grow along the direction of travel by the distance covered in
            // `stretch` seconds; the cross-section keeps the authored size, so
            // a stretched sprite gets longer without getting fatter.
            let half_length = in.pos_size.w + speed * stretch;
            let plane = along * (corner.y * half_length) + across * (corner.x * in.pos_size.w);
            world = in.pos_size.xyz
                + frame.camera_right.xyz * plane.x
                + frame.camera_up.xyz * plane.y;
        } else {
            world = in.pos_size.xyz
                + (frame.camera_right.xyz * corner.x + frame.camera_up.xyz * corner.y)
                    * in.pos_size.w;
        }
    } else {
        world = in.pos_size.xyz
            + (frame.camera_right.xyz * corner.x + frame.camera_up.xyz * corner.y)
                * in.pos_size.w;
    }

    var out: VsOut;
    out.clip = frame.view_proj * vec4(world, 1.0);
    out.corner = corner;
    out.color = in.color;
    out.view_distance = length(in.pos_size.xyz - frame.camera_pos.xyz);
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Quadratic falloff from the center: a soft puff rather than a hard
    // circle, and exactly zero at the quad edge so sprites never show seams.
    let d = length(in.corner);
    let fade = clamp(1.0 - d, 0.0, 1.0);
    let alpha = in.color.a * fade * fade;
    // The corners outside the disc — over a fifth of every sprite — and any
    // particle faded to nothing contribute alpha 0, and `src * 0 + dst * 1`
    // leaves the destination byte for byte. Dropping those fragments is
    // therefore bit-identical to blending them, and smoke is drawn
    // back-to-front over large parts of the screen, so it is the cheapest
    // fill this pass can save.
    if alpha == 0.0 {
        discard;
    }

    // Fog (M16), off unless the scene asked for it. Distant smoke that stays
    // fully saturated while the geometry behind it fades reads as a decal
    // stuck to the lens; fogging the sprite's color puts it back in the air
    // with everything else. Only the color is fogged, never the alpha — haze
    // does not make a puff of smoke thicker.
    var rgb = in.color.rgb;
    let density = frame.camera_pos.w;
    if density > 0.0 {
        let amount = clamp(1.0 - exp(-pow(in.view_distance * density, 2.0)), 0.0, 1.0);
        rgb = mix(rgb, frame.fog_color.rgb, amount);
    }
    return vec4(rgb, alpha);
}
