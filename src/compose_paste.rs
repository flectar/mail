//! Text pasted with its formatting. Desktop clipboards hold what's copied from a web page, a
//! document or another message as HTML too, and this reads that HTML into the composer's text
//! and styles: what the composer can write, matched to the message, and nothing else.
//!
//! As Merge Formatting does in word processors, the look the text only had where it came from
//! gives way to the message's: its usual font, size and gray or black color become the
//! message's own, and page backgrounds and colors that would vanish on a light page go. What
//! marks out parts of it stays: bold and the other marks, links, lists, quotes, headings,
//! colors, highlights and alignment.

use crate::rich_compose::{
    Alignment, BOLD, CODE, CharacterStyle, ComposeStyleRun, FontFamily, ITALIC, QUOTE_MARKER, Rgb,
    STRIKE, TextSize, UNDERLINE,
};
use html5ever::tokenizer::{
    BufferQueue, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer, states::RawKind,
};
use std::{cell::RefCell, ops::Range, sync::Arc};

/// Clipboard HTML larger than this pastes as plain text: reading it would hold up typing.
const MAX_HTML_BYTES: usize = 4 << 20;
/// The size of normal text in CSS pixels, which `font-size` keywords and `<font size>` scale.
const NORMAL_PX: f32 = 16.0;

/// The text of `html` with the styles it gives it, matched to a message whose default text
/// style is `base`, or `None` when it holds no text.
pub(crate) fn styled_text(
    html: &str,
    base: &CharacterStyle,
) -> Option<(String, Vec<ComposeStyleRun>)> {
    if html.len() > MAX_HTML_BYTES {
        return None;
    }
    let tokenizer = Tokenizer::new(Sink(RefCell::default()), Default::default());
    let input = BufferQueue::default();
    input.push_back(html.into());
    let _ = tokenizer.feed(&input);
    tokenizer.end();
    let parts = tokenizer.sink.0.into_inner();
    // Browsers mark what was copied, with the elements that hold it around it.
    let marked = parts
        .iter()
        .any(|part| matches!(part, Part::Comment(text) if text.trim() == "StartFragment"));
    // Mail written in Outlook spaces its paragraphs with empty ones, in a style sheet that takes
    // their margins off, which a copy can leave out: there, each paragraph is a line.
    let paragraph_breaks = if has_empty_paragraph(&parts) { 1 } else { 2 };
    let mut reader = Reader::new(!marked, paragraph_breaks);
    for part in parts {
        reader.part(part);
    }
    reader.finish(base)
}

/// Whether a paragraph holds no text, only spaces.
fn has_empty_paragraph(parts: &[Part]) -> bool {
    let mut paragraph_has_text = None;
    for part in parts {
        match part {
            Part::Start { name, .. } if name == "p" => {
                if paragraph_has_text == Some(false) {
                    return true;
                }
                paragraph_has_text = Some(false);
            }
            Part::End(name) if name == "p" => {
                if paragraph_has_text == Some(false) {
                    return true;
                }
                paragraph_has_text = None;
            }
            Part::Text(text)
                if paragraph_has_text.is_some()
                    && text.chars().any(|character| !character.is_whitespace()) =>
            {
                paragraph_has_text = Some(true);
            }
            _ => {}
        }
    }
    false
}

enum Part {
    Start {
        name: String,
        attributes: Vec<(String, String)>,
    },
    End(String),
    Text(String),
    Comment(String),
}

struct Sink(RefCell<Vec<Part>>);

impl TokenSink for Sink {
    type Handle = ();

    fn process_token(&self, token: Token, _: u64) -> TokenSinkResult<()> {
        let mut parts = self.0.borrow_mut();
        match token {
            Token::TagToken(tag) if tag.kind == TagKind::EndTag => {
                parts.push(Part::End(tag.name.as_str().to_ascii_lowercase()));
            }
            Token::TagToken(tag) => {
                let name = tag.name.as_str().to_ascii_lowercase();
                // Style sheets and scripts hold text that isn't markup.
                let raw = match name.as_str() {
                    "style" | "xmp" | "iframe" | "noembed" | "noframes" => Some(RawKind::Rawtext),
                    "script" => Some(RawKind::ScriptData),
                    "title" | "textarea" => Some(RawKind::Rcdata),
                    _ => None,
                };
                let attributes = tag
                    .attrs
                    .iter()
                    .map(|attribute| {
                        (
                            attribute.name.local.as_str().to_ascii_lowercase(),
                            attribute.value.to_string(),
                        )
                    })
                    .collect();
                parts.push(Part::Start { name, attributes });
                if let Some(raw) = raw.filter(|_| !tag.self_closing) {
                    return TokenSinkResult::RawData(raw);
                }
            }
            Token::CharacterTokens(text) => parts.push(Part::Text(text.to_string())),
            Token::CommentToken(text) => parts.push(Part::Comment(text.to_string())),
            _ => {}
        }
        TokenSinkResult::Continue
    }
}

/// How text looks where it comes from, before it's matched to the message.
#[derive(Clone, Debug, PartialEq)]
struct Look {
    marks: u8,
    link: Option<Arc<str>>,
    color: Option<Rgb>,
    highlight: Option<Rgb>,
    px: f32,
    family: Option<FontFamily>,
    align: Alignment,
    preformatted: bool,
    hidden: bool,
}

impl Default for Look {
    fn default() -> Self {
        Self {
            marks: 0,
            link: None,
            color: None,
            highlight: None,
            px: NORMAL_PX,
            family: None,
            align: Alignment::Left,
            preformatted: false,
            hidden: false,
        }
    }
}

