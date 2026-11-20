use super::*;
use tokio::io::{AsyncBufReadExt, BufReader, DuplexStream, duplex};

const GREETING: &[u8] = b"* OK IMAP4rev2 ready\r\n";

fn server(kind: ProxyKind, credentials: Option<(&str, &str)>) -> ProxyServer {
    ProxyServer {
        kind,
        host: "proxy.example".into(),
        port: 1080,
        credentials: credentials.map(|(user, password)| Credentials {
            user: user.into(),
            password: password.into(),
        }),
    }
}

/// Run `proxy` against the far end of a connection while the tunnel opens
/// on the near end, then read what the tunnel delivers first.
async fn tunnel_to<F, Fut>(
    server: ProxyServer,
    host: &str,
    proxy: F,
) -> (Result<(), ProxyError>, Vec<u8>)
where
    F: FnOnce(DuplexStream) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let (mut near, far) = duplex(64 * 1024);
    let proxy = tokio::spawn(proxy(far));
    let result = open(&mut near, &server, host, 993).await;
    proxy.await.unwrap();
    let mut delivered = Vec::new();
    if result.is_ok() {
        near.read_to_end(&mut delivered).await.unwrap();
    }
    (result, delivered)
}

async fn read_exactly(stream: &mut DuplexStream, length: usize) -> Vec<u8> {
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes).await.unwrap();
    bytes
}

/// The SOCKS5 CONNECT request for `name`:993.
fn socks_request(name: &str) -> Vec<u8> {
    let mut request = vec![5, 1, 0, 3, name.len() as u8];
    request.extend_from_slice(name.as_bytes());
    request.extend_from_slice(&993u16.to_be_bytes());
    request
}

#[tokio::test]
async fn socks5_tunnel_keeps_a_greeting_sent_with_the_reply() {
    let (result, delivered) = tunnel_to(
        server(ProxyKind::Socks5, None),
        "imap.example.com",
        |mut far| async move {
            assert_eq!(read_exactly(&mut far, 3).await, [5, 1, 0]);
            far.write_all(&[5, 0]).await.unwrap();
            let request = socks_request("imap.example.com");
            assert_eq!(read_exactly(&mut far, request.len()).await, request);
            // The reply and the server's greeting in one write.
            let mut reply = vec![5, 0, 0, 1, 192, 0, 2, 1, 0x03, 0xe1];
            reply.extend_from_slice(GREETING);
            far.write_all(&reply).await.unwrap();
        },
    )
    .await;
    result.unwrap();
    assert_eq!(delivered, GREETING);
}

#[tokio::test]
async fn socks5_reads_a_reply_that_names_the_bound_host() {
    let (result, delivered) = tunnel_to(
        server(ProxyKind::Socks5, None),
        "imap.example.com",
        |mut far| async move {
            read_exactly(&mut far, 3).await;
            far.write_all(&[5, 0]).await.unwrap();
            read_exactly(&mut far, socks_request("imap.example.com").len()).await;
            let mut reply = vec![5, 0, 0, 3, 9];
            reply.extend_from_slice(b"relay.lan");
            reply.extend_from_slice(&[0x04, 0x38]);
            reply.extend_from_slice(GREETING);
            far.write_all(&reply).await.unwrap();
        },
    )
    .await;
    result.unwrap();
    assert_eq!(delivered, GREETING);
}

#[tokio::test]
async fn socks5_signs_in_with_user_name_and_password() {
    let (result, delivered) = tunnel_to(
        server(ProxyKind::Socks5, Some(("ana", "s3cret"))),
        "imap.example.com",
        |mut far| async move {
            assert_eq!(read_exactly(&mut far, 4).await, [5, 2, 0, 2]);
            far.write_all(&[5, 2]).await.unwrap();
            assert_eq!(
                read_exactly(&mut far, 12).await,
                b"\x01\x03ana\x06s3cret".as_slice()
            );
            far.write_all(&[1, 0]).await.unwrap();
            read_exactly(&mut far, socks_request("imap.example.com").len()).await;
            far.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0])
                .await
                .unwrap();
            far.write_all(GREETING).await.unwrap();
        },
    )
    .await;
    result.unwrap();
    assert_eq!(delivered, GREETING);
}

