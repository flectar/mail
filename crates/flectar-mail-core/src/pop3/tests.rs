use super::*;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

const CERT: &str = include_str!("../../tests/fixtures/tls/server.pem");
const KEY: &str = include_str!("../../tests/fixtures/tls/server-key.pem");

/// One exchange of a scripted server.
enum Step {
    /// Write without waiting for a command (greetings, pipelined replies).
    Send(String),
    /// Read one command, check that it starts with the prefix, then reply.
    Reply(String, String),
    /// Read `STLS`, accept it, and continue the script over TLS.
    StartTls,
    /// Hang up.
    Close,
}

fn send(reply: impl Into<String>) -> Step {
    Step::Send(reply.into())
}

fn reply(command: impl Into<String>, reply: impl Into<String>) -> Step {
    Step::Reply(command.into(), reply.into())
}

fn capa(lines: &[&str]) -> String {
    let mut reply = String::from("+OK capability list follows\r\n");
    for line in lines {
        reply.push_str(line);
        reply.push_str("\r\n");
    }
    reply.push_str(".\r\n");
    reply
}

fn acceptor() -> tokio_rustls::TlsAcceptor {
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![CertificateDer::from_pem_slice(CERT.as_bytes()).unwrap()],
        PrivateKeyDer::from_pem_slice(KEY.as_bytes()).unwrap(),
    )
    .unwrap();
    tokio_rustls::TlsAcceptor::from(Arc::new(config))
}

trait Socket: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> Socket for T {}

/// The next command line, recorded, or `None` once the client hung up.
async fn read_command(
    socket: &mut BufReader<Box<dyn Socket>>,
    log: &Mutex<Vec<String>>,
) -> Option<String> {
    let mut line = String::new();
    if socket.read_line(&mut line).await.unwrap_or(0) == 0 {
        return None;
    }
    let line = line.trim_end_matches(['\r', '\n']).to_owned();
    log.lock().unwrap().push(line.clone());
    Some(line)
}

/// A POP3 server that replays `steps`, optionally over TLS from the start,
/// and records every command line it receives. The task ends when the client
/// hangs up.
async fn server(
    tls: bool,
    steps: Vec<Step>,
) -> (u16, Arc<Mutex<Vec<String>>>, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let received = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&received);
    let task = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut socket: BufReader<Box<dyn Socket>> = if tls {
            BufReader::new(Box::new(acceptor().accept(tcp).await.unwrap()))
        } else {
            BufReader::new(Box::new(tcp))
        };
        for step in steps {
            match step {
                Step::Send(text) => socket.get_mut().write_all(text.as_bytes()).await.unwrap(),
                Step::Reply(expected, text) => {
                    let Some(line) = read_command(&mut socket, &log).await else {
                        return;
                    };
                    assert!(
                        line.starts_with(&expected),
                        "expected {expected:?}, got {line:?}"
                    );
                    socket.get_mut().write_all(text.as_bytes()).await.unwrap();
                }
                Step::StartTls => {
                    let line = read_command(&mut socket, &log).await;
                    assert_eq!(line.as_deref(), Some("STLS"));
                    socket
                        .get_mut()
                        .write_all(b"+OK begin TLS\r\n")
                        .await
                        .unwrap();
                    let tls = acceptor().accept(socket.into_inner()).await.unwrap();
                    socket = BufReader::new(Box::new(tls));
                }
                Step::Close => return,
            }
        }
        // Record whatever the client sends until it hangs up.
        while read_command(&mut socket, &log).await.is_some() {}
    });
    (port, received, task)
}

fn server_at(port: u16, security: ConnectionSecurity) -> Server<'static> {
    Server {
        host: "127.0.0.1",
        port,
        security,
        trusted_certificate_pem: CERT,
    }
}

