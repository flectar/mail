use crate::error::Result;
use crate::models::{
    Address, ContactRecord, ContactRecordCursor, ContactRecordPage, ContactSuggestion,
};
use crate::search::fold;
use rusqlite::{Connection, OptionalExtension, Row, params};

/// A real address-book entry is explicitly saved or favorited in Flectar Mail,
/// or backed by a live CardDAV object. Mail-derived identities deliberately
/// stay outside this set until the user takes one of those explicit actions.
fn saved_contact_predicate(alias: &str) -> String {
    format!(
        "({alias}.is_managed = 1 OR {alias}.is_favorite = 1 OR EXISTS (
            SELECT 1 FROM carddav_objects co
            WHERE co.contact_id = {alias}.id
              AND co.remote_exists = 1 AND co.deleted = 0
        ))"
    )
}

fn record_from_row(row: &Row<'_>) -> rusqlite::Result<ContactRecord> {
    let account_ids = row
        .get::<_, String>(15)?
        .split(',')
        .filter_map(|value| value.parse::<i64>().ok())
        .collect();
    Ok(ContactRecord {
        id: row.get(0)?,
        name: row.get(1)?,
        email: row.get(2)?,
        phone: row.get(3)?,
        company: row.get(4)?,
        job_title: row.get(5)?,
        website: row.get(6)?,
        birthday: row.get(7)?,
        postal_address: row.get(8)?,
        notes: row.get(9)?,
        tags: row.get(10)?,
        is_favorite: row.get(11)?,
        interactions: row.get(12)?,
        last_interacted: row.get(13)?,
        is_managed: row.get::<_, i64>(14)? != 0,
        account_ids,
    })
}

