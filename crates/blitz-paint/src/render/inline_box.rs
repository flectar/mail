//! The box decorations of non-atomic inline elements: their backgrounds,
//! borders and box shadows, painted on each of their line box fragments (CSS
//! 2.1 Appendix E, step 7.2.1).
//!
//! An inline element broken across lines is painted as if it were one box, laid
//! out as a strip of all its fragments end to end and then sliced into them
//! (`box-decoration-break: slice`): its inline-start border, padding and radii
//! are only on its first fragment, its inline-end ones only on its last, and
//! its backgrounds continue from one fragment to the next.

use std::cell::RefCell;
use std::collections::HashMap;

use anyrender::PaintScene;
use blitz_dom::node::{GeneratedText, TextBrush};
use blitz_dom::{BaseDocument, NodeId};
use kurbo::{Affine, Rect, Vec2};
use parley::{InlineBoxKind, Layout, Line, PositionedLayoutItem};
use peniko::Fill;
use style::properties::ComputedValues;
use style::values::computed::{BorderStyle, LengthPercentage, Margin};
use style::values::generics::image::GenericImage;

use super::ElementCx;
use crate::color::ToColorColor as _;

/// One line's part of a decorated inline element.
pub(crate) struct InlineFragment {
    pub(crate) node_id: NodeId,
    /// The line the fragment is on.
    pub(crate) line: usize,
    /// How many decorated inline ancestors the element has in its inline
    /// formatting context. Outer elements paint first.
    depth: u16,
    /// The fragment's border box, in the layout's (device) pixels.
    rect: Rect,
    /// Where the fragment starts in its element's strip, and the strip's width.
    strip_offset: f64,
    strip_width: f64,
    /// Sides that are an edge of the whole element rather than a line break:
    /// what is painted past them, such as shadows, is not cut off.
    open_left: bool,
    open_right: bool,
}

impl InlineFragment {
    /// How far past its border box the fragment may paint: its outer shadows.
    pub(crate) fn ink_rect(&self, style: &ComputedValues, scale: f64) -> Rect {
        let reach = shadow_reach(style, scale);
        self.rect.inflate(reach, reach)
    }
}

/// Paint-relevant geometry of a decorated inline element, in device pixels.
#[derive(Clone, Copy)]
struct Decorated {
    depth: u16,
    rtl: bool,
    margin_start: f64,
    margin_end: f64,
    /// Padding and border above and below the content area.
    top: f64,
    bottom: f64,
}

/// What the runs and boxes of one line contribute to one element.
struct LineExtent {
    line: usize,
    x0: f32,
    x1: f32,
    /// The extents of the element's own start and end edge placeholders.
    start: Option<(f32, f32)>,
    end: Option<(f32, f32)>,
    /// Baseline, ascent and descent of the element's own text on the line,
    /// else of the first text inside it.
    own_metrics: Option<(f32, f32, f32)>,
    metrics: Option<(f32, f32, f32)>,
    /// The line's content ends before its hanging white space.
    content_x0: f32,
    content_x1: f32,
}

fn length(lp: &LengthPercentage) -> f64 {
    lp.to_length().map_or(0.0, |l| f64::from(l.px()))
}

fn margin(margin: &Margin) -> f64 {
    match margin {
        Margin::LengthPercentage(lp) => length(lp),
        _ => 0.0,
    }
}

/// A border's used widths, top, right, bottom and left. Its computed width
/// stays `medium` when it has no style.
fn border_widths(style: &ComputedValues) -> [f64; 4] {
    let border = style.get_border();
    let width = |style: BorderStyle, width: f32| {
        if matches!(style, BorderStyle::None | BorderStyle::Hidden) {
            0.0
        } else {
            f64::from(width)
        }
    };
    [
        width(
            border.border_top_style,
            border.border_top_width.0.to_f32_px(),
        ),
        width(
            border.border_right_style,
            border.border_right_width.0.to_f32_px(),
        ),
        width(
            border.border_bottom_style,
            border.border_bottom_width.0.to_f32_px(),
        ),
        width(
            border.border_left_style,
            border.border_left_width.0.to_f32_px(),
        ),
    ]
}

