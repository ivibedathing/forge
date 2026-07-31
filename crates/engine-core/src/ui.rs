//! Screen-space UI layout (M31, `designs/ui-system-design.md`).
//!
//! M12 placed every HUD element by an anchor and a pixel offset it had solved
//! by hand. This module is the answer to the three things that made a *screen*
//! unbuildable that way: a hierarchy, so a dialog's parts move together; hug
//! sizing, so a box follows its text instead of the text being fitted to a box;
//! and a published rectangle, so an agent that cannot see the menu can still
//! click it.
//!
//! **Layout is a pure function of (tree, width, height).** No incremental
//! layout, no dirty flags, no measurement cache that could go stale — which is
//! what lets `engine ui-layout` print the same rectangles the renderer draws
//! and the hit test uses. Like `hud.rs` and `diff.rs` it is GPU-free, so all of
//! it is unit-tested on machines with no adapter.
//!
//! # The compatibility rule
//!
//! Every pre-M31 scene must lay out **identically**, not merely closely. Two
//! properties give that by construction rather than by measurement:
//!
//! - An element with no `parent` is a child of the viewport placed by
//!   [`anchored`], which is the M12 arithmetic verbatim — same expression, same
//!   rounding, same order of operations.
//! - The draw order (depth-first, a panel before its children, siblings by
//!   `(class, file order)`) **collapses** to the M12 rule when nothing names a
//!   parent: every element is then a root sibling, so the order is exactly
//!   "rects, images and panels in file order, then texts in file order". There
//!   is no arrangement of pre-M31 components the two rules sort differently.

use std::sync::Arc;

use glam::{Vec2, Vec3};

use crate::components::{
    HudAlign, HudAnchor, HudImage, HudInteract, HudLayout, HudPanel, HudRect, HudText, HudTextAlign,
};
use crate::texture::TextureData;

/// Glyph cell edge in font units; the built-in font is fixed 8×8.
pub const GLYPH: u32 = 8;

/// How deep a `parent` chain may nest. Beyond this is `hud_nesting_too_deep`,
/// a *validation* error rather than a runtime guard — a hung layout with no
/// output is the worst failure an agent loop can hit, which is the argument
/// `tree_too_complex` makes for trees.
pub const MAX_HUD_DEPTH: usize = 16;

/// The integer scale factor `HudText.size` snaps to: `max(1, round(size / 8))`.
pub fn text_scale(size: f32) -> u32 {
    ((size / GLYPH as f32).round() as i64).max(1) as u32
}

/// Split a label into the lines that will actually be drawn.
///
/// `\n` breaks explicitly; `wrap` (a pixel width) breaks greedily on spaces.
/// **A word longer than `wrap` overflows rather than splitting**, because a
/// mid-word break in a fixed-width font reads as corruption rather than as
/// wrapping.
///
/// At `wrap: 0` — the default, and every pre-M31 scene — a label with no `\n`
/// comes back as exactly itself, one line, byte for byte.
pub fn text_lines(text: &HudText) -> Vec<String> {
    let cell = (GLYPH * text_scale(text.size)) as f32;
    let max_chars = if text.wrap > 0.0 && cell > 0.0 {
        (text.wrap / cell).floor() as usize
    } else {
        0
    };

    let mut lines = Vec::new();
    for paragraph in text.text.split('\n') {
        if max_chars == 0 {
            lines.push(paragraph.to_string());
            continue;
        }
        let mut current = String::new();
        for word in paragraph.split(' ').filter(|w| !w.is_empty()) {
            let candidate = if current.is_empty() {
                word.chars().count()
            } else {
                current.chars().count() + 1 + word.chars().count()
            };
            if !current.is_empty() && candidate > max_chars {
                lines.push(std::mem::take(&mut current));
            }
            if !current.is_empty() {
                current.push(' ');
            }
            current.push_str(word);
        }
        lines.push(current);
    }
    lines
}

/// One element of the overlay, with its component as authored.
///
/// The component is kept whole rather than flattened into shared fields, so
/// there is exactly one source of truth for `parent`, `visible` and `stretch`;
/// the accessors below read them off whichever variant this is.
#[derive(Debug, Clone, PartialEq)]
pub enum HudKind {
    Panel(HudPanel),
    Rect(HudRect),
    /// The image and its decoded pixels, resolved once at extraction. `None`
    /// when this context has no asset directory (a GPU-less test): the element
    /// still lays out, and draws as nothing.
    Image(HudImage, Option<Arc<TextureData>>),
    Text(HudText),
}

impl HudKind {
    pub fn parent(&self) -> Option<&str> {
        match self {
            Self::Panel(p) => p.parent.as_deref(),
            Self::Rect(r) => r.parent.as_deref(),
            Self::Image(i, _) => i.parent.as_deref(),
            Self::Text(t) => t.parent.as_deref(),
        }
    }

    pub fn visible(&self) -> bool {
        match self {
            Self::Panel(p) => p.visible,
            Self::Rect(r) => r.visible,
            Self::Image(i, _) => i.visible,
            Self::Text(t) => t.visible,
        }
    }

    pub fn stretch(&self) -> [bool; 2] {
        match self {
            Self::Panel(p) => p.stretch,
            Self::Rect(r) => r.stretch,
            Self::Image(i, _) => i.stretch,
            Self::Text(t) => t.stretch,
        }
    }

    pub fn anchor(&self) -> HudAnchor {
        match self {
            Self::Panel(p) => p.anchor,
            Self::Rect(r) => r.anchor,
            Self::Image(i, _) => i.anchor,
            Self::Text(t) => t.anchor,
        }
    }

    pub fn offset(&self) -> Vec2 {
        match self {
            Self::Panel(p) => p.offset,
            Self::Rect(r) => r.offset,
            Self::Image(i, _) => i.offset,
            Self::Text(t) => t.offset,
        }
    }

