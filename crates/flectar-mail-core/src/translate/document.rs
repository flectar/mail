//! A message in pieces a translation service can take, and the message put
//! back together from what comes back (#240).
//!
//! Each run of text in the message, such as a paragraph or a table cell, is
//! one piece. Links, bold text and other inline elements in a run travel as
//! numbered tags, so the service keeps them with the words they wrap. What
//! the service returns is never taken as markup. The translated message is
//! built from the original elements, attributes and all, and only text comes
//! from the service, as Firefox does when it translates a page
//! (`toolkit/components/translations/content/translations-document.sys.mjs`,
//! where `merge` re-attaches the page's own elements and copies text only).
//!
//! A service that can't keep tags, or a reply whose tags don't match, gets
//! the run's text nodes one by one instead. Image descriptions (`alt`) and
//! link titles are pieces of their own, as Firefox translates those too and
//! the reader shows `alt` in place of a blocked image.

use super::{Piece, Slot};
use html5ever::tokenizer::states::RawKind;
use html5ever::tokenizer::{BufferQueue, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer};
use std::cell::RefCell;
use std::collections::BTreeMap;

/// Elements nested deeper than this, or more nodes than [`MAX_NODES`], make
/// a message too complex to translate. The reader's own limits are lower.
const MAX_DEPTH: usize = 256;
const MAX_NODES: usize = 200_000;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Node {
    Element(Element),
    Text(String),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Element {
    pub(crate) name: String,
    pub(crate) attrs: Vec<(String, String)>,
    pub(crate) children: Vec<Node>,
}

impl Element {
    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    fn set_attr(&mut self, name: &str, value: String) {
        match self.attrs.iter_mut().find(|(key, _)| key == name) {
            Some((_, old)) => *old = value,
            None => self.attrs.push((name.to_owned(), value)),
        }
    }

    fn has_class(&self, class: &str) -> bool {
        self.attr("class")
            .is_some_and(|classes| classes.split_ascii_whitespace().any(|c| c == class))
    }

    /// A copy with the attributes and none of the children.
    fn shell(&self) -> Element {
        Element {
            name: self.name.clone(),
            attrs: self.attrs.clone(),
            children: Vec::new(),
        }
    }
}

fn is_void(name: &str) -> bool {
    matches!(
        name,
        "area"
            | "base"
            | "basefont"
            | "bgsound"
            | "br"
            | "col"
            | "embed"
            | "frame"
            | "hr"
            | "img"
            | "input"
            | "keygen"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}

/// Elements whose text the tokenizer reads as it stands, so it's written
/// back as it stands.
fn is_raw_text(name: &str) -> bool {
    matches!(
        name,
        "style" | "script" | "xmp" | "iframe" | "noembed" | "noframes" | "noscript" | "plaintext"
    )
}

/// Elements that sit inside a line of text rather than starting a block of
/// their own (HTML phrasing content, plus the old presentational ones mail
/// still uses).
fn is_inline(name: &str) -> bool {
    matches!(
        name,
        "a" | "abbr"
            | "acronym"
            | "b"
            | "bdi"
            | "bdo"
            | "big"
            | "br"
            | "cite"
            | "code"
            | "data"
            | "del"
            | "dfn"
            | "em"
            | "font"
            | "i"
            | "img"
            | "ins"
            | "kbd"
            | "mark"
            | "nobr"
            | "q"
            | "s"
            | "samp"
            | "small"
            | "span"
            | "strike"
            | "strong"
            | "sub"
            | "sup"
            | "time"
            | "tt"
            | "u"
            | "var"
            | "wbr"
    )
}

/// Elements whose content is never translated: code and its kin, as in
/// Firefox's `CONTENT_EXCLUDED_TAGS`, what isn't shown, and what the sender
/// marked with the `notranslate` class.
fn is_untranslated(element: &Element) -> bool {
    matches!(
        element.name.as_str(),
        "code"
            | "kbd"
            | "samp"
            | "var"
            | "acronym"
            | "head"
            | "title"
            | "style"
            | "script"
            | "template"
            | "textarea"
            | "noscript"
            | "svg"
            | "math"
    ) || element.has_class("notranslate")
}

/// Whether text has something to translate, rather than only numbers,
/// punctuation and space.
pub(crate) fn has_words(text: &str) -> bool {
    text.chars().any(char::is_alphabetic)
}

// ---- Parsing --------------------------------------------------------------

struct Builder {
    /// Open elements. The first is the document itself.
    stack: Vec<Element>,
    nodes: usize,
    too_complex: bool,
}

impl Builder {
    fn push_node(&mut self, node: Node) {
        self.nodes += 1;
        if self.nodes > MAX_NODES {
            self.too_complex = true;
            return;
        }
        let parent = self.stack.last_mut().expect("the document stays open");
        match (parent.children.last_mut(), node) {
            (Some(Node::Text(last)), Node::Text(text)) => last.push_str(&text),
            (_, node) => parent.children.push(node),
        }
    }

    fn close_to(&mut self, depth: usize) {
        while self.stack.len() > depth.max(1) {
            let element = self.stack.pop().expect("deeper than the document");
            self.stack
                .last_mut()
                .expect("the document stays open")
                .children
                .push(Node::Element(element));
        }
    }
}

struct Sink(RefCell<Builder>);

impl TokenSink for Sink {
    type Handle = ();

    fn process_token(&self, token: Token, _: u64) -> TokenSinkResult<()> {
        let mut builder = self.0.borrow_mut();
        if builder.too_complex {
            return TokenSinkResult::Continue;
        }
        match token {
            Token::TagToken(tag) if tag.kind == TagKind::StartTag => {
                let name = tag.name.to_string();
                let element = Element {
                    attrs: tag
                        .attrs
                        .iter()
                        .map(|attr| (attr.name.local.to_string(), attr.value.to_string()))
                        .collect(),
                    name,
                    children: Vec::new(),
                };
                if is_void(&element.name) || tag.self_closing {
                    builder.push_node(Node::Element(element));
                    return TokenSinkResult::Continue;
                }
                if builder.stack.len() > MAX_DEPTH {
                    builder.too_complex = true;
                    return TokenSinkResult::Continue;
                }
                builder.nodes += 1;
                let raw = match element.name.as_str() {
                    "title" | "textarea" => Some(RawKind::Rcdata),
                    "script" => Some(RawKind::ScriptData),
                    name if is_raw_text(name) => Some(RawKind::Rawtext),
                    _ => None,
                };
                builder.stack.push(element);
                if let Some(kind) = raw {
                    return TokenSinkResult::RawData(kind);
                }
            }
            Token::TagToken(tag) => {
                let name = tag.name.as_ref();
                if let Some(depth) = builder
                    .stack
                    .iter()
                    .skip(1)
                    .rposition(|element| element.name == name)
                {
                    builder.close_to(depth + 1);
                }
            }
            Token::CharacterTokens(text) => builder.push_node(Node::Text(text.to_string())),
            _ => {}
        }
        TokenSinkResult::Continue
    }
}

/// The message as a tree of elements and text. `None` when it's too complex
/// to translate.
pub(crate) fn parse(html: &str) -> Option<Element> {
    let sink = Sink(RefCell::new(Builder {
        stack: vec![Element {
            name: String::new(),
            attrs: Vec::new(),
            children: Vec::new(),
        }],
        nodes: 0,
        too_complex: false,
    }));
    let tokenizer = Tokenizer::new(sink, Default::default());
    let input = BufferQueue::default();
    input.push_back(html.into());
    let _ = tokenizer.feed(&input);
    tokenizer.end();
    let mut builder = tokenizer.sink.0.into_inner();
    if builder.too_complex {
        return None;
    }
    builder.close_to(1);
    builder.stack.pop()
}

// ---- Writing --------------------------------------------------------------

fn escape_text(text: &str, out: &mut String) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\u{a0}' => out.push_str("&nbsp;"),
            c => out.push(c),
        }
    }
}

