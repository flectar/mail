//! Collapsible sections: `<details>` with its `<summary>` (HTML, "The details
//! element"). The reader runs no scripts, so it does what a browser does for
//! them itself. A click on the summary opens or closes the section, opening
//! one member of a named group closes the others, a section without a summary
//! gets a default one, and find in page and links within the message open the
//! sections that hide what they land on.

use super::semantics::visible;
use blitz_dom::{BaseDocument, Node, NodeId, local_name, qual_name};
use std::{cell::RefCell, collections::HashSet, rc::Rc};

thread_local! {
    /// The default summary's words in the UI language.
    static DEFAULT_SUMMARY: RefCell<Rc<str>> = RefCell::new(Rc::from("Details"));
}

/// The UI language changed. Messages read from now on title a section that
/// has no summary of its own with `words`, as browsers show "Details".
pub(crate) fn set_default_summary(words: &str) {
    DEFAULT_SUMMARY.with(|summary| *summary.borrow_mut() = Rc::from(words));
}

fn is_details(node: &Node) -> bool {
    node.data.is_element_with_tag_name(&local_name!("details"))
}

fn is_open(node: &Node) -> bool {
    node.data.has_attr(local_name!("open"))
}

/// The summary of `details`: its first `<summary>` child. Any other summary is
/// content of the section.
fn summary_of(document: &BaseDocument, details: NodeId) -> Option<NodeId> {
    document
        .get_node(details)?
        .children
        .iter()
        .copied()
        .find(|&child| {
            document
                .get_node(child)
                .is_some_and(|node| node.data.is_element_with_tag_name(&local_name!("summary")))
        })
}

/// The group of sections of which only one may be open, named by `name`.
fn group(node: &Node) -> Option<&str> {
    node.data
        .attr(local_name!("name"))
        .filter(|name| !name.is_empty())
}

/// Give each section without a summary a default one, and keep only the first
/// open section of each group open, as browsers do while they parse it.
pub(super) fn prepare(document: &mut BaseDocument) {
    let mut unnamed = Vec::new();
    let mut closing = Vec::new();
    let mut open_groups = HashSet::new();
    document.visit(|id, node| {
        if !is_details(node) {
            return;
        }
        if summary_of(document, id).is_none() {
            unnamed.push(id);
        }
        if let Some(group) = group(node).filter(|_| is_open(node))
            && !open_groups.insert(group.to_owned())
        {
            closing.push(id);
        }
    });
    if unnamed.is_empty() && closing.is_empty() {
        return;
    }
    let words = DEFAULT_SUMMARY.with(|summary| summary.borrow().clone());
    let mut mutator = document.mutate();
    for details in unnamed {
        let summary = mutator.create_element(qual_name!("summary", html), Vec::new());
        let text = mutator.create_text_node(&words);
        mutator.append_children(summary, &[text]);
        mutator.prepend_nodes(details, &[summary]);
    }
    for details in closing {
        mutator.clear_attribute(details, qual_name!("open"));
    }
}

/// The section that a click at `(x, y)`, in document coordinates, opens or
/// closes. The point must be on the summary of its section, and not on a link
/// within the summary, since a link's own activation comes first.
pub(super) fn clicked_section(document: &BaseDocument, x: f32, y: f32) -> Option<NodeId> {
    let mut id = Some(document.hit(x, y)?.node_id);
    while let Some(node_id) = id {
        let node = document.get_node(node_id)?;
        if node.data.is_element_with_tag_name(&local_name!("a"))
            && node.data.has_attr(local_name!("href"))
        {
            return None;
        }
        if node.data.is_element_with_tag_name(&local_name!("summary")) {
            return summarized_section(document, node_id).map(|(details, _)| details);
        }
        id = node.parent;
    }
    None
}

/// The section that `summary` is the summary of, and whether it is open.
pub(super) fn summarized_section(
    document: &BaseDocument,
    summary: NodeId,
) -> Option<(NodeId, bool)> {
    let node = document.get_node(summary)?;
    if !node.data.is_element_with_tag_name(&local_name!("summary")) {
        return None;
    }
    let details = node.parent?;
    let section = document.get_node(details)?;
    (is_details(section) && summary_of(document, details) == Some(summary))
        .then(|| (details, is_open(section)))
}

/// The message's sections in document order, which reader mode counts.
pub(super) fn sections(document: &BaseDocument) -> Vec<NodeId> {
    let mut sections = Vec::new();
    document.visit(|id, node| {
        if is_details(node) {
            sections.push(id);
        }
    });
    sections
}

