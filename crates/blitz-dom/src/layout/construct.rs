use blitz_traits::node_id::NodeId;
use core::str;
use std::sync::Arc;

use markup5ever::{QualName, local_name, ns};
use parley::{
    FontContext, InlineBox, InlineBoxKind, LayoutContext, StyleProperty, TreeBuilder,
    WhiteSpaceCollapse,
};
use style::{
    computed_values::position::T as PositionProperty,
    data::ElementData as StyloElementData,
    shared_lock::StylesheetGuards,
    values::{
        computed::{Content, ContentItem, Display, Float},
        specified::box_::{DisplayInside, DisplayOutside},
    },
};
use thin_vec::ThinVec;

use crate::{
    BaseDocument, ElementData, Node, NodeData,
    layout::damage::{
        CONSTRUCT_BOX, CONSTRUCT_DESCENDENT, CONSTRUCT_FC, sort_layout_children_by_order,
    },
    node::{
        GeneratedText, INLINE_END_EDGE, INLINE_START_EDGE, InlineSlice, ListItemLayout,
        ListItemLayoutPosition, Marker, NodeFlags, NodeKind, SpecialElementData, TextBrush,
        TextInputData, TextLayout, is_inline_edge_char,
    },
    qual_name, stylo_to_parley,
    traversal::{iter_children, iter_children_and_pseudos},
};

use super::{
    damage::ALL_DAMAGE,
    list::{BULLET_FONT_FAMILY, number_list_items},
    replaced::is_inline_box_element,
    table::build_table_context,
    text_transform::{CaseTransform, TextTransformer},
};

const DUMMY_NAME: QualName = qual_name!("div", html);

#[derive(Clone)]
pub(crate) struct ConstructionTask {
    pub(crate) node_id: NodeId,
    pub(crate) data: ConstructionTaskData,
}

pub(crate) struct ConstructionTaskResult {
    pub(crate) node_id: NodeId,
    pub(crate) data: ConstructionTaskResultData,
}

#[derive(Clone)]
pub(crate) enum ConstructionTaskData {
    InlineLayout(Box<TextLayout>),
}

pub(crate) enum ConstructionTaskResultData {
    InlineLayout(Box<TextLayout>),
}

/// Accumulator threaded through layout-child collection.
///
/// `children` is the list of layout children being built up, and
/// `anonymous_block_id` is the currently-open anonymous block container (if
/// any) that wrapping children are being appended to.
#[derive(Default)]
pub(crate) struct LayoutChildren {
    pub(crate) children: ThinVec<NodeId>,
    pub(crate) anonymous_block_id: Option<NodeId>,
    /// All anonymous blocks created while collecting these layout children.
    ///
    /// These are recorded on the container node so they can be deallocated the
    /// next time it is reconstructed.
    pub(crate) anonymous_blocks: ThinVec<NodeId>,
    /// The currently-open anonymous table that table parts outside a table
    /// are appended to.
    pub(crate) anonymous_table_id: Option<NodeId>,
}

impl LayoutChildren {
    /// Append a single layout child.
    fn push(&mut self, child_id: NodeId, doc: &mut BaseDocument) {
        self.maybe_push_anon_block(doc);
        self.anonymous_table_id = None;
        self.children.push(child_id);
    }

    /// Append all layout children in `slice`.
    fn extend(&mut self, slice: &[NodeId], doc: &mut BaseDocument) {
        self.maybe_push_anon_block(doc);
        self.anonymous_table_id = None;
        self.children.extend_from_slice(slice);
    }

    /// Append a table part found outside a table to the open anonymous table,
    /// creating one if there is none (CSS 2.1 §17.2.1): a table's rows or
    /// cells whose table is `display: block`, or `display: table-cell` boxes
    /// without a table, lay out as a table rather than side by side.
    fn push_table_part(
        &mut self,
        container_node_id: NodeId,
        child_id: NodeId,
        doc: &mut BaseDocument,
    ) {
        self.maybe_push_anon_block(doc);
        let table_id = match self.anonymous_table_id {
            Some(table_id) => table_id,
            None => {
                use style::selector_parser::PseudoElement;
                let table_id = create_anonymous_box_for(
                    doc,
                    container_node_id,
                    &PseudoElement::ServoAnonymousTable,
                );
                self.children.push(table_id);
                self.anonymous_blocks.push(table_id);
                self.anonymous_table_id = Some(table_id);
                table_id
            }
        };
        doc.nodes[table_id].children.push(child_id);
    }

    fn maybe_push_anon_block(&mut self, doc: &mut BaseDocument) {
        fn block_is_only_whitespace(doc: &BaseDocument, node_id: NodeId) -> bool {
            let slices = doc.nodes[node_id]
                .element_data()
                .map(|element| &element.inline_slices[..])
                .unwrap_or_default();
            doc.nodes[node_id]
                .children
                .iter()
                .all(|child_id| holds_only_whitespace(doc, slices, *child_id))
        }

        // If anonymous block node only contains whitespace then delete it
        if let Some(anon_id) = self.anonymous_block_id {
            if block_is_only_whitespace(doc, anon_id) {
                // Remove by identity, not pop(): hoisted display:contents
                // children may have been pushed after the anon block.
                if let Some(pos) = self.children.iter().rposition(|id| *id == anon_id) {
                    self.children.remove(pos);
                }
                self.anonymous_blocks.retain(|id| *id != anon_id);
                doc.remove_node_from_tree(anon_id);
            }
        }

        self.anonymous_block_id = None;
    }

    fn push_wrapped(
        &mut self,
        container_node_id: NodeId,
        child_id: NodeId,
        doc: &mut BaseDocument,
    ) {
        self.anonymous_table_id = None;
        if self.anonymous_block_id.is_none() {
            self.create_anonymous_block(container_node_id, doc);
        }
        doc.nodes[self.anonymous_block_id.unwrap()]
            .children
            .push(child_id);
    }

    fn create_anonymous_block(&mut self, container_node_id: NodeId, doc: &mut BaseDocument) {
        let node_id = create_anonymous_box(doc, container_node_id);
        self.children.push(node_id);
        self.anonymous_block_id = Some(node_id);
        self.anonymous_blocks.push(node_id);
    }

    /// Record the part of a split inline element that the open anonymous
    /// block holds.
    fn push_slice(&mut self, slice: InlineSlice, doc: &mut BaseDocument) {
        if let Some(element) = self
            .anonymous_block_id
            .and_then(|anon_id| doc.nodes[anon_id].element_data_mut())
        {
            element.inline_slices.push(slice);
        }
    }
}

/// An inline element's boxes in order: its `::before`, its children and its
/// `::after`.
fn inline_box_children(node: &Node) -> impl Iterator<Item = NodeId> + '_ {
    node.before()
        .into_iter()
        .chain(node.children.iter().copied())
        .chain(node.after())
}

/// The boxes of `node` in an anonymous block holding `slices`: all of them,
/// unless a block splits the element and the anonymous block holds a part.
fn sliced_box_children<'a>(
    node: &'a Node,
    slices: &[InlineSlice],
) -> impl Iterator<Item = NodeId> + 'a {
    let (start, end) = slices
        .iter()
        .find(|slice| slice.element == node.id)
        .map_or((0, usize::MAX), |slice| (slice.start, slice.end));
    inline_box_children(node)
        .skip(start)
        .take(end.saturating_sub(start))
}

/// Whether a box in an anonymous block lays out to nothing: collapsible white
/// space, or a part of a split element that holds only that and neither edge
/// of the element, as an empty line box has no height (CSS 2.1 section 9.4.2).
fn holds_only_whitespace(doc: &BaseDocument, slices: &[InlineSlice], node_id: NodeId) -> bool {
    let node = &doc.nodes[node_id];
    if node.is_collapsible_whitespace_node() {
        return true;
    }
    let Some(slice) = slices.iter().find(|slice| slice.element == node_id) else {
        return false;
    };
    let (start_edge, end_edge) = node
        .primary_styles()
        .map(|style| inline_edge_widths(&style))
        .unwrap_or_default();
    if (slice.first && start_edge != 0.0) || (slice.last && end_edge != 0.0) {
        return false;
    }
    sliced_box_children(node, slices).all(|child_id| {
        doc.nodes[child_id].data.kind() == NodeKind::Comment
            || holds_only_whitespace(doc, slices, child_id)
    })
}

/// Whether `node` is an inline element that holds an in-flow block, which
/// splits it in two around the block (CSS 2.1 section 9.2.1.1).
fn holds_block_inline(node: &Node) -> bool {
    let Some(element) = node.element_data() else {
        return false;
    };
    let display = node.display_style().unwrap_or(Display::inline());
    display.outside() == DisplayOutside::Inline
        && display.inside() == DisplayInside::Flow
        && !is_inline_box_element(&element.name.local)
        && node.is_or_contains_block()
}

/// Whether the container splits the inline element `node_id` around the
/// blocks it holds. One with nothing else to lay out on lines, such as a link
/// around a block image, lays out as browsers lay it out as a block box,
/// which keeps a box of its own for the reader's links and anchors.
fn splits_around_block(doc: &BaseDocument, node_id: NodeId) -> bool {
    let node = &doc.nodes[node_id];
    holds_block_inline(node) && holds_inline_content(doc, node)
}

/// Whether an inline element holds anything but blocks: content for its
/// lines, or an edge of its own.
fn holds_inline_content(doc: &BaseDocument, node: &Node) -> bool {
    let (start_edge, end_edge) = node
        .primary_styles()
        .map(|style| inline_edge_widths(&style))
        .unwrap_or_default();
    if start_edge != 0.0 || end_edge != 0.0 {
        return true;
    }
    inline_box_children(node).any(|child_id| {
        let child = &doc.nodes[child_id];
        match child.data.kind() {
            NodeKind::Comment => false,
            NodeKind::Text => !child.is_collapsible_whitespace_node(),
            _ => {
                if child
                    .display_style()
                    .is_some_and(|display| display.inside() == DisplayInside::None)
                {
                    false
                } else if holds_block_inline(child) {
                    holds_inline_content(doc, child)
                } else {
                    !child.is_or_contains_block()
                }
            }
        }
    })
}

