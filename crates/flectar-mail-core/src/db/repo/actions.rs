use crate::error::Result;
use crate::models::now_ms;
use rusqlite::{Connection, OptionalExtension, Row, params};

use super::parse_json_column;

/// `not_before` of a send held for Flectar Mail Sync: it never comes due on
/// its own, and goes out only once this device's claim on it holds.
pub const HELD: i64 = i64::MAX;

/// How long before its claim ends a device stops starting a synced send, so
/// the next device that claims it never sends at the same time.
pub const CLAIM_MARGIN_MS: i64 = 60_000;

#[derive(Debug, Clone)]
pub struct PendingAction {
    pub id: i64,
    pub account_id: i64,
    pub kind: String,
    pub message_id: Option<i64>,
    pub thread_id: Option<i64>,
    pub payload: serde_json::Value,
    pub state: String,
    pub attempts: i64,
    pub not_before: Option<i64>,
    pub created_at: i64,
}

fn from_row(row: &Row) -> rusqlite::Result<PendingAction> {
    let payload_json = row.get::<_, String>("payload")?;
    let payload = parse_json_column(&payload_json, 5)?;
    Ok(PendingAction {
        id: row.get("id")?,
        account_id: row.get("account_id")?,
        kind: row.get("kind")?,
        message_id: row.get("message_id")?,
        thread_id: row.get("thread_id")?,
        payload,
        state: row.get("state")?,
        attempts: row.get("attempts")?,
        not_before: row.get("not_before")?,
        created_at: row.get("created_at")?,
    })
}

pub fn enqueue(
    conn: &Connection,
    account_id: i64,
    kind: &str,
    message_id: Option<i64>,
    thread_id: Option<i64>,
    payload: &serde_json::Value,
    not_before: Option<i64>,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO pending_actions (account_id, kind, message_id, thread_id, payload, not_before, created_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![
            account_id,
            kind,
            message_id,
            thread_id,
            serde_json::to_string(payload)?,
            not_before,
            now_ms()
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Due actions for one account, oldest first.
pub fn due(conn: &Connection, account_id: i64, now: i64, limit: i64) -> Result<Vec<PendingAction>> {
    // cal_% actions belong to the CalDAV task, not the IMAP executor.
    let mut stmt = conn.prepare(
        "SELECT id, account_id, kind, message_id, thread_id, payload, state,
                attempts, not_before, created_at
         FROM pending_actions
         WHERE account_id = ?1 AND state = 'pending' AND (not_before IS NULL OR not_before <= ?2)
           AND kind NOT LIKE 'cal!_%' ESCAPE '!'
         ORDER BY created_at ASC, id ASC LIMIT ?3",
    )?;
    let rows = stmt
        .query_map(params![account_id, now, limit], from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Due IMAP/local actions for one account. SMTP submission has a dedicated
/// worker so an Inbox sync can never hold an interactive send in the queue.
pub fn due_except_send(
    conn: &Connection,
    account_id: i64,
    now: i64,
    limit: i64,
) -> Result<Vec<PendingAction>> {
    let mut stmt = conn.prepare(
        "SELECT id, account_id, kind, message_id, thread_id, payload, state,
                attempts, not_before, created_at
         FROM pending_actions
         WHERE account_id = ?1 AND state = 'pending' AND (not_before IS NULL OR not_before <= ?2)
           AND kind NOT LIKE 'cal!_%' ESCAPE '!' AND kind <> 'send'
         ORDER BY created_at ASC, id ASC LIMIT ?3",
    )?;
    let rows = stmt
        .query_map(params![account_id, now, limit], from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Due SMTP submissions for one account, oldest first. Only the dedicated
/// outbound worker calls this query.
pub fn due_sends(
    conn: &Connection,
    account_id: i64,
    now: i64,
    limit: i64,
) -> Result<Vec<PendingAction>> {
    let mut stmt = conn.prepare(
        "SELECT id, account_id, kind, message_id, thread_id, payload, state,
                attempts, not_before, created_at
         FROM pending_actions
         WHERE account_id = ?1 AND state = 'pending' AND kind = 'send'
           AND (not_before IS NULL OR not_before <= ?2)
         ORDER BY created_at ASC, id ASC LIMIT ?3",
    )?;
    let rows = stmt
        .query_map(params![account_id, now, limit], from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Whether one account has more IMAP/SMTP actions ready to execute now.
/// CalDAV actions are owned by the calendar task and intentionally excluded.
pub fn has_due(conn: &Connection, account_id: i64, now: i64) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM pending_actions
             WHERE account_id = ?1 AND state = 'pending'
               AND (not_before IS NULL OR not_before <= ?2)
               AND kind NOT LIKE 'cal!_%' ESCAPE '!'
         )",
        params![account_id, now],
        |row| row.get(0),
    )?)
}

pub fn has_due_except_send(conn: &Connection, account_id: i64, now: i64) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM pending_actions
             WHERE account_id = ?1 AND state = 'pending'
               AND (not_before IS NULL OR not_before <= ?2)
               AND kind NOT LIKE 'cal!_%' ESCAPE '!' AND kind <> 'send'
         )",
        params![account_id, now],
        |row| row.get(0),
    )?)
}

pub fn has_due_sends(conn: &Connection, account_id: i64, now: i64) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM pending_actions
             WHERE account_id = ?1 AND state = 'pending' AND kind = 'send'
               AND (not_before IS NULL OR not_before <= ?2)
         )",
        params![account_id, now],
        |row| row.get(0),
    )?)
}

