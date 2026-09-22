//! Security regression test for the draft-attachment staging fix.
//!
//! `save_draft` accepts an attachment `file_path` from its caller. Reading that
//! exact path later at dispatch would let a compromised UI attach `~/.ssh/id_rsa` or
//! `/etc/passwd` and exfiltrate it. The fix copies every picked file into an
//! app-managed staging dir at save time and persists only the staged path;
//! dispatch additionally refuses to read anything outside that dir. This test
//! locks in the save-time control (the primary boundary) end-to-end.

use flectar_mail_core::Core;
use flectar_mail_core::config::Paths;
use flectar_mail_core::models::{DraftAttachmentIn, SaveDraftArgs, roles};

async fn stored_paths(core: &Core, draft_id: i64) -> Vec<String> {
    // Manual row loop so the test needn't name `rusqlite` (not a dev-dep); the
    // `?`s convert rusqlite errors into CoreError via the crate's From impl.
    core.db
        .read(move |conn| {
            let mut stmt =
                conn.prepare("SELECT file_path FROM draft_attachments WHERE draft_id = ?1")?;
            let mut rows = stmt.query([draft_id])?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                out.push(row.get::<_, String>(0)?);
            }
            Ok(out)
        })
        .await
        .unwrap()
}

fn draft_args(account_id: i64, atts: Vec<DraftAttachmentIn>) -> SaveDraftArgs {
    SaveDraftArgs {
        draft_id: None,
        account_id,
        from: None,
        to: vec![],
        cc: vec![],
        bcc: vec![],
        subject: "hello".into(),
        body_text: "body".into(),
        body_html: None,
        mode: "new".into(),
        in_reply_to_message_id: None,
        attachments: atts,
    }
}

#[tokio::test]
async fn save_draft_stages_external_file_into_app_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let paths = Paths::for_tests(&data_dir);

    // A "secret" file living OUTSIDE the app data dir - stands in for the
    // arbitrary local file a compromised caller might try to attach and send.
    let secret = tmp.path().join("secret.txt");
    std::fs::write(&secret, b"top secret bytes").unwrap();

    let core = Core::start(paths.clone()).await.unwrap();
    core.db
        .write(|conn| {
            conn.execute_batch(
                "INSERT INTO accounts
                   (id,email,provider,auth_kind,username,imap_host,imap_port,smtp_host,smtp_port,created_at)
                   VALUES (1,'me@example.com','imap','password','me','h',993,'h',465,0);
                 INSERT INTO folders (id,account_id,imap_name,role)
                   VALUES (1,1,'Drafts','drafts');",
            )?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(roles::DRAFTS, "drafts");

    let draft_id = core
        .save_draft(draft_args(
            1,
            vec![DraftAttachmentIn {
                file_path: secret.to_string_lossy().into_owned(),
                filename: "secret.txt".into(),
            }],
        ))
        .await
        .unwrap();

    let stored = stored_paths(&core, draft_id).await;
    assert_eq!(stored.len(), 1);
    let stored = stored[0].as_str();

    let staging_root = std::fs::canonicalize(paths.draft_attachments_dir()).unwrap();
    let stored_canon = std::fs::canonicalize(stored).unwrap();

    assert!(
        stored_canon.starts_with(&staging_root),
        "stored path {stored_canon:?} must live under staging root {staging_root:?}"
    );
    assert_ne!(stored_canon, std::fs::canonicalize(&secret).unwrap());
    assert_eq!(std::fs::read(&stored_canon).unwrap(), b"top secret bytes");
}

#[tokio::test]
async fn save_draft_neutralizes_traversal_filename_and_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let paths = Paths::for_tests(&data_dir);

    let src = tmp.path().join("payload.bin");
    std::fs::write(&src, b"xyz").unwrap();

    let core = Core::start(paths.clone()).await.unwrap();
    core.db
        .write(|conn| {
            conn.execute_batch(
                "INSERT INTO accounts
                   (id,email,provider,auth_kind,username,imap_host,imap_port,smtp_host,smtp_port,created_at)
                   VALUES (1,'me@example.com','imap','password','me','h',993,'h',465,0);
                 INSERT INTO folders (id,account_id,imap_name,role)
                   VALUES (1,1,'Drafts','drafts');",
            )?;
            Ok(())
        })
        .await
        .unwrap();

    // A path-traversal filename must not escape the staging subdir.
    let draft_id = core
        .save_draft(draft_args(
            1,
            vec![DraftAttachmentIn {
                file_path: src.to_string_lossy().into_owned(),
                filename: "../../../../etc/evil".into(),
            }],
        ))
        .await
        .unwrap();

    let staging_root = std::fs::canonicalize(paths.draft_attachments_dir()).unwrap();
    let staged = stored_paths(&core, draft_id).await[0].clone();
    let staged_canon = std::fs::canonicalize(&staged).unwrap();
    assert!(
        staged_canon.starts_with(&staging_root),
        "traversal filename escaped staging: {staged_canon:?}"
    );

    // Re-saving the draft, echoing back the already-staged path (as the reloaded
    // composer does), must keep it in place rather than fail or re-nest.
    let draft_id2 = core
        .save_draft(SaveDraftArgs {
            draft_id: Some(draft_id),
            ..draft_args(
                1,
                vec![DraftAttachmentIn {
                    file_path: staged.clone(),
                    filename: "evil".into(),
                }],
            )
        })
        .await
        .unwrap();
    assert_eq!(draft_id2, draft_id);
    let restaged = std::fs::canonicalize(&stored_paths(&core, draft_id).await[0]).unwrap();
    assert_eq!(
        restaged, staged_canon,
        "already-staged path should be reused"
    );
}
