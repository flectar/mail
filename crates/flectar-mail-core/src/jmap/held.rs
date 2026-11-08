//! Scheduled mail a JMAP server holds and releases at its time (RFC 8621
//! §7, with FUTURERELEASE from RFC 4865), so it goes out with every device
//! off. As in Fastmail's own Scheduled Send, the message waits in the
//! account's Scheduled mailbox, goes to Sent once released, and back to
//! Drafts when cancelled. A server holds a message only into a Scheduled
//! mailbox the account already has, and only as long as its
//! `maxDelayedSend` allows; otherwise the user's devices send it, as for
//! other accounts.
//!
//! Every device of the account shows the message waiting: a pending
//! submission of a message in the Scheduled mailbox becomes a held send on
//! each of them, and any of them cancels it on the server.

use super::client::{self, ConnectedClient};
use super::sync::{
    ensure_role_mailbox, ensure_submitted_email_state, finish_sent_locally, map_submission_error,
    prepared_email, send_action, sending_identity,
};
use crate::db::repo;
use crate::error::{CoreError, Result};
use crate::events::CoreEvent;
use crate::models::{AccountConfig, Address as MailAddress, now_ms, roles};
use crate::sync::engine::SyncCtx;
use jmap_client::core::response::{EmailSubmissionGetResponse, EmailSubmissionSetResponse};
use jmap_client::core::set::{SetErrorType, SetObject};
use jmap_client::email_submission::query::Filter;
use jmap_client::email_submission::{Address, UndoStatus};
use jmap_client::mailbox::Role;
use rusqlite::{OptionalExtension, params};
use std::collections::HashMap;

/// A wait shorter than this stays with the device: the server's hold is for
/// mail scheduled ahead, not for the seconds of undo send.
pub(crate) const MIN_HOLD_MS: i64 = 2 * 60 * 1000;

/// How much shorter than the server's longest hold a wait has to be.
const HOLD_MARGIN_SECS: i64 = 60;

/// A release time this far ahead says the server holds the message.
const HELD_SLACK_MS: i64 = 30 * 1000;

/// Whether a server that holds submissions up to `max_delayed_send` seconds
/// can hold a message until `send_at`.
pub(crate) fn can_hold(max_delayed_send: i64, send_at: i64, now: i64) -> bool {
    let wait = send_at - now;
    wait >= MIN_HOLD_MS && wait / 1000 + HOLD_MARGIN_SECS <= max_delayed_send
}

/// How a send action ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Finish {
    /// Sent.
    Done,
    /// It waits: held by the server, or for its time on this device.
    Waiting,
    /// The server's hold was cancelled: the message is a draft again.
    Cancelled,
}

/// Run a send action: cancel the server's hold on it, hand it to the server
/// to hold, or send it now.
pub(super) async fn send(
    ctx: &SyncCtx,
    config: &AccountConfig,
    c: &ConnectedClient,
    action: &repo::actions::PendingAction,
) -> Result<Finish> {
    if action.payload["unsend"].as_bool() == Some(true) {
        return unhold(ctx, config, c, action).await;
    }
    if action.payload["serverHold"].as_bool() == Some(true)
        && let Some(send_at) = action.payload["sendAt"].as_i64()
    {
        return hold(ctx, config, c, action, send_at).await;
    }
    send_action(ctx, config, c, action).await?;
    Ok(Finish::Done)
}

/// What the server says of a submission.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Submission {
    undo: UndoStatus,
    /// When it was or will be released, in ms.
    send_at: i64,
    email_id: Option<String>,
}

async fn submissions(c: &ConnectedClient, ids: &[String]) -> Result<HashMap<String, Submission>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let mut request = c.request();
    request
        .get_email_submission()
        .account_id(&c.account_id)
        .ids(ids.iter().map(String::as_str));
    let mut response = c
        .send_single::<EmailSubmissionGetResponse>(request)
        .await
        .map_err(client::map_error)?;
    Ok(response
        .take_list()
        .into_iter()
        .filter_map(|submission| {
            Some((
                submission.id()?.to_owned(),
                Submission {
                    undo: *submission.undo_status()?,
                    send_at: submission
                        .send_at()
                        .unwrap_or_default()
                        .saturating_mul(1000),
                    email_id: submission.email_id().map(str::to_owned),
                },
            ))
        })
        .collect())
}

