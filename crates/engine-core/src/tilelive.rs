//! Re-solving a `TileGrid` while the scene is running (M50).
//!
//! M47 solves at the command line and commits the answer; the scene draws what
//! the file says. This module is the other entry point: a script asks for the
//! blocks near a world position to be solved again, and the grid's geometry is
//! regrown in place.
//!
//! # What is held, and why it is lazy
//!
//! A load resolves a grid's two files into a [`ResolvedTileGrid`] and drops
//! everything else. A runtime solve needs what was dropped — the expanded
//! tileset, the compatibility table, the rules, the terraced grid, the cells and
//! the locks — so [`LiveGrids`] keeps them, per entity, **from the first call
//! that names that entity**. A scene that never synthesizes at runtime reads
//! nothing twice and holds nothing extra, which is M37's `SpawnLedger` rule:
//! the feature costs the scenes that predate it exactly nothing.
//!
//! # This is not hidden state
//!
//! Invariant 2 asks that everything needed to reconstruct a scene live in text
//! files. The live cells do: they are a pure function of the committed layout,
//! the tileset, and the sequence of calls a script made against a fixed clock.
//! Replaying the scene reproduces them exactly. What is **not** true after the
//! first call is that the live cells are the *file's* cells — a run does not
//! write back, and `engine synthesize` remains the only thing that does.
//!
//! # Physics does not follow
//!
//! A grid carrying a `Collider` is refused rather than re-solved. Its trimesh is
//! built once, and rebuilding one mid-run means removing and re-inserting a
//! static collider — which perturbs the broad phase and moves every body in the
//! scene, the rule CLAUDE.md records twice. A stale collider is a village you
//! fall through and a silent rebuild moves crates at the other end of the arena,
//! so the third option is the honest one: say no, and name the entity.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::components::{Collider, TileGrid, Transform};
use crate::error::{EngineError, Result};
use crate::scene::{ResolvedTileGrid, Scene};
use crate::tilelayout::Grid;
use crate::tileset::{Compat, ExpandedTile, Tileset};

