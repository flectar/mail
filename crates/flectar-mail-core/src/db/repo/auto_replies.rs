//! Automatic replies as last read from or saved to each account's server
//! (#239).
use crate::error::Result;
use crate::models::AutoReply;
use rusqlite::{Connection, OptionalExtension, Row, params};

const COLUMNS: &str = "enabled, subject, text, starts_at, ends_at, contacts_only, domain_only";

fn reply(row: &Row<'_>, first: usize) -> rusqlite::Result<AutoReply> {
    Ok(AutoReply {
        enabled: row.get(first)?,
        subject: row.get(first + 1)?,
        text: row.get(first + 2)?,
        starts_at: row.get(first + 3)?,
        ends_at: row.get(first + 4)?,
        contacts_only: row.get(first + 5)?,
        domain_only: row.get(first + 6)?,
    })
}

/// Account `account_id`'s replies, if the app knows them.
pub fn get(conn: &Connection, account_id: i64) -> Result<Option<AutoReply>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM auto_replies WHERE account_id = ?1"),
            params![account_id],
            |row| reply(row, 0),
        )
        .optional()?)
}

/// Every account's replies the app knows, by account.
pub fn all(conn: &Connection) -> Result<Vec<(i64, AutoReply)>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT account_id, {COLUMNS} FROM auto_replies ORDER BY account_id"
    ))?;
    let rows = stmt.query_map([], |row| Ok((row.get(0)?, reply(row, 1)?)))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Keeps `reply` as account `account_id`'s, read from its server at `now`,
/// or saved to it by the app when `saved`. Whether that changed them.
pub fn store(
    conn: &Connection,
    account_id: i64,
    reply: &AutoReply,
    now: i64,
    saved: bool,
) -> Result<bool> {
    let changed = get(conn, account_id)?.as_ref() != Some(reply);
    conn.execute(
        "INSERT INTO auto_replies
           (account_id, enabled, subject, text, starts_at, ends_at, contacts_only, domain_only,
            saved_at, read_at)
         SELECT id, ?2, ?3, ?4, ?5, ?6, ?7, ?8, CASE WHEN ?10 THEN ?9 END, ?9
         FROM accounts WHERE id = ?1
         ON CONFLICT(account_id) DO UPDATE SET
           enabled = excluded.enabled, subject = excluded.subject, text = excluded.text,
           starts_at = excluded.starts_at, ends_at = excluded.ends_at,
           contacts_only = excluded.contacts_only, domain_only = excluded.domain_only,
           saved_at = COALESCE(excluded.saved_at, auto_replies.saved_at),
           read_at = excluded.read_at",
        params![
            account_id,
            reply.enabled,
            reply.subject,
            reply.text,
            reply.starts_at,
            reply.ends_at,
            reply.contacts_only,
            reply.domain_only,
            now,
            saved,
        ],
    )?;
    Ok(changed)
}

/// Whether the app saved account `account_id`'s replies, so it uses the
/// provider's permission to change mail settings.
pub fn uses_settings(conn: &Connection, account_id: i64) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM auto_replies
                        WHERE account_id = ?1 AND saved_at IS NOT NULL)",
        params![account_id],
        |row| row.get(0),
    )?)
}

/// Forgets that the app may change account `account_id`'s mail settings,
/// once the provider refused, so the next save asks the user again.
pub fn forget_settings(conn: &Connection, account_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE auto_replies SET saved_at = NULL WHERE account_id = ?1",
        params![account_id],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testutil;

    fn away() -> AutoReply {
        AutoReply {
            enabled: true,
            subject: "Out of office".into(),
            text: "Back on Monday.".into(),
            starts_at: Some(1_000),
            ends_at: Some(2_000),
            contacts_only: true,
            domain_only: false,
        }
    }

    #[test]
    fn replies_are_kept_per_account_with_what_the_app_saved() {
        let conn = testutil::conn();
        testutil::seed_account(&conn);
        assert_eq!(get(&conn, 1).unwrap(), None);
        // Read from the server: the app hasn't used the settings permission.
        assert!(store(&conn, 1, &away(), 10, false).unwrap());
        assert_eq!(get(&conn, 1).unwrap(), Some(away()));
        assert!(!uses_settings(&conn, 1).unwrap());
        assert!(!store(&conn, 1, &away(), 20, false).unwrap(), "unchanged");

        // Saved by the app, then read back changed in webmail.
        let off = AutoReply {
            enabled: false,
            ..away()
        };
        assert!(store(&conn, 1, &off, 30, true).unwrap());
        assert!(uses_settings(&conn, 1).unwrap());
        assert!(store(&conn, 1, &away(), 40, false).unwrap());
        assert!(uses_settings(&conn, 1).unwrap(), "a read keeps the save");
        assert_eq!(all(&conn).unwrap(), [(1, away())]);

        forget_settings(&conn, 1).unwrap();
        assert!(!uses_settings(&conn, 1).unwrap());
        // An account that doesn't exist keeps nothing.
        store(&conn, 4242, &away(), 50, true).unwrap();
        assert_eq!(get(&conn, 4242).unwrap(), None);
    }
}
