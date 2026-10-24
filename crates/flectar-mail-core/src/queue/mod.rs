//! Pending-action executor. Local mutations already happened optimistically
//! when the action was enqueued (see Core::perform_action); this module
//! replays the intent against the server, in order, when connected. For mail
//! that exists only on this device, the queued change is already the result,
//! and only deletions have work left to finish.

use crate::accounts::credentials::{self, Slot};
use crate::db::repo;
use crate::error::{CoreError, Result};
use crate::events::CoreEvent;
use crate::imap::{self, Session};
use crate::models::*;
use crate::smtp;
use crate::sync::engine::SyncCtx;
use tokio::time::{Duration, Instant};

const MAX_ATTEMPTS: i64 = 8;
const MAX_ACTIONS_PER_SLICE: i64 = 20;
const MAX_SLICE_DURATION: Duration = Duration::from_secs(2);
const OFFLINE_SEND_RETRY_DELAY: Duration = Duration::from_secs(30);

/// Outcome of one fair pending-action execution slice.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ActionSliceResult {
    /// Actions atomically claimed and attempted during this slice.
    pub processed: usize,
    /// More IMAP/SMTP actions are ready now; the caller should schedule another
    /// slice without waiting for the normal sync interval.
    pub due_remaining: bool,
}

/// Where queued actions are carried out.
enum ActionTarget<'a> {
    /// The account's IMAP mailbox.
    Imap(&'a mut Session),
    /// The SMTP submission worker, which only sends.
    Submission,
    /// Mail that exists only on this device.
    Device,
}

pub async fn execute_due(
    ctx: &SyncCtx,
    config: &AccountConfig,
    session: &mut Session,
) -> Result<ActionSliceResult> {
    execute_due_except_send(ctx, config, ActionTarget::Imap(session)).await
}

/// Complete the due actions of an account whose mail exists only on this
/// device. Nothing here contacts a server.
pub async fn execute_due_on_device(
    ctx: &SyncCtx,
    config: &AccountConfig,
) -> Result<ActionSliceResult> {
    execute_due_except_send(ctx, config, ActionTarget::Device).await
}

async fn execute_due_except_send(
    ctx: &SyncCtx,
    config: &AccountConfig,
    target: ActionTarget<'_>,
) -> Result<ActionSliceResult> {
    let account_id = config.id;
    let due = ctx
        .db
        .read(move |conn| {
            repo::actions::due_except_send(conn, account_id, now_ms(), MAX_ACTIONS_PER_SLICE)
        })
        .await?;
    let result = execute_actions(ctx, config, target, due).await?;
    let due_remaining = ctx
        .db
        .read(move |conn| repo::actions::has_due_except_send(conn, account_id, now_ms()))
        .await?;
    Ok(ActionSliceResult {
        due_remaining,
        ..result
    })
}

/// Execute SMTP submissions independently of the IMAP synchronization actor.
/// This worker owns only `send` actions; remote Sent filing is enqueued as a
/// separate IMAP action after the relay has accepted the message.
pub async fn execute_due_sends(ctx: &SyncCtx, config: &AccountConfig) -> Result<ActionSliceResult> {
    let account_id = config.id;
    let due = ctx
        .db
        .read(move |conn| {
            repo::actions::due_sends(conn, account_id, now_ms(), MAX_ACTIONS_PER_SLICE)
        })
        .await?;
    let result = execute_actions(ctx, config, ActionTarget::Submission, due).await?;
    let due_remaining = ctx
        .db
        .read(move |conn| repo::actions::has_due_sends(conn, account_id, now_ms()))
        .await?;
    Ok(ActionSliceResult {
        due_remaining,
        ..result
    })
}

async fn execute_actions(
    ctx: &SyncCtx,
    config: &AccountConfig,
    mut target: ActionTarget<'_>,
    due: Vec<repo::actions::PendingAction>,
) -> Result<ActionSliceResult> {
    let account_id = config.id;
    let started = Instant::now();
    let mut processed = 0;

    for action in due {
        // This is a cooperative budget: never abandon an action midway through
        // an IMAP/SMTP command, but do not start another after the slice expires.
        if started.elapsed() >= MAX_SLICE_DURATION {
            break;
        }

        let action_id = action.id;
        // Atomic claim: if undo/cancel got here first, skip.
        let claimed = ctx
            .db
            .write(move |conn| repo::actions::try_claim(conn, action_id))
            .await?;
        if !claimed {
            continue;
        }
        processed += 1;

        let outcome = execute_one(ctx, config, &mut target, &action).await;
        match outcome {
            Ok(()) => {
                ctx.db
                    .write(move |conn| repo::actions::set_state(conn, action_id, "done", None))
                    .await?;
                ctx.bus.emit(CoreEvent::ActionState {
                    action_id,
                    state: "done".into(),
                    error: None,
                });
            }
            Err(e @ (CoreError::NeedsReauth | CoreError::Auth(_))) => {
                // Leave pending; the actor will pause on reauth. Log the real
                // cause: for SMTP this is usually the mail host rejecting auth
                // (e.g. Office365 tenants disable SMTP AUTH by default), which
                // was previously silent and looked like a stuck "Sending…".
                tracing::warn!(
                    account_id, action_id, kind = %action.kind, error = %e,
                    "action needs auth; pausing (check mail-host auth / SMTP AUTH enabled)",
                );
                // The optimistic local mutation remains visible while auth is
                // paused, so every action kind must tell the UI it has not yet
                // reached the server.
                ctx.bus.emit(CoreEvent::ActionState {
                    action_id,
                    state: "paused".into(),
                    error: Some(e.to_string()),
                });
                let msg = e.to_string();
                ctx.db
                    .write(move |conn| {
                        repo::actions::bump_attempt(conn, action_id, now_ms() + 60_000, &msg)
                    })
                    .await?;
                return Err(CoreError::NeedsReauth);
            }
            Err(e) => {
                let msg = e.to_string();
                if action.kind == "send" && is_connection_failure(&msg) {
                    // SMTP has no shared session whose successful connection
                    // can gate this independent worker. A refused/timed-out
                    // connection means the transport is unavailable, not that
                    // the durable send is invalid, so preserve its full retry
                    // budget while the device or relay is offline.
                    let retry_at = now_ms().saturating_add(
                        i64::try_from(OFFLINE_SEND_RETRY_DELAY.as_millis()).unwrap_or(i64::MAX),
                    );
                    let saved = msg.clone();
                    ctx.db
                        .write(move |conn| {
                            repo::actions::defer_offline(conn, action_id, retry_at, &saved)
                        })
                        .await?;
                    ctx.bus.emit(CoreEvent::ActionState {
                        action_id,
                        state: "retrying".into(),
                        error: Some(msg),
                    });
                    return Err(e);
                }
                let attempts = action.attempts + 1;
                tracing::warn!(
                    account_id, action_id, kind = %action.kind, attempts, error = %msg,
                    "action attempt failed",
                );
                if attempts >= MAX_ATTEMPTS || is_permanent(&e) {
                    fail_action(ctx, &action, msg).await?;
                } else {
                    // Exponential backoff with jitter.
                    let delay = (1 << attempts.min(8)) * 1000 + (action_id % 997);
                    let m = msg.clone();
                    ctx.db
                        .write(move |conn| {
                            repo::actions::bump_attempt(conn, action_id, now_ms() + delay, &m)
                        })
                        .await?;
                    ctx.bus.emit(CoreEvent::ActionState {
                        action_id,
                        state: "retrying".into(),
                        error: Some(msg.clone()),
                    });
                    // Connection-level errors: bail out, actor reconnects.
                    if is_connection_failure(&msg) {
                        return Err(e);
                    }
                }
            }
        }
    }

    Ok(ActionSliceResult {
        processed,
        due_remaining: false,
    })
}

