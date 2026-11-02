//! Small, defensive vCard 3.0/4.0 projection for the fields the contact UI owns.

use std::borrow::Cow;

use crate::error::Result;
use crate::models::{ContactRecord, ContactValue};
use base64::Engine;
use sha2::{Digest, Sha256};

pub const MAX_VCARD_BYTES: usize = 2 * 1024 * 1024;

pub fn ensure_size(input: &str) -> Result<()> {
    if input.len() > MAX_VCARD_BYTES {
        return Err(super::err("vCard exceeds the 2 MiB safety limit"));
    }
    Ok(())
}

/// Sending requires one mailbox; importing a contact does not.
pub fn usable_email(value: &str) -> bool {
    !value.is_empty()
        && !value.chars().any(char::is_whitespace)
        && value.parse::<lettre::Address>().is_ok()
}

fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('n' | 'N') => out.push('\n'),
                Some(ch) => out.push(ch),
                None => out.push('\\'),
            }
        } else {
            out.push(ch);
        }
    }
    out
}

fn escape(value: &str) -> String {
    let capacity = value
        .chars()
        .map(|ch| match ch {
            '\\' | '\n' | ';' | ',' => 2,
            _ => ch.len_utf8(),
        })
        .sum();
    let mut escaped = String::with_capacity(capacity);
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => {}
            ';' => escaped.push_str("\\;"),
            ',' => escaped.push_str("\\,"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

/// CATEGORIES is a list; the editor shows it as comma-separated text.
fn categories(text: &str) -> String {
    text.split(',')
        .map(str::trim)
        .filter(|category| !category.is_empty())
        .map(escape)
        .collect::<Vec<_>>()
        .join(",")
}

/// N for a display name the app does not split into its parts. Clients that
/// show N rather than FN then show the same name.
fn structured_name(name: &str) -> String {
    format!(";{};;;", escape(name))
}

fn split_escaped(value: &str, delimiter: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut escaped = false;
    let mut start = 0;
    for (byte, ch) in value.char_indices() {
        if escaped {
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == delimiter {
            parts.push(&value[start..byte]);
            start = byte + ch.len_utf8();
        }
    }
    parts.push(&value[start..]);
    parts
}

fn folded_len(line: &str) -> usize {
    let mut size = line.len() + 2;
    let mut column = 0;
    for ch in line.chars() {
        if column + ch.len_utf8() > 75 {
            size += 3; // CRLF and continuation space
            column = 1;
        }
        column += ch.len_utf8();
    }
    size
}

fn append_folded_line(output: &mut String, line: &str) {
    let mut column = 0;
    for ch in line.chars() {
        if column + ch.len_utf8() > 75 {
            output.push_str("\r\n ");
            column = 1;
        }
        output.push(ch);
        column += ch.len_utf8();
    }
    output.push_str("\r\n");
}

fn physical_lines(input: &str) -> impl Iterator<Item = &str> {
    let mut rest = Some(input);
    std::iter::from_fn(move || {
        let current = rest.take()?;
        if let Some(end) = current.find(['\r', '\n']) {
            let newline = current.as_bytes()[end];
            let tail = &current[end + 1..];
            rest = Some(if newline == b'\r' {
                tail.strip_prefix('\n').unwrap_or(tail)
            } else {
                tail
            });
            Some(&current[..end])
        } else {
            Some(current)
        }
    })
}

fn unfold(input: &str) -> Vec<Cow<'_, str>> {
    let mut lines: Vec<Cow<'_, str>> = Vec::new();
    for line in physical_lines(input) {
        if (line.starts_with(' ') || line.starts_with('\t')) && !lines.is_empty() {
            lines.last_mut().unwrap().to_mut().push_str(&line[1..]);
        } else {
            lines.push(Cow::Borrowed(line));
        }
    }
    let mut joined = Vec::<Cow<'_, str>>::new();
    for line in lines {
        let continues_quoted_printable = joined.last().is_some_and(|previous| {
            previous.ends_with('=')
                && previous
                    .split_once(':')
                    .is_some_and(|(head, _)| contains_ascii_case(head, "QUOTED-PRINTABLE"))
        });
        if continues_quoted_printable {
            joined.last_mut().unwrap().to_mut().pop();
            joined.last_mut().unwrap().to_mut().push_str(&line);
        } else {
            joined.push(line);
        }
    }
    joined
}

fn contains_ascii_case(value: &str, needle: &str) -> bool {
    value
        .as_bytes()
        .windows(needle.len())
        .any(|candidate| candidate.eq_ignore_ascii_case(needle.as_bytes()))
}

fn decoded_value<'a>(head: &str, raw: &'a str) -> Cow<'a, str> {
    if !contains_ascii_case(head, "QUOTED-PRINTABLE") {
        return Cow::Borrowed(raw);
    }
    let bytes = raw.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'='
            && index + 2 < bytes.len()
            && let (Some(high), Some(low)) = (
                (bytes[index + 1] as char).to_digit(16),
                (bytes[index + 2] as char).to_digit(16),
            )
        {
            decoded.push(((high << 4) | low) as u8);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    let charset = head.split(';').find_map(|parameter| {
        let (name, value) = parameter.split_once('=')?;
        name.eq_ignore_ascii_case("CHARSET")
            .then(|| value.trim_matches('"'))
    });
    if let Some(encoding) =
        charset.and_then(|label| encoding_rs::Encoding::for_label(label.as_bytes()))
    {
        Cow::Owned(encoding.decode(&decoded).0.into_owned())
    } else {
        Cow::Owned(
            String::from_utf8(decoded)
                .unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned()),
        )
    }
}

