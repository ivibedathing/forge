//! Global illumination: the baked irradiance field and its file format (M35).
//!
//! The design is `designs/global-illumination-design.md`. This module holds the
//! parts that need no GPU and no ray tracer: the probe grid a
//! [`LightProbeVolume`](crate::components::LightProbeVolume) derives from its
//! `Transform`, the budget that refuses an over-large volume *before* anything
//! is allocated, and the on-disk format the bake writes and the renderer reads.
//!
//! # What is stored is transfer, not irradiance
//!
//! A bake that stored the light which arrived when it ran would carry noon's
//! fill light through midnight, because `daylight` is a flagship system and it
//! moves the sky every frame. So a probe stores **transfer**: how much light
//! reaches it per unit of light emitted by each basis source, which is a
//! property of geometry and albedo alone. Evaluation is then a scaled sum
//! against whatever the sky is doing right now:
//!
//! ```text
//! irradiance(probe) = Σ_basis  transfer[probe][basis] · live_radiance[basis]
//! ```
//!
//! The basis is M16's own fill model — the two bands `sky_ambient` mixes, which
//! `apply_daylight` rewrites every frame — so GI tracks day and night exactly,
//! with no extra machinery. Two rather than the three the design assumed; see
//! [`SKY_BANDS`] for the measurement and why the difference matters. The sun is
//! deliberately *not* a basis source in M35 (design §5.3): it is the one source
//! whose direction moves, which is what would cost N transfer vectors instead
//! of one.
//!
//! # The file is text, one probe per line
//!
//! Invariant 1 — no binary formats. A header object, then one object per probe,
//! so a `git diff` shows which probes moved rather than that the blob changed.
//! Coefficients are quantized to [`QUANT_DECIMALS`] decimals, which is what
//! makes the file both diffable and byte-reproducible.

pub mod bake;
pub mod evaluate;

pub use evaluate::{evaluate, IrradianceField};

use serde::{Deserialize, Serialize};

use crate::components::LightProbeVolume;
use crate::math::Vec3;

/// The format tag in a bake file's header. A reader that finds anything else
/// refuses rather than guessing — `gi_bake_malformed`.
pub const FORMAT: &str = "forge-gi/1";

/// Coefficients in an SH-L1 vector: one constant band plus three linear.
pub const SH_L1_COEFFS: usize = 4;

/// Colour channels. GI is coloured — a red wall reddening its neighbour is the
/// whole point — so transfer is per channel, not scalar.
pub const CHANNELS: usize = 3;

/// Numbers in one basis source's transfer vector: 4 coefficients × 3 channels.
pub const NUMBERS_PER_BASIS: usize = SH_L1_COEFFS * CHANNELS;

/// Sky basis sources, in the order they are stored: **zenith, then ground**.
///
/// Two, not three — and this corrects the design doc, which assumed three on
/// the strength of `sky_gradient`. The distinction is the whole reason §3.1's
/// guarantee survives:
///
/// * `sky_gradient` draws the sky **dome** and picks the fog colour, and it
///   does interpolate three bands, easing horizon → zenith above the horizon
///   and horizon → ground below it.
/// * `sky_ambient` is the **fill term GI actually replaces**, and it mixes only
///   `sky_ground` and `sky_zenith`, linearly in `n.y * 0.5 + 0.5`, normalized by
///   their mean. `sky_horizon` never appears in it.
///
/// Baking a third band would let GI produce fill light the pre-M35 engine could
/// not, so an open-sky probe would match `sky_ambient(n)` in total energy but
/// not in shape — and §3.1 pinned exactly that equality, because it is what
/// makes `AmbientLight.color`/`.intensity` keep predicting what they predict and
/// what makes every visible difference attributable to geometry.
///
/// The cost, stated: a sunset's horizon colour does not tint GI. It does not
/// tint the ambient fill today either, so nothing regresses — and widening
/// `sky_ambient` to three bands would edit one of the four ULP-sensitive
/// lighting lines, which is a different milestone.
pub const SKY_BANDS: usize = 2;

/// Index of the zenith band in a probe's `sky` array.
pub const BAND_ZENITH: usize = 0;
/// Index of the ground band in a probe's `sky` array.
pub const BAND_GROUND: usize = 1;

/// Decimals each coefficient is rounded to before it is written.
///
/// A format contract: the bake promises byte-reproducibility across *machines*,
/// not merely across runs on one adapter, and rounding to a fixed number of
/// decimals is what removes the last-place noise that would otherwise differ
/// between a debug and a release `f32` sum.
pub const QUANT_DECIMALS: u32 = 4;

