use flectar_mail_core::{Core, config::Paths, db::repo};

#[tokio::test]
async fn empty_trash_without_a_known_trash_folder_reports_unavailable() {
    let tmp = tempfile::tempdir().unwrap();
    let core = Core::start(Paths::for_tests(tmp.path())).await.unwrap();
    let error = core.empty_trash(None).await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("Trash folder is not available yet")
    );
}

#[tokio::test]
async fn empty_trash_queues_each_provider_mailbox_even_without_cached_messages() {
    let tmp = tempfile::tempdir().unwrap();
    let core = Core::start(Paths::for_tests(tmp.path())).await.unwrap();
    core.db
        .write(|conn| {
            conn.execute_batch(
                "INSERT INTO accounts (id,email,provider,auth_kind,username,
             imap_host,imap_port,smtp_host,smtp_port,created_at) VALUES
             (1,'a@example.com','imap','password','a','127.0.0.1',1,'127.0.0.1',1,0),
             (2,'b@example.com','imap','password','b','127.0.0.1',1,'127.0.0.1',1,0);
             INSERT INTO folders (id,account_id,imap_name,role) VALUES
             (11,1,'Trash','trash'),(22,2,'Trash','trash');",
            )?;
            Ok(())
        })
        .await
        .unwrap();

    let first = core.empty_trash(Some(1)).await.unwrap();
    assert_eq!(first.action_ids.len(), 1);
    let second = core.empty_trash(None).await.unwrap();
    assert_eq!(second.action_ids.len(), 1);
    let repeated = core.empty_trash(None).await.unwrap();
    assert!(repeated.action_ids.is_empty());

    let first_action = core
        .db
        .read(move |conn| repo::actions::get(conn, first.action_ids[0]))
        .await
        .unwrap()
        .unwrap();
    let second_action = core
        .db
        .read(move |conn| repo::actions::get(conn, second.action_ids[0]))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            first_action.account_id,
            first_action.payload["folderId"].as_i64()
        ),
        (1, Some(11))
    );
    assert_eq!(
        (
            second_action.account_id,
            second_action.payload["folderId"].as_i64()
        ),
        (2, Some(22))
    );
    assert_eq!(first_action.kind, "empty_trash");
    assert_eq!(second_action.kind, "empty_trash");
}
