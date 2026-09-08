//! MIME parsing (mail-parser), HTML sanitization (ammonia), and outgoing
//! message construction (mail-builder).

use crate::error::{CoreError, Result};
use crate::models::{Address, SenderVerification};
use mail_parser::MimeHeaders;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default)]
pub struct ParsedHeaders {
    pub message_id: Option<String>,
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
    /// The party that actually transmitted the message, when its domain does
    /// NOT align with From: (mailing lists, ESPs, send-on-behalf - and mail
    /// whose From: is spoofed). First misaligned identity out of the RFC 5322
    /// Sender:, the receiver-stamped Return-Path, and the DKIM d= domain;
    /// None when everything aligns with From:.
    pub via: Option<String>,
    /// Strongest authentication result reported by the receiving MTA.
    pub sender_verification: SenderVerification,
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

    fn param(params: &async_imap::imap_proto::BodyParams<'_>, key: &str) -> Option<String> {
        params.as_ref().and_then(|items| {
            items
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(key))
                .map(|(_, value)| value.to_string())
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
            path.iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(".")
        }
    }

    fn add_single(
        common: &BodyContentCommon<'_>,
        other: &BodyContentSinglePart<'_>,
        path: &[u32],
        plan: &mut MimePlan,
    ) {
        let section = section(path);
        let mime_type = format!("{}/{}", common.ty.ty, common.ty.subtype).to_ascii_lowercase();
        let filename = common
            .disposition
            .as_ref()
            .and_then(|value| param(&value.params, "filename"))
            .or_else(|| param(&common.ty.params, "name"))
            .map(|name| decode_encoded_words(&name));
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
                charset: param(&common.ty.params, "charset"),
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
    let has_content_type = String::from_utf8_lossy(&out)
        .to_ascii_lowercase()
        .contains("content-type:");
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
    String::from_utf8_lossy(mime_header)
        .to_ascii_lowercase()
        .contains("quoted-printable")
}

fn header_declares_base64(mime_header: &[u8]) -> bool {
    String::from_utf8_lossy(mime_header)
        .to_ascii_lowercase()
        .lines()
        .any(|line| {
            line.strip_prefix("content-transfer-encoding:")
                .is_some_and(|value| value.trim().eq_ignore_ascii_case("base64"))
        })
}

fn decode_base64_transfer(input: &[u8]) -> Option<Vec<u8>> {
    use base64::Engine;

    let compact: Vec<u8> = input
        .iter()
        .copied()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();
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
    let probe = String::from_utf8_lossy(probe).to_ascii_lowercase();
    probe.starts_with("<!doctype html")
        || probe.starts_with("<html")
        || probe.starts_with("<head")
        || probe.starts_with("<body")
        || probe.starts_with("<meta")
        || probe.starts_with("<style")
        || (probe.starts_with("<?xml") && probe.contains("<html"))
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

    format!(
        "<html{html_attributes}><head>{head}</head><body{body_attributes}>{body_content}</body></html>"
    )
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
    let lower = html.to_ascii_lowercase();
    let open_start = find_html_tag(&lower, tag, 0, false)?;
    let open_end = html_tag_end(html, open_start)?;
    let name_end = open_start + 1 + tag.len();
    let attributes = html.get(name_end..open_end)?;

    // The html element normally encloses the whole document and its explicit
    // closing tag is optional. Its content is not used by the caller, but the
    // same representation keeps attribute extraction consistent.
    let close_start = find_html_tag(&lower, tag, open_end + 1, true).unwrap_or(html.len());
    let content = html.get(open_end + 1..close_start)?;
    Some(HtmlDocumentSection {
        attributes,
        content,
    })
}

fn find_html_tag(lower: &str, tag: &str, from: usize, closing: bool) -> Option<usize> {
    let prefix = if closing {
        format!("</{tag}")
    } else {
        format!("<{tag}")
    };
    let mut cursor = from;
    while let Some(relative) = lower.get(cursor..)?.find(&prefix) {
        let start = cursor + relative;
        let boundary = lower.as_bytes().get(start + prefix.len()).copied();
        if boundary.is_none_or(|byte| byte == b'>' || byte == b'/' || byte.is_ascii_whitespace()) {
            return Some(start);
        }
        cursor = start + prefix.len();
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
    let probe = format!("<div{attributes}></div>");
    let cleaned = SANITIZER.clean(&probe).to_string();
    let Some(start) = cleaned.to_ascii_lowercase().find("<div") else {
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
    text.filter(|value| !value.trim().is_empty())
        .map(make_snippet)
        .or_else(|| html.map(make_html_snippet))
        .unwrap_or_default()
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
    CID_SRC
        .replace_all(html, |caps: &regex::Captures| {
            let cid = caps
                .get(1)
                .or_else(|| caps.get(2))
                .map(|m| m.as_str())
                .unwrap_or("");
            match map.get(&normalize_cid(cid)) {
                Some(data_uri) => format!("src=\"{data_uri}\""),
                None => caps.get(0).unwrap().as_str().to_string(),
            }
        })
        .into_owned()
}

#[cfg(test)]
mod cid_tests {
    use super::{normalize_cid, referenced_cids, rewrite_cid_src};
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
    let a = a.trim().trim_end_matches('.').to_ascii_lowercase();
    let b = b.trim().trim_end_matches('.').to_ascii_lowercase();
    a == b || a.ends_with(&format!(".{b}")) || b.ends_with(&format!(".{a}"))
}

/// The transmitting party to show as "via", when it doesn't align with From:.
/// Order: Sender: (self-declared), Return-Path (envelope sender, stamped by
/// the receiving server - a forged From: can't easily hide it), DKIM d=.
fn resolve_via(msg: &mail_parser::Message, from: Option<&Address>) -> Option<String> {
    let from_domain = from.map(|a| domain_of(&a.email).to_string());

    let sender_email = msg
        .sender()
        .and_then(|s| s.first())
        .and_then(addr_from)
        .map(|a| a.email);
    let return_path = match msg.return_path() {
        mail_parser::HeaderValue::Text(t) => Some(t.trim_matches(['<', '>', ' ']).to_string()),
        mail_parser::HeaderValue::TextList(l) => l
            .last()
            .map(|t| t.trim_matches(['<', '>', ' ']).to_string()),
        _ => None,
    }
    .filter(|s| !s.is_empty());
    let dkim_domain = msg
        .header("DKIM-Signature")
        .and_then(|h| h.as_text())
        .and_then(|s| DKIM_DOMAIN.captures(s))
        .map(|c| c[1].to_string());

    [sender_email, return_path, dkim_domain]
        .into_iter()
        .flatten()
        .find(|cand| match &from_domain {
            Some(fd) => !domains_aligned(domain_of(cand), fd),
            None => true,
        })
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
        .unwrap_or("")
        .to_ascii_lowercase();
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
    PREFIXES.iter().any(|p| local.starts_with(p))
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
struct AuthenticationResult {
    method: String,
    result: String,
    properties: Vec<(String, String)>,
}

/// Remove RFC-style comments before tokenizing an Authentication-Results
/// clause. This prevents text such as `(upstream said bimi=pass)` from being
/// mistaken for a receiver-authenticated result.
fn without_comments(value: &str) -> String {
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
    output
}

/// Parse only the first (topmost) Authentication-Results field. Receiving
/// MTAs prepend their result, while older fields below it may be supplied by
/// an untrusted sender or an intermediate relay.
fn authentication_results(msg: &mail_parser::Message) -> Vec<AuthenticationResult> {
    let Some(results) = first_raw_header(msg, "Authentication-Results") else {
        return Vec::new();
    };
    // Strip comments before splitting clauses: comments are allowed to contain
    // semicolons, and splitting first could turn comment text into a fake
    // method=result clause.
    let results = without_comments(&results);
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
            let properties = tokens
                .filter_map(|token| token.split_once('='))
                .map(|(name, value)| {
                    (
                        name.trim().to_ascii_lowercase(),
                        value
                            .trim_matches(|ch: char| ch == '"' || ch == '\'')
                            .trim_end_matches(',')
                            .to_ascii_lowercase(),
                    )
                })
                .collect();
            Some(AuthenticationResult {
                method: method.to_ascii_lowercase(),
                result: result.trim_end_matches(',').to_ascii_lowercase(),
                properties,
            })
        })
        .collect()
}

fn auth_property<'a>(result: &'a AuthenticationResult, name: &str) -> Option<&'a str> {
    result
        .properties
        .iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

/// Classify only strong, receiver-reported authentication. SPF alone never
/// qualifies: it authenticates the envelope sender and survives neither From
/// spoofing nor many forwarding paths. Standard domain authentication needs
/// both DMARC and a DKIM signature aligned with the visible From domain.
fn sender_verification(
    results: &[AuthenticationResult],
    from: Option<&Address>,
) -> SenderVerification {
    if let Some(bimi) = results
        .iter()
        .find(|result| result.method == "bimi" && result.result == "pass")
    {
        return if auth_property(bimi, "policy.authority") == Some("pass") {
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
        result.method == "dmarc"
            && result.result == "pass"
            && auth_property(result, "header.from")
                .is_some_and(|domain| domains_aligned(domain, from_domain))
    });
    let aligned_dkim_pass = results.iter().any(|result| {
        result.method == "dkim"
            && result.result == "pass"
            && auth_property(result, "header.d")
                .is_some_and(|domain| domains_aligned(domain, from_domain))
    });
    if dmarc_pass && aligned_dkim_pass {
        return SenderVerification::Domain;
    }

    if results
        .iter()
        .any(|result| result.method == "compauth" && result.result == "pass")
    {
        return SenderVerification::Microsoft;
    }

    SenderVerification::None
}

/// Split a List-Unsubscribe header into its URIs. RFC 2369 wraps each URI in
/// angle brackets; tolerate bare comma-separated values from sloppy senders.
pub fn parse_unsubscribe_uris(raw: &str) -> Vec<String> {
    let bracketed: Vec<String> = raw
        .match_indices('<')
        .filter_map(|(start, _)| {
            let rest = &raw[start + 1..];
            let end = rest.find('>')?;
            let uri = rest[..end].trim();
            (!uri.is_empty()).then(|| uri.to_string())
        })
        .collect();
    if !bracketed.is_empty() {
        return bracketed;
    }
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// RFC 8058 §3.1: the header value must be exactly "List-Unsubscribe=One-Click"
/// (compared case-insensitively; some senders add stray whitespace).
pub fn is_one_click_post(raw: &str) -> bool {
    raw.trim()
        .eq_ignore_ascii_case("List-Unsubscribe=One-Click")
}

fn parse_headers(msg: &mail_parser::Message) -> ParsedHeaders {
    let mut refs: Vec<String> = Vec::new();
    if let Some(irt) = msg.in_reply_to().as_text_list() {
        refs.extend(irt.iter().map(|s| s.to_string()));
    }
    if let Some(r) = msg.references().as_text_list() {
        for s in r {
            let s = s.to_string();
            if !refs.contains(&s) {
                refs.push(s);
            }
        }
    }

    let from = msg.from().and_then(|f| f.first()).and_then(addr_from);
    let via = resolve_via(msg, from.as_ref());
    let authentication_results = authentication_results(msg);
    let sender_verification = sender_verification(&authentication_results, from.as_ref());

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

    let is_automated = msg.header("List-Id").is_some()
        || msg.header("List-Unsubscribe").is_some()
        || msg
            .header("Precedence")
            .and_then(|h| h.as_text())
            .map(|v| {
                let v = v.to_ascii_lowercase();
                v == "bulk" || v == "list" || v == "junk"
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
        via,
        sender_verification,
    }
}

/// Parse a full raw RFC 5322 message.
pub fn parse_message(raw: &[u8]) -> Result<ParsedBody> {
    let msg = mail_parser::MessageParser::default()
        .parse(raw)
        .ok_or_else(|| CoreError::Mime("unparseable message".into()))?;

    let headers = parse_headers(&msg);

    let text = msg.body_text(0).map(|t| t.to_string());
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
    })
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
    bytes: Vec<u8>,
}

/// Replace `src="data:image/...;base64,..."` with `src="cid:..."` and return
/// the decoded images. Editors embed pasted screenshots as data URIs, but
/// many mail clients (Gmail included) strip those - CID parts survive.
fn extract_data_uri_images(html: &str) -> (String, Vec<InlineImage>) {
    use base64::Engine;
    const MARKER: &str = "src=\"data:image/";

    let mut out = String::with_capacity(html.len());
    let mut images = Vec::new();
    let mut rest = html;
    while let Some(pos) = rest.find(MARKER) {
        out.push_str(&rest[..pos]);
        let after = &rest[pos + 5..]; // past `src="`, starts at `data:image/`
        let Some(endq) = after.find('"') else {
            // unterminated attribute: keep the tail untouched
            out.push_str(&rest[pos..]);
            rest = "";
            break;
        };
        let uri = &after[..endq];
        let decoded = uri
            .strip_prefix("data:")
            .and_then(|u| u.split_once(";base64,"))
            .and_then(|(mime, b64)| {
                base64::engine::general_purpose::STANDARD
                    .decode(b64.trim())
                    .ok()
                    .map(|bytes| (mime.to_string(), bytes))
            });
        match decoded {
            Some((mime_type, bytes)) => {
                let cid = format!("img{}.{}", images.len() + 1, rand_token());
                out.push_str(&format!("src=\"cid:{cid}\""));
                images.push(InlineImage {
                    mime_type,
                    cid,
                    bytes,
                });
            }
            None => out.push_str(&rest[pos..pos + 5 + endq + 1]),
        }
        rest = &after[endq + 1..];
    }
    out.push_str(rest);
    (out, images)
}

/// Build a raw RFC 5322 message. Returns (message_id, raw_bytes).
pub fn build_message(out: &OutgoingMessage) -> Result<(String, Vec<u8>)> {
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

    let to_mb: Vec<(String, String)> = out
        .to
        .iter()
        .map(|a| (a.name.clone().unwrap_or_default(), a.email.clone()))
        .collect();
    let cc_mb: Vec<(String, String)> = out
        .cc
        .iter()
        .map(|a| (a.name.clone().unwrap_or_default(), a.email.clone()))
        .collect();
    let bcc_mb: Vec<(String, String)> = out
        .bcc
        .iter()
        .map(|a| (a.name.clone().unwrap_or_default(), a.email.clone()))
        .collect();

    let mut builder = mail_builder::MessageBuilder::new()
        .message_id(msg_id.trim_matches(['<', '>']).to_string())
        .from((
            out.from.name.clone().unwrap_or_default(),
            out.from.email.clone(),
        ))
        .subject(out.subject)
        .text_body(out.body_text);

    if let Some(html) = out.body_html
        && !html.trim().is_empty()
    {
        let (html, inline_images) = extract_data_uri_images(html);
        builder = builder.html_body(html);
        for img in inline_images {
            builder = builder.inline(img.mime_type, img.cid, img.bytes);
        }
    }

    if !to_mb.is_empty() {
        builder = builder.to(to_mb);
    }
    if !cc_mb.is_empty() {
        builder = builder.cc(cc_mb);
    }
    if !bcc_mb.is_empty() {
        builder = builder.bcc(bcc_mb);
    }
    for att in &out.attachments {
        builder = builder.attachment(
            att.mime_type.clone(),
            att.filename.clone(),
            att.bytes.clone(),
        );
    }
    if let Some(irt) = out.in_reply_to {
        builder = builder.in_reply_to(irt.trim_matches(['<', '>']).to_string());
    }
    if !out.references.is_empty() {
        let refs: Vec<String> = out
            .references
            .iter()
            .map(|r| r.trim_matches(['<', '>']).to_string())
            .collect();
        builder = builder.references(refs);
    }

    let raw = builder
        .write_to_vec()
        .map_err(|e| CoreError::Mime(e.to_string()))?;
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
    let who = from.name.clone().unwrap_or_else(|| from.email.clone());
    let quoted: String = original_text.lines().map(|l| format!("> {l}\n")).collect();
    format!("\n\nOn {when}, {who} <{}> wrote:\n{quoted}", from.email)
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

    fn outgoing<'a>(text: &'a str, html: Option<&'a str>) -> OutgoingMessage<'a> {
        OutgoingMessage {
            from: Address {
                name: Some("Me".into()),
                email: "me@example.com".into(),
            },
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

    #[test]
    fn text_only_message_stays_plain() {
        let (_, raw) = build_message(&outgoing("plain", None)).unwrap();
        let s = String::from_utf8_lossy(&raw);
        assert!(!s.contains("text/html"));
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
    fn data_uri_images_become_cid_inline_parts() {
        // 1x1 transparent PNG
        let png_b64 = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";
        let html = format!(r#"before <img src="data:image/png;base64,{png_b64}" alt="dot"> after"#);
        let (rewritten, images) = extract_data_uri_images(&html);
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
    fn malformed_data_uri_is_left_alone() {
        let html = r#"<img src="data:image/png;base64,@@notbase64@@">"#;
        let (rewritten, images) = extract_data_uri_images(html);
        assert!(images.is_empty());
        assert_eq!(rewritten, html);
    }
}
