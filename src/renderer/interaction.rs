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
        match kind {
            "down" | "long-press" => {
                self.pointer_down = true;
                self.press_link = link.map(|url| (x, y, url));
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
            }
            _ => {}
        }
        if let (Some((a, ai)), Some((f, fi))) = (self.selection_anchor, self.selection_focus) {
            email.document.set_text_selection(a, ai, f, fi);
        }
        let changed = before != email.document.get_text_selection_ranges();
        self.paint_dirty |= changed;
        changed
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
        let Some(email) = self.email.as_mut() else {
            return;
        };
        let mut roots = Vec::new();
        email.document.visit(|id, node| {
            if visible(node)
                && node
                    .element_data()
                    .and_then(|e| e.inline_layout_data.as_ref())
                    .is_some_and(|l| !l.text.is_empty())
            {
                roots.push(id);
            }
        });
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
            Key::ArrowLeft if word => selection.previous_visual_word(&inline.layout, false),
            Key::ArrowRight if word => selection.next_visual_word(&inline.layout, false),
            Key::ArrowLeft => selection.previous_visual(&inline.layout, false),
            Key::ArrowRight => selection.next_visual(&inline.layout, false),
            Key::ArrowUp => selection.previous_line(&inline.layout, false),
            Key::ArrowDown => selection.next_line(&inline.layout, false),
            Key::Home => selection.line_start(&inline.layout, false),
            Key::End => selection.line_end(&inline.layout, false),
            Key::PageUp => selection.move_lines(&inline.layout, -20, false),
            Key::PageDown => selection.move_lines(&inline.layout, 20, false),
            _ => return,
        };
        let mut focus = (id, next.focus().index());
        let backwards = matches!(key, Key::ArrowLeft | Key::ArrowUp | Key::PageUp | Key::Home);
        if word && matches!(key, Key::Home | Key::End) {
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
        let Some(email) = self.email.as_mut() else {
            return (0, 0, None);
        };
        if query != self.find_query || self.find_revision != self.metadata_revision {
            self.find_revision = self.metadata_revision;
            self.find_query = query.into();
            self.find_matches.clear();
            self.find_index = 0;
            if !query.is_empty() {
                email.document.visit(|id, node| {
                    if !visible(node) || self.find_matches.len() >= 2000 {
                        return;
                    }
                    let Some(inline) = node
                        .element_data()
                        .and_then(|e| e.inline_layout_data.as_ref())
                    else {
                        return;
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
                    for (i, m) in folded.match_indices(&query.to_lowercase()) {
                        let start = offsets[i];
                        let last = offsets[i + m.len() - 1];
                        let end = last + inline.text[last..].chars().next().unwrap().len_utf8();
                        self.find_matches.push((id, start, end));
                        if self.find_matches.len() >= 2000 {
                            break;
                        }
                    }
                });
            }
        } else if !self.find_matches.is_empty() {
            self.find_index = (self.find_index as i32 + direction)
                .rem_euclid(self.find_matches.len() as i32) as usize;
        }
        if query.is_empty() {
            email.document.clear_text_selection();
            self.selection_anchor = None;
            self.selection_focus = None;
            self.paint_dirty = true;
        }
        let count = self.find_matches.len();
        let y = if let Some(&(id, start, end)) = self.find_matches.get(self.find_index) {
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
        email.document.visit(|id, node| {
            if let Some(inline) = node
                .element_data()
                .and_then(|e| e.inline_layout_data.as_ref())
            {
                if id == anchor.0 {
                    start = Some(offset + anchor.1);
                }
                if id == focus.0 {
                    end = Some(offset + focus.1);
                }
                offset += inline.text.len();
            }
        });
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
        email.document.visit(|id, node| {
            if let Some(inline) = node
                .element_data()
                .and_then(|e| e.inline_layout_data.as_ref())
            {
                let len = inline.text.len();
                if start >= offset && start <= offset + len && anchor.is_none() {
                    anchor = Some((id, inline.text.floor_char_boundary(start - offset)));
                }
                if end >= offset && end <= offset + len && focus.is_none() {
                    focus = Some((id, inline.text.floor_char_boundary(end - offset)));
                }
                offset += len;
            }
        });
        if let (Some((a, ai)), Some((f, fi))) = (anchor, focus) {
            email.document.set_text_selection(a, ai, f, fi);
            self.selection_anchor = anchor;
            self.selection_focus = focus;
            self.paint_dirty = true;
        }
    }
}
