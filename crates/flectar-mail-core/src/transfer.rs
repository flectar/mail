//! Copying and moving mail to a folder of another account.
//!
//! Each message becomes a row of `mail_transfers` (see
//! [`repo::transfers`]) whose stages are each committed before the next
//! starts, so a transfer carries on after a restart. Its raw message is
//! fetched from the source into a file of its own, then uploaded to the
//! target folder the way an import uploads mail: with its date and its read
//! and starred states, and not where the account has it already. A move
//! then removes the source, only once the copy is in place.

use crate::db::repo::{
    self,
    transfers::{NewTransfer, Stage, Transfer},
};
use crate::error::{CoreError, Result};
use crate::events::{CoreEvent, TransferReport};
use crate::mail_import::{Uploaded, Uploads};
use crate::models::{ActionKind, ActionTarget, MailProtocol, PerformActionArgs, now_ms, roles};
use crate::{imap, jmap, queue, sync};
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::HashMap;
use std::time::Duration;

/// Tries at a stage that failed for want of a connection before the
/// transfer gives up.
const ATTEMPTS: i64 = 8;
/// The wait after the first such failure, doubling with each one after.
const FIRST_WAIT_MS: i64 = 30_000;
const LONGEST_WAIT_MS: i64 = 30 * 60_000;

/// What a run of transfers keeps open from one message to the next, and
/// what became of them.
#[derive(Default)]
struct Run {
    uploads: Uploads,
    /// IMAP sessions fetching from source accounts, with the folder they
    /// have selected and whether its UIDs are those this app knows.
    imap: HashMap<i64, (imap::Session, Option<(i64, bool)>)>,
    jmap: HashMap<i64, jmap::client::ConnectedClient>,
    gmail: HashMap<i64, sync::gmail::Uploader>,
    report: TransferReport,
}

/// What a stage leads to.
enum Next {
    Stage(Stage, Option<String>),
    /// The transfer is done; whether the target had the message already.
    Done {
        present: bool,
    },
}

impl crate::Core {
    /// Copies or moves mail to a folder of another account. Mail of the
    /// folder's own account and drafts are left out. Returns how many
    /// messages are on their way.
    pub async fn transfer_mail(
        &self,
        target: ActionTarget,
        folder_id: i64,
        is_move: bool,
    ) -> Result<usize> {
        let now = now_ms();
        let started = self
            .db
            .write(move |conn| {
                let tx = conn.transaction()?;
                let folder = repo::folders::get(&tx, folder_id)?
                    .ok_or_else(|| CoreError::NotFound(format!("folder {folder_id}")))?;
                if !folder.selectable {
                    return Err(CoreError::Other(format!(
                        "{} is a folder for other folders",
                        folder.path
                    )));
                }
                let mut started = 0;
                for mut source in transfer_sources(&tx, &target)? {
                    if source.source_account_id == folder.account_id {
                        continue;
                    }
                    source.target_folder_id = folder_id;
                    source.is_move = is_move;
                    if repo::transfers::insert(&tx, &source, now)?.is_some() {
                        started += 1;
                    }
                }
                if started > 0 {
                    repo::folders::mark_moved_to(&tx, folder_id, now)?;
                }
                tx.commit()?;
                Ok(started)
            })
            .await?;
        if started > 0 {
            self.transfers.notify_one();
        }
        Ok(started)
    }

    /// Long-lived loop: runs transfers as they come due, one at a time, and
    /// once none is due ends the run, which closes its connections and
    /// reports what became of it.
    pub(crate) async fn transfer_loop(&self) {
        use tokio::sync::broadcast::error::RecvError;
        self.remove_stray_transfer_files().await;
        let mut events = self.bus.subscribe();
        let mut run = None::<Run>;
        loop {
            let now = now_ms();
            match self
                .db
                .read(move |conn| repo::transfers::next_due(conn, now))
                .await
            {
                Ok(Some(transfer)) => {
                    self.run_transfer(run.get_or_insert_with(Run::default), transfer)
                        .await;
                    continue;
                }
                Ok(None) => {}
                Err(error) => tracing::warn!(%error, "reading the mail transfers failed"),
            }
            if let Some(run) = run.take() {
                self.end_run(run).await;
            }
            let wait = match self
                .db
                .read(|conn| repo::transfers::next_attempt_at(conn))
                .await
            {
                Ok(Some(at)) => Duration::from_millis(u64::try_from(at - now_ms()).unwrap_or(0))
                    .max(Duration::from_secs(1)),
                _ => Duration::from_secs(60 * 60),
            };
            // Coming back online retries what waited for a connection.
            let online = async {
                loop {
                    match events.recv().await {
                        Ok(CoreEvent::NetworkState { online: true }) => return true,
                        Err(RecvError::Closed) => return false,
                        _ => {}
                    }
                }
            };
            tokio::select! {
                () = self.transfers.notified() => {}
                () = tokio::time::sleep(wait) => {}
                online = online => {
                    if !online {
                        return;
                    }
                    if let Err(error) = self.db.write(|conn| repo::transfers::retry_now(conn)).await {
                        tracing::warn!(%error, "retrying the mail transfers failed");
                    }
                }
            }
        }
    }

