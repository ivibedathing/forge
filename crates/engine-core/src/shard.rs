//! Convex shard geometry (M43).
//!
//! A [`Shard`] is a convex point set that owns its geometry — the
//! `Water`/`Terrain`/`Cloud`/`Meadow` rule, applied to a broken piece. This
//! module turns the points into the one hull that both the renderer draws and
//! rapier collides, CPU-side and GPU-free, so the whole thing unit-tests
//! without an adapter.
//!
//! # Why the hull is recomputed rather than stored
//!
//! A scene file carries points, never faces. rapier builds its collider from
//! the hull of the points whatever the file says, so a stored face list would
//! be a second source of truth that can disagree with the shape physics
//! actually uses. Recomputing here makes the drawn shard and the collided shard
//! the same solid by construction.
//!
//! # The hull
//!
//! Brute force over triples, deliberately. A shard has a handful of points (the
//! schema caps it at [`MAX_SHARD_POINTS`]), and for that size an exhaustive
//! search is both faster to trust and far easier to keep deterministic than an
//! incremental hull: a plane through three points is a face when every other
//! point lies on one side of it, and there is no merge order, no horizon walk
//! and no degenerate-case bookkeeping to get subtly wrong.
//!
//! Faces are **flat-shaded** — each gets its own vertices and one normal.
//! Shards read as shards because their facets are sharp; smoothing normals
//! across the hull turns gravel into river pebbles.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use glam::Vec3;

use crate::components::Shard;
use crate::mesh::MeshData;

/// Points per shard, capped in the schema. A Voronoi cell of a box rarely
/// passes twenty vertices; the ceiling is here so the O(n⁴) face search stays a
/// bounded cost at load and so a generated scene file stays legible.
pub const MAX_SHARD_POINTS: usize = 32;

/// Two points closer than this are the same point. Generous next to the
/// generator's own output — its cells are computed by intersecting planes, and
/// three planes meeting at a shallow angle put the same corner in twice with a
/// few ulps between them.
const WELD: f32 = 1e-5;

/// How far off a plane a point may sit and still count as on it. Also the
/// tolerance the "everything is on one side" test runs at.
const ON_PLANE: f32 = 1e-4;

/// One flat face of the hull: an outward normal and its points in
/// counter-clockwise order seen from outside.
#[derive(Debug, Clone, PartialEq)]
pub struct Face {
    pub normal: Vec3,
    pub points: Vec<Vec3>,
}

/// The hull's faces, or `None` when the points do not bound a volume — fewer
/// than four distinct points, or all of them coplanar, collinear or coincident.
///
/// `None` is the case validation reports as `shard_degenerate`. It is worth
/// catching there rather than here: a degenerate shard draws nothing and
/// collides with nothing, which is the hardest failure there is to read off a
/// picture.
pub fn hull(points: &[Vec3]) -> Option<Vec<Face>> {
    let unique = welded(points);
    if unique.len() < 4 {
        return None;
    }

    // Every plane through three of the points that has all the others on one
    // side. Collected in index order, so the face list is a function of the
    // point list and not of anything's iteration order.
    let mut planes: Vec<(Vec3, f32)> = Vec::new();
    for i in 0..unique.len() {
        for j in (i + 1)..unique.len() {
            for k in (j + 1)..unique.len() {
                let cross = (unique[j] - unique[i]).cross(unique[k] - unique[i]);
                let length = cross.length();
                if length < WELD {
                    continue; // collinear triple: no plane
                }
                let normal = cross / length;
                let offset = normal.dot(unique[i]);

                let mut above = false;
                let mut below = false;
                for point in &unique {
                    let gap = normal.dot(*point) - offset;
                    if gap > ON_PLANE {
                        above = true;
                    } else if gap < -ON_PLANE {
                        below = true;
                    }
                }
                // Inside on one side only, and the normal points away from it.
                let outward = match (above, below) {
                    (false, true) => (normal, offset),
                    (true, false) => (-normal, -offset),
                    _ => continue,
                };
                if !planes
                    .iter()
                    .any(|(n, d)| n.dot(outward.0) > 0.9999 && (d - outward.1).abs() < ON_PLANE)
                {
                    planes.push(outward);
                }
            }
        }
    }

    // Four planes is a tetrahedron; fewer means the points never bounded a
    // volume — every triple was coplanar with every other point.
    if planes.len() < 4 {
        return None;
    }

    let faces: Vec<Face> = planes
        .into_iter()
        .filter_map(|(normal, offset)| {
            let on: Vec<Vec3> = unique
                .iter()
                .copied()
                .filter(|p| (normal.dot(*p) - offset).abs() <= ON_PLANE)
                .collect();
            (on.len() >= 3).then(|| Face {
                normal,
                points: wound(on, normal),
            })
        })
        .collect();

    (faces.len() >= 4).then_some(faces)
}

