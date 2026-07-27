//! Geometry: built-in primitives, and the resolution of `Mesh.asset` strings.
//!
//! Mesh data lives here rather than in `engine-render` so it stays testable
//! without a GPU and so `engine validate` can resolve asset references without
//! linking wgpu. Reading actual glTF files needs the `gltf` crate and lives in
//! `engine-assets`; this module owns everything about an asset *reference* that
//! can be decided without parsing the file — is it a builtin, is the path
//! relative, does the file exist, is the extension one the engine reads.

use std::path::{Path, PathBuf};

use glam::Vec3;

use crate::error::{EngineError, Result};

/// CPU-side geometry, ready to upload.
///
/// Positions, normals, and uvs are parallel arrays of the same length;
/// `indices` refers into them. UVs are carried from M3 so glTF files load them
/// once; nothing samples a texture until M4.
#[derive(Debug, Clone, PartialEq)]
pub struct MeshData {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
}

impl MeshData {
    pub fn vertex_count(&self) -> usize {
        self.positions.len()
    }

    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }
}

/// A primitive the engine can produce without loading anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinMesh {
    Cube,
    Cylinder,
    Plane,
    Sphere,
    Triangle,
}

impl BuiltinMesh {
    /// Prefix marking an asset reference as built-in rather than a file path.
    pub const PREFIX: &'static str = "builtin:";

    /// Every built-in's full asset string, for errors and suggestions.
    pub const ASSETS: &'static [&'static str] = &[
        "builtin:cube",
        "builtin:cylinder",
        "builtin:plane",
        "builtin:sphere",
        "builtin:triangle",
    ];

    /// Parse a `builtin:` reference. `None` means the string is not a builtin
    /// reference at all (it is a file path); `Some(Err)` means it claims to be
    /// one but names no known primitive.
    pub fn parse(asset: &str) -> Option<Result<Self>> {
        let name = asset.strip_prefix(Self::PREFIX)?;
        Some(match name {
            "cube" => Ok(Self::Cube),
            "cylinder" => Ok(Self::Cylinder),
            "plane" => Ok(Self::Plane),
            "sphere" => Ok(Self::Sphere),
            "triangle" => Ok(Self::Triangle),
            _ => Err(EngineError::new(
                crate::codes::ASSET_NOT_FOUND,
                format!("no built-in mesh named {name:?}"),
            )
            .field("asset")
            .suggest_from(asset, Self::ASSETS.iter().copied())),
        })
    }

    pub fn data(self) -> MeshData {
        match self {
            Self::Cube => cube(),
            Self::Cylinder => cylinder(),
            Self::Plane => plane(),
            Self::Sphere => sphere(),
            Self::Triangle => triangle(),
        }
    }
}

/// File extensions the mesh loader reads, lowercase.
pub const MESH_EXTENSIONS: &[&str] = &["gltf", "glb"];

/// A `Mesh.asset` reference, resolved as far as it can be without opening the
/// file.
///
/// This is the single seam between "what a scene says" and "what is on disk":
/// `engine validate` calls [`MeshAsset::resolve`] to reject bad references
/// before render time, and `engine-assets` calls it to decide what to load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MeshAsset {
    Builtin(BuiltinMesh),
    /// A mesh file, as `base_dir` + the reference's relative path.
    File(PathBuf),
}