fn property_name(line: &str) -> Option<&str> {
    let (head, _) = line.split_once(':')?;
    Some(
        head.split(';')
            .next()
            .unwrap_or(head)
            .rsplit('.')
            .next()
            .unwrap_or(head),
    )
}

fn render(lines: Vec<Cow<'_, str>>) -> String {
    if lines.is_empty() {
        return "\r\n".into();
    }
    let mut output = String::with_capacity(lines.iter().map(|line| folded_len(line)).sum());
    for line in lines {
        append_folded_line(&mut output, &line);
    }
    output
}

fn value_head(name: &str, value: &ContactValue, version3: bool) -> String {
    let mut head = name.to_owned();
    let mut label = value.label.clone();
    if version3
        && value.preference == 1
        && !label.split(',').any(|p| p.eq_ignore_ascii_case("PREF"))
    {
        if !label.is_empty() {
            label.push(',');
        }
        label.push_str("PREF");
    }
    if !label.is_empty() {
        head.push_str(&format!(";TYPE={label}"));
    }
    if !version3 && value.preference < 100 {
        head.push_str(&format!(";PREF={}", value.preference));
    }
    head
}

fn serialized_lines(record: &ContactRecord, uid: &str, version: &str) -> Vec<Cow<'static, str>> {
    let display_name = [&record.name, &record.email, &record.phone]
        .into_iter()
        .find(|value| !value.trim().is_empty())
        .map_or("", |value| value.trim());
    let mut lines = vec![
        "BEGIN:VCARD".into(),
        format!("VERSION:{version}").into(),
        format!("UID:{}", escape(uid)).into(),
        format!("FN:{}", escape(display_name)).into(),
        // N is mandatory in vCard 3.0.
        format!("N:{}", structured_name(display_name)).into(),
    ];
    for (name, values) in [
        ("EMAIL", &record.details.emails),
        ("TEL", &record.details.phones),
    ] {
        for value in values.iter().filter(|value| !value.value.trim().is_empty()) {
            lines.push(
                format!(
                    "{}:{}",
                    value_head(name, value, version == "3.0"),
                    escape(value.value.trim())
                )
                .into(),
            );
        }
    }
    for (name, value) in [
        ("ORG", &record.company),
        ("TITLE", &record.job_title),
        ("URL", &record.website),
        ("BDAY", &record.birthday),
        ("NOTE", &record.notes),
    ] {
        if !value.trim().is_empty() {
            lines.push(format!("{name}:{}", escape(value.trim())).into());
        }
    }
    let tags = categories(&record.tags);
    if !tags.is_empty() {
        lines.push(format!("CATEGORIES:{tags}").into());
    }
    if !record.postal_address.trim().is_empty() {
        lines.push(format!("ADR:;;{};;;;", escape(record.postal_address.trim())).into());
    }
    lines.push("END:VCARD".into());
    lines
}

