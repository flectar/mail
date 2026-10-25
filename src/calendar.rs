//! Calendar domain state and its projection into Slint models.

use crate::{
    AppWindow, CalendarAgendaFilterRow, CalendarAgendaRow, CalendarDay, CalendarEventRow,
    CalendarSourceRow, CalendarWeekDay, I18n,
    date_format::{self, DateFormats, DateStyle},
};
use chrono::{
    Datelike, Duration as ChronoDuration, Local, NaiveDate, NaiveDateTime, NaiveTime, TimeZone,
    Timelike, Utc,
};
use flectar_mail_core::models::{
    Account, Address, Calendar, CalendarConnectionKind, CalendarEvent,
};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    rc::Rc,
};

#[derive(Clone)]
pub(crate) struct LocalCalendarEvent {
    pub(crate) id: i32,
    pub(crate) title: String,
    pub(crate) date: NaiveDate,
    pub(crate) occurrence_start: i64,
    pub(crate) end: NaiveDateTime,
    pub(crate) start_minutes: i32,
    pub(crate) duration_minutes: i32,
    pub(crate) color_index: i32,
    pub(crate) all_day: bool,
    pub(crate) account_id: i64,
    pub(crate) calendar_id: Option<i64>,
    pub(crate) location: String,
    pub(crate) organizer: String,
    pub(crate) description: String,
    pub(crate) attendees: String,
    pub(crate) attendee_addresses: Vec<Address>,
    pub(crate) join_url: String,
    pub(crate) rsvp_status: String,
    pub(crate) recurrence: String,
    pub(crate) status: String,
    pub(crate) is_local: bool,
}

#[derive(Clone)]
pub(crate) struct LocalCalendarAccount {
    pub(crate) id: i64,
    pub(crate) name: String,
    pub(crate) email: String,
    pub(crate) provider: String,
}

#[derive(Clone)]
pub(crate) struct LocalCalendarSource {
    pub(crate) id: i64,
    pub(crate) account_id: i64,
    /// The service the calendar syncs with, named like account providers:
    /// a CalDAV calendar of a Gmail account is "generic", not "gmail".
    pub(crate) provider: &'static str,
    pub(crate) name: String,
    pub(crate) color: String,
    pub(crate) read_only: bool,
    pub(crate) enabled: bool,
    pub(crate) is_default: bool,
    pub(crate) last_synced_at: Option<i64>,
}

pub(crate) struct LocalCalendarState {
    pub(crate) selected_date: NaiveDate,
    pub(crate) visible_month: NaiveDate,
    pub(crate) view_mode: String,
    pub(crate) events: Vec<LocalCalendarEvent>,
    pub(crate) accounts: Vec<LocalCalendarAccount>,
    pub(crate) sources: Vec<LocalCalendarSource>,
    pub(crate) source_rows: Rc<crate::retained_model::RetainedModel<CalendarSourceRow>>,
    pub(crate) agenda_rows: Rc<crate::retained_model::RetainedModel<CalendarAgendaRow>>,
    pub(crate) agenda_filters: Rc<crate::retained_model::RetainedModel<CalendarAgendaFilterRow>>,
    // View filters never pause provider sync or change stored calendar visibility.
    pub(crate) agenda_hidden: HashSet<i64>,
}

impl LocalCalendarState {
    pub(crate) fn new(today: NaiveDate) -> Self {
        Self {
            selected_date: today,
            visible_month: first_of_month(today),
            view_mode: "week".to_owned(),
            events: Vec::new(),
            accounts: Vec::new(),
            sources: Vec::new(),
            source_rows: Rc::default(),
            agenda_rows: Rc::default(),
            agenda_filters: Rc::default(),
            agenda_hidden: HashSet::new(),
        }
    }

    pub(crate) fn navigate(&mut self, scope: &str, direction: i32) {
        if scope == "month" || matches!(self.view_mode.as_str(), "month" | "agenda") {
            self.visible_month = shift_month(self.visible_month, direction);
            if matches!(self.view_mode.as_str(), "month" | "agenda") {
                self.selected_date = self.visible_month;
            }
        } else {
            self.selected_date += ChronoDuration::days(i64::from(direction) * 7);
            self.visible_month = first_of_month(self.selected_date);
        }
    }

    pub(crate) fn set_view(&mut self, view: &str) {
        self.view_mode = match view {
            "month" => "month",
            "agenda" => "agenda",
            _ => "week",
        }
        .to_owned();
        if self.view_mode == "week" {
            self.visible_month = first_of_month(self.selected_date);
        } else if first_of_month(self.selected_date) != self.visible_month {
            self.selected_date = self.visible_month;
        }
    }
}

