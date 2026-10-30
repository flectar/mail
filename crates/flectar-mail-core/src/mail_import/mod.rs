//! Importing saved mail: mbox files, messages saved one per file, maildirs,
//! and the mail Thunderbird keeps in its profile.
//!
//! Imported mail goes into folders that exist only on this device: those of
//! local mail, or of a POP3 account, whose mail is already kept here alone.
//! A scan ([`scan_folder`], [`scan_files`], and through
//! [`crate::migration::open`] archives and Thunderbird profiles) only lists
//! the folders a source holds. [`crate::Core::import_mail`] then reads each
//! chosen folder's messages once and files them like downloaded mail. A
//! message the destination folder already has is skipped, so an import that
//! stopped part way can be run again.

pub(crate) mod scan;
pub(crate) mod state;
mod upload;
mod write;

pub use scan::{ChosenFile, scan_files, scan_folder};
pub use write::{Destination, MailImportProgress, MailImportReport, MailImportRequest};

use std::path::PathBuf;

/// How the file names of imported originals end.
pub(crate) const ORIGINAL_SUFFIX: &str = ".imported.eml";

/// Mail found in files, a folder, or a ZIP archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailSource {
    /// Every folder with mail in it or below it, each before its
    /// subfolders.
    pub folders: Vec<SourceFolder>,
    /// The archive the folders are in.
    pub(crate) archive: Option<PathBuf>,
}

impl MailSource {
    /// The size of all the mail, for estimating an import.
    pub fn size(&self) -> u64 {
        self.folders.iter().map(|folder| folder.size).sum()
    }
}

impl SourceFolder {
    /// Whether it holds messages itself, not only in its subfolders.
    pub fn has_messages(&self) -> bool {
        !self.parts.is_empty()
    }
}

/// A folder of mail found in a source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFolder {
    /// The folder's name after those of the folders it is in. Empty for
    /// messages chosen one by one, which the import puts in a folder it
    /// names.
    pub path: Vec<String>,
    pub kind: FolderKind,
    /// The size of the folder's own mail, without its subfolders'.
    pub size: u64,
    /// Where its messages are. Empty when only its subfolders hold mail.
    pub(crate) parts: Vec<Part>,
}

/// What a top-level folder holds, going by the names mail apps give their
/// own folders. Thunderbird's are always English on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FolderKind {
    Inbox,
    Drafts,
    Sent,
    Archive,
    Junk,
    Trash,
    Other,
}

impl FolderKind {
    pub(crate) fn of(name: &str) -> Self {
        match name.trim().to_lowercase().as_str() {
            "inbox" => Self::Inbox,
            "drafts" | "draft" => Self::Drafts,
            "sent" | "sent messages" | "sent items" | "sent mail" => Self::Sent,
            "archive" | "archives" => Self::Archive,
            "junk" | "spam" | "junk e-mail" | "junk email" => Self::Junk,
            "trash" | "deleted messages" | "deleted items" | "bin" => Self::Trash,
            _ => Self::Other,
        }
    }
}

/// A file holding some of a folder's messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Part {
    Mbox(Location),
    /// One message, such as an `.eml` file or a file of a maildir.
    Message(Location),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Location {
    File(PathBuf),
    /// A file in [`MailSource::archive`].
    Entry {
        index: usize,
        name: String,
    },
}

impl Location {
    fn file_name(&self) -> Option<String> {
        match self {
            Self::File(path) => path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned()),
            Self::Entry { name, .. } => Some(name.clone()),
        }
    }
}

#[cfg(test)]
mod tests;
