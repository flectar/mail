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
            creates_keywords: false,
            permanent_keywords: Vec::new(),
        }),
        Some(100)
    );
    assert_eq!(
        selected_highest_uid(&imap::SelectedFolder {
            uid_validity: Some(7),
            uid_next: Some(2),
            exists: 2,
            creates_keywords: false,
            permanent_keywords: Vec::new(),
        }),
        None
    );
    assert_eq!(
        selected_highest_uid(&imap::SelectedFolder {
            uid_validity: Some(7),
            uid_next: None,
            exists: 2,
            creates_keywords: false,
            permanent_keywords: Vec::new(),
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
                // Without LIST-STATUS, each folder but the Inbox is asked.
                "STATUS" => {
                    assert!(command.contains("\"Archive\""), "{command:?}");
                    format!(
                        "* STATUS \"Archive\" (MESSAGES 1 UIDNEXT 89 UNSEEN 1 UIDVALIDITY 7)\r\n\
                         {tag} OK status\r\n"
                    )
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
        pace: Default::default(),
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
                    list_id: None,
                    sender_addr: None,
                    sender_verification: SenderVerification::None,
                    receipt_to: Vec::new(),
                    receipt_confirm: false,
                    receipt_handled: false,
                    importance: Default::default(),
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
        proxy: Default::default(),
        incoming_login: Default::default(),
        smtp_login: Default::default(),
        smtp_username: None,
        server_timeout_secs: None,
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
            &hist_tx,
            &mut inbox_baseline,
            &mut listed_now(),
            CycleMode {
                cycle: 1,
                foreground_only: false,
                immediate_follow_up: false,
                list_status: false,
                inbox_pushed: PushedChanges::default(),
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

/// One cycle against a server without LIST-STATUS in which Lists changed,
/// the Inbox keeping one unread message here. Returns the commands, the
/// Inbox's unread messages only on the server afterwards, and whether the
/// app was told they changed.
async fn count_inbox_without_list_status(inbox_uid_next: u32) -> (Vec<String>, Option<i64>, bool) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let commands = Arc::new(Mutex::new(Vec::<String>::new()));
    let command_log = commands.clone();
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut stream = BufReader::new(tls_acceptor().accept(tcp).await.unwrap());
        stream.write_all(b"* OK fixture ready\r\n").await.unwrap();
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
                "CAPABILITY" => format!("* CAPABILITY IMAP4rev1 IDLE\r\n{tag} OK capability\r\n"),
                "STATUS" if command.contains("Lists") => format!(
                    "* STATUS Lists (MESSAGES 50 UIDNEXT 51 UNSEEN 4 UIDVALIDITY 7)\r\n\
                     {tag} OK status\r\n"
                ),
                "STATUS" => format!(
                    "* STATUS INBOX (MESSAGES 3 UIDNEXT {inbox_uid_next} UNSEEN 3 UIDVALIDITY 7)\r\n\
                     {tag} OK status\r\n"
                ),
                "SELECT" => {
                    let (exists, uid_next) = if command.contains("Lists") {
                        (50, 51)
                    } else {
                        (3, 10)
                    };
                    format!(
                        "* {exists} EXISTS\r\n* OK [UIDVALIDITY 7] valid\r\n\
                         * OK [UIDNEXT {uid_next}] next\r\n{tag} OK [READ-WRITE] selected\r\n"
                    )
                }
                "UID" if fields[2] == "FETCH" => {
                    format!("* 1 FETCH (UID 50 FLAGS (\\Seen))\r\n{tag} OK fetched\r\n")
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

    let (_dir, ctx, config, inbox) = deleted_mail_context(port).await;
    let account_id = config.id;
    let inbox_id = inbox.id;
    ctx.db
        .write(move |conn| {
            repo::folders::set_uid_state(conn, inbox_id, Some(7), Some(10), None)?;
            repo::folders::set_last_seen_uid(conn, inbox_id, 9)?;
            crate::db::testutil::seed_message(conn, "sender@example.test", "Kept", false);
            // Lists' history is still downloading, so only the Inbox's
            // count can become known.
            let lists = repo::folders::upsert(conn, account_id, "Lists", Some("/"), None)?;
            repo::folders::set_uid_state(conn, lists, Some(7), Some(51), None)?;
            repo::folders::set_last_seen_uid(conn, lists, 50)?;
            repo::folders::set_backfill(conn, lists, Some(1), false)?;
            repo::folders::set_server_counts(conn, lists, Some(50), Some(5))?;
            Ok(())
        })
        .await
        .unwrap();

    let mut events = ctx.bus.subscribe();
    let mut session = connect(&ctx, &config).await.unwrap();
    let offered = imap::capabilities(&mut session).await.unwrap();
    assert!(!offered.list_status);
    let (hist_tx, _hist_rx) = mpsc::channel(1);
    run_cycle(
        &ctx,
        &config,
        &mut session,
        &hist_tx,
        &mut None,
        &mut listed_now(),
        CycleMode {
            cycle: 1,
            foreground_only: false,
            immediate_follow_up: false,
            list_status: offered.list_status,
            inbox_pushed: PushedChanges::default(),
        },
    )
    .await
    .unwrap();
    imap::logout(session).await;
    server.await.unwrap();
    let server_only = ctx
        .db
        .read(move |conn| repo::folders::server_only_unseen(conn, &[inbox_id]))
        .await
        .unwrap();
    let told = std::iter::from_fn(|| events.try_recv().ok())
        .any(|event| matches!(event, CoreEvent::ServerOnlyUnreadChanged { .. }));
    let commands = commands.lock().unwrap().clone();
    (commands, server_only, told)
}

#[tokio::test]
async fn without_list_status_the_inbox_is_counted_once_another_folder_is_open() {
    let (commands, server_only, told) = count_inbox_without_list_status(10).await;
    let position = |needle: &str| {
        commands
            .iter()
            .position(|command| command.contains(needle))
            .unwrap_or_else(|| panic!("no {needle}: {commands:?}"))
    };
    // STATUS never asks the selected Inbox: only after Lists is open.
    assert!(position("STATUS \"INBOX\"") > position("SELECT \"Lists\""));
    // Three unread on the server, one of them kept here.
    assert_eq!(server_only, Some(2));
    assert!(told);
}

#[tokio::test]
async fn an_inbox_count_taken_after_new_mail_arrived_is_not_kept() {
    // UIDNEXT moved past what the Inbox's sync saw: the count includes mail
    // that is not here yet.
    let (_, server_only, told) = count_inbox_without_list_status(11).await;
    assert_eq!((server_only, told), (None, false));
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
        pace: Default::default(),
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
        proxy: Default::default(),
        incoming_login: Default::default(),
        smtp_login: Default::default(),
        smtp_username: None,
        server_timeout_secs: None,
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
async fn a_blocked_senders_new_mail_goes_to_spam_unannounced() {
    let (_dir, ctx, config, folder) = deleted_mail_context(1).await;
    let (account_id, inbox) = (config.id, folder.id);
    let spam = ctx
        .db
        .write(move |conn| {
            let spam =
                repo::folders::upsert(conn, account_id, "Junk", Some("/"), Some(roles::SPAM))?;
            repo::blocked::block(conn, "sender@example.test", 0)?;
            Ok(spam)
        })
        .await
        .unwrap();
    let (updated, fresh) = store_headers(
        &ctx,
        &config,
        &folder,
        vec![deletion_header(50, false)],
        Some(1),
        NewHeaders::Insert,
    )
    .await
    .unwrap();
    assert_eq!(updated.len(), 1);
    assert!(fresh.is_empty());
    ctx.db
        .read(move |conn| {
            let (id, folder_id, uid): (i64, Option<i64>, Option<i64>) = conn.query_row(
                "SELECT id, folder_id, uid FROM messages
                 WHERE message_id = 'deletion-50@example.test'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            assert_eq!((folder_id, uid), (Some(spam), None));
            // The server copy moves out of the inbox like Move to spam's.
            let (kind, payload): (String, String) = conn.query_row(
                "SELECT kind, payload FROM pending_actions WHERE message_id = ?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            assert_eq!(kind, "spam");
            let payload: serde_json::Value = serde_json::from_str(&payload).unwrap();
            assert_eq!(payload["srcFolderId"], inbox);
            assert_eq!(payload["srcUid"], 50);
            assert_eq!(payload["messageOnly"], true);
            let notifications: i64 =
                conn.query_row("SELECT COUNT(*) FROM notification_outbox", [], |row| {
                    row.get(0)
                })?;
            assert_eq!(notifications, 0);
            Ok(())
        })
        .await
        .unwrap();

    // Once unblocked, their mail arrives as before.
    ctx.db
        .write(|conn| repo::blocked::unblock(conn, "sender@example.test"))
        .await
        .unwrap();
    let (_, fresh) = store_headers(
        &ctx,
        &config,
        &folder,
        vec![deletion_header(51, false)],
        Some(1),
        NewHeaders::Insert,
    )
    .await
    .unwrap();
    assert_eq!(fresh.len(), 1);
    ctx.db
        .read(move |conn| {
            assert!(repo::messages::by_folder_uid(conn, inbox, 51)?.is_some());
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn rules_sort_new_inbox_mail_and_what_they_file_away_stays_silent() {
    use crate::models::{MailRule, RuleAction, RuleCondition, RuleField, RuleOp};
    let (_dir, ctx, config, folder) = deleted_mail_context(1).await;
    let (account_id, inbox) = (config.id, folder.id);
    let news = ctx
        .db
        .write(move |conn| {
            let news = repo::folders::upsert(conn, account_id, "News", Some("/"), None)?;
            repo::rules::save(
                conn,
                &MailRule {
                    id: 0,
                    account_id: Some(account_id),
                    name: "News".into(),
                    enabled: true,
                    match_all: true,
                    conditions: vec![RuleCondition {
                        field: RuleField::Subject,
                        header: String::new(),
                        op: RuleOp::Is,
                        value: "message 60".into(),
                        negate: false,
                    }],
                    actions: vec![RuleAction::Move { folder: news }],
                    stop: false,
                },
                0,
            )?;
            Ok(news)
        })
        .await
        .unwrap();
    let (updated, fresh) = store_headers(
        &ctx,
        &config,
        &folder,
        vec![deletion_header(60, false), deletion_header(61, false)],
        Some(1),
        NewHeaders::Insert,
    )
    .await
    .unwrap();
    assert_eq!(updated.len(), 2);
    // Only the mail the rule left in the Inbox notifies.
    assert_eq!(fresh.len(), 1);
    ctx.db
        .read(move |conn| {
            let filed = repo::messages::get_row(
                conn,
                conn.query_row(
                    "SELECT id FROM messages WHERE message_id = 'deletion-60@example.test'",
                    [],
                    |row| row.get(0),
                )?,
            )?
            .unwrap();
            assert_eq!(filed.folder_id, Some(news));
            let kind: String = conn.query_row(
                "SELECT kind FROM pending_actions WHERE message_id = ?1",
                [filed.id],
                |row| row.get(0),
            )?;
            assert_eq!(kind, "move");
            assert!(repo::messages::by_folder_uid(conn, inbox, 61)?.is_some());
            Ok(())
        })
        .await
        .unwrap();

    // What a history pass finds stays where the server has it.
    let mut old = deletion_header(40, false);
    old.header_bytes = b"Message-ID: <old-40@example.test>\r\nFrom: sender@example.test\r\nSubject: Message 60\r\n\r\n".to_vec();
    store_headers(&ctx, &config, &folder, vec![old], None, NewHeaders::Insert)
        .await
        .unwrap();
    ctx.db
        .read(move |conn| {
            assert!(repo::messages::by_folder_uid(conn, inbox, 40)?.is_some());
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn a_rule_on_the_body_holds_new_mail_until_its_body_comes() {
    use crate::models::{MailRule, RuleAction, RuleCondition, RuleField, RuleOp};
    let (_dir, ctx, config, folder) = deleted_mail_context(1).await;
    let (account_id, inbox) = (config.id, folder.id);
    let news = ctx
        .db
        .write(move |conn| {
            let news = repo::folders::upsert(conn, account_id, "News", Some("/"), None)?;
            repo::rules::save(
                conn,
                &MailRule {
                    id: 0,
                    account_id: Some(account_id),
                    name: "Lists".into(),
                    enabled: true,
                    match_all: true,
                    conditions: vec![RuleCondition {
                        field: RuleField::Body,
                        header: String::new(),
                        op: RuleOp::Contains,
                        value: "unsubscribe".into(),
                        negate: false,
                    }],
                    actions: vec![RuleAction::Move { folder: news }],
                    stop: false,
                },
                0,
            )?;
            Ok(news)
        })
        .await
        .unwrap();
    let (_, fresh) = store_headers(
        &ctx,
        &config,
        &folder,
        vec![deletion_header(80, false), deletion_header(81, false)],
        Some(1),
        NewHeaders::Insert,
    )
    .await
    .unwrap();
    // Both wait for their bodies, and so do their notifications.
    assert!(fresh.is_empty());
    let waiting = ctx
        .db
        .read(move |conn| repo::messages::rules_waiting_in(conn, inbox))
        .await
        .unwrap();
    assert_eq!(
        waiting.iter().map(|(_, uid)| *uid).collect::<Vec<_>>(),
        [80, 81]
    );
    let raw = |uid: i64, text: &str| {
        format!(
            "Message-ID: <deletion-{uid}@example.test>\r\nFrom: sender@example.test\r\nTo: reader@example.test\r\nSubject: Message {uid}\r\n\r\n{text}\r\n"
        )
        .into_bytes()
    };
    let guard = SelectivePersistGuard::Background { folder_id: inbox };
    for ((message_id, uid), text) in waiting.iter().zip(["Unsubscribe here", "Hello"]) {
        let stored = persist_body(
            &ctx,
            &config,
            *message_id,
            guard,
            *uid as u32,
            &raw(*uid, text),
        )
        .await
        .unwrap();
        assert!(stored);
    }
    // The one that asks to unsubscribe moved without a word; the other
    // notifies now.
    let (moved, kept) = (waiting[0].0, waiting[1].0);
    ctx.db
        .read(move |conn| {
            let folder_of = |id| -> rusqlite::Result<Option<i64>> {
                conn.query_row(
                    "SELECT folder_id FROM messages WHERE id = ?1",
                    [id],
                    |row| row.get(0),
                )
            };
            assert_eq!(folder_of(moved)?, Some(news));
            assert_eq!(folder_of(kept)?, Some(inbox));
            let notified: Vec<i64> = conn
                .prepare("SELECT message_id FROM notification_outbox")?
                .query_map([], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            assert_eq!(notified, [kept]);
            assert!(repo::messages::rules_waiting_in(conn, inbox)?.is_empty());
            Ok(())
        })
        .await
        .unwrap();
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
        &hist_tx,
        &mut None,
        &mut listed_now(),
        CycleMode {
            cycle: 1,
            foreground_only: true,
            immediate_follow_up: false,
            list_status: false,
            inbox_pushed: PushedChanges::default(),
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
    // The Inbox pass caught up, which the app shows as the last check.
    assert!(matches!(
        events.try_recv().unwrap(),
        CoreEvent::AccountCaughtUp { account_id: caught_up, .. } if caught_up == account_id
    ));
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
        &hist_tx,
        &mut None,
        &mut listed_now(),
        CycleMode {
            cycle: 2,
            foreground_only: true,
            immediate_follow_up: false,
            list_status: false,
            inbox_pushed: PushedChanges::default(),
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        events.try_recv().unwrap(),
        CoreEvent::MailUpdated { .. }
    ));
    assert!(matches!(
        events.try_recv().unwrap(),
        CoreEvent::AccountCaughtUp { .. }
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

#[test]
fn reported_changes_are_rechecked_only_where_new_mail_does_not_explain_them() {
    let known = repo::folders::ServerStatus {
        uidvalidity: Some(7),
        uidnext: Some(51),
        messages: Some(50),
        unseen: Some(5),
    };
    let now = |messages, uid_next, unseen| imap::MailboxStatus {
        messages,
        uid_next: Some(uid_next),
        unseen: Some(unseen),
        uid_validity: Some(7),
    };
    let check = |reported| FolderCheck {
        reported: Some(reported),
        known: Some(known),
        ..Default::default()
    };

    assert!(check(now(50, 51, 5)).unchanged());
    assert!(!check(now(50, 51, 4)).unchanged());
    assert!(!check(now(50, 52, 5)).unchanged());
    // Two new messages, one of them unread, explain the whole change.
    assert_eq!(check(now(52, 53, 6)).reconcile(2, 1, true), (false, false));
    // A message was read elsewhere while an unread one arrived.
    assert_eq!(check(now(51, 52, 5)).reconcile(1, 1, true), (true, false));
    // A message was removed elsewhere.
    assert_eq!(check(now(49, 51, 5)).reconcile(0, 0, true), (false, true));
    // A folder still catching up with new mail is compared once it has.
    assert_eq!(check(now(49, 51, 4)).reconcile(0, 0, false), (false, false));
    // Without counts from the last sync there is nothing to compare yet.
    let first = FolderCheck {
        known: Some(repo::folders::ServerStatus {
            messages: None,
            unseen: None,
            ..known
        }),
        ..check(now(50, 51, 4))
    };
    assert!(!first.unchanged());
    assert_eq!(first.reconcile(0, 0, true), (false, false));
    // What a full pass or a push asks for is checked regardless.
    let asked = FolderCheck {
        flags: true,
        removals: true,
        ..check(now(52, 53, 6))
    };
    assert_eq!(asked.reconcile(2, 1, true), (true, true));
}

/// A folder that reports what it did at its last sync is not opened; one
/// whose unread count fell is opened and its read state checked again.
#[tokio::test]
async fn list_status_opens_only_the_folders_that_changed() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let commands = Arc::new(Mutex::new(Vec::<String>::new()));
    let command_log = commands.clone();
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut stream = BufReader::new(tls_acceptor().accept(tcp).await.unwrap());
        stream.write_all(b"* OK fixture ready\r\n").await.unwrap();
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
                    format!("* CAPABILITY IMAP4rev1 IDLE LIST-STATUS\r\n{tag} OK capability\r\n")
                }
                "LIST" => {
                    assert!(command.contains("RETURN (STATUS"), "{command:?}");
                    format!(
                        "* LIST () \"/\" INBOX\r\n\
                         * STATUS INBOX (MESSAGES 3 UIDNEXT 10 UNSEEN 1 UIDVALIDITY 7)\r\n\
                         * LIST () \"/\" Archive\r\n\
                         * STATUS Archive (MESSAGES 20 UIDNEXT 21 UNSEEN 0 UIDVALIDITY 7)\r\n\
                         * LIST () \"/\" Lists\r\n\
                         * STATUS Lists (MESSAGES 50 UIDNEXT 51 UNSEEN 4 UIDVALIDITY 7)\r\n\
                         {tag} OK list completed\r\n"
                    )
                }
                "SELECT" => {
                    assert!(!command.contains("Archive"), "unchanged folder opened");
                    let (exists, uid_next) = if command.contains("Lists") {
                        (50, 51)
                    } else {
                        (3, 10)
                    };
                    format!(
                        "* {exists} EXISTS\r\n* OK [UIDVALIDITY 7] valid\r\n\
                         * OK [UIDNEXT {uid_next}] next\r\n{tag} OK [READ-WRITE] selected\r\n"
                    )
                }
                "UID" if fields[2] == "FETCH" => {
                    assert!(command.contains("FLAGS"), "{command:?}");
                    format!("* 1 FETCH (UID 50 FLAGS (\\Seen))\r\n{tag} OK fetched\r\n")
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

    let (_dir, ctx, config, inbox) = deleted_mail_context(port).await;
    let account_id = config.id;
    let inbox_id = inbox.id;
    let (archive_id, lists_id, message_id) = ctx
        .db
        .write(move |conn| {
            repo::folders::set_uid_state(conn, inbox_id, Some(7), Some(10), None)?;
            repo::folders::set_last_seen_uid(conn, inbox_id, 9)?;
            let archive_id = synced_folder(conn, account_id, "Archive", 21, 20, 0)?;
            let lists_id = synced_folder(conn, account_id, "Lists", 51, 50, 5)?;
            let message_id = seed_unread(conn, account_id, lists_id, 50)?;
            Ok((archive_id, lists_id, message_id))
        })
        .await
        .unwrap();

    let mut session = connect(&ctx, &config).await.unwrap();
    let offered = imap::capabilities(&mut session).await.unwrap();
    assert!(offered.list_status);
    let (hist_tx, _hist_rx) = mpsc::channel(1);
    let remaining = run_cycle(
        &ctx,
        &config,
        &mut session,
        &hist_tx,
        &mut None,
        &mut listed_now(),
        CycleMode {
            cycle: 1,
            foreground_only: false,
            immediate_follow_up: false,
            list_status: offered.list_status,
            inbox_pushed: PushedChanges::default(),
        },
    )
    .await
    .unwrap();
    assert!(!remaining);
    imap::logout(session).await;
    server.await.unwrap();

    let commands = commands.lock().unwrap().clone();
    assert!(
        !commands.iter().any(|command| command.contains(" STATUS ")),
        "LIST-STATUS answered for every folder: {commands:?}"
    );
    assert!(
        !commands.iter().any(|command| command.contains("UNDELETED")),
        "nothing was removed, so nothing is searched for removals: {commands:?}"
    );
    ctx.db
        .read(move |conn| {
            assert!(repo::messages::get_row(conn, message_id)?.unwrap().is_read);
            let statuses = repo::folders::server_statuses(conn, account_id)?;
            assert_eq!(statuses[&lists_id].unseen, Some(4));
            assert_eq!(statuses[&archive_id].messages, Some(20));
            assert_eq!(statuses[&inbox_id].unseen, Some(1));
            Ok(())
        })
        .await
        .unwrap();
}

/// A folder list read just now, so a cycle doesn't read it again.
fn listed_now() -> FolderListing {
    FolderListing {
        listed_at: Some(tokio::time::Instant::now()),
        ..FolderListing::default()
    }
}

/// A folder synced up to `uid_next`, with the counts the server last gave.
fn synced_folder(
    conn: &rusqlite::Connection,
    account_id: i64,
    path: &str,
    uid_next: i64,
    messages: i64,
    unseen: i64,
) -> crate::Result<i64> {
    let id = repo::folders::upsert(conn, account_id, path, Some("/"), None)?;
    repo::folders::set_uid_state(conn, id, Some(7), Some(uid_next), None)?;
    repo::folders::set_last_seen_uid(conn, id, uid_next - 1)?;
    repo::folders::set_backfill(conn, id, Some(1), true)?;
    repo::folders::set_server_counts(conn, id, Some(messages), Some(unseen))?;
    Ok(id)
}

/// Inserts an unread message with `uid` in `folder_id`.
fn seed_unread(
    conn: &rusqlite::Connection,
    account_id: i64,
    folder_id: i64,
    uid: i64,
) -> crate::Result<i64> {
    let thread_id = repo::threads::create(conn, account_id, None, "digest")?;
    let message_id = repo::messages::insert(
        conn,
        &NewMessage {
            account_id,
            folder_id,
            uid: Some(uid),
            message_id: Some(format!("{uid}@example.test")),
            gm_msgid: None,
            gm_thrid: None,
            subject: "Digest".into(),
            from: Some(Address {
                name: None,
                email: "list@example.test".into(),
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
            is_automated: true,
            has_attachments: false,
            size: None,
            snippet: String::new(),
            references: Vec::new(),
            list_unsubscribe: None,
            list_unsubscribe_post: None,
            list_id: None,
            sender_addr: None,
            sender_verification: SenderVerification::None,
            receipt_to: Vec::new(),
            receipt_confirm: false,
            receipt_handled: false,
            importance: Default::default(),
        },
        thread_id,
    )?;
    repo::threads::recompute(conn, thread_id)?;
    Ok(message_id)
}

/// Serves one connection until LOGOUT, logging each command in `commands`
/// and answering it with `respond`, which gets the command and its words.
fn serve(
    listener: tokio::net::TcpListener,
    commands: Arc<Mutex<Vec<String>>>,
    mut respond: impl FnMut(&str, &[&str]) -> String + Send + 'static,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut stream = BufReader::new(tls_acceptor().accept(tcp).await.unwrap());
        stream.write_all(b"* OK fixture ready\r\n").await.unwrap();
        loop {
            let command = read_line(&mut stream).await;
            if command.is_empty() {
                break;
            }
            commands.lock().unwrap().push(command.clone());
            let fields: Vec<_> = command.split_whitespace().collect();
            if fields[1] == "LOGOUT" {
                let tag = fields[0];
                stream
                    .write_all(format!("* BYE closing\r\n{tag} OK logout\r\n").as_bytes())
                    .await
                    .unwrap();
                break;
            }
            let response = respond(&command, &fields);
            stream.write_all(response.as_bytes()).await.unwrap();
        }
    })
}

/// The mailboxes `commands` selected, in order.
fn selected_folders(commands: &[String]) -> Vec<&str> {
    commands
        .iter()
        .filter(|command| command.split_whitespace().nth(1) == Some("SELECT"))
        .filter_map(|command| command.split('"').nth(1))
        .collect()
}

/// Gmail renames its folders when its language changes, and the old names
/// then answer `NO [NONEXISTENT]` (issue #215). That one folder must not
/// stop the cycle: the folders after it still sync on the same connection,
/// and the folder list is read again, so the old folder goes away and the
/// folder under its new name syncs straight away.
#[tokio::test]
async fn a_folder_the_server_no_longer_has_does_not_stop_the_cycle() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let commands = Arc::new(Mutex::new(Vec::<String>::new()));
    let mut selected = "";
    let server = serve(listener, commands.clone(), move |command, fields| {
        let tag = fields[0];
        match fields[1] {
            "LOGIN" => format!("{tag} OK logged in\r\n"),
            "CAPABILITY" => format!("* CAPABILITY IMAP4rev1 IDLE\r\n{tag} OK capability\r\n"),
            "STATUS" | "SELECT" if command.contains("\"Gone\"") => {
                selected = "";
                format!(
                    "{tag} NO [NONEXISTENT] Unknown Mailbox: Gone \
                     (now in authenticated state) (Failure)\r\n"
                )
            }
            "STATUS" if command.contains("\"Projects\"") => format!(
                "* STATUS Projects (MESSAGES 50 UIDNEXT 51 UNSEEN 4 UIDVALIDITY 7)\r\n\
                 {tag} OK status\r\n"
            ),
            "STATUS" => format!(
                "* STATUS INBOX (MESSAGES 3 UIDNEXT 10 UNSEEN 0 UIDVALIDITY 7)\r\n\
                 {tag} OK status\r\n"
            ),
            "SELECT" => {
                let (name, exists, uid_next) = if command.contains("\"Projects\"") {
                    ("Projects", 50, 51)
                } else {
                    ("INBOX", 3, 10)
                };
                selected = name;
                format!(
                    "* {exists} EXISTS\r\n* OK [UIDVALIDITY 7] valid\r\n\
                     * OK [UIDNEXT {uid_next}] next\r\n{tag} OK [READ-WRITE] selected\r\n"
                )
            }
            "UID" if fields[2] == "FETCH" && selected == "Projects" => {
                format!("* 1 FETCH (UID 50 FLAGS (\\Seen))\r\n{tag} OK fetched\r\n")
            }
            "UID" if fields[2] == "FETCH" => format!("{tag} OK fetched\r\n"),
            // Gone is listed no more; Renamed is what it became.
            "LIST" => format!(
                "* LIST (\\HasNoChildren) \"/\" INBOX\r\n\
                 * LIST (\\HasNoChildren) \"/\" Projects\r\n\
                 * LIST (\\HasNoChildren) \"/\" Renamed\r\n\
                 {tag} OK list completed\r\n"
            ),
            other => panic!("unexpected command {other}: {command:?}"),
        }
    });

    let (_dir, ctx, config, inbox) = deleted_mail_context(port).await;
    let account_id = config.id;
    let inbox_id = inbox.id;
    let (gone_id, message_id) = ctx
        .db
        .write(move |conn| {
            repo::folders::set_uid_state(conn, inbox_id, Some(7), Some(10), None)?;
            repo::folders::set_last_seen_uid(conn, inbox_id, 9)?;
            // Synced before Projects: the old name comes first.
            let gone_id = synced_folder(conn, account_id, "Gone", 21, 20, 0)?;
            let projects_id = synced_folder(conn, account_id, "Projects", 51, 50, 5)?;
            let message_id = seed_unread(conn, account_id, projects_id, 50)?;
            Ok((gone_id, message_id))
        })
        .await
        .unwrap();

    let mut session = connect(&ctx, &config).await.unwrap();
    let offered = imap::capabilities(&mut session).await.unwrap();
    let (hist_tx, _hist_rx) = mpsc::channel(1);
    let follow_up = match run_cycle(
        &ctx,
        &config,
        &mut session,
        &hist_tx,
        &mut None,
        &mut listed_now(),
        CycleMode {
            cycle: 1,
            foreground_only: false,
            immediate_follow_up: false,
            list_status: offered.list_status,
            inbox_pushed: PushedChanges::default(),
        },
    )
    .await
    {
        Ok(follow_up) => follow_up,
        Err(error) => panic!("the cycle stopped at the missing folder: {error}"),
    };
    assert!(
        follow_up,
        "the folder under its new name waits for a later cycle"
    );
    imap::logout(session).await;
    server.await.unwrap();

    let commands = commands.lock().unwrap().clone();
    assert_eq!(selected_folders(&commands), ["INBOX", "Gone", "Projects"]);
    let refused = commands
        .iter()
        .position(|command| command.contains("SELECT \"Gone\""))
        .unwrap();
    assert!(
        commands[refused..]
            .iter()
            .any(|command| command.contains(" LIST ")),
        "the folder list wasn't read again: {commands:?}"
    );
    ctx.db
        .read(move |conn| {
            assert!(repo::messages::get_row(conn, message_id)?.unwrap().is_read);
            let listed = repo::folders::list(conn, Some(account_id))?
                .into_iter()
                .map(|folder| (folder.id, folder.path))
                .collect::<Vec<_>>();
            assert!(
                listed.iter().all(|(id, _)| *id != gone_id),
                "the folder the server no longer lists is still shown: {listed:?}"
            );
            assert!(
                listed.iter().any(|(_, path)| path == "Renamed"),
                "{listed:?}"
            );
            Ok(())
        })
        .await
        .unwrap();
}

/// A folder the server lists but won't open, such as one without read
/// rights, is skipped as well. The folder list then still has it, so the
/// next refusals don't read the list again before it is due.
#[tokio::test]
async fn a_refused_folder_the_server_still_lists_is_not_listed_again() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let commands = Arc::new(Mutex::new(Vec::<String>::new()));
    let server = serve(listener, commands.clone(), |command, fields| {
        let tag = fields[0];
        match fields[1] {
            "LOGIN" => format!("{tag} OK logged in\r\n"),
            "CAPABILITY" => format!("* CAPABILITY IMAP4rev1 IDLE\r\n{tag} OK capability\r\n"),
            "STATUS" | "SELECT" if command.contains("\"Locked\"") => {
                format!("{tag} NO [NOPERM] Permission denied\r\n")
            }
            "STATUS" => format!(
                "* STATUS INBOX (MESSAGES 3 UIDNEXT 10 UNSEEN 0 UIDVALIDITY 7)\r\n\
                 {tag} OK status\r\n"
            ),
            "SELECT" => format!(
                "* 3 EXISTS\r\n* OK [UIDVALIDITY 7] valid\r\n\
                 * OK [UIDNEXT 10] next\r\n{tag} OK [READ-WRITE] selected\r\n"
            ),
            "UID" if fields[2] == "FETCH" => format!("{tag} OK fetched\r\n"),
            "LIST" => format!(
                "* LIST (\\HasNoChildren) \"/\" INBOX\r\n\
                 * LIST (\\HasNoChildren) \"/\" Locked\r\n\
                 {tag} OK list completed\r\n"
            ),
            other => panic!("unexpected command {other}: {command:?}"),
        }
    });

    let (_dir, ctx, config, inbox) = deleted_mail_context(port).await;
    let account_id = config.id;
    let inbox_id = inbox.id;
    let locked_id = ctx
        .db
        .write(move |conn| {
            repo::folders::set_uid_state(conn, inbox_id, Some(7), Some(10), None)?;
            repo::folders::set_last_seen_uid(conn, inbox_id, 9)?;
            synced_folder(conn, account_id, "Locked", 21, 20, 0)
        })
        .await
        .unwrap();

    let mut session = connect(&ctx, &config).await.unwrap();
    let offered = imap::capabilities(&mut session).await.unwrap();
    let (hist_tx, _hist_rx) = mpsc::channel(1);
    // Never listed yet: the first cycle reads the folder list first thing.
    let mut listing = FolderListing::default();
    for cycle in 1..=2 {
        let result = run_cycle(
            &ctx,
            &config,
            &mut session,
            &hist_tx,
            &mut None,
            &mut listing,
            CycleMode {
                cycle,
                foreground_only: false,
                immediate_follow_up: false,
                list_status: offered.list_status,
                inbox_pushed: PushedChanges::default(),
            },
        )
        .await;
        assert!(matches!(result, Ok(false)), "cycle {cycle}: {result:?}");
    }
    imap::logout(session).await;
    server.await.unwrap();

    let commands = commands.lock().unwrap().clone();
    assert_eq!(
        selected_folders(&commands),
        ["INBOX", "Locked", "INBOX", "Locked"]
    );
    let lists = commands
        .iter()
        .filter(|command| command.contains(" LIST "))
        .count();
    assert_eq!(lists, 1, "{commands:?}");
    let locked_listed = ctx
        .db
        .read(move |conn| {
            Ok(repo::folders::list(conn, Some(account_id))?
                .iter()
                .any(|folder| folder.id == locked_id))
        })
        .await
        .unwrap();
    assert!(locked_listed);
}

#[test]
fn the_folder_list_is_read_first_and_then_every_half_hour() {
    let start = tokio::time::Instant::now();
    let mut listing = FolderListing::default();
    assert!(listing.due(start));
    listing.listed_at = Some(start);
    assert!(!listing.due(start + FOLDER_LIST_INTERVAL - std::time::Duration::from_secs(1)));
    assert!(listing.due(start + FOLDER_LIST_INTERVAL));
}

#[test]
fn only_a_folder_refused_anew_calls_for_the_folder_list() {
    let mut listing = FolderListing::default();
    assert!(!listing.recheck(&[]));
    assert!(listing.recheck(&[7]));
    listing.refused_but_listed.insert(7);
    assert!(!listing.recheck(&[7]));
    assert!(listing.recheck(&[7, 8]));
}

/// The folders the body worker selects for empty batches from `batches`,
/// one after another on one connection. The server refuses Gone, and has
/// made Reset afresh since its mail was queued.
async fn body_batch_selects(batches: &[&str]) -> Vec<String> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let commands = Arc::new(Mutex::new(Vec::<String>::new()));
    let server = serve(listener, commands.clone(), |command, fields| {
        let tag = fields[0];
        match fields[1] {
            "LOGIN" => format!("{tag} OK logged in\r\n"),
            "CAPABILITY" => format!("* CAPABILITY IMAP4rev1 IDLE\r\n{tag} OK capability\r\n"),
            "SELECT" if command.contains("\"Gone\"") => {
                format!("{tag} NO [NONEXISTENT] Unknown Mailbox: Gone\r\n")
            }
            "SELECT" => {
                let uid_validity = if command.contains("\"Reset\"") { 9 } else { 7 };
                format!(
                    "* 3 EXISTS\r\n* OK [UIDVALIDITY {uid_validity}] valid\r\n\
                     * OK [UIDNEXT 10] next\r\n{tag} OK [READ-WRITE] selected\r\n"
                )
            }
            other => panic!("unexpected command {other}: {command:?}"),
        }
    });

    let (_dir, ctx, config, _inbox) = deleted_mail_context(port).await;
    let account_id = config.id;
    let folders = ctx
        .db
        .write(move |conn| {
            let mut folders = HashMap::new();
            for name in ["Lists", "Gone", "Reset"] {
                folders.insert(name, synced_folder(conn, account_id, name, 10, 3, 0)?);
            }
            repo::folders::set_uid_state(conn, folders["Reset"], Some(8), Some(10), None)?;
            Ok(folders)
        })
        .await
        .unwrap();

    let mut session = connect(&ctx, &config).await.unwrap();
    let mut selected = None;
    for name in batches {
        let chunk = BodyChunk {
            folder_id: folders[name],
            folder_name: (*name).into(),
            uid_validity: Some(if *name == "Reset" { 8 } else { 7 }),
            items: Vec::new(),
        };
        let fetched = fetch_body_chunk(&ctx, &config, &mut session, &mut selected, &chunk).await;
        assert!(matches!(fetched, Ok(0)), "{name}");
    }
    imap::logout(session).await;
    server.await.unwrap();
    let commands = commands.lock().unwrap().clone();
    selected_folders(&commands)
        .into_iter()
        .map(str::to_owned)
        .collect()
}

/// The body worker skips a batch from a folder the server turns down,
/// rather than splitting it over new connections, and the next batch
/// selects its folder again.
#[tokio::test]
async fn a_body_batch_from_a_refused_folder_is_skipped() {
    assert_eq!(
        body_batch_selects(&["Lists", "Gone", "Lists"]).await,
        ["Lists", "Gone", "Lists"]
    );
}

/// After opening a folder to find its UIDVALIDITY changed, the next batch
/// of the folder open before selects it again instead of reading its UIDs
/// in the other folder.
#[tokio::test]
async fn a_body_batch_after_a_new_uidvalidity_selects_its_folder_again() {
    assert_eq!(
        body_batch_selects(&["Lists", "Reset", "Lists"]).await,
        ["Lists", "Reset", "Lists"]
    );
}

/// History downloads go on past a folder the server turns down, on the same
/// connection.
#[tokio::test]
async fn history_backfill_goes_on_past_a_refused_folder() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let commands = Arc::new(Mutex::new(Vec::<String>::new()));
    let server = serve(listener, commands.clone(), |command, fields| {
        let tag = fields[0];
        match fields[1] {
            "LOGIN" => format!("{tag} OK logged in\r\n"),
            "CAPABILITY" => format!("* CAPABILITY IMAP4rev1 IDLE\r\n{tag} OK capability\r\n"),
            "SELECT" if command.contains("\"Gone\"") => {
                format!("{tag} NO [NONEXISTENT] Unknown Mailbox: Gone\r\n")
            }
            "SELECT" => format!(
                "* 3 EXISTS\r\n* OK [UIDVALIDITY 7] valid\r\n\
                 * OK [UIDNEXT 10] next\r\n{tag} OK [READ-WRITE] selected\r\n"
            ),
            other => panic!("unexpected command {other}: {command:?}"),
        }
    });

    let (_dir, ctx, config, _inbox) = deleted_mail_context(port).await;
    let account_id = config.id;
    // Both still need their history; Gone comes first.
    ctx.db
        .write(move |conn| {
            repo::folders::upsert(conn, account_id, "Gone", Some("/"), None)?;
            repo::folders::upsert(conn, account_id, "Projects", Some("/"), None)?;
            Ok(())
        })
        .await
        .unwrap();

    let (nudge, rx) = mpsc::channel(1);
    let (pool_tx, _pool_rx) = mpsc::channel(1);
    let (_settings, settings_rx) = watch::channel(config.settings.clone());
    let worker = tokio::spawn(run_history_backfill(
        ctx.clone(),
        config.clone(),
        rx,
        pool_tx,
        settings_rx,
    ));
    nudge.send(()).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), server)
        .await
        .expect("the history worker never signed out")
        .unwrap();
    drop(nudge);
    tokio::time::timeout(std::time::Duration::from_secs(5), worker)
        .await
        .expect("the history worker didn't stop")
        .unwrap();

    let commands = commands.lock().unwrap().clone();
    assert_eq!(selected_folders(&commands), ["Gone", "Projects"]);
    let sign_ins = commands
        .iter()
        .filter(|command| command.contains(" LOGIN "))
        .count();
    assert_eq!(sign_ins, 1, "{commands:?}");
}

/// A server search brings a page of mail not kept here at a time, newest
/// first across the folders it covers, with the matches kept here that the
/// page passes. A page stops in each folder at one UID and the next goes on
/// below it, so no match is missed or found twice. Found mail isn't new
/// mail: the folders' sync positions stay where they were.
#[tokio::test]
async fn a_server_search_pages_through_the_folders_newest_first() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let commands = Arc::new(Mutex::new(Vec::<String>::new()));
    // Each folder's matches and the day in March 2020 the server got them.
    let inbox: &[(u32, u32)] = &[(30, 5), (20, 3), (10, 1)];
    let archive: &[(u32, u32)] = &[(7, 4), (5, 2)];
    let mut selected = "";
    let server = serve(listener, commands.clone(), move |command, fields| {
        let tag = fields[0];
        match fields[1] {
            "LOGIN" => format!("{tag} OK logged in\r\n"),
            "CAPABILITY" => format!("* CAPABILITY IMAP4rev1 IDLE\r\n{tag} OK capability\r\n"),
            "SELECT" => {
                selected = ["INBOX", "Archive"]
                    .into_iter()
                    .find(|name| command.contains(&format!("\"{name}\"")))
                    .unwrap_or_else(|| panic!("searched another folder: {command:?}"));
                format!(
                    "* 40 EXISTS\r\n* OK [UIDVALIDITY 7] valid\r\n\
                     * OK [UIDNEXT 41] next\r\n{tag} OK [READ-WRITE] selected\r\n"
                )
            }
            "UID" if fields[2] == "SEARCH" => {
                assert_eq!(
                    command.trim_end(),
                    format!("{tag} UID SEARCH (TEXT \"invoice\" FROM \"ana\")")
                );
                let found = if selected == "INBOX" { inbox } else { archive };
                let uids = found.iter().map(|(uid, _)| uid.to_string());
                format!(
                    "* SEARCH {}\r\n{tag} OK searched\r\n",
                    uids.collect::<Vec<_>>().join(" ")
                )
            }
            "UID" if fields[2] == "FETCH" => {
                let found = if selected == "INBOX" { inbox } else { archive };
                let asked = fields[3]
                    .split(',')
                    .map(|uid| uid.parse::<u32>().unwrap())
                    .collect::<Vec<_>>();
                let mut response = String::new();
                for (index, (uid, day)) in found
                    .iter()
                    .filter(|(uid, _)| asked.contains(uid))
                    .enumerate()
                {
                    let arrived = format!("INTERNALDATE \"{day:02}-Mar-2020 10:00:00 +0000\"");
                    if command.contains("HEADER.FIELDS") {
                        let header = format!(
                            "Message-ID: <{selected}-{uid}@example.test>\r\n\
                             From: Ana <ana@example.test>\r\n\
                             To: reader@example.test\r\n\
                             Subject: Invoice {uid}\r\n\
                             Date: {day} Mar 2020 10:00:00 +0000\r\n\r\n"
                        );
                        response.push_str(&format!(
                            "* {} FETCH (UID {uid} FLAGS (\\Seen) {arrived} RFC822.SIZE 200 \
                             BODY[HEADER.FIELDS (MESSAGE-ID FROM TO SUBJECT DATE)] {{{}}}\r\n\
                             {header})\r\n",
                            index + 1,
                            header.len()
                        ));
                    } else {
                        response
                            .push_str(&format!("* {} FETCH (UID {uid} {arrived})\r\n", index + 1));
                    }
                }
                format!("{response}{tag} OK fetched\r\n")
            }
            other => panic!("unexpected command {other}: {command:?}"),
        }
    });

    let (_dir, ctx, config, inbox_folder) = deleted_mail_context(port).await;
    let account_id = config.id;
    let inbox_id = inbox_folder.id;
    let (archive_id, kept_id) = ctx
        .db
        .write(move |conn| {
            let archive_id = repo::folders::upsert(
                conn,
                account_id,
                "Archive",
                Some("/"),
                Some(roles::ARCHIVE),
            )?;
            // Left out unless a search names it.
            repo::folders::upsert(conn, account_id, "Trash", Some("/"), Some(roles::TRASH))?;
            let kept_id = seed_unread(conn, account_id, inbox_id, 30)?;
            Ok((archive_id, kept_id))
        })
        .await
        .unwrap();

    let tree = crate::search::parse("invoice from:ana").tree.unwrap();
    let mut session = connect(&ctx, &config).await.unwrap();
    let first = search_server(
        &ctx,
        &config,
        &mut session,
        &tree,
        &ServerSearchCursor::default(),
        3,
    )
    .await
    .unwrap();
    let next = first
        .next
        .clone()
        .expect("the search stopped after one page");
    let mut below = next.below.clone();
    below.sort_unstable();
    // Archive has nothing new left.
    let mut expected = vec![(inbox_id, 20), (archive_id, 5)];
    expected.sort_unstable();
    assert_eq!(below, expected);
    let second = search_server(&ctx, &config, &mut session, &tree, &next, 3)
        .await
        .unwrap();
    assert_eq!(second.next, None);
    imap::logout(session).await;
    server.await.unwrap();

    let commands = commands.lock().unwrap().clone();
    let mut headers_fetched = commands
        .iter()
        .filter(|command| command.contains("HEADER.FIELDS"))
        .map(|command| command.split_whitespace().nth(3).unwrap())
        .collect::<Vec<_>>();
    headers_fetched.sort_unstable();
    // 30 is kept here already.
    assert_eq!(headers_fetched, ["10", "20", "5,7"]);
    assert!(
        selected_folders(&commands)
            .iter()
            .all(|name| *name != "Trash")
    );

    assert!(first.message_ids.contains(&kept_id));
    let (first_found, second_found, archive_seen) = ctx
        .db
        .read(move |conn| {
            let place = |ids: &[i64]| -> crate::Result<Vec<(i64, i64)>> {
                let mut places = ids
                    .iter()
                    .map(|id| {
                        let row = repo::messages::get_row(conn, *id)?.unwrap();
                        Ok((row.folder_id.unwrap(), row.uid.unwrap()))
                    })
                    .collect::<crate::Result<Vec<_>>>()?;
                places.sort_unstable();
                Ok(places)
            };
            Ok((
                place(&first.message_ids)?,
                place(&second.message_ids)?,
                repo::folders::get(conn, archive_id)?.unwrap().last_seen_uid,
            ))
        })
        .await
        .unwrap();
    // The newest three not kept here, and 30 on the way.
    let mut expected = vec![
        (inbox_id, 30),
        (inbox_id, 20),
        (archive_id, 7),
        (archive_id, 5),
    ];
    expected.sort_unstable();
    assert_eq!(first_found, expected);
    assert_eq!(second_found, [(inbox_id, 10)]);
    assert_eq!(
        archive_seen, 0,
        "found mail moved the folder's sync position"
    );
}

/// Issue #371: a server can list thousands of shared folders. With
/// subscribed folders only, the account lists the folders the user
/// subscribed to, the Inbox, a folder for each purpose mail is filed into,
/// and those its rules file into. The others go from this device with their
/// mail, unless queued work still involves them, and LIST-STATUS asks the
/// server about the listed folders alone.
#[tokio::test]
async fn subscribed_folders_only_lists_and_syncs_just_those() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let commands = Arc::new(Mutex::new(Vec::<String>::new()));
    let server = serve(listener, commands.clone(), move |command, fields| {
        let tag = fields[0];
        match fields[1] {
            "LOGIN" => format!("{tag} OK logged in\r\n"),
            "CAPABILITY" => format!(
                "* CAPABILITY IMAP4rev1 LIST-EXTENDED LIST-STATUS\r\n{tag} OK capability\r\n"
            ),
            "LIST" if command.contains(" RETURN ") => format!(
                "* LIST () \"/\" INBOX\r\n\
                 * STATUS INBOX (MESSAGES 1 UIDNEXT 2 UNSEEN 0 UIDVALIDITY 7)\r\n\
                 {tag} OK list completed\r\n"
            ),
            "LIST" => format!(
                "* LIST (\\HasNoChildren) \"/\" INBOX\r\n\
                 * LIST (\\HasNoChildren) \"/\" Projects\r\n\
                 * LIST (\\Noselect \\HasChildren) \"/\" Shared\r\n\
                 * LIST (\\HasChildren) \"/\" \"Shared/alice\"\r\n\
                 * LIST (\\HasNoChildren) \"/\" \"Shared/alice/Reports\"\r\n\
                 * LIST (\\HasNoChildren) \"/\" \"Shared/alice/Trash\"\r\n\
                 * LIST (\\HasNoChildren) \"/\" \"Shared/bob\"\r\n\
                 * LIST (\\HasNoChildren \\Trash) \"/\" Bin\r\n\
                 * LIST (\\HasNoChildren) \"/\" Receipts\r\n\
                 * LIST (\\HasNoChildren) \"/\" Busy\r\n\
                 {tag} OK list completed\r\n"
            ),
            // A subscription can outlive its folder.
            "LSUB" => format!(
                "* LSUB () \"/\" Projects\r\n\
                 * LSUB () \"/\" \"Shared/alice/Reports\"\r\n\
                 * LSUB () \"/\" Gone\r\n\
                 {tag} OK lsub completed\r\n"
            ),
            other => panic!("unexpected command {other}: {command:?}"),
        }
    });

    let (_dir, ctx, config, _inbox) = deleted_mail_context(port).await;
    let account_id = config.id;
    let (alice_mail, bob_id, bob_mail, busy_id) = ctx
        .db
        .write(move |conn| {
            let alice_id = synced_folder(conn, account_id, "Shared/alice", 2, 1, 1)?;
            let alice_mail = seed_unread(conn, account_id, alice_id, 1)?;
            let bob_id = synced_folder(conn, account_id, "Shared/bob", 2, 1, 1)?;
            let bob_mail = seed_unread(conn, account_id, bob_id, 1)?;
            let busy_id = synced_folder(conn, account_id, "Busy", 2, 1, 1)?;
            let busy_mail = seed_unread(conn, account_id, busy_id, 1)?;
            repo::actions::enqueue(
                conn,
                account_id,
                "markRead",
                Some(busy_mail),
                None,
                &serde_json::json!({}),
                None,
            )?;
            let receipts_id = synced_folder(conn, account_id, "Receipts", 2, 1, 1)?;
            repo::rules::save(
                conn,
                &MailRule {
                    id: 0,
                    account_id: Some(account_id),
                    name: "Receipts".into(),
                    enabled: false,
                    match_all: true,
                    conditions: Vec::new(),
                    actions: vec![RuleAction::Move {
                        folder: receipts_id,
                    }],
                    stop: true,
                },
                0,
            )?;
            Ok((alice_mail, bob_id, bob_mail, busy_id))
        })
        .await
        .unwrap();
    let mut events = ctx.bus.subscribe();

    let mut session = connect(&ctx, &config).await.unwrap();
    discover_folders(&ctx, &config, &mut session, true)
        .await
        .unwrap();
    let listed = ctx
        .db
        .read(move |conn| repo::folders::list(conn, Some(account_id)))
        .await
        .unwrap();
    let mut paths = listed
        .iter()
        .map(|folder| (folder.path.as_str(), folder.selectable))
        .collect::<Vec<_>>();
    paths.sort();
    assert_eq!(
        paths,
        [
            ("Bin", true),
            // Queued work still involves it.
            ("Busy", true),
            ("INBOX", true),
            ("Projects", true),
            ("Receipts", true),
            ("Shared", false),
            // Only the container of a subscribed folder now.
            ("Shared/alice", false),
            ("Shared/alice/Reports", true),
        ]
    );
    assert!(listed.iter().any(|folder| folder.id == busy_id));
    assert!(matches!(events.try_recv(), Ok(CoreEvent::SyncProgress(_))));
    assert!(matches!(
        events.try_recv(),
        Ok(CoreEvent::MailUpdated { .. })
    ));
    ctx.db
        .read(move |conn| {
            assert!(repo::messages::get_row(conn, alice_mail)?.is_none());
            assert!(repo::messages::get_row(conn, bob_mail)?.is_none());
            assert!(repo::folders::get(conn, bob_id)?.is_none());
            Ok(())
        })
        .await
        .unwrap();

    // Each cycle asks about the listed folders alone.
    let synced = listed
        .iter()
        .filter(|folder| folder.selectable)
        .collect::<Vec<_>>();
    let reported = report_folders(&mut session, &synced, true, true).await;
    assert!(reported.contains_key("INBOX"));

    // Showing every folder again lists the others anew.
    discover_folders(&ctx, &config, &mut session, false)
        .await
        .unwrap();
    imap::logout(session).await;
    server.await.unwrap();
    let listed = ctx
        .db
        .read(move |conn| repo::folders::list(conn, Some(account_id)))
        .await
        .unwrap();
    assert!(listed.iter().any(|folder| folder.path == "Shared/bob"));
    assert!(
        listed
            .iter()
            .any(|folder| folder.path == "Shared/alice/Trash")
    );

    let commands = commands.lock().unwrap().clone();
    let status = commands
        .iter()
        .find(|command| command.contains(" RETURN "))
        .unwrap();
    assert!(
        status.starts_with(&format!(
            "{} LIST \"\" (",
            status.split_whitespace().next().unwrap()
        )),
        "{status:?}"
    );
    for path in [
        "INBOX",
        "Projects",
        "Shared/alice/Reports",
        "Bin",
        "Receipts",
        "Busy",
    ] {
        assert!(status.contains(&format!("\"{path}\"")), "{status:?}");
    }
    assert!(
        !status.contains("bob") && !status.contains('*'),
        "{status:?}"
    );
    assert_eq!(
        commands
            .iter()
            .filter(|command| command.split_whitespace().nth(1) == Some("LSUB"))
            .count(),
        1,
        "only the subscribed-only listing reads subscriptions: {commands:?}"
    );
}

/// A folder chosen for a role such as Sent keeps it each time the folder
/// list is read, and is listed with subscribed folders only though not
/// subscribed (#252).
#[tokio::test]
async fn folders_chosen_for_roles_keep_them_when_folders_are_listed() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let commands = Arc::new(Mutex::new(Vec::<String>::new()));
    let server = serve(listener, commands, move |command, fields| {
        let tag = fields[0];
        match fields[1] {
            "LOGIN" => format!("{tag} OK logged in\r\n"),
            "CAPABILITY" => format!("* CAPABILITY IMAP4rev1\r\n{tag} OK capability\r\n"),
            "LSUB" => format!("* LSUB () \"/\" Projects\r\n{tag} OK lsub completed\r\n"),
            "LIST" => format!(
                "* LIST (\\HasNoChildren) \"/\" INBOX\r\n\
                 * LIST (\\HasNoChildren \\Sent) \"/\" Sent\r\n\
                 * LIST (\\HasNoChildren) \"/\" Gesendet\r\n\
                 * LIST (\\HasNoChildren) \"/\" Projects\r\n\
                 * LIST (\\HasNoChildren) \"/\" Shared\r\n\
                 {tag} OK list completed\r\n"
            ),
            other => panic!("unexpected command {other}: {command:?}"),
        }
    });
    let (_dir, ctx, config, _inbox) = deleted_mail_context(port).await;
    let account_id = config.id;
    ctx.db
        .write(move |conn| {
            let mut config = repo::accounts::get_config(conn, account_id)?.unwrap();
            config
                .settings
                .folder_roles
                .insert(roles::SENT.into(), "Gesendet".into());
            repo::accounts::set_settings(conn, account_id, &config.settings)
        })
        .await
        .unwrap();
    let mut session = connect(&ctx, &config).await.unwrap();
    // Each reading gives the folders the server's roles first.
    for _ in 0..2 {
        discover_folders(&ctx, &config, &mut session, true)
            .await
            .unwrap();
    }
    imap::logout(session).await;
    server.await.unwrap();
    let (sent, mut paths) = ctx
        .db
        .read(move |conn| {
            Ok((
                repo::folders::by_role(conn, account_id, roles::SENT)?.map(|folder| folder.path),
                repo::folders::list(conn, Some(account_id))?
                    .into_iter()
                    .map(|folder| folder.path)
                    .collect::<Vec<_>>(),
            ))
        })
        .await
        .unwrap();
    assert_eq!(sent.as_deref(), Some("Gesendet"));
    paths.sort();
    assert_eq!(paths, ["Gesendet", "INBOX", "Projects", "Sent"]);
}

/// The folders listed with subscribed folders only from a server that turns
/// down LSUB, which RFC 9051 deprecates, and answers LIST (SUBSCRIBED) or
/// not, by path, with the commands it got.
async fn listed_without_lsub(list_subscribed: bool) -> (Vec<String>, Vec<String>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let commands = Arc::new(Mutex::new(Vec::<String>::new()));
    let server = serve(listener, commands.clone(), move |command, fields| {
        let tag = fields[0];
        match fields[1] {
            "LOGIN" => format!("{tag} OK logged in\r\n"),
            "CAPABILITY" => {
                format!("* CAPABILITY IMAP4rev1 IMAP4rev2 LIST-EXTENDED\r\n{tag} OK capability\r\n")
            }
            "LSUB" => format!("{tag} BAD unknown command\r\n"),
            "LIST" if command.contains("(SUBSCRIBED)") && list_subscribed => format!(
                "* LIST (\\Subscribed) \"/\" Projects\r\n\
                 * LIST (\\Subscribed \\NonExistent) \"/\" Archive\r\n\
                 {tag} OK list completed\r\n"
            ),
            "LIST" if command.contains("(SUBSCRIBED)") => {
                format!("{tag} BAD unknown selection option\r\n")
            }
            "LIST" => format!(
                "* LIST (\\HasNoChildren) \"/\" INBOX\r\n\
                 * LIST (\\HasNoChildren) \"/\" Projects\r\n\
                 * LIST (\\HasNoChildren) \"/\" Archive2\r\n\
                 * LIST (\\HasNoChildren) \"/\" Shared\r\n\
                 {tag} OK list completed\r\n"
            ),
            other => panic!("unexpected command {other}: {command:?}"),
        }
    });
    let (_dir, ctx, config, _inbox) = deleted_mail_context(port).await;
    let account_id = config.id;
    let mut session = connect(&ctx, &config).await.unwrap();
    discover_folders(&ctx, &config, &mut session, true)
        .await
        .unwrap();
    imap::logout(session).await;
    server.await.unwrap();
    let mut paths = ctx
        .db
        .read(move |conn| repo::folders::list(conn, Some(account_id)))
        .await
        .unwrap()
        .into_iter()
        .map(|folder| folder.path)
        .collect::<Vec<_>>();
    paths.sort();
    let commands = commands.lock().unwrap().clone();
    (paths, commands)
}

#[tokio::test]
async fn subscriptions_come_from_list_subscribed_when_lsub_is_refused() {
    let (paths, commands) = listed_without_lsub(true).await;
    assert_eq!(paths, ["INBOX", "Projects"]);
    assert!(
        commands
            .iter()
            .any(|command| command.contains(" LIST (SUBSCRIBED) \"\" \"*\"")),
        "{commands:?}"
    );
}

/// A server that keeps no subscriptions has every folder listed rather than
/// stop the account's sync.
#[tokio::test]
async fn without_subscriptions_on_the_server_every_folder_is_listed() {
    let (paths, _) = listed_without_lsub(false).await;
    assert_eq!(paths, ["Archive2", "INBOX", "Projects", "Shared"]);
}

/// A server that turns down every sign-in with `reply`, counting them.
async fn refusing_server(reply: &'static str) -> (u16, Arc<std::sync::atomic::AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let sign_ins = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = sign_ins.clone();
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let counted = counted.clone();
            tokio::spawn(async move {
                let Ok(tls) = tls_acceptor().accept(tcp).await else {
                    return;
                };
                let mut stream = BufReader::new(tls);
                if stream.write_all(b"* OK fixture ready\r\n").await.is_err() {
                    return;
                }
                loop {
                    let mut line = String::new();
                    if !matches!(stream.read_line(&mut line).await, Ok(1..)) {
                        return;
                    }
                    let fields: Vec<_> = line.split_whitespace().collect();
                    let (Some(tag), Some(command)) = (fields.first(), fields.get(1)) else {
                        return;
                    };
                    let response = match command.to_ascii_uppercase().as_str() {
                        "CAPABILITY" => {
                            format!("* CAPABILITY IMAP4rev1\r\n{tag} OK capability\r\n")
                        }
                        "LOGIN" => {
                            counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            format!("{tag} {reply}\r\n")
                        }
                        "LOGOUT" => format!("* BYE closing\r\n{tag} OK logout\r\n"),
                        _ => format!("{tag} BAD unexpected\r\n"),
                    };
                    if stream.write_all(response.as_bytes()).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    (port, sign_ins)
}

/// Waits until the account waits for its server, and returns until when.
async fn throttled(
    events: &mut tokio::sync::broadcast::Receiver<CoreEvent>,
    account_id: i64,
) -> i64 {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if let Ok(CoreEvent::AccountState {
                account_id: id,
                sync_state,
                sync_retry_at: Some(at),
                ..
            }) = events.recv().await
                && id == account_id
                && sync_state == "throttled"
            {
                return at;
            }
        }
    })
    .await
    .expect("the account never waited for its server")
}

/// The sync actor of `config`, with channels to it.
fn spawn_actor(
    ctx: &SyncCtx,
    config: &AccountConfig,
) -> (mpsc::Sender<SyncCmd>, tokio::task::JoinHandle<()>) {
    let (tx, rx) = mpsc::channel(8);
    let (pool_tx, _) = mpsc::channel(1);
    let (hist_tx, _) = mpsc::channel(1);
    let (_, settings_rx) = watch::channel(config.settings.clone());
    let actor = tokio::spawn(run_actor(
        ctx.clone(),
        config.clone(),
        rx,
        pool_tx,
        hist_tx,
        settings_rx,
    ));
    (tx, actor)
}

async fn check_now(tx: &mpsc::Sender<SyncCmd>) -> std::result::Result<(), String> {
    let (done, answer) = oneshot::channel();
    tx.send(SyncCmd::SyncNow {
        complete: Some(done),
    })
    .await
    .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), answer)
        .await
        .expect("the check was never answered")
        .unwrap()
}

#[tokio::test]
async fn an_account_with_too_many_connections_waits_and_opens_fewer() {
    // Gmail's words when an account has too many connections open.
    let (port, sign_ins) =
        refusing_server("NO [ALERT] Too many simultaneous connections. (Failure)").await;
    let (_dir, ctx, config, _) = deleted_mail_context(port).await;
    let account_id = config.id;
    let mut events = ctx.bus.subscribe();
    let (tx, actor) = spawn_actor(&ctx, &config);

    // The account waits a minute, give or take a quarter, and says so.
    let retry_at = throttled(&mut events, account_id).await;
    assert!(
        (44_000..=76_000).contains(&(retry_at - now_ms())),
        "{retry_at}"
    );
    let account = ctx
        .db
        .read(move |conn| repo::accounts::get(conn, account_id))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(account.sync_state, "throttled");
    assert_eq!(account.sync_retry_at, Some(retry_at));
    assert!(
        account
            .sync_error
            .is_some_and(|error| error.contains("Too many simultaneous connections"))
    );
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(sign_ins.load(std::sync::atomic::Ordering::SeqCst), 1);

    // Sync chose that wait, so a check someone asks for goes ahead. The
    // server still refuses, and the next wait is twice as long.
    let answer = check_now(&tx).await.unwrap_err();
    assert!(
        answer.contains("Too many simultaneous connections"),
        "{answer}"
    );
    assert_eq!(sign_ins.load(std::sync::atomic::Ordering::SeqCst), 2);
    let retry_at = throttled(&mut events, account_id).await;
    assert!(
        (89_000..=151_000).contains(&(retry_at - now_ms())),
        "{retry_at}"
    );
    // Each refusal leaves the account one fewer connection for work in the
    // background, down to one.
    assert_eq!(ctx.pace.account(account_id).bulk_allowed(), 1);

    tx.send(SyncCmd::Shutdown).await.unwrap();
    actor.await.unwrap();
}

#[tokio::test]
async fn a_wait_the_server_named_is_kept_when_someone_asks_for_a_check() {
    // Exchange Online names its wait.
    let (port, sign_ins) =
        refusing_server("NO Request is throttled. Suggested Backoff Time: 600000 milliseconds")
            .await;
    let (_dir, ctx, config, _) = deleted_mail_context(port).await;
    let mut events = ctx.bus.subscribe();
    let (tx, actor) = spawn_actor(&ctx, &config);

    let retry_at = throttled(&mut events, config.id).await;
    assert!(
        (599_000..=631_000).contains(&(retry_at - now_ms())),
        "{retry_at}"
    );
    // The check is answered at once with why the account waits, and the
    // server isn't asked again.
    let answer = check_now(&tx).await.unwrap_err();
    assert!(answer.contains("Suggested Backoff Time"), "{answer}");
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(sign_ins.load(std::sync::atomic::Ordering::SeqCst), 1);

    tx.send(SyncCmd::Shutdown).await.unwrap();
    actor.await.unwrap();
}
