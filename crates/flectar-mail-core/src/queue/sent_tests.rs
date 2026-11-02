use super::*;
use crate::accounts::credentials::DevelopmentFileCredentialStore;
use crate::{Core, config::Paths, db::testutil};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpListener,
};

const CERT: &str = include_str!("../../tests/fixtures/tls/server.pem");
const KEY: &str = include_str!("../../tests/fixtures/tls/server-key.pem");
const RAW: &[u8] = b"Message-ID: <submission@test.dev>\r\nX-Flectar-Submission-ID: submission@test.dev\r\nFrom: me@test.dev\r\nTo: alice@test.dev\r\nSubject: Status\r\n\r\nFor Alice only.\r\n";

pub(crate) async fn local_outgoing() -> (
    tempfile::TempDir,
    Core,
    AccountConfig,
    repo::actions::PendingAction,
) {
    let temp = tempfile::tempdir().unwrap();
    let credentials = Arc::new(DevelopmentFileCredentialStore::new(
        temp.path().join("credentials.json"),
    ));
    let core = Core::start_mail_ui_with_credentials(Paths::for_tests(temp.path()), credentials)
        .await
        .unwrap();
    let dir = core.paths.mail_dir(1);
    tokio::fs::create_dir_all(&dir).await.unwrap();
    let path = dir.join("sent.eml");
    tokio::fs::write(&path, RAW).await.unwrap();
    let (config, action) = core.db.write(move |conn| {
        testutil::seed_account(conn);
        conn.execute("UPDATE folders SET role='sent', path='Sent', uidvalidity=7", [])?;
        let (thread, id) = testutil::seed_message(conn, "me@test.dev", "Status", false);
        conn.execute("UPDATE messages SET is_outgoing=1, uid=NULL, message_id='submission@test.dev', raw_path=?1 WHERE id=?2", rusqlite::params![path.to_str().unwrap(), id])?;
        repo::actions::enqueue(conn, 1, "append_sent", Some(id), Some(thread), &serde_json::json!({}), None)?;
        Ok((repo::accounts::get_config(conn, 1)?.unwrap(), repo::actions::due(conn, 1, now_ms(), 10)?.remove(0)))
    }).await.unwrap();
    (temp, core, config, action)
}

#[derive(Clone, Copy)]
enum Reply {
    Receipt,
    NoUidPlus,
    Disconnect,
    Reject,
}
struct Server {
    port: u16,
    appends: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}
