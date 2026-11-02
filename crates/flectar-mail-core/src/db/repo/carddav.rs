//! CardDAV connection, address-book, and remote-object persistence.

use super::contact_entries;
use crate::carddav::vcard::{self, Card};
use crate::error::{CoreError, Result};
use crate::models::{AddressBook, CardDavConnection};
use rusqlite::{Connection, OptionalExtension, Row, params};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardDavConfig {
    pub account_id: i64,
    pub base_url: String,
    pub username: String,
    pub principal_url: Option<String>,
    pub home_set_url: String,
    pub enabled: bool,
    pub last_error: Option<String>,
}

/// A local change waiting to upload: a deletion, or the entry's document.
#[derive(Debug, Clone)]
pub struct PendingUpload {
    pub id: i64,
    pub addressbook_id: i64,
    pub href: String,
    pub etag: Option<String>,
    pub remote_exists: bool,
    pub deleted: bool,
    pub revision: i64,
    pub document: Option<String>,
}

fn config_row(row: &Row<'_>) -> rusqlite::Result<CardDavConfig> {
    Ok(CardDavConfig {
        account_id: row.get(0)?,
        base_url: row.get(1)?,
        username: row.get(2)?,
        principal_url: row.get(3)?,
        home_set_url: row.get(4)?,
        enabled: row.get::<_, i64>(5)? != 0,
        last_error: row.get(6)?,
    })
}

fn book_row(row: &Row<'_>) -> rusqlite::Result<AddressBook> {
    Ok(AddressBook {
        id: row.get(0)?,
        account_id: row.get(1)?,
        url: row.get(2)?,
        display_name: row.get(3)?,
        read_only: row.get::<_, i64>(4)? != 0,
        enabled: row.get::<_, i64>(5)? != 0,
        is_default: row.get::<_, i64>(6)? != 0,
        last_synced_at: row.get(7)?,
    })
}

fn pending_upload_row(row: &Row<'_>) -> rusqlite::Result<PendingUpload> {
    Ok(PendingUpload {
        id: row.get(0)?,
        addressbook_id: row.get(1)?,
        href: row.get(2)?,
        etag: row.get(3)?,
        remote_exists: row.get(4)?,
        deleted: row.get(5)?,
        revision: row.get(6)?,
        document: row.get(7)?,
    })
}

pub fn upsert_config(conn: &Connection, cfg: &CardDavConfig) -> Result<()> {
    conn.execute(
        "INSERT INTO carddav_config
           (account_id,base_url,username,principal_url,home_set_url,enabled,last_error)
         VALUES (?1,?2,?3,?4,?5,?6,NULL)
         ON CONFLICT(account_id) DO UPDATE SET base_url=excluded.base_url,
           username=excluded.username, principal_url=excluded.principal_url,
           home_set_url=excluded.home_set_url, enabled=excluded.enabled, last_error=NULL",
        params![
            cfg.account_id,
            cfg.base_url,
            cfg.username,
            cfg.principal_url,
            cfg.home_set_url,
            cfg.enabled as i64
        ],
    )?;
    Ok(())
}

pub fn get_config(conn: &Connection, account_id: i64) -> Result<Option<CardDavConfig>> {
    conn.query_row(
        "SELECT account_id,base_url,username,principal_url,home_set_url,enabled,last_error
         FROM carddav_config WHERE account_id=?1",
        [account_id],
        config_row,
    )
    .optional()
    .map_err(Into::into)
}

