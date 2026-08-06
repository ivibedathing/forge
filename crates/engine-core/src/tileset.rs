//! Tilesets: the vocabulary a `TileGrid` is synthesized from (M47).
//!
//! A tileset file names a `palette` of materials and a list of `tiles`. A tile
//! is **grown, not modelled**: it carries a handful of parametric `parts` —
//! boxes, wedges, cylinders — in the same metres and the same `at`/`size`
//! convention a `Transform` uses, plus a socket string per face saying what it
//! may sit against.
//!
//! That choice is the milestone's premise rather than a convenience. An agent
//! asked for "a low-poly medieval village" can write a hundred lines of boxes
//! and wedges; it cannot model a `.glb`. Making the tile a recipe is what puts
//! the whole tileset inside the text medium — the argument M19 made for `Tree`
//! over a tree mesh, and M22 for `Terrain` over a height-map image.
//!
//! # Coordinates
//!
//! Part coordinates are **cell-local: the origin is the cell's centre in X and
//! Z, and its floor in Y.** `at` is a part's centre and `size` its full extent.
//! A cell is [`Tileset::cell`] metres.
//!
//! # Rotation
//!
//! A tile declares `rotations` of 1, 2 or 4 and the set is **expanded** before
//! anything else runs, so the solver only ever sees tiles with fixed sockets.
//! The engine's Euler convention carries +X to −Z under +90° about Y, so one
//! turn permutes the faces `px ← pz`, `pz ← nx`, `nx ← nz`, `nz ← px`. Those
//! four lines are the most dangerous four in the milestone — reversed, a
//! tileset solves cleanly and renders with every wall facing inward — so
//! `a_turn_moves_geometry_and_sockets_together` checks them against the rotated
//! *geometry* rather than against a hand-written table.

use std::collections::BTreeMap;
use std::f32::consts::FRAC_PI_2;
use std::path::{Path, PathBuf};

use glam::{Mat3, Vec3};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::components::Material;
use crate::error::{EngineError, Result};
use crate::mesh::MeshData;

/// Expanded tiles a tileset may produce. The solver's support counters are
/// `u16` against this, and a bitset row is `MAX_TILES / 64` words.
pub const MAX_TILES: usize = 512;

/// Parts one tile may carry. A tile is a handful of primitives; a tile that
/// wants thirty-three of them wants a mesh file, which is §10 of the design.
pub const MAX_PARTS_PER_TILE: usize = 32;

/// File extension a tileset reference must carry.
pub const TILESET_EXTENSION: &str = "json";

// ── Faces and directions ──────────────────────────────────────────────

/// One face of a cell. The discriminants are the solver's direction indices,
/// so `opposite` is `index ^ 1` and a neighbour lookup is an array index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Face {
    PosX = 0,
    NegX = 1,
    PosY = 2,
    NegY = 3,
    PosZ = 4,
    NegZ = 5,
}

impl Face {
    /// Every face, in direction-index order.
    pub const ALL: [Face; 6] = [
        Face::PosX,
        Face::NegX,
        Face::PosY,
        Face::NegY,
        Face::PosZ,
        Face::NegZ,
    ];

    /// The JSON key each face is written as, in direction-index order.
    pub const KEYS: [&'static str; 6] = ["px", "nx", "py", "ny", "pz", "nz"];

    pub fn index(self) -> usize {
        self as usize
    }

    /// The face across the interface: what `+X` of one cell touches.
    pub fn opposite(self) -> Face {
        Face::ALL[self.index() ^ 1]
    }

    /// Whether this face is `py` or `ny` — the two that carry a rotation index.
    pub fn is_vertical(self) -> bool {
        matches!(self, Face::PosY | Face::NegY)
    }

    /// The unit step to the neighbour across this face, in cells.
    pub fn step(self) -> [i32; 3] {
        match self {
            Face::PosX => [1, 0, 0],
            Face::NegX => [-1, 0, 0],
            Face::PosY => [0, 1, 0],
            Face::NegY => [0, -1, 0],
            Face::PosZ => [0, 0, 1],
            Face::NegZ => [0, 0, -1],
        }
    }

    /// Which face of the unturned tile supplies this face after one +90° turn
    /// about Y.
    ///
    /// Read it as "where did what is now here come from". `+90°` carries `+X`
    /// to `−Z`, so what is now at `−Z` came from `+X`, and what is now at `+X`
    /// came from `+Z`.
    pub fn source_after_turn(self) -> Face {
        match self {
            Face::PosX => Face::PosZ,
            Face::PosZ => Face::NegX,
            Face::NegX => Face::NegZ,
            Face::NegZ => Face::PosX,
            vertical => vertical,
        }
    }
}

// ── Sockets ───────────────────────────────────────────────────────────

/// What a face may sit against.
///
/// Four forms and no more:
///
/// | Written | Meaning |
/// |---|---|
/// | `"0"` | nothing here — reserved, mates itself, ignores rotation |
/// | `"name"` | symmetric: mates another `"name"` |
/// | `"name_l"` / `"name_r"` | a mirrored pair: each mates the other, neither mates itself |
///
/// **Plain is symmetric**, which is where this departs from the DeBroglie
/// convention it otherwise follows (there, plain `x` mates `xf`). Symmetric is
/// what an author means almost every time — two identical walls in a row meet
/// through faces carrying the same string — so it is the form that gets no
/// suffix, and the rarer mirrored pair is the one that has to be spelled out.
/// Both halves being marked is deliberate: `x` mating `xf` while `x` refuses
/// itself is the part of the original convention that reads as a bug.
///
/// Any form may take a trailing `_i` on a **vertical** face, which drops the
/// rotation index from the match (see [`FaceSocket`]).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Socket {
    Empty,
    Symmetric(String),
    Left(String),
    Right(String),
}

