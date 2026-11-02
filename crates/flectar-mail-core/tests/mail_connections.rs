//! Local protocol fixtures exercise actual TLS handshakes without external accounts.
use flectar_mail_core::accounts::credentials::DevelopmentFileCredentialStore;
use flectar_mail_core::config::Paths;
use flectar_mail_core::models::*;
use flectar_mail_core::{Core, imap, net, smtp};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use std::sync::Arc;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::TcpListener,
};

const CERT: &str = include_str!("fixtures/tls/server.pem");
const PROTON_BRIDGE_CERT: &str = include_str!("fixtures/tls/proton-bridge.pem");
const KEY: &str = include_str!("fixtures/tls/server-key.pem");

fn acceptor_for(cert: &str) -> tokio_rustls::TlsAcceptor {
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![CertificateDer::from_pem_slice(cert.as_bytes()).unwrap()],
        PrivateKeyDer::from_pem_slice(KEY.as_bytes()).unwrap(),
    )
    .unwrap();
    tokio_rustls::TlsAcceptor::from(Arc::new(config))
}
fn settings(mode: ConnectionSecurity) -> MailConnectionSettings {
    settings_with_certificate(mode, CERT)
}

fn settings_with_certificate(
    mode: ConnectionSecurity,
    certificate: &str,
) -> MailConnectionSettings {
    MailConnectionSettings {
        incoming_security: mode,
        smtp_security: mode,
        trusted_certificate_pem: certificate.into(),
    }
}
async fn line<S: tokio::io::AsyncRead + Unpin>(stream: &mut BufReader<S>) -> String {
    let mut line = String::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        stream.read_line(&mut line),
    )
    .await
    .unwrap()
    .unwrap();
    line
}

async fn advertise_capabilities<S>(stream: &mut BufReader<S>, capabilities: &str)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let command = line(stream).await;
    assert!(
        command.contains(" CAPABILITY"),
        "unexpected command: {command:?}"
    );
    let tag = command.split_whitespace().next().unwrap();
    stream
        .write_all(format!("* CAPABILITY {capabilities}\r\n{tag} OK capability\r\n").as_bytes())
        .await
        .unwrap();
}

async fn accept_client_identification<S>(stream: &mut BufReader<S>)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let command = line(stream).await;
    assert!(command.contains(" ID ("), "unexpected command: {command:?}");
    assert!(command.contains(r#""name" "Flectar Mail""#));
    assert!(command.contains(&format!(r#""version" "{}""#, env!("CARGO_PKG_VERSION"))));
    assert!(command.contains(r#""vendor" "Flectar""#));
    let tag = command.split_whitespace().next().unwrap();
    stream
        .write_all(format!("* ID NIL\r\n{tag} OK identified\r\n").as_bytes())
        .await
        .unwrap();
}

async fn accept_inbox_select<S>(stream: &mut BufReader<S>, command: &str)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    assert!(
        command.contains(r#" SELECT "INBOX""#),
        "unexpected command: {command:?}"
    );
    let tag = command.split_whitespace().next().unwrap();
    stream
        .write_all(
            format!(
                "* FLAGS (\\Seen \\Deleted)\r\n\
                 * 0 EXISTS\r\n\
                 * OK [UIDVALIDITY 1] valid\r\n\
                 * OK [UIDNEXT 1] next\r\n\
                 {tag} OK [READ-WRITE] selected\r\n"
            )
            .as_bytes(),
        )
        .await
        .unwrap();
}
async fn imap_server(starttls: bool, reject: bool) -> (u16, tokio::task::JoinHandle<()>) {
    imap_server_with_certificate(starttls, reject, CERT).await
}

async fn imap_server_with_certificate(
    starttls: bool,
    reject: bool,
    certificate: &'static str,
) -> (u16, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let tcp = if starttls {
            let mut plain = BufReader::new(tcp);
            plain.write_all(b"* OK test IMAP ready\r\n").await.unwrap();
            let command = line(&mut plain).await;
            assert!(
                command.ends_with(" STARTTLS\r\n"),
                "credentials sent before upgrade"
            );
            let tag = command.split_whitespace().next().unwrap();
            if reject {
                plain
                    .write_all(format!("{tag} NO TLS unavailable\r\n").as_bytes())
                    .await
                    .unwrap();
                assert!(
                    line(&mut plain).await.is_empty(),
                    "client sent plaintext credentials after rejected STARTTLS"
                );
                return;
            }
            plain
                .write_all(format!("{tag} OK upgrade\r\n").as_bytes())
                .await
                .unwrap();
            plain.into_inner()
        } else {
            tcp
        };
        let Ok(tls) = acceptor_for(certificate).accept(tcp).await else {
            return;
        };
        let mut stream = BufReader::new(tls);
        if !starttls {
            stream.write_all(b"* OK test IMAP ready\r\n").await.unwrap();
        }
        let login = line(&mut stream).await;
        if login.is_empty() {
            return;
        }
        assert!(login.contains(" LOGIN "));
        let tag = login.split_whitespace().next().unwrap();
        stream
            .write_all(format!("{tag} OK logged in\r\n").as_bytes())
            .await
            .unwrap();
        advertise_capabilities(&mut stream, "IMAP4rev1 ID").await;
        accept_client_identification(&mut stream).await;
        let mut logout = line(&mut stream).await;
        if logout.contains(r#" SELECT "INBOX""#) {
            accept_inbox_select(&mut stream, &logout).await;
            logout = line(&mut stream).await;
        }
        assert!(logout.contains(" LOGOUT"), "unexpected command: {logout:?}");
        let tag = logout.split_whitespace().next().unwrap();
        stream
            .write_all(format!("* BYE closing\r\n{tag} OK logout\r\n").as_bytes())
            .await
            .unwrap();
    });
    (port, task)
}
/// An unencrypted IMAP server. With `login_disabled` it announces
/// LOGINDISABLED and fails the test if credentials arrive anyway.
async fn plaintext_imap_server(login_disabled: bool) -> (u16, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut stream = BufReader::new(tcp);
        stream.write_all(b"* OK test IMAP ready\r\n").await.unwrap();
        advertise_capabilities(
            &mut stream,
            if login_disabled {
                "IMAP4rev1 STARTTLS LOGINDISABLED"
            } else {
                "IMAP4rev1 ID"
            },
        )
        .await;
        let login = line(&mut stream).await;
        if login_disabled {
            assert!(
                login.is_empty(),
                "credentials sent despite LOGINDISABLED: {login:?}"
            );
            return;
        }
        assert!(login.contains(" LOGIN "), "unexpected command: {login:?}");
        let tag = login.split_whitespace().next().unwrap();
        stream
            .write_all(format!("{tag} OK logged in\r\n").as_bytes())
            .await
            .unwrap();
        advertise_capabilities(&mut stream, "IMAP4rev1 ID").await;
        accept_client_identification(&mut stream).await;
        let logout = line(&mut stream).await;
        assert!(logout.contains(" LOGOUT"), "unexpected command: {logout:?}");
        let tag = logout.split_whitespace().next().unwrap();
        stream
            .write_all(format!("* BYE closing\r\n{tag} OK logout\r\n").as_bytes())
            .await
            .unwrap();
    });
    (port, task)
}

/// An unencrypted SMTP server. Without `offer_auth` it offers sign-in only
/// after STARTTLS and fails the test if credentials arrive anyway.
async fn plaintext_smtp_server(offer_auth: bool) -> (u16, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut stream = BufReader::new(tcp);
        stream.write_all(b"220 localhost ESMTP\r\n").await.unwrap();
        assert!(line(&mut stream).await.starts_with("EHLO "));
        if !offer_auth {
            stream
                .write_all(b"250-localhost\r\n250 STARTTLS\r\n")
                .await
                .unwrap();
            assert_eq!(
                line(&mut stream).await,
                "QUIT\r\n",
                "credentials sent without AUTH"
            );
            stream.write_all(b"221 Goodbye\r\n").await.unwrap();
            return;
        }
        stream
            .write_all(b"250-localhost\r\n250 AUTH PLAIN\r\n")
            .await
            .unwrap();
        assert!(line(&mut stream).await.starts_with("AUTH PLAIN "));
        stream.write_all(b"235 Authenticated\r\n").await.unwrap();
        assert_eq!(line(&mut stream).await, "NOOP\r\n");
        stream.write_all(b"250 OK\r\n").await.unwrap();
        assert_eq!(line(&mut stream).await, "QUIT\r\n");
        stream.write_all(b"221 Goodbye\r\n").await.unwrap();
    });
    (port, task)
}

