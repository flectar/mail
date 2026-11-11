//! Senders whose mail goes to Spam (#227). Sync turns away what they send to
//! an inbox (see `crate::turn_away`), and blocking one moves what is in an
//! inbox now.
use crate::error::Result;
use crate::models::BlockedSender;
use rusqlite::{Connection, params};

/// Mail in an inbox, sent by someone else, in an account with a Spam folder
/// to move it to. `m` is the message.
const MOVABLE_INBOX_MAIL: &str = "m.is_draft = 0 AND m.is_outgoing = 0
     AND EXISTS (SELECT 1 FROM folders s WHERE s.account_id = m.account_id AND s.role = 'spam')
     AND (EXISTS (SELECT 1 FROM folders f WHERE f.id = m.folder_id AND f.role = 'inbox')
          OR EXISTS (SELECT 1 FROM message_folders mf JOIN folders f ON f.id = mf.folder_id
                     WHERE mf.message_id = m.id AND f.role = 'inbox'))";

/// The form an address is kept and compared in, as `LOWER(from_addr)`
/// gives it. None for something that isn't an address.
pub fn normalized(address: &str) -> Option<String> {
    let address = address.trim().to_ascii_lowercase();
    (address.contains('@') && !address.contains(char::is_whitespace)).then_some(address)
}

/// Block `address`. False when it already was.
pub fn block(conn: &Connection, address: &str, now: i64) -> Result<bool> {
    Ok(conn.execute(
        "INSERT INTO blocked_senders (address, blocked_at) VALUES (?1, ?2)
         ON CONFLICT(address) DO NOTHING",
        params![address, now],
    )? == 1)
}

/// Stop blocking `address`. False when it wasn't blocked.
pub fn unblock(conn: &Connection, address: &str) -> Result<bool> {
    Ok(conn.execute(
        "DELETE FROM blocked_senders WHERE address = ?1",
        params![address],
    )? == 1)
}

pub fn is_blocked(conn: &Connection, address: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM blocked_senders WHERE address = ?1)",
        params![address],
        |r| r.get(0),
    )?)
}

/// Every blocked address, alphabetically.
pub fn list(conn: &Connection) -> Result<Vec<BlockedSender>> {
    let mut stmt =
        conn.prepare("SELECT address, blocked_at FROM blocked_senders ORDER BY address")?;
    let rows = stmt.query_map([], |r| {
        Ok(BlockedSender {
            address: r.get(0)?,
            blocked_at: r.get(1)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// The mail from `address` in an inbox now that blocking it moves to Spam,
/// newest first.
pub fn inbox_messages(conn: &Connection, address: &str) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT m.id FROM messages m
         WHERE LOWER(m.from_addr) = ?1 AND {MOVABLE_INBOX_MAIL}
         ORDER BY m.date DESC, m.id DESC
         LIMIT 10000"
    ))?;
    let rows = stmt.query_map(params![address], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Whether sync should turn `message_id` away: mail it just stored in an
/// inbox from a blocked sender, which can move to Spam.
pub fn turned_away(conn: &Connection, message_id: i64) -> Result<bool> {
    Ok(conn.query_row(
        &format!(
            "SELECT EXISTS (SELECT 1 FROM messages m
                 JOIN blocked_senders b ON b.address = LOWER(m.from_addr)
                 WHERE m.id = ?1 AND {MOVABLE_INBOX_MAIL})"
        ),
        params![message_id],
        |r| r.get(0),
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testutil;

    fn with_spam_folder(conn: &Connection) {
        conn.execute(
            "INSERT INTO folders (id, account_id, path, role) VALUES (2, 1, 'Junk', 'spam')",
            [],
        )
        .unwrap();
    }

    #[test]
    fn addresses_are_kept_lowercase_and_once() {
        let conn = testutil::conn();
        assert_eq!(
            normalized("  News@Example.COM ").as_deref(),
            Some("news@example.com")
        );
        assert_eq!(normalized("not an address"), None);
        assert_eq!(normalized("nobody"), None);

        assert!(block(&conn, "news@example.com", 10).unwrap());
        assert!(!block(&conn, "news@example.com", 20).unwrap());
        assert!(block(&conn, "ads@example.com", 30).unwrap());
        assert!(is_blocked(&conn, "news@example.com").unwrap());
        assert_eq!(
            list(&conn).unwrap(),
            vec![
                BlockedSender {
                    address: "ads@example.com".into(),
                    blocked_at: 30
                },
                BlockedSender {
                    address: "news@example.com".into(),
                    blocked_at: 10
                },
            ]
        );
        assert!(unblock(&conn, "news@example.com").unwrap());
        assert!(!unblock(&conn, "news@example.com").unwrap());
        assert!(!is_blocked(&conn, "news@example.com").unwrap());
    }

    #[test]
    fn only_a_blocked_senders_inbox_mail_is_turned_away() {
        let conn = testutil::conn();
        testutil::seed_account(&conn);
        let (_, blocked) = testutil::seed_message(&conn, "Ads@Example.com", "Sale", true);
        let (_, other) = testutil::seed_message(&conn, "friend@example.com", "Hi", false);
        block(&conn, "ads@example.com", 0).unwrap();

        // Without a Spam folder there is nowhere to move it.
        assert!(!turned_away(&conn, blocked).unwrap());
        assert!(inbox_messages(&conn, "ads@example.com").unwrap().is_empty());

        with_spam_folder(&conn);
        assert!(turned_away(&conn, blocked).unwrap());
        assert!(!turned_away(&conn, other).unwrap());
        assert_eq!(
            inbox_messages(&conn, "ads@example.com").unwrap(),
            vec![blocked]
        );

        // Mail outside the inbox, and one's own, stays where it is.
        conn.execute("UPDATE messages SET folder_id = 2 WHERE id = ?1", [blocked])
            .unwrap();
        assert!(!turned_away(&conn, blocked).unwrap());
        conn.execute(
            "UPDATE messages SET folder_id = 1, is_outgoing = 1 WHERE id = ?1",
            [blocked],
        )
        .unwrap();
        assert!(!turned_away(&conn, blocked).unwrap());

        // Mail with several mailboxes counts as in the inbox through any.
        conn.execute(
            "INSERT INTO folders (id, account_id, path, role) VALUES (3, 1, 'All Mail', 'all')",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE messages SET folder_id = 3, is_outgoing = 0 WHERE id = ?1",
            [blocked],
        )
        .unwrap();
        assert!(!turned_away(&conn, blocked).unwrap());
        conn.execute(
            "INSERT INTO message_folders (message_id, folder_id) VALUES (?1, 1)",
            [blocked],
        )
        .unwrap();
        assert!(turned_away(&conn, blocked).unwrap());
    }
}
