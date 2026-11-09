//! The original message a reply or forward carries below what the user
//! writes. It stays out of the editor: the composer shows it read-only, as
//! the reader draws it, and sends it as HTML with a plain-text alternative.

use crate::email_document::{append_escaped, quoted_body};
use crate::rich_compose::QUOTE_MARKER;

/// Marks the quote this composer appended, so a reopened draft finds it.
const QUOTE_CLASS: &str = "flectar-quote";
const FORWARD_CLASS: &str = "flectar-forward";
/// The quote style mail clients have long sent, a thin rule at the left.
const BLOCKQUOTE_STYLE: &str = "margin:0 0 0 .8ex;border-left:1px solid #ccc;padding-left:1ex";
/// Keeps a blank line between what the user wrote and the quote.
const SEPARATOR: &str = "<br>";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ComposeQuote {
    /// Forwarded rather than answered.
    pub(crate) forward: bool,
    /// What follows the user's text in the plain-text part, and what
    /// "Edit as text" puts in the editor.
    pub(crate) plain: String,
    /// What follows the user's text in the HTML part.
    pub(crate) html: String,
}

impl ComposeQuote {
    /// A reply quoting `text`, or `html` when the original has an HTML part.
    /// `attribution` is the "On …, X wrote:" line.
    pub(crate) fn reply(attribution: &str, text: &str, html: Option<&str>) -> Self {
        let text = text.trim_end();
        let mut plain = String::with_capacity(attribution.len() + text.len() + 64);
        plain.push_str(attribution);
        for line in text.lines() {
            plain.push_str("\n> ");
            plain.push_str(line);
        }
        let mut block = format!("<div class=\"{QUOTE_CLASS}\"><div>");
        append_escaped(&mut block, attribution);
        block.push_str("<br></div><blockquote type=\"cite\" style=\"");
        block.push_str(BLOCKQUOTE_STYLE);
        block.push_str("\">");
        append_original(&mut block, text, html);
        block.push_str("</blockquote></div>");
        Self {
            forward: false,
            plain,
            html: block,
        }
    }

    /// A forward of `text` or `html` under the forwarded message's header
    /// lines, the first being its "Forwarded message" rule.
    pub(crate) fn forward(header: &[String], text: &str, html: Option<&str>) -> Self {
        let text = text.trim_end();
        let mut plain = header.join("\n");
        plain.push_str("\n\n");
        plain.push_str(text);
        let mut block = format!("<div class=\"{QUOTE_CLASS} {FORWARD_CLASS}\"><div>");
        for line in header {
            append_escaped(&mut block, line);
            block.push_str("<br>");
        }
        block.push_str("</div><br>");
        append_original(&mut block, text, html);
        block.push_str("</div>");
        Self {
            forward: true,
            plain,
            html: block,
        }
    }

    /// The quote as the editor shows it once the user edits it. A reply's
    /// original becomes a quote block under its attribution line.
    pub(crate) fn editable_text(&self) -> String {
        if self.forward {
            return self.plain.clone();
        }
        let mut lines = self.plain.lines();
        let mut text = lines.next().unwrap_or_default().to_owned();
        for line in lines {
            text.push('\n');
            text.push_str(QUOTE_MARKER);
            text.push_str(
                line.strip_prefix("> ")
                    .or_else(|| line.strip_prefix('>'))
                    .unwrap_or(line),
            );
        }
        text
    }

    /// The message's plain-text and HTML parts with the quote below what
    /// the user wrote. `html` is `None` when they wrote nothing.
    pub(crate) fn below(&self, text: &str, html: Option<String>) -> (String, String) {
        let text = text.trim_end_matches(['\r', '\n']);
        let plain = if text.trim().is_empty() {
            self.plain.clone()
        } else {
            format!("{text}\n\n{}", self.plain)
        };
        let html = match html {
            Some(own) => format!("{own}{SEPARATOR}{}", self.html),
            None => self.html.clone(),
        };
        (plain, html)
    }

    /// The quote of a saved draft, as [`Self::below`] wrote it. `text` is
    /// the draft's plain-text part; `reopen` loads what the user wrote from
    /// its HTML and returns how much of `text` that covers. The rest of
    /// `text` is the quote's.
    pub(crate) fn split_saved<'a>(
        html: &'a str,
        text: &str,
        reopen: impl FnOnce(&'a str) -> Option<usize>,
    ) -> Option<Self> {
        let start = quote_start(html)?;
        let (own, block) = html.split_at(start);
        let own = own.strip_suffix(SEPARATOR).unwrap_or(own);
        let written = reopen(own)?;
        let plain = text
            .get(written..)
            .map(|rest| rest.trim_start_matches(['\r', '\n']).to_owned())
            .filter(|rest| !rest.is_empty())
            .unwrap_or_else(|| crate::email_document::fallback(block));
        let forward = block[..block.find('>').unwrap_or(block.len())].contains(FORWARD_CLASS);
        Some(Self {
            forward,
            plain,
            html: block.to_owned(),
        })
    }
}