/// What a run of the pasted text is.
#[derive(Clone, Debug, PartialEq)]
enum Piece {
    Text(Look),
    /// A list or quote marker, which readers show in their usual text.
    Marker(Alignment),
    LineEnd(Alignment),
}

/// An element being read, and how what's in it looks.
struct Element {
    name: String,
    look: Look,
    /// A list item, whose marker goes before its first text.
    item: bool,
    /// Line breaks before what follows it: 1 for a block, 2 for a paragraph.
    breaks: usize,
    list: bool,
    quote: bool,
}

struct List {
    ordered: bool,
    next: u64,
}

struct Reader {
    open: Vec<Element>,
    text: String,
    pieces: Vec<(Range<usize>, Piece)>,
    /// Whether the line being written has text yet.
    line_has_text: bool,
    pending_breaks: usize,
    pending_space: bool,
    /// The marker of a list item, written before its first text.
    marker: Option<String>,
    lists: Vec<List>,
    quotes: usize,
    /// Whether the quote being read has text yet, so that a blank line is part of it.
    quote_has_text: bool,
    /// Cells written in the table row being read.
    cells: usize,
    /// Whether the text being read was copied, rather than what holds it.
    copying: bool,
    /// Inside a Word conditional comment that holds its own list bullets.
    in_conditional: bool,
    /// Line breaks after a paragraph.
    paragraph_breaks: usize,
    /// How long the marker the line being written starts with is.
    line_marker_len: usize,
}

impl Reader {
    fn new(copying: bool, paragraph_breaks: usize) -> Self {
        Self {
            open: Vec::new(),
            text: String::new(),
            pieces: Vec::new(),
            line_has_text: false,
            pending_breaks: 0,
            pending_space: false,
            marker: None,
            lists: Vec::new(),
            quotes: 0,
            quote_has_text: false,
            cells: 0,
            copying,
            in_conditional: false,
            paragraph_breaks,
            line_marker_len: 0,
        }
    }

    fn look(&self) -> Look {
        self.open
            .last()
            .map_or_else(Look::default, |element| element.look.clone())
    }

    fn part(&mut self, part: Part) {
        match part {
            Part::Start { name, attributes } => self.start(name, &attributes),
            Part::End(name) => self.end(&name),
            Part::Text(text) => {
                let look = self.look();
                if self.copying && !look.hidden && !self.in_conditional {
                    self.text_in(&text, &look);
                }
            }
            Part::Comment(comment) => {
                let comment = comment.trim();
                match comment {
                    "StartFragment" => self.copying = true,
                    "EndFragment" => self.copying = false,
                    // Word writes list bullets and numbers as text inside these, for readers
                    // that don't know its lists.
                    _ if comment.starts_with("[if !support") => self.in_conditional = true,
                    "[endif]" => self.in_conditional = false,
                    _ => {}
                }
            }
        }
    }

    fn start(&mut self, name: String, attributes: &[(String, String)]) {
        let attribute = |wanted: &str| {
            attributes
                .iter()
                .find(|(name, _)| name == wanted)
                .map(|(_, value)| value.as_str())
        };
        match name.as_str() {
            "br" => {
                let look = self.look();
                if self.copying && !look.hidden && !self.in_conditional {
                    self.line_break(&look);
                }
                return;
            }
            "hr" => {
                self.request_breaks(1);
                return;
            }
            // Images wait for the composer to hold them; the rest hold no text.
            "img" | "wbr" | "meta" | "link" | "input" | "col" | "area" | "base" | "embed"
            | "param" | "source" | "track" | "basefont" => return,
            _ => {}
        }

        let mut look = self.look();
        let mut breaks = 0;
        let mut list = false;
        let mut quote = false;
        match name.as_str() {
            "b" | "strong" => look.marks |= BOLD,
            "i" | "em" | "cite" | "dfn" | "var" => look.marks |= ITALIC,
            "u" | "ins" => look.marks |= UNDERLINE,
            "s" | "strike" | "del" => look.marks |= STRIKE,
            "code" | "kbd" | "samp" | "tt" => look.marks |= CODE,
            "mark" => look.highlight = Some(0xff_ff_00),
            "small" => look.px *= 0.83,
            "big" => look.px *= 1.2,
            "a" => {
                if let Some(href) = attribute("href").map(str::trim).filter(|href| {
                    let lower = href.to_ascii_lowercase();
                    ["http://", "https://", "mailto:"]
                        .iter()
                        .any(|scheme| lower.starts_with(scheme))
                }) {
                    look.link = Some(Arc::from(href));
                }
            }
            "font" => {
                if let Some(color) = attribute("color").and_then(parse_color) {
                    look.color = color;
                }
                if let Some(px) = attribute("size").and_then(font_size_px) {
                    look.px = px;
                }
                if let Some(face) = attribute("face") {
                    look.family = Some(classify_family(face));
                }
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                look.marks |= BOLD;
                look.px = match name.as_str() {
                    "h1" => 32.0,
                    "h2" => 24.0,
                    "h3" => 18.72,
                    "h4" => 16.0,
                    "h5" => 13.28,
                    _ => 10.72,
                };
                breaks = 1;
            }
            "pre" | "listing" | "plaintext" => {
                look.preformatted = true;
                look.marks |= CODE;
                breaks = 1;
            }
            "p" => breaks = self.paragraph_breaks,
            "blockquote" => {
                quote = true;
                breaks = 1;
            }
            "ul" | "ol" | "menu" | "dir" => {
                list = true;
                breaks = 1;
            }
            "li" => breaks = 1,
            "center" => {
                look.align = Alignment::Center;
                breaks = 1;
            }
            "tr" => {
                self.cells = 0;
                breaks = 1;
            }
            "td" | "th" => {
                if name == "th" {
                    look.marks |= BOLD;
                }
                if self.cells > 0 && self.line_has_text && self.copying {
                    self.pending_space = false;
                    self.write("\t", &look);
                }
                self.cells += 1;
            }
            "div" | "section" | "article" | "header" | "footer" | "nav" | "aside" | "main"
            | "figure" | "figcaption" | "address" | "dl" | "dt" | "dd" | "fieldset" | "form"
            | "details" | "summary" | "hgroup" | "legend" | "table" | "thead" | "tbody"
            | "tfoot" | "caption" | "option" | "body" => breaks = 1,
            "head" | "template" | "noscript" | "svg" | "math" | "select" | "object" | "audio"
            | "video" | "canvas" | "style" | "script" | "title" | "textarea" | "xmp" | "iframe"
            | "noembed" | "noframes" | "button" => look.hidden = true,
            _ => {}
        }
        if let Some(align) = attribute("align").and_then(parse_alignment) {
            look.align = align;
        }
        if attribute("hidden").is_some() {
            look.hidden = true;
        }
        let mut word_list_item = false;
        if let Some(style) = attribute("style") {
            let parent_px = self.look().px;
            let margins_off = apply_css(style, &mut look, parent_px, &mut word_list_item);
            // A paragraph without margins, as document editors write each line, and Word's list
            // paragraphs are a line each.
            if name == "p" && (margins_off || word_list_item) {
                breaks = 1;
            }
        }
        // Inside a list, paragraphs are its items' lines.
        if breaks == 2 && !self.lists.is_empty() {
            breaks = 1;
        }

        if breaks > 0 {
            self.request_breaks(breaks);
        }
        if list {
            let start = attribute("start")
                .and_then(|start| start.trim().parse().ok())
                .unwrap_or(1);
            self.lists.push(List {
                ordered: name == "ol",
                next: start,
            });
        }
        if quote {
            self.quotes += 1;
            self.quote_has_text = false;
        }
        if name == "li" || word_list_item {
            self.marker = Some(match self.lists.last_mut() {
                Some(list) if list.ordered => {
                    let number = list.next;
                    list.next += 1;
                    format!("{number}. ")
                }
                _ => "• ".to_owned(),
            });
        }
        let item = name == "li" || word_list_item;
        self.open.push(Element {
            name,
            look,
            item,
            breaks,
            list,
            quote,
        });
    }

