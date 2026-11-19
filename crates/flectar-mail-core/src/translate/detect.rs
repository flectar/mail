//! The language a message is written in, found on the device (#240).
//!
//! whatlang reads the sender's own words: quoted replies, signatures, links
//! and addresses come out first, since a reply in German over an English
//! quote reads as English otherwise. A message gets a language only when
//! whatlang calls its guess reliable, so a translation bar for the wrong
//! language is rare (about 2 in 1,000 messages of two sentences, in our
//! measurements), and short messages get none, as Chromium needs 25 bytes.
//! Long messages are sampled at the start, middle and end, as Chromium and
//! Stalwart do.

use super::document::{Element, Node, has_words, parse};

/// Chromium's `kMinimumContentLengthBytes`.
const SHORTEST: usize = 25;
/// The most text whatlang reads, in three samples.
const SAMPLE: usize = 4096;

/// A message's language as a BCP 47 primary language subtag, such as "de",
/// when whatlang is sure of it.
pub(crate) fn language(text: Option<&str>, html: Option<&str>) -> Option<&'static str> {
    let own = match text.filter(|text| has_words(text)) {
        Some(text) => own_words(text, true),
        None => own_words(&visible_text(html?, true)?, true),
    };
    detected(&own)
}

/// The language of the whole message, quotes included: the one a
/// forwarded document is in, below a note in another language.
pub(crate) fn whole_language(text: Option<&str>, html: Option<&str>) -> Option<&'static str> {
    let all = match text.filter(|text| has_words(text)) {
        Some(text) => own_words(text, false),
        None => own_words(&visible_text(html?, false)?, false),
    };
    detected(&all)
}

fn detected(words: &str) -> Option<&'static str> {
    let sample = sample(words)?;
    let info = whatlang::detect(&sample)?;
    if !info.is_reliable() {
        return None;
    }
    tag(info.lang().code())
}

// ---- What the sender wrote ------------------------------------------------

/// Classes and ids mail apps put on quoted mail and signatures. Gmail and
/// Yahoo wrap quotes in `gmail_quote` and `yahoo_quoted`, Thunderbird marks
/// the quote header and signature with `moz-cite-prefix` and `moz-signature`,
/// and Outlook puts the reply header in `divRplyFwdMsg` with the quote
/// after it.
fn is_quote(element: &Element) -> bool {
    let class = |name| {
        element
            .attrs
            .iter()
            .any(|(key, value)| key == "class" && value.split_ascii_whitespace().any(|c| c == name))
    };
    element.name == "blockquote"
        || [
            "gmail_quote",
            "gmail_quote_container",
            "gmail_signature",
            "yahoo_quoted",
            "moz-cite-prefix",
            "moz-signature",
        ]
        .into_iter()
        .any(class)
}

/// Outlook's reply header and what it wraps the quote in: everything from
/// here on is quoted.
fn starts_quote(element: &Element) -> bool {
    element.attrs.iter().any(|(key, value)| {
        key == "id"
            && matches!(
                value.as_str(),
                "divRplyFwdMsg"
                    | "appendonsend"
                    | "OLK_SRC_BODY_SECTION"
                    | "mail-editor-reference-message-container"
            )
    })
}

fn is_block(name: &str) -> bool {
    matches!(
        name,
        "p" | "div"
            | "br"
            | "li"
            | "tr"
            | "td"
            | "th"
            | "table"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "pre"
            | "ul"
            | "ol"
            | "center"
            | "section"
            | "article"
            | "header"
            | "footer"
            | "hr"
    )
}

/// Collects shown text, a line per block. With `own`, quotes are left out,
/// and it returns false once a quote has begun, as nothing after it is the
/// sender's own.
fn collect(element: &Element, own: bool, out: &mut String) -> bool {
    if own && starts_quote(element) {
        return false;
    }
    if (own && is_quote(element))
        || matches!(
            element.name.as_str(),
            "head" | "style" | "script" | "title" | "template" | "code" | "pre"
        )
    {
        return true;
    }
    let block = is_block(&element.name);
    if block {
        out.push('\n');
    }
    for child in &element.children {
        match child {
            Node::Text(text) => out.push_str(text),
            Node::Element(inner) => {
                if !collect(inner, own, out) {
                    return false;
                }
            }
        }
    }
    if block {
        out.push('\n');
    }
    true
}

