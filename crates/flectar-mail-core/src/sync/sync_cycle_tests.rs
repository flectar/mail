//! End-to-end fixtures for scheduling and IMAP compatibility in one sync cycle.
use super::*;
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
                    provider: Provider::Imap,
                    auth_kind: AuthKind::Password,
                    mail_protocol: MailProtocol::Imap,
                    username: "reader@example.test",
                    jmap_url: "",
                    jmap_account_id: None,
                    imap_host: "127.0.0.1",
                    imap_port: port,
                    smtp_host: "127.0.0.1",
                    smtp_port: 465,
                },
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
        imap_security: ConnectionSecurity::Tls,
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
