//! Roads (M23): turning a polygon of corners into a continuous drivable
//! ribbon, and the surface coordinates its markings are painted in.
//!
//! A [`Road`] is authored as a closed or open polygon whose vertices carry a
//! corner radius, exactly the way `make_car_track.py` learned to author a
//! circuit: a closed polygon returns to its own first vertex and its exterior
//! angles sum to one turn, so position *and* heading close without solving
//! anything, and nothing in the file has to carry a heading.
//!
//! What this module produces is one mesh — asphalt, shoulders and the
//! embankment skirt are the same triangle strip, so there is no edge between
//! them for a wheel to catch on, which is the failure the plate road had to be
//! papered over to avoid. The same mesh is what the physics engine builds its
//! trimesh collider from.
//!
//! Every vertex carries two surface coordinates in its UVs:
//!
//! - `u` (uv[1]): signed distance from the centerline **along the
//!   cross-section**, in metres, positive to the driver's right. Because it is
//!   cross-section arc length rather than a lateral offset,
//!   `|u| > width/2 + shoulder` is exactly "on the skirt" whatever the profile
//!   does.
//! - `v` (uv[0]): distance travelled along the centerline, in metres.
//!
//! `road.wgsl` paints every marking from those two numbers, which is what makes
//! a painted line follow the curve and the grade for free, and a dash the same
//! length in metres on a straight and through a hairpin.
//!
//! [`Road`]: crate::components::Road

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use glam::{Mat4, Quat, Vec2, Vec3};

use crate::components::Road;
use crate::mesh::MeshData;

/// The terrain a road follows, when it named one (M40).
///
/// Re-exported rather than redeclared: a meadow standing on the ground and a
/// road riding over it need the same pair — the height field and the transform
/// that turns its unit grid into world metres — and two structurally identical
/// types would be two places to fix when `world_height_at` changes what it
/// needs.
pub use crate::meadow::Ground;

/// Most kerbed corners one road may carry. The shader's span array is
/// fixed-size, so this is a real limit rather than a tuning knob — validation
/// rejects a road with more (`too_many_road_kerbs`) instead of silently
/// dropping the ones that did not fit.
pub const MAX_ROAD_KERBS: usize = 32;

/// The widest a sharp (`radius: 0`) vertex may turn before its mitre folds.
///
/// A cross-section at a sharp vertex bisects the angle and widens by
/// `1 / cos(turn / 2)`, which is the standard polyline-stroke join and is exact
/// — until the angle gets big enough that the widened section reaches back past
/// the neighbouring ones and the ribbon self-intersects. Past this a corner
/// wants a radius, and validation says so.
pub const MAX_SHARP_TURN_DEGREES: f32 = 60.0;

/// One kerbed stretch of road, handed to the shader as a span in `v`.
///
/// The CPU decides these because per-pixel code cannot: which corners are tight
/// enough to kerb, and which side of the road is the *inside* of the turn, are
/// both facts about the plan-view geometry.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KerbSpan {
    /// Metres along the centerline where the kerb starts.
    pub start: f32,
    /// Metres along the centerline where it ends.
    pub end: f32,
    /// `+1` for the driver's right, `-1` for the left — the inside of the turn.
    pub side: f32,
    /// Metres per red or white stripe, fitted so a whole number of them covers
    /// the span exactly.
    pub stripe: f32,
}

/// One sampled point on the finished centerline: where the road is, which way
/// it faces there, and how far along it that is.
///
/// This is what anything *furnishing* a road needs — guardrails, signs, start
/// lights, a trackside camera. It exists so that a generator placing them does
/// not have to re-implement the sampler and drift out of agreement with the
/// ribbon it is decorating; `engine road-centerline` publishes it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CenterPoint {
    /// World position of the road surface at the centerline.
    pub position: Vec3,
    /// Unit heading in the XZ plane. The driver's right is `(-z, x)`.
    pub direction: Vec2,
    /// Metres travelled along the centerline — the same `v` the markings are
    /// painted in.
    pub v: f32,
    /// Width of the asphalt here, in metres (M40). The road's own `width`
    /// unless a `RoadPoint` asked for another; the shoulder each side scales in
    /// the same proportion.
    pub width: f32,
    /// Roll of the cross-section here, in **radians**, positive raising the
    /// driver's right edge (M40). The file authors degrees; the conversion
    /// happens once, on the way in.
    ///
    /// Published for the same reason the heading is: anything placed *on* a
    /// banked corner — a car on the grid, a marshal's post, a junction reading
    /// this road's mouth — needs the roll, and a generator re-deriving it from
    /// the polygon is how two implementations of one road start disagreeing.
    pub bank: f32,
}

/// A road's generated geometry, plus everything about it the shader cannot
/// derive per pixel.
#[derive(Debug, Clone, PartialEq)]
pub struct RoadSurface {
    /// The ribbon. Held behind its own `Arc` because the renderer's geometry
    /// cache (M15) keys on that allocation's address: a stable `Arc` is the
    /// difference between uploading the road once and uploading it every frame.
    pub mesh: Arc<MeshData>,
    /// Total centerline length in metres — the range `v` spans.
    pub length: f32,
    /// Centre-line dash period in metres (dash + gap), fitted on a closed road
    /// so the pattern closes on itself instead of leaving a short dash at the
    /// seam. `0` when the road has no centre line to dash.
    pub dash_period: f32,
    /// Fraction of `dash_period` that is painted. `1` is a solid line.
    pub dash_duty: f32,
    /// Kerbed corners, in the order they are met. Never longer than
    /// [`MAX_ROAD_KERBS`] — validation refuses the scene first.
    pub kerbs: Vec<KerbSpan>,
    /// The sampled centerline, one entry per cross-section, closing on itself
    /// (the last entry repeats the first position at `v = length`).
    pub centerline: Vec<CenterPoint>,
}

thread_local! {
    /// One surface per distinct road geometry per thread, for the `MeshSource`
    /// contract's reason: the renderer keys uploaded vertex buffers on the
    /// mesh's address, and a viewer rebuilds its draw list every frame.
    static SURFACE_CACHE: RefCell<HashMap<RoadKey, Arc<RoadSurface>>> =
        RefCell::new(HashMap::new());
}

/// Everything about a [`Road`] that changes its geometry or its marking
/// *layout*, as exact bits.
///
/// Colours and paint widths are deliberately absent: they are read per pixel by
/// the shader and cannot move a vertex, so dragging a colour picker in the
/// editor must not regenerate the mesh and re-upload it every frame.
#[derive(Clone, PartialEq, Eq, Hash)]
struct RoadKey(Vec<u32>);