#[tokio::test]
async fn socks5_reports_rejected_credentials() {
    let (result, _) = tunnel_to(
        server(ProxyKind::Socks5, Some(("ana", "wrong"))),
        "imap.example.com",
        |mut far| async move {
            read_exactly(&mut far, 4).await;
            far.write_all(&[5, 2]).await.unwrap();
            read_exactly(&mut far, 11).await;
            far.write_all(&[1, 1]).await.unwrap();
        },
    )
    .await;
    assert!(
        matches!(result, Err(ProxyError::CredentialsRejected { ref proxy }) if proxy == "proxy.example:1080"),
        "{result:?}"
    );
}

#[tokio::test]
async fn socks5_says_when_the_proxy_wants_credentials() {
    let (result, _) = tunnel_to(
        server(ProxyKind::Socks5, None),
        "imap.example.com",
        |mut far| async move {
            read_exactly(&mut far, 3).await;
            far.write_all(&[5, 0xff]).await.unwrap();
        },
    )
    .await;
    assert!(
        matches!(result, Err(ProxyError::CredentialsNeeded { .. })),
        "{result:?}"
    );
}

#[tokio::test]
async fn socks5_explains_a_refusal() {
    let (result, _) = tunnel_to(
        server(ProxyKind::Socks5, None),
        "imap.example.com",
        |mut far| async move {
            read_exactly(&mut far, 3).await;
            far.write_all(&[5, 0]).await.unwrap();
            read_exactly(&mut far, socks_request("imap.example.com").len()).await;
            far.write_all(&[5, 5, 0, 1, 0, 0, 0, 0, 0, 0])
                .await
                .unwrap();
        },
    )
    .await;
    let error = result.unwrap_err();
    assert!(matches!(error, ProxyError::Refused { .. }), "{error:?}");
    assert_eq!(
        error.to_string(),
        "the proxy proxy.example:1080 could not connect to imap.example.com:993: \
         the server refused the connection"
    );
}

#[tokio::test]
async fn socks5_sends_addresses_as_addresses() {
    for (host, address) in [
        ("192.0.2.7", vec![1, 192, 0, 2, 7]),
        (
            "[2001:db8::7]",
            [
                vec![4],
                "2001:db8::7"
                    .parse::<std::net::Ipv6Addr>()
                    .unwrap()
                    .octets()
                    .to_vec(),
            ]
            .concat(),
        ),
        (
            "2001:db8::7",
            [
                vec![4],
                "2001:db8::7"
                    .parse::<std::net::Ipv6Addr>()
                    .unwrap()
                    .octets()
                    .to_vec(),
            ]
            .concat(),
        ),
    ] {
        let expected = [vec![5, 1, 0], address, 993u16.to_be_bytes().to_vec()].concat();
        let (result, _) = tunnel_to(
            server(ProxyKind::Socks5, None),
            host,
            |mut far| async move {
                read_exactly(&mut far, 3).await;
                far.write_all(&[5, 0]).await.unwrap();
                assert_eq!(read_exactly(&mut far, expected.len()).await, expected);
                far.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0])
                    .await
                    .unwrap();
            },
        )
        .await;
        result.unwrap();
    }
}

#[tokio::test]
async fn socks5_sends_international_names_in_ascii() {
    let (result, _) = tunnel_to(
        server(ProxyKind::Socks5, None),
        "bücher.example",
        |mut far| async move {
            read_exactly(&mut far, 3).await;
            far.write_all(&[5, 0]).await.unwrap();
            let request = socks_request("xn--bcher-kva.example");
            assert_eq!(read_exactly(&mut far, request.len()).await, request);
            far.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0])
                .await
                .unwrap();
        },
    )
    .await;
    result.unwrap();
}