/// The hull as geometry: flat-shaded, fan-triangulated, with planar UVs.
pub fn mesh_from_points(points: &[Vec3]) -> Option<MeshData> {
    let faces = hull(points)?;
    let mut mesh = MeshData::default();

    for face in &faces {
        let (u_axis, v_axis) = basis(face.normal);
        let base = mesh.positions.len() as u32;
        for point in &face.points {
            mesh.positions.push(point.to_array());
            mesh.normals.push(face.normal.to_array());
            // Planar projection in the face's own basis, in metres. A shard has
            // no natural parameterization, and a material with a texture on one
            // is showing the same surface the parent showed.
            mesh.uvs.push([u_axis.dot(*point), v_axis.dot(*point)]);
        }
        // A fan off the first vertex. Faces are convex by construction, so the
        // fan is valid however many points the face has.
        for corner in 1..(face.points.len() as u32 - 1) {
            mesh.indices
                .extend_from_slice(&[base, base + corner, base + corner + 1]);
        }
    }

    Some(mesh)
}

/// Build a shard's geometry, or hand back the copy already built.
///
/// Keyed on the points themselves. Sharing the `Arc` is not just an allocation
/// saved: the renderer's per-frame upload cache keys on `Arc` identity (M15),
/// so a fresh copy each frame would re-upload every shard in the scene every
/// frame — and a shattered crate is thirty of them.
///
/// A degenerate shard caches as an empty mesh rather than as a miss, so a scene
/// that somehow reached the renderer with one does not re-attempt the hull
/// every frame. Validation is what stops that scene existing.
pub fn mesh_for(shard: &Shard) -> Arc<MeshData> {
    SHARD_CACHE.with(|cache| {
        let key = ShardKey::of(shard);
        if let Some(hit) = cache.borrow().get(&key) {
            return Arc::clone(hit);
        }
        let built = Arc::new(mesh_from_points(&shard.points).unwrap_or_default());
        let mut cache = cache.borrow_mut();
        // A script animating a shard's points would mint a key a step, so the
        // cache is bounded rather than an incidental leak — `cloud.rs`'s
        // reasoning and its three-line resolution.
        if cache.len() >= MAX_CACHED_SHARDS {
            cache.clear();
        }
        cache.insert(key, Arc::clone(&built));
        built
    })
}

/// The centroid of a point set — where a fragment is, for the scatter M43's
/// materials aim (`breaking.rs`), and where the generator reports a cell.
///
/// The mean of the hull's *vertices*, not the volume centroid. It is the
/// cheaper quantity, it needs no hull, and the difference between the two only
/// matters for a shard far more lopsided than a Voronoi cell of a box.
pub fn centroid(points: &[Vec3]) -> Vec3 {
    if points.is_empty() {
        return Vec3::ZERO;
    }
    points.iter().copied().sum::<Vec3>() / points.len() as f32
}

