//! Unread mail older than an account's mail history, which only the server
//! keeps, counted against a local Dovecot (see dovecot_e2e.rs).
//!
//! Gated: runs only when FLECTAR_MAIL_TEST_IMAP=1.

use chrono::{Duration as Days, Utc};
use flectar_mail_core::Core;
use flectar_mail_core::config::Paths;
use flectar_mail_core::imap;
use flectar_mail_core::models::*;
use std::time::{Duration, Instant};

fn gated() -> bool {
    std::env::var("FLECTAR_MAIL_TEST_IMAP").is_ok_and(|value| value == "1")
}

async fn session() -> imap::Session {
    imap::connect(
        "127.0.0.1",
        10993,
        imap::ImapCredentials::Password {
            user: "testuser".into(),
            password: "pass".into(),
        },
    )
    .await
    .expect("connect")
}

/// Files a message in the Inbox, received `age` ago; returns its UID.
async fn deliver(session: &mut imap::Session, subject: &str, age: Days, seen: bool) -> u32 {
    let received = Utc::now() - age;
    let raw = format!(
        "Message-ID: <{}@example.com>\r\nFrom: Sender <sender@example.com>\r\n\
         To: testuser@example.com\r\nSubject: {subject}\r\nDate: {}\r\n\r\nHello.\r\n",
        subject.to_lowercase().replace(' ', "-"),
        received.to_rfc2822(),
    );
    imap::append_message(
        session,
        "INBOX",
        raw.as_bytes(),
        imap::AppendOptions {
            seen,
            flagged: false,
            internal_date: Some(received),
        },
    )
    .await
    .expect("append")
    .expect("Dovecot reports the UID of appended mail")
    .uid
}

async fn wait_for<T, F, Fut>(what: &str, timeout: Duration, mut f: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let start = Instant::now();
    loop {
        if let Some(value) = f().await {
            return value;
        }
        assert!(
            start.elapsed() < timeout,
            "timed out after {timeout:?} waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Whether the account's Inbox shows a conversation with this subject.
async fn shows(core: &Core, account_id: i64, subject: &str) -> bool {
    core.list_threads(View::Inbox, None, None, None, None, (None, 50))
        .await
        .unwrap()
        .threads
        .iter()
        .any(|thread| thread.account_id == account_id && thread.subject == subject)
}

#[tokio::test(flavor = "multi_thread")]
async fn older_unread_mail_is_counted_as_only_on_the_server() {
    if !gated() {
        eprintln!("skipping (set FLECTAR_MAIL_TEST_IMAP=1 to run)");
        return;
    }
    // SAFETY: single-threaded test setup, before any threads read the env.
    unsafe { std::env::set_var("FLECTAR_MAIL_TLS_INSECURE", "1") };
    let credentials = std::env::temp_dir().join(format!(
        "flectar-mail-server-only-creds-{}.json",
        std::process::id()
    ));
    unsafe { std::env::set_var("FLECTAR_MAIL_CREDENTIALS_INSECURE_FILE", &credentials) };

    // The default mail history keeps six months: one unread message is
    // older, and one more is older but read.
    let mut server = session().await;
    imap::select(&mut server, "INBOX")
        .await
        .expect("select inbox");
    for uid in imap::uid_search_all(&mut server).await.unwrap_or_default() {
        let _ = imap::store_flag(&mut server, uid, "\\Deleted", true).await;
    }
    let _ = imap::expunge_all(&mut server).await;
    deliver(&mut server, "Recent news", Days::days(1), false).await;
    let old_unread = deliver(&mut server, "Old invoice", Days::days(400), false).await;
    deliver(&mut server, "Old receipt", Days::days(420), true).await;

    let directory = tempfile::tempdir().unwrap();
    let core = Core::start(Paths::for_tests(directory.path()))
        .await
        .unwrap();
    let account_id = core
        .add_account_password(AddPasswordAccountArgs {
            connection: MailConnectionSettings::new(
                ConnectionSecurity::Tls,
                ConnectionSecurity::Starttls,
            ),
            email: "testuser@example.com".into(),
            display_name: None,
            username: "testuser".into(),
            password: "pass".into(),
            mail_protocol: MailProtocol::Imap,
            jmap_url: String::new(),
            incoming_host: "127.0.0.1".into(),
            incoming_port: 10993,
            smtp_host: "127.0.0.1".into(),
            smtp_port: 10587,
        })
        .await
        .expect("add account")
        .id;
    let inbox = wait_for("the Inbox", Duration::from_secs(60), || async {
        core.list_folders(Some(account_id))
            .await
            .unwrap()
            .into_iter()
            .find(|folder| folder.role.as_deref() == Some(roles::INBOX))
            .map(|folder| folder.id)
    })
    .await;
    let only_on_server = || async { core.server_only_unread(vec![inbox]).await.unwrap() };

    wait_for("the recent message", Duration::from_secs(60), || async {
        shows(&core, account_id, "Recent news").await.then_some(())
    })
    .await;
    assert!(!shows(&core, account_id, "Old invoice").await);
    // New mail arrives here and on the server alike.
    deliver(&mut server, "Fresh reply", Days::zero(), false).await;
    wait_for(
        "one older unread message counted",
        Duration::from_secs(60),
        || async {
            let arrived = shows(&core, account_id, "Fresh reply").await;
            (arrived && only_on_server().await == Some(1)).then_some(())
        },
    )
    .await;

    // Read elsewhere, it is no longer unread anywhere.
    imap::select(&mut server, "INBOX").await.unwrap();
    imap::store_flag(&mut server, old_unread, "\\Seen", true)
        .await
        .unwrap();
    wait_for(
        "the older message read elsewhere",
        Duration::from_secs(60),
        || async { (only_on_server().await == Some(0)).then_some(()) },
    )
    .await;
    imap::logout(server).await;
}
