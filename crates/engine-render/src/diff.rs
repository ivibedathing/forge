//! Image comparison for visual regression (`engine diff-render`).
//!
//! Pure CPU functions over [`Image`] — no GPU, no wgpu types — so everything
//! here unit-tests everywhere, including GPU-less CI. The comparison model is
//! two knobs, each explainable in one sentence (design doc: no perceptual
//! metrics — the consumer is an agent that needs *predictable* semantics):
//!
//! 1. A pixel **differs** when any of its four RGBA channels deviates from
//!    the baseline by more than `threshold` (absolute, 0–255 scale).
//! 2. The comparison **passes** when the percentage of differing pixels is
//!    at most `max_diff_percent`.
//!
//! Comparison happens on the bytes an agent sees in the PNG — sRGB-encoded
//! values, no decode-to-linear round trip. The PNG is the contract.

use engine_core::{codes, EngineError, Result};

use crate::offscreen::Image;

/// Bounding box of all violating pixels, inclusive on all edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiffBounds {
    pub min_x: u32,
    pub min_y: u32,
    pub max_x: u32,
    pub max_y: u32,
}

/// What the comparison found.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DiffStats {
    /// Pixels with at least one channel beyond the threshold.
    pub diff_pixels: u64,
    pub total_pixels: u64,
    /// Largest single-channel deviation anywhere — including within-threshold
    /// pixels. Makes near-misses self-diagnosing: `1` says "precision noise,
    /// raise the threshold"; `214` says "something actually moved".
    pub max_channel_delta: u8,
    /// Absent when nothing violates.
    pub bounds: Option<DiffBounds>,
}

impl DiffStats {
    pub fn diff_percent(&self) -> f64 {
        if self.total_pixels == 0 {
            return 0.0;
        }
        self.diff_pixels as f64 * 100.0 / self.total_pixels as f64
    }

    /// The pass verdict for a given percent budget.
    pub fn passes(&self, max_diff_percent: f64) -> bool {
        self.diff_percent() <= max_diff_percent
    }
}

/// Diff-image pixel classes, pinned as part of the contract so a diff image
/// is itself comparable across runs: red means violation, yellow means
/// tolerated (nonzero but within threshold — drift stays *visible* before it
/// becomes a flake), faded grayscale means identical.
pub const DIFF_VIOLATION: [u8; 4] = [255, 0, 0, 255];
pub const DIFF_TOLERATED: [u8; 4] = [255, 200, 0, 255];

/// The faded-grayscale rendering of an unchanged baseline pixel: structure
/// stays recognizable, nothing screams.
pub fn faded(baseline: [u8; 4]) -> [u8; 4] {
    let [r, g, b, _] = baseline.map(u32::from);
    let luma = (r + 2 * g + b) / 4;
    let gray = (191 + luma / 4) as u8;
    [gray, gray, gray, 255]
}

