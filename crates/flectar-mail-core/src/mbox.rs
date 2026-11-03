//! Mail as mbox (RFC 4155): each message follows a `From ` line.
//!
//! Exports are written as mboxrd, which quotes every line that could be
//! mistaken for a `From ` line with `>` and ends each message with an empty
//! line. Each message's header says whether it was read or starred, and a
//! folder's subfolders go into a folder named like its file, so an export
//! imports back as it was. [`Reader`] also reads what other mail apps write:
//! Thunderbird's `From - ` lines, messages without the empty line, CRLF line
//! endings, and the dates Google Takeout adds a zone to.

use std::collections::{HashMap, HashSet};
use std::io::{self, BufRead};
use std::path::PathBuf;

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
/// `From ` lines, and the empty line that ends it. Its header records
/// whether it was read or starred the way mutt, Dovecot and Thunderbird read
/// it, with `Status` and `X-Status`; fields that recorded it before are
/// left out.
pub(crate) fn entry(message: &[u8], read: bool, starred: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(message.len() + 64);
    let state = |out: &mut Vec<u8>| {
        out.extend_from_slice(if read {
            b"Status: RO\n"
        } else {
            b"Status: O\n"
        });
        if starred {
            out.extend_from_slice(b"X-Status: F\n");
        }
    };
    let mut in_header = true;
    let mut replaced_field = false;
    for line in message.split_inclusive(|&byte| byte == b'\n') {
        let content = line.strip_suffix(b"\n").unwrap_or(line);
        let content = content.strip_suffix(b"\r").unwrap_or(content);
        if in_header {
            if content.is_empty() {
                state(&mut out);
                in_header = false;
            } else if matches!(content[0], b' ' | b'\t') {
                if replaced_field {
                    continue;
                }
            } else {
                replaced_field = is_state_field(content);
                if replaced_field {
                    continue;
                }
            }
        }
        let unquoted = content.iter().position(|&byte| byte != b'>');
        if unquoted.is_some_and(|start| content[start..].starts_with(b"From ")) {
            out.push(b'>');
        }
        out.extend_from_slice(content);
        out.push(b'\n');
    }
    if in_header {
        state(&mut out);
    }
    out.push(b'\n');
    out
}

/// Whether a header line starts a field that records a message's state.
fn is_state_field(line: &[u8]) -> bool {
    let Some(colon) = line.iter().position(|&byte| byte == b':') else {
        return false;
    };
    let name = &line[..colon];
    [
        "status",
        "x-status",
        "x-mozilla-status",
        "x-mozilla-status2",
    ]
    .iter()
    .any(|field| name.eq_ignore_ascii_case(field.as_bytes()))
}

/// Where each folder of an export is written, by folder id: the folder's
/// messages go into `<name>.mbox`, inside a folder `<name>` of its parent's.
/// Folder paths separate their levels with `/`. Names are ones every
/// platform accepts, and unique within their folder whatever the letter
/// case.
pub(crate) fn layout<'a>(folders: impl IntoIterator<Item = (i64, &'a str)>) -> Vec<(i64, PathBuf)> {
    let mut folders = folders.into_iter().collect::<Vec<_>>();
    folders.sort_by(|left, right| left.1.cmp(right.1));
    let mut names = Names::default();
    folders
        .into_iter()
        .map(|(id, path)| {
            let mut file = PathBuf::new();
            for (end, _) in path.match_indices('/') {
                file.push(names.of(&path[..end]));
            }
            file.push(format!("{}.mbox", names.of(path)));
            (id, file)
        })
        .collect()
}

/// The names given to the folder paths of an export.
#[derive(Default)]
struct Names {
    by_path: HashMap<String, String>,
    /// Lowercase names already given, by the path of the folder they are in.
    taken: HashMap<String, HashSet<String>>,
}

impl Names {
    fn of(&mut self, path: &str) -> String {
        if let Some(name) = self.by_path.get(path) {
            return name.clone();
        }
        let (parent, leaf) = path.rsplit_once('/').unwrap_or(("", path));
        let cleaned: String = leaf
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
        let taken = self.taken.entry(parent.to_owned()).or_default();
        let mut name = base.to_owned();
        let mut number = 2;
        while !taken.insert(name.to_lowercase()) {
            name = format!("{base} ({number})");
            number += 1;
        }
        self.by_path.insert(path.to_owned(), name.clone());
        name
    }
}

/// One entry of an mbox file.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Entry {
    Message {
        /// The message without its `From ` line, with quoting removed.
        raw: Vec<u8>,
        /// When the message was stored, from its `From ` line.
        stored_at: Option<i64>,
    },
    /// A message larger than the reader's limit. It was skipped.
    TooLarge,
}

