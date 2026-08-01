//! Junctions (M40): the patch of asphalt where roads meet.
//!
//! A [`Road`](crate::components::Road) is a ribbon swept along a curve, which
//! is the wrong primitive for a crossroads — two ribbons crossing leave a hole,
//! and no amount of extra centerline points closes it. A [`Junction`] is the
//! other shape: the area **bounded by the mouths of the roads that reach it**.
//!
//! Every number this module needs is read off the roads' finished
//! [`RoadSurface`]s — the same geometry the renderer draws and physics builds
//! its trimesh from. Nothing here re-derives a centerline, a width or a bank,
//! which is `engine road-centerline`'s rule applied inside the engine: two
//! implementations of one curve is how a junction ends up a few centimetres off
//! the road it joins, and a few centimetres is a step a wheel catches on.
//!
//! The patch is drawn by **`road.wgsl`, unchanged**. It emits the same two
//! surface coordinates a road does — `u`, metres out from the middle, and `v` —
//! so asphalt, shoulder and embankment colour themselves through exactly the
//! code that colours a road, with the markings switched off. No new shader, no
//! new pipeline, and nothing added to a fragment path this repo flags as
//! ULP-sensitive.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use glam::{Mat4, Quat, Vec2, Vec3};

use crate::components::{Junction, JunctionEnd, Road};
use crate::mesh::MeshData;
use crate::road::RoadSurface;

/// One road offered to a junction, already built.
///
/// The junction borrows rather than looks up: resolving an entity name is the
/// scene's job, and keeping it out of here is what lets this module be tested
/// without a world.
#[derive(Clone, Copy)]
pub struct Arm<'a> {
    /// The road entity's name, carried through so the published mouths can say
    /// which arm they came from.
    pub name: &'a str,
    pub road: &'a Road,
    pub surface: &'a RoadSurface,
    /// The road entity's flattened transform. The road's centerline is in *its*
    /// local space and the patch is built in the junction's, so everything
    /// crosses through world space on the way.
    pub model: Mat4,
    /// Which end of the road arrives here.
    pub end: JunctionEnd,
}

/// Where one arm actually met the junction, in the junction's local space.
///
/// Published by `engine junction-plan`, because "the patch looks wrong" is
/// otherwise a question only a screenshot can answer — and a screenshot cannot
/// tell a road that stopped 8 m short from a road that arrived at the wrong
/// angle.
#[derive(Debug, Clone, PartialEq)]
pub struct Mouth {
    /// The road entity this arm named.
    pub road: String,
    /// The centre of the road's terminal cross-section.
    pub center: Vec3,
    /// Unit heading **into** the junction, in the XZ plane.
    pub into: Vec2,
    /// Half the asphalt width there, in metres.
    pub half_asphalt: f32,
    /// Half the asphalt-plus-shoulder width there, in metres.
    pub half_total: f32,
    /// How far the mouth sits from the junction's centre, in metres — the
    /// number that says whether a road stopped where the author meant it to.
    pub reach: f32,
}

/// A junction's generated geometry, plus what the arms turned out to be.
#[derive(Debug, Clone, PartialEq)]
pub struct JunctionSurface {
    /// The patch. Behind its own `Arc` for [`RoadSurface`]'s reason: the
    /// renderer's upload cache keys on the allocation's address.
    pub mesh: Arc<MeshData>,
    /// The arms, in the rotational order the patch was built in.
    pub mouths: Vec<Mouth>,
    /// Half the asphalt width the shader is told about — the mean of the arms',
    /// and what `u` reaches at the asphalt boundary. See [`surface`].
    pub half_asphalt: f32,
    /// Shoulder width the shader is told about, likewise the mean.
    pub shoulder: f32,
}

thread_local! {
    static SURFACE_CACHE: RefCell<HashMap<JunctionKey, Arc<JunctionSurface>>> =
        RefCell::new(HashMap::new());
}

const SURFACE_CACHE_LIMIT: usize = 64;

#[derive(Clone, PartialEq, Eq, Hash)]
struct JunctionKey(Vec<u32>);

