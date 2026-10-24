//! Per-account sync actors. The main actor's IMAP connection runs a cycle
//! loop of only the cheap, latency-sensitive work: drain commands -> execute
//! due IMAP actions -> per-folder new-mail checks and flag/expunge
//! reconciliation. Everything heavy runs on its own connections in parallel:
//! first-time header backfills and historical extension on the dedicated
//! history connection (`run_history_backfill`), and bulk body downloads on a
//! pool of up to BODY_POOL_CONNS connections (`run_backfill_pool`). SMTP
//! submission also has a dedicated durable worker, so sending never waits for
//! a mailbox cycle. Cycles
//! therefore finish in seconds and the actor is back in IDLE almost
//! immediately, so new mail keeps arriving fast even during a huge backfill.
//!
//! On servers that support it the actor waits in IMAP IDLE between cycles, so
//! new inbox mail wakes it within seconds; the user-selected periodic cycle is
//! still a correctness backstop. Servers without IDLE use that same cadence. Either
//! way commands (body fetches, action nudges) interrupt the wait immediately
//! because it selects on the command channel.

use crate::accounts::credentials::{self, CredentialStoreHandle, Slot};
use crate::config::Paths;
use crate::db::Db;
use crate::db::repo::{self, folders::Folder};
use crate::error::{CoreError, Result};
use crate::events::{CoreEvent, EventBus, SyncProgress};
use crate::imap::{self, FetchedHeader, ImapCredentials, Session};
use crate::models::*;
use crate::oauth::tokens::TokenProvider;
use crate::queue;
use rusqlite::OptionalExtension;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, watch};

const HEADER_CHUNK: usize = 200;
/// Priority reads stay small so opening one thread never monopolizes the
/// dedicated reader connection.
const PRIORITY_BODY_FETCH_CHUNK: usize = 25;
/// Background selective reads are grouped into one multi-UID FETCH whenever
/// their MIME section layouts match. A large UID batch removes the per-message
/// network round trip and permits hundreds of small messages per second on a
/// fast server.
const BODY_FETCH_CHUNK: usize = 200;
/// Each round queues at most this many missing IDs across the account,
/// regardless of mailbox size. Older rows are reached through keyset pages.
const BODY_PLAN_PAGE: i64 = 2 * BODY_FETCH_CHUNK as i64;
/// At most two account rounds may retain a 400-ID planning page and its
/// workers at once. Release after each round so waiting accounts can advance.
/// This is acquired before the shared body-byte permit; foreground reads use
/// neither permit.
const BACKGROUND_BODY_PLAN_PAGES: usize = 2;
static BACKGROUND_BODY_PLAN_BUDGET: std::sync::OnceLock<std::sync::Arc<tokio::sync::Semaphore>> =
    std::sync::OnceLock::new();

async fn reserve_background_plan_page() -> tokio::sync::OwnedSemaphorePermit {
    BACKGROUND_BODY_PLAN_BUDGET
        .get_or_init(|| {
            std::sync::Arc::new(tokio::sync::Semaphore::new(BACKGROUND_BODY_PLAN_PAGES))
        })
        .clone()
        .acquire_owned()
        .await
        .expect("body planning budget is never closed")
}

/// Bound one selective response independently of message count. Two workers at
/// this ceiling use modest memory while still allowing high-throughput bursts.
const MAX_SELECTIVE_BATCH_BYTES: u64 = 8 * 1024 * 1024;
const MAX_MICROSOFT_BATCH_BYTES: u64 = 4 * 1024 * 1024;
/// Concurrent IMAP connections dedicated to bulk body backfill. Together with
/// the sync actor and the on-demand reader that's BODY_POOL_CONNS + 2
/// connections per account, well under common server caps (Gmail allows 15).
const BODY_POOL_CONNS: usize = 2;
const HISTORY_CHUNK: u32 = 500;
const CYCLE_SECS: u64 = 60;
/// Short retry used only while a new account does not have an Inbox row yet.
const IDLE_FALLBACK_SECS: u64 = 20;
/// Re-issue IDLE at least this often (RFC 2177 recommends < 29 min) to keep the
/// connection alive through NAT/firewall idle timeouts.
const IDLE_MAX_SECS: u64 = 25 * 60;
const FLAG_WINDOW: i64 = 1000; // most recent N UIDs get flag reconciliation
const SYNC_COMMAND_QUEUE_CAPACITY: usize = 64;
const PRIORITY_FETCH_QUEUE_CAPACITY: usize = 128;

#[derive(Debug, Clone, Copy)]
struct InboxBaseline {
    uid_validity: Option<i64>,
    /// UIDs below this value existed at the first successful INBOX SELECT and
    /// are launch catch-up. UIDs at/above it arrived while Flectar Mail was running.
    first_live_uid: u32,
}

#[derive(Debug, Clone, Copy)]
struct CycleMode {
    foreground_only: bool,
    immediate_follow_up: bool,
}

#[derive(Debug)]
pub enum SyncCmd {
    SyncNow {
        complete: Option<oneshot::Sender<std::result::Result<(), String>>>,
    },
    FetchBody {
        message_id: i64,
    },
    RunActions,
    Shutdown,
}

#[derive(Clone)]
pub struct AccountHandle {
    pub account_id: i64,
    tx: mpsc::Sender<SyncCmd>,
    /// Separate channel + connection for on-demand body reads so opening a
    /// message never waits behind a long bulk-sync cycle on the main actor.
    body_tx: mpsc::Sender<PriorityFetchCmd>,
    /// Coalesced wake-up for the dedicated SMTP submission worker. Gmail and
    /// JMAP own their native send queues and therefore leave this unset.
    send_tx: Option<mpsc::Sender<()>>,
    /// Set for accounts whose mail exists only on this device (POP3), where a
    /// message's original file can only be downloaded again from the server.
    restore_tx: Option<mpsc::Sender<RestoreRequest>>,
    settings_tx: watch::Sender<crate::models::AccountSettings>,
    tasks: Vec<tokio::task::AbortHandle>,
}

/// A request to download a stored message's original file again after it
/// went missing from this device. Answered with the file's new path.
pub(crate) struct RestoreRequest {
    pub message_id: i64,
    pub complete: oneshot::Sender<std::result::Result<String, String>>,
}

/// Restoring waits for any running download session to end first.
const RESTORE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5 * 60);

#[derive(Debug)]
pub(crate) enum PriorityFetchCmd {
    Body(i64),
    Attachment {
        attachment_id: i64,
        complete: oneshot::Sender<std::result::Result<Vec<u8>, String>>,
    },
}

impl AccountHandle {
    pub fn send(&self, cmd: SyncCmd) -> bool {
        let wakes_sender = matches!(&cmd, SyncCmd::RunActions);
        let sender_woken = wakes_sender
            && self
                .send_tx
                .as_ref()
                .is_some_and(|tx| match tx.try_send(()) {
                    Ok(()) | Err(mpsc::error::TrySendError::Full(())) => true,
                    Err(mpsc::error::TrySendError::Closed(())) => false,
                });
        // Route priority body fetches to the dedicated reader connection;
        // everything else drives the bulk sync actor.
        let primary_woken = match cmd {
            SyncCmd::FetchBody { message_id } => self
                .body_tx
                .try_send(PriorityFetchCmd::Body(message_id))
                .is_ok(),
            other => match self.tx.try_send(other) {
                Ok(()) => true,
                Err(mpsc::error::TrySendError::Full(command)) => {
                    if let SyncCmd::SyncNow {
                        complete: Some(complete),
                    } = command
                    {
                        let _ = complete.send(Err("sync command queue is busy".into()));
                    }
                    false
                }
                Err(mpsc::error::TrySendError::Closed(command)) => {
                    if let SyncCmd::SyncNow {
                        complete: Some(complete),
                    } = command
                    {
                        let _ = complete.send(Err("sync worker stopped".into()));
                    }
                    false
                }
            },
        };
        primary_woken || sender_woken
    }

    /// Stop every worker owned by this account immediately. This is used when
    /// credentials or the remote account identity change, so an old in-flight
    /// worker cannot commit data from the previous server after replacement.
    pub fn abort(&self) {
        let _ = self.tx.try_send(SyncCmd::Shutdown);
        for task in &self.tasks {
            task.abort();
        }
    }

    pub fn sync_now(&self) -> oneshot::Receiver<std::result::Result<(), String>> {
        let (tx, rx) = oneshot::channel();
        self.send(SyncCmd::SyncNow { complete: Some(tx) });
        rx
    }

    pub fn update_settings(&self, settings: crate::models::AccountSettings) {
        self.settings_tx.send_replace(settings);
        self.send(SyncCmd::SyncNow { complete: None });
    }

    /// Download a stored message's original again; see [`RestoreRequest`].
    pub(crate) async fn restore_message(&self, message_id: i64) -> Result<String> {
        let restore_tx = self.restore_tx.as_ref().ok_or_else(|| {
            CoreError::Other("this account reads its messages from the server".into())
        })?;
        let (tx, rx) = oneshot::channel();
        restore_tx
            .send(RestoreRequest {
                message_id,
                complete: tx,
            })
            .await
            .map_err(|_| CoreError::Other("mail worker stopped".into()))?;
        tokio::time::timeout(RESTORE_TIMEOUT, rx)
            .await
            .map_err(|_| CoreError::Other("downloading the message again timed out".into()))?
            .map_err(|_| CoreError::Other("mail worker stopped".into()))?
            .map_err(CoreError::Other)
    }

    pub async fn fetch_attachment(&self, attachment_id: i64) -> Result<Vec<u8>> {
        let (tx, rx) = oneshot::channel();
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            self.body_tx.send(PriorityFetchCmd::Attachment {
                attachment_id,
                complete: tx,
            }),
        )
        .await
        .map_err(|_| CoreError::Other("attachment reader queue is busy".into()))?
        .map_err(|_| CoreError::Other("attachment reader stopped".into()))?;
        tokio::time::timeout(
            imap::ATTACHMENT_TIMEOUT + std::time::Duration::from_secs(10),
            rx,
        )
        .await
        .map_err(|_| CoreError::Imap("attachment request timed out".into()))?
        .map_err(|_| CoreError::Other("attachment reader stopped".into()))?
        .map_err(CoreError::Other)
    }
}

#[cfg(test)]
mod account_handle_tests {
    use super::*;

    #[tokio::test]
    async fn action_nudge_wakes_sync_and_smtp_workers() {
        let (tx, mut rx) = mpsc::channel(1);
        let (body_tx, _body_rx) = mpsc::channel(1);
        let (send_tx, mut send_rx) = mpsc::channel(1);
        let (settings_tx, _settings_rx) = watch::channel(crate::db::testutil::account_settings());
        let handle = AccountHandle {
            account_id: 7,
            tx,
            body_tx,
            send_tx: Some(send_tx),
            restore_tx: None,
            settings_tx,
            tasks: Vec::new(),
        };

        assert!(handle.send(SyncCmd::RunActions));
        assert!(matches!(rx.recv().await, Some(SyncCmd::RunActions)));
        assert_eq!(send_rx.recv().await, Some(()));
    }
}

#[derive(Clone)]
pub struct SyncCtx {
    pub db: Db,
    pub calendar_db: Db,
    pub bus: EventBus,
    pub paths: Arc<Paths>,
    pub tokens: TokenProvider,
    pub credentials: CredentialStoreHandle,
}

pub fn spawn_account(ctx: SyncCtx, config: AccountConfig) -> AccountHandle {
    let account_id = config.id;
    let (tx, rx) = mpsc::channel(SYNC_COMMAND_QUEUE_CAPACITY);
    let (body_tx, body_rx) = mpsc::channel(PRIORITY_FETCH_QUEUE_CAPACITY);
    let (settings_tx, settings_rx) = watch::channel(config.settings.clone());
    // API protocols own their submission queues. SMTP-based protocols get a
    // dedicated submission worker that never waits behind mailbox sync.
    let (joins, send_tx, restore_tx) = match config.mail_protocol {
        MailProtocol::Gmail => (
            crate::sync::gmail::spawn(ctx, config, rx, body_rx, settings_rx),
            None,
            None,
        ),
        MailProtocol::Jmap => (
            crate::jmap::sync::spawn(ctx, config, rx, body_rx, settings_rx),
            None,
            None,
        ),
        MailProtocol::Imap => {
            let (send_tx, send_rx) = mpsc::channel(1);
            // These carry idempotent wakeups rather than work. A single
            // pending item represents every newer nudge and prevents bursts
            // from retaining memory.
            let (pool_tx, pool_rx) = mpsc::channel(1);
            let (hist_tx, hist_rx) = mpsc::channel(1);
            let joins = vec![
                tokio::spawn(run_send_dispatcher(
                    ctx.clone(),
                    config.clone(),
                    send_rx,
                    tx.clone(),
                )),
                tokio::spawn(run_actor(
                    ctx.clone(),
                    config.clone(),
                    rx,
                    pool_tx.clone(),
                    hist_tx,
                )),
                tokio::spawn(run_body_fetcher(ctx.clone(), config.clone(), body_rx)),
                tokio::spawn(run_backfill_pool(
                    ctx.clone(),
                    config.clone(),
                    pool_rx,
                    settings_rx.clone(),
                )),
                tokio::spawn(run_history_backfill(
                    ctx,
                    config,
                    hist_rx,
                    pool_tx,
                    settings_rx,
                )),
            ];
            (joins, Some(send_tx), None)
        }
        MailProtocol::Pop3 => {
            let (send_tx, send_rx) = mpsc::channel(1);
            let (restore_tx, restore_rx) = mpsc::channel(PRIORITY_FETCH_QUEUE_CAPACITY);
            let joins = vec![
                tokio::spawn(run_send_dispatcher(
                    ctx.clone(),
                    config.clone(),
                    send_rx,
                    tx.clone(),
                )),
                tokio::spawn(crate::sync::pop3::run(
                    ctx,
                    config,
                    crate::sync::pop3::Inputs {
                        commands: rx,
                        reads: body_rx,
                        restores: restore_rx,
                        settings: settings_rx,
                    },
                )),
            ];
            (joins, Some(send_tx), Some(restore_tx))
        }
    };
    AccountHandle {
        account_id,
        tx,
        body_tx,
        send_tx,
        restore_tx,
        settings_tx,
        tasks: joins
            .iter()
            .map(tokio::task::JoinHandle::abort_handle)
            .collect(),
    }
}

/// Durable per-account SMTP dispatcher. It checks the database on startup and
/// owns its own due-time timer, so delivery does not depend on a best-effort
/// channel notification or the five-second global scheduler tick.
async fn run_send_dispatcher(
    ctx: SyncCtx,
    config: AccountConfig,
    mut wake_rx: mpsc::Receiver<()>,
    action_tx: mpsc::Sender<SyncCmd>,
) {
    let account_id = config.id;
    tracing::debug!(account_id, "SMTP dispatcher started");
    loop {
        match queue::execute_due_sends(&ctx, &config).await {
            Ok(result) => {
                if result.processed != 0 {
                    // A successful submission enqueues durable Sent filing.
                    // Wake the IMAP actor immediately instead of waiting for
                    // the global scheduler's next tick.
                    let _ = action_tx.try_send(SyncCmd::RunActions);
                }
                if result.due_remaining {
                    continue;
                }
            }
            Err(error) => {
                // The executor already persisted retry/failure state. Keep the
                // worker alive. A short floor also prevents corrupt queue data
                // or a database read failure from creating a hot retry loop.
                tracing::warn!(account_id, error = %error, "SMTP dispatcher pass failed");
                tokio::select! {
                    _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {}
                    wake = wake_rx.recv() => if wake.is_none() { return },
                }
            }
        }

        let next_due = ctx
            .db
            .read(move |conn| repo::actions::next_send_due_at(conn, account_id))
            .await;
        match next_due {
            Ok(Some(due_at)) if due_at <= now_ms() => {
                // A newly queued action may have arrived between the pass and
                // this query. Yield once for fairness, then claim it.
                tokio::task::yield_now().await;
            }
            Ok(Some(due_at)) => {
                let delay =
                    std::time::Duration::from_millis(due_at.saturating_sub(now_ms()).max(1) as u64);
                tokio::select! {
                    _ = tokio::time::sleep(delay) => {}
                    wake = wake_rx.recv() => {
                        if wake.is_none() {
                            return;
                        }
                        while wake_rx.try_recv().is_ok() {}
                    }
                }
            }
            Ok(None) => match wake_rx.recv().await {
                Some(()) => while wake_rx.try_recv().is_ok() {},
                None => return,
            },
            Err(error) => {
                tracing::warn!(account_id, error = %error, "could not schedule SMTP dispatcher");
                tokio::select! {
                    _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {}
                    wake = wake_rx.recv() => if wake.is_none() { return },
                }
            }
        }
    }
}

pub(crate) async fn configured_sync_interval(db: &Db) -> std::time::Duration {
    let minutes = db
        .read(|conn| Ok(repo::settings::get(conn)?.sync_interval_minutes))
        .await
        .unwrap_or(5);
    let minutes = match minutes {
        1 | 5 | 15 => minutes as u64,
        _ => 5,
    };
    std::time::Duration::from_secs(minutes * 60)
}

