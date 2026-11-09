//! Scheduled sends that Flectar Mail Sync shares between the user's devices,
//! so another device sends one when the device that scheduled it is off.
//! What syncs is the schedule and which device claimed the send. A Gmail or
//! JMAP message waits in the account's Drafts on the mail server; an IMAP or
//! POP3 account keeps drafts on the device that wrote them, so the message
//! itself travels with the schedule.

use crate::error::Result;
use rusqlite::{Connection, OptionalExtension, Row, params};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncedSend {
    pub sync_id: String,
    pub account_id: i64,
    /// The draft's Message-ID, without angle brackets.
    pub message_id: String,
    pub send_at: i64,
    /// The device that scheduled it, by device id in hex.
    pub origin: String,
    /// The draft's version on the mail server that other devices send.
    pub draft_ref: Option<String>,
    /// `scheduled`, `sent`, `cancelled` or `failed`.
    pub state: String,
    pub error: Option<String>,
    /// The device that may send it, and until when.
    pub claimed_by: Option<String>,
    pub claim_until: Option<i64>,
    /// The send this device holds or made for it.
    pub action_id: Option<i64>,
    /// Until when this device may send it.
    pub may_send_until: Option<i64>,
    /// Another device's IMAP or POP3 message as a draft here.
    pub shadow_id: Option<i64>,
    /// Why this device could not send another device's send: it leaves it
    /// to the others.
    pub failed_here: Option<String>,
}

/// What another device says about a shared send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shared<'a> {
    pub account_id: i64,
    pub message_id: &'a str,
    pub send_at: i64,
    pub origin: &'a str,
    pub draft_ref: Option<&'a str>,
    pub state: &'a str,
    pub error: Option<&'a str>,
    pub claimed_by: Option<&'a str>,
    pub claim_until: Option<i64>,
}

pub const SCHEDULED: &str = "scheduled";
pub const SENT: &str = "sent";
pub const CANCELLED: &str = "cancelled";
pub const FAILED: &str = "failed";

const COLUMNS: &str = "sync_id, account_id, message_id, send_at, origin, draft_ref, state, error,
                       claimed_by, claim_until, action_id, may_send_until, shadow_id,
                       failed_here";

fn from_row(row: &Row) -> rusqlite::Result<SyncedSend> {
    Ok(SyncedSend {
        sync_id: row.get(0)?,
        account_id: row.get(1)?,
        message_id: row.get(2)?,
        send_at: row.get(3)?,
        origin: row.get(4)?,
        draft_ref: row.get(5)?,
        state: row.get(6)?,
        error: row.get(7)?,
        claimed_by: row.get(8)?,
        claim_until: row.get(9)?,
        action_id: row.get(10)?,
        may_send_until: row.get(11)?,
        shadow_id: row.get(12)?,
        failed_here: row.get(13)?,
    })
}

/// The message a shared IMAP or POP3 send goes out as, once known here.
pub fn mime(conn: &Connection, sync_id: &str) -> Result<Option<Vec<u8>>> {
    Ok(conn
        .query_row(
            "SELECT mime FROM synced_sends WHERE sync_id = ?1",
            params![sync_id],
            |row| row.get::<_, Option<Vec<u8>>>(0),
        )
        .optional()?
        .flatten())
}

/// Keep the message a shared send goes out as, and the version other
/// devices send: `draft_ref` names this message.
pub fn set_mime(
    conn: &Connection,
    sync_id: &str,
    mime: Option<&[u8]>,
    draft_ref: Option<&str>,
    now_ms: i64,
) -> Result<()> {
    conn.execute(
        "UPDATE synced_sends SET mime = ?2, draft_ref = ?3, updated_at = ?4 WHERE sync_id = ?1",
        params![sync_id, mime, draft_ref, now_ms],
    )?;
    Ok(())
}

