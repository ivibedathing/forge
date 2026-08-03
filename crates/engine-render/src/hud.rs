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

use std::sync::Arc;

use engine_core::components::{HudImage, HudPanel, HudRect, HudText};
use engine_core::texture::TextureData;
use engine_core::ui::{self, HudKind, HudTree, PixelRect, UiLayout};

/// Glyph cell edge in font units; the font is fixed 8×8. The layout engine
/// owns the measurement formulas (M31) — this is a re-export so the rasterizer
/// and the layout can never disagree about a glyph cell.
pub use engine_core::ui::{text_scale, GLYPH};

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

/// A rasterized overlay: sRGB-encoded straight-alpha RGBA8, tightly packed.
///
/// The canvas covers only the region the HUD actually touches — the union of
/// every element's clipped pixel box, positioned at (`origin_x`, `origin_y`)
/// in the render target. A frame's HUD is typically a few percent of the
/// screen, and rasterizing (and uploading) the whole target instead was the
/// single largest per-frame cost in the viewer: at 2560×1440 it cost ~29 ms of
/// CPU per frame, which capped the frame rate far below what the scene itself
/// could sustain. Pixels outside the region are transparent by construction,
/// and compositing a transparent pixel is a no-op, so the smaller canvas is
/// bit-identical to a full-screen one.
///
/// An empty HUD produces an empty canvas (`is_empty`); the overlay pass then
/// does not run at all.
pub struct HudCanvas {
    /// Where this canvas' (0, 0) pixel sits in the render target.
    pub origin_x: u32,
    pub origin_y: u32,
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl HudCanvas {
    /// Nothing to composite — no element covered a pixel of the target.
    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// RGBA at a *render-target* pixel, transparent outside the covered
    /// region — the view of the canvas that predates the region optimization,
    /// and the one anything reasoning about screen coordinates wants.
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let (Some(lx), Some(ly)) = (x.checked_sub(self.origin_x), y.checked_sub(self.origin_y))
        else {
            return [0; 4];
        };
        if lx >= self.width || ly >= self.height {
            return [0; 4];
        }
        let i = ((ly * self.width + lx) * 4) as usize;
        [
            self.pixels[i],
            self.pixels[i + 1],
            self.pixels[i + 2],
            self.pixels[i + 3],
        ]
    }
}

/// The rectangle a HUD element covers, in render-target pixels, already
/// clipped to the target. Empty (`None`) when the element falls entirely
/// outside.
#[derive(Debug, Clone, Copy)]
struct Region {
    x: i64,
    y: i64,
    width: u32,
    height: u32,
}

impl Region {
    /// Clip `x0..x1 × y0..y1` to a `width × height` target.
    fn clipped(x0: i64, y0: i64, x1: i64, y1: i64, width: u32, height: u32) -> Option<Self> {
        let (x0, y0) = (x0.max(0), y0.max(0));
        let (x1, y1) = (x1.min(width as i64), y1.min(height as i64));
        (x1 > x0 && y1 > y0).then_some(Self {
            x: x0,
            y: y0,
            width: (x1 - x0) as u32,
            height: (y1 - y0) as u32,
        })
    }

    fn union(a: Option<Self>, b: Option<Self>) -> Option<Self> {
        match (a, b) {
            (Some(a), Some(b)) => {
                let (x0, y0) = (a.x.min(b.x), a.y.min(b.y));
                let x1 = (a.x + a.width as i64).max(b.x + b.width as i64);
                let y1 = (a.y + a.height as i64).max(b.y + b.height as i64);
                Some(Self {
                    x: x0,
                    y: y0,
                    width: (x1 - x0) as u32,
                    height: (y1 - y0) as u32,
                })
            }
            (some, None) | (None, some) => some,
        }
    }

    /// Whether two regions share a pixel — the test that decides which
    /// elements must composite on one canvas.
    fn intersects(&self, other: Self) -> bool {
        self.x < other.x + other.width as i64
            && other.x < self.x + self.width as i64
            && self.y < other.y + other.height as i64
            && other.y < self.y + self.height as i64
    }

    /// Index of a target pixel in a region-sized buffer, or `None` when the
    /// pixel lies outside. Every element's own pixels lie inside the union
    /// region by construction, so this rejects exactly what the target-bounds
    /// check used to.
    #[inline(always)]
    fn index(&self, x: i64, y: i64) -> Option<usize> {
        let (lx, ly) = (x - self.x, y - self.y);
        (lx >= 0 && ly >= 0 && lx < self.width as i64 && ly < self.height as i64)
            .then(|| ly as usize * self.width as usize + lx as usize)
    }