fn credentials() -> imap::ImapCredentials {
    imap::ImapCredentials::Password {
        user: "bridge-user".into(),
        password: "bridge-password".into(),
    }
}

#[tokio::test]
async fn imap_without_encryption_signs_in_over_plain_tcp() {
    let (port, task) = plaintext_imap_server(false).await;
    let session = imap::connect_with_settings(
        "127.0.0.1",
        port,
        credentials(),
        &settings(ConnectionSecurity::None),
    )
    .await
    .unwrap();
    imap::logout(session).await;
    task.await.unwrap();
}

#[tokio::test]
async fn imap_never_sends_a_password_the_server_refuses_unencrypted() {
    let (port, task) = plaintext_imap_server(true).await;
    let error = imap::connect_with_settings(
        "127.0.0.1",
        port,
        credentials(),
        &settings(ConnectionSecurity::None),
    )
    .await
    .err()
    .unwrap();
    assert!(
        error
            .to_string()
            .contains("only over an encrypted connection"),
        "{error}"
    );
    assert!(!matches!(
        error,
        flectar_mail_core::error::CoreError::Auth(_)
    ));
    task.await.unwrap();
}

#[tokio::test]
async fn oauth_tokens_never_travel_unencrypted() {
    // Refused before connecting, so nothing needs to listen on the port.
    let error = imap::connect_with_settings(
        "127.0.0.1",
        9,
        imap::ImapCredentials::XOAuth2 {
            user: "user@example.com".into(),
            access_token: "token".into(),
        },
        &settings(ConnectionSecurity::None),
    )
    .await
    .err()
    .unwrap();
    assert!(
        error.to_string().contains("encrypted connection"),
        "{error}"
    );

    let mut config = smtp_config(9, ConnectionSecurity::None, CERT);
    config.auth_kind = AuthKind::Oauth2;
    let error = smtp::test_connection(&config, &smtp::SmtpAuth::XOAuth2("token".into()))
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("encrypted connection"),
        "{error}"
    );
}

#[tokio::test]
async fn smtp_without_encryption_signs_in_over_plain_tcp() {
    let (port, task) = plaintext_smtp_server(true).await;
    let config = smtp_config(port, ConnectionSecurity::None, CERT);
    smtp::test_connection(&config, &smtp::SmtpAuth::Password("bridge-password".into()))
        .await
        .unwrap();
    task.await.unwrap();
}

#[tokio::test]
async fn smtp_never_sends_a_password_the_server_does_not_accept_unencrypted() {
    let (port, task) = plaintext_smtp_server(false).await;
    let config = smtp_config(port, ConnectionSecurity::None, CERT);
    let error = smtp::test_connection(&config, &smtp::SmtpAuth::Password("bridge-password".into()))
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("only over an encrypted connection"),
        "{error}"
    );
    assert!(!matches!(
        error,
        flectar_mail_core::error::CoreError::Auth(_)
    ));
    task.await.unwrap();
}

#[tokio::test]
async fn imap_starttls_on_custom_port() {
    let (port, task) = imap_server(true, false).await;
    let session = imap::connect_with_settings(
        "127.0.0.1",
        port,
        credentials(),
        &settings(ConnectionSecurity::Starttls),
    )
    .await
    .unwrap();
    imap::logout(session).await;
    task.await.unwrap();
}
#[tokio::test]
async fn imap_implicit_tls_on_custom_port() {
    let (port, task) = imap_server(false, false).await;
    let session = imap::connect_with_settings(
        "127.0.0.1",
        port,
        credentials(),
        &settings(ConnectionSecurity::Tls),
    )
    .await
    .unwrap();
    imap::logout(session).await;
    task.await.unwrap();
}
#[tokio::test]
async fn rejected_starttls_never_sends_credentials() {
    let (port, task) = imap_server(true, true).await;
    let error = imap::connect_with_settings(
        "127.0.0.1",
        port,
        credentials(),
        &settings(ConnectionSecurity::Starttls),
    )
    .await
    .err()
    .unwrap();
    assert!(error.to_string().contains("STARTTLS was rejected"));
    task.await.unwrap();
}
#[tokio::test]
async fn untrusted_certificate_is_rejected() {
    let (port, task) = imap_server(false, false).await;
    let mut config = settings(ConnectionSecurity::Tls);
    config.trusted_certificate_pem.clear();
    assert!(
        imap::connect_with_settings("127.0.0.1", port, credentials(), &config)
            .await
            .is_err()
    );
    task.await.unwrap();
}

