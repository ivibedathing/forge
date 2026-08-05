//! The solved grid, as a file (M47).
//!
//! A `TileGrid` names a layout file and renders nothing until `engine
//! synthesize` has written one — the `LightProbeVolume`/`bake-gi` shape (M35),
//! including `--check` and a staleness digest. Two commands to first pixels is
//! a real cost and it buys three things: invariant 2's answer (the solved world
//! is on disk as text), something for a local edit to *edit*, and a validation
//! pass that can re-verify the layout against the tileset's own adjacency rules
//! cheaply.
//!
//! # The format
//!
//! NDJSON: a header object, then one line per grid row.
//!
//! ```text
//! {"format":"forge-tiles-1","entity":"Village",...,"size":[8,2,6],"seed":11,...}
//! {"y":0,"z":0,"row":"cobble@0 cobble@0 corner@0 wall@0 wall@0 corner@1 cobble@0 cobble@0"}
//! {"y":0,"z":1,"row":"cobble@0 post@0 wall@3 floor@0 floor@0 wall@1 cobble@0 cobble@0"}
//! ```
//!
//! Copied line for line from `gi/mod.rs`, which earns the shape four ways: a
//! per-line `serde_json` parse gives a real line number on error; the header is
//! an object `jq` reads; **the line order is the layout**, checked explicitly,
//! because a permuted file parses as valid JSON and renders a wrong world; and
//! [`TileLayout::to_text`] round-trips byte-identically, so a re-solve at the
//! same seed leaves the file untouched and the diff empty.
//!
//! A `!` before a token **locks** that cell: a hard constraint on every solve,
//! never re-picked, byte-identical after a full re-solve. It is how an author
//! says "the door goes *there*" and keeps it through everything else changing.
//!
//! # Indexing, and the two kinds of edge
//!
//! Cells are `x + nx*(z + nz*y)` — x fastest, then z, then y — which is exactly
//! the file's row order, so the file and the array are one traversal and there
//! is nowhere to get a transpose wrong.
//!
//! [`Grid::neighbour`] is the only place the grid's shape is interpreted, and
//! it makes a distinction the rest of the milestone rests on: **the grid's
//! vertical ends are closed and its horizontal edges are open.** A patch of
//! village is a window onto a larger world sideways, so a cell at `x == 0` is
//! unconstrained on `−X`; but there is no storey below `y == 0` and no storey
//! above the top, so those faces must mate the empty socket. That single rule
//! is what stops a roof from floating at ground level without any tile having
//! to say so.

use serde::{Deserialize, Serialize};

use crate::tileset::{ExpandedTile, Face};

/// The format string a file this version reads carries.
pub const FORMAT: &str = "forge-tiles-1";

/// The header object on a layout file's first line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayoutHeader {
    /// Always [`FORMAT`] for a file this version can read.
    pub format: String,
    /// The scene the layout was solved for, for a human reading the file.
    pub scene: String,
    /// The `TileGrid` entity it belongs to — a scene may hold several.
    pub entity: String,
    /// The tileset reference, as the component spelled it.
    pub tileset: String,
    /// Digest of the tileset file's bytes, so a tileset edited under a layout
    /// is caught without re-reading it.
    pub tileset_hash: String,
    /// Digest of every input the solve read; what `synthesize --check`
    /// recomputes.
    pub inputs_hash: String,
    /// Cells along each axis, copied from the component so a component edited
    /// after its solve is caught cheaply, before the digest.
    pub size: [u32; 3],
    pub seed: u32,

    /// Block extent in cells. **In the header rather than on the component**,
    /// following M35's split of `bounces` (which changes what the runtime sees)
    /// from `samples` (which is a property of the artifact).
    pub block: [u32; 3],
    /// Cells of overlap between neighbouring blocks.
    pub overlap: u32,
    /// Retries a block was allowed before it took the known-good fill.
    pub attempts: u32,
    /// Blocks that used up their retries and took the fill.
    ///
    /// The number that says a tileset is over-constrained without reading the
    /// picture: an over-constrained tileset does not fail loudly, it produces
    /// bland output.
    #[serde(default)]
    pub fallbacks: u32,

    /// Y offset in **cells** per column, in `x + nx*z` order, when the grid
    /// follows a `Terrain`. Absent for a flat grid.
    ///
    /// Written out rather than recomputed on load because they are an input to
    /// the solve — [`Grid::neighbour`] shears by them — so a layout is only
    /// meaningful beside the offsets it was solved against.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub offsets: Vec<i32>,
}

/// One row of the grid.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct RowLine {
    y: u32,
    z: u32,
    row: String,
}

