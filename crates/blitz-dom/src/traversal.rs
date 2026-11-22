use blitz_traits::node_id::NodeId;
use std::cmp::Ordering;

use style::dom::TNode as _;

use crate::{BaseDocument, Node};

macro_rules! iter_children {
    ($node_expr:expr, $cb:expr) => {{
        let node = &mut $node_expr;
        let children = core::mem::take(&mut node.children);
        for child_id in children.iter().copied() {
            $cb(child_id)
        }
        $node_expr.children = children;
    }};
}
pub(crate) use iter_children;

macro_rules! iter_children_and_pseudos {
    ($node_expr:expr, $cb:expr) => {{
        // Load node
        let node = &mut $node_expr;

        // Copy before, after, and take children
        let before = node.before();
        let after = node.after();
        let children = core::mem::take(&mut node.children);

        if let Some(before) = before {
            $cb(before)
        }
        for child_id in children.iter().copied() {
            $cb(child_id)
        }
        if let Some(after) = after {
            $cb(after)
        }

        // Reload node and put children back
        $node_expr.children = children;
    }};
}
pub(crate) use iter_children_and_pseudos;

#[derive(Clone)]
/// An pre-order tree traverser for a [BaseDocument](crate::document::BaseDocument).
pub struct TreeTraverser<'a> {
    doc: &'a BaseDocument,
    stack: Vec<NodeId>,
}

impl<'a> TreeTraverser<'a> {
    /// Creates a new tree traverser for the given document which starts at the root node.
    pub fn new(doc: &'a BaseDocument) -> Self {
        Self::new_with_root(doc, doc.root_node().id)
    }

    /// Creates a new tree traverser for the given document which starts at the specified node.
    pub fn new_with_root(doc: &'a BaseDocument, root: NodeId) -> Self {
        let mut stack = Vec::with_capacity(32);
        stack.push(root);
        TreeTraverser { doc, stack }
    }
}
impl Iterator for TreeTraverser<'_> {
    type Item = NodeId;

    fn next(&mut self) -> Option<Self::Item> {
        let id = self.stack.pop()?;
        let node = self.doc.get_node(id)?;
        self.stack.extend(node.children.iter().rev());
        Some(id)
    }
}

#[derive(Clone)]
/// An ancestor traverser for a [BaseDocument](crate::document::BaseDocument).
pub struct AncestorTraverser<'a> {
    doc: &'a BaseDocument,
    current: NodeId,
}
impl<'a> AncestorTraverser<'a> {
    /// Creates a new ancestor traverser for the given document and node ID.
    pub fn new(doc: &'a BaseDocument, node_id: NodeId) -> Self {
        AncestorTraverser {
            doc,
            current: node_id,
        }
    }
}
impl Iterator for AncestorTraverser<'_> {
    type Item = NodeId;

    fn next(&mut self) -> Option<Self::Item> {
        let current_node = self.doc.get_node(self.current)?;
        self.current = current_node.parent?;
        Some(self.current)
    }
}

impl BaseDocument {
    /// Collect the nodes into a chain by traversing upwards
    pub fn node_chain(&self, node_id: NodeId) -> Vec<NodeId> {
        let mut chain = Vec::with_capacity(16);
        chain.push(node_id);
        chain.extend(
            AncestorTraverser::new(self, node_id).filter(|id| self.nodes[*id].is_element()),
        );
        chain
    }

    pub fn visit<F>(&self, mut visit: F)
    where
        F: FnMut(NodeId, &Node),
    {
        TreeTraverser::new(self).for_each(|node_id| visit(node_id, &self.nodes[node_id]));
    }

    /// If the node is non-anonymous then returns the node's id
    /// Else find's the first non-anonymous ancester of the node
    pub fn non_anon_ancestor_if_anon(&self, mut node_id: NodeId) -> NodeId {
        loop {
            let node = &self.nodes[node_id];

            if !node.is_anonymous() {
                return node.id;
            }

            let Some(parent_id) = node.layout_parent.get() else {
                // Shouldn't be reachable unless invalid node_id is passed
                // as root node is always non-anonymous
                panic!("Node does not exist or does not have a non-anonymous parent");
            };

            node_id = parent_id;
        }
    }

    pub fn iter_children_mut(
        &mut self,
        node_id: NodeId,
        mut cb: impl FnMut(NodeId, &mut BaseDocument),
    ) {
        let children = std::mem::take(&mut self.nodes[node_id].children);
        for child_id in children.iter().cloned() {
            cb(child_id, self);
        }
        self.nodes[node_id].children = children;
    }

    pub fn iter_subtree_mut(
        &mut self,
        node_id: NodeId,
        mut cb: impl FnMut(NodeId, &mut BaseDocument),
    ) {
        cb(node_id, self);
        iter_subtree_mut_inner(self, node_id, &mut cb);
        fn iter_subtree_mut_inner(
            doc: &mut BaseDocument,
            node_id: NodeId,
            cb: &mut impl FnMut(NodeId, &mut BaseDocument),
        ) {
            let children = std::mem::take(&mut doc.nodes[node_id].children);
            for child_id in children.iter().cloned() {
                cb(child_id, doc);
                iter_subtree_mut_inner(doc, child_id, cb);
            }
            doc.nodes[node_id].children = children;
        }
    }

