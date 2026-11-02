//! End-to-end fixtures for scheduling and IMAP compatibility in one sync cycle.
use super::*;
use crate::db::repo::messages::NewMessage;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use std::sync::Mutex;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const CERT: &str = include_str!("../../tests/fixtures/tls/server.pem");
const KEY: &str = include_str!("../../tests/fixtures/tls/server-key.pem");

async fn read_line<S: tokio::io::AsyncRead + Unpin>(stream: &mut BufReader<S>) -> String {
    let mut line = String::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        stream.read_line(&mut line),
    )
    .await
    .expect("client command timed out")
    .expect("client command could not be read");
    line
}

fn tls_acceptor() -> tokio_rustls::TlsAcceptor {
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![CertificateDer::from_pem_slice(CERT.as_bytes()).unwrap()],
        PrivateKeyDer::from_pem_slice(KEY.as_bytes()).unwrap(),
    )
    .unwrap();
    tokio_rustls::TlsAcceptor::from(Arc::new(config))
}

#[test]
fn uidnext_is_used_only_when_it_is_a_consistent_high_watermark() {
    assert_eq!(
        selected_highest_uid(&imap::SelectedFolder {
            uid_validity: Some(7),
            uid_next: Some(101),
            exists: 2,
        }),
        Some(100)
    );
    assert_eq!(
        selected_highest_uid(&imap::SelectedFolder {
            uid_validity: Some(7),
            uid_next: Some(2),
            exists: 2,
        }),
        None
    );
    assert_eq!(
        selected_highest_uid(&imap::SelectedFolder {
            uid_validity: Some(7),
            uid_next: None,
            exists: 2,
        }),
        None
    );
}