/// What stands in one cell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    /// `name@rotation`, without the lock marker.
    pub token: String,
    /// Written `!token`: a hard constraint the solver never re-picks.
    pub locked: bool,
}

impl Cell {
    pub fn new(token: impl Into<String>, locked: bool) -> Self {
        Self {
            token: token.into(),
            locked,
        }
    }

    fn write(&self) -> String {
        if self.locked {
            format!("!{}", self.token)
        } else {
            self.token.clone()
        }
    }
}

/// A parsed layout file.
#[derive(Debug, Clone, PartialEq)]
pub struct TileLayout {
    pub header: LayoutHeader,
    /// One entry per cell, in `x + nx*(z + nz*y)` order.
    pub cells: Vec<Cell>,
}

/// Why a layout file could not be used. Mapped to `tile_layout_malformed` at
/// the validation boundary; kept apart here so the message can say which.
#[derive(Debug, Clone, PartialEq)]
pub enum LayoutError {
    Parse { line: usize, message: String },
    Malformed(String),
}

impl std::fmt::Display for LayoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse { line, message } => write!(f, "line {line}: {message}"),
            Self::Malformed(message) => f.write_str(message),
        }
    }
}

impl TileLayout {
    /// Parse a layout file: a header line, then one line per grid row.
    pub fn parse(text: &str) -> Result<Self, LayoutError> {
        let mut lines = text
            .lines()
            .enumerate()
            .filter(|(_, l)| !l.trim().is_empty());

        let (header_line, header_text) = lines.next().ok_or_else(|| {
            LayoutError::Malformed("the file is empty; it needs a header line".into())
        })?;
        let header: LayoutHeader =
            serde_json::from_str(header_text).map_err(|e| LayoutError::Parse {
                line: header_line + 1,
                message: e.to_string(),
            })?;

        if header.format != FORMAT {
            return Err(LayoutError::Malformed(format!(
                "the file says format {:?}, but this engine reads {FORMAT:?}",
                header.format
            )));
        }

        let [nx, ny, nz] = header.size;
        if nx == 0 || ny == 0 || nz == 0 {
            return Err(LayoutError::Malformed(format!(
                "the header declares a {:?} grid, which holds no cells",
                header.size
            )));
        }
        if !header.offsets.is_empty() && header.offsets.len() as u64 != u64::from(nx) * u64::from(nz)
        {
            return Err(LayoutError::Malformed(format!(
                "the header lists {} column offsets but a {nx}×{nz} footprint has {}",
                header.offsets.len(),
                u64::from(nx) * u64::from(nz)
            )));
        }

        let mut rows: Vec<(usize, RowLine)> = Vec::new();
        for (index, line) in lines {
            let row: RowLine = serde_json::from_str(line).map_err(|e| LayoutError::Parse {
                line: index + 1,
                message: e.to_string(),
            })?;
            rows.push((index + 1, row));
        }

        let expected = u64::from(ny) * u64::from(nz);
        if rows.len() as u64 != expected {
            return Err(LayoutError::Malformed(format!(
                "the header says a {:?} grid ({expected} rows) but the file holds {}",
                header.size,
                rows.len()
            )));
        }

        // Line order *is* the layout, checked for the reason `gi/mod.rs`
        // checks its probe order: a permuted file parses as valid JSON, carries
        // the right count, and renders a wrong world. This is the only cheap
        // moment to catch it.
        let mut cells = Vec::with_capacity((expected * u64::from(nx)) as usize);
        for (position, (line, row)) in rows.iter().enumerate() {
            let want_y = position as u32 / nz;
            let want_z = position as u32 % nz;
            if (row.y, row.z) != (want_y, want_z) {
                return Err(LayoutError::Malformed(format!(
                    "row {line} is y={} z={} but this grid puts y={want_y} z={want_z} there; \
                     the file's line order is its layout (x fastest, then z, then y)",
                    row.y, row.z
                )));
            }
            let tokens: Vec<&str> = row.row.split_whitespace().collect();
            if tokens.len() as u32 != nx {
                return Err(LayoutError::Malformed(format!(
                    "row {line} holds {} cells but the grid is {nx} wide",
                    tokens.len()
                )));
            }
            for token in tokens {
                match token.strip_prefix('!') {
                    Some("") => {
                        return Err(LayoutError::Malformed(format!(
                            "row {line} holds a bare \"!\"; the marker locks a tile and \
                             needs one after it"
                        )));
                    }
                    Some(rest) => cells.push(Cell::new(rest, true)),
                    None => cells.push(Cell::new(token, false)),
                }
            }
        }

        Ok(Self { header, cells })
    }

