//! Minimal Microsoft Graph client: mint a Teams online meeting, write events
//! into the user's Outlook / Microsoft 365 calendar, and pull calendars +
//! events back (calendarView delta) for two-way sync - Microsoft has no
//! CalDAV endpoint, so Graph is the only calendar API for these accounts.
//!
//! Auth is a delegated (per-user) Graph access token obtained separately from
//! the mail token, see `oauth::tokens::TokenProvider::access_token_for_scope`.

use crate::error::{CoreError, Result};
use crate::http_body;

pub const GRAPH_BASE: &str = "https://graph.microsoft.com/v1.0";
const MAX_GRAPH_RESPONSE_BODY_BYTES: usize = 16 * 1024 * 1024;
const MAX_GRAPH_PAGINATION_URL_BYTES: usize = 16 * 1024;
const MAX_GRAPH_CALENDAR_PAGES: usize = 100;
const MAX_GRAPH_CALENDARS: usize = 10_000;

fn graph_http_client() -> Result<&'static reqwest::Client> {
    static HTTP: once_cell::sync::Lazy<std::result::Result<reqwest::Client, String>> =
        once_cell::sync::Lazy::new(|| {
            crate::http_tls::client_builder()
                .user_agent("flectar-mail-graph/0.1")
                .connect_timeout(std::time::Duration::from_secs(10))
                .timeout(std::time::Duration::from_secs(30))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|error| format!("graph HTTP client: {error}"))
        });
    HTTP.as_ref()
        .map_err(|message| CoreError::Network(message.clone()))
}

/// A created Teams online meeting (only the fields we surface today).
pub struct OnlineMeeting {
    pub join_url: String,
}

/// One attendee for a Graph calendar event.
pub struct GraphAttendee {
    pub email: String,
    pub name: Option<String>,
}

/// A calendar event to create in the user's Microsoft 365 calendar.
pub struct GraphEvent<'a> {
    pub subject: &'a str,
    pub body_html: Option<String>,
    pub location: Option<&'a str>,
    pub start_ms: i64,
    pub end_ms: i64,
    pub all_day: bool,
    pub attendees: Vec<GraphAttendee>,
}

/// Send a JSON request to Graph. `path_or_url` is either a v1.0 path
/// (`/me/events`) or an absolute URL (delta/next links come back absolute).
/// `401`/`403` map to `NeedsReauth` so callers can trigger consent. Returns
/// the HTTP status and raw body so callers can react to e.g. `410 Gone`.
async fn request_raw(
    method: reqwest::Method,
    access_token: &str,
    path_or_url: &str,
    body: Option<&serde_json::Value>,
) -> Result<(u16, String)> {
    let url = validated_graph_url(path_or_url)?;
    // reqwest is built without the `json` feature here, so set the body/headers
    // by hand rather than `.json()`.
    let mut req = graph_http_client()?
        .request(method, url)
        .bearer_auth(access_token);
    if let Some(body) = body {
        req = req
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.to_string());
    }
    let resp = req
        .send()
        .await
        .map_err(|e| CoreError::Network(format!("graph request failed: {e}")))?;

    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(CoreError::NeedsReauth);
    }
    let text = http_body::text(
        resp,
        MAX_GRAPH_RESPONSE_BODY_BYTES,
        "Microsoft Graph response",
    )
    .await?;
    Ok((status.as_u16(), text))
}

/// Like `request_raw`, but any non-2xx is an error and the body is parsed.
async fn request_json(
    method: reqwest::Method,
    access_token: &str,
    path_or_url: &str,
    body: Option<&serde_json::Value>,
) -> Result<serde_json::Value> {
    let m = method.clone();
    let (status, text) = request_raw(method, access_token, path_or_url, body).await?;
    if !(200..300).contains(&status) {
        return Err(CoreError::Network(format!(
            "graph {m} request failed ({status}): {}",
            response_excerpt(&text)
        )));
    }
    serde_json::from_str(&text).map_err(|e| CoreError::Other(format!("graph response parse: {e}")))
}

