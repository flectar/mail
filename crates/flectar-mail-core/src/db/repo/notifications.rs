use crate::error::{CoreError, Result};
use crate::models::{Settings, now_ms, roles};
use crate::sync::ingest::CHIME_RECENT_MS;
use rusqlite::{Connection, OptionalExtension, Row, params};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationOutboxItem {
    pub id: i64,
    pub account_id: i64,
    pub message_id: i64,
    pub thread_id: Option<i64>,
    pub sender_name: Option<String>,
    pub sender_addr: Option<String>,
    pub subject: String,
    pub state: String,
    pub attempts: i64,
    pub not_before: Option<i64>,
    pub created_at: i64,
    pub claimed_at: Option<i64>,
    pub delivered_at: Option<i64>,
    pub suppressed_at: Option<i64>,
    pub suppression_reason: Option<String>,
    pub last_error: Option<String>,
}

fn from_row(row: &Row) -> rusqlite::Result<NotificationOutboxItem> {
    Ok(NotificationOutboxItem {
        id: row.get("id")?,
        account_id: row.get("account_id")?,
        message_id: row.get("message_id")?,
        thread_id: row.get("thread_id")?,
        sender_name: row.get("sender_name")?,
        sender_addr: row.get("sender_addr")?,
        subject: row.get("subject")?,
        state: row.get("state")?,
        attempts: row.get("attempts")?,
        not_before: row.get("not_before")?,
        created_at: row.get("created_at")?,
        claimed_at: row.get("claimed_at")?,
        delivered_at: row.get("delivered_at")?,
        suppressed_at: row.get("suppressed_at")?,
        suppression_reason: row.get("suppression_reason")?,
        last_error: row.get("last_error")?,
    })
}

/// Enqueue one notification by snapshotting the message fields needed by the
/// native dispatcher. Repeated calls for the same message return the original
/// outbox id and never reset a terminal delivery state.
pub fn enqueue(conn: &Connection, message_id: i64) -> Result<i64> {
    enqueue_at(conn, message_id, now_ms())
}

pub fn enqueue_at(conn: &Connection, message_id: i64, created_at: i64) -> Result<i64> {
    conn.execute(
        "INSERT INTO notification_outbox (
             account_id, message_id, thread_id, sender_name, sender_addr, subject, created_at
         )
         SELECT account_id, id, thread_id, from_name, from_addr, subject, ?2
         FROM messages WHERE id = ?1
         ON CONFLICT(message_id) DO NOTHING",
        params![message_id, created_at],
    )?;

    conn.query_row(
        "SELECT id FROM notification_outbox WHERE message_id = ?1",
        params![message_id],
        |row| row.get(0),
    )
    .optional()?
    .ok_or_else(|| CoreError::NotFound(format!("message {message_id}")))
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
    let mut stmt = conn.prepare(
        "SELECT id, account_id, message_id, thread_id, sender_name, sender_addr,
                subject, state, attempts, not_before, created_at, claimed_at,
                delivered_at, suppressed_at, suppression_reason, last_error
         FROM notification_outbox WHERE id = ?1",
    )?;
    Ok(stmt.query_row(params![id], from_row).optional()?)
}

