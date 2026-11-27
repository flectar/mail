//! Google Contacts over CardDAV (#423). Google's CardDAV interface signs in
//! only with OAuth, so a Gmail account reaches its contacts with its own
//! tokens once its consent includes the contacts scope. This module names the
//! account's address book, and turns Google's JSON refusals into what the
//! person can do about them. How Google's sync differs from the RFCs is in
//! `super::sync`.

use super::discovery::{DiscoveredAddressBook, Discovery};
use super::http::{DavAuth, DavResponse, HttpTransport, Transport};
use super::xml;
use crate::error::{CoreError, Result};
use crate::net::proxy::Owner;

/// Google's CardDAV host, where every peer client starts.
const ORIGIN: &str = "https://www.googleapis.com";

/// Whether `url` is on Google's CardDAV servers. Google's sync departs from
/// the RFCs whichever way the connection signs in, so peer clients tell it
/// by its host, as this one does.
pub fn is_google(url: &str) -> bool {
    url::Url::parse(url).is_ok_and(|url| {
        url.host_str().is_some_and(|host| {
            host.eq_ignore_ascii_case("www.googleapis.com")
                || host.eq_ignore_ascii_case("apidata.googleusercontent.com")
        })
    })
}

/// The address book of the Google account `email`. Google documents one,
/// `lists/default/` under the account's principal. Its well-known address
/// answers with a 301 there, which an HTTP client follows as a GET, so the
/// book is named directly, as the Google calendar's is.
pub fn start_url(email: &str) -> Result<String> {
    let email = email.trim();
    if email.is_empty() {
        return Err(super::err("a Google account needs its address"));
    }
    let mut url = url::Url::parse(ORIGIN)
        .map_err(|error| super::err(format!("invalid Google CardDAV address: {error}")))?;
    url.path_segments_mut()
        .map_err(|_| super::err("invalid Google CardDAV address"))?
        .clear()
        .extend(["carddav", "v1", "principals", email, "lists", "default", ""]);
    Ok(url.to_string())
}

/// The account's address book at `base_url`. Google has left out
/// `resourcetype`, so any successful answer counts, as CalDAV's
/// `discover_known_collection` does for a Google calendar.
pub async fn discover(t: &dyn Transport, base_url: &str) -> Result<Discovery> {
    let response = t
        .request(
            "PROPFIND",
            base_url,
            Some("0"),
            &[],
            Some("application/xml; charset=utf-8"),
            Some(xml::collections()),
        )
        .await?;
    if response.status != 207 {
        return Err(super::err(format!(
            "Google Contacts answered HTTP {} for the address book",
            response.status
        )));
    }
    let item = xml::parse(&response.body)?
        .items
        .into_iter()
        .find(|item| (200..300).contains(&item.status))
        .ok_or_else(|| super::err("Google Contacts returned no address book"))?;
    Ok(Discovery {
        principal_url: None,
        home_set_url: base_url.into(),
        addressbooks: vec![DiscoveredAddressBook {
            url: base_url.into(),
            display_name: item.display_name,
            read_only: item.can_write == Some(false),
        }],
    })
}

/// Requests to Google's origin, normally with the account's access token.
pub fn transport(owner: Owner, auth: DavAuth, base_url: &str) -> Result<GoogleTransport> {
    Ok(GoogleTransport {
        inner: HttpTransport::new(owner, auth, base_url)?,
    })
}

pub struct GoogleTransport {
    inner: HttpTransport,
}

#[async_trait::async_trait]
impl Transport for GoogleTransport {
    async fn request(
        &self,
        method: &str,
        url: &str,
        depth: Option<&str>,
        headers: &[(&str, &str)],
        content_type: Option<&str>,
        body: Option<String>,
    ) -> Result<DavResponse> {
        let response = self
            .inner
            .request(method, url, depth, headers, content_type, body)
            .await?;
        match refusal(&response) {
            Some(error) => Err(error),
            None => Ok(response),
        }
    }
}

/// What a Google refusal means, when it isn't a WebDAV answer the sync
/// handles itself, such as a contact the account may only read.
fn refusal(response: &DavResponse) -> Option<CoreError> {
    if response.status != 403 && response.status != 429 {
        return None;
    }
    let reasons = reasons(&response.body);
    let has = |wanted: &[&str]| {
        reasons
            .iter()
            .any(|reason| wanted.contains(&reason.as_str()))
    };
    if has(&["ACCESS_TOKEN_SCOPE_INSUFFICIENT", "insufficientPermissions"]) {
        // The token lacks the contacts scope, which another consent adds.
        return Some(CoreError::NeedsReauth);
    }
    if has(&["SERVICE_DISABLED", "accessNotConfigured"]) {
        return Some(super::err(
            "The Google Contacts CardDAV API (carddav.googleapis.com) is off in the Google Cloud project of this app's keys. Turn it on there.",
        ));
    }
    if response.status == 429
        || has(&[
            "RATE_LIMIT_EXCEEDED",
            "rateLimitExceeded",
            "userRateLimitExceeded",
        ])
    {
        return Some(super::err(
            "Google is limiting how often this account reaches its contacts. They sync again later.",
        ));
    }
    None
}

