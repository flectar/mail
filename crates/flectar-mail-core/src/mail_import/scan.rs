//! Finding saved mail in what the user picks, without reading it all:
//!
//! - mbox files, with or without an extension. Thunderbird keeps a folder's
//!   subfolders in `<name>.sbd`; Flectar's exports in a folder named like
//!   the file.
//! - Apple Mail's exports: `<name>.mbox` folders holding a file `mbox`.
//! - Maildirs, whose messages are files in `cur` and `new`. Dovecot keeps
//!   subfolders as `.<name>` maildirs inside; Thunderbird in `<name>.sbd`.
//! - `.eml` files, one message each.
//! - ZIP archives of any of these.

use super::{FolderKind, Location, MailSource, Part, SourceFolder};
use crate::migration::SourceError;
use std::collections::{HashMap, HashSet};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

/// Folders deeper than this are not looked into.
const MAX_DEPTH: usize = 32;
/// Files and folders looked at in one scan.
const MAX_ENTRIES: usize = 200_000;
/// Bytes read from a file to tell what it holds.
const SNIFF_BYTES: u64 = 4096;
/// Extensions of files mail apps keep next to their mail, and of other
/// common files, which are never mail.
const NOT_MAIL: [&str; 25] = [
    "msf", "dat", "html", "htm", "json", "js", "sqlite", "plist", "log", "txt", "xml", "ini",
    "lock", "ics", "vcf", "pdf", "png", "jpg", "jpeg", "gif", "zip", "doc", "docx", "xls", "csv",
];

/// Where a source's files are read: a folder on this device or a ZIP
/// archive.
pub(crate) enum Tree {
    Folder,
    Archive(zip::ZipArchive<io::BufReader<std::fs::File>>),
}

impl Tree {
    pub(crate) fn for_source(source: &MailSource) -> io::Result<Self> {
        match &source.archive {
            Some(path) => open_archive(path).map(Self::Archive),
            None => Ok(Self::Folder),
        }
    }

    pub(crate) fn open(&mut self, location: &Location) -> io::Result<Box<dyn Read + '_>> {
        match (self, location) {
            (_, Location::File(path)) => Ok(Box::new(std::fs::File::open(path)?)),
            (Self::Archive(archive), Location::Entry { index, .. }) => Ok(Box::new(
                archive.by_index(*index).map_err(io::Error::other)?,
            )),
            (Self::Folder, Location::Entry { .. }) => {
                Err(io::Error::other("the archive is not open"))
            }
        }
    }

    fn head(&mut self, location: &Location) -> io::Result<Vec<u8>> {
        let mut head = Vec::new();
        self.open(location)?
            .take(SNIFF_BYTES)
            .read_to_end(&mut head)?;
        Ok(head)
    }
}

fn open_archive(path: &Path) -> io::Result<zip::ZipArchive<io::BufReader<std::fs::File>>> {
    zip::ZipArchive::new(io::BufReader::new(std::fs::File::open(path)?)).map_err(io::Error::other)
}

/// Mail in a folder on this device: an mbox file's folder, a Thunderbird
/// store, a maildir, an export, or a folder of `.eml` files.
pub fn scan_folder(path: &Path) -> Result<MailSource, SourceError> {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut budget = MAX_ENTRIES;
    let nodes = list_folder(path, 0, &mut budget).map_err(unreadable)?;
    let root = Node {
        name,
        kind: NodeKind::Folder(nodes),
    };
    let folders = found_in_root(&mut Tree::Folder, &root);
    source(folders, None)
}

/// Mail in a ZIP archive, below `prefix` (`""`, or a folder path ending in
/// `/`), named like the archive itself when it holds loose messages.
pub(crate) fn scan_archive(path: &Path, prefix: &str) -> Result<MailSource, SourceError> {
    let mut archive = open_archive(path).map_err(|error| SourceError::Malformed {
        format: "ZIP",
        reason: error.to_string(),
    })?;
    let mut budget = MAX_ENTRIES;
    let nodes = list_archive(&mut archive, prefix, &mut budget);
    let name = prefix
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            path.file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
        })
        .unwrap_or_default();
    let root = Node {
        name,
        kind: NodeKind::Folder(nodes),
    };
    let folders = found_in_root(&mut Tree::Archive(archive), &root);
    source(folders, Some(path.to_owned()))
}