fn cache_key(road: &Road, model: Mat4, ground: Option<Ground<'_>>) -> RoadKey {
    let mut key = Vec::with_capacity(road.points.len() * 6 + 16);
    key.push(road.closed as u32);
    for point in &road.points {
        key.extend(point.position.to_array().iter().map(|f| f.to_bits()));
        key.push(point.radius.to_bits());
        // `None` has to key differently from any authored value, so the option
        // rides as a discriminant word beside the bits. `f32::to_bits` covers
        // the whole u32 range, so a sentinel would collide with a real width.
        key.push(point.width.is_some() as u32);
        key.push(point.width.unwrap_or(0.0).to_bits());
        key.push(point.bank.is_some() as u32);
        key.push(point.bank.unwrap_or(0.0).to_bits());
        key.push(point.pin_height as u32);
    }
    for value in [
        road.width,
        road.shoulder,
        road.skirt,
        road.segment_length,
        road.segment_angle,
        road.auto_bank,
        road.auto_bank_radius,
        road.follow_smoothing,
        road.follow_blend,
        road.markings.kerb_max_radius,
        road.markings.kerb_stripe,
        road.markings.center_width,
        road.markings.center_dash,
        road.markings.center_gap,
    ] {
        key.push(value.to_bits());
    }
    // The model matrix and the terrain enter the key **only** when the road
    // actually follows one (M40). A road with absolute heights is placed by a
    // transform the renderer applies, so its ribbon is the same vertices
    // wherever the entity sits — and keeping the key free of the matrix is what
    // lets an animated road transform reuse one upload, exactly as it did
    // before M40.
    if let (Some(name), Some(ground)) = (&road.follow_terrain, ground) {
        key.extend(name.bytes().map(u32::from));
        key.extend(model.to_cols_array().map(f32::to_bits));
        key.extend([
            ground.terrain.seed,
            ground.terrain.segments,
            ground.terrain.octaves,
            ground.terrain.height.to_bits(),
            ground.terrain.feature_scale.to_bits(),
            ground.terrain.warp.to_bits(),
            ground.terrain.persistence.to_bits(),
            ground.transform.position.x.to_bits(),
            ground.transform.position.y.to_bits(),
            ground.transform.position.z.to_bits(),
            ground.transform.scale.x.to_bits(),
            ground.transform.scale.y.to_bits(),
            ground.transform.scale.z.to_bits(),
        ]);
    }
    RoadKey(key)
}

/// How many distinct road geometries stay cached before the dead ones are
/// dropped. Far above any scene's road count; this exists for the case a clip
/// or a script animates a *geometry* field, which mints a new surface per
/// frame and would otherwise grow the cache without bound.
const SURFACE_CACHE_LIMIT: usize = 64;

/// The road's surface, generated once per distinct geometry.
///
/// `model` is the entity's flattened transform and `ground` the terrain it
/// named, both of which matter only when [`Road::follow_terrain`] is set — a
/// road with absolute heights ignores them and keys the cache without them, so
/// nothing about a pre-M40 road's caching changed. Pass `None` when the scene
/// has no such terrain; the road falls back to its authored heights, which is
/// the same thing validation reports as `road_terrain_invalid`.
pub fn surface(road: &Road, model: Mat4, ground: Option<Ground<'_>>) -> Arc<RoadSurface> {
    let key = cache_key(road, model, ground);
    SURFACE_CACHE.with(|cache| {
        if let Some(held) = cache.borrow().get(&key) {
            return Arc::clone(held);
        }
        let built = Arc::new(build(road, model, ground));
        let mut cache = cache.borrow_mut();
        if cache.len() >= SURFACE_CACHE_LIMIT {
            // Anything nothing else still holds is a surface no live road is
            // using — an animated width leaves one of these behind every frame.
            cache.retain(|_, held| Arc::strong_count(held) > 1);
        }
        cache.insert(key, Arc::clone(&built));
        built
    })
}

/// A sample that is on a straight rather than at an authored corner.
const NO_CORNER: usize = usize::MAX;

/// Hard ceiling on cross-sections in one road. Roughly 200 km of road at the
/// default sampling, and nine vertices apiece.
const MAX_ROAD_SAMPLES: usize = 100_000;

/// One sampled point on the centerline, in plan view.
#[derive(Debug, Clone, Copy)]
struct Sample {
    /// Plan-view position (x, z).
    position: Vec2,
    /// Unit heading in the XZ plane. At a sharp vertex this is the bisector.
    direction: Vec2,
    /// Distance travelled in plan view, metres.
    plan_distance: f32,
    /// Cross-section widening at a mitre join: `1 / cos(turn / 2)`, and `1`
    /// everywhere else.
    mitre: f32,
    /// Which authored point this sample belongs to — [`NO_CORNER`] on a
    /// straight, which belongs to neither of the corners it runs between.
    corner: usize,
    /// Whether it is on that corner's arc, as opposed to being its mitred
    /// sharp vertex.
    on_arc: bool,
}

/// One authored corner, rounded.
#[derive(Debug, Clone, Copy)]
struct Fillet {
    /// Where the incoming straight ends and the arc begins.
    entry: Vec2,
    /// Where the arc ends and the outgoing straight begins.
    exit: Vec2,
    center: Vec2,
    /// Unit direction of the incoming edge.
    incoming: Vec2,
    /// Turn angle in degrees, unsigned.
    turn: f32,
    /// `+1` turns right, `-1` left.
    sign: f32,
    radius: f32,
}

/// The direction to the driver's right of `d`, in the XZ plane.
///
/// One function so the geometry, the kerb sides and the `u` coordinate cannot
/// disagree about which side is which: with the engine's forward convention
/// (an entity faces its local −Z), a road heading −Z has its right at +X.
fn right_of(d: Vec2) -> Vec2 {
    Vec2::new(-d.y, d.x)
}

/// Rotate `d` toward the driver's right by `radians` (negative turns left).
fn turn_right(d: Vec2, radians: f32) -> Vec2 {
    let (sin, cos) = radians.sin_cos();
    d * cos + right_of(d) * sin
}

fn plan_of(position: Vec3) -> Vec2 {
    Vec2::new(position.x, position.z)
}

/// Round one polygon corner, or leave it sharp when its radius is zero.
fn fillet(previous: Vec3, corner: Vec3, following: Vec3, radius: f32) -> Fillet {
    let here = plan_of(corner);
    let incoming = (here - plan_of(previous)).normalize_or(Vec2::NEG_Y);
    let outgoing = (plan_of(following) - here).normalize_or(incoming);

    let cross = incoming.x * outgoing.y - incoming.y * outgoing.x;
    let dot = incoming.dot(outgoing).clamp(-1.0, 1.0);
    let turn = dot.acos().to_degrees();
    let sign = if cross >= 0.0 { 1.0 } else { -1.0 };

    // The fillet is the circle of this radius tucked into the wedge between the
    // two edges; it touches each edge a tangent length back from the vertex,
    // which is the room the straights have to leave for it.
    //
    // The turn is clamped before the tangent: a road that doubles back on
    // itself turns 180°, `tan(90°)` is 1.6e16, and the sampler asked to cut
    // that many metres into segments does not return this century. Such a road
    // is rejected by `geometry_problems` — the clamp is only what makes the
    // rejection reachable.
    let tangent = radius * (turn.min(179.0).to_radians() / 2.0).tan();
    let entry = here - incoming * tangent;
    let inward = right_of(incoming) * sign;

    Fillet {
        entry,
        exit: here + outgoing * tangent,
        center: entry + inward * radius,
        incoming,
        turn,
        sign,
        radius,
    }
}

/// Every corner of the polygon, rounded. An open road's endpoints have no
/// wedge to tuck a circle into, so they are sharp by construction.
fn fillets(road: &Road) -> Vec<Fillet> {
    let count = road.points.len();
    (0..count)
        .map(|i| {
            let point = &road.points[i];
            let endpoint = !road.closed && (i == 0 || i + 1 == count);
            let previous = road.points[(i + count - 1) % count].position;
            let following = road.points[(i + 1) % count].position;
            let radius = if endpoint { 0.0 } else { point.radius.max(0.0) };
            let mut fillet = fillet(previous, point.position, following, radius);
            if endpoint {
                // An endpoint's direction is whichever edge it actually has.
                let direction = if i == 0 {
                    (plan_of(road.points[1].position) - plan_of(point.position))
                        .normalize_or(Vec2::NEG_Y)
                } else {
                    (plan_of(point.position) - plan_of(road.points[count - 2].position))
                        .normalize_or(Vec2::NEG_Y)
                };
                fillet.entry = plan_of(point.position);
                fillet.exit = fillet.entry;
                fillet.center = fillet.entry;
                fillet.incoming = direction;
                fillet.turn = 0.0;
                fillet.sign = 1.0;
                fillet.radius = 0.0;
            }
            fillet
        })
        .collect()
}

