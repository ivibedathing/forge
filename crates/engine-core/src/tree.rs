//! Procedural tree geometry (M19).
//!
//! A [`Tree`] component is not a mesh reference — it is a recipe, and this
//! module is what turns the recipe into two meshes: the woody one (trunk and
//! branches, drawn with the entity's `Material`) and the leafy one (drawn with
//! the tree's own foliage fields). Everything here is CPU-side and GPU-free, so
//! the whole generator unit-tests without an adapter, exactly like
//! [`crate::particles`].
//!
//! # The model
//!
//! A branch is a polyline that wanders. Each of its `segments` steps rotates
//! the growth direction by a random *crook* and bends it back toward +Y by
//! *tropism*, and a tube of `sides` faces is swept along the result with the
//! radius tapering from base to tip. Children attach at points spaced along the
//! parent past `branch_start`, rotated `branch_angle` off the parent's
//! direction and spun around it by `branch_twist` per point — the golden angle
//! by default, which is what actual phyllotaxis converges on. Leaves scatter
//! over the outermost generation.
//!
//! That is a deliberately small model. It is not Weber–Penn: there are no
//! per-level parameter arrays, no splits, no pruning envelope. What it does
//! have is the four things that separate a tree from a lollipop — taper, curve,
//! recursive branching, and a root flare — and one dial (`jitter`) that makes
//! every instance of it different.
//!
//! # Determinism
//!
//! One private xorshift, seeded from `Tree::seed`, drives every draw, in a
//! fixed order: a branch draws its own segment wander, then recurses into each
//! child in index order, then (if it is outermost) scatters its leaves. Same
//! component, same mesh, forever — which is what lets a forest sit under a
//! `diff-render` baseline. The generator and its hash are spelled out in this
//! repo for the same reason the particle RNG is: no dependency upgrade may
//! change what a scene looks like.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use glam::{Quat, Vec3};

use crate::components::{Tree, TreeLeaf};
use crate::mesh::MeshData;

/// A generated tree: bark, and foliage when it has any.
#[derive(Debug, Clone)]
pub struct TreeMeshes {
    /// Trunk and branches, for the entity's own `Material`.
    pub bark: Arc<MeshData>,
    /// Leaves, for [`Tree::leaf_material`]. `None` when the tree is bare —
    /// an empty mesh would cost a draw call that renders nothing.
    pub leaves: Option<Arc<MeshData>>,
}

/// Grow a tree's geometry, or hand back the copy already grown.
///
/// The cache is keyed on the component's exact field bits, so two entities
/// with identical parameters share one mesh (and one GPU upload), while one
/// changed field — a different `seed`, an animated `height` — is a different
/// tree. Sharing the `Arc` is not just an allocation saved: `MeshSource`'s
/// contract, and the renderer's per-frame upload cache, both key on `Arc`
/// identity, so handing back a fresh copy each frame would re-upload every
/// tree in the scene every frame.
pub fn meshes_for(tree: &Tree) -> TreeMeshes {
    TREE_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(hit) = cache.get(&TreeKey::of(tree)) {
            return hit.clone();
        }

        // Animating a tree parameter mints a new key every step, so the cache
        // is bounded rather than an incidental leak. Clearing wholesale (over
        // evicting by age) keeps this to three lines; the cost of a miss is one
        // regeneration, and a scene with hundreds of *distinct* trees is
        // already paying that at load.
        if cache.len() >= MAX_CACHED_TREES {
            cache.clear();
        }

        let (bark, leaves) = generate(tree);
        let grown = TreeMeshes {
            bark: Arc::new(bark),
            leaves: (!leaves.indices.is_empty()).then(|| Arc::new(leaves)),
        };
        cache.insert(TreeKey::of(tree), grown.clone());
        grown
    })
}

/// Grow a tree's geometry unconditionally: `(bark, leaves)`, the leaf mesh
/// empty when the tree is bare. Pure — the cached [`meshes_for`] is the one
/// callers should use.
pub fn generate(tree: &Tree) -> (MeshData, MeshData) {
    let mut builder = Builder {
        tree,
        rng: seed_state(tree.seed),
        bark: MeshData {
            positions: Vec::new(),
            normals: Vec::new(),
            uvs: Vec::new(),
            indices: Vec::new(),
            ..MeshData::default()
        },
        leaves: MeshData {
            positions: Vec::new(),
            normals: Vec::new(),
            uvs: Vec::new(),
            indices: Vec::new(),
            ..MeshData::default()
        },
    };

    builder.grow(
        0,
        Vec3::ZERO,
        Vec3::Y,
        Vec3::X,
        tree.height,
        tree.trunk_radius,
    );

    (builder.bark, builder.leaves)
}

