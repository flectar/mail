use crate::error::{CoreError, Result};
use crate::models::{Settings, now_ms, roles};
use crate::sync::ingest::CHIME_RECENT_MS;
use rusqlite::{Connection, OptionalExtension, Row, params};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationOutboxItem {
    pub id: i64,
    pub account_id: i64,
    /// `None` once the message is gone.
    pub message_id: Option<i64>,
    /// The message the notification was queued for, kept after it is gone.
    pub notified_message_id: i64,
    pub thread_id: Option<i64>,
    pub sender_name: Option<String>,
    pub sender_addr: Option<String>,
    pub subject: String,
    pub audible: bool,
    pub state: String,
    pub in_summary: bool,
    pub attempts: i64,
    pub not_before: Option<i64>,
    pub created_at: i64,
    pub claimed_at: Option<i64>,
    pub delivered_at: Option<i64>,
    pub suppressed_at: Option<i64>,
    pub suppression_reason: Option<String>,
    pub withdrawn_at: Option<i64>,
    pub last_error: Option<String>,
}

const COLUMNS: &str = "id, account_id, message_id, notified_message_id, thread_id, sender_name,
     sender_addr, subject, audible, state, in_summary, attempts, not_before, created_at,
     claimed_at, delivered_at, suppressed_at, suppression_reason, withdrawn_at, last_error";

fn from_row(row: &Row) -> rusqlite::Result<NotificationOutboxItem> {
    Ok(NotificationOutboxItem {
        id: row.get("id")?,
        account_id: row.get("account_id")?,
        message_id: row.get("message_id")?,
        notified_message_id: row.get("notified_message_id")?,
        thread_id: row.get("thread_id")?,
        sender_name: row.get("sender_name")?,
        sender_addr: row.get("sender_addr")?,
        subject: row.get("subject")?,
        audible: row.get("audible")?,
        state: row.get("state")?,
        in_summary: row.get("in_summary")?,
        attempts: row.get("attempts")?,
        not_before: row.get("not_before")?,
        created_at: row.get("created_at")?,
        claimed_at: row.get("claimed_at")?,
        delivered_at: row.get("delivered_at")?,
        suppressed_at: row.get("suppressed_at")?,
        suppression_reason: row.get("suppression_reason")?,
        withdrawn_at: row.get("withdrawn_at")?,
        last_error: row.get("last_error")?,
    })
}

/// Enqueue one notification by snapshotting the message fields needed by the
/// native dispatcher. `audible` marks mail from a person, the only kind that
/// may play the new-mail sound. Repeated calls for the same message return the
/// original outbox id and never reset a terminal delivery state.
pub fn enqueue(conn: &Connection, message_id: i64, audible: bool) -> Result<i64> {
    enqueue_at(conn, message_id, audible, now_ms())
}

pub fn enqueue_at(
    conn: &Connection,
    message_id: i64,
    audible: bool,
    created_at: i64,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO notification_outbox (
             account_id, message_id, notified_message_id, thread_id, sender_name,
             sender_addr, subject, audible, created_at
         )
         SELECT account_id, id, id, thread_id, from_name, from_addr, subject, ?2, ?3
         FROM messages WHERE id = ?1
         ON CONFLICT(message_id) DO NOTHING",
        params![message_id, audible, created_at],
    )?;

    conn.query_row(
        "SELECT id FROM notification_outbox WHERE message_id = ?1",
        params![message_id],
        |row| row.get(0),
    )
    .optional()?
    .ok_or_else(|| CoreError::NotFound(format!("message {message_id}")))
}

/// Queue notifications for mail that reached an Inbox while no copy of the
/// app was following its account, so that a check in the background tells of
/// it as a running app would have. `since` holds, per account, when its last
/// complete sync before this start began (see
/// [`super::accounts::caught_up`]). Mail that came in after that, by the
/// measure a running sync uses ([`crate::sync::ingest::live_since`]), and is
/// incoming, recent ([`CHIME_RECENT_MS`]) and still new ([`STILL_NEW`]) is
/// queued, once per conversation as a sync queues it: its newest message,
/// unless the conversation has a notification for one as new or newer.
/// Returns the accounts and conversations it queued for.
pub fn enqueue_missed(
    conn: &Connection,
    since: &BTreeMap<i64, i64>,
    now: i64,
) -> Result<Vec<(i64, i64)>> {
    let mut statement = conn.prepare(&format!(
        "SELECT m.id, m.thread_id, m.is_automated, COALESCE(m.from_addr, '')
         FROM messages m JOIN accounts a ON a.id = m.account_id
         WHERE m.account_id = ?1
           AND m.is_outgoing = 0
           AND COALESCE(m.internal_date, m.date) >= ?3
           AND COALESCE(m.internal_date, m.date) > ?4
           AND {}
           AND NOT EXISTS (
                 SELECT 1 FROM notification_outbox o JOIN messages n ON n.id = o.message_id
                 WHERE o.thread_id = m.thread_id
                   AND COALESCE(n.internal_date, n.date) >= COALESCE(m.internal_date, m.date))
         ORDER BY COALESCE(m.internal_date, m.date) DESC, m.id DESC",
        still_new_condition(2)
    ))?;
    let mut queued = Vec::new();
    for (&account_id, &caught_up_at) in since {
        // Newest first, so the first message of a conversation is its newest.
        let mut newest = Vec::new();
        {
            let mut threads = std::collections::HashSet::new();
            let mut rows = statement.query(params![
                account_id,
                roles::INBOX,
                crate::sync::ingest::live_since(caught_up_at),
                now - CHIME_RECENT_MS,
            ])?;
            while let Some(row) = rows.next()? {
                let thread_id: i64 = row.get(1)?;
                if threads.insert(thread_id) {
                    let from: String = row.get(3)?;
                    let audible = crate::sync::ingest::chime_worthy(row.get(2)?, &from);
                    newest.push((row.get::<_, i64>(0)?, thread_id, audible));
                }
            }
        }
        // Oldest first, the order a sync queues them in.
        for (message_id, thread_id, audible) in newest.into_iter().rev() {
            enqueue_at(conn, message_id, audible, now)?;
            queued.push((account_id, thread_id));
        }
    }
    Ok(queued)
}

/// How a thread currently routes, for notification-scope filtering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoutedTab {
    /// AI classification is still in flight; the caller should wait and re-check.
    Pending,
    /// Resolved route key: "important" | "other" | "split:<id>" | "label:<id>".
    Resolved(String),
}