impl MeshAsset {
    /// Resolve an asset reference against the directory of the scene file that
    /// contains it (invariant 3: assets are referenced by relative path).
    ///
    /// Rejects, in order of usefulness to an agent: unknown builtins, absolute
    /// paths, extensions the loader does not read, and files that do not
    /// exist. A missing file suggests near-miss names from the directory it
    /// should have been in, alongside the builtins.
    pub fn resolve(asset: &str, base_dir: &Path) -> Result<Self> {
        if let Some(builtin) = BuiltinMesh::parse(asset) {
            return Ok(Self::Builtin(builtin?));
        }

        if Path::new(asset).is_absolute() {
            return Err(EngineError::new(
                crate::codes::ASSET_PATH_NOT_RELATIVE,
                format!(
                    "mesh asset {asset:?} is an absolute path; assets are referenced \
                     by path relative to the scene file, so scenes stay portable"
                ),
            )
            .field("asset"));
        }

        let resolved = base_dir.join(asset);

        match resolved.extension().and_then(|e| e.to_str()) {
            Some(ext) if MESH_EXTENSIONS.contains(&ext.to_lowercase().as_str()) => {}
            _ => {
                return Err(EngineError::new(
                    crate::codes::ASSET_UNSUPPORTED,
                    format!(
                        "mesh asset {asset:?} is not a format the engine reads; \
                         use a .gltf or .glb file, or one of {}",
                        BuiltinMesh::ASSETS.join(", ")
                    ),
                )
                .field("asset"));
            }
        }

        if !resolved.is_file() {
            let candidates = sibling_candidates(asset, &resolved);
            return Err(EngineError::new(
                crate::codes::ASSET_NOT_FOUND,
                format!(
                    "no mesh file at {} (asset paths resolve relative to the scene file)",
                    resolved.display()
                ),
            )
            .field("asset")
            .suggest_from(asset, candidates.iter().map(String::as_str)));
        }

        Ok(Self::File(resolved))
    }
}