/// Dedicated reader connection. Owns its own IMAP session and services only
/// on-demand single-message body fetches (user opened a thread). Kept separate
/// from `run_actor` so reading an email is instant even while the main actor is
/// busy with a long initial backfill. Connects lazily and reconnects on error.
async fn run_body_fetcher(
    ctx: SyncCtx,
    config: AccountConfig,
    mut rx: mpsc::Receiver<PriorityFetchCmd>,
) {
    let account_id = config.id;
    let mut session: Option<Session> = None;
    let mut deferred = std::collections::VecDeque::new();
    loop {
        let command = match deferred.pop_front() {
            Some(command) => command,
            None => match rx.recv().await {
                Some(command) => command,
                None => break,
            },
        };
        let first = match command {
            PriorityFetchCmd::Attachment {
                attachment_id,
                complete,
            } => {
                let mut result = Err(CoreError::Other("attachment fetch did not run".into()));
                for _ in 0..2 {
                    if session.is_none() {
                        match connect(&ctx, &config).await {
                            Ok(value) => session = Some(value),
                            Err(error) => {
                                result = Err(error);
                                break;
                            }
                        }
                    }
                    let Some(active_session) = session.as_mut() else {
                        result = Err(CoreError::Other(
                            "attachment fetch lost its IMAP session".into(),
                        ));
                        break;
                    };
                    match fetch_attachment_bytes(&ctx, active_session, attachment_id).await {
                        Ok(bytes) => {
                            result = Ok(bytes);
                            break;
                        }
                        Err(error) => {
                            result = Err(error);
                            if let Some(value) = session.take() {
                                imap::logout(value).await;
                            }
                        }
                    }
                }
                let _ = complete.send(result.map_err(|error| error.to_string()));
                continue;
            }
            PriorityFetchCmd::Body(first) => first,
        };
        // Coalesce every request already queued (plus a brief window for the
        // rest of a thread's messages to land) into ONE batch, so opening a
        // thread pulls all its bodies in a single FETCH round-trip instead of
        // one per message. Opening a single message just fetches that one.
        let mut ids = vec![first];
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        while let Ok(command) = rx.try_recv() {
            match command {
                PriorityFetchCmd::Body(id) if ids.len() < PRIORITY_BODY_FETCH_CHUNK => ids.push(id),
                other => deferred.push_back(other),
            }
        }
        ids.sort_unstable();
        ids.dedup();

        // Try the reused session first; if it's gone stale (servers close idle
        // connections), the fetch fails, so drop it and retry once on a fresh
        // connection. Without the retry the skeleton would hang forever: the
        // requests were already consumed from the channel, and nothing
        // re-nudges them until the user reopens the thread.
        let mut fetched = false;
        for attempt in 0..2 {
            if session.is_none() {
                match connect(&ctx, &config).await {
                    Ok(s) => session = Some(s),
                    Err(e) => {
                        tracing::warn!(account_id, error = %e, "body fetcher: connect failed");
                        break;
                    }
                }
            }
            match fetch_bodies_batch(&ctx, &config, &mut session, &ids).await {
                Ok(()) => {
                    fetched = true;
                    break;
                }
                Err(e) => {
                    tracing::warn!(
                        account_id, attempt, error = %e,
                        "body fetcher: batch fetch failed; dropping session",
                    );
                    // The session may be broken; drop it so the retry (and any
                    // later request) reconnects.
                    drop(session.take());
                }
            }
        }
        if !fetched {
            // Fetch didn't land. Roll body_state back to "none" for all so both
            // paths recover: a reopen re-nudges, and the bulk backfill (which
            // only scans body_state = 'none') can pick them up. Left at
            // "fetching" they would be stuck and the UI would show an endless
            // skeleton. No event is emitted here on purpose - invalidating the
            // open thread would make get_thread re-nudge immediately and, while
            // offline, spin a tight fail/retry loop.
            reset_bodies_none(&ctx, ids.into_iter()).await;
        }
    }
}

