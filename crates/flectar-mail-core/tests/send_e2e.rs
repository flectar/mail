//! End-to-end send test: reply draft -> queue_send -> executor -> SMTP sink.
//! Needs the Dovecot container (IMAP, user "senduser") plus the STARTTLS
//! sink from scratchpad/smtp_sink.py on port 10588.
//! Gated: FLECTAR_MAIL_TEST_IMAP=1 and FLECTAR_MAIL_TEST_SINK_DIR=<sink output dir>.

use flectar_mail_core::Core;
use flectar_mail_core::config::Paths;
use flectar_mail_core::imap;
use flectar_mail_core::models::*;
use std::time::{Duration, Instant};

async fn wait_for<T, F, Fut>(what: &str, timeout: Duration, mut f: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let start = Instant::now();
    loop {
        if let Some(v) = f().await {
            return v;
        }
        assert!(
            start.elapsed() < timeout,
            "timed out after {timeout:?} waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
}

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
    let date = chrono::Utc::now().to_rfc2822();
    let raw = format!(
        "Message-ID: <m1@example.com>\r\nFrom: Alice Chen <alice@example.com>\r\n\
         To: Send User <senduser@example.com>\r\nSubject: Quarterly report draft\r\n\
         Date: {date}\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n\
         Can you review the revenue section before Friday?\r\n"
    );
    imap::select(&mut s, "INBOX").await.expect("select");
    imap::append(&mut s, "INBOX", raw.as_bytes(), false)
        .await
        .expect("append");
    imap::logout(s).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn reply_send_reaches_smtp_and_sent_folder() {
    if std::env::var("FLECTAR_MAIL_TEST_IMAP").as_deref() != Ok("1") {
        eprintln!("skipping (set FLECTAR_MAIL_TEST_IMAP=1)");
        return;
    }
    let Ok(sink_dir) = std::env::var("FLECTAR_MAIL_TEST_SINK_DIR") else {
        eprintln!("skipping (set FLECTAR_MAIL_TEST_SINK_DIR)");
        return;
    };
    // SAFETY: single-threaded test setup, before any threads read the env.
    unsafe { std::env::set_var("FLECTAR_MAIL_TLS_INSECURE", "1") };
    let creds_file = std::env::temp_dir().join(format!(
        "flectar-mail-send-creds-{}.json",
        std::process::id()
    ));
    unsafe { std::env::set_var("FLECTAR_MAIL_CREDENTIALS_INSECURE_FILE", &creds_file) };
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "flectar_mail_core=debug".into()),
        )
        .try_init();

    seed().await;

    let tmp = tempfile::tempdir().unwrap();
    let core = Core::start(Paths::for_tests(tmp.path())).await.unwrap();
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
        smtp_port: 10588, // the sink
    })
    .await
    .expect("add account");

    // Wait for Alice's message to sync in.
    let thread = wait_for("alice thread", Duration::from_secs(60), || async {
        let p = core
            .list_threads(View::Inbox, None, None, None, None, (None, 10))
            .await
            .unwrap();
        p.threads.into_iter().next()
    })
    .await;
    let detail = core.get_thread(thread.id).await.unwrap();
    let parent = &detail.messages[0];

    // Compose a reply and send it "now" (bypass the undo window).
    let draft_id = core
        .save_draft(SaveDraftArgs {
            draft_id: None,
            account_id: parent.account_id,
            to: vec![Address {
                name: Some("Alice Chen".into()),
                email: "alice@example.com".into(),
            }],
            cc: vec![],
            bcc: vec![],
            subject: "Re: Quarterly report draft".into(),
            body_text: "Reviewed - the revenue section looks solid. Ship it.".into(),
            body_html: Some("Reviewed - the revenue section looks <b>solid</b>. Ship it.".into()),
            mode: "reply".into(),
            in_reply_to_message_id: Some(parent.id),
            attachments: vec![],
        })
        .await
        .expect("save draft");
    let result = core
        .queue_send(QueueSendArgs {
            draft_id,
            send_at: Some(now_ms()),
        })
        .await
        .expect("queue send");
    eprintln!(
        "queued action {} dispatch at {}",
        result.action_id, result.dispatch_at
    );

    // The sink receives the message (scheduler nudges the actor within ~5s).
    // Scan for the newest .eml - the sink numbers files across its lifetime.
    let find_delivery = |dir: String| async move {
        let mut newest: Option<(std::time::SystemTime, std::path::PathBuf)> = None;
        for entry in std::fs::read_dir(&dir).ok()?.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("eml") {
                let mtime = entry.metadata().ok()?.modified().ok()?;
                if newest.as_ref().is_none_or(|(t, _)| mtime > *t) {
                    newest = Some((mtime, path));
                }
            }
        }
        let (_, path) = newest?;
        let eml = std::fs::read_to_string(&path).ok()?;
        let envelope = std::fs::read_to_string(path.with_extension("envelope")).ok()?;
        eml.contains("revenue section looks solid")
            .then_some((eml, envelope))
    };
    let (eml, envelope) = wait_for("sink delivery", Duration::from_secs(60), || {
        find_delivery(sink_dir.clone())
    })
    .await;

    assert!(
        eml.contains("To: ") && eml.contains("alice@example.com"),
        "To header"
    );
    assert!(
        eml.contains("Subject: Re: Quarterly report draft"),
        "Subject"
    );
    assert!(
        eml.contains("In-Reply-To: <m1@example.com>"),
        "In-Reply-To:\n{eml}"
    );
    assert!(eml.contains("References: <m1@example.com>"), "References");
    assert!(eml.contains("revenue section looks solid"), "body");
    assert!(envelope.contains("alice@example.com"), "envelope rcpt");

    // Local state: the draft became a sent message in the same thread.
    let detail = wait_for("local sent state", Duration::from_secs(30), || async {
        let d = core.get_thread(thread.id).await.unwrap();
        d.messages
            .iter()
            .any(|m| m.is_outgoing && !m.is_draft && m.subject.starts_with("Re:"))
            .then_some(d)
    })
    .await;
    assert_eq!(detail.messages.len(), 2, "reply joined the thread");

    // Server state: APPEND put a copy in the Sent folder.
    wait_for("sent folder copy", Duration::from_secs(30), || async {
        let creds = imap::ImapCredentials::Password {
            user: "senduser".into(),
            password: "pass".into(),
        };
        let mut s = imap::connect("127.0.0.1", 10993, creds).await.ok()?;
        let sel = imap::select(&mut s, "Sent").await.ok()?;
        let n = sel.exists;
        imap::logout(s).await;
        (n == 1).then_some(())
    })
    .await;

    eprintln!("send e2e OK: SMTP delivery, References headers, Sent APPEND, local thread state");
}