fn is_permanent(error: &CoreError) -> bool {
    match error {
        CoreError::NotFound(_) => true,
        // Tagged NO/BAD responses reject the command itself. Async IMAP has
        // used both `NO ...` and `no: ...` display forms across versions.
        CoreError::Imap(message) => {
            let message = message.trim().to_ascii_lowercase();
            if message.starts_with("this imap server does not support safe permanent deletion") {
                return true;
            }
            ["no", "bad"].iter().any(|status| {
                message == *status
                    || message
                        .strip_prefix(status)
                        .is_some_and(|tail| matches!(tail.chars().next(), Some(' ' | ':' | '[')))
            })
        }
        // All SMTP 5xx replies are permanent failures of the current request.
        CoreError::Smtp(message) => has_permanent_smtp_reply(message),
        _ => false,
    }
}

/// Whether an SMTP error carries a 5xx reply code. A code stands alone, as in
/// lettre's `permanent error (550)` or a raw `550 5.7.1 ...` reply, which
/// keeps the `host:587:` prefix of every SMTP error from reading as one.
fn has_permanent_smtp_reply(message: &str) -> bool {
    let bytes = message.as_bytes();
    bytes.windows(3).enumerate().any(|(start, code)| {
        let before = start.checked_sub(1).map(|index| bytes[index]);
        let after = bytes.get(start + 3).copied();
        code[0] == b'5'
            && code.iter().all(u8::is_ascii_digit)
            && before.is_none_or(|byte| byte.is_ascii_whitespace() || byte == b'(')
            && after.is_none_or(|byte| byte.is_ascii_whitespace() || matches!(byte, b')' | b'-'))
    })
}

fn is_connection_failure(msg: &str) -> bool {
    let msg = msg.to_ascii_lowercase();
    msg.contains("connect")
        || msg.contains("broken")
        || msg.contains("closed")
        || msg.contains("timed out")
        || msg.contains("unexpected eof")
}

fn is_move_kind(kind: &str) -> bool {
    matches!(
        kind,
        "archive" | "unarchive" | "trash" | "spam" | "not_spam" | "move"
    )
}

/// Mark an exhausted action failed and undo its optimistic move when it is
/// still the newest intent for that message. A later queued move owns the
/// visible location and must not be overwritten by an older failure.
async fn fail_action(
    ctx: &SyncCtx,
    action: &repo::actions::PendingAction,
    message: String,
) -> Result<()> {
    let failed = action.clone();
    let saved_message = message.clone();
    let changed_thread = ctx
        .db
        .write(move |conn| mark_failed_and_rollback(conn, &failed, &saved_message))
        .await?;

    if let Some(thread_id) = changed_thread {
        ctx.bus.emit(CoreEvent::MailUpdated {
            thread_ids: vec![thread_id],
        });
    }
    ctx.bus.emit(CoreEvent::ActionState {
        action_id: action.id,
        state: "failed".into(),
        error: Some(message),
    });
    Ok(())
}

fn mark_failed_and_rollback(
    conn: &mut rusqlite::Connection,
    failed: &repo::actions::PendingAction,
    message: &str,
) -> Result<Option<i64>> {
    let tx = conn.transaction()?;
    repo::actions::set_state(&tx, failed.id, "failed", Some(message))?;
    if !is_move_kind(&failed.kind) {
        tx.commit()?;
        return Ok(None);
    }

    let (Some(message_id), Some(source_folder), target_folder) = (
        failed.message_id,
        failed.payload["srcFolderId"].as_i64(),
        failed.payload["targetFolderId"].as_i64(),
    ) else {
        tx.commit()?;
        return Ok(None);
    };
    let has_newer_move: bool = tx.query_row(
        "SELECT EXISTS(
           SELECT 1 FROM pending_actions
           WHERE message_id=?1 AND id>?2
             AND kind IN ('archive','unarchive','trash','spam','not_spam','move')
             AND state IN ('pending','inflight','done')
         )",
        rusqlite::params![message_id, failed.id],
        |row| row.get(0),
    )?;
    let row = repo::messages::get_row(&tx, message_id)?;
    if has_newer_move
        || row
            .as_ref()
            .is_none_or(|row| row.folder_id != target_folder)
    {
        tx.commit()?;
        return Ok(None);
    }

    let source_uid = failed.payload["srcUid"].as_i64();
    repo::messages::set_uid_and_folder(&tx, message_id, source_folder, source_uid)?;
    let protocol: crate::models::MailProtocol = tx.query_row(
        "SELECT mail_protocol FROM accounts WHERE id=?1",
        rusqlite::params![failed.account_id],
        |row| row.get(0),
    )?;
    if protocol.has_multi_mailbox_messages() {
        if let Some(target_folder) = target_folder {
            tx.execute(
                "DELETE FROM message_folders WHERE message_id=?1 AND folder_id=?2",
                rusqlite::params![message_id, target_folder],
            )?;
        }
        tx.execute(
            "INSERT OR IGNORE INTO message_folders (message_id, folder_id) VALUES (?1, ?2)",
            rusqlite::params![message_id, source_folder],
        )?;
    }
    if let Some(thread_id) = failed.thread_id {
        repo::threads::recompute(&tx, thread_id)?;
    }
    tx.commit()?;
    Ok(failed.thread_id)
}