impl Socket {
    /// Whether two sockets name the same interface from opposite sides.
    pub fn mates(&self, other: &Socket) -> bool {
        match (self, other) {
            (Socket::Empty, Socket::Empty) => true,
            (Socket::Symmetric(a), Socket::Symmetric(b)) => a == b,
            (Socket::Left(a), Socket::Right(b)) | (Socket::Right(a), Socket::Left(b)) => a == b,
            _ => false,
        }
    }

    /// The base name, for reporting which interface has no partner.
    pub fn name(&self) -> &str {
        match self {
            Socket::Empty => "0",
            Socket::Symmetric(a) | Socket::Left(a) | Socket::Right(a) => a,
        }
    }
}

/// A socket as one expanded tile carries it: the interface, plus the rotation
/// index a **vertical** face keeps.
///
/// A `py`/`ny` socket matches on the rotation index as well as the interface,
/// so a wall at rotation 1 does not stack under a wall at rotation 2. That
/// over-constrains on purpose: over-constraining has a report — `engine
/// list-tiles` warns on a socket with no partner — while under-constraining
/// renders a second storey rotated off its ground floor and says nothing. The
/// `_i` suffix opts a vertical socket out.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FaceSocket {
    pub socket: Socket,
    /// `Some(rotation)` for a vertical face that keeps its turn; `None` for
    /// every horizontal face and for anything suffixed `_i`.
    pub turn: Option<u8>,
}

impl FaceSocket {
    pub fn mates(&self, other: &FaceSocket) -> bool {
        let turn_agrees = match (self.turn, other.turn) {
            (Some(a), Some(b)) => a == b,
            _ => true,
        };
        turn_agrees && self.socket.mates(&other.socket)
    }
}

/// Parse a socket string into its interface and whether it opted out of the
/// rotation index. The error is the message, so the caller can attach a path.
pub fn parse_socket(text: &str) -> std::result::Result<(Socket, bool), String> {
    let mut body = text;
    let mut invariant = false;
    let mut symmetry: Option<char> = None;

    // Suffixes strip from the right, so `wall_l_i` and `wall_i_l` both read.
    loop {
        if let Some(rest) = body.strip_suffix("_i") {
            if invariant {
                return Err(format!("socket {text:?} repeats the _i suffix"));
            }
            invariant = true;
            body = rest;
            continue;
        }
        if let Some(rest) = body.strip_suffix("_l").or_else(|| body.strip_suffix("_r")) {
            let marker = body.as_bytes()[body.len() - 1] as char;
            if symmetry.is_some() {
                return Err(format!(
                    "socket {text:?} carries both halves of a mirrored pair; \
                     write _l on one face and _r on the other"
                ));
            }
            symmetry = Some(marker);
            body = rest;
            continue;
        }
        break;
    }

    if body.is_empty() {
        return Err(format!(
            "socket {text:?} is all suffix and names no interface"
        ));
    }

    let socket = match (body, symmetry) {
        ("0", None) => Socket::Empty,
        ("0", Some(_)) => {
            return Err(format!(
                "socket {text:?} mirrors \"0\", which is the reserved empty \
                 interface and has no sides"
            ));
        }
        (name, None) => Socket::Symmetric(name.to_string()),
        (name, Some('l')) => Socket::Left(name.to_string()),
        (name, Some(_)) => Socket::Right(name.to_string()),
    };
    Ok((socket, invariant))
}

// ── The file ──────────────────────────────────────────────────────────

/// Which horizontal direction a wedge's slope climbs toward.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Rise {
    #[default]
    Px,
    Nx,
    Pz,
    Nz,
}

impl Rise {
    /// Quarter-turns about Y that carry a `Px` wedge onto this one. `+90°`
    /// takes `+X` to `−Z`, so `Nz` is one turn and `Pz` is three.
    fn turns(self) -> u8 {
        match self {
            Rise::Px => 0,
            Rise::Nz => 1,
            Rise::Nx => 2,
            Rise::Pz => 3,
        }
    }
}

/// A primitive the engine grows. Three, deliberately: a doorway is two boxes
/// and a lintel, a roof is two wedges, a column is a cylinder, and each extra
/// kind costs a schema variant, a validation arm, a vertex-count term, a
/// winding test and a UV convention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum PartKind {
    #[default]
    Box,
    Wedge,
    Cylinder,
}

/// One primitive inside a cell.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Part {
    pub kind: PartKind,
    /// The part's centre, cell-local: origin at the cell's centre in X and Z,
    /// its floor in Y.
    #[serde(default)]
    #[schemars(with = "[f32; 3]")]
    pub at: Vec3,
    /// The part's full extent in metres, before `yaw`.
    #[schemars(with = "[f32; 3]")]
    pub size: Vec3,
    /// Which way a `wedge` climbs. Ignored by the other kinds.
    #[serde(default)]
    pub rise: Rise,
    /// Degrees about the cell's Y axis, on top of the tile's own rotation.
    #[serde(default)]
    pub yaw: f32,
    /// Sides of a `cylinder`. Ignored by the other kinds.
    #[serde(default = "default_sides")]
    #[schemars(range(min = 3, max = 32))]
    pub sides: u32,
    /// Which [`Tileset::palette`] entry paints it.
    pub material: String,
}

fn default_sides() -> u32 {
    8
}

/// The six sockets of a tile, one per face. An omitted face is `"0"` — nothing
/// there — which is what an `air` tile is made of.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Faces {
    pub px: String,
    pub nx: String,
    pub py: String,
    pub ny: String,
    pub pz: String,
    pub nz: String,
}

impl Default for Faces {
    fn default() -> Self {
        Self {
            px: "0".into(),
            nx: "0".into(),
            py: "0".into(),
            ny: "0".into(),
            pz: "0".into(),
            nz: "0".into(),
        }
    }
}

