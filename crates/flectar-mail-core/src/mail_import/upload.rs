//! Importing mail into the server of an IMAP, JMAP or Gmail account. Each
//! message is uploaded to a folder there, by IMAP APPEND, JMAP Email/import
//! or a Gmail import, with its date and its read and starred states, and the
//! account's sync then brings it into the app like any other mail.
//!
//! The mail always goes into a folder of its own, created where needed, so
//! it never mixes with the account's Inbox or Sent. A message the account
//! already has, here or in the server's folder, is skipped, so an upload
//! that stopped part way can be run again.

use super::write::{Incoming, Item, READ_AHEAD, folder_name, read_folders};
use super::{MailImportProgress, MailImportReport, MailSource, SourceFolder};
use crate::db::repo;
use crate::error::{CoreError, Result};
use crate::events::CoreEvent;
use crate::models::{AccountConfig, MailProtocol, roles};
use crate::sync::engine::SyncCmd;
use crate::{imap, jmap, queue, sync};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::{mpsc, watch};

/// Retries of a message whose upload failed for want of a connection.
const RETRIES: u32 = 4;

/// Mail to upload, from [`crate::Core::import_mail`].
pub(super) struct Upload {
    pub(super) account_id: i64,
    pub(super) protocol: MailProtocol,
    pub(super) source: MailSource,
    pub(super) chosen: Vec<usize>,
    /// The folder the source's folders go in, and its messages chosen one
    /// by one.
    pub(super) parent_folder: String,
}

/// A folder uploaded mail goes into.
#[derive(Debug, Clone)]
struct Target {
    folder_id: i64,
    path: String,
    jmap_id: Option<String>,
    gmail_label: Option<String>,
}

/// What became of one message.
enum Outcome {
    Uploaded,
    /// The account already has it.
    Present,
    /// The server refused it, for the reason given.
    Refused(String),
}

/// The upload's connection to the server, made when first needed.
enum Remote {
    Imap {
        session: Option<imap::Session>,
        selected: Option<i64>,
        /// Each folder's next UID before anything was uploaded to it.
        first_uids: HashMap<i64, i64>,
    },
    Jmap {
        client: Option<jmap::client::ConnectedClient>,
    },
    Gmail {
        uploader: sync::gmail::Uploader,
    },
}