/// A part of a split inline element whose boxes are being collected.
struct OpenSlice {
    element: NodeId,
    start: usize,
    first: bool,
    /// The box being collected, which for all but the innermost part is the
    /// split element inside it.
    current: usize,
}

/// Collect an inline element that holds an in-flow block as CSS 2.1 section
/// 9.2.1.1 lays it out, as browsers do. Each block inside it is a layout child
/// of the container, between anonymous blocks that hold the inline content
/// around the block, with the parts of the element before and after the
/// block, which the anonymous blocks record as slices. So text before the
/// element and its own text before the block share a line, as do its text
/// after the block and the content after the element, and the element's
/// padding and borders stay on its lines instead of going around the block.
fn push_split_inline(
    doc: &mut BaseDocument,
    container_node_id: NodeId,
    element_id: NodeId,
    out: &mut LayoutChildren,
) {
    out.push_wrapped(container_node_id, element_id, doc);
    collect_split_inline(doc, container_node_id, element_id, out, &mut Vec::new());
}

fn collect_split_inline(
    doc: &mut BaseDocument,
    container_node_id: NodeId,
    element_id: NodeId,
    out: &mut LayoutChildren,
    open: &mut Vec<OpenSlice>,
) {
    flush_pseudo_elements(doc, element_id);
    open.push(OpenSlice {
        element: element_id,
        start: 0,
        first: true,
        current: 0,
    });
    let boxes: Vec<NodeId> = inline_box_children(&doc.nodes[element_id]).collect();
    for (index, child_id) in boxes.iter().copied().enumerate() {
        let child = &doc.nodes[child_id];
        if !child.is_or_contains_block() {
            continue;
        }
        let is_block = child
            .display_style()
            .is_some_and(|display| display.outside() == DisplayOutside::Block);
        open.last_mut().unwrap().current = index;
        if holds_block_inline(child) {
            collect_split_inline(doc, container_node_id, child_id, out, open);
            continue;
        }
        if !is_block {
            continue;
        }
        // The block ends the parts of the open elements before it...
        let innermost = open.len() - 1;
        for (depth, part) in open.iter().enumerate() {
            let end = if depth == innermost {
                index
            } else {
                part.current + 1
            };
            out.push_slice(
                InlineSlice {
                    element: part.element,
                    start: part.start,
                    end,
                    first: part.first,
                    last: false,
                },
                doc,
            );
        }
        out.push(child_id, doc);
        // ...and their parts after it go into a new anonymous block.
        for (depth, part) in open.iter_mut().enumerate() {
            part.start = if depth == innermost {
                index + 1
            } else {
                part.current
            };
            part.first = false;
        }
        out.push_wrapped(container_node_id, open[0].element, doc);
    }
    let part = open.pop().unwrap();
    out.push_slice(
        InlineSlice {
            element: element_id,
            start: part.start,
            end: boxes.len(),
            first: part.first,
            last: true,
        },
        doc,
    );
}

/// A new anonymous block box in `container_node_id`, styled as an anonymous
/// box that inherits from it. The caller gives it its children and owns it:
/// it must be deallocated when the container is reconstructed.
pub(crate) fn create_anonymous_box(doc: &mut BaseDocument, container_node_id: NodeId) -> NodeId {
    use style::selector_parser::PseudoElement;
    create_anonymous_box_for(doc, container_node_id, &PseudoElement::ServoAnonymousBox)
}

/// A new anonymous box in `container_node_id`, styled as the anonymous box
/// `pseudo` (an anonymous block or table) that inherits from it.
fn create_anonymous_box_for(
    doc: &mut BaseDocument,
    container_node_id: NodeId,
    pseudo: &style::selector_parser::PseudoElement,
) -> NodeId {
    const NAME: QualName = QualName {
        prefix: None,
        ns: ns!(html),
        local: local_name!("div"),
    };
    let node_id = doc.create_node(NodeData::AnonymousBlock(Box::new(ElementData::new(
        NAME,
        Vec::new(),
    ))));

    // Set style data
    let parent_style = doc.nodes[container_node_id].primary_styles().unwrap();
    let read_guard = doc.guard.read();
    let guards = StylesheetGuards::same(&read_guard);
    let style = doc
        .stylist
        .style_for_anonymous::<&Node>(&guards, pseudo, &parent_style);
    let mut stylo_element_data = StyloElementData {
        damage: ALL_DAMAGE,
        ..Default::default()
    };
    drop(parent_style);

    stylo_element_data.styles.primary = Some(style);
    stylo_element_data.set_restyled();

    *doc.nodes[node_id]
        .stylo_element_data_mut()
        .ensure_init_mut() = stylo_element_data;

    if doc.nodes[container_node_id]
        .flags
        .contains(NodeFlags::IS_IN_DOCUMENT)
    {
        doc.nodes[node_id].flags.insert(NodeFlags::IS_IN_DOCUMENT);
    }
    doc.nodes[node_id].parent = Some(container_node_id);
    doc.nodes[node_id]
        .layout_parent
        .set(Some(container_node_id));
    node_id
}

fn push_children_and_pseudos(layout_children: &mut ThinVec<NodeId>, node: &Node) {
    if let Some(before) = node.before() {
        layout_children.push(before);
    }
    layout_children.extend(node.children.iter().copied().filter(|child_id| {
        let child_node = node.with(*child_id);
        child_node.data.kind() != NodeKind::Comment
    }));
    if let Some(after) = node.after() {
        layout_children.push(after);
    }
}

/// Wrapping policy of the nearest non-contents ancestor container. Children
/// hoisted out of display:contents nodes must be wrapped in anonymous blocks
/// exactly as if they were direct children of that container: without this,
/// a bare text node could end up as the layout child of a block/flex/grid
/// container, which cannot be laid out (text nodes carry no style).
#[derive(Copy, Clone)]
struct WrapContext {
    /// The container whose style anonymous blocks inherit from.
    container_node_id: NodeId,
    needs_wrap: fn(NodeKind, DisplayOutside) -> bool,
    /// The container is a flow container, which splits inline elements that
    /// hold blocks.
    splits_inlines: bool,
}

fn block_item_needs_wrap(child_node_kind: NodeKind, display_outside: DisplayOutside) -> bool {
    child_node_kind == NodeKind::Text || display_outside == DisplayOutside::Inline
}

/// Used for flex/grid containers, and for flow containers whose in-flow
/// children are all out-of-flow (where inline elements are pushed raw, but
/// bare text still cannot be laid out directly).
fn text_item_needs_wrap(child_node_kind: NodeKind, _display_outside: DisplayOutside) -> bool {
    child_node_kind == NodeKind::Text
}

/// Push a single hoisted child, recursing through display:contents nodes and
/// wrapping text/inline children per the ancestor container's `WrapContext`.
fn push_hoisted_child(
    doc: &mut BaseDocument,
    child_id: NodeId,
    out: &mut LayoutChildren,
    wrap: Option<WrapContext>,
) {
    let child = &doc.nodes[child_id];
    let child_display = child.display_style().unwrap_or(Display::inline());
    if matches!(child_display.inside(), DisplayInside::Contents) {
        collect_layout_children_with_wrap(doc, child_id, out, wrap);
        return;
    }
    if let Some(wrap_ctx) = wrap {
        if wrap_ctx.splits_inlines && splits_around_block(doc, child_id) {
            push_split_inline(doc, wrap_ctx.container_node_id, child_id, out);
            return;
        }
        let child_node_kind = child.data.kind();
        let display_outside = if child.is_or_contains_block() {
            DisplayOutside::Block
        } else {
            child_display.outside()
        };
        if (wrap_ctx.needs_wrap)(child_node_kind, display_outside) {
            out.push_wrapped(wrap_ctx.container_node_id, child_id, doc);
            return;
        }
    }
    out.push(child_id, doc);
}

/// Push the container's children (and ::before/::after pseudos) as layout
/// children, hoisting transparently through display:contents nodes and
/// filtering out comments and whitespace.
fn push_hoisted_children_and_pseudos(
    doc: &mut BaseDocument,
    container_node_id: NodeId,
    out: &mut LayoutChildren,
    wrap: Option<WrapContext>,
) {
    if let Some(before) = doc.nodes[container_node_id].before() {
        push_hoisted_child(doc, before, out, wrap);
    }
    // Take children array from node to avoid borrow checker issues.
    let children = std::mem::take(&mut doc.nodes[container_node_id].children);
    for child_id in children.iter().copied() {
        let child = &doc.nodes[child_id];
        if child.data.kind() == NodeKind::Comment || child.is_collapsible_whitespace_node() {
            continue;
        }
        push_hoisted_child(doc, child_id, out, wrap);
    }
    doc.nodes[container_node_id].children = children;
    if let Some(after) = doc.nodes[container_node_id].after() {
        push_hoisted_child(doc, after, out, wrap);
    }
}

fn push_non_whitespace_children_and_pseudos(layout_children: &mut ThinVec<NodeId>, node: &Node) {
    if let Some(before) = node.before() {
        layout_children.push(before);
    }
    layout_children.extend(node.children.iter().copied().filter(|child_id| {
        let child_node = node.with(*child_id);
        !child_node.is_whitespace_node() && child_node.data.kind() != NodeKind::Comment
    }));
    if let Some(after) = node.after() {
        layout_children.push(after);
    }
}

/// Result of classifying the in-flow children of a flow container as
/// all-block, all-inline and/or all-out-of-flow.
struct FlowClassification {
    all_block: bool,
    all_inline: bool,
    all_out_of_flow: bool,
    has_contents: bool,
    /// An in-flow child is a table part, which needs an anonymous table.
    has_table_parts: bool,
}

impl Default for FlowClassification {
    fn default() -> Self {
        Self {
            all_block: true,
            all_inline: true,
            all_out_of_flow: true,
            has_contents: false,
            has_table_parts: false,
        }
    }
}