/// Things a missing asset reference could plausibly have meant: every builtin,
/// plus every file actually present in the directory the reference points
/// into, spelled the way the scene would spell it.
fn sibling_candidates(asset: &str, resolved: &Path) -> Vec<String> {
    let mut candidates: Vec<String> = BuiltinMesh::ASSETS.iter().map(|s| s.to_string()).collect();

    let Some(dir) = resolved.parent() else {
        return candidates;
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return candidates;
    };

    let prefix = Path::new(asset).parent().filter(|p| !p.as_os_str().is_empty());
    for entry in entries.flatten() {
        if !entry.path().is_file() {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        candidates.push(match prefix {
            Some(prefix) => format!("{}/{name}", prefix.display()),
            None => name.to_string(),
        });
    }

    candidates
}

/// Anything that can turn a `Mesh.asset` string into geometry.
///
/// `Scene::render_items` takes one of these, which is what keeps `engine-core`
/// free of glTF parsing: the real file-reading implementation lives in
/// `engine-assets`, and GPU-less contexts (unit tests, validation) use
/// [`BuiltinAssets`].
pub trait MeshSource {
    fn load_mesh(&self, asset: &str) -> Result<MeshData>;
}

/// A [`MeshSource`] that resolves only `builtin:` primitives — for tests and
/// other contexts with no asset directory to load from.
pub struct BuiltinAssets;

impl MeshSource for BuiltinAssets {
    fn load_mesh(&self, asset: &str) -> Result<MeshData> {
        match BuiltinMesh::parse(asset) {
            Some(builtin) => Ok(builtin?.data()),
            None => Err(EngineError::new(
                crate::codes::ASSET_NOT_FOUND,
                format!(
                    "cannot load {asset:?}: only {} are available in this context \
                     (mesh files load through engine-assets)",
                    BuiltinMesh::ASSETS.join(", ")
                ),
            )
            .field("asset")
            .suggest_from(asset, BuiltinMesh::ASSETS.iter().copied())),
        }
    }
}

/// Build a quad facing `normal`, wound counter-clockwise seen from outside.
///
/// `u` and `v` must satisfy `cross(u, v) == normal`; that constraint is what
/// makes the winding come out right for every face without a hand-checked
/// vertex table.
fn quad(normal: Vec3, u: Vec3, v: Vec3, mesh: &mut MeshData) {
    debug_assert!(
        (u.cross(v) - normal).length() < 1e-5,
        "cross(u, v) must equal the normal, or the face winds backwards"
    );

    let center = normal * 0.5;
    let (u, v) = (u * 0.5, v * 0.5);
    let base = mesh.positions.len() as u32;

    for (corner, uv) in [
        (center - u - v, [0.0, 0.0]),
        (center + u - v, [1.0, 0.0]),
        (center + u + v, [1.0, 1.0]),
        (center - u + v, [0.0, 1.0]),
    ] {
        mesh.positions.push(corner.to_array());
        mesh.normals.push(normal.to_array());
        mesh.uvs.push(uv);
    }

    mesh.indices
        .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
}

/// Unit cube centered on the origin, with flat per-face normals.
fn cube() -> MeshData {
    let mut mesh = MeshData {
        positions: Vec::with_capacity(24),
        normals: Vec::with_capacity(24),
        uvs: Vec::with_capacity(24),
        indices: Vec::with_capacity(36),
    };

    let (x, y, z) = (Vec3::X, Vec3::Y, Vec3::Z);
    quad(x, y, z, &mut mesh);
    quad(-x, z, y, &mut mesh);
    quad(y, z, x, &mut mesh);
    quad(-y, x, z, &mut mesh);
    quad(z, x, y, &mut mesh);
    quad(-z, y, x, &mut mesh);

    mesh
}

/// Unit quad in the XZ plane, facing up.
fn plane() -> MeshData {
    let mut mesh = MeshData {
        positions: Vec::with_capacity(4),
        normals: Vec::with_capacity(4),
        uvs: Vec::with_capacity(4),
        indices: Vec::with_capacity(6),
    };
    // Offset back to the origin: `quad` pushes the face out along its normal.
    quad(Vec3::Y, Vec3::Z, Vec3::X, &mut mesh);
    for position in &mut mesh.positions {
        position[1] -= 0.5;
    }
    mesh
}

/// UV sphere, unit radius, centered on the origin: the lighting probe.
///
/// Smooth normals equal to the normalized positions — the property that makes
/// a sphere ideal for judging specular response in a screenshot, and the
/// reason this primitive exists (roughness and Fresnel are invisible on flat
/// faces). 32 segments × 16 rings is plenty at screenshot resolutions.
fn sphere() -> MeshData {
    const SEGMENTS: u32 = 32;
    const RINGS: u32 = 16;

    let vertex_count = ((RINGS + 1) * (SEGMENTS + 1)) as usize;
    let mut mesh = MeshData {
        positions: Vec::with_capacity(vertex_count),
        normals: Vec::with_capacity(vertex_count),
        uvs: Vec::with_capacity(vertex_count),
        indices: Vec::with_capacity((RINGS * SEGMENTS * 6) as usize),
    };

    // The seam column is duplicated (segment 0 == segment SEGMENTS) so UVs can
    // wrap without interpolating backwards across the seam.
    for ring in 0..=RINGS {
        // Polar angle from the +Y pole.
        let theta = std::f32::consts::PI * ring as f32 / RINGS as f32;
        for segment in 0..=SEGMENTS {
            let phi = std::f32::consts::TAU * segment as f32 / SEGMENTS as f32;
            let position = Vec3::new(
                theta.sin() * phi.cos(),
                theta.cos(),
                theta.sin() * phi.sin(),
            );
            mesh.positions.push(position.to_array());
            mesh.normals.push(position.to_array());
            mesh.uvs.push([
                segment as f32 / SEGMENTS as f32,
                ring as f32 / RINGS as f32,
            ]);
        }
    }

    let columns = SEGMENTS + 1;
    for ring in 0..RINGS {
        for segment in 0..SEGMENTS {
            let a = ring * columns + segment;
            let b = a + 1;
            let c = a + columns;
            let d = c + 1;

            // Two triangles per quad, wound counter-clockwise seen from
            // outside; the pole rows each collapse one triangle to a line, so
            // skip those rather than emit degenerates.
            if ring != 0 {
                mesh.indices.extend_from_slice(&[a, b, c]);
            }
            if ring != RINGS - 1 {
                mesh.indices.extend_from_slice(&[b, d, c]);
            }
        }
    }

    mesh
}

/// Cylinder along Y, radius 0.5 and height 1, so like the cube it fits the
/// unit extent and `Transform.scale` means the same thing on both.
///
/// The side is smooth-shaded (radial normals, seam column duplicated for UV
/// wrap, like the sphere); the caps are flat-shaded triangle fans with their
/// own vertices so the silhouette edge stays sharp.
fn cylinder() -> MeshData {
    const SEGMENTS: u32 = 32;
    const RADIUS: f32 = 0.5;
    const HALF_HEIGHT: f32 = 0.5;

    let vertex_count = (2 * (SEGMENTS + 1) + 2 * (SEGMENTS + 1)) as usize;
    let mut mesh = MeshData {
        positions: Vec::with_capacity(vertex_count),
        normals: Vec::with_capacity(vertex_count),
        uvs: Vec::with_capacity(vertex_count),
        indices: Vec::with_capacity((SEGMENTS * 6 + SEGMENTS * 6) as usize),
    };

    let rim = |segment: u32| {
        let phi = std::f32::consts::TAU * segment as f32 / SEGMENTS as f32;
        (phi.cos(), phi.sin())
    };

    // Side: two rings of shared vertices, top row first so v runs 0 at the
    // top like the sphere's.
    for segment in 0..=SEGMENTS {
        let (cos, sin) = rim(segment);
        let u = segment as f32 / SEGMENTS as f32;
        for (y, v) in [(HALF_HEIGHT, 0.0), (-HALF_HEIGHT, 1.0)] {
            mesh.positions.push([RADIUS * cos, y, RADIUS * sin]);
            mesh.normals.push([cos, 0.0, sin]);
            mesh.uvs.push([u, v]);
        }
    }
    for segment in 0..SEGMENTS {
        let top = segment * 2;
        let (bottom, next_top, next_bottom) = (top + 1, top + 2, top + 3);
        // Counter-clockwise seen from outside (radially outward).
        mesh.indices
            .extend_from_slice(&[bottom, top, next_bottom, next_bottom, top, next_top]);
    }

    // Caps: a center vertex and its own rim ring per cap, flat ±Y normals.
    for (y, normal_y) in [(HALF_HEIGHT, 1.0), (-HALF_HEIGHT, -1.0)] {
        let center = mesh.positions.len() as u32;
        mesh.positions.push([0.0, y, 0.0]);
        mesh.normals.push([0.0, normal_y, 0.0]);
        mesh.uvs.push([0.5, 0.5]);
        for segment in 0..=SEGMENTS {
            let (cos, sin) = rim(segment);
            mesh.positions.push([RADIUS * cos, y, RADIUS * sin]);
            mesh.normals.push([0.0, normal_y, 0.0]);
            mesh.uvs.push([0.5 + cos * 0.5, 0.5 + sin * 0.5]);
        }
        for segment in 0..SEGMENTS {
            let (a, b) = (center + 1 + segment, center + 2 + segment);
            // The top cap winds (center, next, current) seen from +Y; the
            // bottom cap mirrors.
            if normal_y > 0.0 {
                mesh.indices.extend_from_slice(&[center, b, a]);
            } else {
                mesh.indices.extend_from_slice(&[center, a, b]);
            }
        }
    }

    mesh
}

/// The M0 triangle, kept as a primitive so the oldest render path stays
/// reachable from a scene file.
fn triangle() -> MeshData {
    MeshData {
        positions: vec![[-0.8, -0.6, 0.0], [0.8, -0.6, 0.0], [0.0, 0.8, 0.0]],
        normals: vec![[0.0, 0.0, 1.0]; 3],
        uvs: vec![[0.0, 0.0], [1.0, 0.0], [0.5, 1.0]],
        indices: vec![0, 1, 2],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cube_has_flat_shaded_faces() {
        let cube = BuiltinMesh::Cube.data();
        assert_eq!(cube.vertex_count(), 24, "4 verts per face, not shared");
        assert_eq!(cube.triangle_count(), 12);
        assert_eq!(cube.normals.len(), cube.positions.len());
    }

    #[test]
    fn cube_faces_all_wind_outward() {
        // If any face were wound backwards it would be culled and the cube
        // would render with a hole. Check the geometric normal of each triangle
        // agrees with the direction from the cube's center.
        let cube = BuiltinMesh::Cube.data();

        for triangle in cube.indices.chunks(3) {
            let [a, b, c] = [
                Vec3::from_array(cube.positions[triangle[0] as usize]),
                Vec3::from_array(cube.positions[triangle[1] as usize]),
                Vec3::from_array(cube.positions[triangle[2] as usize]),
            ];

            let geometric = (b - a).cross(c - a).normalize();
            let outward = ((a + b + c) / 3.0).normalize();

            assert!(
                geometric.dot(outward) > 0.5,
                "a face winds inward: normal {geometric:?} vs outward {outward:?}"
            );
        }
    }

    #[test]
    fn cube_spans_the_unit_extent() {
        let cube = BuiltinMesh::Cube.data();
        for axis in 0..3 {
            let min = cube
                .positions
                .iter()
                .map(|p| p[axis])
                .fold(f32::MAX, f32::min);
            let max = cube
                .positions
                .iter()
                .map(|p| p[axis])
                .fold(f32::MIN, f32::max);
            assert_eq!((min, max), (-0.5, 0.5));
        }
    }

    #[test]
    fn plane_sits_on_the_origin() {
        let plane = BuiltinMesh::Plane.data();
        assert!(plane.positions.iter().all(|p| p[1] == 0.0));
        assert_eq!(plane.normals[0], [0.0, 1.0, 0.0]);
    }

    #[test]
    fn every_primitive_keeps_its_arrays_parallel() {
        for builtin in [
            BuiltinMesh::Cube,
            BuiltinMesh::Cylinder,
            BuiltinMesh::Plane,
            BuiltinMesh::Sphere,
            BuiltinMesh::Triangle,
        ] {
            let mesh = builtin.data();
            assert_eq!(mesh.normals.len(), mesh.positions.len(), "{builtin:?}");
            assert_eq!(mesh.uvs.len(), mesh.positions.len(), "{builtin:?}");
        }
    }

    #[test]
    fn sphere_is_unit_radius_with_normals_matching_positions() {
        let sphere = BuiltinMesh::Sphere.data();
        assert_eq!(sphere.vertex_count(), 17 * 33, "(rings+1) x (segments+1)");
        assert_eq!(
            sphere.triangle_count(),
            2 * 32 * 16 - 2 * 32,
            "two per quad minus one per pole quad"
        );

        for (position, normal) in sphere.positions.iter().zip(&sphere.normals) {
            let p = Vec3::from_array(*position);
            assert!(
                (p.length() - 1.0).abs() < 1e-5,
                "every vertex sits on the unit sphere, got {p:?}"
            );
            assert_eq!(position, normal, "smooth normals = normalized positions");
        }
    }

    #[test]
    fn sphere_triangles_wind_outward() {
        // Same check as the cube: an inward-wound triangle would be culled,
        // leaving a hole that renders as background — invisible, not loud.
        let sphere = BuiltinMesh::Sphere.data();
        for triangle in sphere.indices.chunks(3) {
            let [a, b, c] = [
                Vec3::from_array(sphere.positions[triangle[0] as usize]),
                Vec3::from_array(sphere.positions[triangle[1] as usize]),
                Vec3::from_array(sphere.positions[triangle[2] as usize]),
            ];
            let geometric = (b - a).cross(c - a);
            assert!(
                geometric.length() > 1e-7,
                "degenerate triangle survived pole skipping: {a:?} {b:?} {c:?}"
            );
            let outward = (a + b + c) / 3.0;
            assert!(
                geometric.dot(outward) > 0.0,
                "a sphere face winds inward: {a:?} {b:?} {c:?}"
            );
        }
    }

    #[test]
    fn cylinder_fits_the_unit_extent() {
        let cylinder = BuiltinMesh::Cylinder.data();
        for position in &cylinder.positions {
            let radial = (position[0] * position[0] + position[2] * position[2]).sqrt();
            assert!(radial <= 0.5 + 1e-5, "radius stays 0.5, got {radial}");
            assert!(
                position[1] == 0.5 || position[1] == -0.5,
                "every vertex sits on a cap plane, got y={}",
                position[1]
            );
        }
    }

    #[test]
    fn cylinder_side_normals_are_radial_and_caps_flat() {
        let cylinder = BuiltinMesh::Cylinder.data();
        for (position, normal) in cylinder.positions.iter().zip(&cylinder.normals) {
            let n = Vec3::from_array(*normal);
            assert!((n.length() - 1.0).abs() < 1e-5, "unit normal, got {n:?}");
            if normal[1] == 0.0 {
                // Side: the normal is the position's radial direction.
                let radial = Vec3::new(position[0], 0.0, position[2]).normalize();
                assert!((n - radial).length() < 1e-5, "{n:?} vs {radial:?}");
            } else {
                assert_eq!(n.abs(), Vec3::Y, "caps are flat ±Y");
            }
        }
    }

    #[test]
    fn cylinder_triangles_wind_outward() {
        // Same stakes as the cube and sphere checks: an inward winding is
        // culled into an invisible hole, not an error.
        let cylinder = BuiltinMesh::Cylinder.data();
        for triangle in cylinder.indices.chunks(3) {
            let [a, b, c] = [
                Vec3::from_array(cylinder.positions[triangle[0] as usize]),
                Vec3::from_array(cylinder.positions[triangle[1] as usize]),
                Vec3::from_array(cylinder.positions[triangle[2] as usize]),
            ];
            let geometric = (b - a).cross(c - a);
            assert!(
                geometric.length() > 1e-7,
                "degenerate triangle: {a:?} {b:?} {c:?}"
            );
            let outward = (a + b + c) / 3.0;
            assert!(
                geometric.dot(outward) > 0.0,
                "a cylinder face winds inward: {a:?} {b:?} {c:?}"
            );
        }
    }

    #[test]
    fn resolves_builtin_assets() {
        assert_eq!(
            MeshAsset::resolve("builtin:cube", Path::new("")).unwrap(),
            MeshAsset::Builtin(BuiltinMesh::Cube)
        );
    }

    #[test]
    fn suggests_a_near_miss_builtin() {
        let err = MeshAsset::resolve("builtin:cuve", Path::new("")).unwrap_err();
        assert_eq!(err.error, "asset_not_found");
        assert_eq!(
            err.context().unwrap().did_you_mean.as_deref(),
            Some("builtin:cube")
        );
    }

    /// A directory containing one mesh file, torn down on drop.
    struct AssetDir(PathBuf);

    impl AssetDir {
        fn with_file(test: &str, file: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("engine-mesh-{test}-{}", std::process::id()));
            std::fs::create_dir_all(dir.join("meshes")).unwrap();
            std::fs::write(dir.join(file), b"{}").unwrap();
            Self(dir)
        }
    }

    impl Drop for AssetDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn resolves_an_existing_mesh_file() {
        let dir = AssetDir::with_file("exists", "meshes/pyramid.gltf");
        let resolved = MeshAsset::resolve("meshes/pyramid.gltf", &dir.0).unwrap();
        assert_eq!(resolved, MeshAsset::File(dir.0.join("meshes/pyramid.gltf")));
    }

    #[test]
    fn rejects_an_absolute_asset_path() {
        let err = MeshAsset::resolve("/etc/meshes/cube.glb", Path::new("")).unwrap_err();
        assert_eq!(err.error, "asset_path_not_relative");
    }

    #[test]
    fn rejects_an_extension_the_loader_does_not_read() {
        let err = MeshAsset::resolve("meshes/cube.obj", Path::new("")).unwrap_err();
        assert_eq!(err.error, "asset_unsupported");
        assert!(err.message.contains(".gltf"), "{}", err.message);
    }

    #[test]
    fn missing_file_suggests_a_sibling_from_its_directory() {
        let dir = AssetDir::with_file("suggest", "meshes/pyramid.gltf");
        let err = MeshAsset::resolve("meshes/pyramod.gltf", &dir.0).unwrap_err();
        assert_eq!(err.error, "asset_not_found");
        assert_eq!(
            err.context().unwrap().did_you_mean.as_deref(),
            Some("meshes/pyramid.gltf")
        );
    }

    #[test]
    fn builtin_source_loads_builtins_and_refuses_files() {
        assert_eq!(
            BuiltinAssets.load_mesh("builtin:plane").unwrap(),
            BuiltinMesh::Plane.data()
        );
        let err = BuiltinAssets.load_mesh("meshes/cube.glb").unwrap_err();
        assert_eq!(err.error, "asset_not_found");
    }
}
