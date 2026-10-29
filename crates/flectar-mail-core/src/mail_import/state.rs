//! What another mail app recorded about a message it stored: whether it was
//! read or starred, and whether it was deleted but not yet removed from the
//! file. Each app has its own place for this:
//!
//! - Thunderbird and Netscape write `X-Mozilla-Status`, a hexadecimal set of
//!   flags, into every message they store.
//! - mutt, Dovecot and other mbox writers use `Status` (`R` for read) and
//!   `X-Status` (`F` for flagged, `D` for deleted).
//! - Google Takeout lists a message's Gmail labels in `X-Gmail-Labels`.
//! - Maildir keeps flags in the file name, after `:2,`.

/// Flags of `X-Mozilla-Status`.
const MOZILLA_READ: u32 = 0x0001;
const MOZILLA_MARKED: u32 = 0x0004;
const MOZILLA_EXPUNGED: u32 = 0x0008;

/// Header text read at most, which bounds the work for a message whose
/// header never ends.
const MAX_HEADER_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Recorded {
    /// `None` when nothing was recorded either way.
    pub read: Option<bool>,
    pub starred: bool,
    /// Deleted in that app, but still in its file until the file is
    /// compacted.
    pub deleted: bool,
}

/// What a message's header, or the name of the file it is stored in, says.
pub(crate) fn recorded(raw: &[u8], file_name: Option<&str>) -> Recorded {
    let fields = Fields::of(raw);
    if let Some(flags) = fields.mozilla_status {
        return Recorded {
            read: Some(flags & MOZILLA_READ != 0),
            starred: flags & MOZILLA_MARKED != 0,
            deleted: flags & MOZILLA_EXPUNGED != 0,
        };
    }
    if let Some(recorded) = file_name.and_then(maildir_flags) {
        return recorded;
    }
    let mut recorded = Recorded::default();
    if let Some(status) = &fields.status {
        recorded.read = Some(status.contains('R'));
    }
    if let Some(flags) = &fields.x_status {
        recorded.starred = flags.contains('F');
        recorded.deleted = flags.contains('D');
    }
    if let Some(labels) = &fields.gmail_labels {
        let has = |name: &str| {
            labels
                .split(',')
                .any(|label| label.trim().eq_ignore_ascii_case(name))
        };
        recorded.read = recorded.read.or(Some(!has("Unread")));
        recorded.starred |= has("Starred");
    }
    recorded
}

/// The flags of a maildir file name such as `1700000000.M1P2.host:2,FS`.
/// Windows tools write `!` or `;` instead of the colon.
fn maildir_flags(name: &str) -> Option<Recorded> {
    let (_, flags) = [":2,", "!2,", ";2,"]
        .into_iter()
        .find_map(|separator| name.rsplit_once(separator))?;
    Some(Recorded {
        read: Some(flags.contains('S')),
        starred: flags.contains('F'),
        deleted: flags.contains('T'),
    })
}

#[derive(Default)]
struct Fields {
    mozilla_status: Option<u32>,
    status: Option<String>,
    x_status: Option<String>,
    gmail_labels: Option<String>,
}

impl Fields {
    /// The fields this module reads, from the header that ends at the first
    /// empty line. Folded fields are unfolded.
    fn of(raw: &[u8]) -> Self {
        let mut fields = Self::default();
        let mut current: Option<(String, String)> = None;
        let mut read = 0;
        for line in raw.split(|&byte| byte == b'\n') {
            read += line.len() + 1;
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            if line.is_empty() || read > MAX_HEADER_BYTES {
                break;
            }
            if matches!(line[0], b' ' | b'\t') {
                if let Some((_, value)) = &mut current {
                    value.push(' ');
                    value.push_str(String::from_utf8_lossy(line).trim());
                }
                continue;
            }
            if let Some((name, value)) = current.take() {
                fields.set(&name, value);
            }
            current = line.iter().position(|&byte| byte == b':').map(|colon| {
                (
                    String::from_utf8_lossy(&line[..colon])
                        .trim()
                        .to_ascii_lowercase(),
                    String::from_utf8_lossy(&line[colon + 1..])
                        .trim()
                        .to_owned(),
                )
            });
        }
        if let Some((name, value)) = current {
            fields.set(&name, value);
        }
        fields
    }

    fn set(&mut self, name: &str, value: String) {
        match name {
            "x-mozilla-status" => self.mozilla_status = u32::from_str_radix(&value, 16).ok(),
            "status" => self.status = Some(value),
            "x-status" => self.x_status = Some(value),
            "x-gmail-labels" => self.gmail_labels = Some(value),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(header: &str) -> Recorded {
        recorded(format!("{header}\r\n\r\nX-Status: D\r\n").as_bytes(), None)
    }

    #[test]
    fn thunderbird_flags_win() {
        assert_eq!(
            read("X-Mozilla-Status: 0005\r\nStatus: O"),
            Recorded {
                read: Some(true),
                starred: true,
                deleted: false,
            }
        );
        assert_eq!(
            read("Subject: x\r\nX-Mozilla-Status: 0008"),
            Recorded {
                read: Some(false),
                starred: false,
                deleted: true,
            }
        );
        // Thunderbird's flags outrank a maildir name.
        assert_eq!(
            recorded(b"X-Mozilla-Status: 0000\n\n", Some("1.M2:2,S")).read,
            Some(false)
        );
    }

    #[test]
    fn mbox_status_fields_are_read() {
        assert_eq!(
            read("Status: RO\r\nX-Status: F"),
            Recorded {
                read: Some(true),
                starred: true,
                deleted: false,
            }
        );
        assert_eq!(read("Status: O").read, Some(false));
        assert!(read("X-Status: AD").deleted);
        // The body never counts.
        assert_eq!(read("Subject: x"), Recorded::default());
    }

    #[test]
    fn takeout_labels_are_read() {
        assert_eq!(
            read("X-Gmail-Labels: Inbox,Starred,\r\n Unread"),
            Recorded {
                read: Some(false),
                starred: true,
                deleted: false,
            }
        );
        assert_eq!(read("X-Gmail-Labels: Archived,Opened").read, Some(true));
    }

    #[test]
    fn maildir_names_carry_flags() {
        assert_eq!(
            recorded(b"Subject: x\n\n", Some("1700000000.M1P2.host:2,FS")),
            Recorded {
                read: Some(true),
                starred: true,
                deleted: false,
            }
        );
        assert!(recorded(b"", Some("1.M2!2,ST")).deleted);
        assert_eq!(recorded(b"", Some("1.M2.host")), Recorded::default());
    }
}
