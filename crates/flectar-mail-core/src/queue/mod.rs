//! Pending-action executor. Local mutations already happened optimistically
//! when the action was enqueued (see Core::perform_action); this module
//! replays the intent against the server, in order, when connected.

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

pub async fn execute_due(
    ctx: &SyncCtx,
    config: &AccountConfig,
    session: &mut Session,
) -> Result<ActionSliceResult> {
    let account_id = config.id;
    let due = ctx
        .db
        .read(move |conn| {
            repo::actions::due_except_send(conn, account_id, now_ms(), MAX_ACTIONS_PER_SLICE)
        })
        .await?;
    let result = execute_actions(ctx, config, Some(session), due).await?;
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
    let result = execute_actions(ctx, config, None, due).await?;
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
    mut session: Option<&mut Session>,
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

        let outcome = execute_one(ctx, config, session.as_deref_mut(), &action).await;
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
        CoreError::Smtp(message) => message.split(|c: char| !c.is_ascii_digit()).any(|token| {
            token.len() == 3
                && token.starts_with('5')
                && token.bytes().all(|byte| byte.is_ascii_digit())
        }),
        _ => false,
    }
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
    let multi_mailbox: bool = tx.query_row(
        "SELECT provider='gmail' OR mail_protocol='jmap' FROM accounts WHERE id=?1",
        rusqlite::params![failed.account_id],
        |row| row.get(0),
    )?;
    if multi_mailbox {
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
    session: Option<&mut Session>,
    action: &repo::actions::PendingAction,
) -> Result<()> {
    if action.kind == "send" {
        return send_action(ctx, config, action).await;
    }
    let session = session.ok_or_else(|| {
        CoreError::Other(format!(
            "{} action was routed without an IMAP session",
            action.kind
        ))
    })?;
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
            imap::select(session, &folder.imap_name).await?;
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
            let selected = imap::select(session, &folder.imap_name).await?;
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
    let (Some(folder_id), Some(uid)) = (row.folder_id, row.uid) else {
        return Ok(None);
    };
    let folder = ctx
        .db
        .read(move |conn| repo::folders::get(conn, folder_id))
        .await?;
    let Some(folder) = folder else {
        return Ok(None);
    };
    imap::select(session, &folder.imap_name).await?;
    Ok(Some((folder, uid as u32)))
}

/// For moves the local row already points at the *target* folder; the remote
/// source location is stored in the payload.
async fn resolve_source(
    ctx: &SyncCtx,
    session: &mut Session,
    action: &repo::actions::PendingAction,
) -> Result<(repo::folders::Folder, u32)> {
    let fid = action.payload["srcFolderId"]
        .as_i64()
        .ok_or_else(|| CoreError::NotFound("source mailbox for queued move".into()))?;
    let uid = action.payload["srcUid"]
        .as_i64()
        .and_then(|uid| u32::try_from(uid).ok())
        .filter(|uid| *uid > 0)
        .ok_or_else(|| CoreError::NotFound("valid source UID for queued move".into()))?;
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
    imap::select(session, &folder.imap_name).await?;
    Ok((folder, uid))
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
    let (_src, uid) = resolve_source(ctx, session, action).await?;
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

    imap::uid_move(session, uid, &target.imap_name).await?;

    // The message's new UID in the target is unknown (COPYUID not parsed in
    // v1); clear it so the next target-folder sync re-links by Message-ID.
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
    let (raw_path, rfc_message_id) = ctx
        .db
        .read(move |conn| {
            let row = repo::messages::get_row(conn, message_id)?
                .ok_or_else(|| CoreError::NotFound(format!("message {message_id}")))?;
            Ok((
                row.raw_path
                    .ok_or_else(|| CoreError::NotFound("outgoing MIME snapshot".into()))?,
                row.message_id
                    .ok_or_else(|| CoreError::NotFound("outgoing Message-ID".into()))?,
            ))
        })
        .await?;
    let sent = ctx
        .db
        .read({
            let account_id = config.id;
            move |conn| repo::folders::by_role(conn, account_id, roles::SENT)
        })
        .await?
        .ok_or_else(|| CoreError::Imap("Sent mailbox has not been discovered yet".into()))?;

    // The action payload is durable database state, but still treat its file
    // reference as untrusted. Outgoing snapshots must stay inside this
    // account's app-managed mail directory.
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

    imap::select(session, &sent.imap_name).await?;
    let header_value = format!("<{}>", rfc_message_id.trim_matches(['<', '>']));
    if imap::uid_search_header(session, "Message-ID", &header_value)
        .await?
        .is_empty()
    {
        imap::append(session, &sent.imap_name, &raw, true).await?;
        tracing::debug!(
            account_id = config.id,
            message_id,
            folder = %sent.imap_name,
            "smtp send: appended durable copy to Sent",
        );
    } else {
        tracing::debug!(
            account_id = config.id,
            message_id,
            folder = %sent.imap_name,
            "smtp send: Sent copy already exists",
        );
    }
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
    let from = ctx
        .db
        .read({
            let requested = detail.from.email.clone();
            move |conn| {
                repo::sender_identities::get_verified(conn, account_id, &requested)?
                    .map(|identity| identity.address())
                    .ok_or_else(|| {
                        CoreError::Other(format!(
                            "{requested} is not an authorized sender identity for this account"
                        ))
                    })
            }
        })
        .await?;
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
    let (msg_id, raw) = crate::mime::build_message(&out)?;
    let raw = crate::mail_security::protect_draft(
        &ctx.db,
        config.id,
        draft_id,
        raw,
        out.to
            .iter()
            .chain(out.cc)
            .chain(out.bcc)
            .cloned()
            .collect(),
    )
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
    ctx.db
        .write(move |conn| {
            conn.execute(
                "UPDATE messages SET message_id = ?2 WHERE id = ?1",
                rusqlite::params![draft_id, stable_id],
            )?;
            Ok(())
        })
        .await?;
    tracing::debug!(
        account_id = config.id,
        draft_id,
        message_id = %msg_id,
        bytes = raw.len(),
        attachments = out.attachments.len(),
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
    match smtp::send_raw(config, &auth, &from.email, &recipients, &raw).await {
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
            smtp::send_raw(config, &retry_auth, &from.email, &recipients, &raw).await?;
        }
        Err(error) => return Err(error),
        Ok(()) => {}
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
    let should_append_sent = config.provider != Provider::Gmail;
    let (thread_id, staged_paths) = ctx
        .db
        .write(move |conn| {
            let tx = conn.transaction()?;
            tx.execute(
                "UPDATE messages SET is_draft = 0, is_outgoing = 1, is_read = 1,
                        message_id = ?2, raw_path = ?3,
                        folder_id = COALESCE(?4, folder_id), uid = NULL, date = ?5
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
            "INSERT INTO folders (id, account_id, imap_name, role)
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
            "INSERT INTO folders (id, account_id, imap_name, role)
             VALUES (2, 1, 'Archive', 'archive');
             INSERT INTO folders (id, account_id, imap_name, role)
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