/// The hull's enclosed volume in m³ — what the generator's tiling test measures
/// and what makes a shard's mass reportable without building a physics world.
///
/// Signed tetrahedra from the origin, summed over the triangulated faces: the
/// standard divergence-theorem form, and exact for any closed hull.
pub fn volume(points: &[Vec3]) -> f32 {
    let Some(faces) = hull(points) else {
        return 0.0;
    };
    let mut total = 0.0;
    for face in &faces {
        for corner in 1..(face.points.len() - 1) {
            let (a, b, c) = (face.points[0], face.points[corner], face.points[corner + 1]);
            total += a.dot(b.cross(c)) / 6.0;
        }
    }
    total.abs()
}

/// Distinct points, in first-seen order.
fn welded(points: &[Vec3]) -> Vec<Vec3> {
    let mut unique: Vec<Vec3> = Vec::with_capacity(points.len());
    for point in points {
        if !unique.iter().any(|kept| kept.distance(*point) < WELD) {
            unique.push(*point);
        }
    }
    unique
}

/// A face's points in counter-clockwise order seen from outside, by angle
/// around their own centre in the face's plane.
fn wound(points: Vec<Vec3>, normal: Vec3) -> Vec<Vec3> {
    let centre = centroid(&points);
    let (u_axis, v_axis) = basis(normal);
    let mut sorted: Vec<(f32, Vec3)> = points
        .into_iter()
        .map(|point| {
            let offset = point - centre;
            (v_axis.dot(offset).atan2(u_axis.dot(offset)), point)
        })
        .collect();
    // `total_cmp`, not `partial_cmp`: a NaN angle would otherwise make the sort
    // order depend on the comparison order, and a shard's winding is part of
    // what the renderer draws.
    sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
    sorted.into_iter().map(|(_, point)| point).collect()
}

/// Two unit axes spanning the plane a normal defines, chosen deterministically.
fn basis(normal: Vec3) -> (Vec3, Vec3) {
    // Cross with whichever cardinal axis the normal is least aligned to, so the
    // cross product never approaches zero.
    let seed = if normal.x.abs() < 0.9 {
        Vec3::X
    } else {
        Vec3::Y
    };
    let u_axis = seed.cross(normal).normalize_or_zero();
    let u_axis = if u_axis == Vec3::ZERO {
        Vec3::X
    } else {
        u_axis
    };
    (u_axis, normal.cross(u_axis))
}

const MAX_CACHED_SHARDS: usize = 1024;

/// A shard's points, bit for bit — geometry is all a `Shard` has.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ShardKey(Vec<[u32; 3]>);

impl ShardKey {
    fn of(shard: &Shard) -> Self {
        Self(
            shard
                .points
                .iter()
                .map(|p| [p.x.to_bits(), p.y.to_bits(), p.z.to_bits()])
                .collect(),
        )
    }
}

