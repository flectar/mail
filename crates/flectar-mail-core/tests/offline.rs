//! Real-offline behavior: with the account's servers unreachable, the app
//! must (1) report the account offline, (2) apply actions locally and keep
//! them queued as pending (never failed), (3) accept a send into the durable
//! queue, and (4) preserve all pending actions across a restart.
//!
//! No live server needed - an immediately-refused localhost port stands in
//! for "no network". The replay-on-reconnect half is covered by the gated
//! send_e2e/dovecot_e2e tests.

use flectar_mail_core::Core;
use flectar_mail_core::accounts::credentials::{self, Slot, SystemCredentialStore};
use flectar_mail_core::config::Paths;
use flectar_mail_core::db::{Db, repo};
use flectar_mail_core::models::*;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

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
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// A localhost port with nothing listening: connections are refused instantly.
fn dead_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

/// Seed an account (pointing at unreachable servers), its folders, and one
/// unread inbox message directly in the database - the normal add-account
/// flow probes IMAP first, which can't succeed offline.
async fn seed(db: &Db, port: u16) -> (i64, i64) {
    db.write(move |conn| {
        let account_id = repo::accounts::insert(
            conn,
            &repo::accounts::NewAccount {
                email: "offline@example.com",
                display_name: Some("Offline User"),
                avatar_url: None,
                provider: Provider::Imap,
                auth_kind: AuthKind::Password,
                mail_protocol: flectar_mail_core::models::MailProtocol::Imap,
                username: "offline",
                jmap_url: "",
                jmap_account_id: None,
                imap_host: "127.0.0.1",
                imap_port: port,
                smtp_host: "127.0.0.1",
                smtp_port: port,
            },
        )?;
        let inbox = repo::folders::upsert(conn, account_id, "INBOX", None, Some(roles::INBOX))?;
        repo::folders::upsert(conn, account_id, "Archive", None, Some(roles::ARCHIVE))?;
        repo::folders::upsert(conn, account_id, "Sent", None, Some(roles::SENT))?;
        repo::folders::upsert(conn, account_id, "Drafts", None, Some(roles::DRAFTS))?;
        repo::folders::upsert(conn, account_id, "Trash", None, Some(roles::TRASH))?;

        let thread_id = repo::threads::create(conn, account_id, None, "quarterly report")?;
        let msg = repo::messages::NewMessage {
            account_id,
            folder_id: inbox,
            uid: Some(7),
            message_id: Some("m1@example.com".into()),
            gm_msgid: None,
            gm_thrid: None,
            subject: "Quarterly report".into(),
            from: Some(Address {
                name: Some("Alice Chen".into()),
                email: "alice@example.com".into(),
            }),
            to: vec![Address {
                name: None,
                email: "offline@example.com".into(),
            }],
            cc: vec![],
            bcc: vec![],
            date: now_ms(),
            internal_date: None,
            is_read: false,
            is_starred: false,
            is_draft: false,
            is_outgoing: false,
            is_automated: false,
            has_attachments: false,
            size: None,
            snippet: "please review".into(),
            references: vec![],
            list_unsubscribe: None,
            list_unsubscribe_post: None,
            sender_addr: None,
            sender_verification: Default::default(),
        };
        repo::messages::insert(conn, &msg, thread_id)?;
        repo::threads::recompute(conn, thread_id)?;
        Ok((account_id, thread_id))
    })
    .await
    .expect("seed")
}

/// (kind, state, attempts) for every queued action, oldest first.
async fn actions(db: &Db) -> Vec<(String, String, i64)> {
    db.read(|conn| {
        let mut stmt =
            conn.prepare("SELECT kind, state, attempts FROM pending_actions ORDER BY id")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    })
    .await
    .expect("query actions")
}

