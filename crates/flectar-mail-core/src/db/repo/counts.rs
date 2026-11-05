//! Exact sidebar badges, sharing the predicate shapes of `threads::list`.
//! Kept out of threads.rs so the two evolve independently.

use crate::error::Result;
use crate::models::{MailGrouping, MailboxBadgeCounts};
use rusqlite::Connection;
use std::collections::HashMap;

const INBOX_BADGE_SQL: &str = "SELECT t.account_id, COUNT(*)
         FROM threads t
         LEFT JOIN snoozes s ON s.thread_id = t.id
         WHERE t.unread_count > 0
           AND s.thread_id IS NULL
           AND (EXISTS (
                SELECT 1 FROM messages m
                WHERE m.thread_id = t.id
                  AND m.folder_id IN (SELECT id FROM folders WHERE role = 'inbox')
           ) OR EXISTS (
                SELECT 1 FROM messages m
                JOIN accounts ma ON ma.id = m.account_id AND ma.provider = 'gmail'
                JOIN message_folders mf ON mf.message_id = m.id
                JOIN folders f ON f.id = mf.folder_id
                WHERE m.thread_id = t.id AND f.role = 'inbox'
           ))
         GROUP BY t.account_id";

/// Unread messages in an inbox outside snoozed conversations, for lists that
/// show each message on its own. Mirrors the message-level inbox filter of
/// `threads::list_messages`.
const INBOX_MESSAGE_BADGE_SQL: &str = "SELECT m.account_id, COUNT(*)
         FROM messages m
         JOIN accounts a ON a.id = m.account_id
         LEFT JOIN snoozes s ON s.thread_id = m.thread_id
         WHERE m.is_read = 0
           AND m.is_draft = 0
           AND m.thread_id IS NOT NULL
           AND s.thread_id IS NULL
           AND (m.folder_id IN (SELECT id FROM folders WHERE role = 'inbox')
                OR (a.provider = 'gmail' AND EXISTS (
                     SELECT 1 FROM message_folders mf
                     JOIN folders f ON f.id = mf.folder_id
                     WHERE mf.message_id = m.id AND f.role = 'inbox'
                )))
         GROUP BY m.account_id";