thread_local! {
    /// Generated geometry is a pure function of the component, so a
    /// process-local cache is not hidden state (invariant 2) any more than
    /// `mesh.rs`'s builtin cache is.
    static SHARD_CACHE: RefCell<HashMap<ShardKey, Arc<MeshData>>> = RefCell::new(HashMap::new());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The eight corners of a unit cube, in an order no algorithm would pick.
    fn cube() -> Vec<Vec3> {
        vec![
            Vec3::new(0.5, 0.5, -0.5),
            Vec3::new(-0.5, -0.5, -0.5),
            Vec3::new(0.5, -0.5, 0.5),
            Vec3::new(-0.5, 0.5, 0.5),
            Vec3::new(0.5, -0.5, -0.5),
            Vec3::new(-0.5, 0.5, -0.5),
            Vec3::new(0.5, 0.5, 0.5),
            Vec3::new(-0.5, -0.5, 0.5),
        ]
    }

    #[test]
    fn a_cube_hulls_into_six_faces() {
        let faces = hull(&cube()).expect("a cube is a hull");
        assert_eq!(faces.len(), 6, "one face per side, coplanar triples merged");
        for face in &faces {
            assert_eq!(face.points.len(), 4, "a cube's face is a quad");
            // Every face normal is a cardinal axis pointing outward.
            let outward = face.normal.dot(centroid(&face.points));
            assert!(outward > 0.0, "normal points away from the centre");
        }
    }

    #[test]
    fn a_cube_encloses_its_volume() {
        assert!((volume(&cube()) - 1.0).abs() < 1e-4, "{}", volume(&cube()));
    }

    #[test]
    fn extra_points_inside_the_hull_change_nothing() {
        let mut with_interior = cube();
        with_interior.push(Vec3::new(0.1, -0.2, 0.05));
        with_interior.push(Vec3::ZERO);
        let faces = hull(&with_interior).expect("still a cube");
        assert_eq!(faces.len(), 6);
        assert!((volume(&with_interior) - 1.0).abs() < 1e-4);
    }

    #[test]
    fn a_tetrahedron_is_the_smallest_hull() {
        let points = vec![Vec3::ZERO, Vec3::X, Vec3::Y, Vec3::Z];
        let faces = hull(&points).expect("four points bound a volume");
        assert_eq!(faces.len(), 4);
        // A corner tetrahedron of the unit cube is a sixth of it.
        assert!((volume(&points) - 1.0 / 6.0).abs() < 1e-5);
    }

    #[test]
    fn degenerate_point_sets_have_no_hull() {
        assert_eq!(hull(&[]), None, "nothing");
        assert_eq!(hull(&[Vec3::ZERO, Vec3::X, Vec3::Y]), None, "a triangle");
        assert_eq!(
            hull(&[Vec3::ZERO, Vec3::X, Vec3::Y, Vec3::new(1.0, 1.0, 0.0)]),
            None,
            "four coplanar points"
        );
        assert_eq!(
            hull(&[Vec3::ZERO, Vec3::X, Vec3::X * 2.0, Vec3::X * 3.0]),
            None,
            "collinear points"
        );
        assert_eq!(
            hull(&[Vec3::ZERO, Vec3::ZERO, Vec3::ZERO, Vec3::ZERO]),
            None,
            "one point four times"
        );
    }

    #[test]
    fn the_mesh_is_flat_shaded_and_closed() {
        let mesh = mesh_from_points(&cube()).expect("a cube meshes");
        // Six quads, each with its own four vertices and two triangles.
        assert_eq!(mesh.positions.len(), 24, "no vertex is shared across faces");
        assert_eq!(mesh.triangle_count(), 12);
        assert_eq!(mesh.normals.len(), mesh.positions.len());
        assert_eq!(mesh.uvs.len(), mesh.positions.len());

        // Every triangle winds counter-clockwise seen from outside, which is
        // the whole reason a wrongly wound shard would render as nothing.
        for triangle in mesh.indices.chunks_exact(3) {
            let [a, b, c] =
                [0, 1, 2].map(|i| Vec3::from_array(mesh.positions[triangle[i] as usize]));
            let facing = (b - a).cross(c - a);
            let outward = centroid(&[a, b, c]);
            assert!(
                facing.dot(outward) > 0.0,
                "triangle {triangle:?} winds inward"
            );
        }
    }

    #[test]
    fn the_hull_does_not_depend_on_the_point_order() {
        let straight = hull(&cube()).unwrap();
        let mut shuffled = cube();
        shuffled.reverse();
        let reversed = hull(&shuffled).unwrap();
        assert_eq!(straight.len(), reversed.len());
        // Same set of planes, whatever order they were found in.
        for face in &straight {
            assert!(
                reversed
                    .iter()
                    .any(|other| other.normal.distance(face.normal) < 1e-5),
                "face {:?} is missing from the reversed hull",
                face.normal
            );
        }
    }

    #[test]
    fn geometry_is_shared_across_calls() {
        let shard = Shard { points: cube() };
        assert!(Arc::ptr_eq(&mesh_for(&shard), &mesh_for(&shard)));
        assert!(!Arc::ptr_eq(
            &mesh_for(&shard),
            &mesh_for(&Shard {
                points: vec![Vec3::ZERO, Vec3::X, Vec3::Y, Vec3::Z]
            })
        ));
    }
}