#[tokio::test(flavor = "multi_thread")]
async fn offline_actions_queue_locally_and_survive_restart() {
    let creds_file = std::env::temp_dir().join(format!(
        "flectar-mail-offline-creds-{}.json",
        std::process::id()
    ));
    // SAFETY: single-threaded test setup, before any threads read the env.
    unsafe { std::env::set_var("FLECTAR_MAIL_CREDENTIALS_INSECURE_FILE", &creds_file) };

    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::for_tests(tmp.path());
    paths.ensure().unwrap();
    let port = dead_port();

    let db = Db::open(&paths.db_file()).unwrap();
    let (account_id, thread_id) = seed(&db, port).await;
    credentials::store_async(
        Arc::new(SystemCredentialStore),
        account_id,
        Slot::Password,
        "pass".into(),
    )
    .await
    .unwrap();

    let core = Core::start(Paths::for_tests(tmp.path())).await.unwrap();
    core.notify_ui_ready();

    // The sync actor detects the dead server and marks the account offline.
    wait_for("offline sync state", Duration::from_secs(15), || async {
        let accs = core.list_accounts().await.unwrap();
        (accs[0].sync_state == "offline").then_some(())
    })
    .await;

    // Triage while offline: local mutation is immediate, intent is queued.
    core.perform_action(PerformActionArgs {
        kind: ActionKind::MarkRead,
        thread_ids: vec![thread_id],
        params: None,
    })
    .await
    .expect("mark read offline");
    core.perform_action(PerformActionArgs {
        kind: ActionKind::Archive,
        thread_ids: vec![thread_id],
        params: None,
    })
    .await
    .expect("archive offline");

    let inbox = core
        .list_threads(View::Inbox, None, None, None, None, (None, 10))
        .await
        .unwrap();
    assert!(inbox.threads.is_empty(), "archive applied locally");
    let done = core
        .list_threads(View::Done, None, None, None, None, (None, 10))
        .await
        .unwrap();
    assert_eq!(done.threads.len(), 1, "thread visible in Done");
    assert_eq!(done.threads[0].unread_count, 0, "mark_read applied locally");

    // Compose and send while offline: accepted into the durable queue.
    let draft_id = core
        .save_draft(SaveDraftArgs {
            draft_id: None,
            account_id,
            from: None,
            to: vec![Address {
                name: Some("Alice Chen".into()),
                email: "alice@example.com".into(),
            }],
            cc: vec![],
            bcc: vec![],
            subject: "Written on a plane".into(),
            body_text: "Sending this from 30,000 feet.".into(),
            body_html: None,
            mode: "new".into(),
            in_reply_to_message_id: None,
            attachments: vec![],
        })
        .await
        .expect("save draft offline");
    core.queue_send(QueueSendArgs {
        draft_id,
        send_at: Some(now_ms()),
    })
    .await
    .expect("queue send offline");

    // Give the scheduler time to make the send due and the actor several
    // failed reconnect attempts: nothing may be marked failed or retried
    // against the server while offline.
    tokio::time::sleep(Duration::from_secs(6)).await;
    let rows = actions(&db).await;
    let kinds: Vec<&str> = rows.iter().map(|(k, _, _)| k.as_str()).collect();
    assert_eq!(kinds, vec!["mark_read", "archive", "send"]);
    for (kind, state, attempts) in &rows {
        assert_eq!(state, "pending", "{kind} must stay pending while offline");
        assert_eq!(*attempts, 0, "{kind} must not burn attempts while offline");
    }

    // Restart: the queue and the optimistic local state are durable.
    drop(core);
    let core = Core::start(Paths::for_tests(tmp.path())).await.unwrap();
    core.notify_ui_ready();
    let rows = actions(&db).await;
    assert_eq!(rows.len(), 3, "queue survives restart");
    assert!(
        rows.iter().all(|(_, state, _)| state == "pending"),
        "all actions still pending after restart"
    );
    let inbox = core
        .list_threads(View::Inbox, None, None, None, None, (None, 10))
        .await
        .unwrap();
    assert!(inbox.threads.is_empty(), "archived state survives restart");

    let _ = std::fs::remove_file(&creds_file);
}