fn escape_attr(text: &str, out: &mut String) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '\u{a0}' => out.push_str("&nbsp;"),
            c => out.push(c),
        }
    }
}

fn write_children(children: &[Node], raw: bool, out: &mut String) {
    for child in children {
        match child {
            Node::Text(text) if raw => out.push_str(text),
            Node::Text(text) => escape_text(text, out),
            Node::Element(element) => write_element(element, out),
        }
    }
}

fn write_element(element: &Element, out: &mut String) {
    out.push('<');
    out.push_str(&element.name);
    for (name, value) in &element.attrs {
        out.push(' ');
        out.push_str(name);
        out.push_str("=\"");
        escape_attr(value, out);
        out.push('"');
    }
    out.push('>');
    if is_void(&element.name) {
        return;
    }
    write_children(&element.children, is_raw_text(&element.name), out);
    out.push_str("</");
    out.push_str(&element.name);
    out.push('>');
}

/// The tree written back out as HTML, as html5ever's serializer writes it.
pub(crate) fn write(document: &Element) -> String {
    let mut out = String::new();
    write_children(&document.children, false, &mut out);
    out
}

// ---- Runs -----------------------------------------------------------------

/// Whether a node belongs in a run of inline content.
fn inline_node(node: &Node) -> bool {
    match node {
        Node::Text(_) => true,
        Node::Element(element) => {
            is_inline(&element.name)
                && (is_untranslated(element) || element.children.iter().all(inline_node))
        }
    }
}