fn cache_key(junction: &Junction, model: Mat4, arms: &[Arm<'_>]) -> JunctionKey {
    let mut key = Vec::with_capacity(arms.len() * 24 + 32);
    key.push(junction.corner_segments);
    for value in [junction.flare, junction.shoulder, junction.skirt] {
        key.push(value.to_bits());
    }
    key.extend(model.to_cols_array().map(f32::to_bits));
    // The arms' *mouths* are what the patch is built from, so they are what the
    // key holds — not the whole road. A road whose far end moved rebuilds its
    // own ribbon and leaves this junction's patch alone.
    for arm in arms {
        key.extend(arm.name.bytes().map(u32::from));
        key.push(matches!(arm.end, JunctionEnd::End) as u32);
        key.extend(arm.model.to_cols_array().map(f32::to_bits));
        key.push(arm.road.shoulder.to_bits());
        key.push(arm.road.width.to_bits());
        if let Some(point) = terminal(arm) {
            key.extend(point.position.to_array().map(f32::to_bits));
            key.push(point.direction.x.to_bits());
            key.push(point.direction.y.to_bits());
            key.push(point.width.to_bits());
            key.push(point.bank.to_bits());
        }
    }
    JunctionKey(key)
}

/// The junction's patch, generated once per distinct geometry.
pub fn surface(junction: &Junction, model: Mat4, arms: &[Arm<'_>]) -> Arc<JunctionSurface> {
    let key = cache_key(junction, model, arms);
    SURFACE_CACHE.with(|cache| {
        if let Some(held) = cache.borrow().get(&key) {
            return Arc::clone(held);
        }
        let built = Arc::new(build(junction, model, arms));
        let mut cache = cache.borrow_mut();
        if cache.len() >= SURFACE_CACHE_LIMIT {
            cache.retain(|_, held| Arc::strong_count(held) > 1);
        }
        cache.insert(key, Arc::clone(&built));
        built
    })
}

/// The centerline sample at the end of the road that arrives here.
fn terminal<'a>(arm: &Arm<'a>) -> Option<&'a crate::road::CenterPoint> {
    match arm.end {
        JunctionEnd::Start => arm.surface.centerline.first(),
        JunctionEnd::End => arm.surface.centerline.last(),
    }
}

/// One arm resolved into the junction's local space: the five points the patch
/// is stitched from, and the heading that orders them.
struct Resolved {
    name: String,
    center: Vec3,
    into: Vec2,
    /// Asphalt corners, right then left as seen looking *into* the junction.
    asphalt: (Vec3, Vec3),
    /// The same corners out at the shoulder's edge.
    outer: (Vec3, Vec3),
    half_asphalt: f32,
    half_total: f32,
}

fn resolve(arm: &Arm<'_>, inverse: Mat4) -> Option<Resolved> {
    let point = terminal(arm)?;

    // The cross-section frame in the road's own space, banked exactly as the
    // ribbon's last section was — a junction meeting a banked road has to meet
    // it at the same angle or there is a ridge across the mouth.
    let heading = Vec3::new(point.direction.x, 0.0, point.direction.y);
    let level_right = Vec3::new(-point.direction.y, 0.0, point.direction.x);
    let right = if point.bank == 0.0 {
        level_right
    } else {
        Quat::from_axis_angle(heading.normalize_or(Vec3::NEG_Z), -point.bank) * level_right
    };

    let scale = if arm.road.width.abs() < 1e-6 {
        1.0
    } else {
        point.width / arm.road.width
    };
    let half = point.width / 2.0;
    let outer_half = half + arm.road.shoulder.max(0.0) * scale;

    // Corners are transformed rather than widths, so any placement the two
    // entities happen to carry — a translation, a yaw, a scale — comes out
    // exact instead of being reconstructed from a scalar.
    let to_local = |p: Vec3| inverse.transform_point3(arm.model.transform_point3(p));
    let center = to_local(point.position);
    let asphalt = (
        to_local(point.position + right * half),
        to_local(point.position - right * half),
    );
    let outer = (
        to_local(point.position + right * outer_half),
        to_local(point.position - right * outer_half),
    );

    // The heading has to be re-derived in local space too; `direction` runs
    // along increasing `v`, so the road at its *start* points away from the
    // junction and is flipped.
    let world_heading = arm.model.transform_vector3(heading);
    let local_heading = inverse.transform_vector3(world_heading);
    let mut into = Vec2::new(local_heading.x, local_heading.z).normalize_or(Vec2::NEG_Y);
    if matches!(arm.end, JunctionEnd::Start) {
        into = -into;
    }

    Some(Resolved {
        name: arm.name.to_string(),
        center,
        into,
        asphalt,
        outer,
        half_asphalt: center.distance(asphalt.0),
        half_total: center.distance(outer.0),
    })
}

