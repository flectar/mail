//! Files dragged onto the client's surfaces from a file manager or another
//! app.
//!
//! Drags go to the clipboard's own data device. Some compositors send a
//! client's drags to only one of its data devices, so a second device just for
//! drops would miss them, and on some it would silence the clipboard.

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;

/// A drag of files over one of the client's surfaces, named by the
/// `wl_surface` pointer, which its owner can match against its windows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DropEvent {
    /// Files came over the surface.
    Enter { surface: usize },
    /// They left it, or the drop held no files.
    Leave { surface: usize },
    /// They were dropped on it. Without a portal key these are the names the
    /// source gave, which a sandboxed client may not be able to open.
    Drop { surface: usize, paths: Vec<PathBuf> },
}

impl DropEvent {
    /// The surface the drag is over.
    pub fn surface(&self) -> usize {
        match self {
            Self::Enter { surface } | Self::Leave { surface } | Self::Drop { surface, .. } => {
                *surface
            },
        }
    }
}

/// Where drags go. Called on the clipboard's thread.
pub trait DropHandler: Send {
    /// A drag moved onto, off or was dropped on a surface.
    fn event(&mut self, event: DropEvent);

    /// The files a document portal transfer `key` names, readable by this
    /// client, which can be sandboxed. `None` when the portal can't tell, and
    /// the drop falls back to the files the source named itself.
    fn retrieve_files(&mut self, key: &str) -> Option<Vec<PathBuf>>;
}

/// Types that carry a document portal transfer key, which a sandboxed client
/// turns into files it can read. GTK 4 before 4.8.1 offered only the second.
pub(crate) const PORTAL_TYPES: [&str; 2] =
    ["application/vnd.portal.filetransfer", "application/vnd.portal.files"];

/// The list of file URIs every file manager offers.
pub(crate) const URI_LIST: &str = "text/uri-list";

/// Which of a portal key and the URI list to read from a drag offering
/// `offered`, or `None` when it carries neither.
pub(crate) fn wanted_types(offered: &[String]) -> Option<(Option<String>, bool)> {
    let portal = PORTAL_TYPES
        .iter()
        .find(|portal| offered.iter().any(|mime| mime == *portal))
        .map(|portal| portal.to_string());
    let uri_list = offered.iter().any(|mime| mime == URI_LIST);
    (portal.is_some() || uri_list).then_some((portal, uri_list))
}

/// The key in a portal transfer's data. Sources differ on a trailing NUL, so
/// it ends at the first one.
pub(crate) fn portal_key(data: &[u8]) -> Option<String> {
    let key = data.split(|byte| *byte == 0).next()?;
    let key = String::from_utf8_lossy(key).trim().to_string();
    (!key.is_empty()).then_some(key)
}

/// The local files a `text/uri-list` names (RFC 2483), one URI a line, with
/// comments starting with `#`. Other schemes and other hosts are left out.
pub(crate) fn file_paths(data: &[u8]) -> Vec<PathBuf> {
    String::from_utf8_lossy(data)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(file_path)
        .collect()
}

/// The path of a `file:` URI on this host, written `file:///p`,
/// `file://localhost/p` or `file:/p`, with percent escapes decoded.
fn file_path(uri: &str) -> Option<PathBuf> {
    if !uri.get(..5)?.eq_ignore_ascii_case("file:") {
        return None;
    }
    let rest = &uri[5..];
    let path = match rest.strip_prefix("//") {
        Some(authority_and_path) => {
            let slash = authority_and_path.find('/')?;
            let host = &authority_and_path[..slash];
            if !host.is_empty() && !host.eq_ignore_ascii_case("localhost") {
                return None;
            }
            &authority_and_path[slash..]
        },
        None => rest,
    };
    if !path.starts_with('/') {
        return None;
    }
    // A query or fragment isn't part of the file's name.
    let path = path.split(['?', '#']).next()?;
    Some(PathBuf::from(OsString::from_vec(percent_decode(path)?)))
}

fn percent_decode(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = bytes.get(index + 1..index + 3)?;
            let hex = std::str::from_utf8(hex).ok()?;
            decoded.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    // A NUL would cut the name short.
    (!decoded.contains(&0)).then_some(decoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_drag_is_read_for_its_portal_key_and_its_files() {
        let types = |offered: &[&str]| {
            wanted_types(&offered.iter().map(|mime| mime.to_string()).collect::<Vec<_>>())
        };
        assert_eq!(
            types(&["text/plain", "application/vnd.portal.filetransfer", "text/uri-list"]),
            Some((Some("application/vnd.portal.filetransfer".into()), true))
        );
        assert_eq!(
            types(&["application/vnd.portal.files"]),
            Some((Some("application/vnd.portal.files".into()), false))
        );
        assert_eq!(types(&["text/uri-list"]), Some((None, true)));
        assert_eq!(types(&["text/plain;charset=utf-8", "text/html"]), None);
    }

    #[test]
    fn portal_keys_end_at_a_nul() {
        assert_eq!(portal_key(b"4142\0").as_deref(), Some("4142"));
        assert_eq!(portal_key(b"4142").as_deref(), Some("4142"));
        assert_eq!(portal_key(b" 4142\n").as_deref(), Some("4142"));
        assert_eq!(portal_key(b"\0"), None);
        assert_eq!(portal_key(b""), None);
    }

    #[test]
    fn uri_lists_name_local_files() {
        let list = b"# from a file manager\r\n\
            file:///home/me/Plan%20B.pdf\r\n\
            file://localhost/home/me/notes.txt\r\n\
            file:/tmp/a.txt\n\
            FILE:///tmp/upper.txt\n\
            file:///tmp/caf%C3%A9.txt#section\n\
            https://example.test/remote.pdf\n\
            file://elsewhere/home/me/remote.txt\n\
            file:///tmp/bad%zz.txt\n\
            file:///tmp/nul%00.txt\n\
            \n";
        assert_eq!(
            file_paths(list),
            [
                "/home/me/Plan B.pdf",
                "/home/me/notes.txt",
                "/tmp/a.txt",
                "/tmp/upper.txt",
                "/tmp/café.txt",
            ]
            .map(PathBuf::from)
        );
    }

    #[test]
    fn names_that_are_not_utf_8_stay_as_they_are() {
        assert_eq!(
            file_paths(b"file:///tmp/%FF.bin"),
            [PathBuf::from(OsString::from_vec(b"/tmp/\xFF.bin".to_vec()))]
        );
    }
}
