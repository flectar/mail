//! End-to-end test against a local Dovecot (see README / CI setup):
//!   docker run -d --name flectar-mail-dovecot -e USER_PASSWORD='{plain}pass' \
//!     -p 10993:31993 -p 10587:31587 dovecot/dovecot:latest
//! seeded with 6 messages (2 in one thread, 2 automated senders).
//!
//! Gated: runs only when FLECTAR_MAIL_TEST_IMAP=1.

use flectar_mail_core::Core;
use flectar_mail_core::config::Paths;
use flectar_mail_core::models::*;
use std::time::{Duration, Instant};

fn gated() -> bool {
    std::env::var("FLECTAR_MAIL_TEST_IMAP")
        .map(|v| v == "1")
        .unwrap_or(false)
}

/// Reset the test mailbox to a known state: wipe INBOX/Archive, append the
/// six fixture messages. Self-contained so reruns are idempotent.
async fn seed_mailbox() {
    use flectar_mail_core::imap;
    let creds = imap::ImapCredentials::Password {
        user: "testuser".into(),
        password: "pass".into(),
    };
    let mut s = imap::connect("127.0.0.1", 10993, creds)
        .await
        .expect("seed connect");

    let now = chrono::Utc::now();
    let msg = |i: usize,
               from: &str,
               subj: &str,
               body: &str,
               irt: Option<&str>,
               days_ago: i64,
               extra: &str| {
        let date = (now - chrono::Duration::days(days_ago)).to_rfc2822();
        let refs = irt
            .map(|r| format!("In-Reply-To: {r}\r\nReferences: {r}\r\n"))
            .unwrap_or_default();
        format!(
            "Message-ID: <m{i}@example.com>\r\nFrom: {from}\r\nTo: Test User <testuser@example.com>\r\n\
             Subject: {subj}\r\nDate: {date}\r\n{refs}{extra}\
             Content-Type: text/plain; charset=utf-8\r\n\r\n{body}\r\n"
        )
    };
    let fixtures = [
        msg(
            1,
            "Alice Chen <alice@example.com>",
            "Quarterly report draft",
            "Hi,\r\n\r\nAttached the quarterly numbers draft. Can you review the revenue section before Friday?\r\n\r\nAlice",
            None,
            0,
            "",
        ),
        msg(
            2,
            "Alice Chen <alice@example.com>",
            "Re: Quarterly report draft",
            "Ping - did you get a chance to look?\r\n\r\nAlice",
            Some("<m1@example.com>"),
            0,
            "",
        ),
        msg(
            3,
            "Bob Kowalski <bob@widgets.io>",
            "Lunch tomorrow?",
            "Thinking pho at 12:30. You in?\r\n\r\nBob",
            None,
            1,
            "",
        ),
        msg(
            4,
            "GitHub <notifications@github.com>",
            "[flectar-mail] CI failed on master",
            "Run #4512 failed on master.\r\n",
            None,
            3,
            "List-Id: <flectar-mail.github.com>\r\n",
        ),
        msg(
            5,
            "Stripe <billing@stripe.com>",
            "Your invoice is ready",
            "Invoice INV-2201 for $49.00 is available.\r\n",
            None,
            4,
            "List-Unsubscribe: <https://stripe.com/unsub>\r\n",
        ),
        msg(
            6,
            "Dana <dana@example.com>",
            "Flight options for the offsite",
            "Found three options under $400, sending the comparison later today.\r\n\r\nDana",
            None,
            5,
            "",
        ),
        // Multipart message with an attachment (base64 "MOCKUP NOTES - review by Friday").
        format!(
            "Message-ID: <m7@example.com>\r\nFrom: Eve Park <eve@example.com>\r\n\
             To: Test User <testuser@example.com>\r\nSubject: Design mockups\r\nDate: {}\r\n\
             MIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"b1\"\r\n\r\n\
             --b1\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n\
             Mockups attached, notes inside.\r\n\
             --b1\r\nContent-Type: text/plain; name=\"notes.txt\"\r\n\
             Content-Disposition: attachment; filename=\"notes.txt\"\r\n\
             Content-Transfer-Encoding: base64\r\n\r\n\
             TU9DS1VQIE5PVEVTIC0gcmV2aWV3IGJ5IEZyaWRheQ==\r\n\
             --b1--\r\n",
            (now - chrono::Duration::days(1)).to_rfc2822()
        ),
        // Meeting invite with a text/calendar part.
        format!(
            "Message-ID: <m8@example.com>\r\nFrom: Frank Ops <frank@example.com>\r\n\
             To: Test User <testuser@example.com>\r\nSubject: Invitation: Sync on Q3 plan\r\nDate: {}\r\n\
             MIME-Version: 1.0\r\nContent-Type: multipart/alternative; boundary=\"b2\"\r\n\r\n\
             --b2\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n\
             You are invited to Sync on Q3 plan.\r\n\
             --b2\r\nContent-Type: text/calendar; method=REQUEST; charset=utf-8\r\n\r\n\
             BEGIN:VCALENDAR\r\nVERSION:2.0\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\n\
             UID:q3sync-1@cal.example.com\r\nSUMMARY:Sync on Q3 plan\r\nLOCATION:Room 7\r\n\
             ORGANIZER;CN=Frank:mailto:frank@example.com\r\n\
             DTSTART:20260720T090000Z\r\nDTEND:20260720T093000Z\r\nSTATUS:CONFIRMED\r\n\
             END:VEVENT\r\nEND:VCALENDAR\r\n\
             --b2--\r\n",
            (now - chrono::Duration::hours(6)).to_rfc2822()
        ),
    ];

    for folder in ["Sent", "Drafts", "Trash", "Junk", "Archive"] {
        let _ = imap::create_folder(&mut s, folder).await; // exists on reruns
    }
    for folder in ["INBOX", "Archive"] {
        if imap::select(&mut s, folder).await.is_ok() {
            let all = imap::uid_search_all(&mut s).await.unwrap_or_default();
            for uid in all {
                let _ = imap::store_flag(&mut s, uid, "\\Deleted", true).await;
            }
            let _ = imap::expunge_all(&mut s).await;
        }
    }

    imap::select(&mut s, "INBOX").await.expect("select inbox");
    for raw in &fixtures {
        imap::append(&mut s, "INBOX", raw.as_bytes(), false)
            .await
            .expect("seed append");
    }
    imap::logout(s).await;
}

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
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn full_sync_triage_and_search() {
    if !gated() {
        eprintln!("skipping (set FLECTAR_MAIL_TEST_IMAP=1 to run)");
        return;
    }
    // SAFETY: single-threaded test setup, before any threads read the env.
    unsafe { std::env::set_var("FLECTAR_MAIL_TLS_INSECURE", "1") };
    let creds_file = std::env::temp_dir().join(format!(
        "flectar-mail-test-creds-{}.json",
        std::process::id()
    ));
    unsafe { std::env::set_var("FLECTAR_MAIL_CREDENTIALS_INSECURE_FILE", &creds_file) };
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "flectar_mail_core=debug".into()),
        )
        .try_init();
    seed_mailbox().await;

    let tmp = tempfile::tempdir().unwrap();
    let core = Core::start(Paths::for_tests(tmp.path())).await.unwrap();

    // 1. Add the account (verifies IMAP login on the way in).
    let account = core
        .add_account_password(AddPasswordAccountArgs {
            connection: Default::default(),
            email: "testuser@example.com".into(),
            display_name: Some("Test User".into()),
            username: "testuser".into(),
            password: "pass".into(),
            mail_protocol: flectar_mail_core::models::MailProtocol::Imap,
            jmap_url: String::new(),
            imap_host: "127.0.0.1".into(),
            imap_port: 10993,
            smtp_host: "127.0.0.1".into(),
            smtp_port: 10587,
        })
        .await
        .expect("add account");
    assert_eq!(account.email, "testuser@example.com");

    // 2. Backfill: 7 threads appear in the inbox (8 messages, one reply chain).
    let page = wait_for("7 inbox threads", Duration::from_secs(90), || async {
        let p = core
            .list_threads(View::Inbox, None, None, None, None, (None, 50))
            .await
            .unwrap();
        eprintln!(
            "poll: {} inbox threads: {:?}",
            p.threads.len(),
            p.threads.iter().map(|t| &t.subject).collect::<Vec<_>>()
        );
        (p.threads.len() == 7).then_some(p)
    })
    .await;

    // 3. Threading: the Alice reply chain is one thread with 2 messages.
    let alice = page
        .threads
        .iter()
        .find(|t| t.subject.contains("Quarterly report"))
        .expect("quarterly thread");
    assert_eq!(alice.message_count, 2, "References threading failed");

    // 4. Split inbox: Important (-1) = 3 human threads, Other (-2) = 2 automated.
    let important = core
        .list_threads(View::Inbox, Some(-1), None, None, None, (None, 50))
        .await
        .unwrap();
    let other = core
        .list_threads(View::Inbox, Some(-2), None, None, None, (None, 50))
        .await
        .unwrap();
    assert_eq!(important.threads.len(), 5, "Important split");
    assert_eq!(
        other.threads.len(),
        2,
        "Other split (List-Id/List-Unsubscribe)"
    );

    // 5. Bodies arrive and are searchable via FTS.
    let alice_id = alice.id;
    wait_for("alice body cached", Duration::from_secs(60), || async {
        let d = core.get_thread(alice_id).await.unwrap();
        d.messages
            .iter()
            .any(|m| {
                m.body_state == "cached"
                    && m.text_body
                        .as_deref()
                        .unwrap_or("")
                        .contains("quarterly numbers")
            })
            .then_some(())
    })
    .await;

    let hits = core.search("quarterly".into(), None, 10).await.unwrap();
    assert!(
        hits.iter().any(|t| t.id == alice_id),
        "FTS search should find the quarterly thread"
    );
    let hits = core.search("from:stripe".into(), None, 10).await.unwrap();
    assert_eq!(hits.len(), 1, "from: operator");

    // 6. Archive Bob's thread: optimistic local move + remote replay.
    let bob = page
        .threads
        .iter()
        .find(|t| t.subject.contains("Lunch"))
        .expect("bob thread");
    core.perform_action(PerformActionArgs {
        kind: ActionKind::Archive,
        thread_ids: vec![bob.id],
        params: None,
    })
    .await
    .unwrap();

    // Optimistic: gone from inbox immediately.
    let inbox = core
        .list_threads(View::Inbox, None, None, None, None, (None, 50))
        .await
        .unwrap();
    assert_eq!(inbox.threads.len(), 6, "optimistic archive");
    // And present in Done.
    let done = core
        .list_threads(View::Done, None, None, None, None, (None, 50))
        .await
        .unwrap();
    assert!(done.threads.iter().any(|t| t.id == bob.id));

    // 7. Undo brings it back locally.
    assert!(core.undo_last().await.unwrap());
    let inbox = core
        .list_threads(View::Inbox, None, None, None, None, (None, 50))
        .await
        .unwrap();
    assert_eq!(inbox.threads.len(), 7, "undo restored inbox");

    // 8. Snooze hides a thread from inbox; unsnooze restores it.
    core.perform_action(PerformActionArgs {
        kind: ActionKind::Snooze,
        thread_ids: vec![alice_id],
        params: Some(ActionParams {
            wake_at: Some(now_ms() + 3_600_000),
            target_folder_id: None,
            label_id: None,
        }),
    })
    .await
    .unwrap();
    let inbox = core
        .list_threads(View::Inbox, None, None, None, None, (None, 50))
        .await
        .unwrap();
    assert_eq!(inbox.threads.len(), 6, "snoozed thread hidden");
    let snoozed = core
        .list_threads(View::Snoozed, None, None, None, None, (None, 50))
        .await
        .unwrap();
    assert!(snoozed.threads.iter().any(|t| t.id == alice_id));
    core.perform_action(PerformActionArgs {
        kind: ActionKind::Unsnooze,
        thread_ids: vec![alice_id],
        params: None,
    })
    .await
    .unwrap();

    // 9. Contacts were harvested for autocomplete.
    let contacts = core.list_contacts("ali".into(), None, 5).await.unwrap();
    assert!(contacts.iter().any(|c| c.email == "alice@example.com"));

    // 9b. Attachment: parsed during body sync and extractable to disk.
    let eve = page
        .threads
        .iter()
        .find(|t| t.subject.contains("Design mockups"))
        .expect("eve thread");
    let eve_id = eve.id;
    let attachment_id = wait_for("attachment parsed", Duration::from_secs(60), || async {
        let d = core.get_thread(eve_id).await.unwrap();
        d.messages
            .iter()
            .flat_map(|m| m.attachments.iter())
            .find(|a| a.filename.as_deref() == Some("notes.txt"))
            .map(|a| a.id)
    })
    .await;
    let path = core
        .get_attachment(attachment_id)
        .await
        .expect("extract attachment");
    let content = std::fs::read_to_string(&path).expect("read extracted file");
    assert_eq!(
        content, "MOCKUP NOTES - review by Friday",
        "attachment content"
    );
    let eve_hits = core
        .search("has:attachment".into(), None, 10)
        .await
        .unwrap();
    assert!(
        eve_hits.iter().any(|t| t.id == eve_id),
        "has:attachment finds it"
    );

    // 9c. Calendar: the invite's VEVENT lands in calendar_events.
    let ev = wait_for("calendar event parsed", Duration::from_secs(60), || async {
        let events = core.list_events(0, i64::MAX / 2).await.unwrap();
        events
            .into_iter()
            .find(|e| e.summary.as_deref() == Some("Sync on Q3 plan"))
    })
    .await;
    assert_eq!(ev.location.as_deref(), Some("Room 7"));
    assert_eq!(ev.organizer.as_deref(), Some("frank@example.com"));
    assert_eq!(
        ev.ends_at.unwrap() - ev.starts_at,
        30 * 60 * 1000,
        "30-minute event"
    );
    assert_eq!(ev.method.as_deref(), Some("REQUEST"));

    // 10. Archive again and verify the REMOTE side converges: message leaves
    // the server's INBOX (action queue replay), within a couple sync cycles.
    let bob_id = bob.id;
    core.perform_action(PerformActionArgs {
        kind: ActionKind::Archive,
        thread_ids: vec![bob_id],
        params: None,
    })
    .await
    .unwrap();
    wait_for("remote archive replay", Duration::from_secs(90), || async {
        let status = core.get_sync_status().await.unwrap();
        let _ = status;
        // Server truth via a raw IMAP roundtrip through our own client module.
        let creds = flectar_mail_core::imap::ImapCredentials::Password {
            user: "testuser".into(),
            password: "pass".into(),
        };
        let mut s = flectar_mail_core::imap::connect("127.0.0.1", 10993, creds)
            .await
            .ok()?;
        let sel = flectar_mail_core::imap::select(&mut s, "INBOX")
            .await
            .ok()?;
        let n = sel.exists;
        flectar_mail_core::imap::logout(s).await;
        (n == 7).then_some(())
    })
    .await;

    // 11. IMAP IDLE push: with the actor waiting in IDLE, a message appended to
    // INBOX should surface in seconds - far faster than the 60s poll cycle.
    // Wait until the actor is quiescent (state "idle" => in the IDLE wait), then
    // append; the server's push must wake it.
    wait_for("account idle", Duration::from_secs(90), || async {
        let st = core.get_sync_status().await.unwrap();
        st.iter()
            .find(|s| s.account_id == account.id)
            .filter(|s| s.state == "idle")
            .map(|_| ())
    })
    .await;
    // Small buffer so the actor has entered idle_wait (SELECT + IDLE init).
    tokio::time::sleep(Duration::from_secs(2)).await;

    let mut ev_rx = core.bus.subscribe();
    {
        let creds = flectar_mail_core::imap::ImapCredentials::Password {
            user: "testuser".into(),
            password: "pass".into(),
        };
        let mut s = flectar_mail_core::imap::connect("127.0.0.1", 10993, creds)
            .await
            .expect("idle-push connect");
        let raw = format!(
            "Message-ID: <idle-push@example.com>\r\nFrom: Grace <grace@example.com>\r\n\
             To: Test User <testuser@example.com>\r\nSubject: Pushed via IDLE\r\nDate: {}\r\n\
             Content-Type: text/plain; charset=utf-8\r\n\r\nArrived in real time.\r\n",
            chrono::Utc::now().to_rfc2822()
        );
        flectar_mail_core::imap::append(&mut s, "INBOX", raw.as_bytes(), false)
            .await
            .expect("idle-push append");
        flectar_mail_core::imap::logout(s).await;
    }

    // 15s << the 60s poll cadence, so a MailNew arriving in time proves the
    // IDLE push path (not a poll) woke the actor.
    let pushed = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            match ev_rx.recv().await {
                Ok(flectar_mail_core::events::CoreEvent::MailNew { .. }) => break true,
                Ok(_) => continue,
                Err(_) => break false,
            }
        }
    })
    .await
    .unwrap_or(false);
    assert!(
        pushed,
        "IDLE should deliver new mail within 15s (poll cadence is 60s)"
    );

    let inbox = core
        .list_threads(View::Inbox, None, None, None, None, (None, 50))
        .await
        .unwrap();
    assert!(
        inbox.threads.iter().any(|t| t.subject == "Pushed via IDLE"),
        "IDLE-pushed message should appear in the inbox"
    );

    eprintln!(
        "e2e OK: sync, threading, splits, bodies, FTS, archive+undo, snooze, contacts, remote replay, IDLE push"
    );
}
