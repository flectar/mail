//! The read receipt action marks the message `$MDNSent` only where the
//! mailbox keeps that keyword (RFC 3503 PERMANENTFLAGS).
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

/// A server whose INBOX keeps `permanent` and records each STORE.
async fn server(
    permanent: &'static str,
) -> (u16, Arc<Mutex<Vec<String>>>, tokio::task::JoinHandle<()>) {
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
                    "* FLAGS (\\Seen \\Flagged $MDNSent)\r\n* OK [PERMANENTFLAGS ({permanent})] kept\r\n* 1 EXISTS\r\n* OK [UIDVALIDITY 7] valid\r\n{tag} OK [READ-WRITE] selected\r\n"
                )
            } else if command.starts_with("UID STORE ") {
                seen.lock().unwrap().push(command.to_owned());
                format!("{tag} OK stored\r\n")
            } else {
                panic!("unexpected command: {command}");
            };
            stream.write_all(response.as_bytes()).await.unwrap();
        }
    });
    (port, stores, task)
}

/// A received message asking for a receipt, declined, with its action.
async fn declined() -> (
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
        .write(|conn| {
            testutil::seed_account(conn);
            let (thread, id) = testutil::seed_message(conn, "ana@example.com", "Plan", false);
            conn.execute(
                "UPDATE messages SET uid = 12, receipt_to = '[{\"name\":null,\"email\":\"ana@example.com\"}]'
                 WHERE id = ?1",
                [id],
            )?;
            repo::actions::enqueue(
                conn,
                1,
                "read_receipt",
                Some(id),
                Some(thread),
                &serde_json::json!({ "send": false, "userAgreed": false }),
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
async fn an_answer_is_marked_where_the_mailbox_keeps_new_keywords() {
    let (_temp, core, config, action) = declined().await;
    let (port, stores, task) = server("\\Seen \\Flagged \\*").await;
    let mut session = connect(port).await;
    read_receipt_on_imap(&core.sync_ctx(), &config, &mut session, &action)
        .await
        .unwrap();
    assert_eq!(
        *stores.lock().unwrap(),
        ["UID STORE 12 +FLAGS.SILENT ($MDNSent)"]
    );
    task.abort();
}

#[tokio::test]
async fn a_mailbox_that_names_the_keyword_keeps_it_too() {
    let (_temp, core, config, action) = declined().await;
    let (port, stores, task) = server("\\Seen $MDNSent").await;
    let mut session = connect(port).await;
    read_receipt_on_imap(&core.sync_ctx(), &config, &mut session, &action)
        .await
        .unwrap();
    assert_eq!(stores.lock().unwrap().len(), 1);
    task.abort();
}

#[tokio::test]
async fn a_mailbox_that_would_forget_the_keyword_isnt_asked_to_set_it() {
    let (_temp, core, config, action) = declined().await;
    let (port, stores, task) = server("\\Seen \\Flagged").await;
    let mut session = connect(port).await;
    read_receipt_on_imap(&core.sync_ctx(), &config, &mut session, &action)
        .await
        .unwrap();
    assert!(stores.lock().unwrap().is_empty());
    task.abort();
}
