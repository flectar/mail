//! Mail as mboxrd (RFC 4155), the format mail apps import: each message
//! follows a `From ` line, and its lines that could be mistaken for one are
//! quoted with `>`.

use std::collections::HashSet;

/// The `From ` line that starts a message.
pub(crate) fn separator(sender: &str, date_ms: i64) -> String {
    let date =
        chrono::DateTime::from_timestamp_millis(date_ms).unwrap_or(chrono::DateTime::UNIX_EPOCH);
    let sender =
        if sender.is_empty() || sender.contains(|c: char| c.is_whitespace() || c.is_control()) {
            "MAILER-DAEMON"
        } else {
            sender
        };
    format!("From {sender} {}\n", date.format("%a %b %e %H:%M:%S %Y"))
}

/// A message as it follows its `From ` line: LF line endings, `>`-quoted
/// `From ` lines, and the empty line that ends it.
pub(crate) fn entry(message: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(message.len() + 64);
    for line in message.split_inclusive(|&byte| byte == b'\n') {
        let content = line.strip_suffix(b"\n").unwrap_or(line);
        let content = content.strip_suffix(b"\r").unwrap_or(content);
        let unquoted = content.iter().position(|&byte| byte != b'>');
        if unquoted.is_some_and(|start| content[start..].starts_with(b"From ")) {
            out.push(b'>');
        }
        out.extend_from_slice(content);
        out.push(b'\n');
    }
    out.push(b'\n');
    out
}

/// A file name for a folder's mbox file that every platform accepts and no
/// other folder of the export already uses, whatever the letter case.
pub(crate) fn file_name(folder: &str, taken: &mut HashSet<String>) -> String {
    let cleaned: String = folder
        .chars()
        .map(|character| match character {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            character if character.is_control() => '_',
            character => character,
        })
        .collect();
    let base = match cleaned.trim().trim_matches('.') {
        "" => "Folder",
        base => base,
    };
    let mut name = format!("{base}.mbox");
    let mut number = 2;
    while !taken.insert(name.to_lowercase()) {
        name = format!("{base} ({number}).mbox");
        number += 1;
    }
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_are_quoted_and_separated() {
        assert_eq!(
            separator("alice@example.com", 0),
            "From alice@example.com Thu Jan  1 00:00:00 1970\n"
        );
        assert!(separator("", 0).starts_with("From MAILER-DAEMON "));
        assert!(separator("two words", 0).starts_with("From MAILER-DAEMON "));
        assert_eq!(
            entry(b"Subject: x\r\n\r\nFrom here\r\n>From there\r\n>>From far\r\n>\r\nlast"),
            b"Subject: x\n\n>From here\n>>From there\n>>>From far\n>\nlast\n\n"
        );
        assert_eq!(entry(b"From:me\n"), b"From:me\n\n");
    }

    #[test]
    fn folder_file_names_are_safe_and_unique() {
        let mut taken = HashSet::new();
        assert_eq!(file_name("Inbox", &mut taken), "Inbox.mbox");
        assert_eq!(file_name("inbox", &mut taken), "inbox (2).mbox");
        assert_eq!(file_name("Work/Q: 1?", &mut taken), "Work_Q_ 1_.mbox");
        assert_eq!(file_name("..", &mut taken), "Folder.mbox");
        assert_eq!(file_name(" \u{7} ", &mut taken), "_.mbox");
    }
}
