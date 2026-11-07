//! Flectar Mail Sync between two app cores through a real sync server, started
//! from the binary that `FLECTAR_MAIL_SYNC_SERVER` names (build it in
//! flectar/mail-sync with `cargo build -p flectar-mail-sync-server`). The
//! test is skipped without it: the server is not part of the app's build.

use flectar_mail_core::{
    Core,
    accounts::credentials::{CredentialStore, Slot},
    config::Paths,
    device_sync::{DeviceSyncState, PairingEvent, SyncBillingStatus},
    error::{CoreError, Result},
    events::CoreEvent,
    models::{
        AccountSettings, Address, AuthKind, ConnectionSecurity, DraftAttachmentIn,
        MailConnectionSettings, MailProtocol, PortableAccountConfig, Provider, QueueSendArgs,
        SaveDraftArgs, now_ms, roles,
    },
};
use std::collections::HashMap;
use std::process::{Child, Command};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Default)]
struct MemoryCredentials(Mutex<HashMap<(i64, Slot), String>>);

impl CredentialStore for MemoryCredentials {
    fn store(&self, account_id: i64, slot: Slot, secret: &str) -> Result<()> {
        self.0
            .lock()
            .unwrap()
            .insert((account_id, slot), secret.into());
        Ok(())
    }

    fn load(&self, account_id: i64, slot: Slot) -> Result<String> {
        self.0
            .lock()
            .unwrap()
            .get(&(account_id, slot))
            .cloned()
            .ok_or_else(|| CoreError::Auth("no stored credential".into()))
    }

    fn delete(&self, account_id: i64, slot: Slot) -> Result<()> {
        self.0.lock().unwrap().remove(&(account_id, slot));
        Ok(())
    }

    fn delete_all(&self, account_id: i64) -> Result<()> {
        self.0
            .lock()
            .unwrap()
            .retain(|(owner, _), _| *owner != account_id);
        Ok(())
    }
}