async fn fetch_attachment_bytes(
    ctx: &SyncCtx,
    session: &mut Session,
    attachment_id: i64,
) -> Result<Vec<u8>> {
    let (message_id, section) = ctx
        .db
        .read(move |conn| {
            conn.query_row(
                "SELECT message_id, imap_section FROM attachments WHERE id = ?1",
                rusqlite::params![attachment_id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .map_err(Into::into)
        })
        .await?;
    let section = section.ok_or_else(|| CoreError::NotFound("attachment IMAP section".into()))?;
    let message = ctx
        .db
        .read(move |conn| repo::messages::get_row(conn, message_id))
        .await?
        .ok_or_else(|| CoreError::NotFound(format!("message {message_id}")))?;
    let (folder_id, uid) = message
        .folder_id
        .zip(message.uid)
        .ok_or_else(|| CoreError::NotFound("remote attachment location".into()))?;
    let folder = ctx
        .db
        .read(move |conn| repo::folders::get(conn, folder_id))
        .await?
        .ok_or_else(|| CoreError::NotFound(format!("folder {folder_id}")))?;
    select_folder_for_remote_read(ctx, session, &folder).await?;
    if !remote_location_is_current(ctx, message_id, folder_id, uid).await? {
        return Err(CoreError::NotFound(
            "remote attachment location changed while fetching".into(),
        ));
    }
    let fetched = imap::fetch_attachment_section(session, uid as u32, &section)
        .await?
        .ok_or_else(|| CoreError::NotFound(format!("attachment section {section}")))?;
    crate::mime::decode_attachment_section(&fetched.mime_header, &fetched.body)
}

/// Fetch bodies for a set of messages, grouping by folder so each folder's UIDs
/// go out in a single batched FETCH. Idempotent: already-cached messages are
/// skipped, so a retry after a partial failure only refetches what's missing.
async fn fetch_bodies_batch(
    ctx: &SyncCtx,
    config: &AccountConfig,
    session: &mut Option<Session>,
    ids: &[i64],
) -> Result<()> {
    use std::collections::HashMap;
    // folder_id -> [(uid, message_id)]
    let mut by_folder: HashMap<i64, Vec<(u32, i64)>> = HashMap::new();
    for &message_id in ids {
        let row = ctx
            .db
            .read(move |conn| repo::messages::get_row(conn, message_id))
            .await?;
        let Some(row) = row else { continue };
        if row.body_state == "cached" {
            continue;
        }
        let (Some(folder_id), Some(uid)) = (row.folder_id, row.uid) else {
            // `request_body` marks the row fetching before it reaches this
            // worker. A local-only/orphaned row cannot be fetched remotely,
            // so release it instead of leaving an endless skeleton.
            reset_bodies_none(ctx, std::iter::once(message_id)).await;
            continue;
        };
        by_folder
            .entry(folder_id)
            .or_default()
            .push((uid as u32, message_id));
    }

    for (folder_id, mut items) in by_folder {
        let folder = ctx
            .db
            .read(move |conn| repo::folders::get(conn, folder_id))
            .await?;
        let Some(folder) = folder else {
            // Folder row gone (deleted/renamed mid-flight): don't fail the whole
            // batch - just release these back to "none" for a later retry.
            reset_bodies_none(ctx, items.iter().map(|(_, m)| *m)).await;
            continue;
        };
        select_folder_for_remote_read(ctx, session.as_mut().ok_or(CoreError::Offline)?, &folder)
            .await?;
        items.sort_unstable_by_key(|(uid, _)| *uid);
        for (uid, message_id) in items {
            if !remote_location_is_current(ctx, message_id, folder_id, i64::from(uid)).await? {
                reset_bodies_none(ctx, std::iter::once(message_id)).await;
                continue;
            }
            if let Err(e) =
                fetch_selective_content(ctx, config, session, message_id, folder_id, uid, true)
                    .await
            {
                tracing::warn!(message_id, error = %e, "priority content fetch failed; releasing");
                record_content_failure(ctx, message_id, &e).await;
                reset_bodies_none(ctx, std::iter::once(message_id)).await;
                return Err(e);
            }
        }
    }
    Ok(())
}

/// Best-effort reset of message body_state back to "none" (release for retry).
async fn reset_bodies_none(ctx: &SyncCtx, ids: impl Iterator<Item = i64>) {
    for message_id in ids {
        let _ = ctx
            .db
            .write(move |conn| {
                // A batch may cache earlier messages before a later one fails.
                // Do not roll successful siblings back to `none`.
                conn.execute(
                    "UPDATE messages SET body_state = 'none'
                     WHERE id = ?1 AND body_state = 'fetching'",
                    rusqlite::params![message_id],
                )?;
                Ok(())
            })
            .await;
    }
}

/// Select a folder only while its persisted UID namespace still matches the
/// server. A mismatch invalidates every saved UID in that folder, so clear the
/// map before allowing any later read to retry.
async fn select_folder_for_remote_read(
    ctx: &SyncCtx,
    session: &mut Session,
    folder: &Folder,
) -> Result<()> {
    let selected = imap::select(session, &folder.path).await?;
    if selected.uid_validity != folder.uidvalidity {
        let folder_id = folder.id;
        let folder_name = folder.path.clone();
        let expected_uidvalidity = folder.uidvalidity;
        let remote_uidvalidity = selected.uid_validity;
        let remote_uidnext = selected.uid_next;
        ctx.db
            .write(move |conn| {
                let tx = conn.transaction()?;
                let current = repo::folders::get(&tx, folder_id)?;
                if current.as_ref().is_some_and(|current| {
                    current.path == folder_name && current.uidvalidity == expected_uidvalidity
                }) {
                    repo::folders::reset_uid_mappings(&tx, folder_id)?;
                    tx.execute(
                        "UPDATE messages SET body_state = 'none'
                         WHERE folder_id = ?1 AND body_state = 'fetching'",
                        rusqlite::params![folder_id],
                    )?;
                    repo::folders::set_uid_state(
                        &tx,
                        folder_id,
                        remote_uidvalidity,
                        remote_uidnext,
                        None,
                    )?;
                }
                tx.commit()?;
                Ok(())
            })
            .await?;
        return Err(CoreError::Imap(format!(
            "folder UIDVALIDITY changed before remote read: expected {:?}, selected {:?}",
            folder.uidvalidity, selected.uid_validity
        )));
    }

    let folder_id = folder.id;
    let folder_name = folder.path.clone();
    let uidvalidity = folder.uidvalidity;
    let still_current = ctx
        .db
        .read(move |conn| {
            Ok(repo::folders::get(conn, folder_id)?.is_some_and(|current| {
                current.path == folder_name && current.uidvalidity == uidvalidity
            }))
        })
        .await?;
    if !still_current {
        return Err(CoreError::NotFound(
            "folder changed while selecting remote mailbox".into(),
        ));
    }
    Ok(())
}

async fn remote_location_is_current(
    ctx: &SyncCtx,
    message_id: i64,
    folder_id: i64,
    uid: i64,
) -> Result<bool> {
    ctx.db
        .read(move |conn| {
            Ok(current_body_row(conn, message_id)?.is_some_and(|current| {
                current.folder_id == Some(folder_id) && current.uid == Some(uid)
            }))
        })
        .await
}

async fn imap_credentials(ctx: &SyncCtx, config: &AccountConfig) -> Result<ImapCredentials> {
    match config.auth_kind {
        AuthKind::Password => {
            let password =
                credentials::load_async(ctx.credentials.clone(), config.id, Slot::Password).await?;
            Ok(ImapCredentials::Password {
                user: config.username.clone(),
                password,
            })
        }
        AuthKind::Oauth2 => {
            let token = ctx.tokens.access_token(config.id, config.provider).await?;
            Ok(ImapCredentials::XOAuth2 {
                user: config.username.clone(),
                access_token: token,
            })
        }
    }
}

async fn run_actor(
    ctx: SyncCtx,
    config: AccountConfig,
    mut rx: mpsc::Receiver<SyncCmd>,
    pool_tx: mpsc::Sender<()>,
    hist_tx: mpsc::Sender<()>,
) {
    let account_id = config.id;
    crate::events::sync_diagnostic("imap_worker", Some(account_id), "started");
    tracing::debug!(account_id, "sync actor started");
    let mut session: Option<Session> = None;
    // Whether the current connection supports IMAP IDLE (push). Re-probed on
    // every reconnect; `None` until the first successful connect.
    let mut supports_idle: Option<bool> = None;
    let mut backoff_secs: u64 = 1;
    let mut cycle: u64 = 0;
    let mut immediate_follow_up = false;
    let mut inbox_baseline: Option<InboxBaseline> = None;
    let mut sync_waiters: Vec<oneshot::Sender<std::result::Result<(), String>>> = Vec::new();
    // When the next cycle is due; only read by the waits below (any wake-up
    // from a wait runs a cycle immediately).
    let mut cycle_due;

    loop {
        // 1. Ensure a connection.
        if session.is_none() {
            match connect(&ctx, &config).await {
                Ok(mut s) => {
                    // Probe IDLE support once per connection; on error assume
                    // no push and fall back to polling.
                    supports_idle = Some(imap::supports_idle(&mut s).await.unwrap_or(false));
                    tracing::info!(
                        account_id,
                        idle = supports_idle == Some(true),
                        "receive: connected; IDLE push {}",
                        if supports_idle == Some(true) {
                            "enabled"
                        } else {
                            "unavailable, polling"
                        },
                    );
                    session = Some(s);
                    backoff_secs = 1;
                    ctx.bus.emit(CoreEvent::NetworkState { online: true });
                    set_state(&ctx, account_id, "syncing").await;
                }
                Err(e @ (CoreError::NeedsReauth | CoreError::Auth(_))) => {
                    crate::events::sync_diagnostic("imap_connect", Some(account_id), e.code());
                    tracing::warn!(
                        account_id,
                        incoming_host = %config.incoming_host,
                        error = %e,
                        "imap connect rejected at auth; marking needs_reauth"
                    );
                    set_state(&ctx, account_id, "needs_reauth").await;
                    // Wait for an external nudge before retrying auth.
                    match rx.recv().await {
                        None | Some(SyncCmd::Shutdown) => return,
                        Some(SyncCmd::SyncNow { complete }) => {
                            if let Some(complete) = complete {
                                sync_waiters.push(complete);
                            }
                            continue;
                        }
                        Some(_) => continue,
                    }
                }
                Err(e) => {
                    crate::events::sync_diagnostic("imap_connect", Some(account_id), e.code());
                    tracing::warn!(account_id, "imap connect failed: {e}");
                    ctx.bus.emit(CoreEvent::NetworkState { online: false });
                    let message = e.to_string();
                    set_state_error(&ctx, account_id, "offline", &message).await;
                    // Backoff, but wake early on commands.
                    let wait = std::time::Duration::from_secs(backoff_secs);
                    backoff_secs = (backoff_secs * 2).min(300);
                    match tokio::time::timeout(wait, rx.recv()).await {
                        Ok(None) | Ok(Some(SyncCmd::Shutdown)) => return,
                        Ok(Some(SyncCmd::SyncNow { complete })) => {
                            if let Some(complete) = complete {
                                sync_waiters.push(complete);
                            }
                            continue;
                        }
                        _ => continue,
                    }
                }
            }
        }
        // Coalesce every command that arrived while connecting or while the
        // previous cycle was running. All Sync Now callers share the next
        // foreground Inbox pass instead of queuing duplicate full cycles.
        while let Ok(command) = rx.try_recv() {
            match command {
                SyncCmd::SyncNow { complete } => {
                    if let Some(complete) = complete {
                        sync_waiters.push(complete);
                    }
                }
                SyncCmd::Shutdown => {
                    if let Some(session) = session.take() {
                        imap::logout(session).await;
                    }
                    return;
                }
                SyncCmd::RunActions => {}
                // AccountHandle routes these to the dedicated reader; keep the
                // arm defensive for direct/internal senders.
                SyncCmd::FetchBody { .. } => {}
            }
        }
        let Some(mut s) = session.take() else {
            tracing::error!(
                account_id,
                "sync actor lost its IMAP session before a cycle"
            );
            continue;
        };

        // 2. Run one cycle; on IMAP errors drop the session and reconnect.
        // Bulk body downloads run on the backfill pool's own connections, so a
        // cycle is only folder/header/flag/action work and stays short.
        tracing::debug!(account_id, cycle, "sync cycle start");
        set_state(&ctx, account_id, "syncing").await;
        let foreground_only = !sync_waiters.is_empty();
        let cycle_result = run_cycle(
            &ctx,
            &config,
            &mut s,
            cycle,
            &hist_tx,
            &mut inbox_baseline,
            CycleMode {
                foreground_only,
                immediate_follow_up,
            },
        )
        .await;
        tracing::debug!(
            account_id,
            cycle,
            ok = cycle_result.is_ok(),
            "sync cycle end"
        );
        // Pacing for the next cycle; also applies after errors so a flaky
        // server isn't hammered.
        cycle_due = tokio::time::Instant::now() + configured_sync_interval(&ctx.db).await;
        let mut rerun_immediately = false;
        match cycle_result {
            Ok(work_remaining) => {
                session = Some(s);
                cycle += 1;
                immediate_follow_up = work_remaining;
                // Historical content is explicitly background work. It may
                // continue for hours without keeping the foreground account
                // state (and the top-bar spinner) stuck on syncing.
                let (done, total) = ctx
                    .db
                    .read(move |conn| {
                        let cutoff = repo::accounts::get_config(conn, account_id)?
                            .and_then(|config| config.settings.mail_history.cutoff_ms_at(now_ms()));
                        repo::messages::body_progress_since(conn, account_id, cutoff)
                    })
                    .await
                    .unwrap_or((0, 0));
                if done < total {
                    let _ = pool_tx.try_send(());
                }
                if work_remaining {
                    // Continue the same logical synchronization without
                    // publishing an idle transition between bounded slices.
                    rerun_immediately = true;
                } else {
                    set_state(&ctx, account_id, "idle").await;
                    ctx.bus.emit(CoreEvent::SyncProgress(SyncProgress {
                        account_id,
                        folder: String::new(),
                        phase: "idle".into(),
                        done,
                        total,
                    }));
                }
                for waiter in sync_waiters.drain(..) {
                    let _ = waiter.send(Ok(()));
                }
            }
            Err(CoreError::NeedsReauth) | Err(CoreError::Auth(_)) => {
                imap::logout(s).await;
                set_state(&ctx, account_id, "needs_reauth").await;
                for waiter in sync_waiters.drain(..) {
                    let _ = waiter.send(Err("authentication required".into()));
                }
            }
            Err(e) => {
                tracing::warn!(account_id, "sync cycle error: {e}");
                imap::logout(s).await;
                ctx.bus.emit(CoreEvent::NetworkState { online: false });
                let message = e.to_string();
                set_state_error(&ctx, account_id, "offline", &message).await;
                for waiter in sync_waiters.drain(..) {
                    let _ = waiter.send(Err(message.clone()));
                }
            }
        }

        if rerun_immediately {
            continue;
        }

        // 3. Wait for new mail (IDLE), the next cycle, or an immediate command.
        // The backfill pool drains bodies on its own connections, so the actor
        // is free to IDLE even while thousands of bodies are still downloading.
        let use_idle = supports_idle == Some(true) && session.is_some();

        if use_idle {
            // After a cycle the selected mailbox may be sent/drafts/etc, and
            // IDLE needs a selected mailbox, so re-SELECT the inbox first. v1
            // pushes on the inbox only; other folders arrive via the full cycle.
            let inbox = ctx
                .db
                .read(move |conn| repo::folders::by_role(conn, account_id, roles::INBOX))
                .await;
            match inbox {
                Ok(Some(inbox)) => {
                    let Some(mut s) = session.take() else {
                        supports_idle = None;
                        continue;
                    };
                    match imap::select(&mut s, &inbox.path).await {
                        Ok(_) => {
                            // Cap IDLE at the next full cycle so the selected
                            // correctness backstop is preserved (a message that
                            // slipped in just before IDLE started is still
                            // caught by the timed-out cycle), clamped to
                            // IDLE_MAX_SECS as an upper safety bound.
                            let max = cycle_due
                                .saturating_duration_since(tokio::time::Instant::now())
                                .min(std::time::Duration::from_secs(IDLE_MAX_SECS))
                                .max(std::time::Duration::from_secs(1));
                            tracing::info!(
                                account_id,
                                max_secs = max.as_secs(),
                                "receive: waiting in IDLE for new mail",
                            );
                            match imap::idle_wait(s, &mut rx, max).await {
                                Ok((s, outcome)) => {
                                    session = Some(s);
                                    match outcome {
                                        // New mail pushed by the server: fall
                                        // through, next iteration syncs now.
                                        imap::IdleOutcome::Activity => {
                                            tracing::info!(
                                                account_id,
                                                "receive: IDLE signaled activity, syncing now",
                                            );
                                        }
                                        // Backstop timeout or a sync/action
                                        // nudge: next iteration runs a cycle.
                                        imap::IdleOutcome::Timeout
                                        | imap::IdleOutcome::Command(SyncCmd::RunActions) => {}
                                        imap::IdleOutcome::Command(SyncCmd::SyncNow {
                                            complete,
                                        }) => {
                                            if let Some(complete) = complete {
                                                sync_waiters.push(complete);
                                            }
                                        }
                                        imap::IdleOutcome::Command(SyncCmd::FetchBody {
                                            message_id,
                                        }) => {
                                            if let Err(e) = fetch_one_body(
                                                &ctx,
                                                &config,
                                                &mut session,
                                                message_id,
                                            )
                                            .await
                                            {
                                                tracing::warn!("priority body fetch failed: {e}");
                                            }
                                        }
                                        imap::IdleOutcome::Command(SyncCmd::Shutdown)
                                        | imap::IdleOutcome::ChannelClosed => {
                                            if let Some(s) = session.take() {
                                                imap::logout(s).await;
                                            }
                                            return;
                                        }
                                    }
                                }
                                Err(e) => {
                                    // Session was consumed by idle_wait; drop
                                    // IDLE state and reconnect next iteration.
                                    tracing::warn!(account_id, "idle failed: {e}");
                                    supports_idle = None;
                                    ctx.bus.emit(CoreEvent::NetworkState { online: false });
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!(account_id, "select inbox for idle failed: {e}");
                            imap::logout(s).await;
                            supports_idle = None;
                        }
                    }
                }
                _ => {
                    // No inbox row yet (first onboarding): short wait, then run
                    // a full cycle so the inbox lands promptly.
                    match tokio::time::timeout(
                        std::time::Duration::from_secs(IDLE_FALLBACK_SECS),
                        rx.recv(),
                    )
                    .await
                    {
                        Ok(None) | Ok(Some(SyncCmd::Shutdown)) => {
                            if let Some(s) = session.take() {
                                imap::logout(s).await;
                            }
                            return;
                        }
                        _ => {}
                    }
                }
            }
        } else {
            // Poll path: servers without IDLE use the same user-selected
            // cadence as every other periodic provider refresh.
            let mut deadline = cycle_due;
            loop {
                match tokio::time::timeout_at(deadline, rx.recv()).await {
                    Err(_) => {
                        // Timer fired: next iteration runs a full cycle.
                        break;
                    }
                    Ok(None) | Ok(Some(SyncCmd::Shutdown)) => {
                        if let Some(s) = session.take() {
                            imap::logout(s).await;
                        }
                        return;
                    }
                    Ok(Some(SyncCmd::SyncNow { complete })) => {
                        if let Some(complete) = complete {
                            sync_waiters.push(complete);
                        }
                        break;
                    }
                    Ok(Some(SyncCmd::RunActions)) => {
                        // Commands need a full cycle (folder sync / action replay).
                        break;
                    }
                    Ok(Some(SyncCmd::FetchBody { message_id })) => {
                        if session.is_some() {
                            if let Err(e) =
                                fetch_one_body(&ctx, &config, &mut session, message_id).await
                            {
                                tracing::warn!("priority body fetch failed: {e}");
                                deadline = tokio::time::Instant::now();
                            }
                        } else {
                            break;
                        }
                    }
                }
            }
        }
    }
}

pub(crate) async fn set_state(ctx: &SyncCtx, account_id: i64, state: &str) {
    set_state_inner(ctx, account_id, state, None).await;
}

pub(crate) async fn set_state_error(ctx: &SyncCtx, account_id: i64, state: &str, error: &str) {
    set_state_inner(ctx, account_id, state, Some(error)).await;
}

async fn set_state_inner(ctx: &SyncCtx, account_id: i64, state: &str, error: Option<&str>) {
    let st = state.to_string();
    let error = error.map(str::to_owned);
    let event_error = error.clone();
    let changed = ctx
        .db
        .write(move |conn| {
            repo::accounts::update_sync_state_with_error(conn, account_id, &st, error.as_deref())
        })
        .await
        .unwrap_or(false);
    if !changed {
        return;
    }
    ctx.bus.emit(CoreEvent::AccountState {
        account_id,
        sync_state: state.to_string(),
        sync_error: event_error,
    });
    emit_sync_status(ctx, account_id).await;
}

/// Build the authoritative per-account foreground/background snapshot used by
/// both the event stream and `get_sync_status`. Historical work is deliberately
/// separate from the account's foreground readiness state.
pub async fn status_for_account(db: &Db, account_id: i64) -> Result<SyncStatus> {
    db.read(move |conn| {
        let account = repo::accounts::get(conn, account_id)?
            .ok_or_else(|| CoreError::NotFound(format!("account {account_id}")))?;
        let cutoff = repo::accounts::get_config(conn, account_id)?
            .and_then(|config| config.settings.mail_history.cutoff_ms_at(now_ms()));
        let content = repo::messages::content_progress_since(conn, account_id, cutoff)?;
        let gmail = account.mail_protocol == MailProtocol::Gmail;
        let (header_done, header_total): (i64, i64) = match account.mail_protocol {
            MailProtocol::Gmail => conn.query_row(
                "SELECT COALESCE(backfill_done, 0), 1
                 FROM gmail_sync_state WHERE account_id = ?1
                 UNION ALL SELECT 0, 1
                 WHERE NOT EXISTS (SELECT 1 FROM gmail_sync_state WHERE account_id = ?1)
                 LIMIT 1",
                rusqlite::params![account_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?,
            // The folders the history worker fills, as `repo::folders::list`
            // shows them.
            MailProtocol::Imap | MailProtocol::Jmap => conn.query_row(
                "SELECT COALESCE(SUM(backfill_done), 0), COUNT(*)
                 FROM folders
                 WHERE account_id = ?1 AND COALESCE(role, '') <> 'all'
                   AND selectable = 1 AND missing_on_server = 0
                   AND (jmap_id IS NOT NULL) = ?2",
                rusqlite::params![account_id, account.mail_protocol == MailProtocol::Jmap],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?,
            // POP3 downloads whole messages in its foreground sync.
            MailProtocol::Pop3 => (0, 0),
        };
        let header_failed: i64 = conn.query_row(
            "SELECT COUNT(*)
             FROM sync_failures sf
             JOIN folders f ON f.id = sf.folder_id
             WHERE sf.account_id = ?1 AND sf.stage = 'header'
               AND (?2 = 1 OR COALESCE(f.role, '') <> 'all')",
            rusqlite::params![account_id, gmail],
            |row| row.get(0),
        )?;
        let (indexed, index_total): (i64, i64) = conn.query_row(
            "SELECT COALESCE(SUM(embedding_state = 'done'), 0), COUNT(*)
             FROM messages
             WHERE account_id = ?1 AND body_state = 'cached'",
            rusqlite::params![account_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;

        let background = if header_done < header_total || header_failed > 0 {
            Some(SyncBackgroundProgress {
                phase: if header_failed > 0 {
                    "retrying"
                } else {
                    "headers"
                }
                .into(),
                done: header_done.max(0) as u64,
                total: header_total.max(0) as u64,
                failed: header_failed.max(0) as u64,
            })
        } else if content.done < content.total {
            Some(SyncBackgroundProgress {
                phase: if content.failed > 0 {
                    "retrying"
                } else {
                    "content"
                }
                .into(),
                done: content.done,
                total: content.total,
                failed: content.failed,
            })
        } else if indexed < index_total {
            Some(SyncBackgroundProgress {
                phase: "indexing".into(),
                done: indexed.max(0) as u64,
                total: index_total.max(0) as u64,
                failed: 0,
            })
        } else {
            None
        };

        Ok(SyncStatus {
            account_id,
            foreground_phase: if account.sync_state == "syncing" {
                "inbox".into()
            } else {
                "idle".into()
            },
            state: account.sync_state,
            background,
        })
    })
    .await
}

pub(super) async fn emit_sync_status(ctx: &SyncCtx, account_id: i64) {
    if let Ok(status) = status_for_account(&ctx.db, account_id).await {
        ctx.bus.emit(CoreEvent::SyncStatus(status));
    }
}

async fn connect(ctx: &SyncCtx, config: &AccountConfig) -> Result<Session> {
    crate::events::sync_diagnostic("imap_credentials", Some(config.id), "loading");
    let creds = imap_credentials(ctx, config).await?;
    crate::events::sync_diagnostic("imap_connect", Some(config.id), "connecting");
    match imap::connect_with_settings(
        &config.incoming_host,
        config.incoming_port,
        creds,
        &config.settings.connection,
    )
    .await
    {
        Err(error)
            if config.auth_kind == AuthKind::Oauth2
                && matches!(&error, CoreError::NeedsReauth | CoreError::Auth(_)) =>
        {
            // The provider can invalidate an access token before its advertised
            // expiry. Refresh once before pausing the account for interactive
            // reauthentication; Exchange auth rejection occurs before mailbox
            // selection, so reconnecting here cannot replay a mutation.
            tracing::info!(
                account_id = config.id,
                provider = ?config.provider,
                "IMAP rejected cached OAuth token; refreshing once"
            );
            ctx.tokens.invalidate(config.id).await;
            let creds = imap_credentials(ctx, config).await?;
            imap::connect_with_settings(
                &config.incoming_host,
                config.incoming_port,
                creds,
                &config.settings.connection,
            )
            .await
        }
        result => result,
    }
}

/// One sync cycle: replay queued actions, then sync every folder's headers,
/// flags, and expunges. Bulk bodies are NOT fetched here - the backfill pool
/// downloads them concurrently on its own connections; this only nudges it.
async fn run_cycle(
    ctx: &SyncCtx,
    config: &AccountConfig,
    session: &mut Session,
    cycle: u64,
    hist_tx: &mpsc::Sender<()>,
    inbox_baseline: &mut Option<InboxBaseline>,
    mode: CycleMode,
) -> Result<bool> {
    let CycleMode {
        foreground_only,
        immediate_follow_up,
    } = mode;
    let account_id = config.id;

    // Folder discovery: first cycle and then every ~30 cycles.
    if !immediate_follow_up && cycle.is_multiple_of(30) {
        discover_folders(ctx, config, session).await?;
    }

    let folders = {
        let acc = account_id;
        ctx.db
            .read(move |conn| repo::folders::list(conn, Some(acc)))
            .await?
    };

    // Sync order: inbox first, then sent/drafts, then the rest.
    let mut ordered: Vec<&Folder> = folders.iter().filter(|folder| folder.selectable).collect();
    ordered.sort_by_key(|f| match f.role.as_deref() {
        Some(roles::INBOX) => 0,
        Some(roles::SENT) => 1,
        Some(roles::DRAFTS) => 2,
        Some(roles::ARCHIVE) => 3,
        _ => 4,
    });

    // Give queued mutations one bounded turn before checking Inbox. This keeps
    // an Inbox catch-up from starving moves while the following Inbox pass
    // ensures a large mutation queue cannot starve newly arrived mail.
    let action_slice = queue::execute_due(ctx, config, session).await?;

    let mut inbox_forward_remaining = false;
    if let Some(inbox) = ordered
        .iter()
        .copied()
        .find(|f| f.role.as_deref() == Some(roles::INBOX))
    {
        inbox_forward_remaining = sync_folder(
            ctx,
            config,
            session,
            inbox,
            // Reload must also reconcile existing Inbox messages, even when
            // the next scheduled flag/expunge pass is several cycles away.
            foreground_only || (!immediate_follow_up && cycle > 0 && cycle.is_multiple_of(5)),
            hist_tx,
            inbox_baseline,
        )
        .await?;
    }

    // A manual Sync Now resolves at the foreground Inbox boundary. Finish any
    // remaining bounded action/Inbox work in an immediate normal pass.
    if foreground_only || action_slice.due_remaining || inbox_forward_remaining {
        return Ok(true);
    }

    for folder in ordered {
        if folder.role.as_deref() == Some(roles::INBOX) {
            continue;
        }
        // A special-use \All mailbox repeats messages stored in other folders.
        if folder.role.as_deref() == Some(roles::ALL) {
            continue;
        }
        // Non-inbox folders get new-mail checks every cycle but heavy
        // reconciliation (flags/expunge) only every 5th cycle.
        let heavy = !immediate_follow_up && cycle > 0 && cycle.is_multiple_of(5);
        if let Err(e) =
            sync_folder(ctx, config, session, folder, heavy, hist_tx, inbox_baseline).await
        {
            tracing::warn!(folder = %folder.path, "folder sync failed: {e}");
            return Err(e); // connection likely broken; reconnect
        }
    }

    // GC old finished actions (keep 7 days).
    let _ = ctx
        .db
        .write(move |conn| repo::actions::gc(conn, now_ms() - 7 * 24 * 3600 * 1000))
        .await;

    Ok(false)
}

async fn discover_folders(
    ctx: &SyncCtx,
    config: &AccountConfig,
    session: &mut Session,
) -> Result<()> {
    let account_id = config.id;
    ctx.bus.emit(CoreEvent::SyncProgress(SyncProgress {
        account_id,
        folder: String::new(),
        phase: "folders".into(),
        done: 0,
        total: 0,
    }));
    let remote = imap::list_folders(session).await?;
    ctx.db
        .write(move |conn| {
            let tx = conn.transaction()?;
            let mut discovered = Vec::with_capacity(remote.len());
            let mut known_names = remote
                .iter()
                .map(|folder| folder.name.clone())
                .collect::<std::collections::HashSet<_>>();
            for rf in &remote {
                let role = crate::sync::folder_map::detect_role(rf);
                let selectable = crate::sync::folder_map::should_sync(rf, role);
                let permissions = crate::sync::folder_map::permissions(rf, role);
                let id = repo::folders::upsert_discovered(
                    &tx,
                    account_id,
                    &rf.name,
                    rf.delimiter.as_deref(),
                    role,
                    selectable,
                    permissions,
                )?;
                discovered.push((id, rf.name.clone(), rf.delimiter.clone()));
            }
            for rf in &remote {
                for ancestor in crate::sync::folder_map::ancestor_names(rf) {
                    if !known_names.insert(ancestor.clone()) {
                        continue;
                    }
                    let synthetic = imap::RemoteFolder {
                        name: ancestor,
                        delimiter: rf.delimiter.clone(),
                        attributes: vec!["noselect".into()],
                    };
                    let role = crate::sync::folder_map::detect_role(&synthetic);
                    let id = repo::folders::upsert_discovered(
                        &tx,
                        account_id,
                        &synthetic.name,
                        synthetic.delimiter.as_deref(),
                        role,
                        false,
                        repo::folders::FolderPermissions {
                            can_create_children: synthetic
                                .delimiter
                                .as_deref()
                                .is_some_and(|delimiter| !delimiter.is_empty()),
                            can_rename: false,
                            can_delete: false,
                        },
                    )?;
                    discovered.push((id, synthetic.name, synthetic.delimiter));
                }
            }

            // LIST may return hierarchy containers with `\\Noselect`. They are
            // still first-class tree nodes even though they must never be
            // selected for synchronization.
            let ids_by_name = discovered
                .iter()
                .map(|(id, name, _)| (name.clone(), *id))
                .collect::<std::collections::HashMap<_, _>>();
            for (id, name, delimiter) in discovered {
                let parent_id = delimiter
                    .as_deref()
                    .filter(|delimiter| !delimiter.is_empty())
                    .and_then(|delimiter| name.rsplit_once(delimiter))
                    .and_then(|(parent, _)| ids_by_name.get(parent).copied());
                repo::folders::set_parent(&tx, id, parent_id)?;
            }
            // A LIST without folders can only be a server fault; keep them.
            if !remote.is_empty() {
                repo::folders::mark_missing_on_server(&tx, account_id, &known_names)?;
            }
            tx.commit()?;
            Ok(())
        })
        .await
}

async fn sync_folder(
    ctx: &SyncCtx,
    config: &AccountConfig,
    session: &mut Session,
    folder: &Folder,
    heavy: bool,
    hist_tx: &mpsc::Sender<()>,
    inbox_baseline: &mut Option<InboxBaseline>,
) -> Result<bool> {
    let account_id = config.id;
    let selected = imap::select(session, &folder.path).await?;
    let is_inbox = folder.role.as_deref() == Some(roles::INBOX);

    // UIDVALIDITY handling.
    if let (Some(stored), Some(remote)) = (folder.uidvalidity, selected.uid_validity)
        && stored != remote
    {
        tracing::warn!(folder = %folder.path, "UIDVALIDITY changed; resetting uid map");
        let fid = folder.id;
        ctx.db
            .write(move |conn| repo::folders::reset_uid_mappings(conn, fid))
            .await?;
        if is_inbox {
            *inbox_baseline = None;
        }
    }

    {
        let (fid, uv, un) = (folder.id, selected.uid_validity, selected.uid_next);
        ctx.db
            .write(move |conn| repo::folders::set_uid_state(conn, fid, uv, un, None))
            .await?;
    }

    let fresh_folder = {
        let fid = folder.id;
        ctx.db
            .read(move |conn| repo::folders::get(conn, fid))
            .await?
            .ok_or_else(|| CoreError::NotFound("folder".into()))?
    };

    let initialize_inbox_baseline = is_inbox
        && inbox_baseline
            .as_ref()
            .is_none_or(|b| b.uid_validity != selected.uid_validity);
    let initialize_history = fresh_folder.backfill_cursor.is_none();
    let mailbox_high = if initialize_inbox_baseline || initialize_history {
        Some(mailbox_highest_uid(session, &selected).await?)
    } else {
        None
    };
    if initialize_inbox_baseline {
        *inbox_baseline = Some(InboxBaseline {
            uid_validity: selected.uid_validity,
            first_live_uid: mailbox_high.unwrap_or(0).saturating_add(1).max(1),
        });
    }

    let mut forward_remaining = false;
    if initialize_history {
        // Snapshot the server high-water immediately. The dedicated history
        // worker then walks downward from that point, while this actor owns all
        // later UIDs. This avoids a long initial backfill swallowing genuinely
        // live mail or leaving a gap above a date-filtered search result.
        let live_high = mailbox_high.unwrap_or(0);
        let history_cursor = live_high.saturating_add(1).max(1);
        let fid = fresh_folder.id;
        ctx.db
            .write(move |conn| {
                repo::folders::set_last_seen_uid(conn, fid, i64::from(live_high))?;
                repo::folders::set_backfill(
                    conn,
                    fid,
                    Some(i64::from(history_cursor)),
                    live_high == 0,
                )
            })
            .await?;
        let _ = hist_tx.try_send(());
    } else {
        // When SELECT indicates possible arrivals or omits UIDNEXT, SEARCH is
        // authoritative for the live UIDs. Fetch exact UIDs so sparse UID
        // spaces do not cause empty numeric scan cycles.
        let last_seen = u32::try_from(fresh_folder.last_seen_uid.max(0)).unwrap_or(u32::MAX);
        let selected_high = selected_highest_uid(&selected);
        let new_uids = if selected_high.is_none_or(|high| high > last_seen) {
            imap::uid_search_after(session, last_seen).await?
        } else {
            Vec::new()
        };
        let chunk_len = new_uids.len().min(HEADER_CHUNK);
        forward_remaining = new_uids.len() > chunk_len;
        if let Some(scanned_through) = new_uids
            .get(..chunk_len)
            .and_then(|uids| uids.last())
            .copied()
        {
            let set = imap::uid_set(&new_uids[..chunk_len]);
            let headers = imap::fetch_headers(session, &set).await?;
            let new: Vec<FetchedHeader> = headers
                .into_iter()
                .filter(|h| h.uid > last_seen && h.uid <= scanned_through)
                .collect();
            if !new.is_empty() {
                tracing::info!(
                    account_id,
                    folder = %folder.path,
                    count = new.len(),
                    since_uid = last_seen,
                    "receive: new mail on server",
                );
                let notify_min_uid = if is_inbox {
                    inbox_baseline.as_ref().map(|b| b.first_live_uid)
                } else {
                    None
                };
                let (thread_ids, fresh) = store_headers(
                    ctx,
                    config,
                    &fresh_folder,
                    new,
                    notify_min_uid,
                    NewHeaders::Insert,
                )
                .await?;
                if !thread_ids.is_empty() {
                    ctx.bus.emit(CoreEvent::MailUpdated { thread_ids });
                }
                // MailNew drives the chime + desktop notification, so it only
                // fires for genuinely fresh arrivals: unread, incoming, recent,
                // and never during the launch catch-up cycle (notify=false).
                // New-to-the-folder UIDs that are our own sends, already read
                // on another device, or moved back into the inbox (unarchive /
                // unsnooze) refresh the list via MailUpdated above but stay
                // silent.
                if is_inbox && !fresh.is_empty() {
                    ctx.bus.emit(CoreEvent::MailNew {
                        account_id,
                        thread_ids: fresh,
                    });
                }
            }
            // The exact UID batch was scanned successfully. Advance even when
            // messages disappeared between SEARCH and FETCH, or every row was
            // already present; parse failures are persisted for retry.
            let fid = fresh_folder.id;
            ctx.db
                .write(move |conn| {
                    repo::folders::set_last_seen_uid(conn, fid, i64::from(scanned_through))
                })
                .await?;
        } else if let Some(selected_high) = selected_high
            && selected_high > last_seen
        {
            // UIDNEXT can advance across expunged gaps. Once SEARCH confirms
            // there are no live messages in the gap, skip it permanently.
            let fid = fresh_folder.id;
            ctx.db
                .write(move |conn| {
                    repo::folders::set_last_seen_uid(conn, fid, i64::from(selected_high))
                })
                .await?;
        }
        if heavy {
            reconcile_flags(ctx, session, &fresh_folder).await?;
            reconcile_mailbox(ctx, config, session, &fresh_folder).await?;
        }
        // Historical backfill (extending the window downward) also runs on the
        // history connection; just make sure it's awake.
        if !fresh_folder.backfill_done {
            let _ = hist_tx.try_send(());
        }
    }
    Ok(forward_remaining)
}

fn selected_highest_uid(selected: &imap::SelectedFolder) -> Option<u32> {
    let uid_next = u32::try_from(selected.uid_next?).ok()?;
    let high = uid_next.checked_sub(1)?;
    // With unique positive UIDs, a valid high watermark cannot be below the
    // number of live messages. Treat inconsistent server metadata like an
    // omitted UIDNEXT and fall back to SEARCH.
    (high >= selected.exists).then_some(high)
}

async fn mailbox_highest_uid(
    session: &mut Session,
    selected: &imap::SelectedFolder,
) -> Result<u32> {
    if selected.exists == 0 {
        return Ok(0);
    }
    if let Some(high) = selected_highest_uid(selected) {
        return Ok(high);
    }
    let high = imap::uid_search_all(session)
        .await?
        .into_iter()
        .max()
        .ok_or_else(|| {
            CoreError::Imap(format!(
                "mailbox reported {} messages but UID SEARCH returned none",
                selected.exists
            ))
        })?;
    if high < selected.exists {
        return Err(CoreError::Imap(format!(
            "mailbox reported {} messages but highest UID was {high}",
            selected.exists
        )));
    }
    Ok(high)
}

/// Dedicated header-backfill connection. Performs first-time folder backfills
/// and extends the historical window on its own IMAP session, so the sync
/// actor's cycles stay short (actions, new-mail checks, reconciliation) and it
/// can sit in IDLE - new mail keeps landing within seconds even while months
/// of history are downloading. Woken by the actor's nudges or a backstop tick;
/// exits when the account handle is dropped.
async fn run_history_backfill(
    ctx: SyncCtx,
    config: AccountConfig,
    mut rx: mpsc::Receiver<()>,
    pool_tx: mpsc::Sender<()>,
    mut settings_rx: watch::Receiver<crate::models::AccountSettings>,
) {
    let account_id = config.id;
    let mut session: Option<Session> = None;
    loop {
        let (closed, settings_changed) = tokio::select! {
            result = tokio::time::timeout(
                std::time::Duration::from_secs(CYCLE_SECS),
                rx.recv(),
            ) => (matches!(result, Ok(None)), false),
            result = settings_rx.changed() => (result.is_err(), result.is_ok()),
        };
        if closed {
            return; // channel closed: account removed / shutdown
        }
        while rx.try_recv().is_ok() {} // coalesce piled-up nudges
        if settings_changed {
            settings_rx.borrow_and_update();
            // The settings write initially reopens every folder, but an
            // in-flight step using the previous period may finish afterward
            // and mark its old boundary complete. Reopen once more only after
            // this worker has observed the new value so that widening or
            // narrowing the period cannot be lost to that race.
            if let Err(error) = ctx
                .db
                .write(move |conn| repo::folders::reopen_backfill_for_account(conn, account_id))
                .await
            {
                tracing::warn!(
                    account_id,
                    error = %error,
                    "could not reopen history after settings change"
                );
            }
        }

        // Work passes: each pass advances every folder that still needs
        // headers by one step - a full (resumable) initial backfill, or one
        // HISTORY_CHUNK extension - until nothing is left or the connection
        // breaks. One step per pass keeps folders progressing evenly.
        'work: loop {
            if settings_rx.has_changed().unwrap_or(true) {
                break 'work;
            }
            let mut progressed = match retry_one_header_failure(&ctx, &config, &mut session).await {
                Ok(progressed) => progressed,
                Err(e) => {
                    tracing::warn!(account_id, error = %e, "header retry failed; reconnecting");
                    if let Some(s) = session.take() {
                        imap::logout(s).await;
                    }
                    break 'work;
                }
            };
            let folders = match ctx
                .db
                .read(move |conn| repo::folders::list(conn, Some(account_id)))
                .await
            {
                Ok(f) => f,
                Err(_) => break,
            };
            let mut ordered: Vec<&Folder> =
                folders.iter().filter(|folder| folder.selectable).collect();
            ordered.sort_by_key(|f| match f.role.as_deref() {
                Some(roles::INBOX) => 0,
                Some(roles::SENT) => 1,
                Some(roles::DRAFTS) => 2,
                Some(roles::ARCHIVE) => 3,
                _ => 4,
            });

            for folder in ordered {
                if folder.role.as_deref() == Some(roles::ALL) {
                    continue;
                }
                if folder.backfill_cursor.is_some() && folder.backfill_done {
                    continue;
                }
                if session.is_none() {
                    match connect(&ctx, &config).await {
                        Ok(s) => session = Some(s),
                        Err(e) => {
                            tracing::warn!(account_id, error = %e, "history backfill: connect failed");
                            break 'work; // retry on the next nudge/tick
                        }
                    }
                }
                let Some(s) = session.as_mut() else {
                    break 'work;
                };
                let step = async {
                    imap::select(s, &folder.path).await?;
                    // Re-read: the sync actor may have advanced this folder.
                    let fid = folder.id;
                    let fresh = ctx
                        .db
                        .read(move |conn| repo::folders::get(conn, fid))
                        .await?
                        .ok_or_else(|| CoreError::NotFound("folder".into()))?;
                    if fresh.backfill_cursor.is_none() || fresh.backfill_done {
                        return Ok::<bool, CoreError>(false);
                    }
                    let mail_history = { settings_rx.borrow().mail_history };
                    match mail_history {
                        crate::models::MailHistory::All => {
                            extend_history(&ctx, &config, s, &fresh).await?;
                        }
                        mail_history => {
                            bounded_history_backfill(&ctx, &config, s, &fresh, mail_history)
                                .await?;
                        }
                    }
                    Ok::<bool, CoreError>(true)
                }
                .await;
                match step {
                    Ok(did_step) => {
                        progressed |= did_step;
                        // New headers mean new missing bodies.
                        if did_step {
                            let _ = pool_tx.try_send(());
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            account_id,
                            folder = %folder.path,
                            error = %e,
                            "history backfill: step failed; dropping session",
                        );
                        if let Some(s) = session.take() {
                            imap::logout(s).await;
                        }
                        break 'work; // retry on the next nudge/tick
                    }
                }
                if settings_rx.has_changed().unwrap_or(true) {
                    break 'work;
                }
            }
            if !progressed {
                // Fully caught up. Don't hold an idle connection open between
                // ticks - servers reap them and the next step would fail.
                if let Err(e) = settle_unlinked_mail(&ctx, &config, &mut session).await {
                    tracing::warn!(account_id, error = %e, "could not settle mail without a UID; retrying later");
                }
                if let Some(s) = session.take() {
                    imap::logout(s).await;
                }
                break;
            }
        }
    }
}

/// Settles mail that lost its UIDs to a server change, a recreated mailbox or
/// a folder the server no longer lists, once every folder has its history.
/// Each folder's mail since the oldest waiting message is matched by
/// Message-ID first, so mail outside the history is found too, and whatever
/// still waits is no longer on the server.
async fn settle_unlinked_mail(
    ctx: &SyncCtx,
    config: &AccountConfig,
    session: &mut Option<Session>,
) -> Result<()> {
    let account_id = config.id;
    let Some(oldest) = ctx
        .db
        .read(move |conn| repo::messages::oldest_relink_pending(conn, account_id))
        .await?
    else {
        return Ok(());
    };
    let folders = ctx
        .db
        .read(move |conn| repo::folders::list(conn, Some(account_id)))
        .await?;
    let synced = folders
        .iter()
        .filter(|folder| folder.selectable && folder.role.as_deref() != Some(roles::ALL))
        .collect::<Vec<_>>();
    // Until the server's folders are known, such as right after a switch from
    // JMAP, nothing could have been matched.
    if !synced
        .iter()
        .any(|folder| folder.role.as_deref() == Some(roles::INBOX))
        || synced
            .iter()
            .any(|folder| folder.backfill_cursor.is_none() || !folder.backfill_done)
    {
        return Ok(());
    }
    // A day's margin covers servers recording slightly different times.
    let since = chrono::DateTime::from_timestamp_millis(oldest.saturating_sub(24 * 60 * 60 * 1000))
        .unwrap_or(chrono::DateTime::<chrono::Utc>::UNIX_EPOCH)
        .date_naive();
    for folder in synced {
        relink_waiting_mail(ctx, config, session, folder, since).await?;
    }
    let (thread_ids, stale_paths) = ctx
        .db
        .write(move |conn| {
            let tx = conn.transaction()?;
            let dropped = repo::messages::drop_unrelinked_imap_mail(&tx, account_id)?;
            tx.commit()?;
            Ok(dropped)
        })
        .await?;
    for path in stale_paths {
        let _ = tokio::fs::remove_file(path).await;
    }
    if !thread_ids.is_empty() {
        ctx.bus.emit(CoreEvent::MailUpdated { thread_ids });
    }
    Ok(())
}

/// Links waiting messages to the folder's mail received since `since` that
/// no local message stands for, without storing the rest.
async fn relink_waiting_mail(
    ctx: &SyncCtx,
    config: &AccountConfig,
    session: &mut Option<Session>,
    folder: &Folder,
    since: chrono::NaiveDate,
) -> Result<()> {
    if session.is_none() {
        *session = Some(connect(ctx, config).await?);
    }
    let Some(s) = session.as_mut() else {
        return Ok(());
    };
    imap::select(s, &folder.path).await?;
    let uids = imap::uid_search_since(s, since).await?;
    let folder_id = folder.id;
    let linked: std::collections::HashSet<i64> = ctx
        .db
        .read(move |conn| repo::messages::uids_in_folder(conn, folder_id))
        .await?
        .into_iter()
        .map(|(_, uid)| uid)
        .collect();
    let older = uids
        .into_iter()
        .filter(|uid| !linked.contains(&i64::from(*uid)))
        .collect::<Vec<_>>();
    for chunk in older.chunks(HEADER_CHUNK) {
        let headers = imap::fetch_headers(s, &imap::uid_set(chunk)).await?;
        let (thread_ids, _) =
            store_headers(ctx, config, folder, headers, None, NewHeaders::Skip).await?;
        if !thread_ids.is_empty() {
            ctx.bus.emit(CoreEvent::MailUpdated { thread_ids });
        }
    }
    Ok(())
}

/// Retry one header that was fetched successfully but could not be parsed or
/// persisted. The forward scan watermark is allowed to advance because this
/// durable ledger keeps the UID independently retryable.
async fn retry_one_header_failure(
    ctx: &SyncCtx,
    config: &AccountConfig,
    session: &mut Option<Session>,
) -> Result<bool> {
    let account_id = config.id;
    let failure = ctx
        .db
        .read(move |conn| {
            Ok(
                repo::sync_failures::due_headers(conn, account_id, now_ms(), 1)?
                    .into_iter()
                    .next(),
            )
        })
        .await?;
    let Some(failure) = failure else {
        return Ok(false);
    };
    let folder_id = failure
        .folder_id
        .ok_or_else(|| CoreError::Other("header retry missing folder".into()))?;
    let uid = failure
        .uid
        .ok_or_else(|| CoreError::Other("header retry missing UID".into()))?;
    let folder = ctx
        .db
        .read(move |conn| repo::folders::get(conn, folder_id))
        .await?
        .ok_or_else(|| CoreError::NotFound(format!("folder {folder_id}")))?;
    if !folder.selectable {
        ctx.db
            .write(move |conn| {
                repo::sync_failures::clear_header(conn, folder_id, uid)?;
                Ok(())
            })
            .await?;
        return Ok(true);
    }
    if session.is_none() {
        *session = Some(connect(ctx, config).await?);
    }
    let s = session
        .as_mut()
        .ok_or_else(|| CoreError::Other("header retry lost its IMAP session".into()))?;
    imap::select(s, &folder.path).await?;
    let headers = imap::fetch_headers(s, &uid.to_string()).await?;
    if headers.is_empty() {
        let error = "server no longer returns this UID";
        let retry_at = now_ms() + 5 * 60_000;
        ctx.db
            .write(move |conn| {
                repo::sync_failures::record_header(conn, folder_id, uid, Some(retry_at), error)?;
                Ok(())
            })
            .await?;
        return Ok(true);
    }
    let (thread_ids, _) =
        store_headers(ctx, config, &folder, headers, None, NewHeaders::Insert).await?;
    if !thread_ids.is_empty() {
        ctx.bus.emit(CoreEvent::MailUpdated { thread_ids });
    }
    Ok(true)
}

/// Fetch one newest-first chunk within a bounded account history window. The
/// SEARCH is intentionally repeated between chunks: it is cheap, makes the
/// operation resumable without another cursor format, and lets a changed
/// account setting take effect after at most one FETCH batch.
async fn bounded_history_backfill(
    ctx: &SyncCtx,
    config: &AccountConfig,
    session: &mut Session,
    folder: &Folder,
    mail_history: crate::models::MailHistory,
) -> Result<()> {
    let account_id = config.id;
    let since = mail_history
        .cutoff_date_at(now_ms())
        .ok_or_else(|| CoreError::Other("bounded history is missing a cutoff".into()))?;
    let uids = imap::uid_search_since(session, since).await?;

    // Resume support: a previous run may have died partway (flaky server, app
    // quit) - skip UIDs whose headers already landed so a restart only fetches
    // what's missing instead of re-downloading the whole window every time.
    let existing: std::collections::HashSet<i64> = {
        let fid = folder.id;
        ctx.db
            .read(move |conn| repo::messages::uids_in_folder(conn, fid))
            .await?
            .into_iter()
            .map(|(_, uid)| uid)
            .collect()
    };
    let missing: Vec<u32> = uids
        .iter()
        .copied()
        .filter(|u| !existing.contains(&(*u as i64)))
        .collect();
    let total = uids.len() as u64;
    let start = missing.len().saturating_sub(HEADER_CHUNK);
    let chunk = &missing[start..];
    let mut fetched = 0u64;
    if !chunk.is_empty() {
        let set = imap::uid_set(chunk);
        let headers = imap::fetch_headers(session, &set).await?;
        fetched = headers.len() as u64;
        let (thread_ids, _) =
            store_headers(ctx, config, folder, headers, None, NewHeaders::Insert).await?;
        if !thread_ids.is_empty() {
            ctx.bus.emit(CoreEvent::MailUpdated { thread_ids });
        }
    }

    // Keep the oldest UID inside the bounded result. If the user later chooses
    // All, the unbounded worker can continue immediately below this point.
    let complete = start == 0;
    let bounded_min_uid = (if complete {
        uids.first()
    } else {
        chunk.first()
    })
    .copied()
    .map(i64::from)
    .or(folder.backfill_cursor)
    .unwrap_or(1);
    // Never move the deepest downloaded cursor forward when the user narrows
    // the window. If they later select All, already-cached history is not
    // needlessly fetched again.
    let min_uid = folder
        .backfill_cursor
        .map_or(bounded_min_uid, |existing| existing.min(bounded_min_uid));
    let fid = folder.id;
    ctx.db
        .write(move |conn| repo::folders::set_backfill(conn, fid, Some(min_uid), complete))
        .await?;
    let already_present = total.saturating_sub(missing.len() as u64);
    ctx.bus.emit(CoreEvent::SyncProgress(SyncProgress {
        account_id,
        folder: folder.path.clone(),
        phase: "headers".into(),
        done: already_present.saturating_add(fetched).min(total),
        total,
    }));
    emit_sync_status(ctx, account_id).await;
    Ok(())
}

async fn extend_history(
    ctx: &SyncCtx,
    config: &AccountConfig,
    session: &mut Session,
    folder: &Folder,
) -> Result<()> {
    let Some(cursor) = folder.backfill_cursor else {
        return Ok(());
    };
    if cursor <= 1 {
        let fid = folder.id;
        ctx.db
            .write(move |conn| repo::folders::set_backfill(conn, fid, Some(1), true))
            .await?;
        return Ok(());
    }
    let hi = (cursor - 1).max(1) as u32;
    let lo = hi.saturating_sub(HISTORY_CHUNK - 1).max(1);
    let set = format!("{lo}:{hi}");
    let headers = imap::fetch_headers(session, &set).await?;
    let (thread_ids, _) =
        store_headers(ctx, config, folder, headers, None, NewHeaders::Insert).await?;
    if !thread_ids.is_empty() {
        ctx.bus.emit(CoreEvent::MailUpdated { thread_ids });
    }
    let fid = folder.id;
    let new_cursor = lo as i64;
    ctx.db
        .write(move |conn| {
            repo::folders::set_backfill(conn, fid, Some(new_cursor), new_cursor <= 1)
        })
        .await?;
    ctx.bus.emit(CoreEvent::SyncProgress(SyncProgress {
        account_id: config.id,
        folder: folder.path.clone(),
        phase: "history".into(),
        done: 0,
        total: 0,
    }));
    emit_sync_status(ctx, config.id).await;
    Ok(())
}

/// What `store_headers` does with headers that match no local message.
#[derive(Clone, Copy, PartialEq, Eq)]
enum NewHeaders {
    Insert,
    /// Mail fetched only to link waiting local messages with the same
    /// Message-ID. Nothing else about it is kept, and the folder's scan
    /// position stays where the live sync left it.
    Skip,
}

/// Insert fetched headers. Returns `(all, fresh)` thread ids: `all` for list
/// refreshes, of the newly inserted messages and of those that moved to the
/// folder of their server copy, `fresh` only for threads that gained an
/// unread, incoming, recent message (chime/notification-worthy).
async fn store_headers(
    ctx: &SyncCtx,
    config: &AccountConfig,
    folder: &Folder,
    headers: Vec<FetchedHeader>,
    notify_min_uid: Option<u32>,
    new_headers: NewHeaders,
) -> Result<(Vec<i64>, Vec<i64>)> {
    if headers.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    let account_id = config.id;
    let folder_id = folder.id;
    let folder_role = folder.role.clone();

    ctx.db
        .write(move |conn| {
            let tx = conn.transaction()?;
            let mut importer = super::ingest::Importer::new(&tx, account_id)?;
            let mut max_uid: i64 = 0;
            let mut inserted = 0u32;
            let mut moved_threads = Vec::new();

            for fh in &headers {
                max_uid = max_uid.max(fh.uid as i64);

                // History FETCH ranges include messages awaiting expunge, and
                // flags can change between SEARCH and FETCH. Never import
                // these into the visible cache or generate arrival events.
                if fh.flags.deleted {
                    repo::sync_failures::clear_header(&tx, folder_id, fh.uid as i64)?;
                    continue;
                }

                // Already have this UID?
                if let Some(existing) =
                    repo::messages::by_folder_uid(&tx, folder_id, fh.uid as i64)?
                {
                    if let Some(plan) = fh.mime_plan.as_ref() {
                        repo::messages::set_mime_plan(&tx, existing.id, Some(plan))?;
                        repo::messages::upsert_planned_attachments(
                            &tx,
                            existing.id,
                            &plan.attachments,
                        )?;
                    }
                    let _ = repo::sync_failures::clear_header(&tx, folder_id, fh.uid as i64);
                    continue;
                }

                let parsed = match crate::mime::parse_header_block(&fh.header_bytes) {
                    Ok(p) => p,
                    Err(_) if new_headers == NewHeaders::Skip => continue,
                    Err(e) => {
                        let retry_at = now_ms() + 60_000;
                        repo::sync_failures::record_header(
                            &tx,
                            folder_id,
                            fh.uid as i64,
                            Some(retry_at),
                            &e.to_string(),
                        )?;
                        tracing::warn!(
                            account_id,
                            folder_id,
                            uid = fh.uid,
                            error = %e,
                            "could not parse fetched header; queued for retry",
                        );
                        continue;
                    }
                };

                // Dedupe: same Message-ID already stored (e.g. our own sent
                // message APPENDed earlier, or Gmail label duplication).
                let mut existing = match &parsed.message_id {
                    Some(mid) => repo::messages::by_message_id(&tx, account_id, mid)?,
                    None => None,
                };
                // A stable client marker survives providers rewriting Message-ID.
                // Never guess identity from a subject or a timestamp.
                if existing.is_none()
                    && folder_role.as_deref() == Some(roles::SENT)
                    && let Some(submission_id) = parsed.submission_id.as_deref()
                {
                    existing = repo::messages::outgoing_sent_adoption_candidate(
                        &tx, account_id, folder_id, submission_id,
                    )?;
                }
                if let Some(existing) = existing {
                    // SMTP and IMAP workers run concurrently: the server copy
                    // can arrive before send_action commits local completion.
                    let sending = folder_role.as_deref() == Some(roles::SENT)
                        && existing.raw_path.is_some()
                        && tx.query_row(
                            "SELECT EXISTS(SELECT 1 FROM pending_actions a JOIN messages m ON m.id=a.message_id
                             WHERE a.message_id=?1 AND a.kind='send' AND a.state='inflight' AND m.is_draft=1)",
                            [existing.id], |r| r.get::<_, bool>(0),
                        )?;
                    if (existing.uid.is_none() || sending)
                        && !repo::actions::has_active_move_from(&tx, existing.id, folder_id)?
                    {
                        repo::messages::set_uid_and_folder(
                            &tx,
                            existing.id,
                            folder_id,
                            Some(fh.uid as i64),
                        )?;
                        // Its copy on the server is in another folder.
                        if existing.folder_id != Some(folder_id) {
                            moved_threads.extend(existing.thread_id);
                        }
                    }
                    if let Some(plan) = fh.mime_plan.as_ref() {
                        repo::messages::set_mime_plan(&tx, existing.id, Some(plan))?;
                        repo::messages::upsert_planned_attachments(
                            &tx,
                            existing.id,
                            &plan.attachments,
                        )?;
                    }
                    let _ = repo::sync_failures::clear_header(&tx, folder_id, fh.uid as i64);
                    continue;
                }
                if new_headers == NewHeaders::Skip {
                    continue;
                }

                let imported = importer.insert(
                    &tx,
                    &parsed,
                    &super::ingest::Delivery {
                        folder_id,
                        folder_role: folder_role.as_deref(),
                        uid: Some(fh.uid as i64),
                        received_ms: fh.internal_date_ms,
                        seen: fh.flags.seen,
                        flagged: fh.flags.flagged,
                        draft: fh.flags.draft,
                        has_attachments: fh.has_attachments,
                        size: fh.size.map(|s| s as i64),
                    },
                )?;
                let msg_id = imported.message_id;
                if let Some(plan) = fh.mime_plan.as_ref() {
                    repo::messages::set_mime_plan(&tx, msg_id, Some(plan))?;
                    repo::messages::upsert_planned_attachments(&tx, msg_id, &plan.attachments)?;
                }
                let _ = repo::sync_failures::clear_header(&tx, folder_id, fh.uid as i64);
                inserted += 1;
                repo::labels::reconcile_keywords(&tx, msg_id, &fh.flags.keywords)?;
                repo::threads::recompute(&tx, imported.thread_id)?;
                repo::search::index_message(&tx, msg_id)?;
                importer.learn_contacts(&tx, &parsed, &imported)?;
                // Only mail past the inbox watermark arrived while this
                // account was being followed.
                let live = notify_min_uid.is_some_and(|min_uid| fh.uid >= min_uid);
                importer.announce(&tx, &imported, live)?;
            }

            if max_uid > 0 && new_headers == NewHeaders::Insert {
                repo::folders::set_last_seen_uid(&tx, folder_id, max_uid)?;
            }
            let (mut thread_ids, fresh_ids) = importer.finish(&tx)?;
            for thread_id in moved_threads {
                if !thread_ids.contains(&thread_id) {
                    thread_ids.push(thread_id);
                }
            }
            tx.commit()?;
            if inserted > 0 {
                tracing::info!(
                    account_id,
                    folder = %folder_role.as_deref().unwrap_or("?"),
                    fetched = headers.len(),
                    inserted,
                    threads = thread_ids.len(),
                    "receive: stored new message headers",
                );
            }
            Ok((thread_ids, fresh_ids))
        })
        .await
}

/// Reconcile read/star flags for the most recent window of UIDs.
async fn reconcile_flags(ctx: &SyncCtx, session: &mut Session, folder: &Folder) -> Result<()> {
    let folder_id = folder.id;
    let max_uid = ctx
        .db
        .read(move |conn| repo::messages::max_uid_in_folder(conn, folder_id))
        .await?;
    if max_uid == 0 {
        return Ok(());
    }
    let lo = (max_uid - FLAG_WINDOW).max(1);
    let set = format!("{lo}:{max_uid}");
    let remote_flags = imap::fetch_flags(session, &set).await?;

    let changed = ctx
        .db
        .write(move |conn| {
            let tx = conn.transaction()?;
            let mut changed_threads: Vec<i64> = Vec::new();
            for (uid, flags) in &remote_flags {
                if let Some(row) = repo::messages::by_folder_uid(&tx, folder_id, *uid as i64)? {
                    // Local pending intent wins over remote state.
                    if repo::actions::has_pending_for_message(&tx, row.id)? {
                        continue;
                    }
                    let flags_changed =
                        row.is_read != flags.seen || row.is_starred != flags.flagged;
                    if flags_changed {
                        repo::messages::set_flags(&tx, row.id, flags.seen, flags.flagged)?;
                    }
                    let labels_changed =
                        repo::labels::reconcile_keywords(&tx, row.id, &flags.keywords)?;
                    if (flags_changed || labels_changed)
                        && let Some(tid) = row.thread_id
                    {
                        repo::threads::recompute(&tx, tid)?;
                        if !changed_threads.contains(&tid) {
                            changed_threads.push(tid);
                        }
                    }
                }
            }
            tx.commit()?;
            Ok(changed_threads)
        })
        .await?;

    if !changed.is_empty() {
        ctx.bus.emit(CoreEvent::MailUpdated {
            thread_ids: changed,
        });
    }
    Ok(())
}

/// Remove local messages that vanished or are marked `\\Deleted` on the
/// server (within the synced window). Search the whole folder so this also
/// repairs cached messages outside the recent flag-reconciliation window.
/// Re-import missing visible UIDs in that window as well: another client can
/// clear `\\Deleted` without assigning a new UID or advancing UIDNEXT.
async fn reconcile_mailbox(
    ctx: &SyncCtx,
    config: &AccountConfig,
    session: &mut Session,
    folder: &Folder,
) -> Result<()> {
    let remote_uids = imap::uid_search_undeleted(session).await?;
    let folder_id = folder.id;
    let cursor = folder.backfill_cursor.unwrap_or(i64::MAX);
    let last_seen = folder.last_seen_uid;

    let (changed, missing) = ctx
        .db
        .write(move |conn| {
            let remote: std::collections::HashSet<i64> =
                remote_uids.iter().map(|u| *u as i64).collect();
            let local = repo::messages::uids_in_folder(conn, folder_id)?;
            let local_uids: std::collections::HashSet<i64> =
                local.iter().map(|(_, uid)| *uid).collect();
            // Bound recovery to already-scanned history and one batch per
            // pass. The live and history workers own everything outside it.
            let missing: Vec<u32> = remote_uids
                .into_iter()
                .filter(|uid| {
                    let uid = i64::from(*uid);
                    uid >= cursor && uid <= last_seen && !local_uids.contains(&uid)
                })
                .take(HEADER_CHUNK)
                .collect();
            let tx = conn.transaction()?;
            let mut changed_threads: Vec<i64> = Vec::new();
            for (id, uid) in local {
                if uid >= cursor
                    && !remote.contains(&uid)
                    && let Some(row) = repo::messages::get_row(&tx, id)?
                {
                    // Skip if a pending action is mid-flight for it (a
                    // local move produces exactly this state).
                    if repo::actions::has_pending_for_message(&tx, id)? {
                        continue;
                    }
                    repo::messages::delete(&tx, id)?;
                    if let Some(tid) = row.thread_id {
                        repo::threads::recompute(&tx, tid)?;
                        if !changed_threads.contains(&tid) {
                            changed_threads.push(tid);
                        }
                    }
                }
            }
            tx.commit()?;
            Ok((changed_threads, missing))
        })
        .await?;

    if !changed.is_empty() {
        ctx.bus.emit(CoreEvent::MailUpdated {
            thread_ids: changed,
        });
    }
    if !missing.is_empty() {
        let headers = imap::fetch_headers(session, &imap::uid_set(&missing)).await?;
        // Restoration is a list update, never a new-mail notification.
        let (thread_ids, _) =
            store_headers(ctx, config, folder, headers, None, NewHeaders::Insert).await?;
        if !thread_ids.is_empty() {
            ctx.bus.emit(CoreEvent::MailUpdated { thread_ids });
        }
    }
    Ok(())
}

/// One unit of backfill-pool work: a single folder's chunk of UIDs, pulled in
/// one batched FETCH.
struct BodyChunk {
    folder_id: i64,
    folder_name: String,
    uid_validity: Option<i64>,
    /// (message_id, uid) pairs.
    items: Vec<(i64, i64)>,
}

struct CurrentBodyRow {
    folder_id: Option<i64>,
    uid: Option<i64>,
    body_state: String,
}

fn current_body_row(
    conn: &rusqlite::Connection,
    message_id: i64,
) -> Result<Option<CurrentBodyRow>> {
    Ok(conn
        .query_row(
            "SELECT folder_id, uid, body_state FROM messages WHERE id = ?1",
            rusqlite::params![message_id],
            |row| {
                Ok(CurrentBodyRow {
                    folder_id: row.get(0)?,
                    uid: row.get(1)?,
                    body_state: row.get(2)?,
                })
            },
        )
        .optional()?)
}

fn is_current_background_row(row: &CurrentBodyRow, folder_id: i64, uid: i64) -> bool {
    row.folder_id == Some(folder_id) && row.uid == Some(uid) && row.body_state == "none"
}

enum BodyChunkError {
    /// The selected IMAP protocol stream is no longer safe to reuse. Reconnect
    /// and reduce the request before retrying.
    Reconnect(CoreError),
    /// A local DB/task/persistence error cannot be repaired by reconnecting to
    /// IMAP or bisecting the same request.
    Fatal(CoreError),
}

fn classify_body_chunk_error(error: CoreError) -> BodyChunkError {
    match error {
        error @ (CoreError::Imap(_)
        | CoreError::ImapParse(_)
        | CoreError::Network(_)
        | CoreError::ResponseTooLarge { .. }
        | CoreError::Offline) => BodyChunkError::Reconnect(error),
        error => BodyChunkError::Fatal(error),
    }
}

#[derive(Clone)]
struct PlannedBodyFetch {
    message_id: i64,
    uid: u32,
    plan: crate::mime::MimePlan,
}

struct DecodedSelectiveContent {
    message_id: i64,
    uid: u32,
    plan: crate::mime::MimePlan,
    text_body: Option<String>,
    html_body: Option<String>,
    calendar_parts: Vec<String>,
    snippet: String,
}

#[derive(Clone, Copy)]
enum SelectivePersistGuard {
    Background { folder_id: i64 },
    Priority { folder_id: i64 },
}

fn can_persist_selective_content(
    row: &CurrentBodyRow,
    guard: SelectivePersistGuard,
    uid: u32,
) -> bool {
    let (folder_id, expected_state) = match guard {
        SelectivePersistGuard::Background { folder_id } => (folder_id, "none"),
        SelectivePersistGuard::Priority { folder_id } => (folder_id, "fetching"),
    };
    row.folder_id == Some(folder_id)
        && row.uid == Some(i64::from(uid))
        && row.body_state == expected_state
}

// All accounts share admission for background body batches. This budgets
// encoded BODYSTRUCTURE estimates, not decoded strings or total process RSS.
// Oversized single messages run alone; foreground message reads stay independent.
const BACKGROUND_BODY_BUDGET_KIB: u32 = 16 * 1024;
static BACKGROUND_BODY_BUDGET: std::sync::OnceLock<std::sync::Arc<tokio::sync::Semaphore>> =
    std::sync::OnceLock::new();

async fn reserve_background_body_bytes(bytes: u64) -> tokio::sync::OwnedSemaphorePermit {
    let permits = bytes
        .div_ceil(1024)
        .clamp(1, u64::from(BACKGROUND_BODY_BUDGET_KIB)) as u32;
    BACKGROUND_BODY_BUDGET
        .get_or_init(|| {
            std::sync::Arc::new(tokio::sync::Semaphore::new(
                BACKGROUND_BODY_BUDGET_KIB as usize,
            ))
        })
        .clone()
        .acquire_many_owned(permits)
        .await
        .expect("body budget is never closed")
}

fn planned_text_bytes(item: &PlannedBodyFetch) -> u64 {
    item.plan
        .text_sections
        .iter()
        .map(|section| u64::from(section.size))
        .sum::<u64>()
        .max(1)
}

/// Split one shared-layout group by both UID count and BODYSTRUCTURE's encoded
/// byte estimate. The first oversized message still gets its own batch.
fn selective_fetch_batches(
    items: Vec<PlannedBodyFetch>,
    max_bytes: u64,
) -> Vec<Vec<PlannedBodyFetch>> {
    let mut batches = Vec::new();
    let mut batch = Vec::new();
    let mut bytes = 0u64;
    for item in items {
        let item_bytes = planned_text_bytes(&item);
        if !batch.is_empty()
            && (batch.len() >= BODY_FETCH_CHUNK || bytes.saturating_add(item_bytes) > max_bytes)
        {
            batches.push(std::mem::take(&mut batch));
            bytes = 0;
        }
        bytes = bytes.saturating_add(item_bytes);
        batch.push(item);
    }
    if !batch.is_empty() {
        batches.push(batch);
    }
    batches
}

#[cfg(test)]
mod content_batch_tests {
    use super::*;
    use crate::db::testutil;

    #[test]
    fn content_retries_escalate_at_exact_boundaries_and_reset_when_cleared() {
        let mut conn = testutil::conn();
        testutil::seed_account(&conn);
        let (_, id) = testutil::seed_message(&conn, "sender@test.dev", "Retry", false);
        let mut now = 1_000;
        let delays = [60_000; 5]
            .into_iter()
            .chain([900_000; 15])
            .chain([86_400_000; 2]);
        for (index, delay) in delays.enumerate() {
            record_content_failure_details_at(&mut conn, vec![(id, "decode".into())], now).unwrap();
            let deadline = now + delay;
            assert!(
                repo::sync_failures::due_content(&conn, 1, deadline - 1, 10)
                    .unwrap()
                    .is_empty()
            );
            let due = repo::sync_failures::due_content(&conn, 1, deadline, 10).unwrap();
            assert_eq!(due.len(), 1);
            assert_eq!(due[0].message_id, Some(id));
            assert_eq!(due[0].attempts, index as i64 + 1);
            assert_eq!(due[0].next_retry_at, Some(deadline));
            assert_eq!(due[0].created_at, 1_000);
            assert_eq!(due[0].updated_at, now);
            now = deadline;
        }

        // Retry counts are per message, and clearing a recovered message
        // restarts the short delay without affecting other messages.
        let (_, fresh) = testutil::seed_message(&conn, "sender@test.dev", "Fresh", false);
        record_content_failure_details_at(&mut conn, vec![(fresh, "fetch".into())], now).unwrap();
        assert_eq!(
            repo::sync_failures::content_attempts(&conn, id).unwrap(),
            22
        );
        repo::sync_failures::clear_content(&conn, id).unwrap();
        record_content_failure_details_at(&mut conn, vec![(id, "fetch".into())], now).unwrap();
        let due = repo::sync_failures::due_content(&conn, 1, now + 60_000, 10).unwrap();
        assert_eq!(due.len(), 2);
        for failure in due {
            assert_eq!(failure.attempts, 1);
            assert_eq!(failure.next_retry_at, Some(now + 60_000));
            assert_eq!(failure.last_error, "fetch");
        }
    }

    #[test]
    fn content_failure_batch_rolls_back_when_attempt_count_cannot_be_read() {
        let mut conn = testutil::conn();
        testutil::seed_account(&conn);
        let (_, first) = testutil::seed_message(&conn, "sender@test.dev", "First", false);
        let (_, second) = testutil::seed_message(&conn, "sender@test.dev", "Second", false);
        let failure =
            repo::sync_failures::record_content_at(&conn, second, Some(123), "old", 1).unwrap();
        // SQLite permits a non-integer value in this column. Reading it must
        // fail rather than silently scheduling a new one-minute retry.
        conn.execute(
            "UPDATE sync_failures SET attempts = 'invalid' WHERE id = ?1",
            [failure],
        )
        .unwrap();
        assert!(
            record_content_failure_details_at(
                &mut conn,
                vec![(first, "new".into()), (second, "new".into())],
                1_000,
            )
            .is_err()
        );
        assert_eq!(
            repo::sync_failures::content_attempts(&conn, first).unwrap(),
            0
        );
        let stored: (String, i64, String) = conn
            .query_row(
                "SELECT attempts, next_retry_at, last_error FROM sync_failures WHERE id = ?1",
                [failure],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(stored, ("invalid".into(), 123, "old".into()));
    }

    #[test]
    fn late_content_failures_skip_cached_and_deleted_messages() {
        let mut conn = testutil::conn();
        testutil::seed_account(&conn);
        let (_, cached) = testutil::seed_message(&conn, "sender@test.dev", "Cached", false);
        let (_, deleted) = testutil::seed_message(&conn, "sender@test.dev", "Deleted", false);
        repo::messages::set_body_state(&conn, cached, "cached").unwrap();
        repo::messages::delete(&conn, deleted).unwrap();
        record_content_failure_details_at(
            &mut conn,
            vec![(cached, "late".into()), (deleted, "late".into())],
            1_000,
        )
        .unwrap();
        assert!(
            repo::sync_failures::due_content(&conn, 1, i64::MAX, 10)
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn background_planning_pages_are_shared_and_waiters_cancel_cleanly() {
        let first = reserve_background_plan_page().await;
        let second = reserve_background_plan_page().await;
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(10),
                reserve_background_plan_page()
            )
            .await
            .is_err()
        );
        // The worker must keep the slot after the parent releases its handle,
        // and dropping the round must abort the worker rather than detach it.
        let first = Arc::new(first);
        let worker_slot = Arc::clone(&first);
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let mut workers = tokio::task::JoinSet::new();
        workers.spawn(async move {
            let _slot = worker_slot;
            started_tx.send(()).unwrap();
            std::future::pending::<()>().await;
        });
        started_rx.await.unwrap();
        drop(first);
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(10),
                reserve_background_plan_page()
            )
            .await
            .is_err()
        );
        drop(workers);
        let third = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            reserve_background_plan_page(),
        )
        .await
        .unwrap();
        drop(second);
        drop(third);
    }

    #[tokio::test]
    async fn background_budget_is_shared_and_oversized_batches_run_alone() {
        let permit = reserve_background_body_bytes(u64::MAX).await;
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(10),
                reserve_background_body_bytes(1)
            )
            .await
            .is_err()
        );
        drop(permit);
        let permit = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            reserve_background_body_bytes(16 * 1024 * 1024),
        )
        .await
        .unwrap();
        drop(permit);
    }

    fn item(id: i64, size: u32) -> PlannedBodyFetch {
        PlannedBodyFetch {
            message_id: id,
            uid: id as u32,
            plan: crate::mime::MimePlan {
                version: crate::mime::MIME_PLAN_VERSION,
                text_sections: vec![crate::mime::PlannedTextSection {
                    section: "1".into(),
                    kind: crate::mime::TextSectionKind::Plain,
                    mime_type: "text/plain".into(),
                    charset: Some("utf-8".into()),
                    transfer_encoding: "8bit".into(),
                    size,
                }],
                attachments: Vec::new(),
            },
        }
    }

    #[test]
    fn batch_packing_uses_two_fetches_for_three_hundred_small_messages() {
        let items = (1..=300).map(|id| item(id, 1_000)).collect();
        let batches = selective_fetch_batches(items, MAX_SELECTIVE_BATCH_BYTES);
        assert_eq!(batches.iter().map(Vec::len).collect::<Vec<_>>(), [200, 100]);
    }

    #[test]
    fn batch_packing_honors_bytes_and_keeps_oversized_singletons() {
        let batches = selective_fetch_batches(vec![item(1, 40), item(2, 40), item(3, 40)], 100);
        assert_eq!(batches.iter().map(Vec::len).collect::<Vec<_>>(), [2, 1]);

        let batches = selective_fetch_batches(vec![item(1, 120), item(2, 10)], 100);
        assert_eq!(batches.iter().map(Vec::len).collect::<Vec<_>>(), [1, 1]);
    }

    #[test]
    fn background_row_guard_rejects_stale_location_and_state() {
        let current = CurrentBodyRow {
            folder_id: Some(7),
            uid: Some(42),
            body_state: "none".into(),
        };
        assert!(is_current_background_row(&current, 7, 42));

        for stale in [
            CurrentBodyRow {
                folder_id: Some(8),
                ..current_body_row_copy(&current)
            },
            CurrentBodyRow {
                uid: Some(43),
                ..current_body_row_copy(&current)
            },
            CurrentBodyRow {
                body_state: "fetching".into(),
                ..current_body_row_copy(&current)
            },
            CurrentBodyRow {
                body_state: "cached".into(),
                ..current_body_row_copy(&current)
            },
        ] {
            assert!(!is_current_background_row(&stale, 7, 42));
        }
    }

    fn current_body_row_copy(row: &CurrentBodyRow) -> CurrentBodyRow {
        CurrentBodyRow {
            folder_id: row.folder_id,
            uid: row.uid,
            body_state: row.body_state.clone(),
        }
    }

    #[test]
    fn only_protocol_transport_errors_request_reconnect() {
        assert!(matches!(
            classify_body_chunk_error(CoreError::Imap("protocol".into())),
            BodyChunkError::Reconnect(_)
        ));
        assert!(matches!(
            classify_body_chunk_error(CoreError::Network("transport".into())),
            BodyChunkError::Reconnect(_)
        ));
        assert!(matches!(
            classify_body_chunk_error(CoreError::Offline),
            BodyChunkError::Reconnect(_)
        ));
        assert!(matches!(
            classify_body_chunk_error(CoreError::Other("join failure".into())),
            BodyChunkError::Fatal(_)
        ));
        assert!(matches!(
            classify_body_chunk_error(CoreError::Mime("decode failure".into())),
            BodyChunkError::Fatal(_)
        ));
    }
}

/// Background body backfill. Waits for a nudge from the sync actor (headers
/// just landed) or a periodic backstop tick, then drains every missing body
/// for the account with up to BODY_POOL_CONNS concurrent IMAP connections.
/// Runs beside the main actor, so folder/header sync, IDLE, and offline
/// actions never wait behind bulk body downloads. Exits when the account
/// handle is dropped (the nudge channel closes).
async fn run_backfill_pool(
    ctx: SyncCtx,
    config: AccountConfig,
    mut rx: mpsc::Receiver<()>,
    mut settings_rx: watch::Receiver<crate::models::AccountSettings>,
) {
    let account_id = config.id;
    loop {
        let closed = tokio::select! {
            result = tokio::time::timeout(
                std::time::Duration::from_secs(CYCLE_SECS),
                rx.recv(),
            ) => matches!(result, Ok(None)),
            result = settings_rx.changed() => result.is_err(),
        };
        if closed {
            return; // channel closed: account removed / shutdown
        }
        // Coalesce nudges that piled up while the previous drain ran.
        while rx.try_recv().is_ok() {}
        if let Err(e) = drain_missing_bodies(&ctx, &config, &mut settings_rx).await {
            tracing::warn!(account_id, error = %e, "body backfill: drain failed");
        }
    }
}

/// Fetch ALL missing bodies for the account, inbox first, by handing
/// folder-grouped chunks to a shared queue serviced by up to BODY_POOL_CONNS
/// parallel connections. Loops until a scan finds nothing left; bails out when
/// a full round makes no forward progress (server unreachable, or every
/// remaining UID vanished) so it can't spin - the next nudge or backstop tick
/// retries.
async fn drain_missing_bodies(
    ctx: &SyncCtx,
    config: &AccountConfig,
    settings_rx: &mut watch::Receiver<crate::models::AccountSettings>,
) -> Result<()> {
    use std::collections::{HashMap, HashSet, VecDeque};
    use std::sync::atomic::{AtomicU64, Ordering};

    let account_id = config.id;
    let mut did_work = false;
    let mut cursors: HashMap<i64, (i64, i64)> = HashMap::new();
    let mut exhausted = HashSet::new();
    let mut pass_persisted = 0u64;
    let mut pass_had_chunks = false;

    loop {
        // Hold one shared page slot through worker completion. This bounds
        // queued IDs across accounts and always takes the planning permit
        // before any worker can take the body-byte permit.
        let plan_page = Arc::new(reserve_background_plan_page().await);
        let mail_history = settings_rx.borrow_and_update().mail_history;
        let cutoff_ms = mail_history.cutoff_ms_at(now_ms());
        let folders = ctx
            .db
            .read(move |conn| repo::folders::list(conn, Some(account_id)))
            .await?;
        let mut ordered: Vec<&Folder> = folders.iter().filter(|folder| folder.selectable).collect();
        ordered.sort_by_key(|f| match f.role.as_deref() {
            Some(roles::INBOX) => 0,
            Some(roles::SENT) => 1,
            _ => 2,
        });

        let mut chunks: VecDeque<BodyChunk> = VecDeque::new();
        for folder in ordered {
            if folder.role.as_deref() == Some(roles::ALL) {
                continue;
            }
            if exhausted.contains(&folder.id) {
                continue;
            }
            let fid = folder.id;
            let cursor = cursors.get(&fid).copied();
            let missing = ctx
                .db
                .read(move |conn| {
                    repo::messages::missing_bodies_page_since(
                        conn,
                        fid,
                        BODY_PLAN_PAGE,
                        cutoff_ms,
                        cursor,
                    )
                })
                .await?;
            if let Some(&(id, _, date)) = missing.last() {
                cursors.insert(fid, (date, id));
            }
            if missing.len() < BODY_PLAN_PAGE as usize {
                exhausted.insert(fid);
            }
            let missing: Vec<(i64, i64)> = missing
                .into_iter()
                .map(|(mid, uid, _)| (mid, uid))
                .collect();
            for chunk in missing.chunks(BODY_FETCH_CHUNK) {
                chunks.push_back(BodyChunk {
                    folder_id: folder.id,
                    folder_name: folder.path.clone(),
                    uid_validity: folder.uidvalidity,
                    items: chunk.to_vec(),
                });
            }
            if !chunks.is_empty() {
                // One page at a time across the account, including folders
                // that would otherwise accumulate behind an active inbox.
                break;
            }
        }

        if chunks.is_empty() {
            if pass_persisted > 0 {
                cursors.clear();
                exhausted.clear();
                pass_persisted = 0;
                pass_had_chunks = false;
                continue;
            }
            if pass_had_chunks {
                return Err(CoreError::Imap(
                    "body backfill made no progress; will retry".into(),
                ));
            }
            if did_work {
                tracing::info!(account_id, "body backfill: drained");
            }
            return Ok(());
        }
        did_work = true;
        pass_had_chunks = true;
        tracing::debug!(
            account_id,
            chunks = chunks.len(),
            "body backfill: starting round"
        );

        // Fan the queue out to a pool of connections. Each worker owns its own
        // session and pops chunks until the queue is empty or its connection
        // breaks, so a slow chunk never stalls the others. Office 365 throttles
        // concurrent IMAP FETCH streams hard (it resets extra connections), so
        // it gets a smaller pool - more workers there just die and burn
        // reconnect round-trips.
        let cap = match config.provider {
            Provider::Microsoft => 1,
            _ => BODY_POOL_CONNS,
        };
        let workers = cap.min(chunks.len());
        let queue = Arc::new(tokio::sync::Mutex::new(chunks));
        let persisted = Arc::new(AtomicU64::new(0));
        // Dropping this set on account removal aborts its workers instead of
        // detaching them from the round's planning guard.
        let mut handles = tokio::task::JoinSet::new();
        for _ in 0..workers {
            let plan_page = Arc::clone(&plan_page);
            let worker_ctx = ctx.clone();
            let worker_config = config.clone();
            let worker_queue = queue.clone();
            let worker_persisted = persisted.clone();
            let worker_settings = settings_rx.clone();
            handles.spawn(async move {
                let _plan_page = plan_page;
                body_worker(
                    worker_ctx,
                    worker_config,
                    worker_queue,
                    worker_persisted,
                    worker_settings,
                )
                .await
            });
        }
        let mut worker_error = None;
        while let Some(result) = handles.join_next().await {
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    if worker_error.is_none() {
                        worker_error = Some(error);
                    }
                }
                Err(error) => {
                    if worker_error.is_none() {
                        worker_error = Some(CoreError::Other(format!(
                            "body worker task failed: {error}"
                        )));
                    }
                }
            }
        }
        if let Some(error) = worker_error {
            return Err(error);
        }
        let round_persisted = persisted.load(Ordering::Relaxed);
        pass_persisted += round_persisted;
        // Continue from each folder's cursor, then re-scan after a productive
        // full pass so headers that landed meanwhile are picked up too.
    }
}

