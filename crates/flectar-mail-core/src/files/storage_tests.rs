use super::{store, *};
use crate::{
    Core,
    config::Paths,
    db::{Db, testutil},
};
use serde_json::json;

#[tokio::test]
async fn independent_store_migrates_legacy_settings_and_recovers_uncertain_jobs() {
    let root = tempfile::tempdir().unwrap();
    let core = Core::start_mail_ui(Paths::for_tests(root.path()))
        .await
        .unwrap();
    core.db
        .write(|c| {
            testutil::seed_account(c);
            c.execute(
                "INSERT INTO app_settings(key,value) VALUES('files:1',?1)",
                [json!({"endpoint":"https://files.example.test","webdav":false}).to_string()],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    core.recover_files().await.unwrap();
    assert_eq!(
        core.file_connection_settings(1).await.unwrap().endpoint,
        "https://files.example.test"
    );
    let space = store::space(
        &core.files_db,
        1,
        "personal".into(),
        "Personal".into(),
        "jmap",
    )
    .await
    .unwrap();
    let queued = store::begin(&core.files_db, 1, Some(space), "upload".into(), "{}".into())
        .await
        .unwrap();
    let running = store::begin(&core.files_db, 1, Some(space), "rename".into(), "{}".into())
        .await
        .unwrap();
    store::finish(&core.files_db, running, "running", None)
        .await
        .unwrap();
    core.recover_files().await.unwrap();
    let operations = store::operations(&core.files_db, 1).await.unwrap();
    assert_eq!(
        operations.iter().find(|o| o.id == queued).unwrap().state,
        "queued"
    );
    assert_eq!(
        operations.iter().find(|o| o.id == running).unwrap().state,
        "uncertain"
    );
    assert!(root.path().join("flectar-files.db").exists());
    let tables=core.files_db.read(|c|Ok(c.query_row("SELECT count(*) FROM sqlite_master WHERE name IN ('messages','attachments','calendar_events')",[],|r|r.get::<_,i64>(0))?)).await.unwrap();
    assert_eq!(tables, 0);
}

#[tokio::test]
async fn cached_files_are_scoped_and_complete_collection_replacement_removes_stale_rows() {
    let root = tempfile::tempdir().unwrap();
    let db = Db::open_files(&root.path().join("files.db")).unwrap();
    db.write(|c|{c.execute("INSERT INTO connections(account_id,settings_json,updated_at) VALUES(1,'{}',0),(2,'{}',0)",[])?;Ok(())}).await.unwrap();
    let one = store::space(&db, 1, "same-id".into(), "One".into(), "jmap")
        .await
        .unwrap();
    let two = store::space(&db, 2, "same-id".into(), "Two".into(), "jmap")
        .await
        .unwrap();
    let node = FileNode {
        id: "x".into(),
        name: "Résumé.pdf".into(),
        media_type: Some("application/pdf".into()),
        ..Default::default()
    };
    store::replace_collection(
        &db,
        one,
        None,
        vec![node],
        Some("s1".into()),
        Some("q1".into()),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        store::cached(&db, one, None, "resume".into())
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(
        store::cached(&db, two, None, "resume".into())
            .await
            .unwrap()
            .is_empty()
    );
    // A visible folder page must not advance the full-account sync cursor.
    assert!(
        db.read(move |c| Ok(c
            .query_row("SELECT state FROM spaces WHERE id=?1", [one], |r| r
                .get::<_, Option<String>>(0))?))
            .await
            .unwrap()
            .is_none()
    );
    store::replace_collection(
        &db,
        one,
        None,
        vec![],
        Some("s2".into()),
        Some("q2".into()),
        None,
    )
    .await
    .unwrap();
    assert!(
        store::cached(&db, one, None, "".into())
            .await
            .unwrap()
            .is_empty()
    );
    db.write(|c| {
        c.execute("DELETE FROM connections WHERE account_id=1", [])?;
        Ok(())
    })
    .await
    .unwrap();
    assert!(store::operations(&db, 1).await.unwrap().is_empty());
}

#[tokio::test]
async fn metadata_snapshot_contains_all_three_stores() {
    let root = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let core = Core::start_mail_ui(Paths::for_tests(root.path()))
        .await
        .unwrap();
    let manifest = core
        .create_database_snapshot(out.path().join("snapshot"))
        .await
        .unwrap();
    assert_eq!(manifest.version, 2);
    assert!(manifest.files.is_some());
    for name in ["mail.sqlite3", "calendar.sqlite3", "files.sqlite3"] {
        assert!(out.path().join("snapshot").join(name).exists());
    }
}

#[tokio::test]
async fn connection_changes_cancel_queued_writes_but_preserve_reconciliation_history() {
    let root = tempfile::tempdir().unwrap();
    let core = Core::start_mail_ui(Paths::for_tests(root.path()))
        .await
        .unwrap();
    core.db
        .write(|c| {
            testutil::seed_account(c);
            Ok(())
        })
        .await
        .unwrap();
    core.save_file_connection_settings(
        1,
        ConnectionSettings {
            endpoint: "https://one.test".into(),
            webdav: false,
        },
    )
    .await
    .unwrap();
    let space = store::space(
        &core.files_db,
        1,
        "personal".into(),
        "Personal".into(),
        "jmap",
    )
    .await
    .unwrap();
    let queued = store::begin(&core.files_db, 1, Some(space), "upload".into(), "{}".into())
        .await
        .unwrap();
    let uncertain = store::begin(&core.files_db, 1, Some(space), "copy".into(), "{}".into())
        .await
        .unwrap();
    store::finish(
        &core.files_db,
        uncertain,
        "uncertain",
        Some("Inspect destination".into()),
    )
    .await
    .unwrap();
    core.save_file_connection_settings(
        1,
        ConnectionSettings {
            endpoint: "https://two.test".into(),
            webdav: true,
        },
    )
    .await
    .unwrap();
    let jobs = store::operations(&core.files_db, 1).await.unwrap();
    assert_eq!(
        jobs.iter().find(|o| o.id == queued).unwrap().state,
        "cancelled"
    );
    assert_eq!(
        jobs.iter().find(|o| o.id == uncertain).unwrap().state,
        "uncertain"
    );
    assert!(
        store::selected_space(&core.files_db, 1)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn cache_eviction_preserves_pins_and_removes_unreferenced_versions() {
    let root = tempfile::tempdir().unwrap();
    let core = Core::start_mail_ui(Paths::for_tests(root.path()))
        .await
        .unwrap();
    core.db
        .write(|c| {
            testutil::seed_account(c);
            Ok(())
        })
        .await
        .unwrap();
    core.save_file_connection_settings(1, ConnectionSettings::default())
        .await
        .unwrap();
    let space = store::space(
        &core.files_db,
        1,
        "personal".into(),
        "Personal".into(),
        "jmap",
    )
    .await
    .unwrap();
    let dir = core.paths.files_cache_dir(1);
    tokio::fs::create_dir_all(&dir).await.unwrap();
    for key in ["a".repeat(64), "b".repeat(64), "c".repeat(64)] {
        tokio::fs::write(dir.join(key), b"data").await.unwrap();
    }
    core.files_db.write(move |c|{for (id,pinned) in [("a",true),("b",false)] {c.execute("INSERT INTO content_cache(space_id,remote_id,validator,relative_path,byte_size,accessed_at,pinned) VALUES(?1,?2,'v1',?3,4,0,?4)",rusqlite::params![space,id,id.repeat(64),pinned])?;}Ok(())}).await.unwrap();
    cache::trim(&core, 1, 4).await.unwrap();
    assert!(dir.join("a".repeat(64)).exists());
    assert!(!dir.join("b".repeat(64)).exists());
    assert!(!dir.join("c".repeat(64)).exists());
    assert!(cache::trim(&core, 1, 0).await.is_err());
    assert!(dir.join("a".repeat(64)).exists());
    cache::pin(&core, space, "a".into(), false).await.unwrap();
    cache::trim(&core, 1, 0).await.unwrap();
    assert!(!dir.join("a".repeat(64)).exists());
}

#[tokio::test]
async fn snapshots_include_portable_staged_uploads() {
    let root = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let core = Core::start_mail_ui(Paths::for_tests(root.path()))
        .await
        .unwrap();
    core.db
        .write(|c| {
            testutil::seed_account(c);
            Ok(())
        })
        .await
        .unwrap();
    core.save_file_connection_settings(1, ConnectionSettings::default())
        .await
        .unwrap();
    let id = store::begin(
        &core.files_db,
        1,
        None,
        "upload".into(),
        json!({"stagedName":"budget.txt"}).to_string(),
    )
    .await
    .unwrap();
    let dir = core.paths.files_staging_dir(1).join(id.to_string());
    tokio::fs::create_dir_all(&dir).await.unwrap();
    tokio::fs::write(dir.join("budget.txt"), b"durable bytes")
        .await
        .unwrap();
    let manifest = core
        .create_database_snapshot(out.path().join("snapshot"))
        .await
        .unwrap();
    assert!(manifest.file_transfer_payloads);
    assert_eq!(
        std::fs::read(
            out.path()
                .join("snapshot/file_transfers/1")
                .join(id.to_string())
                .join("budget.txt")
        )
        .unwrap(),
        b"durable bytes"
    );
}

#[tokio::test]
async fn attachment_metadata_browsing_does_not_wait_for_remote_tree_sync() {
    let root = tempfile::tempdir().unwrap();
    let core = Core::start_mail_ui(Paths::for_tests(root.path()))
        .await
        .unwrap();
    let _scan = core.file_work_lock.lock().await;
    let mut service = service::FilesService {
        attachments: true,
        ..Default::default()
    };
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        service.execute(&core, "load", "", "", -1, None),
    )
    .await
    .expect("Mail attachments must not wait for the storage scan")
    .unwrap();
}

#[tokio::test]
async fn sidebar_selects_account_by_identity_and_enters_attachment_scope() {
    let root = tempfile::tempdir().unwrap();
    let core = Core::start_mail_ui(Paths::for_tests(root.path()))
        .await
        .unwrap();
    core.db
        .write(|c| {
            testutil::seed_account(c);
            Ok(())
        })
        .await
        .unwrap();
    let _scan = core.file_work_lock.lock().await;
    let mut service = service::FilesService::default();
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        service.execute(&core, "account-id", "1", "attachments", -1, None),
    )
    .await
    .expect("Switching to mail attachments must not wait for file sync")
    .unwrap();
    assert!(service.attachments);
    assert_eq!(service.account_id(), Some(1));
    assert!(
        service
            .execute(&core, "account-id", "9999", "attachments", -1, None)
            .await
            .is_err()
    );
    assert_eq!(service.account_id(), Some(1));
    service
        .execute(&core, "account", "0", "attachments", -1, None)
        .await
        .unwrap();
    assert!(service.account_id().is_none());
}

#[tokio::test]
async fn folder_refresh_prunes_revoked_descendants_but_preserves_promoted_children() {
    let root = tempfile::tempdir().unwrap();
    let db = Db::open_files(&root.path().join("files.db")).unwrap();
    db.write(|c| {
        c.execute("INSERT INTO connections VALUES(1,'{}',NULL,0)", [])?;
        Ok(())
    })
    .await
    .unwrap();
    let space = store::space(&db, 1, "storage".into(), "Storage".into(), "jmap")
        .await
        .unwrap();
    let node = |id: &str, parent: Option<&str>| FileNode {
        id: id.into(),
        name: id.into(),
        parent_id: parent.map(str::to_owned),
        ..Default::default()
    };
    let nodes = vec![
        node("folder", None),
        node("private", Some("folder")),
        node("promoted", Some("folder")),
        node("child", Some("promoted")),
    ];
    db.write(move |c| {
        for node in nodes {
            store::upsert(c, space, &node)?;
        }
        c.execute(
            "INSERT INTO content_cache VALUES(?1,'private','v','cached',4,0,1)",
            [space],
        )?;
        c.execute(
            "INSERT INTO collections VALUES(?1,'folder',NULL,NULL,1,0)",
            [space],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    store::replace_collection(
        &db,
        space,
        None,
        vec![node("promoted", None)],
        None,
        None,
        None,
    )
    .await
    .unwrap();
    let found = store::cached(&db, space, None, "private".into())
        .await
        .unwrap();
    assert!(found.is_empty(), "revoked descendant remained searchable");
    let children = store::cached(&db, space, Some("promoted".into()), "".into())
        .await
        .unwrap();
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].id, "child");
    db.read(move |c| {
        assert_eq!(
            c.query_row("SELECT count(*) FROM content_cache", [], |r| r
                .get::<_, i64>(0))?,
            0
        );
        assert_eq!(
            c.query_row(
                "SELECT count(*) FROM collections WHERE parent_id='folder'",
                [],
                |r| r.get::<_, i64>(0)
            )?,
            0
        );
        Ok(())
    })
    .await
    .unwrap();
}
