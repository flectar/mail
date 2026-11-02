//! Saved contacts. Each entry's vCard document is authoritative; its columns
//! and contact_values are a projection rewritten whenever the document is.
//! A synced entry is one that a CardDAV object refers to (see repo::carddav).

use crate::{
    carddav::vcard::{self, Card},
    error::{CoreError, Result},
    models::{ContactRecord, ContactSync, ContactValue},
    search::fold,
};
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};
use std::collections::HashMap;

/// Insert or rewrite an entry from a card and return its id. Every write
/// advances the entry's revision.
pub fn store(conn: &Connection, id: Option<i64>, card: &Card) -> Result<i64> {
    let (document, record) = (card.document(), card.record());
    let photo = vcard::photo(document);
    let photo_hash = photo.as_deref().map(vcard::photo_hash);
    let values = record
        .details
        .emails
        .iter()
        .chain(&record.details.phones)
        .map(|value| value.value.as_str());
    let searchable = [
        record.name.as_str(),
        &record.company,
        &record.job_title,
        &record.website,
        &record.postal_address,
        &record.tags,
    ]
    .into_iter()
    .chain(values)
    .collect::<Vec<_>>()
    .join(" ");
    let folded = fold(&searchable);
    let columns = params![
        document,
        record.name,
        record.email,
        record.phone,
        record.company,
        record.job_title,
        record.website,
        record.birthday,
        record.postal_address,
        record.notes,
        record.tags,
        folded,
        photo_hash,
        vcard::uid(document),
    ];
    let id = match id {
        Some(id) => {
            conn.execute(
                "UPDATE contact_entries SET document = ?1, name = ?2, email = ?3, phone = ?4,
                   company = ?5, job_title = ?6, website = ?7, birthday = ?8,
                   postal_address = ?9, notes = ?10, tags = ?11, folded = ?12,
                   photo_hash = ?13, uid = ?14, revision = revision + 1
                 WHERE id = ?15",
                params_from_iter(columns.iter().copied().chain([&id as &dyn rusqlite::ToSql])),
            )?;
            conn.execute("DELETE FROM contact_values WHERE contact_id = ?1", [id])?;
            id
        }
        None => {
            conn.execute(
                "INSERT INTO contact_entries (document, name, email, phone, company, job_title,
                   website, birthday, postal_address, notes, tags, folded, photo_hash, uid)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                columns,
            )?;
            conn.last_insert_rowid()
        }
    };
    let mut insert = conn.prepare_cached(
        "INSERT INTO contact_values (contact_id, line, kind, value, label, preference)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    for (kind, values) in [
        ("EMAIL", &record.details.emails),
        ("TEL", &record.details.phones),
    ] {
        for value in values {
            let line = value
                .key
                .parse::<i64>()
                .map_err(|_| CoreError::Other("contact value has no document line".into()))?;
            insert.execute(params![
                id,
                line,
                kind,
                value.value,
                value.label,
                value.preference
            ])?;
        }
    }
    if let (Some(hash), Some(bytes)) = (photo_hash, photo) {
        conn.execute(
            "INSERT OR IGNORE INTO contact_photos (hash, bytes) VALUES (?1, ?2)",
            params![hash, bytes],
        )?;
    }
    Ok(id)
}

/// Fill the stored state, source, and values of saved records in two queries.
/// A suggestion's only value is its address, so saving it adds that address.
pub fn load_details(conn: &Connection, records: &mut [ContactRecord]) -> Result<()> {
    for record in records.iter_mut().filter(|record| record.id < 0) {
        record.details.emails = vec![ContactValue {
            value: record.email.clone(),
            preference: ContactValue::NO_PREFERENCE,
            ..Default::default()
        }];
    }
    let ids = records
        .iter()
        .map(|record| record.id)
        .filter(|id| *id > 0)
        .collect::<Vec<_>>();
    if ids.is_empty() {
        return Ok(());
    }
    let placeholders = vec!["?"; ids.len()].join(",");
    struct State {
        revision: i64,
        photo_hash: Option<String>,
        source: Option<String>,
        read_only: bool,
        sync: ContactSync,
    }
    let mut states = conn
        .prepare(&format!(
            "SELECT e.id, e.revision, e.photo_hash, e.detached_unsynced,
                    o.id IS NOT NULL, COALESCE(b.display_name, b.url), a.email,
                    COALESCE(b.read_only, 0), COALESCE(b.available, 1), COALESCE(o.dirty, 0),
                    o.conflict_vcard IS NOT NULL OR COALESCE(o.conflict_deleted, 0), o.last_error
             FROM contact_entries e
             LEFT JOIN carddav_objects o ON o.entry_id = e.id
             LEFT JOIN carddav_addressbooks b ON b.id = o.addressbook_id
             LEFT JOIN accounts a ON a.id = b.account_id
             WHERE e.id IN ({placeholders})"
        ))?
        .query_map(params_from_iter(&ids), |row| {
            let synced: bool = row.get(4)?;
            let available: bool = row.get(8)?;
            let error: Option<String> = row.get(11)?;
            let sync = if !synced {
                if row.get(3)? {
                    ContactSync::Detached
                } else {
                    ContactSync::Local
                }
            } else if row.get(10)? {
                ContactSync::Conflict
            } else if !available {
                ContactSync::Unavailable
            } else if let Some(error) = error.clone() {
                ContactSync::Failed(error)
            } else if row.get(9)? {
                ContactSync::Pending
            } else {
                ContactSync::Synced
            };
            let source = match (
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
            ) {
                (Some(book), Some(account)) => Some(format!("{book} · {account}")),
                (book, _) => book,
            };
            Ok((
                row.get::<_, i64>(0)?,
                State {
                    revision: row.get(1)?,
                    photo_hash: row.get(2)?,
                    source,
                    read_only: synced && (row.get(7)? || !available || error.is_some()),
                    sync,
                },
            ))
        })?
        .collect::<rusqlite::Result<HashMap<_, _>>>()?;
    let mut values = HashMap::<(i64, String), Vec<ContactValue>>::new();
    let mut statement = conn.prepare(&format!(
        "SELECT contact_id, kind, line, value, label, preference FROM contact_values
         WHERE contact_id IN ({placeholders}) ORDER BY contact_id, line"
    ))?;
    let rows = statement.query_map(params_from_iter(&ids), |row| {
        Ok((
            (row.get::<_, i64>(0)?, row.get::<_, String>(1)?),
            ContactValue {
                key: row.get::<_, i64>(2)?.to_string(),
                value: row.get(3)?,
                label: row.get(4)?,
                preference: row.get(5)?,
            },
        ))
    })?;
    for row in rows {
        let (key, value) = row?;
        values.entry(key).or_default().push(value);
    }
    for record in records.iter_mut().filter(|record| record.id > 0) {
        if let Some(state) = states.remove(&record.id) {
            record.details.revision = state.revision;
            record.details.photo_hash = state.photo_hash;
            record.details.source = state.source;
            record.details.read_only = state.read_only;
            record.details.sync = state.sync;
        }
        for (kind, target) in [
            ("EMAIL", &mut record.details.emails),
            ("TEL", &mut record.details.phones),
        ] {
            *target = values
                .remove(&(record.id, kind.to_owned()))
                .unwrap_or_default();
        }
    }
    Ok(())
}

