use super::*;
use crate::accounts::credentials::{CredentialStoreHandle, DevelopmentFileCredentialStore};
use crate::config::Paths;
use crate::db::Db;
use crate::events::EventBus;
use crate::models::MailHistory;
use crate::oauth::tokens::TokenProvider;
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

fn email(id: &str, thread: &str) -> Value {
    json!({
        "id": id, "blobId": format!("blob-{id}"), "threadId": thread,
        "mailboxIds": {"inbox-remote": true}, "keywords": {},
        "size": 100, "receivedAt": "2026-10-08T12:00:00Z",
        "messageId": [format!("{id}@mail.example")],
        "from": [{"name": "Sender", "email": "sender@mail.example"}],
        "to": [{"name": "Reader", "email": "reader@mail.example"}],
        "subject": "Conversation", "hasAttachment": false,
        "attachments": [], "preview": "Example message"
    })
}

async fn fixture() -> (tempfile::TempDir, SyncCtx, AccountConfig) {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("mail.db")).unwrap();
    let credentials: CredentialStoreHandle = Arc::new(DevelopmentFileCredentialStore::new(
        dir.path().join("credentials.json"),
    ));
    let tokens = TokenProvider::new(
        credentials.clone(),
        Arc::new(crate::oauth::redirect::LoopbackRedirectBroker::default()),
    );
    let ctx = SyncCtx {
        db,
        calendar_db: Db::deferred_calendar(dir.path().join("calendar.db")),
        bus: EventBus::new(),
        paths: Arc::new(Paths::for_tests(dir.path())),
        tokens,
        credentials,
    };
    let mut config = ctx
        .db
        .write(|conn| {
            crate::db::testutil::seed_account(conn);
            conn.execute("UPDATE accounts SET mail_protocol='jmap' WHERE id=1", [])?;
            conn.execute("UPDATE folders SET jmap_id='inbox-remote' WHERE id=1", [])?;
            Ok(repo::accounts::get_config(conn, 1)?.unwrap())
        })
        .await
        .unwrap();
    config.settings.mail_history = MailHistory::All;
    (dir, ctx, config)
}

async fn seed_snoozed_thread(ctx: &SyncCtx, wake_at: i64) -> i64 {
    ctx.db
        .write(move |conn| {
            let (thread, message) = crate::db::testutil::seed_message(
                conn,
                "sender@mail.example",
                "Conversation",
                false,
            );
            conn.execute(
                "UPDATE threads SET jmap_id='remote-conversation' WHERE id=?1",
                [thread],
            )?;
            repo::messages::set_jmap_remote(conn, message, "old-message", Some("blob-old"))?;
            repo::snoozes::set(conn, thread, 1, wake_at, Some(1))?;
            // Keep a higher local id so deleting/recreating the conversation
            // cannot accidentally reuse its original id in the test.
            repo::threads::create_jmap(conn, 1, "other-conversation", "other")?;
            conn.execute(
                "INSERT INTO jmap_sync_state(account_id,email_state) VALUES(1,'state-0')",
                [],
            )?;
            Ok(thread)
        })
        .await
        .unwrap()
}

async fn snoozed_thread(ctx: &SyncCtx) -> (Option<i64>, Option<i64>) {
    ctx.db
        .read(|conn| {
            let thread = repo::threads::by_jmap_id(conn, 1, "remote-conversation")?;
            let wake = match thread {
                Some(thread) => conn
                    .query_row(
                        "SELECT wake_at FROM snoozes WHERE thread_id=?1",
                        [thread],
                        |r| r.get(0),
                    )
                    .optional()?,
                None => None,
            };
            Ok((thread, wake))
        })
        .await
        .unwrap()
}

