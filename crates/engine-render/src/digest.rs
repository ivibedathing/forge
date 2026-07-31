//! A one-pass summary of a rendered frame (`engine screenshot`, `filmstrip`).
//!
//! `entities_drawn` catches "nothing loaded". It does not catch "everything is
//! black", which is the more common bad render — a scene with one light and no
//! ambient, a camera inside geometry, a sun aimed away — and whose diagnosis
//! otherwise costs an image read. The frame is already in memory before the
//! PNG encode, so summarizing it is a pass over a resident buffer.
//!
//! Pure CPU over [`Image`], like [`diff`](crate::diff), so it unit-tests
//! everywhere including GPU-less CI, and so the render path is untouched: the
//! CLI calls this *between* rendering and encoding.
//!
//! **This is a diagnostic and never a pin.** `diff-render` is what pins a
//! render, bit-exactly, with a diff image showing where. Two things follow.
//! The numbers are **quantized** — M22 records that a terrain patch under MSAA
//! renders ~24 pixels differently run to run on this adapter, and a
//! full-precision mean over the frame would differ in its low digits between
//! two runs of an unchanged scene, turning a diagnostic into a source of
//! phantom diffs. And there is deliberately no hash: a hash invites comparing
//! two renders by number, which is the job `diff-render` already does properly.
//!
//! Adapter noise is three to four orders of magnitude below the quantum
//! (24 pixels at 6/255 over a 640×360 frame moves the mean luminance by
//! ~2e-6, against a 1e-3 step), so the same scene reports the same digest —
//! but that is a property of how small the noise is, not a guarantee, which
//! is the other reason nothing may pin it.

use std::collections::HashMap;

use crate::offscreen::Image;

/// Rounding applied to every reported fraction: three decimals.
///
/// Chosen against measured adapter noise rather than for looks — see the
/// module docs.
const QUANTUM: f64 = 1000.0;

/// What one frame looks like, in four numbers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Digest {
    /// Mean luminance over the frame, `0.0`–`1.0`.
    ///
    /// Computed on the **encoded** (sRGB) bytes the PNG carries, not on
    /// linearized values: the question this answers is "does the image look
    /// black", and the image is the encoded one. Not a photometric quantity.
    pub mean_luminance: f64,
    /// The most common exact color in the frame — the sky, the clear color, or
    /// whatever fills the empty part of the shot — as sRGB bytes.
    pub background: [u8; 3],
    /// Fraction of pixels that are *not* that color, `0.0`–`1.0`.
    ///
    /// The number that separates "black frame" (`0.0` — nothing but
    /// background) from "black background with a lit subject in it".
    pub coverage: f64,
}

/// Summarize a rendered frame.
///
/// Ties on the most common color are broken toward the numerically smallest,
/// so a frame split exactly between two colors still reports the same
/// background every run — the determinism this file exists to preserve would
/// otherwise be lost in a `HashMap`'s iteration order.
pub fn of(image: &Image) -> Digest {
    let total = (image.width as u64) * (image.height as u64);
    if total == 0 || image.pixels.len() < 4 {
        return Digest {
            mean_luminance: 0.0,
            background: [0, 0, 0],
            coverage: 0.0,
        };
    }

    let mut luminance_sum = 0.0f64;
    let mut counts: HashMap<[u8; 3], u64> = HashMap::new();

    for pixel in image.pixels.chunks_exact(4) {
        let rgb = [pixel[0], pixel[1], pixel[2]];
        // Rec. 709 luma weights, on the encoded values.
        luminance_sum += 0.2126 * f64::from(rgb[0])
            + 0.7152 * f64::from(rgb[1])
            + 0.0722 * f64::from(rgb[2]);
        *counts.entry(rgb).or_insert(0) += 1;
    }

    let (background, background_count) = counts
        .into_iter()
        .max_by(|(a_color, a_count), (b_color, b_count)| {
            a_count.cmp(b_count).then(b_color.cmp(a_color))
        })
        .expect("a non-empty frame has at least one color");

    Digest {
        mean_luminance: quantize(luminance_sum / (total as f64 * 255.0)),
        background,
        coverage: quantize((total - background_count) as f64 / total as f64),
    }
}

fn quantize(value: f64) -> f64 {
    (value * QUANTUM).round() / QUANTUM
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(width: u32, height: u32, fill: [u8; 4]) -> Image {
        Image {
            width,
            height,
            pixels: fill.repeat((width * height) as usize),
        }
    }

    #[test]
    fn an_all_black_frame_reports_no_coverage() {
        // The render this exists to diagnose without an image read.
        let digest = of(&image(16, 16, [0, 0, 0, 255]));
        assert_eq!(digest.mean_luminance, 0.0);
        assert_eq!(digest.background, [0, 0, 0]);
        assert_eq!(digest.coverage, 0.0, "nothing is in front of the background");
    }

    #[test]
    fn a_subject_on_a_black_background_is_visibly_different_from_an_empty_frame() {
        let mut frame = image(10, 10, [0, 0, 0, 255]);
        // A 2×2 white square: 4 of 100 pixels.
        for (x, y) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
            let i = ((y * 10 + x) * 4) as usize;
            frame.pixels[i..i + 3].copy_from_slice(&[255, 255, 255]);
        }

        let digest = of(&frame);
        assert_eq!(digest.background, [0, 0, 0], "the empty part is still the mode");
        assert_eq!(digest.coverage, 0.04);
        assert!(
            digest.mean_luminance > 0.0,
            "a lit subject lifts the mean off zero"
        );
    }

    #[test]
    fn a_white_frame_is_fully_luminous() {
        let digest = of(&image(8, 8, [255, 255, 255, 255]));
        assert_eq!(digest.mean_luminance, 1.0);
        assert_eq!(digest.background, [255, 255, 255]);
        assert_eq!(digest.coverage, 0.0);
    }

    #[test]
    fn luminance_weights_green_over_blue() {
        // Not a mean of channels: the same byte in green reads brighter than
        // in blue, which is what makes "looks black" the right question.
        let green = of(&image(4, 4, [0, 200, 0, 255])).mean_luminance;
        let blue = of(&image(4, 4, [0, 0, 200, 255])).mean_luminance;
        assert!(green > blue, "{green} should exceed {blue}");
    }

    #[test]
    fn quantization_absorbs_adapter_noise() {
        // M22's measured worst case, scaled up: 100 pixels of a 640×360 frame
        // moving by 18/255 must not move a reported digit.
        let clean = image(640, 360, [40, 40, 40, 255]);
        let mut noisy = clean.clone();
        for pixel in 0..100usize {
            let i = pixel * 4;
            noisy.pixels[i] = 58;
            noisy.pixels[i + 1] = 58;
            noisy.pixels[i + 2] = 58;
        }
        assert_eq!(
            of(&clean).mean_luminance,
            of(&noisy).mean_luminance,
            "adapter noise must not move a diagnostic"
        );
    }

    #[test]
    fn a_tie_on_the_background_resolves_the_same_way_every_run() {
        // Half red, half blue: whichever wins, it must not depend on hash
        // iteration order.
        let mut frame = image(2, 1, [255, 0, 0, 255]);
        frame.pixels[4..7].copy_from_slice(&[0, 0, 255]);
        let first = of(&frame).background;
        for _ in 0..64 {
            assert_eq!(of(&frame).background, first);
        }
    }

    #[test]
    fn a_zero_sized_frame_does_not_panic() {
        let digest = of(&Image {
            width: 0,
            height: 0,
            pixels: Vec::new(),
        });
        assert_eq!(digest.coverage, 0.0);
    }
}
