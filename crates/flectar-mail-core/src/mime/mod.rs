//! MIME parsing (mail-parser), HTML sanitization (ammonia), and outgoing
//! message construction (mail-builder).

use crate::error::{CoreError, Result};
use crate::models::{Address, SenderVerification};
use mail_parser::MimeHeaders;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};

/// Retains our submission identity when a provider replaces Message-ID.
pub const SUBMISSION_ID_HEADER: &str = "X-Flectar-Submission-ID";

#[derive(Debug, Clone, Default)]
pub struct ParsedHeaders {
    pub message_id: Option<String>,
    pub submission_id: Option<String>,
    pub subject: String,
    pub from: Option<Address>,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub bcc: Vec<Address>,
    pub date_ms: Option<i64>,
    pub references: Vec<String>,
    pub is_automated: bool,
    /// Raw List-Unsubscribe header value, e.g. "<https://…>, <mailto:…>".
    pub list_unsubscribe: Option<String>,
    /// Raw List-Unsubscribe-Post header value; "List-Unsubscribe=One-Click"
    /// marks the HTTPS URI above as an RFC 8058 one-click endpoint.
    pub list_unsubscribe_post: Option<String>,
    /// The list's identifier from List-Id (RFC 2919), lowercase and without
    /// its description or angle brackets.
    pub list_id: Option<String>,
    /// The party that actually transmitted the message, when its domain does
    /// NOT align with From: (mailing lists, ESPs, send-on-behalf - and mail
    /// whose From: is spoofed). First misaligned identity out of the RFC 5322
    /// Sender:, the receiver-stamped Return-Path, and the DKIM d= domain;
    /// None when everything aligns with From:.
    pub via: Option<String>,
    /// Strongest authentication result reported by the receiving MTA.
    pub sender_verification: SenderVerification,
    /// Where a read receipt was asked to go (RFC 8098
    /// Disposition-Notification-To). Empty when the message asks for none,
    /// or for one this client must not send.
    pub receipt_to: Vec<Address>,
    /// RFC 8098 section 2.1 wants the user's confirmation before a receipt
    /// goes out: there is no Return-Path, it differs from the receipt
    /// address, or several receipt addresses are listed.
    pub receipt_confirm: bool,
    /// The message is a read receipt that came back: a multipart/report
    /// (RFC 6522) of report-type disposition-notification. False when the
    /// fetch left Content-Type out.
    pub disposition_report: bool,
}

#[derive(Debug, Clone)]
pub struct ParsedAttachment {
    pub part_id: String,
    pub filename: Option<String>,
    pub mime_type: Option<String>,
    pub size: i64,
    pub content_id: Option<String>,
    pub is_inline: bool,
}

/// Schema version written into `messages.mime_plan_json`.
/// Bumped to 3 when `is_inline` stopped treating a bare Content-ID as proof of
/// an embedded part: plans stored under the old rule are rebuilt on demand.
pub const MIME_PLAN_VERSION: u8 = 3;

/// A compact, serializable description of the IMAP sections needed to make a
/// message readable without downloading unrelated attachment bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MimePlan {
    pub version: u8,
    #[serde(default)]
    pub text_sections: Vec<PlannedTextSection>,
    #[serde(default)]
    pub attachments: Vec<PlannedAttachment>,
}

impl Default for MimePlan {
    fn default() -> Self {
        Self {
            version: MIME_PLAN_VERSION,
            text_sections: Vec::new(),
            attachments: Vec::new(),
        }
    }
}

impl MimePlan {
    /// IMAP section identifiers safe to pass to [`crate::imap::fetch_content_sections`].
    pub fn text_section_ids(&self) -> Vec<String> {
        self.text_sections
            .iter()
            .map(|part| part.section.clone())
            .collect()
    }

    /// The paperclip signal deliberately excludes embedded CID images.
    pub fn has_file_attachments(&self) -> bool {
        self.attachments.iter().any(|part| !part.is_inline)
    }

    /// The message carries a read receipt's report (RFC 8098). The part is
    /// no text to read, so the plan lists it with the files.
    pub fn carries_disposition_report(&self) -> bool {
        self.attachments
            .iter()
            .any(|part| is_disposition_report_part(&part.mime_type))
    }

    /// The encoded size of every planned part, each counted once.
    pub fn encoded_size(&self) -> u64 {
        let mut seen = std::collections::HashSet::new();
        self.text_sections
            .iter()
            .map(|part| (&part.section, part.size))
            .chain(
                self.attachments
                    .iter()
                    .map(|part| (&part.section, part.size)),
            )
            .filter(|(section, _)| seen.insert(*section))
            .map(|(_, size)| u64::from(size))
            .sum()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextSectionKind {
    Plain,
    Html,
    Calendar,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedTextSection {
    /// Numeric IMAP BODY section, for example `1` or `2.1`.
    pub section: String,
    pub kind: TextSectionKind,
    pub mime_type: String,
    pub charset: Option<String>,
    pub transfer_encoding: String,
    /// Encoded octet count reported by BODYSTRUCTURE.
    pub size: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedAttachment {
    /// Numeric IMAP BODY section, for example `2` or `3.1`.
    pub section: String,
    pub filename: Option<String>,
    pub mime_type: String,
    pub size: u32,
    pub content_id: Option<String>,
    pub is_inline: bool,
    pub transfer_encoding: String,
}

/// Decode RFC 2047 encoded-words (`=?charset?enc?text?=`) embedded in a header
/// parameter value. IMAP BODYSTRUCTURE hands back `name`/`filename` parameters
/// verbatim, and many mailers place encoded-words there (even though RFC 2231
/// is the standard mechanism for non-ASCII parameters), so an attachment can
/// arrive named `=?utf-8?B?R2nhuqNp...?=`. Full-message parsing goes through
/// mail-parser, which already decodes these; this covers the BODYSTRUCTURE-only
/// path. Plain text and unrecognized tokens pass through unchanged, so this is
/// idempotent and safe to also apply when reading rows synced before the fix.
pub(crate) fn decode_encoded_words(value: &str) -> String {
    use mail_parser::parsers::MessageStream;

    // Cheap reject: no encoded-word can exist without this marker.
    if !value.contains("=?") {
        return value.to_string();
    }

    let bytes = value.as_bytes();
    let mut out = String::with_capacity(value.len());
    let mut cursor = 0; // start of the plain-text run not yet copied to `out`
    let mut i = 0;
    // End index of the previously decoded encoded-word; RFC 2047 §6.2 says the
    // linear whitespace separating two adjacent encoded-words is not part of
    // the decoded text and must be dropped.
    let mut prev_word_end: Option<usize> = None;

    while i + 1 < bytes.len() {
        if bytes[i] == b'='
            && bytes[i + 1] == b'?'
            && let Some(end) = encoded_word_end(bytes, i)
        {
            // The decoder expects the stream positioned just past the
            // leading `=`, i.e. `?charset?enc?text?=`.
            if let Some(decoded) = MessageStream::new(&bytes[i + 1..end]).decode_rfc2047() {
                let gap = &value[cursor..i];
                if !(prev_word_end == Some(cursor) && gap.trim().is_empty()) {
                    out.push_str(gap);
                }
                out.push_str(&decoded);
                cursor = end;
                prev_word_end = Some(end);
                i = end;
                continue;
            }
        }
        i += 1;
    }

    out.push_str(&value[cursor..]);
    out
}

/// Byte index just past the closing `?=` of the encoded-word starting at
/// `start` (where `bytes[start..]` begins with `=?`), or None if malformed.
fn encoded_word_end(bytes: &[u8], start: usize) -> Option<usize> {
    let charset_end = find_byte(bytes, start + 2, b'?')?; // charset ... `?`
    let encoding_end = find_byte(bytes, charset_end + 1, b'?')?; // encoding ... `?`
    // Encoded-text never contains a literal `?` (Q-encoding escapes it as `=3F`,
    // the Base64 alphabet excludes it), so the next `?` is the terminator.
    let terminator = find_byte(bytes, encoding_end + 1, b'?')?;
    (bytes.get(terminator + 1) == Some(&b'=')).then_some(terminator + 2)
}

fn find_byte(bytes: &[u8], from: usize, target: u8) -> Option<usize> {
    bytes
        .get(from..)?
        .iter()
        .position(|&b| b == target)
        .map(|offset| from + offset)
}

/// Build the selective-fetch plan directly from an IMAP BODYSTRUCTURE tree.
/// Only readable body parts (plain text, HTML, and calendars) enter
/// `text_sections`; PDFs, images, and other files are descriptors only.
pub fn plan_bodystructure(bs: &async_imap::imap_proto::BodyStructure<'_>) -> MimePlan {
    use async_imap::imap_proto::{BodyContentCommon, BodyContentSinglePart, BodyStructure};

    fn param<'a>(params: &'a async_imap::imap_proto::BodyParams<'_>, key: &str) -> Option<&'a str> {
        params.as_ref().and_then(|items| {
            items
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(key))
                .map(|(_, value)| value.as_ref())
        })
    }

    fn encoding(value: &async_imap::imap_proto::ContentEncoding<'_>) -> String {
        use async_imap::imap_proto::ContentEncoding;
        match value {
            ContentEncoding::SevenBit => "7bit".into(),
            ContentEncoding::EightBit => "8bit".into(),
            ContentEncoding::Binary => "binary".into(),
            ContentEncoding::Base64 => "base64".into(),
            ContentEncoding::QuotedPrintable => "quoted-printable".into(),
            ContentEncoding::Other(value) => value.to_ascii_lowercase(),
        }
    }

    fn section(path: &[u32]) -> String {
        if path.is_empty() {
            "1".into()
        } else {
            use std::fmt::Write;
            let mut section = String::new();
            for (index, number) in path.iter().enumerate() {
                if index > 0 {
                    section.push('.');
                }
                write!(&mut section, "{number}").expect("writing to a String cannot fail");
            }
            section
        }
    }

    fn add_single(
        common: &BodyContentCommon<'_>,
        other: &BodyContentSinglePart<'_>,
        path: &[u32],
        plan: &mut MimePlan,
    ) {
        let section = section(path);
        let mut mime_type = format!("{}/{}", common.ty.ty, common.ty.subtype);
        mime_type.make_ascii_lowercase();
        let filename = common
            .disposition
            .as_ref()
            .and_then(|value| param(&value.params, "filename"))
            .or_else(|| param(&common.ty.params, "name"))
            .map(decode_encoded_words);
        let disposition = common.disposition.as_ref().map(|value| value.ty.as_ref());
        let explicit_attachment = disposition
            .is_some_and(|value| value.eq_ignore_ascii_case("attachment"))
            || filename.is_some();
        let content_id = other.id.as_ref().map(|value| normalize_cid(value));
        // A Content-ID alone does not make a part inline: Gmail and Exchange
        // stamp one on every part, including ordinary file attachments, so
        // trusting it hid real attachments (no paperclip, no footer chip).
        // An explicit disposition is authoritative; the Content-ID is only a
        // fallback for parts that declare no disposition at all.
        let is_inline = match disposition {
            Some(value) if value.eq_ignore_ascii_case("attachment") => false,
            Some(value) if value.eq_ignore_ascii_case("inline") => true,
            _ => content_id.is_some(),
        };

        let kind = if mime_type == "text/plain" {
            Some(TextSectionKind::Plain)
        } else if mime_type == "text/html" {
            Some(TextSectionKind::Html)
        } else if mime_type == "text/calendar" {
            Some(TextSectionKind::Calendar)
        } else {
            None
        };

        // A named text file is an attachment, not the message body. Calendar
        // files are the exception: they are fetched for event extraction and
        // also remain visible as an attachment.
        if let Some(kind) =
            kind.filter(|kind| !explicit_attachment || matches!(kind, TextSectionKind::Calendar))
        {
            plan.text_sections.push(PlannedTextSection {
                section: section.clone(),
                kind,
                mime_type: mime_type.clone(),
                charset: param(&common.ty.params, "charset").map(str::to_owned),
                transfer_encoding: encoding(&other.transfer_encoding),
                size: other.octets,
            });
        }

        let is_non_body_part = !mime_type.starts_with("text/") || explicit_attachment;
        if is_non_body_part || (mime_type == "text/calendar" && explicit_attachment) {
            plan.attachments.push(PlannedAttachment {
                section,
                filename,
                mime_type,
                size: other.octets,
                content_id,
                is_inline,
                transfer_encoding: encoding(&other.transfer_encoding),
            });
        }
    }

    fn walk(bs: &BodyStructure<'_>, path: &mut Vec<u32>, plan: &mut MimePlan) {
        match bs {
            BodyStructure::Multipart { bodies, .. } => {
                for (index, body) in bodies.iter().enumerate() {
                    path.push((index + 1) as u32);
                    walk(body, path, plan);
                    path.pop();
                }
            }
            BodyStructure::Basic { common, other, .. }
            | BodyStructure::Text { common, other, .. } => {
                add_single(common, other, path, plan);
            }
            BodyStructure::Message { common, other, .. } => {
                // message/rfc822 is normally a forwarded-message attachment.
                // Treat it atomically; recursing would require the special
                // RFC 3501 MESSAGE section-numbering rules and would bulk-fetch
                // content the user did not ask to open.
                add_single(common, other, path, plan);
            }
        }
    }

    let mut plan = MimePlan::default();
    walk(bs, &mut Vec::new(), &mut plan);
    plan
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedTextSection {
    pub kind: TextSectionKind,
    /// Plain/calendar text, or sanitized HTML for `Html`.
    pub content: String,
}

fn section_entity(mime_header: &[u8], encoded_body: &[u8]) -> Vec<u8> {
    let mut raw = Vec::with_capacity(mime_header.len() + encoded_body.len() + 4);
    raw.extend_from_slice(mime_header);
    if !(mime_header.ends_with(b"\r\n\r\n") || mime_header.ends_with(b"\n\n")) {
        if mime_header.ends_with(b"\r\n") {
            raw.extend_from_slice(b"\r\n");
        } else if mime_header.ends_with(b"\n") {
            raw.extend_from_slice(b"\n");
        } else {
            raw.extend_from_slice(b"\r\n\r\n");
        }
    }
    raw.extend_from_slice(encoded_body);
    raw
}

/// Does the decoded output still read as raw quoted-printable? Counts
/// `=XX` escapes (uppercase hex, as QP mandates); legitimate prose rarely
/// has more than a couple.
fn qp_artifact_count(content: &str) -> usize {
    let bytes = content.as_bytes();
    let mut count = 0;
    let mut i = 0;
    while i + 2 < bytes.len() {
        let is_hex = |b: u8| b.is_ascii_digit() || (b'A'..=b'F').contains(&b);
        if bytes[i] == b'=' && is_hex(bytes[i + 1]) && is_hex(bytes[i + 2]) {
            count += 1;
            i += 3;
        } else {
            i += 1;
        }
    }
    count
}

/// Conservatively identify cached text that still contains a transfer-encoded
/// quoted-printable body. The threshold intentionally matches selective
/// decoding's repair gate: isolated `=XX` strings are common in legitimate
/// prose, while eight escapes strongly indicate an undecoded MIME part.
pub fn looks_like_undecoded_quoted_printable(content: &str) -> bool {
    qp_artifact_count(content) >= 8
}

/// Quoted-printable decoding that never fails: valid `=XX` escapes decode,
/// soft line breaks (`=` + optional transport padding + line end) vanish,
/// and anything malformed passes through literally. Mail gateways that
/// splice disclaimers into bodies (Exchange transport rules) routinely
/// produce QP that strict decoders refuse.
fn lenient_qp_decode(input: &[u8]) -> Vec<u8> {
    let hex = |b: u8| -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'A'..=b'F' => Some(b - b'A' + 10),
            b'a'..=b'f' => Some(b - b'a' + 10),
            _ => None,
        }
    };
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        if input[i] != b'=' {
            out.push(input[i]);
            i += 1;
            continue;
        }
        // Soft break: '=' then optional spaces/tabs, then CRLF or LF.
        let mut j = i + 1;
        while j < input.len() && (input[j] == b' ' || input[j] == b'\t') {
            j += 1;
        }
        if input.get(j) == Some(&b'\r') && input.get(j + 1) == Some(&b'\n') {
            i = j + 2;
            continue;
        }
        if input.get(j) == Some(&b'\n') {
            i = j + 1;
            continue;
        }
        if let (Some(high), Some(low)) = (
            input.get(i + 1).copied().and_then(hex),
            input.get(i + 2).copied().and_then(hex),
        ) {
            out.push(high * 16 + low);
            i += 3;
            continue;
        }
        out.push(b'=');
        i += 1;
    }
    out
}

/// The fetched header block with any Content-Transfer-Encoding lines (and
/// their folded continuations) replaced by `8bit` - for re-parsing a body
/// whose transfer encoding we have already undone by hand.
fn header_with_8bit_cte(
    mime_header: &[u8],
    kind: TextSectionKind,
    declared_mime_type: Option<&str>,
    declared_charset: Option<&str>,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(mime_header.len() + 40);
    let mut dropping = false;
    for line in mime_header.split_inclusive(|&b| b == b'\n') {
        let is_continuation = line.first().is_some_and(|&b| b == b' ' || b == b'\t');
        if dropping && is_continuation {
            continue;
        }
        dropping = line
            .len()
            .checked_sub(26)
            .is_some_and(|_| line[..26].eq_ignore_ascii_case(b"content-transfer-encoding:"));
        if !dropping {
            out.extend_from_slice(line);
        }
    }
    while out.ends_with(b"\r\n") || out.ends_with(b"\n") {
        let cut = if out.ends_with(b"\r\n") { 2 } else { 1 };
        out.truncate(out.len() - cut);
    }
    if !out.is_empty() {
        out.extend_from_slice(b"\r\n");
    }
    let has_content_type = out
        .windows(b"content-type:".len())
        .any(|window| window.eq_ignore_ascii_case(b"content-type:"));
    if !has_content_type {
        let default_mime = match kind {
            TextSectionKind::Plain => "text/plain",
            TextSectionKind::Html => "text/html",
            TextSectionKind::Calendar => "text/calendar",
        };
        let mime_type = declared_mime_type.unwrap_or(default_mime);
        let charset = declared_charset.unwrap_or("utf-8");
        out.extend_from_slice(
            format!("Content-Type: {mime_type}; charset=\"{charset}\"\r\n").as_bytes(),
        );
    }
    out.extend_from_slice(b"Content-Transfer-Encoding: 8bit\r\n\r\n");
    out
}

fn header_declares_qp(mime_header: &[u8]) -> bool {
    mime_header
        .windows(b"quoted-printable".len())
        .any(|window| window.eq_ignore_ascii_case(b"quoted-printable"))
}

fn header_declares_base64(mime_header: &[u8]) -> bool {
    String::from_utf8_lossy(mime_header).lines().any(|line| {
        const PREFIX: &str = "content-transfer-encoding:";
        line.get(..PREFIX.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(PREFIX))
            && line
                .get(PREFIX.len()..)
                .is_some_and(|value| value.trim().eq_ignore_ascii_case("base64"))
    })
}

fn decode_base64_transfer(input: &[u8]) -> Option<Vec<u8>> {
    use base64::Engine;

    let compact = if input.iter().any(u8::is_ascii_whitespace) {
        std::borrow::Cow::Owned(
            input
                .iter()
                .copied()
                .filter(|byte| !byte.is_ascii_whitespace())
                .collect::<Vec<_>>(),
        )
    } else {
        std::borrow::Cow::Borrowed(input)
    };
    if compact.is_empty() {
        return Some(Vec::new());
    }
    base64::engine::general_purpose::STANDARD
        .decode(&compact)
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(&compact))
        .ok()
}

fn decoded_looks_like_html(decoded: &[u8]) -> bool {
    let decoded = decoded.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(decoded);
    let start = decoded
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(decoded.len());
    let probe = &decoded[start..decoded.len().min(start.saturating_add(1024))];
    let starts_with = |prefix: &[u8]| {
        probe
            .get(..prefix.len())
            .is_some_and(|value| value.eq_ignore_ascii_case(prefix))
    };
    [
        b"<!doctype html".as_slice(),
        b"<html",
        b"<head",
        b"<body",
        b"<meta",
        b"<style",
    ]
    .into_iter()
    .any(starts_with)
        || (starts_with(b"<?xml")
            && probe
                .windows(b"<html".len())
                .any(|value| value.eq_ignore_ascii_case(b"<html")))
}

fn decoded_looks_like_calendar(decoded: &[u8]) -> bool {
    let decoded = decoded.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(decoded);
    let start = decoded
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(decoded.len());
    decoded[start..]
        .get(..15)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"BEGIN:VCALENDAR"))
}

fn decoded_matches_kind(kind: TextSectionKind, decoded: &[u8]) -> bool {
    match kind {
        TextSectionKind::Html => decoded_looks_like_html(decoded),
        TextSectionKind::Calendar => decoded_looks_like_calendar(decoded),
        // Plain text can legitimately be a Base64 token, so never infer its
        // transfer encoding without BODYSTRUCTURE or a MIME header.
        TextSectionKind::Plain => false,
    }
}

