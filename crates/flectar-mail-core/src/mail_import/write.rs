//! The import itself. A reader on a blocking thread reads the chosen
//! folders' messages in order and parses them, while the import stores
//! their originals and files them in batches, each in one durable
//! transaction. Originals are on stable storage before the transaction that
//! refers to them commits, as for downloaded POP3 mail.

use super::scan::Tree;
use super::state::{self, Recorded};
use super::{FolderKind, Location, MailSource, ORIGINAL_SUFFIX, Part, SourceFolder};
use crate::db::repo::{self, sender_identities::OwnAddresses};
use crate::error::{CoreError, Result};
use crate::events::CoreEvent;
use crate::mbox;
use crate::mime::{ParsedBody, ParsedHeaders};
use crate::models::{MailProtocol, roles};
use crate::sync::ingest::{self, Delivery, Importer};
use futures::StreamExt;
use rusqlite::{Connection, params};
use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;

/// Messages stored in one transaction, at most.
const BATCH_MESSAGES: usize = 64;
/// Message bytes stored in one transaction, at most.
const BATCH_BYTES: usize = 32 * 1024 * 1024;
/// How long a batch waits to fill up before it is stored.
const BATCH_WAIT: Duration = Duration::from_millis(250);
/// Originals written at the same time.
const PARALLEL_WRITES: usize = 8;
/// Messages read and parsed ahead of those being stored.
pub(super) const READ_AHEAD: usize = 4;
const READ_BUFFER: usize = 256 * 1024;
/// The largest message imported. Larger ones are skipped, since originals
/// are read back with this limit.
const MAX_MESSAGE_BYTES: usize = crate::MAX_CACHED_MESSAGE_BYTES;