/// Where the quote block starts, if the HTML ends with one.
fn quote_start(html: &str) -> Option<usize> {
    let mut search = 0;
    while let Some(found) = html[search..].find("<div") {
        let start = search + found;
        let tag_end = html[start..]
            .find('>')
            .map_or(html.len(), |end| start + end);
        let tag = &html[start..tag_end];
        if tag.split(['"', '\'']).any(|value| {
            value
                .split_ascii_whitespace()
                .any(|class| class == QUOTE_CLASS)
        }) && tag.contains("class=")
        {
            // Another client may have added text below the quote, which
            // would turn read-only with it, so such a draft isn't split.
            return block_end(html, start)
                .filter(|&end| ends_the_message(&html[end..]))
                .map(|_| start);
        }
        search = start + 4;
    }
    None
}

/// Just past the `</div>` that closes the `<div>` opening at `start`.
fn block_end(html: &str, start: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut at = start;
    loop {
        let open = html[at..].find("<div").map(|found| at + found);
        let close = html[at..].find("</div>").map(|found| at + found);
        match (open, close) {
            (Some(open), Some(close)) if open < close => {
                depth += 1;
                at = open + "<div".len();
            }
            (_, Some(close)) => {
                depth = depth.checked_sub(1)?;
                at = close + "</div>".len();
                if depth == 0 {
                    return Some(at);
                }
            }
            _ => return None,
        }
    }
}

/// Whether `rest` holds nothing but the document's closing tags.
fn ends_the_message(rest: &str) -> bool {
    let mut rest = rest.trim_start();
    while !rest.is_empty() {
        let Some(tag) = ["</body>", "</html>"].into_iter().find(|tag| {
            rest.get(..tag.len())
                .is_some_and(|start| start.eq_ignore_ascii_case(tag))
        }) else {
            return false;
        };
        rest = rest[tag.len()..].trim_start();
    }
    true
}

fn append_original(block: &mut String, text: &str, html: Option<&str>) {
    match html.map(quoted_body).filter(|body| !body.trim().is_empty()) {
        Some(body) => block.push_str(&body),
        None => {
            for line in text.lines() {
                block.push_str("<div>");
                if line.is_empty() {
                    block.push_str("<br>");
                } else {
                    append_plain_line(block, line);
                }
                block.push_str("</div>");
            }
        }
    }
}

