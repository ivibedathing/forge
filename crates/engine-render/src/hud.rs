//! CPU rasterizer for the screen-space HUD.
//!
//! Two layers share this one code path:
//!
//! - **Components** (M12): `HudRect` and `HudText` entities from the scene
//!   file, anchored and colored per their schema.
//! - **Script debug lines** (M11.6): the printable-ASCII lines the last step
//!   pushed through `world.hud(...)`, drawn on their translucent panel in the
//!   top-left corner, topmost.
//!
//! Like `diff.rs`, this is deliberately GPU-free: `rasterize` is a pure
//! function of (HUD content, canvas dimensions), so anchor math, glyph
//! placement, and compositing are unit-tested everywhere, including machines
//! with no adapter. The GPU's only HUD job is blitting the finished canvas
//! over the lit scene (`SceneRenderer`'s overlay pass).
//!
//! Text uses the public-domain 8×8 pixel font at integer scales with no
//! anti-aliasing, so a glyph pixel is fully opaque and byte-exact in
//! baselines. Rect `opacity` and the panel's translucency composite here in
//! linear space; the canvas the GPU receives is sRGB-encoded straight-alpha
//! RGBA8.

use engine_core::components::{HudAnchor, HudRect, HudText};
use engine_core::scene::HudItems;

/// Glyph cell edge in font units; the font is fixed 8×8.
pub const GLYPH: u32 = 8;

// Debug-line panel geometry and colors. These are pinned by the lap
// baseline (`verify/baselines/m11_lap.png`): the panel bytes must land
// exactly as they did when that PNG was blessed.
/// Debug lines draw at this integer scale.
const SCALE: u32 = 2;
/// Padding between the text block and the panel edge, in pixels.
const PAD: u32 = 8;
/// Vertical gap between lines, in pixels.
const LINE_GAP: u32 = 6;
/// Panel offset from the top-left corner of the frame.
const MARGIN: u32 = 10;
/// Translucent dark panel behind the text, sRGB-encoded straight alpha.
const PANEL: [u8; 4] = [10, 12, 16, 200];
const TEXT: [u8; 4] = [255, 255, 255, 255];

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