/// The most probes one volume may hold.
///
/// Refused before allocating, `tree_too_complex`'s precedent: a hung bake that
/// produces no output is the worst failure an agent loop can hit, and the
/// arithmetic that predicts the count is cheap and exact.
///
/// 262,144 probes is a 64³ grid — roughly 9 MB of text at the decided scope,
/// already past what belongs in a repo, so the limit bites well before memory
/// does.
pub const MAX_GI_PROBES: u64 = 262_144;

/// Probes along each axis for a volume of `extent` metres at `spacing` metres.
///
/// `spacing` is metres between probes rather than a resolution, so resizing a
/// volume keeps its GI detail instead of stretching it, and two volumes at the
/// same spacing agree where they meet. A degenerate or non-finite extent still
/// yields a usable grid: every axis has at least two probes, because one probe
/// cannot be interpolated and the shader's trilinear fetch assumes a cell.
pub fn grid_counts(extent: Vec3, spacing: f32) -> [u32; 3] {
    // The negation is load-bearing, not a style slip: `!(e > 0.0)` is *not*
    // `e <= 0.0` when `e` is NaN, and only the negated form sends a NaN extent
    // to the safe branch instead of through it into `as u32`. Rewriting either
    // comparison "the readable way" makes `a_nan_extent_falls_to_the_minimum_grid`
    // fail — which is why that test exists. The same reason the validation
    // passes carry this allow; see CLAUDE.md's Traps.
    #[allow(clippy::neg_cmp_op_on_partial_ord)]
    let axis = |e: f32| -> u32 {
        if !(e > 0.0) || !(spacing > 0.0) {
            return 2;
        }
        // +1 because n cells need n+1 probes on their corners.
        ((e / spacing).floor() as u32).saturating_add(1).max(2)
    };
    [axis(extent.x), axis(extent.y), axis(extent.z)]
}

/// How many probes a grid holds, in `u64` so the budget check cannot overflow
/// the value it is checking.
pub fn probe_count(grid: [u32; 3]) -> u64 {
    grid[0] as u64 * grid[1] as u64 * grid[2] as u64
}

/// Probes a volume would place, derived from its `Transform` scale — the whole
/// budget question answered without touching a ray or a buffer.
pub fn probe_count_for(volume: &LightProbeVolume, scale: Vec3) -> u64 {
    probe_count(grid_counts(scale, volume.spacing))
}

/// One probe's transfer, as it appears on its own line of the bake file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Probe {
    /// Grid coordinate, `[x, y, z]`. Written so a diff names the probe that
    /// moved rather than a line number, and so a truncated file is detectable.
    pub p: [u32; 3],

    /// Transfer per sky band, in [`SKY_BANDS`] order, each an SH-L1 vector of
    /// [`NUMBERS_PER_BASIS`] numbers laid out coefficient-major.
    pub sky: Vec<Vec<f32>>,
}

impl Probe {
    /// Whether this probe's arrays are the shape the format promises. A file
    /// that parses as JSON but carries a 9-number vector is `gi_bake_malformed`,
    /// not a panic three stages later in an upload.
    pub fn is_well_formed(&self) -> bool {
        self.sky.len() == SKY_BANDS && self.sky.iter().all(|v| v.len() == NUMBERS_PER_BASIS)
    }
}

/// The header object on a bake file's first line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BakeHeader {
    /// Always [`FORMAT`] for a file this version can read.
    pub format: String,
    /// The scene the bake was taken from, for a human reading the file.
    pub scene: String,
    /// The volume entity the bake belongs to — a scene may hold several.
    pub entity: String,
    /// Digest of every input the bake read; see [`InputsHasher`].
    pub inputs_hash: String,
    /// Probes along each axis.
    pub grid: [u32; 3],
    /// World position of probe `[0, 0, 0]` — the volume's minimum corner.
    pub origin: [f32; 3],
    /// Metres between probes, copied from the component so a reader can detect
    /// a component edited after its bake without recomputing the hash.
    pub spacing: f32,
    /// Basis sources and their counts, e.g. `{"sky": 3}`.
    ///
    /// A named map rather than a positional list precisely so the deferred sun
    /// basis (design §5.3) can arrive beside `sky` without a version bump: a
    /// reader that finds an entry it knows adds terms to the same sum.
    pub basis: std::collections::BTreeMap<String, u32>,
    /// Rays per probe. Recorded because a file baked at 128 samples and one at
    /// 512 are different artifacts, and a render must be able to say which one
    /// it is looking at.
    pub samples: u32,
    /// Light bounces the bake gathered.
    pub bounces: u32,
    /// Probes that were found inside geometry and relocated or filled from a
    /// neighbour. The number that says whether a volume is badly placed.
    #[serde(default)]
    pub relocated: u32,
}

