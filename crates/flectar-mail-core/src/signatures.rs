//! Account-scoped signature validation and plain-text composition.
use crate::{
    Core,
    db::repo,
    error::{CoreError, Result},
    models::{Signature, SignatureDefaults},
};
use html5ever::tokenizer::{BufferQueue, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer};
use std::{cell::RefCell, collections::HashSet};

/// A signature's HTML, pictures included, can be this large.
pub const MAX_SIGNATURE_BYTES: usize = 256 * 1024;
pub(crate) const MAX_SIGNATURES_PER_ACCOUNT: usize = 20;

pub fn plain_text(signature: &Signature) -> String {
    let mut text = mail_parser::decoders::html::html_to_text(&signature.html);
    text.truncate(text.trim_end().len());
    let start = text.len() - text.trim_start().len();
    if start > 0 {
        text.drain(..start);
    }
    // The decoder reserves the HTML input length, which can greatly exceed
    // the retained text after markup removal and whitespace trimming.
    text.shrink_to_fit();
    text
}

pub fn text_html(text: &str) -> String {
    let extra: usize = text
        .bytes()
        .map(|byte| match byte {
            b'&' => 4,
            b'<' | b'>' | b'\n' => 3,
            _ => 0,
        })
        .sum();
    let mut html = String::with_capacity(text.len() + extra + 11);
    html.push_str("<div>");
    for ch in text.chars() {
        match ch {
            '&' => html.push_str("&amp;"),
            '<' => html.push_str("&lt;"),
            '>' => html.push_str("&gt;"),
            '\n' => html.push_str("<br>"),
            _ => html.push(ch),
        }
    }
    html.push_str("</div>");
    html
}

/// The class of a block that keeps the layout it came with, such as a signature's table with a
/// logo beside its text, which the composer shows as it is rather than as text to edit.
pub const LAYOUT_CLASS: &str = "flectar-layout";

/// The CSS properties signatures keep, for their type, colors and the boxes of their layout.
/// None of them takes a picture or another address, so nothing in a signature's CSS loads
/// from the web.
const STYLE_PROPERTIES: &[&str] = &[
    "color",
    "background-color",
    "font",
    "font-family",
    "font-size",
    "font-stretch",
    "font-style",
    "font-variant",
    "font-weight",
    "line-height",
    "letter-spacing",
    "word-spacing",
    "text-align",
    "text-decoration",
    "text-decoration-color",
    "text-decoration-line",
    "text-decoration-style",
    "text-indent",
    "text-transform",
    "white-space",
    "word-break",
    "word-wrap",
    "overflow-wrap",
    "vertical-align",
    "direction",
    "display",
    "width",
    "min-width",
    "max-width",
    "height",
    "min-height",
    "max-height",
    "box-sizing",
    "overflow",
    "float",
    "clear",
    "margin",
    "margin-top",
    "margin-right",
    "margin-bottom",
    "margin-left",
    "padding",
    "padding-top",
    "padding-right",
    "padding-bottom",
    "padding-left",
    "border",
    "border-top",
    "border-right",
    "border-bottom",
    "border-left",
    "border-width",
    "border-style",
    "border-color",
    "border-top-width",
    "border-right-width",
    "border-bottom-width",
    "border-left-width",
    "border-top-style",
    "border-right-style",
    "border-bottom-style",
    "border-left-style",
    "border-top-color",
    "border-right-color",
    "border-bottom-color",
    "border-left-color",
    "border-radius",
    "border-collapse",
    "border-spacing",
    "table-layout",
    "list-style-type",
    "list-style-position",
];

