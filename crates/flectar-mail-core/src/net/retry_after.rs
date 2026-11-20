//! How long an HTTP server asked to wait before the next request: its
//! Retry-After field (RFC 9110 section 10.2.3), in seconds or as a date.

use chrono::{DateTime, NaiveDateTime, Utc};
use std::time::Duration;

/// The wait a response's Retry-After field names, if it names one.
pub fn of(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let value = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
    parse(value, Utc::now())
}

/// The wait `value` names at `now`: a number of seconds, or a date in any
/// of HTTP's three formats. A date that has passed is no wait at all.
pub fn parse(value: &str, now: DateTime<Utc>) -> Option<Duration> {
    let value = value.trim();
    if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Some(Duration::from_secs(value.parse().unwrap_or(u64::MAX)));
    }
    until(date(value)?, now)
}

/// The wait from `now` until `at`.
pub fn until(at: DateTime<Utc>, now: DateTime<Utc>) -> Option<Duration> {
    Some((at - now).to_std().unwrap_or(Duration::ZERO))
}

fn date(value: &str) -> Option<DateTime<Utc>> {
    // IMF-fixdate, "Sun, 06 Nov 1994 08:49:37 GMT", the one servers send.
    if let Ok(date) = DateTime::parse_from_rfc2822(value) {
        return Some(date.with_timezone(&Utc));
    }
    // The obsolete RFC 850 and asctime formats recipients still accept:
    // "Sunday, 06-Nov-94 08:49:37 GMT" and "Sun Nov  6 08:49:37 1994".
    ["%A, %d-%b-%y %H:%M:%S GMT", "%a %b %e %H:%M:%S %Y"]
        .into_iter()
        .find_map(|format| NaiveDateTime::parse_from_str(value, format).ok())
        .map(|date| date.and_utc())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("1994-11-06T08:49:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn seconds_and_dates_in_every_http_format() {
        assert_eq!(parse("120", now()), Some(Duration::from_secs(120)));
        assert_eq!(parse(" 0 ", now()), Some(Duration::ZERO));
        for date in [
            "Sun, 06 Nov 1994 08:49:37 GMT",
            "Sunday, 06-Nov-94 08:49:37 GMT",
            "Sun Nov  6 08:49:37 1994",
        ] {
            assert_eq!(parse(date, now()), Some(Duration::from_secs(37)), "{date}");
        }
    }

    #[test]
    fn a_passed_date_is_no_wait_and_nonsense_none() {
        assert_eq!(
            parse("Sat, 05 Nov 1994 08:49:37 GMT", now()),
            Some(Duration::ZERO)
        );
        assert_eq!(parse("soon", now()), None);
        assert_eq!(parse("", now()), None);
        assert_eq!(parse("-5", now()), None);
        assert_eq!(parse("1.5", now()), None);
        // Too many seconds to count is a very long wait, which callers cap.
        assert_eq!(
            parse("99999999999999999999999", now()),
            Some(Duration::from_secs(u64::MAX))
        );
    }

    #[test]
    fn the_field_of_a_response() {
        let mut headers = reqwest::header::HeaderMap::new();
        assert_eq!(of(&headers), None);
        headers.insert(reqwest::header::RETRY_AFTER, "30".parse().unwrap());
        assert_eq!(of(&headers), Some(Duration::from_secs(30)));
    }
}