    /// The component's `"type"` name, for reports and errors.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Panel(_) => "HudPanel",
            Self::Rect(_) => "HudRect",
            Self::Image(_, _) => "HudImage",
            Self::Text(_) => "HudText",
        }
    }

    /// Draw class: backgrounds (0) under text (1). This *is* the M12 rule —
    /// "all text draws over all rects" — generalized to siblings, and it is
    /// what makes the new order collapse to the old one.
    pub fn class(&self) -> u8 {
        match self {
            Self::Text(_) => 1,
            _ => 0,
        }
    }
}

/// One HUD element and the entity it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct HudNode {
    /// The source entity's stable name (invariant 4) — how `parent` addresses
    /// it, how the script API reaches it, and what `ui-layout` reports.
    pub entity: String,
    pub kind: HudKind,
    /// The entity's `HudInteract`, if it carries one.
    pub interact: Option<HudInteract>,
}

/// The scene's overlay as plain data, in scene-file order.
///
/// Replaces M12's `HudItems { rects, texts }`: one flat list, because the
/// hierarchy is a `parent` *name* in a flat file (the `Wheel.vehicle`
/// precedent) rather than a nested structure. File order is still what breaks
/// ties among siblings, so the list's order is load-bearing.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HudTree {
    pub nodes: Vec<HudNode>,
}

impl HudTree {
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// The index of the node whose entity has this name, if any.
    pub fn index_of(&self, entity: &str) -> Option<usize> {
        self.nodes.iter().position(|n| n.entity == entity)
    }
}

/// A laid-out rectangle in framebuffer pixels, rounded exactly once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelRect {
    pub x: i64,
    pub y: i64,
    pub width: u32,
    pub height: u32,
}

impl PixelRect {
    /// Does this rectangle cover the pixel containing `(x, y)`?
    pub fn contains(&self, x: f32, y: f32) -> bool {
        let (x, y) = (x.floor() as i64, y.floor() as i64);
        x >= self.x
            && y >= self.y
            && x < self.x + self.width as i64
            && y < self.y + self.height as i64
    }
}

/// An element's resolved placement.
#[derive(Debug, Clone, PartialEq)]
pub struct PlacedElement {
    /// Index into [`HudTree::nodes`].
    pub node: usize,
    pub rect: PixelRect,
    /// False when this element *or any ancestor* is hidden — hiding a panel
    /// hides its whole subtree, which is how one boolean opens and closes a
    /// menu.
    pub visible: bool,
    /// The name of the parent panel, or `None` for a child of the viewport.
    pub parent: Option<String>,
    /// Nesting depth; 0 for a child of the viewport.
    pub depth: usize,
}

/// Every element's placement, in draw order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UiLayout {
    pub viewport: (u32, u32),
    /// Depth-first: a panel immediately before its children, siblings ordered
    /// by `(class, file order)`. Hidden elements are present — `ui-layout`
    /// reports where a hidden menu would be — and carry `visible: false`.
    pub placed: Vec<PlacedElement>,
}

impl UiLayout {
    /// The placement of a named element, if it has one.
    pub fn of<'a>(&'a self, tree: &HudTree, entity: &str) -> Option<&'a PlacedElement> {
        let index = tree.index_of(entity)?;
        self.placed.iter().find(|p| p.node == index)
    }

    /// The topmost visible, enabled, interactive element under a cursor in
    /// framebuffer pixels.
    ///
    /// **Last in draw order wins**, so a modal panel carrying a `HudInteract`
    /// swallows clicks to what is under it while one without is click-through
    /// — which makes "does this menu block the game" an authored property
    /// rather than an accident.
    pub fn hit(&self, tree: &HudTree, x: f32, y: f32) -> Option<usize> {
        self.placed
            .iter()
            .rev()
            .find(|placed| {
                placed.visible
                    && tree.nodes[placed.node]
                        .interact
                        .as_ref()
                        .is_some_and(|i| !i.disabled)
                    && placed.rect.contains(x, y)
            })
            .map(|placed| placed.node)
    }
}

/// A box in continuous pixels. Layout runs in `f32` end to end and rounds
/// once, per element, at emission: rounding at every level of the tree instead
/// would let a deeply nested element drift by a pixel per level.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Box2 {
    position: Vec2,
    size: Vec2,
}

impl Box2 {
    fn rounded(&self) -> PixelRect {
        PixelRect {
            x: self.position.x.round() as i64,
            y: self.position.y.round() as i64,
            width: self.size.x.round().max(0.0) as u32,
            height: self.size.y.round().max(0.0) as u32,
        }
    }
}

/// M12's anchor arithmetic, verbatim: the top-left of an `element`-sized box
/// anchored inside a `container`-sized box, with `offset` measured **inward**
/// from the anchor.
///
/// This expression is the compatibility guarantee. For an element with no
/// parent the container is the viewport, so the result is bit-identical to what
/// `anchored_box` in the rasterizer computed before this module existed —
/// which is why no pre-M31 baseline moves.
pub fn anchored(anchor: HudAnchor, offset: Vec2, element: Vec2, container: Vec2) -> Vec2 {
    let (w, h) = (element.x, element.y);
    let (cw, ch) = (container.x, container.y);
    let (x, y) = match anchor {
        HudAnchor::TopLeft => (offset.x, offset.y),
        HudAnchor::TopRight => (cw - offset.x - w, offset.y),
        HudAnchor::BottomLeft => (offset.x, ch - offset.y - h),
        HudAnchor::BottomRight => (cw - offset.x - w, ch - offset.y - h),
        HudAnchor::Center => (cw / 2.0 + offset.x - w / 2.0, ch / 2.0 + offset.y - h / 2.0),
    };
    Vec2::new(x, y)
}

/// The tree's parent/child structure, resolved once.
struct Structure {
    /// Children of each node, already sorted by `(class, file order)`.
    children: Vec<Vec<usize>>,
    /// Roots, sorted the same way.
    roots: Vec<usize>,
}