pub fn parse(input: &str) -> Result<ContactRecord> {
    ensure_size(input)?;
    let lines = unfold(input);
    if lines
        .iter()
        .filter(|line| line.eq_ignore_ascii_case("BEGIN:VCARD"))
        .count()
        != 1
        || lines
            .iter()
            .filter(|line| line.eq_ignore_ascii_case("END:VCARD"))
            .count()
            != 1
    {
        return Err(super::err("resource is not a vCard"));
    }
    let version = lines.iter().find_map(|line| {
        let (head, value) = line.split_once(':')?;
        head.eq_ignore_ascii_case("VERSION").then(|| value.trim())
    });
    if !matches!(version, Some("3.0" | "4.0")) {
        return Err(super::err("vCard has no supported VERSION"));
    }
    let has_uid = lines.iter().any(|line| {
        line.split_once(':').is_some_and(|(head, value)| {
            head.split(';')
                .next()
                .is_some_and(|name| name.eq_ignore_ascii_case("UID"))
                && !value.trim().is_empty()
        })
    });
    let has_fn = lines.iter().any(|line| {
        line.split_once(':').is_some_and(|(head, value)| {
            head.split(';')
                .next()
                .is_some_and(|name| name.eq_ignore_ascii_case("FN"))
                && !value.trim().is_empty()
        })
    });
    if !has_uid || !has_fn {
        return Err(super::err("vCard is missing its required UID or FN"));
    }
    let mut record = ContactRecord {
        details: Default::default(),
        id: 0,
        name: String::new(),
        email: String::new(),
        phone: String::new(),
        company: String::new(),
        job_title: String::new(),
        website: String::new(),
        birthday: String::new(),
        postal_address: String::new(),
        notes: String::new(),
        tags: String::new(),
        is_favorite: false,
        interactions: 0,
        last_interacted: None,
        account_ids: Vec::new(),
        is_managed: true,
    };
    for line in lines {
        let Some((head, raw)) = line.split_once(':') else {
            continue;
        };
        let name = head
            .split(';')
            .next()
            .unwrap_or(head)
            .rsplit('.')
            .next()
            .unwrap_or(head);
        let Some(name) = [
            "FN",
            "N",
            "ORG",
            "TITLE",
            "URL",
            "BDAY",
            "ADR",
            "NOTE",
            "CATEGORIES",
        ]
        .into_iter()
        .find(|candidate| name.eq_ignore_ascii_case(candidate)) else {
            continue;
        };
        if (name == "N" && !record.name.is_empty()) || (name == "URL" && !record.website.is_empty())
        {
            continue;
        }
        let decoded = decoded_value(head, raw);
        let value = if matches!(name, "N" | "ORG" | "ADR") {
            String::new()
        } else {
            unescape(&decoded).trim().to_owned()
        };
        match name {
            "FN" => record.name = value,
            "N" if record.name.is_empty() => {
                let parts = split_escaped(decoded.trim(), ';');
                for part in [
                    parts.get(1).copied().unwrap_or(""),
                    parts.first().copied().unwrap_or(""),
                ]
                .into_iter()
                .filter(|value| !value.is_empty())
                {
                    if !record.name.is_empty() {
                        record.name.push(' ');
                    }
                    record.name.push_str(&unescape(part));
                }
            }
            "ORG" => {
                record.company = split_escaped(decoded.trim(), ';')
                    .first()
                    .map(|value| unescape(value).trim().to_owned())
                    .unwrap_or_default()
            }
            "TITLE" => record.job_title = value,
            "URL" if record.website.is_empty() => record.website = value,
            "BDAY" => record.birthday = value,
            "ADR" => {
                record.postal_address.clear();
                for part in split_escaped(decoded.trim(), ';') {
                    let unescaped = unescape(part);
                    let value = unescaped.trim();
                    if !value.is_empty() {
                        if !record.postal_address.is_empty() {
                            record.postal_address.push_str(", ");
                        }
                        record.postal_address.push_str(value);
                    }
                }
            }
            "NOTE" => record.notes = value,
            "CATEGORIES" => {
                record.tags = split_escaped(decoded.trim(), ',')
                    .into_iter()
                    .map(|category| unescape(category).trim().to_owned())
                    .filter(|category| !category.is_empty())
                    .collect::<Vec<_>>()
                    .join(", ")
            }
            _ => {}
        }
    }
    record.details.emails = values(input, "EMAIL");
    record.details.phones = values(input, "TEL");
    record.refresh_primary_values();
    if record.name.is_empty() {
        record.name = record.email.clone();
    }
    Ok(record)
}