fn validated_graph_url(path_or_url: &str) -> Result<url::Url> {
    if path_or_url.len() > MAX_GRAPH_PAGINATION_URL_BYTES {
        return Err(CoreError::Network(
            "Microsoft Graph returned an oversized pagination URL".into(),
        ));
    }
    let url = if path_or_url.starts_with('/') && !path_or_url.starts_with("//") {
        url::Url::parse(&format!("{GRAPH_BASE}{path_or_url}"))
    } else {
        url::Url::parse(path_or_url)
    }
    .map_err(|_| CoreError::Network("Microsoft Graph returned an invalid pagination URL".into()))?;

    let trusted = url.scheme() == "https"
        && url
            .host_str()
            .is_some_and(|host| host.eq_ignore_ascii_case("graph.microsoft.com"))
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
        && (url.path() == "/v1.0" || url.path().starts_with("/v1.0/"));
    if !trusted {
        return Err(CoreError::Network(
            "refusing an untrusted Microsoft Graph pagination URL".into(),
        ));
    }
    Ok(url)
}

fn graph_resource_url(segments: &[&str]) -> Result<url::Url> {
    let mut url = url::Url::parse(GRAPH_BASE)
        .map_err(|error| CoreError::Other(format!("invalid built-in Graph URL: {error}")))?;
    {
        let mut path = url
            .path_segments_mut()
            .map_err(|_| CoreError::Other("built-in Graph URL cannot hold path segments".into()))?;
        for segment in segments {
            path.push(segment);
        }
    }
    Ok(url)
}

fn response_excerpt(body: &str) -> String {
    http_body::single_line_excerpt(body, 512)
}

async fn post(
    access_token: &str,
    path: &str,
    body: &serde_json::Value,
) -> Result<serde_json::Value> {
    request_json(reqwest::Method::POST, access_token, path, Some(body)).await
}

/// Create an ad-hoc Teams meeting via `POST /me/onlineMeetings`.
///
/// `start_ms`/`end_ms` are epoch milliseconds; Graph wants RFC 3339. The window
/// is advisory for an online meeting (the join URL stays valid regardless), but
/// a sensible start/end keeps it tidy in the organizer's Teams calendar.
pub async fn create_online_meeting(
    access_token: &str,
    subject: &str,
    start_ms: i64,
    end_ms: i64,
) -> Result<OnlineMeeting> {
    let payload = serde_json::json!({
        "subject": subject,
        "startDateTime": rfc3339(start_ms)?,
        "endDateTime": rfc3339(end_ms)?,
    });
    let parsed = post(access_token, "/me/onlineMeetings", &payload).await?;
    // v1.0 returns `joinWebUrl`; older payloads used `joinUrl`. Accept either.
    let join_url = parsed
        .get("joinWebUrl")
        .or_else(|| parsed.get("joinUrl"))
        .and_then(|v| v.as_str())
        .filter(|u| !u.is_empty())
        .ok_or_else(|| CoreError::Other("graph meeting had no join URL".into()))?
        .to_string();
    Ok(OnlineMeeting { join_url })
}

fn event_payload(ev: &GraphEvent<'_>) -> Result<serde_json::Value> {
    let attendees: Vec<serde_json::Value> = ev
        .attendees
        .iter()
        .map(|a| {
            serde_json::json!({
                "emailAddress": { "address": a.email, "name": a.name },
                "type": "required",
            })
        })
        .collect();

    let mut payload = serde_json::json!({
        "subject": ev.subject,
        "isAllDay": ev.all_day,
        "start": { "dateTime": graph_datetime(ev.start_ms, ev.all_day)?, "timeZone": "UTC" },
        "end": { "dateTime": graph_datetime(ev.end_ms, ev.all_day)?, "timeZone": "UTC" },
        "attendees": attendees,
    });
    if let Some(html) = &ev.body_html {
        payload["body"] = serde_json::json!({ "contentType": "HTML", "content": html });
    }
    if let Some(loc) = ev.location {
        payload["location"] = serde_json::json!({ "displayName": loc });
    }
    Ok(payload)
}