/// Per-account boundaries prevent provider history backfills from being
/// mistaken for new relationship activity. The defensive insert covers
/// profiles created by unusual import paths that bypassed the account trigger.
pub fn learning_boundaries(conn: &Connection, account_id: i64, now_ms: i64) -> Result<(i64, i64)> {
    conn.execute(
        "INSERT OR IGNORE INTO contact_learning_state
             (account_id, outgoing_since, incoming_since)
         VALUES (?1, ?2, ?2)",
        params![account_id, now_ms],
    )?;
    conn.query_row(
        "SELECT outgoing_since, incoming_since
         FROM contact_learning_state WHERE account_id=?1",
        [account_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .map_err(Into::into)
}

/// Starting a learning direction is intentionally prospective: messages that
/// predate the opt-in never enter Suggestions if a provider backfills later.
pub fn advance_learning_boundaries(
    conn: &Connection,
    outgoing: bool,
    incoming: bool,
    now_ms: i64,
) -> Result<()> {
    if outgoing {
        conn.execute(
            "UPDATE contact_learning_state SET outgoing_since=?1",
            [now_ms],
        )?;
    }
    if incoming {
        conn.execute(
            "UPDATE contact_learning_state SET incoming_since=?1",
            [now_ms],
        )?;
    }
    Ok(())
}

/// Record an address seen in mail headers on `account_id`'s mail. `sent` = we
/// sent to them. Updates both the global `contacts` row (identity + global
/// affinity used by search and sender_known) and the per-account
/// `contact_accounts` row that scopes compose autocomplete to the sending
/// account.
pub fn harvest(
    conn: &Connection,
    account_id: i64,
    addr: &Address,
    sent: bool,
    when_ms: i64,
) -> Result<()> {
    if addr.email.is_empty() || !addr.email.contains('@') {
        return Ok(());
    }
    let email = addr.email.to_lowercase();
    let name = addr.name.as_deref().unwrap_or("");
    let folded = fold(&format!("{} {}", name, addr.email));
    let saved = saved_contact_predicate("contacts");
    conn.execute(
        &format!(
            "INSERT INTO contacts (email, name, folded, send_count, recv_count, last_interacted)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(email) DO UPDATE SET
            name = CASE WHEN {saved} THEN contacts.name
                   ELSE COALESCE(NULLIF(excluded.name, ''), contacts.name) END,
            folded = CASE
                WHEN (NOT {saved} AND NULLIF(excluded.name, '') IS NOT NULL)
                OR contacts.folded IS NULL
                THEN excluded.folded ELSE contacts.folded END,
            send_count = contacts.send_count + ?4,
            recv_count = contacts.recv_count + ?5,
            last_interacted = MAX(COALESCE(contacts.last_interacted, 0), ?6)"
        ),
        params![email, name, folded, sent as i64, (!sent) as i64, when_ms],
    )?;
    conn.execute(
        "INSERT INTO contact_accounts (contact_id, account_id, send_count, recv_count, last_interacted)
         SELECT id, ?2, ?3, ?4, ?5 FROM contacts WHERE email = ?1
         ON CONFLICT(contact_id, account_id) DO UPDATE SET
            send_count = contact_accounts.send_count + ?3,
            recv_count = contact_accounts.recv_count + ?4,
            last_interacted = MAX(COALESCE(contact_accounts.last_interacted, 0), ?5)",
        params![email, account_id, sent as i64, (!sent) as i64, when_ms],
    )?;
    Ok(())
}

/// Learn all recipients of a successfully submitted local message exactly
/// once. Provider send reconciliation may safely call this more than once.
pub fn record_sent_recipients(
    conn: &Connection,
    account_id: i64,
    message_id: i64,
    when_ms: i64,
) -> Result<usize> {
    let row = conn
        .query_row(
            "SELECT contact_learning_recorded, to_json, cc_json, bcc_json
             FROM messages WHERE id=?1 AND account_id=?2",
            params![message_id, account_id],
            |row| {
                Ok((
                    row.get::<_, bool>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .optional()?;
    let Some((already_recorded, to_json, cc_json, bcc_json)) = row else {
        return Err(crate::error::CoreError::NotFound(format!(
            "message {message_id}"
        )));
    };
    if already_recorded {
        return Ok(0);
    }

    let settings = super::settings::get(conn)?;
    let mut learned = 0;
    if settings.collect_outgoing_contacts {
        let own_addresses = super::sender_identities::list(conn, account_id)?
            .into_iter()
            .filter(|identity| identity.is_primary || identity.verification_status == "accepted")
            .map(|identity| identity.email.to_ascii_lowercase())
            .collect::<std::collections::HashSet<_>>();
        let mut recipients = Vec::new();
        for json in [&to_json, &cc_json, &bcc_json] {
            recipients.extend(serde_json::from_str::<Vec<Address>>(json).unwrap_or_default());
        }
        let mut seen = std::collections::HashSet::new();
        for recipient in recipients {
            let email = recipient.email.trim().to_ascii_lowercase();
            if !email.is_empty() && !own_addresses.contains(&email) && seen.insert(email) {
                harvest(conn, account_id, &recipient, true, when_ms)?;
                learned += 1;
            }
        }
    }
    conn.execute(
        "UPDATE messages SET contact_learning_recorded=1
         WHERE id=?1 AND account_id=?2",
        params![message_id, account_id],
    )?;
    Ok(learned)
}

/// One-time fill of `contacts.folded` for rows harvested before the column
/// existed. Cheap no-op once every row is folded.
pub fn backfill_folded(conn: &Connection) -> Result<()> {
    loop {
        let rows = {
            let mut stmt = conn.prepare(
                "SELECT id, COALESCE(name,''), email FROM contacts WHERE folded IS NULL LIMIT 256",
            )?;
            stmt.query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
        };
        if rows.is_empty() {
            break;
        }
        let tx = conn.unchecked_transaction()?;
        for (id, name, email) in rows {
            tx.execute(
                "UPDATE contacts SET folded = ?1 WHERE id = ?2",
                params![fold(&format!("{name} {email}")), id],
            )?;
        }
        tx.commit()?;
    }
    Ok(())
}

/// Build the WHERE fragment requiring every folded query token to appear in
/// `contacts.folded`, pushing one `%tok%` bind per token. Returns None for
/// queries with no usable tokens.
fn folded_clauses(query: &str, bind: &mut Vec<Box<dyn rusqlite::types::ToSql>>) -> Option<String> {
    let folded = fold(query);
    let tokens: Vec<&str> = folded.split_whitespace().collect();
    if tokens.is_empty() {
        return None;
    }
    let mut clauses = Vec::with_capacity(tokens.len());
    for tok in tokens {
        // Escape LIKE wildcards so a literal % or _ in the query can't scan-match.
        let esc = tok
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        bind.push(Box::new(format!("%{esc}%")));
        clauses.push(format!(
            "LOWER(COALESCE(folded, email) || ' ' || COALESCE(job_title, '') || ' ' ||
                   COALESCE(website, '') || ' ' || COALESCE(postal_address, ''))
             LIKE ?{} ESCAPE '\\'",
            bind.len()
        ));
    }
    Some(clauses.join(" AND "))
}

fn record_where_clause(
    query: &str,
    account_id: Option<i64>,
    favorites_only: bool,
    suggestions_only: bool,
    bind: &mut Vec<Box<dyn rusqlite::types::ToSql>>,
) -> String {
    let mut clauses = Vec::new();
    if let Some(query_clause) = folded_clauses(query, bind) {
        clauses.push(format!("({query_clause})"));
    }
    if favorites_only {
        clauses.push("is_favorite = 1".to_owned());
    }
    let saved = saved_contact_predicate("contacts");
    clauses.push(if suggestions_only {
        format!("NOT {saved} AND (send_count > 0 OR recv_count > 0)")
    } else {
        saved
    });
    if let Some(account_id) = account_id {
        bind.push(Box::new(account_id));
        clauses.push(if suggestions_only {
            format!(
                "EXISTS (
                    SELECT 1 FROM contact_accounts ca
                    WHERE ca.contact_id = contacts.id AND ca.account_id = ?{}
                )",
                bind.len()
            )
        } else {
            format!(
                "(is_managed = 1 OR EXISTS (
                SELECT 1 FROM contact_accounts ca
                WHERE ca.contact_id = contacts.id AND ca.account_id = ?{}
                ))",
                bind.len()
            )
        });
    }
    if clauses.is_empty() {
        "1 = 1".to_owned()
    } else {
        clauses.join(" AND ")
    }
}

/// Contacts matching every query token (accent- and case-insensitive), ranked
/// by interaction affinity - people you actually email float to the top. When
/// `account_id` is Some, only contacts that account has corresponded with are
/// returned, ranked by that account's affinity; None searches all contacts.
pub fn suggest(
    conn: &Connection,
    query: &str,
    account_id: Option<i64>,
    include_suggestions: bool,
    limit: i64,
) -> Result<Vec<ContactSuggestion>> {
    let mut bind: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    let Some(where_sql) = folded_clauses(query, &mut bind) else {
        return Ok(Vec::new());
    };
    // `contact_accounts` has no name/email/folded columns, so the folded WHERE
    // clause stays unambiguous; only the affinity columns get an alias.
    let saved_filter = (!include_suggestions)
        .then(|| format!(" AND {}", saved_contact_predicate("c")))
        .unwrap_or_default();
    let sql = if let Some(aid) = account_id {
        bind.push(Box::new(aid));
        let aid_ix = bind.len();
        bind.push(Box::new(limit));
        format!(
            "SELECT c.name, c.email,
                    COALESCE(ca.send_count * 3 + ca.recv_count,
                             c.send_count * 3 + c.recv_count)
             FROM contacts c
             LEFT JOIN contact_accounts ca
               ON ca.contact_id = c.id AND ca.account_id = ?{aid_ix}
             WHERE ({where_sql}) AND (ca.account_id IS NOT NULL OR c.is_managed = 1)
                   {saved_filter}
             ORDER BY COALESCE(ca.send_count * 3 + ca.recv_count,
                               c.send_count * 3 + c.recv_count) DESC,
                      COALESCE(ca.last_interacted, c.last_interacted) DESC
             LIMIT ?{}",
            bind.len()
        )
    } else {
        bind.push(Box::new(limit));
        format!(
            "SELECT name, email, send_count * 3 + recv_count FROM contacts c
             WHERE {where_sql}{saved_filter}
             ORDER BY (send_count * 3 + recv_count) DESC, last_interacted DESC
             LIMIT ?{}",
            bind.len()
        )
    };
    let mut stmt = conn.prepare(&sql)?;
    let params_ref: Vec<&dyn rusqlite::types::ToSql> = bind.iter().map(|b| b.as_ref()).collect();
    let rows = stmt
        .query_map(params_ref.as_slice(), |r| {
            Ok(ContactSuggestion {
                name: r.get(0)?,
                email: r.get(1)?,
                interactions: r.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn autocomplete(
    conn: &Connection,
    prefix: &str,
    account_id: Option<i64>,
    include_suggestions: bool,
    limit: i64,
) -> Result<Vec<Address>> {
    Ok(
        suggest(conn, prefix, account_id, include_suggestions, limit)?
            .into_iter()
            .map(|c| Address {
                name: c.name,
                email: c.email,
            })
            .collect(),
    )
}

/// List address-book records for the dedicated contacts workspace. An empty
/// query returns the full directory; otherwise every folded token must match.
pub fn list_records(conn: &Connection, query: &str, limit: i64) -> Result<Vec<ContactRecord>> {
    let mut bind: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    let query_sql = folded_clauses(query, &mut bind).unwrap_or_else(|| "1 = 1".to_owned());
    let saved = saved_contact_predicate("contacts");
    bind.push(Box::new(limit.clamp(1, 500)));
    let sql = format!(
        "SELECT id, COALESCE(name, ''), email, phone, company, job_title,
                website, birthday, postal_address, notes, tags, is_favorite,
                send_count * 3 + recv_count, last_interacted,
                CASE WHEN {saved} THEN 1 ELSE 0 END,
                COALESCE((SELECT GROUP_CONCAT(ca.account_id)
                          FROM contact_accounts ca
                          WHERE ca.contact_id = contacts.id), '')
         FROM contacts
         WHERE ({query_sql}) AND {saved}
         ORDER BY is_favorite DESC,
                  CASE WHEN name IS NULL OR name = '' THEN email ELSE name END COLLATE NOCASE,
                  email COLLATE NOCASE,
                  id ASC
         LIMIT ?{}",
        bind.len()
    );
    let params_ref: Vec<&dyn rusqlite::types::ToSql> = bind.iter().map(|b| b.as_ref()).collect();
    let mut stmt = conn.prepare(&sql)?;
    Ok(stmt
        .query_map(params_ref.as_slice(), record_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Query one strict directory page and the scalar counts needed by the
/// sidebar. The keyset cursor follows the complete sort tuple, so
/// contacts inserted before the cursor cannot shift or duplicate later pages.
pub fn list_record_page(
    conn: &Connection,
    query: &str,
    account_id: Option<i64>,
    favorites_only: bool,
    suggestions_only: bool,
    cursor: Option<&ContactRecordCursor>,
    limit: i64,
) -> Result<ContactRecordPage> {
    let limit = limit.clamp(1, 100);

    let mut bind: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    let mut where_sql = record_where_clause(
        query,
        account_id,
        favorites_only,
        suggestions_only,
        &mut bind,
    );
    if let Some(cursor) = cursor {
        bind.push(Box::new(i64::from(cursor.is_favorite)));
        let favorite_index = bind.len();
        bind.push(Box::new(cursor.sort_name.clone()));
        let name_index = bind.len();
        bind.push(Box::new(cursor.email.clone()));
        let email_index = bind.len();
        bind.push(Box::new(cursor.id));
        let id_index = bind.len();
        let sort_name = "CASE WHEN name IS NULL OR name = '' THEN email ELSE name END";
        where_sql.push_str(&format!(
            " AND (
                is_favorite < ?{favorite_index}
                OR (is_favorite = ?{favorite_index} AND (
                    {sort_name} COLLATE NOCASE > ?{name_index} COLLATE NOCASE
                    OR ({sort_name} COLLATE NOCASE = ?{name_index} COLLATE NOCASE AND (
                        email COLLATE NOCASE > ?{email_index} COLLATE NOCASE
                        OR (email COLLATE NOCASE = ?{email_index} COLLATE NOCASE
                            AND id > ?{id_index})
                    ))
                ))
            )"
        ));
    }
    bind.push(Box::new(limit + 1));
    let limit_index = bind.len();
    let saved = saved_contact_predicate("contacts");
    let sql = format!(
        "SELECT id, COALESCE(name, ''), email, phone, company, job_title,
                website, birthday, postal_address, notes, tags, is_favorite,
                send_count * 3 + recv_count, last_interacted,
                CASE WHEN {saved} THEN 1 ELSE 0 END,
                COALESCE((SELECT GROUP_CONCAT(ca.account_id)
                          FROM contact_accounts ca
                          WHERE ca.contact_id = contacts.id), '')
         FROM contacts
         WHERE {where_sql}
         ORDER BY is_favorite DESC,
                  CASE WHEN name IS NULL OR name = '' THEN email ELSE name END COLLATE NOCASE,
                  email COLLATE NOCASE,
                  id ASC
         LIMIT ?{limit_index}"
    );
    let params_ref = bind
        .iter()
        .map(|value| value.as_ref())
        .collect::<Vec<&dyn rusqlite::types::ToSql>>();
    let mut stmt = conn.prepare(&sql)?;
    let mut records = stmt
        .query_map(params_ref.as_slice(), record_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let has_more = records.len() > limit as usize;
    if has_more {
        records.truncate(limit as usize);
    }

    let (total_count, favorite_count, suggestion_count) = conn.query_row(
        &format!(
            "SELECT
                COALESCE(SUM(CASE WHEN {saved} THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN {saved} AND is_favorite = 1 THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN NOT {saved}
                    AND (send_count > 0 OR recv_count > 0) THEN 1 ELSE 0 END), 0)
             FROM contacts"
        ),
        [],
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        },
    )?;
    let saved_for_counts = saved_contact_predicate("c");
    let mut account_stmt = conn.prepare(&format!(
        "SELECT a.id, COUNT(DISTINCT c.id)
         FROM accounts a
         LEFT JOIN contacts c ON {saved_for_counts}
           AND (c.is_managed = 1 OR EXISTS (
                SELECT 1 FROM contact_accounts ca
                WHERE ca.contact_id = c.id AND ca.account_id = a.id
           ))
         GROUP BY a.id"
    ))?;
    let account_counts = account_stmt
        .query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?.max(0) as usize))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let next_cursor = has_more.then(|| {
        let last = records
            .last()
            .expect("a page with a look-ahead row has a retained row");
        ContactRecordCursor {
            is_favorite: last.is_favorite,
            sort_name: if last.name.is_empty() {
                last.email.clone()
            } else {
                last.name.clone()
            },
            email: last.email.clone(),
            id: last.id,
        }
    });

    Ok(ContactRecordPage {
        next_cursor,
        records,
        total_count: total_count.max(0) as usize,
        favorite_count: favorite_count.max(0) as usize,
        suggestion_count: suggestion_count.max(0) as usize,
        account_counts,
    })
}

/// Remove mail-derived suggestion-only identities and erase interaction
/// ranking from saved contacts. CardDAV/manual entries and their account
/// associations remain intact.
pub fn clear_suggestions(conn: &Connection, now_ms: i64) -> Result<usize> {
    let saved = saved_contact_predicate("contacts");
    let removed = conn.execute(&format!("DELETE FROM contacts WHERE NOT {saved}"), [])?;
    conn.execute(
        "UPDATE contacts
         SET send_count = 0, recv_count = 0, last_interacted = NULL
         WHERE send_count != 0 OR recv_count != 0 OR last_interacted IS NOT NULL",
        [],
    )?;
    conn.execute(
        "UPDATE contact_accounts
         SET send_count = 0, recv_count = 0, last_interacted = NULL
         WHERE send_count != 0 OR recv_count != 0 OR last_interacted IS NOT NULL",
        [],
    )?;
    advance_learning_boundaries(conn, true, true, now_ms)?;
    Ok(removed)
}

/// Insert or update a user-managed contact and return the canonical stored row.
pub fn save_record(
    conn: &Connection,
    record: &ContactRecord,
    now_ms: i64,
) -> Result<ContactRecord> {
    let email = record.email.trim().to_lowercase();
    let name = record.name.trim();
    if email.is_empty() || !email.contains('@') {
        return Err(crate::error::CoreError::Other(
            "contact email address is invalid".into(),
        ));
    }
    let folded = fold(&format!(
        "{name} {email} {} {} {} {} {} {}",
        record.company,
        record.phone,
        record.job_title,
        record.website,
        record.tags,
        record.postal_address,
    ));
    if record.id > 0 {
        let changed = conn.execute(
            "UPDATE contacts SET name = ?1, email = ?2, folded = ?3, phone = ?4,
                    company = ?5, job_title = ?6, website = ?7, birthday = ?8,
                    postal_address = ?9, notes = ?10, tags = ?11, is_favorite = ?12,
                    is_managed = 1, updated_at = ?13
             WHERE id = ?14",
            params![
                name,
                email,
                folded,
                record.phone.trim(),
                record.company.trim(),
                record.job_title.trim(),
                record.website.trim(),
                record.birthday.trim(),
                record.postal_address.trim(),
                record.notes.trim(),
                record.tags.trim(),
                record.is_favorite,
                now_ms,
                record.id
            ],
        )?;
        if changed == 0 {
            return Err(crate::error::CoreError::Other("contact not found".into()));
        }
    } else {
        conn.execute(
            "INSERT INTO contacts (
                email, name, folded, phone, company, job_title, website, birthday,
                postal_address, notes, tags, is_favorite, is_managed, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 1, ?13)
             ON CONFLICT(email) DO UPDATE SET
                name = excluded.name,
                folded = excluded.folded,
                phone = excluded.phone,
                company = excluded.company,
                job_title = excluded.job_title,
                website = excluded.website,
                birthday = excluded.birthday,
                postal_address = excluded.postal_address,
                notes = excluded.notes,
                tags = excluded.tags,
                is_favorite = excluded.is_favorite,
                is_managed = 1,
                updated_at = excluded.updated_at",
            params![
                email,
                name,
                folded,
                record.phone.trim(),
                record.company.trim(),
                record.job_title.trim(),
                record.website.trim(),
                record.birthday.trim(),
                record.postal_address.trim(),
                record.notes.trim(),
                record.tags.trim(),
                record.is_favorite,
                now_ms
            ],
        )?;
    }
    let id = if record.id > 0 {
        record.id
    } else {
        conn.query_row(
            "SELECT id FROM contacts WHERE email = ?1 COLLATE NOCASE",
            params![email],
            |row| row.get(0),
        )?
    };
    let address_book = saved_contact_predicate("contacts");
    let mut saved = conn.query_row(
        &format!(
            "SELECT id, COALESCE(name, ''), email, phone, company, job_title,
                website, birthday, postal_address, notes, tags, is_favorite,
                send_count * 3 + recv_count, last_interacted,
                CASE WHEN {address_book} THEN 1 ELSE 0 END,
                COALESCE((SELECT GROUP_CONCAT(ca.account_id)
                          FROM contact_accounts ca
                          WHERE ca.contact_id = contacts.id), '')
         FROM contacts WHERE id = ?1"
        ),
        [id],
        record_from_row,
    )?;
    saved.email = email;
    Ok(saved)
}

pub fn delete_record(conn: &Connection, id: i64) -> Result<()> {
    if conn.execute("DELETE FROM contacts WHERE id = ?1", params![id])? == 0 {
        return Err(crate::error::CoreError::Other("contact not found".into()));
    }
    Ok(())
}

pub fn get_record(conn: &Connection, id: i64) -> Result<Option<ContactRecord>> {
    let address_book = saved_contact_predicate("contacts");
    conn.query_row(
        &format!(
            "SELECT id, COALESCE(name, ''), email, phone, company, job_title,
                website, birthday, postal_address, notes, tags, is_favorite,
                send_count * 3 + recv_count, last_interacted,
                CASE WHEN {address_book} THEN 1 ELSE 0 END,
                COALESCE((SELECT GROUP_CONCAT(ca.account_id)
                          FROM contact_accounts ca
                          WHERE ca.contact_id = contacts.id), '')
         FROM contacts WHERE id = ?1"
        ),
        [id],
        record_from_row,
    )
    .optional()
    .map_err(Into::into)
}

/// Affinity score (send_count*3 + recv_count) per email, for the given
/// lowercase addresses. Used to personalize search ranking.
pub fn affinity_for(
    conn: &Connection,
    emails: &[String],
) -> Result<std::collections::HashMap<String, i64>> {
    let mut out = std::collections::HashMap::new();
    if emails.is_empty() {
        return Ok(out);
    }
    let placeholders = (1..=emails.len())
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT email, send_count * 3 + recv_count FROM contacts WHERE email IN ({placeholders})"
    );
    let mut stmt = conn.prepare(&sql)?;
    let params_ref: Vec<&dyn rusqlite::types::ToSql> = emails
        .iter()
        .map(|e| e as &dyn rusqlite::types::ToSql)
        .collect();
    let rows = stmt.query_map(params_ref.as_slice(), |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
    })?;
    for row in rows {
        let (email, score) = row?;
        out.insert(email, score);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testutil;
    use crate::models::Address;

    fn addr(email: &str, name: Option<&str>) -> Address {
        Address {
            name: name.map(str::to_string),
            email: email.into(),
        }
    }

    #[test]
    fn harvest_counts_and_autocomplete() {
        let c = testutil::conn();
        testutil::seed_account(&c);
        harvest(&c, 1, &addr("alice@acme.com", Some("Alice")), true, 100).unwrap();
        harvest(&c, 1, &addr("alice@acme.com", None), true, 200).unwrap();
        harvest(&c, 1, &addr("bob@other.org", Some("Bob")), false, 150).unwrap();

        let (send, recv): (i64, i64) = c
            .query_row(
                "SELECT send_count, recv_count FROM contacts WHERE email = 'alice@acme.com'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((send, recv), (2, 0));

        let hits = autocomplete(&c, "ali", None, true, 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].email, "alice@acme.com");
        // harvested name survives even when a later sighting had none
        assert_eq!(hits[0].name.as_deref(), Some("Alice"));

        assert!(autocomplete(&c, "zzz", None, true, 10).unwrap().is_empty());
    }

    #[test]
    fn successful_send_learns_unique_recipients_exactly_once() {
        let c = testutil::conn();
        testutil::seed_account(&c);
        c.execute(
            "INSERT INTO sender_identities (
               account_id,email,is_primary,is_provider_default,verification_status,last_synced_at
             ) VALUES (1,'me@test.dev',1,1,'accepted',0)",
            [],
        )
        .unwrap();
        let (_, message_id) = testutil::seed_message(&c, "me@test.dev", "Sent", false);
        c.execute(
            "UPDATE messages SET to_json=?2,cc_json=?3,bcc_json=?4 WHERE id=?1",
            params![
                message_id,
                serde_json::to_string(&[
                    addr("alice@example.test", Some("Alice")),
                    addr("BOB@example.test", Some("Bob")),
                ])
                .unwrap(),
                serde_json::to_string(&[addr("ALICE@example.test", None)]).unwrap(),
                serde_json::to_string(&[addr("me@test.dev", None)]).unwrap(),
            ],
        )
        .unwrap();

        assert_eq!(record_sent_recipients(&c, 1, message_id, 500).unwrap(), 2);
        assert_eq!(record_sent_recipients(&c, 1, message_id, 600).unwrap(), 0);
        let learned = c
            .prepare("SELECT email,send_count FROM contacts ORDER BY email")
            .unwrap()
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            learned,
            [
                ("alice@example.test".into(), 1),
                ("bob@example.test".into(), 1),
            ]
        );
        assert!(
            c.query_row(
                "SELECT contact_learning_recorded FROM messages WHERE id=?1",
                [message_id],
                |row| row.get::<_, bool>(0),
            )
            .unwrap()
        );

        let mut settings = super::super::settings::get(&c).unwrap();
        settings.collect_outgoing_contacts = false;
        super::super::settings::set(&c, &settings).unwrap();
        let (_, disabled_message_id) = testutil::seed_message(&c, "me@test.dev", "Disabled", false);
        c.execute(
            "UPDATE messages SET to_json=?2 WHERE id=?1",
            params![
                disabled_message_id,
                serde_json::to_string(&[addr("disabled@example.test", None)]).unwrap()
            ],
        )
        .unwrap();
        assert_eq!(
            record_sent_recipients(&c, 1, disabled_message_id, 700).unwrap(),
            0
        );
        assert_eq!(
            c.query_row(
                "SELECT COUNT(*) FROM contacts WHERE email='disabled@example.test'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
            0
        );
    }

    #[test]
    fn autocomplete_scopes_to_account() {
        let c = testutil::conn();
        testutil::seed_account(&c);
        c.execute(
            "INSERT INTO accounts (id, email, provider, auth_kind, username,
             imap_host, imap_port, smtp_host, smtp_port, created_at)
             VALUES (2,'two@test.dev','imap','password','two','h',993,'h',587,0)",
            [],
        )
        .unwrap();
        // alice belongs to account 1, carol to account 2.
        harvest(&c, 1, &addr("alice@acme.com", Some("Alice")), true, 100).unwrap();
        harvest(&c, 2, &addr("carol@acme.com", Some("Carol")), true, 100).unwrap();

        // Account-scoped: each account only sees its own contact.
        let a1 = autocomplete(&c, "a", Some(1), true, 10).unwrap();
        assert_eq!(
            a1.iter().map(|h| h.email.as_str()).collect::<Vec<_>>(),
            ["alice@acme.com"]
        );
        let a2 = autocomplete(&c, "a", Some(2), true, 10).unwrap();
        assert_eq!(
            a2.iter().map(|h| h.email.as_str()).collect::<Vec<_>>(),
            ["carol@acme.com"]
        );

        // Global (view-all): both surface.
        let all = autocomplete(&c, "a", None, true, 10).unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn managed_contact_round_trip_and_harvest_preserves_edited_name() {
        let c = testutil::conn();
        testutil::seed_account(&c);
        let created = save_record(
            &c,
            &ContactRecord {
                id: 0,
                name: "Ada Lovelace".into(),
                email: "ADA@EXAMPLE.COM".into(),
                phone: "+44 20 0000 0000".into(),
                company: "Analytical Engines".into(),
                job_title: "Programmer".into(),
                website: "https://example.com/ada".into(),
                birthday: "1815-12-10".into(),
                postal_address: "London".into(),
                notes: "Prefers written updates.".into(),
                tags: "vip, history".into(),
                is_favorite: true,
                interactions: 0,
                last_interacted: None,
                account_ids: Vec::new(),
                is_managed: true,
            },
            100,
        )
        .unwrap();
        assert!(created.id > 0);
        assert_eq!(created.email, "ada@example.com");
        assert!(created.is_favorite);
        assert_eq!(
            autocomplete(&c, "ada", Some(1), true, 20).unwrap()[0].email,
            "ada@example.com",
            "manually managed contacts remain available to account-scoped compose"
        );

        harvest(
            &c,
            1,
            &addr("ada@example.com", Some("Automated Header Name")),
            false,
            200,
        )
        .unwrap();
        let listed = list_records(&c, "analytical vip", 20).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "Ada Lovelace");
        assert_eq!(listed[0].interactions, 1);
        assert_eq!(listed[0].account_ids, vec![1]);
        assert!(listed[0].is_managed);

        delete_record(&c, created.id).unwrap();
        assert!(list_records(&c, "", 20).unwrap().is_empty());
    }

    #[test]
    fn suggestions_stay_separate_until_saved() {
        let c = testutil::conn();
        testutil::seed_account(&c);
        harvest(
            &c,
            1,
            &addr("grace@example.com", Some("Grace Hopper")),
            true,
            100,
        )
        .unwrap();

        let saved_page = list_record_page(&c, "", None, false, false, None, 25).unwrap();
        let suggestion_page = list_record_page(&c, "", None, false, true, None, 25).unwrap();
        assert!(saved_page.records.is_empty());
        assert_eq!(saved_page.total_count, 0);
        assert_eq!(suggestion_page.records.len(), 1);
        assert_eq!(suggestion_page.suggestion_count, 1);
        assert!(!suggestion_page.records[0].is_managed);
        assert!(
            autocomplete(&c, "grace", None, false, 10)
                .unwrap()
                .is_empty()
        );
        assert_eq!(autocomplete(&c, "grace", None, true, 10).unwrap().len(), 1);

        let promoted = save_record(&c, &suggestion_page.records[0], 200).unwrap();
        assert!(promoted.is_managed);
        assert_eq!(
            list_record_page(&c, "", None, false, false, None, 25)
                .unwrap()
                .records
                .len(),
            1
        );
        assert!(
            list_record_page(&c, "", None, false, true, None, 25)
                .unwrap()
                .records
                .is_empty()
        );
    }

    #[test]
    fn clearing_suggestions_preserves_saved_contacts_and_erases_affinity() {
        let c = testutil::conn();
        testutil::seed_account(&c);
        harvest(
            &c,
            1,
            &addr("saved@example.com", Some("Saved Person")),
            true,
            100,
        )
        .unwrap();
        harvest(
            &c,
            1,
            &addr("suggested@example.com", Some("Suggested Person")),
            false,
            150,
        )
        .unwrap();
        let saved = get_record(
            &c,
            c.query_row(
                "SELECT id FROM contacts WHERE email = 'saved@example.com'",
                [],
                |row| row.get(0),
            )
            .unwrap(),
        )
        .unwrap()
        .unwrap();
        save_record(&c, &saved, 200).unwrap();

        assert_eq!(clear_suggestions(&c, 300).unwrap(), 1);
        let remaining = list_records(&c, "", 20).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].email, "saved@example.com");
        assert_eq!(remaining[0].interactions, 0);
        assert_eq!(remaining[0].last_interacted, None);
        let account_affinity: (i64, i64, Option<i64>) = c
            .query_row(
                "SELECT send_count, recv_count, last_interacted
                 FROM contact_accounts WHERE contact_id = ?1 AND account_id = 1",
                [remaining[0].id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(account_affinity, (0, 0, None));
        assert_eq!(learning_boundaries(&c, 1, 999).unwrap(), (300, 300));
        advance_learning_boundaries(&c, false, true, 450).unwrap();
        assert_eq!(learning_boundaries(&c, 1, 999).unwrap(), (300, 450));
    }

    #[test]
    fn carddav_contacts_stay_saved_and_keep_remote_names_when_seen_in_mail() {
        let c = testutil::conn();
        testutil::seed_account(&c);
        let book_id = crate::db::repo::carddav::upsert_addressbook(
            &c,
            1,
            "https://dav.example.test/addressbook/",
            Some("Contacts"),
            false,
        )
        .unwrap();
        crate::db::repo::carddav::upsert_remote(
            &c,
            1,
            book_id,
            "/addressbook/grace.vcf",
            Some("v1"),
            "BEGIN:VCARD\r\nEND:VCARD\r\n",
            &ContactRecord {
                id: 0,
                name: "Grace Hopper".into(),
                email: "grace@example.com".into(),
                phone: String::new(),
                company: "Navy".into(),
                job_title: String::new(),
                website: String::new(),
                birthday: String::new(),
                postal_address: String::new(),
                notes: String::new(),
                tags: String::new(),
                is_favorite: false,
                interactions: 0,
                last_interacted: None,
                account_ids: vec![1],
                is_managed: false,
            },
        )
        .unwrap();

        let listed = list_records(&c, "", 20).unwrap();
        assert_eq!(listed.len(), 1);
        assert!(listed[0].is_managed);
        harvest(
            &c,
            1,
            &addr("grace@example.com", Some("Header Alias")),
            false,
            250,
        )
        .unwrap();
        let refreshed = get_record(&c, listed[0].id).unwrap().unwrap();
        assert_eq!(refreshed.name, "Grace Hopper");
        assert_eq!(refreshed.interactions, 1);
        assert!(
            list_record_page(&c, "", None, false, true, None, 25)
                .unwrap()
                .records
                .is_empty()
        );
        assert_eq!(clear_suggestions(&c, 300).unwrap(), 0);
        assert_eq!(
            get_record(&c, listed[0].id).unwrap().unwrap().interactions,
            0
        );
        crate::db::repo::carddav::disconnect(&c, 1).unwrap();
        let disconnected = get_record(&c, listed[0].id).unwrap().unwrap();
        assert!(disconnected.is_managed);
        assert_eq!(list_records(&c, "", 20).unwrap().len(), 1);
    }

    #[test]
    fn directory_pages_are_strict_non_overlapping_batches() {
        let c = testutil::conn();
        testutil::seed_account(&c);
        for index in 0..61 {
            harvest(
                &c,
                1,
                &addr(
                    &format!("person-{index:02}@example.com"),
                    Some(&format!("Person {index:02}")),
                ),
                false,
                index,
            )
            .unwrap();
        }

        let first = list_record_page(&c, "", None, false, true, None, 25).unwrap();
        harvest(
            &c,
            1,
            &addr("aardvark@example.com", Some("Aardvark")),
            false,
            100,
        )
        .unwrap();
        let second =
            list_record_page(&c, "", None, false, true, first.next_cursor.as_ref(), 25).unwrap();
        let third =
            list_record_page(&c, "", None, false, true, second.next_cursor.as_ref(), 25).unwrap();
        assert_eq!(first.records.len(), 25);
        assert_eq!(second.records.len(), 25);
        assert_eq!(third.records.len(), 11);
        assert!(first.next_cursor.is_some());
        assert!(second.next_cursor.is_some());
        assert!(third.next_cursor.is_none());
        let ids = first
            .records
            .iter()
            .chain(&second.records)
            .chain(&third.records)
            .map(|contact| contact.id)
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(ids.len(), 61);
        assert!(
            first
                .records
                .iter()
                .chain(&second.records)
                .chain(&third.records)
                .all(|contact| contact.email != "aardvark@example.com"),
            "the newly inserted head belongs before the cursor"
        );
        assert_eq!(first.total_count, 0);
        assert_eq!(first.suggestion_count, 61);
        assert_eq!(first.account_counts, vec![(1, 0)]);
    }
}