/// Compare `actual` against `baseline` and produce the stats plus the visual
/// diff image. Never touches the GPU.
pub fn diff(actual: &Image, baseline: &Image, threshold: u8) -> Result<(DiffStats, Image)> {
    if actual.width != baseline.width || actual.height != baseline.height {
        return Err(EngineError::new(
            codes::DIMENSION_MISMATCH,
            format!(
                "cannot compare a {}x{} image against a {}x{} baseline",
                actual.width, actual.height, baseline.width, baseline.height
            ),
        ));
    }

    let mut stats = DiffStats {
        diff_pixels: 0,
        total_pixels: u64::from(actual.width) * u64::from(actual.height),
        max_channel_delta: 0,
        bounds: None,
    };
    let mut pixels = Vec::with_capacity(actual.pixels.len());

    for y in 0..actual.height {
        for x in 0..actual.width {
            let a = actual.pixel(x, y);
            let b = baseline.pixel(x, y);

            let mut pixel_delta = 0u8;
            for channel in 0..4 {
                pixel_delta = pixel_delta.max(a[channel].abs_diff(b[channel]));
            }
            stats.max_channel_delta = stats.max_channel_delta.max(pixel_delta);

            let class = if pixel_delta > threshold {
                stats.diff_pixels += 1;
                stats.bounds = Some(match stats.bounds {
                    None => DiffBounds {
                        min_x: x,
                        min_y: y,
                        max_x: x,
                        max_y: y,
                    },
                    Some(bounds) => DiffBounds {
                        min_x: bounds.min_x.min(x),
                        min_y: bounds.min_y.min(y),
                        max_x: bounds.max_x.max(x),
                        max_y: bounds.max_y.max(y),
                    },
                });
                DIFF_VIOLATION
            } else if pixel_delta > 0 {
                DIFF_TOLERATED
            } else {
                faded(b)
            };
            pixels.extend_from_slice(&class);
        }
    }

    let image = Image {
        width: actual.width,
        height: actual.height,
        pixels,
    };
    Ok((stats, image))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A solid-color image with a few explicit pixel overrides.
    fn image(width: u32, height: u32, fill: [u8; 4], overrides: &[(u32, u32, [u8; 4])]) -> Image {
        let mut pixels = Vec::with_capacity((width * height * 4) as usize);
        for _ in 0..width * height {
            pixels.extend_from_slice(&fill);
        }
        let mut image = Image {
            width,
            height,
            pixels,
        };
        for &(x, y, value) in overrides {
            let i = ((y * width + x) * 4) as usize;
            image.pixels[i..i + 4].copy_from_slice(&value);
        }
        image
    }

    const GRAY: [u8; 4] = [100, 100, 100, 255];

    #[test]
    fn identical_images_pass_with_zero_stats() {
        let a = image(8, 8, GRAY, &[]);
        let b = image(8, 8, GRAY, &[]);
        let (stats, _) = diff(&a, &b, 0).unwrap();

        assert_eq!(stats.diff_pixels, 0);
        assert_eq!(stats.total_pixels, 64);
        assert_eq!(stats.max_channel_delta, 0);
        assert_eq!(stats.bounds, None);
        assert!(stats.passes(0.0));
    }

    #[test]
    fn the_threshold_boundary_is_the_contract() {
        let baseline = image(4, 4, GRAY, &[]);

        // Off by exactly the threshold: tolerated.
        let at = image(4, 4, GRAY, &[(1, 1, [103, 100, 100, 255])]);
        let (stats, _) = diff(&at, &baseline, 3).unwrap();
        assert_eq!(stats.diff_pixels, 0, "delta == threshold must pass");
        assert_eq!(stats.max_channel_delta, 3, "but the delta is still reported");

        // Off by threshold + 1: violation.
        let over = image(4, 4, GRAY, &[(1, 1, [104, 100, 100, 255])]);
        let (stats, _) = diff(&over, &baseline, 3).unwrap();
        assert_eq!(stats.diff_pixels, 1);

        // At threshold 0, off by 1 anywhere fails — bit-exact is the default.
        let off_by_one = image(4, 4, GRAY, &[(0, 0, [100, 101, 100, 255])]);
        let (stats, _) = diff(&off_by_one, &baseline, 0).unwrap();
        assert_eq!(stats.diff_pixels, 1);
    }

    #[test]
    fn alpha_is_compared_like_any_other_channel() {
        let baseline = image(2, 2, GRAY, &[]);
        let actual = image(2, 2, GRAY, &[(0, 1, [100, 100, 100, 200])]);
        let (stats, _) = diff(&actual, &baseline, 0).unwrap();
        assert_eq!(stats.diff_pixels, 1);
        assert_eq!(stats.max_channel_delta, 55);
    }

    #[test]
    fn max_channel_delta_is_the_true_maximum() {
        let baseline = image(4, 1, GRAY, &[]);
        let actual = image(
            4,
            1,
            GRAY,
            &[
                (0, 0, [110, 100, 100, 255]), // delta 10
                (2, 0, [100, 100, 58, 255]),  // delta 42
                (3, 0, [100, 99, 100, 255]),  // delta 1
            ],
        );
        let (stats, _) = diff(&actual, &baseline, 0).unwrap();
        assert_eq!(stats.max_channel_delta, 42);
        assert_eq!(stats.diff_pixels, 3);
    }

    #[test]
    fn bounds_are_the_tight_box_around_violations() {
        let baseline = image(16, 16, GRAY, &[]);
        let actual = image(
            16,
            16,
            GRAY,
            &[
                (3, 2, [0, 0, 0, 255]),
                (12, 2, [0, 0, 0, 255]),
                (7, 9, [0, 0, 0, 255]),
            ],
        );
        let (stats, _) = diff(&actual, &baseline, 0).unwrap();
        assert_eq!(
            stats.bounds,
            Some(DiffBounds {
                min_x: 3,
                min_y: 2,
                max_x: 12,
                max_y: 9
            })
        );
    }

    #[test]
    fn tolerated_pixels_do_not_widen_the_bounds() {
        let baseline = image(8, 8, GRAY, &[]);
        let actual = image(
            8,
            8,
            GRAY,
            &[
                (4, 4, [0, 0, 0, 255]),       // violation
                (0, 0, [101, 100, 100, 255]), // within threshold 2
            ],
        );
        let (stats, _) = diff(&actual, &baseline, 2).unwrap();
        assert_eq!(
            stats.bounds,
            Some(DiffBounds {
                min_x: 4,
                min_y: 4,
                max_x: 4,
                max_y: 4
            })
        );
    }

    #[test]
    fn the_percent_budget_boundary_is_inclusive() {
        // 1 violating pixel out of 100 = exactly 1.0%.
        let baseline = image(10, 10, GRAY, &[]);
        let actual = image(10, 10, GRAY, &[(5, 5, [0, 0, 0, 255])]);
        let (stats, _) = diff(&actual, &baseline, 0).unwrap();

        assert!(stats.passes(1.0), "exactly on budget passes");
        assert!(!stats.passes(0.99));
        assert!(!stats.passes(0.0));
    }

    #[test]
    fn diff_image_pixel_classes_are_pinned() {
        let baseline = image(3, 1, [100, 150, 200, 255], &[]);
        let actual = image(
            3,
            1,
            [100, 150, 200, 255],
            &[
                (1, 0, [101, 150, 200, 255]), // within threshold 2 → yellow
                (2, 0, [255, 150, 200, 255]), // violation → red
            ],
        );
        let (_, image) = diff(&actual, &baseline, 2).unwrap();

        // Unchanged: gray = 191 + luma/4, luma = (r + 2g + b)/4 = 150.
        assert_eq!(image.pixel(0, 0), [228, 228, 228, 255]);
        assert_eq!(image.pixel(1, 0), DIFF_TOLERATED);
        assert_eq!(image.pixel(2, 0), DIFF_VIOLATION);
    }

    #[test]
    fn faded_formula_endpoints() {
        assert_eq!(faded([0, 0, 0, 255]), [191, 191, 191, 255]);
        assert_eq!(faded([255, 255, 255, 255]), [254, 254, 254, 255]);
    }

    #[test]
    fn dimension_mismatch_is_rejected() {
        let a = image(4, 4, GRAY, &[]);
        let b = image(4, 5, GRAY, &[]);
        let err = diff(&a, &b, 0).unwrap_err();
        assert_eq!(err.error, "dimension_mismatch");
    }
}
