//! A solved grid as geometry (M47).
//!
//! Turns a `TileGrid` plus its tileset and layout into the meshes the renderer
//! draws and the one rapier collides. Two products from one pass, because they
//! want different splits:
//!
//! - **Per palette material**, for drawing. Each becomes one ordinary
//!   `RenderItem` on the existing mesh path — `Tree` already emits two items
//!   for one entity, so nothing new is needed to allow it, and a six-material
//!   village is six draws. No shader is edited and no pipeline is added, which
//!   keeps `mesh.wgsl`'s four ULP-sensitive lines out of the blast radius.
//! - **All of it merged**, for physics: `GeneratedSurface` is one mesh per
//!   entity name, so the palette split is a rendering product only.
//!
//! # Placement
//!
//! The grid is centred on its entity in X and Z and bottom-anchored in Y. A
//! column that follows a `Terrain` is lifted by whole cells (see
//! `tilelayout::Grid`), which is what makes the faces between two columns of a
//! terrace flush rather than leaving a slot down every wall.
//!
//! # The cache is a correctness-of-performance contract
//!
//! The renderer keys its uploads on `Arc::as_ptr` (M15), so handing it a fresh
//! `Arc` every frame re-uploads the entire village every frame. `shard.rs` says
//! this outright and `terrain.rs` follows the same shape; the key here names
//! every input that changes a vertex.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use glam::Vec3;

use crate::components::TileGrid;
use crate::mesh::MeshData;
use crate::tilelayout::Grid;
use crate::tileset::{ExpandedTile, Tileset};

/// Vertices one grid may grow.
///
/// A `Terrain` at 512 segments is ~263k, and a thousand-cell village at a
/// hundred vertices a cell is ~100k, so this is roughly four times the largest
/// thing the engine builds today. The count is *exact* rather than an estimate,
/// because the layout says which tile is in which cell.
pub const MAX_TILE_GRID_VERTICES: usize = 1_000_000;

/// What a grid grows.
#[derive(Debug, Clone, PartialEq)]
pub struct TileGridSolid {
    /// One merged mesh per palette key any placed tile used, with the material
    /// that paints it, in palette order.
    ///
    /// `BTreeMap` order rather than hash order, for the reason `terrain_items`
    /// sorts by entity name: the draw list must not depend on how a map
    /// happened to iterate. The material is carried here rather than looked up
    /// later because the tileset is a *file*, and nothing downstream of the
    /// resolve pass has one.
    pub by_palette: Vec<(String, crate::components::Material, Arc<MeshData>)>,
    /// Every palette merged into one, for the trimesh collider.
    pub collision: Arc<MeshData>,
}

impl TileGridSolid {
    pub fn vertex_count(&self) -> usize {
        self.by_palette
            .iter()
            .map(|(_, _, mesh)| mesh.vertex_count())
            .sum()
    }
}

/// The cell's own origin in the grid's local space: its centre in X and Z, its
/// floor in Y, with the column's terrace lift applied.
pub fn cell_origin(tileset: &Tileset, grid: &Grid, index: usize) -> Vec3 {
    let [nx, _, nz] = grid.size;
    let (x, y, z) = grid.coords(index);
    let lift = grid.offset(x, z);
    Vec3::new(
        (x as f32 + 0.5 - nx as f32 * 0.5) * tileset.cell.x,
        (y as i64 + i64::from(lift)) as f32 * tileset.cell.y,
        (z as f32 + 0.5 - nz as f32 * 0.5) * tileset.cell.z,
    )
}

/// Vertices this grid would grow, exactly.
pub fn vertex_count(tileset: &Tileset, tiles: &[ExpandedTile], resolved: &[usize]) -> usize {
    resolved
        .iter()
        .map(|tile| crate::tileset::tile_vertex_count(tileset, &tiles[*tile]))
        .sum()
}

/// Build a grid's geometry, or hand back the copy already built.
pub fn solid_for(
    tileset: &Tileset,
    grid: &Grid,
    tiles: &[ExpandedTile],
    resolved: &[usize],
) -> Arc<TileGridSolid> {
    GRID_CACHE.with(|cache| {
        let key = GridKey::of(tileset, grid, tiles, resolved);
        if let Some(hit) = cache.borrow().get(&key) {
            return Arc::clone(hit);
        }
        let built = Arc::new(build(tileset, grid, tiles, resolved));
        let mut cache = cache.borrow_mut();
        // Bounded rather than an incidental leak — `cloud.rs`'s reasoning and
        // its three-line resolution. A grid is large, so the bound is small.
        if cache.len() >= MAX_CACHED_GRIDS {
            cache.clear();
        }
        cache.insert(key, Arc::clone(&built));
        built
    })
}

