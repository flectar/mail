use super::*;
use crate::accounts::credentials::{CredentialStoreHandle, DevelopmentFileCredentialStore};
use crate::config::Paths;
use crate::db::Db;
use crate::events::EventBus;
use crate::models::{
    AuthKind, ConnectionSecurity, MailConnectionSettings, MailHistory, MailProtocol, Provider,
};
use crate::oauth::tokens::TokenProvider;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

/// A message in the fake server's maildrop.
#[derive(Clone)]
struct Stored {
    uidl: String,
    raw: Vec<u8>,
}

/// A POP3 server whose maildrop and behavior tests change between sessions.
/// Every session's commands are recorded. Messages marked with DELE leave
/// the maildrop when their session ends with QUIT.
struct Server {
    capabilities: Vec<&'static str>,
    maildrop: Vec<Stored>,
    /// Answer to PASS; `None` accepts the password.
    pass_reply: Option<&'static str>,
    /// UIDLs whose RETR is refused.
    refused: HashSet<String>,
    /// UIDLs whose DELE is refused.
    kept: HashSet<String>,
    /// The connection drops instead of answering this DELE of a session.
    drop_at_delete: Option<usize>,
    sessions: Vec<Vec<String>>,
}

impl Server {
    fn new(maildrop: Vec<Stored>) -> Self {
        Self {
            capabilities: vec!["USER", "UIDL", "TOP", "PIPELINING", "RESP-CODES"],
            maildrop,
            pass_reply: None,
            refused: HashSet::new(),
            kept: HashSet::new(),
            drop_at_delete: None,
            sessions: Vec::new(),
        }
    }
}

type Shared = Arc<Mutex<Server>>;

fn message(id: &str, subject: &str, date: &str) -> Stored {
    Stored {
        uidl: format!("uidl-{id}"),
        raw: format!(
            "Message-ID: <{id}@example.com>\r\nFrom: Alice <alice@example.com>\r\n\
             To: Me <me@example.com>\r\nSubject: {subject}\r\nDate: {date}\r\n\
             Content-Type: text/plain\r\n\r\nHello about {subject}.\r\n.dotted line\r\n"
        )
        .into_bytes(),
    }
}

fn recent(id: &str, subject: &str) -> Stored {
    message(id, subject, &chrono::Utc::now().to_rfc2822())
}

fn dot_stuffed(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for line in data.split_inclusive(|&byte| byte == b'\n') {
        if line.starts_with(b".") {
            out.push(b'.');
        }
        out.extend_from_slice(line);
    }
    out.extend_from_slice(b".\r\n");
    out
}

fn header_section(raw: &[u8]) -> Vec<u8> {
    let end = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map_or(raw.len(), |at| at + 4);
    raw[..end].to_vec()
}

async fn serve(server: Shared) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let (tcp, _) = listener.accept().await.unwrap();
            session(tcp, server.clone()).await;
        }
    });
    port
}

