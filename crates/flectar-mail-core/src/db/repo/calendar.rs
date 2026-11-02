use crate::calendar::IcsEvent;
use crate::error::Result;
use crate::models::{CalendarEvent, EventAttendee, UpdateEventArgs};
use rusqlite::{Connection, OptionalExtension, Row, params};

use super::parse_json_column;

macro_rules! event_select {
    ($tail:literal) => {
        concat!(
            "SELECT id, account_id, message_id, summary, location, organizer, ",
            "description, attendees_json, join_url, rsvp_status, is_local, ",
            "calendar_id, rrule, starts_at, ends_at, all_day, status, method ",
            "FROM calendar_events ",
            $tail
        )
    };
}

macro_rules! sync_event_select {
    ($tail:literal) => {
        concat!(
            "SELECT id, account_id, message_id, summary, location, organizer, ",
            "description, attendees_json, join_url, rsvp_status, is_local, ",
            "calendar_id, rrule, starts_at, ends_at, all_day, status, method, ",
            "ical_uid, sequence, caldav_href, etag, ical_raw, deleted ",
            "FROM calendar_events ",
            $tail
        )
    };
}

fn from_row(row: &Row) -> rusqlite::Result<CalendarEvent> {
    let attendees_json: Option<String> = row.get("attendees_json")?;
    let attendees = match attendees_json.as_deref() {
        Some(json) => parse_json_column(json, 7)?,
        None => Vec::new(),
    };
    Ok(CalendarEvent {
        id: row.get("id")?,
        account_id: row.get("account_id")?,
        message_id: row.get("message_id")?,
        summary: row.get("summary")?,
        location: row.get("location")?,
        organizer: row.get("organizer")?,
        description: row.get("description")?,
        attendees,
        join_url: row.get("join_url")?,
        rsvp_status: row.get("rsvp_status")?,
        is_local: row.get::<_, i64>("is_local")? != 0,
        calendar_id: row.get("calendar_id")?,
        rrule: row.get("rrule")?,
        starts_at: row.get("starts_at")?,
        ends_at: row.get("ends_at")?,
        all_day: row.get::<_, i64>("all_day")? != 0,
        status: row.get("status")?,
        method: row.get("method")?,
    })
}

/// Everything the CalDAV push path needs about one dirty/tombstoned row.
#[derive(Debug, Clone)]
pub struct SyncRow {
    pub event: CalendarEvent,
    pub ical_uid: String,
    pub sequence: i64,
    pub caldav_href: Option<String>,
    pub etag: Option<String>,
    pub ical_raw: Option<String>,
    pub deleted: bool,
}

fn sync_row(row: &Row) -> rusqlite::Result<SyncRow> {
    Ok(SyncRow {
        event: from_row(row)?,
        ical_uid: row.get("ical_uid")?,
        sequence: row.get("sequence")?,
        caldav_href: row.get("caldav_href")?,
        etag: row.get("etag")?,
        ical_raw: row.get("ical_raw")?,
        deleted: row.get::<_, i64>("deleted")? != 0,
    })
}

fn attendees_json(ev: &IcsEvent) -> Result<Option<String>> {
    if ev.attendees.is_empty() {
        return Ok(None);
    }
    let list: Vec<EventAttendee> = ev
        .attendees
        .iter()
        .map(|a| EventAttendee {
            email: a.email.clone(),
            name: a.name.clone(),
            partstat: a.partstat.clone(),
        })
        .collect();
    Ok(Some(serde_json::to_string(&list)?))
}

fn attendees_to_json(list: &[EventAttendee]) -> Result<Option<String>> {
    if list.is_empty() {
        return Ok(None);
    }
    Ok(Some(serde_json::to_string(list)?))
}

/// Pick the one row an email invitation is allowed to update. Prefer the row
/// already linked to mail, then a detached row, then a stable provider copy.
/// A UID may exist in multiple provider calendars, so callers must never
/// update every matching row.
fn preferred_mail_row(conn: &Connection, account_id: i64, uid: &str) -> Result<Option<i64>> {
    conn.query_row(
        "SELECT id FROM calendar_events
         WHERE account_id = ?1 AND ical_uid = ?2
         ORDER BY (message_id IS NOT NULL) DESC,
                  (calendar_id IS NULL) DESC, id
         LIMIT 1",
        params![account_id, uid],
        |row| row.get(0),
    )
    .optional()
    .map_err(Into::into)
}