/// Conservatively identify the bad-cache signature produced when a Base64
/// HTML part was stored literally. Intended for a one-shot recovery scan that
/// resets matching cached rows to `body_state = 'none'` for selective refetch.
pub fn looks_like_base64_encoded_html(content: &str) -> bool {
    if content.len() < 32 || content.as_bytes().contains(&b'<') {
        return false;
    }
    decode_base64_transfer(content.as_bytes())
        .is_some_and(|decoded| decoded_looks_like_html(&decoded))
}

/// Decode transfer encoding and charset for one fetched text MIME entity.
/// HTML is sanitized through the same allow-list as full-message parsing.
///
/// `declared_encoding` is the BODYSTRUCTURE's transfer encoding for the part.
/// It backstops gateway-mangled mail (Exchange disclaimer banners and the
/// like), where the part's own header and body drift out of sync: if the part
/// is quoted-printable by either account and the parsed output still reads as
/// raw QP, the body is decoded leniently by hand and re-parsed for charset.
pub fn decode_text_section(
    kind: TextSectionKind,
    declared_encoding: Option<&str>,
    mime_header: &[u8],
    encoded_body: &[u8],
) -> Result<DecodedTextSection> {
    decode_text_section_inner(
        kind,
        declared_encoding,
        None,
        None,
        mime_header,
        encoded_body,
    )
}

/// Decode a fetched text part using the complete BODYSTRUCTURE plan as the
/// authoritative fallback when Outlook omits `BODY[section.MIME]` or returns
/// it without Content-Transfer-Encoding.
pub fn decode_planned_text_section(
    planned: &PlannedTextSection,
    mime_header: &[u8],
    encoded_body: &[u8],
) -> Result<DecodedTextSection> {
    decode_text_section_inner(
        planned.kind,
        Some(&planned.transfer_encoding),
        Some(&planned.mime_type),
        planned.charset.as_deref(),
        mime_header,
        encoded_body,
    )
}

fn parse_text_entity(mime_header: &[u8], body: &[u8]) -> Result<String> {
    let raw = section_entity(mime_header, body);
    let msg = mail_parser::MessageParser::default()
        .parse(&raw)
        .ok_or_else(|| CoreError::Mime("unparseable MIME section".into()))?;
    Ok(msg
        .root_part()
        .text_contents()
        .map(str::to_owned)
        .unwrap_or_else(|| String::from_utf8_lossy(msg.root_part().contents()).into_owned()))
}

fn decode_text_section_inner(
    kind: TextSectionKind,
    declared_encoding: Option<&str>,
    declared_mime_type: Option<&str>,
    declared_charset: Option<&str>,
    mime_header: &[u8],
    encoded_body: &[u8],
) -> Result<DecodedTextSection> {
    let base64_declared = header_declares_base64(mime_header)
        || declared_encoding.is_some_and(|value| value.eq_ignore_ascii_case("base64"));
    let decoded_base64 = if base64_declared {
        Some(
            decode_base64_transfer(encoded_body)
                .ok_or_else(|| CoreError::Mime("invalid Base64 selective MIME section".into()))?,
        )
    } else {
        // Legacy callers may not yet pass BODYSTRUCTURE metadata. Infer only
        // strong structured signatures; never guess for arbitrary plain text.
        decode_base64_transfer(encoded_body).filter(|decoded| decoded_matches_kind(kind, decoded))
    };

    let mut content = if let Some(decoded) = decoded_base64.as_deref() {
        let fixed_header =
            header_with_8bit_cte(mime_header, kind, declared_mime_type, declared_charset);
        parse_text_entity(&fixed_header, decoded)?
    } else {
        parse_text_entity(mime_header, encoded_body)?
    };

    let qp_declared = header_declares_qp(mime_header)
        || declared_encoding.is_some_and(|value| value.eq_ignore_ascii_case("quoted-printable"));
    if decoded_base64.is_none() && qp_declared && looks_like_undecoded_quoted_printable(&content) {
        let decoded = lenient_qp_decode(encoded_body);
        let fixed_header =
            header_with_8bit_cte(mime_header, kind, declared_mime_type, declared_charset);
        if let Ok(reparsed) = parse_text_entity(&fixed_header, &decoded) {
            content = reparsed;
        }
    }

    Ok(DecodedTextSection {
        kind,
        content: if kind == TextSectionKind::Html {
            sanitize_html(&content)
        } else {
            content
        },
    })
}

/// Decode one on-demand attachment MIME entity without parsing or retaining a
/// complete RFC 5322 message.
pub fn decode_attachment_section(mime_header: &[u8], encoded_body: &[u8]) -> Result<Vec<u8>> {
    let raw = section_entity(mime_header, encoded_body);
    let msg = mail_parser::MessageParser::default()
        .parse(&raw)
        .ok_or_else(|| CoreError::Mime("unparseable attachment MIME section".into()))?;
    Ok(msg.root_part().contents().to_vec())
}

#[cfg(test)]
mod selective_section_tests {
    use super::*;
    use async_imap::imap_proto::{
        BodyContentCommon, BodyContentSinglePart, BodyStructure, ContentDisposition,
        ContentEncoding, ContentType,
    };
    use std::borrow::Cow;

    fn common(
        ty: &'static str,
        subtype: &'static str,
        params: &[(&'static str, &'static str)],
        disposition: Option<(&'static str, &'static str)>,
    ) -> BodyContentCommon<'static> {
        BodyContentCommon {
            ty: ContentType {
                ty: Cow::Borrowed(ty),
                subtype: Cow::Borrowed(subtype),
                params: (!params.is_empty()).then(|| {
                    params
                        .iter()
                        .map(|(key, value)| (Cow::Borrowed(*key), Cow::Borrowed(*value)))
                        .collect()
                }),
            },
            disposition: disposition.map(|(ty, filename)| ContentDisposition {
                ty: Cow::Borrowed(ty),
                params: Some(vec![(Cow::Borrowed("filename"), Cow::Borrowed(filename))]),
            }),
            language: None,
            location: None,
        }
    }

    fn other(
        id: Option<&'static str>,
        transfer_encoding: ContentEncoding<'static>,
        octets: u32,
    ) -> BodyContentSinglePart<'static> {
        BodyContentSinglePart {
            id: id.map(Cow::Borrowed),
            md5: None,
            description: None,
            transfer_encoding,
            octets,
        }
    }

    fn text(
        subtype: &'static str,
        params: &[(&'static str, &'static str)],
        disposition: Option<(&'static str, &'static str)>,
        encoding: ContentEncoding<'static>,
        octets: u32,
    ) -> BodyStructure<'static> {
        BodyStructure::Text {
            common: common("text", subtype, params, disposition),
            other: other(None, encoding, octets),
            lines: 1,
            extension: None,
        }
    }

    fn basic(
        ty: &'static str,
        subtype: &'static str,
        disposition: Option<(&'static str, &'static str)>,
        id: Option<&'static str>,
        octets: u32,
    ) -> BodyStructure<'static> {
        BodyStructure::Basic {
            common: common(ty, subtype, &[], disposition),
            other: other(id, ContentEncoding::Base64, octets),
            extension: None,
        }
    }

    #[test]
    fn bodystructure_plan_fetches_only_readable_sections() {
        let structure = BodyStructure::Multipart {
            common: common("multipart", "mixed", &[], None),
            bodies: vec![
                BodyStructure::Multipart {
                    common: common("multipart", "alternative", &[], None),
                    bodies: vec![
                        text(
                            "plain",
                            &[("charset", "iso-8859-1")],
                            None,
                            ContentEncoding::QuotedPrintable,
                            20,
                        ),
                        text("html", &[], None, ContentEncoding::Base64, 40),
                    ],
                    extension: None,
                },
                text(
                    "calendar",
                    &[("charset", "utf-8")],
                    Some(("attachment", "invite.ics")),
                    ContentEncoding::EightBit,
                    80,
                ),
                basic(
                    "image",
                    "png",
                    Some(("inline", "logo.png")),
                    Some("<logo@cid>"),
                    100,
                ),
                basic(
                    "application",
                    "pdf",
                    Some(("attachment", "report.pdf")),
                    None,
                    5_000,
                ),
            ],
            extension: None,
        };

        let plan = plan_bodystructure(&structure);
        assert_eq!(plan.version, MIME_PLAN_VERSION);
        assert_eq!(plan.text_section_ids(), vec!["1.1", "1.2", "2"]);
        assert_eq!(
            plan.text_sections
                .iter()
                .map(|part| part.kind)
                .collect::<Vec<_>>(),
            vec![
                TextSectionKind::Plain,
                TextSectionKind::Html,
                TextSectionKind::Calendar
            ]
        );
        assert_eq!(
            plan.attachments
                .iter()
                .map(|part| (part.section.as_str(), part.is_inline))
                .collect::<Vec<_>>(),
            vec![("2", false), ("3", true), ("4", false)]
        );
        assert_eq!(plan.attachments[1].content_id.as_deref(), Some("logo@cid"));
        assert!(plan.has_file_attachments());

        let json = serde_json::to_string(&plan).unwrap();
        assert_eq!(serde_json::from_str::<MimePlan>(&json).unwrap(), plan);
    }

    #[test]
    fn content_id_does_not_make_a_disposed_attachment_inline() {
        // Gmail and Exchange stamp a Content-ID on ordinary attachments; the
        // explicit disposition decides, so the paperclip still shows.
        let structure = BodyStructure::Multipart {
            common: common("multipart", "mixed", &[], None),
            bodies: vec![
                text("plain", &[], None, ContentEncoding::SevenBit, 20),
                basic(
                    "application",
                    "vnd.openxmlformats-officedocument.wordprocessingml.document",
                    Some(("attachment", "raport.docx")),
                    Some("<f_mqkxxopb0>"),
                    1_700_000,
                ),
            ],
            extension: None,
        };

        let plan = plan_bodystructure(&structure);
        assert_eq!(plan.attachments.len(), 1);
        assert!(!plan.attachments[0].is_inline);
        assert_eq!(
            plan.attachments[0].content_id.as_deref(),
            Some("f_mqkxxopb0")
        );
        assert!(plan.has_file_attachments());
    }

    #[test]
    fn content_id_without_a_disposition_still_reads_as_inline() {
        let structure = basic("image", "png", None, Some("<logo@cid>"), 100);
        let plan = plan_bodystructure(&structure);
        assert!(plan.attachments[0].is_inline);
        assert!(!plan.has_file_attachments());
    }

    #[test]
    fn decodes_rfc2047_attachment_filename() {
        // `w6nDqQ==` is the Base64 of the UTF-8 bytes for "éé".
        let structure = basic(
            "application",
            "pdf",
            Some(("attachment", "=?utf-8?B?w6nDqQ==?=")),
            None,
            5_000,
        );
        let plan = plan_bodystructure(&structure);
        assert_eq!(plan.attachments.len(), 1);
        assert_eq!(plan.attachments[0].filename.as_deref(), Some("éé"));
    }

    #[test]
    fn decode_encoded_words_covers_common_shapes() {
        // Plain ASCII is returned untouched (fast path).
        assert_eq!(decode_encoded_words("report.pdf"), "report.pdf");
        // Base64 UTF-8.
        assert_eq!(
            decode_encoded_words("=?utf-8?b?VGjDrXMgw61zIHbDoWzDrWQgw5pURjg=?="),
            "Thís ís válíd ÚTF8"
        );
        // Quoted-printable with a non-UTF-8 charset.
        assert_eq!(
            decode_encoded_words("=?iso-8859-1?q?Olle_J=E4rnefors?="),
            "Olle Järnefors"
        );
        // Encoded-word surrounded by literal text.
        assert_eq!(
            decode_encoded_words("report-=?UTF-8?B?w6nDqQ==?=.pdf"),
            "report-éé.pdf"
        );
        // Whitespace between two adjacent encoded-words is dropped (RFC 2047 §6.2);
        // `w6k=` is the Base64 of the UTF-8 bytes for "é".
        assert_eq!(
            decode_encoded_words("=?utf-8?B?w6k=?= =?utf-8?B?w6k=?="),
            "éé"
        );
        // A malformed token is left verbatim rather than swallowing the tail.
        assert_eq!(decode_encoded_words("=?utf-8?bogus"), "=?utf-8?bogus");
    }

    #[test]
    fn decodes_charset_and_sanitizes_html_sections() {
        let plain = decode_text_section(
            TextSectionKind::Plain,
            Some("quoted-printable"),
            b"Content-Type: text/plain; charset=iso-8859-1\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\n",
            b"Ol=E1 mundo",
        )
        .unwrap();
        assert_eq!(plain.content, "Ol\u{e1} mundo");

        let html = decode_text_section(
            TextSectionKind::Html,
            Some("8bit"),
            b"Content-Type: text/html; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n",
            b"<p onclick=\"steal()\">Hello</p><script>steal()</script>",
        )
        .unwrap();
        assert!(html.content.contains("<p>Hello</p>"));
        assert!(!html.content.contains("onclick"));
        assert!(!html.content.contains("<script"));
    }

    #[test]
    fn decodes_outlook_base64_html_when_mime_header_omits_cte() {
        use base64::Engine;

        let source = b"<html>\r\n<head><title>Outlook</title></head>\r\n<body><div>Hello from Outlook</div><script>bad()</script></body></html>";
        let encoded = base64::engine::general_purpose::STANDARD.encode(source);
        assert!(encoded.starts_with("PGh0bWw+DQo8"));
        let planned = PlannedTextSection {
            section: "1".into(),
            kind: TextSectionKind::Html,
            mime_type: "text/html".into(),
            charset: Some("utf-8".into()),
            transfer_encoding: "base64".into(),
            size: encoded.len() as u32,
        };

        // Office 365 occasionally returns no BODY[1.MIME] data even though
        // BODYSTRUCTURE correctly reports BASE64. The plan must win.
        let decoded = decode_planned_text_section(&planned, b"", encoded.as_bytes()).unwrap();
        assert!(decoded.content.contains("Hello from Outlook"));
        assert!(!decoded.content.contains("PGh0bWw"));
        assert!(!decoded.content.contains("<script"));

        // Legacy callers without plan metadata remain recoverable, but only
        // because the decoded bytes carry a strong full-HTML signature.
        let inferred =
            decode_text_section(TextSectionKind::Html, None, b"", encoded.as_bytes()).unwrap();
        assert!(inferred.content.contains("Hello from Outlook"));
    }

    #[test]
    fn cached_base64_html_detection_is_conservative() {
        use base64::Engine;

        let encoded_html = base64::engine::general_purpose::STANDARD
            .encode(b"<html><body>A cached Outlook message</body></html>");
        let encoded_plain =
            base64::engine::general_purpose::STANDARD.encode(b"ordinary plain text payload");
        let encoded_meta = base64::engine::general_purpose::STANDARD
            .encode(b"<meta charset=\"utf-8\"><div>Outlook fragment</div>");
        assert!(looks_like_base64_encoded_html(&encoded_html));
        assert!(looks_like_base64_encoded_html(&encoded_meta));
        assert!(!looks_like_base64_encoded_html(&encoded_plain));
        assert!(!looks_like_base64_encoded_html(
            "<html><body>Already decoded</body></html>"
        ));
        assert!(!looks_like_base64_encoded_html("PGh0bWw+"));
    }

    #[test]
    fn recovers_gateway_mangled_quoted_printable() {
        // An Exchange-style disclaimer rewrite: the part header lost its
        // Content-Transfer-Encoding, the body kept hundreds of QP escapes,
        // and soft breaks picked up transport padding ("= " before the line
        // end). BODYSTRUCTURE still declares quoted-printable.
        let body = b"Qu=E1=BA=A3ng c=C3=A1o =E2=80=8D=E2=80=8D=E2=80=8D=E2=80=8D= \r\n=E2=80=8D=E2=80=8D=E2=80=8D=E2=80=8Dduy=E1=BB=87t"
            .to_vec();
        assert!(looks_like_undecoded_quoted_printable(
            std::str::from_utf8(&body).unwrap()
        ));
        assert!(!looks_like_undecoded_quoted_printable(
            "A legitimate short token =3D and price =20"
        ));
        let decoded = decode_text_section(
            TextSectionKind::Plain,
            Some("quoted-printable"),
            b"Content-Type: text/plain; charset=utf-8\r\n\r\n",
            &body,
        )
        .unwrap();
        assert!(decoded.content.starts_with("Qu\u{1ea3}ng c\u{e1}o"));
        assert!(decoded.content.ends_with("duy\u{1ec7}t"));
        assert!(!decoded.content.contains("=E1"));
        assert!(!decoded.content.contains("= "));
    }

    #[test]
    fn decodes_one_attachment_entity() {
        let bytes = decode_attachment_section(
            b"Content-Type: application/octet-stream\r\nContent-Transfer-Encoding: base64\r\n\r\n",
            b"AAEC/w==",
        )
        .unwrap();
        assert_eq!(bytes, vec![0, 1, 2, 255]);
    }
}

#[derive(Debug, Clone, Default)]
pub struct ParsedBody {
    pub headers: ParsedHeaders,
    pub text: Option<String>,
    /// Sanitized HTML, safe for the native HTML renderer.
    pub html: Option<String>,
    pub snippet: String,
    pub attachments: Vec<ParsedAttachment>,
    /// Raw text of any text/calendar parts (meeting invites).
    pub calendar_parts: Vec<String>,
    /// The message is a read receipt that came back.
    pub disposition_report: Option<DispositionReport>,
}

fn addr_from(a: &mail_parser::Addr) -> Option<Address> {
    a.address().map(|e| Address {
        name: a.name().map(|n| n.to_string()),
        email: e.to_string(),
    })
}

fn collect_addrs(value: Option<&mail_parser::Address>) -> Vec<Address> {
    let mut out = Vec::new();
    if let Some(addr) = value {
        match addr {
            mail_parser::Address::List(list) => {
                out.extend(list.iter().filter_map(addr_from));
            }
            mail_parser::Address::Group(groups) => {
                for g in groups {
                    out.extend(g.addresses.iter().filter_map(addr_from));
                }
            }
        }
    }
    out
}

static SANITIZER: Lazy<ammonia::Builder<'static>> = Lazy::new(|| {
    let mut b = ammonia::Builder::default();
    b.add_generic_attributes([
        "style",
        "class",
        "id",
        "align",
        "valign",
        "width",
        "height",
        "border",
        "cellpadding",
        "cellspacing",
        "bgcolor",
        "color",
        // Text direction, as on every element in browsers' and other mail
        // clients' sanitizers: right-to-left mail depends on it.
        "dir",
    ])
    .add_tags([
        "html", "head", "body", "meta", "title", "table", "thead", "tbody", "tfoot", "tr", "td",
        "th", "center", "font", "img", "style",
    ])
    .add_tag_attributes("meta", ["name", "content", "http-equiv"])
    .add_tag_attributes("img", ["src", "alt", "width", "height"])
    .add_tag_attributes("font", ["face", "size", "color"])
    .add_tag_attributes("a", ["href", "title"])
    .url_schemes(std::collections::HashSet::from([
        "http", "https", "mailto", "tel", "cid", "data",
    ]))
    // CSS is needed by the native Blitz renderer for email typography and
    // layout. It is still inert here because executable script content stays
    // stripped and the renderer does not execute HTML.
    .clean_content_tags(std::collections::HashSet::from(["script"]))
    .link_rel(Some("noopener noreferrer"));
    b
});

pub fn sanitize_html(html: &str) -> String {
    let Some(body) = html_document_section(html, "body") else {
        return SANITIZER.clean(html).to_string();
    };

    // Ammonia intentionally returns an HTML fragment. Passing it a complete
    // document therefore discards html/body attributes and unwraps the head;
    // text from metadata such as <title> then becomes visible body content.
    // Sanitize the inert head and renderable body independently, then restore
    // their semantic boundary. This also keeps class/id hooks meaningful for
    // the sanitized stylesheet without special-casing email template names.
    let head = html_document_section(html, "head")
        .map(|section| SANITIZER.clean(section.content).to_string())
        .unwrap_or_default();
    let body_content = SANITIZER.clean(body.content).to_string();
    let body_attributes = sanitize_container_attributes(body.attributes);
    let html_attributes = html_document_section(html, "html")
        .map(|section| sanitize_container_attributes(section.attributes))
        .unwrap_or_default();

    let capacity = "<html><head></head><body></body></html>".len()
        + html_attributes.len()
        + head.len()
        + body_attributes.len()
        + body_content.len();
    let mut sanitized = String::with_capacity(capacity);
    for part in [
        "<html",
        &html_attributes,
        "><head>",
        &head,
        "</head><body",
        &body_attributes,
        ">",
        &body_content,
        "</body></html>",
    ] {
        sanitized.push_str(part);
    }
    sanitized
}