/// Whether an edit leaves every field stored in the vCard unchanged.
fn same_fields(current: &ContactRecord, edited: &ContactRecord) -> bool {
    let values = |values: &[ContactValue]| {
        values
            .iter()
            .filter(|value| !value.value.trim().is_empty())
            .map(|value| {
                (
                    value.value.trim().to_owned(),
                    value.label.clone(),
                    value.preference,
                )
            })
            .collect::<Vec<_>>()
    };
    let fields = |record: &ContactRecord| {
        [
            record.name.trim().to_owned(),
            record.company.clone(),
            record.job_title.clone(),
            record.website.clone(),
            record.birthday.clone(),
            record.postal_address.clone(),
            record.notes.clone(),
            record.tags.clone(),
        ]
    };
    fields(current) == fields(edited)
        && values(&current.details.emails) == values(&edited.details.emails)
        && values(&current.details.phones) == values(&edited.details.phones)
}

fn validate(record: &ContactRecord) -> Result<()> {
    for value in record.details.emails.iter().chain(&record.details.phones) {
        if value.label.contains(['\r', '\n', ':', ';', '"', '\\'])
            || value.value.contains(['\r', '\n'])
        {
            return Err(CoreError::Other(
                "Contact email, phone, or label contains invalid characters".into(),
            ));
        }
    }
    if record.name.trim().is_empty() && record.email.is_empty() && record.phone.is_empty() {
        return Err(CoreError::Other(
            "Enter a name, email address, or phone number.".into(),
        ));
    }
    Ok(())
}

/// Save an edited or new contact and return its id. Email addresses and phone
/// numbers come from the value lists; the primary fields are derived. An edit
/// must be based on the entry's current revision.
pub fn save(conn: &Connection, record: &ContactRecord) -> Result<i64> {
    let current = if record.id > 0 {
        let current = super::contacts::get_record(conn, record.id)?
            .ok_or_else(|| CoreError::Other("Contact no longer exists".into()))?;
        if current.details.revision != record.details.revision {
            return Err(CoreError::Other(
                "Contact changed; reopen it before saving".into(),
            ));
        }
        let locked = current.details.sync == ContactSync::Conflict || current.details.read_only;
        if locked {
            // Favorites are stored only on this device.
            if same_fields(&current, record) {
                conn.execute(
                    "UPDATE contact_entries SET is_favorite = ?2, revision = revision + 1
                     WHERE id = ?1",
                    params![current.id, record.is_favorite],
                )?;
                return Ok(current.id);
            }
            return Err(CoreError::Other(
                if current.details.sync == ContactSync::Conflict {
                    "Resolve the contact conflict before editing"
                } else {
                    "This address book is read-only or unavailable"
                }
                .into(),
            ));
        }
        Some(current)
    } else {
        None
    };
    let mut record = record.clone();
    // Addresses typed in the editor are normalized; ones already stored keep
    // the form their address book uses.
    let stored = current
        .as_ref()
        .map_or(&[][..], |current| &current.details.emails[..]);
    for value in &mut record.details.emails {
        if !stored
            .iter()
            .any(|stored| stored.key == value.key && stored.value == value.value)
        {
            value.value = value.value.trim().to_lowercase();
        }
    }
    for value in &mut record.details.phones {
        value.value = value.value.trim().to_owned();
    }
    record
        .details
        .emails
        .retain(|value| !value.value.is_empty());
    record
        .details
        .phones
        .retain(|value| !value.value.is_empty());
    record.refresh_primary_values();
    record.name = record.name.trim().to_owned();
    validate(&record)?;
    let existing = match current {
        Some(current) => Some((
            current.id,
            conn.query_row(
                "SELECT document FROM contact_entries WHERE id = ?1",
                [current.id],
                |row| row.get::<_, String>(0),
            )?,
        )),
        None => None,
    };
    if record.name.is_empty() {
        record.name = [&record.email, &record.phone]
            .into_iter()
            .find(|value| !value.is_empty())
            .cloned()
            .unwrap_or_default();
    }
    let document = match &existing {
        Some((_, document)) => vcard::update(document, &record)?,
        None => vcard::serialize(&record, &vcard::new_uid()),
    };
    vcard::ensure_size(&document)?;
    let card = Card::parse(document)?;
    let id = store(conn, existing.as_ref().map(|(id, _)| *id), &card)?;
    conn.execute(
        "UPDATE contact_entries SET is_favorite = ?2 WHERE id = ?1",
        params![id, record.is_favorite],
    )?;
    if existing
        .as_ref()
        .is_some_and(|(_, previous)| !vcard::equivalent(previous, card.document()))
    {
        conn.execute(
            "UPDATE carddav_objects SET dirty = 1, revision = revision + 1
             WHERE entry_id = ?1 AND deleted = 0",
            [id],
        )?;
    }
    Ok(id)
}