/// Whether a run has words to translate, outside what's never translated.
fn run_has_words(nodes: &[Node]) -> bool {
    nodes.iter().any(|node| match node {
        Node::Text(text) => has_words(text),
        Node::Element(element) => !is_untranslated(element) && run_has_words(&element.children),
    })
}

/// The runs of inline content among an element's children, as index ranges:
/// a paragraph's whole content, or the text between two blocks.
fn runs(children: &[Node]) -> Vec<std::ops::Range<usize>> {
    let mut runs = Vec::new();
    let mut start = None;
    for (index, child) in children.iter().enumerate() {
        match (inline_node(child), start) {
            (true, None) => start = Some(index),
            (false, Some(from)) => {
                runs.push(from..index);
                start = None;
            }
            _ => {}
        }
    }
    if let Some(from) = start {
        runs.push(from..children.len());
    }
    runs.retain(|range| run_has_words(&children[range.clone()]));
    runs
}

/// What a visit does with a run: given its number, its nodes and whether
/// it's preformatted, the nodes to put in its place, or `None` to keep it.
type RunAct<'a> = dyn FnMut(usize, &[Node], bool) -> Option<Vec<Node>> + 'a;

/// Visits every run in document order, numbering them, and replaces a run
/// with what `act` returns for it.
fn visit_runs(element: &mut Element, pre: bool, next: &mut usize, act: &mut RunAct<'_>) {
    if is_untranslated(element) {
        return;
    }
    let pre = pre
        || matches!(
            element.name.as_str(),
            "pre" | "listing" | "xmp" | "plaintext"
        );
    let runs = runs(&element.children);
    let old = std::mem::take(&mut element.children);
    let mut children = Vec::with_capacity(old.len());
    let mut runs = runs.into_iter().peekable();
    let mut old = old.into_iter().enumerate().peekable();
    while let Some((index, mut child)) = old.next() {
        if let Some(range) = runs.next_if(|range| range.start == index) {
            let mut nodes = vec![child];
            while let Some((_, next_child)) = old.next_if(|(i, _)| *i < range.end) {
                nodes.push(next_child);
            }
            let id = *next;
            *next += 1;
            match act(id, &nodes, pre) {
                Some(replacement) if !replacement.is_empty() => children.extend(replacement),
                _ => children.extend(nodes),
            }
            continue;
        }
        if let Node::Element(inner) = &mut child {
            visit_runs(inner, pre, next, act);
        }
        children.push(child);
    }
    element.children = children;
}

/// Calls `act` with every element that has a description or title worth
/// translating, in document order.
fn visit_attributes(element: &mut Element, act: &mut dyn FnMut(&mut Element, &'static str)) {
    if is_untranslated(element) {
        return;
    }
    for name in ["alt", "title"] {
        let wanted = match name {
            "alt" => matches!(element.name.as_str(), "img" | "area"),
            _ => matches!(element.name.as_str(), "a" | "img" | "abbr" | "area"),
        };
        if wanted && element.attr(name).is_some_and(has_words) {
            act(element, name);
        }
    }
    for child in &mut element.children {
        if let Node::Element(inner) = child {
            visit_attributes(inner, act);
        }
    }
}

// ---- Encoding a run -------------------------------------------------------

/// What a numbered tag in a piece stands for.
#[derive(Clone, Debug)]
enum Part {
    /// `<gN>…</gN>`: an inline element whose text is translated with the rest.
    Wrap(Element),
    /// `<xN/>`: something kept as it is, such as an image, a line break or
    /// code.
    Keep(Node),
}

pub(super) fn escape_piece(text: &str, out: &mut String) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
}

/// HTML collapses runs of white space, so a piece does too.
fn push_collapsed(text: &str, out: &mut String) {
    let mut space = out.ends_with(' ');
    let mut chunk = String::new();
    for c in text.chars() {
        if c.is_whitespace() && c != '\u{a0}' {
            if !space {
                chunk.push(' ');
                space = true;
            }
        } else {
            chunk.push(c);
            space = false;
        }
    }
    escape_piece(&chunk, out);
}