fn shadow_reach(style: &ComputedValues, scale: f64) -> f64 {
    style
        .get_effects()
        .box_shadow
        .0
        .iter()
        .filter(|shadow| !shadow.inset)
        .map(|shadow| {
            let offset = f64::from(shadow.base.horizontal.px().abs())
                .max(f64::from(shadow.base.vertical.px().abs()));
            offset + f64::from(shadow.spread.px()) + f64::from(shadow.base.blur.px()) * 2.5
        })
        .fold(0.0, f64::max)
        * scale
        + 2.0
}

/// Whether an inline element has anything of its own box to paint.
fn has_box_decorations(style: &ComputedValues) -> bool {
    let background = style.get_background();
    let current_color = style.clone_color();
    let color = background
        .background_color
        .resolve_to_absolute(&current_color)
        .as_srgb_color();
    if color.components[3] > 0.0 {
        return true;
    }
    if background
        .background_image
        .0
        .iter()
        .any(|image| !matches!(image, GenericImage::None))
    {
        return true;
    }
    if border_widths(style).iter().any(|width| *width > 0.0) {
        return true;
    }
    !style.get_effects().box_shadow.0.is_empty()
}

/// The decorated inline elements a run or box is inside, innermost first,
/// from `id` up to (but not including) the inline root.
fn decorated_chain(
    doc: &BaseDocument,
    root: NodeId,
    id: NodeId,
    scale: f64,
    decorated: &mut HashMap<NodeId, Option<Decorated>>,
    chains: &mut HashMap<NodeId, Vec<NodeId>>,
) -> Vec<NodeId> {
    if let Some(chain) = chains.get(&id) {
        return chain.clone();
    }
    // The ancestors on the way up, innermost first, that stay inline.
    let mut path = Vec::new();
    let mut current = Some(id);
    while let Some(node_id) = current {
        if node_id == root {
            break;
        }
        let Some(node) = doc.get_node(node_id) else {
            break;
        };
        if let Some(styles) = node.primary_styles() {
            let display = styles.clone_display();
            // A `display: contents` element has no box; inline content of an
            // anonymous block keeps its DOM parent, which is not inline.
            if !display.is_contents() {
                if !display.is_inline_flow() {
                    break;
                }
                path.push(node_id);
            }
        }
        current = node.parent;
    }
    let depth_of = |index: usize| (path.len() - 1 - index) as u16;
    let chain: Vec<NodeId> = path
        .iter()
        .enumerate()
        .filter(|(index, node_id)| {
            decorated
                .entry(**node_id)
                .or_insert_with(|| decoration(doc, **node_id, depth_of(*index), scale))
                .is_some()
        })
        .map(|(_, node_id)| *node_id)
        .collect();
    chains.insert(id, chain.clone());
    chain
}

fn decoration(doc: &BaseDocument, id: NodeId, depth: u16, scale: f64) -> Option<Decorated> {
    let node = doc.get_node(id)?;
    let style = node.primary_styles()?;
    if style.get_inherited_box().visibility != style::computed_values::visibility::T::Visible
        || !has_box_decorations(&style)
    {
        return None;
    }
    let margins = style.get_margin();
    let padding = style.get_padding();
    let [border_top, _, border_bottom, _] = border_widths(&style);
    let rtl = !style.writing_mode.is_bidi_ltr();
    let (margin_start, margin_end) = if rtl {
        (margin(&margins.margin_right), margin(&margins.margin_left))
    } else {
        (margin(&margins.margin_left), margin(&margins.margin_right))
    };
    Some(Decorated {
        depth,
        rtl,
        margin_start: margin_start * scale,
        margin_end: margin_end * scale,
        top: (length(&padding.padding_top.0) + border_top) * scale,
        bottom: (length(&padding.padding_bottom.0) + border_bottom) * scale,
    })
}