pub(crate) fn calendar_accounts(accounts: &[Account]) -> Vec<LocalCalendarAccount> {
    accounts
        .iter()
        .map(|account| LocalCalendarAccount {
            id: account.id,
            name: account
                .display_name
                .as_ref()
                .filter(|name| !name.trim().is_empty())
                .unwrap_or(&account.email)
                .clone(),
            email: account.email.clone(),
            provider: account.provider.as_str().to_owned(),
        })
        .collect()
}

pub(crate) fn calendar_sources(calendars: Vec<Calendar>) -> Vec<LocalCalendarSource> {
    calendars
        .into_iter()
        .map(|calendar| LocalCalendarSource {
            id: calendar.id,
            account_id: calendar.account_id,
            provider: match calendar.kind {
                CalendarConnectionKind::Google => "gmail",
                CalendarConnectionKind::Microsoft => "microsoft",
                CalendarConnectionKind::Generic => "generic",
            },
            name: calendar.display_name.unwrap_or_default(),
            color: calendar.color.unwrap_or_default(),
            read_only: calendar.read_only,
            enabled: calendar.enabled,
            is_default: calendar.is_default,
            last_synced_at: calendar.last_synced_at,
        })
        .collect()
}

/// Map arbitrary provider colors onto the four event palettes currently used
/// by the Slint calendar. This keeps Google's Birthdays/holiday colors
/// visually consistent instead of assigning colors from database row ids.
fn calendar_color_index(color: &str, fallback_id: i64) -> i32 {
    let parsed = color
        .strip_prefix('#')
        .filter(|hex| hex.len() >= 6)
        .and_then(|hex| u32::from_str_radix(&hex[..6], 16).ok());
    let Some(rgb) = parsed else {
        return i32::try_from(fallback_id.rem_euclid(4)).unwrap_or_default();
    };
    let sample = (
        ((rgb >> 16) & 0xff) as i32,
        ((rgb >> 8) & 0xff) as i32,
        (rgb & 0xff) as i32,
    );
    const PALETTE: [(i32, i32, i32); 4] = [
        (52, 120, 246),
        (139, 92, 246),
        (232, 121, 36),
        (38, 162, 105),
    ];
    PALETTE
        .iter()
        .enumerate()
        .min_by_key(|entry| {
            let (red, green, blue) = *entry.1;
            (sample.0 - red).pow(2) + (sample.1 - green).pow(2) + (sample.2 - blue).pow(2)
        })
        .map(|(index, _)| index as i32)
        .unwrap_or_default()
}

pub(crate) fn core_calendar_event(event: CalendarEvent) -> LocalCalendarEvent {
    let start = if event.all_day {
        Utc.timestamp_millis_opt(event.starts_at)
            .single()
            .map(|value| value.naive_utc())
    } else {
        Local
            .timestamp_millis_opt(event.starts_at)
            .single()
            .map(|value| value.naive_local())
    }
    .unwrap_or_else(|| Local::now().naive_local());
    let attendees = event
        .attendees
        .iter()
        .map(|attendee| {
            let identity = attendee
                .name
                .as_ref()
                .filter(|name| !name.trim().is_empty())
                .map(|name| format!("{name} <{}>", attendee.email))
                .unwrap_or_else(|| attendee.email.clone());
            attendee
                .partstat
                .as_ref()
                .filter(|status| !status.trim().is_empty())
                .map(|status| format!("{identity} · {}", status.replace('-', " ")))
                .unwrap_or(identity)
        })
        .collect::<Vec<_>>()
        .join("\n");
    let attendee_addresses = event
        .attendees
        .iter()
        .map(|attendee| Address {
            name: attendee.name.clone(),
            email: attendee.email.clone(),
        })
        .collect();
    let end_ms = event
        .ends_at
        .filter(|end| *end > event.starts_at)
        .unwrap_or_else(|| event.starts_at.saturating_add(1_800_000));
    let end = if event.all_day {
        Utc.timestamp_millis_opt(end_ms)
            .single()
            .map(|value| value.naive_utc())
    } else {
        Local
            .timestamp_millis_opt(end_ms)
            .single()
            .map(|value| value.naive_local())
    }
    .unwrap_or(start + ChronoDuration::minutes(30));
    LocalCalendarEvent {
        id: i32::try_from(event.id).unwrap_or(i32::MAX),
        title: event.summary.unwrap_or_default(),
        date: start.date(),
        occurrence_start: event.starts_at,
        end,
        start_minutes: if event.all_day {
            0
        } else {
            start.time().hour() as i32 * 60 + start.time().minute() as i32
        },
        duration_minutes: event
            .ends_at
            .map(|end| {
                (end.saturating_sub(event.starts_at) / 60_000).clamp(1, i64::from(i32::MAX)) as i32
            })
            .unwrap_or(30),
        color_index: i32::try_from(event.calendar_id.unwrap_or(event.account_id).rem_euclid(4))
            .unwrap_or_default(),
        all_day: event.all_day,
        account_id: event.account_id,
        calendar_id: event.calendar_id,
        location: event.location.unwrap_or_default(),
        organizer: event.organizer.unwrap_or_default(),
        description: event.description.unwrap_or_default(),
        attendees,
        attendee_addresses,
        join_url: event.join_url.unwrap_or_default(),
        rsvp_status: event.rsvp_status.unwrap_or_default(),
        recurrence: event.rrule.unwrap_or_default(),
        status: event.status.unwrap_or_default(),
        is_local: event.is_local,
    }
}

