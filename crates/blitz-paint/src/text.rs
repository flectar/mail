use anyrender::PaintScene;
use anyrender::filters::{Filter, FilterEffect};
use blitz_dom::{
    BaseDocument, NodeId,
    node::{GeneratedText, TextBrush},
    util::ToColorColor,
};
use kurbo::{Affine, BezPath, Cap, Circle, Rect, Shape as _, Stroke};
use parley::{Affinity, Cursor, GlyphRun, Layout, Line, PositionedLayoutItem, Selection};
use peniko::{Fill, Mix};
use std::collections::HashMap;
use std::sync::Arc;
use style::properties::generated::longhands::text_decoration_style::computed_value::T as TextDecorationStyle;
use style::values::computed::{
    Length, LengthPercentage, TextDecorationLength, TextDecorationLine, TextUnderlinePosition,
};
use style::values::generics::text::{GenericTextDecorationInset, GenericTextDecorationLength};

use crate::FONT_EMBOLDEN_ENABLED;
use crate::color::Color;

/// Per-font-face cache of the OS/2 `usWinAscent / unitsPerEm` ratio, keyed by the font
/// blob's unique id and face index. `None` is cached for fonts without a usable OS/2 or
/// head table so they aren't re-parsed either.
type WinAscentCache = HashMap<(u64, u32), Option<f32>>;

/// The font's OS/2 `usWinAscent`, in the same (device) pixels as `font_size`.
///
/// This is the ascent browsers use as the top of the "em box" when positioning overlines. It
/// is typically taller than the hhea ascent Parley exposes via [`parley::layout::run::RunMetrics`].
/// Returns `None` when the font has no OS/2 table.
fn win_ascent(cache: &mut WinAscentCache, font: &parley::FontData, font_size: f32) -> Option<f32> {
    let ratio = *cache
        .entry((font.data.id(), font.index))
        .or_insert_with(|| win_ascent_ratio(font));
    Some(ratio? * font_size)
}

/// The unitless `usWinAscent / unitsPerEm` ratio for a font face, or `None` when the font
/// has no usable head/OS/2 table.
fn win_ascent_ratio(font: &parley::FontData) -> Option<f32> {
    use skrifa::raw::{FontRef, TableProvider as _};
    let font_ref = FontRef::from_index(font.data.as_ref(), font.index).ok()?;
    let units_per_em = font_ref.head().ok()?.units_per_em();
    let win_ascent = font_ref.os2().ok()?.us_win_ascent();
    Some(win_ascent as f32 / units_per_em as f32)
}

/// Mirrors Blink's `SelectBestDashGap`: choose the gap length (as close as possible to
/// `gap_length`) that fits a whole number of `dash_length` dashes across `stroke_length`,
/// so dashes are evenly distributed and a dash lands at each end of the line.
fn select_best_dash_gap(stroke_length: f64, dash_length: f64, gap_length: f64) -> f64 {
    let available_length = stroke_length + gap_length;
    let min_num_dashes = (available_length / (dash_length + gap_length)).floor();
    let max_num_dashes = min_num_dashes + 1.0;
    let min_num_gaps = min_num_dashes - 1.0;
    let max_num_gaps = max_num_dashes - 1.0;
    if min_num_gaps < 1.0 {
        return gap_length;
    }
    let min_gap = (stroke_length - min_num_dashes * dash_length) / min_num_gaps;
    let max_gap = (stroke_length - max_num_dashes * dash_length) / max_num_gaps;
    if max_gap <= 0.0 || (min_gap - gap_length).abs() < (max_gap - gap_length).abs() {
        min_gap
    } else {
        max_gap
    }
}

/// A text decoration resolved from a single "decorating box" (an element that
/// sets `text-decoration-line`), ready to be drawn over a glyph run. Because the
/// `text-decoration-*` properties do not inherit, decorations are propagated from
/// ancestors rather than inherited, so a run may be covered by several of these.
///
/// Only the run-independent (style-derived) values are stored here so an instance
/// can be cached on the ancestor stack and reused across every run within the same
/// decorating box. The `text-decoration-inset` and `text-underline-offset` values
/// depend on the run's advance/font-size and are resolved lazily when drawing.
#[derive(Clone)]
struct ResolvedDecoration {
    line: TextDecorationLine,
    style: TextDecorationStyle,
    color: Color,
    thickness: TextDecorationLength,
    /// `text-underline-offset`; `None` for `auto`. Resolved per run.
    underline_offset: Option<LengthPercentage>,
    /// Whether `text-underline-position: under` is in effect.
    underline_under: bool,
    /// Whether `text-underline-position: from-font` is in effect.
    underline_from_font: bool,
    /// `text-decoration-inset`. Resolved per run.
    inset: GenericTextDecorationInset<LengthPercentage>,
}

/// An element on the current ancestor path (inline root -> run node), caching the
/// values resolved from its computed style so descending into the same node across
/// consecutive runs doesn't re-resolve them.
struct DecorationStackEntry {
    node_id: NodeId,
    /// This node's (inherited) text colour, used for the glyphs of runs whose
    /// innermost node is this one.
    text_color: Color,
    /// The decoration this node introduces as a decorating box, if any.
    decoration: Option<ResolvedDecoration>,
    /// This node's (inherited) `text-shadow` list, `None` for `none`. Equal
    /// lists share one allocation, so the runs of a shadowed paragraph are
    /// shadowed together.
    text_shadows: Option<Arc<[TextShadow]>>,
}

/// One `text-shadow`, in device pixels.
#[derive(Clone, Copy, PartialEq)]
struct TextShadow {
    offset: kurbo::Vec2,
    /// The blur's standard deviation, which is half the blur radius.
    std_dev: f64,
    color: Color,
}

/// The most blur a text shadow gets, in device pixels. Blurrier shadows are
/// too faint to tell apart, and the cap bounds the layer a blur needs.
const MAX_SHADOW_STD_DEV: f64 = 100.0;

