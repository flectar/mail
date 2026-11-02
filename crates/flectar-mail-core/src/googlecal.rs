//! Google Calendar REST metadata discovery.
//!
//! CalDAV sync operates on one calendar collection URL at a time and does not
//! provide Google Calendar's account-level calendar list. Use Calendar API
//! `calendarList.list` to enumerate every calendar shown to the user, then
//! hand each calendar id to the existing CalDAV sync engine.

use std::collections::HashSet;

use crate::error::{CoreError, Result};
use crate::http_body;
use crate::{caldav, db::repo, models::Calendar};

const CALENDAR_LIST_URL: &str = "https://www.googleapis.com/calendar/v3/users/me/calendarList";
const MAX_CALENDAR_LIST_BODY_BYTES: usize = 8 * 1024 * 1024;
const MAX_CALENDAR_LIST_PAGES: usize = 100;
const MAX_CALENDARS: usize = 10_000;
const MAX_PAGE_TOKEN_BYTES: usize = 16 * 1024;

fn calendar_http_client() -> Result<&'static reqwest::Client> {
    static HTTP: once_cell::sync::Lazy<std::result::Result<reqwest::Client, String>> =
        once_cell::sync::Lazy::new(|| {
            reqwest::Client::builder()
                .user_agent("flectar-mail-google-calendar/0.1")
                .connect_timeout(std::time::Duration::from_secs(10))
                .timeout(std::time::Duration::from_secs(30))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|error| format!("Google Calendar HTTP client: {error}"))
        });
    HTTP.as_ref()
        .map_err(|message| CoreError::Network(message.clone()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoogleCalendar {
    pub id: String,
    pub name: Option<String>,
    pub color: Option<String>,
    pub primary: bool,
    pub read_only: bool,
    /// Mirrors whether Google Calendar currently displays this calendar.
    pub selected: bool,
}

/// List all calendars in the signed-in user's Google Calendar sidebar.
pub async fn list_calendars(access_token: &str) -> Result<Vec<GoogleCalendar>> {
    let client = calendar_http_client()?;
    let mut page_token: Option<String> = None;
    let mut calendars = Vec::new();
    let mut seen_page_tokens = std::collections::HashSet::new();
    let base_url = url::Url::parse(CALENDAR_LIST_URL).map_err(|error| {
        CoreError::Other(format!(
            "invalid built-in Google Calendar list URL: {error}"
        ))
    })?;

    for _ in 0..MAX_CALENDAR_LIST_PAGES {
        let mut url = base_url.clone();
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("maxResults", "250");
            query.append_pair("showDeleted", "false");
            query.append_pair("showHidden", "true");
            if let Some(token) = page_token.as_deref() {
                query.append_pair("pageToken", token);
            }
        }
        let response = client
            .get(url)
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(network_error)?;
        let status = response.status();
        let body = http_body::text(
            response,
            MAX_CALENDAR_LIST_BODY_BYTES,
            "Google Calendar list response",
        )
        .await?;
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(CoreError::NeedsReauth);
        }
        if !status.is_success() {
            return Err(calendar_api_error(status.as_u16(), &body));
        }
        let (mut page, next) = parse_calendar_list(&body)?;
        if page.len() > MAX_CALENDARS.saturating_sub(calendars.len()) {
            return Err(CoreError::CalDav(format!(
                "Google Calendar returned more than {MAX_CALENDARS} calendars"
            )));
        }
        calendars.append(&mut page);
        match next {
            Some(token) if !token.is_empty() => {
                if token.len() > MAX_PAGE_TOKEN_BYTES {
                    return Err(CoreError::CalDav(
                        "Google Calendar returned an oversized page token".into(),
                    ));
                }
                if !seen_page_tokens.insert(token.clone()) {
                    return Err(CoreError::CalDav(
                        "Google Calendar repeated a page token".into(),
                    ));
                }
                page_token = Some(token);
            }
            _ => {
                page_token = None;
                break;
            }
        }
    }
    if page_token.is_some() {
        return Err(CoreError::CalDav(format!(
            "Google Calendar exceeded the {MAX_CALENDAR_LIST_PAGES}-page safety limit"
        )));
    }

    // Stable, human-friendly order: primary first, then the provider's names.
    calendars.sort_by(|left, right| {
        right
            .primary
            .cmp(&left.primary)
            .then_with(|| name_for_sort(left).cmp(&name_for_sort(right)))
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(calendars)
}

/// Reconcile the Google calendar list into `connection`'s collections. The
/// provider's selected state is used only for new rows; local visibility
/// always survives later refreshes. Calendars that left the list are
/// forgotten. One that another connection of the account already syncs,
/// such as a shared calendar added over CalDAV, stays with that connection.
pub fn reconcile_calendars(
    conn: &rusqlite::Connection,
    connection: &repo::caldav::ConnectionConfig,
    calendars: &[GoogleCalendar],
) -> Result<Vec<Calendar>> {
    let taken: HashSet<String> = repo::caldav::list_calendars(conn, Some(connection.account_id))?
        .into_iter()
        .filter(|calendar| calendar.connection_id != connection.id)
        .map(|calendar| caldav::collection_identity(&calendar.url))
        .collect();
    let mut listed = HashSet::new();
    let mut primary_id = None;
    for calendar in calendars {
        let url = caldav::google_calendar_url(&calendar.id)?;
        if taken.contains(&caldav::collection_identity(&url)) {
            continue;
        }
        let id = repo::caldav::upsert_calendar_with_initial_enabled(
            conn,
            connection,
            &url,
            calendar.name.as_deref(),
            calendar.color.as_deref(),
            calendar.read_only,
            calendar.selected,
        )?;
        if calendar.primary {
            primary_id.get_or_insert(id);
        }
        listed.insert(url);
    }
    repo::caldav::retain_calendars(conn, connection.id, &listed)?;
    repo::caldav::ensure_default_calendar(conn, connection.account_id, primary_id)?;
    repo::caldav::connection_calendars(conn, connection.id)
}

fn name_for_sort(calendar: &GoogleCalendar) -> String {
    calendar
        .name
        .as_deref()
        .unwrap_or(&calendar.id)
        .to_lowercase()
}

fn network_error(error: reqwest::Error) -> CoreError {
    if error.is_connect() || error.is_timeout() {
        CoreError::Offline
    } else {
        CoreError::Network(format!("Google Calendar request failed: {error}"))
    }
}

fn calendar_api_error(status: u16, body: &str) -> CoreError {
    let detail = response_excerpt(body);
    match status {
        403 => CoreError::CalDav(format!(
            "Google Calendar API rejected calendar discovery (HTTP 403). Enable calendar-json.googleapis.com and confirm the Calendar OAuth scope.{detail}"
        )),
        429 => CoreError::CalDav(format!(
            "Google Calendar is temporarily rate-limiting calendar discovery (HTTP 429). Retry shortly.{detail}"
        )),
        500..=599 => CoreError::CalDav(format!(
            "Google Calendar is temporarily unavailable (HTTP {status}).{detail}"
        )),
        _ => CoreError::CalDav(format!(
            "Google Calendar list failed (HTTP {status}).{detail}"
        )),
    }
}

fn response_excerpt(body: &str) -> String {
    let excerpt = http_body::single_line_excerpt(body, 240);
    if excerpt.is_empty() {
        return String::new();
    }
    format!(" Google response: {excerpt}")
}

fn parse_calendar_list(body: &str) -> Result<(Vec<GoogleCalendar>, Option<String>)> {
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|error| CoreError::CalDav(format!("Google Calendar list parse: {error}")))?;
    let calendars = value
        .get("items")
        .and_then(|items| items.as_array())
        .into_iter()
        .flatten()
        .filter_map(parse_calendar)
        .collect();
    let next = value
        .get("nextPageToken")
        .and_then(|token| token.as_str())
        .map(str::to_owned);
    Ok((calendars, next))
}

fn parse_calendar(value: &serde_json::Value) -> Option<GoogleCalendar> {
    if value.get("deleted").and_then(|v| v.as_bool()) == Some(true) {
        return None;
    }
    let id = value.get("id")?.as_str()?.trim();
    if id.is_empty() {
        return None;
    }
    let primary = value
        .get("primary")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let selected = value
        .get("selected")
        .and_then(|v| v.as_bool())
        .unwrap_or(primary)
        && !value
            .get("hidden")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
    let access_role = value
        .get("accessRole")
        .and_then(|v| v.as_str())
        .unwrap_or("reader");
    Some(GoogleCalendar {
        id: id.to_owned(),
        name: value
            .get("summaryOverride")
            .and_then(|v| v.as_str())
            .or_else(|| value.get("summary").and_then(|v| v.as_str()))
            .filter(|name| !name.trim().is_empty())
            .map(str::to_owned),
        color: value
            .get("backgroundColor")
            .and_then(|v| v.as_str())
            .filter(|color| color.starts_with('#'))
            .map(str::to_owned),
        primary,
        read_only: !matches!(access_role, "owner" | "writer"),
        selected,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_primary_birthdays_and_hidden_calendars() {
        let body = r##"{
          "nextPageToken": "page-2",
          "items": [
            {"id":"me@example.com","summary":"Me","backgroundColor":"#4285f4","primary":true,"selected":true,"accessRole":"owner"},
            {"id":"#contacts@group.v.calendar.google.com","summary":"Birthdays","backgroundColor":"#33b679","selected":true,"accessRole":"reader"},
            {"id":"hidden@example.com","summaryOverride":"Private alias","selected":true,"hidden":true,"accessRole":"writer"},
            {"id":"gone@example.com","deleted":true}
          ]
        }"##;
        let (items, next) = parse_calendar_list(body).unwrap();
        assert_eq!(next.as_deref(), Some("page-2"));
        assert_eq!(items.len(), 3);
        assert!(items[0].primary);
        assert!(!items[0].read_only);
        assert_eq!(items[1].name.as_deref(), Some("Birthdays"));
        assert!(items[1].read_only);
        assert!(items[1].selected);
        assert_eq!(items[2].name.as_deref(), Some("Private alias"));
        assert!(!items[2].selected);
    }

    #[test]
    fn primary_defaults_to_selected_when_google_omits_the_flag() {
        let (items, _) = parse_calendar_list(r#"{"items":[{"id":"me","primary":true}]}"#).unwrap();
        assert!(items[0].selected);
    }

    #[test]
    fn reconciliation_uses_primary_as_default_and_encodes_special_ids() {
        let conn = crate::db::testutil::calendar_conn();
        let google = repo::caldav::tests::add_connection(
            &conn,
            1,
            crate::models::CalendarConnectionKind::Google,
        );
        let calendars = vec![
            GoogleCalendar {
                id: "#contacts@group.v.calendar.google.com".into(),
                name: Some("Birthdays".into()),
                color: Some("#f6bf26".into()),
                primary: false,
                read_only: true,
                selected: true,
            },
            GoogleCalendar {
                id: "me@example.com".into(),
                name: Some("Me".into()),
                color: Some("#4285f4".into()),
                primary: true,
                read_only: false,
                selected: true,
            },
        ];
        let stored = reconcile_calendars(&conn, &google, &calendars).unwrap();
        assert_eq!(stored.len(), 2);
        let primary = stored.iter().find(|calendar| calendar.is_default).unwrap();
        assert_eq!(primary.display_name.as_deref(), Some("Me"));
        let birthdays = stored
            .iter()
            .find(|calendar| calendar.display_name.as_deref() == Some("Birthdays"))
            .unwrap();
        assert!(birthdays.read_only);
        assert!(
            birthdays
                .url
                .contains("%23contacts@group.v.calendar.google.com")
        );

        // A calendar that left the list is forgotten.
        let stored = reconcile_calendars(&conn, &google, &calendars[1..]).unwrap();
        assert_eq!(stored.len(), 1);
        assert!(stored[0].is_default);
    }

    #[test]
    fn a_shared_calendar_already_added_over_caldav_stays_there() {
        let conn = crate::db::testutil::calendar_conn();
        let shared = repo::caldav::tests::add_connection(
            &conn,
            1,
            crate::models::CalendarConnectionKind::Generic,
        );
        let legacy = "https://www.google.com/calendar/dav/shared@example.com/events/";
        let shared_calendar =
            repo::caldav::upsert_calendar(&conn, &shared, legacy, Some("Shared"), None, false)
                .unwrap();
        let google = repo::caldav::tests::add_connection(
            &conn,
            1,
            crate::models::CalendarConnectionKind::Google,
        );
        let listed = [
            GoogleCalendar {
                id: "me@example.com".into(),
                name: Some("Me".into()),
                color: None,
                primary: true,
                read_only: false,
                selected: true,
            },
            GoogleCalendar {
                id: "shared@example.com".into(),
                name: Some("Shared".into()),
                color: None,
                primary: false,
                read_only: false,
                selected: true,
            },
        ];
        let stored = reconcile_calendars(&conn, &google, &listed).unwrap();
        assert_eq!(
            stored
                .iter()
                .map(|calendar| calendar.display_name.as_deref())
                .collect::<Vec<_>>(),
            vec![Some("Me")]
        );
        let kept = repo::caldav::get_calendar(&conn, shared_calendar)
            .unwrap()
            .unwrap();
        assert_eq!(kept.connection_id, shared.id);
    }
}