/// Where imported mail goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Destination {
    /// Local mail, which the first import creates.
    Local,
    /// The folders of an account: a POP3 account's, which exist only on this
    /// device, or those on the server of an IMAP, JMAP or Gmail account,
    /// which the mail is uploaded to.
    Account(i64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailImportRequest {
    pub source: MailSource,
    /// Indexes into `source.folders` of the folders to import.
    pub folders: Vec<usize>,
    pub destination: Destination,
    /// A folder to put the source's folders in, instead of next to the
    /// destination's own. Folders for a purpose stay ordinary folders in it.
    /// Mail uploaded to a server always goes in one, so it never mixes with
    /// the account's own mail: this one, or else one named
    /// `loose_folder_name`, which then also holds the messages chosen one by
    /// one.
    pub parent_folder: Option<String>,
    /// The name of the folder that messages chosen one by one go into.
    pub loose_folder_name: String,
    /// Addresses the mail's owner sent from, beside those of the accounts
    /// here, such as the identities of a Thunderbird profile. Mail from them
    /// is filed as sent.
    pub own_addresses: Vec<String>,
}

/// How far an import is. Bytes are those of the chosen folders' files.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MailImportProgress {
    pub done_bytes: u64,
    pub total_bytes: u64,
    pub imported: u64,
    /// The source folder being imported, an index into the source's
    /// folders.
    pub folder: Option<usize>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MailImportReport {
    /// The account the mail went into.
    pub account_id: i64,
    /// The folders it went into, in the order of the source's folders.
    pub folders: Vec<i64>,
    pub imported: u64,
    /// Messages the destination folder already had.
    pub already_present: u64,
    /// Messages larger than the largest message Flectar keeps.
    pub too_large: u64,
    /// Folders whose mail couldn't be read to the end, with the reason.
    pub unreadable: Vec<(Vec<String>, String)>,
    /// Whether the import was stopped before it was done. What it stored
    /// until then stays.
    pub stopped: bool,
    /// Messages a server refused, and the first reason it gave.
    pub refused: u64,
    pub refusal: Option<String>,
}

impl crate::Core {
    /// Copy the chosen folders of a source into a destination's folders,
    /// reporting progress as it goes. Setting `stop` ends the import after
    /// the batch being stored.
    pub async fn import_mail(
        &self,
        request: MailImportRequest,
        progress: watch::Sender<MailImportProgress>,
        stop: Arc<AtomicBool>,
    ) -> Result<MailImportReport> {
        let MailImportRequest {
            source,
            folders: mut chosen,
            destination,
            parent_folder,
            loose_folder_name,
            own_addresses,
        } = request;
        chosen.retain(|index| *index < source.folders.len());
        chosen.sort_unstable();
        chosen.dedup();
        let account_id = match destination {
            Destination::Local => self.ensure_local_mail().await?,
            Destination::Account(account_id) => account_id,
        };
        let protocol = self.account_protocol(account_id).await?;
        match protocol {
            MailProtocol::Pop3 | MailProtocol::Local => {}
            MailProtocol::Gmail | MailProtocol::Imap | MailProtocol::Jmap => {
                return self
                    .upload_mail(
                        super::upload::Upload {
                            account_id,
                            protocol,
                            source,
                            chosen,
                            parent_folder: parent_folder.unwrap_or(loose_folder_name),
                        },
                        &progress,
                        stop,
                    )
                    .await;
            }
        }

        let targets = {
            let folders = source.folders.clone();
            let chosen = chosen.clone();
            self.db
                .write(move |conn| {
                    let tx = conn.transaction()?;
                    let targets = create_folders(
                        &tx,
                        account_id,
                        protocol,
                        &folders,
                        &chosen,
                        parent_folder.as_deref(),
                        &loose_folder_name,
                    )?;
                    tx.commit()?;
                    Ok(targets)
                })
                .await?
        };
        self.bus.emit(CoreEvent::MailUpdated { thread_ids: vec![] });
        let own = self
            .db
            .read(move |conn| {
                let mut own = repo::sender_identities::own_addresses_of_all(conn)?;
                for address in own_addresses {
                    own.insert(address);
                }
                Ok(own)
            })
            .await?;

        let mut chosen_folders = Vec::new();
        for index in &chosen {
            if let Some(target) = targets.get(index)
                && !chosen_folders.contains(&target.folder_id)
            {
                chosen_folders.push(target.folder_id);
            }
        }
        let total_bytes = chosen.iter().map(|index| source.folders[*index].size).sum();
        progress.send_replace(MailImportProgress {
            done_bytes: 0,
            total_bytes,
            imported: 0,
            folder: None,
        });
        let paths = chosen
            .iter()
            .map(|index| (*index, source.folders[*index].path.clone()))
            .collect::<HashMap<_, _>>();
        let (sender, mut receiver) = mpsc::channel(READ_AHEAD);
        let reader = {
            let stop = stop.clone();
            tokio::task::spawn_blocking(move || read_folders(&source, &chosen, &sender, &stop))
        };

        let mut writer = Writer {
            core: self,
            account_id,
            // Mail a POP3 account gets this way is its own, like the mail it
            // downloads: it teaches contacts, invitations, and inbox tabs.
            downloaded: protocol == MailProtocol::Pop3,
            targets,
            paths,
            own,
            progress: &progress,
            report: MailImportReport {
                account_id,
                folders: chosen_folders,
                ..MailImportReport::default()
            },
        };
        let result: Result<()> = async {
            while let Some(first) = receiver.recv().await {
                let started = Instant::now();
                let mut bytes = first.bytes();
                let mut batch = vec![first];
                while batch.len() < BATCH_MESSAGES && bytes < BATCH_BYTES {
                    let wait = BATCH_WAIT.saturating_sub(started.elapsed());
                    match tokio::time::timeout(wait, receiver.recv()).await {
                        Ok(Some(item)) => {
                            bytes += item.bytes();
                            batch.push(item);
                        }
                        Ok(None) | Err(_) => break,
                    }
                }
                writer.store(batch).await?;
                if stop.load(Ordering::Relaxed) {
                    break;
                }
            }
            Ok(())
        }
        .await;
        // The reader stops once nothing receives its messages.
        drop(receiver);
        let read = reader.await;
        result?;
        read.map_err(|error| CoreError::Other(format!("reading the mail failed: {error}")))?;
        writer.report.stopped = stop.load(Ordering::Relaxed);
        Ok(writer.report)
    }

    /// Local mail's account, created the first time with its worker.
    pub(crate) async fn ensure_local_mail(&self) -> Result<i64> {
        let (account_id, created) = self
            .db
            .write(|conn| {
                let tx = conn.transaction()?;
                let existing = repo::local::id(&tx)?;
                let account_id = repo::local::ensure(&tx)?;
                tx.commit()?;
                Ok((account_id, existing.is_none()))
            })
            .await?;
        self.spawn_local_actor(account_id).await;
        if created {
            self.bus.emit(CoreEvent::MailUpdated { thread_ids: vec![] });
        }
        Ok(account_id)
    }
}

/// The folder a source folder's messages go into.
#[derive(Debug, Clone)]
struct Target {
    folder_id: i64,
    role: Option<String>,
}

/// Creates the destination folders of the chosen folders that hold mail,
/// with any folders above them. A folder that exists is reused, so a second
/// import of the same mail adds to the folders of the first.
fn create_folders(
    conn: &Connection,
    account_id: i64,
    protocol: MailProtocol,
    folders: &[SourceFolder],
    chosen: &[usize],
    parent_folder: Option<&str>,
    loose_folder_name: &str,
) -> Result<HashMap<usize, Target>> {
    let delimiter = repo::folders::DEVICE_DELIMITER;
    let mut existing = repo::folders::list(conn, Some(account_id))?
        .into_iter()
        .map(|folder| (folder.path.clone(), (folder.id, folder.role)))
        .collect::<HashMap<_, _>>();
    let mut targets = HashMap::new();
    for &index in chosen {
        let folder = &folders[index];
        if folder.parts.is_empty() {
            continue;
        }
        let mut names = if folder.path.is_empty() {
            vec![folder_name(loose_folder_name)]
        } else {
            folder.path.iter().map(|name| folder_name(name)).collect()
        };
        // The folders a mail app has for a purpose become this account's
        // folders for it, with their subfolders inside.
        if let Some(parent) = parent_folder {
            names.insert(0, folder_name(parent));
        } else if let Some(top) = folder.path.first()
            && let Some(role) = role_for(protocol, FolderKind::of(top))
            && let Some(role_folder) = repo::folders::by_role(conn, account_id, role)?
        {
            names[0] = role_folder.path;
        }
        let mut parent = None;
        let mut path = String::new();
        let mut target = None;
        for name in names {
            if !path.is_empty() {
                path.push_str(delimiter);
            }
            path.push_str(&name);
            let (folder_id, role) = match existing.get(&path) {
                Some(found) => found.clone(),
                None => {
                    let folder_id =
                        repo::folders::upsert(conn, account_id, &path, Some(delimiter), None)?;
                    repo::folders::set_parent(conn, folder_id, parent)?;
                    existing.insert(path.clone(), (folder_id, None));
                    (folder_id, None)
                }
            };
            parent = Some(folder_id);
            target = Some(Target { folder_id, role });
        }
        if let Some(target) = target {
            targets.insert(index, target);
        }
    }
    Ok(targets)
}

/// The role of the folder a mail app's folder for a purpose is imported
/// into. Local mail has only a Trash; Thunderbird's local Inbox, Sent and
/// others stay ordinary folders there, since they are no longer used.
fn role_for(protocol: MailProtocol, kind: FolderKind) -> Option<&'static str> {
    match (protocol, kind) {
        (MailProtocol::Local | MailProtocol::Pop3, FolderKind::Trash) => Some(roles::TRASH),
        (MailProtocol::Pop3, FolderKind::Inbox) => Some(roles::INBOX),
        (MailProtocol::Pop3, FolderKind::Drafts) => Some(roles::DRAFTS),
        (MailProtocol::Pop3, FolderKind::Sent) => Some(roles::SENT),
        (MailProtocol::Pop3, FolderKind::Archive) => Some(roles::ARCHIVE),
        (MailProtocol::Pop3, FolderKind::Junk) => Some(roles::SPAM),
        _ => None,
    }
}

