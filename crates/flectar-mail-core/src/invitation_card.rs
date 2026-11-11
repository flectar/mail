//! The card above a message that carries a calendar (#228): an invitation,
//! its update or cancellation, or an answer to one. It shows the event as
//! the message has it, set against the user's calendar now, as Outlook,
//! Apple Mail, Gmail and Thunderbird do above theirs.

use crate::db::repo;
use crate::db::repo::calendar::CalendarVersion;
use crate::error::Result;
use crate::models::{Address, EventAttendee, InvitationCard, InvitationKind, now_ms};
use crate::{Core, caldav, calendar};

/// How far ahead the next occurrence of a repeating event is looked for.
const NEXT_OCCURRENCE_WITHIN_MS: i64 = 400 * 24 * 60 * 60_000;
/// How many of the user's other events at the same time the card names.
const MAX_CONFLICTS: usize = 3;
/// How long an event without an end is taken to last, for clashes.
const DEFAULT_DURATION_MS: i64 = 60 * 60_000;

impl InvitationKind {
    /// What a calendar's METHOD asks (RFC 5546 section 1.4). A calendar
    /// without one, or with one RFC 5546 doesn't define, only shows an event.
    fn of(method: Option<&str>) -> Self {
        match method {
            Some("REQUEST" | "ADD") => Self::Request,
            Some("CANCEL") => Self::Cancel,
            Some("REPLY") => Self::Reply,
            Some("COUNTER") => Self::Counter,
            Some("DECLINECOUNTER") => Self::DeclineCounter,
            Some("REFRESH") => Self::Refresh,
            _ => Self::Publish,
        }
    }
}

/// The event as one message has it.
struct Shown {
    kind: InvitationKind,
    uid: String,
    title: Option<String>,
    starts_at: i64,
    ends_at: Option<i64>,
    all_day: bool,
    rrule: Option<String>,
    /// The message is about one occurrence of a repeating event.
    occurrence: bool,
    location: Option<String>,
    join_url: Option<String>,
    organizer: Option<Address>,
    attendees: Vec<EventAttendee>,
    cancelled: bool,
    sequence: i64,
    dtstamp: Option<i64>,
    /// The calendar the message carries, whose exceptions a repeating event
    /// skips.
    calendar: Option<String>,
}

impl Shown {
    /// The message's version of its event: the series it is about, or its
    /// first occurrence when it is about occurrences alone.
    fn of_calendar(text: String) -> Option<Self> {
        let events = calendar::parse_ics(&text);
        let uid = events.first()?.uid.clone();
        let shown = events
            .iter()
            .filter(|event| event.uid == uid)
            .min_by_key(|event| (event.recurrence_id_ms.is_some(), event.recurrence_id_ms))?;
        Some(Self {
            kind: InvitationKind::of(shown.method.as_deref()),
            title: shown.summary.clone(),
            starts_at: shown.starts_at_ms,
            ends_at: shown.ends_at_ms,
            all_day: shown.all_day,
            rrule: shown.rrule.clone(),
            occurrence: shown.recurrence_id_ms.is_some(),
            location: shown.location.clone(),
            join_url: shown.join_url.clone(),
            organizer: shown.organizer.clone().map(|email| Address {
                name: shown.organizer_name.clone(),
                email,
            }),
            attendees: shown
                .attendees
                .iter()
                .map(|attendee| EventAttendee {
                    email: attendee.email.clone(),
                    name: attendee.name.clone(),
                    partstat: attendee.partstat.clone(),
                    answered_at: None,
                })
                .collect(),
            cancelled: shown.status.as_deref() == Some("CANCELLED"),
            sequence: shown.sequence,
            dtstamp: shown.dtstamp_ms,
            uid,
            calendar: Some(text),
        })
    }

