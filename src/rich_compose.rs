use std::ops::Range;

pub(crate) const BOLD: u8 = 1 << 0;
pub(crate) const ITALIC: u8 = 1 << 1;
pub(crate) const UNDERLINE: u8 = 1 << 2;
pub(crate) const STRIKE: u8 = 1 << 3;
pub(crate) const CODE: u8 = 1 << 4;
const MAX_HISTORY: usize = 128;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CharacterStyle {
    pub(crate) marks: u8,
    pub(crate) link: Option<String>,
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
    styles: Vec<CharacterStyle>,
    typing_style: CharacterStyle,
    typing_override: Option<usize>,
    selection: (i32, i32),
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
/// The editable string never contains markup. A parallel style cell is retained
/// for each Unicode scalar and exposed as byte-ranged runs to the cosmic-text
/// layout surface. Slint's hidden TextInput is only the platform IME/clipboard
/// bridge; it is not used to position visible text, selection, or the caret.
#[derive(Default)]
pub struct RichComposeDocument {
    text: String,
    styles: Vec<CharacterStyle>,
    typing_style: CharacterStyle,
    typing_override: Option<usize>,
    selection: (i32, i32),
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    revision: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BlockKind {
    Bullet,
    Number,
    Quote,
}

#[derive(Clone, Debug, Default)]
struct RichLine {
    chars: Vec<char>,
    styles: Vec<CharacterStyle>,
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

    pub(crate) fn style_runs(&self) -> Vec<ComposeStyleRun> {
        let mut runs = Vec::new();
        let mut start = 0usize;
        let mut active: Option<CharacterStyle> = None;

        for ((byte, _), style) in self.text.char_indices().zip(self.styles.iter()) {
            if active.as_ref() != Some(style) {
                if let Some(previous) = active.replace(style.clone()) {
                    runs.push(ComposeStyleRun {
                        range: start..byte,
                        style: previous,
                    });
                }
                start = byte;
            }
        }
        if let Some(style) = active {
            runs.push(ComposeStyleRun {
                range: start..self.text.len(),
                style,
            });
        }
        runs
    }

    pub(crate) fn typing_style(&self) -> &CharacterStyle {
        &self.typing_style
    }

    pub fn synchronize(&mut self, new_text: &str, anchor: i32, cursor: i32) -> ComposeSelection {
        let continuation = continue_list_edit(&self.text, new_text);
        let (effective_text, anchor, cursor) = continuation
            .map(|(text, position)| (text, position, position))
            .unwrap_or_else(|| (new_text.to_owned(), anchor, cursor));

        if effective_text != self.text {
            self.push_undo();
            self.redo.clear();
            self.replace_text_preserving_styles(&effective_text);
            self.auto_link_urls();
            self.bump_revision();
        }
        self.update_selection(anchor, cursor)
    }

    pub fn update_selection(&mut self, anchor: i32, cursor: i32) -> ComposeSelection {
        let anchor = valid_offset(&self.text, anchor);
        let cursor = valid_offset(&self.text, cursor);
        self.selection = (to_i32(anchor), to_i32(cursor));

        if anchor == cursor && self.typing_override != Some(cursor) {
            let char_index = byte_to_char(&self.text, cursor);
            self.typing_style = if char_index > 0
                && self
                    .text
                    .chars()
                    .nth(char_index - 1)
                    .is_some_and(|character| character != '\n')
            {
                self.styles.get(char_index - 1).cloned().unwrap_or_default()
            } else {
                self.styles.get(char_index).cloned().unwrap_or_default()
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

    pub fn format(
        &mut self,
        kind: &str,
        source: &str,
        anchor: i32,
        cursor: i32,
    ) -> ComposeSelection {
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
        let snapshot = if direction == "redo" {
            let snapshot = self.redo.pop()?;
            self.undo.push(self.snapshot());
            snapshot
        } else {
            let snapshot = self.undo.pop()?;
            self.redo.push(self.snapshot());
            snapshot
        };

        self.restore(snapshot);
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
            let start = byte_to_char(&self.text, start);
            let end = byte_to_char(&self.text, end);
            let mut relevant = self.styles[start..end]
                .iter()
                .zip(
                    self.text
                        .chars()
                        .skip(start)
                        .take(end.saturating_sub(start)),
                )
                .filter(|(_, character)| !character.is_whitespace())
                .map(|(style, _)| style.marks);
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

        let start = byte_to_char(&self.text, start);
        let end = byte_to_char(&self.text, end);
        let mut links = self.styles[start..end]
            .iter()
            .zip(
                self.text
                    .chars()
                    .skip(start)
                    .take(end.saturating_sub(start)),
            )
            .filter(|(_, character)| !character.is_whitespace())
            .map(|(style, _)| style.link.as_deref());
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

        let normalized = normalize_link(url);
        let start = byte_to_char(&self.text, start_byte);
        let end = byte_to_char(&self.text, end_byte);
        self.push_undo();
        self.redo.clear();
        self.typing_override = None;
        for style in &mut self.styles[start..end] {
            style.link = normalized.clone();
            style.link_is_auto = false;
        }
        self.selection = (to_i32(start_byte), to_i32(end_byte));
        self.bump_revision();
        ComposeSelection {
            start: to_i32(start_byte),
            end: to_i32(end_byte),
        }
    }

    pub fn body_html(&self) -> Option<String> {
        if self.text.trim().is_empty() {
            return None;
        }

        let lines = self.lines();
        let mut html = String::new();
        let mut open_list: Option<BlockKind> = None;

        for line in &lines {
            let prefix = block_prefix(&line.chars);
            let block = prefix.map(|(kind, _)| kind);
            if !matches!(block, Some(BlockKind::Bullet | BlockKind::Number)) {
                close_list(&mut html, &mut open_list);
            }

            match prefix {
                Some((BlockKind::Bullet, prefix_len)) => {
                    ensure_list(&mut html, &mut open_list, BlockKind::Bullet);
                    html.push_str("<li>");
                    html.push_str(&inline_html(line, prefix_len));
                    html.push_str("</li>");
                }
                Some((BlockKind::Number, prefix_len)) => {
                    ensure_list(&mut html, &mut open_list, BlockKind::Number);
                    html.push_str("<li>");
                    html.push_str(&inline_html(line, prefix_len));
                    html.push_str("</li>");
                }
                Some((BlockKind::Quote, prefix_len)) => {
                    html.push_str("<blockquote>");
                    let content = inline_html(line, prefix_len);
                    html.push_str(if content.is_empty() { "<br>" } else { &content });
                    html.push_str("</blockquote>");
                }
                None => {
                    html.push_str("<div>");
                    let content = inline_html(line, 0);
                    html.push_str(if content.is_empty() { "<br>" } else { &content });
                    html.push_str("</div>");
                }
            }
        }
        close_list(&mut html, &mut open_list);
        Some(html)
    }

    fn toggle_mark(&mut self, mark: u8, start_byte: usize, end_byte: usize) {
        if start_byte == end_byte {
            self.typing_style.marks ^= mark;
            self.typing_override = Some(start_byte);
            self.bump_revision();
            return;
        }

        let start = byte_to_char(&self.text, start_byte);
        let end = byte_to_char(&self.text, end_byte);
        self.push_undo();
        self.redo.clear();
        self.typing_override = None;
        let remove = self.styles[start..end]
            .iter()
            .zip(
                self.text
                    .chars()
                    .skip(start)
                    .take(end.saturating_sub(start)),
            )
            .filter(|(_, character)| !character.is_whitespace())
            .all(|(style, _)| style.marks & mark != 0);
        for style in &mut self.styles[start..end] {
            if remove {
                style.marks &= !mark;
            } else {
                style.marks |= mark;
            }
        }
        self.bump_revision();
    }

    fn toggle_block(
        &mut self,
        kind: BlockKind,
        start_byte: usize,
        end_byte: usize,
    ) -> ComposeSelection {
        let chars = self.text.chars().collect::<Vec<_>>();
        let start_char = byte_to_char(&self.text, start_byte);
        let end_char = byte_to_char(&self.text, end_byte);
        let start_line = chars[..start_char].iter().filter(|&&c| c == '\n').count();
        let end_probe = if end_char > start_char {
            end_char.saturating_sub(1)
        } else {
            end_char
        };
        let end_line = chars[..end_probe.min(chars.len())]
            .iter()
            .filter(|&&c| c == '\n')
            .count();

        let mut lines = self.lines();
        if lines.is_empty() || start_line >= lines.len() {
            return ComposeSelection {
                start: to_i32(start_byte),
                end: to_i32(end_byte),
            };
        }
        let end_line = end_line.min(lines.len() - 1);
        self.push_undo();
        self.redo.clear();
        self.typing_override = None;

        let all_target = lines[start_line..=end_line]
            .iter()
            .all(|line| block_prefix(&line.chars).is_some_and(|(current, _)| current == kind));

        for (offset, line) in lines[start_line..=end_line].iter_mut().enumerate() {
            if let Some((_, prefix_len)) = block_prefix(&line.chars) {
                line.chars.drain(..prefix_len);
                line.styles.drain(..prefix_len);
            }
            if all_target {
                continue;
            }
            let prefix = match kind {
                BlockKind::Bullet => "• ".to_owned(),
                BlockKind::Number => format!("{}. ", offset + 1),
                BlockKind::Quote => "│ ".to_owned(),
            };
            let prefix_chars = prefix.chars().collect::<Vec<_>>();
            line.styles.splice(
                0..0,
                std::iter::repeat_n(CharacterStyle::default(), prefix_chars.len()),
            );
            line.chars.splice(0..0, prefix_chars);
        }

        self.rebuild_lines(lines);
        self.bump_revision();
        let rebuilt_chars = self.text.chars().collect::<Vec<_>>();
        let selection_start_char = line_start_char(&rebuilt_chars, start_line);
        let selection_end_char = line_end_char(&rebuilt_chars, end_line);
        let start = char_to_byte(&self.text, selection_start_char);
        let end = char_to_byte(&self.text, selection_end_char);
        self.selection = (to_i32(start), to_i32(end));
        ComposeSelection {
            start: to_i32(start),
            end: to_i32(end),
        }
    }

    fn replace_text_preserving_styles(&mut self, new_text: &str) {
        let old_chars = self.text.chars().collect::<Vec<_>>();
        let new_chars = new_text.chars().collect::<Vec<_>>();
        let mut prefix = 0usize;
        while prefix < old_chars.len()
            && prefix < new_chars.len()
            && old_chars[prefix] == new_chars[prefix]
        {
            prefix += 1;
        }
        let mut suffix = 0usize;
        while suffix < old_chars.len().saturating_sub(prefix)
            && suffix < new_chars.len().saturating_sub(prefix)
            && old_chars[old_chars.len() - 1 - suffix] == new_chars[new_chars.len() - 1 - suffix]
        {
            suffix += 1;
        }

        let inserted = new_chars.len().saturating_sub(prefix + suffix);
        let inherited = if inserted > 0 {
            self.typing_style.clone()
        } else {
            CharacterStyle::default()
        };
        let mut styles = Vec::with_capacity(new_chars.len());
        styles.extend(self.styles[..prefix.min(self.styles.len())].iter().cloned());
        styles.extend(std::iter::repeat_n(inherited, inserted));
        if suffix > 0 {
            styles.extend(self.styles[self.styles.len() - suffix..].iter().cloned());
        }
        styles.resize(new_chars.len(), CharacterStyle::default());
        self.text = new_text.to_owned();
        self.styles = styles;
    }

    fn auto_link_urls(&mut self) {
        for style in &mut self.styles {
            if style.link_is_auto {
                style.link = None;
                style.link_is_auto = false;
            }
        }

        let chars = self.text.chars().collect::<Vec<_>>();
        let mut start = 0usize;
        while start < chars.len() {
            while start < chars.len() && chars[start].is_whitespace() {
                start += 1;
            }
            let mut end = start;
            while end < chars.len() && !chars[end].is_whitespace() {
                end += 1;
            }
            let mut url_end = end;
            while url_end > start
                && matches!(
                    chars[url_end - 1],
                    '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '}'
                )
            {
                url_end -= 1;
            }
            if url_end > start {
                let candidate = chars[start..url_end].iter().collect::<String>();
                if candidate.starts_with("https://") || candidate.starts_with("http://") {
                    for style in &mut self.styles[start..url_end] {
                        if style.link.is_none() || style.link_is_auto {
                            style.link = Some(candidate.clone());
                            style.link_is_auto = true;
                        }
                    }
                }
            }
            start = end.max(start + 1);
        }
    }

    fn lines(&self) -> Vec<RichLine> {
        let mut lines = vec![RichLine::default()];
        for (character, style) in self.text.chars().zip(self.styles.iter().cloned()) {
            if character == '\n' {
                lines.push(RichLine::default());
            } else if let Some(line) = lines.last_mut() {
                line.chars.push(character);
                line.styles.push(style);
            }
        }
        lines
    }

    fn rebuild_lines(&mut self, lines: Vec<RichLine>) {
        let mut text = String::new();
        let mut styles = Vec::new();
        for (index, line) in lines.into_iter().enumerate() {
            if index > 0 {
                text.push('\n');
                styles.push(CharacterStyle::default());
            }
            text.extend(line.chars.iter());
            styles.extend(line.styles);
        }
        self.text = text;
        self.styles = styles;
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            text: self.text.clone(),
            styles: self.styles.clone(),
            typing_style: self.typing_style.clone(),
            typing_override: self.typing_override,
            selection: self.selection,
        }
    }

    fn push_undo(&mut self) {
        if self.undo.len() == MAX_HISTORY {
            self.undo.remove(0);
        }
        self.undo.push(self.snapshot());
    }

    fn restore(&mut self, snapshot: Snapshot) {
        self.text = snapshot.text;
        self.styles = snapshot.styles;
        self.typing_style = snapshot.typing_style;
        self.typing_override = snapshot.typing_override;
        self.selection = snapshot.selection;
        self.bump_revision();
    }

    fn bump_revision(&mut self) {
        self.revision = self.revision.wrapping_add(1);
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
    let old = old_text.chars().collect::<Vec<_>>();
    let mut new = new_text.chars().collect::<Vec<_>>();
    if new.len() != old.len() + 1 {
        return None;
    }

    let mut insertion = 0usize;
    while insertion < old.len() && old[insertion] == new[insertion] {
        insertion += 1;
    }
    if new.get(insertion) != Some(&'\n') || old[insertion..] != new[insertion + 1..] {
        return None;
    }

    let line_start = old[..insertion]
        .iter()
        .rposition(|character| *character == '\n')
        .map_or(0, |index| index + 1);
    let line = &old[line_start..insertion];
    let (kind, prefix_len) = block_prefix(line)?;
    let content_is_empty = line[prefix_len..]
        .iter()
        .all(|character| character.is_whitespace());
    if content_is_empty {
        new.drain(line_start..=insertion);
        let text = new.iter().collect::<String>();
        return Some((text.clone(), to_i32(char_to_byte(&text, line_start))));
    }

    let prefix = match kind {
        BlockKind::Bullet => "• ".to_owned(),
        BlockKind::Quote => "│ ".to_owned(),
        BlockKind::Number => {
            let current = line[..prefix_len.saturating_sub(2)]
                .iter()
                .collect::<String>()
                .parse::<usize>()
                .unwrap_or(1);
            format!("{}. ", current.saturating_add(1))
        }
    };
    let prefix = prefix.chars().collect::<Vec<_>>();
    let cursor_character = insertion + 1 + prefix.len();
    new.splice(insertion + 1..insertion + 1, prefix);
    let text = new.iter().collect::<String>();
    Some((text.clone(), to_i32(char_to_byte(&text, cursor_character))))
}

fn block_prefix(chars: &[char]) -> Option<(BlockKind, usize)> {
    if chars.starts_with(&['•', ' ']) {
        return Some((BlockKind::Bullet, 2));
    }
    if chars.starts_with(&['│', ' ']) {
        return Some((BlockKind::Quote, 2));
    }
    let digits = chars
        .iter()
        .take_while(|character| character.is_ascii_digit())
        .count();
    if digits > 0 && chars.get(digits) == Some(&'.') && chars.get(digits + 1) == Some(&' ') {
        return Some((BlockKind::Number, digits + 2));
    }
    None
}

fn ensure_list(html: &mut String, current: &mut Option<BlockKind>, requested: BlockKind) {
    if *current == Some(requested) {
        return;
    }
    close_list(html, current);
    html.push_str(if requested == BlockKind::Bullet {
        "<ul>"
    } else {
        "<ol>"
    });
    *current = Some(requested);
}

fn close_list(html: &mut String, current: &mut Option<BlockKind>) {
    if let Some(kind) = current.take() {
        html.push_str(if kind == BlockKind::Bullet {
            "</ul>"
        } else {
            "</ol>"
        });
    }
}

fn inline_html(line: &RichLine, skip: usize) -> String {
    let mut result = String::new();
    let mut start = skip.min(line.chars.len());
    while start < line.chars.len() {
        let style = &line.styles[start];
        let mut end = start + 1;
        while end < line.chars.len() && line.styles[end] == *style {
            end += 1;
        }
        let escaped = line.chars[start..end]
            .iter()
            .copied()
            .map(escape_html_char)
            .collect::<String>();
        result.push_str(&html_run(&escaped, style));
        start = end;
    }
    result
}

fn html_run(escaped: &str, style: &CharacterStyle) -> String {
    if escaped.is_empty() {
        return String::new();
    }
    let mut value = escaped.to_owned();
    if style.marks & CODE != 0 {
        value = format!("<code>{value}</code>");
    }
    if style.marks & UNDERLINE != 0 {
        value = format!("<u>{value}</u>");
    }
    if style.marks & STRIKE != 0 {
        value = format!("<s>{value}</s>");
    }
    if style.marks & ITALIC != 0 {
        value = format!("<em>{value}</em>");
    }
    if style.marks & BOLD != 0 {
        value = format!("<strong>{value}</strong>");
    }
    if let Some(url) = style.link.as_deref() {
        value = format!("<a href=\"{}\">{value}</a>", escape_html_attribute(url));
    }
    value
}

fn escape_html_char(character: char) -> String {
    match character {
        '&' => "&amp;".to_owned(),
        '<' => "&lt;".to_owned(),
        '>' => "&gt;".to_owned(),
        '"' => "&quot;".to_owned(),
        '\'' => "&#39;".to_owned(),
        _ => character.to_string(),
    }
}

fn escape_html_attribute(value: &str) -> String {
    value.chars().map(escape_html_char).collect()
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

fn byte_to_char(text: &str, byte: usize) -> usize {
    text[..byte.min(text.len())].chars().count()
}

fn char_to_byte(text: &str, character: usize) -> usize {
    text.char_indices()
        .nth(character)
        .map_or(text.len(), |(byte, _)| byte)
}

fn line_start_char(chars: &[char], target: usize) -> usize {
    if target == 0 {
        return 0;
    }
    chars
        .iter()
        .enumerate()
        .filter(|(_, character)| **character == '\n')
        .nth(target - 1)
        .map_or(chars.len(), |(index, _)| index + 1)
}

fn line_end_char(chars: &[char], target: usize) -> usize {
    let start = line_start_char(chars, target);
    chars[start..]
        .iter()
        .position(|&character| character == '\n')
        .map_or(chars.len(), |relative| start + relative)
}

fn to_i32(value: usize) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