/// Files chosen one by one: each mbox file is a folder, and single
/// messages share one folder with no name, which the import names.
pub fn scan_files(paths: &[PathBuf]) -> Result<MailSource, SourceError> {
    let mut tree = Tree::Folder;
    let mut folders = Vec::new();
    let mut loose = Found::default();
    for path in paths {
        let location = Location::File(path.clone());
        let head = tree.head(&location).map_err(unreadable)?;
        let size = std::fs::metadata(path).map_err(unreadable)?.len();
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        if is_mbox(&head) {
            folders.push(Found {
                name: strip_mbox_extension(&name).to_owned(),
                parts: vec![Part::Mbox(location)],
                size,
                children: Vec::new(),
            });
        } else if has_extension(&name, "eml") || is_message(&head) {
            loose.parts.push(Part::Message(location));
            loose.size += size;
        } else {
            return Err(SourceError::UnknownFormat);
        }
    }
    if !loose.parts.is_empty() {
        folders.insert(0, loose);
    }
    source(folders, None)
}

/// Whether a file's first bytes are those of an mbox file.
pub(crate) fn is_mbox(head: &[u8]) -> bool {
    let start = head
        .iter()
        .position(|&byte| !matches!(byte, b'\r' | b'\n'))
        .unwrap_or(head.len());
    head[start..].starts_with(b"From ")
}

/// Whether a file's first bytes are those of a message: a header whose
/// first line is a field, which has a field every message has.
pub(crate) fn is_message(head: &[u8]) -> bool {
    const FIELDS: [&str; 9] = [
        "from",
        "date",
        "subject",
        "message-id",
        "received",
        "return-path",
        "delivered-to",
        "mime-version",
        "x-mozilla-status",
    ];
    let mut known = false;
    for (number, line) in head.split(|&byte| byte == b'\n').enumerate() {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            break;
        }
        if matches!(line[0], b' ' | b'\t') && number > 0 {
            continue;
        }
        let Some(colon) = line.iter().position(|&byte| byte == b':') else {
            return false;
        };
        let name = &line[..colon];
        if name.is_empty()
            || !name
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'-')
        {
            return false;
        }
        known |= FIELDS
            .iter()
            .any(|field| name.eq_ignore_ascii_case(field.as_bytes()));
    }
    known
}

fn unreadable(error: io::Error) -> SourceError {
    SourceError::Unreadable(error.to_string())
}

fn source(found: Vec<Found>, archive: Option<PathBuf>) -> Result<MailSource, SourceError> {
    let mut folders = Vec::new();
    flatten(found, &[], &mut folders);
    if folders.is_empty() {
        return Err(SourceError::NoMail);
    }
    Ok(MailSource { folders, archive })
}

struct Node {
    name: String,
    kind: NodeKind,
}

enum NodeKind {
    Folder(Vec<Node>),
    File { location: Location, size: u64 },
}

impl Node {
    fn children(&self) -> Vec<&Node> {
        match &self.kind {
            NodeKind::Folder(children) => children.iter().collect(),
            NodeKind::File { .. } => Vec::new(),
        }
    }

    fn is_maildir(&self) -> bool {
        matches!(&self.kind, NodeKind::Folder(children)
        if children.iter().any(|child| {
            matches!(child.kind, NodeKind::Folder(_)) && matches!(child.name.as_str(), "cur" | "new")
        }))
    }
}

/// Lists a folder on this device. Hidden entries and links are left out:
/// a link could lead anywhere, including back into the folder.
fn list_folder(path: &Path, depth: usize, budget: &mut usize) -> io::Result<Vec<Node>> {
    let mut nodes = Vec::new();
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        if *budget == 0 {
            break;
        }
        *budget -= 1;
        let name = entry.file_name().to_string_lossy().into_owned();
        let kind = entry.file_type()?;
        if kind.is_dir() {
            let children = if depth < MAX_DEPTH {
                // An unreadable subfolder hides only itself.
                list_folder(&entry.path(), depth + 1, budget).unwrap_or_default()
            } else {
                Vec::new()
            };
            nodes.push(Node {
                name,
                kind: NodeKind::Folder(children),
            });
        } else if kind.is_file() {
            let size = entry.metadata()?.len();
            nodes.push(Node {
                name,
                kind: NodeKind::File {
                    location: Location::File(entry.path()),
                    size,
                },
            });
        }
    }
    Ok(nodes)
}

