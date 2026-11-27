//! Plain text in format=flowed (RFC 3676). A line that ends in a space goes
//! on in the next one, so a mail app can show each paragraph as wide as its
//! window, and the text still reads as ordinary lines where the parameter
//! means nothing.

use std::borrow::Cow;

use unicode_width::UnicodeWidthChar;

/// How wide written lines are, in columns. RFC 3676 section 4.2 keeps lines
/// to 78 characters and suggests 72 for paragraphs longer than that.
pub const WRAP_WIDTH: usize = 72;
/// The narrowest a deeply quoted paragraph wraps to, whatever its quote
/// marks take of the line.
const MIN_WRAP_WIDTH: usize = 20;
/// The Usenet signature separator, a line that is neither fixed nor flowed
/// (RFC 3676 section 4.3).
const SIGNATURE_SEPARATOR: &str = "-- ";
/// The longest line SMTP carries, not counting its CRLF (RFC 5321 section
/// 4.5.3.1.6).
const MAX_LINE_OCTETS: usize = 998;

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

/// `text` written as format=flowed with DelSp=no, in CRLF lines (RFC 3676
/// section 4.2). A line wider than [`WRAP_WIDTH`] columns breaks after the
/// spaces between two words, and those spaces stay at the end of the line to
/// say it goes on. A word too wide for a line keeps a line of its own.
///
/// A line's quote marks, however they are spaced, become `>` once per level
/// and a space, on every line its paragraph wraps onto (section 4.5), and
/// an empty quoted line is the marks alone. Spaces before a line break go,
/// as the break is meant; the signature separator stays as it is (section
/// 4.3); and an unquoted line that starts with a space, `>` or "From " gets
/// a space before it (section 4.4).
pub fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + text.len() / 16 + 2);
    let (body, ends_with_newline) = match text.strip_suffix('\n') {
        Some(body) => (body.strip_suffix('\r').unwrap_or(body), true),
        None => (text, false),
    };
    if text.is_empty() {
        return out;
    }
    let mut lines = body.split('\n').peekable();
    while let Some(line) = lines.next() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        let (depth, content) = quote_marks(line);
        let prefix = ">".repeat(depth);
        if content == SIGNATURE_SEPARATOR {
            out.push_str(&prefix);
            if depth > 0 {
                out.push(' ');
            }
            out.push_str(SIGNATURE_SEPARATOR);
        } else {
            let content = content.trim_end_matches(' ');
            if content.is_empty() {
                out.push_str(&prefix);
            } else {
                let lead = if depth > 0 { depth + 1 } else { 0 };
                let width = WRAP_WIDTH.saturating_sub(lead).max(MIN_WRAP_WIDTH);
                let pieces = wrapped(content, width);
                let count = pieces.len();
                for (index, piece) in pieces.into_iter().enumerate() {
                    out.push_str(&prefix);
                    if depth > 0 || needs_stuffing(piece) {
                        out.push(' ');
                    }
                    out.push_str(piece);
                    if index + 1 < count {
                        out.push_str("\r\n");
                    }
                }
            }
        }
        if lines.peek().is_some() || ends_with_newline {
            out.push_str("\r\n");
        }
    }
    out
}

/// The quote depth of a line of text that isn't flowed, and what follows its
/// quote marks. Marks may have a space between them, as some mail apps write
/// them, and one space after them is part of the marks.
pub fn quote_marks(line: &str) -> (usize, &str) {
    let mut depth = 0;
    let mut rest = line;
    loop {
        if let Some(after) = rest.strip_prefix('>') {
            depth += 1;
            rest = after;
        } else if depth > 0
            && let Some(after) = rest.strip_prefix(" >")
        {
            depth += 1;
            rest = after;
        } else {
            break;
        }
    }
    if depth > 0 {
        rest = rest.strip_prefix(' ').unwrap_or(rest);
    }
    (depth, rest)
}

fn needs_stuffing(line: &str) -> bool {
    line.starts_with([' ', '>']) || line.starts_with("From ")
}

