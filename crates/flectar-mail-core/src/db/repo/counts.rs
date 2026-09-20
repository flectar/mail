//! Exact unread counts for split tabs and sidebar rows: a handful of scalar
//! COUNT queries sharing the predicate shapes of `threads::list`. Kept out of
//! threads.rs so the two evolve independently.

use crate::error::Result;
use crate::models::{Label, MailboxBadgeCounts, SplitRule, UnreadCounts, roles};
use rusqlite::Connection;
use std::collections::HashMap;

/// Composable COUNT(*) query over threads (+ snoozes join, like SUMMARY_SELECT).
struct Q {
    clauses: Vec<String>,
    bind: Vec<Box<dyn rusqlite::types::ToSql>>,
}

impl Q {
    /// Base: threads with unread messages, optionally account-scoped.
    fn unread(account_id: Option<i64>) -> Self {
        let mut q = Q {
            clauses: vec!["t.unread_count > 0".into()],
            bind: Vec::new(),
        };
        if let Some(acc) = account_id {
            q.bind.push(Box::new(acc));
            q.clauses.push(format!("t.account_id = ?{}", q.bind.len()));
        }
        q
    }

    /// Same base without the unread filter (drafts badge counts all drafts).
    fn any(account_id: Option<i64>) -> Self {
        let mut q = Q {
            clauses: Vec::new(),
            bind: Vec::new(),
        };
        if let Some(acc) = account_id {
            q.bind.push(Box::new(acc));
            q.clauses.push(format!("t.account_id = ?{}", q.bind.len()));
        }
        q
    }

    fn clause(mut self, c: impl Into<String>) -> Self {
        self.clauses.push(c.into());
        self
    }

    fn role_exists(&mut self, role: &str) -> String {
        self.bind.push(Box::new(role.to_string()));
        format!(
            "(EXISTS (SELECT 1 FROM messages m JOIN folders f ON f.id = m.folder_id
                      WHERE m.thread_id = t.id AND f.role = ?{0})
              OR EXISTS (
                    SELECT 1 FROM messages m
                    JOIN accounts ma ON ma.id = m.account_id AND ma.provider = 'gmail'
                    JOIN message_folders mf ON mf.message_id = m.id
                    JOIN folders f ON f.id = mf.folder_id
                    WHERE m.thread_id = t.id AND f.role = ?{0}
              ))",
            self.bind.len(),
        )
    }

    fn inbox(mut self) -> Self {
        let c = self.role_exists(roles::INBOX);
        self.clauses.push(c);
        self.clauses.push("s.thread_id IS NULL".into());
        self
    }

    /// Important (`automated=false`) / Other (`automated=true`) default buckets:
    /// forced by a routing rule, or unrouted mail split by the newest incoming
    /// message. Mirrors the bucket clauses in `threads::list`.
    fn bucket(mut self, automated: bool) -> Self {
        let (want, forced) = if automated {
            (1, "other")
        } else {
            (0, "important")
        };
        self.clauses.push(format!(
            "(t.routed_tab = '{forced}'
              OR ((t.routed_tab IS NULL OR t.routed_tab = 'pending')
                  AND (SELECT m.is_automated FROM messages m
                       WHERE m.thread_id = t.id AND m.is_draft = 0 AND m.is_outgoing = 0
                       ORDER BY m.date DESC, m.id DESC LIMIT 1) = {want}))"
        ));
        self
    }

    /// A custom split tab: threads routed to `split:<id>`.
    fn routed_split(mut self, id: i64) -> Self {
        self.bind.push(Box::new(format!("split:{id}")));
        self.clauses
            .push(format!("t.routed_tab = ?{}", self.bind.len()));
        self
    }

    /// An auto-category tab: threads routed to `label:<id>`.
    fn routed_label(mut self, id: i64) -> Self {
        self.bind.push(Box::new(format!("label:{id}")));
        self.clauses
            .push(format!("t.routed_tab = ?{}", self.bind.len()));
        self
    }

    /// A manual (user) label: a cross-cutting membership filter, not a routed tab.
    fn label(mut self, label_id: i64) -> Self {
        self.bind.push(Box::new(label_id));
        self.clauses.push(format!(
            "EXISTS (SELECT 1 FROM message_labels ml JOIN messages m ON m.id = ml.message_id
                     WHERE m.thread_id = t.id AND ml.label_id = ?{})",
            self.bind.len()
        ));
        self
    }

    fn run(self, conn: &Connection) -> Result<i64> {
        let where_sql = if self.clauses.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", self.clauses.join(" AND "))
        };
        let sql = format!(
            "SELECT COUNT(*) FROM threads t LEFT JOIN snoozes s ON s.thread_id = t.id {where_sql}"
        );
        let params: Vec<&dyn rusqlite::types::ToSql> =
            self.bind.iter().map(|b| b.as_ref()).collect();
        Ok(conn.query_row(&sql, params.as_slice(), |r| r.get(0))?)
    }
}

