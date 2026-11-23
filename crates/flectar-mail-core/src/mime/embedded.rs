//! Messages carried inside a message as message/rfc822 parts: a forward sent
//! whole, the message a bounce returns, the messages of a digest (#455). One
//! the sender didn't send as an attachment shows in the body, under its own
//! header lines, as RFC 2183 asks of an inline part and as Thunderbird, K-9
//! Mail, Roundcube and Evolution show it. It is a file as well, named after
//! its subject, and so is each file it carries.

use super::{collect_addrs, find_html_tag, html_document_section, raw_header, sanitize_html};
use crate::models::Address;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

/// How many messages deep the body shows messages inside messages. Deeper
/// ones stay files.
pub(crate) const MAX_DEPTH: usize = 8;

/// The longest name a message's file takes from its subject, in characters.
const MAX_NAME_CHARS: usize = 64;

/// What the reader shows above a message inside another: who wrote it, when,
/// about what, and to whom.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddedHeaders {
    #[serde(default)]
    pub from: Vec<Address>,
    #[serde(default)]
    pub to: Vec<Address>,
    #[serde(default)]
    pub cc: Vec<Address>,
    #[serde(default)]
    pub subject: String,
    /// When it was written, as an RFC 5322 date.
    #[serde(default)]
    pub date: Option<String>,
    /// The same moment in RFC 3339, which the reader shows the user's way.
    #[serde(default)]
    pub date_iso: Option<String>,
}

/// A message inside another that the body shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EmbeddedMessage {
    pub headers: EmbeddedHeaders,
    pub text: Option<String>,
    /// Sanitized.
    pub html: Option<String>,
    /// The messages it shows in turn.
    pub messages: Vec<EmbeddedMessage>,
}

/// Whether `media_type` is a whole message, as message/rfc822 and
/// message/global (RFC 6532) are.
pub fn is_message_type(media_type: &str) -> bool {
    let media = media_type.split(';').next().unwrap_or_default().trim();
    media.eq_ignore_ascii_case("message/rfc822") || media.eq_ignore_ascii_case("message/global")
}

/// Whether a part of `media_type` with this Content-Disposition type is a
/// message the body shows: one the sender didn't send as an attachment.
pub(crate) fn shows_in_body(media_type: &str, disposition: Option<&str>) -> bool {
    is_message_type(media_type)
        && !disposition.is_some_and(|value| value.trim().eq_ignore_ascii_case("attachment"))
}