/// Vertices this tree would generate, computed from the parameters alone —
/// what validation checks before anything is allocated.
///
/// Exact rather than an estimate, so the budget error can name a real number.
pub fn vertex_count(tree: &Tree) -> u64 {
    let per_node = tree.sides as u64;
    // Rings, plus the two cap centers.
    let per_branch = per_node * (tree.segments as u64 + 1) + 2;

    let mut branches_at_level = 1u64;
    let mut total_branches = 0u64;
    // The generation that carries leaves. Not always `levels`: a tree that
    // branches nowhere (`branches: 0`) is one trunk, and the trunk is
    // outermost.
    let mut outermost = 1u64;
    for level in 0..=tree.levels {
        if branches_at_level == 0 {
            break;
        }
        total_branches = total_branches.saturating_add(branches_at_level);
        outermost = branches_at_level;
        branches_at_level = branches_at_level
            .saturating_mul(tree.branches as u64)
            .saturating_mul(if level == 0 { tree.whorl as u64 } else { 1 });
    }

    let leaves = outermost.saturating_mul(tree.leaves_per_branch as u64) * leaf_vertices(tree.leaf);
    total_branches
        .saturating_mul(per_branch)
        .saturating_add(leaves)
}

/// Beyond this a tree is a mistake in a parameter, not a plan — see
/// `tree_too_complex`. Generous enough for `levels: 4` at sane branch counts.
pub const MAX_TREE_VERTICES: u64 = 100_000;

const MAX_CACHED_TREES: usize = 256;

fn leaf_vertices(leaf: TreeLeaf) -> u64 {
    match leaf {
        // Two flat wings, doubled to face both ways, flat-shaded.
        TreeLeaf::Blade => 12,
        TreeLeaf::Cluster => 6,
        TreeLeaf::None => 0,
    }
}

// ── the generator ──────────────────────────────────────────────────────────

/// One node of a branch's polyline: where it is, which way it is heading, and
/// the reference direction that keeps consecutive rings from twisting against
/// each other.
#[derive(Clone, Copy)]
struct Node {
    position: Vec3,
    axis: Vec3,
    /// Perpendicular to `axis`, carried forward by the same rotation each
    /// segment applies (parallel transport). Without it, rebuilding a
    /// perpendicular from a fixed world axis makes the tube spin where the
    /// branch happens to align with that axis.
    normal: Vec3,
    radius: f32,
}

struct Builder<'a> {
    tree: &'a Tree,
    rng: u32,
    bark: MeshData,
    leaves: MeshData,
}