/// Reads the messages of an mbox file one at a time, holding at most one
/// message in memory.
pub(crate) struct Reader<R> {
    input: R,
    max_message_bytes: usize,
    /// The `From ` line that starts the next message.
    next: Option<Vec<u8>>,
    /// Whether `From ` lines carry a date. Mail that has one is only split
    /// at `From ` lines that have one too, since a message body can hold a
    /// line that merely starts with "From ". A file whose first `From ` line
    /// has no date is split at `From ` lines that follow an empty line.
    dated: bool,
    started: bool,
    position: u64,
    line: Vec<u8>,
}

impl<R: BufRead> Reader<R> {
    pub(crate) fn new(input: R, max_message_bytes: usize) -> Self {
        Self {
            input,
            max_message_bytes,
            next: None,
            dated: true,
            started: false,
            position: 0,
            line: Vec::new(),
        }
    }

    /// How many bytes of the file have been read.
    pub(crate) fn position(&self) -> u64 {
        self.position
    }

    /// The next message, or `None` at the end of the file. Fails with
    /// [`io::ErrorKind::InvalidData`] when the file isn't an mbox file.
    pub(crate) fn next_entry(&mut self) -> io::Result<Option<Entry>> {
        if !self.started {
            self.started = true;
            self.start()?;
        }
        let Some(separator) = self.next.take() else {
            return Ok(None);
        };
        let stored_at = from_line_date(&separator);
        let mut raw = Vec::new();
        let mut too_large = false;
        let mut after_empty_line = false;
        loop {
            let length = self.read_line()?;
            if length == 0 {
                break;
            }
            if self.line.starts_with(b"From ") && self.separates(after_empty_line) {
                self.next = Some(std::mem::take(&mut self.line));
                break;
            }
            after_empty_line = matches!(self.line.as_slice(), b"\n" | b"\r\n");
            if too_large {
                continue;
            }
            let line = unquote(&self.line);
            if length > self.line.len() || raw.len() + line.len() > self.max_message_bytes {
                too_large = true;
                raw = Vec::new();
                continue;
            }
            raw.extend_from_slice(line);
        }
        if too_large {
            return Ok(Some(Entry::TooLarge));
        }
        // The empty line that ends a message belongs to the file.
        if raw.ends_with(b"\r\n\r\n") {
            raw.truncate(raw.len() - 2);
        } else if raw.ends_with(b"\n\n") {
            raw.pop();
        }
        Ok(Some(Entry::Message { raw, stored_at }))
    }

    /// Finds the first `From ` line. Empty lines before it are tolerated.
    fn start(&mut self) -> io::Result<()> {
        loop {
            if self.read_line()? == 0 {
                return Ok(());
            }
            if matches!(self.line.as_slice(), b"\n" | b"\r\n") {
                continue;
            }
            if !self.line.starts_with(b"From ") {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "not an mbox file",
                ));
            }
            self.dated = from_line_date(&self.line).is_some();
            self.next = Some(std::mem::take(&mut self.line));
            return Ok(());
        }
    }

    /// Whether the `From ` line just read starts a new message.
    fn separates(&self, after_empty_line: bool) -> bool {
        if self.dated {
            from_line_date(&self.line).is_some()
        } else {
            after_empty_line
        }
    }

    /// Reads one line, its line break included, into `self.line`, keeping at
    /// most one byte more than a message may hold. Returns the line's full
    /// length, which is 0 at the end of the file.
    fn read_line(&mut self) -> io::Result<usize> {
        self.line.clear();
        let keep = self.max_message_bytes + 1;
        let mut length = 0;
        loop {
            let available = match self.input.fill_buf() {
                Ok(available) => available,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            };
            if available.is_empty() {
                break;
            }
            let (used, ended) = match available.iter().position(|&byte| byte == b'\n') {
                Some(end) => (end + 1, true),
                None => (available.len(), false),
            };
            let room = keep.saturating_sub(self.line.len());
            self.line.extend_from_slice(&available[..used.min(room)]);
            self.input.consume(used);
            length += used;
            if ended {
                break;
            }
        }
        self.position += length as u64;
        Ok(length)
    }
}

/// Removes the quoting mboxrd adds to lines that start with "From " after
/// any number of `>`.
fn unquote(line: &[u8]) -> &[u8] {
    let quotes = line.iter().take_while(|&&byte| byte == b'>').count();
    if quotes > 0 && line[quotes..].starts_with(b"From ") {
        &line[1..]
    } else {
        line
    }
}

/// The date of a `From ` line in milliseconds. The line names the sender,
/// which may be missing or `-`, and then the time in ctime form, such as
/// `From alice@example.com Thu Jan  1 00:00:00 1970`. Some writers leave out
/// the seconds or add a zone before or after the year.
pub(crate) fn from_line_date(line: &[u8]) -> Option<i64> {
    let text = String::from_utf8_lossy(line.strip_prefix(b"From ")?);
    let tokens = text.split_ascii_whitespace().collect::<Vec<_>>();
    (0..tokens.len()).find_map(|start| ctime(&tokens[start..]))
}