impl Faces {
    /// The socket written for a face, in direction-index order.
    pub fn get(&self, face: Face) -> &str {
        match face {
            Face::PosX => &self.px,
            Face::NegX => &self.nx,
            Face::PosY => &self.py,
            Face::NegY => &self.ny,
            Face::PosZ => &self.pz,
            Face::NegZ => &self.nz,
        }
    }
}

/// One authored tile, before rotation expansion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TileDef {
    /// The token a layout writes, before `@rotation`.
    pub name: String,
    /// Relative frequency among the tiles that fit. Weights are per authored
    /// tile, not per rotation, so a `rotations: 4` tile at weight 4 is as
    /// likely overall as a `rotations: 1` tile at weight 4.
    #[serde(default = "default_weight")]
    #[schemars(extend("exclusiveMinimum" = 0.0))]
    pub weight: f32,
    /// 1, 2 or 4 quarter-turns about Y.
    #[serde(default = "default_rotations")]
    #[schemars(range(min = 1, max = 4))]
    pub rotations: u32,
    #[serde(default)]
    pub faces: Faces,
    #[serde(default)]
    #[schemars(length(max = 32))]
    pub parts: Vec<Part>,
}

fn default_weight() -> f32 {
    1.0
}

fn default_rotations() -> u32 {
    1
}

/// An inclusive `[min, max]`, either end optional.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Bounds {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<u32>,
}

impl Bounds {
    pub fn admits(&self, value: usize) -> bool {
        self.min.is_none_or(|m| value >= m as usize) && self.max.is_none_or(|m| value <= m as usize)
    }

    /// Whether this bound can be satisfied at all — a `min` above a `max` is an
    /// authoring mistake that would otherwise reject every block silently.
    pub fn is_satisfiable(&self) -> bool {
        match (self.min, self.max) {
            (Some(min), Some(max)) => min <= max,
            _ => true,
        }
    }

    pub fn describe(&self) -> String {
        match (self.min, self.max) {
            (Some(min), Some(max)) => format!("between {min} and {max}"),
            (Some(min), None) => format!("at least {min}"),
            (None, Some(max)) => format!("at most {max}"),
            (None, None) => "any number of".to_string(),
        }
    }
}

/// What each region of a constraint's tiles must hold.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RegionContains {
    /// Authored tile names to count inside each region.
    pub tiles: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<u32>,
}

/// A property of the solved grid that face adjacency cannot state (M49).
///
/// A socket is a statement about one *interface*. "This mass of walls encloses
/// a room", "the street reaches everywhere", "a building is smaller than the
/// village" are statements about a **region**, and constraint propagation over
/// adjacency has no representation for one. Without them the M47 village came
/// out as a single 60-cell mass covering 75% of its grid, and the tour's hamlet
/// as 24 wall pieces enclosing zero rooms.
///
/// One shape, four optional predicates, over a set of tiles named by their
/// **authored** names — four rotations of a wall are one kind of wall to
/// whoever writes this, and a constraint listing `wall@0..wall@3` would stop
/// covering a tile whose `rotations` changed.
///
/// Evaluated over the **ground layer**, 4-connected in XZ. A terrace step does
/// not split a region: the lift moves geometry, and two columns at different
/// lifts are still neighbours on the ground plan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Constraint {
    /// What this rule is for. Quoted back when it rejects a block, which is the
    /// difference between "the village came out bland" and a named cause.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// The authored tile names this applies to.
    pub tiles: Vec<String>,
    /// How many such cells the grid holds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<Bounds>,
    /// How many connected regions they form. `max: 1` is connectivity, and is
    /// why this needs no separate kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regions: Option<Bounds>,
    /// The size of each region — the predicate that breaks one sprawling mass
    /// into buildings, and that refuses a lone wall standing on its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region_size: Option<Bounds>,
    /// What each region must contain — "a building has a room in it".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region_contains: Option<RegionContains>,
}

impl Constraint {
    /// A label for messages: the author's `name`, else the tiles it is about.
    pub fn label(&self) -> String {
        if self.name.is_empty() {
            format!("the constraint on {}", self.tiles.join(", "))
        } else {
            format!("{:?}", self.name)
        }
    }
}

/// A tileset file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Tileset {
    /// Metres per cell. Every part's coordinates are inside this box.
    #[serde(default = "default_cell")]
    #[schemars(with = "[f32; 3]")]
    pub cell: Vec3,
    /// Named materials the parts paint with. One `RenderItem` is drawn per
    /// entry that any placed tile uses, so this list is also the grid's draw
    /// count.
    #[serde(default)]
    pub palette: BTreeMap<String, Material>,
    pub tiles: Vec<TileDef>,
    /// Region properties the solver must satisfy (M49). Empty is M47 exactly.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub constraints: Vec<Constraint>,
}

fn default_cell() -> Vec3 {
    Vec3::splat(2.0)
}

impl Tileset {
    /// Whether a JSON object looks like a tileset — how `engine validate`
    /// routes a file by shape rather than by filename, the way it already does
    /// for clips and materials.
    pub fn looks_like(value: &serde_json::Value) -> bool {
        value.get("tiles").is_some()
            && value.get("entities").is_none()
            && value.get("tracks").is_none()
    }

    pub fn tile(&self, name: &str) -> Option<&TileDef> {
        self.tiles.iter().find(|t| t.name == name)
    }
}

// ── Expansion ─────────────────────────────────────────────────────────

/// One tile at one rotation: what the solver places and what a layout names.
#[derive(Debug, Clone, PartialEq)]
pub struct ExpandedTile {
    /// Index into [`Tileset::tiles`].
    pub tile: usize,
    pub rotation: u8,
    /// `name@rotation`, the token a layout row carries.
    pub token: String,
    pub weight: f32,
    /// Sockets in direction-index order.
    pub faces: [FaceSocket; 6],
}