#[tokio::test]
async fn courier_cycle_applies_move_and_downloads_sparse_new_uid_without_uidnext() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let commands = Arc::new(Mutex::new(Vec::<String>::new()));
    let command_log = commands.clone();
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut stream = BufReader::new(tls_acceptor().accept(tcp).await.unwrap());
        stream
            .write_all(b"* OK Courier fixture ready\r\n")
            .await
            .unwrap();
        let mut selected = String::new();
        let mut inbox_selects = 0;

        loop {
            let command = read_line(&mut stream).await;
            if command.is_empty() {
                break;
            }
            command_log.lock().unwrap().push(command.clone());
            let fields: Vec<_> = command.split_whitespace().collect();
            let tag = fields[0];
            let response = match fields[1] {
                "LOGIN" => format!("{tag} OK logged in\r\n"),
                "CAPABILITY" => {
                    format!("* CAPABILITY IMAP4rev1 UIDPLUS\r\n{tag} OK capability\r\n")
                }
                "SELECT" => {
                    selected = if command.contains("Archive") {
                        "Archive".into()
                    } else {
                        inbox_selects += 1;
                        "INBOX".into()
                    };
                    let exists = if selected == "Archive" {
                        1
                    } else if inbox_selects == 1 {
                        2
                    } else {
                        1
                    };
                    format!(
                        "* FLAGS (\\Seen \\Deleted)\r\n\
                         * {exists} EXISTS\r\n\
                         * OK [UIDVALIDITY 7] valid\r\n\
                         {tag} OK [READ-WRITE] selected\r\n"
                    )
                }
                "UID" if fields[2] == "COPY" => format!("{tag} OK copied\r\n"),
                "UID" if fields[2] == "STORE" => format!("{tag} OK stored\r\n"),
                "UID" if fields[2] == "EXPUNGE" => {
                    format!("* 1 EXPUNGE\r\n{tag} OK expunged\r\n")
                }
                "UID" if fields[2] == "SEARCH" => {
                    let hits = if selected == "INBOX" {
                        if command.contains(" SEARCH ALL") {
                            "1000042"
                        } else {
                            // Include the old high UID to reproduce IMAP's
                            // reversed n:* behavior on some servers.
                            "42 1000042"
                        }
                    } else {
                        "88"
                    };
                    format!("* SEARCH {hits}\r\n{tag} OK searched\r\n")
                }
                "UID" if fields[2] == "FETCH" => {
                    let (uid, message_id, subject) = if selected == "INBOX" {
                        assert_eq!(fields[3], "1000042", "unexpected fetch: {command:?}");
                        (1_000_042, "new-1000042@example.test", "Sparse arrival")
                    } else {
                        assert_eq!(fields[3], "88", "unexpected fetch: {command:?}");
                        (88, "archived@example.test", "Archived message")
                    };
                    let header = format!(
                        "Message-ID: <{message_id}>\r\n\
                         From: sender@example.test\r\n\
                         To: reader@example.test\r\n\
                         Subject: {subject}\r\n\
                         Date: Tue, 22 Sep 2026 12:00:00 +0000\r\n\
                         Content-Type: text/plain; charset=utf-8\r\n\r\n"
                    );
                    format!(
                        "* 1 FETCH (UID {uid} FLAGS () RFC822.SIZE 200 BODY[HEADER.FIELDS (FROM TO SUBJECT DATE MESSAGE-ID CONTENT-TYPE)] {{{}}}\r\n{header})\r\n{tag} OK fetched\r\n",
                        header.len()
                    )
                }
                "LOGOUT" => {
                    stream
                        .write_all(format!("* BYE closing\r\n{tag} OK logout\r\n").as_bytes())
                        .await
                        .unwrap();
                    break;
                }
                other => panic!("unexpected command {other}: {command:?}"),
            };
            stream.write_all(response.as_bytes()).await.unwrap();
        }
    });

    let dir = tempfile::tempdir().unwrap();
    let paths = Arc::new(Paths::for_tests(dir.path()));
    let db = Db::open(&dir.path().join("mail.db")).unwrap();
    let calendar_db = Db::open_calendar(&dir.path().join("calendar.db")).unwrap();
    let credentials: CredentialStoreHandle = Arc::new(
        credentials::DevelopmentFileCredentialStore::new(dir.path().join("credentials.json")),
    );
    let tokens = TokenProvider::new(
        credentials.clone(),
        Arc::new(crate::oauth::redirect::LoopbackRedirectBroker::default()),
    );
    let ctx = SyncCtx {
        db,
        calendar_db,
        bus: EventBus::new(),
        paths,
        tokens,
        credentials,
    };
    let (mut config, action_id, inbox_id, archived_message_id) = ctx
        .db
        .write(move |conn| {
            let account_id = repo::accounts::insert(
                conn,
                &repo::accounts::NewAccount {
                    email: "reader@example.test",
                    display_name: None,
                    avatar_url: None,
                    provider: Provider::Generic,
                    auth_kind: AuthKind::Password,
                    mail_protocol: MailProtocol::Imap,
                    username: "reader@example.test",
                    jmap_url: "",
                    jmap_account_id: None,
                    incoming_host: "127.0.0.1",
                    incoming_port: port,
                    smtp_host: "127.0.0.1",
                    smtp_port: 465,
                },
                &crate::db::testutil::account_settings(),
            )?;
            let inbox_id =
                repo::folders::upsert(conn, account_id, "INBOX", Some("/"), Some(roles::INBOX))?;
            let archive_id = repo::folders::upsert(
                conn,
                account_id,
                "Archive",
                Some("/"),
                Some(roles::ARCHIVE),
            )?;
            repo::folders::set_uid_state(conn, inbox_id, Some(7), None, None)?;
            repo::folders::set_last_seen_uid(conn, inbox_id, 42)?;
            repo::folders::set_backfill(conn, inbox_id, Some(43), true)?;
            repo::folders::set_uid_state(conn, archive_id, Some(7), None, None)?;
            repo::folders::set_backfill(conn, archive_id, Some(1), true)?;

            let thread_id = repo::threads::create(conn, account_id, None, "archived message")?;
            let message_id = repo::messages::insert(
                conn,
                &NewMessage {
                    account_id,
                    folder_id: archive_id,
                    uid: None,
                    message_id: Some("archived@example.test".into()),
                    gm_msgid: None,
                    gm_thrid: None,
                    subject: "Archived message".into(),
                    from: Some(Address {
                        name: None,
                        email: "sender@example.test".into(),
                    }),
                    to: Vec::new(),
                    cc: Vec::new(),
                    bcc: Vec::new(),
                    date: 1,
                    internal_date: None,
                    is_read: false,
                    is_starred: false,
                    is_draft: false,
                    is_outgoing: false,
                    is_automated: false,
                    has_attachments: false,
                    size: None,
                    snippet: String::new(),
                    references: Vec::new(),
                    list_unsubscribe: None,
                    list_unsubscribe_post: None,
                    sender_addr: None,
                    sender_verification: SenderVerification::None,
                },
                thread_id,
            )?;
            repo::threads::recompute(conn, thread_id)?;
            let action_id = repo::actions::enqueue(
                conn,
                account_id,
                "archive",
                Some(message_id),
                Some(thread_id),
                &serde_json::json!({
                    "srcFolderId": inbox_id,
                    "srcUid": 42,
                    "targetFolderId": archive_id,
                }),
                None,
            )?;
            Ok((
                repo::accounts::get_config(conn, account_id)?.unwrap(),
                action_id,
                inbox_id,
                message_id,
            ))
        })
        .await
        .unwrap();
    config.settings.connection = MailConnectionSettings {
        incoming_security: ConnectionSecurity::Tls,
        smtp_security: ConnectionSecurity::Tls,
        trusted_certificate_pem: CERT.into(),
    };
    credentials::store_async(
        ctx.credentials.clone(),
        config.id,
        Slot::Password,
        "fixture-secret".into(),
    )
    .await
    .unwrap();

    let mut session = connect(&ctx, &config).await.unwrap();
    let (hist_tx, _hist_rx) = mpsc::channel(1);
    let mut inbox_baseline = None;
    let remaining = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        run_cycle(
            &ctx,
            &config,
            &mut session,
            1,
            &hist_tx,
            &mut inbox_baseline,
            CycleMode {
                foreground_only: false,
                immediate_follow_up: false,
            },
        ),
    )
    .await
    .expect("sync cycle timed out")
    .unwrap();
    assert!(!remaining);
    imap::logout(session).await;
    server.await.unwrap();

    ctx.db
        .read(move |conn| {
            assert_eq!(repo::actions::get(conn, action_id)?.unwrap().state, "done");
            assert!(repo::messages::by_folder_uid(conn, inbox_id, 1_000_042)?.is_some());
            let archived = repo::messages::get_row(conn, archived_message_id)?.unwrap();
            assert_eq!(archived.uid, Some(88));
            Ok(())
        })
        .await
        .unwrap();
    let commands = commands.lock().unwrap();
    let copy = commands
        .iter()
        .position(|command| command.contains(" UID COPY 42 "))
        .unwrap();
    let search = commands
        .iter()
        .position(|command| command.contains(" UID SEARCH UID 43:*"))
        .unwrap();
    assert!(
        copy < search,
        "queued move was not scheduled before Inbox catch-up"
    );
}