    /// Mail from before messages kept their calendars: the event its
    /// invitation left in the calendar, which is still this message's.
    fn of_event(uid: String, version: &CalendarVersion) -> Self {
        let event = &version.event;
        let organizer = event
            .organizer
            .clone()
            .filter(|email| !email.is_empty())
            .map(|email| Address {
                // The organizer is often among the guests, with a name.
                name: event
                    .attendees
                    .iter()
                    .find(|attendee| attendee.email.eq_ignore_ascii_case(&email))
                    .and_then(|attendee| attendee.name.clone()),
                email,
            });
        Self {
            kind: InvitationKind::of(event.method.as_deref()),
            uid,
            title: event.summary.clone(),
            starts_at: event.starts_at,
            ends_at: event.ends_at,
            all_day: event.all_day,
            rrule: event.rrule.clone(),
            occurrence: false,
            location: event.location.clone(),
            join_url: event.join_url.clone(),
            organizer,
            attendees: event.attendees.clone(),
            cancelled: event.status.as_deref() == Some("CANCELLED"),
            sequence: version.sequence,
            dtstamp: version.dtstamp,
            calendar: None,
        }
    }

    /// When the event happens next: a repeating event's next occurrence once
    /// its first is over, else its own time.
    fn upcoming(&self, now: i64) -> (i64, Option<i64>) {
        let over = self.ends_at.unwrap_or(self.starts_at) <= now;
        let Some(rrule) = self.rrule.as_deref().filter(|_| over && !self.occurrence) else {
            return (self.starts_at, self.ends_at);
        };
        let duration = self.ends_at.map_or(0, |end| end - self.starts_at);
        caldav::rrule::expand(
            rrule,
            self.starts_at,
            duration,
            self.all_day,
            self.calendar.as_deref(),
            now,
            now + NEXT_OCCURRENCE_WITHIN_MS,
        )
        .and_then(|occurrences| occurrences.into_iter().min_by_key(|next| next.start))
        .map_or((self.starts_at, self.ends_at), |next| {
            (next.start, self.ends_at.map(|_| next.end))
        })
    }
}