fn visible_text(html: &str, own: bool) -> Option<String> {
    let document = parse(html)?;
    let mut out = String::new();
    collect(&document, own, &mut out);
    Some(out)
}

/// Words that say a quoted message follows ("… wrote:"), in the languages
/// mail apps write that line in.
const WROTE: &[&str] = &[
    "wrote",
    "writes",
    "schrieb",
    "écrit",
    "escribió",
    "escreveu",
    "ha scritto",
    "schreef",
    "skrev",
    "napisał",
    "napsal",
    "kirjoitti",
    "írta",
    "yazdı",
    "пишет",
    "написал",
    "написа",
    "έγραψε",
    "kirjutas",
    "rakstīja",
    "rašė",
    "scrisse",
    "a scris",
    "写道",
    "寫道",
    "のメッセージ",
    "작성",
    "כתב",
    "كتب",
];

fn is_reply_header(line: &str) -> bool {
    let line = line.trim();
    if !(line.ends_with(':') || line.ends_with('：')) {
        return false;
    }
    let lower = line.to_lowercase();
    WROTE.iter().any(|word| lower.contains(word))
}

/// `-----Original Message-----` and its kin in other languages.
fn is_original_message(line: &str) -> bool {
    let line = line.trim();
    (line.starts_with("-----") || line.starts_with("_____"))
        && line.len() > 10
        && (has_words(line) || line.chars().all(|c| c == '_'))
}

/// Outlook's header block before a quote, in any language: three or more
/// lines in a row that each start with a short word and a colon (From:,
/// Sent:, To:, Subject:).
fn header_block_at(lines: &[&str], index: usize) -> bool {
    let header = |line: &str| {
        let line = line.trim_start_matches('*').trim_start();
        line.find([':', '：']).is_some_and(|colon| {
            let name = line[..colon].trim_end_matches('*').trim();
            !name.is_empty() && name.chars().count() <= 20 && name.split_whitespace().count() <= 2
        })
    };
    lines.len() >= index + 3 && lines[index..index + 3].iter().all(|line| header(line))
}

fn is_url_or_address(word: &str) -> bool {
    let lower = word.to_ascii_lowercase();
    lower.contains("://")
        || lower.starts_with("www.")
        || lower.starts_with("mailto:")
        || lower.starts_with("cid:")
        || lower.starts_with("tel:")
        || (word.contains('@') && word.contains('.'))
        // Long runs of letters and digits mixed, such as ids and keys.
        || (word.len() > 24
            && word.chars().any(|c| c.is_ascii_digit())
            && word.chars().all(|c| c.is_ascii_alphanumeric() || "+/=-_".contains(c)))
}

/// The words of a message without links, addresses or lines without
/// words, as one line. With `own`, only the sender's: the text up to a
/// quote or the signature, without quoted lines.
pub(crate) fn own_words(text: &str, own: bool) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut kept = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if own
            && (is_reply_header(trimmed)
                || is_original_message(trimmed)
                || trimmed.starts_with("Sent from my ")
                || line.trim_end() == "--"
                || *line == "-- "
                || (index > 0 && header_block_at(&lines, index)))
        {
            break;
        }
        if (own && trimmed.starts_with('>')) || !has_words(trimmed) {
            continue;
        }
        let trimmed = trimmed.trim_start_matches(['>', ' ']);
        let words: Vec<&str> = trimmed
            .split_whitespace()
            .filter(|word| !is_url_or_address(word))
            .collect();
        if words.iter().any(|word| has_words(word)) {
            kept.push(words.join(" "));
        }
    }
    kept.join(" ")
}