pub fn unread_counts(
    conn: &Connection,
    account_id: Option<i64>,
    splits: &[SplitRule],
    labels: &[Label],
) -> Result<UnreadCounts> {
    let inbox = Q::unread(account_id).inbox().run(conn)?;
    let important = Q::unread(account_id).inbox().bucket(false).run(conn)?;
    let other = Q::unread(account_id).inbox().bucket(true).run(conn)?;

    let mut splits_map = HashMap::new();
    for sp in splits {
        let n = Q::unread(account_id)
            .inbox()
            .routed_split(sp.id)
            .run(conn)?;
        splits_map.insert(sp.id.to_string(), n);
    }

    // Auto-category tabs read the single resolved tab; manual labels stay a
    // membership filter.
    let mut labels_map = HashMap::new();
    for l in labels {
        let q = Q::unread(account_id).inbox();
        let n = if l.is_auto {
            q.routed_label(l.id).run(conn)?
        } else {
            q.label(l.id).run(conn)?
        };
        labels_map.insert(l.id.to_string(), n);
    }

    let mut views = HashMap::new();
    views.insert(
        "starred".to_string(),
        Q::unread(account_id)
            .clause("t.starred_count > 0")
            .run(conn)?,
    );
    views.insert(
        "snoozed".to_string(),
        Q::unread(account_id)
            .clause("s.thread_id IS NOT NULL")
            .run(conn)?,
    );
    // Drafts badge = number of threads with a draft, unread or not.
    views.insert(
        "drafts".to_string(),
        Q::any(account_id)
            .clause(
                "(EXISTS (SELECT 1 FROM messages m JOIN folders f ON f.id = m.folder_id
                          WHERE m.thread_id = t.id AND m.is_draft = 1 AND f.role = 'drafts')
                  OR EXISTS (
                        SELECT 1 FROM messages m
                        JOIN accounts ma ON ma.id = m.account_id AND ma.provider = 'gmail'
                        JOIN message_folders mf ON mf.message_id = m.id
                        JOIN folders f ON f.id = mf.folder_id
                        WHERE m.thread_id = t.id AND m.is_draft = 1 AND f.role = 'drafts'
                  ))",
            )
            .run(conn)?,
    );

    Ok(UnreadCounts {
        inbox,
        important,
        other,
        splits: splits_map,
        labels: labels_map,
        views,
    })
}

