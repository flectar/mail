use crate::error::{CoreError, Result};
use rusqlite::Connection;

/// How one schema step is applied.
enum Migration {
    /// Runs in a single transaction with foreign keys enforced.
    Plain(&'static str),
    /// Rebuilds a table that other tables reference, following SQLite's
    /// documented procedure for schema changes ALTER TABLE cannot make.
    /// Enforcement is disabled for the step so dropping the old table neither
    /// cascades into nor is blocked by the referencing rows; the step only
    /// commits when it introduced no foreign-key violation.
    TableRebuild(&'static str),
}

// File metadata search is additive and preserves existing mail profiles.
const MIGRATIONS: &[Migration] = &[
    Migration::Plain(include_str!("migrations/001_init.sql")),
    Migration::Plain(include_str!("migrations/002_attachment_files.sql")),
    Migration::Plain(include_str!("migrations/003_attachment_content.sql")),
    Migration::Plain(include_str!("migrations/004_carddav.sql")),
    Migration::Plain(include_str!("migrations/005_mailbox_count_indexes.sql")),
    Migration::Plain(include_str!("migrations/006_contact_recovery.sql")),
    Migration::Plain(include_str!("migrations/007_account_label_ownership.sql")),
    Migration::Plain(include_str!("migrations/008_sender_identities.sql")),
    Migration::Plain(include_str!(
        "migrations/009_contact_learning_clean_start.sql"
    )),
    Migration::Plain(include_str!("migrations/010_folder_hierarchy.sql")),
    Migration::Plain(include_str!("migrations/011_pending_body_page.sql")),
    Migration::Plain(include_str!("migrations/012_sender_identity_sources.sql")),
    Migration::Plain(include_str!("migrations/013_contact_entries.sql")),
    Migration::Plain(include_str!("migrations/014_folder_paths.sql")),
    Migration::TableRebuild(include_str!("migrations/015_account_protocols.sql")),
    Migration::Plain(include_str!("migrations/016_pop3.sql")),
    Migration::Plain(include_str!("migrations/017_jmap_attachment_listing.sql")),
    Migration::Plain(include_str!("migrations/018_jmap_full_sync_progress.sql")),
    Migration::Plain(include_str!("migrations/019_clean_snippets.sql")),
    Migration::Plain(include_str!("migrations/020_server_relink.sql")),
];
pub const LATEST_VERSION: i64 = MIGRATIONS.len() as i64;

/// Data conversions SQL cannot express, run after a migration's SQL in the
/// same transaction.
fn convert(conn: &Connection, version: i64) -> Result<()> {
    match version {
        13 => super::repo::contact_entries::migrate(conn),
        19 => clean_snippets(conn),
        _ => Ok(()),
    }
}

/// Rewrites stored snippets the way new ones are made. SQLite calls back into
/// the snippet builders, so rows stream through without being loaded at once
/// and only the ones that change are written.
///
/// A Gmail snippet holding `&` may still be HTML-escaped. Every path that
/// stores a body-derived snippet also stores the body, so a message with a
/// body takes its snippet from it again, and one without can only hold
/// Gmail's snippet, which is safe to decode.
fn clean_snippets(conn: &Connection) -> Result<()> {
    use rusqlite::functions::FunctionFlags;
    let flags = FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC;
    conn.create_scalar_function("make_snippet", 1, flags, |ctx| {
        Ok(crate::mime::make_snippet(&ctx.get::<String>(0)?))
    })?;
    conn.create_scalar_function("make_html_snippet", 1, flags, |ctx| {
        Ok(crate::mime::make_html_snippet(&ctx.get::<String>(0)?))
    })?;
    conn.create_scalar_function("make_body_snippet", 2, flags, |ctx| {
        Ok(crate::mime::make_body_snippet(
            ctx.get::<Option<String>>(0)?.as_deref(),
            ctx.get::<Option<String>>(1)?.as_deref(),
        ))
    })?;
    let converted = conn.execute_batch(
        "UPDATE messages SET snippet = COALESCE(
           NULLIF((SELECT make_body_snippet(b.text_body, b.html_body)
                   FROM message_bodies b WHERE b.message_id = messages.id), ''),
           make_html_snippet(snippet))
         WHERE snippet LIKE '%&%'
           AND account_id IN (SELECT id FROM accounts WHERE mail_protocol = 'gmail');
         UPDATE threads SET snippet = COALESCE(
           (SELECT snippet FROM messages WHERE thread_id = threads.id ORDER BY date DESC LIMIT 1), '')
         WHERE snippet LIKE '%&%'
           AND account_id IN (SELECT id FROM accounts WHERE mail_protocol = 'gmail');
         UPDATE messages SET snippet = make_snippet(snippet) WHERE snippet <> make_snippet(snippet);
         UPDATE threads SET snippet = make_snippet(snippet) WHERE snippet <> make_snippet(snippet);",
    );
    for (name, arguments) in [
        ("make_snippet", 1),
        ("make_html_snippet", 1),
        ("make_body_snippet", 2),
    ] {
        conn.remove_function(name, arguments)?;
    }
    Ok(converted?)
}

pub fn run(conn: &mut Connection) -> Result<()> {
    upgrade_to(conn, LATEST_VERSION)
}

fn upgrade_to(conn: &mut Connection, latest: i64) -> Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version > LATEST_VERSION {
        return Err(CoreError::Other(format!(
            "mail database schema {version} requires a newer application; open this profile with a compatible version. Existing data has not been changed."
        )));
    }

    for (index, migration) in MIGRATIONS
        .iter()
        .enumerate()
        .take(latest as usize)
        .skip(version as usize)
    {
        let target = (index + 1) as i64;
        apply(conn, migration, target)?;
        tracing::info!("applied mail db migration {target}");
    }
    Ok(())
}

fn apply(conn: &mut Connection, migration: &Migration, target: i64) -> Result<()> {
    match migration {
        Migration::Plain(sql) => {
            let tx = conn.transaction()?;
            tx.execute_batch(sql)?;
            convert(&tx, target)?;
            tx.pragma_update(None, "user_version", target)?;
            tx.commit()?;
            Ok(())
        }
        Migration::TableRebuild(sql) => rebuild(conn, sql, target),
    }
}