/// Classify a flow container's children (including its ::before/::after
/// pseudo-elements) for inline-vs-block layout, recursing transparently
/// through display:contents nodes (whose children participate in the
/// container's formatting context).
fn classify_flow_children(
    doc: &BaseDocument,
    container_node_id: NodeId,
    classification: &mut FlowClassification,
) {
    let node = &doc.nodes[container_node_id];
    // ::before/::after pseudos with display:contents are transparent for box
    // generation, so their children (e.g. generated text) participate in the
    // container's formatting context and vote in the classification. Pseudos
    // with any other display value generate their own box, which every
    // construction arm already pushes explicitly, so they cast no vote.
    let pseudo_ids = node
        .before()
        .into_iter()
        .chain(node.after())
        .filter(|pe_id| {
            let display = doc.nodes[*pe_id]
                .display_style()
                .unwrap_or(Display::inline());
            matches!(display.inside(), DisplayInside::Contents)
        });
    let child_ids = node.children.iter().copied().chain(pseudo_ids);
    for child_id in child_ids {
        let child = &doc.nodes[child_id];

        // Comment nodes generate no boxes and must not affect the
        // inline-vs-block classification: an unstyled comment would
        // default to display:inline below and force an inline
        // formatting context on the container, swallowing element
        // siblings into the inline layout (zero-sizing any
        // out-of-flow ones).
        if child.data.kind() == NodeKind::Comment {
            continue;
        }

        // Unwraps on Text and SVG nodes
        let style = child.primary_styles();
        let style = style.as_ref();
        let display = child.display_style().unwrap_or(Display::inline());
        if matches!(display.inside(), DisplayInside::Contents) {
            // Transparent for box generation: the contents node casts
            // no vote itself — its children decide.
            classification.has_contents = true;
            classify_flow_children(doc, child_id, classification);
        } else if matches!(display.inside(), DisplayInside::None) {
            // display:none children generate no boxes and cast no vote.
            continue;
        } else {
            let position = style
                .map(|s| s.clone_position())
                .unwrap_or(PositionProperty::Static);
            let float = style.map(|s| s.clone_float()).unwrap_or(Float::None);

            // Ignore nodes whose whitespace is entirely collapsed away
            if child.is_collapsible_whitespace_node() {
                continue;
            }

            // display:none children generate no boxes and cast no vote
            if matches!(display.outside(), DisplayOutside::None) {
                continue;
            }

            let is_in_flow = matches!(
                position,
                PositionProperty::Static | PositionProperty::Relative | PositionProperty::Sticky
            ) && matches!(float, Float::None);

            if !is_in_flow {
                continue;
            }

            classification.all_out_of_flow = false;
            match display.outside() {
                DisplayOutside::None => {}
                DisplayOutside::InternalTable => {
                    classification.all_inline = false;
                    classification.has_table_parts = true;
                }
                DisplayOutside::Block | DisplayOutside::TableCaption => {
                    classification.all_inline = false
                }
                DisplayOutside::Inline => {
                    classification.all_block = false;

                    // We need the "complex" tree fixing when an inline contains a block,
                    // unless the block split it and this is the anonymous block of a part.
                    let holds_part = node.element_data().is_some_and(|element| {
                        element
                            .inline_slices
                            .iter()
                            .any(|slice| slice.element == child_id)
                    });
                    if child.is_or_contains_block() && !holds_part {
                        classification.all_inline = false;
                    }
                }
            }
        }
    }
}

pub(crate) fn collect_layout_children(
    doc: &mut BaseDocument,
    container_node_id: NodeId,
    out: &mut LayoutChildren,
) {
    collect_layout_children_with_wrap(doc, container_node_id, out, None)
}

fn collect_layout_children_with_wrap(
    doc: &mut BaseDocument,
    container_node_id: NodeId,
    out: &mut LayoutChildren,
    wrap: Option<WrapContext>,
) {
    // Record the `display` the box is being constructed with
    if let Some(display) = doc.nodes[container_node_id].display_style() {
        *doc.nodes[container_node_id].display_constructed_as_mut() = display;
    }

    // Reset construction flags
    // TODO: make incremental and only remove this if the element is no longer an inline root
    doc.nodes[container_node_id]
        .flags
        .reset_construction_flags();
    if let Some(element_data) = doc.nodes[container_node_id].element_data_mut() {
        element_data.take_inline_layout();
    }

    flush_pseudo_elements(doc, container_node_id);

    if let Some(el) = doc.nodes[container_node_id].data.downcast_element() {
        // Handle text inputs
        let tag_name = el.name.local.as_ref();
        if matches!(tag_name, "input" | "textarea") {
            let type_attr: Option<&str> = doc.nodes[container_node_id]
                .data
                .downcast_element()
                .and_then(|el| el.attr(local_name!("type")));
            if tag_name == "textarea" {
                create_text_editor(doc, container_node_id, true);
                return;
            } else if matches!(
                type_attr,
                None | Some("text" | "password" | "email" | "number" | "search" | "tel" | "url")
            ) {
                create_text_editor(doc, container_node_id, false);
                return;
            } else if matches!(type_attr, Some("checkbox" | "radio")) {
                create_checkbox_input(doc, container_node_id);
                return;
            }
        }

        #[cfg(feature = "svg")]
        if matches!(tag_name, "svg") {
            let outer_html = doc.get_node(container_node_id).unwrap().svg_source();

            // Remove contruction damage from subtree
            doc.iter_subtree_mut(container_node_id, |id: NodeId, doc: &mut BaseDocument| {
                doc.nodes[id].remove_damage(CONSTRUCT_BOX | CONSTRUCT_DESCENDENT | CONSTRUCT_FC);
            });

            match crate::util::parse_svg_image(outer_html.as_bytes()) {
                Ok(svg) => {
                    doc.get_node_mut(container_node_id)
                        .unwrap()
                        .element_data_mut()
                        .unwrap()
                        .special_data =
                        SpecialElementData::Image(Box::new(crate::node::ImageData::Svg(svg)));
                }
                Err(err) => {
                    #[cfg(feature = "tracing")]
                    tracing::warn!(
                        node_id = ?container_node_id,
                        html = outer_html,
                        error = ?err,
                        "SVG parse failed",
                    );
                    #[cfg(not(feature = "tracing"))]
                    let _ = err;
                }
            };
            return;
        }

        let is_details = tag_name == "details";
        number_list_items(doc, container_node_id);

        if is_details && collect_details_children(doc, container_node_id, out) {
            return;
        }
    }

    // Skip further construction if the node has no children or psuedo-children.
    // An inside marker still needs a line of its own.
    {
        let node = &doc.nodes[container_node_id];
        if node.children.is_empty()
            && node.before().is_none()
            && node.after().is_none()
            && inside_marker(node).is_none()
        {
            return;
        }
    }

    let container_display = doc.nodes[container_node_id].display_style().unwrap_or(
        match doc.nodes[container_node_id].data.kind() {
            NodeKind::AnonymousBlock => Display::Block,
            _ => Display::Inline,
        },
    );

    match container_display.inside() {
        DisplayInside::None => {}
        DisplayInside::Contents => {
            doc.nodes[container_node_id]
                .remove_damage(CONSTRUCT_BOX | CONSTRUCT_DESCENDENT | CONSTRUCT_FC);
            // display:contents is transparent for box generation: hoist the
            // children THEMSELVES (not their layout children) into the
            // parent, recursing only through nested contents nodes.
            push_hoisted_children_and_pseudos(doc, container_node_id, out, wrap);
        }
        DisplayInside::Flex | DisplayInside::Grid => {
            // ::before/::after pseudos must be checked too: a pseudo with
            // display:contents hoists its text content into the container.
            let container = &doc.nodes[container_node_id];
            let has_text_node_or_contents = container
                .children
                .iter()
                .copied()
                .chain(container.before())
                .chain(container.after())
                .map(|child_id| &doc.nodes[child_id])
                .any(|child| {
                    let display = child.display_style().unwrap_or(Display::inline());
                    let node_kind = child.data.kind();
                    display.inside() == DisplayInside::Contents || node_kind == NodeKind::Text
                });

            if !has_text_node_or_contents {
                push_non_whitespace_children_and_pseudos(
                    &mut out.children,
                    &doc.nodes[container_node_id],
                );
            } else {
                collect_complex_layout_children(
                    doc,
                    container_node_id,
                    out,
                    true,
                    text_item_needs_wrap,
                );
            }

            // Flex/grid items are laid out in order-modified document order.
            sort_layout_children_by_order(&doc.nodes, &mut out.children);
        }

        DisplayInside::Table => {
            let (table_context, tlayout_children, anonymous_cells) =
                build_table_context(doc, container_node_id);
            out.anonymous_blocks.extend(anonymous_cells);
            #[allow(clippy::arc_with_non_send_sync)]
            let data = SpecialElementData::TableRoot(Arc::new(table_context));
            doc.nodes[container_node_id]
                .flags
                .insert(NodeFlags::IS_TABLE_ROOT);
            doc.nodes[container_node_id]
                .data
                .downcast_element_mut()
                .unwrap()
                .special_data = data;
            if let Some(before) = doc.nodes[container_node_id].before() {
                out.push(before, doc);
            }
            out.extend(&tlayout_children, doc);
            if let Some(after) = doc.nodes[container_node_id].after() {
                out.push(after, doc);
            }
        }

        // Flow, FlowRoot and TableCell, plus internal table displays (row,
        // row group, column, ...) occurring outside of a table. Blitz does
        // not yet generate anonymous table wrapper boxes for the latter, so
        // they are laid out as flow containers, which crucially ensures
        // their text children get wrapped rather than being pushed as bare
        // layout children (text nodes carry no style and cannot be laid out
        // as block/flex/grid items).
        _ => {
            let mut classification = FlowClassification::default();
            classify_flow_children(doc, container_node_id, &mut classification);

            // An inside marker is in flow, so its list item has a line for it.
            if classification.all_out_of_flow
                && inside_marker(&doc.nodes[container_node_id]).is_none()
            {
                // Contents-transparent: a display:contents child may be
                // holding the out-of-flow elements (otherwise the contents
                // node itself would be pushed as a layout box).
                let wrap = Some(WrapContext {
                    container_node_id,
                    needs_wrap: text_item_needs_wrap,
                    splits_inlines: false,
                });
                return push_hoisted_children_and_pseudos(doc, container_node_id, out, wrap);
            }

            // TODO: fix display:contents
            if classification.all_inline {
                let existing_layout = doc.nodes[container_node_id]
                    .element_data_mut()
                    .and_then(|el| el.inline_layout_data.take());
                let layout = existing_layout.unwrap_or_else(|| Box::new(TextLayout::new()));

                // Queue node for inline layout construction. Deferring construction of inline layouts to a
                // dedicated phase allows us to multithread the expensive text shaping step.
                doc.deferred_construction_nodes.push(ConstructionTask {
                    node_id: container_node_id,
                    data: ConstructionTaskData::InlineLayout(layout),
                });
                doc.nodes[container_node_id]
                    .flags
                    .insert(NodeFlags::IS_INLINE_ROOT);

                find_inline_layout_embedded_boxes(doc, container_node_id, &mut out.children);
                return;
            }

            // If the children are either all inline or all block then simply return the regular children
            // as the layout children
            if classification.all_block
                & !classification.has_contents
                & !classification.has_table_parts
            {
                push_non_whitespace_children_and_pseudos(
                    &mut out.children,
                    &doc.nodes[container_node_id],
                );
                return place_inside_marker(doc, container_node_id, out);
            } else if classification.all_inline & !classification.has_contents {
                return push_children_and_pseudos(&mut out.children, &doc.nodes[container_node_id]);
            }

            collect_complex_layout_children(
                doc,
                container_node_id,
                out,
                false,
                block_item_needs_wrap,
            );
            place_inside_marker(doc, container_node_id, out);
        }
    }
}

