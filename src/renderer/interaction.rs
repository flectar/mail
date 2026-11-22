use super::details::{self, FindMatch, Searchable};
use super::*;
use parley::layout::{Affinity, Cursor, Selection};
use unicode_segmentation::UnicodeSegmentation;

impl GpuEmailRenderer {
    pub(super) fn pointer_input(
        &mut self,
        x: f32,
        y: f32,
        kind: &str,
        mods: InputModifiers,
    ) -> bool {
        if kind == "move" && !self.pointer_down {
            return false;
        }
        let link = self.link_at(x * self.zoom, y * self.zoom);
        let Some(email) = self.email.as_mut() else {
            return false;
        };
        let before = email.document.get_text_selection_ranges();
        let mut toggled = None;
        match kind {
            "down" | "long-press" => {
                self.pointer_down = true;
                self.press_link = link.map(|url| (x, y, url));
                self.press_section = (kind == "down")
                    .then(|| details::clicked_section(&email.document, x, y))
                    .flatten()
                    .map(|section| (x, y, section));
                let now = Instant::now();
                self.click_count = if self.last_pointer_down.is_some_and(|(time, px, py)| {
                    now.duration_since(time) < Duration::from_millis(500)
                        && (px - x).abs() < 4.0
                        && (py - y).abs() < 4.0
                }) {
                    self.click_count % 3 + 1
                } else {
                    1
                };
                self.last_pointer_down = Some((now, x, y));
                if kind == "long-press" {
                    self.click_count = 2;
                }
                if let Some((id, offset)) = email.document.find_text_position(x, y) {
                    if !mods.shift || self.selection_anchor.is_none() {
                        self.selection_anchor = Some((id, offset));
                    }
                    self.selection_focus = Some((id, offset));
                    if self.click_count >= 2 {
                        self.press_link = None;
                        if let Some(inline) = email
                            .document
                            .get_node(id)
                            .and_then(|n| n.element_data())
                            .and_then(|e| e.inline_layout_data.as_ref())
                        {
                            let range = if self.click_count == 3 {
                                0..inline.text.len()
                            } else {
                                inline
                                    .text
                                    .split_word_bound_indices()
                                    .find(|(i, w)| *i <= offset && offset < i + w.len())
                                    .map(|(i, w)| i..i + w.len())
                                    .unwrap_or(offset..offset)
                            };
                            self.selection_anchor = Some((id, range.start));
                            self.selection_focus = Some((id, range.end));
                        }
                    }
                } else {
                    self.selection_anchor = None;
                    self.selection_focus = None;
                    email.document.clear_text_selection();
                }
            }
            "move" if self.pointer_down => {
                if self
                    .press_link
                    .as_ref()
                    .is_some_and(|(px, py, _)| (px - x).abs() > 3.0 || (py - y).abs() > 3.0)
                {
                    self.press_link = None;
                }
                if self
                    .press_section
                    .is_some_and(|(px, py, _)| (px - x).abs() > 3.0 || (py - y).abs() > 3.0)
                {
                    self.press_section = None;
                }
                if let Some(pos) = email.document.find_text_position(x, y) {
                    self.selection_focus = Some(pos);
                }
            }
            "up" => {
                self.pointer_down = false;
                if let Some((_, _, url)) = self.press_link.take()
                    && link.as_ref() == Some(&url)
                    && self.click_count == 1
                    && !mods.shift
                {
                    self.activation = Some(url);
                }
                if let Some((_, _, section)) = self.press_section.take()
                    && !mods.shift
                    && details::clicked_section(&email.document, x, y) == Some(section)
                {
                    toggled = Some(section);
                }
            }
            "extend-start" | "extend-end" => {
                if let Some(pos) = email.document.find_text_position(x, y) {
                    if kind == "extend-start" {
                        self.selection_anchor = Some(pos);
                    } else {
                        self.selection_focus = Some(pos);
                    }
                }
            }
            "cancel" => {
                self.pointer_down = false;
                self.press_link = None;
                self.press_section = None;
            }
            _ => {}
        }
        if let (Some((a, ai)), Some((f, fi))) = (self.selection_anchor, self.selection_focus) {
            email.document.set_text_selection(a, ai, f, fi);
        }
        let changed = before != email.document.get_text_selection_ranges();
        self.paint_dirty |= changed;
        if let Some(section) = toggled {
            self.toggle_section_node(section);
            return true;
        }
        changed
    }
    /// Open or close the message's `index`th section, as reader mode's button
    /// for its summary does.
    pub fn toggle_section(&mut self, index: usize) -> bool {
        let Some(email) = self.email.as_mut() else {
            return false;
        };
        let Some(&section) = details::sections(&email.document).get(index) else {
            return false;
        };
        self.toggle_section_node(section);
        true
    }
    fn toggle_section_node(&mut self, section: NodeId) {
        let Some(email) = self.email.as_mut() else {
            return;
        };
        // The selection could lie in what closes, and a click selects nothing.
        email.document.clear_text_selection();
        self.selection_anchor = None;
        self.selection_focus = None;
        details::toggle(&mut email.document, section);
        self.relayout_now();
    }
    pub fn take_activation(&mut self) -> Option<String> {
        self.activation.take()
    }
    pub fn selection_active(&self) -> bool {
        self.pointer_down
    }
    pub fn set_zoom(&mut self, zoom: f32) {
        self.zoom = zoom.clamp(0.5, 3.0);
        self.last_size = None;
        self.dirty = true;
    }
    pub fn clear_selection(&mut self) {
        if let Some(email) = self.email.as_mut() {
            email.document.clear_text_selection();
        }
        self.selection_anchor = None;
        self.selection_focus = None;
        self.paint_dirty = true;
    }
    pub(super) fn move_selection(&mut self, key: Key, mods: InputModifiers) {
        let Key::Named(key) = key else {
            return;
        };
        let Some(email) = self.email.as_mut() else {
            return;
        };
        // In layout order, which includes the anonymous blocks that hold text
        // next to a block.
        let roots: Vec<NodeId> = details::inline_roots(&email.document)
            .into_iter()
            .filter(|&id| {
                email
                    .document
                    .get_node(id)
                    .and_then(|node| node.element_data())
                    .and_then(|e| e.inline_layout_data.as_ref())
                    .is_some_and(|l| !l.text.is_empty())
            })
            .collect();
        let Some(first) = roots.first().copied() else {
            return;
        };
        let (id, offset) = self.selection_focus.unwrap_or((first, 0));
        let Some(inline) = email
            .document
            .get_node(id)
            .and_then(|n| n.element_data())
            .and_then(|e| e.inline_layout_data.as_ref())
        else {
            return;
        };
        let selection = Selection::from_byte_index(&inline.layout, offset, Affinity::Downstream);
        let word = mods.control || mods.alt || mods.meta;
        let next = match key {
            NamedKey::ArrowLeft if word => selection.previous_visual_word(&inline.layout, false),
            NamedKey::ArrowRight if word => selection.next_visual_word(&inline.layout, false),
            NamedKey::ArrowLeft => selection.previous_visual(&inline.layout, false),
            NamedKey::ArrowRight => selection.next_visual(&inline.layout, false),
            NamedKey::ArrowUp => selection.previous_line(&inline.layout, false),
            NamedKey::ArrowDown => selection.next_line(&inline.layout, false),
            NamedKey::Home => selection.line_start(&inline.layout, false),
            NamedKey::End => selection.line_end(&inline.layout, false),
            NamedKey::PageUp => selection.move_lines(&inline.layout, -20, false),
            NamedKey::PageDown => selection.move_lines(&inline.layout, 20, false),
            _ => return,
        };
        let mut focus = (id, next.focus().index());
        let backwards = matches!(
            key,
            NamedKey::ArrowLeft | NamedKey::ArrowUp | NamedKey::PageUp | NamedKey::Home
        );
        if word && matches!(key, NamedKey::Home | NamedKey::End) {
            let target = if backwards {
                first
            } else {
                *roots.last().unwrap()
            };
            focus = (
                target,
                if backwards {
                    0
                } else {
                    email
                        .document
                        .get_node(target)
                        .unwrap()
                        .element_data()
                        .unwrap()
                        .inline_layout_data
                        .as_ref()
                        .unwrap()
                        .text
                        .len()
                },
            );
        } else if focus.1 == offset {
            let index = roots.iter().position(|n| *n == id).unwrap_or(0);
            let target = if backwards {
                index.checked_sub(1)
            } else {
                (index + 1 < roots.len()).then_some(index + 1)
            };
            if let Some(target) = target {
                let id = roots[target];
                let len = email
                    .document
                    .get_node(id)
                    .unwrap()
                    .element_data()
                    .unwrap()
                    .inline_layout_data
                    .as_ref()
                    .unwrap()
                    .text
                    .len();
                focus = (id, if backwards { len } else { 0 });
            }
        }
        if !mods.shift || self.selection_anchor.is_none() {
            self.selection_anchor = Some(if mods.shift { (id, offset) } else { focus });
        }
        self.selection_focus = Some(focus);
        let (a, ai) = self.selection_anchor.unwrap();
        email.document.set_text_selection(a, ai, focus.0, focus.1);
        self.paint_dirty = true;
    }
    pub fn find(&mut self, query: &str, direction: i32) -> (usize, usize, Option<f32>) {
        if self.email.is_none() {
            return (0, 0, None);
        }
        if query != self.find_query {
            self.find_query = query.into();
            self.find_index = 0;
            self.collect_find_matches();
        } else {
            if self.find_revision != self.metadata_revision {
                self.collect_find_matches();
            }
            if !self.find_matches.is_empty() {
                self.find_index = (self.find_index as i32 + direction)
                    .rem_euclid(self.find_matches.len() as i32)
                    as usize;
            }
        }
        // A match in a closed section opens it, and then any closed section
        // that the match is still in (HTML, "Interaction with details").
        for _ in 0..16 {
            let Some(&FindMatch::Hidden(section)) = self.find_matches.get(self.find_index) else {
                break;
            };
            let Some(email) = self.email.as_mut() else {
                break;
            };
            details::open_section(&mut email.document, section);
            if !self.relayout_now() {
                break;
            }
            self.collect_find_matches();
        }
        let Some(email) = self.email.as_mut() else {
            return (0, 0, None);
        };
        if query.is_empty() {
            email.document.clear_text_selection();
            self.selection_anchor = None;
            self.selection_focus = None;
            self.paint_dirty = true;
        }
        let count = self.find_matches.len();
        let y = if let Some(&FindMatch::Shown(id, start, end)) =
            self.find_matches.get(self.find_index)
        {
            email.document.set_text_selection(id, start, id, end);
            self.selection_anchor = Some((id, start));
            self.selection_focus = Some((id, end));
            self.paint_dirty = true;
            email.document.get_node(id).and_then(|n| {
                let inline = n.element_data()?.inline_layout_data.as_ref()?;
                let cursor = Cursor::from_byte_index(&inline.layout, start, Affinity::Downstream);
                Some(
                    (n.absolute_position(0.0, 0.0).y
                        + cursor.geometry(&inline.layout, 1.0).y0 as f32 / inline.layout.scale())
                        * self.zoom,
                )
            })
        } else {
            None
        };
        (if count == 0 { 0 } else { self.find_index + 1 }, count, y)
    }

