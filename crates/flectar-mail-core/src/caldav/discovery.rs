//! Bootstrap: from a base URL to the list of VEVENT-capable collections.
//! current-user-principal -> calendar-home-set -> Depth:1 listing (RFC 4791),
//! with two shortcuts: a pasted URL that already is a calendar collection is
//! used directly, and a bare host tries /.well-known/caldav (RFC 6764).

use super::xml;
use super::{Transport, err};
use crate::error::Result;

#[derive(Debug, Clone)]
pub struct DiscoveredCalendar {
    /// Absolute collection URL.
    pub url: String,
    pub display_name: Option<String>,
    pub color: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Discovery {
    pub principal_url: Option<String>,
    pub home_set_url: String,
    pub calendars: Vec<DiscoveredCalendar>,
}

/// Resolve a (possibly relative) href against the URL it came from.
pub fn resolve(base: &str, href: &str) -> Result<String> {
    let base = super::http::validated_dav_url(base)?;
    let joined = base
        .join(href)
        .map_err(|e| err(format!("bad href {href}: {e}")))?;
    super::http::validated_dav_url(joined.as_str())?;
    if joined.origin() != base.origin() {
        return Err(err(
            "calendar href changed origin; refusing to forward credentials",
        ));
    }
    Ok(joined.to_string())
}

async fn propfind(
    t: &dyn Transport,
    url: &str,
    depth: &str,
    body: String,
) -> Result<xml::Multistatus> {
    let resp = t
        .request("PROPFIND", url, Some(depth), &[], Some(body))
        .await?;
    if resp.status != 207 {
        return Err(err(propfind_error_message(url, resp.status, &resp.body)));
    }
    xml::parse_multistatus(&resp.body)
}

fn propfind_error_message(url: &str, status: u16, body: &str) -> String {
    let detail = response_excerpt(body);
    let suffix = if detail.is_empty() {
        String::new()
    } else {
        format!(" Google response: {detail}")
    };
    if url.starts_with(super::GOOGLE_CALDAV_BASE) {
        return match status {
            403 => format!(
                "Enable CalDAV API (caldav.googleapis.com) in the OAuth project's Google Cloud API Library; Google rejected calendar access with HTTP 403.{suffix}"
            ),
            404 => format!(
                "Google could not find this calendar (HTTP 404). Use the account's primary Google email as its Calendar ID and confirm Calendar is available for that user.{suffix}"
            ),
            429 => format!(
                "Google Calendar is temporarily rate-limiting this account (HTTP 429). Retry after a short wait.{suffix}"
            ),
            500..=599 => format!(
                "Google Calendar is temporarily unavailable (HTTP {status}). Retry automatically or try again later.{suffix}"
            ),
            _ => format!("Google CalDAV PROPFIND failed with HTTP {status}.{suffix}"),
        };
    }
    format!("PROPFIND {url}: HTTP {status}.{suffix}")
}

fn response_excerpt(body: &str) -> String {
    const MAX_CHARS: usize = 240;
    crate::http_body::single_line_excerpt(body, MAX_CHARS)
}

/// How a CalDAV server schedules for the account (RFC 6638): whether it
/// sends attendees' answers itself, and the addresses it does that for.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Scheduling {
    pub auto_schedule: bool,
    /// Lowercase email addresses from the principal's
    /// calendar-user-address-set.
    pub addresses: Vec<String>,
}

impl Scheduling {
    /// The server tells the organizer when `attendee` changes their answer
    /// in their own copy.
    pub fn answers_for(&self, attendee: &str) -> bool {
        self.auto_schedule
            && self
                .addresses
                .iter()
                .any(|address| address.eq_ignore_ascii_case(attendee.trim()))
    }
}

/// Ask whether the server schedules: `calendar-auto-schedule` in the DAV
/// header of a calendar's OPTIONS (RFC 6638 §2), and the principal's
/// calendar-user-address-set (§2.4.1), the addresses it schedules for.
pub async fn scheduling(
    t: &dyn Transport,
    calendar_url: &str,
    principal_url: Option<&str>,
) -> Result<Scheduling> {
    let options = t.request("OPTIONS", calendar_url, None, &[], None).await?;
    let auto_schedule = options.dav.as_deref().is_some_and(|features| {
        features.split(',').any(|feature| {
            feature
                .trim()
                .eq_ignore_ascii_case("calendar-auto-schedule")
        })
    });
    let Some(principal_url) = principal_url.filter(|_| auto_schedule) else {
        return Ok(Scheduling {
            auto_schedule,
            addresses: Vec::new(),
        });
    };
    let ms = propfind(t, principal_url, "0", xml::propfind_scheduling()).await?;
    let addresses = ms
        .items
        .iter()
        .flat_map(|item| &item.calendar_user_addresses)
        .filter_map(|href| {
            let href = href.trim();
            href.get(..7)
                .filter(|scheme| scheme.eq_ignore_ascii_case("mailto:"))
                .map(|_| href[7..].trim().to_ascii_lowercase())
        })
        .collect();
    Ok(Scheduling {
        auto_schedule,
        addresses,
    })
}