/// A folder name a path level can hold: the separator and control
/// characters become `_`, and it has a length a folder name may have.
pub(super) fn folder_name(name: &str) -> String {
    let name = name
        .trim()
        .chars()
        .map(|character| match character {
            '/' => '_',
            character if character.is_control() => '_',
            character => character,
        })
        .take(255)
        .collect::<String>();
    if name.trim().is_empty() {
        "Folder".to_owned()
    } else {
        name
    }
}

/// What the reader sends.
pub(super) enum Item {
    Message(Box<Incoming>),
    TooLarge {
        folder: usize,
        read: u64,
    },
    /// A folder whose mail couldn't be read to the end.
    Unreadable {
        folder: usize,
        error: String,
        read: u64,
    },
}

impl Item {
    fn bytes(&self) -> usize {
        match self {
            Self::Message(message) => message.raw.len(),
            Self::TooLarge { .. } | Self::Unreadable { .. } => 0,
        }
    }
}

pub(super) struct Incoming {
    pub(super) folder: usize,
    pub(super) raw: Vec<u8>,
    pub(super) stored_at: Option<i64>,
    pub(super) recorded: Recorded,
    pub(super) parsed: ParsedBody,
    /// Bytes of the chosen folders read once this message was.
    pub(super) read: u64,
}