impl Builder<'_> {
    /// Grow one branch and everything it carries.
    fn grow(&mut self, depth: u32, base: Vec3, axis: Vec3, normal: Vec3, length: f32, radius: f32) {
        let tree = self.tree;
        let segments = tree.segments.max(1);
        let step = length / segments as f32;

        let tip_radius = radius * tree.taper;
        let mut nodes: Vec<Node> = Vec::with_capacity(segments as usize + 1);
        let mut axis = axis.normalize_or(Vec3::Y);
        let mut normal = orthonormalize(normal, axis);
        let mut position = base;

        for index in 0..=segments {
            let t = index as f32 / segments as f32;
            // Taper on a power curve, not a straight line. A trunk loses
            // radius because it sheds branches, so it stays near full
            // thickness through the bare part and thins fast once it is
            // branching; interpolating linearly instead draws a carrot.
            let mut r = lerp(radius, tip_radius, t.powf(TAPER_CURVE));
            if depth == 0 && tree.flare > 0.0 {
                // Root buttress: a widening over the bottom fifth of the
                // trunk, quadratic so it swells rather than steps.
                let f = (1.0 - t / FLARE_HEIGHT).max(0.0);
                r *= 1.0 + tree.flare * f * f;
            }
            nodes.push(Node {
                position,
                axis,
                normal,
                radius: r,
            });
            if index == segments {
                break;
            }

            // Wander, then bend back toward the light. Both are per meter, so
            // the same parameters describe the same tree at any `segments`.
            if tree.crook > 0.0 {
                let azimuth = self.unit() * std::f32::consts::TAU;
                let bend = tree.crook * step * (self.unit() * 2.0 - 1.0);
                let binormal = axis.cross(normal);
                let pivot = azimuth.cos() * normal + azimuth.sin() * binormal;
                let rotation = Quat::from_axis_angle(pivot, bend.to_radians());
                axis = rotation * axis;
                normal = rotation * normal;
            }
            // A trunk's crook is a random walk, and a random walk with nothing
            // pulling on it drifts: at `crook: 18` a six-meter trunk can end
            // up growing sideways, and which seeds do that is pure luck. Real
            // trunks wander around vertical because gravitropism keeps
            // returning them to it, so the trunk gives back a fixed fraction
            // of whatever lean it has accumulated, every segment. That bounds
            // the wander without straightening it, and it is what makes one
            // seed as usable as another.
            if depth == 0 {
                let pivot = axis.cross(Vec3::Y);
                if pivot.length_squared() > 1e-8 {
                    let lean = axis.dot(Vec3::Y).clamp(-1.0, 1.0).acos();
                    let rotation = Quat::from_axis_angle(pivot.normalize(), lean * TRUNK_UPRIGHT);
                    axis = rotation * axis;
                    normal = rotation * normal;
                }
            }

            // Tropism bends a branch toward the sky (positive) or lets gravity
            // pull it down (negative) — and it is a *branch* behaviour, never
            // the trunk's. Applying it at depth 0 makes the trunk unstable:
            // one degree of crook tips it off vertical, and a negative tropism
            // then bends it further off, every segment, until the tree grows
            // sideways. A trunk's line is its crook alone; the whole point of
            // a trunk is that it is what the branches answer to.
            if tree.tropism != 0.0 && depth > 0 {
                let target = if tree.tropism > 0.0 {
                    Vec3::Y
                } else {
                    Vec3::NEG_Y
                };
                let pivot = axis.cross(target);
                if pivot.length_squared() > 1e-8 {
                    // Never overshoot: a branch already pointing at the target
                    // has arrived, and rotating past it would swing it back.
                    let remaining = axis.dot(target).clamp(-1.0, 1.0).acos();
                    let turn = (tree.tropism.abs() * step).to_radians().min(remaining);
                    let rotation = Quat::from_axis_angle(pivot.normalize(), turn);
                    axis = rotation * axis;
                    normal = rotation * normal;
                }
            }
            axis = axis.normalize_or(Vec3::Y);
            normal = orthonormalize(normal, axis);
            position += axis * step;
        }

        self.emit_tube(&nodes);

        if depth < tree.levels && tree.branches > 0 {
            self.emit_children(depth, &nodes, length);
        } else {
            self.emit_leaves(&nodes);
        }
    }

    /// Attach this branch's children and recurse into them.
    fn emit_children(&mut self, depth: u32, nodes: &[Node], length: f32) {
        let tree = self.tree;
        let count = tree.branches;
        // Whorls are a property of the *trunk*, not of branching in general: a
        // spruce puts out a ring of limbs at one height, but the shoots on
        // those limbs are ordinary alternate ones. Compounding whorl at every
        // level is also how a plausible conifer turns into a hundred thousand
        // vertices — `whorl: 5` would be 25 children per node, then 125.
        let whorl = if depth == 0 { tree.whorl.max(1) } else { 1 };
        for index in 0..count {
            // Spread the attachment points over the branch above
            // `branch_start`, biased so the last one sits below the very tip
            // (a child growing out of the tip is a fork, not a branch).
            let span = 1.0 - tree.branch_start;
            let t = tree.branch_start
                + span * (index as f32 + 0.5) / count as f32
                + span * self.jitter_signed() * 0.1;
            let t = t.clamp(tree.branch_start, 0.98);

            let at = sample(nodes, t);
            let base_azimuth = (tree.branch_twist * index as f32).to_radians();

            for slot in 0..whorl {
                let azimuth = base_azimuth
                    + slot as f32 * std::f32::consts::TAU / whorl as f32
                    + self.jitter_signed() * 0.5;
                let binormal = at.axis.cross(at.normal);
                let radial =
                    (azimuth.cos() * at.normal + azimuth.sin() * binormal).normalize_or(at.normal);

                let angle = (tree.branch_angle * self.jitter_multiplier()).to_radians();
                let child_axis =
                    (at.axis * angle.cos() + radial * angle.sin()).normalize_or(at.axis);
                let child_normal = orthonormalize(radial, child_axis);

                let child_length = (length
                    * tree.length_ratio
                    * (1.0 - tree.length_falloff * t)
                    * self.jitter_multiplier())
                .max(1e-3);
                // Never thicker than what carries it: a child that outgrows
                // its parent reads as a mushroom.
                let child_radius = (at.radius * tree.radius_ratio * self.jitter_multiplier())
                    .clamp(1e-4, at.radius * 0.9);

                // Start just inside the parent's surface so the join has no
                // seam and no gap; the tubes interpenetrate, which is
                // invisible and far cheaper than a real union.
                let origin = at.position + radial * at.radius * 0.7;

                self.grow(
                    depth + 1,
                    origin,
                    child_axis,
                    child_normal,
                    child_length,
                    child_radius,
                );
            }
        }
    }

    /// Sweep a tube along a branch's polyline, capped at both ends.
    ///
    /// Winding: with `normal × binormal == axis` right-handed, walking the ring
    /// in increasing angle and stepping along the axis gives outward-facing
    /// triangles — pinned by `every_wall_triangle_faces_outward`.
    fn emit_tube(&mut self, nodes: &[Node]) {
        let sides = self.tree.sides.max(3);
        let ring_start = self.bark.positions.len() as u32;

        for (index, node) in nodes.iter().enumerate() {
            let v = index as f32 / (nodes.len() - 1).max(1) as f32;
            let binormal = node.axis.cross(node.normal);
            for side in 0..sides {
                let angle = side as f32 / sides as f32 * std::f32::consts::TAU;
                let radial =
                    (angle.cos() * node.normal + angle.sin() * binormal).normalize_or(node.normal);
                self.bark
                    .positions
                    .push((node.position + radial * node.radius).to_array());
                self.bark.normals.push(radial.to_array());
                self.bark.uvs.push([side as f32 / sides as f32, v]);
            }
        }

        for ring in 0..nodes.len() as u32 - 1 {
            for side in 0..sides {
                let next = (side + 1) % sides;
                let a = ring_start + ring * sides + side;
                let b = ring_start + ring * sides + next;
                let c = ring_start + (ring + 1) * sides + next;
                let d = ring_start + (ring + 1) * sides + side;
                self.bark.indices.extend_from_slice(&[a, b, c, a, c, d]);
            }
        }

        let first = nodes[0];
        let last = nodes[nodes.len() - 1];
        self.emit_cap(first, ring_start, sides, false);
        self.emit_cap(
            last,
            ring_start + (nodes.len() as u32 - 1) * sides,
            sides,
            true,
        );
    }

    /// Close one end of a tube with a fan. `outward` picks which way the cap
    /// faces: the base looks back down its branch, the tip looks along it.
    fn emit_cap(&mut self, node: Node, ring_start: u32, sides: u32, outward: bool) {
        let normal = if outward { node.axis } else { -node.axis };
        let center = self.bark.positions.len() as u32;
        self.bark.positions.push(node.position.to_array());
        self.bark.normals.push(normal.to_array());
        self.bark.uvs.push([0.5, if outward { 1.0 } else { 0.0 }]);

        for side in 0..sides {
            let next = (side + 1) % sides;
            let (a, b) = (ring_start + side, ring_start + next);
            if outward {
                self.bark.indices.extend_from_slice(&[center, a, b]);
            } else {
                self.bark.indices.extend_from_slice(&[center, b, a]);
            }
        }
    }

    /// Scatter leaves over an outermost branch.
    fn emit_leaves(&mut self, nodes: &[Node]) {
        let tree = self.tree;
        if tree.leaf == TreeLeaf::None || tree.leaves_per_branch == 0 || tree.leaf_size <= 0.0 {
            return;
        }

        for index in 0..tree.leaves_per_branch {
            // Foliage lives on the outer two thirds of a shoot; the base of it
            // is shaded in a real tree and bare in this one.
            let t = (LEAF_START
                + (1.0 - LEAF_START) * (index as f32 + 0.5) / tree.leaves_per_branch as f32
                + self.jitter_signed() * 0.15)
                .clamp(0.0, 1.0);
            let at = sample(nodes, t);

            let azimuth = (LEAF_TWIST * index as f32).to_radians() + self.jitter_signed();
            let binormal = at.axis.cross(at.normal);
            let radial =
                (azimuth.cos() * at.normal + azimuth.sin() * binormal).normalize_or(at.normal);

            // Leaves stand off the shoot at a wide angle and lift toward the
            // sky — a petiole's job. Both are jittered, or the foliage draws a
            // visible helix.
            let out = (LEAF_ANGLE * self.jitter_multiplier()).to_radians();
            let mut direction = (at.axis * out.cos() + radial * out.sin()).normalize_or(Vec3::Y);
            direction = (direction + Vec3::Y * LEAF_LIFT).normalize_or(Vec3::Y);

            let size = tree.leaf_size * self.jitter_multiplier();
            let roll = self.unit() * std::f32::consts::TAU;
            let origin = at.position + radial * at.radius;

            match tree.leaf {
                TreeLeaf::Blade => self.emit_blade(origin, direction, roll, size),
                TreeLeaf::Cluster => self.emit_cluster(origin, direction, roll, size),
                TreeLeaf::None => {}
            }
        }
    }

    /// One leaf: a midrib with two wings folded down along it.
    ///
    /// The fold is the whole point. A flat card lit by one sun is a single
    /// value of green, and a canopy of them flickers between "all lit" and
    /// "all dark" as the camera moves; two wings at a dihedral catch the light
    /// at different angles and the canopy gets texture from shading alone —
    /// which matters here because the engine has no alpha-cut leaf textures to
    /// get it from. Emitted twice with opposite winding and normals, since
    /// backface culling is on and a leaf has two sides.
    fn emit_blade(&mut self, origin: Vec3, direction: Vec3, roll: f32, size: f32) {
        let (side, fold) = leaf_frame(direction, roll);
        let width = size * LEAF_WIDTH;

        let base = origin;
        let tip = origin + direction * size;
        let left = origin + direction * (size * 0.4) - side * width - fold * (width * LEAF_FOLD);
        let right = origin + direction * (size * 0.4) + side * width - fold * (width * LEAF_FOLD);

        for (a, b, c) in [(base, left, tip), (base, tip, right)] {
            push_triangle(&mut self.leaves, a, b, c);
            push_triangle(&mut self.leaves, a, c, b);
        }
    }

    /// A foliage blob: an octahedron with radial normals, squashed along the
    /// shoot. Six vertices for a unit of cover a leaf cannot give — which is
    /// what conifer sprays and background trees actually need.
    fn emit_cluster(&mut self, origin: Vec3, direction: Vec3, roll: f32, size: f32) {
        let (side, fold) = leaf_frame(direction, roll);
        let center = origin + direction * (size * 0.5);
        let radius = size * 0.5;

        let axes = [
            direction * radius * CLUSTER_STRETCH,
            side * radius,
            fold * radius,
        ];
        let base = self.leaves.positions.len() as u32;
        let mut corners = Vec::with_capacity(6);
        for axis in axes {
            corners.push(axis);
            corners.push(-axis);
        }
        for corner in &corners {
            self.leaves.positions.push((center + *corner).to_array());
            self.leaves
                .normals
                .push(corner.normalize_or(Vec3::Y).to_array());
            self.leaves.uvs.push([0.5, 0.5]);
        }

        // Eight faces, each joining one end of each axis. The parity of the
        // three choices decides the winding, so it is computed rather than
        // tabulated.
        for x in 0..2u32 {
            for y in 0..2u32 {
                for z in 0..2u32 {
                    let (a, b, c) = (base + x, base + 2 + y, base + 4 + z);
                    if (x + y + z) % 2 == 0 {
                        self.leaves.indices.extend_from_slice(&[a, b, c]);
                    } else {
                        self.leaves.indices.extend_from_slice(&[a, c, b]);
                    }
                }
            }
        }
    }

    // ── randomness ─────────────────────────────────────────────────────

    fn unit(&mut self) -> f32 {
        unit(&mut self.rng)
    }

    /// A multiplier in `[1 - jitter, 1 + jitter]`. Always consumes exactly one
    /// draw, even at `jitter: 0` — unlike the particle emitter, no tree
    /// baseline predates this field, so keeping the draw sequence independent
    /// of the parameter values is the simpler contract to hold.
    fn jitter_multiplier(&mut self) -> f32 {
        1.0 + (self.unit() * 2.0 - 1.0) * self.tree.jitter
    }

    /// A signed offset in `[-jitter, jitter]`, for quantities that are added
    /// rather than scaled.
    fn jitter_signed(&mut self) -> f32 {
        (self.unit() * 2.0 - 1.0) * self.tree.jitter
    }
}