pub fn enabled_configs(conn: &Connection) -> Result<Vec<CardDavConfig>> {
    let mut stmt = conn.prepare(
        "SELECT account_id,base_url,username,principal_url,home_set_url,enabled,last_error
         FROM carddav_config WHERE enabled=1 ORDER BY account_id",
    )?;
    Ok(stmt
        .query_map([], config_row)?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn list_connections(conn: &Connection) -> Result<Vec<CardDavConnection>> {
    let mut stmt = conn.prepare(
        "SELECT account_id,base_url,username,enabled,last_error
         FROM carddav_config ORDER BY account_id",
    )?;
    Ok(stmt
        .query_map([], |row| {
            Ok(CardDavConnection {
                account_id: row.get(0)?,
                base_url: row.get(1)?,
                username: row.get(2)?,
                enabled: row.get::<_, i64>(3)? != 0,
                last_error: row.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn set_enabled(conn: &Connection, account_id: i64, enabled: bool) -> Result<bool> {
    Ok(conn.execute(
        "UPDATE carddav_config SET enabled=?2,last_error=NULL WHERE account_id=?1",
        params![account_id, enabled as i64],
    )? > 0)
}

pub fn set_error(conn: &Connection, account_id: i64, error: Option<&str>) -> Result<()> {
    conn.execute(
        "UPDATE carddav_config SET last_error=?2 WHERE account_id=?1",
        params![account_id, error],
    )?;
    Ok(())
}

/// Forget an account's CardDAV connection. Deleting its address books keeps
/// their contacts on this device (see migration 13).
pub fn disconnect(conn: &Connection, account_id: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM carddav_addressbooks WHERE account_id=?1",
        [account_id],
    )?;
    conn.execute(
        "DELETE FROM carddav_config WHERE account_id=?1",
        [account_id],
    )?;
    Ok(())
}

pub fn upsert_addressbook(
    conn: &Connection,
    account_id: i64,
    url: &str,
    display_name: Option<&str>,
    read_only: bool,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO carddav_addressbooks(account_id,url,display_name,read_only)
         VALUES (?1,?2,?3,?4)
         ON CONFLICT(account_id,url) DO UPDATE SET display_name=excluded.display_name,
           read_only=excluded.read_only,available=1",
        params![account_id, url, display_name, read_only as i64],
    )?;
    conn.query_row(
        "SELECT id FROM carddav_addressbooks WHERE account_id=?1 AND url=?2",
        params![account_id, url],
        |r| r.get(0),
    )
    .map_err(Into::into)
}

/// Mark missing collections unavailable while retaining documents and pending work.
pub fn retain_addressbooks(
    conn: &Connection,
    account_id: i64,
    urls: &std::collections::HashSet<String>,
) -> Result<()> {
    let mut stmt = conn.prepare("SELECT id,url FROM carddav_addressbooks WHERE account_id=?1")?;
    let books = stmt
        .query_map([account_id], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, url) in books {
        conn.execute(
            "UPDATE carddav_addressbooks SET available=?2 WHERE id=?1",
            params![id, urls.contains(&url)],
        )?;
    }
    Ok(())
}

pub fn ensure_default(conn: &Connection, account_id: i64, addressbook_id: i64) -> Result<()> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM carddav_addressbooks WHERE account_id=?1 AND is_default=1 AND available=1 AND read_only=0",
        [account_id],
        |r| r.get(0),
    )?;
    if count == 0 {
        conn.execute(
            "UPDATE carddav_addressbooks SET is_default=0 WHERE account_id=?1",
            [account_id],
        )?;
        conn.execute(
            "UPDATE carddav_addressbooks SET is_default=(id=?2) WHERE account_id=?1",
            params![account_id, addressbook_id],
        )?;
    }
    Ok(())
}

pub fn list_addressbooks(conn: &Connection, account_id: Option<i64>) -> Result<Vec<AddressBook>> {
    let sql = if account_id.is_some() {
        "SELECT id,account_id,url,display_name,read_only,(enabled=1 AND available=1),is_default,last_synced_at
         FROM carddav_addressbooks WHERE account_id=?1 ORDER BY is_default DESC,display_name COLLATE NOCASE,id"
    } else {
        "SELECT id,account_id,url,display_name,read_only,(enabled=1 AND available=1),is_default,last_synced_at
         FROM carddav_addressbooks ORDER BY account_id,is_default DESC,display_name COLLATE NOCASE,id"
    };
    let mut stmt = conn.prepare(sql)?;
    let rows = match account_id {
        Some(id) => stmt
            .query_map([id], book_row)?
            .collect::<rusqlite::Result<_>>()?,
        None => stmt
            .query_map([], book_row)?
            .collect::<rusqlite::Result<_>>()?,
    };
    Ok(rows)
}

pub fn sync_state(conn: &Connection, id: i64) -> Result<(Option<String>, Option<String>)> {
    conn.query_row(
        "SELECT ctag,sync_token FROM carddav_addressbooks WHERE id=?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .map_err(Into::into)
}

pub fn set_sync_state(
    conn: &Connection,
    id: i64,
    ctag: Option<&str>,
    token: Option<&str>,
    at: i64,
) -> Result<()> {
    conn.execute(
        "UPDATE carddav_addressbooks SET ctag=?2,sync_token=?3,last_synced_at=?4 WHERE id=?1",
        params![id, ctag, token, at],
    )?;
    Ok(())
}

pub fn object_etags(conn: &Connection, book_id: i64) -> Result<Vec<(String, Option<String>)>> {
    // A resource that failed to import has no usable ETag, so it is
    // downloaded again.
    let mut stmt = conn.prepare(
        "SELECT href,CASE WHEN last_error IS NULL THEN etag END FROM carddav_objects
         WHERE addressbook_id=?1 AND remote_exists=1",
    )?;
    Ok(stmt
        .query_map([book_id], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn has_failed_resources(conn: &Connection, book_id: i64) -> Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM carddav_objects WHERE addressbook_id=?1 AND last_error IS NOT NULL)",
        [book_id],
        |r| r.get(0),
    )
    .map_err(Into::into)
}

/// Local changes ready to upload to an account's writable, reachable address
/// books. Conflicts and failed resources wait for the user or the server.
pub fn pending_uploads(conn: &Connection, account_id: i64) -> Result<Vec<PendingUpload>> {
    let mut stmt = conn.prepare(
        "SELECT o.id,o.addressbook_id,o.href,o.etag,o.remote_exists,o.deleted,o.revision,e.document
         FROM carddav_objects o
         JOIN carddav_addressbooks b ON b.id=o.addressbook_id
         LEFT JOIN contact_entries e ON e.id=o.entry_id
         WHERE b.account_id=?1 AND b.enabled=1 AND b.available=1 AND b.read_only=0
           AND o.dirty=1 AND o.last_error IS NULL AND o.conflict_vcard IS NULL
           AND o.conflict_deleted=0
         ORDER BY o.id",
    )?;
    Ok(stmt
        .query_map([account_id], pending_upload_row)?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn addressbook_url(conn: &Connection, id: i64) -> Result<Option<String>> {
    conn.query_row(
        "SELECT url FROM carddav_addressbooks WHERE id=?1",
        [id],
        |r| r.get(0),
    )
    .optional()
    .map_err(Into::into)
}

pub fn mark_addressbook_read_only(conn: &Connection, id: i64) -> Result<()> {
    conn.execute(
        "UPDATE carddav_addressbooks SET read_only=1 WHERE id=?1",
        [id],
    )?;
    Ok(())
}

fn bump_entry_revision(conn: &Connection, entry: Option<i64>) -> Result<()> {
    if let Some(entry) = entry {
        conn.execute(
            "UPDATE contact_entries SET revision=revision+1 WHERE id=?1",
            [entry],
        )?;
    }
    Ok(())
}

/// Apply the server's version of a resource. A pending local change is never
/// overwritten: an ETag already seen keeps it, the server returning the
/// pending document acknowledges it, and anything else becomes a conflict.
/// A new resource whose UID matches a contact kept on this device after its
/// address book was removed is linked to that contact again.
pub fn apply_remote(
    conn: &Connection,
    book_id: i64,
    href: &str,
    etag: Option<&str>,
    card: &Card,
) -> Result<()> {
    struct Existing {
        id: i64,
        entry: Option<i64>,
        dirty: bool,
        deleted: bool,
        etag: Option<String>,
        conflict_etag: Option<String>,
        document: Option<String>,
    }
    let existing = conn
        .query_row(
            "SELECT o.id,o.entry_id,o.dirty,o.deleted,o.etag,o.conflict_etag,e.document
             FROM carddav_objects o LEFT JOIN contact_entries e ON e.id=o.entry_id
             WHERE o.addressbook_id=?1 AND o.href=?2",
            params![book_id, href],
            |r| {
                Ok(Existing {
                    id: r.get(0)?,
                    entry: r.get(1)?,
                    dirty: r.get(2)?,
                    deleted: r.get(3)?,
                    etag: r.get(4)?,
                    conflict_etag: r.get(5)?,
                    document: r.get(6)?,
                })
            },
        )
        .optional()?;
    // A pending deletion of a resource that never had an entry has nothing
    // to show in a conflict, so the server's version is imported instead.
    if let Some(object) = existing
        .as_ref()
        .filter(|object| object.dirty && object.entry.is_some())
    {
        // The server answered for this resource, so an earlier download
        // error no longer applies.
        if etag.is_some()
            && (object.etag.as_deref() == etag || object.conflict_etag.as_deref() == etag)
        {
            conn.execute(
                "UPDATE carddav_objects SET last_error=NULL WHERE id=?1",
                [object.id],
            )?;
            return Ok(());
        }
        if !object.deleted
            && object
                .document
                .as_deref()
                .is_some_and(|document| vcard::equivalent(document, card.document()))
        {
            // The upload succeeded but its response was lost.
            conn.execute(
                "UPDATE carddav_objects SET etag=?2,dirty=0,remote_exists=1,last_error=NULL,
                   conflict_vcard=NULL,conflict_etag=NULL,conflict_deleted=0
                 WHERE id=?1",
                params![object.id, etag],
            )?;
            return Ok(());
        }
        conn.execute(
            "UPDATE carddav_objects SET conflict_vcard=?2,conflict_etag=?3,conflict_deleted=0,
               last_error=NULL
             WHERE id=?1",
            params![object.id, card.document(), etag],
        )?;
        return bump_entry_revision(conn, object.entry);
    }
    let kept = match (&existing, vcard::uid(card.document())) {
        (None, Some(uid)) => conn
            .query_row(
                "SELECT id,document FROM contact_entries e WHERE uid=?1
                   AND NOT EXISTS(SELECT 1 FROM carddav_objects o WHERE o.entry_id=e.id)
                 ORDER BY id LIMIT 1",
                [uid],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?,
        _ => None,
    };
    if let Some((entry, document)) = &kept
        && !vcard::equivalent(document, card.document())
    {
        // Changed on this device while disconnected: keep both versions.
        conn.execute(
            "INSERT INTO carddav_objects(addressbook_id,entry_id,href,etag,dirty,revision,
               conflict_vcard,conflict_etag)
             VALUES (?1,?2,?3,?4,1,1,?5,?4)",
            params![book_id, entry, href, etag, card.document()],
        )?;
        conn.execute(
            "UPDATE contact_entries SET detached_unsynced=0,revision=revision+1 WHERE id=?1",
            [*entry],
        )?;
        return Ok(());
    }
    let entry = contact_entries::store(
        conn,
        existing
            .and_then(|object| object.entry)
            .or(kept.map(|(entry, _)| entry)),
        card,
    )?;
    conn.execute(
        "UPDATE contact_entries SET detached_unsynced=0 WHERE id=?1",
        [entry],
    )?;
    conn.execute(
        "INSERT INTO carddav_objects(addressbook_id,entry_id,href,etag) VALUES (?1,?2,?3,?4)
         ON CONFLICT(addressbook_id,href) DO UPDATE SET entry_id=excluded.entry_id,
           etag=excluded.etag,remote_exists=1,dirty=0,deleted=0,conflict_vcard=NULL,
           conflict_etag=NULL,conflict_deleted=0,last_error=NULL",
        params![book_id, entry, href, etag],
    )?;
    Ok(())
}

/// Record a resource that could not be imported; it is downloaded again on
/// the next sync. An entry it already had keeps its last good version.
pub fn record_resource_error(
    conn: &Connection,
    book_id: i64,
    href: &str,
    etag: Option<&str>,
    error: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO carddav_objects(addressbook_id,href,etag,last_error) VALUES (?1,?2,?3,?4)
         ON CONFLICT(addressbook_id,href) DO UPDATE SET last_error=excluded.last_error",
        params![book_id, href, etag, error],
    )?;
    Ok(())
}

/// The server no longer has a resource. A pending local edit turns this into
/// a conflict; otherwise the contact is removed.
pub fn remove_remote(conn: &Connection, book_id: i64, href: &str) -> Result<()> {
    let object = conn
        .query_row(
            "SELECT id,entry_id,dirty,deleted FROM carddav_objects WHERE addressbook_id=?1 AND href=?2",
            params![book_id, href],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, Option<i64>>(1)?,
                    r.get::<_, bool>(2)?,
                    r.get::<_, bool>(3)?,
                ))
            },
        )
        .optional()?;
    match object {
        Some((id, entry, true, false)) => {
            conn.execute(
                "UPDATE carddav_objects SET conflict_deleted=1,conflict_vcard=NULL,conflict_etag=NULL,
                   last_error=NULL
                 WHERE id=?1",
                [id],
            )?;
            bump_entry_revision(conn, entry)
        }
        Some((id, ..)) => forget_object(conn, id),
        None => Ok(()),
    }
}

/// Record a successful upload. `document` is what the server stored, which
/// may be a normalization of the upload. Pending work is cleared only when no
/// local change followed the uploaded revision.
pub fn acknowledge_upload(
    conn: &Connection,
    object_id: i64,
    revision: i64,
    etag: Option<&str>,
    document: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE carddav_objects SET etag=?3,remote_exists=1,
           dirty=CASE WHEN revision=?2 THEN 0 ELSE 1 END,
           last_error=CASE WHEN ?3 IS NULL THEN 'Server returned no ETag after saving; waiting to reconcile' END
         WHERE id=?1",
        params![object_id, revision, etag],
    )?;
    let current = conn
        .query_row(
            "SELECT e.id,e.document FROM carddav_objects o JOIN contact_entries e ON e.id=o.entry_id
             WHERE o.id=?1 AND o.revision=?2",
            params![object_id, revision],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()?;
    if let Some((entry, stored)) = current
        && !vcard::equivalent(&stored, document)
        && let Ok(card) = Card::parse(document)
    {
        contact_entries::store(conn, Some(entry), &card)?;
    }
    Ok(())
}

/// Remove a resource and its contact after the server confirmed they are gone.
pub fn forget_object(conn: &Connection, object_id: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM contact_entries WHERE id=(SELECT entry_id FROM carddav_objects WHERE id=?1)",
        [object_id],
    )?;
    conn.execute("DELETE FROM carddav_objects WHERE id=?1", [object_id])?;
    Ok(())
}

/// Queue a local contact for upload to the account's default writable address
/// book. Returns false when the account has none.
pub fn attach_new_contact(conn: &Connection, account_id: i64, entry_id: i64) -> Result<bool> {
    if conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM carddav_objects WHERE entry_id=?1)",
        [entry_id],
        |r| r.get::<_, bool>(0),
    )? {
        return Ok(true);
    }
    let book = conn
        .query_row(
            "SELECT id,url FROM carddav_addressbooks
             WHERE account_id=?1 AND enabled=1 AND available=1 AND read_only=0
             ORDER BY is_default DESC,id LIMIT 1",
            [account_id],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()?;
    let Some((book, url)) = book else {
        return Ok(false);
    };
    let document: String = conn.query_row(
        "SELECT document FROM contact_entries WHERE id=?1",
        [entry_id],
        |r| r.get(0),
    )?;
    let uid =
        vcard::uid(&document).ok_or_else(|| CoreError::CardDav("contact has no UID".into()))?;
    let mut url = url::Url::parse(&url).map_err(|e| CoreError::CardDav(e.to_string()))?;
    url.path_segments_mut()
        .map_err(|_| CoreError::CardDav("Invalid address book URL".into()))?
        .pop_if_empty()
        .push(&format!("{}.vcf", uid.trim_start_matches("urn:uuid:")));
    let href = match url.query() {
        Some(q) => format!("{}?{q}", url.path()),
        None => url.path().to_owned(),
    };
    conn.execute(
        "INSERT INTO carddav_objects(addressbook_id,entry_id,href,remote_exists,dirty,revision)
         VALUES (?1,?2,?3,0,1,1)",
        params![book, entry_id, href],
    )?;
    bump_entry_revision(conn, Some(entry_id))?;
    Ok(true)
}

/// Keep one complete version of a conflicting contact. Keeping the local
/// version uploads it over the server's current version on the next sync.
pub fn resolve_conflict(
    conn: &Connection,
    entry: i64,
    revision: i64,
    keep_local: bool,
) -> Result<()> {
    let actual: i64 = conn.query_row(
        "SELECT revision FROM contact_entries WHERE id=?1",
        [entry],
        |r| r.get(0),
    )?;
    if actual != revision {
        return Err(CoreError::Other(
            "Contact changed; reopen the conflict".into(),
        ));
    }
    let (id, book, href, server, etag, server_deleted, writable): (
        i64,
        i64,
        String,
        Option<String>,
        Option<String>,
        bool,
        bool,
    ) = conn.query_row(
        "SELECT o.id,o.addressbook_id,o.href,o.conflict_vcard,o.conflict_etag,o.conflict_deleted,
                b.read_only=0 AND b.available=1
         FROM carddav_objects o JOIN carddav_addressbooks b ON b.id=o.addressbook_id
         WHERE o.entry_id=?1",
        [entry],
        |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
            ))
        },
    )?;
    if server.is_none() && !server_deleted {
        return Err(CoreError::Other(
            "Conflict has already been resolved".into(),
        ));
    }
    if keep_local {
        if !writable {
            return Err(CoreError::Other(
                "This address book is read-only or unavailable".into(),
            ));
        }
        if etag.is_none() && !server_deleted {
            return Err(CoreError::Other(
                "Sync again before keeping your version".into(),
            ));
        }
        conn.execute(
            "UPDATE carddav_objects SET etag=conflict_etag,remote_exists=1-conflict_deleted,
               conflict_vcard=NULL,conflict_etag=NULL,conflict_deleted=0,dirty=1,
               revision=revision+1,last_error=NULL
             WHERE id=?1",
            [id],
        )?;
        return bump_entry_revision(conn, Some(entry));
    }
    let Some(card) = server else {
        return forget_object(conn, id);
    };
    conn.execute(
        "UPDATE carddav_objects SET dirty=0,deleted=0,conflict_vcard=NULL,conflict_etag=NULL,
           conflict_deleted=0,last_error=NULL
         WHERE id=?1",
        [id],
    )?;
    apply_remote(conn, book, &href, etag.as_deref(), &Card::parse(card)?)
}
