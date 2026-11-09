//! Non-rendering email policies. Tokenization bounds work before recursive layout.
use html5ever::tokenizer::{BufferQueue, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer};
use std::cell::RefCell;

pub const MAX_HTML_BYTES: usize = 24 * 1024 * 1024;
const MAX_TAGS: usize = 20_000;
const MAX_DEPTH: usize = 96;
const MAX_TEXT_BYTES: usize = 1_000_000;

pub fn escape(text: &str) -> String {
    let extra: usize = text
        .bytes()
        .map(|byte| match byte {
            b'&' => 4,
            b'<' | b'>' => 3,
            b'"' => 5,
            _ => 0,
        })
        .sum();
    let mut output = String::with_capacity(text.len() + extra);
    append_escaped(&mut output, text);
    output
}

pub(crate) fn append_escaped(output: &mut String, text: &str) {
    let mut start = 0;
    for (index, byte) in text.bytes().enumerate() {
        let escaped = match byte {
            b'&' => "&amp;",
            b'<' => "&lt;",
            b'>' => "&gt;",
            b'"' => "&quot;",
            _ => continue,
        };
        output.push_str(&text[start..index]);
        output.push_str(escaped);
        start = index + 1;
    }
    output.push_str(&text[start..]);
}

#[derive(Default)]
struct Scan {
    stack: Vec<html5ever::LocalName>,
    count: usize,
    excessive: bool,
    text: String,
    remote: bool,
    export: String,
    raw: Option<html5ever::LocalName>,
    emit_export: bool,
    collect_text: bool,
    truncated: bool,
}
impl Scan {
    fn append_text(&mut self, text: &str) {
        if !self.collect_text {
            return;
        }
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
                let name = tag.name;
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
                        s.export.push_str("</");
                        s.export.push_str(&name);
                        s.export.push('>');
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
                        let attr_name = attr.name.local.as_str();
                        if (matches!(attr_name, "colspan" | "rowspan" | "span")
                            && value.parse::<u64>().is_ok_and(|n| n > 512))
                            || (attr_name == "style" && excessive_css(value))
                        {
                            s.excessive = true;
                        }
                        if (name.as_str() == "img"
                            && attr.name.local.as_str() == "src"
                            && is_remote(value))
                            || (attr.name.local.as_str() == "style" && css_remote(value))
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
                            let n = attr.name.local.as_str();
                            if !n.starts_with("on") && n != "srcdoc" {
                                s.export.push(' ');
                                s.export.push_str(n);
                                s.export.push_str("=\"");
                                append_escaped(&mut s.export, &attr.value);
                                s.export.push('"');
                            }
                        }
                        s.export.push('>');
                    }
                    if name.as_str() == "br" {
                        s.append_text("\n");
                    }
                    if name.as_str() == "img"
                        && let Some(alt) = tag.attrs.iter().find(|a| a.name.local.as_str() == "alt")
                    {
                        s.append_text(&alt.value);
                    }
                    if matches!(name.as_str(), "script" | "style" | "title") {
                        s.raw = Some(name.clone());
                        return TokenSinkResult::RawData(if name.as_str() == "title" {
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
                        append_escaped(&mut s.export, &text);
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
fn scan(html: &str, emit_export: bool, collect_text: bool) -> Scan {
    scan_into(html, emit_export, collect_text, String::new())
}

fn scan_into(html: &str, emit_export: bool, collect_text: bool, export: String) -> Scan {
    let sink = Sink(RefCell::new(Scan {
        emit_export,
        collect_text,
        export,
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
    css_tokens_excessive(&mut cssparser::Parser::new(css), 0)
}

fn css_tokens_excessive(parser: &mut cssparser::Parser<'_>, depth: usize) -> bool {
    use cssparser::Token as CssToken;
    if depth > 64 {
        return true;
    }
    while let Ok(token) = parser.next() {
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
            CssToken::Function(name) if name.eq_ignore_ascii_case("url") => {
                // Contents are resource identifiers, not layout expressions.
            }
            CssToken::Function(name) => {
                let repeat = name.eq_ignore_ascii_case("repeat");
                let mut excessive = false;
                let _: Result<(), cssparser::ParseError<()>> =
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
                let _: Result<(), cssparser::ParseError<()>> =
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
    result.truncate(result.trim_end().len());
    result
}

fn is_remote(value: &str) -> bool {
    let value = value.trim().as_bytes();
    value
        .get(..6)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"https:"))
        || value
            .get(..5)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"http:"))
        || value.starts_with(b"//")
}
fn css_remote(css: &str) -> bool {
    // CSS escapes are conservatively counted as remote when a URL is present.
    contains_ascii_case_insensitive(css, b"url(")
        && (contains_ascii_case_insensitive(css, b"http:")
            || contains_ascii_case_insensitive(css, b"https:")
            || css.contains("//")
            || css.contains('\\'))
}

fn contains_ascii_case_insensitive(value: &str, needle: &[u8]) -> bool {
    value
        .as_bytes()
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle))
}
pub fn has_remote_images(html: &str) -> bool {
    scan(
        &html[..html.floor_char_boundary(html.len().min(MAX_HTML_BYTES))],
        false,
        false,
    )
    .remote
}
pub fn bounded_html(html: &str) -> (String, Option<String>) {
    if html.len() > MAX_HTML_BYTES {
        return ("<p>This message is too large to render. Use the original source or plain text view.</p>".into(), Some("Message exceeds the HTML size limit.".into()));
    }
    let s = scan(html, false, true);
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
            true,
        )
        .text,
    )
}