#[tokio::test]
async fn socks4a_sends_the_name_and_keeps_the_greeting() {
    let (result, delivered) = tunnel_to(
        server(ProxyKind::Socks4a, None),
        "imap.example.com",
        |mut far| async move {
            let mut request = vec![4, 1, 0x03, 0xe1, 0, 0, 0, 1, 0];
            request.extend_from_slice(b"imap.example.com\0");
            assert_eq!(read_exactly(&mut far, request.len()).await, request);
            let mut reply = vec![0, 90, 0, 0, 0, 0, 0, 0];
            reply.extend_from_slice(GREETING);
            far.write_all(&reply).await.unwrap();
        },
    )
    .await;
    result.unwrap();
    assert_eq!(delivered, GREETING);
}

#[tokio::test]
async fn socks4a_sends_ipv4_addresses_and_explains_a_refusal() {
    let (result, _) = tunnel_to(
        server(ProxyKind::Socks4a, None),
        "192.0.2.7",
        |mut far| async move {
            assert_eq!(
                read_exactly(&mut far, 9).await,
                [4, 1, 0x03, 0xe1, 192, 0, 2, 7, 0]
            );
            far.write_all(&[0, 91, 0, 0, 0, 0, 0, 0]).await.unwrap();
        },
    )
    .await;
    assert!(
        matches!(result, Err(ProxyError::Refused { .. })),
        "{result:?}"
    );
    let (ipv6, _) = tunnel_to(server(ProxyKind::Socks4a, None), "2001:db8::7", |_| async {
    })
    .await;
    assert!(matches!(ipv6, Err(ProxyError::Refused { .. })), "{ipv6:?}");
}

/// Read an HTTP request head from the far end.
async fn request_head(far: &mut DuplexStream) -> String {
    let mut reader = BufReader::new(far);
    let mut head = String::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        head.push_str(&line);
        if line == "\r\n" {
            return head;
        }
    }
}

#[tokio::test]
async fn connect_tunnel_keeps_a_greeting_sent_with_the_reply() {
    let (result, delivered) = tunnel_to(
        server(ProxyKind::Http, None),
        "imap.example.com",
        |mut far| async move {
            let head = request_head(&mut far).await;
            assert!(
                head.starts_with(
                    "CONNECT imap.example.com:993 HTTP/1.1\r\nHost: imap.example.com:993\r\n"
                ),
                "{head}"
            );
            assert!(!head.contains("Proxy-Authorization"), "{head}");
            let mut reply = b"HTTP/1.1 200 Connection established\r\n\r\n".to_vec();
            reply.extend_from_slice(GREETING);
            far.write_all(&reply).await.unwrap();
        },
    )
    .await;
    result.unwrap();
    assert_eq!(delivered, GREETING);
}

#[tokio::test]
async fn connect_accepts_http_1_0_replies_and_bare_line_feeds() {
    let (result, delivered) = tunnel_to(
        server(ProxyKind::Http, None),
        "imap.example.com",
        |mut far| async move {
            request_head(&mut far).await;
            let mut reply = b"HTTP/1.0 200 OK\nVia: 1.1 squid\n\n".to_vec();
            reply.extend_from_slice(GREETING);
            far.write_all(&reply).await.unwrap();
        },
    )
    .await;
    result.unwrap();
    assert_eq!(delivered, GREETING);
}

#[tokio::test]
async fn connect_sends_basic_credentials_and_brackets_ipv6() {
    let (result, _) = tunnel_to(
        server(ProxyKind::Http, Some(("ana", "s3cret"))),
        "2001:db8::7",
        |mut far| async move {
            let head = request_head(&mut far).await;
            assert!(
                head.starts_with("CONNECT [2001:db8::7]:993 HTTP/1.1\r\n"),
                "{head}"
            );
            // base64("ana:s3cret")
            assert!(
                head.contains("\r\nProxy-Authorization: Basic YW5hOnMzY3JldA==\r\n"),
                "{head}"
            );
            far.write_all(b"HTTP/1.1 200 OK\r\n\r\n").await.unwrap();
        },
    )
    .await;
    result.unwrap();
}