fn build(
    tileset: &Tileset,
    grid: &Grid,
    tiles: &[ExpandedTile],
    resolved: &[usize],
) -> TileGridSolid {
    let mut by_palette: BTreeMap<String, MeshData> = BTreeMap::new();
    // The grid's own order — y, then z, then x — so the merged buffer is a
    // function of the layout rather than of an iteration.
    for (index, tile) in resolved.iter().enumerate() {
        let origin = cell_origin(tileset, grid, index);
        crate::tileset::grow_tile(tileset, &tiles[*tile], origin, &mut by_palette);
    }
    // A palette entry no placed tile used grows nothing, and an empty mesh
    // would be an empty draw with a bind group behind it.
    by_palette.retain(|_, mesh| mesh.vertex_count() > 0);

    let mut collision = MeshData::default();
    for mesh in by_palette.values() {
        append(&mut collision, mesh);
    }

    TileGridSolid {
        by_palette: by_palette
            .into_iter()
            .map(|(key, mesh)| {
                let material = tileset.palette.get(&key).cloned().unwrap_or_default();
                (key, material, Arc::new(mesh))
            })
            .collect(),
        collision: Arc::new(collision),
    }
}

/// Concatenate `from` onto `into`, rebasing its indices.
fn append(into: &mut MeshData, from: &MeshData) {
    let base = into.positions.len() as u32;
    into.positions.extend_from_slice(&from.positions);
    into.normals.extend_from_slice(&from.normals);
    into.uvs.extend_from_slice(&from.uvs);
    into.indices.extend(from.indices.iter().map(|i| i + base));
}

const MAX_CACHED_GRIDS: usize = 8;

/// Every input that changes a vertex.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct GridKey {
    tileset: String,
    size: [u32; 3],
    offsets: Vec<i32>,
    cells: Vec<u32>,
}

impl GridKey {
    fn of(tileset: &Tileset, grid: &Grid, _tiles: &[ExpandedTile], resolved: &[usize]) -> Self {
        Self {
            // The tileset's content digest rather than the tileset: two grids
            // sharing a tileset share its geometry, and an edited tileset must
            // not hand back the old village.
            tileset: crate::tileset::digest(tileset),
            size: grid.size,
            offsets: grid.offsets.clone(),
            cells: resolved.iter().map(|c| *c as u32).collect(),
        }
    }
}

thread_local! {
    /// Generated geometry is a pure function of its inputs, so a process-local
    /// cache is not hidden state (invariant 2) any more than `mesh.rs`'s
    /// builtin cache is.
    static GRID_CACHE: RefCell<HashMap<GridKey, Arc<TileGridSolid>>> = RefCell::new(HashMap::new());
}

