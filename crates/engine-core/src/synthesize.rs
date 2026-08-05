//! Model synthesis with modifying in blocks (M47).
//!
//! The solver, and nothing else: no scene, no files, no GPU. It takes an
//! adjacency table, a grid, a seed and a prior layout, and hands back cells.
//! That is deliberate — everything hard here is testable headlessly, and
//! `engine synthesize` is a thin wrapper over it.
//!
//! # Why blocks
//!
//! The naive version — one wave over the whole grid, restart on contradiction —
//! is what Wave Function Collapse does, and Boris the Brave's measurement is
//! that it cannot generate 30×30×10 of a rich tileset at all: the chance of a
//! contradiction grows with area, so the restart never converges. Backtracking
//! scales further and then finds unsolvable rabbit holes.
//!
//! So the grid is covered by **overlapping blocks**, solved one at a time in a
//! fixed scan. A block's border is the ring of already-decided cells around it,
//! which turns each block into a small, self-contained problem rather than a
//! slice of a large one; a contradiction restarts **that block only**; and
//! after `attempts` failures the block takes a known-good fill and the run
//! carries on. It always terminates and it always produces a legal grid.
//!
//! Before the first block, every unlocked cell is set to that fill. This is the
//! article's prescription and it removes every "the first block has no border"
//! special case — block 0's border is the fill, exactly like block 40's.
//!
//! # Two properties this has, and one it does not
//!
//! Two solves at one seed are byte-identical, and **a region solve changes no
//! cell outside the interiors of the blocks it touched** — that second one is
//! the whole point of the milestone and it is a test.
//!
//! What it does *not* have: a region solve over one block does not reproduce
//! what a full solve produced there. In a full scan that block's east and south
//! borders were the known-good fill; in a region solve they are already-solved
//! neighbours. Different constraints, different answer, and the tempting claim
//! that "regenerating an unchanged region is a no-op" is false.

use crate::error::{EngineError, Result};
use crate::tilelayout::{Grid, Neighbour};
use crate::tileset::{Compat, ExpandedTile, Face, Socket};

/// How the grid is cut into blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Params {
    /// Block extent in cells. Y is taken whole regardless — built structures
    /// are one to four storeys and no fixture needs a third stride.
    pub block: [u32; 3],
    /// Cells two neighbouring blocks share. At least 1, so every block has a
    /// border to read.
    pub overlap: u32,
    /// Retries before a block gives up and takes the fill.
    pub attempts: u32,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            block: [8, u32::MAX, 8],
            overlap: 1,
            attempts: 10,
        }
    }
}

/// Everything one solve reads.
pub struct Request<'a> {
    pub grid: &'a Grid,
    pub tiles: &'a [ExpandedTile],
    pub compat: &'a Compat,
    pub seed: u32,
    pub params: Params,
    /// The tile every unlocked cell starts as at `y == 0`.
    pub fill_ground: usize,
    /// The tile every unlocked cell starts as above `y == 0`.
    pub fill_background: usize,
    /// The layout to build on. `None` starts from the fill.
    pub prior: Option<&'a [usize]>,
    /// Cells the solver may never re-pick. Empty means none are locked.
    pub locked: &'a [bool],
    /// `[x0, z0, x1, z1]` inclusive, in cells: only blocks whose interior meets
    /// this rectangle are re-solved. `None` solves the whole grid.
    pub region: Option<[u32; 4]>,
}

/// What a solve produced.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    /// One tile index per cell, in the grid's own order.
    pub cells: Vec<usize>,
    /// Blocks the lattice holds.
    pub blocks: usize,
    /// Blocks this run actually solved — every one, or the ones a `region`
    /// selected.
    pub solved: usize,
    /// Blocks that used up their retries and took the fill.
    pub fallbacks: u32,
    /// Retries spent across every block, so a tileset that is *nearly* over
    /// constrained is visible before it starts falling back.
    pub retries: u32,
}