/// A vCard document and the record parsed from it. The record's value keys
/// refer to lines of this exact document.
#[derive(Debug, Clone)]
pub struct Card {
    document: String,
    record: ContactRecord,
}

impl Card {
    pub fn parse(document: impl Into<String>) -> Result<Self> {
        let document = document.into();
        let record = parse(&document)?;
        Ok(Self { document, record })
    }

    pub fn document(&self) -> &str {
        &self.document
    }

    pub fn record(&self) -> &ContactRecord {
        &self.record
    }
}

pub fn serialize(record: &ContactRecord, uid: &str) -> String {
    // vCard 3.0 is CardDAV's mandatory baseline. A collection may advertise
    // v4 support, but new objects must remain interoperable when it does not.
    render(serialized_lines(record, uid, "3.0"))
}

pub fn new_uid() -> String {
    let mut bytes = rand::random::<[u8; 16]>();
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "urn:uuid:{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    )
}

/// The document's UID, which also names a new resource on the server.
pub fn uid(input: &str) -> Option<String> {
    unfold(input).iter().find_map(|line| {
        let (head, value) = line.split_once(':')?;
        (property_name(line)?.eq_ignore_ascii_case("UID") && !head.contains(';'))
            .then(|| unescape(value.trim()))
            .filter(|uid| !uid.is_empty())
    })
}

/// Whether two documents contain the same properties in the same order,
/// ignoring line folding.
pub fn equivalent(a: &str, b: &str) -> bool {
    unfold(a)
        .into_iter()
        .filter(|l| !l.is_empty())
        .eq(unfold(b).into_iter().filter(|l| !l.is_empty()))
}

/// Every non-empty occurrence of a repeated property, keyed by its line.
pub fn values(input: &str, name: &str) -> Vec<ContactValue> {
    unfold(input)
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            if !property_name(line)?.eq_ignore_ascii_case(name) {
                return None;
            }
            let (head, raw) = line.split_once(':')?;
            let value = unescape(&decoded_value(head, raw)).trim().to_owned();
            if value.is_empty() {
                return None;
            }
            // vCard 3.0 writers may repeat TYPE, e.g. TYPE=WORK;TYPE=PREF.
            let label = head
                .split(';')
                .filter_map(|p| p.split_once('='))
                .filter(|(key, _)| key.eq_ignore_ascii_case("TYPE"))
                .map(|(_, value)| value.trim_matches('"'))
                .collect::<Vec<_>>()
                .join(",");
            let preference = head
                .split(';')
                .filter_map(|p| p.split_once('='))
                .find(|(key, _)| key.eq_ignore_ascii_case("PREF"))
                .and_then(|(_, value)| value.parse().ok())
                .unwrap_or(
                    if label.split(',').any(|t| t.eq_ignore_ascii_case("PREF")) {
                        1
                    } else {
                        100
                    },
                );
            Some(ContactValue {
                key: index.to_string(),
                value,
                label: label
                    .split(',')
                    .filter(|p| !p.eq_ignore_ascii_case("PREF"))
                    .collect::<Vec<_>>()
                    .join(","),
                preference,
            })
        })
        .collect()
}

