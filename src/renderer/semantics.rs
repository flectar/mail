use super::*;
use style::computed_values::visibility::T as Visibility;

pub(super) fn visible(node: &blitz_dom::Node) -> bool {
    node.primary_styles().is_none_or(|style| {
        !style.clone_display().is_none() && style.clone_visibility() == Visibility::Visible
    })
}
/// Elements that hold no text of the message.
fn metadata(node: &blitz_dom::Node) -> bool {
    node.element_data().is_some_and(|e| {
        matches!(
            e.name.local.as_str(),
            "head" | "style" | "script" | "title" | "template"
        )
    })
}
fn excluded(node: &blitz_dom::Node) -> bool {
    metadata(node) || !visible(node)
}
fn boundary(output: &mut String, count: usize) {
    while output.ends_with(' ') || output.ends_with('\t') {
        output.pop();
    }
    if !output.is_empty() {
        let existing = output.chars().rev().take_while(|c| *c == '\n').count();
        for _ in existing..count {
            output.push('\n');
        }
    }
}
/// The message's text. Plain text has no summary to open a closed section
/// with, so it shows what the section holds, as mail clients without
/// collapsible sections do.
pub(super) fn plain_text(document: &HtmlDocument) -> String {
    text_under(document, document.root_node().id, true)
}
/// The text under `root` that the reader shows.
pub(super) fn subtree_text(document: &HtmlDocument, root: NodeId) -> String {
    text_under(document, root, false)
}
fn text_under(document: &HtmlDocument, root: NodeId, closed_sections: bool) -> String {
    let mut output = String::new();
    let mut stack = vec![(root, false, false)];
    while let Some((id, end, pre)) = stack.pop() {
        let Some(node) = document.get_node(id) else {
            continue;
        };
        if metadata(node)
            || (!visible(node)
                && !(closed_sections && details::hidden_by_closed_section(document, id)))
        {
            continue;
        }
        let tag = node
            .element_data()
            .map(|e| e.name.local.as_str())
            .unwrap_or("");
        let block = matches!(
            tag,
            "p" | "div"
                | "section"
                | "article"
                | "blockquote"
                | "h1"
                | "h2"
                | "h3"
                | "h4"
                | "h5"
                | "h6"
                | "ul"
                | "ol"
                | "pre"
        );
        if end {
            if block {
                boundary(&mut output, 2);
            } else if matches!(tag, "li" | "tr") {
                boundary(&mut output, 1);
            } else if matches!(tag, "td" | "th") {
                output.push('\t');
            }
            continue;
        }
        if block {
            boundary(&mut output, 2);
        }
        if tag == "br" {
            output.push('\n');
        }
        if tag == "li" {
            boundary(&mut output, 1);
            output.push_str("• ");
        }
        if tag == "img"
            && let Some(alt) = node.data.attr(local_name!("alt"))
        {
            output.push_str(alt);
        }
        if let Some(text) = node.text_data() {
            if pre {
                output.push_str(&text.content);
            } else {
                for c in text.content.chars() {
                    if c.is_whitespace() {
                        if !output.is_empty() && !output.ends_with(char::is_whitespace) {
                            output.push(' ');
                        }
                    } else {
                        output.push(c);
                    }
                }
            }
        }
        stack.push((id, true, pre));
        stack.extend(
            node.children
                .iter()
                .rev()
                .map(|id| (*id, false, pre || tag == "pre")),
        );
    }
    output.trim().to_owned()
}