#[derive(Clone, Copy)]
struct HtmlDocumentSection<'a> {
    attributes: &'a str,
    content: &'a str,
}

/// Locate one top-level HTML document element without trying to sanitize with
/// string substitution. Tag ends honor quoted attribute values, and ASCII
/// case-folding preserves byte offsets into the original UTF-8 source.
fn html_document_section<'a>(html: &'a str, tag: &str) -> Option<HtmlDocumentSection<'a>> {
    let open_start = find_html_tag(html, tag, 0, false)?;
    let open_end = html_tag_end(html, open_start)?;
    let name_end = open_start + 1 + tag.len();
    let attributes = html.get(name_end..open_end)?;

    // The html element normally encloses the whole document and its explicit
    // closing tag is optional. Its content is not used by the caller, but the
    // same representation keeps attribute extraction consistent.
    let close_start = find_html_tag(html, tag, open_end + 1, true).unwrap_or(html.len());
    let content = html.get(open_end + 1..close_start)?;
    Some(HtmlDocumentSection {
        attributes,
        content,
    })
}

fn find_html_tag(html: &str, tag: &str, from: usize, closing: bool) -> Option<usize> {
    let bytes = html.as_bytes();
    let name_offset = if closing { 2 } else { 1 };
    let mut cursor = from;
    while let Some(relative) = bytes.get(cursor..)?.iter().position(|&byte| byte == b'<') {
        let start = cursor + relative;
        let name_start = start + name_offset;
        let name_end = name_start.checked_add(tag.len())?;
        let closing_matches = !closing || bytes.get(start + 1) == Some(&b'/');
        if closing_matches
            && bytes
                .get(name_start..name_end)
                .is_some_and(|name| name.eq_ignore_ascii_case(tag.as_bytes()))
            && bytes
                .get(name_end)
                .is_none_or(|byte| *byte == b'>' || *byte == b'/' || byte.is_ascii_whitespace())
        {
            return Some(start);
        }
        cursor = start + 1;
    }
    None
}

fn html_tag_end(html: &str, start: usize) -> Option<usize> {
    let mut quote = None;
    for (relative, byte) in html.as_bytes().get(start..)?.iter().copied().enumerate() {
        match (quote, byte) {
            (None, b'\'' | b'"') => quote = Some(byte),
            (Some(active), value) if active == value => quote = None,
            (None, b'>') => return Some(start + relative),
            _ => {}
        }
    }
    None
}

/// Reuse Ammonia's attribute policy by cleaning the attributes on an allowed
/// neutral element, then transplanting the serialized safe attributes onto
/// html/body. Event handlers and other unknown attributes never pass through.
fn sanitize_container_attributes(attributes: &str) -> String {
    if attributes.trim().is_empty() {
        return String::new();
    }
    let probe = format!("<div{attributes}></div>");
    let cleaned = SANITIZER.clean(&probe).to_string();
    let Some(start) = find_html_tag(&cleaned, "div", 0, false) else {
        return String::new();
    };
    let Some(end) = html_tag_end(&cleaned, start) else {
        return String::new();
    };
    cleaned[start + 4..end].to_owned()
}

pub(crate) fn collapse_whitespace(text: &str, max_chars: Option<usize>) -> String {
    let limit = max_chars.unwrap_or(usize::MAX);
    let mut output = String::with_capacity(text.len().min(limit));
    let mut written = 0_usize;
    let mut pending_space = false;
    for character in text.chars() {
        if character.is_whitespace() {
            pending_space = !output.is_empty();
            continue;
        }
        if pending_space {
            if written == limit {
                break;
            }
            output.push(' ');
            written += 1;
            pending_space = false;
        }
        if written == limit {
            break;
        }
        output.push(character);
        written += 1;
    }
    output
}

pub fn make_snippet(text: &str) -> String {
    collapse_whitespace(text, Some(140))
}

/// Convert HTML into readable preview text before applying the normal snippet
/// length and whitespace rules. `ammonia::clean_text` is an HTML *escaper*,
/// not a tag stripper, and caused previews such as `&#10;&lt;div...`.
pub fn make_html_snippet(html: &str) -> String {
    make_snippet(&mail_parser::decoders::html::html_to_text(html))
}

/// Build a preview with the same precedence everywhere. A whitespace-only
/// plain alternative carries no useful preview, so fall back to HTML.
pub fn make_body_snippet(text: Option<&str>, html: Option<&str>) -> String {
    body_text(text, html)
        .map(|text| make_snippet(&text))
        .unwrap_or_default()
}

/// The readable text of a body, chosen the way previews are.
pub fn body_text<'a>(
    text: Option<&'a str>,
    html: Option<&str>,
) -> Option<std::borrow::Cow<'a, str>> {
    text.filter(|value| !value.trim().is_empty())
        .map(std::borrow::Cow::Borrowed)
        .or_else(|| {
            html.map(|html| {
                std::borrow::Cow::Owned(mail_parser::decoders::html::html_to_text(html))
            })
        })
}