/// Which tile a name resolves to, for the two fill fields.
///
/// Empty means the tileset's first tile for the ground and its last for the
/// background, which is the arrangement a tileset authored ground-first
/// already has — so a `TileGrid` that names neither still solves.
pub fn fill_indices(
    component: &TileGrid,
    tileset: &Tileset,
    tiles: &[ExpandedTile],
) -> Result<(usize, usize), String> {
    let pick = |name: &str, fallback: usize| -> Result<usize, String> {
        if name.is_empty() {
            return Ok(fallback);
        }
        tiles
            .iter()
            .position(|t| tileset.tiles[t.tile].name == name && t.rotation == 0)
            .ok_or_else(|| name.to_string())
    };
    let last = tiles.len().saturating_sub(1);
    Ok((
        pick(&component.fill_ground, 0)?,
        pick(&component.fill_background, last)?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tileset::{Faces, Part, PartKind, Rise, TileDef};

    fn slab(material: &str) -> Part {
        Part {
            kind: PartKind::Box,
            at: Vec3::new(0.0, 0.1, 0.0),
            size: Vec3::new(2.0, 0.2, 2.0),
            rise: Rise::Px,
            yaw: 0.0,
            sides: 8,
            material: material.into(),
        }
    }

    fn tileset() -> Tileset {
        Tileset {
            cell: Vec3::new(2.0, 2.5, 2.0),
            palette: BTreeMap::from([
                ("stone".to_string(), Default::default()),
                ("wood".to_string(), Default::default()),
                ("never_used".to_string(), Default::default()),
            ]),
            tiles: vec![
                TileDef {
                    name: "stone".into(),
                    weight: 1.0,
                    rotations: 1,
                    faces: Faces::default(),
                    parts: vec![slab("stone")],
                },
                TileDef {
                    name: "wood".into(),
                    weight: 1.0,
                    rotations: 1,
                    faces: Faces::default(),
                    parts: vec![slab("wood")],
                },
                TileDef {
                    name: "air".into(),
                    weight: 1.0,
                    rotations: 1,
                    faces: Faces::default(),
                    parts: Vec::new(),
                },
            ],
            constraints: Vec::new(),
        }
    }

    fn grid() -> Grid {
        Grid {
            size: [4, 1, 4],
            offsets: Vec::new(),
        }
    }

    fn checkerboard(tiles: &[ExpandedTile], grid: &Grid) -> Vec<usize> {
        let _ = tiles;
        (0..grid.cell_count()).map(|i| i % 3).collect()
    }

    #[test]
    fn a_grid_draws_one_mesh_per_palette_entry_it_uses() {
        let tileset = tileset();
        let tiles = crate::tileset::expand(&tileset).unwrap();
        let grid = grid();
        let cells = checkerboard(&tiles, &grid);
        let solid = solid_for(&tileset, &grid, &tiles, &cells);

        let keys: Vec<&str> = solid.by_palette.iter().map(|(k, _, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            ["stone", "wood"],
            "palette order, and nothing for an entry no tile used"
        );
        assert_eq!(
            solid.vertex_count(),
            vertex_count(&tileset, &tiles, &cells),
            "the budget must be exact, not an estimate"
        );
    }

    #[test]
    fn the_collision_mesh_is_every_palette_merged() {
        let tileset = tileset();
        let tiles = crate::tileset::expand(&tileset).unwrap();
        let grid = grid();
        let cells = checkerboard(&tiles, &grid);
        let solid = solid_for(&tileset, &grid, &tiles, &cells);

        assert_eq!(solid.collision.vertex_count(), solid.vertex_count());
        assert_eq!(
            solid.collision.triangle_count(),
            solid
                .by_palette
                .iter()
                .map(|(_, _, m)| m.triangle_count())
                .sum::<usize>()
        );
        // Every index still points inside the merged buffer — the rebase is
        // the one thing a concatenation gets wrong, and rapier's answer to an
        // out-of-range index is a panic three stages later.
        let count = solid.collision.positions.len() as u32;
        assert!(solid.collision.indices.iter().all(|i| *i < count));
    }

    /// The renderer keys uploads on `Arc` identity, so this is a correctness
    /// contract for frame cost rather than an allocation saved.
    #[test]
    fn geometry_is_shared_across_calls() {
        let tileset = tileset();
        let tiles = crate::tileset::expand(&tileset).unwrap();
        let grid = grid();
        let cells = checkerboard(&tiles, &grid);
        assert!(Arc::ptr_eq(
            &solid_for(&tileset, &grid, &tiles, &cells),
            &solid_for(&tileset, &grid, &tiles, &cells)
        ));

        let mut moved = cells.clone();
        moved.swap(0, 1);
        assert!(!Arc::ptr_eq(
            &solid_for(&tileset, &grid, &tiles, &cells),
            &solid_for(&tileset, &grid, &tiles, &moved)
        ));

        // And an edited tileset must not hand back the old village.
        let mut edited = tileset.clone();
        edited.tiles[0].parts[0].size.y = 0.4;
        assert!(!Arc::ptr_eq(
            &solid_for(&tileset, &grid, &tiles, &cells),
            &solid_for(&edited, &grid, &tiles, &cells)
        ));
    }

    #[test]
    fn the_grid_is_centred_in_xz_and_stands_on_its_floor() {
        let tileset = tileset();
        let grid = Grid {
            size: [4, 2, 4],
            offsets: Vec::new(),
        };
        // Four cells of 2 m span 8 m, so the first cell's centre is at −3.
        assert_eq!(
            cell_origin(&tileset, &grid, grid.index(0, 0, 0)),
            Vec3::new(-3.0, 0.0, -3.0)
        );
        assert_eq!(
            cell_origin(&tileset, &grid, grid.index(3, 0, 3)),
            Vec3::new(3.0, 0.0, 3.0)
        );
        // Layer 1 sits a cell height up, not half of one.
        assert_eq!(
            cell_origin(&tileset, &grid, grid.index(0, 1, 0)).y,
            2.5,
            "bottom-anchored: a building stands on the ground"
        );
    }

    #[test]
    fn a_lifted_column_raises_its_whole_stack() {
        let tileset = tileset();
        let mut grid = Grid {
            size: [2, 2, 1],
            offsets: vec![0, 2],
        };
        assert_eq!(cell_origin(&tileset, &grid, grid.index(1, 0, 0)).y, 5.0);
        assert_eq!(cell_origin(&tileset, &grid, grid.index(1, 1, 0)).y, 7.5);
        grid.offsets = vec![0, -1];
        assert_eq!(cell_origin(&tileset, &grid, grid.index(1, 0, 0)).y, -2.5);
    }

    #[test]
    fn an_unnamed_fill_takes_the_first_and_last_tile() {
        let tileset = tileset();
        let tiles = crate::tileset::expand(&tileset).unwrap();
        let component = TileGrid::default();
        assert_eq!(fill_indices(&component, &tileset, &tiles), Ok((0, 2)));

        let named = TileGrid {
            fill_ground: "wood".into(),
            fill_background: "air".into(),
            ..TileGrid::default()
        };
        assert_eq!(fill_indices(&named, &tileset, &tiles), Ok((1, 2)));

        let wrong = TileGrid {
            fill_ground: "gravel".into(),
            ..TileGrid::default()
        };
        assert_eq!(
            fill_indices(&wrong, &tileset, &tiles),
            Err("gravel".to_string())
        );
    }
}