/// Resolve which inbox tab a thread lands in, mirroring the list view's
/// Important/Other fallback for threads that carry no explicit `routed_tab`.
/// Returns `None` when the thread no longer exists.
pub fn resolve_tab(conn: &Connection, thread_id: i64) -> Result<Option<RoutedTab>> {
    let routed: Option<Option<String>> = conn
        .query_row(
            "SELECT routed_tab FROM threads WHERE id = ?1",
            params![thread_id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(routed) = routed else {
        return Ok(None);
    };
    Ok(Some(match routed.as_deref() {
        Some("pending") => RoutedTab::Pending,
        Some(key) => RoutedTab::Resolved(key.to_owned()),
        // Unrouted (legacy threads): a thread whose incoming mail is entirely
        // automated shows under Other, otherwise Important - the same split the
        // thread list applies to a null `routed_tab`.
        None => {
            let all_automated: bool = conn.query_row(
                "SELECT EXISTS (SELECT 1 FROM messages
                     WHERE thread_id = ?1 AND is_draft = 0 AND is_outgoing = 0
                       AND is_automated = 1)
                 AND NOT EXISTS (SELECT 1 FROM messages
                     WHERE thread_id = ?1 AND is_draft = 0 AND is_outgoing = 0
                       AND is_automated = 0)",
                params![thread_id],
                |r| r.get(0),
            )?;
            RoutedTab::Resolved(if all_automated { "other" } else { "important" }.to_owned())
        }
    }))
}

/// Push a pending item's next dispatch out without consuming a delivery attempt.
/// Used to wait for a thread's tab to resolve before applying scope filtering.
pub fn defer(conn: &Connection, id: i64, not_before: i64) -> Result<bool> {
    Ok(conn.execute(
        "UPDATE notification_outbox SET not_before = ?2
         WHERE id = ?1 AND state = 'pending'",
        params![id, not_before],
    )? > 0)
}

pub fn get(conn: &Connection, id: i64) -> Result<Option<NotificationOutboxItem>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM notification_outbox WHERE id = ?1"
    ))?;
    Ok(stmt.query_row(params![id], from_row).optional()?)
}

/// Pending notifications ready for dispatch, oldest first.
pub fn list_due(conn: &Connection, now: i64, limit: i64) -> Result<Vec<NotificationOutboxItem>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM notification_outbox
         WHERE state = 'pending' AND (not_before IS NULL OR not_before <= ?1)
         ORDER BY created_at, id LIMIT ?2"
    ))?;
    let rows = stmt
        .query_map(params![now, limit], from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Claim a due row immediately before calling the native notification API.
/// Attempts count actual delivery starts rather than scheduling failures.
pub fn try_claim(conn: &Connection, id: i64, claimed_at: i64) -> Result<bool> {
    Ok(conn.execute(
        "UPDATE notification_outbox
         SET state = 'delivering', attempts = attempts + 1, claimed_at = ?2,
             last_error = NULL
         WHERE id = ?1 AND state = 'pending'
           AND (not_before IS NULL OR not_before <= ?2)",
        params![id, claimed_at],
    )? > 0)
}

pub fn mark_delivered(
    conn: &Connection,
    id: i64,
    in_summary: bool,
    delivered_at: i64,
) -> Result<bool> {
    Ok(conn.execute(
        "UPDATE notification_outbox
         SET state = 'delivered', in_summary = ?2, delivered_at = ?3, not_before = NULL,
             last_error = NULL
         WHERE id = ?1 AND state IN ('pending','delivering')",
        params![id, in_summary, delivered_at],
    )? > 0)
}

pub fn mark_suppressed(
    conn: &Connection,
    id: i64,
    suppressed_at: i64,
    reason: &str,
) -> Result<bool> {
    Ok(conn.execute(
        "UPDATE notification_outbox
         SET state = 'suppressed', suppressed_at = ?2, suppression_reason = ?3,
             not_before = NULL, last_error = NULL
         WHERE id = ?1 AND state IN ('pending','delivering')",
        params![id, suppressed_at, reason],
    )? > 0)
}

pub fn retry(conn: &Connection, id: i64, retry_at: i64, error: &str) -> Result<bool> {
    Ok(conn.execute(
        "UPDATE notification_outbox
         SET state = 'pending', not_before = ?2, claimed_at = NULL, last_error = ?3
         WHERE id = ?1 AND state = 'delivering'",
        params![id, retry_at, error],
    )? > 0)
}

/// Recover a process crash after claim but before a terminal update.
pub fn recover_delivering(conn: &Connection, retry_at: i64) -> Result<usize> {
    Ok(conn.execute(
        "UPDATE notification_outbox
         SET state = 'pending', not_before = ?1, claimed_at = NULL,
             last_error = COALESCE(last_error, 'delivery interrupted')
         WHERE state = 'delivering'",
        params![retry_at],
    )?)
}

/// How long a notification waits for its thread's AI route before another look.
pub const ROUTE_WAIT_MS: i64 = 60_000;

/// How long a notification stays up, and settled rows are kept. Windows and
/// Android drop their own notifications after three days; macOS and the Linux
/// desktops keep them, so one shown this long ago is taken back (see
/// [`stale`]), and its row forgotten this long after that (see [`prune`]).
pub const KEEP_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// How long after an account's summary was last raised new mail still joins
/// it: the rest of the sync it came from, with the mail whose tab is routed
/// a little later ([`ROUTE_WAIT_MS`]). Mail after that starts over, on its
/// own or in a summary of its own: the user may have dismissed the old one.
pub const SUMMARY_JOIN_MS: i64 = 2 * ROUTE_WAIT_MS;

/// One new-mail notification for the host to raise. The fields are raw
/// database values, so the host builds the translated title and body itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewMailNotification {
    pub account_id: i64,
    pub message_id: i64,
    pub thread_id: i64,
    /// The sender's display name, or the address when there is none.
    pub sender: String,
    pub subject: String,
    /// The start of the message text, when its body is already here.
    pub preview: Option<String>,
    /// When the server received the message, else the message's own date.
    pub received_at: i64,
    /// The account's address, to tell accounts apart when there are several.
    pub account: Option<String>,
    /// Mail from a person with sounds on: the host may play the new-mail
    /// sound for it.
    pub sound: bool,
    /// The account has an Archive or All Mail folder to archive into;
    /// otherwise the message can only go to the Trash.
    pub can_archive: bool,
}

/// One dispatch step's notifications for the host to raise, with what the
/// host needs to sum them up per account.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NotificationBatch {
    pub mail: Vec<NewMailNotification>,
    /// For each account in `mail`, how many notifications from earlier
    /// batches its summary counts while new mail still joins it (see
    /// [`joinable_summaries`]).
    pub summarized: BTreeMap<i64, u32>,
}

