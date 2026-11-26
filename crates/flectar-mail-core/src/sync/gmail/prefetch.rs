//! Gmail bodies downloaded ahead of time.
//!
//! A Gmail body was fetched only when its message was opened, so opening new
//! mail waited on Google (16 s seen for one reply). IMAP accounts have a
//! background body backfill (`engine::run_backfill_pool`); this is the Gmail
//! counterpart. Once the metadata backfill is done it walks messages without a
//! body, Inbox first and newest first, within the account's mail history, and
//! stores them the same way an open does.

use super::{GmailApi, SyncCtx, hydrate, store_hydrated_body};
use crate::error::{CoreError, Result};
use crate::events::CoreEvent;
use crate::models::{AccountConfig, AccountSettings, DownloadPolicy, now_ms};
use crate::sync::engine::note_limit;
use rusqlite::{Connection, params};
use std::collections::HashSet;
use std::time::Duration;
use tokio::sync::watch;

/// How long to wait before looking for missing bodies again once none are
/// left, or after Google could not be reached.
const IDLE_INTERVAL: Duration = Duration::from_secs(60);
/// Bodies fetched between pauses.
const BATCH: usize = 10;
/// Pause between batches. messages.get costs 20 quota units (see
/// `BACKFILL_INTERVAL` in gmail.rs), plus one attachments.get per separate
/// part. 10 bodies every 10 s is about 1,200 units a minute, a fifth of
/// Google's 6,000 per user, leaving room for sync and opens.
const PACE: Duration = Duration::from_secs(10);
/// Messages larger than this are left for when they are opened: their
/// attachment parts are downloaded whole while reading the body.
const MAX_MESSAGE_BYTES: i64 = 10 * 1024 * 1024;

pub(super) async fn run(
    ctx: SyncCtx,
    config: AccountConfig,
    mut settings_rx: watch::Receiver<AccountSettings>,
) {
    let account_id = config.id;
    let api = match GmailApi::new(ctx.clone(), config.clone()) {
        Ok(api) => api,
        Err(error) => {
            tracing::error!(account_id, %error, "could not create Gmail prefetch");
            return;
        }
    };
    // Messages that failed for a reason other than the network are not tried
    // again until the app restarts, so one bad message cannot loop.
    let mut failed = HashSet::<i64>::new();
    let mut delay = IDLE_INTERVAL;
    loop {
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            changed = settings_rx.changed() => {
                if changed.is_err() {
                    return; // account removed / shutdown
                }
            }
        }
        let (mail_history, download) = {
            let settings = settings_rx.borrow_and_update();
            (settings.mail_history, settings.download)
        };
        // Only the headers are wanted, or downloads wait on a metered
        // connection: messages download when they're opened, as with the IMAP
        // backfill (#253).
        if download == DownloadPolicy::Headers || ctx.metered.downloads_wait() {
            delay = IDLE_INTERVAL;
            continue;
        }
        // Gmail asked the account to wait; asking again can make it longer.
        if let Some(pause) = ctx.pace.account(account_id).paused() {
            delay = until(pause.until);
            continue;
        }
        let cutoff_ms = mail_history.cutoff_ms_at(now_ms());
        let skip: Vec<i64> = failed.iter().copied().collect();
        let batch = ctx
            .db
            .read(move |conn| missing_bodies(conn, account_id, cutoff_ms, BATCH, &skip))
            .await;
        let batch = match batch {
            Ok(batch) => batch,
            Err(error) => {
                tracing::warn!(account_id, %error, "Gmail prefetch: lookup failed");
                delay = IDLE_INTERVAL;
                continue;
            }
        };
        if batch.is_empty() {
            delay = IDLE_INTERVAL;
            continue;
        }
        delay = PACE;
        let mut touched = Vec::<i64>::new();
        for (message_id, provider_message_id) in batch {
            let stored = async {
                let resource = api.message_full(&provider_message_id).await?;
                // As an open does: a forwarded message comes from the raw
                // message (#455).
                let body = hydrate(&ctx, &config, &api, &resource).await?;
                store_hydrated_body(&ctx, &config, provider_message_id.clone(), body).await
            }
            .await;
            match stored {
                Ok(thread_ids) => {
                    for thread_id in thread_ids {
                        if !touched.contains(&thread_id) {
                            touched.push(thread_id);
                        }
                    }
                }
                Err(error) => match outcome(&error) {
                    Outcome::Limited => {
                        tracing::debug!(account_id, %error, "Gmail prefetch: asked to wait");
                        delay = note_limit(&ctx, account_id, &error)
                            .map_or(IDLE_INTERVAL, |pause| until(pause.until));
                        break;
                    }
                    Outcome::Later => {
                        tracing::debug!(account_id, %error, "Gmail prefetch: paused");
                        delay = IDLE_INTERVAL;
                        break;
                    }
                    Outcome::Skip => {
                        tracing::warn!(account_id, message_id, %error, "Gmail prefetch: skipped a message");
                        failed.insert(message_id);
                    }
                },
            }
        }
        if !touched.is_empty() {
            ctx.bus.emit(CoreEvent::MailUpdated {
                thread_ids: touched,
            });
        }
    }
}