/// The lines `content` wraps onto at `width` columns. Each line but the last
/// ends with the spaces where it breaks, and no line is made of a signature
/// separator alone, which a paragraph can't hold (section 4.2).
fn wrapped(content: &str, width: usize) -> Vec<&str> {
    let mut lines = Vec::new();
    // Where the line being filled starts, where its last whole word and the
    // spaces after it end, and how wide that much of it is.
    let mut start = 0;
    let mut end = 0;
    let mut columns = 0;
    for (word_start, spaces_end) in words(content) {
        // The spaces after a word stay on its line when the line breaks
        // there, and count toward its width (RFC 3676 section 4.2).
        let word = columns_of(&content[word_start..spaces_end]);
        let fits = columns + word <= width;
        let breakable = end > start && &content[start..end] != SIGNATURE_SEPARATOR;
        if !fits && breakable {
            lines.push(&content[start..end]);
            start = end;
            columns = 0;
        }
        columns += columns_of(&content[end..spaces_end]);
        end = spaces_end;
    }
    lines.push(&content[start..]);
    lines
}

/// Where each word of `content` starts, and where the spaces after it end.
/// The first word takes the spaces before it, as a line can't break before
/// its first word.
fn words(content: &str) -> impl Iterator<Item = (usize, usize)> + '_ {
    let mut at = 0;
    std::iter::from_fn(move || {
        if at >= content.len() {
            return None;
        }
        let word_start = if at == 0 {
            content.len() - content.trim_start_matches(' ').len()
        } else {
            at
        };
        let word_end = content[word_start..]
            .find(' ')
            .map_or(content.len(), |offset| word_start + offset);
        let spaces_end = content.len() - content[word_end..].trim_start_matches(' ').len();
        let item = (at, spaces_end);
        at = spaces_end;
        Some(item)
    })
}

fn columns_of(text: &str) -> usize {
    text.chars()
        .map(|character| character.width().unwrap_or(0))
        .sum()
}

/// How a text part goes over the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferEncoding {
    SevenBit,
    EightBit,
    Base64,
}

impl TransferEncoding {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SevenBit => "7bit",
            Self::EightBit => "8bit",
            Self::Base64 => "base64",
        }
    }
}