    async fn run_transfer(&self, run: &mut Run, transfer: Transfer) {
        let id = transfer.id;
        let next = match transfer.stage {
            Stage::Fetch => self
                .fetch_original(run, &transfer)
                .await
                .map(|path| Next::Stage(Stage::Upload, Some(path))),
            Stage::Upload => {
                self.upload_copy(run, &transfer)
                    .await
                    .map(|present| match transfer.is_move {
                        true => Next::Stage(Stage::Remove, None),
                        false => Next::Done { present },
                    })
            }
            Stage::Remove => self
                .remove_original(&transfer)
                .await
                .map(|()| Next::Done { present: false }),
        };
        let result = match next {
            Ok(Next::Stage(stage, path)) => {
                self.db
                    .write(move |conn| repo::transfers::advance(conn, id, stage, path.as_deref()))
                    .await
            }
            Ok(Next::Done { present }) => {
                if present {
                    run.report.present += 1;
                } else if transfer.is_move {
                    run.report.moved += 1;
                } else {
                    run.report.copied += 1;
                }
                self.end_transfer(&transfer).await
            }
            Err(error) => {
                // A connection that failed is made again for the next try.
                run.imap.remove(&transfer.source_account_id);
                run.jmap.remove(&transfer.source_account_id);
                if retryable(&error) && transfer.attempts + 1 < ATTEMPTS {
                    let wait = FIRST_WAIT_MS
                        .saturating_mul(1 << transfer.attempts.clamp(0, 16))
                        .min(LONGEST_WAIT_MS);
                    let at = now_ms() + wait;
                    let reason = error.to_string();
                    tracing::warn!(id, %error, attempt = transfer.attempts + 1, "a mail transfer failed; trying again");
                    self.db
                        .write(move |conn| repo::transfers::retry_at(conn, id, at, &reason))
                        .await
                } else {
                    tracing::warn!(id, %error, "a mail transfer failed");
                    run.report.failed += 1;
                    run.report.error.get_or_insert_with(|| error.to_string());
                    self.end_transfer(&transfer).await
                }
            }
        };
        if let Err(error) = result {
            tracing::warn!(id, %error, "recording a mail transfer failed");
        }
    }

    /// The transfer's row and file go.
    async fn end_transfer(&self, transfer: &Transfer) -> Result<()> {
        let id = transfer.id;
        self.db
            .write(move |conn| repo::transfers::finish(conn, id))
            .await?;
        if let Some(path) = &transfer.staged_path {
            let _ = tokio::fs::remove_file(path).await;
        }
        Ok(())
    }

    /// Closes the run's connections, has the accounts that got mail sync it,
    /// and reports what became of the run's transfers.
    async fn end_run(&self, run: Run) {
        let Run {
            uploads,
            imap,
            report,
            ..
        } = run;
        for (session, _) in imap.into_values() {
            imap::logout(session).await;
        }
        if let Err(error) = self.finish_uploads(uploads).await {
            tracing::warn!(%error, "finishing the uploads of mail transfers failed");
        }
        if report != TransferReport::default() {
            self.bus.emit(CoreEvent::TransfersFinished(report));
        }
    }