/// The part of a file name that a message with `subject` takes: the subject
/// without the characters file systems refuse, on one line, at most 64
/// characters long. Empty when nothing of it is left.
pub fn message_file_stem(subject: &str) -> String {
    let mut stem = subject
        .chars()
        .map(|character| match character {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => ' ',
            character if character.is_control() => ' ',
            character => character,
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if stem.chars().count() > MAX_NAME_CHARS {
        stem = stem.chars().take(MAX_NAME_CHARS).collect::<String>();
    }
    stem.trim().trim_matches('.').trim().to_owned()
}

/// The file a message inside another is, as mail apps name one sent without
/// a name: its subject with `.eml`, as Thunderbird names it. None when it has
/// no subject to name it by.
pub fn message_file_name(subject: &str) -> Option<String> {
    let stem = message_file_stem(subject);
    (!stem.is_empty()).then(|| format!("{stem}.eml"))
}

impl EmbeddedHeaders {
    /// The header lines of the message inside `message`.
    pub(crate) fn of(message: &mail_parser::Message) -> Self {
        // A date that isn't one keeps the words it was written in.
        let date = message.date().filter(|date| date.is_valid());
        Self {
            from: collect_addrs(message.from()),
            to: collect_addrs(message.to()),
            cc: collect_addrs(message.cc()),
            subject: message.subject().unwrap_or_default().trim().to_owned(),
            date: date
                .map(mail_parser::DateTime::to_rfc822)
                .or_else(|| raw_header(message, "Date")),
            date_iso: date.map(mail_parser::DateTime::to_rfc3339),
        }
    }

    /// The lines the header shows, in the order a forward writes them, each
    /// by field and in English: the stored text says them so, and the reader
    /// says them in the user's language.
    fn lines(&self) -> Vec<(Field, String)> {
        let mut lines = Vec::with_capacity(5);
        let mut add = |field, value: String| {
            if !value.trim().is_empty() {
                lines.push((field, value));
            }
        };
        add(Field::From, addresses(&self.from));
        add(Field::Date, self.date.clone().unwrap_or_default());
        add(Field::Subject, self.subject.clone());
        add(Field::To, addresses(&self.to));
        add(Field::Cc, addresses(&self.cc));
        lines
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    From,
    Date,
    Subject,
    To,
    Cc,
}

impl Field {
    /// How the stored body and the reader's markup name the field.
    fn key(self) -> &'static str {
        match self {
            Self::From => "from",
            Self::Date => "date",
            Self::Subject => "subject",
            Self::To => "to",
            Self::Cc => "cc",
        }
    }

    fn english(self) -> &'static str {
        match self {
            Self::From => "From",
            Self::Date => "Date",
            Self::Subject => "Subject",
            Self::To => "To",
            Self::Cc => "Cc",
        }
    }
}

fn addresses(list: &[Address]) -> String {
    list.iter()
        .map(|address| match address.name.as_deref().map(str::trim) {
            Some(name) if !name.is_empty() && !name.eq_ignore_ascii_case(&address.email) => {
                format!("{name} <{}>", address.email)
            }
            _ => address.email.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The messages `message` carries that its body shows, in the order they
/// come.
pub(crate) fn shown_in(message: &mail_parser::Message, depth: usize) -> Vec<EmbeddedMessage> {
    if depth >= MAX_DEPTH {
        return Vec::new();
    }
    message
        .attachments()
        .filter(|part| shows_in_body(&part_media_type(part), disposition(part)))
        .filter_map(|part| part.message())
        .map(|inner| EmbeddedMessage {
            headers: EmbeddedHeaders::of(inner),
            text: inner.body_text(0).map(Cow::into_owned),
            html: inner.body_html(0).map(|html| sanitize_html(&html)),
            messages: shown_in(inner, depth + 1),
        })
        .collect()
}

pub(crate) fn part_media_type(part: &mail_parser::MessagePart) -> String {
    use mail_parser::MimeHeaders;
    part.content_type()
        .map(|content_type| match content_type.subtype() {
            Some(subtype) => format!("{}/{subtype}", content_type.ctype()),
            None => content_type.ctype().to_owned(),
        })
        .unwrap_or_default()
}

pub(crate) fn disposition<'a>(part: &'a mail_parser::MessagePart) -> Option<&'a str> {
    use mail_parser::MimeHeaders;
    part.content_disposition()
        .map(|disposition| disposition.ctype())
}

/// The messages a plan's body shows, nested as they are, from their header
/// lines and the text decoded for each: `texts[i]` holds the plain and HTML
/// sections of `planned[i]`.
pub(crate) fn planned_messages(
    planned: &[super::PlannedMessage],
    mut texts: Vec<(Vec<String>, Vec<String>)>,
) -> Vec<EmbeddedMessage> {
    fn inside(
        parent: Option<usize>,
        planned: &[super::PlannedMessage],
        texts: &mut [(Vec<String>, Vec<String>)],
    ) -> Vec<EmbeddedMessage> {
        let mut messages = Vec::new();
        for (index, message) in planned.iter().enumerate() {
            // A message comes after the one it is inside of, which also
            // keeps a damaged plan from nesting a message in itself.
            let parent_of = message.parent.map(usize::from);
            if parent_of != parent || parent.is_some_and(|parent| parent >= index) {
                continue;
            }
            let (plain, html) = texts.get_mut(index).map(std::mem::take).unwrap_or_default();
            messages.push(EmbeddedMessage {
                headers: message.headers.clone(),
                text: (!plain.is_empty()).then(|| plain.join("\n\n")),
                html: (!html.is_empty()).then(|| html.join("\n")),
                messages: inside(Some(index), planned, texts),
            });
        }
        messages
    }
    texts.resize(planned.len(), Default::default());
    inside(None, planned, &mut texts)
}

/// A message's body with the messages it shows below its own text, each
/// under its header lines (#455). Without such messages the body is as it
/// was.
pub(crate) fn with_messages(
    text: Option<String>,
    html: Option<String>,
    messages: &[EmbeddedMessage],
) -> (Option<String>, Option<String>) {
    if messages.is_empty() {
        return (text, html);
    }
    let mut plain = text
        .clone()
        .or_else(|| html.as_deref().map(html_text))
        .unwrap_or_default();
    let mut head = String::new();
    let mut blocks = String::new();
    for message in messages {
        append_text(&mut plain, message);
        append_html(&mut blocks, &mut head, message);
    }
    let html = match html.or_else(|| text.as_deref().map(text_html)) {
        Some(outer) => insert(&outer, &head, &blocks),
        None => format!("<html><head>{head}</head><body>{blocks}</body></html>"),
    };
    (Some(plain), Some(html))
}

fn html_text(html: &str) -> String {
    mail_parser::decoders::html::html_to_text(html)
}

/// Plain text as HTML, as the body of a plain message is.
pub(crate) fn text_html(text: &str) -> String {
    sanitize_html(&mail_parser::decoders::html::text_to_html(text))
}

/// The forward rule and header lines mail apps write above a forwarded
/// message's text, then its text.
fn append_text(out: &mut String, message: &EmbeddedMessage) {
    out.truncate(out.trim_end().len());
    if !out.is_empty() {
        out.push_str("\n\n");
    }
    out.push_str("---------- Forwarded message ----------");
    for (field, value) in message.headers.lines() {
        out.push('\n');
        out.push_str(field.english());
        out.push_str(": ");
        out.push_str(&value);
    }
    let body = message
        .text
        .clone()
        .or_else(|| message.html.as_deref().map(html_text))
        .unwrap_or_default();
    if !body.trim().is_empty() {
        out.push_str("\n\n");
        out.push_str(body.trim_end());
    }
    for inner in &message.messages {
        append_text(out, inner);
    }
}

/// A rule, the header lines, then the message's own body and the messages
/// it shows in turn. Each field names itself so the reader can say it in
/// the user's language and show the date the user's way; the words here are
/// what other readers of the stored body get. The body's stylesheets join
/// `head`, as a browser applies a stylesheet wherever it is.
fn append_html(blocks: &mut String, head: &mut String, message: &EmbeddedMessage) {
    blocks.push_str(
        "<div class=\"flectar-embedded-message\" style=\"margin:24px 0 0;border-top:1px solid #dadce0\">\
         <div class=\"flectar-embedded-headers\" style=\"padding:12px 0 16px;font-size:13px;line-height:20px;color:#5f6368\">",
    );
    for (field, value) in message.headers.lines() {
        blocks.push_str("<div class=\"flectar-embedded-field\" data-field=\"");
        blocks.push_str(field.key());
        blocks.push_str("\">");
        blocks.push_str(field.english());
        blocks.push_str(": <span class=\"flectar-embedded-value\" style=\"color:#202124\">");
        match message.headers.date_iso.as_deref() {
            Some(iso) if field == Field::Date => {
                blocks.push_str("<time datetime=\"");
                push_escaped(blocks, iso);
                blocks.push_str("\">");
                push_escaped(blocks, &value);
                blocks.push_str("</time>");
            }
            _ => push_escaped(blocks, &value),
        }
        blocks.push_str("</span></div>");
    }
    blocks.push_str("</div>");
    let body = message
        .html
        .clone()
        .or_else(|| message.text.as_deref().map(text_html));
    if let Some(body) = body {
        match html_document_section(&body, "body") {
            Some(section) => {
                if let Some(styles) = html_document_section(&body, "head") {
                    head.push_str(styles.content);
                }
                blocks.push_str("<div class=\"flectar-embedded-body\"");
                blocks.push_str(section.attributes);
                blocks.push('>');
                blocks.push_str(section.content);
            }
            None => {
                blocks.push_str("<div class=\"flectar-embedded-body\">");
                blocks.push_str(&body);
            }
        }
        blocks.push_str("</div>");
    }
    for inner in &message.messages {
        append_html(blocks, head, inner);
    }
    blocks.push_str("</div>");
}

/// `outer` with `head` joining its stylesheets and `blocks` at the end of
/// its body.
fn insert(outer: &str, head: &str, blocks: &str) -> String {
    const END: &str = "</body></html>";
    let mut out = String::with_capacity(outer.len() + head.len() + blocks.len());
    let (content, end) = match outer.strip_suffix(END) {
        Some(content) => (content, END),
        None => (outer, ""),
    };
    match find_html_tag(content, "head", 0, true).filter(|_| !head.is_empty()) {
        Some(at) => {
            out.push_str(&content[..at]);
            out.push_str(head);
            out.push_str(&content[at..]);
        }
        None => {
            out.push_str(head);
            out.push_str(content);
        }
    }
    out.push_str(blocks);
    out.push_str(end);
    out
}

fn push_escaped(out: &mut String, text: &str) {
    for character in text.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            character => out.push(character),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{
        MimePlan, TextSectionKind, extract_attachment, parse_message, plan_bodystructure,
    };
    use super::*;

    /// The message of #455, as its reporter imported it.
    const INLINE_FORWARD: &str = "From: sender@example.com\r\n\
        To: reader@example.com\r\n\
        Date: Wed, 18 Nov 2026 12:00:00 +0900\r\n\
        Subject: Inline forward reproduction\r\n\
        Message-ID: <inline-forward-repro@example.com>\r\n\
        MIME-Version: 1.0\r\n\
        Content-Type: multipart/mixed; boundary=repro\r\n\
        \r\n\
        --repro\r\n\
        Content-Type: text/plain\r\n\
        \r\n\
        Outer body.\r\n\
        --repro\r\n\
        Content-Type: message/rfc822\r\n\
        Content-Disposition: inline\r\n\
        \r\n\
        From: original@example.com\r\n\
        Date: Tue, 17 Nov 2026 12:00:00 +0900\r\n\
        Subject: Original message\r\n\
        \r\n\
        Forwarded body.\r\n\
        --repro--\r\n";

    fn forward(disposition: &str, inner: &str) -> String {
        format!(
            "From: sender@example.com\r\n\
             To: reader@example.com\r\n\
             Subject: Fwd\r\n\
             MIME-Version: 1.0\r\n\
             Content-Type: multipart/mixed; boundary=outer\r\n\
             \r\n\
             --outer\r\n\
             Content-Type: text/plain\r\n\
             \r\n\
             See below.\r\n\
             --outer\r\n\
             Content-Type: message/rfc822\r\n\
             {disposition}\
             \r\n\
             {inner}\r\n\
             --outer--\r\n"
        )
    }

    /// A forwarded message with an HTML body, an embedded image and a file.
    const RICH: &str = "From: \"Maya Patel\" <maya@example.com>\r\n\
        To: Alex <alex@example.com>, sam@example.com\r\n\
        Cc: =?utf-8?q?Jos=C3=A9?= <jose@example.com>\r\n\
        Date: Mon, 16 Nov 2026 09:30:00 +0000\r\n\
        Subject: =?utf-8?q?Q3_plan_=E2=80=94_draft?=\r\n\
        MIME-Version: 1.0\r\n\
        Content-Type: multipart/mixed; boundary=inner\r\n\
        \r\n\
        --inner\r\n\
        Content-Type: multipart/related; boundary=rel\r\n\
        \r\n\
        --rel\r\n\
        Content-Type: text/html; charset=utf-8\r\n\
        \r\n\
        <html><head><style>p.lead{color:#123456}</style></head>\
        <body style=\"background:#fafafa\"><p class=\"lead\">The plan</p>\
        <img src=\"cid:chart@example.com\"><script>bad()</script></body></html>\r\n\
        --rel\r\n\
        Content-Type: image/png\r\n\
        Content-ID: <chart@example.com>\r\n\
        Content-Disposition: inline\r\n\
        Content-Transfer-Encoding: base64\r\n\
        \r\n\
        iVBORw0KGgo=\r\n\
        --rel--\r\n\
        --inner\r\n\
        Content-Type: application/pdf; name=\"plan.pdf\"\r\n\
        Content-Disposition: attachment; filename=\"plan.pdf\"\r\n\
        Content-Transfer-Encoding: base64\r\n\
        \r\n\
        JVBERi0xLjQK\r\n\
        --inner\r\n\
        Content-Type: text/calendar; charset=utf-8; method=REQUEST\r\n\
        \r\n\
        BEGIN:VCALENDAR\r\n\
        METHOD:REQUEST\r\n\
        END:VCALENDAR\r\n\
        --inner--";

    #[test]
    fn issue_455_a_message_forwarded_inline_shows_in_the_body_and_as_a_file() {
        let parsed = parse_message(INLINE_FORWARD.as_bytes()).unwrap();

        let text = parsed.text.as_deref().unwrap();
        assert_eq!(
            text,
            "Outer body.\n\n\
             ---------- Forwarded message ----------\n\
             From: original@example.com\n\
             Date: Tue, 17 Nov 2026 12:00:00 +0900\n\
             Subject: Original message\n\n\
             Forwarded body."
        );
        let html = parsed.html.as_deref().unwrap();
        let outer = html.find("Outer body.").unwrap();
        let block = html
            .find("<div class=\"flectar-embedded-message\"")
            .unwrap();
        let body = html.find("Forwarded body.").unwrap();
        assert!(outer < block && block < body, "{html}");
        assert!(html.contains(
            "<div class=\"flectar-embedded-field\" data-field=\"subject\">Subject: \
             <span class=\"flectar-embedded-value\" style=\"color:#202124\">Original message</span></div>"
        ));
        assert!(html.contains(
            "<time datetime=\"2026-11-17T12:00:00+09:00\">Tue, 17 Nov 2026 12:00:00 +0900</time>"
        ));
        assert!(html.ends_with("</body></html>"), "{html}");
        assert!(parsed.snippet.starts_with("Outer body."));

        let [file] = parsed.attachments.as_slice() else {
            panic!("{:?}", parsed.attachments);
        };
        assert_eq!(file.mime_type.as_deref(), Some("message/rfc822"));
        assert_eq!(file.filename.as_deref(), Some("Original message.eml"));
        assert!(file.is_inline);
        let (bytes, declared) = extract_attachment(INLINE_FORWARD.as_bytes(), "0").unwrap();
        assert!(declared.is_none());
        assert!(
            String::from_utf8(bytes)
                .unwrap()
                .contains("Subject: Original message")
        );
    }

    #[test]
    fn a_message_sent_as_an_attachment_stays_a_file() {
        let raw = forward(
            "Content-Disposition: attachment\r\n",
            "From: original@example.com\r\nSubject: Original message\r\n\r\nForwarded body.",
        );
        let parsed = parse_message(raw.as_bytes()).unwrap();
        assert_eq!(parsed.text.as_deref(), Some("See below."));
        assert!(!parsed.html.unwrap().contains("Forwarded body."));
        let [file] = parsed.attachments.as_slice() else {
            panic!("{:?}", parsed.attachments);
        };
        assert_eq!(file.filename.as_deref(), Some("Original message.eml"));
        assert!(!file.is_inline);

        // The name the sender gave it wins over the subject.
        let raw = forward(
            "Content-Disposition: attachment; filename=\"thread.eml\"\r\n",
            "Subject: Original message\r\n\r\nForwarded body.",
        );
        let parsed = parse_message(raw.as_bytes()).unwrap();
        assert_eq!(
            parsed.attachments[0].filename.as_deref(),
            Some("thread.eml")
        );
    }

    #[test]
    fn a_message_without_a_disposition_shows_as_a_digest_or_bounce_carries_it() {
        let raw = forward("", "Subject: Returned\r\n\r\nThe original.");
        let parsed = parse_message(raw.as_bytes()).unwrap();
        assert!(
            parsed
                .text
                .unwrap()
                .ends_with("Subject: Returned\n\nThe original.")
        );
    }

    #[test]
    fn a_forwarded_message_shows_its_html_and_lists_its_files() {
        let raw = forward("Content-Disposition: inline\r\n", RICH);
        let parsed = parse_message(raw.as_bytes()).unwrap();

        let html = parsed.html.as_deref().unwrap();
        // Its stylesheet joins the message's, its body keeps its own look,
        // and what the sanitizer removes stays removed.
        let head_end = html.find("</head>").unwrap();
        assert!(html[..head_end].contains("p.lead{color:#123456}"), "{html}");
        assert!(html.contains(
            "<div class=\"flectar-embedded-body\" style=\"background:#fafafa\"><p class=\"lead\">The plan</p>"
        ));
        assert!(html.contains("src=\"cid:chart@example.com\""));
        assert!(!html.contains("bad()"));
        assert!(html.contains("Maya Patel &lt;maya@example.com&gt;"));
        assert!(html.contains("Alex &lt;alex@example.com&gt;, sam@example.com"));
        assert!(html.contains(">José &lt;jose@example.com&gt;<"));
        assert!(html.contains(">Q3 plan — draft<"));
        let text = parsed.text.as_deref().unwrap();
        assert!(
            text.contains("Cc: José <jose@example.com>\n\nThe plan"),
            "{text}"
        );

        let files: Vec<_> = parsed
            .attachments
            .iter()
            .map(|file| {
                (
                    file.part_id.as_str(),
                    file.filename.as_deref(),
                    file.mime_type.as_deref(),
                    file.is_inline,
                )
            })
            .collect();
        assert_eq!(
            files,
            vec![
                (
                    "0",
                    Some("Q3 plan — draft.eml"),
                    Some("message/rfc822"),
                    true
                ),
                ("0.0", None, Some("image/png"), true),
                ("0.1", Some("plan.pdf"), Some("application/pdf"), false),
                ("0.2", None, Some("text/calendar"), false),
            ]
        );
        assert_eq!(
            parsed.attachments[1].content_id.as_deref(),
            Some("chart@example.com")
        );
        // Its invitation is the sender's, not one to the reader.
        assert!(parsed.calendar_parts.is_empty());
        assert_eq!(
            extract_attachment(raw.as_bytes(), "0.1").unwrap(),
            (b"%PDF-1.4\n".to_vec(), Some("plan.pdf".to_owned()))
        );
        assert!(extract_attachment(raw.as_bytes(), "0.9").is_err());
        assert!(extract_attachment(raw.as_bytes(), "0.").is_err());
    }

    #[test]
    fn messages_inside_messages_show_nested_up_to_a_depth() {
        let mut raw = "Subject: Deepest\r\n\r\nBottom.".to_owned();
        for level in (0..=MAX_DEPTH).rev() {
            raw = format!(
                "Subject: Level {level}\r\n\
                 MIME-Version: 1.0\r\n\
                 Content-Type: multipart/mixed; boundary=b{level}\r\n\
                 \r\n\
                 --b{level}\r\n\
                 Content-Type: text/plain\r\n\
                 \r\n\
                 Text {level}.\r\n\
                 --b{level}\r\n\
                 Content-Type: message/rfc822\r\n\
                 \r\n\
                 {raw}\r\n\
                 --b{level}--\r\n"
            );
        }
        let parsed = parse_message(raw.as_bytes()).unwrap();
        let text = parsed.text.unwrap();
        assert_eq!(
            text.matches("---------- Forwarded message").count(),
            MAX_DEPTH
        );
        assert!(text.contains(&format!("Text {}.", MAX_DEPTH - 1)));
        assert!(!text.contains("Bottom."));
        let html = parsed.html.unwrap();
        assert_eq!(
            html.matches("<div class=\"flectar-embedded-message\"")
                .count(),
            MAX_DEPTH
        );
        // The deepest message shown lists the message it carries as a file.
        assert_eq!(
            parsed.attachments.last().unwrap().part_id,
            ["0"; MAX_DEPTH + 1].join(".")
        );
    }

    #[test]
    fn a_message_of_only_a_forward_shows_it() {
        let raw = "From: sender@example.com\r\n\
                   Subject: Fwd\r\n\
                   MIME-Version: 1.0\r\n\
                   Content-Type: message/rfc822\r\n\
                   \r\n\
                   Subject: Original\r\n\r\nOnly the original.\r\n";
        let parsed = parse_message(raw.as_bytes()).unwrap();
        assert_eq!(
            parsed.text.as_deref(),
            Some(
                "---------- Forwarded message ----------\nSubject: Original\n\nOnly the original."
            )
        );
        let html = parsed.html.unwrap();
        assert!(
            html.starts_with("<html><head></head><body><div class=\"flectar-embedded-message\"")
        );
        assert!(html.contains("Only the original."));
    }

    #[test]
    fn file_names_come_from_subjects() {
        assert_eq!(
            message_file_name("Re: Q3 / report: final?").as_deref(),
            Some("Re Q3 report final.eml")
        );
        assert_eq!(message_file_name("  ...  "), None);
        assert_eq!(message_file_name(""), None);
        assert_eq!(message_file_stem(&"a".repeat(80)).chars().count(), 64);
    }

    /// The BODYSTRUCTURE Dovecot reports for `forward("Content-Disposition:
    /// inline", RICH)`, by the syntax of RFC 3501 section 7.4.2.
    const RICH_BODYSTRUCTURE: &[u8] = b"* 1 FETCH (UID 7 BODYSTRUCTURE ((\"text\" \"plain\" (\"charset\" \"us-ascii\") NIL NIL \"7bit\" 10 1 NIL NIL NIL NIL)\
        (\"message\" \"rfc822\" NIL NIL NIL \"7bit\" 1200 (\"Mon, 16 Nov 2026 09:30:00 +0000\" \"=?utf-8?q?Q3_plan_=E2=80=94_draft?=\" \
        ((\"Maya Patel\" NIL \"maya\" \"example.com\")) ((\"Maya Patel\" NIL \"maya\" \"example.com\")) ((\"Maya Patel\" NIL \"maya\" \"example.com\")) \
        ((\"Alex\" NIL \"alex\" \"example.com\")(NIL NIL \"sam\" \"example.com\")) ((\"=?utf-8?q?Jos=C3=A9?=\" NIL \"jose\" \"example.com\")) NIL NIL NIL) \
        (((\"text\" \"html\" (\"charset\" \"utf-8\") NIL NIL \"7bit\" 160 1 NIL NIL NIL NIL)\
        (\"image\" \"png\" NIL \"<chart@example.com>\" NIL \"base64\" 12 NIL (\"inline\" NIL) NIL NIL) \"related\" (\"boundary\" \"rel\") NIL NIL NIL)\
        (\"application\" \"pdf\" (\"name\" \"plan.pdf\") NIL NIL \"base64\" 12 NIL (\"attachment\" (\"filename\" \"plan.pdf\")) NIL NIL)\
        (\"text\" \"calendar\" (\"charset\" \"utf-8\" \"method\" \"REQUEST\") NIL NIL \"7bit\" 40 3 NIL NIL NIL NIL) \"mixed\" (\"boundary\" \"inner\") NIL NIL NIL) \
        30 NIL (\"inline\" NIL) NIL NIL) \"mixed\" (\"boundary\" \"outer\") NIL NIL NIL))\r\n";

    fn plan_of(response: &[u8]) -> MimePlan {
        use async_imap::imap_proto::{AttributeValue, Response};
        let (_, Response::Fetch(_, attributes)) = Response::parse(response).unwrap() else {
            panic!("not a FETCH response");
        };
        attributes
            .iter()
            .find_map(|attribute| match attribute {
                AttributeValue::BodyStructure(structure) => Some(plan_bodystructure(structure)),
                _ => None,
            })
            .unwrap()
    }

    #[test]
    fn issue_455_the_imap_plan_reads_a_forwarded_message_as_the_body() {
        let plan = plan_of(RICH_BODYSTRUCTURE);

        let sections: Vec<_> = plan
            .text_sections
            .iter()
            .map(|part| (part.section.as_str(), part.kind, part.message))
            .collect();
        assert_eq!(
            sections,
            vec![
                ("1", TextSectionKind::Plain, None),
                ("2.1.1", TextSectionKind::Html, Some(0)),
            ]
        );
        let files: Vec<_> = plan
            .attachments
            .iter()
            .map(|part| {
                (
                    part.section.as_str(),
                    part.filename.as_deref(),
                    part.is_inline,
                    part.message,
                )
            })
            .collect();
        assert_eq!(
            files,
            vec![
                ("2", Some("Q3 plan — draft.eml"), true, None),
                ("2.1.2", None, true, Some(0)),
                ("2.2", Some("plan.pdf"), false, Some(0)),
                ("2.3", None, false, Some(0)),
            ]
        );
        assert!(plan.has_file_attachments());
        assert_eq!(plan.encoded_size(), 1210);

        let [message] = plan.messages.as_slice() else {
            panic!("{:?}", plan.messages);
        };
        assert_eq!(message.section, "2");
        assert_eq!(message.parent, None);
        assert_eq!(
            message.headers,
            EmbeddedHeaders {
                from: vec![Address {
                    name: Some("Maya Patel".into()),
                    email: "maya@example.com".into(),
                }],
                to: vec![
                    Address {
                        name: Some("Alex".into()),
                        email: "alex@example.com".into(),
                    },
                    Address {
                        name: None,
                        email: "sam@example.com".into(),
                    },
                ],
                cc: vec![Address {
                    name: Some("José".into()),
                    email: "jose@example.com".into(),
                }],
                subject: "Q3 plan — draft".into(),
                date: Some("Mon, 16 Nov 2026 09:30:00 +0000".into()),
                date_iso: Some("2026-11-16T09:30:00Z".into()),
            }
        );

        // A stored plan of this version reads back the same.
        let json = serde_json::to_string(&plan).unwrap();
        assert_eq!(serde_json::from_str::<MimePlan>(&json).unwrap(), plan);
    }

    #[test]
    fn the_imap_plan_keeps_a_message_sent_as_an_attachment_whole() {
        let response = b"* 1 FETCH (UID 7 BODYSTRUCTURE ((\"text\" \"plain\" NIL NIL NIL \"7bit\" 10 1 NIL NIL NIL NIL)\
            (\"message\" \"rfc822\" NIL NIL NIL \"7bit\" 300 (NIL \"Original\" NIL NIL NIL NIL NIL NIL NIL NIL) \
            (\"text\" \"plain\" NIL NIL NIL \"7bit\" 16 1 NIL NIL NIL NIL) 6 NIL (\"attachment\" NIL) NIL NIL) \"mixed\" (\"boundary\" \"b\") NIL NIL NIL))\r\n";
        let plan = plan_of(response);
        assert!(plan.messages.is_empty());
        assert_eq!(plan.text_sections.len(), 1);
        assert_eq!(plan.attachments.len(), 1);
        assert_eq!(
            plan.attachments[0].filename.as_deref(),
            Some("Original.eml")
        );
        assert!(!plan.attachments[0].is_inline);
    }

    #[test]
    fn the_imap_plan_numbers_a_single_part_forward_below_it() {
        let response = b"* 1 FETCH (UID 7 BODYSTRUCTURE ((\"text\" \"plain\" NIL NIL NIL \"7bit\" 10 1 NIL NIL NIL NIL)\
            (\"message\" \"rfc822\" NIL NIL NIL \"7bit\" 300 (\"Tue, 17 Nov 2026 12:00:00 +0900\" \"Original message\" ((NIL NIL \"original\" \"example.com\")) NIL NIL NIL NIL NIL NIL NIL) \
            (\"text\" \"plain\" NIL NIL NIL \"7bit\" 16 1 NIL NIL NIL NIL) 6 NIL (\"inline\" NIL) NIL NIL) \"mixed\" (\"boundary\" \"b\") NIL NIL NIL))\r\n";
        let plan = plan_of(response);
        assert_eq!(plan.text_sections[1].section, "2.1");
        assert_eq!(plan.text_sections[1].message, Some(0));
        assert_eq!(plan.messages[0].section, "2");
    }

    #[test]
    fn a_plan_body_nests_the_messages_it_shows() {
        let headers = |subject: &str| EmbeddedHeaders {
            subject: subject.into(),
            ..EmbeddedHeaders::default()
        };
        let planned = vec![
            super::super::PlannedMessage {
                section: "2".into(),
                parent: None,
                headers: headers("Outer forward"),
            },
            super::super::PlannedMessage {
                section: "2.2".into(),
                parent: Some(0),
                headers: headers("Inner forward"),
            },
            // A damaged plan can't nest a message in itself.
            super::super::PlannedMessage {
                section: "3".into(),
                parent: Some(2),
                headers: headers("Lost"),
            },
        ];
        let messages = planned_messages(
            &planned,
            vec![(vec!["First.".into(), "Second.".into()], Vec::new())],
        );
        let [outer] = messages.as_slice() else {
            panic!("{messages:?}");
        };
        assert_eq!(outer.headers.subject, "Outer forward");
        assert_eq!(outer.text.as_deref(), Some("First.\n\nSecond."));
        assert_eq!(outer.messages.len(), 1);
        assert_eq!(outer.messages[0].headers.subject, "Inner forward");
        assert_eq!(outer.messages[0].text, None);

        let (text, html) = with_messages(Some("Own.".into()), None, &messages);
        assert_eq!(
            text.as_deref(),
            Some(
                "Own.\n\n---------- Forwarded message ----------\nSubject: Outer forward\n\n\
                 First.\n\nSecond.\n\n---------- Forwarded message ----------\nSubject: Inner forward"
            )
        );
        let html = html.unwrap();
        assert!(html.starts_with("<html><head></head><body>Own."), "{html}");
        // The inner message sits inside the outer one.
        let outer_end = html.rfind("</div></div></body>").unwrap();
        assert!(html.find("Inner forward").unwrap() < outer_end);
    }

    #[test]
    fn a_body_without_messages_is_left_as_it_is() {
        assert_eq!(
            with_messages(Some("a".into()), Some("<p>a</p>".into()), &[]),
            (Some("a".into()), Some("<p>a</p>".into()))
        );
    }
}