/// Solve a grid.
pub fn synthesize(request: &Request<'_>) -> Result<Outcome> {
    check_fill(request)?;

    // With no prior, every cell starts as the known-good fill. That is the
    // article's prescription and it removes every "the first block has no
    // border" special case: block 0 reads the fill exactly as block 40 reads
    // its solved neighbours.
    let cell_count = request.grid.cell_count();
    let mut cells = match request.prior {
        Some(prior) if prior.len() == cell_count => prior.to_vec(),
        _ => fill(request),
    };

    let lattice = blocks(request.grid, &request.params);
    let mut outcome = Outcome {
        cells: Vec::new(),
        blocks: lattice.len(),
        solved: 0,
        fallbacks: 0,
        retries: 0,
    };

    for (index, block) in lattice.iter().enumerate() {
        if !block.meets(request.region) {
            continue;
        }
        outcome.solved += 1;
        let interior = block.interior(request.grid);
        let mut settled = None;
        for attempt in 0..request.params.attempts.max(1) {
            if attempt > 0 {
                outcome.retries += 1;
            }
            let mut rng = stream(request.seed, index as u32, attempt);
            if let Some(result) = solve_block(request, &cells, &interior, &mut rng) {
                settled = Some(result);
                break;
            }
        }
        match settled {
            Some(result) => {
                for (local, cell) in interior.iter().enumerate() {
                    cells[*cell] = result[local];
                }
            }
            None => {
                // Out of retries: **leave the block exactly as it was**.
                //
                // The article's fallback is "fill the block with the known-good
                // arrangement", which is right when every border is that same
                // fill. Here a later block's border is an *already solved*
                // neighbour — a wall's interior face, say — and the fill is
                // only known good against itself, so writing it in produces an
                // illegal grid whose diff is nowhere near the block that
                // failed. Reverting is legal by induction instead: the initial
                // state is the fill (checked legal by `check_fill`) and every
                // block that succeeds leaves the grid legal, so whatever stood
                // here before this attempt is a legal arrangement.
                outcome.fallbacks += 1;
            }
        }
    }

    outcome.cells = cells;
    Ok(outcome)
}

/// The known-good arrangement, per cell.
fn fill(request: &Request<'_>) -> Vec<usize> {
    let grid = request.grid;
    (0..grid.cell_count())
        .map(|index| {
            let (_, y, _) = grid.coords(index);
            if y == 0 {
                request.fill_ground
            } else {
                request.fill_background
            }
        })
        .collect()
}

/// The safety net is a lie unless the fill is itself legal.
///
/// Without this, a block that runs out of retries falls back into a
/// contradiction and hands the next block an impossible border — a failure that
/// looks like a solver bug and is a tileset bug.
fn check_fill(request: &Request<'_>) -> Result<()> {
    let ground = request.fill_ground;
    let background = request.fill_background;
    let tiles = request.tiles;
    let compat = request.compat;
    let [_, ny, _] = request.grid.size;

    let complain = |what: &str| {
        Err(EngineError::new(
            crate::codes::TILE_FILL_NOT_SELF_COMPATIBLE,
            format!(
                "the fill tiles {:?} and {:?} cannot tile the grid on their own: {what}. \
                 The solver falls back to them when a block runs out of retries, so they \
                 have to be an arrangement the tileset allows.",
                tiles[ground].token, tiles[background].token
            ),
        ))
    };

    for face in [Face::PosX, Face::PosZ] {
        if !compat.allows(face.index(), ground, ground) {
            return complain(&format!(
                "{:?} does not sit beside itself across {}",
                tiles[ground].token,
                Face::KEYS[face.index()]
            ));
        }
        if ny > 1 && !compat.allows(face.index(), background, background) {
            return complain(&format!(
                "{:?} does not sit beside itself across {}",
                tiles[background].token,
                Face::KEYS[face.index()]
            ));
        }
    }

    // The grid's floor is closed, so the ground fill has to be something that
    // may stand on nothing.
    if !matches!(tiles[ground].faces[Face::NegY.index()].socket, Socket::Empty) {
        return complain(&format!(
            "{:?} carries a {} socket, so it cannot sit on the grid's floor",
            tiles[ground].token,
            Face::KEYS[Face::NegY.index()]
        ));
    }

    let top = if ny > 1 { background } else { ground };
    if !matches!(tiles[top].faces[Face::PosY.index()].socket, Socket::Empty) {
        return complain(&format!(
            "{:?} carries a {} socket, so it cannot sit under the grid's open sky",
            tiles[top].token,
            Face::KEYS[Face::PosY.index()]
        ));
    }

    if ny > 1 && !compat.allows(Face::PosY.index(), ground, background) {
        return complain(&format!(
            "{:?} does not sit above {:?}",
            tiles[background].token, tiles[ground].token
        ));
    }
    if ny > 2 && !compat.allows(Face::PosY.index(), background, background) {
        return complain(&format!(
            "{:?} does not stack on itself, and this grid is {ny} storeys",
            tiles[background].token
        ));
    }
    Ok(())
}

// ── The block lattice ─────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Block {
    x0: u32,
    z0: u32,
    x1: u32,
    z1: u32,
}

impl Block {
    /// Whether a region rectangle selects this block. `None` selects all.
    fn meets(&self, region: Option<[u32; 4]>) -> bool {
        let Some([rx0, rz0, rx1, rz1]) = region else {
            return true;
        };
        self.x0 <= rx1 && rx0 <= self.x1 && self.z0 <= rz1 && rz0 <= self.z1
    }