/// One backfill-pool connection: pops chunks off the shared queue until it is
/// empty or the connection breaks. On error the chunk goes back on the queue
/// for the surviving workers (or the next round).
async fn body_worker(
    ctx: SyncCtx,
    config: AccountConfig,
    queue: Arc<tokio::sync::Mutex<std::collections::VecDeque<BodyChunk>>>,
    persisted: Arc<std::sync::atomic::AtomicU64>,
    settings_rx: watch::Receiver<crate::models::AccountSettings>,
) -> Result<()> {
    let account_id = config.id;
    let mut session = match connect(&ctx, &config).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(account_id, error = %e, "body worker: connect failed");
            return Err(e);
        }
    };
    let mut selected: Option<(String, Option<i64>)> = None;
    loop {
        if settings_rx.has_changed().unwrap_or(false) {
            break;
        }
        let chunk = queue.lock().await.pop_front();
        let Some(chunk) = chunk else { break };
        let chunk_started = std::time::Instant::now();
        match fetch_body_chunk(&ctx, &config, &mut session, &mut selected, &chunk).await {
            Ok(n) => {
                persisted.fetch_add(n, std::sync::atomic::Ordering::Relaxed);
                if n > 0 {
                    let elapsed = chunk_started.elapsed();
                    let rate = n as f64 / elapsed.as_secs_f64().max(0.001);
                    tracing::info!(
                        account_id,
                        cached = n,
                        elapsed_ms = elapsed.as_millis() as u64,
                        messages_per_second = rate,
                        "body backfill: selective batch cached"
                    );
                }
                // Live "Sync x/total": one COUNT per chunk, not per message.
                let progress_cutoff = settings_rx.borrow().mail_history.cutoff_ms_at(now_ms());
                let (done, total) = ctx
                    .db
                    .read(move |conn| {
                        repo::messages::body_progress_since(conn, account_id, progress_cutoff)
                    })
                    .await
                    .unwrap_or((0, 0));
                ctx.bus.emit(CoreEvent::SyncProgress(SyncProgress {
                    account_id,
                    folder: chunk.folder_name.clone(),
                    phase: "bodies".into(),
                    done,
                    total,
                }));
                emit_sync_status(&ctx, account_id).await;
            }
            Err(BodyChunkError::Reconnect(error)) => {
                tracing::warn!(account_id, error = %error, "body worker: IMAP chunk failed");
                let mut retry = chunk;
                if retry.items.len() > 1 {
                    // A timeout or server-side response-size limit must not let
                    // one large batch block every sibling forever. Bisect,
                    // reconnect (the timed-out protocol stream is not safely
                    // reusable), and retry the two halves on a fresh session.
                    let right_items = retry.items.split_off(retry.items.len() / 2);
                    let right = BodyChunk {
                        folder_id: retry.folder_id,
                        folder_name: retry.folder_name.clone(),
                        uid_validity: retry.uid_validity,
                        items: right_items,
                    };
                    {
                        let mut pending = queue.lock().await;
                        pending.push_front(right);
                        pending.push_front(retry);
                    }
                    drop(session);
                    match connect(&ctx, &config).await {
                        Ok(fresh) => {
                            session = fresh;
                            selected = None;
                            continue;
                        }
                        Err(connect_error) => tracing::warn!(
                            account_id,
                            error = %connect_error,
                            "body worker: reconnect after batch split failed"
                        ),
                    }
                } else if matches!(error, CoreError::ImapParse(_)) {
                    let failures = retry
                        .items
                        .iter()
                        .map(|(message_id, _)| (*message_id, error.to_string()))
                        .collect();
                    record_content_failure_details(&ctx, failures).await?;
                    drop(session);
                    if queue.lock().await.is_empty() {
                        return Ok(());
                    }
                    session = connect(&ctx, &config).await?;
                    selected = None;
                    continue;
                } else {
                    queue.lock().await.push_front(retry);
                }
                // A singleton failure waits for the next periodic retry rather
                // than spinning. Dropping the session closes it.
                return Err(error);
            }
            Err(BodyChunkError::Fatal(error)) => {
                tracing::warn!(
                    account_id,
                    error = %error,
                    "body worker: local batch failure; aborting drain without reconnect"
                );
                return Err(error);
            }
        }
    }
    imap::logout(session).await;
    Ok(())
}

