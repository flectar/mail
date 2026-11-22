use super::*;
use crate::config::Paths;
use crate::mail_import::{
    Destination, MailImportProgress, MailImportRequest, MailSource, scan_folder,
};
use crate::models::{AuthKind, Provider};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tokio::sync::{broadcast, watch};

fn message(id: &str, subject: &str, status: &str) -> String {
    format!(
        "From ana@example.org Fri Jan  5 10:00:00 2024\r\n\
         From: Ana <ana@example.org>\r\nTo: me@example.com\r\nSubject: {subject}\r\n\
         Date: Fri, 05 Jan 2024 10:00:00 +0000\r\nMessage-ID: <{id}@example.org>\r\n\
         X-Mozilla-Status: {status}\r\n\r\nHello.\r\n\r\n"
    )
}

/// Mail of a POP3 account, which this app keeps itself, and local mail with
/// a folder to transfer it to.
struct Accounts {
    _directory: tempfile::TempDir,
    core: crate::Core,
    pop3: i64,
    local: i64,
    events: broadcast::Receiver<CoreEvent>,
}

async fn import(core: &crate::Core, source: &MailSource, destination: Destination) -> i64 {
    let (progress, _) = watch::channel(MailImportProgress::default());
    core.import_mail(
        MailImportRequest {
            source: source.clone(),
            folders: (0..source.folders.len()).collect(),
            destination,
            parent_folder: None,
            loose_folder_name: "Imported".into(),
            own_addresses: Vec::new(),
        },
        progress,
        Arc::new(AtomicBool::new(false)),
    )
    .await
    .unwrap()
    .account_id
}

async fn accounts(directory: tempfile::TempDir) -> Accounts {
    let core = crate::Core::start_mail_ui(Paths::for_tests(directory.path()))
        .await
        .unwrap();
    let pop3 = core
        .db
        .write(|conn| {
            repo::accounts::insert(
                conn,
                &repo::accounts::NewAccount {
                    email: "me@example.com",
                    display_name: None,
                    avatar_url: None,
                    provider: Provider::Generic,
                    auth_kind: AuthKind::Password,
                    mail_protocol: MailProtocol::Pop3,
                    username: "me",
                    jmap_url: "",
                    jmap_account_id: None,
                    incoming_host: "pop.example.com",
                    incoming_port: 995,
                    smtp_host: "smtp.example.com",
                    smtp_port: 465,
                },
                &crate::db::testutil::account_settings(),
            )
        })
        .await
        .unwrap();
    let files = tempfile::tempdir().unwrap();
    let write = |name: &str, contents: String| {
        std::fs::write(files.path().join(name), contents).unwrap();
    };
    // Read and starred, and unread.
    write(
        "Inbox",
        message("a", "Receipt", "0005") + &message("b", "Itinerary", "0000"),
    );
    import(
        &core,
        &scan_folder(files.path()).unwrap(),
        Destination::Account(pop3),
    )
    .await;
    let local_files = tempfile::tempdir().unwrap();
    std::fs::write(
        local_files.path().join("Trips"),
        message("z", "Older trip", "0001"),
    )
    .unwrap();
    let local = import(
        &core,
        &scan_folder(local_files.path()).unwrap(),
        Destination::Local,
    )
    .await;
    let events = core.bus.subscribe();
    Accounts {
        _directory: directory,
        core,
        pop3,
        local,
        events,
    }
}