/// Expand every tile over its rotations, in authored order then rotation order.
///
/// The order is a format contract: a layout's tokens are `name@rotation`, but
/// the solver's bitsets are indexed by position in this list, and a stored
/// digest covers it.
pub fn expand(tileset: &Tileset) -> Result<Vec<ExpandedTile>> {
    let mut expanded = Vec::new();
    for (index, tile) in tileset.tiles.iter().enumerate() {
        let turns = tile.rotations.clamp(1, 4) as u8;
        for rotation in 0..turns {
            let mut faces: [FaceSocket; 6] = std::array::from_fn(|_| FaceSocket {
                socket: Socket::Empty,
                turn: None,
            });
            for face in Face::ALL {
                // Walk the permutation `rotation` times: what is at `face`
                // now came from `source_after_turn` applied that many times.
                let mut source = face;
                for _ in 0..rotation {
                    source = source.source_after_turn();
                }
                let text = tile.faces.get(source);
                let (socket, invariant) = parse_socket(text).map_err(|message| {
                    EngineError::new(crate::codes::UNKNOWN_SOCKET_FORM, message)
                        .field(Face::KEYS[face.index()])
                })?;
                let turn = (face.is_vertical() && !invariant && !matches!(socket, Socket::Empty))
                    .then_some(rotation);
                faces[face.index()] = FaceSocket { socket, turn };
            }
            expanded.push(ExpandedTile {
                tile: index,
                rotation,
                token: format!("{}@{}", tile.name, rotation),
                weight: tile.weight,
                faces,
            });
        }
    }

    if expanded.len() > MAX_TILES {
        return Err(EngineError::new(
            crate::codes::TILESET_TOO_COMPLEX,
            format!(
                "the tileset expands to {} tiles over their rotations, more than \
                 the {MAX_TILES} the solver indexes",
                expanded.len()
            ),
        ));
    }
    Ok(expanded)
}

/// A digest of everything about a tileset that changes what a solve or a draw
/// produces.
///
/// Content rather than file bytes, so reformatting a tileset does not
/// invalidate every layout that names it while any change to a socket, a
/// weight, a part or a palette entry does. Feeds both a layout's
/// `tileset_hash` and the merged-geometry cache key.
pub fn digest(tileset: &Tileset) -> String {
    let mut h = crate::gi::InputsHasher::new();
    h.vec3(tileset.cell);
    h.u32(tileset.palette.len() as u32);
    for (key, material) in &tileset.palette {
        h.str(key);
        // The material through its own serialization: it is plain data, and
        // spelling out twenty fields here is a list that silently stops
        // matching the day one is added.
        h.str(&serde_json::to_string(material).unwrap_or_default());
    }
    // Fed in only when there are some, so a tileset that does not use M49
    // digests exactly as it did — and every layout solved before this milestone
    // stays current rather than reporting stale for a field it never had. The
    // house rule that let M16 add five features without re-blessing anything.
    if !tileset.constraints.is_empty() {
        h.u32(tileset.constraints.len() as u32);
        for constraint in &tileset.constraints {
            h.str(&serde_json::to_string(constraint).unwrap_or_default());
        }
    }
    h.u32(tileset.tiles.len() as u32);
    for tile in &tileset.tiles {
        h.str(&tile.name);
        h.f32(tile.weight);
        h.u32(tile.rotations);
        for face in Face::ALL {
            h.str(tile.faces.get(face));
        }
        h.u32(tile.parts.len() as u32);
        for part in &tile.parts {
            h.str(&serde_json::to_string(part).unwrap_or_default());
        }
    }
    h.finish()
}

// ── Adjacency ─────────────────────────────────────────────────────────

/// Which expanded tiles may sit in which direction from which, as a bitset per
/// (direction, tile).
#[derive(Debug, Clone, PartialEq)]
pub struct Compat {
    pub tiles: usize,
    words: usize,
    bits: Vec<u64>,
}

impl Compat {
    /// Build the table by mating every ordered pair across every direction.
    ///
    /// `O(6 · T²)` socket comparisons at up to `T = 512`, which is a
    /// millisecond and is done once per solve rather than per cell.
    pub fn build(tiles: &[ExpandedTile]) -> Compat {
        let count = tiles.len();
        let words = count.div_ceil(64);
        let mut bits = vec![0u64; 6 * count * words];
        for face in Face::ALL {
            let dir = face.index();
            let across = face.opposite().index();
            for (a, here) in tiles.iter().enumerate() {
                for (b, there) in tiles.iter().enumerate() {
                    if here.faces[dir].mates(&there.faces[across]) {
                        let base = (dir * count + a) * words;
                        bits[base + b / 64] |= 1u64 << (b % 64);
                    }
                }
            }
        }
        Compat {
            tiles: count,
            words,
            bits,
        }
    }

    pub fn words(&self) -> usize {
        self.words
    }

    /// The bitset of tiles that may sit in direction `dir` from tile `a`.
    pub fn row(&self, dir: usize, a: usize) -> &[u64] {
        let base = (dir * self.tiles + a) * self.words;
        &self.bits[base..base + self.words]
    }

    pub fn allows(&self, dir: usize, a: usize, b: usize) -> bool {
        self.row(dir, a)[b / 64] & (1u64 << (b % 64)) != 0
    }

    /// How many tiles may sit in direction `dir` from tile `a` — what
    /// `engine list-tiles` prints, and what makes an orphaned socket visible.
    pub fn partners(&self, dir: usize, a: usize) -> usize {
        self.row(dir, a)
            .iter()
            .map(|word| word.count_ones() as usize)
            .sum()
    }
}

// ── Geometry ──────────────────────────────────────────────────────────