/// Delete a saved contact, or forget a mail-derived suggestion. A synced
/// contact stays hidden until the server confirms its deletion. Like an
/// edit, deleting a saved contact must be based on its current revision.
pub fn delete(conn: &Connection, id: i64, revision: i64) -> Result<()> {
    if id < 0 {
        conn.execute("DELETE FROM contacts WHERE id = ?1", [-id])?;
        return Ok(());
    }
    let Some(record) = super::contacts::get_record(conn, id)? else {
        return Ok(());
    };
    if record.details.revision != revision {
        return Err(CoreError::Other(
            "Contact changed; reopen it before deleting".into(),
        ));
    }
    if record.details.sync == ContactSync::Conflict {
        return Err(CoreError::Other(
            "Resolve the contact conflict before deleting".into(),
        ));
    }
    if record.details.read_only {
        return Err(CoreError::Other(
            "This address book is read-only or unavailable".into(),
        ));
    }
    let pending = conn.execute(
        "UPDATE carddav_objects SET dirty = 1, deleted = 1, revision = revision + 1
         WHERE entry_id = ?1",
        [id],
    )?;
    if pending == 0 {
        conn.execute("DELETE FROM contact_entries WHERE id = ?1", [id])?;
    } else {
        conn.execute(
            "UPDATE contact_entries SET revision = revision + 1 WHERE id = ?1",
            [id],
        )?;
    }
    Ok(())
}

pub fn photo_bytes(conn: &Connection, hash: &str) -> Result<Option<Vec<u8>>> {
    conn.query_row(
        "SELECT bytes FROM contact_photos WHERE hash = ?1",
        [hash],
        |row| row.get(0),
    )
    .optional()
    .map_err(Into::into)
}

/// Profile fields of a contact saved by an older version, which kept one
/// email and phone per contact row.
struct LegacyContact {
    name: String,
    email: String,
    phone: String,
    company: String,
    job_title: String,
    website: String,
    birthday: String,
    postal_address: String,
    notes: String,
    tags: String,
    is_favorite: bool,
}