impl Structure {
    /// Resolve `parent` names to indices.
    ///
    /// Defensive where validation is authoritative: a name that matches no
    /// entity, one that matches an entity carrying no `HudPanel`, a cycle, and
    /// anything past [`MAX_HUD_DEPTH`] are all validation errors with their own
    /// codes — but layout must still terminate on a scene that reached it
    /// anyway, so each of those cases falls back to "this element is a child of
    /// the viewport" rather than looping or panicking.
    fn resolve(tree: &HudTree) -> Self {
        let count = tree.nodes.len();
        let mut parent: Vec<Option<usize>> = (0..count)
            .map(|i| {
                let name = tree.nodes[i].kind.parent()?;
                let target = tree.index_of(name)?;
                // Only a panel can be a parent, and nothing parents itself.
                (target != i && matches!(tree.nodes[target].kind, HudKind::Panel(_)))
                    .then_some(target)
            })
            .collect();

        // Break cycles and over-deep chains by rooting the offending node.
        // Walked against a snapshot, so rooting one node cannot change what
        // the next one sees.
        let chains = parent.clone();
        let rooted: Vec<usize> = (0..count)
            .filter(|&start| {
                let mut walk = chains[start];
                let mut steps = 0usize;
                while let Some(next) = walk {
                    steps += 1;
                    if steps > MAX_HUD_DEPTH || next == start {
                        return true;
                    }
                    walk = chains[next];
                }
                false
            })
            .collect();
        for i in rooted {
            parent[i] = None;
        }

        let mut children = vec![Vec::new(); count];
        let mut roots = Vec::new();
        for (i, of) in parent.iter().enumerate() {
            match of {
                Some(p) => children[*p].push(i),
                None => roots.push(i),
            }
        }

        let order = |a: &usize, b: &usize| {
            (tree.nodes[*a].kind.class(), *a).cmp(&(tree.nodes[*b].kind.class(), *b))
        };
        for list in &mut children {
            list.sort_by(order);
        }
        roots.sort_by(order);

        Self { children, roots }
    }
}

/// Lay the overlay out against a framebuffer.
///
/// Children are measured **bottom-up** (a hugging panel needs its children's
/// sizes) and placed **top-down** (a child's position needs its parent's box).
pub fn layout(tree: &HudTree, width: u32, height: u32) -> UiLayout {
    let structure = Structure::resolve(tree);
    let viewport = Vec2::new(width as f32, height as f32);

    // Bottom-up: memoized so a panel measures its subtree once.
    let mut intrinsic: Vec<Option<Vec2>> = vec![None; tree.nodes.len()];
    for i in 0..tree.nodes.len() {
        measure(tree, &structure, i, &mut intrinsic);
    }

    let mut placed = Vec::with_capacity(tree.nodes.len());
    let viewport_box = Box2 {
        position: Vec2::ZERO,
        size: viewport,
    };
    place_children(
        tree,
        &structure,
        &intrinsic,
        &structure.roots,
        // The viewport is a `free` container that never hugs, so a root
        // element resolves through exactly the M12 anchor path.
        &Container {
            content: viewport_box,
            layout: HudLayout::Free,
            gap: 0.0,
            align: HudAlign::Start,
            hugs: [false, false],
        },
        None,
        true,
        0,
        &mut placed,
    );

    UiLayout {
        viewport: (width, height),
        placed,
    }
}

/// What a child is being placed inside: a panel's content box, or the viewport.
struct Container {
    content: Box2,
    layout: HudLayout,
    gap: f32,
    align: HudAlign,
    /// Whether the container hugs on each axis — which is what decides
    /// whether a `free` child's anchor means anything (see [`place_children`]).
    hugs: [bool; 2],
}

/// An element's size ignoring `stretch`: what it wants to be.
fn measure(tree: &HudTree, structure: &Structure, index: usize, memo: &mut [Option<Vec2>]) -> Vec2 {
    if let Some(size) = memo[index] {
        return size;
    }
    // Guard against a cycle that survived `resolve` (it cannot, but a
    // recursive measure must not be the thing that discovers otherwise).
    memo[index] = Some(Vec2::ZERO);

    let size = match &tree.nodes[index].kind {
        HudKind::Rect(rect) => rect.size,
        HudKind::Image(image, _) => image.size,
        HudKind::Text(text) => text_size(text),
        HudKind::Panel(panel) => {
            let content = measure_content(tree, structure, index, panel, memo);
            Vec2::new(
                panel.width.unwrap_or(content.x + 2.0 * panel.padding),
                panel.height.unwrap_or(content.y + 2.0 * panel.padding),
            )
        }
    };

    memo[index] = Some(size);
    size
}

/// The block a label occupies: the widest line by the glyph-cell formula, and
/// one cell per line plus `line_gap` between them.
///
/// A single unwrapped line is `chars * 8 * scale` wide and one cell tall, which
/// is M12's measurement unchanged.
fn text_size(text: &HudText) -> Vec2 {
    let cell = (GLYPH * text_scale(text.size)) as f32;
    let lines = text_lines(text);
    let widest = lines
        .iter()
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(0);
    let count = lines.len().max(1) as f32;
    Vec2::new(
        widest as f32 * cell,
        count * cell + (count - 1.0) * text.line_gap,
    )
}