async fn deleted_mail_context(port: u16) -> (tempfile::TempDir, SyncCtx, AccountConfig, Folder) {
    let dir = tempfile::tempdir().unwrap();
    let credentials: CredentialStoreHandle = Arc::new(
        credentials::DevelopmentFileCredentialStore::new(dir.path().join("credentials.json")),
    );
    let ctx = SyncCtx {
        db: Db::open(&dir.path().join("mail.db")).unwrap(),
        calendar_db: Db::open_calendar(&dir.path().join("calendar.db")).unwrap(),
        bus: EventBus::new(),
        paths: Arc::new(Paths::for_tests(dir.path())),
        tokens: TokenProvider::new(
            credentials.clone(),
            Arc::new(crate::oauth::redirect::LoopbackRedirectBroker::default()),
        ),
        credentials,
    };
    let (mut config, folder) = ctx
        .db
        .write(move |conn| {
            let account_id = repo::accounts::insert(
                conn,
                &repo::accounts::NewAccount {
                    email: "reader@example.test",
                    display_name: None,
                    avatar_url: None,
                    provider: Provider::Generic,
                    auth_kind: AuthKind::Password,
                    mail_protocol: MailProtocol::Imap,
                    username: "reader@example.test",
                    jmap_url: "",
                    jmap_account_id: None,
                    incoming_host: "127.0.0.1",
                    incoming_port: port,
                    smtp_host: "127.0.0.1",
                    smtp_port: 465,
                },
                &crate::db::testutil::account_settings(),
            )?;
            let folder_id =
                repo::folders::upsert(conn, account_id, "INBOX", Some("/"), Some(roles::INBOX))?;
            repo::folders::set_backfill(conn, folder_id, Some(1), true)?;
            Ok((
                repo::accounts::get_config(conn, account_id)?.unwrap(),
                repo::folders::get(conn, folder_id)?.unwrap(),
            ))
        })
        .await
        .unwrap();
    config.settings.connection = MailConnectionSettings {
        incoming_security: ConnectionSecurity::Tls,
        smtp_security: ConnectionSecurity::Tls,
        trusted_certificate_pem: CERT.into(),
    };
    credentials::store_async(
        ctx.credentials.clone(),
        config.id,
        Slot::Password,
        "fixture-secret".into(),
    )
    .await
    .unwrap();
    (dir, ctx, config, folder)
}

