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
/// re-link after UIDVALIDITY resets and to dedupe Gmail label-folders).
pub fn by_message_id(
    conn: &Connection,
    account_id: i64,
    message_id: &str,
) -> Result<Option<MessageRow>> {
    let mut stmt = conn.prepare_cached(basic_message_select!(
        "WHERE account_id = ?1 AND message_id = ?2 LIMIT 1"
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
    let rows = stmt
        .query_map(
            params![account_id, message_id, subject, size, received_at_ms],
            row_basic,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok((rows.len() == 1).then(|| rows[0].clone()))
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
    conn.execute(
        "INSERT INTO messages (account_id, thread_id, folder_id, uid, message_id, gm_msgid, gm_thrid,
            subject, from_name, from_addr, to_json, cc_json, bcc_json, date, internal_date,
            is_read, is_starred, is_draft, is_outgoing, is_automated, has_attachments, size, snippet,
            list_unsubscribe, list_unsubscribe_post, sender_addr, sender_verification)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,?27)",
        params![
            m.account_id,
            thread_id,
            m.folder_id,
            m.uid,
            m.message_id,
            m.gm_msgid,
            m.gm_thrid,
            m.subject,
            m.from.as_ref().and_then(|a| a.name.clone()),
            m.from.as_ref().map(|a| a.email.clone()),
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

pub fn set_uid_and_folder(
    conn: &Connection,
    id: i64,
    folder_id: i64,
    uid: Option<i64>,
) -> Result<()> {
    conn.execute(
        "UPDATE messages SET folder_id = ?2, uid = ?3 WHERE id = ?1",
        params![id, folder_id, uid],
    )?;
    Ok(())
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
    // Full text is now available: queue the message for semantic embedding.
    // Inference happens off the writer thread (see the embed worker), so this
    // only flips a cheap flag.
    conn.execute(
        "UPDATE messages SET body_state = 'cached', raw_path = COALESCE(?2, raw_path),
                has_attachments = ?3, snippet = COALESCE(?4, snippet),
                embedding_state = 'pending'
         WHERE id = ?1",
        params![id, raw_path, has_attachments as i64, snippet],
    )?;
    Ok(())
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
            "INSERT INTO folders (id, account_id, imap_name, role)
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
            "INSERT INTO folders (id, account_id, imap_name, role)
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
        c.execute("UPDATE accounts SET provider = 'gmail' WHERE id = 1", [])
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
}
