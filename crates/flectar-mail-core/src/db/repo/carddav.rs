//! CardDAV connection, address-book, and remote-object persistence.

use crate::error::Result;
use crate::models::{AddressBook, CardDavConnection, ContactRecord};
use crate::search::fold;
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

#[derive(Debug, Clone)]
pub struct CardDavObject {
    pub id: i64,
    pub addressbook_id: i64,
    pub contact_id: Option<i64>,
    pub href: String,
    pub etag: Option<String>,
    pub vcard_raw: String,
    pub remote_exists: bool,
    pub dirty: bool,
    pub deleted: bool,
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

fn object_row(row: &Row<'_>) -> rusqlite::Result<CardDavObject> {
    Ok(CardDavObject {
        id: row.get(0)?,
        addressbook_id: row.get(1)?,
        contact_id: row.get(2)?,
        href: row.get(3)?,
        etag: row.get(4)?,
        vcard_raw: row.get(5)?,
        remote_exists: row.get::<_, i64>(6)? != 0,
        dirty: row.get::<_, i64>(7)? != 0,
        deleted: row.get::<_, i64>(8)? != 0,
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

pub fn disconnect(conn: &Connection, account_id: i64) -> Result<()> {
    // Synced contacts stay useful locally. Removing mappings prevents later
    // edits from being sent to a server the user disconnected.
    conn.execute(
        "UPDATE contacts SET is_managed=1
         WHERE id IN (
             SELECT o.contact_id
             FROM carddav_objects o
             JOIN carddav_addressbooks b ON b.id=o.addressbook_id
             WHERE b.account_id=?1 AND o.contact_id IS NOT NULL AND o.deleted=0
         )",
        [account_id],
    )?;
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
           read_only=excluded.read_only",
        params![account_id, url, display_name, read_only as i64],
    )?;
    conn.query_row(
        "SELECT id FROM carddav_addressbooks WHERE account_id=?1 AND url=?2",
        params![account_id, url],
        |r| r.get(0),
    )
    .map_err(Into::into)
}

/// Remove collections no longer advertised during an explicit reconnect.
/// Their contact rows remain local, matching disconnect behavior.
pub fn retain_addressbooks(
    conn: &Connection,
    account_id: i64,
    urls: &std::collections::HashSet<String>,
) -> Result<()> {
    let mut statement =
        conn.prepare("SELECT id,url FROM carddav_addressbooks WHERE account_id=?1")?;
    let books = statement
        .query_map([account_id], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);
    for (id, url) in books {
        if !urls.contains(&url) {
            conn.execute(
                "UPDATE contacts SET is_managed=1
                 WHERE id IN (
                     SELECT contact_id FROM carddav_objects
                     WHERE addressbook_id=?1 AND contact_id IS NOT NULL AND deleted=0
                 )",
                [id],
            )?;
            conn.execute("DELETE FROM carddav_addressbooks WHERE id=?1", [id])?;
        }
    }
    Ok(())
}

pub fn ensure_default(conn: &Connection, account_id: i64, addressbook_id: i64) -> Result<()> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM carddav_addressbooks WHERE account_id=?1 AND is_default=1",
        [account_id],
        |r| r.get(0),
    )?;
    if count == 0 {
        conn.execute(
            "UPDATE carddav_addressbooks SET is_default=(id=?2) WHERE account_id=?1",
            params![account_id, addressbook_id],
        )?;
    }
    Ok(())
}

pub fn list_addressbooks(conn: &Connection, account_id: Option<i64>) -> Result<Vec<AddressBook>> {
    let sql = if account_id.is_some() {
        "SELECT id,account_id,url,display_name,read_only,enabled,is_default,last_synced_at
         FROM carddav_addressbooks WHERE account_id=?1 ORDER BY is_default DESC,display_name COLLATE NOCASE,id"
    } else {
        "SELECT id,account_id,url,display_name,read_only,enabled,is_default,last_synced_at
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
    let mut stmt = conn
        .prepare("SELECT href,etag FROM carddav_objects WHERE addressbook_id=?1 AND deleted=0")?;
    Ok(stmt
        .query_map([book_id], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn dirty_objects(conn: &Connection, account_id: i64) -> Result<Vec<CardDavObject>> {
    let mut stmt = conn.prepare(
        "SELECT o.id,o.addressbook_id,o.contact_id,o.href,o.etag,o.vcard_raw,o.remote_exists,o.dirty,o.deleted
         FROM carddav_objects o JOIN carddav_addressbooks b ON b.id=o.addressbook_id
         WHERE b.account_id=?1 AND b.enabled=1 AND b.read_only=0 AND o.dirty=1 ORDER BY o.id",
    )?;
    Ok(stmt
        .query_map([account_id], object_row)?
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

/// Merge a server vCard into the unified contact row and bind its DAV object.
pub fn upsert_remote(
    conn: &Connection,
    account_id: i64,
    book_id: i64,
    href: &str,
    etag: Option<&str>,
    vcard: &str,
    record: &ContactRecord,
) -> Result<()> {
    let existing_object: Option<(i64, Option<i64>, bool)> = conn.query_row(
        "SELECT id,contact_id,owns_contact FROM carddav_objects WHERE addressbook_id=?1 AND href=?2",
        params![book_id, href], |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? != 0)))
        .optional()?;
    let existing_contact: Option<i64> = conn
        .query_row(
            "SELECT id FROM contacts WHERE email=?1 COLLATE NOCASE",
            [&record.email],
            |r| r.get(0),
        )
        .optional()?;
    let owns = existing_object.as_ref().is_some_and(|v| v.2) || existing_contact.is_none();
    let old_contact = existing_object.as_ref().and_then(|value| value.1);
    let folded = fold(&format!(
        "{} {} {} {} {} {} {} {}",
        record.name,
        record.email,
        record.company,
        record.phone,
        record.job_title,
        record.website,
        record.tags,
        record.postal_address
    ));
    conn.execute(
        "INSERT INTO contacts(email,name,folded,phone,company,job_title,website,birthday,
          postal_address,notes,tags,is_managed,updated_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,0,?12)
         ON CONFLICT(email) DO UPDATE SET name=excluded.name,folded=excluded.folded,
          phone=excluded.phone,company=excluded.company,job_title=excluded.job_title,
          website=excluded.website,birthday=excluded.birthday,postal_address=excluded.postal_address,
          notes=excluded.notes,tags=excluded.tags,updated_at=excluded.updated_at",
        params![record.email.to_lowercase(),record.name,folded,record.phone,record.company,
                record.job_title,record.website,record.birthday,record.postal_address,record.notes,
                record.tags,crate::models::now_ms()],
    )?;
    let contact_id: i64 = conn.query_row(
        "SELECT id FROM contacts WHERE email=?1 COLLATE NOCASE",
        [&record.email],
        |r| r.get(0),
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO contact_accounts(contact_id,account_id) VALUES (?1,?2)",
        params![contact_id, account_id],
    )?;
    conn.execute(
        "INSERT INTO carddav_objects(addressbook_id,contact_id,href,etag,vcard_raw,remote_exists,owns_contact)
         VALUES (?1,?2,?3,?4,?5,1,?6)
         ON CONFLICT(addressbook_id,href) DO UPDATE SET contact_id=excluded.contact_id,
          etag=excluded.etag,vcard_raw=excluded.vcard_raw,remote_exists=1,dirty=0,deleted=0",
        params![book_id, contact_id, href, etag, vcard, owns as i64],
    )?;
    if owns && let Some(old_contact) = old_contact.filter(|old| *old != contact_id) {
        let references: i64 = conn.query_row(
            "SELECT COUNT(*) FROM carddav_objects WHERE contact_id=?1",
            [old_contact],
            |row| row.get(0),
        )?;
        let interactions: i64 = conn.query_row(
            "SELECT send_count+recv_count FROM contacts WHERE id=?1",
            [old_contact],
            |row| row.get(0),
        )?;
        if references == 0 && interactions == 0 {
            conn.execute("DELETE FROM contacts WHERE id=?1", [old_contact])?;
        }
    }
    Ok(())
}

/// Retain the DAV identity and ETag for a resource the app cannot project
/// into its email-keyed contact model. This prevents endless redownloads and
/// lets a later server change make the resource importable.
pub fn upsert_unmapped_remote(
    conn: &Connection,
    book_id: i64,
    href: &str,
    etag: Option<&str>,
    vcard: &str,
) -> Result<()> {
    remove_remote(conn, book_id, href)?;
    conn.execute(
        "INSERT INTO carddav_objects(addressbook_id,contact_id,href,etag,vcard_raw,remote_exists,owns_contact)
         VALUES (?1,NULL,?2,?3,?4,1,0)",
        params![book_id, href, etag, vcard],
    )?;
    Ok(())
}

pub fn remove_remote(conn: &Connection, book_id: i64, href: &str) -> Result<()> {
    let object: Option<(i64, Option<i64>, bool)> = conn.query_row(
        "SELECT id,contact_id,owns_contact FROM carddav_objects WHERE addressbook_id=?1 AND href=?2",
        params![book_id, href], |r| Ok((r.get(0)?,r.get(1)?,r.get::<_,i64>(2)? != 0))).optional()?;
    let Some((object_id, contact_id, owns)) = object else {
        return Ok(());
    };
    conn.execute("DELETE FROM carddav_objects WHERE id=?1", [object_id])?;
    if owns && let Some(contact_id) = contact_id {
        let other: i64 = conn.query_row(
            "SELECT COUNT(*) FROM carddav_objects WHERE contact_id=?1",
            [contact_id],
            |r| r.get(0),
        )?;
        let interactions: i64 = conn.query_row(
            "SELECT send_count+recv_count FROM contacts WHERE id=?1",
            [contact_id],
            |r| r.get(0),
        )?;
        if other == 0 && interactions == 0 {
            conn.execute("DELETE FROM contacts WHERE id=?1", [contact_id])?;
        } else if other == 0 {
            conn.execute("UPDATE contacts SET is_managed=0 WHERE id=?1", [contact_id])?;
        }
    }
    Ok(())
}

pub fn mark_saved_contact_dirty(conn: &Connection, contact_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE carddav_objects SET dirty=1 WHERE contact_id=?1 AND deleted=0",
        [contact_id],
    )?;
    Ok(())
}

pub fn mark_deleted_contact(conn: &Connection, contact_id: i64) -> Result<bool> {
    let changed = conn.execute(
        "UPDATE carddav_objects SET contact_id=NULL,dirty=1,deleted=1 WHERE contact_id=?1",
        [contact_id],
    )?;
    Ok(changed > 0)
}

pub fn clear_dirty(
    conn: &Connection,
    object_id: i64,
    etag: Option<&str>,
    vcard: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE carddav_objects SET etag=?2,vcard_raw=?3,remote_exists=1,dirty=0 WHERE id=?1",
        params![object_id, etag, vcard],
    )?;
    Ok(())
}

pub fn delete_object(conn: &Connection, object_id: i64) -> Result<()> {
    conn.execute("DELETE FROM carddav_objects WHERE id=?1", [object_id])?;
    Ok(())
}

pub fn contact(conn: &Connection, id: i64) -> Result<Option<ContactRecord>> {
    crate::db::repo::contacts::get_record(conn, id)
}

/// Queue a newly-created contact for the selected account's default writable
/// address book. Existing contacts acquire mappings through server pull.
pub fn attach_new_contact(conn: &Connection, account_id: i64, contact_id: i64) -> Result<bool> {
    // `save_record` merges by email. If a "new" UI entry resolved to an
    // existing remote contact, update that resource instead of creating a
    // duplicate vCard on the same server.
    if conn.execute(
        "UPDATE carddav_objects SET dirty=1
         WHERE contact_id=?1 AND deleted=0 AND addressbook_id IN
           (SELECT id FROM carddav_addressbooks WHERE account_id=?2)",
        params![contact_id, account_id],
    )? > 0
    {
        return Ok(true);
    }
    let book: Option<(i64, String)> = conn
        .query_row(
            "SELECT id,url FROM carddav_addressbooks
         WHERE account_id=?1 AND enabled=1 AND read_only=0
         ORDER BY is_default DESC,id LIMIT 1",
            [account_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((book_id, book_url)) = book else {
        return Ok(false);
    };
    let record = contact(conn, contact_id)?
        .ok_or_else(|| crate::error::CoreError::CardDav("contact no longer exists".into()))?;
    let uid = crate::carddav::vcard::new_uid();
    let vcard = crate::carddav::vcard::serialize(&record, &uid);
    let resource_name = format!("{}.vcf", uid.trim_start_matches("urn:uuid:"));
    let mut url =
        url::Url::parse(&book_url).map_err(|e| crate::error::CoreError::CardDav(e.to_string()))?;
    url.path_segments_mut()
        .map_err(|_| {
            crate::error::CoreError::CardDav("address book URL cannot accept a contact path".into())
        })?
        .pop_if_empty()
        .push(&resource_name);
    let mut href = url.path().to_owned();
    if let Some(query) = url.query() {
        href.push('?');
        href.push_str(query);
    }
    conn.execute(
        "INSERT INTO carddav_objects(addressbook_id,contact_id,href,vcard_raw,remote_exists,dirty,owns_contact)
         VALUES (?1,?2,?3,?4,0,1,1)",
        params![book_id, contact_id, href, vcard],
    )?;
    Ok(true)
}