    /// Find the current query in the message as it is laid out now, keeping
    /// the current match's place in the list.
    fn collect_find_matches(&mut self) {
        self.find_revision = self.metadata_revision;
        self.find_matches.clear();
        let Some(email) = self.email.as_ref() else {
            return;
        };
        let query = self.find_query.to_lowercase();
        if query.is_empty() {
            return;
        }
        for run in details::searchable(&email.document) {
            if self.find_matches.len() >= 2000 {
                break;
            }
            match run {
                Searchable::Shown(id) => {
                    let Some(inline) = email
                        .document
                        .get_node(id)
                        .and_then(|node| node.element_data())
                        .and_then(|element| element.inline_layout_data.as_ref())
                    else {
                        continue;
                    };
                    // Map folded characters back to original UTF-8 offsets.
                    let mut folded = String::new();
                    let mut offsets = Vec::new();
                    for (i, c) in inline.text.char_indices() {
                        for lower in c.to_lowercase() {
                            for _ in 0..lower.len_utf8() {
                                offsets.push(i);
                            }
                            folded.push(lower);
                        }
                    }
                    offsets.push(inline.text.len());
                    for (i, m) in folded.match_indices(&query) {
                        let start = offsets[i];
                        let last = offsets[i + m.len() - 1];
                        let end = last + inline.text[last..].chars().next().unwrap().len_utf8();
                        self.find_matches.push(FindMatch::Shown(id, start, end));
                        if self.find_matches.len() >= 2000 {
                            break;
                        }
                    }
                }
                Searchable::Hidden { details, blocks } => {
                    for block in blocks {
                        let found = block.to_lowercase().matches(&query).count();
                        self.find_matches
                            .extend(std::iter::repeat_n(FindMatch::Hidden(details), found));
                    }
                    self.find_matches.truncate(2000);
                }
            }
        }
        self.find_index = self
            .find_index
            .min(self.find_matches.len().saturating_sub(1));
    }
}

