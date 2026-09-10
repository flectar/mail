use flectar_mail_core::{
    Core,
    config::Paths,
    db::repo,
    models::{Signature, SignatureDefaults},
};

#[tokio::test]
async fn signature_ownership_defaults_sanitization_and_persistence() {
    let temp = tempfile::tempdir().unwrap();
    let paths = Paths::for_tests(temp.path());
    let core = Core::start(paths.clone()).await.unwrap();
    core.db.write(|conn| {
        conn.execute_batch("INSERT INTO accounts(id,email,provider,auth_kind,username,imap_host,imap_port,smtp_host,smtp_port,created_at) VALUES (1,'alice@example.org','imap','password','alice','imap.example.org',993,'smtp.example.org',465,0),(2,'bob@example.org','imap','password','bob','imap.example.org',993,'smtp.example.org',465,0)")?;
        Ok(())
    }).await.unwrap();
    let saved = core
        .save_signature(Signature {
            id: String::new(),
            account_id: 1,
            name: " Work ".into(),
            html: "<b>Alice</b><script>alert(1)</script><img src='https://example.org/tracker'>"
                .into(),
        })
        .await
        .unwrap();
    assert_eq!(saved.name, "Work");
    assert!(!saved.html.contains("script"));
    assert!(!saved.html.contains("img"));
    assert!(saved.html.contains("<b>Alice</b>"));
    let defaults = SignatureDefaults {
        new_id: Some(saved.id.clone()),
        reply_id: Some(saved.id.clone()),
    };
    core.set_signature_defaults(1, defaults.clone())
        .await
        .unwrap();
    assert!(core.set_signature_defaults(2, defaults).await.is_err());
    assert!(
        core.save_signature(Signature {
            account_id: 2,
            ..saved.clone()
        })
        .await
        .is_err()
    );
    core.delete_signature(2, saved.id.clone()).await.unwrap();
    assert_eq!(core.get_settings().await.unwrap().signature_list.len(), 1);
    drop(core);
    let core = Core::start(paths).await.unwrap();
    let settings = core.get_settings().await.unwrap();
    assert_eq!(
        settings.signature_defaults["1"].new_id,
        Some(saved.id.clone())
    );
    assert_eq!(settings.signature_list[0].html, saved.html);
    core.delete_signature(1, saved.id).await.unwrap();
    let settings = core.get_settings().await.unwrap();
    assert!(settings.signature_list.is_empty());
    assert!(settings.signature_defaults["1"].new_id.is_none());
    assert!(settings.signature_defaults["1"].reply_id.is_none());
    let old = core
        .db
        .read(|conn| repo::accounts::get_config(conn, 1))
        .await
        .unwrap()
        .unwrap();
    assert!(!old.settings.security.enabled());
}

#[tokio::test]
async fn protected_drafts_stay_local_and_queued_policy_cannot_downgrade() {
    use flectar_mail_core::{
        mail_security::{MailSecurity, protect_draft},
        models::{Address, SaveDraftArgs},
    };
    let temp = tempfile::tempdir().unwrap();
    let core = Core::start(Paths::for_tests(temp.path())).await.unwrap();
    let policy = MailSecurity {
        require_encryption: true,
        signing_fingerprint: "INVALID-IMPORTED-FINGERPRINT".into(),
        ..Default::default()
    };
    let stored = policy.clone();
    core.db.write(move |conn| {
        conn.execute_batch("INSERT INTO accounts(id,email,provider,auth_kind,username,imap_host,imap_port,smtp_host,smtp_port,created_at) VALUES(1,'alice@example.org','gmail','oauth2','alice','imap.example.org',993,'smtp.example.org',465,0)")?;
        let mut config = repo::accounts::get_config(conn, 1)?.unwrap(); config.settings.security = stored;
        repo::accounts::set_settings(conn, 1, &config.settings)
    }).await.unwrap();
    let draft = core
        .save_draft(SaveDraftArgs {
            draft_id: None,
            account_id: 1,
            to: vec![Address {
                name: None,
                email: "bob@example.org".into(),
            }],
            cc: vec![],
            bcc: vec![],
            subject: "Private".into(),
            body_text: "Local only".into(),
            body_html: None,
            mode: "new".into(),
            in_reply_to_message_id: None,
            attachments: vec![],
        })
        .await
        .unwrap();
    let pending: i64 = core.db.read(move |conn| Ok(conn.query_row("SELECT COUNT(*) FROM pending_actions WHERE message_id=?1 AND kind='save_draft' AND state='pending'", [draft], |r| r.get(0))?)).await.unwrap();
    assert_eq!(pending, 0);
    // Pin protected intent then turn the account default off. Dispatch must
    // still reject the invalid imported fingerprint before invoking GnuPG,
    // instead of returning plaintext or touching the user's keyring.
    core.db
        .write(move |conn| {
            repo::actions::enqueue(
                conn,
                1,
                "send",
                Some(draft),
                None,
                &serde_json::json!({"draftId":draft,"mailSecurity":policy}),
                None,
            )?;
            let mut config = repo::accounts::get_config(conn, 1)?.unwrap();
            config.settings.security = Default::default();
            repo::accounts::set_settings(conn, 1, &config.settings)
        })
        .await
        .unwrap();
    let result = protect_draft(
        &core.db,
        1,
        draft,
        b"From: alice@example.org\r\nContent-Type: text/plain\r\n\r\nPrivate".to_vec(),
        vec![],
    )
    .await;
    assert!(result.is_err());
}