/// Create an event in a Microsoft 365 calendar (the default one when
/// `calendar_id` is None). Times are sent in UTC (`timeZone: "UTC"`);
/// Outlook/Teams render them in the viewer's own timezone. Returns the Graph
/// event id so sync can track the resource.
pub async fn create_calendar_event(
    access_token: &str,
    calendar_id: Option<&str>,
    ev: &GraphEvent<'_>,
) -> Result<String> {
    let path = match calendar_id {
        Some(id) => graph_resource_url(&["me", "calendars", id, "events"])?.to_string(),
        None => "/me/events".to_string(),
    };
    let created = post(access_token, &path, &event_payload(ev)?).await?;
    Ok(created
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string())
}

/// Update an existing event via `PATCH /me/events/{id}`.
pub async fn update_calendar_event(
    access_token: &str,
    event_id: &str,
    ev: &GraphEvent<'_>,
) -> Result<()> {
    let url = graph_resource_url(&["me", "events", event_id])?;
    request_json(
        reqwest::Method::PATCH,
        access_token,
        url.as_str(),
        Some(&event_payload(ev)?),
    )
    .await
    .map(|_| ())
}

/// Delete an event via `DELETE /me/events/{id}`. A 404 counts as success
/// (already gone remotely).
pub async fn delete_calendar_event(access_token: &str, event_id: &str) -> Result<()> {
    let url = graph_resource_url(&["me", "events", event_id])?;
    let (status, text) =
        request_raw(reqwest::Method::DELETE, access_token, url.as_str(), None).await?;
    if (200..300).contains(&status) || status == 404 {
        Ok(())
    } else {
        Err(CoreError::Network(format!(
            "graph DELETE event failed ({status}): {}",
            response_excerpt(&text)
        )))
    }
}

/// One calendar from `GET /me/calendars`.
pub struct GraphCalendar {
    pub id: String,
    pub name: Option<String>,
    /// CSS-style color ("lightBlue" presets map poorly; hexColor when present).
    pub hex_color: Option<String>,
    pub is_default: bool,
    pub can_edit: bool,
}

/// List the account's calendars.
pub async fn list_calendars(access_token: &str) -> Result<Vec<GraphCalendar>> {
    let mut out = Vec::new();
    let mut url =
        "/me/calendars?$select=id,name,hexColor,isDefaultCalendar,canEdit&$top=100".to_string();
    let mut seen_links = std::collections::HashSet::new();
    for _ in 0..MAX_GRAPH_CALENDAR_PAGES {
        let page = request_json(reqwest::Method::GET, access_token, &url, None).await?;
        for c in page
            .get("value")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
        {
            if out.len() == MAX_GRAPH_CALENDARS {
                return Err(CoreError::Network(format!(
                    "Microsoft Graph returned more than {MAX_GRAPH_CALENDARS} calendars"
                )));
            }
            let Some(id) = c.get("id").and_then(|v| v.as_str()) else {
                continue;
            };
            out.push(GraphCalendar {
                id: id.to_string(),
                name: c
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                hex_color: c
                    .get("hexColor")
                    .and_then(|v| v.as_str())
                    .filter(|s| s.starts_with('#'))
                    .map(|s| s.to_string()),
                is_default: c
                    .get("isDefaultCalendar")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                can_edit: c.get("canEdit").and_then(|v| v.as_bool()).unwrap_or(true),
            });
        }
        match page.get("@odata.nextLink").and_then(|v| v.as_str()) {
            Some(next) => {
                validated_graph_url(next)?;
                if !seen_links.insert(next.to_string()) {
                    return Err(CoreError::Network(
                        "Microsoft Graph repeated a calendar page link".into(),
                    ));
                }
                url = next.to_string();
            }
            None => return Ok(out),
        }
    }
    Err(CoreError::Network(format!(
        "Microsoft Graph exceeded the {MAX_GRAPH_CALENDAR_PAGES}-page calendar safety limit"
    )))
}

