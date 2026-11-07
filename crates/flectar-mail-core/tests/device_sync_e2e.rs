//! Flectar Sync between two app cores through a real sync server, started
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
        AccountSettings, AuthKind, ConnectionSecurity, MailConnectionSettings, MailProtocol,
        PortableAccountConfig, Provider,
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