/// Walk the rounded polygon: arc, straight, arc, straight, closing on itself.
///
/// It begins at the **first point's** corner, not at the straight leading into
/// it, so `v = 0` is where the author's first point is. That is what makes a
/// start line placeable: give the point a radius of 0 and the road begins
/// exactly there.
fn walk(road: &Road, fillets: &[Fillet]) -> Vec<Sample> {
    let count = road.points.len();
    let step = road.segment_length.max(0.05);
    let arc_step = road.segment_angle.max(0.25);

    let mut samples: Vec<Sample> = Vec::new();
    let mut push = |position: Vec2, direction: Vec2, mitre: f32, corner: usize, on_arc: bool| {
        // A backstop, not a policy: validation refuses every road that could
        // reach this (a corner that does not fit, a fold), and the schema
        // floors `segment_length` and `segment_angle`. What it buys is that a
        // road which slips through cannot hang the engine.
        if samples.len() >= MAX_ROAD_SAMPLES {
            return;
        }
        let plan_distance = match samples.last() {
            Some(previous) => previous.plan_distance + previous.position.distance(position),
            None => 0.0,
        };
        samples.push(Sample {
            position,
            direction,
            plan_distance,
            mitre,
            corner,
            on_arc,
        });
    };

    for i in 0..count {
        let here = &fillets[i];

        if here.turn > 1e-3 && here.radius > 1e-4 {
            // The arc through the corner itself.
            let sweep = here.turn.to_radians() * here.sign;
            let steps = (here.turn / arc_step).ceil().max(1.0) as usize;
            for k in 0..steps {
                let direction = turn_right(here.incoming, sweep * (k as f32 / steps as f32));
                // Every point on the arc is one radius from the centre, square
                // to the heading there.
                let position = here.center - right_of(direction) * here.radius * here.sign;
                push(position, direction, 1.0, i, true);
            }
        } else {
            // A sharp vertex: one cross-section on the bisector, widened so the
            // mitred join covers the corner exactly.
            let outgoing = turn_right(here.incoming, here.turn.to_radians() * here.sign);
            let direction = (here.incoming + outgoing).normalize_or(here.incoming);
            let mitre = 1.0 / (here.turn.to_radians() / 2.0).cos().max(0.2);
            push(here.entry, direction, mitre, i, false);
        }

        // The straight running out of this corner, to the next corner's entry.
        // An open road's last point has no edge in front of it.
        let next = (i + 1) % count;
        if road.closed || next != 0 {
            let span = fillets[next].entry - here.exit;
            let length = span.length();
            if length > 1e-4 {
                let direction = span / length;
                let steps = (length / step).ceil().max(1.0) as usize;
                for k in 0..steps {
                    push(
                        here.exit + span * (k as f32 / steps as f32),
                        direction,
                        1.0,
                        NO_CORNER,
                        false,
                    );
                }
            }
        }
    }

    samples
}

/// Where each authored point lands on the walked centerline, as plan distance,
/// sorted along the road.
///
/// A corner's mark is the middle of its arc, so the quantity being profiled is
/// constant along a straight and turns over through the corner. Height, width
/// and bank all hang off these same marks (M40) — three profiles over one set
/// of knots, rather than three answers to "where is point 4".
fn corner_marks(road: &Road, samples: &[Sample]) -> Vec<(f32, usize)> {
    let count = road.points.len();
    let mut marks: Vec<(f32, usize)> = Vec::with_capacity(count);
    for i in 0..count {
        let on_corner: Vec<usize> = samples
            .iter()
            .enumerate()
            .filter(|(_, s)| s.corner == i)
            .map(|(k, _)| k)
            .collect();
        if let Some(&middle) = on_corner.get(on_corner.len() / 2) {
            marks.push((samples[middle].plan_distance, i));
        }
    }
    marks.sort_by(|a, b| a.0.total_cmp(&b.0));
    marks
}

/// A quantity interpolated along the centerline through the authored points.
///
/// Monotone cubic (Fritsch–Carlson), not linear and not Catmull-Rom. Linear
/// ramps put a discontinuity in the *grade* at every corner, which the car
/// feels as a bump exactly where it is loaded up; plain Catmull-Rom smooths
/// that but overshoots, so a road authored to reach 5 m crests at 5.4 and the
/// file stops predicting the scene. Monotone cubic is smooth and never leaves
/// the authored range.
///
/// M23 had this inline in `heights`. M40 needs the same curve for the width and
/// the bank, and for the same reason each time — a linear ramp in width creases
/// the road at every corner, and a linear ramp in bank steps the roll rate
/// where the car is loaded up. **The extraction moved no expression**: Rust does
/// not contract float arithmetic into FMA without an explicit `mul_add`, so
/// unlike this repo's WGSL splices, code motion here is exact by construction.
struct Profile {
    /// `(plan distance, value)`, sorted, with a closed road's wrap appended.
    knots: Vec<(f32, f32)>,
    tangents: Vec<f32>,
    closed: bool,
    total: f32,
}

impl Profile {
    /// `marks` must be sorted by distance and non-empty.
    fn new(marks: Vec<(f32, f32)>, closed: bool, total: f32) -> Self {
        // A closed road wraps: the segment after the last mark runs to the first
        // one again, one lap further along. An open road just ends.
        let mut knots = marks.clone();
        if closed {
            knots.push((marks[0].0 + total, marks[0].1));
        }
        if knots.len() < 2 {
            return Self {
                knots,
                tangents: Vec::new(),
                closed,
                total,
            };
        }

        let secants: Vec<f32> = knots
            .windows(2)
            .map(|w| {
                let run = w[1].0 - w[0].0;
                if run.abs() < 1e-6 {
                    0.0
                } else {
                    (w[1].1 - w[0].1) / run
                }
            })
            .collect();

        // Fritsch–Carlson tangents: the average of the neighbouring secants,
        // zeroed at a turning point and limited to three times the smaller
        // neighbour, which is what stops the curve leaving the data's range.
        let last = knots.len() - 1;
        let tangents: Vec<f32> = (0..knots.len())
            .map(|k| {
                let before = if k == 0 {
                    if closed {
                        secants[secants.len() - 1]
                    } else {
                        secants[0]
                    }
                } else {
                    secants[k - 1]
                };
                let after = if k == last {
                    if closed {
                        secants[0]
                    } else {
                        secants[last - 1]
                    }
                } else {
                    secants[k]
                };
                if before * after <= 0.0 {
                    0.0
                } else {
                    let average = (before + after) / 2.0;
                    let limit = 3.0 * before.abs().min(after.abs());
                    average.clamp(-limit, limit)
                }
            })
            .collect();

        Self {
            knots,
            tangents,
            closed,
            total,
        }
    }