/// Why reading a folder's file ended early.
enum Halt {
    /// The import stopped, or ended with an error.
    Stopped,
    Failed(String),
}

/// Reads the chosen folders' messages in order, until all are read, the
/// import is stopped, or the import no longer receives them.
pub(super) fn read_folders(
    source: &MailSource,
    chosen: &[usize],
    sender: &mpsc::Sender<Item>,
    stop: &AtomicBool,
) {
    let mut tree = match Tree::for_source(source) {
        Ok(tree) => tree,
        Err(error) => {
            for &folder in chosen {
                let item = Item::Unreadable {
                    folder,
                    error: error.to_string(),
                    read: 0,
                };
                if sender.blocking_send(item).is_err() {
                    return;
                }
            }
            return;
        }
    };
    let mut read = 0;
    for &folder in chosen {
        let start = read;
        for part in &source.folders[folder].parts {
            let result = match part {
                Part::Mbox(location) => {
                    read_mbox(&mut tree, location, folder, &mut read, sender, stop)
                }
                Part::Message(location) => {
                    read_file(&mut tree, location, folder, &mut read, sender, stop)
                }
            };
            match result {
                Ok(()) => {}
                Err(Halt::Stopped) => return,
                // The folder's other files may still be readable.
                Err(Halt::Failed(error)) => {
                    let item = Item::Unreadable {
                        folder,
                        error,
                        read,
                    };
                    if sender.blocking_send(item).is_err() {
                        return;
                    }
                }
            }
        }
        read = start + source.folders[folder].size;
    }
}

fn read_mbox(
    tree: &mut Tree,
    location: &Location,
    folder: usize,
    read: &mut u64,
    sender: &mpsc::Sender<Item>,
    stop: &AtomicBool,
) -> std::result::Result<(), Halt> {
    let file = tree
        .open(location)
        .map_err(|error| Halt::Failed(error.to_string()))?;
    let mut reader = mbox::Reader::new(
        std::io::BufReader::with_capacity(READ_BUFFER, file),
        MAX_MESSAGE_BYTES,
    );
    let start = *read;
    loop {
        if stop.load(Ordering::Relaxed) {
            return Err(Halt::Stopped);
        }
        let entry = reader
            .next_entry()
            .map_err(|error| Halt::Failed(error.to_string()))?;
        *read = start + reader.position();
        let item = match entry {
            None => return Ok(()),
            Some(mbox::Entry::TooLarge) => Item::TooLarge {
                folder,
                read: *read,
            },
            Some(mbox::Entry::Message { raw, stored_at }) => {
                match incoming(folder, raw, stored_at, None, *read) {
                    Some(message) => Item::Message(Box::new(message)),
                    None => continue,
                }
            }
        };
        sender.blocking_send(item).map_err(|_| Halt::Stopped)?;
    }
}

fn read_file(
    tree: &mut Tree,
    location: &Location,
    folder: usize,
    read: &mut u64,
    sender: &mpsc::Sender<Item>,
    stop: &AtomicBool,
) -> std::result::Result<(), Halt> {
    if stop.load(Ordering::Relaxed) {
        return Err(Halt::Stopped);
    }
    let mut raw = Vec::new();
    tree.open(location)
        .and_then(|file| {
            file.take(MAX_MESSAGE_BYTES as u64 + 1)
                .read_to_end(&mut raw)
        })
        .map_err(|error| Halt::Failed(error.to_string()))?;
    *read += raw.len() as u64;
    let item = if raw.len() > MAX_MESSAGE_BYTES {
        Item::TooLarge {
            folder,
            read: *read,
        }
    } else {
        match incoming(folder, raw, None, location.file_name(), *read) {
            Some(message) => Item::Message(Box::new(message)),
            None => return Ok(()),
        }
    };
    sender.blocking_send(item).map_err(|_| Halt::Stopped)
}

