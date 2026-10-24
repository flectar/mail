//! POP3 accounts through the public API, against in-process POP3 and SMTP
//! servers: setup, download, local folders and actions, and sending.

use flectar_mail_core::Core;
use flectar_mail_core::accounts::credentials::DevelopmentFileCredentialStore;
use flectar_mail_core::config::Paths;
use flectar_mail_core::models::*;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

/// A POP3 server holding `maildrop`, recording every command it receives.
async fn pop3_server(maildrop: Vec<(String, String)>) -> (u16, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let commands = Arc::new(Mutex::new(Vec::new()));
    let log = commands.clone();
    tokio::spawn(async move {
        loop {
            let (tcp, _) = listener.accept().await.unwrap();
            let (read, mut write) = tcp.into_split();
            let mut reader = BufReader::new(read);
            write.write_all(b"+OK ready\r\n").await.unwrap();
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                    break;
                }
                let line = line.trim_end().to_owned();
                log.lock().unwrap().push(line.clone());
                let mut words = line.split(' ');
                let command = words.next().unwrap_or_default().to_ascii_uppercase();
                let message = words
                    .next()
                    .and_then(|number| number.parse::<usize>().ok())
                    .and_then(|number| maildrop.get(number.checked_sub(1)?));
                let reply = match (command.as_str(), message) {
                    ("CAPA", _) => "+OK\r\nUSER\r\nUIDL\r\nTOP\r\n.\r\n".to_owned(),
                    ("USER" | "PASS", _) => "+OK\r\n".to_owned(),
                    ("UIDL", _) => {
                        let mut reply = String::from("+OK\r\n");
                        for (index, (uidl, _)) in maildrop.iter().enumerate() {
                            reply.push_str(&format!("{} {uidl}\r\n", index + 1));
                        }
                        reply + ".\r\n"
                    }
                    ("LIST", _) => {
                        let mut reply = String::from("+OK\r\n");
                        for (index, (_, raw)) in maildrop.iter().enumerate() {
                            reply.push_str(&format!("{} {}\r\n", index + 1, raw.len()));
                        }
                        reply + ".\r\n"
                    }
                    ("RETR", Some((_, raw))) => format!("+OK\r\n{raw}.\r\n"),
                    ("QUIT", _) => {
                        let _ = write.write_all(b"+OK bye\r\n").await;
                        break;
                    }
                    _ => "-ERR unsupported\r\n".to_owned(),
                };
                if write.write_all(reply.as_bytes()).await.is_err() {
                    break;
                }
            }
        }
    });
    (port, commands)
}

/// An SMTP server that accepts every sign-in and message, handing each
/// delivered message to the returned channel.
async fn smtp_server() -> (u16, tokio::sync::mpsc::UnboundedReceiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (delivered, received) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            let (tcp, _) = listener.accept().await.unwrap();
            let delivered = delivered.clone();
            tokio::spawn(async move {
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
                            let mut message = String::new();
                            loop {
                                let mut next = String::new();
                                stream.read_line(&mut next).await.unwrap();
                                if next == ".\r\n" {
                                    break;
                                }
                                message.push_str(&next);
                            }
                            let _ = delivered.send(message);
                            b"250 Queued\r\n"
                        }
                        "QUIT" => {
                            let _ = stream.write_all(b"221 Goodbye\r\n").await;
                            return;
                        }
                        _ => b"250 OK\r\n",
                    };
                    stream.write_all(reply).await.unwrap();
                }
            });
        }
    });
    (port, received)
}

fn incoming(id: &str, subject: &str) -> (String, String) {
    (
        format!("uidl-{id}"),
        format!(
            "Message-ID: <{id}@example.com>\r\nFrom: Alice <alice@example.com>\r\n\
             To: Me <me@example.com>\r\nSubject: {subject}\r\nDate: {}\r\n\r\nHello.\r\n",
            chrono::Utc::now().to_rfc2822()
        ),
    )
}

fn pop3_args(pop3_port: u16, smtp_port: u16) -> AddPasswordAccountArgs {
    AddPasswordAccountArgs {
        email: "me@example.com".into(),
        display_name: Some("Me".into()),
        username: "me".into(),
        password: "secret".into(),
        mail_protocol: MailProtocol::Pop3,
        jmap_url: String::new(),
        incoming_host: "127.0.0.1".into(),
        incoming_port: pop3_port,
        smtp_host: "127.0.0.1".into(),
        smtp_port,
        connection: MailConnectionSettings::new(ConnectionSecurity::None, ConnectionSecurity::None),
    }
}