impl crate::Core {
    pub(super) async fn upload_mail(
        &self,
        upload: Upload,
        progress: &watch::Sender<MailImportProgress>,
        stop: Arc<AtomicBool>,
    ) -> Result<MailImportReport> {
        let Upload {
            account_id,
            protocol,
            source,
            chosen,
            parent_folder,
        } = upload;
        let config = self.folder_account_config(account_id).await?;
        let mut remote = match protocol {
            MailProtocol::Imap => Remote::Imap {
                session: None,
                selected: None,
                first_uids: HashMap::new(),
            },
            MailProtocol::Jmap => Remote::Jmap { client: None },
            MailProtocol::Gmail => Remote::Gmail {
                uploader: sync::gmail::Uploader::new(self.sync_ctx(), config.clone())?,
            },
            MailProtocol::Pop3 | MailProtocol::Local => {
                return Err(CoreError::Other(
                    "mail kept on this device is not uploaded".into(),
                ));
            }
        };
        // Folders made elsewhere since the account last synced are used,
        // not made again.
        match &mut remote {
            // Folders are made over connections of their own, so this one
            // ends first: some servers allow few at a time.
            Remote::Imap { .. } => {
                let mut session = sync::engine::connect(&self.sync_ctx(), &config).await?;
                let listed =
                    sync::engine::discover_folders(&self.sync_ctx(), &config, &mut session).await;
                imap::logout(session).await;
                listed?;
            }
            Remote::Jmap { client } => {
                let client = self.jmap_client(&config, client).await?;
                jmap::sync::persist_session(&self.sync_ctx(), account_id, client).await?;
                jmap::sync::sync_mailboxes(&self.sync_ctx(), account_id, client).await?;
            }
            Remote::Gmail { uploader } => uploader.refresh_labels().await?,
        }
        let targets = self
            .upload_targets(account_id, &source.folders, &chosen, &parent_folder)
            .await?;

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
        let mut report = MailImportReport {
            account_id,
            folders: chosen
                .iter()
                .filter_map(|index| targets.get(index).map(|target| target.folder_id))
                .fold(Vec::new(), |mut folders, folder_id| {
                    if !folders.contains(&folder_id) {
                        folders.push(folder_id);
                    }
                    folders
                }),
            ..MailImportReport::default()
        };

        let (sender, mut receiver) = mpsc::channel(READ_AHEAD);
        let reader = {
            let stop = stop.clone();
            tokio::task::spawn_blocking(move || read_folders(&source, &chosen, &sender, &stop))
        };
        let result: Result<()> = async {
            while let Some(item) = receiver.recv().await {
                let (folder, read) = match item {
                    Item::Message(message) => {
                        let folder = message.folder;
                        let read = message.read;
                        if let Some(target) = targets.get(&folder) {
                            match self
                                .upload_message(&config, &mut remote, &message, target)
                                .await?
                            {
                                Outcome::Uploaded => report.imported += 1,
                                Outcome::Present => report.already_present += 1,
                                Outcome::Refused(reason) => {
                                    report.refused += 1;
                                    report.refusal.get_or_insert(reason);
                                }
                            }
                        }
                        (folder, read)
                    }
                    Item::TooLarge { folder, read } => {
                        report.too_large += 1;
                        (folder, read)
                    }
                    Item::Unreadable {
                        folder,
                        error,
                        read,
                    } => {
                        let path = paths.get(&folder).cloned().unwrap_or_default();
                        if !report.unreadable.iter().any(|(known, _)| *known == path) {
                            report.unreadable.push((path, error));
                        }
                        (folder, read)
                    }
                };
                progress.send_modify(|progress| {
                    progress.done_bytes = read.min(progress.total_bytes);
                    progress.imported = report.imported;
                    progress.folder = Some(folder);
                });
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
        let finished = self.finish_upload(account_id, &mut remote).await;
        result?;
        finished?;
        read.map_err(|error| CoreError::Other(format!("reading the mail failed: {error}")))?;
        report.stopped = stop.load(Ordering::Relaxed);
        Ok(report)
    }

    /// The folders the chosen source folders go into, created where they
    /// don't exist yet: a folder named `parent` at the top of the account,
    /// or in its Inbox where the top is closed, with the source's folders
    /// inside, and its messages chosen one by one in it.
    async fn upload_targets(
        &self,
        account_id: i64,
        folders: &[SourceFolder],
        chosen: &[usize],
        parent: &str,
    ) -> Result<HashMap<usize, Target>> {
        let account = self
            .db
            .read(move |conn| repo::accounts::get(conn, account_id))
            .await?
            .ok_or_else(|| CoreError::NotFound(format!("account {account_id}")))?;
        let inbox = self
            .db
            .read(move |conn| repo::folders::by_role(conn, account_id, roles::INBOX))
            .await?
            .map(|folder| folder.id);
        let parent = folder_name(parent);
        let root = if account.can_create_top_level_mailbox {
            match self.ensure_folder(account_id, None, &parent).await {
                Ok(root) => root,
                // Some IMAP servers keep every folder in the Inbox.
                Err(error) if !retryable(&error) => match inbox {
                    Some(inbox) => {
                        tracing::info!(account_id, %error, "putting uploaded mail in the Inbox");
                        self.ensure_folder(account_id, Some(inbox), &parent).await?
                    }
                    None => return Err(error),
                },
                Err(error) => return Err(error),
            }
        } else {
            let inbox = inbox.ok_or_else(|| {
                CoreError::Other("the account has no folder that can hold new folders".into())
            })?;
            self.ensure_folder(account_id, Some(inbox), &parent).await?
        };

        let mut targets = HashMap::new();
        for &index in chosen {
            let folder = &folders[index];
            if !folder.has_messages() {
                continue;
            }
            let mut target = root;
            for name in &folder.path {
                target = self
                    .ensure_folder(account_id, Some(target), &folder_name(name))
                    .await?;
            }
            let row = self
                .db
                .read(move |conn| {
                    let folder = repo::folders::get(conn, target)?
                        .ok_or_else(|| CoreError::NotFound(format!("folder {target}")))?;
                    let label = repo::gmail::provider_label_for_folder(conn, account_id, target)?;
                    Ok((folder, label))
                })
                .await?;
            let (folder, gmail_label) = row;
            targets.insert(
                index,
                Target {
                    folder_id: folder.id,
                    path: folder.path,
                    jmap_id: folder.jmap_id,
                    gmail_label,
                },
            );
        }
        Ok(targets)
    }

    /// The folder `name` in `parent`, created if it doesn't exist. A name
    /// with the server's hierarchy separator gets `_` in its place.
    async fn ensure_folder(&self, account_id: i64, parent: Option<i64>, name: &str) -> Result<i64> {
        let separator = self
            .db
            .read(move |conn| {
                Ok(match parent {
                    Some(parent) => {
                        repo::folders::get(conn, parent)?.and_then(|folder| folder.delimiter)
                    }
                    None => repo::folders::list(conn, Some(account_id))?
                        .into_iter()
                        .find_map(|folder| folder.delimiter),
                })
            })
            .await?
            .filter(|separator| separator.len() == 1 && separator.as_str() != " ");
        let name = match separator {
            Some(separator) => name.replace(separator.as_str(), "_"),
            None => name.to_owned(),
        };
        if let Some(folder) = self.child_folder(account_id, parent, &name).await? {
            return Ok(folder);
        }
        self.create_folder_in(account_id, parent, name).await
    }

    async fn upload_message(
        &self,
        config: &AccountConfig,
        remote: &mut Remote,
        message: &Incoming,
        target: &Target,
    ) -> Result<Outcome> {
        let message_id = message
            .parsed
            .headers
            .message_id
            .as_deref()
            .map(|id| id.trim().trim_matches(['<', '>']).to_owned())
            .filter(|id| !id.is_empty());
        // Mail the account has, here, isn't uploaded again.
        if let Some(message_id) = message_id.clone() {
            let account_id = config.id;
            let known = self
                .db
                .read(move |conn| repo::messages::by_message_id(conn, account_id, &message_id))
                .await?;
            if known.is_some() {
                return Ok(Outcome::Present);
            }
        }
        let seen = message.recorded.read.unwrap_or(true);
        let flagged = message.recorded.starred;
        let date_ms = message.parsed.headers.date_ms.or(message.stored_at);

        let mut attempt = 0;
        loop {
            let result = match remote {
                Remote::Imap {
                    session,
                    selected,
                    first_uids,
                } => {
                    self.imap_upload(
                        config,
                        session,
                        selected,
                        first_uids,
                        message,
                        target,
                        message_id.as_deref(),
                        imap::AppendOptions {
                            seen,
                            flagged,
                            internal_date: date_ms
                                .and_then(chrono::DateTime::from_timestamp_millis),
                        },
                    )
                    .await
                }
                Remote::Jmap { client } => {
                    self.jmap_upload(
                        config,
                        client,
                        message,
                        target,
                        message_id.as_deref(),
                        seen,
                        flagged,
                        date_ms,
                    )
                    .await
                }
                Remote::Gmail { uploader } => {
                    gmail_upload(
                        uploader,
                        message,
                        target,
                        message_id.as_deref(),
                        seen,
                        flagged,
                    )
                    .await
                }
            };
            match result {
                Err(error) if attempt < RETRIES && retryable(&error) => {
                    attempt += 1;
                    tracing::warn!(account_id = config.id, %error, attempt, "uploading a message failed; retrying");
                    match remote {
                        Remote::Imap {
                            session, selected, ..
                        } => {
                            *session = None;
                            *selected = None;
                        }
                        Remote::Jmap { client } => *client = None,
                        Remote::Gmail { .. } => {}
                    }
                    tokio::time::sleep(Duration::from_secs(2u64.pow(attempt).min(60))).await;
                }
                result => return result,
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn imap_upload(
        &self,
        config: &AccountConfig,
        session: &mut Option<imap::Session>,
        selected: &mut Option<i64>,
        first_uids: &mut HashMap<i64, i64>,
        message: &Incoming,
        target: &Target,
        message_id: Option<&str>,
        options: imap::AppendOptions,
    ) -> Result<Outcome> {
        if session.is_none() {
            *selected = None;
        }
        let session = self.imap_session(config, session).await?;
        if *selected != Some(target.folder_id) {
            let folder = imap::select(session, &target.path).await?;
            *selected = Some(target.folder_id);
            if let std::collections::hash_map::Entry::Vacant(entry) =
                first_uids.entry(target.folder_id)
            {
                let next = match folder.uid_next {
                    Some(next) => next,
                    None => imap::uid_search_all(session)
                        .await?
                        .into_iter()
                        .max()
                        .map_or(1, |uid| i64::from(uid) + 1),
                };
                entry.insert(next);
            }
        }
        if queue::uid_by_message_id(session, message_id)
            .await?
            .is_some()
        {
            return Ok(Outcome::Present);
        }
        match imap::append_message(session, &target.path, &message.raw, options).await {
            Ok(_) => Ok(Outcome::Uploaded),
            Err(CoreError::Imap(reason)) if !queue::is_connection_failure(&reason) => {
                Ok(Outcome::Refused(reason))
            }
            Err(error) => Err(error),
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn jmap_upload(
        &self,
        config: &AccountConfig,
        client: &mut Option<jmap::client::ConnectedClient>,
        message: &Incoming,
        target: &Target,
        message_id: Option<&str>,
        seen: bool,
        flagged: bool,
        date_ms: Option<i64>,
    ) -> Result<Outcome> {
        use jmap_client::email::import::EmailImportResponse;

        let mailbox = target
            .jmap_id
            .clone()
            .ok_or_else(|| CoreError::Jmap(format!("folder {} has no mailbox", target.path)))?;
        let client = self.jmap_client(config, client).await?;
        if let Some(message_id) = message_id {
            let filter = jmap_client::core::query::Filter::and([
                jmap_client::email::query::Filter::in_mailbox(mailbox.clone()),
                jmap_client::email::query::Filter::header(
                    "Message-ID",
                    Some(format!("<{message_id}>")),
                ),
            ]);
            let matches = client
                .email_query(filter)
                .await
                .map_err(jmap::client::map_error)?;
            if !matches.ids().is_empty() {
                return Ok(Outcome::Present);
            }
        }
        let limit = client
            .client
            .session()
            .core_capabilities()
            .map(|core| core.max_size_upload());
        if let Some(limit) = limit
            && message.raw.len() > limit
        {
            return Ok(Outcome::Refused(format!(
                "the server takes messages of up to {limit} bytes"
            )));
        }
        // A large message gets time to upload at 64 KiB/s.
        let usual = client.timeout;
        client.timeout = usual.max(Duration::from_secs(message.raw.len() as u64 / (64 * 1024)));
        let uploaded = client.upload(message.raw.clone(), "message/rfc822").await;
        client.timeout = usual;
        let blob_id = match uploaded {
            Ok(mut uploaded) => uploaded.take_blob_id(),
            // The server's own failures fail the import; it refused only
            // what it answers for with a client error.
            Err(jmap_client::Error::Problem(problem))
                if problem.status().is_none_or(|status| status < 500) =>
            {
                return Ok(Outcome::Refused(problem.to_string()));
            }
            Err(jmap_client::Error::Server(status)) if status.starts_with("413") => {
                return Ok(Outcome::Refused(status));
            }
            Err(error) => return Err(jmap::client::map_error(error)),
        };
        let mut keywords = Vec::new();
        if seen {
            keywords.push("$seen");
        }
        if flagged {
            keywords.push("$flagged");
        }
        let mut request = client.request();
        let import = request
            .import_email()
            .account_id(&client.account_id)
            .email(blob_id)
            .mailbox_ids([mailbox])
            .keywords(keywords);
        if let Some(date_ms) = date_ms {
            import.received_at(date_ms.div_euclid(1000));
        }
        let create_id = import.create_id();
        let mut response = client
            .send_single::<EmailImportResponse>(request)
            .await
            .map_err(jmap::client::map_error)?;
        match response.created(&create_id) {
            Ok(_) => Ok(Outcome::Uploaded),
            Err(jmap_client::Error::Set(error)) => match error.error() {
                jmap_client::core::set::SetErrorType::AlreadyExists => Ok(Outcome::Present),
                _ => Ok(Outcome::Refused(error.to_string())),
            },
            Err(error) => Err(jmap::client::map_error(error)),
        }
    }

    /// The upload's IMAP session, connected if it isn't.
    async fn imap_session<'a>(
        &self,
        config: &AccountConfig,
        session: &'a mut Option<imap::Session>,
    ) -> Result<&'a mut imap::Session> {
        if session.is_none() {
            *session = Some(sync::engine::connect(&self.sync_ctx(), config).await?);
        }
        session
            .as_mut()
            .ok_or_else(|| CoreError::Imap("no IMAP session".into()))
    }

    /// The upload's JMAP client, connected if it isn't.
    async fn jmap_client<'a>(
        &self,
        config: &AccountConfig,
        client: &'a mut Option<jmap::client::ConnectedClient>,
    ) -> Result<&'a mut jmap::client::ConnectedClient> {
        if client.is_none() {
            let secret = crate::accounts::credentials::load_async(
                self.credentials.clone(),
                config.id,
                crate::accounts::credentials::Slot::Password,
            )
            .await?;
            *client = Some(jmap::client::connect(config, &secret).await?);
        }
        client
            .as_mut()
            .ok_or_else(|| CoreError::Jmap("no JMAP session".into()))
    }

    /// End the upload's connection, let IMAP sync fetch what was uploaded to
    /// folders it hadn't synced yet, and have the account sync it now.
    async fn finish_upload(&self, account_id: i64, remote: &mut Remote) -> Result<()> {
        if let Remote::Imap {
            session,
            first_uids,
            ..
        } = remote
        {
            if let Some(session) = session.take() {
                imap::logout(session).await;
            }
            let first_uids = first_uids.clone();
            self.db
                .write(move |conn| {
                    let tx = conn.transaction()?;
                    for (folder_id, next) in first_uids {
                        repo::folders::lower_last_seen_uid(&tx, folder_id, next - 1)?;
                        // Its earlier mail, if any, is history to fetch.
                        if let Some(folder) = repo::folders::get(&tx, folder_id)?
                            && folder.backfill_cursor.is_none()
                        {
                            repo::folders::set_backfill(&tx, folder_id, Some(next), next <= 1)?;
                        }
                    }
                    tx.commit()?;
                    Ok(())
                })
                .await?;
        }
        self.bus.emit(CoreEvent::MailUpdated { thread_ids: vec![] });
        self.nudge(Some(account_id), || SyncCmd::SyncNow { complete: None })
            .await;
        Ok(())
    }
}

async fn gmail_upload(
    uploader: &sync::gmail::Uploader,
    message: &Incoming,
    target: &Target,
    message_id: Option<&str>,
    seen: bool,
    flagged: bool,
) -> Result<Outcome> {
    let label = target
        .gmail_label
        .as_deref()
        .ok_or_else(|| CoreError::Other(format!("folder {} has no Gmail label", target.path)))?;
    if let Some(message_id) = message_id
        && uploader.has_message(message_id).await?
    {
        return Ok(Outcome::Present);
    }
    let mut labels = vec![label];
    if !seen {
        labels.push("UNREAD");
    }
    if flagged {
        labels.push("STARRED");
    }
    match uploader.import(&message.raw, &labels).await {
        Ok(()) => Ok(Outcome::Uploaded),
        Err(CoreError::Other(reason)) => Ok(Outcome::Refused(reason)),
        Err(error) => Err(error),
    }
}

/// Whether trying again could help: the connection failed, or the server
/// asked to slow down.
fn retryable(error: &CoreError) -> bool {
    match error {
        CoreError::Network(_) | CoreError::Offline => true,
        CoreError::Imap(reason) | CoreError::Jmap(reason) => queue::is_connection_failure(reason),
        _ => false,
    }
}

#[cfg(test)]
#[path = "upload_tests.rs"]
mod tests;