/// A message to store, unless it is empty or its app had deleted it.
pub(super) fn incoming(
    folder: usize,
    raw: Vec<u8>,
    stored_at: Option<i64>,
    file_name: Option<String>,
    read: u64,
) -> Option<Incoming> {
    if raw.iter().all(u8::is_ascii_whitespace) {
        return None;
    }
    let recorded = state::recorded(&raw, file_name.as_deref());
    if recorded.deleted {
        return None;
    }
    let parsed = ingest::parse_or_text(&raw);
    Some(Incoming {
        folder,
        raw,
        stored_at,
        recorded,
        parsed,
        read,
    })
}

/// How a folder recognizes a message it already has: by Message-ID, or
/// without one by its date, sender, subject and size.
#[derive(Debug, Clone)]
enum Key {
    MessageId(String),
    Fields {
        date: i64,
        from: String,
        subject: String,
        size: i64,
    },
    /// Nothing to recognize it by.
    None,
}

impl Key {
    fn of(headers: &ParsedHeaders, stored_at: Option<i64>, size: usize) -> Self {
        if let Some(message_id) = &headers.message_id {
            return Self::MessageId(message_id.clone());
        }
        match headers.date_ms.or(stored_at) {
            Some(date) => Self::Fields {
                date,
                from: headers
                    .from
                    .as_ref()
                    .map(|from| from.email.clone())
                    .unwrap_or_default(),
                subject: headers.subject.clone(),
                size: i64::try_from(size).unwrap_or(i64::MAX),
            },
            None => Self::None,
        }
    }

    fn present(&self, conn: &Connection, account_id: i64, folder_id: i64) -> Result<bool> {
        Ok(match self {
            Self::MessageId(message_id) => conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM messages
                 WHERE account_id = ?1 AND message_id = ?2 AND folder_id = ?3)",
                params![account_id, message_id, folder_id],
                |row| row.get(0),
            )?,
            Self::Fields {
                date,
                from,
                subject,
                size,
            } => conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM messages
                 WHERE folder_id = ?1 AND message_id IS NULL AND date = ?2
                   AND COALESCE(from_addr, '') = ?3 AND subject = ?4 AND size = ?5)",
                params![folder_id, date, from, subject, size],
                |row| row.get(0),
            )?,
            Self::None => false,
        })
    }
}

struct Writer<'a> {
    core: &'a crate::Core,
    account_id: i64,
    downloaded: bool,
    targets: HashMap<usize, Target>,
    paths: HashMap<usize, Vec<String>>,
    own: OwnAddresses,
    progress: &'a watch::Sender<MailImportProgress>,
    report: MailImportReport,
}

/// A message whose original was written, waiting for its transaction.
struct Written {
    message: Incoming,
    target: Target,
    key: Key,
    path: PathBuf,
}