/// A submission of the Email that sends or sent it. A cancelled one never
/// went out, so the Email may still be sent.
pub(super) async fn live_submission(c: &ConnectedClient, email_id: &str) -> Result<Option<String>> {
    let ids = c
        .email_submission_query(Filter::email_ids([email_id.to_owned()]))
        .await
        .map_err(client::map_error)?
        .ids()
        .to_vec();
    Ok(submissions(c, &ids)
        .await?
        .into_iter()
        .find(|(_, submission)| submission.undo != UndoStatus::Canceled)
        .map(|(id, _)| id))
}

/// Hand a scheduled message to the server to hold until `send_at`.
async fn hold(
    ctx: &SyncCtx,
    config: &AccountConfig,
    c: &ConnectedClient,
    action: &repo::actions::PendingAction,
    send_at: i64,
) -> Result<Finish> {
    let account_id = config.id;
    let action_id = action.id;
    let draft_id = action.payload["draftId"]
        .as_i64()
        .ok_or_else(|| CoreError::Jmap("send action has no draftId".into()))?;
    // As the last session said.
    let (scheduled, max_delayed_send) = ctx
        .db
        .read(move |conn| {
            let scheduled = repo::folders::by_jmap_role(conn, account_id, roles::SCHEDULED)?
                .and_then(|folder| folder.jmap_id.map(|remote| (folder.id, remote)));
            let max_delayed_send: i64 = conn.query_row(
                "SELECT jmap_max_delayed_send FROM accounts WHERE id = ?1",
                [account_id],
                |row| row.get(0),
            )?;
            Ok((scheduled, max_delayed_send))
        })
        .await?;
    let Some((scheduled_folder, scheduled)) =
        scheduled.filter(|_| can_hold(max_delayed_send, send_at, now_ms()))
    else {
        return on_device(ctx, action_id, send_at).await;
    };
    let remote_email = prepared_email(ctx, config, c, action, draft_id, false).await?;

    // A lost response must not hand it over twice.
    let submission_id = match live_submission(c, &remote_email).await? {
        Some(id) => id,
        None => {
            let (sender, identity_id) = sending_identity(ctx, c, draft_id).await?;
            let recipients = recipients(ctx, draft_id).await?;
            if recipients.is_empty() {
                return Err(CoreError::Jmap(
                    "the scheduled message has no recipients".into(),
                ));
            }
            let hold_for = ((send_at - now_ms()) / 1000).max(1);
            let mut request = c.request();
            let set = request.set_email_submission().account_id(&c.account_id);
            let create_id = set
                .create()
                .email_id(&remote_email)
                .identity_id(identity_id)
                .envelope(
                    Address::new(sender).parameter("HOLDFOR", Some(hold_for.to_string())),
                    recipients,
                )
                .create_id()
                .unwrap_or_else(|| "c0".into());
            // It waits in the Scheduled mailbox, still a draft.
            set.arguments()
                .on_success_update_email(&create_id)
                .mailbox_ids([scheduled.clone()]);
            let mut response = c
                .send_single::<EmailSubmissionSetResponse>(request)
                .await
                .map_err(map_submission_error)?;
            match response.created(&create_id) {
                Ok(created) => created
                    .id()
                    .map(str::to_owned)
                    .ok_or_else(|| CoreError::Jmap("EmailSubmission/set returned no id".into()))?,
                // The server would not hold it: this device sends it.
                Err(jmap_client::Error::Set(error))
                    if error.type_ == SetErrorType::InvalidProperties =>
                {
                    tracing::info!(account_id, %error, "the JMAP server would not hold a scheduled message");
                    return on_device(ctx, action_id, send_at).await;
                }
                Err(error) => return Err(map_submission_error(error)),
            }
        }
    };

    // RFC 8621 makes sendAt the release time when the hold took.
    let status = submissions(c, std::slice::from_ref(&submission_id))
        .await?
        .remove(&submission_id);
    match status {
        Some(status)
            if status.undo == UndoStatus::Pending && status.send_at > now_ms() + HELD_SLACK_MS =>
        {
            let thread = ctx
                .db
                .write(move |conn| {
                    let tx = conn.transaction()?;
                    repo::actions::hold_on_server(&tx, action_id, &submission_id, &remote_email)?;
                    let thread = move_locally(&tx, draft_id, scheduled_folder)?;
                    tx.commit()?;
                    Ok(thread)
                })
                .await?;
            changed(ctx, action_id, "pending", thread);
            Ok(Finish::Waiting)
        }
        Some(status) if status.undo == UndoStatus::Canceled => Ok(Finish::Cancelled),
        // It went out now.
        _ => sent(ctx, config, c, Some(draft_id), &remote_email).await,
    }
}