/// Whether `node` is content that a closed section hides: a child of a closed
/// `<details>` other than its summary.
pub(super) fn hidden_by_closed_section(document: &BaseDocument, node: NodeId) -> bool {
    let Some(parent) = document.get_node(node).and_then(|node| node.parent) else {
        return false;
    };
    document
        .get_node(parent)
        .is_some_and(|section| is_details(section) && !is_open(section))
        && summary_of(document, parent) != Some(node)
}

/// Open or close a section, as activating its summary does.
pub(super) fn toggle(document: &mut BaseDocument, details: NodeId) {
    match document.get_node(details) {
        Some(node) if is_open(node) => document
            .mutate()
            .clear_attribute(details, qual_name!("open")),
        Some(_) => open(document, details),
        None => {}
    }
}

/// Open a section, closing the open member of its group.
fn open(document: &mut BaseDocument, details: NodeId) {
    let mut others = Vec::new();
    if let Some(name) = document.get_node(details).and_then(group) {
        document.visit(|id, node| {
            if id != details && is_details(node) && is_open(node) && group(node) == Some(name) {
                others.push(id);
            }
        });
    }
    let mut mutator = document.mutate();
    mutator.set_attribute(details, qual_name!("open"), "");
    for other in others {
        mutator.clear_attribute(other, qual_name!("open"));
    }
}

/// Open each closed section that hides `node` (HTML's ancestor revealing
/// algorithm). Returns whether one was closed.
pub(super) fn reveal(document: &mut BaseDocument, node: NodeId) -> bool {
    let mut hiding = Vec::new();
    let mut child = node;
    while let Some(parent) = document.get_node(child).and_then(|node| node.parent) {
        if document
            .get_node(parent)
            .is_some_and(|node| is_details(node) && !is_open(node))
            && summary_of(document, parent) != Some(child)
        {
            hiding.push(parent);
        }
        child = parent;
    }
    for &details in &hiding {
        open(document, details);
    }
    !hiding.is_empty()
}

/// A match of find in page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FindMatch {
    /// In a laid out run of text: its node and the match's byte range.
    Shown(NodeId, usize, usize),
    /// In the content that this closed section hides.
    Hidden(NodeId),
}

/// Open the section `details` hides content in, as find in page does to show
/// a match there.
pub(super) fn open_section(document: &mut BaseDocument, details: NodeId) {
    if document
        .get_node(details)
        .is_some_and(|node| is_details(node) && !is_open(node))
    {
        open(document, details);
    }
}

/// What find in page searches, in document order.
pub(super) enum Searchable {
    /// A laid out run of text: a node with an inline layout.
    Shown(NodeId),
    /// The text of content that a closed section hides, which find in page
    /// searches too and opens the section for (HTML, "Interaction with
    /// details"). A block's text stays apart from the next block's, as each
    /// block is its own run once shown.
    Hidden {
        details: NodeId,
        blocks: Vec<String>,
    },
}

/// The runs of text that find in page searches. This follows the layout tree,
/// which includes the anonymous blocks that hold text next to a block, and
/// takes the content that a closed section hides after its summary.
pub(super) fn searchable(document: &BaseDocument) -> Vec<Searchable> {
    enum Step {
        Visit(NodeId),
        Hidden(NodeId),
    }
    let mut runs = Vec::new();
    let mut stack = vec![Step::Visit(document.root_node().id)];
    while let Some(step) = stack.pop() {
        let id = match step {
            Step::Visit(id) => id,
            Step::Hidden(details) => {
                let summary = summary_of(document, details);
                let children = document
                    .get_node(details)
                    .map(|node| node.children.as_slice())
                    .unwrap_or_default();
                let blocks: Vec<String> = children
                    .iter()
                    .copied()
                    .filter(|&child| Some(child) != summary)
                    .flat_map(|child| hidden_blocks(document, child))
                    .collect();
                if !blocks.is_empty() {
                    runs.push(Searchable::Hidden { details, blocks });
                }
                continue;
            }
        };
        let Some(node) = document.get_node(id) else {
            continue;
        };
        // A child of a `visibility: hidden` element may be visible again.
        let shown = visible(node);
        if shown
            && node
                .element_data()
                .is_some_and(|element| element.inline_layout_data.is_some())
        {
            runs.push(Searchable::Shown(id));
        }
        if shown && is_details(node) && !is_open(node) {
            stack.push(Step::Hidden(id));
        }
        if let Some(children) = node.layout_children.borrow().as_deref() {
            stack.extend(children.iter().rev().map(|&child| Step::Visit(child)));
        }
    }
    runs
}