    /// Fetches the source's raw message into the transfer's file, from where
    /// this app keeps it when it does, and otherwise from its server.
    async fn fetch_original(&self, run: &mut Run, transfer: &Transfer) -> Result<String> {
        let message_id = transfer
            .source_message_id
            .ok_or_else(|| CoreError::NotFound("the message".into()))?;
        let source = self
            .db
            .read(move |conn| Source::get(conn, message_id))
            .await?
            .ok_or_else(|| CoreError::NotFound("the message".into()))?;
        let raw = match source.full_original() {
            Some(path) => {
                self.read_original(
                    source.account_id,
                    message_id,
                    path,
                    crate::MAX_CACHED_MESSAGE_BYTES,
                    "message to transfer",
                )
                .await?
            }
            None => match source.protocol {
                MailProtocol::Imap => self.fetch_from_imap(run, &source).await?,
                MailProtocol::Jmap => self.fetch_from_jmap(run, &source).await?,
                MailProtocol::Gmail => self.fetch_from_gmail(run, &source).await?,
                MailProtocol::Pop3 | MailProtocol::Local => {
                    return Err(CoreError::NotFound("the message's original".into()));
                }
            },
        };
        if raw.len() > crate::MAX_CACHED_MESSAGE_BYTES {
            return Err(CoreError::Other(
                "the message is too large to transfer".into(),
            ));
        }
        let directory = self.paths.transfers_dir();
        tokio::fs::create_dir_all(&directory).await?;
        let path = directory.join(format!("{}.eml", transfer.id));
        crate::file_io::write_durable(&path, &raw, "message being transferred").await?;
        crate::file_io::restrict_permissions(&directory, &[&path])?;
        Ok(path.to_string_lossy().into_owned())
    }