fn encode_nodes(nodes: &[Node], pre: bool, parts: &mut Vec<Part>, out: &mut String) {
    for node in nodes {
        match node {
            Node::Text(text) if pre => {
                let mut lines = text.split('\n');
                if let Some(first) = lines.next() {
                    escape_piece(first, out);
                }
                for line in lines {
                    parts.push(Part::Keep(Node::Text("\n".to_owned())));
                    out.push_str(&format!("<x{}/>", parts.len()));
                    escape_piece(line, out);
                }
            }
            Node::Text(text) => push_collapsed(text, out),
            Node::Element(element) if is_void(&element.name) || is_untranslated(element) => {
                parts.push(Part::Keep(node.clone()));
                out.push_str(&format!("<x{}/>", parts.len()));
            }
            Node::Element(element) => {
                parts.push(Part::Wrap(element.shell()));
                let id = parts.len();
                out.push_str(&format!("<g{id}>"));
                encode_nodes(&element.children, pre, parts, out);
                out.push_str(&format!("</g{id}>"));
            }
        }
    }
}

struct Encoded {
    text: String,
    parts: Vec<Part>,
    /// The run started or ended with white space, which the piece leaves out
    /// and the translation gets back.
    lead: bool,
    trail: bool,
}

fn encode(nodes: &[Node], pre: bool) -> Encoded {
    let mut parts = Vec::new();
    let mut text = String::new();
    encode_nodes(nodes, pre, &mut parts, &mut text);
    let lead = text.starts_with(char::is_whitespace);
    let trail = text.ends_with(char::is_whitespace);
    Encoded {
        text: text.trim().to_owned(),
        parts,
        lead,
        trail,
    }
}

// ---- Decoding a translated run --------------------------------------------

