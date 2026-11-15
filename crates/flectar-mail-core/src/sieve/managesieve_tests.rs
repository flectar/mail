use super::*;
use crate::sieve::test_server::{Socket, acceptor, read_command, server_at};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

/// One exchange of a scripted server.
enum Step {
    /// Write without waiting for a command, such as the capabilities.
    Send(String),
    /// Read one command, check that it starts with the prefix, then reply.
    Reply(String, String),
    /// Read STARTTLS, accept it, and continue over TLS.
    StartTls,
}

fn send(reply: impl Into<String>) -> Step {
    Step::Send(reply.into())
}

fn reply(command: impl Into<String>, reply: impl Into<String>) -> Step {
    Step::Reply(command.into(), reply.into())
}

/// Capability lines and the OK after them.
fn capabilities(extra: &[&str]) -> String {
    let mut reply = String::from(
        "\"IMPLEMENTATION\" \"Test Sieve\"\r\n\"SIEVE\" \"fileinto vacation date relational include\"\r\n",
    );
    for line in extra {
        reply.push_str(line);
        reply.push_str("\r\n");
    }
    reply.push_str("OK \"ready\"\r\n");
    reply
}

/// A ManageSieve server that replays `steps` and records every command.
async fn server(steps: Vec<Step>) -> (u16, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let received = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&received);
    tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut socket: BufReader<Box<dyn Socket>> = BufReader::new(Box::new(tcp));
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
                    assert_eq!(line.as_deref(), Some("STARTTLS"));
                    socket
                        .get_mut()
                        .write_all(b"OK \"Begin TLS\"\r\n")
                        .await
                        .unwrap();
                    let tls = acceptor().accept(socket.into_inner()).await.unwrap();
                    socket = BufReader::new(Box::new(tls));
                }
            }
        }
        while read_command(&mut socket, &log).await.is_some() {}
    });
    (port, received)
}

async fn signed_in_session(port: u16) -> Session {
    let mut session = Session::open(&server_at(port)).await.unwrap();
    session.sign_in("me@example.test", "secret").await.unwrap();
    session
}

/// The capabilities, STARTTLS, the capabilities again over TLS with SASL,
/// and a PLAIN sign-in.
fn signed_in(extra: &[&str]) -> Vec<Step> {
    let mut after_tls = vec!["\"SASL\" \"PLAIN OAUTHBEARER\""];
    after_tls.extend_from_slice(extra);
    vec![
        send(capabilities(&["\"STARTTLS\""])),
        Step::StartTls,
        send(capabilities(&after_tls)),
        reply("AUTHENTICATE \"PLAIN\"", "OK \"Logged in\"\r\n"),
    ]
}

#[tokio::test]
async fn scripts_are_listed_read_stored_and_activated_over_tls() {
    let mut steps = signed_in(&[]);
    steps.extend([
        reply(
            "LISTSCRIPTS",
            "\"roundcube\" ACTIVE\r\n{12}\r\nflectar-mail\r\nOK\r\n",
        ),
        reply(
            "GETSCRIPT \"roundcube\"",
            "{19}\r\nrequire \"fileinto\";\r\nOK\r\n",
        ),
        reply(
            "PUTSCRIPT \"flectar-mail\"",
            "OK (WARNINGS) \"line 2: :days is clamped to 7\"\r\n",
        ),
        reply("SETACTIVE \"flectar-mail\"", "OK \"activated\"\r\n"),
        reply("LOGOUT", "OK \"Bye\"\r\n"),
    ]);
    let (port, received) = server(steps).await;
    let mut session = signed_in_session(port).await;
    // The capabilities sent again over TLS are the ones that count.
    assert_eq!(session.capabilities().sasl, ["PLAIN", "OAUTHBEARER"]);
    assert!(session.capabilities().has_extension("Vacation"));
    assert_eq!(session.capabilities().implementation, "Test Sieve");

    assert_eq!(
        session.list_scripts().await.unwrap(),
        [
            Script {
                name: "roundcube".into(),
                active: true,
            },
            Script {
                name: "flectar-mail".into(),
                active: false,
            },
        ]
    );
    assert_eq!(
        session.get_script("roundcube").await.unwrap(),
        "require \"fileinto\";"
    );
    let script = "require \"vacation\";\r\nvacation :days 9 \"Away\";\r\n";
    assert_eq!(
        session
            .put_script("flectar-mail", script)
            .await
            .unwrap()
            .as_deref(),
        Some("line 2: :days is clamped to 7")
    );
    session.set_active("flectar-mail").await.unwrap();
    session.logout().await.unwrap();

    let received = received.lock().unwrap().clone();
    assert_eq!(received[0], "STARTTLS");
    let plain = base64::engine::general_purpose::STANDARD.encode("\0me@example.test\0secret");
    assert_eq!(received[1], format!("AUTHENTICATE \"PLAIN\" \"{plain}\""));
    assert_eq!(
        received[4],
        format!("PUTSCRIPT \"flectar-mail\" {{{}+}}\n{script}", script.len())
    );
}