#[tokio::test]
async fn imap_incremental_search_handles_sparse_uids_and_reversed_star_ranges() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let tls = acceptor_for(CERT).accept(tcp).await.unwrap();
        let mut stream = BufReader::new(tls);
        stream.write_all(b"* OK test IMAP ready\r\n").await.unwrap();

        let login = line(&mut stream).await;
        assert!(login.contains(" LOGIN "), "unexpected command: {login:?}");
        let tag = login.split_whitespace().next().unwrap();
        stream
            .write_all(format!("{tag} OK logged in\r\n").as_bytes())
            .await
            .unwrap();
        advertise_capabilities(&mut stream, "IMAP4rev1").await;

        let select = line(&mut stream).await;
        assert!(
            select.contains(r#" SELECT "INBOX""#),
            "unexpected command: {select:?}"
        );
        let tag = select.split_whitespace().next().unwrap();
        stream
            .write_all(
                format!(
                    "* FLAGS (\\Seen \\Deleted)\r\n\
                     * 2 EXISTS\r\n\
                     * OK [UIDVALIDITY 7] valid\r\n\
                     {tag} OK [READ-WRITE] selected\r\n"
                )
                .as_bytes(),
            )
            .await
            .unwrap();

        let sparse = line(&mut stream).await;
        assert!(
            sparse.contains(" UID SEARCH UID 43:*"),
            "unexpected command: {sparse:?}"
        );
        let tag = sparse.split_whitespace().next().unwrap();
        stream
            .write_all(format!("* SEARCH 1000042 42 1000042\r\n{tag} OK searched\r\n").as_bytes())
            .await
            .unwrap();

        let reversed = line(&mut stream).await;
        assert!(
            reversed.contains(" UID SEARCH UID 1000043:*"),
            "unexpected command: {reversed:?}"
        );
        let tag = reversed.split_whitespace().next().unwrap();
        stream
            .write_all(format!("* SEARCH 1000042\r\n{tag} OK searched\r\n").as_bytes())
            .await
            .unwrap();

        let logout = line(&mut stream).await;
        assert!(logout.contains(" LOGOUT"), "unexpected command: {logout:?}");
        let tag = logout.split_whitespace().next().unwrap();
        stream
            .write_all(format!("* BYE closing\r\n{tag} OK logout\r\n").as_bytes())
            .await
            .unwrap();
    });

    let mut session = imap::connect_with_settings(
        "127.0.0.1",
        port,
        credentials(),
        &settings(ConnectionSecurity::Tls),
    )
    .await
    .unwrap();
    let selected = imap::select(&mut session, "INBOX").await.unwrap();
    assert_eq!(selected.uid_next, None);
    assert_eq!(
        imap::uid_search_after(&mut session, 42).await.unwrap(),
        [1000042]
    );
    assert!(
        imap::uid_search_after(&mut session, 1000042)
            .await
            .unwrap()
            .is_empty()
    );
    imap::logout(session).await;
    task.await.unwrap();
}

#[derive(Clone, Copy)]
enum ImapMoveFixture {
    LegacyUidPlus,
    AdvertisedMoveRejected,
    LegacyWithoutUidPlus,
}

async fn imap_move_server(fixture: ImapMoveFixture) -> (u16, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let tls = acceptor_for(CERT).accept(tcp).await.unwrap();
        let mut stream = BufReader::new(tls);
        stream.write_all(b"* OK test IMAP ready\r\n").await.unwrap();

        let login = line(&mut stream).await;
        assert!(login.contains(" LOGIN "), "unexpected command: {login:?}");
        let tag = login.split_whitespace().next().unwrap();
        stream
            .write_all(format!("{tag} OK logged in\r\n").as_bytes())
            .await
            .unwrap();

        advertise_capabilities(&mut stream, "IMAP4rev1").await;

        let select = line(&mut stream).await;
        assert!(
            select.contains(" SELECT "),
            "unexpected command: {select:?}"
        );
        let tag = select.split_whitespace().next().unwrap();
        stream
            .write_all(
                format!(
                    "* FLAGS (\\Seen \\Deleted)\r\n\
                     * 2 EXISTS\r\n\
                     * OK [UIDVALIDITY 1] valid\r\n\
                     * OK [UIDNEXT 43] next\r\n\
                     {tag} OK [READ-WRITE] selected\r\n"
                )
                .as_bytes(),
            )
            .await
            .unwrap();

        let capability = line(&mut stream).await;
        assert!(
            capability.contains(" CAPABILITY"),
            "unexpected command: {capability:?}"
        );
        let tag = capability.split_whitespace().next().unwrap();
        let capabilities = match fixture {
            ImapMoveFixture::LegacyUidPlus => "IMAP4rev1 UIDPLUS",
            ImapMoveFixture::AdvertisedMoveRejected => "IMAP4rev1 UIDPLUS MOVE",
            ImapMoveFixture::LegacyWithoutUidPlus => "IMAP4rev1",
        };
        stream
            .write_all(format!("* CAPABILITY {capabilities}\r\n{tag} OK capability\r\n").as_bytes())
            .await
            .unwrap();

        match fixture {
            ImapMoveFixture::LegacyUidPlus => {
                let copy = line(&mut stream).await;
                assert!(
                    copy.contains(" UID COPY 42 "),
                    "unexpected command: {copy:?}"
                );
                let tag = copy.split_whitespace().next().unwrap();
                stream
                    .write_all(format!("{tag} OK copied\r\n").as_bytes())
                    .await
                    .unwrap();

                let store = line(&mut stream).await;
                assert!(
                    store.contains(" UID STORE 42 +FLAGS.SILENT (\\Deleted)"),
                    "unexpected command: {store:?}"
                );
                let tag = store.split_whitespace().next().unwrap();
                stream
                    .write_all(format!("{tag} OK stored\r\n").as_bytes())
                    .await
                    .unwrap();

                let expunge = line(&mut stream).await;
                assert!(
                    expunge.contains(" UID EXPUNGE 42"),
                    "unexpected command: {expunge:?}"
                );
                let tag = expunge.split_whitespace().next().unwrap();
                stream
                    .write_all(format!("* 1 EXPUNGE\r\n{tag} OK expunged\r\n").as_bytes())
                    .await
                    .unwrap();
            }
            ImapMoveFixture::AdvertisedMoveRejected => {
                let move_command = line(&mut stream).await;
                assert!(
                    move_command.contains(" UID MOVE 42 "),
                    "unexpected command: {move_command:?}"
                );
                let tag = move_command.split_whitespace().next().unwrap();
                stream
                    .write_all(format!("{tag} NO [NOPERM] move denied\r\n").as_bytes())
                    .await
                    .unwrap();
            }
            ImapMoveFixture::LegacyWithoutUidPlus => {
                let copy = line(&mut stream).await;
                assert!(
                    copy.contains(" UID COPY 42 "),
                    "unexpected command: {copy:?}"
                );
                let tag = copy.split_whitespace().next().unwrap();
                stream
                    .write_all(format!("{tag} OK copied\r\n").as_bytes())
                    .await
                    .unwrap();

                let store = line(&mut stream).await;
                assert!(
                    store.contains(" UID STORE 42 +FLAGS.SILENT (\\Deleted)"),
                    "unexpected command: {store:?}"
                );
                let tag = store.split_whitespace().next().unwrap();
                stream
                    .write_all(format!("{tag} OK stored\r\n").as_bytes())
                    .await
                    .unwrap();
            }
        }

        // In the rejection case this proves the client did not issue a second
        // COPY. Without UIDPLUS it proves the client did not use the unsafe,
        // mailbox-wide EXPUNGE command.
        let logout = line(&mut stream).await;
        assert!(
            logout.contains(" LOGOUT"),
            "unsafe command after move result: {logout:?}"
        );
        let tag = logout.split_whitespace().next().unwrap();
        stream
            .write_all(format!("* BYE closing\r\n{tag} OK logout\r\n").as_bytes())
            .await
            .unwrap();
    });
    (port, task)
}