/// Greeting, CAPA, and USER/PASS sign-in, then CAPA again.
fn signed_in(capabilities: &[&str]) -> Vec<Step> {
    vec![
        send("+OK POP3 ready\r\n"),
        reply("CAPA", capa(capabilities)),
        reply("USER me", "+OK\r\n"),
        reply("PASS pw", "+OK maildrop locked\r\n"),
        reply("CAPA", capa(capabilities)),
    ]
}

async fn connect(port: u16) -> Session {
    Session::connect(&server_at(port, ConnectionSecurity::None), "me", "pw")
        .await
        .unwrap()
}

#[tokio::test]
async fn lists_the_maildrop_and_ends_with_quit() {
    let mut steps = signed_in(&["USER", "UIDL", "TOP"]);
    steps.extend([
        reply(
            "UIDL",
            "+OK\r\n1 whqtswO00WBw418f9t5JxYwZ\r\n2 QhdPYR:00WBw1Ph7x7\r\n.\r\n",
        ),
        reply("LIST", "+OK 2 messages\r\n1 120\r\n2 200 extra\r\n.\r\n"),
        reply("QUIT", "+OK bye\r\n"),
    ]);
    let (port, received, task) = server(false, steps).await;
    let mut session = connect(port).await;
    assert!(session.capabilities().uidl && session.capabilities().top);
    assert_eq!(
        session.unique_ids().await.unwrap(),
        [
            (1, "whqtswO00WBw418f9t5JxYwZ".to_owned()),
            (2, "QhdPYR:00WBw1Ph7x7".to_owned()),
        ]
    );
    assert_eq!(session.sizes().await.unwrap(), [(1, 120), (2, 200)]);
    session.quit().await.unwrap();
    task.await.unwrap();
    assert_eq!(received.lock().unwrap().last().unwrap(), "QUIT");
}

#[tokio::test]
async fn per_user_capabilities_come_from_the_second_capa() {
    let (port, _, _task) = server(
        false,
        vec![
            send("+OK ready\r\n"),
            reply(
                "CAPA",
                capa(&["USER", "EXPIRE 30 USER", "LOGIN-DELAY 900 USER"]),
            ),
            reply("USER me", "+OK\r\n"),
            reply("PASS pw", "+OK\r\n"),
            reply("CAPA", capa(&["USER", "EXPIRE 0", "LOGIN-DELAY 300"])),
        ],
    )
    .await;
    let session = connect(port).await;
    assert_eq!(session.capabilities().expire, Some(Expire::Days(0)));
    assert_eq!(
        session.capabilities().login_delay,
        Some(Duration::from_secs(300))
    );
}

#[tokio::test]
async fn short_sasl_plain_credentials_travel_with_the_command() {
    let encoded = base64::engine::general_purpose::STANDARD.encode("\0me\0pw");
    let (port, received, task) = server(
        false,
        vec![
            send("+OK ready\r\n"),
            reply("CAPA", capa(&["SASL PLAIN LOGIN", "USER"])),
            reply("AUTH PLAIN ", "+OK\r\n"),
            reply("CAPA", "-ERR\r\n"),
        ],
    )
    .await;
    connect(port).await;
    task.await.unwrap();
    assert_eq!(received.lock().unwrap()[1], format!("AUTH PLAIN {encoded}"));
}

#[tokio::test]
async fn long_sasl_plain_credentials_follow_as_a_continuation() {
    let password = "p".repeat(300);
    let encoded = base64::engine::general_purpose::STANDARD.encode(format!("\0me\0{password}"));
    let (port, received, task) = server(
        false,
        vec![
            send("+OK ready\r\n"),
            reply("CAPA", capa(&["SASL PLAIN"])),
            reply("AUTH PLAIN", "+ \r\n"),
            reply(encoded.clone(), "+OK\r\n"),
            reply("CAPA", capa(&["SASL PLAIN"])),
        ],
    )
    .await;
    Session::connect(&server_at(port, ConnectionSecurity::None), "me", &password)
        .await
        .unwrap();
    task.await.unwrap();
    let received = received.lock().unwrap();
    assert_eq!(received[1], "AUTH PLAIN");
    assert_eq!(received[2], encoded);
}