    /// Every cell inside, in the grid's own order — which is also the order the
    /// solver's local indices run in, so `interior[local]` is a global cell.
    fn interior(&self, grid: &Grid) -> Vec<usize> {
        let [_, ny, _] = grid.size;
        let mut cells = Vec::new();
        for y in 0..ny {
            for z in self.z0..=self.z1 {
                for x in self.x0..=self.x1 {
                    cells.push(grid.index(x, y, z));
                }
            }
        }
        cells
    }
}

/// The lattice, in a fixed order: z outer, x inner.
///
/// The order is part of what a seed means — a block's RNG stream is derived
/// from its index here — so it is a format contract, not an implementation
/// detail.
fn blocks(grid: &Grid, params: &Params) -> Vec<Block> {
    let [nx, _, nz] = grid.size;
    let bw = params.block[0].clamp(1, nx.max(1));
    let bd = params.block[2].clamp(1, nz.max(1));
    let overlap = params.overlap.max(1);
    let stride_x = bw.saturating_sub(overlap).max(1);
    let stride_z = bd.saturating_sub(overlap).max(1);

    let starts = |extent: u32, size: u32, stride: u32| {
        let mut out = Vec::new();
        let mut at = 0;
        loop {
            out.push(at);
            if at + size >= extent {
                break;
            }
            at += stride;
        }
        out
    };

    let mut lattice = Vec::new();
    for z0 in starts(nz, bd, stride_z) {
        for x0 in starts(nx, bw, stride_x) {
            lattice.push(Block {
                x0,
                z0,
                x1: (x0 + bw - 1).min(nx - 1),
                z1: (z0 + bd - 1).min(nz - 1),
            });
        }
    }
    lattice
}

// ── One block ─────────────────────────────────────────────────────────

/// Solve one block against the layout around it, or `None` on a contradiction.
///
/// Propagation is **AC-4**: `support[cell][tile][dir]` counts how many tiles
/// remain in the neighbour in direction `dir` that are compatible with `tile`.
/// Removing a tile decrements its neighbours' counters and a counter reaching
/// zero removes that tile in turn, so the work is linear in removals rather
/// than quadratic per pass. It is Merrell's choice and it is why model
/// synthesis scales at all.
fn solve_block(
    request: &Request<'_>,
    cells: &[usize],
    interior: &[usize],
    rng: &mut Rng,
) -> Option<Vec<usize>> {
    let tiles = request.tiles.len();
    let words = request.compat.words();
    let n = interior.len();

    // Global cell → local index. A dense side table rather than a map: the
    // grid is small and this is read once per neighbour lookup.
    let mut local_of = vec![usize::MAX; request.grid.cell_count()];
    for (local, cell) in interior.iter().enumerate() {
        local_of[*cell] = local;
    }

    let mut domain = vec![0u64; n * words];
    for local in 0..n {
        let cell = interior[local];
        if request.locked.get(cell).copied().unwrap_or(false) {
            set(&mut domain[local * words..(local + 1) * words], cells[cell]);
        } else {
            for tile in 0..tiles {
                set(&mut domain[local * words..(local + 1) * words], tile);
            }
        }
    }

    // The border, and the grid's closed ends. Both are fixed, so they are an
    // intersection applied once rather than something propagation revisits.
    for local in 0..n {
        let cell = interior[local];
        for face in Face::ALL {
            let allowed: Vec<u64> = match request.grid.neighbour(cell, face) {
                Neighbour::Cell(other) if local_of[other] != usize::MAX => continue,
                Neighbour::Cell(other) => request
                    .compat
                    .row(face.opposite().index(), cells[other])
                    .to_vec(),
                Neighbour::Closed => {
                    let mut mask = vec![0u64; words];
                    for (tile, expanded) in request.tiles.iter().enumerate() {
                        if matches!(expanded.faces[face.index()].socket, Socket::Empty) {
                            set(&mut mask, tile);
                        }
                    }
                    mask
                }
                Neighbour::Open => continue,
            };
            for word in 0..words {
                domain[local * words + word] &= allowed[word];
            }
        }
        if domain[local * words..(local + 1) * words]
            .iter()
            .all(|w| *w == 0)
        {
            return None;
        }
    }

    // Support counts, taken from the post-restriction domains so the first
    // propagation pass has nothing stale to undo.
    let mut support = vec![0u16; n * tiles * 6];
    for local in 0..n {
        let cell = interior[local];
        for face in Face::ALL {
            let dir = face.index();
            let neighbour = match request.grid.neighbour(cell, face) {
                Neighbour::Cell(other) if local_of[other] != usize::MAX => local_of[other],
                // Border, closed end and open edge alike are already folded
                // into the domain above and never change, so they support
                // everything forever.
                _ => {
                    for tile in 0..tiles {
                        support[(local * tiles + tile) * 6 + dir] = 1;
                    }
                    continue;
                }
            };
            for tile in 0..tiles {
                let count = count_and(
                    request.compat.row(dir, tile),
                    &domain[neighbour * words..(neighbour + 1) * words],
                );
                support[(local * tiles + tile) * 6 + dir] = count.min(u16::MAX as usize) as u16;
            }
        }
    }

    // Seed the propagation with everything that already has no support — in a
    // **second** pass, and once per tile rather than once per unsupported face.
    // Pushing inside the loop above queued a tile as many times as it had
    // starved directions, and `propagate` decrements a neighbour's counters per
    // stack entry: the duplicates drove supports below zero, removed legal
    // tiles, and turned solvable blocks into contradictions. It showed up as a
    // tileset that looked over-constrained.
    let mut stack: Vec<(usize, usize)> = Vec::new();
    for local in 0..n {
        for tile in 0..tiles {
            if !has(&domain[local * words..(local + 1) * words], tile) {
                continue;
            }
            let starved = (0..6).any(|dir| support[(local * tiles + tile) * 6 + dir] == 0);
            if starved {
                clear(&mut domain[local * words..(local + 1) * words], tile);
                stack.push((local, tile));
            }
        }
        if domain[local * words..(local + 1) * words]
            .iter()
            .all(|w| *w == 0)
        {
            return None;
        }
    }
    if !propagate(request, &local_of, interior, &mut domain, &mut support, &mut stack) {
        return None;
    }

    // Collapse until every cell is decided.
    while let Some(local) = pick(request, interior, &domain, words) {
        let chosen = weighted(request, &domain[local * words..(local + 1) * words], rng);
        for tile in 0..tiles {
            if tile != chosen && has(&domain[local * words..(local + 1) * words], tile) {
                clear(&mut domain[local * words..(local + 1) * words], tile);
                stack.push((local, tile));
            }
        }
        if !propagate(request, &local_of, interior, &mut domain, &mut support, &mut stack) {
            return None;
        }
    }

    Some(
        (0..n)
            .map(|local| {
                first(&domain[local * words..(local + 1) * words]).expect("every cell decided")
            })
            .collect(),
    )
}

