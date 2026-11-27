//! ETag-safe CardDAV push followed by incremental or full collection pull.

use super::{
    discovery::resolve,
    http::Transport,
    vcard::{self, Card},
    xml,
};
use crate::{
    db::{Db, repo},
    error::{CoreError, Result},
    events::{CoreEvent, EventBus},
    models::now_ms,
};
use std::collections::{HashMap, HashSet, VecDeque};

const MULTIGET_BATCH: usize = 50;

fn required_etag(href: &str, etag: Option<&str>) -> Result<String> {
    etag.map(str::trim)
        .filter(|etag| !etag.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| super::err(format!("CardDAV resource {href} has no required ETag")))
}

fn href_key(collection_url: &str, href: &str) -> Result<String> {
    let resolved = resolve(collection_url, href)?;
    let url = url::Url::parse(&resolved)
        .map_err(|error| super::err(format!("invalid CardDAV href {href}: {error}")))?;
    let mut key = url.path().to_owned();
    if let Some(query) = url.query() {
        key.push('?');
        key.push_str(query);
    }
    Ok(key)
}

async fn accept_server_version(
    db: &Db,
    transport: &dyn Transport,
    book_id: i64,
    book_url: &str,
    href: &str,
) -> Result<()> {
    let target = resolve(book_url, href)?;
    let response = transport
        .request(
            "GET",
            &target,
            None,
            &[("Accept", "text/vcard")],
            None,
            None,
        )
        .await?;
    if response.status == 404 {
        let href = href.to_owned();
        db.write(move |conn| repo::carddav::remove_remote(conn, book_id, &href))
            .await?;
        return Ok(());
    }
    if !response.ok() {
        return Err(super::err(format!(
            "GET {target} after an ETag conflict: HTTP {}",
            response.status
        )));
    }
    let etag = required_etag(&target, response.etag.as_deref())?;
    let href = href.to_owned();
    let card = response.body;
    let parsed = Card::parse(card);
    db.write(move |conn| {
        let tx = conn.transaction()?;
        match parsed {
            Ok(card) => repo::carddav::apply_remote(&tx, book_id, &href, Some(&etag), &card)?,
            Err(error) => {
                tracing::warn!(%href, %error, "unsupported CardDAV conflict version");
                repo::carddav::record_resource_error(
                    &tx,
                    book_id,
                    &href,
                    Some(&etag),
                    &error.to_string(),
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    })
    .await
}

pub async fn sync_account(
    db: &Db,
    bus: &EventBus,
    transport: &dyn Transport,
    account_id: i64,
) -> Result<bool> {
    let mut changed = push_dirty(db, transport, account_id).await?;
    let books = db
        .read(move |conn| repo::carddav::list_addressbooks(conn, Some(account_id)))
        .await?;
    for book in books.into_iter().filter(|book| book.enabled) {
        changed |= sync_addressbook(db, transport, book.id, &book.url).await?;
    }
    if changed {
        bus.emit(CoreEvent::ContactsUpdated { account_id });
    }
    Ok(changed)
}

/// Upload local changes. Google takes new contacts by POST to the address
/// book's add-member URL (RFC 5995): a PUT lands at another address under
/// another UID, which the next sync would bring back as a second contact.
async fn push_dirty(db: &Db, transport: &dyn Transport, account_id: i64) -> Result<bool> {
    let objects = db
        .read(move |conn| repo::carddav::pending_uploads(conn, account_id))
        .await?;
    let changed = !objects.is_empty();
    // Where each address book takes new contacts, asked once a run.
    let mut add_members = HashMap::<i64, Option<String>>::new();
    for object in objects {
        let book_id = object.addressbook_id;
        let book_url = db
            .read(move |conn| repo::carddav::addressbook_url(conn, book_id))
            .await?
            .ok_or_else(|| super::err("address book no longer exists"))?;
        let target = resolve(&book_url, &object.href)?;
        if object.deleted {
            if !object.remote_exists {
                let id = object.id;
                db.write(move |conn| repo::carddav::forget_object(conn, id))
                    .await?;
                continue;
            }
            let etag = object.etag.as_deref().ok_or_else(|| {
                super::err(format!(
                    "cannot safely delete {} because the server supplied no ETag",
                    object.href
                ))
            })?;
            let headers = [("If-Match", etag)];
            let response = transport
                .request("DELETE", &target, None, &headers, None, None)
                .await?;
            match response.status {
                200..=299 | 404 => {
                    let id = object.id;
                    db.write(move |conn| repo::carddav::forget_object(conn, id))
                        .await?;
                }
                412 => {
                    // Preserve the deletion intent alongside the conflicting server version.
                    accept_server_version(db, transport, book_id, &book_url, &object.href).await?;
                }
                403 => {
                    db.write(move |conn| repo::carddav::mark_addressbook_read_only(conn, book_id))
                        .await?;
                    tracing::warn!(
                        book_id,
                        "CardDAV address book rejected DELETE; marking it read-only"
                    );
                }
                status => return Err(super::err(format!("DELETE {target}: HTTP {status}"))),
            }
            continue;
        }
        let Some(body) = object.document.clone() else {
            continue;
        };
        vcard::ensure_size(&body)?;
        if super::google::is_google(&book_url) && !object.remote_exists {
            let member_url = match add_members.get(&book_id) {
                Some(url) => url.clone(),
                None => {
                    let url = add_member_url(transport, &book_url).await?;
                    add_members.insert(book_id, url.clone());
                    url
                }
            };
            if let Some(member_url) = member_url {
                post_new_contact(db, transport, &object, &book_url, &member_url, body).await?;
                continue;
            }
        }
        let headers = if object.remote_exists {
            let etag = object.etag.as_deref().ok_or_else(|| {
                super::err(format!(
                    "cannot safely update {} because the server supplied no ETag",
                    object.href
                ))
            })?;
            vec![("If-Match", etag)]
        } else {
            vec![("If-None-Match", "*")]
        };
        let mut response = transport
            .request(
                "PUT",
                &target,
                None,
                &headers,
                Some("text/vcard; charset=utf-8"),
                Some(body.clone()),
            )
            .await?;
        if response.status == 403 {
            db.write(move |conn| repo::carddav::mark_addressbook_read_only(conn, book_id))
                .await?;
            tracing::warn!(
                book_id,
                "CardDAV address book rejected PUT; marking it read-only"
            );
            continue;
        }
        if response.status == 412 {
            // Also reconcile a creation whose successful response was lost.
            accept_server_version(db, transport, book_id, &book_url, &object.href).await?;
            continue;
        }
        if !response.ok() {
            return Err(super::err(format!(
                "PUT {target}: HTTP {}",
                response.status
            )));
        }
        let mut saved_body = body.clone();
        if response
            .etag
            .as_deref()
            .is_none_or(|etag| etag.trim().is_empty())
            && let Ok(get) = transport
                .request(
                    "GET",
                    &target,
                    None,
                    &[("Accept", "text/vcard")],
                    None,
                    None,
                )
                .await
            && get.ok()
            && vcard::parse(&get.body).is_ok()
        {
            saved_body = get.body;
            response.etag = get.etag;
        }
        let etag = response.etag.filter(|etag| !etag.trim().is_empty());
        let missing_etag = etag.is_none();
        let href = object.href.clone();
        record_upload(db, &object, href, body, saved_body, etag).await?;
        if missing_etag {
            return Err(super::err(format!(
                "server stored {target} but did not return its required ETag"
            )));
        }
    }
    Ok(changed)
}

/// Record what the server stored for `object`, now at `href`: its own version
/// when a newer local edit is waiting, or else the upload's acknowledgement.
async fn record_upload(
    db: &Db,
    object: &repo::carddav::PendingUpload,
    href: String,
    body: String,
    saved_body: String,
    etag: Option<String>,
) -> Result<()> {
    let id = object.id;
    let revision = object.revision;
    let book_id = object.addressbook_id;
    let moved = href != object.href;
    db.write(move |conn| {
        let tx = conn.transaction()?;
        if moved {
            repo::carddav::move_object(&tx, id, &href)?;
        }
        let newer: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM carddav_objects WHERE id=?1 AND revision<>?2)",
            rusqlite::params![id, revision],
            |r| r.get(0),
        )?;
        if newer && etag.is_some() && !vcard::equivalent(&body, &saved_body) {
            // A normalization or later server edit changed the document
            // while a newer local edit was pending. Retain both versions.
            // Without an ETag the resource is downloaded again first.
            repo::carddav::apply_remote(
                &tx,
                book_id,
                &href,
                etag.as_deref(),
                &Card::parse(saved_body.as_str())?,
            )?;
        } else {
            repo::carddav::acknowledge_upload(&tx, id, revision, etag.as_deref(), &saved_body)?;
        }
        tx.commit()?;
        Ok(())
    })
    .await
}

/// Where an address book takes new members by POST, when it says (RFC 5995).
async fn add_member_url(transport: &dyn Transport, book_url: &str) -> Result<Option<String>> {
    let response = transport
        .request(
            "PROPFIND",
            book_url,
            Some("0"),
            &[],
            Some("application/xml; charset=utf-8"),
            Some(xml::add_member()),
        )
        .await?;
    if response.status != 207 {
        return Ok(None);
    }
    xml::parse(&response.body)?
        .items
        .into_iter()
        .find_map(|item| item.add_member)
        .map(|href| resolve(book_url, &href))
        .transpose()
}

/// Create `object` by POST to `member_url`, then file it under the address
/// the server gave it, with the server's version of it, since Google
/// replaces the UID with its own.
async fn post_new_contact(
    db: &Db,
    transport: &dyn Transport,
    object: &repo::carddav::PendingUpload,
    book_url: &str,
    member_url: &str,
    body: String,
) -> Result<()> {
    let book_id = object.addressbook_id;
    let response = transport
        .request(
            "POST",
            member_url,
            None,
            &[],
            Some("text/vcard; charset=utf-8"),
            Some(body.clone()),
        )
        .await?;
    if response.status == 403 {
        db.write(move |conn| repo::carddav::mark_addressbook_read_only(conn, book_id))
            .await?;
        tracing::warn!(
            book_id,
            "CardDAV address book rejected POST; marking it read-only"
        );
        return Ok(());
    }
    if !response.ok() {
        return Err(super::err(format!(
            "POST {member_url}: HTTP {}",
            response.status
        )));
    }
    let Some(location) = response
        .location
        .as_deref()
        .map(str::trim)
        .filter(|location| !location.is_empty())
    else {
        // Stored somewhere unsaid, so it must not be posted again. The error
        // makes the next sync list the address book in full, which replaces
        // it with the server's copy.
        let id = object.id;
        db.write(move |conn| {
            repo::carddav::stop_upload(
                conn,
                id,
                "Server stored this contact without saying where; waiting to reconcile",
            )
        })
        .await?;
        return Ok(());
    };
    let href = href_key(book_url, location)?;
    let (saved_body, etag) = match fetch_member(transport, book_url, &href).await? {
        Some((etag, card)) => (card, Some(etag)),
        None => (body.clone(), None),
    };
    let missing_etag = etag.is_none();
    record_upload(db, object, href.clone(), body, saved_body, etag).await?;
    if missing_etag {
        return Err(super::err(format!(
            "server stored {href} but did not return its required ETag"
        )));
    }
    Ok(())
}

/// The ETag and vCard the server holds at `href`, read with a multiget, the
/// way Google documents reading contacts.
async fn fetch_member(
    transport: &dyn Transport,
    book_url: &str,
    href: &str,
) -> Result<Option<(String, String)>> {
    let response = transport
        .request(
            "REPORT",
            book_url,
            Some("0"),
            &[],
            Some("application/xml; charset=utf-8"),
            Some(xml::multiget(&[href.to_owned()])),
        )
        .await?;
    if response.status != 207 {
        return Ok(None);
    }
    for item in xml::parse(&response.body)?.items {
        if href_key(book_url, &item.href)? == href
            && (200..300).contains(&item.status)
            && let Some(etag) = item.etag.filter(|etag| !etag.trim().is_empty())
            && let Some(card) = item.address_data
            && vcard::parse(&card).is_ok()
        {
            return Ok(Some((etag, card)));
        }
    }
    Ok(None)
}

/// The address book's getctag and sync-token, when the server tells.
async fn read_state(
    transport: &dyn Transport,
    book_url: &str,
) -> Result<(Option<String>, Option<String>)> {
    let state = transport
        .request(
            "PROPFIND",
            book_url,
            Some("0"),
            &[],
            Some("application/xml; charset=utf-8"),
            Some(xml::state()),
        )
        .await?;
    if state.status != 207 {
        return Ok((None, None));
    }
    let parsed = xml::parse(&state.body)?;
    let first = parsed.items.first();
    Ok((
        first.and_then(|item| item.ctag.clone()),
        first.and_then(|item| item.sync_token.clone()),
    ))
}

/// Pull one address book's changes. Google departs from the RFCs in four
/// ways (#423): polling getctag is throttled, so a stored sync token is used
/// as it is; sync-collection needs Depth 1, since Depth 0 reports no
/// changes; an empty token is refused, so a first sync lists everything;
/// and addressbook-query isn't implemented, so the listing is a PROPFIND.
async fn sync_addressbook(
    db: &Db,
    transport: &dyn Transport,
    book_id: i64,
    book_url: &str,
) -> Result<bool> {
    let google = super::google::is_google(book_url);
    let (stored_ctag, stored_token) = db
        .read(move |conn| repo::carddav::sync_state(conn, book_id))
        .await?;
    let reads_state = !(google && stored_token.is_some());
    let (mut ctag, mut advertised_token) = if reads_state {
        read_state(transport, book_url).await?
    } else {
        (stored_ctag.clone(), None)
    };
    // Resources that failed to import are retried even when the collection is
    // unchanged, which needs a full listing rather than a sync-token delta.
    let retry_resources = db
        .read(move |conn| repo::carddav::has_failed_resources(conn, book_id))
        .await?;
    if reads_state && !retry_resources && ctag.is_some() && ctag == stored_ctag {
        return Ok(false);
    }

    let collection_href = href_key(book_url, book_url)?;
    let mut removed = Vec::new();
    let mut remote = Vec::<(String, Option<String>)>::new();
    let mut token = None;
    let mut incremental = false;
    let requested_token = if google {
        stored_token.clone()
    } else if stored_token.is_some() || advertised_token.is_some() {
        // An empty token requests the initial state.
        Some(stored_token.clone().unwrap_or_default())
    } else {
        None
    };
    if !retry_resources && let Some(requested_token) = requested_token {
        // RFC 6578 requires Depth 0; sync-level controls the collection
        // scope. Google answers Depth 0 as though nothing changed.
        let depth = if google { "1" } else { "0" };
        let response = transport
            .request(
                "REPORT",
                book_url,
                Some(depth),
                &[],
                Some("application/xml; charset=utf-8"),
                Some(xml::sync_collection(&requested_token)),
            )
            .await?;
        if response.status == 207 && !response.body.contains("valid-sync-token") {
            let parsed = xml::parse(&response.body)?;
            for item in parsed.items {
                let href = href_key(book_url, &item.href)?;
                if href.is_empty() || href == collection_href {
                    continue;
                }
                if item.status == 404 {
                    removed.push(href);
                } else if !(200..300).contains(&item.status) {
                    return Err(super::err(format!(
                        "sync-collection {href}: HTTP {}",
                        item.status
                    )));
                } else {
                    let etag = required_etag(&href, item.etag.as_deref())?;
                    remote.push((href, Some(etag)));
                }
            }
            token = parsed.sync_token.or(advertised_token.clone());
            incremental = true;
        }
    }
    if !incremental {
        let (listing, response) = if google {
            // A full listing starts from a fresh token, since the stored one
            // was refused or leaves out the resources to retry.
            if !reads_state {
                (ctag, advertised_token) = read_state(transport, book_url).await?;
            }
            let response = transport
                .request(
                    "PROPFIND",
                    book_url,
                    Some("1"),
                    &[],
                    Some("application/xml; charset=utf-8"),
                    Some(xml::etags()),
                )
                .await?;
            ("PROPFIND", response)
        } else {
            let response = transport
                .request(
                    "REPORT",
                    book_url,
                    Some("1"),
                    &[],
                    Some("application/xml; charset=utf-8"),
                    Some(xml::addressbook_query()),
                )
                .await?;
            ("addressbook-query", response)
        };
        if response.status != 207 {
            return Err(super::err(format!("{listing}: HTTP {}", response.status)));
        }
        let parsed = xml::parse(&response.body)?;
        for item in parsed.items {
            if item.href.is_empty() {
                return Err(super::err(format!("{listing} returned an empty href")));
            }
            let href = href_key(book_url, &item.href)?;
            // A listing names the collection too, with or without its slash.
            if href.trim_end_matches('/') == collection_href.trim_end_matches('/') {
                continue;
            }
            if !(200..300).contains(&item.status) {
                return Err(super::err(format!(
                    "{listing} {href}: HTTP {}",
                    item.status
                )));
            }
            let etag = required_etag(&href, item.etag.as_deref())?;
            remote.push((href, Some(etag)));
        }
        let remote_set = remote
            .iter()
            .map(|(href, _)| href.as_str())
            .collect::<HashSet<_>>();
        let local = db
            .read(move |conn| repo::carddav::object_etags(conn, book_id))
            .await?;
        removed.extend(
            local
                .into_iter()
                .map(|(href, _)| href)
                .filter(|href| !remote_set.contains(href.as_str())),
        );
        token = advertised_token;
    }

    let local = db
        .read(move |conn| repo::carddav::object_etags(conn, book_id))
        .await?;
    let etags = local.into_iter().collect::<HashMap<_, _>>();
    remote.retain(|(href, etag)| {
        etags
            .get(href)
            .is_none_or(|stored| etag.is_none() || stored != etag)
    });
    let mut changed = !removed.is_empty();
    let mut batches = remote
        .chunks(MULTIGET_BATCH)
        .map(|batch| batch.to_vec())
        .collect::<VecDeque<_>>();
    while let Some(batch) = batches.pop_front() {
        let hrefs = batch
            .iter()
            .map(|(href, _)| href.clone())
            .collect::<Vec<_>>();
        let response = transport
            .request(
                "REPORT",
                book_url,
                Some("0"),
                &[],
                Some("application/xml; charset=utf-8"),
                Some(xml::multiget(&hrefs)),
            )
            .await;
        let oversized = matches!(&response, Err(CoreError::ResponseTooLarge { .. }))
            || matches!(&response, Ok(response) if response.status == 413);
        if oversized {
            if batch.len() > 1 {
                let mid = batch.len() / 2;
                batches.push_front(batch[mid..].to_vec());
                batches.push_front(batch[..mid].to_vec());
            } else {
                let (href, etag) = batch.into_iter().next().unwrap();
                db.write(move |conn| {
                    repo::carddav::record_resource_error(
                        conn,
                        book_id,
                        &href,
                        etag.as_deref(),
                        "Contact response exceeded the download limit",
                    )
                })
                .await?;
                changed = true;
            }
            continue;
        }
        let response = response?;
        if response.status != 207 {
            return Err(super::err(format!(
                "addressbook-multiget: HTTP {}",
                response.status
            )));
        }
        let items = xml::parse(&response.body)?
            .items
            .into_iter()
            .map(|item| Ok((href_key(book_url, &item.href)?, item)))
            .collect::<Result<Vec<_>>>()?;
        let expected = hrefs.iter().cloned().collect::<HashSet<_>>();
        let mut seen = HashSet::new();
        for (href, item) in &items {
            if !expected.contains(href) {
                continue;
            }
            seen.insert(href.clone());
            if item.status == 404 {
                removed.push(href.clone());
            }
        }
        if !seen.is_empty() {
            changed = true;
        }
        db.write(move |conn| {
            let tx = conn.transaction()?;
            for missing in expected.difference(&seen) {
                repo::carddav::record_resource_error(
                    &tx,
                    book_id,
                    missing,
                    None,
                    "Server omitted this contact from its response",
                )?;
            }
            for (href, item) in items
                .into_iter()
                .filter(|(href, item)| expected.contains(href) && item.status != 404)
            {
                let error = if !(200..300).contains(&item.status) {
                    Some(format!("Contact download failed: HTTP {}", item.status))
                } else if item.address_data.is_none() {
                    Some("Server returned no vCard for this contact".into())
                } else if item
                    .etag
                    .as_deref()
                    .is_none_or(|etag| etag.trim().is_empty())
                {
                    Some("Server returned no ETag for this contact".into())
                } else {
                    None
                };
                if let Some(error) = error {
                    repo::carddav::record_resource_error(
                        &tx,
                        book_id,
                        &href,
                        item.etag.as_deref(),
                        &error,
                    )?;
                    continue;
                }
                let Some(card) = item.address_data else {
                    continue;
                };
                match Card::parse(card) {
                    Ok(card) => repo::carddav::apply_remote(
                        &tx,
                        book_id,
                        &href,
                        item.etag.as_deref(),
                        &card,
                    )?,
                    Err(error) => {
                        tracing::warn!(href = %item.href, %error, "unsupported CardDAV resource");
                        repo::carddav::record_resource_error(
                            &tx,
                            book_id,
                            &href,
                            item.etag.as_deref(),
                            &error.to_string(),
                        )?;
                    }
                }
            }
            tx.commit()?;
            Ok(())
        })
        .await?;
    }
    if !removed.is_empty() {
        db.write(move |conn| {
            let tx = conn.transaction()?;
            for href in removed {
                repo::carddav::remove_remote(&tx, book_id, &href)?;
            }
            tx.commit()?;
            Ok(())
        })
        .await?;
    }
    db.write(move |conn| {
        repo::carddav::set_sync_state(conn, book_id, ctag.as_deref(), token.as_deref(), now_ms())
    })
    .await?;
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::carddav::http::{DavResponse, MockTransport};
    fn rename_first_contact(conn: &rusqlite::Connection, name: &str) -> Result<()> {
        let mut record = repo::contacts::list_records(conn, "", 10)?.remove(0);
        record.name = name.into();
        repo::contact_entries::save(conn, &record)?;
        Ok(())
    }

    fn dav(body: &str) -> DavResponse {
        DavResponse {
            status: 207,
            body: body.into(),
            etag: None,
            location: None,
        }
    }

    #[tokio::test]
    async fn full_pull_imports_a_vcard() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("mail.db")).unwrap();
        db.write(|conn| {
            crate::db::testutil::seed_account(conn);
            repo::carddav::upsert_config(
                conn,
                &repo::carddav::CardDavConfig {
                    account_id: 1,
                    kind: crate::models::CardDavKind::Generic,
                    base_url: "https://dav.test/".into(),
                    username: "me".into(),
                    principal_url: None,
                    home_set_url: "https://dav.test/books/me/".into(),
                    enabled: true,
                    last_error: None,
                },
            )?;
            repo::carddav::upsert_addressbook(
                conn,
                1,
                "https://dav.test/books/me/default/",
                Some("Contacts"),
                false,
            )?;
            Ok(())
        })
        .await
        .unwrap();
        let state = r#"<d:multistatus xmlns:d="DAV:"><d:response><d:href>/books/me/default/</d:href><d:propstat><d:prop/><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#;
        let query = r#"<d:multistatus xmlns:d="DAV:"><d:response><d:href>/books/me/default/ada.vcf</d:href><d:propstat><d:prop><d:getetag>"1"</d:getetag></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#;
        let card = r#"<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:carddav"><d:response><d:href>/books/me/default/ada.vcf</d:href><d:propstat><d:prop><d:getetag>"1"</d:getetag><c:address-data>BEGIN:VCARD
VERSION:4.0
UID:ada
FN:Ada Lovelace
EMAIL:ada@example.test
END:VCARD</c:address-data></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#;
        let transport = MockTransport::new(vec![dav(state), dav(query), dav(card)]);
        assert!(
            sync_account(&db, &EventBus::new(), &transport, 1)
                .await
                .unwrap()
        );
        let contacts = db
            .read(|conn| repo::contacts::list_records(conn, "", 10))
            .await
            .unwrap();
        assert_eq!(contacts[0].name, "Ada Lovelace");
        assert_eq!(contacts[0].account_ids, vec![1]);
        let seen = transport.seen.lock().unwrap();
        assert_eq!(seen[1].5.as_deref(), Some("1"));
        assert_eq!(seen[2].5.as_deref(), Some("0"));
    }

    #[tokio::test]
    async fn initial_sync_uses_an_empty_sync_token_when_supported() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("mail.db")).unwrap();
        db.write(|conn| {
            crate::db::testutil::seed_account(conn);
            repo::carddav::upsert_config(
                conn,
                &repo::carddav::CardDavConfig {
                    account_id: 1,
                    kind: crate::models::CardDavKind::Generic,
                    base_url: "https://dav.test/".into(),
                    username: "me".into(),
                    principal_url: None,
                    home_set_url: "https://dav.test/books/me/".into(),
                    enabled: true,
                    last_error: None,
                },
            )?;
            repo::carddav::upsert_addressbook(
                conn,
                1,
                "https://dav.test/books/me/default/",
                Some("Contacts"),
                false,
            )?;
            Ok(())
        })
        .await
        .unwrap();
        let state = r#"<d:multistatus xmlns:d="DAV:"><d:response><d:href>/books/me/default/</d:href><d:propstat><d:prop><d:sync-token>urn:sync:before</d:sync-token></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#;
        let changes = r#"<d:multistatus xmlns:d="DAV:"><d:response><d:href>/books/me/default/ada.vcf</d:href><d:propstat><d:prop><d:getetag>&quot;1&quot;</d:getetag></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response><d:sync-token>urn:sync:after</d:sync-token></d:multistatus>"#;
        let card = r#"<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:carddav"><d:response><d:href>/books/me/default/ada.vcf</d:href><d:propstat><d:prop><d:getetag>&quot;1&quot;</d:getetag><c:address-data>BEGIN:VCARD
