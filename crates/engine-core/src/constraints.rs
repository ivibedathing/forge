//! Region constraints: the properties face adjacency cannot state (M49).
//!
//! A socket is a statement about one *interface*. "This mass of walls encloses a
//! room", "the street reaches everywhere", "a building is smaller than the
//! village" are statements about a **region**, and constraint propagation over
//! adjacency has no representation for one. The M47 village came out as a single
//! 60-cell mass covering 75% of its grid; the tour's hamlet as 24 wall pieces
//! enclosing zero rooms.
//!
//! # What is evaluated
//!
//! The **ground layer** (`y == 0`), 4-connected in XZ. Buildings, streets and
//! courtyards are ground-plane properties; extending regions up through the
//! storeys would make a roof part of its building's region, turning every size
//! bound into a function of how many storeys the *grid* has — a scene decision,
//! while the constraint is a tileset one.
//!
//! A terrace step does not split a region. The lift moves geometry, and two
//! columns at different lifts are still neighbours on the ground plan; making a
//! step split a building would mean a village on a slope could never satisfy a
//! size bound.
//!
//! # How they are enforced
//!
//! By **rejection**, in `synthesize`: a block whose result violates a constraint
//! is re-rolled, using the retry machinery M47 already has for contradictions.
//! Checking constraints *inside* propagation is the real version and a milestone
//! of its own — it has to decide, for every candidate tile in every cell,
//! whether choosing it could disconnect a region that is still half-undecided.

use crate::tileset::{Bounds, Constraint, ExpandedTile, Tileset};
use crate::error::{EngineError, Result};
use crate::tilelayout::Grid;

/// One constraint with its tile sets resolved to the expansion indices the
/// solver works in.
///
/// Prepared once per solve rather than per block: the membership test is a
/// bitmask lookup in the inner loop, and resolving authored names there would
/// put a string compare inside a flood fill.
#[derive(Debug, Clone, PartialEq)]
pub struct Prepared {
    pub label: String,
    /// Which expanded tiles belong to this constraint's set.
    members: Vec<bool>,
    /// Which belong to its `region_contains` set, when it has one.
    contained: Vec<bool>,
    count: Option<Bounds>,
    regions: Option<Bounds>,
    region_size: Option<Bounds>,
    contains: Option<Bounds>,
}

/// Every constraint a tileset declares, ready for the solver.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Rules(pub Vec<Prepared>);

impl Rules {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Resolve a tileset's constraints against its expansion.
    ///
    /// Errors on a name no tile carries, which is the mistake that would
    /// otherwise make a constraint quietly cover nothing.
    pub fn prepare(tileset: &Tileset, tiles: &[ExpandedTile]) -> Result<Rules> {
        let mut prepared = Vec::new();
        for constraint in &tileset.constraints {
            let members = membership(tileset, tiles, &constraint.tiles, constraint)?;
            let contained = match &constraint.region_contains {
                Some(contains) => membership(tileset, tiles, &contains.tiles, constraint)?,
                None => vec![false; tiles.len()],
            };
            prepared.push(Prepared {
                label: constraint.label(),
                members,
                contained,
                count: constraint.count,
                regions: constraint.regions,
                region_size: constraint.region_size,
                contains: constraint.region_contains.as_ref().map(|c| Bounds {
                    min: c.min,
                    max: c.max,
                }),
            });
        }
        Ok(Rules(prepared))
    }
}

fn membership(
    tileset: &Tileset,
    tiles: &[ExpandedTile],
    names: &[String],
    constraint: &Constraint,
) -> Result<Vec<bool>> {
    let mut members = vec![false; tiles.len()];
    for name in names {
        if !tileset.tiles.iter().any(|tile| &tile.name == name) {
            return Err(EngineError::new(
                crate::codes::UNKNOWN_TILE,
                format!(
                    "{} names the tile {name:?}, which this tileset does not define",
                    constraint.label()
                ),
            )
            .suggest_from(name, tileset.tiles.iter().map(|t| t.name.as_str())));
        }
        for (index, tile) in tiles.iter().enumerate() {
            if tileset.tiles[tile.tile].name == *name {
                members[index] = true;
            }
        }
    }
    Ok(members)
}