/// Resolve the cached style values for a single node into a [`DecorationStackEntry`].
fn resolve_decoration_entry(
    doc: &BaseDocument,
    node_id: NodeId,
    scale: f64,
    parent: Option<&DecorationStackEntry>,
) -> DecorationStackEntry {
    let Some(styles) = doc.get_node(node_id).and_then(|node| node.primary_styles()) else {
        return DecorationStackEntry {
            node_id,
            text_color: Color::BLACK,
            decoration: None,
            text_shadows: None,
        };
    };

    let itext = styles.get_inherited_text();
    let text = styles.get_text();
    let text_color = itext.color.as_color_color();

    // `text-shadow` lengths are CSS pixels and the layout is in device pixels.
    // A shadow without a colour takes the text's.
    let text_shadows = (!itext.text_shadow.0.is_empty()).then(|| {
        itext
            .text_shadow
            .0
            .iter()
            .map(|shadow| TextShadow {
                offset: kurbo::Vec2::new(
                    f64::from(shadow.horizontal.px()) * scale,
                    f64::from(shadow.vertical.px()) * scale,
                ),
                std_dev: (f64::from(shadow.blur.px()) * scale / 2.0).min(MAX_SHADOW_STD_DEV),
                color: shadow
                    .color
                    .resolve_to_absolute(&itext.color)
                    .as_color_color(),
            })
            .collect::<Arc<[_]>>()
    });
    let text_shadows = match (text_shadows, parent.and_then(|p| p.text_shadows.as_ref())) {
        (Some(own), Some(inherited)) if *own == **inherited => Some(inherited.clone()),
        (own, _) => own,
    };

    let drawn_lines = TextDecorationLine::UNDERLINE
        | TextDecorationLine::OVERLINE
        | TextDecorationLine::LINE_THROUGH;
    let line = text.text_decoration_line;
    // Decorations propagate through the box tree, and a `display: contents` element
    // generates no box, so its decorations have no effect on descendants.
    let is_contents = styles.clone_display().is_contents();
    let decoration = (!is_contents && line.intersects(drawn_lines)).then(|| {
        // `text-decoration-color: currentColor` (the initial value) resolves against
        // the decorating box's own colour, not the descendant run's.
        let color = text
            .text_decoration_color
            .as_absolute()
            .map(ToColorColor::as_color_color)
            .unwrap_or(text_color);

        ResolvedDecoration {
            line,
            style: text.text_decoration_style,
            color,
            thickness: text.text_decoration_thickness.clone(),
            underline_offset: itext.text_underline_offset.non_auto(),
            underline_under: itext
                .text_underline_position
                .contains(TextUnderlinePosition::UNDER),
            underline_from_font: itext
                .text_underline_position
                .contains(TextUnderlinePosition::FROM_FONT),
            inset: text.text_decoration_inset.clone(),
        }
    });

    DecorationStackEntry {
        node_id,
        text_color,
        decoration,
        text_shadows,
    }
}

/// The run-dependent geometry used to position and size a decoration, captured from a
/// single glyph run. Firefox draws a decoration once for the whole decorating box using
/// the box's own font, so we prefer the values from a run whose innermost node *is* the
/// decorating box (its own text), falling back to the first run it covers.
#[derive(Clone)]
struct DecorationRunGeometry {
    /// The baseline the decoration is positioned from (the run's own, `vertical-align`
    /// shifted, baseline).
    baseline: f32,
    ascent: f32,
    descent: f32,
    underline_offset: f32,
    underline_size: f32,
    strikethrough_size: f32,
    font: parley::FontData,
    font_size: f32,
    css_font_size: f64,
}

/// A decoration accumulated across the runs of a single line for one decorating box.
///
/// The `text-decoration-*` properties describe the whole box, so we gather the horizontal
/// extent it covers on the line (`min_x`/`max_x`) and the font geometry to draw with, then
/// paint a single line spanning the box rather than one segment per run.
struct LineDecoration {
    node_id: NodeId,
    deco: ResolvedDecoration,
    min_x: f64,
    max_x: f64,
    /// Geometry from a run whose innermost node is the decorating box itself.
    own: Option<DecorationRunGeometry>,
    /// Geometry from the first run the box covers (fallback when it has no own text on
    /// this line, e.g. it only wraps differently-sized descendants).
    first: Option<DecorationRunGeometry>,
}

/// Reusable scratch storage for drawing text across all inline formatting contexts in a
/// document. The vectors are cleared between uses without releasing their allocations.
#[derive(Default)]
pub(crate) struct DrawTextContext {
    stack: Vec<DecorationStackEntry>,
    path_scratch: Vec<NodeId>,
    deco_boxes: Vec<LineDecoration>,
    win_ascent_ratios: WinAscentCache,
    outline_bounds: HashMap<(u64, u32), Option<Rect>>,
    ink_skip: crate::ink_skip::InkSkip,
    /// The glyphs of a line's runs under an underline or overline, whose ink
    /// those skip.
    inked_runs: Vec<InkedRun>,
    inked_glyphs: Vec<(u32, f32, f32)>,
    inked_coords: Vec<i16>,
    /// The decorating boxes (indices into `deco_boxes`) over each inked run.
    inked_boxes: Vec<usize>,
    /// The glyph runs of the line being painted.
    line_runs: Vec<LineRun>,
}

/// A glyph run of the line being painted, resolved before any of the line is
/// drawn so that its text shadows can go beneath all of its text.
struct LineRun {
    /// The run's index among the line's items.
    item: usize,
    color: Color,
    embolden: kurbo::Vec2,
    /// Whether none of the run's glyphs can reach the viewport.
    culled: bool,
    /// The run's ink before the transform, where the font's bounds are known.
    ink: Option<Rect>,
    /// The run's advance across the line's height.
    extent: Rect,
    shadows: Option<Arc<[TextShadow]>>,
}

/// A glyph run under an underline or overline on the current line.
struct InkedRun {
    font: parley::FontData,
    font_size: f32,
    /// A synthetic italic's slant, as x per y.
    skew: f32,
    glyphs: std::ops::Range<usize>,
    coords: std::ops::Range<usize>,
    boxes: std::ops::Range<usize>,
}

/// How to paint the text of an inline formatting context.
#[derive(Clone, Copy)]
pub(crate) struct TextPaint<'t> {
    /// Glyph runs entirely outside it are not drawn.
    pub(crate) viewport: Option<Rect>,
    /// Only the runs whose node it accepts are drawn and decorated.
    pub(crate) run_filter: Option<&'t dyn Fn(NodeId) -> bool>,
    /// The layout's text.
    pub(crate) text: &'t str,
    /// Whether the paragraph runs right to left.
    pub(crate) rtl: bool,
    /// A colour for every glyph instead of its own, such as a highlight's.
    pub(crate) text_color: Option<Color>,
}

// The font-wide outline bounds include accents and overhangs which line-height
// does not. Unknown, variable and color fonts keep the normal paint path.
fn font_outline_bounds(font: &parley::FontData) -> Option<Rect> {
    use skrifa::raw::{FontRef, TableProvider as _};
    let font = FontRef::from_index(font.data.as_ref(), font.index).ok()?;
    if font.fvar().is_ok()
        || font.colr().is_ok()
        || font.cbdt().is_ok()
        || font.sbix().is_ok()
        || font.svg().is_ok()
    {
        return None;
    }
    let head = font.head().ok()?;
    let units = f64::from(head.units_per_em());
    if units == 0.0 || head.x_min() >= head.x_max() || head.y_min() >= head.y_max() {
        return None;
    }
    Some(Rect::new(
        f64::from(head.x_min()) / units,
        -f64::from(head.y_max()) / units,
        f64::from(head.x_max()) / units,
        -f64::from(head.y_min()) / units,
    ))
}

