//! Moving mail between two IMAP accounts on a local Dovecot (see
//! dovecot_e2e.rs), with users `testuser` and `probeuser`.
//!
//! Gated: runs only when FLECTAR_MAIL_TEST_IMAP=1.

use flectar_mail_core::Core;
use flectar_mail_core::config::Paths;
use flectar_mail_core::imap;
use flectar_mail_core::models::*;
use std::time::{Duration, Instant};

const MESSAGE_ID: &str = "<moved-1@example.com>";

fn gated() -> bool {
    std::env::var("FLECTAR_MAIL_TEST_IMAP").is_ok_and(|value| value == "1")
}

async fn session(user: &str) -> imap::Session {
    imap::connect(
        "127.0.0.1",
        10993,
        imap::ImapCredentials::Password {
            user: user.into(),
            password: "pass".into(),
        },
    )
    .await
    .expect("connect")
}

/// Empties a user's Inbox.
async fn empty_inbox(session: &mut imap::Session) {
    imap::select(session, "INBOX").await.expect("select inbox");
    for uid in imap::uid_search_all(session).await.unwrap_or_default() {
        let _ = imap::store_flag(session, uid, "\\Deleted", true).await;
    }
    let _ = imap::expunge_all(session).await;
}

/// The flags of the Inbox messages, by whether they are seen and flagged.
async fn inbox(user: &str) -> Vec<(bool, bool)> {
    let mut session = session(user).await;
    imap::select(&mut session, "INBOX").await.expect("select");
    let uids = imap::uid_search_all(&mut session).await.unwrap();
    let mut flags = Vec::new();
    if !uids.is_empty() {
        let set = uids
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(",");
        for header in imap::fetch_headers(&mut session, &set).await.unwrap() {
            if !header.flags.deleted {
                flags.push((header.flags.seen, header.flags.flagged));
            }
        }
    }
    imap::logout(session).await;
    flags
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
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn add(core: &Core, user: &str) -> i64 {
    core.add_account_password(AddPasswordAccountArgs {
        connection: MailConnectionSettings::new(
            ConnectionSecurity::Tls,
            ConnectionSecurity::Starttls,
        ),
        email: format!("{user}@example.com"),
        display_name: None,
        username: user.into(),
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
    .id
}

/// The account's Inbox conversation with the moved message, if it has it.
async fn moved_thread(core: &Core, account_id: i64) -> Option<i64> {
    core.list_threads(View::Inbox, None, None, None, None, (None, 50))
        .await
        .unwrap()
        .threads
        .into_iter()
        .find(|thread| thread.account_id == account_id && thread.subject == "Boarding pass")
        .map(|thread| thread.id)
}

#[tokio::test(flavor = "multi_thread")]
async fn mail_moves_between_imap_accounts_with_its_state() {
    if !gated() {
        eprintln!("skipping (set FLECTAR_MAIL_TEST_IMAP=1 to run)");
        return;
    }
    // SAFETY: single-threaded test setup, before any threads read the env.
    unsafe { std::env::set_var("FLECTAR_MAIL_TLS_INSECURE", "1") };
    let credentials = std::env::temp_dir().join(format!(
        "flectar-mail-transfer-creds-{}.json",
        std::process::id()
    ));
    unsafe { std::env::set_var("FLECTAR_MAIL_CREDENTIALS_INSECURE_FILE", &credentials) };

    // A read and starred message for testuser, nothing for probeuser.
    let mut source = session("testuser").await;
    empty_inbox(&mut source).await;
    let raw = format!(
        "Message-ID: {MESSAGE_ID}\r\nFrom: Air <tickets@example.com>\r\n\
         To: testuser@example.com\r\nSubject: Boarding pass\r\n\
         Date: Fri, 05 Jan 2024 10:00:00 +0000\r\n\r\nSeat 12A.\r\n"
    );
    imap::append(&mut source, "INBOX", raw.as_bytes(), true)
        .await
        .expect("append");
    imap::select(&mut source, "INBOX").await.unwrap();
    for uid in imap::uid_search_all(&mut source).await.unwrap() {
        imap::store_flag(&mut source, uid, "\\Flagged", true)
            .await
            .unwrap();
    }
    imap::logout(source).await;
    let mut target = session("probeuser").await;
    empty_inbox(&mut target).await;
    imap::logout(target).await;

    let directory = tempfile::tempdir().unwrap();
    let core = Core::start(Paths::for_tests(directory.path()))
        .await
        .unwrap();
    let testuser = add(&core, "testuser").await;
    let probeuser = add(&core, "probeuser").await;
    let thread = wait_for("the message to move", Duration::from_secs(60), || async {
        moved_thread(&core, testuser).await
    })
    .await;
    let probe_inbox = wait_for("probeuser's Inbox", Duration::from_secs(60), || async {
        core.list_folders(Some(probeuser))
            .await
            .unwrap()
            .into_iter()
            .find(|folder| folder.role.as_deref() == Some(roles::INBOX))
            .map(|folder| folder.id)
    })
    .await;

    let started = core
        .transfer_mail(ActionTarget::Threads(vec![thread]), probe_inbox, true)
        .await
        .unwrap();
    assert_eq!(started, 1);

    // The copy is on probeuser's server, read and starred, and the original
    // is gone from testuser's.
    wait_for(
        "the copy and no original",
        Duration::from_secs(60),
        || async {
            let copied = inbox("probeuser").await == [(true, true)];
            let removed = inbox("testuser").await.is_empty();
            (copied && removed).then_some(())
        },
    )
    .await;
    // Both accounts show it where it is now.
    wait_for("the move in the app", Duration::from_secs(60), || async {
        let arrived = moved_thread(&core, probeuser).await.is_some();
        let left = moved_thread(&core, testuser).await.is_none();
        (arrived && left).then_some(())
    })
    .await;
}