/// The server cannot hold it: this device sends it at its time.
async fn on_device(ctx: &SyncCtx, action_id: i64, send_at: i64) -> Result<Finish> {
    ctx.db
        .write(move |conn| repo::actions::hold_on_device(conn, action_id, send_at))
        .await?;
    Ok(Finish::Waiting)
}

/// Everyone the draft goes to, once each.
async fn recipients(ctx: &SyncCtx, draft_id: i64) -> Result<Vec<String>> {
    ctx.db
        .read(move |conn| {
            let detail = repo::messages::detail(conn, draft_id)?;
            let bcc: String = conn.query_row(
                "SELECT bcc_json FROM messages WHERE id = ?1",
                [draft_id],
                |row| row.get(0),
            )?;
            let bcc = serde_json::from_str::<Vec<MailAddress>>(&bcc)?;
            let mut seen = std::collections::HashSet::new();
            Ok(detail
                .to
                .into_iter()
                .chain(detail.cc)
                .chain(bcc)
                .map(|address| address.email)
                .filter(|email| !email.is_empty() && seen.insert(email.to_lowercase()))
                .collect())
        })
        .await
}

/// Cancel the server's hold: the message is a draft again, in Drafts.
async fn unhold(
    ctx: &SyncCtx,
    config: &AccountConfig,
    c: &ConnectedClient,
    action: &repo::actions::PendingAction,
) -> Result<Finish> {
    let held = &action.payload["jmapHeld"];
    let (Some(submission_id), Some(email_id)) =
        (held["submissionId"].as_str(), held["emailId"].as_str())
    else {
        return Ok(Finish::Cancelled);
    };
    let mut request = c.request();
    request
        .set_email_submission()
        .account_id(&c.account_id)
        .update(submission_id)
        .undo_status(UndoStatus::Canceled);
    let mut response = c
        .send_single::<EmailSubmissionSetResponse>(request)
        .await
        .map_err(client::map_error)?;
    match response.updated(submission_id) {
        Ok(_) => {}
        // Gone from the server: nothing is left to cancel.
        Err(jmap_client::Error::Set(error)) if error.type_ == SetErrorType::NotFound => {}
        // Cancelled already, or too late: the server says which.
        Err(jmap_client::Error::Set(error)) => {
            let status = submissions(c, &[submission_id.to_owned()])
                .await?
                .remove(submission_id)
                .map(|status| status.undo);
            match status {
                Some(UndoStatus::Canceled) | None => {}
                Some(UndoStatus::Final) => {
                    return sent(ctx, config, c, action.message_id, email_id).await;
                }
                Some(UndoStatus::Pending) => {
                    return Err(CoreError::Jmap(format!(
                        "the server would not cancel the scheduled message: {error}"
                    )));
                }
            }
        }
        Err(error) => return Err(client::map_error(error)),
    }
    back_to_drafts(ctx, config, c, action.message_id, email_id).await?;
    Ok(Finish::Cancelled)
}