pub(crate) fn refresh_calendar_events(
    core: &crate::mail::CoreMailSource,
    runtime: &tokio::runtime::Runtime,
    state: &mut LocalCalendarState,
    accounts: &[Account],
) -> Result<(), String> {
    let (start_ms, end_ms) = calendar_range_millis(state.visible_month)?;
    let (events, calendars) = runtime.block_on(async {
        tokio::join!(
            core.load_events(start_ms, end_ms),
            core.load_calendars(None),
        )
    });
    state.events = events?.into_iter().map(core_calendar_event).collect();
    state.accounts = calendar_accounts(accounts);
    state.sources = calendar_sources(calendars?);
    Ok(())
}

pub(crate) fn calendar_range_millis(visible_month: NaiveDate) -> Result<(i64, i64), String> {
    let month_start = first_of_month(visible_month);
    let range_start = start_of_week(month_start) - ChronoDuration::days(8);
    let range_end = shift_month(month_start, 2) + ChronoDuration::days(8);
    let start_ms = Local
        .from_local_datetime(&range_start.and_time(NaiveTime::MIN))
        .earliest()
        .ok_or_else(|| "calendar range has no local start".to_owned())?
        .timestamp_millis();
    let end_ms = Local
        .from_local_datetime(&range_end.and_time(NaiveTime::MIN))
        .latest()
        .ok_or_else(|| "calendar range has no local end".to_owned())?
        .timestamp_millis();
    Ok((start_ms, end_ms))
}

pub(crate) fn first_of_month(date: NaiveDate) -> NaiveDate {
    date.with_day(1).unwrap_or(date)
}

pub(crate) fn start_of_week(date: NaiveDate) -> NaiveDate {
    date - ChronoDuration::days(date.weekday().num_days_from_monday() as i64)
}

pub(crate) fn shift_month(date: NaiveDate, delta: i32) -> NaiveDate {
    let month_index = date.year() * 12 + date.month0() as i32 + delta;
    let year = month_index.div_euclid(12);
    let month = month_index.rem_euclid(12) as u32 + 1;
    NaiveDate::from_ymd_opt(year, month, 1).unwrap_or(date)
}

fn calendar_period_title(formats: &DateFormats, state: &LocalCalendarState) -> String {
    if matches!(state.view_mode.as_str(), "month" | "agenda") {
        return formats.date(DateStyle::MonthYear, state.visible_month);
    }
    let start = start_of_week(state.selected_date);
    formats.date_range(start, start + ChronoDuration::days(6))
}

#[derive(Debug, PartialEq, Eq)]
struct AgendaOccurrence {
    event_index: usize,
    date: NaiveDate,
    start_minutes: i32,
    end_minutes: i32,
}

/// Split intervals at local midnight, clipping to the requested month. The
/// exclusive end prevents an all-day or midnight-ending event gaining a day.
fn agenda_occurrences(state: &LocalCalendarState) -> BTreeMap<NaiveDate, Vec<AgendaOccurrence>> {
    let start = first_of_month(state.visible_month);
    let end = shift_month(start, 1);
    let mut days: BTreeMap<_, Vec<_>> = BTreeMap::new();
    let enabled: HashMap<_, _> = state
        .sources
        .iter()
        .map(|source| (source.id, source.enabled))
        .collect();
    for (event_index, event) in state.events.iter().enumerate() {
        if state
            .agenda_hidden
            .contains(&event.calendar_id.unwrap_or(-1))
            || event
                .calendar_id
                .is_some_and(|id| enabled.get(&id) == Some(&false))
        {
            continue;
        }
        let last_day = (event.end - ChronoDuration::nanoseconds(1)).date();
        let mut date = event.date.max(start);
        while date <= last_day && date < end {
            days.entry(date).or_default().push(AgendaOccurrence {
                event_index,
                date,
                start_minutes: if date == event.date {
                    event.start_minutes
                } else {
                    0
                },
                end_minutes: if date == event.end.date() {
                    event.end.hour() as i32 * 60 + event.end.minute() as i32
                } else {
                    1440
                },
            });
            date += ChronoDuration::days(1);
        }
    }
    for occurrences in days.values_mut() {
        occurrences.sort_by_key(|occurrence| {
            let event = &state.events[occurrence.event_index];
            (
                !event.all_day,
                occurrence.start_minutes,
                event.end,
                event.id,
            )
        });
    }
    days
}