/// One event from a calendarView delta page, reduced to the fields sync
/// stores. `removed` items carry only the id.
pub struct DeltaEvent {
    pub id: String,
    pub removed: bool,
    pub ical_uid: Option<String>,
    /// "singleInstance" | "occurrence" | "exception" | "seriesMaster"
    pub event_type: Option<String>,
    /// Set on occurrence/exception instances; identifies the (unstored)
    /// series master so its own `@removed` delta item can cascade-delete
    /// every materialized occurrence of that series.
    pub series_master_id: Option<String>,
    pub etag: Option<String>,
    pub subject: Option<String>,
    pub body_preview: Option<String>,
    pub location: Option<String>,
    pub organizer_email: Option<String>,
    pub attendees: Vec<(String, Option<String>, Option<String>)>, // (email, name, partstat)
    pub join_url: Option<String>,
    pub start_ms: i64,
    pub end_ms: Option<i64>,
    pub all_day: bool,
    pub cancelled: bool,
}

pub struct DeltaPage {
    pub events: Vec<DeltaEvent>,
    pub next_link: Option<String>,
    pub delta_link: Option<String>,
}

/// Build the initial calendarView delta URL for a calendar and time window.
/// Timestamps use the `Z` suffix: an RFC 3339 `+00:00` offset would put a
/// literal `+` in the query string, which decodes to a space server-side.
pub fn delta_url(calendar_id: &str, start_ms: i64, end_ms: i64) -> Result<String> {
    let utc_z = |ms: i64| -> Result<String> {
        chrono::DateTime::from_timestamp_millis(ms)
            .map(|dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string())
            .ok_or_else(|| CoreError::Other("invalid timestamp".into()))
    };
    let mut url = graph_resource_url(&["me", "calendars", calendar_id, "calendarView", "delta"])?;
    url.query_pairs_mut()
        .append_pair("startDateTime", &utc_z(start_ms)?)
        .append_pair("endDateTime", &utc_z(end_ms)?);
    Ok(url.to_string())
}

/// Fetch one delta page (initial URL, nextLink or deltaLink). `Ok(None)`
/// means the delta token expired (`410 Gone`): the caller restarts from a
/// fresh `delta_url`.
pub async fn delta_page(access_token: &str, url: &str) -> Result<Option<DeltaPage>> {
    let (status, text) = request_raw(reqwest::Method::GET, access_token, url, None).await?;
    if status == 410 {
        return Ok(None);
    }
    if !(200..300).contains(&status) {
        return Err(CoreError::Network(format!(
            "graph delta failed ({status}): {}",
            response_excerpt(&text)
        )));
    }
    let page: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| CoreError::Other(format!("graph delta parse: {e}")))?;
    let events = page
        .get("value")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter_map(parse_delta_event)
        .collect();
    Ok(Some(DeltaPage {
        events,
        next_link: page
            .get("@odata.nextLink")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        delta_link: page
            .get("@odata.deltaLink")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
    }))
}

