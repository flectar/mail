use crate::error::{CoreError, Result};
use rusqlite::Connection;

// File metadata search is additive and preserves existing mail profiles.
const MIGRATIONS: &[&str] = &[
    include_str!("migrations/001_init.sql"),
    include_str!("migrations/002_attachment_files.sql"),
    include_str!("migrations/003_attachment_content.sql"),
    include_str!("migrations/004_carddav.sql"),
    include_str!("migrations/005_mailbox_count_indexes.sql"),
    include_str!("migrations/006_contact_recovery.sql"),
    include_str!("migrations/007_account_label_ownership.sql"),
    include_str!("migrations/008_sender_identities.sql"),
    include_str!("migrations/009_contact_learning_clean_start.sql"),
    include_str!("migrations/010_folder_hierarchy.sql"),
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
    fn sender_identity_migration_preserves_account_ownership_and_seeds_primary() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        for (index, sql) in MIGRATIONS.iter().take(7).enumerate() {
            conn.execute_batch(sql).unwrap();
            conn.pragma_update(None, "user_version", (index + 1) as i64)
                .unwrap();
        }
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
    }

    #[test]
    fn contact_learning_migration_keeps_only_outgoing_suggestions() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        for (index, sql) in MIGRATIONS.iter().take(8).enumerate() {
            conn.execute_batch(sql).unwrap();
            conn.pragma_update(None, "user_version", (index + 1) as i64)
                .unwrap();
        }
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
        run(&mut conn).unwrap();
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
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        for (index, sql) in MIGRATIONS.iter().take(9).enumerate() {
            conn.execute_batch(sql).unwrap();
            conn.pragma_update(None, "user_version", (index + 1) as i64)
                .unwrap();
        }
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
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        for sql in &MIGRATIONS[..6] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 6).unwrap();
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
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        for sql in &MIGRATIONS[..3] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 3).unwrap();
        seed_mail_graph(&conn);
        for id in 1..=600 {
            conn.execute(
                "INSERT INTO contacts(id, name, email, is_managed)
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
