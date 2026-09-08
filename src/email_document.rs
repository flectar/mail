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
    let mut input = cssparser::ParserInput::new(css);
    css_tokens_excessive(&mut cssparser::Parser::new(&mut input), 0)
}

fn css_tokens_excessive(parser: &mut cssparser::Parser<'_, '_>, depth: usize) -> bool {
    use cssparser::Token as CssToken;
    if depth > 64 {
        return true;
    }
    while let Ok(token) = parser.next().cloned() {
        match token {
            // Bound numeric magnitude, not decimal precision or digits in URLs,
            // selectors, comments, strings, and colors. Marketing generators
            // routinely emit widths such as 172.89473684210526px.
            CssToken::Number { value, .. } | CssToken::Dimension { value, .. }
                if !value.is_finite() || value.abs() >= 100_000_000.0 =>
            {
                return true;
            }
            CssToken::Percentage { unit_value, .. }
                if !unit_value.is_finite() || unit_value.abs() >= 1_000_000.0 =>
            {
                return true;
            }
            CssToken::Function(ref name) if name.eq_ignore_ascii_case("url") => {
                // Contents are resource identifiers, not layout expressions.
            }
            CssToken::Function(ref name) => {
                let repeat = name.eq_ignore_ascii_case("repeat");
                let mut excessive = false;
                let _: Result<(), cssparser::ParseError<'_, ()>> =
                    parser.parse_nested_block(|nested| {
                        if repeat
                            && nested
                                .try_parse(|p| p.expect_number())
                                .is_ok_and(|n| n > 512.0)
                        {
                            excessive = true;
                        }
                        excessive |= css_tokens_excessive(nested, depth + 1);
                        Ok(())
                    });
                if excessive {
                    return true;
                }
            }
            CssToken::ParenthesisBlock
            | CssToken::CurlyBracketBlock
            | CssToken::SquareBracketBlock => {
                let mut excessive = false;
                let _: Result<(), cssparser::ParseError<'_, ()>> =
                    parser.parse_nested_block(|nested| {
                        excessive = css_tokens_excessive(nested, depth + 1);
                        Ok(())
                    });
                if excessive {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

// HTML templates contain indentation and empty spacer rows. Preserve paragraph
// separation and preformatted indentation without making recovery text start
// thousands of blank lines below the viewport.
fn readable_fallback(text: &str) -> String {
    let mut result = String::new();
    let mut blank = false;
    for line in text.trim().lines() {
        if line.trim().is_empty() {
            if !blank {
                result.push('\n');
            }
            blank = true;
        } else {
            result.push_str(line.trim_end());
            result.push('\n');
            blank = false;
        }
    }
    result.trim_end().to_owned()
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
                "<pre style='margin:16px;white-space:pre-wrap;overflow-wrap:anywhere;color:#111;background:#fff'>{}</pre>",
                escape(&readable_fallback(&s.text))
            ),
            Some(if s.truncated { "Message text exceeds the display limit. Full content is available in the original source." } else { "Complex message shown as plain text." }.into()),
        );
    }
    (html.into(), None)
}
pub fn fallback(html: &str) -> String {
    readable_fallback(
        &scan(
            &html[..html.floor_char_boundary(html.len().min(MAX_HTML_BYTES))],
            false,
        )
        .text,
    )
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
    fn generated_css_precision_and_identifiers_are_not_complexity() {
        for css in [
            "width:33.333333333333336%;max-width:172.89473684210526px",
            ".mj-column-per-33-333333333333336 { width:33.333333333333336% !important }",
            "background:url(https://example.test/12345678901234567890.png)",
            "background:url('https://example.test/12345678901234567890.png')",
            "content:'12345678901234567890'; /* ((([[[ 999999999999999 */",
            "max-width:NaNpx;padding:auto;font-size:max(16px,1rem)",
        ] {
            assert!(!excessive_css(css), "{css}");
        }
        for css in [
            "width:100000000px",
            "width:1e12px",
            "width:1e999px",
            "width:100000000%",
            "grid-template-columns:repeat(513,1px)",
            "@media screen { .grid {grid-template-columns:RePeAt(1000000,1px)} }",
        ] {
            assert!(excessive_css(css), "{css}");
        }
        assert!(excessive_css(&format!(
            "width:{}1px{}",
            "calc(".repeat(70),
            ")".repeat(70)
        )));
    }

    #[test]
    fn marketing_templates_keep_html_and_recovery_starts_with_content() {
        for html in [
            include_str!("../resources/test-emails/revolut-precision.html"),
            include_str!("../resources/test-emails/mailersend-precision.html"),
        ] {
            let (bounded, notice) = bounded_html(html);
            assert!(notice.is_none(), "{notice:?}");
            assert_eq!(bounded, html);
        }
        let html = format!(
            "<body>{}<div style='width:1e12px'>Visible recovery</div>{}<p>Second paragraph</p></body>",
            "\n ".repeat(500),
            "\n ".repeat(500)
        );
        let (bounded, notice) = bounded_html(&html);
        assert!(notice.is_some());
        assert!(
            bounded.contains(">Visible recovery\n\n Second paragraph"),
            "{bounded}"
        );
        assert!(bounded.len() < 250);
        assert!(fallback(&html).starts_with("Visible recovery"));
    }

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
