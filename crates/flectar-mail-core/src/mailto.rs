//! RFC 6068 `mailto:` URIs, from links in messages, other applications and
//! List-Unsubscribe headers.
//!
//! Any web page can hand the system a mailto link, so its content only ever
//! pre-fills a message the user reviews, and only in fields that cannot hurt:
//!
//! - Recipients, subject and body are taken, plus In-Reply-To so a reply link
//!   from a list archive threads. From, Reply-To, other headers and HTML parts
//!   are ignored, as RFC 6068 section 3 asks and as Thunderbird does.
//! - Attachment requests (`attach`, `attachment`) are never opened: links
//!   that attached local files leaked them from Thunderbird, Evolution and
//!   KMail (CVE-2020-11879, CVE-2020-11880). Like Thunderbird, Apple Mail and
//!   Outlook, the file is not attached; its name is kept so the composer can
//!   say what was asked for.
//! - Each part is percent-decoded exactly once and `+` stays a plus, which is
//!   common in addresses (RFC 6068 section 5). Form decoding would turn
//!   `list+unsubscribe@` into a space.
//! - Line breaks survive only in the body, so a link cannot add header lines.

use mail_parser::{HeaderValue, MessageParser};
use percent_encoding::percent_decode_str;
use std::collections::HashSet;

/// Longer links are refused rather than cut. Command lines stop at 32 KiB on
/// Windows and 128 KiB per argument on Linux; this leaves room for pasted
/// bodies while bounding what a page can make the composer load.
pub const MAX_URI_BYTES: usize = 1024 * 1024;

const SCHEME: &str = "mailto:";
/// RFC 5322 line length limit; a Message-ID never comes close.
const MAX_MESSAGE_ID_BYTES: usize = 998;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mailbox {
    pub name: Option<String>,
    pub email: String,
}

/// What a mailto link asks to pre-fill. Every field is already decoded and
/// safe to show; nothing here should be sent without the user's review.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Mailto {
    pub to: Vec<Mailbox>,
    pub cc: Vec<Mailbox>,
    pub bcc: Vec<Mailbox>,
    pub subject: Option<String>,
    pub body: Option<String>,
    /// The Message-ID being replied to, with angle brackets.
    pub in_reply_to: Option<String>,
    /// Files the link asked to attach, as written. Never opened.
    pub attachment_requests: Vec<String>,
}

/// Whether `value` is a mailto URI (scheme names are case-insensitive).
pub fn is_mailto(value: &str) -> bool {
    value
        .as_bytes()
        .get(..SCHEME.len())
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case(SCHEME.as_bytes()))
}

/// Parse a mailto URI. `None` when it is not one or is longer than
/// [`MAX_URI_BYTES`]; a bare `mailto:` is a valid, empty message.
pub fn parse(uri: &str) -> Option<Mailto> {
    let uri = uri.trim();
    if uri.len() > MAX_URI_BYTES || !is_mailto(uri) {
        return None;
    }
    // Some browsers have produced `mailto://address`; nothing valid starts
    // with a slash.
    let rest = uri[SCHEME.len()..].trim_start_matches('/');
    // RFC 6068 defines no fragment; `#` inside a value must be `%23`.
    let rest = rest.split_once('#').map_or(rest, |(rest, _)| rest);
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));

    let mut message = Mailto::default();
    let mut bodies = Vec::new();
    push_mailboxes(&mut message.to, &decode(path));
    for field in query.split('&') {
        // Links copied out of HTML sometimes keep the `&amp;` escape.
        let field = field.strip_prefix("amp;").unwrap_or(field);
        if field.is_empty() {
            continue;
        }
        let (name, value) = field.split_once('=').unwrap_or((field, ""));
        let value = decode(value);
        match decode(name).to_ascii_lowercase().as_str() {
            "to" => push_mailboxes(&mut message.to, &value),
            "cc" => push_mailboxes(&mut message.cc, &value),
            "bcc" => push_mailboxes(&mut message.bcc, &value),
            // Repeats are not allowed; like Thunderbird, the last one wins.
            "subject" => message.subject = Some(subject(&value)).filter(|s| !s.is_empty()),
            "body" => bodies.push(body(&value)),
            "in-reply-to" => {
                if let Some(id) = message_id(&value) {
                    message.in_reply_to = Some(id);
                }
            }
            "attach" | "attachment" => {
                let request = single_line(&value);
                if !request.is_empty() {
                    message.attachment_requests.push(request);
                }
            }
            _ => {}
        }
    }
    if !bodies.is_empty() {
        message.body = Some(bodies.join("\n"));
    }
    remove_repeated_recipients(&mut message);
    Some(message)
}