/// Local paint bounds for each line, computed once per layout. Unknown font
/// bounds, authored decorations and text shadows conservatively retain the
/// original path.
pub(crate) fn line_paint_bounds(
    layout: &Layout<TextBrush>,
    doc: &BaseDocument,
    root: NodeId,
) -> Vec<Option<Rect>> {
    let mut fonts = HashMap::new();
    let mut decorated = HashMap::new();
    layout
        .lines()
        .map(|line| {
            let mut bounds: Option<Rect> = None;
            for item in line.items() {
                let PositionedLayoutItem::GlyphRun(glyph_run) = item else {
                    continue;
                };
                let id = glyph_run.style().brush.id;
                let has_decoration = *decorated.entry(id).or_insert_with(|| {
                    // `text-shadow` inherits, so the run's own node has it.
                    let shadowed = doc
                        .get_node(id)
                        .and_then(|node| node.primary_styles())
                        .is_some_and(|styles| {
                            !styles.get_inherited_text().text_shadow.0.is_empty()
                        });
                    if shadowed {
                        return true;
                    }
                    let mut ancestor = Some(id);
                    while let Some(id) = ancestor {
                        if resolve_decoration_entry(doc, id, 1.0, None)
                            .decoration
                            .is_some()
                        {
                            return true;
                        }
                        if id == root {
                            break;
                        }
                        ancestor = doc.get_node(id).and_then(|node| node.parent);
                    }
                    false
                });
                if has_decoration {
                    return None;
                }
                let run = glyph_run.run();
                let font = run.font();
                let font_bounds = (*fonts
                    .entry((font.data.id(), font.index))
                    .or_insert_with(|| font_outline_bounds(font)))?;
                // Some web fonts draw past their `head` bounding box, so allow a
                // margin that grows with the font size.
                let margin = (f64::from(run.font_size()) * 0.25).max(2.0);
                let glyph_bounds = Affine::scale(run.font_size() as f64)
                    .transform_rect_bbox(font_bounds)
                    .inflate(margin, margin);
                let synthesis = run
                    .synthesis()
                    .skew()
                    .map(|angle| Affine::skew(angle.to_radians().tan() as f64, 0.0))
                    .unwrap_or(Affine::IDENTITY);
                let glyph_bounds = synthesis.transform_rect_bbox(glyph_bounds);
                let metrics = run.font_metrics();
                // Inline backgrounds may extend past the glyph outlines.
                let mut run_bounds = Rect::new(
                    glyph_run.offset() as f64,
                    (glyph_run.baseline() - metrics.ascent) as f64,
                    (glyph_run.offset() + glyph_run.advance()) as f64,
                    (glyph_run.baseline() + metrics.descent) as f64,
                );
                for glyph in glyph_run.positioned_glyphs() {
                    run_bounds = run_bounds
                        .union(glyph_bounds + kurbo::Vec2::new(glyph.x as f64, glyph.y as f64));
                }
                bounds = Some(bounds.map_or(run_bounds, |bounds| bounds.union(run_bounds)));
            }
            bounds.filter(|bounds| bounds.is_finite())
        })
        .collect()
}

/// Resolve the CSS `text-decoration-thickness` to device pixels, as Blink's
/// `TextDecorationThickness::Resolve` does: `auto` is a tenth of the font
/// size, lengths and percentages (of the font size) are rounded, `from-font`
/// is the font's own thickness. It is at least one pixel.
fn decoration_thickness(
    thickness: &TextDecorationLength,
    metric_size: f32,
    css_font_size: f64,
    scale: f64,
) -> f64 {
    (match thickness {
        GenericTextDecorationLength::LengthPercentage(lp) => {
            (lp.resolve(Length::new(css_font_size as f32)).px() as f64 * scale).round()
        }
        GenericTextDecorationLength::FromFont => metric_size as f64,
        GenericTextDecorationLength::Auto => css_font_size * scale / 10.0,
    })
    .max(1.0)
}

/// The thickness a line of `style` is painted with: whole pixels, rounded
/// down for solid, double and wavy lines and to the nearest for dotted and
/// dashed ones (Blink's `DecorationLinePainter`).
fn painted_thickness(thickness: f64, style: TextDecorationStyle) -> f64 {
    match style {
        TextDecorationStyle::Dotted | TextDecorationStyle::Dashed => thickness.round(),
        _ => thickness.floor(),
    }
    .max(1.0)
}

/// Rounds `y`, in the coordinates `transform` maps to device pixels, to the
/// nearest device pixel row, so that a line is crisp. Lines under rotated
/// or scaled text are left alone.
fn snap_y(transform: Affine, y: f64) -> f64 {
    let [a, b, c, d, _, ty] = transform.as_coeffs();
    if b != 0.0 || c != 0.0 || (a - 1.0).abs() > 1e-6 || (d - 1.0).abs() > 1e-6 {
        return y;
    }
    (y + ty + 0.5).floor() - ty
}

/// One decoration line to draw: its extent, the top of its (first) line and
/// what it looks like.
struct DecorationLine<'b> {
    x0: f64,
    x1: f64,
    top: f64,
    thickness: f64,
    brush: &'b anyrender::Paint,
    style: TextDecorationStyle,
    /// Where the second line of a `double` decoration is, from the first.
    double_offset: f64,
    /// Spans where the line skips glyphs' ink, sorted and disjoint.
    gaps: &'b [(f64, f64)],
}

/// Draws a decoration line. Geometry is built explicitly (rather than relying
/// on backend dash support) so it renders consistently across renderers.
fn draw_decoration_line(scene: &mut impl PaintScene, transform: Affine, line: &DecorationLine<'_>) {
    let (x, w) = (line.x0, line.x1 - line.x0);
    if w <= 0.0 {
        return;
    }
    let size = painted_thickness(line.thickness, line.style);
    let brush = line.brush;
    match line.style {
        TextDecorationStyle::MozNone => {
            // no line. Equivalent to `text-decoration-line: none`
        }
        // Solid and double lines are rectangles on whole device pixel rows,
        // drawn in pieces between the glyphs whose ink they skip.
        TextDecorationStyle::Solid | TextDecorationStyle::Double => {
            let mut tops = vec![snap_y(transform, line.top)];
            if line.style == TextDecorationStyle::Double {
                tops.push(snap_y(transform, line.top + line.double_offset));
            }
            let mut start = x;
            let mut pieces = Vec::new();
            for &(gap_start, gap_end) in line.gaps {
                if gap_end <= start || gap_start >= x + w {
                    continue;
                }
                if gap_start > start {
                    pieces.push((start, gap_start));
                }
                start = start.max(gap_end);
            }
            if start < x + w {
                pieces.push((start, x + w));
            }
            for top in tops {
                for &(piece_start, piece_end) in &pieces {
                    let rect = Rect::new(piece_start, top, piece_end, top + size);
                    scene.fill(Fill::NonZero, transform, brush, None, &rect);
                }
            }
        }
        style => {
            // A dotted or dashed line is centred on a pixel row (or between two,
            // when it is an even number of pixels thick).
            let center = line.top + size / 2.0;
            let y = if size as i64 % 2 == 1 {
                snap_y(transform, center - 0.5) + 0.5
            } else {
                snap_y(transform, center)
            };
            // Ink skipping cuts these through a clip.
            let clipped = !line.gaps.is_empty();
            if clipped {
                let reach = 3.0 * size + 2.0;
                let mut clip = BezPath::new();
                let bounds = Rect::new(x - reach, y - reach, x + w + reach, y + reach);
                clip.extend(bounds.path_elements(0.1));
                for &(gap_start, gap_end) in line.gaps {
                    let hole = Rect::new(gap_start, y - reach, gap_end, y + reach);
                    clip.extend(hole.path_elements(0.1));
                }
                scene.push_clip_layer(Fill::EvenOdd, transform, &clip);
            }
            draw_patterned_line(scene, transform, brush, style, x, w, y, size);
            if clipped {
                scene.pop_layer();
            }
        }
    }
}