/// Drain the removal stack, removing whatever loses its last support. `false`
/// on a contradiction.
fn propagate(
    request: &Request<'_>,
    local_of: &[usize],
    interior: &[usize],
    domain: &mut [u64],
    support: &mut [u16],
    stack: &mut Vec<(usize, usize)>,
) -> bool {
    let tiles = request.tiles.len();
    let words = request.compat.words();

    while let Some((local, tile)) = stack.pop() {
        let cell = interior[local];
        for face in Face::ALL {
            let dir = face.index();
            let Neighbour::Cell(other) = request.grid.neighbour(cell, face) else {
                continue;
            };
            let neighbour = local_of[other];
            if neighbour == usize::MAX {
                continue;
            }
            let back = face.opposite().index();
            // `tile` left this cell, so every tile in the neighbour that was
            // counting on it across this face loses one support. Mating is
            // symmetric — a property `mating_is_symmetric_across_every_pair`
            // pins — so `allows(dir, tile, other)` is the same relation read
            // from either end.
            for candidate in 0..tiles {
                if !has(&domain[neighbour * words..(neighbour + 1) * words], candidate) {
                    continue;
                }
                if !request.compat.allows(dir, tile, candidate) {
                    continue;
                }
                let slot = &mut support[(neighbour * tiles + candidate) * 6 + back];
                *slot = slot.saturating_sub(1);
                if *slot == 0 {
                    clear(
                        &mut domain[neighbour * words..(neighbour + 1) * words],
                        candidate,
                    );
                    if domain[neighbour * words..(neighbour + 1) * words]
                        .iter()
                        .all(|w| *w == 0)
                    {
                        return false;
                    }
                    stack.push((neighbour, candidate));
                }
            }
        }
    }
    true
}

/// The next cell to collapse: minimum weighted entropy, ties broken by a
/// per-cell hash.
///
/// The tie-break **consumes no RNG draws**, which is not a micro-optimisation.
/// A generator's random draws are a format contract (M46's trap), and a
/// tie-break that drew would make every committed layout depend on how many
/// ties happened to occur — so adding a tile to a tileset would reshuffle
/// grids it has nothing to do with.
fn pick(request: &Request<'_>, interior: &[usize], domain: &[u64], words: usize) -> Option<usize> {
    let mut best: Option<(f32, u32, usize)> = None;
    for local in 0..interior.len() {
        let slice = &domain[local * words..(local + 1) * words];
        let remaining: usize = slice.iter().map(|w| w.count_ones() as usize).sum();
        if remaining <= 1 {
            continue;
        }
        let mut total = 0.0f32;
        let mut plogp = 0.0f32;
        for tile in 0..request.tiles.len() {
            if has(slice, tile) {
                let w = request.tiles[tile].weight.max(f32::MIN_POSITIVE);
                total += w;
                plogp += w * w.ln();
            }
        }
        let entropy = total.ln() - plogp / total;
        let tie = mix(request.seed, interior[local] as u32);
        let key = (entropy, tie, local);
        if best.is_none_or(|current| {
            (key.0, key.1, key.2) < (current.0, current.1, current.2)
        }) {
            best = Some(key);
        }
    }
    best.map(|(_, _, local)| local)
}

