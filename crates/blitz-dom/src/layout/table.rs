use blitz_traits::node_id::NodeId;
use std::{ops::Range, sync::Arc};

use atomic_refcell::AtomicRefCell;
use markup5ever::local_name;
use style::properties::style_structs::Border;
use style::servo_arc::Arc as ServoArc;
use style::values::specified::box_::{DisplayInside, DisplayOutside};
use style::{
    Atom, computed_values::border_collapse::T as BorderCollapse,
    computed_values::table_layout::T as TableLayout, values::computed::BorderStyle,
};
use taffy::{
    DetailedGridInfo, LayoutPartialTree as _, ResolveOrZero, TrackSizingFunction, style_helpers,
};

use crate::BaseDocument;

use super::damage::{CONSTRUCT_BOX, CONSTRUCT_DESCENDENT, CONSTRUCT_FC};
use super::resolve_calc_value;

pub struct TableTreeWrapper<'doc> {
    pub(crate) doc: &'doc mut BaseDocument,
    pub(crate) ctx: Arc<TableContext>,
}

#[derive(Debug, Clone)]
pub struct TableContext {
    pub style: taffy::Style<Atom>,
    pub cells: Vec<TableCell>,
    pub rows: Vec<TableRow>,
    pub is_fixed: bool,
    pub computed_grid_info: AtomicRefCell<Option<DetailedGridInfo<Atom>>>,
    pub border_style: Option<ServoArc<Border>>,
    pub border_collapse: BorderCollapse,
}

// #[derive(Debug, Clone, Eq, PartialEq)]
// pub enum TableItemKind {
//     Row,
//     Cell,
// }

#[derive(Debug, Clone)]
pub struct TableCell {
    // kind: TableItemKind,
    node_id: NodeId,
    style: taffy::Style<Atom>,
}

#[derive(Debug, Clone)]
pub struct TableRow {
    // kind: TableItemKind,
    pub node_id: NodeId,
    pub height: f32,
}