/// Lays out a `<details>` as its UA shadow tree does (HTML "The details and
/// summary elements"): its first `<summary>` first, wherever it is among the
/// children, and everything else after it in one block, while it is open. A
/// closed one shows only its summary, so its text is hidden along with its
/// elements, which `display: none` alone leaves showing. Returns false for a
/// `<details>` that isn't a block container, which lays out as usual.
fn collect_details_children(
    doc: &mut BaseDocument,
    container_node_id: NodeId,
    out: &mut LayoutChildren,
) -> bool {
    let node = &doc.nodes[container_node_id];
    let is_flow = node.display_style().is_some_and(|display| {
        matches!(
            display.inside(),
            DisplayInside::Flow | DisplayInside::FlowRoot
        )
    });
    if !is_flow {
        return false;
    }
    let summary = node.children.iter().copied().find(|&child| {
        doc.nodes[child]
            .data
            .is_element_with_tag_name(&local_name!("summary"))
    });
    let open = node
        .element_data()
        .is_some_and(|element| element.has_attr(local_name!("open")));
    let content: Vec<NodeId> = node
        .children
        .iter()
        .copied()
        .filter(|&child| Some(child) != summary)
        .collect();
    if let Some(summary) = summary {
        let display = doc.nodes[summary]
            .display_style()
            .unwrap_or(Display::inline());
        if display.outside() == DisplayOutside::Inline {
            out.push_wrapped(container_node_id, summary, doc);
        } else {
            out.push(summary, doc);
        }
    }
    let shows_content = open
        && content
            .iter()
            .any(|&child| !doc.nodes[child].is_collapsible_whitespace_node());
    if shows_content {
        out.maybe_push_anon_block(doc);
        let slot = create_anonymous_box(doc, container_node_id);
        doc.nodes[slot].children = content.into();
        out.children.push(slot);
        out.anonymous_blocks.push(slot);
    }
    out.maybe_push_anon_block(doc);
    true
}

/// The marker of a list item whose `list-style-position` is `inside`.
fn inside_marker(node: &Node) -> Option<&Marker> {
    match node.element_data()?.list_item_data.as_deref()? {
        ListItemLayout {
            marker,
            position: ListItemLayoutPosition::Inside,
        } => Some(marker),
        _ => None,
    }
}

/// An inside marker is an inline box in front of the list item's content
/// (CSS Lists 3, `list-style-position`). A list item that is an inline
/// formatting context of its own draws it in its own text. One whose content
/// starts with an anonymous block of inline content hands the marker to that
/// block. One whose content starts with a block, such as a `<summary>` holding
/// a heading, gets an anonymous block that holds only the marker, which then
/// stands on a line above the block as in browsers.
fn place_inside_marker(
    doc: &mut BaseDocument,
    container_node_id: NodeId,
    out: &mut LayoutChildren,
) {
    let Some(marker) = inside_marker(&doc.nodes[container_node_id]).cloned() else {
        return;
    };
    let line = match out.children.first() {
        Some(&first) if out.anonymous_blocks.contains(&first) => first,
        _ => {
            let open_block = out.anonymous_block_id.take();
            out.create_anonymous_block(container_node_id, doc);
            let line = out.children.pop().unwrap();
            out.children.insert(0, line);
            out.anonymous_block_id = open_block;
            line
        }
    };
    if let Some(element_data) = doc.nodes[line].element_data_mut() {
        element_data.list_item_data = Some(Box::new(ListItemLayout {
            marker,
            position: ListItemLayoutPosition::Inside,
        }));
    }
}

/// Extract the text generated by a pseudo-element's `content` property
/// (only string content items are currently supported; other item types
/// are skipped).
fn pe_content_text(style: &style::properties::ComputedValues) -> Option<String> {
    match &style.get_counters().content {
        Content::Items(item_data) => {
            let items = &item_data.items[0..item_data.alt_start];
            let mut text = String::new();
            let mut has_string = false;
            for item in items {
                // TODO: other types of content
                if let ContentItem::String(owned_str) = item {
                    text.push_str(owned_str);
                    has_string = true;
                }
            }
            has_string.then_some(text)
        }
        _ => None,
    }
}

fn flush_pseudo_elements(doc: &mut BaseDocument, node_id: NodeId) {
    let (before_style, after_style, before_node_id, after_node_id) = {
        let node = &doc.nodes[node_id];

        let before_node_id = node.before();
        let after_node_id = node.after();

        // Note: yes these are kinda backwards
        let style_data = node.try_stylo_element_data().and_then(|s| s.get());
        let before_style = style_data
            .as_ref()
            .and_then(|d| d.styles.pseudos.as_array()[1].clone());
        let after_style = style_data
            .as_ref()
            .and_then(|d| d.styles.pseudos.as_array()[0].clone());

        (before_style, after_style, before_node_id, after_node_id)
    };

    // Sync pseudo element
    // TODO: Make incremental
    for (idx, pe_style, pe_node_id) in [
        (1, before_style, before_node_id),
        (0, after_style, after_node_id),
    ] {
        // Delete psuedo element if it exists but shouldn't
        if let (Some(pe_node_id), None) = (pe_node_id, &pe_style) {
            doc.remove_and_drop_pe(pe_node_id);
            let node = &mut doc.nodes[node_id];
            node.set_pe_by_index(idx, None);
            node.insert_damage(ALL_DAMAGE);
        }

        // Create pseudo element if it should exist but doesn't
        if let (None, Some(pe_style)) = (pe_node_id, &pe_style) {
            let new_node_id = doc.create_node(NodeData::AnonymousBlock(Box::new(
                ElementData::new(DUMMY_NAME, Vec::new()),
            )));
            doc.nodes[new_node_id].parent = Some(node_id);
            doc.nodes[new_node_id].layout_parent.set(Some(node_id));
            if doc.nodes[node_id].flags.contains(NodeFlags::IS_IN_DOCUMENT) {
                doc.nodes[new_node_id]
                    .flags
                    .insert(NodeFlags::IS_IN_DOCUMENT);
            }

            if let Some(text) = pe_content_text(pe_style) {
                let text_node_id = doc.create_text_node(&text);
                doc.nodes[text_node_id].parent = Some(new_node_id);
                doc.nodes[new_node_id].children.push(text_node_id);
            }

            let mut element_data = StyloElementData::default();
            element_data.styles.primary = Some(pe_style.clone());
            element_data.set_restyled();
            element_data.damage = ALL_DAMAGE;
            *doc.nodes[new_node_id]
                .stylo_element_data_mut()
                .ensure_init_mut() = element_data;

            let node = &mut doc.nodes[node_id];
            node.set_pe_by_index(idx, Some(new_node_id));
            node.insert_damage(ALL_DAMAGE);

            doc.pending_style_image_nodes.push(new_node_id);
        }

        // Else: Update psuedo element
        if let (Some(pe_node_id), Some(pe_style)) = (pe_node_id, pe_style) {
            // Sync the pseudo-element's text node with its `content` style, which
            // may have changed (e.g. `details[open] summary::after { content: ... }`).
            //
            // Note: this deliberately compares the text itself rather than relying on
            // the style-pointer comparison below, as the pseudo-element's style may
            // already have been updated by `sync_pseudo_element_styles` during the
            // style traversal without the text having been updated.
            let new_text = pe_content_text(&pe_style);
            let existing_text_node_id = doc.nodes[pe_node_id]
                .children
                .first()
                .copied()
                .filter(|&child_id| doc.nodes[child_id].is_text_node());
            match (existing_text_node_id, new_text) {
                (Some(text_node_id), Some(new_text)) => {
                    let text_data = doc.nodes[text_node_id].text_data_mut().unwrap();
                    if text_data.content != new_text {
                        text_data.content = new_text;
                        doc.nodes[node_id].insert_damage(ALL_DAMAGE);
                    }
                }
                (None, Some(new_text)) => {
                    let text_node_id = doc.create_text_node(&new_text);
                    doc.nodes[text_node_id].parent = Some(pe_node_id);
                    doc.nodes[pe_node_id].children.push(text_node_id);
                    doc.nodes[node_id].insert_damage(ALL_DAMAGE);
                }
                (Some(text_node_id), None) => {
                    doc.nodes[pe_node_id]
                        .children
                        .retain(|&child_id| child_id != text_node_id);
                    doc.remove_node_from_tree(text_node_id);
                    doc.nodes[node_id].insert_damage(ALL_DAMAGE);
                }
                (None, None) => {}
            }

            let mut node_styles = doc.nodes[pe_node_id]
                .try_stylo_element_data_mut()
                .and_then(|s| s.get_mut());
            let node_styles = &mut node_styles.as_mut().unwrap();
            node_styles.damage.insert(ALL_DAMAGE);
            let primary_styles = &mut node_styles.styles.primary;

            if !std::ptr::eq(&**primary_styles.as_ref().unwrap(), &*pe_style) {
                *primary_styles = Some(pe_style);
                node_styles.set_restyled();
                doc.pending_style_image_nodes.push(pe_node_id);
            }
        }
    }
}