/// Earliest future not_before across pending actions (for the scheduler).
/// Due CalDAV write actions (the calendar task's slice of the queue).
pub fn due_calendar(
    conn: &Connection,
    account_id: i64,
    now: i64,
    limit: i64,
) -> Result<Vec<PendingAction>> {
    let mut stmt = conn.prepare(
        "SELECT id, account_id, kind, message_id, thread_id, payload, state,
                attempts, not_before, created_at
         FROM pending_actions
         WHERE account_id = ?1 AND state = 'pending' AND (not_before IS NULL OR not_before <= ?2)
           AND kind LIKE 'cal!_%' ESCAPE '!'
         ORDER BY created_at ASC LIMIT ?3",
    )?;
    let rows = stmt
        .query_map(params![account_id, now, limit], from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn next_due_at(conn: &Connection) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT MIN(not_before) FROM pending_actions
             WHERE state = 'pending' AND not_before IS NOT NULL AND not_before < ?1",
            params![HELD],
            |r| r.get::<_, Option<i64>>(0),
        )
        .optional()?
        .flatten())
}

/// Drafts waiting to be sent at a time the user chose, soonest first.
pub fn scheduled_sends(conn: &Connection) -> Result<Vec<crate::models::ScheduledSend>> {
    let mut statement = conn.prepare(
        "SELECT pa.id, pa.message_id, m.thread_id, pa.account_id,
                COALESCE(json_extract(pa.payload, '$.sendAt'), pa.not_before) AS send_at,
                pa.state, pa.last_error
         FROM pending_actions pa LEFT JOIN messages m ON m.id = pa.message_id
         WHERE pa.kind = 'send' AND pa.message_id IS NOT NULL
           AND pa.state IN ('pending', 'inflight', 'failed')
           AND json_extract(pa.payload, '$.scheduled') = 1
         ORDER BY send_at",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(crate::models::ScheduledSend {
            action_id: row.get(0)?,
            draft_id: row.get(1)?,
            thread_id: row.get(2)?,
            account_id: row.get(3)?,
            send_at: row.get::<_, Option<i64>>(4)?.unwrap_or_default(),
            state: row.get(5)?,
            error: row.get(6)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Stop the draft's waiting or failed send. Returns the action it stopped.
pub fn cancel_draft_send(conn: &Connection, draft_id: i64, now_ms: i64) -> Result<Option<i64>> {
    let action: Option<i64> = conn
        .query_row(
            "SELECT id FROM pending_actions
             WHERE message_id = ?1 AND kind = 'send' AND state IN ('pending', 'failed')
             ORDER BY id DESC LIMIT 1",
            params![draft_id],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(action) = action {
        conn.execute(
            "UPDATE pending_actions SET state = 'cancelled', finished_at = ?2 WHERE id = ?1",
            params![action, now_ms],
        )?;
    }
    Ok(action)
}

/// Whether a timed mail action came due after `after`, up to `until`.
pub fn mail_due_between(conn: &Connection, after: i64, until: i64) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM pending_actions
           WHERE state = 'pending' AND not_before > ?1 AND not_before <= ?2
             AND kind NOT LIKE 'cal!_%' ESCAPE '!')",
        params![after, until],
        |row| row.get(0),
    )?)
}