    async fn fetch_from_imap(&self, run: &mut Run, source: &Source) -> Result<Vec<u8>> {
        let folder_id = source
            .folder_id
            .ok_or_else(|| CoreError::NotFound("the message's folder".into()))?;
        let folder = self
            .db
            .read(move |conn| repo::folders::get(conn, folder_id))
            .await?
            .ok_or_else(|| CoreError::NotFound("the message's folder".into()))?;
        let (session, selected) = match run.imap.entry(source.account_id) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => {
                let config = self.folder_account_config(source.account_id).await?;
                let session = sync::engine::connect(&self.sync_ctx(), &config).await?;
                entry.insert((session, None))
            }
        };
        let current = match *selected {
            Some((selected_id, current)) if selected_id == folder_id => current,
            _ => {
                let mailbox = imap::select(session, &folder.path).await?;
                let current =
                    folder.uidvalidity.is_some() && folder.uidvalidity == mailbox.uid_validity;
                *selected = Some((folder_id, current));
                current
            }
        };
        let uid = match source
            .uid
            .filter(|_| current)
            .and_then(|uid| u32::try_from(uid).ok())
            .filter(|uid| *uid > 0)
        {
            Some(uid) => Some(uid),
            None => queue::uid_by_message_id(session, source.message_id.as_deref()).await?,
        };
        let uid = uid.ok_or_else(|| CoreError::NotFound("the message on the server".into()))?;
        imap::fetch_full(session, uid)
            .await?
            .ok_or_else(|| CoreError::NotFound("the message on the server".into()))
    }

    async fn fetch_from_jmap(&self, run: &mut Run, source: &Source) -> Result<Vec<u8>> {
        let blob_id = source
            .jmap_blob_id
            .clone()
            .ok_or_else(|| CoreError::NotFound("the message on the server".into()))?;
        let client = match run.jmap.entry(source.account_id) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => {
                let config = self.folder_account_config(source.account_id).await?;
                let secret = crate::accounts::credentials::load_async(
                    self.credentials.clone(),
                    config.id,
                    crate::accounts::credentials::Slot::Password,
                )
                .await?;
                entry.insert(jmap::client::connect(&config, &secret).await?)
            }
        };
        jmap::sync::download_message_bounded(client, &blob_id).await
    }

    async fn fetch_from_gmail(&self, run: &mut Run, source: &Source) -> Result<Vec<u8>> {
        let provider_id = source
            .gm_msgid
            .clone()
            .ok_or_else(|| CoreError::NotFound("the message on the server".into()))?;
        let uploader = match run.gmail.entry(source.account_id) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => {
                let config = self.folder_account_config(source.account_id).await?;
                entry.insert(sync::gmail::Uploader::new(self.sync_ctx(), config)?)
            }
        };
        let size = source
            .size
            .and_then(|size| u64::try_from(size).ok())
            .unwrap_or(0);
        uploader.raw_message(&provider_id, size).await
    }

    /// Puts the transfer's file in the target folder. Returns whether the
    /// target's account had the message already.
    async fn upload_copy(&self, run: &mut Run, transfer: &Transfer) -> Result<bool> {
        let path = transfer
            .staged_path
            .clone()
            .ok_or_else(|| CoreError::NotFound("the fetched message".into()))?;
        let raw = crate::file_io::read(
            &path,
            crate::MAX_CACHED_MESSAGE_BYTES,
            "message being transferred",
        )
        .await?;
        let folder_id = transfer.target_folder_id;
        let protocol = self
            .db
            .read(move |conn| {
                let folder = repo::folders::get(conn, folder_id)?
                    .ok_or_else(|| CoreError::NotFound("the folder".into()))?;
                repo::accounts::protocol(conn, folder.account_id)?
                    .ok_or_else(|| CoreError::NotFound("the folder's account".into()))
            })
            .await?;
        match protocol {
            MailProtocol::Pop3 | MailProtocol::Local => Ok(!self
                .store_in_folder(
                    folder_id,
                    raw,
                    transfer.seen,
                    transfer.flagged,
                    transfer.received_at,
                )
                .await?),
            MailProtocol::Imap | MailProtocol::Jmap | MailProtocol::Gmail => {
                match self
                    .upload_to_folder(
                        &mut run.uploads,
                        folder_id,
                        raw,
                        transfer.seen,
                        transfer.flagged,
                        transfer.received_at,
                    )
                    .await?
                {
                    Uploaded::Copied => Ok(false),
                    Uploaded::Present => Ok(true),
                    Uploaded::Refused(reason) => Err(CoreError::Other(format!(
                        "the server refused the message: {reason}"
                    ))),
                }
            }
        }
    }

    /// Removes the source of a move, now that the copy is in place, the way
    /// its platform removes the original of a move between accounts: IMAP
    /// and JMAP delete it (see `remove_transferred` in the queue), Gmail
    /// moves it to Trash as deleting it there does, and mail kept only on
    /// this device goes from it.
    async fn remove_original(&self, transfer: &Transfer) -> Result<()> {
        let Some(message_id) = transfer.source_message_id else {
            // Gone already.
            return Ok(());
        };
        let Some(source) = self
            .db
            .read(move |conn| Source::get(conn, message_id))
            .await?
        else {
            return Ok(());
        };
        match source.protocol {
            MailProtocol::Imap | MailProtocol::Jmap => {
                let account_id = source.account_id;
                let queued = self
                    .db
                    .write(move |conn| {
                        let tx = conn.transaction()?;
                        let (folder_id, uid, thread_id) = tx.query_row(
                            "SELECT folder_id, uid, thread_id FROM messages WHERE id = ?1",
                            params![message_id],
                            |row| {
                                Ok((
                                    row.get::<_, Option<i64>>(0)?,
                                    row.get::<_, Option<i64>>(1)?,
                                    row.get::<_, Option<i64>>(2)?,
                                ))
                            },
                        )?;
                        let queued: bool = tx.query_row(
                            "SELECT EXISTS(SELECT 1 FROM pending_actions
                             WHERE message_id = ?1 AND kind = 'remove_transferred'
                               AND state IN ('pending','inflight'))",
                            params![message_id],
                            |row| row.get(0),
                        )?;
                        if !queued {
                            repo::actions::enqueue(
                                &tx,
                                account_id,
                                "remove_transferred",
                                Some(message_id),
                                thread_id,
                                &serde_json::json!({ "folderId": folder_id, "uid": uid }),
                                None,
                            )?;
                        }
                        tx.commit()?;
                        Ok(!queued)
                    })
                    .await?;
                if queued {
                    self.nudge(Some(account_id), || sync::engine::SyncCmd::RunActions)
                        .await;
                }
                Ok(())
            }
            MailProtocol::Gmail => {
                let in_trash = self
                    .db
                    .read(move |conn| {
                        Ok(conn
                            .query_row(
                                "SELECT f.role FROM messages m JOIN folders f ON f.id = m.folder_id
                                 WHERE m.id = ?1",
                                params![message_id],
                                |row| row.get::<_, Option<String>>(0),
                            )
                            .optional()?
                            .flatten()
                            .as_deref()
                            == Some(roles::TRASH))
                    })
                    .await?;
                self.perform_action(PerformActionArgs {
                    kind: if in_trash {
                        ActionKind::DeletePermanently
                    } else {
                        ActionKind::Trash
                    },
                    target: ActionTarget::Messages(vec![message_id]),
                    params: None,
                })
                .await
                .map(|_| ())
            }
            MailProtocol::Pop3 | MailProtocol::Local => {
                sync::finish_permanent_delete(&self.sync_ctx(), Some(message_id)).await
            }
        }
    }

    /// Files left in the transfers folder by transfers that ended while the
    /// app was closing.
    async fn remove_stray_transfer_files(&self) {
        let staged = match self
            .db
            .read(|conn| repo::transfers::staged_paths(conn))
            .await
        {
            Ok(staged) => staged
                .into_iter()
                .map(std::path::PathBuf::from)
                .collect::<std::collections::HashSet<_>>(),
            Err(error) => {
                tracing::warn!(%error, "listing the mail transfers failed");
                return;
            }
        };
        let Ok(mut files) = tokio::fs::read_dir(self.paths.transfers_dir()).await else {
            return;
        };
        while let Ok(Some(file)) = files.next_entry().await {
            if !staged.contains(&file.path()) {
                let _ = tokio::fs::remove_file(file.path()).await;
            }
        }
    }
}