/// A time of day from minutes after midnight, up to 1440 for the day's end.
fn minute_label(formats: &DateFormats, minutes: i32) -> String {
    let minutes = minutes.clamp(0, 24 * 60) as u32;
    formats.time(minutes / 60, minutes % 60)
}

pub(crate) fn apply_agenda(
    app: &AppWindow,
    state: &LocalCalendarState,
    today: NaiveDate,
    time: NaiveTime,
) {
    let mut days = agenda_occurrences(state);
    // Count occurrences once in the toolbar, even when they span several days.
    let event_count = days
        .values()
        .flatten()
        .map(|row| row.event_index)
        .collect::<HashSet<_>>()
        .len();
    let month_start = first_of_month(state.visible_month);
    if event_count > 0 && today >= month_start && today < shift_month(month_start, 1) {
        days.entry(today).or_default();
    }
    let now_minutes = time.hour() as i32 * 60 + time.minute() as i32;
    let formats = date_format::current();
    let mut rows = Vec::new();
    let mut anchor = 0;
    let mut height = 0;
    let mut anchored = false;
    let mut event_rows = HashMap::new();
    for (date, occurrences) in days {
        if !anchored && date >= state.selected_date {
            anchor = height;
            anchored = true;
        }
        let iso_date = date.format("%Y-%m-%d").to_string();
        rows.push(CalendarAgendaRow {
            key: format!("day:{date}").into(),
            kind: "day".into(),
            iso_date: iso_date.clone().into(),
            date_label: formats.date(DateStyle::WeekdayDayMonth, date).into(),
            is_today: date == today,
            count: occurrences.len() as i32,
            ..Default::default()
        });
        height += 68;
        let mut now_shown = date != today;
        let now_row = || CalendarAgendaRow {
            key: format!("now:{date}").into(),
            kind: "now".into(),
            iso_date: iso_date.clone().into(),
            time_label: minute_label(&formats, now_minutes).into(),
            ..Default::default()
        };
        for occurrence in occurrences {
            let event = &state.events[occurrence.event_index];
            if !now_shown && !event.all_day && occurrence.start_minutes >= now_minutes {
                rows.push(now_row());
                height += 68;
                now_shown = true;
            }
            rows.push(CalendarAgendaRow {
                key: format!("event:{date}:{}:{}", event.id, event.occurrence_start).into(),
                kind: "event".into(),
                iso_date: iso_date.clone().into(),
                time_label: if event.all_day {
                    app.global::<I18n>().invoke_calendar_all_day()
                } else {
                    minute_label(&formats, occurrence.start_minutes).into()
                },
                end_label: if event.all_day {
                    "".into()
                } else {
                    minute_label(&formats, occurrence.end_minutes).into()
                },
                event: event_rows
                    .entry(occurrence.event_index)
                    .or_insert_with(|| event_row(app, state, &formats, event))
                    .clone(),
                ..Default::default()
            });
            height += 68;
        }
        if !now_shown {
            rows.push(now_row());
            height += 68;
        }
    }
    if !anchored && event_count > 0 {
        anchor = height;
    }
    if event_count > 0 {
        rows.push(CalendarAgendaRow {
            key: "end".into(),
            kind: "end".into(),
            ..Default::default()
        });
    }
    let mut offset = 0;
    let mut previous = -1;
    for (index, row) in rows.iter_mut().enumerate() {
        row.offset = offset as f32;
        row.previous_event = previous;
        offset += 68;
        if row.kind == "event" {
            previous = index as i32;
        }
    }
    let mut next = -1;
    for (index, row) in rows.iter_mut().enumerate().rev() {
        row.next_event = next;
        if row.kind == "event" {
            next = index as i32;
        }
    }
    let mut filters = vec![CalendarAgendaFilterRow {
        id: -1,
        name: app.global::<I18n>().invoke_ui_message_calendar(
            "Local calendar".into(),
            "".into(),
            "".into(),
            "".into(),
        ),
        checked: !state.agenda_hidden.contains(&-1),
        ..Default::default()
    }];
    filters.extend(
        state
            .sources
            .iter()
            .filter(|source| source.enabled)
            .map(|source| CalendarAgendaFilterRow {
                id: i32::try_from(source.id).unwrap_or(i32::MAX),
                name: if source.name.is_empty() {
                    app.global::<I18n>().invoke_ui_message_calendar(
                        "Calendar".into(),
                        "".into(),
                        "".into(),
                        "".into(),
                    )
                } else {
                    source.name.clone().into()
                },
                account: state
                    .accounts
                    .iter()
                    .find(|account| account.id == source.account_id)
                    .map(|account| account.email.clone())
                    .unwrap_or_default()
                    .into(),
                color_index: calendar_color_index(&source.color, source.id),
                checked: !state.agenda_hidden.contains(&source.id),
            }),
    );
    app.set_calendar_agenda_filter_count(
        filters.iter().filter(|filter| !filter.checked).count() as i32
    );
    state
        .agenda_filters
        .reconcile_by(filters, |row| row.id, PartialEq::eq);
    state
        .agenda_rows
        .reconcile_by(rows, |row| row.key.clone(), PartialEq::eq);
    app.set_calendar_agenda_rows(Rc::clone(&state.agenda_rows).into());
    app.set_calendar_agenda_filters(Rc::clone(&state.agenda_filters).into());
    app.set_calendar_agenda_event_count(event_count as i32);
    app.set_calendar_agenda_anchor(anchor as f32);
    app.set_calendar_timezone(Local::now().format("UTC%:z").to_string().into());
}