/// Fetch one chunk's bodies over `session` and persist them. Returns how many
/// bodies were cached. The keyset cursor advances past UIDs the server omits;
/// durable content failures postpone retry until the next backstop cycle.
async fn fetch_body_chunk(
    ctx: &SyncCtx,
    config: &AccountConfig,
    session: &mut Session,
    selected: &mut Option<(String, Option<i64>)>,
    chunk: &BodyChunk,
) -> std::result::Result<u64, BodyChunkError> {
    // The main actor can rename/reset a folder while this background queue is
    // waiting. Do not use a stale folder snapshot, especially across a
    // UIDVALIDITY reset where the same numeric UID can name different mail.
    let folder_is_current = ctx
        .db
        .read({
            let folder_id = chunk.folder_id;
            let folder_name = chunk.folder_name.clone();
            let uid_validity = chunk.uid_validity;
            move |conn| {
                Ok(repo::folders::get(conn, folder_id)?.is_some_and(|folder| {
                    folder.path == folder_name && folder.uidvalidity == uid_validity
                }))
            }
        })
        .await
        .map_err(BodyChunkError::Fatal)?;
    if !folder_is_current {
        return Ok(0);
    }

    let selected_matches = selected.as_ref().is_some_and(|(name, uid_validity)| {
        name == &chunk.folder_name && *uid_validity == chunk.uid_validity
    });
    if !selected_matches {
        let mailbox = imap::select(session, &chunk.folder_name)
            .await
            .map_err(BodyChunkError::Reconnect)?;
        if mailbox.uid_validity != chunk.uid_validity {
            tracing::warn!(
                folder_id = chunk.folder_id,
                folder = %chunk.folder_name,
                queued_uidvalidity = ?chunk.uid_validity,
                selected_uidvalidity = ?mailbox.uid_validity,
                "body backfill: stale folder UIDVALIDITY; skipping queued rows"
            );
            return Ok(0);
        }
        *selected = Some((chunk.folder_name.clone(), mailbox.uid_validity));
    }

    let (planned, mut failures) = prepare_body_fetches(ctx, session, chunk.folder_id, &chunk.items)
        .await
        .map_err(classify_body_chunk_error)?;
    let mut groups: std::collections::BTreeMap<Vec<String>, Vec<PlannedBodyFetch>> =
        std::collections::BTreeMap::new();
    for item in planned {
        if item.plan.text_sections.is_empty() {
            failures.push((
                item.message_id,
                "BODYSTRUCTURE contains no readable text sections".into(),
            ));
            continue;
        }
        groups
            .entry(item.plan.text_section_ids())
            .or_default()
            .push(item);
    }

    record_content_failure_details(ctx, failures)
        .await
        .map_err(BodyChunkError::Fatal)?;

    let mut cached = 0u64;
    for (sections, items) in groups {
        let max_bytes = match config.provider {
            Provider::Microsoft => MAX_MICROSOFT_BATCH_BYTES,
            _ => MAX_SELECTIVE_BATCH_BYTES,
        };
        for batch in selective_fetch_batches(items, max_bytes) {
            // A priority reader, action, expunge, or UID reset may have changed
            // rows while plans were being discovered. Check again immediately
            // before constructing the UID set.
            let batch = revalidate_background_fetches(ctx, chunk.folder_id, batch)
                .await
                .map_err(BodyChunkError::Fatal)?;
            if batch.is_empty() {
                continue;
            }
            let memory_permit = reserve_background_body_bytes(
                batch
                    .iter()
                    .map(planned_text_bytes)
                    .fold(0u64, u64::saturating_add),
            )
            .await;
            let uids: Vec<u32> = batch.iter().map(|item| item.uid).collect();
            let fetched = imap::fetch_content_sections_batch(session, &uids, &sections)
                .await
                .map_err(BodyChunkError::Reconnect)?;
            let mut by_uid: std::collections::HashMap<_, _> = fetched
                .into_iter()
                .map(|message| (message.uid, message.sections))
                .collect();
            let (decoded, batch_failures, memory_permit) = tokio::task::spawn_blocking(move || {
                let mut decoded = Vec::with_capacity(batch.len());
                let mut failures = Vec::new();
                for item in batch {
                    let message_id = item.message_id;
                    match by_uid
                        .remove(&item.uid)
                        .ok_or_else(|| {
                            CoreError::Imap(format!(
                                "server omitted selective content for UID {}",
                                item.uid
                            ))
                        })
                        .and_then(|sections| decode_selective_content(item, sections))
                    {
                        Ok(content) => decoded.push(content),
                        Err(error) => failures.push((message_id, error.to_string())),
                    }
                }
                (decoded, failures, memory_permit)
            })
            .await
            .map_err(|error| {
                BodyChunkError::Fatal(CoreError::Other(format!(
                    "content decode task failed: {error}"
                )))
            })?;
            record_content_failure_details(ctx, batch_failures)
                .await
                .map_err(BodyChunkError::Fatal)?;
            cached += persist_selective_batch(
                ctx,
                config.id,
                SelectivePersistGuard::Background {
                    folder_id: chunk.folder_id,
                },
                decoded,
                Some(memory_permit),
            )
            .await
            .map_err(BodyChunkError::Fatal)?;
        }
    }
    Ok(cached)
}