/// Upsert by iCal UID (mail-invite path). CANCEL marks the stored event
/// cancelled - or stores a cancelled tombstone row when the UID is unknown,
/// because archive backfill and body re-decode parse messages out of send
/// order and the matching REQUEST may only arrive later. REQUEST replaces the
/// details only when its SEQUENCE is not older than the stored one, and never
/// revives a CANCELLED event without a strictly newer SEQUENCE. Our own RSVP
/// state, the is_local flag, and CalDAV sync bookkeeping survive updates.
pub fn upsert(conn: &Connection, account_id: i64, message_id: i64, ev: &IcsEvent) -> Result<()> {
    if ev.method.as_deref() == Some("CANCEL") {
        if let Some(id) = preferred_mail_row(conn, account_id, &ev.uid)? {
            // A stale cancellation must not override a newer REQUEST.
            conn.execute(
                "UPDATE calendar_events
                 SET status = 'CANCELLED', method = 'CANCEL', sequence = ?2
                 WHERE id = ?1 AND ?2 >= sequence",
                params![id, ev.sequence],
            )?;
        } else {
            conn.execute(
                "INSERT INTO calendar_events
                    (account_id, message_id, ical_uid, method, summary, location, organizer,
                     description, attendees_json, join_url, sequence, rrule, tzid,
                     starts_at, ends_at, all_day, status)
                 VALUES (?1,?2,?3,'CANCEL',?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,'CANCELLED')",
                params![
                    account_id,
                    message_id,
                    ev.uid,
                    ev.summary,
                    ev.location,
                    ev.organizer,
                    ev.description,
                    attendees_json(ev)?,
                    ev.join_url,
                    ev.sequence,
                    ev.rrule,
                    ev.tzid,
                    ev.starts_at_ms,
                    ev.ends_at_ms,
                    ev.all_day as i64,
                ],
            )?;
        }
        return Ok(());
    }
    let existing = preferred_mail_row(conn, account_id, &ev.uid)?;
    let attendee_json = attendees_json(ev)?;
    if let Some(id) = existing {
        conn.execute(
            "UPDATE calendar_events SET
                message_id = ?2, method = ?3, summary = ?4, location = ?5,
                organizer = ?6, description = ?7, attendees_json = ?8,
                join_url = ?9, sequence = ?10, rrule = ?11, tzid = ?12,
                starts_at = ?13, ends_at = ?14, all_day = ?15, status = ?16
             WHERE id = ?1 AND dirty = 0
               AND (?10 > sequence
                    OR (?10 = sequence AND COALESCE(status, '') != 'CANCELLED'))",
            params![
                id,
                message_id,
                ev.method,
                ev.summary,
                ev.location,
                ev.organizer,
                ev.description,
                attendee_json,
                ev.join_url,
                ev.sequence,
                ev.rrule,
                ev.tzid,
                ev.starts_at_ms,
                ev.ends_at_ms,
                ev.all_day as i64,
                ev.status,
            ],
        )?;
    } else {
        conn.execute(
            "INSERT INTO calendar_events
                (account_id, message_id, ical_uid, method, summary, location, organizer,
                 description, attendees_json, join_url, sequence, rrule, tzid,
                 starts_at, ends_at, all_day, status)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
            params![
                account_id,
                message_id,
                ev.uid,
                ev.method,
                ev.summary,
                ev.location,
                ev.organizer,
                ev.description,
                attendee_json,
                ev.join_url,
                ev.sequence,
                ev.rrule,
                ev.tzid,
                ev.starts_at_ms,
                ev.ends_at_ms,
                ev.all_day as i64,
                ev.status,
            ],
        )?;
    }
    Ok(())
}

/// Upsert an event pulled from a CalDAV collection, keyed by (calendar_id,
/// href), then by UID within that same collection (resource renames), with an
/// unfiled UID fallback that adopts rows which arrived first as mail invites.
/// A same-UID row in a different provider calendar is deliberately distinct.
/// Rows with local unsynced edits (dirty/deleted) are never clobbered - the
/// push path resolves them via If-Match.
///
/// Returns `true` when a brand-new row was inserted (not an update of an
/// existing event), so the pull can surface genuinely-new events to the user.
#[allow(clippy::too_many_arguments)]
pub fn upsert_remote(
    conn: &Connection,
    account_id: i64,
    calendar_id: i64,
    href: &str,
    etag: &str,
    ical_raw: &str,
    ev: &IcsEvent,
) -> Result<bool> {
    let existing: Option<(i64, i64, i64, Option<i64>)> = conn
        .query_row(
            "SELECT id, dirty, deleted, calendar_id FROM calendar_events
             WHERE (calendar_id = ?1 AND caldav_href = ?2)
                OR (account_id = ?3 AND ical_uid = ?4
                    AND (calendar_id = ?1 OR calendar_id IS NULL))
             ORDER BY CASE
                        WHEN calendar_id = ?1 AND caldav_href = ?2 THEN 0
                        WHEN calendar_id = ?1 THEN 1
                        WHEN message_id IS NOT NULL THEN 2
                        ELSE 3
                      END,
                      id
             LIMIT 1",
            params![calendar_id, href, account_id, ev.uid],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;

    match existing {
        Some((_, dirty, _, _)) if dirty != 0 => Ok(false),
        // Tombstone awaiting our own server-side DELETE - the push resolves it.
        Some((_, _, deleted, Some(_))) if deleted != 0 => Ok(false),
        // A deleted row with no collection is a terminal mail-invite
        // tombstone; the event demonstrably exists on the server, so the
        // server wins and the update below revives it (deleted = 0).
        Some((id, _, _, _)) => {
            conn.execute(
                "UPDATE calendar_events SET
                    deleted = 0,
                    calendar_id = ?2, caldav_href = ?3, etag = ?4, ical_raw = ?5,
                    ical_uid = ?6, summary = ?7, location = ?8, organizer = ?9,
                    description = ?10, attendees_json = ?11, join_url = ?12,
                    sequence = ?13, rrule = ?14, tzid = ?15, starts_at = ?16,
                    ends_at = ?17, all_day = ?18, status = ?19
                 WHERE id = ?1",
                params![
                    id,
                    calendar_id,
                    href,
                    etag,
                    ical_raw,
                    ev.uid,
                    ev.summary,
                    ev.location,
                    ev.organizer,
                    ev.description,
                    attendees_json(ev)?,
                    ev.join_url,
                    ev.sequence,
                    ev.rrule,
                    ev.tzid,
                    ev.starts_at_ms,
                    ev.ends_at_ms,
                    ev.all_day as i64,
                    ev.status,
                ],
            )?;
            Ok(false)
        }
        None => {
            conn.execute(
                "INSERT INTO calendar_events
                    (account_id, calendar_id, caldav_href, etag, ical_raw, ical_uid,
                     summary, location, organizer, description, attendees_json,
                     join_url, sequence, rrule, tzid, starts_at, ends_at, all_day, status)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19)",
                params![
                    account_id,
                    calendar_id,
                    href,
                    etag,
                    ical_raw,
                    ev.uid,
                    ev.summary,
                    ev.location,
                    ev.organizer,
                    ev.description,
                    attendees_json(ev)?,
                    ev.join_url,
                    ev.sequence,
                    ev.rrule,
                    ev.tzid,
                    ev.starts_at_ms,
                    ev.ends_at_ms,
                    ev.all_day as i64,
                    ev.status,
                ],
            )?;
            Ok(true)
        }
    }
}

