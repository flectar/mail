use super::*;

pub(super) fn visible(node: &blitz_dom::Node) -> bool {
    node.primary_styles().is_none_or(|style| {
        !style.clone_display().is_none()
            && style.clone_visibility() == blitz_dom::Visibility::Visible
    })
}
fn excluded(node: &blitz_dom::Node) -> bool {
    node.element_data().is_some_and(|e| {
        matches!(
            e.name.local.as_ref(),
            "head" | "style" | "script" | "title" | "template"
        )
    }) || !visible(node)
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
pub(super) fn plain_text(document: &HtmlDocument) -> String {
    subtree_text(document, document.root_node().id)
}
pub(super) fn subtree_text(document: &HtmlDocument, root: NodeId) -> String {
    let mut output = String::new();
    let mut stack = vec![(root, false, false)];
    while let Some((id, end, pre)) = stack.pop() {
        let Some(node) = document.get_node(id) else {
            continue;
        };
        if excluded(node) {
            continue;
        }
        let tag = node
            .element_data()
            .map(|e| e.name.local.as_ref())
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
            let Some(e) = node.element_data() else {
                return;
            };
            let tag = e.name.local.as_ref();
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
            });
        });
        if items.is_empty() && !email.plain_text.is_empty() {
            items.push(crate::ReaderItem {
                name: email.plain_text.clone().into(),
                ..Default::default()
            });
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
    pub fn fragment_y(&self, fragment: &str) -> Option<f32> {
        let email = self.email.as_ref()?;
        let value = url::Url::parse(&format!("https://email.invalid/{fragment}")).ok()?;
        let encoded = value.fragment()?;
        let fragment = percent_encoding::percent_decode_str(encoded)
            .decode_utf8()
            .ok()?;
        if fragment.is_empty() {
            return Some(0.0);
        }
        let mut found = None;
        email.document.visit(|_, node| {
            if node.data.attr(local_name!("id")) == Some(fragment.as_ref())
                || node.data.attr(local_name!("name")) == Some(fragment.as_ref())
            {
                found = Some(node.absolute_position(0.0, 0.0).y * self.zoom);
            }
        });
        found
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
            if element.name.local.as_ref() != "img"
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
            if element.name.local.as_ref() != "img" || element.raster_image_data().is_some() {
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