fn rebuild(conn: &mut Connection, sql: &str, target: i64) -> Result<()> {
    let enforced: bool = conn.pragma_query_value(None, "foreign_keys", |row| row.get(0))?;
    // The setting is ignored inside a transaction, so change it first.
    conn.pragma_update(None, "foreign_keys", false)?;
    let applied = (|| -> Result<()> {
        let tx = conn.transaction()?;
        // Compare instead of requiring zero so a violation left by an older
        // build elsewhere in the profile cannot block this upgrade.
        let before = foreign_key_violations(&tx)?;
        tx.execute_batch(sql)?;
        convert(&tx, target)?;
        let after = foreign_key_violations(&tx)?;
        if after > before {
            return Err(CoreError::Other(format!(
                "mail database migration {target} would orphan existing data; the profile has not been changed."
            )));
        }
        tx.pragma_update(None, "user_version", target)?;
        tx.commit()?;
        Ok(())
    })();
    let restored = conn.pragma_update(None, "foreign_keys", enforced);
    applied?;
    restored?;
    Ok(())
}

fn foreign_key_violations(conn: &Connection) -> Result<i64> {
    Ok(
        conn.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;
    use std::collections::BTreeSet;

    fn fresh() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        run(&mut conn).unwrap();
        conn
    }

    /// A profile left at `version` by an older build.
    fn staged(version: i64) -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        upgrade_to(&mut conn, version).unwrap();
        conn
    }

    fn application_tables(conn: &Connection) -> BTreeSet<String> {
        conn.prepare(
            "SELECT name FROM sqlite_master
             WHERE type = 'table'
               AND name NOT LIKE 'sqlite_%'
               AND name NOT LIKE 'messages_fts_%'
               AND name NOT LIKE 'attachment_files_fts_%'
               AND name NOT LIKE 'attachment_text_fts_%'
             ORDER BY name",
        )
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
    }

    /// One account with two folders, a thread, and a message, written in the
    /// shape of whichever schema version `conn` is at.
    fn seed_mail_graph(conn: &Connection) {
        let version: i64 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        if version >= 15 {
            conn.execute(
                "INSERT INTO accounts (
                   id, email, provider, auth_kind, mail_protocol, username, incoming_host,
                   incoming_port, smtp_host, smtp_port, created_at, settings_json
                 ) VALUES (1, 'me@test.dev', 'generic', 'password', 'imap', 'me', 'h', 993,
                           'h', 587, 0, '{}')",
                [],
            )
            .unwrap();
        } else {
            conn.execute(
                "INSERT INTO accounts (
                   id, email, provider, auth_kind, username, imap_host, imap_port,
                   smtp_host, smtp_port, created_at
                 ) VALUES (1, 'me@test.dev', 'imap', 'password', 'me', 'h', 993, 'h', 587, 0)",
                [],
            )
            .unwrap();
        }
        let folder_path = if version >= 14 { "path" } else { "imap_name" };
        conn.execute(
            &format!(
                "INSERT INTO folders (id, account_id, {folder_path}, role)
                 VALUES (1, 1, 'INBOX', 'inbox'), (2, 1, 'STARRED', NULL)"
            ),
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO threads (id, account_id, subject_norm) VALUES (1, 1, 'schema')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO messages (
               id, account_id, thread_id, folder_id, uid, subject, from_addr,
               to_json, cc_json, date, snippet
             ) VALUES (
               1, 1, 1, 1, 7, 'Schema contract', 'sender@test.dev', '[]', '[]', 1,
               'freshprofileterm'
             )",
            [],
        )
        .unwrap();
    }

    #[test]
    fn fresh_schema_has_the_canonical_mail_contract() {
        let conn = fresh();
        let expected = [
            "accounts",
            "ai_usage_events",
            "app_settings",
            "attachment_files_fts",
            "attachment_text_fts",
            "attachments",
            "contact_accounts",
            "contact_entries",
            "contact_values",
            "contact_photos",
            "carddav_addressbooks",
            "carddav_config",
            "carddav_objects",
            "contacts",
            "contact_learning_state",
            "cross_store_operations",
            "draft_attachments",
            "drafts_meta",
            "folders",
            "gmail_labels",
            "gmail_sync_state",
            "jmap_full_sync_stored",
            "jmap_sync_state",
            "labels",
            "message_bodies",
            "message_embeddings",
            "message_folders",
            "message_labels",
            "message_refs",
            "messages",
            "messages_fts",
            "notification_outbox",
            "pending_actions",
            "pop3_sync_state",
            "pop3_uidls",
            "route_cache",
            "sender_identities",
            "snippets",
            "snoozes",
            "split_rules",
            "sync_failures",
            "threads",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        assert_eq!(application_tables(&conn), expected);
        assert_eq!(
            conn.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            LATEST_VERSION
        );

        let index_columns = |name: &str| {
            conn.prepare("SELECT name FROM pragma_index_info(?1) ORDER BY seqno")
                .unwrap()
                .query_map([name], |row| row.get::<_, String>(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        };
        assert_eq!(
            index_columns("idx_messages_thread_folder_account"),
            ["thread_id", "folder_id", "account_id"]
        );
        assert_eq!(index_columns("idx_messages_body_fetching"), ["id"]);
        assert_eq!(
            index_columns("idx_messages_pending_body_page"),
            ["folder_id", "date", "id"]
        );
        assert_eq!(index_columns("idx_contacts_unfolded"), ["id"]);
        assert_eq!(
            index_columns("idx_threads_unread"),
            ["account_id", "starred_count"]
        );

        for forbidden in ["calendar_events", "calendars", "caldav_config"] {
            assert!(!application_tables(&conn).contains(forbidden));
        }
        let seeded: i64 = conn
            .query_row("SELECT COUNT(*) FROM labels WHERE is_auto = 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(seeded, 4);

        let violations = conn
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .query([])
            .unwrap()
            .next()
            .unwrap()
            .is_some();
        assert!(!violations);
        let integrity: String = conn
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap();
        assert_eq!(integrity, "ok");
    }

    #[test]
    fn pending_body_index_upgrade_includes_existing_rows_and_tracks_completion() {
        let mut conn = staged(10);
        seed_mail_graph(&conn);
        run(&mut conn).unwrap();
        let indexed: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM messages INDEXED BY idx_messages_pending_body_page
                 WHERE uid IS NOT NULL AND body_state = 'none'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(indexed, 1);
        conn.execute("UPDATE messages SET body_state = 'cached' WHERE id = 1", [])
            .unwrap();
        let indexed: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM messages INDEXED BY idx_messages_pending_body_page
                 WHERE uid IS NOT NULL AND body_state = 'none'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(indexed, 0);
    }

    #[test]
    fn jmap_attachment_listing_upgrade_keeps_what_header_sync_listed() {
        let mut conn = staged(16);
        seed_mail_graph(&conn);
        conn.execute_batch(
            "UPDATE messages SET jmap_id = 'email-1', jmap_blob_id = 'blob-1', has_attachments = 1
             WHERE id = 1;
             INSERT INTO messages (id, account_id, thread_id, folder_id, subject, date, has_attachments)
             VALUES (2, 1, 1, 1, 'Not on a JMAP server', 2, 1);",
        )
        .unwrap();
        run(&mut conn).unwrap();
        let markers = conn
            .prepare("SELECT id, jmap_attachments_blob_id FROM messages ORDER BY id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<(i64, Option<String>)>>>()
            .unwrap();
        assert_eq!(markers, [(1, Some("blob-1".to_owned())), (2, None)]);
        let queued = |conn: &Connection| {
            conn.query_row(
                "SELECT COUNT(*) FROM messages INDEXED BY idx_messages_jmap_attachments_pending
                 WHERE has_attachments = 1 AND body_state <> 'cached'
                   AND jmap_blob_id IS NOT NULL AND jmap_attachments_blob_id IS NOT jmap_blob_id",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap()
        };
        assert_eq!(queued(&conn), 0);
        conn.execute(
            "UPDATE messages SET jmap_blob_id = 'blob-2' WHERE id = 1",
            [],
        )
        .unwrap();
        assert_eq!(queued(&conn), 1);
        conn.execute("UPDATE messages SET body_state = 'cached' WHERE id = 1", [])
            .unwrap();
        assert_eq!(queued(&conn), 0);
    }

    #[test]
    fn snippet_upgrade_puts_stored_previews_on_one_line() {
        let mut conn = staged(18);
        seed_mail_graph(&conn);
        conn.execute(
            "UPDATE messages SET snippet = '\nDear Partner,\r\n\n  As reported' WHERE id = 1",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE threads SET snippet = 'View this post\non the web' WHERE id = 1",
            [],
        )
        .unwrap();
        run(&mut conn).unwrap();
        let snippets: (String, String) = conn
            .query_row(
                "SELECT m.snippet, t.snippet FROM messages m JOIN threads t ON t.id = m.thread_id",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            snippets,
            (
                "Dear Partner, As reported".into(),
                "View this post on the web".into()
            )
        );
    }

    #[test]
    fn snippet_upgrade_decodes_only_escaped_gmail_snippets() {
        let mut conn = staged(18);
        seed_mail_graph(&conn);
        conn.execute_batch(
            "UPDATE messages SET snippet = 'Fish &amp; chips' WHERE id = 1;
             INSERT INTO accounts (
               id, email, provider, auth_kind, mail_protocol, username, incoming_host,
               incoming_port, smtp_host, smtp_port, created_at, settings_json
             ) VALUES (2, 'me@gmail.test', 'gmail', 'oauth2', 'gmail', 'me', 'h', 993,
                       'h', 587, 0, '{}');
             INSERT INTO folders (id, account_id, path, role) VALUES (3, 2, 'INBOX', 'inbox');
             INSERT INTO threads (id, account_id, snippet)
             VALUES (2, 2, 'Tom & Jerry <tj@example.com>'), (3, 2, 'It&#39;s here');
             INSERT INTO messages (
               id, account_id, thread_id, folder_id, subject, from_addr, to_json, cc_json,
               date, snippet
             ) VALUES
               (2, 2, 2, 3, 'Hydrated', 'a@test.dev', '[]', '[]', 1,
                'Tom & Jerry <tj@example.com>'),
               (3, 2, 3, 3, 'Headers only', 'a@test.dev', '[]', '[]', 1, 'It&#39;s here');
             INSERT INTO message_bodies (message_id, text_body)
             VALUES (2, 'Tom & Jerry <tj@example.com>');",
        )
        .unwrap();
        run(&mut conn).unwrap();
        let snippets = |table: &str| -> Vec<String> {
            conn.prepare(&format!("SELECT snippet FROM {table} ORDER BY id"))
                .unwrap()
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap()
        };
        assert_eq!(
            snippets("messages"),
            [
                "Fish &amp; chips",
                "Tom & Jerry <tj@example.com>",
                "It's here"
            ]
        );
        assert_eq!(
            snippets("threads"),
            ["", "Tom & Jerry <tj@example.com>", "It's here"]
        );
    }

    #[test]
    fn jmap_full_sync_progress_upgrade_keeps_sync_states() {
        let mut conn = staged(17);
        seed_mail_graph(&conn);
        conn.execute(
            "INSERT INTO jmap_sync_state(account_id,mailbox_state,email_state)
             VALUES(1,'mailboxes','emails')",
            [],
        )
        .unwrap();
        run(&mut conn).unwrap();
        let states: (String, String, Option<String>) = conn
            .query_row(
                "SELECT mailbox_state,email_state,full_sync_state FROM jmap_sync_state",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(states, ("mailboxes".into(), "emails".into(), None));
        conn.execute(
            "INSERT INTO jmap_full_sync_stored(account_id,email_id) VALUES(1,'email-a')",
            [],
        )
        .unwrap();
        conn.execute("DELETE FROM accounts WHERE id=1", []).unwrap();
        let progress: i64 = conn
            .query_row("SELECT COUNT(*) FROM jmap_full_sync_stored", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(progress, 0);
    }

    #[test]
    fn relink_upgrade_flags_mail_an_earlier_switch_left() {
        let mut conn = staged(19);
        seed_mail_graph(&conn);
        conn.execute_batch(
            "INSERT INTO accounts (
               id, email, provider, auth_kind, mail_protocol, username, incoming_host,
               incoming_port, smtp_host, smtp_port, created_at, settings_json
             ) VALUES (2, 'jmap@test.dev', 'generic', 'password', 'jmap', 'me', 'h', 993,
                       'h', 587, 0, '{}');
             INSERT INTO messages (id, account_id, thread_id, folder_id, uid, jmap_id,
                                   subject, date, is_outgoing)
             VALUES (2, 1, 1, 1, NULL, NULL, 'Left by a switch from JMAP', 1, 0),
                    (3, 1, 1, 1, NULL, NULL, 'Sent from this device', 1, 1),
                    (4, 1, 1, 1, NULL, NULL, 'Draft', 1, 0),
                    (5, 2, NULL, NULL, 9, NULL, 'Left by a switch to JMAP', 1, 0),
                    (6, 2, NULL, NULL, NULL, 'email-6', 'On the JMAP server', 1, 0),
                    (7, 2, NULL, NULL, NULL, NULL, 'Sent from this device', 1, 1);
             INSERT INTO drafts_meta (message_id) VALUES (4);",
        )
        .unwrap();
        run(&mut conn).unwrap();
        let waiting = conn
            .prepare(
                "SELECT id FROM messages INDEXED BY idx_messages_relink_pending
                 WHERE account_id IN (1, 2) AND relink_pending = 1",
            )
            .unwrap()
            .query_map([], |row| row.get::<_, i64>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(waiting, [2, 5]);
        let missing: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM folders WHERE missing_on_server = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(missing, 0);
        assert!(
            conn.execute("UPDATE folders SET missing_on_server = 2 WHERE id = 2", [])
                .is_err()
        );
    }

    #[test]
    fn sender_identity_migration_preserves_account_ownership_and_seeds_primary() {
        let mut conn = staged(7);
        conn.execute(
            "INSERT INTO accounts (
               id,email,display_name,provider,auth_kind,username,imap_host,
               imap_port,smtp_host,smtp_port,created_at
             ) VALUES (1,'login@example.test','Login','gmail','oauth2','login',
                       '',993,'',465,123)",
            [],
        )
        .unwrap();

        run(&mut conn).unwrap();

        let row: (String, Option<String>, bool, bool, String) = conn
            .query_row(
                "SELECT email,display_name,is_primary,is_provider_default,
                        verification_status
                 FROM sender_identities WHERE account_id=1",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            row,
            (
                "login@example.test".into(),
                Some("Login".into()),
                true,
                true,
                "accepted".into(),
            )
        );
        let (source, delivery): (String, String) = conn
            .query_row(
                "SELECT source,delivery_status FROM sender_identities WHERE account_id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            (source.as_str(), delivery.as_str()),
            ("provider", "unknown")
        );
    }

    #[test]
    fn contact_learning_migration_keeps_only_outgoing_suggestions() {
        let mut conn = staged(8);
        conn.execute(
            "INSERT INTO accounts (
               id,email,provider,auth_kind,username,imap_host,imap_port,
               smtp_host,smtp_port,created_at
             ) VALUES (1,'me@example.test','imap','password','me','h',993,'h',587,0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO contacts
               (id,email,name,send_count,recv_count,last_interacted,is_favorite,is_managed)
             VALUES
               (1,'saved@example.test','Saved',4,2,100,0,1),
               (2,'legacy@example.test','Legacy',0,9,200,0,0),
               (3,'dav@example.test','CardDAV',1,1,300,0,0),
               (4,'favorite@example.test','Favorite',2,1,400,1,0),
               (5,'sent@example.test','Sent',3,7,500,0,0),
               (6,'repaired@example.test','Repaired',0,4,600,0,0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO contact_accounts
               (contact_id,account_id,send_count,recv_count,last_interacted)
             VALUES
               (1,1,4,2,100),(2,1,0,9,200),(3,1,1,1,300),
               (4,1,2,1,400),(5,1,3,7,500),(6,1,2,4,600)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO carddav_addressbooks (id,account_id,url)
             VALUES (1,1,'https://dav.example.test/contacts/')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO carddav_objects
               (addressbook_id,contact_id,href,remote_exists,deleted)
             VALUES (1,3,'/contacts/dav.vcf',1,0)",
            [],
        )
        .unwrap();

        let before = crate::models::now_ms();
        // Migration 13 moves the profile columns checked here into entries.
        upgrade_to(&mut conn, 12).unwrap();
        let after = crate::models::now_ms();

        let contacts = conn
            .prepare(
                "SELECT id,send_count,recv_count,last_interacted,is_managed
                 FROM contacts ORDER BY id",
            )
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, bool>(4)?,
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            contacts,
            [
                (1, 4, 2, Some(100), true),
                (3, 1, 1, Some(300), false),
                (4, 2, 1, Some(400), true),
                (5, 3, 0, Some(500), false),
                (6, 2, 0, Some(600), false),
            ]
        );
        let account_affinity = conn
            .prepare(
                "SELECT contact_id,send_count,recv_count,last_interacted
                 FROM contact_accounts ORDER BY contact_id",
            )
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            account_affinity,
            [
                (1, 4, 2, Some(100)),
                (3, 1, 1, Some(300)),
                (4, 2, 1, Some(400)),
                (5, 3, 0, Some(500)),
                (6, 2, 0, Some(600)),
            ]
        );
        let boundaries: (i64, i64) = conn
            .query_row(
                "SELECT outgoing_since,incoming_since
                 FROM contact_learning_state WHERE account_id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(boundaries.0, 0);
        assert!((before - 1_000..=after + 1_000).contains(&boundaries.1));

        conn.execute(
            "INSERT INTO accounts (
               id,email,provider,auth_kind,username,imap_host,imap_port,
               smtp_host,smtp_port,created_at
             ) VALUES (2,'new@example.test','imap','password','new','h',993,'h',587,0)",
            [],
        )
        .unwrap();
        let created: (i64, i64) = conn
            .query_row(
                "SELECT outgoing_since,incoming_since
                 FROM contact_learning_state WHERE account_id=2",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(created.0, 0);
        assert!((before - 1_000..=crate::models::now_ms() + 1_000).contains(&created.1));
    }

    #[test]
    fn folder_hierarchy_migration_preserves_existing_folders() {
        let mut conn = staged(9);
        conn.execute(
            "INSERT INTO accounts (
               id,email,provider,auth_kind,username,imap_host,imap_port,
               smtp_host,smtp_port,created_at
             ) VALUES (1,'me@example.test','imap','password','me','h',993,'h',587,0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO folders(id,account_id,imap_name,delimiter,role)
             VALUES(7,1,'Archive','/','archive')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO accounts (
               id,email,provider,auth_kind,mail_protocol,username,imap_host,imap_port,
               smtp_host,smtp_port,created_at
             ) VALUES (2,'jmap@example.test','imap','password','jmap','me','h',993,'h',587,0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO folders(id,account_id,imap_name,delimiter,role,jmap_id)
             VALUES(8,2,'Projects','/',NULL,'projects')",
            [],
        )
        .unwrap();

        run(&mut conn).unwrap();

        let hierarchy: (Option<i64>, bool) = conn
            .query_row(
                "SELECT parent_id,selectable FROM folders WHERE id=7",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(hierarchy, (None, true));
        let imap_rights: (i64, i64, i64, i64) = conn
            .query_row(
                "SELECT a.can_create_top_level_mailbox, f.can_create_children,
                        f.can_rename, f.can_delete
                 FROM accounts a JOIN folders f ON f.account_id=a.id
                 WHERE a.id=1 AND f.id=7",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(imap_rights, (1, 1, 1, 1));
        let jmap_rights: (i64, i64, i64, i64) = conn
            .query_row(
                "SELECT a.can_create_top_level_mailbox, f.can_create_children,
                        f.can_rename, f.can_delete
                 FROM accounts a JOIN folders f ON f.account_id=a.id
                 WHERE a.id=2 AND f.id=8",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(jmap_rights, (0, 0, 0, 0));
        assert_eq!(
            conn.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            LATEST_VERSION
        );
    }

    #[test]
    fn fresh_schema_preserves_fts_contact_and_gmail_constraints() {
        let conn = fresh();
        seed_mail_graph(&conn);
        conn.execute(
            "INSERT INTO messages_fts (rowid, subject, from_text, to_text, body)
             VALUES (1, 'Schema contract', 'sender@test.dev', '', 'freshprofileterm')",
            [],
        )
        .unwrap();
        let matches = |term: &str| {
            conn.query_row(
                "SELECT COUNT(*) FROM messages_fts WHERE messages_fts MATCH ?1",
                params![term],
                |row| row.get::<_, i64>(0),
            )
            .unwrap()
        };
        assert_eq!(matches("freshprofileterm"), 1);
        conn.execute("DELETE FROM messages_fts WHERE rowid = 1", [])
            .unwrap();
        assert_eq!(matches("freshprofileterm"), 0);

        conn.execute(
            "INSERT INTO contacts (email) VALUES ('Person@Example.com')",
            [],
        )
        .unwrap();
        assert!(
            conn.execute(
                "INSERT INTO contacts (email) VALUES ('person@example.com')",
                []
            )
            .is_err()
        );

        conn.execute(
            "INSERT INTO message_folders (message_id, folder_id) VALUES (1, 1), (1, 2)",
            [],
        )
        .unwrap();
        assert!(
            conn.execute(
                "INSERT INTO message_folders (message_id, folder_id) VALUES (1, 2)",
                [],
            )
            .is_err()
        );
    }

    #[test]
    fn upgrade_splits_merged_provider_labels_by_account_without_losing_membership() {
        let mut conn = staged(6);
        seed_mail_graph(&conn);
        conn.execute(
            "INSERT INTO accounts (
               id, email, provider, auth_kind, username, imap_host, imap_port,
               smtp_host, smtp_port, created_at
             ) VALUES (2, 'other@test.dev', 'gmail', 'oauth2', 'other', '', 993, '', 587, 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE accounts SET provider = 'gmail', auth_kind = 'oauth2' WHERE id = 1",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO folders (id, account_id, imap_name) VALUES (3, 2, 'Travel')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO threads (id, account_id, subject_norm) VALUES (2, 2, 'travel')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO messages (
               id, account_id, thread_id, folder_id, uid, subject, from_addr,
               to_json, cc_json, date, snippet
             ) VALUES (2, 2, 2, 3, 8, 'Trip', 'sender@test.dev', '[]', '[]', 2, '')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO labels (id, name, color, keyword, position)
             VALUES (20, 'Travel', '#123456', 'Travel', 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO message_labels (message_id, label_id) VALUES (1, 20), (2, 20)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO gmail_labels (
               account_id, provider_id, name, kind, folder_id, local_label_id,
               background_color
             ) VALUES
               (1, 'Label_A', 'Travel', 'user', NULL, 20, '#111111'),
               (2, 'Label_B', 'Travel', 'user', 3, 20, '#222222')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO pending_actions (
               account_id, kind, message_id, thread_id, payload, created_at
             ) VALUES (2, 'add_label', 2, 2, '{\"labelId\":20}', 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO split_rules (id, name, query_json)
             VALUES (9, 'Travel mail', '{\"labels\":[20,999]}')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO app_settings (key, value) VALUES (
               'settings',
               '{\"aiAutomationRules\":[
                  {\"id\":\"travel\",\"enabled\":true,\"actions\":[
                    {\"kind\":\"add_label\",\"value\":\"20\"}
                  ]},
                  {\"id\":\"read\",\"enabled\":true,\"actions\":[
                    {\"kind\":\"mark_read\",\"value\":\"\"}
                  ]}
                ]}'
             )",
            [],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO accounts (
               id, email, provider, auth_kind, username, imap_host, imap_port,
               smtp_host, smtp_port, created_at
             ) VALUES (3, 'unlinked@test.dev', 'gmail', 'oauth2', 'unlinked', '', 993, '', 587, 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO pending_actions (
               account_id, kind, message_id, thread_id, payload, created_at
             ) VALUES (3, 'add_label', NULL, NULL, '{\"labelId\":20}', 2)",
            [],
        )
        .unwrap();

        run(&mut conn).unwrap();

        let bindings = conn
            .prepare(
                "SELECT gl.account_id, gl.local_label_id, l.owner_account_id, l.color
                   FROM gmail_labels gl JOIN labels l ON l.id = gl.local_label_id
                  WHERE gl.provider_id IN ('Label_A', 'Label_B')
                  ORDER BY gl.account_id",
            )
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(bindings.len(), 2);
        assert_ne!(bindings[0].1, bindings[1].1);
        assert_eq!(bindings[0].0, bindings[0].2);
        assert_eq!(bindings[1].0, bindings[1].2);
        assert_eq!(bindings[0].3, "#111111");
        assert_eq!(bindings[1].3, "#222222");

        let memberships = conn
            .prepare(
                "SELECT m.account_id, ml.label_id
                   FROM message_labels ml JOIN messages m ON m.id = ml.message_id
                  ORDER BY m.account_id",
            )
            .unwrap()
            .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(memberships, vec![(1, bindings[0].1), (2, bindings[1].1)]);
        let queued_label_id: i64 = conn
            .query_row(
                "SELECT json_extract(payload, '$.labelId') FROM pending_actions WHERE account_id = 2",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(queued_label_id, bindings[1].1);
        let split_label_ids: String = conn
            .query_row(
                "SELECT json_extract(query_json, '$.labels')
                   FROM split_rules WHERE id = 9",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let split_label_ids: Vec<i64> = serde_json::from_str(&split_label_ids).unwrap();
        assert_eq!(split_label_ids, vec![bindings[0].1, bindings[1].1, 999]);
        let automation_states: String = conn
            .query_row(
                "SELECT json_extract(value, '$.aiAutomationRules')
                   FROM app_settings WHERE key = 'settings'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let automation_states: serde_json::Value =
            serde_json::from_str(&automation_states).unwrap();
        assert_eq!(automation_states[0]["enabled"], false);
        assert_eq!(automation_states[1]["enabled"], true);
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM pending_actions WHERE account_id = 3",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM message_labels", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            2
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
    fn memory_upgrade_repairs_legacy_contacts_in_multiple_batches() {
        let mut conn = staged(3);
        seed_mail_graph(&conn);
        for id in 1..=600 {
            conn.execute(
                "INSERT INTO contacts(id, name, email, send_count)
                 VALUES(?1, 'Café', ?2, 1)",
                params![id, format!("person-{id}@example.com")],
            )
            .unwrap();
        }
        run(&mut conn).unwrap();
        crate::db::repo::contacts::backfill_folded(&conn).unwrap();
        let repaired: i64 = conn
            .query_row(
                "SELECT count(*) FROM contacts WHERE folded LIKE 'cafe person-%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(repaired, 600);
        let changes = conn.total_changes();
        run(&mut conn).unwrap();
        crate::db::repo::contacts::backfill_folded(&conn).unwrap();
        assert_eq!(conn.total_changes(), changes);
        assert_eq!(
            conn.query_row("SELECT count(*) FROM messages", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
    }

    #[test]
    fn attachment_upgrade_preserves_mail_and_reconciles_only_once() {
        let mut conn = staged(1);
        seed_mail_graph(&conn);
        conn.execute_batch(
            "INSERT INTO attachments(id,message_id,part_id,filename,mime_type,size,file_path)
             VALUES(42,1,'2','Café-budget.txt','text/plain',17,'/private/cached-copy');
             INSERT INTO jmap_sync_state(account_id,mailbox_state,email_state,identity_state)
             VALUES(1,'mailboxes-before','emails-before','identities-before');",
        )
        .unwrap();

        run(&mut conn).unwrap();
        let attachment: (i64, String, String, Option<String>) = conn
            .query_row(
                "SELECT message_id,part_id,file_path,jmap_blob_id FROM attachments WHERE id=42",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            attachment,
            (1, "2".into(), "/private/cached-copy".into(), None)
        );
        let metadata_matches = |term: &str| {
            conn.query_row(
                "SELECT count(*) FROM attachment_files_fts WHERE attachment_files_fts MATCH ?1",
                [term],
                |r| r.get::<_, i64>(0),
            )
            .unwrap()
        };
        assert_eq!(metadata_matches("cafe"), 1);
        assert_eq!(metadata_matches("schema"), 1);
        let state: (String, Option<String>, String) = conn.query_row(
            "SELECT mailbox_state,email_state,identity_state FROM jmap_sync_state WHERE account_id=1",
            [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
        ).unwrap();
        assert_eq!(
            state,
            ("mailboxes-before".into(), None, "identities-before".into())
        );
        conn.execute_batch(
            "UPDATE jmap_sync_state SET email_state='emails-reconciled' WHERE account_id=1;
             UPDATE attachments SET jmap_blob_id='blob-1' WHERE id=42;
             INSERT INTO attachment_text_fts(rowid,content) VALUES(42,'indexed-content');",
        )
        .unwrap();
        // Opening an already-upgraded profile neither resets its cursor nor
        // rebuilds/drops downloaded attachment identities or their text index.
        run(&mut conn).unwrap();
        assert_eq!(
            conn.query_row(
                "SELECT email_state FROM jmap_sync_state WHERE account_id=1",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "emails-reconciled"
        );
        assert_eq!(
            conn.query_row("SELECT count(*) FROM attachment_text_fts", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        conn.execute(
            "UPDATE attachments SET jmap_blob_id='blob-2' WHERE id=42",
            [],
        )
        .unwrap();
        assert_eq!(
            conn.query_row("SELECT count(*) FROM attachment_text_fts", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        conn.execute("DELETE FROM messages WHERE id=1", []).unwrap();
        assert_eq!(
            conn.query_row("SELECT count(*) FROM attachment_files_fts", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            conn.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
    }

    /// Row counts of every table with a foreign key to `accounts`.
    fn account_dependents(conn: &Connection) -> Vec<(String, i64)> {
        let tables = conn
            .prepare(
                "SELECT DISTINCT m.name FROM sqlite_master m
                 JOIN pragma_foreign_key_list(m.name) f
                 WHERE m.type = 'table' AND f.\"table\" = 'accounts'
                 ORDER BY m.name",
            )
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        tables
            .into_iter()
            .map(|table| {
                let count = conn
                    .query_row(&format!("SELECT COUNT(*) FROM \"{table}\""), [], |row| {
                        row.get(0)
                    })
                    .unwrap();
                (table, count)
            })
            .collect()
    }

    #[test]
    fn account_protocol_rebuild_keeps_every_referencing_row() {
        let mut conn = staged(13);
        seed_mail_graph(&conn);
        conn.execute_batch(
            r#"
            UPDATE accounts SET settings_json = '{"mailHistory":"oneYear"}' WHERE id = 1;
            INSERT INTO accounts (
              id, email, provider, auth_kind, mail_protocol, username, jmap_url,
              imap_host, imap_port, smtp_host, smtp_port, created_at, settings_json,
              sort_order, avatar_url, can_create_top_level_mailbox
            ) VALUES
              (2, 'legacy@example.test', 'imap', 'password', 'imap', 'legacy', '',
               'mail.example.test', 143, 'mail.example.test', 465, 2,
               '{"defaultSenderEmail":"alias@example.test","connection":{"imapSecurity":"auto","smtpSecurity":"auto","trustedCertificatePem":"PEM"}}',
               4, NULL, 1),
              (3, 'jmap@example.test', 'imap', 'password', 'jmap', 'jmap',
               'https://jmap.example.test', '', 0, '', 0, 3,
               '{"connection":{"imapSecurity":"starttls","smtpSecurity":"tls"}}', 3, NULL, 0),
              (4, 'person@gmail.com', 'gmail', 'oauth2', 'imap', 'person@gmail.com', '',
               'imap.gmail.com', 993, 'smtp.gmail.com', 465, 4, '{}', 2,
               'https://avatar.example.test/a.png', 1),
              (5, 'person@outlook.com', 'microsoft', 'oauth2', 'imap', 'person@outlook.com',
               '', 'outlook.office365.com', 993, 'smtp.office365.com', 587, 5,
               '{"connection":{"imapSecurity":"auto","smtpSecurity":"auto"}}', 1, NULL, 1);
            INSERT INTO folders (id, account_id, imap_name, role, jmap_id)
              VALUES (3, 3, 'Inbox', 'inbox', 'mailbox-a'), (4, 4, 'INBOX', 'inbox', NULL);
            INSERT INTO sender_identities (
              account_id, email, is_primary, is_provider_default, verification_status,
              last_synced_at
            ) VALUES (5, 'person@outlook.com', 1, 1, 'accepted', 0);
            INSERT INTO pending_actions (account_id, kind, payload, created_at)
              VALUES (4, 'star', '{}', 1);
            "#,
        )
        .unwrap();
        let dependents = account_dependents(&conn);
        assert!(dependents.iter().any(|(table, _)| table == "messages"));
        assert!(dependents.iter().all(|(table, _)| table != "accounts"));

        // Through the rebuild only: later migrations add tables of their own.
        for (index, migration) in MIGRATIONS.iter().enumerate().take(15).skip(13) {
            apply(&mut conn, migration, (index + 1) as i64).unwrap();
        }

        assert_eq!(account_dependents(&conn), dependents);
        let accounts = conn
            .prepare(
                "SELECT id, provider, mail_protocol, incoming_host, incoming_port,
                        settings_json, sort_order, avatar_url, can_create_top_level_mailbox
                 FROM accounts ORDER BY id",
            )
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    serde_json::from_str::<serde_json::Value>(&row.get::<_, String>(5)?).unwrap(),
                    row.get::<_, i64>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, bool>(8)?,
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        let connection = |incoming: &str, smtp: &str, pem: &str| {
            serde_json::json!({
                "incomingSecurity": incoming,
                "smtpSecurity": smtp,
                "trustedCertificatePem": pem,
            })
        };
        assert_eq!(
            accounts,
            [
                (
                    1,
                    "generic".into(),
                    "imap".into(),
                    "h".into(),
                    993,
                    serde_json::json!({
                        "mailHistory": "oneYear",
                        "connection": connection("tls", "starttls", ""),
                    }),
                    0,
                    None,
                    true,
                ),
                (
                    2,
                    "generic".into(),
                    "imap".into(),
                    "mail.example.test".into(),
                    143,
                    serde_json::json!({
                        "defaultSenderEmail": "alias@example.test",
                        "connection": connection("starttls", "tls", "PEM"),
                    }),
                    4,
                    None,
                    true,
                ),
                (
                    3,
                    "generic".into(),
                    "jmap".into(),
                    "".into(),
                    0,
                    serde_json::json!({"connection": connection("starttls", "tls", "")}),
                    3,
                    None,
                    false,
                ),
                (
                    4,
                    "gmail".into(),
                    "gmail".into(),
                    "imap.gmail.com".into(),
                    993,
                    serde_json::json!({"connection": connection("tls", "tls", "")}),
                    2,
                    Some("https://avatar.example.test/a.png".into()),
                    true,
                ),
                (
                    5,
                    "microsoft".into(),
                    "imap".into(),
                    "outlook.office365.com".into(),
                    993,
                    serde_json::json!({"connection": connection("tls", "starttls", "")}),
                    1,
                    None,
                    true,
                ),
            ]
        );
        for (id, _, _, _, _, settings, _, _, _) in &accounts {
            let settings: crate::models::AccountSettings =
                serde_json::from_value(settings.clone()).unwrap();
            assert_eq!(
                settings.mail_history,
                if *id == 1 {
                    crate::models::MailHistory::OneYear
                } else {
                    crate::models::MailHistory::SixMonths
                }
            );
        }

        let enforced: bool = conn
            .pragma_query_value(None, "foreign_keys", |row| row.get(0))
            .unwrap();
        assert!(enforced);
        let paths = conn
            .prepare("SELECT path FROM folders ORDER BY id")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(paths, ["INBOX", "STARRED", "Inbox", "INBOX"]);
        assert!(
            conn.execute(
                "INSERT INTO folders (account_id, path) VALUES (1, 'INBOX')",
                [],
            )
            .is_err(),
            "the per-account path stays unique"
        );

        conn.execute(
            "INSERT INTO accounts (
               id, email, provider, auth_kind, mail_protocol, username, incoming_host,
               incoming_port, smtp_host, smtp_port, created_at, settings_json
             ) VALUES (6, 'new@example.test', 'generic', 'password', 'imap', 'new', 'h', 993,
                       'h', 587, 6, '{}')",
            [],
        )
        .unwrap();
        let learning: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM contact_learning_state WHERE account_id = 6",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(learning, 1, "the account trigger is recreated");
        conn.execute("DELETE FROM accounts WHERE id = 4", [])
            .unwrap();
        let orphans: i64 = conn
            .query_row(
                "SELECT (SELECT COUNT(*) FROM folders WHERE account_id = 4)
                      + (SELECT COUNT(*) FROM pending_actions WHERE account_id = 4)",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            orphans, 0,
            "deletes still cascade through the rebuilt table"
        );
        let integrity: String = conn
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap();
        assert_eq!(integrity, "ok");
        assert_eq!(foreign_key_violations(&conn).unwrap(), 0);
    }

    #[test]
    fn account_rows_must_pair_provider_and_protocol() {
        let conn = fresh();
        let insert = |id: i64, provider: &str, auth: &str, protocol: &str| {
            conn.execute(
                "INSERT INTO accounts (
                   id, email, provider, auth_kind, mail_protocol, username, incoming_host,
                   incoming_port, smtp_host, smtp_port, created_at, settings_json
                 ) VALUES (?1, ?1 || '@example.test', ?2, ?3, ?4, 'me', 'h', 993, 'h', 465,
                           0, '{}')",
                params![id, provider, auth, protocol],
            )
        };
        for (id, provider, auth, protocol) in [
            (1, "generic", "password", "imap"),
            (2, "generic", "password", "jmap"),
            (3, "generic", "password", "pop3"),
            (4, "gmail", "oauth2", "gmail"),
            (5, "microsoft", "oauth2", "imap"),
        ] {
            insert(id, provider, auth, protocol).unwrap();
        }
        for (id, provider, auth, protocol) in [
            (10, "imap", "password", "imap"),
            (11, "generic", "password", "gmail"),
            (12, "gmail", "oauth2", "imap"),
            (13, "microsoft", "oauth2", "jmap"),
            (14, "microsoft", "oauth2", "pop3"),
            (15, "generic", "password", "smtp"),
        ] {
            assert!(
                insert(id, provider, auth, protocol).is_err(),
                "{provider}/{protocol} must be rejected"
            );
        }
        assert!(
            conn.execute(
                "INSERT INTO accounts (
                   email, provider, auth_kind, mail_protocol, username, incoming_host,
                   incoming_port, smtp_host, smtp_port, created_at
                 ) VALUES ('none@example.test', 'generic', 'password', 'imap', 'me', 'h', 993,
                           'h', 465, 0)",
                [],
            )
            .is_err(),
            "settings are always written explicitly"
        );
    }

    #[test]
    fn a_rebuild_that_orphans_rows_changes_nothing() {
        let mut conn = fresh();
        seed_mail_graph(&conn);
        let version = LATEST_VERSION;
        let dependents = account_dependents(&conn);
        let error = rebuild(
            &mut conn,
            "CREATE TABLE accounts_copy (id INTEGER PRIMARY KEY);
             DROP VIEW directory_accounts;
             DROP TABLE accounts;
             ALTER TABLE accounts_copy RENAME TO accounts;",
            version + 1,
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("would orphan existing data"),
            "{error}"
        );
        assert!(rebuild(&mut conn, "SELECT no_such_function();", version + 1).is_err());

        assert_eq!(
            conn.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            version
        );
        let enforced: bool = conn
            .pragma_query_value(None, "foreign_keys", |row| row.get(0))
            .unwrap();
        assert!(enforced);
        let accounts: i64 = conn
            .query_row("SELECT COUNT(*) FROM accounts", [], |row| row.get(0))
            .unwrap();
        assert_eq!(accounts, 1);
        assert_eq!(account_dependents(&conn), dependents);
    }

    #[test]
    fn newer_profiles_are_rejected_without_mutation() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "user_version", 28).unwrap();
        let error = run(&mut conn).unwrap_err().to_string();
        assert!(error.contains("requires a newer application"));
        assert_eq!(
            conn.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            28
        );
        assert!(application_tables(&conn).is_empty());
    }
    #[test]
    fn contact_upgrade_converts_every_saved_contact_and_pending_change() {
        let mut conn = staged(12);
        conn.execute_batch(
            "INSERT INTO accounts (id, email, provider, auth_kind, username, imap_host,
                                   imap_port, smtp_host, smtp_port, created_at)
               VALUES (1, 'me@test.dev', 'imap', 'password', 'me', 'h', 993, 'h', 587, 0);
             INSERT INTO folders (id, account_id, imap_name, role) VALUES (1, 1, 'INBOX', 'inbox');",
        )
        .unwrap();
        conn.execute_batch(
            "INSERT INTO contacts(id,email,name,phone,company,is_managed,is_favorite,send_count) VALUES
               (1,'shared@example.test','Unsynced name','','',1,1,7),
               (2,'solo@example.test','Solo edited','555','',1,0,0),
               (3,'local@example.test','Local person','','Acme',1,0,0),
               (4,'gone@example.test','Gone','','',1,0,0),
               (5,'history@example.test','History','','',0,0,2),
               (6,'linked@example.test','Saved before sync','','',1,1,3);
             INSERT INTO carddav_addressbooks(id,account_id,url) VALUES
               (1,1,'https://dav.example/one/'),(2,1,'https://dav.example/two/');",
        )
        .unwrap();
        let card = |uid: &str, extra: &str| {
            format!(
                "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:{uid}\r\nFN:Server {uid}\r\n{extra}END:VCARD\r\n"
            )
        };
        let objects = [
            // One unsent edit shared by two cards cannot be assigned to either.
            (
                1,
                Some(1),
                "/shared.vcf",
                card("one", "EMAIL:shared@example.test\r\n"),
                1,
                0,
            ),
            (
                2,
                Some(1),
                "/shared.vcf",
                card("two", "EMAIL:shared@example.test\r\n"),
                1,
                0,
            ),
            (
                1,
                Some(2),
                "/solo.vcf",
                card(
                    "solo",
                    "EMAIL:solo@example.test\r\nTEL;TYPE=CELL:111\r\nTEL;TYPE=WORK,PREF:222\r\nX-CUSTOM:kept\r\n",
                ),
                1,
                0,
            ),
            (
                1,
                Some(4),
                "/gone.vcf",
                card("gone", "EMAIL:gone@example.test\r\n"),
                1,
                1,
            ),
            // A contact saved before the card that matched it was synced.
            (
                2,
                Some(6),
                "/linked.vcf",
                card("linked", "EMAIL:linked@example.test\r\n"),
                0,
                0,
            ),
            // The old directory hid cards without an email address.
            (1, None, "/hidden.vcf", card("hidden", "TEL:123\r\n"), 0, 0),
            (
                1,
                None,
                "/broken.vcf",
                "BEGIN:VCARD\r\nEND:VCARD\r\n".into(),
                0,
                0,
            ),
        ];
        for (book, contact, href, vcard, dirty, deleted) in objects {
            conn.execute(
                "INSERT INTO carddav_objects(addressbook_id,contact_id,href,etag,vcard_raw,dirty,deleted,owns_contact)
                 VALUES (?1,?2,?3,'v1',?4,?5,?6,?7)",
                params![book, contact, href, vcard, dirty, deleted, href != "/linked.vcf"],
            )
            .unwrap();
        }
        run(&mut conn).unwrap();

        use crate::db::repo::{carddav, contacts};
        use crate::models::ContactSync;
        let entries = contacts::list_records(&conn, "", 20).unwrap();
        let names = entries.iter().map(|e| e.name.as_str()).collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "Server linked",
                "Unsynced name",
                "Unsynced name",
                "Local person",
                "Server hidden",
                "Solo edited"
            ]
        );
        // Linked to a card, so it is that card's entry and not also local.
        assert!(entries[0].is_favorite);
        assert_eq!(entries[0].details.sync, ContactSync::Synced);
        for shared in &entries[1..3] {
            assert!(shared.is_favorite);
            assert_eq!(shared.details.sync, ContactSync::Conflict);
        }
        assert_eq!(entries[3].company, "Acme");
        assert_eq!(entries[3].details.sync, ContactSync::Local);
        assert_eq!(entries[4].phone, "123");
        assert_eq!(entries[4].details.sync, ContactSync::Synced);
        let solo = &entries[5];
        assert_eq!(solo.phone, "222");
        assert_eq!(solo.details.sync, ContactSync::Pending);
        let pending = carddav::pending_uploads(&conn, 1).unwrap();
        assert_eq!(pending.len(), 2);
        let solo_document = pending[0].document.as_deref().unwrap();
        assert!(solo_document.contains("UID:solo\r\nFN:Solo edited\r\n"));
        // The old version showed the first TEL, so the edit replaces that one.
        assert!(
            solo_document
                .contains("TEL;TYPE=CELL:555\r\nTEL;TYPE=WORK,PREF:222\r\nX-CUSTOM:kept\r\n")
        );
        assert!(pending[1].deleted);
        assert!(
            conn.query_row(
                "SELECT last_error IS NOT NULL FROM carddav_objects WHERE href='/broken.vcf'",
                [],
                |r| r.get::<_, bool>(0)
            )
            .unwrap()
        );

        // Mail history keeps its counts; profile-only rows and columns are gone.
        let history = conn
            .prepare("SELECT email,send_count FROM contacts ORDER BY id")
            .unwrap()
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            history,
            [
                ("shared@example.test".into(), 7),
                ("history@example.test".into(), 2),
                ("linked@example.test".into(), 3)
            ]
        );
        assert!(
            !conn
                .prepare("SELECT name FROM pragma_table_info('contacts')")
                .unwrap()
                .query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .any(|name| name.unwrap() == "is_managed")
        );
        assert_eq!(entries[1].interactions, 21);
        let suggestions =
            contacts::list_record_page(&conn, "", None, false, true, None, 20).unwrap();
        assert_eq!(suggestions.records[0].email, "history@example.test");

        run(&mut conn).unwrap();
        assert_eq!(contacts::list_records(&conn, "", 20).unwrap().len(), 6);
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
}