/// Pending notifications ready for dispatch, oldest first.
pub fn list_due(conn: &Connection, now: i64, limit: i64) -> Result<Vec<NotificationOutboxItem>> {
    let mut stmt = conn.prepare(
        "SELECT id, account_id, message_id, thread_id, sender_name, sender_addr,
                subject, state, attempts, not_before, created_at, claimed_at,
                delivered_at, suppressed_at, suppression_reason, last_error
         FROM notification_outbox
         WHERE state = 'pending' AND (not_before IS NULL OR not_before <= ?1)
         ORDER BY created_at, id LIMIT ?2",
    )?;
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

pub fn mark_delivered(conn: &Connection, id: i64, delivered_at: i64) -> Result<bool> {
    Ok(conn.execute(
        "UPDATE notification_outbox
         SET state = 'delivered', delivered_at = ?2, not_before = NULL,
             last_error = NULL
         WHERE id = ?1 AND state IN ('pending','delivering')",
        params![id, delivered_at],
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
}

impl NewMailNotification {
    fn new(item: &NotificationOutboxItem, thread_id: i64) -> Self {
        let sender = item
            .sender_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .or_else(|| item.sender_addr.clone())
            .unwrap_or_default();
        Self {
            account_id: item.account_id,
            message_id: item.message_id,
            thread_id,
            sender,
            subject: item.subject.clone(),
        }
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
                    claimed.push((item.id, NewMailNotification::new(item, thread_id)));
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

/// Whether a due notification goes out. Nothing does while notifications are
/// off, so turning them on later doesn't replay old mail. Mail stops counting
/// as new after the window sync uses to queue it, or once it is read or moved
/// to Trash or Spam. A thread whose AI route is still pending waits.
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
    if !unread_in_mailbox(conn, item.message_id)? {
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

/// Whether the message is still unread and outside Trash and Spam.
fn unread_in_mailbox(conn: &Connection, message_id: i64) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT m.is_read = 0 AND COALESCE(f.role, '') NOT IN (?2, ?3)
             FROM messages m LEFT JOIN folders f ON f.id = m.folder_id
             WHERE m.id = ?1",
            params![message_id, roles::TRASH, roles::SPAM],
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

        let id = enqueue_at(&c, message_id, 100).unwrap();
        assert_eq!(enqueue_at(&c, message_id, 200).unwrap(), id);
        let row = get(&c, id).unwrap().unwrap();
        assert_eq!(row.account_id, 1);
        assert_eq!(row.thread_id, Some(thread_id));
        assert_eq!(row.sender_name.as_deref(), Some("Alice"));
        assert_eq!(row.sender_addr.as_deref(), Some("alice@test.dev"));
        assert_eq!(row.subject, "Deployment");
        assert_eq!(row.created_at, 100);
        assert_eq!(list_due(&c, 100, 10).unwrap().len(), 1);
    }

    #[test]
    fn claim_retry_delivery_and_recovery_are_durable() {
        let c = testutil::conn();
        testutil::seed_account(&c);
        let (_, message_id) = testutil::seed_message(&c, "a@test.dev", "One", false);
        let id = enqueue_at(&c, message_id, 10).unwrap();

        assert!(try_claim(&c, id, 20).unwrap());
        assert!(!try_claim(&c, id, 20).unwrap());
        assert!(retry(&c, id, 50, "native service unavailable").unwrap());
        assert!(list_due(&c, 49, 10).unwrap().is_empty());
        assert_eq!(list_due(&c, 50, 10).unwrap()[0].attempts, 1);

        assert!(try_claim(&c, id, 50).unwrap());
        assert_eq!(recover_delivering(&c, 60).unwrap(), 1);
        assert!(try_claim(&c, id, 60).unwrap());
        assert!(mark_delivered(&c, id, 61).unwrap());
        let row = get(&c, id).unwrap().unwrap();
        assert_eq!(row.state, "delivered");
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
        let id = enqueue_at(&c, message_id, 10).unwrap();

        assert!(defer(&c, id, 500).unwrap());
        assert!(list_due(&c, 499, 10).unwrap().is_empty());
        assert_eq!(list_due(&c, 500, 10).unwrap()[0].attempts, 0);

        // A claimed (in-flight) row is never rewound by defer.
        assert!(try_claim(&c, id, 500).unwrap());
        assert!(!defer(&c, id, 900).unwrap());
    }

    #[test]
    fn suppression_is_terminal_and_message_delete_cascades() {
        let c = testutil::conn();
        c.pragma_update(None, "foreign_keys", "ON").unwrap();
        testutil::seed_account(&c);
        let (_, message_id) = testutil::seed_message(&c, "a@test.dev", "Two", false);
        let id = enqueue_at(&c, message_id, 10).unwrap();
        assert!(mark_suppressed(&c, id, 11, "window focused").unwrap());
        let row = get(&c, id).unwrap().unwrap();
        assert_eq!(row.state, "suppressed");
        assert_eq!(row.suppression_reason.as_deref(), Some("window focused"));
        assert!(!mark_delivered(&c, id, 12).unwrap());

        c.execute("DELETE FROM messages WHERE id = ?1", params![message_id])
            .unwrap();
        assert!(get(&c, id).unwrap().is_none());
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
            "UPDATE messages SET from_name = ' Alice ' WHERE id = ?1",
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
        let (_, bulk) = testutil::seed_message(&c, "news@test.dev", "Digest", true);
        let (routing_thread, routing) =
            testutil::seed_message(&c, "dave@test.dev", "Routing", false);
        c.execute(
            "UPDATE threads SET routed_tab = 'pending' WHERE id = ?1",
            params![routing_thread],
        )
        .unwrap();
        let (_, old) = testutil::seed_message(&c, "erin@test.dev", "Old", false);

        let [fresh_id, read_id, trashed_id, bulk_id, routing_id] =
            [fresh, read, trashed, bulk, routing]
                .map(|id| enqueue_at(&c, id, now - 1_000).unwrap());
        let old_id = enqueue_at(&c, old, now - CHIME_RECENT_MS).unwrap();

        let settled = settle_due(&c, &settings, now, 10).unwrap();
        assert_eq!(settled.read, 6);
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
                }
            )]
        );
        assert_eq!(get(&c, fresh_id).unwrap().unwrap().state, "delivering");
        assert_eq!(suppression(&c, read_id).as_deref(), Some("read or moved"));
        assert_eq!(
            suppression(&c, trashed_id).as_deref(),
            Some("read or moved")
        );
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
        assert_eq!(next_due_at(&c, now + ROUTE_WAIT_MS).unwrap(), None);
    }

    #[test]
    fn settle_suppresses_mail_while_notifications_are_off() {
        let c = testutil::conn();
        testutil::seed_account(&c);
        let (_, message_id) = testutil::seed_message(&c, "a@test.dev", "Quiet", false);
        let id = enqueue_at(&c, message_id, 1_000).unwrap();
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
}