    fn evaluate(&self, distance: f32) -> f32 {
        if self.knots.len() < 2 {
            return self.knots[0].1;
        }
        let last = self.knots.len() - 1;
        let first_mark = self.knots[0].0;

        // Bring the sample onto the knot range. A closed road's marks cover one
        // lap starting at the first corner's, so anything before it belongs to
        // the wrapped final segment.
        let mut d = distance;
        if self.closed && d < first_mark {
            d += self.total;
        }
        let k = self
            .knots
            .windows(2)
            .position(|w| d >= w[0].0 && d <= w[1].0)
            .unwrap_or(if d < first_mark { 0 } else { last - 1 });

        let (s0, h0) = self.knots[k];
        let (s1, h1) = self.knots[k + 1];
        let run = s1 - s0;
        if run.abs() < 1e-6 {
            return h0;
        }
        let t = ((d - s0) / run).clamp(0.0, 1.0);
        let (m0, m1) = (self.tangents[k] * run, self.tangents[k + 1] * run);
        let t2 = t * t;
        let t3 = t2 * t;
        (2.0 * t3 - 3.0 * t2 + 1.0) * h0
            + (t3 - 2.0 * t2 + t) * m0
            + (-2.0 * t3 + 3.0 * t2) * h1
            + (t3 - t2) * m1
    }
}

/// Interpolate the authored heights along the walked centerline.
fn heights(road: &Road, samples: &[Sample], total: f32, marks: &[(f32, usize)]) -> Vec<f32> {
    if samples.is_empty() {
        return Vec::new();
    }
    if marks.is_empty() {
        let flat = road.points.first().map_or(0.0, |p| p.position.y);
        return vec![flat; samples.len()];
    }
    let profile = Profile::new(
        marks
            .iter()
            .map(|&(d, i)| (d, road.points[i].position.y))
            .collect(),
        road.closed,
        total,
    );
    samples
        .iter()
        .map(|s| profile.evaluate(s.plan_distance))
        .collect()
}

/// The local asphalt width at every sample, as a **multiple** of the road's own
/// `width` (M40).
///
/// `None` when no point authored a width, which is the answer for every road
/// written before M40 — and the caller skips the multiply entirely rather than
/// scaling by a vector of exact ones, so a pre-M40 road reaches the vertex
/// arithmetic through the code it always did.
fn width_scales(
    road: &Road,
    samples: &[Sample],
    total: f32,
    marks: &[(f32, usize)],
) -> Option<Vec<f32>> {
    if !road.points.iter().any(|p| p.width.is_some()) {
        return None;
    }
    let nominal = if road.width.abs() < 1e-6 {
        return None;
    } else {
        road.width
    };
    if marks.is_empty() {
        return None;
    }
    let profile = Profile::new(
        marks
            .iter()
            .map(|&(d, i)| (d, road.points[i].width.unwrap_or(nominal)))
            .collect(),
        road.closed,
        total,
    );
    Some(
        samples
            .iter()
            .map(|s| profile.evaluate(s.plan_distance) / nominal)
            .collect(),
    )
}

/// The bank a corner takes from [`Road::auto_bank`], in degrees, signed so the
/// **outside** of the turn is raised.
///
/// `fillet.sign` is `+1` for a right-hand turn, whose outside is the driver's
/// left — and a positive bank raises the driver's right — so the sign is
/// negated. Getting this backwards builds a circuit that throws the car off at
/// every corner, which is exactly why the field exists instead of leaving the
/// sign to the file.
fn auto_bank_degrees(road: &Road, fillet: &Fillet, radius: f32) -> f32 {
    if road.auto_bank <= 0.0 || fillet.turn <= 1e-3 || radius <= 0.0 {
        return 0.0;
    }
    let reference = road.auto_bank_radius.max(1e-3);
    let magnitude = road.auto_bank * reference / radius.max(reference);
    -fillet.sign * magnitude
}

/// The roll of the cross-section at every sample, in **radians**.
///
/// `None` when nothing banks this road at all, so an unbanked road keeps M23's
/// horizontal cross-section frame untouched rather than rotating it by an angle
/// that happens to be zero.
fn bank_angles(
    road: &Road,
    fillets: &[Fillet],
    samples: &[Sample],
    total: f32,
    marks: &[(f32, usize)],
) -> Option<Vec<f32>> {
    let explicit = road.points.iter().any(|p| p.bank.is_some());
    if !explicit && road.auto_bank <= 0.0 {
        return None;
    }
    if marks.is_empty() {
        return None;
    }
    let profile = Profile::new(
        marks
            .iter()
            .map(|&(d, i)| {
                let point = &road.points[i];
                let degrees = point
                    .bank
                    .unwrap_or_else(|| auto_bank_degrees(road, &fillets[i], point.radius));
                (d, degrees.to_radians())
            })
            .collect(),
        road.closed,
        total,
    );
    Some(
        samples
            .iter()
            .map(|s| profile.evaluate(s.plan_distance))
            .collect(),
    )
}

/// Longest the smoothing filter walks either side of a sample, in samples.
///
/// A backstop rather than a policy: at the default 2 m sampling this is 512 m
/// of window, far past any `follow_smoothing` worth authoring, and it stops a
/// road cut into 100,000 samples with a 5 km smoothing radius from turning a
/// linear filter into a quadratic one.
const MAX_SMOOTHING_TAPS: usize = 256;

/// Arc distance between two points on the centerline, the short way round on a
/// closed road.
fn arc_gap(a: f32, b: f32, total: f32, closed: bool) -> f32 {
    let direct = (a - b).abs();
    if closed && total > 0.0 {
        direct.min(total - direct)
    } else {
        direct
    }
}