pub fn export_html(html: &str, print: bool) -> String {
    // Exports never fetch live remote content. Embedded MIME resources remain
    // available; explicit clicked links still open normally in the browser.
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
    let prefix = format!(
        "<!doctype html><html><head><meta charset='utf-8'><meta http-equiv='Content-Security-Policy' content=\"default-src 'none'; img-src data:; style-src 'unsafe-inline'; font-src data:; {policy}; base-uri 'none'; form-action 'none'\"></head><body>"
    );
    let mut output = scan_into(html, true, false, prefix).export;
    if print {
        output.push_str("<script>");
        output.push_str(PRINT);
        output.push_str("</script>");
    }
    output.push_str("</body></html>");
    output
}

/// The largest message whose style sheets a quote copies onto its elements.
/// Replies are built as the composer opens, so bigger ones skip that work.
const MAX_INLINED_HTML_BYTES: usize = 2 * 1024 * 1024;

/// A message's body as it goes inside a reply or forward: what lies within
/// `<body>`, without the head, style sheets, scripts or event handlers, which
/// would otherwise restyle or act in the message quoting it. The sheets' rules
/// stay on the elements they style, and the body's colors and fonts on a
/// `<div>` around the rest, so the original keeps its look.
pub fn quoted_body(html: &str) -> String {
    struct Quote {
        output: String,
        in_head: bool,
        skipped: Option<html5ever::LocalName>,
        wrapped: bool,
    }
    struct QuoteSink(RefCell<Quote>);
    impl TokenSink for QuoteSink {
        type Handle = ();
        fn process_token(&self, token: Token, _: u64) -> TokenSinkResult<()> {
            let mut quote = self.0.borrow_mut();
            match token {
                Token::TagToken(tag) => {
                    let name = tag.name.as_str();
                    if tag.kind == TagKind::EndTag {
                        if quote.skipped.as_ref() == Some(&tag.name) {
                            quote.skipped = None;
                        } else if name == "head" {
                            quote.in_head = false;
                        } else if !quote.in_head
                            && !matches!(name, "html" | "body" | "meta" | "base" | "link")
                        {
                            quote.output.push_str("</");
                            quote.output.push_str(name);
                            quote.output.push('>');
                        }
                        return TokenSinkResult::Continue;
                    }
                    if matches!(name, "style" | "script" | "title" | "noscript") {
                        quote.skipped = Some(tag.name.clone());
                        return TokenSinkResult::RawData(
                            html5ever::tokenizer::states::RawKind::Rawtext,
                        );
                    }
                    match name {
                        "head" => quote.in_head = true,
                        "body" => {
                            quote.in_head = false;
                            let look = body_look(&tag.attrs);
                            if !look.is_empty() && !quote.wrapped {
                                quote.wrapped = true;
                                quote.output.push_str("<div style=\"");
                                append_escaped(&mut quote.output, &look);
                                quote.output.push_str("\">");
                            }
                        }
                        "html" | "meta" | "base" | "link" => {}
                        _ if quote.in_head => {}
                        _ => {
                            quote.output.push('<');
                            quote.output.push_str(name);
                            for attribute in &tag.attrs {
                                let attribute_name = attribute.name.local.as_str();
                                if attribute_name.starts_with("on") || attribute_name == "srcdoc" {
                                    continue;
                                }
                                quote.output.push(' ');
                                quote.output.push_str(attribute_name);
                                quote.output.push_str("=\"");
                                append_escaped(&mut quote.output, &attribute.value);
                                quote.output.push('"');
                            }
                            quote.output.push('>');
                        }
                    }
                }
                Token::CharacterTokens(text) if quote.skipped.is_none() && !quote.in_head => {
                    append_escaped(&mut quote.output, &text);
                }
                _ => {}
            }
            TokenSinkResult::Continue
        }
    }
    let html = &html[..html.floor_char_boundary(html.len().min(MAX_HTML_BYTES))];
    let inlined = inline_style_sheets(html);
    let html = inlined.as_deref().unwrap_or(html);
    let sink = QuoteSink(RefCell::new(Quote {
        output: String::with_capacity(html.len()),
        in_head: false,
        skipped: None,
        wrapped: false,
    }));
    let tokenizer = Tokenizer::new(sink, Default::default());
    let input = BufferQueue::default();
    input.push_back(html.into());
    let _ = tokenizer.feed(&input);
    tokenizer.end();
    let mut quote = tokenizer.sink.0.into_inner();
    if quote.wrapped {
        quote.output.push_str("</div>");
    }
    quote.output
}

