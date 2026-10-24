//! End-to-end POP3 test against the Dovecot container of dovecot_e2e.rs. The
//! image needs POP3 switched on, and the unencrypted case needs passwords
//! allowed without TLS:
//!   printf 'protocols {\n  pop3 = yes\n}\nauth_allow_cleartext = yes\n' > pop3.conf
//!   docker run -d --name flectar-mail-dovecot -e USER_PASSWORD='{plain}pass' \
//!     -v "$PWD/pop3.conf:/etc/dovecot/conf.d/pop3.conf:ro" \
//!     -p 10993:31993 -p 10110:31110 -p 10995:31995 dovecot/dovecot:latest
//! The container's certificate is self-signed, so run with
//! FLECTAR_MAIL_TLS_INSECURE=1.
//!
//! Gated: runs only when FLECTAR_MAIL_TEST_POP3=1. The mailbox is seeded over
//! IMAP, so reruns start from the same three messages. The account part also
//! submits through the container's port 10587, as dovecot_e2e.rs does.

use flectar_mail_core::accounts::credentials::DevelopmentFileCredentialStore;
use flectar_mail_core::config::Paths;
use flectar_mail_core::models::{
    AddPasswordAccountArgs, ConnectionSecurity, KeepOnServer, MailConnectionSettings, MailProtocol,
    View,
};
use flectar_mail_core::{Core, imap, pop3};
use std::sync::Arc;
use std::time::Duration;

fn gated() -> bool {
    std::env::var("FLECTAR_MAIL_TEST_POP3")
        .map(|value| value == "1")
        .unwrap_or(false)
}

async fn seed(subjects: &[&str]) {
    let credentials = imap::ImapCredentials::Password {
        user: "testuser".into(),
        password: "pass".into(),
    };
    let mut session = imap::connect("127.0.0.1", 10993, credentials)
        .await
        .expect("seed connect");
    imap::select(&mut session, "INBOX")
        .await
        .expect("select inbox");
    for uid in imap::uid_search_all(&mut session).await.unwrap_or_default() {
        imap::store_flag(&mut session, uid, "\\Deleted", true)
            .await
            .expect("mark old mail deleted");
    }
    imap::expunge_all(&mut session).await.expect("expunge");
    for (index, subject) in subjects.iter().enumerate() {
        let raw = format!(
            "Message-ID: <pop3-e2e-{index}@example.com>\r\nFrom: Alice <alice@example.com>\r\n\
             To: Test User <testuser@example.com>\r\nSubject: {subject}\r\n\
             Date: Wed, 21 Oct 2026 10:0{index}:00 +0000\r\n\r\n\
             Body of {subject}\r\n.A line that starts with a dot\r\n"
        );
        imap::append(&mut session, "INBOX", raw.as_bytes(), false)
            .await
            .expect("seed append");
    }
    imap::logout(session).await;
}

fn server(port: u16, security: ConnectionSecurity) -> pop3::Server<'static> {
    pop3::Server {
        host: "127.0.0.1",
        port,
        security,
        trusted_certificate_pem: "",
    }
}

async fn connect(port: u16, security: ConnectionSecurity) -> pop3::Session {
    pop3::Session::connect(&server(port, security), "testuser", "pass")
        .await
        .unwrap_or_else(|error| panic!("connect on {port} with {security:?}: {error}"))
}