/// The extent a panel's children need, before padding.
///
/// Two rules that are easy to get wrong and both deliberate:
///
/// - **A hidden child takes no space**, so toggling one item in a column
///   closes the gap it left rather than leaving a hole.
/// - **A child stretched on an axis contributes nothing on that axis**, which
///   is what keeps "fill your parent" from being circular inside a parent that
///   is sizing itself to its children.
/// - In a `row`/`column`, a child's `offset` is a *nudge* and does not enlarge
///   the parent; otherwise nudging a label two pixels right would resize the
///   dialog around it.
fn measure_content(
    tree: &HudTree,
    structure: &Structure,
    index: usize,
    panel: &HudPanel,
    memo: &mut [Option<Vec2>],
) -> Vec2 {
    let children: Vec<usize> = structure.children[index]
        .iter()
        .copied()
        .filter(|c| tree.nodes[*c].kind.visible())
        .collect();
    if children.is_empty() {
        return Vec2::ZERO;
    }

    let mut extent = Vec2::ZERO;
    let mut main = 0.0f32;
    for (position, &child) in children.iter().enumerate() {
        let size = measure(tree, structure, child, memo);
        let stretch = tree.nodes[child].kind.stretch();
        let size = Vec2::new(
            if stretch[0] { 0.0 } else { size.x },
            if stretch[1] { 0.0 } else { size.y },
        );
        let gap = if position == 0 { 0.0 } else { panel.gap };

        match panel.layout {
            HudLayout::Free => {
                // A hugging free panel measures from its content origin, so a
                // child's `offset` is where it sits.
                let offset = tree.nodes[child].kind.offset();
                extent = extent.max(offset + size);
            }
            HudLayout::Row => {
                main += gap + size.x;
                extent.y = extent.y.max(size.y);
            }
            HudLayout::Column => {
                main += gap + size.y;
                extent.x = extent.x.max(size.x);
            }
        }
    }

    match panel.layout {
        HudLayout::Free => extent,
        HudLayout::Row => Vec2::new(main, extent.y),
        HudLayout::Column => Vec2::new(extent.x, main),
    }
}

/// An element's final size inside a container, applying `stretch`.
///
/// `stretch` on a `row`/`column`'s **main** axis is ignored: distributing
/// leftover space among siblings is flex-grow, which is a named non-goal, and
/// filling the whole main axis would silently stack every child on top of the
/// others. On the cross axis it is exactly "the button spans the column's
/// width", which is what the field is for.
fn resolved_size(container: &Container, intrinsic: Vec2, stretch: [bool; 2]) -> Vec2 {
    let content = container.content.size;
    let (fill_x, fill_y) = match container.layout {
        HudLayout::Free => (stretch[0], stretch[1]),
        HudLayout::Row => (false, stretch[1]),
        HudLayout::Column => (stretch[0], false),
    };
    Vec2::new(
        if fill_x { content.x } else { intrinsic.x },
        if fill_y { content.y } else { intrinsic.y },
    )
}

/// Where one line of text starts within the label's own box.
///
/// This only differs from the box's left edge when the text is wrapped (so
/// lines differ in width) or stretched (so the box is wider than the text) —
/// which is why M12 never needed it.
pub fn line_offset(align: HudTextAlign, box_width: f32, line_width: f32) -> f32 {
    match align {
        HudTextAlign::Left => 0.0,
        HudTextAlign::Center => ((box_width - line_width) / 2.0).max(0.0),
        HudTextAlign::Right => (box_width - line_width).max(0.0),
    }
}

/// Where `size` sits on the cross axis of a `row`/`column`.
fn cross_offset(align: HudAlign, available: f32, size: f32) -> f32 {
    match align {
        HudAlign::Start => 0.0,
        HudAlign::Center => (available - size) / 2.0,
        HudAlign::End => available - size,
    }
}

#[allow(clippy::too_many_arguments)]
fn place_children(
    tree: &HudTree,
    structure: &Structure,
    intrinsic: &[Option<Vec2>],
    children: &[usize],
    container: &Container,
    parent_name: Option<&str>,
    parent_visible: bool,
    depth: usize,
    out: &mut Vec<PlacedElement>,
) {
    // Hidden children are out of the flow entirely (they take no space), so
    // the cursor only advances for visible ones.
    let mut cursor = 0.0f32;
    let mut first = true;

    for &child in children {
        let node = &tree.nodes[child];
        let size = resolved_size(
            container,
            intrinsic[child].unwrap_or(Vec2::ZERO),
            node.kind.stretch(),
        );
        let offset = node.kind.offset();
        let visible = node.kind.visible();

        let position = match container.layout {
            HudLayout::Free => {
                // In a container that hugs on an axis, the anchor on that axis
                // degenerates to top-left: the container's size was *derived*
                // from this child's `offset + size`, so anchoring against it
                // would be circular. On a sized axis the full M12 rule applies,
                // which is the path every pre-M31 element takes.
                let anchored_position =
                    anchored(node.kind.anchor(), offset, size, container.content.size);
                Vec2::new(
                    if container.hugs[0] {
                        offset.x
                    } else {
                        anchored_position.x
                    },
                    if container.hugs[1] {
                        offset.y
                    } else {
                        anchored_position.y
                    },
                ) + container.content.position
            }
            HudLayout::Row => {
                if visible && !first {
                    cursor += container.gap;
                }
                let position = container.content.position
                    + Vec2::new(
                        cursor,
                        cross_offset(container.align, container.content.size.y, size.y),
                    )
                    + offset;
                if visible {
                    cursor += size.x;
                    first = false;
                }
                position
            }
            HudLayout::Column => {
                if visible && !first {
                    cursor += container.gap;
                }
                let position = container.content.position
                    + Vec2::new(
                        cross_offset(container.align, container.content.size.x, size.x),
                        cursor,
                    )
                    + offset;
                if visible {
                    cursor += size.y;
                    first = false;
                }
                position
            }
        };

        let box2 = Box2 { position, size };
        let effective_visible = parent_visible && visible;
        out.push(PlacedElement {
            node: child,
            rect: box2.rounded(),
            visible: effective_visible,
            parent: parent_name.map(str::to_string),
            depth,
        });

        // Depth-first: a panel's background is already pushed, so its children
        // land on top of it.
        if let HudKind::Panel(panel) = &node.kind {
            let padding = Vec2::splat(panel.padding);
            let content = Box2 {
                position: box2.position + padding,
                size: (box2.size - 2.0 * padding).max(Vec2::ZERO),
            };
            let stretch = node.kind.stretch();
            place_children(
                tree,
                structure,
                intrinsic,
                &structure.children[child],
                &Container {
                    content,
                    layout: panel.layout,
                    gap: panel.gap,
                    align: panel.align,
                    hugs: [
                        panel.width.is_none() && !stretch[0],
                        panel.height.is_none() && !stretch[1],
                    ],
                },
                Some(&node.entity),
                effective_visible,
                depth + 1,
                out,
            );
        }
    }
}