/// Whether a timed action of the calendar store came due after `after`, up
/// to `until`.
pub fn due_between(conn: &Connection, after: i64, until: i64) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM pending_actions
           WHERE state = 'pending' AND not_before > ?1 AND not_before <= ?2)",
        params![after, until],
        |row| row.get(0),
    )?)
}

pub fn next_mail_due_at(conn: &Connection) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT MIN(not_before) FROM pending_actions
             WHERE state = 'pending' AND not_before IS NOT NULL AND not_before < ?1
               AND kind NOT LIKE 'cal!_%' ESCAPE '!'",
            params![HELD],
            |r| r.get::<_, Option<i64>>(0),
        )
        .optional()?
        .flatten())
}

pub fn next_send_due_at(conn: &Connection, account_id: i64) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT MIN(COALESCE(not_before, created_at)) FROM pending_actions
             WHERE account_id = ?1 AND state = 'pending' AND kind = 'send'
               AND COALESCE(not_before, 0) < ?2",
            params![account_id, HELD],
            |r| r.get::<_, Option<i64>>(0),
        )
        .optional()?
        .flatten())
}

pub fn get(conn: &Connection, id: i64) -> Result<Option<PendingAction>> {
    let mut stmt = conn.prepare(
        "SELECT id, account_id, kind, message_id, thread_id, payload, state,
                attempts, not_before, created_at
         FROM pending_actions WHERE id = ?1",
    )?;
    Ok(stmt.query_row(params![id], from_row).optional()?)
}

pub fn set_state(conn: &Connection, id: i64, state: &str, error: Option<&str>) -> Result<()> {
    conn.execute(
        "UPDATE pending_actions SET state = ?2, last_error = ?3,
                finished_at = CASE WHEN ?2 IN ('done','failed','cancelled') THEN ?4 ELSE finished_at END
         WHERE id = ?1",
        params![id, state, error, now_ms()],
    )?;
    Ok(())
}

pub fn set_payload(conn: &Connection, id: i64, payload: &serde_json::Value) -> Result<()> {
    conn.execute(
        "UPDATE pending_actions SET payload=?2 WHERE id=?1",
        params![id, serde_json::to_string(payload)?],
    )?;
    Ok(())
}