/// One broken rule, and the cells to blame for it.
#[derive(Debug, Clone, PartialEq)]
pub struct Violation {
    /// Index into the tileset's `constraints`.
    pub constraint: usize,
    /// What went wrong, in the words the report prints.
    pub detail: String,
    /// The cells the violation is *about*, so a block can be judged on the
    /// consequences of its own choices and not on its neighbours'.
    pub cells: Vec<usize>,
}

/// Every constraint a grid breaks.
///
/// `cells` holds one resolved tile index per cell, in the grid's own order.
pub fn evaluate(rules: &Rules, grid: &Grid, cells: &[usize]) -> Vec<Violation> {
    let mut violations = Vec::new();
    for (index, rule) in rules.0.iter().enumerate() {
        let members = ground_members(grid, cells, &rule.members);

        if let Some(bounds) = rule.count {
            if !bounds.admits(members.len()) {
                violations.push(Violation {
                    constraint: index,
                    detail: format!(
                        "{} of them, and there must be {}",
                        members.len(),
                        bounds.describe()
                    ),
                    cells: members.clone(),
                });
            }
        }

        if rule.regions.is_none() && rule.region_size.is_none() && rule.contains.is_none() {
            continue;
        }
        let regions = label(grid, &members);

        if let Some(bounds) = rule.regions {
            if !bounds.admits(regions.len()) {
                violations.push(Violation {
                    constraint: index,
                    detail: format!(
                        "they form {} separate regions, and there must be {}",
                        regions.len(),
                        bounds.describe()
                    ),
                    cells: members.clone(),
                });
            }
        }

        for region in &regions {
            if let Some(bounds) = rule.region_size {
                if !bounds.admits(region.len()) {
                    violations.push(Violation {
                        constraint: index,
                        detail: format!(
                            "a region of {} cells, and each must be {}",
                            region.len(),
                            bounds.describe()
                        ),
                        cells: region.clone(),
                    });
                }
            }
            if let Some(bounds) = rule.contains {
                let held = region
                    .iter()
                    .filter(|cell| rule.contained[cells[**cell]])
                    .count();
                if !bounds.admits(held) {
                    violations.push(Violation {
                        constraint: index,
                        detail: format!(
                            "a region of {} cells holding {held} of what it must hold {} of",
                            region.len(),
                            bounds.describe()
                        ),
                        cells: region.clone(),
                    });
                }
            }
        }
    }
    violations
}

/// The ground-layer cells belonging to a set, in grid order.
fn ground_members(grid: &Grid, cells: &[usize], members: &[bool]) -> Vec<usize> {
    let [nx, _, nz] = grid.size;
    let mut out = Vec::new();
    for z in 0..nz {
        for x in 0..nx {
            let index = grid.index(x, 0, z);
            if cells.get(index).is_some_and(|tile| members[*tile]) {
                out.push(index);
            }
        }
    }
    out
}