struct Server {
    child: Child,
    url: String,
    _dir: tempfile::TempDir,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

async fn start_server() -> Option<Server> {
    start_server_with(&[]).await
}

/// A server with more settings, such as billing.
async fn start_server_with(env: &[(&str, &str)]) -> Option<Server> {
    let Ok(binary) = std::env::var("FLECTAR_MAIL_SYNC_SERVER") else {
        eprintln!("skipped: set FLECTAR_MAIL_SYNC_SERVER to a flectar-mail-sync-server binary");
        return None;
    };
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let dir = tempfile::tempdir().unwrap();
    let url = format!("http://127.0.0.1:{port}");
    let child = Command::new(binary)
        .env("FLECTAR_MAIL_SYNC_PUBLIC_URL", &url)
        .env("FLECTAR_MAIL_SYNC_LISTEN", format!("127.0.0.1:{port}"))
        .env("FLECTAR_MAIL_SYNC_DATABASE", dir.path().join("sync.db"))
        .env("RUST_LOG", "warn")
        .envs(env.iter().copied())
        .spawn()
        .unwrap();
    let server = Server {
        child,
        url,
        _dir: dir,
    };
    for _ in 0..100 {
        if reqwest::get(format!("{}/healthz", server.url))
            .await
            .is_ok_and(|response| response.status().is_success())
        {
            return Some(server);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the sync server did not start");
}

async fn start_core(dir: &std::path::Path) -> Core {
    Core::start_mail_ui_with_credentials(
        Paths::for_tests(dir),
        Arc::new(MemoryCredentials::default()),
    )
    .await
    .unwrap()
}

fn imap_account(email: &str) -> PortableAccountConfig {
    PortableAccountConfig {
        email: email.into(),
        display_name: Some("Work".into()),
        provider: Provider::Generic,
        auth_kind: AuthKind::Password,
        mail_protocol: MailProtocol::Imap,
        username: email.into(),
        jmap_url: String::new(),
        incoming_host: "imap.example.com".into(),
        incoming_port: 993,
        smtp_host: "smtp.example.com".into(),
        smtp_port: 465,
        settings: AccountSettings::new(MailConnectionSettings::new(
            ConnectionSecurity::Tls,
            ConnectionSecurity::Tls,
        )),
    }
}

/// A JMAP account whose server is never reached.
fn jmap_account(email: &str) -> PortableAccountConfig {
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    PortableAccountConfig {
        mail_protocol: MailProtocol::Jmap,
        jmap_url: format!("http://127.0.0.1:{port}/.well-known/jmap"),
        ..imap_account(email)
    }
}

/// The next event of `pairing` that `matches`.
async fn pairing_event(
    events: &mut tokio::sync::broadcast::Receiver<CoreEvent>,
    pairing: u64,
    matches: impl Fn(&PairingEvent) -> bool,
) -> PairingEvent {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Ok(CoreEvent::DeviceSyncPairing {
                pairing: from,
                event,
            }) = events.recv().await
                && from == pairing
                && matches(&event)
            {
                return event;
            }
        }
    })
    .await
    .expect("the pairing event arrives")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_devices_share_accounts_and_preferences() {
    let Some(server) = start_server().await else {
        return;
    };
    let laptop_dir = tempfile::tempdir().unwrap();
    let phone_dir = tempfile::tempdir().unwrap();
    let laptop = start_core(laptop_dir.path()).await;
    let phone = start_core(phone_dir.path()).await;

    // The laptop has an account and its preferences, and turns sync on.
    laptop
        .import_account_configs(vec![imap_account("me@example.com")])
        .await
        .unwrap();
    let mut settings = laptop.get_settings().await.unwrap();
    settings.theme = "carbon".into();
    settings.show_avatars = false;
    // A per-device preference, which must not travel.
    settings.close_to_tray = true;
    laptop.set_settings(settings).await.unwrap();
    laptop
        .save_snippet(
            None,
            "Thanks".into(),
            Some("ty".into()),
            None,
            "Thanks for your message!".into(),
        )
        .await
        .unwrap();
    let words = laptop
        .device_sync_turn_on(Some(server.url.clone()))
        .await
        .unwrap();
    assert_eq!(words.split(' ').count(), 24);
    assert!(laptop.device_sync_run().await.unwrap());

    // The phone has its own preferences, and joins with a short code.
    let mut phone_settings = phone.get_settings().await.unwrap();
    phone_settings.theme = "snow".into();
    phone.set_settings(phone_settings).await.unwrap();
    let mut laptop_events = laptop.bus.subscribe();
    let mut phone_events = phone.bus.subscribe();
    let invitation = laptop.device_sync_invite(true).await.unwrap();
    let code = invitation.code.clone().expect("a short code to show");
    // Accepting before any device asks must not let the next one in unseen.
    laptop.device_sync_accept(invitation.pairing);
    let joining = phone
        .device_sync_join_with_code(Some(server.url.clone()), code)
        .await
        .unwrap();
    let PairingEvent::Request {
        words: laptop_words,
    } = pairing_event(&mut laptop_events, invitation.pairing, |event| {
        matches!(event, PairingEvent::Request { .. })
    })
    .await
    else {
        unreachable!()
    };
    let PairingEvent::Waiting { words: phone_words } =
        pairing_event(&mut phone_events, joining, |event| {
            matches!(event, PairingEvent::Waiting { .. })
        })
        .await
    else {
        unreachable!()
    };
    assert_eq!(
        laptop_words, phone_words,
        "both screens show the same words"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        phone.device_sync_status().state,
        DeviceSyncState::Off,
        "the phone waits for a decision made after its request"
    );
    // A decision for another pairing is ignored too.
    laptop.device_sync_accept(invitation.pairing + 1);
    laptop.device_sync_accept(invitation.pairing);
    pairing_event(&mut phone_events, joining, |event| {
        *event == PairingEvent::Joined
    })
    .await;

    // The phone takes the group's account and preferences, not its own.
    assert!(phone.device_sync_run().await.unwrap());
    let accounts = phone.list_accounts().await.unwrap();
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0].email, "me@example.com");
    assert_eq!(accounts[0].sync_state, "needs_reauth", "it asks to sign in");
    let synced = phone.get_settings().await.unwrap();
    assert_eq!(synced.theme, "carbon");
    assert!(!synced.show_avatars);
    assert!(!synced.close_to_tray, "per-device preferences stay");
    assert_eq!(phone.device_sync_status().devices.len(), 2);
    let templates = phone.list_snippets().await.unwrap();
    assert_eq!(templates.len(), 1, "templates follow the user");
    assert_eq!(templates[0].shortcut.as_deref(), Some("ty"));

    // A change on the phone reaches the laptop.
    let mut synced = phone.get_settings().await.unwrap();
    synced.conversation_view = false;
    phone.set_settings(synced).await.unwrap();
    phone.device_sync_run().await.unwrap();
    laptop.device_sync_run().await.unwrap();
    assert!(!laptop.get_settings().await.unwrap().conversation_view);

    // An account removed on the laptop goes from the phone too.
    let laptop_account = laptop.list_accounts().await.unwrap()[0].id;
    laptop.remove_account(laptop_account).await.unwrap();
    laptop.device_sync_run().await.unwrap();
    phone.device_sync_run().await.unwrap();
    assert!(phone.list_accounts().await.unwrap().is_empty());

    // The laptop removes the phone, which stops syncing but keeps its data.
    let phone_id = phone
        .device_sync_status()
        .devices
        .iter()
        .find(|device| device.this_device)
        .unwrap()
        .id
        .clone();
    laptop.device_sync_remove_device(phone_id).await.unwrap();
    phone.device_sync_run().await.unwrap();
    assert_eq!(phone.device_sync_status().state, DeviceSyncState::Removed);
    assert_eq!(phone.get_settings().await.unwrap().theme, "carbon");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn uploads_wait_for_payment_on_a_server_that_charges() {
    // No trial, and no Stripe to reach: refusing uploads never needs it.
    let Some(server) = start_server_with(&[
        ("FLECTAR_MAIL_SYNC_STRIPE_SECRET_KEY", "sk_test_x"),
        ("FLECTAR_MAIL_SYNC_STRIPE_WEBHOOK_SECRET", "whsec_x"),
        ("FLECTAR_MAIL_SYNC_STRIPE_PRICE_MONTHLY", "price_monthly"),
        (
            "FLECTAR_MAIL_SYNC_BILLING_RETURN_URL",
            "https://flectar.test/sync",
        ),
        ("FLECTAR_MAIL_SYNC_TRIAL_DAYS", "0"),
        ("FLECTAR_MAIL_SYNC_STRIPE_API", "http://127.0.0.1:9"),
    ])
    .await
    else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let laptop = start_core(dir.path()).await;

    // The offer shows before sync is on.
    let offer = laptop
        .device_sync_offer(Some(server.url.clone()))
        .await
        .unwrap()
        .expect("the server charges");
    assert_eq!(offer.trial_days, 0);

    laptop
        .device_sync_turn_on(Some(server.url.clone()))
        .await
        .unwrap();
    // This device's records can't upload, but the run itself succeeds.
    assert!(laptop.device_sync_run().await.unwrap());
    let status = laptop.device_sync_status();
    assert_eq!(status.state, DeviceSyncState::PaymentRequired);
    let billing = status.billing.expect("the status says what the group pays");
    assert_eq!(billing.status, SyncBillingStatus::Expired);
    assert!(billing.trial_ends_at.is_some());
}

