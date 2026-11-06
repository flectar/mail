//! Messages being copied or moved to a folder of another account.

use crate::error::Result;
use rusqlite::{Connection, OptionalExtension, Row, params};

/// What a transfer does next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// Get the source's raw message.
    Fetch,
    /// Put the copy in the target folder.
    Upload,
    /// Remove the source of a move.
    Remove,
}

impl Stage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fetch => "fetch",
            Self::Upload => "upload",
            Self::Remove => "remove",
        }
    }

    fn parse(stage: &str) -> Option<Self> {
        match stage {
            "fetch" => Some(Self::Fetch),
            "upload" => Some(Self::Upload),
            "remove" => Some(Self::Remove),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transfer {
    pub id: i64,
    /// Gone once the source is: removed by the transfer, or elsewhere.
    pub source_message_id: Option<i64>,
    pub source_account_id: i64,
    pub target_folder_id: i64,
    pub is_move: bool,
    pub stage: Stage,
    /// The raw message, once fetched.
    pub staged_path: Option<String>,
    pub rfc_message_id: Option<String>,
    pub seen: bool,
    pub flagged: bool,
    pub received_at: Option<i64>,
    pub attempts: i64,
}

/// A transfer to start: one message, and the folder it goes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewTransfer {
    pub source_message_id: i64,
    pub source_account_id: i64,
    pub target_folder_id: i64,
    pub is_move: bool,
    pub rfc_message_id: Option<String>,
    pub seen: bool,
    pub flagged: bool,
    pub received_at: Option<i64>,
}

const COLUMNS: &str = "id, source_message_id, source_account_id, target_folder_id, is_move,
     stage, staged_path, rfc_message_id, seen, flagged, received_at, attempts";

fn transfer(row: &Row) -> rusqlite::Result<Transfer> {
    let stage: String = row.get(5)?;
    Ok(Transfer {
        id: row.get(0)?,
        source_message_id: row.get(1)?,
        source_account_id: row.get(2)?,
        target_folder_id: row.get(3)?,
        is_move: row.get::<_, i64>(4)? != 0,
        stage: Stage::parse(&stage).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                5,
                rusqlite::types::Type::Text,
                format!("unknown transfer stage {stage}").into(),
            )
        })?,
        staged_path: row.get(6)?,
        rfc_message_id: row.get(7)?,
        seen: row.get::<_, i64>(8)? != 0,
        flagged: row.get::<_, i64>(9)? != 0,
        received_at: row.get(10)?,
        attempts: row.get(11)?,
    })
}

/// Starts a transfer, unless the same message is already on its way to the
/// same folder. Returns its id, or `None` for one already under way.
pub fn insert(conn: &Connection, new: &NewTransfer, now: i64) -> Result<Option<i64>> {
    let under_way: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM mail_transfers
                       WHERE source_message_id = ?1 AND target_folder_id = ?2)",
        params![new.source_message_id, new.target_folder_id],
        |row| row.get(0),
    )?;
    if under_way {
        return Ok(None);
    }
    conn.execute(
        "INSERT INTO mail_transfers (
           source_message_id, source_account_id, target_folder_id, is_move, stage,
           rfc_message_id, seen, flagged, received_at, created_at
         ) VALUES (?1, ?2, ?3, ?4, 'fetch', ?5, ?6, ?7, ?8, ?9)",
        params![
            new.source_message_id,
            new.source_account_id,
            new.target_folder_id,
            new.is_move,
            new.rfc_message_id,
            new.seen,
            new.flagged,
            new.received_at,
            now,
        ],
    )?;
    Ok(Some(conn.last_insert_rowid()))
}

pub fn get(conn: &Connection, id: i64) -> Result<Option<Transfer>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM mail_transfers WHERE id = ?1"),
            params![id],
            transfer,
        )
        .optional()?)
}

/// The oldest transfer due by `now`.
pub fn next_due(conn: &Connection, now: i64) -> Result<Option<Transfer>> {
    Ok(conn
        .query_row(
            &format!(
                "SELECT {COLUMNS} FROM mail_transfers
                 WHERE next_attempt_at <= ?1 ORDER BY id LIMIT 1"
            ),
            params![now],
            transfer,
        )
        .optional()?)
}

/// When the next transfer that waits to try again is due.
pub fn next_attempt_at(conn: &Connection) -> Result<Option<i64>> {
    Ok(conn.query_row(
        "SELECT MIN(next_attempt_at) FROM mail_transfers",
        [],
        |row| row.get(0),
    )?)
}