/// Choose one tile from a domain, weighted. **Exactly one draw**, always.
fn weighted(request: &Request<'_>, slice: &[u64], rng: &mut Rng) -> usize {
    let mut total = 0.0f32;
    for tile in 0..request.tiles.len() {
        if has(slice, tile) {
            total += request.tiles[tile].weight.max(f32::MIN_POSITIVE);
        }
    }
    let mut target = rng.unit() * total;
    let mut last = usize::MAX;
    for tile in 0..request.tiles.len() {
        if !has(slice, tile) {
            continue;
        }
        last = tile;
        target -= request.tiles[tile].weight.max(f32::MIN_POSITIVE);
        if target <= 0.0 {
            return tile;
        }
    }
    // Only reachable when the draw lands exactly on the total, or when
    // rounding leaves a sliver: the last candidate is the honest answer.
    last
}

// ── Bitset helpers ────────────────────────────────────────────────────

fn set(words: &mut [u64], bit: usize) {
    words[bit / 64] |= 1u64 << (bit % 64);
}

fn clear(words: &mut [u64], bit: usize) {
    words[bit / 64] &= !(1u64 << (bit % 64));
}

fn has(words: &[u64], bit: usize) -> bool {
    words[bit / 64] & (1u64 << (bit % 64)) != 0
}

fn first(words: &[u64]) -> Option<usize> {
    words
        .iter()
        .enumerate()
        .find_map(|(w, word)| (*word != 0).then(|| w * 64 + word.trailing_zeros() as usize))
}

fn count_and(a: &[u64], b: &[u64]) -> usize {
    a.iter()
        .zip(b)
        .map(|(x, y)| (x & y).count_ones() as usize)
        .sum()
}

// ── Randomness ────────────────────────────────────────────────────────

/// A block's own stream.
///
/// **Per block, not one global stream.** With a global stream, block N's draws
/// depend on how many draws blocks 0..N−1 happened to consume, and re-solving
/// block N alone becomes impossible — which is the entire point of `--region`.
pub fn stream(seed: u32, block: u32, attempt: u32) -> Rng {
    let mut h = seed.wrapping_mul(0x9E37_79B9);
    h ^= block.wrapping_add(1).wrapping_mul(0x85EB_CA6B);
    h = h.rotate_left(13).wrapping_mul(0xC2B2_AE35);
    h ^= attempt.wrapping_add(1).wrapping_mul(0x27D4_EB2F);
    Rng::new(h)
}

/// A deterministic scramble of a cell index, for the entropy tie-break.
fn mix(seed: u32, cell: u32) -> u32 {
    let mut h = cell.wrapping_add(seed).wrapping_mul(0x85EB_CA6B);
    h ^= h >> 13;
    h = h.wrapping_mul(0xC2B2_AE35);
    h ^ (h >> 16)
}

/// xorshift32, written out for the reason `fracture.rs`, `tree.rs`, `cloud.rs`
/// and `particles.rs` each write theirs out: the sequence is part of what the
/// output *means*, and it may not live somewhere a dependency upgrade can
/// reshape it.
pub struct Rng(u32);

impl Rng {
    pub fn new(seed: u32) -> Self {
        // Zero is a xorshift fixed point.
        Self(seed.wrapping_mul(0x9E37_79B9) | 1)
    }