/// What a failed fetch means for the walk.
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    /// Gmail asked to slow down: the account waits as long as it said.
    Limited,
    /// Network or sign-in trouble passes; the message is fine.
    Later,
    /// Something about this message: left for when it's opened.
    Skip,
}

fn outcome(error: &CoreError) -> Outcome {
    match error {
        CoreError::RateLimited { .. } => Outcome::Limited,
        CoreError::Offline
        | CoreError::Network(_)
        | CoreError::Auth(_)
        | CoreError::NeedsReauth => Outcome::Later,
        _ => Outcome::Skip,
    }
}

/// How long until `instant`, at least one batch's pause.
fn until(instant: tokio::time::Instant) -> Duration {
    instant
        .saturating_duration_since(tokio::time::Instant::now())
        .max(PACE)
}

/// Up to `limit` Gmail messages of `account_id` still without a body, as
/// (local id, Gmail id): Inbox first, then newest first. Nothing until the
/// metadata backfill is done, since that alone takes most of the quota.
/// Spam, Trash, mail older than `cutoff_ms` and very large messages wait for
/// an open.
fn missing_bodies(
    conn: &Connection,
    account_id: i64,
    cutoff_ms: Option<i64>,
    limit: usize,
    skip: &[i64],
) -> Result<Vec<(i64, String)>> {
    let backfill_done: bool = conn
        .query_row(
            "SELECT backfill_done FROM gmail_sync_state WHERE account_id = ?1",
            params![account_id],
            |row| row.get::<_, i64>(0),
        )
        .map(|done| done != 0)
        .or_else(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => Ok(false),
            other => Err(other),
        })?;
    if !backfill_done {
        return Ok(Vec::new());
    }
    let mut statement = conn.prepare_cached(
        "SELECT m.id, m.gm_msgid FROM messages m
         WHERE m.account_id = ?1
           AND m.body_state = 'none'
           AND m.gm_msgid IS NOT NULL
           AND (?2 IS NULL OR COALESCE(m.internal_date, m.date) >= ?2)
           AND COALESCE(m.size, 0) <= ?3
           AND NOT EXISTS (SELECT 1 FROM folders f
                           WHERE f.id = m.folder_id AND f.role IN ('spam', 'trash'))
         ORDER BY EXISTS (SELECT 1 FROM message_folders mf
                          JOIN folders f ON f.id = mf.folder_id
                          WHERE mf.message_id = m.id AND f.role = 'inbox') DESC,
                  m.date DESC, m.id DESC
         LIMIT ?4",
    )?;
    let rows = statement.query_map(
        params![
            account_id,
            cutoff_ms,
            MAX_MESSAGE_BYTES,
            (limit + skip.len()) as i64
        ],
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
    )?;
    let mut output = Vec::with_capacity(limit);
    for row in rows {
        let (id, gm_msgid) = row?;
        if skip.contains(&id) {
            continue;
        }
        output.push((id, gm_msgid));
        if output.len() == limit {
            break;
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded(backfill_done: bool) -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::run(&mut conn).unwrap();
        conn.execute_batch(
            "INSERT INTO accounts (
               id, email, provider, auth_kind, mail_protocol, username, incoming_host,
               incoming_port, smtp_host, smtp_port, created_at, settings_json
             ) VALUES (1, 'me@mail.example', 'gmail', 'oauth2', 'gmail', 'me', 'h', 993,
                       'h', 587, 0, '{}');
             INSERT INTO folders (id, account_id, path, role) VALUES
               (1, 1, 'INBOX', 'inbox'),
               (2, 1, '[Gmail]/All Mail', 'archive'),
               (3, 1, '[Gmail]/Spam', 'spam');
             INSERT INTO threads (id, account_id, snippet) VALUES (1, 1, '');
             INSERT INTO messages (
               id, account_id, thread_id, folder_id, subject, from_addr, to_json, cc_json,
               date, snippet, body_state, gm_msgid, size
             ) VALUES
               (1, 1, 1, 2, 'old archived', 'a@mail.example', '[]', '[]', 100, '', 'none', 'g1', 1),
               (2, 1, 1, 2, 'new archived', 'a@mail.example', '[]', '[]', 300, '', 'none', 'g2', 1),
               (3, 1, 1, 2, 'old in inbox', 'a@mail.example', '[]', '[]', 200, '', 'none', 'g3', 1),
               (4, 1, 1, 2, 'cached', 'a@mail.example', '[]', '[]', 400, '', 'cached', 'g4', 1),
               (5, 1, 1, 3, 'spam', 'a@mail.example', '[]', '[]', 500, '', 'none', 'g5', 1),
               (6, 1, 1, 2, 'huge', 'a@mail.example', '[]', '[]', 600, '', 'none', 'g6', 99999999),
               (7, 1, 1, 2, 'too old', 'a@mail.example', '[]', '[]', 10, '', 'none', 'g7', 1);
             INSERT INTO message_folders (message_id, folder_id, since) VALUES (3, 1, 0);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO gmail_sync_state (account_id, generation, backfill_done, updated_at)
             VALUES (1, 1, ?1, 0)",
            params![backfill_done as i64],
        )
        .unwrap();
        conn
    }

    fn ids(rows: Vec<(i64, String)>) -> Vec<i64> {
        rows.into_iter().map(|(id, _)| id).collect()
    }

    #[test]
    fn inbox_first_then_newest_within_history() {
        let conn = seeded(true);
        let rows = missing_bodies(&conn, 1, Some(50), 10, &[]).unwrap();
        assert_eq!(ids(rows), [3, 2, 1]);
    }

    #[test]
    fn skipped_messages_and_limit() {
        let conn = seeded(true);
        let rows = missing_bodies(&conn, 1, Some(50), 1, &[3]).unwrap();
        assert_eq!(ids(rows), [2]);
    }

    #[test]
    fn quota_waits_network_pauses_and_others_skip() {
        let limited = CoreError::RateLimited {
            service: "Gmail".into(),
            kind: crate::error::LimitKind::Requests,
            retry_after: None,
            detail: "userRateLimitExceeded".into(),
        };
        assert_eq!(outcome(&limited), Outcome::Limited);
        assert_eq!(outcome(&CoreError::Offline), Outcome::Later);
        assert_eq!(outcome(&CoreError::NeedsReauth), Outcome::Later);
        assert_eq!(outcome(&CoreError::Other("bad part".into())), Outcome::Skip);
    }

    #[test]
    fn waits_for_the_metadata_backfill() {
        let conn = seeded(false);
        assert!(missing_bodies(&conn, 1, None, 10, &[]).unwrap().is_empty());
    }
}