fn ctime(tokens: &[&str]) -> Option<i64> {
    const WEEKDAYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];
    const MONTHS: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    let [weekday, month, day, time, rest @ ..] = tokens else {
        return None;
    };
    let name = |token: &str| token.get(..3).map(str::to_ascii_lowercase);
    if !WEEKDAYS.contains(&name(weekday)?.as_str()) {
        return None;
    }
    let month = name(month)?;
    let month = (1..).zip(MONTHS).find(|(_, known)| *known == month)?.0;
    let day = day.parse::<u32>().ok()?;
    let mut clock = time.split(':').map(|part| part.parse::<u32>().ok());
    let (hour, minute) = (clock.next()??, clock.next()??);
    let second = clock.next().unwrap_or(Some(0))?;
    if clock.next().is_some() {
        return None;
    }
    let mut year = None;
    let mut offset_minutes = 0;
    for token in rest.iter().take(2) {
        if token.len() == 4 && token.bytes().all(|byte| byte.is_ascii_digit()) {
            year = token.parse::<i32>().ok();
        } else if let Some(minutes) = zone_offset(token) {
            offset_minutes = minutes;
        }
    }
    let local = chrono::NaiveDate::from_ymd_opt(year?, month, day)?
        .and_hms_opt(hour, minute, second)?
        .and_utc()
        .timestamp_millis();
    Some(local - i64::from(offset_minutes) * 60_000)
}