impl Core {
    /// The card above a message that carries a calendar, or None for a
    /// message without one.
    pub async fn invitation_card(&self, message_id: i64) -> Result<Option<InvitationCard>> {
        let (text, recorded_uid) = self
            .calendar_db
            .read(move |conn| {
                let text = repo::calendar::message_calendar(conn, message_id)?;
                let uid = match text {
                    Some(_) => None,
                    None => repo::calendar::message_meeting(conn, message_id)?,
                };
                Ok((text, uid))
            })
            .await?;
        let shown = match text {
            Some(text) => Shown::of_calendar(text),
            None => None,
        };
        let Some(uid) = shown
            .as_ref()
            .map(|shown| shown.uid.clone())
            .or(recorded_uid)
        else {
            return Ok(None);
        };
        let mail = self
            .db
            .read(move |conn| {
                let Some(row) = repo::messages::get_row(conn, message_id)? else {
                    return Ok(None);
                };
                let own = repo::sender_identities::own_addresses(conn, row.account_id)?;
                let kept_out =
                    repo::messages::kept_from_calendar(conn, &[message_id])?.contains(&message_id);
                Ok(Some((row.account_id, own, kept_out)))
            })
            .await?;
        let Some((account_id, own, kept_out)) = mail else {
            return Ok(None);
        };
        let lookup = uid.clone();
        let (stored, same_meeting) = self
            .calendar_db
            .read(move |conn| {
                Ok((
                    repo::calendar::version(conn, account_id, &lookup)?,
                    repo::calendar::meeting_rows(conn, account_id, &lookup)?,
                ))
            })
            .await?;
        let stored = stored.filter(|stored| !stored.deleted);
        let Some(mut shown) = shown.or_else(|| Some(Shown::of_event(uid, stored.as_ref()?))) else {
            return Ok(None);
        };
        // An answer, or a request for the latest version, may leave out the
        // event's details (RFC 5546 sections 3.2.3 and 3.2.6); the calendar
        // has them.
        if let Some(stored) = &stored {
            let event = &stored.event;
            if shown.starts_at == 0 {
                shown.starts_at = event.starts_at;
                shown.ends_at = event.ends_at;
                shown.all_day = event.all_day;
            }
            shown.title = shown.title.or_else(|| event.summary.clone());
            shown.location = shown.location.or_else(|| event.location.clone());
        }
        let kind = shown.kind;

        let invited_as = shown
            .attendees
            .iter()
            .find(|attendee| own.contains(&attendee.email))
            .map(|attendee| attendee.email.clone());
        let organized = shown
            .organizer
            .as_ref()
            .is_some_and(|organizer| own.contains(&organizer.email));
        // A version is out of date once the calendar has a later one: a
        // higher SEQUENCE, or the same one written later (RFC 5546 section
        // 2.1.5). An answer is, once the calendar heard a later one.
        let out_of_date = match (&stored, kind) {
            (Some(stored), InvitationKind::Reply) => {
                let answered = shown.attendees.first().and_then(|answer| {
                    stored
                        .event
                        .attendees
                        .iter()
                        .find(|attendee| attendee.email.eq_ignore_ascii_case(&answer.email))
                        .and_then(|attendee| attendee.answered_at)
                });
                matches!((answered, shown.dtstamp), (Some(newest), Some(this)) if newest > this)
            }
            (Some(stored), _) => {
                stored.sequence > shown.sequence
                    || (stored.sequence == shown.sequence
                        && matches!((stored.dtstamp, shown.dtstamp), (Some(newest), Some(this)) if newest > this))
            }
            (None, _) => false,
        };
        let my_answer = stored
            .as_ref()
            .and_then(|stored| {
                stored.event.rsvp_status.clone().or_else(|| {
                    let mine = invited_as.as_deref()?;
                    stored
                        .event
                        .attendees
                        .iter()
                        .find(|attendee| attendee.email.eq_ignore_ascii_case(mine))
                        .and_then(|attendee| attendee.partstat.clone())
                })
            })
            .filter(|answer| answer != "NEEDS-ACTION");
        let cancelled = kind == InvitationKind::Cancel
            || shown.cancelled
            || stored
                .as_ref()
                .is_some_and(|stored| stored.event.status.as_deref() == Some("CANCELLED"));
        let (starts_at, ends_at) = shown.upcoming(now_ms());
        let conflicts = match kind {
            InvitationKind::Request | InvitationKind::Publish
                if !cancelled && !organized && !kept_out && starts_at != 0 =>
            {
                let end = ends_at
                    .filter(|end| *end > starts_at)
                    .unwrap_or(starts_at + DEFAULT_DURATION_MS);
                self.conflicts(starts_at, end, shown.all_day, &same_meeting)
                    .await?
            }
            _ => Vec::new(),
        };
        let answer = match kind {
            InvitationKind::Reply => shown.attendees.first().cloned(),
            _ => None,
        };
        Ok(Some(InvitationCard {
            kind,
            starts_at,
            ends_at,
            all_day: shown.all_day,
            repeats: shown.rrule.is_some()
                || shown.occurrence
                || stored
                    .as_ref()
                    .is_some_and(|stored| stored.event.rrule.is_some()),
            occurrence: shown.occurrence,
            event_id: stored.as_ref().map(|stored| stored.event.id),
            title: shown.title,
            location: shown.location,
            join_url: shown.join_url,
            organizer: shown.organizer,
            attendees: shown.attendees,
            my_answer,
            invited_as,
            organized,
            out_of_date,
            cancelled,
            answer,
            kept_out,
            conflicts,
        }))
    }

    /// The titles of the user's other events between `start` and `end` that
    /// keep them busy: not cancelled or declined, and all-day only when the
    /// event is.
    async fn conflicts(
        &self,
        start: i64,
        end: i64,
        all_day: bool,
        same_meeting: &[i64],
    ) -> Result<Vec<String>> {
        let mut titles: Vec<String> = Vec::new();
        for other in self.list_events(start, end).await? {
            let busy = other.status.as_deref() != Some("CANCELLED")
                && other.rsvp_status.as_deref() != Some("DECLINED")
                && other.all_day == all_day
                && other.starts_at < end
                && other.ends_at.unwrap_or(other.starts_at) > start;
            let title = other.summary.filter(|title| !title.trim().is_empty());
            if busy
                && !same_meeting.contains(&other.id)
                && let Some(title) = title
                && !titles.contains(&title)
            {
                titles.push(title);
            }
        }
        titles.truncate(MAX_CONFLICTS);
        Ok(titles)
    }
}
