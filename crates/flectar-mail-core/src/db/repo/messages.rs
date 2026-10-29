use crate::error::{CoreError, Result};
use crate::mime::{MimePlan, PlannedAttachment};
use crate::models::*;
use rusqlite::{Connection, OptionalExtension, Row, params};

use super::parse_addrs;

/// Parsed header data ready for insertion (produced by the sync engine).
#[derive(Debug, Clone)]
pub struct NewMessage {
    pub account_id: i64,
    pub folder_id: i64,
    pub uid: Option<i64>,
    pub message_id: Option<String>,
    pub gm_msgid: Option<String>,
    pub gm_thrid: Option<String>,
    pub subject: String,
    pub from: Option<Address>,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub bcc: Vec<Address>,
    pub date: i64,
    pub internal_date: Option<i64>,
    pub is_read: bool,
    pub is_starred: bool,
    pub is_draft: bool,
    pub is_outgoing: bool,
    pub is_automated: bool,
    pub has_attachments: bool,
    pub size: Option<i64>,
    pub snippet: String,
    pub references: Vec<String>,
    pub list_unsubscribe: Option<String>,
    pub list_unsubscribe_post: Option<String>,
    /// Transmitting party misaligned with From: (see mime::resolve_via):
    /// email or bare DKIM domain, shown as "via" in the UI. None when the
    /// transmitting domain aligns with From:.
    pub sender_addr: Option<String>,
    pub sender_verification: SenderVerification,
}

#[derive(Debug, Clone)]
pub struct MessageRow {
    pub id: i64,
    pub account_id: i64,
    pub thread_id: Option<i64>,
    pub folder_id: Option<i64>,
    pub uid: Option<i64>,
    pub message_id: Option<String>,
    pub subject: String,
    pub size: Option<i64>,
    pub is_read: bool,
    pub is_starred: bool,
    pub body_state: String,
    pub raw_path: Option<String>,
    pub jmap_id: Option<String>,
    pub jmap_blob_id: Option<String>,
}

fn row_basic(row: &Row) -> rusqlite::Result<MessageRow> {
    Ok(MessageRow {
        id: row.get("id")?,
        account_id: row.get("account_id")?,
        thread_id: row.get("thread_id")?,
        folder_id: row.get("folder_id")?,
        uid: row.get("uid")?,
        message_id: row.get("message_id")?,
        subject: row.get("subject")?,
        size: row.get("size")?,
        is_read: row.get::<_, i64>("is_read")? != 0,
        is_starred: row.get::<_, i64>("is_starred")? != 0,
        body_state: row.get("body_state")?,
        raw_path: row.get("raw_path")?,
        jmap_id: row.get("jmap_id")?,
        jmap_blob_id: row.get("jmap_blob_id")?,
    })
}

// These identity lookups sit on sync and body-fetch hot paths. Selecting the
// full messages row also decoded address JSON, snippets, MIME plans, and other
// potentially large text columns that `MessageRow` never reads.
macro_rules! basic_message_select {
    ($tail:literal) => {
        concat!(
            "SELECT id, account_id, thread_id, folder_id, uid, message_id, ",
            "subject, size, is_read, is_starred, body_state, raw_path, ",
            "jmap_id, jmap_blob_id FROM messages ",
            $tail
        )
    };
}

pub fn get_row(conn: &Connection, id: i64) -> Result<Option<MessageRow>> {
    let mut stmt = conn.prepare_cached(basic_message_select!("WHERE id = ?1"))?;
    Ok(stmt.query_row(params![id], row_basic).optional()?)
}

pub fn by_folder_uid(conn: &Connection, folder_id: i64, uid: i64) -> Result<Option<MessageRow>> {
    let mut stmt =
        conn.prepare_cached(basic_message_select!("WHERE folder_id = ?1 AND uid = ?2"))?;
    Ok(stmt
        .query_row(params![folder_id, uid], row_basic)
        .optional()?)
}

/// Find an existing message row for this account by RFC Message-ID (used to
/// re-link after UIDVALIDITY resets and to dedupe Gmail label-folders). Of
/// several, the one still to be linked to the server comes first.
pub fn by_message_id(
    conn: &Connection,
    account_id: i64,
    message_id: &str,
) -> Result<Option<MessageRow>> {
    let mut stmt = conn.prepare_cached(basic_message_select!(
        "WHERE account_id = ?1 AND message_id = ?2
         ORDER BY relink_pending DESC, uid IS NULL DESC, id LIMIT 1"
    ))?;
    Ok(stmt
        .query_row(params![account_id, message_id], row_basic)
        .optional()?)
}

/// Conservatively adopt a row created by another transport. RFC 8621 allows
/// multiple Emails with the same Message-ID, so Message-ID by itself is not a
/// safe remote identity.
pub fn jmap_adoption_candidate(
    conn: &Connection,
    account_id: i64,
    message_id: &str,
    subject: &str,
    received_at_ms: i64,
    size: i64,
) -> Result<Option<MessageRow>> {
    let mut stmt = conn.prepare_cached(basic_message_select!(
        "WHERE account_id=?1 AND message_id=?2 AND jmap_id IS NULL
           AND subject=?3 AND size=?4
           AND ABS(COALESCE(internal_date,date)-?5) <= 300000
         ORDER BY id LIMIT 2"
    ))?;
    let mut rows = stmt.query_map(
        params![account_id, message_id, subject, size, received_at_ms],
        row_basic,
    )?;
    let candidate = rows.next().transpose()?;
    // Decode the second row too, preserving errors and rejecting ambiguity.
    let ambiguous = rows.next().transpose()?.is_some();
    Ok(if ambiguous { None } else { candidate })
}

/// A message of a JMAP account that no message on the server stands for:
/// mail that an earlier server or protocol of the account left behind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JmapLeftover {
    pub id: i64,
    pub thread_id: Option<i64>,
    pub raw_path: Option<String>,
    /// When it was received, as far as the earlier server said.
    pub received_at: i64,
}

