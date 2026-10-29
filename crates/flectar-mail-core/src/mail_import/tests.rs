use super::*;
use crate::config::Paths;
use crate::db::repo;
use crate::migration::{Source, SourceError};
use crate::models::{
    ActionKind, AuthKind, MailProtocol, PerformActionArgs, Provider, now_ms, roles,
};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tokio::sync::watch;

fn message(id: &str, subject: &str, extra: &str) -> String {
    format!(
        "{extra}From: Ana <ana@example.org>\r\nTo: me@example.com\r\nSubject: {subject}\r\n\
         Date: Mon, 1 Jan 2001 10:00:00 +0000\r\nMessage-ID: <{id}@example.org>\r\n\r\n\
         Hello from {subject}.\r\n"
    )
}

/// An mbox file the way Thunderbird writes one.
fn mbox(messages: &[String]) -> Vec<u8> {
    let mut file = Vec::new();
    for message in messages {
        file.extend_from_slice(b"From - Mon Jan  1 10:00:00 2001\r\n");
        file.extend_from_slice(message.as_bytes());
        file.extend_from_slice(b"\r\n");
    }
    file
}

fn write(path: &Path, bytes: impl AsRef<[u8]>) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn paths(source: &MailSource) -> Vec<String> {
    source
        .folders
        .iter()
        .map(|folder| folder.path.join("/"))
        .collect()
}

#[test]
fn thunderbird_stores_are_listed_with_their_subfolders() {
    let store = tempfile::tempdir().unwrap();
    let root = store.path();
    write(&root.join("Inbox"), mbox(&[message("a", "One", "")]));
    write(
        &root.join("Inbox.msf"),
        "// <!-- <mdb:mork:z v=\"1.4\"/> -->",
    );
    write(&root.join("Trash"), mbox(&[message("b", "Two", "")]));
    // Archives holds only subfolders, and Unsent Messages nothing.
    write(&root.join("Archives"), "");
    write(
        &root.join("Archives.sbd/2019"),
        mbox(&[message("c", "Three", "")]),
    );
    write(&root.join("Unsent Messages"), "");
    write(&root.join("popstate.dat"), "# POP3 State File");
    write(&root.join("msgFilterRules.dat"), "version=\"9\"");

    let source = scan_folder(root).unwrap();

    assert_eq!(
        paths(&source),
        ["Archives", "Archives/2019", "Inbox", "Trash"]
    );
    let kinds = source
        .folders
        .iter()
        .map(|folder| folder.kind)
        .collect::<Vec<_>>();
    assert_eq!(
        kinds,
        [
            FolderKind::Archive,
            FolderKind::Other,
            FolderKind::Inbox,
            FolderKind::Trash
        ]
    );
    assert!(source.folders[0].parts.is_empty());
    assert_eq!(source.folders[0].size, 0);
    assert_eq!(
        source.folders[2].size,
        std::fs::metadata(root.join("Inbox")).unwrap().len()
    );
}

#[test]
fn exports_bundles_and_loose_messages_are_listed() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path().join("Saved");
    // Flectar's exports keep subfolders in a folder named like the file.
    write(&root.join("Work.mbox"), mbox(&[message("a", "One", "")]));
    write(&root.join("Work/Q1.mbox"), mbox(&[message("b", "Two", "")]));
    // Apple Mail's exports are folders holding a file `mbox`.
    write(
        &root.join("Old.mbox/mbox"),
        mbox(&[message("c", "Three", "")]),
    );
    write(&root.join("Old.mbox/table_of_contents"), [0u8, 1, 2, 3]);
    write(&root.join("Old.mbox/Info.plist"), "<plist/>");
    write(
        &root.join("Old.mbox/Sub.mbox/mbox"),
        mbox(&[message("d", "Four", "")]),
    );
    write(&root.join("a.eml"), message("e", "Five", ""));
    write(&root.join("notes.txt"), "From me, a note");
    write(&root.join("photo.jpg"), [0xFFu8, 0xD8]);

    let source = scan_folder(&root).unwrap();

    assert_eq!(
        paths(&source),
        ["Old", "Old/Sub", "Saved", "Work", "Work/Q1"]
    );
    assert_eq!(source.folders[2].parts.len(), 1);
}