/// Grow one placed tile's geometry into per-palette meshes.
///
/// `origin` is the cell's own origin in the grid's local space — its centre in
/// X and Z, its floor in Y — and positions come out there rather than in cell
/// space, because **UVs are a planar projection of the final position** and a
/// per-cell projection would make an eight-tile wall read as eight stamps
/// rather than one wall (`shard.rs`'s choice, for the same reason).
pub fn grow_tile(
    tileset: &Tileset,
    expanded: &ExpandedTile,
    origin: Vec3,
    out: &mut BTreeMap<String, MeshData>,
) {
    let tile = &tileset.tiles[expanded.tile];
    let turn = f32::from(expanded.rotation) * FRAC_PI_2;
    let placement = Mat3::from_rotation_y(turn);

    for part in &tile.parts {
        let mesh = out.entry(part.material.clone()).or_default();
        // The tile's turn moves the part's centre; the part's own `yaw` rides
        // on top of it. Building the primitive axis-aligned and rotating the
        // whole thing is what keeps `size` meaning one thing — a quarter-turn
        // does not have to swap its components anywhere.
        let rotation = Mat3::from_rotation_y(turn + part.yaw.to_radians());
        let centre = origin + placement * part.at;
        match part.kind {
            PartKind::Box => grow_box(mesh, part.size, rotation, centre),
            PartKind::Wedge => grow_wedge(mesh, part, rotation, centre),
            PartKind::Cylinder => grow_cylinder(mesh, part, rotation, centre),
        }
    }
}

/// Vertices one part grows, exactly — the term of the grid's budget.
pub fn part_vertex_count(part: &Part) -> usize {
    match part.kind {
        // Six quads, each with its own four vertices: flat shading means no
        // vertex is shared across faces.
        PartKind::Box => 24,
        // Three quads and two triangles.
        PartKind::Wedge => 18,
        // `sides` side quads, and a flat-shaded fan per cap.
        PartKind::Cylinder => 10 * part.sides.clamp(3, 32) as usize,
    }
}

/// Vertices one expanded tile grows.
pub fn tile_vertex_count(tileset: &Tileset, expanded: &ExpandedTile) -> usize {
    tileset.tiles[expanded.tile]
        .parts
        .iter()
        .map(part_vertex_count)
        .sum()
}

/// A quad, counter-clockwise seen from outside. Flat-shaded: its four
/// vertices are its own, and its normal comes from its own winding.
fn push_quad(mesh: &mut MeshData, corners: [Vec3; 4]) {
    let normal = (corners[1] - corners[0])
        .cross(corners[2] - corners[0])
        .normalize_or_zero();
    let base = mesh.positions.len() as u32;
    push_ring(mesh, &corners, normal);
    mesh.indices
        .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
}

/// A triangle, counter-clockwise seen from outside.
fn push_tri(mesh: &mut MeshData, corners: [Vec3; 3]) {
    let normal = (corners[1] - corners[0])
        .cross(corners[2] - corners[0])
        .normalize_or_zero();
    let base = mesh.positions.len() as u32;
    push_ring(mesh, &corners, normal);
    mesh.indices
        .extend_from_slice(&[base, base + 1, base + 2]);
}

fn push_ring(mesh: &mut MeshData, corners: &[Vec3], normal: Vec3) {
    let (u_axis, v_axis) = basis(normal);
    for corner in corners {
        mesh.positions.push(corner.to_array());
        mesh.normals.push(normal.to_array());
        // Planar, in metres, off the grid-local position — so neighbouring
        // cells continue one another's texture rather than restarting it.
        mesh.uvs.push([u_axis.dot(*corner), v_axis.dot(*corner)]);
    }
}

/// Two unit axes spanning the plane a normal defines, chosen deterministically.
/// `shard.rs`'s basis, which the UV convention is shared with.
fn basis(normal: Vec3) -> (Vec3, Vec3) {
    let seed = if normal.x.abs() < 0.9 { Vec3::X } else { Vec3::Y };
    let u_axis = seed.cross(normal).normalize_or_zero();
    let u_axis = if u_axis == Vec3::ZERO { Vec3::X } else { u_axis };
    (u_axis, normal.cross(u_axis))
}

fn grow_box(mesh: &mut MeshData, size: Vec3, rotation: Mat3, centre: Vec3) {
    let h = size.abs() * 0.5;
    let place = |x: f32, y: f32, z: f32| centre + rotation * Vec3::new(x, y, z);
    let (hx, hy, hz) = (h.x, h.y, h.z);

    push_quad(
        mesh,
        [
            place(hx, -hy, hz),
            place(hx, -hy, -hz),
            place(hx, hy, -hz),
            place(hx, hy, hz),
        ],
    );
    push_quad(
        mesh,
        [
            place(-hx, -hy, -hz),
            place(-hx, -hy, hz),
            place(-hx, hy, hz),
            place(-hx, hy, -hz),
        ],
    );
    push_quad(
        mesh,
        [
            place(-hx, hy, hz),
            place(hx, hy, hz),
            place(hx, hy, -hz),
            place(-hx, hy, -hz),
        ],
    );
    push_quad(
        mesh,
        [
            place(-hx, -hy, -hz),
            place(hx, -hy, -hz),
            place(hx, -hy, hz),
            place(-hx, -hy, hz),
        ],
    );
    push_quad(
        mesh,
        [
            place(-hx, -hy, hz),
            place(hx, -hy, hz),
            place(hx, hy, hz),
            place(-hx, hy, hz),
        ],
    );
    push_quad(
        mesh,
        [
            place(hx, -hy, -hz),
            place(-hx, -hy, -hz),
            place(-hx, hy, -hz),
            place(hx, hy, -hz),
        ],
    );
}