/// Lists the files of an archive below `prefix` as a tree. Entry names are
/// only used as folder names, so no name is ever resolved on disk.
fn list_archive(
    archive: &mut zip::ZipArchive<io::BufReader<std::fs::File>>,
    prefix: &str,
    budget: &mut usize,
) -> Vec<Node> {
    let mut root = Vec::new();
    for index in 0..archive.len() {
        if *budget == 0 {
            break;
        }
        // The name comes from the archive's directory, so only the entries
        // below the prefix are read.
        let Some(Ok(name)) = archive.name_for_index(index) else {
            continue;
        };
        let name = name.replace('\\', "/");
        let Some(relative) = name.strip_prefix(prefix) else {
            continue;
        };
        let parts = relative
            .split('/')
            .filter(|part| !part.is_empty() && *part != ".")
            .collect::<Vec<_>>();
        if parts.is_empty() || parts.len() > MAX_DEPTH || parts.contains(&"..") {
            continue;
        }
        let Ok(entry) = archive.by_index(index) else {
            continue;
        };
        if !entry.is_file() {
            continue;
        }
        *budget -= 1;
        let size = entry.size();
        let mut level = &mut root;
        for folder in &parts[..parts.len() - 1] {
            let position = match level.iter().position(|node: &Node| {
                node.name == *folder && matches!(node.kind, NodeKind::Folder(_))
            }) {
                Some(position) => position,
                None => {
                    level.push(Node {
                        name: (*folder).to_owned(),
                        kind: NodeKind::Folder(Vec::new()),
                    });
                    level.len() - 1
                }
            };
            let NodeKind::Folder(children) = &mut level[position].kind else {
                unreachable!("only folders are matched");
            };
            level = children;
        }
        level.push(Node {
            name: parts[parts.len() - 1].to_owned(),
            kind: NodeKind::File {
                location: Location::Entry {
                    index,
                    name: parts[parts.len() - 1].to_owned(),
                },
                size,
            },
        });
    }
    root
}

/// A folder found, before folders without mail are dropped.
#[derive(Default)]
struct Found {
    name: String,
    parts: Vec<Part>,
    /// The size of `parts`.
    size: u64,
    children: Vec<Found>,
}

impl Found {
    fn has_mail(&self) -> bool {
        !self.parts.is_empty() || self.children.iter().any(Found::has_mail)
    }
}

/// The folders found in a chosen folder. When it is a maildir itself, or
/// holds messages of its own, it is a folder too.
fn found_in_root(tree: &mut Tree, root: &Node) -> Vec<Found> {
    if root.is_maildir() {
        return vec![maildir(tree, root, None, 0)];
    }
    let level = find(tree, &root.children(), 0);
    let mut folders = level.folders;
    if !level.loose.is_empty() {
        folders.insert(
            0,
            Found {
                name: root.name.clone(),
                parts: level.loose,
                size: level.loose_size,
                children: Vec::new(),
            },
        );
    }
    folders
}

#[derive(Default)]
struct Level {
    folders: Vec<Found>,
    /// Single messages in the folder itself.
    loose: Vec<Part>,
    loose_size: u64,
}

fn find(tree: &mut Tree, nodes: &[&Node], depth: usize) -> Level {
    let mut level = Level::default();
    if depth > MAX_DEPTH {
        return level;
    }
    let mut nodes = nodes.to_vec();
    nodes.sort_by(|left, right| {
        (left.name.to_lowercase(), &left.name).cmp(&(right.name.to_lowercase(), &right.name))
    });
    let folders = nodes
        .iter()
        .filter(|node| matches!(node.kind, NodeKind::Folder(_)))
        .map(|node| (node.name.as_str(), *node))
        .collect::<HashMap<_, _>>();
    let mut claimed = HashSet::new();

    for node in &nodes {
        let NodeKind::File { location, size } = &node.kind else {
            continue;
        };
        if node.name.starts_with('.') || NOT_MAIL.iter().any(|ext| has_extension(&node.name, ext)) {
            continue;
        }
        if has_extension(&node.name, "eml") {
            level.loose.push(Part::Message(location.clone()));
            level.loose_size += size;
            continue;
        }
        let Ok(head) = tree.head(location) else {
            continue;
        };
        // An empty mbox file is a Thunderbird folder without messages.
        if !is_mbox(&head) && !(head.is_empty() && folders.contains_key(sbd(&node.name).as_str())) {
            continue;
        }
        let name = strip_mbox_extension(&node.name).to_owned();
        let mut found = Found {
            name: name.clone(),
            parts: if *size == 0 {
                Vec::new()
            } else {
                vec![Part::Mbox(location.clone())]
            },
            size: *size,
            children: Vec::new(),
        };
        for subfolders in [sbd(&node.name), name] {
            if let Some(folder) = folders.get(subfolders.as_str())
                && !folder.is_maildir()
                && claimed.insert(subfolders.clone())
            {
                let below = find(tree, &folder.children(), depth + 1);
                found.children.extend(below.folders);
                found.parts.extend(below.loose);
                found.size += below.loose_size;
            }
        }
        level.folders.push(found);
    }

    for node in &nodes {
        let NodeKind::Folder(_) = &node.kind else {
            continue;
        };
        if node.name.starts_with('.') || claimed.contains(&node.name) {
            continue;
        }
        let children = node.children();
        if node.is_maildir() {
            let subfolders = folders.get(sbd(&node.name).as_str()).copied();
            if let Some(subfolders) = subfolders {
                claimed.insert(subfolders.name.clone());
            }
            level.folders.push(maildir(tree, node, subfolders, depth));
            continue;
        }
        if let Some(name) = node.name.strip_suffix(".sbd") {
            // The folder's own file is missing; its subfolders remain.
            let below = find(tree, &children, depth + 1);
            level.folders.push(Found {
                name: name.to_owned(),
                parts: below.loose,
                size: below.loose_size,
                children: below.folders,
            });
            continue;
        }
        if let Some(name) = strip_extension(&node.name, "mbox") {
            // Apple Mail's export: the messages are in the file `mbox`.
            let mailbox = children
                .iter()
                .find(|child| child.name == "mbox" && matches!(child.kind, NodeKind::File { .. }));
            let rest = children
                .iter()
                .copied()
                .filter(|child| child.name != "mbox")
                .collect::<Vec<_>>();
            let below = find(tree, &rest, depth + 1);
            let mut found = Found {
                name: name.to_owned(),
                parts: below.loose,
                size: below.loose_size,
                children: below.folders,
            };
            if let Some(Node {
                kind: NodeKind::File { location, size },
                ..
            }) = mailbox
            {
                found.parts.insert(0, Part::Mbox(location.clone()));
                found.size += size;
            }
            level.folders.push(found);
            continue;
        }
        let below = find(tree, &children, depth + 1);
        level.folders.push(Found {
            name: node.name.clone(),
            parts: below.loose,
            size: below.loose_size,
            children: below.folders,
        });
    }
    level
}