async fn connect_error(credentials: Option<(&str, &str)>, reply: &'static str) -> ProxyError {
    let (result, _) = tunnel_to(
        server(ProxyKind::Http, credentials),
        "imap.example.com",
        move |mut far| async move {
            request_head(&mut far).await;
            far.write_all(reply.as_bytes()).await.unwrap();
        },
    )
    .await;
    result.unwrap_err()
}

#[tokio::test]
async fn connect_tells_a_needed_sign_in_from_rejected_credentials() {
    const BASIC: &str = "HTTP/1.1 407 Proxy Authentication Required\r\n\
        Proxy-Authenticate: Basic realm=\"office, main\"\r\n\r\n";
    let needed = connect_error(None, BASIC).await;
    assert!(
        matches!(needed, ProxyError::CredentialsNeeded { .. }),
        "{needed:?}"
    );
    let rejected = connect_error(Some(("ana", "wrong")), BASIC).await;
    assert!(
        matches!(rejected, ProxyError::CredentialsRejected { .. }),
        "{rejected:?}"
    );
}

#[tokio::test]
async fn connect_names_sign_in_methods_it_cannot_use() {
    let error = connect_error(
        Some(("ana", "s3cret")),
        "HTTP/1.1 407 Proxy Authentication Required\r\n\
         Proxy-Authenticate: Negotiate\r\n\
         proxy-authenticate: NTLM\r\n\r\n",
    )
    .await;
    assert!(
        matches!(error, ProxyError::UnsupportedSignIn { ref offered, .. } if offered == "Negotiate, NTLM"),
        "{error:?}"
    );
}

#[tokio::test]
async fn connect_reports_a_refusal_with_its_status() {
    let error = connect_error(None, "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n").await;
    assert_eq!(
        error.to_string(),
        "the proxy proxy.example:1080 could not connect to imap.example.com:993: \
         it answered HTTP/1.1 403 Forbidden"
    );
}

#[tokio::test]
async fn connect_rejects_replies_that_are_not_http() {
    let error = connect_error(None, "SSH-2.0-OpenSSH_9.9\r\n\r\n").await;
    assert!(matches!(error, ProxyError::Protocol { .. }), "{error:?}");
}

#[tokio::test]
async fn connect_gives_up_on_an_endless_reply_head() {
    let (result, _) = tunnel_to(
        server(ProxyKind::Http, None),
        "imap.example.com",
        |mut far| async move {
            request_head(&mut far).await;
            far.write_all(b"HTTP/1.1 200 OK\r\n").await.unwrap();
            let filler = b"X-Filler: 0123456789abcdef0123456789abcdef\r\n".repeat(400);
            // The client stops reading once the head is too long.
            let _ = far.write_all(&filler).await;
        },
    )
    .await;
    assert!(
        matches!(result, Err(ProxyError::Protocol { ref detail, .. }) if detail.contains("too long")),
        "{result:?}"
    );
}

#[tokio::test]
async fn a_proxy_that_hangs_up_is_a_protocol_error() {
    for kind in [ProxyKind::Socks5, ProxyKind::Http] {
        let (result, _) = tunnel_to(server(kind, None), "imap.example.com", |far| async move {
            drop(far);
        })
        .await;
        assert!(
            matches!(
                result,
                Err(ProxyError::Protocol { .. } | ProxyError::Io { .. })
            ),
            "{kind:?}: {result:?}"
        );
    }
}

#[test]
fn rejects_names_a_proxy_cannot_connect_to() {
    assert!(matches!(
        Target::parse("bad name.example"),
        Err(ProxyError::Settings(_))
    ));
    assert!(matches!(Target::parse(""), Err(ProxyError::Settings(_))));
}

#[test]
fn reads_challenge_schemes_around_quoted_commas() {
    assert_eq!(
        proxy_authenticate_schemes(
            "HTTP/1.1 407 x\r\nProxy-Authenticate: Digest realm=\"a, b\", nonce=\"n\", \
             Basic realm=\"c\"\r\nProxy-Authenticate: Negotiate abc==\r\n\r\n"
        ),
        ["Digest", "Basic", "Negotiate"]
    );
}
