pub mod engine;
pub mod folder_map;
pub mod gmail;
pub mod threading;

use crate::{db::repo, error::Result, events::CoreEvent};
use engine::SyncCtx;
use rusqlite::{OptionalExtension, params};

/// Only a composer-created draft with no provider identity can be removed
/// locally once nothing can still create it remotely. A synced draft whose UID
/// was invalidated has no `drafts_meta` row, while IMAP moves retain their
/// source UID in the action log; neither can be mistaken for a local-only draft.
pub(crate) async fn is_local_only_draft(ctx: &SyncCtx, message_id: i64) -> Result<bool> {
    ctx.db
        .read(move |conn| {
            let identity = conn
                .query_row(
                    "SELECT m.is_draft, m.uid, m.gm_msgid, m.gmail_draft_id,
                            m.jmap_id, a.provider, a.mail_protocol
                     FROM messages m JOIN accounts a ON a.id = m.account_id
                     WHERE m.id = ?1",
                    params![message_id],
                    |row| {
                        Ok((
                            row.get::<_, bool>(0)?,
                            row.get::<_, Option<i64>>(1)?,
                            row.get::<_, Option<String>>(2)?,
                            row.get::<_, Option<String>>(3)?,
                            row.get::<_, Option<String>>(4)?,
                            row.get::<_, String>(5)?,
                            row.get::<_, String>(6)?,
                        ))
                    },
                )
                .optional()?;
            let Some((is_draft, uid, gm_msgid, gmail_draft_id, jmap_id, provider, protocol)) =
                identity
            else {
                return Ok(false);
            };
            let locally_created: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM drafts_meta WHERE message_id = ?1)",
                params![message_id],
                |row| row.get(0),
            )?;
            if !is_draft
                || !locally_created
                || repo::actions::has_pending_remote_creation(conn, message_id)?
            {
                return Ok(false);
            }
            if provider == "gmail" {
                return Ok(gm_msgid.is_none() && gmail_draft_id.is_none());
            }
            if protocol == "jmap" {
                return Ok(jmap_id.is_none());
            }
            if uid.is_some() {
                return Ok(false);
            }
            let moved_from_server: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM pending_actions
                 WHERE message_id = ?1 AND kind IN ('trash','move')
                   AND json_extract(payload, '$.srcUid') IS NOT NULL)",
                params![message_id],
                |row| row.get(0),
            )?;
            Ok(!moved_from_server)
        })
        .await
}

/// Remove the cache entry only after the provider confirms permanent deletion.
/// Keeping it until then lets a failed queued action be retried safely.
pub(crate) async fn finish_permanent_delete(ctx: &SyncCtx, message_id: Option<i64>) -> Result<()> {
    let Some(message_id) = message_id else {
        return Ok(());
    };
    let (changed, raw_path, staged_paths) = ctx
        .db
        .write(move |conn| {
            let tx = conn.transaction()?;
            let Some(row) = repo::messages::get_row(&tx, message_id)? else {
                return Ok((None, None, Vec::new()));
            };
            let raw_path = row.raw_path;
            let staged_paths = repo::messages::take_draft_attachment_paths(&tx, message_id)?;
            repo::messages::delete(&tx, message_id)?;
            if let Some(thread_id) = row.thread_id {
                repo::threads::recompute(&tx, thread_id)?;
            }
            tx.commit()?;
            Ok((row.thread_id, raw_path, staged_paths))
        })
        .await?;
    if let Some(path) = raw_path {
        let _ = tokio::fs::remove_file(path).await;
    }
    for path in staged_paths {
        crate::remove_staged_attachment(&ctx.paths.draft_attachments_dir(), &path).await;
    }
    if let Some(thread_id) = changed {
        ctx.bus.emit(CoreEvent::MailUpdated {
            thread_ids: vec![thread_id],
        });
    }
    Ok(())
}

/// Capture local Trash rows before provider I/O. A sync may insert new mail
/// while the purge is running, and that mail was not necessarily deleted by
/// the provider-side operation.
pub(crate) async fn snapshot_trash_ids(
    ctx: &SyncCtx,
    account_id: i64,
) -> Result<std::collections::HashSet<i64>> {
    ctx.db
        .read(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT m.id FROM messages m WHERE m.account_id = ?1 AND (
                   EXISTS (SELECT 1 FROM folders f WHERE f.id = m.folder_id AND f.role = 'trash')
                   OR EXISTS (SELECT 1 FROM message_folders mf JOIN folders f ON f.id = mf.folder_id
                              WHERE mf.message_id = m.id AND f.role = 'trash')
                 )",
            )?;
            Ok(stmt
                .query_map(params![account_id], |row| row.get::<_, i64>(0))?
                .collect::<rusqlite::Result<std::collections::HashSet<_>>>()?)
        })
        .await
}

pub(crate) async fn finish_empty_trash(
    ctx: &SyncCtx,
    account_id: i64,
    snapshot_ids: std::collections::HashSet<i64>,
) -> Result<()> {
    let (thread_ids, paths, staged_paths) = ctx
        .db
        .write(move |conn| {
            let tx = conn.transaction()?;
            let mut stmt = tx.prepare(
                "SELECT m.id, m.thread_id, m.raw_path FROM messages m
             WHERE m.account_id = ?1 AND (
               EXISTS (SELECT 1 FROM folders f WHERE f.id = m.folder_id AND f.role = 'trash')
               OR EXISTS (SELECT 1 FROM message_folders mf JOIN folders f ON f.id = mf.folder_id
                          WHERE mf.message_id = m.id AND f.role = 'trash')
             )",
            )?;
            let rows = stmt
                .query_map(rusqlite::params![account_id], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Option<i64>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            drop(stmt);
            let mut thread_ids = std::collections::HashSet::new();
            let mut paths = Vec::new();
            let mut staged_paths = Vec::new();
            for (id, thread_id, path) in rows {
                if !snapshot_ids.contains(&id) {
                    continue;
                }
                staged_paths.extend(repo::messages::take_draft_attachment_paths(&tx, id)?);
                repo::messages::delete(&tx, id)?;
                if let Some(thread_id) = thread_id {
                    thread_ids.insert(thread_id);
                }
                if let Some(path) = path {
                    paths.push(path);
                }
            }
            for &thread_id in &thread_ids {
                repo::threads::recompute(&tx, thread_id)?;
            }
            tx.commit()?;
            Ok((
                thread_ids.into_iter().collect::<Vec<_>>(),
                paths,
                staged_paths,
            ))
        })
        .await?;
    for path in paths {
        let _ = tokio::fs::remove_file(path).await;
    }
    for path in staged_paths {
        crate::remove_staged_attachment(&ctx.paths.draft_attachments_dir(), &path).await;
    }
    if !thread_ids.is_empty() {
        ctx.bus.emit(CoreEvent::MailUpdated { thread_ids });
    }
    Ok(())
}
