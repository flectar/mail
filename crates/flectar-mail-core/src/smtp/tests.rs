use super::*;
use crate::models::{AuthKind, MailConnectionSettings, MailProtocol, Provider};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

/// How the stand-in server treats a message.
#[derive(Clone, Copy)]
enum End {
    /// It takes the message.
    Accept,
    /// It refuses the finished message.
    Refuse,
    /// It closes the connection once the message ended, without a reply.
    Vanish,
    /// It closes the connection halfway through the message.
    Drop,
    /// It stops reading the message for a while, then reads on.
    Stall,
}

/// A plain-text SMTP server that signs anyone in. For each message it
/// reports whether the message reached its end-of-data marker.
async fn server(end: End) -> (u16, mpsc::UnboundedReceiver<bool>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (ended, seen) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut stream = BufReader::new(tcp);
        stream.write_all(b"220 localhost ESMTP\r\n").await.unwrap();
        loop {
            let mut line = String::new();
            if stream.read_line(&mut line).await.unwrap_or(0) == 0 {
                return;
            }
            let verb = line.split([' ', '\r']).next().unwrap_or_default();
            let reply: &[u8] = match verb.to_ascii_uppercase().as_str() {
                "EHLO" => b"250-localhost\r\n250 AUTH PLAIN\r\n",
                "AUTH" => b"235 Authenticated\r\n",
                "DATA" => {
                    stream.write_all(b"354 Continue\r\n").await.unwrap();
                    if matches!(end, End::Stall) {
                        tokio::time::sleep(Duration::from_secs(2)).await;
                    }
                    let mut lines = 0;
                    loop {
                        let mut next = String::new();
                        if stream.read_line(&mut next).await.unwrap_or(0) == 0 {
                            let _ = ended.send(false);
                            return;
                        }
                        if next == ".\r\n" {
                            break;
                        }
                        lines += 1;
                        if matches!(end, End::Drop) && lines == 2 {
                            let _ = ended.send(false);
                            return;
                        }
                    }
                    let _ = ended.send(true);
                    match end {
                        End::Refuse => b"554 5.7.1 Message refused\r\n",
                        End::Vanish => return,
                        _ => b"250 Queued\r\n",
                    }
                }
                "QUIT" => {
                    let _ = stream.write_all(b"221 Goodbye\r\n").await;
                    return;
                }
                _ => b"250 OK\r\n",
            };
            if stream.write_all(reply).await.is_err() {
                return;
            }
        }
    });
    (port, seen)
}

fn account(port: u16) -> AccountConfig {
    let mut config = AccountConfig {
        id: 1,
        email: "me@example.test".into(),
        display_name: None,
        name: None,
        avatar_url: None,
        provider: Provider::Generic,
        auth_kind: AuthKind::Password,
        mail_protocol: MailProtocol::Imap,
        username: "me".into(),
        jmap_url: String::new(),
        jmap_account_id: None,
        incoming_host: "127.0.0.1".into(),
        incoming_port: 993,
        smtp_host: "127.0.0.1".into(),
        smtp_port: port,
        settings: crate::db::testutil::account_settings(),
    };
    config.settings.connection =
        MailConnectionSettings::new(ConnectionSecurity::Tls, ConnectionSecurity::None);
    config
}

fn message(lines: usize) -> Vec<u8> {
    let mut raw =
        b"From: me@example.test\r\nTo: alice@example.test\r\nSubject: Monday plans\r\n\r\n"
            .to_vec();
    for _ in 0..lines {
        raw.extend_from_slice(&[b'x'; 76]);
        raw.extend_from_slice(b"\r\n");
    }
    raw
}

async fn send(end: End, raw: &[u8], deadline: Option<i64>) -> (Result<()>, Vec<bool>) {
    let (port, mut seen) = server(end).await;
    let result = send_raw_before(
        &account(port),
        &SmtpAuth::Password("secret".into()),
        "me@example.test",
        &["alice@example.test".into()],
        raw,
        deadline,
    )
    .await;
    // The server reports once the client has gone.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut ends = Vec::new();
    while let Ok(ended) = seen.try_recv() {
        ends.push(ended);
    }
    (result, ends)
}

#[tokio::test]
async fn an_accepted_message_is_sent() {
    let (result, ends) = send(End::Accept, &message(3), None).await;
    result.unwrap();
    assert_eq!(ends, [true]);
}

#[tokio::test]
async fn a_refused_message_was_not_sent() {
    let (result, ends) = send(End::Refuse, &message(3), None).await;
    match result {
        Err(CoreError::Smtp(error)) => {
            assert!(crate::queue::has_permanent_smtp_reply(&error), "{error}");
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert_eq!(ends, [true]);
}

#[tokio::test]
async fn a_reply_lost_after_the_message_leaves_it_uncertain() {
    let (result, ends) = send(End::Vanish, &message(3), None).await;
    assert!(
        matches!(result, Err(CoreError::SendUncertain(_))),
        "{result:?}"
    );
    assert_eq!(ends, [true]);
}

#[tokio::test]
async fn a_connection_lost_mid_message_was_not_sent() {
    // Far more than the connection buffers hold, so the client is still
    // writing when the server hangs up. A message that fit in them would
    // have gone out whole first, and then a missing reply is all the client
    // could know of.
    let (result, ends) = send(End::Drop, &message(300_000), None).await;
    assert!(matches!(result, Err(CoreError::Smtp(_))), "{result:?}");
    assert_eq!(ends, [false]);
}

#[tokio::test]
async fn a_message_not_out_by_its_deadline_is_never_finished() {
    // Far more than the connection buffers hold, so the client still has
    // most of it to write when the server stops reading, and the deadline
    // passes before the server reads on.
    let raw = message(300_000);
    let deadline = crate::models::now_ms() + 500;
    let (result, ends) = send(End::Stall, &raw, Some(deadline)).await;
    assert!(matches!(result, Err(CoreError::Smtp(_))), "{result:?}");
    assert_eq!(ends, [false]);
}

#[tokio::test]
async fn a_send_already_past_its_deadline_writes_nothing() {
    let deadline = crate::models::now_ms() - 1;
    let (result, ends) = send(End::Accept, &message(3), Some(deadline)).await;
    assert!(matches!(result, Err(CoreError::Smtp(_))), "{result:?}");
    assert!(ends.is_empty());
}
