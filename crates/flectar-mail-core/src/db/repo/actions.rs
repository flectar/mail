use crate::error::Result;
use crate::models::now_ms;
use rusqlite::{Connection, OptionalExtension, Row, params};

use super::parse_json_column;

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
             WHERE state = 'pending' AND not_before IS NOT NULL",
            [],
            |r| r.get::<_, Option<i64>>(0),
        )
        .optional()?
        .flatten())
}

pub fn next_mail_due_at(conn: &Connection) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT MIN(not_before) FROM pending_actions
             WHERE state = 'pending' AND not_before IS NOT NULL
               AND kind NOT LIKE 'cal!_%' ESCAPE '!'",
            [],
            |r| r.get::<_, Option<i64>>(0),
        )
        .optional()?
        .flatten())
}

pub fn next_send_due_at(conn: &Connection, account_id: i64) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT MIN(COALESCE(not_before, created_at)) FROM pending_actions
             WHERE account_id = ?1 AND state = 'pending' AND kind = 'send'",
            params![account_id],
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
pub fn try_claim(conn: &Connection, id: i64) -> Result<bool> {
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