/// A parsed bake file.
#[derive(Debug, Clone, PartialEq)]
pub struct BakedGi {
    pub header: BakeHeader,
    pub probes: Vec<Probe>,
}

/// Why a bake file could not be used. Each maps to one of M35's error codes at
/// the validation boundary; keeping them apart here means the message can say
/// which of the three went wrong.
#[derive(Debug, Clone, PartialEq)]
pub enum BakeError {
    /// The file is not valid NDJSON, or a line is not the object it must be.
    Parse { line: usize, message: String },
    /// It parsed, but its version, grid, basis or probe count disagrees with
    /// itself or with the component that names it.
    Malformed(String),
}

impl std::fmt::Display for BakeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse { line, message } => write!(f, "line {line}: {message}"),
            Self::Malformed(message) => f.write_str(message),
        }
    }
}

impl BakedGi {
    /// Parse a bake file: a header line, then one line per probe.
    ///
    /// Shape is checked here rather than at the upload, because the failure mode
    /// this whole format guards against is the one where everything runs and the
    /// picture is quietly wrong.
    pub fn parse(text: &str) -> Result<Self, BakeError> {
        let mut lines = text
            .lines()
            .enumerate()
            .filter(|(_, l)| !l.trim().is_empty());

        let (header_line, header_text) = lines.next().ok_or_else(|| {
            BakeError::Malformed("the file is empty; it needs a header line".into())
        })?;
        let header: BakeHeader =
            serde_json::from_str(header_text).map_err(|e| BakeError::Parse {
                line: header_line + 1,
                message: e.to_string(),
            })?;

        if header.format != FORMAT {
            return Err(BakeError::Malformed(format!(
                "the file says format {:?}, but this engine reads {FORMAT:?}",
                header.format
            )));
        }

        let mut probes = Vec::new();
        for (index, line) in lines {
            let probe: Probe = serde_json::from_str(line).map_err(|e| BakeError::Parse {
                line: index + 1,
                message: e.to_string(),
            })?;
            if !probe.is_well_formed() {
                return Err(BakeError::Malformed(format!(
                    "probe {:?} on line {} carries {} basis vectors of {:?} numbers; \
                     the format is {SKY_BANDS} of {NUMBERS_PER_BASIS}",
                    probe.p,
                    index + 1,
                    probe.sky.len(),
                    probe.sky.iter().map(Vec::len).collect::<Vec<_>>(),
                )));
            }
            probes.push(probe);
        }

        let expected = probe_count(header.grid);
        if probes.len() as u64 != expected {
            return Err(BakeError::Malformed(format!(
                "the header says a {:?} grid ({expected} probes) but the file holds {}",
                header.grid,
                probes.len()
            )));
        }

        // Line order *is* the layout. The bake writes x fastest, then y, then z,
        // and both the evaluation and the 3D-texture upload index by line rather
        // than by searching for a coordinate — which is what makes the upload a
        // memcpy per plane. A file whose probes are permuted parses as valid
        // JSON, carries the right count, and renders light from the wrong place;
        // checking it here is the only cheap moment.
        for (index, probe) in probes.iter().enumerate() {
            let i = index as u64;
            let x = i % header.grid[0] as u64;
            let y = (i / header.grid[0] as u64) % header.grid[1] as u64;
            let z = i / (header.grid[0] as u64 * header.grid[1] as u64);
            let want = [x as u32, y as u32, z as u32];
            if probe.p != want {
                return Err(BakeError::Malformed(format!(
                    "probe {} is {:?} but this grid puts {want:?} there; the file's \
                     line order is its layout (x fastest, then y, then z)",
                    index + 1,
                    probe.p,
                )));
            }
        }

        Ok(Self { header, probes })
    }

