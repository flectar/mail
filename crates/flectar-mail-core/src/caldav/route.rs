//! Which connection pushes each pending local change. An account may sync
//! several servers, so every change is routed once, from one snapshot, to
//! the connection owning its calendar; two connections can never both
//! create the same new event.

use std::collections::HashMap;

use rusqlite::Connection;

use crate::db::repo;
use crate::db::repo::calendar::SyncRow;
use crate::error::Result;
use crate::models::Calendar;

/// A pending change and the calendar it goes to.
#[derive(Debug, Clone)]
pub struct PendingPush {
    pub row: SyncRow,
    /// The calendar the event lives in, or the one a new event goes to.
    pub calendar: Calendar,
}

/// The account's pending changes, sorted by destination.
#[derive(Debug, Default)]
pub struct Routes {
    /// Changes per connection id.
    pub pushes: HashMap<i64, Vec<PendingPush>>,
    /// Deleted events that never reached a server: nothing to tell anyone.
    pub discard: Vec<i64>,
}

/// Where one pending row goes. An event already on a server stays with its
/// calendar. A new one goes to the calendar chosen for it when that still
/// syncs and is writable, else to the account's default. None leaves the row
/// pending: the account has no writable calendar yet.
pub fn destination<'a>(
    row: &SyncRow,
    calendars: &'a [Calendar],
    default: Option<&'a Calendar>,
) -> Option<&'a Calendar> {
    let current = row
        .event
        .calendar_id
        .and_then(|id| calendars.iter().find(|calendar| calendar.id == id));
    if row.caldav_href.is_some() {
        return current;
    }
    current
        .filter(|calendar| calendar.enabled && !calendar.read_only)
        .or(default)
}

/// Snapshot and route the account's pending changes.
pub fn pending(conn: &Connection, account_id: i64) -> Result<Routes> {
    let rows = repo::calendar::dirty_rows(conn, account_id)?;
    if rows.is_empty() {
        return Ok(Routes::default());
    }
    let calendars = repo::caldav::list_calendars(conn, Some(account_id))?;
    let default = repo::caldav::default_calendar(conn, account_id)?;
    let mut routes = Routes::default();
    for row in rows {
        if row.deleted && (row.event.calendar_id.is_none() || row.caldav_href.is_none()) {
            routes.discard.push(row.event.id);
            continue;
        }
        if let Some(calendar) = destination(&row, &calendars, default.as_ref()) {
            routes
                .pushes
                .entry(calendar.connection_id)
                .or_default()
                .push(PendingPush {
                    calendar: calendar.clone(),
                    row,
                });
        }
    }
    Ok(routes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::repo::caldav::tests::add_connection;
    use crate::db::testutil;
    use crate::models::CalendarConnectionKind;
    use rusqlite::params;

    fn insert(
        c: &Connection,
        uid: &str,
        calendar_id: Option<i64>,
        href: Option<&str>,
        dirty: i64,
        deleted: i64,
    ) {
        c.execute(
            "INSERT INTO calendar_events
                (account_id, ical_uid, starts_at, calendar_id, caldav_href, dirty, deleted)
             VALUES (1, ?1, 1000, ?2, ?3, ?4, ?5)",
            params![uid, calendar_id, href, dirty, deleted],
        )
        .unwrap();
    }

    fn routed(routes: &Routes, connection_id: i64) -> Vec<(String, i64)> {
        let mut out: Vec<(String, i64)> = routes
            .pushes
            .get(&connection_id)
            .into_iter()
            .flatten()
            .map(|push| (push.row.ical_uid.clone(), push.calendar.id))
            .collect();
        out.sort();
        out
    }

    #[test]
    fn every_pending_change_goes_to_exactly_one_connection() {
        let c = testutil::calendar_conn();
        let own = add_connection(&c, 1, CalendarConnectionKind::Generic);
        let shared = add_connection(&c, 1, CalendarConnectionKind::Generic);
        let mine =
            repo::caldav::upsert_calendar(&c, &own, "https://a/me/", None, None, false).unwrap();
        let team = repo::caldav::upsert_calendar(&c, &shared, "https://b/team/", None, None, false)
            .unwrap();
        let holidays =
            repo::caldav::upsert_calendar(&c, &shared, "https://b/holidays/", None, None, true)
                .unwrap();
        repo::caldav::set_default_calendar(&c, 1, mine).unwrap();

        insert(&c, "edit-team", Some(team), Some("/team/e.ics"), 1, 0);
        insert(&c, "delete-team", Some(team), Some("/team/d.ics"), 0, 1);
        insert(&c, "new-in-team", Some(team), None, 1, 0);
        insert(&c, "new-unplaced", None, None, 1, 0);
        // A read-only choice falls back to the default calendar.
        insert(&c, "new-in-holidays", Some(holidays), None, 1, 0);
        insert(&c, "never-synced", Some(team), None, 0, 1);
        insert(&c, "clean", Some(mine), Some("/me/c.ics"), 0, 0);

        let routes = pending(&c, 1).unwrap();
        assert_eq!(
            routed(&routes, own.id),
            vec![
                ("new-in-holidays".into(), mine),
                ("new-unplaced".into(), mine)
            ]
        );
        assert_eq!(
            routed(&routes, shared.id),
            vec![
                ("delete-team".into(), team),
                ("edit-team".into(), team),
                ("new-in-team".into(), team),
            ]
        );
        assert_eq!(routes.discard.len(), 1);

        // Without a writable calendar a new event simply stays pending.
        repo::caldav::set_calendar_enabled(&c, mine, false).unwrap();
        repo::caldav::set_calendar_enabled(&c, team, false).unwrap();
        let routes = pending(&c, 1).unwrap();
        assert!(routed(&routes, own.id).is_empty());
        assert_eq!(
            routed(&routes, shared.id),
            vec![("delete-team".into(), team), ("edit-team".into(), team)]
        );
    }
}