/// A numeric zone such as `+0200`, in minutes east of UTC.
fn zone_offset(token: &str) -> Option<i32> {
    let (sign, digits) = match token.as_bytes().first()? {
        b'+' => (1, &token[1..]),
        b'-' => (-1, &token[1..]),
        _ => return None,
    };
    if digits.len() != 4 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let value = digits.parse::<i32>().ok()?;
    Some(sign * (value / 100 * 60 + value % 100))
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
            entry(
                b"Subject: x\r\n\r\nFrom here\r\n>From there\r\n>>From far\r\n>\r\nlast",
                true,
                false
            ),
            b"Subject: x\nStatus: RO\n\n>From here\n>>From there\n>>>From far\n>\nlast\n\n"
        );
        assert_eq!(
            entry(b"From:me\n", false, true),
            b"From:me\nStatus: O\nX-Status: F\n\n"
        );
    }

    #[test]
    fn earlier_state_fields_are_replaced() {
        assert_eq!(
            entry(
                b"X-Mozilla-Status: 0001\r\nX-Mozilla-Status2: 00000000\r\nSubject: x\r\n\
                  Status: RO\r\nX-Status: F\r\n  continued\r\n\r\nStatus: in the body\r\n",
                false,
                false
            ),
            b"Subject: x\nStatus: O\n\nStatus: in the body\n\n"
        );
        // The state survives a round trip through the reader.
        let mut file = separator("a@example.com", 0).into_bytes();
        file.extend(entry(b"Subject: x\n\nbody\n", true, true));
        let mut reader = Reader::new(file.as_slice(), 1024);
        let Some(Entry::Message { raw, .. }) = reader.next_entry().unwrap() else {
            panic!("expected a message");
        };
        assert_eq!(
            crate::mail_import::state::recorded(&raw, None),
            crate::mail_import::state::Recorded {
                read: Some(true),
                starred: true,
                deleted: false,
            }
        );
    }

    fn read_all(file: &[u8]) -> io::Result<Vec<Entry>> {
        let mut reader = Reader::new(file, 1024);
        let mut entries = Vec::new();
        while let Some(entry) = reader.next_entry()? {
            entries.push(entry);
        }
        assert_eq!(reader.position(), file.len() as u64);
        Ok(entries)
    }

    fn message(raw: &[u8], stored_at: Option<i64>) -> Entry {
        Entry::Message {
            raw: raw.to_vec(),
            stored_at,
        }
    }

    #[test]
    fn exports_read_back_with_their_bodies_unchanged() {
        let originals: [&[u8]; 3] = [
            b"Subject: one\n\nFrom here\n>From there\n",
            b"Subject: two\n\nno line break at the end",
            b"Subject: three\n\n\n",
        ];
        let mut file = Vec::new();
        for original in originals {
            file.extend_from_slice(separator("alice@example.com", 86_400_000).as_bytes());
            file.extend(entry(original, true, false));
        }
        assert_eq!(
            read_all(&file).unwrap(),
            [
                message(
                    b"Subject: one\nStatus: RO\n\nFrom here\n>From there\n",
                    Some(86_400_000)
                ),
                message(
                    b"Subject: two\nStatus: RO\n\nno line break at the end\n",
                    Some(86_400_000)
                ),
                message(b"Subject: three\nStatus: RO\n\n\n", Some(86_400_000)),
            ]
        );
    }

    #[test]
    fn dated_mail_is_split_only_at_dated_from_lines() {
        let file = b"From - Mon Jan  1 00:00:00 2001\r\nSubject: a\r\n\r\n\
            From me to you\r\n\r\nFrom - Tue Jan  2 00:00:00 2001\r\nSubject: b\r\n\r\nb\r\n\
            From - Wed Jan  3 00:00:00 2001\r\nSubject: c\r\n\r\nc\r\n";
        assert_eq!(
            read_all(file).unwrap(),
            [
                message(
                    b"Subject: a\r\n\r\nFrom me to you\r\n",
                    Some(978_307_200_000)
                ),
                message(b"Subject: b\r\n\r\nb\r\n", Some(978_393_600_000)),
                message(b"Subject: c\r\n\r\nc\r\n", Some(978_480_000_000)),
            ]
        );
    }

    #[test]
    fn undated_mail_is_split_after_empty_lines() {
        let file = b"\nFrom alice\nSubject: a\n\nbody\nFrom here\n\nFrom bob\nSubject: b\n\nb\n";
        assert_eq!(
            read_all(file).unwrap(),
            [
                message(b"Subject: a\n\nbody\nFrom here\n", None),
                message(b"Subject: b\n\nb\n", None),
            ]
        );
    }

    #[test]
    fn oversized_messages_are_skipped() {
        let mut file = b"From - Mon Jan  1 00:00:00 2001\nSubject: long line\n\n".to_vec();
        file.extend([b'x'; 2000]);
        file.extend(b"\n\nFrom - Mon Jan  1 00:00:00 2001\nSubject: many lines\n\n");
        for _ in 0..200 {
            file.extend(b"0123456789\n");
        }
        file.extend(b"\nFrom - Mon Jan  1 00:00:00 2001\nSubject: small\n\nok\n");
        assert_eq!(
            read_all(&file).unwrap(),
            [
                Entry::TooLarge,
                Entry::TooLarge,
                message(b"Subject: small\n\nok\n", Some(978_307_200_000)),
            ]
        );
    }

    #[test]
    fn other_files_are_not_mbox_files() {
        let error = read_all(b"Subject: hello\n\nFrom - Mon Jan  1 00:00:00 2001\n").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(read_all(b"").unwrap(), []);
        assert_eq!(read_all(b"\r\n\n").unwrap(), []);
    }

    #[test]
    fn from_line_dates_allow_zones_and_missing_seconds() {
        let date = |line: &[u8]| from_line_date(line);
        assert_eq!(
            date(b"From 1797@xxx Wed Jan 01 00:00:00 +0000 2020\n"),
            Some(1_577_836_800_000)
        );
        assert_eq!(
            date(b"From - Thu Jan  1 01:00:00 1970\r\n"),
            Some(3_600_000)
        );
        assert_eq!(date(b"From x Thu Jan  1 02:00 +0200 1970\n"), Some(0));
        assert_eq!(
            date(b"From x Thu Jan  1 00:00:00 1970 -0100\n"),
            Some(3_600_000)
        );
        assert_eq!(date(b"From Thu Jan  1 00:00:00 1970\n"), Some(0));
        assert_eq!(date(b"From my experience, it works\n"), None);
        assert_eq!(date(b"From x Thu Jan 32 00:00:00 1970\n"), None);
        assert_eq!(date(b"From x Thu Jan  1 00:00:00:00 1970\n"), None);
        assert_eq!(date(b"Subject: Thu Jan  1 00:00:00 1970\n"), None);
    }

    #[test]
    fn folders_are_laid_out_with_safe_unique_names() {
        let layout = layout([
            (1, "Inbox"),
            (2, "inbox"),
            (3, "Work"),
            (4, "Work/Q: 1?"),
            (5, ".."),
            (6, " \u{7} "),
            (7, "Archive/2019/Spring"),
            (8, "inbox/Sub"),
        ]);
        let files = layout
            .iter()
            .map(|(id, file)| (*id, file.to_string_lossy().replace('\\', "/")))
            .collect::<HashMap<_, _>>();
        assert_eq!(files[&1], "Inbox.mbox");
        assert_eq!(files[&2], "inbox (2).mbox");
        assert_eq!(files[&3], "Work.mbox");
        assert_eq!(files[&4], "Work/Q_ 1_.mbox");
        assert_eq!(files[&5], "Folder.mbox");
        assert_eq!(files[&6], "_.mbox");
        // A folder above that isn't exported still gives its name.
        assert_eq!(files[&7], "Archive/2019/Spring.mbox");
        // Subfolders follow their folder's name, renamed or not.
        assert_eq!(files[&8], "inbox (2)/Sub.mbox");
    }
}