/// Atomically claim a pending action for execution. Returns false if it was
/// cancelled (or otherwise transitioned) since being read - the executor must
/// skip it in that case.
///
/// A send shared through Flectar Mail Sync goes out only while this device
/// may send it; otherwise it is held again until its claim holds. Sends
/// live in the mail database only: the calendar store's own queue has no
/// shared sends to check.
pub fn try_claim(conn: &Connection, id: i64) -> Result<bool> {
    let send: bool = conn
        .query_row(
            "SELECT kind = 'send' FROM pending_actions WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(false);
    if send
        && conn.execute(
            "UPDATE pending_actions SET not_before = ?2
             WHERE id = ?1 AND state = 'pending' AND EXISTS (
               SELECT 1 FROM synced_sends s
               WHERE s.action_id = ?1
                 AND NOT (s.state = 'scheduled' AND COALESCE(s.may_send_until, 0) > ?3))",
            params![id, HELD, now_ms().saturating_add(CLAIM_MARGIN_MS)],
        )? > 0
    {
        return Ok(false);
    }
    let n = conn.execute(
        "UPDATE pending_actions SET state = 'inflight' WHERE id = ?1 AND state = 'pending'",
        params![id],
    )?;
    Ok(n > 0)
}

/// Reset actions abandoned mid-flight (the app was killed/crashed while one was
/// executing) back to pending so they get retried. An `inflight` row can only be
/// orphaned at startup because no actor is running yet; left as-is it would be
/// invisible to `due()` and stick forever (e.g. a send stuck on "Sending…").
/// Returns how many were recovered.
pub fn recover_inflight(conn: &Connection) -> Result<usize> {
    let n = conn.execute(
        "UPDATE pending_actions SET state = 'pending', not_before = ?1
         WHERE state = 'inflight'",
        params![now_ms()],
    )?;
    Ok(n)
}

/// Make a still-pending action due immediately ("send now" / skip the undo
/// window). Returns the action's account_id so the caller can nudge that actor,
/// or None if it was already claimed/cancelled/sent.
pub fn expedite(conn: &Connection, id: i64) -> Result<Option<i64>> {
    let n = conn.execute(
        "UPDATE pending_actions SET not_before = ?2 WHERE id = ?1 AND state = 'pending'",
        params![id, now_ms()],
    )?;
    if n == 0 {
        return Ok(None);
    }
    Ok(conn
        .query_row(
            "SELECT account_id FROM pending_actions WHERE id = ?1",
            params![id],
            |r| r.get::<_, i64>(0),
        )
        .optional()?)
}

/// Transition pending -> cancelled; returns false if it was no longer pending.
/// A send the account's JMAP server holds to release at its time: the
/// EmailSubmission it made, and the Email it sends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldSubmission {
    pub action_id: i64,
    pub draft_id: Option<i64>,
    pub submission_id: String,
    pub email_id: String,
    pub send_at: i64,
    /// The user asked to cancel it.
    pub unsend: bool,
}

/// The server holds the send now: it waits, held, until the server
/// releases it or it is cancelled.
pub fn hold_on_server(
    conn: &Connection,
    id: i64,
    submission_id: &str,
    email_id: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE pending_actions
         SET state = 'pending', not_before = ?4, attempts = 0, last_error = NULL,
             payload = json_set(json_remove(payload, '$.serverHold', '$.unsend'),
                                '$.jmapHeld',
                                json_object('submissionId', ?2, 'emailId', ?3))
         WHERE id = ?1",
        params![id, submission_id, email_id, HELD],
    )?;
    Ok(())
}

/// The server cannot hold the send after all: this device sends it at its
/// time.
pub fn hold_on_device(conn: &Connection, id: i64, send_at: i64) -> Result<()> {
    conn.execute(
        "UPDATE pending_actions
         SET state = 'pending', not_before = ?2, payload = json_remove(payload, '$.serverHold')
         WHERE id = ?1",
        params![id, send_at],
    )?;
    Ok(())
}