/// One Email/get object per page, with an optional failure on a later page.
async fn server(
    emails: Vec<Value>,
    changes: Option<Value>,
    fail_on: Option<(&'static str, Arc<AtomicBool>)>,
) -> (ConnectedClient, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let core = json!({
        "maxSizeUpload": 50_000_000, "maxConcurrentUpload": 4,
        "maxSizeRequest": 10_000_000, "maxConcurrentRequests": 4,
        "maxCallsInRequest": 16, "maxObjectsInGet": 1,
        "maxObjectsInSet": 500, "collationAlgorithms": []
    });
    let mail = json!({
        "maxMailboxesPerEmail": null, "maxMailboxDepth": 10,
        "maxSizeMailboxName": 255, "maxSizeAttachmentsPerEmail": 50_000_000,
        "emailQuerySortOptions": ["receivedAt"], "mayCreateTopLevelMailbox": true
    });
    let submission = json!({"maxDelayedSend": 0, "submissionExtensions": []});
    let session = json!({
        "capabilities": {
            "urn:ietf:params:jmap:core": core,
            "urn:ietf:params:jmap:mail": mail,
            "urn:ietf:params:jmap:submission": submission
        },
        "accounts": {"mail-account": {
            "name": "Test", "isPersonal": true, "isReadOnly": false,
            "accountCapabilities": {
                "urn:ietf:params:jmap:mail": mail,
                "urn:ietf:params:jmap:submission": submission
            }
        }},
        "primaryAccounts": {"urn:ietf:params:jmap:mail": "mail-account"},
        "username": "reader@mail.example", "apiUrl": format!("{origin}/api"),
        "downloadUrl": format!("{origin}/download/{{accountId}}/{{blobId}}/{{name}}?accept={{type}}"),
        "uploadUrl": format!("{origin}/upload/{{accountId}}"),
        "eventSourceUrl": format!("{origin}/events?types={{types}}&closeafter={{closeafter}}&ping={{ping}}"),
        "state": "session-0"
    });
    let task = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            let mut line = String::new();
            assert_ne!(stream.read_line(&mut line).await.unwrap(), 0);
            let get = line.starts_with("GET ");
            let mut length = 0;
            loop {
                line.clear();
                assert_ne!(stream.read_line(&mut line).await.unwrap(), 0);
                if line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.split_once(':')
                    && name.eq_ignore_ascii_case("content-length")
                {
                    length = value.trim().parse::<usize>().unwrap();
                }
            }
            let mut body = vec![0; length];
            stream.read_exact(&mut body).await.unwrap();
            let response = if get {
                session.clone()
            } else {
                let request: Value = serde_json::from_slice(&body).unwrap();
                let mut responses = Vec::new();
                for call in request["methodCalls"].as_array().unwrap() {
                    let method = call[0].as_str().unwrap();
                    let args = &call[1];
                    let call_id = &call[2];
                    let payload = match method {
                        "Email/changes" => {
                            let changes = changes.as_ref().unwrap();
                            json!({
                                "accountId": "mail-account", "oldState": "state-0",
                                "newState": "state-1", "hasMoreChanges": false,
                                "created": changes["created"], "updated": changes["updated"],
                                "destroyed": changes["destroyed"]
                            })
                        }
                        "Email/query" => json!({
                            "accountId": "mail-account", "queryState": "query-0",
                            "canCalculateChanges": false, "position": 0,
                            "ids": emails.iter().map(|email| &email["id"]).collect::<Vec<_>>(),
                            "total": emails.len()
                        }),
                        "Email/get" => {
                            let ids = args["ids"].as_array().unwrap();
                            if fail_on.as_ref().is_some_and(|(id, fail)| {
                                ids.iter().any(|requested| requested == id)
                                    && fail.load(Ordering::SeqCst)
                            }) {
                                responses.push(json!(["error", {"type": "serverFail"}, call_id]));
                                continue;
                            }
                            let mut list = Vec::new();
                            let mut not_found = Vec::new();
                            for id in ids {
                                match emails.iter().find(|email| &email["id"] == id) {
                                    Some(email) => list.push(email),
                                    None => not_found.push(id),
                                }
                            }
                            let state = if changes.is_some() { "state-1" } else { "state-0" };
                            json!({"accountId": "mail-account", "state": state, "list": list, "notFound": not_found})
                        }
                        other => panic!("unexpected method {other}"),
                    };
                    responses.push(json!([method, payload, call_id]));
                }
                json!({"methodResponses": responses, "sessionState": "session-0"})
            }
            .to_string();
            let http = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response.len(),
                response
            );
            stream.get_mut().write_all(http.as_bytes()).await.unwrap();
        }
    });
    let connected = client::connect_with(
        "reader@mail.example",
        "reader@mail.example",
        "example-password",
        &origin,
        None,
    )
    .await
    .unwrap();
    (connected, task)
}

