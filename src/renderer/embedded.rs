//! The header lines of a message forwarded inside the one the reader shows
//! (#455). The stored body writes each line in English, for whatever else
//! reads it, and names the line's field. The reader says the lines in the UI
//! language and shows the date the user's way, as it does for the message's
//! own header.

use blitz_dom::{BaseDocument, LocalName, Node, NodeId, local_name};
use std::{cell::RefCell, rc::Rc};

/// Where a translated header line puts the field's value.
pub(crate) const HEADER_VALUE: &str = "\u{e000}";

/// The header lines, by field and as the stored body writes them.
const LINES: [(&str, &str); 5] = [
    ("from", "From: {}"),
    ("date", "Date: {}"),
    ("subject", "Subject: {}"),
    ("to", "To: {}"),
    ("cc", "Cc: {}"),
];

/// A header line in the UI language: the words before and after its value.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Line {
    before: Rc<str>,
    after: Rc<str>,
}

impl Line {
    /// The line `translated` says, with [`HEADER_VALUE`] where the value goes.
    fn of(translated: &str) -> Self {
        let (before, after) = translated
            .split_once(HEADER_VALUE)
            .unwrap_or((translated, ""));
        Self {
            before: Rc::from(before),
            after: Rc::from(after),
        }
    }
}

thread_local! {
    /// The header lines, by field, in the UI language.
    static HEADER_LINES: RefCell<Rc<[(&'static str, Line)]>> = RefCell::new(
        LINES
            .iter()
            .map(|(field, line)| (*field, Line::of(&line.replace("{}", HEADER_VALUE))))
            .collect(),
    );
}

/// A forwarded message's header lines in the UI language, each with
/// [`HEADER_VALUE`] where its value goes.
pub(crate) struct HeaderLines {
    pub(crate) from: String,
    pub(crate) date: String,
    pub(crate) subject: String,
    pub(crate) to: String,
    pub(crate) cc: String,
}

/// The UI language changed. Messages read from now on say the header lines
/// of a message forwarded inside them as `lines` do.
pub(crate) fn set_header_lines(lines: HeaderLines) {
    let lines = [
        ("from", lines.from),
        ("date", lines.date),
        ("subject", lines.subject),
        ("to", lines.to),
        ("cc", lines.cc),
    ]
    .iter()
    .map(|(field, line)| (*field, Line::of(line)))
    .collect();
    HEADER_LINES.with(|current| *current.borrow_mut() = lines);
}

fn has_class(node: &Node, name: &str) -> bool {
    node.data
        .attr(local_name!("class"))
        .is_some_and(|classes| classes.split_ascii_whitespace().any(|class| class == name))
}

/// Say each header line of a forwarded message in the UI language, and its
/// date the user's way.
pub(super) fn prepare(document: &mut BaseDocument) {
    let field = LocalName::from("data-field");
    let mut fields = Vec::new();
    document.visit(|id, node| {
        if has_class(node, "flectar-embedded-field")
            && let Some(name) = node.data.attr(field.clone())
        {
            fields.push((id, name.to_owned()));
        }
    });
    if fields.is_empty() {
        return;
    }
    let lines = HEADER_LINES.with(|lines| lines.borrow().clone());
    let mut changes = Vec::with_capacity(fields.len());
    for (id, name) in fields {
        let Some(line) = lines
            .iter()
            .find(|(field, _)| *field == name)
            .map(|(_, line)| line.clone())
        else {
            continue;
        };
        let Some(value) = document.get_node(id).and_then(|node| {
            node.children.iter().copied().find(|&child| {
                document
                    .get_node(child)
                    .is_some_and(|child| has_class(child, "flectar-embedded-value"))
            })
        }) else {
            continue;
        };
        let date = (name == "date")
            .then(|| shown_date(document, value))
            .flatten();
        changes.push((id, line, value, date));
    }
    let mut mutator = document.mutate();
    for (id, line, value, date) in changes {
        let mut nodes = Vec::with_capacity(3);
        if !line.before.is_empty() {
            nodes.push(mutator.create_text_node(&line.before));
        }
        nodes.push(value);
        if !line.after.is_empty() {
            nodes.push(mutator.create_text_node(&line.after));
        }
        mutator.replace_children(id, &nodes);
        if let Some((time, shown)) = date {
            let text = mutator.create_text_node(&shown);
            mutator.replace_children(time, &[text]);
        }
    }
}

/// The `<time>` in a date line's value, and its moment the user's way, as a
/// forward's header says it.
fn shown_date(document: &BaseDocument, value: NodeId) -> Option<(NodeId, String)> {
    let time = document
        .get_node(value)?
        .children
        .iter()
        .copied()
        .find(|&child| {
            document
                .get_node(child)
                .is_some_and(|node| node.data.is_element_with_tag_name(&local_name!("time")))
        })?;
    let moment = document
        .get_node(time)?
        .data
        .attr(local_name!("datetime"))
        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())?;
    let local = moment.with_timezone(&chrono::Local).naive_local();
    let shown = crate::date_format::current()
        .date_time(crate::date_format::DateStyle::WeekdayMedium, local);
    Some((time, shown))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A forward's header lines as the core writes them into a stored body.
    fn stored(fields: &str) -> String {
        format!(
            "<html><head></head><body>Outer body.\
             <div class=\"flectar-embedded-message\" style=\"margin:24px 0 0;border-top:1px solid #dadce0\">\
             <div class=\"flectar-embedded-headers\" style=\"padding:12px 0 16px;font-size:13px;line-height:20px;color:#5f6368\">\
             {fields}</div><div class=\"flectar-embedded-body\">Forwarded body.</div></div></body></html>"
        )
    }

    fn field(name: &str, english: &str, value: &str) -> String {
        format!(
            "<div class=\"flectar-embedded-field\" data-field=\"{name}\">{english}: \
             <span class=\"flectar-embedded-value\" style=\"color:#202124\">{value}</span></div>"
        )
    }

    fn lines(from: &str, date: &str, subject: &str, to: &str, cc: &str) -> HeaderLines {
        let line = |pattern: &str| pattern.replace("{}", HEADER_VALUE);
        HeaderLines {
            from: line(from),
            date: line(date),
            subject: line(subject),
            to: line(to),
            cc: line(cc),
        }
    }

    fn english() -> HeaderLines {
        lines("From: {}", "Date: {}", "Subject: {}", "To: {}", "Cc: {}")
    }

    fn shown(html: &str) -> String {
        crate::renderer::prepare_email_html(html)
            .unwrap()
            .plain_text
            .clone()
    }

    #[test]
    fn issue_455_a_forwarded_header_speaks_the_ui_language() {
        let html = stored(&[
            field("from", "From", "Maya &lt;maya@example.com&gt;"),
            field(
                "date",
                "Date",
                "<time datetime=\"2026-11-17T12:00:00+09:00\">Tue, 17 Nov 2026 12:00:00 +0900</time>",
            ),
            field("subject", "Subject", "Original message"),
            field("to", "To", "Alex &lt;alex@example.com&gt;"),
            field("cc", "Cc", "sam@example.com"),
        ]
        .concat());
        set_header_lines(lines(
            "De : {}",
            "Fecha: {}",
            "Asunto: {}",
            "{} への",
            "CC: {}",
        ));
        let text = shown(&html);
        set_header_lines(english());

        let moment = chrono::DateTime::parse_from_rfc3339("2026-11-17T12:00:00+09:00")
            .unwrap()
            .with_timezone(&chrono::Local)
            .naive_local();
        let date = crate::date_format::current()
            .date_time(crate::date_format::DateStyle::WeekdayMedium, moment);
        for line in [
            "De : Maya <maya@example.com>".to_owned(),
            format!("Fecha: {date}"),
            "Asunto: Original message".to_owned(),
            "Alex <alex@example.com> への".to_owned(),
            "CC: sam@example.com".to_owned(),
        ] {
            assert!(text.contains(&line), "{line:?} in {text:?}");
        }
        assert!(!text.contains("From:") && !text.contains("+0900"), "{text}");
        assert!(text.contains("Outer body.") && text.contains("Forwarded body."));
    }

    #[test]
    fn english_is_the_language_until_one_is_set() {
        let html = stored(&field("subject", "Subject", "Original message"));
        assert!(shown(&html).contains("Subject: Original message"));
    }

    #[test]
    fn lines_the_core_did_not_write_stay_as_they_are() {
        set_header_lines(lines(
            "De: {}",
            "Fecha: {}",
            "Asunto: {}",
            "Para: {}",
            "CC: {}",
        ));
        let text = shown(&stored(
            &[
                // A field this version doesn't know, and one without a value.
                field("bcc", "Bcc", "hidden@example.com"),
                "<div class=\"flectar-embedded-field\" data-field=\"from\">From: nobody</div>"
                    .to_owned(),
                // A date that is no RFC 3339 moment keeps the words it has.
                field(
                    "date",
                    "Date",
                    "<time datetime=\"yesterday\">Mon, 16 Nov 2026</time>",
                ),
            ]
            .concat(),
        ));
        set_header_lines(english());
        assert!(text.contains("Bcc: hidden@example.com"), "{text}");
        assert!(text.contains("From: nobody"), "{text}");
        assert!(text.contains("Fecha: Mon, 16 Nov 2026"), "{text}");
    }
}