/// Draws a dotted, dashed or wavy line centred on `y`.
#[allow(clippy::too_many_arguments)]
fn draw_patterned_line(
    scene: &mut impl PaintScene,
    transform: Affine,
    brush: &anyrender::Paint,
    style: TextDecorationStyle,
    x: f64,
    w: f64,
    y: f64,
    size: f64,
) {
    match style {
        // Round dots (diameter = line thickness) spaced one dot apart.
        TextDecorationStyle::Dotted => {
            let radius = size / 2.0;
            let step = 2.0 * size;
            let mut cx = x + radius;
            while cx <= x + w - radius {
                let dot = Circle::new((cx, y), radius);
                scene.fill(Fill::NonZero, transform, brush, None, &dot);
                cx += step;
            }
        }
        // Dashes as filled rectangles. Dash/gap lengths are relative to the
        // thickness (matching Blink): thinner lines use proportionally longer
        // dashes and gaps so they don't read as dotted or solid.
        TextDecorationStyle::Dashed => {
            let (dash_ratio, gap_ratio) = if size >= 3.0 { (2.0, 1.0) } else { (3.0, 2.0) };
            let dash = size * dash_ratio;
            let nominal_gap = size * gap_ratio;
            // Nudge the gap so a whole number of dashes fits the run evenly.
            let gap = if w > 2.0 * dash {
                select_best_dash_gap(w, dash, nominal_gap)
            } else {
                nominal_gap
            };
            let mut dx = x;
            while dx < x + w {
                let end = (dx + dash).min(x + w);
                let rect = Rect::new(dx, y - size / 2.0, end, y + size / 2.0);
                scene.fill(Fill::NonZero, transform, brush, None, &rect);
                dx += dash + gap;
            }
        }
        // A squiggle built from alternating quadratic Béziers.
        TextDecorationStyle::Wavy => {
            let amplitude = size;
            let half_wave = (2.0 * size).max(1.0);
            let mut path = BezPath::new();
            path.move_to((x, y));
            let mut px = x;
            let mut up = true;
            while px < x + w {
                let nx = (px + half_wave).min(x + w);
                // A quad Bézier reaches half the control-point offset at its
                // midpoint, so use `2 * amplitude` to hit the target peak.
                let ctrl_y = if up {
                    y - 2.0 * amplitude
                } else {
                    y + 2.0 * amplitude
                };
                path.quad_to(((px + nx) / 2.0, ctrl_y), (nx, y));
                px = nx;
                up = !up;
            }
            let stroke = Stroke::new(size).with_caps(Cap::Round);
            scene.stroke(&stroke, transform, brush, None, &path);
        }
        _ => {}
    }
}

/// The spans an underline or overline of decorating box `index` skips on a
/// line, between its `top` and `bottom` edges: around every glyph under the
/// box whose outline crosses the line, less half a pixel above and below, from
/// `dilation` before the outline's leftmost crossing to `dilation` after its
/// rightmost one. Sorted and merged.
fn ink_gaps(
    inked: &InkedLine<'_>,
    ink_skip: &mut crate::ink_skip::InkSkip,
    index: usize,
    top: f64,
    bottom: f64,
    dilation: f64,
) -> Vec<(f64, f64)> {
    let mut gaps = Vec::new();
    let (band_top, band_bottom) = (top + 0.5, (bottom - 0.5).max(top + 0.5));
    for run in inked.runs {
        if !inked.boxes[run.boxes.clone()].contains(&index) {
            continue;
        }
        let coords = &inked.coords[run.coords.clone()];
        for &(glyph, x, y) in &inked.glyphs[run.glyphs.clone()] {
            let baseline = f64::from(y);
            if let Some((left, right)) = ink_skip.intercept(
                &run.font,
                run.font_size,
                coords,
                run.skew,
                glyph,
                (band_top - baseline) as f32,
                (band_bottom - baseline) as f32,
            ) {
                let x = f64::from(x);
                gaps.push((
                    x + f64::from(left) - dilation,
                    x + f64::from(right) + dilation,
                ));
            }
        }
    }
    gaps.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut merged: Vec<(f64, f64)> = Vec::with_capacity(gaps.len());
    for gap in gaps {
        match merged.last_mut() {
            Some(last) if gap.0 <= last.1 => last.1 = last.1.max(gap.1),
            _ => merged.push(gap),
        }
    }
    merged
}