/// Moves a transfer on to `stage`, which starts with no failed attempts.
pub fn advance(conn: &Connection, id: i64, stage: Stage, staged_path: Option<&str>) -> Result<()> {
    conn.execute(
        "UPDATE mail_transfers
         SET stage = ?2, staged_path = COALESCE(?3, staged_path),
             attempts = 0, next_attempt_at = 0, error = NULL
         WHERE id = ?1",
        params![id, stage.as_str(), staged_path],
    )?;
    Ok(())
}

/// Puts a transfer off until `at` after a failure that may pass.
pub fn retry_at(conn: &Connection, id: i64, at: i64, error: &str) -> Result<()> {
    conn.execute(
        "UPDATE mail_transfers
         SET attempts = attempts + 1, next_attempt_at = ?2, error = ?3
         WHERE id = ?1",
        params![id, at, error],
    )?;
    Ok(())
}

/// Has every transfer that waits to try again try now.
pub fn retry_now(conn: &Connection) -> Result<()> {
    conn.execute(
        "UPDATE mail_transfers SET next_attempt_at = 0 WHERE next_attempt_at > 0",
        [],
    )?;
    Ok(())
}

/// Ends a transfer, done or failed.
pub fn finish(conn: &Connection, id: i64) -> Result<()> {
    conn.execute("DELETE FROM mail_transfers WHERE id = ?1", params![id])?;
    Ok(())
}

/// The files of the transfers under way, so others left in their folder
/// can go.
pub fn staged_paths(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt =
        conn.prepare("SELECT staged_path FROM mail_transfers WHERE staged_path IS NOT NULL")?;
    let paths = stmt
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testutil;

    fn new(message: i64, folder: i64) -> NewTransfer {
        NewTransfer {
            source_message_id: message,
            source_account_id: 1,
            target_folder_id: folder,
            is_move: true,
            rfc_message_id: Some("a@example.com".into()),
            seen: true,
            flagged: false,
            received_at: Some(1_000),
        }
    }

    #[test]
    fn transfers_run_in_order_and_wait_after_a_failure() {
        let conn = testutil::conn();
        testutil::seed_account(&conn);
        conn.execute(
            "INSERT INTO folders (id, account_id, path) VALUES (2, 1, 'Other')",
            [],
        )
        .unwrap();
        let (_, first) = testutil::seed_message(&conn, "a@test.dev", "first", false);
        let (_, second) = testutil::seed_message(&conn, "b@test.dev", "second", false);

        let one = insert(&conn, &new(first, 2), 5).unwrap().unwrap();
        assert_eq!(
            insert(&conn, &new(first, 2), 6).unwrap(),
            None,
            "already under way"
        );
        let two = insert(&conn, &new(second, 2), 7).unwrap().unwrap();
        assert_eq!(next_due(&conn, 10).unwrap().map(|t| t.id), Some(one));
        let started = get(&conn, one).unwrap().unwrap();
        assert_eq!(started.stage, Stage::Fetch);
        assert!(started.is_move && started.seen && !started.flagged);

        retry_at(&conn, one, 100, "offline").unwrap();
        assert_eq!(next_due(&conn, 10).unwrap().map(|t| t.id), Some(two));
        assert_eq!(next_attempt_at(&conn).unwrap(), Some(0));
        finish(&conn, two).unwrap();
        assert_eq!(next_due(&conn, 10).unwrap(), None);
        assert_eq!(next_attempt_at(&conn).unwrap(), Some(100));
        assert_eq!(get(&conn, one).unwrap().unwrap().attempts, 1);

        advance(&conn, one, Stage::Upload, Some("/tmp/one.eml")).unwrap();
        let uploaded = next_due(&conn, 10).unwrap().unwrap();
        assert_eq!(uploaded.stage, Stage::Upload);
        assert_eq!(uploaded.attempts, 0, "a stage starts afresh");
        assert_eq!(uploaded.staged_path.as_deref(), Some("/tmp/one.eml"));
        advance(&conn, one, Stage::Remove, None).unwrap();
        assert_eq!(
            get(&conn, one).unwrap().unwrap().staged_path.as_deref(),
            Some("/tmp/one.eml"),
            "the file stays until the transfer ends"
        );
        assert_eq!(staged_paths(&conn).unwrap(), ["/tmp/one.eml"]);

        // The source going leaves the transfer to finish.
        conn.execute("DELETE FROM messages WHERE id = ?1", [first])
            .unwrap();
        assert_eq!(get(&conn, one).unwrap().unwrap().source_message_id, None);
    }
}