    fn end(&mut self, name: &str) {
        // Close what's still open inside it too, as browsers do for markup copied in part.
        let Some(index) = self.open.iter().rposition(|element| element.name == name) else {
            return;
        };
        for element in self.open.drain(index..).rev().collect::<Vec<_>>() {
            if element.list {
                self.lists.pop();
            }
            if element.quote {
                self.quotes = self.quotes.saturating_sub(1);
                self.quote_has_text = self.quotes > 0;
            }
            if element.item {
                self.marker = None;
            }
            if element.breaks > 0 {
                self.request_breaks(element.breaks);
            }
        }
    }

    fn request_breaks(&mut self, breaks: usize) {
        self.pending_breaks = self.pending_breaks.max(breaks);
        self.pending_space = false;
    }

    /// Writes text as HTML shows it: runs of white space as one space, none at the start or end
    /// of a line, except in preformatted text, which keeps its spaces and line breaks.
    fn text_in(&mut self, text: &str, look: &Look) {
        if look.preformatted {
            for line in text.split_inclusive('\n') {
                let content = line.trim_end_matches(['\n', '\r']);
                if !content.is_empty() {
                    self.write(content, look);
                }
                if line.ends_with('\n') {
                    self.line_break(look);
                }
            }
            return;
        }
        let mut word = String::new();
        for character in text.chars() {
            if matches!(character, ' ' | '\t' | '\n' | '\r' | '\u{c}') {
                if !word.is_empty() {
                    self.write(&word, look);
                    word.clear();
                }
                if self.line_has_text {
                    self.pending_space = true;
                }
            } else {
                // A no-break space keeps its place, as a plain space in the message's text.
                word.push(if character == '\u{a0}' {
                    ' '
                } else {
                    character
                });
            }
        }
        if !word.is_empty() {
            self.write(&word, look);
        }
    }

    /// Writes `text`, after the line breaks, marker and space that come before it.
    fn write(&mut self, text: &str, look: &Look) {
        if self.pending_breaks > 0 {
            if !self.text.is_empty() {
                if self.line_has_text {
                    self.end_line(look.align);
                }
                // Paragraphs have a blank line between them, which a quote's keep in the quote.
                if self.pending_breaks > 1 {
                    if self.quotes > 0 && self.quote_has_text {
                        self.push(QUOTE_MARKER, Piece::Marker(look.align));
                        self.line_marker_len = QUOTE_MARKER.len();
                    }
                    self.end_line(look.align);
                }
            }
            self.pending_breaks = 0;
            self.pending_space = false;
        }
        if !self.line_has_text {
            if let Some(marker) = self.marker.take() {
                self.push(&marker, Piece::Marker(look.align));
                self.line_marker_len = marker.len();
            } else if self.quotes > 0 && !self.text_line_has_marker() {
                self.push(QUOTE_MARKER, Piece::Marker(look.align));
                self.line_marker_len = QUOTE_MARKER.len();
            }
            self.pending_space = false;
        }
        if self.pending_space {
            self.push(" ", Piece::Text(look.clone()));
            self.pending_space = false;
        }
        self.push(text, Piece::Text(look.clone()));
        self.line_has_text = true;
        if self.quotes > 0 {
            self.quote_has_text = true;
        }
    }