pub(crate) fn build_table_context(
    doc: &mut BaseDocument,
    table_root_node_id: NodeId,
) -> (TableContext, Vec<NodeId>) {
    let mut cells: Vec<TableCell> = Vec::new();
    let mut rows: Vec<TableRow> = Vec::new();
    let mut row = 0u16;
    let mut col = 0u16;

    let root_node = &mut doc.nodes[table_root_node_id];

    let children = std::mem::take(&mut root_node.children);

    let Some(stylo_styles) = root_node.primary_styles() else {
        panic!("Ignoring table because it has no styles");
    };

    let mut style = stylo_taffy::to_taffy_style(&stylo_styles);
    style.item_is_table = true;
    // Use `dense` row-flow so that each cell scans the row from its
    // leftmost column for the first free track. Without `dense`,
    // `place_definite_secondary_axis_item` keeps a per-item secondary
    // cursor across rows, which means cells in later rows do not
    // backfill columns freed up by rowspan cells from earlier rows.
    style.grid_auto_flow = taffy::GridAutoFlow::RowDense;
    style.grid_auto_columns = Vec::new();
    style.grid_auto_rows = Vec::new();

    let is_fixed = match stylo_styles.clone_table_layout() {
        TableLayout::Fixed => true,
        TableLayout::Auto => false,
    };

    let border_collapse = stylo_styles.clone_border_collapse();
    let border_spacing = stylo_styles.clone_border_spacing().0;
    let table_border = stylo_styles.clone_border();

    drop(stylo_styles);

    let mut column_sizes: Vec<taffy::TrackSizingFunction> = Vec::new();
    let mut first_cell_border: Option<ServoArc<Border>> = None;
    for child_id in children.iter().copied() {
        collect_table_cells(
            doc,
            child_id,
            is_fixed,
            border_collapse,
            &mut row,
            &mut col,
            &mut cells,
            &mut rows,
            &mut column_sizes,
            &mut first_cell_border,
        );
    }
    column_sizes.resize(
        col as usize,
        if is_fixed {
            style_helpers::auto()
        } else {
            auto_table_column()
        },
    );

    style.grid_template_columns = column_sizes.into_iter().map(|dim| dim.into()).collect();
    style.grid_template_rows = vec![style_helpers::auto(); row as usize];

    style.gap = match border_collapse {
        BorderCollapse::Separate => {
            // In the separated borders model, `border-spacing` also applies between
            // the table border and the outermost cells, in addition to between cells.
            let spacing_x = border_spacing.width.px();
            let spacing_y = border_spacing.height.px();
            let padding = style.padding.resolve_or_zero(None, resolve_calc_value);
            style.padding = taffy::Rect {
                left: style_helpers::length(padding.left + spacing_x),
                right: style_helpers::length(padding.right + spacing_x),
                top: style_helpers::length(padding.top + spacing_y),
                bottom: style_helpers::length(padding.bottom + spacing_y),
            };
            taffy::Size {
                width: style_helpers::length(spacing_x),
                height: style_helpers::length(spacing_y),
            }
        }
        // Collapsed borders are shared at row/column boundaries; they are not
        // CSS grid gaps. In particular, a `none` border still computes a
        // medium width, so turning that latent width into a gap fragments
        // otherwise borderless layout tables. Visible borders retain their
        // own box geometry below instead of being synthesized from a gap.
        BorderCollapse::Collapse => taffy::Size::ZERO.map(style_helpers::length),
    };

    if border_collapse == BorderCollapse::Collapse {
        let visible_width = |border_style: BorderStyle, width: app_units::Au| {
            if border_style.none_or_hidden() {
                0.0
            } else {
                width.to_f32_px()
            }
        };
        style.border = taffy::Rect {
            left: style_helpers::length(visible_width(
                table_border.border_left_style,
                table_border.border_left_width.0,
            )),
            right: style_helpers::length(visible_width(
                table_border.border_right_style,
                table_border.border_right_width.0,
            )),
            top: style_helpers::length(visible_width(
                table_border.border_top_style,
                table_border.border_top_width.0,
            )),
            bottom: style_helpers::length(visible_width(
                table_border.border_bottom_style,
                table_border.border_bottom_width.0,
            )),
        };
    }

    let layout_children = cells.iter().map(|cell| cell.node_id).collect();
    let root_node = &mut doc.nodes[table_root_node_id];
    root_node.children = children;

    (
        TableContext {
            style,
            cells,
            rows,
            is_fixed,
            computed_grid_info: AtomicRefCell::new(None),
            border_collapse,
            border_style: first_cell_border,
        },
        layout_children,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn collect_table_cells(
    doc: &mut BaseDocument,
    node_id: NodeId,
    is_fixed: bool,
    border_collapse: BorderCollapse,
    row: &mut u16,
    col: &mut u16,
    cells: &mut Vec<TableCell>,
    rows: &mut Vec<TableRow>,
    columns: &mut Vec<TrackSizingFunction>,
    first_cell_border: &mut Option<ServoArc<Border>>,
) {
    let node = &mut doc.nodes[node_id];

    if !node.is_element() {
        return;
    }

    let Some(display) = node.primary_styles().map(|s| s.clone_display()) else {
        #[cfg(feature = "tracing")]
        tracing::info!("Ignoring table descendent because it has no styles");
        return;
    };

    if display.outside() == DisplayOutside::None {
        node.remove_damage(CONSTRUCT_DESCENDENT | CONSTRUCT_FC | CONSTRUCT_BOX);
        return;
    }

    match display.inside() {
        DisplayInside::TableRowGroup
        | DisplayInside::TableHeaderGroup
        | DisplayInside::TableFooterGroup
        | DisplayInside::Contents => {
            let children = std::mem::take(&mut doc.nodes[node_id].children);
            for child_id in children.iter().copied() {
                doc.nodes[child_id]
                    .remove_damage(CONSTRUCT_DESCENDENT | CONSTRUCT_FC | CONSTRUCT_BOX);
                collect_table_cells(
                    doc,
                    child_id,
                    is_fixed,
                    border_collapse,
                    row,
                    col,
                    cells,
                    rows,
                    columns,
                    first_cell_border,
                );
            }
            doc.nodes[node_id].children = children;
        }
        DisplayInside::TableRow => {
            node.remove_damage(CONSTRUCT_DESCENDENT | CONSTRUCT_FC | CONSTRUCT_BOX);
            *row += 1;
            *col = 0;

            rows.push(TableRow {
                node_id,
                height: 0.0,
            });

            let children = std::mem::take(&mut doc.nodes[node_id].children);
            for child_id in children.iter().copied() {
                collect_table_cells(
                    doc,
                    child_id,
                    is_fixed,
                    border_collapse,
                    row,
                    col,
                    cells,
                    rows,
                    columns,
                    first_cell_border,
                );
            }
            doc.nodes[node_id].children = children;
        }
        DisplayInside::TableCell => {
            push_table_cell(
                doc,
                node_id,
                is_fixed,
                border_collapse,
                row,
                col,
                cells,
                columns,
                first_cell_border,
                true,
            );
        }
        DisplayInside::Flow
        | DisplayInside::FlowRoot
        | DisplayInside::Flex
        | DisplayInside::Grid => {
            if display.outside() == DisplayOutside::TableCaption {
                node.remove_damage(CONSTRUCT_DESCENDENT | CONSTRUCT_FC | CONSTRUCT_BOX);
            } else {
                // CSS table fixup wraps an otherwise invalid child in
                // anonymous row/cell boxes. Floats blockify `display:
                // table-cell` elements into flow boxes, which is common in
                // responsive email layouts. Treat the existing node as the
                // anonymous grid cell so its complete subtree still lays out.
                push_table_cell(
                    doc,
                    node_id,
                    is_fixed,
                    border_collapse,
                    row,
                    col,
                    cells,
                    columns,
                    first_cell_border,
                    false,
                );
            }
        }
        DisplayInside::TableColumnGroup | DisplayInside::TableColumn | DisplayInside::Table => {
            node.remove_damage(CONSTRUCT_DESCENDENT | CONSTRUCT_FC | CONSTRUCT_BOX);
            //Ignore
        }
        DisplayInside::None => {
            node.remove_damage(CONSTRUCT_DESCENDENT | CONSTRUCT_FC | CONSTRUCT_BOX);
            // Ignore
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn push_table_cell(
    doc: &mut BaseDocument,
    node_id: NodeId,
    is_fixed: bool,
    border_collapse: BorderCollapse,
    row: &mut u16,
    col: &mut u16,
    cells: &mut Vec<TableCell>,
    columns: &mut Vec<TrackSizingFunction>,
    first_cell_border: &mut Option<ServoArc<Border>>,
    use_spans: bool,
) {
    // A table without an explicit row still gets an anonymous first row.
    if *row == 0 {
        *row = 1;
        *col = 0;
    }

    let node = &mut doc.nodes[node_id];
    let colspan: u16 = use_spans
        .then(|| node.attr(local_name!("colspan")))
        .flatten()
        .and_then(|val| val.parse().ok())
        .unwrap_or(1);
    let rowspan: u16 = use_spans
        .then(|| node.attr(local_name!("rowspan")))
        .flatten()
        .and_then(|val| val.parse::<u16>().ok())
        .map(|v| v.clamp(1, 65534))
        .unwrap_or(1);
    let (mut style, cell_border) = {
        let stylo_style = node.primary_styles().unwrap();
        (
            stylo_taffy::to_taffy_style(&stylo_style),
            stylo_style.clone_border(),
        )
    };

    if first_cell_border.is_none() {
        *first_cell_border = Some(cell_border.clone());
    }

    if *row == 1 {
        let column = if !is_fixed {
            // Widths on cells in an auto-layout table are minimum suggestions,
            // not fixed grid tracks. Allow the track to grow beyond that hint
            // when its content needs more room.
            if style.size.width.tag() == taffy::CompactLength::LENGTH_TAG {
                let len = style.size.width.value();
                let padding = style.padding.resolve_or_zero(None, resolve_calc_value);
                let border = style.border.resolve_or_zero(None, resolve_calc_value);
                let suggested = match style.box_sizing {
                    taffy::BoxSizing::ContentBox => {
                        len + padding.left + padding.right + border.left + border.right
                    }
                    taffy::BoxSizing::BorderBox => len,
                };
                style_helpers::minmax(
                    taffy::MinTrackSizingFunction::length(suggested),
                    taffy::MaxTrackSizingFunction::auto(),
                )
            } else {
                auto_table_column()
            }
        } else {
            match style.size.width.tag() {
                taffy::CompactLength::LENGTH_TAG => {
                    let len = style.size.width.value();
                    let padding = style.padding.resolve_or_zero(None, resolve_calc_value);
                    let border = style.border.resolve_or_zero(None, resolve_calc_value);
                    match style.box_sizing {
                        taffy::BoxSizing::ContentBox => style_helpers::length(
                            len + padding.left + padding.right + border.left + border.right,
                        ),
                        taffy::BoxSizing::BorderBox => style_helpers::length(len),
                    }
                }
                taffy::CompactLength::PERCENT_TAG => {
                    style_helpers::percent(style.size.width.value())
                }
                taffy::CompactLength::AUTO_TAG => style_helpers::auto(),
                // Dimension values are always length, percentage, auto or calc(),
                // so any other tag is a calc() value. Pass it through so that
                // Taffy resolves it against the table's inner width.
                _ => style.size.width.into(),
            }
        };
        columns.push(column);
    }

    // Keep only visible cell borders in their box geometry. CSS computes a
    // non-zero width even when border-style is `none`, but that latent width
    // must occupy no layout space. Blitz does not yet implement the full
    // collapsed-border conflict algorithm; retaining real edges is a faithful
    // fallback for asymmetric borders and avoids dropping them entirely.
    if border_collapse == BorderCollapse::Collapse {
        let border = &cell_border;
        let visible_width = |border_style: BorderStyle, width: app_units::Au| {
            if border_style.none_or_hidden() {
                0.0
            } else {
                width.to_f32_px()
            }
        };
        style.border = taffy::Rect {
            left: style_helpers::length(visible_width(
                border.border_left_style,
                border.border_left_width.0,
            )),
            right: style_helpers::length(visible_width(
                border.border_right_style,
                border.border_right_width.0,
            )),
            top: style_helpers::length(visible_width(
                border.border_top_style,
                border.border_top_width.0,
            )),
            bottom: style_helpers::length(visible_width(
                border.border_bottom_style,
                border.border_bottom_width.0,
            )),
        };
    }

    // The margin properties do not apply to table-internal elements.
    style.margin = taffy::Rect::ZERO.map(style_helpers::length);

    // Let Taffy auto-place the column. Combined with `RowDense` on the table
    // root, each cell scans from the first track in its row for a free slot.
    style.grid_column = taffy::Line {
        start: style_helpers::auto(),
        end: style_helpers::span(colspan),
    };
    style.grid_row = taffy::Line {
        start: style_helpers::line(*row as i16),
        end: style_helpers::span(rowspan),
    };
    style.size.width = style_helpers::auto();
    cells.push(TableCell { node_id, style });

    *col += colspan;
}

fn auto_table_column() -> TrackSizingFunction {
    // The minimum content width of an unbroken identifier may exceed the
    // viewport. Let unconstrained columns shrink so `overflow-wrap:break-word`
    // can wrap those identifiers after the table receives its final width.
    style_helpers::minmax(
        taffy::MinTrackSizingFunction::length(0.0),
        taffy::MaxTrackSizingFunction::auto(),
    )
}

pub struct RangeIter(Range<usize>);

impl Iterator for RangeIter {
    type Item = taffy::NodeId;

    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(taffy::NodeId::from)
    }
}

impl taffy::TraversePartialTree for TableTreeWrapper<'_> {
    type ChildIter<'a>
        = RangeIter
    where
        Self: 'a;

    #[inline(always)]
    fn child_ids(&self, _node_id: taffy::NodeId) -> Self::ChildIter<'_> {
        RangeIter(0..self.ctx.cells.len())
    }

    #[inline(always)]
    fn child_count(&self, _node_id: taffy::NodeId) -> usize {
        self.ctx.cells.len()
    }

    #[inline(always)]
    fn get_child_id(&self, _node_id: taffy::NodeId, index: usize) -> taffy::NodeId {
        index.into()
    }
}
impl taffy::TraverseTree for TableTreeWrapper<'_> {}

impl taffy::LayoutPartialTree for TableTreeWrapper<'_> {
    type CoreContainerStyle<'a>
        = &'a taffy::Style<Atom>
    where
        Self: 'a;

    type CustomIdent = Atom;

    fn get_core_container_style(&self, _node_id: taffy::NodeId) -> &taffy::Style<Atom> {
        &self.ctx.style
    }

    fn resolve_calc_value(&self, calc_ptr: *const (), parent_size: f32) -> f32 {
        resolve_calc_value(calc_ptr, parent_size)
    }

    fn set_unrounded_layout(&mut self, node_id: taffy::NodeId, layout: &taffy::Layout) {
        let node_id = crate::taffy_node_id(self.ctx.cells[usize::from(node_id)].node_id);
        self.doc.set_unrounded_layout(node_id, layout)
    }

    fn compute_child_layout(
        &mut self,
        node_id: taffy::NodeId,
        inputs: taffy::tree::LayoutInput,
    ) -> taffy::LayoutOutput {
        let cell = &self.ctx.cells[usize::from(node_id)];
        let node_id = crate::taffy_node_id(cell.node_id);
        // The document layout pass restores the authored cell width after the
        // table context is built. For auto table sizing, that width is a track
        // suggestion, not a constraint on measuring the cell's contents.
        let width = self.doc.nodes[cell.node_id].style().size.width;
        let measure_without_width =
            !self.ctx.is_fixed && width.tag() == taffy::CompactLength::LENGTH_TAG;
        if measure_without_width {
            self.doc.nodes[cell.node_id].style_mut().size.width = style_helpers::auto();
        }
        let output = self.doc.compute_child_layout(node_id, inputs);
        if measure_without_width {
            self.doc.nodes[cell.node_id].style_mut().size.width = width;
        }
        output
    }
}

impl taffy::LayoutGridContainer for TableTreeWrapper<'_> {
    type GridContainerStyle<'a>
        = &'a taffy::Style<Atom>
    where
        Self: 'a;

    type GridItemStyle<'a>
        = &'a taffy::Style<Atom>
    where
        Self: 'a;

    fn get_grid_container_style(&self, node_id: taffy::NodeId) -> Self::GridContainerStyle<'_> {
        self.get_core_container_style(node_id)
    }

    fn get_grid_child_style(&self, child_node_id: taffy::NodeId) -> Self::GridItemStyle<'_> {
        &self.ctx.cells[usize::from(child_node_id)].style
    }

    fn set_detailed_grid_info(
        &mut self,
        _node_id: taffy::NodeId,
        detailed_grid_info: DetailedGridInfo<Atom>,
    ) {
        *self.ctx.computed_grid_info.borrow_mut() = Some(detailed_grid_info);
    }
}
