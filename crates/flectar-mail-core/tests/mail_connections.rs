//! Local protocol fixtures exercise actual TLS handshakes without external accounts.
use flectar_mail_core::models::*;
use flectar_mail_core::{imap, smtp};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use std::sync::Arc;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::TcpListener,
};

const CERT: &str = include_str!("fixtures/tls/server.pem");
const KEY: &str = include_str!("fixtures/tls/server-key.pem");

fn acceptor() -> tokio_rustls::TlsAcceptor {
    acceptor_for(CERT)
}

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
    MailConnectionSettings {
        imap_security: mode,
        smtp_security: mode,
        trusted_certificate_pem: CERT.into(),
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
async fn imap_server(starttls: bool, reject: bool) -> (u16, tokio::task::JoinHandle<()>) {
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
        let Ok(tls) = acceptor().accept(tcp).await else {
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
        let logout = line(&mut stream).await;
        let tag = logout.split_whitespace().next().unwrap();
        stream
            .write_all(format!("* BYE closing\r\n{tag} OK logout\r\n").as_bytes())
            .await
            .unwrap();
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

async fn smtp_connection(mode: ConnectionSecurity) {
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
        let mut stream = BufReader::new(acceptor().accept(tcp).await.unwrap());
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
    let config = AccountConfig {
        id: 1,
        email: "user@example.com".into(),
        display_name: None,
        avatar_url: None,
        provider: Provider::Imap,
        auth_kind: AuthKind::Password,
        mail_protocol: MailProtocol::Imap,
        username: "bridge-user".into(),
        jmap_url: String::new(),
        jmap_account_id: None,
        imap_host: "127.0.0.1".into(),
        imap_port: 993,
        smtp_host: "127.0.0.1".into(),
        smtp_port: port,
        settings: AccountSettings {
            connection: settings(mode),
            ..Default::default()
        },
    };
    smtp::test_connection(&config, &smtp::SmtpAuth::Password("bridge-password".into()))
        .await
        .unwrap();
    task.await.unwrap();
}
#[tokio::test]
async fn smtp_starttls_on_custom_port() {
    smtp_connection(ConnectionSecurity::Starttls).await;
}
#[tokio::test]
async fn smtp_implicit_tls_on_custom_port() {
    smtp_connection(ConnectionSecurity::Tls).await;
}

#[test]
fn rejects_invalid_certificate_imports() {
    assert!(imap::trusted_certificates(KEY).is_err());
    assert!(imap::trusted_certificates(&format!("{CERT}\n{KEY}")).is_err());
    assert!(imap::trusted_certificates("not a certificate").is_err());
    assert!(imap::trusted_certificates(&"x".repeat(256 * 1024 + 1)).is_err());
    assert_eq!(imap::trusted_certificates(CERT).unwrap().len(), 1);
}
#[test]
fn settings_round_trip_and_legacy_defaults() {
    let legacy: AccountSettings = serde_json::from_str("{}").unwrap();
    assert_eq!(legacy.connection, MailConnectionSettings::default());
    let configured = AccountSettings {
        connection: settings(ConnectionSecurity::Starttls),
        ..legacy
    };
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
    let (imap_port, imap_task) = imap_server(true, false).await;
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
            password: "bridge-password".into(),
            mail_protocol: MailProtocol::Imap,
            jmap_url: String::new(),
            imap_host: "127.0.0.1".into(),
            imap_port,
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
async fn backup_import_preserves_transport_settings_and_rejects_private_keys() {
    use flectar_mail_core::{Core, config::Paths};
    let temp = tempfile::tempdir().unwrap();
    let core = Core::start_mail_ui(Paths::for_tests(temp.path()))
        .await
        .unwrap();
    let mut config = PortableAccountConfig {
        email: "user@example.com".into(),
        display_name: None,
        provider: Provider::Imap,
        auth_kind: AuthKind::Password,
        mail_protocol: MailProtocol::Imap,
        username: "bridge-user".into(),
        jmap_url: String::new(),
        imap_host: "127.0.0.1".into(),
        imap_port: 1143,
        smtp_host: "127.0.0.1".into(),
        smtp_port: 1025,
        settings: AccountSettings {
            connection: settings(ConnectionSecurity::Starttls),
            ..Default::default()
        },
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