/// A maildir: its messages are the files in `cur` and `new`. Its subfolders
/// are Thunderbird's `<name>.sbd` next to it, and Dovecot's `.<name>`
/// maildirs inside it, where dots separate the levels of the name.
fn maildir(tree: &mut Tree, node: &Node, sbd_folder: Option<&Node>, depth: usize) -> Found {
    let mut found = Found {
        name: node.name.clone(),
        ..Found::default()
    };
    for child in node.children() {
        match (&child.kind, child.name.as_str()) {
            (NodeKind::Folder(files), "cur" | "new") => {
                for file in files {
                    if let NodeKind::File { location, size } = &file.kind
                        && !file.name.starts_with('.')
                    {
                        found.parts.push(Part::Message(location.clone()));
                        found.size += size;
                    }
                }
            }
            (NodeKind::Folder(_), name) if name.starts_with('.') && child.is_maildir() => {
                let mut path = name[1..]
                    .split('.')
                    .filter(|part| !part.is_empty())
                    .map(str::to_owned)
                    .collect::<Vec<_>>();
                let Some(leaf) = path.pop() else {
                    continue;
                };
                let mut sub = maildir(tree, child, None, depth + 1);
                sub.name = leaf;
                let mut level = &mut found.children;
                for name in path {
                    let position = match level.iter().position(|folder| folder.name == name) {
                        Some(position) => position,
                        None => {
                            level.push(Found {
                                name,
                                ..Found::default()
                            });
                            level.len() - 1
                        }
                    };
                    level = &mut level[position].children;
                }
                level.push(sub);
            }
            _ => {}
        }
    }
    if let Some(folder) = sbd_folder
        && depth < MAX_DEPTH
    {
        let below = find(tree, &folder.children(), depth + 1);
        found.children.extend(below.folders);
    }
    found
}

fn flatten(found: Vec<Found>, path: &[String], folders: &mut Vec<SourceFolder>) {
    let mut found = found
        .into_iter()
        .filter(Found::has_mail)
        .collect::<Vec<_>>();
    found.sort_by(|left, right| {
        (left.name.to_lowercase(), &left.name).cmp(&(right.name.to_lowercase(), &right.name))
    });
    for folder in found {
        let mut folder_path = path.to_vec();
        if !folder.name.is_empty() || !path.is_empty() {
            folder_path.push(folder.name);
        }
        folders.push(SourceFolder {
            kind: if folder_path.len() == 1 {
                FolderKind::of(&folder_path[0])
            } else {
                FolderKind::Other
            },
            path: folder_path.clone(),
            size: folder.size,
            parts: folder.parts,
        });
        flatten(folder.children, &folder_path, folders);
    }
}

fn sbd(name: &str) -> String {
    format!("{name}.sbd")
}

fn has_extension(name: &str, extension: &str) -> bool {
    strip_extension(name, extension).is_some()
}

/// `name` without the extension, compared without regard to case.
fn strip_extension<'a>(name: &'a str, extension: &str) -> Option<&'a str> {
    let (stem, found) = name.rsplit_once('.')?;
    (!stem.is_empty() && found.eq_ignore_ascii_case(extension)).then_some(stem)
}

fn strip_mbox_extension(name: &str) -> &str {
    strip_extension(name, "mbox")
        .or_else(|| strip_extension(name, "mbx"))
        .unwrap_or(name)
}