/// The account's sends its JMAP server holds, as this device knows them.
pub fn held_submissions(conn: &Connection, account_id: i64) -> Result<Vec<HeldSubmission>> {
    let mut statement = conn.prepare(
        "SELECT id, message_id, json_extract(payload, '$.jmapHeld.submissionId'),
                json_extract(payload, '$.jmapHeld.emailId'),
                COALESCE(json_extract(payload, '$.sendAt'), 0),
                COALESCE(json_extract(payload, '$.unsend'), 0)
         FROM pending_actions
         WHERE account_id = ?1 AND kind = 'send' AND state IN ('pending', 'inflight')
           AND json_extract(payload, '$.jmapHeld.submissionId') IS NOT NULL
         ORDER BY id",
    )?;
    let held = statement
        .query_map(params![account_id], |row| {
            Ok(HeldSubmission {
                action_id: row.get(0)?,
                draft_id: row.get(1)?,
                submission_id: row.get(2)?,
                email_id: row.get(3)?,
                send_at: row.get(4)?,
                unsend: row.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(held)
}

/// The waiting send of a draft its account's JMAP server holds, if any.
pub fn held_send_of(conn: &Connection, draft_id: i64) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT id FROM pending_actions
             WHERE message_id = ?1 AND kind = 'send' AND state IN ('pending', 'inflight')
               AND json_extract(payload, '$.jmapHeld.submissionId') IS NOT NULL
             ORDER BY id DESC LIMIT 1",
            params![draft_id],
            |row| row.get(0),
        )
        .optional()?)
}

/// Whether the action is a send its account's JMAP server holds.
pub fn is_held_on_server(conn: &Connection, id: i64) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT json_extract(payload, '$.jmapHeld.submissionId') IS NOT NULL
             FROM pending_actions WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(false))
}

/// A submission another device or app made: the message waits here as a
/// send the server holds too. Returns the new send, unless one is here
/// already.
pub fn mirror_held(
    conn: &Connection,
    account_id: i64,
    draft_id: i64,
    thread_id: Option<i64>,
    send_at: i64,
    submission_id: &str,
    email_id: &str,
) -> Result<Option<i64>> {
    let known: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM pending_actions
           WHERE account_id = ?1 AND kind = 'send'
             AND (json_extract(payload, '$.jmapHeld.submissionId') = ?2
                  OR (message_id = ?3 AND state IN ('pending', 'inflight'))))",
        params![account_id, submission_id, draft_id],
        |row| row.get(0),
    )?;
    if known {
        return Ok(None);
    }
    let payload = serde_json::json!({
        "draftId": draft_id,
        "scheduled": true,
        "sendAt": send_at,
        "jmapHeld": {"submissionId": submission_id, "emailId": email_id},
    });
    Ok(Some(enqueue(
        conn,
        account_id,
        "send",
        Some(draft_id),
        thread_id,
        &payload,
        Some(HELD),
    )?))
}

/// Ask for a held send to be cancelled: it comes due at once for the
/// account's JMAP actor, which cancels it on the server. Returns whether it
/// was waiting.
pub fn request_unsend(conn: &Connection, id: i64, now_ms: i64) -> Result<bool> {
    Ok(conn.execute(
        "UPDATE pending_actions
         SET payload = json_set(payload, '$.unsend', json('true')), not_before = ?2,
             attempts = 0, last_error = NULL
         WHERE id = ?1 AND state = 'pending'
           AND json_extract(payload, '$.jmapHeld.submissionId') IS NOT NULL",
        params![id, now_ms],
    )? == 1)
}

/// The cancel did not reach the server: the send stays held. Returns whether
/// it was still waiting to be cancelled.
pub fn withdraw_unsend(conn: &Connection, id: i64) -> Result<bool> {
    Ok(conn.execute(
        "UPDATE pending_actions
         SET payload = json_remove(payload, '$.unsend'), not_before = ?2
         WHERE id = ?1 AND state = 'pending' AND json_extract(payload, '$.unsend') = 1",
        params![id, HELD],
    )? == 1)
}

pub fn try_cancel(conn: &Connection, id: i64) -> Result<bool> {
    let n = conn.execute(
        "UPDATE pending_actions SET state = 'cancelled', finished_at = ?2
         WHERE id = ?1 AND state = 'pending'",
        params![id, now_ms()],
    )?;
    Ok(n > 0)
}

pub fn bump_attempt(conn: &Connection, id: i64, retry_at: i64, error: &str) -> Result<()> {
    conn.execute(
        "UPDATE pending_actions SET attempts = attempts + 1, state = 'pending',
                not_before = ?2, last_error = ?3
         WHERE id = ?1",
        params![id, retry_at, error],
    )?;
    Ok(())
}