/// The file a shared IMAP or POP3 message too large for its record went
/// into, and the preview other devices show of it.
pub fn file(conn: &Connection, sync_id: &str) -> Result<(Option<String>, Option<Vec<u8>>)> {
    Ok(conn
        .query_row(
            "SELECT file, preview FROM synced_sends WHERE sync_id = ?1",
            params![sync_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
        .unwrap_or((None, None)))
}

/// Keep the file a shared message went into, and its preview: as this
/// device stored it, or as another device's record carries it. A file it
/// replaces waits for its device to remove it (migration 037).
pub fn put_file(
    conn: &Connection,
    sync_id: &str,
    file: Option<&str>,
    preview: Option<&[u8]>,
) -> Result<()> {
    conn.execute(
        "UPDATE synced_sends SET file = ?2, preview = ?3
         WHERE sync_id = ?1 AND (file IS NOT ?2 OR preview IS NOT ?3)",
        params![sync_id, file, preview],
    )?;
    Ok(())
}

/// A send as it waited when this device began to share its message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AsSeen {
    pub updated_at: i64,
    pub send_at: i64,
}

/// Share the message of a send this device scheduled, as `version`: in its
/// record, or as a file and its preview. Only while the send still waits as
/// it did when its sharing began: a send moved, edited or cancelled since is
/// shared again as it is now, or not at all. Returns whether it was shared.
pub fn share(
    conn: &Connection,
    sync_id: &str,
    version: &str,
    mime: Option<&[u8]>,
    file: Option<(&str, &[u8])>,
    seen: AsSeen,
    now_ms: i64,
) -> Result<bool> {
    let (file, preview) = file.unzip();
    let changed = conn.execute(
        "UPDATE synced_sends
         SET mime = ?2, draft_ref = ?3, file = ?4, preview = ?5, updated_at = ?6
         WHERE sync_id = ?1 AND state = 'scheduled' AND draft_ref IS NULL
           AND updated_at = ?7 AND send_at = ?8",
        params![
            sync_id,
            mime,
            version,
            file,
            preview,
            now_ms,
            seen.updated_at,
            seen.send_at
        ],
    )?;
    Ok(changed == 1)
}

/// A file this device stored that no send took, to remove.
pub fn forget_file(conn: &Connection, file: &str, origin: &str) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO synced_send_files_gone (file, origin) VALUES (?1, ?2)",
        params![file, origin],
    )?;
    Ok(())
}

/// Let go of the files of this device's sends that went out or were
/// cancelled: nothing needs them any more. Returns how many it let go.
pub fn release_finished_files(conn: &Connection, origin: &str) -> Result<usize> {
    Ok(conn.execute(
        "UPDATE synced_sends SET file = NULL, preview = NULL
         WHERE origin = ?1 AND state IN ('sent', 'cancelled') AND file IS NOT NULL",
        params![origin],
    )?)
}

/// The files `origin` stored that nothing needs any more, to remove from
/// Flectar Mail Sync's storage. Other devices remove their own.
pub fn files_gone(conn: &Connection, origin: &str) -> Result<Vec<String>> {
    conn.execute(
        "DELETE FROM synced_send_files_gone WHERE origin IS NOT ?1",
        params![origin],
    )?;
    let mut statement = conn.prepare("SELECT file FROM synced_send_files_gone ORDER BY file")?;
    let files = statement
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(files)
}

/// A file removed from Flectar Mail Sync's storage.
pub fn file_removed(conn: &Connection, file: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM synced_send_files_gone WHERE file = ?1",
        params![file],
    )?;
    Ok(())
}

/// Forget the files waiting for removal: this device left the group.
pub fn clear_files_gone(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM synced_send_files_gone", [])?;
    Ok(())
}

/// Take on the message another device's record carries, if any.
pub fn put_mime(conn: &Connection, sync_id: &str, mime: Option<&[u8]>) -> Result<()> {
    conn.execute(
        "UPDATE synced_sends SET mime = ?2 WHERE sync_id = ?1 AND mime IS NOT ?2",
        params![sync_id, mime],
    )?;
    Ok(())
}

/// Note the draft that stands for another device's message here.
pub fn set_shadow(conn: &Connection, sync_id: &str, shadow_id: Option<i64>) -> Result<()> {
    conn.execute(
        "UPDATE synced_sends SET shadow_id = ?2 WHERE sync_id = ?1",
        params![sync_id, shadow_id],
    )?;
    Ok(())
}

/// Share a send this device scheduled and holds as `action_id`. Returns its
/// sync id.
pub fn create(
    conn: &Connection,
    account_id: i64,
    message_id: &str,
    send_at: i64,
    origin: &str,
    action_id: i64,
    now_ms: i64,
) -> Result<String> {
    Ok(conn.query_row(
        "INSERT INTO synced_sends
           (sync_id, account_id, message_id, send_at, origin, action_id, updated_at)
         VALUES (lower(hex(randomblob(16))), ?1, ?2, ?3, ?4, ?5, ?6)
         RETURNING sync_id",
        params![account_id, message_id, send_at, origin, action_id, now_ms],
        |row| row.get(0),
    )?)
}