/// The formatting the composer writes, the layout of signatures made elsewhere, such as a table
/// with a logo beside the text, and pictures the signature holds itself: no active HTML,
/// pictures from the web or other remote requests in signature settings. The composer cleans
/// each layout it keeps the same way.
pub fn sanitize_html(html: &str) -> String {
    static SANITIZER: std::sync::LazyLock<ammonia::Builder<'static>> =
        std::sync::LazyLock::new(|| {
            let mut builder = ammonia::Builder::default();
            builder
                // Signatures written in older mail apps set their type with `<font>`.
                .add_tags(["font", "tfoot"])
                .add_generic_attributes([
                    "style",
                    "align",
                    "dir",
                    // The attributes signatures lay their tables out with, which mail apps
                    // render and other mail clients' sanitizers keep.
                    "valign",
                    "width",
                    "height",
                    "border",
                    "cellpadding",
                    "cellspacing",
                    "bgcolor",
                    // Tells screen readers a table only lays the signature out.
                    "role",
                ])
                .add_tag_attributes("font", ["color", "face", "size"])
                .add_tag_attributes("hr", ["color", "noshade"])
                .add_tag_attributes("td", ["nowrap"])
                .add_tag_attributes("th", ["nowrap"])
                .add_allowed_classes("div", [LAYOUT_CLASS])
                .add_url_schemes(["data"])
                .filter_style_properties(STYLE_PROPERTIES.iter().copied().collect())
                .attribute_filter(|element, attribute, value| match (element, attribute) {
                    ("img", "src") => is_embedded_picture(value).then(|| value.into()),
                    (_, "href") if has_data_scheme(value) => None,
                    _ => Some(value.into()),
                })
                .link_rel(Some("noopener noreferrer"));
            builder
        });
    with_layout_tables_marked(without_unsourced_pictures(
        SANITIZER.clean(html).to_string(),
    ))
}

/// A tag or a text of HTML ammonia wrote, read back.
enum Piece {
    Start {
        name: String,
        attributes: Vec<(String, String)>,
    },
    End(String),
    Text(String),
}

struct Pieces(RefCell<Vec<Piece>>);

impl TokenSink for Pieces {
    type Handle = ();

    fn process_token(&self, token: Token, _: u64) -> TokenSinkResult<()> {
        let piece = match token {
            Token::TagToken(tag) if tag.kind == TagKind::EndTag => Piece::End(tag.name.to_string()),
            Token::TagToken(tag) => Piece::Start {
                name: tag.name.to_string(),
                attributes: tag
                    .attrs
                    .iter()
                    .map(|attribute| {
                        (
                            attribute.name.local.to_string(),
                            attribute.value.to_string(),
                        )
                    })
                    .collect(),
            },
            Token::CharacterTokens(text) => Piece::Text(text.to_string()),
            _ => return TokenSinkResult::Continue,
        };
        self.0.borrow_mut().push(piece);
        TokenSinkResult::Continue
    }
}

/// `html`, as ammonia writes it, with each table that only lays the signature out marked so for
/// screen readers, as the W3C and the Email Markup Consortium advise for mail: one without a
/// role, header cells, a caption or a summary of its own. Everything else is written as it was.
fn with_layout_tables_marked(html: String) -> String {
    if !html.contains("<table") {
        return html;
    }
    let tokenizer = Tokenizer::new(Pieces(RefCell::default()), Default::default());
    let input = BufferQueue::default();
    input.push_back(html.as_str().into());
    let _ = tokenizer.feed(&input);
    tokenizer.end();
    let pieces = tokenizer.sink.0.into_inner();
    // The start tags of the layout tables, by their place among the pieces. A header cell or a
    // caption belongs to the innermost table open.
    let mut layout = HashSet::new();
    let mut open: Vec<(usize, bool)> = Vec::new();
    for (index, piece) in pieces.iter().enumerate() {
        match piece {
            Piece::Start { name, attributes } if name == "table" => {
                let described = attributes.iter().any(|(attribute, value)| {
                    attribute == "role" || (attribute == "summary" && !value.trim().is_empty())
                });
                open.push((index, !described));
            }
            Piece::Start { name, .. } if name == "th" || name == "caption" => {
                if let Some(table) = open.last_mut() {
                    table.1 = false;
                }
            }
            Piece::End(name) if name == "table" => {
                if let Some((index, true)) = open.pop() {
                    layout.insert(index);
                }
            }
            _ => {}
        }
    }
    if layout.is_empty() {
        return html;
    }
    // Written back the way ammonia's serializer escapes, so only the roles change.
    let escape = |output: &mut String, text: &str, attribute: bool| {
        for character in text.chars() {
            match character {
                '&' => output.push_str("&amp;"),
                '\u{a0}' => output.push_str("&nbsp;"),
                '"' if attribute => output.push_str("&quot;"),
                '<' if !attribute => output.push_str("&lt;"),
                '>' if !attribute => output.push_str("&gt;"),
                character => output.push(character),
            }
        }
    };
    let mut marked = String::with_capacity(html.len() + layout.len() * 20);
    for (index, piece) in pieces.iter().enumerate() {
        match piece {
            Piece::Start { name, attributes } => {
                marked.push('<');
                marked.push_str(name);
                for (attribute, value) in attributes {
                    marked.push(' ');
                    marked.push_str(attribute);
                    marked.push_str("=\"");
                    escape(&mut marked, value, true);
                    marked.push('"');
                }
                if layout.contains(&index) {
                    marked.push_str(" role=\"presentation\"");
                }
                marked.push('>');
            }
            Piece::End(name) => {
                marked.push_str("</");
                marked.push_str(name);
                marked.push('>');
            }
            Piece::Text(text) => escape(&mut marked, text, false),
        }
    }
    marked
}