#[tokio::test]
async fn refusals_carry_their_code_and_text() {
    let mut steps = signed_in(&[]);
    steps.push(reply(
        "PUTSCRIPT",
        "NO (QUOTA/MAXSIZE) \"Script is too big\"\r\n",
    ));
    let (port, _) = server(steps).await;
    let mut session = signed_in_session(port).await;
    let refused = session.put_script("flectar-mail", "keep;").await;
    match refused {
        Err(Error::Refused {
            command: "PUTSCRIPT",
            code: Some(code),
            text,
        }) => {
            assert_eq!(code, "QUOTA/MAXSIZE");
            assert_eq!(text, "Script is too big");
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn credentials_never_travel_without_tls() {
    // No STARTTLS on a plain connection: the client gives up unsigned in.
    let (port, received) = server(vec![send(capabilities(&["\"SASL\" \"PLAIN\""]))]).await;
    assert!(matches!(
        Session::open(&server_at(port)).await,
        Err(Error::NoSignIn)
    ));
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(received.lock().unwrap().is_empty(), "nothing was sent");
}

#[tokio::test]
async fn an_unencrypted_account_signs_in_as_its_mail_does() {
    // The user chose no encryption for this account's mail, so the client
    // doesn't upgrade, and a long password goes as a literal.
    let password = "p".repeat(800);
    let (port, received) = server(vec![
        send(capabilities(&["\"STARTTLS\"", "\"SASL\" \"PLAIN\""])),
        reply("AUTHENTICATE \"PLAIN\" {", "OK\r\n"),
    ])
    .await;
    let mut session = Session::open(&Server {
        security: ConnectionSecurity::None,
        ..server_at(port)
    })
    .await
    .unwrap();
    session.sign_in("me", &password).await.unwrap();
    let plain = base64::engine::general_purpose::STANDARD.encode(format!("\0me\0{password}"));
    assert_eq!(
        received.lock().unwrap()[0],
        format!("AUTHENTICATE \"PLAIN\" {{{}+}}\n{plain}", plain.len())
    );
}

#[tokio::test]
async fn a_challenge_after_plain_is_cancelled() {
    let mut steps = signed_in(&[]);
    steps.pop();
    steps.extend([
        reply("AUTHENTICATE \"PLAIN\"", "\"\"\r\n"),
        reply("\"*\"", "NO \"Authentication cancelled\"\r\n"),
    ]);
    let (port, _) = server(steps).await;
    let mut session = Session::open(&server_at(port)).await.unwrap();
    assert!(matches!(
        session.sign_in("me@example.test", "secret").await,
        Err(Error::Refused {
            command: "AUTHENTICATE",
            ..
        })
    ));
}

#[test]
fn names_are_quoted_and_never_carry_control_characters() {
    assert_eq!(quoted("a \"b\" \\c").unwrap(), "\"a \\\"b\\\" \\\\c\"");
    assert!(quoted("a\r\nLOGOUT").is_err());
}