async fn session(tcp: tokio::net::TcpStream, server: Shared) {
    let (read, mut write) = tcp.into_split();
    let mut reader = BufReader::new(read);
    let (maildrop, capabilities, pass_reply, refused, kept, drop_at_delete) = {
        let mut server = server.lock().unwrap();
        server.sessions.push(Vec::new());
        (
            server.maildrop.clone(),
            server.capabilities.clone(),
            server.pass_reply,
            server.refused.clone(),
            server.kept.clone(),
            server.drop_at_delete,
        )
    };
    let mut deleted = vec![false; maildrop.len()];
    let mut deletes = 0;
    write.write_all(b"+OK ready\r\n").await.unwrap();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
            return;
        }
        let line = line.trim_end().to_owned();
        server
            .lock()
            .unwrap()
            .sessions
            .last_mut()
            .unwrap()
            .push(line.clone());
        let mut words = line.split(' ');
        let command = words.next().unwrap_or_default().to_ascii_uppercase();
        let index = |word: Option<&str>| {
            word.and_then(|value| value.parse::<usize>().ok())
                .and_then(|value| value.checked_sub(1))
                .filter(|&index| index < maildrop.len())
        };
        let number = |word: Option<&str>| {
            index(word)
                .filter(|&index| !deleted[index])
                .map(|index| &maildrop[index])
        };
        let reply: Vec<u8> = match command.as_str() {
            "CAPA" => {
                let mut reply = String::from("+OK\r\n");
                for capability in &capabilities {
                    reply.push_str(capability);
                    reply.push_str("\r\n");
                }
                reply.push_str(".\r\n");
                reply.into_bytes()
            }
            "USER" => b"+OK\r\n".to_vec(),
            "PASS" => pass_reply.unwrap_or("+OK\r\n").as_bytes().to_vec(),
            "UIDL" => {
                let mut reply = String::from("+OK\r\n");
                for (index, message) in maildrop.iter().enumerate() {
                    if !deleted[index] {
                        reply.push_str(&format!("{} {}\r\n", index + 1, message.uidl));
                    }
                }
                reply.push_str(".\r\n");
                reply.into_bytes()
            }
            "LIST" => {
                let mut reply = String::from("+OK\r\n");
                for (index, message) in maildrop.iter().enumerate() {
                    if !deleted[index] {
                        reply.push_str(&format!("{} {}\r\n", index + 1, message.raw.len()));
                    }
                }
                reply.push_str(".\r\n");
                reply.into_bytes()
            }
            "TOP" if capabilities.contains(&"TOP") => match number(words.next()) {
                Some(message) => [
                    b"+OK\r\n".to_vec(),
                    dot_stuffed(&header_section(&message.raw)),
                ]
                .concat(),
                None => b"-ERR no such message\r\n".to_vec(),
            },
            "RETR" => match number(words.next()) {
                Some(message) if refused.contains(&message.uidl) => {
                    b"-ERR [SYS/TEMP] try later\r\n".to_vec()
                }
                Some(message) => [b"+OK\r\n".to_vec(), dot_stuffed(&message.raw)].concat(),
                None => b"-ERR no such message\r\n".to_vec(),
            },
            "DELE" => match index(words.next()) {
                Some(index) if deleted[index] => b"-ERR message already deleted\r\n".to_vec(),
                Some(index) if kept.contains(&maildrop[index].uidl) => {
                    b"-ERR [SYS/PERM] message can't be deleted\r\n".to_vec()
                }
                Some(index) => {
                    deletes += 1;
                    if drop_at_delete == Some(deletes) {
                        return;
                    }
                    deleted[index] = true;
                    b"+OK marked\r\n".to_vec()
                }
                None => b"-ERR no such message\r\n".to_vec(),
            },
            "QUIT" => {
                {
                    let mut server = server.lock().unwrap();
                    assert_eq!(server.maildrop.len(), deleted.len());
                    let mut index = 0;
                    server.maildrop.retain(|_| {
                        index += 1;
                        !deleted[index - 1]
                    });
                }
                let _ = write.write_all(b"+OK bye\r\n").await;
                return;
            }
            _ => b"-ERR unknown command\r\n".to_vec(),
        };
        if write.write_all(&reply).await.is_err() {
            return;
        }
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    ctx: SyncCtx,
    config: AccountConfig,
    server: Shared,
}

