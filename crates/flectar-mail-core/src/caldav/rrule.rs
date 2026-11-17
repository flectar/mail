//! Recurrence expansion for the RRULE subset that covers real-world calendar
//! data: FREQ=DAILY/WEEKLY/MONTHLY/YEARLY with INTERVAL, COUNT, UNTIL and
//! BYDAY (weekly day lists; monthly ordinals like 2TU / -1FR), plus EXDATE
//! and RECURRENCE-ID overrides read from the raw VCALENDAR. Anything fancier
//! falls back to the master occurrence only.
//!
//! Timed expansion iterates machine-local wall-clock time, so a weekly 09:00
//! meeting stays at 09:00 across DST transitions. All-day expansion uses UTC
//! civil dates, matching the database's timezone-neutral DATE sentinel.

use chrono::{Datelike, Duration, NaiveDateTime, TimeZone, Weekday};

use crate::calendar::{parse_dt, parse_ics};

/// Cap one requested expansion window. This must be anchored to the visible
/// window, not DTSTART: otherwise an old birthday/anniversary master can never
/// produce an occurrence in the current year.
const MAX_HORIZON_MS: i64 = 548 * 86_400_000; // ~18 months
const MAX_CANDIDATES: usize = 100_000;
const MAX_OCCURRENCES: usize = 250;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Occurrence {
    pub start: i64,
    pub end: i64,
    /// The start the series gives it, which names it (its RECURRENCE-ID)
    /// also when it was moved on its own.
    pub original: i64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Freq {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

#[derive(Debug, Clone)]
struct Rule {
    freq: Freq,
    interval: i64,
    count: Option<usize>,
    until_ms: Option<i64>,
    /// weekly: plain weekdays; monthly: (ordinal, weekday) when ordinal != 0
    bydays: Vec<(i32, Weekday)>,
    /// true when the rule has parts we don't support (BYSETPOS, BYMONTHDAY…)
    unsupported: bool,
}

fn weekday_of(s: &str) -> Option<Weekday> {
    match s {
        "MO" => Some(Weekday::Mon),
        "TU" => Some(Weekday::Tue),
        "WE" => Some(Weekday::Wed),
        "TH" => Some(Weekday::Thu),
        "FR" => Some(Weekday::Fri),
        "SA" => Some(Weekday::Sat),
        "SU" => Some(Weekday::Sun),
        _ => None,
    }
}

fn parse_rule(rrule: &str) -> Option<Rule> {
    let mut rule = Rule {
        freq: Freq::Weekly,
        interval: 1,
        count: None,
        until_ms: None,
        bydays: Vec::new(),
        unsupported: false,
    };
    let mut saw_freq = false;
    for part in rrule.split(';') {
        let Some((k, v)) = part.split_once('=') else {
            continue;
        };
        match k.trim().to_ascii_uppercase().as_str() {
            "FREQ" => {
                saw_freq = true;
                rule.freq = match v.to_ascii_uppercase().as_str() {
                    "DAILY" => Freq::Daily,
                    "WEEKLY" => Freq::Weekly,
                    "MONTHLY" => Freq::Monthly,
                    "YEARLY" => Freq::Yearly,
                    _ => {
                        rule.unsupported = true;
                        Freq::Weekly
                    }
                };
            }
            "INTERVAL" => rule.interval = v.parse().unwrap_or(1).max(1),
            "COUNT" => rule.count = v.parse().ok(),
            "UNTIL" => rule.until_ms = parse_dt(v).map(|(ms, _)| ms),
            "BYDAY" => {
                for tok in v.split(',') {
                    let tok = tok.trim().to_ascii_uppercase();
                    let (ord_str, day_str) = tok.split_at(tok.len().saturating_sub(2));
                    let Some(day) = weekday_of(day_str) else {
                        rule.unsupported = true;
                        continue;
                    };
                    let ord: i32 = if ord_str.is_empty() {
                        0
                    } else {
                        ord_str.parse().unwrap_or(0)
                    };
                    rule.bydays.push((ord, day));
                }
            }
            "WKST" | "BYHOUR" | "BYMINUTE" | "BYSECOND" => {}
            _ => rule.unsupported = true, // BYSETPOS, BYMONTHDAY, BYMONTH, …
        }
    }
    saw_freq.then_some(rule)
}

fn to_naive(ms: i64, all_day: bool) -> Option<NaiveDateTime> {
    if all_day {
        Some(chrono::Utc.timestamp_millis_opt(ms).earliest()?.naive_utc())
    } else {
        Some(
            chrono::Local
                .timestamp_millis_opt(ms)
                .earliest()?
                .naive_local(),
        )
    }
}

fn to_ms(naive: NaiveDateTime, all_day: bool) -> Option<i64> {
    if all_day {
        Some(chrono::Utc.from_utc_datetime(&naive).timestamp_millis())
    } else {
        Some(
            chrono::Local
                .from_local_datetime(&naive)
                .earliest()?
                .timestamp_millis(),
        )
    }
}

/// Nth weekday of the month (ord > 0) or from the end (ord < 0).
fn nth_weekday(year: i32, month: u32, ord: i32, day: Weekday) -> Option<chrono::NaiveDate> {
    let first = chrono::NaiveDate::from_ymd_opt(year, month, 1)?;
    let days_in_month = {
        let next = if month == 12 {
            chrono::NaiveDate::from_ymd_opt(year + 1, 1, 1)?
        } else {
            chrono::NaiveDate::from_ymd_opt(year, month + 1, 1)?
        };
        next.signed_duration_since(first).num_days() as u32
    };
    if ord > 0 {
        let offset = (7 + day.num_days_from_monday() as i64
            - first.weekday().num_days_from_monday() as i64)
            % 7;
        let dom = 1 + offset as u32 + (ord as u32 - 1) * 7;
        (dom <= days_in_month)
            .then(|| first.with_day(dom))
            .flatten()
    } else {
        let last = first.with_day(days_in_month)?;
        let offset = (7 + last.weekday().num_days_from_monday() as i64
            - day.num_days_from_monday() as i64)
            % 7;
        let dom = days_in_month as i64 - offset - 7 * (ord.unsigned_abs() as i64 - 1);
        (dom >= 1).then(|| first.with_day(dom as u32)).flatten()
    }
}

/// All candidate starts (wall clock) for the series, capped by horizon/count.
fn candidates(rule: &Rule, dtstart: NaiveDateTime, horizon: NaiveDateTime) -> Vec<NaiveDateTime> {
    let mut out = Vec::new();
    let time = dtstart.time();
    match rule.freq {
        Freq::Daily => {
            let mut d = dtstart;
            while d <= horizon && out.len() < MAX_CANDIDATES {
                out.push(d);
                d += Duration::days(rule.interval);
            }
        }
        Freq::Weekly => {
            let days: Vec<Weekday> = if rule.bydays.is_empty() {
                vec![dtstart.weekday()]
            } else {
                rule.bydays.iter().map(|(_, d)| *d).collect()
            };
            // Monday-based week containing dtstart.
            let week0 =
                dtstart.date() - Duration::days(dtstart.weekday().num_days_from_monday() as i64);
            let mut week = week0;
            'outer: loop {
                for offset in 0..7 {
                    let date = week + Duration::days(offset);
                    if !days.contains(&date.weekday()) {
                        continue;
                    }
                    let cand = date.and_time(time);
                    if cand < dtstart {
                        continue;
                    }
                    if cand > horizon || out.len() >= MAX_CANDIDATES {
                        break 'outer;
                    }
                    out.push(cand);
                }
                week += Duration::days(7 * rule.interval);
                if week.and_time(time) > horizon {
                    break;
                }
            }
        }
        Freq::Monthly => {
            let by_ord = rule.bydays.iter().find(|(ord, _)| *ord != 0);
            let mut year = dtstart.year();
            let mut month = dtstart.month();
            let mut i = 0;
            loop {
                let date = match by_ord {
                    Some((ord, day)) => nth_weekday(year, month, *ord, *day),
                    None => chrono::NaiveDate::from_ymd_opt(year, month, dtstart.day()),
                };
                if let Some(date) = date {
                    let cand = date.and_time(time);
                    if cand >= dtstart {
                        if cand > horizon || out.len() >= MAX_CANDIDATES {
                            break;
                        }
                        out.push(cand);
                    }
                }
                i += 1;
                if i > MAX_CANDIDATES {
                    break;
                }
                let total = year as i64 * 12 + month as i64 - 1 + rule.interval;
                year = (total / 12) as i32;
                month = (total % 12) as u32 + 1;
                if chrono::NaiveDate::from_ymd_opt(year, month, 1)
                    .map(|d| d.and_time(time) > horizon)
                    .unwrap_or(true)
                {
                    break;
                }
            }
        }
        Freq::Yearly => {
            let mut year = dtstart.year();
            loop {
                if let Some(date) =
                    chrono::NaiveDate::from_ymd_opt(year, dtstart.month(), dtstart.day())
                {
                    let cand = date.and_time(time);
                    if cand >= dtstart {
                        if cand > horizon || out.len() >= MAX_CANDIDATES {
                            break;
                        }
                        out.push(cand);
                    }
                }
                year += rule.interval as i32;
                if out.len() >= MAX_CANDIDATES {
                    break;
                }
            }
        }
    }
    out
}

/// Expand a recurring master into concrete occurrences overlapping
/// [window_start, window_end). `ical_raw` (when present) supplies EXDATEs and
/// RECURRENCE-ID overrides. Returns None when the rule is unsupported (caller
/// shows the master only).
pub fn expand(
    rrule: &str,
    dtstart_ms: i64,
    duration_ms: i64,
    all_day: bool,
    ical_raw: Option<&str>,
    window_start: i64,
    window_end: i64,
) -> Option<Vec<Occurrence>> {
    let rule = parse_rule(rrule)?;
    if rule.unsupported {
        return None;
    }
    // Monthly BYDAY without ordinal ("every month on Tuesday"?) is really a
    // weekly-ish rule we don't model; bail to master-only.
    if rule.freq == Freq::Monthly && rule.bydays.iter().any(|(ord, _)| *ord == 0) {
        return None;
    }

    let dtstart = to_naive(dtstart_ms, all_day)?;
    let horizon_anchor = window_start.max(dtstart_ms);
    let horizon_ms = horizon_anchor
        .saturating_add(MAX_HORIZON_MS)
        .min(window_end.max(dtstart_ms));
    let horizon = to_naive(horizon_ms, all_day)?;

    // EXDATE + overrides from the raw calendar.
    let mut exdates: Vec<i64> = Vec::new();
    let mut overrides: Vec<(i64, i64, i64)> = Vec::new(); // (orig_start, new_start, new_end)
    if let Some(raw) = ical_raw {
        for line in unfolded_lines(raw) {
            if let Some(rest) = line.strip_prefix("EXDATE:").or_else(|| {
                line.split_once(':')
                    .and_then(|(k, v)| k.starts_with("EXDATE;").then_some(v))
            }) {
                for tok in rest.split(',') {
                    if let Some((ms, _)) = parse_dt(tok) {
                        exdates.push(ms);
                    }
                }
            }
        }
        for ev in parse_ics(raw) {
            if let Some(orig) = ev.recurrence_id_ms {
                let end = ev.ends_at_ms.unwrap_or(ev.starts_at_ms + duration_ms);
                if ev.status.as_deref() == Some("CANCELLED") {
                    exdates.push(orig);
                } else {
                    overrides.push((orig, ev.starts_at_ms, end));
                }
            }
        }
    }

    let mut occurrences: Vec<Occurrence> = Vec::new();
    let cands = candidates(&rule, dtstart, horizon);
    for (i, cand) in cands.iter().enumerate() {
        if let Some(count) = rule.count
            && i >= count
        {
            break;
        }
        let Some(start) = to_ms(*cand, all_day) else {
            continue;
        };
        if let Some(until) = rule.until_ms
            && start > until
        {
            break;
        }
        if exdates.iter().any(|ex| same_occurrence(*ex, start)) {
            continue;
        }
        let original = start;
        let (start, end) = match overrides
            .iter()
            .find(|(orig, _, _)| same_occurrence(*orig, start))
        {
            Some((_, s, e)) => (*s, *e),
            None => (start, start + duration_ms),
        };
        if start < window_end && end > window_start {
            occurrences.push(Occurrence {
                start,
                end,
                original,
            });
            if occurrences.len() >= MAX_OCCURRENCES {
                break;
            }
        }
    }
    Some(occurrences)
}

/// EXDATE/RECURRENCE-ID values may be date-only or differ by TZ rendering;
/// treat anything within the same civil day + exact-time matches as the same
/// occurrence when exact, else compare with minute tolerance.
fn same_occurrence(a: i64, b: i64) -> bool {
    (a - b).abs() < 60_000
}

fn unfolded_lines(text: &str) -> Vec<std::borrow::Cow<'_, str>> {
    let mut out: Vec<std::borrow::Cow<'_, str>> = Vec::new();
    for raw in text.lines() {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if (line.starts_with(' ') || line.starts_with('\t')) && !out.is_empty() {
            if let Some(previous) = out.last_mut() {
                previous.to_mut().push_str(&line[1..]);
            }
        } else {
            out.push(std::borrow::Cow::Borrowed(line));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn local_ms(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> i64 {
        chrono::Local
            .from_local_datetime(
                &NaiveDate::from_ymd_opt(y, mo, d)
                    .unwrap()
                    .and_hms_opt(h, mi, 0)
                    .unwrap(),
            )
            .earliest()
            .unwrap()
            .timestamp_millis()
    }

    const HOUR: i64 = 3_600_000;
    const DAY: i64 = 86_400_000;

    #[test]
    fn daily_count() {
        let start = local_ms(2026, 7, 1, 9, 0);
        let occ = expand(
            "FREQ=DAILY;COUNT=3",
            start,
            HOUR,
            false,
            None,
            0,
            start + 30 * DAY,
        )
        .unwrap();
        assert_eq!(occ.len(), 3);
        assert_eq!(occ[0].start, start);
        assert_eq!(occ[2].start, local_ms(2026, 7, 3, 9, 0));
        assert_eq!(occ[0].end - occ[0].start, HOUR);
    }

    #[test]
    fn weekly_byday_interval() {
        // Wed Jul 1 2026; every 2 weeks on Mon+Fri.
        let start = local_ms(2026, 7, 1, 10, 0);
        let occ = expand(
            "FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,FR",
            start,
            HOUR,
            false,
            None,
            0,
            local_ms(2026, 8, 1, 0, 0),
        )
        .unwrap();
        // Week of Jun 29: Fri Jul 3 (Mon Jun 29 predates dtstart). Then week
        // of Jul 13: Mon 13, Fri 17. Then week of Jul 27: Mon 27, Fri 31.
        let starts: Vec<i64> = occ.iter().map(|o| o.start).collect();
        assert_eq!(
            starts,
            vec![
                local_ms(2026, 7, 3, 10, 0),
                local_ms(2026, 7, 13, 10, 0),
                local_ms(2026, 7, 17, 10, 0),
                local_ms(2026, 7, 27, 10, 0),
                local_ms(2026, 7, 31, 10, 0),
            ]
        );
    }

    #[test]
    fn weekly_keeps_wall_clock_across_dst() {
        // Weekly on the dtstart weekday from late October (DST ends Oct 25
        // 2026 in Europe; if the host TZ has no DST this still passes since
        // wall-clock arithmetic is used throughout).
        let start = local_ms(2026, 10, 20, 9, 0);
        let occ = expand(
            "FREQ=WEEKLY;COUNT=3",
            start,
            HOUR,
            false,
            None,
            0,
            start + 40 * DAY,
        )
        .unwrap();
        assert_eq!(occ.len(), 3);
        for (i, o) in occ.iter().enumerate() {
            let dt = to_naive(o.start, false).unwrap();
            assert_eq!(
                dt.time(),
                chrono::NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
                "occ {i}"
            );
        }
    }

    #[test]
    fn yearly_all_day_birthday_reaches_the_visible_year() {
        let start = chrono::Utc
            .with_ymd_and_hms(1990, 8, 4, 0, 0, 0)
            .unwrap()
            .timestamp_millis();
        let window_start = chrono::Utc
            .with_ymd_and_hms(2026, 8, 1, 0, 0, 0)
            .unwrap()
            .timestamp_millis();
        let window_end = chrono::Utc
            .with_ymd_and_hms(2026, 9, 1, 0, 0, 0)
            .unwrap()
            .timestamp_millis();
        let occurrences = expand(
            "FREQ=YEARLY",
            start,
            DAY,
            true,
            None,
            window_start,
            window_end,
        )
        .unwrap();
        assert_eq!(occurrences.len(), 1);
        assert_eq!(
            occurrences[0].start,
            chrono::Utc
                .with_ymd_and_hms(2026, 8, 4, 0, 0, 0)
                .unwrap()
                .timestamp_millis()
        );
        assert_eq!(occurrences[0].end - occurrences[0].start, DAY);
    }

    #[test]
    fn monthly_ordinal_and_last() {
        // 2nd Tuesday monthly from Jul 2026.
        let start = local_ms(2026, 7, 14, 15, 0);
        let occ = expand(
            "FREQ=MONTHLY;BYDAY=2TU;COUNT=3",
            start,
            HOUR,
            false,
            None,
            0,
            start + 200 * DAY,
        )
        .unwrap();
        let starts: Vec<i64> = occ.iter().map(|o| o.start).collect();
        assert_eq!(
            starts,
            vec![
                local_ms(2026, 7, 14, 15, 0),
                local_ms(2026, 8, 11, 15, 0),
                local_ms(2026, 9, 8, 15, 0),
            ]
        );
        // Last Friday.
        let start = local_ms(2026, 7, 31, 8, 0);
        let occ = expand(
            "FREQ=MONTHLY;BYDAY=-1FR;COUNT=2",
            start,
            HOUR,
            false,
            None,
            0,
            start + 100 * DAY,
        )
        .unwrap();
        assert_eq!(occ[1].start, local_ms(2026, 8, 28, 8, 0));
    }

    #[test]
    fn monthly_day_31_skips_short_months() {
        let start = local_ms(2026, 1, 31, 12, 0);
        let occ = expand(
            "FREQ=MONTHLY;COUNT=4",
            start,
            HOUR,
            false,
            None,
            0,
            start + 200 * DAY,
        )
        .unwrap();
        let starts: Vec<i64> = occ.iter().map(|o| o.start).collect();
        // Feb/Apr have no 31st; COUNT applies to emitted candidates.
        assert_eq!(
            starts,
            vec![
                local_ms(2026, 1, 31, 12, 0),
                local_ms(2026, 3, 31, 12, 0),
                local_ms(2026, 5, 31, 12, 0),
                local_ms(2026, 7, 31, 12, 0),
            ]
        );
    }

    #[test]
    fn until_bound_is_inclusive() {
        let start = local_ms(2026, 7, 1, 9, 0);
        let until = chrono::Local
            .timestamp_millis_opt(local_ms(2026, 7, 3, 9, 0))
            .earliest()
            .unwrap()
            .with_timezone(&chrono::Utc)
            .format("%Y%m%dT%H%M%SZ")
            .to_string();
        let occ = expand(
            &format!("FREQ=DAILY;UNTIL={until}"),
            start,
            HOUR,
            false,
            None,
            0,
            start + 30 * DAY,
        )
        .unwrap();
        assert_eq!(occ.len(), 3);
    }

    #[test]
    fn exdate_and_override() {
        let start = local_ms(2026, 7, 6, 9, 0); // Monday
        let ex = chrono::Local
            .timestamp_millis_opt(local_ms(2026, 7, 13, 9, 0))
            .earliest()
            .unwrap()
            .with_timezone(&chrono::Utc)
            .format("%Y%m%dT%H%M%SZ")
            .to_string();
        let ovr_orig = chrono::Local
            .timestamp_millis_opt(local_ms(2026, 7, 20, 9, 0))
            .earliest()
            .unwrap()
            .with_timezone(&chrono::Utc)
            .format("%Y%m%dT%H%M%SZ")
            .to_string();
        let raw = format!(
            "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:m1\r\nDTSTART:20260706T090000\r\nRRULE:FREQ=WEEKLY\r\nEXDATE:{ex}\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:m1\r\nRECURRENCE-ID:{ovr_orig}\r\nDTSTART:20260721T140000\r\nDTEND:20260721T150000\r\nSUMMARY:moved\r\nEND:VEVENT\r\nEND:VCALENDAR"
        );
        let occ = expand(
            "FREQ=WEEKLY",
            start,
            HOUR,
            false,
            Some(&raw),
            0,
            local_ms(2026, 7, 28, 0, 0),
        )
        .unwrap();
        let starts: Vec<i64> = occ.iter().map(|o| o.start).collect();
        assert!(starts.contains(&local_ms(2026, 7, 6, 9, 0)));
        assert!(
            !starts.contains(&local_ms(2026, 7, 13, 9, 0)),
            "EXDATE skipped"
        );
        assert!(
            !starts.contains(&local_ms(2026, 7, 20, 9, 0)),
            "overridden slot moved"
        );
        assert!(
            starts.contains(&local_ms(2026, 7, 21, 14, 0)),
            "override present"
        );
    }

    #[test]
    fn unsupported_rules_bail_to_master() {
        let start = local_ms(2026, 7, 1, 9, 0);
        assert!(
            expand(
                "FREQ=WEEKLY;BYSETPOS=2",
                start,
                HOUR,
                false,
                None,
                0,
                i64::MAX / 2
            )
            .is_none()
        );
        assert!(expand("FREQ=HOURLY", start, HOUR, false, None, 0, i64::MAX / 2).is_none());
        assert!(expand("COUNT=3", start, HOUR, false, None, 0, i64::MAX / 2).is_none());
        // no FREQ
    }

    #[test]
    fn horizon_and_occurrence_caps() {
        let start = local_ms(2026, 1, 1, 9, 0);
        let occ = expand(
            "FREQ=DAILY",
            start,
            HOUR,
            false,
            None,
            0,
            start + 10_000 * DAY,
        )
        .unwrap();
        assert!(occ.len() <= MAX_OCCURRENCES);
        let last = occ.last().unwrap().start;
        assert!(last <= start + MAX_HORIZON_MS);
    }

    #[test]
    fn window_filtering() {
        let start = local_ms(2026, 7, 1, 9, 0);
        let occ = expand(
            "FREQ=DAILY",
            start,
            HOUR,
            false,
            None,
            local_ms(2026, 7, 10, 0, 0),
            local_ms(2026, 7, 12, 0, 0),
        )
        .unwrap();
        assert_eq!(occ.len(), 2); // Jul 10 + Jul 11
    }
}