#[tokio::test]
async fn servers_without_capa_get_user_and_pass() {
    let (port, _, _task) = server(
        false,
        vec![
            send("+OK ready\r\n"),
            reply("CAPA", "-ERR unknown command\r\n"),
            reply("USER me", "+OK\r\n"),
            reply("PASS pw", "+OK\r\n"),
            reply("CAPA", "-ERR unknown command\r\n"),
        ],
    )
    .await;
    assert!(!connect(port).await.capabilities().announced);
}

#[tokio::test]
async fn utf8_mode_is_requested_before_signing_in() {
    for answer in ["+OK UTF8 enabled\r\n", "-ERR not available\r\n"] {
        let (port, received, task) = server(
            false,
            vec![
                send("+OK ready\r\n"),
                reply("CAPA", capa(&["USER", "UTF8 USER"])),
                reply("UTF8", answer),
                reply("USER me", "+OK\r\n"),
                reply("PASS contraseña", "+OK\r\n"),
                reply("CAPA", capa(&["USER"])),
            ],
        )
        .await;
        Session::connect(
            &server_at(port, ConnectionSecurity::None),
            "me",
            "contraseña",
        )
        .await
        .unwrap();
        task.await.unwrap();
        assert_eq!(received.lock().unwrap()[1], "UTF8", "{answer}");
    }
}

#[tokio::test]
async fn a_server_that_withholds_passwords_on_plaintext_is_not_sent_one() {
    let (port, received, task) = server(
        false,
        vec![
            send("+OK ready\r\n"),
            reply("CAPA", capa(&["STLS", "UIDL", "SASL"])),
        ],
    )
    .await;
    let error = Session::connect(&server_at(port, ConnectionSecurity::None), "me", "pw")
        .await
        .err()
        .unwrap();
    assert!(matches!(error, Error::NoPasswordSignIn), "{error}");
    task.await.unwrap();
    assert_eq!(received.lock().unwrap().as_slice(), ["CAPA"]);
}

async fn refusal(port: u16, password: &str) -> SignInRefusal {
    match Session::connect(&server_at(port, ConnectionSecurity::None), "me", password).await {
        Err(Error::SignIn { refusal, .. }) => refusal,
        other => panic!("unexpected {:?}", other.err()),
    }
}

#[tokio::test]
async fn refused_sign_ins_are_classified_by_their_response_code() {
    const CODED: &[&str] = &["USER", "RESP-CODES", "AUTH-RESP-CODE"];
    for (capabilities, answer, expected) in [
        (CODED, "-ERR [AUTH] invalid\r\n", SignInRefusal::Credentials),
        (CODED, "-ERR [IN-USE] locked\r\n", SignInRefusal::InUse),
        (
            CODED,
            "-ERR [LOGIN-DELAY] wait\r\n",
            SignInRefusal::LoginDelay,
        ),
        (CODED, "-ERR [SYS/TEMP] later\r\n", SignInRefusal::Temporary),
        (CODED, "-ERR [SYS/PERM] gone\r\n", SignInRefusal::Permanent),
        // AUTH-RESP-CODE rules out the credentials when AUTH is missing.
        (CODED, "-ERR no\r\n", SignInRefusal::Temporary),
        (CODED, "-ERR [X-ODD] no\r\n", SignInRefusal::Temporary),
        (&["USER"], "-ERR no\r\n", SignInRefusal::Unspecified),
        (
            &["USER"],
            "-ERR [AUTH] invalid\r\n",
            SignInRefusal::Credentials,
        ),
    ] {
        let (port, _, _task) = server(
            false,
            vec![
                send("+OK ready\r\n"),
                reply("CAPA", capa(capabilities)),
                reply("USER me", "+OK\r\n"),
                reply("PASS pw", answer),
            ],
        )
        .await;
        assert_eq!(refusal(port, "pw").await, expected, "{answer}");
    }
}