pub(super) fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let end = rest[..rest.len().min(12)].find(';');
        let decoded = end.and_then(|end| {
            let entity = &rest[1..end];
            let c = match entity {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some('\u{a0}'),
                _ => entity
                    .strip_prefix("#x")
                    .or_else(|| entity.strip_prefix("#X"))
                    .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                    .or_else(|| entity.strip_prefix('#').and_then(|dec| dec.parse().ok()))
                    .and_then(char::from_u32),
            };
            c.map(|c| (c, end))
        });
        match decoded {
            Some((c, end)) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// One of the numbered tags at the start of `text`: `<gN>`, `</gN>` or
/// `<xN/>`, with the length it takes. Services sometimes add spaces inside
/// a tag, which is allowed.
fn tag_at(text: &str) -> Option<(char, bool, usize, usize)> {
    let bytes = text.as_bytes();
    let mut i = 1;
    let closing = bytes.get(i) == Some(&b'/');
    if closing {
        i += 1;
    }
    let kind = match bytes.get(i) {
        Some(b'g' | b'G') => 'g',
        Some(b'x' | b'X') => 'x',
        _ => return None,
    };
    i += 1;
    let digits = bytes[i..].iter().take_while(|b| b.is_ascii_digit()).count();
    if digits == 0 || digits > 6 {
        return None;
    }
    let id: usize = text[i..i + digits].parse().ok()?;
    i += digits;
    while bytes.get(i) == Some(&b' ') {
        i += 1;
    }
    let empty = bytes.get(i) == Some(&b'/');
    if empty {
        i += 1;
    }
    if bytes.get(i) != Some(&b'>') {
        return None;
    }
    match (kind, closing, empty) {
        ('g', _, false) | ('x', false, true) => Some((kind, closing, id, i + 1)),
        _ => None,
    }
}

/// The run rebuilt from a translated piece, or `None` when its tags don't
/// match the ones sent: one each, of the same kinds, properly nested.
fn decode(text: &str, parts: &[Part]) -> Option<Vec<Node>> {
    let mut used = vec![false; parts.len()];
    // Open wraps, each with its id and the nodes inside it so far.
    let mut stack: Vec<(usize, Vec<Node>)> = vec![(0, Vec::new())];
    let mut pending = String::new();
    let mut rest = text;
    let flush = |pending: &mut String, stack: &mut Vec<(usize, Vec<Node>)>| {
        if !pending.is_empty() {
            let text = unescape(pending);
            pending.clear();
            let nodes = &mut stack.last_mut().expect("the run stays open").1;
            match nodes.last_mut() {
                Some(Node::Text(last)) => last.push_str(&text),
                _ => nodes.push(Node::Text(text)),
            }
        }
    };
    while let Some(at) = rest.find('<') {
        pending.push_str(&rest[..at]);
        rest = &rest[at..];
        let Some((kind, closing, id, len)) = tag_at(rest) else {
            pending.push('<');
            rest = &rest[1..];
            continue;
        };
        rest = &rest[len..];
        flush(&mut pending, &mut stack);
        let index = id.checked_sub(1).filter(|index| *index < parts.len())?;
        match (kind, closing, &parts[index]) {
            ('x', _, Part::Keep(node)) => {
                if std::mem::replace(&mut used[index], true) {
                    return None;
                }
                stack.last_mut()?.1.push(node.clone());
            }
            ('g', false, Part::Wrap(_)) => {
                if std::mem::replace(&mut used[index], true) {
                    return None;
                }
                stack.push((id, Vec::new()));
            }
            ('g', true, Part::Wrap(shell)) => {
                let (open, children) = stack.pop()?;
                if open != id || stack.is_empty() {
                    return None;
                }
                let mut element = shell.clone();
                element.children = children;
                stack.last_mut()?.1.push(Node::Element(element));
            }
            _ => return None,
        }
    }
    pending.push_str(rest);
    flush(&mut pending, &mut stack);
    if stack.len() != 1 || used.iter().any(|used| !used) {
        return None;
    }
    stack.pop().map(|(_, nodes)| nodes)
}

/// Adds the white space a run started or ended with back around its
/// translation.
fn pad(mut nodes: Vec<Node>, lead: bool, trail: bool) -> Vec<Node> {
    if lead {
        match nodes.first_mut() {
            Some(Node::Text(text)) if !text.starts_with(char::is_whitespace) => text.insert(0, ' '),
            Some(Node::Text(_)) => {}
            _ => nodes.insert(0, Node::Text(" ".to_owned())),
        }
    }
    if trail {
        match nodes.last_mut() {
            Some(Node::Text(text)) if !text.ends_with(char::is_whitespace) => text.push(' '),
            Some(Node::Text(_)) => {}
            _ => nodes.push(Node::Text(" ".to_owned())),
        }
    }
    nodes
}

// ---- Text nodes one by one ------------------------------------------------

/// Calls `act` with each text node of a run that has words, in order,
/// leaving out what's never translated.
fn visit_text(nodes: &mut [Node], act: &mut dyn FnMut(&mut String)) {
    for node in nodes {
        match node {
            Node::Text(text) if has_words(text) => act(text),
            Node::Element(element) if !is_untranslated(element) => {
                visit_text(&mut element.children, act)
            }
            _ => {}
        }
    }
}

/// A text node's text with its outer white space left out, and that white
/// space.
fn split_space(text: &str) -> (&str, &str, &str) {
    let inner = text.trim();
    let start = text.len() - text.trim_start().len();
    (&text[..start], inner, &text[start + inner.len()..])
}

// ---- The plan -------------------------------------------------------------

/// A message ready to translate.
pub(crate) struct Document {
    tree: Element,
}

impl Document {
    /// `None` when the message is too complex to translate.
    pub(crate) fn parse(html: &str) -> Option<Self> {
        parse(html).map(|tree| Document { tree })
    }

    /// The pieces to translate. With `tags`, a run is one piece whose inline
    /// elements are numbered tags. Without them, every text node is one.
    pub(crate) fn pieces(&self, tags: bool) -> Vec<Piece> {
        let mut tree = self.tree.clone();
        let mut pieces = Vec::new();
        let mut attribute = 0;
        visit_attributes(&mut tree, &mut |element, name| {
            let text = element.attr(name).unwrap_or_default().trim().to_owned();
            pieces.push(Piece {
                text,
                tagged: false,
                slot: Slot::Attribute(attribute),
            });
            attribute += 1;
        });
        let mut next = 0;
        visit_runs(&mut tree, false, &mut next, &mut |run, nodes, pre| {
            if tags {
                let encoded = encode(nodes, pre);
                pieces.push(Piece {
                    tagged: !encoded.parts.is_empty(),
                    text: encoded.text,
                    slot: Slot::Run(run),
                });
            } else {
                pieces.extend(text_pieces(run, nodes));
            }
            None
        });
        pieces
    }

    /// The text nodes of the given runs as pieces, for runs whose tagged
    /// translation didn't fit.
    pub(crate) fn fallback_pieces(&self, runs: &[usize]) -> Vec<Piece> {
        let mut tree = self.tree.clone();
        let mut pieces = Vec::new();
        let mut next = 0;
        visit_runs(&mut tree, false, &mut next, &mut |run, nodes, _| {
            if runs.contains(&run) {
                pieces.extend(text_pieces(run, nodes));
            }
            None
        });
        pieces
    }

    /// The runs whose tagged translation, the text at the same index as
    /// their piece, doesn't fit the tags sent.
    pub(crate) fn misfits(&self, pieces: &[Piece], translations: &[String]) -> Vec<usize> {
        let mut tree = self.tree.clone();
        let wanted: BTreeMap<usize, &str> = pieces
            .iter()
            .zip(translations)
            .filter_map(|(piece, text)| match piece.slot {
                Slot::Run(run) if piece.tagged => Some((run, text.as_str())),
                _ => None,
            })
            .collect();
        let mut misfits = Vec::new();
        let mut next = 0;
        visit_runs(&mut tree, false, &mut next, &mut |run, nodes, pre| {
            if let Some(text) = wanted.get(&run)
                && decode(text, &encode(nodes, pre).parts).is_none()
            {
                misfits.push(run);
            }
            None
        });
        misfits
    }

    /// The message with each piece's translation, at the same index, in its
    /// place. A run with a misfit translation and no text-node pieces stays
    /// as it was. `direction` is the translation's script direction, "rtl"
    /// or "ltr", set where the message sets one.
    pub(crate) fn translated(
        mut self,
        pieces: &[Piece],
        translations: &[String],
        direction: &str,
    ) -> String {
        let mut by_slot: BTreeMap<Slot, &str> = BTreeMap::new();
        for (piece, text) in pieces.iter().zip(translations) {
            if !text.trim().is_empty() {
                by_slot.insert(piece.slot, text.as_str());
            }
        }
        let mut attribute = 0;
        visit_attributes(&mut self.tree, &mut |element, name| {
            if let Some(text) = by_slot.get(&Slot::Attribute(attribute)) {
                element.set_attr(name, text.trim().to_owned());
            }
            attribute += 1;
        });
        let mut next = 0;
        visit_runs(&mut self.tree, false, &mut next, &mut |run, nodes, pre| {
            if let Some(text) = by_slot.get(&Slot::Run(run)) {
                let encoded = encode(nodes, pre);
                if let Some(rebuilt) = decode(text, &encoded.parts) {
                    return Some(pad(rebuilt, encoded.lead, encoded.trail));
                }
            }
            let mut nodes = nodes.to_vec();
            let mut node = 0;
            let mut changed = false;
            visit_text(&mut nodes, &mut |text| {
                if let Some(translation) = by_slot.get(&Slot::Text { run, node }) {
                    let (lead, _, trail) = split_space(text);
                    *text = format!("{lead}{}{trail}", translation.trim());
                    changed = true;
                }
                node += 1;
            });
            changed.then_some(nodes)
        });
        if !set_direction(&mut self.tree, direction) && direction == "rtl" {
            // A message without a body of its own gets one direction for all.
            let children = std::mem::take(&mut self.tree.children);
            self.tree.children = vec![Node::Element(Element {
                name: "div".to_owned(),
                attrs: vec![("dir".to_owned(), direction.to_owned())],
                children,
            })];
        }
        write(&self.tree)
    }
}

fn text_pieces(run: usize, nodes: &[Node]) -> Vec<Piece> {
    let mut nodes = nodes.to_vec();
    let mut pieces = Vec::new();
    let mut node = 0;
    visit_text(&mut nodes, &mut |text| {
        pieces.push(Piece {
            text: split_space(text).1.to_owned(),
            tagged: false,
            slot: Slot::Text { run, node },
        });
        node += 1;
    });
    pieces
}

/// Points every `dir` the message sets, and its body, the way the
/// translation's script runs, as Firefox does after translating a page.
/// Whether the message has a body.
fn set_direction(element: &mut Element, direction: &str) -> bool {
    let mut body = element.name == "body";
    if body || element.attr("dir").is_some() {
        element.set_attr("dir", direction.to_owned());
    }
    for child in &mut element.children {
        if let Node::Element(inner) = child {
            body |= set_direction(inner, direction);
        }
    }
    body
}

#[cfg(test)]
#[path = "document_tests.rs"]
mod tests;