/// 4-connected regions over the ground plan, each in grid order.
///
/// Plain XZ adjacency rather than [`Grid::neighbour`]: that function treats a
/// terrace step as a free edge, which is right for *sockets* and wrong here — a
/// building does not stop being one building because it stands on two levels.
fn label(grid: &Grid, members: &[usize]) -> Vec<Vec<usize>> {
    let [nx, _, nz] = grid.size;
    let mut inside = vec![false; grid.cell_count()];
    for cell in members {
        inside[*cell] = true;
    }

    let mut seen = vec![false; grid.cell_count()];
    let mut regions = Vec::new();
    for start in members {
        if seen[*start] {
            continue;
        }
        seen[*start] = true;
        let mut stack = vec![*start];
        let mut region = Vec::new();
        while let Some(cell) = stack.pop() {
            region.push(cell);
            let (x, _, z) = grid.coords(cell);
            for (dx, dz) in [(1i32, 0i32), (-1, 0), (0, 1), (0, -1)] {
                let (tx, tz) = (x as i64 + dx as i64, z as i64 + dz as i64);
                if tx < 0 || tz < 0 || tx >= i64::from(nx) || tz >= i64::from(nz) {
                    continue;
                }
                let next = grid.index(tx as u32, 0, tz as u32);
                if inside[next] && !seen[next] {
                    seen[next] = true;
                    stack.push(next);
                }
            }
        }
        // Grid order, so a region's reported cells do not depend on the order
        // the flood fill happened to pop them in.
        region.sort_unstable();
        regions.push(region);
    }
    regions
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tileset::{Faces, TileDef};
    use glam::Vec3;

    /// Three tiles, no geometry: this module only ever looks at which tile is
    /// in which cell.
    fn tileset(constraints: Vec<Constraint>) -> Tileset {
        let tile = |name: &str| TileDef {
            name: name.into(),
            weight: 1.0,
            rotations: 1,
            faces: Faces::default(),
            parts: Vec::new(),
        };
        Tileset {
            cell: Vec3::splat(2.0),
            palette: Default::default(),
            tiles: vec![tile("open"), tile("wall"), tile("floor")],
            constraints,
        }
    }

    /// A grid from a picture: `.` open, `#` wall, `o` floor. Rows are z.
    fn grid_of(rows: &[&str]) -> (Grid, Vec<usize>) {
        let nz = rows.len() as u32;
        let nx = rows[0].len() as u32;
        let grid = Grid {
            size: [nx, 1, nz],
            offsets: Vec::new(),
        };
        let mut cells = vec![0usize; grid.cell_count()];
        for (z, row) in rows.iter().enumerate() {
            for (x, glyph) in row.chars().enumerate() {
                cells[grid.index(x as u32, 0, z as u32)] = match glyph {
                    '#' => 1,
                    'o' => 2,
                    _ => 0,
                };
            }
        }
        (grid, cells)
    }

    fn rules(constraints: Vec<Constraint>) -> (Tileset, Rules) {
        let tileset = tileset(constraints);
        let tiles = crate::tileset::expand(&tileset).unwrap();
        let rules = Rules::prepare(&tileset, &tiles).unwrap();
        (tileset, rules)
    }

    fn constraint(tiles: &[&str]) -> Constraint {
        Constraint {
            name: String::new(),
            tiles: tiles.iter().map(|t| t.to_string()).collect(),
            count: None,
            regions: None,
            region_size: None,
            region_contains: None,
        }
    }

    #[test]
    fn regions_are_four_connected_and_diagonals_do_not_join_them() {
        let (_, rules) = rules(vec![Constraint {
            regions: Some(Bounds {
                min: None,
                max: Some(1),
            }),
            ..constraint(&["wall"])
        }]);
        // Two blocks touching only at a corner are two buildings.
        let (grid, cells) = grid_of(&["#..", ".#.", "..."]);
        let broken = evaluate(&rules, &grid, &cells);
        assert_eq!(broken.len(), 1);
        assert!(broken[0].detail.contains("2 separate regions"), "{broken:?}");

        // Sharing an edge makes them one.
        let (grid, cells) = grid_of(&["##.", "...", "..."]);
        assert!(evaluate(&rules, &grid, &cells).is_empty());
    }

    /// The predicate that breaks one sprawling mass into buildings — the M47
    /// village was a single region of 60.
    #[test]
    fn region_size_refuses_a_mass_and_a_lone_cell() {
        let (_, rules) = rules(vec![Constraint {
            region_size: Some(Bounds {
                min: Some(2),
                max: Some(4),
            }),
            ..constraint(&["wall"])
        }]);

        let (grid, cells) = grid_of(&["####", "####", "...."]);
        let broken = evaluate(&rules, &grid, &cells);
        assert_eq!(broken.len(), 1);
        assert!(broken[0].detail.contains("region of 8 cells"), "{broken:?}");

        let (grid, cells) = grid_of(&["#...", "....", "...."]);
        assert!(
            evaluate(&rules, &grid, &cells)[0].detail.contains("1 cells"),
            "a wall standing on its own is not a building"
        );

        let (grid, cells) = grid_of(&["##..", "##..", "...."]);
        assert!(evaluate(&rules, &grid, &cells).is_empty(), "four is allowed");
    }

    /// The predicate that fixes the tour's hamlet: 24 wall pieces, zero rooms.
    #[test]
    fn region_contains_demands_a_room_in_every_building() {
        let (_, rules) = rules(vec![Constraint {
            region_contains: Some(crate::tileset::RegionContains {
                tiles: vec!["floor".into()],
                min: Some(1),
                max: None,
            }),
            ..constraint(&["wall", "floor"])
        }]);

        let (grid, cells) = grid_of(&["###", "###", "..."]);
        let broken = evaluate(&rules, &grid, &cells);
        assert_eq!(broken.len(), 1, "no floor anywhere in the mass");

        let (grid, cells) = grid_of(&["###", "#o#", "###"]);
        assert!(evaluate(&rules, &grid, &cells).is_empty());

        // Two buildings, one of them hollow: only the hollow one is reported,
        // and the cells it blames are its own.
        let (grid, cells) = grid_of(&["#o#.###", "###.###", "......."]);
        let broken = evaluate(&rules, &grid, &cells);
        assert_eq!(broken.len(), 1);
        assert!(broken[0].cells.iter().all(|c| grid.coords(*c).0 >= 4));
    }

    #[test]
    fn count_bounds_the_whole_grid() {
        let (_, rules) = rules(vec![Constraint {
            count: Some(Bounds {
                min: Some(2),
                max: Some(3),
            }),
            ..constraint(&["floor"])
        }]);
        let (grid, cells) = grid_of(&["oooo", "....", "...."]);
        assert!(evaluate(&rules, &grid, &cells)[0].detail.contains("4 of them"));

        let (grid, cells) = grid_of(&["o...", "....", "...."]);
        assert!(evaluate(&rules, &grid, &cells)[0].detail.contains("1 of them"));

        let (grid, cells) = grid_of(&["oo..", "....", "...."]);
        assert!(evaluate(&rules, &grid, &cells).is_empty());
    }

    /// A grid nothing is placed on satisfies a `min` on regions vacuously, and
    /// that is the honest reading: zero regions is not "one region that is too
    /// small".
    #[test]
    fn an_empty_set_has_no_regions_to_judge() {
        let (_, rules) = rules(vec![Constraint {
            region_size: Some(Bounds {
                min: Some(4),
                max: None,
            }),
            ..constraint(&["wall"])
        }]);
        let (grid, cells) = grid_of(&["....", "....", "...."]);
        assert!(evaluate(&rules, &grid, &cells).is_empty());
    }

    /// A step in the terrain does not cut a building in half.
    #[test]
    fn a_terrace_step_does_not_split_a_region() {
        let (_, rules) = rules(vec![Constraint {
            regions: Some(Bounds {
                min: None,
                max: Some(1),
            }),
            ..constraint(&["wall"])
        }]);
        let (mut grid, cells) = grid_of(&["####"]);
        grid.offsets = vec![0, 0, 1, 1];
        assert!(
            evaluate(&rules, &grid, &cells).is_empty(),
            "the lift moves geometry, not the ground plan"
        );
    }

    #[test]
    fn a_constraint_naming_no_tile_is_an_error_with_a_suggestion() {
        let tileset = tileset(vec![constraint(&["wal"])]);
        let tiles = crate::tileset::expand(&tileset).unwrap();
        let error = Rules::prepare(&tileset, &tiles).unwrap_err();
        assert_eq!(error.error, crate::codes::UNKNOWN_TILE);
        assert_eq!(
            error.context().and_then(|c| c.did_you_mean.as_deref()),
            Some("wall")
        );
    }

    #[test]
    fn a_tileset_with_no_constraints_prepares_to_nothing() {
        let (_, rules) = rules(Vec::new());
        assert!(rules.is_empty());
        let (grid, cells) = grid_of(&["####"]);
        assert!(evaluate(&rules, &grid, &cells).is_empty());
    }
}