    /// Serialize back to the on-disk form: header line, then one probe per line.
    ///
    /// Coefficients go through [`quantize`] on the way out, which is what makes
    /// a re-bake byte-identical to the committed file.
    pub fn to_text(&self) -> String {
        let mut out = serde_json::to_string(&self.header).expect("header serializes");
        out.push('\n');
        for probe in &self.probes {
            let quantized = Probe {
                p: probe.p,
                sky: probe
                    .sky
                    .iter()
                    .map(|v| v.iter().copied().map(quantize).collect())
                    .collect(),
            };
            out.push_str(&serde_json::to_string(&quantized).expect("probe serializes"));
            out.push('\n');
        }
        out
    }

    /// Whether this bake still describes `volume`. Catches a component edited
    /// after its bake without recomputing the whole input digest — cheap, and it
    /// is the mismatch an author hits most.
    pub fn matches(&self, volume: &LightProbeVolume, scale: Vec3) -> bool {
        self.header.grid == grid_counts(scale, volume.spacing)
            && self.header.spacing.to_bits() == volume.spacing.to_bits()
            && self.header.bounces == volume.bounces
    }
}

/// Round to [`QUANT_DECIMALS`] decimals.
///
/// `-0.0` is folded to `0.0`: the two are equal as floats but serialize to
/// different text, which would be a spurious diff and would break the
/// byte-for-byte re-bake promise on a probe that happened to gather nothing.
pub fn quantize(value: f32) -> f32 {
    let scale = 10f32.powi(QUANT_DECIMALS as i32);
    let rounded = (value * scale).round() / scale;
    if rounded == 0.0 {
        0.0
    } else {
        rounded
    }
}

/// FNV-1a, 64-bit, spelled out in-repo.
///
/// Written here rather than taken from a dependency for the reason the particle
/// xorshift and the meadow reseed hash are: a baked file sits under a render
/// baseline, so the digest is a **format contract**, and a dependency upgrade
/// that changed it would invalidate every committed bake at once.
///
/// It hashes the *inputs* to a bake — geometry, transforms, albedos, and the
/// bake's own parameters — so that a scene edited after its bake fails
/// `validate` (`gi_bake_stale`) instead of rendering with light that no longer
/// matches the geometry.
#[derive(Debug, Clone)]
pub struct InputsHasher {
    state: u64,
}

impl Default for InputsHasher {
    fn default() -> Self {
        Self::new()
    }
}

impl InputsHasher {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    pub fn new() -> Self {
        Self {
            state: Self::OFFSET_BASIS,
        }
    }

    pub fn byte(&mut self, b: u8) -> &mut Self {
        self.state ^= b as u64;
        self.state = self.state.wrapping_mul(Self::PRIME);
        self
    }

    pub fn bytes(&mut self, bytes: &[u8]) -> &mut Self {
        for &b in bytes {
            self.byte(b);
        }
        self
    }

    pub fn str(&mut self, s: &str) -> &mut Self {
        // The length goes in too, so "ab" + "c" cannot collide with "a" + "bc".
        self.u32(s.len() as u32).bytes(s.as_bytes())
    }

    pub fn u32(&mut self, v: u32) -> &mut Self {
        self.bytes(&v.to_le_bytes())
    }

    /// Feed a float by its *quantized* value, so the digest is stable across
    /// build profiles for the same reason the file is: the last place of an
    /// `f32` is not a promise this engine makes across machines.
    pub fn f32(&mut self, v: f32) -> &mut Self {
        self.bytes(&quantize(v).to_bits().to_le_bytes())
    }

    pub fn vec3(&mut self, v: Vec3) -> &mut Self {
        self.f32(v.x).f32(v.y).f32(v.z)
    }

