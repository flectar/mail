//! Plain text as the reader shows it (#544): quote levels as bars beside
//! the quoted text, links that open, the signature set back, and a
//! fixed-width font if chosen. Also how messages show when they open, and
//! the ones the user switched from that.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;

use flectar_mail_core::mime::flowed::quote_marks;
use flectar_mail_core::models::MessageView;
use linkify::{LinkFinder, LinkKind};
use slint::ComponentHandle;

use crate::email_document::append_escaped;
use crate::mail::MailMessage;
use crate::reader_host::ReaderRef;
use crate::{AppWindow, InboxState};

/// The Usenet signature separator. The lines after it are the signature.
const SIGNATURE_SEPARATOR: &str = "-- ";
/// The bar color of each quote level, from the first. Deeper levels take
/// the last.
const LEVEL_COLORS: [&str; 5] = ["#729fcf", "#ad7fa8", "#8ae234", "#fcaf3e", "#e9b96e"];
/// Text in the font the message reads in, or in the fixed-width one, at the
/// same size: a plain `monospace` would make it smaller, as in browsers. Each
/// line is a block of its own, so that an empty one keeps its height in any
/// engine, where a newline at the edge of a `pre` would not.
const STYLE: &str = "body{margin:0;padding:32px;line-height:1.6;color:#303348;\
font-family:Arial,sans-serif;white-space:pre-wrap;overflow-wrap:anywhere}\
body.fixed{font-family:monospace,monospace}\
blockquote{margin:.4em 0;padding-inline-start:1ex;border-inline-start:2px solid}\
.signature{opacity:.6}";

/// How plain text looks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Look {
    pub(crate) fixed_width: bool,
    pub(crate) right_to_left: bool,
}

/// `text` as an HTML document for the reader. Each line is a `div` that wraps
/// with the window, and an empty one holds a `br`. A run of quoted lines goes
/// in a `blockquote` for each level, without its quote marks. Web and email
/// addresses link.
pub(crate) fn document(text: &str, look: Look) -> String {
    let mut html = String::with_capacity(text.len() + text.len() / 4 + 1024);
    html.push_str("<html><head><style>");
    html.push_str(STYLE);
    let mut nested = String::from("blockquote");
    for color in LEVEL_COLORS {
        html.push_str(&nested);
        html.push_str("{border-color:");
        html.push_str(color);
        html.push('}');
        nested.push_str(" blockquote");
    }
    html.push_str("</style></head><body class=\"plain");
    if look.fixed_width {
        html.push_str(" fixed");
    }
    html.push('"');
    if look.right_to_left {
        html.push_str(" dir=\"rtl\"");
    }
    html.push('>');
    let mut depth = 0;
    let mut in_signature = false;
    for line in text.lines() {
        if !in_signature && line == SIGNATURE_SEPARATOR {
            close_quotes(&mut html, &mut depth, 0);
            html.push_str("<div class=\"signature\">");
            in_signature = true;
        }
        let (level, content) = if in_signature {
            (0, line)
        } else {
            quote_marks(line)
        };
        close_quotes(&mut html, &mut depth, level);
        while depth < level {
            html.push_str("<blockquote>");
            depth += 1;
        }
        if content.is_empty() {
            html.push_str("<div><br></div>");
        } else {
            html.push_str("<div>");
            push_linked(&mut html, content);
            html.push_str("</div>");
        }
    }
    close_quotes(&mut html, &mut depth, 0);
    if in_signature {
        html.push_str("</div>");
    }
    html.push_str("</body></html>");
    html
}

/// Ends the quote levels deeper than `level`.
fn close_quotes(html: &mut String, depth: &mut usize, level: usize) {
    while *depth > level {
        html.push_str("</blockquote>");
        *depth -= 1;
    }
}

/// `text`, escaped, with each web address and email address a link. An
/// address without a scheme links only when it starts with "www.", so names
/// such as "file.txt" stay text.
fn push_linked(html: &mut String, text: &str) {
    let mut finder = LinkFinder::new();
    finder.url_must_have_scheme(false);
    let mut copied = 0;
    for link in finder.links(text) {
        let Some(href) = href(link.as_str(), link.kind()) else {
            continue;
        };
        append_escaped(html, &text[copied..link.start()]);
        html.push_str("<a href=\"");
        append_escaped(html, &href);
        html.push_str("\">");
        append_escaped(html, link.as_str());
        html.push_str("</a>");
        copied = link.end();
    }
    append_escaped(html, &text[copied..]);
}

fn href(link: &str, kind: &LinkKind) -> Option<String> {
    let lower = link.to_ascii_lowercase();
    match kind {
        LinkKind::Email => Some(format!("mailto:{link}")),
        LinkKind::Url if lower.starts_with("https://") || lower.starts_with("http://") => {
            Some(link.to_owned())
        }
        LinkKind::Url if lower.starts_with("www.") => Some(format!("https://{link}")),
        _ => None,
    }
}

struct Env {
    app: slint::Weak<AppWindow>,
    state: Rc<RefCell<InboxState>>,
    runtime: tokio::runtime::Handle,
}

thread_local! {
    /// What showing a message again takes.
    static ENV: RefCell<Option<Env>> = const { RefCell::new(None) };
    /// How messages show when they open, and whether plain text is fixed
    /// width, from the settings.
    static SETTINGS: Cell<(MessageView, bool)> =
        const { Cell::new((MessageView::Original, false)) };
    /// The messages the user switched from the way messages show by
    /// default, for this session.
    static SWITCHED: RefCell<HashSet<i32>> = RefCell::new(HashSet::new());
}

pub(super) fn install(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Handle,
) {
    ENV.set(Some(Env {
        app: app.as_weak(),
        state: Rc::clone(state),
        runtime: runtime.clone(),
    }));
}

