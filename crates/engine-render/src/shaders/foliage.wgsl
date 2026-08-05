// Foliage sway (M46): the wind, in the vertex stage.
//
// Spliced ahead of `mesh.wgsl`'s object uniform by `foliage_producer`, and
// ahead of the two shadow casters by their own splices — a tree whose shadow
// does not move is worse than one that does not move at all, because a surface
// displaced away from its own recorded depth acnes and the acne crawls.
//
// The shape of the motion is two terms:
//
//   1. a **bend**, a rotation of the vertex about the tree's own foot, by an
//      angle that grows with the per-vertex weight the generator authored
//      (`MeshData::sway`, see `engine_core::tree`) — so the trunk's base is
//      exactly fixed, its top drifts, and the twigs wave;
//   2. a **flutter**, a beat along the vertex's own normal on a per-leaf phase,
//      applied to leaf draws only.
//
// A rotation rather than a translation, for the reason a meadow bends rather
// than leaning: translating a branch stretches it away from the one it grows
// out of, and the join opens.
//
// Everything here is a pure function of (uniforms, vertex), and the only clock
// is `object.foliage_clock.x` — the same scene time `--time` sets. There is no
// wind state, no integration, and nothing carried between frames, which is what
// keeps a moving tree inside a `diff-render` baseline.

/// Metres per unit of the wind-noise coordinate — the size of a gust.
///
/// **The same constant `meadow.wgsl` uses**, against the same travelling noise,
/// which is the entire reason it is copied rather than chosen: a meadow under a
/// stand of trees has to gust *with* it, and two independently tuned wind
/// fields in one frame read as two weathers.
const FOLIAGE_GUST_SCALE: f32 = 0.06;

/// The still-air fraction of the bend: what the wind does between gusts.
///
/// `meadow.wgsl`'s `0.35 + 0.65 * gust`, for its reason — wind that drops to
/// nothing between gusts reads as a fan being switched on and off.
const FOLIAGE_LULL: f32 = 0.35;

/// How fast a leaf beats, in Hz, at zero wind speed and per metre/second of it.
///
/// Leaves are the fast half of the motion — around 2 Hz at the default breeze,
/// which is what a poplar actually does and roughly the fastest thing the eye
/// still reads as motion rather than as noise. A field was considered and
/// dropped: `Tree` carries thirty, and a leaf that beats out of proportion to
/// the wind moving it is not a thing anyone wants to author.
const FLUTTER_BASE_HZ: f32 = 0.6;
const FLUTTER_HZ_PER_SPEED: f32 = 0.45;

const FOLIAGE_TAU: f32 = 6.28318530717959;

// The hash and the value noise, duplicated from `meadow.wgsl` rather than
// shared — the `sky_common.wgsl` seam prepends one file and only one, and these
// twenty lines are cheaper to copy than a second injection point is to build.
// What matters is that the bits are identical, since both are what a scene file
// *means* under a baseline.

fn foliage_hash_u32(value: u32) -> u32 {
    var h = value;
    h = h ^ (h >> 16u);
    h = h * 0x7FEB352Du;
    h = h ^ (h >> 15u);
    h = h * 0x846CA68Bu;
    h = h ^ (h >> 16u);
    return h;
}

fn foliage_rand01(seed: u32, salt: u32) -> f32 {
    return f32(foliage_hash_u32(seed ^ foliage_hash_u32(salt)) >> 8u) / 16777216.0;
}

/// Smooth 1-D value noise — smooth because per-step randomness makes foliage
/// *vibrate* rather than sway.
fn foliage_value_noise(x: f32) -> f32 {
    let cell = floor(x);
    let f = x - cell;
    let smoothed = f * f * (3.0 - 2.0 * f);
    let index = bitcast<u32>(i32(cell));
    let a = foliage_rand01(index, 0x9E3779B9u);
    let b = foliage_rand01(index + 1u, 0x9E3779B9u);
    return mix(a, b, smoothed);
}

/// How hard the wind is blowing where this vertex is, in `[0, 1]`-ish.
///
/// Sampled against a coordinate that **travels with the wind**, which is what
/// makes a gust cross a stand of trees as a wave instead of making every tree
/// shimmer on its own. The dot is taken against the *local* wind direction on
/// purpose: the bend has to come out pointing the same way in world space for
/// every tree however each one is yawed, so the CPU packs the direction in
/// entity space — and the price is that a yawed tree meets the gust front at a
/// yawed phase, which is an offset in *when*, never in *which way*.
fn foliage_gust(world_position: vec3<f32>) -> f32 {
    let travel = dot(vec2<f32>(world_position.x, world_position.z), object.foliage_wind.zw)
            * FOLIAGE_GUST_SCALE
        - object.foliage_clock.x * object.foliage_wind.y * FOLIAGE_GUST_SCALE;
    return foliage_value_noise(travel) * 0.65 + foliage_value_noise(travel * 2.7 + 11.0) * 0.35;
}

/// Where the wind has put this vertex, in the entity's own space.
///
/// `sway.x` is the weight, `sway.y` the leaf's flutter phase in turns.
fn foliage_vertex(local: vec3<f32>, normal: vec3<f32>, sway: vec2<f32>) -> vec3<f32> {
    let world_position = (object.model * vec4<f32>(local, 1.0)).xyz;
    let strength = FOLIAGE_LULL + (1.0 - FOLIAGE_LULL) * foliage_gust(world_position);

    // ── the bend ──────────────────────────────────────────────────────────
    //
    // A rotation about the entity's origin — which is the tree's foot, since
    // `tree.rs` grows from `Vec3::ZERO` — in its local horizontal/vertical
    // frame. The same decomposition `meadow.wgsl` bends a blade of grass with:
    // split the vertex into the component along the wind and the component
    // across it, turn the first against the vertical, leave the second alone.
    let bend_dir = object.foliage_wind.zw;
    let theta = object.foliage_wind.x * sway.x * strength;
    let bend_sin = sin(theta);
    let bend_cos = cos(theta);

    let flat = vec2<f32>(local.x, local.z);
    let along = dot(flat, bend_dir);
    let across = flat - bend_dir * along;
    let bent_along = along * bend_cos + local.y * bend_sin;
    let bent_flat = across + bend_dir * bent_along;
    let bent = vec3<f32>(bent_flat.x, local.y * bend_cos - along * bend_sin, bent_flat.y);

    // ── the flutter ───────────────────────────────────────────────────────
    //
    // Along the vertex's own normal, so a leaf turns its face to the wind
    // rather than sliding sideways — the motion the eye reads as foliage at any
    // distance a whole tree is in frame. `foliage_clock.y` is zero on the bark
    // draw, which is what keeps a branch from breathing.
    //
    // It rides the same gust as the bend: leaves that keep beating through a
    // lull read as an insect swarm.
    let hz = FLUTTER_BASE_HZ + object.foliage_wind.y * FLUTTER_HZ_PER_SPEED;
    let beat = sin(FOLIAGE_TAU * (object.foliage_clock.x * hz + sway.y));
    return bent + normal * (object.foliage_clock.y * strength * beat);
}