impl GpuEmailRenderer {
    pub fn reader_items(&self) -> Vec<crate::ReaderItem> {
        let Some(email) = &self.email else {
            return vec![];
        };
        let sections = details::sections(&email.document);
        let mut items = Vec::new();
        email.document.visit(|id, node| {
            let chain = email.document.node_chain(id);
            if !visible(node)
                || chain
                    .iter()
                    .any(|id| email.document.get_node(*id).is_some_and(excluded))
                // A summary is one button, so nothing inside it is listed apart.
                || chain[1..]
                    .iter()
                    .any(|&id| details::summarized_section(&email.document, id).is_some())
            {
                return;
            }
            if let Some((section, open)) = details::summarized_section(&email.document, id) {
                items.push(crate::ReaderItem {
                    name: subtree_text(&email.document, id).into(),
                    kind: "section".into(),
                    section: sections
                        .iter()
                        .position(|&listed| listed == section)
                        .unwrap_or_default() as i32,
                    open,
                    y: node.absolute_position(0.0, 0.0).y * self.zoom,
                    ..Default::default()
                });
                return;
            }
            let Some(e) = node.element_data() else {
                return;
            };
            let tag = e.name.local.as_str();
            let semantic = matches!(
                tag,
                "p" | "h1"
                    | "h2"
                    | "h3"
                    | "h4"
                    | "h5"
                    | "h6"
                    | "li"
                    | "tr"
                    | "img"
                    | "a"
                    | "pre"
                    | "blockquote"
            );
            let inline_root = e
                .inline_layout_data
                .as_ref()
                .filter(|inline| !inline.text.trim().is_empty());
            if !semantic && inline_root.is_none() {
                return;
            }
            // Nested anchors are separate actionable records; paragraphs keep
            // their complete sentence for a useful reading order.
            let name = if semantic {
                subtree_text(&email.document, id)
            } else {
                inline_root.unwrap().text.clone()
            };
            if name.is_empty() {
                return;
            }
            let url = if tag == "a" {
                node.data
                    .attr(local_name!("href"))
                    .and_then(browser_url)
                    .unwrap_or_default()
            } else {
                String::new()
            };
            items.push(crate::ReaderItem {
                name: name.into(),
                url: url.into(),
                kind: (if tag.starts_with('h') {
                    "heading"
                } else if tag == "img" {
                    "image"
                } else {
                    tag
                })
                .into(),
                y: node.absolute_position(0.0, 0.0).y * self.zoom,
                ..Default::default()
            });
        });
        let shown = subtree_text(&email.document, email.document.root_node().id);
        if !shown.is_empty() {
            // Mixed inline/block containers can leave visible text in an
            // anonymous layout box with no semantic element of its own. A
            // reader projection built only from retained element layouts
            // would silently omit that text. Verify that the non-action rows
            // partition the complete shown text; if they do not, use the
            // authoritative text projection and retain link and section
            // actions.
            let projected = items
                .iter()
                .filter(|item| item.url.is_empty())
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            let normalized = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
            if normalized(&projected) != normalized(&shown) {
                items.retain(|item| !item.url.is_empty() || item.kind == "section");
                items.insert(
                    0,
                    crate::ReaderItem {
                        name: shown.into(),
                        kind: "text".into(),
                        ..Default::default()
                    },
                );
            }
        }
        items
    }
    pub fn plain_text(&self) -> String {
        self.email
            .as_ref()
            .map(|e| plain_text(&e.document))
            .unwrap_or_default()
    }
    pub fn link_at(&self, x: f32, y: f32) -> Option<String> {
        let email = self.email.as_ref()?;
        let hit = email.document.hit(x / self.zoom, y / self.zoom)?;
        anchor_url_for_node(&email.document, hit.node_id)
    }
    /// Where a link to `fragment` within the message scrolls to. Like a
    /// browser, it targets the first element with that id, or else the first
    /// `<a>` with that name, and opens the closed sections around it.
    pub fn fragment_y(&mut self, fragment: &str) -> Option<f32> {
        let email = self.email.as_mut()?;
        let value = url::Url::parse(&format!("https://email.invalid/{fragment}")).ok()?;
        let encoded = value.fragment()?;
        let fragment = percent_encoding::percent_decode_str(encoded)
            .decode_utf8()
            .ok()?;
        if fragment.is_empty() {
            return Some(0.0);
        }
        let (mut by_id, mut by_name) = (None, None);
        email.document.visit(|id, node| {
            if by_id.is_none() && node.data.attr(local_name!("id")) == Some(fragment.as_ref()) {
                by_id = Some(id);
            }
            if by_name.is_none()
                && node.data.is_element_with_tag_name(&local_name!("a"))
                && node.data.attr(local_name!("name")) == Some(fragment.as_ref())
            {
                by_name = Some(id);
            }
        });
        let target = by_id.or(by_name)?;
        if details::reveal(&mut email.document, target) {
            self.relayout_now();
        }
        let email = self.email.as_ref()?;
        Some(
            email
                .document
                .get_node(target)?
                .absolute_position(0.0, 0.0)
                .y
                * self.zoom,
        )
    }
}