/// `src="cid:<id>"` / `src='cid:<id>'` in a body (id in group 2 or 3).
static CID_SRC: Lazy<regex::Regex> =
    Lazy::new(|| regex::Regex::new(r#"(?i)src\s*=\s*(?:"cid:([^"]+)"|'cid:([^']+)')"#).unwrap());

/// Normalize a Content-ID / `cid:` reference for matching (drop `<>` + spaces).
pub fn normalize_cid(id: &str) -> String {
    id.trim().trim_matches(|c| c == '<' || c == '>').to_string()
}

/// Normalized Content-IDs that are actually referenced by image sources.
/// Callers use this before loading attachment blobs so unrelated inline parts
/// never consume I/O or memory merely because they share the message.
pub fn referenced_cids(html: &str) -> std::collections::HashSet<String> {
    CID_SRC
        .captures_iter(html)
        .filter_map(|captures| captures.get(1).or_else(|| captures.get(2)))
        .map(|value| normalize_cid(value.as_str()))
        .collect()
}

/// Rewrite inline `src="cid:<id>"` references to `data:` URIs using `map`
/// (keyed by [`normalize_cid`]). Unknown cids are left untouched - the browser
/// still can't fetch them, but nothing else is changed.
pub fn rewrite_cid_src(html: &str, map: &std::collections::HashMap<String, String>) -> String {
    let data_for = |caps: &regex::Captures| {
        let cid = caps.get(1).or_else(|| caps.get(2))?.as_str();
        map.get(cid.trim().trim_matches(|c| c == '<' || c == '>'))
    };
    let mut capacity = html.len();
    let mut found = false;
    for caps in CID_SRC.captures_iter(html) {
        let Some(data_uri) = data_for(&caps) else {
            continue;
        };
        found = true;
        capacity = capacity
            .checked_add("src=\"\"".len())
            .and_then(|size| size.checked_add(data_uri.len()))
            .and_then(|size| size.checked_sub(caps.get(0).unwrap().len()))
            .expect("rewritten CID HTML length overflow");
    }
    if !found {
        return html.to_owned();
    }
    let mut rewritten = String::with_capacity(capacity);
    let mut offset = 0;
    for caps in CID_SRC.captures_iter(html) {
        let whole = caps.get(0).unwrap();
        rewritten.push_str(&html[offset..whole.start()]);
        if let Some(data_uri) = data_for(&caps) {
            rewritten.push_str("src=\"");
            rewritten.push_str(data_uri);
            rewritten.push('"');
        } else {
            rewritten.push_str(whole.as_str());
        }
        offset = whole.end();
    }
    rewritten.push_str(&html[offset..]);
    rewritten
}

/// Write cached CID image bytes directly into the final HTML. This avoids a
/// second set of large base64 strings while the rewritten document is built.
pub fn rewrite_cid_src_with_images(
    html: &str,
    map: &std::collections::HashMap<String, (String, Vec<u8>)>,
) -> String {
    use base64::Engine;

    let image_for = |caps: &regex::Captures| {
        let cid = caps.get(1).or_else(|| caps.get(2))?.as_str();
        map.get(cid.trim().trim_matches(|c| c == '<' || c == '>'))
    };
    let mut capacity = html.len();
    let mut found = false;
    for caps in CID_SRC.captures_iter(html) {
        let Some((mime, bytes)) = image_for(&caps) else {
            continue;
        };
        found = true;
        let encoded_len = base64::encoded_len(bytes.len(), true)
            .expect("bounded inline CID image cannot overflow base64 length");
        let replacement_len = "src=\"data:;base64,\"".len() + mime.len() + encoded_len;
        capacity = capacity
            .checked_add(replacement_len)
            .and_then(|size| size.checked_sub(caps.get(0).unwrap().len()))
            .expect("rewritten CID HTML length overflow");
    }
    if !found {
        return html.to_owned();
    }
    let mut rewritten = String::with_capacity(capacity);
    let mut offset = 0;
    for caps in CID_SRC.captures_iter(html) {
        let whole = caps.get(0).unwrap();
        rewritten.push_str(&html[offset..whole.start()]);
        if let Some((mime, bytes)) = image_for(&caps) {
            rewritten.push_str("src=\"data:");
            rewritten.push_str(mime);
            rewritten.push_str(";base64,");
            base64::engine::general_purpose::STANDARD.encode_string(bytes, &mut rewritten);
            rewritten.push('"');
        } else {
            rewritten.push_str(whole.as_str());
        }
        offset = whole.end();
    }
    rewritten.push_str(&html[offset..]);
    rewritten
}

#[cfg(test)]
mod cid_tests {
    use super::{normalize_cid, referenced_cids, rewrite_cid_src, rewrite_cid_src_with_images};
    use std::collections::HashMap;

    #[test]
    fn rewrites_known_cid_and_leaves_unknown() {
        let mut map = HashMap::new();
        map.insert(
            "logo@x".to_string(),
            "data:image/png;base64,AAAA".to_string(),
        );
        let html = r#"<img src="cid:logo@x"><img src='cid:missing@y'>"#;
        let out = rewrite_cid_src(html, &map);
        assert!(out.contains(r#"src="data:image/png;base64,AAAA""#));
        assert!(
            out.contains("cid:missing@y"),
            "unknown cid untouched: {out}"
        );
    }

    #[test]
    fn byte_rewrite_matches_string_rewrite_with_repeated_and_unknown_cids() {
        use base64::Engine;

        let mut bytes = HashMap::new();
        bytes.insert(
            "logo@x".to_string(),
            ("image/png".to_string(), vec![0, 1, 2, 255]),
        );
        let uri = format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode([0, 1, 2, 255])
        );
        let strings = HashMap::from([("logo@x".to_string(), uri)]);
        let html = "é<img src='cid:<logo@x>'><img SRC=\"cid:logo@x\"><img src='cid:unknown'>尾";
        assert_eq!(
            rewrite_cid_src_with_images(html, &bytes),
            rewrite_cid_src(html, &strings)
        );
    }

    #[test]
    fn collects_only_cids_used_by_image_sources() {
        let cids = referenced_cids(
            r#"<a href="cid:not-an-image">x</a><img src='cid:<used@host>'><img SRC="cid:two">"#,
        );
        assert_eq!(cids.len(), 2);
        assert!(cids.contains("used@host"));
        assert!(cids.contains("two"));
    }

    #[test]
    fn matches_angle_bracketed_content_id() {
        let mut map = HashMap::new();
        map.insert(
            normalize_cid("<abc@host>"),
            "data:image/gif;base64,BBBB".to_string(),
        );
        let out = rewrite_cid_src(r#"<img src="cid:abc@host">"#, &map);
        assert!(out.contains("data:image/gif;base64,BBBB"));
    }
}

/// `d=` tag of a DKIM-Signature header (the signing domain).
static DKIM_DOMAIN: Lazy<regex::Regex> =
    Lazy::new(|| regex::Regex::new(r"(?i)\bd\s*=\s*([A-Za-z0-9._-]+)").unwrap());

/// Domain part of an email, or the input itself when it's already a bare domain.
fn domain_of(addr_or_domain: &str) -> &str {
    addr_or_domain.rsplit('@').next().unwrap_or(addr_or_domain)
}

/// Relaxed alignment (as in DMARC): equal, or one is a subdomain of the other
/// (`mailer.substack.com` aligns with `substack.com`). Case-insensitive.
fn domains_aligned(a: &str, b: &str) -> bool {
    let a = a.trim().trim_end_matches('.');
    let b = b.trim().trim_end_matches('.');
    fn subdomain(candidate: &str, parent: &str) -> bool {
        candidate
            .len()
            .checked_sub(parent.len())
            .is_some_and(|offset| {
                offset > 0
                    && candidate.as_bytes()[offset - 1] == b'.'
                    && candidate.as_bytes()[offset..].eq_ignore_ascii_case(parent.as_bytes())
            })
    }
    a.eq_ignore_ascii_case(b) || subdomain(a, b) || subdomain(b, a)
}

/// The transmitting party to show as "via", when it doesn't align with From:.
/// Order: Sender: (self-declared), Return-Path (envelope sender, stamped by
/// the receiving server - a forged From: can't easily hide it), DKIM d=.
fn resolve_via(msg: &mail_parser::Message, from: Option<&Address>) -> Option<String> {
    let from_domain = from.map(|a| domain_of(&a.email));

    let sender_email = msg
        .sender()
        .and_then(|s| s.first())
        .and_then(|address| address.address());
    let return_path = match msg.return_path() {
        mail_parser::HeaderValue::Text(t) => Some(t.trim_matches(['<', '>', ' '])),
        mail_parser::HeaderValue::TextList(l) => l.last().map(|t| t.trim_matches(['<', '>', ' '])),
        _ => None,
    }
    .filter(|s| !s.is_empty());
    let dkim_domain = msg
        .header("DKIM-Signature")
        .and_then(|h| h.as_text())
        .and_then(|s| DKIM_DOMAIN.captures(s))
        .and_then(|captures| captures.get(1).map(|domain| domain.as_str()));

    [sender_email, return_path, dkim_domain]
        .into_iter()
        .flatten()
        .find(|cand| match from_domain {
            Some(fd) => !domains_aligned(domain_of(cand), fd),
            None => true,
        })
        .map(str::to_owned)
}

#[cfg(test)]
mod read_receipt_tests {
    use super::{ReadReceipt, build_read_receipt};
    use crate::models::Address;

    fn build(user_agreed: bool, original_message_id: Option<&str>) -> String {
        let recipient = Address {
            name: Some("Me".into()),
            email: "me@example.org".into(),
        };
        let to = [Address {
            name: Some("Ana".into()),
            email: "ana@example.com".into(),
        }];
        let (id, raw) = build_read_receipt(&ReadReceipt {
            recipient: &recipient,
            to: &to,
            original_subject: "Quarterly plan",
            original_message_id,
            user_agreed,
            reporting_domain: "example.org",
        })
        .unwrap();
        assert!(id.starts_with('<') && id.ends_with("@example.org>"));
        String::from_utf8(raw).unwrap()
    }

    #[test]
    fn a_receipt_is_a_disposition_report_about_the_original() {
        let raw = build(true, Some("<plan@example.com>"));
        assert!(raw.contains("multipart/report"));
        assert!(
            raw.contains("report-type=\"disposition-notification\"")
                || raw.contains("report-type=disposition-notification")
        );
        assert!(raw.contains("Content-Type: message/disposition-notification"));
        assert!(raw.contains("Reporting-UA: example.org; Flectar Mail\r\n"));
        assert!(raw.contains("Final-Recipient: rfc822; me@example.org\r\n"));
        assert!(raw.contains("Original-Message-ID: <plan@example.com>\r\n"));
        assert!(raw.contains("Disposition: manual-action/MDN-sent-manually; displayed\r\n"));
        assert!(raw.contains("In-Reply-To: <plan@example.com>"));
        assert!(raw.contains("Subject: Read: Quarterly plan"));
        assert!(
            raw.contains("To: \"Ana\" <ana@example.com>")
                || raw.contains("To: Ana <ana@example.com>")
        );
        assert!(!raw.contains("Auto-Submitted"));
        assert!(!raw.contains("Disposition-Notification-To"));
    }

    #[test]
    fn a_receipt_settings_sent_says_so_and_marks_itself_automatic() {
        let raw = build(false, None);
        assert!(raw.contains("Disposition: manual-action/MDN-sent-automatically; displayed\r\n"));
        assert!(raw.contains("Auto-Submitted: auto-replied"));
        // Without a Message-ID there is none to name.
        assert!(!raw.contains("Original-Message-ID"));
        assert!(!raw.contains("In-Reply-To"));
    }

    #[test]
    fn the_report_part_stays_7bit() {
        let raw = build(true, Some("<plan@example.com>"));
        let report = &raw[raw
            .find("Content-Type: message/disposition-notification")
            .unwrap()..];
        let headers = &report[..report.find("\r\n\r\n").unwrap()];
        assert!(
            headers.contains("Content-Transfer-Encoding: 7bit"),
            "{headers}"
        );
    }
}

#[cfg(test)]
mod returned_receipt_tests {
    use super::{ReadReceipt, build_read_receipt, parse_header_block, parse_message};
    use crate::models::{Address, Disposition};

    fn receipt(report_type: &str, report: &str, extra_headers: &str) -> Vec<u8> {
        format!(
            "Message-ID: <receipt@example.com>\r\n\
             From: Ana Ruiz <ana@example.com>\r\n\
             To: me@example.org\r\n\
             Subject: Read: Plan\r\n\
             Date: Tue, 01 Sep 2026 12:30:00 +0000\r\n\
             {extra_headers}\
             MIME-Version: 1.0\r\n\
             Content-Type: multipart/report; report-type=disposition-notification; boundary=\"b\"\r\n\
             \r\n\
             --b\r\n\
             Content-Type: text/plain\r\n\
             \r\n\
             Your message was displayed.\r\n\
             --b\r\n\
             Content-Type: {report_type}\r\n\
             \r\n\
             {report}\r\n\
             --b--\r\n"
        )
        .into_bytes()
    }

    fn report_of(raw: &[u8]) -> Option<super::DispositionReport> {
        parse_message(raw).unwrap().disposition_report
    }

    #[test]
    fn a_receipt_this_client_sends_reads_back() {
        let recipient = Address {
            name: Some("Me".into()),
            email: "me@example.org".into(),
        };
        let to = [Address {
            name: None,
            email: "ana@example.com".into(),
        }];
        let (_, raw) = build_read_receipt(&ReadReceipt {
            recipient: &recipient,
            to: &to,
            original_subject: "Plan",
            original_message_id: Some("<plan@example.com>"),
            user_agreed: true,
            reporting_domain: "example.org",
        })
        .unwrap();
        let report = report_of(&raw).unwrap();
        assert_eq!(report.original_message_id, "plan@example.com");
        assert_eq!(report.recipient, recipient);
        assert_eq!(report.disposition, Disposition::Displayed);
        assert!(report.reported_at.is_some());
    }

    #[test]
    fn a_report_names_the_message_recipient_and_what_happened() {
        let raw = receipt(
            "message/disposition-notification",
            "Reporting-UA: example.com; Mailer\r\n\
             Original-Recipient: rfc822;team@example.com\r\n\
             Final-Recipient: rfc822; Ana.Ruiz@Example.com\r\n\
             Original-Message-ID: <plan@example.org>\r\n\
             Disposition: automatic-action/MDN-sent-automatically; deleted (by a rule)",
            "",
        );
        let report = report_of(&raw).unwrap();
        assert_eq!(report.original_message_id, "plan@example.org");
        // Final-Recipient, in its own case, and without the From name: the
        // receipt came from another address.
        assert_eq!(
            report.recipient,
            Address {
                name: None,
                email: "Ana.Ruiz@Example.com".into()
            }
        );
        assert_eq!(report.disposition, Disposition::Deleted);
        assert_eq!(report.reported_at, Some(1_788_265_800_000));
    }

    #[test]
    fn missing_fields_fall_back_to_the_receipts_own() {
        // As FairEmail writes them: no Final-Recipient, no
        // Original-Message-ID.
        let raw = receipt(
            "message/disposition-notification; name=\"MDNPart2.txt\"",
            "Reporting-UA: eu.faircode.email; FairEmail\r\n\
             Original-Recipient: rfc822;ana@example.com\r\n\
             Disposition: manual-action/MDN-sent-manually; displayed",
            "In-Reply-To: <plan@example.org>\r\nReferences: <start@example.org> <plan@example.org>\r\n",
        );
        let report = report_of(&raw).unwrap();
        assert_eq!(report.original_message_id, "plan@example.org");
        assert_eq!(
            report.recipient,
            Address {
                name: Some("Ana Ruiz".into()),
                email: "ana@example.com".into()
            }
        );
        // Nothing but the receipt's From to go on.
        let raw = receipt(
            "message/disposition-notification",
            "Original-Message-ID: <plan@example.org>\r\n\
             Disposition: manual-action/MDN-sent-manually; displayed",
            "",
        );
        assert_eq!(report_of(&raw).unwrap().recipient.email, "ana@example.com");
    }

    #[test]
    fn errors_and_refusals_report_nothing() {
        for disposition in [
            "manual-action/MDN-sent-manually; displayed/error",
            "manual-action/MDN-sent-manually; denied",
            "automatic-action/MDN-sent-automatically; failed",
            "manual-action/MDN-sent-manually",
        ] {
            let raw = receipt(
                "message/disposition-notification",
                &format!(
                    "Final-Recipient: rfc822; ana@example.com\r\n\
                     Original-Message-ID: <plan@example.org>\r\n\
                     Disposition: {disposition}"
                ),
                "",
            );
            assert_eq!(report_of(&raw), None, "{disposition}");
        }
        // No message to put it on.
        let raw = receipt(
            "message/disposition-notification",
            "Final-Recipient: rfc822; ana@example.com\r\n\
             Disposition: manual-action/MDN-sent-manually; displayed",
            "",
        );
        assert_eq!(report_of(&raw), None);
    }

    #[test]
    fn addresses_outside_ascii_come_through() {
        let raw = receipt(
            "message/global-disposition-notification",
            "Final-Recipient: utf-8; jörg@exämple.de\r\n\
             Original-Message-ID: <plan@example.org>\r\n\
             Disposition: manual-action/MDN-sent-manually; processed",
            "",
        );
        let report = report_of(&raw).unwrap();
        assert_eq!(report.recipient.email, "jörg@exämple.de");
        assert_eq!(report.disposition, Disposition::Processed);
        let raw = receipt(
            "message/disposition-notification",
            "Final-Recipient: utf-8; j\\x{F6}rg@example.de\r\n\
             Original-Message-ID: <plan@example.org>\r\n\
             Disposition: manual-action/MDN-sent-manually; dispatched",
            "",
        );
        let report = report_of(&raw).unwrap();
        assert_eq!(report.recipient.email, "jörg@example.de");
        assert_eq!(report.disposition, Disposition::Dispatched);
    }

    #[test]
    fn only_a_report_itself_counts() {
        let report = "Final-Recipient: rfc822; ana@example.com\r\n\
                      Original-Message-ID: <plan@example.org>\r\n\
                      Disposition: manual-action/MDN-sent-manually; displayed";
        // The same part in ordinary mail.
        let mixed = String::from_utf8(receipt("message/disposition-notification", report, ""))
            .unwrap()
            .replace(
                "multipart/report; report-type=disposition-notification",
                "multipart/mixed",
            );
        assert_eq!(report_of(mixed.as_bytes()), None);
        // A receipt forwarded inside another message.
        let forwarded = format!(
            "From: Ben <ben@example.com>\r\n\
             To: me@example.org\r\n\
             Subject: Fwd: Read: Plan\r\n\
             MIME-Version: 1.0\r\n\
             Content-Type: multipart/mixed; boundary=\"outer\"\r\n\
             \r\n\
             --outer\r\n\
             Content-Type: text/plain\r\n\
             \r\n\
             See below.\r\n\
             --outer\r\n\
             Content-Type: message/rfc822\r\n\
             \r\n\
             {}\r\n\
             --outer--\r\n",
            String::from_utf8(receipt("message/disposition-notification", report, "")).unwrap()
        );
        assert_eq!(report_of(forwarded.as_bytes()), None);
    }

    #[test]
    fn headers_tell_a_receipt_from_other_reports() {
        let headers = |content_type: &str| {
            parse_header_block(
                format!("From: a@example.com\r\nContent-Type: {content_type}\r\n\r\n").as_bytes(),
            )
            .unwrap()
            .disposition_report
        };
        assert!(headers(
            "multipart/report; report-type=disposition-notification; boundary=b"
        ));
        assert!(headers(
            "Multipart/Report; Report-Type=\"Disposition-Notification\"; boundary=b"
        ));
        assert!(!headers(
            "multipart/report; report-type=delivery-status; boundary=b"
        ));
        assert!(!headers("multipart/mixed; boundary=b"));
    }
}

#[cfg(test)]
mod receipt_request_tests {
    use super::parse_message;

    fn request(headers: &str) -> (Vec<String>, bool) {
        let raw = format!(
            "{headers}From: Ana <ana@example.com>\r\nTo: me@example.org\r\nSubject: hi\r\n\r\nbody"
        );
        let parsed = parse_message(raw.as_bytes()).unwrap().headers;
        (
            parsed.receipt_to.into_iter().map(|a| a.email).collect(),
            parsed.receipt_confirm,
        )
    }

    #[test]
    fn a_request_to_the_return_path_needs_no_confirmation() {
        let (to, confirm) = request(
            "Return-Path: <ana@EXAMPLE.com>\r\nDisposition-Notification-To: Ana <ana@example.com>\r\n",
        );
        assert_eq!(to, ["ana@example.com"]);
        assert!(!confirm, "the domain compares without case");
    }

    #[test]
    fn rfc_8098_section_2_1_cases_need_confirmation() {
        // No Return-Path.
        assert!(request("Disposition-Notification-To: ana@example.com\r\n").1);
        // A null Return-Path is no address.
        assert!(request("Return-Path: <>\r\nDisposition-Notification-To: ana@example.com\r\n").1);
        // Another address than the Return-Path; the local part keeps its case.
        assert!(request("Return-Path: <bounce@example.com>\r\nDisposition-Notification-To: ana@example.com\r\n").1);
        assert!(
            request(
                "Return-Path: <Ana@example.com>\r\nDisposition-Notification-To: ana@example.com\r\n"
            )
            .1
        );
        // Several receipt addresses.
        let (to, confirm) = request(
            "Return-Path: <ana@example.com>\r\nDisposition-Notification-To: ana@example.com, boss@example.com\r\n",
        );
        assert_eq!(to.len(), 2);
        assert!(confirm);
    }

    #[test]
    fn quoting_is_undone_before_comparing() {
        let (_, confirm) = request(
            "Return-Path: <\"ana\"@example.com>\r\nDisposition-Notification-To: ana@example.com\r\n",
        );
        assert!(!confirm);
    }

    #[test]
    fn a_required_option_stops_the_receipt_and_no_header_asks_for_none() {
        let (to, _) = request(
            "Return-Path: <ana@example.com>\r\nDisposition-Notification-To: ana@example.com\r\nDisposition-Notification-Options: signed-receipt=required,pkcs7-signature\r\n",
        );
        assert!(to.is_empty());
        let (to, _) = request(
            "Return-Path: <ana@example.com>\r\nDisposition-Notification-To: ana@example.com\r\nDisposition-Notification-Options: signed-receipt=optional,pkcs7-signature\r\n",
        );
        assert_eq!(to, ["ana@example.com"]);
        assert_eq!(
            request("Return-Path: <ana@example.com>\r\n"),
            (Vec::new(), false)
        );
    }

    #[test]
    fn the_same_address_twice_is_one_receipt_address() {
        let (to, confirm) = request(
            "Return-Path: <ana@example.com>\r\nDisposition-Notification-To: ana@example.com\r\nDisposition-Notification-To: Ana <ana@EXAMPLE.COM>\r\n",
        );
        assert_eq!(to, ["ana@example.com"]);
        assert!(!confirm);
    }
}

#[cfg(test)]
mod via_tests {
    use super::parse_message;

    fn via_of(raw: &str) -> Option<String> {
        parse_message(raw.as_bytes()).unwrap().headers.via
    }

    #[test]
    fn spoofed_from_surfaces_return_path() {
        // Claims to be next.com but was accepted from evil.example: the
        // receiver-stamped Return-Path exposes the real transmitting party.
        let raw = "Return-Path: <bounce@evil.example>\r\nFrom: Next <next@next.com>\r\nTo: a@b.com\r\nSubject: hi\r\n\r\nbody";
        assert_eq!(via_of(raw).as_deref(), Some("bounce@evil.example"));
    }

    #[test]
    fn aligned_subdomain_return_path_is_quiet() {
        let raw = "Return-Path: <bounces@mailer.next.com>\r\nFrom: next@next.com\r\nSubject: hi\r\n\r\nbody";
        assert_eq!(via_of(raw), None);
    }

    #[test]
    fn no_transmit_headers_is_quiet() {
        let raw = "From: next@next.com\r\nSubject: hi\r\n\r\nbody";
        assert_eq!(via_of(raw), None);
    }

    #[test]
    fn misaligned_dkim_domain_shows() {
        let raw = "DKIM-Signature: v=1; a=rsa-sha256; d=esp.example; s=sel; bh=xx\r\nFrom: news@next.com\r\nSubject: hi\r\n\r\nbody";
        assert_eq!(via_of(raw).as_deref(), Some("esp.example"));
    }

    #[test]
    fn sender_header_wins_over_return_path() {
        let raw = "Sender: list@groups.example\r\nReturn-Path: <b+tok@groups.example>\r\nFrom: alice@corp.com\r\nSubject: hi\r\n\r\nbody";
        assert_eq!(via_of(raw).as_deref(), Some("list@groups.example"));
    }
}

#[cfg(test)]
mod automated_tests {
    use super::{parse_message, robot_sender};

    fn automated(raw: &str) -> bool {
        parse_message(raw.as_bytes()).unwrap().headers.is_automated
    }

    #[test]
    fn robot_local_part_is_automated_without_bulk_headers() {
        assert!(automated(
            "From: noreply@binh.ong\r\nSubject: New booking\r\n\r\nbody"
        ));
        assert!(automated(
            "From: notifications@example.com\r\nSubject: You've joined the group\r\n\r\nbody"
        ));
    }

    #[test]
    fn exchange_ndr_is_automated() {
        // Exchange NDR sender local-part, no bulk headers.
        assert!(automated(
            "From: MicrosoftExchange329e71ec88ae4615bbc36ab6ce41109e@corp.com\r\nSubject: Undeliverable: hi\r\n\r\nbody"
        ));
        // DSN envelope content type.
        assert!(automated(
            "From: odd-sender@corp.com\r\nContent-Type: multipart/report; report-type=delivery-status; boundary=b\r\nSubject: Undeliverable: hi\r\n\r\nbody"
        ));
    }

    #[test]
    fn plain_human_mail_is_not_automated() {
        assert!(!automated(
            "From: alice@example.com\r\nSubject: Lunch tomorrow?\r\n\r\nbody"
        ));
        // RFC 3834 defines the literal value "no" as explicitly not
        // automatically submitted. Some human-mail clients include it, so
        // treating mere header presence as automated would suppress a real
        // new-mail notification.
        assert!(!automated(
            "From: alice@example.com\r\nAuto-Submitted: no\r\nSubject: Still human\r\n\r\nbody"
        ));
    }

    #[test]
    fn generated_auto_submitted_mail_is_automated() {
        assert!(automated(
            "From: service@example.com\r\nAuto-Submitted: auto-generated\r\nSubject: Report\r\n\r\nbody"
        ));
    }

    #[test]
    fn robot_prefix_before_plus_only() {
        assert!(robot_sender("alerts+prod@x.com"));
        assert!(!robot_sender("alice+noreply@x.com"));
    }
}

/// Robot senders whose mail is automated even when it carries none of the
/// bulk-mail headers `is_automated` checks (cloud monitoring alarms, CI
/// notifications, and Exchange NDRs are often plain SMTP with a noreply-style
/// local-part).
pub fn robot_sender(email: &str) -> bool {
    let local = email
        .split('@')
        .next()
        .unwrap_or("")
        .split('+')
        .next()
        .unwrap_or("");
    const PREFIXES: &[&str] = &[
        "noreply",
        "no-reply",
        "no_reply",
        "donotreply",
        "do-not-reply",
        "notification",
        "notify",
        "alert",
        "alarm",
        "mailer-daemon",
        "postmaster",
        "bounce",
        "microsoftexchange",
    ];
    PREFIXES.iter().any(|prefix| {
        local
            .as_bytes()
            .get(..prefix.len())
            .is_some_and(|candidate| candidate.eq_ignore_ascii_case(prefix.as_bytes()))
    })
}

/// Raw (unparsed) header value with folding whitespace collapsed, or None when
/// absent/empty. Needed for headers mail-parser types as something other than
/// text (List-Unsubscribe parses as an Address, so as_text() is always None).
fn raw_header(msg: &mail_parser::Message, name: &str) -> Option<String> {
    let raw = msg.header_raw(name)?;
    let unfolded = collapse_whitespace(raw, None);
    (!unfolded.is_empty()).then_some(unfolded)
}

/// The first occurrence of a repeated field in wire order. `mail-parser`'s
/// single-header accessor intentionally returns the last occurrence, which is
/// appropriate for many ordinary fields but unsafe for Authentication-Results:
/// a receiving MTA prepends its trusted result above any older field.
fn first_raw_header(msg: &mail_parser::Message, name: &str) -> Option<String> {
    let raw = msg
        .header_as(name, mail_parser::HeaderForm::Raw)
        .into_iter()
        .next()?
        .into_text()?;
    let unfolded = collapse_whitespace(&raw, None);
    (!unfolded.is_empty()).then_some(unfolded)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AuthenticationResult<'a> {
    method: &'a str,
    result: &'a str,
    properties: Vec<(&'a str, &'a str)>,
}

/// Remove RFC-style comments before tokenizing an Authentication-Results
/// clause. This prevents text such as `(upstream said bimi=pass)` from being
/// mistaken for a receiver-authenticated result.
fn without_comments(value: &str) -> std::borrow::Cow<'_, str> {
    if !value.contains(['(', '\\']) {
        return std::borrow::Cow::Borrowed(value);
    }
    let mut output = String::with_capacity(value.len());
    let mut depth = 0u32;
    let mut escaped = false;
    for ch in value.chars() {
        if escaped {
            if depth == 0 {
                output.push(ch);
            }
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        match ch {
            '(' => depth = depth.saturating_add(1),
            ')' if depth > 0 => depth -= 1,
            _ if depth == 0 => output.push(ch),
            _ => {}
        }
    }
    std::borrow::Cow::Owned(output)
}

/// Tokenize an unfolded Authentication-Results field after removing comments.
/// The caller selects the topmost field and keeps its storage alive so these
/// method/property slices can remain borrowed through classification.
fn authentication_results(results: &str) -> Vec<AuthenticationResult<'_>> {
    results
        .split(';')
        .filter_map(|clause| {
            let mut tokens = clause.split_ascii_whitespace();
            let (method, result) = tokens.next()?.split_once('=')?;
            if method.contains('.') {
                // Property names (header.d, policy.authority, ...) cannot
                // begin a result clause.
                return None;
            }
            if !["bimi", "dmarc", "dkim", "compauth"]
                .iter()
                .any(|known| method.eq_ignore_ascii_case(known))
            {
                return None;
            }
            let properties = tokens
                .filter_map(|token| token.split_once('='))
                .filter(|(name, _)| {
                    [
                        "policy.authority",
                        "header.from",
                        "header.d",
                        "header.i",
                        "header.b",
                    ]
                    .iter()
                    .any(|known| name.trim().eq_ignore_ascii_case(known))
                })
                .map(|(name, value)| {
                    (
                        name.trim(),
                        value
                            .trim_matches(|ch: char| ch == '"' || ch == '\'')
                            .trim_end_matches(','),
                    )
                })
                .collect();
            Some(AuthenticationResult {
                method,
                result: result.trim_end_matches(','),
                properties,
            })
        })
        .collect()
}

fn auth_property<'a>(result: &AuthenticationResult<'a>, name: &str) -> Option<&'a str> {
    result
        .properties
        .iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
        .map(|(_, value)| *value)
}

/// Classify only strong, receiver-reported authentication. SPF alone never
/// qualifies: it authenticates the envelope sender and survives neither From
/// spoofing nor many forwarding paths. Standard domain authentication needs
/// both DMARC and a DKIM signature aligned with the visible From domain.
fn sender_verification(
    results: &[AuthenticationResult<'_>],
    from: Option<&Address>,
) -> SenderVerification {
    if let Some(bimi) = results.iter().find(|result| {
        result.method.eq_ignore_ascii_case("bimi") && result.result.eq_ignore_ascii_case("pass")
    }) {
        return if auth_property(bimi, "policy.authority")
            .is_some_and(|value| value.eq_ignore_ascii_case("pass"))
        {
            SenderVerification::Brand
        } else {
            SenderVerification::Bimi
        };
    }

    let Some(from_domain) = from
        .map(|address| domain_of(&address.email))
        .filter(|domain| !domain.is_empty())
    else {
        return SenderVerification::None;
    };

    let dmarc_pass = results.iter().any(|result| {
        result.method.eq_ignore_ascii_case("dmarc")
            && result.result.eq_ignore_ascii_case("pass")
            && auth_property(result, "header.from")
                .is_some_and(|domain| domains_aligned(domain, from_domain))
    });
    let aligned_dkim_pass = results.iter().any(|result| {
        result.method.eq_ignore_ascii_case("dkim")
            && result.result.eq_ignore_ascii_case("pass")
            && auth_property(result, "header.d")
                .is_some_and(|domain| domains_aligned(domain, from_domain))
    });
    if dmarc_pass && aligned_dkim_pass {
        return SenderVerification::Domain;
    }

    if results.iter().any(|result| {
        result.method.eq_ignore_ascii_case("compauth") && result.result.eq_ignore_ascii_case("pass")
    }) {
        return SenderVerification::Microsoft;
    }

    SenderVerification::None
}

/// Split a List-Unsubscribe header into its URIs. RFC 2369 wraps each URI in
/// angle brackets; tolerate bare comma-separated values from sloppy senders.
pub fn parse_unsubscribe_uris(raw: &str) -> Vec<String> {
    unsubscribe_uris(raw).map(str::to_owned).collect()
}

/// Borrow URI text while choosing a mechanism; only the selected plan needs
/// owned strings. Any nonempty bracketed URI suppresses the bare fallback.
pub(crate) fn unsubscribe_uris(raw: &str) -> impl Iterator<Item = &str> + Clone {
    let mut bracketed = raw
        .match_indices('<')
        .filter_map(|(start, _)| {
            let rest = &raw[start + 1..];
            let end = rest.find('>')?;
            let uri = rest[..end].trim();
            (!uri.is_empty()).then_some(uri)
        })
        .peekable();
    let has_bracketed = bracketed.peek().is_some();
    let mut bare = raw.split(',').map(str::trim).filter(|s| !s.is_empty());
    std::iter::from_fn(move || {
        if has_bracketed {
            bracketed.next()
        } else {
            bare.next()
        }
    })
}

/// RFC 8058 §3.1: the header value must be exactly "List-Unsubscribe=One-Click"
/// (compared case-insensitively; some senders add stray whitespace).
pub fn is_one_click_post(raw: &str) -> bool {
    raw.trim()
        .eq_ignore_ascii_case("List-Unsubscribe=One-Click")
}

/// Whether a message's header block asks for an RFC 8058 one-click
/// unsubscribe that its sender signed. Section 4 requires a valid DKIM
/// signature whose h= tag covers both List-Unsubscribe and
/// List-Unsubscribe-Post; without one, receivers should not offer one-click.
///
/// Signatures are not verified here. As with the verified-sender badge, the
/// result the receiving server put in the topmost Authentication-Results
/// field (RFC 8601) decides, and its header.b (RFC 6008) tells which
/// signature passed. When a server reports only the signing domain, every
/// signature of that domain has to cover both fields. Each field has to
/// appear exactly once: RFC 2369 allows no more, and a signature that covers
/// one instance says nothing about another (RFC 6376 section 8.15).
pub fn one_click_signed(raw: &[u8]) -> bool {
    let Some(msg) = mail_parser::MessageParser::default().parse_headers(raw) else {
        return false;
    };
    let fields = |name: &'static str| msg.header_as(name, mail_parser::HeaderForm::Raw);
    let post = fields("List-Unsubscribe-Post");
    if fields("List-Unsubscribe").len() != 1
        || post.len() != 1
        || !post[0].as_text().is_some_and(is_one_click_post)
    {
        return false;
    }
    let signatures: Vec<DkimSignature> = fields("DKIM-Signature")
        .iter()
        .filter_map(|value| value.as_text().and_then(DkimSignature::parse))
        .collect();
    let authentication = first_raw_header(&msg, "Authentication-Results").unwrap_or_default();
    let authentication = without_comments(&authentication);
    authentication_results(&authentication)
        .iter()
        .filter(|result| {
            result.method.eq_ignore_ascii_case("dkim") && result.result.eq_ignore_ascii_case("pass")
        })
        .any(|result| {
            let mut reported = signatures
                .iter()
                .filter(|signature| signature.reported_by(result))
                .peekable();
            reported.peek().is_some() && reported.all(DkimSignature::covers_one_click)
        })
}

/// The DKIM-Signature tags that say who signed which fields (RFC 6376
/// section 3.5).
struct DkimSignature {
    domain: String,
    /// Signed field names, lowercase.
    signed: Vec<String>,
    /// The signature, without folding whitespace.
    signature: String,
}

impl DkimSignature {
    fn parse(value: &str) -> Option<Self> {
        let (mut domain, mut signed, mut signature) = (None, None, None);
        for tag in value.split(';') {
            let Some((name, value)) = tag.split_once('=') else {
                continue;
            };
            let compact = || -> String { value.split_ascii_whitespace().collect() };
            // Tag names are case-sensitive.
            match name.trim() {
                "d" => domain = Some(compact()),
                "h" => {
                    signed = Some(
                        compact()
                            .split(':')
                            .map(str::to_ascii_lowercase)
                            .collect::<Vec<_>>(),
                    )
                }
                "b" => signature = Some(compact()),
                _ => {}
            }
        }
        Some(Self {
            domain: domain.filter(|domain| !domain.is_empty())?,
            signed: signed?,
            signature: signature.filter(|signature| !signature.is_empty())?,
        })
    }

    /// Whether the receiver's result is about this signature: by the start of
    /// its b= value (matched case-sensitively, RFC 6008), else by the domain
    /// it reports. An i= identity names the signing domain or a subdomain.
    fn reported_by(&self, result: &AuthenticationResult<'_>) -> bool {
        if let Some(signature) = auth_property(result, "header.b") {
            return !signature.is_empty() && self.signature.starts_with(signature);
        }
        if let Some(domain) = auth_property(result, "header.d") {
            return self
                .domain
                .trim_end_matches('.')
                .eq_ignore_ascii_case(domain.trim_end_matches('.'));
        }
        auth_property(result, "header.i").is_some_and(|identity| {
            let identity = domain_of(identity).trim_end_matches('.');
            let domain = self.domain.trim_end_matches('.');
            identity.eq_ignore_ascii_case(domain)
                || identity
                    .len()
                    .checked_sub(domain.len() + 1)
                    .is_some_and(|dot| {
                        identity.as_bytes()[dot] == b'.'
                            && identity[dot + 1..].eq_ignore_ascii_case(domain)
                    })
        })
    }

    fn covers_one_click(&self) -> bool {
        ["list-unsubscribe", "list-unsubscribe-post"]
            .iter()
            .all(|name| self.signed.iter().any(|signed| signed == name))
    }
}

fn parse_headers(msg: &mail_parser::Message) -> ParsedHeaders {
    let mut refs: Vec<String> = Vec::new();
    if let Some(irt) = msg.in_reply_to().as_text_list() {
        refs.extend(irt.iter().map(|s| s.to_string()));
    }
    if let Some(r) = msg.references().as_text_list() {
        for s in r {
            if !refs.iter().any(|reference| reference == s.as_ref()) {
                refs.push(s.to_string());
            }
        }
    }

    let from = msg.from().and_then(|f| f.first()).and_then(addr_from);
    let via = resolve_via(msg, from.as_ref());
    // Receiving MTAs prepend their result; older fields may be untrusted.
    // Remove comments before splitting clauses because they may contain
    // semicolons and text resembling method=result authentication claims.
    let authentication_header = first_raw_header(msg, "Authentication-Results").unwrap_or_default();
    let authentication_header = without_comments(&authentication_header);
    let authentication_results = authentication_results(&authentication_header);
    let sender_verification = sender_verification(&authentication_results, from.as_ref());
    let (receipt_to, receipt_confirm) = receipt_request(msg);

    // multipart/report is the DSN/MDN envelope: bounces and read receipts.
    let is_report = msg
        .content_type()
        .map(|ct| {
            ct.ctype().eq_ignore_ascii_case("multipart")
                && ct
                    .subtype()
                    .is_some_and(|s| s.eq_ignore_ascii_case("report"))
        })
        .unwrap_or(false);

    let disposition_report = is_report
        && msg
            .content_type()
            .and_then(|ct| ct.attributes())
            .into_iter()
            .flatten()
            .any(|attribute| {
                attribute.name.eq_ignore_ascii_case("report-type")
                    && attribute
                        .value
                        .eq_ignore_ascii_case("disposition-notification")
            });

    let is_automated = msg.header("List-Id").is_some()
        || msg.header("List-Unsubscribe").is_some()
        || msg
            .header("Precedence")
            .and_then(|h| h.as_text())
            .map(|v| {
                ["bulk", "list", "junk"]
                    .iter()
                    .any(|kind| v.eq_ignore_ascii_case(kind))
            })
            .unwrap_or(false)
        || msg
            .header("Auto-Submitted")
            .and_then(|header| header.as_text())
            .is_some_and(|value| !value.trim().eq_ignore_ascii_case("no"))
        || msg.header("X-Autoreply").is_some()
        || msg.header("X-Failed-Recipients").is_some()
        || is_report
        || from.as_ref().is_some_and(|a| robot_sender(&a.email));

    ParsedHeaders {
        message_id: msg.message_id().map(|s| s.to_string()),
        submission_id: raw_header(msg, SUBMISSION_ID_HEADER)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty() && !value.chars().any(char::is_whitespace)),
        subject: msg.subject().unwrap_or_default().to_string(),
        from,
        to: collect_addrs(msg.to()),
        cc: collect_addrs(msg.cc()),
        bcc: collect_addrs(msg.bcc()),
        date_ms: msg.date().map(|d| d.to_timestamp() * 1000),
        references: refs,
        is_automated,
        // mail-parser types List-Unsubscribe as an Address header, so as_text()
        // is always None; read the raw value and unfold it ourselves.
        list_unsubscribe: raw_header(msg, "List-Unsubscribe"),
        list_unsubscribe_post: raw_header(msg, "List-Unsubscribe-Post"),
        list_id: raw_header(msg, "List-Id").and_then(|raw| list_id(&raw)),
        via,
        sender_verification,
        receipt_to,
        receipt_confirm,
        disposition_report,
    }
}

/// A read receipt request (RFC 8098 section 2): where the receipt goes, and
/// whether section 2.1 wants the user's confirmation first.
fn receipt_request(msg: &mail_parser::Message) -> (Vec<Address>, bool) {
    // RFC 8098 defines no options. One a receiver must understand to answer
    // stops the receipt, as RFC 3798 answered such a request with "failed".
    if raw_header(msg, "Disposition-Notification-Options")
        .is_some_and(|options| requires_unknown_option(&options))
    {
        return (Vec::new(), false);
    }
    let mut to = Vec::<Address>::new();
    for value in msg.header_as(
        "Disposition-Notification-To",
        mail_parser::HeaderForm::Addresses,
    ) {
        if let mail_parser::HeaderValue::Address(addresses) = value {
            for address in collect_addrs(Some(&addresses)) {
                if !to
                    .iter()
                    .any(|seen| same_receipt_address(&seen.email, &address.email))
                {
                    to.push(address);
                }
            }
        }
    }
    let Some(first) = to.first() else {
        return (to, false);
    };
    let return_paths = match msg.return_path() {
        mail_parser::HeaderValue::Text(path) => vec![path.as_ref()],
        mail_parser::HeaderValue::TextList(paths) => paths.iter().map(AsRef::as_ref).collect(),
        _ => Vec::new(),
    }
    .into_iter()
    .map(|path| {
        path.trim()
            .trim_start_matches('<')
            .trim_end_matches('>')
            .trim()
    })
    .filter(|path| !path.is_empty())
    .collect::<Vec<_>>();
    // Several Return-Path fields that disagree count as a mismatch.
    let confirm = to.len() > 1
        || return_paths.is_empty()
        || return_paths
            .iter()
            .any(|path| !same_receipt_address(path, &first.email));
    (to, confirm)
}

/// Whether a Disposition-Notification-Options value has an attribute marked
/// `required`; RFC 8098 defines none, so any is one this client doesn't know.
fn requires_unknown_option(options: &str) -> bool {
    options.split(';').any(|option| {
        option
            .split_once('=')
            .and_then(|(_, value)| value.split(',').next())
            .is_some_and(|importance| importance.trim().eq_ignore_ascii_case("required"))
    })
}

/// RFC 8098 section 2.1 compares only the address: the local part exactly,
/// once quoting is undone, and the domain without regard to case.
fn same_receipt_address(left: &str, right: &str) -> bool {
    fn parts(address: &str) -> Option<(String, String)> {
        let (local, domain) = address.trim().rsplit_once('@')?;
        let mut unquoted = String::with_capacity(local.len());
        let mut chars = local.chars();
        while let Some(c) = chars.next() {
            match c {
                '"' => {}
                '\\' => unquoted.extend(chars.next()),
                c => unquoted.push(c),
            }
        }
        Some((unquoted, domain.to_ascii_lowercase()))
    }
    match (parts(left), parts(right)) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

/// A read receipt that came back (RFC 8098 section 3): the message it's
/// about, whom it reports for, and what happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispositionReport {
    /// The Message-ID of the message it's about, without angle brackets, as
    /// messages keep theirs.
    pub original_message_id: String,
    /// The recipient it reports for. The name is the receipt's sender's,
    /// when the receipt came from that address.
    pub recipient: Address,
    pub disposition: crate::models::Disposition,
    /// When it was written: the receipt's Date field.
    pub reported_at: Option<i64>,
}

/// The MIME types of a report part: RFC 8098's, and RFC 6533's for
/// addresses that aren't ASCII.
pub fn is_disposition_report_part(mime_type: &str) -> bool {
    [
        "message/disposition-notification",
        "message/global-disposition-notification",
    ]
    .iter()
    .any(|report| mime_type.eq_ignore_ascii_case(report))
}

/// Read a report part. The receipt's own fields stand in for what some
/// reporters leave out: In-Reply-To for Original-Message-ID, and From for
/// both recipient fields. A report of an error, or one that says nothing
/// happened ("denied", "failed"), is no receipt.
pub fn disposition_report(
    report: &[u8],
    in_reply_to: Option<&str>,
    from: Option<&Address>,
    date_ms: Option<i64>,
) -> Option<DispositionReport> {
    let fields = mail_parser::MessageParser::default().parse_headers(report)?;
    let disposition = report_disposition(&raw_header(&fields, "Disposition")?)?;
    let original_message_id = raw_header(&fields, "Original-Message-ID")
        .and_then(|value| bare_message_id(&value))
        .or_else(|| in_reply_to.and_then(bare_message_id))?;
    let email = ["Final-Recipient", "Original-Recipient"]
        .iter()
        .find_map(|field| raw_header(&fields, field).and_then(|value| report_address(&value)))
        .or_else(|| from.map(|from| from.email.clone()))?;
    let name = from
        .filter(|from| from.email.eq_ignore_ascii_case(&email))
        .and_then(|from| from.name.clone())
        .filter(|name| !name.trim().is_empty());
    Some(DispositionReport {
        original_message_id,
        recipient: Address { name, email },
        disposition,
        reported_at: date_ms,
    })
}

/// The disposition type of a Disposition field (RFC 8098 section 3.2.6):
/// `manual-action/MDN-sent-manually; displayed`, maybe with modifiers after
/// a slash, of which "error" undoes it.
fn report_disposition(value: &str) -> Option<crate::models::Disposition> {
    let value = without_comments(value);
    let (_, outcome) = value.split_once(';')?;
    let (kind, modifiers) = outcome.split_once('/').unwrap_or((outcome, ""));
    if modifiers
        .split(',')
        .any(|modifier| modifier.trim().eq_ignore_ascii_case("error"))
    {
        return None;
    }
    crate::models::Disposition::parse(kind.trim())
}

/// The address in a Final-Recipient or Original-Recipient field, `rfc822;
/// ana@example.com`. RFC 6533's utf-8 type writes characters outside ASCII
/// as `\x{HEX}` in a plain report part.
fn report_address(value: &str) -> Option<String> {
    let (kind, address) = value.split_once(';')?;
    let kind = kind.trim();
    if !kind.eq_ignore_ascii_case("rfc822") && !kind.eq_ignore_ascii_case("utf-8") {
        return None;
    }
    let address = address.trim();
    let address = match (address.rfind('<'), address.rfind('>')) {
        (Some(start), Some(end)) if start < end => &address[start + 1..end],
        _ => address,
    };
    let address = unescape_utf8_xtext(address.trim());
    (address.contains('@') && !address.contains(char::is_whitespace)).then_some(address)
}

/// Undo RFC 6533's `\x{HEX}` escapes; anything else stays as it is.
fn unescape_utf8_xtext(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find("\\x{") {
        output.push_str(&rest[..start]);
        let escaped = &rest[start + 3..];
        let decoded = escaped.split_once('}').and_then(|(hex, after)| {
            u32::from_str_radix(hex, 16)
                .ok()
                .and_then(char::from_u32)
                .map(|ch| (ch, after))
        });
        match decoded {
            Some((ch, after)) => {
                output.push(ch);
                rest = after;
            }
            None => {
                output.push_str("\\x{");
                rest = escaped;
            }
        }
    }
    output.push_str(rest);
    output
}

/// A msg-id without its angle brackets: the first `<...>` in the value, or
/// the whole value when it has none.
fn bare_message_id(value: &str) -> Option<String> {
    let value = value.trim();
    let id = match value.find('<') {
        Some(start) => {
            let rest = &value[start + 1..];
            &rest[..rest.find('>')?]
        }
        None => value,
    };
    let id = id.trim();
    (!id.is_empty() && !id.contains(char::is_whitespace)).then(|| id.to_string())
}

/// The identifier in a List-Id value: `Weekly news <news.example.com>` names
/// the list `news.example.com`. Older lists sometimes send the bare
/// identifier. RFC 2919 limits it to 255 characters.
pub fn list_id(raw: &str) -> Option<String> {
    let id = match (raw.rfind('<'), raw.rfind('>')) {
        (Some(start), Some(end)) if start < end => &raw[start + 1..end],
        _ => raw,
    };
    let id = id.trim();
    (!id.is_empty() && id.len() <= 255 && !id.contains(char::is_whitespace))
        .then(|| id.to_ascii_lowercase())
}

/// Parse a full raw RFC 5322 message.
pub fn parse_message(raw: &[u8]) -> Result<ParsedBody> {
    let msg = mail_parser::MessageParser::default()
        .parse(raw)
        .ok_or_else(|| CoreError::Mime("unparseable message".into()))?;

    let headers = parse_headers(&msg);

    let text = msg.body_text(0).map(|t| t.into_owned());
    let html = msg.body_html(0).map(|h| sanitize_html(&h));

    let snippet = make_body_snippet(text.as_deref(), html.as_deref());

    let mut attachments = Vec::new();
    let mut calendar_parts = Vec::new();
    for (i, part) in msg.attachments().enumerate() {
        let filename = part.attachment_name().map(|s| s.to_string());
        let mime_type = part.content_type().map(|ct| match ct.subtype() {
            Some(sub) => format!("{}/{}", ct.ctype(), sub),
            None => ct.ctype().to_string(),
        });
        if mime_type
            .as_deref()
            .is_some_and(|m| m.eq_ignore_ascii_case("text/calendar"))
        {
            calendar_parts.push(String::from_utf8_lossy(part.contents()).into_owned());
        }
        let content_id = part.content_id().map(|s| s.to_string());
        // Mirrors `plan_bodystructure`: Content-Disposition decides, and a bare
        // Content-ID only counts when no disposition was declared.
        let is_inline = match part.content_disposition() {
            Some(cd) if cd.is_attachment() => false,
            Some(cd) if cd.is_inline() => true,
            _ => content_id.is_some(),
        };
        attachments.push(ParsedAttachment {
            part_id: i.to_string(),
            filename,
            mime_type,
            size: part.contents().len() as i64,
            is_inline,
            content_id,
        });
    }
    let disposition_report = returned_receipt(&msg, &headers);
    // Invites are often an *alternative* body part rather than an attachment.
    for part in msg.text_bodies() {
        let is_cal = part.content_type().is_some_and(|ct| {
            ct.subtype()
                .is_some_and(|s| s.eq_ignore_ascii_case("calendar"))
        });
        if is_cal {
            calendar_parts.push(String::from_utf8_lossy(part.contents()).into_owned());
        }
    }

    Ok(ParsedBody {
        headers,
        text,
        html,
        snippet,
        attachments,
        calendar_parts,
        disposition_report,
    })
}

/// The read receipt a message is, when it's a multipart/report (RFC 6522)
/// carrying a disposition report. One forwarded inside another message
/// isn't the message itself, and isn't one.
fn returned_receipt(
    msg: &mail_parser::Message,
    headers: &ParsedHeaders,
) -> Option<DispositionReport> {
    let content_type = msg.content_type()?;
    if !content_type.ctype().eq_ignore_ascii_case("multipart")
        || !content_type
            .subtype()
            .is_some_and(|subtype| subtype.eq_ignore_ascii_case("report"))
    {
        return None;
    }
    let report = msg.attachments().find(|part| {
        part.content_type().is_some_and(|ct| {
            ct.ctype().eq_ignore_ascii_case("message")
                && ct.subtype().is_some_and(|subtype| {
                    is_disposition_report_part(&format!("message/{subtype}"))
                })
        })
    })?;
    let in_reply_to = msg
        .in_reply_to()
        .as_text_list()
        .and_then(|ids| ids.first().map(|id| id.to_string()));
    disposition_report(
        report.contents(),
        in_reply_to.as_deref(),
        headers.from.as_ref(),
        headers.date_ms,
    )
}

/// Parse only headers (from a HEADER.FIELDS fetch).
pub fn parse_header_block(raw: &[u8]) -> Result<ParsedHeaders> {
    let msg = mail_parser::MessageParser::default()
        .parse_headers(raw)
        .ok_or_else(|| CoreError::Mime("unparseable headers".into()))?;
    Ok(parse_headers(&msg))
}

/// Extract one attachment's bytes from a raw message by attachment index.
pub fn extract_attachment(raw: &[u8], part_id: &str) -> Result<(Vec<u8>, Option<String>)> {
    let msg = mail_parser::MessageParser::default()
        .parse(raw)
        .ok_or_else(|| CoreError::Mime("unparseable message".into()))?;
    let idx: usize = part_id
        .parse()
        .map_err(|_| CoreError::Mime("bad part id".into()))?;
    let part = msg
        .attachments()
        .nth(idx)
        .ok_or_else(|| CoreError::NotFound("attachment".into()))?;
    Ok((
        part.contents().to_vec(),
        part.attachment_name().map(|s| s.to_string()),
    ))
}

pub struct OutgoingAttachment {
    pub filename: String,
    pub mime_type: String,
    pub bytes: Vec<u8>,
}

pub struct OutgoingMessage<'a> {
    pub from: Address,
    /// Reply-To from the sender identity, when it differs from `from`.
    pub reply_to: Option<Address>,
    pub to: &'a [Address],
    pub cc: &'a [Address],
    pub bcc: &'a [Address],
    pub subject: &'a str,
    pub body_text: &'a str,
    /// Rich body. Embedded `data:` images are converted to inline CID parts.
    pub body_html: Option<&'a str>,
    /// Message-ID of the message being replied to.
    pub in_reply_to: Option<&'a str>,
    /// Full reference chain (oldest first), including in_reply_to last.
    pub references: &'a [String],
    /// Stable RFC 5322 Message-ID for retries/draft revisions. When absent a
    /// fresh id is generated and returned by [`build_message`].
    pub message_id: Option<&'a str>,
    pub message_id_domain: &'a str,
    pub attachments: Vec<OutgoingAttachment>,
}

