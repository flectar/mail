//! RFC 6764/6352 discovery from a service URL to address-book collections.

use super::{
    http::{Transport, validated_url},
    xml,
};
use crate::error::{CoreError, Result};
use crate::models::CardDavKind;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredAddressBook {
    pub url: String,
    pub display_name: Option<String>,
    pub read_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovery {
    pub principal_url: Option<String>,
    pub home_set_url: String,
    pub addressbooks: Vec<DiscoveredAddressBook>,
}

pub fn resolve(base: &str, href: &str) -> Result<String> {
    let base = validated_url(base)?;
    let joined = base
        .join(href)
        .map_err(|e| super::err(format!("invalid href {href}: {e}")))?;
    validated_url(joined.as_str())?;
    if joined.origin() != base.origin() {
        return Err(super::err(
            "address-book href changed origin; refusing to forward credentials",
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
    let response = t
        .request(
            "PROPFIND",
            url,
            Some(depth),
            &[],
            Some("application/xml; charset=utf-8"),
            Some(body),
        )
        .await?;
    if response.status != 207 {
        return Err(super::err(format!(
            "PROPFIND {url}: HTTP {}",
            response.status
        )));
    }
    xml::parse(&response.body)
}

/// The address books of a connection of `kind` at `base_url`.
pub async fn discover_for(
    kind: CardDavKind,
    t: &dyn Transport,
    base_url: &str,
) -> Result<Discovery> {
    match kind {
        CardDavKind::Generic => discover(t, base_url).await,
        CardDavKind::Google => super::google::discover(t, base_url).await,
    }
}

pub async fn discover(t: &dyn Transport, base_url: &str) -> Result<Discovery> {
    validated_url(base_url)?;
    // A pasted collection URL is a useful and common shortcut.
    if let Ok(multistatus) = propfind(t, base_url, "0", xml::collections()).await
        && let Some(item) = multistatus.items.iter().find(|item| item.is_addressbook)
    {
        return Ok(Discovery {
            principal_url: None,
            home_set_url: base_url.into(),
            addressbooks: vec![DiscoveredAddressBook {
                url: base_url.into(),
                display_name: item.display_name.clone(),
                read_only: item.can_write == Some(false),
            }],
        });
    }

    let mut principal = None;
    for candidate in [
        base_url.to_owned(),
        resolve(base_url, "/.well-known/carddav")?,
    ] {
        match propfind(t, &candidate, "0", xml::principal()).await {
            Ok(ms) => {
                if let Some(href) = ms.items.iter().find_map(|item| item.principal.clone()) {
                    principal = Some(resolve(&candidate, &href)?);
                    break;
                }
            }
            Err(CoreError::NeedsReauth) => return Err(CoreError::NeedsReauth),
            Err(_) => {}
        }
    }
    let principal = principal.ok_or_else(|| super::err("no current-user-principal found"))?;
    let home_response = propfind(t, &principal, "0", xml::home_set()).await?;
    let home_href = home_response
        .items
        .iter()
        .find_map(|item| item.home_set.clone())
        .ok_or_else(|| super::err("no addressbook-home-set found"))?;
    let home = resolve(&principal, &home_href)?;
    let listing = propfind(t, &home, "1", xml::collections()).await?;
    let addressbooks = listing
        .items
        .iter()
        .filter(|item| item.is_addressbook && !item.href.is_empty())
        .map(|item| {
            Ok(DiscoveredAddressBook {
                url: resolve(&home, &item.href)?,
                display_name: item.display_name.clone(),
                read_only: item.can_write == Some(false),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    if addressbooks.is_empty() {
        return Err(super::err("no address books found on the server"));
    }
    Ok(Discovery {
        principal_url: Some(principal),
        home_set_url: home,
        addressbooks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::carddav::http::{DavResponse, MockTransport};
    fn response(body: &str) -> DavResponse {
        DavResponse {
            status: 207,
            body: body.into(),
            etag: None,
            location: None,
        }
    }

    #[tokio::test]
    async fn discovers_a_standard_addressbook_home() {
        let not_book = r#"<d:multistatus xmlns:d="DAV:"><d:response><d:href>/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#;
        let principal = r#"<d:multistatus xmlns:d="DAV:"><d:response><d:href>/</d:href><d:propstat><d:prop><d:current-user-principal><d:href>/principals/me/</d:href></d:current-user-principal></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#;
        let home = r#"<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:carddav"><d:response><d:href>/principals/me/</d:href><d:propstat><d:prop><c:addressbook-home-set><d:href>/dav/addressbooks/me/</d:href></c:addressbook-home-set></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#;
        let books = r#"<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:carddav"><d:response><d:href>/dav/addressbooks/me/contacts/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/><c:addressbook/></d:resourcetype><d:displayname>Contacts</d:displayname><d:current-user-privilege-set><d:privilege><d:read/></d:privilege></d:current-user-privilege-set></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#;
        let transport = MockTransport::new(vec![
            response(not_book),
            response(principal),
            response(home),
            response(books),
        ]);
        let found = discover(&transport, "https://dav.example.test/")
            .await
            .unwrap();
        assert_eq!(
            found.home_set_url,
            "https://dav.example.test/dav/addressbooks/me/"
        );
        assert_eq!(
            found.addressbooks[0].display_name.as_deref(),
            Some("Contacts")
        );
        assert!(found.addressbooks[0].read_only);
    }

    #[test]
    fn refuses_cross_origin_hrefs() {
        assert!(resolve("https://dav.example.test/", "https://evil.example/a/").is_err());
    }
}