async fn fixture(maildrop: Vec<Stored>) -> Fixture {
    let server = Arc::new(Mutex::new(Server::new(maildrop)));
    let port = serve(server.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let credentials: CredentialStoreHandle = Arc::new(DevelopmentFileCredentialStore::new(
        dir.path().join("credentials.json"),
    ));
    let ctx = SyncCtx {
        db: Db::open(&dir.path().join("mail.db")).unwrap(),
        calendar_db: Db::open_calendar(&dir.path().join("calendar.db")).unwrap(),
        bus: EventBus::new(),
        paths: Arc::new(Paths::for_tests(dir.path())),
        tokens: TokenProvider::new(
            credentials.clone(),
            Arc::new(crate::oauth::redirect::LoopbackRedirectBroker::default()),
        ),
        credentials,
    };
    let mut settings = AccountSettings::new(MailConnectionSettings::new(
        ConnectionSecurity::None,
        ConnectionSecurity::None,
    ));
    settings.mail_history = MailHistory::All;
    let config = ctx
        .db
        .write(move |conn| {
            let account_id = repo::accounts::insert(
                conn,
                &repo::accounts::NewAccount {
                    email: "me@example.com",
                    display_name: Some("Me"),
                    avatar_url: None,
                    provider: Provider::Generic,
                    auth_kind: AuthKind::Password,
                    mail_protocol: MailProtocol::Pop3,
                    username: "me",
                    jmap_url: "",
                    jmap_account_id: None,
                    incoming_host: "127.0.0.1",
                    incoming_port: port,
                    smtp_host: "127.0.0.1",
                    smtp_port: 587,
                },
                &settings,
            )?;
            Ok(repo::accounts::get_config(conn, account_id)?.unwrap())
        })
        .await
        .unwrap();
    credentials::store_async(
        ctx.credentials.clone(),
        config.id,
        Slot::Password,
        "secret".into(),
    )
    .await
    .unwrap();
    Fixture {
        _dir: dir,
        ctx,
        config,
        server,
    }
}

impl Fixture {
    async fn sync(&self) -> std::result::Result<Synced, Failure> {
        sync(&self.ctx, &self.config).await
    }

    fn last_session(&self) -> Vec<String> {
        self.server.lock().unwrap().sessions.last().unwrap().clone()
    }

    fn retrieved(&self) -> Vec<String> {
        self.last_session()
            .into_iter()
            .filter(|command| command.starts_with("RETR"))
            .collect()
    }

    /// Inbox subjects, oldest first.
    async fn inbox(&self) -> Vec<String> {
        let account_id = self.config.id;
        self.ctx
            .db
            .read(move |conn| {
                let mut statement = conn.prepare(
                    "SELECT m.subject FROM messages m JOIN folders f ON f.id = m.folder_id
                     WHERE m.account_id = ?1 AND f.role = 'inbox' ORDER BY m.id",
                )?;
                Ok(statement
                    .query_map([account_id], |row| row.get(0))?
                    .collect::<rusqlite::Result<Vec<String>>>()?)
            })
            .await
            .unwrap()
    }

    async fn known(&self) -> HashMap<String, KnownUidl> {
        let account_id = self.config.id;
        self.ctx
            .db
            .read(move |conn| repo::pop3::known_uidls(conn, account_id))
            .await
            .unwrap()
    }

    /// The Sent copy of the message `recent("self", ...)` delivers.
    async fn sent_to_self(&self) -> i64 {
        let account_id = self.config.id;
        let original = self._dir.path().join("sent-to-self.eml");
        tokio::fs::write(&original, recent("self", "Note to self").raw)
            .await
            .unwrap();
        let original = original.to_string_lossy().into_owned();
        self.ctx
            .db
            .write(move |conn| {
                let sent = repo::folders::by_role(conn, account_id, roles::SENT)?.unwrap();
                let thread = conn.query_row(
                    "INSERT INTO threads (account_id, subject_norm, last_message_at)
                     VALUES (?1, 'note to self', 1) RETURNING id",
                    [account_id],
                    |row| row.get::<_, i64>(0),
                )?;
                Ok(conn.query_row(
                    "INSERT INTO messages (thread_id, account_id, folder_id, message_id, subject,
                       from_addr, date, is_read, is_outgoing, raw_path)
                     VALUES (?1, ?2, ?3, 'self@example.com', 'Note to self', 'me@example.com',
                             1, 1, 1, ?4)
                     RETURNING id",
                    rusqlite::params![thread, account_id, sent.id, original],
                    |row| row.get::<_, i64>(0),
                )?)
            })
            .await
            .unwrap()
    }

    /// The UIDLs left on the server.
    fn maildrop(&self) -> Vec<String> {
        let server = self.server.lock().unwrap();
        server
            .maildrop
            .iter()
            .map(|message| message.uidl.clone())
            .collect()
    }

    /// The last session's DELE commands, in order.
    fn removed(&self) -> Vec<String> {
        self.last_session()
            .into_iter()
            .filter(|command| command.starts_with("DELE"))
            .collect()
    }

    async fn message_id(&self, subject: &str) -> i64 {
        let account_id = self.config.id;
        let subject = subject.to_owned();
        self.ctx
            .db
            .read(move |conn| {
                Ok(conn.query_row(
                    "SELECT id FROM messages WHERE account_id = ?1 AND subject = ?2",
                    rusqlite::params![account_id, subject],
                    |row| row.get(0),
                )?)
            })
            .await
            .unwrap()
    }

    /// Delete a message the way emptying the Trash does.
    async fn delete_here(&self, subject: &str) {
        let message_id = self.message_id(subject).await;
        crate::sync::finish_permanent_delete(&self.ctx, Some(message_id))
            .await
            .unwrap();
    }

    async fn original(&self, subject: &str) -> String {
        let message_id = self.message_id(subject).await;
        self.ctx
            .db
            .read(move |conn| Ok(repo::messages::get_row(conn, message_id)?.unwrap().raw_path))
            .await
            .unwrap()
            .unwrap()
    }

    async fn originals(&self) -> Vec<std::path::PathBuf> {
        let mut found = Vec::new();
        if let Ok(mut entries) = tokio::fs::read_dir(self.ctx.paths.mail_dir(self.config.id)).await
        {
            while let Some(entry) = entries.next_entry().await.unwrap() {
                found.push(entry.path());
            }
        }
        found
    }
}

