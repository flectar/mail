//! Searches kept in the sidebar (#243).
use super::*;
use crate::accounts::credentials::DevelopmentFileCredentialStore;

async fn core(directory: &tempfile::TempDir) -> Core {
    let credentials = Arc::new(DevelopmentFileCredentialStore::new(
        directory.path().join("credentials.json"),
    ));
    Core::start_mail_ui_with_credentials(Paths::for_tests(directory.path()), credentials)
        .await
        .unwrap()
}

fn ids(settings: &Settings) -> Vec<i64> {
    settings
        .saved_searches
        .iter()
        .map(|saved| saved.id)
        .collect()
}

#[tokio::test]
async fn searches_are_kept_changed_ordered_and_taken_out() {
    let directory = tempfile::tempdir().unwrap();
    let core = core(&directory).await;

    let (_, invoices) = core
        .save_search(
            None,
            " Invoices ".into(),
            " invoice from:billing ".into(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        (invoices.name.as_str(), invoices.query.as_str()),
        ("Invoices", "invoice from:billing")
    );
    let (settings, unread) = core
        .save_search(None, "Unread".into(), "is:unread".into(), None)
        .await
        .unwrap();
    assert_eq!(ids(&settings), [invoices.id, unread.id]);

    // A name in use, nothing to search for, or a profile that isn't there.
    for (name, query, profile) in [
        ("invoices", "receipt", None),
        ("Receipts", "  ", None),
        ("Receipts", "receipt", Some("profile-gone".to_owned())),
    ] {
        assert!(
            core.save_search(None, name.into(), query.into(), profile)
                .await
                .is_err(),
            "{name} {query:?} was saved"
        );
    }

    let (_, renamed) = core
        .save_search(Some(invoices.id), "Bills".into(), "invoice".into(), None)
        .await
        .unwrap();
    assert_eq!(
        (renamed.id, renamed.name.as_str(), renamed.query.as_str()),
        (invoices.id, "Bills", "invoice")
    );

    let settings = core.move_saved_search(unread.id, 0).await.unwrap();
    assert_eq!(ids(&settings), [unread.id, invoices.id]);
    let settings = core.move_saved_search(unread.id, 9).await.unwrap();
    assert_eq!(ids(&settings), [invoices.id, unread.id]);

    let settings = core.delete_saved_search(invoices.id).await.unwrap();
    assert_eq!(ids(&settings), [unread.id]);
    assert!(core.delete_saved_search(invoices.id).await.is_err());
    assert_eq!(ids(&core.get_settings().await.unwrap()), [unread.id]);
}

#[tokio::test]
async fn a_saved_search_counts_its_unread_mail_where_it_looks() {
    let directory = tempfile::tempdir().unwrap();
    let core = core(&directory).await;
    core.db
        .write(|conn| {
            crate::db::testutil::seed_account(conn);
            conn.execute(
                "INSERT INTO accounts (id, email, provider, auth_kind, username,
                 incoming_host, incoming_port, smtp_host, smtp_port, created_at,
                 mail_protocol, settings_json)
                 VALUES (2, 'two@test.dev', 'generic', 'password', 'two', 'h', 993,
                         'h', 587, 0, 'imap', '{}')",
                [],
            )?;
            conn.execute(
                "INSERT INTO folders (id, account_id, path, role) VALUES (2, 2, 'INBOX', 'inbox')",
                [],
            )?;
            for subject in ["Invoice March", "Invoice April", "Lunch"] {
                let (_, message_id) =
                    crate::db::testutil::seed_message(conn, "billing@one.dev", subject, false);
                repo::search::index_message(conn, message_id)?;
            }
            // April's is read, and a second account has an unread invoice.
            conn.execute(
                "UPDATE messages SET is_read = 1 WHERE subject = 'Invoice April'",
                [],
            )?;
            let (_, message_id) =
                crate::db::testutil::seed_message(conn, "billing@two.dev", "Invoice May", false);
            conn.execute(
                "UPDATE messages SET account_id = 2, folder_id = 2 WHERE id = ?1",
                [message_id],
            )?;
            repo::search::index_message(conn, message_id)?;
            Ok(())
        })
        .await
        .unwrap();
    let (_, invoices) = core
        .save_search(None, "Invoices".into(), "invoice".into(), None)
        .await
        .unwrap();
    let (_, profile) = core
        .save_mail_profile(None, "Second".into(), "#0969DA".into())
        .await
        .unwrap();
    core.assign_account_profile(2, Some(profile.id.clone()))
        .await
        .unwrap();
    let (_, second) = core
        .save_search(
            None,
            "Second's invoices".into(),
            "invoice".into(),
            Some(profile.id.clone()),
        )
        .await
        .unwrap();

    // Both accounts' unread invoices, then only what the view shows; the
    // profile's search keeps to its account either way.
    assert_eq!(
        core.saved_search_counts(true, None).await.unwrap(),
        [(invoices.id, 2), (second.id, 1)]
    );
    assert_eq!(
        core.saved_search_counts(false, Some(vec![1]))
            .await
            .unwrap(),
        [(invoices.id, 1), (second.id, 1)]
    );

    // Without the profile, its search covers the accounts in view.
    let settings = core.delete_mail_profile(profile.id).await.unwrap();
    assert_eq!(settings.saved_searches[1].profile_id, None);
    assert_eq!(
        core.saved_search_counts(true, None).await.unwrap(),
        [(invoices.id, 2), (second.id, 2)]
    );
}