    /// The pixels of `x0..x1 × y0..y1` that land on both the target and this
    /// region, as `(rows, columns)`.
    ///
    /// Exactly the pixels — in exactly the order — that testing every pixel of
    /// the box against [`index`](Self::index) accepted, found with one
    /// intersection instead of one `Option` per pixel. Everything the result
    /// covers is inside the region by construction, so a caller addresses it
    /// from [`row_base`](Self::row_base) and never re-derives the bound.
    #[inline(always)]
    fn spans(
        &self,
        x0: i64,
        y0: i64,
        x1: i64,
        y1: i64,
        width: u32,
        height: u32,
    ) -> Option<(std::ops::Range<i64>, std::ops::Range<i64>)> {
        let x0 = x0.max(0).max(self.x);
        let y0 = y0.max(0).max(self.y);
        let x1 = x1.min(width as i64).min(self.x + self.width as i64);
        let y1 = y1.min(height as i64).min(self.y + self.height as i64);
        (x1 > x0 && y1 > y0).then_some((y0..y1, x0..x1))
    }

    /// Index of the first pixel of target row `y` in a region-sized buffer.
    /// Meaningful only for a row this region covers — which is what
    /// [`spans`](Self::spans) returns.
    #[inline(always)]
    fn row_base(&self, y: i64) -> usize {
        (y - self.y) as usize * self.width as usize
    }
}

/// One HUD element, identified so a cluster can redraw exactly its own
/// members in the overall draw order.
///
/// Since M31 the draw order *is* `UiLayout::placed`, so an element is just an
/// index into it — the rasterizer no longer decides who draws over whom, and
/// no longer computes where anything is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Element {
    /// Index into [`UiLayout::placed`].
    Placed(usize),
    /// The script debug-line panel, which is always drawn last.
    Lines,
}

/// A rasterized overlay: the canvases the HUD's content actually needs.
///
/// Elements that overlap share a canvas and composite there in linear space,
/// exactly as they always have; elements that do not overlap get their own,
/// so a gauge in one corner and a readout in another cost two small canvases
/// instead of one screen-sized one. Canvases never disagree about a pixel —
/// two elements covering the same pixel necessarily overlap and are therefore
/// on the same canvas — so compositing them in any order gives the same
/// frame, and pixels no canvas covers are left exactly as the scene drew
/// them.
#[derive(Default)]
pub struct HudOverlay {
    pub canvases: Vec<HudCanvas>,
}

impl HudOverlay {
    /// Nothing to composite.
    pub fn is_empty(&self) -> bool {
        self.canvases.iter().all(HudCanvas::is_empty)
    }

    /// RGBA at a render-target pixel, transparent where no canvas covers it.
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        self.canvases
            .iter()
            .map(|canvas| canvas.pixel(x, y))
            .find(|pixel| pixel[3] != 0)
            .unwrap_or([0; 4])
    }

    /// Total pixels rasterized — how much work the overlay cost, which is the
    /// property the region split exists to keep small.
    pub fn covered_pixels(&self) -> usize {
        self.canvases
            .iter()
            .map(|canvas| canvas.width as usize * canvas.height as usize)
            .sum()
    }
}

/// Rasterize the overlay, laying it out first.
///
/// Draw order is [`ui::layout`]'s: depth-first over the `parent` tree, a panel
/// before its children, siblings by `(class, file order)` — which collapses to
/// M12's "rects then texts, each in file order" for any scene where nothing
/// names a parent. The script debug-line panel still draws over everything.
pub fn rasterize(hud: &HudTree, lines: &[String], width: u32, height: u32) -> HudOverlay {
    let layout = ui::layout(hud, width, height);
    rasterize_laid_out(hud, &layout, lines, width, height)
}

/// Rasterize against a layout the caller already computed — the path the
/// simulation loop takes, since it needs the same rectangles for hit-testing
/// and recomputing them could only introduce a disagreement.
pub fn rasterize_laid_out(
    hud: &HudTree,
    layout: &UiLayout,
    lines: &[String],
    width: u32,
    height: u32,
) -> HudOverlay {
    // Measure first: layout has already resolved every pixel box, so all that
    // is left is clipping them to the target.
    let mut placed: Vec<(Element, Region)> = Vec::new();
    let mut place = |element, bounds: Option<Region>| {
        if let Some(bounds) = bounds {
            placed.push((element, bounds));
        }
    };
    for (index, element) in layout.placed.iter().enumerate() {
        if !element.visible {
            continue;
        }
        place(
            Element::Placed(index),
            element_bounds(&hud.nodes[element.node].kind, element.rect, width, height),
        );
    }
    place(Element::Lines, lines_bounds(lines, width, height));

    HudOverlay {
        canvases: cluster(&placed)
            .into_iter()
            .map(|(region, members)| {
                draw_cluster(hud, layout, lines, width, height, region, &members)
            })
            .collect(),
    }
}