async fn run_imap_move(fixture: ImapMoveFixture) -> flectar_mail_core::error::Result<()> {
    let (port, task) = imap_move_server(fixture).await;
    let mut session = imap::connect_with_settings(
        "127.0.0.1",
        port,
        credentials(),
        &settings(ConnectionSecurity::Tls),
    )
    .await
    .unwrap();
    imap::select(&mut session, "INBOX").await.unwrap();
    let result = imap::uid_move(&mut session, 42, "Archive").await;
    imap::logout(session).await;
    task.await.unwrap();
    result
}

#[tokio::test]
async fn imap_move_uses_uidplus_fallback_when_move_is_not_advertised() {
    run_imap_move(ImapMoveFixture::LegacyUidPlus).await.unwrap();
}

#[tokio::test]
async fn rejected_advertised_move_does_not_fall_back_to_copy() {
    let error = run_imap_move(ImapMoveFixture::AdvertisedMoveRejected)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("move denied"));
}

#[tokio::test]
async fn legacy_move_without_uidplus_never_uses_mailbox_wide_expunge() {
    run_imap_move(ImapMoveFixture::LegacyWithoutUidPlus)
        .await
        .unwrap();
}

#[derive(Clone, Copy)]
enum TrashFixture {
    DeleteOneWithUidPlus,
    DeleteOneWithoutUidPlus,
    EmptyAll,
}

async fn trash_server(fixture: TrashFixture) -> (u16, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let tls = acceptor_for(CERT).accept(tcp).await.unwrap();
        let mut stream = BufReader::new(tls);
        stream.write_all(b"* OK test IMAP ready\r\n").await.unwrap();

        let login = line(&mut stream).await;
        assert!(login.contains(" LOGIN "), "unexpected command: {login:?}");
        let tag = login.split_whitespace().next().unwrap();
        stream
            .write_all(format!("{tag} OK logged in\r\n").as_bytes())
            .await
            .unwrap();
        advertise_capabilities(&mut stream, "IMAP4rev1").await;

        let select = line(&mut stream).await;
        assert!(
            select.contains(" SELECT \"Trash\""),
            "unexpected command: {select:?}"
        );
        let tag = select.split_whitespace().next().unwrap();
        stream
            .write_all(
                format!(
                    "* FLAGS (\\Seen \\Deleted)\r\n\
                     * 2 EXISTS\r\n\
                     * OK [UIDVALIDITY 1] valid\r\n\
                     * OK [UIDNEXT 43] next\r\n\
                     {tag} OK [READ-WRITE] selected\r\n"
                )
                .as_bytes(),
            )
            .await
            .unwrap();

        if !matches!(fixture, TrashFixture::EmptyAll) {
            advertise_capabilities(
                &mut stream,
                if matches!(fixture, TrashFixture::DeleteOneWithUidPlus) {
                    "IMAP4rev1 UIDPLUS"
                } else {
                    "IMAP4rev1"
                },
            )
            .await;
        }
        match fixture {
            TrashFixture::DeleteOneWithUidPlus => {
                let store = line(&mut stream).await;
                assert!(
                    store.contains(" UID STORE 42 +FLAGS.SILENT (\\Deleted)"),
                    "unexpected command: {store:?}"
                );
                let tag = store.split_whitespace().next().unwrap();
                stream
                    .write_all(format!("{tag} OK stored\r\n").as_bytes())
                    .await
                    .unwrap();
                let expunge = line(&mut stream).await;
                assert!(
                    expunge.contains(" UID EXPUNGE 42"),
                    "unexpected command: {expunge:?}"
                );
                let tag = expunge.split_whitespace().next().unwrap();
                stream
                    .write_all(format!("* 1 EXPUNGE\r\n{tag} OK expunged\r\n").as_bytes())
                    .await
                    .unwrap();
            }
            TrashFixture::DeleteOneWithoutUidPlus => {}
            TrashFixture::EmptyAll => {
                let search = line(&mut stream).await;
                assert!(
                    search.contains(" UID SEARCH ALL"),
                    "unexpected command: {search:?}"
                );
                let tag = search.split_whitespace().next().unwrap();
                stream
                    .write_all(format!("* SEARCH 41 42\r\n{tag} OK searched\r\n").as_bytes())
                    .await
                    .unwrap();
                let store = line(&mut stream).await;
                assert!(
                    store.contains(" UID STORE 1:* +FLAGS.SILENT (\\Deleted)"),
                    "unexpected command: {store:?}"
                );
                let tag = store.split_whitespace().next().unwrap();
                stream
                    .write_all(format!("{tag} OK stored\r\n").as_bytes())
                    .await
                    .unwrap();
                let expunge = line(&mut stream).await;
                assert!(
                    expunge.contains(" EXPUNGE") && !expunge.contains(" UID EXPUNGE"),
                    "unexpected command: {expunge:?}"
                );
                let tag = expunge.split_whitespace().next().unwrap();
                stream
                    .write_all(format!("* 1 EXPUNGE\r\n{tag} OK expunged\r\n").as_bytes())
                    .await
                    .unwrap();
            }
        }

        let logout = line(&mut stream).await;
        assert!(logout.contains(" LOGOUT"), "unexpected command: {logout:?}");
        let tag = logout.split_whitespace().next().unwrap();
        stream
            .write_all(format!("* BYE closing\r\n{tag} OK logout\r\n").as_bytes())
            .await
            .unwrap();
    });
    (port, task)
}