async fn execute_one(
    ctx: &SyncCtx,
    config: &AccountConfig,
    target: &mut ActionTarget<'_>,
    action: &repo::actions::PendingAction,
) -> Result<()> {
    if action.kind == "send" {
        return send_action(ctx, config, action).await;
    }
    match target {
        ActionTarget::Imap(session) => execute_on_imap(ctx, config, session, action).await,
        ActionTarget::Device => complete_on_device(ctx, config, action).await,
        ActionTarget::Submission => Err(CoreError::Other(format!(
            "{} action was routed to the submission worker",
            action.kind
        ))),
    }
}

/// Finish an action on mail that exists only on this device. Its change
/// was made when it was queued; deletions end with the same cleanup that
/// follows a server's confirmation.
async fn complete_on_device(
    ctx: &SyncCtx,
    config: &AccountConfig,
    action: &repo::actions::PendingAction,
) -> Result<()> {
    match action.kind.as_str() {
        "empty_trash" => {
            let snapshot_ids = crate::sync::snapshot_trash_ids(ctx, config.id).await?;
            crate::sync::finish_empty_trash(ctx, config.id, snapshot_ids).await
        }
        "delete_permanently" => crate::sync::finish_permanent_delete(ctx, action.message_id).await,
        "archive" | "unarchive" | "trash" | "spam" | "not_spam" | "move" | "mark_read"
        | "mark_unread" | "star" | "unstar" | "add_label" | "remove_label" | "snooze"
        | "unsnooze" => Ok(()),
        other => {
            tracing::warn!("action kind {other} has nothing to do on this device");
            Ok(())
        }
    }
}

async fn execute_on_imap(
    ctx: &SyncCtx,
    config: &AccountConfig,
    session: &mut Session,
    action: &repo::actions::PendingAction,
) -> Result<()> {
    match action.kind.as_str() {
        "empty_trash" => {
            let folder_id = action.payload["folderId"]
                .as_i64()
                .ok_or_else(|| CoreError::NotFound("Trash folder for queued purge".into()))?;
            let folder = ctx
                .db
                .read(move |conn| repo::folders::get(conn, folder_id))
                .await?
                .ok_or_else(|| CoreError::NotFound("Trash folder".into()))?;
            if folder.account_id != config.id
                || folder.role.as_deref() != Some(crate::models::roles::TRASH)
            {
                return Err(CoreError::Other(
                    "queued Trash folder does not match this account".into(),
                ));
            }
            let snapshot_ids = crate::sync::snapshot_trash_ids(ctx, config.id).await?;
            imap::select(session, &folder.path).await?;
            imap::empty_selected_trash(session).await?;
            crate::sync::finish_empty_trash(ctx, config.id, snapshot_ids).await
        }
        "delete_permanently" => {
            let Some(message_id) = action.message_id else {
                return Ok(());
            };
            if crate::sync::is_local_only_draft(ctx, message_id).await? {
                return crate::sync::finish_permanent_delete(ctx, Some(message_id)).await;
            }
            let row = ctx
                .db
                .read(move |conn| repo::messages::get_row(conn, message_id))
                .await?;
            let Some(row) = row else {
                return Ok(());
            };
            let (Some(folder_id), Some(uid)) = (row.folder_id, row.uid) else {
                return Err(CoreError::Other(
                    "message is waiting for its Trash UID".into(),
                ));
            };
            let uid = u32::try_from(uid)
                .ok()
                .filter(|uid| *uid > 0)
                .ok_or_else(|| CoreError::NotFound("valid Trash UID".into()))?;
            let folder = ctx
                .db
                .read(move |conn| repo::folders::get(conn, folder_id))
                .await?
                .ok_or_else(|| CoreError::NotFound("Trash folder".into()))?;
            if folder.account_id != config.id
                || folder.role.as_deref() != Some(crate::models::roles::TRASH)
            {
                return Err(CoreError::NotFound("message is no longer in Trash".into()));
            }
            let selected = imap::select(session, &folder.path).await?;
            match (folder.uidvalidity, selected.uid_validity) {
                (Some(stored), Some(remote)) if stored == remote => {}
                _ => {
                    return Err(CoreError::Imap(
                        "Trash UIDVALIDITY changed or is unavailable; waiting for mailbox sync"
                            .into(),
                    ));
                }
            }
            imap::uid_delete_permanently(session, uid).await?;
            crate::sync::finish_permanent_delete(ctx, action.message_id).await
        }
        "mark_read" => flag_action(ctx, session, action, "\\Seen", true).await,
        "mark_unread" => flag_action(ctx, session, action, "\\Seen", false).await,
        "star" => flag_action(ctx, session, action, "\\Flagged", true).await,
        "unstar" => flag_action(ctx, session, action, "\\Flagged", false).await,
        "archive" | "unarchive" | "trash" | "spam" | "not_spam" | "move" => {
            move_action(ctx, config, session, action).await
        }
        "add_label" => keyword_action(ctx, session, action, true).await,
        "remove_label" => keyword_action(ctx, session, action, false).await,
        "append_sent" => append_sent_action(ctx, config, session, action).await,
        // Local-only kinds recorded for undo history.
        "snooze" | "unsnooze" => Ok(()),
        other => {
            tracing::warn!("unknown action kind {other}");
            Ok(())
        }
    }
}