/// Only mail that came from a server waits for its copy here: the account's
/// own drafts and the sent mail only this device holds are kept, and queued
/// sends are created on the server later.
pub fn jmap_leftovers(conn: &Connection, account_id: i64) -> Result<Vec<JmapLeftover>> {
    let mut stmt = conn.prepare_cached(
        "SELECT m.id, m.thread_id, m.raw_path, COALESCE(m.internal_date, m.date)
         FROM messages m
         WHERE m.account_id = ?1 AND m.jmap_id IS NULL AND m.relink_pending = 1
           AND NOT EXISTS (SELECT 1 FROM drafts_meta d WHERE d.message_id = m.id)",
    )?;
    let rows = stmt
        .query_map(params![account_id], |row| {
            Ok(JmapLeftover {
                id: row.get(0)?,
                thread_id: row.get(1)?,
                raw_path: row.get(2)?,
                received_at: row.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut leftovers = Vec::with_capacity(rows.len());
    for row in rows {
        if !super::actions::has_pending_remote_creation(conn, row.id)? {
            leftovers.push(row);
        }
    }
    Ok(leftovers)
}

/// A JMAP message whose attachments haven't been listed for its current blob.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JmapAttachmentListing {
    pub message_id: i64,
    pub remote_id: String,
    pub blob_id: String,
    pub size: i64,
}

/// Repeats the WHERE terms of `idx_messages_jmap_attachments_pending`, so the
/// queue is read from that index.
const JMAP_ATTACHMENT_LISTING_QUEUE: &str =
    "SELECT id, jmap_id, jmap_blob_id, COALESCE(size, 0) FROM messages
     WHERE account_id = ?1 AND has_attachments = 1 AND body_state <> 'cached'
       AND jmap_blob_id IS NOT NULL AND jmap_attachments_blob_id IS NOT jmap_blob_id
       AND jmap_id IS NOT NULL
     ORDER BY date DESC, id DESC LIMIT ?2";

/// The newest messages of the account whose attachments still need listing.
pub fn jmap_attachment_listing_queue(
    conn: &Connection,
    account_id: i64,
    limit: i64,
) -> Result<Vec<JmapAttachmentListing>> {
    let mut stmt = conn.prepare_cached(JMAP_ATTACHMENT_LISTING_QUEUE)?;
    let rows = stmt
        .query_map(params![account_id, limit], |row| {
            Ok(JmapAttachmentListing {
                message_id: row.get(0)?,
                remote_id: row.get(1)?,
                blob_id: row.get(2)?,
                size: row.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Adopt only an exact submission identifier generated by this client.
/// Subject/time similarities are not identity: another client can send a
/// different email with the same headers. Legacy copies without the marker
/// remain separate when their Message-ID was rewritten.
pub fn outgoing_sent_adoption_candidate(
    conn: &Connection,
    account_id: i64,
    folder_id: i64,
    submission_id: &str,
) -> Result<Option<MessageRow>> {
    if submission_id.is_empty() {
        return Ok(None);
    }
    let mut stmt = conn.prepare_cached(basic_message_select!(
        "WHERE account_id=?1 AND message_id=?3 AND raw_path IS NOT NULL
           AND ((folder_id=?2 AND is_outgoing=1 AND uid IS NULL)
                OR (is_draft=1 AND EXISTS (SELECT 1 FROM pending_actions
                    WHERE message_id=messages.id AND kind='send' AND state='inflight')))
         ORDER BY id LIMIT 2"
    ))?;
    let mut rows = stmt.query_map(params![account_id, folder_id, submission_id], row_basic)?;
    let candidate = rows.next().transpose()?;
    let ambiguous = rows.next().transpose()?.is_some();
    Ok(if ambiguous { None } else { candidate })
}

/// Stable Gmail API resource lookup. Unlike an RFC Message-ID this is always
/// present, unique within the account and unchanged by labels/moves.
pub fn by_gm_msgid(
    conn: &Connection,
    account_id: i64,
    gm_msgid: &str,
) -> Result<Option<MessageRow>> {
    let mut stmt = conn.prepare_cached(basic_message_select!(
        "WHERE account_id = ?1 AND gm_msgid = ?2 LIMIT 1"
    ))?;
    Ok(stmt
        .query_row(params![account_id, gm_msgid], row_basic)
        .optional()?)
}

pub fn by_jmap_id(conn: &Connection, account_id: i64, jmap_id: &str) -> Result<Option<MessageRow>> {
    let mut stmt = conn.prepare_cached(basic_message_select!(
        "WHERE account_id = ?1 AND jmap_id = ?2 LIMIT 1"
    ))?;
    Ok(stmt
        .query_row(params![account_id, jmap_id], row_basic)
        .optional()?)
}

pub fn set_jmap_remote(
    conn: &Connection,
    id: i64,
    jmap_id: &str,
    blob_id: Option<&str>,
) -> Result<()> {
    conn.execute(
        "UPDATE messages SET jmap_id = ?2, jmap_blob_id = ?3 WHERE id = ?1",
        params![id, jmap_id, blob_id],
    )?;
    Ok(())
}

pub fn insert(conn: &Connection, m: &NewMessage, thread_id: i64) -> Result<i64> {
    // The body may arrive later; until then the subject and preview are
    // all there is to read, and storing the body reads it again.
    let verification_code = (!m.is_outgoing && !m.is_draft)
        .then(|| crate::verification_code::find(&m.subject, &m.snippet))
        .flatten();
    conn.execute(
        "INSERT INTO messages (account_id, thread_id, folder_id, uid, message_id, gm_msgid, gm_thrid,
            subject, from_name, from_addr, to_json, cc_json, bcc_json, date, internal_date,
            is_read, is_starred, is_draft, is_outgoing, is_automated, has_attachments, size, snippet,
            list_unsubscribe, list_unsubscribe_post, sender_addr, sender_verification,
            verification_code)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,?27,?28)",
        params![
            m.account_id,
            thread_id,
            m.folder_id,
            m.uid,
            m.message_id,
            m.gm_msgid,
            m.gm_thrid,
            m.subject,
            m.from.as_ref().and_then(|a| a.name.as_deref()),
            m.from.as_ref().map(|a| a.email.as_str()),
            serde_json::to_string(&m.to)?,
            serde_json::to_string(&m.cc)?,
            serde_json::to_string(&m.bcc)?,
            m.date,
            m.internal_date,
            m.is_read as i64,
            m.is_starred as i64,
            m.is_draft as i64,
            m.is_outgoing as i64,
            m.is_automated as i64,
            m.has_attachments as i64,
            m.size,
            m.snippet,
            m.list_unsubscribe,
            m.list_unsubscribe_post,
            m.sender_addr,
            m.sender_verification.as_str(),
            verification_code,
        ],
    )?;
    let id = conn.last_insert_rowid();
    for r in &m.references {
        conn.execute(
            "INSERT OR IGNORE INTO message_refs (message_id, ref_message_id) VALUES (?1, ?2)",
            params![id, r],
        )?;
    }
    Ok(id)
}

/// A message given a UID is linked to the server and no longer waits to be
/// matched with its copy there.
pub fn set_uid_and_folder(
    conn: &Connection,
    id: i64,
    folder_id: i64,
    uid: Option<i64>,
) -> Result<()> {
    conn.execute(
        "UPDATE messages SET folder_id = ?2, uid = ?3,
                relink_pending = CASE WHEN ?3 IS NULL THEN relink_pending ELSE 0 END
         WHERE id = ?1",
        params![id, folder_id, uid],
    )?;
    Ok(())
}

/// What the server said about the account's mail, or a folder's, no longer
/// holds: another server or protocol took over, or the mailbox was recreated.
/// UIDs go, and the mail that came from a server waits to be matched by
/// Message-ID with its copy on the current one. The account's own drafts and
/// the sent mail only this device holds keep their place.
pub fn await_relink(conn: &Connection, account_id: i64, folder_id: Option<i64>) -> Result<()> {
    conn.execute(
        "UPDATE messages SET relink_pending = 1
         WHERE account_id = ?1 AND (?2 IS NULL OR folder_id = ?2)
           AND (uid IS NOT NULL OR jmap_id IS NOT NULL OR is_outgoing = 0)
           AND NOT EXISTS (SELECT 1 FROM drafts_meta d WHERE d.message_id = messages.id)",
        params![account_id, folder_id],
    )?;
    conn.execute(
        "UPDATE messages SET uid = NULL
         WHERE account_id = ?1 AND (?2 IS NULL OR folder_id = ?2) AND uid IS NOT NULL",
        params![account_id, folder_id],
    )?;
    Ok(())
}

/// When the oldest message of the account still waiting to be matched with
/// its copy on the IMAP server was received, if any waits without work
/// queued for it.
pub fn oldest_relink_pending(conn: &Connection, account_id: i64) -> Result<Option<i64>> {
    Ok(conn.query_row(
        "SELECT MIN(COALESCE(internal_date, date)) FROM messages
         WHERE account_id = ?1 AND relink_pending = 1
           AND NOT EXISTS (SELECT 1 FROM pending_actions p WHERE p.message_id = messages.id
                           AND p.state IN ('pending', 'inflight'))",
        params![account_id],
        |row| row.get(0),
    )?)
}

/// Removes the mail of an IMAP account that is still waiting to be matched
/// with a copy on the server once every folder has its history: it is no
/// longer on the server. Mail with work queued waits for it to finish. What
/// stays in a folder the server no longer lists, or in one of another
/// protocol, moves to the account's folder for the same purpose, and such
/// folders go once empty. Returns the changed threads and the files of
/// removed bodies.
pub fn drop_unrelinked_imap_mail(
    conn: &Connection,
    account_id: i64,
) -> Result<(Vec<i64>, Vec<String>)> {
    conn.execute(
        "UPDATE messages SET relink_pending = 0
         WHERE account_id = ?1 AND relink_pending = 1
           AND EXISTS (SELECT 1 FROM drafts_meta d WHERE d.message_id = messages.id)",
        params![account_id],
    )?;
    let gone = conn
        .prepare(
            "SELECT id, thread_id, raw_path FROM messages
             WHERE account_id = ?1 AND relink_pending = 1
               AND NOT EXISTS (SELECT 1 FROM pending_actions p WHERE p.message_id = messages.id
                               AND p.state IN ('pending', 'inflight'))",
        )?
        .query_map(params![account_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut thread_ids = Vec::new();
    let mut paths = Vec::new();
    for (id, thread_id, path) in gone {
        delete(conn, id)?;
        if let Some(thread_id) = thread_id
            && !thread_ids.contains(&thread_id)
        {
            thread_ids.push(thread_id);
        }
        paths.extend(path);
    }
    for thread_id in rehome_mail(
        conn,
        account_id,
        "f.missing_on_server = 1 OR f.jmap_id IS NOT NULL",
    )? {
        if !thread_ids.contains(&thread_id) {
            thread_ids.push(thread_id);
        }
    }
    for thread_id in &thread_ids {
        super::threads::recompute(conn, *thread_id)?;
    }
    conn.execute(
        "DELETE FROM folders WHERE account_id = ?1
           AND (missing_on_server = 1 OR jmap_id IS NOT NULL)
           AND NOT EXISTS (SELECT 1 FROM messages WHERE messages.folder_id = folders.id)",
        params![account_id],
    )?;
    Ok((thread_ids, paths))
}

/// Moves the account's mail out of the folders `leaving` selects (as `f`):
/// to the account's folder for the same purpose as the one it is in, or else
/// to Drafts for its drafts and Sent for its sent mail. Mail still waiting
/// for its copy on the server, and mail without such a folder, stays.
/// Returns the threads of mail that moved.
pub(crate) fn rehome_mail(conn: &Connection, account_id: i64, leaving: &str) -> Result<Vec<i64>> {
    let stranded = conn
        .prepare(&format!(
            "SELECT m.id, m.thread_id, f.role,
                    CASE
                      WHEN EXISTS (SELECT 1 FROM drafts_meta d WHERE d.message_id = m.id)
                        THEN 'drafts'
                      WHEN m.is_outgoing = 1 THEN 'sent'
                    END
             FROM messages m JOIN folders f ON f.id = m.folder_id
             WHERE m.account_id = ?1 AND m.relink_pending = 0 AND ({leaving})"
        ))?
        .query_map(params![account_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut thread_ids = Vec::new();
    for (id, thread_id, folder_role, own_role) in stranded {
        let mut home = None;
        for role in [folder_role, own_role].into_iter().flatten() {
            home = super::folders::by_role(conn, account_id, &role)?;
            if home.is_some() {
                break;
            }
        }
        let Some(home) = home else { continue };
        set_uid_and_folder(conn, id, home.id, None)?;
        if let Some(thread_id) = thread_id
            && !thread_ids.contains(&thread_id)
        {
            thread_ids.push(thread_id);
        }
    }
    Ok(thread_ids)
}

pub fn set_flags(conn: &Connection, id: i64, is_read: bool, is_starred: bool) -> Result<()> {
    conn.execute(
        "UPDATE messages SET is_read = ?2, is_starred = ?3 WHERE id = ?1",
        params![id, is_read as i64, is_starred as i64],
    )?;
    Ok(())
}

pub fn set_read(conn: &Connection, id: i64, is_read: bool) -> Result<()> {
    conn.execute(
        "UPDATE messages SET is_read = ?2 WHERE id = ?1",
        params![id, is_read as i64],
    )?;
    Ok(())
}

pub fn set_starred(conn: &Connection, id: i64, is_starred: bool) -> Result<()> {
    conn.execute(
        "UPDATE messages SET is_starred = ?2 WHERE id = ?1",
        params![id, is_starred as i64],
    )?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContentProgress {
    pub done: u64,
    pub total: u64,
    pub failed: u64,
}

/// Text-content cache progress. Local-only/orphaned rows cannot be fetched from
/// IMAP and therefore must not hold completion open forever.
pub fn content_progress(conn: &Connection, account_id: i64) -> Result<ContentProgress> {
    content_progress_since(conn, account_id, None)
}

pub fn content_progress_since(
    conn: &Connection,
    account_id: i64,
    cutoff_ms: Option<i64>,
) -> Result<ContentProgress> {
    conn.query_row(
        "SELECT
           COALESCE(SUM(a.provider = 'gmail' OR m.body_state = 'cached'), 0),
           COUNT(*),
           COALESCE(SUM(a.provider <> 'gmail' AND m.body_state != 'cached' AND sf.id IS NOT NULL), 0)
         FROM messages m
         JOIN folders f ON f.id = m.folder_id
         JOIN accounts a ON a.id = m.account_id
         LEFT JOIN sync_failures sf
           ON sf.stage = 'content' AND sf.message_id = m.id
         WHERE m.account_id = ?1 AND (
           (a.provider = 'gmail' AND m.gm_msgid IS NOT NULL)
           OR
           (a.provider <> 'gmail' AND m.uid IS NOT NULL AND COALESCE(f.role, '') <> 'all')
         )
           AND (?2 IS NULL OR COALESCE(m.internal_date, m.date) >= ?2)",
        params![account_id, cutoff_ms],
        |r| {
            Ok(ContentProgress {
                done: r.get::<_, i64>(0)? as u64,
                total: r.get::<_, i64>(1)? as u64,
                failed: r.get::<_, i64>(2)? as u64,
            })
        },
    )
    .map_err(Into::into)
}

/// Compatibility tuple used by the existing body worker.
pub fn body_progress(conn: &Connection, account_id: i64) -> Result<(u64, u64)> {
    let progress = content_progress(conn, account_id)?;
    Ok((progress.done, progress.total))
}

pub fn body_progress_since(
    conn: &Connection,
    account_id: i64,
    cutoff_ms: Option<i64>,
) -> Result<(u64, u64)> {
    let progress = content_progress_since(conn, account_id, cutoff_ms)?;
    Ok((progress.done, progress.total))
}

pub fn set_body_state(conn: &Connection, id: i64, state: &str) -> Result<()> {
    conn.execute(
        "UPDATE messages SET body_state = ?2 WHERE id = ?1",
        params![id, state],
    )?;
    Ok(())
}

/// Claim a body fetch exactly once. Repeated opens while a request is already
/// queued must not enqueue duplicate network work.
pub fn begin_body_fetch(conn: &Connection, id: i64) -> Result<bool> {
    Ok(conn.execute(
        "UPDATE messages SET body_state = 'fetching'
         WHERE id = ?1 AND body_state = 'none'",
        [id],
    )? != 0)
}

/// Release a body-fetch claim only if it has not completed in the meantime.
pub fn cancel_body_fetch(conn: &Connection, id: i64) -> Result<()> {
    conn.execute(
        "UPDATE messages SET body_state = 'none'
         WHERE id = ?1 AND body_state = 'fetching'",
        [id],
    )?;
    Ok(())
}

pub fn set_mime_plan(conn: &Connection, id: i64, plan: Option<&MimePlan>) -> Result<()> {
    let json = plan.map(serde_json::to_string).transpose()?;
    let changed = conn.execute(
        "UPDATE messages SET mime_plan_json = ?2 WHERE id = ?1",
        params![id, json],
    )?;
    if changed == 0 {
        return Err(CoreError::NotFound(format!("message {id}")));
    }
    Ok(())
}

pub fn mime_plan(conn: &Connection, id: i64) -> Result<Option<MimePlan>> {
    let json: Option<String> = conn
        .query_row(
            "SELECT mime_plan_json FROM messages WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    json.map(|value| serde_json::from_str(&value))
        .transpose()
        .map_err(Into::into)
}

pub fn store_body(
    conn: &Connection,
    id: i64,
    text_body: Option<&str>,
    html_body: Option<&str>,
    raw_path: Option<&str>,
    has_attachments: bool,
    snippet: Option<&str>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO message_bodies (message_id, text_body, html_body) VALUES (?1,?2,?3)
         ON CONFLICT(message_id) DO UPDATE SET text_body = excluded.text_body, html_body = excluded.html_body",
        params![id, text_body, html_body],
    )?;
    // The full text replaces the preview a code was first looked for in.
    let received: Option<(String, String)> = conn
        .query_row(
            "SELECT subject, snippet FROM messages
             WHERE id = ?1 AND is_outgoing = 0 AND is_draft = 0",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let verification_code = received.and_then(|(subject, stored_snippet)| {
        find_verification_code(
            &subject,
            text_body,
            html_body,
            snippet.unwrap_or(&stored_snippet),
        )
    });
    // Full text is now available: queue the message for semantic embedding.
    // Inference happens off the writer thread (see the embed worker), so this
    // only flips a cheap flag.
    conn.execute(
        "UPDATE messages SET body_state = 'cached', raw_path = COALESCE(?2, raw_path),
                has_attachments = ?3, snippet = COALESCE(?4, snippet),
                verification_code = ?5, embedding_state = 'pending'
         WHERE id = ?1",
        params![
            id,
            raw_path,
            has_attachments as i64,
            snippet,
            verification_code
        ],
    )?;
    Ok(())
}

/// The newest message of each of `thread_ids` when it is received mail from
/// `since` on, its body is unread, no code was found in its subject and
/// preview, and reading its body has not failed recently:
/// (message, account, subject, snippet).
pub fn pending_code_bodies(
    conn: &Connection,
    thread_ids: &[i64],
    since: i64,
) -> Result<Vec<(i64, i64, String, String)>> {
    let mut found: Vec<(i64, i64, String, String)> = Vec::new();
    // Plain `prepare`: the placeholder count varies per call.
    for chunk in thread_ids.chunks(500) {
        let placeholders = vec!["?"; chunk.len()].join(",");
        let mut stmt = conn.prepare(&format!(
            "SELECT m.id, m.account_id, m.subject, m.snippet
             FROM threads t
             JOIN messages m ON m.id = (
               SELECT id FROM messages WHERE thread_id = t.id ORDER BY date DESC LIMIT 1)
             LEFT JOIN sync_failures sf ON sf.stage = 'content' AND sf.message_id = m.id
             WHERE t.id IN ({placeholders})
               AND m.body_state = 'none' AND m.verification_code IS NULL
               AND m.is_outgoing = 0 AND m.is_draft = 0 AND m.date >= ?
               AND (sf.id IS NULL OR sf.next_retry_at IS NULL OR sf.next_retry_at <= ?)
             ORDER BY m.date DESC"
        ))?;
        let values = chunk
            .iter()
            .copied()
            .chain([since, now_ms()])
            .collect::<Vec<_>>();
        found.extend(
            stmt.query_map(rusqlite::params_from_iter(values), |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?,
        );
    }
    Ok(found)
}

/// The code a received message asks its reader to enter, read from its full
/// text, or from its preview when the body has none.
pub(crate) fn find_verification_code(
    subject: &str,
    text_body: Option<&str>,
    html_body: Option<&str>,
    snippet: &str,
) -> Option<String> {
    let text = crate::mime::body_text(text_body, html_body);
    crate::verification_code::find(subject, text.as_deref().unwrap_or(snippet))
}

/// Newest non-draft message of a thread that carries a List-Unsubscribe
/// header, if any.
pub fn thread_unsubscribe_message(conn: &Connection, thread_id: i64) -> Result<Option<i64>> {
    let mut stmt = conn.prepare(
        "SELECT id FROM messages
         WHERE thread_id = ?1 AND is_draft = 0 AND list_unsubscribe IS NOT NULL
         ORDER BY date DESC LIMIT 1",
    )?;
    Ok(stmt
        .query_row(params![thread_id], |r| r.get(0))
        .optional()?)
}

/// Thread messages with no stored List-Unsubscribe but a raw MIME file on
/// disk, newest first. Mail synced before the header was part of the IMAP
/// header fetch has a NULL column even when the raw carries the header, so
/// these rows are worth re-reading once before reporting "no unsubscribe link".
pub fn thread_unsubscribe_candidates(
    conn: &Connection,
    thread_id: i64,
) -> Result<Vec<(i64, String)>> {
    let mut stmt = conn.prepare(
        "SELECT id, raw_path FROM messages
         WHERE thread_id = ?1 AND is_draft = 0
           AND list_unsubscribe IS NULL AND raw_path IS NOT NULL
         ORDER BY date DESC",
    )?;
    let rows = stmt.query_map(params![thread_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// Persist a List-Unsubscribe pair recovered from a cached raw message.
pub fn set_list_unsubscribe(
    conn: &Connection,
    id: i64,
    list_unsubscribe: &str,
    list_unsubscribe_post: Option<&str>,
) -> Result<()> {
    conn.execute(
        "UPDATE messages SET list_unsubscribe = ?2, list_unsubscribe_post = ?3 WHERE id = ?1",
        params![id, list_unsubscribe, list_unsubscribe_post],
    )?;
    Ok(())
}

pub fn set_sender_verification(
    conn: &Connection,
    id: i64,
    verification: SenderVerification,
) -> Result<()> {
    conn.execute(
        "UPDATE messages SET sender_verification = ?2 WHERE id = ?1",
        params![id, verification.as_str()],
    )?;
    Ok(())
}

pub struct NewAttachment<'a> {
    pub message_id: i64,
    pub part_id: Option<&'a str>,
    pub filename: Option<&'a str>,
    pub mime_type: Option<&'a str>,
    pub size: Option<i64>,
    pub content_id: Option<&'a str>,
    pub is_inline: bool,
}

pub fn replace_attachments(
    conn: &Connection,
    message_id: i64,
    atts: &[NewAttachment],
) -> Result<()> {
    #[derive(Debug)]
    struct Existing {
        id: i64,
        part_id: Option<String>,
        filename: Option<String>,
        mime_type: Option<String>,
        size: Option<i64>,
        content_id: Option<String>,
        has_file: bool,
        has_imap_section: bool,
    }

    let existing = {
        let mut stmt = conn.prepare(
            "SELECT id, part_id, filename, mime_type, size, content_id,
                    file_path IS NOT NULL, imap_section IS NOT NULL
             FROM attachments WHERE message_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map(params![message_id], |row| {
            Ok(Existing {
                id: row.get(0)?,
                part_id: row.get(1)?,
                filename: row.get(2)?,
                mime_type: row.get(3)?,
                size: row.get(4)?,
                content_id: row.get(5)?,
                has_file: row.get::<_, i64>(6)? != 0,
                has_imap_section: row.get::<_, i64>(7)? != 0,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    let mut used = std::collections::HashSet::new();

    for a in atts {
        // Full-message parsing uses attachment indexes while BODYSTRUCTURE
        // uses IMAP section ids. Match by the strongest immutable metadata so
        // an explicit-open fallback does not destroy planned row IDs or an
        // attachment file already cached on disk.
        let matched = existing
            .iter()
            .filter(|item| !used.contains(&item.id))
            .filter_map(|item| {
                let score = if a.part_id.is_some() && item.part_id.as_deref() == a.part_id {
                    Some(0)
                } else if a.content_id.is_some() && item.content_id.as_deref() == a.content_id {
                    Some(1)
                } else if a.filename.is_some()
                    && item.filename.as_deref() == a.filename
                    && (a.mime_type.is_none() || item.mime_type.as_deref() == a.mime_type)
                {
                    Some(2)
                } else if a.filename.is_none()
                    && a.content_id.is_none()
                    && item.filename.is_none()
                    && item.content_id.is_none()
                    && item.mime_type.as_deref() == a.mime_type
                    && item.size == a.size
                {
                    Some(3)
                } else {
                    None
                };
                score.map(|score| (score, item.id))
            })
            .min()
            .map(|(_, id)| id);

        if let Some(id) = matched {
            used.insert(id);
            conn.execute(
                "UPDATE attachments
                 SET part_id = ?2, filename = ?3, mime_type = ?4, size = ?5,
                     content_id = ?6, is_inline = ?7
                 WHERE id = ?1",
                params![
                    id,
                    a.part_id,
                    a.filename,
                    a.mime_type,
                    a.size,
                    a.content_id,
                    a.is_inline as i64,
                ],
            )?;
        } else {
            conn.execute(
                "INSERT INTO attachments (
                   message_id, part_id, filename, mime_type, size, content_id, is_inline
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![
                    message_id,
                    a.part_id,
                    a.filename,
                    a.mime_type,
                    a.size,
                    a.content_id,
                    a.is_inline as i64,
                ],
            )?;
            used.insert(conn.last_insert_rowid());
        }
    }

    // Remove stale, uncached legacy-only descriptors. Planned IMAP rows and
    // downloaded files are intentionally retained if a quirky full parser
    // cannot match them; losing either would break stable UI IDs/offline use.
    for item in existing {
        if !used.contains(&item.id) && !item.has_file && !item.has_imap_section {
            conn.execute("DELETE FROM attachments WHERE id = ?1", params![item.id])?;
        }
    }
    Ok(())
}

pub fn set_attachment_imap_section(
    conn: &Connection,
    attachment_id: i64,
    imap_section: Option<&str>,
) -> Result<()> {
    let changed = conn.execute(
        "UPDATE attachments SET imap_section = ?2 WHERE id = ?1",
        params![attachment_id, imap_section],
    )?;
    if changed == 0 {
        return Err(CoreError::NotFound(format!("attachment {attachment_id}")));
    }
    Ok(())
}

pub fn attachment_by_imap_section(
    conn: &Connection,
    message_id: i64,
    imap_section: &str,
) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT id FROM attachments WHERE message_id = ?1 AND imap_section = ?2",
            params![message_id, imap_section],
            |row| row.get(0),
        )
        .optional()?)
}

/// Insert or refresh BODYSTRUCTURE attachment descriptors without replacing
/// rows. Stable IDs and already-downloaded `file_path` values are therefore
/// preserved across header re-syncs and plan upgrades.
///
/// Legacy rows have no `imap_section`; where possible they are adopted by
/// Content-ID, then by filename + MIME type, before a new row is inserted.
pub fn upsert_planned_attachments(
    conn: &Connection,
    message_id: i64,
    attachments: &[PlannedAttachment],
) -> Result<Vec<i64>> {
    let mut ids = Vec::with_capacity(attachments.len());
    for attachment in attachments {
        let existing = conn
            .query_row(
                "SELECT id FROM attachments
                 WHERE message_id = ?1 AND imap_section = ?2",
                params![message_id, attachment.section],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;

        let legacy = match existing {
            Some(id) => Some(id),
            None if attachment.content_id.is_some() => conn
                .query_row(
                    "SELECT id FROM attachments
                     WHERE message_id = ?1 AND imap_section IS NULL
                       AND content_id = ?2
                     ORDER BY (file_path IS NOT NULL) DESC, id
                     LIMIT 1",
                    params![message_id, attachment.content_id],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?,
            None => None,
        };
        let legacy = match legacy {
            Some(id) => Some(id),
            None if attachment.filename.is_some() => conn
                .query_row(
                    "SELECT id FROM attachments
                     WHERE message_id = ?1 AND imap_section IS NULL
                       AND filename = ?2
                       AND (mime_type = ?3 OR mime_type IS NULL)
                     ORDER BY (file_path IS NOT NULL) DESC, id
                     LIMIT 1",
                    params![message_id, attachment.filename, attachment.mime_type],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?,
            None => None,
        };

        let id = if let Some(id) = legacy {
            conn.execute(
                "UPDATE attachments
                 SET imap_section = ?2,
                     filename = COALESCE(?3, filename),
                     mime_type = COALESCE(?4, mime_type),
                     size = COALESCE(?5, size),
                     content_id = COALESCE(?6, content_id),
                     is_inline = ?7
                 WHERE id = ?1",
                params![
                    id,
                    attachment.section,
                    attachment.filename,
                    attachment.mime_type,
                    attachment.size as i64,
                    attachment.content_id,
                    attachment.is_inline as i64,
                ],
            )?;
            id
        } else {
            conn.execute(
                "INSERT INTO attachments (
                   message_id, filename, mime_type, size, content_id, is_inline, imap_section
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    message_id,
                    attachment.filename,
                    attachment.mime_type,
                    attachment.size as i64,
                    attachment.content_id,
                    attachment.is_inline as i64,
                    attachment.section,
                ],
            )?;
            conn.last_insert_rowid()
        };
        ids.push(id);
    }
    Ok(ids)
}

/// Remove a message that was expunged remotely.
pub fn delete(conn: &Connection, id: i64) -> Result<()> {
    conn.execute("DELETE FROM messages_fts WHERE rowid = ?1", params![id])
        .ok();
    conn.execute("DELETE FROM messages WHERE id = ?1", params![id])?;
    Ok(())
}

fn detail_from_row(row: &rusqlite::Row) -> rusqlite::Result<MessageDetail> {
    let subject: String = row.get("subject")?;
    let prefix: String = row.get("local_subject_prefix")?;
    let automation_note = row.get::<_, String>("local_body_note")?;
    Ok(MessageDetail {
        id: row.get("id")?,
        thread_id: row.get::<_, Option<i64>>("thread_id")?.unwrap_or(0),
        account_id: row.get("account_id")?,
        from: Address {
            name: row.get("from_name")?,
            email: row
                .get::<_, Option<String>>("from_addr")?
                .unwrap_or_default(),
        },
        to: parse_addrs(&row.get::<_, String>("to_json")?, 5)?,
        cc: parse_addrs(&row.get::<_, String>("cc_json")?, 6)?,
        subject: if prefix.is_empty() {
            subject
        } else {
            format!("{prefix}{subject}")
        },
        date: row.get("date")?,
        is_read: row.get::<_, i64>("is_read")? != 0,
        is_starred: row.get::<_, i64>("is_starred")? != 0,
        is_draft: row.get::<_, i64>("is_draft")? != 0,
        is_outgoing: row.get::<_, i64>("is_outgoing")? != 0,
        snippet: row.get("snippet")?,
        body_state: row.get("body_state")?,
        text_body: row.get("text_body")?,
        html_body: row.get("html_body")?,
        automation_note: (!automation_note.trim().is_empty()).then_some(automation_note),
        attachments: Vec::new(),
        list_unsubscribe: row.get("list_unsubscribe")?,
        list_unsubscribe_post: row.get("list_unsubscribe_post")?,
        via: row.get("sender_addr")?,
        sender_verification: SenderVerification::from_storage(
            &row.get::<_, String>("sender_verification")?,
        ),
        send_state: row.get("send_state")?,
        send_error: row.get("send_error")?,
    })
}

/// Correlated-subquery columns that annotate a draft with the state of its
/// queued send action, so a stuck/failed send stays visible with its error
/// instead of looking like an ordinary draft. `NULL` for anything without an
/// active send action. Kept as a shared fragment so `detail` and
/// `list_for_thread` expose the same columns `detail_from_row` reads.
const SEND_STATE_COLS: &str = "
    (SELECT CASE WHEN pa.state = 'failed' OR pa.last_error IS NOT NULL
                 THEN 'failed' ELSE 'queued' END
     FROM pending_actions pa
     WHERE pa.message_id = m.id AND pa.kind = 'send'
       AND pa.state IN ('pending', 'inflight', 'failed')
     ORDER BY pa.id DESC LIMIT 1) AS send_state,
    (SELECT pa.last_error
     FROM pending_actions pa
     WHERE pa.message_id = m.id AND pa.kind = 'send'
       AND pa.state IN ('pending', 'inflight', 'failed')
     ORDER BY pa.id DESC LIMIT 1) AS send_error";

// Message details intentionally omit sync-only identifiers, MIME plans, raw
// paths, and address fields the reader never touches. Besides avoiding large
// unused text values, the explicit projection keeps decoding independent of
// the physical table column order.
const DETAIL_MESSAGE_COLS: &str = "
    m.id AS id, m.thread_id AS thread_id, m.account_id AS account_id,
    m.from_name AS from_name, m.from_addr AS from_addr, m.to_json AS to_json,
    m.cc_json AS cc_json, m.subject AS subject,
    m.local_subject_prefix AS local_subject_prefix, m.date AS date,
    m.is_read AS is_read, m.is_starred AS is_starred, m.is_draft AS is_draft,
    m.is_outgoing AS is_outgoing, m.snippet AS snippet, m.body_state AS body_state,
    m.local_body_note AS local_body_note, m.list_unsubscribe AS list_unsubscribe,
    m.list_unsubscribe_post AS list_unsubscribe_post, m.sender_addr AS sender_addr,
    m.sender_verification AS sender_verification";

fn attachment_meta_from_row(
    row: &rusqlite::Row,
    first_col: usize,
) -> rusqlite::Result<AttachmentMeta> {
    Ok(AttachmentMeta {
        id: row.get(first_col)?,
        // Decode RFC 2047 encoded-words at read time so rows synced before the
        // BODYSTRUCTURE decode fix still display a readable name (idempotent for
        // already-clean values).
        filename: row
            .get::<_, Option<String>>(first_col + 1)?
            .map(|name| crate::mime::decode_encoded_words(&name)),
        mime_type: row.get(first_col + 2)?,
        size: row.get(first_col + 3)?,
        is_inline: row.get::<_, i64>(first_col + 4)? != 0,
    })
}

/// A message as an export writes it.
pub struct ExportedMessage {
    pub id: i64,
    pub raw_path: Option<String>,
    pub is_draft: bool,
    pub is_read: bool,
    pub is_starred: bool,
    pub date: i64,
    pub from: String,
}

/// A folder's messages for an export, oldest first.
pub fn in_folder_for_export(conn: &Connection, folder_id: i64) -> Result<Vec<ExportedMessage>> {
    let mut statement = conn.prepare(
        "SELECT id, raw_path, is_draft, is_read, is_starred, date, COALESCE(from_addr, '')
         FROM messages WHERE folder_id = ?1 ORDER BY date, id",
    )?;
    Ok(statement
        .query_map([folder_id], |row| {
            Ok(ExportedMessage {
                id: row.get(0)?,
                raw_path: row.get(1)?,
                is_draft: row.get::<_, i64>(2)? != 0,
                is_read: row.get::<_, i64>(3)? != 0,
                is_starred: row.get::<_, i64>(4)? != 0,
                date: row.get(5)?,
                from: row.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn detail(conn: &Connection, id: i64) -> Result<MessageDetail> {
    let sql = format!(
        "SELECT {DETAIL_MESSAGE_COLS}, b.text_body, b.html_body, {SEND_STATE_COLS}
         FROM messages m LEFT JOIN message_bodies b ON b.message_id = m.id
         WHERE m.id = ?1"
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut detail = stmt
        .query_row(params![id], detail_from_row)
        .optional()?
        .ok_or_else(|| CoreError::NotFound(format!("message {id}")))?;

    let mut astmt = conn.prepare(
        "SELECT id, filename, mime_type, size, is_inline FROM attachments WHERE message_id = ?1",
    )?;
    let atts = astmt.query_map(params![id], |row| attachment_meta_from_row(row, 0))?;
    for a in atts {
        detail.attachments.push(a?);
    }
    Ok(detail)
}

/// Remove composer staging rows and return their app-managed file paths so the
/// caller can delete them after the surrounding transaction commits.
pub fn take_draft_attachment_paths(conn: &Connection, draft_id: i64) -> Result<Vec<String>> {
    let paths = {
        let mut statement = conn
            .prepare("SELECT file_path FROM draft_attachments WHERE draft_id = ?1 ORDER BY id")?;
        statement
            .query_map(params![draft_id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    conn.execute(
        "DELETE FROM draft_attachments WHERE draft_id = ?1",
        params![draft_id],
    )?;
    Ok(paths)
}

pub fn latest_in_thread(conn: &Connection, thread_id: i64) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT id FROM messages WHERE thread_id = ?1 ORDER BY date DESC, id DESC LIMIT 1",
            params![thread_id],
            |row| row.get(0),
        )
        .optional()?)
}

pub fn list_for_thread(conn: &Connection, thread_id: i64) -> Result<Vec<MessageDetail>> {
    let sql = format!(
        "SELECT {DETAIL_MESSAGE_COLS}, b.text_body, b.html_body, {SEND_STATE_COLS}
         FROM messages m LEFT JOIN message_bodies b ON b.message_id = m.id
         WHERE m.thread_id = ?1
         ORDER BY m.date ASC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut out = stmt
        .query_map(params![thread_id], detail_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if out.is_empty() {
        return Ok(out);
    }

    let mut astmt = conn.prepare(
        "SELECT a.message_id, a.id, a.filename, a.mime_type, a.size, a.is_inline
         FROM attachments a JOIN messages m ON m.id = a.message_id
         WHERE m.thread_id = ?1",
    )?;
    let mut by_message: std::collections::HashMap<i64, Vec<AttachmentMeta>> =
        std::collections::HashMap::new();
    let atts = astmt.query_map(params![thread_id], |row| {
        Ok((row.get::<_, i64>(0)?, attachment_meta_from_row(row, 1)?))
    })?;
    for a in atts {
        let (mid, meta) = a?;
        by_message.entry(mid).or_default().push(meta);
    }
    for m in &mut out {
        if let Some(atts) = by_message.remove(&m.id) {
            m.attachments = atts;
        }
    }
    Ok(out)
}

/// Conversation headers and snippets without materializing stored bodies or
/// attachment rows. The reader uses this for its virtualized thread timeline
/// and loads only the expanded message body separately.
pub fn list_outline_for_thread(conn: &Connection, thread_id: i64) -> Result<Vec<MessageDetail>> {
    let sql = format!(
        "SELECT {DETAIL_MESSAGE_COLS}, NULL AS text_body, NULL AS html_body, {SEND_STATE_COLS}
         FROM messages m
         WHERE m.thread_id = ?1
         ORDER BY m.date ASC, m.id ASC"
    );
    let mut stmt = conn.prepare(&sql)?;
    Ok(stmt
        .query_map(params![thread_id], detail_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

/// All (id, uid) pairs currently mapped in a folder - used for expunge reconciliation.
pub fn uids_in_folder(conn: &Connection, folder_id: i64) -> Result<Vec<(i64, i64)>> {
    let mut stmt =
        conn.prepare("SELECT id, uid FROM messages WHERE folder_id = ?1 AND uid IS NOT NULL")?;
    let rows = stmt
        .query_map(params![folder_id], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn max_uid_in_folder(conn: &Connection, folder_id: i64) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COALESCE(MAX(uid), 0) FROM messages WHERE folder_id = ?1",
        params![folder_id],
        |r| r.get(0),
    )?)
}

/// The user's own sent messages with bodies, newest first - the corpus for
/// learning their writing voice. `(id, subject, text_body)`.
pub fn list_sent_bodies(
    conn: &Connection,
    account_id: Option<i64>,
    limit: i64,
) -> Result<Vec<(i64, String, String)>> {
    let (acc_sql, bind): (&str, Vec<Box<dyn rusqlite::types::ToSql>>) = match account_id {
        Some(id) => ("AND m.account_id = ?2", vec![Box::new(limit), Box::new(id)]),
        None => ("", vec![Box::new(limit)]),
    };
    let sql = format!(
        "SELECT m.id, m.subject, b.text_body
         FROM messages m
         JOIN folders f ON f.id = m.folder_id AND f.role = 'sent'
         JOIN message_bodies b ON b.message_id = m.id
         WHERE m.is_draft = 0 AND b.text_body IS NOT NULL AND b.text_body <> '' {acc_sql}
         ORDER BY m.date DESC LIMIT ?1"
    );
    let mut stmt = conn.prepare(&sql)?;
    let params_ref: Vec<&dyn rusqlite::types::ToSql> = bind.iter().map(|b| b.as_ref()).collect();
    let rows = stmt
        .query_map(params_ref.as_slice(), |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Of `ids`, return those that are the user's own sent (non-draft) messages,
/// preserving the input order. Used to keep only self-authored few-shot hits.
pub fn filter_sent(conn: &Connection, ids: &[i64]) -> Result<Vec<i64>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let id_list = ids
        .iter()
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT m.id FROM messages m
         JOIN folders f ON f.id = m.folder_id AND f.role = 'sent'
         WHERE m.is_draft = 0 AND m.id IN ({id_list})"
    );
    let mut stmt = conn.prepare(&sql)?;
    let sent: std::collections::HashSet<i64> = stmt
        .query_map([], |r| r.get::<_, i64>(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(ids.iter().copied().filter(|id| sent.contains(id)).collect())
}

/// Messages in a folder still lacking bodies, newest first.
pub fn missing_bodies(conn: &Connection, folder_id: i64, limit: i64) -> Result<Vec<(i64, i64)>> {
    missing_bodies_at(conn, folder_id, limit, now_ms(), None)
}

pub fn missing_bodies_since(
    conn: &Connection,
    folder_id: i64,
    limit: i64,
    cutoff_ms: Option<i64>,
) -> Result<Vec<(i64, i64)>> {
    missing_bodies_at(conn, folder_id, limit, now_ms(), cutoff_ms)
}

/// One newest-first page for background body planning. The `(date, id)`
/// cursor remains stable when many messages have the same timestamp.
pub fn missing_bodies_page_since(
    conn: &Connection,
    folder_id: i64,
    limit: i64,
    cutoff_ms: Option<i64>,
    cursor: Option<(i64, i64)>,
) -> Result<Vec<(i64, i64, i64)>> {
    let (before_date, before_id) = cursor.unwrap_or((i64::MAX, i64::MAX));
    let mut stmt = conn.prepare(
        "SELECT m.id, m.uid, m.date
         FROM messages m
         JOIN folders f ON f.id = m.folder_id
         JOIN accounts a ON a.id = m.account_id
         LEFT JOIN sync_failures sf
           ON sf.stage = 'content' AND sf.message_id = m.id
         WHERE m.folder_id = ?1 AND m.uid IS NOT NULL AND m.body_state = 'none'
           AND (a.provider = 'gmail' OR COALESCE(f.role, '') <> 'all')
           AND (sf.id IS NULL OR sf.next_retry_at IS NULL OR sf.next_retry_at <= ?3)
           AND (?4 IS NULL OR COALESCE(m.internal_date, m.date) >= ?4)
           AND (m.date, m.id) < (?5, ?6)
         ORDER BY m.date DESC, m.id DESC LIMIT ?2",
    )?;
    Ok(stmt
        .query_map(
            params![
                folder_id,
                limit,
                now_ms(),
                cutoff_ms,
                before_date,
                before_id
            ],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

fn missing_bodies_at(
    conn: &Connection,
    folder_id: i64,
    limit: i64,
    now: i64,
    cutoff_ms: Option<i64>,
) -> Result<Vec<(i64, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT m.id, m.uid
         FROM messages m
         JOIN folders f ON f.id = m.folder_id
         JOIN accounts a ON a.id = m.account_id
         LEFT JOIN sync_failures sf
           ON sf.stage = 'content' AND sf.message_id = m.id
         WHERE m.folder_id = ?1 AND m.uid IS NOT NULL AND m.body_state = 'none'
           AND (a.provider = 'gmail' OR COALESCE(f.role, '') <> 'all')
           AND (sf.id IS NULL OR sf.next_retry_at IS NULL OR sf.next_retry_at <= ?3)
           AND (?4 IS NULL OR COALESCE(m.internal_date, m.date) >= ?4)
         ORDER BY m.date DESC LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(params![folder_id, limit, now, cutoff_ms], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{repo::sync_failures, testutil};

    #[test]
    fn missing_body_pages_cover_equal_timestamps_without_repeating_ids() {
        let conn = testutil::conn();
        testutil::seed_account(&conn);
        let (thread, _) = testutil::seed_message(&conn, "sender@example.com", "Subject", false);
        for uid in 2..=7 {
            conn.execute(
                "INSERT INTO messages (thread_id, account_id, folder_id, uid, message_id, subject, date)
                 VALUES (?1, 1, 1, ?2, 'page-' || ?2, 'Subject', 1000)",
                params![thread, uid],
            )
            .unwrap();
        }
        let mut cursor = None;
        let mut ids = Vec::new();
        loop {
            let page = missing_bodies_page_since(&conn, 1, 2, None, cursor).unwrap();
            if page.is_empty() {
                break;
            }
            cursor = page.last().map(|(id, _, date)| (*date, *id));
            ids.extend(page.into_iter().map(|(id, _, _)| id));
        }
        assert_eq!(ids.len(), 7);
        assert!(ids.windows(2).all(|pair| pair[0] > pair[1]));
        let plan = conn
            .prepare(
                "EXPLAIN QUERY PLAN SELECT m.id, m.uid, m.date FROM messages m
                 JOIN folders f ON f.id = m.folder_id
                 JOIN accounts a ON a.id = m.account_id
                 LEFT JOIN sync_failures sf ON sf.stage = 'content' AND sf.message_id = m.id
                 WHERE m.folder_id = 1 AND m.uid IS NOT NULL AND m.body_state = 'none'
                   AND (a.provider = 'gmail' OR COALESCE(f.role, '') <> 'all')
                   AND (sf.id IS NULL OR sf.next_retry_at IS NULL OR sf.next_retry_at <= 0)
                   AND (m.date, m.id) < (1000, 9999)
                 ORDER BY m.date DESC, m.id DESC LIMIT 2",
            )
            .unwrap()
            .query_map([], |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert!(
            plan.iter()
                .any(|step| step.contains("idx_messages_pending_body_page")
                    && step.contains("date<?")),
            "unexpected body-page plan: {plan:?}"
        );
    }

    #[test]
    fn missing_body_page_defers_recorded_failure_without_a_memory_skip_set() {
        let conn = testutil::conn();
        testutil::seed_account(&conn);
        let (_, failed_id) = testutil::seed_message(&conn, "sender@example.com", "Subject", false);
        let first = missing_bodies_page_since(&conn, 1, 2, None, None).unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].0, failed_id);

        sync_failures::record_content(&conn, failed_id, Some(now_ms() + 60_000), "omitted")
            .unwrap();
        assert!(
            missing_bodies_page_since(&conn, 1, 2, None, None)
                .unwrap()
                .is_empty()
        );
        sync_failures::clear_content(&conn, failed_id).unwrap();
        assert_eq!(
            missing_bodies_page_since(&conn, 1, 2, None, None).unwrap()[0].0,
            failed_id
        );
    }

    #[test]
    fn preview_selects_only_the_newest_message_without_fetching_history() {
        let conn = testutil::conn();
        testutil::seed_account(&conn);
        let (thread, old) = testutil::seed_message(&conn, "sender@example.com", "Subject", false);
        conn.execute("UPDATE messages SET date = 1 WHERE id = ?1", [old])
            .unwrap();
        for uid in 100..200 {
            conn.execute(
                "INSERT INTO messages (thread_id, account_id, folder_id, uid, message_id, subject, from_addr, date)
                 VALUES (?1, 1, 1, ?2, 'preview-' || ?2, 'Subject', 'sender@example.com', 2)",
                params![thread, uid],
            ).unwrap();
        }
        let newest = conn.last_insert_rowid();
        assert_eq!(latest_in_thread(&conn, thread).unwrap(), Some(newest));
        assert_eq!(latest_in_thread(&conn, -1).unwrap(), None);
        assert!(begin_body_fetch(&conn, newest).unwrap());
        let claimed: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE body_state = 'fetching'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(claimed, 1);
    }

    fn received(subject: &str, snippet: &str, is_outgoing: bool) -> NewMessage {
        NewMessage {
            account_id: 1,
            folder_id: 1,
            uid: None,
            message_id: None,
            gm_msgid: None,
            gm_thrid: None,
            subject: subject.into(),
            from: None,
            to: Vec::new(),
            cc: Vec::new(),
            bcc: Vec::new(),
            date: 1000,
            internal_date: None,
            is_read: false,
            is_starred: false,
            is_draft: false,
            is_outgoing,
            is_automated: true,
            has_attachments: false,
            size: None,
            snippet: snippet.into(),
            references: Vec::new(),
            list_unsubscribe: None,
            list_unsubscribe_post: None,
            sender_addr: None,
            sender_verification: SenderVerification::None,
        }
    }

    #[test]
    fn verification_codes_follow_the_text_a_message_shows() {
        let conn = testutil::conn();
        testutil::seed_account(&conn);
        conn.execute(
            "INSERT INTO threads (id, account_id, subject_norm) VALUES (10, 1, 'code')",
            [],
        )
        .unwrap();
        let code = |id: i64| -> Option<String> {
            conn.query_row(
                "SELECT verification_code FROM messages WHERE id = ?1",
                [id],
                |row| row.get(0),
            )
            .unwrap()
        };

        // A preview that shows the code is enough until the body arrives.
        let previewed = insert(
            &conn,
            &received("Your Acme code", "Your verification code is 482913", false),
            10,
        )
        .unwrap();
        assert_eq!(code(previewed).as_deref(), Some("482913"));
        // Sent mail never offers a code, even one it quotes.
        let sent = insert(
            &conn,
            &received(
                "Re: Your Acme code",
                "Your verification code is 482913",
                true,
            ),
            10,
        )
        .unwrap();
        assert_eq!(code(sent), None);
        store_body(
            &conn,
            sent,
            Some("Your verification code is 482913"),
            None,
            None,
            false,
            None,
        )
        .unwrap();
        assert_eq!(code(sent), None);

        // The full text replaces what the preview showed.
        let cut_off = insert(
            &conn,
            &received(
                "Please verify your device",
                "To complete the sign in, enter the",
                false,
            ),
            10,
        )
        .unwrap();
        assert_eq!(code(cut_off), None);
        store_body(
            &conn,
            cut_off,
            None,
            Some(
                "<p>To complete the sign in, enter the verification code.</p>\
                 <p>Verification code: 551203</p>",
            ),
            None,
            false,
            Some("To complete the sign in, enter the verification code."),
        )
        .unwrap();
        assert_eq!(code(cut_off).as_deref(), Some("551203"));
        store_body(
            &conn,
            previewed,
            Some("Thanks for joining Acme."),
            None,
            None,
            false,
            Some("Thanks for joining Acme."),
        )
        .unwrap();
        assert_eq!(code(previewed), None);
    }

    #[test]
    fn code_bodies_are_read_early_only_for_recent_received_mail_without_one() {
        let conn = testutil::conn();
        testutil::seed_account(&conn);
        let now = now_ms();
        let since = now - 24 * 60 * 60 * 1000;
        let seed = |date: i64, update: &str| {
            let (thread, message) =
                testutil::seed_message(&conn, "noreply@acme.test", "Verify your device", true);
            conn.execute(
                "UPDATE messages SET date = ?2 WHERE id = ?1",
                params![message, date],
            )
            .unwrap();
            if !update.is_empty() {
                conn.execute(
                    &format!("UPDATE messages SET {update} WHERE id = ?1"),
                    [message],
                )
                .unwrap();
            }
            (thread, message)
        };
        let (fresh, fresh_message) = seed(now, "");
        let (old, _) = seed(since - 1, "");
        let (sent, _) = seed(now, "is_outgoing = 1");
        let (coded, _) = seed(now, "verification_code = '482913'");
        let (read, _) = seed(now, "body_state = 'cached'");
        let threads = [fresh, old, sent, coded, read];
        let wanted = pending_code_bodies(&conn, &threads, since).unwrap();
        assert_eq!(
            wanted
                .iter()
                .map(|(message, account, ..)| (*message, *account))
                .collect::<Vec<_>>(),
            [(fresh_message, 1)]
        );

        // A body that failed to download waits for its retry.
        sync_failures::record_content(&conn, fresh_message, Some(now + 60_000), "omitted").unwrap();
        assert!(
            pending_code_bodies(&conn, &threads, since)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn body_fetch_claim_deduplicates_and_can_be_released() {
        let conn = testutil::conn();
        testutil::seed_account(&conn);
        let (_, message_id) = testutil::seed_message(&conn, "sender@test.dev", "Subject", false);

        assert!(begin_body_fetch(&conn, message_id).unwrap());
        assert!(!begin_body_fetch(&conn, message_id).unwrap());
        cancel_body_fetch(&conn, message_id).unwrap();
        assert!(begin_body_fetch(&conn, message_id).unwrap());
    }

    /// The batched list_for_thread must return exactly what per-message
    /// detail() calls would, in date order, across the body/attachment
    /// combinations: cached body + attachments, cached body only, no body.
    #[test]
    fn list_for_thread_matches_per_message_detail() {
        let c = testutil::conn();
        testutil::seed_account(&c);
        let (thread_id, first_id) = testutil::seed_message(&c, "a@test.dev", "Subject", false);
        let mut ids = vec![first_id];
        for i in 2..=3 {
            c.execute(
                "INSERT INTO messages (thread_id, account_id, folder_id, uid, message_id,
                 subject, from_addr, date, is_read, is_draft, is_outgoing)
                 VALUES (?1, 1, 1, ?2, 'mid-x-' || ?2, 'Subject', 'a@test.dev', ?3, 0, 0, 0)",
                params![thread_id, 100 + i, 1000 + i],
            )
            .unwrap();
            ids.push(c.last_insert_rowid());
        }
        store_body(
            &c,
            ids[0],
            Some("plain"),
            Some("<p>html</p>"),
            None,
            true,
            None,
        )
        .unwrap();
        store_body(&c, ids[1], Some("only text"), None, None, false, None).unwrap();
        // ids[2] stays body_state = 'none'.
        c.execute(
            "INSERT INTO attachments (message_id, filename, mime_type, size, is_inline, imap_section)
             VALUES (?1, 'a.png', 'image/png', 10, 1, '2'), (?1, 'b.pdf', 'application/pdf', 20, 0, '3')",
            params![ids[0]],
        )
        .unwrap();

        let batched = list_for_thread(&c, thread_id).unwrap();
        assert_eq!(batched.len(), 3);
        let looped: Vec<MessageDetail> =
            batched.iter().map(|m| detail(&c, m.id).unwrap()).collect();
        for (b, l) in batched.iter().zip(&looped) {
            assert_eq!(b.id, l.id);
            assert_eq!(b.text_body, l.text_body);
            assert_eq!(b.html_body, l.html_body);
            assert_eq!(b.body_state, l.body_state);
            assert_eq!(b.attachments.len(), l.attachments.len());
            for (ba, la) in b.attachments.iter().zip(&l.attachments) {
                assert_eq!(ba.id, la.id);
                assert_eq!(ba.filename, la.filename);
                assert_eq!(ba.is_inline, la.is_inline);
            }
        }
        // Date order preserved.
        let dates: Vec<i64> = batched.iter().map(|m| m.date).collect();
        let mut sorted = dates.clone();
        sorted.sort_unstable();
        assert_eq!(dates, sorted);
    }

    #[test]
    fn thread_outline_keeps_headers_but_omits_body_and_attachments() {
        let c = testutil::conn();
        testutil::seed_account(&c);
        let (thread_id, message_id) =
            testutil::seed_message(&c, "sender@test.dev", "Subject", false);
        store_body(
            &c,
            message_id,
            Some("plain body"),
            Some("<p>rich body</p>"),
            None,
            true,
            None,
        )
        .unwrap();
        c.execute(
            "INSERT INTO attachments (message_id, filename, mime_type, size, is_inline)
             VALUES (?1, 'report.pdf', 'application/pdf', 42, 0)",
            [message_id],
        )
        .unwrap();

        let outline = list_outline_for_thread(&c, thread_id).unwrap();

        assert_eq!(outline.len(), 1);
        assert_eq!(outline[0].id, message_id);
        assert_eq!(outline[0].subject, "Subject");
        assert_eq!(outline[0].body_state, "cached");
        assert!(outline[0].text_body.is_none());
        assert!(outline[0].html_body.is_none());
        assert!(outline[0].attachments.is_empty());
    }

    /// A thread whose rows predate the List-Unsubscribe header fetch offers no
    /// stored header, but its cached raws are candidates for recovery; once one
    /// is backfilled it becomes the thread's unsubscribe target.
    #[test]
    fn unsubscribe_target_falls_back_to_raw_candidates() {
        let c = testutil::conn();
        testutil::seed_account(&c);
        let (thread_id, msg_id) = testutil::seed_message(&c, "news@test.dev", "Weekly", false);
        assert_eq!(thread_unsubscribe_message(&c, thread_id).unwrap(), None);
        // No raw on disk yet: nothing to re-read.
        assert!(
            thread_unsubscribe_candidates(&c, thread_id)
                .unwrap()
                .is_empty()
        );

        store_body(
            &c,
            msg_id,
            Some("hi"),
            None,
            Some("/cache/x.eml"),
            false,
            None,
        )
        .unwrap();
        assert_eq!(
            thread_unsubscribe_candidates(&c, thread_id).unwrap(),
            vec![(msg_id, "/cache/x.eml".to_string())]
        );

        set_list_unsubscribe(
            &c,
            msg_id,
            "<https://x.dev/u>",
            Some("List-Unsubscribe=One-Click"),
        )
        .unwrap();
        assert_eq!(
            thread_unsubscribe_message(&c, thread_id).unwrap(),
            Some(msg_id)
        );
        // Backfilled rows drop out of the candidate list.
        assert!(
            thread_unsubscribe_candidates(&c, thread_id)
                .unwrap()
                .is_empty()
        );
        let d = detail(&c, msg_id).unwrap();
        assert_eq!(d.list_unsubscribe.as_deref(), Some("<https://x.dev/u>"));
        assert_eq!(
            d.list_unsubscribe_post.as_deref(),
            Some("List-Unsubscribe=One-Click")
        );
    }

    #[test]
    fn content_progress_counts_only_remotely_fetchable_messages() {
        let c = testutil::conn();
        testutil::seed_account(&c);
        let (_, cached_id) = testutil::seed_message(&c, "one@test.dev", "Cached", false);
        let (_, missing_id) = testutil::seed_message(&c, "two@test.dev", "Missing", false);
        c.execute(
            "UPDATE messages SET body_state = 'cached' WHERE id = ?1",
            params![cached_id],
        )
        .unwrap();
        // Neither local drafts nor rows detached from a remote UID are
        // actionable by the body/content pool.
        c.execute(
            "INSERT INTO messages (account_id, subject, date, is_draft, folder_id, uid)
             VALUES (1, 'Local', 1, 1, NULL, NULL),
                    (1, 'No UID', 2, 0, 1, NULL)",
            [],
        )
        .unwrap();
        // Generic IMAP accounts intentionally skip a special-use All folder;
        // rows left there by an older build must not hold progress open.
        c.execute_batch(
            "INSERT INTO folders (id, account_id, path, role)
             VALUES (2, 1, 'All', 'all');
             INSERT INTO messages (account_id, subject, date, folder_id, uid)
             VALUES (1, 'Skipped duplicate', 3, 2, 9)",
        )
        .unwrap();
        sync_failures::record_content_at(&c, missing_id, None, "decode", 10).unwrap();

        assert_eq!(
            content_progress(&c, 1).unwrap(),
            ContentProgress {
                done: 1,
                total: 2,
                failed: 1,
            }
        );
        assert_eq!(body_progress(&c, 1).unwrap(), (1, 2));

        c.execute(
            "UPDATE messages SET date = CASE id WHEN ?1 THEN 100 ELSE 200 END
             WHERE id IN (?1, ?2)",
            params![cached_id, missing_id],
        )
        .unwrap();
        assert_eq!(
            content_progress_since(&c, 1, Some(150)).unwrap(),
            ContentProgress {
                done: 0,
                total: 1,
                failed: 1,
            }
        );
    }

    #[test]
    fn jmap_adoption_never_relies_on_message_id_alone() {
        let c = testutil::conn();
        testutil::seed_account(&c);
        let (thread_id, message_id) = testutil::seed_message(&c, "one@test.dev", "Subject", false);
        let rfc_message_id = format!("mid-{thread_id}");
        c.execute(
            "UPDATE messages SET size=1234,internal_date=1000 WHERE id=?1",
            params![message_id],
        )
        .unwrap();
        assert_eq!(
            jmap_adoption_candidate(&c, 1, &rfc_message_id, "Subject", 1000, 1234)
                .unwrap()
                .map(|row| row.id),
            Some(message_id)
        );
        assert!(
            jmap_adoption_candidate(&c, 1, &rfc_message_id, "Other", 1000, 1234)
                .unwrap()
                .is_none()
        );
        c.execute(
            "INSERT INTO messages(account_id,folder_id,message_id,subject,date,internal_date,size)
             VALUES(1,1,?1,'Subject',1000,1000,1234)",
            params![rfc_message_id],
        )
        .unwrap();
        assert!(
            jmap_adoption_candidate(&c, 1, &format!("mid-{thread_id}"), "Subject", 1000, 1234)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn jmap_attachment_listing_queue_reads_its_partial_index() {
        let c = testutil::conn();
        let plan = c
            .prepare(&format!(
                "EXPLAIN QUERY PLAN {JMAP_ATTACHMENT_LISTING_QUEUE}"
            ))
            .unwrap()
            .query_map(params![1, 10], |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
            .join("\n");
        assert!(
            plan.contains("idx_messages_jmap_attachments_pending"),
            "{plan}"
        );
        assert!(!plan.contains("TEMP B-TREE"), "{plan}");
    }

    #[test]
    fn sent_adoption_does_not_guess_from_subject_and_time() {
        let c = testutil::conn();
        testutil::seed_account(&c);
        testutil::seed_message(&c, "me@test.dev", "Status", false);
        c.execute("UPDATE messages SET is_outgoing=1, uid=NULL, message_id='local-a', raw_path='sent.eml', date=1000000", []).unwrap();
        assert!(
            outgoing_sent_adoption_candidate(&c, 1, 1, "webmail-b")
                .unwrap()
                .is_none()
        );
        assert!(
            outgoing_sent_adoption_candidate(&c, 1, 1, "")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn sent_adoption_requires_unique_local_submission_in_this_account_and_folder() {
        let c = testutil::conn();
        testutil::seed_account(&c);
        let (_, id) = testutil::seed_message(&c, "me@test.dev", "Subject", false);
        c.execute("UPDATE messages SET is_outgoing=1, uid=NULL, message_id='submission-a', raw_path='sent.eml'", []).unwrap();
        assert_eq!(
            outgoing_sent_adoption_candidate(&c, 1, 1, "submission-a")
                .unwrap()
                .map(|r| r.id),
            Some(id)
        );
        for (account, folder) in [(2, 1), (1, 2)] {
            assert!(
                outgoing_sent_adoption_candidate(&c, account, folder, "submission-a")
                    .unwrap()
                    .is_none()
            );
        }
        for update in ["uid=42", "is_outgoing=0", "raw_path=NULL"] {
            c.execute(&format!("UPDATE messages SET {update}"), [])
                .unwrap();
            assert!(
                outgoing_sent_adoption_candidate(&c, 1, 1, "submission-a")
                    .unwrap()
                    .is_none()
            );
            c.execute(
                "UPDATE messages SET uid=NULL, is_outgoing=1, raw_path='sent.eml'",
                [],
            )
            .unwrap();
        }
        let (_, second) = testutil::seed_message(&c, "me@test.dev", "Subject", false);
        c.execute("UPDATE messages SET uid=NULL, is_outgoing=1, raw_path='other.eml', message_id='submission-a' WHERE id=?1", [second]).unwrap();
        assert!(
            outgoing_sent_adoption_candidate(&c, 1, 1, "submission-a")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn missing_bodies_obeys_content_retry_deadlines_and_all_folder_policy() {
        let c = testutil::conn();
        testutil::seed_account(&c);
        let (_, no_failure) = testutil::seed_message(&c, "one@test.dev", "No failure", false);
        let (_, future) = testutil::seed_message(&c, "two@test.dev", "Future retry", false);
        let (_, due) = testutil::seed_message(&c, "three@test.dev", "Due retry", false);
        let (_, no_deadline) =
            testutil::seed_message(&c, "four@test.dev", "Retry without deadline", false);
        const NOW: i64 = 1_000;
        sync_failures::record_content_at(&c, future, Some(NOW + 1), "fetch", 10).unwrap();
        sync_failures::record_content_at(&c, due, Some(NOW), "fetch", 10).unwrap();
        sync_failures::record_content_at(&c, no_deadline, None, "decode", 10).unwrap();

        let eligible = missing_bodies_at(&c, 1, 20, NOW, None).unwrap();
        let eligible: std::collections::HashSet<i64> =
            eligible.into_iter().map(|(id, _)| id).collect();
        assert_eq!(
            eligible,
            std::collections::HashSet::from([no_failure, due, no_deadline])
        );
        assert!(!eligible.contains(&future));

        c.execute(
            "UPDATE messages SET date = CASE id WHEN ?1 THEN 100 ELSE 200 END
             WHERE id IN (?1, ?2)",
            params![no_failure, due],
        )
        .unwrap();
        let bounded = missing_bodies_at(&c, 1, 20, NOW, Some(150)).unwrap();
        assert!(!bounded.iter().any(|(id, _)| *id == no_failure));
        assert!(bounded.iter().any(|(id, _)| *id == due));

        // Generic IMAP accounts must not download duplicate content from the
        // special-use All folder. Gmail keeps All Mail as its canonical copy.
        c.execute(
            "INSERT INTO folders (id, account_id, path, role)
             VALUES (2, 1, 'All', 'all')",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO messages (account_id, folder_id, uid, subject, date)
             VALUES (1, 2, 99, 'All copy', 2)",
            [],
        )
        .unwrap();
        assert!(missing_bodies_at(&c, 2, 20, NOW, None).unwrap().is_empty());
        c.execute(
            "UPDATE accounts SET provider = 'gmail', auth_kind = 'oauth2',
                    mail_protocol = 'gmail'
             WHERE id = 1",
            [],
        )
        .unwrap();
        assert_eq!(missing_bodies_at(&c, 2, 20, NOW, None).unwrap().len(), 1);
    }

    #[test]
    fn mime_plan_and_imap_section_roundtrip_without_replacing_legacy_ids() {
        let c = testutil::conn();
        testutil::seed_account(&c);
        let (_, message_id) = testutil::seed_message(&c, "a@test.dev", "MIME", false);
        let plan = MimePlan {
            version: crate::mime::MIME_PLAN_VERSION,
            text_sections: vec![crate::mime::PlannedTextSection {
                section: "1".into(),
                kind: crate::mime::TextSectionKind::Plain,
                mime_type: "text/plain".into(),
                charset: Some("utf-8".into()),
                transfer_encoding: "quoted-printable".into(),
                size: 42,
            }],
            attachments: Vec::new(),
        };
        set_mime_plan(&c, message_id, Some(&plan)).unwrap();
        assert_eq!(mime_plan(&c, message_id).unwrap(), Some(plan.clone()));

        c.execute(
            "INSERT INTO attachments (message_id, part_id, filename, mime_type, file_path)
             VALUES (?1, 'legacy-2', 'a.pdf', 'application/pdf', '/cache/a.pdf')",
            params![message_id],
        )
        .unwrap();
        let attachment_id = c.last_insert_rowid();
        let planned = PlannedAttachment {
            section: "2".into(),
            filename: Some("a.pdf".into()),
            mime_type: "application/pdf".into(),
            size: 900,
            content_id: None,
            is_inline: false,
            transfer_encoding: "base64".into(),
        };
        assert_eq!(
            upsert_planned_attachments(&c, message_id, std::slice::from_ref(&planned)).unwrap(),
            vec![attachment_id]
        );
        assert_eq!(
            upsert_planned_attachments(&c, message_id, &[planned]).unwrap(),
            vec![attachment_id]
        );
        assert_eq!(
            attachment_by_imap_section(&c, message_id, "2").unwrap(),
            Some(attachment_id)
        );
        replace_attachments(
            &c,
            message_id,
            &[NewAttachment {
                message_id,
                part_id: Some("0"),
                filename: Some("a.pdf"),
                mime_type: Some("application/pdf"),
                size: Some(900),
                content_id: None,
                is_inline: false,
            }],
        )
        .unwrap();
        let legacy: (String, String, i64) = c
            .query_row(
                "SELECT part_id, file_path, size FROM attachments WHERE id = ?1",
                params![attachment_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(legacy, ("0".into(), "/cache/a.pdf".into(), 900));
        assert_eq!(
            attachment_by_imap_section(&c, message_id, "2").unwrap(),
            Some(attachment_id)
        );
        assert_eq!(
            c.query_row(
                "SELECT COUNT(*) FROM attachments WHERE message_id = ?1",
                params![message_id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
            1
        );
    }

    #[test]
    fn local_automation_annotations_are_exposed_without_rewriting_source_fields() {
        let c = testutil::conn();
        testutil::seed_account(&c);
        let (_thread_id, message_id) =
            testutil::seed_message(&c, "billing@vendor.test", "July invoice", false);
        c.execute(
            "UPDATE messages SET local_subject_prefix = '[FINANCE] ',
                                 local_body_note = 'Send to accounts payable.'
             WHERE id = ?1",
            params![message_id],
        )
        .unwrap();

        let shown = detail(&c, message_id).unwrap();
        assert_eq!(shown.subject, "[FINANCE] July invoice");
        assert_eq!(
            shown.automation_note.as_deref(),
            Some("Send to accounts payable.")
        );
        let stored: String = c
            .query_row(
                "SELECT subject FROM messages WHERE id = ?1",
                params![message_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, "July invoice");
    }

    fn relink_pending(conn: &Connection, id: i64) -> bool {
        conn.query_row(
            "SELECT relink_pending FROM messages WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )
        .unwrap()
    }

    #[test]
    fn the_copy_waiting_for_the_server_is_linked_first() {
        let conn = testutil::conn();
        testutil::seed_account(&conn);
        let (_, linked) = testutil::seed_message(&conn, "sender@example.com", "Copy", false);
        let (_, moved) = testutil::seed_message(&conn, "sender@example.com", "Copy", false);
        let (_, waiting) = testutil::seed_message(&conn, "sender@example.com", "Copy", false);
        conn.execute("UPDATE messages SET message_id = 'same@example.com'", [])
            .unwrap();
        conn.execute(
            "UPDATE messages SET uid = NULL WHERE id IN (?1, ?2)",
            params![moved, waiting],
        )
        .unwrap();
        let first = |conn: &Connection| {
            by_message_id(conn, 1, "same@example.com")
                .unwrap()
                .unwrap()
                .id
        };
        assert_eq!(first(&conn), moved);

        conn.execute(
            "UPDATE messages SET relink_pending = 1 WHERE id = ?1",
            params![waiting],
        )
        .unwrap();
        assert_eq!(first(&conn), waiting);
        // A move keeps it waiting; only a UID links it.
        set_uid_and_folder(&conn, waiting, 1, None).unwrap();
        assert!(relink_pending(&conn, waiting));
        set_uid_and_folder(&conn, waiting, 1, Some(40)).unwrap();
        assert!(!relink_pending(&conn, waiting));
        assert_eq!(first(&conn), moved);
        assert!(!relink_pending(&conn, linked));
    }

    #[test]
    fn mail_still_waiting_once_the_history_is_complete_goes() {
        let conn = testutil::conn();
        testutil::seed_account(&conn);
        conn.execute_batch(
            "INSERT INTO folders (id, account_id, path, role) VALUES (2, 1, 'Sent', 'sent');
             INSERT INTO folders (id, account_id, path, role) VALUES (3, 1, 'Drafts', 'drafts');
             INSERT INTO folders (id, account_id, path, role, missing_on_server)
             VALUES (4, 1, 'Sent Messages', 'sent', 1);
             INSERT INTO folders (id, account_id, path, jmap_id) VALUES (5, 1, 'Notes', 'notes');
             INSERT INTO folders (id, account_id, path, missing_on_server) VALUES (6, 1, 'Gone', 1);",
        )
        .unwrap();
        let seed = |subject: &str, folder_id: i64, waiting: bool, received: i64| {
            let (thread_id, id) =
                testutil::seed_message(&conn, "sender@example.com", subject, false);
            conn.execute(
                "UPDATE messages SET folder_id = ?2, uid = NULL, relink_pending = ?3,
                        internal_date = ?4, raw_path = ?5
                 WHERE id = ?1",
                params![id, folder_id, waiting, received, format!("{subject}.eml")],
            )
            .unwrap();
            (thread_id, id)
        };
        let (gone_thread, gone) = seed("gone", 1, true, 5_000);
        let (_, busy) = seed("busy", 6, true, 1_000);
        let (_, draft) = seed("draft", 5, true, 3_000);
        let (_, sent_copy) = seed("sent", 4, false, 2_000);
        let (_, linked) = testutil::seed_message(&conn, "sender@example.com", "linked", false);
        let linked_uid = get_row(&conn, linked).unwrap().unwrap().uid;
        conn.execute(
            "INSERT INTO drafts_meta (message_id) VALUES (?1)",
            params![draft],
        )
        .unwrap();
        conn.execute(
            "UPDATE messages SET is_outgoing = 1 WHERE id = ?1",
            params![sent_copy],
        )
        .unwrap();
        crate::db::repo::actions::enqueue(
            &conn,
            1,
            "star",
            Some(busy),
            None,
            &serde_json::json!({}),
            None,
        )
        .unwrap();

        // Mail with work queued waits for it, however old.
        assert_eq!(oldest_relink_pending(&conn, 1).unwrap(), Some(3_000));
        let (threads, paths) = drop_unrelinked_imap_mail(&conn, 1).unwrap();
        assert_eq!(paths, ["gone.eml"]);
        assert!(threads.contains(&gone_thread));
        assert!(get_row(&conn, gone).unwrap().is_none());

        let folder_of = |id: i64| get_row(&conn, id).unwrap().unwrap().folder_id;
        assert_eq!(folder_of(busy), Some(6));
        assert!(relink_pending(&conn, busy));
        assert_eq!(folder_of(draft), Some(3));
        assert!(!relink_pending(&conn, draft));
        assert_eq!(folder_of(sent_copy), Some(2));
        assert_eq!(folder_of(linked), Some(1));
        assert_eq!(get_row(&conn, linked).unwrap().unwrap().uid, linked_uid);
        let folders = conn
            .prepare("SELECT id FROM folders ORDER BY id")
            .unwrap()
            .query_map([], |row| row.get::<_, i64>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(folders, [1, 2, 3, 6]);
        assert_eq!(oldest_relink_pending(&conn, 1).unwrap(), None);
    }
}