    /// Whether the line being written already starts with a quote marker.
    fn text_line_has_marker(&self) -> bool {
        let line_start = self.text.rfind('\n').map_or(0, |index| index + 1);
        self.text[line_start..].starts_with(QUOTE_MARKER)
    }

    fn line_break(&mut self, look: &Look) {
        if self.pending_breaks > 0 && self.line_has_text {
            self.end_line(look.align);
        }
        self.pending_breaks = 0;
        if self.text.is_empty() {
            return;
        }
        self.end_line(look.align);
    }

    fn end_line(&mut self, align: Alignment) {
        // A line ends without spaces, and one of only spaces is empty.
        let line_start = self.text.rfind('\n').map_or(0, |index| index + 1);
        let content_start = (line_start + self.line_marker_len).min(self.text.len());
        let kept = content_start + self.text[content_start..].trim_end().len();
        if kept < self.text.len() {
            self.text.truncate(kept);
            self.pieces.retain_mut(|(range, _)| {
                range.end = range.end.min(kept);
                range.start < range.end
            });
        }
        self.line_marker_len = 0;
        self.push("\n", Piece::LineEnd(align));
        self.line_has_text = false;
        self.pending_space = false;
    }

    fn push(&mut self, text: &str, piece: Piece) {
        let start = self.text.len();
        self.text.push_str(text);
        let end = self.text.len();
        match self.pieces.last_mut() {
            Some((range, last)) if *last == piece && range.end == start => range.end = end,
            _ => self.pieces.push((start..end, piece)),
        }
    }

    fn finish(mut self, base: &CharacterStyle) -> Option<(String, Vec<ComposeStyleRun>)> {
        // A copy ends without the line break after its last line.
        let kept = self.text.trim_end().len();
        self.text.truncate(kept);
        self.pieces.retain_mut(|(range, _)| {
            range.end = range.end.min(kept);
            range.start < range.end
        });
        if self.text.trim().is_empty() {
            return None;
        }

        // What most of the text looks like is what it looked like where it came from.
        let mut families = Tally::default();
        let mut colors = Tally::default();
        for (range, piece) in &self.pieces {
            if let Piece::Text(look) = piece {
                let count = self.text[range.clone()]
                    .chars()
                    .filter(|character| !character.is_whitespace())
                    .count();
                families.add(look.family, count);
                colors.add(look.color, count);
            }
        }
        let usual_family = families.usual().flatten();
        let usual_color = colors.usual().flatten().filter(|color| is_neutral(*color));

        let base_size = TextSize::ALL
            .iter()
            .position(|size| *size == base.size)
            .unwrap_or(1);
        let styles = self
            .pieces
            .into_iter()
            .map(|(range, piece)| {
                let style = match piece {
                    Piece::Marker(align) => CharacterStyle {
                        align,
                        ..CharacterStyle::default()
                    },
                    Piece::LineEnd(align) => CharacterStyle {
                        align,
                        ..base.clone()
                    },
                    Piece::Text(look) => {
                        let highlight = look.highlight.filter(|highlight| {
                            let luminance = luminance(*highlight);
                            // Page and card backgrounds, light or dark, aren't highlights.
                            !(is_neutral(*highlight) && (luminance > 0.85 || luminance < 0.2))
                        });
                        let color = look.color.filter(|color| {
                            let luminance = luminance(*color);
                            Some(*color) != usual_color
                                // Text as dark as usual text takes the reader's own color.
                                && !(luminance < 0.05 && is_neutral(*color))
                                // Text nearly as light as the page vanishes on it, unless it's on a
                                // highlight that stays.
                                && !(luminance > 0.75 && highlight.is_none())
                        });
                        // Text goes as many sizes up or down from the message's own as it was
                        // from normal text: body text in documents and pages is normal, headings
                        // larger and fine print smaller.
                        let step: isize = if look.px <= 13.5 {
                            -1
                        } else if look.px < 21.0 {
                            0
                        } else if look.px < 28.0 {
                            1
                        } else {
                            2
                        };
                        let size = TextSize::ALL[base_size.saturating_add_signed(step).min(3)];
                        let family = match look.family {
                            Some(family) if Some(family) != usual_family => family,
                            _ => base.family,
                        };
                        CharacterStyle {
                            marks: look.marks,
                            link: look.link,
                            link_is_auto: false,
                            color,
                            highlight,
                            size,
                            family,
                            align: look.align,
                        }
                    }
                };
                ComposeStyleRun { range, style }
            })
            .collect();
        Some((self.text, styles))
    }
}

/// How much of the text has each value.
struct Tally<T>(Vec<(T, usize)>);

impl<T> Default for Tally<T> {
    fn default() -> Self {
        Self(Vec::new())
    }
}

impl<T: PartialEq + Copy> Tally<T> {
    fn add(&mut self, value: T, count: usize) {
        match self.0.iter_mut().find(|(seen, _)| *seen == value) {
            Some((_, total)) => *total += count,
            None => self.0.push((value, count)),
        }
    }

    /// The value most of the text has, if it's clearly most: what the text looked like where it
    /// came from, rather than a style picked for a part of it.
    fn usual(&self) -> Option<T> {
        let total: usize = self.0.iter().map(|(_, count)| *count).sum();
        self.0
            .iter()
            .find(|(_, count)| *count * 10 >= total * 7)
            .map(|(value, _)| *value)
    }
}

