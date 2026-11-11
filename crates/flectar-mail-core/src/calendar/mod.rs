//! Minimal iCalendar (RFC 5545) parsing and generation - enough for meeting
//! invites carried in text/calendar MIME parts: VEVENT with UID/SUMMARY/
//! DTSTART/DTEND/LOCATION/ORGANIZER/STATUS/DESCRIPTION/ATTENDEE plus the
//! calendar-level METHOD, and building REQUEST (new invite) / REPLY (RSVP)
//! calendars for outbound mail.

use crate::models::Address;
use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, TimeZone, Utc, Weekday};
use std::{borrow::Cow, collections::HashMap};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct IcsAttendee {
    pub email: String,
    pub name: Option<String>,
    /// NEEDS-ACTION | ACCEPTED | TENTATIVE | DECLINED
    pub partstat: Option<String>,
    /// The organizer asked this attendee to reply (RSVP=TRUE). RFC 5545
    /// makes FALSE the default.
    pub rsvp: bool,
    /// REQ-PARTICIPANT, OPT-PARTICIPANT, NON-PARTICIPANT or CHAIR.
    pub role: Option<String>,
    /// INDIVIDUAL, GROUP, RESOURCE, ROOM or UNKNOWN.
    pub cutype: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct IcsEvent {
    pub uid: String,
    pub method: Option<String>,
    pub summary: Option<String>,
    pub location: Option<String>,
    pub organizer: Option<String>,
    /// The organizer's display name (CN).
    pub organizer_name: Option<String>,
    /// When this version of the event was written (DTSTAMP). It orders two
    /// versions with the same SEQUENCE (RFC 5546 §2.1.5).
    pub dtstamp_ms: Option<i64>,
    pub description: Option<String>,
    pub attendees: Vec<IcsAttendee>,
    /// Video-call link: explicit URL/X-GOOGLE-CONFERENCE property, or the
    /// first meeting-service URL found in location/description.
    pub join_url: Option<String>,
    pub sequence: i64,
    /// Raw RRULE value (e.g. "FREQ=WEEKLY;BYDAY=MO"); None = one-off event.
    pub rrule: Option<String>,
    /// TZID parameter of DTSTART, informational.
    pub tzid: Option<String>,
    /// For recurrence overrides: the original occurrence start this VEVENT
    /// replaces (RECURRENCE-ID).
    pub recurrence_id_ms: Option<i64>,
    pub starts_at_ms: i64,
    pub ends_at_ms: Option<i64>,
    pub all_day: bool,
    pub status: Option<String>,
}

/// Unfold RFC 5545 folded lines (continuations start with space or tab).
fn unfold(text: &str) -> Vec<Cow<'_, str>> {
    let mut out: Vec<Cow<'_, str>> = Vec::new();
    for raw in text.lines() {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if (line.starts_with(' ') || line.starts_with('\t')) && !out.is_empty() {
            let cont = &line[1..];
            if let Some(previous) = out.last_mut() {
                previous.to_mut().push_str(cont);
            }
        } else {
            out.push(Cow::Borrowed(line));
        }
    }
    out
}

/// "KEY;PARAM=X;PARAM="Y":VALUE" -> (KEY, [(PARAM, X)…], VALUE). The name/
/// param section ends at the first ':' outside double quotes.
type ParsedProperty<'a> = (String, Vec<(String, Cow<'a, str>)>, &'a str);

fn split_prop(line: &str) -> Option<ParsedProperty<'_>> {
    let mut in_quotes = false;
    let mut colon = None;
    for (i, ch) in line.char_indices() {
        match ch {
            '"' => in_quotes = !in_quotes,
            ':' if !in_quotes => {
                colon = Some(i);
                break;
            }
            _ => {}
        }
    }
    let colon = colon?;
    let (lhs, value) = (&line[..colon], &line[colon + 1..]);
    // Split the name/param section on ';' outside double quotes.
    let mut parts: Vec<Cow<'_, str>> = Vec::new();
    let mut start = 0;
    let mut quoted = false;
    for (byte, ch) in lhs.char_indices() {
        match ch {
            '"' => quoted = !quoted,
            ';' if !quoted => {
                parts.push(parameter_segment(&lhs[start..byte]));
                start = byte + 1;
            }
            _ => {}
        }
    }
    parts.push(parameter_segment(&lhs[start..]));
    let mut parts = parts.into_iter();
    let key = parts.next()?.to_ascii_uppercase();
    let params = parts
        .filter_map(|part| match part {
            Cow::Borrowed(part) => {
                let (key, value) = part.split_once('=')?;
                Some((
                    key.to_ascii_uppercase(),
                    Cow::Borrowed(value.trim_matches('"')),
                ))
            }
            Cow::Owned(part) => {
                let (key, value) = part.split_once('=')?;
                Some((
                    key.to_ascii_uppercase(),
                    Cow::Owned(value.trim_matches('"').to_owned()),
                ))
            }
        })
        .collect();
    Some((key, params, value))
}

fn parameter_segment(segment: &str) -> Cow<'_, str> {
    if segment.contains('"') {
        Cow::Owned(segment.replace('"', ""))
    } else {
        Cow::Borrowed(segment)
    }
}

