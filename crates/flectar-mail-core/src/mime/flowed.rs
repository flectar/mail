//! Plain text in format=flowed (RFC 3676). A line that ends in a space goes
//! on in the next one, so a mail app can show each paragraph as wide as its
//! window, and the text still reads as ordinary lines where the parameter
//! means nothing.

use std::borrow::Cow;

/// The Usenet signature separator, a line that is neither fixed nor flowed
/// (RFC 3676 section 4.3).
const SIGNATURE_SEPARATOR: &str = "-- ";

/// What a text/plain part's Content-Type says about its line breaks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Format {
    /// Every line break is meant: format=fixed, or no format parameter.
    #[default]
    Fixed,
    /// A line that ends in a space goes on in the next one. With
    /// `delete_space` (DelSp=yes) the writer added that space itself, and it
    /// comes out again.
    Flowed { delete_space: bool },
}

impl Format {
    /// The format of a part with this Content-Type. Only text/plain flows
    /// (RFC 3676 section 4).
    pub fn of(content_type: Option<&mail_parser::ContentType<'_>>) -> Self {
        let Some(content_type) = content_type else {
            return Self::Fixed;
        };
        let plain = content_type.ctype().eq_ignore_ascii_case("text")
            && content_type
                .subtype()
                .is_none_or(|subtype| subtype.eq_ignore_ascii_case("plain"));
        let says = |name: &str, value: &str| {
            content_type
                .attribute(name)
                .is_some_and(|given| given.trim().eq_ignore_ascii_case(value))
        };
        if !(plain && says("format", "flowed")) {
            return Self::Fixed;
        }
        Self::Flowed {
            delete_space: says("delsp", "yes"),
        }
    }

    /// The format a Content-Type header's value gives, as an API hands the
    /// header over: `text/plain; charset=utf-8; format=flowed`.
    pub fn of_header(value: &str) -> Self {
        let header = format!("Content-Type: {value}\r\n\r\n");
        let parsed = mail_parser::MessageParser::default().parse_headers(header.as_bytes());
        Self::of(parsed.as_ref().and_then(|message| {
            use mail_parser::MimeHeaders;
            message.content_type()
        }))
    }

    /// `text` with each flowed paragraph back on one line, as [`decode`]
    /// reads it, or as it is when its line breaks are fixed.
    pub fn read(self, text: &str) -> Cow<'_, str> {
        match self {
            Self::Fixed => Cow::Borrowed(text),
            Self::Flowed { delete_space } => Cow::Owned(decode(text, delete_space)),
        }
    }
}

/// `text`, written as format=flowed, with each paragraph back on one line
/// (RFC 3676 section 4.1). A paragraph also ends where the quote depth
/// changes or a signature separator comes, as sections 4.3 and 4.5 ask of a
/// reader. Quoted lines keep their quote marks, as `>` once per level and a
/// space, and an unquoted line that starts with `>` keeps the space that
/// protected it, so that neither reads as anything else once the text is
/// taken as fixed lines. Line ends stay CRLF or LF, as `text` has them.
pub fn decode(text: &str, delete_space: bool) -> String {
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let (body, ends_with_newline) = match text.strip_suffix('\n') {
        Some(body) => (body.strip_suffix('\r').unwrap_or(body), true),
        None => (text, false),
    };
    let mut out = String::with_capacity(text.len());
    if text.is_empty() {
        return out;
    }
    // The quote depth of the paragraph a flowed line left open.
    let mut open: Option<usize> = None;
    let mut lines = body.split('\n').peekable();
    while let Some(line) = lines.next() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        let last = lines.peek().is_none();
        let depth = line.bytes().take_while(|&byte| byte == b'>').count();
        // A space after the quote marks, or at the start of an unquoted
        // line, is stuffing (section 4.4).
        let content = &line[depth..];
        let content = content.strip_prefix(' ').unwrap_or(content);
        let separator = content == SIGNATURE_SEPARATOR;
        if open.is_some_and(|open| open != depth || separator) {
            out.push_str(newline);
            open = None;
        }
        if open.is_none() {
            push_quote_marks(&mut out, depth, content);
        }
        if !separator && content.ends_with(' ') {
            out.push_str(match delete_space {
                true => &content[..content.len() - 1],
                false => content,
            });
            open = Some(depth);
        } else {
            out.push_str(content);
            open = None;
            if !last || ends_with_newline {
                out.push_str(newline);
            }
        }
    }
    if open.is_some() && ends_with_newline {
        out.push_str(newline);
    }
    out
}

