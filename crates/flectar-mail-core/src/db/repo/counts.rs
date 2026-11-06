//! Exact sidebar badges, sharing the predicate shapes of `threads::list`.
//! Kept out of threads.rs so the two evolve independently.

use crate::error::Result;
use crate::models::{MailGrouping, MailboxBadgeCounts, roles};
use rusqlite::Connection;
use rusqlite::types::ValueRef;
use std::collections::HashMap;

/// Every unread conversation's messages, one conversation after another
/// (`CROSS JOIN` keeps the conversations the outer loop), with the Gmail
/// label folders each message is in. Its folders decide which mailboxes and
/// folders list the conversation, as in `threads::list`. Only the index on
/// a conversation's messages and folders is read, never the messages.
const CONVERSATION_ROWS_SQL: &str = "
    SELECT t.id, t.account_id, t.routed_tab, s.thread_id IS NOT NULL, t.starred_count > 0,
           m.folder_id, mf.folder_id
    FROM threads t
    LEFT JOIN snoozes s ON s.thread_id = t.id
    CROSS JOIN messages m ON m.thread_id = t.id
    LEFT JOIN message_folders mf
           ON mf.message_id = m.id
          AND t.account_id IN (SELECT id FROM accounts WHERE provider = 'gmail')
    WHERE t.unread_count > 0";

/// Every unread message, one after another, with its Gmail label folders.
/// Snoozes and inbox tabs belong to the conversation, as in
/// `threads::list_messages`.
const MESSAGE_ROWS_SQL: &str = "
    SELECT m.id, m.account_id, t.routed_tab, s.thread_id IS NOT NULL, m.is_starred,
           m.folder_id, mf.folder_id, m.thread_id
    FROM messages m
    CROSS JOIN threads t ON t.id = m.thread_id
    LEFT JOIN snoozes s ON s.thread_id = m.thread_id
    LEFT JOIN message_folders mf
           ON mf.message_id = m.id
          AND m.account_id IN (SELECT id FROM accounts WHERE provider = 'gmail')
    WHERE m.is_read = 0 AND m.is_draft = 0 AND m.thread_id IS NOT NULL";

/// Unread conversations per manual label, which lists conversations with a
/// message that has the label. Categories are inbox tabs and are counted
/// with the inbox.
const CONVERSATION_LABELS_SQL: &str = "
    SELECT t.account_id, ml.label_id, COUNT(DISTINCT t.id)
    FROM threads t
    CROSS JOIN messages m ON m.thread_id = t.id
    JOIN message_labels ml ON ml.message_id = m.id
    JOIN labels l ON l.id = ml.label_id AND l.is_auto = 0
    WHERE t.unread_count > 0
    GROUP BY t.account_id, ml.label_id";

const MESSAGE_LABELS_SQL: &str = "
    SELECT m.account_id, ml.label_id, COUNT(*)
    FROM messages m
    JOIN message_labels ml ON ml.message_id = m.id
    JOIN labels l ON l.id = ml.label_id AND l.is_auto = 0
    WHERE m.is_read = 0 AND m.is_draft = 0 AND m.thread_id IS NOT NULL
    GROUP BY m.account_id, ml.label_id";

/// Whether each conversation's newest incoming message is automated, which
/// puts a conversation without a route in Other rather than Important, as in
/// `threads::list`. Asked once, for the unrouted inbox mail of a refresh;
/// `?1` is a JSON array of conversation ids.
const NEWEST_INCOMING_SQL: &str = "
    SELECT c.value,
           (SELECT m.is_automated FROM messages m
            WHERE m.thread_id = c.value AND m.is_draft = 0 AND m.is_outgoing = 0
            ORDER BY m.date DESC, m.id DESC LIMIT 1)
    FROM json_each(?1) c";

