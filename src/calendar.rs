//! Calendar domain state and its projection into Slint models.

use crate::{AppWindow, CalendarDay, CalendarEventRow, CalendarSourceRow, CalendarWeekDay, I18n};
use chrono::{
    Datelike, Duration as ChronoDuration, Local, NaiveDate, NaiveTime, TimeZone, Timelike, Utc,
};
use flectar_mail_core::models::{Account, Address, Calendar, CalendarEvent};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::{collections::HashMap, rc::Rc};

#[derive(Clone)]
pub(crate) struct LocalCalendarEvent {
    pub(crate) id: i32,
    pub(crate) title: String,
    pub(crate) date: NaiveDate,
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
    LocalCalendarEvent {
        id: i32::try_from(event.id).unwrap_or(i32::MAX),
        title: event.summary.unwrap_or_default(),
        date: start.date(),
        start_minutes: if event.all_day {
            0
        } else {
            start.time().hour() as i32 * 60 + start.time().minute() as i32
        },
        duration_minutes: event
            .ends_at
            .map(|end| ((end - event.starts_at) / 60_000).max(1) as i32)
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

fn month_name(app: &AppWindow, date: NaiveDate, short: bool) -> String {
    app.global::<I18n>()
        .invoke_month_name(date.month() as i32, short)
        .into()
}

fn calendar_period_title(app: &AppWindow, state: &LocalCalendarState) -> String {
    if state.view_mode == "month" {
        return format!(
            "{} {}",
            month_name(app, state.visible_month, false),
            state.visible_month.year()
        );
    }
    let start = start_of_week(state.selected_date);
    let end = start + ChronoDuration::days(6);
    if start.year() == end.year() && start.month() == end.month() {
        format!(
            "{} {} – {}, {}",
            month_name(app, start, false),
            start.day(),
            end.day(),
            start.year()
        )
    } else if start.year() == end.year() {
        format!(
            "{} {} – {} {}, {}",
            month_name(app, start, true),
            start.day(),
            month_name(app, end, true),
            end.day(),
            start.year()
        )
    } else {
        format!(
            "{} {}, {} – {} {}, {}",
            month_name(app, start, true),
            start.day(),
            start.year(),
            month_name(app, end, true),
            end.day(),
            end.year()
        )
    }
}

pub(crate) fn apply_calendar(app: &AppWindow, state: &LocalCalendarState, today: NaiveDate) {
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
                weekday: app
                    .global::<I18n>()
                    .invoke_weekday_short(date.weekday().num_days_from_monday() as i32)
                    .to_uppercase()
                    .into(),
                day: date.day().to_string().into(),
                iso_date: date.format("%Y-%m-%d").to_string().into(),
                is_today: date == today,
            }
        })
        .collect::<Vec<_>>();

    let (range_start, range_end) = if state.view_mode == "month" {
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
                provider: account
                    .map(|account| account.provider.as_str())
                    .unwrap_or("local")
                    .into(),
                color: source.color.clone().into(),
                color_index: calendar_color_index(&source.color, source.id),
                read_only: source.read_only,
                enabled: source.enabled,
                is_default: source.is_default,
                last_synced: source
                    .last_synced_at
                    .and_then(|millis| Local.timestamp_millis_opt(millis).single())
                    .map(|value| value.format("%Y-%m-%d %H:%M").to_string())
                    .unwrap_or_default()
                    .into(),
            }
        })
        .collect::<Vec<_>>();

    let events = state
        .events
        .iter()
        .filter(|event| event.date >= range_start && event.date <= range_end)
        .map(|event| {
            let source = event.calendar_id.and_then(|calendar_id| {
                state.sources.iter().find(|source| source.id == calendar_id)
            });
            let account = state
                .accounts
                .iter()
                .find(|account| account.id == event.account_id);
            let start_time = NaiveTime::from_num_seconds_from_midnight_opt(
                (event.start_minutes.max(0) * 60) as u32,
                0,
            )
            .unwrap_or_default();
            let end_minutes = (event.start_minutes + event.duration_minutes).min(24 * 60 - 1);
            let end_time =
                NaiveTime::from_num_seconds_from_midnight_opt((end_minutes.max(0) * 60) as u32, 0)
                    .unwrap_or_default();
            CalendarEventRow {
                id: event.id,
                editable: event.is_local,
                title: if event.title.is_empty() {
                    app.global::<I18n>().invoke_untitled_event()
                } else {
                    event.title.clone().into()
                },
                detail: if event.all_day {
                    app.global::<I18n>().invoke_calendar_all_day()
                } else {
                    format!(
                        "{} – {}",
                        start_time.format("%H:%M"),
                        end_time.format("%H:%M")
                    )
                    .into()
                },
                iso_date: event.date.format("%Y-%m-%d").to_string().into(),
                day_index: (event.date - week_start).num_days() as i32,
                month_index: (event.date - grid_start).num_days() as i32,
                start_minutes: event.start_minutes,
                duration_minutes: event.duration_minutes,
                color_index: source
                    .map(|source| calendar_color_index(&source.color, source.id))
                    .unwrap_or(event.color_index),
                all_day: event.all_day,
                provider: account
                    .map(|account| account.provider.as_str())
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
        })
        .collect::<Vec<_>>();

    app.set_calendar_month_days(ModelRc::new(VecModel::from(month_days)));
    app.set_calendar_week_days(ModelRc::new(VecModel::from(week_days)));
    state
        .source_rows
        .reconcile_by(source_rows, |row| row.id, PartialEq::eq);
    app.set_calendar_events(ModelRc::new(VecModel::from(events)));
    app.set_calendar_month_title(
        format!(
            "{} {}",
            month_name(app, state.visible_month, false),
            state.visible_month.year()
        )
        .into(),
    );
    app.set_calendar_period_title(calendar_period_title(app, state).into());
    app.set_calendar_week_label(format!("W{:02}", state.selected_date.iso_week().week()).into());
    app.set_calendar_selected_date(state.selected_date.format("%Y-%m-%d").to_string().into());
    app.set_calendar_selected_date_has_events(selected_date_has_events);
    app.set_calendar_view_mode(state.view_mode.clone().into());
}

#[cfg(test)]
mod tests {
    use super::calendar_color_index;

    #[test]
    fn provider_colors_map_to_the_nearest_calendar_palette() {
        assert_eq!(calendar_color_index("#4285f4", 9), 0);
        assert_eq!(calendar_color_index("#f6bf26", 9), 2);
        assert_eq!(calendar_color_index("#33b679", 9), 3);
        assert_eq!(calendar_color_index("not-a-color", 9), 1);
    }
}