fn parse_delta_event(v: &serde_json::Value) -> Option<DeltaEvent> {
    let id = v.get("id").and_then(|x| x.as_str())?.to_string();
    if v.get("@removed").is_some() {
        return Some(DeltaEvent {
            id,
            removed: true,
            ical_uid: None,
            event_type: None,
            series_master_id: None,
            etag: None,
            subject: None,
            body_preview: None,
            location: None,
            organizer_email: None,
            attendees: Vec::new(),
            join_url: None,
            start_ms: 0,
            end_ms: None,
            all_day: false,
            cancelled: false,
        });
    }
    let s = |path: &[&str]| -> Option<String> {
        let mut cur = v;
        for k in path {
            cur = cur.get(k)?;
        }
        cur.as_str()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
    };
    let all_day = v.get("isAllDay").and_then(|x| x.as_bool()).unwrap_or(false);
    let attendees = v
        .get("attendees")
        .and_then(|x| x.as_array())
        .into_iter()
        .flatten()
        .filter_map(|a| {
            let email = a
                .get("emailAddress")
                .and_then(|e| e.get("address"))
                .and_then(|x| x.as_str())?
                .to_string();
            let name = a
                .get("emailAddress")
                .and_then(|e| e.get("name"))
                .and_then(|x| x.as_str())
                .map(|s| s.to_string());
            let partstat = a
                .get("status")
                .and_then(|st| st.get("response"))
                .and_then(|x| x.as_str())
                .map(|r| match r {
                    "accepted" | "organizer" => "ACCEPTED".to_string(),
                    "declined" => "DECLINED".to_string(),
                    "tentativelyAccepted" => "TENTATIVE".to_string(),
                    _ => "NEEDS-ACTION".to_string(),
                });
            Some((email, name, partstat))
        })
        .collect();
    Some(DeltaEvent {
        removed: false,
        ical_uid: s(&["iCalUId"]),
        event_type: s(&["type"]),
        series_master_id: s(&["seriesMasterId"]),
        etag: s(&["@odata.etag"]).or_else(|| s(&["changeKey"])),
        subject: s(&["subject"]),
        body_preview: s(&["bodyPreview"]),
        location: s(&["location", "displayName"]),
        organizer_email: s(&["organizer", "emailAddress", "address"]),
        attendees,
        join_url: s(&["onlineMeeting", "joinUrl"]).or_else(|| s(&["onlineMeetingUrl"])),
        start_ms: parse_graph_time(v.get("start")).unwrap_or(0),
        end_ms: parse_graph_time(v.get("end")),
        all_day,
        cancelled: v
            .get("isCancelled")
            .and_then(|x| x.as_bool())
            .unwrap_or(false),
        id,
    })
}

/// Parse a Graph `{ dateTime, timeZone }` pair into epoch ms. Graph returns
/// UTC unless a Prefer header asks otherwise (we don't), so the local-style
/// string is interpreted as UTC.
fn parse_graph_time(v: Option<&serde_json::Value>) -> Option<i64> {
    let dt = v?.get("dateTime")?.as_str()?;
    let fmt = if dt.contains('.') {
        "%Y-%m-%dT%H:%M:%S%.f"
    } else {
        "%Y-%m-%dT%H:%M:%S"
    };
    let naive = chrono::NaiveDateTime::parse_from_str(dt, fmt).ok()?;
    Some(naive.and_utc().timestamp_millis())
}

fn rfc3339(ms: i64) -> Result<String> {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|dt| dt.to_rfc3339())
        .ok_or_else(|| CoreError::Other("invalid timestamp".into()))
}

