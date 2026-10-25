use crate::error::Result;
use crate::models::*;
use rusqlite::{Connection, OptionalExtension, Row, params};

use super::{parse_addrs, parse_json_column};

fn summary_from_row(row: &Row) -> rusqlite::Result<ThreadSummary> {
    Ok(ThreadSummary {
        id: row.get("id")?,
        account_id: row.get("account_id")?,
        account_email: row.get("account_email")?,
        account_addresses: parse_json_column(&row.get::<_, String>("account_addresses")?, 3)?,
        subject: row.get::<_, Option<String>>("subject")?.unwrap_or_default(),
        snippet: row.get("snippet")?,
        participants: parse_addrs(&row.get::<_, String>("participants_json")?, 6)?,
        last_message_at: row.get("last_message_at")?,
        message_count: row.get("message_count")?,
        unread_count: row.get("unread_count")?,
        is_starred: row.get::<_, i64>("starred_count")? > 0,
        has_attachments: row.get::<_, i64>("attachment_count")? > 0,
        has_replied: row.get::<_, i64>("has_replied")? > 0,
        snoozed_until: row.get("snoozed_until")?,
        labels: parse_id_list(
            &row.get::<_, Option<String>>("label_ids")?
                .unwrap_or_default(),
        ),
        verification_code: row.get("verification_code")?,
        first_attachment: row
            .get::<_, Option<String>>("first_attachment")?
            .map(|json| {
                parse_json_column::<(i64, Option<String>, Option<String>, Option<i64>)>(&json, 16)
            })
            .transpose()?
            .map(|(id, filename, mime_type, size)| AttachmentMeta {
                id,
                filename,
                mime_type,
                size,
                is_inline: false,
            }),
        attachment_total: row.get("attachment_total")?,
    })
}

/// Parse a SQLite `group_concat` result ("1,3,5") into a list of ids.
fn parse_id_list(csv: &str) -> Vec<i64> {
    csv.split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect()
}

const SUMMARY_SELECT: &str = "
    SELECT t.id, t.account_id, a.email AS account_email,
           COALESCE((SELECT json_group_array(si.email)
                     FROM sender_identities si
                     WHERE si.account_id=t.account_id
                       AND (si.is_primary=1 OR si.verification_status='accepted')), '[]')
             AS account_addresses,
           (SELECT COALESCE(m.local_subject_prefix, '') || m.subject
              FROM messages m WHERE m.thread_id = t.id ORDER BY m.date DESC LIMIT 1) AS subject,
           t.snippet, t.participants_json, t.last_message_at, t.message_count,
           t.unread_count, t.starred_count, t.attachment_count,
           EXISTS (
               SELECT 1 FROM messages reply
               JOIN message_refs mr ON mr.message_id = reply.id
               WHERE reply.thread_id = t.id
                 AND reply.is_outgoing = 1
                 AND reply.is_draft = 0
           ) AS has_replied,
           s.wake_at AS snoozed_until,
           (SELECT group_concat(DISTINCT ml.label_id) FROM message_labels ml
              JOIN messages ml_m ON ml_m.id = ml.message_id
              WHERE ml_m.thread_id = t.id) AS label_ids,
           (SELECT m.verification_code
              FROM messages m WHERE m.thread_id = t.id ORDER BY m.date DESC LIMIT 1)
             AS verification_code,
           (SELECT json_array(f.id, f.filename, f.mime_type, f.size)
              FROM messages fm JOIN attachments f ON f.message_id = fm.id
              WHERE fm.thread_id = t.id AND f.is_inline = 0
              ORDER BY fm.date DESC, f.id LIMIT 1) AS first_attachment,
           (SELECT COUNT(*)
              FROM messages fm JOIN attachments f ON f.message_id = fm.id
              WHERE fm.thread_id = t.id AND f.is_inline = 0) AS attachment_total
    FROM threads t
    JOIN accounts a ON a.id = t.account_id
    LEFT JOIN snoozes s ON s.thread_id = t.id";

