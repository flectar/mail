//! The account that holds mail kept only on this device: mail the user
//! imported, in folders that exist nowhere else. It has no address, server,
//! sign-in, or settings, so the rest of [`super::accounts`] leaves it out.

use super::folders;
use crate::error::Result;
use crate::models::{now_ms, roles};
use rusqlite::{Connection, OptionalExtension, params};

/// The account's id, once mail has been kept on this device.
pub fn id(conn: &Connection) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT id FROM accounts WHERE mail_protocol = 'local'",
            [],
            |row| row.get(0),
        )
        .optional()?)
}

/// The account's id, created with its Trash folder if it doesn't exist yet.
/// Run it in the transaction that first puts mail in it.
pub fn ensure(conn: &Connection) -> Result<i64> {
    if let Some(id) = id(conn)? {
        return Ok(id);
    }
    conn.execute(
        "INSERT INTO accounts (mail_protocol, created_at) VALUES ('local', ?1)",
        params![now_ms()],
    )?;
    let id = conn.last_insert_rowid();
    folders::upsert(
        conn,
        id,
        "Trash",
        Some(folders::DEVICE_DELIMITER),
        Some(roles::TRASH),
    )?;
    Ok(id)
}

/// How much mail the account holds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Contents {
    pub messages: u64,
    pub bytes: u64,
}

pub fn contents(conn: &Connection, id: i64) -> Result<Contents> {
    Ok(conn.query_row(
        "SELECT COUNT(*), COALESCE(SUM(size), 0) FROM messages WHERE account_id = ?1",
        params![id],
        |row| {
            Ok(Contents {
                messages: row.get::<_, i64>(0)?.max(0) as u64,
                bytes: row.get::<_, i64>(1)?.max(0) as u64,
            })
        },
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_account_holds_mail_kept_on_this_device() {
        let conn = crate::db::testutil::conn();
        assert_eq!(id(&conn).unwrap(), None);

        let created = ensure(&conn).unwrap();
        assert_eq!(ensure(&conn).unwrap(), created);
        assert_eq!(id(&conn).unwrap(), Some(created));
        let trash = folders::by_role(&conn, created, roles::TRASH)
            .unwrap()
            .unwrap();
        assert_eq!(trash.path, "Trash");
        assert_eq!(contents(&conn, created).unwrap(), Contents::default());

        // It isn't an account the rest of the app lists, configures, or
        // orders.
        assert!(super::super::accounts::list(&conn).unwrap().is_empty());
        assert!(
            super::super::accounts::list_configs(&conn)
                .unwrap()
                .is_empty()
        );
        assert!(
            super::super::accounts::get(&conn, created)
                .unwrap()
                .is_none()
        );
        assert!(
            super::super::accounts::get_config(&conn, created)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            super::super::accounts::protocol(&conn, created).unwrap(),
            Some(crate::models::MailProtocol::Local)
        );
        // A second one, or one with an address, is refused.
        assert!(
            conn.execute(
                "INSERT INTO accounts (mail_protocol, created_at) VALUES ('local', 0)",
                [],
            )
            .is_err()
        );
        assert!(
            conn.execute(
                "INSERT INTO accounts (email, mail_protocol, created_at)
                 VALUES ('me@example.test', 'local', 0)",
                [],
            )
            .is_err()
        );
    }
}
