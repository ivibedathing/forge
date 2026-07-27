//! CPU rasterizer for the screen-space HUD (M12).
//!
//! Like `diff.rs`, this is deliberately GPU-free: `rasterize` is a pure
//! function of (HUD components, canvas dimensions), so anchor math, glyph
//! placement, and compositing are unit-tested everywhere, including machines
//! with no adapter. The GPU's only HUD job is blitting the finished canvas
//! over the lit scene (`SceneRenderer`'s overlay pass).
//!
//! Text uses the public-domain 8×8 pixel font at integer scales with no
//! anti-aliasing, so a glyph pixel is fully opaque and byte-exact in
//! baselines. Rect `opacity` composites here in linear space; the canvas the
//! GPU receives is sRGB-encoded straight-alpha RGBA8.

use engine_core::components::{HudAnchor, HudRect, HudText};
use engine_core::scene::HudItems;
use font8x8::UnicodeFonts;

/// Glyph cell edge in font units; the font is fixed 8×8.
pub const GLYPH: u32 = 8;

/// A rasterized overlay, sized to the render target: sRGB-encoded
/// straight-alpha RGBA8, tightly packed.
pub struct HudCanvas {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// The integer scale factor `HudText.size` snaps to: `max(1, round(size / 8))`.
pub fn text_scale(size: f32) -> u32 {
    ((size / GLYPH as f32).round() as i64).max(1) as u32
}

/// Rasterize the overlay in draw order: `rects` under `texts`, each in the
/// order given (scene-file order, per `Scene::hud_items`).
pub fn rasterize(hud: &HudItems, width: u32, height: u32) -> HudCanvas {
    // Linear-space straight-alpha accumulation; encoded once at the end.
    let mut canvas = vec![[0.0f32; 4]; (width as usize) * (height as usize)];

    for rect in &hud.rects {
        draw_rect(&mut canvas, width, height, rect);
    }
    for text in &hud.texts {
        draw_text(&mut canvas, width, height, text);
    }

    let pixels = canvas
        .iter()
        .flat_map(|&[r, g, b, a]| {
            [
                encode_srgb(r),
                encode_srgb(g),
                encode_srgb(b),
                (a.clamp(0.0, 1.0) * 255.0).round() as u8,
            ]
        })
        .collect();

    HudCanvas {
        width,
        height,
        pixels,
    }
}

/// Pixel box of a HUD element: top-left corner from the anchor rule
/// (`HudAnchor` doc), rounded to whole pixels.
fn anchored_box(
    anchor: HudAnchor,
    offset: glam::Vec2,
    element: (f32, f32),
    canvas: (u32, u32),
) -> (i64, i64) {
    let (w, h) = element;
    let (cw, ch) = (canvas.0 as f32, canvas.1 as f32);
    let (x, y) = match anchor {
        HudAnchor::TopLeft => (offset.x, offset.y),
        HudAnchor::TopRight => (cw - offset.x - w, offset.y),
        HudAnchor::BottomLeft => (offset.x, ch - offset.y - h),
        HudAnchor::BottomRight => (cw - offset.x - w, ch - offset.y - h),
        HudAnchor::Center => (
            cw / 2.0 + offset.x - w / 2.0,
            ch / 2.0 + offset.y - h / 2.0,
        ),
    };
    (x.round() as i64, y.round() as i64)
}

fn draw_rect(canvas: &mut [[f32; 4]], width: u32, height: u32, rect: &HudRect) {
    let (x0, y0) = anchored_box(
        rect.anchor,
        rect.offset,
        (rect.size.x, rect.size.y),
        (width, height),
    );
    // Rounding the far edge (not the size) keeps adjacent rects gapless.
    let x1 = x0 + rect.size.x.round() as i64;
    let y1 = y0 + rect.size.y.round() as i64;

    let color = [rect.color.x, rect.color.y, rect.color.z];
    let alpha = rect.opacity.clamp(0.0, 1.0);
    for y in y0.max(0)..y1.min(height as i64) {
        for x in x0.max(0)..x1.min(width as i64) {
            blend(&mut canvas[(y as u32 * width + x as u32) as usize], color, alpha);
        }
    }
}

fn draw_text(canvas: &mut [[f32; 4]], width: u32, height: u32, text: &HudText) {
    let scale = text_scale(text.size);
    let cell = (GLYPH * scale) as i64;
    let text_width = (text.text.chars().count() as u32 * GLYPH * scale) as f32;
    let (x0, y0) = anchored_box(
        text.anchor,
        text.offset,
        (text_width, (GLYPH * scale) as f32),
        (width, height),
    );

    let color = [text.color.x, text.color.y, text.color.z];
    for (index, ch) in text.text.chars().enumerate() {
        // Outside the font's coverage: a filled box — visibly wrong in the
        // screenshot, never a panic (hud-design.md §2).
        let bitmap = font8x8::BASIC_FONTS.get(ch).unwrap_or([0xFF; 8]);
        let gx = x0 + index as i64 * cell;
        for (row, bits) in bitmap.iter().enumerate() {
            for col in 0..GLYPH as usize {
                if bits >> col & 1 == 0 {
                    continue;
                }
                // One font pixel is a scale×scale block, fully opaque.
                for dy in 0..scale as i64 {
                    for dx in 0..scale as i64 {
                        let x = gx + (col as i64 * scale as i64) + dx;
                        let y = y0 + (row as i64 * scale as i64) + dy;
                        if x < 0 || y < 0 || x >= width as i64 || y >= height as i64 {
                            continue;
                        }
                        blend(&mut canvas[(y as u32 * width + x as u32) as usize], color, 1.0);
                    }
                }
            }
        }
    }
}

/// Straight-alpha "over" in linear space.
fn blend(dst: &mut [f32; 4], src: [f32; 3], src_alpha: f32) {
    let keep = 1.0 - src_alpha;
    dst[0] = src[0] * src_alpha + dst[0] * dst[3] * keep;
    dst[1] = src[1] * src_alpha + dst[1] * dst[3] * keep;
    dst[2] = src[2] * src_alpha + dst[2] * dst[3] * keep;
    dst[3] = src_alpha + dst[3] * keep;
    if dst[3] > 0.0 {
        // Back to straight alpha so the encode step stores color, not
        // premultiplied color.
        dst[0] /= dst[3];
        dst[1] /= dst[3];
        dst[2] /= dst[3];
    }
}

/// Linear [0, 1] → sRGB-encoded byte, the standard piecewise transfer
/// function — the same math the render-target hardware applies on write, so
/// an opaque canvas pixel blitted with alpha 1 lands byte-identical.
fn encode_srgb(linear: f32) -> u8 {
    let linear = linear.clamp(0.0, 1.0);
    let encoded = if linear <= 0.003_130_8 {
        linear * 12.92
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    };
    (encoded * 255.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::{Vec2, Vec3};

    fn rect(anchor: HudAnchor, offset: [f32; 2], size: [f32; 2]) -> HudRect {
        HudRect {
            anchor,
            offset: Vec2::from(offset),
            size: Vec2::from(size),
            color: Vec3::ONE,
            opacity: 1.0,
        }
    }

    fn text(s: &str, anchor: HudAnchor, offset: [f32; 2], size: f32) -> HudText {
        HudText {
            text: s.into(),
            anchor,
            offset: Vec2::from(offset),
            size,
            color: Vec3::ONE,
        }
    }

    fn only_rects(rects: Vec<HudRect>) -> HudItems {
        HudItems {
            rects,
            texts: vec![],
        }
    }

    fn alpha_at(canvas: &HudCanvas, x: u32, y: u32) -> u8 {
        canvas.pixels[((y * canvas.width + x) * 4 + 3) as usize]
    }

    fn covered(canvas: &HudCanvas) -> Vec<(u32, u32)> {
        let mut on = Vec::new();
        for y in 0..canvas.height {
            for x in 0..canvas.width {
                if alpha_at(canvas, x, y) > 0 {
                    on.push((x, y));
                }
            }
        }
        on
    }

    #[test]
    fn empty_hud_is_fully_transparent() {
        let canvas = rasterize(&HudItems::default(), 8, 8);
        assert!(canvas.pixels.iter().all(|&b| b == 0));
    }

    #[test]
    fn rect_lands_exactly_where_each_anchor_says() {
        // A 2×2 rect, offset [1, 1], on a 10×10 canvas: the anchor rule puts
        // its outer corner one pixel in from the matching canvas corner.
        let cases = [
            (HudAnchor::TopLeft, (1, 1)),
            (HudAnchor::TopRight, (7, 1)),
            (HudAnchor::BottomLeft, (1, 7)),
            (HudAnchor::BottomRight, (7, 7)),
        ];
        for (anchor, (ex, ey)) in cases {
            let canvas = rasterize(&only_rects(vec![rect(anchor, [1.0, 1.0], [2.0, 2.0])]), 10, 10);
            let expected: Vec<(u32, u32)> =
                vec![(ex, ey), (ex + 1, ey), (ex, ey + 1), (ex + 1, ey + 1)];
            assert_eq!(covered(&canvas), expected, "{anchor:?}");
        }
    }

    #[test]
    fn center_anchor_centers_the_element() {
        let canvas = rasterize(
            &only_rects(vec![rect(HudAnchor::Center, [0.0, 0.0], [2.0, 2.0])]),
            10,
            10,
        );
        assert_eq!(covered(&canvas), vec![(4, 4), (5, 4), (4, 5), (5, 5)]);
    }

    #[test]
    fn rects_clip_at_canvas_edges_instead_of_panicking() {
        let canvas = rasterize(
            &only_rects(vec![rect(HudAnchor::TopLeft, [-3.0, -3.0], [100.0, 100.0])]),
            4,
            4,
        );
        assert_eq!(covered(&canvas).len(), 16, "clipped rect covers the whole canvas");
    }

    #[test]
    fn opaque_rect_color_is_srgb_encoded_exactly() {
        // Linear 0.5 encodes to 188 — the same expectation the lighting pixel
        // tests compute. Alpha stays linear (255).
        let mut r = rect(HudAnchor::TopLeft, [0.0, 0.0], [1.0, 1.0]);
        r.color = Vec3::splat(0.5);
        let canvas = rasterize(&only_rects(vec![r]), 1, 1);
        assert_eq!(&canvas.pixels, &[188, 188, 188, 255]);
    }

    #[test]
    fn fractional_opacity_lands_in_the_alpha_channel() {
        let mut r = rect(HudAnchor::TopLeft, [0.0, 0.0], [1.0, 1.0]);
        r.opacity = 0.5;
        let canvas = rasterize(&only_rects(vec![r]), 1, 1);
        assert_eq!(alpha_at(&canvas, 0, 0), 128);
    }

    #[test]
    fn later_rect_draws_over_earlier_and_text_over_rects() {
        let mut under = rect(HudAnchor::TopLeft, [0.0, 0.0], [1.0, 1.0]);
        under.color = Vec3::new(1.0, 0.0, 0.0);
        let mut over = rect(HudAnchor::TopLeft, [0.0, 0.0], [1.0, 1.0]);
        over.color = Vec3::new(0.0, 1.0, 0.0);
        let canvas = rasterize(&only_rects(vec![under, over]), 1, 1);
        assert_eq!(&canvas.pixels, &[0, 255, 0, 255], "file order is z-order");

        // A '█'-free check that text wins over rects: the fallback box glyph
        // is fully filled, so every pixel of its cell takes the text color.
        let mut blue = text("\u{2588}", HudAnchor::TopLeft, [0.0, 0.0], 8.0);
        blue.color = Vec3::new(0.0, 0.0, 1.0);
        let mut red_rect = rect(HudAnchor::TopLeft, [0.0, 0.0], [8.0, 8.0]);
        red_rect.color = Vec3::new(1.0, 0.0, 0.0);
        let canvas = rasterize(
            &HudItems {
                rects: vec![red_rect],
                texts: vec![blue],
            },
            8,
            8,
        );
        assert_eq!(&canvas.pixels[0..4], &[0, 0, 255, 255]);
    }

    #[test]
    fn text_scale_snaps_to_integer_multiples_of_eight() {
        assert_eq!(text_scale(8.0), 1);
        assert_eq!(text_scale(16.0), 2);
        assert_eq!(text_scale(12.0), 2, "12px rounds to 2× (16px)");
        assert_eq!(text_scale(4.0), 1, "the schema minimum still renders");
    }

    #[test]
    fn glyphs_are_upright_and_unmirrored() {
        // Orientation pins that survive not knowing the font tables by heart:
        // 'L' is bottom-left-heavy, '.' sits in the bottom half. A flipped
        // bit-order or row-order breaks at least one.
        let l = rasterize(
            &HudItems {
                rects: vec![],
                texts: vec![text("L", HudAnchor::TopLeft, [0.0, 0.0], 8.0)],
            },
            8,
            8,
        );
        let on = covered(&l);
        let left = on.iter().filter(|(x, _)| *x < 4).count();
        let right = on.len() - left;
        assert!(left > right, "'L' should be left-heavy: {on:?}");
        let bottom_row_lit = on.iter().filter(|(_, y)| *y >= 5).count();
        assert!(bottom_row_lit >= 3, "'L' has a bottom bar: {on:?}");

        let dot = rasterize(
            &HudItems {
                rects: vec![],
                texts: vec![text(".", HudAnchor::TopLeft, [0.0, 0.0], 8.0)],
            },
            8,
            8,
        );
        assert!(
            covered(&dot).iter().all(|(_, y)| *y >= 4),
            "'.' sits in the bottom half: {:?}",
            covered(&dot)
        );
    }

    #[test]
    fn text_width_scales_with_glyph_count_and_scale() {
        // Right-anchored text must measure itself to sit flush: two glyphs at
        // 2× are 32px wide, so offset 0 puts the last column at the edge.
        let canvas = rasterize(
            &HudItems {
                rects: vec![],
                texts: vec![text("\u{2588}\u{2588}", HudAnchor::TopRight, [0.0, 0.0], 16.0)],
            },
            64,
            16,
        );
        let on = covered(&canvas);
        assert!(on.contains(&(63, 0)), "flush against the right edge");
        assert!(on.contains(&(32, 0)), "extends 32px left of the edge");
        assert!(!on.contains(&(31, 0)), "and no further");
    }

    #[test]
    fn unknown_glyphs_render_as_a_filled_box() {
        let canvas = rasterize(
            &HudItems {
                rects: vec![],
                texts: vec![text("\u{2588}", HudAnchor::TopLeft, [0.0, 0.0], 8.0)],
            },
            8,
            8,
        );
        assert_eq!(covered(&canvas).len(), 64);
    }
}