/// Return the native sidebar's badges for every account in one grouped scan.
/// This deliberately avoids calling `unread_counts` once per account: the
/// Slint sidebar does not render split/label badges, and repeating those
/// scalar queries would make a live refresh proportional to account count.
pub fn mailbox_badge_counts(conn: &Connection) -> Result<Vec<MailboxBadgeCounts>> {
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
    let mut inbox = conn.prepare(
        "SELECT t.account_id, COUNT(*)
         FROM threads t
         LEFT JOIN snoozes s ON s.thread_id = t.id
         WHERE t.unread_count > 0
           AND s.thread_id IS NULL
           AND (EXISTS (
                SELECT 1 FROM messages m JOIN folders f ON f.id = m.folder_id
                WHERE m.thread_id = t.id AND f.role = 'inbox'
           ) OR EXISTS (
                SELECT 1 FROM messages m
                JOIN accounts ma ON ma.id = m.account_id AND ma.provider = 'gmail'
                JOIN message_folders mf ON mf.message_id = m.id
                JOIN folders f ON f.id = mf.folder_id
                WHERE m.thread_id = t.id AND f.role = 'inbox'
           ))
         GROUP BY t.account_id",
    )?;
    for row in inbox.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))? {
        let (account_id, value) = row?;
        if let Some(index) = positions.get(&account_id) {
            counts[*index].inbox = value;
        }
    }

    let mut starred = conn.prepare(
        "SELECT account_id, COUNT(*) FROM threads
         WHERE unread_count > 0 AND starred_count > 0
         GROUP BY account_id",
    )?;
    for row in starred.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))? {
        let (account_id, value) = row?;
        if let Some(index) = positions.get(&account_id) {
            counts[*index].starred = value;
        }
    }

    // Draft folders are normally tiny. Starting from those folders uses the
    // existing folder/message indexes and avoids inspecting every inbox row.
    let mut drafts = conn.prepare(
        "SELECT account_id, COUNT(DISTINCT thread_id)
         FROM (
             SELECT m.account_id, m.thread_id
             FROM folders f
             JOIN messages m ON m.folder_id = f.id
             WHERE f.role = 'drafts' AND m.is_draft = 1 AND m.thread_id IS NOT NULL
             UNION ALL
             SELECT m.account_id, m.thread_id
             FROM folders f
             JOIN message_folders mf ON mf.folder_id = f.id
             JOIN messages m ON m.id = mf.message_id
             JOIN accounts a ON a.id = m.account_id AND a.provider = 'gmail'
             WHERE f.role = 'drafts' AND m.is_draft = 1 AND m.thread_id IS NOT NULL
         )
         GROUP BY account_id",
    )?;
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
    use crate::models::SplitRuleQuery;
    use rusqlite::params;

    fn test_db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::run(&mut conn).unwrap();
        conn.execute(
            "INSERT INTO accounts (id, email, provider, auth_kind, username,
             imap_host, imap_port, smtp_host, smtp_port, created_at)
             VALUES (1,'t@x.com','imap','password','t','h',993,'h',587,0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO folders (id, account_id, imap_name, role) VALUES (1,1,'INBOX','inbox')",
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

    fn append_incoming(conn: &Connection, thread_id: i64, date: i64, automated: bool) {
        conn.execute(
            "INSERT INTO messages (thread_id, account_id, folder_id, subject,
             from_addr, date, is_read, is_automated, is_draft, is_outgoing)
             VALUES (?1, 1, 1, 'follow-up', 'reply@b.c', ?2, 0, ?3, 0, 0)",
            params![thread_id, date, automated as i64],
        )
        .unwrap();
    }

    #[test]
    fn partitions_important_and_other() {
        let conn = test_db();
        seed_thread(&conn, 1, 1, false); // unread human
        seed_thread(&conn, 2, 1, true); // unread automated
        seed_thread(&conn, 3, 0, true); // read automated

        let c = unread_counts(&conn, None, &[], &[]).unwrap();
        assert_eq!(c.inbox, 2);
        assert_eq!(c.important, 1);
        assert_eq!(c.other, 1);
        assert_eq!(c.views["starred"], 0);

        // account filter that matches nothing
        let none = unread_counts(&conn, Some(99), &[], &[]).unwrap();
        assert_eq!(none.inbox, 0);
    }

    #[test]
    fn mixed_threads_are_counted_by_the_newest_incoming_message() {
        let conn = test_db();
        seed_thread(&conn, 1, 1, false);
        append_incoming(&conn, 1, 2000, true);
        seed_thread(&conn, 2, 1, true);
        append_incoming(&conn, 2, 2000, false);

        let c = unread_counts(&conn, None, &[], &[]).unwrap();
        assert_eq!(c.inbox, 2);
        assert_eq!(c.important, 1, "latest human reply belongs in Important");
        assert_eq!(c.other, 1, "latest automated reply belongs in Other");
    }

    #[test]
    fn native_sidebar_badges_follow_read_and_draft_mutations() {
        let conn = test_db();
        conn.execute(
            "INSERT INTO accounts (id, email, provider, auth_kind, username,
             imap_host, imap_port, smtp_host, smtp_port, created_at, sort_order)
             VALUES (2,'two@x.com','imap','password','two','h',993,'h',587,0,1)",
            [],
        )
        .unwrap();
        seed_thread(&conn, 1, 1, false);
        conn.execute("UPDATE threads SET starred_count = 1 WHERE id = 1", [])
            .unwrap();
        conn.execute(
            "INSERT INTO folders (id, account_id, imap_name, role)
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

        let badges = mailbox_badge_counts(&conn).unwrap();
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
        let badges = mailbox_badge_counts(&conn).unwrap();
        assert_eq!(badges[0].inbox, 0);
        assert_eq!(badges[0].starred, 0);
        assert_eq!(badges[0].drafts, 1);
    }

    #[test]
    fn split_and_label_maps() {
        let conn = test_db();
        seed_thread(&conn, 1, 1, false);
        conn.execute(
            "INSERT INTO labels (id, name, color, keyword, position) VALUES (5,'L','#fff','KwL',0)",
            [],
        )
        .unwrap();
        let msg_id: i64 = conn
            .query_row("SELECT id FROM messages WHERE thread_id = 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        conn.execute(
            "INSERT INTO message_labels (message_id, label_id) VALUES (?1, 5)",
            params![msg_id],
        )
        .unwrap();
        // The custom-split count now reads the resolved tab, so route thread 1
        // into split:7 (the resolver does this at sync time).
        conn.execute("UPDATE threads SET routed_tab = 'split:7' WHERE id = 1", [])
            .unwrap();

        let split = SplitRule {
            id: 7,
            name: "s".into(),
            position: 0,
            query: SplitRuleQuery {
                senders: Some(vec!["a@b.c".into()]),
                ..Default::default()
            },
            target: None,
        };
        let label = Label {
            id: 5,
            name: "L".into(),
            color: "#fff".into(),
            keyword: "KwL".into(),
            position: 0,
            owner_account_id: None,
            is_auto: false,
        };

        let c = unread_counts(&conn, Some(1), &[split], &[label]).unwrap();
        assert_eq!(c.splits["7"], 1);
        assert_eq!(c.labels["5"], 1);
    }
}