/// Applies a `style` attribute's declarations the composer has a style for. Returns whether
/// it takes the vertical margins off, and sets `list_item` for Word's list paragraphs.
fn apply_css(style: &str, look: &mut Look, parent_px: f32, list_item: &mut bool) -> bool {
    let mut margin_top = None;
    let mut margin_bottom = None;
    for declaration in style.split(';') {
        let Some((property, value)) = declaration.split_once(':') else {
            continue;
        };
        let property = property.trim().to_ascii_lowercase();
        let value = value
            .trim()
            .trim_end_matches("!important")
            .trim()
            .to_ascii_lowercase();
        match property.as_str() {
            "font-weight" => {
                let bold = match value.as_str() {
                    "bold" | "bolder" => true,
                    "normal" | "lighter" => false,
                    number => match number.parse::<f32>() {
                        Ok(weight) => weight >= 600.0,
                        Err(_) => continue,
                    },
                };
                set_mark(look, BOLD, bold);
            }
            "font-style" => set_mark(
                look,
                ITALIC,
                value.starts_with("italic") || value.starts_with("oblique"),
            ),
            "text-decoration" | "text-decoration-line" => {
                if value.contains("none") {
                    set_mark(look, UNDERLINE | STRIKE, false);
                } else {
                    if value.contains("underline") {
                        look.marks |= UNDERLINE;
                    }
                    if value.contains("line-through") {
                        look.marks |= STRIKE;
                    }
                }
            }
            "color" => {
                if let Some(color) = parse_color(&value) {
                    look.color = color;
                }
            }
            "background-color" | "background" => {
                if let Some(color) = value.split_whitespace().find_map(parse_color) {
                    look.highlight = color;
                }
            }
            "font-size" => {
                if let Some(px) = css_size_px(&value, parent_px) {
                    look.px = px;
                }
            }
            "font-family" => look.family = Some(classify_family(&value)),
            "font" => {
                // The shorthand's family comes last, after its size.
                if let Some(px) = value
                    .split_whitespace()
                    .find_map(|part| css_size_px(part.split('/').next()?, parent_px))
                {
                    look.px = px;
                }
            }
            "text-align" => {
                if let Some(align) = parse_alignment(&value) {
                    look.align = align;
                }
            }
            "display" if value == "none" => look.hidden = true,
            "visibility" if value == "hidden" || value == "collapse" => look.hidden = true,
            "mso-hide" if value == "all" => look.hidden = true,
            "mso-list" if value == "ignore" => look.hidden = true,
            "mso-list" if value != "none" => *list_item = true,
            "white-space" => {
                look.preformatted = matches!(
                    value.as_str(),
                    "pre" | "pre-wrap" | "pre-line" | "break-spaces"
                );
            }
            "margin" => {
                let zero = value.split_whitespace().all(is_zero_length);
                margin_top = Some(zero);
                margin_bottom = Some(zero);
            }
            "margin-top" => margin_top = Some(is_zero_length(&value)),
            "margin-bottom" => margin_bottom = Some(is_zero_length(&value)),
            _ => {}
        }
    }
    margin_top == Some(true) && margin_bottom == Some(true)
}

fn set_mark(look: &mut Look, mark: u8, on: bool) {
    if on {
        look.marks |= mark;
    } else {
        look.marks &= !mark;
    }
}

fn is_zero_length(value: &str) -> bool {
    value
        .trim_end_matches(|character: char| character.is_ascii_alphabetic() || character == '%')
        .parse::<f32>()
        .is_ok_and(|number| number == 0.0)
}

fn parse_alignment(value: &str) -> Option<Alignment> {
    match value.trim().to_ascii_lowercase().as_str() {
        "left" | "start" | "justify" => Some(Alignment::Left),
        "center" | "middle" => Some(Alignment::Center),
        "right" | "end" => Some(Alignment::Right),
        _ => None,
    }
}

/// A CSS color, a translucent one as it shows on a white page: `Some(None)` for a transparent
/// one, `None` when it isn't a color.
fn parse_color(value: &str) -> Option<Option<Rgb>> {
    let value = value.trim().to_ascii_lowercase();
    let rgb = |red: u8, green: u8, blue: u8, alpha: f32| {
        let over_white =
            |channel: u8| (f32::from(channel) * alpha + 255.0 * (1.0 - alpha)).round() as Rgb;
        (alpha > 0.0).then(|| (over_white(red) << 16) | (over_white(green) << 8) | over_white(blue))
    };
    if let Some(hex) = value.strip_prefix('#') {
        let (red, green, blue, alpha) = cssparser::color::parse_hash_color(hex.as_bytes()).ok()?;
        return Some(rgb(red, green, blue, alpha.clamp(0.0, 1.0)));
    }
    if let Some(arguments) = value
        .strip_prefix("rgba(")
        .or_else(|| value.strip_prefix("rgb("))
        .and_then(|rest| rest.strip_suffix(')'))
    {
        let parts: Vec<&str> = arguments
            .split([',', ' ', '/'])
            .filter(|part| !part.is_empty())
            .collect();
        let channel = |part: &str| -> Option<u8> {
            let number = match part.strip_suffix('%') {
                Some(percent) => percent.parse::<f32>().ok()? * 2.55,
                None => part.parse::<f32>().ok()?,
            };
            Some(number.round().clamp(0.0, 255.0) as u8)
        };
        let [red, green, blue] = [
            channel(parts.first()?)?,
            channel(parts.get(1)?)?,
            channel(parts.get(2)?)?,
        ];
        let alpha = match parts.get(3) {
            Some(alpha) => match alpha.strip_suffix('%') {
                Some(percent) => percent.parse::<f32>().ok()? / 100.0,
                None => alpha.parse::<f32>().ok()?,
            },
            None => 1.0,
        };
        return Some(rgb(red, green, blue, alpha.clamp(0.0, 1.0)));
    }
    match value.as_str() {
        "transparent" => Some(None),
        // Word's name for the reader's text color.
        "windowtext" => Some(Some(0)),
        name => {
            let (red, green, blue) = cssparser::color::parse_named_color(name).ok()?;
            Some(rgb(red, green, blue, 1.0))
        }
    }
}