#[tokio::test]
async fn refused_sasl_sign_ins_are_classified() {
    let (port, _, _task) = server(
        false,
        vec![
            send("+OK ready\r\n"),
            reply("CAPA", capa(&["SASL PLAIN", "AUTH-RESP-CODE"])),
            reply("AUTH PLAIN ", "-ERR [AUTH] invalid\r\n"),
        ],
    )
    .await;
    assert_eq!(refusal(port, "pw").await, SignInRefusal::Credentials);

    // Refused before the long credentials were sent.
    let (port, received, task) = server(
        false,
        vec![
            send("+OK ready\r\n"),
            reply("CAPA", capa(&["SASL PLAIN"])),
            reply("AUTH PLAIN", "-ERR [SYS/TEMP] later\r\n"),
        ],
    )
    .await;
    assert_eq!(
        refusal(port, &"p".repeat(300)).await,
        SignInRefusal::Temporary
    );
    task.await.unwrap();
    assert_eq!(received.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn implicit_tls_verifies_the_account_certificate() {
    let (port, _, _task) = server(true, signed_in(&["USER"])).await;
    Session::connect(&server_at(port, ConnectionSecurity::Tls), "me", "pw")
        .await
        .unwrap();

    let (port, _, _task) = server(true, signed_in(&["USER"])).await;
    let untrusted = Server {
        trusted_certificate_pem: "",
        ..server_at(port, ConnectionSecurity::Tls)
    };
    assert!(matches!(
        Session::connect(&untrusted, "me", "pw").await,
        Err(Error::Tls(_))
    ));
}

#[tokio::test]
async fn stls_upgrades_before_any_credentials() {
    let mut steps = vec![send("+OK ready\r\n"), Step::StartTls];
    steps.extend(signed_in(&["USER"]).into_iter().skip(1));
    let (port, received, task) = server(false, steps).await;
    Session::connect(&server_at(port, ConnectionSecurity::Starttls), "me", "pw")
        .await
        .unwrap();
    task.await.unwrap();
    assert_eq!(received.lock().unwrap()[0], "STLS");
}

#[tokio::test]
async fn a_refused_stls_never_falls_back_to_plaintext() {
    let (port, received, task) = server(
        false,
        vec![
            send("+OK ready\r\n"),
            reply("STLS", "-ERR TLS not available\r\n"),
        ],
    )
    .await;
    let error = Session::connect(&server_at(port, ConnectionSecurity::Starttls), "me", "pw")
        .await
        .err()
        .unwrap();
    assert!(
        matches!(
            error,
            Error::Refused {
                command: "STLS",
                ..
            }
        ),
        "{error}"
    );
    task.await.unwrap();
    assert!(
        !received
            .lock()
            .unwrap()
            .iter()
            .any(|line| line.starts_with("USER")),
        "credentials sent after a refused upgrade"
    );
}

#[tokio::test]
async fn stls_refuses_data_sent_before_the_handshake() {
    let (port, received, task) = server(
        false,
        vec![
            send("+OK ready\r\n"),
            reply("STLS", "+OK begin TLS\r\n+OK injected\r\n"),
        ],
    )
    .await;
    let error = Session::connect(&server_at(port, ConnectionSecurity::Starttls), "me", "pw")
        .await
        .err()
        .unwrap();
    assert!(matches!(error, Error::Protocol(_)), "{error}");
    task.await.unwrap();
    assert_eq!(received.lock().unwrap().as_slice(), ["STLS"]);
}

#[tokio::test]
async fn messages_keep_their_bytes_and_lose_only_dot_stuffing() {
    let message = "From: a@example.com\r\n\r\n..leading dot\r\nbare\nline\r\n.\r\n";
    let mut steps = signed_in(&["USER"]);
    steps.push(reply("RETR 1", format!("+OK 60 octets\r\n{message}")));
    let (port, _, _task) = server(false, steps).await;
    let mut session = connect(port).await;
    assert_eq!(
        session.retrieve(1, 1024).await.unwrap(),
        b"From: a@example.com\r\n\r\n.leading dot\r\nbare\nline\r\n"
    );
}

#[tokio::test]
async fn an_oversized_message_ends_the_session_without_quit() {
    let mut steps = signed_in(&["USER"]);
    steps.push(reply(
        "RETR 1",
        format!("+OK\r\n{}\r\n.\r\n", "x".repeat(200)),
    ));
    let (port, received, task) = server(false, steps).await;
    let mut session = connect(port).await;
    assert!(matches!(
        session.retrieve(1, 100).await,
        Err(Error::TooLarge(100))
    ));
    assert!(matches!(session.delete(1).await, Err(Error::Ended)));
    assert!(matches!(session.quit().await, Err(Error::Ended)));
    task.await.unwrap();
    assert!(!received.lock().unwrap().iter().any(|line| line == "QUIT"));
}

#[tokio::test]
async fn pipelined_headers_survive_a_refused_message() {
    let mut steps = signed_in(&["USER", "TOP", "PIPELINING"]);
    steps.extend([
        reply("TOP 1 0", ""),
        reply("TOP 2 0", ""),
        reply(
            "TOP 3 0",
            "+OK\r\nSubject: one\r\n\r\n.\r\n-ERR no such message\r\n+OK\r\nSubject: three\r\n\r\n.\r\n",
        ),
    ]);
    let (port, _, _task) = server(false, steps).await;
    let mut session = connect(port).await;
    assert_eq!(
        session.headers(&[1, 2, 3], 1024).await.unwrap(),
        [
            Some(b"Subject: one\r\n\r\n".to_vec()),
            None,
            Some(b"Subject: three\r\n\r\n".to_vec()),
        ]
    );
}

#[tokio::test]
async fn an_oversized_header_section_is_skipped_and_the_session_continues() {
    let mut steps = signed_in(&["USER", "TOP"]);
    let large = format!("X-Large: {}\r\n\r\n", "y".repeat(2000));
    steps.extend([
        reply("TOP 1 0", format!("+OK\r\n{large}.\r\n")),
        reply("TOP 2 0", "+OK\r\nSubject: two\r\n\r\n.\r\n"),
        reply("QUIT", "+OK\r\n"),
    ]);
    let (port, received, task) = server(false, steps).await;
    let mut session = connect(port).await;
    assert_eq!(
        session.headers(&[1, 2], 1024).await.unwrap(),
        [None, Some(b"Subject: two\r\n\r\n".to_vec())]
    );
    session.quit().await.unwrap();
    task.await.unwrap();
    assert_eq!(received.lock().unwrap().last().unwrap(), "QUIT");
}

#[tokio::test]
async fn deletions_are_committed_by_quit() {
    let mut steps = signed_in(&["USER"]);
    steps.extend([
        reply("DELE 2", "+OK marked\r\n"),
        reply("QUIT", "-ERR some deleted messages not removed\r\n"),
    ]);
    let (port, _, _task) = server(false, steps).await;
    let mut session = connect(port).await;
    session.delete(2).await.unwrap();
    assert!(matches!(
        session.quit().await,
        Err(Error::Refused {
            command: "QUIT",
            ..
        })
    ));
}

#[tokio::test]
async fn credentials_that_cannot_travel_are_never_sent() {
    let long = "p".repeat(300);
    for (capabilities, password) in [
        (&["USER", "SASL PLAIN"][..], "pw\r\nDELE 1"),
        (&["USER", "SASL PLAIN"], "pw\0"),
        // USER and PASS must fit 255 octets; only SASL can carry more.
        (&["USER"], long.as_str()),
    ] {
        let (port, received, task) = server(
            false,
            vec![send("+OK ready\r\n"), reply("CAPA", capa(capabilities))],
        )
        .await;
        let error = Session::connect(&server_at(port, ConnectionSecurity::None), "me", password)
            .await
            .err()
            .unwrap();
        assert!(matches!(error, Error::UnsendableCredentials), "{error}");
        task.await.unwrap();
        assert_eq!(received.lock().unwrap().as_slice(), ["CAPA"]);
    }
}

#[tokio::test]
async fn malformed_unique_ids_are_rejected() {
    for listing in ["0 abc", "1 ", "x abc", "1 has space", "1 \u{e9}t\u{e9}"] {
        let mut steps = signed_in(&["USER", "UIDL"]);
        steps.push(reply("UIDL", format!("+OK\r\n{listing}\r\n.\r\n")));
        let (port, _, _task) = server(false, steps).await;
        let mut session = connect(port).await;
        assert!(session.unique_ids().await.is_err(), "{listing:?}");
    }
}

/// A deterministic xorshift sequence for the randomized tests.
fn random(mut state: u64) -> impl FnMut() -> u64 {
    move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    }
}

/// Arbitrary lines must parse or fail, never panic, and accepted listings
/// must hold valid values.
#[test]
fn arbitrary_lines_never_panic_the_parsers() {
    const PIECES: [&[u8]; 16] = [
        b"+OK", b"-ERR", b"[AUTH]", b"[", b"]", b" ", b"\t", b"0", b"1", b"99999", b"SASL",
        b"EXPIRE", b"NEVER", b"USER", b"\xe9", b"\xff",
    ];
    let mut next = random(0x2545_f491_4f6c_dd1d);
    for _ in 0..20_000 {
        let mut line = Vec::new();
        for _ in 0..next() % 12 {
            if next().is_multiple_of(4) {
                line.push(next() as u8);
            } else {
                line.extend_from_slice(PIECES[(next() % PIECES.len() as u64) as usize]);
            }
        }
        response::parse_status(&line);
        if let Ok((number, id)) = response::parse_unique_id(&line) {
            assert!(number > 0 && (1..=70).contains(&id.len()), "{line:?}");
        }
        if let Ok((number, _)) = response::parse_size(&line) {
            assert!(number > 0, "{line:?}");
        }
        Capabilities::parse(&[line]);
    }
}

/// Arbitrary server bytes must end in an answer or an error, never a panic
/// or a hang, and never more data than allowed.
#[tokio::test]
async fn arbitrary_responses_never_panic_hang_or_overrun_limits() {
    const PIECES: [&str; 13] = [
        "+OK", "-ERR", "[IN-USE]", ".", "..", "\r\n", "\n", "1 ", "abc", " ", "\r", "+ ", "\0",
    ];
    let mut next = random(0x9e37_79b9_7f4a_7c15);
    for round in 0..200 {
        let mut garbage = String::new();
        for _ in 0..next() % 40 {
            garbage.push_str(PIECES[(next() % PIECES.len() as u64) as usize]);
        }
        let command = ["RETR 1", "TOP 1 0", "UIDL", "LIST"][round % 4];
        let mut steps = signed_in(&["USER", "TOP", "UIDL"]);
        steps.extend([reply(command, garbage.clone()), Step::Close]);
        let (port, _, _task) = server(false, steps).await;
        let mut session = connect(port).await;
        let finished = tokio::time::timeout(Duration::from_secs(5), async {
            match round % 4 {
                0 => {
                    if let Ok(data) = session.retrieve(1, 64).await {
                        assert!(data.len() <= 64, "{garbage:?}");
                    }
                }
                1 => {
                    if let Ok(sections) = session.headers(&[1], 64).await {
                        assert!(sections.iter().flatten().all(|section| section.len() <= 64));
                    }
                }
                2 => drop(session.unique_ids().await),
                _ => drop(session.sizes().await),
            }
        })
        .await;
        assert!(finished.is_ok(), "hung on {command} with {garbage:?}");
    }
}
