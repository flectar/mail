use crate::error::Result;
use crate::models::now_ms;
use rusqlite::{Connection, OptionalExtension, params};

#[derive(Debug, Clone, Default)]
pub struct SyncState {
    pub sync_cursor: Option<String>,
    pub history_id: Option<String>,
    pub history_page_token: Option<String>,
    pub generation: i64,
    pub full_started_at: Option<i64>,
    pub backfill_done: bool,
}

pub fn state(conn: &Connection, account_id: i64) -> Result<SyncState> {
    Ok(conn
        .query_row(
            "SELECT sync_cursor, history_id, history_page_token, generation,
                    full_started_at, backfill_done
             FROM gmail_sync_state WHERE account_id = ?1",
            params![account_id],
            |row| {
                Ok(SyncState {
                    sync_cursor: row.get(0)?,
                    history_id: row.get(1)?,
                    history_page_token: row.get(2)?,
                    generation: row.get(3)?,
                    full_started_at: row.get(4)?,
                    backfill_done: row.get::<_, i64>(5)? != 0,
                })
            },
        )
        .optional()?
        .unwrap_or_default())
}

/// Start a new authoritative scan. The profile watermark is captured before
/// message listing so history reconciliation closes the race with live mail.
pub fn begin_full_sync(conn: &Connection, account_id: i64, history_id: &str) -> Result<i64> {
    let previous = state(conn, account_id)?;
    let generation = previous.generation.saturating_add(1).max(1);
    conn.execute(
        "INSERT INTO gmail_sync_state (
             account_id, sync_cursor, history_id, history_page_token,
             generation, full_started_at, backfill_done, updated_at
         ) VALUES (?1, NULL, ?2, NULL, ?3, ?4, 0, ?4)
         ON CONFLICT(account_id) DO UPDATE SET
             sync_cursor = NULL,
             history_id = excluded.history_id,
             history_page_token = NULL,
             generation = excluded.generation,
             full_started_at = excluded.full_started_at,
             backfill_done = 0,
             updated_at = excluded.updated_at",
        params![account_id, history_id, generation, now_ms()],
    )?;
    Ok(generation)
}

pub fn checkpoint_full_page(
    conn: &Connection,
    account_id: i64,
    next_cursor: Option<&str>,
) -> Result<()> {
    conn.execute(
        "UPDATE gmail_sync_state
         SET sync_cursor = ?2, backfill_done = (?2 IS NULL), updated_at = ?3
         WHERE account_id = ?1",
        params![account_id, next_cursor, now_ms()],
    )?;
    Ok(())
}

pub fn checkpoint_history_page(
    conn: &Connection,
    account_id: i64,
    next_page_token: Option<&str>,
    completed_history_id: Option<&str>,
) -> Result<()> {
    conn.execute(
        "UPDATE gmail_sync_state SET
             history_page_token = ?2,
             history_id = COALESCE(?3, history_id),
             updated_at = ?4
         WHERE account_id = ?1",
        params![account_id, next_page_token, completed_history_id, now_ms()],
    )?;
    Ok(())
}

/// The History API returns 404 when its start id has aged out. Keep local data
/// available, but restart an authoritative generation so stale rows can be
/// removed only after every replacement page has committed.
pub fn expire_history(conn: &Connection, account_id: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO gmail_sync_state (account_id, updated_at)
         VALUES (?1, ?2)
         ON CONFLICT(account_id) DO UPDATE SET
             sync_cursor = NULL, history_id = NULL, history_page_token = NULL,
             generation = 0, backfill_done = 0, full_started_at = NULL,
             count_history_id = NULL, updated_at = excluded.updated_at",
        params![account_id, now_ms()],
    )?;
    Ok(())
}

/// The Inbox folder and the history id to read its unread count at, when
/// the sync has caught up with the whole mail history, the mailbox changed
/// since the count was last read, and no action that changes counts is on
/// its way to Gmail.
pub fn inbox_count_due(conn: &Connection, account_id: i64) -> Result<Option<(i64, String)>> {
    let due = conn
        .query_row(
            &format!(
                "SELECT f.id, s.history_id FROM gmail_sync_state s
                 JOIN folders f ON f.account_id = s.account_id AND f.role = 'inbox'
                 WHERE s.account_id = ?1 AND s.backfill_done AND s.history_page_token IS NULL
                   AND s.history_id IS NOT NULL
                   AND s.history_id IS NOT s.count_history_id
                   AND NOT {}",
                super::actions::counts_settling_sql("?1")
            ),
            params![account_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    Ok(due)
}

/// Records the Inbox's unread count on Gmail, read at `history_id`; returns
/// whether what is only on Gmail changed.
pub fn set_inbox_count(
    conn: &Connection,
    account_id: i64,
    inbox: i64,
    history_id: &str,
    unread: i64,
) -> Result<bool> {
    let changed = super::folders::set_server_unseen(conn, inbox, unread)?;
    conn.execute(
        "UPDATE gmail_sync_state SET count_history_id = ?2 WHERE account_id = ?1",
        params![account_id, history_id],
    )?;
    Ok(changed)
}

pub fn set_message_folders(conn: &Connection, message_id: i64, folder_ids: &[i64]) -> Result<()> {
    conn.execute(
        "DELETE FROM message_folders WHERE message_id = ?1",
        params![message_id],
    )?;
    for folder_id in folder_ids {
        conn.execute(
            "INSERT OR IGNORE INTO message_folders (message_id, folder_id) VALUES (?1, ?2)",
            params![message_id, folder_id],
        )?;
    }
    Ok(())
}

pub fn provider_label_for_local(
    conn: &Connection,
    account_id: i64,
    local_label_id: i64,
) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT provider_id FROM gmail_labels
             WHERE account_id = ?1 AND local_label_id = ?2 LIMIT 1",
            params![account_id, local_label_id],
            |row| row.get(0),
        )
        .optional()?)
}

