use super::*;
use crate::accounts::credentials::DevelopmentFileCredentialStore;
use crate::models::{ActionKind, ConnectionSecurity, MailConnectionSettings, PerformActionArgs};
use crate::{Core, config::Paths, db::testutil};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::TcpListener,
};

#[derive(Debug, Clone, PartialEq, Eq)]
struct Stored {
    uid: u32,
    message_id: String,
    flags: Vec<String>,
}

fn stored(uid: u32, message_id: &str, flags: &[&str]) -> Stored {
    Stored {
        uid,
        message_id: message_id.into(),
        flags: flags.iter().map(|flag| flag.to_string()).collect(),
    }
}

/// Mailboxes by name: the next UID and the messages.
type Mailboxes = BTreeMap<String, (u32, Vec<Stored>)>;

/// A server that moves and flags messages. Its SEARCH matches loosely, as
/// word-based server searches do, and returns every message.
struct Server {
    port: u16,
    mailboxes: Arc<Mutex<Mailboxes>>,
    task: tokio::task::JoinHandle<()>,
}

impl Server {
    async fn start(mailboxes: Mailboxes) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let mailboxes = Arc::new(Mutex::new(mailboxes));
        let state = mailboxes.clone();
        let task = tokio::spawn(async move {
            loop {
                let (tcp, _) = listener.accept().await.unwrap();
                let mut stream = BufReader::new(tcp);
                stream.write_all(b"* OK ready\r\n").await.unwrap();
                let mut selected = String::new();
                loop {
                    let mut line = String::new();
                    if stream.read_line(&mut line).await.unwrap() == 0 {
                        break;
                    }
                    let (tag, command) = line.trim_end().split_once(' ').unwrap();
                    let response = respond(&mut state.lock().unwrap(), &mut selected, command);
                    stream
                        .write_all(format!("{response}{tag} OK done\r\n").as_bytes())
                        .await
                        .unwrap();
                }
            }
        });
        Self {
            port,
            mailboxes,
            task,
        }
    }

    async fn connect(&self) -> Session {
        imap::connect_with_settings(
            "127.0.0.1",
            self.port,
            imap::ImapCredentials::Password {
                user: "me".into(),
                password: "password".into(),
            },
            &MailConnectionSettings::new(ConnectionSecurity::None, ConnectionSecurity::None),
        )
        .await
        .unwrap()
    }

    fn mailbox(&self, name: &str) -> Vec<Stored> {
        self.mailboxes.lock().unwrap()[name].1.clone()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// The untagged part of the reply to one command.
fn respond(mailboxes: &mut Mailboxes, selected: &mut String, command: &str) -> String {
    if command.starts_with("LOGIN ") {
        String::new()
    } else if command == "CAPABILITY" {
        "* CAPABILITY IMAP4rev1 MOVE UIDPLUS\r\n".into()
    } else if let Some(name) = command.strip_prefix("SELECT ") {
        *selected = name.trim_matches('"').into();
        let (next, messages) = &mailboxes[selected.as_str()];
        format!(
            "* {} EXISTS\r\n* OK [UIDVALIDITY 7] valid\r\n* OK [UIDNEXT {next}] next\r\n",
            messages.len()
        )
    } else if command.starts_with("UID SEARCH HEADER Message-ID ") {
        let uids = mailboxes[selected.as_str()]
            .1
            .iter()
            .map(|message| format!(" {}", message.uid))
            .collect::<String>();
        format!("* SEARCH{uids}\r\n")
    } else if let Some(rest) = command.strip_prefix("UID FETCH ") {
        let (set, _) = rest.split_once(' ').unwrap();
        let messages = &mailboxes[selected.as_str()].1;
        let mut out = String::new();
        for (index, message) in messages.iter().enumerate() {
            if set.split(',').any(|uid| uid == message.uid.to_string()) {
                let header = format!(
                    "Message-ID: <{}>\r\nSubject: Plans\r\n\r\n",
                    message.message_id
                );
                out += &format!(
                    "* {} FETCH (UID {} FLAGS ({}) BODY[HEADER.FIELDS (MESSAGE-ID SUBJECT)] {{{}}}\r\n{header})\r\n",
                    index + 1,
                    message.uid,
                    message.flags.join(" "),
                    header.len()
                );
            }
        }
        out
    } else if let Some(rest) = command.strip_prefix("UID MOVE ") {
        let (uid, target) = rest.split_once(' ').unwrap();
        let source = &mut mailboxes.get_mut(selected.as_str()).unwrap().1;
        let position = source
            .iter()
            .position(|message| message.uid.to_string() == uid)
            .unwrap();
        let mut message = source.remove(position);
        let (next, messages) = mailboxes.get_mut(target.trim_matches('"')).unwrap();
        message.uid = *next;
        *next += 1;
        messages.push(message);
        String::new()
    } else if let Some(rest) = command.strip_prefix("UID STORE ") {
        let (uid, change) = rest.split_once(' ').unwrap();
        let flag = change
            .strip_prefix("+FLAGS.SILENT (")
            .and_then(|flag| flag.strip_suffix(')'))
            .unwrap();
        let messages = &mut mailboxes.get_mut(selected.as_str()).unwrap().1;
        let message = messages
            .iter_mut()
            .find(|message| message.uid.to_string() == uid)
            .unwrap();
        message.flags.push(flag.into());
        String::new()
    } else {
        panic!("unexpected command: {command}");
    }
}

#[tokio::test]
async fn undo_and_star_find_a_moved_message_before_its_mailbox_syncs() {
    let temp = tempfile::tempdir().unwrap();
    let credentials = Arc::new(DevelopmentFileCredentialStore::new(
        temp.path().join("credentials.json"),
    ));
    let core = Core::start_mail_ui_with_credentials(Paths::for_tests(temp.path()), credentials)
        .await
        .unwrap();
    let (config, thread_id, message_id) = core
        .db
        .write(|conn| {
            testutil::seed_account(conn);
            conn.execute_batch(
                "UPDATE folders SET uidvalidity = 7;
                 INSERT INTO folders (id, account_id, path, role, uidvalidity)
                   VALUES (2, 1, 'Archive', 'archive', 7);",
            )?;
            let (thread_id, message_id) =
                testutil::seed_message(conn, "alice@test.dev", "Plans", false);
            conn.execute(
                "UPDATE messages SET uid = 5, message_id = 'plans@test.dev' WHERE id = ?1",
                [message_id],
            )?;
            Ok((
                repo::accounts::get_config(conn, 1)?.unwrap(),
                thread_id,
                message_id,
            ))
        })
        .await
        .unwrap();
    let server = Server::start(BTreeMap::from([
        ("INBOX".into(), (6, vec![stored(5, "plans@test.dev", &[])])),
        (
            "Archive".into(),
            (
                10,
                vec![
                    stored(3, "notes@test.dev", &[]),
                    stored(4, "plans@test.dev", &["\\Deleted"]),
                ],
            ),
        ),
    ]))
    .await;
    let mut session = server.connect().await;
    let ctx = core.sync_ctx();
    let perform = |kind| {
        core.perform_action(PerformActionArgs {
            kind,
            thread_ids: vec![thread_id],
            params: None,
        })
    };
    let states = || {
        core.db.read(|conn| {
            let mut statement =
                conn.prepare("SELECT kind, state FROM pending_actions ORDER BY id")?;
            let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
            Ok(rows.collect::<rusqlite::Result<Vec<(String, String)>>>()?)
        })
    };

    perform(ActionKind::Archive).await.unwrap();
    execute_due(&ctx, &config, &mut session).await.unwrap();
    assert_eq!(
        server.mailbox("Archive").last(),
        Some(&stored(10, "plans@test.dev", &[]))
    );

    // The archive already ran, so the server's copy has a UID this device
    // has not seen yet.
    assert!(core.undo_last().await.unwrap());
    execute_due(&ctx, &config, &mut session).await.unwrap();
    assert_eq!(server.mailbox("INBOX"), [stored(6, "plans@test.dev", &[])]);
    assert_eq!(
        server.mailbox("Archive"),
        [
            stored(3, "notes@test.dev", &[]),
            stored(4, "plans@test.dev", &["\\Deleted"]),
        ]
    );

    perform(ActionKind::Star).await.unwrap();
    execute_due(&ctx, &config, &mut session).await.unwrap();
    assert_eq!(
        server.mailbox("INBOX"),
        [stored(6, "plans@test.dev", &["\\Flagged"])]
    );
    assert_eq!(
        states().await.unwrap(),
        [
            ("archive".into(), "done".into()),
            ("move".into(), "done".into()),
            ("star".into(), "done".into()),
        ]
    );
    let row = core
        .db
        .read(move |conn| repo::messages::get_row(conn, message_id))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.folder_id, Some(1));
}
