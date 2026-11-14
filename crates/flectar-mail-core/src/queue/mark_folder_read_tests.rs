//! Marking a whole IMAP folder read: one STORE through the newest message
//! the last check knew of, windows when the server turns that down, and
//! nothing when the folder's UIDs now name other mail.
use super::*;
use crate::{Core, config::Paths, db::testutil};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use std::sync::{Arc, Mutex};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::TcpListener,
};

const CERT: &str = include_str!("../../tests/fixtures/tls/server.pem");
const KEY: &str = include_str!("../../tests/fixtures/tls/server-key.pem");

/// A server whose INBOX has UIDVALIDITY 7. It turns down a STORE over a
/// range wider than `limit` UIDs, and records every STORE.
async fn server(limit: u32) -> (u16, Arc<Mutex<Vec<String>>>, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let stores = Arc::new(Mutex::new(Vec::new()));
    let seen = stores.clone();
    let task = tokio::spawn(async move {
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
        let (tcp, _) = listener.accept().await.unwrap();
        let mut stream = BufReader::new(acceptor.accept(tcp).await.unwrap());
        stream.write_all(b"* OK ready\r\n").await.unwrap();
        loop {
            let mut line = String::new();
            if stream.read_line(&mut line).await.unwrap() == 0 {
                break;
            }
            let (tag, command) = line.trim_end().split_once(' ').unwrap();
            let response = if command.starts_with("LOGIN ") {
                format!("{tag} OK logged in\r\n")
            } else if command == "CAPABILITY" {
                format!("* CAPABILITY IMAP4rev1 UIDPLUS\r\n{tag} OK capabilities\r\n")
            } else if command.starts_with("SELECT ") {
                format!(
                    "* FLAGS (\\Seen \\Flagged)\r\n* OK [PERMANENTFLAGS (\\Seen \\Flagged)] kept\r\n* 40 EXISTS\r\n* OK [UIDVALIDITY 7] valid\r\n{tag} OK [READ-WRITE] selected\r\n"
                )
            } else if let Some(store) = command.strip_prefix("UID STORE ") {
                seen.lock().unwrap().push(store.to_owned());
                let range = store.split_once(' ').unwrap().0;
                let (low, high) = range.split_once(':').unwrap_or((range, range));
                let (low, high) = (low.parse::<u32>().unwrap(), high.parse::<u32>().unwrap());
                if high - low + 1 > limit {
                    format!("{tag} NO [LIMIT] too many messages\r\n")
                } else {
                    format!("{tag} OK stored\r\n")
                }
            } else {
                panic!("unexpected command: {command}");
            };
            stream.write_all(response.as_bytes()).await.unwrap();
        }
    });
    (port, stores, task)
}

/// Inbox marked read through UID `through`, queued when the folder had
/// UIDVALIDITY `validity`.
async fn queued(
    through: i64,
    validity: i64,
) -> (
    tempfile::TempDir,
    Core,
    AccountConfig,
    repo::actions::PendingAction,
) {
    let temp = tempfile::tempdir().unwrap();
    let core = Core::start_mail_ui(Paths::for_tests(temp.path()))
        .await
        .unwrap();
    let (config, action) = core
        .db
        .write(move |conn| {
            testutil::seed_account(conn);
            repo::actions::enqueue(
                conn,
                1,
                "mark_folder_read",
                None,
                None,
                &serde_json::json!({
                    "folderId": 1, "throughUid": through, "uidValidity": validity
                }),
                None,
            )?;
            Ok((
                repo::accounts::get_config(conn, 1)?.unwrap(),
                repo::actions::due(conn, 1, now_ms(), 10)?.remove(0),
            ))
        })
        .await
        .unwrap();
    (temp, core, config, action)
}

async fn connect(port: u16) -> Session {
    imap::connect_with_settings(
        "127.0.0.1",
        port,
        imap::ImapCredentials::Password {
            user: "me".into(),
            password: "password".into(),
        },
        &MailConnectionSettings {
            trusted_certificate_pem: CERT.into(),
            ..MailConnectionSettings::new(ConnectionSecurity::Tls, ConnectionSecurity::Tls)
        },
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn one_store_marks_the_folder_through_the_last_check() {
    let (_temp, core, config, action) = queued(40, 7).await;
    let (port, stores, task) = server(u32::MAX).await;
    let mut session = connect(port).await;
    mark_folder_read_on_imap(&core.sync_ctx(), &config, &mut session, &action)
        .await
        .unwrap();
    assert_eq!(*stores.lock().unwrap(), ["1:40 +FLAGS.SILENT (\\Seen)"]);
    task.abort();
}

#[tokio::test]
async fn a_server_that_turns_the_whole_range_down_gets_windows() {
    let (_temp, core, config, action) = queued(25_000, 7).await;
    let (port, stores, task) = server(10_000).await;
    let mut session = connect(port).await;
    mark_folder_read_on_imap(&core.sync_ctx(), &config, &mut session, &action)
        .await
        .unwrap();
    assert_eq!(
        *stores.lock().unwrap(),
        [
            "1:25000 +FLAGS.SILENT (\\Seen)",
            "15001:25000 +FLAGS.SILENT (\\Seen)",
            "5001:15000 +FLAGS.SILENT (\\Seen)",
            "1:5000 +FLAGS.SILENT (\\Seen)",
        ]
    );
    task.abort();
}

#[tokio::test]
async fn new_uids_name_other_mail_so_nothing_is_marked() {
    let (_temp, core, config, action) = queued(40, 6).await;
    let (port, stores, task) = server(u32::MAX).await;
    let mut session = connect(port).await;
    mark_folder_read_on_imap(&core.sync_ctx(), &config, &mut session, &action)
        .await
        .unwrap();
    assert!(stores.lock().unwrap().is_empty());
    task.abort();
}

#[tokio::test]
async fn a_refused_flag_change_is_an_error_not_a_success() {
    let (port, _stores, task) = server(0).await;
    let mut session = connect(port).await;
    imap::select(&mut session, "INBOX").await.unwrap();
    let result = imap::store_flag(&mut session, 12, "\\Seen", true).await;
    assert!(
        matches!(result, Err(CoreError::ImapRefused(_))),
        "{result:?}"
    );
    task.abort();
}