/// Exponent of the radius taper along a branch; `1` would be linear.
const TAPER_CURVE: f32 = 1.6;
/// Fraction of its accumulated lean the trunk gives back each segment.
const TRUNK_UPRIGHT: f32 = 0.3;
/// Fraction of the trunk over which the root flare fades out.
const FLARE_HEIGHT: f32 = 0.2;
/// Fraction of a shoot left bare before leaves start.
const LEAF_START: f32 = 0.3;
/// Degrees of spin between successive leaves — the golden angle again.
const LEAF_TWIST: f32 = 137.5;
/// Degrees a leaf stands off its shoot.
const LEAF_ANGLE: f32 = 62.0;
/// How strongly a leaf turns toward the sky after standing off.
const LEAF_LIFT: f32 = 0.45;
/// Half-width of a blade as a fraction of its length.
const LEAF_WIDTH: f32 = 0.34;
/// How far the wings fold below the midrib, as a fraction of half-width.
const LEAF_FOLD: f32 = 0.55;
/// How far a cluster is drawn out along its shoot.
const CLUSTER_STRETCH: f32 = 1.6;

/// A frame across a leaf's growth direction: the width axis, and the direction
/// its wings fold toward. `roll` spins the leaf about its own midrib.
fn leaf_frame(direction: Vec3, roll: f32) -> (Vec3, Vec3) {
    let reference = if direction.y.abs() > 0.9 {
        Vec3::X
    } else {
        Vec3::Y
    };
    let side = direction.cross(reference).normalize_or(Vec3::X);
    let fold = direction.cross(side).normalize_or(Vec3::Z);
    let rotation = Quat::from_axis_angle(direction, roll);
    (rotation * side, rotation * fold)
}

