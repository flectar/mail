//! First-sync behavior against a live Dovecot: with a mailbox bigger than one
//! body-fetch budget, the actor's bodies-only drain cycles must cache every
//! body quickly (the old 30-bodies-per-60s trickle would take ~3 minutes for
//! this fixture), and the backfill must emit MailUpdated so the UI fills in
//! live. Gated: FLECTAR_MAIL_TEST_IMAP=1 (see dovecot_e2e.rs for the container).

use flectar_mail_core::Core;
use flectar_mail_core::config::Paths;
use flectar_mail_core::db::Db;
use flectar_mail_core::events::CoreEvent;
use flectar_mail_core::imap;
use flectar_mail_core::models::*;
use std::time::{Duration, Instant};

// Three maximum-size selective UID batches. This catches regressions back to
// one network round trip or one SQLite commit per message.
const MSGS: usize = 600;

async fn seed() {
    let creds = imap::ImapCredentials::Password {
        user: "senduser".into(),
        password: "pass".into(),
    };
    let mut s = imap::connect("127.0.0.1", 10993, creds)
        .await
        .expect("seed connect");
    for folder in ["Sent", "Drafts", "Trash", "Junk", "Archive"] {
        let _ = imap::create_folder(&mut s, folder).await;
    }
    for folder in ["INBOX", "Sent"] {
        if imap::select(&mut s, folder).await.is_ok() {
            for uid in imap::uid_search_all(&mut s).await.unwrap_or_default() {
                let _ = imap::store_flag(&mut s, uid, "\\Deleted", true).await;
            }
            let _ = imap::expunge_all(&mut s).await;
        }
    }
    imap::select(&mut s, "INBOX").await.expect("select");
    let date = chrono::Utc::now().to_rfc2822();
    for i in 0..MSGS {
        let raw = format!(
            "Message-ID: <bulk{i}@example.com>\r\nFrom: Sender {i} <s{i}@example.com>\r\n\
             To: Send User <senduser@example.com>\r\nSubject: Bulk message {i}\r\n\
             Date: {date}\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n\
             Body of bulk message number {i}.\r\n"
        );
        imap::append(&mut s, "INBOX", raw.as_bytes(), false)
            .await
            .expect("append");
    }
    imap::logout(s).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn first_sync_drains_bodies_fast_and_updates_live() {
    if std::env::var("FLECTAR_MAIL_TEST_IMAP").as_deref() != Ok("1") {
        eprintln!("skipping (set FLECTAR_MAIL_TEST_IMAP=1)");
        return;
    }
    // SAFETY: single-threaded test setup, before any threads read the env.
    unsafe { std::env::set_var("FLECTAR_MAIL_TLS_INSECURE", "1") };
    let creds_file = std::env::temp_dir().join(format!(
        "flectar-mail-firstsync-creds-{}.json",
        std::process::id()
    ));
    unsafe { std::env::set_var("FLECTAR_MAIL_CREDENTIALS_INSECURE_FILE", &creds_file) };

    seed().await;

    let tmp = tempfile::tempdir().unwrap();
    let core = Core::start(Paths::for_tests(tmp.path())).await.unwrap();
    let mut events = core.bus.subscribe();

    let started = Instant::now();
    core.add_account_password(AddPasswordAccountArgs {
        connection: Default::default(),
        email: "senduser@example.com".into(),
        display_name: Some("Send User".into()),
        username: "senduser".into(),
        password: "pass".into(),
        mail_protocol: flectar_mail_core::models::MailProtocol::Imap,
        jmap_url: String::new(),
        imap_host: "127.0.0.1".into(),
        imap_port: 10993,
        smtp_host: "127.0.0.1".into(),
        smtp_port: 10588,
    })
    .await
    .expect("add account");

    // Every body must be cached well before the old one-budget-per-60s pace.
    // The fixture spans three 200-UID batches so it also exercises queue
    // handoff between high-throughput selective fetches.
    let db = Db::open(&Paths::for_tests(tmp.path()).db_file()).unwrap();
    let deadline = Duration::from_secs(45);
    loop {
        let (total, cached): (i64, i64) = db
            .read(|conn| {
                Ok(conn.query_row(
                    "SELECT COUNT(*), SUM(body_state = 'cached') FROM messages",
                    [],
                    |r| Ok((r.get(0)?, r.get::<_, Option<i64>>(1)?.unwrap_or(0))),
                )?)
            })
            .await
            .unwrap();
        if total >= MSGS as i64 && cached == total {
            break;
        }
        assert!(
            started.elapsed() < deadline,
            "bodies not drained in {deadline:?}: {cached}/{total} cached"
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    eprintln!("all {MSGS} bodies cached in {:?}", started.elapsed());

    // The backfill emitted MailUpdated along the way (live UI fill-in).
    let mut saw_mail_updated = false;
    while let Ok(ev) = events.try_recv() {
        if matches!(ev, CoreEvent::MailUpdated { .. }) {
            saw_mail_updated = true;
            break;
        }
    }
    assert!(saw_mail_updated, "initial backfill should emit MailUpdated");

    // And the account settles back to idle once caught up.
    let settle = Instant::now();
    loop {
        let accs = core.list_accounts().await.unwrap();
        if accs[0].sync_state == "idle" {
            break;
        }
        assert!(
            settle.elapsed() < Duration::from_secs(10),
            "account should return to idle after the drain, got {}",
            accs[0].sync_state
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let _ = std::fs::remove_file(&creds_file);
}