pub(crate) fn agenda_row_index(rows: ModelRc<CalendarAgendaRow>, key: SharedString) -> i32 {
    use slint::Model;
    if key.is_empty() {
        return -1;
    }
    rows.iter()
        .position(|row| row.kind == "event" && row.key == key)
        .map(|index| index as i32)
        .unwrap_or(-1)
}

fn event_row(
    app: &AppWindow,
    state: &LocalCalendarState,
    formats: &DateFormats,
    event: &LocalCalendarEvent,
) -> CalendarEventRow {
    let week_start = start_of_week(state.selected_date);
    let grid_start = start_of_week(first_of_month(state.visible_month));
    let source = event
        .calendar_id
        .and_then(|calendar_id| state.sources.iter().find(|source| source.id == calendar_id));
    let account = state
        .accounts
        .iter()
        .find(|account| account.id == event.account_id);
    CalendarEventRow {
        id: event.id,
        editable: event.is_local,
        title: if event.title.is_empty() {
            app.global::<I18n>().invoke_untitled_event()
        } else {
            event.title.clone().into()
        },
        detail: if event.all_day {
            let last_day = (event.end - ChronoDuration::nanoseconds(1)).date();
            if last_day > event.date {
                format!(
                    "{} · {}",
                    app.global::<I18n>().invoke_calendar_all_day(),
                    formats.date_range(event.date, last_day)
                )
                .into()
            } else {
                app.global::<I18n>().invoke_calendar_all_day()
            }
        } else {
            let end = if event.end.date() == event.date {
                formats.time(event.end.hour(), event.end.minute())
            } else {
                formats.date_time(DateStyle::Medium, event.end)
            };
            formats
                .range(&minute_label(formats, event.start_minutes), &end)
                .into()
        },
        iso_date: event.date.format("%Y-%m-%d").to_string().into(),
        date_label: formats.date(DateStyle::WeekdayMedium, event.date).into(),
        day_index: (event.date - week_start).num_days() as i32,
        month_index: (event.date - grid_start).num_days() as i32,
        start_minutes: event.start_minutes,
        duration_minutes: event.duration_minutes,
        color_index: source
            .map(|source| calendar_color_index(&source.color, source.id))
            .unwrap_or(event.color_index),
        all_day: event.all_day,
        provider: source
            .map(|source| source.provider)
            .or_else(|| account.map(|account| account.provider.as_str()))
            .unwrap_or("local")
            .into(),
        account: account
            .map(|account| {
                if account.name == account.email {
                    account.email.clone()
                } else {
                    format!("{} · {}", account.name, account.email)
                }
            })
            .unwrap_or_default()
            .into(),
        calendar: source
            .map(|source| source.name.as_str())
            .unwrap_or_default()
            .into(),
        location: event.location.clone().into(),
        organizer: event.organizer.clone().into(),
        description: event.description.clone().into(),
        attendees: event.attendees.clone().into(),
        join_url: event.join_url.clone().into(),
        rsvp_status: event.rsvp_status.clone().into(),
        recurrence: event.recurrence.clone().into(),
        status: event.status.clone().into(),
        source: if event.is_local { "local" } else { "provider" }.into(),
    }
}

