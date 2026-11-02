use super::*;
use crate::accounts::credentials::{self, DevelopmentFileCredentialStore, Slot};
use crate::config::Paths;
use crate::mail_import::write::incoming;
use crate::mail_import::{Destination, MailImportRequest, scan_folder};
use crate::models::{AuthKind, ConnectionSecurity, MailConnectionSettings, Provider};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

const CERT: &str = include_str!("../../tests/fixtures/tls/server.pem");
const KEY: &str = include_str!("../../tests/fixtures/tls/server-key.pem");

fn message(id: &str, subject: &str, extra: &str) -> String {
    format!(
        "From: Ana <ana@example.org>\r\nTo: me@example.com\r\nSubject: {subject}\r\n\
         Date: Fri, 05 Jan 2024 10:00:00 +0000\r\nMessage-ID: <{id}@example.org>\r\n{extra}\r\n\
         Hello.\r\n"
    )
}

fn mbox(messages: &[String]) -> String {
    messages
        .iter()
        .map(|message| format!("From ana@example.org Fri Jan  5 10:00:00 2024\r\n{message}\r\n"))
        .collect()
}

fn write(path: &Path, contents: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

/// A message an IMAP mailbox holds.
#[derive(Debug, Clone)]
struct Stored {
    uid: u32,
    flags: String,
    date: String,
    raw: Vec<u8>,
}

#[derive(Default)]
struct Mailboxes {
    boxes: BTreeMap<String, (u32, Vec<Stored>)>,
    appends: usize,
}

/// A small IMAP server that keeps what is appended to it.
struct ImapServer {
    port: u16,
    state: Arc<Mutex<Mailboxes>>,
    task: tokio::task::JoinHandle<()>,
}

impl ImapServer {
    /// `top_closed` refuses folders outside INBOX; `refuse` refuses
    /// messages whose text has it.
    async fn start(top_closed: bool, refuse: Option<&'static str>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let state = Arc::new(Mutex::new(Mailboxes::default()));
        state
            .lock()
            .unwrap()
            .boxes
            .insert("INBOX".into(), (1, Vec::new()));
        let shared = state.clone();
        let task = tokio::spawn(async move {
            let tls = rustls::ServerConfig::builder_with_provider(Arc::new(
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
            let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls));
            loop {
                let (tcp, _) = listener.accept().await.unwrap();
                let acceptor = acceptor.clone();
                let shared = shared.clone();
                tokio::spawn(async move {
                    let Ok(tls) = acceptor.accept(tcp).await else {
                        return;
                    };
                    let mut stream = BufReader::new(tls);
                    stream.write_all(b"* OK ready\r\n").await.unwrap();
                    let mut selected = String::new();
                    loop {
                        let mut line = String::new();
                        if stream.read_line(&mut line).await.unwrap_or(0) == 0 {
                            break;
                        }
                        let Some((tag, command)) = line.trim_end().split_once(' ') else {
                            break;
                        };
                        let upper = command.to_ascii_uppercase();
                        let response = if upper == "CAPABILITY" {
                            format!("* CAPABILITY IMAP4rev1 UIDPLUS ID\r\n{tag} OK done\r\n")
                        } else if upper.starts_with("LOGIN ") || upper.starts_with("NOOP") {
                            format!("{tag} OK done\r\n")
                        } else if upper.starts_with("ID ") {
                            format!("* ID NIL\r\n{tag} OK done\r\n")
                        } else if upper.starts_with("LOGOUT") {
                            stream
                                .write_all(format!("* BYE\r\n{tag} OK done\r\n").as_bytes())
                                .await
                                .unwrap();
                            break;
                        } else if upper.starts_with("CREATE ") {
                            let name = quoted(command);
                            let closed = top_closed && !name.starts_with("INBOX/");
                            match shared.lock().unwrap().boxes.entry(name) {
                                std::collections::btree_map::Entry::Occupied(_) => {
                                    format!("{tag} NO [ALREADYEXISTS] exists\r\n")
                                }
                                std::collections::btree_map::Entry::Vacant(_) if closed => {
                                    format!("{tag} NO [CANNOT] only below INBOX\r\n")
                                }
                                std::collections::btree_map::Entry::Vacant(entry) => {
                                    entry.insert((1, Vec::new()));
                                    format!("{tag} OK created\r\n")
                                }
                            }
                        } else if upper.starts_with("LIST ") {
                            let state = shared.lock().unwrap();
                            let listed = state
                                .boxes
                                .keys()
                                .map(|name| format!("* LIST () \"/\" \"{name}\"\r\n"))
                                .collect::<String>();
                            format!("{listed}{tag} OK listed\r\n")
                        } else if upper.starts_with("SELECT ") {
                            selected = quoted(command);
                            let state = shared.lock().unwrap();
                            match state.boxes.get(&selected) {
                                Some((next, messages)) => format!(
                                    "* {} EXISTS\r\n* OK [UIDVALIDITY 7] valid\r\n* OK [UIDNEXT {next}] next\r\n{tag} OK [READ-WRITE] selected\r\n",
                                    messages.len()
                                ),
                                None => format!("{tag} NO no such mailbox\r\n"),
                            }
                        } else if upper.starts_with("UID SEARCH HEADER ") {
                            let wanted = quoted(command).to_ascii_lowercase();
                            let state = shared.lock().unwrap();
                            let uids = state
                                .boxes
                                .get(&selected)
                                .map(|(_, messages)| {
                                    messages
                                        .iter()
                                        .filter(|stored| {
                                            String::from_utf8_lossy(&stored.raw)
                                                .to_ascii_lowercase()
                                                .contains(&format!("message-id: {wanted}"))
                                        })
                                        .map(|stored| format!(" {}", stored.uid))
                                        .collect::<String>()
                                })
                                .unwrap_or_default();
                            format!("* SEARCH{uids}\r\n{tag} OK searched\r\n")
                        } else if upper.starts_with("UID FETCH ") {
                            let set = command.split(' ').nth(2).unwrap_or_default().to_owned();
                            let state = shared.lock().unwrap();
                            let mut out = String::new();
                            if let Some((_, messages)) = state.boxes.get(&selected) {
                                for (number, stored) in messages.iter().enumerate() {
                                    if !set.split(',').any(|uid| uid == stored.uid.to_string()) {
                                        continue;
                                    }
                                    let text = String::from_utf8_lossy(&stored.raw);
                                    let header = text
                                        .lines()
                                        .find(|line| {
                                            line.to_ascii_lowercase().starts_with("message-id:")
                                        })
                                        .map(|line| format!("{line}\r\n\r\n"))
                                        .unwrap_or_else(|| "\r\n".into());
                                    out.push_str(&format!(
                                    "* {} FETCH (UID {} FLAGS ({}) BODY[HEADER.FIELDS (MESSAGE-ID)] {{{}}}\r\n{header})\r\n",
                                    number + 1,
                                    stored.uid,
                                    stored.flags,
                                    header.len()
                                ));
                                }
                            }
                            format!("{out}{tag} OK fetched\r\n")
                        } else if upper.starts_with("APPEND ") {
                            let name = quoted(command);
                            let size: usize = command
                                .rsplit_once('{')
                                .unwrap()
                                .1
                                .trim_end_matches('}')
                                .parse()
                                .unwrap();
                            let flags = command
                                .split_once('(')
                                .and_then(|(_, rest)| rest.split_once(')'))
                                .map(|(flags, _)| flags.to_owned())
                                .unwrap_or_default();
                            let date = command
                                .split('"')
                                .nth(3)
                                .map(str::to_owned)
                                .unwrap_or_default();
                            stream.write_all(b"+ go ahead\r\n").await.unwrap();
                            let mut literal = vec![0; size + 2];
                            stream.read_exact(&mut literal).await.unwrap();
                            literal.truncate(size);
                            let mut state = shared.lock().unwrap();
                            if refuse.is_some_and(|text| {
                                String::from_utf8_lossy(&literal).contains(text)
                            }) {
                                format!("{tag} NO [TOOBIG] message too large\r\n")
                            } else {
                                state.appends += 1;
                                match state.boxes.get_mut(&name) {
                                    Some((next, messages)) => {
                                        let uid = *next;
                                        *next += 1;
                                        messages.push(Stored {
                                            uid,
                                            flags,
                                            date,
                                            raw: literal,
                                        });
                                        format!("{tag} OK [APPENDUID 7 {uid}] appended\r\n")
                                    }
                                    None => format!("{tag} NO [TRYCREATE] no such mailbox\r\n"),
                                }
                            }
                        } else {
                            format!("{tag} BAD unknown command\r\n")
                        };
                        stream.write_all(response.as_bytes()).await.unwrap();
                    }
                });
            }
        });
        Self { port, state, task }
    }

    fn mailbox(&self, name: &str) -> Option<Vec<Stored>> {
        self.state
            .lock()
            .unwrap()
            .boxes
            .get(name)
            .map(|(_, messages)| messages.clone())
    }

    fn appends(&self) -> usize {
        self.state.lock().unwrap().appends
    }

    /// Make a mailbox, as another app would.
    fn create(&self, name: &str) {
        self.state
            .lock()
            .unwrap()
            .boxes
            .insert(name.into(), (1, Vec::new()));
    }
}

impl Drop for ImapServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// The first quoted string of a command.
fn quoted(command: &str) -> String {
    command.split('"').nth(1).unwrap_or_default().to_owned()
}

/// A core with an account of `protocol` on the server at `address`.
async fn account(
    protocol: MailProtocol,
    address: String,
    port: u16,
) -> (tempfile::TempDir, crate::Core, i64) {
    let directory = tempfile::tempdir().unwrap();
    let credentials = Arc::new(DevelopmentFileCredentialStore::new(
        directory.path().join("credentials.json"),
    ));
    let core = crate::Core::start_mail_ui_with_credentials(
        Paths::for_tests(directory.path()),
        credentials,
    )
    .await
    .unwrap();
    let account_id = core
        .db
        .write(move |conn| {
            let mut settings = crate::db::testutil::account_settings();
            settings.connection = MailConnectionSettings {
                incoming_security: ConnectionSecurity::Tls,
                smtp_security: ConnectionSecurity::Tls,
                trusted_certificate_pem: CERT.into(),
            };
            let (provider, auth_kind) = match protocol {
                MailProtocol::Gmail => (Provider::Gmail, AuthKind::Oauth2),
                _ => (Provider::Generic, AuthKind::Password),
            };
            let account_id = repo::accounts::insert(
                conn,
                &repo::accounts::NewAccount {
                    email: "me@example.com",
                    display_name: None,
                    avatar_url: None,
                    provider,
                    auth_kind,
                    mail_protocol: protocol,
                    username: "me@example.com",
                    jmap_url: &address,
                    jmap_account_id: None,
                    incoming_host: "127.0.0.1",
                    incoming_port: port,
                    smtp_host: "127.0.0.1",
                    smtp_port: 465,
                },
                &settings,
            )?;
            if protocol == MailProtocol::Imap {
                let inbox = repo::folders::upsert(
                    conn,
                    account_id,
                    "INBOX",
                    Some("/"),
                    Some(roles::INBOX),
                )?;
                repo::folders::set_backfill(conn, inbox, Some(1), true)?;
            }
            Ok(account_id)
        })
        .await
        .unwrap();
    credentials::store_async(
        core.credentials.clone(),
        account_id,
        Slot::Password,
        "secret".into(),
    )
    .await
    .unwrap();
    (directory, core, account_id)
}

/// A core with an IMAP account on the server at `port`.
async fn imap_account(port: u16) -> (tempfile::TempDir, crate::Core, i64) {
    account(MailProtocol::Imap, String::new(), port).await
}

/// A Thunderbird store: Inbox with a read and starred message and an
/// unread one, and Archives with a subfolder.
fn thunderbird_store() -> (tempfile::TempDir, MailSource) {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path();
    write(
        &root.join("Inbox"),
        &mbox(&[
            message("a", "Read and starred", "X-Mozilla-Status: 0005\r\n"),
            message("b", "Unread", "Status: O\r\n"),
        ]),
    );
    write(&root.join("Archives"), "");
    write(
        &root.join("Archives.sbd/2023"),
        &mbox(&[message("c", "Old news", "")]),
    );
    let source = scan_folder(root).unwrap();
    (folder, source)
}

async fn import_into(core: &crate::Core, source: &MailSource, account_id: i64) -> MailImportReport {
    let (progress, _) = watch::channel(MailImportProgress::default());
    core.import_mail(
        MailImportRequest {
            source: source.clone(),
            folders: (0..source.folders.len()).collect(),
            destination: Destination::Account(account_id),
            parent_folder: Some("Local Folders".into()),
            loose_folder_name: "Imported".into(),
            own_addresses: Vec::new(),
        },
        progress,
        Arc::new(AtomicBool::new(false)),
    )
    .await
    .unwrap()
}

async fn folder(core: &crate::Core, account_id: i64, path: &'static str) -> repo::folders::Folder {
    core.db
        .read(move |conn| {
            Ok(repo::folders::list(conn, Some(account_id))?
                .into_iter()
                .find(|folder| folder.path == path))
        })
        .await
        .unwrap()
        .unwrap_or_else(|| panic!("no folder {path}"))
}

#[tokio::test]
async fn mail_is_uploaded_to_an_imap_account_once() {
    let server = ImapServer::start(false, None).await;
    let (_directory, core, account_id) = imap_account(server.port).await;
    let (_store, source) = thunderbird_store();
    // Another app made the folder since the account last synced.
    server.create("Local Folders");

    let report = import_into(&core, &source, account_id).await;

    assert_eq!(report.imported, 3);
    assert_eq!(report.already_present, 0);
    assert_eq!(report.refused, 0);
    // A folder of its own, with the store's folders inside, on the server
    // and here.
    let inbox = server.mailbox("Local Folders/Inbox").unwrap();
    assert_eq!(inbox.len(), 2);
    assert_eq!(inbox[0].flags, "\\Seen \\Flagged");
    assert_eq!(inbox[1].flags, "");
    assert_eq!(inbox[0].date, "05-Jan-2024 10:00:00 +0000");
    assert_eq!(
        server.mailbox("Local Folders/Archives/2023").unwrap().len(),
        1
    );
    assert!(server.mailbox("Local Folders/Archives").unwrap().is_empty());
    let uploaded = folder(&core, account_id, "Local Folders/Inbox").await;
    assert_eq!(report.folders.len(), 2);
    assert!(report.folders.contains(&uploaded.id));
    // Its sync fetches the uploads whatever their age: none of them is
    // history.
    assert_eq!(uploaded.last_seen_uid, 0);
    assert_eq!(uploaded.backfill_cursor, Some(1));
    assert!(uploaded.backfill_done);

    // Mail the server has isn't uploaded again.
    let again = import_into(&core, &source, account_id).await;
    assert_eq!(again.imported, 0);
    assert_eq!(again.already_present, 3);
    assert_eq!(server.appends(), 3);
}

#[tokio::test]
async fn messages_chosen_one_by_one_go_in_a_folder_of_their_own() {
    let server = ImapServer::start(false, None).await;
    let (_directory, core, account_id) = imap_account(server.port).await;
    let files = tempfile::tempdir().unwrap();
    let paths = ["a", "b"].map(|id| {
        let path = files.path().join(format!("{id}.eml"));
        write(&path, &message(id, "Saved", ""));
        path
    });
    let source =
        crate::mail_import::scan_files(&paths.map(crate::mail_import::ChosenFile::at)).unwrap();
    let (progress, _) = watch::channel(MailImportProgress::default());

    let report = core
        .import_mail(
            MailImportRequest {
                source: source.clone(),
                folders: (0..source.folders.len()).collect(),
                destination: Destination::Account(account_id),
                parent_folder: None,
                loose_folder_name: "Imported messages".into(),
                own_addresses: Vec::new(),
            },
            progress,
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();

    assert_eq!(report.imported, 2);
    assert_eq!(server.mailbox("Imported messages").unwrap().len(), 2);
    assert_eq!(server.appends(), 2);
}

#[tokio::test]
async fn mail_the_account_has_or_the_server_refuses_is_counted() {
    let server = ImapServer::start(true, Some("Old news")).await;
    let (_directory, core, account_id) = imap_account(server.port).await;
    // The account already has one message, in its Inbox here.
    core.db
        .write(move |conn| {
            let (_, id) =
                crate::db::testutil::seed_message(conn, "ana@example.org", "Unread", false);
            conn.execute(
                "UPDATE messages SET account_id = ?1, message_id = 'b@example.org' WHERE id = ?2",
                rusqlite::params![account_id, id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let (_store, source) = thunderbird_store();

    let report = import_into(&core, &source, account_id).await;

    assert_eq!(report.imported, 1);
    assert_eq!(report.already_present, 1);
    assert_eq!(report.refused, 1);
    assert!(report.refusal.unwrap().contains("message too large"));
    // Where folders can't be made at the top, they go in the Inbox.
    assert_eq!(
        server.mailbox("INBOX/Local Folders/Inbox").unwrap().len(),
        1
    );
    assert_eq!(server.appends(), 1);
}

/// A request an HTTP test server answered.
#[derive(Debug, Clone)]
struct HttpRequest {
    method: String,
    target: String,
    authorization: Option<String>,
    content_type: Option<String>,
    body: Vec<u8>,
}

/// An answer: status, content type and body.
type HttpAnswer = (u16, &'static str, String);

/// An HTTP/1.1 server on a local port that keeps the requests it answers.
struct HttpServer {
    origin: String,
    requests: Arc<Mutex<Vec<HttpRequest>>>,
    task: tokio::task::JoinHandle<()>,
}

impl HttpServer {
    async fn start(answer: impl Fn(&HttpRequest) -> HttpAnswer + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let answer = Arc::new(answer);
        let kept = requests.clone();
        let task = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let requests = kept.clone();
                let answer = answer.clone();
                tokio::spawn(async move {
                    let mut stream = BufReader::new(stream);
                    // Like a real server, keep the connection for more requests.
                    loop {
                        let mut line = String::new();
                        if stream.read_line(&mut line).await.unwrap_or(0) == 0 {
                            return;
                        }
                        let mut words = line.split_whitespace();
                        let method = words.next().unwrap_or_default().to_owned();
                        let target = words.next().unwrap_or_default().to_owned();
                        let (mut length, mut authorization, mut content_type) = (0, None, None);
                        loop {
                            let mut header = String::new();
                            if stream.read_line(&mut header).await.unwrap_or(0) == 0 {
                                return;
                            }
                            if header == "\r\n" {
                                break;
                            }
                            if let Some((name, value)) = header.split_once(':') {
                                let value = value.trim().to_owned();
                                match name.to_ascii_lowercase().as_str() {
                                    "content-length" => length = value.parse().unwrap(),
                                    "authorization" => authorization = Some(value),
                                    "content-type" => content_type = Some(value),
                                    _ => {}
                                }
                            }
                        }
                        let mut body = vec![0; length];
                        if stream.read_exact(&mut body).await.is_err() {
                            return;
                        }
                        let request = HttpRequest {
                            method,
                            target,
                            authorization,
                            content_type,
                            body,
                        };
                        let (status, kind, text) = answer(&request);
                        requests.lock().unwrap().push(request);
                        let response = format!(
                            "HTTP/1.1 {status} Answer\r\nContent-Type: {kind}\r\nContent-Length: {}\r\n\r\n{text}",
                            text.len()
                        );
                        if stream
                            .get_mut()
                            .write_all(response.as_bytes())
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                });
            }
        });
        Self {
            origin,
            requests,
            task,
        }
    }

    fn requests(&self) -> Vec<HttpRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for HttpServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[tokio::test]
async fn mail_is_imported_into_gmail_with_its_labels() {
    let gmail = HttpServer::start(|request| {
        let json = "application/json";
        if request.target.starts_with("/gmail/v1/users/me/messages?") {
            // Gmail has "a" already.
            let found = request.target.contains("a%40example.org");
            let messages = if found {
                json!({ "messages": [{ "id": "1", "threadId": "1" }] })
            } else {
                json!({ "resultSizeEstimate": 0 })
            };
            (200, json, messages.to_string())
        } else if request
            .target
            .starts_with("/upload/gmail/v1/users/me/messages/import?")
        {
            if String::from_utf8_lossy(&request.body).contains("Subject: Old news") {
                let error = json!({ "error": { "code": 400, "message": "Message too large" } });
                (400, json, error.to_string())
            } else {
                (200, json, json!({ "id": "2", "threadId": "2" }).to_string())
            }
        } else {
            (404, json, "{}".into())
        }
    })
    .await;
    let (_directory, core, account_id) = account(MailProtocol::Gmail, String::new(), 0).await;
    let ctx = core.sync_ctx();
    ctx.tokens
        .store_initial(
            account_id,
            "access".into(),
            Some(3600),
            Some("refresh".into()),
            "client".into(),
            None,
        )
        .await
        .unwrap();
    let config = core.folder_account_config(account_id).await.unwrap();
    let uploader = sync::gmail::Uploader::with_base(ctx, config, &gmail.origin).unwrap();
    let target = Target {
        folder_id: 1,
        path: "Local Folders/Inbox".into(),
        jmap_id: None,
        gmail_label: Some("Label_7".into()),
    };
    let upload = |id: &str, subject: &str, seen: bool, flagged: bool| {
        let message = incoming(0, message(id, subject, "").into_bytes(), None, None, 0).unwrap();
        let uploader = &uploader;
        let target = &target;
        let id = format!("{id}@example.org");
        async move { gmail_upload(uploader, &message, target, Some(&id), seen, flagged).await }
    };

    assert!(matches!(
        upload("a", "Here already", true, false).await.unwrap(),
        Outcome::Present
    ));
    assert!(matches!(
        upload("b", "Unread and starred", false, true)
            .await
            .unwrap(),
        Outcome::Uploaded
    ));
    assert!(matches!(
        upload("c", "Old news", true, false).await.unwrap(),
        Outcome::Refused(reason) if reason.contains("Message too large")
    ));

    let requests = gmail.requests();
    let imports = requests
        .iter()
        .filter(|request| request.method == "POST")
        .collect::<Vec<_>>();
    assert_eq!(imports.len(), 2);
    let import = imports[0];
    // Dated by its Date header, and kept away from the spam filter.
    assert!(import.target.contains("uploadType=multipart"));
    assert!(import.target.contains("internalDateSource=dateHeader"));
    assert!(import.target.contains("neverMarkSpam=true"));
    assert_eq!(import.authorization.as_deref(), Some("Bearer access"));
    let boundary = import
        .content_type
        .as_deref()
        .and_then(|kind| kind.strip_prefix("multipart/related; boundary="))
        .unwrap();
    let body = String::from_utf8(import.body.clone()).unwrap();
    assert!(body.starts_with(&format!(
        "--{boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n\
         {{\"labelIds\":[\"Label_7\",\"UNREAD\",\"STARRED\"]}}\r\n\
         --{boundary}\r\nContent-Type: message/rfc822\r\n\r\nFrom: Ana"
    )));
    assert!(body.contains("Message-ID: <b@example.org>"));
    assert!(body.ends_with(&format!("Hello.\r\n\r\n--{boundary}--\r\n")));
    // The search looks everywhere, Spam and Trash too.
    assert!(
        requests[0]
            .target
            .contains("includeSpamTrash=true&q=rfc822msgid%3A%3Ca%40example.org%3E")
    );
}

/// A JMAP server's mailboxes, uploads and mail.
#[derive(Default)]
struct JmapState {
    /// Id, name, parent and role of each mailbox.
    mailboxes: Vec<(String, String, Option<String>, Option<&'static str>)>,
    blobs: HashMap<String, Vec<u8>>,
    emails: Vec<JmapEmail>,
}

#[derive(Debug, Clone)]
struct JmapEmail {
    mailbox: String,
    raw: Vec<u8>,
    keywords: Value,
    received_at: Value,
}

/// A JMAP server that refuses uploads with `refuse` in them.
async fn jmap_server(refuse: &'static str) -> (HttpServer, Arc<Mutex<JmapState>>) {
    let state = Arc::new(Mutex::new(JmapState {
        mailboxes: vec![("inbox".into(), "Inbox".into(), None, Some("inbox"))],
        ..JmapState::default()
    }));
    let origin = Arc::new(std::sync::OnceLock::<String>::new());
    let server = HttpServer::start({
        let state = state.clone();
        let origin = origin.clone();
        move |request| jmap_answer(&state, origin.get().unwrap(), refuse, request)
    })
    .await;
    origin.set(server.origin.clone()).unwrap();
    (server, state)
}

fn jmap_answer(
    state: &Mutex<JmapState>,
    origin: &str,
    refuse: &str,
    request: &HttpRequest,
) -> HttpAnswer {
    let json = "application/json";
    let mut state = state.lock().unwrap();
    if request.method == "GET" {
        let mail = json!({
            "maxMailboxesPerEmail": null, "maxMailboxDepth": 10,
            "maxSizeMailboxName": 255, "maxSizeAttachmentsPerEmail": 50_000_000,
            "emailQuerySortOptions": ["receivedAt"], "mayCreateTopLevelMailbox": true
        });
        let submission = json!({ "maxDelayedSend": 0, "submissionExtensions": [] });
        let session = json!({
            "capabilities": {
                "urn:ietf:params:jmap:core": {
                    "maxSizeUpload": 50_000_000, "maxConcurrentUpload": 4,
                    "maxSizeRequest": 10_000_000, "maxConcurrentRequests": 4,
                    "maxCallsInRequest": 16, "maxObjectsInGet": 500,
                    "maxObjectsInSet": 500, "collationAlgorithms": []
                },
                "urn:ietf:params:jmap:mail": mail,
                "urn:ietf:params:jmap:submission": submission
            },
            "accounts": { "mail": {
                "name": "Test", "isPersonal": true, "isReadOnly": false,
                "accountCapabilities": {
                    "urn:ietf:params:jmap:mail": mail,
                    "urn:ietf:params:jmap:submission": submission
                }
            }},
            "primaryAccounts": { "urn:ietf:params:jmap:mail": "mail" },
            "username": "me@example.com", "apiUrl": format!("{origin}/api"),
            "downloadUrl": format!("{origin}/download/{{accountId}}/{{blobId}}/{{name}}?accept={{type}}"),
            "uploadUrl": format!("{origin}/upload/{{accountId}}"),
            "eventSourceUrl": format!("{origin}/events?types={{types}}&closeafter={{closeafter}}&ping={{ping}}"),
            "state": "session"
        });
        return (200, json, session.to_string());
    }
    if request.target == "/upload/mail" {
        if String::from_utf8_lossy(&request.body).contains(refuse) {
            let problem = json!({
                "type": "urn:ietf:params:jmap:error:limit", "status": 400,
                "detail": "The message has too many recipients"
            });
            return (400, "application/problem+json", problem.to_string());
        }
        let blob_id = format!("blob-{}", state.blobs.len() + 1);
        state.blobs.insert(blob_id.clone(), request.body.clone());
        let uploaded = json!({
            "accountId": "mail", "blobId": blob_id,
            "type": "message/rfc822", "size": request.body.len()
        });
        return (201, json, uploaded.to_string());
    }
    let rights = json!({
        "mayReadItems": true, "mayAddItems": true, "mayRemoveItems": true,
        "maySetSeen": true, "maySetKeywords": true, "mayCreateChild": true,
        "mayRename": true, "mayDelete": true, "maySubmit": true
    });
    let call: Value = serde_json::from_slice(&request.body).unwrap();
    let mut responses = Vec::new();
    for call in call["methodCalls"].as_array().unwrap() {
        let (method, arguments) = (call[0].as_str().unwrap(), &call[1]);
        let response = match method {
            "Mailbox/get" => {
                let list = state
                    .mailboxes
                    .iter()
                    .map(|(id, name, parent, role)| {
                        json!({
                            "id": id, "name": name, "parentId": parent,
                            "role": role, "myRights": rights
                        })
                    })
                    .collect::<Vec<_>>();
                json!({ "accountId": "mail", "state": "mailboxes", "list": list, "notFound": [] })
            }
            "Mailbox/set" => {
                let mut created = serde_json::Map::new();
                for (create_id, mailbox) in arguments["create"].as_object().unwrap() {
                    let id = format!("mailbox-{}", state.mailboxes.len() + 1);
                    state.mailboxes.push((
                        id.clone(),
                        mailbox["name"].as_str().unwrap().to_owned(),
                        mailbox["parentId"].as_str().map(str::to_owned),
                        None,
                    ));
                    created.insert(
                        create_id.clone(),
                        json!({
                            "id": id, "myRights": rights, "totalEmails": 0,
                            "unreadEmails": 0, "totalThreads": 0, "unreadThreads": 0,
                            "isSubscribed": true, "sortOrder": 0
                        }),
                    );
                }
                json!({
                    "accountId": "mail", "oldState": "mailboxes",
                    "newState": "mailboxes", "created": created
                })
            }
            "Email/query" => {
                let conditions = arguments["filter"]["conditions"].as_array().unwrap();
                let mailbox = conditions
                    .iter()
                    .find_map(|condition| condition["inMailbox"].as_str())
                    .unwrap();
                let header = conditions
                    .iter()
                    .find_map(|condition| condition["header"].as_array())
                    .unwrap();
                assert_eq!(header[0], "Message-ID");
                let wanted = format!("message-id: {}", header[1].as_str().unwrap());
                let ids = state
                    .emails
                    .iter()
                    .enumerate()
                    .filter(|(_, email)| {
                        email.mailbox == mailbox
                            && String::from_utf8_lossy(&email.raw)
                                .to_ascii_lowercase()
                                .contains(&wanted.to_ascii_lowercase())
                    })
                    .map(|(index, _)| format!("email-{}", index + 1))
                    .collect::<Vec<_>>();
                json!({
                    "accountId": "mail", "queryState": "emails",
                    "canCalculateChanges": false, "position": 0, "ids": ids
                })
            }
            "Email/import" => {
                let mut created = serde_json::Map::new();
                for (create_id, email) in arguments["emails"].as_object().unwrap() {
                    let raw = state.blobs[email["blobId"].as_str().unwrap()].clone();
                    let mailboxes = email["mailboxIds"].as_object().unwrap();
                    assert_eq!(mailboxes.len(), 1);
                    state.emails.push(JmapEmail {
                        mailbox: mailboxes.keys().next().unwrap().clone(),
                        raw,
                        keywords: email["keywords"].clone(),
                        received_at: email["receivedAt"].clone(),
                    });
                    let id = format!("email-{}", state.emails.len());
                    created.insert(
                        create_id.clone(),
                        json!({ "id": id, "blobId": email["blobId"], "threadId": id, "size": 100 }),
                    );
                }
                json!({
                    "accountId": "mail", "oldState": "emails",
                    "newState": "emails", "created": created
                })
            }
            other => panic!("unexpected JMAP method {other}"),
        };
        responses.push(json!([method, response, call[2]]));
    }
    let answer = json!({ "methodResponses": responses, "sessionState": "session" });
    (200, json, answer.to_string())
}

#[tokio::test]
async fn mail_is_imported_into_a_jmap_account_once() {
    let (server, state) = jmap_server("Subject: Old news").await;
    let (_directory, core, account_id) =
        account(MailProtocol::Jmap, server.origin.clone(), 0).await;
    let (_store, source) = thunderbird_store();

    let report = import_into(&core, &source, account_id).await;

    assert_eq!(report.imported, 2);
    assert_eq!(report.refused, 1);
    assert!(report.refusal.unwrap().contains("too many recipients"));
    let (mailboxes, emails) = {
        let state = state.lock().unwrap();
        (state.mailboxes.clone(), state.emails.clone())
    };
    let named = |name: &str, parent: Option<&str>| {
        mailboxes
            .iter()
            .find(|(_, known, known_parent, _)| known == name && known_parent.as_deref() == parent)
            .map(|(id, ..)| id.clone())
            .unwrap_or_else(|| panic!("no mailbox {name}"))
    };
    let root = named("Local Folders", None);
    let inbox = named("Inbox", Some(&root));
    named("2023", Some(&named("Archives", Some(&root))));
    assert_eq!(emails.len(), 2);
    assert!(emails.iter().all(|email| email.mailbox == inbox));
    assert_eq!(
        emails[0].keywords,
        json!({ "$seen": true, "$flagged": true })
    );
    assert_eq!(emails[1].keywords, json!({}));
    assert_eq!(emails[0].received_at, "2024-01-05T10:00:00Z");
    let uploaded = folder(&core, account_id, "Local Folders / Inbox").await;
    assert_eq!(uploaded.jmap_id.as_deref(), Some(inbox.as_str()));
    assert!(report.folders.contains(&uploaded.id));

    // What the server has isn't imported again; what it refused, it refuses
    // again.
    let again = import_into(&core, &source, account_id).await;
    assert_eq!(again.imported, 0);
    assert_eq!(again.already_present, 2);
    assert_eq!(again.refused, 1);
    assert_eq!(state.lock().unwrap().emails.len(), 2);
    assert_eq!(
        state
            .lock()
            .unwrap()
            .mailboxes
            .iter()
            .filter(|(_, name, ..)| name == "Local Folders")
            .count(),
        1
    );
}