fn param<'a>(params: &'a [(String, Cow<'_, str>)], key: &str) -> Option<&'a str> {
    params
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_ref())
}

/// Parse "20260711", "20260711T130000", "20260711T130000Z" to (ms, all_day).
/// A bare naive datetime is treated as machine-local; callers with a TZID and a
/// VTIMEZONE table use [`resolve_dt`] instead so the invite's own zone wins.
pub(crate) fn parse_dt(value: &str) -> Option<(i64, bool)> {
    let v = value.trim();
    if let Ok(d) = NaiveDate::parse_from_str(v, "%Y%m%d") {
        let dt = d.and_hms_opt(0, 0, 0)?;
        // RFC 5545 DATE values are civil dates, not instants. The database
        // stores them at UTC midnight as a timezone-neutral sentinel so an
        // all-day event cannot move to the previous day in positive UTC
        // offsets (or change dates when the user travels).
        let ms = Utc.from_utc_datetime(&dt).timestamp_millis();
        return Some((ms, true));
    }
    if let Some(stripped) = v.strip_suffix('Z') {
        let dt = NaiveDateTime::parse_from_str(stripped, "%Y%m%dT%H%M%S").ok()?;
        return Some((Utc.from_utc_datetime(&dt).timestamp_millis(), false));
    }
    let dt = NaiveDateTime::parse_from_str(v, "%Y%m%dT%H%M%S").ok()?;
    Some((
        chrono::Local
            .from_local_datetime(&dt)
            .earliest()?
            .timestamp_millis(),
        false,
    ))
}

/// One STANDARD/DAYLIGHT observance from a VTIMEZONE: the UTC offset it applies
/// and, for zones with DST, the yearly rule for when it takes effect.
#[derive(Debug, Clone)]
struct TzObservance {
    /// Offset from UTC in seconds (TZOFFSETTO). Wall clock = UTC + this.
    offset_to: i32,
    /// Transition month (BYMONTH), 1-12. None for a fixed, ruleless observance.
    month: Option<u32>,
    /// Nth weekday of the month (BYDAY): 1..=5, or -1 for "last".
    nth: i32,
    weekday: Option<Weekday>,
    /// Seconds past midnight the switch happens (from the observance DTSTART).
    time_sec: u32,
}

/// Parse "+0700" / "-0430" (±HHMM) to an offset in seconds.
fn parse_offset(v: &str) -> Option<i32> {
    let v = v.trim();
    let (sign, rest) = match v.strip_prefix('-') {
        Some(r) => (-1, r),
        None => (1, v.strip_prefix('+').unwrap_or(v)),
    };
    if rest.len() < 4 {
        return None;
    }
    let h: i32 = rest[0..2].parse().ok()?;
    let m: i32 = rest[2..4].parse().ok()?;
    Some(sign * (h * 3600 + m * 60))
}

/// Parse a BYDAY token like "2SU" or "-1SU" into (nth, weekday).
fn parse_byday(v: &str) -> Option<(i32, Weekday)> {
    let v = v.trim();
    let split = v.len().saturating_sub(2);
    let (num, day) = v.split_at(split);
    let weekday = match day.to_ascii_uppercase().as_str() {
        "SU" => Weekday::Sun,
        "MO" => Weekday::Mon,
        "TU" => Weekday::Tue,
        "WE" => Weekday::Wed,
        "TH" => Weekday::Thu,
        "FR" => Weekday::Fri,
        "SA" => Weekday::Sat,
        _ => return None,
    };
    let nth = if num.is_empty() { 1 } else { num.parse().ok()? };
    Some((nth, weekday))
}

/// Date of the nth `weekday` in `year`/`month` (nth = -1 means the last one).
fn nth_weekday(year: i32, month: u32, weekday: Weekday, nth: i32) -> Option<NaiveDate> {
    if nth < 0 {
        // Walk back from the last day of the month.
        let first_next = if month == 12 {
            NaiveDate::from_ymd_opt(year + 1, 1, 1)?
        } else {
            NaiveDate::from_ymd_opt(year, month + 1, 1)?
        };
        let mut d = first_next.pred_opt()?;
        while d.weekday() != weekday {
            d = d.pred_opt()?;
        }
        return Some(d);
    }
    let first = NaiveDate::from_ymd_opt(year, month, 1)?;
    let shift = (7 + weekday.num_days_from_sunday() as i64
        - first.weekday().num_days_from_sunday() as i64)
        % 7;
    let day = 1 + shift + (nth as i64 - 1) * 7;
    NaiveDate::from_ymd_opt(year, month, day as u32)
}

/// Collect VTIMEZONE definitions into TZID -> observances. Uses the offsets the
/// invite carries in-band, so we never need a Windows/IANA zone-name table.
fn parse_vtimezones(lines: &[Cow<'_, str>]) -> HashMap<String, Vec<TzObservance>> {
    let mut map: HashMap<String, Vec<TzObservance>> = HashMap::new();
    let mut tzid: Option<String> = None;
    let mut in_sub = false;
    let mut cur: Option<TzObservance> = None;

    for line in lines {
        let Some((key, _params, value)) = split_prop(line) else {
            continue;
        };
        match key.as_str() {
            "BEGIN" if value.eq_ignore_ascii_case("VTIMEZONE") => tzid = Some(String::new()),
            "END" if value.eq_ignore_ascii_case("VTIMEZONE") => tzid = None,
            "TZID" if tzid.is_some() && !in_sub => tzid = Some(value.trim().to_string()),
            "BEGIN"
                if value.eq_ignore_ascii_case("STANDARD")
                    || value.eq_ignore_ascii_case("DAYLIGHT") =>
            {
                in_sub = true;
                cur = Some(TzObservance {
                    offset_to: 0,
                    month: None,
                    nth: 1,
                    weekday: None,
                    time_sec: 0,
                });
            }
            "END"
                if value.eq_ignore_ascii_case("STANDARD")
                    || value.eq_ignore_ascii_case("DAYLIGHT") =>
            {
                if let (Some(id), Some(obs)) = (tzid.as_deref(), cur.take())
                    && !id.is_empty()
                {
                    map.entry(id.to_string()).or_default().push(obs);
                }
                in_sub = false;
            }
            "TZOFFSETTO" if in_sub => {
                if let (Some(c), Some(off)) = (cur.as_mut(), parse_offset(value)) {
                    c.offset_to = off;
                }
            }
            "DTSTART" if in_sub => {
                if let (Some(c), Some(t)) = (cur.as_mut(), value.trim().split_once('T')) {
                    let hms = t.1;
                    if hms.len() >= 6 {
                        let h: u32 = hms[0..2].parse().unwrap_or(0);
                        let m: u32 = hms[2..4].parse().unwrap_or(0);
                        let s: u32 = hms[4..6].parse().unwrap_or(0);
                        c.time_sec = h * 3600 + m * 60 + s;
                    }
                }
            }
            "RRULE" if in_sub => {
                if let Some(c) = cur.as_mut() {
                    for kv in value.split(';') {
                        match kv.split_once('=') {
                            Some((k, v)) if k.eq_ignore_ascii_case("BYMONTH") => {
                                c.month = v.trim().parse().ok();
                            }
                            Some((k, v)) if k.eq_ignore_ascii_case("BYDAY") => {
                                if let Some((n, wd)) = parse_byday(v) {
                                    c.nth = n;
                                    c.weekday = Some(wd);
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            _ => {}
        }
    }
    map
}

/// The UTC offset (seconds) a zone applies at a given wall-clock instant. For a
/// no-DST zone (one observance, or all offsets equal) this is constant; for a
/// DST zone we pick the observance whose yearly transition most recently
/// preceded `local`, which is the standard "which side of the boundary" rule.
fn offset_for(obs: &[TzObservance], local: NaiveDateTime) -> Option<i32> {
    match obs.len() {
        0 => None,
        1 => Some(obs[0].offset_to),
        _ => {
            let year = local.year();
            // Transition wall-clock time for each observance in this year.
            let mut with_trans: Vec<(NaiveDateTime, i32)> = obs
                .iter()
                .filter_map(|o| {
                    let (month, wd) = (o.month?, o.weekday?);
                    let date = nth_weekday(year, month, wd, o.nth)?;
                    let dt = date.and_hms_opt(0, 0, 0)? + Duration::seconds(o.time_sec as i64);
                    Some((dt, o.offset_to))
                })
                .collect();
            if with_trans.len() < 2 {
                // Missing rules: fall back to the first offset.
                return Some(obs[0].offset_to);
            }
            with_trans.sort_by_key(|(dt, _)| *dt);
            // Latest transition at or before `local`; before the year's first
            // transition, the last one (previous year's) is still in effect.

            with_trans
                .iter()
                .rev()
                .find(|(dt, _)| *dt <= local)
                .or_else(|| with_trans.last())
                .map(|(_, off)| *off)
        }
    }
}

/// Resolve a DTSTART/DTEND value honoring its TZID against the invite's
/// VTIMEZONE table. UTC (`…Z`) and date-only values ignore the TZID; a floating
/// value with a known TZID is converted from that zone's offset; anything else
/// falls back to [`parse_dt`]'s machine-local interpretation.
fn resolve_dt(
    value: &str,
    tzid: Option<&str>,
    tztable: &HashMap<String, Vec<TzObservance>>,
) -> Option<(i64, bool)> {
    let v = value.trim();
    // UTC and date-only forms are unambiguous; TZID does not apply.
    if v.ends_with('Z') || !v.contains('T') {
        return parse_dt(v);
    }
    if let Some(obs) = tzid.and_then(|id| tztable.get(id))
        && let Ok(naive) = NaiveDateTime::parse_from_str(v, "%Y%m%dT%H%M%S")
        && let Some(off) = offset_for(obs, naive)
    {
        let as_utc = Utc.from_utc_datetime(&naive).timestamp_millis();
        return Some((as_utc - off as i64 * 1000, false));
    }
    parse_dt(v)
}

/// The address in a CAL-ADDRESS value such as an ORGANIZER or ATTENDEE:
/// the "mailto:" scheme comes off in any case.
fn cal_address(value: &str) -> String {
    let value = value.trim();
    match value.get(..7) {
        Some(scheme) if scheme.eq_ignore_ascii_case("mailto:") => value[7..].trim().to_string(),
        _ => value.to_string(),
    }
}

/// An RFC 5545 DURATION ("PT1H30M", "P1D", "P2W", "-PT15M") in milliseconds.
/// None for anything else.
fn parse_duration(value: &str) -> Option<i64> {
    let value = value.trim();
    let (sign, value) = match value.strip_prefix('-') {
        Some(rest) => (-1, rest),
        None => (1, value.strip_prefix('+').unwrap_or(value)),
    };
    let mut rest = value.strip_prefix('P')?;
    let mut total = 0i64;
    let mut in_time = false;
    let mut parts = 0;
    while !rest.is_empty() {
        if let Some(time) = rest.strip_prefix('T') {
            if in_time {
                return None;
            }
            in_time = true;
            rest = time;
            continue;
        }
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        let number: i64 = rest[..digits].parse().ok()?;
        let unit = match (rest[digits..].chars().next()?, in_time) {
            ('W', false) => 7 * 86_400_000,
            ('D', false) => 86_400_000,
            ('H', true) => 3_600_000,
            ('M', true) => 60_000,
            ('S', true) => 1_000,
            _ => return None,
        };
        total = total.checked_add(number.checked_mul(unit)?)?;
        rest = &rest[digits + 1..];
        parts += 1;
    }
    (parts > 0).then_some(sign * total)
}

fn unescape(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut chars = v.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') | Some('N') => out.push('\n'),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

/// Hosts whose URLs are treated as video-call join links.
const MEETING_HOSTS: &[&str] = &[
    "zoom.us",
    "meet.google.com",
    "teams.microsoft.com",
    "teams.live.com",
    "webex.com",
    "whereby.com",
    "meet.jit.si",
];

/// First meeting-service URL inside free text (location or description).
pub fn find_join_url(text: &str) -> Option<String> {
    for (idx, _) in text.match_indices("https://") {
        let rest = &text[idx..];
        let end = rest
            .find(|c: char| c.is_whitespace() || matches!(c, '>' | '"' | '\'' | ')' | ','))
            .unwrap_or(rest.len());
        let url = rest[..end].trim_end_matches(['.', ';']);
        if MEETING_HOSTS.iter().any(|h| {
            url.split('/')
                .nth(2)
                .is_some_and(|host| host == *h || host.ends_with(&format!(".{h}")))
        }) {
            return Some(url.to_string());
        }
    }
    None
}

pub fn parse_ics(text: &str) -> Vec<IcsEvent> {
    let lines = unfold(text);
    let tztable = parse_vtimezones(&lines);
    let mut method: Option<String> = None;
    let mut events = Vec::new();
    let mut current: Option<IcsEvent> = None;
    // Components inside the VEVENT being read, such as a VALARM, whose
    // properties are not the event's.
    let mut nested = 0usize;
    let mut duration_ms: Option<i64> = None;

    for line in &lines {
        let Some((key, params, value)) = split_prop(line) else {
            continue;
        };
        match key.as_str() {
            "BEGIN" if current.is_none() && value.eq_ignore_ascii_case("VEVENT") => {
                current = Some(IcsEvent::default());
                nested = 0;
                duration_ms = None;
            }
            "BEGIN" if current.is_some() => nested += 1,
            "END" if current.is_some() && nested > 0 => nested -= 1,
            "END" if value.eq_ignore_ascii_case("VEVENT") => {
                if let Some(mut ev) = current.take() {
                    ev.method = method.clone();
                    if ev.ends_at_ms.is_none()
                        && let Some(duration) = duration_ms
                    {
                        ev.ends_at_ms = ev.starts_at_ms.checked_add(duration);
                    }
                    if ev.join_url.is_none() {
                        ev.join_url = ev
                            .location
                            .as_deref()
                            .and_then(find_join_url)
                            .or_else(|| ev.description.as_deref().and_then(find_join_url));
                    }
                    // Answers and cancellations may name an event by its
                    // UID alone (RFC 5546 §3.2); anything else needs a start.
                    let names_only = matches!(
                        method.as_deref(),
                        Some("REPLY" | "CANCEL" | "REFRESH" | "COUNTER" | "DECLINECOUNTER")
                    );
                    if !ev.uid.is_empty() && (ev.starts_at_ms != 0 || names_only) {
                        events.push(ev);
                    }
                }
            }
            "METHOD" if current.is_none() => method = Some(value.trim().to_ascii_uppercase()),
            _ if nested > 0 => {}
            _ => {
                let Some(ev) = current.as_mut() else { continue };
                match key.as_str() {
                    "UID" => ev.uid = value.trim().to_string(),
                    "DTSTAMP" => ev.dtstamp_ms = parse_dt(value).map(|(ms, _)| ms),
                    "DURATION" => duration_ms = parse_duration(value),
                    "SUMMARY" => ev.summary = Some(unescape(value.trim())),
                    "LOCATION" => ev.location = Some(unescape(value.trim())),
                    "DESCRIPTION" => ev.description = Some(unescape(value.trim())),
                    "STATUS" => ev.status = Some(value.trim().to_ascii_uppercase()),
                    "SEQUENCE" => ev.sequence = value.trim().parse().unwrap_or(0),
                    "RRULE" => ev.rrule = Some(value.trim().to_string()),
                    "RECURRENCE-ID" => {
                        let tzid = param(&params, "TZID");
                        ev.recurrence_id_ms = resolve_dt(value, tzid, &tztable).map(|(ms, _)| ms);
                    }
                    "EXDATE" => {
                        // Collected by the recurrence expander from the raw
                        // VCALENDAR; nothing to store on the master here.
                    }
                    "URL" | "X-GOOGLE-CONFERENCE" => {
                        if ev.join_url.is_none() {
                            let v = value.trim();
                            if v.starts_with("http") {
                                ev.join_url = Some(v.to_string());
                            }
                        }
                    }
                    "ORGANIZER" => {
                        ev.organizer = Some(cal_address(value));
                        ev.organizer_name = param(&params, "CN").map(str::to_string);
                    }
                    "ATTENDEE" => {
                        let email = cal_address(value);
                        if !email.is_empty() {
                            let upper = |key| param(&params, key).map(str::to_ascii_uppercase);
                            ev.attendees.push(IcsAttendee {
                                email,
                                name: param(&params, "CN").map(str::to_string),
                                partstat: upper("PARTSTAT"),
                                rsvp: param(&params, "RSVP")
                                    .is_some_and(|rsvp| rsvp.eq_ignore_ascii_case("TRUE")),
                                role: upper("ROLE"),
                                cutype: upper("CUTYPE"),
                            });
                        }
                    }
                    "DTSTART" => {
                        let tzid = param(&params, "TZID");
                        if let Some((ms, all_day)) = resolve_dt(value, tzid, &tztable) {
                            ev.starts_at_ms = ms;
                            ev.all_day = all_day;
                        }
                        if let Some(tz) = tzid {
                            ev.tzid = Some(tz.to_string());
                        }
                    }
                    "DTEND" => {
                        let tzid = param(&params, "TZID");
                        if let Some((ms, _)) = resolve_dt(value, tzid, &tztable) {
                            ev.ends_at_ms = Some(ms);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    events
}

pub(crate) fn escape(v: &str) -> String {
    let capacity = v
        .chars()
        .map(|character| match character {
            '\\' | ',' | ';' | '\n' => 2,
            '\r' => 0,
            _ => character.len_utf8(),
        })
        .sum();
    let mut out = String::with_capacity(capacity);
    for c in v.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            ',' => out.push_str("\\,"),
            ';' => out.push_str("\\;"),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            _ => out.push(c),
        }
    }
    out
}

pub(crate) fn fmt_utc(ms: i64) -> String {
    Utc.timestamp_millis_opt(ms)
        .earliest()
        .unwrap_or_else(Utc::now)
        .format("%Y%m%dT%H%M%SZ")
        .to_string()
}

/// Fold a content line at 74 octets per RFC 5545 §3.1 (continuations get a
/// leading space). Splits on char boundaries, so multi-byte text stays valid.
pub(crate) fn fold(line: &str, out: &mut String) {
    fold_parts([line], out);
}

fn fold_parts<'a>(parts: impl IntoIterator<Item = &'a str>, out: &mut String) {
    const LIMIT: usize = 74;
    let mut budget = LIMIT;
    let mut used = 0;
    for c in parts.into_iter().flat_map(str::chars) {
        let w = c.len_utf8();
        if used + w > budget {
            out.push_str("\r\n ");
            budget = LIMIT - 1;
            used = 0;
        }
        out.push(c);
        used += w;
    }
    out.push_str("\r\n");
}

pub(crate) fn push_prop(out: &mut String, key: &str, value: &str) {
    fold_parts([key, ":", value], out);
}

/// Details for an outbound invite (METHOD:REQUEST).
pub struct InviteSpec<'a> {
    pub uid: &'a str,
    pub sequence: i64,
    pub summary: &'a str,
    pub description: Option<&'a str>,
    pub location: Option<&'a str>,
    pub join_url: Option<&'a str>,
    pub organizer: &'a Address,
    pub attendees: &'a [Address],
    pub starts_at_ms: i64,
    pub ends_at_ms: i64,
    pub dtstamp_ms: i64,
}

fn calendar_shell(method: &str, body: impl FnOnce(&mut String)) -> String {
    let mut out = String::new();
    push_prop(&mut out, "BEGIN", "VCALENDAR");
    push_prop(
        &mut out,
        "PRODID",
        "-//FlectarMail//FlectarMail Calendar//EN",
    );
    push_prop(&mut out, "VERSION", "2.0");
    push_prop(&mut out, "METHOD", method);
    push_prop(&mut out, "BEGIN", "VEVENT");
    body(&mut out);
    push_prop(&mut out, "END", "VEVENT");
    push_prop(&mut out, "END", "VCALENDAR");
    out
}

/// Encode a parameter value per RFC 5545 §3.2: quote when it contains
/// characters that would break the property line (quotes themselves are
/// disallowed in param values, so they are dropped).
fn param_value(v: &str) -> Cow<'_, str> {
    let quote = v.contains([',', ';', ':']);
    if !quote && !v.contains('"') {
        return Cow::Borrowed(v);
    }
    let capacity =
        v.len() - v.bytes().filter(|&byte| byte == b'"').count() + if quote { 2 } else { 0 };
    let mut clean = String::with_capacity(capacity);
    if quote {
        clean.push('"');
    }
    clean.extend(v.chars().filter(|&character| character != '"'));
    if quote {
        clean.push('"');
    }
    Cow::Owned(clean)
}

fn organizer_prop(out: &mut String, addr: &Address) {
    match &addr.name {
        Some(name) if !name.is_empty() => {
            let name = param_value(name);
            fold_parts(
                [
                    "ORGANIZER;CN=",
                    name.as_ref(),
                    ":mailto:",
                    addr.email.as_str(),
                ],
                out,
            );
        }
        _ => fold_parts(["ORGANIZER:mailto:", addr.email.as_str()], out),
    }
}

fn attendee_prop(out: &mut String, attendee: &Address, parameters: &[&str]) {
    let name = attendee
        .name
        .as_deref()
        .filter(|name| !name.is_empty())
        .map(param_value);
    let prefix = if name.is_some() { "CN=" } else { "" };
    let suffix = if name.is_some() { ";" } else { "" };
    fold_parts(
        ["ATTENDEE;", prefix, name.as_deref().unwrap_or(""), suffix]
            .into_iter()
            .chain(parameters.iter().copied())
            .chain([":mailto:", attendee.email.as_str()]),
        out,
    );
}

/// Build a METHOD:REQUEST calendar for a new meeting invite.
pub fn build_request_ics(spec: &InviteSpec) -> String {
    calendar_shell("REQUEST", |out| {
        push_prop(out, "UID", spec.uid);
        push_prop(out, "DTSTAMP", &fmt_utc(spec.dtstamp_ms));
        push_prop(out, "SEQUENCE", &spec.sequence.to_string());
        push_prop(out, "DTSTART", &fmt_utc(spec.starts_at_ms));
        push_prop(out, "DTEND", &fmt_utc(spec.ends_at_ms));
        push_prop(out, "SUMMARY", &escape(spec.summary));
        if let Some(loc) = spec.location {
            push_prop(out, "LOCATION", &escape(loc));
        }
        let desc = match (spec.description, spec.join_url) {
            (Some(d), Some(u)) => Some(Cow::Owned(format!("{d}\n\n{u}"))),
            (Some(d), None) => Some(Cow::Borrowed(d)),
            (None, Some(u)) => Some(Cow::Borrowed(u)),
            (None, None) => None,
        };
        if let Some(d) = &desc {
            push_prop(out, "DESCRIPTION", &escape(d));
        }
        if let Some(u) = spec.join_url {
            push_prop(out, "URL", u);
        }
        push_prop(out, "STATUS", "CONFIRMED");
        organizer_prop(out, spec.organizer);
        for a in spec.attendees {
            attendee_prop(
                out,
                a,
                &["ROLE=REQ-PARTICIPANT;PARTSTAT=NEEDS-ACTION;RSVP=TRUE"],
            );
        }
    })
}

/// Build a METHOD:CANCEL calendar revoking a meeting we organize. SEQUENCE
/// must be higher than the last one sent so clients accept the cancellation.
pub fn build_cancel_ics(spec: &InviteSpec) -> String {
    calendar_shell("CANCEL", |out| {
        push_prop(out, "UID", spec.uid);
        push_prop(out, "DTSTAMP", &fmt_utc(spec.dtstamp_ms));
        push_prop(out, "SEQUENCE", &spec.sequence.to_string());
        push_prop(out, "DTSTART", &fmt_utc(spec.starts_at_ms));
        push_prop(out, "DTEND", &fmt_utc(spec.ends_at_ms));
        push_prop(out, "SUMMARY", &escape(spec.summary));
        push_prop(out, "STATUS", "CANCELLED");
        organizer_prop(out, spec.organizer);
        for a in spec.attendees {
            fold_parts(["ATTENDEE:mailto:", a.email.as_str()], out);
        }
    })
}

/// A content line split into its name, its parameters as written (such as
/// `CN="Doe, Jane"`) and its value.
fn raw_parts(line: &str) -> Option<(&str, Vec<&str>, &str)> {
    let mut quoted = false;
    let mut colon = None;
    for (index, character) in line.char_indices() {
        match character {
            '"' => quoted = !quoted,
            ':' if !quoted => {
                colon = Some(index);
                break;
            }
            _ => {}
        }
    }
    let colon = colon?;
    let head = &line[..colon];
    let mut segments = Vec::new();
    let mut start = 0;
    quoted = false;
    for (index, character) in head.char_indices() {
        match character {
            '"' => quoted = !quoted,
            ';' if !quoted => {
                segments.push(&head[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    segments.push(&head[start..]);
    let name = segments.remove(0);
    Some((name, segments, &line[colon + 1..]))
}

/// Whether `line` is one of the properties `names`.
fn property_is(line: &str, names: &[&str]) -> bool {
    raw_parts(line).is_some_and(|(name, _, _)| names.iter().any(|n| name.eq_ignore_ascii_case(n)))
}

/// One top-level component of a calendar, such as a VEVENT or a VTIMEZONE,
/// as its unfolded lines with BEGIN and END.
#[derive(Debug, Clone)]
struct Component {
    kind: String,
    lines: Vec<String>,
}

impl Component {
    /// The component's own properties, without those of the components it
    /// holds (such as a VALARM's).
    fn own_lines(&self) -> impl Iterator<Item = &str> {
        let mut depth = 0usize;
        self.lines.iter().filter_map(move |line| {
            let upper = line.to_ascii_uppercase();
            if upper.starts_with("BEGIN:") {
                depth += 1;
                None
            } else if upper.starts_with("END:") {
                depth = depth.saturating_sub(1);
                None
            } else {
                (depth == 1).then_some(line.as_str())
            }
        })
    }
}

/// What identifies a VEVENT among the versions of an event, and orders
/// them (RFC 5546 §2.1.5).
#[derive(Debug, Clone, Default, PartialEq)]
struct EventKey {
    uid: String,
    recurrence_id_ms: Option<i64>,
    sequence: i64,
    dtstamp_ms: Option<i64>,
}

impl EventKey {
    /// This version replaces `stored`: a higher SEQUENCE, or the same one
    /// written no earlier. Re-reading the stored version itself counts, so
    /// applying a message twice changes nothing.
    fn supersedes(&self, stored: &EventKey) -> bool {
        self.sequence > stored.sequence
            || (self.sequence == stored.sequence
                && match (self.dtstamp_ms, stored.dtstamp_ms) {
                    (Some(new), Some(old)) => new >= old,
                    _ => true,
                })
    }
}

/// A calendar split into its own properties and its top-level components,
/// so components can be replaced, added or cleaned without rewriting the
/// rest of it.
#[derive(Debug, Clone, Default)]
struct CalendarParts {
    properties: Vec<String>,
    components: Vec<Component>,
}

impl CalendarParts {
    fn parse(text: &str) -> Self {
        let mut parts = Self::default();
        let mut current: Option<Component> = None;
        let mut depth = 0usize;
        for line in unfold(text) {
            let line = line.into_owned();
            let upper = line.to_ascii_uppercase();
            if let Some(component) = current.as_mut() {
                component.lines.push(line);
                if upper.starts_with("BEGIN:") {
                    depth += 1;
                } else if upper.starts_with("END:") {
                    depth -= 1;
                    if depth == 0
                        && let Some(component) = current.take()
                    {
                        parts.components.push(component);
                    }
                }
                continue;
            }
            if line.trim().is_empty() || upper == "BEGIN:VCALENDAR" || upper == "END:VCALENDAR" {
                continue;
            }
            if let Some(kind) = upper.strip_prefix("BEGIN:") {
                current = Some(Component {
                    kind: kind.trim().to_string(),
                    lines: vec![line],
                });
                depth = 1;
            } else {
                parts.properties.push(line);
            }
        }
        parts
    }

    fn render(&self) -> String {
        let mut out = String::new();
        push_prop(&mut out, "BEGIN", "VCALENDAR");
        for line in &self.properties {
            fold(line, &mut out);
        }
        for component in &self.components {
            for line in &component.lines {
                fold(line, &mut out);
            }
        }
        push_prop(&mut out, "END", "VCALENDAR");
        out
    }

    fn zones(&self) -> HashMap<String, Vec<TzObservance>> {
        let lines: Vec<Cow<'_, str>> = self
            .components
            .iter()
            .filter(|component| component.kind == "VTIMEZONE")
            .flat_map(|component| {
                component
                    .lines
                    .iter()
                    .map(|line| Cow::Borrowed(line.as_str()))
            })
            .collect();
        parse_vtimezones(&lines)
    }

    fn zone_id(component: &Component) -> Option<String> {
        component.own_lines().find_map(|line| {
            let (key, _, value) = split_prop(line)?;
            (key == "TZID").then(|| value.trim().to_string())
        })
    }

    fn event_key(
        component: &Component,
        zones: &HashMap<String, Vec<TzObservance>>,
    ) -> Option<EventKey> {
        if component.kind != "VEVENT" {
            return None;
        }
        let mut key = EventKey::default();
        for line in component.own_lines() {
            let Some((name, params, value)) = split_prop(line) else {
                continue;
            };
            match name.as_str() {
                "UID" => key.uid = value.trim().to_string(),
                "RECURRENCE-ID" => {
                    key.recurrence_id_ms =
                        resolve_dt(value, param(&params, "TZID"), zones).map(|(ms, _)| ms);
                }
                "SEQUENCE" => key.sequence = value.trim().parse().unwrap_or(0),
                "DTSTAMP" => key.dtstamp_ms = parse_dt(value).map(|(ms, _)| ms),
                _ => {}
            }
        }
        Some(key)
    }
}

/// What an invitation leaves in the calendar: its events, without the
/// METHOD that made it a message (RFC 4791 §4.1) and without the alarms they
/// carry, since an organizer's reminders aren't the attendee's (RFC 9671 §4).
pub(crate) fn stored_form(calendar: &str) -> String {
    let mut parts = CalendarParts::parse(calendar);
    parts
        .properties
        .retain(|line| !property_is(line, &["METHOD"]));
    for component in &mut parts.components {
        if component.kind != "VEVENT" {
            continue;
        }
        let mut kept = Vec::with_capacity(component.lines.len());
        let mut alarm_depth = 0usize;
        for line in std::mem::take(&mut component.lines) {
            let upper = line.to_ascii_uppercase();
            if alarm_depth > 0 {
                if upper.starts_with("BEGIN:") {
                    alarm_depth += 1;
                } else if upper.starts_with("END:") {
                    alarm_depth -= 1;
                }
                continue;
            }
            if upper == "BEGIN:VALARM" {
                alarm_depth = 1;
                continue;
            }
            kept.push(line);
        }
        component.lines = kept;
    }
    parts.render()
}

/// `stored` with the occurrences of `uid` that `update` describes (VEVENTs
/// with a RECURRENCE-ID): each replaces the stored version of that
/// occurrence when it is newer, or joins the calendar. With `cancel` they
/// join as cancelled, which takes those occurrences out of the series.
/// Returns None when nothing changed. `stored` may be empty.
pub(crate) fn merge_occurrences(
    stored: &str,
    update: &str,
    uid: &str,
    cancel: bool,
) -> Option<String> {
    let mut merged = CalendarParts::parse(stored);
    let incoming = CalendarParts::parse(update);
    let incoming_zones = incoming.zones();
    let mut changed = false;
    for component in &incoming.components {
        let Some(key) = CalendarParts::event_key(component, &incoming_zones) else {
            continue;
        };
        let Some(occurrence) = key.recurrence_id_ms else {
            continue;
        };
        if key.uid != uid {
            continue;
        }
        let mut component = component.clone();
        if cancel {
            component
                .lines
                .retain(|line| !property_is(line, &["STATUS"]));
            let end = component.lines.len().saturating_sub(1);
            component.lines.insert(end, "STATUS:CANCELLED".into());
            // A cancellation may name the occurrence alone; the stored
            // occurrence starts where it would have.
            let has_start = component
                .own_lines()
                .any(|line| property_is(line, &["DTSTART"]));
            let recurrence_id = component
                .own_lines()
                .find(|line| property_is(line, &["RECURRENCE-ID"]))
                .map(str::to_string);
            if !has_start && let Some(recurrence_id) = recurrence_id {
                let start = format!("DTSTART{}", &recurrence_id["RECURRENCE-ID".len()..]);
                component.lines.insert(end, start);
            }
        }
        let stored_zones = merged.zones();
        let existing = merged.components.iter().position(|stored| {
            CalendarParts::event_key(stored, &stored_zones).is_some_and(|stored| {
                stored.uid == uid
                    && stored
                        .recurrence_id_ms
                        .is_some_and(|id| (id - occurrence).abs() < 60_000)
            })
        });
        match existing {
            Some(index) => {
                let stored_key = CalendarParts::event_key(&merged.components[index], &stored_zones);
                if stored_key.is_some_and(|stored| key.supersedes(&stored)) {
                    merged.components[index] = component;
                    changed = true;
                }
            }
            None => {
                merged.components.push(component);
                changed = true;
            }
        }
    }
    if !changed {
        return None;
    }
    if merged.properties.is_empty() {
        merged.properties = incoming
            .properties
            .iter()
            .filter(|line| !property_is(line, &["METHOD"]))
            .cloned()
            .collect();
    }
    // The zones the occurrences' times refer to come along.
    for zone in incoming.components.iter().filter(|c| c.kind == "VTIMEZONE") {
        let id = CalendarParts::zone_id(zone);
        let known = merged
            .components
            .iter()
            .any(|c| c.kind == "VTIMEZONE" && CalendarParts::zone_id(c) == id);
        if !known {
            merged.components.insert(0, zone.clone());
        }
    }
    Some(merged.render())
}

/// `calendar` with `attendee`'s participation set to `partstat` in each of
/// its events. Answering through a calendar server is this change to the
/// attendee's own copy (RFC 6638 §3.2.2.3), and a server may refuse an
/// attendee changing anything else, so every other line stays as it was.
/// None when the calendar doesn't list the attendee.
pub(crate) fn with_partstat(calendar: &str, attendee: &str, partstat: &str) -> Option<String> {
    let mut parts = CalendarParts::parse(calendar);
    let mut found = false;
    for component in parts
        .components
        .iter_mut()
        .filter(|component| component.kind == "VEVENT")
    {
        let mut depth = 0usize;
        for line in component.lines.iter_mut() {
            let upper = line.to_ascii_uppercase();
            if upper.starts_with("BEGIN:") {
                depth += 1;
                continue;
            }
            if upper.starts_with("END:") {
                depth = depth.saturating_sub(1);
                continue;
            }
            let listed = depth == 1
                && raw_parts(line).is_some_and(|(name, _, value)| {
                    name.eq_ignore_ascii_case("ATTENDEE")
                        && cal_address(value).eq_ignore_ascii_case(attendee)
                });
            if !listed {
                continue;
            }
            let kept = without_params(line, |name| name.eq_ignore_ascii_case("PARTSTAT"));
            if let Some((head, value)) = split_value(&kept) {
                *line = format!("{};PARTSTAT={partstat}:{value}", head.trim_end_matches(';'));
                found = true;
            }
        }
    }
    found.then(|| parts.render())
}

/// Whether the calendar's organizer is marked as scheduled by the client
/// (ORGANIZER;SCHEDULE-AGENT=CLIENT, RFC 6638 §7.1): its server then tells
/// the organizer nothing, and answers go by mail.
pub(crate) fn client_schedules(calendar: &str) -> bool {
    CalendarParts::parse(calendar)
        .components
        .iter()
        .filter(|component| component.kind == "VEVENT")
        .flat_map(Component::own_lines)
        .any(|line| {
            raw_parts(line).is_some_and(|(name, segments, _)| {
                name.eq_ignore_ascii_case("ORGANIZER")
                    && segments.iter().any(|segment| {
                        segment.split_once('=').is_some_and(|(key, value)| {
                            key.eq_ignore_ascii_case("SCHEDULE-AGENT")
                                && value.trim_matches('"').eq_ignore_ascii_case("CLIENT")
                        })
                    })
            })
        })
}

/// An attendee's answer to an invitation (METHOD:REPLY, RFC 5546 §3.2.3).
pub struct ReplySpec<'a> {
    /// The invitation as received. Its event's details are copied from it
    /// unchanged, as RFC 5546 requires; without it the reply carries only
    /// what identifies the event.
    pub invitation: Option<&'a str>,
    pub uid: &'a str,
    /// The occurrence answered (its original start); None answers them all.
    pub recurrence_id_ms: Option<i64>,
    /// The event lasts whole days, so an occurrence is named by its date.
    pub all_day: bool,
    /// The invitation's SEQUENCE, used when the invitation isn't at hand.
    pub sequence: i64,
    pub summary: Option<&'a str>,
    pub organizer: &'a str,
    /// The invited address answering.
    pub attendee: &'a Address,
    /// Who sends the answer for the invited address, when that's another
    /// address (SENT-BY).
    pub sent_by: Option<&'a str>,
    /// ACCEPTED | TENTATIVE | DECLINED
    pub partstat: &'a str,
    /// A note to the organizer.
    pub comment: Option<&'a str>,
    pub dtstamp_ms: i64,
}

/// The event's own lines a reply repeats unchanged. The rest of the event,
/// such as alarms, the description and the other attendees, stays out.
const REPLY_COPIED: &[&str] = &[
    "UID",
    "RECURRENCE-ID",
    "SEQUENCE",
    "DTSTART",
    "DTEND",
    "DURATION",
    "SUMMARY",
    "LOCATION",
];

/// Parameters that only belong between a CalDAV client and its server,
/// which RFC 6638 §7 forbids in scheduling messages.
fn is_server_scheduling_param(name: &str) -> bool {
    ["SCHEDULE-AGENT", "SCHEDULE-STATUS", "SCHEDULE-FORCE-SEND"]
        .iter()
        .any(|param| name.eq_ignore_ascii_case(param))
}

fn segment_name(segment: &str) -> &str {
    segment.split_once('=').map_or(segment, |(name, _)| name)
}

/// The TZID a property line refers to, if any.
fn line_tzid(line: &str) -> Option<String> {
    let (_, segments, _) = raw_parts(line)?;
    segments.iter().find_map(|segment| {
        let (name, value) = segment.split_once('=')?;
        name.eq_ignore_ascii_case("TZID")
            .then(|| value.trim_matches('"').to_string())
    })
}

/// Build a METHOD:REPLY calendar answering an invitation. Only one ATTENDEE
/// goes back, the invited address answering, and the event's details are the
/// invitation's own lines: rewriting them would be a change, which only a
/// COUNTER may propose.
pub fn build_reply_ics(spec: &ReplySpec) -> String {
    let invitation = spec
        .invitation
        .map(CalendarParts::parse)
        .unwrap_or_default();
    let zones = invitation.zones();
    let events: Vec<(EventKey, &Component)> = invitation
        .components
        .iter()
        .filter_map(|component| {
            CalendarParts::event_key(component, &zones)
                .filter(|key| key.uid == spec.uid)
                .map(|key| (key, component))
        })
        .collect();
    let same = |a: i64, b: i64| (a - b).abs() < 60_000;
    let occurrence = spec.recurrence_id_ms.and_then(|wanted| {
        events
            .iter()
            .find(|(key, _)| key.recurrence_id_ms.is_some_and(|id| same(id, wanted)))
    });
    let master = events
        .iter()
        .find(|(key, _)| key.recurrence_id_ms.is_none());
    // An occurrence the invitation doesn't describe on its own repeats only
    // the series' identity: the series' times aren't this occurrence's.
    let (source, whole) = match (spec.recurrence_id_ms, occurrence, master) {
        (_, Some((_, event)), _) => (Some(*event), true),
        (None, None, Some((_, event))) => (Some(*event), true),
        (Some(_), None, Some((_, event))) => (Some(*event), false),
        _ => (None, false),
    };
    let invited = |line: &str| {
        raw_parts(line).is_some_and(|(name, _, value)| {
            name.eq_ignore_ascii_case("ATTENDEE")
                && cal_address(value).eq_ignore_ascii_case(&spec.attendee.email)
        })
    };

    let mut copied: Vec<String> = Vec::new();
    let mut organizer_line = None;
    let mut attendee_line = None;
    if let Some(event) = source {
        for line in event.own_lines() {
            if property_is(line, &["ORGANIZER"]) {
                organizer_line = Some(without_params(line, is_server_scheduling_param));
            } else if invited(line) {
                attendee_line = Some(line.to_string());
            } else if (whole && property_is(line, REPLY_COPIED))
                || (!whole && property_is(line, &["UID", "SEQUENCE", "SUMMARY"]))
            {
                copied.push(line.to_string());
            }
        }
    }
    let mut needed_zones: Vec<String> = Vec::new();
    for line in &copied {
        if let Some(tzid) = line_tzid(line)
            && !needed_zones.contains(&tzid)
        {
            needed_zones.push(tzid);
        }
    }

    let mut out = String::new();
    push_prop(&mut out, "BEGIN", "VCALENDAR");
    push_prop(
        &mut out,
        "PRODID",
        "-//FlectarMail//FlectarMail Calendar//EN",
    );
    push_prop(&mut out, "VERSION", "2.0");
    push_prop(&mut out, "METHOD", "REPLY");
    for zone in invitation
        .components
        .iter()
        .filter(|component| component.kind == "VTIMEZONE")
    {
        if CalendarParts::zone_id(zone).is_some_and(|id| needed_zones.contains(&id)) {
            for line in &zone.lines {
                fold(line, &mut out);
            }
        }
    }
    push_prop(&mut out, "BEGIN", "VEVENT");
    if copied.is_empty() {
        push_prop(&mut out, "UID", spec.uid);
        push_prop(&mut out, "SEQUENCE", &spec.sequence.to_string());
        if let Some(summary) = spec.summary {
            push_prop(&mut out, "SUMMARY", &escape(summary));
        }
    }
    for line in &copied {
        fold(line, &mut out);
    }
    if let Some(original) = spec.recurrence_id_ms
        && !whole
    {
        let value = if spec.all_day {
            format!("RECURRENCE-ID;VALUE=DATE:{}", fmt_utc_date(original))
        } else {
            format!("RECURRENCE-ID:{}", fmt_utc(original))
        };
        fold(&value, &mut out);
    }
    push_prop(&mut out, "DTSTAMP", &fmt_utc(spec.dtstamp_ms));
    match organizer_line {
        Some(line) => fold(&line, &mut out),
        None => fold_parts(["ORGANIZER:mailto:", spec.organizer], &mut out),
    }
    let mut attendee = match &attendee_line {
        Some(line) => without_params(line, |name| {
            name.eq_ignore_ascii_case("PARTSTAT")
                || name.eq_ignore_ascii_case("RSVP")
                || name.eq_ignore_ascii_case("SENT-BY")
                || is_server_scheduling_param(name)
        }),
        None => {
            let mut line = String::from("ATTENDEE");
            if let Some(name) = spec
                .attendee
                .name
                .as_deref()
                .filter(|name| !name.is_empty())
            {
                line.push_str(";CN=");
                line.push_str(&param_value(name));
            }
            line.push_str(":mailto:");
            line.push_str(&spec.attendee.email);
            line
        }
    };
    if let Some((head, value)) = split_value(&attendee) {
        let mut line = head.trim_end_matches(';').to_string();
        line.push_str(";PARTSTAT=");
        line.push_str(spec.partstat);
        if let Some(sent_by) = spec.sent_by {
            line.push_str(";SENT-BY=\"mailto:");
            line.push_str(sent_by);
            line.push('"');
        }
        line.push(':');
        line.push_str(value);
        attendee = line;
    }
    fold(&attendee, &mut out);
    if let Some(comment) = spec.comment.map(str::trim).filter(|c| !c.is_empty()) {
        push_prop(&mut out, "COMMENT", &escape(comment));
    }
    push_prop(&mut out, "END", "VEVENT");
    push_prop(&mut out, "END", "VCALENDAR");
    out
}

/// `line` with the parameters `drop` names removed, the rest as written.
fn without_params(line: &str, drop: impl Fn(&str) -> bool) -> String {
    let Some((name, segments, value)) = raw_parts(line) else {
        return line.to_string();
    };
    let mut out = name.to_string();
    for segment in segments {
        if !drop(segment_name(segment)) {
            out.push(';');
            out.push_str(segment);
        }
    }
    out.push(':');
    out.push_str(value);
    out
}

/// A line's name and parameters, and its value.
fn split_value(line: &str) -> Option<(&str, &str)> {
    let (name, segments, value) = raw_parts(line)?;
    let head_len = name.len()
        + segments
            .iter()
            .map(|segment| segment.len() + 1)
            .sum::<usize>();
    Some((&line[..head_len], value))
}

fn fmt_utc_date(ms: i64) -> String {
    Utc.timestamp_millis_opt(ms)
        .earliest()
        .unwrap_or_else(Utc::now)
        .format("%Y%m%d")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:evt-1@cal.example.com\r\nSUMMARY:Design review\\, part 2\r\nLOCATION:Room 4\r\nORGANIZER;CN=Alice:mailto:alice@example.com\r\nDTSTART:20260715T140000Z\r\nDTEND:20260715T150000Z\r\nSTATUS:CONFIRMED\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

    #[test]
    fn parses_invite() {
        let events = parse_ics(SAMPLE);
        assert_eq!(events.len(), 1);
        let ev = &events[0];
        assert_eq!(ev.uid, "evt-1@cal.example.com");
        assert_eq!(ev.summary.as_deref(), Some("Design review, part 2"));
        assert_eq!(ev.method.as_deref(), Some("REQUEST"));
        assert_eq!(ev.organizer.as_deref(), Some("alice@example.com"));
        assert!(!ev.all_day);
        assert_eq!(ev.ends_at_ms.unwrap() - ev.starts_at_ms, 3_600_000);
    }

    #[test]
    fn tzid_no_dst_zone_uses_embedded_offset() {
        // Exchange invite: SE Asia (+07, no DST). The floating DTSTART must be
        // read in +07 regardless of the machine's own timezone.
        let ics = "BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nBEGIN:VTIMEZONE\r\nTZID:SE Asia Standard Time\r\nBEGIN:STANDARD\r\nDTSTART:16010101T000000\r\nTZOFFSETFROM:+0700\r\nTZOFFSETTO:+0700\r\nEND:STANDARD\r\nBEGIN:DAYLIGHT\r\nDTSTART:16010101T000000\r\nTZOFFSETFROM:+0700\r\nTZOFFSETTO:+0700\r\nEND:DAYLIGHT\r\nEND:VTIMEZONE\r\nBEGIN:VEVENT\r\nUID:tz-1\r\nSUMMARY:Standup\r\nDTSTART;TZID=SE Asia Standard Time:20260714T080000\r\nDTEND;TZID=SE Asia Standard Time:20260714T083000\r\nEND:VEVENT\r\nEND:VCALENDAR";
        let ev = &parse_ics(ics)[0];
        // 08:00 +07 == 01:00 UTC.
        let expect = Utc
            .with_ymd_and_hms(2026, 7, 14, 1, 0, 0)
            .unwrap()
            .timestamp_millis();
        assert_eq!(ev.starts_at_ms, expect);
        assert_eq!(ev.ends_at_ms.unwrap() - ev.starts_at_ms, 30 * 60_000);
        assert_eq!(ev.tzid.as_deref(), Some("SE Asia Standard Time"));
    }

    #[test]
    fn tzid_dst_zone_picks_correct_side_of_transition() {
        // US Eastern: EDT (-04) Mar..Nov, EST (-05) otherwise. A July meeting is
        // in EDT; a January one is in EST.
        let vtz = "BEGIN:VTIMEZONE\r\nTZID:Eastern Standard Time\r\nBEGIN:STANDARD\r\nDTSTART:16010101T020000\r\nTZOFFSETFROM:-0400\r\nTZOFFSETTO:-0500\r\nRRULE:FREQ=YEARLY;BYDAY=1SU;BYMONTH=11\r\nEND:STANDARD\r\nBEGIN:DAYLIGHT\r\nDTSTART:16010101T020000\r\nTZOFFSETFROM:-0500\r\nTZOFFSETTO:-0400\r\nRRULE:FREQ=YEARLY;BYDAY=2SU;BYMONTH=3\r\nEND:DAYLIGHT\r\nEND:VTIMEZONE\r\n";
        let july = format!(
            "BEGIN:VCALENDAR\r\n{vtz}BEGIN:VEVENT\r\nUID:e\r\nDTSTART;TZID=Eastern Standard Time:20260715T090000\r\nEND:VEVENT\r\nEND:VCALENDAR"
        );
        // 09:00 EDT (-04) == 13:00 UTC.
        let ev = &parse_ics(&july)[0];
        assert_eq!(
            ev.starts_at_ms,
            Utc.with_ymd_and_hms(2026, 7, 15, 13, 0, 0)
                .unwrap()
                .timestamp_millis()
        );
        let jan = format!(
            "BEGIN:VCALENDAR\r\n{vtz}BEGIN:VEVENT\r\nUID:e\r\nDTSTART;TZID=Eastern Standard Time:20260115T090000\r\nEND:VEVENT\r\nEND:VCALENDAR"
        );
        // 09:00 EST (-05) == 14:00 UTC.
        let ev = &parse_ics(&jan)[0];
        assert_eq!(
            ev.starts_at_ms,
            Utc.with_ymd_and_hms(2026, 1, 15, 14, 0, 0)
                .unwrap()
                .timestamp_millis()
        );
    }

    #[test]
    fn utc_and_unknown_tzid_still_parse() {
        // A `Z` value ignores any TZID; an unknown TZID falls back gracefully.
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:e\r\nDTSTART;TZID=Nonexistent/Zone:20260715T140000Z\r\nEND:VEVENT\r\nEND:VCALENDAR";
        let ev = &parse_ics(ics)[0];
        assert_eq!(
            ev.starts_at_ms,
            Utc.with_ymd_and_hms(2026, 7, 15, 14, 0, 0)
                .unwrap()
                .timestamp_millis()
        );
    }

    #[test]
    fn parses_all_day_and_folding() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:evt-2\r\nSUMMARY:Offsite\r\n day one\r\nDTSTART:20260801\r\nEND:VEVENT\r\nEND:VCALENDAR";
        let events = parse_ics(ics);
        assert_eq!(events.len(), 1);
        assert!(events[0].all_day);
        assert_eq!(
            events[0].starts_at_ms,
            Utc.with_ymd_and_hms(2026, 8, 1, 0, 0, 0)
                .unwrap()
                .timestamp_millis()
        );
        assert_eq!(events[0].summary.as_deref(), Some("Offsiteday one"));
    }

    #[test]
    fn parses_attendees_description_and_meet_link() {
        let ics = "BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:evt-3\r\nSUMMARY:Sync\r\nDESCRIPTION:Agenda\\nJoin: https://meet.google.com/abc-defg-hij\r\nATTENDEE;CN=\"Bob B\";PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:bob@example.com\r\nATTENDEE;PARTSTAT=ACCEPTED:mailto:carol@example.com\r\nSEQUENCE:2\r\nDTSTART:20260720T090000Z\r\nEND:VEVENT\r\nEND:VCALENDAR";
        let ev = &parse_ics(ics)[0];
        assert_eq!(
            ev.description.as_deref(),
            Some("Agenda\nJoin: https://meet.google.com/abc-defg-hij")
        );
        assert_eq!(ev.sequence, 2);
        assert_eq!(ev.attendees.len(), 2);
        assert_eq!(ev.attendees[0].email, "bob@example.com");
        assert_eq!(ev.attendees[0].name.as_deref(), Some("Bob B"));
        assert_eq!(ev.attendees[1].partstat.as_deref(), Some("ACCEPTED"));
        assert_eq!(
            ev.join_url.as_deref(),
            Some("https://meet.google.com/abc-defg-hij")
        );
    }

    #[test]
    fn finds_zoom_url_in_location() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:evt-4\r\nLOCATION:https://acme.zoom.us/j/123456?pwd=x\r\nDTSTART:20260720T090000Z\r\nEND:VEVENT\r\nEND:VCALENDAR";
        assert_eq!(
            parse_ics(ics)[0].join_url.as_deref(),
            Some("https://acme.zoom.us/j/123456?pwd=x")
        );
        assert_eq!(find_join_url("call me at https://example.com/x"), None);
    }

    #[test]
    fn reads_what_answering_an_invitation_needs() {
        let ics = "BEGIN:VCALENDAR\r\nMETHOD:request\r\nBEGIN:VEVENT\r\nUID:evt-5\r\nDTSTAMP:20260701T080000Z\r\nORGANIZER;CN=Alice A:MAILTO:alice@example.com\r\nATTENDEE;CN=Bob;ROLE=opt-participant;CUTYPE=INDIVIDUAL;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:MAILTO:bob@example.com\r\nATTENDEE;CUTYPE=GROUP:mailto:team@example.com\r\nDTSTART:20260720T090000Z\r\nDURATION:PT1H30M\r\nEND:VEVENT\r\nEND:VCALENDAR";
        let ev = &parse_ics(ics)[0];
        assert_eq!(ev.method.as_deref(), Some("REQUEST"));
        assert_eq!(ev.organizer.as_deref(), Some("alice@example.com"));
        assert_eq!(ev.organizer_name.as_deref(), Some("Alice A"));
        assert_eq!(
            ev.dtstamp_ms,
            parse_dt("20260701T080000Z").map(|(ms, _)| ms)
        );
        assert_eq!(ev.ends_at_ms, Some(ev.starts_at_ms + 90 * 60_000));
        let bob = &ev.attendees[0];
        assert_eq!(bob.email, "bob@example.com");
        assert!(bob.rsvp);
        assert_eq!(bob.role.as_deref(), Some("OPT-PARTICIPANT"));
        assert_eq!(bob.cutype.as_deref(), Some("INDIVIDUAL"));
        let team = &ev.attendees[1];
        assert!(!team.rsvp, "RSVP is FALSE unless the organizer asks");
        assert_eq!(team.cutype.as_deref(), Some("GROUP"));
    }

    #[test]
    fn an_alarm_inside_the_event_keeps_its_own_properties() {
        let ics = "BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:evt-6\r\nSUMMARY:Planning\r\nDESCRIPTION:Agenda\r\nDTSTART:20260720T090000Z\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nDESCRIPTION:Reminder\r\nTRIGGER:-PT15M\r\nEND:VALARM\r\nLOCATION:Room 2\r\nEND:VEVENT\r\nEND:VCALENDAR";
        let events = parse_ics(ics);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].description.as_deref(), Some("Agenda"));
        assert_eq!(
            events[0].location.as_deref(),
            Some("Room 2"),
            "properties after the alarm are the event's again"
        );
    }

    #[test]
    fn an_occurrence_is_found_in_its_own_time_zone() {
        let ics = "BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nBEGIN:VTIMEZONE\r\nTZID:SE Asia Standard Time\r\nBEGIN:STANDARD\r\nDTSTART:16010101T000000\r\nTZOFFSETFROM:+0700\r\nTZOFFSETTO:+0700\r\nEND:STANDARD\r\nEND:VTIMEZONE\r\nBEGIN:VEVENT\r\nUID:series-1\r\nRECURRENCE-ID;TZID=SE Asia Standard Time:20260714T080000\r\nDTSTART;TZID=SE Asia Standard Time:20260714T100000\r\nDTEND;TZID=SE Asia Standard Time:20260714T103000\r\nEND:VEVENT\r\nEND:VCALENDAR";
        let ev = &parse_ics(ics)[0];
        // 08:00 at UTC+7 is 01:00 UTC.
        assert_eq!(
            ev.recurrence_id_ms,
            parse_dt("20260714T010000Z").map(|(ms, _)| ms)
        );
    }

    #[test]
    fn durations_follow_rfc_5545() {
        assert_eq!(parse_duration("PT1H30M"), Some(90 * 60_000));
        assert_eq!(parse_duration("P1D"), Some(86_400_000));
        assert_eq!(parse_duration("P2W"), Some(14 * 86_400_000));
        assert_eq!(parse_duration("P1DT12H"), Some(36 * 3_600_000));
        assert_eq!(parse_duration("-PT15M"), Some(-15 * 60_000));
        assert_eq!(parse_duration("P"), None);
        assert_eq!(parse_duration("PT"), None);
        assert_eq!(parse_duration("PT1D"), None);
        assert_eq!(parse_duration("1H"), None);
    }

    #[test]
    fn request_roundtrips_through_parser() {
        let organizer = Address {
            name: Some("Dean".into()),
            email: "bd@northbeam.com".into(),
        };
        let attendees = vec![
            Address {
                name: Some("Bob, Jr.".into()),
                email: "bob@example.com".into(),
            },
            Address {
                name: None,
                email: "carol@example.com".into(),
            },
        ];
        let ics = build_request_ics(&InviteSpec {
            uid: "abc-123@flectar-mail",
            sequence: 0,
            summary: "Planning; H2, kickoff",
            description: Some("Line one\nline two"),
            location: Some("Room 9"),
            join_url: Some("https://meet.google.com/xyz"),
            organizer: &organizer,
            attendees: &attendees,
            starts_at_ms: 1_784_124_000_000,
            ends_at_ms: 1_784_127_600_000,
            dtstamp_ms: 1_784_000_000_000,
        });
        let evs = parse_ics(&ics);
        assert_eq!(evs.len(), 1);
        let ev = &evs[0];
        assert_eq!(ev.method.as_deref(), Some("REQUEST"));
        assert_eq!(ev.uid, "abc-123@flectar-mail");
        assert_eq!(ev.summary.as_deref(), Some("Planning; H2, kickoff"));
        assert_eq!(ev.location.as_deref(), Some("Room 9"));
        assert_eq!(ev.organizer.as_deref(), Some("bd@northbeam.com"));
        assert_eq!(ev.starts_at_ms, 1_784_124_000_000);
        assert_eq!(ev.ends_at_ms, Some(1_784_127_600_000));
        assert_eq!(ev.join_url.as_deref(), Some("https://meet.google.com/xyz"));
        assert_eq!(ev.attendees.len(), 2);
        assert_eq!(ev.attendees[0].name.as_deref(), Some("Bob, Jr."));
        assert_eq!(ev.attendees[0].partstat.as_deref(), Some("NEEDS-ACTION"));
        // every line respects the 75-octet cap
        for line in ics.lines() {
            assert!(line.len() <= 75, "line too long: {line}");
        }
    }

    const INVITATION: &str = "BEGIN:VCALENDAR\r\nPRODID:-//Example//EN\r\nVERSION:2.0\r\nMETHOD:REQUEST\r\nBEGIN:VTIMEZONE\r\nTZID:Europe/Madrid\r\nBEGIN:STANDARD\r\nDTSTART:16010101T000000\r\nTZOFFSETFROM:+0100\r\nTZOFFSETTO:+0100\r\nEND:STANDARD\r\nEND:VTIMEZONE\r\nBEGIN:VTIMEZONE\r\nTZID:Asia/Tokyo\r\nBEGIN:STANDARD\r\nDTSTART:16010101T000000\r\nTZOFFSETFROM:+0900\r\nTZOFFSETTO:+0900\r\nEND:STANDARD\r\nEND:VTIMEZONE\r\nBEGIN:VEVENT\r\nUID:weekly-1@example.com\r\nSEQUENCE:3\r\nDTSTAMP:20261101T080000Z\r\nDTSTART;TZID=Europe/Madrid:20261102T100000\r\nDTEND;TZID=Europe/Madrid:20261102T110000\r\nRRULE:FREQ=WEEKLY\r\nSUMMARY:Weekly sync\r\nLOCATION:Room 4\r\nDESCRIPTION:A long agenda\r\nORGANIZER;CN=Alice;SCHEDULE-AGENT=SERVER:mailto:alice@example.com\r\nATTENDEE;CN=\"Doe, Bob\";ROLE=REQ-PARTICIPANT;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:bob@example.com\r\nATTENDEE;PARTSTAT=ACCEPTED:mailto:carol@example.com\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT10M\r\nEND:VALARM\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:weekly-1@example.com\r\nRECURRENCE-ID;TZID=Europe/Madrid:20261109T100000\r\nSEQUENCE:4\r\nDTSTART;TZID=Europe/Madrid:20261109T120000\r\nDTEND;TZID=Europe/Madrid:20261109T130000\r\nSUMMARY:Weekly sync (moved)\r\nORGANIZER;CN=Alice:mailto:alice@example.com\r\nATTENDEE;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:bob@example.com\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

    fn bob() -> Address {
        Address {
            name: Some("Bob".into()),
            email: "BOB@example.com".into(),
        }
    }

    fn answer<'a>(invitation: Option<&'a str>, attendee: &'a Address) -> ReplySpec<'a> {
        ReplySpec {
            invitation,
            uid: "weekly-1@example.com",
            recurrence_id_ms: None,
            all_day: false,
            sequence: 3,
            summary: Some("Weekly sync"),
            organizer: "alice@example.com",
            attendee,
            sent_by: None,
            partstat: "ACCEPTED",
            comment: None,
            dtstamp_ms: 1_794_000_000_000,
        }
    }

    fn unfolded(ics: &str) -> Vec<String> {
        unfold(ics).into_iter().map(Cow::into_owned).collect()
    }

    #[test]
    fn a_reply_repeats_the_invitation_and_answers_for_one_attendee() {
        let me = bob();
        let ics = build_reply_ics(&ReplySpec {
            comment: Some("See you there"),
            ..answer(Some(INVITATION), &me)
        });
        let lines = unfolded(&ics);
        let has = |line: &str| lines.iter().any(|l| l == line);
        assert!(has("METHOD:REPLY"));
        assert!(has("UID:weekly-1@example.com"));
        assert!(has("SEQUENCE:3"));
        // The organizer's own times, in their zone, with that zone.
        assert!(has("DTSTART;TZID=Europe/Madrid:20261102T100000"));
        assert!(has("TZID:Europe/Madrid"));
        assert!(
            !has("TZID:Asia/Tokyo"),
            "zones the reply doesn't use stay out"
        );
        assert!(has("ORGANIZER;CN=Alice:mailto:alice@example.com"));
        assert!(has(
            "ATTENDEE;CN=\"Doe, Bob\";ROLE=REQ-PARTICIPANT;PARTSTAT=ACCEPTED:mailto:bob@example.com"
        ));
        assert!(has("COMMENT:See you there"));
        assert!(has(&format!("DTSTAMP:{}", fmt_utc(1_794_000_000_000))));
        let attendees = lines.iter().filter(|l| l.starts_with("ATTENDEE")).count();
        assert_eq!(attendees, 1, "only the attendee answering");
        for absent in [
            "RRULE",
            "DESCRIPTION",
            "BEGIN:VALARM",
            "RECURRENCE-ID",
            "SCHEDULE-AGENT",
        ] {
            assert!(!ics.contains(absent), "{absent} in:\n{ics}");
        }
        let ev = &parse_ics(&ics)[0];
        assert_eq!(ev.method.as_deref(), Some("REPLY"));
        assert_eq!(ev.attendees[0].partstat.as_deref(), Some("ACCEPTED"));
    }

    #[test]
    fn the_stored_form_drops_the_method_and_the_organizers_alarms() {
        let stored = stored_form(INVITATION);
        assert!(!stored.contains("METHOD"), "{stored}");
        assert!(!stored.contains("VALARM"), "{stored}");
        assert!(stored.contains("RRULE:FREQ=WEEKLY"));
        assert!(
            stored.contains("TZID:Asia/Tokyo"),
            "the rest stays as it was"
        );
        assert_eq!(
            parse_ics(&stored).len(),
            2,
            "the series and its moved occurrence"
        );
    }

    #[test]
    fn answering_on_a_server_changes_only_the_attendees_participation() {
        let answered = with_partstat(INVITATION, "BOB@example.com", "ACCEPTED").unwrap();
        let lines = unfolded(&answered);
        let bob: Vec<&String> = lines
            .iter()
            .filter(|line| line.starts_with("ATTENDEE") && line.ends_with("bob@example.com"))
            .collect();
        assert_eq!(
            bob,
            [
                "ATTENDEE;CN=\"Doe, Bob\";ROLE=REQ-PARTICIPANT;RSVP=TRUE;PARTSTAT=ACCEPTED:mailto:bob@example.com",
                "ATTENDEE;RSVP=TRUE;PARTSTAT=ACCEPTED:mailto:bob@example.com",
            ],
            "in the series and its moved occurrence"
        );
        let unchanged =
            |line: &&String| !(line.starts_with("ATTENDEE") && line.ends_with("bob@example.com"));
        assert_eq!(
            lines.iter().filter(unchanged).collect::<Vec<_>>(),
            unfolded(&CalendarParts::parse(INVITATION).render())
                .iter()
                .filter(unchanged)
                .collect::<Vec<_>>(),
            "everything else is as the server sent it"
        );
        assert!(with_partstat(INVITATION, "nobody@example.com", "ACCEPTED").is_none());
    }

    #[test]
    fn a_copy_the_client_schedules_is_recognized() {
        assert!(!client_schedules(INVITATION));
        let stored = INVITATION.replace(
            "ORGANIZER;CN=Alice;SCHEDULE-AGENT=SERVER:",
            "ORGANIZER;CN=Alice;SCHEDULE-AGENT=CLIENT:",
        );
        assert!(client_schedules(&stored));
    }

    #[test]
    fn a_moved_occurrence_is_answered_with_its_own_details() {
        let me = bob();
        let moved = parse_ics(INVITATION)[1].recurrence_id_ms;
        let ics = build_reply_ics(&ReplySpec {
            recurrence_id_ms: moved,
            partstat: "DECLINED",
            ..answer(Some(INVITATION), &me)
        });
        let lines = unfolded(&ics);
        let has = |line: &str| lines.iter().any(|l| l == line);
        assert!(has("RECURRENCE-ID;TZID=Europe/Madrid:20261109T100000"));
        assert!(has("SEQUENCE:4"));
        assert!(has("DTSTART;TZID=Europe/Madrid:20261109T120000"));
        assert!(has("ATTENDEE;PARTSTAT=DECLINED:mailto:bob@example.com"));
    }

    #[test]
    fn an_occurrence_the_invitation_does_not_list_is_named_by_its_start() {
        let me = bob();
        // The third week's 10:00 in Madrid (UTC+1) is 09:00 UTC.
        let third = parse_dt("20261116T090000Z").map(|(ms, _)| ms);
        let ics = build_reply_ics(&ReplySpec {
            recurrence_id_ms: third,
            partstat: "DECLINED",
            ..answer(Some(INVITATION), &me)
        });
        let lines = unfolded(&ics);
        assert!(lines.iter().any(|l| l == "RECURRENCE-ID:20261116T090000Z"));
        assert!(
            !ics.contains("DTSTART"),
            "the series' start isn't this occurrence's"
        );
        assert!(lines.iter().any(|l| l == "SEQUENCE:3"));

        let ics = build_reply_ics(&ReplySpec {
            recurrence_id_ms: parse_dt("20261116").map(|(ms, _)| ms),
            all_day: true,
            ..answer(None, &me)
        });
        assert!(
            unfolded(&ics)
                .iter()
                .any(|l| l == "RECURRENCE-ID;VALUE=DATE:20261116")
        );
    }

    #[test]
    fn without_the_invitation_a_reply_names_only_the_event() {
        let me = bob();
        let ics = build_reply_ics(&ReplySpec {
            sent_by: Some("assistant@example.com"),
            ..answer(None, &me)
        });
        let lines = unfolded(&ics);
        let has = |line: &str| lines.iter().any(|l| l == line);
        assert!(has("UID:weekly-1@example.com"));
        assert!(has("SEQUENCE:3"));
        assert!(has("SUMMARY:Weekly sync"));
        assert!(has("ORGANIZER:mailto:alice@example.com"));
        assert!(has(
            "ATTENDEE;CN=Bob;PARTSTAT=ACCEPTED;SENT-BY=\"mailto:assistant@example.com\":mailto:BOB@example.com"
        ));
        assert!(!ics.contains("DTSTART"), "no times it could get wrong");
    }
}
