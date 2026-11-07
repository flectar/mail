//! "Remind me if no reply" (issue #39). Unless someone replies to a sent
//! message by the time the user chose, its conversation comes back to the
//! top of the inbox, marked as waiting for a reply, until a reply arrives or
//! the user dismisses it. Each reminder names its message by Message-ID, so
//! Flectar Mail Sync carries it to the user's other devices, and each device
//! finds the conversation there.

use crate::error::Result;
use rusqlite::{Connection, OptionalExtension, params};

/// A reminder that nobody has replied to a message yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyReminder {
    pub sync_id: String,
    pub account_id: i64,
    pub message_id: String,
    pub remind_at: i64,
    pub due_at: Option<i64>,
    pub thread_id: Option<i64>,
}

fn reminder_from_row(row: &rusqlite::Row) -> rusqlite::Result<ReplyReminder> {
    Ok(ReplyReminder {
        sync_id: row.get(0)?,
        account_id: row.get(1)?,
        message_id: row.get(2)?,
        remind_at: row.get(3)?,
        due_at: row.get(4)?,
        thread_id: row.get(5)?,
    })
}

const SELECT: &str =
    "SELECT sync_id, account_id, message_id, remind_at, due_at, thread_id FROM reply_reminders";

pub fn all(conn: &Connection) -> Result<Vec<ReplyReminder>> {
    let mut statement = conn.prepare(&format!("{SELECT} ORDER BY remind_at, sync_id"))?;
    let rows = statement
        .query_map([], reminder_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn get(conn: &Connection, sync_id: &str) -> Result<Option<ReplyReminder>> {
    Ok(conn
        .query_row(
            &format!("{SELECT} WHERE sync_id = ?1"),
            params![sync_id],
            reminder_from_row,
        )
        .optional()?)
}

/// The conversation a message is in here: its sent copy's, or its draft's
/// while it waits to go out.
fn thread_of(conn: &Connection, account_id: i64, message_id: &str) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT thread_id FROM messages
             WHERE account_id = ?1 AND message_id = ?2 AND thread_id IS NOT NULL
             ORDER BY is_draft, id LIMIT 1",
            params![account_id, message_id],
            |row| row.get(0),
        )
        .optional()?)
}