pub fn get(conn: &Connection, sync_id: &str) -> Result<Option<SyncedSend>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM synced_sends WHERE sync_id = ?1"),
            params![sync_id],
            from_row,
        )
        .optional()?)
}

/// The shared send this device holds or made as `action_id`.
pub fn by_action(conn: &Connection, action_id: i64) -> Result<Option<SyncedSend>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM synced_sends WHERE action_id = ?1"),
            params![action_id],
            from_row,
        )
        .optional()?)
}

/// Every shared send, soonest first.
pub fn all(conn: &Connection) -> Result<Vec<SyncedSend>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM synced_sends ORDER BY send_at, sync_id"
    ))?;
    let rows = statement.query_map([], from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn any(conn: &Connection) -> Result<bool> {
    Ok(
        conn.query_row("SELECT EXISTS (SELECT 1 FROM synced_sends)", [], |row| {
            row.get(0)
        })?,
    )
}

/// Take on what another device says about a shared send. Which send this
/// device holds for it, and whether it may send it, stay as they are.
pub fn put_shared(conn: &Connection, sync_id: &str, shared: &Shared, now_ms: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO synced_sends
           (sync_id, account_id, message_id, send_at, origin, draft_ref, state, error,
            claimed_by, claim_until, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT(sync_id) DO UPDATE SET
           account_id = excluded.account_id, message_id = excluded.message_id,
           send_at = excluded.send_at, origin = excluded.origin,
           draft_ref = excluded.draft_ref, state = excluded.state, error = excluded.error,
           claimed_by = excluded.claimed_by, claim_until = excluded.claim_until,
           updated_at = excluded.updated_at",
        params![
            sync_id,
            shared.account_id,
            shared.message_id,
            shared.send_at,
            shared.origin,
            shared.draft_ref,
            shared.state,
            shared.error,
            shared.claimed_by,
            shared.claim_until,
            now_ms
        ],
    )?;
    Ok(())
}

pub fn set_claim(
    conn: &Connection,
    sync_id: &str,
    device: Option<&str>,
    until: Option<i64>,
    now_ms: i64,
) -> Result<()> {
    conn.execute(
        "UPDATE synced_sends SET claimed_by = ?2, claim_until = ?3, updated_at = ?4
         WHERE sync_id = ?1",
        params![sync_id, device, until, now_ms],
    )?;
    Ok(())
}

/// Move a shared send to `send_at`. A claim stays: only the device that
/// holds it may give it up, since it may be sending under it right now. It
/// sends at the new time, or gives the claim up when the new time is past it.
pub fn reschedule(conn: &Connection, sync_id: &str, send_at: i64, now_ms: i64) -> Result<()> {
    conn.execute(
        "UPDATE synced_sends SET send_at = ?2, state = 'scheduled', error = NULL, updated_at = ?3
         WHERE sync_id = ?1",
        params![sync_id, send_at, now_ms],
    )?;
    Ok(())
}

pub fn set_state(
    conn: &Connection,
    sync_id: &str,
    state: &str,
    error: Option<&str>,
    now_ms: i64,
) -> Result<()> {
    conn.execute(
        "UPDATE synced_sends SET state = ?2, error = ?3, updated_at = ?4 WHERE sync_id = ?1",
        params![sync_id, state, error, now_ms],
    )?;
    Ok(())
}

pub fn set_action(conn: &Connection, sync_id: &str, action_id: Option<i64>) -> Result<()> {
    conn.execute(
        "UPDATE synced_sends SET action_id = ?2 WHERE sync_id = ?1",
        params![sync_id, action_id],
    )?;
    Ok(())
}

pub fn set_failed_here(conn: &Connection, sync_id: &str, error: &str) -> Result<()> {
    conn.execute(
        "UPDATE synced_sends SET failed_here = ?2 WHERE sync_id = ?1",
        params![sync_id, error],
    )?;
    Ok(())
}

pub fn set_may_send_until(conn: &Connection, sync_id: &str, until: Option<i64>) -> Result<()> {
    conn.execute(
        "UPDATE synced_sends SET may_send_until = ?2 WHERE sync_id = ?1",
        params![sync_id, until],
    )?;
    Ok(())
}

pub fn set_draft_ref(conn: &Connection, sync_id: &str, draft_ref: &str, now_ms: i64) -> Result<()> {
    conn.execute(
        "UPDATE synced_sends SET draft_ref = ?2, updated_at = ?3 WHERE sync_id = ?1",
        params![sync_id, draft_ref, now_ms],
    )?;
    Ok(())
}

pub fn delete(conn: &Connection, sync_id: &str) -> Result<Option<SyncedSend>> {
    let send = get(conn, sync_id)?;
    drop_shadow(conn, sync_id)?;
    conn.execute(
        "DELETE FROM synced_sends WHERE sync_id = ?1",
        params![sync_id],
    )?;
    Ok(send)
}

/// Forget sends that finished before `before`: sent, cancelled or failed.
pub fn prune_finished(conn: &Connection, before: i64) -> Result<usize> {
    let mut statement = conn.prepare(
        "SELECT sync_id FROM synced_sends WHERE state <> 'scheduled' AND updated_at < ?1",
    )?;
    let finished = statement
        .query_map(params![before], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for sync_id in &finished {
        delete(conn, sync_id)?;
    }
    Ok(finished.len())
}

/// Forget the draft that stood for another device's message here, unless it
/// went out from this device. Returns its conversation.
pub fn drop_shadow(conn: &Connection, sync_id: &str) -> Result<Option<i64>> {
    let shadow: Option<i64> = conn
        .query_row(
            "SELECT shadow_id FROM synced_sends WHERE sync_id = ?1",
            params![sync_id],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    let Some(shadow) = shadow else {
        return Ok(None);
    };
    set_shadow(conn, sync_id, None)?;
    let row: Option<(bool, Option<i64>)> = conn
        .query_row(
            "SELECT is_draft, thread_id FROM messages WHERE id = ?1",
            params![shadow],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((is_draft, thread)) = row else {
        return Ok(None);
    };
    if is_draft {
        super::messages::delete(conn, shadow)?;
        if let Some(thread) = thread {
            super::threads::recompute(conn, thread)?;
        }
    }
    Ok(thread)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testutil;

    #[test]
    fn what_another_device_says_keeps_this_devices_send() {
        let conn = testutil::conn();
        testutil::seed_account(&conn);
        conn.execute(
            "INSERT INTO pending_actions (id, account_id, kind, created_at) VALUES (7, 1, 'send', 0)",
            [],
        )
        .unwrap();
        let sync_id = create(&conn, 1, "plans@example.com", 1_000, "aa", 7, 0).unwrap();
        set_may_send_until(&conn, &sync_id, Some(9_000)).unwrap();
        put_shared(
            &conn,
            &sync_id,
            &Shared {
                account_id: 1,
                message_id: "plans@example.com",
                send_at: 1_500,
                origin: "aa",
                draft_ref: Some("r1"),
                state: SCHEDULED,
                error: None,
                claimed_by: Some("bb"),
                claim_until: Some(2_000),
            },
            1,
        )
        .unwrap();
        let send = get(&conn, &sync_id).unwrap().unwrap();
        assert_eq!(send.send_at, 1_500);
        assert_eq!(send.claimed_by.as_deref(), Some("bb"));
        assert_eq!(send.action_id, Some(7));
        assert_eq!(send.may_send_until, Some(9_000));
        assert_eq!(by_action(&conn, 7).unwrap(), Some(send));

        // Moving it keeps the claim: only its holder gives it up.
        reschedule(&conn, &sync_id, 5_000, 2).unwrap();
        let moved = get(&conn, &sync_id).unwrap().unwrap();
        assert_eq!(moved.send_at, 5_000);
        assert_eq!(moved.claimed_by.as_deref(), Some("bb"));
    }

    #[test]
    fn finished_sends_are_forgotten_after_a_while() {
        let conn = testutil::conn();
        testutil::seed_account(&conn);
        conn.execute(
            "INSERT INTO pending_actions (id, account_id, kind, created_at) VALUES (7, 1, 'send', 0)",
            [],
        )
        .unwrap();
        let waiting = create(&conn, 1, "a@example.com", 1_000, "aa", 7, 0).unwrap();
        let sent = create(&conn, 1, "b@example.com", 1_000, "aa", 7, 0).unwrap();
        set_state(&conn, &sent, SENT, None, 10).unwrap();
        assert_eq!(prune_finished(&conn, 5).unwrap(), 0);
        assert_eq!(prune_finished(&conn, 11).unwrap(), 1);
        assert!(get(&conn, &waiting).unwrap().is_some());
        assert!(get(&conn, &sent).unwrap().is_none());
    }

    fn file_named(id: &str) -> String {
        format!(r#"{{"id":"{id}","key":"00","size":1}}"#)
    }

    #[test]
    fn files_nothing_needs_wait_for_the_device_that_stored_them() {
        let conn = testutil::conn();
        testutil::seed_account(&conn);
        conn.execute(
            "INSERT INTO pending_actions (id, account_id, kind, created_at) VALUES (7, 1, 'send', 0)",
            [],
        )
        .unwrap();
        let mine = create(&conn, 1, "a@example.com", 1_000, "aa", 7, 0).unwrap();
        let theirs = create(&conn, 1, "b@example.com", 1_000, "bb", 7, 0).unwrap();
        let forgotten = create(&conn, 1, "c@example.com", 1_000, "aa", 7, 0).unwrap();
        put_file(&conn, &mine, Some(&file_named("01")), Some(b"Plans")).unwrap();
        put_file(&conn, &theirs, Some(&file_named("02")), None).unwrap();
        put_file(&conn, &forgotten, Some(&file_named("03")), None).unwrap();

        // The same file with another preview is still needed.
        put_file(&conn, &mine, Some(&file_named("01")), Some(b"Other")).unwrap();
        assert!(files_gone(&conn, "aa").unwrap().is_empty());

        // Stored again, or its send forgotten: the old file goes. Another
        // device's file is that device's to remove.
        put_file(&conn, &mine, Some(&file_named("04")), None).unwrap();
        put_file(&conn, &theirs, None, None).unwrap();
        delete(&conn, &forgotten).unwrap();
        assert_eq!(
            files_gone(&conn, "aa").unwrap(),
            [file_named("01"), file_named("03")]
        );
        file_removed(&conn, &file_named("01")).unwrap();
        assert_eq!(files_gone(&conn, "aa").unwrap(), [file_named("03")]);

        // Sent: its file goes too.
        assert_eq!(release_finished_files(&conn, "aa").unwrap(), 0);
        set_state(&conn, &mine, SENT, None, 10).unwrap();
        assert_eq!(release_finished_files(&conn, "aa").unwrap(), 1);
        assert_eq!(file(&conn, &mine).unwrap(), (None, None));
        assert_eq!(
            files_gone(&conn, "aa").unwrap(),
            [file_named("03"), file_named("04")]
        );

        // One that no send took.
        forget_file(&conn, &file_named("05"), "aa").unwrap();
        assert_eq!(files_gone(&conn, "aa").unwrap().len(), 3);
    }

    #[test]
    fn a_send_that_changed_while_its_message_was_stored_is_shared_again() {
        let conn = testutil::conn();
        testutil::seed_account(&conn);
        conn.execute(
            "INSERT INTO pending_actions (id, account_id, kind, created_at) VALUES (7, 1, 'send', 0)",
            [],
        )
        .unwrap();
        let sync_id = create(&conn, 1, "a@example.com", 1_000, "aa", 7, 0).unwrap();
        let first = AsSeen {
            updated_at: 0,
            send_at: 1_000,
        };
        let stored = file_named("01");

        // Moved, and so edited perhaps, while its file went up.
        reschedule(&conn, &sync_id, 2_000, 5).unwrap();
        set_mime(&conn, &sync_id, None, None, 5).unwrap();
        let file_and_preview = Some((stored.as_str(), &b"Plans"[..]));
        assert!(!share(&conn, &sync_id, "v1", None, file_and_preview, first, 6).unwrap());
        assert_eq!(get(&conn, &sync_id).unwrap().unwrap().draft_ref, None);
        assert_eq!(file(&conn, &sync_id).unwrap(), (None, None));

        // As it waits now.
        let now = AsSeen {
            updated_at: 5,
            send_at: 2_000,
        };
        assert!(share(&conn, &sync_id, "v1", None, file_and_preview, now, 7).unwrap());
        assert_eq!(
            get(&conn, &sync_id).unwrap().unwrap().draft_ref.as_deref(),
            Some("v1")
        );
        assert_eq!(
            file(&conn, &sync_id).unwrap(),
            (Some(stored.clone()), Some(b"Plans".to_vec()))
        );

        // Shared already, or cancelled: not again.
        let after = AsSeen {
            updated_at: 7,
            send_at: 2_000,
        };
        assert!(!share(&conn, &sync_id, "v2", None, None, after, 8).unwrap());
        set_mime(&conn, &sync_id, None, None, 9).unwrap();
        set_state(&conn, &sync_id, CANCELLED, None, 9).unwrap();
        let cancelled = AsSeen {
            updated_at: 9,
            send_at: 2_000,
        };
        assert!(!share(&conn, &sync_id, "v2", None, None, cancelled, 10).unwrap());
    }
}