/// Return an action to the pending queue without consuming its retry budget.
///
/// A connection failure says nothing about whether the action itself is
/// valid. Keeping transport availability separate from delivery attempts lets
/// offline-first actions wait indefinitely for the network to return.
pub fn defer_offline(conn: &Connection, id: i64, retry_at: i64, error: &str) -> Result<()> {
    conn.execute(
        "UPDATE pending_actions SET state = 'pending', not_before = ?2, last_error = ?3
         WHERE id = ?1",
        params![id, retry_at, error],
    )?;
    Ok(())
}

/// Whether an optimistic move is still in flight from this folder. Header sync
/// uses this to avoid re-linking the old server copy to a row already moved
/// locally while the queued IMAP command is waiting to run.
pub fn has_active_move_from(conn: &Connection, message_id: i64, folder_id: i64) -> Result<bool> {
    let mut stmt = conn.prepare(
        "SELECT payload FROM pending_actions
         WHERE message_id = ?1
           AND kind IN ('archive','unarchive','trash','spam','not_spam','move')
           AND state IN ('pending','inflight')",
    )?;
    let payloads = stmt.query_map(params![message_id], |row| row.get::<_, String>(0))?;
    for payload in payloads {
        let payload: serde_json::Value = serde_json::from_str(&payload?)?;
        if payload["srcFolderId"].as_i64() == Some(folder_id) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Most recent undoable action (pending or just done, within the window).
pub fn last_undoable(conn: &Connection, since_ms: i64) -> Result<Option<PendingAction>> {
    let mut stmt = conn.prepare(
        "SELECT id, account_id, kind, message_id, thread_id, payload, state,
                attempts, not_before, created_at
         FROM pending_actions
         WHERE created_at >= ?1 AND state IN ('pending','inflight','done')
           AND kind IN ('archive','trash','spam','mark_read','mark_unread','star','unstar','snooze','send','move','add_label','remove_label')
         ORDER BY created_at DESC LIMIT 1",
    )?;
    Ok(stmt.query_row(params![since_ms], from_row).optional()?)
}

/// Whether an action that changes which mail a folder holds, or whether it
/// is read, is still on its way to the server for `account` (an SQL
/// expression): until it arrives, this device and the server count apart.
pub(crate) fn counts_settling_sql(account: &str) -> String {
    format!(
        "EXISTS (SELECT 1 FROM pending_actions a
                 WHERE a.account_id = {account} AND a.state IN ('pending', 'inflight')
                   AND a.kind IN ('mark_read', 'mark_unread', 'archive', 'unarchive', 'trash',
                                  'delete_permanently', 'spam', 'not_spam', 'move', 'copy',
                                  'empty_trash', 'remove_transferred'))"
    )
}

/// See `counts_settling_sql`.
pub fn counts_settling(conn: &Connection, account_id: i64) -> Result<bool> {
    Ok(conn.query_row(
        &format!("SELECT {}", counts_settling_sql("?1")),
        params![account_id],
        |row| row.get(0),
    )?)
}

/// Is there any pending action referencing this message (local intent wins over remote flags)?
pub fn has_pending_for_message(conn: &Connection, message_id: i64) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pending_actions
         WHERE message_id = ?1 AND state IN ('pending','inflight')",
        params![message_id],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// Whether local intent may recreate or submit an Email that disappeared
/// remotely. Ordinary flag/move actions cannot resurrect a destroyed Email and
/// therefore must not keep a stale local row alive.
pub fn has_pending_remote_creation(conn: &Connection, message_id: i64) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(
           SELECT 1 FROM pending_actions
           WHERE message_id=?1 AND state IN ('pending','inflight')
             AND kind IN ('save_draft','send','append_sent')
         )",
        params![message_id],
        |row| row.get(0),
    )?)
}

