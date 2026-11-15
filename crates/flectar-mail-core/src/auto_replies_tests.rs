use super::*;
use crate::accounts::credentials::DevelopmentFileCredentialStore;
use crate::config::Paths;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

/// A request the Gmail stand-in answered: method, target and body.
type Request = (String, String, String);

/// A Gmail stand-in that answers each request with `answer`'s status and
/// JSON, and keeps the requests.
async fn gmail(
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
    let (origin, requests) = gmail(|(method, target, body)| {
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
        .write(|conn| repo::auto_replies::store(conn, 1, &away(), 1, false))
        .await
        .unwrap();
    let scopes = core.google_scopes_in_use(1).await.unwrap();
    assert!(!scopes.contains(&oauth::providers::GMAIL_SETTINGS_SCOPE));

    // Saved by the app, they do, and a new sign-in asks for it again.
    core.db
        .write(|conn| repo::auto_replies::store(conn, 1, &away(), 2, true))
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
