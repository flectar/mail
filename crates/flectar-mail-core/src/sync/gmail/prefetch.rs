//! Gmail bodies downloaded ahead of time.
//!
//! A Gmail body was fetched only when its message was opened, so opening new
//! mail waited on Google (16 s seen for one reply). IMAP accounts have a
//! background body backfill (`engine::run_backfill_pool`); this is the Gmail
//! counterpart. Once the metadata backfill is done it walks messages without a
//! body, Inbox first and newest first, within the account's mail history, and
//! stores them the same way an open does. The sync wakes it when it reached
//! Gmail, so new mail's bodies follow it in.
//!
//! Gmail counts every request against a quota of units a user spends in a
//! minute, and a whole message costs 60 of them (see [`cost`]). The walk only
//! spends what the account's other requests of that minute leave of
//! [`BACKGROUND_UNITS`], so opening mail, sync and the app on the user's
//! other devices keep the rest.

use super::{GMAIL_API, GmailApi, SyncCtx, cost, hydrate_at, store_hydrated_body};
use crate::db::repo::{self, messages::GmailQueued};
use crate::error::{CoreError, Result};
use crate::events::CoreEvent;
use crate::models::{AccountConfig, AccountSettings, DownloadPolicy, now_ms};
use crate::sync::engine::{
    note_limit, record_content_failure_details, reserve_background_body_bytes,
};
use crate::sync::pacing::Backoff;
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;

/// What the walk may spend in a minute, together with everything else the
/// account sent in it: a third of a user's quota.
const BACKGROUND_UNITS: u32 = cost::USER_MINUTE / 3;
/// Between two bodies, so they spread over the minute rather than go out
/// together: 1.8 s.
const SPACING: Duration =
    Duration::from_millis(60_000 * cost::MESSAGES_GET_CONTENT as u64 / BACKGROUND_UNITS as u64);
/// Bodies looked up at a time.
const BATCH: i64 = 10;
/// How often to look for missing bodies when none are left, or when the walk
/// stopped and nothing woke it.
const IDLE_INTERVAL: Duration = Duration::from_secs(60);
/// Messages larger than this, attachments included, wait for an open: a
/// message that carries another downloads whole (#455).
const MAX_MESSAGE_BYTES: i64 = DownloadPolicy::LARGEST_ATTACHMENT;

pub(super) async fn run(
    ctx: SyncCtx,
    config: AccountConfig,
    settings_rx: watch::Receiver<AccountSettings>,
    nudges: mpsc::Receiver<()>,
) {
    run_at(ctx, config, settings_rx, nudges, GMAIL_API).await
}