/// A right prism with a triangular cross-section: full height at the `rise`
/// face, nothing at the opposite one.
///
/// Built rising toward `+X` and turned onto its `rise`, which is why the extent
/// swaps its X and Z for an odd number of turns — the rotation swaps them back.
fn grow_wedge(mesh: &mut MeshData, part: &Part, rotation: Mat3, centre: Vec3) {
    let turns = part.rise.turns();
    let size = if turns % 2 == 1 {
        Vec3::new(part.size.z, part.size.y, part.size.x)
    } else {
        part.size
    };
    let h = size.abs() * 0.5;
    let (hx, hy, hz) = (h.x, h.y, h.z);
    let rotation = rotation * Mat3::from_rotation_y(f32::from(turns) * FRAC_PI_2);
    let place = |x: f32, y: f32, z: f32| centre + rotation * Vec3::new(x, y, z);

    // The tall face, at +X.
    push_quad(
        mesh,
        [
            place(hx, -hy, hz),
            place(hx, -hy, -hz),
            place(hx, hy, -hz),
            place(hx, hy, hz),
        ],
    );
    // The floor.
    push_quad(
        mesh,
        [
            place(-hx, -hy, -hz),
            place(hx, -hy, -hz),
            place(hx, -hy, hz),
            place(-hx, -hy, hz),
        ],
    );
    // The slope, from the low edge at −X up to the top edge at +X.
    push_quad(
        mesh,
        [
            place(-hx, -hy, hz),
            place(hx, hy, hz),
            place(hx, hy, -hz),
            place(-hx, -hy, -hz),
        ],
    );
    // The two triangular ends.
    push_tri(
        mesh,
        [
            place(-hx, -hy, hz),
            place(hx, -hy, hz),
            place(hx, hy, hz),
        ],
    );
    push_tri(
        mesh,
        [
            place(hx, -hy, -hz),
            place(-hx, -hy, -hz),
            place(hx, hy, -hz),
        ],
    );
}

/// An `n`-sided prism about the part's local Y, flat-shaded — a low-poly
/// column reads as facets, and smoothing them turns a post into a pipe.
fn grow_cylinder(mesh: &mut MeshData, part: &Part, rotation: Mat3, centre: Vec3) {
    let sides = part.sides.clamp(3, 32) as usize;
    let h = part.size.abs() * 0.5;
    let place = |x: f32, y: f32, z: f32| centre + rotation * Vec3::new(x, y, z);
    let rim = |i: usize, y: f32| {
        let angle = std::f32::consts::TAU * i as f32 / sides as f32;
        place(h.x * angle.cos(), y, h.z * angle.sin())
    };

    for i in 0..sides {
        let next = (i + 1) % sides;
        push_quad(
            mesh,
            [rim(i, -h.y), rim(i, h.y), rim(next, h.y), rim(next, -h.y)],
        );
    }
    let top = place(0.0, h.y, 0.0);
    let bottom = place(0.0, -h.y, 0.0);
    for i in 0..sides {
        let next = (i + 1) % sides;
        push_tri(mesh, [top, rim(next, h.y), rim(i, h.y)]);
        push_tri(mesh, [bottom, rim(i, -h.y), rim(next, -h.y)]);
    }
}

// ── Loading ───────────────────────────────────────────────────────────

/// Resolve a `TileGrid.tileset` reference against the scene file's directory.
///
/// The checks that can be made without parsing, `material::resolve_material`'s
/// shape.
pub fn resolve_tileset(asset: &str, base_dir: &Path) -> Result<PathBuf> {
    if Path::new(asset).is_absolute() {
        return Err(EngineError::new(
            crate::codes::ASSET_PATH_NOT_RELATIVE,
            format!(
                "tileset {asset:?} is an absolute path; assets are referenced \
                 by path relative to the scene file, so scenes stay portable"
            ),
        ));
    }

    let resolved = base_dir.join(asset);
    match resolved.extension().and_then(|e| e.to_str()) {
        Some(ext) if ext.eq_ignore_ascii_case(TILESET_EXTENSION) => {}
        _ => {
            return Err(EngineError::new(
                crate::codes::ASSET_UNSUPPORTED,
                format!("tileset {asset:?} must be a .{TILESET_EXTENSION} file"),
            ));
        }
    }

    if !resolved.is_file() {
        return Err(EngineError::new(
            crate::codes::TILESET_NOT_FOUND,
            format!(
                "no tileset file at {} (asset paths resolve relative to the scene file)",
                resolved.display()
            ),
        ));
    }

    Ok(resolved)
}

/// Read a tileset, inlining any palette entry that names a `materials/*.json`.
///
/// **A tileset file's own references are relative to itself**, which is what
/// makes a tileset shareable at all — the rule `material.rs` states for texture
/// maps, applied one level up. So a palette's `asset` resolves against the
/// tileset's directory here, and [`rebase_tileset`] moves what is left onto the
/// scene afterwards.
pub fn load_tileset(path: &Path) -> Result<Tileset> {
    let display = path.display().to_string();
    let source = std::fs::read_to_string(path).map_err(|e| {
        EngineError::new(
            crate::codes::TILESET_NOT_FOUND,
            format!("could not read tileset {display}: {e}"),
        )
        .file(&display)
    })?;
    let mut tileset: Tileset = serde_json::from_str(&source).map_err(|e| {
        EngineError::new(
            crate::codes::TILESET_MALFORMED,
            format!("tileset {display} does not parse: {e}"),
        )
        .file(&display)
        .line(e.line() as u32)
    })?;

    let dir = path.parent().unwrap_or(Path::new(""));
    for (key, material) in &mut tileset.palette {
        let Some(asset) = material.asset.clone() else {
            continue;
        };
        let mut loaded = crate::material::resolve_material(&asset, dir)
            .and_then(|found| crate::material::load_material(&found))
            .map_err(|e| e.file(&display).field(key))?;
        crate::material::rebase_maps(&mut loaded, &asset);
        loaded.asset = Some(asset);
        *material = loaded;
    }

    Ok(tileset)
}

