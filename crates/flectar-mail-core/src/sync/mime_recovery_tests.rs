//! Local TLS transcripts exercise the real async-imap decoder and sync paths.
use super::*;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use std::sync::Mutex;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const CERT: &str = include_str!("../../tests/fixtures/tls/server.pem");
const KEY: &str = include_str!("../../tests/fixtures/tls/server-key.pem");
const TEXT_PLAN: &str = "(\"TEXT\" \"PLAIN\" (\"CHARSET\" \"UTF-8\") NIL NIL \"7BIT\" 6 1)";

#[derive(Clone, Copy)]
enum Behavior {
    Broken,
    Denied,
    Reauth,
    Reset,
    GlobalLegacy,
    GlobalExtended,
}

struct Fixture {
    ctx: SyncCtx,
    config: AccountConfig,
    folder: Folder,
    ids: Vec<i64>,
    commands: Arc<Mutex<Vec<(usize, String)>>>,
    server: tokio::task::JoinHandle<()>,
    _dir: tempfile::TempDir,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
fn raw(uid: u32) -> String {
    format!(
        "Message-ID: <fixture-{uid}@example.test>\r\nFrom: sender@example.test\r\nTo: reader@example.test\r\nSubject: Message {uid}\r\nDate: Tue, 01 Sep 2026 12:00:00 +0000\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nBody {uid}"
    )
}
fn uids(set: &str) -> Vec<u32> {
    set.split(',')
        .flat_map(|part| {
            let mut ends = part.split(':').map(|n| n.parse::<u32>().unwrap());
            let first = ends.next().unwrap();
            first..=ends.next().unwrap_or(first)
        })
        .collect()
}
async fn fixture(behavior: Behavior) -> (Fixture, Option<Session>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let tls = rustls::ServerConfig::builder_with_provider(Arc::new(
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
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls));
    let commands = Arc::new(Mutex::new(Vec::new()));
    let log = commands.clone();
    let server = tokio::spawn(async move {
        for connection in 0.. {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(acceptor.accept(tcp).await.unwrap());
            stream.write_all(b"* OK fixture ready\r\n").await.unwrap();
            loop {
                let mut line = String::new();
                if stream.read_line(&mut line).await.unwrap_or(0) == 0 {
                    break;
                }
                log.lock().unwrap().push((connection, line.clone()));
                let parts: Vec<_> = line.split_whitespace().collect();
                let tag = parts[0];
                let mut response = String::new();
                match parts[1] {
                    "LOGIN" if connection > 0 && matches!(behavior, Behavior::Reauth) => {
                        stream
                            .write_all(
                                format!("{tag} NO [AUTHENTICATIONFAILED] denied\r\n").as_bytes(),
                            )
                            .await
                            .unwrap();
                        continue;
                    }
                    "LOGIN" => {}
                    "SELECT" => {
                        let validity = if connection > 0 && matches!(behavior, Behavior::Reset) {
                            8
                        } else {
                            7
                        };
                        response =
                            format!("* 3 EXISTS\r\n* OK [UIDVALIDITY {validity}] namespace\r\n");
                    }
                    "UID" => {
                        assert_eq!(parts[2], "FETCH");
                        let requested = uids(parts[3]);
                        if line.contains("BODYSTRUCTURE") && matches!(behavior, Behavior::Denied) {
                            stream
                                .write_all(format!("{tag} NO [NOPERM] denied\r\n").as_bytes())
                                .await
                                .unwrap();
                            continue;
                        }
                        for uid in requested {
                            if line.contains("BODYSTRUCTURE") {
                                if uid == 2
                                    && matches!(
                                        behavior,
                                        Behavior::GlobalLegacy | Behavior::GlobalExtended
                                    )
                                {
                                    let plan = if matches!(behavior, Behavior::GlobalLegacy) {
                                        "(\"MESSAGE\" \"GLOBAL\" NIL NIL NIL \"7BIT\" 123)"
                                            .to_owned()
                                    } else {
                                        format!(
                                            "(\"MESSAGE\" \"GLOBAL\" NIL NIL NIL \"7BIT\" 123 (NIL NIL NIL NIL NIL NIL NIL NIL NIL NIL) {TEXT_PLAN} 1)"
                                        )
                                    };
                                    response.push_str(&format!(
                                        "* {uid} FETCH (UID {uid} BODYSTRUCTURE {plan})\r\n"
                                    ));
                                    continue;
                                }
                                if uid == 2 {
                                    response
                                        .push_str("* 2 FETCH (UID 2 BODYSTRUCTURE (BOGUS))\r\n");
                                    break;
                                }
                                response.push_str(&format!(
                                    "* {uid} FETCH (UID {uid} BODYSTRUCTURE {TEXT_PLAN})\r\n"
                                ));
                            } else if line.contains("HEADER.FIELDS") {
                                let header = raw(uid);
                                response.push_str(&format!("* {uid} FETCH (UID {uid} FLAGS () BODY[HEADER.FIELDS (FROM TO SUBJECT DATE MESSAGE-ID CONTENT-TYPE)] {{{}}}\r\n{header})\r\n", header.len()));
                            } else if line.contains("BODY.PEEK[]") {
                                let message = raw(uid);
                                response.push_str(&format!(
                                    "* {uid} FETCH (UID {uid} BODY[] {{{}}}\r\n{message})\r\n",
                                    message.len()
                                ));
                            } else {
                                assert!(line.contains("BODY.PEEK[1]"), "unexpected fetch: {line}");
                                let mime = "Content-Type: text/plain; charset=utf-8\r\n\r\n";
                                let body = format!("Body {uid}");
                                response.push_str(&format!("* {uid} FETCH (UID {uid} BODY[1.MIME] {{{}}}\r\n{mime} BODY[1] {{{}}}\r\n{body})\r\n", mime.len(), body.len()));
                            }
                        }
                    }
                    "LOGOUT" => {
                        response.push_str("* BYE closing\r\n");
                    }
                    other => panic!("unexpected command: {other}"),
                }
                response.push_str(&format!("{tag} OK complete\r\n"));
                stream.write_all(response.as_bytes()).await.unwrap();
                if parts[1] == "LOGOUT" {
                    break;
                }
            }
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
    let (mut config, folder) = ctx
        .db
        .write(move |conn| {
            let id = repo::accounts::insert(
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
            let folder_id =
                repo::folders::upsert(conn, id, "INBOX", Some("/"), Some(roles::INBOX))?;
            repo::folders::set_uid_state(conn, folder_id, Some(7), Some(4), None)?;
            Ok((
                repo::accounts::get_config(conn, id)?.unwrap(),
                repo::folders::get(conn, folder_id)?.unwrap(),
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
    select_folder_for_remote_read(&ctx, &mut session, &folder)
        .await
        .unwrap();
    let headers = imap::fetch_headers(&mut session, "1:3").await.unwrap();
    store_headers(&ctx, &config, &folder, headers, None)
        .await
        .unwrap();
    let folder_id = folder.id;
    let ids = ctx
        .db
        .read(move |conn| {
            (1..=3)
                .map(|uid| {
                    Ok(repo::messages::by_folder_uid(conn, folder_id, uid)?
                        .unwrap()
                        .id)
                })
                .collect::<Result<Vec<_>>>()
        })
        .await
        .unwrap();
    (
        Fixture {
            ctx,
            config,
            folder,
            ids,
            commands,
            server,
            _dir: dir,
        },
        Some(session),
    )
}
async fn open_second(f: &Fixture, session: &mut Option<Session>) -> Result<()> {
    let id = f.ids[1];
    f.ctx
        .db
        .write(move |conn| {
            conn.execute(
                "UPDATE messages SET body_state='fetching' WHERE id=?1",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        fetch_one_body(&f.ctx, &f.config, session, id),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn headers_survive_parse_failure_and_open_reconnects_without_marking_seen() {
    let (f, mut session) = fixture(Behavior::Broken).await;
    open_second(&f, &mut session).await.unwrap();
    let ids = f.ids.clone();
    f.ctx
        .db
        .read(move |conn| {
            for id in &ids {
                assert!(repo::messages::get_row(conn, *id)?.is_some());
            }
            let row = repo::messages::get_row(conn, ids[1])?.unwrap();
            assert_eq!(row.body_state, "cached");
            assert!(!row.is_read);
            assert!(row.raw_path.is_some());
            Ok(())
        })
        .await
        .unwrap();
    let commands = f.commands.lock().unwrap();
    assert!(
        commands
            .iter()
            .any(|(c, line)| *c == 0 && line.contains("BODYSTRUCTURE"))
    );
    assert!(
        commands
            .iter()
            .any(|(c, line)| *c == 1 && line.contains("SELECT"))
    );
    assert!(
        commands
            .iter()
            .any(|(c, line)| *c == 1 && line.contains("BODY.PEEK[]"))
    );
    assert!(
        !commands
            .iter()
            .any(|(_, line)| line.contains("STORE") || line.contains("LOGOUT"))
    );
}

#[tokio::test]
async fn background_isolates_bad_uid_and_caches_both_siblings() {
    let (f, session) = fixture(Behavior::Broken).await;
    drop(session);
    let items = f
        .ids
        .iter()
        .enumerate()
        .map(|(i, id)| (*id, i as i64 + 1))
        .collect();
    let queue = Arc::new(tokio::sync::Mutex::new(std::collections::VecDeque::from([
        BodyChunk {
            folder_id: f.folder.id,
            folder_name: "INBOX".into(),
            uid_validity: Some(7),
            items,
        },
    ])));
    let skip = Arc::new(Mutex::new(Default::default()));
    let persisted = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let (_settings, rx) = watch::channel(f.config.settings.clone());
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        body_worker(
            f.ctx.clone(),
            f.config.clone(),
            queue,
            skip,
            persisted.clone(),
            rx,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(persisted.load(std::sync::atomic::Ordering::Relaxed), 2);
    let ids = f.ids.clone();
    f.ctx
        .db
        .read(move |conn| {
            for i in [0, 2] {
                assert_eq!(
                    repo::messages::get_row(conn, ids[i])?.unwrap().body_state,
                    "cached"
                );
            }
            assert_eq!(
                repo::messages::get_row(conn, ids[1])?.unwrap().body_state,
                "none"
            );
            assert!(
                conn.query_row(
                    "SELECT count(*) FROM sync_failures WHERE message_id=?1",
                    [ids[1]],
                    |r| r.get::<_, i64>(0)
                )? > 0
            );
            Ok(())
        })
        .await
        .unwrap();
    assert!(
        !f.commands
            .lock()
            .unwrap()
            .iter()
            .any(|(_, line)| line.contains("BODY.PEEK[]"))
    );
}

#[tokio::test]
async fn rejected_fetch_does_not_trigger_compatibility_fallback() {
    let (f, mut session) = fixture(Behavior::Denied).await;
    let result = open_second(&f, &mut session).await;
    assert!(
        matches!(result, Err(CoreError::Imap(_))),
        "{result:?}: {:?}",
        f.commands.lock().unwrap()
    );
    assert!(
        f.commands
            .lock()
            .unwrap()
            .iter()
            .all(|(c, line)| *c == 0 && !line.contains("BODY.PEEK[]"))
    );
}

#[tokio::test]
async fn reconnect_authentication_failure_is_returned_without_fetching_body() {
    let (f, mut session) = fixture(Behavior::Reauth).await;
    assert!(matches!(
        open_second(&f, &mut session).await,
        Err(CoreError::Auth(_))
    ));
    assert!(session.is_none());
    assert!(
        !f.commands
            .lock()
            .unwrap()
            .iter()
            .any(|(_, line)| line.contains("BODY.PEEK[]"))
    );
}

#[tokio::test]
async fn changed_uidvalidity_prevents_full_message_fetch() {
    let (f, mut session) = fixture(Behavior::Reset).await;
    let result = open_second(&f, &mut session).await;
    assert!(
        matches!(result, Err(CoreError::Imap(_))),
        "{result:?}: {:?}",
        f.commands.lock().unwrap()
    );
    assert!(session.is_none());
    assert!(
        !f.commands
            .lock()
            .unwrap()
            .iter()
            .any(|(_, line)| line.contains("BODY.PEEK[]"))
    );
}

// Both legal message/global representations must remain readable even when
// the pinned IMAP parser cannot build a selective plan for one of them.
#[tokio::test]
async fn legal_message_global_variants_remain_readable() {
    for behavior in [Behavior::GlobalLegacy, Behavior::GlobalExtended] {
        let (f, mut session) = fixture(behavior).await;
        open_second(&f, &mut session).await.unwrap();
        let id = f.ids[1];
        f.ctx
            .db
            .read(move |conn| {
                assert_eq!(
                    repo::messages::get_row(conn, id)?.unwrap().body_state,
                    "cached"
                );
                Ok(())
            })
            .await
            .unwrap();
    }
}