/// [`run`] with the mailbox API at `base`.
async fn run_at(
    ctx: SyncCtx,
    config: AccountConfig,
    mut settings_rx: watch::Receiver<AccountSettings>,
    mut nudges: mpsc::Receiver<()>,
    base: &str,
) {
    let account_id = config.id;
    let api = match GmailApi::new(ctx.clone(), config.clone()) {
        Ok(api) => api,
        Err(error) => {
            tracing::error!(account_id, %error, "could not create Gmail prefetch");
            return;
        }
    };
    let pace = ctx.pace.account(account_id);
    // Waits after the walk stopped on something other than the quota.
    let mut failures = Backoff::default();
    let mut delay = IDLE_INTERVAL;
    let mut last_sent: Option<Instant> = None;
    loop {
        tokio::select! {
            () = tokio::time::sleep(delay) => {}
            nudge = nudges.recv() => if nudge.is_none() {
                return; // the sync stopped: account removed / shutdown
            },
            changed = settings_rx.changed() => if changed.is_err() {
                return;
            },
        }
        delay = IDLE_INTERVAL;
        if !wanted(&ctx, &settings_rx) {
            continue;
        }
        // Gmail asked the account to wait; asking again can make it longer.
        if let Some(pause) = pace.paused() {
            delay = until(pause.until);
            continue;
        }
        let since = settings_rx.borrow().mail_history.cutoff_ms_at(now_ms());
        let queue = ctx
            .db
            .read(move |conn| queue(conn, account_id, since))
            .await;
        let queue = match queue {
            Ok(queue) => queue,
            Err(error) => {
                tracing::warn!(account_id, %error, "Gmail prefetch: lookup failed");
                continue;
            }
        };
        if queue.is_empty() {
            continue;
        }
        let mut walk = Walk::default();
        for message in queue {
            // The account's own requests come first: wait for the minute to
            // leave room, and spread the bodies over it.
            let room = pace.room_at(cost::MESSAGES_GET_CONTENT, BACKGROUND_UNITS);
            let spaced = last_sent.map_or(room, |sent| room.max(sent + SPACING));
            tokio::time::sleep_until(spaced).await;
            if !wanted(&ctx, &settings_rx) || pace.paused().is_some() {
                walk.stop = Some(Duration::ZERO);
                break;
            }
            let _memory = reserve_background_body_bytes(message.size.max(0) as u64).await;
            last_sent = Some(Instant::now());
            match fetch(&ctx, &config, &api, base, &message.gm_msgid).await {
                Ok(thread_ids) => {
                    walk.stored += 1;
                    for thread_id in thread_ids {
                        if !walk.touched.contains(&thread_id) {
                            walk.touched.push(thread_id);
                        }
                    }
                }
                Err(error) => match outcome(&error) {
                    Outcome::Limited => {
                        tracing::debug!(account_id, %error, "Gmail prefetch: asked to wait");
                        walk.stop = Some(
                            note_limit(&ctx, account_id, &error)
                                .map_or(IDLE_INTERVAL, |pause| until(pause.until)),
                        );
                        break;
                    }
                    Outcome::Later => {
                        tracing::debug!(account_id, %error, "Gmail prefetch: paused");
                        walk.stop = Some(failures.after(&error).duration.max(IDLE_INTERVAL));
                        break;
                    }
                    Outcome::Skip => {
                        tracing::warn!(
                            account_id,
                            message_id = message.message_id,
                            %error,
                            "Gmail prefetch: a body didn't download"
                        );
                        walk.failed.push((message.message_id, error.to_string()));
                        walk.last_error = Some(error);
                    }
                },
            }
        }
        // Each waits as a body that failed does, and comes when it's opened.
        if let Err(error) = record_content_failure_details(&ctx, walk.failed).await {
            tracing::warn!(account_id, %error, "Gmail prefetch: could not note failures");
        }
        if !walk.touched.is_empty() {
            ctx.bus.emit(CoreEvent::MailUpdated {
                thread_ids: walk.touched,
            });
        }
        delay = match (walk.stop, walk.last_error) {
            (Some(wait), _) => wait,
            // Nothing in a whole batch downloaded: more likely the account
            // than its messages, such as Gmail turned off by its domain.
            (None, Some(error)) if walk.stored == 0 => {
                failures.after(&error).duration.max(IDLE_INTERVAL)
            }
            // Bodies may still wait; the quota paces the next batch.
            _ => {
                failures.reset();
                Duration::ZERO
            }
        };
    }
}

/// One batch's walk: what it stored and why it stopped.
#[derive(Default)]
struct Walk {
    stored: usize,
    touched: Vec<i64>,
    failed: Vec<(i64, String)>,
    last_error: Option<CoreError>,
    /// The walk stopped early, and the next looks this much later.
    stop: Option<Duration>,
}

/// Whether bodies download ahead of time now: the account's setting wants
/// them, as with the IMAP backfill (#253), and a metered connection doesn't
/// hold downloads back.
fn wanted(ctx: &SyncCtx, settings: &watch::Receiver<AccountSettings>) -> bool {
    settings.borrow().download != DownloadPolicy::Headers && !ctx.metered.downloads_wait()
}

/// The next bodies to download. None while the account needs signing in
/// again: the sync asks for that, and wakes the walk once it reaches Gmail.
fn queue(
    conn: &rusqlite::Connection,
    account_id: i64,
    since: Option<i64>,
) -> Result<Vec<GmailQueued>> {
    let signed_out = repo::accounts::get(conn, account_id)?
        .is_none_or(|account| account.sync_state == "needs_reauth");
    if signed_out {
        return Ok(Vec::new());
    }
    repo::messages::gmail_body_download_queue(conn, account_id, since, MAX_MESSAGE_BYTES, BATCH)
}