/// Query-plan details for the inbox badge path used by `mailbox_badge_counts`.
pub fn inbox_badge_query_plan(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {INBOX_BADGE_SQL}"))?;
    Ok(stmt
        .query_map([], |row| row.get(3))?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Every account's drafts, as conversations or messages. Draft folders are
/// normally tiny. Starting from those folders uses the existing folder and
/// message indexes and avoids inspecting every inbox row; `CROSS JOIN` keeps
/// the folders the outer loop.
fn drafts_sql(messages: bool) -> String {
    let counted = if messages { "message_id" } else { "thread_id" };
    format!(
        "SELECT account_id, COUNT(DISTINCT {counted})
         FROM (
             SELECT m.account_id, m.thread_id, m.id AS message_id
             FROM folders f
             CROSS JOIN messages m ON m.folder_id = f.id
             WHERE f.role = 'drafts' AND m.is_draft = 1 AND m.thread_id IS NOT NULL
             UNION ALL
             SELECT m.account_id, m.thread_id, m.id AS message_id
             FROM folders f
             CROSS JOIN message_folders mf ON mf.folder_id = f.id
             CROSS JOIN messages m ON m.id = mf.message_id
             JOIN accounts a ON a.id = m.account_id AND a.provider = 'gmail'
             WHERE f.role = 'drafts' AND m.is_draft = 1 AND m.thread_id IS NOT NULL
         )
         GROUP BY account_id"
    )
}

/// Return the native sidebar's badges for every account in one grouped scan,
/// so a live refresh does not grow with the number of accounts. Badges count
/// what the mail list shows: conversations, or messages.
pub fn mailbox_badge_counts(
    conn: &Connection,
    grouping: MailGrouping,
) -> Result<Vec<MailboxBadgeCounts>> {
    let messages = grouping == MailGrouping::Messages;
    let mut counts = conn
        .prepare("SELECT id FROM accounts ORDER BY sort_order, id")?
        .query_map([], |row| {
            Ok(MailboxBadgeCounts {
                account_id: row.get(0)?,
                inbox: 0,
                starred: 0,
                drafts: 0,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let positions = counts
        .iter()
        .enumerate()
        .map(|(index, counts)| (counts.account_id, index))
        .collect::<HashMap<_, _>>();

    // The inbox predicate mirrors `Q::inbox`, but grouping by account returns
    // every native sidebar section without repeating the scan per account.
    let mut inbox = conn.prepare(if messages {
        INBOX_MESSAGE_BADGE_SQL
    } else {
        INBOX_BADGE_SQL
    })?;
    for row in inbox.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))? {
        let (account_id, value) = row?;
        if let Some(index) = positions.get(&account_id) {
            counts[*index].inbox = value;
        }
    }

    let mut starred = conn.prepare(if messages {
        "SELECT account_id, COUNT(*) FROM messages
         WHERE is_read = 0 AND is_draft = 0 AND is_starred = 1 AND thread_id IS NOT NULL
         GROUP BY account_id"
    } else {
        "SELECT account_id, COUNT(*) FROM threads
         WHERE unread_count > 0 AND starred_count > 0
         GROUP BY account_id"
    })?;
    for row in starred.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))? {
        let (account_id, value) = row?;
        if let Some(index) = positions.get(&account_id) {
            counts[*index].starred = value;
        }
    }

    let mut drafts = conn.prepare(&drafts_sql(messages))?;
    for row in drafts.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))? {
        let (account_id, value) = row?;
        if let Some(index) = positions.get(&account_id) {
            counts[*index].drafts = value;
        }
    }

    Ok(counts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    fn test_db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::run(&mut conn).unwrap();
        conn.execute(
            "INSERT INTO accounts (id, email, provider, auth_kind, username,
             incoming_host, incoming_port, smtp_host, smtp_port, created_at, mail_protocol, settings_json)
             VALUES (1,'t@x.com','generic','password','t','h',993,'h',587,0,'imap','{\"connection\":{\"incomingSecurity\":\"tls\",\"smtpSecurity\":\"tls\"}}')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO folders (id, account_id, path, role) VALUES (1,1,'INBOX','inbox')",
            [],
        )
        .unwrap();
        conn
    }

    fn seed_thread(conn: &Connection, id: i64, unread: i64, automated: bool) {
        conn.execute(
            "INSERT INTO threads (id, account_id, subject_norm, unread_count, last_message_at)
             VALUES (?1, 1, 'subj', ?2, 1000)",
            params![id, unread],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO messages (thread_id, account_id, folder_id, uid, message_id, subject,
             from_addr, date, is_read, is_automated, is_draft, is_outgoing)
             VALUES (?1, 1, 1, ?1, 'mid-' || ?1, 'subj', 'a@b.c', 1000, ?2, ?3, 0, 0)",
            params![id, (unread == 0) as i64, automated as i64],
        )
        .unwrap();
    }

    #[test]
    fn native_sidebar_badges_follow_read_and_draft_mutations() {
        let conn = test_db();
        conn.execute(
            "INSERT INTO accounts (id, email, provider, auth_kind, username,
             incoming_host, incoming_port, smtp_host, smtp_port, created_at, sort_order, mail_protocol, settings_json)
             VALUES (2,'two@x.com','generic','password','two','h',993,'h',587,0,1,'imap','{\"connection\":{\"incomingSecurity\":\"tls\",\"smtpSecurity\":\"tls\"}}')",
            [],
        )
        .unwrap();
        seed_thread(&conn, 1, 1, false);
        conn.execute("UPDATE threads SET starred_count = 1 WHERE id = 1", [])
            .unwrap();
        conn.execute(
            "INSERT INTO folders (id, account_id, path, role)
             VALUES (2,1,'Drafts','drafts')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO threads (id, account_id, subject_norm, unread_count, last_message_at)
             VALUES (2,1,'draft',0,2000)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO messages (thread_id, account_id, folder_id, subject, from_addr,
             date, is_read, is_draft, is_outgoing)
             VALUES (2,1,2,'draft','t@x.com',2000,1,1,1)",
            [],
        )
        .unwrap();

        let badges = mailbox_badge_counts(&conn, MailGrouping::Conversations).unwrap();
        assert_eq!(
            badges,
            vec![
                MailboxBadgeCounts {
                    account_id: 1,
                    inbox: 1,
                    starred: 1,
                    drafts: 1,
                },
                MailboxBadgeCounts {
                    account_id: 2,
                    inbox: 0,
                    starred: 0,
                    drafts: 0,
                },
            ]
        );

        conn.execute("UPDATE threads SET unread_count = 0 WHERE id = 1", [])
            .unwrap();
        let badges = mailbox_badge_counts(&conn, MailGrouping::Conversations).unwrap();
        assert_eq!(badges[0].inbox, 0);
        assert_eq!(badges[0].starred, 0);
        assert_eq!(badges[0].drafts, 1);
    }

    #[test]
    fn native_sidebar_badges_count_gmail_links_once_and_exclude_snoozed_inbox() {
        let conn = test_db();
        conn.execute(
            "INSERT INTO accounts (id, email, provider, auth_kind, username,
             incoming_host, incoming_port, smtp_host, smtp_port, created_at, sort_order, mail_protocol, settings_json)
             VALUES (2,'gmail@x.com','gmail','oauth2','gmail','h',993,'h',587,0,1,'gmail','{\"connection\":{\"incomingSecurity\":\"tls\",\"smtpSecurity\":\"tls\"}}')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO folders (id, account_id, path, role) VALUES
             (2,2,'All Mail','all'), (3,2,'INBOX','inbox'), (4,2,'Drafts','drafts')",
            [],
        )
        .unwrap();

        seed_thread(&conn, 1, 1, false);
        seed_thread(&conn, 2, 1, false);
        conn.execute("UPDATE threads SET starred_count = 1 WHERE id = 1", [])
            .unwrap();
        conn.execute(
            "INSERT INTO snoozes (thread_id, account_id, wake_at) VALUES (1,1,9999)",
            [],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO threads (id, account_id, subject_norm, unread_count, starred_count,
             last_message_at) VALUES (3,2,'gmail',2,1,1000)",
            [],
        )
        .unwrap();
        for date in [1000, 1001] {
            conn.execute(
                "INSERT INTO messages (thread_id, account_id, folder_id, subject, date)
                 VALUES (3,2,2,'gmail',?1)",
                [date],
            )
            .unwrap();
            let message_id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO message_folders (message_id, folder_id) VALUES (?1,3)",
                [message_id],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO threads (id, account_id, subject_norm, unread_count, last_message_at)
             VALUES (4,2,'draft',0,1002)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO messages (thread_id, account_id, folder_id, subject, date, is_draft)
             VALUES (4,2,4,'draft',1002,1)",
            [],
        )
        .unwrap();

        assert_eq!(
            mailbox_badge_counts(&conn, MailGrouping::Conversations).unwrap(),
            vec![
                MailboxBadgeCounts {
                    account_id: 1,
                    inbox: 1,
                    starred: 1,
                    drafts: 0,
                },
                MailboxBadgeCounts {
                    account_id: 2,
                    inbox: 1,
                    starred: 1,
                    drafts: 1,
                },
            ]
        );
    }

    #[test]
    fn message_badges_count_unread_messages() {
        let conn = crate::db::testutil::conn();
        crate::db::testutil::seed_account(&conn);
        let (thread_id, _) =
            crate::db::testutil::seed_message(&conn, "a@b.dev", "two unread", false);
        let (own_thread, reply) =
            crate::db::testutil::seed_message(&conn, "a@b.dev", "re: two unread", false);
        conn.execute(
            "UPDATE messages SET thread_id = ?1, is_starred = 1, uid = 10000 + id WHERE id = ?2",
            params![thread_id, reply],
        )
        .unwrap();
        conn.execute("DELETE FROM threads WHERE id = ?1", [own_thread])
            .unwrap();
        crate::db::repo::threads::recompute(&conn, thread_id).unwrap();

        let conversations = mailbox_badge_counts(&conn, MailGrouping::Conversations).unwrap();
        assert_eq!((conversations[0].inbox, conversations[0].starred), (1, 1));
        let messages = mailbox_badge_counts(&conn, MailGrouping::Messages).unwrap();
        assert_eq!((messages[0].inbox, messages[0].starred), (2, 1));
    }

    #[test]
    fn drafts_are_counted_from_the_drafts_folders() {
        let conn = test_db();
        for messages in [false, true] {
            let plan = conn
                .prepare(&format!("EXPLAIN QUERY PLAN {}", drafts_sql(messages)))
                .unwrap()
                .query_map([], |row| row.get::<_, String>(3))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
                .join("\n");
            assert!(plan.contains("SCAN f"), "{plan}");
            assert!(!plan.contains("SCAN m"), "{plan}");
        }
    }
}
