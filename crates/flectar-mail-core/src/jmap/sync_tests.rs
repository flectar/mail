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
        pace: Default::default(),
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
    /// What Mailbox/changes answers: its `created` and `updated` ids and
    /// `hasMoreChanges`, or an `error` type.
    mailbox_changes: Option<Value>,
    /// Email/get fails with serverFail while this id is requested and the
    /// flag is set.
    fail_on: Option<(&'static str, Arc<AtomicBool>)>,
    /// Ids Email/get reports as notFound.
    missing: Vec<&'static str>,
    max_objects_in_get: usize,
    /// How long to hold back the answer to a method call.
    delay: Option<fn(&str, &Value) -> Option<Duration>>,
    /// The longest the server holds a submission, in seconds.
    max_delayed_send: u64,
    /// The EmailSubmissions the server keeps.
    submissions: Arc<std::sync::Mutex<Vec<Value>>>,
    /// Emails Email/set marked `$seen`.
    seen: Arc<std::sync::Mutex<Vec<String>>>,
    /// Email/set answers as if it marked them, and doesn't.
    forget_seen: bool,
    /// What a download of these blobs returns; any other is a short message.
    blobs: Vec<(&'static str, &'static str)>,
}

impl Default for MockJmap {
    fn default() -> Self {
        Self {
            mailboxes: Vec::new(),
            emails: Vec::new(),
            changes: None,
            mailbox_changes: None,
            fail_on: None,
            missing: Vec::new(),
            // One Email/get object per page.
            max_objects_in_get: 1,
            delay: None,
            max_delayed_send: 0,
            submissions: Arc::new(std::sync::Mutex::new(Vec::new())),
            seen: Arc::default(),
            forget_seen: false,
            blobs: Vec::new(),
        }
    }
}