/// Remind the user at `remind_at` unless someone replies to the message by
/// then, here and on the user's other devices. A message has one reminder:
/// setting it again moves it. Returns its sync id.
pub fn set(conn: &Connection, account_id: i64, message_id: &str, remind_at: i64) -> Result<String> {
    let thread_id = thread_of(conn, account_id, message_id)?;
    let existing: Option<String> = conn
        .query_row(
            "SELECT sync_id FROM reply_reminders WHERE account_id = ?1 AND message_id = ?2",
            params![account_id, message_id],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(sync_id) = existing {
        settle(conn, &sync_id)?;
        conn.execute(
            "UPDATE reply_reminders SET remind_at = ?2, thread_id = COALESCE(?3, thread_id)
             WHERE sync_id = ?1",
            params![sync_id, remind_at, thread_id],
        )?;
        return Ok(sync_id);
    }
    Ok(conn.query_row(
        "INSERT INTO reply_reminders (sync_id, account_id, message_id, remind_at, thread_id)
         VALUES (lower(hex(randomblob(16))), ?1, ?2, ?3, ?4) RETURNING sync_id",
        params![account_id, message_id, remind_at, thread_id],
        |row| row.get(0),
    )?)
}

/// Take on a reminder another device set or moved. When two reminders name
/// one message, the one with the smaller sync id holds it on every device:
/// this device drops the other, and its deletion reaches the rest. Returns
/// the conversation whose reminder changed here, if its mail is here.
pub fn put_shared(
    conn: &Connection,
    sync_id: &str,
    account_id: i64,
    message_id: &str,
    remind_at: i64,
) -> Result<Option<i64>> {
    let other: Option<String> = conn
        .query_row(
            "SELECT sync_id FROM reply_reminders
             WHERE account_id = ?1 AND message_id = ?2 AND sync_id <> ?3",
            params![account_id, message_id, sync_id],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(other) = other {
        if other.as_str() < sync_id {
            conn.execute(
                "DELETE FROM reply_reminders WHERE sync_id = ?1",
                params![sync_id],
            )?;
            return Ok(None);
        }
        conn.execute(
            "DELETE FROM reply_reminders WHERE sync_id = ?1",
            params![other],
        )?;
    }
    let thread_id = thread_of(conn, account_id, message_id)?;
    let before = get(conn, sync_id)?;
    match before {
        Some(before) if before.remind_at == remind_at && before.account_id == account_id => {
            Ok(None)
        }
        Some(_) => {
            settle(conn, sync_id)?;
            conn.execute(
                "UPDATE reply_reminders SET account_id = ?2, message_id = ?3, remind_at = ?4,
                        thread_id = COALESCE(?5, thread_id)
                 WHERE sync_id = ?1",
                params![sync_id, account_id, message_id, remind_at, thread_id],
            )?;
            Ok(thread_id)
        }
        None => {
            conn.execute(
                "INSERT INTO reply_reminders (sync_id, account_id, message_id, remind_at, thread_id)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![sync_id, account_id, message_id, remind_at, thread_id],
            )?;
            Ok(thread_id)
        }
    }
}

/// A due reminder stops waiting: its conversation goes back to the place
/// its own mail gives it.
fn settle(conn: &Connection, sync_id: &str) -> Result<()> {
    let due: Option<Option<i64>> = conn
        .query_row(
            "UPDATE reply_reminders SET due_at = NULL
             WHERE sync_id = ?1 AND due_at IS NOT NULL RETURNING thread_id",
            params![sync_id],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(Some(thread)) = due {
        super::threads::settle(conn, thread)?;
    }
    Ok(())
}

/// Forget a reminder. Returns the conversation it belonged to here.
pub fn delete(conn: &Connection, sync_id: &str) -> Result<Option<i64>> {
    settle(conn, sync_id)?;
    let thread: Option<Option<i64>> = conn
        .query_row(
            "DELETE FROM reply_reminders WHERE sync_id = ?1 RETURNING thread_id",
            params![sync_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(thread.flatten())
}

/// Forget the reminders of a conversation, here and on the user's other
/// devices: the user dismissed them, or the conversation went away.
/// `due_only` keeps the reminders still waiting for their time, as when the
/// conversation is archived before then. Returns how many went.
pub fn clear_thread(conn: &Connection, thread_id: i64, due_only: bool) -> Result<usize> {
    let mut statement = conn.prepare(
        "SELECT sync_id FROM reply_reminders
         WHERE thread_id = ?1 AND (?2 = 0 OR due_at IS NOT NULL)",
    )?;
    let ids = statement
        .query_map(params![thread_id, due_only], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for id in &ids {
        delete(conn, id)?;
    }
    Ok(ids.len())
}

/// Whether someone other than the user replied in the conversation after
/// the message dated `sent_at`. Automated mail, such as an out-of-office
/// reply, does not count.
fn replied(conn: &Connection, thread_id: i64, sent_at: i64) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM messages
           WHERE thread_id = ?1 AND is_outgoing = 0 AND is_draft = 0
             AND is_automated = 0 AND date > ?2)",
        params![thread_id, sent_at],
        |row| row.get(0),
    )?)
}

/// What checking the reminders changed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Checked {
    /// Conversations that came back to the top of the inbox.
    pub due: Vec<i64>,
    /// Conversations whose reminder went, because someone replied.
    pub replied: Vec<i64>,
}

/// Bring back the conversations whose reminder time has passed with no
/// reply, and forget the reminders someone replied to. A message that has
/// not gone out yet, or has not arrived here, waits.
pub fn check(conn: &Connection, now_ms: i64) -> Result<Checked> {
    let mut checked = Checked::default();
    let mut statement = conn.prepare(&format!("{SELECT} WHERE remind_at <= ?1"))?;
    let reminders = statement
        .query_map(params![now_ms], reminder_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for reminder in reminders {
        let sent: Option<(i64, i64)> = conn
            .query_row(
                "SELECT thread_id, date FROM messages
                 WHERE account_id = ?1 AND message_id = ?2 AND is_draft = 0
                   AND thread_id IS NOT NULL
                 ORDER BY date LIMIT 1",
                params![reminder.account_id, reminder.message_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((thread_id, sent_at)) = sent else {
            continue;
        };
        if replied(conn, thread_id, sent_at)? {
            delete(conn, &reminder.sync_id)?;
            if reminder.due_at.is_some() {
                checked.replied.push(thread_id);
            }
            continue;
        }
        if reminder.due_at.is_some() && reminder.thread_id == Some(thread_id) {
            continue;
        }
        conn.execute(
            "UPDATE reply_reminders SET due_at = COALESCE(due_at, ?2), thread_id = ?3
             WHERE sync_id = ?1",
            params![reminder.sync_id, now_ms, thread_id],
        )?;
        if reminder.due_at.is_none() {
            super::threads::resurface(conn, thread_id, now_ms)?;
            checked.due.push(thread_id);
        }
    }
    Ok(checked)
}

/// Find the conversations of reminders whose message has arrived since.
/// Returns them.
pub fn attach_waiting(conn: &Connection) -> Result<Vec<i64>> {
    let mut statement = conn.prepare(
        "UPDATE reply_reminders SET thread_id = (
           SELECT m.thread_id FROM messages m
           WHERE m.account_id = reply_reminders.account_id
             AND m.message_id = reply_reminders.message_id AND m.thread_id IS NOT NULL
           ORDER BY m.is_draft, m.id LIMIT 1)
         WHERE thread_id IS NULL AND EXISTS (
           SELECT 1 FROM messages m
           WHERE m.account_id = reply_reminders.account_id
             AND m.message_id = reply_reminders.message_id AND m.thread_id IS NOT NULL)
         RETURNING thread_id",
    )?;
    let threads = statement
        .query_map([], |row| row.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(threads)
}

/// Forget reminders whose message never arrived here, long past their time.
pub fn drop_stale(conn: &Connection, before_ms: i64) -> Result<usize> {
    Ok(conn.execute(
        "DELETE FROM reply_reminders WHERE remind_at < ?1 AND due_at IS NULL
           AND NOT EXISTS (SELECT 1 FROM messages m
                 WHERE m.account_id = reply_reminders.account_id
                   AND m.message_id = reply_reminders.message_id)",
        params![before_ms],
    )?)
}

/// Whether a reminder came due here: a reply ends it.
pub fn any_due(conn: &Connection) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM reply_reminders WHERE due_at IS NOT NULL)",
        [],
        |row| row.get(0),
    )?)
}

