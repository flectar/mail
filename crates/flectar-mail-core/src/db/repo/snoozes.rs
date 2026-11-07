//! Snoozed conversations. `snoozes` hides them on this device;
//! `synced_snoozes` is what Flectar Mail Sync carries to the user's other
//! devices, naming each conversation by an anchor they can find.

use crate::error::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Map, Value};

/// Snooze a conversation here and on the user's other devices.
pub fn set(
    conn: &Connection,
    thread_id: i64,
    account_id: i64,
    wake_at: i64,
    orig_folder_id: Option<i64>,
) -> Result<()> {
    set_local(conn, thread_id, account_id, wake_at, orig_folder_id)?;
    let anchor = anchor(conn, thread_id)?.to_string();
    let updated = conn.execute(
        "UPDATE synced_snoozes SET wake_at = ?2, anchor = ?3 WHERE thread_id = ?1",
        params![thread_id, wake_at, anchor],
    )?;
    if updated == 0 {
        conn.execute(
            "INSERT INTO synced_snoozes (sync_id, account_id, anchor, wake_at, thread_id)
             VALUES (lower(hex(randomblob(16))), ?1, ?2, ?3, ?4)",
            params![account_id, anchor, wake_at, thread_id],
        )?;
    }
    Ok(())
}

/// Snooze a conversation on this device only, as another device asked.
pub fn set_local(
    conn: &Connection,
    thread_id: i64,
    account_id: i64,
    wake_at: i64,
    orig_folder_id: Option<i64>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO snoozes (thread_id, account_id, wake_at, orig_folder_id)
         VALUES (?1,?2,?3,?4)
         ON CONFLICT(thread_id) DO UPDATE SET wake_at = excluded.wake_at",
        params![thread_id, account_id, wake_at, orig_folder_id],
    )?;
    Ok(())
}

/// End a conversation's snooze here and on the user's other devices.
pub fn clear(conn: &Connection, thread_id: i64) -> Result<()> {
    clear_local(conn, thread_id)?;
    conn.execute(
        "DELETE FROM synced_snoozes WHERE thread_id = ?1",
        params![thread_id],
    )?;
    Ok(())
}

/// End a conversation's snooze on this device only.
pub fn clear_local(conn: &Connection, thread_id: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM snoozes WHERE thread_id = ?1",
        params![thread_id],
    )?;
    Ok(())
}