/// What the pointer is doing to the overlay: hover, an in-flight press, and
/// the click that press turned into.
///
/// Updated once per fixed step, **before scripts**, from M28's cursor and
/// button set. There is no event queue and no dispatch — a script asks
/// `world.clicked(name)`, exactly as `world.key` asks about a key. §8 of the
/// design lists why an `on_click` field was rejected; the short version is
/// that a button which runs code is a *binding*, and bindings are game logic,
/// and game logic lives in scripts.
///
/// **The press capture is runtime state and is deliberately not baked.** It is
/// of the same kind as `world.state` and rapier's contact state:
/// replay-deterministic, reset by a fresh run, and meaningless in a file —
/// a half-finished click is not a property of the scene.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Interaction {
    /// The element under the cursor this step.
    hovered: Option<String>,
    /// The element a still-held press started on. This is the one thing a
    /// polled API cannot derive for itself, which is why the engine keeps it.
    captured: Option<String>,
    /// The element released over this step — true for exactly one step.
    clicked: Option<String>,
    /// Whether a button was down at the end of the previous step, so a press
    /// edge can be told from a hold.
    was_down: bool,
}

impl Interaction {
    /// Advance one fixed step.
    ///
    /// `cursor` is in framebuffer pixels — M28 stores a *fraction*, and the
    /// caller multiplies by the frame it is laying out against, because that
    /// caller is the one that knows which frame this is (§7).
    pub fn update(&mut self, tree: &HudTree, layout: &UiLayout, cursor: Vec2, button_down: bool) {
        let under = layout
            .hit(tree, cursor.x, cursor.y)
            .map(|node| tree.nodes[node].entity.clone());

        self.hovered = under.clone();
        self.clicked = None;

        match (self.was_down, button_down) {
            // Press edge: whatever is under the cursor now owns this press.
            (false, true) => self.captured = under,
            // Release: a click only if the release landed on the element the
            // press started on. Pressing a button and sliding off it is how
            // every UI lets you change your mind.
            (true, false) => {
                if self.captured.is_some() && self.captured == under {
                    self.clicked = self.captured.take();
                } else {
                    self.captured = None;
                }
            }
            _ => {}
        }
        self.was_down = button_down;
    }

    /// Is the cursor over this element right now?
    pub fn hovered(&self, entity: &str) -> bool {
        self.hovered.as_deref() == Some(entity)
    }

    /// Did a press start on this element and is it still held? True whether or
    /// not the cursor has since slid off, which is what makes a slide-off
    /// release *not* a click while still showing the button as armed.
    pub fn pressed(&self, entity: &str) -> bool {
        self.captured.as_deref() == Some(entity)
    }

    /// Was this element clicked on this exact step?
    pub fn clicked(&self, entity: &str) -> bool {
        self.clicked.as_deref() == Some(entity)
    }

    /// The colour multiplier this element's `HudInteract` asks for right now.
    ///
    /// Press wins over hover, since a held button is over its own hover. The
    /// default tints are `[1, 1, 1]`, so an element that is not being pointed
    /// at — and every element in a scene with no cursor on it — multiplies by
    /// one and renders exactly as it would have without the component.
    pub fn tint(&self, node: &HudNode) -> Vec3 {
        let Some(interact) = &node.interact else {
            return Vec3::ONE;
        };
        if interact.disabled {
            return Vec3::ONE;
        }
        if self.pressed(&node.entity) {
            interact.press_tint
        } else if self.hovered(&node.entity) {
            interact.hover_tint
        } else {
            Vec3::ONE
        }
    }

    /// Multiply every interactive element's colour by its current tint.
    ///
    /// Applied to the extracted tree just before it is drawn, rather than
    /// inside the rasterizer: the renderer has no business knowing what a
    /// pointer is, and `hud::rasterize` stays a pure function of (tree, lines,
    /// size). Clamped after multiplying, since a hover tint brightens and may
    /// legally exceed 1.
    pub fn apply_tints(&self, tree: &mut HudTree) {
        for node in &mut tree.nodes {
            if node.interact.is_none() {
                continue;
            }
            let tint = self.tint(node);
            if tint == Vec3::ONE {
                continue;
            }
            let tinted = |c: Vec3| (c * tint).clamp(Vec3::ZERO, Vec3::ONE);
            match &mut node.kind {
                HudKind::Panel(p) => p.color = tinted(p.color),
                HudKind::Rect(r) => r.color = tinted(r.color),
                HudKind::Image(i, _) => i.tint = tinted(i.tint),
                HudKind::Text(t) => t.color = tinted(t.color),
            }
        }
    }
}

