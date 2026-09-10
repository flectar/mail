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
    let started = Instant::now();
    let due = ctx
        .db
        .read(move |conn| repo::actions::due(conn, account_id, now_ms(), MAX_ACTIONS_PER_SLICE))
        .await?;
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

        let outcome = execute_one(ctx, config, session, &action).await;
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
                // A queued send that can't authenticate would otherwise sit in the
                // outbox indefinitely with the composer already closed: the message
                // just vanishes with no warning (only a needs-reauth dot buried in
                // Settings). Tell the UI so it can surface that the mail wasn't sent
                // and point the user at re-authentication. Other action kinds stay
                // quiet; the account's needs_reauth state already covers them.
                if action.kind == "send" {
                    ctx.bus.emit(CoreEvent::ActionState {
                        action_id,
                        state: "paused".into(),
                        error: Some(e.to_string()),
                    });
                }
                let msg = "authentication required".to_string();
                ctx.db
                    .write(move |conn| {
                        repo::actions::bump_attempt(conn, action_id, now_ms() + 60_000, &msg)
                    })
                    .await?;
                return Err(CoreError::NeedsReauth);
            }
            Err(e) => {
                let msg = e.to_string();
                let attempts = action.attempts + 1;
                tracing::warn!(
                    account_id, action_id, kind = %action.kind, attempts, error = %msg,
                    "action attempt failed",
                );
                if attempts >= MAX_ATTEMPTS || is_permanent(&msg) {
                    let m = msg.clone();
                    ctx.db
                        .write(move |conn| {
                            repo::actions::set_state(conn, action_id, "failed", Some(&m))
                        })
                        .await?;
                    ctx.bus.emit(CoreEvent::ActionState {
                        action_id,
                        state: "failed".into(),
                        error: Some(msg),
                    });
                } else {
                    // Exponential backoff with jitter.
                    let delay = (1 << attempts.min(8)) * 1000 + (action_id % 997);
                    let m = msg.clone();
                    ctx.db
                        .write(move |conn| {
                            repo::actions::bump_attempt(conn, action_id, now_ms() + delay, &m)
                        })
                        .await?;
                    // Connection-level errors: bail out, actor reconnects.
                    if is_connection_failure(&msg) {
                        return Err(e);
                    }
                }
            }
        }
    }

    let due_remaining = ctx
        .db
        .read(move |conn| repo::actions::has_due(conn, account_id, now_ms()))
        .await?;
    Ok(ActionSliceResult {
        processed,
        due_remaining,
    })
}

fn is_permanent(msg: &str) -> bool {
    // IMAP tagged NO responses and SMTP 5xx are not going to succeed on retry.
    msg.contains("NO ") || msg.contains("550") || msg.contains("553") || msg.contains("bad ")
}

fn is_connection_failure(msg: &str) -> bool {
    let msg = msg.to_ascii_lowercase();
    msg.contains("connect")
        || msg.contains("broken")
        || msg.contains("closed")
        || msg.contains("timed out")
        || msg.contains("unexpected eof")
}

async fn execute_one(
    ctx: &SyncCtx,
    config: &AccountConfig,
    session: &mut Session,
    action: &repo::actions::PendingAction,
) -> Result<()> {
    match action.kind.as_str() {
        "mark_read" => flag_action(ctx, session, action, "\\Seen", true).await,
        "mark_unread" => flag_action(ctx, session, action, "\\Seen", false).await,
        "star" => flag_action(ctx, session, action, "\\Flagged", true).await,
        "unstar" => flag_action(ctx, session, action, "\\Flagged", false).await,
        "archive" | "unarchive" | "trash" | "spam" | "not_spam" | "move" => {
            move_action(ctx, config, session, action).await
        }
        "add_label" => keyword_action(ctx, session, action, true).await,
        "remove_label" => keyword_action(ctx, session, action, false).await,
        "send" => send_action(ctx, config, session, action).await,
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
) -> Result<Option<(repo::folders::Folder, u32)>> {
    let src_folder_id = action.payload["srcFolderId"].as_i64();
    let src_uid = action.payload["srcUid"].as_i64();
    let (Some(fid), Some(uid)) = (src_folder_id, src_uid) else {
        return Ok(None);
    };
    let folder = ctx
        .db
        .read(move |conn| repo::folders::get(conn, fid))
        .await?;
    let Some(folder) = folder else {
        return Ok(None);
    };
    imap::select(session, &folder.imap_name).await?;
    Ok(Some((folder, uid as u32)))
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
    let Some(message_id) = action.message_id else {
        return Ok(());
    };
    let Some((_src, uid)) = resolve_source(ctx, session, action).await? else {
        return Ok(());
    };
    let target_folder_id = action.payload["targetFolderId"].as_i64();
    let Some(tfid) = target_folder_id else {
        return Ok(());
    };
    let target = ctx
        .db
        .read(move |conn| repo::folders::get(conn, tfid))
        .await?
        .ok_or_else(|| CoreError::NotFound("target folder".into()))?;

    imap::uid_move(session, uid, &target.imap_name).await?;

    // The message's new UID in the target is unknown (COPYUID not parsed in
    // v1); clear it so the next target-folder sync re-links by Message-ID.
    let _ = config;
    ctx.db
        .write(move |conn| repo::messages::set_uid_and_folder(conn, message_id, tfid, None))
        .await?;
    Ok(())
}

async fn send_action(
    ctx: &SyncCtx,
    config: &AccountConfig,
    session: &mut Session,
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
    let from = Address {
        name: config.display_name.clone(),
        email: config.email.clone(),
    };
    let domain = config
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
    // Persist before SMTP. If the relay accepts the message but the response
    // is lost, action recovery rebuilds the exact same Message-ID instead of
    // producing an avoidable duplicate with a fresh identity.
    let msg_id_bare = msg_id.trim_matches(['<', '>']).to_string();
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
    match smtp::send_raw(config, &auth, &config.email, &recipients, &raw).await {
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
            smtp::send_raw(config, &retry_auth, &config.email, &recipients, &raw).await?;
        }
        Err(error) => return Err(error),
        Ok(()) => {}
    }
    tracing::info!(account_id = config.id, "smtp send: accepted by server");

    // Gmail stores SMTP submissions in Sent automatically.
    if config.provider != Provider::Gmail {
        let sent = ctx
            .db
            .read({
                let account_id = config.id;
                move |conn| repo::folders::by_role(conn, account_id, roles::SENT)
            })
            .await?;
        if let Some(sent) = sent {
            match imap::append(session, &sent.imap_name, &raw, true).await {
                Ok(_) => tracing::debug!(
                    account_id = config.id,
                    folder = %sent.imap_name,
                    "smtp send: appended copy to Sent",
                ),
                Err(e) => tracing::warn!("append to sent failed (message was sent): {e}"),
            }
        }
    }

    let sent_folder_id = ctx
        .db
        .read({
            let account_id = config.id;
            move |conn| Ok(repo::folders::by_role(conn, account_id, roles::SENT)?.map(|f| f.id))
        })
        .await?;
    // mail-parser strips angle brackets from Message-IDs; store the same form
    // so the Sent-folder sync dedupes against this row instead of duplicating.
    let (thread_id, staged_paths) = ctx
        .db
        .write(move |conn| {
            let tx = conn.transaction()?;
            tx.execute(
                "UPDATE messages SET is_draft = 0, is_outgoing = 1, is_read = 1,
                        message_id = ?2, folder_id = COALESCE(?3, folder_id), uid = NULL, date = ?4
                 WHERE id = ?1",
                rusqlite::params![draft_id, msg_id_bare, sent_folder_id, now_ms()],
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
}