/// `html` as ammonia writes it, without the pictures whose address it took out: some mail apps
/// show an `<img>` without one as a broken picture. Ammonia quotes every attribute value with
/// `"` and escapes that inside them, so a tag ends at the first `>` outside quotes.
fn without_unsourced_pictures(html: String) -> String {
    if !html.contains("<img") {
        return html;
    }
    let mut kept = String::with_capacity(html.len());
    let mut rest = html.as_str();
    while let Some(start) = rest.find("<img") {
        kept.push_str(&rest[..start]);
        let tag = &rest[start..];
        let mut quoted = false;
        let end = tag
            .char_indices()
            .find(|&(_, character)| {
                if character == '"' {
                    quoted = !quoted;
                }
                character == '>' && !quoted
            })
            .map_or(tag.len(), |(at, _)| at + 1);
        let is_picture = tag[4..]
            .chars()
            .next()
            .is_some_and(|next| next.is_ascii_whitespace() || next == '>' || next == '/');
        if !is_picture || tag[..end].contains(" src=\"") {
            kept.push_str(&tag[..end]);
        }
        rest = &tag[end..];
    }
    kept.push_str(rest);
    kept
}

/// Whether `src` holds a PNG, JPEG, GIF or WebP picture itself, as the composer writes them.
fn is_embedded_picture(src: &str) -> bool {
    let src = src.trim();
    ["png", "jpeg", "gif", "webp"].iter().any(|kind| {
        let prefix = format!("data:image/{kind};base64,");
        src.get(..prefix.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(&prefix))
    })
}

fn has_data_scheme(url: &str) -> bool {
    url.trim()
        .get(..5)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("data:"))
}

/// Where a signature goes in `body`: above the quotation, or at the end.
pub fn insertion_point(body: &str) -> usize {
    quotation_start(body).unwrap_or(body.len())
}

/// Insert above the quotation, preserving the standard `-- ` delimiter.
pub fn insert(body: &str, signature: &str) -> String {
    let signature = signature.trim();
    if signature.is_empty() {
        return body.to_owned();
    }
    let at = insertion_point(body);
    let mut result = String::with_capacity(body.len() + signature.len() + 6);
    result.push_str(&body[..at]);
    result.push_str("\n\n-- \n");
    result.push_str(signature);
    result.push_str(&body[at..]);
    result
}

/// Where a reply or forward quotation begins: the paragraph that opens with
/// a forwarded message's dashed rule, or whose first line introduces quoted
/// lines, written with `>` or as the composer's quote block. The structure,
/// unlike the wording, is the same in every language the header was written
/// in.
fn quotation_start(body: &str) -> Option<usize> {
    let quoted = |line: &str| line.starts_with('>') || line.starts_with("│ ");
    (0..body.len()).find(|&at| {
        if !body.as_bytes()[at..].starts_with(b"\n\n") {
            return false;
        }
        let paragraph = &body[at + 2..];
        let mut lines = paragraph.lines();
        paragraph.starts_with("---------- ")
            || (lines.next().is_some_and(|line| !quoted(line)) && lines.next().is_some_and(quoted))
    })
}