fn deletion_header(uid: u32, deleted: bool) -> FetchedHeader {
    FetchedHeader {
        uid,
        flags: imap::FetchedFlags { deleted, ..Default::default() },
        internal_date_ms: Some(now_ms()),
        size: None,
        header_bytes: format!("Message-ID: <deletion-{uid}@example.test>\r\nFrom: sender@example.test\r\nTo: reader@example.test\r\nSubject: Message {uid}\r\n\r\n").into_bytes(),
        has_attachments: false,
        mime_plan: None,
    }
}

#[tokio::test]
async fn deleted_headers_are_skipped_without_stalling_progress_or_notifying() {
    let (_dir, ctx, config, folder) = deleted_mail_context(1).await;
    let folder_id = folder.id;
    let failure_id = ctx
        .db
        .write(move |conn| {
            repo::sync_failures::record_header(conn, folder_id, 42, None, "old parse failure")
        })
        .await
        .unwrap();
    let (updated, fresh) = store_headers(
        &ctx,
        &config,
        &folder,
        vec![deletion_header(42, true)],
        Some(1),
        NewHeaders::Insert,
    )
    .await
    .unwrap();
    assert!(updated.is_empty());
    assert!(fresh.is_empty());
    ctx.db
        .read(move |conn| {
            assert!(repo::messages::uids_in_folder(conn, folder_id)?.is_empty());
            assert_eq!(
                repo::folders::get(conn, folder_id)?.unwrap().last_seen_uid,
                42
            );
            assert!(repo::sync_failures::get(conn, failure_id)?.is_none());
            let notifications: i64 =
                conn.query_row("SELECT COUNT(*) FROM notification_outbox", [], |row| {
                    row.get(0)
                })?;
            assert_eq!(notifications, 0);
            Ok(())
        })
        .await
        .unwrap();

    // Mixed batches still import and notify for the real arrival.
    let (updated, fresh) = store_headers(
        &ctx,
        &config,
        &folder,
        vec![deletion_header(43, false), deletion_header(44, true)],
        Some(1),
        NewHeaders::Insert,
    )
    .await
    .unwrap();
    assert_eq!(updated.len(), 1);
    assert_eq!(fresh, updated);
    ctx.db
        .read(move |conn| {
            assert!(repo::messages::by_folder_uid(conn, folder_id, 43)?.is_some());
            assert!(repo::messages::by_folder_uid(conn, folder_id, 44)?.is_none());
            assert_eq!(
                repo::folders::get(conn, folder_id)?.unwrap().last_seen_uid,
                44
            );
            Ok(())
        })
        .await
        .unwrap();

    // Being in Trash does not imply the IMAP \Deleted flag.
    let account_id = config.id;
    let trash = ctx
        .db
        .write(move |conn| {
            let id =
                repo::folders::upsert(conn, account_id, "Trash", Some("/"), Some(roles::TRASH))?;
            Ok(repo::folders::get(conn, id)?.unwrap())
        })
        .await
        .unwrap();
    let (updated, _) = store_headers(
        &ctx,
        &config,
        &trash,
        vec![deletion_header(45, false)],
        None,
        NewHeaders::Insert,
    )
    .await
    .unwrap();
    assert_eq!(updated.len(), 1);
}