/// Query-plan details for the pass over unread mail behind the badges.
pub fn badge_query_plan(conn: &Connection, grouping: MailGrouping) -> Result<Vec<String>> {
    let sql = match grouping {
        MailGrouping::Conversations => CONVERSATION_ROWS_SQL,
        MailGrouping::Messages => MESSAGE_ROWS_SQL,
    };
    let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?;
    Ok(stmt
        .query_map([], |row| row.get(3))?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

/// The standard mailboxes a folder's role lists its mail in. Drafts counts
/// every draft and Starred is a flag, so neither is a role here.
#[derive(Clone, Copy)]
enum Mailbox {
    Inbox,
    Sent,
    Archive,
    Spam,
    Trash,
}

/// The inbox tab that lists an inbox conversation, as `threads::list`
/// routes it.
#[derive(Clone, Copy, Default)]
enum Tab {
    /// Not routed yet: the newest incoming message decides.
    #[default]
    Unrouted,
    Important,
    Other,
    /// A category, by label id.
    Category(i64),
    /// A custom split, which has no sidebar row.
    Elsewhere,
}

impl Tab {
    fn routed(route: ValueRef<'_>) -> Self {
        match route {
            ValueRef::Null | ValueRef::Text(b"pending") => Self::Unrouted,
            ValueRef::Text(b"important") => Self::Important,
            ValueRef::Text(b"other") => Self::Other,
            ValueRef::Text(route) => route
                .strip_prefix(b"label:")
                .and_then(|id| std::str::from_utf8(id).ok()?.parse().ok())
                .map_or(Self::Elsewhere, Self::Category),
            _ => Self::Elsewhere,
        }
    }
}

/// One unread conversation or message, gathered from its rows.
#[derive(Default)]
struct Item {
    id: i64,
    account_id: i64,
    tab: Tab,
    snoozed: bool,
    starred: bool,
    folders: Vec<i64>,
    thread_id: i64,
}

impl Item {
    fn start(&mut self, id: i64, account_id: i64, tab: Tab) {
        self.id = id;
        self.account_id = account_id;
        self.tab = tab;
        self.folders.clear();
    }
}

/// The badges of every account, filled in one item at a time.
struct Tally {
    counts: Vec<MailboxBadgeCounts>,
    positions: HashMap<i64, usize>,
    /// The account of the previous item, since items come account by account.
    last_account: Option<(i64, usize)>,
    roles: HashMap<i64, Mailbox>,
    /// Per account: each folder's count and the mailbox its role lists it in.
    folders: Vec<HashMap<i64, (i64, Option<Mailbox>)>>,
    labels: Vec<HashMap<i64, i64>>,
    /// Unrouted inbox items, as (conversation, account index), waiting for
    /// `NEWEST_INCOMING_SQL` to tell Important from Other.
    unrouted: Vec<(i64, usize)>,
}

impl Tally {
    fn new(conn: &Connection) -> Result<Self> {
        let counts = conn
            .prepare("SELECT id FROM accounts ORDER BY sort_order, id")?
            .query_map([], |row| {
                Ok(MailboxBadgeCounts {
                    account_id: row.get(0)?,
                    ..Default::default()
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let positions = counts
            .iter()
            .enumerate()
            .map(|(index, counts)| (counts.account_id, index))
            .collect();
        let roles = conn
            .prepare(
                "SELECT id, role FROM folders
                 WHERE role IN ('inbox', 'sent', 'archive', 'spam', 'trash')",
            )?
            .query_map([], |row| {
                let mailbox = match row.get_ref(1)?.as_str()? {
                    roles::INBOX => Mailbox::Inbox,
                    roles::SENT => Mailbox::Sent,
                    roles::ARCHIVE => Mailbox::Archive,
                    roles::SPAM => Mailbox::Spam,
                    _ => Mailbox::Trash,
                };
                Ok((row.get(0)?, mailbox))
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(Self {
            folders: vec![HashMap::new(); counts.len()],
            labels: vec![HashMap::new(); counts.len()],
            counts,
            positions,
            last_account: None,
            roles,
            unrouted: Vec::new(),
        })
    }

    /// Counts the item in each view that lists it.
    fn add(&mut self, item: &mut Item) {
        let index = match self.last_account {
            Some((account_id, index)) if account_id == item.account_id => index,
            _ => {
                let Some(&index) = self.positions.get(&item.account_id) else {
                    return;
                };
                self.last_account = Some((item.account_id, index));
                index
            }
        };
        item.folders.sort_unstable();
        item.folders.dedup();
        let (mut inbox, mut sent, mut archive, mut spam, mut trash) =
            (false, false, false, false, false);
        let roles = &self.roles;
        for folder in &item.folders {
            let (count, mailbox) = self.folders[index]
                .entry(*folder)
                .or_insert_with(|| (0, roles.get(folder).copied()));
            *count += 1;
            match mailbox {
                Some(Mailbox::Inbox) => inbox = true,
                Some(Mailbox::Sent) => sent = true,
                Some(Mailbox::Archive) => archive = true,
                Some(Mailbox::Spam) => spam = true,
                Some(Mailbox::Trash) => trash = true,
                None => {}
            }
        }
        let counts = &mut self.counts[index];
        counts.starred += i64::from(item.starred);
        counts.sent += i64::from(sent);
        // Archive leaves out what is also in the inbox.
        counts.archive += i64::from(archive && !inbox);
        counts.spam += i64::from(spam);
        counts.trash += i64::from(trash);
        if !inbox || item.snoozed {
            return;
        }
        counts.inbox += 1;
        match item.tab {
            Tab::Unrouted => self.unrouted.push((item.thread_id, index)),
            Tab::Important => counts.important += 1,
            Tab::Other => counts.other += 1,
            Tab::Category(label) => *self.labels[index].entry(label).or_default() += 1,
            Tab::Elsewhere => {}
        }
    }

    /// Splits the unrouted inbox mail into Important and Other.
    fn add_unrouted(&mut self, conn: &Connection) -> Result<()> {
        if self.unrouted.is_empty() {
            return Ok(());
        }
        let mut threads = self
            .unrouted
            .iter()
            .map(|(thread_id, _)| *thread_id)
            .collect::<Vec<_>>();
        threads.sort_unstable();
        threads.dedup();
        let mut automated = Vec::with_capacity(threads.len());
        let mut stmt = conn.prepare(NEWEST_INCOMING_SQL)?;
        let mut rows = stmt.query([serde_json::to_string(&threads)?])?;
        while let Some(row) = rows.next()? {
            automated.push((row.get::<_, i64>(0)?, row.get::<_, Option<bool>>(1)?));
        }
        for (thread_id, index) in std::mem::take(&mut self.unrouted) {
            let found = automated
                .binary_search_by_key(&thread_id, |(id, _)| *id)
                .ok()
                .and_then(|position| automated[position].1);
            let counts = &mut self.counts[index];
            match found {
                Some(false) => counts.important += 1,
                Some(true) => counts.other += 1,
                None => {}
            }
        }
        Ok(())
    }

    /// Adds each account's unread mail per manual label.
    fn add_labels(&mut self, conn: &Connection, sql: &str) -> Result<()> {
        let mut stmt = conn.prepare(sql)?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            if let Some(&index) = self.positions.get(&row.get::<_, i64>(0)?) {
                *self.labels[index].entry(row.get(1)?).or_default() += row.get::<_, i64>(2)?;
            }
        }
        Ok(())
    }

    fn finish(self) -> Vec<MailboxBadgeCounts> {
        let sorted = |list: Vec<(i64, i64)>| {
            let mut list = list;
            list.sort_unstable();
            list
        };
        self.counts
            .into_iter()
            .zip(self.folders)
            .zip(self.labels)
            .map(|((mut counts, folders), labels)| {
                counts.folders = sorted(
                    folders
                        .into_iter()
                        .map(|(folder, (count, _))| (folder, count))
                        .collect(),
                );
                counts.labels = sorted(labels.into_iter().collect());
                counts
            })
            .collect()
    }
}

fn tally_conversations(conn: &Connection, tally: &mut Tally) -> Result<()> {
    let mut stmt = conn.prepare(CONVERSATION_ROWS_SQL)?;
    let mut rows = stmt.query([])?;
    let mut item = Item::default();
    let mut started = false;
    while let Some(row) = rows.next()? {
        let id = row.get::<_, i64>(0)?;
        if !started || id != item.id {
            if started {
                tally.add(&mut item);
            }
            item.start(id, row.get(1)?, Tab::routed(row.get_ref(2)?));
            item.snoozed = row.get(3)?;
            item.starred = row.get(4)?;
            item.thread_id = id;
            started = true;
        }
        item.folders.extend(row.get::<_, Option<i64>>(5)?);
        item.folders.extend(row.get::<_, Option<i64>>(6)?);
    }
    if started {
        tally.add(&mut item);
    }
    Ok(())
}

fn tally_messages(conn: &Connection, tally: &mut Tally) -> Result<()> {
    let mut stmt = conn.prepare(MESSAGE_ROWS_SQL)?;
    let mut rows = stmt.query([])?;
    let mut item = Item::default();
    let mut started = false;
    while let Some(row) = rows.next()? {
        let id = row.get::<_, i64>(0)?;
        if !started || id != item.id {
            if started {
                tally.add(&mut item);
            }
            item.start(id, row.get(1)?, Tab::routed(row.get_ref(2)?));
            item.snoozed = row.get(3)?;
            item.starred = row.get(4)?;
            item.thread_id = row.get(7)?;
            started = true;
        }
        item.folders.extend(row.get::<_, Option<i64>>(5)?);
        item.folders.extend(row.get::<_, Option<i64>>(6)?);
    }
    if started {
        tally.add(&mut item);
    }
    Ok(())
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

/// Return the sidebar's badges for every account. One pass over the unread
/// mail fills in the folders, mailboxes and inbox tabs of all accounts, so a
/// refresh does not grow with the number of accounts or folders, and costs
/// nothing for mail that is read. Badges count what the mail list shows:
/// conversations, or messages.
pub fn mailbox_badge_counts(
    conn: &Connection,
    grouping: MailGrouping,
) -> Result<Vec<MailboxBadgeCounts>> {
    let messages = grouping == MailGrouping::Messages;
    let mut tally = Tally::new(conn)?;
    if messages {
        tally_messages(conn, &mut tally)?;
    } else {
        tally_conversations(conn, &mut tally)?;
    }
    tally.add_unrouted(conn)?;
    tally.add_labels(
        conn,
        if messages {
            MESSAGE_LABELS_SQL
        } else {
            CONVERSATION_LABELS_SQL
        },
    )?;

    let mut drafts = conn.prepare(&drafts_sql(messages))?;
    for row in drafts.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))? {
        let (account_id, value) = row?;
        if let Some(index) = tally.positions.get(&account_id) {
            tally.counts[*index].drafts = value;
        }
    }

    Ok(tally.finish())
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
                    important: 1,
                    folders: vec![(1, 1)],
                    ..Default::default()
                },
                MailboxBadgeCounts {
                    account_id: 2,
                    ..Default::default()
                },
            ]
        );

        conn.execute("UPDATE threads SET unread_count = 0 WHERE id = 1", [])
            .unwrap();
        let badges = mailbox_badge_counts(&conn, MailGrouping::Conversations).unwrap();
        assert_eq!(
            badges[0],
            MailboxBadgeCounts {
                account_id: 1,
                drafts: 1,
                ..Default::default()
            }
        );
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
                    important: 1,
                    // The folder lists the snoozed conversation too.
                    folders: vec![(1, 2)],
                    ..Default::default()
                },
                MailboxBadgeCounts {
                    account_id: 2,
                    inbox: 1,
                    starred: 1,
                    drafts: 1,
                    important: 1,
                    folders: vec![(2, 1), (3, 1)],
                    ..Default::default()
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

    /// Adds a thread whose one incoming message is in the inbox.
    fn routed_thread(conn: &Connection, id: i64, routed_tab: Option<&str>, automated: bool) {
        seed_thread(conn, id, 1, automated);
        conn.execute(
            "UPDATE threads SET routed_tab = ?2 WHERE id = ?1",
            params![id, routed_tab],
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
        crate::db::repo::threads::recompute(conn, thread_id).unwrap();
    }

    #[test]
    fn the_inbox_splits_by_route_or_by_the_newest_incoming_message() {
        let conn = test_db();
        routed_thread(&conn, 1, None, false);
        routed_thread(&conn, 2, Some("pending"), true);
        routed_thread(&conn, 3, Some("other"), false);
        routed_thread(&conn, 4, Some("label:2"), false);
        routed_thread(&conn, 5, Some("split:7"), false);
        // A human reply after a notification moves the conversation to
        // Important, and a notification after a reply to Other.
        routed_thread(&conn, 6, None, true);
        append_incoming(&conn, 6, 2000, false);
        routed_thread(&conn, 7, None, false);
        append_incoming(&conn, 7, 2000, true);

        for grouping in [MailGrouping::Conversations, MailGrouping::Messages] {
            let badges = &mailbox_badge_counts(&conn, grouping).unwrap()[0];
            let per_message = i64::from(grouping == MailGrouping::Messages);
            assert_eq!(badges.inbox, 7 + 2 * per_message, "{grouping:?}");
            assert_eq!(badges.important, 2 + per_message, "{grouping:?}");
            assert_eq!(badges.other, 3 + per_message, "{grouping:?}");
            assert_eq!(badges.labels, vec![(2, 1)], "{grouping:?}");
        }
    }

    /// A small xorshift generator, so the mailbox below is the same each run.
    struct Dice(u64);

    impl Dice {
        fn roll(&mut self, sides: u64) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0 % sides
        }

        fn chance(&mut self, percent: u64) -> bool {
            self.roll(100) < percent
        }
    }

    const SETTINGS: &str =
        "{\"connection\":{\"incomingSecurity\":\"tls\",\"smtpSecurity\":\"tls\"}}";

    /// An IMAP and a Gmail account with every case the badges mirror: Gmail
    /// label links, two Trash folders, nested folders, snoozes, every route,
    /// drafts, sent mail, stars and manual labels.
    fn varied_mailbox() -> Connection {
        let conn = crate::db::testutil::conn();
        conn.execute_batch(&format!(
            "INSERT INTO accounts (id, email, provider, auth_kind, username, incoming_host,
             incoming_port, smtp_host, smtp_port, created_at, sort_order, mail_protocol,
             settings_json) VALUES
             (1,'imap@test.dev','generic','password','i','h',993,'h',587,0,0,'imap','{SETTINGS}'),
             (2,'gmail@test.dev','gmail','oauth2','g','h',993,'h',587,0,1,'gmail','{SETTINGS}');
             INSERT INTO folders (id, account_id, path, role, parent_id) VALUES
             (1,1,'INBOX','inbox',NULL), (2,1,'Sent','sent',NULL),
             (3,1,'Archive','archive',NULL), (4,1,'Junk','spam',NULL),
             (5,1,'Trash','trash',NULL), (6,1,'Deleted Items','trash',NULL),
             (7,1,'Drafts','drafts',NULL), (8,1,'Projects',NULL,NULL),
             (9,1,'Projects/Launch',NULL,8), (10,1,'INBOX/Receipts',NULL,1),
             (11,2,'[Gmail]/All Mail','all',NULL), (12,2,'INBOX','inbox',NULL),
             (13,2,'[Gmail]/Sent Mail','sent',NULL), (14,2,'[Gmail]/Spam','spam',NULL),
             (15,2,'[Gmail]/Trash','trash',NULL), (16,2,'[Gmail]/Drafts','drafts',NULL),
             (17,2,'Archive','archive',NULL);
             INSERT INTO labels (id, name, color, keyword, position, is_auto, scope,
             owner_account_id, origin) VALUES
             (20,'Work','#fff','Work',0,0,'global',NULL,'user'),
             (21,'Travel','#fff','Travel',1,0,'account',2,'provider');"
        ))
        .unwrap();
        let mut dice = Dice(0x9e37_79b9_7f4a_7c15);
        let routes = [
            None,
            Some("pending"),
            Some("important"),
            Some("other"),
            Some("label:1"),
            Some("label:3"),
            Some("split:7"),
        ];
        let mut thread_id = 0;
        let mut date = 1_000;
        for account_id in [1, 2] {
            for _ in 0..60 {
                thread_id += 1;
                let route = routes[dice.roll(routes.len() as u64) as usize];
                conn.execute(
                    "INSERT INTO threads (id, account_id, subject_norm, routed_tab)
                     VALUES (?1, ?2, 's', ?3)",
                    params![thread_id, account_id, route],
                )
                .unwrap();
                if dice.chance(10) {
                    conn.execute(
                        "INSERT INTO snoozes (thread_id, account_id, wake_at) VALUES (?1, ?2, 9999)",
                        params![thread_id, account_id],
                    )
                    .unwrap();
                }
                for _ in 0..=dice.roll(3) {
                    date += 1;
                    let draft = dice.chance(5);
                    let outgoing = draft || dice.chance(20);
                    // Sent mail is read, except some imported mail.
                    let read = draft || dice.chance(if outgoing { 90 } else { 60 });
                    let (folder, links): (i64, Vec<i64>) = if account_id == 1 {
                        let folder = match (draft, outgoing) {
                            (true, _) => 7,
                            (_, true) => 2,
                            _ => [1, 1, 1, 3, 3, 4, 5, 6, 8, 9, 10][dice.roll(11) as usize],
                        };
                        (folder, Vec::new())
                    } else if draft {
                        (11, vec![16])
                    } else if outgoing {
                        (11, vec![13])
                    } else {
                        match dice.roll(7) {
                            0..=2 => (11, vec![12]),
                            3 => (14, vec![14]),
                            4 => (15, vec![15]),
                            5 => (11, vec![17, 12]),
                            _ => (11, vec![17]),
                        }
                    };
                    conn.execute(
                        "INSERT INTO messages (thread_id, account_id, folder_id, subject, from_addr,
                         date, is_read, is_starred, is_draft, is_outgoing, is_automated)
                         VALUES (?1, ?2, ?3, 's', 'a@b.c', ?4, ?5, ?6, ?7, ?8, ?9)",
                        params![
                            thread_id,
                            account_id,
                            folder,
                            date,
                            read,
                            dice.chance(15),
                            draft,
                            outgoing,
                            dice.chance(50)
                        ],
                    )
                    .unwrap();
                    let message_id = conn.last_insert_rowid();
                    for link in links {
                        conn.execute(
                            "INSERT INTO message_folders (message_id, folder_id) VALUES (?1, ?2)",
                            params![message_id, link],
                        )
                        .unwrap();
                    }
                    for (label, owner) in [(20, None), (21, Some(2))] {
                        if owner.is_none_or(|owner| owner == account_id) && dice.chance(20) {
                            conn.execute(
                                "INSERT INTO message_labels (message_id, label_id) VALUES (?1, ?2)",
                                params![message_id, label],
                            )
                            .unwrap();
                        }
                    }
                }
                crate::db::repo::threads::recompute(&conn, thread_id).unwrap();
            }
        }
        conn
    }

    #[test]
    fn every_badge_counts_the_unread_rows_its_list_shows() {
        use crate::db::repo::threads::{self, ListArgs, TabFilter};
        use crate::models::View;

        let conn = varied_mailbox();
        for grouping in [MailGrouping::Conversations, MailGrouping::Messages] {
            let messages = grouping == MailGrouping::Messages;
            // What a list shows: its rows, and the unread ones among them.
            let list = |account_id, view, folder_id, tab| {
                let args = ListArgs {
                    view,
                    tab,
                    account_id: Some(account_id),
                    account_ids: None,
                    folder_id,
                    cursor: None,
                    limit: 10_000,
                };
                if messages {
                    let page = threads::list_messages(&conn, &args, None).unwrap();
                    let unread = page.messages.iter().filter(|m| !m.is_read && !m.is_draft);
                    (page.messages.len() as i64, unread.count() as i64)
                } else {
                    let page = threads::list(&conn, &args).unwrap();
                    let unread = page.threads.iter().filter(|t| t.unread_count > 0);
                    (page.threads.len() as i64, unread.count() as i64)
                }
            };
            let unread =
                |account_id, view, folder_id, tab| list(account_id, view, folder_id, tab).1;
            let badges = mailbox_badge_counts(&conn, grouping).unwrap();
            assert_eq!(badges.len(), 2);
            for badge in &badges {
                let account = badge.account_id;
                let context = format!("{grouping:?}, account {account}");
                assert_eq!(
                    badge.inbox,
                    unread(account, View::Inbox, None, None),
                    "{context}"
                );
                assert_eq!(
                    badge.starred,
                    unread(account, View::Starred, None, None),
                    "{context}"
                );
                assert_eq!(
                    badge.sent,
                    unread(account, View::Sent, None, None),
                    "{context}"
                );
                assert_eq!(
                    badge.archive,
                    unread(account, View::Done, None, None),
                    "{context}"
                );
                assert_eq!(
                    badge.spam,
                    unread(account, View::Spam, None, None),
                    "{context}"
                );
                assert_eq!(
                    badge.trash,
                    unread(account, View::Trash, None, None),
                    "{context}"
                );
                // Drafts counts every draft the list holds.
                assert_eq!(
                    badge.drafts,
                    list(account, View::Drafts, None, None).0,
                    "{context}"
                );
                assert_eq!(
                    badge.important,
                    unread(account, View::Inbox, None, Some(TabFilter::Important)),
                    "{context}"
                );
                assert_eq!(
                    badge.other,
                    unread(account, View::Inbox, None, Some(TabFilter::Other)),
                    "{context}"
                );
                let folders = badge.folders.iter().copied().collect::<HashMap<_, _>>();
                let account_folders = conn
                    .prepare("SELECT id FROM folders WHERE account_id = ?1")
                    .unwrap()
                    .query_map([account], |row| row.get::<_, i64>(0))
                    .unwrap()
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .unwrap();
                for folder in account_folders {
                    assert_eq!(
                        folders.get(&folder).copied().unwrap_or_default(),
                        unread(account, View::All, Some(folder), None),
                        "{context}, folder {folder}"
                    );
                }
                let labels = badge.labels.iter().copied().collect::<HashMap<_, _>>();
                for category in 1..=4 {
                    assert_eq!(
                        labels.get(&category).copied().unwrap_or_default(),
                        unread(
                            account,
                            View::Inbox,
                            None,
                            Some(TabFilter::AutoLabel(category))
                        ),
                        "{context}, category {category}"
                    );
                }
                for label in [20, 21] {
                    assert_eq!(
                        labels.get(&label).copied().unwrap_or_default(),
                        unread(
                            account,
                            View::All,
                            None,
                            Some(TabFilter::ManualLabel(label))
                        ),
                        "{context}, label {label}"
                    );
                }
            }
            // The mailbox exercises each case, so the comparisons above are
            // not all of zeros.
            let [imap, gmail] = &badges[..] else {
                unreachable!()
            };
            for (case, count) in [
                ("inbox", imap.inbox + gmail.inbox),
                ("sent", imap.sent + gmail.sent),
                ("archive", imap.archive + gmail.archive),
                ("spam", imap.spam + gmail.spam),
                ("trash", imap.trash + gmail.trash),
                ("important", imap.important + gmail.important),
                ("other", imap.other + gmail.other),
                ("drafts", imap.drafts + gmail.drafts),
                ("starred", imap.starred + gmail.starred),
                ("nested folder", folders_of(imap, 9)),
                ("second Trash", folders_of(imap, 6)),
                ("Gmail inbox link", folders_of(gmail, 12)),
                ("category", labels_of(imap, 1) + labels_of(gmail, 3)),
                ("global label", labels_of(imap, 20) + labels_of(gmail, 20)),
                ("Gmail label", labels_of(gmail, 21)),
            ] {
                assert!(count > 0, "{grouping:?}: no unread {case} mail to compare");
            }
        }
    }

    fn folders_of(badges: &MailboxBadgeCounts, folder: i64) -> i64 {
        badges
            .folders
            .iter()
            .find(|(id, _)| *id == folder)
            .map_or(0, |(_, count)| *count)
    }

    fn labels_of(badges: &MailboxBadgeCounts, label: i64) -> i64 {
        badges
            .labels
            .iter()
            .find(|(id, _)| *id == label)
            .map_or(0, |(_, count)| *count)
    }

    #[test]
    fn badges_read_only_the_unread_mail() {
        let conn = varied_mailbox();
        for analyzed in [false, true] {
            if analyzed {
                conn.execute_batch("ANALYZE").unwrap();
            }
            let plan = badge_query_plan(&conn, MailGrouping::Conversations)
                .unwrap()
                .join("\n");
            assert!(
                plan.contains("SCAN t USING INDEX idx_threads_unread"),
                "{plan}"
            );
            let plan = badge_query_plan(&conn, MailGrouping::Messages)
                .unwrap()
                .join("\n");
            assert!(
                plan.contains("SCAN m USING COVERING INDEX idx_messages_unread"),
                "{plan}"
            );
        }
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
