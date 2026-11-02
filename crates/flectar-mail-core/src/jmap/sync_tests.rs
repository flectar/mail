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

/// A JMAP server answering from fixed data. Every connection is served on its
/// own task, and every method call is recorded.
struct MockJmap {
    mailboxes: Vec<Value>,
    emails: Vec<Value>,
    changes: Option<Value>,
    /// Email/get fails with serverFail while this id is requested and the
    /// flag is set.
    fail_on: Option<(&'static str, Arc<AtomicBool>)>,
    /// Ids Email/get reports as notFound.
    missing: Vec<&'static str>,
    max_objects_in_get: usize,
    /// How long to hold back the answer to a method call.
    delay: Option<fn(&str, &Value) -> Option<Duration>>,
}

impl Default for MockJmap {
    fn default() -> Self {
        Self {
            mailboxes: Vec::new(),
            emails: Vec::new(),
            changes: None,
            fail_on: None,
            missing: Vec::new(),
            // One Email/get object per page.
            max_objects_in_get: 1,
            delay: None,
        }
    }
}

struct Mock {
    connected: ConnectedClient,
    task: tokio::task::JoinHandle<()>,
    calls: Arc<std::sync::Mutex<Vec<(String, Value)>>>,
    /// TCP connections the server accepted.
    connections: Arc<std::sync::atomic::AtomicUsize>,
}

impl Mock {
    /// The arguments of every recorded call to `method`, in order.
    fn calls(&self, method: &str) -> Vec<Value> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(name, _)| name == method)
            .map(|(_, arguments)| arguments.clone())
            .collect()
    }
}