/// Full discovery from `base_url`. Connection test = this succeeding with at
/// least one usable collection.
pub async fn discover(t: &dyn Transport, base_url: &str) -> Result<Discovery> {
    super::http::validated_dav_url(base_url)?;
    // Shortcut: the URL is itself a calendar collection.
    if let Ok(ms) = propfind(t, base_url, "0", xml::propfind_collections()).await
        && let Some(item) = ms.items.first()
        && item.is_vevent_calendar()
    {
        return Ok(Discovery {
            principal_url: None,
            home_set_url: base_url.to_string(),
            calendars: vec![DiscoveredCalendar {
                url: base_url.to_string(),
                display_name: item.displayname.clone(),
                color: item.color.clone(),
            }],
        });
    }

    // Find the principal: the given URL first, then /.well-known/caldav.
    let mut principal: Option<String> = None;
    for candidate in [
        base_url.to_string(),
        resolve(base_url, "/.well-known/caldav")?,
    ] {
        match propfind(t, &candidate, "0", xml::propfind_principal()).await {
            Ok(ms) => {
                if let Some(p) = ms
                    .items
                    .iter()
                    .find_map(|i| i.current_user_principal.clone())
                {
                    principal = Some(resolve(&candidate, &p)?);
                    break;
                }
            }
            Err(crate::error::CoreError::NeedsReauth) => {
                return Err(crate::error::CoreError::NeedsReauth);
            }
            Err(_) => continue,
        }
    }
    let principal = principal.ok_or_else(|| err("no current-user-principal found"))?;

    // Principal -> calendar home set.
    let ms = propfind(t, &principal, "0", xml::propfind_home_set()).await?;
    let home = ms
        .items
        .iter()
        .find_map(|i| i.calendar_home_set.clone())
        .ok_or_else(|| err("no calendar-home-set on principal"))?;
    let home = resolve(&principal, &home)?;

    // Home set -> collections.
    let ms = propfind(t, &home, "1", xml::propfind_collections()).await?;
    let calendars: Vec<DiscoveredCalendar> = ms
        .items
        .iter()
        .filter(|i| i.is_vevent_calendar() && !i.href.is_empty())
        .map(|i| {
            Ok(DiscoveredCalendar {
                url: resolve(&home, &i.href)?,
                display_name: i.displayname.clone(),
                color: i.color.clone(),
            })
        })
        .collect::<Result<_>>()?;

    if calendars.is_empty() {
        return Err(err("no calendars found on the server"));
    }
    Ok(Discovery {
        principal_url: Some(principal),
        home_set_url: home,
        calendars,
    })
}