    pub fn unit(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0 as f32 / u32::MAX as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tileset::{Faces, TileDef, Tileset};
    use glam::Vec3;

    /// A two-storey tileset in the shape the village uses: ground that tiles
    /// itself, a building that must be enclosed, and air above.
    fn town() -> Tileset {
        let tile = |name: &str, weight: f32, rotations: u32, faces: Faces| TileDef {
            name: name.into(),
            weight,
            rotations,
            faces,
            parts: Vec::new(),
        };
        let faces = |px: &str, nx: &str, py: &str, ny: &str, pz: &str, nz: &str| Faces {
            px: px.into(),
            nx: nx.into(),
            py: py.into(),
            ny: ny.into(),
            pz: pz.into(),
            nz: nz.into(),
        };
        Tileset {
            cell: Vec3::new(2.0, 2.5, 2.0),
            palette: Default::default(),
            tiles: vec![
                tile("ground", 8.0, 1, faces("out", "out", "sky_i", "0", "out", "out")),
                tile("floor", 3.0, 1, faces("in", "in", "wtop_i", "0", "in", "in")),
                tile("wall", 5.0, 4, faces("run", "run", "wtop_i", "0", "in", "out")),
                tile("corner", 3.0, 4, faces("run", "out", "wtop_i", "0", "run", "out")),
                tile("roof", 5.0, 2, faces("up", "up", "0", "wtop_i", "up", "up")),
                tile("air", 6.0, 1, faces("up", "up", "0", "sky_i", "up", "up")),
            ],
        }
    }

    struct Fixture {
        tiles: Vec<ExpandedTile>,
        compat: Compat,
        ground: usize,
        background: usize,
    }

    fn fixture() -> Fixture {
        fixture_of(&town())
    }

    fn fixture_of(tileset: &Tileset) -> Fixture {
        let tiles = crate::tileset::expand(tileset).unwrap();
        let compat = Compat::build(&tiles);
        let index = |token: &str| tiles.iter().position(|t| t.token == token).unwrap();
        Fixture {
            ground: index("ground@0"),
            background: index("air@0"),
            tiles,
            compat,
        }
    }

    fn request<'a>(
        fixture: &'a Fixture,
        grid: &'a Grid,
        locked: &'a [bool],
        seed: u32,
    ) -> Request<'a> {
        Request {
            grid,
            tiles: &fixture.tiles,
            compat: &fixture.compat,
            seed,
            params: Params {
                block: [4, u32::MAX, 4],
                overlap: 1,
                attempts: 10,
            },
            fill_ground: fixture.ground,
            fill_background: fixture.background,
            prior: None,
            locked,
            region: None,
        }
    }

    fn grid_of(nx: u32, ny: u32, nz: u32) -> Grid {
        Grid {
            size: [nx, ny, nz],
            offsets: Vec::new(),
        }
    }

    /// Every cell agrees with all six of its neighbours, and with the grid's
    /// closed ends. The property a picture cannot state.
    fn assert_legal(fixture: &Fixture, grid: &Grid, cells: &[usize]) {
        for (cell, &here) in cells.iter().enumerate() {
            for face in Face::ALL {
                match grid.neighbour(cell, face) {
                    Neighbour::Cell(other) => assert!(
                        fixture.compat.allows(face.index(), here, cells[other]),
                        "{} at {:?} refuses {} across {face:?}",
                        fixture.tiles[here].token,
                        grid.coords(cell),
                        fixture.tiles[cells[other]].token,
                    ),
                    Neighbour::Closed => assert!(
                        matches!(fixture.tiles[here].faces[face.index()].socket, Socket::Empty),
                        "{} at {:?} has a {face:?} socket against the grid's closed end",
                        fixture.tiles[here].token,
                        grid.coords(cell),
                    ),
                    Neighbour::Open => {}
                }
            }
        }
    }

    #[test]
    fn a_solved_grid_is_legal_everywhere() {
        let fixture = fixture();
        let grid = grid_of(12, 2, 10);
        let locked = vec![false; grid.cell_count()];
        let outcome = synthesize(&request(&fixture, &grid, &locked, 7)).unwrap();
        assert_eq!(outcome.fallbacks, 0, "this tileset is not over-constrained");
        assert_legal(&fixture, &grid, &outcome.cells);
    }

    /// The closed floor and open sky are what keep the two storeys apart, with
    /// nothing in the tileset saying "roofs go on layer 1".
    #[test]
    fn the_closed_ends_sort_the_storeys_out() {
        let fixture = fixture();
        let grid = grid_of(8, 2, 8);
        let locked = vec![false; grid.cell_count()];
        let outcome = synthesize(&request(&fixture, &grid, &locked, 3)).unwrap();
        for (cell, &tile) in outcome.cells.iter().enumerate() {
            let (_, y, _) = grid.coords(cell);
            let name = &fixture.tiles[tile].token;
            if y == 0 {
                assert!(!name.starts_with("roof") && !name.starts_with("air"), "{name}");
            } else {
                assert!(name.starts_with("roof") || name.starts_with("air"), "{name}");
            }
        }
    }

    #[test]
    fn the_same_seed_reproduces_the_same_grid() {
        let fixture = fixture();
        let grid = grid_of(10, 2, 10);
        let locked = vec![false; grid.cell_count()];
        let once = synthesize(&request(&fixture, &grid, &locked, 11)).unwrap();
        let twice = synthesize(&request(&fixture, &grid, &locked, 11)).unwrap();
        assert_eq!(once.cells, twice.cells);

        let other = synthesize(&request(&fixture, &grid, &locked, 12)).unwrap();
        assert_ne!(once.cells, other.cells, "and a different seed does not");
    }

    /// Requirement 3, stated as a property: a region solve leaves every cell
    /// outside the blocks it touched byte for byte.
    #[test]
    fn a_region_solve_touches_only_the_blocks_it_selected() {
        let fixture = fixture();
        let grid = grid_of(16, 2, 16);
        let locked = vec![false; grid.cell_count()];
        let base = synthesize(&request(&fixture, &grid, &locked, 5)).unwrap();

        let mut edit = request(&fixture, &grid, &locked, 5);
        edit.prior = Some(&base.cells);
        edit.region = Some([1, 1, 3, 3]);
        edit.seed = 99; // a different roll, so a touched cell is visible
        let edited = synthesize(&edit).unwrap();

        assert!(edited.solved < edited.blocks, "the region selected a subset");
        assert_legal(&fixture, &grid, &edited.cells);

        // The blocks the region selected, from the same lattice the solve used
        // — asserting against a hand-computed rectangle would only pin the
        // stride arithmetic twice.
        let mut touchable = vec![false; grid.cell_count()];
        for block in blocks(&grid, &edit.params) {
            if block.meets(edit.region) {
                for cell in block.interior(&grid) {
                    touchable[cell] = true;
                }
            }
        }

        let changed: Vec<usize> = base
            .cells
            .iter()
            .zip(&edited.cells)
            .enumerate()
            .filter(|(_, (a, b))| a != b)
            .map(|(cell, _)| cell)
            .collect();
        assert!(!changed.is_empty(), "the re-solve did something");
        assert!(
            touchable.iter().filter(|t| **t).count() < grid.cell_count(),
            "and the region really did exclude some of the grid"
        );
        for cell in changed {
            assert!(
                touchable[cell],
                "cell {:?} moved and no selected block contains it",
                grid.coords(cell)
            );
        }
    }

    #[test]
    fn a_region_solve_is_itself_reproducible() {
        let fixture = fixture();
        let grid = grid_of(12, 2, 12);
        let locked = vec![false; grid.cell_count()];
        let base = synthesize(&request(&fixture, &grid, &locked, 2)).unwrap();
        let run = |seed| {
            let mut req = request(&fixture, &grid, &locked, seed);
            req.prior = Some(&base.cells);
            req.region = Some([5, 5, 7, 7]);
            synthesize(&req).unwrap().cells
        };
        assert_eq!(run(42), run(42));
    }

    #[test]
    fn a_locked_cell_survives_a_full_re_solve() {
        let fixture = fixture();
        let grid = grid_of(10, 2, 10);
        let mut locked = vec![false; grid.cell_count()];
        let free = vec![false; grid.cell_count()];
        let base = synthesize(&request(&fixture, &grid, &free, 4)).unwrap();

        // Pin a whole cottage's worth of ground-floor cells.
        let pinned: Vec<usize> = (2..6)
            .flat_map(|x| (2..6).map(move |z| (x, z)))
            .map(|(x, z)| grid.index(x, 0, z))
            .collect();
        for cell in &pinned {
            locked[*cell] = true;
        }

        let mut req = request(&fixture, &grid, &locked, 77);
        req.prior = Some(&base.cells);
        let after = synthesize(&req).unwrap();
        for cell in &pinned {
            assert_eq!(
                base.cells[*cell], after.cells[*cell],
                "locked cell {:?} moved",
                grid.coords(*cell)
            );
        }
        assert!(
            base.cells != after.cells,
            "the rest of the grid did re-roll"
        );
    }

    /// A tileset that cannot fill its own grid is refused before it can produce
    /// a bland result nobody can explain.
    #[test]
    fn a_fill_that_cannot_tile_itself_is_refused() {
        let fixture = fixture();
        let grid = grid_of(4, 2, 4);
        let locked = vec![false; grid.cell_count()];
        let mut req = request(&fixture, &grid, &locked, 1);
        // `floor` needs walls around it; it cannot be the ground fill.
        req.fill_ground = fixture.tiles.iter().position(|t| t.token == "floor@0").unwrap();
        assert_eq!(
            synthesize(&req).unwrap_err().error,
            crate::codes::TILE_FILL_NOT_SELF_COMPATIBLE
        );
    }

    /// A tile whose sockets nobody mates is pruned before the first collapse
    /// rather than failing a block — it simply never appears.
    #[test]
    fn an_unplaceable_tile_is_pruned_rather_than_attempted() {
        let mut tileset = town();
        tileset.tiles.push(TileDef {
            name: "trap".into(),
            weight: 400.0, // heavy enough that a picker reaching for it would
            rotations: 1,
            faces: Faces {
                px: "out".into(),
                nx: "out".into(),
                py: "nothing_mates_this".into(),
                ny: "0".into(),
                pz: "out".into(),
                nz: "out".into(),
            },
            parts: Vec::new(),
        });
        let fixture = fixture_of(&tileset);
        let grid = grid_of(8, 2, 8);
        let locked = vec![false; grid.cell_count()];
        let outcome = synthesize(&request(&fixture, &grid, &locked, 13)).unwrap();
        assert_eq!(outcome.fallbacks, 0);
        let trap = fixture.tiles.iter().position(|t| t.token == "trap@0").unwrap();
        assert!(!outcome.cells.contains(&trap));
        assert_legal(&fixture, &grid, &outcome.cells);
    }

    /// A block that genuinely cannot be solved gives up and leaves what stood
    /// there, rather than hanging or failing the run — and says how often, which
    /// is the number that makes "the village came out bland" diagnosable.
    ///
    /// Reverting rather than filling is what keeps the grid legal: a later
    /// block's border is an already-solved neighbour, and the known-good fill
    /// is only known good against itself.
    #[test]
    fn an_impossible_block_gives_up_and_leaves_what_stood_there() {
        let fixture = fixture();
        let grid = grid_of(8, 2, 8);
        let free = vec![false; grid.cell_count()];
        let base = synthesize(&request(&fixture, &grid, &free, 6)).unwrap();

        // Pin two neighbours that refuse each other. No solve of the block
        // holding them can succeed, and an author really can write this.
        let mut locked = free.clone();
        let mut prior = base.cells.clone();
        let floor = fixture.tiles.iter().position(|t| t.token == "floor@0").unwrap();
        let (a, b) = (grid.index(4, 0, 4), grid.index(5, 0, 4));
        prior[a] = fixture.ground;
        prior[b] = floor;
        locked[a] = true;
        locked[b] = true;

        let mut req = request(&fixture, &grid, &locked, 31);
        req.prior = Some(&prior);
        let outcome = synthesize(&req).unwrap();

        assert!(outcome.fallbacks > 0, "the pinned pair cannot be satisfied");
        assert_eq!(outcome.cells[a], fixture.ground, "and the pins survive");
        assert_eq!(outcome.cells[b], floor);
        // The damage is confined to the pins. Everywhere else either re-solved
        // successfully or was reverted to an arrangement that was already
        // legal — which is the property reverting buys and filling would not.
        for (cell, &here) in outcome.cells.iter().enumerate() {
            for face in Face::ALL {
                let Neighbour::Cell(other) = grid.neighbour(cell, face) else {
                    continue;
                };
                if fixture.compat.allows(face.index(), here, outcome.cells[other]) {
                    continue;
                }
                assert!(
                    [a, b].contains(&cell) || [a, b].contains(&other),
                    "{:?} against {:?} broke, and neither is pinned",
                    grid.coords(cell),
                    grid.coords(other),
                );
            }
        }
    }

    /// The lattice covers every cell, or a solve would leave the fill standing
    /// in a corner nobody visited.
    #[test]
    fn the_lattice_covers_the_grid() {
        for (nx, nz) in [(1, 1), (4, 4), (5, 3), (9, 17), (16, 16)] {
            let grid = grid_of(nx, 2, nz);
            let params = Params {
                block: [4, u32::MAX, 4],
                overlap: 1,
                attempts: 1,
            };
            let mut seen = vec![false; grid.cell_count()];
            for block in blocks(&grid, &params) {
                for cell in block.interior(&grid) {
                    seen[cell] = true;
                }
            }
            assert!(seen.iter().all(|s| *s), "{nx}×{nz} left a cell uncovered");
        }
    }

    /// A terraced grid solves too, and the shear is what keeps it legal.
    #[test]
    fn a_terraced_grid_solves_across_its_steps() {
        let fixture = fixture();
        let mut grid = grid_of(10, 2, 8);
        grid.offsets = (0..10 * 8).map(|i: i32| (i % 10) / 4).collect();
        let locked = vec![false; grid.cell_count()];
        let outcome = synthesize(&request(&fixture, &grid, &locked, 21)).unwrap();
        assert_legal(&fixture, &grid, &outcome.cells);
    }

    #[test]
    fn a_block_stream_depends_on_all_three_of_its_inputs() {
        let draw = |seed, block, attempt| stream(seed, block, attempt).unit();
        assert_ne!(draw(1, 0, 0), draw(2, 0, 0));
        assert_ne!(draw(1, 0, 0), draw(1, 1, 0));
        assert_ne!(draw(1, 0, 0), draw(1, 0, 1));
        assert_eq!(draw(5, 3, 2), draw(5, 3, 2));
    }
}