/// The fragments of the decorated inline elements in an inline formatting
/// context, in paint order: by line, then outer elements before inner ones.
pub(crate) fn inline_fragments(
    doc: &BaseDocument,
    layout: &Layout<TextBrush>,
    root: NodeId,
    scale: f64,
) -> Vec<InlineFragment> {
    let mut decorated = HashMap::new();
    let mut chains = HashMap::new();

    // Most paragraphs have no decorated inline element: tell from their
    // styles, without walking their lines.
    let any = layout.styles().iter().any(|style| {
        !decorated_chain(
            doc,
            root,
            style.brush.id,
            scale,
            &mut decorated,
            &mut chains,
        )
        .is_empty()
    }) || layout.inline_boxes().any(|inline_box| {
        let parent = doc
            .get_node(NodeId::from_u64(inline_box.id))
            .and_then(|node| node.parent);
        parent.is_some_and(|parent| {
            !decorated_chain(doc, root, parent, scale, &mut decorated, &mut chains).is_empty()
        })
    });
    if !any {
        return Vec::new();
    }

    let rtl_paragraph = layout.is_rtl();
    let mut extents: HashMap<NodeId, Vec<LineExtent>> = HashMap::new();
    for (line_index, line) in layout.lines().enumerate() {
        let metrics = line.metrics();
        let start = metrics.offset + metrics.inline_min_coord;
        let (content_x0, content_x1) = if rtl_paragraph {
            (start + metrics.hanging_advance, start + metrics.advance)
        } else {
            (start, start + metrics.advance - metrics.hanging_advance)
        };
        for item in line.items() {
            let (owner, x0, x1, run) = match item {
                PositionedLayoutItem::GlyphRun(glyph_run) => {
                    let x0 = glyph_run.offset();
                    let brush = glyph_run.style().brush;
                    (
                        brush.id,
                        x0,
                        x0 + glyph_run.advance(),
                        Some((glyph_run, brush)),
                    )
                }
                PositionedLayoutItem::InlineBox(inline_box) => {
                    if inline_box.kind != InlineBoxKind::InFlow {
                        continue;
                    }
                    let Some(parent) = doc
                        .get_node(NodeId::from_u64(inline_box.id))
                        .and_then(|node| node.parent)
                    else {
                        continue;
                    };
                    (parent, inline_box.x, inline_box.x + inline_box.width, None)
                }
            };
            let chain = decorated_chain(doc, root, owner, scale, &mut decorated, &mut chains);
            for node_id in chain {
                let lines = extents.entry(node_id).or_default();
                if lines.last().is_none_or(|extent| extent.line != line_index) {
                    lines.push(LineExtent {
                        line: line_index,
                        x0,
                        x1,
                        start: None,
                        end: None,
                        own_metrics: None,
                        metrics: None,
                        content_x0,
                        content_x1,
                    });
                }
                let extent = lines.last_mut().unwrap();
                extent.x0 = extent.x0.min(x0);
                extent.x1 = extent.x1.max(x1);
                let Some((glyph_run, brush)) = &run else {
                    continue;
                };
                let run_metrics = glyph_run.run().font_metrics();
                let metrics = (
                    glyph_run.baseline(),
                    run_metrics.ascent,
                    run_metrics.descent,
                );
                if brush.id == node_id {
                    match brush.generated {
                        GeneratedText::StartEdge => extent.start = Some((x0, x1)),
                        GeneratedText::EndEdge => extent.end = Some((x0, x1)),
                        GeneratedText::None | GeneratedText::LineBreakOpportunity => {}
                    }
                    extent.own_metrics.get_or_insert(metrics);
                } else {
                    extent.metrics.get_or_insert(metrics);
                }
            }
        }
    }

    let mut fragments = Vec::new();
    for (node_id, lines) in extents {
        let Some(Some(decoration)) = decorated.get(&node_id).copied() else {
            continue;
        };
        let first = fragments.len();
        for extent in &lines {
            let Some((baseline, ascent, descent)) = extent.own_metrics.or(extent.metrics) else {
                continue;
            };
            // Without its edge, a side of the fragment is a line break, where
            // the hanging white space at the end of the line is left out.
            let (left_edge, right_edge, left_margin, right_margin) = if decoration.rtl {
                (
                    extent.end,
                    extent.start,
                    decoration.margin_end,
                    decoration.margin_start,
                )
            } else {
                (
                    extent.start,
                    extent.end,
                    decoration.margin_start,
                    decoration.margin_end,
                )
            };
            let x0 = match left_edge {
                Some((x0, _)) => f64::from(x0) + left_margin,
                None => f64::from(extent.x0.max(extent.content_x0.min(extent.x1))),
            };
            let x1 = match right_edge {
                Some((_, x1)) => f64::from(x1) - right_margin,
                None => f64::from(extent.x1.min(extent.content_x1.max(extent.x0))),
            };
            // The content area is the font's ascent and descent around the
            // baseline, which padding and borders extend.
            let baseline = f64::from(baseline);
            let y0 = baseline - f64::from(ascent).round() - decoration.top;
            let y1 = baseline + f64::from(descent).round() + decoration.bottom;
            let rect = Rect::new(
                x0.round(),
                y0.round(),
                x1.round().max(x0.round()),
                y1.round(),
            );
            fragments.push(InlineFragment {
                node_id,
                line: extent.line,
                depth: decoration.depth,
                rect,
                strip_offset: 0.0,
                strip_width: 0.0,
                open_left: left_edge.is_some(),
                open_right: right_edge.is_some(),
            });
        }
        // The strip runs in line order, right to left for a right-to-left
        // element, so its start edge is on the first fragment.
        let parts = &mut fragments[first..];
        let width: f64 = parts.iter().map(|fragment| fragment.rect.width()).sum();
        let mut offset = 0.0;
        let count = parts.len();
        for (index, fragment) in parts.iter_mut().enumerate() {
            fragment.strip_width = width;
            fragment.strip_offset = if decoration.rtl {
                width - offset - fragment.rect.width()
            } else {
                offset
            };
            offset += fragment.rect.width();
            // Only a fragment with its element's start or end edge leaves that
            // side open. An element's only fragment is never cut.
            if count == 1 {
                fragment.open_left = true;
                fragment.open_right = true;
            } else if decoration.rtl {
                fragment.open_right |= index == 0;
                fragment.open_left |= index + 1 == count;
            } else {
                fragment.open_left |= index == 0;
                fragment.open_right |= index + 1 == count;
            }
        }
    }
    fragments.sort_by(|a, b| {
        (a.line, a.depth)
            .cmp(&(b.line, b.depth))
            .then(a.rect.x0.total_cmp(&b.rect.x0))
    });
    fragments
}