/// Handles the cases where there are text nodes or inline nodes that need to be wrapped in an anonymous block node
fn collect_complex_layout_children(
    doc: &mut BaseDocument,
    container_node_id: NodeId,
    out: &mut LayoutChildren,
    hide_whitespace: bool,
    needs_wrap: fn(NodeKind, DisplayOutside) -> bool,
) {
    // Only flow containers wrap table parts in anonymous tables here.
    let wraps_table_parts = !hide_whitespace;
    doc.iter_children_and_pseudos_mut(container_node_id, |child_id, doc| {
        // Get node kind (text, element, comment, etc)
        let child_node_kind = doc.nodes[child_id].data.kind();

        // Get Display style. Default to inline because nodes without styles are probably text nodes
        let contains_block = doc.nodes[child_id].is_or_contains_block();
        let child_display = &doc.nodes[child_id]
            .display_style()
            .unwrap_or(Display::inline());
        let display_inside = child_display.inside();
        let display_outside = if contains_block {
            DisplayOutside::Block
        } else {
            child_display.outside()
        };

        let is_whitespace_node = doc.nodes[child_id].is_whitespace_node();

        // Skip comment nodes. Note that we do *not* skip `Display::None` nodes as they may need to be hidden.
        // Taffy knows how to deal with `Display::None` children.
        //
        // Also hide all-whitespace flexbox children as these should be ignored
        if child_node_kind == NodeKind::Comment || (hide_whitespace && is_whitespace_node) {
            // return;
        }
        // Recurse into `Display::Contents` nodes, wrapping hoisted text and
        // inline children as if they were direct children of this container
        else if display_inside == DisplayInside::Contents {
            let wrap = Some(WrapContext {
                container_node_id,
                needs_wrap,
                splits_inlines: wraps_table_parts,
            });
            collect_layout_children_with_wrap(doc, child_id, out, wrap)
        }
        // Table parts outside a table go into an anonymous table, through
        // the white space between them, in flow containers. (Floated and
        // absolutely positioned ones are blockified.)
        else if wraps_table_parts && child_display.outside() == DisplayOutside::InternalTable {
            out.push_table_part(container_node_id, child_id, doc);
        } else if out.anonymous_table_id.is_some() && is_whitespace_node {
        }
        // An inline element that holds a block splits around it, in flow
        // containers.
        else if wraps_table_parts && splits_around_block(doc, child_id) {
            push_split_inline(doc, container_node_id, child_id, out);
        }
        // Push nodes that need wrapping into the current "anonymous block container".
        // If there is not an open one then we create one.
        else if needs_wrap(child_node_kind, display_outside) {
            out.push_wrapped(container_node_id, child_id, doc);
        }
        // Else push the child directly (and close any open "anonymous block container")
        else {
            out.push(child_id, doc);
        }
    });

    // If anonymous block node only contains whitespace then delete it, else push it
    out.maybe_push_anon_block(doc);
}

fn create_text_editor(doc: &mut BaseDocument, input_element_id: NodeId, is_multiline: bool) {
    let node = &mut doc.nodes[input_element_id];
    let parley_style = node
        .primary_styles()
        .as_ref()
        .map(|s| stylo_to_parley::style(node.id, s))
        .unwrap_or_default();
    let alignment = node
        .primary_styles()
        .map(|s| stylo_to_parley::text_align(s.clone_text_align()))
        .unwrap_or(parley::layout::Alignment::Start);
    let base_direction = node
        .primary_styles()
        .map(|s| stylo_to_parley::base_direction(s.clone_direction(), s.clone_unicode_bidi()))
        .unwrap_or(parley::BaseDirection::Auto);

    let initial_text = if is_multiline {
        node.text_content()
    } else {
        node.attr(local_name!("value")).unwrap_or("").to_string()
    };
    let element = &mut node.data.downcast_element_mut().unwrap();
    if !matches!(element.special_data, SpecialElementData::TextInput(_)) {
        let mut text_input_data = TextInputData::new(is_multiline);
        let editor = &mut text_input_data.editor;
        editor.set_text(&initial_text);
        element.special_data = SpecialElementData::TextInput(text_input_data);
    }

    let SpecialElementData::TextInput(text_input_data) = &mut element.special_data else {
        unreachable!();
    };

    let editor = &mut text_input_data.editor;
    editor.set_scale(doc.viewport.scale_f64() as f32);
    editor.set_width(None);

    let styles = editor.edit_styles();
    styles.retain(|_| false);
    styles.insert(StyleProperty::FontFamily(parley_style.font_family));
    styles.insert(StyleProperty::FontSize(parley_style.font_size));
    styles.insert(StyleProperty::FontWidth(parley_style.font_width));
    styles.insert(StyleProperty::FontStyle(parley_style.font_style));
    styles.insert(StyleProperty::FontWeight(parley_style.font_weight));
    styles.insert(StyleProperty::FontVariations(parley_style.font_variations));
    styles.insert(StyleProperty::FontFeatures(parley_style.font_features));
    styles.insert(StyleProperty::LineHeight(parley_style.line_height));
    styles.insert(StyleProperty::WordSpacing(parley_style.word_spacing));
    styles.insert(StyleProperty::LetterSpacing(parley_style.letter_spacing));
    styles.insert(StyleProperty::WordBreak(parley_style.word_break));
    styles.insert(StyleProperty::OverflowWrap(parley_style.overflow_wrap));
    styles.insert(StyleProperty::TextWrapMode(parley_style.text_wrap_mode));
    styles.insert(StyleProperty::WhiteSpaceCollapse(
        parley_style.white_space_collapse,
    ));
    styles.insert(StyleProperty::Brush(parley_style.brush));
    editor.set_alignment(alignment);
    editor.set_base_direction(base_direction);

    editor.refresh_layout(&mut doc.font_ctx.lock().unwrap(), &mut doc.layout_ctx);
}

fn create_checkbox_input(doc: &mut BaseDocument, input_element_id: NodeId) {
    let node = &mut doc.nodes[input_element_id];

    let element = &mut node.data.downcast_element_mut().unwrap();
    if !matches!(element.special_data, SpecialElementData::CheckboxInput(_)) {
        let checked = element.has_attr(local_name!("checked"));
        element.special_data = SpecialElementData::CheckboxInput(checked);
        element.set_checkbox_input_checked(checked);
    }
}

/// Find and return the "layout_children" (inline boxes) for an inline layout
/// without actually constructing the layout. This allows us to defer the expensive
/// construction of the Parley layout (which invokes text shaping) to a paralell phase.
pub(crate) fn find_inline_layout_embedded_boxes(
    doc: &mut BaseDocument,
    inline_context_root_node_id: NodeId,
    layout_children: &mut ThinVec<NodeId>,
) {
    flush_inline_pseudos_recursive(doc, inline_context_root_node_id);

    let slices = doc.nodes[inline_context_root_node_id]
        .element_data()
        .map(|element| element.inline_slices.clone())
        .unwrap_or_default();
    iter_children_and_pseudos!(doc.nodes[inline_context_root_node_id], |child_id| {
        find_inline_layout_embedded_boxes_recursive(
            &mut doc.nodes,
            inline_context_root_node_id,
            child_id,
            layout_children,
            &slices,
            inline_context_root_node_id,
        );
    });

    fn flush_inline_pseudos_recursive(doc: &mut BaseDocument, node_id: NodeId) {
        doc.iter_children_mut(node_id, |child_id, doc| {
            flush_pseudo_elements(doc, child_id);
            let display = doc.nodes[node_id]
                .display_style()
                .unwrap_or(Display::inline());
            let do_recurse = match (display.outside(), display.inside()) {
                (DisplayOutside::None, DisplayInside::Contents) => true,
                (DisplayOutside::Inline, DisplayInside::Flow) => true,
                (_, _) => false,
            };
            if do_recurse {
                flush_inline_pseudos_recursive(doc, child_id);
            }
        });
    }

    fn find_inline_layout_embedded_boxes_recursive(
        nodes: &mut crate::NodeTree,
        parent_id: NodeId,
        node_id: NodeId,
        layout_children: &mut ThinVec<NodeId>,
        slices: &[InlineSlice],
        root_id: NodeId,
    ) {
        // A split element's layout parent is the first anonymous block that
        // holds a part of it, where it starts.
        let holds_part = |part_id: NodeId| {
            nodes.get(part_id).is_some_and(|part| {
                part.element_data().is_some_and(|element| {
                    element
                        .inline_slices
                        .iter()
                        .any(|slice| slice.element == node_id)
                })
            })
        };
        let keeps_parent = slices.iter().any(|slice| slice.element == node_id)
            && nodes[node_id].layout_parent.get().is_some_and(holds_part);

        let node = &mut nodes[node_id];

        // Set layout_parent for node.
        if !keeps_parent {
            node.layout_parent.set(Some(parent_id));
        }

        match &node.data {
            NodeData::Element(element_data) | NodeData::AnonymousBlock(element_data) => {
                // if the input type is hidden, hide it
                if *element_data.name.local == *"input" {
                    if let Some("hidden") = element_data.attr(local_name!("type")) {
                        return;
                    }
                }

                let display = node.display_style().unwrap_or(Display::inline());

                match (display.outside(), display.inside()) {
                    (DisplayOutside::None, DisplayInside::None) => {
                        node.remove_damage(CONSTRUCT_DESCENDENT | CONSTRUCT_FC | CONSTRUCT_BOX);
                    }
                    (DisplayOutside::None, DisplayInside::Contents) => {
                        node.remove_damage(CONSTRUCT_DESCENDENT | CONSTRUCT_FC | CONSTRUCT_BOX);
                        iter_children!(nodes[node_id], |child_id| {
                            find_inline_layout_embedded_boxes_recursive(
                                nodes,
                                parent_id,
                                child_id,
                                layout_children,
                                slices,
                                root_id,
                            );
                        });
                    }
                    (DisplayOutside::Inline, DisplayInside::Flow) => {
                        let tag_name = &element_data.name.local;

                        if is_inline_box_element(tag_name) {
                            layout_children.push(node_id);
                        } else if *tag_name == local_name!("br") {
                            node.remove_damage(CONSTRUCT_DESCENDENT | CONSTRUCT_FC | CONSTRUCT_BOX);
                        } else if slices.iter().any(|slice| slice.element == node_id) {
                            // The element's other parts are in other
                            // anonymous blocks, so the content of this part
                            // reaches its inline root through `layout_parent`
                            // directly.
                            node.remove_damage(CONSTRUCT_DESCENDENT | CONSTRUCT_FC | CONSTRUCT_BOX);
                            let children: Vec<NodeId> =
                                sliced_box_children(&nodes[node_id], slices).collect();
                            for child_id in children {
                                find_inline_layout_embedded_boxes_recursive(
                                    nodes,
                                    root_id,
                                    child_id,
                                    layout_children,
                                    slices,
                                    root_id,
                                );
                            }
                        } else {
                            node.remove_damage(CONSTRUCT_DESCENDENT | CONSTRUCT_FC | CONSTRUCT_BOX);
                            iter_children_and_pseudos!(nodes[node_id], |child_id| {
                                find_inline_layout_embedded_boxes_recursive(
                                    nodes,
                                    node_id,
                                    child_id,
                                    layout_children,
                                    slices,
                                    root_id,
                                );
                            });
                        }
                    }
                    // Inline box
                    (_, _) => {
                        layout_children.push(node_id);
                    }
                };
            }
            NodeData::Comment { .. } | NodeData::Text(_) => {
                node.remove_damage(CONSTRUCT_DESCENDENT | CONSTRUCT_FC | CONSTRUCT_BOX);
            }
            NodeData::Document(_) => unreachable!(),
        }
    }
}