async fn run_trash_fixture(fixture: TrashFixture) -> flectar_mail_core::error::Result<()> {
    let (port, task) = trash_server(fixture).await;
    let mut session = imap::connect_with_settings(
        "127.0.0.1",
        port,
        credentials(),
        &settings(ConnectionSecurity::Tls),
    )
    .await
    .unwrap();
    imap::select(&mut session, "Trash").await.unwrap();
    let result = match fixture {
        TrashFixture::DeleteOneWithUidPlus | TrashFixture::DeleteOneWithoutUidPlus => {
            imap::uid_delete_permanently(&mut session, 42).await
        }
        TrashFixture::EmptyAll => imap::empty_selected_trash(&mut session).await,
    };
    imap::logout(session).await;
    task.await.unwrap();
    result
}

#[tokio::test]
async fn permanent_delete_uses_uid_expunge_for_only_the_selected_message() {
    run_trash_fixture(TrashFixture::DeleteOneWithUidPlus)
        .await
        .unwrap();
}

#[tokio::test]
async fn permanent_delete_without_uidplus_leaves_other_deleted_messages_untouched() {
    let error = run_trash_fixture(TrashFixture::DeleteOneWithoutUidPlus)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("UIDPLUS"));
}

#[tokio::test]
async fn empty_trash_expunge_covers_the_entire_selected_mailbox() {
    run_trash_fixture(TrashFixture::EmptyAll).await.unwrap();
}

async fn smtp_connection(mode: ConnectionSecurity) {
    smtp_connection_with_certificate(mode, CERT).await;
}

async fn smtp_connection_with_certificate(mode: ConnectionSecurity, certificate: &'static str) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let tcp = if mode == ConnectionSecurity::Starttls {
            let mut plain = BufReader::new(tcp);
            plain.write_all(b"220 localhost ESMTP\r\n").await.unwrap();
            assert!(line(&mut plain).await.starts_with("EHLO "));
            plain
                .write_all(b"250-localhost\r\n250 STARTTLS\r\n")
                .await
                .unwrap();
            assert_eq!(line(&mut plain).await, "STARTTLS\r\n");
            plain.write_all(b"220 Ready\r\n").await.unwrap();
            plain.into_inner()
        } else {
            tcp
        };
        let mut stream = BufReader::new(acceptor_for(certificate).accept(tcp).await.unwrap());
        if mode == ConnectionSecurity::Tls {
            stream.write_all(b"220 localhost ESMTP\r\n").await.unwrap();
        }
        assert!(line(&mut stream).await.starts_with("EHLO "));
        stream
            .write_all(b"250-localhost\r\n250 AUTH PLAIN\r\n")
            .await
            .unwrap();
        assert!(line(&mut stream).await.starts_with("AUTH PLAIN "));
        stream.write_all(b"235 Authenticated\r\n").await.unwrap();
        assert_eq!(line(&mut stream).await, "NOOP\r\n");
        stream.write_all(b"250 OK\r\n").await.unwrap();
        assert_eq!(line(&mut stream).await, "QUIT\r\n");
        stream.write_all(b"221 Goodbye\r\n").await.unwrap();
    });
    let config = smtp_config(port, mode, certificate);
    smtp::test_connection(&config, &smtp::SmtpAuth::Password("bridge-password".into()))
        .await
        .unwrap();
    task.await.unwrap();
}

async fn smtp_test_and_delivery_server() -> (
    u16,
    tokio::sync::oneshot::Receiver<String>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (delivered_tx, delivered_rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let mut delivered_tx = Some(delivered_tx);
        for delivery in [false, true] {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(acceptor_for(CERT).accept(tcp).await.unwrap());
            stream.write_all(b"220 localhost ESMTP\r\n").await.unwrap();
            assert!(line(&mut stream).await.starts_with("EHLO "));
            stream
                .write_all(b"250-localhost\r\n250 AUTH PLAIN\r\n")
                .await
                .unwrap();
            assert!(line(&mut stream).await.starts_with("AUTH PLAIN "));
            stream.write_all(b"235 Authenticated\r\n").await.unwrap();
            if !delivery {
                assert_eq!(line(&mut stream).await, "NOOP\r\n");
                stream.write_all(b"250 OK\r\n").await.unwrap();
            } else {
                assert!(line(&mut stream).await.starts_with("MAIL FROM:"));
                stream.write_all(b"250 Sender accepted\r\n").await.unwrap();
                assert!(line(&mut stream).await.starts_with("RCPT TO:"));
                stream
                    .write_all(b"250 Recipient accepted\r\n")
                    .await
                    .unwrap();
                assert_eq!(line(&mut stream).await, "DATA\r\n");
                stream.write_all(b"354 Continue\r\n").await.unwrap();
                let mut message = String::new();
                loop {
                    let next = line(&mut stream).await;
                    if next == ".\r\n" {
                        break;
                    }
                    message.push_str(&next);
                }
                stream.write_all(b"250 Queued\r\n").await.unwrap();
                delivered_tx.take().unwrap().send(message).unwrap();
            }
            assert_eq!(line(&mut stream).await, "QUIT\r\n");
            stream.write_all(b"221 Goodbye\r\n").await.unwrap();
        }
    });
    (port, delivered_rx, task)
}

