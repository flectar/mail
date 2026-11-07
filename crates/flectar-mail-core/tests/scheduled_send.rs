//! Send later on one device: a scheduled draft waits in the Scheduled view,
//! scheduling it again moves it instead of sending it twice, and cancelling
//! leaves it a draft. A reply reminder goes with a send, and brings the
//! conversation back to the inbox. The account's servers are unreachable,
//! so nothing is ever sent.

use flectar_mail_core::Core;
use flectar_mail_core::config::Paths;
use flectar_mail_core::db::{Db, repo};
use flectar_mail_core::models::*;

/// A localhost port with nothing listening.
fn dead_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

async fn seed(db: &Db, port: u16) -> i64 {
    db.write(move |conn| {
        let account_id = repo::accounts::insert(
            conn,
            &repo::accounts::NewAccount {
                email: "later@example.com",
                display_name: Some("Later User"),
                avatar_url: None,
                provider: Provider::Generic,
                auth_kind: AuthKind::Password,
                mail_protocol: MailProtocol::Imap,
                username: "later",
                jmap_url: "",
                jmap_account_id: None,
                incoming_host: "127.0.0.1",
                incoming_port: port,
                smtp_host: "127.0.0.1",
                smtp_port: port,
            },
            &AccountSettings::new(MailConnectionSettings::new(
                ConnectionSecurity::Tls,
                ConnectionSecurity::Tls,
            )),
        )?;
        for (path, role) in [
            ("INBOX", roles::INBOX),
            ("Sent", roles::SENT),
            ("Drafts", roles::DRAFTS),
        ] {
            repo::folders::upsert(conn, account_id, path, None, Some(role))?;
        }
        Ok(account_id)
    })
    .await
    .expect("seed")
}

async fn draft(core: &Core, account_id: i64) -> i64 {
    core.save_draft(SaveDraftArgs {
        draft_id: None,
        account_id,
        from: None,
        to: vec![Address {
            name: Some("Alice Chen".into()),
            email: "alice@example.com".into(),
        }],
        cc: vec![],
        bcc: vec![],
        subject: "Monday plans".into(),
        body_text: "See you on Monday.".into(),
        body_html: None,
        mode: "new".into(),
        in_reply_to_message_id: None,
        in_reply_to_header: None,
        attachments: vec![],
    })
    .await
    .expect("save draft")
}

async fn drafts(core: &Core) -> usize {
    core.list_threads(View::Drafts, None, None, None, None, (None, 10))
        .await
        .unwrap()
        .threads
        .len()
}