/// The server released it: in Sent, no longer a draft.
async fn sent(
    ctx: &SyncCtx,
    config: &AccountConfig,
    c: &ConnectedClient,
    draft_id: Option<i64>,
    email_id: &str,
) -> Result<Finish> {
    let sent_mailbox =
        ensure_role_mailbox(ctx, config.id, c, roles::SENT, "Sent", Role::Sent).await?;
    ensure_submitted_email_state(c, email_id, &sent_mailbox).await?;
    if let Some(draft_id) = draft_id {
        finish_sent_locally(ctx, config, draft_id).await?;
    }
    Ok(Finish::Done)
}

async fn back_to_drafts(
    ctx: &SyncCtx,
    config: &AccountConfig,
    c: &ConnectedClient,
    draft_id: Option<i64>,
    email_id: &str,
) -> Result<()> {
    let drafts =
        ensure_role_mailbox(ctx, config.id, c, roles::DRAFTS, "Drafts", Role::Drafts).await?;
    match c.email_set_mailboxes(email_id, [drafts]).await {
        Ok(_) => {}
        Err(jmap_client::Error::Set(error)) if error.type_ == SetErrorType::NotFound => {}
        Err(error) => return Err(client::map_error(error)),
    }
    let account_id = config.id;
    if let Some(draft_id) = draft_id {
        let thread = ctx
            .db
            .write(move |conn| {
                let Some(folder) = repo::folders::by_jmap_role(conn, account_id, roles::DRAFTS)?
                else {
                    return Ok(None);
                };
                move_locally(conn, draft_id, folder.id)
            })
            .await?;
        if let Some(thread) = thread {
            ctx.bus.emit(CoreEvent::MailUpdated {
                thread_ids: vec![thread],
            });
        }
    }
    Ok(())
}

/// Show the message in `folder` until the next sync says so too. Returns
/// its conversation.
fn move_locally(conn: &rusqlite::Connection, message: i64, folder: i64) -> Result<Option<i64>> {
    let Some(row) = repo::messages::get_row(conn, message)? else {
        return Ok(None);
    };
    conn.execute(
        "UPDATE messages SET folder_id = ?2 WHERE id = ?1",
        params![message, folder],
    )?;
    repo::gmail::set_message_folders(conn, message, &[folder])?;
    if let Some(thread) = row.thread_id {
        repo::threads::recompute(conn, thread)?;
    }
    Ok(row.thread_id)
}

fn changed(ctx: &SyncCtx, action_id: i64, state: &str, thread: Option<i64>) {
    ctx.bus.emit(CoreEvent::ActionState {
        action_id,
        state: state.into(),
        error: None,
    });
    if let Some(thread) = thread {
        ctx.bus.emit(CoreEvent::MailUpdated {
            thread_ids: vec![thread],
        });
    }
}