/// Resolve the message's current remote (folder_name, uid); selects the folder.
async fn resolve_remote(
    ctx: &SyncCtx,
    session: &mut Session,
    message_id: i64,
) -> Result<Option<(repo::folders::Folder, u32)>> {
    let row = ctx
        .db
        .read(move |conn| repo::messages::get_row(conn, message_id))
        .await?;
    let Some(row) = row else { return Ok(None) };

    // The payload's remote location: where the message lived when the action
    // was enqueued (optimistic mutation may have already moved it locally).
    let Some(folder_id) = row.folder_id else {
        return Ok(None);
    };
    let folder = ctx
        .db
        .read(move |conn| repo::folders::get(conn, folder_id))
        .await?;
    let Some(folder) = folder else {
        return Ok(None);
    };
    imap::select(session, &folder.path).await?;
    let uid = match valid_uid(row.uid) {
        Some(uid) => Some(uid),
        None => uid_by_message_id(session, row.message_id.as_deref()).await?,
    };
    Ok(uid.map(|uid| (folder, uid)))
}

/// For moves the local row already points at the *target* folder; the remote
/// source location is stored in the payload.
async fn resolve_source(
    ctx: &SyncCtx,
    session: &mut Session,
    action: &repo::actions::PendingAction,
    message_id: i64,
) -> Result<(repo::folders::Folder, u32)> {
    let fid = action.payload["srcFolderId"]
        .as_i64()
        .ok_or_else(|| CoreError::NotFound("source mailbox for queued move".into()))?;
    let folder = ctx
        .db
        .read(move |conn| repo::folders::get(conn, fid))
        .await?;
    let folder = folder.ok_or_else(|| CoreError::NotFound("source folder".into()))?;
    if folder.account_id != action.account_id {
        return Err(CoreError::Other(
            "queued move source belongs to another account".into(),
        ));
    }
    imap::select(session, &folder.path).await?;
    if let Some(uid) = valid_uid(action.payload["srcUid"].as_i64()) {
        return Ok((folder, uid));
    }
    let row = ctx
        .db
        .read(move |conn| repo::messages::get_row(conn, message_id))
        .await?;
    let uid = uid_by_message_id(session, row.and_then(|row| row.message_id).as_deref())
        .await?
        .ok_or_else(|| CoreError::NotFound("message in the source mailbox".into()))?;
    Ok((folder, uid))
}

fn valid_uid(uid: Option<i64>) -> Option<u32> {
    uid.and_then(|uid| u32::try_from(uid).ok())
        .filter(|uid| *uid > 0)
}

/// The newest copy of a message in the selected mailbox, by Message-ID. A
/// move that already ran leaves the message's UID unknown until its new
/// mailbox syncs, and an undo or a flag change can come first. SEARCH HEADER
/// matches substrings, or words on some servers, so each candidate's header
/// is compared whole, and copies awaiting expunge don't count.
async fn uid_by_message_id(session: &mut Session, message_id: Option<&str>) -> Result<Option<u32>> {
    let Some(bare) = message_id
        .map(|id| id.trim_matches(['<', '>']))
        .filter(|id| !id.is_empty())
    else {
        return Ok(None);
    };
    let candidates = imap::uid_search_header(session, "Message-ID", &format!("<{bare}>")).await?;
    let mut newest = None;
    for chunk in candidates.chunks(100) {
        let uids = chunk
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(",");
        for header in imap::fetch_headers(session, &uids).await? {
            if !header.flags.deleted
                && crate::mime::parse_header_block(&header.header_bytes)
                    .is_ok_and(|parsed| parsed.message_id.as_deref() == Some(bare))
            {
                newest = newest.max(Some(header.uid));
            }
        }
    }
    Ok(newest)
}

async fn flag_action(
    ctx: &SyncCtx,
    session: &mut Session,
    action: &repo::actions::PendingAction,
    flag: &str,
    add: bool,
) -> Result<()> {
    let Some(message_id) = action.message_id else {
        return Ok(());
    };
    match resolve_remote(ctx, session, message_id).await? {
        Some((_folder, uid)) => imap::store_flag(session, uid, flag, add).await,
        None => Ok(()), // deleted remotely; flag intent is moot
    }
}

/// Push a label as a custom IMAP keyword on the message's current remote copy.
async fn keyword_action(
    ctx: &SyncCtx,
    session: &mut Session,
    action: &repo::actions::PendingAction,
    add: bool,
) -> Result<()> {
    let Some(message_id) = action.message_id else {
        return Ok(());
    };
    let Some(keyword) = action.payload["keyword"].as_str() else {
        return Ok(());
    };
    match resolve_remote(ctx, session, message_id).await? {
        Some((_folder, uid)) => imap::store_flag(session, uid, keyword, add).await,
        None => Ok(()), // deleted remotely; label intent is moot
    }
}

async fn move_action(
    ctx: &SyncCtx,
    config: &AccountConfig,
    session: &mut Session,
    action: &repo::actions::PendingAction,
) -> Result<()> {
    let message_id = action
        .message_id
        .ok_or_else(|| CoreError::NotFound("message for queued move".into()))?;
    let (_src, uid) = resolve_source(ctx, session, action, message_id).await?;
    let tfid = action.payload["targetFolderId"]
        .as_i64()
        .ok_or_else(|| CoreError::NotFound("target mailbox for queued move".into()))?;
    let target = ctx
        .db
        .read(move |conn| repo::folders::get(conn, tfid))
        .await?
        .ok_or_else(|| CoreError::NotFound("target folder".into()))?;
    if target.account_id != action.account_id || target.account_id != config.id {
        return Err(CoreError::Other(
            "queued move target belongs to another account".into(),
        ));
    }

    imap::uid_move(session, uid, &target.path).await?;

    // The message's new UID in the target is unknown (COPYUID not parsed in
    // v1); clear it so the next target-folder sync re-links by Message-ID.
    // Actions that run before then look it up the same way.
    ctx.db
        .write(move |conn| repo::messages::set_uid_and_folder(conn, message_id, tfid, None))
        .await?;
    Ok(())
}