async fn scheduled_threads(core: &Core) -> usize {
    core.list_threads(View::Scheduled, None, None, None, None, (None, 10))
        .await
        .unwrap()
        .threads
        .len()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_scheduled_draft_waits_moves_and_cancels() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::for_tests(tmp.path());
    paths.ensure().unwrap();
    let db = Db::open(&paths.db_file()).unwrap();
    let account_id = seed(&db, dead_port()).await;
    let core = Core::start(Paths::for_tests(tmp.path())).await.unwrap();

    let draft_id = draft(&core, account_id).await;
    let in_an_hour = now_ms() + 3_600_000;
    let first = core
        .queue_send(QueueSendArgs {
            draft_id,
            send_at: Some(in_an_hour),
            remind_at: None,
        })
        .await
        .expect("schedule");
    assert_eq!(scheduled_threads(&core).await, 1);
    assert_eq!(
        drafts(&core).await,
        0,
        "a scheduled draft waits in Scheduled, not Drafts"
    );

    // Scheduling it again moves the one send.
    let tomorrow = now_ms() + 86_400_000;
    let second = core
        .queue_send(QueueSendArgs {
            draft_id,
            send_at: Some(tomorrow),
            remind_at: None,
        })
        .await
        .expect("reschedule");
    assert_eq!(second.action_id, first.action_id);
    let scheduled = core.scheduled_sends().await.unwrap();
    assert_eq!(scheduled.len(), 1);
    assert_eq!(scheduled[0].draft_id, draft_id);
    assert_eq!(scheduled[0].send_at, tomorrow);

    // Cancelling leaves it a draft.
    assert!(core.cancel_scheduled_send(draft_id).await.unwrap());
    assert!(core.scheduled_sends().await.unwrap().is_empty());
    assert_eq!(scheduled_threads(&core).await, 0);
    assert_eq!(drafts(&core).await, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_send_in_the_undo_window_is_not_scheduled() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::for_tests(tmp.path());
    paths.ensure().unwrap();
    let db = Db::open(&paths.db_file()).unwrap();
    let account_id = seed(&db, dead_port()).await;
    let core = Core::start(Paths::for_tests(tmp.path())).await.unwrap();

    let draft_id = draft(&core, account_id).await;
    core.queue_send(QueueSendArgs {
        draft_id,
        send_at: None,
        remind_at: None,
    })
    .await
    .expect("send");
    assert!(core.scheduled_sends().await.unwrap().is_empty());
    assert_eq!(scheduled_threads(&core).await, 0);
}

async fn inbox(core: &Core) -> Vec<ThreadSummary> {
    core.list_threads(View::Inbox, None, None, None, None, (None, 10))
        .await
        .unwrap()
        .threads
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reply_reminder_goes_with_the_send_and_brings_the_conversation_back() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::for_tests(tmp.path());
    paths.ensure().unwrap();
    let db = Db::open(&paths.db_file()).unwrap();
    let account_id = seed(&db, dead_port()).await;
    let core = Core::start(Paths::for_tests(tmp.path())).await.unwrap();

    let draft_id = draft(&core, account_id).await;
    let remind_at = now_ms() + 3 * 24 * 3_600_000;
    core.queue_send(QueueSendArgs {
        draft_id,
        send_at: Some(now_ms() + 3_600_000),
        remind_at: Some(remind_at),
    })
    .await
    .expect("schedule");
    // The draft now has the Message-ID it goes out with, and the reminder
    // names it.
    let (message_id, reminders) = db
        .read(move |conn| {
            let message_id: String = conn.query_row(
                "SELECT message_id FROM messages WHERE id = ?1",
                [draft_id],
                |row| row.get(0),
            )?;
            Ok((message_id, repo::reply_reminders::all(conn)?))
        })
        .await
        .unwrap();
    assert!(message_id.ends_with("@example.com"), "{message_id}");
    assert_eq!(reminders.len(), 1);
    assert_eq!(reminders[0].message_id, message_id);
    assert_eq!(reminders[0].remind_at, remind_at);

    // Not sent yet when the time comes: it waits.
    let checked = db
        .write(move |conn| repo::reply_reminders::check(conn, remind_at))
        .await
        .unwrap();
    assert!(checked.due.is_empty());

    // Sent, filed in Sent, and nobody replied: back at the top of the inbox.
    db.write(move |conn| {
        conn.execute(
            "UPDATE messages SET is_draft = 0, is_outgoing = 1, date = ?2,
                    folder_id = (SELECT id FROM folders WHERE role = 'sent')
             WHERE id = ?1",
            rusqlite::params![draft_id, now_ms()],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    assert!(inbox(&core).await.is_empty());
    let checked = db
        .write(move |conn| repo::reply_reminders::check(conn, remind_at))
        .await
        .unwrap();
    assert_eq!(checked.due.len(), 1);
    let listed = inbox(&core).await;
    assert_eq!(listed.len(), 1);
    assert!(listed[0].reply_reminder_due);
    assert_eq!(listed[0].last_message_at, remind_at);

    // Dismissed: it leaves the inbox, where its mail never was.
    let thread_id = listed[0].id;
    core.dismiss_reply_reminder(thread_id).await.unwrap();
    assert!(inbox(&core).await.is_empty());
    assert!(
        db.read(|conn| repo::reply_reminders::all(conn))
            .await
            .unwrap()
            .is_empty()
    );

    // Set again from the conversation: it names the message the user sent.
    let later = remind_at + 3_600_000;
    core.set_reply_reminder(thread_id, None, later)
        .await
        .unwrap();
    let reminders = db
        .read(|conn| repo::reply_reminders::all(conn))
        .await
        .unwrap();
    assert_eq!(reminders.len(), 1);
    assert_eq!(reminders[0].message_id, message_id);
    assert_eq!(reminders[0].remind_at, later);
    assert_eq!(reminders[0].thread_id, Some(thread_id));
    // A conversation with nothing the user sent has nothing to wait for.
    let incoming = db
        .write(move |conn| {
            conn.execute(
                "UPDATE messages SET is_outgoing = 0 WHERE id = ?1",
                [draft_id],
            )?;
            Ok(())
        })
        .await;
    assert!(incoming.is_ok());
    assert!(
        core.set_reply_reminder(thread_id, None, later)
            .await
            .is_err()
    );
}