pub(crate) fn apply_calendar(app: &AppWindow, state: &LocalCalendarState, today: NaiveDate) {
    let formats = date_format::current();
    let selected_date_has_events = state
        .events
        .iter()
        .any(|event| event.date == state.selected_date);
    let month_start = first_of_month(state.visible_month);
    let grid_start = start_of_week(month_start);
    let month_days = (0..42)
        .map(|offset| {
            let date = grid_start + ChronoDuration::days(offset);
            CalendarDay {
                day: date.day().to_string().into(),
                iso_date: date.format("%Y-%m-%d").to_string().into(),
                date_label: formats.date(DateStyle::WeekdayMedium, date).into(),
                outside_month: date.month() != state.visible_month.month(),
                is_today: date == today,
                is_selected: date == state.selected_date,
                has_events: state.events.iter().any(|event| event.date == date),
            }
        })
        .collect::<Vec<_>>();

    let week_start = start_of_week(state.selected_date);
    let week_days = (0..7)
        .map(|offset| {
            let date = week_start + ChronoDuration::days(offset);
            CalendarWeekDay {
                weekday: formats
                    .weekday(date.weekday().num_days_from_monday(), true)
                    .to_uppercase()
                    .into(),
                day: date.day().to_string().into(),
                iso_date: date.format("%Y-%m-%d").to_string().into(),
                date_label: formats.date(DateStyle::WeekdayMedium, date).into(),
                is_today: date == today,
            }
        })
        .collect::<Vec<_>>();

    let (range_start, range_end) = if matches!(state.view_mode.as_str(), "month" | "agenda") {
        (grid_start, grid_start + ChronoDuration::days(41))
    } else {
        (week_start, week_start + ChronoDuration::days(6))
    };
    let accounts: HashMap<_, _> = state
        .accounts
        .iter()
        .map(|account| (account.id, account))
        .collect();
    let mut previous_source_account_id = None;
    let source_rows = state
        .sources
        .iter()
        .map(|source| {
            let group_start = previous_source_account_id != Some(source.account_id);
            previous_source_account_id = Some(source.account_id);
            let account = accounts.get(&source.account_id);
            CalendarSourceRow {
                id: i32::try_from(source.id).unwrap_or(i32::MAX),
                account_id: i32::try_from(source.account_id).unwrap_or(i32::MAX),
                group_start,
                name: source.name.clone().into(),
                account_name: account
                    .map(|account| account.name.as_str())
                    .unwrap_or_default()
                    .into(),
                email: account
                    .map(|account| account.email.as_str())
                    .unwrap_or_default()
                    .into(),
                provider: source.provider.into(),
                color: source.color.clone().into(),
                color_index: calendar_color_index(&source.color, source.id),
                read_only: source.read_only,
                enabled: source.enabled,
                is_default: source.is_default,
                last_synced: source
                    .last_synced_at
                    .and_then(|millis| Local.timestamp_millis_opt(millis).single())
                    .map(|value| formats.date_time(DateStyle::Medium, value.naive_local()))
                    .unwrap_or_default()
                    .into(),
            }
        })
        .collect::<Vec<_>>();

    let events = state
        .events
        .iter()
        .filter(|event| event.date >= range_start && event.date <= range_end)
        .map(|event| event_row(app, state, &formats, event))
        .collect::<Vec<_>>();

    apply_agenda(app, state, today, Local::now().time());
    app.set_calendar_month_days(ModelRc::new(VecModel::from(month_days)));
    app.set_calendar_week_days(ModelRc::new(VecModel::from(week_days)));
    state
        .source_rows
        .reconcile_by(source_rows, |row| row.id, PartialEq::eq);
    app.set_calendar_events(ModelRc::new(VecModel::from(events)));
    app.set_calendar_month_title(
        formats
            .date(DateStyle::MonthYear, state.visible_month)
            .into(),
    );
    app.set_calendar_period_title(calendar_period_title(&formats, state).into());
    app.set_calendar_week_label(format!("W{:02}", state.selected_date.iso_week().week()).into());
    app.set_calendar_selected_date(state.selected_date.format("%Y-%m-%d").to_string().into());
    app.set_calendar_selected_date_label(
        formats
            .date(DateStyle::WeekdayMedium, state.selected_date)
            .into(),
    );
    apply_hour_labels(app, &formats);
    app.set_calendar_selected_date_has_events(selected_date_has_events);
    app.set_calendar_view_mode(state.view_mode.clone().into());
}