async fn run_sync(
    ctx: &SyncCtx,
    config: &AccountConfig,
    connected: &ConnectedClient,
) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(10), sync_emails(ctx, config, connected))
        .await
        .expect("sync timed out")
}

#[tokio::test]
async fn replacement_message_preserves_conversation_and_snooze() {
    for not_found in [false, true] {
        let (_dir, ctx, config) = fixture().await;
        let wake_at = now_ms() + 86_400_000;
        let original_thread = seed_snoozed_thread(&ctx, wake_at).await;
        let changes = if not_found {
            // Email/changes reports an update, but the message disappears
            // before its Email/get page. Its replacement is on the next page.
            json!({"created": ["replacement-message"], "updated": ["old-message"], "destroyed": []})
        } else {
            json!({"created": ["replacement-message"], "updated": [], "destroyed": ["old-message"]})
        };
        let (connected, task) = server(
            vec![email("replacement-message", "remote-conversation")],
            Some(changes),
            None,
        )
        .await;
        run_sync(&ctx, &config, &connected).await.unwrap();
        task.abort();
        assert_eq!(
            snoozed_thread(&ctx).await,
            (Some(original_thread), Some(wake_at))
        );
        ctx.db
            .read(move |conn| {
                assert!(repo::messages::by_jmap_id(conn, 1, "old-message")?.is_none());
                let replacement =
                    repo::messages::by_jmap_id(conn, 1, "replacement-message")?.unwrap();
                assert_eq!(replacement.thread_id, Some(original_thread));
                Ok(())
            })
            .await
            .unwrap();
        assert_eq!(
            saved_email_state(&ctx, 1).await.unwrap(),
            Some("state-1".into())
        );
    }
}