fn smtp_config(port: u16, mode: ConnectionSecurity, certificate: &str) -> AccountConfig {
    AccountConfig {
        id: 1,
        email: "user@example.com".into(),
        display_name: None,
        avatar_url: None,
        provider: Provider::Generic,
        auth_kind: AuthKind::Password,
        mail_protocol: MailProtocol::Imap,
        username: "bridge-user".into(),
        jmap_url: String::new(),
        jmap_account_id: None,
        incoming_host: "127.0.0.1".into(),
        incoming_port: 993,
        smtp_host: "127.0.0.1".into(),
        smtp_port: port,
        settings: AccountSettings::new(settings_with_certificate(mode, certificate)),
    }
}
/// A relay that takes the message and then never answers must not hold the
/// submission worker forever.
#[tokio::test]
async fn smtp_send_gives_up_on_a_stalled_relay() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (stalled_tx, stalled) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut stream = BufReader::new(acceptor_for(CERT).accept(tcp).await.unwrap());
        stream.write_all(b"220 localhost ESMTP\r\n").await.unwrap();
        assert!(line(&mut stream).await.starts_with("EHLO "));
        stream
            .write_all(b"250-localhost\r\n250 AUTH PLAIN\r\n")
            .await
            .unwrap();
        assert!(line(&mut stream).await.starts_with("AUTH PLAIN "));
        stream.write_all(b"235 Authenticated\r\n").await.unwrap();
        assert!(line(&mut stream).await.starts_with("MAIL FROM:"));
        stream.write_all(b"250 OK\r\n").await.unwrap();
        assert!(line(&mut stream).await.starts_with("RCPT TO:"));
        stream.write_all(b"250 OK\r\n").await.unwrap();
        assert_eq!(line(&mut stream).await, "DATA\r\n");
        stream.write_all(b"354 Continue\r\n").await.unwrap();
        while line(&mut stream).await != ".\r\n" {}
        stalled_tx.send(()).unwrap();
        std::future::pending::<()>().await;
    });
    let config = smtp_config(port, ConnectionSecurity::Tls, CERT);
    let send = tokio::spawn(async move {
        smtp::send_raw(
            &config,
            &smtp::SmtpAuth::Password("bridge-password".into()),
            "user@example.com",
            &["friend@example.com".into()],
            b"Subject: Hello\r\n\r\nBody\r\n",
        )
        .await
    });
    // The exchange runs in real time. Time that advances on its own while the
    // handshake still waits on the network would fire unrelated deadlines, so
    // the clock only jumps past the idle deadline once the relay has stalled.
    tokio::time::timeout(std::time::Duration::from_secs(10), stalled)
        .await
        .expect("the relay never received the message")
        .unwrap();
    tokio::time::pause();
    tokio::time::advance(std::time::Duration::from_secs(61)).await;
    let error = send.await.unwrap().unwrap_err();
    assert!(
        error.to_string().contains("connection timed out after 60s"),
        "{error}"
    );
    server.abort();
}