/// What a script asked for, drained by the caller after the step.
#[derive(Clone, Debug, PartialEq)]
pub struct SynthesisRequest {
    pub entity: String,
    pub kind: RequestKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RequestKind {
    /// Every unlocked cell back to the known-good fill, nothing solved.
    Clear,
    /// Re-solve the blocks meeting a world-space disc.
    Solve {
        /// The disc's centre in world metres, XZ.
        at: [f32; 2],
        /// Its radius in metres. Zero is the single cell the centre is in.
        radius: f32,
        /// The roll to use, or the component's own `seed` when absent.
        seed: Option<u32>,
    },
}

/// What one applied request did — the trace line, and `simulate`'s count.
#[derive(Clone, Debug, PartialEq)]
pub struct SynthesisReport {
    pub entity: String,
    /// The cell rectangle the disc resolved to; `None` for a clear.
    pub region: Option<[u32; 4]>,
    /// Blocks the run actually solved. Zero for a clear.
    pub solved: usize,
    /// Blocks that used up their retries and kept what they had.
    pub fallbacks: u32,
    /// Cells whose tile is not what it was before this request. The number that
    /// says whether anything happened — a re-solve that changes nothing is a
    /// legitimate outcome and looks exactly like one that failed.
    pub changed: usize,
}

/// One grid's runtime state: everything a solve reads, resident.
struct LiveGrid {
    component: TileGrid,
    tileset: Tileset,
    tiles: Vec<ExpandedTile>,
    compat: Compat,
    rules: crate::constraints::Rules,
    grid: Grid,
    params: crate::synthesize::Params,
    placement: Transform,
    /// The current arrangement, as indices into `tiles`.
    cells: Vec<usize>,
    locked: Vec<bool>,
    fill_ground: usize,
    fill_background: usize,
    /// What the last solve reported, carried so [`LiveGrids::apply`] can put it
    /// in the report without `solve` having to return two things.
    last_solved: usize,
    last_fallbacks: u32,
}

/// Every grid a run has synthesized at runtime, by entity name.
pub struct LiveGrids {
    base_dir: PathBuf,
    grids: HashMap<String, LiveGrid>,
}

impl LiveGrids {
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: base_dir.into(),
            grids: HashMap::new(),
        }
    }

    /// Apply one request: solve or clear, then regrow the entity's geometry.
    pub fn apply(
        &mut self,
        scene: &mut Scene,
        request: &SynthesisRequest,
    ) -> Result<SynthesisReport> {
        self.ensure(scene, &request.entity)?;
        let live = self
            .grids
            .get_mut(&request.entity)
            .expect("ensured just above");

        let before = live.cells.clone();
        let region = match &request.kind {
            RequestKind::Clear => {
                live.clear();
                None
            }
            RequestKind::Solve { at, radius, seed } => {
                Some(live.solve(*at, *radius, *seed, &request.entity)?)
            }
        };
        let (solved, fallbacks) = match &request.kind {
            RequestKind::Clear => (0, 0),
            RequestKind::Solve { .. } => (live.last_solved, live.last_fallbacks),
        };
        let changed = before
            .iter()
            .zip(&live.cells)
            .filter(|(was, now)| was != now)
            .count();

        // Regrow only when something moved. `solid_for` is cached on the cells,
        // so an unchanged arrangement would hand back the same `Arc` anyway —
        // this skips the hash of a thousand cells as well.
        if changed > 0 {
            let solid =
                crate::tilegrid::solid_for(&live.tileset, &live.grid, &live.tiles, &live.cells);
            if let Some(entity) = scene.entity(&request.entity) {
                let _ = scene.world.insert_one(entity, ResolvedTileGrid(solid));
            }
        }

        Ok(SynthesisReport {
            entity: request.entity.clone(),
            region,
            solved,
            fallbacks,
            changed,
        })
    }

    /// Load one grid's files, once. Every error here names the entity, because
    /// a script call is the only thing that can reach this code and the script
    /// wrote a name rather than a path.
    fn ensure(&mut self, scene: &Scene, name: &str) -> Result<()> {
        if self.grids.contains_key(name) {
            return Ok(());
        }
        let entity = scene.entity(name).ok_or_else(|| {
            EngineError::new(
                crate::codes::ENTITY_NOT_FOUND,
                format!("no entity named {name:?}"),
            )
            .suggest_from(name, scene.names())
        })?;
        let component = scene
            .world
            .get::<&TileGrid>(entity)
            .map(|grid| (*grid).clone())
            .map_err(|_| {
                EngineError::new(
                    crate::codes::MISSING_COMPONENT,
                    format!("entity {name:?} has no TileGrid to synthesize"),
                )
                .entity(name)
            })?;

        // §5 of the design: refused rather than re-solved or silently staled.
        if scene.world.get::<&Collider>(entity).is_ok() {
            return Err(EngineError::new(
                crate::codes::TILE_GRID_COLLIDES,
                format!(
                    "{name:?} has a Collider, so its geometry is also a physics trimesh and \
                     cannot be re-solved while the scene runs; drop the Collider, or solve it \
                     with `engine synthesize`"
                ),
            )
            .entity(name)
            .component("TileGrid"));
        }

        let tileset_path = crate::tileset::resolve_tileset(&component.tileset, &self.base_dir)
            .map_err(|e| e.entity(name))?;
        let tileset = crate::tileset::load_tileset(&tileset_path).map_err(|e| e.entity(name))?;
        let tiles = crate::tileset::expand(&tileset).map_err(|e| e.entity(name))?;
        let compat = Compat::build(&tiles);
        let rules =
            crate::constraints::Rules::prepare(&tileset, &tiles).map_err(|e| e.entity(name))?;

        let layout_path = self.base_dir.join(&component.layout);
        let text = std::fs::read_to_string(&layout_path).map_err(|e| {
            EngineError::new(
                crate::codes::TILE_LAYOUT_MISSING,
                format!(
                    "could not read the tile layout at {}: {e}; run `engine synthesize --write`",
                    layout_path.display()
                ),
            )
            .entity(name)
        })?;
        let layout = crate::tilelayout::TileLayout::parse(&text).map_err(|bad| {
            EngineError::new(
                crate::codes::TILE_LAYOUT_MALFORMED,
                format!(
                    "the tile layout at {:?} is not readable: {bad}",
                    component.layout
                ),
            )
            .entity(name)
        })?;
        let cells = layout.resolve(&tiles).map_err(|unknown| {
            EngineError::new(
                crate::codes::UNKNOWN_TILE,
                format!(
                    "the tile layout at {:?} places tiles the tileset does not define: {}",
                    component.layout,
                    unknown.join(", ")
                ),
            )
            .entity(name)
        })?;

        let (fill_ground, fill_background) =
            crate::tilegrid::fill_indices(&component, &tileset, &tiles).map_err(|missing| {
                EngineError::new(
                    crate::codes::UNKNOWN_TILE,
                    format!("the tileset defines no tile named {missing:?}"),
                )
                .entity(name)
            })?;

        // The header's blocks, not `Params::default()`. A layout solved at a
        // different block size has its seams in different places, and a runtime
        // solve on the default lattice would read borders the file was never
        // built against — M49's `--check` trap, one room over.
        let params = crate::synthesize::Params {
            block: layout.header.block,
            overlap: layout.header.overlap,
            attempts: layout.header.attempts,
        };

        self.grids.insert(
            name.to_string(),
            LiveGrid {
                grid: Grid {
                    size: component.size,
                    offsets: layout.header.offsets.clone(),
                    // The header's, not the component's: a runtime solve
                    // builds on the committed layout, and the file is only
                    // meaningful beside the edge mode it was solved with. A
                    // component that disagrees is `tile_layout_mismatch` at
                    // validation, before a run gets this far.
                    edges: layout.header.edges,
                },
                placement: scene.transform_of(entity),
                locked: layout.cells.iter().map(|c| c.locked).collect(),
                component,
                tileset,
                tiles,
                compat,
                rules,
                params,
                cells,
                fill_ground,
                fill_background,
                last_solved: 0,
                last_fallbacks: 0,
            },
        );
        Ok(())
    }

    /// Whether a grid has been loaded — the "a scene that never calls pays
    /// nothing" claim, as something a test can ask.
    pub fn is_resident(&self, name: &str) -> bool {
        self.grids.contains_key(name)
    }

    /// The tokens one grid holds *now*, in the grid's own cell order.
    ///
    /// What `engine tile-grid --steps` reports, and the only way to read a
    /// runtime arrangement at all: nothing writes it back to disk, so without
    /// this the answer to "what did it build" is a picture. `None` for a grid
    /// no call has named — which reads correctly as "the file is still the
    /// truth for that one".
    pub fn tokens(&self, name: &str) -> Option<Vec<String>> {
        self.grids.get(name).map(|live| {
            live.cells
                .iter()
                .map(|cell| live.tiles[*cell].token.clone())
                .collect()
        })
    }
}

