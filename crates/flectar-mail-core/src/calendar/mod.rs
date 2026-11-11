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
                    if !ev.uid.is_empty() && ev.starts_at_ms != 0 {
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

/// Details for an RSVP reply (METHOD:REPLY).
pub struct ReplySpec<'a> {
    pub uid: &'a str,
    pub sequence: i64,
    pub summary: Option<&'a str>,
    /// ACCEPTED | TENTATIVE | DECLINED
    pub partstat: &'a str,
    pub organizer_email: &'a str,
    pub attendee: &'a Address,
    pub starts_at_ms: i64,
    pub ends_at_ms: Option<i64>,
    pub dtstamp_ms: i64,
}

/// Build a METHOD:REPLY calendar answering an invite.
pub fn build_reply_ics(spec: &ReplySpec) -> String {
    calendar_shell("REPLY", |out| {
        push_prop(out, "UID", spec.uid);
        push_prop(out, "DTSTAMP", &fmt_utc(spec.dtstamp_ms));
        push_prop(out, "SEQUENCE", &spec.sequence.to_string());
        push_prop(out, "DTSTART", &fmt_utc(spec.starts_at_ms));
        if let Some(end) = spec.ends_at_ms {
            push_prop(out, "DTEND", &fmt_utc(end));
        }
        if let Some(s) = spec.summary {
            push_prop(out, "SUMMARY", &escape(s));
        }
        fold_parts(["ORGANIZER:mailto:", spec.organizer_email], out);
        attendee_prop(out, spec.attendee, &["PARTSTAT=", spec.partstat]);
    })
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

    #[test]
    fn reply_roundtrips_through_parser() {
        let me = Address {
            name: None,
            email: "bd@northbeam.com".into(),
        };
        let ics = build_reply_ics(&ReplySpec {
            uid: "evt-1@cal.example.com",
            sequence: 1,
            summary: Some("Design review"),
            partstat: "ACCEPTED",
            organizer_email: "alice@example.com",
            attendee: &me,
            starts_at_ms: 1_784_124_000_000,
            ends_at_ms: None,
            dtstamp_ms: 1_784_000_000_000,
        });
        let ev = &parse_ics(&ics)[0];
        assert_eq!(ev.method.as_deref(), Some("REPLY"));
        assert_eq!(ev.attendees[0].partstat.as_deref(), Some("ACCEPTED"));
        assert_eq!(ev.attendees[0].email, "bd@northbeam.com");
    }
}