/// The reasons of a Google JSON error, in its older `errors` list or its
/// newer `details`.
fn reasons(body: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return Vec::new();
    };
    let error = &value["error"];
    let listed = error["errors"].as_array().into_iter().flatten();
    let detailed = error["details"].as_array().into_iter().flatten();
    listed
        .chain(detailed)
        .filter_map(|entry| entry["reason"].as_str())
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::carddav::http::MockTransport;

    #[test]
    fn tells_googles_servers_by_their_host() {
        assert!(is_google(
            "https://www.googleapis.com/carddav/v1/principals/a@gmail.com/lists/default/"
        ));
        assert!(is_google(
            "https://apidata.googleusercontent.com/carddav/v1/principals/a@gmail.com/lists/default/"
        ));
        assert!(!is_google("https://dav.example.com/carddav/"));
        assert!(!is_google("https://www.googleapis.com.example.com/"));
        assert!(!is_google("not a url"));
    }

    #[test]
    fn names_the_accounts_own_address_book() {
        assert_eq!(
            start_url("ana.lopez+work@gmail.com").unwrap(),
            "https://www.googleapis.com/carddav/v1/principals/ana.lopez+work@gmail.com/lists/default/"
        );
        assert!(start_url(" ").is_err());
    }

    #[tokio::test]
    async fn the_address_book_counts_without_a_resourcetype() {
        let url = start_url("ana@gmail.com").unwrap();
        let answer = r#"<d:multistatus xmlns:d="DAV:"><d:response><d:href>/carddav/v1/principals/ana@gmail.com/lists/default/</d:href><d:propstat><d:prop><d:displayname>Address Book</d:displayname></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#;
        let transport = MockTransport::new(vec![DavResponse {
            status: 207,
            body: answer.into(),
            ..Default::default()
        }]);
        let found = discover(&transport, &url).await.unwrap();
        assert_eq!(
            found.addressbooks,
            [DiscoveredAddressBook {
                url: url.clone(),
                display_name: Some("Address Book".into()),
                read_only: false,
            }]
        );
        {
            let seen = transport.seen.lock().unwrap();
            assert_eq!(seen[0].0, "PROPFIND");
            assert_eq!(seen[0].5.as_deref(), Some("0"));
        }
        let missing = MockTransport::new(vec![DavResponse {
            status: 404,
            ..Default::default()
        }]);
        assert!(discover(&missing, &url).await.is_err());
    }

    fn answer(status: u16, body: &str) -> Option<String> {
        refusal(&DavResponse {
            status,
            etag: None,
            body: body.into(),
            location: None,
        })
        .map(|error| match error {
            CoreError::NeedsReauth => "needs_reauth".into(),
            other => other.to_string(),
        })
    }

    #[test]
    fn google_refusals_say_what_to_do() {
        let scope = r#"{"error":{"code":403,"message":"Request had insufficient authentication scopes.","status":"PERMISSION_DENIED","details":[{"@type":"type.googleapis.com/google.rpc.ErrorInfo","reason":"ACCESS_TOKEN_SCOPE_INSUFFICIENT","domain":"googleapis.com"}]}}"#;
        assert_eq!(answer(403, scope).as_deref(), Some("needs_reauth"));
        let disabled = r#"{"error":{"code":403,"message":"Google Contacts CardDAV API has not been used in project 1 before or it is disabled.","errors":[{"message":"Access Not Configured.","domain":"usageLimits","reason":"accessNotConfigured"}],"status":"PERMISSION_DENIED","details":[{"@type":"type.googleapis.com/google.rpc.ErrorInfo","reason":"SERVICE_DISABLED","domain":"googleapis.com"}]}}"#;
        assert!(
            answer(403, disabled)
                .unwrap()
                .contains("Google Contacts CardDAV API (carddav.googleapis.com) is off")
        );
        let limited = r#"{"error":{"code":403,"message":"Rate Limit Exceeded","errors":[{"reason":"rateLimitExceeded"}]}}"#;
        assert!(answer(403, limited).unwrap().contains("sync again later"));
        assert!(answer(429, "").unwrap().contains("sync again later"));
    }

    #[test]
    fn webdav_answers_reach_the_sync() {
        // A read-only contact, a missing one and a conflict are the sync's.
        assert_eq!(answer(403, "<d:error xmlns:d=\"DAV:\"/>"), None);
        assert_eq!(answer(403, ""), None);
        assert_eq!(answer(404, "{}"), None);
        assert_eq!(answer(412, ""), None);
        assert_eq!(answer(207, "<d:multistatus xmlns:d=\"DAV:\"/>"), None);
    }
}