/// An image extracted from the HTML body, to be sent as an inline CID part.
struct InlineImage {
    mime_type: String,
    cid: String,
    filename: String,
    bytes: Vec<u8>,
}

/// The media type of an embedded image, lowercased, when it's a plain
/// `image/<subtype>` that can go in a part's header as it is.
fn image_media_type(value: &str) -> Option<String> {
    let value = value.split(';').next()?.trim().to_ascii_lowercase();
    let subtype = value.strip_prefix("image/")?;
    let valid = (1..=64).contains(&subtype.len())
        && subtype
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'+' | b'-'));
    valid.then_some(value)
}

/// A file name for the `index`th image of a message, for mail programs that
/// list inline images or save them.
fn inline_image_filename(index: usize, mime_type: &str) -> String {
    let subtype = mime_type.strip_prefix("image/").unwrap_or_default();
    let extension = match subtype {
        "jpeg" | "pjpeg" => "jpg",
        "svg+xml" => "svg",
        "x-icon" | "vnd.microsoft.icon" => "ico",
        other if !other.is_empty() && other.bytes().all(|byte| byte.is_ascii_alphanumeric()) => {
            other
        }
        _ => "img",
    };
    format!("image{index}.{extension}")
}

/// How much of an HTML body its embedded `data:` images take.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct EmbeddedImages {
    /// The bytes of the `data:` addresses in the HTML.
    pub(crate) source_bytes: usize,
    /// The size of the images they hold, once decoded.
    pub(crate) image_bytes: usize,
}