/// Group elements that overlap, and return each group with its bounding
/// region. Elements only need to share a canvas when they can touch the same
/// pixel, and merging is transitive — an element bridging two groups joins
/// them — so the result is the coarsest grouping that still composites every
/// overlap exactly.
fn cluster(placed: &[(Element, Region)]) -> Vec<(Region, Vec<Element>)> {
    let mut clusters: Vec<(Region, Vec<Element>)> = Vec::new();
    for &(element, bounds) in placed {
        // Absorb every existing cluster this element reaches, then land as
        // one cluster with all of them.
        let mut region = bounds;
        let mut members = vec![element];
        let mut index = 0;
        while index < clusters.len() {
            if clusters[index].0.intersects(region) {
                let (other_region, other_members) = clusters.remove(index);
                region = Region::union(Some(region), Some(other_region)).expect("both present");
                members.extend(other_members);
                // A merged region is larger and may now reach clusters that
                // were checked already, so start over.
                index = 0;
            } else {
                index += 1;
            }
        }
        clusters.push((region, members));
    }

    // Draw order within a cluster is the order elements were placed in — which
    // is the layout's order, with the debug lines last.
    for (_, members) in &mut clusters {
        members.sort_by_key(|element| match element {
            Element::Placed(index) => (0, *index),
            Element::Lines => (1, 0),
        });
    }
    clusters
}

/// Rasterize one cluster's members into a canvas covering just its region.
fn draw_cluster(
    hud: &HudTree,
    layout: &UiLayout,
    lines: &[String],
    width: u32,
    height: u32,
    region: Region,
    members: &[Element],
) -> HudCanvas {
    // Linear-space straight-alpha accumulation; encoded once at the end, so
    // stacked translucent elements quantize exactly once however many there
    // are.
    let mut canvas = vec![[0.0f32; 4]; region.width as usize * region.height as usize];

    for member in members {
        match member {
            Element::Placed(index) => {
                let element = &layout.placed[*index];
                draw_element(
                    &mut canvas,
                    region,
                    width,
                    height,
                    &hud.nodes[element.node].kind,
                    element.rect,
                );
            }
            Element::Lines => draw_lines(&mut canvas, region, width, height, lines),
        }
    }

    // Runs of one value are what a HUD canvas is made of — a flat panel band,
    // the inside of a glyph, the transparent margin no element reached — and
    // the encode is three `powf`s a pixel. Carrying the last pixel's bytes and
    // reusing them when this pixel is *bit*-identical is exact by construction
    // (the encode is a pure function of the four floats) and is most of what
    // this loop used to cost.
    // Sized once and written by index rather than grown four bytes at a time:
    // the output length is known exactly, and a per-pixel `extend_from_slice`
    // spends more on its capacity check than the encode it is storing.
    let mut pixels = vec![0u8; canvas.len() * 4];
    let mut carried: Option<([u32; 4], [u8; 4])> = None;
    for (pixel, out) in canvas.iter().zip(pixels.chunks_exact_mut(4)) {
        let bits = [
            pixel[0].to_bits(),
            pixel[1].to_bits(),
            pixel[2].to_bits(),
            pixel[3].to_bits(),
        ];
        let encoded = match carried {
            Some((seen, bytes)) if seen == bits => bytes,
            _ => {
                let bytes = [
                    encode_srgb(pixel[0]),
                    encode_srgb(pixel[1]),
                    encode_srgb(pixel[2]),
                    (pixel[3].clamp(0.0, 1.0) * 255.0).round() as u8,
                ];
                carried = Some((bits, bytes));
                bytes
            }
        };
        out.copy_from_slice(&encoded);
    }

    HudCanvas {
        origin_x: region.x as u32,
        origin_y: region.y as u32,
        width: region.width,
        height: region.height,
        pixels,
    }
}

/// The pixel box an element covers, clipped to the target.
///
/// A fully transparent panel is skipped outright — it produces no canvas — so
/// an invisible layout group costs exactly what it did before it existed. That
/// is what lets a scene be restructured into panels without any of its pixels
/// or its canvas clustering changing.
fn element_bounds(kind: &HudKind, rect: PixelRect, width: u32, height: u32) -> Option<Region> {
    if let HudKind::Panel(panel) = kind {
        if panel.opacity <= 0.0 {
            return None;
        }
    }
    Region::clipped(
        rect.x,
        rect.y,
        rect.x + rect.width as i64,
        rect.y + rect.height as i64,
        width,
        height,
    )
}

fn draw_element(
    canvas: &mut [[f32; 4]],
    region: Region,
    width: u32,
    height: u32,
    kind: &HudKind,
    rect: PixelRect,
) {
    match kind {
        HudKind::Panel(panel) => draw_panel(canvas, region, width, height, panel, rect),
        HudKind::Rect(r) => draw_rect(canvas, region, width, height, r, rect),
        HudKind::Image(image, texture) => {
            draw_image(canvas, region, width, height, image, texture.as_ref(), rect)
        }
        HudKind::Text(text) => draw_text(canvas, region, width, height, text, rect),
    }
}