/// The laid out runs of text in document order.
pub(super) fn inline_roots(document: &BaseDocument) -> Vec<NodeId> {
    searchable(document)
        .into_iter()
        .filter_map(|run| match run {
            Searchable::Shown(id) => Some(id),
            Searchable::Hidden { .. } => None,
        })
        .collect()
}

/// The text under `root` that a closed section hides, one string per block.
/// Nothing inside it has styles yet, so blocks are told by their elements.
fn hidden_blocks(document: &BaseDocument, root: NodeId) -> Vec<String> {
    const BLOCKS: &[&str] = &[
        "address",
        "article",
        "aside",
        "blockquote",
        "br",
        "caption",
        "dd",
        "details",
        "div",
        "dl",
        "dt",
        "figcaption",
        "figure",
        "footer",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "header",
        "hr",
        "li",
        "main",
        "nav",
        "ol",
        "p",
        "pre",
        "section",
        "summary",
        "table",
        "td",
        "th",
        "tr",
        "ul",
    ];
    enum Step {
        Visit(NodeId),
        Break,
    }
    let mut blocks = vec![String::new()];
    let mut stack = vec![Step::Visit(root)];
    while let Some(step) = stack.pop() {
        let id = match step {
            Step::Visit(id) => id,
            Step::Break => {
                blocks.push(String::new());
                continue;
            }
        };
        let Some(node) = document.get_node(id) else {
            continue;
        };
        if let Some(text) = node.text_data() {
            let block = blocks.last_mut().unwrap();
            if text.content.starts_with(char::is_whitespace) && !block.is_empty() {
                block.push(' ');
            }
            for (index, word) in text.content.split_whitespace().enumerate() {
                if index > 0 {
                    block.push(' ');
                }
                block.push_str(word);
            }
            if text.content.ends_with(char::is_whitespace) && !block.is_empty() {
                block.push(' ');
            }
            continue;
        }
        let Some(element) = node.element_data() else {
            continue;
        };
        let name = element.name.local.as_ref();
        if matches!(name, "style" | "script" | "template" | "title") {
            continue;
        }
        let block = BLOCKS.contains(&name);
        if block {
            stack.push(Step::Break);
        }
        stack.extend(node.children.iter().rev().map(|&child| Step::Visit(child)));
        if block {
            stack.push(Step::Break);
        }
    }
    blocks
        .into_iter()
        .map(|block| block.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|block| !block.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use blitz_dom::DocumentConfig;
    use blitz_html::HtmlDocument;
    use blitz_traits::shell::{ColorScheme, Viewport};

    fn document(html: &str) -> HtmlDocument {
        let mut document = HtmlDocument::from_html(
            html,
            DocumentConfig {
                viewport: Some(Viewport::new(600, 800, 1.0, ColorScheme::Light)),
                ..Default::default()
            },
        );
        prepare(&mut document);
        document.resolve(0.0);
        document
    }

    fn by_id(document: &BaseDocument, id: &str) -> NodeId {
        document
            .query_selector(&format!("#{id}"))
            .unwrap()
            .unwrap_or_else(|| panic!("no #{id}"))
    }

    fn opened(document: &BaseDocument, id: &str) -> bool {
        is_open(node(document, id))
    }

    fn node<'a>(document: &'a BaseDocument, id: &str) -> &'a Node {
        document.get_node(by_id(document, id)).unwrap()
    }

    /// A point inside the middle of the element's box.
    fn middle(document: &BaseDocument, id: &str) -> (f32, f32) {
        let node = node(document, id);
        let origin = node.absolute_position(0.0, 0.0);
        let size = node.final_layout().size;
        (origin.x + size.width / 2.0, origin.y + size.height / 2.0)
    }

    fn inline_text(document: &BaseDocument, id: NodeId) -> String {
        document
            .get_node(id)
            .and_then(|node| node.element_data())
            .and_then(|element| element.inline_layout_data.as_ref())
            .map(|layout| layout.text.clone())
            .unwrap_or_default()
    }

    #[test]
    fn a_section_without_a_summary_gets_the_default_one() {
        set_default_summary("Detalles");
        let document = document(r#"<details id="d"><p>Body</p></details>"#);
        set_default_summary("Details");
        let details = by_id(&document, "d");
        let summary = summary_of(&document, details).expect("a default summary");
        assert_eq!(
            document.get_node(details).unwrap().children.first(),
            Some(&summary)
        );
        assert_eq!(inline_text(&document, summary), "▸ Detalles");
    }

    #[test]
    fn only_the_first_open_member_of_a_group_stays_open() {
        let document = document(
            r#"<details id="a" name="faq" open><summary>A</summary></details>
            <details id="b" name="faq" open><summary>B</summary></details>
            <details id="c" name="other" open><summary>C</summary></details>
            <details id="d" name="" open><summary>D</summary></details>
            <details id="e" name="" open><summary>E</summary></details>"#,
        );
        assert!(opened(&document, "a"));
        assert!(!opened(&document, "b"));
        assert!(opened(&document, "c"));
        assert!(
            opened(&document, "d") && opened(&document, "e"),
            "an empty name is no group"
        );
    }

    #[test]
    fn a_click_on_the_summary_finds_its_section_but_a_link_takes_its_own() {
        let document = document(
            r#"<body style="margin:0"><details id="d"><summary id="s">
                <div id="title" style="height:40px">Title</div>
                <a id="link" href="https://example.com" style="display:block;height:40px">Link</a>
            </summary><p id="body">Body</p></details>
            <details open id="o"><summary>Open</summary><summary id="second" style="height:40px">Second</summary></details></body>"#,
        );
        let (x, y) = middle(&document, "title");
        assert_eq!(
            clicked_section(&document, x, y),
            Some(by_id(&document, "d"))
        );
        let (x, y) = middle(&document, "link");
        assert_eq!(clicked_section(&document, x, y), None);
        let (x, y) = middle(&document, "second");
        assert_eq!(
            clicked_section(&document, x, y),
            None,
            "only the first summary opens it"
        );
        let summary = node(&document, "s").absolute_position(0.0, 0.0);
        assert_eq!(
            clicked_section(&document, summary.x + 4.0, summary.y + 4.0),
            Some(by_id(&document, "d")),
            "the marker's own line belongs to the summary"
        );
    }

    #[test]
    fn opening_a_member_of_a_group_closes_the_open_one() {
        let mut document = document(
            r#"<details id="a" name="faq" open><summary>A</summary><p>One</p></details>
            <details id="b" name="faq"><summary>B</summary><p>Two</p></details>
            <details id="c"><summary>C</summary><p>Three</p></details>"#,
        );
        let b = by_id(&document, "b");
        toggle(&mut document, b);
        assert!(opened(&document, "b") && !opened(&document, "a"));
        toggle(&mut document, b);
        assert!(!opened(&document, "b") && !opened(&document, "a"));
        let c = by_id(&document, "c");
        toggle(&mut document, c);
        assert!(opened(&document, "c"));
    }

    #[test]
    fn reveal_opens_every_closed_section_around_a_node() {
        let mut document = document(
            r#"<details id="outer"><summary>Outer</summary>
                <details id="inner"><summary id="inner-summary">Inner</summary><p id="target">Target</p></details>
            </details>"#,
        );
        let summary = by_id(&document, "inner-summary");
        let target = by_id(&document, "target");
        assert!(reveal(&mut document, summary));
        assert!(opened(&document, "outer") && !opened(&document, "inner"));
        assert!(reveal(&mut document, target));
        assert!(opened(&document, "inner"));
        assert!(!reveal(&mut document, target), "nothing left to open");
    }

    #[test]
    fn find_searches_what_a_closed_section_hides_after_its_summary() {
        let document = document(
            r#"<p id="before">Before</p>
            <details id="d"><summary id="s">Sum</summary><p>Alpha</p><div>Beta  <b>bold</b></div>
                <style>p { color: red }</style><p>Gamma<br>Delta</p></details>
            <p id="after">After</p>"#,
        );
        let runs: Vec<_> = searchable(&document)
            .into_iter()
            .map(|run| match run {
                Searchable::Shown(id) => inline_text(&document, id),
                Searchable::Hidden { details, blocks } => {
                    assert_eq!(details, by_id(&document, "d"));
                    format!("hidden {blocks:?}")
                }
            })
            .filter(|text| !text.trim().is_empty())
            .collect();
        assert_eq!(
            runs,
            [
                "Before",
                "▸ Sum",
                r#"hidden ["Alpha", "Beta bold", "Gamma", "Delta"]"#,
                "After"
            ]
        );
    }

    #[test]
    fn find_reaches_text_beside_a_block() {
        let document = document(r#"<div>Loose words<p>Paragraph</p></div>"#);
        let texts: Vec<_> = searchable(&document)
            .into_iter()
            .filter_map(|run| match run {
                Searchable::Shown(id) => Some(inline_text(&document, id)),
                Searchable::Hidden { .. } => None,
            })
            .collect();
        assert!(texts.iter().any(|text| text == "Loose words"), "{texts:?}");
        assert!(texts.iter().any(|text| text == "Paragraph"), "{texts:?}");
    }
}