/// Load persisted MIME plans in one DB call and discover legacy/null plans in
/// one lightweight BODYSTRUCTURE FETCH. Existing installations therefore avoid
/// two round trips per uncached message after migrating to selective caching.
async fn prepare_body_fetches(
    ctx: &SyncCtx,
    session: &mut Session,
    folder_id: i64,
    items: &[(i64, i64)],
) -> Result<(Vec<PlannedBodyFetch>, Vec<(i64, String)>)> {
    let rows = items.to_vec();
    let mut loaded = ctx
        .db
        .read(move |conn| {
            let mut result = Vec::with_capacity(rows.len());
            for (message_id, uid) in rows {
                let Some(current) = current_body_row(conn, message_id)? else {
                    continue;
                };
                // A priority reader, move, expunge, or UID reset may have
                // changed this message after the background queue was built.
                if !is_current_background_row(&current, folder_id, uid) {
                    continue;
                }
                result.push((
                    message_id,
                    uid,
                    repo::messages::mime_plan(conn, message_id)?,
                ));
            }
            Ok(result)
        })
        .await?;

    for (_, _, plan) in &mut loaded {
        if plan
            .as_ref()
            .is_some_and(|plan| plan.version != crate::mime::MIME_PLAN_VERSION)
        {
            *plan = None;
        }
    }

    let missing_uids: Vec<u32> = loaded
        .iter()
        .filter_map(|(_, uid, plan)| plan.is_none().then(|| u32::try_from(*uid).ok()).flatten())
        .collect();
    if !missing_uids.is_empty() {
        let discovered: std::collections::HashMap<u32, crate::mime::MimePlan> =
            imap::fetch_mime_plans_batch(session, &missing_uids)
                .await?
                .into_iter()
                .filter_map(|fetched| {
                    (fetched.plan.version == crate::mime::MIME_PLAN_VERSION)
                        .then_some((fetched.uid, fetched.plan))
                })
                .collect();

        let mut to_persist = Vec::new();
        for (message_id, uid, plan) in &mut loaded {
            if plan.is_none()
                && let Ok(uid) = u32::try_from(*uid)
                && let Some(value) = discovered.get(&uid)
            {
                *plan = Some(value.clone());
                to_persist.push((*message_id, i64::from(uid), value.clone()));
            }
        }
        if !to_persist.is_empty() {
            ctx.db
                .write(move |conn| {
                    let tx = conn.transaction()?;
                    for (message_id, uid, plan) in to_persist {
                        let Some(current) = current_body_row(&tx, message_id)? else {
                            continue;
                        };
                        if !is_current_background_row(&current, folder_id, uid) {
                            continue;
                        }
                        repo::messages::set_mime_plan(&tx, message_id, Some(&plan))?;
                        repo::messages::upsert_planned_attachments(
                            &tx,
                            message_id,
                            &plan.attachments,
                        )?;
                    }
                    tx.commit()?;
                    Ok(())
                })
                .await?;
        }
    }

    // Plan discovery is a network round trip. Re-check every row afterward so
    // moved, expunged, reset, or priority-claimed messages are silent skips.
    loaded = ctx
        .db
        .read(move |conn| {
            let mut current_rows = Vec::with_capacity(loaded.len());
            for (message_id, uid, plan) in loaded {
                let Some(current) = current_body_row(conn, message_id)? else {
                    continue;
                };
                if is_current_background_row(&current, folder_id, uid) {
                    current_rows.push((message_id, uid, plan));
                }
            }
            Ok(current_rows)
        })
        .await?;

    let mut planned = Vec::with_capacity(loaded.len());
    let mut failures = Vec::new();
    for (message_id, uid, plan) in loaded {
        let Ok(uid) = u32::try_from(uid) else {
            failures.push((message_id, format!("invalid remote UID {uid}")));
            continue;
        };
        match plan {
            Some(plan) => planned.push(PlannedBodyFetch {
                message_id,
                uid,
                plan,
            }),
            None => failures.push((
                message_id,
                format!("server did not provide BODYSTRUCTURE for UID {uid}"),
            )),
        }
    }
    Ok((planned, failures))
}