/// The height profile of a road that follows a terrain (M40).
///
/// Three layers, in order: the ground, smoothed along the road; the authored
/// `y` values as a clearance riding on top of it; and a local correction at
/// each pinned point.
fn followed_heights(
    road: &Road,
    samples: &[Sample],
    total: f32,
    marks: &[(f32, usize)],
    width_scales: Option<&Vec<f32>>,
    model: Mat4,
    ground: Ground<'_>,
) -> Vec<f32> {
    // The ribbon is built in the entity's local space and placed by `model`, so
    // the ground has to be asked in world space and the answer brought back.
    // A road whose transform rolls or pitches has no single answer here — local
    // `y` would no longer be world "up" — which is what `road_follow_rotated`
    // warns about; the arithmetic below is exact for the translation, uniform
    // scale and yaw a road is actually placed with.
    let lift = model.w_axis.y;
    let scale_y = {
        let s = model.y_axis.length();
        if s.abs() < 1e-6 {
            1.0
        } else {
            s
        }
    };

    // Sampled **across** the road, not only down its middle, and the highest of
    // the three wins.
    //
    // The cross-section is level, so a road whose centerline lies on the ground
    // has its uphill edge buried in the hillside — and this engine does not
    // carve the terrain, so buried means the ground pokes through the asphalt.
    // Riding the highest ground the road covers puts the downhill edge in the
    // air instead, which is the failure the `skirt` already exists to hide. It
    // matters more the wider the road is, which is why per-point width and this
    // arrived together.
    let edge = |i: usize| {
        let half = road.width.max(0.0) / 2.0 + road.shoulder.max(0.0);
        half * width_scales.map_or(1.0, |scales| scales[i]) * samples[i].mitre
    };
    let sampled: Vec<f32> = samples
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let across = right_of(s.direction) * edge(i);
            let highest = [Vec2::ZERO, across, -across]
                .into_iter()
                .map(|offset| {
                    let local = Vec3::new(s.position.x + offset.x, 0.0, s.position.y + offset.y);
                    let world = model.transform_point3(local);
                    crate::terrain::world_height_at(
                        ground.terrain,
                        ground.transform,
                        world.x,
                        world.z,
                    )
                })
                .fold(f32::MIN, f32::max);
            (highest - lift) / scale_y
        })
        .collect();

    // Smooth along the road. Terrain is noise; a road that reproduces it is
    // undrivable, and the width of this filter is the difference between a
    // road that lies on the ground and one that follows every rut in it.
    let radius = road.follow_smoothing.max(0.0);
    let smoothed: Vec<f32> = if radius <= 0.0 {
        sampled.clone()
    } else {
        let n = sampled.len();
        (0..n)
            .map(|i| {
                let here = samples[i].plan_distance;
                let mut sum = sampled[i];
                let mut count = 1.0f32;
                for step in 1..=MAX_SMOOTHING_TAPS {
                    let mut reached = false;
                    for direction in [-1isize, 1] {
                        let offset = direction * step as isize;
                        let k = if road.closed {
                            // The closing sample repeats the first, so the ring
                            // of *distinct* samples is one shorter.
                            let ring = n.saturating_sub(1).max(1);
                            (i as isize + offset).rem_euclid(ring as isize) as usize
                        } else {
                            let k = i as isize + offset;
                            if k < 0 || k as usize >= n {
                                continue;
                            }
                            k as usize
                        };
                        if arc_gap(samples[k].plan_distance, here, total, road.closed) > radius {
                            continue;
                        }
                        sum += sampled[k];
                        count += 1.0;
                        reached = true;
                    }
                    if !reached {
                        break;
                    }
                }
                sum / count
            })
            .collect()
    };

    // The authored clearance above that ground. Pinned points say an absolute
    // height instead, so they contribute no clearance knot — a road whose every
    // point is pinned simply follows the pins.
    let clearance: Vec<f32> = {
        let knots: Vec<(f32, f32)> = marks
            .iter()
            .filter(|&&(_, i)| !road.points[i].pin_height)
            .map(|&(d, i)| (d, road.points[i].position.y))
            .collect();
        if knots.is_empty() {
            vec![0.0; samples.len()]
        } else {
            let profile = Profile::new(knots, road.closed, total);
            samples
                .iter()
                .map(|s| profile.evaluate(s.plan_distance))
                .collect()
        }
    };

    let mut heights: Vec<f32> = smoothed
        .iter()
        .zip(&clearance)
        .map(|(&ground_here, &above)| ground_here + above)
        .collect();

    // Pins, as local corrections. Each one is the gap between the height the
    // author demanded and the height following the ground produced, faded out
    // over `follow_blend` metres either side — so the road reaches the pin
    // exactly and goes back to hugging the ground. With no pins this loop does
    // nothing at all.
    let blend = road.follow_blend.max(0.0);
    for &(distance, i) in marks {
        let point = &road.points[i];
        if !point.pin_height {
            continue;
        }
        // The height the pin has to correct is the followed profile *at* the
        // pin, which is the sample the mark came from.
        let at = samples
            .iter()
            .position(|s| s.plan_distance >= distance)
            .unwrap_or(0)
            .min(heights.len() - 1);
        let delta = point.position.y - heights[at];
        if blend <= 0.0 {
            heights[at] += delta;
            continue;
        }
        for (k, height) in heights.iter_mut().enumerate() {
            let t = (arc_gap(samples[k].plan_distance, distance, total, road.closed) / blend)
                .clamp(0.0, 1.0);
            // Smoothstep, so the correction leaves and rejoins the ground
            // profile with no kink in the grade — the same thing the monotone
            // cubic is protecting everywhere else.
            *height += delta * (1.0 - t * t * (3.0 - 2.0 * t));
        }
    }

    heights
}

/// Which corners are tight enough to kerb, as spans in `v`.
fn kerb_spans(road: &Road, samples: &[Sample], v: &[f32], total: f32) -> Vec<KerbSpan> {
    let limit = road.markings.kerb_max_radius;
    if limit <= 0.0 || road.markings.kerb_width <= 0.0 {
        return Vec::new();
    }

    let fillets = fillets(road);
    let mut spans = Vec::new();
    for (i, point) in road.points.iter().enumerate() {
        if point.radius <= 0.0 || point.radius > limit || fillets[i].turn <= 1e-3 {
            continue;
        }
        let on_arc: Vec<usize> = samples
            .iter()
            .enumerate()
            .filter(|(_, s)| s.corner == i && s.on_arc)
            .map(|(k, _)| k)
            .collect();
        let (Some(&first), Some(&last)) = (on_arc.first(), on_arc.last()) else {
            continue;
        };
        // The arc's samples run from its entry to one step short of its exit;
        // carry the span on to the next sample so the kerb reaches the end of
        // the corner rather than stopping a segment early.
        let start = v[first];
        let end = v.get(last + 1).copied().unwrap_or(total);
        let length = end - start;
        if length <= 0.0 {
            continue;
        }
        // Fit a whole number of stripes into the corner, so a kerb starts and
        // ends on a stripe boundary instead of on a sliver.
        let wanted = road.markings.kerb_stripe.max(0.05);
        let count = (length / wanted).round().max(1.0);
        spans.push(KerbSpan {
            start,
            end,
            side: fillets[i].sign,
            stripe: length / count,
        });
        if spans.len() == MAX_ROAD_KERBS {
            // Validation rejects a road that needs more than this, so reaching
            // the cap here means the scene never loaded.
            break;
        }
    }
    spans
}

