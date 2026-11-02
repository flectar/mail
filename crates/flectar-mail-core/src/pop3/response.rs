//! Parsing of POP3 response lines.

use super::{Error, Result};

/// A response code (RFC 2449 section 8, RFC 3206) at the start of a status
/// line's text, such as `-ERR [IN-USE] Mailbox locked`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResponseCode {
    /// The credentials were rejected.
    Auth,
    /// Sign-in succeeded, but another session holds the maildrop.
    InUse,
    /// The user signed in too recently and must wait.
    LoginDelay,
    /// A temporary server problem.
    SysTemp,
    /// A permanent server problem unrelated to the credentials.
    SysPerm,
    /// The command needs UTF8 mode.
    Utf8,
    Other(String),
}

impl ResponseCode {
    fn parse(code: &str) -> Self {
        match code.to_ascii_uppercase().as_str() {
            "AUTH" => Self::Auth,
            "IN-USE" => Self::InUse,
            "LOGIN-DELAY" => Self::LoginDelay,
            "SYS/TEMP" => Self::SysTemp,
            "SYS/PERM" => Self::SysPerm,
            "UTF8" => Self::Utf8,
            _ => Self::Other(code.to_owned()),
        }
    }
}

pub(super) enum Status {
    Ok(String),
    Err(Option<ResponseCode>, String),
}

/// `+OK text` or `-ERR [CODE] text`. Status indicators are case-sensitive
/// in RFC 1939, but some servers answer `+ok`; both spellings are accepted.
pub(super) fn parse_status(line: &[u8]) -> Option<Status> {
    let text = |rest: &[u8]| String::from_utf8_lossy(rest).trim().to_owned();
    if let Some(rest) = strip_indicator(line, b"+OK") {
        return Some(Status::Ok(text(rest)));
    }
    let rest = strip_indicator(line, b"-ERR")?;
    let rest = text(rest);
    if let Some(bracketed) = rest.strip_prefix('[')
        && let Some((code, message)) = bracketed.split_once(']')
        && !code.is_empty()
        && !code.contains(char::is_whitespace)
    {
        return Some(Status::Err(
            Some(ResponseCode::parse(code)),
            message.trim().to_owned(),
        ));
    }
    Some(Status::Err(None, rest))
}

fn strip_indicator<'a>(line: &'a [u8], indicator: &[u8]) -> Option<&'a [u8]> {
    if line.len() < indicator.len() || !line[..indicator.len()].eq_ignore_ascii_case(indicator) {
        return None;
    }
    let rest = &line[indicator.len()..];
    // `+OKAY` is not `+OK`.
    match rest.first() {
        None | Some(b' ' | b'\t') => Some(rest),
        Some(_) => None,
    }
}

/// `1 whqtswO00WBw418f9t5JxYwZ`. A unique id is 1 to 70 printable ASCII
/// characters (RFC 1939 section 7).
pub(super) fn parse_unique_id(line: &[u8]) -> Result<(u32, String)> {
    let (number, rest) = split_number(line, "UIDL")?;
    let id = rest.trim_ascii();
    if id.is_empty() || id.len() > 70 || !id.iter().all(|byte| (0x21..=0x7e).contains(byte)) {
        return Err(malformed("UIDL", line));
    }
    // Validated as ASCII above.
    let id = String::from_utf8_lossy(id).into_owned();
    Ok((number, id))
}

/// `1 120`: a message number and its size in octets.
pub(super) fn parse_size(line: &[u8]) -> Result<(u32, u64)> {
    let (number, rest) = split_number(line, "LIST")?;
    let size = rest
        .trim_ascii()
        .split(|byte| byte.is_ascii_whitespace())
        .next()
        .and_then(|size| std::str::from_utf8(size).ok())
        .and_then(|size| size.parse().ok())
        .ok_or_else(|| malformed("LIST", line))?;
    Ok((number, size))
}

fn split_number<'a>(line: &'a [u8], command: &'static str) -> Result<(u32, &'a [u8])> {
    let line_trimmed = line.trim_ascii_start();
    let end = line_trimmed
        .iter()
        .position(|byte| byte.is_ascii_whitespace())
        .ok_or_else(|| malformed(command, line))?;
    let number = std::str::from_utf8(&line_trimmed[..end])
        .ok()
        .and_then(|number| number.parse::<u32>().ok())
        .filter(|number| *number > 0)
        .ok_or_else(|| malformed(command, line))?;
    Ok((number, &line_trimmed[end..]))
}

fn malformed(command: &'static str, line: &[u8]) -> Error {
    Error::Protocol(format!(
        "malformed {command} line: {}",
        String::from_utf8_lossy(&line[..line.len().min(120)])
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(line: &str) -> (Option<ResponseCode>, String) {
        match parse_status(line.as_bytes()) {
            Some(Status::Err(code, text)) => (code, text),
            _ => panic!("not an error status: {line:?}"),
        }
    }

    #[test]
    fn status_lines() {
        assert!(matches!(parse_status(b"+OK"), Some(Status::Ok(text)) if text.is_empty()));
        assert!(
            matches!(parse_status(b"+ok 2 messages"), Some(Status::Ok(text)) if text == "2 messages")
        );
        assert!(parse_status(b"+OKAY").is_none());
        assert!(parse_status(b"* OK").is_none());
        assert!(parse_status(b"").is_none());
        assert_eq!(err("-ERR"), (None, String::new()));
        assert_eq!(
            err("-ERR [SYS/TEMP] try again"),
            (Some(ResponseCode::SysTemp), "try again".into())
        );
        assert_eq!(
            err("-ERR [in-use] locked"),
            (Some(ResponseCode::InUse), "locked".into())
        );
        assert_eq!(
            err("-ERR [X-VENDOR] odd"),
            (Some(ResponseCode::Other("X-VENDOR".into())), "odd".into())
        );
        // Brackets that are not a leading code stay part of the text.
        assert_eq!(err("-ERR [not a code] x"), (None, "[not a code] x".into()));
        assert_eq!(err("-ERR no [AUTH]"), (None, "no [AUTH]".into()));
    }

    #[test]
    fn listing_lines() {
        assert_eq!(parse_unique_id(b"3 a!~").unwrap(), (3, "a!~".into()));
        assert_eq!(parse_unique_id(b" 7  id ").unwrap(), (7, "id".into()));
        assert!(parse_unique_id(&[b"1 ".as_slice(), &[b'x'; 71]].concat()).is_err());
        assert_eq!(parse_size(b"2 4096").unwrap(), (2, 4096));
        assert!(parse_size(b"2").is_err());
        assert!(parse_size(b"2 -1").is_err());
    }
}