/// What became of a notification the host was asked to raise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotificationOutcome {
    /// The system shows it on its own.
    Shown,
    /// The system shows it inside its account's summary of new mail.
    ShownInSummary,
    /// Not shown now; worth another try later, such as when the system's
    /// notification service didn't answer.
    Retry(String),
    /// Not shown, and trying again won't help: notifications are turned off
    /// for the app, or the system has no notification service.
    Unavailable(String),
    /// Not shown because the message is already on screen in the app.
    OnScreen,
}

/// A shown notification, under the ids the host raised it with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ShownNotification {
    pub account_id: i64,
    pub message_id: i64,
}

/// Shown notifications to take back: the ones raised on their own, and the
/// accounts whose summary of new mail no longer counts anything.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Withdrawal {
    pub messages: Vec<ShownNotification>,
    pub summaries: Vec<i64>,
    /// The outbox rows this withdraws.
    pub ids: Vec<i64>,
}

impl Withdrawal {
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }
}

/// The due rows one [`settle_due`] step read, and the ones it claimed, with
/// their outbox ids.
#[derive(Debug)]
pub struct Settled {
    pub read: usize,
    pub claimed: Vec<(i64, NewMailNotification)>,
}

enum Decision {
    Publish(i64),
    Wait,
    Suppress(&'static str),
}

/// Settle up to `limit` due notifications: claim the ones to publish, and
/// suppress or postpone the rest.
pub fn settle_due(conn: &Connection, settings: &Settings, now: i64, limit: i64) -> Result<Settled> {
    let due = list_due(conn, now, limit)?;
    let mut claimed = Vec::new();
    for item in &due {
        match decide(conn, settings, now, item)? {
            Decision::Publish(thread_id) => {
                if try_claim(conn, item.id, now)? {
                    claimed.push((item.id, notification(conn, settings, item, thread_id)?));
                }
            }
            Decision::Wait => {
                defer(conn, item.id, now + ROUTE_WAIT_MS)?;
            }
            Decision::Suppress(reason) => {
                mark_suppressed(conn, item.id, now, reason)?;
            }
        }
    }
    Ok(Settled {
        read: due.len(),
        claimed,
    })
}

/// The notification for a claimed row: the snapshot taken when it was
/// queued, plus what has arrived since, such as the body's preview.
fn notification(
    conn: &Connection,
    settings: &Settings,
    item: &NotificationOutboxItem,
    thread_id: i64,
) -> Result<NewMailNotification> {
    let sender = item
        .sender_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .or_else(|| item.sender_addr.clone())
        .unwrap_or_default();
    let (snippet, received_at, account, several_accounts, can_archive) = conn.query_row(
        "SELECT m.snippet, COALESCE(m.internal_date, m.date), a.email,
                (SELECT COUNT(*) FROM accounts WHERE mail_protocol <> 'local') > 1,
                EXISTS (SELECT 1 FROM folders f
                        WHERE f.account_id = m.account_id AND f.role IN (?2, ?3))
         FROM messages m JOIN accounts a ON a.id = m.account_id
         WHERE m.id = ?1",
        params![item.notified_message_id, roles::ARCHIVE, roles::ALL],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, bool>(3)?,
                row.get::<_, bool>(4)?,
            ))
        },
    )?;
    Ok(NewMailNotification {
        account_id: item.account_id,
        message_id: item.notified_message_id,
        thread_id,
        sender,
        subject: item.subject.clone(),
        preview: Some(snippet.trim().to_owned()).filter(|preview| !preview.is_empty()),
        received_at,
        account: account.filter(|_| several_accounts),
        sound: settings.sound_enabled && item.audible,
        can_archive,
    })
}

/// How many notifications each of these accounts' summaries already counts.
pub fn summarized(
    conn: &Connection,
    account_ids: impl IntoIterator<Item = i64>,
) -> Result<BTreeMap<i64, u32>> {
    let mut stmt = conn.prepare(
        "SELECT COUNT(*) FROM notification_outbox
         WHERE account_id = ?1 AND state = 'delivered' AND in_summary = 1",
    )?;
    let mut counts = BTreeMap::new();
    for account_id in account_ids {
        let count: u32 = stmt.query_row(params![account_id], |row| row.get(0))?;
        counts.insert(account_id, count);
    }
    Ok(counts)
}

/// How many notifications each of these accounts' summaries counts while new
/// mail still joins it: the ones raised within [`SUMMARY_JOIN_MS`] of each
/// other, the last within that of `now`. An older summary counts none.
pub fn joinable_summaries(
    conn: &Connection,
    account_ids: impl IntoIterator<Item = i64>,
    now: i64,
) -> Result<BTreeMap<i64, u32>> {
    let mut stmt = conn.prepare(
        "SELECT COALESCE(delivered_at, created_at) FROM notification_outbox
         WHERE account_id = ?1 AND state = 'delivered' AND in_summary = 1
         ORDER BY 1 DESC",
    )?;
    let mut counts = BTreeMap::new();
    for account_id in account_ids {
        let mut count = 0;
        let mut later = now;
        let mut rows = stmt.query(params![account_id])?;
        while let Some(row) = rows.next()? {
            let raised: i64 = row.get(0)?;
            if later.saturating_sub(raised) >= SUMMARY_JOIN_MS {
                break;
            }
            count += 1;
            later = raised;
        }
        counts.insert(account_id, count);
    }
    Ok(counts)
}

/// Whether a due notification goes out. Nothing does while notifications are
/// off, so turning them on later doesn't replay old mail. Mail stops counting
/// as new after the window sync uses to queue it, or once it no longer
/// waits in the Inbox (see [`still_new`]). A thread whose AI route is still
/// pending waits.
fn decide(
    conn: &Connection,
    settings: &Settings,
    now: i64,
    item: &NotificationOutboxItem,
) -> Result<Decision> {
    if !settings.notifications_enabled {
        return Ok(Decision::Suppress("notifications are off"));
    }
    if now - item.created_at >= CHIME_RECENT_MS {
        return Ok(Decision::Suppress("no longer new"));
    }
    if !still_new(conn, item.message_id)? {
        return Ok(Decision::Suppress("read or moved"));
    }
    let Some(thread_id) = item.thread_id else {
        return Ok(Decision::Suppress("thread was deleted"));
    };
    Ok(match resolve_tab(conn, thread_id)? {
        None => Decision::Suppress("thread was deleted"),
        Some(RoutedTab::Pending) => Decision::Wait,
        Some(RoutedTab::Resolved(tab)) if scope_allows(settings, &tab) => {
            Decision::Publish(thread_id)
        }
        Some(RoutedTab::Resolved(_)) => Decision::Suppress("outside notification scope"),
    })
}