/// Fill a laid-out box with a flat colour — the one primitive behind both
/// `HudRect` and a `HudPanel`'s background.
fn fill(
    canvas: &mut [[f32; 4]],
    region: Region,
    width: u32,
    height: u32,
    rect: PixelRect,
    color: glam::Vec3,
    opacity: f32,
) {
    let color = [color.x, color.y, color.z];
    let alpha = opacity.clamp(0.0, 1.0);
    let (x1, y1) = (rect.x + rect.width as i64, rect.y + rect.height as i64);
    let Some((rows, columns)) = region.spans(rect.x, rect.y, x1, y1, width, height) else {
        return;
    };
    for y in rows {
        let base = region.row_base(y);
        for x in columns.clone() {
            blend(&mut canvas[base + (x - region.x) as usize], color, alpha);
        }
    }
}

fn draw_rect(
    canvas: &mut [[f32; 4]],
    region: Region,
    width: u32,
    height: u32,
    rect: &HudRect,
    box2: PixelRect,
) {
    fill(
        canvas,
        region,
        width,
        height,
        box2,
        rect.color,
        rect.opacity,
    );
}

fn draw_panel(
    canvas: &mut [[f32; 4]],
    region: Region,
    width: u32,
    height: u32,
    panel: &HudPanel,
    box2: PixelRect,
) {
    fill(
        canvas,
        region,
        width,
        height,
        box2,
        panel.color,
        panel.opacity,
    );
}

/// Where a destination coordinate reads from in the source, under nine-slice.
///
/// The three bands are: a `start`-wide corner copied 1:1, a middle that
/// **tiles**, and an `end`-wide corner copied 1:1 from the source's far edge.
/// Tiling rather than stretching, because tiling at nearest is exact and
/// stretching at nearest is a moiré pattern.
///
/// When the destination is too small to hold both corners they are shrunk in
/// proportion (integer arithmetic, left rounding down) rather than overlapping
/// — a 12-pixel frame drawn into an 8-pixel box still reads as a frame.
#[inline(always)]
fn slice_source(destination: i64, dest_size: u32, source_size: u32, start: u32, end: u32) -> u32 {
    if source_size == 0 {
        return 0;
    }
    let (start, end) = (start.min(source_size), end.min(source_size));
    let (start, end) = if start + end <= dest_size {
        (start, end)
    } else {
        match (dest_size * start).checked_div(start + end) {
            Some(scaled) => (scaled, dest_size - scaled),
            // Both insets are zero, so there are no corners to shrink.
            None => (0, 0),
        }
    };

    let d = destination.clamp(0, dest_size.saturating_sub(1) as i64) as u32;
    if d < start {
        return d;
    }
    if d >= dest_size.saturating_sub(end) {
        return source_size - (dest_size - d);
    }
    // The tiling middle. With no middle band in the source (the insets meet),
    // clamp to the last texel of the start band rather than dividing by zero.
    let middle = source_size.saturating_sub(start + end);
    if middle == 0 {
        return start.min(source_size - 1);
    }
    start + (d - start) % middle
}

/// Draw a textured rectangle, sampled **nearest-neighbour** — the filter is a
/// format contract (a render sits under a baseline), and nearest is exactly
/// reproducible where a bilinear filter is a float-rounding question.
///
/// Only the base mip is read: the overlay never minifies below one destination
/// pixel per texel band, so a level selection would have no correct answer.
fn draw_image(
    canvas: &mut [[f32; 4]],
    region: Region,
    width: u32,
    height: u32,
    image: &HudImage,
    texture: Option<&Arc<TextureData>>,
    rect: PixelRect,
) {
    // No asset directory (a GPU-less test) or an unreadable file: lay out,
    // draw nothing. The reference itself was checked by `validate`.
    let Some(texture) = texture else {
        return;
    };
    if texture.width == 0 || texture.height == 0 || rect.width == 0 || rect.height == 0 {
        return;
    }
    let pixels = texture.rgba();
    let inset = |v: f32| v.max(0.0) as u32;
    let (left, top) = (inset(image.slice[0]), inset(image.slice[1]));
    let (right, bottom) = (inset(image.slice[2]), inset(image.slice[3]));
    let tint = [image.tint.x, image.tint.y, image.tint.z];
    let opacity = image.opacity.clamp(0.0, 1.0);

    let (x1, y1) = (rect.x + rect.width as i64, rect.y + rect.height as i64);
    let Some((rows, columns)) = region.spans(rect.x, rect.y, x1, y1, width, height) else {
        return;
    };
    // The source column a destination column reads from does not depend on the
    // row, so it is resolved once per column rather than once per pixel — the
    // nine-slice band arithmetic was running for every texel of a card frame
    // that has the same few hundred columns all the way down.
    let sources: Vec<u32> = columns
        .clone()
        .map(|x| slice_source(x - rect.x, rect.width, texture.width, left, right))
        .collect();

    for y in rows {
        let sy = slice_source(y - rect.y, rect.height, texture.height, top, bottom);
        let base = region.row_base(y);
        for (x, &sx) in columns.clone().zip(&sources) {
            let texel = ((sy * texture.width + sx) * 4) as usize;
            let alpha = pixels[texel + 3] as f32 / 255.0 * opacity;
            if alpha <= 0.0 {
                continue;
            }
            // The texture is sRGB-encoded bytes; decode so the tint multiplies
            // in linear space like every other colour in the engine.
            let color = [
                decode_srgb(pixels[texel]) * tint[0],
                decode_srgb(pixels[texel + 1]) * tint[1],
                decode_srgb(pixels[texel + 2]) * tint[2],
            ];
            blend(&mut canvas[base + (x - region.x) as usize], color, alpha);
        }
    }
}