/// Follow what the server did with the messages it holds: released,
/// cancelled elsewhere, or new from another device or app. Asks the server
/// only while this device knows of a held message, or the Scheduled mailbox
/// has one.
pub(super) async fn reconcile(
    ctx: &SyncCtx,
    config: &AccountConfig,
    c: &ConnectedClient,
) -> Result<()> {
    let account_id = config.id;
    let (held, scheduled) = ctx
        .db
        .read(move |conn| {
            let held = repo::actions::held_submissions(conn, account_id)?;
            let scheduled = repo::folders::by_jmap_role(conn, account_id, roles::SCHEDULED)?
                .map(|folder| folder.id)
                .filter(|folder| holds_mail(conn, *folder).unwrap_or(false));
            Ok((held, scheduled))
        })
        .await?;
    if held.is_empty() && scheduled.is_none() {
        return Ok(());
    }
    let mut ids: Vec<String> = held.iter().map(|held| held.submission_id.clone()).collect();
    if scheduled.is_some() {
        let pending = c
            .email_submission_query(Filter::undo_status(UndoStatus::Pending))
            .await
            .map_err(client::map_error)?;
        for id in pending.ids() {
            if !ids.contains(id) {
                ids.push(id.clone());
            }
        }
    }
    let statuses = submissions(c, &ids).await?;
    let now = now_ms();

    for held in &held {
        // Being cancelled from here.
        if held.unsend {
            continue;
        }
        let status = statuses.get(&held.submission_id);
        let ended = match status.map(|status| status.undo) {
            Some(UndoStatus::Pending) => continue,
            Some(UndoStatus::Final) => Finish::Done,
            Some(UndoStatus::Canceled) => Finish::Cancelled,
            // The server forgot it: sent if its time came.
            None if now >= held.send_at => Finish::Done,
            None => Finish::Cancelled,
        };
        let end = match ended {
            Finish::Done => sent(ctx, config, c, held.draft_id, &held.email_id).await,
            _ => back_to_drafts(ctx, config, c, held.draft_id, &held.email_id)
                .await
                .map(|()| Finish::Cancelled),
        };
        let state = match end {
            Ok(Finish::Done) => "done",
            Ok(_) => "cancelled",
            Err(error) => {
                tracing::warn!(account_id, %error, "could not follow a held JMAP send");
                continue;
            }
        };
        let action_id = held.action_id;
        ctx.db
            .write(move |conn| repo::actions::set_state(conn, action_id, state, None))
            .await?;
        changed(ctx, action_id, state, None);
    }

    // Another device's or app's: the message waits here too.
    let Some(scheduled) = scheduled else {
        return Ok(());
    };
    for (submission_id, status) in statuses {
        if status.undo != UndoStatus::Pending
            || held.iter().any(|held| held.submission_id == submission_id)
        {
            continue;
        }
        let Some(email_id) = status.email_id else {
            continue;
        };
        let send_at = status.send_at;
        let mirrored = ctx
            .db
            .write(move |conn| {
                let Some((message, thread)) = message_in(conn, account_id, &email_id, scheduled)?
                else {
                    return Ok(None);
                };
                let action = repo::actions::mirror_held(
                    conn,
                    account_id,
                    message,
                    thread,
                    send_at,
                    &submission_id,
                    &email_id,
                )?;
                Ok(action.map(|action| (action, thread)))
            })
            .await?;
        if let Some((action, thread)) = mirrored {
            changed(ctx, action, "pending", thread);
        }
    }
    Ok(())
}

fn holds_mail(conn: &rusqlite::Connection, folder: i64) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM messages WHERE folder_id = ?1)
             OR EXISTS (SELECT 1 FROM message_folders WHERE folder_id = ?1)",
        params![folder],
        |row| row.get(0),
    )?)
}

/// The local copy of `email_id` in the `folder` mailbox, and its
/// conversation.
fn message_in(
    conn: &rusqlite::Connection,
    account_id: i64,
    email_id: &str,
    folder: i64,
) -> Result<Option<(i64, Option<i64>)>> {
    Ok(conn
        .query_row(
            "SELECT m.id, m.thread_id FROM messages m
             WHERE m.account_id = ?1 AND m.jmap_id = ?2
               AND (m.folder_id = ?3
                    OR EXISTS (SELECT 1 FROM message_folders mf
                               WHERE mf.message_id = m.id AND mf.folder_id = ?3))",
            params![account_id, email_id, folder],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_server_holds_what_its_longest_hold_covers() {
        let now = 1_000_000_000;
        let day = 86_400_000;
        // Fastmail's Cyrus says 512 days; Stalwart 30 by default.
        assert!(can_hold(512 * 86_400, now + 30 * day, now));
        assert!(can_hold(30 * 86_400, now + 29 * day, now));
        assert!(!can_hold(30 * 86_400, now + 30 * day, now));
        // None, or the seconds of undo send.
        assert!(!can_hold(0, now + day, now));
        assert!(!can_hold(512 * 86_400, now + 20_000, now));
        assert!(can_hold(512 * 86_400, now + MIN_HOLD_MS, now));
    }
}