impl MockJmap {
    async fn start(self) -> Mock {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let core = json!({
            "maxSizeUpload": 50_000_000, "maxConcurrentUpload": 4,
            "maxSizeRequest": 10_000_000, "maxConcurrentRequests": 4,
            "maxCallsInRequest": 16, "maxObjectsInGet": self.max_objects_in_get,
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
        let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let connections = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let server = Arc::new((self, session));
        let task = tokio::spawn({
            let calls = calls.clone();
            let connections = connections.clone();
            async move {
                loop {
                    let (stream, _) = listener.accept().await.unwrap();
                    connections.fetch_add(1, Ordering::SeqCst);
                    let server = server.clone();
                    let calls = calls.clone();
                    tokio::spawn(async move {
                        let (mock, session) = &*server;
                        mock.serve(stream, session, &calls).await;
                    });
                }
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
        Mock {
            connected,
            task,
            calls,
            connections,
        }
    }

    async fn serve(
        &self,
        stream: tokio::net::TcpStream,
        session: &Value,
        calls: &std::sync::Mutex<Vec<(String, Value)>>,
    ) {
        let mut stream = BufReader::new(stream);
        // Like a real server, keep the connection open for further requests.
        loop {
            let mut line = String::new();
            if stream.read_line(&mut line).await.unwrap_or(0) == 0 {
                return;
            }
            let get = line.starts_with("GET ");
            let download = line.starts_with("GET /download/");
            let mut length = 0;
            loop {
                line.clear();
                if stream.read_line(&mut line).await.unwrap_or(0) == 0 {
                    return;
                }
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
            if stream.read_exact(&mut body).await.is_err() {
                return;
            }
            let mut status = "200 OK";
            let mut hold = Duration::ZERO;
            let response = if download {
                "From: sender@mail.example\r\nSubject: Conversation\r\n\r\nBody\r\n".to_owned()
            } else if get {
                session.to_string()
            } else {
                let request: Value = serde_json::from_slice(&body).unwrap();
                // Like Fastmail, reject any capability the session does not
                // advertise (RFC 8620 §3.6.1).
                let unknown = request["using"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(Value::as_str)
                    .find(|capability| session["capabilities"].get(*capability).is_none());
                if let Some(capability) = unknown {
                    status = "400 Bad Request";
                    json!({
                        "type": "urn:ietf:params:jmap:error:unknownCapability",
                        "status": 400,
                        "detail": format!("unknown capability {capability}")
                    })
                    .to_string()
                } else {
                    let mut responses = Vec::new();
                    for call in request["methodCalls"].as_array().unwrap() {
                        let method = call[0].as_str().unwrap();
                        let call_id = &call[2];
                        calls
                            .lock()
                            .unwrap()
                            .push((method.to_owned(), call[1].clone()));
                        if let Some(delay) = self.delay.and_then(|delay| delay(method, &call[1])) {
                            hold = hold.max(delay);
                        }
                        match self.answer(method, &call[1]) {
                            Ok(payload) => responses.push(json!([method, payload, call_id])),
                            Err(error) => responses.push(json!(["error", error, call_id])),
                        }
                    }
                    json!({"methodResponses": responses, "sessionState": "session-0"}).to_string()
                }
            };
            let content_type = if download {
                "message/rfc822"
            } else {
                "application/json"
            };
            let http = format!(
                "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\r\n{}",
                response.len(),
                response
            );
            tokio::time::sleep(hold).await;
            // The client may have given up on a slow answer.
            if stream.get_mut().write_all(http.as_bytes()).await.is_err() {
                return;
            }
        }
    }

    fn answer(&self, method: &str, args: &Value) -> std::result::Result<Value, Value> {
        Ok(match method {
            "Email/changes" => {
                let changes = self.changes.as_ref().unwrap();
                json!({
                    "accountId": "mail-account", "oldState": "state-0",
                    "newState": "state-1", "hasMoreChanges": false,
                    "created": changes["created"], "updated": changes["updated"],
                    "destroyed": changes["destroyed"]
                })
            }
            "Email/query" => {
                let received = |value: &Value| {
                    chrono::DateTime::parse_from_rfc3339(value.as_str().unwrap()).unwrap()
                };
                let after = args["filter"].get("after").map(received);
                let ids = self
                    .emails
                    .iter()
                    .filter(|email| {
                        after.is_none_or(|after| received(&email["receivedAt"]) > after)
                    })
                    .map(|email| &email["id"])
                    .collect::<Vec<_>>();
                json!({
                    "accountId": "mail-account", "queryState": "query-0",
                    "canCalculateChanges": false, "position": 0,
                    "total": ids.len(), "ids": ids
                })
            }
            "Email/get" => {
                let ids = args["ids"].as_array().unwrap();
                if self.fail_on.as_ref().is_some_and(|(id, fail)| {
                    ids.iter().any(|requested| requested == id) && fail.load(Ordering::SeqCst)
                }) {
                    return Err(json!({"type": "serverFail"}));
                }
                let mut list = Vec::new();
                let mut not_found = Vec::new();
                for id in ids {
                    match self.emails.iter().find(|email| &email["id"] == id) {
                        Some(email) if !self.missing.iter().any(|missing| id == missing) => {
                            list.push(email)
                        }
                        _ => not_found.push(id),
                    }
                }
                let state = if self.changes.is_some() {
                    "state-1"
                } else {
                    "state-0"
                };
                json!({"accountId": "mail-account", "state": state, "list": list, "notFound": not_found})
            }
            // Like Stalwart, a Mailbox/get without ids returns no more than
            // maxObjectsInGet mailboxes, without saying so.
            "Mailbox/get" => {
                let list = match args["ids"].as_array() {
                    Some(ids) => self
                        .mailboxes
                        .iter()
                        .filter(|mailbox| ids.contains(&mailbox["id"]))
                        .collect::<Vec<_>>(),
                    None => self
                        .mailboxes
                        .iter()
                        .take(self.max_objects_in_get)
                        .collect(),
                };
                json!({"accountId": "mail-account", "state": "mailboxes-0", "list": list, "notFound": []})
            }
            "Mailbox/query" => {
                let position = args["position"].as_u64().unwrap_or(0) as usize;
                let limit = args["limit"]
                    .as_u64()
                    .map_or(usize::MAX, |limit| limit as usize);
                json!({
                    "accountId": "mail-account", "queryState": "mailbox-query-0",
                    "canCalculateChanges": false, "position": position,
                    "total": self.mailboxes.len(),
                    "ids": self.mailboxes.iter().skip(position).take(limit)
                        .map(|mailbox| &mailbox["id"]).collect::<Vec<_>>()
                })
            }
            other => panic!("unexpected method {other}"),
        })
    }
}

/// The original fixed server: one Email/get object per page, with an
/// optional failure on a later page.
async fn server(
    emails: Vec<Value>,
    changes: Option<Value>,
    fail_on: Option<(&'static str, Arc<AtomicBool>)>,
) -> (ConnectedClient, tokio::task::JoinHandle<()>) {
    let mock = MockJmap {
        emails,
        changes,
        fail_on,
        ..MockJmap::default()
    }
    .start()
    .await;
    (mock.connected, mock.task)
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
async fn previews_are_stored_on_one_line() {
    let (_dir, ctx, config) = fixture().await;
    let mut message = email("multi-line", "remote-conversation");
    message["preview"] = json!("\nDear Partner,\r\n\n  As previously reported");
    let (connected, task) = server(vec![message], None, None).await;
    run_sync(&ctx, &config, &connected).await.unwrap();
    task.abort();
    let snippet: String = ctx
        .db
        .read(|conn| {
            Ok(conn.query_row(
                "SELECT snippet FROM messages WHERE jmap_id = 'multi-line'",
                [],
                |row| row.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(snippet, "Dear Partner, As previously reported");
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

#[tokio::test]
async fn requests_declare_only_advertised_capabilities() {
    let (connected, task) = server(Vec::new(), None, None).await;
    let request = connected.request();
    let using = request
        .using
        .iter()
        .map(|capability| capability.as_ref())
        .collect::<Vec<_>>();
    assert_eq!(
        using,
        [
            client::CORE_CAPABILITY,
            client::MAIL_CAPABILITY,
            client::SUBMISSION_CAPABILITY
        ]
    );
    task.abort();
}

fn with_attachment(mut email: Value, size: i64, received_at: &str) -> Value {
    let id = email["id"].as_str().unwrap().to_owned();
    email["hasAttachment"] = json!(true);
    email["size"] = json!(size);
    email["receivedAt"] = json!(received_at);
    email["attachments"] = json!([{
        "partId": "2", "blobId": format!("part-{id}"), "size": 10,
        "name": format!("{id}.pdf"), "type": "application/pdf",
        "disposition": "attachment"
    }]);
    email
}

fn listed_ids(call: &Value) -> Vec<&str> {
    call["ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|id| id.as_str().unwrap())
        .collect()
}

/// One attachment listing request with the full byte budget.
async fn list_once(ctx: &SyncCtx, config: &AccountConfig, c: &ConnectedClient) -> bool {
    let mut budget = ATTACHMENT_LISTING_BYTES;
    list_attachments(ctx, config, c, &mut budget).await.unwrap()
}

async fn attachment_rows(ctx: &SyncCtx) -> Vec<(String, String, String, String)> {
    ctx.db
        .read(|conn| {
            let mut stmt = conn.prepare(
                "SELECT m.jmap_id, a.part_id, a.filename, a.jmap_blob_id
                 FROM attachments a JOIN messages m ON m.id = a.message_id
                 ORDER BY m.jmap_id, a.part_id",
            )?;
            Ok(stmt
                .query_map([], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn header_sync_leaves_attachment_lists_to_the_listing_pass() {
    let (_dir, ctx, config) = fixture().await;
    let mock = MockJmap {
        emails: vec![
            with_attachment(email("email-a", "thread-a"), 100, "2026-10-08T12:00:00Z"),
            email("email-b", "thread-b"),
        ],
        ..MockJmap::default()
    }
    .start()
    .await;
    run_sync(&ctx, &config, &mock.connected).await.unwrap();
    // Stalwart reads every requested message in full to list attachments.
    let header_pages = mock.calls("Email/get");
    assert!(!header_pages.is_empty());
    for page in &header_pages {
        assert!(
            !page["properties"]
                .as_array()
                .unwrap()
                .contains(&json!("attachments")),
            "{page}"
        );
    }
    assert!(attachment_rows(&ctx).await.is_empty());

    assert!(list_once(&ctx, &config, &mock.connected).await);
    let listing = mock.calls("Email/get").pop().unwrap();
    assert_eq!(listed_ids(&listing), ["email-a"]);
    assert_eq!(
        listing["properties"],
        json!(["id", "blobId", "attachments"])
    );
    assert_eq!(
        attachment_rows(&ctx).await,
        [(
            "email-a".to_owned(),
            "2".to_owned(),
            "email-a.pdf".to_owned(),
            "part-email-a".to_owned()
        )]
    );
    let calls = mock.calls("Email/get").len();
    assert!(!list_once(&ctx, &config, &mock.connected).await);
    assert_eq!(mock.calls("Email/get").len(), calls);
    mock.task.abort();
}

#[tokio::test]
async fn attachment_listing_bounds_the_message_bytes_of_each_request() {
    let (_dir, ctx, config) = fixture().await;
    let mib = 1024 * 1024;
    let mock = MockJmap {
        emails: vec![
            with_attachment(
                email("email-a", "thread-a"),
                20 * mib,
                "2026-10-08T12:00:05Z",
            ),
            with_attachment(
                email("email-b", "thread-b"),
                20 * mib,
                "2026-10-08T12:00:04Z",
            ),
            with_attachment(
                email("email-c", "thread-c"),
                40 * mib,
                "2026-10-08T12:00:03Z",
            ),
            with_attachment(email("email-d", "thread-d"), 1024, "2026-10-08T12:00:02Z"),
            with_attachment(email("email-e", "thread-e"), 1024, "2026-10-08T12:00:01Z"),
        ],
        max_objects_in_get: 500,
        ..MockJmap::default()
    }
    .start()
    .await;
    run_sync(&ctx, &config, &mock.connected).await.unwrap();
    let headers = mock.calls("Email/get").len();
    while list_once(&ctx, &config, &mock.connected).await {}
    let listings = mock.calls("Email/get").split_off(headers);
    // Newest first; a message larger than the budget goes alone.
    assert_eq!(
        listings.iter().map(listed_ids).collect::<Vec<_>>(),
        [
            vec!["email-a"],
            vec!["email-b"],
            vec!["email-c"],
            vec!["email-d", "email-e"],
        ]
    );
    assert_eq!(attachment_rows(&ctx).await.len(), 5);
    mock.task.abort();
}

#[tokio::test]
async fn attachment_listing_moves_past_messages_the_server_no_longer_matches() {
    let (_dir, ctx, config) = fixture().await;
    let mock = MockJmap {
        emails: vec![
            with_attachment(email("email-a", "thread-a"), 100, "2026-10-08T12:00:02Z"),
            with_attachment(email("email-b", "thread-b"), 100, "2026-10-08T12:00:01Z"),
        ],
        max_objects_in_get: 500,
        ..MockJmap::default()
    }
    .start()
    .await;
    run_sync(&ctx, &config, &mock.connected).await.unwrap();
    ctx.db
        .write(|conn| {
            // Header sync has not seen the server's current copy yet.
            conn.execute(
                "UPDATE messages SET jmap_blob_id='blob-newer' WHERE jmap_id='email-b'",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let headers = mock.calls("Email/get").len();
    // The server has since deleted email-a.
    let gone = MockJmap {
        emails: vec![
            with_attachment(email("email-a", "thread-a"), 100, "2026-10-08T12:00:02Z"),
            with_attachment(email("email-b", "thread-b"), 100, "2026-10-08T12:00:01Z"),
        ],
        missing: vec!["email-a"],
        max_objects_in_get: 500,
        ..MockJmap::default()
    }
    .start()
    .await;
    assert!(list_once(&ctx, &config, &gone.connected).await);
    assert!(!list_once(&ctx, &config, &gone.connected).await);
    assert_eq!(gone.calls("Email/get").len(), 1);
    assert_eq!(mock.calls("Email/get").len(), headers);
    assert!(attachment_rows(&ctx).await.is_empty());
    ctx.db
        .read(|conn| {
            let markers = conn
                .prepare("SELECT jmap_id, jmap_attachments_blob_id FROM messages ORDER BY jmap_id")?
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            assert_eq!(
                markers,
                [
                    ("email-a".to_owned(), "blob-email-a".to_owned()),
                    ("email-b".to_owned(), "blob-newer".to_owned())
                ]
            );
            Ok(())
        })
        .await
        .unwrap();
    mock.task.abort();
    gone.task.abort();
}

#[tokio::test]
async fn adopting_a_message_drops_the_attachment_rows_of_its_previous_copy() {
    let (_dir, ctx, config) = fixture().await;
    let received = chrono::DateTime::parse_from_rfc3339("2026-10-08T12:00:00Z")
        .unwrap()
        .timestamp_millis();
    let local = ctx
        .db
        .write(move |conn| {
            let (_, message) = crate::db::testutil::seed_message(
                conn,
                "sender@mail.example",
                "Conversation",
                false,
            );
            conn.execute(
                "UPDATE messages SET message_id='email-a@mail.example', size=100,
                        date=?2, internal_date=?2, has_attachments=1
                 WHERE id=?1",
                params![message, received],
            )?;
            conn.execute(
                "INSERT INTO attachments(message_id, part_id, filename, imap_section)
                 VALUES(?1, '2', 'old-copy.pdf', '2')",
                [message],
            )?;
            Ok(message)
        })
        .await
        .unwrap();
    let mock = MockJmap {
        emails: vec![with_attachment(
            email("email-a", "thread-a"),
            100,
            "2026-10-08T12:00:00Z",
        )],
        ..MockJmap::default()
    }
    .start()
    .await;
    run_sync(&ctx, &config, &mock.connected).await.unwrap();
    ctx.db
        .read(move |conn| {
            let adopted = repo::messages::by_jmap_id(conn, 1, "email-a")?.unwrap();
            assert_eq!(adopted.id, local);
            let queue = repo::messages::jmap_attachment_listing_queue(conn, 1, 10)?;
            assert_eq!(
                queue.iter().map(|item| item.message_id).collect::<Vec<_>>(),
                [local]
            );
            Ok(())
        })
        .await
        .unwrap();
    assert!(attachment_rows(&ctx).await.is_empty());
    assert!(list_once(&ctx, &config, &mock.connected).await);
    assert_eq!(
        attachment_rows(&ctx).await,
        [(
            "email-a".to_owned(),
            "2".to_owned(),
            "email-a.pdf".to_owned(),
            "part-email-a".to_owned()
        )]
    );
    mock.task.abort();
}

/// Email/get calls naming more than two messages answer too late.
fn slow_header_pages(method: &str, args: &Value) -> Option<Duration> {
    let ids = args["ids"].as_array()?;
    (method == "Email/get" && ids.len() > 2).then_some(Duration::from_secs(5))
}

#[tokio::test]
async fn header_pages_that_time_out_are_fetched_in_halves() {
    let (_dir, ctx, config) = fixture().await;
    let mut mock = MockJmap {
        emails: (0..6)
            .map(|n| email(&format!("email-{n}"), &format!("thread-{n}")))
            .collect(),
        max_objects_in_get: 6,
        delay: Some(slow_header_pages),
        ..MockJmap::default()
    }
    .start()
    .await;
    mock.connected.timeout = Duration::from_millis(300);
    run_sync(&ctx, &config, &mock.connected).await.unwrap();
    let pages = mock
        .calls("Email/get")
        .iter()
        .map(|call| call["ids"].as_array().unwrap().len())
        .collect::<Vec<_>>();
    // After the state request, pages of six and three time out; the rest
    // keep the size that worked.
    assert_eq!(pages, [0, 6, 3, 1, 1, 1, 1, 1, 1]);
    assert_eq!(
        saved_email_state(&ctx, 1).await.unwrap(),
        Some("state-0".into())
    );
    ctx.db
        .read(|conn| {
            let count: i64 = conn.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))?;
            assert_eq!(count, 6);
            Ok(())
        })
        .await
        .unwrap();
    mock.task.abort();
}

/// Attachment listings answer too late when they name more than one message,
/// and always for email-a.
fn slow_listings(method: &str, args: &Value) -> Option<Duration> {
    let ids = args["ids"].as_array()?;
    let listing = args["properties"]
        .as_array()
        .is_some_and(|properties| properties.contains(&json!("attachments")));
    (method == "Email/get" && listing && (ids.len() > 1 || ids.contains(&json!("email-a"))))
        .then_some(Duration::from_secs(5))
}

/// Attachment listings answer too late when they name more than one message.
fn slow_shared_listings(method: &str, args: &Value) -> Option<Duration> {
    let ids = args["ids"].as_array()?;
    let listing = args["properties"]
        .as_array()
        .is_some_and(|properties| properties.contains(&json!("attachments")));
    (method == "Email/get" && listing && ids.len() > 1).then_some(Duration::from_secs(5))
}

#[tokio::test]
async fn attachment_listings_that_time_out_shrink_without_known_sizes() {
    let (_dir, ctx, config) = fixture().await;
    let mut mock = MockJmap {
        emails: vec![
            with_attachment(email("email-x", "thread-x"), 0, "2026-10-08T12:00:02Z"),
            with_attachment(email("email-y", "thread-y"), 0, "2026-10-08T12:00:01Z"),
        ],
        max_objects_in_get: 500,
        delay: Some(slow_shared_listings),
        ..MockJmap::default()
    }
    .start()
    .await;
    run_sync(&ctx, &config, &mock.connected).await.unwrap();
    mock.connected.timeout = Duration::from_millis(300);
    let headers = mock.calls("Email/get").len();
    let mut budget = ATTACHMENT_LISTING_BYTES;
    tokio::time::timeout(Duration::from_secs(10), async {
        while list_attachments(&ctx, &config, &mock.connected, &mut budget)
            .await
            .unwrap()
        {}
    })
    .await
    .expect("listing made no progress");
    let listings = mock.calls("Email/get").split_off(headers);
    assert_eq!(
        listings.iter().map(listed_ids).collect::<Vec<_>>(),
        [vec!["email-x", "email-y"], vec!["email-x"], vec!["email-y"]]
    );
    assert_eq!(attachment_rows(&ctx).await.len(), 2);
    mock.task.abort();
}

#[tokio::test]
async fn attachment_listings_that_time_out_shrink_and_skip_what_never_answers() {
    let (_dir, ctx, config) = fixture().await;
    let mut mock = MockJmap {
        emails: vec![
            with_attachment(email("email-a", "thread-a"), 1024, "2026-10-08T12:00:03Z"),
            with_attachment(email("email-b", "thread-b"), 1024, "2026-10-08T12:00:02Z"),
            with_attachment(email("email-c", "thread-c"), 1024, "2026-10-08T12:00:01Z"),
        ],
        max_objects_in_get: 500,
        delay: Some(slow_listings),
        ..MockJmap::default()
    }
    .start()
    .await;
    run_sync(&ctx, &config, &mock.connected).await.unwrap();
    mock.connected.timeout = Duration::from_millis(300);
    let headers = mock.calls("Email/get").len();
    let mut budget = ATTACHMENT_LISTING_BYTES;
    while list_attachments(&ctx, &config, &mock.connected, &mut budget)
        .await
        .unwrap()
    {}
    let listings = mock.calls("Email/get").split_off(headers);
    assert_eq!(
        listings.iter().map(listed_ids).collect::<Vec<_>>(),
        [
            vec!["email-a", "email-b", "email-c"],
            vec!["email-a"],
            vec!["email-b"],
            vec!["email-c"],
        ]
    );
    // Half of what timed out, which later listings keep.
    assert_eq!(budget, 1536);
    assert_eq!(
        attachment_rows(&ctx)
            .await
            .into_iter()
            .map(|row| row.0)
            .collect::<Vec<_>>(),
        ["email-b", "email-c"]
    );
    mock.task.abort();
}

#[tokio::test]
async fn interrupted_full_sync_resumes_where_it_stopped() {
    let (_dir, ctx, config) = fixture().await;
    let fail = Arc::new(AtomicBool::new(true));
    let mock = MockJmap {
        emails: vec![
            email("email-a", "thread-a"),
            email("email-b", "thread-b"),
            email("email-c", "thread-c"),
        ],
        fail_on: Some(("email-c", fail.clone())),
        ..MockJmap::default()
    }
    .start()
    .await;
    assert!(run_sync(&ctx, &config, &mock.connected).await.is_err());
    assert_eq!(saved_email_state(&ctx, 1).await.unwrap(), None);
    let first_attempt = mock.calls("Email/get").len();

    fail.store(false, Ordering::SeqCst);
    run_sync(&ctx, &config, &mock.connected).await.unwrap();
    // Only what is still missing is fetched, and the state the full sync
    // started from becomes the checkpoint.
    let resumed = mock.calls("Email/get").split_off(first_attempt);
    assert_eq!(
        resumed.iter().map(listed_ids).collect::<Vec<_>>(),
        [vec!["email-c"]]
    );
    assert_eq!(
        saved_email_state(&ctx, 1).await.unwrap(),
        Some("state-0".into())
    );
    assert_eq!(unfinished_full_sync(&ctx, 1).await.unwrap(), None);
    ctx.db
        .read(|conn| {
            let stored: i64 =
                conn.query_row("SELECT COUNT(*) FROM jmap_full_sync_stored", [], |r| {
                    r.get(0)
                })?;
            assert_eq!(stored, 0);
            let messages: i64 =
                conn.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))?;
            assert_eq!(messages, 3);
            Ok(())
        })
        .await
        .unwrap();
    mock.task.abort();
}

fn received(email: Value, at: chrono::DateTime<chrono::Utc>) -> Value {
    let mut email = email;
    email["receivedAt"] = json!(at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
    email
}

#[tokio::test]
async fn full_sync_after_a_server_change_keeps_only_mail_on_the_new_server() {
    let (_dir, ctx, mut config) = fixture().await;
    config.settings.mail_history = MailHistory::OneMonth;
    let now = chrono::Utc::now();
    let day = chrono::Duration::days(1);
    let (recent, old, unmatched_old, ancient_local, ancient) = (
        now - day,
        now - day * 60,
        now - day * 70,
        now - day * 90,
        now - day * 200,
    );
    // What the account kept from its IMAP server, next to the folders its
    // first JMAP mailbox sync created.
    let (gone_action, draft) = ctx
        .db
        .write(move |conn| {
            conn.execute_batch(
                "UPDATE folders SET jmap_id = NULL WHERE id = 1;
                 INSERT INTO folders (id, account_id, path, role, jmap_id) VALUES
                   (2, 1, 'Inbox', 'inbox', 'inbox-remote'),
                   (3, 1, 'Archive', 'archive', NULL),
                   (4, 1, 'Drafts', 'drafts', NULL),
                   (5, 1, 'Drafts on the server', 'drafts', 'drafts-remote'),
                   (6, 1, 'Sent', 'sent', NULL),
                   (7, 1, 'Sent on the server', 'sent', 'sent-remote');",
            )?;
            let imap_copy =
                |folder: i64, uid: i64, message_id: &str, at: chrono::DateTime<chrono::Utc>| {
                    let (_, message) = crate::db::testutil::seed_message(
                        conn,
                        "sender@mail.example",
                        "Conversation",
                        false,
                    );
                    conn.execute(
                        "UPDATE messages SET folder_id=?2, uid=?3, message_id=?4, size=100,
                            date=?5, internal_date=?5
                     WHERE id=?1",
                        params![message, folder, uid, message_id, at.timestamp_millis()],
                    )
                    .map(|_| message)
                };
            imap_copy(1, 7, "email-recent@mail.example", recent)?;
            let gone = imap_copy(3, 7, "gone@mail.example", recent)?;
            imap_copy(1, 8, "email-old@mail.example", old)?;
            imap_copy(1, 9, "ancient-gone@mail.example", ancient_local)?;
            let draft = imap_copy(4, 3, "draft@mail.example", recent)?;
            conn.execute("UPDATE messages SET is_draft=1 WHERE id=?1", [draft])?;
            conn.execute("INSERT INTO drafts_meta (message_id) VALUES (?1)", [draft])?;
            // Sent mail the IMAP server never had a copy of, in its folder and
            // in one this server has nothing like.
            for (folder, message_id) in [
                (6, "sent-here@mail.example"),
                (3, "filed-here@mail.example"),
            ] {
                conn.execute(
                    "INSERT INTO messages (account_id, folder_id, message_id, subject,
                       from_addr, date, internal_date, is_outgoing)
                     VALUES (1, ?1, ?2, 'Conversation', 'me@test.dev', ?3, ?3, 1)",
                    params![folder, message_id, recent.timestamp_millis()],
                )?;
            }
            let gone_action =
                repo::actions::enqueue(conn, 1, "trash", Some(gone), None, &json!({}), None)?;
            // As the account's move to this server left it.
            repo::messages::await_relink(conn, 1, None)?;
            Ok((gone_action, draft))
        })
        .await
        .unwrap();
    let mock = MockJmap {
        emails: vec![
            received(email("email-new", "thread-new"), recent),
            received(email("email-recent", "thread-recent"), recent),
            received(email("email-old", "thread-old"), old),
            received(
                email("email-unmatched-old", "thread-unmatched"),
                unmatched_old,
            ),
            received(email("email-ancient", "thread-ancient"), ancient),
        ],
        max_objects_in_get: 500,
        ..MockJmap::default()
    }
    .start()
    .await;
    run_sync(&ctx, &config, &mock.connected).await.unwrap();

    // The query reaches back to the oldest message left here, past the
    // account's month of history, so that message could be matched.
    let query = mock.calls("Email/query").pop().unwrap();
    let after =
        chrono::DateTime::parse_from_rfc3339(query["filter"]["after"].as_str().unwrap()).unwrap();
    assert!(after < ancient_local && after > ancient, "{after}");
    let listed = mock.calls("Email/get").pop().unwrap();
    assert_eq!(listed_ids(&listed).len(), 4);
    ctx.db
        .read(move |conn| {
            let messages = conn
                .prepare(
                    "SELECT COALESCE(jmap_id, message_id), folder_id, uid FROM messages
                     ORDER BY 1",
                )?
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            assert_eq!(
                messages,
                [
                    ("draft@mail.example".to_owned(), 5, None),
                    ("email-new".to_owned(), 2, None),
                    ("email-old".to_owned(), 2, None),
                    ("email-recent".to_owned(), 2, None),
                    ("filed-here@mail.example".to_owned(), 7, None),
                    ("sent-here@mail.example".to_owned(), 7, None),
                ]
            );
            let waiting: i64 = conn.query_row(
                "SELECT COUNT(*) FROM messages WHERE relink_pending = 1",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(waiting, 0);
            assert!(repo::messages::get_row(conn, draft)?.is_some());
            let folders = conn
                .prepare("SELECT id FROM folders ORDER BY id")?
                .query_map([], |row| row.get::<_, i64>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            assert_eq!(folders, [2, 5, 7]);
            assert_eq!(
                repo::actions::get(conn, gone_action)?.unwrap().state,
                "cancelled"
            );
            Ok(())
        })
        .await
        .unwrap();
    mock.task.abort();
}

fn mailbox(id: &str, name: &str, role: Option<&str>) -> Value {
    json!({
        "id": id, "name": name, "parentId": null, "role": role,
        "myRights": {
            "mayReadItems": true, "mayAddItems": true, "mayRemoveItems": true,
            "maySetSeen": true, "maySetKeywords": true, "mayCreateChild": true,
            "mayRename": true, "mayDelete": true, "maySubmit": true
        }
    })
}

async fn synced_mailboxes(ctx: &SyncCtx) -> Vec<String> {
    ctx.db
        .read(|conn| {
            Ok(conn
                .prepare("SELECT jmap_id FROM folders WHERE jmap_id IS NOT NULL ORDER BY jmap_id")?
                .query_map([], |row| row.get(0))?
                .collect::<rusqlite::Result<Vec<String>>>()?)
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn mailboxes_past_the_get_limit_are_all_listed() {
    let (_dir, ctx, _config) = fixture().await;
    let mock = MockJmap {
        mailboxes: vec![
            mailbox("inbox-remote", "Inbox", Some("inbox")),
            mailbox("archive-remote", "Archive", Some("archive")),
            mailbox("projects-remote", "Projects", None),
            mailbox("receipts-remote", "Receipts", None),
            mailbox("travel-remote", "Travel", None),
        ],
        max_objects_in_get: 2,
        ..MockJmap::default()
    }
    .start()
    .await;
    sync_mailboxes(&ctx, 1, &mock.connected).await.unwrap();
    assert_eq!(
        synced_mailboxes(&ctx).await,
        [
            "archive-remote",
            "inbox-remote",
            "projects-remote",
            "receipts-remote",
            "travel-remote"
        ]
    );
    // The first answer filled the limit, so the rest came in pages of two.
    let pages = mock
        .calls("Mailbox/get")
        .iter()
        .map(|call| call["ids"].as_array().map(Vec::len))
        .collect::<Vec<_>>();
    assert_eq!(pages, [None, Some(2), Some(2), Some(1)]);
    mock.task.abort();
}

#[tokio::test]
async fn mailboxes_within_the_get_limit_take_one_request() {
    let (_dir, ctx, _config) = fixture().await;
    let mock = MockJmap {
        mailboxes: vec![
            mailbox("inbox-remote", "Inbox", Some("inbox")),
            mailbox("archive-remote", "Archive", Some("archive")),
        ],
        max_objects_in_get: 3,
        ..MockJmap::default()
    }
    .start()
    .await;
    sync_mailboxes(&ctx, 1, &mock.connected).await.unwrap();
    assert_eq!(
        synced_mailboxes(&ctx).await,
        ["archive-remote", "inbox-remote"]
    );
    assert_eq!(mock.calls("Mailbox/get").len(), 1);
    assert!(mock.calls("Mailbox/query").is_empty());
    mock.task.abort();
}

#[tokio::test]
async fn one_connection_carries_every_request_of_a_session() {
    let (_dir, ctx, config) = fixture().await;
    let mock = MockJmap {
        emails: (0..6)
            .map(|n| email(&format!("email-{n}"), &format!("thread-{n}")))
            .collect(),
        ..MockJmap::default()
    }
    .start()
    .await;
    // Discovery and the Session each used their own connection.
    let connected = mock.connections.load(Ordering::SeqCst);
    run_sync(&ctx, &config, &mock.connected).await.unwrap();
    // The state request, the query and six header pages.
    assert_eq!(mock.calls("Email/get").len(), 7);
    assert_eq!(mock.connections.load(Ordering::SeqCst), connected + 1);
    mock.task.abort();
}

#[tokio::test]
async fn body_fetches_reuse_one_session() {
    let (_dir, ctx, mut config) = fixture().await;
    let mock = MockJmap {
        emails: vec![email("email-a", "thread-a"), email("email-b", "thread-b")],
        ..MockJmap::default()
    }
    .start()
    .await;
    run_sync(&ctx, &config, &mock.connected).await.unwrap();
    config.jmap_url = mock.connected.base_url.clone();
    credentials::store_async(
        ctx.credentials.clone(),
        config.id,
        Slot::Password,
        "example-password".into(),
    )
    .await
    .unwrap();
    let messages = ctx
        .db
        .read(|conn| {
            Ok([
                repo::messages::by_jmap_id(conn, 1, "email-a")?.unwrap().id,
                repo::messages::by_jmap_id(conn, 1, "email-b")?.unwrap().id,
            ])
        })
        .await
        .unwrap();
    let before = mock.connections.load(Ordering::SeqCst);
    let mut session = None;
    for message in messages {
        fetch_body(&ctx, &config, &mut session, message)
            .await
            .unwrap();
    }
    // Discovery, the Session and one connection for both downloads.
    assert_eq!(mock.connections.load(Ordering::SeqCst), before + 3);
    ctx.db
        .read(move |conn| {
            for message in messages {
                assert_eq!(
                    repo::messages::get_row(conn, message)?.unwrap().body_state,
                    "cached"
                );
            }
            Ok(())
        })
        .await
        .unwrap();
    mock.task.abort();
}
