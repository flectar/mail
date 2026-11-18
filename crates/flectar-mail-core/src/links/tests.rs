use super::*;
use crate::accounts::credentials::{CredentialStoreHandle, DevelopmentFileCredentialStore};
use crate::config::Paths;
use crate::models::AuthKind;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A fake server's answer: a status, headers, and JSON or nothing.
struct Reply {
    status: u16,
    headers: Vec<(&'static str, String)>,
    body: Option<Value>,
}

impl From<(u16, Option<Value>)> for Reply {
    fn from((status, body): (u16, Option<Value>)) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body,
        }
    }
}

fn reply(status: u16, body: Value) -> Reply {
    (status, Some(body)).into()
}

fn header(status: u16, name: &'static str, value: String) -> Reply {
    Reply {
        status,
        headers: vec![(name, value)],
        body: None,
    }
}

/// A server that answers each request, on a connection of its own, with the next reply,
/// and hands back the requests it got.
async fn server<R: Into<Reply>>(replies: Vec<R>) -> (String, tokio::task::JoinHandle<Vec<String>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    serve(listener, replies)
}

fn serve<R: Into<Reply>>(
    listener: tokio::net::TcpListener,
    replies: Vec<R>,
) -> (String, tokio::task::JoinHandle<Vec<String>>) {
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let replies = replies.into_iter().map(Into::into).collect::<Vec<Reply>>();
    let task = tokio::spawn(async move {
        let mut requests = Vec::new();
        for Reply {
            status,
            headers,
            body: reply,
        } in replies
        {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut chunk = vec![0; 64 * 1024];
            let head_end = loop {
                let count = stream.read(&mut chunk).await.unwrap();
                assert!(count > 0, "the request ended early");
                bytes.extend_from_slice(&chunk[..count]);
                if let Some(at) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&bytes[..at]).to_lowercase();
                    let length = head
                        .lines()
                        .find_map(|line| {
                            line.strip_prefix("content-length:")
                                .and_then(|length| length.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if bytes.len() >= at + 4 + length {
                        break at + 4;
                    }
                }
            };
            // An upload's body is counted, not kept.
            let mut request = String::from_utf8_lossy(&bytes[..head_end]).into_owned();
            let body = &bytes[head_end..];
            if body.len() > 4096 {
                request.push_str(&format!("<{} bytes>", body.len()));
            } else {
                request.push_str(&String::from_utf8_lossy(body));
            }
            requests.push(request);
            let body = reply.map(|reply| reply.to_string()).unwrap_or_default();
            let headers = headers
                .iter()
                .map(|(name, value)| format!("{name}: {value}\r\n"))
                .collect::<String>();
            let response = format!(
                "HTTP/1.1 {status} Test\r\nConnection: close\r\nContent-Type: application/json\r\n\
                 {headers}Content-Length: {}\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        }
        requests
    });
    (url, task)
}

/// An OCS version 1 answer: HTTP 200, with the outcome in `meta.statuscode`.
fn ocs(code: u16, message: &str, data: Value) -> (u16, Option<Value>) {
    (
        200,
        Some(json!({
            "ocs": {
                "meta": {"status": if code == 100 { "ok" } else { "failure" }, "statuscode": code, "message": message},
                "data": data,
            }
        })),
    )
}

fn user() -> (u16, Option<Value>) {
    ocs(100, "OK", json!({"id": "ana", "display-name": "Ana Ruiz"}))
}

fn capabilities(public: Value, extra: Value) -> (u16, Option<Value>) {
    let mut capabilities = json!({
        "files_sharing": {"api_enabled": true, "public": public},
        "theming": {"name": "Example Cloud"},
    });
    if let (Some(capabilities), Some(extra)) = (capabilities.as_object_mut(), extra.as_object()) {
        for (key, value) in extra {
            capabilities.insert(key.clone(), value.clone());
        }
    }
    ocs(100, "OK", json!({"capabilities": capabilities}))
}

fn open_links() -> Value {
    json!({"enabled": true, "password": {"enforced": false}, "expire_date": {"enabled": false}})
}

async fn core(directory: &tempfile::TempDir) -> Core {
    let credentials: CredentialStoreHandle = Arc::new(DevelopmentFileCredentialStore::new(
        directory.path().join("credentials.json"),
    ));
    Core::start_mail_ui_with_credentials(Paths::for_tests(directory.path()), credentials)
        .await
        .unwrap()
}

/// The form fields of a request's body.
fn form(request: &str) -> std::collections::HashMap<String, String> {
    let body = request.split("\r\n\r\n").nth(1).unwrap_or_default();
    url::form_urlencoded::parse(body.as_bytes())
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect()
}

fn request_line(request: &str) -> &str {
    request.lines().next().unwrap_or_default()
}

#[test]
fn a_pasted_address_names_the_server() {
    let base = |value| nextcloud::base_url(value).map(|url| url.to_string());
    assert_eq!(
        base("cloud.example.com").unwrap(),
        "https://cloud.example.com/"
    );
    assert_eq!(
        base("https://cloud.example.com/nextcloud/remote.php/dav/files/ana/").unwrap(),
        "https://cloud.example.com/nextcloud/"
    );
    assert_eq!(
        base("https://cloud.example.com/index.php/apps/files/?dir=/").unwrap(),
        "https://cloud.example.com/"
    );
    assert!(base("http://cloud.example.com").is_err(), "only HTTPS");
}

#[test]
fn a_link_works_as_long_as_settings_and_the_server_say() {
    let today = chrono::NaiveDate::from_ymd_opt(2026, 11, 14).unwrap();
    assert_eq!(expiry_date(today, 30, None).as_deref(), Some("2026-12-14"));
    assert_eq!(expiry_date(today, 0, None), None);
    // A server that limits links has the last word.
    assert_eq!(
        expiry_date(today, 0, Some(7)).as_deref(),
        Some("2026-11-21")
    );
    assert_eq!(
        expiry_date(today, 30, Some(7)).as_deref(),
        Some("2026-11-21")
    );
    assert_eq!(
        expiry_date(today, 3, Some(7)).as_deref(),
        Some("2026-11-17")
    );
}

#[tokio::test]
async fn signing_in_in_the_browser_keeps_the_service() {
    let directory = tempfile::tempdir().unwrap();
    let core = core(&directory).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/", listener.local_addr().unwrap());
    // The poll endpoint is the same server, which the fake answers in order.
    let (url, requests) = serve(
        listener,
        vec![
            (
                200,
                Some(json!({
                    "poll": {"token": "polltoken", "endpoint": format!("{base}login/v2/poll")},
                    "login": format!("{base}login/v2/flow/abc"),
                })),
            ),
            (404, None),
            (
                200,
                Some(
                    json!({"server": base.trim_end_matches('/'), "loginName": "ana@example.org", "appPassword": "app-secret"}),
                ),
            ),
            user(),
            capabilities(open_links(), json!({})),
        ],
    );
    assert_eq!(url, base);
    let login = core.start_link_service_login(&url).await.unwrap();
    assert_eq!(login.login_url, format!("{base}login/v2/flow/abc"));
    let service = core.finish_link_service_login(login).await.unwrap();
    assert_eq!(service.server_url, base);
    assert_eq!(service.login_name, "ana@example.org");
    assert_eq!(service.display_name, "Example Cloud");
    assert_eq!(
        core.link_services().await.unwrap(),
        std::slice::from_ref(&service)
    );
    let requests = requests.await.unwrap();
    assert_eq!(
        request_line(&requests[0]),
        "POST /index.php/login/v2 HTTP/1.1"
    );
    assert_eq!(form(&requests[1])["token"], "polltoken");
    // The app password signs in from then on, and stays out of the database.
    assert!(requests[3].to_lowercase().contains("authorization: basic"));
    assert!(
        requests[3].contains("OCS-APIRequest: true")
            || requests[3].to_lowercase().contains("ocs-apirequest: true")
    );
    let secret = credentials::load_async(
        core.credentials.clone(),
        0,
        Slot::LinkServicePassword(service.id),
    )
    .await
    .unwrap();
    assert_eq!(secret, "app-secret");
    core.remove_link_service(service.id).await.unwrap();
    assert!(core.link_services().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_server_without_links_isnt_kept() {
    let directory = tempfile::tempdir().unwrap();
    let core = core(&directory).await;
    let (url, _) = server(vec![
        user(),
        capabilities(json!({"enabled": false}), json!({})),
    ])
    .await;
    let error = core
        .add_link_service(&url, "ana", "app-secret")
        .await
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "This server doesn't allow links to files."
    );
    assert!(core.link_services().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_file_goes_up_and_is_shared_by_a_link_that_ends() {
    let directory = tempfile::tempdir().unwrap();
    let core = core(&directory).await;
    let file = directory.path().join("report.pdf");
    std::fs::write(&file, b"%PDF report").unwrap();
    let (url, requests) = server(vec![
        user(),
        capabilities(open_links(), json!({})),
        user(),
        capabilities(open_links(), json!({})),
        (201, None),
        (201, None),
        (201, None),
        ocs(
            100,
            "OK",
            json!({"id": 7, "url": "https://cloud.example.com/s/AbCdEf", "expiration": "2026-12-14 00:00:00"}),
        ),
        user(),
        ocs(100, "OK", json!([])),
        (204, None),
    ])
    .await;
    let service = core
        .add_link_service(&url, "ana", "app-secret")
        .await
        .unwrap();
    let link = core
        .share_by_link(service.id, &file, "report.pdf")
        .await
        .unwrap();
    assert_eq!(link.url, "https://cloud.example.com/s/AbCdEf");
    assert_eq!(link.size, 11);
    assert_eq!(link.expires_on.as_deref(), Some("2026-12-14"));
    assert_eq!(link.password, None);
    assert_eq!(link.share_id, "7");
    core.unshare_link(&link).await.unwrap();

    let requests = requests.await.unwrap();
    assert_eq!(
        request_line(&requests[4]),
        "MKCOL /remote.php/dav/files/ana/Mail%20attachments HTTP/1.1"
    );
    assert!(
        request_line(&requests[5])
            .starts_with("MKCOL /remote.php/dav/files/ana/Mail%20attachments/")
    );
    assert!(
        request_line(&requests[6]).starts_with("PUT /remote.php/dav/files/ana/Mail%20attachments/")
    );
    assert!(request_line(&requests[6]).ends_with("/report.pdf HTTP/1.1"));
    assert!(requests[6].ends_with("%PDF report"));
    let share = form(&requests[7]);
    assert_eq!(share["path"], format!("/{}/report.pdf", link.folder));
    assert_eq!(share["shareType"], "3");
    assert_eq!(share["permissions"], "1");
    // Settings' 30 days, from today.
    let expected = (chrono::Local::now().date_naive() + chrono::Days::new(30))
        .format("%Y-%m-%d")
        .to_string();
    assert_eq!(share["expireDate"], expected);
    assert!(!share.contains_key("password"));
    assert_eq!(
        request_line(&requests[9]),
        "DELETE /ocs/v1.php/apps/files_sharing/api/v1/shares/7?format=json HTTP/1.1"
    );
    assert!(
        request_line(&requests[10])
            .starts_with("DELETE /remote.php/dav/files/ana/Mail%20attachments/")
    );
}

#[tokio::test]
async fn a_server_that_asks_for_passwords_and_short_links_gets_them() {
    let directory = tempfile::tempdir().unwrap();
    let core = core(&directory).await;
    let file = directory.path().join("plans.zip");
    std::fs::write(&file, b"zip").unwrap();
    let strict = json!({
        "enabled": true,
        "password": {"enforced": true},
        "expire_date": {"enabled": true, "enforced": true, "days": 7},
    });
    let (url, requests) = server(vec![
        user(),
        capabilities(strict.clone(), json!({})),
        user(),
        capabilities(strict, json!({})),
        (201, None),
        (201, None),
        (201, None),
        ocs(
            100,
            "OK",
            json!({"id": "9", "url": "https://cloud.example.com/s/Zz", "expiration": null}),
        ),
    ])
    .await;
    let service = core
        .add_link_service(&url, "ana", "app-secret")
        .await
        .unwrap();
    let link = core
        .share_by_link(service.id, &file, "plans.zip")
        .await
        .unwrap();
    let password = link.password.clone().expect("a password");
    assert_eq!(password.len(), 16);
    let expected = (chrono::Local::now().date_naive() + chrono::Days::new(7))
        .format("%Y-%m-%d")
        .to_string();
    assert_eq!(link.expires_on.as_deref(), Some(expected.as_str()));
    let requests = requests.await.unwrap();
    let share = form(&requests[7]);
    assert_eq!(share["password"], password);
    assert_eq!(share["expireDate"], expected);
}

#[tokio::test]
async fn a_refused_share_leaves_nothing_behind() {
    let directory = tempfile::tempdir().unwrap();
    let core = core(&directory).await;
    let file = directory.path().join("notes.txt");
    std::fs::write(&file, b"notes").unwrap();
    let (url, requests) = server(vec![
        user(),
        capabilities(open_links(), json!({})),
        user(),
        capabilities(open_links(), json!({})),
        (201, None),
        (201, None),
        (201, None),
        ocs(
            403,
            "Passwords are enforced for link and mail shares",
            Value::Null,
        ),
        (204, None),
    ])
    .await;
    let service = core
        .add_link_service(&url, "ana", "app-secret")
        .await
        .unwrap();
    let error = core
        .share_by_link(service.id, &file, "notes.txt")
        .await
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "Passwords are enforced for link and mail shares"
    );
    let requests = requests.await.unwrap();
    assert!(
        request_line(&requests[8])
            .starts_with("DELETE /remote.php/dav/files/ana/Mail%20attachments/")
    );
}

#[tokio::test]
async fn a_large_file_goes_up_in_pieces_where_the_server_takes_them() {
    let directory = tempfile::tempdir().unwrap();
    let core = core(&directory).await;
    let file = directory.path().join("video.mp4");
    let size = 2 * 10 * 1024 * 1024 + 5;
    std::fs::write(&file, vec![7u8; size]).unwrap();
    let chunking = json!({"files": {"bigfilechunking": true}});
    let (url, requests) = server(vec![
        user(),
        capabilities(open_links(), chunking.clone()),
        user(),
        capabilities(open_links(), chunking),
        (201, None),
        (201, None),
        (201, None),
        (201, None),
        (201, None),
        (201, None),
        (201, None),
        ocs(100, "OK", json!({"id": 3, "url": "https://cloud.example.com/s/Vid", "expiration": "2026-12-14 00:00:00"})),
    ])
    .await;
    let service = core
        .add_link_service(&url, "ana", "app-secret")
        .await
        .unwrap();
    let link = core
        .share_by_link(service.id, &file, "video.mp4")
        .await
        .unwrap();
    assert_eq!(link.size, size as u64);
    let requests = requests.await.unwrap();
    assert!(request_line(&requests[6]).starts_with("MKCOL /remote.php/dav/uploads/ana/flectar-"));
    let destination =
        format!("destination: {url}remote.php/dav/files/ana/Mail%20attachments/").to_lowercase();
    for (index, (number, length)) in [(1, 10 * 1024 * 1024), (2, 10 * 1024 * 1024), (3, 5)]
        .into_iter()
        .enumerate()
    {
        let request = &requests[7 + index];
        assert!(
            request_line(request).ends_with(&format!("/{number:05} HTTP/1.1")),
            "{}",
            request_line(request)
        );
        assert!(request.to_lowercase().contains(&destination));
        assert!(
            request
                .to_lowercase()
                .contains(&format!("oc-total-length: {size}"))
        );
        if length > 4096 {
            assert!(request.ends_with(&format!("<{length} bytes>")));
        }
    }
    assert!(request_line(&requests[10]).starts_with("MOVE /remote.php/dav/uploads/ana/flectar-"));
    assert!(request_line(&requests[10]).ends_with("/.file HTTP/1.1"));
}

/// A mail account signed in with OAuth, or with a password for another provider.
async fn mail_account(core: &Core, provider: Provider, email: &'static str) -> i64 {
    core.db
        .write(move |conn| {
            let settings = crate::db::testutil::account_settings();
            let (protocol, auth_kind) = match provider {
                Provider::Gmail => (crate::models::MailProtocol::Gmail, AuthKind::Oauth2),
                Provider::Microsoft => (crate::models::MailProtocol::Imap, AuthKind::Oauth2),
                Provider::Generic => (crate::models::MailProtocol::Imap, AuthKind::Password),
            };
            repo::accounts::insert(
                conn,
                &repo::accounts::NewAccount {
                    email,
                    display_name: None,
                    avatar_url: None,
                    provider,
                    auth_kind,
                    mail_protocol: protocol,
                    username: email,
                    jmap_url: "",
                    jmap_account_id: None,
                    incoming_host: "imap.example.com",
                    incoming_port: 993,
                    smtp_host: "smtp.example.com",
                    smtp_port: 465,
                },
                &settings,
            )
        })
        .await
        .unwrap()
}

/// A storage service kept as signing in keeps it, answering at `url`.
async fn kept_service(
    core: &Core,
    kind: LinkServiceKind,
    account_id: Option<i64>,
    url: &str,
) -> i64 {
    let url = url.to_owned();
    core.db
        .write(move |conn| {
            conn.execute(
                "INSERT INTO link_services
                   (kind, account_id, server_url, login_name, display_name, created_at)
                 VALUES (?1, ?2, ?3, 'ana@example.org', 'Storage', 0)",
                params![kind.as_str(), account_id, url],
            )?;
            Ok(conn.last_insert_rowid())
        })
        .await
        .unwrap()
}

/// A request's JSON body.
fn body(request: &str) -> Value {
    serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap_or_default()).unwrap()
}

/// A file of `size` bytes in `directory`.
fn file_of(directory: &tempfile::TempDir, name: &str, size: usize) -> std::path::PathBuf {
    let path = directory.path().join(name);
    std::fs::write(
        &path,
        (0..size).map(|index| index as u8).collect::<Vec<_>>(),
    )
    .unwrap();
    path
}

#[tokio::test]
async fn a_message_sends_files_from_its_own_accounts_storage_or_the_server() {
    let directory = tempfile::tempdir().unwrap();
    let core = core(&directory).await;
    let gmail = mail_account(&core, Provider::Gmail, "ana@gmail.com").await;
    let microsoft = mail_account(&core, Provider::Microsoft, "ana@outlook.com").await;
    let other = mail_account(&core, Provider::Generic, "ana@example.org").await;
    assert_eq!(core.link_service_for_account(gmail).await.unwrap(), None);
    assert!(core.google_scopes_in_use(gmail).await.unwrap().is_empty());

    let drive = kept_service(&core, LinkServiceKind::GoogleDrive, Some(gmail), drive::API).await;
    let pick = |account| {
        let core = &core;
        async move {
            core.link_service_for_account(account)
                .await
                .unwrap()
                .map(|service| service.id)
        }
    };
    assert_eq!(pick(gmail).await, Some(drive));
    // One account's storage doesn't take another's files.
    assert_eq!(pick(microsoft).await, None);
    assert_eq!(pick(other).await, None);
    // Signing in again asks Google for Drive as well, to keep it.
    assert_eq!(
        core.google_scopes_in_use(gmail).await.unwrap(),
        [oauth::providers::GOOGLE_DRIVE_FILE_SCOPE]
    );

    let server = kept_service(
        &core,
        LinkServiceKind::Nextcloud,
        None,
        "https://cloud.example.com/",
    )
    .await;
    assert_eq!(pick(gmail).await, Some(drive));
    assert_eq!(pick(microsoft).await, Some(server));
    assert_eq!(pick(other).await, Some(server));

    // An account that goes takes its storage with it.
    core.db
        .write(move |conn| repo::accounts::delete(conn, gmail))
        .await
        .unwrap();
    let left = core.link_services().await.unwrap();
    assert_eq!(
        left.iter().map(|service| service.id).collect::<Vec<_>>(),
        [server]
    );
    let onedrive = kept_service(
        &core,
        LinkServiceKind::OneDrive,
        Some(microsoft),
        onedrive::API,
    )
    .await;
    assert_eq!(pick(microsoft).await, Some(onedrive));
    // Its storage has no password to forget.
    core.remove_link_service(onedrive).await.unwrap();
    assert_eq!(pick(microsoft).await, Some(server));
}

#[tokio::test]
async fn a_file_goes_up_to_google_drive_and_is_shared_by_a_link_that_lasts() {
    let directory = tempfile::tempdir().unwrap();
    let core = core(&directory).await;
    let gmail = mail_account(&core, Provider::Gmail, "ana@gmail.com").await;
    core.tokens
        .store_initial(
            gmail,
            "drive-token".into(),
            Some(3600),
            Some("refresh".into()),
            "client".into(),
            None,
        )
        .await
        .unwrap();
    let file = directory.path().join("report.pdf");
    std::fs::write(&file, b"%PDF report").unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/", listener.local_addr().unwrap());
    let (url, requests) = serve(
        listener,
        vec![
            reply(200, json!({"files": []})),
            reply(200, json!({"id": "folder1"})),
            header(
                200,
                "Location",
                format!("{base}upload/drive/v3/files?uploadType=resumable&upload_id=s1"),
            ),
            reply(200, json!({"id": "file1"})),
            reply(200, json!({"id": "anyoneWithLink"})),
            reply(
                200,
                json!({"webViewLink": "https://drive.google.com/file/d/file1/view?usp=drivesdk"}),
            ),
            (204, None).into(),
        ],
    );
    let service = kept_service(&core, LinkServiceKind::GoogleDrive, Some(gmail), &url).await;
    let link = core
        .share_by_link(service, &file, "report.pdf")
        .await
        .unwrap();
    assert_eq!(
        link.url,
        "https://drive.google.com/file/d/file1/view?usp=drivesdk"
    );
    assert_eq!(link.size, 11);
    // Drive can't make a link that anyone opens expire, and asks for no password.
    assert_eq!(link.expires_on, None);
    assert_eq!(link.password, None);
    assert_eq!(link.share_id, "file1");
    core.unshare_link(&link).await.unwrap();

    let requests = requests.await.unwrap();
    assert!(
        request_line(&requests[0])
            .starts_with("GET /drive/v3/files?q=name+%3D+%27Mail+attachments%27")
    );
    assert!(
        requests[0]
            .to_lowercase()
            .contains("authorization: bearer drive-token")
    );
    assert_eq!(
        request_line(&requests[1]),
        "POST /drive/v3/files?fields=id HTTP/1.1"
    );
    assert_eq!(
        body(&requests[1]),
        json!({"name": "Mail attachments", "mimeType": "application/vnd.google-apps.folder"})
    );
    assert_eq!(
        request_line(&requests[2]),
        "POST /upload/drive/v3/files?uploadType=resumable&fields=id HTTP/1.1"
    );
    assert!(
        requests[2]
            .to_lowercase()
            .contains("x-upload-content-length: 11")
    );
    assert_eq!(
        body(&requests[2]),
        json!({"name": "report.pdf", "parents": ["folder1"]})
    );
    assert_eq!(
        request_line(&requests[3]),
        "PUT /upload/drive/v3/files?uploadType=resumable&upload_id=s1 HTTP/1.1"
    );
    // The session's address authorizes the file's bytes, which go without the token.
    assert!(
        requests[3]
            .to_lowercase()
            .contains("content-range: bytes 0-10/11")
    );
    assert!(!requests[3].to_lowercase().contains("authorization"));
    assert!(requests[3].ends_with("%PDF report"));
    assert_eq!(
        request_line(&requests[4]),
        "POST /drive/v3/files/file1/permissions HTTP/1.1"
    );
    assert_eq!(
        body(&requests[4]),
        json!({"role": "reader", "type": "anyone"})
    );
    assert_eq!(
        request_line(&requests[5]),
        "GET /drive/v3/files/file1?fields=webViewLink HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[6]),
        "DELETE /drive/v3/files/file1 HTTP/1.1"
    );
}

#[tokio::test]
async fn a_large_file_goes_up_to_drive_from_where_google_says() {
    let directory = tempfile::tempdir().unwrap();
    let size = 8 * 1024 * 1024 + 5;
    let file = file_of(&directory, "big.bin", size);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/", listener.local_addr().unwrap());
    let (url, requests) = serve(
        listener,
        vec![
            header(200, "Location", format!("{base}upload/s2")),
            // Half the first piece arrived.
            header(308, "Range", "bytes=0-4194303".to_owned()),
            reply(201, json!({"id": "big"})),
        ],
    );
    let drive =
        drive::Drive::new(crate::net::proxy::Owner::App, &url, "drive-token".into()).unwrap();
    let (id, uploaded) = drive.upload(&file, "big.bin", "folder1").await.unwrap();
    assert_eq!((id.as_str(), uploaded), ("big", size as u64));
    let requests = requests.await.unwrap();
    assert!(
        requests[1]
            .to_lowercase()
            .contains("content-range: bytes 0-8388607/8388613")
    );
    assert!(requests[1].ends_with("<8388608 bytes>"));
    assert!(
        requests[2]
            .to_lowercase()
            .contains("content-range: bytes 4194304-8388612/8388613")
    );
    assert!(requests[2].ends_with("<4194309 bytes>"));
}

#[tokio::test]
async fn a_drive_link_the_organization_refuses_leaves_nothing_behind() {
    let directory = tempfile::tempdir().unwrap();
    let file = file_of(&directory, "notes.txt", 5);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/", listener.local_addr().unwrap());
    let (url, requests) = serve(
        listener,
        vec![
            // The app's folder is there from before.
            reply(200, json!({"files": [{"id": "folder1"}]})),
            header(200, "Location", format!("{base}upload/s3")),
            reply(200, json!({"id": "file1"})),
            reply(
                400,
                json!({"error": {
                    "errors": [{
                        "domain": "global",
                        "reason": "invalidSharingRequest",
                        "message": "Bad Request. User message: \"ACL change not allowed.\"",
                    }],
                    "code": 400,
                    "message": "Bad Request",
                }}),
            ),
            (204, None).into(),
        ],
    );
    let drive =
        drive::Drive::new(crate::net::proxy::Owner::App, &url, "drive-token".into()).unwrap();
    let error = share_on_drive(&drive, 1, &file, "notes.txt")
        .await
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "Your organization doesn't allow links that anyone can open."
    );
    let requests = requests.await.unwrap();
    assert!(request_line(&requests[1]).starts_with("POST /upload/drive/v3/files"));
    assert_eq!(
        request_line(&requests[4]),
        "DELETE /drive/v3/files/file1 HTTP/1.1"
    );
}

#[tokio::test]
async fn a_file_goes_up_to_onedrive_in_a_folder_of_its_own() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("plan.pdf");
    std::fs::write(&file, b"%PDF report").unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/", listener.local_addr().unwrap());
    let (url, requests) = serve(
        listener,
        vec![
            reply(
                200,
                json!({"id": "app1", "name": "Mail attachments", "folder": {}}),
            ),
            reply(201, json!({"id": "folder9", "folder": {}})),
            reply(
                200,
                json!({"uploadUrl": format!("{base}up/abc"), "expirationDateTime": "2026-11-22T10:00:00Z"}),
            ),
            reply(
                201,
                json!({"id": "item1", "name": "Q3_ plan_.pdf", "size": 11, "parentReference": {"id": "folder9"}}),
            ),
            // A personal account without Microsoft 365 can't make a link expire.
            reply(
                400,
                json!({"error": {"code": "invalidRequest", "message": "Invalid expiration date"}}),
            ),
            reply(
                201,
                json!({"id": "perm1", "link": {"type": "view", "scope": "anonymous", "webUrl": "https://1drv.ms/u/s!abc"}}),
            ),
            (204, None).into(),
        ],
    );
    let drive =
        onedrive::OneDrive::new(crate::net::proxy::Owner::App, &url, "graph-token".into()).unwrap();
    let link = share_on_onedrive(&drive, 4, &file, "Q3: plan?.pdf", Some("2026-12-15".into()))
        .await
        .unwrap();
    assert_eq!(link.url, "https://1drv.ms/u/s!abc");
    assert_eq!(link.name, "Q3: plan?.pdf");
    assert_eq!(link.size, 11);
    assert_eq!(link.expires_on, None);
    assert_eq!(
        (link.share_id.as_str(), link.folder.as_str()),
        ("item1", "folder9")
    );
    drive.delete(&link.folder).await.unwrap();

    let requests = requests.await.unwrap();
    assert_eq!(
        request_line(&requests[0]),
        "GET /me/drive/root:/Mail%20attachments HTTP/1.1"
    );
    assert!(
        requests[0]
            .to_lowercase()
            .contains("authorization: bearer graph-token")
    );
    assert_eq!(
        request_line(&requests[1]),
        "POST /me/drive/items/app1/children HTTP/1.1"
    );
    let folder = body(&requests[1]);
    assert_eq!(folder["name"].as_str().map(str::len), Some(12));
    assert_eq!(folder["folder"], json!({}));
    let line = request_line(&requests[2]);
    // OneDrive refuses some characters in names.
    assert_eq!(
        line,
        "POST /me/drive/items/folder9:/Q3_%20plan_.pdf:/createUploadSession HTTP/1.1"
    );
    assert_eq!(
        body(&requests[2]),
        json!({"item": {"@microsoft.graph.conflictBehavior": "rename"}})
    );
    assert_eq!(request_line(&requests[3]), "PUT /up/abc HTTP/1.1");
    // Graph refuses a token with the file's bytes.
    assert!(!requests[3].to_lowercase().contains("authorization"));
    assert!(
        requests[3]
            .to_lowercase()
            .contains("content-range: bytes 0-10/11")
    );
    assert_eq!(
        request_line(&requests[4]),
        "POST /me/drive/items/item1/createLink HTTP/1.1"
    );
    assert_eq!(
        body(&requests[4]),
        json!({"type": "view", "scope": "anonymous", "expirationDateTime": "2026-12-15T23:59:59Z"})
    );
    assert_eq!(
        body(&requests[5]),
        json!({"type": "view", "scope": "anonymous"})
    );
    assert_eq!(
        request_line(&requests[6]),
        "DELETE /me/drive/items/folder9 HTTP/1.1"
    );
}

#[tokio::test]
async fn a_large_file_goes_up_to_onedrive_from_where_the_session_says() {
    let directory = tempfile::tempdir().unwrap();
    let size = 10 * 1024 * 1024 + 7;
    let file = file_of(&directory, "big.bin", size);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/", listener.local_addr().unwrap());
    let (url, requests) = serve(
        listener,
        vec![
            reply(200, json!({"uploadUrl": format!("{base}up/big")})),
            reply(202, json!({"nextExpectedRanges": ["5242880-"]})),
            reply(201, json!({"id": "big", "parentReference": {"id": "f2"}})),
        ],
    );
    let drive =
        onedrive::OneDrive::new(crate::net::proxy::Owner::App, &url, "graph-token".into()).unwrap();
    let (id, uploaded) = drive.upload(&file, "f2", "big.bin").await.unwrap();
    assert_eq!((id.as_str(), uploaded), ("big", size as u64));
    let requests = requests.await.unwrap();
    assert_eq!(
        request_line(&requests[0]),
        "POST /me/drive/items/f2:/big.bin:/createUploadSession HTTP/1.1"
    );
    assert!(
        requests[1]
            .to_lowercase()
            .contains("content-range: bytes 0-10485759/10485767")
    );
    assert!(requests[1].ends_with("<10485760 bytes>"));
    assert!(
        requests[2]
            .to_lowercase()
            .contains("content-range: bytes 5242880-10485766/10485767")
    );
    assert!(requests[2].ends_with("<5242887 bytes>"));
}

#[tokio::test]
async fn an_empty_file_goes_up_to_onedrive_in_one_request() {
    let directory = tempfile::tempdir().unwrap();
    let file = file_of(&directory, "empty.txt", 0);
    let (url, requests) = server(vec![reply(
        201,
        json!({"id": "e1", "parentReference": {"id": "f3"}}),
    )])
    .await;
    let drive =
        onedrive::OneDrive::new(crate::net::proxy::Owner::App, &url, "graph-token".into()).unwrap();
    let (id, size) = drive.upload(&file, "f3", "empty.txt").await.unwrap();
    assert_eq!((id.as_str(), size), ("e1", 0));
    let requests = requests.await.unwrap();
    assert_eq!(
        request_line(&requests[0]),
        "PUT /me/drive/items/f3:/empty.txt:/content HTTP/1.1"
    );
    assert!(
        requests[0]
            .to_lowercase()
            .contains("authorization: bearer graph-token")
    );
}

#[tokio::test]
async fn a_onedrive_link_the_organization_refuses_leaves_nothing_behind() {
    let directory = tempfile::tempdir().unwrap();
    let file = file_of(&directory, "notes.txt", 5);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/", listener.local_addr().unwrap());
    let (url, requests) = serve(
        listener,
        vec![
            // The app's folder isn't there yet, and is made.
            reply(
                404,
                json!({"error": {"code": "itemNotFound", "message": "The resource could not be found."}}),
            ),
            reply(201, json!({"id": "app1", "folder": {}})),
            reply(201, json!({"id": "folder2", "folder": {}})),
            reply(200, json!({"uploadUrl": format!("{base}up/n")})),
            reply(
                201,
                json!({"id": "item2", "parentReference": {"id": "folder2"}}),
            ),
            reply(
                403,
                json!({"error": {"code": "accessDenied", "message": "Access denied"}}),
            ),
            (204, None).into(),
        ],
    );
    let drive =
        onedrive::OneDrive::new(crate::net::proxy::Owner::App, &url, "graph-token".into()).unwrap();
    let error = share_on_onedrive(&drive, 1, &file, "notes.txt", None)
        .await
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "Your organization doesn't allow links that anyone can open."
    );
    let requests = requests.await.unwrap();
    assert_eq!(
        request_line(&requests[1]),
        "POST /me/drive/root/children HTTP/1.1"
    );
    assert_eq!(
        body(&requests[1]),
        json!({"name": "Mail attachments", "folder": {}, "@microsoft.graph.conflictBehavior": "fail"})
    );
    assert_eq!(
        request_line(&requests[2]),
        "POST /me/drive/items/app1/children HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[6]),
        "DELETE /me/drive/items/folder2 HTTP/1.1"
    );
}