    /// Serialize back to the on-disk form. Byte-identical for an unchanged
    /// solve, which is what makes a re-run's diff empty.
    pub fn to_text(&self) -> String {
        let [nx, ny, nz] = self.header.size;
        let mut out = serde_json::to_string(&self.header).expect("header serializes");
        out.push('\n');
        for y in 0..ny {
            for z in 0..nz {
                let start = ((z + nz * y) * nx) as usize;
                let row = self.cells[start..start + nx as usize]
                    .iter()
                    .map(Cell::write)
                    .collect::<Vec<_>>()
                    .join(" ");
                let line = RowLine { y, z, row };
                out.push_str(&serde_json::to_string(&line).expect("row serializes"));
                out.push('\n');
            }
        }
        out
    }

    /// The grid this layout is indexed by.
    pub fn grid(&self) -> Grid {
        Grid {
            size: self.header.size,
            offsets: self.header.offsets.clone(),
        }
    }

    /// Every cell's index into `tiles`, or the tokens that name nothing.
    ///
    /// Returned as a whole rather than one at a time so a layout written
    /// against an older tileset reports every stale token at once, which is the
    /// all-at-once contract applied to a file `validate` reads.
    pub fn resolve(&self, tiles: &[ExpandedTile]) -> Result<Vec<usize>, Vec<String>> {
        let mut resolved = Vec::with_capacity(self.cells.len());
        let mut unknown: Vec<String> = Vec::new();
        for cell in &self.cells {
            match tiles.iter().position(|t| t.token == cell.token) {
                Some(index) => resolved.push(index),
                None if !unknown.contains(&cell.token) => unknown.push(cell.token.clone()),
                None => {}
            }
        }
        if unknown.is_empty() {
            Ok(resolved)
        } else {
            Err(unknown)
        }
    }
}

/// The shape a layout is indexed by: extent in cells, and the per-column Y
/// offset a grid following a `Terrain` terraces to.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Grid {
    pub size: [u32; 3],
    /// Cells of lift per column, `x + nx*z`. Empty for a flat grid.
    pub offsets: Vec<i32>,
}

/// What lies across one face of a cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Neighbour {
    /// Another cell of this grid.
    Cell(usize),
    /// Outside the grid, and **constrained to the empty socket** — the two
    /// vertical ends. There is no storey below the ground or above the sky.
    Closed,
    /// Outside the grid and unconstrained — a horizontal edge, where the patch
    /// is a window onto a larger world, and the exposed side of a terrace step.
    Open,
}

impl Grid {
    pub fn cell_count(&self) -> usize {
        let [nx, ny, nz] = self.size;
        (nx as usize) * (ny as usize) * (nz as usize)
    }

    pub fn index(&self, x: u32, y: u32, z: u32) -> usize {
        let [nx, _, nz] = self.size;
        (x + nx * (z + nz * y)) as usize
    }

    pub fn coords(&self, index: usize) -> (u32, u32, u32) {
        let [nx, _, nz] = self.size;
        let i = index as u32;
        (i % nx, i / (nx * nz), (i / nx) % nz)
    }

    /// The column lift, in cells, at a footprint position.
    pub fn offset(&self, x: u32, z: u32) -> i32 {
        if self.offsets.is_empty() {
            return 0;
        }
        let [nx, _, _] = self.size;
        self.offsets
            .get((x + nx * z) as usize)
            .copied()
            .unwrap_or(0)
    }

    /// What lies across `face` from `index`.
    ///
    /// The **only** place the grid's shape is interpreted.
    ///
    /// # A terrace step is a free edge
    ///
    /// Two columns at different lifts do not constrain each other, and that is
    /// a deliberate reversal of the obvious design. Shearing the neighbour by
    /// the difference in lift — so that grid layer 1 of a low column faces
    /// layer 0 of the column one step above it — is *geometrically* right:
    /// those two cells really do touch in world space. But what they touch
    /// across is a **cut face**, the raised column's ground layer against the
    /// lower column's open air, and constraining it would oblige every tileset
    /// to carry a socket for "the side of a hill". Every flat tileset would
    /// stop working the moment it followed ground.
    ///
    /// So a step is simply open on both sides, the way the patch's own edges
    /// are. What it costs is a building that spans a step, which a terraced
    /// village does not want anyway: buildings sit on one terrace. The lift
    /// still moves the geometry — that is what terracing *is* — it just stops
    /// being an adjacency relation.
    pub fn neighbour(&self, index: usize, face: Face) -> Neighbour {
        let [nx, ny, nz] = self.size;
        let (x, y, z) = self.coords(index);
        let step = face.step();

        let (Some(tx), Some(tz)) = (checked_step(x, step[0], nx), checked_step(z, step[2], nz))
        else {
            // Sideways off the patch: open, because a village is a window onto
            // a larger world and its edge tiles must not be constrained as if
            // the world ended there.
            return Neighbour::Open;
        };

        if self.offset(x, z) != self.offset(tx, tz) {
            return Neighbour::Open;
        }

        let target = y as i64 + i64::from(step[1]);
        if target < 0 || target >= i64::from(ny) {
            // Straight up off the top or down through the floor: closed, so a
            // `sky`/`ground` socket has something to fail against. This is
            // what stops a roof floating at ground level without any tile
            // having to say so.
            return Neighbour::Closed;
        }
        Neighbour::Cell(self.index(tx, target as u32, tz))
    }
}