async fn append_sent_action(
    ctx: &SyncCtx,
    config: &AccountConfig,
    session: &mut Session,
    action: &repo::actions::PendingAction,
) -> Result<()> {
    let message_id = action
        .message_id
        .ok_or_else(|| CoreError::NotFound("message for Sent filing".into()))?;
    let row = ctx
        .db
        .read(move |conn| {
            repo::messages::get_row(conn, message_id)?
                .ok_or_else(|| CoreError::NotFound(format!("message {message_id}")))
        })
        .await?;
    if row.account_id != config.id || action.account_id != config.id {
        return Err(CoreError::Other("Sent filing account mismatch".into()));
    }
    let sent = ctx
        .db
        .read({
            let account_id = config.id;
            move |conn| repo::folders::by_role(conn, account_id, roles::SENT)
        })
        .await?
        .ok_or_else(|| CoreError::Imap("Sent mailbox has not been discovered yet".into()))?;
    let selected = imap::select(session, &sent.path).await?;
    let existing_uid = if row.folder_id == Some(sent.id)
        && sent.uidvalidity.is_some()
        && sent.uidvalidity == selected.uid_validity
    {
        row.uid
            .and_then(|uid| u32::try_from(uid).ok())
            .filter(|uid| *uid > 0)
    } else {
        None
    };
    let rfc_message_id = row
        .message_id
        .as_deref()
        .ok_or_else(|| CoreError::NotFound("outgoing Message-ID".into()))?;
    let sent_uid = match existing_uid {
        Some(uid) => Some(uid),
        None => find_sent_uid(session, rfc_message_id).await?,
    };
    if let Some(uid) = sent_uid {
        return finish_sent_filing(ctx, action, sent.id, selected.uid_validity, Some(uid)).await;
    }
    // Persisted *before* the command. A timeout, disconnect, or process crash
    // cannot prove APPEND failed. Reconcile by identity on subsequent attempts,
    // and leave the local outgoing snapshot intact if the result stays unknown.
    if action.payload["sentAppendStarted"].as_bool() == Some(true) {
        return Err(CoreError::Other(
            "Sent copy could not be confirmed after an interrupted APPEND; automatic append suppressed to avoid duplicates".into(),
        ));
    }
    let raw_path = row
        .raw_path
        .ok_or_else(|| CoreError::NotFound("outgoing MIME snapshot".into()))?;
    let mail_root = tokio::fs::canonicalize(ctx.paths.mail_dir(config.id)).await?;
    let canonical = tokio::fs::canonicalize(&raw_path).await?;
    if !canonical.starts_with(&mail_root) {
        return Err(CoreError::Other(
            "refusing to file a Sent copy from outside the mail store".into(),
        ));
    }
    let raw = crate::file_io::read(
        &canonical,
        crate::MAX_CACHED_MESSAGE_BYTES,
        "outgoing MIME snapshot",
    )
    .await?;
    let action_id = action.id;
    let mut payload = action.payload.clone();
    payload["sentAppendStarted"] = true.into();
    ctx.db
        .write(move |conn| {
            conn.execute(
                "UPDATE pending_actions SET payload=?2 WHERE id=?1",
                rusqlite::params![action_id, serde_json::to_string(&payload)?],
            )?;
            Ok(())
        })
        .await?;
    let receipt = imap::append_with_receipt(session, &sent.path, &raw, true).await?;
    // Tagged OK is enough to complete filing even without UIDPLUS. Header sync
    // will attach the UID later; no fallible post-APPEND SELECT/search is needed.
    let (validity, uid) = receipt
        .map(|r| (Some(i64::from(r.uid_validity)), Some(r.uid)))
        .unwrap_or((selected.uid_validity, None));
    finish_sent_filing(ctx, action, sent.id, validity, uid).await
}

async fn find_sent_uid(session: &mut Session, message_id: &str) -> Result<Option<u32>> {
    let bare = message_id.trim_matches(['<', '>']);
    if bare.is_empty() {
        return Err(CoreError::NotFound("outgoing Message-ID".into()));
    }
    let mut candidates =
        imap::uid_search_header(session, "Message-ID", &format!("<{bare}>")).await?;
    candidates
        .extend(imap::uid_search_header(session, crate::mime::SUBMISSION_ID_HEADER, bare).await?);
    candidates.sort_unstable();
    candidates.dedup();
    let mut found = None;
    // SEARCH HEADER is a substring search: verify complete header identities
    // before assigning a UID. Chunk to keep the FETCH command bounded.
    for chunk in candidates.chunks(100) {
        let uids = chunk
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(",");
        for header in imap::fetch_headers(session, &uids).await? {
            let parsed = crate::mime::parse_header_block(&header.header_bytes)?;
            if parsed.message_id.as_deref() == Some(bare)
                || parsed.submission_id.as_deref() == Some(bare)
            {
                if found.is_some() {
                    return Err(CoreError::Other(
                        "multiple Sent copies match this submission; automatic append suppressed"
                            .into(),
                    ));
                }
                found = Some(header.uid);
            }
        }
    }
    Ok(found)
}

async fn finish_sent_filing(
    ctx: &SyncCtx,
    action: &repo::actions::PendingAction,
    folder_id: i64,
    uid_validity: Option<i64>,
    uid: Option<u32>,
) -> Result<()> {
    let action = action.clone();
    ctx.db
        .write(move |conn| finish_sent_filing_in_db(conn, &action, folder_id, uid_validity, uid))
        .await
}

fn finish_sent_filing_in_db(
    conn: &mut rusqlite::Connection,
    action: &repo::actions::PendingAction,
    folder_id: i64,
    uid_validity: Option<i64>,
    uid: Option<u32>,
) -> Result<()> {
    let tx = conn.transaction()?;
    // A user may have moved the local row while filing was in flight. Never
    // undo that newer intent or bind a UID from another mailbox generation.
    if let (Some(message_id), Some(validity), Some(uid)) = (action.message_id, uid_validity, uid) {
        tx.execute(
            "UPDATE messages SET uid=?3 WHERE id=?1 AND folder_id=?2 AND uid IS NULL
             AND account_id=?4 AND EXISTS(SELECT 1 FROM folders WHERE id=?2 AND uidvalidity=?5)",
            rusqlite::params![
                message_id,
                folder_id,
                i64::from(uid),
                action.account_id,
                validity
            ],
        )?;
    }
    repo::actions::set_state(&tx, action.id, "done", None)?;
    tx.commit()?;
    Ok(())
}

