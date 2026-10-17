use crate::error::{CoreError, Result};
use crate::models::now_ms;
use rusqlite::{Connection, OptionalExtension, Row, params};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncFailure {
    pub id: i64,
    pub account_id: i64,
    pub stage: String,
    pub folder_id: Option<i64>,
    pub message_id: Option<i64>,
    pub uid: Option<i64>,
    pub attempts: i64,
    pub next_retry_at: Option<i64>,
    pub last_error: String,
    pub created_at: i64,
    pub updated_at: i64,
}

fn from_row(row: &Row) -> rusqlite::Result<SyncFailure> {
    Ok(SyncFailure {
        id: row.get("id")?,
        account_id: row.get("account_id")?,
        stage: row.get("stage")?,
        folder_id: row.get("folder_id")?,
        message_id: row.get("message_id")?,
        uid: row.get("uid")?,
        attempts: row.get("attempts")?,
        next_retry_at: row.get("next_retry_at")?,
        last_error: row.get("last_error")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

pub fn record_header(
    conn: &Connection,
    folder_id: i64,
    uid: i64,
    next_retry_at: Option<i64>,
    error: &str,
) -> Result<i64> {
    record_header_at(conn, folder_id, uid, next_retry_at, error, now_ms())
}

pub fn record_header_at(
    conn: &Connection,
    folder_id: i64,
    uid: i64,
    next_retry_at: Option<i64>,
    error: &str,
    now: i64,
) -> Result<i64> {
    let changed = conn.execute(
        "INSERT INTO sync_failures (
             account_id, stage, folder_id, uid, next_retry_at, last_error, created_at, updated_at
         )
         SELECT account_id, 'header', id, ?2, ?3, ?4, ?5, ?5
         FROM folders WHERE id = ?1
         ON CONFLICT(folder_id, uid) WHERE stage = 'header' DO UPDATE SET
             attempts = sync_failures.attempts + 1,
             next_retry_at = excluded.next_retry_at,
             last_error = excluded.last_error,
             updated_at = excluded.updated_at",
        params![folder_id, uid, next_retry_at, error, now],
    )?;
    if changed == 0 {
        return Err(CoreError::NotFound(format!("folder {folder_id}")));
    }
    Ok(conn.query_row(
        "SELECT id FROM sync_failures
         WHERE stage = 'header' AND folder_id = ?1 AND uid = ?2",
        params![folder_id, uid],
        |row| row.get(0),
    )?)
}

pub fn record_content(
    conn: &Connection,
    message_id: i64,
    next_retry_at: Option<i64>,
    error: &str,
) -> Result<i64> {
    record_content_at(conn, message_id, next_retry_at, error, now_ms())
}

/// Attempt count of the content-stage failure for a message (`0` when none has
/// been recorded yet). Callers use it to widen the retry backoff so a body
/// that keeps failing does not re-run every minute forever.
pub fn content_attempts(conn: &Connection, message_id: i64) -> Result<i64> {
    Ok(conn
        .query_row(
            "SELECT attempts FROM sync_failures WHERE stage = 'content' AND message_id = ?1",
            params![message_id],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(0))
}

pub fn record_content_at(
    conn: &Connection,
    message_id: i64,
    next_retry_at: Option<i64>,
    error: &str,
    now: i64,
) -> Result<i64> {
    let changed = conn.execute(
        "INSERT INTO sync_failures (
             account_id, stage, message_id, next_retry_at, last_error, created_at, updated_at
         )
         SELECT account_id, 'content', id, ?2, ?3, ?4, ?4
         FROM messages WHERE id = ?1
         ON CONFLICT(message_id) WHERE stage = 'content' DO UPDATE SET
             attempts = sync_failures.attempts + 1,
             next_retry_at = excluded.next_retry_at,
             last_error = excluded.last_error,
             updated_at = excluded.updated_at",
        params![message_id, next_retry_at, error, now],
    )?;
    if changed == 0 {
        return Err(CoreError::NotFound(format!("message {message_id}")));
    }
    Ok(conn.query_row(
        "SELECT id FROM sync_failures WHERE stage = 'content' AND message_id = ?1",
        params![message_id],
        |row| row.get(0),
    )?)
}

pub fn get(conn: &Connection, id: i64) -> Result<Option<SyncFailure>> {
    let mut stmt = conn.prepare(
        "SELECT id, account_id, stage, folder_id, message_id, uid, attempts,
                next_retry_at, last_error, created_at, updated_at
         FROM sync_failures WHERE id = ?1",
    )?;
    Ok(stmt.query_row(params![id], from_row).optional()?)
}

fn due(
    conn: &Connection,
    account_id: i64,
    stage: &str,
    now: i64,
    limit: i64,
) -> Result<Vec<SyncFailure>> {
    let mut stmt = conn.prepare(
        "SELECT id, account_id, stage, folder_id, message_id, uid, attempts,
                next_retry_at, last_error, created_at, updated_at
         FROM sync_failures
         WHERE account_id = ?1 AND stage = ?2
           AND (next_retry_at IS NULL OR next_retry_at <= ?3)
         ORDER BY updated_at, id LIMIT ?4",
    )?;
    let rows = stmt
        .query_map(params![account_id, stage, now, limit], from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn due_headers(
    conn: &Connection,
    account_id: i64,
    now: i64,
    limit: i64,
) -> Result<Vec<SyncFailure>> {
    due(conn, account_id, "header", now, limit)
}

pub fn due_content(
    conn: &Connection,
    account_id: i64,
    now: i64,
    limit: i64,
) -> Result<Vec<SyncFailure>> {
    due(conn, account_id, "content", now, limit)
}

pub fn clear_header(conn: &Connection, folder_id: i64, uid: i64) -> Result<bool> {
    Ok(conn.execute(
        "DELETE FROM sync_failures
         WHERE stage = 'header' AND folder_id = ?1 AND uid = ?2",
        params![folder_id, uid],
    )? > 0)
}

pub fn clear_content(conn: &Connection, message_id: i64) -> Result<bool> {
    Ok(conn.execute(
        "DELETE FROM sync_failures WHERE stage = 'content' AND message_id = ?1",
        params![message_id],
    )? > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testutil;

    #[test]
    fn header_failures_upsert_gate_and_clear() {
        let c = testutil::conn();
        testutil::seed_account(&c);

        let id = record_header_at(&c, 1, 42, Some(200), "fetch", 100).unwrap();
        assert!(due_headers(&c, 1, 199, 10).unwrap().is_empty());
        assert_eq!(
            record_header_at(&c, 1, 42, Some(300), "parse", 150).unwrap(),
            id
        );
        let row = get(&c, id).unwrap().unwrap();
        assert_eq!(row.attempts, 2);
        assert_eq!(row.last_error, "parse");
        assert_eq!(row.created_at, 100);
        assert_eq!(row.updated_at, 150);
        assert_eq!(due_headers(&c, 1, 300, 10).unwrap(), vec![row]);
        assert!(clear_header(&c, 1, 42).unwrap());
        assert!(get(&c, id).unwrap().is_none());
    }

    #[test]
    fn content_failure_is_unique_per_message_and_cascades() {
        let c = testutil::conn();
        c.pragma_update(None, "foreign_keys", "ON").unwrap();
        testutil::seed_account(&c);
        let (_, message_id) = testutil::seed_message(&c, "a@test.dev", "Body", false);

        let id = record_content_at(&c, message_id, None, "decode", 10).unwrap();
        assert_eq!(
            record_content_at(&c, message_id, Some(30), "fetch", 20).unwrap(),
            id
        );
        let due = due_content(&c, 1, 30, 10).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].attempts, 2);
        c.execute("DELETE FROM messages WHERE id = ?1", params![message_id])
            .unwrap();
        assert!(get(&c, id).unwrap().is_none());
    }

    #[test]
    fn content_attempts_counts_recorded_failures() {
        let c = testutil::conn();
        c.pragma_update(None, "foreign_keys", "ON").unwrap();
        testutil::seed_account(&c);
        let (_, message_id) = testutil::seed_message(&c, "a@test.dev", "Attempts", false);

        assert_eq!(content_attempts(&c, message_id).unwrap(), 0);
        record_content_at(&c, message_id, None, "decode", 10).unwrap();
        assert_eq!(content_attempts(&c, message_id).unwrap(), 1);
        record_content_at(&c, message_id, None, "decode", 20).unwrap();
        assert_eq!(content_attempts(&c, message_id).unwrap(), 2);
    }
}