pub(crate) fn build_inline_layout_into(
    nodes: &crate::NodeTree,
    layout_ctx: &mut LayoutContext<TextBrush>,
    font_ctx: &mut FontContext,
    text_layout: &mut TextLayout,
    scale: f32,
    inline_context_root_node_id: NodeId,
) {
    // Get the inline context's root node's text styles
    let root_node = &nodes[inline_context_root_node_id];
    let root_node_style = root_node.primary_styles().or_else(|| {
        root_node
            .parent
            .and_then(|parent_id| nodes[parent_id].primary_styles())
    });

    let parley_style = root_node_style
        .as_ref()
        .map(|s| stylo_to_parley::style(inline_context_root_node_id, s))
        .unwrap_or_else(|| parley::TextStyle {
            white_space_collapse: WhiteSpaceCollapse::Collapse,
            ..Default::default()
        });

    // Create a parley tree builder
    let mut builder = layout_ctx.tree_builder(font_ctx, scale, true, &parley_style);
    if let Some(style) = root_node_style.as_deref() {
        builder.set_base_direction(stylo_to_parley::base_direction(
            style.clone_direction(),
            style.clone_unicode_bidi(),
        ));
    }

    let text_transform = root_node_style
        .as_deref()
        .map(|s| CaseTransform::from_style(s))
        .unwrap_or(CaseTransform::NONE);

    // Render position-inside list items
    if let Some(ListItemLayout {
        marker,
        position: ListItemLayoutPosition::Inside,
    }) = root_node
        .element_data()
        .and_then(|el| el.list_item_data.as_deref())
    {
        match marker {
            // Bullet glyphs live in the bundled bullet font. The position-outside
            // path already asks for it; without the same span here a marker like
            // disclosure-closed (U+25B8) falls back to the element's own font and
            // renders as a missing glyph.
            Marker::Char(char) => {
                let mut marker_style = parley_style.clone();
                marker_style.font_family = BULLET_FONT_FAMILY.into();
                builder.push_style_span(marker_style);
                builder.push_text(&format!("{char} "));
                builder.pop_style_span();
            }
            Marker::String(str) => builder.push_text(str),
        }
    };
    // The marker is a separate box, so words in the content don't continue from it.
    let mut text_transformer = TextTransformer::default();
    text_transformer.word_break(&builder);
    // The parts of split inline elements an anonymous block holds.
    let slices: &[InlineSlice] = root_node
        .element_data()
        .map(|element| &element.inline_slices[..])
        .unwrap_or_default();
    let mut edges = InlineEdges {
        content_started: !builder.text().is_empty(),
        breaks_anywhere: root_node_style
            .as_deref()
            .is_some_and(|style| breaks_anywhere(style)),
        ..InlineEdges::default()
    };

    if let Some(before_id) = root_node.before() {
        build_inline_layout_recursive(
            &mut builder,
            &mut text_transformer,
            &mut edges,
            nodes,
            scale,
            slices,
            before_id,
            &text_transform,
        );
    }
    for child_id in root_node.children.iter().copied() {
        build_inline_layout_recursive(
            &mut builder,
            &mut text_transformer,
            &mut edges,
            nodes,
            scale,
            slices,
            child_id,
            &text_transform,
        );
    }
    if let Some(after_id) = root_node.after() {
        build_inline_layout_recursive(
            &mut builder,
            &mut text_transformer,
            &mut edges,
            nodes,
            scale,
            slices,
            after_id,
            &text_transform,
        );
    }

    // Lines break where Chromium breaks them, except in text that breaks
    // anywhere, which keeps UAX #14.
    builder.set_line_break_override(match (edges.used, edges.breaks_anywhere) {
        (true, false) => Some(EDGE_AND_CHROMIUM_LINE_BREAKS),
        (true, true) => Some(EDGE_LINE_BREAKS),
        (false, false) => Some(parley::CHROMIUM_LINE_BREAK_OVERRIDE),
        (false, true) => None,
    });
    text_layout.text = builder.build_into(&mut text_layout.layout);
    text_layout.only_collapsible_space = !edges.content_started && !edges.used;
    text_layout.tabs = std::mem::take(&mut edges.tabs);
    return;

    fn build_inline_layout_recursive(
        builder: &mut TreeBuilder<TextBrush>,
        text_transformer: &mut TextTransformer,
        edges: &mut InlineEdges,
        nodes: &crate::NodeTree,
        scale: f32,
        slices: &[InlineSlice],
        node_id: NodeId,
        parent_text_transform: &CaseTransform,
    ) {
        let node = &nodes[node_id];

        let style = node.primary_styles();
        let style = style.as_ref();

        let text_transform = style
            .map(|s| CaseTransform::from_style(s))
            .unwrap_or(CaseTransform::NONE);

        match &node.data {
            NodeData::Element(element_data) | NodeData::AnonymousBlock(element_data) => {
                // if the input type is hidden, hide it
                if *element_data.name.local == *"input" {
                    if let Some("hidden") = element_data.attr(local_name!("type")) {
                        return;
                    }
                }

                let display = node.display_style().unwrap_or(Display::inline());
                let position = style
                    .map(|s| s.clone_position())
                    .unwrap_or(PositionProperty::Static);
                let float = style.map(|s| s.clone_float()).unwrap_or(Float::None);
                let box_kind = if position.is_absolutely_positioned() {
                    InlineBoxKind::OutOfFlow
                } else if cfg!(feature = "floats") && float.is_floating() {
                    // Custom boxes yield until the caller places them. Only the
                    // float-aware line breaker places them; otherwise Parley
                    // appends them with zero size, so the float would overlap the
                    // text after it. Without floats, lay these boxes out in flow
                    // like other inline boxes.
                    InlineBoxKind::CustomOutOfFlow
                } else {
                    InlineBoxKind::InFlow
                };

                match (display.outside(), display.inside()) {
                    (DisplayOutside::None, DisplayInside::None) => {
                        // node.remove_damage(CONSTRUCT_DESCENDENT | CONSTRUCT_FC | CONSTRUCT_BOX);
                    }
                    (DisplayOutside::None, DisplayInside::Contents) => {
                        // The span copies its parent's style, but the
                        // element has no box to align: its content sits on
                        // the parent's baseline.
                        let whitespace = style.map(|s| {
                            [
                                parley::StyleProperty::WhiteSpaceCollapse(
                                    stylo_to_parley::white_space_collapse(
                                        s.clone_white_space_collapse(),
                                    ),
                                ),
                                parley::StyleProperty::TextWrapMode(
                                    stylo_to_parley::text_wrap_mode(s.clone_text_wrap_mode()),
                                ),
                                parley::StyleProperty::VerticalAlign(
                                    parley::VerticalAlign::BASELINE,
                                ),
                            ]
                        });
                        builder.push_style_modification_span(
                            whitespace
                                .as_ref()
                                .map_or(&[][..], |styles| styles.as_slice()),
                        );
                        for child_id in node.children.iter().copied() {
                            // node.remove_damage(CONSTRUCT_DESCENDENT | CONSTRUCT_FC | CONSTRUCT_BOX);
                            build_inline_layout_recursive(
                                builder,
                                text_transformer,
                                edges,
                                nodes,
                                scale,
                                slices,
                                child_id,
                                &text_transform,
                            );
                        }
                        builder.pop_style_span();
                    }
                    (DisplayOutside::Inline, DisplayInside::Flow) => {
                        let tag_name = &element_data.name.local;

                        if is_inline_box_element(tag_name) {
                            if box_kind == InlineBoxKind::InFlow {
                                text_transformer.word_break(builder);
                                edges.content(builder);
                            }
                            builder.push_inline_box(InlineBox {
                                id: node_id.as_u64(),
                                kind: box_kind,
                                // Overridden by push_inline_box method
                                index: 0,
                                // Width and height are set during layout
                                width: 0.0,
                                height: 0.0,
                                baseline: None,
                                vertical_align: inline_vertical_align(nodes, node, scale),
                            });
                        } else if *tag_name == local_name!("wbr") {
                            // A line break opportunity, even in text that
                            // doesn't wrap, as Chromium lays it out.
                            edges.content(builder);
                            builder.push_style_modification_span(&[
                                parley::StyleProperty::TextWrapMode(parley::TextWrapMode::Wrap),
                                parley::StyleProperty::Brush(TextBrush {
                                    id: node_id,
                                    generated: GeneratedText::LineBreakOpportunity,
                                }),
                                parley::StyleProperty::VerticalAlign(
                                    parley::VerticalAlign::BASELINE,
                                ),
                            ]);
                            builder.push_text("\u{200B}");
                            builder.pop_style_span();
                        } else if *tag_name == local_name!("br") {
                            // node.remove_damage(CONSTRUCT_DESCENDENT | CONSTRUCT_FC | CONSTRUCT_BOX);
                            // TODO: update span id for br spans
                            edges.content(builder);
                            // The newline sits on its parent's baseline: the
                            // span copies the parent's `vertical-align`, which
                            // would otherwise align it a second time.
                            builder.push_style_modification_span(&[
                                parley::StyleProperty::WhiteSpaceCollapse(
                                    WhiteSpaceCollapse::Preserve,
                                ),
                                parley::StyleProperty::VerticalAlign(
                                    parley::VerticalAlign::BASELINE,
                                ),
                            ]);
                            builder.push_text("\n");
                            builder.pop_style_span();
                            // A `<br>` that clears floats starts the next line
                            // below them, as in browsers. The line breaker
                            // stops at this empty box to move the line.
                            #[cfg(feature = "floats")]
                            if style.is_some_and(|s| {
                                s.clone_clear() != style::values::computed::Clear::None
                            }) {
                                builder.push_inline_box(InlineBox {
                                    id: node_id.as_u64(),
                                    kind: InlineBoxKind::CustomOutOfFlow,
                                    // Overridden by push_inline_box method
                                    index: 0,
                                    width: 0.0,
                                    height: 0.0,
                                    baseline: None,
                                    vertical_align: parley::VerticalAlign::BASELINE,
                                });
                            }
                            text_transformer.word_break(builder);
                        } else {
                            // node.remove_damage(CONSTRUCT_DESCENDENT | CONSTRUCT_FC | CONSTRUCT_BOX);
                            let mut style = node
                                .primary_styles()
                                .map(|s| stylo_to_parley::style(node.id, &s))
                                .unwrap_or_else(|| parley::TextStyle {
                                    white_space_collapse: WhiteSpaceCollapse::Collapse,
                                    ..Default::default()
                                });
                            style.vertical_align = inline_vertical_align(nodes, node, scale);

                            let (start_edge, end_edge) = node
                                .primary_styles()
                                .map(|s| {
                                    edges.breaks_anywhere |= breaks_anywhere(&s);
                                    inline_edge_widths(&s)
                                })
                                .unwrap_or_default();

                            // A part of an element that a block splits has
                            // the element's start edge only if it starts the
                            // element, and its end edge only if it ends it.
                            let (first, last) = slices
                                .iter()
                                .find(|slice| slice.element == node_id)
                                .map_or((true, true), |slice| (slice.first, slice.last));
                            let start_edge = if first { start_edge } else { 0.0 };
                            let end_edge = if last { end_edge } else { 0.0 };

                            // A space held back from content before this
                            // element precedes it, outside its box.
                            edges.flush_held_space(builder);
                            builder.push_style_span(style);
                            if start_edge != 0.0 {
                                edges.push_start(builder, node.id, start_edge);
                            }
                            if end_edge != 0.0 {
                                edges.end_depth += 1;
                            }

                            for child_id in sliced_box_children(node, slices) {
                                build_inline_layout_recursive(
                                    builder,
                                    text_transformer,
                                    edges,
                                    nodes,
                                    scale,
                                    slices,
                                    child_id,
                                    &text_transform,
                                );
                            }

                            if end_edge != 0.0 {
                                edges.end_depth -= 1;
                                edges.push_end(builder, node.id, end_edge);
                            }
                            builder.pop_style_span();
                        }
                    }
                    // Inline box
                    (_, _) => {
                        if box_kind == InlineBoxKind::InFlow {
                            text_transformer.word_break(builder);
                            edges.content(builder);
                        }
                        builder.push_inline_box(InlineBox {
                            id: node_id.as_u64(),
                            kind: box_kind,
                            // Overridden by push_inline_box method
                            index: 0,
                            // Width and height are set during layout
                            width: 0.0,
                            height: 0.0,
                            baseline: None,
                            vertical_align: inline_vertical_align(nodes, node, scale),
                        });
                    }
                };
            }
            NodeData::Text(data) => {
                // node.remove_damage(CONSTRUCT_DESCENDENT | CONSTRUCT_FC | CONSTRUCT_BOX);
                // dbg!(&data.content);

                let text =
                    text_transformer.transform(&data.content, parent_text_transform, builder);
                let parent_style = node
                    .parent
                    .and_then(|parent| nodes[parent].primary_styles());
                let collapse = parent_style
                    .as_ref()
                    .map(|s| stylo_to_parley::white_space_collapse(s.clone_white_space_collapse()))
                    .unwrap_or(WhiteSpaceCollapse::Collapse);
                // Parley has no tab stops and draws a preserved tab as a
                // missing glyph. A tab advances to the next multiple of
                // `tab-size` columns from the line's start (CSS Text 3 section
                // 4.2), which in a monospace font, as `<pre>` has, is that many
                // spaces, so it becomes them.
                if matches!(
                    collapse,
                    WhiteSpaceCollapse::Preserve | WhiteSpaceCollapse::BreakSpaces
                ) && text.contains('\t')
                {
                    let tab_size = parent_style.as_deref().map_or(8, |s| tab_columns(s));
                    let column = builder
                        .text()
                        .rsplit('\n')
                        .next()
                        .map_or(0, |line| line.chars().count());
                    let (expanded, tabs) = expand_tabs(text, column, tab_size);
                    edges.push_text(builder, &expanded, collapse);
                    // Preserved text is pushed whole, so it ends the text.
                    if let Some(start) = builder.text().len().checked_sub(expanded.len()) {
                        edges.tabs.extend(
                            tabs.into_iter()
                                .map(|tab| start + tab.start..start + tab.end),
                        );
                    }
                } else {
                    edges.push_text(builder, text, collapse);
                }
            }
            NodeData::Comment { .. } => {
                // node.remove_damage(CONSTRUCT_DESCENDENT | CONSTRUCT_FC | CONSTRUCT_BOX);
            }
            NodeData::Document(_) => unreachable!(),
        }
    }
}