#[tokio::test]
async fn interrupted_incremental_sync_defers_deletions_until_retry_finishes() {
    let (dir, ctx, config) = fixture().await;
    let wake_at = now_ms() + 86_400_000;
    let original_thread = seed_snoozed_thread(&ctx, wake_at).await;
    let raw_path = dir.path().join("old-message.eml");
    tokio::fs::write(&raw_path, b"Example cached message")
        .await
        .unwrap();
    let stored_path = raw_path.to_string_lossy().into_owned();
    ctx.db
        .write(move |conn| {
            conn.execute(
                "UPDATE messages SET raw_path=?1 WHERE jmap_id='old-message'",
                [stored_path],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let fail = Arc::new(AtomicBool::new(true));
    let (connected, task) = server(
        vec![
            email("email-a", "another-thread"),
            email("email-b", "remote-conversation"),
        ],
        Some(
            json!({"created": ["email-a", "email-b"], "updated": [], "destroyed": ["old-message"]}),
        ),
        Some(("email-b", fail.clone())),
    )
    .await;
    assert!(run_sync(&ctx, &config, &connected).await.is_err());
    assert_eq!(
        saved_email_state(&ctx, 1).await.unwrap(),
        Some("state-0".into())
    );
    assert_eq!(
        snoozed_thread(&ctx).await,
        (Some(original_thread), Some(wake_at))
    );
    assert!(raw_path.exists());
    ctx.db
        .read(|conn| {
            assert!(repo::messages::by_jmap_id(conn, 1, "old-message")?.is_some());
            assert!(repo::messages::by_jmap_id(conn, 1, "email-a")?.is_some());
            assert!(repo::messages::by_jmap_id(conn, 1, "email-b")?.is_none());
            Ok(())
        })
        .await
        .unwrap();
    fail.store(false, Ordering::SeqCst);
    run_sync(&ctx, &config, &connected).await.unwrap();
    task.abort();
    assert_eq!(
        snoozed_thread(&ctx).await,
        (Some(original_thread), Some(wake_at))
    );
    assert_eq!(
        saved_email_state(&ctx, 1).await.unwrap(),
        Some("state-1".into())
    );
    assert!(!raw_path.exists());
    ctx.db
        .read(move |conn| {
            assert!(repo::messages::by_jmap_id(conn, 1, "old-message")?.is_none());
            assert_eq!(
                repo::messages::by_jmap_id(conn, 1, "email-b")?
                    .unwrap()
                    .thread_id,
                Some(original_thread)
            );
            let count: i64 = conn.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))?;
            assert_eq!(count, 2);
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn first_sync_publishes_pages_and_retries_without_duplicates() {
    let (_dir, ctx, config) = fixture().await;
    ctx.db
        .write(|conn| {
            let (thread, message) =
                crate::db::testutil::seed_message(conn, "sender@mail.example", "Stale", false);
            conn.execute(
                "UPDATE threads SET jmap_id='remote-stale-thread' WHERE id=?1",
                [thread],
            )?;
            repo::messages::set_jmap_remote(
                conn,
                message,
                "remote-stale-message",
                Some("stale-blob"),
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let mut events = ctx.bus.subscribe();
    let fail = Arc::new(AtomicBool::new(true));
    let (connected, task) = server(
        vec![email("email-a", "thread-a"), email("email-b", "thread-b")],
        None,
        Some(("email-b", fail.clone())),
    )
    .await;
    assert!(run_sync(&ctx, &config, &connected).await.is_err());
    assert_eq!(saved_email_state(&ctx, 1).await.unwrap(), None);
    ctx.db
        .read(|conn| {
            assert!(repo::messages::by_jmap_id(conn, 1, "email-a")?.is_some());
            assert!(repo::messages::by_jmap_id(conn, 1, "email-b")?.is_none());
            assert!(repo::messages::by_jmap_id(conn, 1, "remote-stale-message")?.is_some());
            Ok(())
        })
        .await
        .unwrap();
    assert!(matches!(
        events.try_recv().unwrap(),
        CoreEvent::MailUpdated { .. }
    ));
    fail.store(false, Ordering::SeqCst);
    run_sync(&ctx, &config, &connected).await.unwrap();
    task.abort();
    assert_eq!(
        saved_email_state(&ctx, 1).await.unwrap(),
        Some("state-0".into())
    );
    ctx.db
        .read(|conn| {
            let counts: (i64, i64) = conn.query_row(
                "SELECT COUNT(*), COUNT(DISTINCT jmap_id) FROM messages",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            assert_eq!(counts, (2, 2));
            assert!(repo::messages::by_jmap_id(conn, 1, "remote-stale-message")?.is_none());
            assert!(repo::threads::by_jmap_id(conn, 1, "remote-stale-thread")?.is_none());
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn destroyed_only_sync_removes_empty_conversation() {
    let (_dir, ctx, config) = fixture().await;
    seed_snoozed_thread(&ctx, now_ms() + 86_400_000).await;
    let (connected, task) = server(
        Vec::new(),
        Some(json!({"created": [], "updated": [], "destroyed": ["old-message"]})),
        None,
    )
    .await;
    run_sync(&ctx, &config, &connected).await.unwrap();
    task.abort();
    assert_eq!(snoozed_thread(&ctx).await, (None, None));
    assert_eq!(
        saved_email_state(&ctx, 1).await.unwrap(),
        Some("state-1".into())
    );
    ctx.db
        .read(|conn| {
            assert!(repo::messages::by_jmap_id(conn, 1, "old-message")?.is_none());
            Ok(())
        })
        .await
        .unwrap();
}