struct Mock {
    connected: ConnectedClient,
    task: tokio::task::JoinHandle<()>,
    calls: Arc<std::sync::Mutex<Vec<(String, Value)>>>,
    /// TCP connections the server accepted.
    connections: Arc<std::sync::atomic::AtomicUsize>,
    /// The EmailSubmissions the server keeps.
    submissions: Arc<std::sync::Mutex<Vec<Value>>>,
    /// The blobs downloaded, in order.
    downloads: Arc<std::sync::Mutex<Vec<String>>>,
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
        // As RFC 8621 and Fastmail have it: extensions as an object.
        let submission = json!({
            "maxDelayedSend": self.max_delayed_send,
            "submissionExtensions": {"futurerelease": []}
        });
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
        let downloads = Arc::new(std::sync::Mutex::new(Vec::new()));
        let submissions = self.submissions.clone();
        let server = Arc::new((self, session));
        let task = tokio::spawn({
            let calls = calls.clone();
            let connections = connections.clone();
            let downloads = downloads.clone();
            async move {
                loop {
                    let (stream, _) = listener.accept().await.unwrap();
                    connections.fetch_add(1, Ordering::SeqCst);
                    let server = server.clone();
                    let calls = calls.clone();
                    let downloads = downloads.clone();
                    tokio::spawn(async move {
                        let (mock, session) = &*server;
                        mock.serve(stream, session, &calls, &downloads).await;
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
            submissions,
            downloads,
        }
    }

    async fn serve(
        &self,
        stream: tokio::net::TcpStream,
        session: &Value,
        calls: &std::sync::Mutex<Vec<(String, Value)>>,
        downloads: &std::sync::Mutex<Vec<String>>,
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
            // GET /download/{accountId}/{blobId}/{name}?accept={type} HTTP/1.1
            let blob = if download {
                line.split('/').nth(3).unwrap_or_default().to_owned()
            } else {
                String::new()
            };
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
                downloads.lock().unwrap().push(blob.clone());
                match self.blobs.iter().find(|(id, _)| *id == blob) {
                    Some((_, content)) => (*content).to_owned(),
                    None => "From: sender@mail.example\r\nSubject: Conversation\r\n\r\nBody\r\n"
                        .to_owned(),
                }
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
                // One condition, or the conditions of an AND.
                let conditions = match args["filter"].get("conditions") {
                    Some(conditions) => conditions.as_array().unwrap().clone(),
                    None => vec![args["filter"].clone()],
                };
                let condition = |name: &str| conditions.iter().find_map(|c| c.get(name).cloned());
                let after = condition("after").map(|value| received(&value));
                let before = condition("before").map(|value| received(&value));
                let mailbox = condition("inMailbox");
                let not_keyword = condition("notKeyword");
                let seen = self.seen.lock().unwrap().clone();
                let limit = args["limit"]
                    .as_u64()
                    .map_or(usize::MAX, |limit| limit as usize);
                let position = args["position"].as_u64().unwrap_or(0) as usize;
                let matching = self
                    .emails
                    .iter()
                    .filter(|email| {
                        after.is_none_or(|after| received(&email["receivedAt"]) > after)
                            && before.is_none_or(|before| received(&email["receivedAt"]) < before)
                            && mailbox.as_ref().is_none_or(|mailbox| {
                                email["mailboxIds"][mailbox.as_str().unwrap()] == true
                            })
                            && not_keyword.as_ref().is_none_or(|keyword| {
                                let keyword = keyword.as_str().unwrap();
                                email["keywords"][keyword] != true
                                    && !(keyword == "$seen"
                                        && seen.iter().any(|id| email["id"] == id.as_str()))
                            })
                    })
                    .map(|email| &email["id"])
                    .collect::<Vec<_>>();
                let ids = matching
                    .iter()
                    .skip(position)
                    .take(limit)
                    .collect::<Vec<_>>();
                json!({
                    "accountId": "mail-account", "queryState": "query-0",
                    "canCalculateChanges": false, "position": position,
                    "total": matching.len(), "ids": ids
                })
            }
            "Email/set" => {
                let mut updated = serde_json::Map::new();
                for (id, patch) in args["update"].as_object().into_iter().flatten() {
                    if patch["keywords/$seen"] == true && !self.forget_seen {
                        self.seen.lock().unwrap().push(id.clone());
                    }
                    updated.insert(id.clone(), Value::Null);
                }
                json!({
                    "accountId": "mail-account", "oldState": "state-0", "newState": "state-1",
                    "created": {}, "updated": updated, "destroyed": [],
                    "notCreated": {}, "notUpdated": {}, "notDestroyed": {}
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
            "Mailbox/changes" => {
                let changes = self.mailbox_changes.as_ref().unwrap();
                if let Some(error) = changes.get("error") {
                    return Err(json!({"type": error}));
                }
                json!({
                    "accountId": "mail-account", "oldState": args["sinceState"],
                    "newState": "mailboxes-new", "hasMoreChanges": changes["hasMoreChanges"],
                    "created": changes["created"], "updated": changes["updated"],
                    "destroyed": [], "updatedProperties": null
                })
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
            "Identity/get" => json!({
                "accountId": "mail-account", "state": "identities-0", "notFound": [],
                "list": [{"id": "identity-1", "name": "Reader", "email": "reader@mail.example"}]
            }),
            "EmailSubmission/query" => {
                let submissions = self.submissions.lock().unwrap();
                let filter = &args["filter"];
                let ids = submissions
                    .iter()
                    .filter(|submission| {
                        filter["emailIds"]
                            .as_array()
                            .is_none_or(|ids| ids.contains(&submission["emailId"]))
                            && filter
                                .get("undoStatus")
                                .is_none_or(|status| status == &submission["undoStatus"])
                    })
                    .map(|submission| submission["id"].clone())
                    .collect::<Vec<_>>();
                json!({
                    "accountId": "mail-account", "queryState": "submissions-0",
                    "canCalculateChanges": false, "position": 0,
                    "total": ids.len(), "ids": ids
                })
            }
            "EmailSubmission/get" => {
                let submissions = self.submissions.lock().unwrap();
                let ids = args["ids"].as_array().unwrap();
                let list = submissions
                    .iter()
                    .filter(|submission| ids.contains(&submission["id"]))
                    .collect::<Vec<_>>();
                let not_found = ids
                    .iter()
                    .filter(|id| !list.iter().any(|submission| &submission["id"] == *id))
                    .collect::<Vec<_>>();
                json!({
                    "accountId": "mail-account", "state": "submissions-0",
                    "list": list, "notFound": not_found
                })
            }
            // Holds a submission for HOLDFOR seconds, while maxDelayedSend
            // allows it; sends anything else at once.
            "EmailSubmission/set" => {
                let mut submissions = self.submissions.lock().unwrap();
                let mut created = serde_json::Map::new();
                for (create_id, submission) in args["create"].as_object().into_iter().flatten() {
                    let hold_for = submission["envelope"]["mailFrom"]["parameters"]["HOLDFOR"]
                        .as_str()
                        .and_then(|seconds| seconds.parse::<i64>().ok())
                        .filter(|seconds| *seconds <= self.max_delayed_send as i64)
                        .unwrap_or(0);
                    let send_at = chrono::Utc::now() + chrono::Duration::seconds(hold_for);
                    let id = format!("S{}", submissions.len() + 1);
                    let status = if hold_for > 0 { "pending" } else { "final" };
                    submissions.push(json!({
                        "id": id, "emailId": submission["emailId"],
                        "identityId": submission["identityId"],
                        "envelope": submission["envelope"],
                        "undoStatus": status,
                        "sendAt": send_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
                    }));
                    created.insert(create_id.clone(), json!({"id": id}));
                }
                let mut updated = serde_json::Map::new();
                let mut not_updated = serde_json::Map::new();
                for (id, change) in args["update"].as_object().into_iter().flatten() {
                    let Some(submission) = submissions
                        .iter_mut()
                        .find(|submission| &submission["id"] == id)
                    else {
                        not_updated.insert(id.clone(), json!({"type": "notFound"}));
                        continue;
                    };
                    if change["undoStatus"] == "canceled" && submission["undoStatus"] == "pending" {
                        submission["undoStatus"] = json!("canceled");
                        updated.insert(id.clone(), Value::Null);
                    } else {
                        not_updated.insert(id.clone(), json!({"type": "cannotUnsend"}));
                    }
                }
                json!({
                    "accountId": "mail-account", "oldState": "submissions-0",
                    "newState": "submissions-1", "created": created, "updated": updated,
                    "notCreated": {}, "notUpdated": not_updated,
                    "destroyed": [], "notDestroyed": {}
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

/// Sync as a worker that started after all this mail arrived, so none of it
/// notifies.
async fn run_sync(
    ctx: &SyncCtx,
    config: &AccountConfig,
    connected: &ConnectedClient,
) -> Result<()> {
    run_live_sync(ctx, config, connected, i64::MAX).await
}

async fn run_live_sync(
    ctx: &SyncCtx,
    config: &AccountConfig,
    connected: &ConnectedClient,
    live_since: i64,
) -> Result<()> {
    tokio::time::timeout(
        Duration::from_secs(10),
        sync_emails(ctx, config, connected, live_since),
    )
    .await
    .expect("sync timed out")
}

#[tokio::test]
async fn a_blocked_senders_new_mail_goes_to_spam_unannounced() {
    let (_dir, ctx, config) = fixture().await;
    ctx.db
        .write(|conn| {
            conn.execute_batch(
                "INSERT INTO jmap_sync_state(account_id,email_state) VALUES(1,'state-0');
                 INSERT INTO folders (id, account_id, path, role, jmap_id)
                   VALUES (2, 1, 'Junk', 'spam', 'junk-remote');",
            )?;
            repo::blocked::block(conn, "sender@mail.example", 0)?;
            Ok(())
        })
        .await
        .unwrap();
    let now = chrono::Utc::now();
    let mut friend = received(email("friend", "conversation-friend"), now);
    friend["from"] = json!([{"name": "Friend", "email": "friend@mail.example"}]);
    let emails = vec![
        received(email("blocked", "conversation-blocked"), now),
        friend,
    ];
    let changes = json!({"created": ["blocked", "friend"], "updated": [], "destroyed": []});
    let (connected, task) = server(emails, Some(changes), None).await;
    run_live_sync(&ctx, &config, &connected, now.timestamp_millis() - 60_000)
        .await
        .unwrap();
    task.abort();

    ctx.db
        .read(|conn| {
            let placed = |jmap_id: &str| -> rusqlite::Result<(i64, Vec<i64>)> {
                let (id, folder_id): (i64, i64) = conn.query_row(
                    "SELECT id, folder_id FROM messages WHERE jmap_id = ?1",
                    [jmap_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?;
                let mut stmt = conn.prepare(
                    "SELECT folder_id FROM message_folders WHERE message_id = ?1 ORDER BY folder_id",
                )?;
                let mailboxes = stmt
                    .query_map([id], |row| row.get(0))?
                    .collect::<rusqlite::Result<Vec<i64>>>()?;
                Ok((folder_id, mailboxes))
            };
            // Its mailboxes were in place before it moved, so it left the
            // inbox in all of them.
            assert_eq!(placed("blocked")?, (2, vec![2]));
            assert_eq!(placed("friend")?, (1, vec![1]));
            let pending: Vec<String> = conn
                .prepare("SELECT kind FROM pending_actions")?
                .query_map([], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            assert_eq!(pending, ["spam"]);
            let notified: Vec<String> = conn
                .prepare(
                    "SELECT m.jmap_id FROM notification_outbox o
                     JOIN messages m ON m.id = o.message_id",
                )?
                .query_map([], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            assert_eq!(notified, ["friend"]);
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn rules_sort_new_mail_but_not_what_a_first_sync_finds() {
    use crate::models::{MailRule, RuleAction, RuleCondition, RuleField, RuleOp};
    let (_dir, ctx, config) = fixture().await;
    ctx.db
        .write(|conn| {
            conn.execute(
                "INSERT INTO folders (id, account_id, path, role, jmap_id)
                 VALUES (3, 1, 'Lists', NULL, 'lists-remote')",
                [],
            )?;
            repo::rules::save(
                conn,
                &MailRule {
                    id: 0,
                    account_id: Some(1),
                    name: "Lists".into(),
                    enabled: true,
                    match_all: true,
                    conditions: vec![RuleCondition {
                        field: RuleField::From,
                        header: String::new(),
                        op: RuleOp::Is,
                        value: "sender@mail.example".into(),
                        negate: false,
                    }],
                    actions: vec![RuleAction::Move { folder: 3 }],
                    stop: false,
                },
                0,
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let folder_of = |jmap_id: &'static str| {
        let ctx = ctx.clone();
        async move {
            ctx.db
                .read(move |conn| {
                    Ok(repo::messages::by_jmap_id(conn, 1, jmap_id)?
                        .and_then(|message| message.folder_id))
                })
                .await
                .unwrap()
        }
    };
    let (connected, task) = server(vec![email("old", "conversation-old")], None, None).await;
    run_sync(&ctx, &config, &connected).await.unwrap();
    task.abort();
    assert_eq!(folder_of("old").await, Some(1));

    let changes = json!({"created": ["new"], "updated": [], "destroyed": []});
    let (connected, task) = server(
        vec![
            email("old", "conversation-old"),
            email("new", "conversation-new"),
        ],
        Some(changes),
        None,
    )
    .await;
    run_sync(&ctx, &config, &connected).await.unwrap();
    task.abort();
    assert_eq!(folder_of("old").await, Some(1));
    assert_eq!(folder_of("new").await, Some(3));
}

#[tokio::test]
async fn rules_on_the_body_fetch_it_with_the_new_mail() {
    use crate::models::{MailRule, RuleAction, RuleCondition, RuleField, RuleOp};
    let (_dir, ctx, config) = fixture().await;
    ctx.db
        .write(|conn| {
            conn.execute_batch(
                "INSERT INTO jmap_sync_state(account_id,email_state) VALUES(1,'state-0');
                 INSERT INTO folders (id, account_id, path, role, jmap_id)
                   VALUES (3, 1, 'Lists', NULL, 'lists-remote');",
            )?;
            repo::rules::save(
                conn,
                &MailRule {
                    id: 0,
                    account_id: Some(1),
                    name: "Bodies".into(),
                    enabled: true,
                    match_all: true,
                    conditions: vec![RuleCondition {
                        field: RuleField::Body,
                        header: String::new(),
                        op: RuleOp::Contains,
                        // What the stand-in server sends as every body.
                        value: "body".into(),
                        negate: false,
                    }],
                    actions: vec![RuleAction::Move { folder: 3 }],
                    stop: false,
                },
                0,
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let now = chrono::Utc::now();
    let changes = json!({"created": ["new"], "updated": [], "destroyed": []});
    let (connected, task) = server(
        vec![received(email("new", "conversation-new"), now)],
        Some(changes),
        None,
    )
    .await;
    run_live_sync(&ctx, &config, &connected, now.timestamp_millis() - 60_000)
        .await
        .unwrap();
    task.abort();
    ctx.db
        .read(|conn| {
            let message = repo::messages::by_jmap_id(conn, 1, "new")?.unwrap();
            assert_eq!(message.folder_id, Some(3));
            assert_eq!(message.body_state, "cached");
            let notified: i64 =
                conn.query_row("SELECT COUNT(*) FROM notification_outbox", [], |row| {
                    row.get(0)
                })?;
            assert_eq!(notified, 0);
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn live_inbox_arrivals_queue_notifications() {
    let (_dir, ctx, config) = fixture().await;
    ctx.db
        .write(|conn| {
            conn.execute(
                "INSERT INTO jmap_sync_state(account_id,email_state) VALUES(1,'state-0')",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let now = chrono::Utc::now();
    let mut read = received(email("read", "conversation-read"), now);
    read["keywords"] = json!({"$seen": true});
    let mut robot = received(email("robot", "conversation-robot"), now);
    robot["from"] = json!([{"name": "Alerts", "email": "notifications@mail.example"}]);
    let emails = vec![
        received(email("person", "conversation-person"), now),
        read,
        robot,
        received(
            email("caught-up", "conversation-caught-up"),
            now - chrono::Duration::minutes(10),
        ),
    ];
    let changes = json!({
        "created": ["person", "read", "robot", "caught-up"], "updated": [], "destroyed": []
    });
    let (connected, task) = server(emails, Some(changes), None).await;
    let mut events = ctx.bus.subscribe();
    run_live_sync(&ctx, &config, &connected, now.timestamp_millis() - 60_000)
        .await
        .unwrap();
    task.abort();

    // Only mail that arrived after the worker started, unread, is new; mail
    // from a robot notifies without a sound.
    let queued = ctx
        .db
        .read(|conn| {
            let mut stmt = conn.prepare(
                "SELECT m.jmap_id, o.audible FROM notification_outbox o
                 JOIN messages m ON m.id = o.message_id ORDER BY o.id",
            )?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
        .unwrap();
    assert_eq!(
        queued,
        vec![("person".to_owned(), true), ("robot".to_owned(), false)]
    );
    let mut woken = Vec::new();
    while let Ok(event) = events.try_recv() {
        if let CoreEvent::MailNew { thread_ids, .. } = event {
            woken.extend(thread_ids);
        }
    }
    assert_eq!(
        woken.len(),
        2,
        "every queued notification wakes the dispatcher"
    );
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

/// A mailbox with the unread count the server keeps for it.
fn counted(id: &str, name: &str, role: Option<&str>, unread: u64) -> Value {
    let mut mailbox = mailbox(id, name, role);
    mailbox["unreadEmails"] = json!(unread);
    mailbox
}

/// Runs a count sync against a server whose Mailbox/changes answers
/// `changes`, with the account's mailboxes last read at `state`. Returns the
/// ids each Mailbox/get asked for and whether Mailbox/changes was asked.
async fn count_sync(ctx: &SyncCtx, state: &str, changes: Value) -> (Vec<Vec<String>>, bool) {
    let state = state.to_owned();
    ctx.db
        .write(move |conn| {
            conn.execute(
                "INSERT INTO jmap_sync_state(account_id, mailbox_state) VALUES (1, ?1)
                 ON CONFLICT(account_id) DO UPDATE SET mailbox_state = excluded.mailbox_state",
                [state],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let mock = MockJmap {
        mailboxes: vec![
            counted("inbox-remote", "Inbox", Some("inbox"), 4),
            counted("lists-remote", "Lists", None, 7),
        ],
        mailbox_changes: Some(changes),
        ..MockJmap::default()
    }
    .start()
    .await;
    sync_mailbox_counts(ctx, 1, &mock.connected).await.unwrap();
    let gets = mock
        .calls("Mailbox/get")
        .iter()
        .map(|call| {
            assert_eq!(call["properties"], json!(["id", "unreadEmails"]));
            call["ids"]
                .as_array()
                .expect("counts are asked by id")
                .iter()
                .map(|id| id.as_str().unwrap().to_owned())
                .collect()
        })
        .collect();
    let asked_changes = !mock.calls("Mailbox/changes").is_empty();
    mock.task.abort();
    (gets, asked_changes)
}

async fn only_on_server(ctx: &SyncCtx) -> (Option<i64>, Option<i64>) {
    ctx.db
        .read(|conn| {
            Ok((
                repo::folders::server_only_unseen(conn, &[1])?,
                repo::folders::server_only_unseen(conn, &[2])?,
            ))
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn counts_are_asked_only_for_mailboxes_that_changed() {
    let (_dir, ctx, _config) = fixture().await;
    ctx.db
        .write(|conn| {
            conn.execute(
                "INSERT INTO folders (id, account_id, path, jmap_id, backfill_done)
                 VALUES (2, 1, 'Lists', 'lists-remote', 1)",
                [],
            )?;
            // The Inbox keeps one of its four unread messages here.
            crate::db::testutil::seed_message(conn, "sender@mail.example", "Kept", false);
            Ok(())
        })
        .await
        .unwrap();
    let changed = |updated: Value, more: bool| json!({"created": [], "updated": updated, "hasMoreChanges": more});
    let mut events = ctx.bus.subscribe();
    let mut told = move || {
        std::iter::from_fn(|| events.try_recv().ok())
            .filter(|event| matches!(event, CoreEvent::ServerOnlyUnreadChanged { account_id: 1 }))
            .count()
    };

    // First the Inbox alone.
    let (gets, asked) = count_sync(&ctx, "mailboxes-0", changed(json!([]), false)).await;
    assert_eq!(
        (gets, asked),
        (vec![vec!["inbox-remote".to_owned()]], false)
    );
    assert_eq!(only_on_server(&ctx).await, (Some(3), None));
    assert_eq!(told(), 1);
    // Nothing changed since: nothing asked.
    let (gets, asked) = count_sync(&ctx, "mailboxes-0", changed(json!([]), false)).await;
    assert_eq!((gets.len(), asked), (0, false));
    assert_eq!(told(), 0);
    // Then what changed.
    let (gets, asked) =
        count_sync(&ctx, "mailboxes-1", changed(json!(["lists-remote"]), false)).await;
    assert_eq!((gets, asked), (vec![vec!["lists-remote".to_owned()]], true));
    assert_eq!(only_on_server(&ctx).await, (Some(3), Some(7)));

    // While mail read here is on its way, the counts wait.
    let action = ctx
        .db
        .write(|conn| {
            conn.execute(
                "INSERT INTO pending_actions (account_id, kind, created_at) VALUES (1, 'mark_read', 0)",
                [],
            )?;
            Ok(conn.last_insert_rowid())
        })
        .await
        .unwrap();
    let (gets, asked) =
        count_sync(&ctx, "mailboxes-2", changed(json!(["inbox-remote"]), false)).await;
    assert_eq!((gets.len(), asked), (0, false));
    ctx.db
        .write(move |conn| {
            conn.execute("DELETE FROM pending_actions WHERE id = ?1", [action])?;
            Ok(())
        })
        .await
        .unwrap();

    // When the server cannot tell what changed, other counts may be out of
    // date: only the Inbox's is known again.
    let (gets, asked) = count_sync(&ctx, "mailboxes-2", changed(json!([]), true)).await;
    assert_eq!((gets, asked), (vec![vec!["inbox-remote".to_owned()]], true));
    assert_eq!(only_on_server(&ctx).await, (Some(3), None));
    let (gets, asked) = count_sync(
        &ctx,
        "mailboxes-3",
        json!({"error": "cannotCalculateChanges"}),
    )
    .await;
    assert_eq!((gets, asked), (vec![vec!["inbox-remote".to_owned()]], true));
    assert_eq!(only_on_server(&ctx).await, (Some(3), None));
}

/// A JMAP account with Drafts, Scheduled and Sent, on a server that holds
/// submissions up to `max_delayed_send` seconds, and a draft to Alice that
/// is the server's Email `E1`.
async fn holding(max_delayed_send: u64) -> (tempfile::TempDir, SyncCtx, AccountConfig, Mock, i64) {
    let (dir, ctx, config) = fixture().await;
    let mock = MockJmap {
        mailboxes: vec![
            mailbox("inbox-remote", "Inbox", Some("inbox")),
            mailbox("drafts-remote", "Drafts", Some("drafts")),
            mailbox("scheduled-remote", "Scheduled", Some("scheduled")),
            mailbox("sent-remote", "Sent", Some("sent")),
        ],
        max_objects_in_get: 10,
        max_delayed_send,
        ..MockJmap::default()
    }
    .start()
    .await;
    persist_session(&ctx, config.id, &mock.connected)
        .await
        .unwrap();
    sync_mailboxes(&ctx, config.id, &mock.connected)
        .await
        .unwrap();
    let draft = ctx
        .db
        .write(|conn| {
            let drafts = repo::folders::by_jmap_role(conn, 1, roles::DRAFTS)?.unwrap();
            conn.execute(
                "INSERT INTO threads (id, account_id, subject_norm, last_message_at)
                 VALUES (40, 1, 'plans', 1)",
                [],
            )?;
            conn.execute(
                "INSERT INTO messages (thread_id, account_id, folder_id, message_id, subject,
                                       from_addr, to_json, bcc_json, date, is_read, is_draft,
                                       jmap_id)
                 VALUES (40, 1, ?1, 'plans@mail.example', 'Plans', 'reader@mail.example',
                         '[{\"name\":\"Alice\",\"email\":\"alice@mail.example\"}]',
                         '[{\"name\":null,\"email\":\"boss@mail.example\"}]',
                         1, 1, 1, 'E1')",
                [drafts.id],
            )?;
            Ok(conn.last_insert_rowid())
        })
        .await
        .unwrap();
    (dir, ctx, config, mock, draft)
}

async fn scheduled_on_server(ctx: &SyncCtx, draft: i64, send_at: i64) -> i64 {
    ctx.db
        .write(move |conn| {
            repo::actions::enqueue(
                conn,
                1,
                "send",
                Some(draft),
                Some(40),
                &json!({
                    "draftId": draft, "scheduled": true, "sendAt": send_at,
                    "serverHold": true, "jmapPreparedEmailId": "E1"
                }),
                Some(now_ms()),
            )
        })
        .await
        .unwrap()
}

async fn action(ctx: &SyncCtx, id: i64) -> (String, Option<i64>, Value) {
    ctx.db
        .read(move |conn| {
            Ok(conn.query_row(
                "SELECT state, not_before, payload FROM pending_actions WHERE id = ?1",
                [id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        serde_json::from_str(&row.get::<_, String>(2)?).unwrap(),
                    ))
                },
            )?)
        })
        .await
        .unwrap()
}

async fn folder_role_of(ctx: &SyncCtx, message: i64) -> Option<String> {
    ctx.db
        .read(move |conn| {
            Ok(conn.query_row(
                "SELECT f.role FROM messages m JOIN folders f ON f.id = m.folder_id
                 WHERE m.id = ?1",
                [message],
                |row| row.get(0),
            )?)
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn a_server_that_holds_mail_keeps_a_scheduled_message_until_its_time() {
    let (_dir, ctx, config, mock, draft) = holding(30 * 86_400).await;
    // Read from the Session as the server sent it.
    assert_eq!(
        ctx.db
            .read(|conn| Ok(conn.query_row(
                "SELECT jmap_max_delayed_send FROM accounts WHERE id = 1",
                [],
                |row| row.get::<_, i64>(0)
            )?))
            .await
            .unwrap(),
        30 * 86_400
    );
    let send_at = now_ms() + 3_600_000;
    let send = scheduled_on_server(&ctx, draft, send_at).await;
    execute_actions(&ctx, &config, &mock.connected)
        .await
        .unwrap();

    let submitted = mock.calls("EmailSubmission/set");
    assert_eq!(submitted.len(), 1);
    let created = submitted[0]["create"]
        .as_object()
        .unwrap()
        .values()
        .next()
        .unwrap();
    let hold_for: i64 = created["envelope"]["mailFrom"]["parameters"]["HOLDFOR"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((3590..=3600).contains(&hold_for), "{hold_for}");
    assert_eq!(
        created["envelope"]["mailFrom"]["email"],
        "reader@mail.example"
    );
    assert_eq!(
        created["envelope"]["rcptTo"],
        json!([{"email": "alice@mail.example", "parameters": null},
               {"email": "boss@mail.example", "parameters": null}])
    );
    // It waits in Scheduled, still a draft.
    let moved = submitted[0]["onSuccessUpdateEmail"]
        .as_object()
        .unwrap()
        .values()
        .next()
        .unwrap()
        .clone();
    assert_eq!(moved, json!({"mailboxIds": {"scheduled-remote": true}}));
    let (state, not_before, payload) = action(&ctx, send).await;
    assert_eq!(state, "pending");
    assert_eq!(not_before, Some(repo::actions::HELD));
    assert_eq!(
        payload["jmapHeld"],
        json!({"submissionId": "S1", "emailId": "E1"})
    );
    assert_eq!(payload["sendAt"], send_at);
    assert!(payload.get("serverHold").is_none());
    assert_eq!(
        folder_role_of(&ctx, draft).await.as_deref(),
        Some(roles::SCHEDULED)
    );

    // Cancelled: the server stops it, and the message is a draft in Drafts.
    ctx.db
        .write(move |conn| repo::actions::request_unsend(conn, send, now_ms()))
        .await
        .unwrap();
    execute_actions(&ctx, &config, &mock.connected)
        .await
        .unwrap();
    assert_eq!(
        mock.submissions.lock().unwrap()[0]["undoStatus"],
        "canceled"
    );
    let moved_back = mock.calls("Email/set");
    assert_eq!(
        moved_back.last().unwrap()["update"]["E1"]["mailboxIds"],
        json!({"drafts-remote": true})
    );
    assert_eq!(action(&ctx, send).await.0, "cancelled");
    assert_eq!(
        folder_role_of(&ctx, draft).await.as_deref(),
        Some(roles::DRAFTS)
    );
}

#[tokio::test]
async fn a_server_that_holds_mail_for_less_time_leaves_it_to_the_device() {
    let (_dir, ctx, config, mock, draft) = holding(3_600).await;
    let send_at = now_ms() + 2 * 86_400_000;
    let send = scheduled_on_server(&ctx, draft, send_at).await;
    execute_actions(&ctx, &config, &mock.connected)
        .await
        .unwrap();
    assert!(mock.calls("EmailSubmission/set").is_empty());
    let (state, not_before, payload) = action(&ctx, send).await;
    assert_eq!(state, "pending");
    assert_eq!(
        not_before,
        Some(send_at),
        "this device sends it at its time"
    );
    assert!(payload.get("serverHold").is_none());
}

#[tokio::test]
async fn held_mail_follows_the_server_on_every_device() {
    let (_dir, ctx, config, mock, draft) = holding(30 * 86_400).await;
    let send = scheduled_on_server(&ctx, draft, now_ms() + 3_600_000).await;
    execute_actions(&ctx, &config, &mock.connected)
        .await
        .unwrap();

    // Another device or app holds one more, of a message in Scheduled.
    let other = ctx
        .db
        .write(|conn| {
            let scheduled = repo::folders::by_jmap_role(conn, 1, roles::SCHEDULED)?.unwrap();
            conn.execute(
                "INSERT INTO messages (thread_id, account_id, folder_id, message_id, subject,
                                       from_addr, date, is_read, is_draft, jmap_id)
                 VALUES (40, 1, ?1, 'later@mail.example', 'Later', 'reader@mail.example',
                         1, 1, 1, 'E2')",
                [scheduled.id],
            )?;
            Ok(conn.last_insert_rowid())
        })
        .await
        .unwrap();
    let later = (chrono::Utc::now() + chrono::Duration::hours(5))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    mock.submissions.lock().unwrap().push(json!({
        "id": "S9", "emailId": "E2", "identityId": "identity-1",
        "undoStatus": "pending", "sendAt": later
    }));
    super::super::held::reconcile(&ctx, &config, &mock.connected)
        .await
        .unwrap();
    let mirrored = ctx
        .db
        .read(move |conn| repo::actions::held_send_of(conn, other))
        .await
        .unwrap()
        .expect("it waits here too");
    let (state, not_before, payload) = action(&ctx, mirrored).await;
    assert_eq!(state, "pending");
    assert_eq!(not_before, Some(repo::actions::HELD));
    assert_eq!(payload["jmapHeld"]["submissionId"], "S9");
    // Once is enough.
    super::super::held::reconcile(&ctx, &config, &mock.connected)
        .await
        .unwrap();
    assert_eq!(
        ctx.db
            .read(|conn| Ok(repo::actions::held_submissions(conn, 1)?.len()))
            .await
            .unwrap(),
        2
    );

    // The server released the first, and another device cancelled the other.
    mock.submissions.lock().unwrap()[0]["undoStatus"] = json!("final");
    mock.submissions.lock().unwrap()[1]["undoStatus"] = json!("canceled");
    super::super::held::reconcile(&ctx, &config, &mock.connected)
        .await
        .unwrap();
    assert_eq!(action(&ctx, send).await.0, "done");
    assert_eq!(
        folder_role_of(&ctx, draft).await.as_deref(),
        Some(roles::SENT)
    );
    assert_eq!(action(&ctx, mirrored).await.0, "cancelled");
    assert_eq!(
        folder_role_of(&ctx, other).await.as_deref(),
        Some(roles::DRAFTS)
    );
}

#[tokio::test]
async fn a_cancelled_hold_does_not_stop_the_message_going_out() {
    let (_dir, ctx, config, mock, draft) = holding(30 * 86_400).await;
    mock.submissions.lock().unwrap().push(json!({
        "id": "S0", "emailId": "E1", "identityId": "identity-1",
        "undoStatus": "canceled", "sendAt": "2026-11-08T10:00:00Z"
    }));
    // Sent now: the cancelled hold never went out, so this one does.
    let send = ctx
        .db
        .write(move |conn| {
            repo::actions::enqueue(
                conn,
                1,
                "send",
                Some(draft),
                Some(40),
                &json!({"draftId": draft, "jmapPreparedEmailId": "E1"}),
                Some(now_ms()),
            )
        })
        .await
        .unwrap();
    execute_actions(&ctx, &config, &mock.connected)
        .await
        .unwrap();
    assert_eq!(mock.calls("EmailSubmission/set").len(), 1);
    assert_eq!(mock.submissions.lock().unwrap()[1]["undoStatus"], "final");
    assert_eq!(action(&ctx, send).await.0, "done");
    assert_eq!(
        folder_role_of(&ctx, draft).await.as_deref(),
        Some(roles::SENT)
    );
}

/// Inbox mail: a1 unread before the request, a2 already read, a3 unread
/// but newer than it; elsewhere b1 unread.
fn marking_mock() -> MockJmap {
    let mut read = email("a2", "t2");
    read["keywords"] = json!({"$seen": true});
    let mut newer = email("a3", "t3");
    newer["receivedAt"] = json!("2026-10-09T12:00:00Z");
    let mut elsewhere = email("b1", "t4");
    elsewhere["mailboxIds"] = json!({"archive-remote": true});
    MockJmap {
        emails: vec![email("a1", "t1"), read, newer, elsewhere],
        ..MockJmap::default()
    }
}

#[tokio::test]
async fn marking_a_mailbox_read_marks_its_unread_mail_from_before_the_request() {
    let mock = marking_mock().start().await;
    let before = chrono::DateTime::parse_from_rfc3339("2026-10-09T00:00:00Z")
        .unwrap()
        .timestamp_millis();
    mark_mailbox_read(&mock.connected, "inbox-remote", before)
        .await
        .unwrap();
    let seen = mock.calls("Email/set");
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(seen[0]["update"], json!({"a1": {"keywords/$seen": true}}));
    // It asked again until nothing was left.
    assert_eq!(mock.calls("Email/query").len(), 2);
    mock.task.abort();
}

#[tokio::test]
async fn a_server_that_keeps_mail_unread_stops_the_marking() {
    let mock = MockJmap {
        forget_seen: true,
        ..marking_mock()
    }
    .start()
    .await;
    let before = chrono::DateTime::parse_from_rfc3339("2026-10-09T00:00:00Z")
        .unwrap()
        .timestamp_millis();
    let result = mark_mailbox_read(&mock.connected, "inbox-remote", before).await;
    assert!(matches!(result, Err(CoreError::Jmap(_))), "{result:?}");
    assert_eq!(mock.calls("Email/set").len(), 1);
    mock.task.abort();
}

/// A server search keeps what it finds and goes on where it stopped. A page
/// brings mail not kept here, with the matches kept here that it passes.
#[tokio::test]
async fn a_server_search_keeps_what_it_finds_and_goes_on_where_it_stopped() {
    use crate::models::{MailRule, RuleAction, RuleCondition, RuleField, RuleOp};
    use chrono::TimeZone;
    let (_dir, ctx, config) = fixture().await;
    // A rule that would mark it read: found mail isn't new mail.
    ctx.db
        .write(|conn| {
            repo::rules::save(
                conn,
                &MailRule {
                    id: 0,
                    account_id: None,
                    name: "Sender".into(),
                    enabled: true,
                    match_all: true,
                    conditions: vec![RuleCondition {
                        field: RuleField::From,
                        header: String::new(),
                        op: RuleOp::Is,
                        value: "sender@mail.example".into(),
                        negate: false,
                    }],
                    actions: vec![RuleAction::MarkRead],
                    stop: false,
                },
                0,
            )
        })
        .await
        .unwrap();
    let at = |day| chrono::Utc.with_ymd_and_hms(2020, 3, day, 9, 0, 0).unwrap();
    let emails = vec![
        received(email("e3", "t3"), at(3)),
        received(email("e2", "t2"), at(2)),
        received(email("e1", "t1"), at(1)),
    ];
    let mock = MockJmap {
        emails,
        max_objects_in_get: 10,
        ..MockJmap::default()
    }
    .start()
    .await;
    let mut session = Some(mock.connected);
    let tree = crate::search::parse("invoice from:sender").tree.unwrap();
    let first = search_server(&ctx, &config, &mut session, &tree, &Default::default(), 2)
        .await
        .unwrap();
    assert_eq!(first.message_ids.len(), 2);
    let next = first
        .next
        .clone()
        .expect("a page that came full may have more");
    assert_eq!(next.position, 2);

    // The server got the query newest first, and the fixture's one folder,
    // the inbox, leaves nothing to keep out.
    let calls = mock.calls.lock().unwrap().clone();
    let query = &calls
        .iter()
        .find(|(method, _)| method == "Email/query")
        .unwrap()
        .1;
    assert_eq!(
        query["filter"],
        json!({"operator": "AND", "conditions": [{"text": "invoice"}, {"from": "sender"}]})
    );
    assert_eq!(
        query["sort"],
        json!([{"property": "receivedAt", "isAscending": false}])
    );
    assert_eq!(
        (query["position"].clone(), query["limit"].clone()),
        (json!(0), json!(2))
    );

    let second = search_server(&ctx, &config, &mut session, &tree, &next, 2)
        .await
        .unwrap();
    assert_eq!(second.message_ids.len(), 1);
    assert!(
        second.next.is_none(),
        "a page short of full found all there is"
    );

    // Each found email is kept once, newest first as the server gave them.
    let kept = ctx
        .db
        .read(|conn| {
            let mut stmt = conn.prepare("SELECT jmap_id FROM messages ORDER BY date DESC")?;
            Ok(stmt
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
        .unwrap();
    assert_eq!(kept, ["e3", "e2", "e1"]);
    let read = ctx
        .db
        .read(|conn| {
            Ok(conn.query_row(
                "SELECT COUNT(*) FROM messages WHERE is_read = 1",
                [],
                |row| row.get::<_, i64>(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(read, 0);

    // Searched again, the first page has nothing new, so the search goes on
    // to the end of the results and brings what it passed.
    let again = search_server(&ctx, &config, &mut session, &tree, &Default::default(), 2)
        .await
        .unwrap();
    let mut found = [first.message_ids, second.message_ids].concat();
    found.sort_unstable();
    let mut found_again = again.message_ids;
    found_again.sort_unstable();
    assert_eq!(found_again, found);
    assert!(again.next.is_none());
    let positions = mock
        .calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(method, _)| method == "Email/query")
        .map(|(_, query)| query["position"].clone())
        .collect::<Vec<_>>();
    assert_eq!(positions, [json!(0), json!(2), json!(0), json!(2)]);
}

/// `email` with a plain body, and an HTML one when given, as a body download
/// asks for them. Without HTML, the plain part stands for it too, as RFC 8621
/// section 4.1.4 has it.
fn with_bodies(mut email: Value, plain: &str, html: Option<&str>) -> Value {
    let text_part =
        json!({"partId": "10", "blobId": "text-blob", "size": plain.len(), "type": "text/plain"});
    email["textBody"] = json!([text_part]);
    email["bodyValues"] = json!({
        "10": {"value": plain, "isEncodingProblem": false, "isTruncated": false}
    });
    match html {
        Some(html) => {
            email["htmlBody"] = json!([
                {"partId": "11", "blobId": "html-blob", "size": html.len(), "type": "text/html"}
            ]);
            email["bodyValues"]["11"] =
                json!({"value": html, "isEncodingProblem": false, "isTruncated": false});
        }
        None => email["htmlBody"] = json!([text_part]),
    }
    email
}

/// One body download request with the full byte budget.
async fn download_once(ctx: &SyncCtx, config: &AccountConfig, c: &ConnectedClient) -> bool {
    let mut budget = BODY_DOWNLOAD_BYTES;
    download_bodies(ctx, config, c, &mut budget).await.unwrap()
}

/// What each message holds of its body: its remote id, body state, text,
/// HTML and raw file.
type StoredBody = (
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
);

async fn stored_bodies(ctx: &SyncCtx) -> Vec<StoredBody> {
    ctx.db
        .read(|conn| {
            let mut stmt = conn.prepare(
                "SELECT m.jmap_id, m.body_state, b.text_body, b.html_body, m.raw_path
                 FROM messages m LEFT JOIN message_bodies b ON b.message_id = m.id
                 WHERE m.jmap_id IS NOT NULL ORDER BY m.jmap_id",
            )?;
            Ok(stmt
                .query_map([], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
        .unwrap()
}

/// A message's id, body state, unsubscribe link and paperclip.
struct StoredMessage {
    id: i64,
    body_state: String,
    list_unsubscribe: Option<String>,
    has_attachments: bool,
}

async fn message_with(ctx: &SyncCtx, remote_id: &'static str) -> StoredMessage {
    ctx.db
        .read(move |conn| {
            Ok(conn.query_row(
                "SELECT id, body_state, list_unsubscribe, has_attachments FROM messages
                 WHERE jmap_id = ?1",
                [remote_id],
                |row| {
                    Ok(StoredMessage {
                        id: row.get(0)?,
                        body_state: row.get(1)?,
                        list_unsubscribe: row.get(2)?,
                        has_attachments: row.get(3)?,
                    })
                },
            )?)
        })
        .await
        .unwrap()
}

/// An account downloads the text of its messages ahead of time, without
/// their attachments, with the header fields the whole message would give
/// (#253).
#[tokio::test]
async fn bodies_download_ahead_of_time_without_their_attachments() {
    let (_dir, ctx, config) = fixture().await;
    assert_eq!(config.settings.download, DownloadPolicy::Messages);
    let mut letter = with_bodies(
        with_attachment(email("email-a", "thread-a"), 100, "2026-10-08T12:00:01Z"),
        "Plain words",
        Some("<p>HTML words</p><script>alert(1)</script>"),
    );
    letter["header:List-Unsubscribe:all"] = json!([" <https://news.example/unsubscribe>"]);
    letter["header:List-Unsubscribe-Post:all"] = json!([" List-Unsubscribe=One-Click"]);
    let note = with_bodies(email("email-b", "thread-b"), "Just a note", None);
    let mock = MockJmap {
        emails: vec![letter, note],
        max_objects_in_get: 500,
        ..MockJmap::default()
    }
    .start()
    .await;
    run_sync(&ctx, &config, &mock.connected).await.unwrap();
    let headers = mock.calls("Email/get").len();

    assert!(download_once(&ctx, &config, &mock.connected).await);
    let downloads = mock.calls("Email/get").split_off(headers);
    assert_eq!(downloads.len(), 1);
    let request = &downloads[0];
    assert_eq!(listed_ids(request), ["email-a", "email-b"]);
    for property in ["bodyValues", "textBody", "htmlBody", "attachments"] {
        assert!(
            request["properties"]
                .as_array()
                .unwrap()
                .contains(&json!(property)),
            "{request}"
        );
    }
    assert!(
        request["properties"]
            .as_array()
            .unwrap()
            .contains(&json!("header:Authentication-Results:all"))
    );
    assert_eq!(request["fetchTextBodyValues"], true);
    assert_eq!(request["fetchHTMLBodyValues"], true);
    assert_eq!(request["maxBodyValueBytes"], MAX_BODY_VALUE_BYTES);

    let stored = stored_bodies(&ctx).await;
    let (_, state, text, html, raw) = &stored[0];
    assert_eq!(state, "cached");
    assert_eq!(text.as_deref(), Some("Plain words"));
    let html = html.as_deref().unwrap();
    assert!(
        html.contains("HTML words") && !html.contains("script"),
        "{html}"
    );
    assert_eq!(raw, &None);
    assert_eq!(
        stored[1],
        (
            "email-b".to_owned(),
            "cached".to_owned(),
            Some("Just a note".to_owned()),
            None,
            None
        )
    );
    // The attachment is listed, and waits to be opened.
    assert_eq!(
        attachment_rows(&ctx).await,
        [(
            "email-a".to_owned(),
            "2".to_owned(),
            "email-a.pdf".to_owned(),
            "part-email-a".to_owned()
        )]
    );
    assert!(mock.downloads.lock().unwrap().is_empty());
    let letter = message_with(&ctx, "email-a").await;
    assert_eq!(
        letter.list_unsubscribe.as_deref(),
        Some("<https://news.example/unsubscribe>")
    );
    assert!(letter.has_attachments);

    let calls = mock.calls("Email/get").len();
    assert!(!download_once(&ctx, &config, &mock.connected).await);
    assert!(!list_once(&ctx, &config, &mock.connected).await);
    assert_eq!(mock.calls("Email/get").len(), calls);
    mock.task.abort();
}

/// An account set to download headers alone leaves each body on the server
/// until it's opened.
#[tokio::test]
async fn an_account_set_to_headers_only_downloads_no_bodies() {
    let (_dir, ctx, mut config) = fixture().await;
    config.settings.download = DownloadPolicy::Headers;
    let mock = MockJmap {
        emails: vec![with_bodies(email("email-a", "thread-a"), "Words", None)],
        ..MockJmap::default()
    }
    .start()
    .await;
    run_sync(&ctx, &config, &mock.connected).await.unwrap();
    let calls = mock.calls("Email/get").len();
    assert!(!download_once(&ctx, &config, &mock.connected).await);
    assert_eq!(mock.calls("Email/get").len(), calls);
    assert_eq!(stored_bodies(&ctx).await[0].1, "none");
    mock.task.abort();
}

/// A message whose text the server cut short, a small read receipt, whose
/// report is no text, and a message carrying one its body shows (#455)
/// download whole, as when they're opened. One carrying a message sent as an
/// attachment keeps the text the server read out.
#[tokio::test]
async fn messages_with_more_than_their_text_download_whole() {
    let (_dir, ctx, config) = fixture().await;
    let mut long = with_bodies(email("email-a", "thread-a"), "A long text", None);
    long["receivedAt"] = json!("2026-10-08T12:00:02Z");
    long["bodyValues"]["10"]["isTruncated"] = json!(true);
    let mut receipt = with_bodies(email("email-b", "thread-b"), "It was read", None);
    receipt["receivedAt"] = json!("2026-10-08T12:00:01Z");
    receipt["attachments"] = json!([{
        "partId": "2", "blobId": "report-b", "size": 200,
        "type": "message/disposition-notification"
    }]);
    let forward = |id: &str, disposition: &str, received_at: &str| {
        let mut forward = with_bodies(email(id, &format!("thread-{id}")), "See below", None);
        forward["receivedAt"] = json!(received_at);
        forward["attachments"] = json!([{
            "partId": "2", "blobId": format!("forwarded-{id}"), "size": 300,
            "type": "message/rfc822", "disposition": disposition
        }]);
        forward
    };
    let mock = MockJmap {
        emails: vec![
            long,
            receipt,
            forward("email-c", "inline", "2026-10-08T12:00:00Z"),
            forward("email-d", "attachment", "2026-10-08T11:59:59Z"),
        ],
        max_objects_in_get: 500,
        ..MockJmap::default()
    }
    .start()
    .await;
    run_sync(&ctx, &config, &mock.connected).await.unwrap();
    while download_once(&ctx, &config, &mock.connected).await {}
    assert_eq!(
        *mock.downloads.lock().unwrap(),
        ["blob-email-a", "blob-email-b", "blob-email-c"]
    );
    for (remote_id, state, _, _, raw) in stored_bodies(&ctx).await {
        assert_eq!(state, "cached", "{remote_id}");
        assert_eq!(raw.is_some(), remote_id != "email-d", "{remote_id}");
    }
    mock.task.abort();
}

/// A message the server no longer sends as it is here waits as a body that
/// failed does, while the others come.
#[tokio::test]
async fn bodies_the_server_no_longer_sends_wait() {
    let (_dir, ctx, config) = fixture().await;
    ctx.db
        .write(|conn| {
            for (remote_id, blob) in [
                ("email-a", "blob-email-a"),
                ("email-gone", "blob-email-gone"),
                ("email-c", "blob-stale"),
            ] {
                let (_, message) = crate::db::testutil::seed_message(
                    conn,
                    "sender@mail.example",
                    remote_id,
                    false,
                );
                repo::messages::set_jmap_remote(conn, message, remote_id, Some(blob))?;
            }
            Ok(())
        })
        .await
        .unwrap();
    let mock = MockJmap {
        emails: vec![
            with_bodies(email("email-a", "thread-a"), "Here", None),
            with_bodies(email("email-c", "thread-c"), "Changed", None),
        ],
        max_objects_in_get: 500,
        ..MockJmap::default()
    }
    .start()
    .await;
    assert!(download_once(&ctx, &config, &mock.connected).await);
    let states = stored_bodies(&ctx)
        .await
        .into_iter()
        .map(|(remote_id, state, ..)| (remote_id, state))
        .collect::<Vec<_>>();
    assert_eq!(
        states,
        [
            ("email-a".to_owned(), "cached".to_owned()),
            ("email-c".to_owned(), "none".to_owned()),
            ("email-gone".to_owned(), "none".to_owned()),
        ]
    );
    let calls = mock.calls("Email/get").len();
    assert!(!download_once(&ctx, &config, &mock.connected).await);
    assert_eq!(mock.calls("Email/get").len(), calls);
    mock.task.abort();
}

const INVITATION: &str = concat!(
    "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example//EN\r\nMETHOD:REQUEST\r\n",
    "BEGIN:VEVENT\r\nUID:planning@mail.example\r\nDTSTAMP:20261008T120000Z\r\n",
    "DTSTART:20261010T090000Z\r\nDTEND:20261010T100000Z\r\nSUMMARY:Planning\r\n",
    "ORGANIZER:mailto:sender@mail.example\r\nATTENDEE:mailto:reader@mail.example\r\n",
    "END:VEVENT\r\nEND:VCALENDAR\r\n",
);

/// An invitation's calendar downloads as its own blob, for the card above
/// the message.
#[tokio::test]
async fn an_invitation_downloads_its_calendar() {
    let (_dir, ctx, config) = fixture().await;
    let mut invitation = with_bodies(email("email-a", "thread-a"), "Join us", None);
    invitation["attachments"] = json!([{
        "partId": "3", "blobId": "calendar-a", "size": INVITATION.len(),
        "type": "text/calendar", "name": "invite.ics"
    }]);
    let mock = MockJmap {
        emails: vec![invitation],
        blobs: vec![("calendar-a", INVITATION)],
        ..MockJmap::default()
    }
    .start()
    .await;
    run_sync(&ctx, &config, &mock.connected).await.unwrap();
    assert!(download_once(&ctx, &config, &mock.connected).await);
    assert_eq!(*mock.downloads.lock().unwrap(), ["calendar-a"]);
    let message = message_with(&ctx, "email-a").await;
    assert_eq!(message.body_state, "cached");
    let calendar = ctx
        .calendar_db
        .read(move |conn| repo::calendar::message_calendar(conn, message.id))
        .await
        .unwrap();
    assert_eq!(calendar.as_deref(), Some(INVITATION));
    mock.task.abort();
}

#[tokio::test]
async fn body_downloads_bound_the_message_bytes_of_each_request() {
    let (_dir, ctx, config) = fixture().await;
    let mib = 1024 * 1024;
    let message = |id: &str, size: i64, received_at: &str| {
        let mut email = with_bodies(email(id, &format!("thread-{id}")), "Words", None);
        email["size"] = json!(size);
        email["receivedAt"] = json!(received_at);
        email
    };
    let mock = MockJmap {
        emails: vec![
            message("email-a", 20 * mib, "2026-10-08T12:00:05Z"),
            message("email-b", 20 * mib, "2026-10-08T12:00:04Z"),
            message("email-c", 40 * mib, "2026-10-08T12:00:03Z"),
            message("email-d", 1024, "2026-10-08T12:00:02Z"),
            message("email-e", 1024, "2026-10-08T12:00:01Z"),
        ],
        max_objects_in_get: 500,
        ..MockJmap::default()
    }
    .start()
    .await;
    run_sync(&ctx, &config, &mock.connected).await.unwrap();
    let headers = mock.calls("Email/get").len();
    while download_once(&ctx, &config, &mock.connected).await {}
    let downloads = mock.calls("Email/get").split_off(headers);
    // Newest first; a message larger than the budget goes alone.
    assert_eq!(
        downloads.iter().map(listed_ids).collect::<Vec<_>>(),
        [
            vec!["email-a"],
            vec!["email-b"],
            vec!["email-c"],
            vec!["email-d", "email-e"],
        ]
    );
    assert!(
        stored_bodies(&ctx)
            .await
            .iter()
            .all(|(_, state, ..)| state == "cached")
    );
    mock.task.abort();
}

/// An embedded image without a disposition shows in its message, as a MIME
/// part read here does, and a file stays a file with a Content-ID too. Parts
/// listed under the older rule take this one when they're listed again.
#[tokio::test]
async fn embedded_parts_without_a_disposition_show_in_their_message() {
    let (_dir, ctx, config) = fixture().await;
    let mut letter = with_attachment(email("email-a", "thread-a"), 100, "2026-10-08T12:00:00Z");
    letter["attachments"] = json!([
        {
            "partId": "2", "blobId": "logo-a", "size": 10, "type": "image/png",
            "cid": "logo@mail.example"
        },
        {
            "partId": "3", "blobId": "file-a", "size": 10, "type": "application/pdf",
            "name": "a.pdf", "disposition": "attachment", "cid": "file@mail.example"
        }
    ]);
    let mock = MockJmap {
        emails: vec![letter],
        ..MockJmap::default()
    }
    .start()
    .await;
    run_sync(&ctx, &config, &mock.connected).await.unwrap();
    let inline = || async {
        ctx.db
            .read(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT jmap_blob_id, is_inline FROM attachments ORDER BY jmap_blob_id",
                )?;
                Ok(stmt
                    .query_map([], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?)
            })
            .await
            .unwrap()
    };
    let expected = [("file-a".to_owned(), false), ("logo-a".to_owned(), true)];
    assert!(list_once(&ctx, &config, &mock.connected).await);
    assert_eq!(inline().await, expected);

    ctx.db
        .write(|conn| {
            conn.execute(
                "UPDATE attachments SET is_inline = 0 WHERE jmap_blob_id = 'logo-a'",
                [],
            )?;
            conn.execute("UPDATE messages SET jmap_attachments_blob_id = NULL", [])?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(list_once(&ctx, &config, &mock.connected).await);
    assert_eq!(inline().await, expected);
    mock.task.abort();
}
