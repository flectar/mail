//! Sending a message to an SMTP server that takes only 7-bit content.

use mail_parser::{MessageParser, PartType};

/// `raw` with each part whose body holds bytes beyond ASCII sent as base64,
/// for an SMTP server that takes only 7-bit content (no 8BITMIME, RFC 6152).
/// Every other byte stays as it is. None when nothing changes, or when the
/// message still isn't 7-bit after it: headers beyond ASCII need SMTPUTF8,
/// and an attached message can't be base64 (RFC 2046 section 5.2.1).
pub fn seven_bit(raw: &[u8]) -> Option<Vec<u8>> {
    let message = MessageParser::default().parse(raw)?;
    let mut parts = Vec::new();
    for part in &message.parts {
        if matches!(part.body, PartType::Multipart(_)) {
            continue;
        }
        let header = part.offset_header as usize..part.offset_body as usize;
        let body = part.offset_body as usize..part.offset_end as usize;
        if raw.get(body.clone())?.is_ascii() {
            continue;
        }
        if matches!(part.body, PartType::Message(_)) {
            return None;
        }
        parts.push((header, body));
    }
    if parts.is_empty() {
        return None;
    }
    parts.sort_by_key(|(header, _)| header.start);
    let mut out = Vec::with_capacity(raw.len() + raw.len() / 3);
    let mut copied = 0;
    for (header, body) in parts {
        // A part inside one already rewritten is in its body.
        if header.start < copied {
            return None;
        }
        out.extend_from_slice(&raw[copied..header.start]);
        out.extend_from_slice(&header_for_base64(&raw[header]));
        out.extend_from_slice(super::base64_lines(&raw[body.clone()]).as_bytes());
        copied = body.end;
    }
    out.extend_from_slice(&raw[copied..]);
    out.is_ascii().then_some(out)
}

/// A part's header block, blank line included, saying its body is base64:
/// any Content-Transfer-Encoding field goes, and one that says base64 comes
/// before the blank line.
fn header_for_base64(header: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(header.len() + 36);
    let mut dropping = false;
    let mut lines = header.split_inclusive(|&byte| byte == b'\n').peekable();
    while let Some(line) = lines.next() {
        if lines.peek().is_none() && matches!(line, b"\r\n" | b"\n") {
            out.extend_from_slice(b"Content-Transfer-Encoding: base64\r\n");
            out.extend_from_slice(line);
            return out;
        }
        let continuation = line
            .first()
            .is_some_and(|&byte| byte == b' ' || byte == b'\t');
        if !continuation {
            dropping = line
                .get(..26)
                .is_some_and(|name| name.eq_ignore_ascii_case(b"content-transfer-encoding:"));
        }
        if !dropping {
            out.extend_from_slice(line);
        }
    }
    // A header block without its blank line ends the message.
    out.extend_from_slice(b"Content-Transfer-Encoding: base64\r\n\r\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use mail_parser::MimeHeaders;

    fn parsed(raw: &[u8]) -> mail_parser::Message<'_> {
        MessageParser::default().parse(raw).unwrap()
    }

    #[test]
    fn sends_eight_bit_parts_as_base64() {
        let raw = "From: a@example.com\r\nTo: b@example.com\r\nSubject: Hi\r\nMIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"outer\"\r\n\r\n\
--outer\r\nContent-Type: multipart/alternative; boundary=\"inner\"\r\n\r\n\
--inner\r\nContent-Type: text/plain; charset=utf-8; format=flowed; delsp=no\r\n\
Content-Transfer-Encoding: 8bit\r\n\r\nGrüße aus \r\nBerlin\r\n\
--inner\r\nContent-Type: text/html; charset=utf-8\r\nContent-Transfer-Encoding: 7bit\r\n\r\n\
<p>Hi</p>\r\n--inner--\r\n\
--outer\r\nContent-Type: application/octet-stream\r\nContent-Disposition: attachment; filename=\"a.bin\"\r\n\
Content-Transfer-Encoding: base64\r\n\r\nAAEC\r\n--outer--\r\n";
        let seven = seven_bit(raw.as_bytes()).unwrap();
        assert!(seven.is_ascii());
        let before = parsed(raw.as_bytes());
        let after = parsed(&seven);
        assert_eq!(after.body_text(0), before.body_text(0));
        assert_eq!(after.body_text(0).as_deref(), Some("Grüße aus \r\nBerlin"));
        assert_eq!(after.body_html(0), before.body_html(0));
        assert_eq!(after.attachment(0).unwrap().contents(), [0, 1, 2]);
        let text = after.text_part(0).unwrap();
        assert_eq!(
            text.content_type().unwrap().attribute("format"),
            Some("flowed")
        );
        let text_header = &seven[text.offset_header as usize..text.offset_body as usize];
        let text_header = String::from_utf8_lossy(text_header);
        assert!(text_header.contains("Content-Transfer-Encoding: base64\r\n\r\n"));
        assert!(!text_header.contains("8bit"));
        // The HTML part and the attachment come through as they were.
        let html = String::from_utf8_lossy(&seven);
        assert!(html.contains("Content-Transfer-Encoding: 7bit\r\n\r\n<p>Hi</p>\r\n--inner--"));
        assert!(html.contains("Content-Transfer-Encoding: base64\r\n\r\nAAEC\r\n--outer--"));
    }

    #[test]
    fn rewrites_a_single_part_message() {
        let raw = "From: a@example.com\r\nSubject: Hi\r\nContent-Type: text/plain; charset=utf-8\r\n\
Content-Transfer-Encoding: 8bit\r\nMIME-Version: 1.0\r\n\r\nCafé\r\n";
        let seven = seven_bit(raw.as_bytes()).unwrap();
        assert!(seven.is_ascii());
        let after = parsed(&seven);
        assert_eq!(after.body_text(0).as_deref(), Some("Café\r\n"));
        assert_eq!(after.subject(), Some("Hi"));
        // A part without the field gets one.
        let raw = "Subject: Hi\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nCafé";
        let seven = seven_bit(raw.as_bytes()).unwrap();
        assert_eq!(parsed(&seven).body_text(0).as_deref(), Some("Café"));
    }

    #[test]
    fn leaves_what_it_cannot_make_seven_bit() {
        // Already 7-bit.
        assert!(seven_bit(b"Subject: Hi\r\n\r\nHello\r\n").is_none());
        // A header beyond ASCII needs SMTPUTF8 instead.
        assert!(seven_bit("Subject: Grüße\r\n\r\nCafé\r\n".as_bytes()).is_none());
        // An attached message must stay 7bit or 8bit.
        let raw = "Subject: Fwd\r\nContent-Type: multipart/mixed; boundary=b\r\n\r\n--b\r\n\
Content-Type: message/rfc822\r\n\r\nSubject: Inner\r\nContent-Type: text/plain; charset=utf-8\r\n\
Content-Transfer-Encoding: 8bit\r\n\r\nCafé\r\n--b--\r\n";
        assert!(seven_bit(raw.as_bytes()).is_none());
    }
}