fn build(road: &Road, model: Mat4, ground: Option<Ground<'_>>) -> RoadSurface {
    let fillets = fillets(road);
    let mut samples = walk(road, &fillets);

    if samples.is_empty() {
        return RoadSurface {
            mesh: Arc::new(MeshData::default()),
            length: 0.0,
            dash_period: 0.0,
            dash_duty: 1.0,
            kerbs: Vec::new(),
            centerline: Vec::new(),
        };
    }

    // Closing the ring: a closed road repeats its first cross-section at the
    // far end, so the last quad is stitched to the first without the seam
    // vertices being welded (which would make `v` jump from `total` to 0 inside
    // one quad and tear every dash across it).
    let plan_total = if road.closed {
        let closing = samples[0];
        let last = *samples.last().expect("non-empty");
        let total = last.plan_distance + last.position.distance(closing.position);
        samples.push(Sample {
            plan_distance: total,
            ..closing
        });
        total
    } else {
        samples.last().expect("non-empty").plan_distance
    };

    // One set of marks, three profiles over it (M40). Width comes first: a
    // road following a terrain samples the ground across its own cross-section,
    // so it has to know how wide it is there before it knows how high it is.
    let marks = corner_marks(road, &samples);
    let width_scales = width_scales(road, &samples, plan_total, &marks);
    let heights = match (&road.follow_terrain, ground) {
        (Some(_), Some(ground)) => followed_heights(
            road,
            &samples,
            plan_total,
            &marks,
            width_scales.as_ref(),
            model,
            ground,
        ),
        // A road naming a terrain the scene does not have falls back to its
        // authored heights rather than to zero, and validation says so
        // (`road_terrain_invalid`) — the render path does not re-litigate what
        // validation already reported, which is `meadow_terrain_invalid`'s rule.
        _ => heights(road, &samples, plan_total, &marks),
    };
    let banks = bank_angles(road, &fillets, &samples, plan_total, &marks);

    // The centerline in 3D, and `v` as its true arc length: markings are
    // measured along the road as driven, not along its shadow on the ground.
    let centers: Vec<Vec3> = samples
        .iter()
        .zip(&heights)
        .map(|(s, &y)| Vec3::new(s.position.x, y, s.position.y))
        .collect();
    let mut v = Vec::with_capacity(centers.len());
    let mut travelled = 0.0;
    for (i, center) in centers.iter().enumerate() {
        if i > 0 {
            travelled += centers[i - 1].distance(*center);
        }
        v.push(travelled);
    }
    let total = travelled;

    let half = road.width.max(0.0) / 2.0;
    let shoulder = road.shoulder.max(0.0);
    let skirt = road.skirt.max(0.0);
    let edge = half + shoulder;

    // Five surface columns and four skirt ones per cross-section. The surface
    // is flat across, so the middle columns buy nothing geometrically — they
    // keep the shading normal interpolating smoothly across a wide road, and
    // they are where a future banked or crowned profile would bend.
    const SURFACE_COLUMNS: usize = 5;
    let columns = [-edge, -half, 0.0, half, edge];

    let mut mesh = MeshData {
        positions: Vec::with_capacity(centers.len() * 9),
        normals: Vec::with_capacity(centers.len() * 9),
        uvs: Vec::with_capacity(centers.len() * 9),
        indices: Vec::with_capacity(centers.len() * 8 * 6),
        ..MeshData::default()
    };

    // Per-sample frame: the horizontal cross-section direction, and the surface
    // normal averaged along the road so the ribbon shades as one surface.
    let rights: Vec<Vec3> = samples
        .iter()
        .map(|s| {
            let r = right_of(s.direction);
            Vec3::new(r.x, 0.0, r.y)
        })
        .collect();
    // Banking rolls that frame about the local heading (M40). The rotation is
    // rigid, so `u` — cross-section arc length — is untouched and every marking
    // stays exactly where it was; the normals fall out of the existing
    // `right × along` below with no special case. An unbanked road skips this
    // entirely rather than rotating by an angle that happens to be zero.
    let rights: Vec<Vec3> = match &banks {
        None => rights,
        Some(banks) => rights
            .iter()
            .enumerate()
            .map(|(i, right)| {
                let bank = banks[i];
                if bank == 0.0 {
                    return *right;
                }
                let ahead = centers[(i + 1).min(centers.len() - 1)];
                let behind = centers[i.saturating_sub(1)];
                let heading = samples[i].direction;
                let along = (ahead - behind).normalize_or(Vec3::new(heading.x, 0.0, heading.y));
                // `along × right` is −Y on a level road, so rotating `right`
                // about the heading by a positive angle would *lower* the
                // driver's right edge. The angle is negated so a positive bank
                // raises it, which is what the field says it does.
                Quat::from_axis_angle(along, -bank) * *right
            })
            .collect(),
    };
    let segment_normals: Vec<Vec3> = (0..centers.len().saturating_sub(1))
        .map(|i| {
            let along = (centers[i + 1] - centers[i]).normalize_or(Vec3::NEG_Z);
            rights[i].cross(along).normalize_or(Vec3::Y)
        })
        .collect();
    let mut normals: Vec<Vec3> = (0..centers.len())
        .map(|i| {
            let before = if i > 0 {
                segment_normals.get(i - 1).copied()
            } else if road.closed {
                segment_normals.last().copied()
            } else {
                None
            };
            let after = segment_normals.get(i).copied();
            match (before, after) {
                (Some(a), Some(b)) => (a + b).normalize_or(Vec3::Y),
                (Some(a), None) => a,
                (None, Some(b)) => b,
                (None, None) => Vec3::Y,
            }
        })
        .collect();
    if road.closed {
        // The closing cross-section is the first one repeated, so it has to
        // shade like the first one. Left to the rule above it would average
        // only the segment *into* it and leave a lighting crease across the
        // road at the seam — the one place a closed road must not have one.
        let first = normals[0];
        if let Some(last) = normals.last_mut() {
            *last = first;
        }
    }

    for (i, center) in centers.iter().enumerate() {
        let right = rights[i];
        let normal = normals[i];
        // Per-point width scales the whole cross-section, exactly the way a
        // mitre does and folded into the same factor (M40): the *positions*
        // widen while `uv[1]` keeps the nominal column, so the shader's
        // `|u| > half + shoulder` still finds the skirt with nothing extra
        // uploaded and no third vertex channel.
        let mitre = match &width_scales {
            Some(scales) => samples[i].mitre * scales[i],
            None => samples[i].mitre,
        };

        // The drivable surface.
        for column in columns {
            let offset = column * mitre;
            mesh.positions.push((*center + right * offset).to_array());
            mesh.normals.push(normal.to_array());
            mesh.uvs.push([v[i], column]);
        }

        // The skirt, its own vertices so the crease at the road's edge stays a
        // crease instead of being averaged into a soft roll.
        for side in [-1.0f32, 1.0] {
            let top = *center + right * (edge * mitre * side);
            let outward = right * side;
            mesh.positions.push(top.to_array());
            mesh.normals.push(outward.to_array());
            mesh.uvs.push([v[i], edge * side]);

            mesh.positions.push((top - Vec3::Y * skirt).to_array());
            mesh.normals.push(outward.to_array());
            // `u` keeps running down the cross-section, so the shader knows the
            // skirt without a second flag.
            mesh.uvs.push([v[i], (edge + skirt) * side]);
        }
    }

    let stride = SURFACE_COLUMNS + 4;
    for i in 0..centers.len() - 1 {
        let here = (i * stride) as u32;
        let next = ((i + 1) * stride) as u32;

        // Surface quads, wound counter-clockwise seen from above so the ribbon
        // survives the engine's back-face culling.
        for k in 0..SURFACE_COLUMNS as u32 - 1 {
            let (a, b, c, d) = (here + k, next + k, next + k + 1, here + k + 1);
            mesh.indices.extend_from_slice(&[a, c, b, a, d, c]);
        }

        // Skirts, facing outward. The left one winds one way and the right one
        // the other, for the same reason a box's opposite faces do.
        let left = (here + 5, here + 6, next + 5, next + 6);
        mesh.indices
            .extend_from_slice(&[left.0, left.2, left.1, left.1, left.2, left.3]);
        let right = (here + 7, here + 8, next + 7, next + 8);
        mesh.indices
            .extend_from_slice(&[right.0, right.1, right.2, right.1, right.3, right.2]);
    }

    // Fit the dash pattern to the road. On a closed loop the period is snapped
    // so a whole number of dashes covers the lap, which is what stops the
    // pattern from meeting itself half a dash out at the start line.
    let dash = road.markings.center_dash.max(0.0);
    let gap = road.markings.center_gap.max(0.0);
    let (dash_period, dash_duty) = if road.markings.center_width <= 0.0 || dash <= 0.0 {
        (0.0, 1.0)
    } else {
        let wanted = dash + gap;
        let period = if road.closed && wanted > 0.0 && total > 0.0 {
            total / (total / wanted).round().max(1.0)
        } else {
            wanted
        };
        (period, (dash / wanted).clamp(0.0, 1.0))
    };

    RoadSurface {
        mesh: Arc::new(mesh),
        length: total,
        dash_period,
        dash_duty,
        kerbs: kerb_spans(road, &samples, &v, total),
        centerline: centers
            .iter()
            .zip(&samples)
            .zip(&v)
            .enumerate()
            .map(|(i, ((position, sample), v))| CenterPoint {
                position: *position,
                direction: sample.direction,
                v: *v,
                width: road.width * width_scales.as_ref().map_or(1.0, |scales| scales[i]),
                bank: banks.as_ref().map_or(0.0, |banks| banks[i]),
            })
            .collect(),
    }
}

/// How many kerb spans a road *wants*, before the cap — what validation
/// reports on.
pub fn kerb_span_count(road: &Road) -> usize {
    let limit = road.markings.kerb_max_radius;
    if limit <= 0.0 || road.markings.kerb_width <= 0.0 {
        return 0;
    }
    let fillets = fillets(road);
    road.points
        .iter()
        .enumerate()
        .filter(|(i, p)| p.radius > 0.0 && p.radius <= limit && fillets[*i].turn > 1e-3)
        .count()
}

