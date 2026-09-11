use crate::error::{CoreError, Result};
use rusqlite::Connection;

// File metadata search is additive and preserves existing mail profiles.
const MIGRATIONS: &[&str] = &[
    include_str!("migrations/001_init.sql"),
    include_str!("migrations/002_attachment_files.sql"),
    include_str!("migrations/003_attachment_content.sql"),
];
pub const LATEST_VERSION: i64 = MIGRATIONS.len() as i64;

pub fn run(conn: &mut Connection) -> Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let latest = LATEST_VERSION;
    if version > latest {
        return Err(CoreError::Other(format!(
            "mail database schema {version} requires a newer application; open this profile with a compatible version. Existing data has not been changed."
        )));
    }

    for (index, sql) in MIGRATIONS.iter().enumerate().skip(version as usize) {
        let target = (index + 1) as i64;
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", target)?;
        tx.commit()?;
        tracing::info!("applied mail db migration {target}");
    }
    Ok(())
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

    fn seed_mail_graph(conn: &Connection) {
        conn.execute(
            "INSERT INTO accounts (
               id, email, provider, auth_kind, username, imap_host, imap_port,
               smtp_host, smtp_port, created_at
             ) VALUES (1, 'me@test.dev', 'imap', 'password', 'me', 'h', 993, 'h', 587, 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO folders (id, account_id, imap_name, role)
             VALUES (1, 1, 'INBOX', 'inbox'), (2, 1, 'STARRED', NULL)",
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
            "contacts",
            "cross_store_operations",
            "draft_attachments",
            "drafts_meta",
            "folders",
            "gmail_labels",
            "gmail_sync_state",
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
            "route_cache",
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
    fn attachment_upgrade_preserves_mail_and_reconciles_only_once() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        conn.execute_batch(MIGRATIONS[0]).unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
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
}