fn push_quote_marks(out: &mut String, depth: usize, content: &str) {
    if depth > 0 {
        out.extend(std::iter::repeat_n('>', depth));
        out.push(' ');
    } else if content.starts_with('>') {
        out.push(' ');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decoded(text: &str) -> String {
        decode(text, false)
    }

    // Reading, with the cases Thunderbird for Android's FlowedMessageUtils
    // tests.

    #[test]
    fn joins_a_paragraph() {
        assert_eq!(
            decoded("Text that should be \r\ndisplayed on one line"),
            "Text that should be displayed on one line"
        );
    }

    #[test]
    fn keeps_fixed_lines() {
        assert_eq!(
            decoded(
                "Text that \r\nshould be \r\ndisplayed on \r\none line.\r\nText that should retain\r\nits line break."
            ),
            "Text that should be displayed on one line.\r\nText that should retain\r\nits line break."
        );
        assert_eq!(
            decoded("Line one\r\nLine two\r\n"),
            "Line one\r\nLine two\r\n"
        );
    }

    #[test]
    fn joins_quoted_paragraphs_and_keeps_their_marks() {
        assert_eq!(
            decoded(
                "On [date], [user] wrote:\r\n> Text that should be displayed \r\n> on one line\r\n\r\nSome more text that should be \r\ndisplayed on one line.\r\n"
            ),
            "On [date], [user] wrote:\r\n> Text that should be displayed on one line\r\n\r\nSome more text that should be displayed on one line.\r\n"
        );
        assert_eq!(
            decoded("> Text that should be \r\n> displayed on one line"),
            "> Text that should be displayed on one line"
        );
    }

    #[test]
    fn a_change_of_quote_depth_ends_a_paragraph() {
        assert_eq!(
            decoded("> Quoted text \r\nSome other text"),
            "> Quoted text \r\nSome other text"
        );
        assert_eq!(
            decoded(">> Depth 2 \r\n> Depth 1 \r\n> is here\r\nSome other text"),
            ">> Depth 2 \r\n> Depth 1 is here\r\nSome other text"
        );
        assert_eq!(decoded("> Quoted \r\n\r\nText"), "> Quoted \r\n\r\nText");
    }

    #[test]
    fn deletes_the_added_space_with_delsp() {
        assert_eq!(
            decode("Text that is wrapped mid wo \r\nrd", true),
            "Text that is wrapped mid word"
        );
        assert_eq!(decode("> Quoted te \r\n> xt", true), "> Quoted text");
    }

    #[test]
    fn takes_out_space_stuffing() {
        assert_eq!(
            decoded("Text that should be \r\n displayed on one line"),
            "Text that should be displayed on one line"
        );
        assert_eq!(
            decoded("Line 1\r\n Line 2\r\n Line 3\r\n"),
            "Line 1\r\nLine 2\r\nLine 3\r\n"
        );
    }

    #[test]
    fn a_signature_separator_ends_a_paragraph() {
        assert_eq!(
            decoded(
                "Text that should be \r\ndisplayed on one line.\r\n\r\n-- \r\nSignature \r\ntext"
            ),
            "Text that should be displayed on one line.\r\n\r\n-- \r\nSignature text"
        );
        assert_eq!(
            decoded(
                "> Text that should be \r\n> displayed on one line.\r\n> \r\n> -- \r\n> Signature \r\n> text"
            ),
            "> Text that should be displayed on one line.\r\n> \r\n> -- \r\n> Signature text"
        );
        assert_eq!(
            decode("Fake flowed line \r\n-- \r\nSignature", true),
            "Fake flowed line\r\n-- \r\nSignature"
        );
    }

    // Reading, beyond those.

    #[test]
    fn keeps_lf_line_ends_and_an_open_last_paragraph() {
        assert_eq!(decoded("One \ntwo\nthree \n"), "One two\nthree \n");
        assert_eq!(decoded(""), "");
        assert_eq!(decoded("\r\n"), "\r\n");
    }

    #[test]
    fn an_unquoted_line_that_starts_with_a_mark_keeps_its_space() {
        // Taken as fixed text, ">2" without its space would read as a quote.
        assert_eq!(decoded(" >2 is more than 1\r\n"), " >2 is more than 1\r\n");
        assert_eq!(decoded(" From here on\r\n"), "From here on\r\n");
    }

    #[test]
    fn a_line_of_spaces_flows() {
        assert_eq!(decoded("One \r\n  \r\ntwo"), "One  two");
    }

    #[test]
    fn reads_the_format_from_the_content_type() {
        assert_eq!(Format::of_header("text/plain"), Format::Fixed);
        assert_eq!(
            Format::of_header("text/plain; charset=utf-8; format=flowed"),
            Format::Flowed {
                delete_space: false
            }
        );
        assert_eq!(
            Format::of_header("text/plain; format=\"Flowed\"; DelSp=Yes"),
            Format::Flowed { delete_space: true }
        );
        assert_eq!(Format::of_header("text/html; format=flowed"), Format::Fixed);
        assert_eq!(Format::of_header("text/plain; format=fixed"), Format::Fixed);
    }
}