impl Core {
    pub async fn save_signature(&self, mut signature: Signature) -> Result<Signature> {
        signature.name = signature.name.trim().to_owned();
        if signature.name.is_empty()
            || signature.name.len() > 100
            || signature.html.len() > MAX_SIGNATURE_BYTES
        {
            return Err(CoreError::Other(
                "Use a signature name of 1–100 bytes and a body under 256 KiB".into(),
            ));
        }
        signature.html = sanitize_html(&signature.html);
        if signature.id.is_empty() {
            signature.id = format!("sig-{:032x}", rand::random::<u128>());
        }
        self.db
            .write(move |conn| {
                if repo::accounts::get(conn, signature.account_id)?.is_none() {
                    return Err(CoreError::NotFound("account".into()));
                }
                let mut settings = repo::settings::get(conn)?;
                if let Some(existing) = settings
                    .signature_list
                    .iter_mut()
                    .find(|s| s.id == signature.id)
                {
                    if existing.account_id != signature.account_id {
                        return Err(CoreError::Other(
                            "Signature belongs to another account".into(),
                        ));
                    }
                    *existing = signature.clone();
                } else {
                    if settings
                        .signature_list
                        .iter()
                        .filter(|s| s.account_id == signature.account_id)
                        .count()
                        >= MAX_SIGNATURES_PER_ACCOUNT
                    {
                        return Err(CoreError::Other(
                            "An account can have up to 20 signatures".into(),
                        ));
                    }
                    settings.signature_list.push(signature.clone());
                }
                repo::settings::set(conn, &settings)?;
                Ok(signature)
            })
            .await
    }

    pub async fn delete_signature(&self, account_id: i64, id: String) -> Result<()> {
        self.db
            .write(move |conn| {
                let mut settings = repo::settings::get(conn)?;
                settings
                    .signature_list
                    .retain(|s| !(s.account_id == account_id && s.id == id));
                if let Some(defaults) = settings.signature_defaults.get_mut(&account_id.to_string())
                {
                    if defaults.new_id.as_ref() == Some(&id) {
                        defaults.new_id = None;
                    }
                    if defaults.reply_id.as_ref() == Some(&id) {
                        defaults.reply_id = None;
                    }
                }
                repo::settings::set(conn, &settings)
            })
            .await
    }