/// What is wrong with a corner the polygon itself cannot check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoadProblem {
    /// Two arcs need more of the edge between them than it has, so they would
    /// overlap and the ribbon would cross itself.
    CornerDoesNotFit,
    /// A sharp vertex turns far enough that its mitre folds back through the
    /// road.
    CornerNeedsRadius,
}

/// Corners whose fillet does not fit between its neighbours, and sharp vertices
/// whose mitre would fold — everything the polygon itself cannot guarantee.
///
/// Returned as `(index, kind, message)` so validation can point at the
/// offending point and say the arithmetic out loud, rather than rendering a
/// road that crosses itself and leaving the agent to find out from a
/// screenshot. This is the `make_car_track.py` move, moved into the engine.
pub fn geometry_problems(road: &Road) -> Vec<(usize, RoadProblem, String)> {
    let count = road.points.len();
    if count < 2 {
        return Vec::new();
    }
    let fillets = fillets(road);
    let mut problems = Vec::new();

    let edges = if road.closed { count } else { count - 1 };
    for i in 0..edges {
        let next = (i + 1) % count;
        let edge = plan_of(road.points[i].position).distance(plan_of(road.points[next].position));
        let needed = fillets[i].tangent_length() + fillets[next].tangent_length();
        if needed > edge {
            problems.push((
                i,
                RoadProblem::CornerDoesNotFit,
                format!(
                    "the {edge:.2} m edge from point {i} to point {next} cannot hold \
                     {needed:.2} m of corner radius ({:.2} m out of point {i} plus \
                     {:.2} m into point {next}); the two arcs would overlap and the \
                     road would cross itself — shorten a radius or move a point",
                    fillets[i].tangent_length(),
                    fillets[next].tangent_length(),
                ),
            ));
        }
    }

    for (i, point) in road.points.iter().enumerate() {
        let sharp = point.radius <= 0.0;
        let endpoint = !road.closed && (i == 0 || i + 1 == count);
        if sharp && !endpoint && fillets[i].turn > MAX_SHARP_TURN_DEGREES {
            problems.push((
                i,
                RoadProblem::CornerNeedsRadius,
                format!(
                    "point {i} turns {:.1}° with no radius; a sharp vertex is mitred, \
                     and past {MAX_SHARP_TURN_DEGREES:.0}° the mitre folds back through \
                     the road — give this corner a radius",
                    fillets[i].turn
                ),
            ));
        }
    }

    problems
}

impl Fillet {
    /// How far back from the vertex the arc touches each edge.
    fn tangent_length(&self) -> f32 {
        self.radius * (self.turn.min(179.0).to_radians() / 2.0).tan()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::{RoadMarkings, RoadPoint};

    fn straight() -> Road {
        Road {
            points: vec![
                RoadPoint {
                    position: Vec3::new(0.0, 0.0, 0.0),
                    ..RoadPoint::default()
                },
                RoadPoint {
                    position: Vec3::new(0.0, 0.0, -20.0),
                    ..RoadPoint::default()
                },
            ],
            closed: false,
            width: 8.0,
            shoulder: 2.0,
            skirt: 1.0,
            segment_length: 5.0,
            ..Road::default()
        }
    }

    fn square() -> Road {
        Road {
            points: vec![
                RoadPoint {
                    position: Vec3::new(-30.0, 0.0, -30.0),
                    radius: 10.0,
                    ..RoadPoint::default()
                },
                RoadPoint {
                    position: Vec3::new(30.0, 0.0, -30.0),
                    radius: 10.0,
                    ..RoadPoint::default()
                },
                RoadPoint {
                    position: Vec3::new(30.0, 0.0, 30.0),
                    radius: 10.0,
                    ..RoadPoint::default()
                },
                RoadPoint {
                    position: Vec3::new(-30.0, 0.0, 30.0),
                    radius: 10.0,
                    ..RoadPoint::default()
                },
            ],
            closed: true,
            width: 7.0,
            shoulder: 1.5,
            ..Road::default()
        }
    }

    #[test]
    fn the_drivable_surface_faces_up() {
        let built = build(&square(), Mat4::IDENTITY, None);
        let mesh = &built.mesh;
        // The first four quads of every cross-section are the road surface;
        // check every triangle whose vertices are all surface columns.
        let mut checked = 0;
        for triangle in mesh.indices.chunks_exact(3) {
            let column = |i: u32| (i as usize) % 9;
            if triangle.iter().any(|&i| column(i) >= 5) {
                continue;
            }
            let p = |i: u32| Vec3::from_array(mesh.positions[i as usize]);
            let (a, b, c) = (p(triangle[0]), p(triangle[1]), p(triangle[2]));
            let normal = (b - a).cross(c - a).normalize();
            assert!(
                normal.y > 0.9,
                "surface triangle {triangle:?} winds downward: {normal}"
            );
            checked += 1;
        }
        assert!(checked > 0, "no surface triangles were checked");
    }

    #[test]
    fn the_skirt_faces_outward() {
        let built = build(&square(), Mat4::IDENTITY, None);
        let mesh = &built.mesh;
        for triangle in mesh.indices.chunks_exact(3) {
            let column = |i: u32| (i as usize) % 9;
            if triangle.iter().any(|&i| column(i) < 5) {
                continue;
            }
            let p = |i: u32| Vec3::from_array(mesh.positions[i as usize]);
            let (a, b, c) = (p(triangle[0]), p(triangle[1]), p(triangle[2]));
            let normal = (b - a).cross(c - a).normalize();
            // Outward is away from the centerline, which for a loop running
            // clockwise means the *inner* skirt faces toward the origin — so
            // the check is against the stored normal, which is the cross
            // section's outward direction. Winding disagreeing with it is what
            // makes back-face culling show the inside of the embankment.
            let outward = Vec3::from_array(mesh.normals[triangle[0] as usize]);
            assert!(
                normal.dot(outward) > 0.5,
                "skirt triangle {triangle:?} faces inward: {normal} against {outward}"
            );
        }
    }

    #[test]
    fn u_is_cross_section_arc_length() {
        let road = straight();
        let built = build(&road, Mat4::IDENTITY, None);
        let edge = road.width / 2.0 + road.shoulder;
        // One cross-section: five surface columns then the two skirts.
        let uvs: Vec<f32> = built.mesh.uvs[..9].iter().map(|uv| uv[1]).collect();
        assert_eq!(
            uvs,
            vec![
                -edge,
                -road.width / 2.0,
                0.0,
                road.width / 2.0,
                edge,
                -edge,
                -(edge + road.skirt),
                edge,
                edge + road.skirt,
            ],
            "u runs along the cross-section in metres, so the skirt is simply \
             |u| past the shoulder"
        );
    }

    #[test]
    fn v_is_distance_along_the_road() {
        let built = build(&straight(), Mat4::IDENTITY, None);
        assert!((built.length - 20.0).abs() < 1e-3, "{}", built.length);
        let last = built.mesh.uvs.last().expect("vertices")[0];
        assert!((last - 20.0).abs() < 1e-3);
    }

    #[test]
    fn a_closed_road_closes() {
        let built = build(&square(), Mat4::IDENTITY, None);
        // Four 60 m edges with 10 m corners: the straights lose 2 × 10 m of
        // tangent each and the corners give back a quarter circle apiece.
        let expected = 4.0 * (60.0 - 20.0) + 2.0 * std::f32::consts::PI * 10.0;
        assert!(
            (built.length - expected).abs() < 0.5,
            "lap is {} m, expected about {expected} m",
            built.length
        );

        // The ring repeats its first cross-section at the far end rather than
        // welding it, so `v` never jumps inside a quad.
        let stride = 9;
        let first = Vec3::from_array(built.mesh.positions[0]);
        let last = Vec3::from_array(built.mesh.positions[built.mesh.positions.len() - stride]);
        assert!(
            first.distance(last) < 1e-3,
            "the ring should end where it started: {first} vs {last}"
        );
    }

    #[test]
    fn a_closed_ring_shades_across_its_seam() {
        // The closing cross-section is the first one repeated. If it does not
        // carry the first one's averaged normal, the quad that closes the ring
        // shades against a different normal on each side and the road wears a
        // lighting crease exactly where it should be seamless.
        let built = build(&square(), Mat4::IDENTITY, None);
        let stride = 9;
        let first = &built.mesh.normals[..stride];
        let last = &built.mesh.normals[built.mesh.normals.len() - stride..];
        assert_eq!(
            first, last,
            "the seam's two cross-sections must shade alike"
        );
    }

    #[test]
    fn heights_never_overshoot_the_authored_ones() {
        // The whole reason the profile is monotone cubic rather than
        // Catmull-Rom: a road authored to reach 6 m must not crest at 6.4.
        let mut road = square();
        for (point, height) in road.points.iter_mut().zip([0.0, 6.0, 0.0, 0.0]) {
            point.position.y = height;
        }
        let built = build(&road, Mat4::IDENTITY, None);
        let highest = built
            .mesh
            .positions
            .iter()
            .map(|p| p[1])
            .fold(f32::MIN, f32::max);
        assert!(
            highest <= 6.0 + 1e-3,
            "the profile overshot to {highest} m above an authored maximum of 6 m"
        );
    }

    #[test]
    fn the_grade_has_no_kinks() {
        // Linear ramps between corner heights break the grade at every corner.
        // Sample the centerline column and check the second difference stays
        // small — a kink shows up as a spike.
        let mut road = square();
        for (point, height) in road.points.iter_mut().zip([0.0, 4.0, 4.0, 0.0]) {
            point.position.y = height;
        }
        road.segment_length = 2.0;
        let built = build(&road, Mat4::IDENTITY, None);
        let centre: Vec<Vec3> = built
            .mesh
            .positions
            .chunks_exact(9)
            .map(|section| Vec3::from_array(section[2]))
            .collect();
        let grades: Vec<f32> = centre
            .windows(2)
            .map(|w| {
                let run = Vec2::new(w[1].x - w[0].x, w[1].z - w[0].z).length();
                if run < 1e-4 {
                    0.0
                } else {
                    (w[1].y - w[0].y) / run
                }
            })
            .collect();
        let worst = grades
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max);
        assert!(
            worst < 0.02,
            "the grade jumps by {worst} between neighbouring segments, which is a kink"
        );
    }