/// Paint the decorations accumulated for one line, one line per decorating
/// box, positioned and sized as Blink does (`TextDecorationOffset`,
/// `TextDecorationInfo`, `DecorationLinePainter`).
#[allow(clippy::too_many_arguments)]
fn flush_line_decorations(
    scene: &mut impl PaintScene,
    transform: Affine,
    scale: f64,
    deco_boxes: &[LineDecoration],
    win_ascent_ratios: &mut WinAscentCache,
    inline_root_id: NodeId,
    line_baseline: f32,
    inked: &InkedLine<'_>,
    ink_skip: &mut crate::ink_skip::InkSkip,
    shadow: Option<(Color, kurbo::Vec2)>,
) {
    // A text shadow draws the decorations again, offset and in its colour.
    let transform = match shadow {
        Some((_, offset)) => transform * Affine::translate(offset),
        None => transform,
    };
    // Draw innermost boxes first so ancestors' decorations paint on top, matching the
    // per-run drawing order this replaced (`stack.iter().rev()`).
    for (index, acc) in deco_boxes.iter().enumerate().rev() {
        let deco = &acc.deco;
        // Prefer the decorating box's own font; fall back to the first run it covers.
        let geom = match (&acc.own, &acc.first) {
            (Some(own), _) => own.clone(),
            (None, Some(first)) => {
                let mut geom = first.clone();
                // A descendant run's baseline may be shifted by `vertical-align`, but the
                // decoration is positioned from the decorating box's own baseline. For the
                // inline root that is the line's baseline.
                if acc.node_id == inline_root_id {
                    geom.baseline = line_baseline;
                }
                geom
            }
            (None, None) => continue,
        };
        let geom = &geom;
        let width = acc.max_x - acc.min_x;
        if width <= 0.0 {
            continue;
        }
        let brush = anyrender::Paint::from(shadow.map_or(deco.color, |(color, _)| color));
        let baseline = f64::from(geom.baseline);

        // `text-decoration-inset` shortens (or, when negative, extends) the line from the
        // inline-start/end edges. Percentages resolve against the decoration line length
        // (the box's total advance on this line); `auto` is no inset.
        // Assumes LTR: start is the left edge.
        let (inset_start, inset_end) = match &deco.inset {
            GenericTextDecorationInset::LengthPercentage { start, end } => {
                // The extent is in device pixels; convert to CSS pixels so the resolved
                // result can be scaled back.
                let line_length = Length::new((width / scale) as f32);
                (
                    start.resolve(line_length).px() as f64 * scale,
                    end.resolve(line_length).px() as f64 * scale,
                )
            }
            GenericTextDecorationInset::Auto => (0.0, 0.0),
        };
        let (x0, x1) = (acc.min_x + inset_start, acc.max_x - inset_end);

        let thickness = decoration_thickness(
            &deco.thickness,
            geom.underline_size,
            geom.css_font_size,
            scale,
        );
        let painted = painted_thickness(thickness, deco.style);
        // Ink is skipped from a gap as wide as the line is thick, up to 13px.
        let dilation = thickness.min(13.0);
        let mut draw = |top: f64, double_offset: f64, skips_ink: bool| {
            let bottom = if deco.style == TextDecorationStyle::Double {
                (top + painted).max(top + double_offset + painted)
            } else {
                top + painted
            };
            let first_top = top.min(top + double_offset.min(0.0));
            let gaps = if skips_ink {
                ink_gaps(inked, ink_skip, index, first_top, bottom, dilation)
            } else {
                Vec::new()
            };
            draw_decoration_line(
                scene,
                transform,
                &DecorationLine {
                    x0,
                    x1,
                    top,
                    thickness,
                    brush: &brush,
                    style: deco.style,
                    double_offset,
                    gaps: &gaps,
                },
            );
        };

        if deco.line.contains(TextDecorationLine::UNDERLINE) {
            // `text-underline-offset` moves the underline away from the text, resolved
            // against the font size (percentages relative to 1em), rounded.
            let extra_offset = deco
                .underline_offset
                .as_ref()
                .map(|lp| {
                    (lp.resolve(Length::new(geom.css_font_size as f32)).px() as f64 * scale).round()
                })
                .unwrap_or(0.0);
            let top = if deco.underline_under {
                // Below the descenders.
                baseline + f64::from(geom.descent) + extra_offset
            } else if deco.underline_from_font {
                // At the font's suggested position.
                baseline - f64::from(geom.underline_offset) + extra_offset
            } else if deco.underline_offset.is_some() {
                // An explicit offset is taken from the alphabetic baseline.
                baseline + extra_offset
            } else {
                // Just below the baseline, with a gap of half the thickness.
                baseline + (thickness / 2.0).ceil().max(1.0)
            };
            // A `double` underline extends downward, away from the text.
            draw(top, thickness + 1.0, true);
        }
        if deco.line.contains(TextDecorationLine::OVERLINE) {
            // The line sits on the top of the text: its lower edge rests on the ascent so
            // it clears the glyphs, and it extends upward from there.
            //
            // Browsers use the OS/2 `usWinAscent` for this edge, which is taller than the
            // hhea-based ascent Parley reports; using the smaller value would draw the
            // overline too low. Fall back to Parley's ascent for fonts without a usable
            // OS/2 table.
            let ascent =
                win_ascent(win_ascent_ratios, &geom.font, geom.font_size).unwrap_or(geom.ascent);
            let top = baseline - f64::from(ascent) - painted;
            // A `double` overline extends upward, away from the text.
            draw(top, -(thickness + 1.0), true);
        }
        if deco.line.contains(TextDecorationLine::LINE_THROUGH) {
            let thickness = decoration_thickness(
                &deco.thickness,
                geom.strikethrough_size,
                geom.css_font_size,
                scale,
            );
            // Centre the line-through a third of the ascent above the baseline, matching
            // Chrome (which places it `2/3 * ascent` below the text-top). Parley's
            // `strikethrough_offset` (the font's `yStrikeoutPosition`) sits lower and would
            // draw the line too close to the baseline.
            let ascent =
                win_ascent(win_ascent_ratios, &geom.font, geom.font_size).unwrap_or(geom.ascent);
            let top = baseline - f64::from(ascent) / 3.0 - thickness / 2.0;
            draw_decoration_line(
                scene,
                transform,
                &DecorationLine {
                    x0,
                    x1,
                    top,
                    thickness,
                    brush: &brush,
                    style: deco.style,
                    double_offset: (thickness + 1.0).floor(),
                    gaps: &[],
                },
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
/// Draw the glyphs and text decorations of `lines`.
pub(crate) fn stroke_text<'a>(
    scene: &mut impl PaintScene,
    lines: impl Iterator<Item = Line<'a, TextBrush>>,
    doc: &BaseDocument,
    transform: Affine,
    scale: f64,
    inline_root_id: NodeId,
    context: &mut DrawTextContext,
    paint: TextPaint<'_>,
) {
    let TextPaint {
        viewport,
        run_filter,
        text,
        rtl,
        text_color: color_override,
    } = paint;
    let DrawTextContext {
        stack,
        path_scratch,
        deco_boxes,
        win_ascent_ratios,
        outline_bounds,
        ink_skip,
        inked_runs,
        inked_glyphs,
        inked_coords,
        inked_boxes,
        line_runs,
    } = context;
    stack.clear();
    path_scratch.clear();
    deco_boxes.clear();

    // Persistent stack mirroring the ancestor path (inline root -> current run's
    // node) as we walk the runs. The `text-decoration-*` properties are *not*
    // inherited; instead a decoration set on an ancestor is propagated to the
    // descendant text it wraps. Rather than re-resolving styles for the whole
    // ancestor chain on every run, we cache each node's resolved values here and,
    // for each run, only resolve styles for the nodes newly descended into (popping
    // as we ascend). `path_scratch` is a reusable buffer for the run's node path.
    for line in lines {
        // Decorations accumulated for this line, keyed by decorating box, so each box is
        // painted once (spanning all its runs) using its own font — matching Firefox, which
        // draws one decoration per box rather than one stepped segment per differently-sized
        // run. Clearing preserves the allocation for the next line and inline context.
        deco_boxes.clear();
        inked_runs.clear();
        inked_glyphs.clear();
        inked_coords.clear();
        inked_boxes.clear();
        line_runs.clear();
        let line_metrics = line.metrics();

        for (item_index, item) in line.items().enumerate() {
            if let PositionedLayoutItem::GlyphRun(glyph_run) = item {
                let style = glyph_run.style();
                // An inline element's edge has no text to draw or decorate.
                if style.brush.generated != GeneratedText::None
                    || run_filter.is_some_and(|accept| !accept(style.brush.id))
                {
                    continue;
                }
                let run = glyph_run.run();
                let font = run.font();
                let font_size = run.font_size();
                let metrics = run.font_metrics();
                let synthesis = run.synthesis();
                let glyph_xform = synthesis
                    .skew()
                    .map(|angle| Affine::skew(angle.to_radians().tan() as f64, 0.0));

                let css_font_size = font_size as f64 / scale;

                // Reconcile the stack with this run's ancestor path. Build the path
                // (inline root -> run node), keep the shared prefix already on the stack,
                // pop the rest, and resolve styles only for the newly-descended nodes.
                path_scratch.clear();
                let mut walk_id = Some(style.brush.id);
                while let Some(node_id) = walk_id {
                    path_scratch.push(node_id);
                    if node_id == inline_root_id {
                        break;
                    }
                    walk_id = doc.get_node(node_id).and_then(|node| node.parent);
                }
                path_scratch.reverse();

                let shared = stack
                    .iter()
                    .zip(path_scratch.iter())
                    .take_while(|(entry, node_id)| entry.node_id == **node_id)
                    .count();
                stack.truncate(shared);
                for &node_id in &path_scratch[shared..] {
                    let entry = resolve_decoration_entry(doc, node_id, scale, stack.last());
                    stack.push(entry);
                }

                // The glyph colour comes from the run's own node (the stack top): `color`
                // inherits, so the innermost inline element already carries the right value.
                let text_color = color_override
                    .or_else(|| stack.last().map(|e| e.text_color))
                    .unwrap_or(Color::BLACK);

                let mut embolden = if FONT_EMBOLDEN_ENABLED {
                    let fs = font_size as f64 / scale;
                    kurbo::Vec2::new((0.015125 * fs).min(0.3), (0.0121 * fs).min(0.3))
                } else {
                    kurbo::Vec2::default()
                };
                // A face lighter than the weight asked for, with no bolder face
                // to use, is emboldened as Skia fakes bold.
                if synthesis.embolden() {
                    embolden += synthetic_bold(f64::from(font_size));
                }

                // Highlights repaint text without its shadows.
                let shadows = if color_override.is_some() {
                    None
                } else {
                    stack.last().and_then(|entry| entry.text_shadows.clone())
                };

                // The run's ink, to skip runs that can't reach the viewport and
                // to place shadows. A font without usable bounds is never skipped.
                let mut culled = false;
                let mut ink = None;
                if viewport.is_some() || shadows.is_some() {
                    let mut glyphs = glyph_run.positioned_glyphs();
                    match glyphs.next() {
                        None => culled = true,
                        Some(first) => {
                            let mut positions = Rect::new(
                                first.x as f64,
                                first.y as f64,
                                first.x as f64,
                                first.y as f64,
                            );
                            for glyph in glyphs {
                                positions = positions.union_pt((glyph.x as f64, glyph.y as f64));
                            }
                            let bounds = *outline_bounds
                                .entry((font.data.id(), font.index))
                                .or_insert_with(|| font_outline_bounds(font));
                            if let Some(bounds) = bounds {
                                // Include hinting and synthetic emboldening before
                                // applying italic synthesis.
                                let bounds = Affine::scale(font_size as f64)
                                    .transform_rect_bbox(bounds)
                                    .inflate(2.0, 2.0);
                                let bounds = glyph_xform
                                    .unwrap_or(Affine::IDENTITY)
                                    .transform_rect_bbox(bounds);
                                let run_ink = Rect::new(
                                    positions.x0 + bounds.x0,
                                    positions.y0 + bounds.y0,
                                    positions.x1 + bounds.x1,
                                    positions.y1 + bounds.y1,
                                );
                                culled = viewport.is_some_and(|viewport| {
                                    outside_viewport(
                                        transform.transform_rect_bbox(run_ink),
                                        viewport,
                                    )
                                });
                                ink = Some(run_ink);
                            }
                        }
                    }
                }
                let run_x0 = glyph_run.offset() as f64;
                let run_x1 = run_x0 + glyph_run.advance() as f64;
                line_runs.push(LineRun {
                    item: item_index,
                    color: text_color,
                    embolden,
                    culled,
                    ink,
                    extent: Rect::new(
                        run_x0,
                        f64::from(line_metrics.block_min_coord),
                        run_x1,
                        f64::from(line_metrics.block_max_coord),
                    ),
                    shadows,
                });

                // Accumulate this run's contribution to each decorating box on its ancestor
                // path. The decoration is drawn once per box after the whole line has been
                // walked (see `flush_line_decorations`), so mixed font sizes within a box
                // produce a single straight line rather than one stepped segment per run.
                let geometry = DecorationRunGeometry {
                    baseline: glyph_run.baseline(),
                    ascent: metrics.ascent,
                    descent: metrics.descent,
                    underline_offset: metrics.underline_offset,
                    underline_size: metrics.underline_size,
                    strikethrough_size: metrics.strikethrough_size,
                    font: font.clone(),
                    font_size,
                    css_font_size,
                };
                let run_node_id = style.brush.id;

                let boxes_start = inked_boxes.len();
                for entry in stack.iter() {
                    if entry.decoration.is_none() {
                        continue;
                    }
                    let idx = match deco_boxes.iter().position(|d| d.node_id == entry.node_id) {
                        Some(idx) => idx,
                        None => {
                            deco_boxes.push(LineDecoration {
                                node_id: entry.node_id,
                                deco: entry.decoration.clone().unwrap(),
                                min_x: f64::INFINITY,
                                max_x: f64::NEG_INFINITY,
                                own: None,
                                first: None,
                            });
                            deco_boxes.len() - 1
                        }
                    };
                    let acc = &mut deco_boxes[idx];
                    acc.min_x = acc.min_x.min(run_x0);
                    acc.max_x = acc.max_x.max(run_x1);
                    if acc.first.is_none() {
                        acc.first = Some(geometry.clone());
                    }
                    // A run whose innermost node is the box itself is the box's own text, so
                    // its font is the one Firefox positions and sizes the decoration with.
                    if entry.node_id == run_node_id {
                        acc.own = Some(geometry.clone());
                    }
                    if acc
                        .deco
                        .line
                        .intersects(TextDecorationLine::UNDERLINE | TextDecorationLine::OVERLINE)
                    {
                        inked_boxes.push(idx);
                    }
                }

                // Keep the glyphs an underline or overline over this run skips.
                // CJK text keeps the line whole, as in Blink.
                let skips_ink = inked_boxes.len() > boxes_start
                    && !text.get(run.text_range()).is_some_and(|run_text| {
                        run_text.chars().any(crate::ink_skip::never_skips_ink)
                    });
                if skips_ink {
                    let excluded = ink_skip.excluded_glyphs(font);
                    let glyphs_start = inked_glyphs.len();
                    inked_glyphs.extend(
                        glyph_run
                            .positioned_glyphs()
                            .filter(|glyph| !excluded.contains(&Some(glyph.id)))
                            .map(|glyph| (glyph.id, glyph.x, glyph.y)),
                    );
                    let coords_start = inked_coords.len();
                    inked_coords
                        .extend_from_slice(bytemuck::cast_slice::<_, i16>(run.normalized_coords()));
                    inked_runs.push(InkedRun {
                        font: font.clone(),
                        font_size,
                        skew: synthesis
                            .skew()
                            .map_or(0.0, |angle| angle.to_radians().tan()),
                        glyphs: glyphs_start..inked_glyphs.len(),
                        coords: coords_start..inked_coords.len(),
                        boxes: boxes_start..inked_boxes.len(),
                    });
                } else {
                    inked_boxes.truncate(boxes_start);
                }
            }
        }

        // Decorations stop before the white space that hangs past the line's
        // end, which browsers don't paint.
        let metrics = line.metrics();
        let start = f64::from(metrics.offset + metrics.inline_min_coord);
        let (content_x0, content_x1) = if rtl {
            (
                start + f64::from(metrics.hanging_advance),
                start + f64::from(metrics.advance),
            )
        } else {
            (
                start,
                start + f64::from(metrics.advance - metrics.hanging_advance),
            )
        };
        for acc in deco_boxes.iter_mut() {
            acc.min_x = acc.min_x.max(content_x0.min(acc.max_x));
            acc.max_x = acc.max_x.min(content_x1.max(acc.min_x));
        }

        let inked = InkedLine {
            runs: inked_runs,
            glyphs: inked_glyphs,
            coords: inked_coords,
            boxes: inked_boxes,
        };
        let line_height = f64::from(line_metrics.block_max_coord - line_metrics.block_min_coord);

        // Shadows go beneath all of the line's text, the first of a list on
        // top, and shadow the decorations of the text they belong to, as in
        // Blink and Gecko.
        let mut start = 0;
        while start < line_runs.len() {
            let Some(list) = line_runs[start].shadows.clone() else {
                start += 1;
                continue;
            };
            // This run and the following ones with the same list share one
            // layer per blurred shadow.
            let end = start
                + line_runs[start..]
                    .iter()
                    .take_while(|run| {
                        run.shadows
                            .as_ref()
                            .is_some_and(|other| Arc::ptr_eq(other, &list))
                    })
                    .count();
            let batch = &line_runs[start..end];
            start = end;

            let advance = batch
                .iter()
                .map(|run| run.extent)
                .reduce(|a, b| a.union(b))
                .unwrap_or(Rect::ZERO);
            // The decorations over these runs are shadowed with them, cut at
            // the runs' ends.
            let decoration_area = advance.inflate(0.0, line_height);
            let decorated = deco_boxes
                .iter()
                .any(|deco| deco.max_x > advance.x0 && deco.min_x < advance.x1);
            let decorations_overhang = deco_boxes
                .iter()
                .any(|deco| deco.min_x < advance.x0 || deco.max_x > advance.x1);
            let painted = batch
                .iter()
                .map(|run| {
                    run.ink
                        .unwrap_or_else(|| run.extent.inflate(line_height, line_height))
                })
                .fold(decoration_area, |a, b| a.union(b));

            for shadow in list.iter().rev() {
                if shadow.color.components[3] <= 0.0 {
                    continue;
                }
                let spread = 3.0 * shadow.std_dev;
                let reaches_viewport = |area: Rect| {
                    !viewport.is_some_and(|viewport| {
                        outside_viewport(
                            transform.transform_rect_bbox(
                                (area + shadow.offset).inflate(spread, spread),
                            ),
                            viewport,
                        )
                    })
                };
                let visible = |run: &LineRun| match run.ink {
                    Some(ink) => reaches_viewport(ink),
                    None => !run.culled,
                };
                let shadows_decorations = decorated && reaches_viewport(decoration_area);
                if !shadows_decorations && !batch.iter().any(visible) {
                    continue;
                }

                let blurred = shadow.std_dev > 0.0;
                if blurred {
                    let filter = Filter::single(FilterEffect::blur(shadow.std_dev as f32));
                    scene.push_layer(
                        Fill::NonZero,
                        Mix::Normal,
                        1.0,
                        transform,
                        &(painted + shadow.offset).inflate(spread, spread),
                        Some(Arc::new(filter)),
                        None,
                    );
                }
                let shadow_transform = transform * Affine::translate(shadow.offset);
                for (glyph_run, run) in recorded_runs(&line, batch) {
                    if visible(run) {
                        draw_run_glyphs(
                            scene,
                            &glyph_run,
                            shadow.color,
                            run.embolden,
                            shadow_transform,
                        );
                    }
                }
                if shadows_decorations {
                    if decorations_overhang {
                        scene.push_clip_layer(
                            Fill::NonZero,
                            transform,
                            &(decoration_area + shadow.offset),
                        );
                    }
                    flush_line_decorations(
                        scene,
                        transform,
                        scale,
                        deco_boxes,
                        win_ascent_ratios,
                        inline_root_id,
                        line_metrics.baseline,
                        &inked,
                        ink_skip,
                        Some((shadow.color, shadow.offset)),
                    );
                    if decorations_overhang {
                        scene.pop_layer();
                    }
                }
                if blurred {
                    scene.pop_layer();
                }
            }
        }

        for (glyph_run, run) in recorded_runs(&line, line_runs) {
            if !run.culled {
                draw_run_glyphs(scene, &glyph_run, run.color, run.embolden, transform);
            }
        }
        flush_line_decorations(
            scene,
            transform,
            scale,
            deco_boxes,
            win_ascent_ratios,
            inline_root_id,
            line_metrics.baseline,
            &inked,
            ink_skip,
            None,
        );
    }
}

/// The line's glyph runs that `runs` records, each with its record.
fn recorded_runs<'l, 'r>(
    line: &Line<'l, TextBrush>,
    runs: &'r [LineRun],
) -> impl Iterator<Item = (GlyphRun<'l, TextBrush>, &'r LineRun)> + use<'l, 'r> {
    let mut runs = runs.iter().peekable();
    line.items().enumerate().filter_map(move |(index, item)| {
        let PositionedLayoutItem::GlyphRun(glyph_run) = item else {
            return None;
        };
        runs.next_if(|run| run.item == index)
            .map(|run| (glyph_run, run))
    })
}

/// Draw a glyph run in one colour.
fn draw_run_glyphs(
    scene: &mut impl PaintScene,
    glyph_run: &GlyphRun<'_, TextBrush>,
    color: Color,
    embolden: kurbo::Vec2,
    transform: Affine,
) {
    let run = glyph_run.run();
    let glyph_xform = run
        .synthesis()
        .skew()
        .map(|angle| Affine::skew(angle.to_radians().tan() as f64, 0.0));
    scene.draw_glyphs(
        run.font(),
        run.font_size(),
        !FONT_EMBOLDEN_ENABLED, // hint
        bytemuck::cast_slice(run.normalized_coords()),
        embolden,
        Fill::NonZero,
        &anyrender::Paint::from(color),
        1.0, // alpha
        transform,
        glyph_xform,
        glyph_run.positioned_glyphs().map(|glyph| anyrender::Glyph {
            id: glyph.id as _,
            x: glyph.x,
            y: glyph.y,
        }),
    );
}

/// Whether a painted rect lies wholly outside the viewport. A rect that isn't
/// finite never does.
fn outside_viewport(painted: Rect, viewport: Rect) -> bool {
    painted.is_finite()
        && (painted.x1 < viewport.x0
            || painted.x0 > viewport.x1
            || painted.y1 < viewport.y0
            || painted.y0 > viewport.y1)
}

/// The glyphs a line's underlines and overlines skip.
struct InkedLine<'a> {
    runs: &'a [InkedRun],
    glyphs: &'a [(u32, f32, f32)],
    coords: &'a [i16],
    boxes: &'a [usize],
}

/// How much a face is emboldened, per side, where it has no bold of its own:
/// Skia's fake bold strokes the outline `font_size / 24` wide at 9px,
/// narrowing to `font_size / 32` at 36px (`kStdFakeBoldInterpKeys`), and
/// leaves advances as they are.
fn synthetic_bold(font_size: f64) -> kurbo::Vec2 {
    let t = ((font_size - 9.0) / (36.0 - 9.0)).clamp(0.0, 1.0);
    let ratio = 1.0 / 24.0 + t * (1.0 / 32.0 - 1.0 / 24.0);
    let per_side = font_size * ratio / 2.0;
    kurbo::Vec2::new(per_side, per_side)
}

/// The selection highlight rectangles for the given byte range in a layout,
/// with the line each is on. Uses Parley's Selection type for accurate
/// geometry calculation.
///
/// As in Blink, a line whose end the selection goes past ends in a box as
/// wide as a space in its last run's font, which shows that its line break
/// is selected: after a `<br>` (where Parley draws a narrower box), after a
/// word wrapped within itself, and after the text when the selection
/// `continues` beyond it. A wrap after white space needs none, as the
/// highlighted space shows it.
pub(crate) fn text_selection_rects(
    layout: &Layout<TextBrush>,
    selection_start: usize,
    selection_end: usize,
    continues: bool,
) -> Vec<(usize, Rect)> {
    let mut rects = text_range_rects(layout, selection_start, selection_end);

    let line_count = layout.len();
    for (index, line) in layout.lines().enumerate() {
        let range = line.text_range();
        if range.end <= selection_start || range.start >= selection_end {
            continue;
        }
        let explicit = line.break_reason() == parley::BreakReason::Explicit;
        let metrics = line.metrics();
        let break_selected = if index + 1 == line_count {
            continues && selection_end >= range.end
        } else if explicit {
            selection_end >= range.end
        } else {
            selection_end > range.end && metrics.hanging_advance == 0.0
        };
        if !break_selected {
            continue;
        }
        // The line's content as laid out, justification included.
        let (left, right) = line
            .items()
            .fold((f64::MAX, f64::MIN), |(left, right), item| {
                let (start, width) = match item {
                    PositionedLayoutItem::GlyphRun(run) => (run.offset(), run.advance()),
                    PositionedLayoutItem::InlineBox(inline_box) => (inline_box.x, inline_box.width),
                };
                (
                    left.min(f64::from(start)),
                    right.max(f64::from(start + width)),
                )
            });
        let Some(space) = range
            .end
            .checked_sub(1)
            .and_then(|index| parley::Cluster::from_byte_index(layout, index))
            .and_then(|cluster| space_advance(&cluster.run()))
        else {
            continue;
        };
        if left > right {
            continue;
        }
        let top = f64::from(metrics.block_min_coord.min(metrics.content_block_min_coord));
        let bottom = f64::from(metrics.block_max_coord.max(metrics.content_block_max_coord));
        // Parley's box after a `<br>` gives way to this one.
        for (_, rect) in rects.iter_mut().filter(|(line, _)| *line == index) {
            rect.x1 = rect.x1.min(right).max(rect.x0);
        }
        rects.push((
            index,
            if layout.is_rtl() {
                Rect::new(left - space, top, left, bottom)
            } else {
                Rect::new(right, top, right + space, bottom)
            },
        ));
    }
    rects.retain(|(_, rect)| rect.width() > 0.0);
    rects
}

/// The rectangles of a byte range of a layout's text, with the line each is
/// on, from Parley's selection geometry.
pub(crate) fn text_range_rects(
    layout: &Layout<TextBrush>,
    start: usize,
    end: usize,
) -> Vec<(usize, Rect)> {
    let anchor = Cursor::from_byte_index(layout, start, Affinity::Downstream);
    let focus = Cursor::from_byte_index(layout, end, Affinity::Downstream);
    let mut rects = Vec::new();
    Selection::new(anchor, focus).geometry_with(layout, |rect, line_idx| {
        rects.push((
            line_idx,
            kurbo::Rect::new(rect.x0, rect.y0, rect.x1, rect.y1),
        ));
    });
    rects
}

/// Strokes a line's text, in black where `highlights` cover it, as Blink
/// paints the text of find-in-page matches. `area` holds all of the line's
/// ink.
#[allow(clippy::too_many_arguments)]
pub(crate) fn stroke_highlighted_text(
    scene: &mut impl PaintScene,
    line: Line<'_, TextBrush>,
    doc: &BaseDocument,
    transform: Affine,
    scale: f64,
    inline_root_id: NodeId,
    context: &mut DrawTextContext,
    paint: TextPaint<'_>,
    highlights: &[Rect],
    area: Rect,
) {
    if highlights.is_empty() {
        stroke_text(
            scene,
            std::iter::once(line),
            doc,
            transform,
            scale,
            inline_root_id,
            context,
            paint,
        );
        return;
    }
    let mut outside = BezPath::new();
    let mut inside = BezPath::new();
    outside.extend(area.path_elements(0.1));
    for highlight in highlights {
        outside.extend(highlight.path_elements(0.1));
        inside.extend(highlight.path_elements(0.1));
    }
    scene.push_clip_layer(Fill::EvenOdd, transform, &outside);
    stroke_text(
        scene,
        std::iter::once(line.clone()),
        doc,
        transform,
        scale,
        inline_root_id,
        context,
        paint,
    );
    scene.pop_layer();
    scene.push_clip_layer(Fill::NonZero, transform, &inside);
    stroke_text(
        scene,
        std::iter::once(line),
        doc,
        transform,
        scale,
        inline_root_id,
        context,
        TextPaint {
            text_color: Some(Color::BLACK),
            ..paint
        },
    );
    scene.pop_layer();
}

/// The advance of a space in a run's font, which is how wide Blink draws a
/// selected line break.
fn space_advance(run: &parley::Run<'_, TextBrush>) -> Option<f64> {
    use skrifa::instance::{LocationRef, NormalizedCoord, Size};
    use skrifa::{FontRef, MetadataProvider as _};
    let font = run.font();
    let font_ref = FontRef::from_index(font.data.as_ref(), font.index).ok()?;
    let glyph = font_ref.charmap().map(' ')?;
    let coords: Vec<NormalizedCoord> = bytemuck::cast_slice::<_, i16>(run.normalized_coords())
        .iter()
        .map(|coord| NormalizedCoord::from_bits(*coord))
        .collect();
    font_ref
        .glyph_metrics(Size::new(run.font_size()), LocationRef::new(&coords))
        .advance_width(glyph)
        .map(f64::from)
}