/// Content hash identifying an embedded photo in storage.
pub fn photo_hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Decode embedded photographs only. URI properties remain in the document.
/// A bad/oversized photo does not invalidate the contact.
pub fn photo(input: &str) -> Option<Vec<u8>> {
    const MAX_PHOTO_BYTES: usize = 1024 * 1024;
    for line in unfold(input) {
        if !property_name(&line).is_some_and(|name| name.eq_ignore_ascii_case("PHOTO")) {
            continue;
        }
        let (head, raw) = line.split_once(':')?;
        let encoded = if raw
            .get(..5)
            .is_some_and(|p| p.eq_ignore_ascii_case("data:"))
        {
            let (metadata, data) = raw.split_once(',')?;
            if !metadata.to_ascii_lowercase().starts_with("data:image/")
                || !metadata.to_ascii_lowercase().ends_with(";base64")
            {
                continue;
            }
            data
        } else if head.split(';').any(|p| {
            p.eq_ignore_ascii_case("ENCODING=b") || p.eq_ignore_ascii_case("ENCODING=BASE64")
        }) {
            raw
        } else {
            continue;
        };
        if encoded.len() > MAX_PHOTO_BYTES * 4 / 3 + 4 {
            continue;
        }
        if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(encoded)
            && bytes.len() <= MAX_PHOTO_BYTES
        {
            return Some(bytes);
        }
    }
    None
}