/// Line breaking around inline edge placeholders: a start edge stays with
/// what follows it, and an end edge with what precedes it. UAX #14 mostly
/// does this, as a start edge is alphabetic and an end edge a word joiner,
/// but a start edge may break from an ideograph, and `line-break: anywhere`
/// and Chromium's break after every space would break before an end edge.
static EDGE_LINE_BREAKS: &parley::LineBreakOverrideFn =
    &(edge_line_break as fn(parley::LineBreakContext) -> Option<bool>);

fn edge_line_break(cx: parley::LineBreakContext) -> Option<bool> {
    (cx.before == INLINE_START_EDGE || cx.after == INLINE_END_EDGE).then_some(false)
}

/// Line breaking as Chromium does it (between two ASCII characters only
/// where its table allows, and after every run of spaces), with
/// [`EDGE_LINE_BREAKS`] first.
///
/// Chromium's table sees no inline edges, so it looks through them here: an
/// end edge stands for the character before it, and a start edge for a
/// letter, which is how the content it starts usually begins. `a/<span
/// style="padding: 2px">b</span>` doesn't break after the slash.
static EDGE_AND_CHROMIUM_LINE_BREAKS: &parley::LineBreakOverrideFn =
    &(edge_and_chromium_line_break as fn(parley::LineBreakContext) -> Option<bool>);

fn edge_and_chromium_line_break(mut cx: parley::LineBreakContext) -> Option<bool> {
    if let Some(forced) = edge_line_break(cx) {
        return Some(forced);
    }
    if cx.after == INLINE_START_EDGE {
        cx.after = 'a';
    }
    if cx.before == INLINE_END_EDGE {
        cx.before = cx
            .before_before
            .filter(|c| !is_inline_edge_char(*c))
            .unwrap_or('a');
        cx.before_before = None;
    }
    parley::CHROMIUM_LINE_BREAK_OVERRIDE(cx)
}

/// Whether a style lets text break between any two characters, which
/// Chromium's ASCII table would forbid between letters.
fn breaks_anywhere(style: &style::properties::ComputedValues) -> bool {
    use style::values::computed::{LineBreak, WordBreak};
    let text = style.get_inherited_text();
    text.word_break == WordBreak::BreakAll || text.line_break == LineBreak::Anywhere
}

/// The widths of an inline element's inline-start and inline-end edges, its
/// margin, border and padding on each side, in CSS px. Percentages and `auto`
/// count as zero, as the containing block's width is not known yet.
fn inline_edge_widths(style: &style::properties::ComputedValues) -> (f32, f32) {
    use style::values::computed::{LengthPercentage, Margin};
    if !style.writing_mode.is_horizontal() {
        return (0.0, 0.0);
    }
    let length = |lp: &LengthPercentage| lp.to_length().map_or(0.0, |l| l.px());
    let margin = |m: &Margin| match m {
        Margin::LengthPercentage(lp) => length(lp),
        _ => 0.0,
    };
    let (margins, padding, border) = (style.get_margin(), style.get_padding(), style.get_border());
    // A border's computed width stays `medium` when it has no style.
    let border_width = |style: style::values::computed::BorderStyle, width: app_units::Au| {
        if style.none_or_hidden() {
            0.0
        } else {
            width.to_f32_px()
        }
    };
    let left = margin(&margins.margin_left)
        + border_width(border.border_left_style, border.border_left_width.0)
        + length(&padding.padding_left.0);
    let right = margin(&margins.margin_right)
        + border_width(border.border_right_style, border.border_right_width.0)
        + length(&padding.padding_right.0);
    let sanitize = |width: f32| if width.is_finite() { width } else { 0.0 };
    if style.writing_mode.is_bidi_ltr() {
        (sanitize(left), sanitize(right))
    } else {
        (sanitize(right), sanitize(left))
    }
}

/// The columns between tab stops: `tab-size`, taking a length as the
/// default 8.
fn tab_columns(style: &style::properties::ComputedValues) -> usize {
    match style.get_inherited_text().tab_size {
        style::values::generics::length::LengthOrNumber::Number(columns) => {
            columns.0.round() as usize
        }
        style::values::generics::length::LengthOrNumber::Length(_) => 8,
    }
}

