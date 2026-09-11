//! Per-account sync actors. The main actor's IMAP connection runs a cycle
//! loop of only the cheap, latency-sensitive work: drain commands -> execute
//! due pending actions -> per-folder new-mail checks and flag/expunge
//! reconciliation. Everything heavy runs on its own connections in parallel:
//! first-time header backfills and historical extension on the dedicated
//! history connection (`run_history_backfill`), and bulk body downloads on a
//! pool of up to BODY_POOL_CONNS connections (`run_backfill_pool`). Cycles
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
use crate::db::repo::{self, folders::Folder, messages::NewMessage};
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
    action_replay: bool,
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
    settings_tx: watch::Sender<crate::models::AccountSettings>,
    tasks: Vec<tokio::task::AbortHandle>,
}

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
        // Route priority body fetches to the dedicated reader connection;
        // everything else drives the bulk sync actor.
        match cmd {
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
        }
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
    // These carry idempotent wakeups rather than work. A single pending item
    // represents every newer nudge and prevents bursts from retaining memory.
    let (pool_tx, pool_rx) = mpsc::channel(1);
    let (hist_tx, hist_rx) = mpsc::channel(1);
    let (settings_tx, settings_rx) = watch::channel(config.settings.clone());
    let joins = if config.provider == Provider::Gmail {
        crate::sync::gmail::spawn(ctx, config, rx, body_rx, settings_rx)
    } else if config.mail_protocol == MailProtocol::Jmap {
        crate::jmap::sync::spawn(ctx, config, rx, body_rx, settings_rx)
    } else {
        vec![
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
        ]
    };
    AccountHandle {
        account_id,
        tx,
        body_tx,
        settings_tx,
        tasks: joins
            .iter()
            .map(tokio::task::JoinHandle::abort_handle)
            .collect(),
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
            let Some(s) = session.as_mut() else { break };
            match fetch_bodies_batch(&ctx, &config, s, &ids).await {
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
                    if let Some(s) = session.take() {
                        imap::logout(s).await;
                    }
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
    session: &mut Session,
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
        select_folder_for_remote_read(ctx, session, &folder).await?;
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
    let selected = imap::select(session, &folder.imap_name).await?;
    if selected.uid_validity != folder.uidvalidity {
        let folder_id = folder.id;
        let folder_name = folder.imap_name.clone();
        let expected_uidvalidity = folder.uidvalidity;
        let remote_uidvalidity = selected.uid_validity;
        let remote_uidnext = selected.uid_next;
        ctx.db
            .write(move |conn| {
                let tx = conn.transaction()?;
                let current = repo::folders::get(&tx, folder_id)?;
                if current.as_ref().is_some_and(|current| {
                    current.imap_name == folder_name && current.uidvalidity == expected_uidvalidity
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
    let folder_name = folder.imap_name.clone();
    let uidvalidity = folder.uidvalidity;
    let still_current = ctx
        .db
        .read(move |conn| {
            Ok(repo::folders::get(conn, folder_id)?.is_some_and(|current| {
                current.imap_name == folder_name && current.uidvalidity == uidvalidity
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
    tracing::debug!(account_id, "sync actor started");
    let mut session: Option<Session> = None;
    // Whether the current connection supports IMAP IDLE (push). Re-probed on
    // every reconnect; `None` until the first successful connect.
    let mut supports_idle: Option<bool> = None;
    let mut backoff_secs: u64 = 1;
    let mut cycle: u64 = 0;
    let mut replaying_actions = false;
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
                    tracing::warn!(
                        account_id,
                        imap_host = %config.imap_host,
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
                action_replay: replaying_actions,
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
            Ok(actions_remaining) => {
                session = Some(s);
                cycle += 1;
                replaying_actions = actions_remaining;
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
                set_state(&ctx, account_id, "idle").await;
                ctx.bus.emit(CoreEvent::SyncProgress(SyncProgress {
                    account_id,
                    folder: String::new(),
                    phase: "idle".into(),
                    done,
                    total,
                }));
                if actions_remaining {
                    // Replay another bounded slice immediately, with another
                    // INBOX check in front of it.
                    rerun_immediately = true;
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
                    match imap::select(&mut s, &inbox.imap_name).await {
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
                                            if let Some(ref mut s) = session
                                                && let Err(e) =
                                                    fetch_one_body(&ctx, &config, s, message_id)
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
                        if let Some(ref mut s) = session {
                            if let Err(e) = fetch_one_body(&ctx, &config, s, message_id).await {
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
    let _ = ctx
        .db
        .write(move |conn| {
            repo::accounts::set_sync_state_with_error(conn, account_id, &st, error.as_deref())
        })
        .await;
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
        let (header_done, header_total): (i64, i64) = if account.provider == Provider::Gmail {
            conn.query_row(
                "SELECT COALESCE(backfill_done, 0), 1
                 FROM gmail_sync_state WHERE account_id = ?1
                 UNION ALL SELECT 0, 1
                 WHERE NOT EXISTS (SELECT 1 FROM gmail_sync_state WHERE account_id = ?1)
                 LIMIT 1",
                rusqlite::params![account_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?
        } else {
            conn.query_row(
                "SELECT COALESCE(SUM(backfill_done), 0), COUNT(*)
                 FROM folders
                 WHERE account_id = ?1 AND COALESCE(role, '') <> 'all'",
                rusqlite::params![account_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?
        };
        let header_failed: i64 = conn.query_row(
            "SELECT COUNT(*)
             FROM sync_failures sf
             JOIN folders f ON f.id = sf.folder_id
             WHERE sf.account_id = ?1 AND sf.stage = 'header'
               AND (?2 = 1 OR COALESCE(f.role, '') <> 'all')",
            rusqlite::params![account_id, (account.provider == Provider::Gmail) as i64],
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
    let creds = imap_credentials(ctx, config).await?;
    match imap::connect_with_settings(
        &config.imap_host,
        config.imap_port,
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
                &config.imap_host,
                config.imap_port,
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
        action_replay,
    } = mode;
    let account_id = config.id;

    // Folder discovery: first cycle and then every ~30 cycles.
    if !action_replay && cycle.is_multiple_of(30) {
        discover_folders(ctx, config, session).await?;
    }

    let folders = {
        let acc = account_id;
        ctx.db
            .read(move |conn| repo::folders::list(conn, Some(acc)))
            .await?
    };

    // Sync order: inbox first, then sent/drafts, then the rest.
    let mut ordered: Vec<&Folder> = folders.iter().collect();
    ordered.sort_by_key(|f| match f.role.as_deref() {
        Some(roles::INBOX) => 0,
        Some(roles::SENT) => 1,
        Some(roles::DRAFTS) => 2,
        Some(roles::ARCHIVE) => 3,
        _ => 4,
    });

    // The latency-sensitive INBOX pass always runs before any queued mutation.
    // A mass archive/trash replay therefore cannot hide newly arrived mail.
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
            !action_replay && cycle > 0 && cycle.is_multiple_of(5),
            hist_tx,
            inbox_baseline,
        )
        .await?;
    }

    // Foreground readiness ends at the Inbox boundary. Queued mutations,
    // non-Inbox folders, history, and content caching are background work and
    // must never keep the top-bar spinner running for minutes.
    set_state(ctx, account_id, "idle").await;

    // A manual Sync Now resolves at the foreground Inbox boundary. Schedule a
    // normal pass immediately afterward for actions and other folders.
    if foreground_only || inbox_forward_remaining {
        return Ok(true);
    }

    // Execute one bounded action slice. If more remains, return immediately;
    // the actor schedules another pass now and checks INBOX again first.
    let action_slice = queue::execute_due(ctx, config, session).await?;
    if action_slice.due_remaining {
        return Ok(true);
    }

    for folder in ordered {
        if folder.role.as_deref() == Some(roles::INBOX) {
            continue;
        }
        if folder.role.as_deref() == Some(roles::ALL) && config.provider != Provider::Gmail {
            continue;
        }
        // Non-inbox folders get new-mail checks every cycle but heavy
        // reconciliation (flags/expunge) only every 5th cycle.
        let heavy = !action_replay && cycle > 0 && cycle.is_multiple_of(5);
        if let Err(e) =
            sync_folder(ctx, config, session, folder, heavy, hist_tx, inbox_baseline).await
        {
            tracing::warn!(folder = %folder.imap_name, "folder sync failed: {e}");
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
            for rf in &remote {
                let role = crate::sync::folder_map::detect_role(rf);
                if !crate::sync::folder_map::should_sync(rf, role) {
                    continue;
                }
                repo::folders::upsert(conn, account_id, &rf.name, rf.delimiter.as_deref(), role)?;
            }
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
    let selected = imap::select(session, &folder.imap_name).await?;
    let is_inbox = folder.role.as_deref() == Some(roles::INBOX);

    // UIDVALIDITY handling.
    if let (Some(stored), Some(remote)) = (folder.uidvalidity, selected.uid_validity)
        && stored != remote
    {
        tracing::warn!(folder = %folder.imap_name, "UIDVALIDITY changed; resetting uid map");
        let fid = folder.id;
        ctx.db
            .write(move |conn| repo::folders::reset_uid_mappings(conn, fid))
            .await?;
        if is_inbox {
            *inbox_baseline = None;
        }
    }

    if is_inbox
        && inbox_baseline
            .as_ref()
            .is_none_or(|b| b.uid_validity != selected.uid_validity)
    {
        *inbox_baseline = Some(InboxBaseline {
            uid_validity: selected.uid_validity,
            first_live_uid: selected.uid_next.unwrap_or(1).max(1) as u32,
        });
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

    let mut forward_remaining = false;
    if fresh_folder.backfill_cursor.is_none() {
        // Snapshot the server high-water immediately. The dedicated history
        // worker then walks downward from that point, while this actor owns all
        // later UIDs. This avoids a long initial backfill swallowing genuinely
        // live mail or leaving a gap above a date-filtered search result.
        let live_high = selected.uid_next.unwrap_or(1).saturating_sub(1).max(0);
        let history_cursor = live_high.saturating_add(1).max(1);
        let fid = fresh_folder.id;
        ctx.db
            .write(move |conn| {
                repo::folders::set_last_seen_uid(conn, fid, live_high)?;
                repo::folders::set_backfill(conn, fid, Some(history_cursor), live_high == 0)
            })
            .await?;
        let _ = hist_tx.try_send(());
    } else {
        // New mail since last seen UID.
        let last_seen = fresh_folder.last_seen_uid.max(0) as u32;
        let uid_next = selected.uid_next.unwrap_or(i64::MAX);
        if (last_seen as i64) + 1 < uid_next {
            let range_end =
                ((last_seen as u64) + HEADER_CHUNK as u64).min((uid_next - 1).max(0) as u64) as u32;
            forward_remaining = (range_end as i64) < uid_next.saturating_sub(1);
            let set = format!("{}:{range_end}", last_seen + 1);
            let headers = imap::fetch_headers(session, &set).await?;
            let new: Vec<FetchedHeader> = headers
                .into_iter()
                .filter(|h| h.uid > last_seen && h.uid <= range_end)
                .collect();
            if !new.is_empty() {
                tracing::info!(
                    account_id,
                    folder = %folder.imap_name,
                    count = new.len(),
                    since_uid = last_seen,
                    "receive: new mail on server",
                );
                let notify_min_uid = if is_inbox {
                    inbox_baseline.as_ref().map(|b| b.first_live_uid)
                } else {
                    None
                };
                let (thread_ids, fresh) =
                    store_headers(ctx, config, &fresh_folder, new, notify_min_uid).await?;
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
            // The command successfully scanned the whole bounded range. Move
            // the scan watermark even when UIDs are sparse or every row was a
            // duplicate; parse failures are persisted separately for retry.
            let fid = fresh_folder.id;
            ctx.db
                .write(move |conn| repo::folders::set_last_seen_uid(conn, fid, range_end as i64))
                .await?;
        }
        if heavy {
            reconcile_flags(ctx, session, &fresh_folder).await?;
            reconcile_expunges(ctx, session, &fresh_folder).await?;
        }
        // Historical backfill (extending the window downward) also runs on the
        // history connection; just make sure it's awake.
        if !fresh_folder.backfill_done {
            let _ = hist_tx.try_send(());
        }
    }
    Ok(forward_remaining)
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
            let mut ordered: Vec<&Folder> = folders.iter().collect();
            ordered.sort_by_key(|f| match f.role.as_deref() {
                Some(roles::INBOX) => 0,
                Some(roles::SENT) => 1,
                Some(roles::DRAFTS) => 2,
                Some(roles::ARCHIVE) => 3,
                _ => 4,
            });

            for folder in ordered {
                if folder.role.as_deref() == Some(roles::ALL) && config.provider != Provider::Gmail
                {
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
                    imap::select(s, &folder.imap_name).await?;
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
                            folder = %folder.imap_name,
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
                if let Some(s) = session.take() {
                    imap::logout(s).await;
                }
                break;
            }
        }
    }
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
    if session.is_none() {
        *session = Some(connect(ctx, config).await?);
    }
    let s = session
        .as_mut()
        .ok_or_else(|| CoreError::Other("header retry lost its IMAP session".into()))?;
    imap::select(s, &folder.imap_name).await?;
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
    let (thread_ids, _) = store_headers(ctx, config, &folder, headers, None).await?;
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
        let (thread_ids, _) = store_headers(ctx, config, folder, headers, None).await?;
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
        folder: folder.imap_name.clone(),
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
    let (thread_ids, _) = store_headers(ctx, config, folder, headers, None).await?;
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
        folder: folder.imap_name.clone(),
        phase: "history".into(),
        done: 0,
        total: 0,
    }));
    emit_sync_status(ctx, config.id).await;
    Ok(())
}

/// A message older than this never triggers the new-mail chime/notification,
/// even if it lands unread - catches UIDVALIDITY resets and server-side moves
/// replaying old mail as new-to-the-folder UIDs.
const CHIME_RECENT_MS: i64 = 24 * 60 * 60 * 1000;

/// Base candidacy for surfacing a freshly arrived message: unread, incoming,
/// newer than the notify watermark, and genuinely recent (so replayed old UIDs
/// from a UIDVALIDITY reset or a server-side move stay silent). This is scope-
/// agnostic on purpose - the *desktop-notification* scope (Important / all /
/// specific tabs) is applied later by the host dispatcher, which can see the
/// thread's resolved tab. Bulk/robot filtering lives in [`chime_worthy`].
fn arrival_eligible(
    notify_min_uid: Option<u32>,
    uid: u32,
    is_read: bool,
    is_outgoing: bool,
    arrival_ms: i64,
    now: i64,
) -> bool {
    notify_min_uid.is_some_and(|min_uid| uid >= min_uid)
        && !is_read
        && !is_outgoing
        && now - arrival_ms < CHIME_RECENT_MS
}

/// Whether a fresh arrival should ring the audible new-mail chime. The chime
/// stays human-only regardless of the desktop-notification scope, so a
/// newsletter/monitoring flood never rings the bell every minute.
fn chime_worthy(is_automated: bool, from_email: &str) -> bool {
    !is_automated && !crate::mime::robot_sender(from_email)
}

#[cfg(test)]
mod notification_policy_tests {
    use super::{CHIME_RECENT_MS, arrival_eligible, chime_worthy};

    fn eligible(baseline: Option<u32>, uid: u32, read: bool, outgoing: bool, age_ms: i64) -> bool {
        let now = 2 * CHIME_RECENT_MS;
        arrival_eligible(baseline, uid, read, outgoing, now - age_ms, now)
    }

    #[test]
    fn only_live_unread_incoming_mail_is_a_notification_candidate() {
        // Automated/robot senders are still candidates here - scope filtering
        // happens later in the host dispatcher, not at enqueue time.
        assert!(eligible(Some(100), 100, false, false, 1_000));
        // No baseline yet (initial catch-up), older UID, read, or outgoing all
        // disqualify, as does mail that arrived outside the recency window.
        assert!(!eligible(None, 100, false, false, 1_000));
        assert!(!eligible(Some(100), 99, false, false, 1_000));
        assert!(!eligible(Some(100), 100, true, false, 1_000));
        assert!(!eligible(Some(100), 100, false, true, 1_000));
        assert!(!eligible(Some(100), 100, false, false, CHIME_RECENT_MS));
    }

    #[test]
    fn chime_stays_human_only() {
        assert!(chime_worthy(false, "alice@example.com"));
        assert!(!chime_worthy(true, "digest@example.com"));
        assert!(!chime_worthy(false, "notifications@example.com"));
    }
}

/// Insert fetched headers. Returns `(all, fresh)` thread ids for the newly
/// inserted messages: `all` for list refreshes, `fresh` only for threads that
/// gained an unread, incoming, recent message (chime/notification-worthy).
async fn store_headers(
    ctx: &SyncCtx,
    config: &AccountConfig,
    folder: &Folder,
    headers: Vec<FetchedHeader>,
    notify_min_uid: Option<u32>,
) -> Result<(Vec<i64>, Vec<i64>)> {
    if headers.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    let account_id = config.id;
    let account_email = config.email.to_lowercase();
    let folder_id = folder.id;
    let folder_role = folder.role.clone();

    ctx.db
        .write(move |conn| {
            let tx = conn.transaction()?;
            let settings = repo::settings::get(&tx)?;
            let auto_labels = settings.auto_labels_enabled;
            let ai_categorize = settings.ai_categorize;
            let mut thread_ids: Vec<i64> = Vec::new();
            let mut fresh_ids: Vec<i64> = Vec::new();
            // Threads already enqueued for a desktop notification this batch, so
            // a thread that gains several messages in one fetch notifies once.
            let mut enqueued_threads: Vec<i64> = Vec::new();
            let mut max_uid: i64 = 0;
            let mut inserted = 0u32;

            for fh in &headers {
                max_uid = max_uid.max(fh.uid as i64);

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
                if let Some(mid) = &parsed.message_id
                    && let Some(existing) = repo::messages::by_message_id(&tx, account_id, mid)?
                {
                    if existing.uid.is_none()
                        && !repo::actions::has_active_move_from(&tx, existing.id, folder_id)?
                    {
                        repo::messages::set_uid_and_folder(
                            &tx,
                            existing.id,
                            folder_id,
                            Some(fh.uid as i64),
                        )?;
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

                let date_ms = parsed
                    .date_ms
                    .or(fh.internal_date_ms)
                    .unwrap_or_else(now_ms);

                let from_email = parsed
                    .from
                    .as_ref()
                    .map(|a| a.email.to_lowercase())
                    .unwrap_or_default();
                let is_outgoing =
                    from_email == account_email || folder_role.as_deref() == Some(roles::SENT);

                let thread_id =
                    crate::sync::threading::resolve_thread(&tx, account_id, &parsed, date_ms)?;

                let nm = NewMessage {
                    account_id,
                    folder_id,
                    uid: Some(fh.uid as i64),
                    message_id: parsed.message_id.clone(),
                    gm_msgid: None,
                    gm_thrid: None,
                    subject: parsed.subject.clone(),
                    from: parsed.from.clone(),
                    to: parsed.to.clone(),
                    cc: parsed.cc.clone(),
                    bcc: parsed.bcc.clone(),
                    date: date_ms,
                    internal_date: fh.internal_date_ms,
                    is_read: fh.flags.seen || is_outgoing,
                    is_starred: fh.flags.flagged,
                    is_draft: fh.flags.draft || folder_role.as_deref() == Some(roles::DRAFTS),
                    is_outgoing,
                    is_automated: parsed.is_automated,
                    has_attachments: fh.has_attachments,
                    size: fh.size.map(|s| s as i64),
                    snippet: String::new(),
                    references: parsed.references.clone(),
                    list_unsubscribe: parsed.list_unsubscribe.clone(),
                    list_unsubscribe_post: parsed.list_unsubscribe_post.clone(),
                    sender_addr: parsed.via.clone(),
                    sender_verification: parsed.sender_verification,
                };
                let msg_id = repo::messages::insert(&tx, &nm, thread_id)?;
                if let Some(plan) = fh.mime_plan.as_ref() {
                    repo::messages::set_mime_plan(&tx, msg_id, Some(plan))?;
                    repo::messages::upsert_planned_attachments(&tx, msg_id, &plan.attachments)?;
                }
                let _ = repo::sync_failures::clear_header(&tx, folder_id, fh.uid as i64);
                inserted += 1;
                repo::labels::reconcile_keywords(&tx, msg_id, &fh.flags.keywords)?;
                repo::threads::recompute(&tx, thread_id)?;
                repo::search::index_message(&tx, msg_id)?;

                // Harvest contacts.
                let when = date_ms;
                if is_outgoing {
                    for a in parsed.to.iter().chain(parsed.cc.iter()) {
                        repo::contacts::harvest(&tx, account_id, a, true, when)?;
                    }
                } else if let Some(from) = &parsed.from {
                    repo::contacts::harvest(&tx, account_id, from, false, when)?;
                }

                if !thread_ids.contains(&thread_id) {
                    thread_ids.push(thread_id);
                }
                // A fresh arrival (unread + incoming + recent, past the notify
                // watermark) is a candidate for both the chime and a desktop
                // notification.
                let arrival_ms = fh.internal_date_ms.unwrap_or(date_ms);
                if arrival_eligible(
                    notify_min_uid,
                    fh.uid,
                    nm.is_read,
                    nm.is_outgoing,
                    arrival_ms,
                    now_ms(),
                ) {
                    // Enqueue a durable outbox row (once per thread this batch).
                    // The native dispatcher consumes it after commit and applies
                    // the user's notification scope against the thread's resolved
                    // tab - so bulk mail is enqueued too and filtered there, not
                    // here. Snapshotting sender/subject now keeps delivery
                    // independent of any later UI or hydration step.
                    if !enqueued_threads.contains(&thread_id) {
                        enqueued_threads.push(thread_id);
                        repo::notifications::enqueue(&tx, msg_id)?;
                    }
                    // The audible chime stays human-only regardless of scope.
                    if chime_worthy(nm.is_automated, &from_email) && !fresh_ids.contains(&thread_id)
                    {
                        tracing::info!(
                            account_id,
                            thread_id,
                            from = %from_email,
                            "receive: chime-worthy new mail",
                        );
                        fresh_ids.push(thread_id);
                    }
                }
            }

            if max_uid > 0 {
                repo::folders::set_last_seen_uid(&tx, folder_id, max_uid)?;
            }
            // Resolve each touched thread to a single tab (rules + heuristic, or
            // mark it 'pending' for the async AI pass). Runs once per thread with
            // all its messages in place. See crate::route.
            if auto_labels {
                let splits = repo::splits::list(&tx)?;
                for &tid in &thread_ids {
                    crate::route::route_thread_deterministic(&tx, &splits, ai_categorize, tid)?;
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

/// Remove local messages whose UIDs vanished from the server (within the
/// synced window).
async fn reconcile_expunges(ctx: &SyncCtx, session: &mut Session, folder: &Folder) -> Result<()> {
    let remote_uids = imap::uid_search_all(session).await?;
    let folder_id = folder.id;
    let cursor = folder.backfill_cursor.unwrap_or(i64::MAX);

    let changed = ctx
        .db
        .write(move |conn| {
            let remote: std::collections::HashSet<i64> =
                remote_uids.iter().map(|u| *u as i64).collect();
            let local = repo::messages::uids_in_folder(conn, folder_id)?;
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
        error @ (CoreError::Imap(_) | CoreError::Network(_) | CoreError::Offline) => {
            BodyChunkError::Reconnect(error)
        }
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
    use std::collections::{HashSet, VecDeque};
    use std::sync::atomic::{AtomicU64, Ordering};

    let account_id = config.id;
    // Message ids attempted this drain whose bodies the server didn't return
    // (expunged mid-sync): skipped for the rest of the drain so it terminates;
    // expunge reconciliation removes the rows.
    let skip: Arc<std::sync::Mutex<HashSet<i64>>> = Arc::default();
    let mut did_work = false;

    loop {
        let mail_history = settings_rx.borrow_and_update().mail_history;
        let cutoff_ms = mail_history.cutoff_ms_at(now_ms());
        let folders = ctx
            .db
            .read(move |conn| repo::folders::list(conn, Some(account_id)))
            .await?;
        let mut ordered: Vec<&Folder> = folders.iter().collect();
        ordered.sort_by_key(|f| match f.role.as_deref() {
            Some(roles::INBOX) => 0,
            Some(roles::SENT) => 1,
            _ => 2,
        });

        let mut chunks: VecDeque<BodyChunk> = VecDeque::new();
        for folder in ordered {
            if folder.role.as_deref() == Some(roles::ALL) && config.provider != Provider::Gmail {
                continue;
            }
            let fid = folder.id;
            let missing = ctx
                .db
                .read(move |conn| {
                    repo::messages::missing_bodies_since(conn, fid, i64::MAX, cutoff_ms)
                })
                .await?;
            let missing: Vec<(i64, i64)> = {
                let skip = skip.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                missing
                    .into_iter()
                    .filter(|(mid, _)| !skip.contains(mid))
                    .collect()
            };
            for chunk in missing.chunks(BODY_FETCH_CHUNK) {
                chunks.push_back(BodyChunk {
                    folder_id: folder.id,
                    folder_name: folder.imap_name.clone(),
                    uid_validity: folder.uidvalidity,
                    items: chunk.to_vec(),
                });
            }
        }

        if chunks.is_empty() {
            if did_work {
                tracing::info!(account_id, "body backfill: drained");
            }
            return Ok(());
        }
        did_work = true;
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
        let mut handles = Vec::with_capacity(workers);
        for _ in 0..workers {
            handles.push(tokio::spawn(body_worker(
                ctx.clone(),
                config.clone(),
                queue.clone(),
                skip.clone(),
                persisted.clone(),
                settings_rx.clone(),
            )));
        }
        let mut worker_error = None;
        for handle in handles {
            match handle.await {
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
        if persisted.load(Ordering::Relaxed) == 0 {
            return Err(CoreError::Imap(
                "body backfill made no progress; will retry".into(),
            ));
        }
        // Re-scan: headers that landed while this round ran get picked up too.
    }
}

/// One backfill-pool connection: pops chunks off the shared queue until it is
/// empty or the connection breaks. On error the chunk goes back on the queue
/// for the surviving workers (or the next round).
async fn body_worker(
    ctx: SyncCtx,
    config: AccountConfig,
    queue: Arc<tokio::sync::Mutex<std::collections::VecDeque<BodyChunk>>>,
    skip: Arc<std::sync::Mutex<std::collections::HashSet<i64>>>,
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
        match fetch_body_chunk(&ctx, &config, &mut session, &mut selected, &chunk, &skip).await {
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
/// bodies were cached. UIDs the server didn't return have vanished; they go
/// into `skip` so the drain terminates (expunge reconciliation drops the rows).
async fn fetch_body_chunk(
    ctx: &SyncCtx,
    config: &AccountConfig,
    session: &mut Session,
    selected: &mut Option<(String, Option<i64>)>,
    chunk: &BodyChunk,
    skip: &std::sync::Mutex<std::collections::HashSet<i64>>,
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
                    folder.imap_name == folder_name && folder.uidvalidity == uid_validity
                }))
            }
        })
        .await
        .map_err(BodyChunkError::Fatal)?;
    if !folder_is_current {
        skip.lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .extend(chunk.items.iter().map(|(message_id, _)| *message_id));
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
            skip.lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .extend(chunk.items.iter().map(|(message_id, _)| *message_id));
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

    for (message_id, _) in &failures {
        skip.lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(*message_id);
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
            let uids: Vec<u32> = batch.iter().map(|item| item.uid).collect();
            let fetched = imap::fetch_content_sections_batch(session, &uids, &sections)
                .await
                .map_err(BodyChunkError::Reconnect)?;
            let mut by_uid: std::collections::HashMap<_, _> = fetched
                .into_iter()
                .map(|message| (message.uid, message.sections))
                .collect();
            let (decoded, batch_failures) = tokio::task::spawn_blocking(move || {
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
                (decoded, failures)
            })
            .await
            .map_err(|error| {
                BodyChunkError::Fatal(CoreError::Other(format!(
                    "content decode task failed: {error}"
                )))
            })?;
            for (message_id, _) in &batch_failures {
                skip.lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .insert(*message_id);
            }
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
    session: &mut Session,
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
    select_folder_for_remote_read(ctx, session, &folder).await?;
    if !remote_location_is_current(ctx, message_id, folder_id, uid).await? {
        reset_bodies_none(ctx, std::iter::once(message_id)).await;
        return Ok(());
    }
    store_one_body(ctx, config, session, message_id, folder_id, uid as u32).await
}

async fn store_one_body(
    ctx: &SyncCtx,
    config: &AccountConfig,
    session: &mut Session,
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
    let retry_at = now_ms() + 60_000;
    ctx.db
        .write(move |conn| {
            let tx = conn.transaction()?;
            for (message_id, detail) in failures {
                if current_body_row(&tx, message_id)?.is_none() {
                    continue;
                }
                repo::sync_failures::record_content(&tx, message_id, Some(retry_at), &detail)?;
            }
            tx.commit()?;
            Ok(())
        })
        .await
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
            CoreError::Imap(format!(
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
    session: &mut Session,
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
        let plans = imap::fetch_mime_plans_batch(session, &[uid]).await?;
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
            return fetch_full_open_fallback(ctx, config, session, message_id, folder_id, uid)
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
        let fetched = imap::fetch_content_sections(session, uid, &section_ids).await?;
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
        Err(error) if allow_open_fallback => {
            tracing::warn!(
                message_id,
                error = %error,
                "selective content unavailable; using explicit-open full MIME fallback"
            );
            return fetch_full_open_fallback(ctx, config, session, message_id, folder_id, uid)
                .await;
        }
        Err(error) => return Err(error),
    };
    persist_selective_batch(
        ctx,
        config.id,
        SelectivePersistGuard::Priority { folder_id },
        vec![content],
    )
    .await
    .map(|_| ())
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
    let parsed_calendar_events: Vec<_> = parsed
        .calendar_parts
        .iter()
        .flat_map(|ics| crate::calendar::parse_ics(ics))
        .collect();
    let config_id = config.id;
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
            repo::messages::store_body(
                &tx,
                message_id,
                parsed.text.as_deref(),
                parsed.html.as_deref(),
                Some(&path_str),
                // Real files only, mirroring BODYSTRUCTURE's disposition check
                // and the message-card footer (both ignore inline images), so
                // the list paperclip stays consistent before and after backfill.
                parsed.attachments.iter().any(|a| !a.is_inline),
                Some(&parsed.snippet),
            )?;
            repo::messages::set_sender_verification(
                &tx,
                message_id,
                parsed.headers.sender_verification,
            )?;
            let atts: Vec<repo::messages::NewAttachment> = parsed
                .attachments
                .iter()
                .map(|a| repo::messages::NewAttachment {
                    message_id,
                    part_id: Some(a.part_id.as_str()),
                    filename: a.filename.as_deref(),
                    mime_type: a.mime_type.as_deref(),
                    size: Some(a.size),
                    content_id: a.content_id.as_deref(),
                    is_inline: a.is_inline,
                })
                .collect();
            repo::messages::replace_attachments(&tx, message_id, &atts)?;
            repo::search::index_message(&tx, message_id)?;
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

    if !parsed_calendar_events.is_empty() {
        ctx.calendar_db
            .write(move |conn| {
                let tx = conn.transaction()?;
                for event in &parsed_calendar_events {
                    repo::calendar::upsert(&tx, config_id, message_id, event)?;
                }
                tx.commit()?;
                Ok(())
            })
            .await?;
        ctx.bus.emit(CoreEvent::CalendarUpdated {
            account_id: config_id,
        });
    }

    if let Some(tid) = thread_id {
        ctx.bus.emit(CoreEvent::MailUpdated {
            thread_ids: vec![tid],
        });
    }
    Ok(())
}