    pub async fn set_signature_defaults(
        &self,
        account_id: i64,
        defaults: SignatureDefaults,
    ) -> Result<()> {
        self.db
            .write(move |conn| {
                if repo::accounts::get(conn, account_id)?.is_none() {
                    return Err(CoreError::NotFound("account".into()));
                }
                let mut settings = repo::settings::get(conn)?;
                for id in [&defaults.new_id, &defaults.reply_id].into_iter().flatten() {
                    if !settings
                        .signature_list
                        .iter()
                        .any(|s| s.account_id == account_id && &s.id == id)
                    {
                        return Err(CoreError::Other(
                            "Choose a signature belonging to this account".into(),
                        ));
                    }
                }
                settings
                    .signature_defaults
                    .insert(account_id.to_string(), defaults);
                repo::settings::set(conn, &settings)
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inserts_before_history_and_escapes_text() {
        assert_eq!(
            insert("Reply\n\nOn Monday, Pat wrote:\n> Hello", "Me"),
            "Reply\n\n-- \nMe\n\nOn Monday, Pat wrote:\n> Hello"
        );
        assert_eq!(
            insert("Hola\n\nEl lunes, Pat escribió:\n> Hola", "Yo"),
            "Hola\n\n-- \nYo\n\nEl lunes, Pat escribió:\n> Hola"
        );
        assert_eq!(
            insert(
                "\n\n---------- Пересылаемое сообщение ----------\nОт: Pat",
                "Я"
            ),
            "\n\n-- \nЯ\n\n---------- Пересылаемое сообщение ----------\nОт: Pat"
        );
        assert_eq!(
            insert("Hi\n\n\nPat wrote:\n> Hello", "Me"),
            "Hi\n\n\n-- \nMe\n\nPat wrote:\n> Hello"
        );
        // A quote moved into the editor is a quote block under its line.
        assert_eq!(
            insert("Reply\n\nPat wrote:\n│ Hello\n│ \n│ Bye", "Me"),
            "Reply\n\n-- \nMe\n\nPat wrote:\n│ Hello\n│ \n│ Bye"
        );
        assert_eq!(
            insert("On Monday we met.\n\nOnward, then.", "Me"),
            "On Monday we met.\n\nOnward, then.\n\n-- \nMe"
        );
        let s = Signature {
            id: "s".into(),
            account_id: 1,
            name: "Work".into(),
            html: text_html("Zoë <me@example.org>\nEngineer"),
        };
        assert_eq!(plain_text(&s), "Zoë <me@example.org>\nEngineer");
        assert_eq!(insert("Body", ""), "Body");
    }

    #[test]
    fn signatures_keep_their_formatting_and_their_own_pictures() {
        let logo = "data:image/png;base64,iVBORw0KGgo=";
        let html = format!(
            "<div align=\"center\" style=\"text-align:center\"><span style=\"color:#0b5394;\
             font-size:x-large\">Zoë</span><br><a href=\"https://example.org\">example.org</a>\
             <br><img src=\"{logo}\" alt=\"Logo\" width=\"120\" height=\"40\" \
             style=\"width:120px;max-width:100%;height:auto\"></div>"
        );
        let clean = sanitize_html(&html);
        for kept in [
            "align=\"center\"",
            "style=\"text-align:center\"",
            "style=\"color:#0b5394;font-size:x-large\"",
            "href=\"https://example.org\"",
            "src=\"data:image/png;base64,iVBORw0KGgo=\"",
            "alt=\"Logo\"",
            "style=\"width:120px;max-width:100%;height:auto\"",
        ] {
            assert!(clean.contains(kept), "{kept} in {clean}");
        }

        // Pictures from the web, other embedded data, scripts and data links go.
        let clean = sanitize_html(
            "<img src=\"https://tracker.example/p.gif\" alt=\"web\">\
             <img src=\"data:image/svg+xml;base64,PHN2Zy8+\" alt=\"svg\">\
             <a href=\"data:text/html,hi\">data</a><a href=\"javascript:alert(1)\">js</a>\
             <script>alert(1)</script><span onclick=\"x()\">Me</span>",
        );
        for gone in [
            "tracker.example",
            "svg+xml",
            "data:text",
            "javascript",
            "<script",
            "onclick",
            // Without their address, the pictures go entirely.
            "<img",
            "alt=",
        ] {
            assert!(!clean.contains(gone), "{gone} in {clean}");
        }
        assert!(clean.contains("Me"), "{clean}");

        // Only those: the signature's own picture stays, and text that reads like a tag is text.
        let clean = sanitize_html(&format!(
            "<p>a &lt;img&gt; b<img alt='x > y' src='https://example.org/a.png'>c\
             <img alt='Logo \"Zoë\" >' src='{logo}'>d</p>"
        ));
        assert_eq!(
            clean,
            format!(
                "<p>a &lt;img&gt; bc<img alt=\"Logo &quot;Zoë&quot; &gt;\" src=\"{logo}\">d</p>"
            )
        );
    }

    #[test]
    fn signatures_made_elsewhere_keep_their_table_layout() {
        let logo = "data:image/png;base64,iVBORw0KGgo=";
        let html = format!(
            "<div class=\"{LAYOUT_CLASS} brand\"><table role=\"presentation\" cellpadding=\"0\" \
             cellspacing=\"0\" border=\"0\" width=\"480\" style=\"border-collapse:collapse\">\
             <tbody><tr><td valign=\"top\" width=\"96\" bgcolor=\"#ffffff\" nowrap \
             style=\"padding:0 12px 0 0;border-right:2px solid #0a66c2\"><img src=\"{logo}\" \
             width=\"80\" height=\"80\" border=\"0\" alt=\"Logo\" style=\"display:block\"></td>\
             <td valign=\"top\"><font face=\"Arial\" color=\"#1f2328\" size=\"2\"><b>Zoë</b>\
             </font><hr color=\"#d0d7de\" size=\"1\" noshade style=\"border:0;border-top:1px \
             solid #d0d7de\"><a href=\"https://example.org\">example.org</a></td></tr></tbody>\
             </table></div>"
        );
        let clean = sanitize_html(&html);
        for kept in [
            "<div class=\"flectar-layout\">",
            "role=\"presentation\"",
            "cellpadding=\"0\"",
            "cellspacing=\"0\"",
            "border=\"0\"",
            "width=\"480\"",
            "style=\"border-collapse:collapse\"",
            "valign=\"top\"",
            "width=\"96\"",
            "bgcolor=\"#ffffff\"",
            "nowrap",
            "style=\"padding:0 12px 0 0;border-right:2px solid #0a66c2\"",
            "src=\"data:image/png;base64,iVBORw0KGgo=\"",
            "style=\"display:block\"",
            "<font face=\"Arial\" color=\"#1f2328\" size=\"2\">",
            "<hr color=\"#d0d7de\" size=\"1\" noshade=\"\" style=\"border:0;border-top:1px solid #d0d7de\">",
            "href=\"https://example.org\"",
        ] {
            assert!(clean.contains(kept), "{kept} in {clean}");
        }
        // Classes of its own, which nothing styles here, go.
        assert!(!clean.contains("brand"), "{clean}");
    }

    #[test]
    fn layout_tables_are_marked_for_screen_readers() {
        let clean = sanitize_html(
            "<p>Fish &amp; chips\u{a0}&lt;3 \"x\"</p><table cellpadding=\"0\"><tr><td><table><tr>\
             <td title=\"A &amp; B\">nested</td></tr></table></td></tr></table>\
             <table><tr><th>Day</th></tr></table>\
             <table><caption>Hours</caption><tr><td>9</td></tr></table>\
             <table summary=\"Opening hours\"><tr><td>9</td></tr></table>\
             <table role=\"grid\"><tr><td>9</td></tr></table>",
        );
        assert_eq!(
            clean,
            "<p>Fish &amp; chips&nbsp;&lt;3 \"x\"</p><table cellpadding=\"0\" role=\"presentation\">\
             <tbody><tr><td><table role=\"presentation\"><tbody><tr><td title=\"A &amp; B\">nested\
             </td></tr></tbody></table></td></tr></tbody></table>\
             <table><tbody><tr><th>Day</th></tr></tbody></table>\
             <table><caption>Hours</caption><tbody><tr><td>9</td></tr></tbody></table>\
             <table summary=\"Opening hours\"><tbody><tr><td>9</td></tr></tbody></table>\
             <table role=\"grid\"><tbody><tr><td>9</td></tr></tbody></table>"
        );
        // Cleaning it again changes nothing.
        assert_eq!(sanitize_html(&clean), clean);
    }

    #[test]
    fn signature_css_loads_nothing_from_the_web() {
        let clean = sanitize_html(
            "<span style=\"color:#333;background-image:url(https://tracker.example/a.png);\
             padding:4px\">a</span><div style=\"background:url('https://tracker.example/b.png') \
             #fff;list-style-image:url(https://tracker.example/c.png);border-image:\
             url(https://tracker.example/d.png) 30;content:url(https://tracker.example/e.png);\
             cursor:url(https://tracker.example/f.cur),auto;text-align:center\">b</div>",
        );
        assert!(!clean.contains("tracker.example"), "{clean}");
        assert!(clean.contains("style=\"text-align:center\""), "{clean}");
        // The text and the rest of its formatting stay.
        let clean = sanitize_html(
            "<span style=\"background-image:url(https://tracker.example/a.png);color:#333;\
             font-family:Times New Roman, Times, serif\">c</span>",
        );
        assert!(!clean.contains("tracker.example"), "{clean}");
        assert!(clean.contains("color:#333"), "{clean}");
        assert!(clean.contains("font-family:Times New Roman"), "{clean}");
    }
}