/// The SQL condition, on a message `m` of account `a`, for mail that is
/// still worth a notification: unread and waiting in the Inbox, by folder or
/// by Gmail label, and not snoozed. Mail that was read, archived, moved to
/// Trash or Spam, or snoozed, here or on another device, no longer is.
const STILL_NEW: &str = "m.is_read = 0
     AND (EXISTS (SELECT 1 FROM folders f WHERE f.id = m.folder_id AND f.role = ?{inbox})
          OR (a.provider = 'gmail' AND EXISTS (
                SELECT 1 FROM message_folders mf JOIN folders f ON f.id = mf.folder_id
                WHERE mf.message_id = m.id AND f.role = ?{inbox})))
     AND NOT EXISTS (SELECT 1 FROM snoozes s WHERE s.thread_id = m.thread_id)";

fn still_new_condition(inbox: usize) -> String {
    STILL_NEW.replace("{inbox}", &inbox.to_string())
}

/// Whether the message is still worth a notification (see [`STILL_NEW`]); a
/// message that is gone isn't.
fn still_new(conn: &Connection, message_id: Option<i64>) -> Result<bool> {
    let Some(message_id) = message_id else {
        return Ok(false);
    };
    Ok(conn
        .query_row(
            &format!(
                "SELECT {} FROM messages m JOIN accounts a ON a.id = m.account_id
                 WHERE m.id = ?1",
                still_new_condition(2)
            ),
            params![message_id, roles::INBOX],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(false))
}

/// Apply the user's notification scope to a resolved tab key.
fn scope_allows(settings: &Settings, tab: &str) -> bool {
    match settings.notification_scope.as_str() {
        "all" => true,
        "tabs" => settings
            .notification_tabs
            .iter()
            .any(|selected| selected == tab),
        // "important" and any future or unknown value keep the historical
        // Important-only behaviour rather than notifying for every message.
        _ => tab == "important",
    }
}

/// When the next postponed notification falls due, if any.
pub fn next_due_at(conn: &Connection, now: i64) -> Result<Option<i64>> {
    Ok(conn.query_row(
        "SELECT MIN(not_before) FROM notification_outbox
         WHERE state = 'pending' AND not_before > ?1",
        params![now],
        |row| row.get(0),
    )?)
}

/// Shown notifications whose mail is no longer new (see [`STILL_NEW`]):
/// read, moved out of the Inbox, snoozed or deleted, or its account removed.
/// So is one shown [`KEEP_MS`] ago, before its row is forgotten.
pub fn stale(conn: &Connection, now: i64) -> Result<Withdrawal> {
    let mut stmt = conn.prepare(&format!(
        "SELECT o.id, o.account_id, o.notified_message_id, o.in_summary
         FROM notification_outbox o
         WHERE o.state = 'delivered'
           AND (COALESCE(o.delivered_at, o.created_at) < ?2
                OR NOT EXISTS (
                  SELECT 1 FROM messages m JOIN accounts a ON a.id = m.account_id
                  WHERE m.id = o.message_id AND {}))
         ORDER BY o.id",
        still_new_condition(1)
    ))?;
    let rows = stmt
        .query_map(
            params![roles::INBOX, now.saturating_sub(KEEP_MS)],
            shown_row,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    withdrawal(conn, rows)
}

/// Every shown notification, to take them all back when notifications are
/// turned off.
pub fn shown(conn: &Connection) -> Result<Withdrawal> {
    let mut stmt = conn.prepare(
        "SELECT id, account_id, notified_message_id, in_summary
         FROM notification_outbox WHERE state = 'delivered' ORDER BY id",
    )?;
    let rows = stmt
        .query_map([], shown_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    withdrawal(conn, rows)
}

fn shown_row(row: &Row) -> rusqlite::Result<(i64, ShownNotification, bool)> {
    Ok((
        row.get(0)?,
        ShownNotification {
            account_id: row.get(1)?,
            message_id: row.get(2)?,
        },
        row.get(3)?,
    ))
}

/// Group rows to withdraw: the ones shown on their own by message, and an
/// account's summary once every notification it counts goes.
fn withdrawal(conn: &Connection, rows: Vec<(i64, ShownNotification, bool)>) -> Result<Withdrawal> {
    let mut out = Withdrawal::default();
    let mut leaving: BTreeMap<i64, u32> = BTreeMap::new();
    for (id, shown, in_summary) in rows {
        out.ids.push(id);
        if in_summary {
            *leaving.entry(shown.account_id).or_default() += 1;
        } else {
            out.messages.push(shown);
        }
    }
    let counted = summarized(conn, leaving.keys().copied())?;
    out.summaries = leaving
        .into_iter()
        .filter(|(account_id, leaving)| {
            counted
                .get(account_id)
                .is_some_and(|counted| counted <= leaving)
        })
        .map(|(account_id, _)| account_id)
        .collect();
    Ok(out)
}

/// Record that the host took these notifications back.
pub fn mark_withdrawn(conn: &Connection, ids: &[i64], withdrawn_at: i64) -> Result<usize> {
    let mut stmt = conn.prepare(
        "UPDATE notification_outbox SET state = 'withdrawn', withdrawn_at = ?2
         WHERE id = ?1 AND state = 'delivered'",
    )?;
    let mut changed = 0;
    for id in ids {
        changed += stmt.execute(params![id, withdrawn_at])?;
    }
    Ok(changed)
}

/// Forget rows settled more than [`KEEP_MS`] ago. A shown notification's row
/// stays until the notification is taken back, which [`stale`] sees to.
pub fn prune(conn: &Connection, now: i64) -> Result<usize> {
    Ok(conn.execute(
        "DELETE FROM notification_outbox
         WHERE (state = 'suppressed' AND COALESCE(suppressed_at, created_at) < ?1)
            OR (state = 'withdrawn' AND COALESCE(withdrawn_at, created_at) < ?1)",
        params![now.saturating_sub(KEEP_MS)],
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testutil;

    #[test]
    fn enqueue_is_unique_and_snapshots_message_fields() {
        let c = testutil::conn();
        testutil::seed_account(&c);
        let (thread_id, message_id) =
            testutil::seed_message(&c, "alice@test.dev", "Deployment", false);
        c.execute(
            "UPDATE messages SET from_name = 'Alice' WHERE id = ?1",
            params![message_id],
        )
        .unwrap();

        let id = enqueue_at(&c, message_id, true, 100).unwrap();
        assert_eq!(enqueue_at(&c, message_id, false, 200).unwrap(), id);
        let row = get(&c, id).unwrap().unwrap();
        assert_eq!(row.account_id, 1);
        assert_eq!(row.message_id, Some(message_id));
        assert_eq!(row.notified_message_id, message_id);
        assert_eq!(row.thread_id, Some(thread_id));
        assert_eq!(row.sender_name.as_deref(), Some("Alice"));
        assert_eq!(row.sender_addr.as_deref(), Some("alice@test.dev"));
        assert_eq!(row.subject, "Deployment");
        assert!(row.audible);
        assert_eq!(row.created_at, 100);
        assert_eq!(list_due(&c, 100, 10).unwrap().len(), 1);
    }

    #[test]
    fn claim_retry_delivery_and_recovery_are_durable() {
        let c = testutil::conn();
        testutil::seed_account(&c);
        let (_, message_id) = testutil::seed_message(&c, "a@test.dev", "One", false);
        let id = enqueue_at(&c, message_id, false, 10).unwrap();

        assert!(try_claim(&c, id, 20).unwrap());
        assert!(!try_claim(&c, id, 20).unwrap());
        assert!(retry(&c, id, 50, "native service unavailable").unwrap());
        assert!(list_due(&c, 49, 10).unwrap().is_empty());
        assert_eq!(list_due(&c, 50, 10).unwrap()[0].attempts, 1);

        assert!(try_claim(&c, id, 50).unwrap());
        assert_eq!(recover_delivering(&c, 60).unwrap(), 1);
        assert!(try_claim(&c, id, 60).unwrap());
        assert!(mark_delivered(&c, id, false, 61).unwrap());
        let row = get(&c, id).unwrap().unwrap();
        assert_eq!(row.state, "delivered");
        assert!(!row.in_summary);
        assert_eq!(row.attempts, 3);
        assert_eq!(row.delivered_at, Some(61));
        assert!(list_due(&c, i64::MAX, 10).unwrap().is_empty());
    }

    #[test]
    fn resolve_tab_uses_routed_value_then_heuristic() {
        let c = testutil::conn();
        testutil::seed_account(&c);

        // Explicit routed_tab wins verbatim.
        let (human_thread, _) = testutil::seed_message(&c, "alice@test.dev", "Hi", false);
        c.execute(
            "UPDATE threads SET routed_tab = 'split:7' WHERE id = ?1",
            params![human_thread],
        )
        .unwrap();
        assert_eq!(
            resolve_tab(&c, human_thread).unwrap(),
            Some(RoutedTab::Resolved("split:7".to_owned()))
        );

        // A pending thread reports Pending so the dispatcher can wait.
        c.execute(
            "UPDATE threads SET routed_tab = 'pending' WHERE id = ?1",
            params![human_thread],
        )
        .unwrap();
        assert_eq!(
            resolve_tab(&c, human_thread).unwrap(),
            Some(RoutedTab::Pending)
        );

        // Null routed_tab falls back to Important for human mail...
        c.execute(
            "UPDATE threads SET routed_tab = NULL WHERE id = ?1",
            params![human_thread],
        )
        .unwrap();
        assert_eq!(
            resolve_tab(&c, human_thread).unwrap(),
            Some(RoutedTab::Resolved("important".to_owned()))
        );

        // ...and Other for all-automated mail.
        let (auto_thread, _) = testutil::seed_message(&c, "news@test.dev", "Digest", true);
        assert_eq!(
            resolve_tab(&c, auto_thread).unwrap(),
            Some(RoutedTab::Resolved("other".to_owned()))
        );

        assert_eq!(resolve_tab(&c, 9999).unwrap(), None);
    }

    #[test]
    fn defer_pushes_out_pending_dispatch_only() {
        let c = testutil::conn();
        testutil::seed_account(&c);
        let (_, message_id) = testutil::seed_message(&c, "a@test.dev", "One", false);
        let id = enqueue_at(&c, message_id, false, 10).unwrap();

        assert!(defer(&c, id, 500).unwrap());
        assert!(list_due(&c, 499, 10).unwrap().is_empty());
        assert_eq!(list_due(&c, 500, 10).unwrap()[0].attempts, 0);

        // A claimed (in-flight) row is never rewound by defer.
        assert!(try_claim(&c, id, 500).unwrap());
        assert!(!defer(&c, id, 900).unwrap());
    }

    #[test]
    fn suppression_is_terminal_and_deleting_the_message_keeps_the_row() {
        let c = testutil::conn();
        c.pragma_update(None, "foreign_keys", "ON").unwrap();
        testutil::seed_account(&c);
        let (_, message_id) = testutil::seed_message(&c, "a@test.dev", "Two", false);
        let id = enqueue_at(&c, message_id, false, 10).unwrap();
        assert!(mark_suppressed(&c, id, 11, "window focused").unwrap());
        let row = get(&c, id).unwrap().unwrap();
        assert_eq!(row.state, "suppressed");
        assert_eq!(row.suppression_reason.as_deref(), Some("window focused"));
        assert!(!mark_delivered(&c, id, false, 12).unwrap());

        c.execute("DELETE FROM messages WHERE id = ?1", params![message_id])
            .unwrap();
        let row = get(&c, id).unwrap().unwrap();
        assert_eq!(row.message_id, None);
        assert_eq!(row.notified_message_id, message_id);
    }

    fn suppression(c: &Connection, id: i64) -> Option<String> {
        let row = get(c, id).unwrap().unwrap();
        assert_eq!(row.state, "suppressed");
        row.suppression_reason
    }

    #[test]
    fn settle_claims_new_mail_in_scope_and_settles_the_rest() {
        let c = testutil::conn();
        testutil::seed_account(&c);
        c.execute(
            "INSERT INTO folders (id, account_id, path, role) VALUES (2, 1, 'Trash', 'trash')",
            [],
        )
        .unwrap();
        let now = 10 * CHIME_RECENT_MS;
        let settings = Settings::default();

        let (thread, fresh) = testutil::seed_message(&c, "alice@test.dev", "Fresh", false);
        c.execute(
            "UPDATE messages SET from_name = ' Alice ', snippet = ' See you there ',
                 internal_date = 1500
             WHERE id = ?1",
            params![fresh],
        )
        .unwrap();
        let (_, read) = testutil::seed_message(&c, "bob@test.dev", "Read", false);
        c.execute(
            "UPDATE messages SET is_read = 1 WHERE id = ?1",
            params![read],
        )
        .unwrap();
        let (_, trashed) = testutil::seed_message(&c, "carol@test.dev", "Trashed", false);
        c.execute(
            "UPDATE messages SET folder_id = 2 WHERE id = ?1",
            params![trashed],
        )
        .unwrap();
        let (snoozed_thread, snoozed) =
            testutil::seed_message(&c, "zoe@test.dev", "Snoozed", false);
        c.execute(
            "INSERT INTO snoozes (thread_id, account_id, wake_at) VALUES (?1, 1, ?2)",
            params![snoozed_thread, now + 1],
        )
        .unwrap();
        let (_, bulk) = testutil::seed_message(&c, "news@test.dev", "Digest", true);
        let (routing_thread, routing) =
            testutil::seed_message(&c, "dave@test.dev", "Routing", false);
        c.execute(
            "UPDATE threads SET routed_tab = 'pending' WHERE id = ?1",
            params![routing_thread],
        )
        .unwrap();
        let (_, old) = testutil::seed_message(&c, "erin@test.dev", "Old", false);

        let fresh_id = enqueue_at(&c, fresh, true, now - 1_000).unwrap();
        let [read_id, trashed_id, snoozed_id, bulk_id, routing_id] =
            [read, trashed, snoozed, bulk, routing]
                .map(|id| enqueue_at(&c, id, false, now - 1_000).unwrap());
        let old_id = enqueue_at(&c, old, true, now - CHIME_RECENT_MS).unwrap();

        let settled = settle_due(&c, &settings, now, 10).unwrap();
        assert_eq!(settled.read, 7);
        assert_eq!(
            settled.claimed,
            vec![(
                fresh_id,
                NewMailNotification {
                    account_id: 1,
                    message_id: fresh,
                    thread_id: thread,
                    sender: "Alice".into(),
                    subject: "Fresh".into(),
                    preview: Some("See you there".into()),
                    received_at: 1500,
                    account: None,
                    sound: true,
                    can_archive: false,
                }
            )]
        );
        assert_eq!(get(&c, fresh_id).unwrap().unwrap().state, "delivering");
        for id in [read_id, trashed_id, snoozed_id] {
            assert_eq!(suppression(&c, id).as_deref(), Some("read or moved"));
        }
        assert_eq!(
            suppression(&c, bulk_id).as_deref(),
            Some("outside notification scope")
        );
        assert_eq!(suppression(&c, old_id).as_deref(), Some("no longer new"));

        // A pending AI route waits without using up a delivery attempt...
        let waiting = get(&c, routing_id).unwrap().unwrap();
        assert_eq!(waiting.state, "pending");
        assert_eq!(waiting.attempts, 0);
        assert_eq!(next_due_at(&c, now).unwrap(), Some(now + ROUTE_WAIT_MS));
        assert!(
            settle_due(&c, &settings, now, 10)
                .unwrap()
                .claimed
                .is_empty()
        );

        // ...and goes out once the route resolves.
        c.execute(
            "UPDATE threads SET routed_tab = 'important' WHERE id = ?1",
            params![routing_thread],
        )
        .unwrap();
        let later = settle_due(&c, &settings, now + ROUTE_WAIT_MS, 10).unwrap();
        assert_eq!(later.claimed.len(), 1);
        assert_eq!(later.claimed[0].0, routing_id);
        assert!(!later.claimed[0].1.sound);
        assert_eq!(next_due_at(&c, now + ROUTE_WAIT_MS).unwrap(), None);
    }

    #[test]
    fn settled_mail_names_its_account_only_among_several() {
        let c = testutil::conn();
        testutil::seed_account(&c);
        let (_, message_id) = testutil::seed_message(&c, "a@test.dev", "Hello", false);
        let settings = Settings {
            sound_enabled: false,
            ..Settings::default()
        };
        let id = enqueue_at(&c, message_id, true, 1_000).unwrap();
        let claimed = settle_due(&c, &settings, 2_000, 10).unwrap().claimed;
        assert_eq!(claimed[0].1.account, None);
        // Sounds off: never a sound, even for mail from a person.
        assert!(!claimed[0].1.sound);
        // No Archive or All Mail folder: the message can only be deleted.
        assert!(!claimed[0].1.can_archive);
        assert!(retry(&c, id, 2_000, "later").unwrap());

        c.execute(
            "INSERT INTO accounts (id, email, provider, auth_kind, username,
             incoming_host, incoming_port, smtp_host, smtp_port, created_at, mail_protocol,
             settings_json)
             VALUES (2, 'work@test.dev', 'generic', 'password', 'w', 'h', 993, 'h', 587, 0,
                     'imap', '{}')",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO folders (id, account_id, path, role) VALUES (2, 1, 'Archive', 'archive')",
            [],
        )
        .unwrap();
        let claimed = settle_due(&c, &settings, 3_000, 10).unwrap().claimed;
        assert_eq!(claimed[0].1.account.as_deref(), Some("me@test.dev"));
        assert!(claimed[0].1.can_archive);
    }

    #[test]
    fn settle_suppresses_mail_while_notifications_are_off() {
        let c = testutil::conn();
        testutil::seed_account(&c);
        let (_, message_id) = testutil::seed_message(&c, "a@test.dev", "Quiet", false);
        let id = enqueue_at(&c, message_id, false, 1_000).unwrap();
        let settings = Settings {
            notifications_enabled: false,
            ..Settings::default()
        };

        let settled = settle_due(&c, &settings, 2_000, 10).unwrap();
        assert!(settled.claimed.is_empty());
        assert_eq!(
            suppression(&c, id).as_deref(),
            Some("notifications are off")
        );
    }

    /// Queue and deliver a notification for new mail from `from`, on its own
    /// or in its account's summary.
    fn delivered(c: &Connection, from: &str, in_summary: bool) -> (i64, i64) {
        let (_, message_id) = testutil::seed_message(c, from, "Hi", false);
        let id = enqueue_at(c, message_id, false, 10).unwrap();
        assert!(try_claim(c, id, 20).unwrap());
        assert!(mark_delivered(c, id, in_summary, 30).unwrap());
        (id, message_id)
    }

    #[test]
    fn stale_finds_read_moved_snoozed_and_deleted_mail() {
        let c = testutil::conn();
        c.pragma_update(None, "foreign_keys", "ON").unwrap();
        testutil::seed_account(&c);
        c.execute(
            "INSERT INTO folders (id, account_id, path, role) VALUES (2, 1, 'Archive', 'archive')",
            [],
        )
        .unwrap();
        let (_, waiting) = delivered(&c, "a@test.dev", false);
        let (read_id, read) = delivered(&c, "b@test.dev", false);
        let (moved_id, moved) = delivered(&c, "c@test.dev", false);
        let (snoozed_id, snoozed) = delivered(&c, "d@test.dev", false);
        let (deleted_id, deleted) = delivered(&c, "e@test.dev", false);
        c.execute(
            "UPDATE messages SET is_read = 1 WHERE id = ?1",
            params![read],
        )
        .unwrap();
        c.execute(
            "UPDATE messages SET folder_id = 2 WHERE id = ?1",
            params![moved],
        )
        .unwrap();
        c.execute(
            "INSERT INTO snoozes (thread_id, account_id, wake_at)
             SELECT thread_id, 1, 99 FROM messages WHERE id = ?1",
            params![snoozed],
        )
        .unwrap();
        c.execute("DELETE FROM messages WHERE id = ?1", params![deleted])
            .unwrap();

        let withdrawal = stale(&c, 40).unwrap();
        assert_eq!(
            withdrawal.ids,
            vec![read_id, moved_id, snoozed_id, deleted_id]
        );
        assert_eq!(
            withdrawal.messages,
            [read, moved, snoozed, deleted].map(|message_id| ShownNotification {
                account_id: 1,
                message_id
            })
        );
        assert!(withdrawal.summaries.is_empty());

        assert_eq!(mark_withdrawn(&c, &withdrawal.ids, 40).unwrap(), 4);
        assert!(stale(&c, 40).unwrap().is_empty());
        let everything = shown(&c).unwrap();
        assert_eq!(
            everything.messages,
            vec![ShownNotification {
                account_id: 1,
                message_id: waiting
            }]
        );
    }

    #[test]
    fn a_gmail_message_stays_new_while_it_carries_the_inbox_label() {
        let c = testutil::conn();
        testutil::seed_account(&c);
        c.execute_batch(
            "UPDATE accounts SET provider = 'gmail', mail_protocol = 'gmail', auth_kind = 'oauth2';
             INSERT INTO folders (id, account_id, path, role) VALUES (2, 1, '[Gmail]/All Mail', 'all');",
        )
        .unwrap();
        let (_, message_id) = delivered(&c, "a@test.dev", false);
        c.execute_batch(&format!(
            "UPDATE messages SET folder_id = 2 WHERE id = {message_id};
             INSERT INTO message_folders (message_id, folder_id) VALUES ({message_id}, 1);"
        ))
        .unwrap();
        assert!(stale(&c, 40).unwrap().is_empty());

        // Archived elsewhere: the Inbox label goes.
        c.execute(
            "DELETE FROM message_folders WHERE message_id = ?1",
            params![message_id],
        )
        .unwrap();
        assert_eq!(stale(&c, 40).unwrap().messages.len(), 1);
    }

    #[test]
    fn a_summary_goes_once_nothing_it_counts_is_new() {
        let c = testutil::conn();
        testutil::seed_account(&c);
        let (first_id, first) = delivered(&c, "a@test.dev", true);
        let (_, second) = delivered(&c, "b@test.dev", true);
        assert_eq!(summarized(&c, [1]).unwrap(), BTreeMap::from([(1, 2)]));

        c.execute(
            "UPDATE messages SET is_read = 1 WHERE id = ?1",
            params![first],
        )
        .unwrap();
        let partly = stale(&c, 40).unwrap();
        assert_eq!(partly.ids, vec![first_id]);
        assert!(partly.messages.is_empty());
        assert!(partly.summaries.is_empty());
        mark_withdrawn(&c, &partly.ids, 40).unwrap();

        c.execute(
            "UPDATE messages SET is_read = 1 WHERE id = ?1",
            params![second],
        )
        .unwrap();
        assert_eq!(stale(&c, 40).unwrap().summaries, vec![1]);
    }

    #[test]
    fn prune_forgets_settled_rows_after_a_week() {
        let c = testutil::conn();
        testutil::seed_account(&c);
        let (_, pending) = testutil::seed_message(&c, "a@test.dev", "Pending", false);
        let pending_id = enqueue_at(&c, pending, false, 10).unwrap();
        let (shown_id, _) = delivered(&c, "b@test.dev", false);
        let (_, quiet) = testutil::seed_message(&c, "c@test.dev", "Quiet", false);
        let quiet_id = enqueue_at(&c, quiet, false, 10).unwrap();
        mark_suppressed(&c, quiet_id, 10, "notifications are off").unwrap();

        assert_eq!(prune(&c, KEEP_MS).unwrap(), 0);
        // A shown notification's row stays until it is taken back.
        assert_eq!(prune(&c, KEEP_MS + 31).unwrap(), 1);
        assert!(get(&c, quiet_id).unwrap().is_none());
        assert!(get(&c, shown_id).unwrap().is_some());

        mark_withdrawn(&c, &[shown_id], KEEP_MS + 31).unwrap();
        assert_eq!(prune(&c, 2 * KEEP_MS + 31).unwrap(), 0);
        assert_eq!(prune(&c, 2 * KEEP_MS + 32).unwrap(), 1);
        assert!(get(&c, shown_id).unwrap().is_none());
        assert!(get(&c, pending_id).unwrap().is_some());
    }

    #[test]
    fn a_notification_is_taken_back_after_a_week() {
        let c = testutil::conn();
        testutil::seed_account(&c);
        let (alone_id, alone) = delivered(&c, "a@test.dev", false);
        let (first_id, _) = delivered(&c, "b@test.dev", true);
        let (second_id, _) = delivered(&c, "c@test.dev", true);
        // Shown at 30, and the mail is still new.
        assert!(stale(&c, 30 + KEEP_MS).unwrap().is_empty());

        let expired = stale(&c, 31 + KEEP_MS).unwrap();
        assert_eq!(expired.ids, vec![alone_id, first_id, second_id]);
        assert_eq!(
            expired.messages,
            vec![ShownNotification {
                account_id: 1,
                message_id: alone
            }]
        );
        assert_eq!(expired.summaries, vec![1]);
    }

    #[test]
    fn mail_joins_a_summary_only_while_its_sync_lasts() {
        let c = testutil::conn();
        testutil::seed_account(&c);
        delivered(&c, "a@test.dev", true);
        delivered(&c, "b@test.dev", true);
        let joining = |now| joinable_summaries(&c, [1], now).unwrap()[&1];
        assert_eq!(joining(30 + SUMMARY_JOIN_MS - 1), 2);
        assert_eq!(joining(30 + SUMMARY_JOIN_MS), 0);

        // A later burst's summary counts only its own mail, while taking it
        // back still waits for all the mail a summary counted.
        let later = 30 + 10 * SUMMARY_JOIN_MS;
        let (_, message_id) = testutil::seed_message(&c, "c@test.dev", "Hi", false);
        let id = enqueue_at(&c, message_id, false, later).unwrap();
        assert!(try_claim(&c, id, later).unwrap());
        assert!(mark_delivered(&c, id, true, later).unwrap());
        assert_eq!(joining(later + SUMMARY_JOIN_MS - 1), 1);
        assert_eq!(summarized(&c, [1]).unwrap(), BTreeMap::from([(1, 3)]));
    }

    #[test]
    fn important_scope_only_notifies_important() {
        let settings = Settings::default();
        assert!(scope_allows(&settings, "important"));
        assert!(!scope_allows(&settings, "other"));
        assert!(!scope_allows(&settings, "split:1"));
    }

    #[test]
    fn all_scope_notifies_every_tab() {
        let settings = Settings {
            notification_scope: "all".into(),
            ..Settings::default()
        };
        assert!(scope_allows(&settings, "important"));
        assert!(scope_allows(&settings, "other"));
        assert!(scope_allows(&settings, "label:7"));
    }

    #[test]
    fn tabs_scope_honours_the_selected_tabs() {
        let settings = Settings {
            notification_scope: "tabs".into(),
            notification_tabs: vec!["other".into(), "label:7".into()],
            ..Settings::default()
        };
        assert!(!scope_allows(&settings, "important"));
        assert!(scope_allows(&settings, "other"));
        assert!(scope_allows(&settings, "label:7"));
        assert!(!scope_allows(&settings, "split:1"));
    }

    /// Unread incoming mail in the Inbox that the server received at `at`,
    /// in a conversation of its own unless `thread` is given.
    fn arrived(c: &Connection, from: &str, at: i64, thread: Option<i64>) -> (i64, i64) {
        let thread_id = thread.unwrap_or_else(|| {
            c.execute(
                "INSERT INTO threads (account_id, subject_norm, unread_count, last_message_at)
                 VALUES (1, 'news', 1, ?1)",
                params![at],
            )
            .unwrap();
            c.last_insert_rowid()
        });
        c.execute(
            "INSERT INTO messages (thread_id, account_id, folder_id, subject, from_addr, date,
             internal_date, is_read, is_automated, is_draft, is_outgoing)
             VALUES (?1, 1, 1, 'News', ?2, ?3, ?3, 0, 0, 0, 0)",
            params![thread_id, from, at],
        )
        .unwrap();
        (thread_id, c.last_insert_rowid())
    }

    fn queued(c: &Connection) -> Vec<(i64, bool)> {
        c.prepare("SELECT notified_message_id, audible FROM notification_outbox ORDER BY id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    #[test]
    fn mail_that_came_while_nobody_followed_is_queued_once_per_conversation() {
        let c = testutil::conn();
        testutil::seed_account(&c);
        c.execute(
            "INSERT INTO folders (id, account_id, path, role) VALUES (2, 1, 'Archive', 'archive')",
            [],
        )
        .unwrap();
        let now = 10 * CHIME_RECENT_MS;
        let minute = 60_000;
        let caught_up_at = now - 60 * minute;
        let since = BTreeMap::from([(1, caught_up_at)]);

        // Stored by the sync that caught up, which said nothing about it.
        arrived(&c, "old@test.dev", caught_up_at - 2 * minute, None);
        // Within the minute a server's clock may lag, as a sync counts it.
        let (_, skewed) = arrived(&c, "skew@test.dev", caught_up_at - minute / 2, None);
        let (_, person) = arrived(&c, "alice@test.dev", caught_up_at + 10 * minute, None);
        let (_, robot) = arrived(&c, "noreply@test.dev", caught_up_at + 11 * minute, None);
        // Two in one conversation: only the newest.
        let (thread, first) = arrived(&c, "bob@test.dev", caught_up_at + 12 * minute, None);
        let (_, second) = arrived(
            &c,
            "carol@test.dev",
            caught_up_at + 13 * minute,
            Some(thread),
        );
        // Read, sent, moved out of the Inbox, or already told of.
        let (_, read) = arrived(&c, "dave@test.dev", caught_up_at + 14 * minute, None);
        let (_, sent) = arrived(&c, "me@test.dev", caught_up_at + 15 * minute, None);
        let (_, archived) = arrived(&c, "erin@test.dev", caught_up_at + 16 * minute, None);
        let (_, told) = arrived(&c, "frank@test.dev", caught_up_at + 17 * minute, None);
        c.execute_batch(&format!(
            "UPDATE messages SET is_read = 1 WHERE id = {read};
             UPDATE messages SET is_outgoing = 1 WHERE id = {sent};
             UPDATE messages SET folder_id = 2 WHERE id = {archived};"
        ))
        .unwrap();
        enqueue_at(&c, told, true, caught_up_at + 17 * minute).unwrap();
        // A conversation told of while the app ran gets its newer message.
        let (earlier_thread, earlier) =
            arrived(&c, "gina@test.dev", caught_up_at - 30 * minute, None);
        let (_, followup) = arrived(
            &c,
            "gina@test.dev",
            caught_up_at + 18 * minute,
            Some(earlier_thread),
        );
        enqueue_at(&c, earlier, true, caught_up_at - 30 * minute).unwrap();

        let threads = enqueue_missed(&c, &since, now).unwrap();
        assert_eq!(threads.len(), 5, "{threads:?}");
        assert!(threads.contains(&(1, thread)));
        assert!(threads.contains(&(1, earlier_thread)));
        assert_eq!(
            queued(&c),
            [
                (told, true),
                (earlier, true),
                (skewed, true),
                (person, true),
                (robot, false),
                (second, true),
                (followup, true),
            ]
        );
        assert!(!queued(&c).iter().any(|&(id, _)| id == first));

        // Queued once: a second look finds nothing more.
        assert!(enqueue_missed(&c, &since, now).unwrap().is_empty());
        // Nor for an account that was never caught up.
        assert!(
            enqueue_missed(&c, &BTreeMap::new(), now)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn missed_mail_stays_within_the_day_a_sync_tells_of() {
        let c = testutil::conn();
        testutil::seed_account(&c);
        let now = 10 * CHIME_RECENT_MS;
        let hour = 60 * 60_000;
        // Last followed three days ago.
        let since = BTreeMap::from([(1, now - 72 * hour)]);
        arrived(&c, "old@test.dev", now - 25 * hour, None);
        let (_, recent) = arrived(&c, "new@test.dev", now - 2 * hour, None);

        enqueue_missed(&c, &since, now).unwrap();
        assert_eq!(queued(&c), [(recent, true)]);
    }

    #[test]
    fn missed_gmail_mail_counts_while_it_carries_the_inbox_label() {
        let c = testutil::conn();
        testutil::seed_account(&c);
        c.execute_batch(
            "UPDATE accounts SET provider = 'gmail', mail_protocol = 'gmail', auth_kind = 'oauth2';
             INSERT INTO folders (id, account_id, path, role) VALUES (2, 1, '[Gmail]/All Mail', 'all');",
        )
        .unwrap();
        let now = 10 * CHIME_RECENT_MS;
        let since = BTreeMap::from([(1, now - 60 * 60_000)]);
        let (_, labelled) = arrived(&c, "a@test.dev", now - 30 * 60_000, None);
        let (_, archived) = arrived(&c, "b@test.dev", now - 20 * 60_000, None);
        c.execute_batch(&format!(
            "UPDATE messages SET folder_id = 2 WHERE id IN ({labelled}, {archived});
             INSERT INTO message_folders (message_id, folder_id) VALUES ({labelled}, 1);"
        ))
        .unwrap();

        enqueue_missed(&c, &since, now).unwrap();
        assert_eq!(queued(&c), [(labelled, true)]);
    }
}