impl ElementCx<'_, '_> {
    /// Paint one fragment of an inline element's box, positioned by
    /// `transform` (the inline layout's).
    pub(super) fn draw_inline_fragment(
        &self,
        scene: &mut impl PaintScene,
        fragment: &InlineFragment,
        transform: Affine,
    ) {
        let Some(node) = self.context.dom.get_node(fragment.node_id) else {
            return;
        };
        let Some(style) = node.primary_styles() else {
            return;
        };
        if node.element_data().is_none() {
            return;
        }
        let scale = self.scale;
        let [border_top, border_right, border_bottom, border_left] =
            border_widths(&style).map(|width| width as f32);
        let padding = style.get_padding();
        let px = |value: f64| (value / scale) as f32;
        let rect = fragment.rect;
        // The element's whole box, as one strip of its fragments, in CSS px.
        let layout = taffy::Layout {
            size: taffy::Size {
                width: px(fragment.strip_width),
                height: px(rect.height()),
            },
            border: taffy::Rect {
                left: border_left,
                right: border_right,
                top: border_top,
                bottom: border_bottom,
            },
            padding: taffy::Rect {
                left: length(&padding.padding_left.0) as f32,
                right: length(&padding.padding_right.0) as f32,
                top: length(&padding.padding_top.0) as f32,
                bottom: length(&padding.padding_bottom.0) as f32,
            },
            ..taffy::Layout::new()
        };
        let strip_transform =
            transform * Affine::translate(Vec2::new(rect.x0 - fragment.strip_offset, rect.y0));
        let cx = self.context.element_cx(node, layout, strip_transform, None);

        // Cut the strip at the fragment's line breaks.
        let reach = shadow_reach(&style, scale);
        let ink = rect.inflate(reach, reach);
        let clip = Rect::new(
            if fragment.open_left { ink.x0 } else { rect.x0 },
            ink.y0,
            if fragment.open_right { ink.x1 } else { rect.x1 },
            ink.y1,
        );
        let clipped = !(fragment.open_left && fragment.open_right);
        if clipped {
            scene.push_clip_layer(Fill::NonZero, transform, &clip);
        }
        cx.draw_outset_box_shadow(scene);
        cx.draw_background(scene);
        cx.draw_inset_box_shadow(scene);
        cx.draw_border(scene);
        if clipped {
            scene.pop_layer();
        }
    }
}