/// What a transfer needs to know about its source.
struct Source {
    account_id: i64,
    protocol: MailProtocol,
    folder_id: Option<i64>,
    uid: Option<i64>,
    message_id: Option<String>,
    size: Option<i64>,
    raw_path: Option<String>,
    is_outgoing: bool,
    jmap_blob_id: Option<String>,
    gm_msgid: Option<String>,
}

impl Source {
    fn get(conn: &Connection, message_id: i64) -> Result<Option<Self>> {
        Ok(conn
            .query_row(
                "SELECT m.account_id, a.mail_protocol, m.folder_id, m.uid, m.message_id,
                        m.size, m.raw_path, m.is_outgoing, m.jmap_blob_id, m.gm_msgid
                 FROM messages m JOIN accounts a ON a.id = m.account_id
                 WHERE m.id = ?1",
                params![message_id],
                |row| {
                    Ok(Self {
                        account_id: row.get(0)?,
                        protocol: row.get(1)?,
                        folder_id: row.get(2)?,
                        uid: row.get(3)?,
                        message_id: row.get(4)?,
                        size: row.get(5)?,
                        raw_path: row.get(6)?,
                        is_outgoing: row.get::<_, i64>(7)? != 0,
                        jmap_blob_id: row.get(8)?,
                        gm_msgid: row.get(9)?,
                    })
                },
            )
            .optional()?)
    }

    /// The file this app keeps of the whole message, if it keeps one. Gmail
    /// mail is kept as its parts, except what was sent from here.
    fn full_original(&self) -> Option<&str> {
        match self.protocol {
            MailProtocol::Gmail if !self.is_outgoing => None,
            _ => self.raw_path.as_deref(),
        }
    }
}

/// The messages of `target` a transfer can take: a conversation's but its
/// drafts, or those chosen, which are not drafts.
fn transfer_sources(conn: &Connection, target: &ActionTarget) -> Result<Vec<NewTransfer>> {
    let (sql, ids) = match target {
        ActionTarget::Threads(ids) => (
            "SELECT id, account_id, message_id, is_read, is_starred,
                    COALESCE(internal_date, date)
             FROM messages WHERE thread_id = ?1 AND is_draft = 0 ORDER BY date, id",
            ids,
        ),
        ActionTarget::Messages(ids) => (
            "SELECT id, account_id, message_id, is_read, is_starred,
                    COALESCE(internal_date, date)
             FROM messages WHERE id = ?1 AND is_draft = 0",
            ids,
        ),
    };
    let mut statement = conn.prepare_cached(sql)?;
    let mut sources = Vec::new();
    for id in ids {
        let rows = statement.query_map(params![id], |row| {
            Ok(NewTransfer {
                source_message_id: row.get(0)?,
                source_account_id: row.get(1)?,
                target_folder_id: 0,
                is_move: false,
                rfc_message_id: row.get(2)?,
                seen: row.get::<_, i64>(3)? != 0,
                flagged: row.get::<_, i64>(4)? != 0,
                received_at: row.get(5)?,
            })
        })?;
        for source in rows {
            sources.push(source?);
        }
    }
    Ok(sources)
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
#[path = "transfer_tests.rs"]
mod tests;
