use flectar_mail_core::Core;
use flectar_mail_core::config::Paths;
use flectar_mail_core::models::{
    ActionKind, Address, DraftAttachmentIn, PerformActionArgs, SaveDraftArgs, View,
};

async fn setup() -> (tempfile::TempDir, Core) {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::for_tests(tmp.path());
    let core = Core::start(paths).await.unwrap();
    core.db
        .write(|conn| {
            conn.execute_batch(
                "INSERT INTO accounts
                   (id,email,provider,auth_kind,username,imap_host,imap_port,smtp_host,smtp_port,created_at)
                 VALUES (1,'me@example.com','imap','password','me','127.0.0.1',1,'127.0.0.1',1,0);
                 INSERT INTO folders (id,account_id,imap_name,role) VALUES
                   (1,1,'Drafts','drafts'),
                   (2,1,'Archive','archive'),
                   (3,1,'Trash','trash');",
            )?;
            Ok(())
        })
        .await
        .unwrap();
    (tmp, core)
}

fn args(attachment: Option<DraftAttachmentIn>) -> SaveDraftArgs {
    SaveDraftArgs {
        draft_id: None,
        account_id: 1,
        from: None,
        to: vec![Address {
            name: Some("To".into()),
            email: "to@example.com".into(),
        }],
        cc: vec![Address {
            name: None,
            email: "cc@example.com".into(),
        }],
        bcc: vec![Address {
            name: None,
            email: "bcc@example.com".into(),
        }],
        subject: "Half-written".into(),
        body_text: "Still working".into(),
        body_html: Some("<p>Still <strong>working</strong></p>".into()),
        mode: "new".into(),
        in_reply_to_message_id: None,
        attachments: attachment.into_iter().collect(),
    }
}

#[tokio::test]
async fn saved_draft_can_be_reloaded_without_losing_editable_fields() {
    let (tmp, core) = setup().await;
    let source = tmp.path().join("notes.txt");
    std::fs::write(&source, "notes").unwrap();
    let draft_id = core
        .save_draft(args(Some(DraftAttachmentIn {
            file_path: source.to_string_lossy().into_owned(),
            filename: "notes.txt".into(),
        })))
        .await
        .unwrap();

    let loaded = core.get_draft(draft_id).await.unwrap();
    assert_eq!(loaded.draft_id, Some(draft_id));
    assert_eq!(loaded.to[0].email, "to@example.com");
    assert_eq!(loaded.cc[0].email, "cc@example.com");
    assert_eq!(loaded.bcc[0].email, "bcc@example.com");
    assert_eq!(loaded.body_text, "Still working");
    assert_eq!(
        loaded.body_html.as_deref(),
        Some("<p>Still <strong>working</strong></p>")
    );
    assert_eq!(loaded.attachments.len(), 1);
    assert_eq!(loaded.attachments[0].filename, "notes.txt");
    assert_ne!(loaded.attachments[0].file_path, source.to_string_lossy());
}

#[tokio::test]
async fn drafts_accept_only_verified_sender_identities_and_store_provider_metadata() {
    let (_tmp, core) = setup().await;
    // Seed the primary row just as account discovery does, then model one
    // accepted and one still-pending provider alias.
    core.list_sender_identities(1).await.unwrap();
    core.db
        .write(|conn| {
            conn.execute_batch(
                "INSERT INTO sender_identities
                   (account_id,email,display_name,is_primary,is_provider_default,
                    verification_status,last_synced_at)
                 VALUES
                   (1,'work@example.com','Work identity',0,0,'accepted',1),
                   (1,'pending@example.com','Pending identity',0,0,'pending',1);",
            )?;
            Ok(())
        })
        .await
        .unwrap();

    let mut accepted = args(None);
    accepted.from = Some(Address {
        // User input must not be able to replace the provider-authorized name.
        name: Some("Forged display name".into()),
        email: "WORK@example.com".into(),
    });
    let draft_id = core.save_draft(accepted).await.unwrap();
    let loaded = core.get_draft(draft_id).await.unwrap();
    assert_eq!(
        loaded.from,
        Some(Address {
            name: Some("Work identity".into()),
            email: "work@example.com".into(),
        })
    );

    let mut pending = args(None);
    pending.from = Some(Address {
        name: None,
        email: "pending@example.com".into(),
    });
    assert!(
        core.save_draft(pending)
            .await
            .unwrap_err()
            .to_string()
            .contains("not a verified sender identity")
    );
}