/// The opacity of the non-atomic inline elements around `node`, up to its
/// nearest block-level ancestor: they have no box of their own to be painted
/// with a layer, so boxes inside them take it on.
pub(crate) fn inline_ancestor_opacity(doc: &BaseDocument, node: &blitz_dom::Node) -> f32 {
    let mut opacity = 1.0;
    let mut current = node.parent.and_then(|id| doc.get_node(id));
    while let Some(ancestor) = current {
        let Some(style) = ancestor.primary_styles() else {
            break;
        };
        let display = style.clone_display();
        if !display.is_contents() {
            if !display.is_inline_flow() {
                break;
            }
            opacity *= style.get_effects().opacity;
        }
        current = ancestor.parent.and_then(|id| doc.get_node(id));
    }
    opacity
}

/// The non-atomic inline elements of an inline formatting context that have
/// an `opacity` below 1. Each is painted as a group (it is a stacking
/// context): its fragments' decorations and its text go into a layer with its
/// opacity, so that their overlapping parts don't show through each other.
/// Groups nested in another are painted as part of it.
pub(crate) struct OpacityGroups<'a> {
    doc: &'a BaseDocument,
    root: NodeId,
    any: bool,
    /// The outermost group each node is in, if any.
    group: RefCell<HashMap<NodeId, Option<NodeId>>>,
}

impl<'a> OpacityGroups<'a> {
    pub(crate) fn new(doc: &'a BaseDocument, layout: &Layout<TextBrush>, root: NodeId) -> Self {
        let mut groups = Self {
            doc,
            root,
            any: false,
            group: RefCell::new(HashMap::new()),
        };
        groups.any = layout
            .styles()
            .iter()
            .any(|style| groups.group_of(style.brush.id).is_some());
        groups
    }

    pub(crate) fn is_empty(&self) -> bool {
        !self.any
    }

    /// The outermost inline element with an opacity below 1 that `id` is in.
    pub(crate) fn group_of(&self, id: NodeId) -> Option<NodeId> {
        if let Some(group) = self.group.borrow().get(&id) {
            return *group;
        }
        let mut group = None;
        let mut current = Some(id);
        while let Some(node_id) = current {
            if node_id == self.root {
                break;
            }
            let Some(node) = self.doc.get_node(node_id) else {
                break;
            };
            if let Some(style) = node.primary_styles() {
                let display = style.clone_display();
                if !display.is_contents() {
                    if !display.is_inline_flow() {
                        break;
                    }
                    if style.get_effects().opacity < 1.0 {
                        group = Some(node_id);
                    }
                }
            }
            current = node.parent;
        }
        self.group.borrow_mut().insert(id, group);
        group
    }

    /// The groups with content on `line`, in the order they start on it,
    /// with their opacity and the bounds of that content.
    pub(crate) fn on_line(
        &self,
        line: &Line<'_, TextBrush>,
        fragments: &[InlineFragment],
        scale: f64,
    ) -> Vec<(NodeId, f32, Rect)> {
        let mut found: Vec<(NodeId, f32, Rect)> = Vec::new();
        if !self.any {
            return found;
        }
        let metrics = line.metrics();
        let mut add = |group: NodeId, rect: Rect| match found.iter_mut().find(|g| g.0 == group) {
            Some(entry) => entry.2 = entry.2.union(rect),
            None => {
                let opacity = self
                    .doc
                    .get_node(group)
                    .and_then(|node| node.primary_styles())
                    .map_or(1.0, |style| style.get_effects().opacity);
                found.push((group, opacity, rect));
            }
        };
        for item in line.items() {
            let PositionedLayoutItem::GlyphRun(glyph_run) = item else {
                continue;
            };
            let Some(group) = self.group_of(glyph_run.style().brush.id) else {
                continue;
            };
            // Glyphs can reach well past the line box.
            let font_size = f64::from(glyph_run.run().font_size());
            let x0 = f64::from(glyph_run.offset());
            let rect = Rect::new(
                x0 - font_size,
                f64::from(metrics.block_min_coord) - font_size,
                x0 + f64::from(glyph_run.advance()) + font_size,
                f64::from(metrics.block_max_coord) + font_size,
            );
            add(group, rect);
        }
        for fragment in fragments {
            if let Some(group) = self.group_of(fragment.node_id) {
                let style = self
                    .doc
                    .get_node(fragment.node_id)
                    .and_then(|node| node.primary_styles());
                let ink = style.map_or(fragment.rect, |style| fragment.ink_rect(&style, scale));
                add(group, ink);
            }
        }
        found
    }
}