pub fn provider_label_for_folder(
    conn: &Connection,
    account_id: i64,
    folder_id: i64,
) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT provider_id FROM gmail_labels
             WHERE account_id = ?1 AND folder_id = ?2 AND kind = 'user' LIMIT 1",
            params![account_id, folder_id],
            |row| row.get(0),
        )
        .optional()?)
}

pub fn set_draft_ids(
    conn: &Connection,
    account_id: i64,
    gmail_message_id: &str,
    gmail_draft_id: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE messages SET gmail_draft_id = ?3
         WHERE account_id = ?1 AND gm_msgid = ?2",
        params![account_id, gmail_message_id, gmail_draft_id],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testutil;

    #[test]
    fn full_and_history_checkpoints_are_durable() {
        let conn = testutil::conn();
        conn.execute(
            "INSERT INTO accounts (id, email, provider, auth_kind, username,
             incoming_host, incoming_port, smtp_host, smtp_port, created_at, mail_protocol, settings_json)
             VALUES (1, 'me@gmail.com', 'gmail', 'oauth2', 'me@gmail.com',
                     'unused', 993, 'unused', 465, 0, 'gmail', '{\"connection\":{\"incomingSecurity\":\"tls\",\"smtpSecurity\":\"tls\"}}')",
            [],
        )
        .unwrap();
        let generation = begin_full_sync(&conn, 1, "100").unwrap();
        assert_eq!(generation, 1);
        checkpoint_full_page(&conn, 1, Some("page-2")).unwrap();
        let current = state(&conn, 1).unwrap();
        assert_eq!(current.sync_cursor.as_deref(), Some("page-2"));
        assert!(!current.backfill_done);

        checkpoint_full_page(&conn, 1, None).unwrap();
        checkpoint_history_page(&conn, 1, Some("history-2"), None).unwrap();
        let current = state(&conn, 1).unwrap();
        assert!(current.backfill_done);
        assert_eq!(current.history_id.as_deref(), Some("100"));
        assert_eq!(current.history_page_token.as_deref(), Some("history-2"));

        checkpoint_history_page(&conn, 1, None, Some("200")).unwrap();
        let current = state(&conn, 1).unwrap();
        assert_eq!(current.history_id.as_deref(), Some("200"));
        assert!(current.history_page_token.is_none());

        expire_history(&conn, 1).unwrap();
        // A History API request that was already in flight can still finish
        // after the reset. Its stale checkpoint must not prevent the next
        // cycle from recognizing that a new full generation is required.
        checkpoint_history_page(&conn, 1, None, Some("stale-history")).unwrap();
        let expired = state(&conn, 1).unwrap();
        assert_eq!(expired.generation, 0);
        assert!(!expired.backfill_done);
    }

    #[test]
    fn message_folder_memberships_replace_atomically() {
        let conn = testutil::conn();
        testutil::seed_account(&conn);
        let (_, message_id) = testutil::seed_message(&conn, "a@test.dev", "one", false);
        conn.execute(
            "INSERT INTO folders (id, account_id, path, role)
             VALUES (2, 1, 'Archive', 'archive')",
            [],
        )
        .unwrap();
        set_message_folders(&conn, message_id, &[1, 2]).unwrap();
        set_message_folders(&conn, message_id, &[2]).unwrap();
        let memberships: Vec<i64> = conn
            .prepare("SELECT folder_id FROM message_folders WHERE message_id = ?1")
            .unwrap()
            .query_map(params![message_id], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(memberships, vec![2]);
    }

    #[test]
    fn the_inbox_count_is_read_once_the_history_is_here_and_changed() {
        let conn = testutil::conn();
        testutil::seed_account(&conn);
        // One of the Inbox's unread messages is kept here.
        testutil::seed_message(&conn, "a@test.dev", "Kept", false);
        let due = |conn: &Connection| inbox_count_due(conn, 1).unwrap();

        begin_full_sync(&conn, 1, "100").unwrap();
        assert_eq!(due(&conn), None, "history still downloading");
        checkpoint_full_page(&conn, 1, None).unwrap();
        checkpoint_history_page(&conn, 1, Some("history-2"), None).unwrap();
        assert_eq!(due(&conn), None, "history pages left");
        checkpoint_history_page(&conn, 1, None, Some("200")).unwrap();
        assert_eq!(due(&conn), Some((1, "200".to_owned())));

        set_inbox_count(&conn, 1, 1, "200", 5).unwrap();
        assert_eq!(
            super::super::folders::server_only_unseen(&conn, &[1]).unwrap(),
            Some(4)
        );
        assert_eq!(due(&conn), None, "nothing changed since");
        checkpoint_history_page(&conn, 1, None, Some("300")).unwrap();
        assert_eq!(due(&conn), Some((1, "300".to_owned())));

        // Not while an action that changes counts is on its way.
        conn.execute(
            "INSERT INTO pending_actions (account_id, kind, created_at) VALUES (1, 'archive', 0)",
            [],
        )
        .unwrap();
        assert_eq!(due(&conn), None);
        conn.execute("DELETE FROM pending_actions", []).unwrap();

        // A full scan reads it again once done, even unchanged.
        set_inbox_count(&conn, 1, 1, "300", 5).unwrap();
        expire_history(&conn, 1).unwrap();
        begin_full_sync(&conn, 1, "300").unwrap();
        checkpoint_full_page(&conn, 1, None).unwrap();
        assert_eq!(due(&conn), Some((1, "300".to_owned())));
    }
}
