use super::*;
use crate::accounts::credentials::DevelopmentFileCredentialStore;
use crate::config::Paths;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

/// A request the stand-in server answered: method, target and body.
pub(super) type Request = (String, String, String);

/// A Gmail or Microsoft Graph stand-in that answers each request with
/// `answer`'s status and JSON, and keeps the requests.
pub(super) async fn server(
    answer: impl Fn(&Request) -> (u16, Value) + Send + Sync + 'static,
) -> (String, Arc<Mutex<Vec<Request>>>) {
    raw_server(move |request| {
        let (status, json) = answer(request);
        (status, "application/json", json.to_string())
    })
    .await
}

/// A stand-in that answers each request with `answer`'s status, content
/// type and body, and keeps the requests.
async fn raw_server(
    answer: impl Fn(&Request) -> (u16, &'static str, String) + Send + Sync + 'static,
) -> (String, Arc<Mutex<Vec<Request>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let kept = Arc::clone(&requests);
    let answer = Arc::new(answer);
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let (requests, answer) = (Arc::clone(&kept), Arc::clone(&answer));
            tokio::spawn(async move {
                let mut stream = BufReader::new(stream);
                let mut line = String::new();
                stream.read_line(&mut line).await.unwrap();
                let mut parts = line.split_whitespace();
                let method = parts.next().unwrap_or_default().to_owned();
                let target = parts.next().unwrap_or_default().to_owned();
                let mut length = 0;
                loop {
                    let mut header = String::new();
                    stream.read_line(&mut header).await.unwrap();
                    if header.trim().is_empty() {
                        break;
                    }
                    if let Some((name, value)) = header.split_once(':')
                        && name.eq_ignore_ascii_case("content-length")
                    {
                        length = value.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0; length];
                stream.read_exact(&mut body).await.unwrap();
                let request = (method, target, String::from_utf8(body).unwrap());
                let (status, content_type, body) = answer(&request);
                requests.lock().unwrap().push(request);
                let response = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: {content_type}\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream
                    .get_mut()
                    .write_all(response.as_bytes())
                    .await
                    .unwrap();
            });
        }
    });
    (origin, requests)
}