/// Graph's `dateTime` field is a local-style ISO string paired with `timeZone`.
/// We pass UTC, so format the instant as `YYYY-MM-DDTHH:MM:SS`; all-day events
/// must sit on a midnight boundary.
fn graph_datetime(ms: i64, all_day: bool) -> Result<String> {
    let dt = chrono::DateTime::from_timestamp_millis(ms)
        .ok_or_else(|| CoreError::Other("invalid timestamp".into()))?;
    let fmt = if all_day {
        "%Y-%m-%dT00:00:00"
    } else {
        "%Y-%m-%dT%H:%M:%S"
    };
    Ok(dt.format(fmt).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_delta_event_fields() {
        let v = serde_json::json!({
            "id": "AAMkAD1",
            "@odata.etag": "W/\"abc\"",
            "iCalUId": "040000008200E0",
            "type": "singleInstance",
            "subject": "Standup",
            "bodyPreview": "daily",
            "isAllDay": false,
            "isCancelled": false,
            "location": { "displayName": "Room 1" },
            "organizer": { "emailAddress": { "address": "boss@x.com", "name": "Boss" } },
            "attendees": [
                { "emailAddress": { "address": "a@x.com", "name": "A" },
                  "status": { "response": "tentativelyAccepted" } }
            ],
            "onlineMeeting": { "joinUrl": "https://teams.microsoft.com/l/x" },
            "start": { "dateTime": "2026-08-01T09:00:00.0000000", "timeZone": "UTC" },
            "end": { "dateTime": "2026-08-01T10:00:00.0000000", "timeZone": "UTC" }
        });
        let ev = parse_delta_event(&v).unwrap();
        assert!(!ev.removed);
        assert_eq!(ev.subject.as_deref(), Some("Standup"));
        assert_eq!(ev.ical_uid.as_deref(), Some("040000008200E0"));
        assert_eq!(ev.location.as_deref(), Some("Room 1"));
        assert_eq!(ev.organizer_email.as_deref(), Some("boss@x.com"));
        assert_eq!(ev.attendees[0].2.as_deref(), Some("TENTATIVE"));
        assert_eq!(
            ev.join_url.as_deref(),
            Some("https://teams.microsoft.com/l/x")
        );
        assert_eq!(ev.start_ms, 1_785_574_800_000);
        assert_eq!(ev.end_ms, Some(1_785_578_400_000));
    }

    #[test]
    fn parses_series_master_id_on_occurrence() {
        let v = serde_json::json!({
            "id": "AAMkAD1-occ1",
            "type": "occurrence",
            "seriesMasterId": "AAMkAD1-master",
            "start": { "dateTime": "2026-08-01T09:00:00.0000000", "timeZone": "UTC" },
            "end": { "dateTime": "2026-08-01T10:00:00.0000000", "timeZone": "UTC" }
        });
        let ev = parse_delta_event(&v).unwrap();
        assert_eq!(ev.series_master_id.as_deref(), Some("AAMkAD1-master"));
    }

    #[test]
    fn parses_removed_delta_item() {
        let v = serde_json::json!({
            "id": "gone1",
            "@removed": { "reason": "deleted" }
        });
        let ev = parse_delta_event(&v).unwrap();
        assert!(ev.removed);
        assert_eq!(ev.id, "gone1");
    }

    #[test]
    fn delta_url_uses_z_suffix() {
        let url = delta_url("CAL1", 1_785_574_800_000, 1_785_578_400_000).unwrap();
        assert_eq!(
            url,
            "https://graph.microsoft.com/v1.0/me/calendars/CAL1/calendarView/delta\
             ?startDateTime=2026-08-01T09%3A00%3A00Z&endDateTime=2026-08-01T10%3A00%3A00Z"
        );
    }

    #[test]
    fn bearer_auth_is_restricted_to_the_graph_v1_origin() {
        assert!(validated_graph_url("/me/events?$top=10").is_ok());
        assert!(
            validated_graph_url("https://graph.microsoft.com/v1.0/me/events?$skiptoken=x").is_ok()
        );
        for untrusted in [
            "https://example.test/v1.0/me/events",
            "http://graph.microsoft.com/v1.0/me/events",
            "https://user@graph.microsoft.com/v1.0/me/events",
            "https://graph.microsoft.com:444/v1.0/me/events",
            "https://graph.microsoft.com/beta/me/events",
            "//example.test/v1.0/me/events",
        ] {
            assert!(validated_graph_url(untrusted).is_err(), "{untrusted}");
        }
    }

    #[test]
    fn provider_ids_are_encoded_as_single_graph_path_segments() {
        let url = graph_resource_url(&["me", "calendars", "../calendar/with?#", "events"]).unwrap();
        assert_eq!(url.host_str(), Some("graph.microsoft.com"));
        assert!(url.path().starts_with("/v1.0/me/calendars/"));
        assert!(url.path().ends_with("/events"));
        assert!(!url.path().contains("/calendar/with"));
        assert!(url.query().is_none());
        assert!(url.fragment().is_none());
    }

    #[test]
    fn parses_time_without_fraction() {
        let v = serde_json::json!({ "dateTime": "2026-08-01T09:00:00", "timeZone": "UTC" });
        assert_eq!(parse_graph_time(Some(&v)), Some(1_785_574_800_000));
    }
}