/// `<font size>`: 1 to 7, or steps from 3.
fn font_size_px(value: &str) -> Option<f32> {
    let value = value.trim();
    let size = match value.as_bytes().first()? {
        b'+' => 3 + value[1..].parse::<i32>().ok()?,
        b'-' => 3 - value[1..].parse::<i32>().ok()?,
        _ => value.parse::<i32>().ok()?,
    };
    Some(match size.clamp(1, 7) {
        1 => 10.0,
        2 => 13.0,
        3 => 16.0,
        4 => 18.0,
        5 => 24.0,
        6 => 32.0,
        _ => 48.0,
    })
}

/// A CSS `font-size` in pixels, for text inside text `parent_px` pixels high.
fn css_size_px(value: &str, parent_px: f32) -> Option<f32> {
    let value = value.trim();
    let keyword = match value {
        "xx-small" => Some(9.0),
        "x-small" => Some(10.0),
        "small" => Some(13.0),
        "medium" => Some(NORMAL_PX),
        "large" => Some(18.0),
        "x-large" => Some(24.0),
        "xx-large" => Some(32.0),
        "xxx-large" => Some(48.0),
        "smaller" => Some(parent_px / 1.2),
        "larger" => Some(parent_px * 1.2),
        _ => None,
    };
    if keyword.is_some() {
        return keyword;
    }
    let number_end = value
        .find(|character: char| !(character.is_ascii_digit() || character == '.'))
        .unwrap_or(value.len());
    let number: f32 = value[..number_end].parse().ok()?;
    let px = match &value[number_end..] {
        "px" => number,
        "pt" => number * 4.0 / 3.0,
        "em" => number * parent_px,
        "rem" => number * NORMAL_PX,
        "%" => number * parent_px / 100.0,
        _ => return None,
    };
    (px > 0.0).then_some(px)
}

/// Sorts a font list by its first font: serif, monospace, or else the reader's own.
fn classify_family(value: &str) -> FontFamily {
    let first = value
        .split(',')
        .next()
        .unwrap_or_default()
        .trim()
        .trim_matches(['"', '\''])
        .to_ascii_lowercase();
    const MONOSPACE: [&str; 17] = [
        "monospace",
        "courier",
        "consolas",
        "menlo",
        "monaco",
        "lucida console",
        "source code",
        "fira code",
        "fira mono",
        "roboto mono",
        "sf mono",
        "ubuntu mono",
        "cascadia",
        "jetbrains mono",
        "andale mono",
        "dejavu sans mono",
        "liberation mono",
    ];
    const SERIF: [&str; 16] = [
        "serif",
        "times",
        "georgia",
        "garamond",
        "cambria",
        "palatino",
        "book antiqua",
        "baskerville",
        "didot",
        "bodoni",
        "constantia",
        "century schoolbook",
        "liberation serif",
        "noto serif",
        "dejavu serif",
        "merriweather",
    ];
    if MONOSPACE.iter().any(|name| first.contains(name))
        || (first.contains("mono") && !first.contains("monotype"))
    {
        FontFamily::Monospace
    } else if !first.contains("sans")
        && (first
            .split(|character: char| !character.is_alphanumeric())
            .any(|word| word == "serif")
            || SERIF
                .iter()
                .any(|name| first == *name || first.starts_with(&format!("{name} "))))
    {
        FontFamily::Serif
    } else {
        FontFamily::Default
    }
}