/// `text` with each tab replaced by the spaces up to the next multiple of
/// `tab_size` columns, counting from `column` characters into a line, and
/// the byte ranges of those spaces.
fn expand_tabs(
    text: &str,
    mut column: usize,
    tab_size: usize,
) -> (String, Vec<std::ops::Range<usize>>) {
    let mut expanded = String::with_capacity(text.len() + tab_size);
    let mut tabs = Vec::new();
    for c in text.chars() {
        match c {
            '\t' if tab_size == 0 => {}
            '\t' => {
                let spaces = tab_size - column % tab_size;
                let start = expanded.len();
                expanded.extend(std::iter::repeat_n(' ', spaces));
                tabs.push(start..expanded.len());
                column += spaces;
            }
            '\n' => {
                expanded.push(c);
                column = 0;
            }
            _ => {
                expanded.push(c);
                column += 1;
            }
        }
    }
    (expanded, tabs)
}

/// A node's `vertical-align` in its line. Blink and WebKit lower `sub` by a
/// fifth of the parent's font size and raise `super` by a third of it, each
/// plus a device pixel (Blink's `InlineLayoutStateStack::ApplyBaselineShift`).
/// Parley leaves out the pixel, so lines with superscripts were shorter than
/// in browsers. It gets these shifts as lengths instead.
fn inline_vertical_align(
    nodes: &crate::NodeTree,
    node: &Node,
    scale: f32,
) -> parley::VerticalAlign {
    let Some(style) = node.primary_styles() else {
        return parley::VerticalAlign::default();
    };
    let align = stylo_to_parley::vertical_align(&style);
    let parent_font_size = node
        .parent
        .and_then(|parent| nodes[parent].primary_styles())
        .map(|parent| parent.get_font().font_size.used_size.0.px());
    let (Some(font_size), parley::BaselineShift::Sub | parley::BaselineShift::Super) =
        (parent_font_size, align.shift)
    else {
        return align;
    };
    // Blink divides the font size in device pixels as a fixed-point number
    // with six fractional bits, truncating.
    let fixed = (font_size * scale * 64.0).round();
    let shift = if align.shift == parley::BaselineShift::Super {
        (fixed / 3.0).trunc() / 64.0 + 1.0
    } else {
        -((fixed / 5.0).trunc() / 64.0 + 1.0)
    };
    parley::VerticalAlign {
        shift: parley::BaselineShift::Length(shift / scale),
        ..align
    }
}

/// White space processing around inline edge placeholders, which are
/// transparent to it: a collapsible space next to an element's edges collapses
/// with spaces on the other side of them.
///
/// Parley collapses white space between pushed texts and drops it at the end
/// of the paragraph, but a placeholder is content to it. So the leading white
/// space after a start edge that follows a space (or starts the paragraph) is
/// skipped here, and trailing white space inside an element with an end edge is
/// held back, to be pushed after the edge only if more content follows.
#[derive(Default)]
struct InlineEdges {
    /// Whether any placeholder was pushed.
    used: bool,
    /// Whether the paragraph has content other than placeholders.
    content_started: bool,
    /// Skip collapsible white space until the next content.
    skip_leading_space: bool,
    /// A collapsible space taken off the end of the last text.
    held_space: bool,
    /// How many enclosing elements have an end edge.
    end_depth: u32,
    /// Whether any text may break between any two characters.
    breaks_anywhere: bool,
    /// Where preserved tabs were laid out as spaces.
    tabs: Vec<std::ops::Range<usize>>,
}

impl InlineEdges {
    fn flush_held_space(&mut self, builder: &mut TreeBuilder<TextBrush>) {
        if self.held_space {
            self.held_space = false;
            // Parley keeps it pending, so it only lands before more content.
            builder.push_text(" ");
        }
    }

    /// Content that is not text, such as an atomic inline or a line break.
    fn content(&mut self, builder: &mut TreeBuilder<TextBrush>) {
        self.flush_held_space(builder);
        self.skip_leading_space = false;
        self.content_started = true;
    }

    fn push_edge(
        &mut self,
        builder: &mut TreeBuilder<TextBrush>,
        id: NodeId,
        width: f32,
        edge: GeneratedText,
    ) {
        self.used = true;
        builder.push_style_modification_span(&[
            StyleProperty::LetterSpacing(width),
            StyleProperty::Brush(TextBrush {
                id,
                generated: edge,
            }),
            StyleProperty::VerticalAlign(parley::VerticalAlign::BASELINE),
        ]);
        let mut placeholder = [0u8; 4];
        let placeholder = match edge {
            GeneratedText::EndEdge => INLINE_END_EDGE,
            _ => INLINE_START_EDGE,
        }
        .encode_utf8(&mut placeholder);
        builder.push_text(placeholder);
        builder.pop_style_span();
    }

    fn push_start(&mut self, builder: &mut TreeBuilder<TextBrush>, id: NodeId, width: f32) {
        // A pending space before the element is committed before its edge,
        // and white space at the start of its content then collapses into it.
        let after_space = builder.has_pending_whitespace() || !self.content_started;
        self.push_edge(builder, id, width, GeneratedText::StartEdge);
        self.skip_leading_space |= after_space;
    }

    fn push_end(&mut self, builder: &mut TreeBuilder<TextBrush>, id: NodeId, width: f32) {
        self.push_edge(builder, id, width, GeneratedText::EndEdge);
    }

    fn push_text(
        &mut self,
        builder: &mut TreeBuilder<TextBrush>,
        text: &str,
        collapse: WhiteSpaceCollapse,
    ) {
        let collapsible = |c: char| match collapse {
            WhiteSpaceCollapse::Collapse => c.is_ascii_whitespace(),
            WhiteSpaceCollapse::PreserveBreaks => matches!(c, ' ' | '\t'),
            _ => false,
        };
        if !self.held_space && !self.skip_leading_space && self.end_depth == 0 {
            // Preserved white space is content too.
            if !self.content_started && text.contains(|c: char| !collapsible(c)) {
                self.content_started = true;
            }
            builder.push_text(text);
            return;
        }
        let mut text = text;
        if self.skip_leading_space {
            text = text.trim_start_matches(collapsible);
            if text.is_empty() {
                return;
            }
            self.skip_leading_space = false;
        }
        let content = if self.end_depth > 0 {
            text.trim_end_matches(collapsible)
        } else {
            text
        };
        if !content.is_empty() {
            self.flush_held_space(builder);
            builder.push_text(content);
            self.content_started = true;
        }
        if content.len() < text.len() {
            self.held_space = true;
        }
    }
}

#[cfg(test)]
mod inside_marker_tests {
    use crate::{Attribute, BaseDocument, DocumentConfig, DocumentMutator, NodeId, qual_name};
    use blitz_traits::shell::{ColorScheme, Viewport};

    /// A closed `<details>` whose `<summary>` holds what `content` builds,
    /// made by hand because the HTML parser lives in blitz-html.
    fn summary_holding(
        content: impl FnOnce(&mut DocumentMutator) -> Vec<NodeId>,
    ) -> (BaseDocument, NodeId) {
        let mut doc = BaseDocument::new(DocumentConfig {
            viewport: Some(Viewport::new(400, 300, 1.0, ColorScheme::Light)),
            ..Default::default()
        });
        let root_id = doc.root_node().id;
        let mut mutator = doc.mutate();
        let html = mutator.create_element(qual_name!("html", html), vec![]);
        let body = mutator.create_element(
            qual_name!("body", html),
            vec![Attribute {
                name: qual_name!("style"),
                value: "margin:0".into(),
            }],
        );
        let details = mutator.create_element(qual_name!("details", html), vec![]);
        let summary = mutator.create_element(qual_name!("summary", html), vec![]);
        let children = content(&mut mutator);
        mutator.append_children(summary, &children);
        let hidden = mutator.create_element(qual_name!("p", html), vec![]);
        mutator.append_children(details, &[summary, hidden]);
        mutator.append_children(body, &[details]);
        mutator.append_children(html, &[body]);
        mutator.append_children(root_id, &[html]);
        drop(mutator);
        doc.resolve(0.0);
        (doc, summary)
    }

    fn layout_children(doc: &BaseDocument, id: NodeId) -> Vec<NodeId> {
        doc.nodes[id]
            .layout_children
            .borrow()
            .as_deref()
            .unwrap_or_default()
            .to_vec()
    }

    fn inline_text(doc: &BaseDocument, id: NodeId) -> Option<String> {
        let element = doc.nodes[id].element_data()?;
        Some(element.inline_layout_data.as_ref()?.text.clone())
    }

    fn is_anonymous(doc: &BaseDocument, id: NodeId) -> bool {
        matches!(doc.nodes[id].data, crate::NodeData::AnonymousBlock(_))
    }

    #[test]
    fn marker_of_inline_summary_stays_in_its_text() {
        let (doc, summary) = summary_holding(|m| vec![m.create_text_node("Plain")]);
        assert_eq!(inline_text(&doc, summary).as_deref(), Some("▸ Plain"));
    }

    #[test]
    fn marker_stands_on_its_own_line_above_a_block() {
        let (doc, summary) = summary_holding(|m| {
            let heading = m.create_element(qual_name!("h3", html), vec![]);
            let text = m.create_text_node("Changelog");
            m.append_children(heading, &[text]);
            vec![heading]
        });
        let children = layout_children(&doc, summary);
        assert_eq!(children.len(), 2, "a marker line, then the heading");
        assert!(is_anonymous(&doc, children[0]));
        assert_eq!(inline_text(&doc, children[0]).as_deref(), Some("▸"));
        assert_eq!(inline_text(&doc, children[1]).as_deref(), Some("Changelog"));
        assert_eq!(inline_text(&doc, summary), None);
    }

    #[test]
    fn marker_joins_inline_content_before_a_block() {
        let (doc, summary) = summary_holding(|m| {
            let text = m.create_text_node("Notes");
            let block = m.create_element(qual_name!("div", html), vec![]);
            let more = m.create_text_node("More");
            m.append_children(block, &[more]);
            vec![text, block]
        });
        let children = layout_children(&doc, summary);
        assert_eq!(children.len(), 2, "the wrapped text, then the block");
        assert!(is_anonymous(&doc, children[0]));
        assert_eq!(inline_text(&doc, children[0]).as_deref(), Some("▸ Notes"));
        assert_eq!(inline_text(&doc, children[1]).as_deref(), Some("More"));
    }

    #[test]
    fn empty_summary_still_shows_its_marker() {
        let (doc, summary) = summary_holding(|_| Vec::new());
        assert_eq!(inline_text(&doc, summary).as_deref(), Some("▸"));
    }
}