#[tokio::test]
async fn new_mail_is_downloaded_once_and_the_session_ends_with_quit() {
    let fixture = fixture(vec![recent("a", "First"), recent("b", "Second")]).await;
    assert!(matches!(fixture.sync().await, Ok(Synced::Complete)));
    assert_eq!(fixture.inbox().await, ["Second", "First"]);
    assert_eq!(fixture.retrieved(), ["RETR 2", "RETR 1"]);
    assert_eq!(fixture.last_session().last().unwrap(), "QUIT");
    assert_eq!(fixture.originals().await.len(), 2);

    let account_id = fixture.config.id;
    let unread: i64 = fixture
        .ctx
        .db
        .read(move |conn| {
            Ok(conn.query_row(
                "SELECT COUNT(*) FROM messages WHERE account_id = ?1 AND is_read = 0",
                [account_id],
                |row| row.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(unread, 2);

    assert!(matches!(fixture.sync().await, Ok(Synced::Complete)));
    assert!(fixture.retrieved().is_empty());
    assert_eq!(fixture.inbox().await.len(), 2);
}

#[tokio::test]
async fn a_message_downloaded_but_never_committed_is_imported_once_later() {
    let fixture = fixture(vec![recent("a", "Lost commit")]).await;
    fixture.sync().await.unwrap();
    // The state a stop between the file write and the commit leaves.
    let account_id = fixture.config.id;
    fixture
        .ctx
        .db
        .write(move |conn| {
            conn.execute("DELETE FROM pop3_uidls WHERE account_id = ?1", [account_id])?;
            let id: i64 = conn.query_row(
                "SELECT id FROM messages WHERE account_id = ?1",
                [account_id],
                |row| row.get(0),
            )?;
            repo::messages::delete(conn, id)
        })
        .await
        .unwrap();
    fixture.sync().await.unwrap();
    assert_eq!(fixture.inbox().await, ["Lost commit"]);
    fixture.sync().await.unwrap();
    assert_eq!(fixture.inbox().await, ["Lost commit"]);
}

#[tokio::test]
async fn renumbered_vanishing_and_returning_messages_are_never_fetched_twice() {
    let (a, b, c) = (recent("a", "A"), recent("b", "B"), recent("c", "C"));
    let fixture = fixture(vec![a.clone(), b.clone(), c.clone()]).await;
    fixture.sync().await.unwrap();

    // Another client deleted A, so everything moved up one number.
    let d = recent("d", "D");
    fixture.server.lock().unwrap().maildrop = vec![b.clone(), c.clone(), d.clone()];
    fixture.sync().await.unwrap();
    assert_eq!(fixture.retrieved(), ["RETR 3"]);
    let known = fixture.known().await;
    assert!(known["uidl-a"].missing_since.is_some());
    assert!(known["uidl-b"].missing_since.is_none());

    fixture.server.lock().unwrap().maildrop = vec![b, c, d, a];
    fixture.sync().await.unwrap();
    assert!(fixture.retrieved().is_empty());
    assert!(fixture.known().await["uidl-a"].missing_since.is_none());
    assert_eq!(fixture.inbox().await.len(), 4);
}

#[tokio::test]
async fn a_repeated_uidl_downloads_only_its_newest_message() {
    let mut twin = recent("twin-new", "Newer");
    twin.uidl = "uidl-twin".into();
    let mut older = recent("twin-old", "Older");
    older.uidl = "uidl-twin".into();
    let fixture = fixture(vec![older, twin]).await;
    fixture.sync().await.unwrap();
    assert_eq!(fixture.retrieved(), ["RETR 2"]);
    assert_eq!(fixture.inbox().await, ["Newer"]);
}

#[tokio::test]
async fn mail_sent_to_yourself_stays_in_sent() {
    let fixture = fixture(vec![recent("self", "Note to self")]).await;
    let account_id = fixture.config.id;
    let sent_id = fixture.sent_to_self().await;
    fixture.sync().await.unwrap();
    assert!(fixture.inbox().await.is_empty());
    let (known, downloaded_as) = fixture
        .ctx
        .db
        .read(move |conn| {
            Ok((
                repo::pop3::known_uidls(conn, account_id)?,
                repo::pop3::uidl_of_message(conn, sent_id)?,
            ))
        })
        .await
        .unwrap();
    assert_eq!(known["uidl-self"].state, UidlState::Matched);
    // The downloaded copy was not kept beside the existing one, so the Sent
    // copy can't be restored from it.
    assert!(fixture.originals().await.is_empty());
    assert_eq!(downloaded_as, None);
}

#[tokio::test]
async fn the_mail_history_skips_old_mail_without_retrieving_it() {
    let old = message("old", "Old", "Mon, 01 Jan 2018 10:00:00 +0000");
    let mut fixture = fixture(vec![old, recent("new", "New")]).await;
    fixture.config.settings.mail_history = MailHistory::SixMonths;
    fixture.sync().await.unwrap();
    assert_eq!(fixture.inbox().await, ["New"]);
    let session = fixture.last_session();
    assert!(session.iter().any(|command| command == "TOP 1 0"));
    assert_eq!(fixture.retrieved(), ["RETR 2"]);
    let known = fixture.known().await;
    assert_eq!(known["uidl-old"].state, UidlState::Skipped);

    // Widening the history brings it in, without reading its date again.
    fixture.config.settings.mail_history = MailHistory::All;
    fixture.sync().await.unwrap();
    assert_eq!(fixture.retrieved(), ["RETR 1"]);
    assert!(
        !fixture
            .last_session()
            .iter()
            .any(|command| command.starts_with("TOP"))
    );
    assert_eq!(fixture.inbox().await, ["New", "Old"]);
}

#[tokio::test]
async fn without_top_the_mail_history_cannot_apply() {
    let old = message("old", "Old", "Mon, 01 Jan 2018 10:00:00 +0000");
    let mut fixture = fixture(vec![old, recent("new", "New")]).await;
    fixture.server.lock().unwrap().capabilities = vec!["USER", "UIDL"];
    fixture.config.settings.mail_history = MailHistory::SixMonths;
    fixture.sync().await.unwrap();
    assert_eq!(fixture.retrieved(), ["RETR 2", "RETR 1"]);
    assert_eq!(fixture.inbox().await.len(), 2);
}

#[tokio::test]
async fn a_busy_mailbox_and_a_login_delay_are_retried_quietly() {
    let fixture = fixture(vec![recent("a", "A")]).await;
    for reply in ["-ERR [IN-USE] locked\r\n", "-ERR [LOGIN-DELAY] wait\r\n"] {
        fixture.server.lock().unwrap().pass_reply = Some(reply);
        assert!(
            matches!(fixture.sync().await, Err(Failure::Busy(_))),
            "{reply}"
        );
    }
    fixture.server.lock().unwrap().pass_reply = Some("-ERR [AUTH] wrong\r\n");
    assert!(matches!(fixture.sync().await, Err(Failure::Credentials(_))));
    fixture.server.lock().unwrap().pass_reply = Some("-ERR no\r\n");
    assert!(matches!(fixture.sync().await, Err(Failure::Unexplained(_))));
}

#[tokio::test]
async fn the_server_login_delay_postpones_the_next_sign_in() {
    let fixture = fixture(vec![recent("a", "A")]).await;
    fixture.server.lock().unwrap().capabilities = vec!["USER", "UIDL", "TOP", "LOGIN-DELAY 900"];
    fixture.sync().await.unwrap();
    let sessions = fixture.server.lock().unwrap().sessions.len();
    assert!(matches!(fixture.sync().await, Ok(Synced::NotBefore(_))));
    assert_eq!(fixture.server.lock().unwrap().sessions.len(), sessions);
}

#[tokio::test]
async fn a_refused_message_is_retried_later_and_the_rest_still_arrive() {
    let fixture = fixture(vec![recent("a", "A"), recent("b", "B")]).await;
    fixture.server.lock().unwrap().refused = HashSet::from(["uidl-b".to_owned()]);
    assert!(matches!(fixture.sync().await, Ok(Synced::Complete)));
    assert_eq!(fixture.inbox().await, ["A"]);
    assert_eq!(fixture.last_session().last().unwrap(), "QUIT");
    let known = fixture.known().await;
    assert_eq!(known["uidl-b"].state, UidlState::Failed);
    assert!(known["uidl-b"].retry_at.is_some_and(|at| at > now_ms()));
    // Not due yet.
    fixture.server.lock().unwrap().refused.clear();
    fixture.sync().await.unwrap();
    assert!(fixture.retrieved().is_empty());
}

#[tokio::test]
async fn expire_zero_with_a_failing_write_sends_no_quit() {
    let fixture = fixture(vec![recent("a", "A")]).await;
    fixture.server.lock().unwrap().capabilities = vec!["USER", "UIDL", "TOP", "EXPIRE 0"];
    // A file where the mail folder should be makes every write fail.
    let mail_dir = fixture.ctx.paths.mail_dir(fixture.config.id);
    tokio::fs::create_dir_all(mail_dir.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&mail_dir, b"in the way").await.unwrap();
    assert!(matches!(fixture.sync().await, Err(Failure::Other(_))));
    let session = fixture.last_session();
    assert!(session.iter().any(|command| command == "RETR 1"));
    assert!(!session.iter().any(|command| command == "QUIT"));
    assert!(fixture.inbox().await.is_empty());
    assert!(fixture.known().await.is_empty());
}

#[tokio::test]
async fn only_mail_after_the_first_download_notifies() {
    let fixture = fixture(vec![recent("a", "Backlog")]).await;
    let mut events = fixture.ctx.bus.subscribe();
    fixture.sync().await.unwrap();
    let outbox = |fixture: &Fixture| {
        let db = fixture.ctx.db.clone();
        async move {
            db.read(|conn| {
                Ok(
                    conn.query_row("SELECT COUNT(*) FROM notification_outbox", [], |row| {
                        row.get::<_, i64>(0)
                    })?,
                )
            })
            .await
            .unwrap()
        }
    };
    assert_eq!(outbox(&fixture).await, 0);

    fixture
        .server
        .lock()
        .unwrap()
        .maildrop
        .push(recent("b", "Fresh"));
    fixture.sync().await.unwrap();
    assert_eq!(outbox(&fixture).await, 1);
    let mut new_mail = false;
    while let Ok(event) = events.try_recv() {
        new_mail |= matches!(event, CoreEvent::MailNew { .. });
    }
    assert!(new_mail);
}

#[tokio::test]
async fn deleting_mail_here_completes_at_once_and_never_downloads_it_again() {
    let fixture = fixture(vec![recent("a", "Gone")]).await;
    fixture.sync().await.unwrap();
    let account_id = fixture.config.id;
    let message_id = fixture
        .ctx
        .db
        .write(move |conn| {
            let id: i64 = conn.query_row(
                "SELECT id FROM messages WHERE account_id = ?1",
                [account_id],
                |row| row.get(0),
            )?;
            repo::actions::enqueue(
                conn,
                account_id,
                "delete_permanently",
                Some(id),
                None,
                &serde_json::json!({}),
                None,
            )?;
            Ok(id)
        })
        .await
        .unwrap();
    super::super::complete_device_actions(&fixture.ctx, fixture.config.id)
        .await
        .unwrap();
    assert!(fixture.inbox().await.is_empty());
    assert!(fixture.originals().await.is_empty());
    let state: String = fixture
        .ctx
        .db
        .read(move |conn| {
            Ok(conn.query_row(
                "SELECT state FROM pending_actions WHERE message_id IS NULL OR message_id = ?1",
                [message_id],
                |row| row.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(state, "done");

    fixture.sync().await.unwrap();
    assert!(fixture.retrieved().is_empty());
    assert!(fixture.inbox().await.is_empty());
}

#[tokio::test]
async fn a_missing_original_is_downloaded_again_while_the_server_has_it() {
    let fixture = fixture(vec![recent("a", "Keep")]).await;
    fixture.sync().await.unwrap();
    let account_id = fixture.config.id;
    let (message_id, path): (i64, String) = fixture
        .ctx
        .db
        .read(move |conn| {
            Ok(conn.query_row(
                "SELECT id, raw_path FROM messages WHERE account_id = ?1",
                [account_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?)
        })
        .await
        .unwrap();
    tokio::fs::remove_file(&path).await.unwrap();
    let restored = restore(&fixture.ctx, &fixture.config, message_id)
        .await
        .unwrap();
    assert_ne!(restored, path);
    let raw = tokio::fs::read(&restored).await.unwrap();
    assert!(String::from_utf8(raw).unwrap().contains("Subject: Keep"));

    tokio::fs::remove_file(&restored).await.unwrap();
    fixture.server.lock().unwrap().maildrop.clear();
    let error = restore(&fixture.ctx, &fixture.config, message_id)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("no longer has it"), "{error}");
}

#[test]
fn plans_list_the_newest_first_and_skip_what_is_settled() {
    let listed = |number: u32, uidl: &str| Listed {
        number,
        uidl: uidl.into(),
        size: 10,
    };
    let entry = |state, sent_at, retry_at| KnownUidl {
        state,
        sent_at,
        missing_since: None,
        attempts: 1,
        retry_at,
    };
    let known = HashMap::from([
        ("stored".to_owned(), entry(UidlState::Stored, None, None)),
        ("matched".to_owned(), entry(UidlState::Matched, None, None)),
        ("old".to_owned(), entry(UidlState::Skipped, Some(5), None)),
        (
            "recent".to_owned(),
            entry(UidlState::Skipped, Some(50), None),
        ),
        ("due".to_owned(), entry(UidlState::Failed, None, Some(90))),
        (
            "later".to_owned(),
            entry(UidlState::Failed, None, Some(200)),
        ),
        ("never".to_owned(), entry(UidlState::Failed, None, None)),
    ]);
    let listing = [
        listed(1, "stored"),
        listed(2, "old"),
        listed(3, "recent"),
        listed(4, "due"),
        listed(5, "later"),
        listed(6, "never"),
        listed(7, "new"),
        listed(8, "matched"),
    ];
    let planned: Vec<_> = plan(&listing, &known, Some(10), 100)
        .into_iter()
        .map(|candidate| candidate.uidl)
        .collect();
    assert_eq!(planned, ["new", "due", "recent"]);
    let planned: Vec<_> = plan(&listing, &known, None, 100)
        .into_iter()
        .map(|candidate| candidate.uidl)
        .collect();
    assert_eq!(planned, ["new", "due", "recent", "old"]);
}

#[tokio::test]
async fn mail_stays_on_the_server_unless_the_setting_removes_it() {
    let fixture = fixture(vec![recent("a", "First"), recent("b", "Second")]).await;
    fixture.sync().await.unwrap();
    fixture.delete_here("First").await;
    assert!(matches!(fixture.sync().await, Ok(Synced::Complete)));
    assert!(fixture.removed().is_empty());
    assert_eq!(fixture.maildrop(), ["uidl-a", "uidl-b"]);
}

#[tokio::test]
async fn until_downloaded_removes_mail_in_the_session_that_stored_it() {
    let mut fixture = fixture(vec![recent("a", "First"), recent("b", "Second")]).await;
    fixture.config.settings.keep_on_server = KeepOnServer::UntilDownloaded;
    assert!(matches!(fixture.sync().await, Ok(Synced::Complete)));
    assert_eq!(fixture.inbox().await, ["Second", "First"]);
    let session = fixture.last_session();
    let retrieved = session
        .iter()
        .rposition(|command| command.starts_with("RETR"));
    let first_removal = session
        .iter()
        .position(|command| command.starts_with("DELE"));
    assert!(retrieved < first_removal);
    let mut removed = fixture.removed();
    removed.sort();
    assert_eq!(removed, ["DELE 1", "DELE 2"]);
    assert_eq!(session.last().unwrap(), "QUIT");
    assert!(fixture.maildrop().is_empty());

    assert!(matches!(fixture.sync().await, Ok(Synced::Complete)));
    assert!(fixture.retrieved().is_empty() && fixture.removed().is_empty());
    assert_eq!(fixture.inbox().await.len(), 2);
}

#[tokio::test]
async fn mail_deleted_here_is_removed_from_the_server() {
    let mut fixture = fixture(vec![recent("a", "First"), recent("b", "Second")]).await;
    fixture.config.settings.keep_on_server = KeepOnServer::UntilDeleted;
    fixture.sync().await.unwrap();
    assert!(fixture.removed().is_empty());

    fixture.delete_here("First").await;
    assert!(matches!(fixture.sync().await, Ok(Synced::Complete)));
    assert_eq!(fixture.removed(), ["DELE 1"]);
    assert_eq!(fixture.maildrop(), ["uidl-b"]);
    assert_eq!(fixture.inbox().await, ["Second"]);
}

#[tokio::test]
async fn mail_leaves_the_server_once_it_is_older_than_the_setting_keeps_it() {
    let mut fixture = fixture(vec![recent("a", "Older"), recent("b", "Newer")]).await;
    fixture.config.settings.keep_on_server = KeepOnServer::SevenDays;
    fixture.sync().await.unwrap();
    assert!(fixture.removed().is_empty());

    let account_id = fixture.config.id;
    fixture
        .ctx
        .db
        .write(move |conn| {
            conn.execute(
                "UPDATE pop3_uidls SET stored_at = stored_at - 8 * 86400000
                 WHERE account_id = ?1 AND uidl = 'uidl-a'",
                [account_id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    fixture.sync().await.unwrap();
    assert_eq!(fixture.removed(), ["DELE 1"]);
    assert_eq!(fixture.maildrop(), ["uidl-b"]);
    // Mail the setting keeps a while longer still goes once deleted here.
    fixture.delete_here("Newer").await;
    fixture.sync().await.unwrap();
    assert_eq!(fixture.removed(), ["DELE 1"]);
    assert!(fixture.maildrop().is_empty());
    assert_eq!(fixture.inbox().await, ["Older"]);
}

#[tokio::test]
async fn a_message_whose_original_is_missing_stays_on_the_server() {
    let mut fixture = fixture(vec![recent("a", "Lost file"), recent("b", "Kept file")]).await;
    fixture.sync().await.unwrap();
    tokio::fs::remove_file(fixture.original("Lost file").await)
        .await
        .unwrap();
    fixture.config.settings.keep_on_server = KeepOnServer::UntilDownloaded;
    fixture.sync().await.unwrap();
    assert_eq!(fixture.removed(), ["DELE 2"]);
    assert_eq!(fixture.maildrop(), ["uidl-a"]);
}

#[tokio::test]
async fn mail_the_account_already_had_stays_on_the_server() {
    let mut fixture = fixture(vec![recent("self", "Note to self")]).await;
    fixture.config.settings.keep_on_server = KeepOnServer::UntilDownloaded;
    fixture.sent_to_self().await;
    fixture.sync().await.unwrap();
    assert_eq!(fixture.known().await["uidl-self"].state, UidlState::Matched);
    assert!(fixture.removed().is_empty());
    assert_eq!(fixture.maildrop(), ["uidl-self"]);
}

#[tokio::test]
async fn a_reused_or_repeated_uidl_keeps_its_messages_on_the_server() {
    let mut fixture = fixture(vec![recent("a", "First"), recent("b", "Second")]).await;
    fixture.sync().await.unwrap();
    {
        let mut server = fixture.server.lock().unwrap();
        // Another message under a UIDL that was downloaded before, and a
        // second message under the other one.
        let mut reused = recent("other", "Another message entirely");
        reused.uidl = "uidl-a".into();
        server.maildrop[0] = reused;
        let mut repeated = recent("c", "Third");
        repeated.uidl = "uidl-b".into();
        server.maildrop.push(repeated);
    }
    fixture.config.settings.keep_on_server = KeepOnServer::UntilDownloaded;
    fixture.sync().await.unwrap();
    assert!(fixture.removed().is_empty());
    assert_eq!(fixture.maildrop(), ["uidl-a", "uidl-b", "uidl-b"]);
}

#[tokio::test]
async fn a_removal_the_server_refuses_is_tried_again_later() {
    let mut fixture = fixture(vec![recent("a", "First"), recent("b", "Second")]).await;
    fixture.config.settings.keep_on_server = KeepOnServer::UntilDownloaded;
    fixture.server.lock().unwrap().kept.insert("uidl-a".into());
    assert!(matches!(fixture.sync().await, Ok(Synced::Complete)));
    assert_eq!(fixture.maildrop(), ["uidl-a"]);

    fixture.server.lock().unwrap().kept.clear();
    fixture.sync().await.unwrap();
    assert_eq!(fixture.removed(), ["DELE 1"]);
    assert!(fixture.maildrop().is_empty());
}

#[tokio::test]
async fn removals_take_effect_only_when_the_session_ends_with_quit() {
    let mut fixture = fixture(vec![recent("a", "First"), recent("b", "Second")]).await;
    fixture.config.settings.keep_on_server = KeepOnServer::UntilDownloaded;
    fixture.server.lock().unwrap().drop_at_delete = Some(2);
    assert!(matches!(fixture.sync().await, Err(Failure::Other(_))));
    // The downloads were kept, and the server kept everything.
    assert_eq!(fixture.inbox().await, ["Second", "First"]);
    assert_eq!(fixture.maildrop(), ["uidl-a", "uidl-b"]);

    fixture.server.lock().unwrap().drop_at_delete = None;
    assert!(matches!(fixture.sync().await, Ok(Synced::Complete)));
    assert!(fixture.retrieved().is_empty());
    assert!(fixture.maildrop().is_empty());
}