/// One cell face that breaks the tileset's own rules.
#[derive(Debug, Clone, PartialEq)]
pub struct Violation {
    pub cell: usize,
    pub face: Face,
    /// Whether the author pinned this cell. The two cases get different codes:
    /// an unlocked violation is `tile_layout_illegal` (the file was hand-edited
    /// into an illegal state, or the solver is broken), a locked one is
    /// `tile_layout_forced` (the author asserted it, and the engine draws it
    /// and says so).
    pub locked: bool,
    /// What the offending face is up against, for the message.
    pub against: Option<usize>,
}

/// Every cell face that violates adjacency.
///
/// A cheap, complete check that the committed layout is one the tileset could
/// have produced — the property the sidecar format buys and that no inline or
/// solved-at-load form makes any easier. It is also what lets a contact sheet,
/// which locks everything and cares about none of it, pass validation without a
/// special case.
pub fn verify_adjacency(
    layout: &TileLayout,
    grid: &Grid,
    tiles: &[ExpandedTile],
    resolved: &[usize],
    compat: &crate::tileset::Compat,
) -> Vec<Violation> {
    let mut violations = Vec::new();
    for (cell, &here) in resolved.iter().enumerate() {
        for face in Face::ALL {
            let ok = match grid.neighbour(cell, face) {
                Neighbour::Cell(other) => compat.allows(face.index(), here, resolved[other]),
                // A closed end constrains as if the outside were the empty
                // socket, which is what makes `sky` and `ground` sockets do
                // real work rather than being decoration.
                Neighbour::Closed => matches!(
                    tiles[here].faces[face.index()].socket,
                    crate::tileset::Socket::Empty
                ),
                Neighbour::Open => true,
            };
            if !ok {
                violations.push(Violation {
                    cell,
                    face,
                    locked: layout.cells[cell].locked,
                    against: match grid.neighbour(cell, face) {
                        Neighbour::Cell(other) => Some(other),
                        _ => None,
                    },
                });
            }
        }
    }
    violations
}