/// Drop rows whose remote location or ownership changed while MIME plans were
/// being loaded. Missing rows are normal during concurrent expunge cleanup.
async fn revalidate_background_fetches(
    ctx: &SyncCtx,
    folder_id: i64,
    items: Vec<PlannedBodyFetch>,
) -> Result<Vec<PlannedBodyFetch>> {
    ctx.db
        .read(move |conn| {
            let mut current_items = Vec::with_capacity(items.len());
            for item in items {
                let Some(current) = current_body_row(conn, item.message_id)? else {
                    continue;
                };
                if is_current_background_row(&current, folder_id, i64::from(item.uid)) {
                    current_items.push(item);
                }
            }
            Ok(current_items)
        })
        .await
}

/// Priority fetch of a single message body (user opened it).
async fn fetch_one_body(
    ctx: &SyncCtx,
    config: &AccountConfig,
    session: &mut Option<Session>,
    message_id: i64,
) -> Result<()> {
    let row = ctx
        .db
        .read(move |conn| repo::messages::get_row(conn, message_id))
        .await?
        .ok_or_else(|| CoreError::NotFound("message".into()))?;
    if row.body_state == "cached" {
        return Ok(());
    }
    let (Some(folder_id), Some(uid)) = (row.folder_id, row.uid) else {
        return Ok(());
    };
    let folder = ctx
        .db
        .read(move |conn| repo::folders::get(conn, folder_id))
        .await?
        .ok_or_else(|| CoreError::NotFound("folder".into()))?;
    select_folder_for_remote_read(ctx, session.as_mut().ok_or(CoreError::Offline)?, &folder)
        .await?;
    if !remote_location_is_current(ctx, message_id, folder_id, uid).await? {
        reset_bodies_none(ctx, std::iter::once(message_id)).await;
        return Ok(());
    }
    store_one_body(ctx, config, session, message_id, folder_id, uid as u32).await
}