/// Insert an event created in Flectar Mail (we are the organizer).
#[allow(clippy::too_many_arguments)]
pub fn insert_local(
    conn: &Connection,
    account_id: i64,
    calendar_id: Option<i64>,
    uid: &str,
    summary: &str,
    location: Option<&str>,
    description: Option<&str>,
    join_url: Option<&str>,
    organizer: &str,
    attendees: &[EventAttendee],
    starts_at: i64,
    ends_at: i64,
    all_day: bool,
) -> Result<i64> {
    let attendees_json = attendees_to_json(attendees)?;
    conn.execute(
        "INSERT INTO calendar_events
            (account_id, calendar_id, ical_uid, method, summary, location, organizer,
             description, attendees_json, join_url, is_local,
             starts_at, ends_at, all_day, status)
         VALUES (?1,?2,?3,'REQUEST',?4,?5,?6,?7,?8,?9,1,?10,?11,?12,'CONFIRMED')",
        params![
            account_id,
            calendar_id,
            uid,
            summary,
            location,
            organizer,
            description,
            attendees_json,
            join_url,
            starts_at,
            ends_at,
            all_day as i64,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Apply a local edit: replace the user-editable fields, bump SEQUENCE, and
/// flag the row dirty so the CalDAV push picks it up.
pub fn update_local_fields(
    conn: &Connection,
    args: &UpdateEventArgs,
    attendees: &[EventAttendee],
) -> Result<()> {
    conn.execute(
        "UPDATE calendar_events SET
            summary = ?2, description = ?3, location = ?4, join_url = ?5,
            starts_at = ?6, ends_at = ?7, all_day = ?8, attendees_json = ?9,
            sequence = sequence + 1, dirty = 1
         WHERE id = ?1",
        params![
            args.event_id,
            args.summary,
            args.description,
            args.location,
            args.join_url,
            args.starts_at,
            args.ends_at,
            args.all_day as i64,
            attendees_to_json(attendees)?,
        ],
    )?;
    Ok(())
}

/// Flag our own RSVP change for the CalDAV push (PARTSTAT PUT).
pub fn mark_dirty(conn: &Connection, event_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE calendar_events SET dirty = 1 WHERE id = ?1",
        params![event_id],
    )?;
    Ok(())
}

/// Tombstone: hidden from every listing, deleted on the server at next push.
pub fn mark_deleted(conn: &Connection, event_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE calendar_events SET deleted = 1, status = 'CANCELLED' WHERE id = ?1",
        params![event_id],
    )?;
    Ok(())
}

pub fn hard_delete(conn: &Connection, event_id: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM calendar_events WHERE id = ?1",
        params![event_id],
    )?;
    Ok(())
}

/// After a successful PUT: record the server's identity for the row.
pub fn clear_dirty_set_etag(
    conn: &Connection,
    event_id: i64,
    calendar_id: i64,
    href: &str,
    etag: Option<&str>,
    ical_raw: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE calendar_events SET dirty = 0, calendar_id = ?2, caldav_href = ?3,
                etag = COALESCE(?4, etag), ical_raw = ?5
         WHERE id = ?1",
        params![event_id, calendar_id, href, etag, ical_raw],
    )?;
    Ok(())
}