    pub fn iter_children_and_pseudos_mut(
        &mut self,
        node_id: NodeId,
        mut cb: impl FnMut(NodeId, &mut BaseDocument),
    ) {
        let before = self.nodes[node_id].before();
        self.nodes[node_id].set_pe_by_index(1, None);
        if let Some(before_node_id) = before {
            cb(before_node_id, self)
        }
        self.nodes[node_id].set_pe_by_index(1, before);

        self.iter_children_mut(node_id, &mut cb);

        let after = self.nodes[node_id].after();
        self.nodes[node_id].set_pe_by_index(0, None);
        if let Some(after_node_id) = after {
            cb(after_node_id, self)
        }
        self.nodes[node_id].set_pe_by_index(0, after);
    }

    pub fn next_node(&self, start: &Node, mut filter: impl FnMut(&Node) -> bool) -> Option<NodeId> {
        let start_id = start.id;
        let mut node = start;
        let mut look_in_children = true;
        loop {
            // Next is first child
            let next = if look_in_children && !node.children.is_empty() {
                let node_id = node.children[0];
                &self.nodes[node_id]
            }
            // Next is next sibling or parent
            else if let Some(parent) = node.parent_node() {
                let self_idx = parent
                    .children
                    .iter()
                    .position(|id| *id == node.id)
                    .unwrap();
                // Next is next sibling
                if let Some(sibling_id) = parent.children.get(self_idx + 1) {
                    look_in_children = true;
                    &self.nodes[*sibling_id]
                }
                // Next is parent
                else {
                    look_in_children = false;
                    node = parent;
                    continue;
                }
            }
            // Continue search from the root
            else {
                look_in_children = true;
                self.root_node()
            };

            if filter(next) {
                return Some(next.id);
            } else if next.id == start_id {
                return None;
            }

            node = next;
        }
    }

    /// The node that comes last within `node`'s subtree in document order,
    /// which is what precedes `node`'s successor in reverse order.
    fn deepest_last_descendant<'a>(&'a self, mut node: &'a Node) -> &'a Node {
        while let Some(last_child_id) = node.children.last() {
            node = &self.nodes[*last_child_id];
        }
        node
    }

    /// Mirror of [`Self::next_node`]: walks the tree in reverse document
    /// order, wrapping around to the end of the document.
    pub fn prev_node(&self, start: &Node, mut filter: impl FnMut(&Node) -> bool) -> Option<NodeId> {
        let start_id = start.id;
        let mut node = start;
        loop {
            let prev = if let Some(parent) = node.parent_node() {
                let self_idx = parent
                    .children
                    .iter()
                    .position(|id| *id == node.id)
                    .unwrap();
                // Previous is the deepest last descendant of the previous
                // sibling, or the parent when there is no previous sibling
                if self_idx > 0 {
                    self.deepest_last_descendant(&self.nodes[parent.children[self_idx - 1]])
                } else {
                    parent
                }
            }
            // Continue the search from the end of the document
            else {
                self.deepest_last_descendant(self.root_node())
            };

            if filter(prev) {
                return Some(prev.id);
            } else if prev.id == start_id {
                return None;
            }

            node = prev;
        }
    }

    /// `node_id` and its ancestor elements in the DOM, root first. These are
    /// the elements that match `:hover`/`:active` when `node_id` does.
    pub fn node_element_ancestors(&self, node_id: NodeId) -> Vec<NodeId> {
        let mut ancestors = Vec::with_capacity(12);
        let mut maybe_id = Some(node_id);
        while let Some(id) = maybe_id {
            let node = &self.nodes[id];
            if !node.is_element() {
                break;
            }
            ancestors.push(id);
            maybe_id = node.parent;
        }
        ancestors.reverse();
        ancestors
    }

    pub fn maybe_node_element_ancestors(&self, node_id: Option<NodeId>) -> Vec<NodeId> {
        node_id
            .map(|id| self.node_element_ancestors(id))
            .unwrap_or_default()
    }