/// Where two lines cross in the XZ plane, or `None` when they are near-parallel.
fn intersect(a: Vec2, da: Vec2, b: Vec2, db: Vec2) -> Option<Vec2> {
    let denominator = da.x * db.y - da.y * db.x;
    if denominator.abs() < 1e-5 {
        return None;
    }
    let offset = b - a;
    let t = (offset.x * db.y - offset.y * db.x) / denominator;
    Some(a + da * t)
}

fn plan(p: Vec3) -> Vec2 {
    Vec2::new(p.x, p.z)
}

/// Twice the signed area of a polygon in the XZ plane.
///
/// The sign is the polygon's orientation, and orientation is what decides
/// whether the fan winds up or down. Measuring it beats deriving it: the arms'
/// order depends on a bearing convention, the bearing convention depends on
/// which way `atan2(z, x)` runs in a Y-up right-handed space, and getting that
/// wrong builds a patch that is invisible under back-face culling — the failure
/// `CLAUDE.md` says to suspect first.
fn signed_area(loop_points: &[Vec3]) -> f32 {
    let n = loop_points.len();
    (0..n)
        .map(|i| {
            let a = plan(loop_points[i]);
            let b = plan(loop_points[(i + 1) % n]);
            a.x * b.y - b.x * a.y
        })
        .sum()
}

fn empty() -> JunctionSurface {
    JunctionSurface {
        mesh: Arc::new(MeshData::default()),
        mouths: Vec::new(),
        half_asphalt: 1.0,
        shoulder: 1.0,
    }
}