#[tokio::test]
async fn reload_removes_deleted_messages_even_outside_flag_window() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let restored = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let server_restored = restored.clone();
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut stream = BufReader::new(tls_acceptor().accept(tcp).await.unwrap());
        stream.write_all(b"* OK fixture ready\r\n").await.unwrap();
        loop {
            let command = read_line(&mut stream).await;
            let fields: Vec<_> = command.split_whitespace().collect();
            let tag = fields[0];
            let response = match fields[1] {
                "LOGIN" => format!("{tag} OK logged in\r\n"),
                "CAPABILITY" => format!("* CAPABILITY IMAP4rev1\r\n{tag} OK capability\r\n"),
                "SELECT" => {
                    format!(
                        "* 3 EXISTS\r\n* OK [UIDVALIDITY 7] valid\r\n* OK [UIDNEXT 1000043] next\r\n{tag} OK selected\r\n"
                    )
                }
                "UID" if fields[2] == "FETCH" => {
                    if command.contains("HEADER.FIELDS") {
                        assert_eq!(fields[3], "41:42");
                        let mut response = String::new();
                        for uid in [41, 42] {
                            let header =
                                String::from_utf8(deletion_header(uid, false).header_bytes)
                                    .unwrap();
                            response.push_str(&format!(
                                "* {uid} FETCH (UID {uid} FLAGS () BODY[HEADER.FIELDS (MESSAGE-ID FROM TO SUBJECT)] {{{}}}\r\n{header})\r\n",
                                header.len()
                            ));
                        }
                        response.push_str(&format!("{tag} OK fetched\r\n"));
                        response
                    } else {
                        assert_eq!(fields[3], "999042:1000042");
                        assert!(command.contains("FLAGS"));
                        format!("* 3 FETCH (UID 1000042 FLAGS ())\r\n{tag} OK fetched\r\n")
                    }
                }
                "UID" => {
                    assert_eq!(
                        fields[2], "SEARCH",
                        "sync must not mutate the server: {command}"
                    );
                    let hits = match fields[3..].join(" ").as_str() {
                        // 42 and 43 still exist on the server, but are deleted.
                        "ALL" => "42 43 1000042",
                        "UNDELETED"
                            if server_restored.load(std::sync::atomic::Ordering::SeqCst) =>
                        {
                            "41 42 1000042"
                        }
                        "UNDELETED" => "1000042",
                        "SINCE 1-Oct-2026 UNDELETED" => "1000042",
                        "UID 44:* UNDELETED" => "1000042",
                        other => panic!("unexpected search: {other}"),
                    };
                    format!("* SEARCH {hits}\r\n{tag} OK searched\r\n")
                }
                "LOGOUT" => {
                    stream
                        .write_all(format!("* BYE closing\r\n{tag} OK logout\r\n").as_bytes())
                        .await
                        .unwrap();
                    break;
                }
                other => panic!("unexpected command {other}: {command}"),
            };
            stream.write_all(response.as_bytes()).await.unwrap();
        }
    });
    let (_dir, ctx, config, folder) = deleted_mail_context(port).await;
    // Reproduce a cache populated by an older version (plus an expunged UID).
    store_headers(
        &ctx,
        &config,
        &folder,
        vec![
            deletion_header(41, true),
            deletion_header(42, false),
            deletion_header(43, false),
            deletion_header(44, false),
            deletion_header(1_000_042, false),
        ],
        None,
        NewHeaders::Insert,
    )
    .await
    .unwrap();
    let folder_id = folder.id;
    let account_id = config.id;
    let removed_threads = ctx
        .db
        .write(move |conn| {
            let pending = repo::messages::by_folder_uid(conn, folder_id, 43)?.unwrap();
            repo::actions::enqueue(
                conn,
                account_id,
                "mark_read",
                Some(pending.id),
                pending.thread_id,
                &serde_json::json!({}),
                Some(now_ms() + 60_000),
            )?;
            Ok(vec![
                repo::messages::by_folder_uid(conn, folder_id, 42)?
                    .unwrap()
                    .thread_id
                    .unwrap(),
                repo::messages::by_folder_uid(conn, folder_id, 44)?
                    .unwrap()
                    .thread_id
                    .unwrap(),
            ])
        })
        .await
        .unwrap();
    let mut session = connect(&ctx, &config).await.unwrap();
    imap::select(&mut session, "INBOX").await.unwrap();
    assert_eq!(
        imap::uid_search_all(&mut session).await.unwrap(),
        [42, 43, 1_000_042]
    );
    assert_eq!(
        imap::uid_search_since(
            &mut session,
            chrono::NaiveDate::from_ymd_opt(2026, 10, 1).unwrap()
        )
        .await
        .unwrap(),
        [1_000_042]
    );
    assert_eq!(
        imap::uid_search_after(&mut session, 43).await.unwrap(),
        [1_000_042]
    );
    let mut events = ctx.bus.subscribe();
    let (hist_tx, _hist_rx) = mpsc::channel(1);
    // A manual reload on cycle 1 must not wait for the periodic fifth cycle.
    run_cycle(
        &ctx,
        &config,
        &mut session,
        1,
        &hist_tx,
        &mut None,
        CycleMode {
            foreground_only: true,
            immediate_follow_up: false,
        },
    )
    .await
    .unwrap();
    let CoreEvent::MailUpdated { mut thread_ids } = events.try_recv().unwrap() else {
        panic!("expected list refresh after removing deleted messages");
    };
    thread_ids.sort_unstable();
    let mut expected = removed_threads.clone();
    expected.sort_unstable();
    assert_eq!(thread_ids, expected);
    ctx.db
        .read(move |conn| {
            assert!(repo::messages::by_folder_uid(conn, folder_id, 42)?.is_none());
            assert!(repo::messages::by_folder_uid(conn, folder_id, 44)?.is_none());
            assert!(
                repo::messages::by_folder_uid(conn, folder_id, 43)?.is_some(),
                "pending local actions must survive reconciliation"
            );
            assert!(repo::messages::by_folder_uid(conn, folder_id, 1_000_042)?.is_some());
            for tid in removed_threads {
                let count: i64 =
                    conn.query_row("SELECT COUNT(*) FROM threads WHERE id = ?1", [tid], |row| {
                        row.get(0)
                    })?;
                assert_eq!(count, 0, "empty threads must be removed");
            }
            Ok(())
        })
        .await
        .unwrap();
    // Repeated reloads are idempotent.
    reconcile_mailbox(&ctx, &config, &mut session, &folder)
        .await
        .unwrap();
    assert!(events.try_recv().is_err());

    // Clearing \Deleted preserves UID and UIDNEXT. Recover both a removed
    // cached message and one skipped during its initial download.
    restored.store(true, std::sync::atomic::Ordering::SeqCst);
    run_cycle(
        &ctx,
        &config,
        &mut session,
        2,
        &hist_tx,
        &mut None,
        CycleMode {
            foreground_only: true,
            immediate_follow_up: false,
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        events.try_recv().unwrap(),
        CoreEvent::MailUpdated { .. }
    ));
    assert!(
        events.try_recv().is_err(),
        "restoration must not emit MailNew"
    );
    ctx.db
        .read(move |conn| {
            for uid in [41, 42] {
                assert!(repo::messages::by_folder_uid(conn, folder_id, uid)?.is_some());
            }
            assert_eq!(
                repo::folders::get(conn, folder_id)?.unwrap().last_seen_uid,
                1_000_042
            );
            let notifications: i64 =
                conn.query_row("SELECT COUNT(*) FROM notification_outbox", [], |row| {
                    row.get(0)
                })?;
            assert_eq!(notifications, 0);
            Ok(())
        })
        .await
        .unwrap();
    imap::logout(session).await;
    server.await.unwrap();
}