#[tokio::test]
async fn smtp_starttls_on_custom_port() {
    smtp_connection(ConnectionSecurity::Starttls).await;
}
#[tokio::test]
async fn smtp_implicit_tls_on_custom_port() {
    smtp_connection(ConnectionSecurity::Tls).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn smtp_delivery_does_not_wait_for_the_imap_sync_actor() {
    smtp_sent_policy(Provider::Generic, SentCopyPolicy::Auto, 1).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn smtp_microsoft_auto_save_skips_append_and_client_override_restores_it() {
    smtp_sent_policy(Provider::Microsoft, SentCopyPolicy::Auto, 0).await;
    smtp_sent_policy(Provider::Microsoft, SentCopyPolicy::Client, 1).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn smtp_server_override_takes_effect_without_restarting_worker() {
    smtp_sent_policy(Provider::Generic, SentCopyPolicy::Server, 0).await;
}

async fn smtp_sent_policy(provider: Provider, policy: SentCopyPolicy, expected_appends: i64) {
    let (incoming_port, imap_task) = imap_server(false, false).await;
    let (smtp_port, delivered, smtp_task) = smtp_test_and_delivery_server().await;
    let temp = tempfile::tempdir().unwrap();
    let credentials = Arc::new(DevelopmentFileCredentialStore::new(
        temp.path().join("credentials.json"),
    ));
    let core = Core::start_mail_ui_with_credentials(Paths::for_tests(temp.path()), credentials)
        .await
        .unwrap();
    let account = core
        .add_account_password(AddPasswordAccountArgs {
            email: "sender@example.com".into(),
            display_name: Some("Sender".into()),
            username: "sender@example.com".into(),
            password: "test-password".into(),
            mail_protocol: MailProtocol::Imap,
            jmap_url: String::new(),
            incoming_host: "127.0.0.1".into(),
            incoming_port,
            smtp_host: "127.0.0.1".into(),
            smtp_port,
            connection: settings(ConnectionSecurity::Tls),
        })
        .await
        .unwrap();
    // The only IMAP fixture connection was consumed by account validation.
    // The spawned sync actor is now offline, which used to leave this send in
    // `pending` until the composer's 60-second watchdog cancelled it.
    imap_task.await.unwrap();

    // Settings edits must take effect without restarting the SMTP worker.
    let account_id = account.id;
    core.db
        .write(move |conn| {
            conn.execute(
                "UPDATE accounts SET provider=?2 WHERE id=?1",
                rusqlite::params![account_id, provider.as_str()],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    core.set_account_sent_copy_policy(account_id, policy)
        .await
        .unwrap();

    let draft_id = core
        .save_draft(SaveDraftArgs {
            draft_id: None,
            account_id: account.id,
            from: None,
            to: vec![Address {
                name: Some("Recipient".into()),
                email: "recipient@example.com".into(),
            }],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "Independent SMTP dispatch".into(),
            body_text: "The IMAP actor is intentionally offline.".into(),
            body_html: None,
            mode: "new".into(),
            in_reply_to_message_id: None,
            attachments: Vec::new(),
        })
        .await
        .unwrap();
    let queued = core
        .queue_send(QueueSendArgs {
            draft_id,
            send_at: Some(now_ms()),
        })
        .await
        .unwrap();

    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        core.wait_for_send(queued.action_id),
    )
    .await
    .expect("dedicated SMTP worker did not start promptly")
    .unwrap();
    let message = tokio::time::timeout(std::time::Duration::from_secs(2), delivered)
        .await
        .unwrap()
        .unwrap();
    assert!(message.contains("Subject: Independent SMTP dispatch"));
    assert!(message.contains("X-Flectar-Submission-ID:"));
    assert!(message.contains("The IMAP actor is intentionally offline."));

    core.db
        .read(move |conn| {
            let state: String = conn.query_row(
                "SELECT state FROM pending_actions WHERE id=?1",
                [queued.action_id],
                |row| row.get(0),
            )?;
            let sent_follow_up: i64 = conn.query_row(
                "SELECT COUNT(*) FROM pending_actions
                 WHERE message_id=?1 AND kind='append_sent' AND state='pending'",
                [draft_id],
                |row| row.get(0),
            )?;
            assert_eq!(state, "done");
            assert_eq!(sent_follow_up, expected_appends);
            Ok(())
        })
        .await
        .unwrap();
    smtp_task.await.unwrap();
}

#[tokio::test]
async fn proton_bridge_ca_certificate_works_for_imap_and_smtp() {
    let (port, task) = imap_server_with_certificate(true, false, PROTON_BRIDGE_CERT).await;
    let session = imap::connect_with_settings(
        "127.0.0.1",
        port,
        credentials(),
        &settings_with_certificate(ConnectionSecurity::Starttls, PROTON_BRIDGE_CERT),
    )
    .await
    .unwrap();
    imap::logout(session).await;
    task.await.unwrap();

    smtp_connection_with_certificate(ConnectionSecurity::Starttls, PROTON_BRIDGE_CERT).await;
}

#[tokio::test]
async fn proton_bridge_ca_certificate_works_when_sending_mail() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut plain = BufReader::new(tcp);
        plain.write_all(b"220 localhost ESMTP\r\n").await.unwrap();
        assert!(line(&mut plain).await.starts_with("EHLO "));
        plain
            .write_all(b"250-localhost\r\n250 STARTTLS\r\n")
            .await
            .unwrap();
        assert_eq!(line(&mut plain).await, "STARTTLS\r\n");
        plain.write_all(b"220 Ready\r\n").await.unwrap();

        let tls = acceptor_for(PROTON_BRIDGE_CERT)
            .accept(plain.into_inner())
            .await
            .unwrap();
        let mut stream = BufReader::new(tls);
        assert!(line(&mut stream).await.starts_with("EHLO "));
        stream
            .write_all(b"250-localhost\r\n250 AUTH PLAIN\r\n")
            .await
            .unwrap();
        assert!(line(&mut stream).await.starts_with("AUTH PLAIN "));
        stream.write_all(b"235 Authenticated\r\n").await.unwrap();
        assert!(line(&mut stream).await.starts_with("MAIL FROM:"));
        stream.write_all(b"250 Sender accepted\r\n").await.unwrap();
        assert!(line(&mut stream).await.starts_with("RCPT TO:"));
        stream
            .write_all(b"250 Recipient accepted\r\n")
            .await
            .unwrap();
        assert_eq!(line(&mut stream).await, "DATA\r\n");
        stream
            .write_all(b"354 End data with <CR><LF>.<CR><LF>\r\n")
            .await
            .unwrap();
        let mut message = String::new();
        loop {
            let next = line(&mut stream).await;
            if next == ".\r\n" {
                break;
            }
            message.push_str(&next);
        }
        assert!(message.contains("Subject: Bridge test"));
        assert!(message.contains("Bridge body"));
        stream.write_all(b"250 Queued\r\n").await.unwrap();
        assert_eq!(line(&mut stream).await, "QUIT\r\n");
        stream.write_all(b"221 Goodbye\r\n").await.unwrap();
    });

    smtp::send_raw(
        &smtp_config(port, ConnectionSecurity::Starttls, PROTON_BRIDGE_CERT),
        &smtp::SmtpAuth::Password("bridge-password".into()),
        "user@example.com",
        &["recipient@example.com".into()],
        b"From: user@example.com\r\nTo: recipient@example.com\r\nSubject: Bridge test\r\n\r\nBridge body\r\n",
    )
    .await
    .unwrap();
    task.await.unwrap();
}

#[tokio::test]
async fn proton_bridge_ca_certificate_must_match_the_import() {
    let (port, task) = imap_server_with_certificate(false, false, PROTON_BRIDGE_CERT).await;
    let error = imap::connect_with_settings(
        "127.0.0.1",
        port,
        credentials(),
        &settings(ConnectionSecurity::Tls),
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("CaUsedAsEndEntity"), "{error}");
    task.await.unwrap();
}

#[tokio::test]
async fn proton_bridge_ca_certificate_still_checks_hostname() {
    let (port, task) = imap_server_with_certificate(false, false, PROTON_BRIDGE_CERT).await;
    let error = imap::connect_with_settings(
        "localhost",
        port,
        credentials(),
        &settings_with_certificate(ConnectionSecurity::Tls, PROTON_BRIDGE_CERT),
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("not valid for name"), "{error}");
    task.await.unwrap();
}

#[test]
fn rejects_invalid_certificate_imports() {
    assert!(net::tls::trusted_certificates(KEY).is_err());
    assert!(net::tls::trusted_certificates(&format!("{CERT}\n{KEY}")).is_err());
    assert!(net::tls::trusted_certificates("not a certificate").is_err());
    assert!(net::tls::trusted_certificates(&"x".repeat(256 * 1024 + 1)).is_err());
    assert_eq!(net::tls::trusted_certificates(CERT).unwrap().len(), 1);
}
#[test]
fn connection_settings_round_trip() {
    let configured = AccountSettings::new(settings(ConnectionSecurity::Starttls));
    let restored: AccountSettings =
        serde_json::from_str(&serde_json::to_string(&configured).unwrap()).unwrap();
    assert_eq!(configured.connection, restored.connection);
}

#[tokio::test]
async fn imported_certificate_still_checks_hostname() {
    const WRONG: &str = include_str!("fixtures/tls/wrong-host.pem");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        assert!(acceptor_for(WRONG).accept(tcp).await.is_err());
    });
    let mut config = settings(ConnectionSecurity::Tls);
    config.trusted_certificate_pem = WRONG.into();
    let error = imap::connect_with_settings("127.0.0.1", port, credentials(), &config)
        .await
        .err()
        .unwrap();
    assert!(error.to_string().contains("not valid for name"), "{error}");
    task.await.unwrap();
}

#[tokio::test]
async fn smtp_failure_prevents_saving_account() {
    use flectar_mail_core::{
        Core, accounts::credentials::DevelopmentFileCredentialStore, config::Paths,
    };
    let temp = tempfile::tempdir().unwrap();
    let core = Core::start_mail_ui_with_credentials(
        Paths::for_tests(temp.path()),
        Arc::new(DevelopmentFileCredentialStore::new(
            temp.path().join("test-credentials.json"),
        )),
    )
    .await
    .unwrap();
    let (incoming_port, imap_task) = imap_server(true, false).await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let smtp_port = listener.local_addr().unwrap().port();
    let smtp_task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        stream
            .write_all(b"554 Sending unavailable\r\n")
            .await
            .unwrap();
    });
    let error = core
        .add_account_password(AddPasswordAccountArgs {
            email: "user@example.com".into(),
            display_name: None,
            username: "bridge-user".into(),
            // Password managers and browser copy actions can include a line
            // ending even though the password field itself is single-line.
            password: "\r\nbridge-password\r\n".into(),
            mail_protocol: MailProtocol::Imap,
            jmap_url: String::new(),
            incoming_host: "127.0.0.1".into(),
            incoming_port,
            smtp_host: "127.0.0.1".into(),
            smtp_port,
            connection: settings(ConnectionSecurity::Starttls),
        })
        .await
        .unwrap_err();
    assert!(error.to_string().contains("smtp"), "{error}");
    assert!(core.list_accounts().await.unwrap().is_empty());
    imap_task.await.unwrap();
    smtp_task.await.unwrap();
}