/// Measures the `src="data:image/...;base64,..."` images of `html` without
/// decoding them.
pub(crate) fn embedded_images(html: &str) -> EmbeddedImages {
    const MARKER: &str = "src=\"data:image/";
    let mut found = EmbeddedImages::default();
    let mut rest = html;
    while let Some(pos) = rest.find(MARKER) {
        let after = &rest[pos + 5..];
        let Some(end) = after.find('"') else {
            break;
        };
        let uri = &after[..end];
        if let Some((_, data)) = uri.split_once(";base64,") {
            found.source_bytes = found.source_bytes.saturating_add(uri.len());
            found.image_bytes = found.image_bytes.saturating_add(data.trim().len() / 4 * 3);
        }
        rest = &after[end + 1..];
    }
    found
}

/// Replace `src="data:image/...;base64,..."` with `src="cid:..."` and return
/// the decoded images. Editors embed pasted screenshots as data URIs, but
/// many mail clients (Gmail included) strip those - CID parts survive. Each
/// Content-ID is unique and ends in `domain`, as RFC 2392 asks.
fn extract_data_uri_images<'h>(
    html: &'h str,
    domain: &str,
) -> (std::borrow::Cow<'h, str>, Vec<InlineImage>) {
    use base64::Engine;
    const MARKER: &str = "src=\"data:image/";

    if !html.contains(MARKER) {
        return (std::borrow::Cow::Borrowed(html), Vec::new());
    }
    let mut out: Option<String> = None;
    let mut images = Vec::new();
    let mut rest = html;
    let mut copied = 0;
    while let Some(pos) = rest.find(MARKER) {
        let source_start = html.len() - rest.len() + pos;
        let after = &rest[pos + 5..]; // past `src="`, starts at `data:image/`
        let Some(endq) = after.find('"') else {
            break;
        };
        let uri = &after[..endq];
        let decoded = uri
            .strip_prefix("data:")
            .and_then(|u| u.split_once(";base64,"))
            .and_then(|(mime, b64)| {
                let mime_type = image_media_type(mime)?;
                base64::engine::general_purpose::STANDARD
                    .decode(b64.trim())
                    .ok()
                    .map(|bytes| (mime_type, bytes))
            });
        if let Some((mime_type, bytes)) = decoded {
            let index = images.len() + 1;
            let cid = format!("img{index}.{}@{domain}", rand_token());
            let output = out.get_or_insert_with(String::new);
            output.push_str(&html[copied..source_start]);
            output.push_str("src=\"cid:");
            output.push_str(&cid);
            output.push('"');
            copied = source_start + 5 + endq + 1;
            images.push(InlineImage {
                filename: inline_image_filename(index, &mime_type),
                mime_type,
                cid,
                bytes,
            });
        }
        rest = &after[endq + 1..];
    }
    match out {
        Some(mut output) => {
            output.push_str(&html[copied..]);
            (std::borrow::Cow::Owned(output), images)
        }
        None => (std::borrow::Cow::Borrowed(html), images),
    }
}

/// The HTML part of a message with the images it shows: on their own, the
/// HTML, and with images a multipart/related holding the HTML first and then
/// each image as an inline part (RFC 2387), as mail programs expect.
fn html_with_images<'a>(
    html: std::borrow::Cow<'a, str>,
    images: Vec<InlineImage>,
) -> mail_builder::mime::MimePart<'a> {
    use mail_builder::headers::content_type::ContentType;
    use mail_builder::mime::{BodyPart, MimePart};

    let html = MimePart::new("text/html", BodyPart::Text(html));
    if images.is_empty() {
        return html;
    }
    let mut parts = Vec::with_capacity(images.len() + 1);
    parts.push(html);
    for image in images {
        parts.push(
            MimePart::new(
                ContentType::new(image.mime_type).attribute("name", image.filename.clone()),
                BodyPart::Binary(std::borrow::Cow::Owned(image.bytes)),
            )
            .header(
                "Content-Disposition",
                ContentType::new("inline").attribute("filename", image.filename),
            )
            .cid(image.cid),
        );
    }
    MimePart::new(
        ContentType::new("multipart/related").attribute("type", "text/html"),
        parts,
    )
}

struct MessageAttachment<'a> {
    filename: std::borrow::Cow<'a, str>,
    mime_type: std::borrow::Cow<'a, str>,
    bytes: std::borrow::Cow<'a, [u8]>,
}

/// The METHOD of an attachment marked as an iMIP calendar part (RFC 6047
/// §2.4): `text/calendar` with a `method` parameter. A calendar file the
/// user attached has none, and goes out as a plain attachment.
fn imip_method(mime_type: &str) -> Option<&str> {
    let mut parts = mime_type.split(';');
    if !parts.next()?.trim().eq_ignore_ascii_case("text/calendar") {
        return None;
    }
    parts
        .find_map(|parameter| {
            let (name, value) = parameter.split_once('=')?;
            name.trim()
                .eq_ignore_ascii_case("method")
                .then(|| value.trim().trim_matches('"'))
        })
        .filter(|method| !method.is_empty())
}

/// The body of an iMIP message (RFC 6047 §2.4 and §4.2): the text, any HTML
/// and the calendar as alternatives of one another, so mail programs that
/// don't read calendars show the text. The calendar is attached again as
/// invite.ics for programs that only look at attachments, followed by any
/// other attachments.
fn imip_body<'a>(
    out: &'a OutgoingMessage<'_>,
    calendar: MessageAttachment<'a>,
    attachments: Vec<MessageAttachment<'a>>,
) -> mail_builder::mime::MimePart<'a> {
    use mail_builder::headers::content_type::ContentType;
    use mail_builder::mime::{BodyPart, MimePart};

    let method = imip_method(&calendar.mime_type)
        .unwrap_or_default()
        .to_ascii_uppercase();
    let mut alternatives = vec![MimePart::new("text/plain", out.body_text)];
    if let Some(html) = out.body_html.filter(|html| !html.trim().is_empty()) {
        alternatives.push(MimePart::new("text/html", html));
    }
    alternatives.push(MimePart::new(
        ContentType::new("text/calendar")
            .attribute("method", method)
            .attribute("charset", "UTF-8"),
        String::from_utf8_lossy(&calendar.bytes).into_owned(),
    ));
    let mut parts = vec![MimePart::new("multipart/alternative", alternatives)];
    parts.push(
        MimePart::new(
            ContentType::new("application/ics").attribute("name", calendar.filename.clone()),
            BodyPart::Binary(calendar.bytes),
        )
        .attachment(calendar.filename),
    );
    parts.extend(attachments.into_iter().map(attachment_part));
    MimePart::new("multipart/mixed", parts)
}

/// An attachment's part, as mail-builder writes those it adds itself.
fn attachment_part(attachment: MessageAttachment<'_>) -> mail_builder::mime::MimePart<'_> {
    use mail_builder::headers::content_type::ContentType;
    use mail_builder::mime::{BodyPart, MimePart};

    MimePart::new(
        ContentType::new(attachment.mime_type),
        BodyPart::Binary(attachment.bytes),
    )
    .attachment(attachment.filename)
}

/// Build a raw RFC 5322 message. Returns (message_id, raw_bytes).
/// What a read receipt reports (RFC 8098 section 3).
pub struct ReadReceipt<'a> {
    /// The account the message reached: the receipt's From and its
    /// Final-Recipient.
    pub recipient: &'a Address,
    /// The Disposition-Notification-To addresses.
    pub to: &'a [Address],
    pub original_subject: &'a str,
    pub original_message_id: Option<&'a str>,
    /// The user agreed to this receipt, rather than Settings sending it on
    /// its own.
    pub user_agreed: bool,
    /// Reporting-UA's name part: the account's mail domain, which says no
    /// more about the device than the From address does.
    pub reporting_domain: &'a str,
}

/// A read receipt: multipart/report with a sentence for people and the
/// message/disposition-notification part for programs. The message was
/// "displayed", which RFC 8098 is careful to say doesn't mean read or
/// understood. Returns its Message-ID and bytes.
pub fn build_read_receipt(receipt: &ReadReceipt) -> Result<(String, Vec<u8>)> {
    use mail_builder::headers::content_type::ContentType;
    use mail_builder::mime::{BodyPart, MimePart};
    let msg_id = format!(
        "{}.{}@{}",
        chrono::Utc::now().timestamp_micros(),
        rand_token(),
        receipt.reporting_domain
    );
    let original_id = receipt
        .original_message_id
        .map(|id| id.trim().trim_matches(['<', '>']))
        .filter(|id| !id.is_empty() && !id.contains(['\r', '\n']));
    // Section 3: "manual-action" even when the user set the client to send
    // on its own; MDN-sent-manually only for one they agreed to.
    let sending_mode = if receipt.user_agreed {
        "MDN-sent-manually"
    } else {
        "MDN-sent-automatically"
    };
    // The report part must be 7bit; RFC 6533 carries an address that isn't
    // ASCII in message/global-disposition-notification instead.
    let ascii = receipt.recipient.email.is_ascii()
        && original_id.is_none_or(|id| id.is_ascii())
        && receipt.reporting_domain.is_ascii();
    let mut report = format!(
        "Reporting-UA: {}; Flectar Mail\r\nFinal-Recipient: {}; {}\r\n",
        receipt.reporting_domain,
        if ascii { "rfc822" } else { "utf-8" },
        receipt.recipient.email,
    );
    if let Some(id) = original_id {
        report.push_str(&format!("Original-Message-ID: <{id}>\r\n"));
    }
    report.push_str(&format!(
        "Disposition: manual-action/{sending_mode}; displayed\r\n"
    ));
    let text = format!(
        "This is a receipt for the message you sent to {} with the subject \"{}\". \
         It was displayed on the recipient's screen. That doesn't say it was \
         read or understood.\r\n",
        receipt.recipient.email, receipt.original_subject,
    );
    let report_part = MimePart::new(
        if ascii {
            "message/disposition-notification"
        } else {
            "message/global-disposition-notification"
        },
        BodyPart::Text(report.into()),
    );
    let report_part = if ascii {
        report_part.transfer_encoding("7bit")
    } else {
        report_part
    };
    let mut builder = mail_builder::MessageBuilder::new()
        .message_id(msg_id.clone())
        .from((
            receipt.recipient.name.as_deref().unwrap_or_default(),
            receipt.recipient.email.as_str(),
        ))
        .to(mail_builder::headers::address::Address::new_list(
            receipt
                .to
                .iter()
                .map(|address| {
                    mail_builder::headers::address::Address::from((
                        address.name.as_deref().unwrap_or_default(),
                        address.email.as_str(),
                    ))
                })
                .collect(),
        ))
        .subject(format!("Read: {}", receipt.original_subject))
        .body(MimePart::new(
            ContentType::new("multipart/report")
                .attribute("report-type", "disposition-notification"),
            vec![
                MimePart::new("text/plain", BodyPart::Text(text.into())),
                report_part,
            ],
        ));
    if let Some(id) = original_id {
        builder = builder.in_reply_to(id).references(id);
    }
    if !receipt.user_agreed {
        // RFC 3834: other automatic responders shouldn't answer it.
        builder = builder.header(
            "Auto-Submitted",
            mail_builder::headers::raw::Raw::new("auto-replied"),
        );
    }
    let raw = builder
        .write_to_vec()
        .map_err(|error| CoreError::Mime(format!("building a read receipt: {error}")))?;
    Ok((format!("<{msg_id}>"), raw))
}

pub fn build_message(out: &OutgoingMessage) -> Result<(String, Vec<u8>)> {
    build_message_with_attachments(
        out,
        out.attachments.iter().map(|att| MessageAttachment {
            filename: std::borrow::Cow::Borrowed(att.filename.as_str()),
            mime_type: std::borrow::Cow::Borrowed(att.mime_type.as_str()),
            bytes: std::borrow::Cow::Borrowed(att.bytes.as_slice()),
        }),
    )
}

/// Build a message while transferring attachment buffers into the MIME builder.
/// Send paths should use this when the attachments will not be needed again.
pub fn build_message_owned(mut out: OutgoingMessage<'_>) -> Result<(String, Vec<u8>)> {
    let attachments = std::mem::take(&mut out.attachments);
    build_message_with_attachments(
        &out,
        attachments.into_iter().map(|att| MessageAttachment {
            filename: std::borrow::Cow::Owned(att.filename),
            mime_type: std::borrow::Cow::Owned(att.mime_type),
            bytes: std::borrow::Cow::Owned(att.bytes),
        }),
    )
}

fn build_message_with_attachments<'a>(
    out: &'a OutgoingMessage<'_>,
    attachments: impl IntoIterator<Item = MessageAttachment<'a>>,
) -> Result<(String, Vec<u8>)> {
    // A large binary part expands predictably under MIME base64. Reserve near
    // the final size so Vec growth does not leave substantial unused capacity
    // in the complete message held through protection and provider upload.
    const MIN_PREALLOCATED_BINARY_BYTES: usize = 1024 * 1024;
    const MAX_PREALLOCATED_MIME_BYTES: usize = 64 * 1024 * 1024;
    fn encoded_binary_len(bytes: usize) -> usize {
        let encoded = bytes.div_ceil(3).saturating_mul(4);
        encoded.saturating_add(encoded.div_ceil(76).saturating_mul(2))
    }
    let mut binary_output_bytes = 0usize;
    let mut other_source_bytes = out.body_text.len();
    let mut part_count = 0usize;
    let mut attachments: Vec<MessageAttachment<'a>> = attachments.into_iter().collect();
    let calendar = attachments
        .iter()
        .position(|attachment| imip_method(&attachment.mime_type).is_some())
        .map(|index| attachments.remove(index));
    let stable_id = out
        .message_id
        .map(str::trim)
        .map(|value| value.trim_matches(['<', '>']))
        .filter(|value| !value.is_empty() && !value.contains('\r') && !value.contains('\n'));
    let msg_id = stable_id
        .map(|value| format!("<{value}>"))
        .unwrap_or_else(|| {
            format!(
                "<{}.{}@{}>",
                chrono::Utc::now().timestamp_micros(),
                rand_token(),
                out.message_id_domain
            )
        });

    let addresses = |items: &'a [Address]| {
        mail_builder::headers::address::Address::new_list(
            items
                .iter()
                .map(|address| {
                    mail_builder::headers::address::Address::from((
                        address.name.as_deref().unwrap_or_default(),
                        address.email.as_str(),
                    ))
                })
                .collect(),
        )
    };

    let mut builder = mail_builder::MessageBuilder::new()
        .message_id(msg_id.trim_matches(['<', '>']))
        .header(
            SUBMISSION_ID_HEADER,
            mail_builder::headers::raw::Raw::new(msg_id.trim_matches(['<', '>'])),
        )
        .from((
            out.from.name.as_deref().unwrap_or_default(),
            out.from.email.as_str(),
        ))
        .subject(out.subject)
        .text_body(out.body_text);
    if let Some(reply_to) = &out.reply_to {
        builder = builder.reply_to(mail_builder::headers::address::Address::new_address(
            reply_to
                .name
                .as_deref()
                .filter(|name| !name.trim().is_empty()),
            reply_to.email.as_str(),
        ));
    }

    let mut html_body = None;
    if let Some(html) = out.body_html
        && !html.trim().is_empty()
        && calendar.is_none()
    {
        let (html, inline_images) = extract_data_uri_images(html, out.message_id_domain);
        other_source_bytes = other_source_bytes.saturating_add(html.len());
        for img in &inline_images {
            binary_output_bytes =
                binary_output_bytes.saturating_add(encoded_binary_len(img.bytes.len()));
            part_count += 1;
        }
        if inline_images.is_empty() {
            builder = builder.html_body(html);
        } else {
            html_body = Some(html_with_images(html, inline_images));
        }
    }

    if !out.to.is_empty() {
        builder = builder.to(addresses(out.to));
    }
    if !out.cc.is_empty() {
        builder = builder.cc(addresses(out.cc));
    }
    if !out.bcc.is_empty() {
        builder = builder.bcc(addresses(out.bcc));
    }
    for att in &attachments {
        if att
            .mime_type
            .get(..5)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("text/"))
        {
            other_source_bytes = other_source_bytes.saturating_add(att.bytes.len());
        } else {
            binary_output_bytes =
                binary_output_bytes.saturating_add(encoded_binary_len(att.bytes.len()));
        }
        part_count += 1;
    }
    if let Some(calendar) = calendar {
        // The calendar goes out twice: inline and as invite.ics.
        other_source_bytes = other_source_bytes
            .saturating_add(out.body_html.map_or(0, str::len))
            .saturating_add(calendar.bytes.len().saturating_mul(2));
        part_count += 3;
        builder = builder.body(imip_body(out, calendar, attachments));
    } else if let Some(html) = html_body {
        // The text, and the HTML with its images, as alternatives of one
        // another, followed by any attachments.
        use mail_builder::mime::MimePart;
        let alternative = MimePart::new(
            "multipart/alternative",
            vec![MimePart::new("text/plain", out.body_text), html],
        );
        builder = builder.body(if attachments.is_empty() {
            alternative
        } else {
            let mut parts = Vec::with_capacity(attachments.len() + 1);
            parts.push(alternative);
            parts.extend(attachments.into_iter().map(attachment_part));
            MimePart::new("multipart/mixed", parts)
        });
    } else {
        for att in attachments {
            builder = builder.attachment(
                mail_builder::headers::content_type::ContentType::new(att.mime_type),
                att.filename,
                mail_builder::mime::BodyPart::Binary(att.bytes),
            );
        }
    }
    if let Some(irt) = out.in_reply_to {
        builder = builder.in_reply_to(irt.trim_matches(['<', '>']));
    }
    if !out.references.is_empty() {
        builder = builder.references(mail_builder::headers::message_id::MessageId::new_list(
            out.references
                .iter()
                .map(|reference| reference.trim_matches(['<', '>'])),
        ));
    }

    // Quoted-printable and UTF-8 text may expand during serialization.
    let reserve = binary_output_bytes
        .saturating_add(other_source_bytes.saturating_mul(4))
        .saturating_add(64 * 1024)
        .saturating_add(part_count.saturating_mul(1024));
    let raw = if binary_output_bytes >= MIN_PREALLOCATED_BINARY_BYTES
        && other_source_bytes <= binary_output_bytes / 64
        && reserve <= MAX_PREALLOCATED_MIME_BYTES
    {
        let mut raw = Vec::with_capacity(reserve);
        builder
            .write_to(&mut raw)
            .map_err(|e| CoreError::Mime(e.to_string()))?;
        raw
    } else {
        builder
            .write_to_vec()
            .map_err(|e| CoreError::Mime(e.to_string()))?
    };
    Ok((msg_id, raw))
}

pub(crate) fn rand_token() -> String {
    use rand::Rng;
    let mut rng = rand::rng();
    (0..10)
        .map(|_| {
            let c: u8 = rng.random_range(0..36);
            if c < 10 {
                (b'0' + c) as char
            } else {
                (b'a' + c - 10) as char
            }
        })
        .collect()
}

/// Quote a message body for reply/forward.
pub fn quote_body(original_text: &str, from: &Address, date_ms: i64) -> String {
    let when = chrono::DateTime::from_timestamp_millis(date_ms)
        .map(|d| d.format("%a, %b %-d, %Y at %-I:%M %p").to_string())
        .unwrap_or_default();
    let who = from.name.as_deref().unwrap_or(&from.email);
    let prefix = format!("\n\nOn {when}, {who} <{}> wrote:\n", from.email);
    let quoted_len = original_text.lines().fold(0usize, |bytes, line| {
        bytes.saturating_add(line.len()).saturating_add(3)
    });
    let mut quoted = String::with_capacity(prefix.len().saturating_add(quoted_len));
    quoted.push_str(&prefix);
    for line in original_text.lines() {
        quoted.push_str("> ");
        quoted.push_str(line);
        quoted.push('\n');
    }
    quoted
}