/// Threads whose wake time has passed, with that time.
pub fn woken(conn: &Connection, now_ms: i64) -> Result<Vec<(i64, i64)>> {
    let mut stmt = conn.prepare("SELECT thread_id, wake_at FROM snoozes WHERE wake_at <= ?1")?;
    let rows = stmt
        .query_map(params![now_ms], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Bring a woken conversation back at `at`: it moves to the top of the
/// inbox, as Gmail brings back snoozed mail, and its newest message turns
/// unread, so it stands out.
pub fn mark_woken(conn: &Connection, thread_id: i64, at: i64) -> Result<()> {
    conn.execute(
        "UPDATE messages SET is_read = 0 WHERE id =
           (SELECT id FROM messages WHERE thread_id = ?1
            ORDER BY date DESC LIMIT 1)",
        params![thread_id],
    )?;
    super::threads::resurface(conn, thread_id, at)?;
    Ok(())
}

/// Forget shared snoozes whose time has passed while their conversation
/// never arrived here.
pub fn drop_expired(conn: &Connection, now_ms: i64) -> Result<usize> {
    Ok(conn.execute(
        "DELETE FROM synced_snoozes WHERE wake_at <= ?1",
        params![now_ms],
    )?)
}

pub fn next_wake_at(conn: &Connection) -> Result<Option<i64>> {
    Ok(conn
        .query_row("SELECT MIN(wake_at) FROM snoozes", [], |r| {
            r.get::<_, Option<i64>>(0)
        })
        .optional()?
        .flatten())
}

/// How another device finds this conversation: the provider's thread id
/// where the account has one, and the newest message's Message-ID.
pub fn anchor(conn: &Connection, thread_id: i64) -> Result<Value> {
    let mut anchor = Map::new();
    let ids: Option<(Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT gm_thrid, jmap_id FROM threads WHERE id = ?1",
            params![thread_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((gmail, jmap)) = ids {
        if let Some(gmail) = gmail.filter(|id| !id.is_empty()) {
            anchor.insert("gmThrid".into(), gmail.into());
        }
        if let Some(jmap) = jmap.filter(|id| !id.is_empty()) {
            anchor.insert("jmapThread".into(), jmap.into());
        }
    }
    let message_id: Option<String> = conn
        .query_row(
            "SELECT message_id FROM messages
             WHERE thread_id = ?1 AND message_id IS NOT NULL AND message_id <> ''
             ORDER BY date DESC LIMIT 1",
            params![thread_id],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(message_id) = message_id {
        anchor.insert("messageId".into(), message_id.into());
    }
    Ok(Value::Object(anchor))
}

/// The conversation an anchor names in an account on this device.
pub fn resolve(conn: &Connection, account_id: i64, anchor: &Value) -> Result<Option<i64>> {
    let text = |key: &str| anchor.get(key).and_then(Value::as_str);
    let by_thread = |column: &str, id: &str| -> Result<Option<i64>> {
        Ok(conn
            .query_row(
                &format!("SELECT id FROM threads WHERE account_id = ?1 AND {column} = ?2 LIMIT 1"),
                params![account_id, id],
                |row| row.get(0),
            )
            .optional()?)
    };
    if let Some(id) = text("gmThrid")
        && let Some(thread) = by_thread("gm_thrid", id)?
    {
        return Ok(Some(thread));
    }
    if let Some(id) = text("jmapThread")
        && let Some(thread) = by_thread("jmap_id", id)?
    {
        return Ok(Some(thread));
    }
    if let Some(id) = text("messageId") {
        return Ok(
            super::messages::by_message_id(conn, account_id, id)?.and_then(|row| row.thread_id)
        );
    }
    Ok(None)
}

/// Snooze the conversation a shared snooze names, if its mail is here.
/// When two shared snoozes name one conversation, the one with the smaller
/// sync id holds it on every device, and this device drops the other: its
/// deletion reaches the rest. Returns the conversation if it is snoozed now.
pub fn attach(
    conn: &Connection,
    sync_id: &str,
    account_id: i64,
    anchor: &Value,
    wake_at: i64,
) -> Result<Option<i64>> {
    let Some(thread_id) = resolve(conn, account_id, anchor)? else {
        return Ok(None);
    };
    let holder: Option<String> = conn
        .query_row(
            "SELECT sync_id FROM synced_snoozes WHERE thread_id = ?1 AND sync_id <> ?2",
            params![thread_id, sync_id],
            |row| row.get(0),
        )
        .optional()?;
    match holder {
        Some(holder) if holder.as_str() < sync_id => {
            conn.execute(
                "DELETE FROM synced_snoozes WHERE sync_id = ?1",
                params![sync_id],
            )?;
            return Ok(None);
        }
        Some(holder) => {
            conn.execute(
                "DELETE FROM synced_snoozes WHERE sync_id = ?1",
                params![holder],
            )?;
        }
        None => {}
    }
    conn.execute(
        "UPDATE synced_snoozes SET thread_id = ?2 WHERE sync_id = ?1",
        params![sync_id, thread_id],
    )?;
    set_local(conn, thread_id, account_id, wake_at, None)?;
    Ok(Some(thread_id))
}

/// Snooze the conversations of shared snoozes whose mail has arrived since.
/// Returns the conversations snoozed now.
pub fn attach_waiting(conn: &Connection, now_ms: i64) -> Result<Vec<i64>> {
    let mut statement = conn.prepare(
        "SELECT sync_id, account_id, anchor, wake_at FROM synced_snoozes
         WHERE thread_id IS NULL AND wake_at > ?1",
    )?;
    let waiting = statement
        .query_map(params![now_ms], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut snoozed = Vec::new();
    for (sync_id, account_id, anchor, wake_at) in waiting {
        let anchor: Value = serde_json::from_str(&anchor).unwrap_or_default();
        if let Some(thread) = attach(conn, &sync_id, account_id, &anchor, wake_at)? {
            snoozed.push(thread);
        }
    }
    Ok(snoozed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testutil;

    #[test]
    fn set_wake_clear_lifecycle() {
        let c = testutil::conn();
        testutil::seed_account(&c);
        let (t1, _) = testutil::seed_message(&c, "a@b.c", "one", false);
        let (t2, _) = testutil::seed_message(&c, "d@e.f", "two", false);

        set(&c, t1, 1, 1_000, Some(1)).unwrap();
        set(&c, t2, 1, 5_000, None).unwrap();
        // upsert moves the wake time instead of duplicating
        set(&c, t2, 1, 6_000, None).unwrap();

        assert_eq!(next_wake_at(&c).unwrap(), Some(1_000));
        assert_eq!(woken(&c, 1_500).unwrap(), vec![(t1, 1_000)]);
        assert_eq!(woken(&c, 10_000).unwrap().len(), 2);
        assert!(woken(&c, 500).unwrap().is_empty());

        clear(&c, t1).unwrap();
        assert_eq!(next_wake_at(&c).unwrap(), Some(6_000));
    }
}
