//! Two-way CalDAV sync: discovery (RFC 6764/4791), incremental pull via
//! sync-collection (RFC 6578) with a calendar-query fallback, and offline-safe
//! push of local edits (PUT If-Match / DELETE). Google is CalDAV-with-OAuth;
//! Microsoft calendars sync over Graph behind the same seams.

pub mod discovery;
pub mod http;
pub mod push;
pub mod route;
pub mod rrule;
pub mod sync;
pub mod task;
pub mod xml;

pub use http::{DavAuth, DavResponse, HttpTransport, Transport};

use crate::error::CoreError;

/// Google's CalDAV endpoint prefix. Google does not expose a discoverable
/// service root here: clients must start at `/{calendar_id}/events` when they
/// already know the calendar collection. The primary calendar id is the
/// account email address.
pub const GOOGLE_CALDAV_BASE: &str = "https://apidata.googleusercontent.com/caldav/v2/";

/// Collection URL for a Google calendar id, encoded as one URL path segment.
///
/// Using `Url::path_segments_mut` matters for secondary calendar ids, which
/// can contain characters such as `#` that must not be interpreted as URL
/// syntax.
pub fn google_calendar_url(calendar_id: &str) -> Result<String, CoreError> {
    let mut url = url::Url::parse(GOOGLE_CALDAV_BASE)
        .map_err(|error| err(format!("invalid built-in Google CalDAV base URL: {error}")))?;
    url.path_segments_mut()
        .map_err(|_| err("built-in Google CalDAV URL cannot contain path segments"))?
        .pop_if_empty()
        .extend([calendar_id, "events"]);
    Ok(url.to_string())
}

/// The Google calendar id a CalDAV address names. Google serves each
/// calendar at `apidata.googleusercontent.com/caldav/v2/{id}/events`, for
/// OAuth, and at the older `www.google.com/calendar/dav/{id}/events`, which
/// also accepts an app password; another user's calendar shared with the
/// account, such as a shared mailbox's, is reached by its own id.
pub fn google_calendar_id(url: &str) -> Option<String> {
    let url = url::Url::parse(url).ok()?;
    let host = url.host_str()?.to_ascii_lowercase();
    let segments: Vec<&str> = url
        .path_segments()?
        .filter(|segment| !segment.is_empty())
        .collect();
    let id = match (host.as_str(), segments.as_slice()) {
        ("apidata.googleusercontent.com", ["caldav", "v2", id, "events"])
        | ("www.google.com" | "google.com", ["calendar", "dav", id, "events"]) => *id,
        _ => return None,
    };
    let id = percent_encoding::percent_decode_str(id)
        .decode_utf8()
        .ok()?
        .into_owned();
    (!id.is_empty()).then_some(id)
}

/// What a collection address identifies, so one account never syncs a
/// calendar through two connections: Google's calendar id for either Google
/// endpoint, else the address without case, escaping or trailing-slash
/// differences. Microsoft calendars are stored by Graph id and compare as is.
pub fn collection_identity(url: &str) -> String {
    if let Some(id) = google_calendar_id(url) {
        return format!("google:{}", id.to_lowercase());
    }
    match url::Url::parse(url) {
        Ok(parsed) if parsed.has_host() => {
            let path = percent_encoding::percent_decode_str(parsed.path()).decode_utf8_lossy();
            format!(
                "{}{}",
                parsed.origin().ascii_serialization(),
                path.trim_end_matches('/')
            )
        }
        _ => url.to_owned(),
    }
}

pub fn err(msg: impl Into<String>) -> CoreError {
    CoreError::CalDav(msg.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn google_collection_starts_at_the_primary_calendar() {
        assert_eq!(
            google_calendar_url("person@example.com").unwrap(),
            "https://apidata.googleusercontent.com/caldav/v2/person@example.com/events"
        );
    }

    #[test]
    fn both_google_endpoints_name_the_same_calendar() {
        let shared = "https://www.google.com/calendar/dav/shared@example.com/events/";
        assert_eq!(
            google_calendar_id(shared).as_deref(),
            Some("shared@example.com")
        );
        assert_eq!(
            collection_identity(shared),
            collection_identity(&google_calendar_url("Shared@Example.com").unwrap())
        );
        assert_eq!(
            google_calendar_id(
                "https://apidata.googleusercontent.com/caldav/v2/team%23group.v.calendar.google.com/events"
            )
            .as_deref(),
            Some("team#group.v.calendar.google.com")
        );
        for other in [
            "https://www.google.com/calendar/dav/shared@example.com/user/",
            "https://www.google.com/calendar/dav//events",
            "https://calendar.example.com/calendar/dav/shared@example.com/events/",
        ] {
            assert_eq!(google_calendar_id(other), None, "{other}");
        }
    }

    #[test]
    fn collection_identity_ignores_address_noise() {
        assert_eq!(
            collection_identity("https://DAV.example.com:443/cal/Team%20A/"),
            collection_identity("https://dav.example.com/cal/Team A")
        );
        assert_ne!(
            collection_identity("https://dav.example.com/cal/a/"),
            collection_identity("https://dav.example.com/cal/b/")
        );
        assert_eq!(collection_identity("AAMkAGI2"), "AAMkAGI2");
    }

    #[test]
    fn google_collection_encodes_calendar_id_as_one_path_segment() {
        assert_eq!(
            google_calendar_url("team/calendar#group.v.calendar.google.com").unwrap(),
            "https://apidata.googleusercontent.com/caldav/v2/team%2Fcalendar%23group.v.calendar.google.com/events"
        );
    }
}