/// A line of a plain-text original, keeping its indentation and spacing,
/// as the reader shows it: spaces HTML would collapse, at the start, after
/// another space or at the end, become no-break spaces, a tab four of them.
fn append_plain_line(block: &mut String, line: &str) {
    let mut after_space = true;
    let mut characters = line.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\t' => block.push_str("&nbsp;&nbsp;&nbsp;&nbsp;"),
            ' ' if after_space || characters.peek().is_none() => block.push_str("&nbsp;"),
            '&' => block.push_str("&amp;"),
            '<' => block.push_str("&lt;"),
            '>' => block.push_str("&gt;"),
            '"' => block.push_str("&quot;"),
            other => block.push(other),
        }
        after_space = matches!(character, ' ' | '\t');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reply_quotes_the_original_html_under_its_attribution() {
        let quote = ComposeQuote::reply(
            "On Mon, Maya <maya@example.com> wrote:",
            "First line\nSecond line\n",
            Some(
                "<html><head><style>p{}</style></head><body><p>First <b>line</b></p></body></html>",
            ),
        );
        assert!(!quote.forward);
        assert_eq!(
            quote.plain,
            "On Mon, Maya <maya@example.com> wrote:\n> First line\n> Second line"
        );
        assert_eq!(
            quote.html,
            "<div class=\"flectar-quote\"><div>On Mon, Maya &lt;maya@example.com&gt; wrote:<br></div>\
             <blockquote type=\"cite\" style=\"margin:0 0 0 .8ex;border-left:1px solid #ccc;padding-left:1ex\">\
             <p>First <b>line</b></p></blockquote></div>"
        );
    }

    #[test]
    fn plain_text_originals_are_quoted_line_by_line() {
        let quote = ComposeQuote::reply("Maya wrote:", "a < b\n\nc", None);
        assert!(
            quote
                .html
                .ends_with("<div>a &lt; b</div><div><br></div><div>c</div></blockquote></div>")
        );
        // Indentation and runs of spaces show as the reader shows them.
        let spaced = ComposeQuote::reply("Maya wrote:", "  indented\tcode  x \nend", None);
        assert!(spaced.html.ends_with(
            "<div>&nbsp;&nbsp;indented&nbsp;&nbsp;&nbsp;&nbsp;code &nbsp;x&nbsp;</div>\
             <div>end</div></blockquote></div>"
        ));
        let forward = ComposeQuote::forward(
            &[
                "---------- Forwarded message ----------".to_owned(),
                "From: Maya <maya@example.com>".to_owned(),
            ],
            "Body",
            Some("<p>Body</p>"),
        );
        assert!(forward.forward);
        assert_eq!(
            forward.plain,
            "---------- Forwarded message ----------\nFrom: Maya <maya@example.com>\n\nBody"
        );
        assert_eq!(
            forward.html,
            "<div class=\"flectar-quote flectar-forward\"><div>---------- Forwarded message ----------<br>\
             From: Maya &lt;maya@example.com&gt;<br></div><br><p>Body</p></div>"
        );
    }

    #[test]
    fn edited_quotes_become_quote_blocks_under_the_attribution() {
        let reply = ComposeQuote::reply("Maya wrote:", "First\n\n> earlier", None);
        assert_eq!(
            reply.editable_text(),
            "Maya wrote:\n│ First\n│ \n│ > earlier"
        );
        let forward = ComposeQuote::forward(&["Forwarded".to_owned()], "Body", None);
        assert_eq!(forward.editable_text(), forward.plain);
    }

    #[test]
    fn a_saved_draft_splits_into_the_users_text_and_its_quote() {
        let quote = ComposeQuote::reply("Maya wrote:", "Original", Some("<p>Original</p>"));
        let (text, html) = quote.below("Thanks!\n", Some("<div>Thanks!</div>".to_owned()));
        assert_eq!(text, format!("Thanks!\n\n{}", quote.plain));
        let saved = ComposeQuote::split_saved(&html, &text, |own| {
            assert_eq!(own, "<div>Thanks!</div>");
            Some("Thanks!".len())
        })
        .unwrap();
        assert_eq!(saved, quote);

        // The editor reopens what the user wrote from its own HTML.
        let mut document = crate::rich_compose::RichComposeDocument::default();
        let saved =
            ComposeQuote::split_saved(&html, &text, |own| document.load_html_start(own, &text))
                .unwrap();
        assert_eq!(document.text(), "Thanks!");
        assert_eq!(saved, quote);

        // Blank lines the user left at the end, and a tab, reopen too.
        let mut written = crate::rich_compose::RichComposeDocument::default();
        written.load("Qty:\t3\n\n\n", 0, 0);
        let (text, html) = quote.below(written.text(), written.body_html_above_quote());
        assert_eq!(text, format!("Qty:\t3\n\n{}", quote.plain));
        let saved =
            ComposeQuote::split_saved(&html, &text, |own| document.load_html_start(own, &text))
                .unwrap();
        assert_eq!(document.text(), "Qty:\t3");
        assert_eq!(saved, quote);

        // A reply with nothing written yet is only the quote.
        let (only_text, only_html) = quote.below("", None);
        assert_eq!((&only_text, &only_html), (&quote.plain, &quote.html));
        let saved = ComposeQuote::split_saved(&only_html, &only_text, |own| {
            assert_eq!(own, "");
            Some(0)
        })
        .unwrap();
        assert_eq!(saved, quote);

        // A server's sanitized copy keeps the class among other attributes.
        let served = html.replace(
            "<div class=\"flectar-quote\">",
            "<div style=\"x\" class=\"flectar-quote\">",
        );
        assert!(ComposeQuote::split_saved(&served, &text, |_| Some("Thanks!".len())).is_some());
        let wrapped = format!("<html><body>{html}\r\n</body>\n</html>\n");
        assert!(ComposeQuote::split_saved(&wrapped, &text, |_| Some("Thanks!".len())).is_some());
        // Text another client added below the quote keeps the draft whole,
        // so none of it turns read-only.
        let continued = format!("{html}<div>Added on the phone</div>");
        assert!(ComposeQuote::split_saved(&continued, &text, |_| Some(0)).is_none());
        let unclosed = html.trim_end_matches("</div>");
        assert!(ComposeQuote::split_saved(unclosed, &text, |_| Some(0)).is_none());
        // Without the marker, or once the user's part can't be read back,
        // the draft reopens as before.
        assert!(ComposeQuote::split_saved("<div>Thanks!</div>", &text, |_| Some(0)).is_none());
        assert!(ComposeQuote::split_saved(&html, &text, |_| None).is_none());
        assert!(
            ComposeQuote::split_saved("<div class=\"flectar-quoted\">x</div>", "x", |_| Some(0))
                .is_none()
        );
    }
}
