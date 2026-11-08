use std::{
    borrow::Cow,
    collections::{HashSet, VecDeque},
    ops::Range,
    sync::Arc,
};

pub(crate) const BOLD: u8 = 1 << 0;
pub(crate) const ITALIC: u8 = 1 << 1;
pub(crate) const UNDERLINE: u8 = 1 << 2;
pub(crate) const STRIKE: u8 = 1 << 3;
pub(crate) const CODE: u8 = 1 << 4;
const MAX_HISTORY: usize = 128;
// The most recent edit remains undoable even when one transaction exceeds this limit.
const MAX_HISTORY_BYTES: usize = 32 * 1024 * 1024;
const MAX_TYPING_GROUP_BYTES: usize = 64;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CharacterStyle {
    pub(crate) marks: u8,
    pub(crate) link: Option<Arc<str>>,
    link_is_auto: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ComposeStyleRun {
    pub(crate) range: Range<usize>,
    pub(crate) style: CharacterStyle,
}

#[derive(Clone, Debug)]
struct Snapshot {
    text: String,
    styles: Vec<ComposeStyleRun>,
    typing_style: CharacterStyle,
    typing_override: Option<usize>,
    selection: (i32, i32),
}

#[derive(Clone, Debug)]
struct EditFragment {
    text: Arc<str>,
    span_len: usize,
    styles: Vec<ComposeStyleRun>,
}

struct StyleEditBefore {
    fragment: EditFragment,
    typing_style: CharacterStyle,
    typing_override: Option<usize>,
    selection: (i32, i32),
}

#[derive(Clone, Debug)]
struct EditTransaction {
    start: usize,
    before: EditFragment,
    after: EditFragment,
    before_typing_style: CharacterStyle,
    after_typing_style: CharacterStyle,
    before_typing_override: Option<usize>,
    after_typing_override: Option<usize>,
    before_selection: (i32, i32),
    after_selection: (i32, i32),
    retained_bytes: usize,
}

/// Estimated bytes owned by the document and its history. This excludes
/// allocator overhead, temporary edit buffers, renderer state, and process RAM.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct ComposeMemoryUsage {
    pub(crate) text_capacity: usize,
    pub(crate) style_run_bytes: usize,
    pub(crate) link_bytes: usize,
    pub(crate) undo_bytes: usize,
    pub(crate) redo_bytes: usize,
    pub(crate) total_bytes: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ActiveMarks {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub code: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ComposeSelection {
    pub start: i32,
    pub end: i32,
}

/// Rich formatting state for the native composer.
///
/// The editable string never contains markup. Styles are retained as merged
/// UTF-8 byte ranges and exposed to the cosmic-text layout surface. Slint's
/// hidden TextInput is only the platform IME/clipboard
/// bridge; it is not used to position visible text, selection, or the caret.
#[derive(Default)]
pub struct RichComposeDocument {
    text: String,
    styles: Vec<ComposeStyleRun>,
    typing_style: CharacterStyle,
    typing_override: Option<usize>,
    selection: (i32, i32),
    undo: VecDeque<EditTransaction>,
    redo: VecDeque<EditTransaction>,
    typing_group_active: bool,
    revision: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BlockKind {
    Bullet,
    Number,
    Quote,
}

impl RichComposeDocument {
    pub fn reset(&mut self) {
        let revision = self.revision.wrapping_add(1);
        *self = Self::default();
        self.revision = revision;
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn selection(&self) -> ComposeSelection {
        ComposeSelection {
            start: self.selection.0,
            end: self.selection.1,
        }
    }

    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }

    pub(crate) fn style_runs(&self) -> &[ComposeStyleRun] {
        &self.styles
    }

    pub(crate) fn typing_style(&self) -> &CharacterStyle {
        &self.typing_style
    }

    #[allow(dead_code)]
    pub(crate) fn memory_usage(&self) -> ComposeMemoryUsage {
        let mut usage = ComposeMemoryUsage {
            text_capacity: self.text.capacity(),
            style_run_bytes: self.styles.capacity() * std::mem::size_of::<ComposeStyleRun>(),
            undo_bytes: self.undo.capacity() * std::mem::size_of::<EditTransaction>(),
            redo_bytes: self.redo.capacity() * std::mem::size_of::<EditTransaction>(),
            ..Default::default()
        };
        let mut seen_text = HashSet::new();
        let mut seen_links = HashSet::new();
        count_style_links(&self.typing_style, &mut seen_links, &mut usage.link_bytes);
        for run in &self.styles {
            count_style_links(&run.style, &mut seen_links, &mut usage.link_bytes);
        }
        for (transactions, bytes) in [
            (&self.undo, &mut usage.undo_bytes),
            (&self.redo, &mut usage.redo_bytes),
        ] {
            for transaction in transactions {
                for style in [
                    &transaction.before_typing_style,
                    &transaction.after_typing_style,
                ] {
                    count_style_links(style, &mut seen_links, &mut usage.link_bytes);
                }
                for fragment in [&transaction.before, &transaction.after] {
                    *bytes += fragment.styles.capacity() * std::mem::size_of::<ComposeStyleRun>();
                    if seen_text.insert(Arc::as_ptr(&fragment.text) as *const u8 as usize) {
                        *bytes += fragment_text_bytes(&fragment.text);
                    }
                    for run in &fragment.styles {
                        count_style_links(&run.style, &mut seen_links, &mut usage.link_bytes);
                    }
                }
            }
        }
        usage.total_bytes = std::mem::size_of::<Self>()
            + usage.text_capacity
            + usage.style_run_bytes
            + usage.link_bytes
            + usage.undo_bytes
            + usage.redo_bytes;
        usage
    }

    /// Set the text a message starts with: a reply's quote, a reopened draft,
    /// a mail link's body. Undo stops here instead of removing it.
    pub fn load(&mut self, text: &str, anchor: i32, cursor: i32) -> ComposeSelection {
        let selection = self.synchronize(text, anchor, cursor);
        self.undo.clear();
        self.redo.clear();
        self.typing_group_active = false;
        selection
    }

    pub fn synchronize(&mut self, new_text: &str, anchor: i32, cursor: i32) -> ComposeSelection {
        self.synchronize_key_edit(new_text, anchor, cursor, "")
    }

    /// `key_text` must come from the immediately preceding ordinary key event.
    /// Callers pass an empty string for paste, IME, and unclassified edits.
    pub fn synchronize_key_edit(
        &mut self,
        new_text: &str,
        anchor: i32,
        cursor: i32,
        key_text: &str,
    ) -> ComposeSelection {
        let continuation = continue_list_edit(&self.text, new_text);
        let (effective_text, anchor, cursor) = match continuation {
            Some((text, position)) => (Cow::Owned(text), position, position),
            None => (Cow::Borrowed(new_text), anchor, cursor),
        };

        if effective_text.as_ref() != self.text {
            let (start, old_end, new_end) = text_change_ranges(&self.text, &effective_text);
            let inserted_len = new_end - start;
            let inserted = ComposeStyleRun {
                range: 0..inserted_len,
                style: if inserted_len > 0 {
                    self.typing_style.clone()
                } else {
                    CharacterStyle::default()
                },
            };
            let styles = replaced_style_runs(
                &self.styles,
                start,
                old_end,
                std::slice::from_ref(&inserted),
            );
            let before = self.take_snapshot();
            self.styles = styles;
            self.text = effective_text.into_owned();
            self.auto_link_urls();
            self.bump_revision();
            let selection = self.update_selection_internal(anchor, cursor, false);
            self.commit_edit_with_key(before, key_text);
            return selection;
        }
        self.update_selection(anchor, cursor)
    }

    pub fn update_selection(&mut self, anchor: i32, cursor: i32) -> ComposeSelection {
        self.update_selection_internal(anchor, cursor, true)
    }

    fn update_selection_internal(
        &mut self,
        anchor: i32,
        cursor: i32,
        break_typing_group: bool,
    ) -> ComposeSelection {
        let anchor = valid_offset(&self.text, anchor);
        let cursor = valid_offset(&self.text, cursor);
        if break_typing_group && self.selection != (to_i32(anchor), to_i32(cursor)) {
            self.typing_group_active = false;
        }
        self.selection = (to_i32(anchor), to_i32(cursor));

        if anchor == cursor && self.typing_override != Some(cursor) {
            self.typing_style = if self.text[..cursor]
                .char_indices()
                .next_back()
                .is_some_and(|(_, character)| character != '\n')
            {
                self.style_at(cursor.saturating_sub(1))
                    .cloned()
                    .unwrap_or_default()
            } else {
                self.style_at(cursor).cloned().unwrap_or_default()
            };
            self.typing_override = None;
        } else if anchor != cursor {
            self.typing_override = None;
        }

        ComposeSelection {
            start: to_i32(anchor),
            end: to_i32(cursor),
        }
    }

    pub fn insert_text(&mut self, value: &str) -> ComposeSelection {
        let anchor = valid_offset(&self.text, self.selection.0);
        let cursor = valid_offset(&self.text, self.selection.1);
        let (start, end) = ordered(anchor, cursor);
        if value.is_empty() && start == end {
            return self.update_selection(to_i32(start), to_i32(end));
        }

        let style = self.typing_style.clone();
        let inserted = ComposeStyleRun {
            range: 0..value.len(),
            style,
        };
        let styles = replaced_style_runs(&self.styles, start, end, std::slice::from_ref(&inserted));
        let mut text = String::with_capacity(self.text.len() - (end - start) + value.len());
        text.push_str(&self.text[..start]);
        text.push_str(value);
        text.push_str(&self.text[end..]);
        let before = self.take_snapshot();
        self.typing_override = None;
        self.styles = styles;
        self.text = text;
        self.auto_link_urls();
        self.bump_revision();
        let caret = start.saturating_add(value.len());
        let selection = self.update_selection(to_i32(caret), to_i32(caret));
        self.commit_edit(before);
        selection
    }

    pub fn format(
        &mut self,
        kind: &str,
        source: &str,
        anchor: i32,
        cursor: i32,
    ) -> ComposeSelection {
        self.typing_group_active = false;
        if source != self.text {
            self.synchronize(source, anchor, cursor);
        } else {
            self.update_selection(anchor, cursor);
        }

        let anchor = valid_offset(&self.text, anchor);
        let cursor = valid_offset(&self.text, cursor);
        let (start, end) = ordered(anchor, cursor);

        match kind {
            "bold" => self.toggle_mark(BOLD, start, end),
            "italic" => self.toggle_mark(ITALIC, start, end),
            "underline" => self.toggle_mark(UNDERLINE, start, end),
            "strike" => self.toggle_mark(STRIKE, start, end),
            "code" => self.toggle_mark(CODE, start, end),
            "bullet" => return self.toggle_block(BlockKind::Bullet, start, end),
            "number" => return self.toggle_block(BlockKind::Number, start, end),
            "quote" => return self.toggle_block(BlockKind::Quote, start, end),
            _ => {}
        }

        self.selection = (to_i32(start), to_i32(end));
        ComposeSelection {
            start: to_i32(start),
            end: to_i32(end),
        }
    }

    pub fn history(&mut self, direction: &str) -> Option<ComposeSelection> {
        self.typing_group_active = false;
        if direction == "redo" {
            let mut transaction = self.redo.pop_back()?;
            transaction.before_typing_style = self.typing_style.clone();
            transaction.before_typing_override = self.typing_override;
            transaction.before_selection = self.selection;
            self.apply_transaction(&transaction, true);
            self.undo.push_back(transaction);
        } else {
            let mut transaction = self.undo.pop_back()?;
            transaction.after_typing_style = self.typing_style.clone();
            transaction.after_typing_override = self.typing_override;
            transaction.after_selection = self.selection;
            self.apply_transaction(&transaction, false);
            self.redo.push_back(transaction);
        }
        self.trim_history();
        Some(ComposeSelection {
            start: self.selection.0,
            end: self.selection.1,
        })
    }

    pub fn active_marks(&self) -> ActiveMarks {
        let anchor = valid_offset(&self.text, self.selection.0);
        let cursor = valid_offset(&self.text, self.selection.1);
        let (start, end) = ordered(anchor, cursor);
        let marks = if start == end {
            self.typing_style.marks
        } else {
            let mut relevant = self
                .styles
                .iter()
                .filter(|run| run.range.start < end && run.range.end > start)
                .filter(|run| {
                    self.text[run.range.start.max(start)..run.range.end.min(end)]
                        .chars()
                        .any(|character| !character.is_whitespace())
                })
                .map(|run| run.style.marks);
            relevant
                .next()
                .map(|first| relevant.fold(first, |common, value| common & value))
                .unwrap_or_default()
        };
        ActiveMarks {
            bold: marks & BOLD != 0,
            italic: marks & ITALIC != 0,
            underline: marks & UNDERLINE != 0,
            strike: marks & STRIKE != 0,
            code: marks & CODE != 0,
        }
    }

    pub fn active_link(&self) -> Option<&str> {
        let anchor = valid_offset(&self.text, self.selection.0);
        let cursor = valid_offset(&self.text, self.selection.1);
        let (start, end) = ordered(anchor, cursor);
        if start == end {
            return None;
        }

        let mut links = self
            .styles
            .iter()
            .filter(|run| run.range.start < end && run.range.end > start)
            .filter(|run| {
                self.text[run.range.start.max(start)..run.range.end.min(end)]
                    .chars()
                    .any(|character| !character.is_whitespace())
            })
            .map(|run| run.style.link.as_deref());
        let first = links.next().flatten()?;
        links.all(|link| link == Some(first)).then_some(first)
    }

    pub fn set_link(
        &mut self,
        url: &str,
        source: &str,
        anchor: i32,
        cursor: i32,
    ) -> ComposeSelection {
        self.typing_group_active = false;
        if source != self.text {
            self.synchronize(source, anchor, cursor);
        } else {
            self.update_selection(anchor, cursor);
        }

        let anchor = valid_offset(&self.text, anchor);
        let cursor = valid_offset(&self.text, cursor);
        let (start_byte, end_byte) = ordered(anchor, cursor);
        if start_byte == end_byte {
            return ComposeSelection {
                start: to_i32(start_byte),
                end: to_i32(end_byte),
            };
        }

        let normalized = normalize_link(url).map(Arc::<str>::from);
        let before = self.begin_style_edit(start_byte..end_byte);
        self.typing_override = None;
        self.edit_styles(start_byte, end_byte, |style| {
            style.link = normalized.clone();
            style.link_is_auto = false;
        });
        self.selection = (to_i32(start_byte), to_i32(end_byte));
        self.bump_revision();
        self.commit_style_edit(start_byte, before);
        ComposeSelection {
            start: to_i32(start_byte),
            end: to_i32(end_byte),
        }
    }

    pub fn body_html(&self) -> Option<String> {
        if self.text.trim().is_empty() {
            return None;
        }

        let mut html = String::with_capacity(self.text.len());
        let mut open = None;
        let mut line_start = 0;

        for line in self.text.split('\n') {
            let block = html_line_block(line);
            let skip = block.map_or(0, |(_, bytes)| bytes);
            let content = line_start + skip..line_start + line.len();
            match block.map(|(block, _)| block) {
                Some(LineBlock::Bullet) => {
                    if open != Some(OpenHtmlBlock::Bullets) {
                        close_html_block(&mut html, &mut open);
                        html.push_str("<ul>");
                        open = Some(OpenHtmlBlock::Bullets);
                    }
                    html.push_str("<li>");
                    append_inline_html(&mut html, &self.text, &self.styles, content);
                    html.push_str("</li>");
                }
                Some(LineBlock::Number(number)) => {
                    // Recipients see the numbers shown in the composer: a
                    // number that does not continue the list starts a new one.
                    let number = u64::from(number);
                    if open != Some(OpenHtmlBlock::Numbers { next: number }) {
                        close_html_block(&mut html, &mut open);
                        if number == 1 {
                            html.push_str("<ol>");
                        } else {
                            use std::fmt::Write;
                            write!(html, "<ol start=\"{number}\">")
                                .expect("writing to a String cannot fail");
                        }
                    }
                    open = Some(OpenHtmlBlock::Numbers { next: number + 1 });
                    html.push_str("<li>");
                    append_inline_html(&mut html, &self.text, &self.styles, content);
                    html.push_str("</li>");
                }
                Some(LineBlock::Quote) => {
                    if open != Some(OpenHtmlBlock::Quote) {
                        close_html_block(&mut html, &mut open);
                        html.push_str("<blockquote>");
                        open = Some(OpenHtmlBlock::Quote);
                    }
                    append_line_html(&mut html, &self.text, &self.styles, content);
                }
                None => {
                    close_html_block(&mut html, &mut open);
                    append_line_html(&mut html, &self.text, &self.styles, content);
                }
            }
            line_start += line.len() + 1;
        }
        close_html_block(&mut html, &mut open);
        Some(html)
    }

    fn toggle_mark(&mut self, mark: u8, start_byte: usize, end_byte: usize) {
        if start_byte == end_byte {
            self.typing_style.marks ^= mark;
            self.typing_override = Some(start_byte);
            self.bump_revision();
            return;
        }

        let before = self.begin_style_edit(start_byte..end_byte);
        self.typing_override = None;
        let remove = self
            .styles
            .iter()
            .filter(|run| run.range.start < end_byte && run.range.end > start_byte)
            .filter(|run| {
                self.text[run.range.start.max(start_byte)..run.range.end.min(end_byte)]
                    .chars()
                    .any(|character| !character.is_whitespace())
            })
            .all(|run| run.style.marks & mark != 0);
        self.edit_styles(start_byte, end_byte, |style| {
            if remove {
                style.marks &= !mark;
            } else {
                style.marks |= mark;
            }
        });
        self.bump_revision();
        self.commit_style_edit(start_byte, before);
    }

    fn toggle_block(
        &mut self,
        kind: BlockKind,
        start_byte: usize,
        end_byte: usize,
    ) -> ComposeSelection {
        let start_line = self.text.as_bytes()[..start_byte]
            .iter()
            .filter(|&&byte| byte == b'\n')
            .count();
        let end_probe = if end_byte > start_byte {
            end_byte - 1
        } else {
            end_byte
        };
        let end_line = self.text.as_bytes()[..end_probe]
            .iter()
            .filter(|&&byte| byte == b'\n')
            .count();
        let all_target = self
            .text
            .split('\n')
            .skip(start_line)
            .take(end_line - start_line + 1)
            .all(|line| block_prefix_text(line).is_some_and(|(current, _)| current == kind));
        let before = self.take_snapshot();
        self.typing_override = None;
        let text_capacity = before
            .text
            .split('\n')
            .enumerate()
            .skip(start_line)
            .take(end_line - start_line + 1)
            .fold(before.text.len(), |size, (index, line)| {
                let removed = block_prefix_text(line).map_or(0, |(_, bytes)| bytes);
                let added = if all_target {
                    0
                } else {
                    block_marker_len(kind, index - start_line + 1)
                };
                size - removed + added
            });
        let mut text = String::with_capacity(text_capacity);
        let mut styles = Vec::with_capacity(before.styles.len());
        let mut source_start = 0;
        let mut start = 0;
        let mut end = 0;
        for (index, line) in before.text.split('\n').enumerate() {
            if index > 0 {
                let byte = text.len();
                text.push('\n');
                push_style_run(&mut styles, byte..text.len(), CharacterStyle::default());
            }
            if index == start_line {
                start = text.len();
            }
            let selected = (start_line..=end_line).contains(&index);
            let skip = if selected {
                block_prefix_text(line).map_or(0, |(_, bytes)| bytes)
            } else {
                0
            };
            if selected && !all_target {
                let byte = text.len();
                append_block_marker(&mut text, kind, index - start_line + 1);
                push_style_run(&mut styles, byte..text.len(), CharacterStyle::default());
            }
            let content_start = source_start + skip;
            let content_end = source_start + line.len();
            let destination_start = text.len();
            text.push_str(&before.text[content_start..content_end]);
            let first = before
                .styles
                .partition_point(|run| run.range.end <= content_start);
            for run in before.styles[first..]
                .iter()
                .take_while(|run| run.range.start < content_end)
            {
                let first_byte =
                    run.range.start.max(content_start) - content_start + destination_start;
                let last_byte = run.range.end.min(content_end) - content_start + destination_start;
                push_style_run(&mut styles, first_byte..last_byte, run.style.clone());
            }
            if index == end_line {
                end = text.len();
            }
            source_start += line.len() + 1;
        }
        self.text = text;
        self.styles = styles;
        self.bump_revision();
        self.selection = (to_i32(start), to_i32(end));
        self.commit_edit(before);
        ComposeSelection {
            start: to_i32(start),
            end: to_i32(end),
        }
    }

    fn auto_link_urls(&mut self) {
        for run in &mut self.styles {
            let style = &mut run.style;
            if style.link_is_auto {
                style.link = None;
                style.link_is_auto = false;
            }
        }
        self.merge_styles();

        let mut next = 0;
        while next < self.text.len() {
            let Some(relative) =
                self.text[next..].find(|character: char| !character.is_whitespace())
            else {
                break;
            };
            let start = next + relative;
            let end = self.text[start..]
                .find(char::is_whitespace)
                .map_or(self.text.len(), |relative| start + relative);
            next = end;
            let token = self.text[start..end]
                .trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '}']);
            if token.starts_with("https://") || token.starts_with("http://") {
                let end = start + token.len();
                let link = Arc::<str>::from(token);
                self.edit_styles(start, end, |style| {
                    if style.link.is_none() || style.link_is_auto {
                        style.link = Some(link.clone());
                        style.link_is_auto = true;
                    }
                });
            }
        }
    }

    fn style_at(&self, byte: usize) -> Option<&CharacterStyle> {
        style_at_runs(&self.styles, byte)
    }

    fn replace_style_runs(&mut self, start: usize, end: usize, inserted: &[ComposeStyleRun]) {
        self.styles = replaced_style_runs(&self.styles, start, end, inserted);
    }

    fn edit_styles(&mut self, start: usize, end: usize, mut edit: impl FnMut(&mut CharacterStyle)) {
        split_style_run_at(&mut self.styles, start);
        split_style_run_at(&mut self.styles, end);
        for run in &mut self.styles {
            if run.range.start >= start && run.range.end <= end {
                edit(&mut run.style);
            }
        }
        self.merge_styles();
    }

    fn merge_styles(&mut self) {
        self.styles.dedup_by(|next, previous| {
            if previous.range.end == next.range.start && previous.style == next.style {
                previous.range.end = next.range.end;
                true
            } else {
                false
            }
        });
    }

    fn take_snapshot(&mut self) -> Snapshot {
        Snapshot {
            text: std::mem::take(&mut self.text),
            styles: std::mem::take(&mut self.styles),
            typing_style: self.typing_style.clone(),
            typing_override: self.typing_override,
            selection: self.selection,
        }
    }

    fn begin_style_edit(&self, range: Range<usize>) -> StyleEditBefore {
        StyleEditBefore {
            fragment: EditFragment {
                text: Arc::from(""),
                span_len: range.end - range.start,
                styles: style_fragment(&self.styles, range),
            },
            typing_style: self.typing_style.clone(),
            typing_override: self.typing_override,
            selection: self.selection,
        }
    }

    fn commit_style_edit(&mut self, start: usize, before: StyleEditBefore) {
        let end = start + before.fragment.span_len;
        let after = EditFragment {
            text: before.fragment.text.clone(),
            span_len: before.fragment.span_len,
            styles: style_fragment(&self.styles, start..end),
        };
        let retained_bytes = transaction_bytes(&before.fragment, &after);
        self.push_transaction(EditTransaction {
            start,
            before: before.fragment,
            after,
            before_typing_style: before.typing_style,
            after_typing_style: self.typing_style.clone(),
            before_typing_override: before.typing_override,
            after_typing_override: self.typing_override,
            before_selection: before.selection,
            after_selection: self.selection,
            retained_bytes,
        });
    }

    fn commit_edit(&mut self, before: Snapshot) {
        self.commit_edit_with_key(before, "");
    }

    fn commit_edit_with_key(&mut self, before: Snapshot, key_text: &str) {
        let (start, before_end, after_end) =
            changed_ranges(&before.text, &before.styles, &self.text, &self.styles);
        let before_fragment = edit_fragment(&before.text, &before.styles, start..before_end);
        let after_fragment = edit_fragment(&self.text, &self.styles, start..after_end);
        let retained_bytes = transaction_bytes(&before_fragment, &after_fragment);
        self.push_key_transaction(
            EditTransaction {
                start,
                before: before_fragment,
                after: after_fragment,
                before_typing_style: before.typing_style,
                after_typing_style: self.typing_style.clone(),
                before_typing_override: before.typing_override,
                after_typing_override: self.typing_override,
                before_selection: before.selection,
                after_selection: self.selection,
                retained_bytes,
            },
            key_text,
        );
    }

    fn push_transaction(&mut self, transaction: EditTransaction) {
        self.typing_group_active = false;
        self.redo.clear();
        self.undo.push_back(transaction);
        self.trim_history();
    }

    fn push_key_transaction(&mut self, transaction: EditTransaction, key_text: &str) {
        let eligible = is_ordinary_typing(&transaction, key_text);
        self.redo.clear();
        if eligible
            && self.typing_group_active
            && let Some(previous) = self.undo.back_mut()
            && can_group_typing(previous, &transaction)
        {
            let mut text =
                String::with_capacity(previous.after.text.len() + transaction.after.text.len());
            text.push_str(&previous.after.text);
            text.push_str(&transaction.after.text);
            previous.after.text = Arc::from(text);
            previous.after.span_len += transaction.after.span_len;
            previous.after.styles[0].range.end = previous.after.span_len;
            previous.after_typing_style = transaction.after_typing_style;
            previous.after_typing_override = transaction.after_typing_override;
            previous.after_selection = transaction.after_selection;
            previous.retained_bytes = transaction_bytes(&previous.before, &previous.after);
            self.trim_history();
            return;
        }
        self.typing_group_active = eligible;
        self.undo.push_back(transaction);
        self.trim_history();
    }

    fn apply_transaction(&mut self, transaction: &EditTransaction, redo: bool) {
        let (removed, inserted, typing_style, typing_override, selection) = if redo {
            (
                &transaction.before,
                &transaction.after,
                &transaction.after_typing_style,
                transaction.after_typing_override,
                transaction.after_selection,
            )
        } else {
            (
                &transaction.after,
                &transaction.before,
                &transaction.before_typing_style,
                transaction.before_typing_override,
                transaction.before_selection,
            )
        };
        let range = transaction.start..transaction.start + removed.span_len;
        debug_assert_eq!(
            inserted.styles.last().map_or(0, |run| run.range.end),
            inserted.span_len
        );
        self.replace_style_runs(range.start, range.end, &inserted.styles);
        if !Arc::ptr_eq(&transaction.before.text, &transaction.after.text) {
            debug_assert_eq!(removed.span_len, removed.text.len());
            debug_assert_eq!(inserted.span_len, inserted.text.len());
            self.text.replace_range(range, inserted.text.as_ref());
        } else {
            debug_assert_eq!(transaction.before.span_len, transaction.after.span_len);
        }
        self.typing_style = typing_style.clone();
        self.typing_override = typing_override;
        self.selection = selection;
        self.bump_revision();
    }

    fn trim_history(&mut self) {
        while self.undo.len() + self.redo.len() > MAX_HISTORY
            || self.history_retained_bytes() > MAX_HISTORY_BYTES
                && self.undo.len() + self.redo.len() > 1
        {
            if !self.undo.is_empty() {
                self.undo.pop_front();
            } else {
                self.redo.pop_front();
            }
        }
    }

    fn history_retained_bytes(&self) -> usize {
        self.undo
            .iter()
            .chain(&self.redo)
            .map(|snapshot| snapshot.retained_bytes)
            .sum()
    }

    fn bump_revision(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }
}

fn style_at_runs(runs: &[ComposeStyleRun], byte: usize) -> Option<&CharacterStyle> {
    let index = runs.partition_point(|run| run.range.end <= byte);
    runs.get(index)
        .filter(|run| run.range.start <= byte)
        .map(|run| &run.style)
}

fn replaced_style_runs(
    old: &[ComposeStyleRun],
    start: usize,
    end: usize,
    inserted: &[ComposeStyleRun],
) -> Vec<ComposeStyleRun> {
    let inserted_len = inserted.last().map_or(0, |run| run.range.end);
    let prefix_end = old.partition_point(|run| run.range.start < start);
    let suffix_start = old.partition_point(|run| run.range.end <= end);
    let mut styles = Vec::with_capacity(prefix_end + inserted.len() + old.len() - suffix_start);
    for run in &old[..prefix_end] {
        if run.range.start < start {
            push_style_run(
                &mut styles,
                run.range.start..run.range.end.min(start),
                run.style.clone(),
            );
        }
    }
    for run in inserted {
        push_style_run(
            &mut styles,
            start + run.range.start..start + run.range.end,
            run.style.clone(),
        );
    }
    for run in &old[suffix_start..] {
        if run.range.end > end {
            push_style_run(
                &mut styles,
                start + inserted_len + run.range.start.max(end) - end
                    ..start + inserted_len + run.range.end - end,
                run.style.clone(),
            );
        }
    }
    styles
}

fn text_change_ranges(before: &str, after: &str) -> (usize, usize, usize) {
    let before_bytes = before.as_bytes();
    let after_bytes = after.as_bytes();
    let mut start = before_bytes
        .iter()
        .zip(after_bytes)
        .take_while(|(old, new)| old == new)
        .count();
    while !before.is_char_boundary(start) || !after.is_char_boundary(start) {
        start -= 1;
    }
    let mut suffix = 0;
    let limit = (before.len() - start).min(after.len() - start);
    while suffix < limit
        && before_bytes[before.len() - suffix - 1] == after_bytes[after.len() - suffix - 1]
    {
        suffix += 1;
    }
    while !before.is_char_boundary(before.len() - suffix)
        || !after.is_char_boundary(after.len() - suffix)
    {
        suffix -= 1;
    }
    (start, before.len() - suffix, after.len() - suffix)
}

fn changed_ranges(
    before_text: &str,
    before_styles: &[ComposeStyleRun],
    after_text: &str,
    after_styles: &[ComposeStyleRun],
) -> (usize, usize, usize) {
    let before_bytes = before_text.as_bytes();
    let after_bytes = after_text.as_bytes();
    let mut text_prefix = before_bytes
        .iter()
        .zip(after_bytes)
        .take_while(|(before, after)| before == after)
        .count();
    while !before_text.is_char_boundary(text_prefix) || !after_text.is_char_boundary(text_prefix) {
        text_prefix -= 1;
    }

    let mut start = 0;
    while start < text_prefix {
        let before_run =
            &before_styles[before_styles.partition_point(|run| run.range.end <= start)];
        let after_run = &after_styles[after_styles.partition_point(|run| run.range.end <= start)];
        if before_run.style != after_run.style {
            break;
        }
        start = text_prefix
            .min(before_run.range.end)
            .min(after_run.range.end);
    }

    let mut text_suffix = 0;
    let suffix_limit = (before_text.len() - start).min(after_text.len() - start);
    while text_suffix < suffix_limit
        && before_bytes[before_bytes.len() - text_suffix - 1]
            == after_bytes[after_bytes.len() - text_suffix - 1]
    {
        text_suffix += 1;
    }
    while !before_text.is_char_boundary(before_text.len() - text_suffix)
        || !after_text.is_char_boundary(after_text.len() - text_suffix)
    {
        text_suffix -= 1;
    }

    let mut style_suffix = 0;
    while style_suffix < text_suffix {
        let before_pos = before_text.len() - style_suffix - 1;
        let after_pos = after_text.len() - style_suffix - 1;
        let before_run =
            &before_styles[before_styles.partition_point(|run| run.range.end <= before_pos)];
        let after_run =
            &after_styles[after_styles.partition_point(|run| run.range.end <= after_pos)];
        if before_run.style != after_run.style {
            break;
        }
        let step = (text_suffix - style_suffix)
            .min(before_text.len() - style_suffix - before_run.range.start)
            .min(after_text.len() - style_suffix - after_run.range.start);
        style_suffix += step;
    }

    (
        start,
        before_text.len() - style_suffix,
        after_text.len() - style_suffix,
    )
}

fn edit_fragment(text: &str, styles: &[ComposeStyleRun], range: Range<usize>) -> EditFragment {
    EditFragment {
        text: Arc::from(&text[range.clone()]),
        span_len: range.end - range.start,
        styles: style_fragment(styles, range),
    }
}

fn style_fragment(styles: &[ComposeStyleRun], range: Range<usize>) -> Vec<ComposeStyleRun> {
    let first = styles.partition_point(|run| run.range.end <= range.start);
    let end = styles
        .partition_point(|run| run.range.start < range.end)
        .max(first);
    let mut fragment_styles = Vec::with_capacity(end - first);
    for run in &styles[first..end] {
        if run.range.start < range.end && run.range.end > range.start {
            push_style_run(
                &mut fragment_styles,
                run.range.start.max(range.start) - range.start
                    ..run.range.end.min(range.end) - range.start,
                run.style.clone(),
            );
        }
    }
    fragment_styles
}

fn fragment_text_bytes(text: &Arc<str>) -> usize {
    text.len() + 2 * std::mem::size_of::<usize>()
}

fn count_style_links(style: &CharacterStyle, seen: &mut HashSet<usize>, bytes: &mut usize) {
    if let Some(link) = &style.link
        && seen.insert(Arc::as_ptr(link) as *const u8 as usize)
    {
        *bytes += fragment_text_bytes(link);
    }
}

fn transaction_bytes(before: &EditFragment, after: &EditFragment) -> usize {
    let mut bytes = fragment_text_bytes(&before.text)
        + (before.styles.capacity() + after.styles.capacity())
            * std::mem::size_of::<ComposeStyleRun>();
    if !Arc::ptr_eq(&before.text, &after.text) {
        bytes += fragment_text_bytes(&after.text);
    }
    let mut seen_links = HashSet::new();
    for run in before.styles.iter().chain(&after.styles) {
        count_style_links(&run.style, &mut seen_links, &mut bytes);
    }
    bytes
}

fn is_ordinary_typing(transaction: &EditTransaction, key_text: &str) -> bool {
    let mut characters = key_text.chars();
    let Some(character) = characters.next() else {
        return false;
    };
    character.is_alphanumeric()
        && characters.next().is_none()
        && transaction.before_selection == (to_i32(transaction.start), to_i32(transaction.start))
        && transaction.before.span_len == 0
        && transaction.before.text.is_empty()
        && transaction.after.text.as_ref() == key_text
        && transaction.after.span_len == key_text.len()
        && transaction.after.styles.len() == 1
        && transaction.after.styles[0].range == (0..key_text.len())
        && transaction.after.styles[0].style == transaction.before_typing_style
        && transaction.after_typing_style == transaction.before_typing_style
        && transaction.after_typing_override == transaction.before_typing_override
        && transaction.after_selection
            == (
                to_i32(transaction.start + key_text.len()),
                to_i32(transaction.start + key_text.len()),
            )
}

fn can_group_typing(previous: &EditTransaction, next: &EditTransaction) -> bool {
    previous.before.span_len == 0
        && previous.after.styles.len() == 1
        && previous.after.span_len + next.after.span_len <= MAX_TYPING_GROUP_BYTES
        && previous.start + previous.after.span_len == next.start
        && previous.after.styles[0].style == next.after.styles[0].style
        && previous.after_selection == next.before_selection
        && previous.after_typing_style == next.before_typing_style
        && previous.after_typing_override == next.before_typing_override
}

fn push_style_run(runs: &mut Vec<ComposeStyleRun>, range: Range<usize>, style: CharacterStyle) {
    if range.is_empty() {
        return;
    }
    if let Some(last) = runs.last_mut() {
        debug_assert_eq!(last.range.end, range.start);
        if last.range.end == range.start && last.style == style {
            last.range.end = range.end;
            return;
        }
    }
    runs.push(ComposeStyleRun { range, style });
}

fn split_style_run_at(runs: &mut Vec<ComposeStyleRun>, byte: usize) {
    if let Some(index) = runs
        .iter()
        .position(|run| run.range.start < byte && byte < run.range.end)
    {
        let original_end = runs[index].range.end;
        let style = runs[index].style.clone();
        runs[index].range.end = byte;
        runs.insert(
            index + 1,
            ComposeStyleRun {
                range: byte..original_end,
                style,
            },
        );
    }
}

fn normalize_link(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if value.starts_with("https://") || value.starts_with("http://") || value.starts_with("mailto:")
    {
        Some(value.to_owned())
    } else {
        Some(format!("https://{value}"))
    }
}

/// Extends a block marker when the only edit is pressing Enter at the end of
/// a list/quote line. Enter on an empty item exits the block, matching common
/// mail editors.
fn continue_list_edit(old_text: &str, new_text: &str) -> Option<(String, i32)> {
    if new_text.len() != old_text.len() + 1 {
        return None;
    }
    let insertion = old_text
        .as_bytes()
        .iter()
        .zip(new_text.as_bytes())
        .take_while(|(old, new)| old == new)
        .count();
    if !old_text.is_char_boundary(insertion)
        || new_text.as_bytes().get(insertion) != Some(&b'\n')
        || old_text[insertion..] != new_text[insertion + 1..]
    {
        return None;
    }

    let line_start = old_text[..insertion].rfind('\n').map_or(0, |byte| byte + 1);
    let line = &old_text[line_start..insertion];
    let (kind, prefix_len) = block_prefix_text(line)?;
    let content_is_empty = line[prefix_len..].chars().all(char::is_whitespace);
    if content_is_empty {
        let mut text = String::with_capacity(new_text.len() - (insertion + 1 - line_start));
        text.push_str(&new_text[..line_start]);
        text.push_str(&new_text[insertion + 1..]);
        return Some((text, to_i32(line_start)));
    }

    let number = if kind == BlockKind::Number {
        line[..prefix_len.saturating_sub(2)]
            .parse::<usize>()
            .unwrap_or(1)
            .saturating_add(1)
    } else {
        1
    };
    let marker_len = block_marker_len(kind, number);
    let mut text = String::with_capacity(new_text.len() + marker_len);
    text.push_str(&new_text[..insertion + 1]);
    append_block_marker(&mut text, kind, number);
    text.push_str(&new_text[insertion + 1..]);
    Some((text, to_i32(insertion + 1 + marker_len)))
}

fn block_marker_len(kind: BlockKind, number: usize) -> usize {
    match kind {
        BlockKind::Bullet => "• ".len(),
        BlockKind::Quote => "│ ".len(),
        BlockKind::Number => number.checked_ilog10().unwrap_or(0) as usize + 3,
    }
}

fn append_block_marker(text: &mut String, kind: BlockKind, number: usize) {
    match kind {
        BlockKind::Bullet => text.push_str("• "),
        BlockKind::Quote => text.push_str("│ "),
        BlockKind::Number => {
            use std::fmt::Write;
            write!(text, "{number}. ").expect("writing to a String cannot fail");
        }
    }
}

// Prefix lengths are UTF-8 byte offsets into the borrowed line.
fn block_prefix_text(text: &str) -> Option<(BlockKind, usize)> {
    if text.starts_with("• ") {
        return Some((BlockKind::Bullet, "• ".len()));
    }
    if text.starts_with("│ ") {
        return Some((BlockKind::Quote, "│ ".len()));
    }
    let digits = text.bytes().take_while(u8::is_ascii_digit).count();
    if digits > 0 && text.as_bytes().get(digits..digits + 2) == Some(b". ") {
        return Some((BlockKind::Number, digits + 2));
    }
    None
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LineBlock {
    Bullet,
    Number(u32),
    Quote,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OpenHtmlBlock {
    Bullets,
    Numbers { next: u64 },
    Quote,
}

/// A numbered line whose number does not fit an HTML list stays a paragraph.
fn html_line_block(line: &str) -> Option<(LineBlock, usize)> {
    let (kind, bytes) = block_prefix_text(line)?;
    let block = match kind {
        BlockKind::Bullet => LineBlock::Bullet,
        BlockKind::Quote => LineBlock::Quote,
        BlockKind::Number => LineBlock::Number(line[..bytes - 2].parse().ok()?),
    };
    Some((block, bytes))
}

fn close_html_block(html: &mut String, open: &mut Option<OpenHtmlBlock>) {
    match open.take() {
        Some(OpenHtmlBlock::Bullets) => html.push_str("</ul>"),
        Some(OpenHtmlBlock::Numbers { .. }) => html.push_str("</ol>"),
        Some(OpenHtmlBlock::Quote) => html.push_str("</blockquote>"),
        None => {}
    }
}

fn append_line_html(
    output: &mut String,
    text: &str,
    styles: &[ComposeStyleRun],
    range: Range<usize>,
) {
    output.push_str("<div>");
    if range.is_empty() {
        output.push_str("<br>");
    } else {
        append_inline_html(output, text, styles, range);
    }
    output.push_str("</div>");
}

fn append_inline_html(
    output: &mut String,
    text: &str,
    styles: &[ComposeStyleRun],
    range: Range<usize>,
) {
    let first = styles.partition_point(|run| run.range.end <= range.start);
    // HTML collapses repeated spaces and drops them at either end of a line,
    // so those spaces are written as no-break spaces.
    let mut after_space = true;
    for run in styles[first..]
        .iter()
        .take_while(|run| run.range.start < range.end)
    {
        let start = run.range.start.max(range.start);
        let end = run.range.end.min(range.end);
        append_html_run(
            output,
            text,
            start..end,
            range.end,
            &run.style,
            &mut after_space,
        );
    }
}

fn append_html_run(
    output: &mut String,
    text: &str,
    range: Range<usize>,
    line_end: usize,
    style: &CharacterStyle,
    after_space: &mut bool,
) {
    if range.is_empty() {
        return;
    }
    if let Some(url) = style.link.as_deref() {
        output.push_str("<a href=\"");
        for character in url.chars() {
            append_html_char(output, character);
        }
        output.push_str("\">");
    }
    let tags = [
        (BOLD, "strong"),
        (ITALIC, "em"),
        (STRIKE, "s"),
        (UNDERLINE, "u"),
        (CODE, "code"),
    ];
    for (mark, tag) in tags {
        if style.marks & mark != 0 {
            output.push('<');
            output.push_str(tag);
            output.push('>');
        }
    }
    for (offset, character) in text[range.clone()].char_indices() {
        match character {
            ' ' if *after_space || range.start + offset + 1 == line_end => {
                output.push_str("&nbsp;");
            }
            '\t' => output.push_str("&nbsp;&nbsp;&nbsp;&nbsp;"),
            _ => append_html_char(output, character),
        }
        *after_space = matches!(character, ' ' | '\t');
    }
    for (mark, tag) in tags.into_iter().rev() {
        if style.marks & mark != 0 {
            output.push_str("</");
            output.push_str(tag);
            output.push('>');
        }
    }
    if style.link.is_some() {
        output.push_str("</a>");
    }
}

fn append_html_char(output: &mut String, character: char) {
    match character {
        '&' => output.push_str("&amp;"),
        '<' => output.push_str("&lt;"),
        '>' => output.push_str("&gt;"),
        '"' => output.push_str("&quot;"),
        '\'' => output.push_str("&#39;"),
        _ => output.push(character),
    }
}

fn ordered(a: usize, b: usize) -> (usize, usize) {
    if a <= b { (a, b) } else { (b, a) }
}

fn valid_offset(text: &str, offset: i32) -> usize {
    let mut offset = usize::try_from(offset).unwrap_or_default().min(text.len());
    while offset > 0 && !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

fn to_i32(value: usize) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

#[cfg(test)]
mod tests {
    #[test]
    fn loaded_text_is_where_undo_stops() {
        let mut document = RichComposeDocument::default();
        document.load("Quoted reply", 0, 0);
        assert_eq!(document.history("undo"), None);
        assert_eq!(document.text(), "Quoted reply");
        document.synchronize_key_edit("Hi\nQuoted reply", 3, 3, "\n");
        document.history("undo").unwrap();
        assert_eq!(document.text(), "Quoted reply");
        assert_eq!(document.history("undo"), None);
    }

    use super::*;

    fn assert_style_coverage(document: &RichComposeDocument) {
        let mut end = 0;
        for run in &document.styles {
            assert_eq!(run.range.start, end);
            assert!(run.range.end > run.range.start);
            assert!(document.text.is_char_boundary(run.range.start));
            assert!(document.text.is_char_boundary(run.range.end));
            end = run.range.end;
        }
        assert_eq!(end, document.text.len());
        assert!(
            document
                .styles
                .windows(2)
                .all(|pair| pair[0].style != pair[1].style)
        );
    }

    fn assert_fixture_state(
        document: &RichComposeDocument,
        text: &str,
        html: &str,
        selection: (i32, i32),
        runs: &[(Range<usize>, u8, Option<&str>)],
    ) {
        assert_eq!(document.text(), text);
        assert_eq!(document.body_html().as_deref(), Some(html));
        assert_eq!(document.selection, selection);
        assert_eq!(document.styles.len(), runs.len());
        for (actual, expected) in document.styles.iter().zip(runs) {
            assert_eq!(actual.range, expected.0);
            assert_eq!(actual.style.marks, expected.1);
            assert_eq!(actual.style.link.as_deref(), expected.2);
        }
        assert_style_coverage(document);
    }

    #[test]
    fn html_export_preserves_all_marks_and_escaped_unicode() {
        let text = "<&\"'é👩‍🚀>";
        let mut document = RichComposeDocument::default();
        document.synchronize(text, 0, text.len() as i32);
        for mark in ["bold", "italic", "underline", "strike", "code"] {
            document.format(mark, text, 0, text.len() as i32);
        }
        document.set_link("https://example.org/?a=\"&b='", text, 0, text.len() as i32);
        assert_eq!(
            document.body_html().unwrap(),
            "<div><a href=\"https://example.org/?a=&quot;&amp;b=&#39;\"><strong><em><s><u><code>&lt;&amp;&quot;&#39;é👩‍🚀&gt;</code></u></s></em></strong></a></div>"
        );
    }

    #[test]
    fn normalizing_styles_reuses_vector_storage() {
        let mut document = RichComposeDocument {
            text: "é界xy".into(),
            styles: Vec::with_capacity(20),
            ..RichComposeDocument::default()
        };
        for range in [0..2, 2..5, 5..6, 6..7] {
            document.styles.push(ComposeStyleRun {
                range,
                style: CharacterStyle::default(),
            });
        }
        let pointer = document.styles.as_ptr();
        let capacity = document.styles.capacity();
        document.merge_styles();
        assert_eq!(document.styles.as_ptr(), pointer);
        assert_eq!(document.styles.capacity(), capacity);
        assert_eq!(document.styles.len(), 1);
        assert_eq!(document.styles[0].range, 0..7);
        assert_style_coverage(&document);
    }

    #[test]
    fn selected_style_fragment_allocates_only_intersecting_runs() {
        let styles = (0..1000)
            .map(|byte| ComposeStyleRun {
                range: byte..byte + 1,
                style: CharacterStyle {
                    marks: if byte % 2 == 0 { BOLD } else { ITALIC },
                    ..Default::default()
                },
            })
            .collect::<Vec<_>>();
        let fragment = style_fragment(&styles, 900..903);
        assert_eq!(fragment.len(), 3);
        assert_eq!(fragment.capacity(), 3);
        assert_eq!(fragment[0].range, 0..1);
        assert_eq!(fragment[2].range, 2..3);
        assert!(style_fragment(&styles, 900..900).is_empty());
    }

    #[test]
    fn replacing_fragmented_styles_does_not_retain_removed_capacity() {
        let old = (0..1000)
            .map(|byte| ComposeStyleRun {
                range: byte..byte + 1,
                style: CharacterStyle {
                    marks: if byte % 2 == 0 { BOLD } else { ITALIC },
                    ..Default::default()
                },
            })
            .collect::<Vec<_>>();
        let inserted = [ComposeStyleRun {
            range: 0..1,
            style: CharacterStyle::default(),
        }];
        let result = replaced_style_runs(&old, 0, 1000, &inserted);
        assert_eq!(result.len(), 1);
        assert_eq!(result.capacity(), 1);
        assert_eq!(result[0].range, 0..1);
        let empty = replaced_style_runs(&old, 0, 1000, &[]);
        assert_eq!(empty.capacity(), 0);
    }

    #[test]
    fn inline_formatting_never_changes_editable_text() {
        let mut document = RichComposeDocument::default();
        document.synchronize("Hello world", 0, 5);
        let selection = document.format("bold", "Hello world", 0, 5);
        assert_eq!(document.text(), "Hello world");
        assert_eq!(selection, ComposeSelection { start: 0, end: 5 });
        assert_eq!(
            document.body_html().as_deref(),
            Some("<div><strong>Hello</strong> world</div>")
        );
        assert!(document.active_marks().bold);
    }

    #[test]
    fn typing_inside_a_mark_inherits_that_mark() {
        let mut document = RichComposeDocument::default();
        document.synchronize("Hello", 0, 5);
        document.format("italic", "Hello", 0, 5);
        document.update_selection(5, 5);
        document.synchronize("Hello!", 6, 6);
        assert_eq!(
            document.body_html().as_deref(),
            Some("<div><em>Hello!</em></div>")
        );
    }

    #[test]
    fn inserted_template_replaces_the_selection_and_can_be_undone() {
        let mut document = RichComposeDocument::default();
        document.synchronize("Hello placeholder", 6, 17);
        let selection = document.insert_text("there");
        assert_eq!(document.text(), "Hello there");
        assert_eq!(selection, ComposeSelection { start: 11, end: 11 });

        document.history("undo");
        assert_eq!(document.text(), "Hello placeholder");
    }

    #[test]
    fn collapsed_toolbar_mark_survives_the_focus_selection_callback() {
        let mut document = RichComposeDocument::default();
        document.synchronize("Hello ", 6, 6);
        document.format("bold", "Hello ", 6, 6);
        document.update_selection(6, 6);
        document.synchronize("Hello W", 7, 7);

        assert_eq!(
            document.body_html().as_deref(),
            Some("<div>Hello <strong>W</strong></div>")
        );
    }

    #[test]
    fn long_unicode_block_edits_keep_compact_styles_and_undo() {
        let text = format!("{}\nsecond é👩‍🚀\n", "界".repeat(350_000));
        let start = text.find("second").unwrap() as i32;
        let end = (text.len() - 1) as i32;
        let mut document = RichComposeDocument::default();
        document.synchronize(&text, start, end);
        document.format("bold", &text, start, end);
        let html_before = document.body_html();
        document.format("bullet", &text, start, end);
        assert!(document.text().contains("\n• second é👩‍🚀\n"));
        assert!(document.style_runs().len() <= 5);
        assert!(
            document
                .body_html()
                .unwrap()
                .contains("<li><strong>second é👩‍🚀</strong></li>")
        );
        document.history("undo").unwrap();
        assert_eq!(document.text(), text);
        assert_eq!(document.body_html(), html_before);
        document.history("redo").unwrap();
        assert!(document.text().contains("\n• second é👩‍🚀\n"));
        assert_style_coverage(&document);
    }

    #[test]
    fn block_controls_create_semantic_email_html() {
        let mut document = RichComposeDocument::default();
        document.synchronize("One\nTwo", 0, 7);
        document.format("bullet", "One\nTwo", 0, 7);
        assert_eq!(document.text(), "• One\n• Two");
        assert_eq!(
            document.body_html().as_deref(),
            Some("<ul><li>One</li><li>Two</li></ul>")
        );
    }

    #[test]
    fn enter_continues_bulleted_and_numbered_lists() {
        let mut document = RichComposeDocument::default();
        document.synchronize("• First", 9, 9);
        let bullet = document.synchronize("• First\n", 10, 10);
        assert_eq!(document.text(), "• First\n• ");
        assert_eq!(bullet, ComposeSelection { start: 14, end: 14 });

        document.reset();
        document.synchronize("3. Third", 8, 8);
        let numbered = document.synchronize("3. Third\n", 9, 9);
        assert_eq!(document.text(), "3. Third\n4. ");
        assert_eq!(numbered, ComposeSelection { start: 12, end: 12 });
    }

    #[test]
    fn long_list_continuation_preserves_unicode_and_number_overflow() {
        let old = format!("• {}", "界".repeat(350_000));
        let entered = format!("{old}\n");
        let (continued, cursor) = continue_list_edit(&old, &entered).unwrap();
        assert_eq!(continued, format!("{old}\n• "));
        assert_eq!(cursor as usize, continued.len());
        let old = format!("{}. é👩‍🚀", usize::MAX);
        let (continued, _) = continue_list_edit(&old, &format!("{old}\n")).unwrap();
        assert_eq!(continued, format!("{old}\n{}. ", usize::MAX));
        let (empty, cursor) = continue_list_edit("│ \u{2003}", "│ \u{2003}\n").unwrap();
        assert!(empty.is_empty());
        assert_eq!(cursor, 0);
    }

    #[test]
    fn enter_continues_a_list_after_multibyte_text() {
        let mut document = RichComposeDocument::default();
        let original = "• café";
        document.synchronize(original, original.len() as i32, original.len() as i32);
        let entered = format!("{original}\n");
        let selection = document.synchronize(&entered, entered.len() as i32, entered.len() as i32);
        assert_eq!(document.text(), "• café\n• ");
        assert_eq!(selection.start as usize, document.text().len());
        document.history("undo").unwrap();
        assert_eq!(document.text(), original);
        document.history("redo").unwrap();
        assert_eq!(document.text(), "• café\n• ");
        assert_style_coverage(&document);
    }

    #[test]
    fn enter_on_empty_list_item_exits_the_list() {
        let mut document = RichComposeDocument::default();
        document.synchronize("• First\n• ", 14, 14);
        let selection = document.synchronize("• First\n• \n", 15, 15);
        assert_eq!(document.text(), "• First\n");
        assert_eq!(selection, ComposeSelection { start: 10, end: 10 });
    }

    #[test]
    fn pasted_http_urls_are_linked_automatically() {
        let mut document = RichComposeDocument::default();
        let text = "Read https://slint.dev/blog/slint-1.7-released for details.";
        document.synchronize(text, text.len() as i32, text.len() as i32);
        let link_start = "Read ".len();
        let link_end = text.find(" for details.").unwrap();
        let link = document
            .style_at(link_start)
            .unwrap()
            .link
            .as_ref()
            .unwrap();
        assert!(
            document
                .styles
                .iter()
                .filter(|run| run.range.start < link_end && run.range.end > link_start)
                .all(|run| Arc::ptr_eq(link, run.style.link.as_ref().unwrap()))
        );
        assert_eq!(
            document.body_html().as_deref(),
            Some(
                "<div>Read <a href=\"https://slint.dev/blog/slint-1.7-released\">https://slint.dev/blog/slint-1.7-released</a> for details.</div>"
            )
        );
    }

    #[test]
    fn selected_text_uses_the_supplied_link_destination() {
        let mut document = RichComposeDocument::default();
        document.synchronize("Slint release", 0, 13);
        document.set_link("slint.dev/blog/slint-1.7-released", "Slint release", 0, 13);
        assert_eq!(
            document.active_link(),
            Some("https://slint.dev/blog/slint-1.7-released")
        );
        assert_eq!(
            document.body_html().as_deref(),
            Some(
                "<div><a href=\"https://slint.dev/blog/slint-1.7-released\">Slint release</a></div>"
            )
        );
    }

    #[test]
    fn undo_restores_text_and_formatting_together() {
        let mut document = RichComposeDocument::default();
        document.synchronize("Hello", 0, 5);
        document.format("bold", "Hello", 0, 5);
        document.history("undo").unwrap();
        assert_eq!(document.body_html().as_deref(), Some("<div>Hello</div>"));
    }

    #[test]
    fn long_draft_history_stays_within_byte_budget_and_keeps_recent_undo() {
        let mut document = RichComposeDocument::default();
        let initial = "x".repeat(1024 * 1024);
        document.synchronize(&initial, initial.len() as i32, initial.len() as i32);
        for _ in 0..40 {
            document.insert_text("y");
        }

        assert!(document.history_retained_bytes() <= MAX_HISTORY_BYTES);
        assert_eq!(document.undo.len(), 41);
        assert!(document.history_retained_bytes() < 2 * 1024 * 1024);
        assert_eq!(document.styles.len(), 1);
        let latest = document.text().to_owned();
        document.history("undo").unwrap();
        assert_eq!(document.text().len(), latest.len() - 1);
        document.history("redo").unwrap();
        assert_eq!(document.text(), latest);

        document.history("undo").unwrap();
        document.insert_text("z");
        assert!(document.history("redo").is_none());
    }

    #[test]
    fn synchronized_long_unicode_edit_retains_only_changed_history_text() {
        let mut document = RichComposeDocument::default();
        let original = "é".repeat(512 * 1024);
        document.synchronize(&original, original.len() as i32, original.len() as i32);
        let edited = format!("{original}界");
        document.synchronize(&edited, edited.len() as i32, edited.len() as i32);
        let latest = document.undo.back().unwrap();
        assert_eq!(latest.before.text.as_ref(), "");
        assert_eq!(latest.after.text.as_ref(), "界");
        assert!(document.history_retained_bytes() < 2 * 1024 * 1024);
        document.history("undo").unwrap();
        assert_eq!(document.text(), original);
        document.history("redo").unwrap();
        assert_eq!(document.text(), edited);
        assert_style_coverage(&document);
    }

    #[test]
    fn small_style_edits_in_long_draft_retain_only_the_selection() {
        let mut document = RichComposeDocument::default();
        let draft = "x".repeat(1024 * 1024);
        document.synchronize(&draft, 0, 0);
        document.undo.clear();
        let start = (draft.len() / 2) as i32;
        let end = start + 1;
        let text_pointer = document.text.as_ptr();

        document.format("bold", &draft, start, end);
        let latest = document.undo.back().unwrap();
        assert_eq!(latest.before.text.as_ref(), "");
        assert_eq!(latest.before.span_len, 1);
        assert!(Arc::ptr_eq(&latest.before.text, &latest.after.text));
        assert!(document.history_retained_bytes() < 1024);
        assert!(document.active_marks().bold);
        document.history("undo").unwrap();
        assert!(!document.active_marks().bold);
        assert_eq!(document.text.as_ptr(), text_pointer);
        document.history("redo").unwrap();
        assert!(document.active_marks().bold);

        document.set_link("example.org", &draft, start, end);
        let latest = document.undo.back().unwrap();
        assert!(Arc::ptr_eq(&latest.before.text, &latest.after.text));
        assert!(latest.retained_bytes < 1024);
        document.history("undo").unwrap();
        assert_eq!(document.active_link(), None);
        document.history("redo").unwrap();
        assert_eq!(document.active_link(), Some("https://example.org"));
        assert_eq!(document.text.as_ptr(), text_pointer);
        assert_style_coverage(&document);
    }

    #[test]
    fn formatting_entire_long_draft_keeps_text_out_of_history() {
        let mut document = RichComposeDocument::default();
        let draft = "x".repeat(1024 * 1024);
        document.synchronize(&draft, 0, 0);
        document.undo.clear();
        document.format("bold", &draft, 0, draft.len() as i32);
        let transaction = document.undo.back().unwrap();
        assert_eq!(transaction.before.span_len, draft.len());
        assert_eq!(transaction.before.text.as_ref(), "");
        assert_eq!(transaction.after.text.as_ref(), "");
        assert!(transaction.retained_bytes < 1024);
        document.history("undo").unwrap();
        assert!(!document.active_marks().bold);
        document.history("redo").unwrap();
        assert!(document.active_marks().bold);
        assert_style_coverage(&document);
    }

    #[test]
    fn unicode_edits_preserve_compact_formatting_through_undo_and_redo() {
        let mut document = RichComposeDocument::default();
        let text = "Aé👩‍🚀Z";
        let bold_end = text.find('Z').unwrap();
        document.synchronize(text, 1, bold_end as i32);
        document.format("bold", text, 1, bold_end as i32);
        assert_eq!(document.styles.len(), 3);
        assert_style_coverage(&document);

        let after_accent = "Aé".len() as i32;
        document.update_selection(after_accent, after_accent);
        document.insert_text("界");
        assert_eq!(
            document.body_html().as_deref(),
            Some("<div>A<strong>é界👩‍🚀</strong>Z</div>")
        );
        assert_eq!(document.styles.len(), 3);
        assert_style_coverage(&document);

        let shortened = "Aé界Z";
        document.synchronize(shortened, shortened.len() as i32, shortened.len() as i32);
        assert_eq!(
            document.body_html().as_deref(),
            Some("<div>A<strong>é界</strong>Z</div>")
        );
        assert_style_coverage(&document);
        document.history("undo").unwrap();
        assert_eq!(
            document.body_html().as_deref(),
            Some("<div>A<strong>é界👩‍🚀</strong>Z</div>")
        );
        assert_style_coverage(&document);
        document.history("redo").unwrap();
        assert_eq!(
            document.body_html().as_deref(),
            Some("<div>A<strong>é界</strong>Z</div>")
        );
        assert_style_coverage(&document);
    }

    #[test]
    fn undo_replays_automatic_link_changes_and_block_formatting() {
        let mut document = RichComposeDocument::default();
        document.synchronize("http://a", 8, 8);
        assert_eq!(document.active_link(), None);
        document.insert_text("b");
        assert_eq!(
            document.body_html().as_deref(),
            Some("<div><a href=\"http://ab\">http://ab</a></div>")
        );
        document.history("undo").unwrap();
        assert_eq!(
            document.body_html().as_deref(),
            Some("<div><a href=\"http://a\">http://a</a></div>")
        );
        document.history("redo").unwrap();
        assert_eq!(
            document.body_html().as_deref(),
            Some("<div><a href=\"http://ab\">http://ab</a></div>")
        );
        assert_style_coverage(&document);

        document.reset();
        document.synchronize("One\nTwo", 0, 7);
        document.format("bullet", "One\nTwo", 0, 7);
        assert_eq!(
            document.body_html().as_deref(),
            Some("<ul><li>One</li><li>Two</li></ul>")
        );
        document.history("undo").unwrap();
        assert_eq!(
            document.body_html().as_deref(),
            Some("<div>One</div><div>Two</div>")
        );
        document.history("redo").unwrap();
        assert_eq!(
            document.body_html().as_deref(),
            Some("<ul><li>One</li><li>Two</li></ul>")
        );
        assert_style_coverage(&document);
    }

    #[test]
    fn history_entry_limit_keeps_the_recent_128_edits() {
        let mut document = RichComposeDocument::default();
        for _ in 0..140 {
            document.insert_text("x");
        }
        assert_eq!(document.undo.len(), MAX_HISTORY);
        for _ in 0..MAX_HISTORY {
            document.history("undo").unwrap();
        }
        assert_eq!(document.text().len(), 140 - MAX_HISTORY);
        assert!(document.history("undo").is_none());
        for _ in 0..MAX_HISTORY {
            document.history("redo").unwrap();
        }
        assert_eq!(document.text().len(), 140);
        assert_style_coverage(&document);
    }

    #[test]
    fn linked_characters_and_snapshots_share_one_link_allocation() {
        let mut document = RichComposeDocument::default();
        let label = "linked passage";
        document.synchronize(label, 0, label.len() as i32);
        document.set_link("example.org", label, 0, label.len() as i32);
        let first = document.styles[0].style.link.as_ref().unwrap().clone();
        assert!(
            document
                .styles
                .iter()
                .all(|run| Arc::ptr_eq(&first, run.style.link.as_ref().unwrap()))
        );

        document.insert_text("!");
        let snapshot_link = document.undo.back().unwrap().before.styles[0]
            .style
            .link
            .as_ref()
            .unwrap();
        assert!(Arc::ptr_eq(&first, snapshot_link));
        document.history("undo").unwrap();
        assert_eq!(document.active_link(), Some("https://example.org"));
        assert_eq!(
            document.styles[0].style.link.as_deref(),
            Some("https://example.org")
        );
    }

    #[test]
    fn memory_usage_counts_shared_style_only_payloads_once() {
        let mut document = RichComposeDocument::default();
        document.synchronize("linked passage", 0, 14);
        document.set_link("example.org", "linked passage", 0, 14);
        let transaction = document.undo.back().unwrap();
        assert!(Arc::ptr_eq(
            &transaction.before.text,
            &transaction.after.text
        ));
        let usage = document.memory_usage();
        let link = document.styles[0].style.link.as_ref().unwrap();
        assert_eq!(usage.link_bytes, fragment_text_bytes(link));
        assert!(usage.undo_bytes >= fragment_text_bytes(&transaction.before.text));
        assert_eq!(
            usage.total_bytes,
            std::mem::size_of::<RichComposeDocument>()
                + usage.text_capacity
                + usage.style_run_bytes
                + usage.link_bytes
                + usage.undo_bytes
                + usage.redo_bytes
        );
        document.history("undo").unwrap();
        let undone = document.memory_usage();
        assert_eq!(undone.link_bytes, usage.link_bytes);
        assert_eq!(document.text(), "linked passage");
    }

    #[test]
    fn transaction_budget_counts_a_shared_link_once_across_runs() {
        let link: Arc<str> = Arc::from("https://example.org/long/path");
        let text: Arc<str> = Arc::from("ab");
        let styles = vec![
            ComposeStyleRun {
                range: 0..1,
                style: CharacterStyle {
                    marks: BOLD,
                    link: Some(link.clone()),
                    link_is_auto: false,
                },
            },
            ComposeStyleRun {
                range: 1..2,
                style: CharacterStyle {
                    marks: ITALIC,
                    link: Some(link.clone()),
                    link_is_auto: false,
                },
            },
        ];
        let before = EditFragment {
            text: text.clone(),
            span_len: 2,
            styles: styles.clone(),
        };
        let after = EditFragment {
            text,
            span_len: 2,
            styles,
        };
        assert_eq!(
            transaction_bytes(&before, &after),
            fragment_text_bytes(&before.text)
                + (before.styles.capacity() + after.styles.capacity())
                    * std::mem::size_of::<ComposeStyleRun>()
                + fragment_text_bytes(&link)
        );
    }

    #[test]
    fn ordinary_key_edits_group_into_bounded_undo_steps() {
        let mut document = RichComposeDocument::default();
        for character in "hello".chars() {
            let mut next = document.text().to_owned();
            next.push(character);
            let end = next.len() as i32;
            document.synchronize_key_edit(&next, end, end, &character.to_string());
        }
        assert_eq!(document.undo.len(), 1);
        document.history("undo").unwrap();
        assert_eq!(document.text(), "");
        document.history("redo").unwrap();
        assert_eq!(document.text(), "hello");

        document.reset();
        for _ in 0..130 {
            let mut next = document.text().to_owned();
            next.push('x');
            let end = next.len() as i32;
            document.synchronize_key_edit(&next, end, end, "x");
        }
        assert_eq!(document.undo.len(), 3);
        document.history("undo").unwrap();
        assert_eq!(document.text().len(), 128);
    }

    #[test]
    fn typing_groups_break_on_selection_paste_and_ime_edits() {
        let mut document = RichComposeDocument::default();
        document.synchronize_key_edit("a", 1, 1, "a");
        document.synchronize_key_edit("ab", 2, 2, "b");
        document.update_selection(0, 0);
        document.update_selection(2, 2);
        document.synchronize_key_edit("abc", 3, 3, "c");
        assert_eq!(document.undo.len(), 2);

        document.synchronize("abcP", 4, 4); // Paste has no key context.
        document.synchronize_key_edit("abcPq", 5, 5, "q");
        document.synchronize("abcPq語", 8, 8); // IME commit has no key context.
        assert_eq!(document.undo.len(), 5);
        document.history("undo").unwrap();
        assert_eq!(document.text(), "abcPq");
        document.history("undo").unwrap();
        assert_eq!(document.text(), "abcP");
        document.history("undo").unwrap();
        assert_eq!(document.text(), "abc");
        document.history("undo").unwrap();
        assert_eq!(document.text(), "ab");
        document.history("undo").unwrap();
        assert_eq!(document.text(), "");
    }

    #[test]
    fn grouped_unicode_typing_in_middle_preserves_following_text() {
        let mut document = RichComposeDocument::default();
        document.synchronize("left right", 5, 5);
        document.synchronize_key_edit("left 語right", 8, 8, "語");
        document.synchronize_key_edit("left 語界right", 11, 11, "界");
        assert_eq!(document.undo.len(), 2);
        document.history("undo").unwrap();
        assert_eq!(document.text(), "left right");
        assert_eq!(document.selection(), ComposeSelection { start: 5, end: 5 });
        document.history("redo").unwrap();
        assert_eq!(document.text(), "left 語界right");
        assert_style_coverage(&document);
    }

    #[test]
    fn new_typing_after_partial_undo_discards_redo() {
        let mut document = RichComposeDocument::default();
        document.synchronize_key_edit("a", 1, 1, "a");
        document.synchronize_key_edit("ab", 2, 2, "b");
        document.synchronize_key_edit("ab ", 3, 3, " ");
        document.history("undo").unwrap();
        document.synchronize_key_edit("abc", 3, 3, "c");
        assert_eq!(document.text(), "abc");
        assert!(document.redo.is_empty());
        document.history("undo").unwrap();
        assert_eq!(document.text(), "ab");
        document.history("undo").unwrap();
        assert_eq!(document.text(), "");
    }

    #[test]
    fn formatting_and_word_boundaries_break_typing_groups() {
        let mut document = RichComposeDocument::default();
        document.synchronize_key_edit("a", 1, 1, "a");
        document.synchronize_key_edit("ab", 2, 2, "b");
        document.synchronize_key_edit("ab ", 3, 3, " ");
        document.format("bold", "ab ", 0, 2);
        document.update_selection(3, 3);
        document.synchronize_key_edit("ab c", 4, 4, "c");
        assert_eq!(document.undo.len(), 4);
        document.history("undo").unwrap();
        assert_eq!(document.text(), "ab ");
        document.history("undo").unwrap();
        assert_eq!(document.text(), "ab ");
        document.history("undo").unwrap();
        assert_eq!(document.text(), "ab");
        document.history("undo").unwrap();
        assert_eq!(document.text(), "");
    }

    #[test]
    fn rtl_grapheme_and_link_fixture_matches_original_edit_history() {
        // These checkpoints were captured from the original per-character
        // composer before the compact-run and transaction changes.
        let mut document = RichComposeDocument::default();
        let initial = "אבג e\u{301} 👩‍🚀\nمرحبا";
        let appended = format!("{initial}!");
        let plain_html = "<div>אבג e\u{301} 👩‍🚀</div><div>مرحبا</div>";
        let bold_html = "<div><strong>אבג</strong> e\u{301} 👩‍🚀</div><div>مرحبا</div>";
        let link_html = "<div><strong>אבג</strong> e\u{301} <a href=\"https://example.org/a?b=1&amp;c=2\">👩‍🚀</a></div><div>مرحبا</div>";
        let italic_html = "<div><strong>אבג</strong> <em>e\u{301}</em> <a href=\"https://example.org/a?b=1&amp;c=2\">👩‍🚀</a></div><div>مرحبا</div>";
        let appended_html = "<div><strong>אבג</strong> <em>e\u{301}</em> <a href=\"https://example.org/a?b=1&amp;c=2\">👩‍🚀</a></div><div>مرحبا!</div>";
        let href = "https://example.org/a?b=1&c=2";
        let plain_runs = [(0..33, 0, None)];
        let bold_runs = [(0..6, BOLD, None), (6..33, 0, None)];
        let link_runs = [
            (0..6, BOLD, None),
            (6..11, 0, None),
            (11..22, 0, Some(href)),
            (22..33, 0, None),
        ];
        let italic_runs = [
            (0..6, BOLD, None),
            (6..7, 0, None),
            (7..10, ITALIC, None),
            (10..11, 0, None),
            (11..22, 0, Some(href)),
            (22..33, 0, None),
        ];
        let appended_runs = [
            (0..6, BOLD, None),
            (6..7, 0, None),
            (7..10, ITALIC, None),
            (10..11, 0, None),
            (11..22, 0, Some(href)),
            (22..34, 0, None),
        ];

        document.synchronize(initial, 33, 33);
        assert_fixture_state(&document, initial, plain_html, (33, 33), &plain_runs);
        document.format("bold", initial, 0, 6);
        assert_fixture_state(&document, initial, bold_html, (0, 6), &bold_runs);
        document.set_link("example.org/a?b=1&c=2", initial, 11, 22);
        assert_fixture_state(&document, initial, link_html, (11, 22), &link_runs);
        document.format("italic", initial, 7, 10);
        assert_fixture_state(&document, initial, italic_html, (7, 10), &italic_runs);
        document.update_selection(33, 33);
        document.synchronize(&appended, 34, 34);
        assert_fixture_state(
            &document,
            &appended,
            appended_html,
            (34, 34),
            &appended_runs,
        );

        document.history("undo").unwrap();
        assert_fixture_state(&document, initial, italic_html, (33, 33), &italic_runs);
        document.history("undo").unwrap();
        assert_fixture_state(&document, initial, link_html, (7, 10), &link_runs);
        document.history("undo").unwrap();
        assert_fixture_state(&document, initial, bold_html, (11, 22), &bold_runs);
        document.history("undo").unwrap();
        assert_fixture_state(&document, initial, plain_html, (0, 6), &plain_runs);

        document.history("redo").unwrap();
        assert_fixture_state(&document, initial, bold_html, (11, 22), &bold_runs);
        document.history("redo").unwrap();
        assert_fixture_state(&document, initial, link_html, (7, 10), &link_runs);
        document.history("redo").unwrap();
        assert_fixture_state(&document, initial, italic_html, (33, 33), &italic_runs);
        document.history("redo").unwrap();
        assert_fixture_state(
            &document,
            &appended,
            appended_html,
            (34, 34),
            &appended_runs,
        );
    }

    #[test]
    fn rtl_block_modes_and_line_break_fixture_matches_original_history() {
        let mut document = RichComposeDocument::default();
        let plain = "שלום\nمرحبا\nCafe\u{301}";
        let bullet = "• שלום\n• مرحبا\n• Cafe\u{301}";
        let continued = "• שלום\n• مرحبا\n• Cafe\u{301}\n• ";
        let numbered = "1. שלום\n2. مرحبا\n3. Cafe\u{301}\n4. ";
        let quoted = "│ שלום\n│ مرحبا\n│ Cafe\u{301}\n│ ";
        let plain_html = "<div>שלום</div><div>مرحبا</div><div>Cafe\u{301}</div>";
        let bullet_html = "<ul><li>שלום</li><li>مرحبا</li><li>Cafe\u{301}</li></ul>";
        let continued_html = "<ul><li>שלום</li><li>مرحبا</li><li>Cafe\u{301}</li><li></li></ul>";
        let numbered_html = "<ol><li>שלום</li><li>مرحبا</li><li>Cafe\u{301}</li><li></li></ol>";
        let quoted_html = "<blockquote><div>שלום</div><div>مرحبا</div><div>Cafe\u{301}</div><div><br></div></blockquote>";

        document.synchronize(plain, 0, 26);
        assert_fixture_state(&document, plain, plain_html, (0, 26), &[(0..26, 0, None)]);
        document.format("bullet", plain, 0, 26);
        assert_fixture_state(&document, bullet, bullet_html, (0, 38), &[(0..38, 0, None)]);
        let entered = format!("{bullet}\n");
        document.synchronize(&entered, entered.len() as i32, entered.len() as i32);
        assert_fixture_state(
            &document,
            continued,
            continued_html,
            (43, 43),
            &[(0..43, 0, None)],
        );
        document.history("undo").unwrap();
        assert_fixture_state(&document, bullet, bullet_html, (0, 38), &[(0..38, 0, None)]);
        document.history("undo").unwrap();
        assert_fixture_state(&document, plain, plain_html, (0, 26), &[(0..26, 0, None)]);
        document.history("redo").unwrap();
        assert_fixture_state(&document, bullet, bullet_html, (0, 38), &[(0..38, 0, None)]);
        document.history("redo").unwrap();
        assert_fixture_state(
            &document,
            continued,
            continued_html,
            (43, 43),
            &[(0..43, 0, None)],
        );

        document.format("number", continued, 0, 43);
        assert_fixture_state(
            &document,
            numbered,
            numbered_html,
            (0, 39),
            &[(0..39, 0, None)],
        );
        document.format("quote", numbered, 0, 39);
        assert_fixture_state(&document, quoted, quoted_html, (0, 43), &[(0..43, 0, None)]);
        document.history("undo").unwrap();
        assert_fixture_state(
            &document,
            numbered,
            numbered_html,
            (0, 39),
            &[(0..39, 0, None)],
        );
        document.history("undo").unwrap();
        assert_fixture_state(
            &document,
            continued,
            continued_html,
            (0, 43),
            &[(0..43, 0, None)],
        );
        document.history("redo").unwrap();
        assert_fixture_state(
            &document,
            numbered,
            numbered_html,
            (0, 39),
            &[(0..39, 0, None)],
        );
        document.history("redo").unwrap();
        assert_fixture_state(&document, quoted, quoted_html, (0, 43), &[(0..43, 0, None)]);
    }

    #[test]
    fn html_is_escaped_before_formatting() {
        let mut document = RichComposeDocument::default();
        document.synchronize("<script>&", 0, 9);
        document.format("underline", "<script>&", 0, 9);
        assert_eq!(
            document.body_html().as_deref(),
            Some("<div><u>&lt;script&gt;&amp;</u></div>")
        );
    }

    #[test]
    fn combined_native_styles_remain_one_unmodified_text_run() {
        let mut document = RichComposeDocument::default();
        document.synchronize("Hello & welcome", 0, 15);
        document.format("bold", "Hello & welcome", 0, 15);
        document.format("underline", "Hello & welcome", 0, 15);
        document.format("strike", "Hello & welcome", 0, 15);
        document.format("code", "Hello & welcome", 0, 15);

        assert_eq!(document.text(), "Hello & welcome");
        assert_eq!(document.style_runs().len(), 1);
        assert_eq!(document.style_runs()[0].range, 0..15);
        assert_eq!(
            document.style_runs()[0].style.marks,
            BOLD | UNDERLINE | STRIKE | CODE
        );
        assert_eq!(
            document.body_html().as_deref(),
            Some("<div><strong><s><u><code>Hello &amp; welcome</code></u></s></strong></div>")
        );
    }

    fn html_for(text: &str) -> String {
        let mut document = RichComposeDocument::default();
        document.load(text, 0, 0);
        document.body_html().unwrap()
    }

    #[test]
    fn numbered_lists_keep_the_numbers_the_composer_shows() {
        assert_eq!(
            html_for("1. First point\n\n2. Second point"),
            "<ol><li>First point</li></ol><div><br></div><ol start=\"2\"><li>Second point</li></ol>"
        );
        assert_eq!(
            html_for("1. one\n2. two\nA note\n3. three"),
            "<ol><li>one</li><li>two</li></ol><div>A note</div><ol start=\"3\"><li>three</li></ol>"
        );
        assert_eq!(
            html_for("2. two\n3. three"),
            "<ol start=\"2\"><li>two</li><li>three</li></ol>"
        );
        assert_eq!(
            html_for("1. a\n2. b\n5. c"),
            "<ol><li>a</li><li>b</li></ol><ol start=\"5\"><li>c</li></ol>"
        );
        assert_eq!(
            html_for("2026. was a good year"),
            "<ol start=\"2026\"><li>was a good year</li></ol>"
        );
        assert_eq!(
            html_for("99999999999. too long for a list"),
            "<div>99999999999. too long for a list</div>"
        );
    }

    #[test]
    fn quoted_lines_share_one_blockquote() {
        assert_eq!(
            html_for("│ first\n│ second\n│ \nAfter"),
            "<blockquote><div>first</div><div>second</div><div><br></div></blockquote><div>After</div>"
        );
    }

    #[test]
    fn spaces_html_would_collapse_are_written_as_no_break_spaces() {
        assert_eq!(
            html_for("  two  spaces "),
            "<div>&nbsp;&nbsp;two &nbsp;spaces&nbsp;</div>"
        );
        assert_eq!(html_for("one space"), "<div>one space</div>");
        assert_eq!(html_for("a\tb"), "<div>a&nbsp;&nbsp;&nbsp;&nbsp;b</div>");

        let mut document = RichComposeDocument::default();
        document.load("Hello  world", 0, 0);
        document.format("bold", "Hello  world", 0, 6);
        assert_eq!(
            document.body_html().as_deref(),
            Some("<div><strong>Hello </strong>&nbsp;world</div>")
        );
    }
}
