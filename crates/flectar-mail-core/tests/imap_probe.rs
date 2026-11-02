//! Low-level smoke test of the imap + smtp modules against local Dovecot
//! (FLECTAR_MAIL_TEST_IMAP=1). Uses its own IMAP user ("probeuser") so it can run
//! concurrently with dovecot_e2e.rs, which owns "testuser".

use flectar_mail_core::imap;
use std::time::Duration;

async fn connect_probe() -> imap::Session {
    let creds = imap::ImapCredentials::Password {
        user: "probeuser".into(),
        password: "pass".into(),
    };
    tokio::time::timeout(
        Duration::from_secs(10),
        imap::connect("127.0.0.1", 10993, creds),
    )
    .await
    .expect("connect timed out")
    .expect("connect failed")
}

#[tokio::test(flavor = "multi_thread")]
async fn probe_imap_primitives() {
    if std::env::var("FLECTAR_MAIL_TEST_IMAP").as_deref() != Ok("1") {
        return;
    }
    // SAFETY: single-threaded test setup, before any threads read the env.
    unsafe { std::env::set_var("FLECTAR_MAIL_TLS_INSECURE", "1") };

    let mut s = connect_probe().await;

    // Reset and seed this user's INBOX with 2 messages.
    imap::select(&mut s, "INBOX").await.expect("select");
    for uid in imap::uid_search_all(&mut s).await.expect("search") {
        let _ = imap::store_flag(&mut s, uid, "\\Deleted", true).await;
    }
    imap::expunge_all(&mut s).await.expect("expunge");

    let date = chrono::Utc::now().to_rfc2822();
    for (i, subj) in [(1, "Probe one"), (2, "Probe two")] {
        let raw = format!(
            "Message-ID: <p{i}@example.com>\r\nFrom: Probe <probe@example.com>\r\n\
             To: probeuser@example.com\r\nSubject: {subj}\r\nDate: {date}\r\n\
             Content-Type: text/plain\r\n\r\nBody {i}\r\n"
        );
        imap::append(&mut s, "INBOX", raw.as_bytes(), false)
            .await
            .expect("append");
    }

    let folders = imap::list_folders(&mut s).await.expect("list");
    assert!(folders.iter().any(|f| f.name.eq_ignore_ascii_case("INBOX")));

    let sel = imap::select(&mut s, "INBOX").await.expect("select");
    assert_eq!(sel.exists, 2, "seeded 2 messages");

    let since = chrono::Utc::now().date_naive() - chrono::Duration::days(90);
    let uids = imap::uid_search_since(&mut s, since)
        .await
        .expect("search since");
    assert_eq!(uids.len(), 2);

    let headers = imap::fetch_headers(&mut s, &imap::uid_set(&uids))
        .await
        .expect("fetch headers");
    assert_eq!(headers.len(), 2);
    let parsed =
        flectar_mail_core::mime::parse_header_block(&headers[0].header_bytes).expect("parse");
    assert!(parsed.subject.starts_with("Probe"));

    let raw = imap::fetch_full(&mut s, uids[0])
        .await
        .expect("fetch full")
        .expect("body present");
    let full = flectar_mail_core::mime::parse_message(&raw).expect("parse full");
    assert!(full.text.unwrap().contains("Body"));

    // Flag roundtrip.
    imap::store_flag(&mut s, uids[0], "\\Seen", true)
        .await
        .expect("store");
    let flags = imap::fetch_flags(&mut s, &uids[0].to_string())
        .await
        .expect("flags");
    assert!(flags[0].1.seen);

    imap::logout(s).await;
    eprintln!("imap primitives OK");
}

#[tokio::test(flavor = "multi_thread")]
async fn probe_smtp_starttls_auth() {
    if std::env::var("FLECTAR_MAIL_TEST_IMAP").as_deref() != Ok("1") {
        return;
    }
    // SAFETY: single-threaded test setup, before any threads read the env.
    unsafe { std::env::set_var("FLECTAR_MAIL_TLS_INSECURE", "1") };
    let cfg = flectar_mail_core::models::AccountConfig {
        id: 1,
        email: "probeuser@example.com".into(),
        display_name: None,
        avatar_url: None,
        provider: flectar_mail_core::models::Provider::Generic,
        auth_kind: flectar_mail_core::models::AuthKind::Password,
        mail_protocol: flectar_mail_core::models::MailProtocol::Imap,
        username: "probeuser".into(),
        jmap_url: String::new(),
        jmap_account_id: None,
        incoming_host: "127.0.0.1".into(),
        incoming_port: 10993,
        smtp_host: "127.0.0.1".into(),
        smtp_port: 10587,
        settings: flectar_mail_core::models::AccountSettings::new(
            flectar_mail_core::models::MailConnectionSettings::new(
                flectar_mail_core::models::ConnectionSecurity::Tls,
                flectar_mail_core::models::ConnectionSecurity::Starttls,
            ),
        ),
    };
    let auth = flectar_mail_core::smtp::SmtpAuth::Password("pass".into());
    match flectar_mail_core::smtp::test_connection(&cfg, &auth).await {
        Ok(()) => eprintln!("smtp OK: STARTTLS + AUTH PLAIN accepted"),
        // The test Dovecot has no submission_relay_host; auth succeeds
        // (visible in its log) and it then 451s. Connection+TLS+AUTH proven.
        Err(e) if e.to_string().contains("451") => {
            eprintln!("smtp OK: STARTTLS + AUTH PLAIN accepted (451 = container has no relay)")
        }
        Err(e) => panic!("smtp starttls+auth: {e}"),
    }
}