VERSION:3.0
UID:ada
N:;;;;
FN:Ada
EMAIL:ada@example.test
END:VCARD</c:address-data></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#;
        let transport = MockTransport::new(vec![dav(state), dav(changes), dav(card)]);
        sync_account(&db, &EventBus::new(), &transport, 1)
            .await
            .unwrap();
        {
            let seen = transport.seen.lock().unwrap();
            assert_eq!(seen[1].5.as_deref(), Some("0"));
            assert!(
                seen[1]
                    .4
                    .as_deref()
                    .unwrap()
                    .contains("<d:sync-token></d:sync-token>")
            );
            assert_eq!(seen[2].5.as_deref(), Some("0"));
        }
        let (_, token) = db
            .read(|conn| repo::carddav::sync_state(conn, 1))
            .await
            .unwrap();
        assert_eq!(token.as_deref(), Some("urn:sync:after"));
    }

    #[tokio::test]
    async fn new_contact_uses_if_none_match_vcard3_and_uuid_uid() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("mail.db")).unwrap();
        db.write(|conn| {
            crate::db::testutil::seed_account(conn);
            repo::carddav::upsert_config(
                conn,
                &repo::carddav::CardDavConfig {
                    account_id: 1,
                    kind: crate::models::CardDavKind::Generic,
                    base_url: "https://dav.test/".into(),
                    username: "me".into(),
                    principal_url: None,
                    home_set_url: "https://dav.test/books/me/".into(),
                    enabled: true,
                    last_error: None,
                },
            )?;
            repo::carddav::upsert_addressbook(
                conn,
                1,
                "https://dav.test/books/me/default/",
                Some("Contacts"),
                false,
            )?;
            let record = vcard::parse(
                "BEGIN:VCARD\nVERSION:3.0\nUID:temporary\nN:;;;;\nFN:Ada\nEMAIL:ada@example.test\nEND:VCARD",
            )?;
            let id = repo::contact_entries::save(conn, &record)?;
            repo::carddav::attach_new_contact(conn, 1, id)?;
            Ok(())
        })
        .await
        .unwrap();
        let transport = MockTransport::new(vec![DavResponse {
            status: 201,
            etag: Some("\"created\"".into()),
            body: String::new(),
            location: None,
        }]);
        push_dirty(&db, &transport, 1).await.unwrap();
        let seen = transport.seen.lock().unwrap();
        assert_eq!(seen[0].2, vec![("If-None-Match".into(), "*".into())]);
        let body = seen[0].4.as_deref().unwrap();
        assert!(body.contains("VERSION:3.0"));
        assert!(body.contains("UID:urn:uuid:"));
        assert!(body.contains("N:;Ada;;;"));
    }

    #[tokio::test]
    async fn local_edit_uses_vcard_content_type_and_if_match() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("mail.db")).unwrap();
        db.write(|conn| {
            crate::db::testutil::seed_account(conn);
            repo::carddav::upsert_config(
                conn,
                &repo::carddav::CardDavConfig {
                    account_id: 1,
                    kind: crate::models::CardDavKind::Generic,
                    base_url: "https://dav.test/".into(),
                    username: "me".into(),
                    principal_url: None,
                    home_set_url: "https://dav.test/books/me/".into(),
                    enabled: true,
                    last_error: None,
                },
            )?;
            let book = repo::carddav::upsert_addressbook(
                conn,
                1,
                "https://dav.test/books/me/default/",
                Some("Contacts"),
                false,
            )?;
            let card =
                "BEGIN:VCARD\nVERSION:4.0\nUID:ada\nFN:Ada\nEMAIL:ada@example.test\nEND:VCARD";
            repo::carddav::apply_remote(
                conn,
                book,
                "/books/me/default/ada.vcf",
                Some("\"old\""),
                &Card::parse(card)?,
            )?;
            rename_first_contact(conn, "Ada Lovelace")
        })
        .await
        .unwrap();
        let transport = MockTransport::new(vec![DavResponse {
            status: 204,
            etag: Some("\"new\"".into()),
            body: String::new(),
            location: None,
        }]);
        assert!(push_dirty(&db, &transport, 1).await.unwrap());
        let seen = transport.seen.lock().unwrap();
        assert_eq!(seen[0].0, "PUT");
        assert_eq!(seen[0].2, vec![("If-Match".into(), "\"old\"".into())]);
        assert_eq!(seen[0].3.as_deref(), Some("text/vcard; charset=utf-8"));
        let body = seen[0].4.as_deref().unwrap();
        assert!(body.contains("UID:ada\r\nFN:Ada Lovelace\r\nEMAIL:ada@example.test"));
    }

    #[tokio::test]
    async fn etag_conflict_retains_local_edits_for_resolution() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("mail.db")).unwrap();
        db.write(|conn| {
            crate::db::testutil::seed_account(conn);
            repo::carddav::upsert_config(
                conn,
                &repo::carddav::CardDavConfig {
                    account_id: 1,
                    kind: crate::models::CardDavKind::Generic,
                    base_url: "https://dav.test/".into(),
                    username: "me".into(),
                    principal_url: None,
                    home_set_url: "https://dav.test/books/me/".into(),
                    enabled: true,
                    last_error: None,
                },
            )?;
            let book = repo::carddav::upsert_addressbook(
                conn,
                1,
                "https://dav.test/books/me/default/",
                Some("Contacts"),
                false,
            )?;
            let card = "BEGIN:VCARD\nVERSION:3.0\nUID:server-id\nN:;;;;\nFN:Before\nEMAIL:ada@example.test\nEND:VCARD";
            repo::carddav::apply_remote(conn, book, "/books/me/default/ada.vcf", Some("\"old\""), &Card::parse(card)?)?;
            rename_first_contact(conn, "Local edit")
        })
        .await
        .unwrap();
        let transport = MockTransport::new(vec![
            DavResponse {
                status: 412,
                ..Default::default()
            },
            DavResponse {
                status: 200,
                etag: Some("\"server\"".into()),
                body: "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:server-id\r\nN:;;;;\r\nFN:Server version\r\nEMAIL:ada@example.test\r\nEND:VCARD\r\n".into(),
                location: None,
            },
        ]);
        assert!(push_dirty(&db, &transport, 1).await.unwrap());
        let contacts = db
            .read(|conn| repo::contacts::list_records(conn, "", 10))
            .await
            .unwrap();
        assert_eq!(contacts[0].name, "Local edit");
        assert_eq!(
            contacts[0].details.sync,
            crate::models::ContactSync::Conflict
        );
        let seen = transport.seen.lock().unwrap();
        assert_eq!(seen[0].0, "PUT");
        assert_eq!(seen[1].0, "GET");
    }

    #[tokio::test]
    async fn failed_query_item_never_deletes_the_local_contact() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("mail.db")).unwrap();
        db.write(|conn| {
            crate::db::testutil::seed_account(conn);
            repo::carddav::upsert_config(
                conn,
                &repo::carddav::CardDavConfig {
                    account_id: 1,
                    kind: crate::models::CardDavKind::Generic,
                    base_url: "https://dav.test/".into(),
                    username: "me".into(),
                    principal_url: None,
                    home_set_url: "https://dav.test/books/me/".into(),
                    enabled: true,
                    last_error: None,
                },
            )?;
            let book = repo::carddav::upsert_addressbook(
                conn,
                1,
                "https://dav.test/books/me/default/",
                Some("Contacts"),
                false,
            )?;
            let card = "BEGIN:VCARD\nVERSION:3.0\nUID:ada\nN:;;;;\nFN:Ada\nEMAIL:ada@example.test\nEND:VCARD";
            repo::carddav::apply_remote(conn, book, "/books/me/default/ada.vcf", Some("\"old\""), &Card::parse(card)?)
        })
        .await
        .unwrap();
        let state = r#"<d:multistatus xmlns:d="DAV:"><d:response><d:href>/books/me/default/</d:href><d:propstat><d:prop/><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#;
        let failed = r#"<d:multistatus xmlns:d="DAV:"><d:response><d:href>/books/me/default/ada.vcf</d:href><d:status>HTTP/1.1 500 Server Error</d:status></d:response></d:multistatus>"#;
        let transport = MockTransport::new(vec![dav(state), dav(failed)]);
        assert!(
            sync_account(&db, &EventBus::new(), &transport, 1)
                .await
                .is_err()
        );
        let contacts = db
            .read(|conn| repo::contacts::list_records(conn, "", 10))
            .await
            .unwrap();
        assert_eq!(contacts.len(), 1);
        assert_eq!(contacts[0].email, "ada@example.test");
    }
    fn multistatus(items: &[(&str, Option<&str>)]) -> String {
        let mut xml = String::from(
            "<d:multistatus xmlns:d=\"DAV:\" xmlns:c=\"urn:ietf:params:xml:ns:carddav\">",
        );
        for (href, card) in items {
            xml.push_str(&format!(
                "<d:response><d:href>{href}</d:href><d:propstat><d:prop><d:getetag>v1</d:getetag>"
            ));
            if let Some(card) = card {
                xml.push_str(&format!(
                    "<c:address-data><![CDATA[{card}]]></c:address-data>"
                ));
            }
            xml.push_str("</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>");
        }
        xml.push_str("</d:multistatus>");
        xml
    }

    #[tokio::test]
    async fn oversized_batches_split_and_bad_cards_do_not_block_healthy_contacts() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("mail.db")).unwrap();
        let book = db
            .write(|conn| {
                crate::db::testutil::seed_account(conn);
                repo::carddav::upsert_addressbook(
                    conn,
                    1,
                    "https://dav.test/book/",
                    Some("Contacts"),
                    false,
                )
            })
            .await
            .unwrap();
        let good =
            "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:good\r\nFN:Phone only\r\nTEL:123\r\nEND:VCARD\r\n";
        let too_big = format!(
            "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:large\r\nFN:Large\r\nNOTE:{}\r\nEND:VCARD\r\n",
            "x".repeat(vcard::MAX_VCARD_BYTES)
        );
        let transport = MockTransport::new(vec![
            dav(&multistatus(&[("/book/", None)])),
            dav(&multistatus(&[
                ("/book/good.vcf", None),
                ("/book/large.vcf", None),
            ])),
            DavResponse {
                status: 413,
                ..Default::default()
            },
            dav(&multistatus(&[("/book/good.vcf", Some(good))])),
            dav(&multistatus(&[("/book/large.vcf", Some(&too_big))])),
        ]);
        assert!(
            sync_addressbook(&db, &transport, book, "https://dav.test/book/")
                .await
                .unwrap()
        );
        db.read(move |conn| {
            let records = repo::contacts::list_records(conn, "", 20)?;
            assert_eq!(records.len(), 1);
            assert_eq!(records[0].name, "Phone only");
            let error: String = conn.query_row(
                "SELECT last_error FROM carddav_objects WHERE href='/book/large.vcf'",
                [],
                |r| r.get(0),
            )?;
            assert!(error.contains("2 MiB"));
            assert!(repo::carddav::sync_state(conn, book)?.0.is_none());
            Ok(())
        })
        .await
        .unwrap();
        assert_eq!(transport.seen.lock().unwrap().len(), 5);
    }

    #[tokio::test]
    async fn lost_creation_response_reconciles_the_existing_resource() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("mail.db")).unwrap();
        let card = db
            .write(|conn| {
                crate::db::testutil::seed_account(conn);
                repo::carddav::upsert_addressbook(
                    conn,
                    1,
                    "https://dav.test/book/",
                    Some("Contacts"),
                    false,
                )?;
                let record = vcard::parse(
                    "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:new\r\nFN:New person\r\nEND:VCARD\r\n",
                )?;
                let id = repo::contact_entries::save(conn, &record)?;
                repo::carddav::attach_new_contact(conn, 1, id)?;
                Ok(repo::carddav::pending_uploads(conn, 1)?
                    .pop()
                    .unwrap()
                    .document
                    .unwrap())
            })
            .await
            .unwrap();
        let transport = MockTransport::new(vec![
            DavResponse {
                status: 412,
                ..Default::default()
            },
            DavResponse {
                status: 200,
                etag: Some("accepted".into()),
                body: card,
                location: None,
            },
        ]);
        assert!(push_dirty(&db, &transport, 1).await.unwrap());
        db.read(|conn| {
            assert!(repo::carddav::pending_uploads(conn, 1)?.is_empty());
            assert_eq!(repo::contacts::list_records(conn, "", 20)?.len(), 1);
            Ok(())
        })
        .await
        .unwrap();
    }

    const GOOGLE_BOOK: &str =
        "https://www.googleapis.com/carddav/v1/principals/ana@gmail.com/lists/default/";
    const GOOGLE_PATH: &str = "/carddav/v1/principals/ana@gmail.com/lists/default/";

    /// A database whose account syncs its Google Contacts.
    async fn google_db(dir: &tempfile::TempDir) -> Db {
        let db = Db::open(&dir.path().join("mail.db")).unwrap();
        db.write(|conn| {
            crate::db::testutil::seed_account(conn);
            repo::carddav::upsert_config(
                conn,
                &repo::carddav::CardDavConfig {
                    account_id: 1,
                    kind: crate::models::CardDavKind::Google,
                    base_url: GOOGLE_BOOK.into(),
                    username: "ana@gmail.com".into(),
                    principal_url: None,
                    home_set_url: GOOGLE_BOOK.into(),
                    enabled: true,
                    last_error: None,
                },
            )?;
            repo::carddav::upsert_addressbook(conn, 1, GOOGLE_BOOK, Some("Address Book"), false)?;
            Ok(())
        })
        .await
        .unwrap();
        db
    }

    /// A contact made on this device for the Google address book.
    async fn new_google_contact(db: &Db) -> i64 {
        db.write(|conn| {
            let record = vcard::parse(
                "BEGIN:VCARD\nVERSION:3.0\nUID:temporary\nN:;Ada;;;\nFN:Ada\nEMAIL:ada@example.test\nEND:VCARD",
            )?;
            let id = repo::contact_entries::save(conn, &record)?;
            repo::carddav::attach_new_contact(conn, 1, id)?;
            Ok(id)
        })
        .await
        .unwrap()
    }

    fn google_card(name: &str, etag: &str, uid: &str) -> String {
        format!(
            r#"<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:carddav"><d:response><d:href>{GOOGLE_PATH}{uid}</d:href><d:propstat><d:prop><d:getetag>{etag}</d:getetag><c:address-data>BEGIN:VCARD
VERSION:3.0
UID:{uid}
N:;{name};;;
FN:{name}
EMAIL:{uid}@example.test
END:VCARD</c:address-data></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#
        )
    }

    fn google_state(token: &str) -> String {
        format!(
            r#"<d:multistatus xmlns:d="DAV:"><d:response><d:href>{GOOGLE_PATH}</d:href><d:propstat><d:prop><d:sync-token>https://www.googleapis.com/carddav/v1/synctoken/{token}</d:sync-token></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#
        )
    }

    fn requests(transport: &MockTransport) -> Vec<(String, Option<String>)> {
        transport
            .seen
            .lock()
            .unwrap()
            .iter()
            .map(|request| (request.0.clone(), request.5.clone()))
            .collect()
    }

    fn request(method: &str, depth: &str) -> (String, Option<String>) {
        (method.into(), Some(depth.into()))
    }

    #[tokio::test]
    async fn google_lists_by_propfind_then_follows_its_sync_token_with_depth_one() {
        let dir = tempfile::tempdir().unwrap();
        let db = google_db(&dir).await;
        let token = |conn: &mut rusqlite::Connection| repo::carddav::sync_state(conn, 1);

        // Google turns down an empty sync token and addressbook-query.
        let listing = format!(
            r#"<d:multistatus xmlns:d="DAV:"><d:response><d:href>{GOOGLE_PATH}</d:href><d:propstat><d:prop><d:getetag/></d:prop><d:status>HTTP/1.1 404 Not Found</d:status></d:propstat></d:response><d:response><d:href>{GOOGLE_PATH}ada</d:href><d:propstat><d:prop><d:getetag>"1"</d:getetag></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#
        );
        let transport = MockTransport::new(vec![
            dav(&google_state("A")),
            dav(&listing),
            dav(&google_card("Ada", "\"1\"", "ada")),
        ]);
        assert!(
            sync_account(&db, &EventBus::new(), &transport, 1)
                .await
                .unwrap()
        );
        assert_eq!(
            requests(&transport),
            [
                request("PROPFIND", "0"),
                request("PROPFIND", "1"),
                request("REPORT", "0")
            ]
        );
        assert!(
            transport.seen.lock().unwrap()[1]
                .4
                .as_deref()
                .unwrap()
                .contains("<d:getetag/>")
        );
        let (_, stored) = db.read(token).await.unwrap();
        assert!(stored.unwrap().ends_with("synctoken/A"));

        // Later syncs follow the token with Depth 1, which Google needs, and
        // never poll getctag.
        let changes = format!(
            r#"<d:multistatus xmlns:d="DAV:"><d:response><d:href>{GOOGLE_PATH}bob</d:href><d:propstat><d:prop><d:getetag>"2"</d:getetag></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response><d:response><d:href>{GOOGLE_PATH}ada</d:href><d:status>HTTP/1.1 404 Not Found</d:status></d:response><d:sync-token>https://www.googleapis.com/carddav/v1/synctoken/B</d:sync-token></d:multistatus>"#
        );
        let transport = MockTransport::new(vec![
            dav(&changes),
            dav(&google_card("Bob", "\"2\"", "bob")),
        ]);
        assert!(
            sync_account(&db, &EventBus::new(), &transport, 1)
                .await
                .unwrap()
        );
        assert_eq!(
            requests(&transport),
            [request("REPORT", "1"), request("REPORT", "0")]
        );
        assert!(
            transport.seen.lock().unwrap()[0]
                .4
                .as_deref()
                .unwrap()
                .contains("synctoken/A")
        );
        let names = db
            .read(|conn| repo::contacts::list_records(conn, "", 10))
            .await
            .unwrap()
            .into_iter()
            .map(|record| record.name)
            .collect::<Vec<_>>();
        assert_eq!(names, ["Bob"]);

        // An expired token starts over from a fresh one.
        let expired = DavResponse {
            status: 400,
            body: r#"<?xml version="1.0" encoding="UTF-8"?><d:error xmlns:d="DAV:"><d:valid-sync-token/></d:error>"#.into(),
            ..Default::default()
        };
        let listing = format!(
            r#"<d:multistatus xmlns:d="DAV:"><d:response><d:href>{GOOGLE_PATH}bob</d:href><d:propstat><d:prop><d:getetag>"2"</d:getetag></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#
        );
        let transport = MockTransport::new(vec![expired, dav(&google_state("C")), dav(&listing)]);
        sync_account(&db, &EventBus::new(), &transport, 1)
            .await
            .unwrap();
        assert_eq!(
            requests(&transport),
            [
                request("REPORT", "1"),
                request("PROPFIND", "0"),
                request("PROPFIND", "1")
            ]
        );
        let (_, stored) = db.read(token).await.unwrap();
        assert!(stored.unwrap().ends_with("synctoken/C"));
    }

    #[tokio::test]
    async fn google_takes_a_new_contact_by_post_under_its_own_address_and_uid() {
        let dir = tempfile::tempdir().unwrap();
        let db = google_db(&dir).await;
        let entry = new_google_contact(&db).await;
        let members = format!(
            r#"<d:multistatus xmlns:d="DAV:"><d:response><d:href>{GOOGLE_PATH}</d:href><d:propstat><d:prop><d:add-member><d:href>{GOOGLE_PATH}</d:href></d:add-member></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#
        );
        let created = DavResponse {
            status: 201,
            location: Some(format!("{GOOGLE_PATH}c42")),
            ..Default::default()
        };
        let transport = MockTransport::new(vec![
            dav(&members),
            created,
            dav(&google_card("Ada", "\"9\"", "c42")),
        ]);
        assert!(push_dirty(&db, &transport, 1).await.unwrap());
        {
            let seen = transport.seen.lock().unwrap();
            assert!(seen[0].4.as_deref().unwrap().contains("<d:add-member/>"));
            assert_eq!(
                (seen[1].0.as_str(), seen[1].1.as_str()),
                ("POST", GOOGLE_BOOK)
            );
            assert!(seen[1].2.is_empty());
            assert_eq!(seen[1].3.as_deref(), Some("text/vcard; charset=utf-8"));
            assert!(seen[2].4.as_deref().unwrap().contains("lists/default/c42"));
        }
        let (href, etag, dirty, document) = db
            .read(move |conn| {
                Ok(conn.query_row(
                    "SELECT o.href,o.etag,o.dirty,e.document FROM carddav_objects o
                     JOIN contact_entries e ON e.id=o.entry_id WHERE e.id=?1",
                    [entry],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, Option<String>>(1)?,
                            r.get::<_, bool>(2)?,
                            r.get::<_, String>(3)?,
                        ))
                    },
                )?)
            })
            .await
            .unwrap();
        assert_eq!(href, format!("{GOOGLE_PATH}c42"));
        assert_eq!(etag.as_deref(), Some("\"9\""));
        assert!(!dirty);
        assert!(document.contains("UID:c42"));
    }

    #[tokio::test]
    async fn a_contact_google_stored_without_its_address_is_never_posted_twice() {
        let dir = tempfile::tempdir().unwrap();
        let db = google_db(&dir).await;
        new_google_contact(&db).await;
        let members = format!(
            r#"<d:multistatus xmlns:d="DAV:"><d:response><d:href>{GOOGLE_PATH}</d:href><d:propstat><d:prop><d:add-member><d:href>{GOOGLE_PATH}</d:href></d:add-member></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#
        );
        let transport = MockTransport::new(vec![
            dav(&members),
            DavResponse {
                status: 201,
                ..Default::default()
            },
        ]);
        assert!(push_dirty(&db, &transport, 1).await.unwrap());
        let again = MockTransport::new(Vec::new());
        assert!(!push_dirty(&db, &again, 1).await.unwrap());
        assert!(again.seen.lock().unwrap().is_empty());
        // The next sync lists everything, which brings Google's copy.
        assert!(
            db.read(|conn| repo::carddav::has_failed_resources(conn, 1))
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn google_without_an_add_member_address_takes_a_put() {
        let dir = tempfile::tempdir().unwrap();
        let db = google_db(&dir).await;
        new_google_contact(&db).await;
        let members = format!(
            r#"<d:multistatus xmlns:d="DAV:"><d:response><d:href>{GOOGLE_PATH}</d:href><d:propstat><d:prop><d:add-member/></d:prop><d:status>HTTP/1.1 404 Not Found</d:status></d:propstat></d:response></d:multistatus>"#
        );
        let transport = MockTransport::new(vec![
            dav(&members),
            DavResponse {
                status: 201,
                etag: Some("\"1\"".into()),
                ..Default::default()
            },
        ]);
        push_dirty(&db, &transport, 1).await.unwrap();
        let seen = transport.seen.lock().unwrap();
        assert_eq!(seen[1].0, "PUT");
        assert_eq!(seen[1].2, vec![("If-None-Match".into(), "*".into())]);
    }
}