/// Every element's parent chain, for the validation pass's cycle and depth
/// checks — returned as names so the error message can print the ring.
///
/// Separate from [`Structure::resolve`], which *silences* those cases so
/// layout terminates. Validation is where they are reported.
pub fn parent_chain(tree: &HudTree, index: usize) -> Result<Vec<String>, Vec<String>> {
    let mut chain = vec![tree.nodes[index].entity.clone()];
    let mut current = index;
    loop {
        let Some(name) = tree.nodes[current].kind.parent() else {
            return Ok(chain);
        };
        let Some(next) = tree.index_of(name) else {
            return Ok(chain);
        };
        if chain.iter().any(|seen| seen == &tree.nodes[next].entity) {
            chain.push(tree.nodes[next].entity.clone());
            return Err(chain);
        }
        chain.push(tree.nodes[next].entity.clone());
        current = next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn panel(layout: HudLayout) -> HudPanel {
        HudPanel {
            anchor: HudAnchor::TopLeft,
            offset: Vec2::ZERO,
            layout,
            padding: 0.0,
            gap: 0.0,
            align: HudAlign::Start,
            width: None,
            height: None,
            color: Vec3::ONE,
            opacity: 0.0,
            parent: None,
            visible: true,
            stretch: [false, false],
        }
    }

    fn rect(size: [f32; 2]) -> HudRect {
        HudRect {
            anchor: HudAnchor::TopLeft,
            offset: Vec2::ZERO,
            size: Vec2::from(size),
            color: Vec3::ONE,
            opacity: 1.0,
            parent: None,
            visible: true,
            stretch: [false, false],
        }
    }

    fn label(text: &str) -> HudText {
        HudText {
            text: text.into(),
            anchor: HudAnchor::TopLeft,
            offset: Vec2::ZERO,
            size: 8.0,
            color: Vec3::ONE,
            parent: None,
            visible: true,
            stretch: [false, false],
            align: HudTextAlign::Left,
            wrap: 0.0,
            line_gap: 0.0,
        }
    }

    fn node(entity: &str, kind: HudKind) -> HudNode {
        HudNode {
            entity: entity.into(),
            kind,
            interact: None,
        }
    }

    fn tree(nodes: Vec<HudNode>) -> HudTree {
        HudTree { nodes }
    }

    /// The whole compatibility argument, asserted directly rather than through
    /// a render: with nothing naming a parent, the depth-first `(class, file
    /// order)` rule must produce exactly M12's "every rect in file order, then
    /// every text in file order".
    #[test]
    fn draw_order_collapses_to_the_m12_rule_when_nothing_names_a_parent() {
        let scene = tree(vec![
            node("TextA", HudKind::Text(label("a"))),
            node("RectA", HudKind::Rect(rect([4.0, 4.0]))),
            node("TextB", HudKind::Text(label("b"))),
            node("RectB", HudKind::Rect(rect([4.0, 4.0]))),
        ]);
        let out = layout(&scene, 64, 64);
        let order: Vec<&str> = out
            .placed
            .iter()
            .map(|p| scene.nodes[p.node].entity.as_str())
            .collect();
        assert_eq!(order, ["RectA", "RectB", "TextA", "TextB"]);
    }

    /// An unparented element must land where `anchored_box` put it in M12 —
    /// same expression, same rounding.
    #[test]
    fn an_unparented_element_lands_on_the_m12_anchor_arithmetic() {
        for anchor in [
            HudAnchor::TopLeft,
            HudAnchor::TopRight,
            HudAnchor::BottomLeft,
            HudAnchor::BottomRight,
            HudAnchor::Center,
        ] {
            let mut r = rect([20.0, 10.0]);
            r.anchor = anchor;
            r.offset = Vec2::new(3.0, 7.0);
            let scene = tree(vec![node("R", HudKind::Rect(r.clone()))]);
            let out = layout(&scene, 100, 50);
            let expected = anchored(anchor, r.offset, r.size, Vec2::new(100.0, 50.0));
            assert_eq!(out.placed[0].rect.x, expected.x.round() as i64);
            assert_eq!(out.placed[0].rect.y, expected.y.round() as i64);
            assert_eq!(out.placed[0].rect.width, 20);
            assert_eq!(out.placed[0].rect.height, 10);
        }
    }

    #[test]
    fn a_column_stacks_children_with_gaps_and_hugs_them() {
        let mut p = panel(HudLayout::Column);
        p.padding = 5.0;
        p.gap = 3.0;
        let mut a = rect([10.0, 4.0]);
        a.parent = Some("P".into());
        let mut b = rect([20.0, 6.0]);
        b.parent = Some("P".into());

        let scene = tree(vec![
            node("P", HudKind::Panel(p)),
            node("A", HudKind::Rect(a)),
            node("B", HudKind::Rect(b)),
        ]);
        let out = layout(&scene, 200, 200);

        // Hug: widest child (20) and stacked heights (4 + 3 + 6), plus padding.
        let panel_rect = out.of(&scene, "P").unwrap().rect;
        assert_eq!((panel_rect.width, panel_rect.height), (30, 23));
        assert_eq!(out.of(&scene, "A").unwrap().rect.y, 5);
        assert_eq!(out.of(&scene, "B").unwrap().rect.y, 12);
    }

    #[test]
    fn align_centers_children_on_the_cross_axis() {
        let mut p = panel(HudLayout::Column);
        p.align = HudAlign::Center;
        p.width = Some(100.0);
        let mut a = rect([20.0, 4.0]);
        a.parent = Some("P".into());
        let scene = tree(vec![
            node("P", HudKind::Panel(p)),
            node("A", HudKind::Rect(a)),
        ]);
        let out = layout(&scene, 200, 200);
        assert_eq!(out.of(&scene, "A").unwrap().rect.x, 40);
    }

    #[test]
    fn stretch_fills_the_cross_axis_but_never_the_main_one() {
        let mut p = panel(HudLayout::Column);
        p.width = Some(80.0);
        p.height = Some(90.0);
        let mut a = rect([5.0, 7.0]);
        a.parent = Some("P".into());
        a.stretch = [true, true];
        let scene = tree(vec![
            node("P", HudKind::Panel(p)),
            node("A", HudKind::Rect(a)),
        ]);
        let out = layout(&scene, 200, 200);
        let r = out.of(&scene, "A").unwrap().rect;
        assert_eq!(r.width, 80, "cross axis fills");
        assert_eq!(r.height, 7, "main axis keeps its own size");
    }

    #[test]
    fn a_stretched_root_fills_the_viewport() {
        let mut r = rect([1.0, 1.0]);
        r.stretch = [true, true];
        let scene = tree(vec![node("Dim", HudKind::Rect(r))]);
        let out = layout(&scene, 320, 180);
        assert_eq!(
            out.placed[0].rect,
            PixelRect {
                x: 0,
                y: 0,
                width: 320,
                height: 180
            }
        );
    }

    #[test]
    fn a_hidden_child_takes_no_space_and_hides_its_subtree() {
        let mut p = panel(HudLayout::Column);
        p.visible = false;
        let mut a = rect([10.0, 4.0]);
        a.parent = Some("P".into());
        a.visible = false;
        let mut b = rect([10.0, 6.0]);
        b.parent = Some("P".into());

        let scene = tree(vec![
            node("P", HudKind::Panel(p)),
            node("A", HudKind::Rect(a)),
            node("B", HudKind::Rect(b)),
        ]);
        let out = layout(&scene, 200, 200);
        // A is skipped in the flow, so B starts at the top.
        assert_eq!(out.of(&scene, "B").unwrap().rect.y, 0);
        assert_eq!(out.of(&scene, "P").unwrap().rect.height, 6);
        // The hidden panel hides a visible child.
        assert!(!out.of(&scene, "B").unwrap().visible);
    }

    #[test]
    fn wrapping_breaks_on_spaces_and_lets_a_long_word_overflow() {
        let mut t = label("the quick brown fox");
        t.wrap = 80.0; // 10 cells at size 8
        assert_eq!(text_lines(&t), ["the quick", "brown fox"]);

        let mut long = label("supercalifragilistic");
        long.wrap = 40.0;
        assert_eq!(text_lines(&long), ["supercalifragilistic"]);
    }

    #[test]
    fn an_explicit_newline_breaks_a_line_at_wrap_zero() {
        let t = label("one\ntwo");
        assert_eq!(text_lines(&t), ["one", "two"]);
        assert_eq!(text_size(&t), Vec2::new(24.0, 16.0));
    }

    #[test]
    fn a_single_unwrapped_line_measures_exactly_as_it_did_in_m12() {
        let mut t = label("HELLO");
        t.size = 16.0;
        assert_eq!(text_lines(&t), ["HELLO"]);
        assert_eq!(text_size(&t), Vec2::new(5.0 * 16.0, 16.0));
    }

    /// Rounding once at emission rather than per level: five nested panels
    /// each offset by half a pixel must not accumulate five roundings.
    #[test]
    fn nesting_rounds_once_rather_than_drifting_per_level() {
        let mut nodes = Vec::new();
        for level in 0..5 {
            let mut p = panel(HudLayout::Free);
            p.offset = Vec2::splat(0.5);
            p.width = Some(100.0);
            p.height = Some(100.0);
            if level > 0 {
                p.parent = Some(format!("P{}", level - 1));
            }
            nodes.push(node(&format!("P{level}"), HudKind::Panel(p)));
        }
        let out = layout(&tree(nodes.clone()), 200, 200);
        let scene = tree(nodes);
        // Five half-pixel offsets sum to 2.5, rounding once to 3 — not five
        // roundings of 0.5 each, which would give 5.
        assert_eq!(out.of(&scene, "P4").unwrap().rect.x, 3);
    }

    #[test]
    fn a_parent_cycle_is_reported_with_its_ring_and_still_lays_out() {
        let mut a = panel(HudLayout::Free);
        a.parent = Some("B".into());
        let mut b = panel(HudLayout::Free);
        b.parent = Some("A".into());
        let scene = tree(vec![
            node("A", HudKind::Panel(a)),
            node("B", HudKind::Panel(b)),
        ]);

        assert!(parent_chain(&scene, 0).is_err());
        // Layout terminates regardless: both fall back to viewport children.
        assert_eq!(layout(&scene, 64, 64).placed.len(), 2);
    }

    #[test]
    fn the_topmost_interactive_element_wins_a_hit() {
        let mut under = rect([100.0, 100.0]);
        under.parent = None;
        let mut over = rect([50.0, 50.0]);
        over.parent = None;

        let mut scene = tree(vec![
            node("Under", HudKind::Rect(under)),
            node("Over", HudKind::Rect(over)),
        ]);
        scene.nodes[0].interact = Some(HudInteract {
            hover_tint: Vec3::ONE,
            press_tint: Vec3::ONE,
            disabled: false,
        });
        scene.nodes[1].interact = Some(HudInteract {
            hover_tint: Vec3::ONE,
            press_tint: Vec3::ONE,
            disabled: false,
        });

        let out = layout(&scene, 200, 200);
        assert_eq!(out.hit(&scene, 10.0, 10.0), Some(1), "later wins");
        assert_eq!(out.hit(&scene, 70.0, 70.0), Some(0), "only the under one");
        assert_eq!(out.hit(&scene, 150.0, 150.0), None);
    }

    fn button(entity: &str, size: [f32; 2], offset: [f32; 2]) -> HudNode {
        let mut r = rect(size);
        r.offset = Vec2::from(offset);
        HudNode {
            entity: entity.into(),
            kind: HudKind::Rect(r),
            interact: Some(HudInteract {
                hover_tint: Vec3::splat(2.0),
                press_tint: Vec3::splat(0.5),
                disabled: false,
            }),
        }
    }

    /// The edge a polled API cannot derive for itself: `clicked` is true for
    /// exactly one step, and only when the release lands on the element the
    /// press started on.
    #[test]
    fn a_click_is_press_and_release_on_the_same_element_for_one_step() {
        let scene = tree(vec![button("Go", [40.0, 20.0], [0.0, 0.0])]);
        let out = layout(&scene, 200, 200);
        let inside = Vec2::new(10.0, 10.0);
        let mut state = Interaction::default();

        state.update(&scene, &out, inside, false);
        assert!(state.hovered("Go") && !state.pressed("Go") && !state.clicked("Go"));

        state.update(&scene, &out, inside, true);
        assert!(state.pressed("Go"), "the press is captured");
        assert!(!state.clicked("Go"), "not clicked until released");

        state.update(&scene, &out, inside, true);
        assert!(state.pressed("Go"), "still held");
        assert!(!state.clicked("Go"));

        state.update(&scene, &out, inside, false);
        assert!(state.clicked("Go"), "released over the same element");
        assert!(!state.pressed("Go"), "the capture is spent");

        state.update(&scene, &out, inside, false);
        assert!(!state.clicked("Go"), "clicked lasts exactly one step");
    }

    /// Pressing a button and sliding off it is how every UI lets you change
    /// your mind, so it must not click.
    #[test]
    fn pressing_inside_and_releasing_outside_does_not_click() {
        let scene = tree(vec![button("Go", [40.0, 20.0], [0.0, 0.0])]);
        let out = layout(&scene, 200, 200);
        let mut state = Interaction::default();

        state.update(&scene, &out, Vec2::new(10.0, 10.0), true);
        assert!(state.pressed("Go"));
        // Slid off, still holding: armed but no longer hovered.
        state.update(&scene, &out, Vec2::new(150.0, 150.0), true);
        assert!(state.pressed("Go"), "the capture survives the slide");
        assert!(!state.hovered("Go"));
        state.update(&scene, &out, Vec2::new(150.0, 150.0), false);
        assert!(!state.clicked("Go"), "released off the element: no click");
        assert!(!state.pressed("Go"));
    }

    /// Adding a `HudInteract` must move no pixel until a cursor arrives, which
    /// is what the `[1, 1, 1]` defaults are for — and press must win over
    /// hover, since a held button is also under the pointer.
    #[test]
    fn tints_apply_on_hover_and_press_and_never_otherwise() {
        let mut scene = tree(vec![button("Go", [40.0, 20.0], [0.0, 0.0])]);
        let out = layout(&scene, 200, 200);
        let mut state = Interaction::default();
        let color = |t: &HudTree| match &t.nodes[0].kind {
            HudKind::Rect(r) => r.color,
            _ => unreachable!(),
        };

        // Cursor elsewhere: untouched.
        state.update(&scene, &out, Vec2::new(150.0, 150.0), false);
        let mut untouched = scene.clone();
        state.apply_tints(&mut untouched);
        assert_eq!(color(&untouched), Vec3::ONE);

        // Hover brightens — clamped, since the rect was already white.
        state.update(&scene, &out, Vec2::new(10.0, 10.0), false);
        let mut hovered = scene.clone();
        state.apply_tints(&mut hovered);
        assert_eq!(color(&hovered), Vec3::ONE, "2× white clamps back to white");

        // Press darkens, and wins over the hover it is also inside.
        state.update(&scene, &out, Vec2::new(10.0, 10.0), true);
        let mut pressed = scene.clone();
        state.apply_tints(&mut pressed);
        assert_eq!(color(&pressed), Vec3::splat(0.5));

        // A disabled element is not a candidate at all, so nothing tints.
        if let Some(interact) = &mut scene.nodes[0].interact {
            interact.disabled = true;
        }
        let mut disabled = scene.clone();
        Interaction::default().apply_tints(&mut disabled);
        assert_eq!(color(&disabled), Vec3::ONE);
    }

    /// A modal panel carrying a `HudInteract` swallows clicks to what is under
    /// it; one without is click-through. That makes "does this menu block the
    /// game" an authored property rather than an accident.
    #[test]
    fn an_interactive_panel_blocks_and_a_bare_one_does_not() {
        let mut backdrop = panel(HudLayout::Free);
        backdrop.stretch = [true, true];
        let mut nodes = vec![
            HudNode {
                entity: "Backdrop".into(),
                kind: HudKind::Panel(backdrop),
                interact: None,
            },
            button("Under", [40.0, 20.0], [0.0, 0.0]),
        ];
        // Click-through: the bare backdrop is not a candidate.
        let scene = tree(nodes.clone());
        let out = layout(&scene, 200, 200);
        assert_eq!(out.hit(&scene, 10.0, 10.0), Some(1));

        // The same backdrop with a HudInteract swallows it — and it is later
        // in draw order than the button only because the button is nested
        // under nothing, so put it first and give it the interact.
        nodes[0].interact = Some(HudInteract {
            hover_tint: Vec3::ONE,
            press_tint: Vec3::ONE,
            disabled: false,
        });
        nodes.swap(0, 1);
        let scene = tree(nodes);
        let out = layout(&scene, 200, 200);
        assert_eq!(
            out.hit(&scene, 10.0, 10.0),
            Some(1),
            "the topmost interactive element wins, and that is the backdrop"
        );
        assert_eq!(scene.nodes[1].entity, "Backdrop");
    }

    #[test]
    fn a_disabled_or_hidden_element_is_not_a_candidate() {
        let mut r = rect([100.0, 100.0]);
        r.visible = false;
        let mut scene = tree(vec![node("R", HudKind::Rect(r))]);
        scene.nodes[0].interact = Some(HudInteract {
            hover_tint: Vec3::ONE,
            press_tint: Vec3::ONE,
            disabled: false,
        });
        assert_eq!(layout(&scene, 200, 200).hit(&scene, 10.0, 10.0), None);

        scene.nodes[0].kind = HudKind::Rect(rect([100.0, 100.0]));
        scene.nodes[0].interact = Some(HudInteract {
            hover_tint: Vec3::ONE,
            press_tint: Vec3::ONE,
            disabled: true,
        });
        assert_eq!(layout(&scene, 200, 200).hit(&scene, 10.0, 10.0), None);
    }
}
