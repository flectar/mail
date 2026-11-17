//! Trash and Junk emptied of mail that has sat there longer than the user
//! set (#238), as KMail expires folders and Fastmail purges them. Time counts
//! from when a message went into the folder, which the app keeps itself
//! (`messages.folder_since`, `message_folders.since`): the time a message
//! arrived stays the same through moves (RFC 9051 section 2.3.3, RFC 8621
//! `receivedAt`), and by it an old message would go as soon as it was
//! thrown away. Starred mail stays. The app empties the folders while it
//! runs, as no rule on a server can look at how long mail has sat anywhere.

use crate::Core;
use crate::db::repo;
use crate::error::Result;
use crate::models::{ActionKind, now_ms, roles};
use crate::sync::engine::SyncCmd;
use rusqlite::{Connection, params};
use std::collections::BTreeSet;
use std::time::Duration;

/// How long after the app starts it first empties the folders, so the first
/// sync comes first.
const FIRST_AFTER: Duration = Duration::from_secs(2 * 60);
/// How often it empties them after that.
const EVERY: Duration = Duration::from_secs(6 * 60 * 60);
/// The most messages one pass deletes, so a first pass over years of Trash
/// goes in parts.
const MOST: usize = 2000;
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// Up to `limit` messages that have sat in a folder with role `role` since
/// before `before` (milliseconds since the epoch), unstarred and not on
/// their way out already, with their thread.
pub(crate) fn old_mail(
    conn: &Connection,
    role: &str,
    before: i64,
    limit: usize,
) -> Result<Vec<(i64, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT m.id, m.thread_id FROM messages m
         WHERE m.thread_id IS NOT NULL AND m.is_starred = 0
           AND ((EXISTS (SELECT 1 FROM folders f WHERE f.id = m.folder_id AND f.role = ?1)
                 AND m.folder_since < ?2)
                OR EXISTS (SELECT 1 FROM message_folders mf
                           JOIN folders f ON f.id = mf.folder_id
                           WHERE mf.message_id = m.id AND f.role = ?1 AND mf.since < ?2))
           AND NOT EXISTS (SELECT 1 FROM pending_actions a
                           WHERE a.message_id = m.id AND a.kind = 'delete_permanently'
                             AND a.state IN ('pending', 'inflight'))
         ORDER BY m.id
         LIMIT ?3",
    )?;
    let rows = stmt.query_map(
        params![role, before, i64::try_from(limit).unwrap_or(i64::MAX)],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

impl Core {
    /// Empties Trash and Junk, soon after the app starts, then every few
    /// hours.
    pub(crate) async fn cleanup_loop(&self) {
        tokio::time::sleep(FIRST_AFTER).await;
        loop {
            if let Err(error) = self.empty_old_mail().await {
                tracing::warn!(%error, "emptying Trash and Junk failed");
            }
            tokio::time::sleep(EVERY).await;
        }
    }

    /// Deletes for good the mail that has sat in Trash or Junk for longer
    /// than the settings keep it, up to a pass's worth. How many messages
    /// go.
    pub async fn empty_old_mail(&self) -> Result<usize> {
        let settings = self.db.read(|conn| repo::settings::get(conn)).await?;
        let folders = [
            (roles::TRASH, settings.empty_trash_after_days),
            (roles::SPAM, settings.empty_junk_after_days),
        ];
        let now = now_ms();
        let (deleted, accounts) = self
            .db
            .write(move |conn| {
                let tx = conn.transaction()?;
                let mut deleted = 0;
                let mut accounts = BTreeSet::new();
                for (role, days) in folders {
                    if days == 0 {
                        continue;
                    }
                    let before = now.saturating_sub(i64::from(days).saturating_mul(DAY_MS));
                    for (message_id, thread_id) in old_mail(&tx, role, before, MOST - deleted)? {
                        let queued = crate::apply_thread_action_in(
                            &tx,
                            thread_id,
                            Some(&[message_id]),
                            ActionKind::DeletePermanently,
                            None,
                        )?;
                        deleted += usize::from(!queued.is_empty());
                        accounts.extend(queued.into_iter().map(|(_, account_id)| account_id));
                    }
                }
                tx.commit()?;
                Ok((deleted, accounts))
            })
            .await?;
        if deleted > 0 {
            tracing::info!(deleted, "emptied Trash and Junk of old mail");
        }
        for account_id in accounts {
            self.nudge(Some(account_id), || SyncCmd::RunActions).await;
        }
        Ok(deleted)
    }

    /// Keeps mail in Trash for `trash_days` and in Junk for `junk_days`
    /// before deleting it for good, 0 for ever, and empties them now.
    pub async fn set_folder_cleanup(&self, trash_days: u32, junk_days: u32) -> Result<()> {
        self.db
            .write(move |conn| {
                let mut settings = repo::settings::get(conn)?;
                settings.empty_trash_after_days = trash_days;
                settings.empty_junk_after_days = junk_days;
                repo::settings::set(conn, &settings)
            })
            .await?;
        self.empty_old_mail().await?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "cleanup_tests.rs"]
mod tests;