/// Downloads a message whole and stores it as an open does, so a message
/// with a forwarded message inside keeps its content (#455).
async fn fetch(
    ctx: &SyncCtx,
    config: &AccountConfig,
    api: &GmailApi,
    base: &str,
    gm_msgid: &str,
) -> Result<Vec<i64>> {
    let resource = api.message_full_at(base, gm_msgid).await?;
    let body = hydrate_at(ctx, config, api, base, &resource).await?;
    store_hydrated_body(ctx, config, gm_msgid.to_owned(), body).await
}

/// What a failed download means for the walk.
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    /// Gmail asked to slow down: the account waits as long as it said.
    Limited,
    /// The network, the sign-in or this device is in the way, not the
    /// message: the walk stops for now.
    Later,
    /// Something about this message: the walk goes on without it.
    Skip,
}

fn outcome(error: &CoreError) -> Outcome {
    match error {
        CoreError::RateLimited { .. } => Outcome::Limited,
        // Gmail no longer has it, it doesn't parse, or Gmail turned the
        // request down.
        CoreError::NotFound(_)
        | CoreError::Mime(_)
        | CoreError::ResponseTooLarge { .. }
        | CoreError::Other(_) => Outcome::Skip,
        // Such as no connection, a sign-in to renew, or a full disk.
        _ => Outcome::Later,
    }
}