/// Rows with unsynced local changes (edits and tombstones) for one account.
/// Deleted mail-invite rows that no CalDAV collection ever adopted are
/// terminal tombstones with nothing to delete remotely - the push paths must
/// not see them, or they would purge the row and let the next archive
/// re-parse of the invitation email resurrect the event.
pub fn dirty_rows(conn: &Connection, account_id: i64) -> Result<Vec<SyncRow>> {
    let mut stmt = conn.prepare(sync_event_select!(
        "WHERE account_id = ?1 AND (dirty = 1 OR deleted = 1)
           AND NOT (deleted = 1 AND calendar_id IS NULL AND message_id IS NOT NULL)"
    ))?;
    let rows = stmt
        .query_map(params![account_id], sync_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn sync_row_for(conn: &Connection, event_id: i64) -> Result<Option<SyncRow>> {
    let mut stmt = conn.prepare(sync_event_select!("WHERE id = ?1"))?;
    let mut rows = stmt.query_map(params![event_id], sync_row)?;
    Ok(rows.next().transpose()?)
}

/// (id, href, etag) for every synced event of one collection - the pull pass
/// diffs this against the server listing to find remote deletions/changes.
pub fn hrefs_for_calendar(
    conn: &Connection,
    calendar_id: i64,
) -> Result<Vec<(i64, String, Option<String>)>> {
    let mut stmt = conn.prepare(
        "SELECT id, caldav_href, etag FROM calendar_events
         WHERE calendar_id = ?1 AND caldav_href IS NOT NULL",
    )?;
    let rows = stmt
        .query_map(params![calendar_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Remove an event the server no longer has (unless locally dirty). Rows that
/// also exist as a mail invite become terminal tombstones instead: purging
/// them would let the next re-parse of the archived invitation email (fresh
/// index, body re-decode) resurrect an event that was deleted everywhere.
///
/// `id` is matched against either `caldav_href` (the normal case - the
/// server dropped exactly this resource/instance) or `series_master_id`
/// (Microsoft Graph: deleting a whole recurring series sends one `@removed`
/// item carrying the series master's id, which was never stored as its own
/// row - see `graphcal.rs` - so it must cascade to every occurrence row that
/// recorded it as their master).
pub fn delete_by_href(conn: &Connection, calendar_id: i64, href: &str) -> Result<()> {
    conn.execute(
        "UPDATE calendar_events
         SET deleted = 1, status = 'CANCELLED',
             calendar_id = NULL, caldav_href = NULL, etag = NULL
         WHERE calendar_id = ?1 AND (caldav_href = ?2 OR series_master_id = ?2)
           AND dirty = 0 AND deleted = 0 AND message_id IS NOT NULL",
        params![calendar_id, href],
    )?;
    conn.execute(
        "DELETE FROM calendar_events
         WHERE calendar_id = ?1 AND (caldav_href = ?2 OR series_master_id = ?2)
           AND dirty = 0 AND deleted = 0",
        params![calendar_id, href],
    )?;
    Ok(())
}

/// Record which (unstored) series master an occurrence/exception row belongs
/// to, so a later series-wide deletion can find it via `delete_by_href`.
pub fn set_series_master(
    conn: &Connection,
    calendar_id: i64,
    href: &str,
    series_master_id: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE calendar_events SET series_master_id = ?3
         WHERE calendar_id = ?1 AND caldav_href = ?2",
        params![calendar_id, href, series_master_id],
    )?;
    Ok(())
}

/// Record our RSVP (ACCEPTED | TENTATIVE | DECLINED).
pub fn set_rsvp(conn: &Connection, event_id: i64, status: &str) -> Result<()> {
    conn.execute(
        "UPDATE calendar_events SET rsvp_status = ?2 WHERE id = ?1",
        params![event_id, status],
    )?;
    Ok(())
}

pub fn get(conn: &Connection, event_id: i64) -> Result<Option<CalendarEvent>> {
    let mut stmt = conn.prepare(event_select!("WHERE id = ?1"))?;
    let mut rows = stmt.query_map(params![event_id], from_row)?;
    Ok(rows.next().transpose()?)
}

/// The stored iCal UID + sequence (needed to build an RSVP reply).
pub fn uid_and_sequence(conn: &Connection, event_id: i64) -> Result<Option<(String, i64)>> {
    let mut stmt = conn.prepare("SELECT ical_uid, sequence FROM calendar_events WHERE id = ?1")?;
    let mut rows = stmt.query_map(params![event_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.next().transpose()?)
}

pub fn list_range(conn: &Connection, start_ms: i64, end_ms: i64) -> Result<Vec<CalendarEvent>> {
    let mut stmt = conn.prepare(event_select!(
        "WHERE deleted = 0 AND starts_at < ?2 AND COALESCE(ends_at, starts_at) >= ?1
           AND (calendar_id IS NULL OR EXISTS (
                SELECT 1 FROM calendars c WHERE c.id = calendar_events.calendar_id AND c.enabled = 1
           ))
         ORDER BY starts_at ASC, id ASC"
    ))?;
    let rows = stmt
        .query_map(params![start_ms, end_ms], from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Recurring masters that could produce occurrences in [start, end): started
/// before the window ends and still repeat (the expander applies UNTIL/COUNT).
pub fn recurring_masters(conn: &Connection, end_ms: i64) -> Result<Vec<SyncRow>> {
    let mut stmt = conn.prepare(sync_event_select!(
        "WHERE deleted = 0 AND rrule IS NOT NULL AND starts_at < ?1
           AND (calendar_id IS NULL OR EXISTS (
                SELECT 1 FROM calendars c WHERE c.id = calendar_events.calendar_id AND c.enabled = 1
           ))
         ORDER BY starts_at ASC, id ASC"
    ))?;
    let rows = stmt
        .query_map(params![end_ms], sync_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Events attached to one message (for the invite card in the thread view).
pub fn for_message(conn: &Connection, message_id: i64) -> Result<Vec<CalendarEvent>> {
    let mut stmt = conn.prepare(event_select!(
        "WHERE message_id = ?1 AND deleted = 0 ORDER BY starts_at"
    ))?;
    let rows = stmt
        .query_map(params![message_id], from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Non-recurring events starting within the reminder window that have not
/// been notified for this start time yet. (Recurring occurrences are handled
/// by the caller via the expander; the notified_at gate is per-occurrence.)
pub fn upcoming_for_notify(
    conn: &Connection,
    now: i64,
    lead_ms: i64,
) -> Result<Vec<CalendarEvent>> {
    let mut stmt = conn.prepare(event_select!(
        "WHERE deleted = 0 AND all_day = 0
           AND COALESCE(status,'') != 'CANCELLED'
           AND COALESCE(rsvp_status,'') != 'DECLINED'
           AND (calendar_id IS NULL OR EXISTS (
                SELECT 1 FROM calendars c WHERE c.id = calendar_events.calendar_id AND c.enabled = 1
           ))
           AND starts_at > ?1 AND starts_at <= ?1 + ?2
           AND (notified_at IS NULL OR notified_at < starts_at)
         ORDER BY starts_at LIMIT 20"
    ))?;
    let rows = stmt
        .query_map(params![now, lead_ms], from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn set_notified(conn: &Connection, event_id: i64, occurrence_start: i64) -> Result<()> {
    conn.execute(
        "UPDATE calendar_events SET notified_at = ?2 WHERE id = ?1",
        params![event_id, occurrence_start],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calendar::IcsAttendee;
    use crate::db::testutil;
    use crate::models::Address;

    fn sample_ics_event() -> IcsEvent {
        IcsEvent {
            uid: "evt-1@test".into(),
            method: Some("REQUEST".into()),
            summary: Some("Sync".into()),
            organizer: Some("alice@test.dev".into()),
            description: Some("agenda".into()),
            attendees: vec![IcsAttendee {
                email: "me@test.dev".into(),
                name: Some("Me".into()),
                partstat: Some("NEEDS-ACTION".into()),
            }],
            join_url: Some("https://meet.google.com/x".into()),
            sequence: 1,
            starts_at_ms: 1_000_000,
            ends_at_ms: Some(2_000_000),
            ..Default::default()
        }
    }

    fn seed_calendar_at(c: &Connection, url: &str, name: &str) -> i64 {
        c.execute(
            "INSERT OR IGNORE INTO calendar_connections (id, account_id, kind, base_url)
             VALUES (1, 1, 'generic', 'https://cal/')",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO calendars (account_id, connection_id, url, display_name)
             VALUES (1, 1, ?1, ?2)",
            params![url, name],
        )
        .unwrap();
        c.last_insert_rowid()
    }

    fn seed_calendar(c: &Connection) -> i64 {
        seed_calendar_at(c, "https://cal/x/", "Main")
    }

    #[test]
    fn upsert_roundtrips_details_and_preserves_rsvp() {
        let c = testutil::calendar_conn();
        let msg = 41;

        upsert(&c, 1, msg, &sample_ics_event()).unwrap();
        let ev = &for_message(&c, msg).unwrap()[0];
        assert_eq!(ev.description.as_deref(), Some("agenda"));
        assert_eq!(ev.join_url.as_deref(), Some("https://meet.google.com/x"));
        assert_eq!(ev.attendees.len(), 1);
        assert_eq!(ev.attendees[0].email, "me@test.dev");
        assert!(!ev.is_local);

        // RSVP survives a later invite update.
        set_rsvp(&c, ev.id, "ACCEPTED").unwrap();
        let mut updated = sample_ics_event();
        updated.summary = Some("Sync v2".into());
        upsert(&c, 1, msg, &updated).unwrap();
        let ev2 = get(&c, ev.id).unwrap().unwrap();
        assert_eq!(ev2.summary.as_deref(), Some("Sync v2"));
        assert_eq!(ev2.rsvp_status.as_deref(), Some("ACCEPTED"));
        assert_eq!(uid_and_sequence(&c, ev.id).unwrap().unwrap().1, 1);
    }

    #[test]
    fn insert_local_lists_in_range() {
        let c = testutil::calendar_conn();
        let id = insert_local(
            &c,
            1,
            None,
            "local-1@flectar-mail",
            "Planning",
            Some("Room 9"),
            None,
            None,
            "me@test.dev",
            &[],
            5_000,
            6_000,
            false,
        )
        .unwrap();
        let ev = get(&c, id).unwrap().unwrap();
        assert!(ev.is_local);
        assert_eq!(ev.organizer.as_deref(), Some("me@test.dev"));
        assert_eq!(list_range(&c, 0, 10_000).unwrap().len(), 1);
        assert_eq!(list_range(&c, 7_000, 10_000).unwrap().len(), 0);
    }

    #[test]
    fn disabled_provider_calendar_hides_events_and_reminders() {
        let c = testutil::calendar_conn();
        let cal = seed_calendar(&c);
        insert_local(
            &c,
            1,
            Some(cal),
            "hidden-1@flectar-mail",
            "Hidden event",
            None,
            None,
            None,
            "me@test.dev",
            &[],
            5_000,
            6_000,
            false,
        )
        .unwrap();
        assert_eq!(list_range(&c, 0, 10_000).unwrap().len(), 1);
        assert_eq!(upcoming_for_notify(&c, 1_000, 5_000).unwrap().len(), 1);

        c.execute("UPDATE calendars SET enabled = 0 WHERE id = ?1", [cal])
            .unwrap();
        assert!(list_range(&c, 0, 10_000).unwrap().is_empty());
        assert!(upcoming_for_notify(&c, 1_000, 5_000).unwrap().is_empty());
    }

    #[test]
    fn busy_calendar_range_and_recurring_masters_are_not_truncated() {
        let c = testutil::calendar_conn();
        for id in 0..650 {
            c.execute(
                "INSERT INTO calendar_events (account_id, ical_uid, starts_at, ends_at, rrule)
                 VALUES (0, ?1, ?2, ?3, 'FREQ=DAILY')",
                params![
                    format!("busy-{id}@calendar.example"),
                    id * 1000,
                    id * 1000 + 500
                ],
            )
            .unwrap();
        }
        assert_eq!(list_range(&c, 0, 1_000_000).unwrap().len(), 650);
        assert_eq!(recurring_masters(&c, 1_000_000).unwrap().len(), 650);
        assert_eq!(list_range(&c, 600_000, 610_000).unwrap().len(), 10);
    }

    #[test]
    fn dirty_and_tombstone_lifecycle() {
        let c = testutil::calendar_conn();
        let cal = seed_calendar(&c);
        let id = insert_local(
            &c,
            1,
            Some(cal),
            "u1@flectar-mail",
            "Edit me",
            None,
            None,
            None,
            "me@test.dev",
            &[],
            5_000,
            6_000,
            false,
        )
        .unwrap();
        assert_eq!(get(&c, id).unwrap().unwrap().calendar_id, Some(cal));

        // Local edit bumps sequence and flags dirty.
        let args = UpdateEventArgs {
            event_id: id,
            summary: "Edited".into(),
            description: None,
            location: Some("Room 1".into()),
            join_url: None,
            starts_at: 7_000,
            ends_at: 8_000,
            all_day: false,
            attendees: vec![Address {
                name: None,
                email: "bob@test.dev".into(),
            }],
            notify: true,
        };
        let atts = vec![EventAttendee {
            email: "bob@test.dev".into(),
            name: None,
            partstat: Some("NEEDS-ACTION".into()),
        }];
        update_local_fields(&c, &args, &atts).unwrap();
        let rows = dirty_rows(&c, 1).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].sequence, 1);
        assert_eq!(rows[0].event.summary.as_deref(), Some("Edited"));

        // Remote pull must not clobber the dirty row.
        let mut remote = sample_ics_event();
        remote.uid = "u1@flectar-mail".into();
        remote.summary = Some("Server version".into());
        upsert_remote(
            &c,
            1,
            cal,
            "/cal/u1.ics",
            "\"e1\"",
            "BEGIN:VCALENDAR",
            &remote,
        )
        .unwrap();
        assert_eq!(
            get(&c, id).unwrap().unwrap().summary.as_deref(),
            Some("Edited")
        );

        // Successful push clears dirty and records server identity.
        clear_dirty_set_etag(&c, id, cal, "/cal/u1.ics", Some("\"e2\""), "ICSDATA").unwrap();
        assert!(dirty_rows(&c, 1).unwrap().is_empty());
        let row = sync_row_for(&c, id).unwrap().unwrap();
        assert_eq!(row.etag.as_deref(), Some("\"e2\""));
        assert_eq!(row.caldav_href.as_deref(), Some("/cal/u1.ics"));

        // Now the same remote upsert applies (row is clean).
        upsert_remote(&c, 1, cal, "/cal/u1.ics", "\"e3\"", "ICS2", &remote).unwrap();
        assert_eq!(
            get(&c, id).unwrap().unwrap().summary.as_deref(),
            Some("Server version")
        );

        // Tombstone hides from listings, shows in dirty_rows, hard delete ends it.
        mark_deleted(&c, id).unwrap();
        assert!(list_range(&c, 0, 100_000).unwrap().is_empty());
        assert_eq!(dirty_rows(&c, 1).unwrap().len(), 1);
        hard_delete(&c, id).unwrap();
        assert!(dirty_rows(&c, 1).unwrap().is_empty());
    }

    #[test]
    fn cancel_arriving_first_blocks_late_request() {
        let c = testutil::calendar_conn();
        let msg = 42;

        // Backfill parses the CANCEL before the REQUEST it belongs to.
        let mut cancel = sample_ics_event();
        cancel.method = Some("CANCEL".into());
        cancel.sequence = 2;
        upsert(&c, 1, msg, &cancel).unwrap();
        let id = for_message(&c, msg).unwrap()[0].id;
        assert_eq!(
            get(&c, id).unwrap().unwrap().status.as_deref(),
            Some("CANCELLED")
        );

        // Original invite (older sequence) must not resurrect the event.
        upsert(&c, 1, msg, &sample_ics_event()).unwrap();
        assert_eq!(
            get(&c, id).unwrap().unwrap().status.as_deref(),
            Some("CANCELLED")
        );

        // Same sequence as the cancel: still cancelled.
        let mut same = sample_ics_event();
        same.sequence = 2;
        upsert(&c, 1, msg, &same).unwrap();
        assert_eq!(
            get(&c, id).unwrap().unwrap().status.as_deref(),
            Some("CANCELLED")
        );

        // A genuinely newer invite revives it.
        let mut newer = sample_ics_event();
        newer.sequence = 3;
        newer.status = Some("CONFIRMED".into());
        upsert(&c, 1, msg, &newer).unwrap();
        let ev = get(&c, id).unwrap().unwrap();
        assert_eq!(ev.status.as_deref(), Some("CONFIRMED"));

        // A delayed older cancellation cannot undo the newer REQUEST.
        let mut stale_cancel = sample_ics_event();
        stale_cancel.method = Some("CANCEL".into());
        stale_cancel.sequence = 2;
        upsert(&c, 1, msg, &stale_cancel).unwrap();
        assert_eq!(
            get(&c, id).unwrap().unwrap().status.as_deref(),
            Some("CONFIRMED")
        );

        // Still a single row throughout.
        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM calendar_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn stale_request_never_downgrades_newer_state() {
        let c = testutil::calendar_conn();
        let msg = 43;

        let mut v5 = sample_ics_event();
        v5.sequence = 5;
        v5.summary = Some("Moved to Friday".into());
        upsert(&c, 1, msg, &v5).unwrap();
        let id = for_message(&c, msg).unwrap()[0].id;

        // The original invite email re-parsed later (body re-decode).
        upsert(&c, 1, msg, &sample_ics_event()).unwrap();
        let ev = get(&c, id).unwrap().unwrap();
        assert_eq!(ev.summary.as_deref(), Some("Moved to Friday"));
        assert_eq!(uid_and_sequence(&c, id).unwrap().unwrap().1, 5);
    }

    #[test]
    fn deleted_mail_invite_stays_deleted_across_reparse() {
        let c = testutil::calendar_conn();
        let msg = 44;

        upsert(&c, 1, msg, &sample_ics_event()).unwrap();
        let id = for_message(&c, msg).unwrap()[0].id;
        mark_deleted(&c, id).unwrap();

        // Terminal tombstone: hidden from the push (which would purge it)…
        assert!(dirty_rows(&c, 1).unwrap().is_empty());
        // …and a re-parse of the archived invite does not resurrect it.
        upsert(&c, 1, msg, &sample_ics_event()).unwrap();
        assert!(list_range(&c, 0, i64::MAX / 2).unwrap().is_empty());
        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM calendar_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn delete_by_href_cascades_to_occurrences_via_series_master() {
        // Microsoft Graph expands a recurring series into per-occurrence rows
        // (see graphcal.rs) and never stores the series master itself. When
        // the whole series is deleted, Graph's delta sends a single @removed
        // item for the master's id - which must cascade to every occurrence
        // that recorded it as their series_master_id.
        let c = testutil::calendar_conn();
        let cal = seed_calendar(&c);

        let mut day = sample_ics_event();
        day.uid = "occ-1@graph".into();
        upsert_remote(&c, 1, cal, "occ-1", "\"e1\"", "", &day).unwrap();
        let mut day2 = sample_ics_event();
        day2.uid = "occ-2@graph".into();
        upsert_remote(&c, 1, cal, "occ-2", "\"e1\"", "", &day2).unwrap();
        set_series_master(&c, cal, "occ-1", "series-master-1").unwrap();
        set_series_master(&c, cal, "occ-2", "series-master-1").unwrap();

        assert_eq!(list_range(&c, 0, i64::MAX / 2).unwrap().len(), 2);

        // The removed delta item carries the series master's id, not either
        // occurrence's own id.
        delete_by_href(&c, cal, "series-master-1").unwrap();

        assert!(list_range(&c, 0, i64::MAX / 2).unwrap().is_empty());
    }

    #[test]
    fn server_deletion_tombstones_mail_backed_row() {
        let c = testutil::calendar_conn();
        let msg = 45;
        let cal = seed_calendar(&c);

        // Invite email lands first, then the CalDAV pull adopts the row.
        upsert(&c, 1, msg, &sample_ics_event()).unwrap();
        upsert_remote(
            &c,
            1,
            cal,
            "/cal/evt-1.ics",
            "\"e1\"",
            "ICS",
            &sample_ics_event(),
        )
        .unwrap();
        let id = for_message(&c, msg).unwrap()[0].id;

        // Event deleted on the server: keep a terminal tombstone.
        delete_by_href(&c, cal, "/cal/evt-1.ics").unwrap();
        let row = sync_row_for(&c, id).unwrap().unwrap();
        assert!(row.deleted);
        assert_eq!(row.event.calendar_id, None);
        assert!(list_range(&c, 0, i64::MAX / 2).unwrap().is_empty());

        // Re-parsing the archived invite does not bring it back.
        upsert(&c, 1, msg, &sample_ics_event()).unwrap();
        assert!(list_range(&c, 0, i64::MAX / 2).unwrap().is_empty());

        // But if the server has the event again, the server wins.
        upsert_remote(
            &c,
            1,
            cal,
            "/cal/evt-1.ics",
            "\"e2\"",
            "ICS",
            &sample_ics_event(),
        )
        .unwrap();
        let ev = get(&c, id).unwrap().unwrap();
        assert_eq!(ev.calendar_id, Some(cal));
        assert_eq!(list_range(&c, 0, i64::MAX / 2).unwrap().len(), 1);

        // Rows without a mail source are simply purged on server deletion.
        let mut pure = sample_ics_event();
        pure.uid = "srv-only@test".into();
        upsert_remote(&c, 1, cal, "/cal/srv.ics", "\"e1\"", "ICS", &pure).unwrap();
        delete_by_href(&c, cal, "/cal/srv.ics").unwrap();
        let n: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM calendar_events WHERE ical_uid = 'srv-only@test'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn upsert_remote_adopts_mail_invite_row_by_uid() {
        let c = testutil::calendar_conn();
        let msg = 46;
        upsert(&c, 1, msg, &sample_ics_event()).unwrap();
        let cal = seed_calendar(&c);

        upsert_remote(
            &c,
            1,
            cal,
            "/cal/evt-1.ics",
            "\"e1\"",
            "ICS",
            &sample_ics_event(),
        )
        .unwrap();
        // Still one row, now carrying CalDAV identity.
        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM calendar_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        let ev = &for_message(&c, msg).unwrap()[0];
        assert_eq!(ev.calendar_id, Some(cal));
    }

    #[test]
    fn same_uid_stays_distinct_across_provider_calendars() {
        let c = testutil::calendar_conn();
        let cal_a = seed_calendar_at(&c, "https://cal.test/a/", "A");
        let cal_b = seed_calendar_at(&c, "https://cal.test/b/", "B");

        let mut event_a = sample_ics_event();
        event_a.summary = Some("Calendar A".into());
        let mut event_b = sample_ics_event();
        event_b.summary = Some("Calendar B".into());
        assert!(upsert_remote(&c, 1, cal_a, "/a/event.ics", "\"a1\"", "ICS-A", &event_a,).unwrap());
        assert!(upsert_remote(&c, 1, cal_b, "/b/event.ics", "\"b1\"", "ICS-B", &event_b,).unwrap());

        // A renamed resource is still the same row inside its calendar.
        event_b.summary = Some("Calendar B renamed".into());
        assert!(
            !upsert_remote(&c, 1, cal_b, "/b/renamed.ics", "\"b2\"", "ICS-B2", &event_b,).unwrap()
        );

        // A mail update links and changes one deterministic copy, not every
        // same-UID provider row.
        let mut invitation = sample_ics_event();
        invitation.sequence = 2;
        invitation.summary = Some("Invitation update".into());
        upsert(&c, 1, 47, &invitation).unwrap();

        let rows: Vec<(i64, Option<i64>, String, String)> = c
            .prepare(
                "SELECT calendar_id, message_id, caldav_href, summary
                 FROM calendar_events ORDER BY calendar_id",
            )
            .unwrap()
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            rows,
            vec![
                (
                    cal_a,
                    Some(47),
                    "/a/event.ics".into(),
                    "Invitation update".into(),
                ),
                (
                    cal_b,
                    None,
                    "/b/renamed.ics".into(),
                    "Calendar B renamed".into(),
                ),
            ]
        );

        let mut cancel = invitation;
        cancel.method = Some("CANCEL".into());
        upsert(&c, 1, 48, &cancel).unwrap();
        let statuses: Vec<Option<String>> = c
            .prepare("SELECT status FROM calendar_events ORDER BY calendar_id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(statuses, vec![Some("CANCELLED".into()), None]);
    }

    #[test]
    fn notify_window_and_gate() {
        let c = testutil::calendar_conn();
        let id = insert_local(
            &c,
            1,
            None,
            "n1@flectar-mail",
            "Standup",
            None,
            None,
            None,
            "me@test.dev",
            &[],
            100_000,
            101_000,
            false,
        )
        .unwrap();
        // Outside the window.
        assert!(upcoming_for_notify(&c, 0, 10_000).unwrap().is_empty());
        // Inside the window.
        assert_eq!(upcoming_for_notify(&c, 95_000, 10_000).unwrap().len(), 1);
        set_notified(&c, id, 100_000).unwrap();
        assert!(upcoming_for_notify(&c, 95_000, 10_000).unwrap().is_empty());
        // Declined events never notify.
        let id2 = insert_local(
            &c,
            1,
            None,
            "n2@flectar-mail",
            "Skip",
            None,
            None,
            None,
            "me@test.dev",
            &[],
            100_000,
            101_000,
            false,
        )
        .unwrap();
        set_rsvp(&c, id2, "DECLINED").unwrap();
        assert!(upcoming_for_notify(&c, 95_000, 10_000).unwrap().is_empty());
    }

    #[test]
    fn malformed_attendee_json_is_reported() {
        let c = testutil::calendar_conn();
        let id = insert_local(
            &c,
            1,
            None,
            "bad-attendees@flectar-mail",
            "Corrupt",
            None,
            None,
            None,
            "me@test.dev",
            &[],
            100_000,
            101_000,
            false,
        )
        .unwrap();
        c.execute(
            "UPDATE calendar_events SET attendees_json = '{not-json' WHERE id = ?1",
            params![id],
        )
        .unwrap();

        assert!(get(&c, id).is_err());
    }
}