    /// The digest, as the lower-case hex that goes in the header.
    pub fn finish(&self) -> String {
        format!("{:016x}", self.state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_grid_has_at_least_two_probes_per_axis() {
        // One probe cannot be interpolated, and the shader's trilinear fetch
        // assumes a cell exists. A flat volume is a legitimate thing to author.
        assert_eq!(grid_counts(Vec3::new(10.0, 0.0, 10.0), 4.0), [3, 2, 3]);
    }

    #[test]
    fn spacing_is_metres_not_resolution() {
        // Doubling the extent at one spacing must double the probes, not
        // stretch the same count over more ground — the property that lets two
        // volumes agree where they meet.
        let small = grid_counts(Vec3::splat(16.0), 4.0);
        let large = grid_counts(Vec3::splat(32.0), 4.0);
        assert_eq!(small, [5, 5, 5]);
        assert_eq!(large, [9, 9, 9]);
    }

    #[test]
    fn a_nan_extent_falls_to_the_minimum_grid() {
        // Negated comparisons, so NaN takes the safe branch rather than
        // reaching `as u32` — which is UB-adjacent saturating nonsense.
        assert_eq!(grid_counts(Vec3::splat(f32::NAN), 4.0), [2, 2, 2]);
        assert_eq!(grid_counts(Vec3::splat(10.0), f32::NAN), [2, 2, 2]);
        assert_eq!(grid_counts(Vec3::splat(10.0), 0.0), [2, 2, 2]);
    }

    #[test]
    fn probe_count_does_not_overflow_the_check() {
        // u32 multiplication would wrap here and report a tiny count for an
        // enormous volume, which is exactly the hang the budget exists to stop.
        assert_eq!(probe_count([4096, 4096, 4096]), 68_719_476_736);
    }

    #[test]
    fn quantize_folds_negative_zero() {
        // -0.0 == 0.0 as a float but serializes as "-0.0", which would be a
        // spurious diff and would break the byte-for-byte re-bake promise.
        assert_eq!(quantize(-0.000_001).to_bits(), 0.0f32.to_bits());
    }

    #[test]
    fn the_hash_is_order_and_boundary_sensitive() {
        let mut a = InputsHasher::new();
        a.str("ab").str("c");
        let mut b = InputsHasher::new();
        b.str("a").str("bc");
        assert_ne!(a.finish(), b.finish(), "length must be part of the digest");

        let mut c = InputsHasher::new();
        c.str("c").str("ab");
        assert_ne!(a.finish(), c.finish(), "order must matter");
    }

    #[test]
    fn the_hash_is_stable() {
        // Pins the digest itself. This is a format contract: every committed
        // bake carries one, so a change here invalidates all of them and must
        // be a deliberate act with a re-bake attached.
        let mut h = InputsHasher::new();
        h.str("forge").u32(35).f32(1.5);
        assert_eq!(h.finish(), "a9e1d361ee9d27a9");
    }

    #[test]
    fn a_round_trip_reproduces_the_file() {
        let baked = BakedGi {
            header: BakeHeader {
                format: FORMAT.into(),
                scene: "m35_gi.json".into(),
                entity: "Lighting".into(),
                inputs_hash: "0123456789abcdef".into(),
                grid: [2, 2, 2],
                origin: [-1.0, 0.0, -1.0],
                spacing: 2.0,
                basis: [("sky".to_string(), SKY_BANDS as u32)]
                    .into_iter()
                    .collect(),
                samples: 256,
                bounces: 1,
                relocated: 0,
            },
            probes: (0..8)
                .map(|i| Probe {
                    p: [i % 2, (i / 2) % 2, i / 4],
                    sky: vec![vec![0.5; NUMBERS_PER_BASIS]; SKY_BANDS],
                })
                .collect(),
        };

        let text = baked.to_text();
        let reparsed = BakedGi::parse(&text).expect("round trip parses");
        assert_eq!(reparsed, baked);
        assert_eq!(
            reparsed.to_text(),
            text,
            "a re-serialize must be byte-identical, or the re-bake test is a coin flip"
        );
    }

    #[test]
    fn a_truncated_file_is_malformed_not_silently_short() {
        // The failure this format exists to prevent: everything runs, and the
        // picture is quietly wrong because half the volume is missing.
        let text = "{\"format\":\"forge-gi/1\",\"scene\":\"s.json\",\"entity\":\"L\",\
                    \"inputs_hash\":\"0\",\"grid\":[2,2,2],\"origin\":[0,0,0],\"spacing\":2.0,\
                    \"basis\":{\"sky\":2},\"samples\":16,\"bounces\":1}\n\
                    {\"p\":[0,0,0],\"sky\":[[0.0],[0.0]]}\n";
        let err = BakedGi::parse(text).unwrap_err();
        assert!(
            matches!(err, BakeError::Malformed(m) if m.contains("format is 2 of 12")),
            "a short basis vector must be named, not padded"
        );
    }

    #[test]
    fn a_future_format_is_refused_rather_than_guessed() {
        let text = "{\"format\":\"forge-gi/2\",\"scene\":\"s.json\",\"entity\":\"L\",\
                    \"inputs_hash\":\"0\",\"grid\":[2,2,2],\"origin\":[0,0,0],\"spacing\":2.0,\
                    \"basis\":{\"sky\":2},\"samples\":16,\"bounces\":1}\n";
        assert!(matches!(
            BakedGi::parse(text).unwrap_err(),
            BakeError::Malformed(m) if m.contains("forge-gi/1")
        ));
    }
}