/// The state and due time of a device's send of its scheduled draft.
async fn send_of(core: &Core) -> Option<(String, i64)> {
    core.db
        .read(|conn| {
            use rusqlite::OptionalExtension;
            Ok(conn
                .query_row(
                    "SELECT state, not_before FROM pending_actions
                     WHERE kind = 'send' ORDER BY id DESC LIMIT 1",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?)
        })
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_scheduled_send_goes_out_from_one_device() {
    let Some(server) = start_server().await else {
        return;
    };
    let laptop_dir = tempfile::tempdir().unwrap();
    let phone_dir = tempfile::tempdir().unwrap();
    let laptop = start_core(laptop_dir.path()).await;
    let phone = start_core(phone_dir.path()).await;
    laptop
        .import_account_configs(vec![jmap_account("me@example.com")])
        .await
        .unwrap();
    let words = laptop
        .device_sync_turn_on(Some(server.url.clone()))
        .await
        .unwrap();
    laptop.device_sync_run().await.unwrap();
    phone
        .device_sync_recover(Some(server.url.clone()), words)
        .await
        .unwrap();
    phone.device_sync_run().await.unwrap();
    let laptop_account = laptop.list_accounts().await.unwrap()[0].id;
    let phone_account = phone.list_accounts().await.unwrap()[0].id;

    // The laptop schedules a draft, which reaches the mail server as Email
    // "E1" (the JMAP server here is a stand-in: nothing is ever sent).
    let draft = laptop
        .save_draft(SaveDraftArgs {
            draft_id: None,
            account_id: laptop_account,
            from: None,
            to: vec![Address {
                name: None,
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
        .unwrap();
    let send_at = now_ms() + 100_000;
    laptop
        .queue_send(QueueSendArgs {
            draft_id: draft,
            send_at: Some(send_at),
            remind_at: None,
        })
        .await
        .unwrap();
    let message_id = laptop
        .db
        .write(move |conn| {
            conn.execute(
                "UPDATE messages SET jmap_id = 'E1' WHERE id = ?1",
                rusqlite::params![draft],
            )?;
            conn.execute(
                "UPDATE pending_actions SET state = 'done' WHERE kind = 'save_draft'",
                [],
            )?;
            Ok(conn.query_row(
                "SELECT message_id FROM messages WHERE id = ?1",
                rusqlite::params![draft],
                |row| row.get::<_, String>(0),
            )?)
        })
        .await
        .unwrap();
    // It claims the send, the server takes the claim, and it lets the send
    // go out at its time.
    laptop.device_sync_run().await.unwrap();
    assert_eq!(
        send_of(&laptop).await,
        Some(("pending".into(), send_at)),
        "the laptop sends it"
    );

    // The phone has the draft from the mail server, and holds a send of it.
    phone
        .db
        .write(move |conn| {
            let drafts = flectar_mail_core::db::repo::folders::upsert(
                conn,
                phone_account,
                "Drafts",
                None,
                Some(roles::DRAFTS),
            )?;
            conn.execute(
                "INSERT INTO threads (id, account_id, subject_norm, last_message_at)
                 VALUES (77, ?1, 'monday plans', 1)",
                rusqlite::params![phone_account],
            )?;
            conn.execute(
                "INSERT INTO messages (thread_id, account_id, folder_id, message_id, subject,
                                       from_addr, date, is_read, is_draft, jmap_id)
                 VALUES (77, ?1, ?2, ?3, 'Monday plans', 'me@example.com', 1, 1, 1, 'E1')",
                rusqlite::params![phone_account, drafts, message_id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    phone.device_sync_run().await.unwrap();
    let scheduled = phone.scheduled_sends().await.unwrap();
    assert_eq!(scheduled.len(), 1, "it shows in the phone's Scheduled view");
    assert_eq!(scheduled[0].send_at, send_at);
    let (state, due) = send_of(&phone).await.unwrap();
    assert_eq!(state, "pending");
    assert_eq!(due, i64::MAX, "the laptop's claim holds the phone's send");

    // "Send now" on the phone: the laptop, which holds the claim, sends it
    // now. Only the device holding a claim gives it up.
    assert!(phone.send_now(scheduled[0].action_id).await.unwrap());
    phone.device_sync_run().await.unwrap();
    laptop.device_sync_run().await.unwrap();
    let (_, due) = send_of(&laptop).await.unwrap();
    assert!(due <= now_ms(), "the laptop sends it now");
    phone.device_sync_run().await.unwrap();
    assert_eq!(
        send_of(&phone).await.map(|(_, due)| due),
        Some(i64::MAX),
        "the phone keeps holding its send"
    );

    // Sent from the laptop: the phone drops its send.
    laptop
        .db
        .write(|conn| {
            conn.execute(
                "UPDATE pending_actions SET state = 'done' WHERE kind = 'send'",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    laptop.device_sync_run().await.unwrap();
    phone.device_sync_run().await.unwrap();
    assert_eq!(
        send_of(&phone).await.map(|(state, _)| state),
        Some("cancelled".into())
    );
    assert!(phone.scheduled_sends().await.unwrap().is_empty());
}

/// An IMAP account keeps its drafts on the device that wrote them, so a send
/// scheduled there carries the message in its record, and another device
/// keeps it as a draft, ready to send it. The account's servers are never
/// reached.
#[tokio::test(flavor = "multi_thread")]
async fn an_imap_message_scheduled_on_one_device_waits_on_the_other() {
    let Some(server) = start_server().await else {
        return;
    };
    let laptop_dir = tempfile::tempdir().unwrap();
    let phone_dir = tempfile::tempdir().unwrap();
    let laptop = start_core(laptop_dir.path()).await;
    let phone = start_core(phone_dir.path()).await;
    laptop
        .import_account_configs(vec![imap_account("me@example.com")])
        .await
        .unwrap();
    let words = laptop
        .device_sync_turn_on(Some(server.url.clone()))
        .await
        .unwrap();
    laptop.device_sync_run().await.unwrap();
    phone
        .device_sync_recover(Some(server.url.clone()), words)
        .await
        .unwrap();
    phone.device_sync_run().await.unwrap();
    let laptop_account = laptop.list_accounts().await.unwrap()[0].id;
    let phone_account = phone.list_accounts().await.unwrap()[0].id;
    for (core, account) in [(&laptop, laptop_account), (&phone, phone_account)] {
        core.db
            .write(move |conn| {
                flectar_mail_core::db::repo::folders::upsert(
                    conn,
                    account,
                    "Drafts",
                    None,
                    Some(roles::DRAFTS),
                )?;
                Ok(())
            })
            .await
            .unwrap();
    }

    let draft = laptop
        .save_draft(SaveDraftArgs {
            draft_id: None,
            account_id: laptop_account,
            from: None,
            to: vec![Address {
                name: None,
                email: "alice@example.com".into(),
            }],
            cc: vec![],
            bcc: vec![Address {
                name: None,
                email: "boss@example.com".into(),
            }],
            subject: "Monday plans".into(),
            body_text: "See you on Monday.".into(),
            body_html: None,
            mode: "new".into(),
            in_reply_to_message_id: None,
            in_reply_to_header: None,
            attachments: vec![],
        })
        .await
        .unwrap();
    let send_at = now_ms() + 3_600_000;
    laptop
        .queue_send(QueueSendArgs {
            draft_id: draft,
            send_at: Some(send_at),
            remind_at: None,
        })
        .await
        .unwrap();
    // The laptop puts the message in the record, then uploads it.
    laptop.device_sync_run().await.unwrap();
    laptop.device_sync_run().await.unwrap();
    phone.device_sync_run().await.unwrap();

    let scheduled = phone.scheduled_sends().await.unwrap();
    assert_eq!(scheduled.len(), 1, "it shows in the phone's Scheduled view");
    assert_eq!(scheduled[0].send_at, send_at);
    let (state, due) = send_of(&phone).await.unwrap();
    assert_eq!(state, "pending");
    assert_eq!(due, i64::MAX, "held until the phone's claim holds");
    let (subject, bcc): (String, String) = phone
        .db
        .read(move |conn| {
            Ok(conn.query_row(
                "SELECT subject, bcc_json FROM messages WHERE account_id = ?1 AND is_draft = 1",
                [phone_account],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(subject, "Monday plans");
    assert!(bcc.contains("boss@example.com"), "{bcc}");
}

/// An IMAP message too large for its record goes into a file in Flectar Mail
/// Sync's storage, sealed. Other devices show it from the preview the record
/// carries, and the one that sends it fetches it first: here the phone, once
/// the laptop that scheduled it is gone.
#[tokio::test(flavor = "multi_thread")]
async fn a_large_imap_message_waits_in_a_file_until_a_device_sends_it() {
    let files = tempfile::tempdir().unwrap();
    let files_dir = files.path().to_str().unwrap().to_owned();
    let Some(server) = start_server_with(&[("FLECTAR_MAIL_SYNC_FILES_DIR", &files_dir)]).await
    else {
        return;
    };
    let laptop_dir = tempfile::tempdir().unwrap();
    let phone_dir = tempfile::tempdir().unwrap();
    let laptop = start_core(laptop_dir.path()).await;
    let phone = start_core(phone_dir.path()).await;
    laptop
        .import_account_configs(vec![imap_account("me@example.com")])
        .await
        .unwrap();
    let words = laptop
        .device_sync_turn_on(Some(server.url.clone()))
        .await
        .unwrap();
    laptop.device_sync_run().await.unwrap();
    phone
        .device_sync_recover(Some(server.url.clone()), words)
        .await
        .unwrap();
    phone.device_sync_run().await.unwrap();
    let laptop_account = laptop.list_accounts().await.unwrap()[0].id;
    let phone_account = phone.list_accounts().await.unwrap()[0].id;
    for (core, account) in [(&laptop, laptop_account), (&phone, phone_account)] {
        core.db
            .write(move |conn| {
                flectar_mail_core::db::repo::folders::upsert(
                    conn,
                    account,
                    "Drafts",
                    None,
                    Some(roles::DRAFTS),
                )?;
                Ok(())
            })
            .await
            .unwrap();
    }

    // A draft with a 300 KB attachment.
    let attachment_dir = tempfile::tempdir().unwrap();
    let attachment = attachment_dir.path().join("plans.bin");
    let content: Vec<u8> = (0..300_000u32).map(|index| (index % 251) as u8).collect();
    std::fs::write(&attachment, &content).unwrap();
    let draft = laptop
        .save_draft(SaveDraftArgs {
            draft_id: None,
            account_id: laptop_account,
            from: None,
            to: vec![Address {
                name: None,
                email: "alice@example.com".into(),
            }],
            cc: vec![],
            bcc: vec![],
            subject: "Monday plans".into(),
            body_text: "The plans are attached.".into(),
            body_html: None,
            mode: "new".into(),
            in_reply_to_message_id: None,
            in_reply_to_header: None,
            attachments: vec![DraftAttachmentIn {
                file_path: attachment.to_string_lossy().into_owned(),
                filename: "plans.bin".into(),
            }],
        })
        .await
        .unwrap();
    let send_at = now_ms() + 3_600_000;
    laptop
        .queue_send(QueueSendArgs {
            draft_id: draft,
            send_at: Some(send_at),
            remind_at: None,
        })
        .await
        .unwrap();
    laptop.device_sync_run().await.unwrap();
    laptop.device_sync_run().await.unwrap();

    // The server holds one sealed file.
    let stored: Vec<Vec<u8>> = std::fs::read_dir(files.path())
        .unwrap()
        .map(|entry| std::fs::read(entry.unwrap().path()).unwrap())
        .collect();
    assert_eq!(stored.len(), 1);
    assert!(stored[0].len() > 300_000);
    assert!(
        !stored[0]
            .windows(100)
            .any(|window| window == &content[1000..1100])
    );

    // The phone shows it from its preview, without fetching it.
    phone.device_sync_run().await.unwrap();
    let scheduled = phone.scheduled_sends().await.unwrap();
    assert_eq!(scheduled.len(), 1, "it shows in the phone's Scheduled view");
    let subject: String = phone
        .db
        .read(move |conn| {
            Ok(conn.query_row(
                "SELECT subject FROM messages WHERE account_id = ?1 AND is_draft = 1",
                [phone_account],
                |row| row.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(subject, "Monday plans");
    let carried = || async {
        phone
            .db
            .read(|conn| {
                Ok(
                    conn.query_row("SELECT file, mime FROM synced_sends", [], |row| {
                        Ok((
                            row.get::<_, Option<String>>(0)?,
                            row.get::<_, Option<Vec<u8>>>(1)?,
                        ))
                    })?,
                )
            })
            .await
            .unwrap()
    };
    let (file, mime) = carried().await;
    assert!(file.is_some());
    assert!(mime.is_none(), "fetched only to send it");

    // Moved to later on the laptop: shared again as it is now, and the file
    // stored before goes.
    let names = || -> Vec<std::ffi::OsString> {
        std::fs::read_dir(files.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect()
    };
    let before = names();
    laptop
        .queue_send(QueueSendArgs {
            draft_id: draft,
            send_at: Some(send_at + 3_600_000),
            remind_at: None,
        })
        .await
        .unwrap();
    laptop.device_sync_run().await.unwrap();
    laptop.device_sync_run().await.unwrap();
    let after = names();
    assert_eq!(after.len(), 1, "one file: {after:?}");
    assert_ne!(after, before);
    phone.device_sync_run().await.unwrap();
    let (moved, _) = carried().await;
    assert!(moved.is_some());
    assert_ne!(moved, file, "the phone has the new file");
    let scheduled = phone.scheduled_sends().await.unwrap();
    assert_eq!(scheduled.len(), 1);
    assert_eq!(scheduled[0].send_at, send_at + 3_600_000);

    // The laptop is gone. "Send now" on the phone: it claims the send, and
    // fetches the message before sending it.
    laptop.device_sync_turn_off().await.unwrap();
    assert!(phone.send_now(scheduled[0].action_id).await.unwrap());
    for _ in 0..3 {
        phone.device_sync_run().await.unwrap();
    }
    let (_, mime) = carried().await;
    let mime = mime.expect("fetched to send it");
    assert!(String::from_utf8_lossy(&mime).contains("plans.bin"));
    assert!(mime.len() > 300_000);
    let (state, due) = send_of(&phone).await.unwrap();
    assert!(
        state != "pending" || due != i64::MAX,
        "its send started: {state} {due}"
    );
}