/// Fields whose optimistic JMAP values must win while the corresponding
/// remote mutation is active. Keeping these guards separate prevents a star
/// action from accidentally suppressing an unrelated remote mailbox move.
pub fn jmap_reconciliation_guards(conn: &Connection, message_id: i64) -> Result<(bool, bool)> {
    Ok(conn.query_row(
        "SELECT
           EXISTS(SELECT 1 FROM pending_actions
                  WHERE message_id=?1 AND state IN ('pending','inflight')
                    AND kind IN ('mark_read','mark_unread','star','unstar','add_label','remove_label')),
           EXISTS(SELECT 1 FROM pending_actions
                  WHERE message_id=?1 AND state IN ('pending','inflight')
                    AND kind IN ('archive','unarchive','trash','spam','not_spam','move'))",
        params![message_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?)
}

pub fn gc(conn: &Connection, older_than_ms: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM pending_actions
         WHERE state IN ('done','cancelled') AND finished_at < ?1",
        params![older_than_ms],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testutil;

    #[test]
    fn a_cancel_the_server_never_heard_of_leaves_the_send_held() {
        let conn = testutil::conn();
        testutil::seed_account(&conn);
        let send = enqueue(
            &conn,
            1,
            "send",
            None,
            None,
            &serde_json::json!({"draftId": 9, "scheduled": true, "sendAt": 5_000}),
            Some(1_000),
        )
        .unwrap();
        // Not held by the server yet: nothing to cancel there.
        assert!(!request_unsend(&conn, send, 2_000).unwrap());
        hold_on_server(&conn, send, "S1", "E1").unwrap();
        assert!(is_held_on_server(&conn, send).unwrap());
        assert_eq!(held_submissions(&conn, 1).unwrap()[0].send_at, 5_000);

        assert!(request_unsend(&conn, send, 2_000).unwrap());
        let held = &held_submissions(&conn, 1).unwrap()[0];
        assert!(held.unsend);
        assert_eq!(due(&conn, 1, 2_000, 10).unwrap().len(), 1, "due at once");

        assert!(withdraw_unsend(&conn, send).unwrap());
        assert!(!held_submissions(&conn, 1).unwrap()[0].unsend);
        assert!(
            due(&conn, 1, i64::MAX - 1, 10).unwrap().is_empty(),
            "held again"
        );
        assert!(!withdraw_unsend(&conn, send).unwrap());
    }

    #[test]
    fn the_calendar_store_claims_its_actions() {
        // Its own queue, without the mail database's shared sends.
        let conn = testutil::calendar_conn();
        conn.execute(
            "INSERT INTO pending_actions (id, account_id, kind, created_at)
             VALUES (1, 1, 'cal_put', 0)",
            [],
        )
        .unwrap();
        assert!(try_claim(&conn, 1).unwrap());
    }

    #[test]
    fn an_action_stuck_in_the_past_does_not_hide_later_ones() {
        let conn = testutil::conn();
        testutil::seed_account(&conn);
        let empty = serde_json::json!({});
        // Due long ago, on an account that cannot run it.
        enqueue(&conn, 1, "mark_read", None, None, &empty, Some(100)).unwrap();
        enqueue(&conn, 1, "send", None, None, &empty, Some(5_000)).unwrap();
        assert!(mail_due_between(&conn, 0, 200).unwrap());
        // Nudged for it already: nothing new until the send comes due.
        assert!(!mail_due_between(&conn, 200, 4_000).unwrap());
        assert!(mail_due_between(&conn, 4_000, 6_000).unwrap());
    }

    #[test]
    fn due_is_stable_and_has_due_ignores_future_and_calendar_actions() {
        let conn = testutil::conn();
        testutil::seed_account(&conn);

        let later = enqueue(&conn, 1, "star", None, None, &serde_json::json!({}), None).unwrap();
        let first = enqueue(
            &conn,
            1,
            "archive",
            None,
            None,
            &serde_json::json!({}),
            None,
        )
        .unwrap();
        let future = enqueue(
            &conn,
            1,
            "trash",
            None,
            None,
            &serde_json::json!({}),
            Some(20_000),
        )
        .unwrap();
        let calendar = enqueue(
            &conn,
            1,
            "cal_create",
            None,
            None,
            &serde_json::json!({}),
            None,
        )
        .unwrap();

        // Force the same timestamp to exercise the id tie-breaker.
        conn.execute(
            "UPDATE pending_actions SET created_at = 1 WHERE id IN (?1, ?2)",
            params![later, first],
        )
        .unwrap();

        let ids: Vec<i64> = due(&conn, 1, 10_000, 20)
            .unwrap()
            .into_iter()
            .map(|action| action.id)
            .collect();
        assert_eq!(ids, vec![later, first]);
        assert!(has_due(&conn, 1, 10_000).unwrap());

        set_state(&conn, later, "done", None).unwrap();
        set_state(&conn, first, "done", None).unwrap();
        assert!(!has_due(&conn, 1, 10_000).unwrap());
        assert!(has_due(&conn, 1, 20_000).unwrap());

        let calendar_due = due_calendar(&conn, 1, 10_000, 20).unwrap();
        assert_eq!(calendar_due.len(), 1);
        assert_eq!(calendar_due[0].id, calendar);
        assert_eq!(future, due(&conn, 1, 20_000, 20).unwrap()[0].id);
    }

    #[test]
    fn malformed_payload_is_reported_instead_of_becoming_a_null_action() {
        let conn = testutil::conn();
        testutil::seed_account(&conn);
        conn.execute(
            "INSERT INTO pending_actions
             (account_id,kind,payload,state,attempts,created_at)
             VALUES (1,'send','{','pending',0,1)",
            [],
        )
        .unwrap();

        assert!(due(&conn, 1, 1, 20).is_err());
    }

    #[test]
    fn smtp_submissions_are_owned_only_by_the_send_queue() {
        let conn = testutil::conn();
        testutil::seed_account(&conn);
        let send = enqueue(
            &conn,
            1,
            "send",
            None,
            None,
            &serde_json::json!({ "draftId": 7 }),
            Some(1_000),
        )
        .unwrap();
        let append = enqueue(
            &conn,
            1,
            "append_sent",
            None,
            None,
            &serde_json::json!({}),
            Some(1_000),
        )
        .unwrap();

        assert_eq!(
            due_sends(&conn, 1, 1_000, 20)
                .unwrap()
                .into_iter()
                .map(|action| action.id)
                .collect::<Vec<_>>(),
            vec![send]
        );
        assert_eq!(
            due_except_send(&conn, 1, 1_000, 20)
                .unwrap()
                .into_iter()
                .map(|action| action.id)
                .collect::<Vec<_>>(),
            vec![append]
        );
        assert!(has_due_sends(&conn, 1, 1_000).unwrap());
        assert!(has_due_except_send(&conn, 1, 1_000).unwrap());
        assert_eq!(next_send_due_at(&conn, 1).unwrap(), Some(1_000));
    }

    #[test]
    fn jmap_reconciliation_guards_only_the_fields_owned_by_local_intent() {
        let conn = testutil::conn();
        testutil::seed_account(&conn);
        let (_, message_id) = testutil::seed_message(&conn, "sender@test.dev", "Subject", false);

        enqueue(
            &conn,
            1,
            "star",
            Some(message_id),
            None,
            &serde_json::json!({}),
            None,
        )
        .unwrap();
        assert_eq!(
            jmap_reconciliation_guards(&conn, message_id).unwrap(),
            (true, false)
        );
        assert!(!has_pending_remote_creation(&conn, message_id).unwrap());

        enqueue(
            &conn,
            1,
            "move",
            Some(message_id),
            None,
            &serde_json::json!({}),
            None,
        )
        .unwrap();
        assert_eq!(
            jmap_reconciliation_guards(&conn, message_id).unwrap(),
            (true, true)
        );

        enqueue(
            &conn,
            1,
            "send",
            Some(message_id),
            None,
            &serde_json::json!({ "draftId": message_id }),
            None,
        )
        .unwrap();
        assert!(has_pending_remote_creation(&conn, message_id).unwrap());
    }
}