#[test]
fn maildirs_are_listed_with_dovecot_and_thunderbird_subfolders() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path().join("Maildir");
    write(&root.join("cur/1.M1.host:2,S"), message("a", "One", ""));
    write(&root.join("new/2.M2.host"), message("b", "Two", ""));
    write(&root.join("tmp/3.M3.host"), message("c", "Partial", ""));
    write(
        &root.join(".Sent/cur/4.M4.host:2,S"),
        message("d", "Three", ""),
    );
    write(
        &root.join(".Work.Projects/cur/5.M5.host:2,"),
        message("e", "Four", ""),
    );
    let source = scan_folder(&root).unwrap();
    assert_eq!(
        paths(&source),
        [
            "Maildir",
            "Maildir/Sent",
            "Maildir/Work",
            "Maildir/Work/Projects"
        ]
    );
    assert_eq!(source.folders[0].parts.len(), 2);

    // Thunderbird's maildir store keeps subfolders in `<name>.sbd`.
    let store = tempfile::tempdir().unwrap();
    write(
        &store.path().join("Inbox/cur/1700000000000"),
        message("f", "Five", ""),
    );
    write(&store.path().join("Inbox/tmp/.keep"), "");
    write(
        &store.path().join("Inbox.sbd/Sub/cur/1700000000001"),
        message("g", "Six", ""),
    );
    let source = scan_folder(store.path()).unwrap();
    assert_eq!(paths(&source), ["Inbox", "Inbox/Sub"]);
}

#[test]
fn chosen_files_and_archives_are_recognized_by_their_content() {
    let folder = tempfile::tempdir().unwrap();
    let mbox_path = folder.path().join("export");
    write(&mbox_path, mbox(&[message("a", "One", "")]));
    // Mobile pickers can drop extensions.
    let single = folder.path().join("upload");
    write(&single, message("b", "Two", ""));
    let text = folder.path().join("notes");
    write(&text, "Subject line without a header\r\n");

    let source = scan_files(&[single.clone(), mbox_path.clone()]).unwrap();
    assert_eq!(paths(&source), ["", "export"]);
    assert!(matches!(
        crate::migration::open(&single).unwrap(),
        Source::Mail(mail) if mail.folders.len() == 1 && mail.folders[0].path.is_empty()
    ));
    assert_eq!(scan_files(&[text]), Err(SourceError::UnknownFormat));

    let archive_path = folder.path().join("takeout.zip");
    let mut archive = zip::ZipWriter::new(std::fs::File::create(&archive_path).unwrap());
    let options = zip::write::SimpleFileOptions::default();
    use std::io::Write as _;
    archive
        .start_file("Takeout/Mail/All mail.mbox", options)
        .unwrap();
    archive
        .write_all(&mbox(&[message(
            "c",
            "Three",
            "X-Gmail-Labels: Inbox,Unread\r\n",
        )]))
        .unwrap();
    archive
        .start_file("Takeout/archive_browser.html", options)
        .unwrap();
    archive.write_all(b"<html></html>").unwrap();
    archive.finish().unwrap();
    let Source::Mail(mail) = crate::migration::open(&archive_path).unwrap() else {
        panic!("expected mail");
    };
    assert_eq!(
        paths(&mail),
        ["Takeout", "Takeout/Mail", "Takeout/Mail/All mail"]
    );
    assert!(mail.archive.is_some());

    let empty = tempfile::tempdir().unwrap();
    assert_eq!(scan_folder(empty.path()), Err(SourceError::NoMail));
}

async fn core() -> (tempfile::TempDir, crate::Core) {
    let directory = tempfile::tempdir().unwrap();
    let core = crate::Core::start_mail_ui(Paths::for_tests(directory.path()))
        .await
        .unwrap();
    (directory, core)
}

async fn import(
    core: &crate::Core,
    source: &MailSource,
    destination: Destination,
) -> MailImportReport {
    let (progress, _) = watch::channel(MailImportProgress::default());
    core.import_mail(
        MailImportRequest {
            source: source.clone(),
            folders: (0..source.folders.len()).collect(),
            destination,
            parent_folder: None,
            loose_folder_name: "Imported".into(),
            own_addresses: vec!["me@example.com".into()],
        },
        progress,
        Arc::new(AtomicBool::new(false)),
    )
    .await
    .unwrap()
}