/// Append a flat-shaded triangle. Leaves are flat-shaded on purpose: a blade
/// is two facets, and averaging their normals would erase the fold that makes
/// it read.
fn push_triangle(mesh: &mut MeshData, a: Vec3, b: Vec3, c: Vec3) {
    let normal = (b - a).cross(c - a).normalize_or(Vec3::Y);
    let base = mesh.positions.len() as u32;
    for (vertex, uv) in [(a, [0.5, 0.0]), (b, [0.0, 1.0]), (c, [1.0, 1.0])] {
        mesh.positions.push(vertex.to_array());
        mesh.normals.push(normal.to_array());
        mesh.uvs.push(uv);
    }
    mesh.indices.extend_from_slice(&[base, base + 1, base + 2]);
}

/// The point `t` of the way along a branch's polyline, with its frame.
fn sample(nodes: &[Node], t: f32) -> Node {
    let last = nodes.len() - 1;
    if last == 0 {
        return nodes[0];
    }
    let scaled = (t.clamp(0.0, 1.0) * last as f32).min(last as f32 - 1e-4);
    let index = scaled.floor() as usize;
    let f = scaled - index as f32;
    let (a, b) = (nodes[index], nodes[index + 1]);
    Node {
        position: a.position.lerp(b.position, f),
        axis: a.axis.lerp(b.axis, f).normalize_or(a.axis),
        normal: orthonormalize(
            a.normal.lerp(b.normal, f),
            a.axis.lerp(b.axis, f).normalize_or(a.axis),
        ),
        radius: lerp(a.radius, b.radius, f),
    }
}

