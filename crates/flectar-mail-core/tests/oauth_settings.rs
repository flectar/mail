use flectar_mail_core::{Core, config::Paths, models::Provider, oauth::providers};

// Keep this sequence in one test: configured OAuth registrations are global.
#[tokio::test]
async fn oauth_keys_persist_clear_and_remain_unchanged_after_failed_save() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::for_tests(tmp.path());
    let core = Core::start(paths.clone()).await.unwrap();
    let defaults = providers::resolve_credentials(Provider::Gmail).ok();
    let mut settings = core.get_settings().await.unwrap();
    settings.google_client_id = "custom-google-id".into();
    settings.google_client_secret = "custom-google-secret".into();
    settings.ms_client_id = "custom-ms-id".into();
    core.set_settings(settings.clone()).await.unwrap();
    let saved = core
        .set_oauth_apps(
            Some((
                "  custom-google-id  ".into(),
                "  custom-google-secret  ".into(),
            )),
            Some("  custom-ms-id  ".into()),
        )
        .await
        .unwrap();
    assert_eq!(saved.google_client_id, "custom-google-id");
    assert_eq!(saved.ms_client_id, "custom-ms-id");
    assert_eq!(
        providers::resolve_credentials(Provider::Gmail).unwrap(),
        (
            "custom-google-id".into(),
            Some("custom-google-secret".into())
        )
    );
    assert_eq!(
        providers::resolve_credentials(Provider::Microsoft).unwrap(),
        ("custom-ms-id".into(), None)
    );

    drop(core);
    let core = Core::start(paths).await.unwrap();
    assert_eq!(
        core.get_settings().await.unwrap().google_client_id,
        "custom-google-id"
    );
    assert_eq!(
        providers::resolve_credentials(Provider::Gmail).unwrap().0,
        "custom-google-id"
    );

    // Force the actual SQLite write to fail, leaving reads available.
    core.db
        .write(|conn| {
            conn.execute_batch("PRAGMA query_only = ON")?;
            Ok(())
        })
        .await
        .unwrap();
    settings.google_client_id = "must-not-become-active".into();
    assert!(core.set_settings(settings.clone()).await.is_err());
    assert!(
        core.set_oauth_apps(
            Some(("must-not-become-active".into(), "wrong-secret".into())),
            Some("must-not-become-active".into()),
        )
        .await
        .is_err()
    );
    assert_eq!(
        providers::resolve_credentials(Provider::Microsoft)
            .unwrap()
            .0,
        "custom-ms-id"
    );
    assert_eq!(
        providers::resolve_credentials(Provider::Gmail).unwrap().0,
        "custom-google-id"
    );
    assert_eq!(
        core.get_settings().await.unwrap().google_client_id,
        "custom-google-id"
    );
    core.db
        .write(|conn| {
            conn.execute_batch("PRAGMA query_only = OFF")?;
            Ok(())
        })
        .await
        .unwrap();

    // A single-provider update keeps the other provider and unrelated settings.
    settings = core.get_settings().await.unwrap();
    settings.theme = "dark".into();
    core.set_settings(settings).await.unwrap();
    let saved = core
        .set_oauth_apps(Some(("  ".into(), "discard-me".into())), None)
        .await
        .unwrap();
    assert!(saved.google_client_secret.is_empty());
    assert_eq!(saved.theme, "dark");
    assert_eq!(saved.ms_client_id, "custom-ms-id");
    assert_eq!(
        providers::resolve_credentials(Provider::Gmail).ok(),
        defaults
    );
    // Clearing one provider must not disable another provider's custom keys.
    assert_eq!(
        providers::resolve_credentials(Provider::Microsoft)
            .unwrap()
            .0,
        "custom-ms-id"
    );
    // A busy writer must suspend the save, leaving the async executor free.
    let db = core.db.clone();
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let blocked_writer = tokio::spawn(async move {
        db.write(move |_| {
            let _ = entered_tx.send(());
            let _ = release_rx.recv();
            Ok(())
        })
        .await
        .unwrap();
    });
    entered_rx.await.unwrap();
    let mut save = Box::pin(core.set_oauth_apps(
        Some(("delayed-google".into(), "secret".into())),
        Some("delayed-microsoft".into()),
    ));
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(25), &mut save)
            .await
            .is_err()
    );
    release_tx.send(()).unwrap();
    let saved = save.await.unwrap();
    assert_eq!(saved.google_client_id, "delayed-google");
    assert_eq!(saved.ms_client_id, "delayed-microsoft");
    blocked_writer.await.unwrap();

    // Retry both providers after the write failure; both must be saved together.
    core.set_oauth_apps(
        Some(("retry-google".into(), "retry-secret".into())),
        Some("retry-microsoft".into()),
    )
    .await
    .unwrap();
    let saved = core.get_settings().await.unwrap();
    assert_eq!(saved.google_client_id, "retry-google");
    assert_eq!(saved.ms_client_id, "retry-microsoft");
    core.set_oauth_apps(Some((String::new(), String::new())), Some(String::new()))
        .await
        .unwrap();
}