impl LiveGrid {
    /// Every unlocked cell back to the fill.
    ///
    /// The first half of `synthesize --reset`, split out so a script can clear
    /// once and grow the grid over as many steps as it likes. Locks survive for
    /// the reason they survive a reset: they are the one thing it is not meant
    /// to throw away.
    fn clear(&mut self) {
        for index in 0..self.cells.len() {
            if self.locked.get(index).copied().unwrap_or(false) {
                continue;
            }
            self.cells[index] = if self.grid.coords(index).1 == 0 {
                self.fill_ground
            } else {
                self.fill_background
            };
        }
    }

    fn solve(
        &mut self,
        at: [f32; 2],
        radius: f32,
        seed: Option<u32>,
        name: &str,
    ) -> Result<[u32; 4]> {
        let centre = glam::Vec3::new(at[0], 0.0, at[1]);
        let region = crate::tilegrid::region_around(
            centre,
            radius,
            &self.placement,
            self.component.size,
            self.tileset.cell,
        )
        .map_err(|miss| match miss {
            crate::tilegrid::RegionMiss::BadRadius => EngineError::new(
                crate::codes::INVALID_INVOCATION,
                format!("a synthesis radius must be zero or more metres, found {radius}"),
            )
            .entity(name),
            crate::tilegrid::RegionMiss::OffGrid => EngineError::new(
                crate::codes::TILE_REGION_OFF_GRID,
                format!(
                    "the disc at ({}, {}) of radius {radius} does not meet the grid on {name:?}; \
                     `engine tile-grid` reports its footprint",
                    at[0], at[1]
                ),
            )
            .entity(name),
        })?;

        let outcome = crate::synthesize::synthesize(&crate::synthesize::Request {
            grid: &self.grid,
            tiles: &self.tiles,
            compat: &self.compat,
            seed: seed.unwrap_or(self.component.seed),
            params: self.params,
            fill_ground: self.fill_ground,
            fill_background: self.fill_background,
            prior: Some(&self.cells),
            locked: &self.locked,
            region: Some(region),
            rules: &self.rules,
        })
        .map_err(|e| e.entity(name))?;

        self.cells = outcome.cells;
        self.last_solved = outcome.solved;
        self.last_fallbacks = outcome.fallbacks;
        Ok(region)
    }
}