#[tokio::test(flavor = "multi_thread")]
async fn pop3_against_dovecot() {
    if !gated() {
        eprintln!("skipped: set FLECTAR_MAIL_TEST_POP3=1 with Dovecot running");
        return;
    }
    seed(&["first", "second", "third"]).await;

    let mut ids = None;
    for (port, security) in [
        (10110, ConnectionSecurity::None),
        (10110, ConnectionSecurity::Starttls),
        (10995, ConnectionSecurity::Tls),
    ] {
        let mut session = connect(port, security).await;
        let capabilities = session.capabilities().clone();
        assert!(capabilities.uidl && capabilities.top, "{capabilities:?}");
        let listed = session.unique_ids().await.unwrap();
        assert_eq!(listed.len(), 3);
        // Unique ids persist across sessions and connection types.
        assert_eq!(*ids.get_or_insert_with(|| listed.clone()), listed);

        let sizes = session.sizes().await.unwrap();
        let headers = session.headers(&[1, 2, 3], 64 * 1024).await.unwrap();
        for (header, subject) in headers.iter().zip(["first", "second", "third"]) {
            let header = String::from_utf8(header.clone().unwrap()).unwrap();
            assert!(header.contains(&format!("Subject: {subject}")), "{header}");
        }
        let message = session.retrieve(1, 1 << 20).await.unwrap();
        assert_eq!(message.len() as u64, sizes[0].1);
        let message = String::from_utf8(message).unwrap();
        assert!(message.contains("\r\n.A line that starts with a dot\r\n"));
        session.quit().await.unwrap();
    }
    let ids = ids.unwrap();

    match pop3::Session::connect(
        &server(10110, ConnectionSecurity::Starttls),
        "testuser",
        "wrong",
    )
    .await
    {
        Err(pop3::Error::SignIn {
            refusal: pop3::SignInRefusal::Credentials,
            ..
        }) => {}
        other => panic!("wrong password: {:?}", other.err()),
    }

    // A deletion only takes effect when the session ends with QUIT.
    let mut session = connect(10110, ConnectionSecurity::Starttls).await;
    session.delete(1).await.unwrap();
    drop(session);
    let mut session = connect(10110, ConnectionSecurity::Starttls).await;
    assert_eq!(session.unique_ids().await.unwrap(), ids);
    session.delete(1).await.unwrap();
    session.quit().await.unwrap();

    let mut session = connect(10995, ConnectionSecurity::Tls).await;
    let remaining = session.unique_ids().await.unwrap();
    assert_eq!(
        remaining.iter().map(|(_, id)| id).collect::<Vec<_>>(),
        ids[1..].iter().map(|(_, id)| id).collect::<Vec<_>>()
    );
    session.quit().await.unwrap();

    account_against_dovecot().await;
}

/// A POP3 account downloads the mailbox and leaves it on the server.
async fn account_against_dovecot() {
    seed(&["one", "two", "three"]).await;
    let temp = tempfile::tempdir().unwrap();
    let credentials = Arc::new(DevelopmentFileCredentialStore::new(
        temp.path().join("credentials.json"),
    ));
    let core = Core::start_mail_ui_with_credentials(Paths::for_tests(temp.path()), credentials)
        .await
        .unwrap();
    let account = core
        .add_account_password(AddPasswordAccountArgs {
            email: "testuser@example.com".into(),
            display_name: Some("Test User".into()),
            username: "testuser".into(),
            password: "pass".into(),
            mail_protocol: MailProtocol::Pop3,
            jmap_url: String::new(),
            incoming_host: "127.0.0.1".into(),
            incoming_port: 10995,
            smtp_host: "127.0.0.1".into(),
            smtp_port: 10587,
            connection: MailConnectionSettings::new(
                ConnectionSecurity::Tls,
                ConnectionSecurity::Starttls,
            ),
        })
        .await
        .expect("add POP3 account");
    let subjects = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let page = core
                .list_threads(View::Inbox, None, Some(account.id), None, None, (None, 50))
                .await
                .unwrap();
            if page.threads.len() == 3 {
                let mut subjects: Vec<_> = page
                    .threads
                    .into_iter()
                    .map(|thread| thread.subject)
                    .collect();
                subjects.sort();
                return subjects;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("POP3 account download");
    assert_eq!(subjects, ["one", "three", "two"]);

    let mut session = connect(10995, ConnectionSecurity::Tls).await;
    assert_eq!(session.unique_ids().await.unwrap().len(), 3);
    session.quit().await.unwrap();

    // Choosing to keep mail only until it's downloaded empties the server,
    // and the mail stays here.
    core.set_account_keep_on_server(account.id, KeepOnServer::UntilDownloaded)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let mut session = connect(10995, ConnectionSecurity::Tls).await;
            let left = session.unique_ids().await.unwrap().len();
            session.quit().await.unwrap();
            if left == 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .expect("mail removed from the server");
    let page = core
        .list_threads(View::Inbox, None, Some(account.id), None, None, (None, 50))
        .await
        .unwrap();
    assert_eq!(page.threads.len(), 3);
}
