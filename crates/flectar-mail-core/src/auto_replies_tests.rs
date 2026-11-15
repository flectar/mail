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
                let (status, json) = answer(&request);
                requests.lock().unwrap().push(request);
                let body = json.to_string();
                let response = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n\
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
