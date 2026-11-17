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

/// Where replies the app keeps come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// The app saved them to the server.
    Saved,
    /// The app read them from the server, in a read that began when it had
    /// last saved them at `saved_at`, if ever.
    Read { saved_at: Option<i64> },
}

/// Keeps `reply` as account `account_id`'s at `now`. A read that began
/// before the app saved them since keeps nothing, as the server may have
/// answered it with the replies from before. Whether that changed them.
pub fn store(
    conn: &Connection,
    account_id: i64,
    reply: &AutoReply,
    now: i64,
    origin: Origin,
) -> Result<bool> {
    let before = get(conn, account_id)?;
    let (saved, read_since) = match origin {
        Origin::Saved => (true, None),
        Origin::Read { saved_at } => (false, saved_at),
    };
    let stored = conn.execute(
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
           read_at = excluded.read_at
         WHERE ?10 OR auto_replies.saved_at IS ?11",
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
            read_since,
        ],
    )?;
    Ok(stored > 0 && before.as_ref() != Some(reply))
}

/// When the app last saved account `account_id`'s replies to its server,
/// if it has.
pub fn saved_at(conn: &Connection, account_id: i64) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT saved_at FROM auto_replies WHERE account_id = ?1",
            params![account_id],
            |row| row.get(0),
        )
        .optional()?
        .flatten())
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
        let read = Origin::Read { saved_at: None };
        // Read from the server: the app hasn't used the settings permission.
        assert!(store(&conn, 1, &away(), 10, read).unwrap());
        assert_eq!(get(&conn, 1).unwrap(), Some(away()));
        assert!(!uses_settings(&conn, 1).unwrap());
        assert!(!store(&conn, 1, &away(), 20, read).unwrap(), "unchanged");

        // Saved by the app, then read back changed in webmail.
        let off = AutoReply {
            enabled: false,
            ..away()
        };
        assert!(store(&conn, 1, &off, 30, Origin::Saved).unwrap());
        assert!(uses_settings(&conn, 1).unwrap());
        assert_eq!(saved_at(&conn, 1).unwrap(), Some(30));
        let read = Origin::Read { saved_at: Some(30) };
        assert!(store(&conn, 1, &away(), 40, read).unwrap());
        assert!(uses_settings(&conn, 1).unwrap(), "a read keeps the save");
        assert_eq!(all(&conn).unwrap(), [(1, away())]);

        forget_settings(&conn, 1).unwrap();
        assert!(!uses_settings(&conn, 1).unwrap());
        // An account that doesn't exist keeps nothing.
        store(&conn, 4242, &away(), 50, Origin::Saved).unwrap();
        assert_eq!(get(&conn, 4242).unwrap(), None);
    }

    #[test]
    fn a_read_that_began_before_a_save_keeps_the_save() {
        let conn = testutil::conn();
        testutil::seed_account(&conn);
        let off = AutoReply {
            enabled: false,
            ..away()
        };
        // A sync began reading before the app had saved anything, and the
        // user saved while it waited for the server.
        let began = saved_at(&conn, 1).unwrap();
        assert!(store(&conn, 1, &away(), 10, Origin::Saved).unwrap());
        assert!(!store(&conn, 1, &off, 11, Origin::Read { saved_at: began }).unwrap());
        assert_eq!(get(&conn, 1).unwrap(), Some(away()));
        // So did one that began after an older save.
        assert!(store(&conn, 1, &off, 20, Origin::Saved).unwrap());
        assert!(!store(&conn, 1, &away(), 21, Origin::Read { saved_at: Some(10) }).unwrap());
        assert_eq!(get(&conn, 1).unwrap(), Some(off));
    }
}