impl Writer<'_> {
    async fn store(&mut self, batch: Vec<Item>) -> Result<()> {
        let mut read = None;
        let mut current = None;
        let mut messages = Vec::new();
        for item in batch {
            match item {
                Item::Message(message) => {
                    read = Some(message.read);
                    current = Some(message.folder);
                    messages.push(*message);
                }
                Item::TooLarge { folder, read: done } => {
                    read = Some(done);
                    current = Some(folder);
                    self.report.too_large += 1;
                }
                Item::Unreadable {
                    folder,
                    error,
                    read: done,
                } => {
                    read = Some(done);
                    current = Some(folder);
                    // The first problem of a folder stands for the rest.
                    let path = self.paths.get(&folder).cloned().unwrap_or_default();
                    if !self
                        .report
                        .unreadable
                        .iter()
                        .any(|(known, _)| *known == path)
                    {
                        self.report.unreadable.push((path, error));
                    }
                }
            }
        }

        // What the folders have is skipped before its original is written.
        let mut candidates = Vec::new();
        for message in messages {
            let Some(target) = self.targets.get(&message.folder).cloned() else {
                continue;
            };
            let key = Key::of(
                &message.parsed.headers,
                message.stored_at,
                message.raw.len(),
            );
            candidates.push((message, target, key));
        }
        let account_id = self.account_id;
        let checks = candidates
            .iter()
            .map(|(_, target, key)| (target.folder_id, key.clone()))
            .collect::<Vec<_>>();
        let present = self
            .core
            .db
            .read(move |conn| {
                checks
                    .iter()
                    .map(|(folder_id, key)| key.present(conn, account_id, *folder_id))
                    .collect::<Result<Vec<_>>>()
            })
            .await?;
        let mut new = Vec::new();
        for (candidate, present) in candidates.into_iter().zip(present) {
            if present {
                self.report.already_present += 1;
            } else {
                new.push(candidate);
            }
        }

        let directory = self.core.paths.mail_dir(account_id);
        let written = futures::stream::iter(new.into_iter().map(|(message, target, key)| {
            let path = directory.join(format!("{:032x}{ORIGINAL_SUFFIX}", rand::random::<u128>()));
            async move {
                match crate::file_io::write_durable(&path, &message.raw, "imported message").await {
                    Ok(()) => Ok(Written {
                        message,
                        target,
                        key,
                        path,
                    }),
                    Err(error) => Err((error, path)),
                }
            }
        }))
        .buffered(PARALLEL_WRITES)
        .collect::<Vec<_>>()
        .await;
        let mut stored = Vec::new();
        let mut failure = None;
        for outcome in written {
            match outcome {
                Ok(written) => stored.push(written),
                Err((error, path)) => {
                    let _ = tokio::fs::remove_file(&path).await;
                    failure.get_or_insert(error);
                }
            }
        }
        if let Some(error) = failure {
            for written in &stored {
                let _ = tokio::fs::remove_file(&written.path).await;
            }
            return Err(error);
        }

        let originals = stored
            .iter()
            .map(|written| written.path.clone())
            .collect::<Vec<_>>();
        let own = self.own.clone();
        let downloaded = self.downloaded;
        let committed = self
            .core
            .db
            .write_durable(move |tx| {
                let mut importer = Importer::with_own_addresses(tx, account_id, own)?;
                let mut inserted = Vec::new();
                let mut duplicates = Vec::new();
                for Written {
                    message,
                    target,
                    key,
                    path,
                } in stored
                {
                    // Another import may have stored the same mail since.
                    if key.present(tx, account_id, target.folder_id)? {
                        duplicates.push(path);
                        continue;
                    }
                    let Incoming {
                        raw,
                        stored_at,
                        recorded,
                        parsed,
                        ..
                    } = message;
                    let imported = importer.insert(
                        tx,
                        &parsed.headers,
                        &Delivery {
                            folder_id: target.folder_id,
                            folder_role: target.role.as_deref(),
                            uid: None,
                            received_ms: stored_at,
                            // Saved mail without a record of being read was
                            // most likely read long ago.
                            seen: recorded.read.unwrap_or(true),
                            flagged: recorded.starred,
                            draft: false,
                            has_attachments: parsed
                                .attachments
                                .iter()
                                .any(|attachment| !attachment.is_inline),
                            size: i64::try_from(raw.len()).ok(),
                        },
                    )?;
                    ingest::store_body(tx, imported.message_id, &path.to_string_lossy(), &parsed)?;
                    repo::threads::recompute(tx, imported.thread_id)?;
                    if downloaded {
                        importer.learn_contacts(tx, &parsed.headers, &imported)?;
                    }
                    inserted.push((imported.message_id, ingest::calendar_events(&parsed)));
                }
                let thread_ids = if downloaded {
                    importer.finish(tx)?.0
                } else {
                    importer.finish_unrouted()
                };
                Ok((inserted, duplicates, thread_ids))
            })
            .await;
        let (inserted, duplicates, thread_ids) = match committed {
            Ok(committed) => committed,
            Err(error) => {
                for path in &originals {
                    let _ = tokio::fs::remove_file(path).await;
                }
                return Err(error);
            }
        };
        for path in &duplicates {
            let _ = tokio::fs::remove_file(path).await;
        }
        self.report.already_present += duplicates.len() as u64;
        self.report.imported += inserted.len() as u64;
        if self.downloaded {
            for (message_id, events) in inserted {
                if let Err(error) = ingest::store_calendar_events(
                    &self.core.sync_ctx(),
                    account_id,
                    message_id,
                    events,
                )
                .await
                {
                    tracing::warn!(account_id, message_id, %error, "could not record an imported message's invitations");
                }
            }
        }
        if !thread_ids.is_empty() {
            self.core.bus.emit(CoreEvent::MailUpdated { thread_ids });
        }
        self.progress.send_modify(|progress| {
            if let Some(read) = read {
                progress.done_bytes = read.min(progress.total_bytes);
            }
            if current.is_some() {
                progress.folder = current;
            }
            progress.imported = self.report.imported;
        });
        Ok(())
    }
}