fn checked_step(value: u32, step: i32, limit: u32) -> Option<u32> {
    let moved = i64::from(value) + i64::from(step);
    (moved >= 0 && moved < i64::from(limit)).then_some(moved as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(size: [u32; 3]) -> LayoutHeader {
        LayoutHeader {
            format: FORMAT.into(),
            scene: "s.json".into(),
            entity: "Village".into(),
            tileset: "tilesets/village.json".into(),
            tileset_hash: "0".into(),
            inputs_hash: "0".into(),
            size,
            seed: 1,
            block: [4, size[1], 4],
            overlap: 1,
            attempts: 10,
            fallbacks: 0,
            offsets: Vec::new(),
        }
    }

    fn layout(size: [u32; 3]) -> TileLayout {
        let cells = (0..(size[0] * size[1] * size[2]))
            .map(|i| Cell::new(format!("t{i}@0"), i % 5 == 0))
            .collect();
        TileLayout {
            header: header(size),
            cells,
        }
    }

    #[test]
    fn a_layout_round_trips_byte_for_byte() {
        let original = layout([3, 2, 2]);
        let text = original.to_text();
        let reparsed = TileLayout::parse(&text).expect("parses");
        assert_eq!(reparsed, original);
        assert_eq!(
            reparsed.to_text(),
            text,
            "a re-solve at the same seed must leave the file untouched"
        );
    }

    #[test]
    fn a_lock_survives_the_round_trip() {
        let text = layout([3, 1, 1]).to_text();
        assert!(text.contains("!t0@0"), "{text}");
        let reparsed = TileLayout::parse(&text).unwrap();
        assert!(reparsed.cells[0].locked);
        assert!(!reparsed.cells[1].locked);
        assert_eq!(reparsed.cells[0].token, "t0@0", "the marker is not the name");
    }

    /// A permuted file parses as valid JSON and renders a wrong world, so the
    /// order is checked rather than trusted.
    #[test]
    fn line_order_is_the_layout() {
        let text = layout([2, 1, 3]).to_text();
        let mut lines: Vec<&str> = text.lines().collect();
        lines.swap(1, 2);
        let err = TileLayout::parse(&lines.join("\n")).unwrap_err();
        assert!(
            matches!(&err, LayoutError::Malformed(m) if m.contains("line order is its layout")),
            "{err}"
        );
    }

    #[test]
    fn the_header_and_the_rows_must_agree() {
        let text = layout([2, 1, 2]).to_text();
        let short: String = text.lines().take(2).collect::<Vec<_>>().join("\n");
        assert!(matches!(
            TileLayout::parse(&short),
            Err(LayoutError::Malformed(_))
        ));

        let wide = text.replace("t0@0", "t0@0 t0@0");
        assert!(
            matches!(&TileLayout::parse(&wide), Err(LayoutError::Malformed(m)) if m.contains("wide")),
        );
    }

    #[test]
    fn a_bad_line_reports_its_line_number() {
        let text = layout([2, 1, 2]).to_text();
        let broken = text.replace("\"z\":1", "\"z\":");
        assert!(matches!(
            TileLayout::parse(&broken),
            Err(LayoutError::Parse { line: 3, .. })
        ));
    }

    #[test]
    fn a_bare_lock_marker_is_an_error() {
        let text = layout([2, 1, 1]).to_text().replace("!t0@0", "!");
        assert!(matches!(
            TileLayout::parse(&text),
            Err(LayoutError::Malformed(_))
        ));
    }

    #[test]
    fn indexing_agrees_with_the_files_traversal() {
        let grid = Grid {
            size: [3, 2, 4],
            offsets: Vec::new(),
        };
        for index in 0..grid.cell_count() {
            let (x, y, z) = grid.coords(index);
            assert_eq!(grid.index(x, y, z), index);
        }
    }

    /// Vertical ends closed, horizontal edges open — the rule that stops a roof
    /// floating at ground level without any tile having to say so.
    #[test]
    fn the_stack_is_closed_and_the_footprint_is_open() {
        let grid = Grid {
            size: [2, 2, 2],
            offsets: Vec::new(),
        };
        let ground = grid.index(0, 0, 0);
        assert_eq!(grid.neighbour(ground, Face::NegY), Neighbour::Closed);
        assert_eq!(grid.neighbour(ground, Face::NegX), Neighbour::Open);
        assert_eq!(grid.neighbour(ground, Face::NegZ), Neighbour::Open);
        assert_eq!(
            grid.neighbour(ground, Face::PosY),
            Neighbour::Cell(grid.index(0, 1, 0))
        );
        let top = grid.index(1, 1, 1);
        assert_eq!(grid.neighbour(top, Face::PosY), Neighbour::Closed);
        assert_eq!(grid.neighbour(top, Face::PosX), Neighbour::Open);
    }

    /// A terrace step is a free edge on both sides — what the two columns touch
    /// across is a cut face, and making that an adjacency would oblige every
    /// tileset to carry a socket for the side of a hill.
    #[test]
    fn a_terrace_step_is_a_free_edge() {
        let grid = Grid {
            size: [3, 2, 1],
            // The +X-most column stands one cell higher than the other two.
            offsets: vec![0, 0, 1],
        };
        for y in 0..2 {
            assert_eq!(
                grid.neighbour(grid.index(1, y, 0), Face::PosX),
                Neighbour::Open,
                "layer {y} across the step"
            );
            assert_eq!(
                grid.neighbour(grid.index(2, y, 0), Face::NegX),
                Neighbour::Open,
                "and back again, so the relation stays symmetric"
            );
        }
        // Within one terrace nothing changes.
        assert_eq!(
            grid.neighbour(grid.index(0, 0, 0), Face::PosX),
            Neighbour::Cell(grid.index(1, 0, 0))
        );
        // And the stack is still closed at both ends, on every column.
        assert_eq!(grid.neighbour(grid.index(2, 0, 0), Face::NegY), Neighbour::Closed);
        assert_eq!(grid.neighbour(grid.index(2, 1, 0), Face::PosY), Neighbour::Closed);
    }

    #[test]
    fn the_offsets_must_cover_the_footprint() {
        let mut broken = layout([2, 1, 2]);
        broken.header.offsets = vec![0, 1];
        assert!(matches!(
            TileLayout::parse(&broken.to_text()),
            Err(LayoutError::Malformed(_))
        ));
    }
}