/// Change only edited properties. Untouched structured values, parameters,
/// extension properties, and photos survive the round trip.
pub fn update(existing: &str, record: &ContactRecord) -> Result<String> {
    let before = parse(existing)?;
    let mut lines: Vec<String> = unfold(existing)
        .into_iter()
        .map(|s| s.into_owned())
        .collect();
    let version3 = lines.iter().any(|l| l.eq_ignore_ascii_case("VERSION:3.0"));
    // Work with original occurrence indexes until every replacement is applied.
    let mut replacements = std::collections::HashMap::<usize, Option<String>>::new();
    let mut additions = Vec::new();
    for (name, old, new) in [
        ("FN", &before.name, &record.name),
        ("ORG", &before.company, &record.company),
        ("TITLE", &before.job_title, &record.job_title),
        ("URL", &before.website, &record.website),
        ("BDAY", &before.birthday, &record.birthday),
        ("ADR", &before.postal_address, &record.postal_address),
        ("NOTE", &before.notes, &record.notes),
        ("CATEGORIES", &before.tags, &record.tags),
    ] {
        if old == new {
            continue;
        }
        let candidates: Vec<_> = lines
            .iter()
            .enumerate()
            .filter(|(_, line)| property_name(line).is_some_and(|n| n.eq_ignore_ascii_case(name)))
            .collect();
        let selected = if name == "URL" {
            candidates.first().copied()
        } else {
            candidates.last().copied()
        };
        let value = if name == "ADR" {
            format!(";;{};;;;", escape(new))
        } else if name == "CATEGORIES" {
            categories(new)
        } else if name == "ORG" {
            let tail = selected
                .and_then(|(_, line)| line.split_once(':'))
                .map(|(_, v)| {
                    split_escaped(v, ';')
                        .into_iter()
                        .skip(1)
                        .collect::<Vec<_>>()
                        .join(";")
                });
            match tail.filter(|s| !s.is_empty()) {
                Some(tail) => format!("{};{tail}", escape(new)),
                None => escape(new),
            }
        } else {
            escape(new)
        };
        if let Some((index, line)) = selected {
            let head = line.split_once(':').unwrap().0;
            // Values are now UTF-8 text rather than a legacy encoded value.
            let head = head
                .split(';')
                .filter(|p| {
                    !contains_ascii_case(p, "ENCODING=") && !contains_ascii_case(p, "CHARSET=")
                })
                .collect::<Vec<_>>()
                .join(";");
            replacements.insert(
                index,
                (!new.is_empty() || name == "FN").then(|| format!("{head}:{value}")),
            );
        } else if !new.is_empty() {
            additions.push(format!("{name}:{value}"));
        }
    }
    for (name, old, new) in [
        ("EMAIL", &before.details.emails, &record.details.emails),
        ("TEL", &before.details.phones, &record.details.phones),
    ] {
        if old == new {
            continue;
        }
        for previous in old {
            let Ok(index) = previous.key.parse::<usize>() else {
                continue;
            };
            if let Some(next) = new.iter().find(|v| v.key == previous.key) {
                if next != previous {
                    let head = lines[index].split_once(':').unwrap().0;
                    let mut parts = head.split(';');
                    let property = parts.next().unwrap();
                    let extra = parts
                        .filter(|p| {
                            !["TYPE=", "PREF=", "CHARSET=", "ENCODING="]
                                .iter()
                                .any(|key| p.to_ascii_uppercase().starts_with(key))
                        })
                        .collect::<Vec<_>>();
                    let mut head = value_head(property, next, version3);
                    for p in extra {
                        head.push(';');
                        head.push_str(p);
                    }
                    replacements.insert(
                        index,
                        (!next.value.is_empty()).then(|| format!("{head}:{}", escape(&next.value))),
                    );
                }
            } else {
                replacements.insert(index, None);
            }
        }
        for next in new.iter().filter(|v| v.key.is_empty()) {
            if !next.value.is_empty() {
                additions.push(format!(
                    "{}:{}",
                    value_head(name, next, version3),
                    escape(&next.value)
                ));
            }
        }
    }
    if before.name != record.name
        && let Some((index, line)) = lines
            .iter()
            .enumerate()
            .find(|(_, line)| property_name(line).is_some_and(|n| n.eq_ignore_ascii_case("N")))
    {
        let head = line.split_once(':').map_or("N", |(head, _)| head);
        let head = head
            .split(';')
            .filter(|p| !contains_ascii_case(p, "ENCODING=") && !contains_ascii_case(p, "CHARSET="))
            .collect::<Vec<_>>()
            .join(";");
        replacements.insert(
            index,
            Some(format!("{head}:{}", structured_name(&record.name))),
        );
    }
    let mut output = Vec::new();
    for (index, line) in lines.drain(..).enumerate() {
        // Rendering ends every line itself; a trailing empty line would add a
        // blank line on each edit.
        if line.is_empty() {
            continue;
        }
        if line.eq_ignore_ascii_case("END:VCARD") {
            output.extend(additions.drain(..).map(Cow::Owned));
        }
        match replacements.remove(&index) {
            Some(Some(line)) => output.push(Cow::Owned(line)),
            Some(None) => {}
            None => output.push(Cow::Owned(line)),
        }
    }
    Ok(render(output))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_folded_vcard_and_prefers_preferred_email() {
        let card = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:ada\r\nFN:Ada \r\n Lovelace\r\nEMAIL;TYPE=WORK:other@example.test\r\nEMAIL;PREF=1:ADA@example.test\r\nORG:Analytical Engines;Research\r\nADR:;;12 St James Sq;London;;;UK\r\nNOTE:Line one\\nLine two\r\nEND:VCARD\r\n";
        let parsed = parse(card).unwrap();
        assert_eq!(parsed.name, "Ada Lovelace");
        assert_eq!(parsed.email, "ada@example.test");
        assert_eq!(parsed.company, "Analytical Engines");
        assert!(parsed.postal_address.contains("London"));
    }

    #[test]
    fn serialized_card_round_trips_supported_fields() {
        let mut record =
            parse("BEGIN:VCARD\nVERSION:4.0\nUID:alice\nFN:Alice\nEMAIL:a@example.test\nEND:VCARD")
                .unwrap();
        record.notes = "one, two\nthree".into();
        let reparsed = parse(&serialize(&record, "contact-1@example.test")).unwrap();
        assert!(serialize(&record, "contact-1@example.test").contains("VERSION:3.0"));
        assert_eq!(reparsed.email, record.email);
        assert_eq!(reparsed.notes, record.notes);
    }

    #[test]
    fn structured_escapes_and_long_utf8_lines_round_trip() {
        let mut record =
            parse("BEGIN:VCARD\nVERSION:4.0\nUID:alice\nFN:Alice\nEMAIL:a@example.test\nEND:VCARD")
                .unwrap();
        record.company = "Research; Development".into();
        record.notes = "Résumé notes, ".repeat(12);
        let encoded = serialize(&record, "contact-2@example.test");
        assert!(encoded.contains("\r\n "));
        let reparsed = parse(&encoded).unwrap();
        assert_eq!(reparsed.company, record.company);
        assert_eq!(reparsed.notes, record.notes.trim());
    }

    #[test]
    fn update_preserves_unmodeled_and_additional_properties() {
        let original = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:server-id\r\nFN:Alice\r\nEMAIL;TYPE=PREF:old@example.test\r\nEMAIL;TYPE=HOME:other@example.test\r\nTEL;TYPE=CELL:111\r\nTEL;TYPE=HOME:222\r\nPHOTO;ENCODING=b:AAAB\r\nX-AB-LABEL:Friend\r\nEND:VCARD\r\n";
        let mut record = parse(original).unwrap();
        record.details.emails[0].value = "new@example.test".into();
        record.details.phones[0].value = "333".into();
        let encoded = update(original, &record).unwrap();
        assert!(encoded.contains("VERSION:3.0"));
        assert!(encoded.contains("UID:server-id"));
        assert!(encoded.contains("EMAIL;TYPE=PREF:new@example.test"));
        assert!(encoded.contains("EMAIL;TYPE=HOME:other@example.test"));
        assert!(encoded.contains("TEL;TYPE=CELL:333"));
        assert!(encoded.contains("TEL;TYPE=HOME:222"));
        assert!(encoded.contains("PHOTO;ENCODING=b:AAAB"));
        assert!(encoded.contains("X-AB-LABEL:Friend"));
    }

    #[test]
    fn repeated_type_parameters_keep_every_label_and_preference() {
        let card = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:p\r\nFN:P\r\nEMAIL;TYPE=INTERNET;TYPE=WORK;TYPE=PREF:work@example.test\r\nEMAIL;TYPE=HOME:home@example.test\r\nEND:VCARD\r\n";
        let mut record = parse(card).unwrap();
        assert_eq!(record.email, "work@example.test");
        assert_eq!(record.details.emails[0].label, "INTERNET,WORK");
        assert_eq!(record.details.emails[0].preference, 1);
        record.details.emails[0].value = "new@example.test".into();
        let updated = update(card, &record).unwrap();
        assert!(updated.contains("EMAIL;TYPE=INTERNET,WORK,PREF:new@example.test\r\n"));
        assert!(updated.contains("EMAIL;TYPE=HOME:home@example.test\r\n"));
    }

    #[test]
    fn renames_categories_and_repeated_edits_keep_the_document_clean() {
        let card = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:p\r\nFN:Ada Lovelace\r\nN:Lovelace;Ada;;;\r\nCATEGORIES:Friends,Work\r\nEND:VCARD\r\n";
        let mut record = parse(card).unwrap();
        assert_eq!(record.tags, "Friends, Work");
        record.notes = "Edited".into();
        let edited = update(card, &record).unwrap();
        assert!(edited.contains("N:Lovelace;Ada;;;\r\nCATEGORIES:Friends,Work\r\n"));
        assert!(edited.ends_with("END:VCARD\r\n"));
        assert!(update(&edited, &parse(&edited).unwrap()).unwrap() == edited);
        record.name = "Ada King".into();
        record.tags = "Friends, Family".into();
        let renamed = update(card, &record).unwrap();
        assert!(renamed.contains("FN:Ada King\r\nN:;Ada King;;;\r\n"));
        assert!(renamed.contains("CATEGORIES:Friends,Family\r\n"));
        assert!(serialize(&record, "new").contains("N:;Ada King;;;\r\n"));
    }

    #[test]
    fn uid_is_read_from_the_unfolded_document() {
        let card =
            "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:urn:uuid:0123-\r\n 4567\r\nFN:A\r\nEND:VCARD\r\n";
        assert_eq!(uid(card).as_deref(), Some("urn:uuid:0123-4567"));
        assert_eq!(uid("BEGIN:VCARD\r\nFN:A\r\nEND:VCARD\r\n"), None);
    }

    #[test]
    fn generated_uid_is_an_rfc_4122_version_4_urn() {
        let uid = new_uid();
        assert_eq!(uid.len(), 45);
        assert!(uid.starts_with("urn:uuid:"));
        assert_eq!(&uid[23..24], "4");
        assert!(matches!(&uid[28..29], "8" | "9" | "a" | "b"));
    }

    #[test]
    fn parses_vcard3_quoted_printable_and_escaped_structured_name() {
        let card = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:andre\r\nFN;CHARSET=ISO-8859-1;ENCODING=QUOTED-PRINTABLE:Andr=E9 Doe\\;Sr.\r\nN;CHARSET=ISO-8859-1;ENCODING=QUOTED-PRINTABLE:Doe\\;Sr.;Andr=E9\r\nEMAIL:andre@example.test\r\nNOTE;ENCODING=QUOTED-PRINTABLE:Line=20one=\r\n=0ALine=20two\r\nEND:VCARD\r\n";
        let parsed = parse(card).unwrap();
        assert_eq!(parsed.name, "André Doe;Sr.");
        assert_eq!(parsed.notes, "Line one\nLine two");
    }

    #[test]
    fn rejects_multiple_cards_and_missing_required_properties() {
        assert!(parse("BEGIN:VCARD\nVERSION:3.0\nFN:A\nEMAIL:a@example.test\nEND:VCARD").is_err());
        assert!(
            parse("BEGIN:VCARD\nVERSION:2.1\nUID:a\nFN:A\nEMAIL:a@example.test\nEND:VCARD")
                .is_err()
        );
        assert!(parse("BEGIN:VCARD\nVERSION:3.0\nUID:a\nFN:A\nEMAIL:a@example.test\nEND:VCARD\nBEGIN:VCARD\nVERSION:3.0\nUID:b\nFN:B\nEMAIL:b@example.test\nEND:VCARD").is_err());
    }
    #[test]
    fn phone_only_photo_and_structured_fields_survive_email_edit() {
        for photo in [
            "PHOTO;ENCODING=b;TYPE=PNG:AAAB",
            "PHOTO:data:image/png;base64,AAAB",
        ] {
            let original = format!(
                "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:person\r\nFN:Person\r\nN:Family;Given;Middle;Dr.;Jr.\r\nORG:Company;Department\r\nADR;TYPE=HOME:;;Street;City;Region;123;Country\r\nTEL;TYPE=CELL:123\r\n{photo}\r\nX-CUSTOM:Preserve me\r\nEND:VCARD\r\n"
            );
            let mut record = parse(&original).unwrap();
            assert_eq!(super::photo(&original), Some(vec![0, 0, 1]));
            record.details.emails.push(ContactValue {
                value: "person@example.test".into(),
                preference: 1,
                ..Default::default()
            });
            let changed = update(&original, &record).unwrap();
            for line in physical_lines(&original).filter(|line| !line.is_empty()) {
                assert!(unfold(&changed).iter().any(|v| v == line), "Lost {line}");
            }
            assert_eq!(parse(&changed).unwrap().email, "person@example.test");
            record = parse(&changed).unwrap();
            record.details.emails.clear();
            assert!(
                parse(&update(&changed, &record).unwrap())
                    .unwrap()
                    .email
                    .is_empty()
            );
        }
    }

    #[test]
    fn malformed_and_remote_photos_do_not_hide_people() {
        for value in [
            "PHOTO;ENCODING=b:invalid!",
            "PHOTO:https://photos.example/person.png",
            "PHOTO:data:image/png;base64,invalid!",
        ] {
            let doc = format!(
                "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:p\r\nFN:Person\r\n{value}\r\nEND:VCARD\r\n"
            );
            assert!(photo(&doc).is_none());
            let mut record = parse(&doc).unwrap();
            record.notes = "Updated".into();
            assert!(update(&doc, &record).unwrap().contains(value));
        }
    }

    #[test]
    fn sendability_is_independent_of_contact_import() {
        for value in [
            "missing-at",
            "a@",
            "a@example.test,b@example.test",
            "a@example.test\r\nBcc:b@example.test",
        ] {
            assert!(!usable_email(value));
        }
        assert!(usable_email("person@example.test"));
        assert!(
            parse("BEGIN:VCARD\nVERSION:4.0\nUID:p\nFN:Person\nEMAIL:missing-at\nEND:VCARD")
                .is_ok()
        );
    }
    #[test]
    fn selecting_a_preferred_email_updates_vcard3_parameters() {
        let doc = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:p\r\nFN:Person\r\nEMAIL;TYPE=HOME,PREF:first@example.test\r\nEMAIL;TYPE=WORK:second@example.test\r\nEND:VCARD\r\n";
        let mut record = parse(doc).unwrap();
        record.details.emails[0].preference = 100;
        record.details.emails[1].preference = 1;
        let updated = update(doc, &record).unwrap();
        assert!(updated.contains("EMAIL;TYPE=HOME:first@example.test"));
        assert!(updated.contains("EMAIL;TYPE=WORK,PREF:second@example.test"));
        assert!(!updated.contains(";PREF="));
        assert_eq!(parse(&updated).unwrap().email, "second@example.test");
    }
}