/// The component of `v` perpendicular to `axis`, normalized — Gram-Schmidt,
/// with a fallback for the degenerate case where they are parallel.
fn orthonormalize(v: Vec3, axis: Vec3) -> Vec3 {
    let projected = v - axis * v.dot(axis);
    if projected.length_squared() > 1e-8 {
        projected.normalize()
    } else {
        let reference = if axis.x.abs() < 0.9 { Vec3::X } else { Vec3::Y };
        (reference - axis * reference.dot(axis)).normalize()
    }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

// ── randomness ─────────────────────────────────────────────────────────────

/// Same splitmix-style finalizer and xorshift the particle system uses, and
/// duplicated for the same reason it is written out there: the sequence is part
/// of what a scene file *means*, so it may not live in a dependency.
fn seed_state(seed: u32) -> u32 {
    let mut z = seed.wrapping_add(0x9E37_79B9);
    z = (z ^ (z >> 16)).wrapping_mul(0x21F0_AAAD);
    z = (z ^ (z >> 15)).wrapping_mul(0x735A_2D97);
    z ^= z >> 15;
    if z == 0 {
        0x9E37_79B9
    } else {
        z
    }
}

fn next(state: &mut u32) -> u32 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *state = x;
    x
}

/// Uniform in `[0, 1)`, from the generator's top 24 bits.
fn unit(state: &mut u32) -> f32 {
    (next(state) >> 8) as f32 / 16_777_216.0
}

// ── the cache ──────────────────────────────────────────────────────────────

/// A [`Tree`]'s exact field bits. Exact rather than hashed: a hash collision
/// would silently draw the wrong tree, and the whole array is 26 words.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct TreeKey([u32; 26]);

impl TreeKey {
    fn of(tree: &Tree) -> Self {
        Self([
            tree.seed,
            tree.height.to_bits(),
            tree.trunk_radius.to_bits(),
            tree.levels,
            tree.branches,
            tree.whorl,
            tree.branch_angle.to_bits(),
            tree.branch_twist.to_bits(),
            tree.branch_start.to_bits(),
            tree.length_ratio.to_bits(),
            tree.length_falloff.to_bits(),
            tree.radius_ratio.to_bits(),
            tree.taper.to_bits(),
            tree.flare.to_bits(),
            tree.crook.to_bits(),
            tree.tropism.to_bits(),
            tree.jitter.to_bits(),
            tree.sides,
            tree.segments,
            tree.leaf as u32,
            tree.leaf_size.to_bits(),
            tree.leaves_per_branch,
            tree.leaf_color.x.to_bits(),
            tree.leaf_color.y.to_bits(),
            tree.leaf_color.z.to_bits(),
            tree.leaf_roughness.to_bits(),
        ])
    }
}