    /// Compare the document order of two nodes.
    /// Returns Ordering::Less if node_a comes before node_b in document order.
    /// Returns Ordering::Greater if node_a comes after node_b.
    /// Returns Ordering::Equal if they are the same node.
    pub fn compare_document_order(&self, node_a: NodeId, node_b: NodeId) -> Ordering {
        if node_a == node_b {
            return Ordering::Equal;
        }

        // Build ancestor chains from root to node (inclusive)
        let chain_a = self.ancestor_chain_from_root(node_a);
        let chain_b = self.ancestor_chain_from_root(node_b);

        // Find where the chains diverge
        let mut common_depth = 0;
        for (a, b) in chain_a.iter().zip(chain_b.iter()) {
            if a != b {
                break;
            }
            common_depth += 1;
        }

        // If one is an ancestor of the other
        if common_depth == chain_a.len() {
            return Ordering::Less; // node_a is ancestor of node_b
        }
        if common_depth == chain_b.len() {
            return Ordering::Greater; // node_b is ancestor of node_a
        }

        // Safety: common_depth must be > 0 here because both chains start from the same
        // root node (node 0), so they share at least that node. If common_depth were 0,
        // chain_a[0] != chain_b[0], but both start from root, so this is impossible.
        debug_assert!(
            common_depth > 0,
            "nodes must share a common ancestor (the root)"
        );

        // Compare position among siblings at the divergence point
        let divergent_a = chain_a[common_depth];
        let divergent_b = chain_b[common_depth];
        let parent_id = chain_a[common_depth - 1];
        let parent = &self.nodes[parent_id];

        for &child_id in &parent.children {
            if child_id == divergent_a {
                return Ordering::Less;
            }
            if child_id == divergent_b {
                return Ordering::Greater;
            }
        }

        // Should not reach here if tree is well-formed
        Ordering::Equal
    }

    /// Build ancestor chain from root to node (inclusive), ordered [root, ..., node].
    fn ancestor_chain_from_root(&self, node_id: NodeId) -> Vec<NodeId> {
        let mut ancestors = Vec::with_capacity(16);
        let mut current = Some(node_id);
        while let Some(id) = current {
            ancestors.push(id);
            current = self.nodes[id].parent;
        }
        ancestors.reverse();
        ancestors
    }

    /// Collect all inline root nodes between start_node and end_node in document order.
    /// Both start and end are assumed to be inline roots.
    /// Returns the nodes in document order (from first to last).
    ///
    /// This walks the layout tree, which holds the anonymous blocks that wrap text next
    /// to a block. Placed by their parents among the DOM nodes instead, an anonymous
    /// block after a block comes out before it, and a range from the block into that
    /// text ran backwards and selected nothing.
    pub fn collect_inline_roots_in_range(
        &self,
        start_node: NodeId,
        end_node: NodeId,
    ) -> Vec<NodeId> {
        let mut result = Vec::new();
        let mut inside = false;
        let mut stack = vec![self.root_node().id];
        while let Some(node_id) = stack.pop() {
            let node = &self.nodes[node_id];
            let endpoint = node_id == start_node || node_id == end_node;
            if (inside || endpoint) && node.flags.is_inline_root() {
                result.push(node_id);
            }
            if endpoint {
                if inside || start_node == end_node {
                    return result;
                }
                inside = true;
            }
            if let Some(children) = node.layout_children.borrow().as_deref() {
                stack.extend(children.iter().rev().copied());
            }
        }
        result
    }
}

#[cfg(test)]
mod inline_root_range_tests {
    use crate::{BaseDocument, DocumentConfig, NodeId, qual_name};
    use blitz_traits::shell::{ColorScheme, Viewport};

    /// `<body><blockquote>Quote</blockquote>After</body>`, made by hand because the
    /// HTML parser lives in blitz-html, with the blockquote and the anonymous block
    /// that holds the text after it.
    fn quote_then_text() -> (BaseDocument, NodeId, NodeId) {
        let mut doc = BaseDocument::new(DocumentConfig {
            viewport: Some(Viewport::new(400, 300, 1.0, ColorScheme::Light)),
            ..Default::default()
        });
        let root_id = doc.root_node().id;
        let mut mutator = doc.mutate();
        let html = mutator.create_element(qual_name!("html", html), vec![]);
        let body = mutator.create_element(qual_name!("body", html), vec![]);
        let quote = mutator.create_element(qual_name!("blockquote", html), vec![]);
        let quoted = mutator.create_text_node("Quote");
        mutator.append_children(quote, &[quoted]);
        let after = mutator.create_text_node("After");
        mutator.append_children(body, &[quote, after]);
        mutator.append_children(html, &[body]);
        mutator.append_children(root_id, &[html]);
        drop(mutator);
        doc.resolve(0.0);
        let anonymous = doc.nodes[body]
            .layout_children
            .borrow()
            .as_deref()
            .unwrap_or_default()
            .iter()
            .copied()
            .find(|&id| doc.nodes[id].is_anonymous())
            .expect("the text after the blockquote is in an anonymous block");
        (doc, quote, anonymous)
    }

    #[test]
    fn a_range_from_a_block_into_the_text_after_it_holds_both() {
        let (doc, quote, anonymous) = quote_then_text();
        assert_eq!(
            doc.collect_inline_roots_in_range(quote, anonymous),
            [quote, anonymous]
        );
        assert_eq!(
            doc.collect_inline_roots_in_range(anonymous, quote),
            [quote, anonymous]
        );
        assert_eq!(doc.collect_inline_roots_in_range(quote, quote), [quote]);
    }
}