/// Strip Re:/Fwd: prefixes and normalize whitespace/case for subject threading.
pub fn normalize_subject(subject: &str) -> String {
    static PREFIX: Lazy<regex::Regex> = Lazy::new(|| {
        regex::Regex::new(r"(?i)^\s*((re|fwd?|fw|aw|sv)\s*(\[\d+\])?\s*:\s*)+").unwrap()
    });
    static BRACKETS: Lazy<regex::Regex> =
        Lazy::new(|| regex::Regex::new(r"^\s*\[[^\]]{1,60}\]\s*").unwrap());
    let s = PREFIX.replace(subject, "");
    let s = BRACKETS.replace(&s, "");
    s.trim().to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn submission_marker_survives_message_id_rewrite() {
        let mut message = outgoing("content", None);
        message.message_id = Some("stable-submission@test.dev");
        let (id, raw) = build_message(&message).unwrap();
        let raw = String::from_utf8(raw).unwrap();
        let rewritten = raw.replace(
            &format!("Message-ID: {id}"),
            "Message-ID: <rewritten@server>",
        );
        let parsed = parse_header_block(rewritten.as_bytes()).unwrap();
        assert_eq!(parsed.message_id.as_deref(), Some("rewritten@server"));
        assert_eq!(
            parsed.submission_id.as_deref(),
            Some("stable-submission@test.dev")
        );
    }

    #[test]
    fn image_extraction_preserves_invalid_neighbors_and_unterminated_tail() {
        let invalid = "<img src=\"data:image/png;base64,%%%\">";
        assert!(matches!(
            extract_data_uri_images(invalid, "example.com").0,
            std::borrow::Cow::Borrowed(_)
        ));
        let tail = "<img src=\"data:image/png;base64,unterminated";
        let html = format!("é{invalid}<img src=\"data:image/png;base64,AAE=\">{invalid}{tail}");
        let (rewritten, images) = extract_data_uri_images(&html, "example.com");
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].bytes, [0, 1]);
        assert_eq!(
            rewritten,
            format!(
                "é{invalid}<img src=\"cid:{}\">{invalid}{tail}",
                images[0].cid
            )
        );
    }

    #[test]
    fn borrowed_attachment_keeps_source_and_round_trips_large_bytes() {
        let mut message = outgoing("body", None);
        message.attachments.push(OutgoingAttachment {
            filename: "é-large.bin".into(),
            mime_type: "application/octet-stream".into(),
            bytes: vec![0x93; 1024 * 1024],
        });
        let pointer = message.attachments[0].bytes.as_ptr();
        let (_, raw) = build_message(&message).unwrap();
        assert_eq!(message.attachments[0].bytes.as_ptr(), pointer);
        let parsed = mail_parser::MessageParser::default().parse(&raw).unwrap();
        let part = parsed.attachments().next().unwrap();
        assert_eq!(part.attachment_name(), Some("é-large.bin"));
        assert_eq!(part.contents(), message.attachments[0].bytes);
    }

    #[test]
    fn parsed_body_retains_decoded_unicode_and_generated_html_text() {
        let raw = b"From: alice@example.com\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\nH=C3=A9llo";
        assert_eq!(parse_message(raw).unwrap().text.as_deref(), Some("Héllo"));
        let raw = b"From: alice@example.com\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<p>Hello <b>mail</b></p>";
        let parsed = parse_message(raw).unwrap();
        assert!(parsed.text.unwrap().contains("Hello mail"));
        assert!(parsed.html.unwrap().contains("<b>mail</b>"));
    }

    #[test]
    fn cid_rewrite_preserves_unknown_markup_and_reuses_repeated_payload() {
        let uri = format!("data:image/png;base64,{}", "A".repeat(4096));
        let map = std::collections::HashMap::from([("logo".to_owned(), uri.clone())]);
        let html = "é<img SRC='cid: <logo> '><img src=\"cid:logo\"><img src='cid: missing '>";
        let expected = format!("é<img src=\"{uri}\"><img src=\"{uri}\"><img src='cid: missing '>");
        let rewritten = rewrite_cid_src(html, &map);
        assert_eq!(rewritten, expected);
        assert_eq!(rewritten.capacity(), rewritten.len());
        assert_eq!(
            rewrite_cid_src(html, &std::collections::HashMap::new()),
            html
        );
    }

    #[test]
    fn thread_references_preserve_reply_order_and_skip_repeated_ancestors() {
        let raw = b"From: a@example.com\r\nIn-Reply-To: <latest@example.com>\r\nReferences: <root@example.com> <latest@example.com> <root@example.com> <middle@example.com>\r\n\r\nbody";
        assert_eq!(
            parse_message(raw).unwrap().headers.references,
            [
                "latest@example.com",
                "root@example.com",
                "middle@example.com"
            ]
        );
    }

    #[test]
    fn html_signature_recognizes_bom_case_and_xml_without_decoding() {
        assert!(decoded_looks_like_html(
            b"\xef\xbb\xbf \r\n<HTML><body>mail"
        ));
        assert!(decoded_looks_like_html(b"<?XML version='1.0'?><HTML>\xff"));
        assert!(!decoded_looks_like_html(b"\xff<HTML>"));
        assert!(!decoded_looks_like_html(b"plain <html> text"));
        let mut xml = b"<?xml".to_vec();
        xml.resize(1024, b' ');
        xml.extend_from_slice(b"<html>");
        assert!(!decoded_looks_like_html(&xml));
    }

    #[test]
    fn transfer_header_detection_preserves_line_and_unicode_boundaries() {
        assert!(header_declares_base64(
            "Content-Transfer-Encoding: \u{2003}BaSe64\u{2003}\r\n".as_bytes()
        ));
        assert!(!header_declares_base64(
            b"X-Content-Transfer-Encoding: base64\r\n"
        ));
        assert!(!header_declares_base64(
            "éContent-Transfer-Encoding: base64".as_bytes()
        ));
        assert!(header_declares_qp(
            b"Content-Transfer-Encoding: QUOTED-PRINTABLE\r\n"
        ));
        assert!(header_declares_qp(b"\xff quoted-printable"));
    }

    #[test]
    fn base64_transfer_accepts_wrapping_and_missing_padding() {
        for encoded in [b"aGVsbG8=".as_slice(), b"aGVsbG8", b"aG Vs\r\nbG8=\t"] {
            assert_eq!(
                decode_base64_transfer(encoded).as_deref(),
                Some(b"hello".as_slice())
            );
        }
        assert_eq!(decode_base64_transfer(b" \r\n\t"), Some(Vec::new()));
        assert!(decode_base64_transfer(b"%%%invalid").is_none());
    }

    #[test]
    fn automated_sender_prefixes_preserve_case_tags_and_utf8() {
        for email in [
            "NoReply+ticket@example.com",
            "MAILER-DAEMON@example.com",
            "ALERTé@example.com",
        ] {
            assert!(robot_sender(email), "{email}");
        }
        for email in [
            "éalert@example.com",
            "alice+NOREPLY@example.com",
            "no@example.com",
        ] {
            assert!(!robot_sender(email), "{email}");
        }
        let raw = b"From: alice@example.com\r\nPrecedence: BuLk\r\n\r\nbody";
        assert!(parse_message(raw).unwrap().headers.is_automated);
    }

    #[test]
    fn aligned_sender_falls_through_to_misaligned_transmitter() {
        let raw = b"From: Alice <alice@corp.com>\r\nSender: Delegate <sender@MAIL.corp.COM>\r\nReturn-Path: <bounce@esp.example>\r\nDKIM-Signature: v=1; d=dkim.example;\r\n\r\nbody";
        assert_eq!(
            parse_message(raw).unwrap().headers.via.as_deref(),
            Some("bounce@esp.example")
        );
        let raw = b"From: alice@corp.com\r\nSender: sender@mail.corp.com\r\nReturn-Path: <bounce@corp.com>\r\nDKIM-Signature: v=1; d=dkim.example;\r\n\r\nbody";
        assert_eq!(
            parse_message(raw).unwrap().headers.via.as_deref(),
            Some("dkim.example")
        );
    }

    #[test]
    fn normalizes_subjects() {
        assert_eq!(normalize_subject("Re: Re: Hello World"), "hello world");
        assert_eq!(normalize_subject("Fwd: [team] Standup"), "standup");
        assert_eq!(normalize_subject("FW: RE: x"), "x");
    }

    #[test]
    fn parses_simple_message() {
        let raw = b"Message-ID: <abc@example.com>\r\nFrom: Alice <alice@example.com>\r\nTo: Bob <bob@example.com>\r\nSubject: Hi\r\nDate: Mon, 1 Jan 2024 10:00:00 +0000\r\nContent-Type: text/plain\r\n\r\nHello Bob!\r\n";
        let parsed = parse_message(raw).unwrap();
        assert_eq!(parsed.headers.subject, "Hi");
        assert_eq!(
            parsed.headers.from.as_ref().unwrap().email,
            "alice@example.com"
        );
        assert!(parsed.text.unwrap().contains("Hello Bob"));
    }

    #[test]
    fn attachment_with_content_id_is_not_inline() {
        let raw = b"From: alice@example.com\r\nTo: bob@example.com\r\nSubject: Raport\r\nContent-Type: multipart/mixed; boundary=b1\r\n\r\n--b1\r\nContent-Type: text/plain\r\n\r\nHi\r\n--b1\r\nContent-Type: application/msword; name=\"raport.docx\"\r\nContent-Disposition: attachment; filename=\"raport.docx\"\r\nContent-ID: <f_mqkxxopb0>\r\nContent-Transfer-Encoding: base64\r\n\r\nAAAA\r\n--b1--\r\n";
        let parsed = parse_message(raw).unwrap();
        assert_eq!(parsed.attachments.len(), 1);
        assert_eq!(
            parsed.attachments[0].filename.as_deref(),
            Some("raport.docx")
        );
        assert!(!parsed.attachments[0].is_inline);
    }

    #[test]
    fn embedded_image_with_content_id_stays_inline() {
        let raw = b"From: alice@example.com\r\nTo: bob@example.com\r\nSubject: Newsletter\r\nContent-Type: multipart/related; boundary=b1\r\n\r\n--b1\r\nContent-Type: text/html\r\n\r\n<img src=\"cid:logo@cid\">\r\n--b1\r\nContent-Type: image/png\r\nContent-Disposition: inline; filename=\"logo.png\"\r\nContent-ID: <logo@cid>\r\nContent-Transfer-Encoding: base64\r\n\r\nAAAA\r\n--b1--\r\n";
        let parsed = parse_message(raw).unwrap();
        assert_eq!(parsed.attachments.len(), 1);
        assert!(parsed.attachments[0].is_inline);
    }

    #[test]
    fn captures_raw_list_unsubscribe_headers() {
        // mail-parser types List-Unsubscribe as an Address header, which once
        // made as_text() return None and left the DB column empty. The value
        // here is folded across lines to exercise unfolding too.
        let raw = b"From: news@example.com\r\nTo: bob@example.com\r\nSubject: Weekly\r\nList-Unsubscribe: <https://example.com/unsub?u=1>,\r\n <mailto:leave@example.com>\r\nList-Unsubscribe-Post: List-Unsubscribe=One-Click\r\nContent-Type: text/plain\r\n\r\nhi\r\n";
        let parsed = parse_message(raw).unwrap();
        assert_eq!(
            parsed.headers.list_unsubscribe.as_deref(),
            Some("<https://example.com/unsub?u=1>, <mailto:leave@example.com>")
        );
        assert_eq!(
            parsed.headers.list_unsubscribe_post.as_deref(),
            Some("List-Unsubscribe=One-Click")
        );
    }

    fn one_click_message(authentication: &str, signatures: &[&str]) -> String {
        let mut raw = format!("Authentication-Results: {authentication}\r\n");
        for signature in signatures {
            raw.push_str(&format!("DKIM-Signature: {signature}\r\n"));
        }
        raw.push_str(concat!(
            "From: News <news@news.example>\r\n",
            "List-Unsubscribe: <https://news.example/u/1>\r\n",
            "List-Unsubscribe-Post: List-Unsubscribe=One-Click\r\n",
            "Subject: Weekly\r\n\r\nbody"
        ));
        raw
    }

    const COVERING: &str = "v=1; a=rsa-sha256; d=news.example; s=s1;\r\n\th=From:Subject:\r\n List-Unsubscribe:List-Unsubscribe-Post; bh=x;\r\n\tb=AbCdEfGh12\r\n 345";
    const PARTIAL: &str = "v=1; a=rsa-sha256; d=news.example; s=s2; h=From:Subject:List-Unsubscribe; bh=x; b=ZyXwVuTs999";

    #[test]
    fn one_click_needs_the_passing_signature_to_cover_both_fields() {
        // Gmail names the signature that passed by the start of its b= tag.
        let gmail = "mx.google.com; dkim=pass header.i=@news.example header.s=s1 header.b=AbCdEfGh";
        assert!(one_click_signed(
            one_click_message(gmail, &[PARTIAL, COVERING]).as_bytes()
        ));
        // The case of the b= prefix matters (RFC 6008).
        let wrong_case = "mx.google.com; dkim=pass header.b=abcdefgh";
        assert!(!one_click_signed(
            one_click_message(wrong_case, &[COVERING]).as_bytes()
        ));
        // Only the signature that passed counts, not one that covers both.
        let partial_passed = "mx.example; dkim=pass header.b=ZyXwVuTs; dkim=fail header.b=AbCdEfGh";
        assert!(!one_click_signed(
            one_click_message(partial_passed, &[PARTIAL, COVERING]).as_bytes()
        ));
        // A claimed pass inside a comment is not a result.
        let comment = "mx.example; dkim=none (dkim=pass header.b=AbCdEfGh)";
        assert!(!one_click_signed(
            one_click_message(comment, &[COVERING]).as_bytes()
        ));
        assert!(!one_click_signed(
            one_click_message("mx.example; dkim=pass header.b=AbCdEfGh", &[]).as_bytes()
        ));
    }

    #[test]
    fn one_click_by_domain_needs_every_signature_of_that_domain() {
        // Microsoft reports only the signing domain.
        let microsoft = "spf=pass (sender IP is 192.0.2.1) smtp.mailfrom=news.example; dkim=pass (signature was verified) header.d=news.example;dmarc=pass action=none header.from=news.example;compauth=pass reason=100";
        assert!(one_click_signed(
            one_click_message(microsoft, &[COVERING]).as_bytes()
        ));
        assert!(!one_click_signed(
            one_click_message(microsoft, &[COVERING, PARTIAL]).as_bytes()
        ));
        let other_domain = "mx.example; dkim=pass header.d=esp.example";
        assert!(!one_click_signed(
            one_click_message(other_domain, &[COVERING]).as_bytes()
        ));
        // An identity in a subdomain still names the signing domain.
        let identity = "mx.example; dkim=pass header.i=@mail.news.example";
        assert!(one_click_signed(
            one_click_message(identity, &[COVERING]).as_bytes()
        ));
        let lookalike = "mx.example; dkim=pass header.i=@evilnews.example";
        assert!(!one_click_signed(
            one_click_message(lookalike, &[COVERING]).as_bytes()
        ));
    }

    #[test]
    fn one_click_needs_exactly_one_of_each_field() {
        let gmail = "mx.google.com; dkim=pass header.b=AbCdEfGh";
        let signed = one_click_message(gmail, &[COVERING]);
        let added = signed.replacen(
            "List-Unsubscribe:",
            "List-Unsubscribe: <https://elsewhere.example/u>\r\nList-Unsubscribe:",
            1,
        );
        assert!(!one_click_signed(added.as_bytes()));
        let twice = signed.replacen(
            "List-Unsubscribe-Post:",
            "List-Unsubscribe-Post: List-Unsubscribe=One-Click\r\nList-Unsubscribe-Post:",
            1,
        );
        assert!(!one_click_signed(twice.as_bytes()));
        let other_value = signed.replace(
            "List-Unsubscribe-Post: List-Unsubscribe=One-Click",
            "List-Unsubscribe-Post: List-Unsubscribe=Later",
        );
        assert!(!one_click_signed(other_value.as_bytes()));
    }

    #[test]
    fn list_id_keeps_only_the_identifier() {
        assert_eq!(
            list_id("Weekly news <News.Example.COM>").as_deref(),
            Some("news.example.com")
        );
        assert_eq!(
            list_id("list.example.org").as_deref(),
            Some("list.example.org")
        );
        assert_eq!(list_id("<>"), None);
        assert_eq!(list_id("just words"), None);
        let raw = b"From: a@b.example\r\nList-Id: \"Dev list\"\r\n <dev.lists.example>\r\nSubject: x\r\n\r\nbody";
        assert_eq!(
            parse_message(raw).unwrap().headers.list_id.as_deref(),
            Some("dev.lists.example")
        );
    }

    #[test]
    fn accepts_receiver_stamped_bimi_pass() {
        let raw = concat!(
            "Authentication-Results: mx.example; dkim=pass header.d=brand.example;\r\n",
            " dmarc=pass header.from=brand.example; bimi=pass header.d=brand.example\r\n",
            "From: Brand <hello@brand.example>\r\nSubject: hi\r\n\r\nbody"
        );
        let headers = parse_message(raw.as_bytes()).unwrap().headers;
        assert_eq!(headers.sender_verification, SenderVerification::Bimi);
    }

    #[test]
    fn bimi_authority_pass_is_a_verified_brand() {
        let raw = concat!(
            "Authentication-Results: mx.example; bimi=pass header.d=brand.example ",
            "header.selector=default policy.authority=pass\r\n",
            "From: Brand <hello@brand.example>\r\nSubject: hi\r\n\r\nbody"
        );
        let headers = parse_message(raw.as_bytes()).unwrap().headers;
        assert_eq!(headers.sender_verification, SenderVerification::Brand);
    }

    #[test]
    fn aligned_dmarc_and_dkim_authenticate_the_visible_domain() {
        let raw = concat!(
            "Authentication-Results: mx.example; dkim=pass header.d=brand.example; ",
            "dmarc=pass header.from=brand.example\r\n",
            "From: Brand <hello@brand.example>\r\nSubject: hi\r\n\r\nbody"
        );
        assert_eq!(
            parse_message(raw.as_bytes())
                .unwrap()
                .headers
                .sender_verification,
            SenderVerification::Domain
        );
    }

    #[test]
    fn microsoft_compauth_pass_authenticates_sender() {
        let raw = concat!(
            "Authentication-Results: spf=pass smtp.mailfrom=sender.example; ",
            "dkim=none; dmarc=none; compauth=pass reason=109\r\n",
            "From: Sender <hello@sender.example>\r\nSubject: hi\r\n\r\nbody"
        );
        assert_eq!(
            parse_message(raw.as_bytes())
                .unwrap()
                .headers
                .sender_verification,
            SenderVerification::Microsoft
        );
    }

    #[test]
    fn spf_or_unaligned_dkim_never_earn_a_badge() {
        for authentication in [
            "spf=pass smtp.mailfrom=brand.example",
            "dmarc=pass header.from=brand.example; dkim=none",
            "dmarc=pass header.from=brand.example; dkim=pass header.d=attacker.example",
        ] {
            let raw = format!(
                "Authentication-Results: mx.example; {authentication}\r\nFrom: Brand <hello@brand.example>\r\nSubject: hi\r\n\r\nbody"
            );
            assert_eq!(
                parse_message(raw.as_bytes())
                    .unwrap()
                    .headers
                    .sender_verification,
                SenderVerification::None,
                "unexpected verification for {authentication}"
            );
        }
    }

    #[test]
    fn ignores_a_lower_forged_authentication_results_field() {
        let raw = concat!(
            "Authentication-Results: mx.receiver.example; dmarc=fail header.from=brand.example\r\n",
            "Authentication-Results: attacker.example; dkim=pass header.d=brand.example; ",
            "dmarc=pass header.from=brand.example; bimi=pass policy.authority=pass\r\n",
            "From: Brand <hello@brand.example>\r\nSubject: hi\r\n\r\nbody"
        );
        let headers = parse_message(raw.as_bytes()).unwrap().headers;
        assert_eq!(headers.sender_verification, SenderVerification::None);
    }

    #[test]
    fn rejects_receiver_stamped_bimi_failure() {
        let raw = concat!(
            "Authentication-Results: mx.example; dmarc=fail header.from=brand.example; ",
            "bimi=fail header.d=brand.example\r\n",
            "From: Brand <hello@brand.example>\r\nSubject: hi\r\n\r\nbody"
        );
        assert_eq!(
            parse_message(raw.as_bytes())
                .unwrap()
                .headers
                .sender_verification,
            SenderVerification::None
        );
    }

    #[test]
    fn does_not_mistake_bimi_pass_in_a_comment_for_a_result() {
        let raw = concat!(
            "Authentication-Results: mx.example; dmarc=pass header.from=brand.example ",
            "(upstream said; bimi=pass policy.authority=pass)\r\n",
            "From: Brand <hello@brand.example>\r\nSubject: hi\r\n\r\nbody"
        );
        assert_eq!(
            parse_message(raw.as_bytes())
                .unwrap()
                .headers
                .sender_verification,
            SenderVerification::None
        );
    }

    #[test]
    fn html_snippets_extract_readable_text_without_double_decoding() {
        let html = "\n<div style=\"font-weight:bold\">Dear <b>Alice</b> &amp; Bob<br>Welcome</div><script>hidden()</script>";
        let snippet = make_html_snippet(html);
        assert_eq!(snippet, "Dear Alice & Bob Welcome");
        assert!(!snippet.contains("&#"));
        assert!(!snippet.contains("&lt;"));
        assert!(!snippet.contains('<'));
        assert!(!snippet.contains("hidden"));

        // Only one HTML entity layer is decoded. This preserves text that is
        // intentionally displaying the literal string "&lt;div&gt;".
        assert_eq!(
            make_html_snippet("<div>&amp;lt;div&amp;gt;</div>"),
            "&lt;div&gt;"
        );
        assert_eq!(make_snippet("2 < 3 &amp; 4"), "2 < 3 &amp; 4");
        assert_eq!(
            make_body_snippet(Some("\r\n\t"), Some("<p>HTML fallback</p>")),
            "HTML fallback"
        );
    }

    #[test]
    fn sanitizing_html_keeps_css_but_removes_scripts() {
        let cleaned = sanitize_html(
            r#"<style>body { font-family: Georgia, serif; }</style><script>alert(1)</script><p>Mail</p>"#,
        );

        assert!(
            cleaned.contains("font-family: Georgia"),
            "cleaned: {cleaned}"
        );
        assert!(!cleaned.contains("<script"), "cleaned: {cleaned}");
        assert!(!cleaned.contains("alert(1)"), "cleaned: {cleaned}");
        assert!(cleaned.contains("<p>Mail</p>"), "cleaned: {cleaned}");
    }

    #[test]
    fn document_tag_scan_preserves_case_boundaries_and_quoted_angles() {
        let html = "é界<HTML class='a'><HEAD><TITLE>x</TITLE></HEAD><bodyguard>skip</bodyguard><BoDy data-test='>'>👩‍🚀</bOdY></HTML>";
        let body = html_document_section(html, "body").unwrap();
        assert_eq!(body.attributes, " data-test='>'");
        assert_eq!(body.content, "👩‍🚀");
        assert_eq!(
            html_document_section(html, "head").unwrap().content,
            "<TITLE>x</TITLE>"
        );
        assert!(html_document_section("<bodyguard>x</bodyguard>", "body").is_none());
        assert_eq!(
            html_document_section("<BODY>unterminated é", "body")
                .unwrap()
                .content,
            "unterminated é"
        );
        assert!(find_html_tag("é<body>x</body>", "body", 1, false).is_some());
    }

    #[test]
    fn sanitizing_complete_html_preserves_document_css_boundary_and_safe_hooks() {
        let cleaned = sanitize_html(
            r#"<!doctype html><html class="mail" style="margin:0"><head>
                <title>Preview metadata</title>
                <meta name="viewport" content="width=device-width">
                <style>.hidden{display:none}.content{height:40px}</style>
                <script>document.body.remove()</script>
            </head><body id="message" class="campaign" style="margin:0" onload="steal()">
                <span class="hidden">Hidden preview</span><div class="content">Visible body</div>
            </body></html>"#,
        );

        assert!(cleaned.starts_with("<html"), "cleaned: {cleaned}");
        assert!(cleaned.contains("<head>"), "cleaned: {cleaned}");
        assert!(
            cleaned.contains("<title>Preview metadata</title>"),
            "cleaned: {cleaned}"
        );
        assert!(cleaned.contains("class=\"hidden\""), "cleaned: {cleaned}");
        assert!(cleaned.contains("id=\"message\""), "cleaned: {cleaned}");
        assert!(cleaned.contains("class=\"campaign\""), "cleaned: {cleaned}");
        assert!(cleaned.contains("style=\"margin:0\""), "cleaned: {cleaned}");
        assert!(!cleaned.contains("onload"), "cleaned: {cleaned}");
        assert!(!cleaned.contains("document.body"), "cleaned: {cleaned}");
        assert!(!cleaned.contains("steal()"), "cleaned: {cleaned}");
        assert!(
            cleaned.find("</head>").expect("head should close")
                < cleaned
                    .find("Hidden preview")
                    .expect("body should follow head"),
            "cleaned: {cleaned}"
        );
    }

    #[test]
    fn sanitizing_html_keeps_text_direction() {
        let cleaned = sanitize_html(
            r#"<html dir="rtl"><body dir="rtl"><p dir="rtl">שלום</p><div dir="ltr">Hello</div><span dir="auto">x</span></body></html>"#,
        );

        assert!(
            cleaned.starts_with(r#"<html dir="rtl">"#),
            "cleaned: {cleaned}"
        );
        assert!(
            cleaned.contains(r#"<body dir="rtl">"#),
            "cleaned: {cleaned}"
        );
        assert!(cleaned.contains(r#"<p dir="rtl">"#), "cleaned: {cleaned}");
        assert!(cleaned.contains(r#"<div dir="ltr">"#), "cleaned: {cleaned}");
        assert!(
            cleaned.contains(r#"<span dir="auto">"#),
            "cleaned: {cleaned}"
        );
    }

    fn outgoing<'a>(text: &'a str, html: Option<&'a str>) -> OutgoingMessage<'a> {
        OutgoingMessage {
            from: Address {
                name: Some("Me".into()),
                email: "me@example.com".into(),
            },
            reply_to: None,
            to: &[],
            cc: &[],
            bcc: &[],
            subject: "s",
            body_text: text,
            body_html: html,
            in_reply_to: None,
            references: &[],
            message_id: None,
            message_id_domain: "example.com",
            attachments: Vec::new(),
        }
    }

    #[test]
    fn html_body_builds_multipart_alternative() {
        let (_, raw) = build_message(&outgoing("plain", Some("<b>rich</b>"))).unwrap();
        let s = String::from_utf8_lossy(&raw);
        assert!(s.contains("multipart/alternative"), "raw:\n{s}");
        assert!(s.contains("text/html"));
        assert!(s.contains("text/plain"));
    }

    const ANSWER: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nMETHOD:REPLY\r\nBEGIN:VEVENT\r\nUID:evt-1@example.com\r\nSEQUENCE:0\r\nORGANIZER:mailto:alice@example.com\r\nATTENDEE;PARTSTAT=ACCEPTED:mailto:me@example.com\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

    fn part_types(raw: &[u8]) -> Vec<String> {
        let message = mail_parser::MessageParser::default().parse(raw).unwrap();
        message
            .parts
            .iter()
            .map(|part| {
                let content_type = part.content_type().unwrap();
                let mut kind = format!(
                    "{}/{}",
                    content_type.ctype(),
                    content_type.subtype().unwrap_or_default()
                );
                for name in ["method", "charset", "name"] {
                    if let Some(value) = content_type.attribute(name) {
                        kind.push_str(&format!(";{name}={value}"));
                    }
                }
                kind.to_ascii_lowercase()
            })
            .collect()
    }

    #[test]
    fn a_calendar_part_goes_out_as_an_imip_message() {
        let mut message = outgoing("Accepted: Design review", None);
        message.attachments = vec![
            OutgoingAttachment {
                filename: "invite.ics".into(),
                mime_type: "text/calendar; method=reply; charset=UTF-8".into(),
                bytes: ANSWER.as_bytes().to_vec(),
            },
            OutgoingAttachment {
                filename: "agenda.pdf".into(),
                mime_type: "application/pdf".into(),
                bytes: b"%PDF-1.7".to_vec(),
            },
        ];
        let (_, raw) = build_message(&message).unwrap();
        // RFC 6047 §2.4: the method parameter matches METHOD, and the text
        // stands in for mail programs that don't read calendars.
        assert_eq!(
            part_types(&raw),
            [
                "multipart/mixed",
                "multipart/alternative",
                "text/plain;charset=utf-8",
                "text/calendar;method=reply;charset=utf-8",
                "application/ics;name=invite.ics",
                "application/pdf",
            ]
        );
        let parsed = parse_message(&raw).unwrap();
        assert_eq!(
            parsed.calendar_parts.len(),
            1,
            "the attached copy isn't read twice"
        );
        assert!(parsed.calendar_parts[0].contains("UID:evt-1@example.com"));
        assert_eq!(parsed.text.as_deref(), Some("Accepted: Design review"));
    }

    #[test]
    fn a_calendar_file_the_user_attached_stays_an_attachment() {
        let mut message = outgoing("The event", None);
        message.attachments.push(OutgoingAttachment {
            filename: "event.ics".into(),
            mime_type: "text/calendar".into(),
            bytes: ANSWER.as_bytes().to_vec(),
        });
        let (_, raw) = build_message(&message).unwrap();
        assert_eq!(
            part_types(&raw),
            [
                "multipart/mixed",
                "text/plain;charset=utf-8",
                "text/calendar"
            ]
        );
    }

    #[test]
    fn text_only_message_stays_plain() {
        let (_, raw) = build_message(&outgoing("plain", None)).unwrap();
        let s = String::from_utf8_lossy(&raw);
        assert!(!s.contains("text/html"));
        assert!(!s.contains("Reply-To:"));
    }

    #[test]
    fn identity_reply_to_is_written_as_a_header() {
        let mut message = outgoing("plain", None);
        message.reply_to = Some(Address {
            name: None,
            email: "desk@example.com".into(),
        });
        let (_, raw) = build_message(&message).unwrap();
        let s = String::from_utf8_lossy(&raw);
        assert!(s.contains("Reply-To: <desk@example.com>"), "raw:\n{s}");
    }

    #[test]
    fn caller_supplied_message_id_is_stable_and_header_safe() {
        let mut message = outgoing("plain", None);
        message.message_id = Some("<stable.retry@example.com>");
        let (first_id, first) = build_message(&message).unwrap();
        let (second_id, second) = build_message(&message).unwrap();
        assert_eq!(first_id, "<stable.retry@example.com>");
        assert_eq!(first_id, second_id);
        assert!(String::from_utf8_lossy(&first).contains("stable.retry@example.com"));
        assert!(String::from_utf8_lossy(&second).contains("stable.retry@example.com"));

        message.message_id = Some("bad\r\nBcc: injected@example.com");
        let (generated, raw) = build_message(&message).unwrap();
        assert_ne!(generated, "<bad\r\nBcc: injected@example.com>");
        assert!(!String::from_utf8_lossy(&raw).contains("Bcc: injected@example.com"));
    }

    #[test]
    fn owned_attachment_builder_preserves_attachment_content() {
        let mut message = outgoing("body", None);
        message.message_id = Some("stable.attachment@example.com");
        message.attachments.push(OutgoingAttachment {
            filename: "binary.bin".into(),
            mime_type: "application/octet-stream".into(),
            bytes: vec![0, 1, 2, 127, 128, 255],
        });
        let (borrowed_id, borrowed) = build_message(&message).unwrap();
        let (owned_id, owned) = build_message_owned(message).unwrap();
        assert_eq!(borrowed_id, owned_id);
        for raw in [&borrowed, &owned] {
            let parsed = mail_parser::MessageParser::default().parse(raw).unwrap();
            let parts: Vec<_> = parsed.attachments().collect();
            assert_eq!(parts.len(), 1);
            assert_eq!(parts[0].attachment_name(), Some("binary.bin"));
            assert_eq!(parts[0].contents(), &[0, 1, 2, 127, 128, 255]);
        }
    }

    #[test]
    fn large_owned_attachment_round_trips_without_truncation() {
        let mut message = outgoing("body", None);
        message.message_id = Some("stable.large@example.com");
        let payload = vec![0x5a; 1024 * 1024];
        message.attachments.push(OutgoingAttachment {
            filename: "large.bin".into(),
            mime_type: "application/octet-stream".into(),
            bytes: payload.clone(),
        });
        let (message_id, raw) = build_message_owned(message).unwrap();
        assert_eq!(message_id, "<stable.large@example.com>");
        let parsed = mail_parser::MessageParser::default().parse(&raw).unwrap();
        let part = parsed.attachments().next().unwrap();
        assert_eq!(part.attachment_name(), Some("large.bin"));
        assert_eq!(part.contents(), payload);
    }

    #[test]
    fn streamed_reply_quote_matches_original_for_unicode_and_line_endings() {
        for text in ["", "\n", "one", "one\n", "é\r\n界\n\n👩‍🚀", "one\r"] {
            for name in [None, Some("é界".to_owned())] {
                let from = Address {
                    name,
                    email: "sender@example.test".into(),
                };
                let timestamp = 1_700_000_000_000;
                let when = chrono::DateTime::from_timestamp_millis(timestamp)
                    .unwrap()
                    .format("%a, %b %-d, %Y at %-I:%M %p")
                    .to_string();
                let who = from.name.as_deref().unwrap_or(&from.email);
                let body: String = text.lines().map(|line| format!("> {line}\n")).collect();
                let expected = format!("\n\nOn {when}, {who} <{}> wrote:\n{body}", from.email);
                assert_eq!(quote_body(text, &from, timestamp), expected);
            }
        }
    }

    #[test]
    fn borrowed_domain_alignment_matches_case_and_dot_boundaries() {
        for (a, b) in [
            ("Sub.Example.COM.", " example.com "),
            ("badexample.com", "example.com"),
            ("é.EXAMPLE.com", "example.COM"),
            ("", "."),
            ("abc.", ""),
            ("é.com", "É.com"),
        ] {
            let old_a = a.trim().trim_end_matches('.').to_ascii_lowercase();
            let old_b = b.trim().trim_end_matches('.').to_ascii_lowercase();
            let expected = old_a == old_b
                || old_a.ends_with(&format!(".{old_b}"))
                || old_b.ends_with(&format!(".{old_a}"));
            assert_eq!(domains_aligned(a, b), expected, "{a} / {b}");
        }
    }

    #[test]
    fn outgoing_borrowed_headers_preserve_unicode_and_references() {
        let to = [Address {
            name: Some("é界".into()),
            email: "to@example.test".into(),
        }];
        let cc = [Address {
            name: None,
            email: "cc@example.test".into(),
        }];
        let bcc = [Address {
            name: Some("Hidden".into()),
            email: "bcc@example.test".into(),
        }];
        let references = vec!["<first@example.test>".into(), "second@example.test".into()];
        let mut message = outgoing("body", None);
        message.to = &to;
        message.cc = &cc;
        message.bcc = &bcc;
        message.in_reply_to = Some("<second@example.test>");
        message.references = &references;
        message.message_id = Some("<stable@example.test>");
        let (id, raw) = build_message_owned(message).unwrap();
        let parsed = parse_message(&raw).unwrap();
        assert_eq!(id, "<stable@example.test>");
        assert_eq!(parsed.headers.to[0].name.as_deref(), Some("é界"));
        assert_eq!(parsed.headers.cc[0].email, "cc@example.test");
        assert_eq!(parsed.headers.bcc[0].email, "bcc@example.test");
        assert!(
            parsed
                .headers
                .references
                .contains(&"first@example.test".to_owned())
        );
        assert!(
            parsed
                .headers
                .references
                .contains(&"second@example.test".to_owned())
        );
    }

    #[test]
    fn outgoing_html_without_embedded_images_is_borrowed() {
        let html = "<p>é界👩‍🚀</p>".repeat(100_000);
        let (rewritten, images) = extract_data_uri_images(&html, "example.com");
        assert!(matches!(rewritten, std::borrow::Cow::Borrowed(_)));
        assert_eq!(rewritten.as_ptr(), html.as_ptr());
        assert!(images.is_empty());
    }

    #[test]
    fn embedded_image_rewrite_does_not_retain_base64_sized_capacity() {
        use base64::Engine;
        let encoded = base64::engine::general_purpose::STANDARD.encode(vec![7; 1024 * 1024]);
        let html = format!("<img src=\"data:image/png;base64,{encoded}\">");
        let (rewritten, images) = extract_data_uri_images(&html, "example.com");
        let std::borrow::Cow::Owned(rewritten) = rewritten else {
            panic!("image must be rewritten")
        };
        assert_eq!(images[0].bytes.len(), 1024 * 1024);
        assert!(rewritten.capacity() < 1024);
        let invalid = "<img src=\"data:image/png;base64,invalid!\">";
        let (unchanged, images) = extract_data_uri_images(invalid, "example.com");
        assert!(matches!(unchanged, std::borrow::Cow::Borrowed(_)));
        assert_eq!(unchanged.as_ptr(), invalid.as_ptr());
        assert!(images.is_empty());
    }

    #[test]
    fn data_uri_images_become_cid_inline_parts() {
        // 1x1 transparent PNG
        let png_b64 = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";
        let html = format!(r#"before <img src="data:image/png;base64,{png_b64}" alt="dot"> after"#);
        let (rewritten, images) = extract_data_uri_images(&html, "example.com");
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].mime_type, "image/png");
        assert!(rewritten.contains(&format!("src=\"cid:{}\"", images[0].cid)));
        assert!(!rewritten.contains("data:image/"));
        // PNG magic bytes survived the round-trip
        assert_eq!(&images[0].bytes[..4], &[0x89, b'P', b'N', b'G']);

        // And the full message embeds it as an inline part with a Content-ID.
        let (_, raw) = build_message(&outgoing("plain", Some(&html))).unwrap();
        let s = String::from_utf8_lossy(&raw);
        assert!(s.contains("Content-ID"), "raw:\n{s}");
        assert!(s.contains("image/png"));
        assert!(s.contains("inline"));
    }

    #[test]
    fn images_go_with_the_html_they_belong_to() {
        let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";
        let html = format!(
            r#"<p>Chart <img src="data:image/png;base64,{png}"> and photo <img src="data:image/jpeg;base64,/9j/"></p>"#
        );
        for attached in [false, true] {
            let mut message = outgoing("Chart [image] and photo [image]", Some(&html));
            if attached {
                message.attachments.push(OutgoingAttachment {
                    filename: "notes.pdf".into(),
                    mime_type: "application/pdf".into(),
                    bytes: b"%PDF-1.7".to_vec(),
                });
            }
            let (_, raw) = build_message(&message).unwrap();
            let parsed = mail_parser::MessageParser::default().parse(&raw).unwrap();
            let types: Vec<String> = parsed
                .parts
                .iter()
                .map(|part| {
                    let content_type = part.content_type().unwrap();
                    format!(
                        "{}/{}",
                        content_type.ctype(),
                        content_type.subtype().unwrap_or_default()
                    )
                })
                .collect();
            let mut expected = vec![
                "multipart/alternative",
                "text/plain",
                "multipart/related",
                "text/html",
                "image/png",
                "image/jpeg",
            ];
            if attached {
                expected.insert(0, "multipart/mixed");
                expected.push("application/pdf");
            }
            assert_eq!(types, expected, "attached: {attached}");

            let related = parsed
                .parts
                .iter()
                .find(|part| part.content_type().unwrap().subtype() == Some("related"))
                .unwrap();
            assert_eq!(
                related.content_type().unwrap().attribute("type"),
                Some("text/html")
            );
            let html = parsed.body_html(0).unwrap();
            for (index, extension) in [(1, "png"), (2, "jpg")] {
                let image = parsed
                    .parts
                    .iter()
                    .find(|part| {
                        part.attachment_name() == Some(format!("image{index}.{extension}").as_str())
                    })
                    .unwrap();
                let cid = image.content_id().unwrap();
                assert!(cid.starts_with(&format!("img{index}.")), "{cid}");
                assert!(cid.ends_with("@example.com"), "{cid}");
                assert!(html.contains(&format!("src=\"cid:{cid}\"")));
                let disposition = image.content_disposition().unwrap();
                assert_eq!(disposition.ctype(), "inline");
                assert_eq!(
                    disposition.attribute("filename"),
                    Some(format!("image{index}.{extension}").as_str())
                );
            }
            assert!(!html.contains("data:"));
            assert_eq!(
                parsed.body_text(0).unwrap(),
                "Chart [image] and photo [image]"
            );
        }
    }

    #[test]
    fn embedded_images_are_measured_without_decoding() {
        let html = concat!(
            r#"<p>a <img src="data:image/png;base64,AAAAAAAA"> b "#,
            r#"<img src="https://example.com/x.png"> <img src="data:image/gif;base64,AAAA"></p>"#,
        );
        assert_eq!(
            embedded_images(html),
            EmbeddedImages {
                source_bytes: "data:image/png;base64,AAAAAAAA".len()
                    + "data:image/gif;base64,AAAA".len(),
                image_bytes: 9,
            }
        );
        assert_eq!(embedded_images("<p>none</p>"), EmbeddedImages::default());
    }

    #[test]
    fn only_plain_image_types_become_parts() {
        let html = concat!(
            r#"<img src="data:image/png;charset=x;base64,AAE=">"#,
            r#"<img src="data:image/svg+xml;base64,PHN2Zy8+">"#,
            r#"<img src="data:image/png&#13;&#10;X-Evil: 1;base64,AAE=">"#,
            "<img src=\"data:image/png\r\nX-Evil: 1;base64,AAE=\">",
            r#"<img src="data:image/;base64,AAE=">"#,
        );
        let (rewritten, images) = extract_data_uri_images(html, "example.com");
        let found: Vec<_> = images
            .iter()
            .map(|image| (image.mime_type.as_str(), image.filename.as_str()))
            .collect();
        assert_eq!(
            found,
            [("image/png", "image1.png"), ("image/svg+xml", "image2.svg")]
        );
        assert_eq!(rewritten.matches("data:image/").count(), 3);
    }

    #[test]
    fn malformed_data_uri_is_left_alone() {
        let html = r#"<img src="data:image/png;base64,@@notbase64@@">"#;
        let (rewritten, images) = extract_data_uri_images(html, "example.com");
        assert!(images.is_empty());
        assert_eq!(rewritten, html);
    }
}