fn legacy_contact(conn: &Connection, id: i64) -> Result<Option<LegacyContact>> {
    conn.query_row(
        "SELECT COALESCE(name, ''), email, phone, company, job_title, website, birthday,
                postal_address, notes, tags, is_favorite
         FROM contacts WHERE id = ?1",
        [id],
        |row| {
            Ok(LegacyContact {
                name: row.get(0)?,
                email: row.get(1)?,
                phone: row.get(2)?,
                company: row.get(3)?,
                job_title: row.get(4)?,
                website: row.get(5)?,
                birthday: row.get(6)?,
                postal_address: row.get(7)?,
                notes: row.get(8)?,
                tags: row.get(9)?,
                is_favorite: row.get(10)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

/// Apply a legacy contact's fields over a parsed card. A changed email or
/// phone replaces the value the old version showed: the first TEL, and the
/// first preferred EMAIL or else the first one.
fn apply_legacy(mut record: ContactRecord, legacy: &LegacyContact) -> ContactRecord {
    if !legacy.name.trim().is_empty() {
        record.name = legacy.name.clone();
    }
    record.company = legacy.company.clone();
    record.job_title = legacy.job_title.clone();
    record.website = legacy.website.clone();
    record.birthday = legacy.birthday.clone();
    record.postal_address = legacy.postal_address.clone();
    record.notes = legacy.notes.clone();
    record.tags = legacy.tags.clone();
    for (values, value, preferred_first) in [
        (&mut record.details.emails, &legacy.email, true),
        (&mut record.details.phones, &legacy.phone, false),
    ] {
        let shown = preferred_first
            .then(|| {
                values
                    .iter()
                    .position(|candidate| candidate.preference == 1)
            })
            .flatten()
            .or((!values.is_empty()).then_some(0));
        if shown.is_some_and(|index| values[index].value.trim().eq_ignore_ascii_case(value))
            || (shown.is_none() && value.is_empty())
        {
            continue;
        }
        match shown {
            Some(index) if value.is_empty() => {
                values.remove(index);
            }
            Some(index) => values[index].value = value.clone(),
            None => values.push(ContactValue {
                value: value.clone(),
                preference: 1,
                ..Default::default()
            }),
        }
    }
    record
}

fn legacy_values(legacy: &LegacyContact) -> ContactRecord {
    let value = |value: &str| {
        (!value.is_empty())
            .then(|| ContactValue {
                value: value.to_owned(),
                preference: ContactValue::NO_PREFERENCE,
                ..Default::default()
            })
            .into_iter()
            .collect()
    };
    let mut record = ContactRecord {
        details: Default::default(),
        id: 0,
        name: legacy.name.clone(),
        email: String::new(),
        phone: String::new(),
        company: legacy.company.clone(),
        job_title: legacy.job_title.clone(),
        website: legacy.website.clone(),
        birthday: legacy.birthday.clone(),
        postal_address: legacy.postal_address.clone(),
        notes: legacy.notes.clone(),
        tags: legacy.tags.clone(),
        is_favorite: legacy.is_favorite,
        interactions: 0,
        last_interacted: None,
        account_ids: Vec::new(),
        is_managed: true,
    };
    record.details.emails = value(&legacy.email);
    record.details.phones = value(&legacy.phone);
    record.refresh_primary_values();
    record
}

fn store_new(conn: &Connection, document: String, favorite: bool) -> Result<i64> {
    let id = store(conn, None, &Card::parse(document)?)?;
    conn.execute(
        "UPDATE contact_entries SET is_favorite = ?2 WHERE id = ?1",
        params![id, favorite],
    )?;
    Ok(id)
}

/// Convert an older database inside migration 13's transaction. Contacts
/// saved in Flectar Mail become local entries. Every cached CardDAV card
/// becomes its own entry, including cards the old directory hid because they
/// had no email address. An edit that was waiting to upload is applied to its
/// card and stays pending; an edit shared by several cards cannot be assigned
/// to one of them, so each of those cards asks the user to choose a version.
/// Nothing is uploaded that was not already waiting to upload.
pub fn migrate(conn: &Connection) -> Result<()> {
    let local = conn
        .prepare(
            "SELECT id FROM contacts c WHERE (is_managed = 1 OR is_favorite = 1)
               AND NOT EXISTS (SELECT 1 FROM legacy_carddav_objects o WHERE o.contact_id = c.id)",
        )?
        .query_map([], |row| row.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for id in local {
        if let Some(legacy) = legacy_contact(conn, id)? {
            let record = legacy_values(&legacy);
            if validate(&record).is_ok() {
                store_new(
                    conn,
                    vcard::serialize(&record, &vcard::new_uid()),
                    legacy.is_favorite,
                )?;
            }
        }
    }

    struct Legacy {
        addressbook_id: i64,
        contact_id: Option<i64>,
        href: String,
        etag: Option<String>,
        card: String,
        remote_exists: bool,
        dirty: bool,
        deleted: bool,
        shared: bool,
    }
    let objects = conn
        .prepare(
            "SELECT addressbook_id, contact_id, href, etag, vcard_raw, remote_exists, dirty, deleted,
                    (SELECT COUNT(*) FROM legacy_carddav_objects s WHERE s.contact_id = o.contact_id) > 1
             FROM legacy_carddav_objects o ORDER BY id",
        )?
        .query_map([], |row| {
            Ok(Legacy {
                addressbook_id: row.get(0)?,
                contact_id: row.get(1)?,
                href: row.get(2)?,
                etag: row.get(3)?,
                card: row.get(4)?,
                remote_exists: row.get(5)?,
                dirty: row.get(6)?,
                deleted: row.get(7)?,
                shared: row.get(8)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for object in objects {
        let legacy = object
            .contact_id
            .map(|id| legacy_contact(conn, id))
            .transpose()?
            .flatten();
        let favorite = legacy.as_ref().is_some_and(|legacy| legacy.is_favorite);
        let server = vcard::parse(&object.card);
        // An unsent local change lives only in the legacy contact row.
        let edited = legacy.as_ref().filter(|_| object.dirty && !object.deleted);
        let (entry, conflict, error) = match (server, edited) {
            (Ok(_), None) => (
                Some(store_new(conn, object.card.clone(), favorite)?),
                false,
                None,
            ),
            (Ok(server), Some(legacy)) => {
                let document = vcard::update(&object.card, &apply_legacy(server, legacy))?;
                // Only a resource the server has can be the other side of a
                // conflict.
                let conflict = object.shared && object.remote_exists && object.etag.is_some();
                (Some(store_new(conn, document, favorite)?), conflict, None)
            }
            (Err(error), Some(legacy)) => {
                let record = legacy_values(legacy);
                let uid = vcard::uid(&object.card).unwrap_or_else(vcard::new_uid);
                let entry = validate(&record)
                    .is_ok()
                    .then(|| store_new(conn, vcard::serialize(&record, &uid), favorite))
                    .transpose()?;
                (entry, false, Some(error.to_string()))
            }
            // Deleting a resource does not depend on reading it.
            (Err(_), None) if object.deleted => (None, false, None),
            (Err(error), None) => (None, false, Some(error.to_string())),
        };
        conn.execute(
            "INSERT INTO carddav_objects (addressbook_id, entry_id, href, etag, remote_exists,
               dirty, deleted, conflict_vcard, conflict_etag, last_error)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                object.addressbook_id,
                entry,
                object.href,
                object.etag,
                object.remote_exists,
                object.dirty && (entry.is_some() || object.deleted),
                object.deleted,
                conflict.then_some(&object.card),
                object.etag.as_deref().filter(|_| conflict),
                error,
            ],
        )?;
    }

    // Profile fields now live in entries; contacts keeps mail history only.
    conn.execute_batch(
        "DROP TABLE legacy_carddav_objects;
         DROP INDEX idx_contacts_directory;
         DELETE FROM contacts WHERE send_count = 0 AND recv_count = 0;
         ALTER TABLE contacts DROP COLUMN phone;
         ALTER TABLE contacts DROP COLUMN company;
         ALTER TABLE contacts DROP COLUMN job_title;
         ALTER TABLE contacts DROP COLUMN website;
         ALTER TABLE contacts DROP COLUMN birthday;
         ALTER TABLE contacts DROP COLUMN postal_address;
         ALTER TABLE contacts DROP COLUMN notes;
         ALTER TABLE contacts DROP COLUMN tags;
         ALTER TABLE contacts DROP COLUMN is_favorite;
         ALTER TABLE contacts DROP COLUMN is_managed;
         ALTER TABLE contacts DROP COLUMN updated_at;",
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{
        repo::{carddav, contacts},
        testutil,
    };

    fn setup() -> (Connection, i64) {
        let conn = testutil::conn();
        testutil::seed_account(&conn);
        let book = carddav::upsert_addressbook(
            &conn,
            1,
            "https://dav.example/books/",
            Some("Personal"),
            false,
        )
        .unwrap();
        (conn, book)
    }

    fn card(uid: &str, extra: &str) -> String {
        format!(
            "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:{uid}\r\nFN:Person {uid}\r\nN:Family;Given;;;\r\nTEL;TYPE=CELL:123\r\n{extra}END:VCARD\r\n"
        )
    }

    fn import(conn: &Connection, book: i64, uid: &str, extra: &str) -> ContactRecord {
        let doc = card(uid, extra);
        let href = format!("/{uid}.vcf");
        carddav::apply_remote(
            conn,
            book,
            &href,
            Some("v1"),
            &Card::parse(doc.clone()).unwrap(),
        )
        .unwrap();
        let id = conn
            .query_row(
                "SELECT entry_id FROM carddav_objects WHERE href=?1",
                [href],
                |r| r.get(0),
            )
            .unwrap();
        contacts::get_record(conn, id).unwrap().unwrap()
    }

    fn edit(conn: &Connection, record: &ContactRecord) -> ContactRecord {
        let id = save(conn, record).unwrap();
        contacts::get_record(conn, id).unwrap().unwrap()
    }

    fn email(value: &str) -> ContactValue {
        ContactValue {
            value: value.into(),
            preference: ContactValue::NO_PREFERENCE,
            ..Default::default()
        }
    }

    #[test]
    fn phone_only_people_stay_distinct_and_gain_email_without_changing_identity() {
        let (conn, book) = setup();
        let mut a = import(&conn, book, "a", "");
        let b = import(&conn, book, "b", "");
        assert_ne!(a.id, b.id);
        assert_eq!(a.details.sync, ContactSync::Synced);
        assert_eq!(a.details.source.as_deref(), Some("Personal · me@test.dev"));
        assert!(
            contacts::suggest(&conn, "Person", None, false, 20)
                .unwrap()
                .is_empty()
        );
        a.details.emails.push(email("person@example.test"));
        let saved = edit(&conn, &a);
        assert_eq!(saved.id, a.id);
        assert_eq!(saved.email, "person@example.test");
        assert_eq!(saved.details.sync, ContactSync::Pending);
        let pending = carddav::pending_uploads(&conn, 1).unwrap();
        assert_eq!(pending.len(), 1);
        let doc = pending[0].document.as_deref().unwrap();
        assert!(doc.contains("UID:a\r\n"));
        assert!(doc.contains("N:Family;Given;;;\r\n"));
        assert!(doc.contains("TEL;TYPE=CELL:123\r\n"));
        assert_eq!(
            contacts::suggest(&conn, "Person", None, false, 20)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn shared_email_never_merges_entries_from_different_books() {
        let (conn, book) = setup();
        let second =
            carddav::upsert_addressbook(&conn, 1, "https://dav.example/work/", Some("Work"), false)
                .unwrap();
        let mut a = import(&conn, book, "a", "EMAIL:shared@example.test\r\n");
        let b = import(&conn, second, "b", "EMAIL:shared@example.test\r\n");
        assert_ne!(a.id, b.id);
        a.name = "Changed".into();
        edit(&conn, &a);
        assert_eq!(carddav::pending_uploads(&conn, 1).unwrap().len(), 1);
        assert_eq!(
            contacts::get_record(&conn, b.id).unwrap().unwrap().name,
            b.name
        );
        assert_eq!(
            contacts::suggest(&conn, "shared", None, false, 20)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn upload_acknowledgement_cannot_clear_a_newer_edit() {
        let (conn, book) = setup();
        let mut a = import(&conn, book, "a", "");
        a.name = "First edit".into();
        let mut a = edit(&conn, &a);
        let sent = carddav::pending_uploads(&conn, 1).unwrap().remove(0);
        a.name = "Second edit".into();
        edit(&conn, &a);
        carddav::acknowledge_upload(
            &conn,
            sent.id,
            sent.revision,
            Some("v2"),
            sent.document.as_deref().unwrap(),
        )
        .unwrap();
        let pending = carddav::pending_uploads(&conn, 1).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].etag.as_deref(), Some("v2"));
        assert!(
            pending[0]
                .document
                .as_deref()
                .unwrap()
                .contains("Second edit")
        );
        assert_eq!(
            contacts::get_record(&conn, a.id).unwrap().unwrap().name,
            "Second edit"
        );
    }

    #[test]
    fn acknowledgement_stores_the_servers_normalized_document() {
        let (conn, book) = setup();
        let mut a = import(&conn, book, "a", "");
        a.name = "Edited".into();
        edit(&conn, &a);
        let sent = carddav::pending_uploads(&conn, 1).unwrap().remove(0);
        let stored = sent
            .document
            .unwrap()
            .replace("END:VCARD", "PRODID:server\r\nEND:VCARD");
        carddav::acknowledge_upload(&conn, sent.id, sent.revision, Some("v2"), &stored).unwrap();
        let record = contacts::get_record(&conn, a.id).unwrap().unwrap();
        assert_eq!(record.details.sync, ContactSync::Synced);
        let document: String = conn
            .query_row(
                "SELECT document FROM contact_entries WHERE id=?1",
                [a.id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(document.contains("PRODID:server"));
    }

    #[test]
    fn a_lost_upload_response_is_recognized_when_the_server_returns_it() {
        let (conn, book) = setup();
        let mut a = import(&conn, book, "a", "");
        a.name = "Uploaded".into();
        edit(&conn, &a);
        let sent = carddav::pending_uploads(&conn, 1).unwrap().remove(0);
        let document = sent.document.unwrap();
        carddav::apply_remote(
            &conn,
            book,
            "/a.vcf",
            Some("v2"),
            &Card::parse(document.clone()).unwrap(),
        )
        .unwrap();
        assert!(carddav::pending_uploads(&conn, 1).unwrap().is_empty());
        let record = contacts::get_record(&conn, a.id).unwrap().unwrap();
        assert_eq!(record.name, "Uploaded");
        assert_eq!(record.details.sync, ContactSync::Synced);
    }

    #[test]
    fn conflicts_keep_both_versions_until_the_user_chooses() {
        let (conn, book) = setup();
        for keep_local in [true, false] {
            let uid = if keep_local { "local" } else { "server" };
            let href = format!("/{uid}.vcf");
            let mut a = import(&conn, book, uid, "");
            a.name = "Local".into();
            let a = edit(&conn, &a);
            let remote = card(uid, "NOTE:Server edit\r\n");
            carddav::apply_remote(
                &conn,
                book,
                &href,
                Some("v2"),
                &Card::parse(remote.clone()).unwrap(),
            )
            .unwrap();
            let current = contacts::get_record(&conn, a.id).unwrap().unwrap();
            assert_eq!(current.name, "Local");
            assert_eq!(current.details.sync, ContactSync::Conflict);
            let mut renamed = current.clone();
            renamed.name = "Edited during conflict".into();
            assert!(save(&conn, &renamed).is_err());
            assert!(carddav::pending_uploads(&conn, 1).unwrap().is_empty());
            assert!(
                carddav::resolve_conflict(&conn, a.id, a.details.revision, keep_local).is_err()
            );
            carddav::resolve_conflict(&conn, a.id, current.details.revision, keep_local).unwrap();
            let resolved = contacts::get_record(&conn, a.id).unwrap().unwrap();
            if keep_local {
                let pending = carddav::pending_uploads(&conn, 1).unwrap();
                assert_eq!(pending[0].etag.as_deref(), Some("v2"));
                assert!(pending[0].document.as_deref().unwrap().contains("FN:Local"));
                assert_eq!(resolved.details.sync, ContactSync::Pending);
                // Upload the kept version so the next case starts clean.
                carddav::acknowledge_upload(
                    &conn,
                    pending[0].id,
                    pending[0].revision,
                    Some("v3"),
                    pending[0].document.as_deref().unwrap(),
                )
                .unwrap();
            } else {
                assert_eq!(resolved.name, "Person server");
                assert_eq!(resolved.notes, "Server edit");
                assert_eq!(resolved.details.sync, ContactSync::Synced);
            }
        }
        assert!(carddav::pending_uploads(&conn, 1).unwrap().is_empty());
    }

    #[test]
    fn a_server_edit_during_a_pending_deletion_requires_a_choice() {
        let (conn, book) = setup();
        let a = import(&conn, book, "a", "");
        delete(&conn, a.id, a.details.revision).unwrap();
        assert!(contacts::get_record(&conn, a.id).unwrap().is_none());
        let server = card("a", "NOTE:Server edit\r\n");
        carddav::apply_remote(
            &conn,
            book,
            "/a.vcf",
            Some("v2"),
            &Card::parse(server.clone()).unwrap(),
        )
        .unwrap();
        let conflict = contacts::get_record(&conn, a.id).unwrap().unwrap();
        assert_eq!(conflict.details.sync, ContactSync::Conflict);
        assert!(carddav::pending_uploads(&conn, 1).unwrap().is_empty());
        carddav::resolve_conflict(&conn, a.id, conflict.details.revision, false).unwrap();
        let restored = contacts::get_record(&conn, a.id).unwrap().unwrap();
        assert_eq!(restored.notes, "Server edit");
        assert_eq!(restored.details.sync, ContactSync::Synced);
    }

    #[test]
    fn a_server_deletion_of_an_edited_contact_requires_a_choice() {
        let (conn, book) = setup();
        let mut a = import(&conn, book, "a", "");
        a.name = "Edited".into();
        edit(&conn, &a);
        carddav::remove_remote(&conn, book, "/a.vcf").unwrap();
        let conflict = contacts::get_record(&conn, a.id).unwrap().unwrap();
        assert_eq!(conflict.details.sync, ContactSync::Conflict);
        carddav::resolve_conflict(&conn, a.id, conflict.details.revision, true).unwrap();
        let pending = carddav::pending_uploads(&conn, 1).unwrap();
        assert!(!pending[0].remote_exists);
        assert_eq!(pending[0].etag, None);
    }

    #[test]
    fn unavailable_books_keep_unsynced_edits_and_removal_keeps_them_locally() {
        let (conn, book) = setup();
        let mut a = import(&conn, book, "a", "");
        a.name = "Unsynced".into();
        edit(&conn, &a);
        let clean = import(&conn, book, "clean", "");
        carddav::retain_addressbooks(&conn, 1, &Default::default()).unwrap();
        assert!(carddav::pending_uploads(&conn, 1).unwrap().is_empty());
        let record = contacts::get_record(&conn, a.id).unwrap().unwrap();
        assert!(record.details.read_only);
        assert_eq!(record.details.sync, ContactSync::Unavailable);
        let mut renamed = record.clone();
        renamed.name = "Edited while unavailable".into();
        assert!(save(&conn, &renamed).is_err());
        // Favorites are local, so they can still change.
        let mut favorite = record.clone();
        favorite.is_favorite = true;
        assert!(edit(&conn, &favorite).is_favorite);
        assert!(carddav::pending_uploads(&conn, 1).unwrap().is_empty());

        carddav::disconnect(&conn, 1).unwrap();
        let record = contacts::get_record(&conn, a.id).unwrap().unwrap();
        assert!(!record.details.read_only);
        assert_eq!(record.name, "Unsynced");
        assert_eq!(record.details.source, None);
        assert_eq!(record.details.sync, ContactSync::Detached);
        let clean = contacts::get_record(&conn, clean.id).unwrap().unwrap();
        assert_eq!(clean.details.sync, ContactSync::Local);
        edit(&conn, &clean);
    }

    #[test]
    fn account_removal_keeps_work_but_does_not_resurrect_deletions() {
        let (conn, book) = setup();
        let mut kept = import(&conn, book, "kept", "");
        kept.name = "Offline edit".into();
        edit(&conn, &kept);
        let removed = import(&conn, book, "removed", "");
        delete(&conn, removed.id, removed.details.revision).unwrap();
        conn.execute("DELETE FROM accounts WHERE id=1", []).unwrap();
        let records = contacts::list_records(&conn, "", 20).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].name, "Offline edit");
        assert_eq!(records[0].details.sync, ContactSync::Detached);
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM contact_entries", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert!(
            conn.prepare("PRAGMA foreign_key_check")
                .unwrap()
                .query([])
                .unwrap()
                .next()
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn stale_editors_cannot_overwrite_newer_changes() {
        let (conn, book) = setup();
        let mut a = import(&conn, book, "a", "");
        let stale = a.clone();
        a.name = "New".into();
        edit(&conn, &a);
        assert!(save(&conn, &stale).is_err());
        assert!(delete(&conn, i64::MAX, 0).is_ok());
    }

    #[test]
    fn new_contacts_upload_under_their_own_uid_and_round_trip_repeated_values() {
        let (conn, _) = setup();
        let mut record = vcard::parse(&card("draft", "")).unwrap();
        record.id = 0;
        record.name.clear();
        let saved = edit(&conn, &record);
        assert_eq!(saved.name, "123");
        assert_eq!(saved.details.sync, ContactSync::Local);
        assert!(carddav::attach_new_contact(&conn, 1, saved.id).unwrap());
        let pending = carddav::pending_uploads(&conn, 1).unwrap().remove(0);
        let document = pending.document.unwrap();
        let uid = vcard::uid(&document).unwrap();
        assert_ne!(uid, "draft");
        assert_eq!(
            pending.href,
            format!("/books/{}.vcf", uid.trim_start_matches("urn:uuid:"))
        );
        assert!(!pending.remote_exists);

        let mut saved = contacts::get_record(&conn, saved.id).unwrap().unwrap();
        assert_eq!(saved.details.sync, ContactSync::Pending);
        saved.details.emails.push(ContactValue {
            value: "person@example.test".into(),
            label: "WORK".into(),
            preference: 1,
            ..Default::default()
        });
        let saved = edit(&conn, &saved);
        assert_eq!(saved.email, "person@example.test");
        let doc = carddav::pending_uploads(&conn, 1)
            .unwrap()
            .pop()
            .unwrap()
            .document
            .unwrap();
        assert!(doc.contains("EMAIL;TYPE=WORK,PREF:person@example.test"));
        assert_eq!(vcard::parse(&doc).unwrap().phone, "123");
        assert!(carddav::object_etags(&conn, 1).unwrap().is_empty());
    }

    #[test]
    fn labels_cannot_inject_properties_and_oversized_edits_are_rejected() {
        let (conn, book) = setup();
        let mut record = import(&conn, book, "a", "EMAIL:a@example.test\r\n");
        record.details.emails[0].label = "WORK\r\nX-INJECT:yes".into();
        assert!(save(&conn, &record).is_err());
        record.details.emails[0].label = "WORK".into();
        record.notes = "n".repeat(vcard::MAX_VCARD_BYTES);
        assert!(save(&conn, &record).is_err());
        assert!(carddav::pending_uploads(&conn, 1).unwrap().is_empty());
        record.notes.clear();
        record.name.clear();
        record.details.emails.clear();
        record.details.phones.clear();
        assert!(save(&conn, &record).is_err());
    }

    #[test]
    fn photos_are_stored_once_and_released_with_their_last_entry() {
        let (conn, book) = setup();
        let photo = "PHOTO;ENCODING=b;TYPE=PNG:AAAB\r\n";
        let a = import(&conn, book, "a", photo);
        let b = import(&conn, book, "b", photo);
        let hash = a.details.photo_hash.clone().unwrap();
        assert_eq!(b.details.photo_hash.as_deref(), Some(hash.as_str()));
        assert_eq!(photo_bytes(&conn, &hash).unwrap(), Some(vec![0, 0, 1]));
        carddav::remove_remote(&conn, book, "/a.vcf").unwrap();
        assert!(photo_bytes(&conn, &hash).unwrap().is_some());
        carddav::remove_remote(&conn, book, "/b.vcf").unwrap();
        assert_eq!(photo_bytes(&conn, &hash).unwrap(), None);
    }

    #[test]
    fn local_contacts_are_deleted_immediately_and_suggestions_are_forgotten() {
        let (conn, _) = setup();
        let mut record = vcard::parse(&card("local", "")).unwrap();
        record.id = 0;
        let saved = edit(&conn, &record);
        delete(&conn, saved.id, saved.details.revision).unwrap();
        assert!(contacts::get_record(&conn, saved.id).unwrap().is_none());
        conn.execute(
            "INSERT INTO contacts (id, email, send_count) VALUES (7, 'seen@example.test', 1)",
            [],
        )
        .unwrap();
        let suggestion = contacts::get_record(&conn, -7).unwrap().unwrap();
        assert_eq!(suggestion.details.emails[0].value, "seen@example.test");
        delete(&conn, -7, 0).unwrap();
        assert!(contacts::get_record(&conn, -7).unwrap().is_none());
    }
    #[test]
    fn a_failed_download_does_not_lock_a_conflict_or_churn_it() {
        let (conn, book) = setup();
        let mut a = import(&conn, book, "a", "");
        a.name = "Local".into();
        edit(&conn, &a);
        carddav::record_resource_error(&conn, book, "/a.vcf", None, "HTTP 500").unwrap();
        assert!(
            contacts::get_record(&conn, a.id)
                .unwrap()
                .unwrap()
                .details
                .read_only
        );
        let remote = Card::parse(card("a", "NOTE:Server edit\r\n")).unwrap();
        carddav::apply_remote(&conn, book, "/a.vcf", Some("v2"), &remote).unwrap();
        let conflict = contacts::get_record(&conn, a.id).unwrap().unwrap();
        assert_eq!(conflict.details.sync, ContactSync::Conflict);
        assert!(!conflict.details.read_only);
        // Downloading the same server version again changes nothing.
        carddav::apply_remote(&conn, book, "/a.vcf", Some("v2"), &remote).unwrap();
        let again = contacts::get_record(&conn, a.id).unwrap().unwrap();
        assert_eq!(again.details.revision, conflict.details.revision);
        carddav::resolve_conflict(&conn, a.id, again.details.revision, true).unwrap();
        let pending = carddav::pending_uploads(&conn, 1).unwrap();
        assert_eq!(pending[0].etag.as_deref(), Some("v2"));
    }

    #[test]
    fn reconnecting_links_kept_contacts_instead_of_duplicating_them() {
        let (conn, book) = setup();
        let clean = import(&conn, book, "clean", "");
        let edited = import(&conn, book, "edited", "");
        carddav::disconnect(&conn, 1).unwrap();
        let mut edited = contacts::get_record(&conn, edited.id).unwrap().unwrap();
        edited.name = "Changed while disconnected".into();
        edit(&conn, &edited);

        let book = carddav::upsert_addressbook(
            &conn,
            1,
            "https://dav.example/books/",
            Some("Personal"),
            false,
        )
        .unwrap();
        for uid in ["clean", "edited"] {
            let server = Card::parse(card(uid, "")).unwrap();
            carddav::apply_remote(&conn, book, &format!("/{uid}.vcf"), Some("v1"), &server)
                .unwrap();
        }
        assert_eq!(contacts::list_records(&conn, "", 20).unwrap().len(), 2);
        let clean = contacts::get_record(&conn, clean.id).unwrap().unwrap();
        assert_eq!(clean.details.sync, ContactSync::Synced);
        let edited = contacts::get_record(&conn, edited.id).unwrap().unwrap();
        assert_eq!(edited.name, "Changed while disconnected");
        assert_eq!(edited.details.sync, ContactSync::Conflict);
        carddav::resolve_conflict(&conn, edited.id, edited.details.revision, true).unwrap();
        let pending = carddav::pending_uploads(&conn, 1).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].href, "/edited.vcf");
        assert!(pending[0].remote_exists);
    }
}