fn build(junction: &Junction, model: Mat4, arms: &[Arm<'_>]) -> JunctionSurface {
    let inverse = model.inverse();
    let mut resolved: Vec<Resolved> = arms
        .iter()
        .filter_map(|arm| resolve(arm, inverse))
        .collect();
    if resolved.len() < 2 {
        return empty();
    }

    // The centre is the mean of the mouths, and the arms go round it. Sorting
    // by bearing is what makes the file's arm order irrelevant — a junction
    // authored east, west, north builds the same patch as north, east, west.
    let centroid: Vec3 = resolved.iter().map(|r| r.center).sum::<Vec3>() / resolved.len() as f32;
    resolved.sort_by(|a, b| {
        let bearing = |r: &Resolved| {
            let d = plan(r.center) - plan(centroid);
            d.y.atan2(d.x)
        };
        bearing(a).total_cmp(&bearing(b))
    });

    // What the shader is told the cross-section is. A patch has no single width
    // — its arms may differ — so `u` is quoted against the mean, which puts the
    // asphalt/shoulder boundary exactly on the ring that *is* the boundary and
    // keeps `u` in metres, so the shader's `fwidth` antialiasing means what it
    // means on a road.
    let half_asphalt = resolved.iter().map(|r| r.half_asphalt).sum::<f32>() / resolved.len() as f32;
    let shoulder = junction.shoulder.max(0.0);

    // The boundary rings. Both are built with the same structure — arm mouth,
    // then a flare to the next arm — so ring vertex `k` matches ring vertex `k`
    // and the shoulder is a plain quad strip between them.
    let segments = junction.corner_segments.clamp(1, 64) as usize;
    let flare = junction.flare.clamp(0.0, 1.0);
    let mut inner: Vec<Vec3> = Vec::new();
    let mut outer: Vec<Vec3> = Vec::new();
    // Which arm's mouth each ring vertex belongs to, or `-1` out on a flare.
    //
    // The edge *across* a mouth is where the junction stops and the road takes
    // over: the two rings there are the road's own asphalt corners and its own
    // shoulder corners, all four on one line, so the shoulder quad and the
    // skirt quad spanning that edge are zero-area slivers with no normal. They
    // are skipped rather than emitted degenerate — the shoulder they would have
    // covered is the road's, already drawn by the road.
    let mut mouth_of: Vec<i32> = Vec::new();

    for i in 0..resolved.len() {
        let here = &resolved[i];
        let next = &resolved[(i + 1) % resolved.len()];

        // The mouth itself, straight across: right corner then left, which is
        // the order the bearing runs in.
        inner.push(here.asphalt.0);
        outer.push(here.outer.0);
        mouth_of.push(i as i32);
        inner.push(here.asphalt.1);
        outer.push(here.outer.1);
        mouth_of.push(i as i32);

        // The flare to the next arm. `away` is the direction the road leaves
        // by, so each arm's edge line runs out along it, and where those two
        // lines cross is the corner a real junction has.
        let away_here = -here.into;
        let away_next = -next.into;
        for (ring, from, to, out_from, out_to) in [
            (
                &mut inner,
                here.asphalt.1,
                next.asphalt.0,
                away_here,
                away_next,
            ),
            (&mut outer, here.outer.1, next.outer.0, away_here, away_next),
        ] {
            let corner = intersect(plan(from), out_from, plan(to), out_to);
            let chord = (plan(from) + plan(to)) / 2.0;
            // Near-parallel edges — two arms pointing the same way, which is a
            // road passing straight through — have no crossing, and the chord
            // is exactly right there.
            let control_plan = chord + (corner.unwrap_or(chord) - chord) * flare;
            for k in 1..segments {
                let t = k as f32 / segments as f32;
                let one = 1.0 - t;
                let xz =
                    plan(from) * (one * one) + control_plan * (2.0 * one * t) + plan(to) * (t * t);
                // Height rides linearly across the corner: the two ends are on
                // roads whose levels the junction has to meet, and anything
                // cleverer between them would be inventing a grade nothing
                // asked for.
                let y = from.y * one + to.y * t;
                ring.push(Vec3::new(xz.x, y, xz.y));
            }
        }
        for _ in 1..segments {
            mouth_of.push(-1);
        }
    }

    if inner.len() != outer.len() || inner.len() < 3 {
        return empty();
    }

    // Orientation, measured rather than assumed.
    if signed_area(&inner) > 0.0 {
        inner.reverse();
        outer.reverse();
        mouth_of.reverse();
    }

    let ring = inner.len();
    let interior = Vec3::new(
        centroid.x,
        inner.iter().map(|p| p.y).sum::<f32>() / ring as f32,
        centroid.z,
    );

    let mut mesh = MeshData {
        positions: Vec::with_capacity(ring * 4 + 1),
        normals: Vec::with_capacity(ring * 4 + 1),
        uvs: Vec::with_capacity(ring * 4 + 1),
        indices: Vec::with_capacity(ring * 12),
        ..MeshData::default()
    };

    // `v` is the vertex's local X. The patch paints no markings — they are all
    // switched off in the synthesized road — so `v` is only ever read by the
    // grain, which wants a second axis and does not care which.
    let mut push = |position: Vec3, u: f32| {
        mesh.positions.push(position.to_array());
        mesh.normals.push([0.0, 1.0, 0.0]);
        mesh.uvs.push([position.x, u]);
    };

    // 0 = the centre; 1..=ring the asphalt boundary; then the shoulder's outer
    // edge; then the skirt's bottom.
    push(interior, 0.0);
    for point in &inner {
        push(*point, half_asphalt);
    }
    for point in &outer {
        push(*point, half_asphalt + shoulder);
    }
    let skirt_base = mesh.positions.len() as u32;
    for point in &outer {
        // The skirt is its own vertices so the crease at the shoulder's edge
        // stays a crease, exactly as a road's does.
        mesh.positions.push(point.to_array());
        mesh.normals.push([0.0, 0.0, 0.0]);
        mesh.uvs.push([point.x, half_asphalt + shoulder]);
        mesh.positions
            .push((*point - Vec3::Y * junction.skirt.max(0.0)).to_array());
        mesh.normals.push([0.0, 0.0, 0.0]);
        mesh.uvs
            .push([point.x, half_asphalt + shoulder + junction.skirt.max(0.0)]);
    }

    let inner_base = 1u32;
    let outer_base = 1 + ring as u32;

    for k in 0..ring {
        let next = (k + 1) % ring;
        let (a, b) = (inner_base + k as u32, inner_base + next as u32);
        let (c, d) = (outer_base + k as u32, outer_base + next as u32);

        // The fan reaches every edge, mouths included: the span across a
        // mouth is the road's full asphalt width, and the patch has to close
        // back to its centre there like anywhere else.
        mesh.indices.extend_from_slice(&[0, a, b]);

        if mouth_of[k] >= 0 && mouth_of[k] == mouth_of[next] {
            continue;
        }

        mesh.indices.extend_from_slice(&[a, c, b, b, c, d]);

        // The skirt: two vertices per boundary point, top then bottom.
        let (top_here, bottom_here) = (skirt_base + 2 * k as u32, skirt_base + 2 * k as u32 + 1);
        let (top_next, bottom_next) = (
            skirt_base + 2 * next as u32,
            skirt_base + 2 * next as u32 + 1,
        );
        mesh.indices
            .extend_from_slice(&[top_here, bottom_here, top_next]);
        mesh.indices
            .extend_from_slice(&[bottom_here, bottom_next, top_next]);
    }

    fix_normals(&mut mesh, skirt_base, &outer, centroid);

    JunctionSurface {
        mesh: Arc::new(mesh),
        mouths: resolved
            .iter()
            .map(|r| Mouth {
                road: r.name.clone(),
                center: r.center,
                into: r.into,
                half_asphalt: r.half_asphalt,
                half_total: r.half_total,
                reach: plan(r.center).distance(plan(centroid)),
            })
            .collect(),
        half_asphalt,
        shoulder,
    }
}

/// Shade the top surface from the triangles that actually meet at each vertex,
/// and the skirt from the direction it faces.
///
/// The top is accumulated rather than set to `+Y` because a junction between
/// roads at different levels is a ramp, and a ramp shaded as flat ground reads
/// as a rendering bug. The skirt keeps its own outward normals for the reason a
/// road's does: the crease at the shoulder's edge is a crease.
fn fix_normals(mesh: &mut MeshData, skirt_base: u32, outer: &[Vec3], centroid: Vec3) {
    let top = skirt_base as usize;
    for normal in mesh.normals[..top].iter_mut() {
        *normal = [0.0, 0.0, 0.0];
    }
    for triangle in mesh.indices.chunks_exact(3) {
        if triangle.iter().any(|&i| i >= skirt_base) {
            continue;
        }
        let p = |i: u32| Vec3::from_array(mesh.positions[i as usize]);
        let (a, b, c) = (p(triangle[0]), p(triangle[1]), p(triangle[2]));
        let face = (b - a).cross(c - a);
        for &i in triangle {
            let normal = &mut mesh.normals[i as usize];
            *normal = (Vec3::from_array(*normal) + face).to_array();
        }
    }
    for normal in mesh.normals[..top].iter_mut() {
        *normal = Vec3::from_array(*normal).normalize_or(Vec3::Y).to_array();
    }

    for (k, point) in outer.iter().enumerate() {
        let outward = (plan(*point) - plan(centroid)).normalize_or(Vec2::Y);
        let outward = [outward.x, 0.0, outward.y];
        mesh.normals[skirt_base as usize + 2 * k] = outward;
        mesh.normals[skirt_base as usize + 2 * k + 1] = outward;
    }
}

/// The `Road` the shader is handed for a junction's patch.
///
/// A junction is drawn by the road pipeline, so the per-draw uniform is a road
/// uniform, so the junction has to answer as a road — and the honest way to do
/// that is to build the road it *is*: this width, this shoulder, these colours,
/// and **every marking off**. Real junction markings are per-arm and per-lane,
/// they want a lane model the engine does not have, and half of them are decals
/// rather than paint.
pub fn as_road(junction: &Junction, surface: &JunctionSurface) -> Road {
    Road {
        width: surface.half_asphalt * 2.0,
        shoulder: surface.shoulder,
        skirt: junction.skirt,
        color: junction.color,
        roughness: junction.roughness,
        shoulder_color: junction.shoulder_color,
        bank_color: junction.bank_color,
        grain: junction.grain,
        grain_scale: junction.grain_scale,
        markings: crate::components::RoadMarkings {
            edge_width: 0.0,
            center_width: 0.0,
            kerb_max_radius: 0.0,
            kerb_width: 0.0,
            start_line: false,
            ..crate::components::RoadMarkings::default()
        },
        ..Road::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::{JunctionArm, RoadPoint};

    /// A straight road running along ±Z, ending at the origin.
    fn arm_road(from: Vec3, to: Vec3) -> Road {
        Road {
            points: vec![
                RoadPoint {
                    position: from,
                    ..RoadPoint::default()
                },
                RoadPoint {
                    position: to,
                    ..RoadPoint::default()
                },
            ],
            closed: false,
            width: 8.0,
            shoulder: 2.0,
            skirt: 0.5,
            ..Road::default()
        }
    }

    struct Built {
        road: Road,
        surface: Arc<RoadSurface>,
    }

    fn built(from: Vec3, to: Vec3) -> Built {
        let road = arm_road(from, to);
        let surface = crate::road::surface(&road, Mat4::IDENTITY, None);
        Built { road, surface }
    }

    /// A crossroads: four roads ending 12 m out on each axis.
    fn crossroads() -> Vec<Built> {
        vec![
            built(Vec3::new(0.0, 0.0, -60.0), Vec3::new(0.0, 0.0, -12.0)),
            built(Vec3::new(0.0, 0.0, 60.0), Vec3::new(0.0, 0.0, 12.0)),
            built(Vec3::new(-60.0, 0.0, 0.0), Vec3::new(-12.0, 0.0, 0.0)),
            built(Vec3::new(60.0, 0.0, 0.0), Vec3::new(12.0, 0.0, 0.0)),
        ]
    }

    fn arms(roads: &[Built]) -> Vec<Arm<'_>> {
        roads
            .iter()
            .enumerate()
            .map(|(i, b)| Arm {
                name: ["North", "South", "West", "East"][i],
                road: &b.road,
                surface: &b.surface,
                model: Mat4::IDENTITY,
                end: JunctionEnd::End,
            })
            .collect()
    }

    #[test]
    fn the_patch_faces_up() {
        // The failure this guards is the one `CLAUDE.md` says to suspect first:
        // a wrongly wound patch renders *nothing at all* under back-face
        // culling, so "the junction is invisible" and "the junction was never
        // built" look identical from a screenshot.
        let roads = crossroads();
        let built = build(&Junction::default(), Mat4::IDENTITY, &arms(&roads));
        let mesh = &built.mesh;
        // Centre, then three rings of `ring` vertices and a skirt of two per
        // ring point: `1 + 4 * ring` in all.
        let ring = (mesh.positions.len() - 1) / 4;
        let skirt_from = 1 + 2 * ring;

        let mut checked = 0;
        for triangle in mesh.indices.chunks_exact(3) {
            if triangle.iter().any(|&i| i as usize >= skirt_from) {
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
    fn the_patch_reaches_every_mouth() {
        // The junction's whole contract: it meets the roads where they actually
        // ended. A patch that stops short leaves the hole it exists to fill.
        let roads = crossroads();
        let built = build(&Junction::default(), Mat4::IDENTITY, &arms(&roads));
        assert_eq!(built.mouths.len(), 4);
        for mouth in &built.mouths {
            assert!(
                (mouth.reach - 12.0).abs() < 0.2,
                "{} arrived {} m out, expected 12",
                mouth.road,
                mouth.reach
            );
            assert!((mouth.half_asphalt - 4.0).abs() < 1e-3);
            assert!((mouth.half_total - 6.0).abs() < 1e-3);

            // The mouth's corners have to be *on* the patch, or there is a step
            // across the join.
            let corner = mouth.center + Vec3::new(-mouth.into.y, 0.0, mouth.into.x) * 4.0;
            let nearest = built
                .mesh
                .positions
                .iter()
                .map(|p| Vec3::from_array(*p).distance(corner))
                .fold(f32::MAX, f32::min);
            assert!(
                nearest < 1e-3,
                "{}'s asphalt corner is {nearest} m from the patch",
                mouth.road
            );
        }
    }

    #[test]
    fn arm_order_in_the_file_does_not_matter() {
        // Sorting by bearing is what makes this true, and it is the difference
        // between a component an agent can edit and one where moving a line
        // reshapes the geometry.
        let roads = crossroads();
        let forward = build(&Junction::default(), Mat4::IDENTITY, &arms(&roads));
        let mut reversed = arms(&roads);
        reversed.reverse();
        let backward = build(&Junction::default(), Mat4::IDENTITY, &reversed);
        assert_eq!(forward.mesh, backward.mesh);
    }

    #[test]
    fn a_start_end_arm_points_the_same_way_as_an_end_one() {
        // `direction` runs along increasing `v`, so a road *starting* at the
        // junction points away from it. Getting the flip wrong builds the patch
        // inside out around that arm.
        let away = built(Vec3::new(0.0, 0.0, 12.0), Vec3::new(0.0, 0.0, 60.0));
        let toward = built(Vec3::new(0.0, 0.0, 60.0), Vec3::new(0.0, 0.0, 12.0));

        let by_start = resolve(
            &Arm {
                name: "away",
                road: &away.road,
                surface: &away.surface,
                model: Mat4::IDENTITY,
                end: JunctionEnd::Start,
            },
            Mat4::IDENTITY,
        )
        .expect("resolved");
        let by_end = resolve(
            &Arm {
                name: "toward",
                road: &toward.road,
                surface: &toward.surface,
                model: Mat4::IDENTITY,
                end: JunctionEnd::End,
            },
            Mat4::IDENTITY,
        )
        .expect("resolved");

        assert!(
            by_start.into.distance(by_end.into) < 1e-4,
            "{:?} vs {:?}",
            by_start.into,
            by_end.into
        );
    }

    #[test]
    fn u_finds_the_asphalt_edge_the_way_a_road_does() {
        // The patch is drawn by the road shader, which decides asphalt from
        // shoulder by comparing `u` against half the width in the road uniform.
        // If the synthesized road disagrees with the ring the patch was built
        // with, a junction renders as one flat colour.
        let roads = crossroads();
        let junction = Junction::default();
        let built = build(&junction, Mat4::IDENTITY, &arms(&roads));
        let as_road = as_road(&junction, &built);

        assert!((as_road.width / 2.0 - built.half_asphalt).abs() < 1e-4);
        assert!(
            (built.half_asphalt - 4.0).abs() < 1e-3,
            "mean of four 4 m halves"
        );

        let centre_u = built.mesh.uvs[0][1];
        assert_eq!(centre_u, 0.0, "the middle of a junction is asphalt");
        let boundary_u = built.mesh.uvs[1][1];
        assert!((boundary_u - as_road.width / 2.0).abs() < 1e-4);
    }

    #[test]
    fn a_junction_between_two_arms_is_a_join() {
        // Two arms pointing at each other have no corner to flare — the edges
        // are parallel and never cross — and the chord is the right answer
        // there. This is the degenerate case `intersect` returns `None` for.
        let roads = [
            built(Vec3::new(0.0, 0.0, -60.0), Vec3::new(0.0, 0.0, -8.0)),
            built(Vec3::new(0.0, 0.0, 60.0), Vec3::new(0.0, 0.0, 8.0)),
        ];
        let arms: Vec<Arm<'_>> = roads
            .iter()
            .enumerate()
            .map(|(i, b)| Arm {
                name: ["North", "South"][i],
                road: &b.road,
                surface: &b.surface,
                model: Mat4::IDENTITY,
                end: JunctionEnd::End,
            })
            .collect();
        let built = build(&Junction::default(), Mat4::IDENTITY, &arms);
        assert_eq!(built.mouths.len(), 2);
        assert!(
            built.mesh.positions.iter().all(|p| p[0].abs() < 6.1),
            "a straight join must not bulge sideways"
        );
    }

    #[test]
    fn too_few_arms_build_nothing() {
        // Validation refuses this first; the generator still has to not panic.
        let roads = crossroads();
        let arms = arms(&roads);
        let built = build(&Junction::default(), Mat4::IDENTITY, &arms[..1]);
        assert!(built.mesh.positions.is_empty());
        assert!(build(&Junction::default(), Mat4::IDENTITY, &[])
            .mesh
            .positions
            .is_empty());
    }

    #[test]
    fn one_patch_per_geometry_is_shared() {
        let roads = crossroads();
        let junction = Junction::default();
        let arms = arms(&roads);
        assert!(Arc::ptr_eq(
            &surface(&junction, Mat4::IDENTITY, &arms).mesh,
            &surface(&junction, Mat4::IDENTITY, &arms).mesh
        ));

        // Colour is read per pixel and moves no vertex.
        let mut repainted = junction.clone();
        repainted.color = Vec3::new(0.4, 0.1, 0.1);
        assert!(Arc::ptr_eq(
            &surface(&junction, Mat4::IDENTITY, &arms).mesh,
            &surface(&repainted, Mat4::IDENTITY, &arms).mesh
        ));

        let mut wider = junction.clone();
        wider.shoulder = 4.0;
        assert!(!Arc::ptr_eq(
            &surface(&junction, Mat4::IDENTITY, &arms).mesh,
            &surface(&wider, Mat4::IDENTITY, &arms).mesh
        ));
    }

    #[test]
    fn arms_default_to_the_end_of_the_road() {
        let arm: JunctionArm = serde_json::from_str(r#"{"road": "Main"}"#).unwrap();
        assert_eq!(arm.end, JunctionEnd::End);
        assert_eq!(arm.road, "Main");
    }
}