#[tokio::test]
async fn archive_and_trash_remove_local_drafts_from_the_drafts_view() {
    let (_tmp, core) = setup().await;
    let archived_id = core.save_draft(args(None)).await.unwrap();
    let thread_id = core
        .db
        .read(move |conn| {
            Ok(conn.query_row(
                "SELECT thread_id FROM messages WHERE id = ?1",
                [archived_id],
                |row| row.get::<_, i64>(0),
            )?)
        })
        .await
        .unwrap();

    core.perform_action(PerformActionArgs {
        kind: ActionKind::Archive,
        thread_ids: vec![thread_id],
        params: None,
    })
    .await
    .unwrap();
    assert!(
        core.list_threads(View::Drafts, None, None, None, None, (None, 10))
            .await
            .unwrap()
            .threads
            .is_empty()
    );
    assert_eq!(
        core.list_threads(View::Done, None, None, None, None, (None, 10))
            .await
            .unwrap()
            .threads
            .len(),
        1
    );

    let trashed_id = core.save_draft(args(None)).await.unwrap();
    let trashed_thread = core
        .db
        .read(move |conn| {
            Ok(conn.query_row(
                "SELECT thread_id FROM messages WHERE id = ?1",
                [trashed_id],
                |row| row.get::<_, i64>(0),
            )?)
        })
        .await
        .unwrap();
    core.perform_action(PerformActionArgs {
        kind: ActionKind::Trash,
        thread_ids: vec![trashed_thread],
        params: None,
    })
    .await
    .unwrap();
    assert!(
        core.list_threads(View::Drafts, None, None, None, None, (None, 10))
            .await
            .unwrap()
            .threads
            .is_empty()
    );
    assert_eq!(
        core.list_threads(View::Trash, None, None, None, None, (None, 10))
            .await
            .unwrap()
            .threads
            .len(),
        1
    );
}

#[tokio::test]
async fn deleting_a_remote_draft_queues_trash_instead_of_allowing_resurrection() {
    let (_tmp, core) = setup().await;
    let draft_id = core.save_draft(args(None)).await.unwrap();
    core.db
        .write(move |conn| {
            conn.execute("UPDATE messages SET uid = 42 WHERE id = ?1", [draft_id])?;
            Ok(())
        })
        .await
        .unwrap();

    core.delete_draft(draft_id).await.unwrap();
    let (role, action, source_guard): (String, String, bool) = core
        .db
        .read(move |conn| {
            let role = conn.query_row(
                "SELECT f.role FROM messages m JOIN folders f ON f.id = m.folder_id
                 WHERE m.id = ?1",
                [draft_id],
                |row| row.get(0),
            )?;
            let action = conn.query_row(
                "SELECT kind FROM pending_actions WHERE message_id = ?1 ORDER BY id DESC LIMIT 1",
                [draft_id],
                |row| row.get(0),
            )?;
            let source_guard =
                flectar_mail_core::db::repo::actions::has_active_move_from(conn, draft_id, 1)?;
            Ok((role, action, source_guard))
        })
        .await
        .unwrap();
    assert_eq!(role, "trash");
    assert_eq!(action, "trash");
    assert!(source_guard, "sync must not relink the old Drafts copy");
    assert!(
        core.list_threads(View::Drafts, None, None, None, None, (None, 10))
            .await
            .unwrap()
            .threads
            .is_empty()
    );
}