async fn send_action(
    ctx: &SyncCtx,
    config: &AccountConfig,
    action: &repo::actions::PendingAction,
) -> Result<()> {
    let Some(draft_id) = action.payload["draftId"].as_i64() else {
        return Err(CoreError::Other("send action without draftId".into()));
    };
    tracing::info!(account_id = config.id, draft_id, "smtp send: starting");

    let (detail, references) = ctx
        .db
        .read(move |conn| {
            let detail = repo::messages::detail(conn, draft_id)?;
            let irt: Option<i64> = conn
                .query_row(
                    "SELECT in_reply_to_message_id FROM drafts_meta WHERE message_id = ?1",
                    rusqlite::params![draft_id],
                    |r| r.get(0),
                )
                .unwrap_or(None);
            let mut refs: Vec<String> = Vec::new();
            let mut in_reply_to: Option<String> = None;
            if let Some(parent_id) = irt {
                let mut stmt =
                    conn.prepare("SELECT ref_message_id FROM message_refs WHERE message_id = ?1")?;
                refs = stmt
                    .query_map(rusqlite::params![parent_id], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                if let Some(parent) = repo::messages::get_row(conn, parent_id)?
                    && let Some(pmid) = parent.message_id
                {
                    refs.push(pmid.clone());
                    in_reply_to = Some(pmid);
                }
            }
            Ok((detail, (refs, in_reply_to)))
        })
        .await?;

    let (refs, in_reply_to) = references;
    let account_id = config.id;
    let identity = ctx
        .db
        .read({
            let requested = detail.from.email.clone();
            move |conn| {
                repo::sender_identities::get_verified(conn, account_id, &requested)?.ok_or_else(
                    || {
                        CoreError::Other(format!(
                            "{requested} is not an authorized sender identity for this account"
                        ))
                    },
                )
            }
        })
        .await?;
    let from = identity.address();
    let domain = from
        .email
        .split('@')
        .nth(1)
        .unwrap_or("localhost")
        .to_string();

    let (bcc, stored_message_id) = ctx
        .db
        .read(move |conn| {
            let (json, message_id): (String, Option<String>) = conn.query_row(
                "SELECT bcc_json, message_id FROM messages WHERE id = ?1",
                rusqlite::params![draft_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            Ok((serde_json::from_str::<Vec<Address>>(&json)?, message_id))
        })
        .await?;

    let att_rows: Vec<(String, String)> = ctx
        .db
        .read(move |conn| {
            let mut stmt = conn
                .prepare("SELECT file_path, filename FROM draft_attachments WHERE draft_id = ?1")?;
            let rows = stmt
                .query_map(rusqlite::params![draft_id], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await?;
    // Defense in depth: only ever read files inside our own staging root, so a
    // stale/crafted `draft_attachments` row can't turn dispatch into an
    // arbitrary local-file read. `save_draft` copies picked files in here.
    let staging_root = tokio::fs::canonicalize(ctx.paths.draft_attachments_dir())
        .await
        .ok();
    let mut attachments = Vec::new();
    let mut attachment_bytes = 0usize;
    for (path, filename) in att_rows {
        let canon = tokio::fs::canonicalize(&path)
            .await
            .map_err(|e| CoreError::Other(format!("attachment {filename}: {e}")))?;
        let within = staging_root
            .as_ref()
            .is_some_and(|root| canon.starts_with(root));
        if !within {
            return Err(CoreError::Other(format!(
                "attachment {filename}: refusing to read file outside the staging area"
            )));
        }
        let remaining = crate::MAX_DRAFT_ATTACHMENT_BYTES.saturating_sub(attachment_bytes);
        let bytes = crate::file_io::read(&canon, remaining, "draft attachment").await?;
        attachment_bytes += bytes.len();
        let mime_type = mime_guess_from_name(&filename);
        attachments.push(crate::mime::OutgoingAttachment {
            filename,
            mime_type,
            bytes,
        });
    }

    let out = crate::mime::OutgoingMessage {
        from: from.clone(),
        reply_to: identity.reply_to(),
        to: &detail.to,
        cc: &detail.cc,
        bcc: &bcc,
        subject: &detail.subject,
        body_text: detail.text_body.as_deref().unwrap_or(""),
        body_html: detail.html_body.as_deref(),
        in_reply_to: in_reply_to.as_deref(),
        references: &refs,
        message_id: stored_message_id.as_deref(),
        message_id_domain: &domain,
        attachments,
    };
    let attachment_count = out.attachments.len();
    let security_recipients = out
        .to
        .iter()
        .chain(out.cc)
        .chain(out.bcc)
        .cloned()
        .collect();
    let (msg_id, raw) = crate::mime::build_message_owned(out)?;
    let raw =
        crate::mail_security::protect_draft(&ctx.db, config.id, draft_id, raw, security_recipients)
            .await?;
    // Persist the exact protected MIME before SMTP. It gives retries a stable
    // Message-ID and lets Sent filing run independently after delivery without
    // retaining composer attachments or rebuilding encrypted content.
    let msg_id_bare = msg_id.trim_matches(['<', '>']).to_string();
    let mail_dir = ctx.paths.mail_dir(config.id);
    let raw_path = mail_dir.join(format!("{draft_id}.outgoing.eml"));
    crate::file_io::write_atomic(&raw_path, &raw, "outgoing MIME snapshot").await?;
    let raw_path_string = raw_path.to_string_lossy().into_owned();
    let stable_id = msg_id_bare.clone();
    let snapshot_path = raw_path_string.clone();
    ctx.db
        .write(move |conn| {
            conn.execute(
                "UPDATE messages SET message_id = ?2, raw_path = ?3 WHERE id = ?1",
                rusqlite::params![draft_id, stable_id, snapshot_path],
            )?;
            Ok(())
        })
        .await?;
    tracing::debug!(
        account_id = config.id,
        draft_id,
        message_id = %msg_id,
        bytes = raw.len(),
        attachments = attachment_count,
        to = detail.to.len(),
        cc = detail.cc.len(),
        bcc = bcc.len(),
        "smtp send: message built",
    );

    let auth = match config.auth_kind {
        AuthKind::Password => smtp::SmtpAuth::Password(
            credentials::load_async(ctx.credentials.clone(), config.id, Slot::Password).await?,
        ),
        AuthKind::Oauth2 => {
            smtp::SmtpAuth::XOAuth2(ctx.tokens.access_token(config.id, config.provider).await?)
        }
    };

    let recipients: Vec<String> = detail
        .to
        .iter()
        .chain(detail.cc.iter())
        .chain(bcc.iter())
        .map(|a| a.email.clone())
        .collect();
    if recipients.is_empty() {
        return Err(CoreError::Smtp("no recipients".into()));
    }

    tracing::info!(
        account_id = config.id,
        host = %config.smtp_host,
        port = config.smtp_port,
        recipients = recipients.len(),
        "smtp send: dispatching",
    );
    let submitted = match smtp::send_raw(config, &auth, &from.email, &recipients, &raw).await {
        Err(CoreError::Auth(_)) if config.auth_kind == AuthKind::Oauth2 => {
            // AUTH rejection happens before MAIL FROM/DATA, so it is safe to
            // invalidate an unexpectedly stale provider token and retry the
            // connection once without risking a duplicate delivery.
            tracing::info!(
                account_id = config.id,
                provider = ?config.provider,
                "SMTP rejected cached OAuth token; refreshing once"
            );
            ctx.tokens.invalidate(config.id).await;
            let retry_auth =
                smtp::SmtpAuth::XOAuth2(ctx.tokens.access_token(config.id, config.provider).await?);
            smtp::send_raw(config, &retry_auth, &from.email, &recipients, &raw).await
        }
        result => result,
    };
    // Plain SMTP cannot list permitted From addresses, so the submission
    // outcome is the evidence shown next to the identity in settings.
    let (delivery, error) = match submitted {
        Ok(()) => (SenderDeliveryStatus::Confirmed, None),
        // A temporary refusal (4xx) is retried and says nothing lasting about
        // whether the server accepts this sender.
        Err(CoreError::Smtp(message))
            if crate::sender_identities::is_sender_rejection(&message)
                && has_permanent_smtp_reply(&message) =>
        {
            let error = CoreError::Smtp(format!(
                "the server refused to send as {}; ask your provider to allow this address or choose another sender ({message})",
                from.email
            ));
            (SenderDeliveryStatus::Rejected, Some((message, error)))
        }
        Err(error) => return Err(error),
    };
    let sender_email = from.email.clone();
    let server_reply = error.as_ref().map(|(message, _)| message.clone());
    // Best effort: after an accepted submission, failing here must not make
    // the durable action retry and deliver the message twice.
    if let Err(record_error) = ctx
        .db
        .write(move |conn| {
            repo::sender_identities::record_delivery(
                conn,
                account_id,
                &sender_email,
                delivery,
                server_reply.as_deref(),
            )
        })
        .await
    {
        tracing::warn!(account_id, error = %record_error, "could not record sender delivery outcome");
    }
    if let Some((_, error)) = error {
        return Err(error);
    }
    tracing::info!(account_id = config.id, "smtp send: accepted by server");

    let sent_folder_id = ctx
        .db
        .read({
            let account_id = config.id;
            move |conn| Ok(repo::folders::by_role(conn, account_id, roles::SENT)?.map(|f| f.id))
        })
        .await?;
    // mail-parser strips angle brackets from Message-IDs; store the same form
    // so the Sent-folder sync dedupes against this row instead of duplicating.
    let sent_at = now_ms();
    let send_action_id = action.id;
    let (thread_id, staged_paths) = ctx
        .db
        .write(move |conn| {
            let tx = conn.transaction()?;
            // The dedicated SMTP worker outlives settings edits. Read the
            // latest policy at completion instead of its startup snapshot.
            let current = repo::accounts::get_config(&tx, account_id)?
                .ok_or_else(|| CoreError::NotFound(format!("account {account_id}")))?;
            let should_append_sent = match current.mail_protocol {
                MailProtocol::Imap => current
                    .settings
                    .sent_copy_policy
                    .should_append(current.provider),
                // The local Sent folder is the only one a POP3 account has.
                MailProtocol::Pop3 => false,
                // These submit through their own APIs, which file sent mail.
                MailProtocol::Gmail | MailProtocol::Jmap => false,
            };

            tx.execute(
                "UPDATE messages SET is_draft = 0, is_outgoing = 1, is_read = 1,
                        message_id = ?2, raw_path = ?3,
                        folder_id = COALESCE(?4, folder_id),
                        uid = CASE WHEN folder_id = ?4 THEN uid ELSE NULL END, date = ?5
                 WHERE id = ?1",
                rusqlite::params![
                    draft_id,
                    msg_id_bare,
                    raw_path_string,
                    sent_folder_id,
                    sent_at
                ],
            )?;
            tx.execute(
                "DELETE FROM drafts_meta WHERE message_id = ?1",
                rusqlite::params![draft_id],
            )?;
            let staged_paths = repo::messages::take_draft_attachment_paths(&tx, draft_id)?;
            let tid: Option<i64> =
                repo::messages::get_row(&tx, draft_id)?.and_then(|r| r.thread_id);
            if let Some(tid) = tid {
                repo::threads::recompute(&tx, tid)?;
            }
            repo::search::index_message(&tx, draft_id)?;
            repo::contacts::record_sent_recipients(&tx, account_id, draft_id, sent_at)?;
            // SMTP acceptance is the user-visible completion boundary. Remote
            // Sent filing is its own durable, retryable IMAP action and may
            // safely finish after the composer closes.
            if should_append_sent {
                repo::actions::enqueue(
                    &tx,
                    account_id,
                    "append_sent",
                    Some(draft_id),
                    tid,
                    &serde_json::json!({}),
                    None,
                )?;
            }
            // Commit delivery and its follow-up atomically. This narrows the
            // unavoidable SMTP/SQLite crash window and prevents a restart from
            // resubmitting a message after local completion was recorded.
            repo::actions::set_state(&tx, send_action_id, "done", None)?;
            tx.commit()?;
            Ok((tid, staged_paths))
        })
        .await?;

    for path in staged_paths {
        crate::remove_staged_attachment(&ctx.paths.draft_attachments_dir(), &path).await;
    }

    if let Some(tid) = thread_id {
        ctx.bus.emit(CoreEvent::MailUpdated {
            thread_ids: vec![tid],
        });
    }
    tracing::info!(
        account_id = config.id,
        draft_id,
        thread_id = ?thread_id,
        "smtp send: complete",
    );
    Ok(())
}

/// Tiny extension-based MIME guess for outgoing attachments.
pub(crate) fn mime_guess_from_name(name: &str) -> String {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "txt" | "log" | "md" => "text/plain",
        "csv" => "text/csv",
        "json" => "application/json",
        "zip" => "application/zip",
        "gz" => "application/gzip",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "mp4" => "video/mp4",
        "mp3" => "audio/mpeg",
        "eml" => "message/rfc822",
        "ics" => "text/calendar",
        _ => "application/octet-stream",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timed_out_action_forces_a_fresh_session() {
        assert!(is_connection_failure("UID MOVE timed out after 30s"));
        assert!(is_connection_failure("connection closed"));
        assert!(!is_connection_failure("temporary mailbox quota"));
    }

    #[test]
    fn command_rejections_are_permanent_but_transport_errors_retry() {
        assert!(is_permanent(&CoreError::Imap(
            "NO [NOPERM] permission denied".into()
        )));
        assert!(is_permanent(&CoreError::Imap(
            "bad: invalid command".into()
        )));
        assert!(is_permanent(&CoreError::Smtp(
            "554 transaction failed".into()
        )));
        assert!(is_permanent(&CoreError::NotFound("source mailbox".into())));
        assert!(!is_permanent(&CoreError::Imap(
            "connection timed out".into()
        )));
        assert!(!is_permanent(&CoreError::Smtp(
            "451 temporary failure".into()
        )));
    }

    #[test]
    fn smtp_permanence_reads_reply_codes_not_ports() {
        for permanent in [
            "smtp.example.com:587: permanent error (553): 5.7.1 sender rejected",
            "SMTP STARTTLS failed: 554 5.7.0 TLS not available",
            "the server refused to send as a@example.com (smtp.example.com:465: permanent error (550): denied)",
            "550-5.7.1 multi-line rejection",
        ] {
            assert!(
                is_permanent(&CoreError::Smtp(permanent.into())),
                "{permanent}"
            );
        }
        for transient in [
            "smtp.example.com:587: Connection error: timed out",
            "smtp.example.com:587: transient error (451): 4.7.1 try again later",
            "smtp.example.com:587: transient error (450): 4.1.8 Sender address rejected: Domain not found",
            "smtp.example.com:5587: network error",
        ] {
            assert!(
                !is_permanent(&CoreError::Smtp(transient.into())),
                "{transient}"
            );
        }
    }

    fn optimistic_move(
        conn: &rusqlite::Connection,
        message_id: i64,
        thread_id: i64,
        target_folder: i64,
    ) -> repo::actions::PendingAction {
        let source_uid = repo::messages::get_row(conn, message_id)
            .unwrap()
            .unwrap()
            .uid;
        let action_id = repo::actions::enqueue(
            conn,
            1,
            "move",
            Some(message_id),
            Some(thread_id),
            &serde_json::json!({
                "srcFolderId": 1,
                "srcUid": source_uid,
                "targetFolderId": target_folder,
            }),
            None,
        )
        .unwrap();
        repo::messages::set_uid_and_folder(conn, message_id, target_folder, None).unwrap();
        repo::actions::get(conn, action_id).unwrap().unwrap()
    }

    #[test]
    fn terminal_move_failure_restores_the_remote_source_mapping() {
        let mut conn = crate::db::testutil::conn();
        crate::db::testutil::seed_account(&conn);
        conn.execute(
            "INSERT INTO folders (id, account_id, path, role)
             VALUES (2, 1, 'Archive', 'archive')",
            [],
        )
        .unwrap();
        let (thread_id, message_id) =
            crate::db::testutil::seed_message(&conn, "sender@test.dev", "move", false);
        let source_uid = repo::messages::get_row(&conn, message_id)
            .unwrap()
            .unwrap()
            .uid;
        let action = optimistic_move(&conn, message_id, thread_id, 2);

        assert_eq!(
            mark_failed_and_rollback(&mut conn, &action, "server rejected move").unwrap(),
            Some(thread_id)
        );
        let restored = repo::messages::get_row(&conn, message_id).unwrap().unwrap();
        assert_eq!(restored.folder_id, Some(1));
        assert_eq!(restored.uid, source_uid);
        assert_eq!(
            repo::actions::get(&conn, action.id).unwrap().unwrap().state,
            "failed"
        );
    }

    #[test]
    fn older_failure_does_not_overwrite_a_completed_newer_move() {
        let mut conn = crate::db::testutil::conn();
        crate::db::testutil::seed_account(&conn);
        conn.execute_batch(
            "INSERT INTO folders (id, account_id, path, role)
             VALUES (2, 1, 'Archive', 'archive');
             INSERT INTO folders (id, account_id, path, role)
             VALUES (3, 1, 'Later', '');",
        )
        .unwrap();
        let (thread_id, message_id) =
            crate::db::testutil::seed_message(&conn, "sender@test.dev", "move race", false);
        let first = optimistic_move(&conn, message_id, thread_id, 2);
        let newer = repo::actions::enqueue(
            &conn,
            1,
            "move",
            Some(message_id),
            Some(thread_id),
            &serde_json::json!({
                "srcFolderId": 2,
                "srcUid": 84,
                "targetFolderId": 3,
            }),
            None,
        )
        .unwrap();
        repo::actions::set_state(&conn, newer, "done", None).unwrap();
        let newest = repo::actions::enqueue(
            &conn,
            1,
            "move",
            Some(message_id),
            Some(thread_id),
            &serde_json::json!({
                "srcFolderId": 3,
                "srcUid": 126,
                "targetFolderId": 2,
            }),
            None,
        )
        .unwrap();
        repo::actions::set_state(&conn, newest, "done", None).unwrap();

        assert_eq!(
            mark_failed_and_rollback(&mut conn, &first, "server rejected move").unwrap(),
            None
        );
        assert_eq!(
            repo::messages::get_row(&conn, message_id)
                .unwrap()
                .unwrap()
                .folder_id,
            Some(2)
        );
    }
}

#[cfg(test)]
pub(crate) mod sent_tests;

#[cfg(test)]
mod move_tests;