/// (folder path, subject, read, starred, outgoing) of every message.
async fn messages(core: &crate::Core, account_id: i64) -> Vec<(String, String, bool, bool, bool)> {
    core.db
        .read(move |conn| {
            let mut statement = conn.prepare(
                "SELECT f.path, m.subject, m.is_read, m.is_starred, m.is_outgoing
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

#[tokio::test]
async fn saved_mail_goes_into_local_mail_once() {
    let (_directory, core) = core().await;
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path().join("Local Folders");
    write(
        &root.join("Inbox"),
        mbox(&[
            message("read", "Read and starred", "X-Mozilla-Status: 0005\r\n"),
            message("deleted", "Deleted", "X-Mozilla-Status: 0009\r\n"),
            message("unread", "Unread", "Status: O\r\n"),
        ]),
    );
    write(
        &root.join("Trash"),
        mbox(&[message("trashed", "Trashed", "")]),
    );
    write(
        &root.join("Sent"),
        mbox(&[message("sent", "Sent", "")
            .replace("From: Ana <ana@example.org>", "From: Me <me@example.com>")]),
    );
    let source = scan_folder(&root).unwrap();
    assert!(core.local_mail().await.unwrap().is_none());

    let report = import(&core, &source, Destination::Local).await;

    assert_eq!(report.imported, 4);
    assert_eq!(report.already_present, 0);
    let local = core.local_mail().await.unwrap().unwrap();
    assert_eq!(local.account_id, report.account_id);
    assert_eq!(local.messages, 4);
    assert!(core.list_accounts().await.unwrap().is_empty());
    let row = |path: &str, subject: &str, read, starred, outgoing| {
        (path.to_owned(), subject.to_owned(), read, starred, outgoing)
    };
    assert_eq!(
        messages(&core, report.account_id).await,
        [
            row("Inbox", "Read and starred", true, true, false),
            row("Inbox", "Unread", false, false, false),
            // Thunderbird's Sent is an ordinary folder here; the sender's
            // address still marks its mail as sent.
            row("Sent", "Sent", true, false, true),
            row("Trash", "Trashed", true, false, false),
        ]
    );
    let trash = core
        .db
        .read(move |conn| repo::folders::by_role(conn, report.account_id, roles::TRASH))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(trash.path, "Trash");
    let originals = std::fs::read_dir(core.paths.mail_dir(report.account_id))
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .ends_with(ORIGINAL_SUFFIX)
        })
        .count();
    assert_eq!(originals, 4);

    // Local mail lists, searches and counts like any account's.
    let threads = core
        .list_threads(
            crate::models::View::All,
            None,
            Some(report.account_id),
            None,
            None,
            (None, 50),
        )
        .await
        .unwrap()
        .threads;
    assert_eq!(threads.len(), 4);
    assert!(threads.iter().all(|thread| thread.account_email.is_none()));
    let found = core
        .search_chronological_page("starred".into(), None, None, 50)
        .await
        .unwrap();
    assert_eq!(found.threads.len(), 1);
    let badges = core.mailbox_badge_counts().await.unwrap();
    assert!(
        badges
            .iter()
            .any(|counts| counts.account_id == report.account_id)
    );

    // Importing the same mail again adds nothing.
    let again = import(&core, &source, Destination::Local).await;
    assert_eq!(again.imported, 0);
    assert_eq!(again.already_present, 4);
    assert_eq!(core.local_mail().await.unwrap().unwrap().messages, 4);
}

#[tokio::test]
async fn pop3_accounts_get_their_mail_in_their_own_folders() {
    let (_directory, core) = core().await;
    let account_id = core
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
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path();
    write(&root.join("Inbox"), mbox(&[message("a", "Welcome", "")]));
    write(&root.join("Archives"), "");
    write(
        &root.join("Archives.sbd/2019"),
        mbox(&[message("b", "Old news", "")]),
    );
    write(&root.join("Projects"), mbox(&[message("c", "Plans", "")]));
    let source = scan_folder(root).unwrap();

    let report = import(&core, &source, Destination::Account(account_id)).await;

    assert_eq!(report.imported, 3);
    assert_eq!(
        messages(&core, account_id)
            .await
            .into_iter()
            .map(|(path, ..)| path)
            .collect::<Vec<_>>(),
        ["Archive/2019", "Inbox", "Projects"]
    );
    let (inbox_role, archive_parent) = core
        .db
        .read(move |conn| {
            let inbox = repo::folders::by_role(conn, account_id, roles::INBOX)?.unwrap();
            let archive = repo::folders::by_role(conn, account_id, roles::ARCHIVE)?.unwrap();
            let year = repo::folders::list(conn, Some(account_id))?
                .into_iter()
                .find(|folder| folder.path == "Archive/2019")
                .unwrap();
            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM messages WHERE folder_id = ?1",
                [inbox.id],
                |row| row.get(0),
            )?;
            Ok((count, year.parent_id == Some(archive.id)))
        })
        .await
        .unwrap();
    assert_eq!(inbox_role, 1);
    assert!(archive_parent);

    // Mail goes only where it exists on this device alone.
    core.db
        .write(move |conn| {
            conn.execute(
                "UPDATE accounts SET mail_protocol = 'imap' WHERE id = ?1",
                [account_id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let (progress, _) = watch::channel(MailImportProgress::default());
    assert!(
        core.import_mail(
            MailImportRequest {
                source,
                folders: vec![0],
                destination: Destination::Account(account_id),
                parent_folder: None,
                loose_folder_name: String::new(),
                own_addresses: Vec::new(),
            },
            progress,
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn mail_can_go_into_a_folder_of_its_own() {
    let (_directory, core) = core().await;
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path().join("Mail");
    // A message saved twice is imported once.
    write(
        &root.join("Inbox"),
        mbox(&[message("a", "Welcome", ""), message("a", "Welcome", "")]),
    );
    write(&root.join("Trash"), mbox(&[message("b", "Gone", "")]));
    write(
        &root.join("Inbox.sbd/Bills"),
        mbox(&[message("c", "Due", "")]),
    );
    let loose = folder.path().join("Loose.eml");
    write(&loose, message("d", "Loose", ""));
    let mut source = scan_folder(&root).unwrap();
    source
        .folders
        .extend(crate::mail_import::scan_files(&[loose]).unwrap().folders);
    let (progress, receiver) = watch::channel(MailImportProgress::default());
    let report = core
        .import_mail(
            MailImportRequest {
                folders: (0..source.folders.len()).collect(),
                source,
                destination: Destination::Local,
                parent_folder: Some("ana/old@example.org".into()),
                loose_folder_name: "Saved".into(),
                own_addresses: Vec::new(),
            },
            progress,
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();

    assert_eq!(report.imported, 4);
    assert_eq!(report.already_present, 1);
    // Progress names the folder it got to: the messages chosen one by one.
    assert_eq!(receiver.borrow().folder, Some(3));
    assert_eq!(receiver.borrow().imported, 4);
    assert_eq!(report.folders.len(), 4);
    // Its Trash is no longer anyone's, so it stays an ordinary folder.
    assert_eq!(
        messages(&core, report.account_id)
            .await
            .into_iter()
            .map(|(path, ..)| path)
            .collect::<Vec<_>>(),
        [
            "ana_old@example.org/Inbox",
            "ana_old@example.org/Inbox/Bills",
            "ana_old@example.org/Saved",
            "ana_old@example.org/Trash",
        ]
    );
    let folders = core
        .db
        .read(move |conn| repo::folders::list(conn, Some(report.account_id)))
        .await
        .unwrap();
    let parent = folders
        .iter()
        .find(|folder| folder.path == "ana_old@example.org")
        .unwrap();
    let path_of = |id: i64| {
        folders
            .iter()
            .find(|folder| folder.id == id)
            .map(|folder| folder.path.as_str())
            .unwrap()
    };
    assert_eq!(
        report
            .folders
            .iter()
            .map(|id| path_of(*id))
            .collect::<Vec<_>>(),
        [
            "ana_old@example.org/Inbox",
            "ana_old@example.org/Inbox/Bills",
            "ana_old@example.org/Trash",
            "ana_old@example.org/Saved",
        ]
    );
    assert!(folders.iter().all(|folder| {
        folder.path == "Trash"
            || folder.path == parent.path
            || folder.parent_id.is_some() && folder.role.is_none()
    }));
}

/// Apps run imports on their runtime's worker threads.
#[allow(dead_code)]
fn imports_can_run_on_any_thread(core: &crate::Core, request: MailImportRequest) {
    fn send<T: Send>(_: T) {}
    let (progress, _) = watch::channel(MailImportProgress::default());
    send(core.import_mail(request, progress, Arc::new(AtomicBool::new(false))));
}

#[tokio::test]
async fn a_stopped_import_keeps_nothing_it_did_not_finish() {
    let (_directory, core) = core().await;
    let folder = tempfile::tempdir().unwrap();
    write(
        &folder.path().join("Box.mbox"),
        mbox(&[message("a", "One", ""), message("b", "Two", "")]),
    );
    let source = scan_folder(folder.path()).unwrap();
    let (progress, receiver) = watch::channel(MailImportProgress::default());
    let report = core
        .import_mail(
            MailImportRequest {
                source,
                folders: vec![0],
                destination: Destination::Local,
                parent_folder: None,
                loose_folder_name: String::new(),
                own_addresses: Vec::new(),
            },
            progress,
            Arc::new(AtomicBool::new(true)),
        )
        .await
        .unwrap();
    assert!(report.stopped);
    assert_eq!(report.imported, 0);
    assert_eq!(receiver.borrow().imported, 0);
    assert!(receiver.borrow().total_bytes > 0);
}

#[tokio::test]
async fn local_mail_folders_trash_export_and_removal_work_without_a_server() {
    let (_directory, core) = core().await;
    let folder = tempfile::tempdir().unwrap();
    write(
        &folder.path().join("Box.mbox"),
        mbox(&[message("a", "One", "")]),
    );
    let source = scan_folder(folder.path()).unwrap();
    let account_id = import(&core, &source, Destination::Local).await.account_id;

    core.create_folder(account_id, None, "Kept".into())
        .await
        .unwrap();
    let thread_id = core
        .db
        .read(|conn| Ok(conn.query_row("SELECT thread_id FROM messages", [], |row| row.get(0))?))
        .await
        .unwrap();
    core.perform_action(PerformActionArgs {
        kind: ActionKind::Trash,
        thread_ids: vec![thread_id],
        params: None,
    })
    .await
    .unwrap();
    let in_trash = |core: crate::Core| async move {
        core.db
            .read(move |conn| {
                Ok(conn.query_row(
                    "SELECT COUNT(*) FROM messages m JOIN folders f ON f.id = m.folder_id
                     WHERE m.account_id = ?1 AND f.role = 'trash'",
                    [account_id],
                    |row| row.get::<_, i64>(0),
                )?)
            })
            .await
            .unwrap()
    };
    assert_eq!(in_trash(core.clone()).await, 1);

    let export = folder.path().join("export");
    let exported = core
        .export_account_mail(account_id, export.clone())
        .await
        .unwrap();
    assert_eq!(exported.messages, 1);

    // Emptying the Trash finishes on this device, in local mail's worker.
    core.empty_trash(Some(account_id)).await.unwrap();
    let deadline = now_ms() + 10_000;
    while in_trash(core.clone()).await > 0 && now_ms() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(in_trash(core.clone()).await, 0);

    core.remove_account(account_id).await.unwrap();
    assert!(core.local_mail().await.unwrap().is_none());
    assert!(!core.paths.mail_dir(account_id).exists());
}

#[tokio::test]
async fn an_export_imports_back_with_its_folders_and_states() {
    let (_directory, core) = core().await;
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path().join("Local Folders");
    write(&root.join("Archives"), "");
    write(
        &root.join("Archives.sbd/2019"),
        mbox(&[
            message("a", "Starred", "X-Mozilla-Status: 0005\r\n"),
            message("b", "Unread", "X-Mozilla-Status: 0000\r\n"),
        ]),
    );
    let source = scan_folder(&root).unwrap();
    let account_id = import(&core, &source, Destination::Local).await.account_id;
    let before = messages(&core, account_id).await;

    let export = folder.path().join("export");
    core.export_account_mail(account_id, export.clone())
        .await
        .unwrap();
    assert!(export.join("Archives/2019.mbox").is_file());
    core.remove_account(account_id).await.unwrap();

    let again = import(&core, &scan_folder(&export).unwrap(), Destination::Local).await;
    assert_eq!(again.imported, 2);
    assert_eq!(messages(&core, again.account_id).await, before);
}