impl Accounts {
    async fn folder(&self, account_id: i64, path: &'static str) -> i64 {
        self.core
            .db
            .read(move |conn| {
                Ok(repo::folders::list(conn, Some(account_id))?
                    .into_iter()
                    .find(|folder| folder.path == path)
                    .map(|folder| folder.id))
            })
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("no folder {path}"))
    }

    async fn message(&self, account_id: i64, subject: &'static str) -> Option<i64> {
        self.core
            .db
            .read(move |conn| {
                Ok(conn
                    .query_row(
                        "SELECT id FROM messages WHERE account_id = ?1 AND subject = ?2",
                        params![account_id, subject],
                        |row| row.get(0),
                    )
                    .optional()?)
            })
            .await
            .unwrap()
    }

    /// (folder, subject, read, starred, date) of each of an account's
    /// messages.
    async fn mail(&self, account_id: i64) -> Vec<(String, String, bool, bool, i64)> {
        self.core
            .db
            .read(move |conn| {
                let mut statement = conn.prepare(
                    "SELECT f.path, m.subject, m.is_read, m.is_starred, m.date
                     FROM messages m JOIN folders f ON f.id = m.folder_id
                     WHERE m.account_id = ?1 ORDER BY f.path, m.subject",
                )?;
                Ok(statement
                    .query_map([account_id], |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                        ))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?)
            })
            .await
            .unwrap()
    }

    /// What became of the next run of transfers.
    async fn report(&mut self) -> TransferReport {
        let wait = async {
            loop {
                if let Ok(CoreEvent::TransfersFinished(report)) = self.events.recv().await {
                    return report;
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(20), wait)
            .await
            .expect("the transfers ended")
    }

    async fn transfers(&self) -> i64 {
        self.core
            .db
            .read(|conn| {
                Ok(conn.query_row("SELECT COUNT(*) FROM mail_transfers", [], |row| row.get(0))?)
            })
            .await
            .unwrap()
    }

    fn transfer_files(&self) -> usize {
        std::fs::read_dir(self.core.paths.transfers_dir()).map_or(0, |files| files.count())
    }
}

/// 2024-01-05 10:00 UTC, the messages' date.
const DATE: i64 = 1_704_448_800_000;

#[tokio::test]
async fn a_move_to_another_account_keeps_the_message_and_its_state() {
    let mut accounts = accounts(tempfile::tempdir().unwrap()).await;
    let receipt = accounts.message(accounts.pop3, "Receipt").await.unwrap();
    let trips = accounts.folder(accounts.local, "Trips").await;

    let started = accounts
        .core
        .transfer_mail(ActionTarget::Messages(vec![receipt]), trips, true)
        .await
        .unwrap();

    assert_eq!(started, 1);
    assert_eq!(
        accounts.report().await,
        TransferReport {
            moved: 1,
            ..TransferReport::default()
        }
    );
    assert_eq!(
        accounts.mail(accounts.local).await,
        [
            ("Trips".into(), "Older trip".into(), true, false, DATE),
            ("Trips".into(), "Receipt".into(), true, true, DATE),
        ],
        "the copy keeps its date and its read and starred states"
    );
    assert_eq!(
        accounts.mail(accounts.pop3).await,
        [("Inbox".into(), "Itinerary".into(), false, false, DATE)],
        "the original goes once the copy is in place"
    );
    assert_eq!(accounts.transfers().await, 0);
    assert_eq!(accounts.transfer_files(), 0);
}

#[tokio::test]
async fn a_copy_leaves_the_original_and_is_made_once() {
    let mut accounts = accounts(tempfile::tempdir().unwrap()).await;
    let pop3 = accounts.pop3;
    let trips = accounts.folder(accounts.local, "Trips").await;
    let inbox = accounts.folder(pop3, "Inbox").await;
    let thread = |subject: &'static str| {
        let core = accounts.core.clone();
        async move {
            core.db
                .read(move |conn| {
                    Ok(conn.query_row(
                        "SELECT thread_id FROM messages WHERE account_id = ?1 AND subject = ?2",
                        params![pop3, subject],
                        |row| row.get::<_, i64>(0),
                    )?)
                })
                .await
                .unwrap()
        }
    };
    let conversation = ActionTarget::Threads(vec![thread("Itinerary").await]);

    assert_eq!(
        accounts
            .core
            .transfer_mail(conversation.clone(), trips, false)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        accounts.report().await,
        TransferReport {
            copied: 1,
            ..TransferReport::default()
        }
    );
    assert_eq!(
        accounts.mail(pop3).await.len(),
        2,
        "a copy leaves the original"
    );

    // The same copy again finds it there.
    accounts
        .core
        .transfer_mail(conversation, trips, false)
        .await
        .unwrap();
    assert_eq!(
        accounts.report().await,
        TransferReport {
            present: 1,
            ..TransferReport::default()
        }
    );
    assert_eq!(accounts.mail(accounts.local).await.len(), 2);

    // Mail already in the folder's account doesn't transfer.
    let receipt = accounts.message(pop3, "Receipt").await.unwrap();
    assert_eq!(
        accounts
            .core
            .transfer_mail(ActionTarget::Messages(vec![receipt]), inbox, true)
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn a_transfer_a_restart_interrupted_carries_on_from_its_stage() {
    let directory = tempfile::tempdir().unwrap();
    // A file no transfer refers to any more.
    let paths = Paths::for_tests(directory.path());
    std::fs::create_dir_all(paths.transfers_dir()).unwrap();
    let stray = paths.transfers_dir().join("7.eml");
    std::fs::write(&stray, "left over").unwrap();
    let mut accounts = accounts(directory).await;
    let pop3 = accounts.pop3;
    let receipt = accounts.message(pop3, "Receipt").await.unwrap();
    let trips = accounts.folder(accounts.local, "Trips").await;
    assert!(!stray.exists(), "a stray file goes when the app starts");

    // A move fetched before the app stopped, from a message since gone
    // from its folder: the upload carries on from the fetched file.
    let original = accounts
        .core
        .db
        .read(move |conn| Ok(repo::messages::get_row(conn, receipt)?.unwrap().raw_path))
        .await
        .unwrap()
        .unwrap();
    let staged = paths.transfers_dir().join("1.eml");
    std::fs::copy(&original, &staged).unwrap();
    let staged_path = staged.to_string_lossy().into_owned();
    accounts
        .core
        .db
        .write(move |conn| {
            let id = repo::transfers::insert(
                conn,
                &NewTransfer {
                    source_message_id: receipt,
                    source_account_id: pop3,
                    target_folder_id: trips,
                    is_move: true,
                    rfc_message_id: Some("a@example.org".into()),
                    seen: true,
                    flagged: true,
                    received_at: Some(DATE),
                },
                0,
            )?
            .unwrap();
            repo::transfers::advance(conn, id, Stage::Upload, Some(&staged_path))?;
            conn.execute(
                "UPDATE messages SET raw_path = NULL WHERE id = ?1",
                [receipt],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    accounts.core.transfers.notify_one();

    assert_eq!(
        accounts.report().await,
        TransferReport {
            moved: 1,
            ..TransferReport::default()
        }
    );
    assert!(accounts.message(accounts.local, "Receipt").await.is_some());
    assert!(accounts.message(accounts.pop3, "Receipt").await.is_none());
    assert!(!staged.exists());
}

#[tokio::test]
async fn a_transfer_that_cannot_happen_is_reported_and_leaves_the_original() {
    let mut accounts = accounts(tempfile::tempdir().unwrap()).await;
    let receipt = accounts.message(accounts.pop3, "Receipt").await.unwrap();
    let trips = accounts.folder(accounts.local, "Trips").await;
    // The original is gone from this device, and a POP3 server keeps no
    // copy of mail it handed over long ago.
    let original = accounts
        .core
        .db
        .read(move |conn| Ok(repo::messages::get_row(conn, receipt)?.unwrap().raw_path))
        .await
        .unwrap()
        .unwrap();
    std::fs::remove_file(Path::new(&original)).unwrap();

    accounts
        .core
        .transfer_mail(ActionTarget::Messages(vec![receipt]), trips, true)
        .await
        .unwrap();

    let report = accounts.report().await;
    assert_eq!(report.failed, 1);
    assert!(report.error.is_some());
    assert!(accounts.message(accounts.pop3, "Receipt").await.is_some());
    assert!(accounts.message(accounts.local, "Receipt").await.is_none());
    assert_eq!(accounts.transfers().await, 0);
}

#[tokio::test]
async fn forwarded_messages_attach_as_they_arrived_without_their_bcc() {
    let accounts = accounts(tempfile::tempdir().unwrap()).await;
    let receipt = accounts.message(accounts.pop3, "Receipt").await.unwrap();
    let itinerary = accounts.message(accounts.pop3, "Itinerary").await.unwrap();
    // A copy of mail sent from here keeps the addresses it was blind copied to.
    let raw_path: String = accounts
        .core
        .db
        .read(move |conn| {
            Ok(conn.query_row(
                "SELECT raw_path FROM messages WHERE id = ?1",
                params![receipt],
                |row| row.get(0),
            )?)
        })
        .await
        .unwrap();
    let original = std::fs::read(&raw_path).unwrap();
    let mut with_bcc = b"Bcc: secret@example.org,\r\n  other@example.org\r\n".to_vec();
    with_bcc.extend_from_slice(&original);
    std::fs::write(&raw_path, &with_bcc).unwrap();

    let chosen = accounts
        .core
        .forwarded_messages(ActionTarget::Messages(vec![itinerary, receipt]))
        .await
        .unwrap();
    assert_eq!(
        chosen
            .iter()
            .map(|message| (
                message.message_id,
                message.account_id,
                message.subject.as_str()
            ))
            .collect::<Vec<_>>(),
        [
            (itinerary, accounts.pop3, "Itinerary"),
            (receipt, accounts.pop3, "Receipt")
        ]
    );
    let thread: i64 = accounts
        .core
        .db
        .read(move |conn| {
            Ok(conn.query_row(
                "SELECT thread_id FROM messages WHERE id = ?1",
                params![receipt],
                |row| row.get(0),
            )?)
        })
        .await
        .unwrap();
    let conversation = accounts
        .core
        .forwarded_messages(ActionTarget::Threads(vec![thread]))
        .await
        .unwrap();
    assert_eq!(conversation.len(), 1);
    assert_eq!(conversation[0].message_id, receipt);
    assert_eq!(conversation[0].thread_id, Some(thread));

    let mut originals = accounts.core.forwarded_originals(vec![receipt]).await;
    assert_eq!(originals.len(), 1);
    let (id, file) = originals.remove(0);
    assert_eq!(id, receipt);
    let path = file.unwrap();
    assert!(path.starts_with(accounts.core.paths.temp_dir()));
    assert_eq!(
        std::fs::read(&path).unwrap(),
        original,
        "the message as it arrived, without its Bcc field"
    );
    // The file goes with its path.
    let kept = path.to_path_buf();
    drop(path);
    assert!(!kept.exists());
}

/// Show original shows the message as it arrived, every header field
/// included, the Bcc of mail sent from here too (#253).
#[tokio::test]
async fn a_message_shows_its_original_with_every_header_field() {
    let accounts = accounts(tempfile::tempdir().unwrap()).await;
    let receipt = accounts.message(accounts.pop3, "Receipt").await.unwrap();
    let raw_path: String = accounts
        .core
        .db
        .read(move |conn| {
            Ok(conn.query_row(
                "SELECT raw_path FROM messages WHERE id = ?1",
                params![receipt],
                |row| row.get(0),
            )?)
        })
        .await
        .unwrap();
    let mut original = b"Bcc: secret@example.org\r\n".to_vec();
    original.extend_from_slice(&std::fs::read(&raw_path).unwrap());
    std::fs::write(&raw_path, &original).unwrap();

    assert_eq!(
        accounts.core.message_original(receipt).await.unwrap(),
        original
    );
    assert!(accounts.core.message_original(i64::MAX).await.is_err());
}