/// The week view's hour ruler. The model is replaced only when the labels
/// change, since a new model recreates every ruler row.
fn apply_hour_labels(app: &AppWindow, formats: &DateFormats) {
    use slint::Model;
    let labels = (8..=18)
        .map(|hour| SharedString::from(formats.hour(hour)))
        .collect::<Vec<_>>();
    if !app
        .get_calendar_hour_labels()
        .iter()
        .eq(labels.iter().cloned())
    {
        app.set_calendar_hour_labels(ModelRc::new(VecModel::from(labels)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use slint::Model;

    fn date(day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, day).unwrap()
    }

    fn event(id: i32, day: u32, start: i32, duration: i64, all_day: bool) -> LocalCalendarEvent {
        let starts = date(day)
            .and_hms_opt((start / 60) as u32, (start % 60) as u32, 0)
            .unwrap();
        LocalCalendarEvent {
            id,
            title: format!("Event {id}"),
            date: date(day),
            occurrence_start: starts.and_utc().timestamp_millis(),
            end: starts + ChronoDuration::minutes(duration),
            start_minutes: start,
            duration_minutes: duration as i32,
            color_index: id.rem_euclid(4),
            all_day,
            account_id: 0,
            calendar_id: None,
            location: String::new(),
            organizer: String::new(),
            description: String::new(),
            attendees: String::new(),
            attendee_addresses: Vec::new(),
            join_url: String::new(),
            rsvp_status: String::new(),
            recurrence: String::new(),
            status: String::new(),
            is_local: true,
        }
    }

    #[test]
    fn agenda_orders_busy_days_and_preserves_simultaneous_events() {
        let mut state = LocalCalendarState::new(date(8));
        state.events = vec![
            event(3, 8, 600, 30, false),
            event(1, 8, 0, 1440, true),
            event(2, 8, 600, 30, false),
            event(4, 9, 480, 30, false),
        ];
        let days = agenda_occurrences(&state);
        assert_eq!(
            days[&date(8)]
                .iter()
                .map(|row| state.events[row.event_index].id)
                .collect::<Vec<_>>(),
            [1, 2, 3]
        );
        assert_eq!(days[&date(9)].len(), 1);
        state.events = (0..1500).map(|id| event(id, 8, 600, 30, false)).collect();
        assert_eq!(agenda_occurrences(&state)[&date(8)].len(), 1500);
    }

    #[test]
    fn agenda_splits_overnight_and_all_day_events_with_exclusive_ends() {
        let mut state = LocalCalendarState::new(date(8));
        state.events = vec![
            event(1, 8, 1380, 120, false),
            event(2, 8, 0, 2880, true),
            event(3, 8, 1380, 60, false),
        ];
        let days = agenda_occurrences(&state);
        assert_eq!(days[&date(8)].len(), 3);
        assert_eq!(days[&date(9)].len(), 2);
        assert!(!days.contains_key(&date(10)));
        let overnight = days[&date(9)]
            .iter()
            .find(|row| state.events[row.event_index].id == 1)
            .unwrap();
        assert_eq!((overnight.start_minutes, overnight.end_minutes), (0, 60));
        let midnight = days[&date(8)]
            .iter()
            .find(|row| state.events[row.event_index].id == 3)
            .unwrap();
        assert_eq!(midnight.end_minutes, 1440);
    }

    #[test]
    fn agenda_clips_events_to_month_and_filters_without_changing_sources() {
        let mut state = LocalCalendarState::new(date(8));
        let mut carry = event(1, 1, 0, 2880, true);
        carry.date -= ChronoDuration::days(1);
        state.events = vec![
            carry,
            event(2, 31, 1380, 120, false),
            event(3, 8, 600, 30, false),
        ];
        state.events[2].calendar_id = Some(12);
        state.sources.push(LocalCalendarSource {
            id: 12,
            account_id: 0,
            provider: "generic",
            name: "Work".into(),
            color: String::new(),
            read_only: true,
            enabled: true,
            is_default: false,
            last_synced_at: None,
        });
        let days = agenda_occurrences(&state);
        assert_eq!(days.keys().next(), Some(&date(1)));
        assert_eq!(days.keys().last(), Some(&date(31)));
        state.agenda_hidden.insert(12);
        assert!(!agenda_occurrences(&state).contains_key(&date(8)));
        assert!(state.sources[0].enabled);
        state.agenda_hidden.insert(-1);
        assert!(agenda_occurrences(&state).is_empty());
        state.agenda_hidden.clear();
        state.sources[0].enabled = false;
        assert!(!agenda_occurrences(&state).contains_key(&date(8)));
    }

    #[test]
    fn agenda_navigation_and_mode_changes_preserve_the_browsed_month() {
        let mut state = LocalCalendarState::new(date(8));
        state.set_view("agenda");
        state.navigate("period", 2);
        assert_eq!(
            state.visible_month,
            NaiveDate::from_ymd_opt(2026, 12, 1).unwrap()
        );
        state.navigate("month", 1);
        assert_eq!(
            state.selected_date,
            NaiveDate::from_ymd_opt(2027, 1, 1).unwrap()
        );
        state.set_view("month");
        assert_eq!(state.visible_month, state.selected_date);
        state.set_view("week");
        state.navigate("month", 1);
        state.set_view("agenda");
        assert_eq!(
            state.selected_date,
            NaiveDate::from_ymd_opt(2027, 2, 1).unwrap()
        );
        state.set_view("invalid");
        assert_eq!(state.view_mode, "week");
        state.navigate("period", 1);
        assert_eq!(state.selected_date.day(), 8);
    }

    struct Headless(Rc<slint::platform::software_renderer::MinimalSoftwareWindow>);
    impl slint::platform::Platform for Headless {
        fn create_window_adapter(
            &self,
        ) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
            Ok(self.0.clone())
        }
    }

    #[test]
    fn agenda_projection_updates_now_without_losing_occurrences_or_model_identity() {
        use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(Headless(window))).unwrap();
        let app = AppWindow::new().unwrap();
        let mut state = LocalCalendarState::new(date(8));
        state.events = vec![
            event(1, 8, 0, 1440, true),
            event(2, 8, 540, 30, false),
            event(2, 9, 540, 30, false),
            event(3, 8, 720, 30, false),
        ];
        apply_agenda(
            &app,
            &state,
            date(8),
            NaiveTime::from_hms_opt(10, 0, 0).unwrap(),
        );
        let model = app.get_calendar_agenda_rows();
        let rows: Vec<_> = model.iter().collect();
        assert_eq!(
            rows.iter().map(|row| row.kind.as_str()).collect::<Vec<_>>(),
            [
                "day", "event", "event", "now", "event", "day", "event", "end"
            ]
        );
        assert_eq!(rows[3].time_label, "10:00");
        assert_ne!(rows[2].key, rows[6].key); // recurring occurrences share a database id
        assert_eq!(rows[4].previous_event, 2);
        assert_eq!(rows[2].next_event, 4);
        apply_agenda(
            &app,
            &state,
            date(8),
            NaiveTime::from_hms_opt(13, 0, 0).unwrap(),
        );
        assert_eq!(app.get_calendar_agenda_rows(), model);
        let rows: Vec<_> = model.iter().collect();
        assert_eq!(rows[4].kind, "now");
        assert_eq!(rows[4].time_label, "13:00");
        state.selected_date = date(9);
        apply_agenda(
            &app,
            &state,
            date(8),
            NaiveTime::from_hms_opt(13, 0, 0).unwrap(),
        );
        assert_eq!(app.get_calendar_agenda_anchor(), rows[5].offset);
        // Today still gets a marker when there are only events on another day.
        state.events.retain(|event| event.date != date(8));
        apply_agenda(
            &app,
            &state,
            date(8),
            NaiveTime::from_hms_opt(13, 0, 0).unwrap(),
        );
        assert!(model.iter().any(|row| row.kind == "now"));
        state.agenda_hidden.insert(-1);
        apply_agenda(
            &app,
            &state,
            date(8),
            NaiveTime::from_hms_opt(13, 0, 0).unwrap(),
        );
        assert_eq!(model.row_count(), 0);
        assert_eq!(app.get_calendar_agenda_event_count(), 0);
        assert_eq!(app.get_calendar_agenda_filter_count(), 1);
    }

    #[test]
    fn provider_colors_map_to_the_nearest_calendar_palette() {
        assert_eq!(calendar_color_index("#4285f4", 9), 0);
        assert_eq!(calendar_color_index("#f6bf26", 9), 2);
        assert_eq!(calendar_color_index("#33b679", 9), 3);
        assert_eq!(calendar_color_index("not-a-color", 9), 1);
    }

    /// Slint's software renderer panics when it paints a rectangle reaching
    /// past ±32767 physical pixels (#35). Synced timed events can span weeks.
    #[test]
    fn week_view_paints_multi_week_timed_events() {
        use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(Headless(window.clone()))).unwrap();
        let app = AppWindow::new().unwrap();
        app.window()
            .dispatch_event(slint::platform::WindowEvent::ScaleFactorChanged { scale_factor: 2.0 });
        app.set_startup_ready(true);
        app.set_startup_hydrated(true);
        app.set_connected_accounts(slint::ModelRc::new(slint::VecModel::from(vec![
            crate::AccountRow {
                id: 1,
                ..Default::default()
            },
        ])));
        app.set_active_view("calendar".into());
        let (width, height) = (2560, 1800);
        app.window()
            .set_size(slint::PhysicalSize::new(width, height));
        app.show().unwrap();
        let mut state = LocalCalendarState::new(date(8));
        state.events = vec![event(1, 8, 540, 28 * 24 * 60, false)];
        apply_calendar(&app, &state, date(8));
        assert_eq!(app.get_calendar_view_mode(), "week");
        let mut pixels = vec![slint::Rgb8Pixel::default(); (width * height) as usize];
        window.draw_if_needed(|r| {
            r.render(&mut pixels, width as usize);
        });
        app.hide().unwrap();
    }
}