impl GpuEmailRenderer {
    pub fn selection_carets(&self) -> (crate::ReaderCaret, crate::ReaderCaret) {
        let caret = |pos: Option<(NodeId, usize)>| -> crate::ReaderCaret {
            let Some((id, offset)) = pos else {
                return Default::default();
            };
            let Some(node) = self.email.as_ref().and_then(|e| e.document.get_node(id)) else {
                return Default::default();
            };
            let Some(inline) = node
                .element_data()
                .and_then(|e| e.inline_layout_data.as_ref())
            else {
                return Default::default();
            };
            let rect = Cursor::from_byte_index(&inline.layout, offset, Affinity::Downstream)
                .geometry(&inline.layout, 1.0);
            let origin = node.absolute_position(0.0, 0.0);
            let layout = node.final_layout();
            crate::ReaderCaret {
                x: (origin.x
                    + layout.border.left
                    + layout.padding.left
                    + rect.x0 as f32 / inline.layout.scale())
                    * self.zoom,
                y: (origin.y
                    + layout.border.top
                    + layout.padding.top
                    + rect.y0 as f32 / inline.layout.scale())
                    * self.zoom,
                height: (rect.y1 - rect.y0) as f32 / inline.layout.scale() * self.zoom,
                valid: true,
            }
        };
        (caret(self.selection_anchor), caret(self.selection_focus))
    }
    pub fn selection_bookmark(&self) -> Option<(usize, usize)> {
        let email = self.email.as_ref()?;
        let (anchor, focus) = (self.selection_anchor?, self.selection_focus?);
        let (mut start, mut end, mut offset) = (None, None, 0);
        for (id, len) in text_runs(&email.document) {
            if id == anchor.0 {
                start = Some(offset + anchor.1);
            }
            if id == focus.0 {
                end = Some(offset + focus.1);
            }
            offset += len;
        }
        Some((start?, end?))
    }
    pub fn restore_selection(&mut self, bookmark: Option<(usize, usize)>) {
        let Some((start, end)) = bookmark else {
            return;
        };
        let Some(email) = self.email.as_mut() else {
            return;
        };
        let mut offset = 0;
        let (mut anchor, mut focus) = (None, None);
        for (id, len) in text_runs(&email.document) {
            let boundary = |at: usize| {
                email
                    .document
                    .get_node(id)
                    .and_then(|node| node.element_data())
                    .and_then(|e| e.inline_layout_data.as_ref())
                    .map_or(at, |inline| inline.text.floor_char_boundary(at))
            };
            if start >= offset && start <= offset + len && anchor.is_none() {
                anchor = Some((id, boundary(start - offset)));
            }
            if end >= offset && end <= offset + len && focus.is_none() {
                focus = Some((id, boundary(end - offset)));
            }
            offset += len;
        }
        if let (Some((a, ai)), Some((f, fi))) = (anchor, focus) {
            email.document.set_text_selection(a, ai, f, fi);
            self.selection_anchor = anchor;
            self.selection_focus = focus;
            self.paint_dirty = true;
        }
    }
}

/// The laid out runs of text in layout order with their lengths, which a
/// selection bookmark counts through.
fn text_runs(document: &blitz_dom::BaseDocument) -> Vec<(NodeId, usize)> {
    details::inline_roots(document)
        .into_iter()
        .filter_map(|id| {
            let inline = document
                .get_node(id)?
                .element_data()?
                .inline_layout_data
                .as_ref()?;
            Some((id, inline.text.len()))
        })
        .collect()
}
