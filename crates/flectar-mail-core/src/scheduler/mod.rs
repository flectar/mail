//! Wakes snoozed threads, brings back conversations nobody replied to in
//! time, and nudges account actors when timed actions
//! (send-later, undo-send windows) come due. Timers are app-local: anything
//! past due fires on the next tick or at launch.

use crate::db::Db;
use crate::db::repo;
use crate::events::{CoreEvent, EventBus};
use crate::models::now_ms;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::caldav::task::CalTaskHandle;
use crate::sync::engine::{AccountHandle, SyncCmd};

const TICK_SECS: u64 = 5;
/// How often reply reminders are checked.
const REMINDER_CHECK_MS: i64 = 30_000;
/// A reply reminder whose message never arrived on this device is
/// forgotten this long after its time.
const REMINDER_STALE_MS: i64 = 30 * 24 * 60 * 60_000;

pub fn spawn(
    db: Db,
    calendar_db: Db,
    bus: EventBus,
    handles: Arc<RwLock<HashMap<i64, AccountHandle>>>,
    cal_handles: Arc<RwLock<HashMap<i64, CalTaskHandle>>>,
    has_existing_work: bool,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut nudged_until: i64 = 0;
        let mut reminders_checked_at: i64 = 0;
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(TICK_SECS)).await;
            // An empty welcome has nothing timed to dispatch. Waiting here
            // keeps the just-released SQLite reader cache closed. New mail
            // account actors and an opened calendar store activate the same
            // scheduler without creating another task.
            if !has_existing_work
                && !calendar_db.is_open()
                && handles.read().await.is_empty()
                && cal_handles.read().await.is_empty()
            {
                continue;
            }
            let now = now_ms();

            // 1. Wake snoozed threads, on every device: the shared snooze
            // goes too.
            let woken = db.read(move |conn| repo::snoozes::woken(conn, now)).await;
            if let Ok(woken) = woken {
                let mut shown = Vec::new();
                for (thread_id, wake_at) in woken {
                    let res = db
                        .write(move |conn| {
                            repo::snoozes::clear(conn, thread_id)?;
                            repo::snoozes::mark_woken(conn, thread_id, wake_at)
                        })
                        .await;
                    if res.is_ok() {
                        bus.emit(CoreEvent::ThreadWoke { thread_id });
                        shown.push(thread_id);
                    }
                }
                if !shown.is_empty() {
                    bus.emit(CoreEvent::MailUpdated { thread_ids: shown });
                }
            }
            let _ = db
                .write(move |conn| repo::snoozes::drop_expired(conn, now))
                .await;

            // 1b. Reply reminders: a conversation nobody replied to in time
            // comes back to the top of the inbox, and a reply ends its
            // reminder. Only reminders whose time has passed are looked at,
            // twice a minute.
            let reminders_due =
                now - reminders_checked_at >= REMINDER_CHECK_MS
                    && db
                        .read(move |conn| {
                            Ok(repo::reply_reminders::next_remind_at(conn)?
                                .is_some_and(|at| at <= now)
                                || repo::reply_reminders::any_due(conn)?)
                        })
                        .await
                        .unwrap_or(false);
            if reminders_due {
                reminders_checked_at = now;
            }
            if reminders_due
                && let Ok(checked) = db
                    .write(move |conn| {
                        let checked = repo::reply_reminders::check(conn, now)?;
                        repo::reply_reminders::drop_stale(conn, now - REMINDER_STALE_MS)?;
                        Ok(checked)
                    })
                    .await
            {
                let mut shown = checked.due;
                shown.extend(checked.replied);
                if !shown.is_empty() {
                    bus.emit(CoreEvent::MailUpdated { thread_ids: shown });
                }
            }

            // 2. Nudge actors when a timed pending action has come due
            // since the last nudge. Asking for anything due since then, not
            // for the oldest due action, keeps one action that cannot run,
            // such as on an offline account, from holding back later ones.
            let since = nudged_until;
            let mail_due = db
                .read(move |conn| repo::actions::mail_due_between(conn, since, now))
                .await
                .unwrap_or(false);
            let calendar_due = calendar_db.is_open()
                && calendar_db
                    .read(move |conn| repo::actions::due_between(conn, since, now))
                    .await
                    .unwrap_or(false);
            if mail_due || calendar_due {
                nudged_until = now;
                for handle in handles.read().await.values() {
                    handle.send(SyncCmd::RunActions);
                }
                for handle in cal_handles.read().await.values() {
                    handle.nudge();
                }
            }

            // 3. Meeting reminders: fire once per occurrence inside the lead
            // window. Recurring events are covered because list_events
            // expansion is not needed here - the notified_at gate compares
            // against each occurrence start via upcoming_for_notify + the
            // expander below for recurring masters.
            let lead_min = db
                .read(|conn| Ok(repo::settings::get(conn)?.meeting_notify_lead_minutes))
                .await
                .unwrap_or(0);
            if lead_min > 0 && calendar_db.is_open() {
                let lead_ms = lead_min * 60_000;
                if let Ok(due) = calendar_db
                    .read(move |conn| repo::calendar::upcoming_for_notify(conn, now, lead_ms))
                    .await
                {
                    for ev in due {
                        let event_id = ev.id;
                        let occurrence_start = ev.starts_at;
                        let _ = calendar_db
                            .write(move |conn| {
                                repo::calendar::set_notified(conn, event_id, occurrence_start)
                            })
                            .await;
                        bus.emit(CoreEvent::EventReminder {
                            event: Box::new(ev),
                            occurrence_start,
                        });
                    }
                }
                // Recurring masters: check the next occurrence explicitly.
                if let Ok(masters) = calendar_db
                    .read(move |conn| repo::calendar::recurring_masters(conn, now + lead_ms))
                    .await
                {
                    for m in masters {
                        // Same gates as upcoming_for_notify: cancelled series
                        // (incl. CANCEL tombstones) and declined invites are
                        // silent.
                        if m.event.status.as_deref() == Some("CANCELLED")
                            || m.event.rsvp_status.as_deref() == Some("DECLINED")
                        {
                            continue;
                        }
                        let Some(rrule) = m.event.rrule.clone() else {
                            continue;
                        };
                        let duration = m
                            .event
                            .ends_at
                            .map(|e| e - m.event.starts_at)
                            .unwrap_or(1_800_000);
                        let Some(occs) = crate::caldav::rrule::expand(
                            &rrule,
                            m.event.starts_at,
                            duration,
                            m.event.all_day,
                            m.ical_raw.as_deref(),
                            now,
                            now + lead_ms,
                        ) else {
                            continue;
                        };
                        let Some(next) = occs.first().copied() else {
                            continue;
                        };
                        // Gate: skip when this occurrence was already notified,
                        // and never re-notify the master's own start (handled
                        // by upcoming_for_notify above).
                        let already = calendar_db
                            .read({
                                let id = m.event.id;
                                move |conn| {
                                    Ok(conn.query_row(
                                        "SELECT COALESCE(notified_at, 0) FROM calendar_events WHERE id = ?1",
                                        rusqlite::params![id],
                                        |r| r.get::<_, i64>(0),
                                    )?)
                                }
                            })
                            .await
                            .unwrap_or(i64::MAX);
                        if already >= next.start || next.start <= m.event.starts_at {
                            continue;
                        }
                        let event_id = m.event.id;
                        let _ = calendar_db
                            .write(move |conn| {
                                repo::calendar::set_notified(conn, event_id, next.start)
                            })
                            .await;
                        let mut ev = m.event.clone();
                        ev.starts_at = next.start;
                        ev.ends_at = Some(next.end);
                        bus.emit(CoreEvent::EventReminder {
                            event: Box::new(ev),
                            occurrence_start: next.start,
                        });
                    }
                }
            }
        }
    })
}