    #[test]
    fn kerbs_land_on_the_inside_of_the_turn() {
        let mut road = square();
        road.markings = RoadMarkings {
            kerb_max_radius: 12.0,
            ..RoadMarkings::default()
        };
        let built = build(&road, Mat4::IDENTITY, None);
        assert_eq!(built.kerbs.len(), 4, "every corner is tight enough");
        for kerb in &built.kerbs {
            // The square runs clockwise seen from above, so every corner turns
            // right and every kerb belongs on the driver's right.
            assert_eq!(kerb.side, 1.0);
            // Stripes divide the span exactly.
            let count = (kerb.end - kerb.start) / kerb.stripe;
            assert!(
                (count - count.round()).abs() < 1e-3,
                "{count} stripes is not a whole number"
            );
        }
    }

    #[test]
    fn a_closed_dash_pattern_closes() {
        let mut road = square();
        road.markings = RoadMarkings {
            center_width: 0.12,
            center_dash: 3.0,
            center_gap: 6.0,
            ..RoadMarkings::default()
        };
        let built = build(&road, Mat4::IDENTITY, None);
        let laps = built.length / built.dash_period;
        assert!(
            (laps - laps.round()).abs() < 1e-3,
            "a whole number of dashes has to fit the lap, got {laps}"
        );
        assert!((built.dash_duty - 1.0 / 3.0).abs() < 1e-3);
    }

    #[test]
    fn one_surface_per_geometry_is_shared() {
        // The renderer's geometry cache keys on the mesh's address.
        let road = square();
        assert!(Arc::ptr_eq(
            &surface(&road, Mat4::IDENTITY, None).mesh,
            &surface(&road, Mat4::IDENTITY, None).mesh
        ));

        // Colour is read per pixel and moves no vertex, so it must not mint a
        // new surface — an editor colour picker would otherwise re-upload the
        // whole road every frame it is dragged.
        let mut repainted = road.clone();
        repainted.color = Vec3::new(0.5, 0.1, 0.1);
        assert!(Arc::ptr_eq(
            &surface(&road, Mat4::IDENTITY, None).mesh,
            &surface(&repainted, Mat4::IDENTITY, None).mesh
        ));

        let mut wider = road.clone();
        wider.width = 9.0;
        assert!(!Arc::ptr_eq(
            &surface(&road, Mat4::IDENTITY, None).mesh,
            &surface(&wider, Mat4::IDENTITY, None).mesh
        ));
    }

    #[test]
    fn a_corner_too_big_for_its_edges_is_reported() {
        // 60 m edges, so a 60 m radius eats the whole edge as tangent and still
        // has to share it with the next corner's 10 m.
        let mut road = square();
        road.points[0].radius = 60.0;
        let problems = geometry_problems(&road);
        assert!(
            problems
                .iter()
                .any(|(i, kind, _)| (*i == 0 || *i == 3) && *kind == RoadProblem::CornerDoesNotFit),
            "the 40 m radius does not fit a 60 m edge: {problems:?}"
        );
        assert!(geometry_problems(&square()).is_empty());
    }

    #[test]
    fn a_folded_mitre_is_reported() {
        let mut road = square();
        road.points[1].radius = 0.0;
        let problems = geometry_problems(&road);
        assert!(
            problems
                .iter()
                .any(|(i, kind, _)| *i == 1 && *kind == RoadProblem::CornerNeedsRadius),
            "a 90° sharp vertex should be refused: {problems:?}"
        );
    }

    #[test]
    fn the_road_starts_at_the_first_point() {
        // What makes `markings.start_line` placeable: `v = 0` is the author's
        // first point, so a start line is authored by putting a radius-0 point
        // where it belongs — not by counting arc length from wherever the
        // sampler happened to begin.
        // A radius-0 point halfway along the square's left edge — collinear
        // with the edge it sits on, so it turns the road not at all.
        let mut road = square();
        road.points.insert(
            0,
            RoadPoint {
                position: Vec3::new(-30.0, 0.0, 0.0),
                radius: 0.0,
                ..RoadPoint::default()
            },
        );
        let built = build(&road, Mat4::IDENTITY, None);
        let first = Vec3::from_array(built.mesh.positions[2]); // the centre column
        assert!(
            first.distance(Vec3::new(-30.0, 0.0, 0.0)) < 1e-3,
            "the ribbon should begin at the first point, began at {first}"
        );
        assert_eq!(built.mesh.uvs[2][0], 0.0, "and that point is v = 0");
    }

    #[test]
    fn a_sharp_vertex_on_a_straight_is_fine() {
        // How a start line is authored: a point that is not a turn.
        let mut road = straight();
        road.points.insert(
            1,
            RoadPoint {
                position: Vec3::new(0.0, 0.0, -10.0),
                radius: 0.0,
                ..RoadPoint::default()
            },
        );
        assert!(geometry_problems(&road).is_empty());
        let built = build(&road, Mat4::IDENTITY, None);
        assert!((built.length - 20.0).abs() < 1e-3);
    }
}