/// Validate and describe a calendar collection whose URL is already known.
///
/// Google documents `/{calendar_id}/events` as a calendar collection entry
/// point, but its Depth:0 response does not consistently repeat the CalDAV
/// `calendar` resource type. A successful property response is sufficient in
/// this case; requiring generic principal discovery after it rejects a valid
/// Google collection with a misleading `no current-user-principal` error.
pub async fn discover_known_collection(t: &dyn Transport, url: &str) -> Result<Discovery> {
    super::http::validated_dav_url(url)?;
    let ms = propfind(t, url, "0", xml::propfind_collections()).await?;
    let item = ms
        .items
        .iter()
        .find(|item| (200..300).contains(&item.status))
        .ok_or_else(|| err("calendar collection returned no successful properties"))?;
    Ok(Discovery {
        principal_url: None,
        home_set_url: url.to_string(),
        calendars: vec![DiscoveredCalendar {
            url: url.to_string(),
            display_name: item.displayname.clone(),
            color: item.color.clone(),
        }],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::caldav::http::{DavResponse, MockTransport};

    fn ms(body: &str) -> DavResponse {
        DavResponse {
            dav: None,
            status: 207,
            etag: None,
            body: body.into(),
        }
    }

    const PRINCIPAL: &str = r#"<d:multistatus xmlns:d="DAV:"><d:response><d:href>/</d:href><d:propstat><d:prop><d:current-user-principal><d:href>/principals/me/</d:href></d:current-user-principal></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#;
    const HOME: &str = r#"<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:response><d:href>/principals/me/</d:href><d:propstat><d:prop><c:calendar-home-set><d:href>/cal/me/</d:href></c:calendar-home-set></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#;
    const COLLECTIONS: &str = r#"<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:response><d:href>/cal/me/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response><d:response><d:href>/cal/me/work/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/><c:calendar/></d:resourcetype><d:displayname>Work</d:displayname></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#;
    const NOT_A_CALENDAR: &str = r#"<d:multistatus xmlns:d="DAV:"><d:response><d:href>/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#;

    #[tokio::test]
    async fn asks_whether_the_server_schedules_and_for_whom() {
        let options = |features: &str| DavResponse {
            status: 200,
            dav: Some(features.into()),
            ..Default::default()
        };
        let addresses = r#"<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:response><d:href>/principals/me/</d:href><d:propstat><d:prop><c:calendar-user-address-set><d:href>MAILTO:Me@Example.com</d:href><d:href>/principals/me/</d:href></c:calendar-user-address-set></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#;
        let calendar = "https://dav.example.com/cal/me/work/";
        let principal = Some("https://dav.example.com/principals/me/");
        let t = MockTransport::new(vec![
            options("1, 2, access-control, calendar-access, calendar-auto-schedule"),
            ms(addresses),
        ]);
        let found = scheduling(&t, calendar, principal).await.unwrap();
        assert!(found.auto_schedule);
        assert_eq!(found.addresses, ["me@example.com"]);
        assert!(found.answers_for("ME@example.com"));
        assert!(!found.answers_for("alias@example.com"));
        let seen: Vec<(String, String)> = t
            .seen
            .lock()
            .unwrap()
            .iter()
            .map(|(method, url, _)| (method.clone(), url.clone()))
            .collect();
        assert_eq!(seen[0], ("OPTIONS".to_string(), calendar.to_string()));
        assert_eq!(seen[1].0, "PROPFIND");

        // A server that doesn't schedule isn't asked for addresses.
        let t = MockTransport::new(vec![options("1, 2, 3, calendar-access, addressbook")]);
        let found = scheduling(&t, calendar, principal).await.unwrap();
        assert_eq!(found, Scheduling::default());
        assert!(!found.answers_for("me@example.com"));
    }

    #[tokio::test]
    async fn full_discovery_chain() {
        let t = MockTransport::new(vec![
            ms(NOT_A_CALENDAR), // direct-collection probe: not a calendar
            ms(PRINCIPAL),
            ms(HOME),
            ms(COLLECTIONS),
        ]);
        let d = discover(&t, "https://dav.example.com/").await.unwrap();
        assert_eq!(
            d.principal_url.as_deref(),
            Some("https://dav.example.com/principals/me/")
        );
        assert_eq!(d.home_set_url, "https://dav.example.com/cal/me/");
        assert_eq!(d.calendars.len(), 1);
        assert_eq!(d.calendars[0].url, "https://dav.example.com/cal/me/work/");
        assert_eq!(d.calendars[0].display_name.as_deref(), Some("Work"));
    }

    #[tokio::test]
    async fn direct_collection_shortcut() {
        let direct = r#"<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:response><d:href>/cal/me/personal/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/><c:calendar/></d:resourcetype><d:displayname>Personal</d:displayname></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#;
        let t = MockTransport::new(vec![ms(direct)]);
        let d = discover(&t, "https://dav.example.com/cal/me/personal/")
            .await
            .unwrap();
        assert_eq!(d.calendars.len(), 1);
        assert_eq!(
            d.calendars[0].url,
            "https://dav.example.com/cal/me/personal/"
        );
    }

    #[tokio::test]
    async fn known_collection_does_not_require_a_repeated_resource_type() {
        let t = MockTransport::new(vec![ms(NOT_A_CALENDAR)]);
        let url = "https://apidata.googleusercontent.com/caldav/v2/me%40example.com/events";
        let d = discover_known_collection(&t, url).await.unwrap();
        assert_eq!(d.principal_url, None);
        assert_eq!(d.home_set_url, url);
        assert_eq!(d.calendars[0].url, url);
        assert_eq!(t.seen.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn google_forbidden_error_names_the_separate_caldav_api() {
        let t = MockTransport::new(vec![DavResponse {
            status: 403,
            body: "accessNotConfigured".into(),
            ..Default::default()
        }]);
        let error = discover_known_collection(
            &t,
            "https://apidata.googleusercontent.com/caldav/v2/me%40example.com/events",
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(error.contains("Enable CalDAV API (caldav.googleapis.com)"));
        assert!(error.contains("accessNotConfigured"));
    }

    #[test]
    fn provider_response_excerpt_is_bounded_and_single_line() {
        let excerpt = response_excerpt(&format!("first\n{}", "x".repeat(300)));
        assert!(!excerpt.contains('\n'));
        assert!(excerpt.ends_with('…'));
        assert_eq!(excerpt.chars().count(), 241);
    }

    #[test]
    fn hrefs_cannot_move_credentials_to_another_origin() {
        let base = "https://dav.example.com/cal/me/";
        assert_eq!(
            resolve(base, "work/event.ics").unwrap(),
            "https://dav.example.com/cal/me/work/event.ics"
        );
        assert!(resolve(base, "https://attacker.example/event.ics").is_err());
        assert!(resolve(base, "//attacker.example/event.ics").is_err());
        assert!(resolve(base, "https://dav.example.com:444/event.ics").is_err());
    }
}