/// A message of the account that waits to be matched with its copy on the
/// server, as a server change or a recreated mailbox leaves it.
fn seed_waiting(
    conn: &rusqlite::Connection,
    folder_id: i64,
    message_id: &str,
    received: i64,
) -> i64 {
    conn.execute(
        "INSERT INTO threads (account_id, subject_norm) VALUES (1, ?1)",
        [message_id],
    )
    .unwrap();
    let thread_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO messages (account_id, thread_id, folder_id, message_id, subject, date,
                               internal_date, relink_pending)
         VALUES (1, ?1, ?2, ?3, ?3, ?4, ?4, 1)",
        rusqlite::params![thread_id, folder_id, message_id, received],
    )
    .unwrap();
    conn.last_insert_rowid()
}

async fn still_there(ctx: &SyncCtx, id: i64) -> bool {
    ctx.db
        .read(move |conn| Ok(repo::messages::get_row(conn, id)?.is_some()))
        .await
        .unwrap()
}

#[tokio::test]
async fn waiting_mail_stays_until_each_folder_was_searched() {
    // Nothing listens here.
    let (_dir, ctx, config, inbox) = deleted_mail_context(1).await;
    let inbox_id = inbox.id;
    let (archive_id, waiting) = ctx
        .db
        .write(move |conn| {
            let archive =
                repo::folders::upsert(conn, 1, "Archive", Some("/"), Some(roles::ARCHIVE))?;
            repo::folders::set_backfill(conn, archive, Some(1), false)?;
            Ok((
                archive,
                seed_waiting(conn, inbox_id, "old@example.test", 1_000),
            ))
        })
        .await
        .unwrap();
    let mut session = None;

    // Right after a switch from JMAP, the server's folders aren't known yet.
    ctx.db
        .write(move |conn| {
            conn.execute(
                "UPDATE folders SET jmap_id = 'inbox' WHERE id = ?1",
                [inbox_id],
            )?;
            repo::folders::set_backfill(conn, archive_id, Some(1), true)
        })
        .await
        .unwrap();
    settle_unlinked_mail(&ctx, &config, &mut session)
        .await
        .unwrap();
    assert!(still_there(&ctx, waiting).await);

    // A folder still without its history.
    ctx.db
        .write(move |conn| {
            conn.execute(
                "UPDATE folders SET jmap_id = NULL WHERE id = ?1",
                [inbox_id],
            )?;
            repo::folders::set_backfill(conn, archive_id, Some(1), false)
        })
        .await
        .unwrap();
    settle_unlinked_mail(&ctx, &config, &mut session)
        .await
        .unwrap();
    assert!(still_there(&ctx, waiting).await);

    // A server that can't be searched removes nothing.
    ctx.db
        .write(move |conn| repo::folders::set_backfill(conn, archive_id, Some(1), true))
        .await
        .unwrap();
    assert!(
        settle_unlinked_mail(&ctx, &config, &mut session)
            .await
            .is_err()
    );
    assert!(still_there(&ctx, waiting).await);
    assert!(session.is_none());
}