/// `html` with its style sheets' rules copied onto the elements they match,
/// or `None` when it has no sheet or is too big to do that as a reply opens.
fn inline_style_sheets(html: &str) -> Option<String> {
    if html.len() > MAX_INLINED_HTML_BYTES || !contains_ascii_case_insensitive(html, b"<style") {
        return None;
    }
    css_inline::CSSInliner::options()
        // Only the message's own sheets: linked ones are never fetched.
        .load_remote_stylesheets(false)
        // The sheets that define custom properties are left out of the quote.
        .resolve_css_variables(true)
        .minify_css(true)
        .build()
        .inline(html)
        .ok()
}

/// The colors and fonts a message's `<body>` gives all of it, as CSS for a
/// `<div>` around its quote. Its size, spacing and images stay out.
fn body_look(attributes: &[html5ever::Attribute]) -> String {
    let mut look = String::new();
    let mut add = |name: &str, value: &str| {
        look.push_str(name);
        look.push(':');
        look.push_str(value);
        look.push(';');
    };
    // The legacy attributes come first, so the body's own CSS overrides them.
    for attribute in attributes {
        let value = attribute.value.trim();
        let plain = !value.is_empty()
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'#');
        match attribute.name.local.as_str() {
            "bgcolor" if plain => add("background-color", value),
            "text" if plain => add("color", value),
            _ => {}
        }
    }
    for attribute in attributes {
        if attribute.name.local.as_str() != "style" {
            continue;
        }
        for declaration in attribute.value.split(';') {
            let Some((name, value)) = declaration.split_once(':') else {
                continue;
            };
            let name = name.trim().to_ascii_lowercase();
            let value = value.trim();
            let kept = match name.as_str() {
                "background-color" | "color" | "font-family" | "font-size" | "line-height" => true,
                "background" => !contains_ascii_case_insensitive(value, b"url("),
                _ => false,
            };
            if kept && !value.is_empty() {
                add(&name, value);
            }
        }
    }
    look
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn remote_checks_preserve_ascii_case_and_unicode_edges() {
        for value in [
            " HtTpS://test.dev/a ",
            "HTTP:x",
            "//test.dev",
            "éHTTPS:x",
            "https",
            "cid:x",
            "",
        ] {
            let old = value.trim().to_ascii_lowercase();
            assert_eq!(
                is_remote(value),
                old.starts_with("https:") || old.starts_with("http:") || old.starts_with("//")
            );
        }
        for value in [
            "éURL(HTTPS:x)",
            "URL(\\68ttp:x)",
            "url(cid:x)",
            "Url(//test.dev)",
            "url(x) HTTP:y",
            "url",
            "",
        ] {
            let old = value.to_ascii_lowercase();
            assert_eq!(
                css_remote(value),
                old.contains("url(")
                    && (old.contains("http:")
                        || old.contains("https:")
                        || old.contains("//")
                        || old.contains('\\'))
            );
        }
    }
    #[test]
    fn quoted_bodies_keep_the_body_and_its_styles_without_head_or_handlers() {
        let html = "<!doctype html><html><head><title>Sale</title><style>p{color:red}</style>\
            <meta charset=utf-8></head><body style=\"margin:0\"><p class=\"lead\" onclick=\"x()\">\
            Hello &amp; <b>welcome</b></p><style>.x{}</style><img src=\"data:image/png;base64,AA\" alt=\"a&quot;b\">\
            <script>alert(1)</script><br></body></html>";
        assert_eq!(
            quoted_body(html),
            "<p class=\"lead\" style=\"color:red\">Hello &amp; <b>welcome</b></p><img src=\"data:image/png;base64,AA\" alt=\"a&quot;b\"><br>"
        );
        assert_eq!(
            quoted_body("<div>Fragment</div> text"),
            "<div>Fragment</div> text"
        );
    }
    #[test]
    fn quoted_outlook_paragraphs_keep_their_spacing_and_font() {
        // Outlook spaces paragraphs only through its style sheet.
        let html = "<html><head><style><!--\np.MsoNormal, li.MsoNormal\n\t{margin:0cm;\n\t\
            font-size:11.0pt;\n\tfont-family:\"Calibri\",sans-serif;}\n--></style></head>\
            <body lang=\"EN-GB\" style=\"word-wrap:break-word\"><div class=\"WordSection1\">\
            <p class=\"MsoNormal\">Hi Maya,<o:p></o:p></p><p class=\"MsoNormal\"><o:p>&nbsp;</o:p></p>\
            </div></body></html>";
        assert_eq!(
            quoted_body(html),
            "<div class=\"WordSection1\"><p class=\"MsoNormal\" style=\"margin:0cm;font-size:11.0pt;\
            font-family:'Calibri',sans-serif\">Hi Maya,<o:p></o:p></p><p class=\"MsoNormal\" \
            style=\"margin:0cm;font-size:11.0pt;font-family:'Calibri',sans-serif\"><o:p>\u{a0}</o:p></p></div>"
        );
    }
    #[test]
    fn quotes_keep_the_body_colors_and_fonts_but_not_its_size_or_images() {
        let html = "<html><head><style>body{background:#101820;color:#f0f0f0;font-family:Georgia;\
            height:100%}</style></head><body bgcolor=\"#000000\" text=\"red;position:fixed\" \
            style=\"margin:0;background-image:url(https://t.example/p.gif)\"><p>Dark</p></body></html>";
        assert_eq!(
            quoted_body(html),
            "<div style=\"background-color:#000000;background:#101820;color:#f0f0f0;font-family:Georgia;\">\
            <p>Dark</p></div>"
        );
        // A body with nothing but spacing gets no wrapper.
        assert_eq!(
            quoted_body("<body style=\"margin:0\"><p>Light</p></body>"),
            "<p>Light</p>"
        );
    }
    #[test]
    fn style_sheets_of_very_large_messages_are_left_out_of_quotes() {
        let mut html = "<style>p{color:red}</style><p>x</p>".to_owned();
        html.push_str(&"<br>".repeat(MAX_INLINED_HTML_BYTES / 4));
        assert!(quoted_body(&html).starts_with("<p>x</p>"));
    }
    #[test]
    fn escaping_reserves_only_the_final_unicode_output() {
        let text = "<&\"é👩‍🚀>".repeat(10_000);
        let escaped = escape(&text);
        assert_eq!(escaped, "&lt;&amp;&quot;é👩‍🚀&gt;".repeat(10_000));
        assert_eq!(escaped.capacity(), escaped.len());
    }
    #[test]
    fn remote_and_export_scans_do_not_retain_recovery_text() {
        let html = format!(
            "<p>{}</p><img src='https://example.test/a'>",
            "é".repeat(500_000)
        );
        let remote = scan(&html, false, false);
        assert!(remote.remote);
        assert_eq!(remote.text.capacity(), 0);
        let export = scan(&html, true, false);
        assert!(export.export.contains("é"));
        assert_eq!(export.text.capacity(), 0);
        assert_eq!(scan(&html, false, true).text.len(), MAX_TEXT_BYTES);
    }
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
        let result = scan(&huge, false, true);
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