thread_local! {
    /// Generated geometry is a pure function of the component, so a
    /// process-local cache is not hidden state (invariant 2) any more than
    /// `mesh.rs`'s builtin cache is: nothing in it can differ from what the
    /// file says.
    static TREE_CACHE: RefCell<HashMap<TreeKey, TreeMeshes>> = RefCell::new(HashMap::new());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The signed volume of a closed mesh, by the divergence theorem. Positive
    /// means the triangles wind counter-clockwise seen from outside — which is
    /// what wgpu's default front face, and this engine's backface culling,
    /// require. A tree wound the other way renders as nothing at all.
    fn signed_volume(mesh: &MeshData) -> f32 {
        let point = |i: u32| Vec3::from(mesh.positions[i as usize]);
        mesh.indices
            .chunks_exact(3)
            .map(|t| point(t[0]).dot(point(t[1]).cross(point(t[2]))) / 6.0)
            .sum()
    }

    fn bare_pole() -> Tree {
        Tree {
            levels: 0,
            leaf: TreeLeaf::None,
            crook: 0.0,
            tropism: 0.0,
            flare: 0.0,
            taper: 1.0,
            jitter: 0.0,
            height: 4.0,
            trunk_radius: 0.5,
            sides: 16,
            segments: 1,
            ..Tree::default()
        }
    }

    #[test]
    fn a_bare_pole_is_a_closed_cylinder_of_the_right_size() {
        let (bark, leaves) = generate(&bare_pole());
        assert!(leaves.indices.is_empty(), "no leaves were asked for");

        // A 16-gon prism, not a cylinder: its volume is the inscribed
        // polygon's, a few percent under πr²h.
        let expected = 16.0 * (std::f32::consts::TAU / 16.0).sin() / 2.0 * 0.25 * 4.0;
        let volume = signed_volume(&bark);
        assert!(
            (volume - expected).abs() < 0.01,
            "expected ~{expected}, got {volume}"
        );
    }

    #[test]
    fn every_wall_triangle_faces_outward() {
        // The winding pin. Volume alone can hide a mesh that is inside-out in
        // one place and compensating elsewhere, so check each wall face
        // against the axis it was swept around.
        let tree = Tree {
            segments: 4,
            ..bare_pole()
        };
        let (bark, _) = generate(&tree);
        let point = |i: u32| Vec3::from(bark.positions[i as usize]);

        let on_axis = |p: Vec3| Vec3::new(p.x, 0.0, p.z).length() < 1e-4;
        let mut walls = 0;
        for triangle in bark.indices.chunks_exact(3) {
            let (a, b, c) = (point(triangle[0]), point(triangle[1]), point(triangle[2]));
            if on_axis(a) || on_axis(b) || on_axis(c) {
                continue; // a cap fan touches the axis; caps are checked below
            }
            walls += 1;
            let face = (b - a).cross(c - a);
            let center = (a + b + c) / 3.0;
            let radial = Vec3::new(center.x, 0.0, center.z);
            assert!(
                face.dot(radial) > 0.0,
                "a wall triangle at {center:?} faces inward"
            );
        }
        assert_eq!(walls, 4 * 16 * 2, "4 segments x 16 sides x 2 triangles");

        // And the caps: the bottom looks down, the top looks up.
        let normals: Vec<Vec3> = bark.normals.iter().map(|n| Vec3::from(*n)).collect();
        assert!(normals.iter().any(|n| n.y < -0.99), "no downward base cap");
        assert!(normals.iter().any(|n| n.y > 0.99), "no upward tip cap");
    }

    #[test]
    fn the_same_tree_grows_the_same_mesh_and_a_new_seed_does_not() {
        let tree = Tree::default();
        let (first, _) = generate(&tree);
        let (again, _) = generate(&tree);
        assert_eq!(
            first, again,
            "generation is a pure function of the component"
        );

        let (other, _) = generate(&Tree { seed: 1, ..tree });
        assert_eq!(
            first.positions.len(),
            other.positions.len(),
            "same parameters, same vertex budget"
        );
        assert_ne!(
            first.positions, other.positions,
            "a different seed must grow a different tree"
        );
    }

    #[test]
    fn no_jitter_and_no_crook_leaves_nothing_for_the_seed_to_change() {
        // The diagram case, useful when authoring a species: turn off the two
        // sources of randomness in the woody structure and the seed stops
        // mattering. (Leaf roll is deliberately not one of them — there is no
        // authored roll for `jitter` to vary from.)
        let tree = Tree {
            jitter: 0.0,
            crook: 0.0,
            leaf: TreeLeaf::None,
            ..Tree::default()
        };
        let (a, _) = generate(&tree);
        let (b, _) = generate(&Tree { seed: 99, ..tree });
        assert_eq!(
            a.positions, b.positions,
            "no jitter, no crook: no randomness"
        );
    }

    #[test]
    fn vertex_count_predicts_what_generation_produces() {
        // Validation refuses a tree on this number before anything is
        // allocated, so it has to be the truth and not an estimate.
        for tree in [
            Tree::default(),
            Tree {
                levels: 0,
                ..Tree::default()
            },
            Tree {
                levels: 3,
                whorl: 2,
                branches: 3,
                ..Tree::default()
            },
            Tree {
                leaf: TreeLeaf::Cluster,
                leaves_per_branch: 3,
                ..Tree::default()
            },
            Tree {
                leaf: TreeLeaf::None,
                ..Tree::default()
            },
            Tree {
                branches: 0,
                ..Tree::default()
            },
        ] {
            let (bark, leaves) = generate(&tree);
            let actual = (bark.positions.len() + leaves.positions.len()) as u64;
            assert_eq!(vertex_count(&tree), actual, "for {tree:?}");
        }
    }

    #[test]
    fn a_tree_stays_inside_a_plausible_envelope() {
        // Catches the class of bug where tropism or crook runs away and a
        // branch shoots off to infinity — visible as a spike across the whole
        // scene, and easy to miss in a thumbnail.
        let tree = Tree {
            levels: 3,
            crook: 20.0,
            tropism: 20.0,
            ..Tree::default()
        };
        let (bark, leaves) = generate(&tree);
        let mut lowest = f32::MAX;
        let mut reach: f32 = 0.0;
        for position in bark.positions.iter().chain(leaves.positions.iter()) {
            let p = Vec3::from(*position);
            lowest = lowest.min(p.y);
            reach = reach.max(p.length());
        }
        assert!(
            lowest > -0.01,
            "the trunk grew below its own base: {lowest}"
        );
        assert!(
            reach < tree.height * 2.0,
            "a branch left the tree's envelope: {reach} for a {}m tree",
            tree.height
        );
    }

    /// How far the trunk's tip has strayed from straight up, in degrees.
    fn trunk_lean(tree: &Tree) -> f32 {
        let (bark, _) = generate(tree);
        // The trunk's ring vertices come first, and its last ring is the
        // `sides` vertices before the two cap centers.
        let sides = tree.sides as usize;
        let rings = tree.segments as usize + 1;
        let ring = |i: usize| -> Vec3 {
            let mut sum = Vec3::ZERO;
            for s in 0..sides {
                sum += Vec3::from(bark.positions[i * sides + s]);
            }
            sum / sides as f32
        };
        let direction = (ring(rings - 1) - ring(0)).normalize();
        direction.dot(Vec3::Y).clamp(-1.0, 1.0).acos().to_degrees()
    }

    #[test]
    fn the_trunk_stays_near_vertical_however_gnarled() {
        // A random walk with nothing pulling on it drifts, and which seeds
        // topple would be pure luck — the whole point of the uprighting term.
        // Checked across seeds, because one seed proves nothing about a walk.
        let tree = Tree {
            crook: 25.0,
            segments: 12,
            levels: 0,
            leaf: TreeLeaf::None,
            ..Tree::default()
        };
        for seed in 0..40 {
            let lean = trunk_lean(&Tree { seed, ..tree });
            assert!(lean < 30.0, "seed {seed} leaned {lean}° off vertical");
        }
    }

    #[test]
    fn a_drooping_tropism_does_not_topple_the_trunk() {
        // The bug this rule came from: tropism at depth 0 is unstable, since a
        // degree of crook gives a negative tropism something to amplify. The
        // trunk of a spruce must stand up.
        let spruce = Tree {
            tropism: -16.0,
            crook: 6.0,
            levels: 1,
            leaf: TreeLeaf::None,
            ..Tree::default()
        };
        for seed in 0..20 {
            let lean = trunk_lean(&Tree { seed, ..spruce });
            assert!(lean < 20.0, "seed {seed} drooped its trunk {lean}° over");
        }

        // And the branches genuinely do droop, or the rule has cost us the
        // feature it was protecting.
        let (bark, _) = generate(&spruce);
        let lowest = bark.positions.iter().map(|p| p[1]).fold(f32::MAX, f32::min);
        let attachment = spruce.height * spruce.branch_start;
        assert!(
            lowest < attachment,
            "no branch dipped below its own attachment ({lowest} vs {attachment})"
        );
    }

    #[test]
    fn whorls_are_a_trunk_property() {
        // Compounding a whorl at every level multiplies the tree by itself:
        // `whorl: 5` would be 25 children per node and then 125. Botanically
        // it is also wrong — a spruce's limbs carry ordinary alternate shoots.
        let alternate = Tree {
            whorl: 1,
            levels: 2,
            ..Tree::default()
        };
        let whorled = Tree {
            whorl: 4,
            ..alternate
        };
        let ratio = vertex_count(&whorled) as f64 / vertex_count(&alternate) as f64;
        assert!(
            (3.5..4.5).contains(&ratio),
            "a whorl of 4 should cost about 4x, not {ratio}x"
        );
        // And it is the real geometry, not just the prediction.
        assert_eq!(
            vertex_count(&whorled),
            generate(&whorled).0.positions.len() as u64
                + generate(&whorled).1.positions.len() as u64
        );
    }

    #[test]
    fn leaves_are_double_sided() {
        // Backface culling is on, so a single-sided leaf is invisible from
        // half of every camera position. Every blade triangle must have a
        // twin with the opposite winding.
        let tree = Tree {
            levels: 0,
            leaves_per_branch: 4,
            ..Tree::default()
        };
        let (_, leaves) = generate(&tree);
        let mut sum = Vec3::ZERO;
        for normal in &leaves.normals {
            sum += Vec3::from(*normal);
        }
        assert!(
            sum.length() < 1e-3,
            "blade normals should cancel in pairs, got {sum:?}"
        );
    }

    #[test]
    fn the_cache_hands_back_one_arc_per_distinct_tree() {
        // The renderer's upload cache and `MeshSource`'s contract both key on
        // `Arc` identity; a fresh copy per frame would re-upload every tree in
        // the scene every frame.
        let tree = Tree {
            seed: 4242,
            ..Tree::default()
        };
        let first = meshes_for(&tree);
        let again = meshes_for(&tree);
        assert!(Arc::ptr_eq(&first.bark, &again.bark));

        let other = meshes_for(&Tree { seed: 4243, ..tree });
        assert!(!Arc::ptr_eq(&first.bark, &other.bark));
    }

    #[test]
    fn a_bare_tree_has_no_leaf_mesh_to_draw() {
        let none = meshes_for(&Tree {
            leaf: TreeLeaf::None,
            ..Tree::default()
        });
        assert!(none.leaves.is_none());
        let empty = meshes_for(&Tree {
            leaves_per_branch: 0,
            ..Tree::default()
        });
        assert!(empty.leaves.is_none());
        let leafy = meshes_for(&Tree::default());
        assert!(leafy.leaves.is_some());
    }
}