/// Percent-decode once. `+` is literal; malformed escapes stay as written and
/// bytes that are not UTF-8 become replacement characters.
fn decode(value: &str) -> String {
    percent_decode_str(value).decode_utf8_lossy().into_owned()
}

/// Control characters, including line breaks, become spaces.
fn single_line(value: &str) -> String {
    value
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_owned()
}

/// Body text keeps line breaks (as `\n`) and tabs, nothing else from C0/C1.
fn body(value: &str) -> String {
    value
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .chars()
        .filter(|&c| c == '\n' || c == '\t' || !c.is_control())
        .collect()
}

fn subject(value: &str) -> String {
    let value = single_line(value);
    if !value.contains("=?") {
        return value;
    }
    // RFC 2047 encoded words are allowed outside the body.
    let header = format!("Subject: {value}\r\n\r\n");
    MessageParser::default()
        .parse_headers(header.as_bytes())
        .and_then(|message| message.subject().map(single_line))
        .unwrap_or(value)
}

fn push_mailboxes(list: &mut Vec<Mailbox>, value: &str) {
    let value = single_line(value);
    if value.is_empty() {
        return;
    }
    // Plain address lists (the RFC 6068 form) split on commas, and on the
    // semicolons Outlook used. Anything with display names, comments or
    // encoded words goes through the RFC 5322 parser.
    if !value.contains(['"', '<', '(']) && !value.contains("=?") {
        list.extend(
            value
                .split([',', ';'])
                .filter_map(|email| mailbox(None, email)),
        );
        return;
    }
    let header = format!("To: {value}\r\n\r\n");
    let Some(message) = MessageParser::default().parse_headers(header.as_bytes()) else {
        return;
    };
    if let Some(HeaderValue::Address(addresses)) = message.header("To") {
        list.extend(
            addresses.iter().filter_map(|address| {
                mailbox(address.name.as_deref(), address.address.as_deref()?)
            }),
        );
    }
}

fn mailbox(name: Option<&str>, email: &str) -> Option<Mailbox> {
    let email = email.trim();
    let (local, domain) = email.rsplit_once('@')?;
    let valid = !local.is_empty()
        && !domain.is_empty()
        && !email
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || is_invisible_format(c));
    if !valid {
        return None;
    }
    let name = name
        .map(|name| {
            single_line(name)
                .chars()
                .filter(|&c| !is_invisible_format(c))
                .collect::<String>()
        })
        .filter(|name| !name.is_empty() && name != email);
    Some(Mailbox {
        name,
        email: email.to_owned(),
    })
}

/// Bidirectional overrides and other invisible format characters let an
/// address display as a different one (UTS #39 section 3.3 disallows them in
/// local parts). Such addresses are dropped; names lose the characters.
fn is_invisible_format(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{061C}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{FEFF}'
    )
}

/// The first `<id@domain>` in `value`. Bare ids get their brackets.
pub(crate) fn message_id(value: &str) -> Option<String> {
    let value = value.trim();
    let id = match value.strip_prefix('<') {
        Some(rest) => rest.split_once('>')?.0,
        None => value.split_whitespace().next()?,
    };
    let (left, right) = id.split_once('@')?;
    let valid = !left.is_empty()
        && !right.is_empty()
        && id.len() + 2 <= MAX_MESSAGE_ID_BYTES
        && id
            .bytes()
            .all(|b| b.is_ascii_graphic() && !matches!(b, b'<' | b'>'));
    valid.then(|| format!("<{id}>"))
}