/// Move a loaded tileset's texture references from tileset-relative to
/// scene-relative, given the reference the scene wrote.
///
/// The second half of the rule above: everything downstream resolves against
/// the scene, so the join happens once, here.
pub fn rebase_tileset(tileset: &mut Tileset, tileset_asset: &str) {
    for material in tileset.palette.values_mut() {
        crate::material::rebase_maps(material, tileset_asset);
        if let Some(asset) = &material.asset {
            material.asset = Some(crate::material::rebase(tileset_asset, asset));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wall() -> Tileset {
        Tileset {
            cell: Vec3::new(2.0, 2.5, 2.0),
            palette: BTreeMap::from([("stone".to_string(), Material::default())]),
            tiles: vec![TileDef {
                name: "wall".into(),
                weight: 1.0,
                rotations: 4,
                faces: Faces {
                    px: "in".into(),
                    nx: "face".into(),
                    py: "top".into(),
                    ny: "0".into(),
                    pz: "run".into(),
                    nz: "run".into(),
                },
                parts: vec![Part {
                    kind: PartKind::Box,
                    at: Vec3::new(-0.85, 1.25, 0.0),
                    size: Vec3::new(0.3, 2.5, 2.0),
                    rise: Rise::Px,
                    yaw: 0.0,
                    sides: 8,
                    material: "stone".into(),
                }],
            }],
            constraints: Vec::new(),
        }
    }

    #[test]
    fn plain_is_symmetric_and_a_pair_is_spelled_out() {
        let (plain, _) = parse_socket("wall").unwrap();
        assert!(plain.mates(&plain), "a plain socket meets itself");

        let (left, _) = parse_socket("step_l").unwrap();
        let (right, _) = parse_socket("step_r").unwrap();
        assert!(left.mates(&right) && right.mates(&left));
        assert!(!left.mates(&left), "half a mirrored pair refuses itself");
        assert!(!left.mates(&plain), "and refuses a symmetric socket");

        let (empty, invariant) = parse_socket("0").unwrap();
        assert_eq!(empty, Socket::Empty);
        assert!(empty.mates(&empty), "nothing meets nothing");
        assert!(!invariant, "\"0\" carries no _i of its own");

        assert!(parse_socket("wall_l_r").is_err(), "both halves at once");
        assert!(parse_socket("0_l").is_err(), "the empty socket has no sides");
        assert!(parse_socket("_i").is_err(), "all suffix, no interface");
    }

    #[test]
    fn mating_is_symmetric_across_every_pair() {
        let tiles = expand(&wall()).unwrap();
        let compat = Compat::build(&tiles);
        for face in Face::ALL {
            let dir = face.index();
            let back = face.opposite().index();
            for a in 0..tiles.len() {
                for b in 0..tiles.len() {
                    assert_eq!(
                        compat.allows(dir, a, b),
                        compat.allows(back, b, a),
                        "{} beside {} disagrees across {:?}",
                        tiles[a].token,
                        tiles[b].token,
                        face
                    );
                }
            }
        }
    }

    /// The four lines the whole milestone rests on, checked against geometry
    /// rather than against a table: after one turn, the face whose *parts*
    /// moved onto `+X` must carry the socket the unturned tile wrote on `+Z`.
    #[test]
    fn a_turn_moves_geometry_and_sockets_together() {
        let tileset = wall();
        let tiles = expand(&tileset).unwrap();

        for rotation in 0..4u8 {
            let expanded = &tiles[rotation as usize];
            let mut grown = BTreeMap::new();
            grow_tile(&tileset, expanded, Vec3::ZERO, &mut grown);
            let mesh = &grown["stone"];

            // Where the slab actually sits after the turn.
            let centre = mesh
                .positions
                .iter()
                .map(|p| Vec3::from_array(*p))
                .sum::<Vec3>()
                / mesh.positions.len() as f32;
            // The unturned slab hugs −X, so the turned one hugs the image of
            // −X, and the face it hugs is the one carrying "face".
            let hugged = match rotation {
                0 => Face::NegX,
                1 => Face::PosZ,
                2 => Face::PosX,
                _ => Face::NegZ,
            };
            let step = hugged.step();
            let toward = Vec3::new(step[0] as f32, 0.0, step[2] as f32);
            assert!(
                centre.dot(toward) > 0.5,
                "at rotation {rotation} the slab is at {centre}, not against {hugged:?}"
            );
            assert_eq!(
                expanded.faces[hugged.index()].socket,
                Socket::Symmetric("face".into()),
                "at rotation {rotation} the socket did not follow the geometry"
            );
        }
    }

    #[test]
    fn a_vertical_socket_keeps_its_turn_unless_it_opts_out() {
        let tiles = expand(&wall()).unwrap();
        assert_eq!(tiles[0].faces[Face::PosY.index()].turn, Some(0));
        assert_eq!(tiles[2].faces[Face::PosY.index()].turn, Some(2));
        assert!(
            !tiles[0].faces[Face::PosY.index()].mates(&tiles[2].faces[Face::NegY.index()]),
            "a wall does not stack under a wall turned away from it"
        );

        let mut invariant = wall();
        invariant.tiles[0].faces.py = "top_i".into();
        invariant.tiles[0].faces.ny = "top_i".into();
        let tiles = expand(&invariant).unwrap();
        assert_eq!(tiles[0].faces[Face::PosY.index()].turn, None);
        assert!(
            tiles[0].faces[Face::PosY.index()].mates(&tiles[2].faces[Face::NegY.index()]),
            "_i drops the rotation index"
        );
    }

    #[test]
    fn horizontal_faces_never_carry_a_turn() {
        for tile in expand(&wall()).unwrap() {
            for face in Face::ALL.into_iter().filter(|f| !f.is_vertical()) {
                assert_eq!(tile.faces[face.index()].turn, None, "{:?}", face);
            }
        }
    }

    #[test]
    fn every_primitive_winds_outward() {
        let mut tileset = wall();
        tileset.tiles[0].parts = vec![
            Part {
                kind: PartKind::Box,
                at: Vec3::ZERO,
                size: Vec3::new(1.0, 2.0, 3.0),
                rise: Rise::Px,
                yaw: 0.0,
                sides: 8,
                material: "stone".into(),
            },
            Part {
                kind: PartKind::Wedge,
                at: Vec3::new(6.0, 0.0, 0.0),
                size: Vec3::new(2.0, 1.5, 1.0),
                rise: Rise::Nz,
                yaw: 0.0,
                sides: 8,
                material: "stone".into(),
            },
            Part {
                kind: PartKind::Cylinder,
                at: Vec3::new(-6.0, 0.0, 0.0),
                size: Vec3::new(1.0, 3.0, 1.0),
                rise: Rise::Px,
                yaw: 17.0,
                sides: 7,
                material: "stone".into(),
            },
        ];
        let tiles = expand(&tileset).unwrap();
        let mut grown = BTreeMap::new();
        grow_tile(&tileset, &tiles[1], Vec3::new(4.0, 0.0, -2.0), &mut grown);
        let mesh = &grown["stone"];

        assert_eq!(
            mesh.vertex_count(),
            tile_vertex_count(&tileset, &tiles[1]),
            "the budget must be exact, not an estimate"
        );
        assert_eq!(mesh.normals.len(), mesh.positions.len());
        assert_eq!(mesh.uvs.len(), mesh.positions.len());

        // Each primitive is convex, so a triangle's winding is outward exactly
        // when its normal points away from that primitive's own centre.
        for triangle in mesh.indices.chunks_exact(3) {
            let [a, b, c] = [0, 1, 2].map(|i| Vec3::from_array(mesh.positions[triangle[i] as usize]));
            let facing = (b - a).cross(c - a);
            let stored = Vec3::from_array(mesh.normals[triangle[0] as usize]);
            assert!(
                facing.normalize_or_zero().dot(stored) > 0.99,
                "triangle {triangle:?} disagrees with its own normal"
            );
        }
    }

    #[test]
    fn a_wedge_is_tall_at_its_rise_and_flat_opposite() {
        let mut tileset = wall();
        for (rise, toward) in [
            (Rise::Px, Vec3::X),
            (Rise::Nx, -Vec3::X),
            (Rise::Pz, Vec3::Z),
            (Rise::Nz, -Vec3::Z),
        ] {
            tileset.tiles[0].parts = vec![Part {
                kind: PartKind::Wedge,
                at: Vec3::new(0.0, 1.0, 0.0),
                size: Vec3::new(2.0, 2.0, 2.0),
                rise,
                yaw: 0.0,
                sides: 8,
                material: "stone".into(),
            }];
            let tiles = expand(&tileset).unwrap();
            let mut grown = BTreeMap::new();
            grow_tile(&tileset, &tiles[0], Vec3::ZERO, &mut grown);

            let tall: Vec<Vec3> = grown["stone"]
                .positions
                .iter()
                .map(|p| Vec3::from_array(*p))
                .filter(|p| p.y > 1.5)
                .collect();
            assert!(!tall.is_empty(), "{rise:?} grew nothing tall");
            for point in tall {
                assert!(
                    point.dot(toward) > 0.5,
                    "{rise:?} put a high vertex at {point}, away from its rise"
                );
            }
        }
    }

    #[test]
    fn tiles_at_neighbouring_cells_continue_one_anothers_uvs() {
        let tileset = wall();
        let tiles = expand(&tileset).unwrap();
        let mut here = BTreeMap::new();
        let mut there = BTreeMap::new();
        grow_tile(&tileset, &tiles[0], Vec3::ZERO, &mut here);
        grow_tile(&tileset, &tiles[0], Vec3::new(0.0, 0.0, 2.0), &mut there);
        assert_ne!(
            here["stone"].uvs, there["stone"].uvs,
            "a per-cell projection would make an eight-tile wall eight stamps"
        );
    }

    #[test]
    fn expansion_order_is_authored_then_rotation() {
        let mut tileset = wall();
        tileset.tiles.push(TileDef {
            name: "floor".into(),
            weight: 2.0,
            rotations: 1,
            faces: Faces::default(),
            parts: Vec::new(),
        });
        let tokens: Vec<String> = expand(&tileset)
            .unwrap()
            .into_iter()
            .map(|t| t.token)
            .collect();
        assert_eq!(
            tokens,
            ["wall@0", "wall@1", "wall@2", "wall@3", "floor@0"],
            "the order is a format contract: bitsets index into it"
        );
    }

    /// A tileset that declares no constraints digests exactly as one written
    /// before the field existed — which is what keeps every layout solved
    /// before M49 from reporting stale for a feature it never used.
    #[test]
    fn an_absent_constraint_list_does_not_reach_the_digest() {
        let plain = wall();
        let mut empty = wall();
        empty.constraints = Vec::new();
        assert_eq!(digest(&plain), digest(&empty));

        let mut constrained = wall();
        constrained.constraints = vec![Constraint {
            name: "something".into(),
            tiles: vec!["wall".into()],
            count: Some(Bounds {
                min: Some(1),
                max: None,
            }),
            regions: None,
            region_size: None,
            region_contains: None,
        }];
        assert_ne!(
            digest(&plain),
            digest(&constrained),
            "but an edited rule must move it, or a stale layout reads as current"
        );
    }

    #[test]
    fn too_many_tiles_is_an_error_rather_than_a_silent_truncation() {
        let mut tileset = wall();
        tileset.tiles = (0..(MAX_TILES / 4 + 1))
            .map(|i| TileDef {
                name: format!("t{i}"),
                weight: 1.0,
                rotations: 4,
                faces: Faces::default(),
                parts: Vec::new(),
            })
            .collect();
        assert_eq!(
            expand(&tileset).unwrap_err().error,
            crate::codes::TILESET_TOO_COMPLEX
        );
    }
}