#[tokio::test]
async fn waiting_mail_is_matched_with_the_server_or_removed() {
    let day = 24 * 60 * 60 * 1000;
    let matched_at = now_ms() - 730 * day;
    let since = chrono::DateTime::from_timestamp_millis(matched_at - day)
        .unwrap()
        .date_naive()
        .format("%-d-%b-%Y")
        .to_string();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let commands = Arc::new(Mutex::new(Vec::<String>::new()));
    let command_log = commands.clone();
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut stream = BufReader::new(tls_acceptor().accept(tcp).await.unwrap());
        stream.write_all(b"* OK fixture ready\r\n").await.unwrap();
        let mut selected = String::new();
        loop {
            let command = read_line(&mut stream).await;
            command_log.lock().unwrap().push(command.clone());
            let fields: Vec<_> = command.split_whitespace().collect();
            let tag = fields[0];
            let response = match fields[1] {
                "LOGIN" => format!("{tag} OK logged in\r\n"),
                "CAPABILITY" => format!("* CAPABILITY IMAP4rev1\r\n{tag} OK capability\r\n"),
                "SELECT" => {
                    selected = fields[2].trim_matches('"').to_owned();
                    format!("* 2 EXISTS\r\n* OK [UIDVALIDITY 9] valid\r\n{tag} OK selected\r\n")
                }
                "UID" if fields[2] == "SEARCH" => {
                    assert_eq!(fields[3..].join(" "), format!("SINCE {since} UNDELETED"));
                    // Inbox: mail no local message stands for, the copy of
                    // one that lost its UID, and mail that arrived after the
                    // live sync's last scan.
                    let hits = if selected == "INBOX" { "3 5 60" } else { "7" };
                    format!("* SEARCH {hits}\r\n{tag} OK searched\r\n")
                }
                "UID" if fields[2] == "FETCH" => {
                    let found: &[(u32, &str)] = if selected == "INBOX" {
                        assert_eq!(fields[3], "3,5,60");
                        &[
                            (3, "unrelated@example.test"),
                            (5, "recent@example.test"),
                            (60, "fresh@example.test"),
                        ]
                    } else {
                        assert_eq!(fields[3], "7");
                        &[(7, "old@example.test")]
                    };
                    let mut response = String::new();
                    for (index, (uid, message_id)) in found.iter().enumerate() {
                        let header = format!(
                            "Message-ID: <{message_id}>\r\nFrom: sender@example.test\r\nSubject: {message_id}\r\n\r\n"
                        );
                        response.push_str(&format!(
                            "* {} FETCH (UID {uid} FLAGS () BODY[HEADER.FIELDS (MESSAGE-ID FROM SUBJECT)] {{{}}}\r\n{header})\r\n",
                            index + 1,
                            header.len()
                        ));
                    }
                    response.push_str(&format!("{tag} OK fetched\r\n"));
                    response
                }
                "LOGOUT" => {
                    stream
                        .write_all(format!("* BYE closing\r\n{tag} OK logout\r\n").as_bytes())
                        .await
                        .unwrap();
                    break;
                }
                other => panic!("unexpected command {other}: {command}"),
            };
            stream.write_all(response.as_bytes()).await.unwrap();
        }
    });

    let (dir, ctx, config, inbox) = deleted_mail_context(port).await;
    let gone_file = dir.path().join("gone.eml");
    tokio::fs::write(&gone_file, b"Subject: gone\r\n\r\n")
        .await
        .unwrap();
    let gone_path = gone_file.to_str().unwrap().to_owned();
    let inbox_id = inbox.id;
    let (archive_id, matched, recent, gone, busy) = ctx
        .db
        .write(move |conn| {
            repo::folders::set_last_seen_uid(conn, inbox_id, 50)?;
            let archive =
                repo::folders::upsert(conn, 1, "Archive", Some("/"), Some(roles::ARCHIVE))?;
            repo::folders::set_backfill(conn, archive, Some(1), true)?;
            let matched = seed_waiting(conn, inbox_id, "old@example.test", matched_at);
            // Within the history, as mail flagged by the upgrade is.
            let recent = seed_waiting(conn, inbox_id, "recent@example.test", now_ms() - 10 * day);
            let gone = seed_waiting(conn, inbox_id, "gone@example.test", matched_at + day);
            conn.execute(
                "UPDATE messages SET raw_path = ?2 WHERE id = ?1",
                rusqlite::params![gone, gone_path],
            )?;
            // Work queued for it keeps it, and leaves it out of the search.
            let busy = seed_waiting(conn, inbox_id, "busy@example.test", matched_at - 365 * day);
            repo::actions::enqueue(
                conn,
                1,
                "star",
                Some(busy),
                None,
                &serde_json::json!({}),
                Some(now_ms() + 60_000),
            )?;
            Ok((archive, matched, recent, gone, busy))
        })
        .await
        .unwrap();

    let matched_thread = ctx
        .db
        .read(move |conn| Ok(repo::messages::get_row(conn, matched)?.unwrap().thread_id))
        .await
        .unwrap()
        .unwrap();
    let mut events = ctx.bus.subscribe();
    let mut session = None;
    settle_unlinked_mail(&ctx, &config, &mut session)
        .await
        .unwrap();
    if let Some(session) = session.take() {
        imap::logout(session).await;
    }
    server.await.unwrap();
    let CoreEvent::MailUpdated { thread_ids } = events.try_recv().unwrap() else {
        panic!("expected a list refresh for the message that moved to Archive");
    };
    assert_eq!(thread_ids, [matched_thread]);

    ctx.db
        .read(move |conn| {
            let row = repo::messages::get_row(conn, matched)?.unwrap();
            assert_eq!((row.folder_id, row.uid), (Some(archive_id), Some(7)));
            let row = repo::messages::get_row(conn, recent)?.unwrap();
            assert_eq!((row.folder_id, row.uid), (Some(inbox_id), Some(5)));
            assert!(repo::messages::get_row(conn, gone)?.is_none());
            assert!(repo::messages::get_row(conn, busy)?.is_some());
            assert_eq!(repo::messages::oldest_relink_pending(conn, 1)?, None);
            // The rest of the mailbox stays outside the history, and the
            // live sync still fetches what arrived since its last scan.
            for message_id in ["unrelated@example.test", "fresh@example.test"] {
                assert!(repo::messages::by_message_id(conn, 1, message_id)?.is_none());
            }
            assert_eq!(
                repo::folders::get(conn, inbox_id)?.unwrap().last_seen_uid,
                50
            );
            Ok(())
        })
        .await
        .unwrap();
    assert!(!gone_file.exists());
    assert_eq!(
        commands
            .lock()
            .unwrap()
            .iter()
            .filter(|command| command.contains(" SELECT "))
            .count(),
        2
    );
}
