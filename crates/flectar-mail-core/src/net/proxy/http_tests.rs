//! HTTP requests through a proxy, as reqwest makes them for `http_builder`.

// Shared with the integration tests, which use more of it.
#[allow(dead_code)]
#[path = "../../../tests/support/proxy.rs"]
mod test_proxy;

use super::*;
use test_proxy::{Kind, TestProxy};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// A web server that answers one request with `ok`, and tells its request
/// line.
async fn web_server() -> (u16, tokio::task::JoinHandle<String>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut stream = BufReader::new(tcp);
        let mut request_line = String::new();
        stream.read_line(&mut request_line).await.unwrap();
        loop {
            let mut line = String::new();
            stream.read_line(&mut line).await.unwrap();
            if line == "\r\n" || line.is_empty() {
                break;
            }
        }
        stream
            .get_mut()
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
            .await
            .unwrap();
        request_line.trim_end().to_owned()
    });
    (port, task)
}

fn own(account_id: i64, kind: ProxyKind, port: u16, user: &str, password: &str) {
    set_account(
        account_id,
        Some(AccountOverride::Own(Manual::new(
            &ManualProxy {
                kind,
                host: "127.0.0.1".into(),
                port,
                user: user.into(),
                bypass: String::new(),
            },
            Some(password.into()),
        ))),
    );
}

async fn get(owner: Owner, url: &str) -> String {
    http_builder(owner)
        .build()
        .unwrap()
        .get(url)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap()
}

#[tokio::test]
async fn requests_go_through_a_socks5_proxy_that_resolves_the_name() {
    let (port, server) = web_server().await;
    // Characters a URL has to encode reach the proxy as typed.
    let proxy = TestProxy::start(Kind::Socks5, port, Some(("ana@example.com", "p:w/d%"))).await;
    own(
        9_101,
        ProxyKind::Socks5,
        proxy.port,
        "ana@example.com",
        "p:w/d%",
    );
    let body = get(Owner::Account(9_101), "http://web.example.test/page").await;
    assert_eq!(body, "ok");
    assert_eq!(proxy.asked(), ["web.example.test:80"]);
    assert_eq!(server.await.unwrap(), "GET /page HTTP/1.1");
    set_account(9_101, None);
}

#[tokio::test]
async fn requests_go_through_an_http_proxy_that_signs_in() {
    let (port, server) = web_server().await;
    let proxy = TestProxy::start(Kind::Http, port, Some(("ana", "s3cret"))).await;
    own(9_102, ProxyKind::Http, proxy.port, "ana", "s3cret");
    let body = get(Owner::Account(9_102), "http://web.example.test/page").await;
    assert_eq!(body, "ok");
    assert_eq!(proxy.asked(), ["GET http://web.example.test/page HTTP/1.1"]);
    server.await.unwrap();
    set_account(9_102, None);
}