/// The next time a reminder comes due, if any waits.
pub fn next_remind_at(conn: &Connection) -> Result<Option<i64>> {
    Ok(conn.query_row(
        "SELECT MIN(remind_at) FROM reply_reminders WHERE due_at IS NULL",
        [],
        |row| row.get(0),
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testutil;

    /// The user's sent message in its own conversation, dated 1000.
    fn sent(conn: &Connection) -> i64 {
        conn.execute(
            "INSERT INTO folders (id, account_id, path, role) VALUES (2, 1, 'Sent', 'sent')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO threads (id, account_id, subject_norm, last_message_at)
             VALUES (7, 1, 'offer', 1000)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO messages (thread_id, account_id, folder_id, uid, message_id, subject,
             from_addr, date, is_read, is_draft, is_outgoing)
             VALUES (7, 1, 2, 1, 'offer@test.dev', 'Offer', 'me@test.dev', 1000, 1, 0, 1)",
            [],
        )
        .unwrap();
        7
    }

    fn reply(conn: &Connection, date: i64, automated: bool) {
        conn.execute(
            "INSERT INTO messages (thread_id, account_id, folder_id, uid, message_id, subject,
             from_addr, date, is_read, is_draft, is_outgoing, is_automated)
             VALUES (7, 1, 1, ?1, 'reply-' || ?1 || '@test.dev', 'Re: Offer',
                     'client@test.dev', ?1, 0, 0, 0, ?2)",
            params![date, automated],
        )
        .unwrap();
        crate::db::repo::threads::recompute(conn, 7).unwrap();
    }

    fn last_message_at(conn: &Connection, thread: i64) -> i64 {
        conn.query_row(
            "SELECT last_message_at FROM threads WHERE id = ?1",
            params![thread],
            |row| row.get(0),
        )
        .unwrap()
    }

    #[test]
    fn an_unanswered_message_comes_back_to_the_top_when_its_time_comes() {
        let conn = testutil::conn();
        testutil::seed_account(&conn);
        let thread = sent(&conn);
        let id = set(&conn, 1, "offer@test.dev", 5000).unwrap();
        assert_eq!(get(&conn, &id).unwrap().unwrap().thread_id, Some(thread));

        assert_eq!(check(&conn, 4999).unwrap(), Checked::default());
        assert_eq!(
            check(&conn, 5000).unwrap(),
            Checked {
                due: vec![thread],
                replied: Vec::new()
            }
        );
        assert_eq!(get(&conn, &id).unwrap().unwrap().due_at, Some(5000));
        assert_eq!(last_message_at(&conn, thread), 5000, "back at the top");
        // Due once: later checks leave it be.
        assert_eq!(check(&conn, 9000).unwrap(), Checked::default());
        assert_eq!(last_message_at(&conn, thread), 5000);
    }

    #[test]
    fn a_reply_ends_the_reminder_but_an_automatic_one_does_not() {
        let conn = testutil::conn();
        testutil::seed_account(&conn);
        let thread = sent(&conn);
        let id = set(&conn, 1, "offer@test.dev", 5000).unwrap();
        reply(&conn, 2000, true);
        assert_eq!(check(&conn, 5000).unwrap().due, [thread]);

        // A real reply after it came back clears it.
        reply(&conn, 6000, false);
        assert_eq!(
            check(&conn, 7000).unwrap(),
            Checked {
                due: Vec::new(),
                replied: vec![thread]
            }
        );
        assert_eq!(get(&conn, &id).unwrap(), None);
    }

    #[test]
    fn a_reply_before_its_time_means_it_never_comes_back() {
        let conn = testutil::conn();
        testutil::seed_account(&conn);
        sent(&conn);
        set(&conn, 1, "offer@test.dev", 5000).unwrap();
        reply(&conn, 3000, false);
        assert_eq!(check(&conn, 5000).unwrap(), Checked::default());
        assert!(all(&conn).unwrap().is_empty());
        assert_eq!(last_message_at(&conn, 7), 3000);
    }

    #[test]
    fn a_message_not_sent_yet_waits() {
        let conn = testutil::conn();
        testutil::seed_account(&conn);
        sent(&conn);
        conn.execute("UPDATE messages SET is_draft = 1", [])
            .unwrap();
        set(&conn, 1, "offer@test.dev", 5000).unwrap();
        assert_eq!(check(&conn, 6000).unwrap(), Checked::default());
        conn.execute("UPDATE messages SET is_draft = 0", [])
            .unwrap();
        assert_eq!(check(&conn, 6000).unwrap().due, [7]);
    }

    #[test]
    fn setting_it_again_moves_it_and_two_devices_keep_the_smaller_id() {
        let conn = testutil::conn();
        testutil::seed_account(&conn);
        sent(&conn);
        let id = set(&conn, 1, "offer@test.dev", 5000).unwrap();
        assert_eq!(set(&conn, 1, "offer@test.dev", 8000).unwrap(), id);
        assert_eq!(get(&conn, &id).unwrap().unwrap().remind_at, 8000);

        // Another device set one too: the smaller id stays.
        let smaller = "0".repeat(32);
        let larger = "f".repeat(32);
        put_shared(&conn, &larger, 1, "offer@test.dev", 9000).unwrap();
        assert_eq!(get(&conn, &larger).unwrap(), None);
        put_shared(&conn, &smaller, 1, "offer@test.dev", 9000).unwrap();
        let left = all(&conn).unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].sync_id, smaller);
        assert_eq!(left[0].remind_at, 9000);
    }

    #[test]
    fn dismissing_clears_the_due_reminder_and_archiving_keeps_a_waiting_one() {
        let conn = testutil::conn();
        testutil::seed_account(&conn);
        let thread = sent(&conn);
        set(&conn, 1, "offer@test.dev", 5000).unwrap();
        assert_eq!(clear_thread(&conn, thread, true).unwrap(), 0);
        check(&conn, 5000).unwrap();
        assert_eq!(last_message_at(&conn, thread), 5000);
        assert_eq!(clear_thread(&conn, thread, true).unwrap(), 1);
        assert!(all(&conn).unwrap().is_empty());
        assert_eq!(last_message_at(&conn, thread), 1000, "back in its place");
    }

    #[test]
    fn a_reminder_whose_message_never_arrived_is_forgotten() {
        let conn = testutil::conn();
        testutil::seed_account(&conn);
        put_shared(&conn, &"a".repeat(32), 1, "elsewhere@test.dev", 1000).unwrap();
        assert_eq!(next_remind_at(&conn).unwrap(), Some(1000));
        assert_eq!(drop_stale(&conn, 1000).unwrap(), 0);
        assert_eq!(drop_stale(&conn, 1001).unwrap(), 1);
        assert_eq!(next_remind_at(&conn).unwrap(), None);
    }
}