pub fn create(
    conn: &Connection,
    account_id: i64,
    gm_thrid: Option<&str>,
    subject_norm: &str,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO threads (account_id, gm_thrid, subject_norm) VALUES (?1,?2,?3)",
        params![account_id, gm_thrid, subject_norm],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn by_gm_thrid(conn: &Connection, account_id: i64, gm_thrid: &str) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT id FROM threads WHERE account_id = ?1 AND gm_thrid = ?2 LIMIT 1",
            params![account_id, gm_thrid],
            |r| r.get(0),
        )
        .optional()?)
}

pub fn by_jmap_id(conn: &Connection, account_id: i64, jmap_id: &str) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT id FROM threads WHERE account_id = ?1 AND jmap_id = ?2 LIMIT 1",
            params![account_id, jmap_id],
            |row| row.get(0),
        )
        .optional()?)
}

pub fn create_jmap(
    conn: &Connection,
    account_id: i64,
    jmap_id: &str,
    subject_norm: &str,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO threads (account_id, jmap_id, subject_norm) VALUES (?1, ?2, ?3)",
        params![account_id, jmap_id, subject_norm],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Thread that contains a message whose RFC Message-ID is in `refs`.
pub fn by_references(conn: &Connection, account_id: i64, refs: &[String]) -> Result<Option<i64>> {
    for r in refs {
        let hit: Option<i64> = conn
            .query_row(
                "SELECT m.thread_id FROM messages m
                 WHERE m.account_id = ?1 AND m.message_id = ?2 AND m.thread_id IS NOT NULL
                 LIMIT 1",
                params![account_id, r],
                |row| row.get(0),
            )
            .optional()?;
        if hit.is_some() {
            return Ok(hit);
        }
        // Also match messages that themselves referenced r (sibling replies).
        let hit: Option<i64> = conn
            .query_row(
                "SELECT m.thread_id FROM message_refs mr
                 JOIN messages m ON m.id = mr.message_id
                 WHERE m.account_id = ?1 AND mr.ref_message_id = ?2 AND m.thread_id IS NOT NULL
                 LIMIT 1",
                params![account_id, r],
                |row| row.get(0),
            )
            .optional()?;
        if hit.is_some() {
            return Ok(hit);
        }
    }
    Ok(None)
}

/// Subject fallback: same normalized subject, activity within the window.
pub fn by_subject(
    conn: &Connection,
    account_id: i64,
    subject_norm: &str,
    since_ms: i64,
) -> Result<Option<i64>> {
    if subject_norm.is_empty() {
        return Ok(None);
    }
    Ok(conn
        .query_row(
            "SELECT id FROM threads
             WHERE account_id = ?1 AND subject_norm = ?2 AND last_message_at >= ?3
             ORDER BY last_message_at DESC LIMIT 1",
            params![account_id, subject_norm, since_ms],
            |r| r.get(0),
        )
        .optional()?)
}

/// Recompute a thread's denormalized aggregates from its messages.
pub fn recompute(conn: &Connection, thread_id: i64) -> Result<()> {
    let exists: Option<i64> = conn
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE thread_id = ?1",
            params![thread_id],
            |r| r.get(0),
        )
        .optional()?;
    if exists.unwrap_or(0) == 0 {
        conn.execute("DELETE FROM threads WHERE id = ?1", params![thread_id])?;
        return Ok(());
    }
    conn.execute(
        "UPDATE threads SET
            last_message_at = (SELECT COALESCE(MAX(date),0) FROM messages WHERE thread_id = ?1),
            message_count   = (SELECT COUNT(*) FROM messages WHERE thread_id = ?1),
            unread_count    = (SELECT COUNT(*) FROM messages WHERE thread_id = ?1 AND is_read = 0 AND is_draft = 0),
            starred_count   = (SELECT COUNT(*) FROM messages WHERE thread_id = ?1 AND is_starred = 1),
            attachment_count= (SELECT COUNT(*) FROM messages WHERE thread_id = ?1 AND has_attachments = 1),
            snippet         = COALESCE((SELECT snippet FROM messages WHERE thread_id = ?1 ORDER BY date DESC LIMIT 1), '')
         WHERE id = ?1",
        params![thread_id],
    )?;
    // Participants: distinct senders, most recent first, capped at 5.
    let mut stmt = conn.prepare(
        "SELECT from_name, from_addr, MAX(date) FROM messages
         WHERE thread_id = ?1 AND from_addr IS NOT NULL
         GROUP BY LOWER(from_addr) ORDER BY MAX(date) DESC LIMIT 5",
    )?;
    let parts = stmt
        .query_map(params![thread_id], |r| {
            Ok(Address {
                name: r.get(0)?,
                email: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    conn.execute(
        "UPDATE threads SET participants_json = ?2 WHERE id = ?1",
        params![thread_id, serde_json::to_string(&parts)?],
    )?;
    Ok(())
}

pub fn get_summary(conn: &Connection, thread_id: i64) -> Result<Option<ThreadSummary>> {
    let sql = format!("{SUMMARY_SELECT} WHERE t.id = ?1");
    let mut stmt = conn.prepare_cached(&sql)?;
    Ok(stmt
        .query_row(params![thread_id], summary_from_row)
        .optional()?)
}

/// Summaries for many threads in one query, returned in `thread_ids` order.
/// Ids that no longer exist are silently skipped. Plain `prepare` (not
/// cached): the placeholder arity varies per call and would pollute the
/// statement cache.
pub fn get_summaries(conn: &Connection, thread_ids: &[i64]) -> Result<Vec<ThreadSummary>> {
    if thread_ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = thread_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!("{SUMMARY_SELECT} WHERE t.id IN ({placeholders})");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(
            rusqlite::params_from_iter(thread_ids.iter()),
            summary_from_row,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut by_id: std::collections::HashMap<i64, ThreadSummary> =
        rows.into_iter().map(|t| (t.id, t)).collect();
    Ok(thread_ids
        .iter()
        .filter_map(|id| by_id.remove(id))
        .collect())
}

/// Which inbox tab to filter to. Overlay tabs (Split/AutoLabel) read the single
/// resolved `threads.routed_tab` so a thread appears in exactly one; the
/// Important/Other defaults catch everything still unrouted. Manual (user)
/// labels are a cross-cutting filter, not a routed tab.
pub enum TabFilter {
    Important,
    Other,
    Split(i64),
    AutoLabel(i64),
    ManualLabel(i64),
}

pub struct ListArgs {
    pub view: View,
    pub tab: Option<TabFilter>,
    pub account_id: Option<i64>,
    /// Restrict to a set of accounts, such as a mail profile's; an empty set
    /// lists nothing. Combines with `account_id`.
    pub account_ids: Option<Vec<i64>>,
    /// Restrict to threads with a message in this folder (user-folder view).
    pub folder_id: Option<i64>,
    pub cursor: Option<ThreadCursor>,
    pub limit: i64,
}

/// Important/Other follows the newest incoming message. Requiring every
/// historical message to have the same type made mixed threads match neither
/// tab (for example, an automated notification followed by a human reply).
const HUMAN_BUCKET: &str = "(SELECT m.is_automated FROM messages m
        WHERE m.thread_id = t.id AND m.is_draft = 0 AND m.is_outgoing = 0
        ORDER BY m.date DESC, m.id DESC LIMIT 1) = 0";

const AUTO_BUCKET: &str = "(SELECT m.is_automated FROM messages m
        WHERE m.thread_id = t.id AND m.is_draft = 0 AND m.is_outgoing = 0
        ORDER BY m.date DESC, m.id DESC LIMIT 1) = 1";

fn build_filter(
    args: &ListArgs,
    include_cursor: bool,
) -> (String, Vec<Box<dyn rusqlite::types::ToSql>>) {
    let mut where_clauses: Vec<String> = Vec::new();
    let mut bind: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

    let role_exists = |role: &str, bind: &mut Vec<Box<dyn rusqlite::types::ToSql>>| {
        bind.push(Box::new(role.to_string()));
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
            bind.len(),
        )
    };

    match args.view {
        View::Inbox => {
            let c = role_exists(roles::INBOX, &mut bind);
            where_clauses.push(c);
            where_clauses.push("s.thread_id IS NULL".into());
        }
        View::Starred => where_clauses.push("t.starred_count > 0".into()),
        View::Snoozed => where_clauses.push("s.thread_id IS NOT NULL".into()),
        View::Sent => {
            let c = role_exists(roles::SENT, &mut bind);
            where_clauses.push(c);
        }
        View::Drafts => {
            bind.push(Box::new(roles::DRAFTS.to_string()));
            where_clauses.push(format!(
                "(EXISTS (SELECT 1 FROM messages m JOIN folders f ON f.id = m.folder_id
                          WHERE m.thread_id = t.id AND m.is_draft = 1 AND f.role = ?{0})
                  OR EXISTS (
                       SELECT 1 FROM messages m
                       JOIN accounts ma ON ma.id = m.account_id AND ma.provider = 'gmail'
                       JOIN message_folders mf ON mf.message_id = m.id
                       JOIN folders f ON f.id = mf.folder_id
                       WHERE m.thread_id = t.id AND m.is_draft = 1 AND f.role = ?{0}
                  ))",
                bind.len()
            ));
        }
        View::Done => {
            let c = role_exists(roles::ARCHIVE, &mut bind);
            let inbox = role_exists(roles::INBOX, &mut bind);
            where_clauses.push(c);
            where_clauses.push(format!("NOT {inbox}"));
        }
        View::Trash => {
            let c = role_exists(roles::TRASH, &mut bind);
            where_clauses.push(c);
        }
        View::Spam => {
            let c = role_exists(roles::SPAM, &mut bind);
            where_clauses.push(c);
        }
        View::All => {}
    }

    if let Some(acc) = args.account_id {
        bind.push(Box::new(acc));
        where_clauses.push(format!("t.account_id = ?{}", bind.len()));
    }
    if let Some(account_ids) = &args.account_ids {
        where_clauses.push(super::search::account_set_clause(
            "t.account_id",
            account_ids,
            &mut bind,
        ));
    }
    match &args.tab {
        Some(TabFilter::Important) => where_clauses.push(format!(
            "(t.routed_tab = 'important'
              OR ((t.routed_tab IS NULL OR t.routed_tab = 'pending') AND ({HUMAN_BUCKET})))"
        )),
        Some(TabFilter::Other) => where_clauses.push(format!(
            "(t.routed_tab = 'other'
              OR ((t.routed_tab IS NULL OR t.routed_tab = 'pending') AND ({AUTO_BUCKET})))"
        )),
        Some(TabFilter::Split(id)) => {
            bind.push(Box::new(format!("split:{id}")));
            where_clauses.push(format!("t.routed_tab = ?{}", bind.len()));
        }
        Some(TabFilter::AutoLabel(id)) => {
            bind.push(Box::new(format!("label:{id}")));
            where_clauses.push(format!("t.routed_tab = ?{}", bind.len()));
        }
        Some(TabFilter::ManualLabel(id)) => {
            bind.push(Box::new(*id));
            where_clauses.push(format!(
                "EXISTS (SELECT 1 FROM message_labels ml JOIN messages m ON m.id = ml.message_id
                         WHERE m.thread_id = t.id AND ml.label_id = ?{})",
                bind.len()
            ));
        }
        None => {}
    }
    if let Some(folder_id) = args.folder_id {
        bind.push(Box::new(folder_id));
        where_clauses.push(format!(
            "EXISTS (SELECT 1 FROM messages m
                     WHERE m.thread_id = t.id AND (
                       m.folder_id = ?{0}
                       OR (
                         EXISTS (SELECT 1 FROM accounts ma
                                 WHERE ma.id = m.account_id AND ma.provider = 'gmail')
                         AND EXISTS (SELECT 1 FROM message_folders mf
                                     WHERE mf.message_id = m.id AND mf.folder_id = ?{0})
                       )
                     ))",
            bind.len()
        ));
    }
    if include_cursor && let Some(cur) = args.cursor {
        bind.push(Box::new(cur.last_message_at));
        let timestamp_parameter = bind.len();
        bind.push(Box::new(cur.thread_id));
        let id_parameter = bind.len();
        where_clauses.push(format!(
            "(t.last_message_at, t.id) < (?{timestamp_parameter}, ?{id_parameter})"
        ));
    }

    let where_sql = if where_clauses.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", where_clauses.join(" AND "))
    };
    (where_sql, bind)
}