impl GpuEmailRenderer {
    pub fn failed_images(&self) -> i32 {
        let Some(email) = &self.email else {
            return 0;
        };
        let ledger = self
            .resources
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut count = 0;
        email.document.visit(|id, node| {
            let Some(element) = node.element_data() else {
                return;
            };
            if element.name.local.as_str() != "img"
                || element.raster_image_data().is_some()
                || email
                    .document
                    .node_chain(id)
                    .iter()
                    .any(|id| email.document.get_node(*id).is_some_and(excluded))
            {
                return;
            }
            let source = node.data.attr(local_name!("src")).unwrap_or("");
            if !matches!(
                ledger.get(&crate::remote::resource_key(email.document.id(), source)),
                Some(crate::remote::ResourceState::Loading | crate::remote::ResourceState::Blocked)
            ) {
                count += 1;
            }
        });
        count
    }
    pub fn image_placeholders(&self) -> Vec<crate::ReaderImage> {
        let Some(email) = &self.email else {
            return vec![];
        };
        let ledger = self
            .resources
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut items = Vec::new();
        email.document.visit(|id, node| {
            if !visible(node)
                || email
                    .document
                    .node_chain(id)
                    .iter()
                    .any(|id| email.document.get_node(*id).is_some_and(excluded))
            {
                return;
            }
            let Some(element) = node.element_data() else {
                return;
            };
            if element.name.local.as_str() != "img" || element.raster_image_data().is_some() {
                return;
            }
            let pos = node.absolute_position(0.0, 0.0);
            let size = node.final_layout().size;
            if size.width < 4.0 || size.height < 4.0 {
                return;
            }
            let source = node.data.attr(local_name!("src")).unwrap_or("");
            let state = match ledger.get(&crate::remote::resource_key(email.document.id(), source))
            {
                Some(crate::remote::ResourceState::Loading) => "loading",
                Some(crate::remote::ResourceState::Blocked) => "blocked",
                Some(crate::remote::ResourceState::Limited) => "limited",
                _ => "failed",
            };
            items.push(crate::ReaderImage {
                x: pos.x * self.zoom,
                y: pos.y * self.zoom,
                width: size.width * self.zoom,
                height: size.height * self.zoom,
                alt: node.data.attr(local_name!("alt")).unwrap_or("").into(),
                state: state.into(),
            });
        });
        items
    }
}

impl GpuEmailRenderer {
    /// Build a small passive clipboard fragment in logical text order. Never
    /// copy email CSS, event attributes, or remote image references.
    pub fn selected_html(&self) -> String {
        let Some(email) = &self.email else {
            return String::new();
        };
        let mut html = String::new();
        for (id, start, end) in email.document.get_text_selection_ranges() {
            let Some(inline) = email
                .document
                .get_node(id)
                .and_then(|n| n.element_data())
                .and_then(|e| e.inline_layout_data.as_ref())
            else {
                continue;
            };
            let mut spans = BTreeMap::new();
            for line in inline.layout.lines() {
                for item in line.items() {
                    if let PositionedLayoutItem::GlyphRun(run) = item {
                        let range = run.run().text_range();
                        let a = range.start.max(start);
                        let b = range.end.min(end);
                        if a < b {
                            spans.insert(a, (b, run.style().brush.id));
                        }
                    }
                }
            }
            html.push_str(r#"<p style="white-space:pre-wrap">"#);
            let mut cursor = start;
            for (a, (b, node_id)) in spans {
                if a > cursor
                    && let Some(text) = inline.text.get(cursor..a)
                {
                    html.push_str(&crate::email_document::escape(text));
                }
                let a = a.max(cursor);
                if a >= b {
                    continue;
                }
                if let Some(text) = inline.text.get(a..b) {
                    let url = anchor_url_for_node(&email.document, node_id);
                    if let Some(url) = &url {
                        html.push_str(&format!(
                            r#"<a href="{}">"#,
                            crate::email_document::escape(url)
                        ));
                    }
                    let tags: Vec<_> = email
                        .document
                        .node_chain(node_id)
                        .iter()
                        .filter_map(|id| {
                            email
                                .document
                                .get_node(*id)
                                .and_then(|n| n.element_data())
                                .map(|e| e.name.local.to_string())
                        })
                        .collect();
                    let bold = tags.iter().any(|t| matches!(t.as_str(), "b" | "strong"));
                    let italic = tags.iter().any(|t| matches!(t.as_str(), "i" | "em"));
                    if bold {
                        html.push_str("<strong>");
                    }
                    if italic {
                        html.push_str("<em>");
                    }
                    html.push_str(&crate::email_document::escape(text));
                    if italic {
                        html.push_str("</em>");
                    }
                    if bold {
                        html.push_str("</strong>");
                    }
                    if url.is_some() {
                        html.push_str("</a>");
                    }
                }
                cursor = b;
            }
            if let Some(text) = inline.text.get(cursor..end) {
                html.push_str(&crate::email_document::escape(text));
            }
            html.push_str("</p>");
        }
        html
    }
}