async fn store_one_body(
    ctx: &SyncCtx,
    config: &AccountConfig,
    session: &mut Option<Session>,
    message_id: i64,
    folder_id: i64,
    uid: u32,
) -> Result<()> {
    fetch_selective_content(ctx, config, session, message_id, folder_id, uid, true).await
}

async fn record_content_failure(ctx: &SyncCtx, message_id: i64, error: &CoreError) {
    let _ = record_content_failure_details(ctx, vec![(message_id, error.to_string())]).await;
}

async fn record_content_failure_details(ctx: &SyncCtx, failures: Vec<(i64, String)>) -> Result<()> {
    if failures.is_empty() {
        return Ok(());
    }
    ctx.db
        .write(move |conn| record_content_failure_details_at(conn, failures, now_ms()))
        .await
}

fn record_content_failure_details_at(
    conn: &mut rusqlite::Connection,
    failures: Vec<(i64, String)>,
    now: i64,
) -> Result<()> {
    let tx = conn.transaction()?;
    for (message_id, detail) in failures {
        let Some(row) = current_body_row(&tx, message_id)? else {
            continue;
        };
        // A concurrent fetch may have succeeded while this failure was in flight.
        if row.body_state == "cached" {
            continue;
        }
        // The count excludes this failure: failures 1–5 wait one minute,
        // 6–20 wait fifteen minutes, and 21 onward wait a day. These deadlines
        // gate background work only; opening a message can still retry now.
        let attempts = repo::sync_failures::content_attempts(&tx, message_id)?;
        let delay = match attempts {
            0..=4 => 60_000,
            5..=19 => 15 * 60_000,
            _ => 24 * 60 * 60_000,
        };
        repo::sync_failures::record_content_at(&tx, message_id, Some(now + delay), &detail, now)?;
    }
    tx.commit()?;
    Ok(())
}

fn decode_selective_content(
    item: PlannedBodyFetch,
    fetched: Vec<imap::FetchedSection>,
) -> Result<DecodedSelectiveContent> {
    if item.plan.text_sections.is_empty() {
        return Err(CoreError::Mime(
            "BODYSTRUCTURE contains no readable text sections".into(),
        ));
    }
    let fetched_by_section: std::collections::HashMap<_, _> = fetched
        .into_iter()
        .map(|section| (section.section.clone(), section))
        .collect();
    let mut plain = Vec::new();
    let mut html = Vec::new();
    let mut calendar_parts = Vec::new();
    for planned in &item.plan.text_sections {
        let section = fetched_by_section.get(&planned.section).ok_or_else(|| {
            CoreError::Mime(format!(
                "server omitted planned MIME section {}",
                planned.section
            ))
        })?;
        let decoded =
            crate::mime::decode_planned_text_section(planned, &section.mime_header, &section.body)?;
        match decoded.kind {
            crate::mime::TextSectionKind::Plain => plain.push(decoded.content),
            crate::mime::TextSectionKind::Html => html.push(decoded.content),
            crate::mime::TextSectionKind::Calendar => calendar_parts.push(decoded.content),
        }
    }

    let text_body = (!plain.is_empty()).then(|| plain.join("\n\n"));
    let html_body = (!html.is_empty()).then(|| html.join("\n"));
    let snippet = crate::mime::make_body_snippet(text_body.as_deref(), html_body.as_deref());
    Ok(DecodedSelectiveContent {
        message_id: item.message_id,
        uid: item.uid,
        plan: item.plan,
        text_body,
        html_body,
        calendar_parts,
        snippet,
    })
}

/// Fetch only the MIME sections required to make a message readable and
/// searchable. Full raw MIME is a user-open fallback only; the background path
/// never downloads attachment bytes implicitly.
async fn fetch_selective_content(
    ctx: &SyncCtx,
    config: &AccountConfig,
    session: &mut Option<Session>,
    message_id: i64,
    folder_id: i64,
    uid: u32,
    allow_open_fallback: bool,
) -> Result<()> {
    let mut plan = ctx
        .db
        .read(move |conn| repo::messages::mime_plan(conn, message_id))
        .await?;

    // A newer schema may assign different semantics to these fields. Rebuild
    // an unsupported plan rather than interpreting it as version 1.
    if plan
        .as_ref()
        .is_some_and(|plan| plan.version != crate::mime::MIME_PLAN_VERSION)
    {
        plan = None;
    }

    if plan.is_none() {
        if !remote_location_is_current(ctx, message_id, folder_id, i64::from(uid)).await? {
            return Err(CoreError::NotFound(
                "message remote location changed before MIME-plan fetch".into(),
            ));
        }
        let plans = match imap::fetch_mime_plans_batch(
            session.as_mut().ok_or(CoreError::Offline)?,
            &[uid],
        )
        .await
        {
            Ok(plans) => plans,
            Err(error @ CoreError::ImapParse(_)) if allow_open_fallback => {
                tracing::warn!(message_id, error = %error, "MIME plan unavailable; retrying full message on a fresh session");
                reconnect_body_session(ctx, config, session, folder_id).await?;
                return fetch_full_open_fallback(
                    ctx,
                    config,
                    session.as_mut().ok_or(CoreError::Offline)?,
                    message_id,
                    folder_id,
                    uid,
                )
                .await;
            }
            Err(error) => return Err(error),
        };
        plan = plans
            .into_iter()
            .find(|fetched| fetched.uid == uid)
            .map(|fetched| fetched.plan);
        if let Some(discovered) = plan.as_ref() {
            let discovered = discovered.clone();
            ctx.db
                .write(move |conn| {
                    let Some(current) = current_body_row(conn, message_id)? else {
                        return Ok(());
                    };
                    if current.folder_id != Some(folder_id) || current.uid != Some(i64::from(uid)) {
                        return Ok(());
                    }
                    repo::messages::set_mime_plan(conn, message_id, Some(&discovered))?;
                    repo::messages::upsert_planned_attachments(
                        conn,
                        message_id,
                        &discovered.attachments,
                    )?;
                    Ok(())
                })
                .await?;
        }
    }

    let Some(plan) = plan else {
        if allow_open_fallback {
            return fetch_full_open_fallback(
                ctx,
                config,
                session.as_mut().ok_or(CoreError::Offline)?,
                message_id,
                folder_id,
                uid,
            )
            .await;
        }
        return Err(CoreError::Mime(
            "server did not provide a usable BODYSTRUCTURE".into(),
        ));
    };

    // Keep persistence errors distinct: only selective protocol/decode
    // failures are eligible for the user-open full-MIME fallback.
    let selective = async {
        if !remote_location_is_current(ctx, message_id, folder_id, i64::from(uid)).await? {
            return Err(CoreError::NotFound(
                "message remote location changed before content fetch".into(),
            ));
        }
        let section_ids = plan.text_section_ids();
        let fetched = imap::fetch_content_sections(
            session.as_mut().ok_or(CoreError::Offline)?,
            uid,
            &section_ids,
        )
        .await?;
        decode_selective_content(
            PlannedBodyFetch {
                message_id,
                uid,
                plan: plan.clone(),
            },
            fetched,
        )
    }
    .await;
    let content = match selective {
        Ok(content) => content,
        Err(error @ (CoreError::ImapParse(_) | CoreError::Mime(_))) if allow_open_fallback => {
            tracing::warn!(
                message_id,
                error = %error,
                "selective content unavailable; using explicit-open full MIME fallback"
            );
            if matches!(error, CoreError::ImapParse(_)) {
                reconnect_body_session(ctx, config, session, folder_id).await?;
            }
            return fetch_full_open_fallback(
                ctx,
                config,
                session.as_mut().ok_or(CoreError::Offline)?,
                message_id,
                folder_id,
                uid,
            )
            .await;
        }
        Err(error) => return Err(error),
    };
    persist_selective_batch(
        ctx,
        config.id,
        SelectivePersistGuard::Priority { folder_id },
        vec![content],
        None,
    )
    .await
    .map(|_| ())
}

/// A parser failure can leave unread response bytes. Close the old socket
/// without LOGOUT, and verify the UID namespace before using its replacement.
async fn reconnect_body_session(
    ctx: &SyncCtx,
    config: &AccountConfig,
    session: &mut Option<Session>,
    folder_id: i64,
) -> Result<()> {
    drop(session.take());
    let folder = ctx
        .db
        .read(move |conn| repo::folders::get(conn, folder_id))
        .await?
        .ok_or_else(|| CoreError::NotFound("folder".into()))?;
    let mut fresh = connect(ctx, config).await?;
    select_folder_for_remote_read(ctx, &mut fresh, &folder).await?;
    *session = Some(fresh);
    Ok(())
}

async fn fetch_full_open_fallback(
    ctx: &SyncCtx,
    config: &AccountConfig,
    session: &mut Session,
    message_id: i64,
    folder_id: i64,
    uid: u32,
) -> Result<()> {
    if !remote_location_is_current(ctx, message_id, folder_id, i64::from(uid)).await? {
        return Err(CoreError::NotFound(
            "message remote location changed before full content fetch".into(),
        ));
    }
    let Some(raw) = imap::fetch_full(session, uid).await? else {
        return Err(CoreError::NotFound(format!("remote message UID {uid}")));
    };
    persist_body(ctx, config, message_id, folder_id, uid, &raw).await
}

async fn persist_selective_batch(
    ctx: &SyncCtx,
    account_id: i64,
    guard: SelectivePersistGuard,
    contents: Vec<DecodedSelectiveContent>,
    memory_permit: Option<tokio::sync::OwnedSemaphorePermit>,
) -> Result<u64> {
    if contents.is_empty() {
        return Ok(0);
    }
    let calendar_events: Vec<(i64, crate::calendar::IcsEvent)> = contents
        .iter()
        .flat_map(|content| {
            content.calendar_parts.iter().flat_map(move |ics| {
                crate::calendar::parse_ics(ics)
                    .into_iter()
                    .map(move |event| (content.message_id, event))
            })
        })
        .collect();
    let (thread_ids, count) = ctx
        .db
        .write(move |conn| {
            let _memory_permit = memory_permit;
            let tx = conn.transaction()?;
            let mut thread_ids = Vec::new();
            let mut count = 0u64;
            for content in contents {
                let message_id = content.message_id;
                let Some(current) = current_body_row(&tx, message_id)? else {
                    continue;
                };
                if !can_persist_selective_content(&current, guard, content.uid) {
                    continue;
                }
                repo::messages::set_mime_plan(&tx, message_id, Some(&content.plan))?;
                repo::messages::upsert_planned_attachments(
                    &tx,
                    message_id,
                    &content.plan.attachments,
                )?;
                repo::messages::store_body(
                    &tx,
                    message_id,
                    content.text_body.as_deref(),
                    content.html_body.as_deref(),
                    None,
                    content.plan.has_file_attachments(),
                    Some(&content.snippet),
                )?;
                repo::search::index_message(&tx, message_id)?;
                let _ = repo::sync_failures::clear_content(&tx, message_id);
                if let Some(thread_id) =
                    repo::messages::get_row(&tx, message_id)?.and_then(|row| row.thread_id)
                    && !thread_ids.contains(&thread_id)
                {
                    thread_ids.push(thread_id);
                }
                count += 1;
            }
            for &thread_id in &thread_ids {
                repo::threads::recompute(&tx, thread_id)?;
            }
            tx.commit()?;
            Ok((thread_ids, count))
        })
        .await?;
    if !calendar_events.is_empty() {
        ctx.calendar_db
            .write(move |conn| {
                let tx = conn.transaction()?;
                for (message_id, event) in &calendar_events {
                    repo::calendar::upsert(&tx, account_id, *message_id, event)?;
                }
                tx.commit()?;
                Ok(())
            })
            .await?;
        ctx.bus.emit(CoreEvent::CalendarUpdated { account_id });
    }
    if !thread_ids.is_empty() {
        ctx.bus.emit(CoreEvent::MailUpdated { thread_ids });
    }
    Ok(count)
}

/// Parse one already-fetched raw message, persist it to disk + DB, and notify
/// the UI. Shared by the single-body path and the bulk backfill.
async fn persist_body(
    ctx: &SyncCtx,
    config: &AccountConfig,
    message_id: i64,
    folder_id: i64,
    uid: u32,
    raw: &[u8],
) -> Result<()> {
    if !remote_location_is_current(ctx, message_id, folder_id, i64::from(uid)).await? {
        return Ok(());
    }
    let dir = ctx.paths.mail_dir(config.id);
    tokio::fs::create_dir_all(&dir).await?;
    // Include the guarded remote identity so a stale in-flight read can never
    // overwrite a newer location's raw cache file for the same local row.
    let path = dir.join(format!("{message_id}.{folder_id}.{uid}.eml"));
    crate::file_io::write_atomic(&path, raw, "IMAP message cache").await?;
    let path_str = path.to_string_lossy().to_string();

    let parsed = crate::mime::parse_message(raw)?;
    let calendar_events = super::ingest::calendar_events(&parsed);
    let (persisted, thread_id) = ctx
        .db
        .write(move |conn| {
            let tx = conn.transaction()?;
            let Some(current) = current_body_row(&tx, message_id)? else {
                return Ok((false, None));
            };
            if !can_persist_selective_content(
                &current,
                SelectivePersistGuard::Priority { folder_id },
                uid,
            ) {
                return Ok((false, None));
            }
            super::ingest::store_body(&tx, message_id, &path_str, &parsed)?;
            let _ = repo::sync_failures::clear_content(&tx, message_id);
            let tid: Option<i64> =
                repo::messages::get_row(&tx, message_id)?.and_then(|r| r.thread_id);
            if let Some(tid) = tid {
                repo::threads::recompute(&tx, tid)?;
            }
            tx.commit()?;
            Ok((true, tid))
        })
        .await?;

    if !persisted {
        let _ = tokio::fs::remove_file(&path).await;
        return Ok(());
    }
    super::ingest::store_calendar_events(ctx, config.id, message_id, calendar_events).await?;
    if let Some(tid) = thread_id {
        ctx.bus.emit(CoreEvent::MailUpdated {
            thread_ids: vec![tid],
        });
    }
    Ok(())
}

#[cfg(all(test, desktop))]
#[path = "mime_recovery_tests.rs"]
mod mime_recovery_tests;

#[cfg(all(test, desktop))]
#[path = "sync_cycle_tests.rs"]
mod sync_cycle_tests;

#[cfg(test)]
mod sent_identity_tests {
    use super::*;
    use crate::queue::sent_tests::local_outgoing;

    #[tokio::test]
    async fn sent_sync_uses_submission_identity_and_never_subject_time() {
        for marker in ["", "X-Flectar-Submission-ID: submission@test.dev\r\n"] {
            let (_temp, core, config, action) = local_outgoing().await;
            let folder = core
                .db
                .read(|c| repo::folders::get(c, 1))
                .await
                .unwrap()
                .unwrap();
            let header = FetchedHeader {
                uid: 99, flags: Default::default(), internal_date_ms: Some(1000), size: Some(100),
                header_bytes: format!("Message-ID: <rewritten@server>\r\n{marker}Subject: Status\r\nFrom: me@test.dev\r\nTo: bob@test.dev\r\nDate: Thu, 1 Jan 1970 00:00:01 +0000\r\n\r\n").into_bytes(),
                has_attachments: false, mime_plan: None,
            };
            store_headers(
                &core.sync_ctx(),
                &config,
                &folder,
                vec![header.clone()],
                None,
                NewHeaders::Insert,
            )
            .await
            .unwrap();
            store_headers(
                &core.sync_ctx(),
                &config,
                &folder,
                vec![header],
                None,
                NewHeaders::Insert,
            )
            .await
            .unwrap();
            let id = action.message_id.unwrap();
            let (count, uid) = core
                .db
                .read(move |c| {
                    Ok((
                        c.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get::<_, i64>(0))?,
                        repo::messages::get_row(c, id)?.unwrap().uid,
                    ))
                })
                .await
                .unwrap();
            assert_eq!(count, if marker.is_empty() { 2 } else { 1 });
            assert_eq!(uid, if marker.is_empty() { None } else { Some(99) });
        }
    }

    #[tokio::test]
    async fn sent_sync_can_arrive_before_smtp_completion() {
        let (_temp, core, config, action) = local_outgoing().await;
        let id = action.message_id.unwrap();
        core.db.write(move |c| {
            c.execute("INSERT INTO folders(id,account_id,path,role) VALUES (2,1,'Drafts','drafts')", [])?;
            c.execute("UPDATE messages SET is_outgoing=0, is_draft=1, folder_id=2, uid=12 WHERE id=?1", [id])?;
            c.execute("UPDATE pending_actions SET kind='send', state='inflight'", [])?;
            Ok(())
        }).await.unwrap();
        let folder = core
            .db
            .read(|c| repo::folders::get(c, 1))
            .await
            .unwrap()
            .unwrap();
        let header = FetchedHeader {
            uid: 99, flags: Default::default(), internal_date_ms: None, size: None,
            header_bytes: b"Message-ID: <rewritten@server>\r\nX-Flectar-Submission-ID: submission@test.dev\r\nSubject: Status\r\n\r\n".to_vec(),
            has_attachments: false, mime_plan: None,
        };
        store_headers(
            &core.sync_ctx(),
            &config,
            &folder,
            vec![header],
            None,
            NewHeaders::Insert,
        )
        .await
        .unwrap();
        let row = core
            .db
            .read(move |c| repo::messages::get_row(c, id))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.uid, Some(99));
        assert_eq!(row.folder_id, Some(1));
    }
}