/// How long until `instant`.
fn until(instant: Instant) -> Duration {
    instant.saturating_duration_since(Instant::now())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::gmail::search_tests::stand_in_answering;
    use crate::sync::gmail::tests::gmail_account;
    use serde_json::{Value, json};
    use std::sync::{Arc, Mutex};

    fn limited() -> CoreError {
        CoreError::RateLimited {
            service: "Gmail".into(),
            kind: crate::error::LimitKind::Requests,
            retry_after: None,
            detail: "userRateLimitExceeded".into(),
        }
    }

    #[test]
    fn quota_waits_message_trouble_skips_and_the_rest_stops_the_walk() {
        assert_eq!(outcome(&limited()), Outcome::Limited);
        for skipped in [
            CoreError::NotFound("Gmail resource: Not Found".into()),
            CoreError::Mime("too many parts".into()),
            CoreError::Other("Gmail 400 Bad Request: Invalid id value".into()),
        ] {
            assert_eq!(outcome(&skipped), Outcome::Skip, "{skipped}");
        }
        for later in [
            CoreError::Offline,
            CoreError::Network("Gmail 503 Service Unavailable: backendError".into()),
            CoreError::NeedsReauth,
            CoreError::Auth("invalid_grant".into()),
            CoreError::Io(std::io::Error::other("No space left on device")),
            CoreError::Db(rusqlite::Error::InvalidQuery),
        ] {
            assert_eq!(outcome(&later), Outcome::Later, "{later}");
        }
    }

    #[test]
    fn bodies_spread_over_a_minute_within_a_third_of_the_quota() {
        assert_eq!(BACKGROUND_UNITS, 2_000);
        assert_eq!(SPACING, Duration::from_millis(1_800));
        // At most 33 bodies a minute, about 2,000 an hour.
        assert_eq!(60_000 / SPACING.as_millis(), 33);
    }

    /// Gmail account 1 with its metadata in: `m3` in the Inbox, then `m2`
    /// and `m1` archived, newest first; Spam's `m9`, a body already kept and
    /// a message too large wait. The mail is from 1970, so the account keeps
    /// its whole history.
    async fn queued_account() -> (tempfile::TempDir, SyncCtx, AccountConfig) {
        let (dir, ctx, mut config) = gmail_account().await;
        config.settings.mail_history = crate::models::MailHistory::All;
        ctx.tokens
            .store_initial(
                config.id,
                "access".into(),
                Some(3600),
                Some("refresh".into()),
                "client".into(),
                None,
            )
            .await
            .unwrap();
        ctx.db
            .write(|conn| {
                conn.execute_batch(
                    "INSERT INTO gmail_sync_state (account_id, generation, backfill_done, updated_at)
                       VALUES (1, 1, 1, 0);
                     INSERT INTO threads (id, account_id, subject_norm) VALUES (1, 1, 's');
                     INSERT INTO messages (
                       id, account_id, thread_id, folder_id, subject, from_addr, date,
                       body_state, gm_msgid, size
                     ) VALUES
                       (1, 1, 1, 3, 'old', 'a@mail.example', 100, 'none', 'm1', 400),
                       (2, 1, 1, 3, 'new', 'a@mail.example', 300, 'none', 'm2', 400),
                       (3, 1, 1, 1, 'inbox', 'a@mail.example', 200, 'none', 'm3', 400),
                       (4, 1, 1, 3, 'kept', 'a@mail.example', 400, 'cached', 'm4', 400),
                       (5, 1, 1, 2, 'spam', 'a@mail.example', 500, 'none', 'm9', 400),
                       (6, 1, 1, 3, 'huge', 'a@mail.example', 600, 'none', 'm6', 99999999);",
                )?;
                Ok(())
            })
            .await
            .unwrap();
        (dir, ctx, config)
    }

    fn full(id: &str) -> Value {
        json!({
            "id": id, "threadId": "t", "labelIds": ["INBOX"], "sizeEstimate": 400,
            "payload": {
                "mimeType": "text/plain",
                "headers": [{"name": "Content-Type", "value": "text/plain; charset=UTF-8"}],
                "body": {"size": 5, "data": "Qm9keSA"},
            },
        })
    }

    #[tokio::test]
    async fn the_walk_stores_bodies_inbox_first_and_counts_their_quota() {
        let (_dir, ctx, config) = queued_account().await;
        let (origin, requests) = stand_in_answering(|(_, target, _)| {
            let id = target
                .trim_start_matches("/gmail/v1/users/me/messages/")
                .split('?')
                .next()
                .unwrap()
                .to_owned();
            (200, "application/json".into(), full(&id).to_string())
        })
        .await;
        let mut events = ctx.bus.subscribe();
        let (_settings_tx, settings_rx) = watch::channel(config.settings.clone());
        let (nudge, nudges) = mpsc::channel(1);
        let walk = tokio::spawn({
            let (ctx, config) = (ctx.clone(), config.clone());
            let base = format!("{origin}/gmail/v1/users/me");
            async move { run_at(ctx, config, settings_rx, nudges, &base).await }
        });
        nudge.send(()).await.unwrap();
        let stored = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if let Ok(CoreEvent::MailUpdated { .. }) = events.recv().await {
                    return;
                }
            }
        })
        .await;
        walk.abort();
        assert!(stored.is_ok(), "no body was stored");
        let targets: Vec<String> = requests
            .lock()
            .unwrap()
            .iter()
            .map(|(_, target, _)| target.clone())
            .collect();
        assert_eq!(
            targets,
            [
                "/gmail/v1/users/me/messages/m3?format=full",
                "/gmail/v1/users/me/messages/m2?format=full",
                "/gmail/v1/users/me/messages/m1?format=full",
            ]
        );
        let states: Vec<(i64, String)> = ctx
            .db
            .read(|conn| {
                let mut stmt = conn.prepare("SELECT id, body_state FROM messages ORDER BY id")?;
                let rows = stmt
                    .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .await
            .unwrap();
        let kept: Vec<i64> = states
            .iter()
            .filter(|(_, state)| state == "cached")
            .map(|(id, _)| *id)
            .collect();
        assert_eq!(kept, [1, 2, 3, 4]);
        // Three whole messages: 180 units in this minute.
        let pace = ctx.pace.account(config.id);
        assert!(pace.room_at(BACKGROUND_UNITS - 180, BACKGROUND_UNITS) <= Instant::now());
        assert!(pace.room_at(BACKGROUND_UNITS - 179, BACKGROUND_UNITS) > Instant::now());
    }

    #[tokio::test]
    async fn gmail_asking_to_wait_pauses_the_account() {
        let (_dir, ctx, config) = queued_account().await;
        let (origin, requests) = stand_in_answering(|_| {
            (
                429,
                "application/json".into(),
                json!({"error": {"code": 429, "message": "Too many requests",
                                 "errors": [{"reason": "rateLimitExceeded"}]}})
                .to_string(),
            )
        })
        .await;
        let (_settings_tx, settings_rx) = watch::channel(config.settings.clone());
        let (nudge, nudges) = mpsc::channel(1);
        let walk = tokio::spawn({
            let (ctx, config) = (ctx.clone(), config.clone());
            let base = format!("{origin}/gmail/v1/users/me");
            async move { run_at(ctx, config, settings_rx, nudges, &base).await }
        });
        nudge.send(()).await.unwrap();
        let pace = ctx.pace.account(config.id);
        let paused = tokio::time::timeout(Duration::from_secs(10), async {
            while pace.paused().is_none() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        // Nothing more goes out while Gmail's wait lasts.
        tokio::time::sleep(Duration::from_millis(300)).await;
        walk.abort();
        assert!(paused.is_ok(), "the account didn't pause");
        assert_eq!(requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_message_gmail_no_longer_has_waits_and_the_walk_goes_on() {
        let (_dir, ctx, config) = queued_account().await;
        let (origin, _requests) = stand_in_answering(|(_, target, _)| {
            if target.contains("/m3?") {
                return (
                    404,
                    "application/json".into(),
                    json!({"error": {"code": 404, "message": "Requested entity was not found."}})
                        .to_string(),
                );
            }
            let id = target
                .trim_start_matches("/gmail/v1/users/me/messages/")
                .split('?')
                .next()
                .unwrap()
                .to_owned();
            (200, "application/json".into(), full(&id).to_string())
        })
        .await;
        let mut events = ctx.bus.subscribe();
        let (_settings_tx, settings_rx) = watch::channel(config.settings.clone());
        let (nudge, nudges) = mpsc::channel(1);
        let walk = tokio::spawn({
            let (ctx, config) = (ctx.clone(), config.clone());
            let base = format!("{origin}/gmail/v1/users/me");
            async move { run_at(ctx, config, settings_rx, nudges, &base).await }
        });
        nudge.send(()).await.unwrap();
        let stored = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if let Ok(CoreEvent::MailUpdated { .. }) = events.recv().await {
                    return;
                }
            }
        })
        .await;
        walk.abort();
        assert!(stored.is_ok());
        let (retry_at, kept): (Option<i64>, i64) = ctx
            .db
            .read(|conn| {
                let retry_at = conn.query_row(
                    "SELECT next_retry_at FROM sync_failures
                     WHERE stage = 'content' AND message_id = 3",
                    [],
                    |row| row.get(0),
                )?;
                let kept = conn.query_row(
                    "SELECT COUNT(*) FROM messages WHERE body_state = 'cached' AND id IN (1, 2)",
                    [],
                    |row| row.get(0),
                )?;
                Ok((retry_at, kept))
            })
            .await
            .unwrap();
        assert!(retry_at.is_some_and(|at| at > now_ms()));
        assert_eq!(kept, 2);
    }

    #[tokio::test]
    async fn headers_only_downloads_nothing() {
        let (_dir, ctx, mut config) = queued_account().await;
        config.settings.download = DownloadPolicy::Headers;
        let requests = Arc::new(Mutex::new(0));
        let (origin, _) = stand_in_answering({
            let requests = Arc::clone(&requests);
            move |_| {
                *requests.lock().unwrap() += 1;
                (200, "application/json".into(), "{}".into())
            }
        })
        .await;
        let (_settings_tx, settings_rx) = watch::channel(config.settings.clone());
        let (nudge, nudges) = mpsc::channel(1);
        let walk = tokio::spawn({
            let (ctx, config) = (ctx.clone(), config.clone());
            let base = format!("{origin}/gmail/v1/users/me");
            async move { run_at(ctx, config, settings_rx, nudges, &base).await }
        });
        nudge.send(()).await.unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        walk.abort();
        assert_eq!(*requests.lock().unwrap(), 0);
    }
}
