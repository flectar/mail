use super::*;
use crate::accounts::credentials::{CredentialStoreHandle, DevelopmentFileCredentialStore};
use crate::config::Paths;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A server that answers each request, on a connection of its own, with the next reply,
/// JSON or nothing, and hands back the requests it got.
async fn server(
    replies: Vec<(u16, Option<Value>)>,
) -> (String, tokio::task::JoinHandle<Vec<String>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    serve(listener, replies)
}

fn serve(
    listener: tokio::net::TcpListener,
    replies: Vec<(u16, Option<Value>)>,
) -> (String, tokio::task::JoinHandle<Vec<String>>) {
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let mut requests = Vec::new();
        for (status, reply) in replies {
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
            let response = format!(
                "HTTP/1.1 {status} Test\r\nConnection: close\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\n\r\n{body}",
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