#[tokio::test]
async fn imap_mailbox_failure_prevents_saving_account() {
    use flectar_mail_core::{
        Core, accounts::credentials::DevelopmentFileCredentialStore, config::Paths,
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let incoming_port = listener.local_addr().unwrap().port();
    let imap_task = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let tls = acceptor_for(CERT).accept(tcp).await.unwrap();
        let mut stream = BufReader::new(tls);
        stream.write_all(b"* OK test IMAP ready\r\n").await.unwrap();

        let login = line(&mut stream).await;
        assert!(login.contains(" LOGIN "), "unexpected command: {login:?}");
        let tag = login.split_whitespace().next().unwrap();
        stream
            .write_all(format!("{tag} OK logged in\r\n").as_bytes())
            .await
            .unwrap();
        advertise_capabilities(&mut stream, "IMAP4rev1 ID").await;
        accept_client_identification(&mut stream).await;

        let select = line(&mut stream).await;
        assert!(
            select.contains(r#" SELECT "INBOX""#),
            "unexpected command: {select:?}"
        );
        let tag = select.split_whitespace().next().unwrap();
        stream
            .write_all(format!("{tag} NO SELECT Unsafe Login\r\n").as_bytes())
            .await
            .unwrap();

        let logout = line(&mut stream).await;
        assert!(logout.contains(" LOGOUT"), "unexpected command: {logout:?}");
        let tag = logout.split_whitespace().next().unwrap();
        stream
            .write_all(format!("* BYE closing\r\n{tag} OK logout\r\n").as_bytes())
            .await
            .unwrap();
    });

    let temp = tempfile::tempdir().unwrap();
    let core = Core::start_mail_ui_with_credentials(
        Paths::for_tests(temp.path()),
        Arc::new(DevelopmentFileCredentialStore::new(
            temp.path().join("test-credentials.json"),
        )),
    )
    .await
    .unwrap();
    let error = core
        .add_account_password(AddPasswordAccountArgs {
            email: "user@example.com".into(),
            display_name: None,
            username: "bridge-user".into(),
            password: "bridge-password".into(),
            mail_protocol: MailProtocol::Imap,
            jmap_url: String::new(),
            incoming_host: "127.0.0.1".into(),
            incoming_port,
            smtp_host: "127.0.0.1".into(),
            smtp_port: 1,
            connection: settings(ConnectionSecurity::Tls),
        })
        .await
        .unwrap_err();
    assert!(error.to_string().contains("Unsafe Login"), "{error}");
    assert!(core.list_accounts().await.unwrap().is_empty());
    imap_task.await.unwrap();
}

#[tokio::test]
async fn backup_import_preserves_transport_settings_and_rejects_private_keys() {
    use flectar_mail_core::{Core, config::Paths};
    let temp = tempfile::tempdir().unwrap();
    let core = Core::start_mail_ui(Paths::for_tests(temp.path()))
        .await
        .unwrap();
    let mut config = PortableAccountConfig {
        email: "user@example.com".into(),
        display_name: None,
        provider: Provider::Generic,
        auth_kind: AuthKind::Password,
        mail_protocol: MailProtocol::Imap,
        username: "bridge-user".into(),
        jmap_url: String::new(),
        incoming_host: "127.0.0.1".into(),
        incoming_port: 1143,
        smtp_host: "127.0.0.1".into(),
        smtp_port: 1025,
        settings: AccountSettings::new(settings(ConnectionSecurity::Starttls)),
    };
    core.import_account_configs(vec![config.clone()])
        .await
        .unwrap();
    assert_eq!(
        core.list_account_configs().await.unwrap()[0]
            .settings
            .connection,
        config.settings.connection
    );
    config.email = "other@example.com".into();
    config.settings.connection.trusted_certificate_pem = KEY.into();
    assert!(core.import_account_configs(vec![config]).await.is_err());
    assert_eq!(core.list_accounts().await.unwrap().len(), 1);
}

#[tokio::test]
async fn another_gmail_spelling_does_not_edit_a_password_account() {
    let temp = tempfile::tempdir().unwrap();
    let core = Core::start_mail_ui_with_credentials(
        Paths::for_tests(temp.path()),
        Arc::new(DevelopmentFileCredentialStore::new(
            temp.path().join("test-credentials.json"),
        )),
    )
    .await
    .unwrap();
    core.import_account_configs(vec![PortableAccountConfig {
        email: "johndoe@gmail.com".into(),
        display_name: None,
        provider: Provider::Generic,
        auth_kind: AuthKind::Password,
        mail_protocol: MailProtocol::Imap,
        username: "johndoe@gmail.com".into(),
        jmap_url: String::new(),
        incoming_host: "imap.gmail.com".into(),
        incoming_port: 993,
        smtp_host: "smtp.gmail.com".into(),
        smtp_port: 465,
        settings: AccountSettings::new(settings(ConnectionSecurity::Tls)),
    }])
    .await
    .unwrap();

    // Taking the new spelling would look like another user to the update
    // and reset the account's mail, so the form names the existing account.
    let error = core
        .add_account_password(AddPasswordAccountArgs {
            email: "John.Doe@gmail.com".into(),
            display_name: None,
            username: "john.doe@gmail.com".into(),
            password: "app-password".into(),
            mail_protocol: MailProtocol::Imap,
            jmap_url: String::new(),
            incoming_host: "imap.gmail.com".into(),
            incoming_port: 993,
            smtp_host: "smtp.gmail.com".into(),
            smtp_port: 465,
            connection: settings(ConnectionSecurity::Tls),
        })
        .await
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "auth failed: johndoe@gmail.com is already added"
    );
    let configs = core.list_account_configs().await.unwrap();
    assert_eq!(configs.len(), 1);
    assert_eq!(configs[0].username, "johndoe@gmail.com");
}