/// Rasterize the overlay in draw order: component `rects` under component
/// `texts`, each in the order given (scene-file order, per
/// `Scene::hud_items`), and the script debug-line panel over everything.
pub fn rasterize(hud: &HudItems, lines: &[String], width: u32, height: u32) -> HudCanvas {
    // Linear-space straight-alpha accumulation; encoded once at the end.
    let mut canvas = vec![[0.0f32; 4]; (width as usize) * (height as usize)];

    for rect in &hud.rects {
        draw_rect(&mut canvas, width, height, rect);
    }
    for text in &hud.texts {
        draw_text(&mut canvas, width, height, text);
    }
    draw_lines(&mut canvas, width, height, lines);

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
        // Outside the font's ASCII coverage: a filled box — visibly wrong in
        // the screenshot, never a panic (hud-design.md §2).
        let bitmap = glyph(ch as u32);
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

/// The 8×8 bitmap for a code point, from the no-std ASCII table; anything
/// past ASCII is the filled box.
fn glyph(code: u32) -> [u8; 8] {
    font8x8::legacy::BASIC_LEGACY
        .get(code as usize)
        .copied()
        .unwrap_or([0xFF; 8])
}

/// Draw the script debug lines on their translucent panel, top-left. Layout
/// and colors are the M11.6 formulas — the lap baseline pins them.
fn draw_lines(canvas: &mut [[f32; 4]], width: u32, height: u32, lines: &[String]) {
    let longest = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    if lines.is_empty() || longest == 0 {
        return;
    }

    let cell = GLYPH * SCALE;
    let panel_width = longest as u32 * cell + 2 * PAD;
    let panel_height = lines.len() as u32 * cell + (lines.len() as u32 - 1) * LINE_GAP + 2 * PAD;

    // The panel bytes are authored in sRGB; decode so the end-of-rasterize
    // encode reproduces them exactly where the panel is topmost.
    let panel_color = [
        decode_srgb(PANEL[0]),
        decode_srgb(PANEL[1]),
        decode_srgb(PANEL[2]),
    ];
    let panel_alpha = PANEL[3] as f32 / 255.0;
    for y in 0..panel_height.min(height.saturating_sub(MARGIN)) {
        for x in 0..panel_width.min(width.saturating_sub(MARGIN)) {
            let i = (((MARGIN + y) * width) + MARGIN + x) as usize;
            blend(&mut canvas[i], panel_color, panel_alpha);
        }
    }

    let text_color = [
        decode_srgb(TEXT[0]),
        decode_srgb(TEXT[1]),
        decode_srgb(TEXT[2]),
    ];
    for (row, line) in lines.iter().enumerate() {
        let top = MARGIN + PAD + row as u32 * (cell + LINE_GAP);
        for (col, ch) in line.chars().enumerate() {
            // The script API enforces printable ASCII; anything else that
            // reaches us renders as '?' rather than a hole.
            let index = if (0x20..0x7f).contains(&(ch as u32)) {
                ch as u32
            } else {
                b'?' as u32
            };
            let bitmap = glyph(index);
            let left = MARGIN + PAD + col as u32 * cell;
            for (y, bits) in bitmap.iter().enumerate() {
                for x in 0..8u32 {
                    // font8x8 packs rows LSB-leftmost.
                    if bits & (1 << x) == 0 {
                        continue;
                    }
                    for sy in 0..SCALE {
                        for sx in 0..SCALE {
                            let px = left + x * SCALE + sx;
                            let py = top + y as u32 * SCALE + sy;
                            if px >= width || py >= height {
                                continue;
                            }
                            blend(&mut canvas[(py * width + px) as usize], text_color, 1.0);
                        }
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

/// sRGB-encoded byte → linear, the inverse of [`encode_srgb`]. Round-trips
/// every byte value, which is what keeps the authored panel bytes exact.
fn decode_srgb(byte: u8) -> f32 {
    let encoded = byte as f32 / 255.0;
    if encoded <= 0.040_45 {
        encoded / 12.92
    } else {
        ((encoded + 0.055) / 1.055).powf(2.4)
    }
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

    fn components(hud: &HudItems, width: u32, height: u32) -> HudCanvas {
        rasterize(hud, &[], width, height)
    }

    fn pixel_at(canvas: &HudCanvas, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * canvas.width + x) * 4) as usize;
        canvas.pixels[i..i + 4].try_into().unwrap()
    }

    fn alpha_at(canvas: &HudCanvas, x: u32, y: u32) -> u8 {
        pixel_at(canvas, x, y)[3]
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
        let canvas = components(&HudItems::default(), 8, 8);
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
            let canvas = components(&only_rects(vec![rect(anchor, [1.0, 1.0], [2.0, 2.0])]), 10, 10);
            let expected: Vec<(u32, u32)> =
                vec![(ex, ey), (ex + 1, ey), (ex, ey + 1), (ex + 1, ey + 1)];
            assert_eq!(covered(&canvas), expected, "{anchor:?}");
        }
    }

    #[test]
    fn center_anchor_centers_the_element() {
        let canvas = components(
            &only_rects(vec![rect(HudAnchor::Center, [0.0, 0.0], [2.0, 2.0])]),
            10,
            10,
        );
        assert_eq!(covered(&canvas), vec![(4, 4), (5, 4), (4, 5), (5, 5)]);
    }

    #[test]
    fn rects_clip_at_canvas_edges_instead_of_panicking() {
        let canvas = components(
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
        let canvas = components(&only_rects(vec![r]), 1, 1);
        assert_eq!(&canvas.pixels, &[188, 188, 188, 255]);
    }

    #[test]
    fn fractional_opacity_lands_in_the_alpha_channel() {
        let mut r = rect(HudAnchor::TopLeft, [0.0, 0.0], [1.0, 1.0]);
        r.opacity = 0.5;
        let canvas = components(&only_rects(vec![r]), 1, 1);
        assert_eq!(alpha_at(&canvas, 0, 0), 128);
    }

    #[test]
    fn later_rect_draws_over_earlier_and_text_over_rects() {
        let mut under = rect(HudAnchor::TopLeft, [0.0, 0.0], [1.0, 1.0]);
        under.color = Vec3::new(1.0, 0.0, 0.0);
        let mut over = rect(HudAnchor::TopLeft, [0.0, 0.0], [1.0, 1.0]);
        over.color = Vec3::new(0.0, 1.0, 0.0);
        let canvas = components(&only_rects(vec![under, over]), 1, 1);
        assert_eq!(&canvas.pixels, &[0, 255, 0, 255], "file order is z-order");

        // A '█'-free check that text wins over rects: the fallback box glyph
        // is fully filled, so every pixel of its cell takes the text color.
        let mut blue = text("\u{2588}", HudAnchor::TopLeft, [0.0, 0.0], 8.0);
        blue.color = Vec3::new(0.0, 0.0, 1.0);
        let mut red_rect = rect(HudAnchor::TopLeft, [0.0, 0.0], [8.0, 8.0]);
        red_rect.color = Vec3::new(1.0, 0.0, 0.0);
        let canvas = components(
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
        let l = components(
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

        let dot = components(
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
        let canvas = components(
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
        let canvas = components(
            &HudItems {
                rects: vec![],
                texts: vec![text("\u{2588}", HudAnchor::TopLeft, [0.0, 0.0], 8.0)],
            },
            8,
            8,
        );
        assert_eq!(covered(&canvas).len(), 64);
    }

    // ---- Script debug-line panel (the M11.6 layer) ----

    fn lines_only(lines: &[&str], width: u32, height: u32) -> HudCanvas {
        let lines: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
        rasterize(&HudItems::default(), &lines, width, height)
    }

    #[test]
    fn empty_lines_draw_nothing() {
        let canvas = lines_only(&[], 64, 64);
        assert!(canvas.pixels.iter().all(|&b| b == 0));
        let blank = lines_only(&[""], 64, 64);
        assert!(blank.pixels.iter().all(|&b| b == 0));
    }

    #[test]
    fn panel_size_follows_the_text_block() {
        // 4 chars * 16px + 2*8 padding = 80 wide; 2 lines * 16 + 1 gap * 6 +
        // 16 = 54 tall, at MARGIN from the top-left corner.
        let canvas = lines_only(&["ABCD", "A"], 200, 100);
        assert_eq!(pixel_at(&canvas, MARGIN, MARGIN), PANEL);
        assert_eq!(pixel_at(&canvas, MARGIN + 79, MARGIN + 53), PANEL);
        assert_eq!(alpha_at(&canvas, MARGIN - 1, MARGIN), 0, "left of the panel");
        assert_eq!(alpha_at(&canvas, MARGIN + 80, MARGIN), 0, "right of the panel");
        assert_eq!(alpha_at(&canvas, MARGIN, MARGIN + 54), 0, "below the panel");
    }

    #[test]
    fn glyphs_land_as_white_pixels_on_the_panel() {
        let canvas = lines_only(&["A"], 64, 64);
        // Somewhere inside the glyph cell there is text; 'A' is not blank.
        let cell_origin = MARGIN + PAD;
        let cell = (cell_origin..cell_origin + GLYPH * SCALE)
            .flat_map(|y| (cell_origin..cell_origin + GLYPH * SCALE).map(move |x| (x, y)));
        assert!(cell.clone().any(|(x, y)| pixel_at(&canvas, x, y) == TEXT));
        // And a space renders as bare panel everywhere.
        let space = lines_only(&[" "], 64, 64);
        assert!(cell.clone().all(|(x, y)| pixel_at(&space, x, y) == PANEL));
    }

    #[test]
    fn panel_draws_over_component_elements() {
        let mut r = rect(HudAnchor::TopLeft, [0.0, 0.0], [64.0, 64.0]);
        r.color = Vec3::new(1.0, 0.0, 0.0);
        let lines = vec!["A".to_string()];
        let canvas = rasterize(&only_rects(vec![r]), &lines, 64, 64);
        // Where the panel covers the rect, the pixel is panel-over-red, not
        // bare red — the alpha channel is saturated by the opaque rect below.
        let panel_pixel = pixel_at(&canvas, MARGIN, MARGIN);
        assert_eq!(panel_pixel[3], 255);
        assert_ne!(&panel_pixel[..3], &[255, 0, 0], "panel tints the rect");
        // Outside the panel the rect is untouched.
        assert_eq!(pixel_at(&canvas, 63, 63), [255, 0, 0, 255]);
    }

    #[test]
    fn rasterization_is_deterministic() {
        let a = lines_only(&["SPEED 42 KM/H"], 320, 200);
        let b = lines_only(&["SPEED 42 KM/H"], 320, 200);
        assert_eq!(a.pixels, b.pixels);
    }
}