fn luminance(color: Rgb) -> f32 {
    let channel = |shift: u32| {
        let value = ((color >> shift) & 0xff) as f32 / 255.0;
        if value <= 0.040_45 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
}

/// How far from gray a color is, from 0 to 1: the spread of its channels.
fn chroma(color: Rgb) -> f32 {
    let channels = [(color >> 16) & 0xff, (color >> 8) & 0xff, color & 0xff];
    let high = channels.iter().copied().max().unwrap_or(0);
    let low = channels.iter().copied().min().unwrap_or(0);
    (high - low) as f32 / 255.0
}

/// Grays, and colors too close to gray to tell apart from them.
fn is_neutral(color: Rgb) -> bool {
    chroma(color) < 0.15
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pastes `html` into a message in the plain default style.
    fn paste(html: &str) -> (String, Vec<ComposeStyleRun>) {
        styled_text(html, &CharacterStyle::default()).expect("text to paste")
    }

    /// The style of `needle` in pasted text, which all of it must share.
    fn style<'a>(pasted: &'a (String, Vec<ComposeStyleRun>), needle: &str) -> &'a CharacterStyle {
        let (text, runs) = pasted;
        let start = text
            .find(needle)
            .unwrap_or_else(|| panic!("{needle:?} not in {text:?}"));
        let end = start + needle.len();
        let covering: Vec<&ComposeStyleRun> = runs
            .iter()
            .filter(|run| run.range.start < end && run.range.end > start)
            .collect();
        assert!(
            covering.iter().all(|run| run.style == covering[0].style),
            "{needle:?} has several styles: {covering:?}"
        );
        &covering[0].style
    }

    #[test]
    fn a_web_page_keeps_its_emphasis_and_drops_its_page_style() {
        let pasted = paste(concat!(
            "<html><body>\n<!--StartFragment-->",
            "<h2 style=\"color: rgb(31, 35, 40); font-family: -apple-system, &quot;Segoe UI&quot;, sans-serif; font-size: 24px;\">Release notes</h2>",
            "<p style=\"color: rgb(31, 35, 40); font-family: -apple-system, sans-serif; font-size: 16px;\">This   release <strong>fixes</strong> the ",
            "<a href=\"https://example.com/issue\">sync bug</a> and adds ",
            "<code style=\"font-family: ui-monospace, monospace; background-color: rgba(129, 139, 152, 0.12);\">--verbose</code>.</p>",
            "<ul><li>Faster start</li><li>Smaller downloads</li></ul>",
            "<!--EndFragment-->\n</body></html>",
        ));
        assert_eq!(
            pasted.0,
            "Release notes\n\nThis release fixes the sync bug and adds --verbose.\n\n• Faster start\n• Smaller downloads"
        );
        let heading = style(&pasted, "Release notes");
        assert_eq!(
            (heading.marks, heading.size, heading.color),
            (BOLD, TextSize::Large, None)
        );
        assert_eq!(style(&pasted, "This release").size, TextSize::Normal);
        assert_eq!(style(&pasted, "This release").color, None);
        assert_eq!(style(&pasted, "fixes").marks, BOLD);
        assert_eq!(
            style(&pasted, "sync bug").link.as_deref(),
            Some("https://example.com/issue")
        );
        // A faint chip behind code is the page's, and code stays code.
        let code = style(&pasted, "--verbose");
        assert_eq!((code.marks, code.highlight), (CODE, None));
        assert_eq!(style(&pasted, "• ").marks, 0);
    }

    #[test]
    fn document_paragraphs_are_lines_and_their_own_weight_wins() {
        let span = |style: &str, text: &str| {
            format!(
                "<span style=\"font-size:11pt;font-family:Arial,sans-serif;color:#000000;background-color:transparent;font-weight:400;font-style:normal;text-decoration:none;white-space:pre;white-space:pre-wrap;{style}\">{text}</span>"
            )
        };
        let paragraph = |content: String| {
            format!(
                "<p dir=\"ltr\" style=\"line-height:1.38;margin-top:0pt;margin-bottom:0pt;\">{content}</p>"
            )
        };
        let html = format!(
            "<meta charset=\"utf-8\"><b style=\"font-weight:normal;\" id=\"docs-internal-guid-1\">{}{}</b>",
            paragraph(
                span("", "Plain and ")
                    + &span(
                        "background-color:#ffff00;font-weight:700;",
                        "bold highlighted"
                    )
            ),
            paragraph(span("color:#ff0000;", "Red line")),
        );
        let pasted = paste(&html);
        assert_eq!(pasted.0, "Plain and bold highlighted\nRed line");
        assert_eq!(style(&pasted, "Plain and ").marks, 0);
        let highlighted = style(&pasted, "bold highlighted");
        assert_eq!(
            (highlighted.marks, highlighted.highlight),
            (BOLD, Some(0xff_ff_00))
        );
        assert_eq!(style(&pasted, "Red line").color, Some(0xff_00_00));
        // Arial is the document's own font: the message's takes its place.
        assert_eq!(style(&pasted, "Red line").family, FontFamily::Default);
    }

    #[test]
    fn word_lists_become_lists_without_their_text_bullets() {
        let item = |text: &str| {
            format!(
                "<p class=MsoListParagraphCxSpMiddle style='text-indent:-18.0pt;mso-list:l0 level1 lfo1'><![if !supportLists]><span style='font-family:Symbol'><span style='mso-list:Ignore'>·<span style='font:7.0pt \"Times New Roman\"'>&nbsp;&nbsp;&nbsp; </span></span></span><![endif]>{text}<o:p></o:p></p>\r\n"
            )
        };
        let html = format!(
            "<p class=MsoNormal><b><span style='font-size:14.0pt;color:#2F5496'>Agenda<o:p></o:p></span></b></p>\r\n{}{}",
            item("Budget"),
            item("Travel"),
        );
        let pasted = paste(&html);
        assert_eq!(pasted.0, "Agenda\n\n• Budget\n• Travel");
        let agenda = style(&pasted, "Agenda");
        assert_eq!((agenda.marks, agenda.color), (BOLD, Some(0x2f_54_96)));
    }

    #[test]
    fn outlook_paragraphs_are_lines_and_its_empty_ones_blank_lines() {
        let paragraph = |text: &str| format!("<p class=MsoNormal>{text}<o:p></o:p></p>\r\n");
        let html = [
            paragraph("Hi Jamie,"),
            "<p class=MsoNormal><o:p>&nbsp;</o:p></p>\r\n".to_owned(),
            paragraph("The venue is booked.&nbsp;"),
            paragraph("Thanks,"),
        ]
        .concat();
        assert_eq!(paste(&html).0, "Hi Jamie,\n\nThe venue is booked.\nThanks,");
    }

    #[test]
    fn sizes_go_by_the_usual_text_and_the_message_default() {
        let html = concat!(
            "<p style=\"font-size:26pt;margin:0\">Title</p>",
            "<p style=\"font-size:18pt;margin:0\">Subtitle</p>",
            "<p style=\"font-size:11pt;margin:0\">Body text that is most of what was copied.</p>",
            "<p style=\"font-size:8pt;margin:0\">Fine print</p>",
        );
        let pasted = paste(html);
        assert_eq!(style(&pasted, "Title").size, TextSize::Huge);
        assert_eq!(style(&pasted, "Subtitle").size, TextSize::Large);
        assert_eq!(style(&pasted, "Body text").size, TextSize::Normal);
        assert_eq!(style(&pasted, "Fine print").size, TextSize::Small);

        // In a message that starts large, the body text is large too, and steps go from there.
        let base = CharacterStyle {
            size: TextSize::Large,
            family: FontFamily::Serif,
            ..CharacterStyle::default()
        };
        let pasted = styled_text(html, &base).unwrap();
        assert_eq!(style(&pasted, "Body text").size, TextSize::Large);
        assert_eq!(style(&pasted, "Body text").family, FontFamily::Serif);
        assert_eq!(style(&pasted, "Fine print").size, TextSize::Normal);
        assert_eq!(style(&pasted, "Subtitle").size, TextSize::Huge);
        assert_eq!(style(&pasted, "\n").size, TextSize::Large);
    }

    #[test]
    fn a_short_copy_keeps_the_style_it_was_copied_for() {
        let pasted = paste(
            "<div dir=\"ltr\"><font face=\"georgia, serif\" size=\"6\" color=\"#0b5394\">Styled</font> plain</div>",
        );
        assert_eq!(pasted.0, "Styled plain");
        let styled = style(&pasted, "Styled");
        assert_eq!(
            (styled.family, styled.size, styled.color),
            (FontFamily::Serif, TextSize::Huge, Some(0x0b_53_94))
        );
        assert_eq!(style(&pasted, "plain").family, FontFamily::Default);
    }

    #[test]
    fn tables_quotes_preformatted_text_and_alignment() {
        let pasted = paste(concat!(
            "<table><tr><th>Item</th><th>Cost</th></tr><tr><td>Hotel</td><td>120</td></tr></table>",
            "<blockquote><p>First said</p><p>Then said</p></blockquote>",
            "<pre>line one\n  indented</pre>",
            "<p style=\"text-align:center\">Centered</p>",
            "<ol start=\"3\"><li>third</li><li>fourth</li></ol>",
        ));
        assert_eq!(
            pasted.0,
            "Item\tCost\nHotel\t120\n\n│ First said\n│ \n│ Then said\n\nline one\n  indented\n\nCentered\n\n3. third\n4. fourth"
        );
        assert_eq!(style(&pasted, "Item").marks, BOLD);
        assert_eq!(style(&pasted, "  indented").marks, CODE);
        assert_eq!(style(&pasted, "Centered").align, Alignment::Center);
    }

    #[test]
    fn fonts_go_by_kind() {
        for (fonts, family) in [
            ("SFMono-Regular, Consolas, monospace", FontFamily::Monospace),
            ("\"Courier New\", Courier", FontFamily::Monospace),
            ("ui-monospace", FontFamily::Monospace),
            ("Georgia, serif", FontFamily::Serif),
            ("'Times New Roman'", FontFamily::Serif),
            ("Noto Serif Display", FontFamily::Serif),
            ("Noto Sans, sans-serif", FontFamily::Default),
            ("PT Serif Caption", FontFamily::Serif),
            ("-apple-system, BlinkMacSystemFont", FontFamily::Default),
            ("Monotype Corsiva", FontFamily::Default),
        ] {
            assert_eq!(classify_family(fonts), family, "{fonts}");
        }
    }

    #[test]
    fn hidden_text_scripts_and_unsafe_links_stay_out() {
        let pasted = paste(concat!(
            "<head><title>Page</title><style>p { color: red }</style></head>",
            "<script>alert('x')</script>",
            "<div style=\"display:none\">secret</div>",
            "<span hidden>also secret</span>",
            "<a href=\"javascript:alert(1)\">click</a> and ",
            "<a href=\"/relative\">here</a> and ",
            "<a href=\"MAILTO:team@example.com\">mail</a>",
            "<img src=\"https://example.com/a.png\" alt=\"picture\">",
        ));
        assert_eq!(pasted.0, "click and here and mail");
        assert_eq!(style(&pasted, "click").link, None);
        assert_eq!(style(&pasted, "here").link, None);
        assert_eq!(
            style(&pasted, "mail").link.as_deref(),
            Some("MAILTO:team@example.com")
        );
    }

    #[test]
    fn colors_that_would_vanish_on_a_light_page_go() {
        // A dark page: its background and light text aren't the message's.
        let pasted = paste(
            "<div style=\"background-color:#0d1117;color:#e6edf3\">Dark page <span style=\"color:#ff7b72\">keyword</span> text that fills the page</div>",
        );
        assert_eq!(style(&pasted, "Dark page").highlight, None);
        assert_eq!(style(&pasted, "Dark page").color, None);
        assert_eq!(style(&pasted, "keyword").color, Some(0xff_7b_72));

        // White text on a dark highlight that stays keeps its color.
        let pasted = paste(
            "<span style=\"background-color:#0b5394;color:#ffffff\">Label</span> and the rest of the sentence",
        );
        let label = style(&pasted, "Label");
        assert_eq!(
            (label.highlight, label.color),
            (Some(0x0b_53_94), Some(0xff_ff_ff))
        );

        // A light tint is a highlight; a page's near-white card isn't.
        let pasted = paste(
            "<span style=\"background:#fff2cc\">tinted</span> <span style=\"background-color:#f6f8fa\">card</span>",
        );
        assert_eq!(style(&pasted, "tinted").highlight, Some(0xff_f2_cc));
        assert_eq!(style(&pasted, "card").highlight, None);
    }

    #[test]
    fn nothing_to_paste_without_text() {
        let base = CharacterStyle::default();
        for html in [
            "",
            "   ",
            "<img src=\"https://example.com/a.png\">",
            "<p>&nbsp;</p>",
            "<br><br>",
        ] {
            assert!(styled_text(html, &base).is_none(), "{html:?}");
        }
    }
}