impl Server {
    async fn start(reply: Reply, existing: bool, marker_preserved: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let appends = Arc::new(AtomicUsize::new(0));
        let count = appends.clone();
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
            let mut stored = existing;
            loop {
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
                        // The receipt must win over UIDNEXT, including concurrent deliveries.
                        format!(
                            "* FLAGS (\\Seen)\r\n* 1 EXISTS\r\n* OK [UIDVALIDITY 7] valid\r\n* OK [UIDNEXT 40] next\r\n{tag} OK selected\r\n"
                        )
                    } else if command.starts_with("UID SEARCH HEADER ") {
                        let found = stored
                            && marker_preserved
                            && command.contains("X-Flectar-Submission-ID");
                        format!(
                            "* SEARCH{}\r\n{tag} OK searched\r\n",
                            if found { " 99" } else { "" }
                        )
                    } else if command.starts_with("UID FETCH ") {
                        assert!(command.contains("X-FLECTAR-SUBMISSION-ID"));
                        let header = "Message-ID: <rewritten@server>\r\nX-Flectar-Submission-ID: submission@test.dev\r\nSubject: Status\r\n\r\n";
                        format!(
                            "* 1 FETCH (UID 99 FLAGS (\\Seen) BODY[HEADER.FIELDS (MESSAGE-ID X-FLECTAR-SUBMISSION-ID SUBJECT)] {{{}}}\r\n{header})\r\n{tag} OK fetched\r\n",
                            header.len()
                        )
                    } else if command.starts_with("APPEND ") {
                        if matches!(reply, Reply::Reject) {
                            stream
                                .write_all(format!("{tag} NO [OVERQUOTA] rejected\r\n").as_bytes())
                                .await
                                .unwrap();
                            continue;
                        }
                        let size: usize = command
                            .rsplit_once('{')
                            .unwrap()
                            .1
                            .trim_end_matches('}')
                            .parse()
                            .unwrap();
                        stream.write_all(b"+ continue\r\n").await.unwrap();
                        let mut literal = vec![0; size + 2];
                        stream.read_exact(&mut literal).await.unwrap();
                        assert_eq!(&literal[..size], RAW);
                        assert_eq!(&literal[size..], b"\r\n");
                        count.fetch_add(1, Ordering::SeqCst);
                        stored = true;
                        match reply {
                            Reply::Disconnect => break,
                            Reply::Receipt => {
                                format!("* 2 EXISTS\r\n{tag} OK [APPENDUID 7 99] appended\r\n")
                            }
                            Reply::NoUidPlus => format!("{tag} OK appended\r\n"),
                            Reply::Reject => unreachable!(),
                        }
                    } else {
                        panic!("unexpected command: {command}");
                    };
                    stream.write_all(response.as_bytes()).await.unwrap();
                }
            }
        });
        Self {
            port,
            appends,
            task,
        }
    }
    async fn connect(&self) -> Session {
        imap::connect_with_settings(
            "127.0.0.1",
            self.port,
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
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[tokio::test]
async fn sent_append_uses_receipt_and_persisted_uid_on_retry() {
    let (_temp, core, config, action) = local_outgoing().await;
    let server = Server::start(Reply::Receipt, false, false).await;
    let mut session = server.connect().await;
    append_sent_action(&core.sync_ctx(), &config, &mut session, &action)
        .await
        .unwrap();
    let id = action.message_id.unwrap();
    assert_eq!(
        core.db
            .read(move |c| repo::messages::get_row(c, id))
            .await
            .unwrap()
            .unwrap()
            .uid,
        Some(99)
    );
    // Even if both headers were stripped, the recorded UID wins.
    append_sent_action(&core.sync_ctx(), &config, &mut session, &action)
        .await
        .unwrap();
    assert_eq!(server.appends.load(Ordering::SeqCst), 1);
    assert!(
        core.db
            .read(|c| repo::actions::due(c, 1, now_ms(), 10))
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn sent_append_without_uidplus_completes_without_post_append_command() {
    let (_temp, core, config, action) = local_outgoing().await;
    let server = Server::start(Reply::NoUidPlus, false, true).await;
    append_sent_action(
        &core.sync_ctx(),
        &config,
        &mut server.connect().await,
        &action,
    )
    .await
    .unwrap();
    assert_eq!(server.appends.load(Ordering::SeqCst), 1);
    assert!(
        core.db
            .read(|c| repo::actions::due(c, 1, now_ms(), 10))
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn sent_append_lost_response_recovers_by_marker_without_appending_again() {
    for marker in [true, false] {
        let (_temp, core, config, action) = local_outgoing().await;
        let server = Server::start(Reply::Disconnect, false, marker).await;
        let mut session = server.connect().await;
        assert!(
            append_sent_action(&core.sync_ctx(), &config, &mut session, &action)
                .await
                .is_err()
        );
        drop(session);
        let retry = core
            .db
            .read(|c| Ok(repo::actions::due(c, 1, now_ms(), 10)?.remove(0)))
            .await
            .unwrap();
        assert_eq!(retry.payload["sentAppendStarted"], true);
        let mut session = server.connect().await;
        let outcome = append_sent_action(&core.sync_ctx(), &config, &mut session, &retry).await;
        assert_eq!(outcome.is_ok(), marker, "{outcome:?}");
        assert_eq!(server.appends.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn sent_append_reuses_rewritten_existing_copy() {
    let (_temp, core, config, action) = local_outgoing().await;
    let server = Server::start(Reply::Receipt, true, true).await;
    append_sent_action(
        &core.sync_ctx(),
        &config,
        &mut server.connect().await,
        &action,
    )
    .await
    .unwrap();
    assert_eq!(server.appends.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn sent_append_rejection_does_not_send_literal() {
    let (_temp, core, config, action) = local_outgoing().await;
    let server = Server::start(Reply::Reject, false, true).await;
    let error = append_sent_action(
        &core.sync_ctx(),
        &config,
        &mut server.connect().await,
        &action,
    )
    .await
    .unwrap_err();
    assert!(is_permanent(&error), "{error}");
    assert_eq!(server.appends.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn sent_receipt_does_not_undo_moves_or_use_stale_uidvalidity() {
    for moved in [true, false] {
        let (_temp, core, _, action) = local_outgoing().await;
        let id = action.message_id.unwrap();
        let action_copy = action.clone();
        core.db.write(move |c| {
            if moved {
                c.execute("INSERT INTO folders (id, account_id, path, role) VALUES (2,1,'Trash','trash')", [])?;
                c.execute("UPDATE messages SET folder_id=2", [])?;
            } else { c.execute("UPDATE folders SET uidvalidity=8", [])?; }
            finish_sent_filing_in_db(c, &action_copy, 1, Some(7), Some(99))
        }).await.unwrap();
        let row = core
            .db
            .read(move |c| repo::messages::get_row(c, id))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.uid, None);
        assert_eq!(row.folder_id, Some(if moved { 2 } else { 1 }));
    }
}

#[tokio::test]
async fn sent_copy_policy_setting_preserves_other_account_settings() {
    let (_temp, core, _, _) = local_outgoing().await;
    core.set_account_mail_history(1, MailHistory::OneYear)
        .await
        .unwrap();
    core.set_account_sent_copy_policy(1, SentCopyPolicy::Client)
        .await
        .unwrap();
    let settings = core
        .db
        .read(|c| Ok(repo::accounts::get_config(c, 1)?.unwrap().settings))
        .await
        .unwrap();
    assert_eq!(settings.sent_copy_policy, SentCopyPolicy::Client);
    assert_eq!(settings.mail_history, MailHistory::OneYear);
}

#[tokio::test]
async fn only_pop3_accounts_remove_mail_from_their_server() {
    let (_temp, core, _, _) = local_outgoing().await;
    let refused = core
        .set_account_keep_on_server(1, KeepOnServer::UntilDeleted)
        .await
        .unwrap_err();
    assert!(refused.to_string().contains("POP3"), "{refused}");
    let settings = core
        .db
        .read(|c| Ok(repo::accounts::get_config(c, 1)?.unwrap().settings))
        .await
        .unwrap();
    assert_eq!(settings.keep_on_server, KeepOnServer::Always);
}