/// Each address once, in the first field it appears in (To, then Cc, then
/// Bcc), as RFC 6068 allows.
fn remove_repeated_recipients(message: &mut Mailto) {
    let mut seen = HashSet::new();
    for list in [&mut message.to, &mut message.cc, &mut message.bcc] {
        list.retain(|mailbox| seen.insert(mailbox.email.to_lowercase()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn emails(list: &[Mailbox]) -> Vec<&str> {
        list.iter().map(|mailbox| mailbox.email.as_str()).collect()
    }

    #[test]
    fn rejects_other_schemes_and_oversized_links() {
        assert_eq!(parse("https://example.com"), None);
        assert_eq!(parse("mail:a@example.com"), None);
        let long = format!("mailto:a@example.com?body={}", "x".repeat(MAX_URI_BYTES));
        assert_eq!(parse(&long), None);
    }

    #[test]
    fn bare_scheme_is_an_empty_message() {
        assert_eq!(parse("mailto:"), Some(Mailto::default()));
        assert_eq!(parse("MAILTO:"), Some(Mailto::default()));
    }

    #[test]
    fn plus_signs_stay_literal() {
        let message = parse(
            "mailto:googlegroups-manage+123+unsubscribe@googlegroups.com\
             ?subject=1+1%3D2&body=a+b%2Bc&cc=me+list@example.com",
        )
        .unwrap();
        assert_eq!(
            emails(&message.to),
            ["googlegroups-manage+123+unsubscribe@googlegroups.com"]
        );
        assert_eq!(emails(&message.cc), ["me+list@example.com"]);
        assert_eq!(message.subject.as_deref(), Some("1+1=2"));
        assert_eq!(message.body.as_deref(), Some("a+b+c"));
    }

    #[test]
    fn decodes_each_part_once() {
        let message = parse("mailto:a@example.com?subject=100%2525%20sure").unwrap();
        assert_eq!(message.subject.as_deref(), Some("100%25 sure"));
    }

    #[test]
    fn decodes_utf8_and_keeps_malformed_escapes() {
        let message =
            parse("mailto:j%C3%BCrgen@b%C3%BCcher.example?subject=Gr%C3%BC%C3%9Fe%ZZ").unwrap();
        assert_eq!(emails(&message.to), ["jürgen@bücher.example"]);
        assert_eq!(message.subject.as_deref(), Some("Grüße%ZZ"));
        let message = parse("mailto:a@example.com?subject=%FFok").unwrap();
        assert_eq!(message.subject.as_deref(), Some("\u{FFFD}ok"));
    }

    #[test]
    fn field_names_are_case_insensitive() {
        let message =
            parse("mailto:?To=a@example.com&SUBJECT=Hi&Cc=b@example.com&BODY=x&bCc=c@example.com")
                .unwrap();
        assert_eq!(emails(&message.to), ["a@example.com"]);
        assert_eq!(emails(&message.cc), ["b@example.com"]);
        assert_eq!(emails(&message.bcc), ["c@example.com"]);
        assert_eq!(message.subject.as_deref(), Some("Hi"));
        assert_eq!(message.body.as_deref(), Some("x"));
    }

    #[test]
    fn merges_path_and_repeated_address_fields() {
        let message = parse(
            "mailto:a@example.com,b@example.com?to=c@example.com&to=d@example.com;e@example.com\
             &cc=f@example.com&cc=g@example.com",
        )
        .unwrap();
        assert_eq!(
            emails(&message.to),
            [
                "a@example.com",
                "b@example.com",
                "c@example.com",
                "d@example.com",
                "e@example.com"
            ]
        );
        assert_eq!(emails(&message.cc), ["f@example.com", "g@example.com"]);
    }

    #[test]
    fn repeated_recipients_stay_in_their_first_field() {
        let message = parse(
            "mailto:a@example.com?to=A@Example.com&cc=a@example.com,b@example.com\
             &bcc=b@example.com,c@example.com",
        )
        .unwrap();
        assert_eq!(emails(&message.to), ["a@example.com"]);
        assert_eq!(emails(&message.cc), ["b@example.com"]);
        assert_eq!(emails(&message.bcc), ["c@example.com"]);
    }

    #[test]
    fn display_names_and_encoded_words() {
        let message = parse(
            "mailto:?to=%22Doe%2C%20Jane%22%20%3Cjane@example.com%3E,bob@example.com\
             &cc=%3D%3Futf-8%3FQ%3FJ%3DC3%3DBCrgen%3F%3D%20%3Cj@example.com%3E\
             &subject=%3D%3Futf-8%3FQ%3FGr%3DC3%3DBC%3DC3%3D9Fe%3F%3D",
        )
        .unwrap();
        assert_eq!(
            message.to,
            [
                Mailbox {
                    name: Some("Doe, Jane".into()),
                    email: "jane@example.com".into()
                },
                Mailbox {
                    name: None,
                    email: "bob@example.com".into()
                },
            ]
        );
        assert_eq!(message.cc[0].name.as_deref(), Some("Jürgen"));
        assert_eq!(message.subject.as_deref(), Some("Grüße"));
    }

    #[test]
    fn line_breaks_only_survive_in_the_body() {
        let message = parse(
            "mailto:a@example.com%0D%0ABcc:%20x@evil.example\
             ?subject=Hi%0D%0ABcc:%20y@evil.example&body=one%0D%0Atwo%0Dthree%00",
        )
        .unwrap();
        // The address with a header break inside is not an address at all.
        assert!(message.bcc.is_empty());
        assert!(
            message
                .to
                .iter()
                .all(|mailbox| !mailbox.email.contains(['\r', '\n']))
        );
        assert_eq!(message.subject.as_deref(), Some("Hi  Bcc: y@evil.example"));
        assert_eq!(message.body.as_deref(), Some("one\ntwo\nthree"));
    }

    #[test]
    fn repeated_bodies_join_and_last_subject_wins() {
        let message = parse("mailto:?subject=one&body=a&subject=two&body=b").unwrap();
        assert_eq!(message.subject.as_deref(), Some("two"));
        assert_eq!(message.body.as_deref(), Some("a\nb"));
    }

    #[test]
    fn ignores_unsafe_and_unknown_fields() {
        let message = parse(
            "mailto:a@example.com?from=boss@example.com&reply-to=x@evil.example\
             &sender=x@evil.example&html-body=%3Cb%3Ehi%3C/b%3E&X-Priority=1\
             &content-type=text/html&keywords=k",
        )
        .unwrap();
        assert_eq!(
            message,
            Mailto {
                to: vec![Mailbox {
                    name: None,
                    email: "a@example.com".into()
                }],
                ..Mailto::default()
            }
        );
    }

    #[test]
    fn attachment_requests_are_listed_not_opened() {
        let message = parse(
            "mailto:a@example.com?attach=/home/me/.ssh/id_ed25519\
             &Attachment=file:///etc/passwd&attach=%5C%5Cevil.example%5Cshare&attach=",
        )
        .unwrap();
        assert_eq!(
            message.attachment_requests,
            [
                "/home/me/.ssh/id_ed25519",
                "file:///etc/passwd",
                "\\\\evil.example\\share"
            ]
        );
    }

    #[test]
    fn drops_addresses_with_invisible_format_characters() {
        let message =
            parse("mailto:admin%E2%80%AE@example.com,ok@example.com?cc=%E2%80%8Bx@example.com")
                .unwrap();
        assert_eq!(emails(&message.to), ["ok@example.com"]);
        assert!(message.cc.is_empty());
        let message = parse("mailto:?to=%22Al%E2%80%AEice%22%20%3Ca@example.com%3E").unwrap();
        assert_eq!(message.to[0].name.as_deref(), Some("Alice"));
    }

    #[test]
    fn drops_entries_that_are_not_addresses() {
        let message = parse("mailto:nobody,@example.com,a@,ok@example.com,").unwrap();
        assert_eq!(emails(&message.to), ["ok@example.com"]);
    }

    #[test]
    fn tolerates_slashes_fragments_and_escaped_ampersands() {
        let message =
            parse("mailto://a@example.com?subject=Hi&amp;body=There#to=x@evil.example").unwrap();
        assert_eq!(emails(&message.to), ["a@example.com"]);
        assert_eq!(message.subject.as_deref(), Some("Hi"));
        assert_eq!(message.body.as_deref(), Some("There"));
    }

    #[test]
    fn in_reply_to_takes_one_valid_message_id() {
        let id = |uri: &str| parse(uri).unwrap().in_reply_to;
        assert_eq!(
            id("mailto:list@example.com?In-Reply-To=%3C123.abc@mail.example%3E"),
            Some("<123.abc@mail.example>".into())
        );
        assert_eq!(
            id("mailto:?in-reply-to=123@mail.example"),
            Some("<123@mail.example>".into())
        );
        assert_eq!(
            id("mailto:?in-reply-to=%3Ca@b%3E%20%3Cc@d%3E"),
            Some("<a@b>".into())
        );
        assert_eq!(id("mailto:?in-reply-to=no-at-sign"), None);
        assert_eq!(id("mailto:?in-reply-to=%3Ca%20b@c%3E"), None);
        assert_eq!(id("mailto:?in-reply-to=%3Ca@b%0D%0ABcc:x%3E"), None);
    }
}