/// Takes the reading settings, and says whether they changed.
fn set_settings(view: MessageView, fixed_width: bool) -> bool {
    SETTINGS.replace((view, fixed_width)) != (view, fixed_width)
}

/// Takes the reading settings, and shows the open messages again in their
/// light when they changed.
pub(crate) fn apply_settings(view: MessageView, fixed_width: bool) {
    if !set_settings(view, fixed_width) {
        return;
    }
    ENV.with_borrow(|env| {
        let Some(env) = env else { return };
        let Some(app) = env.app.upgrade() else { return };
        crate::reader_host::for_each_reader(&app, |reader| {
            crate::reader_host::show_again(reader, &env.state, &env.runtime);
        });
    });
}

/// Switches the message `reader` shows between its plain text and its HTML.
pub(crate) fn switch_shown(reader: &ReaderRef) {
    switch(reader.email().get_message_id());
    ENV.with_borrow(|env| {
        let Some(env) = env else { return };
        crate::reader_host::show_again(reader, &env.state, &env.runtime);
    });
}

/// How plain text looks, as the settings have it.
pub(crate) fn look() -> Look {
    Look {
        fixed_width: SETTINGS.get().1,
        right_to_left: false,
    }
}

/// Whether `message` shows as plain text: mail without HTML always does,
/// and other mail when that's how messages show, unless the user switched it.
pub(crate) fn shows_plain(message: &MailMessage) -> bool {
    !message.html_part || ((SETTINGS.get().0 == MessageView::PlainText) != switched(message.id))
}

/// Whether `message` opens in Reader mode, as messages with HTML do when
/// that's how messages show.
pub(crate) fn opens_in_reader_mode(message: &MailMessage) -> bool {
    message.html_part && SETTINGS.get().0 == MessageView::Reader
}

fn switched(message_id: i32) -> bool {
    SWITCHED.with_borrow(|switched| switched.contains(&message_id))
}

/// Switches `message_id` between its plain text and its HTML.
fn switch(message_id: i32) {
    SWITCHED.with_borrow_mut(|switched| {
        if !switched.remove(&message_id) {
            switched.insert(message_id);
        }
    });
}

/// The document `message` shows when it shows as plain text: the text its
/// sender wrote, or else the text of its HTML. None while its body loads.
pub(crate) fn shown_html(message: &MailMessage) -> Option<String> {
    if !shows_plain(message) {
        return None;
    }
    let text = flectar_mail_core::mime::body_text(
        message
            .text
            .as_deref()
            .filter(|text| !text.trim().is_empty()),
        message.html.as_deref().filter(|_| message.html_part),
    )?;
    Some(document(&text, look()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(html: &str) -> &str {
        let start = html.find("<body").unwrap();
        let start = start + html[start..].find('>').unwrap() + 1;
        &html[start..html.rfind("</body>").unwrap()]
    }

    #[test]
    fn keeps_lines_and_escapes_them() {
        let html = document("Hi <team>,\n\n  a & b\n", Look::default());
        assert_eq!(
            body(&html),
            "<div>Hi &lt;team&gt;,</div><div><br></div><div>  a &amp; b</div>"
        );
        assert!(html.contains("<body class=\"plain\">"));
    }

    #[test]
    fn puts_each_quote_level_in_a_blockquote_without_its_marks() {
        let html = document(
            "On Monday, Pat wrote:\n> Hi\n> > Earlier\n>> Still earlier\n> Back\nMine",
            Look::default(),
        );
        assert_eq!(
            body(&html),
            "<div>On Monday, Pat wrote:</div>\
             <blockquote><div>Hi</div>\
             <blockquote><div>Earlier</div><div>Still earlier</div></blockquote>\
             <div>Back</div></blockquote>\
             <div>Mine</div>"
        );
        // Empty lines keep their place, at the edge of a quote too.
        let html = document("A\n> B\n>\n> > C\n\nD", Look::default());
        assert_eq!(
            body(&html),
            "<div>A</div><blockquote><div>B</div><div><br></div>\
             <blockquote><div>C</div></blockquote></blockquote>\
             <div><br></div><div>D</div>"
        );
        // Each level has its own bar color.
        assert!(html.contains("blockquote blockquote{border-color:#ad7fa8}"));
    }

    #[test]
    fn sets_the_signature_apart() {
        let html = document("Thanks\n-- \nPat\n> not a quote", Look::default());
        assert_eq!(
            body(&html),
            "<div>Thanks</div><div class=\"signature\"><div>-- </div><div>Pat</div>\
             <div>&gt; not a quote</div></div>"
        );
    }

    #[test]
    fn links_web_and_email_addresses() {
        let html = document(
            "See https://example.com/a?b=1&c=2, www.example.org or (pat@example.com). Not file.txt.",
            Look::default(),
        );
        assert_eq!(
            body(&html),
            "<div>See <a href=\"https://example.com/a?b=1&amp;c=2\">https://example.com/a?b=1&amp;c=2</a>, \
             <a href=\"https://www.example.org\">www.example.org</a> or \
             (<a href=\"mailto:pat@example.com\">pat@example.com</a>). Not file.txt.</div>"
        );
        // Only web addresses link, whatever else is written as a link.
        let html = document("javascript:alert(1) ftp://example.com", Look::default());
        assert!(!html.contains("<a "), "{html}");
    }

    #[test]
    fn can_be_fixed_width_and_right_to_left() {
        let html = document(
            "שלום",
            Look {
                fixed_width: true,
                right_to_left: true,
            },
        );
        assert!(html.contains("<body class=\"plain fixed\" dir=\"rtl\">"));
        assert!(html.contains("body.fixed{font-family:monospace,monospace}"));
    }
}