pub fn list(conn: &Connection, args: &ListArgs) -> Result<ThreadPage> {
    let (sql, bind) = list_sql(args);

    let mut stmt = conn.prepare(&sql)?;
    let params_ref: Vec<&dyn rusqlite::types::ToSql> = bind.iter().map(|b| b.as_ref()).collect();
    let mut threads = stmt
        .query_map(params_ref.as_slice(), summary_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let next_cursor = if threads.len() as i64 > args.limit {
        threads.truncate(args.limit as usize);
        threads.last().map(|thread| ThreadCursor {
            last_message_at: thread.last_message_at,
            thread_id: thread.id,
        })
    } else {
        None
    };
    Ok(ThreadPage {
        threads,
        next_cursor,
    })
}

fn list_sql(args: &ListArgs) -> (String, Vec<Box<dyn rusqlite::types::ToSql>>) {
    let (where_sql, bind) = build_filter(args, true);
    (
        format!(
            "{SUMMARY_SELECT} {where_sql}
             ORDER BY t.last_message_at DESC, t.id DESC LIMIT {}",
            args.limit + 1
        ),
        bind,
    )
}

/// Query-plan details for the exact list shape, including its cursor/filter.
pub fn list_query_plan(conn: &Connection, args: &ListArgs) -> Result<Vec<String>> {
    let (sql, bind) = list_sql(args);
    let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?;
    let params_ref: Vec<&dyn rusqlite::types::ToSql> = bind.iter().map(|b| b.as_ref()).collect();
    Ok(stmt
        .query_map(params_ref.as_slice(), |row| row.get(3))?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Count the exact result set represented by `ListArgs` without materializing
/// thread summaries or walking cursor pages. Keeping this on the same filter
/// builder as `list` prevents sidebar totals from drifting from list contents.
pub fn count(conn: &Connection, args: &ListArgs) -> Result<usize> {
    let (where_sql, bind) = build_filter(args, false);
    let sql = format!(
        "SELECT COUNT(*)
         FROM threads t
         JOIN accounts a ON a.id = t.account_id
         LEFT JOIN snoozes s ON s.thread_id = t.id
         {where_sql}"
    );
    let params_ref: Vec<&dyn rusqlite::types::ToSql> =
        bind.iter().map(|value| value.as_ref()).collect();
    let count = conn.query_row(&sql, params_ref.as_slice(), |row| row.get::<_, i64>(0))?;
    Ok(count.max(0) as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_reports_only_sent_replies_as_replied() {
        let conn = crate::db::testutil::conn();
        crate::db::testutil::seed_account(&conn);
        let (thread_id, _) =
            crate::db::testutil::seed_message(&conn, "sender@example.com", "Update", false);

        assert!(!get_summary(&conn, thread_id).unwrap().unwrap().has_replied);

        let insert_message = |uid: i64,
                              message_id: &str,
                              is_draft: bool,
                              is_outgoing: bool,
                              references_original: bool| {
            let from_addr = if is_outgoing {
                "me@test.dev"
            } else {
                "sender@example.com"
            };
            conn.execute(
                "INSERT INTO messages (thread_id, account_id, folder_id, uid, message_id, subject,
                 from_addr, date, is_read, is_draft, is_outgoing)
                 VALUES (?1, 1, 1, ?2, ?3, 'Re: Update', ?4, 1100, 1, ?5, ?6)",
                params![
                    thread_id,
                    uid,
                    message_id,
                    from_addr,
                    is_draft as i64,
                    is_outgoing as i64
                ],
            )
            .unwrap();
            if references_original {
                let reply_id = conn.last_insert_rowid();
                conn.execute(
                    "INSERT INTO message_refs (message_id, ref_message_id) VALUES (?1, 'mid-1')",
                    params![reply_id],
                )
                .unwrap();
            }
        };

        insert_message(2, "incoming-reply-mid", false, false, true);
        assert!(!get_summary(&conn, thread_id).unwrap().unwrap().has_replied);

        insert_message(3, "draft-reply-mid", true, true, true);
        assert!(!get_summary(&conn, thread_id).unwrap().unwrap().has_replied);

        insert_message(4, "sent-new-message-mid", false, true, false);
        assert!(!get_summary(&conn, thread_id).unwrap().unwrap().has_replied);

        insert_message(5, "sent-reply-mid", false, true, true);
        recompute(&conn, thread_id).unwrap();

        assert!(get_summary(&conn, thread_id).unwrap().unwrap().has_replied);
    }

    #[test]
    fn summary_offers_the_newest_message_code() {
        let conn = crate::db::testutil::conn();
        crate::db::testutil::seed_account(&conn);
        let (thread_id, first) =
            crate::db::testutil::seed_message(&conn, "noreply@acme.test", "Your code", true);
        conn.execute(
            "UPDATE messages SET verification_code = '111111' WHERE id = ?1",
            [first],
        )
        .unwrap();
        let code = || {
            get_summary(&conn, thread_id)
                .unwrap()
                .unwrap()
                .verification_code
        };
        assert_eq!(code().as_deref(), Some("111111"));

        // A newer code replaces the one before it.
        conn.execute(
            "INSERT INTO messages (thread_id, account_id, folder_id, uid, message_id, subject,
             from_addr, date, verification_code)
             VALUES (?1, 1, 1, 2, 'second-code', 'Your code', 'noreply@acme.test', 1100, '222222')",
            [thread_id],
        )
        .unwrap();
        assert_eq!(code().as_deref(), Some("222222"));

        // A reply after it leaves the row without one.
        conn.execute(
            "INSERT INTO messages (thread_id, account_id, folder_id, uid, message_id, subject,
             from_addr, date, is_outgoing)
             VALUES (?1, 1, 1, 3, 'reply', 'Re: Your code', 'me@test.dev', 1200, 1)",
            [thread_id],
        )
        .unwrap();
        assert_eq!(code(), None);
    }

    #[test]
    fn summary_offers_the_newest_file() {
        let conn = crate::db::testutil::conn();
        crate::db::testutil::seed_account(&conn);
        let (thread_id, first) =
            crate::db::testutil::seed_message(&conn, "billing@acme.test", "Invoice", false);
        let summary = || get_summary(&conn, thread_id).unwrap().unwrap();
        let add_file = |message_id: i64, name: &str, size: Option<i64>, inline: bool| {
            conn.execute(
                "INSERT INTO attachments (message_id, filename, mime_type, size, is_inline)
                 VALUES (?1, ?2, 'application/pdf', ?3, ?4)",
                params![message_id, name, size, inline],
            )
            .unwrap();
            conn.last_insert_rowid()
        };

        // Inline images belong to the body.
        add_file(first, "logo.png", Some(900), true);
        assert!(summary().first_attachment.is_none());
        assert_eq!(summary().attachment_total, 0);

        let invoice = add_file(first, "invoice.pdf", Some(52_000), false);
        add_file(first, "terms.pdf", Some(8_000), false);
        let file = summary().first_attachment.unwrap();
        assert_eq!(file.id, invoice);
        assert_eq!(file.filename.as_deref(), Some("invoice.pdf"));
        assert_eq!(file.mime_type.as_deref(), Some("application/pdf"));
        assert_eq!(file.size, Some(52_000));
        assert!(!file.is_inline);
        assert_eq!(summary().attachment_total, 2);

        // A newer message's file leads, and the count spans the thread.
        conn.execute(
            "INSERT INTO messages (thread_id, account_id, folder_id, uid, message_id, subject,
             from_addr, date)
             VALUES (?1, 1, 1, 2, 'receipt', 'Re: Invoice', 'billing@acme.test', 1100)",
            [thread_id],
        )
        .unwrap();
        let receipt = add_file(conn.last_insert_rowid(), "receipt.pdf", None, false);
        let file = summary().first_attachment.unwrap();
        assert_eq!(file.id, receipt);
        assert_eq!(file.size, None);
        assert_eq!(summary().attachment_total, 3);
    }

    #[test]
    fn scalar_count_matches_the_unpaginated_list_filter() {
        let conn = crate::db::testutil::conn();
        crate::db::testutil::seed_account(&conn);
        for subject in ["one", "two", "three"] {
            crate::db::testutil::seed_message(&conn, "sender@example.com", subject, false);
        }

        let args = ListArgs {
            view: View::Inbox,
            tab: None,
            account_id: Some(1),
            account_ids: None,
            folder_id: Some(1),
            cursor: None,
            limit: 2,
        };
        let page = list(&conn, &args).unwrap();

        assert_eq!(page.threads.len(), 2);
        assert!(page.next_cursor.is_some());
        assert_eq!(count(&conn, &args).unwrap(), 3);
    }

    #[test]
    fn account_sets_list_and_count_only_their_accounts() {
        let conn = crate::db::testutil::conn();
        crate::db::testutil::seed_account(&conn);
        let (first, _) = crate::db::testutil::seed_message(&conn, "a@one.dev", "one", false);
        conn.execute(
            "INSERT INTO accounts (id, email, provider, auth_kind, username,
             incoming_host, incoming_port, smtp_host, smtp_port, created_at, mail_protocol, settings_json)
             VALUES (2,'me2@test.dev','generic','password','me2','h',993,'h',587,0,'imap','{\"connection\":{\"incomingSecurity\":\"tls\",\"smtpSecurity\":\"tls\"}}')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO folders (id, account_id, path, role) VALUES (2,2,'INBOX','inbox')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO threads (id, account_id, subject_norm, unread_count, last_message_at)
             VALUES (99, 2, 'two', 1, 1000)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO messages (thread_id, account_id, folder_id, uid, message_id, subject,
             from_addr, date, is_read, is_automated, is_draft, is_outgoing)
             VALUES (99, 2, 2, 99, 'mid-99', 'two', 'b@two.dev', 1000, 0, 0, 0, 0)",
            [],
        )
        .unwrap();

        let listed = |account_ids: Option<Vec<i64>>| {
            let args = ListArgs {
                view: View::Inbox,
                tab: None,
                account_id: None,
                account_ids,
                folder_id: None,
                cursor: None,
                limit: 50,
            };
            let mut ids = list(&conn, &args)
                .unwrap()
                .threads
                .iter()
                .map(|thread| thread.id)
                .collect::<Vec<_>>();
            ids.sort();
            assert_eq!(count(&conn, &args).unwrap(), ids.len());
            ids
        };
        assert_eq!(listed(None), vec![first, 99]);
        assert_eq!(listed(Some(vec![2])), vec![99]);
        assert_eq!(listed(Some(vec![1, 2])), vec![first, 99]);
        assert_eq!(listed(Some(Vec::new())), Vec::<i64>::new());
    }

    #[test]
    fn keyset_cursor_never_skips_threads_with_the_same_timestamp() {
        let conn = crate::db::testutil::conn();
        crate::db::testutil::seed_account(&conn);
        for subject in ["one", "two", "three"] {
            crate::db::testutil::seed_message(&conn, "sender@example.com", subject, false);
        }

        let first = list(
            &conn,
            &ListArgs {
                view: View::Inbox,
                tab: None,
                account_id: None,
                account_ids: None,
                folder_id: None,
                cursor: None,
                limit: 2,
            },
        )
        .unwrap();
        let second = list(
            &conn,
            &ListArgs {
                view: View::Inbox,
                tab: None,
                account_id: None,
                account_ids: None,
                folder_id: None,
                cursor: first.next_cursor,
                limit: 2,
            },
        )
        .unwrap();

        let ids = first
            .threads
            .iter()
            .chain(second.threads.iter())
            .map(|thread| thread.id)
            .collect::<Vec<_>>();
        assert_eq!(ids, vec![3, 2, 1]);
        assert!(second.next_cursor.is_none());
    }

    #[test]
    fn account_keyset_cursor_seeks_the_recent_index() {
        let conn = crate::db::testutil::conn();
        crate::db::testutil::seed_account(&conn);
        for subject in ["one", "two", "three"] {
            crate::db::testutil::seed_message(&conn, "sender@example.com", subject, false);
        }
        let args = ListArgs {
            view: View::Inbox,
            tab: None,
            account_id: Some(1),
            account_ids: None,
            folder_id: None,
            cursor: Some(ThreadCursor {
                last_message_at: i64::MAX,
                thread_id: i64::MAX,
            }),
            limit: 2,
        };
        let plan = list_query_plan(&conn, &args).unwrap();
        assert!(
            plan.iter().any(|line| {
                line.contains("idx_threads_recent") && line.contains("last_message_at<?")
            }),
            "keyset cursor must seek by account and timestamp: {plan:?}"
        );
    }
}