/// What whatlang reads: nothing under [`SHORTEST`], and the start, middle
/// and end of anything longer than [`SAMPLE`].
fn sample(text: &str) -> Option<String> {
    if text.len() < SHORTEST {
        return None;
    }
    if text.len() <= SAMPLE {
        return Some(text.to_owned());
    }
    let third = SAMPLE / 3;
    let piece = |start: usize| {
        let start = text.floor_char_boundary(start);
        let end = text.floor_char_boundary(start + third);
        &text[start..end]
    };
    Some(format!(
        "{} {} {}",
        piece(0),
        piece(text.len() / 2 - third / 2),
        piece(text.len() - third)
    ))
}

/// A whatlang language as a BCP 47 primary language subtag.
pub(crate) fn tag_of(lang: whatlang::Lang) -> Option<&'static str> {
    tag(lang.code())
}

/// whatlang's ISO 639-3 code as a BCP 47 primary language subtag: the
/// two-letter code where there is one (RFC 5646 section 2.2.1).
fn tag(code: &str) -> Option<&'static str> {
    Some(match code {
        "afr" => "af",
        "aka" => "ak",
        "amh" => "am",
        "ara" => "ar",
        "aze" => "az",
        "bel" => "be",
        "ben" => "bn",
        "bul" => "bg",
        "cat" => "ca",
        "ces" => "cs",
        // Mandarin is written with the Chinese macrolanguage code.
        "cmn" => "zh",
        "cym" => "cy",
        "dan" => "da",
        "deu" => "de",
        "ell" => "el",
        "eng" => "en",
        "epo" => "eo",
        "est" => "et",
        "fin" => "fi",
        "fra" => "fr",
        "guj" => "gu",
        "heb" => "he",
        "hin" => "hi",
        "hrv" => "hr",
        "hun" => "hu",
        "hye" => "hy",
        "ind" => "id",
        "ita" => "it",
        "jav" => "jv",
        "jpn" => "ja",
        "kan" => "kn",
        "kat" => "ka",
        "khm" => "km",
        "kor" => "ko",
        "lat" => "la",
        "lav" => "lv",
        "lit" => "lt",
        "mal" => "ml",
        "mar" => "mr",
        "mkd" => "mk",
        "mya" => "my",
        "nep" => "ne",
        "nld" => "nl",
        "nob" => "nb",
        "ori" => "or",
        "pan" => "pa",
        // Iranian Persian, written with the Persian macrolanguage code.
        "pes" => "fa",
        "pol" => "pl",
        "por" => "pt",
        "ron" => "ro",
        "rus" => "ru",
        "sin" => "si",
        "slk" => "sk",
        "slv" => "sl",
        "sna" => "sn",
        "spa" => "es",
        "srp" => "sr",
        "swe" => "sv",
        "tam" => "ta",
        "tel" => "te",
        "tgl" => "tl",
        "tha" => "th",
        "tuk" => "tk",
        "tur" => "tr",
        "ukr" => "uk",
        "urd" => "ur",
        "uzb" => "uz",
        "vie" => "vi",
        "yid" => "yi",
        "zul" => "zu",
        _ => return None,
    })
}

/// The primary language subtag of a language tag or locale, lowercase, with
/// Norwegian's three codes as one, as Firefox maps `no` to `nb`.
pub(crate) fn primary(tag: &str) -> String {
    let primary = tag
        .split(['-', '_', '.', '@'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    match primary.as_str() {
        "no" | "nn" => "nb".to_owned(),
        // Old codes some systems still report (ISO 639 before 1989).
        "iw" => "he".to_owned(),
        "in" => "id".to_owned(),
        "ji" => "yi".to_owned(),
        _ => primary,
    }
}

/// Whether a language is written right to left.
pub(crate) fn right_to_left(tag: &str) -> bool {
    matches!(
        primary(tag).as_str(),
        "ar" | "he" | "fa" | "ur" | "yi" | "ps" | "sd" | "ug" | "dv" | "ckb"
    )
}

#[cfg(test)]
#[path = "detect_tests.rs"]
mod tests;