async fn wait_for<T, F, Fut>(mut check: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(value) = check().await {
                return value;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("condition not reached in time")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pop3_account_downloads_files_and_sends_mail_on_this_device() {
    let (pop3_port, pop3_commands) = pop3_server(vec![incoming("hello", "Hello there")]).await;
    let (smtp_port, mut delivered) = smtp_server().await;
    let temp = tempfile::tempdir().unwrap();
    let credentials = Arc::new(DevelopmentFileCredentialStore::new(
        temp.path().join("credentials.json"),
    ));
    let core = Core::start_mail_ui_with_credentials(Paths::for_tests(temp.path()), credentials)
        .await
        .unwrap();
    let account = core
        .add_account_password(pop3_args(pop3_port, smtp_port))
        .await
        .unwrap();
    assert_eq!(account.mail_protocol, MailProtocol::Pop3);

    let thread = wait_for(|| async {
        core.list_threads(View::Inbox, None, Some(account.id), None, None, (None, 50))
            .await
            .unwrap()
            .threads
            .into_iter()
            .next()
    })
    .await;
    assert_eq!(thread.subject, "Hello there");
    assert!(
        pop3_commands
            .lock()
            .unwrap()
            .iter()
            .any(|line| line == "QUIT")
    );

    // The mail can be kept elsewhere, one mbox file per folder.
    let exports = tempfile::tempdir().unwrap();
    let first_export = exports.path().join("first");
    let exported = core
        .export_account_mail(account.id, first_export.clone())
        .await
        .unwrap();
    assert_eq!(
        (exported.folders, exported.messages, exported.missing),
        (1, 1, 0)
    );
    let inbox = std::fs::read_to_string(first_export.join("Inbox.mbox")).unwrap();
    assert!(inbox.starts_with("From alice@example.com "), "{inbox}");
    assert!(inbox.contains("Subject: Hello there\n"), "{inbox}");
    let again = core
        .export_account_mail(account.id, first_export)
        .await
        .unwrap_err();
    assert!(again.to_string().contains("already exists"), "{again}");

    // The mail can't follow the account to a protocol that keeps mail on a
    // server.
    let switched = core
        .add_account_password(AddPasswordAccountArgs {
            mail_protocol: MailProtocol::Imap,
            ..pop3_args(pop3_port, smtp_port)
        })
        .await
        .unwrap_err();
    assert!(switched.to_string().contains("switch"), "{switched}");

    // Folders are local: no command reaches the server.
    let commands_before = pop3_commands.lock().unwrap().len();
    core.create_folder(account.id, None, "Projects".into())
        .await
        .unwrap();
    let folders = core.list_folders(Some(account.id)).await.unwrap();
    let projects = folders
        .iter()
        .find(|folder| folder.display_name == "Projects")
        .unwrap()
        .id;
    let duplicate = core
        .create_folder(account.id, None, "Projects".into())
        .await
        .unwrap_err();
    assert!(duplicate.to_string().contains("already exists"));
    core.rename_folder(projects, "Q&A".into()).await.unwrap();
    let folders = core.list_folders(Some(account.id)).await.unwrap();
    assert!(folders.iter().any(|folder| folder.display_name == "Q&A"));

    // Moving mail completes at once.
    let result = core
        .perform_action(PerformActionArgs {
            kind: ActionKind::Move,
            thread_ids: vec![thread.id],
            params: Some(ActionParams {
                wake_at: None,
                target_folder_id: Some(projects),
                label_id: None,
            }),
        })
        .await
        .unwrap();
    let action_id = *result.action_ids.first().unwrap();
    wait_for(|| async {
        core.db
            .read(move |conn| {
                Ok(conn.query_row(
                    "SELECT state FROM pending_actions WHERE id = ?1",
                    [action_id],
                    |row| row.get::<_, String>(0),
                )?)
            })
            .await
            .ok()
            .filter(|state| state == "done")
    })
    .await;
    let in_folder = core
        .list_threads(
            View::All,
            None,
            Some(account.id),
            None,
            Some(projects),
            (None, 50),
        )
        .await
        .unwrap();
    assert_eq!(in_folder.threads.len(), 1);

    // Deleting the folder deletes its mail and the stored original.
    core.delete_folder(projects).await.unwrap();
    let account_id = account.id;
    let (messages, originals): (i64, i64) = core
        .db
        .read(move |conn| {
            Ok(conn.query_row(
                "SELECT COUNT(*), COUNT(raw_path) FROM messages WHERE account_id = ?1",
                [account_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?)
        })
        .await
        .unwrap();
    assert_eq!((messages, originals), (0, 0));
    assert_eq!(pop3_commands.lock().unwrap().len(), commands_before);

    // Sending goes through SMTP into the local Sent folder only.
    let draft_id = core
        .save_draft(SaveDraftArgs {
            draft_id: None,
            account_id: account.id,
            from: None,
            to: vec![Address {
                name: None,
                email: "friend@example.com".into(),
            }],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "From a POP3 account".into(),
            body_text: "Kept on this device.".into(),
            body_html: None,
            mode: "new".into(),
            in_reply_to_message_id: None,
            attachments: Vec::new(),
        })
        .await
        .unwrap();
    let queued = core
        .queue_send(QueueSendArgs {
            draft_id,
            send_at: Some(now_ms()),
        })
        .await
        .unwrap();
    tokio::time::timeout(
        Duration::from_secs(10),
        core.wait_for_send(queued.action_id),
    )
    .await
    .unwrap()
    .unwrap();
    let message = tokio::time::timeout(Duration::from_secs(5), delivered.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(message.contains("Subject: From a POP3 account"));
    let (folder_role, appends): (String, i64) = core
        .db
        .read(move |conn| {
            Ok((
                conn.query_row(
                    "SELECT f.role FROM messages m JOIN folders f ON f.id = m.folder_id
                     WHERE m.id = ?1",
                    [draft_id],
                    |row| row.get(0),
                )?,
                conn.query_row(
                    "SELECT COUNT(*) FROM pending_actions WHERE kind = 'append_sent'",
                    [],
                    |row| row.get(0),
                )?,
            ))
        })
        .await
        .unwrap();
    assert_eq!(folder_role, roles::SENT);
    assert_eq!(appends, 0);

    // Drafts have no original yet; the export builds one.
    core.save_draft(SaveDraftArgs {
        draft_id: None,
        account_id: account.id,
        from: None,
        to: Vec::new(),
        cc: Vec::new(),
        bcc: Vec::new(),
        subject: "Unsent idea".into(),
        body_text: "Still thinking.".into(),
        body_html: None,
        mode: "new".into(),
        in_reply_to_message_id: None,
        attachments: Vec::new(),
    })
    .await
    .unwrap();
    let second_export = exports.path().join("second");
    let exported = core
        .export_account_mail(account.id, second_export.clone())
        .await
        .unwrap();
    assert_eq!(
        (exported.folders, exported.messages, exported.missing),
        (2, 2, 0)
    );
    let drafts = std::fs::read_to_string(second_export.join("Drafts.mbox")).unwrap();
    assert!(drafts.contains("Subject: Unsent idea\n"), "{drafts}");
    let sent = std::fs::read_to_string(second_export.join("Sent.mbox")).unwrap();
    assert!(sent.contains("Subject: From a POP3 account\n"), "{sent}");
}

#[tokio::test(flavor = "multi_thread")]
async fn how_long_mail_stays_on_the_server_applies_at_once() {
    let (pop3_port, pop3_commands) = pop3_server(vec![incoming("hello", "Hello there")]).await;
    let (smtp_port, _delivered) = smtp_server().await;
    let temp = tempfile::tempdir().unwrap();
    let credentials = Arc::new(DevelopmentFileCredentialStore::new(
        temp.path().join("credentials.json"),
    ));
    let core = Core::start_mail_ui_with_credentials(Paths::for_tests(temp.path()), credentials)
        .await
        .unwrap();
    let account = core
        .add_account_password(pop3_args(pop3_port, smtp_port))
        .await
        .unwrap();
    wait_for(|| async {
        core.list_threads(View::Inbox, None, Some(account.id), None, None, (None, 50))
            .await
            .unwrap()
            .threads
            .into_iter()
            .next()
    })
    .await;
    let removals = || {
        pop3_commands
            .lock()
            .unwrap()
            .iter()
            .filter(|command| command.starts_with("DELE"))
            .cloned()
            .collect::<Vec<_>>()
    };
    assert!(removals().is_empty());

    core.set_account_keep_on_server(account.id, KeepOnServer::UntilDownloaded)
        .await
        .unwrap();
    let settings = core
        .list_account_configs()
        .await
        .unwrap()
        .into_iter()
        .find(|config| config.id == account.id)
        .unwrap()
        .settings;
    assert_eq!(settings.keep_on_server, KeepOnServer::UntilDownloaded);
    // This server refuses DELE, so every later sync asks again.
    wait_for(|| async { removals().first().cloned() }).await;
    assert!(removals().iter().all(|command| command == "DELE 1"));
}

#[tokio::test]
async fn interrupted_mail_writes_are_removed_at_startup() {
    let temp = tempfile::tempdir().unwrap();
    let mail_dir = Paths::for_tests(temp.path()).mail_dir(7);
    std::fs::create_dir_all(&mail_dir).unwrap();
    let partial = mail_dir.join(".message.eml.0123456789abcdef.partial");
    let orphan = mail_dir.join("0123456789abcdef0123456789abcdef.pop3.eml");
    let cache = mail_dir.join("3.1.42.eml");
    for path in [&partial, &orphan, &cache] {
        std::fs::write(path, b"x").unwrap();
    }
    let credentials = Arc::new(DevelopmentFileCredentialStore::new(
        temp.path().join("credentials.json"),
    ));
    Core::start_mail_ui_with_credentials(Paths::for_tests(temp.path()), credentials)
        .await
        .unwrap();
    assert!(!partial.exists());
    assert!(!orphan.exists());
    assert!(cache.exists());
}