/// How CRLF `text` goes over the wire: as it is, which flowed text wants
/// (RFC 3676 section 4.2 asks not to use quoted-printable only to keep the
/// trailing spaces), unless a line is too long for SMTP or holds a bare CR,
/// LF or NUL, which only base64 carries.
pub fn transfer_encoding(text: &str) -> TransferEncoding {
    let unsafe_line = text
        .split("\r\n")
        .any(|line| line.len() > MAX_LINE_OCTETS || line.contains(['\r', '\n', '\0']));
    if unsafe_line {
        TransferEncoding::Base64
    } else if text.is_ascii() {
        TransferEncoding::SevenBit
    } else {
        TransferEncoding::EightBit
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

    // Writing.

    #[test]
    fn wraps_at_72_columns_after_the_spaces() {
        let word = "abcd ";
        let text = word.repeat(20);
        assert_eq!(
            encode(text.trim_end()),
            format!("{}\r\n{}", word.repeat(14), word.repeat(6).trim_end())
        );
    }

    #[test]
    fn every_written_line_fits_and_reads_back() {
        let text = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat.\nSecond paragraph.";
        let written = encode(text);
        assert!(written.split("\r\n").all(|line| line.len() <= WRAP_WIDTH));
        assert_eq!(decoded(&written).replace("\r\n", "\n"), text);
    }

    #[test]
    fn spaces_before_a_line_break_go() {
        assert_eq!(encode("Hello   \nthere \n"), "Hello\r\nthere\r\n");
        assert_eq!(decoded(&encode("Hello \nthere")), "Hello\r\nthere");
    }

    #[test]
    fn keeps_runs_of_spaces_where_a_line_breaks() {
        let text = format!("{}  {}", "a".repeat(70), "b".repeat(10));
        let written = encode(&text);
        assert_eq!(
            written,
            format!("{}  \r\n{}", "a".repeat(70), "b".repeat(10))
        );
        assert_eq!(decoded(&written), text);
    }

    #[test]
    fn a_word_wider_than_a_line_stays_whole() {
        let url = format!("https://example.com/{}", "x".repeat(100));
        let written = encode(&format!("See {url} now"));
        assert_eq!(written, format!("See \r\n{url} \r\nnow"));
        assert_eq!(decoded(&written), format!("See {url} now"));
    }

    #[test]
    fn stuffs_unquoted_lines_that_could_read_as_something_else() {
        assert_eq!(encode("  indented"), "   indented");
        assert_eq!(encode("From the start"), " From the start");
        // A wrapped line that comes to start with "From " or ">" is too.
        let text = format!("{} From me", "a".repeat(70));
        assert_eq!(encode(&text), format!("{} \r\n From me", "a".repeat(70)));
        assert_eq!(decoded(&encode(&text)), text);
        let text = format!("{} >not a quote", "a".repeat(70));
        assert_eq!(
            encode(&text),
            format!("{} \r\n >not a quote", "a".repeat(70))
        );
        assert_eq!(decoded(&encode(&text)), text);
    }

    #[test]
    fn a_line_that_starts_with_a_mark_is_quoted() {
        // In text that isn't flowed, that is how a quote is written.
        assert_eq!(encode("x\n>2 is more"), "x\r\n> 2 is more");
    }

    #[test]
    fn writes_quote_marks_once_per_level_on_every_line() {
        let quoted = format!("> > {}", "word ".repeat(30).trim_end());
        let written = encode(&quoted);
        let lines: Vec<&str> = written.split("\r\n").collect();
        assert!(lines.len() > 1);
        assert!(
            lines.iter().all(|line| line.starts_with(">> ")),
            "{written}"
        );
        assert!(lines.iter().all(|line| line.len() <= WRAP_WIDTH));
        assert_eq!(
            decoded(&written),
            format!(">> {}", "word ".repeat(30).trim_end())
        );
    }

    #[test]
    fn an_empty_quoted_line_is_its_marks_alone() {
        assert_eq!(encode("> Hi\n>\n> > \n> Bye"), "> Hi\r\n>\r\n>>\r\n> Bye");
    }

    #[test]
    fn keeps_the_signature_separator() {
        assert_eq!(encode("Thanks\n\n-- \nMe"), "Thanks\r\n\r\n-- \r\nMe");
        assert_eq!(encode("> -- \n> Them"), "> -- \r\n> Them");
        assert_eq!(decoded(&encode("Thanks\n-- \nMe")), "Thanks\r\n-- \r\nMe");
    }

    #[test]
    fn never_leaves_a_separator_alone_on_a_wrapped_line() {
        // "-- " would come alone onto the second line, so the word after it
        // stays with it there.
        let text = format!("{} -- {}", "a".repeat(70), "b".repeat(70));
        let written = encode(&text);
        assert_eq!(
            written,
            format!("{} \r\n-- {}", "a".repeat(70), "b".repeat(70))
        );
        assert_eq!(decoded(&written), text);
    }

    #[test]
    fn measures_wide_characters_as_two_columns() {
        let text = "가나다 ".repeat(20);
        let written = encode(text.trim_end());
        assert!(
            written
                .split("\r\n")
                .all(|line| columns_of(line) <= WRAP_WIDTH)
        );
        assert_eq!(decoded(&written), text.trim_end());
    }

    #[test]
    fn keeps_the_last_line_end_as_given() {
        assert_eq!(encode("One\ntwo"), "One\r\ntwo");
        assert_eq!(encode("One\r\ntwo\r\n"), "One\r\ntwo\r\n");
        assert_eq!(encode(""), "");
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

    #[test]
    fn picks_the_transfer_encoding_flowed_text_wants() {
        assert_eq!(
            transfer_encoding("Hello \r\nthere"),
            TransferEncoding::SevenBit
        );
        assert_eq!(
            transfer_encoding("Grüße \r\naus"),
            TransferEncoding::EightBit
        );
        assert_eq!(
            transfer_encoding(&"x".repeat(999)),
            TransferEncoding::Base64
        );
        assert_eq!(transfer_encoding("bare\rreturn"), TransferEncoding::Base64);
        assert_eq!(transfer_encoding("nul\0"), TransferEncoding::Base64);
    }
}