/// A core with a Gmail account, id 1, signed in.
async fn gmail_core(directory: &tempfile::TempDir) -> Core {
    let credentials = Arc::new(DevelopmentFileCredentialStore::new(
        directory.path().join("credentials.json"),
    ));
    let core =
        Core::start_mail_ui_with_credentials(Paths::for_tests(directory.path()), credentials)
            .await
            .unwrap();
    core.db
        .write(|conn| {
            crate::db::testutil::seed_account(conn);
            conn.execute(
                "UPDATE accounts SET provider = 'gmail', mail_protocol = 'gmail',
                                     auth_kind = 'oauth2' WHERE id = 1",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    core.sync_ctx()
        .tokens
        .store_initial(
            1,
            "access".into(),
            Some(3600),
            Some("refresh".into()),
            "client".into(),
            None,
        )
        .await
        .unwrap();
    core
}

fn away() -> AutoReply {
    AutoReply {
        enabled: true,
        subject: "Out of office".into(),
        text: "I'm away until Monday.\n\nAlex".into(),
        starts_at: Some(1_763_251_200_000),
        ends_at: Some(1_763_683_199_999),
        contacts_only: true,
        domain_only: false,
    }
}

#[tokio::test]
async fn replies_go_to_gmail_as_its_vacation_responder() {
    let directory = tempfile::tempdir().unwrap();
    let core = gmail_core(&directory).await;
    let (origin, requests) = server(|(method, target, body)| {
        assert_eq!(method, "PUT");
        assert_eq!(target, "/gmail/v1/users/me/settings/vacation");
        // Gmail answers with what it keeps, which here is what came.
        (200, serde_json::from_str(body).unwrap())
    })
    .await;
    let config = core
        .db
        .read(|conn| repo::accounts::get_config(conn, 1))
        .await
        .unwrap()
        .unwrap();
    let responder =
        sync::gmail::VacationResponder::with_base(core.sync_ctx(), config, &origin).unwrap();

    assert_eq!(responder.save(&away()).await.unwrap(), away());
    let requests = requests.lock().unwrap().clone();
    let sent: Value = serde_json::from_str(&requests[0].2).unwrap();
    assert_eq!(
        sent,
        json!({
            "enableAutoReply": true,
            "responseSubject": "Out of office",
            "responseBodyPlainText": "I'm away until Monday.\n\nAlex",
            "restrictToContacts": true,
            "restrictToDomain": false,
            // Google takes 64-bit numbers as strings.
            "startTime": "1763251200000",
            "endTime": "1763683199999",
        })
    );
}

#[tokio::test]
async fn a_sign_in_keeps_the_settings_permission_once_replies_were_saved() {
    let directory = tempfile::tempdir().unwrap();
    let core = gmail_core(&directory).await;
    let scopes = core.google_scopes_in_use(1).await.unwrap();
    assert!(!scopes.contains(&oauth::providers::GMAIL_SETTINGS_SCOPE));

    // Read back from Gmail, the replies use no settings permission yet.
    core.db
        .write(|conn| {
            let read = repo::auto_replies::Origin::Read { saved_at: None };
            repo::auto_replies::store(conn, 1, &away(), 1, read)
        })
        .await
        .unwrap();
    let scopes = core.google_scopes_in_use(1).await.unwrap();
    assert!(!scopes.contains(&oauth::providers::GMAIL_SETTINGS_SCOPE));

    // Saved by the app, they do, and a new sign-in asks for it again.
    core.db
        .write(|conn| {
            repo::auto_replies::store(conn, 1, &away(), 2, repo::auto_replies::Origin::Saved)
        })
        .await
        .unwrap();
    let scopes = core.google_scopes_in_use(1).await.unwrap();
    assert!(scopes.contains(&oauth::providers::GMAIL_SETTINGS_SCOPE));
    assert_eq!(core.auto_reply(1).await.unwrap(), Some(away()));
    assert_eq!(core.auto_replies().await.unwrap(), [(1, away())]);
}

#[test]
fn replies_are_on_until_their_last_day_ends() {
    let reply = away();
    assert!(reply.on_at(0), "on, waiting for the first day");
    assert!(reply.on_at(1_763_683_199_998));
    assert!(!reply.on_at(1_763_683_199_999));
    let open_ended = AutoReply {
        ends_at: None,
        ..away()
    };
    assert!(open_ended.on_at(i64::MAX));
    let off = AutoReply {
        enabled: false,
        ..away()
    };
    assert!(!off.on_at(0));
}

/// A JMAP session for account "a1" at `origin`, with JMAP Sieve or not.
fn jmap_session(origin: &str, sieve: bool) -> Value {
    let mail = json!({
        "maxMailboxesPerEmail": null,
        "maxMailboxDepth": 10,
        "maxSizeMailboxName": 255,
        "maxSizeAttachmentsPerEmail": 50_000_000,
        "emailQuerySortOptions": ["receivedAt"],
        "mayCreateTopLevelMailbox": true
    });
    let submission = json!({ "maxDelayedSend": 0, "submissionExtensions": [] });
    let mut account = json!({
        "urn:ietf:params:jmap:mail": mail.clone(),
        "urn:ietf:params:jmap:submission": submission.clone(),
        "urn:ietf:params:jmap:vacationresponse": {},
    });
    if sieve {
        account["urn:ietf:params:jmap:sieve"] = json!({});
    }
    json!({
        "capabilities": {
            "urn:ietf:params:jmap:core": {
                "maxSizeUpload": 50_000_000,
                "maxConcurrentUpload": 4,
                "maxSizeRequest": 10_000_000,
                "maxConcurrentRequests": 4,
                "maxCallsInRequest": 16,
                "maxObjectsInGet": 500,
                "maxObjectsInSet": 500,
                "collationAlgorithms": []
            },
            "urn:ietf:params:jmap:mail": mail,
            "urn:ietf:params:jmap:submission": submission,
            "urn:ietf:params:jmap:vacationresponse": {},
        },
        "accounts": {
            "a1": {
                "name": "Test",
                "isPersonal": true,
                "isReadOnly": false,
                "accountCapabilities": account,
            }
        },
        "primaryAccounts": { "urn:ietf:params:jmap:mail": "a1" },
        "username": "me@example.test",
        "apiUrl": format!("{origin}/api"),
        "downloadUrl": format!("{origin}/download/{{accountId}}/{{blobId}}/{{name}}?accept={{type}}"),
        "uploadUrl": format!("{origin}/upload/{{accountId}}"),
        "eventSourceUrl": format!("{origin}/events?types={{types}}&closeafter={{closeafter}}&ping={{ping}}"),
        "state": "session-1"
    })
}

/// A JMAP server whose VacationResponse starts as `stored`, with JMAP Sieve
/// or not.
async fn jmap_server(sieve: bool) -> (String, Arc<Mutex<Value>>) {
    let stored = Arc::new(Mutex::new(json!({ "id": "singleton", "isEnabled": false })));
    let kept = Arc::clone(&stored);
    let origin = Arc::new(Mutex::new(String::new()));
    let known = Arc::clone(&origin);
    let (address, _) = server(move |(method, _, body)| {
        if method == "GET" {
            return (200, jmap_session(&known.lock().unwrap(), sieve));
        }
        let request: Value = serde_json::from_str(body).unwrap();
        let call = &request["methodCalls"][0];
        let response = match call[0].as_str().unwrap() {
            "VacationResponse/get" => json!([
                "VacationResponse/get",
                { "accountId": "a1", "state": "1", "list": [kept.lock().unwrap().clone()], "notFound": [] },
                call[2],
            ]),
            "VacationResponse/set" => {
                let mut object = kept.lock().unwrap();
                for (name, value) in call[1]["update"]["singleton"].as_object().unwrap() {
                    object[name] = value.clone();
                }
                json!([
                    "VacationResponse/set",
                    { "accountId": "a1", "oldState": "1", "newState": "2", "updated": { "singleton": null } },
                    call[2],
                ])
            }
            other => panic!("unexpected {other}"),
        };
        (200, json!({ "methodResponses": [response], "sessionState": "session-1" }))
    })
    .await;
    *origin.lock().unwrap() = address.clone();
    (address, stored)
}

#[tokio::test]
async fn replies_go_to_a_jmap_server_as_its_vacation_response() {
    let (origin, stored) = jmap_server(false).await;
    let connected = crate::jmap::client::connect_with(
        "me@example.test",
        "me@example.test",
        "secret",
        &origin,
        Some("a1"),
    )
    .await
    .unwrap();
    assert!(crate::jmap::vacation::offered(&connected));
    assert!(
        !crate::jmap::vacation::read(&connected)
            .await
            .unwrap()
            .enabled
    );

    let reply = AutoReply {
        contacts_only: false,
        ..away()
    };
    let saved = crate::jmap::vacation::save(&connected, &reply)
        .await
        .unwrap();
    // JMAP keeps whole seconds, so the end comes back to the second.
    assert_eq!(
        saved.ends_at,
        reply.ends_at.map(|at| (at + 1) / 1000 * 1000 - 1)
    );
    assert_eq!(
        AutoReply {
            ends_at: reply.ends_at,
            ..saved
        },
        reply
    );
    let object = stored.lock().unwrap().clone();
    assert_eq!(object["isEnabled"], true);
    assert_eq!(object["textBody"], reply.text);
    // The end is the first moment without replies: the next day's start.
    assert_eq!(object["toDate"], "2025-11-21T00:00:00Z");
}

#[tokio::test]
async fn a_jmap_server_with_sieve_keeps_its_replies_to_itself() {
    let (origin, _) = jmap_server(true).await;
    let connected = crate::jmap::client::connect_with(
        "me@example.test",
        "me@example.test",
        "secret",
        &origin,
        Some("a1"),
    )
    .await
    .unwrap();
    // Saving there would turn the user's own filters off.
    assert!(!crate::jmap::vacation::offered(&connected));
}

/// What a JMAP server with Sieve keeps: its scripts as id, name, blob id
/// and whether it's the active one, and the blobs.
#[derive(Default)]
struct SieveAccount {
    scripts: Vec<(String, String, String, bool)>,
    blobs: std::collections::HashMap<String, String>,
    next: usize,
}

impl SieveAccount {
    fn put(&mut self, name: &str, text: &str, active: bool) {
        self.next += 1;
        let (id, blob) = (format!("s{}", self.next), format!("b{}", self.next));
        self.blobs.insert(blob.clone(), text.into());
        if active {
            for script in &mut self.scripts {
                script.3 = false;
            }
        }
        self.scripts.push((id, name.into(), blob, active));
    }

    fn active(&self) -> Option<&str> {
        self.scripts
            .iter()
            .find(|script| script.3)
            .map(|script| script.1.as_str())
    }

    fn text(&self, name: &str) -> Option<&str> {
        let (_, _, blob, _) = self.scripts.iter().find(|script| script.1 == name)?;
        self.blobs.get(blob).map(String::as_str)
    }
}

/// The session of a JMAP server with Sieve that offers `extensions`, listed
/// with the account as RFC 9661 says, or with the session as earlier drafts
/// did.
fn jmap_sieve_session(origin: &str, extensions: &[&str], draft: bool) -> Value {
    let mut session = jmap_session(origin, false);
    let limits = json!({
        "maxSizeScriptName": 512,
        "maxSizeScript": 1_048_576,
        "maxNumberScripts": 100,
        "maxNumberRedirects": 10,
        "sieveExtensions": extensions,
        "notificationMethods": null,
        "externalLists": null,
    });
    let (in_session, in_account) = if draft {
        (limits, json!({}))
    } else {
        (json!({ "implementation": "Test Sieve" }), limits)
    };
    session["capabilities"]["urn:ietf:params:jmap:sieve"] = in_session;
    session["accounts"]["a1"]["accountCapabilities"]["urn:ietf:params:jmap:sieve"] = in_account;
    session
}

/// A JMAP server with Sieve (RFC 9661) that keeps scripts as a real one
/// does.
async fn jmap_sieve_server(
    extensions: &'static [&'static str],
    draft: bool,
) -> (String, Arc<Mutex<SieveAccount>>) {
    let account = Arc::new(Mutex::new(SieveAccount::default()));
    let kept = Arc::clone(&account);
    let origin = Arc::new(Mutex::new(String::new()));
    let known = Arc::clone(&origin);
    let (address, _) = raw_server(move |(method, target, body)| {
        let mut account = kept.lock().unwrap();
        let json = |value: Value| (200, "application/json", value.to_string());
        if method == "GET" && target.starts_with("/download/") {
            let blob = target.split('/').nth(3).unwrap_or_default();
            return match account.blobs.get(blob) {
                Some(text) => (200, "application/sieve", text.clone()),
                None => (404, "text/plain", String::new()),
            };
        }
        if method == "GET" {
            return json(jmap_sieve_session(
                &known.lock().unwrap(),
                extensions,
                draft,
            ));
        }
        if target.starts_with("/upload/") {
            account.next += 1;
            let blob = format!("b{}", account.next);
            account.blobs.insert(blob.clone(), body.clone());
            return json(json!({
                "accountId": "a1", "blobId": blob, "type": "application/sieve", "size": body.len()
            }));
        }
        let request: Value = serde_json::from_str(body).unwrap();
        let call = &request["methodCalls"][0];
        let arguments = &call[1];
        let response = match call[0].as_str().unwrap() {
            "SieveScript/get" => {
                let list = account
                    .scripts
                    .iter()
                    .map(|(id, name, blob, active)| {
                        json!({ "id": id, "name": name, "blobId": blob, "isActive": active })
                    })
                    .collect::<Vec<_>>();
                json!([
                    "SieveScript/get",
                    { "accountId": "a1", "state": "1", "list": list, "notFound": [] },
                    call[2],
                ])
            }
            "SieveScript/set" => {
                let mut created = serde_json::Map::new();
                let mut ids = std::collections::HashMap::new();
                for (creation, object) in arguments["create"].as_object().into_iter().flatten() {
                    account.next += 1;
                    let id = format!("s{}", account.next);
                    let name = object["name"].as_str().unwrap().to_owned();
                    let blob = object["blobId"].as_str().unwrap().to_owned();
                    account.scripts.push((id.clone(), name, blob, false));
                    created.insert(creation.clone(), json!({ "id": id, "isActive": false }));
                    ids.insert(format!("#{creation}"), id);
                }
                let mut updated = serde_json::Map::new();
                for (id, patch) in arguments["update"].as_object().into_iter().flatten() {
                    let script = account.scripts.iter_mut().find(|script| &script.0 == id);
                    script.unwrap().2 = patch["blobId"].as_str().unwrap().to_owned();
                    updated.insert(id.clone(), Value::Null);
                }
                if let Some(activate) = arguments["onSuccessActivateScript"].as_str() {
                    let id = ids
                        .get(activate)
                        .map_or(activate, String::as_str)
                        .to_owned();
                    for script in &mut account.scripts {
                        script.3 = script.0 == id;
                    }
                }
                if arguments["onSuccessDeactivateScript"] == json!(true) {
                    for script in &mut account.scripts {
                        script.3 = false;
                    }
                }
                json!([
                    "SieveScript/set",
                    {
                        "accountId": "a1", "oldState": "1", "newState": "2",
                        "created": created, "updated": updated
                    },
                    call[2],
                ])
            }
            other => panic!("unexpected {other}"),
        };
        json(json!({ "methodResponses": [response], "sessionState": "session-1" }))
    })
    .await;
    *origin.lock().unwrap() = address.clone();
    (address, account)
}

async fn jmap_connect(origin: &str) -> crate::jmap::client::ConnectedClient {
    crate::jmap::client::connect_with(
        "me@example.test",
        "me@example.test",
        "secret",
        origin,
        Some("a1"),
    )
    .await
    .unwrap()
}

const SIEVE_EVERYTHING: &[&str] = &["fileinto", "vacation", "date", "relational", "include"];

#[tokio::test]
async fn replies_go_in_the_apps_script_on_a_jmap_server_with_sieve() {
    let (origin, account) = jmap_sieve_server(SIEVE_EVERYTHING, false).await;
    let connected = jmap_connect(&origin).await;
    assert!(!crate::jmap::vacation::offered(&connected));
    let mut scripts = crate::jmap::sieve::scripts(&connected).expect("Sieve with vacation");
    let reply = AutoReply {
        contacts_only: false,
        ..away()
    };
    let addresses = ["me@example.test".to_owned()];
    assert_eq!(
        crate::sieve::save_to(&mut scripts, &reply, &addresses, None)
            .await
            .unwrap(),
        AutoReplySaved::Done(reply.clone())
    );
    {
        let account = account.lock().unwrap();
        assert_eq!(account.active(), Some(crate::sieve::script::NAME));
        let text = account.text(crate::sieve::script::NAME).unwrap();
        assert!(text.contains("vacation :days 4 :addresses [\"me@example.test\"]"));
    }
    // Another device reads them back, and a second save replaces the script.
    let mut again = crate::jmap::sieve::scripts(&connected).unwrap();
    assert_eq!(crate::sieve::read_from(&mut again).await.unwrap(), reply);
    let later = AutoReply {
        text: "Back on Tuesday.".into(),
        ..reply.clone()
    };
    crate::sieve::save_to(&mut again, &later, &addresses, None)
        .await
        .unwrap();
    assert_eq!(account.lock().unwrap().scripts.len(), 1);
    let mut third = crate::jmap::sieve::scripts(&connected).unwrap();
    assert_eq!(crate::sieve::read_from(&mut third).await.unwrap(), later);
}

#[tokio::test]
async fn a_jmap_server_runs_the_users_own_script_after_the_replies() {
    let (origin, account) = jmap_sieve_server(SIEVE_EVERYTHING, false).await;
    let filters = "require \"fileinto\";\r\nfileinto \"Lists\";\r\n";
    account.lock().unwrap().put("filters", filters, true);
    let connected = jmap_connect(&origin).await;
    let reply = AutoReply {
        contacts_only: false,
        ..away()
    };
    let question = AutoReplyQuestion::RunBefore("filters".into());
    let mut scripts = crate::jmap::sieve::scripts(&connected).unwrap();
    assert_eq!(
        crate::sieve::save_to(&mut scripts, &reply, &[], None)
            .await
            .unwrap(),
        AutoReplySaved::Ask(question.clone())
    );
    assert_eq!(account.lock().unwrap().scripts.len(), 1, "nothing changed");
    crate::sieve::save_to(&mut scripts, &reply, &[], Some(&question))
        .await
        .unwrap();
    {
        let account = account.lock().unwrap();
        assert_eq!(account.active(), Some(crate::sieve::script::NAME));
        assert_eq!(account.text("filters"), Some(filters));
        let ours = account.text(crate::sieve::script::NAME).unwrap();
        assert!(ours.ends_with("include :personal :optional \"filters\";\r\n"));
    }
    // Turned off, their script runs alone again.
    let off = AutoReply {
        enabled: false,
        ..reply
    };
    let mut scripts = crate::jmap::sieve::scripts(&connected).unwrap();
    crate::sieve::save_to(&mut scripts, &off, &[], None)
        .await
        .unwrap();
    assert_eq!(account.lock().unwrap().active(), Some("filters"));
}

#[tokio::test]
async fn jmap_servers_name_their_sieve_extensions_where_they_can() {
    // Extensions listed with the session, as earlier drafts of RFC 9661 did.
    let (origin, _) = jmap_sieve_server(SIEVE_EVERYTHING, true).await;
    let connected = jmap_connect(&origin).await;
    let scripts = crate::jmap::sieve::scripts(&connected).expect("draft extensions");
    assert!(crate::sieve::Scripts::has_extension(&scripts, "Include"));
    // A server whose scripts can't reply offers no script replies, but
    // takes rules.
    let (origin, _) = jmap_sieve_server(&["fileinto"], false).await;
    let connected = jmap_connect(&origin).await;
    assert!(crate::jmap::sieve::replies(&connected).is_none());
    let scripts = crate::jmap::sieve::scripts(&connected).expect("Sieve for rules");
    assert!(crate::sieve::Scripts::has_extension(&scripts, "fileinto"));
}