fn draw_text(
    canvas: &mut [[f32; 4]],
    region: Region,
    width: u32,
    height: u32,
    text: &HudText,
    box2: PixelRect,
) {
    let scale = text_scale(text.size);
    let cell = (GLYPH * scale) as i64;
    let color = [text.color.x, text.color.y, text.color.z];
    let lines = ui::text_lines(text);
    let step = cell as f32 + text.line_gap;

    for (row, line) in lines.iter().enumerate() {
        let line_width = (line.chars().count() as i64 * cell) as f32;
        // Both offsets are zero for a single unwrapped left-aligned line — the
        // M12 case — so its glyphs land exactly where they always have.
        let x0 = box2.x + ui::line_offset(text.align, box2.width as f32, line_width).round() as i64;
        let y0 = box2.y + (row as f32 * step).round() as i64;

        for (index, ch) in line.chars().enumerate() {
            // Outside the font's ASCII coverage: a filled box — visibly wrong
            // in the screenshot, never a panic (M11.6's design, §2).
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
                            if let Some(i) = region.index(x, y) {
                                blend(&mut canvas[i], color, 1.0);
                            }
                        }
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
/// Panel geometry, shared by the bounds pass and the draw pass so the two can
/// never disagree about where the panel is. `None` when there is nothing to
/// draw.
fn panel_box(lines: &[String]) -> Option<(u32, u32)> {
    let longest = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    if lines.is_empty() || longest == 0 {
        return None;
    }
    let cell = GLYPH * SCALE;
    Some((
        longest as u32 * cell + 2 * PAD,
        lines.len() as u32 * cell + (lines.len() as u32 - 1) * LINE_GAP + 2 * PAD,
    ))
}

/// The pixel box the debug-line panel covers. The panel itself is clamped
/// against the frame edge when drawn, but its glyphs clip against the frame
/// rather than the clamped panel, so the *unclamped* box is what bounds them
/// both.
fn lines_bounds(lines: &[String], width: u32, height: u32) -> Option<Region> {
    let (panel_width, panel_height) = panel_box(lines)?;
    Region::clipped(
        MARGIN as i64,
        MARGIN as i64,
        (MARGIN + panel_width) as i64,
        (MARGIN + panel_height) as i64,
        width,
        height,
    )
}

fn draw_lines(canvas: &mut [[f32; 4]], region: Region, width: u32, height: u32, lines: &[String]) {
    let Some((panel_width, panel_height)) = panel_box(lines) else {
        return;
    };
    let cell = GLYPH * SCALE;

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
            if let Some(i) = region.index((MARGIN + x) as i64, (MARGIN + y) as i64) {
                blend(&mut canvas[i], panel_color, panel_alpha);
            }
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
                            if let Some(i) = region.index(px as i64, py as i64) {
                                blend(&mut canvas[i], text_color, 1.0);
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Straight-alpha "over" in linear space.
#[inline(always)]
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
#[inline(always)]
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
///
/// Its whole domain is a byte, so [`DECODED`] *is* the function rather than an
/// approximation of it: 256 entries, each the expression below evaluated on
/// the same input. `draw_image` calls this three times per texel it samples,
/// which on a nine-sliced card is the second `powf` per pixel in the frame.
#[inline(always)]
fn decode_srgb(byte: u8) -> f32 {
    DECODED[byte as usize]
}

/// [`decode_srgb`] evaluated on all 256 of its possible inputs.
static DECODED: std::sync::LazyLock<[f32; 256]> = std::sync::LazyLock::new(|| {
    std::array::from_fn(|byte| {
        let encoded = byte as f32 / 255.0;
        if encoded <= 0.040_45 {
            encoded / 12.92
        } else {
            ((encoded + 0.055) / 1.055).powf(2.4)
        }
    })
});

#[cfg(test)]
mod tests {
    use super::*;
    use engine_core::components::{HudAnchor, HudTextAlign};
    use engine_core::ui::HudNode;
    use glam::{Vec2, Vec3};

    fn rect(anchor: HudAnchor, offset: [f32; 2], size: [f32; 2]) -> HudRect {
        HudRect {
            anchor,
            offset: Vec2::from(offset),
            size: Vec2::from(size),
            color: Vec3::ONE,
            opacity: 1.0,
            parent: None,
            visible: true,
            stretch: [false, false],
        }
    }

    fn text(s: &str, anchor: HudAnchor, offset: [f32; 2], size: f32) -> HudText {
        HudText {
            text: s.into(),
            anchor,
            offset: Vec2::from(offset),
            size,
            color: Vec3::ONE,
            parent: None,
            visible: true,
            stretch: [false, false],
            align: HudTextAlign::Left,
            wrap: 0.0,
            line_gap: 0.0,
        }
    }

    fn only_rects(rects: Vec<HudRect>) -> HudTree {
        items(rects, vec![])
    }

    /// The M12 shape as a tree: two lists, nothing naming a parent. This is
    /// exactly the arrangement the draw-order collapse is about, which is why
    /// these tests still read the same and still assert the same pixels.
    fn items(rects: Vec<HudRect>, texts: Vec<HudText>) -> HudTree {
        HudTree {
            nodes: rects
                .into_iter()
                .map(HudKind::Rect)
                .chain(texts.into_iter().map(HudKind::Text))
                .enumerate()
                .map(|(i, kind)| HudNode {
                    entity: format!("E{i}"),
                    kind,
                    interact: None,
                })
                .collect(),
        }
    }

    fn components(hud: &HudTree, width: u32, height: u32) -> HudOverlay {
        rasterize(hud, &[], width, height)
    }

    /// Canvases cover only the region the HUD touches, so tests address them
    /// in render-target coordinates like everything else that reasons about
    /// the screen.
    fn pixel_at(canvas: &HudOverlay, x: u32, y: u32) -> [u8; 4] {
        canvas.pixel(x, y)
    }

    fn alpha_at(canvas: &HudOverlay, x: u32, y: u32) -> u8 {
        pixel_at(canvas, x, y)[3]
    }

    /// Lit pixels in render-target coordinates, scanning the whole target.
    fn covered_in(canvas: &HudOverlay, width: u32, height: u32) -> Vec<(u32, u32)> {
        let mut on = Vec::new();
        for y in 0..height {
            for x in 0..width {
                if alpha_at(canvas, x, y) > 0 {
                    on.push((x, y));
                }
            }
        }
        on
    }

    #[test]
    fn empty_hud_is_fully_transparent() {
        let canvas = components(&HudTree::default(), 8, 8);
        assert!(canvas.is_empty(), "an empty HUD rasterizes nothing at all");
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
            let canvas = components(
                &only_rects(vec![rect(anchor, [1.0, 1.0], [2.0, 2.0])]),
                10,
                10,
            );
            let expected: Vec<(u32, u32)> =
                vec![(ex, ey), (ex + 1, ey), (ex, ey + 1), (ex + 1, ey + 1)];
            assert_eq!(covered_in(&canvas, 10, 10), expected, "{anchor:?}");
        }
    }

    #[test]
    fn center_anchor_centers_the_element() {
        let canvas = components(
            &only_rects(vec![rect(HudAnchor::Center, [0.0, 0.0], [2.0, 2.0])]),
            10,
            10,
        );
        assert_eq!(
            covered_in(&canvas, 10, 10),
            vec![(4, 4), (5, 4), (4, 5), (5, 5)]
        );
    }

    #[test]
    fn rects_clip_at_canvas_edges_instead_of_panicking() {
        let canvas = components(
            &only_rects(vec![rect(HudAnchor::TopLeft, [-3.0, -3.0], [100.0, 100.0])]),
            4,
            4,
        );
        assert_eq!(
            covered_in(&canvas, 4, 4).len(),
            16,
            "clipped rect covers the whole canvas"
        );
    }

    #[test]
    fn opaque_rect_color_is_srgb_encoded_exactly() {
        // Linear 0.5 encodes to 188 — the same expectation the lighting pixel
        // tests compute. Alpha stays linear (255).
        let mut r = rect(HudAnchor::TopLeft, [0.0, 0.0], [1.0, 1.0]);
        r.color = Vec3::splat(0.5);
        let canvas = components(&only_rects(vec![r]), 1, 1);
        assert_eq!(&canvas.canvases[0].pixels, &[188, 188, 188, 255]);
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
        assert_eq!(
            &canvas.canvases[0].pixels,
            &[0, 255, 0, 255],
            "file order is z-order"
        );

        // A '█'-free check that text wins over rects: the fallback box glyph
        // is fully filled, so every pixel of its cell takes the text color.
        let mut blue = text("\u{2588}", HudAnchor::TopLeft, [0.0, 0.0], 8.0);
        blue.color = Vec3::new(0.0, 0.0, 1.0);
        let mut red_rect = rect(HudAnchor::TopLeft, [0.0, 0.0], [8.0, 8.0]);
        red_rect.color = Vec3::new(1.0, 0.0, 0.0);
        let canvas = components(&items(vec![red_rect], vec![blue]), 8, 8);
        assert_eq!(&canvas.canvases[0].pixels[0..4], &[0, 0, 255, 255]);
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
            &items(vec![], vec![text("L", HudAnchor::TopLeft, [0.0, 0.0], 8.0)]),
            8,
            8,
        );
        let on = covered_in(&l, 8, 8);
        let left = on.iter().filter(|(x, _)| *x < 4).count();
        let right = on.len() - left;
        assert!(left > right, "'L' should be left-heavy: {on:?}");
        let bottom_row_lit = on.iter().filter(|(_, y)| *y >= 5).count();
        assert!(bottom_row_lit >= 3, "'L' has a bottom bar: {on:?}");

        let dot = components(
            &items(vec![], vec![text(".", HudAnchor::TopLeft, [0.0, 0.0], 8.0)]),
            8,
            8,
        );
        assert!(
            covered_in(&dot, 8, 8).iter().all(|(_, y)| *y >= 4),
            "'.' sits in the bottom half: {:?}",
            covered_in(&dot, 8, 8)
        );
    }

    #[test]
    fn text_width_scales_with_glyph_count_and_scale() {
        // Right-anchored text must measure itself to sit flush: two glyphs at
        // 2× are 32px wide, so offset 0 puts the last column at the edge.
        let canvas = components(
            &items(
                vec![],
                vec![text(
                    "\u{2588}\u{2588}",
                    HudAnchor::TopRight,
                    [0.0, 0.0],
                    16.0,
                )],
            ),
            64,
            16,
        );
        let on = covered_in(&canvas, 64, 16);
        assert!(on.contains(&(63, 0)), "flush against the right edge");
        assert!(on.contains(&(32, 0)), "extends 32px left of the edge");
        assert!(!on.contains(&(31, 0)), "and no further");
    }

    #[test]
    fn unknown_glyphs_render_as_a_filled_box() {
        let canvas = components(
            &items(
                vec![],
                vec![text("\u{2588}", HudAnchor::TopLeft, [0.0, 0.0], 8.0)],
            ),
            8,
            8,
        );
        assert_eq!(covered_in(&canvas, 8, 8).len(), 64);
    }

    // ---- Script debug-line panel (the M11.6 layer) ----

    fn lines_only(lines: &[&str], width: u32, height: u32) -> HudOverlay {
        let lines: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
        rasterize(&HudTree::default(), &lines, width, height)
    }

    #[test]
    fn empty_lines_draw_nothing() {
        let canvas = lines_only(&[], 64, 64);
        assert!(canvas.is_empty());
        let blank = lines_only(&[""], 64, 64);
        assert!(blank.is_empty());
    }

    #[test]
    fn panel_size_follows_the_text_block() {
        // 4 chars * 16px + 2*8 padding = 80 wide; 2 lines * 16 + 1 gap * 6 +
        // 16 = 54 tall, at MARGIN from the top-left corner.
        let canvas = lines_only(&["ABCD", "A"], 200, 100);
        assert_eq!(pixel_at(&canvas, MARGIN, MARGIN), PANEL);
        assert_eq!(pixel_at(&canvas, MARGIN + 79, MARGIN + 53), PANEL);
        assert_eq!(
            alpha_at(&canvas, MARGIN - 1, MARGIN),
            0,
            "left of the panel"
        );
        assert_eq!(
            alpha_at(&canvas, MARGIN + 80, MARGIN),
            0,
            "right of the panel"
        );
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

    // ---- Canvas regions (M15) ----

    #[test]
    fn separated_elements_rasterize_as_separate_small_canvases() {
        // A gauge in one corner and a readout in another must not drag the
        // whole screen through the rasterizer between them.
        let overlay = rasterize(
            &only_rects(vec![
                rect(HudAnchor::TopLeft, [4.0, 4.0], [20.0, 10.0]),
                rect(HudAnchor::BottomRight, [4.0, 4.0], [20.0, 10.0]),
            ]),
            &[],
            800,
            600,
        );
        assert_eq!(overlay.canvases.len(), 2);
        assert_eq!(overlay.covered_pixels(), 2 * 20 * 10);
        // And they still land where the anchors say.
        assert_eq!(overlay.pixel(4, 4)[3], 255);
        assert_eq!(overlay.pixel(800 - 5, 600 - 5)[3], 255);
        assert_eq!(overlay.pixel(400, 300)[3], 0, "nothing in between");
    }

    #[test]
    fn overlapping_elements_share_one_canvas_and_composite_there() {
        // Translucent stacking must quantize once, at the end, however many
        // layers there are — which only holds if they accumulate together.
        let mut under = rect(HudAnchor::TopLeft, [0.0, 0.0], [10.0, 10.0]);
        under.color = Vec3::new(1.0, 0.0, 0.0);
        under.opacity = 0.5;
        let mut over = rect(HudAnchor::TopLeft, [5.0, 5.0], [10.0, 10.0]);
        over.color = Vec3::new(0.0, 0.0, 1.0);
        over.opacity = 0.5;

        let overlay = rasterize(&only_rects(vec![under, over]), &[], 100, 100);
        assert_eq!(overlay.canvases.len(), 1, "overlap means one canvas");
        // In the shared pixel the blue is over the red; outside it each keeps
        // its own color.
        let shared = overlay.pixel(7, 7);
        assert!(shared[2] > shared[0], "blue over red: {shared:?}");
        assert!(overlay.pixel(2, 2)[0] > 0 && overlay.pixel(2, 2)[2] == 0);
        assert!(overlay.pixel(12, 12)[2] > 0 && overlay.pixel(12, 12)[0] == 0);
    }

    #[test]
    fn a_chain_of_overlaps_merges_transitively() {
        // A bridges B and C, so all three must end up on one canvas even
        // though B and C never touch.
        let left = rect(HudAnchor::TopLeft, [0.0, 0.0], [10.0, 10.0]);
        let right = rect(HudAnchor::TopLeft, [18.0, 0.0], [10.0, 10.0]);
        let bridge = rect(HudAnchor::TopLeft, [8.0, 0.0], [12.0, 10.0]);
        // Placed so the bridge arrives last, after the two are separate
        // clusters — the case a single pass over the list would miss.
        let overlay = rasterize(&only_rects(vec![left, right, bridge]), &[], 100, 100);
        assert_eq!(overlay.canvases.len(), 1);
    }

    #[test]
    fn cost_follows_content_not_screen_size() {
        // The property the whole region split exists for: the same HUD on a
        // four-times-larger frame rasterizes the same number of pixels.
        let hud = items(
            vec![rect(HudAnchor::BottomLeft, [10.0, 10.0], [120.0, 8.0])],
            vec![text("SPEED 42", HudAnchor::TopRight, [10.0, 10.0], 16.0)],
        );
        let small = rasterize(&hud, &["LAP 3".into()], 640, 360);
        let large = rasterize(&hud, &["LAP 3".into()], 2560, 1440);
        assert_eq!(small.covered_pixels(), large.covered_pixels());
        assert!(
            large.covered_pixels() < 8_000,
            "a three-element HUD should cost a few thousand pixels, not \
             millions: {}",
            large.covered_pixels()
        );
    }

    #[test]
    fn an_element_entirely_off_screen_costs_nothing() {
        let overlay = rasterize(
            &only_rects(vec![rect(HudAnchor::TopLeft, [-50.0, -50.0], [10.0, 10.0])]),
            &[],
            100,
            100,
        );
        assert!(overlay.is_empty());
    }

    #[test]
    fn rasterization_is_deterministic() {
        let a = lines_only(&["SPEED 42 KM/H"], 320, 200);
        let b = lines_only(&["SPEED 42 KM/H"], 320, 200);
        assert_eq!(a.canvases[0].pixels, b.canvases[0].pixels);
    }

    /// The decode table *is* the transfer function, not an approximation of
    /// it: same expression, same 256 inputs, and therefore the same bits. A
    /// table that merely rounded to the same byte would still shift a blend,
    /// because the value is multiplied by a tint before it is quantized.
    #[test]
    fn the_decode_table_is_the_transfer_function() {
        for byte in 0..=u8::MAX {
            let encoded = byte as f32 / 255.0;
            let expected = if encoded <= 0.040_45 {
                encoded / 12.92
            } else {
                ((encoded + 0.055) / 1.055).powf(2.4)
            };
            assert_eq!(
                decode_srgb(byte).to_bits(),
                expected.to_bits(),
                "decode table disagrees at {byte}"
            );
        }
    }

    /// The encode carries the previous pixel's bytes across a run of identical
    /// pixels. A run is only ever entered on a *bit*-identical pixel, so this
    /// has to hold for a canvas that alternates — the case where the carry is
    /// wrong would show as a colour bleeding one pixel to its right.
    #[test]
    fn carrying_the_encode_across_a_run_matches_encoding_every_pixel() {
        let mut nodes = Vec::new();
        for (index, x) in (0..24).step_by(2).enumerate() {
            let mut bar = rect(HudAnchor::TopLeft, [x as f32, 0.0], [2.0, 6.0]);
            // Alternating colours, so no two neighbouring columns are equal,
            // and one repeated colour so runs happen too.
            bar.color = if index % 2 == 0 {
                Vec3::new(0.2, 0.6, 0.9)
            } else {
                Vec3::new(0.9, 0.1, 0.4)
            };
            nodes.push(bar);
        }
        let overlay = rasterize(&only_rects(nodes), &[], 64, 16);
        let canvas = &overlay.canvases[0];

        for y in 0..canvas.height {
            for x in 0..canvas.width {
                let i = ((y * canvas.width + x) * 4) as usize;
                let opaque = canvas.pixels[i + 3] == 255;
                // Every drawn pixel is one of the two authored colours, so a
                // carry that leaked would show up as a third.
                if opaque {
                    let rgb = [canvas.pixels[i], canvas.pixels[i + 1], canvas.pixels[i + 2]];
                    let a = [
                        encode_srgb(0.2),
                        encode_srgb(0.6),
                        encode_srgb(0.9),
                    ];
                    let b = [
                        encode_srgb(0.9),
                        encode_srgb(0.1),
                        encode_srgb(0.4),
                    ];
                    assert!(rgb == a || rgb == b, "unexpected colour at ({x}, {y}): {rgb:?}");
                }
            }
        }
    }
}
