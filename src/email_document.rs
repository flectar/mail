//! Non-rendering email policies. Tokenization bounds work before recursive layout.
use html5ever::tokenizer::{BufferQueue, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer};
use std::cell::RefCell;

pub const MAX_HTML_BYTES: usize = 24 * 1024 * 1024;
const MAX_TAGS: usize = 20_000;
const MAX_DEPTH: usize = 96;
const MAX_TEXT_BYTES: usize = 1_000_000;

pub fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[derive(Default)]
struct Scan {
    stack: Vec<String>,
    count: usize,
    excessive: bool,
    text: String,
    remote: bool,
    export: String,
    raw: Option<String>,
    emit_export: bool,
    truncated: bool,
}
impl Scan {
    fn append_text(&mut self, text: &str) {
        let remaining = MAX_TEXT_BYTES.saturating_sub(self.text.len());
        let end = text.floor_char_boundary(remaining.min(text.len()));
        self.text.push_str(&text[..end]);
        if end < text.len() {
            self.truncated = true;
            self.excessive = true;
        }
    }
}
struct Sink(RefCell<Scan>);
impl TokenSink for Sink {
    type Handle = ();
    fn process_token(&self, token: Token, _: u64) -> TokenSinkResult<()> {
        let mut s = self.0.borrow_mut();
        match token {
            Token::TagToken(tag) => {
                let name = tag.name.to_string();
                if tag.kind == TagKind::EndTag {
                    if let Some(index) = s.stack.iter().rposition(|n| n == &name) {
                        s.stack.truncate(index);
                    }
                    if matches!(name.as_str(), "script" | "style" | "title") {
                        s.raw = None;
                    }
                    if s.emit_export
                        && !matches!(name.as_str(), "meta" | "base" | "link" | "script")
                    {
                        s.export.push_str(&format!("</{name}>"));
                    }
                    if matches!(
                        name.as_str(),
                        "p" | "div" | "tr" | "li" | "pre" | "h1" | "h2" | "h3"
                    ) {
                        s.append_text("\n");
                    }
                } else {
                    s.count += 1;
                    if !matches!(
                        name.as_str(),
                        "area"
                            | "base"
                            | "br"
                            | "col"
                            | "embed"
                            | "hr"
                            | "img"
                            | "input"
                            | "link"
                            | "meta"
                            | "param"
                            | "source"
                            | "track"
                            | "wbr"
                    ) {
                        if s.stack.len() <= MAX_DEPTH {
                            s.stack.push(name.clone());
                        } else {
                            s.excessive = true;
                        }
                    }
                    if s.count > MAX_TAGS || s.stack.len() > MAX_DEPTH {
                        s.excessive = true;
                    }
                    for attr in &tag.attrs {
                        let value = attr.value.as_ref();
                        let attr_name = attr.name.local.as_ref();
                        if (matches!(attr_name, "colspan" | "rowspan" | "span")
                            && value.parse::<u64>().is_ok_and(|n| n > 512))
                            || (attr_name == "style" && excessive_css(value))
                        {
                            s.excessive = true;
                        }
                        if (name == "img" && attr.name.local.as_ref() == "src" && is_remote(value))
                            || (attr.name.local.as_ref() == "style" && css_remote(value))
                        {
                            s.remote = true;
                        }
                    }
                    if s.emit_export
                        && !matches!(name.as_str(), "meta" | "base" | "link" | "script")
                    {
                        s.export.push('<');
                        s.export.push_str(&name);
                        for attr in &tag.attrs {
                            let n = attr.name.local.as_ref();
                            if !n.starts_with("on") && n != "srcdoc" {
                                s.export
                                    .push_str(&format!(" {n}=\"{}\"", escape(&attr.value)));
                            }
                        }
                        s.export.push('>');
                    }
                    if name == "br" {
                        s.append_text("\n");
                    }
                    if name == "img"
                        && let Some(alt) = tag.attrs.iter().find(|a| a.name.local.as_ref() == "alt")
                    {
                        s.append_text(&alt.value);
                    }
                    if matches!(name.as_str(), "script" | "style" | "title") {
                        s.raw = Some(name.clone());
                        return TokenSinkResult::RawData(if name == "title" {
                            html5ever::tokenizer::states::RawKind::Rcdata
                        } else {
                            html5ever::tokenizer::states::RawKind::Rawtext
                        });
                    }
                }
            }
            Token::CharacterTokens(text) => {
                if s.raw.as_deref() == Some("style") {
                    s.remote |= css_remote(&text);
                    s.excessive |= excessive_css(&text);
                    if s.emit_export {
                        s.export.push_str(&text);
                    }
                } else if s.raw.as_deref() != Some("script") {
                    if s.emit_export {
                        s.export.push_str(&escape(&text));
                    }
                    if s.raw.is_none() {
                        s.append_text(&text);
                    }
                }
            }
            _ => {}
        }
        TokenSinkResult::Continue
    }
}
fn scan(html: &str, emit_export: bool) -> Scan {
    let sink = Sink(RefCell::new(Scan {
        emit_export,
        ..Default::default()
    }));
    let tokenizer = Tokenizer::new(sink, Default::default());
    let input = BufferQueue::default();
    input.push_back(html.into());
    let _ = tokenizer.feed(&input);
    tokenizer.end();
    tokenizer.sink.0.into_inner()
}
fn excessive_css(css: &str) -> bool {
    if css.len() > 512 * 1024 {
        return true;
    }
    let mut nesting = 0usize;
    let mut digits = 0usize;
    for c in css.chars() {
        match c {
            '{' | '(' | '[' => {
                nesting += 1;
                if nesting > 64 {
                    return true;
                }
            }
            '}' | ')' | ']' => nesting = nesting.saturating_sub(1),
            _ => {}
        }
        if c.is_ascii_digit() {
            digits += 1;
            if digits > 8 {
                return true;
            }
        } else {
            digits = 0;
        }
    }
    // Grid repetition can allocate far more tracks than there are DOM nodes.
    let lower = css.to_ascii_lowercase();
    lower.split("repeat(").skip(1).any(|tail| {
        tail.split(',')
            .next()
            .is_some_and(|count| count.trim().parse::<u64>().is_ok_and(|n| n > 512))
    })
}
fn is_remote(value: &str) -> bool {
    let value = value.trim().to_ascii_lowercase();
    value.starts_with("https:") || value.starts_with("http:") || value.starts_with("//")
}
fn css_remote(css: &str) -> bool {
    let css = css.to_ascii_lowercase();
    // CSS escapes are conservatively counted as remote when a URL is present.
    css.contains("url(")
        && (css.contains("http:")
            || css.contains("https:")
            || css.contains("//")
            || css.contains('\\'))
}
pub fn has_remote_images(html: &str) -> bool {
    scan(
        &html[..html.floor_char_boundary(html.len().min(MAX_HTML_BYTES))],
        false,
    )
    .remote
}
pub fn bounded_html(html: &str) -> (String, Option<String>) {
    if html.len() > MAX_HTML_BYTES {
        return ("<p>This message is too large to render. Use the original source or plain text view.</p>".into(), Some("Message exceeds the HTML size limit.".into()));
    }
    let s = scan(html, false);
    if s.excessive {
        return (
            format!(
                "<pre style='white-space:pre-wrap'>{}</pre>",
                escape(&s.text)
            ),
            Some(if s.truncated { "Message text exceeds the display limit. Full content is available in the original source." } else { "Complex message shown as plain text." }.into()),
        );
    }
    (html.into(), None)
}
pub fn fallback(html: &str) -> String {
    scan(
        &html[..html.floor_char_boundary(html.len().min(MAX_HTML_BYTES))],
        false,
    )
    .text
}

pub fn export_html(html: &str, print: bool) -> String {
    // Exports never fetch live remote content. Embedded MIME resources remain
    // available; explicit clicked links still open normally in the browser.
    let clean = scan(html, true).export;
    const PRINT: &str = "window.addEventListener('load',()=>window.print())";
    use base64::Engine;
    use sha2::{Digest, Sha256};
    let policy = if print {
        format!(
            "script-src 'sha256-{}'",
            base64::engine::general_purpose::STANDARD.encode(Sha256::digest(PRINT.as_bytes()))
        )
    } else {
        "script-src 'none'".into()
    };
    let script = if print {
        format!("<script>{PRINT}</script>")
    } else {
        String::new()
    };
    format!(
        "<!doctype html><html><head><meta charset='utf-8'><meta http-equiv='Content-Security-Policy' content=\"default-src 'none'; img-src data:; style-src 'unsafe-inline'; font-src data:; {policy}; base-uri 'none'; form-action 'none'\"></head><body>{clean}{script}</body></html>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn limits_and_export_policy() {
        let html = format!("{}text{}", "<div>".repeat(100), "</div>".repeat(100));
        assert!(bounded_html(&html).1.is_some());
        let exported = export_html(
            "<meta http-equiv='refresh' content='0;url=https://evil.test'><p>Hello</p><script>bad()</script>",
            false,
        );
        assert!(!exported.contains("refresh"));
        assert!(!exported.contains("bad()"));
        assert!(exported.contains("img-src data:"));
        assert!(has_remote_images(
            "<img src='&#104;ttps://example.com/a.png'>"
        ));
        assert!(!has_remote_images("<a href='https://example.com'>link</a>"));
        let huge = "é".repeat(MAX_TEXT_BYTES);
        let result = scan(&huge, false);
        assert!(result.truncated);
        assert!(result.text.len() <= MAX_TEXT_BYTES);
        assert!(bounded_html(&huge).1.unwrap().contains("display limit"));
        assert!(
            bounded_html("<table><tr><td colspan='1000000'>Large span</td></tr></table>")
                .1
                .is_some()
        );
        assert!(bounded_html("<div style='display:grid;grid-template-columns:repeat(1000000,1px)'>Large grid</div>").1.is_some());
        assert!(export_html("<p>Print</p>", true).contains("script-src 'sha256-"));
        assert!(
            !export_html("<body onload='steal()'><script>steal()</script>", true)
                .contains("steal()")
        );
    }
}
