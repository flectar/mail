//! Flectar Mail Sync (issue #39) keeps this device's setup the same on the user's
//! other devices: accounts with their per-account preferences, the synced
//! preferences, signatures and mail profiles. It goes through an end-to-end
//! encrypted server that cannot read it. Mail, contacts and calendars keep
//! syncing with the mail provider, and credentials stay on each device.
//!
//! The protocol and client live in `flectar/mail-sync`. This module keeps
//! the client's state in the mail database and the device's keys in the
//! platform credential store, turns the app's items into records and back
//! (`collections`), and runs the sync loop.

mod collections;
mod merge;
mod scheduled;
mod store;

use crate::Core;
use crate::accounts::credentials::{self, Slot};
use crate::error::{CoreError, Result};
use crate::events::CoreEvent;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use collections::{FollowUp, Names};
use flectar_mail_sync_client::{
    Billing, BillingStatus, Clock, Entry, Error as SyncError, Event, Http, Invitation, JoinAttempt,
    Joining, Plan, Secrets, SyncClient,
};
use futures::StreamExt;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use store::RecordId;
use tokio::sync::{Notify, oneshot};
use tokio::task::JoinHandle;

/// Flectar's sync server. A device can use another; the address travels to
/// new devices when they pair.
pub const DEFAULT_SERVER_URL: &str = "https://sync.flectar.com";

/// How often a device that syncs looks for local changes. Uploads follow
/// within this time even when no part of the app asks for a sync.
const CHECK_INTERVAL: Duration = Duration::from_secs(30);
/// How often it reads the server even without a live event.
const PULL_INTERVAL: Duration = Duration::from_secs(5 * 60);
/// The longest wait between attempts to reconnect the live event stream.
const MAX_EVENTS_BACKOFF: Duration = Duration::from_secs(5 * 60);
/// How long what the group pays is shown before the server is asked again.
/// Uploads the server refuses, and leaving that state, ask at once.
const BILLING_REFRESH: Duration = Duration::from_secs(6 * 60 * 60);

/// Where sync stands on this device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceSyncState {
    Off,
    Idle,
    Syncing,
    /// The last attempt failed; the loop tries again.
    Error(String),
    /// Another device removed this one from the group.
    Removed,
    /// The group was deleted from another device.
    Deleted,
    /// The free trial or subscription ended on a server that charges. This
    /// device still downloads; its changes wait for a payment.
    PaymentRequired,
}

/// A device of the group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncDevice {
    /// Hex device id.
    pub id: String,
    pub name: String,
    pub platform: String,
    pub this_device: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceSyncStatus {
    pub state: DeviceSyncState,
    pub server_url: Option<String>,
    pub last_synced_at: Option<i64>,
    pub devices: Vec<SyncDevice>,
    /// What the group pays. `None` on a server that doesn't charge, or
    /// before the server was asked.
    pub billing: Option<SyncBilling>,
}

impl Default for DeviceSyncStatus {
    fn default() -> Self {
        Self {
            state: DeviceSyncState::Off,
            server_url: None,
            last_synced_at: None,
            devices: Vec::new(),
            billing: None,
        }
    }
}

/// What the group pays on a server that charges, as Flectar's does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncBilling {
    pub status: SyncBillingStatus,
    /// End of the free trial, in milliseconds since the Unix epoch.
    pub trial_ends_at: Option<i64>,
    /// End of the last paid period, in milliseconds since the Unix epoch.
    pub paid_until: Option<i64>,
    /// The subscription renews at `paid_until`.
    pub renews: bool,
    pub plans: Vec<SyncPlan>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncBillingStatus {
    Trial,
    Active,
    /// A renewal payment failed and is being retried.
    PastDue,
    /// The trial or paid period ended: uploads wait for a payment.
    Expired,
    /// A status this version doesn't know.
    Unknown,
}

/// A subscription plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncPlan {
    /// What a checkout names it by, such as "monthly" or "yearly".
    pub id: String,
    /// Price per period in the currency's smallest unit, such as cents.
    pub amount: u64,
    /// ISO 4217 code, lowercase.
    pub currency: String,
    /// "month" or "year".
    pub interval: String,
    /// `amount` includes tax; otherwise tax is added where it applies.
    pub tax_included: bool,
}

/// What a server that charges offers before sync is turned on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncOffer {
    pub trial_days: u32,
    pub plans: Vec<SyncPlan>,
}

fn sync_plan(plan: Plan) -> SyncPlan {
    SyncPlan {
        id: plan.id,
        amount: plan.amount,
        currency: plan.currency,
        interval: plan.interval,
        tax_included: plan.tax_included,
    }
}

fn sync_billing(billing: Billing) -> Option<SyncBilling> {
    if !billing.enabled {
        return None;
    }
    Some(SyncBilling {
        status: match billing.status {
            BillingStatus::Trial => SyncBillingStatus::Trial,
            BillingStatus::Active => SyncBillingStatus::Active,
            BillingStatus::PastDue => SyncBillingStatus::PastDue,
            BillingStatus::Expired => SyncBillingStatus::Expired,
            _ => SyncBillingStatus::Unknown,
        },
        trial_ends_at: billing.trial_ends_at,
        paid_until: billing.paid_until,
        renews: billing.renews,
        plans: billing.plans.into_iter().map(sync_plan).collect(),
    })
}

/// What a device that invites another, or asks to join, shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingCode {
    /// The pairing it starts; its events carry the same number.
    pub pairing: u64,
    /// A QR code for the other device to scan.
    pub qr: Option<String>,
    /// Eight digits for the other device to type instead.
    pub code: Option<String>,
}

/// Progress of adding a device, on either side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairingEvent {
    /// A new device asks to join. With a short code, accept only if its
    /// screen shows the same words.
    Request {
        words: Vec<String>,
    },
    /// This device asked to join; the other device shows these words.
    Waiting {
        words: Vec<String>,
    },
    /// This device joined the group.
    Joined,
    /// The new device was added.
    Added,
    Declined,
    Expired,
    Failed(String),
}

/// What the user decided about a join request.
enum Decision {
    Accept,
    Decline,
}

/// The pairing in progress on this device.
struct Pairing {
    id: u64,
    task: JoinHandle<()>,
    /// Set once a join request waits for the user, so a decision can only
    /// ever answer a request the user was shown.
    decision: Arc<Mutex<Option<oneshot::Sender<Decision>>>>,
}

/// The sync service a [`Core`] holds.
pub(crate) struct Service {
    http: reqwest::Client,
    client: Mutex<Option<Arc<SyncClient>>>,
    wake: Notify,
    run_lock: tokio::sync::Mutex<()>,
    names: tokio::sync::Mutex<Names>,
    status: Mutex<DeviceSyncStatus>,
    pairing: Mutex<Option<Pairing>>,
    next_pairing: AtomicU64,
    events_task: Mutex<Option<JoinHandle<()>>>,
    device: Mutex<DeviceDescription>,
    /// When the server was last asked what the group pays.
    billing_checked: Mutex<Option<tokio::time::Instant>>,
}

impl std::fmt::Debug for Service {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Service").finish_non_exhaustive()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// How this device appears in the group's device list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceDescription {
    pub name: String,
    /// "linux", "windows", "macos", "android", "ios", "harmonyos", ...
    pub platform: String,
}

impl Default for DeviceDescription {
    /// The host name and the operating system Rust reports. The app's
    /// platform layer names the device better through
    /// [`Core::describe_device`].
    fn default() -> Self {
        let name = hostname::get()
            .ok()
            .and_then(|name| name.into_string().ok())
            .filter(|name| !name.is_empty() && name != "localhost")
            .unwrap_or_default();
        Self {
            name,
            platform: std::env::consts::OS.to_owned(),
        }
    }
}

/// The client's errors as the app's. A code or recovery code that matches
/// nothing is `NotFound`, so the app can say so; only failing to reach the
/// server is a network error.
fn sync_error(error: SyncError) -> CoreError {
    let message = error.to_string();
    match error {
        SyncError::PairingExpired | SyncError::UnknownRecoveryCode | SyncError::NotAPairingCode => {
            CoreError::NotFound(message)
        }
        SyncError::Network(_) => CoreError::Network(message),
        _ => CoreError::Other(message),
    }
}

impl Service {
    pub(crate) fn new() -> Self {
        let http = reqwest::Client::builder()
            .user_agent(concat!("flectar-mail/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(15))
            // No overall timeout: event streams stay open and pairing reads
            // wait up to 25 s. Servers send a keep-alive every 15 s.
            .read_timeout(Duration::from_secs(60))
            .build()
            .unwrap_or_default();
        Self {
            http,
            client: Mutex::new(None),
            wake: Notify::new(),
            run_lock: tokio::sync::Mutex::new(()),
            names: tokio::sync::Mutex::new(Names::default()),
            status: Mutex::new(DeviceSyncStatus::default()),
            pairing: Mutex::new(None),
            next_pairing: AtomicU64::new(1),
            events_task: Mutex::new(None),
            device: Mutex::new(DeviceDescription::default()),
            billing_checked: Mutex::new(None),
        }
    }

    fn client(&self) -> Option<Arc<SyncClient>> {
        lock(&self.client).clone()
    }

    fn require_client(&self) -> Result<Arc<SyncClient>> {
        self.client()
            .ok_or_else(|| CoreError::Other("Flectar Mail Sync is off on this device".into()))
    }

    fn end_pairing(&self) {
        if let Some(pairing) = lock(&self.pairing).take() {
            pairing.task.abort();
        }
    }

    /// End the pairing in progress and number the next one.
    fn next_pairing(&self) -> u64 {
        self.end_pairing();
        self.next_pairing.fetch_add(1, Ordering::Relaxed)
    }

    fn stop_events(&self) {
        if let Some(task) = lock(&self.events_task).take() {
            task.abort();
        }
    }
}

impl Core {
    fn sync_service(&self) -> &Service {
        &self.device_sync
    }

    fn emit_sync_status(&self) {
        let status = lock(&self.sync_service().status).clone();
        self.bus.emit(CoreEvent::DeviceSync(status));
    }

    fn set_sync_state(&self, state: DeviceSyncState) {
        lock(&self.sync_service().status).state = state;
        self.emit_sync_status();
    }

    fn emit_pairing(&self, pairing: u64, event: PairingEvent) {
        self.bus
            .emit(CoreEvent::DeviceSyncPairing { pairing, event });
    }

    /// Name this device in the group's device list. The platform layer
    /// calls it at startup.
    pub fn describe_device(&self, description: DeviceDescription) {
        *lock(&self.sync_service().device) = description;
    }

    /// Where sync stands, as last reported.
    pub fn device_sync_status(&self) -> DeviceSyncStatus {
        lock(&self.sync_service().status).clone()
    }

    async fn store_sync_secrets(&self, secrets: &Secrets) -> Result<()> {
        credentials::store_async(
            self.credentials.clone(),
            0,
            Slot::DeviceSync,
            STANDARD.encode(secrets.to_bytes().as_slice()),
        )
        .await
    }

    /// Start using `client`: keep its state, listen for live events, sync.
    async fn adopt_client(&self, client: SyncClient, adopting: bool) -> Result<()> {
        let secrets = client.secrets().await;
        self.store_sync_secrets(&secrets).await?;
        let server_url = client.identity().server_url().to_owned();
        let chain = client.chain().await;
        self.db
            .write(move |conn| {
                let tx = conn.transaction()?;
                store::create_row(&tx, &server_url, &chain, adopting, now_ms())?;
                tx.commit()?;
                Ok(())
            })
            .await?;
        let client = Arc::new(client);
        *lock(&self.sync_service().client) = Some(Arc::clone(&client));
        *self.sync_service().names.lock().await = Names::default();
        {
            let mut status = lock(&self.sync_service().status);
            status.server_url = Some(client.identity().server_url().to_owned());
            status.state = DeviceSyncState::Idle;
        }
        self.start_sync_events(client);
        self.refresh_sync_devices().await;
        self.sync_service().wake.notify_one();
        Ok(())
    }

    /// Stop syncing on this device. The app's data stays.
    async fn drop_sync_state(&self, state: DeviceSyncState) -> Result<()> {
        let service = self.sync_service();
        service.end_pairing();
        service.stop_events();
        *lock(&service.client) = None;
        *lock(&service.billing_checked) = None;
        let threads = self
            .db
            .write(|conn| {
                let tx = conn.transaction()?;
                let threads = scheduled::detach(&tx)?;
                store::clear(&tx)?;
                tx.commit()?;
                Ok(threads)
            })
            .await?;
        if !threads.is_empty() {
            self.bus.emit(CoreEvent::MailUpdated {
                thread_ids: threads,
            });
        }
        credentials::delete_async(self.credentials.clone(), 0, Slot::DeviceSync).await?;
        *lock(&service.status) = DeviceSyncStatus {
            state,
            ..DeviceSyncStatus::default()
        };
        self.emit_sync_status();
        Ok(())
    }

    /// Load this device's sync state at startup and run the loop.
    pub(crate) async fn device_sync_loop(&self) {
        if let Err(error) = self.resume_device_sync().await {
            tracing::warn!(%error, "could not resume Flectar Mail Sync");
            self.set_sync_state(DeviceSyncState::Error(error.to_string()));
        }
        let mut last_pull = tokio::time::Instant::now() - PULL_INTERVAL;
        loop {
            let woken = tokio::select! {
                () = self.sync_service().wake.notified() => true,
                () = tokio::time::sleep(CHECK_INTERVAL) => false,
            };
            if self.sync_service().client().is_none() {
                continue;
            }
            let pull = woken || last_pull.elapsed() >= PULL_INTERVAL;
            match self.run_device_sync(pull).await {
                Ok(true) => last_pull = tokio::time::Instant::now(),
                Ok(false) => {}
                Err(error) => {
                    tracing::info!(%error, "Flectar Mail Sync run failed");
                }
            }
            self.drive_scheduled_sends().await;
        }
    }

    /// Move the sends shared with the user's other devices along: claim the
    /// ones coming due, send the ones this device holds the claim for, and
    /// report how they went. A claim or result is uploaded at once, and the
    /// server's answer acted on.
    async fn drive_scheduled_sends(&self) {
        let Some(client) = self.sync_service().client() else {
            return;
        };
        let me = hex(&client.identity().device_id());
        self.share_scheduled_messages(&me).await;
        for _ in 0..3 {
            let reach = match lock(&self.sync_service().status).state {
                DeviceSyncState::PaymentRequired => scheduled::Reach::PaymentRequired,
                DeviceSyncState::Error(_) => scheduled::Reach::Unreachable,
                _ => scheduled::Reach::Usable,
            };
            let driven = {
                let _run = self.sync_service().run_lock.lock().await;
                let me = me.clone();
                self.db
                    .write(move |conn| {
                        if !crate::db::repo::synced_sends::any(conn)? {
                            return Ok(scheduled::Driven::default());
                        }
                        let tx = conn.transaction()?;
                        let driven = scheduled::drive(&tx, &me, now_ms(), reach)?;
                        tx.commit()?;
                        Ok(driven)
                    })
                    .await
            };
            let driven = match driven {
                Ok(driven) => driven,
                Err(error) => {
                    tracing::warn!(%error, "could not move scheduled sends along");
                    return;
                }
            };
            for account_id in &driven.nudge {
                self.nudge(Some(*account_id), || {
                    crate::sync::engine::SyncCmd::RunActions
                })
                .await;
            }
            if !driven.threads.is_empty() {
                self.bus.emit(CoreEvent::MailUpdated {
                    thread_ids: driven.threads,
                });
            }
            if !driven.upload || reach != scheduled::Reach::Usable {
                return;
            }
            // Another run may have uploaded it already; either way, act on
            // what the server holds now.
            if self.run_device_sync(false).await.is_err() {
                return;
            }
        }
    }

    /// Share the IMAP and POP3 messages this device scheduled. Their drafts
    /// stay on this device, so their records carry them for another device
    /// to send; a message too large for a record goes out from here alone.
    async fn share_scheduled_messages(&self, me: &str) {
        let me = me.to_owned();
        let waiting = self
            .db
            .read(move |conn| {
                let mut statement = conn.prepare(
                    "SELECT ss.sync_id, ss.account_id, MAX(m.id) FROM synced_sends ss
                     JOIN accounts a ON a.id = ss.account_id
                     JOIN messages m ON m.account_id = ss.account_id
                       AND m.message_id = ss.message_id AND m.is_draft = 1
                     WHERE ss.origin = ?1 AND ss.state = 'scheduled' AND ss.draft_ref IS NULL
                       AND a.mail_protocol IN ('imap', 'pop3')
                     GROUP BY ss.sync_id",
                )?;
                let rows = statement
                    .query_map([&me], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, i64>(2)?,
                        ))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .await;
        let Ok(waiting) = waiting else {
            return;
        };
        let mut shared = false;
        for (sync_id, account_id, draft_id) in waiting {
            // Signed and encrypted here, with this device's keys, if the
            // account's mail security asks for it.
            let built =
                match crate::queue::build_outgoing(&self.db, &self.paths, account_id, draft_id)
                    .await
                {
                    Ok(outgoing) => {
                        crate::mail_security::protect_draft(
                            &self.db,
                            account_id,
                            draft_id,
                            outgoing.raw,
                            outgoing.security_recipients,
                        )
                        .await
                    }
                    Err(error) => Err(error),
                };
            let (mime, version) = match built {
                Ok(raw) if raw.len() <= scheduled::MAX_SHARED_MESSAGE_BYTES => {
                    use sha2::Digest as _;
                    let version = hex(&sha2::Sha256::digest(&raw)[..8]);
                    (Some(raw), version)
                }
                Ok(_) => (None, scheduled::LOCAL_ONLY.to_owned()),
                Err(error) => {
                    tracing::info!(%error, "a scheduled message goes out from this device alone");
                    (None, scheduled::LOCAL_ONLY.to_owned())
                }
            };
            shared |= self
                .db
                .write(move |conn| {
                    crate::db::repo::synced_sends::set_mime(
                        conn,
                        &sync_id,
                        mime.as_deref(),
                        Some(&version),
                        now_ms(),
                    )
                })
                .await
                .is_ok();
        }
        if shared {
            self.device_sync_now();
        }
    }

    async fn resume_device_sync(&self) -> Result<()> {
        let Some(row) = self.db.read(|conn| store::load_row(conn)).await? else {
            return Ok(());
        };
        let secrets = match credentials::load_async(self.credentials.clone(), 0, Slot::DeviceSync)
            .await
        {
            Ok(encoded) => STANDARD
                .decode(encoded.trim())
                .map_err(|_| CoreError::Other("stored Flectar Mail Sync keys are damaged".into()))
                .and_then(|bytes| Secrets::from_bytes(&bytes).map_err(sync_error))?,
            Err(error) => {
                // The keys are gone, so this device cannot sign in to its
                // group again: it has to rejoin.
                tracing::warn!(%error, "Flectar Mail Sync keys are missing; turning sync off");
                self.drop_sync_state(DeviceSyncState::Off).await?;
                return Ok(());
            }
        };
        let device = secrets.identity.device_id();
        let floor = flectar_mail_sync_client::Stamp {
            ms: row.clock.0,
            counter: row.clock.1,
            device,
        };
        let client = SyncClient::new(
            self.sync_service().http.clone(),
            secrets,
            row.chain.clone(),
            Some(floor),
        )
        .map_err(sync_error)?;
        let client = Arc::new(client);
        *lock(&self.sync_service().client) = Some(Arc::clone(&client));
        {
            let mut status = lock(&self.sync_service().status);
            status.server_url = Some(row.server_url.clone());
            status.last_synced_at = row.last_synced_at;
            status.state = match row.last_error {
                Some(error) => DeviceSyncState::Error(error),
                None => DeviceSyncState::Idle,
            };
        }
        self.start_sync_events(client);
        self.refresh_sync_devices().await;
        self.sync_service().wake.notify_one();
        Ok(())
    }

    /// Listen to the group's live events and sync when another device
    /// changes something.
    fn start_sync_events(&self, client: Arc<SyncClient>) {
        let core = self.clone();
        let task = tokio::spawn(async move {
            let mut backoff = Duration::from_secs(5);
            loop {
                match client.events().await {
                    Ok(stream) => {
                        backoff = Duration::from_secs(5);
                        let mut stream = Box::pin(stream);
                        while let Some(event) = stream.next().await {
                            match event {
                                Ok(Event::Seq(_) | Event::Chain(_)) => {
                                    core.sync_service().wake.notify_one();
                                }
                                // A payment, a renewal or a cancellation:
                                // show it, and upload what waited for it.
                                Ok(Event::Billing) => {
                                    core.check_billing().await;
                                    core.sync_service().wake.notify_one();
                                }
                                Ok(_) => {}
                                Err(_) => break,
                            }
                        }
                    }
                    Err(error) => {
                        tracing::debug!(%error, "Flectar Mail Sync events unavailable");
                    }
                }
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(MAX_EVENTS_BACKOFF);
            }
        });
        if let Some(previous) = lock(&self.sync_service().events_task).replace(task) {
            previous.abort();
        }
    }

    /// One run: record local changes, then, if there are any or `pull`,
    /// sync with the server and take on what other devices changed. Returns
    /// whether it reached the server.
    async fn run_device_sync(&self, pull: bool) -> Result<bool> {
        let service = self.sync_service();
        let _run = service.run_lock.lock().await;
        let Some(client) = service.client() else {
            return Ok(false);
        };

        let names = {
            let settings = self.get_settings().await?;
            let mut names = service.names.lock().await;
            for name in collections::names_needed(&settings) {
                if !names.contains(&name) {
                    let id = client.named_record_id(&name).await;
                    match id {
                        Ok(id) => names.insert(name, id),
                        // Offline before the first key fetch: try later.
                        Err(error) => return self.sync_failed(error).await,
                    }
                }
            }
            names.clone()
        };
        let device_id = client.identity().device_id();
        let description = lock(&service.device).clone();
        let device_body = serde_json::json!({
            "name": description.name,
            "platform": description.platform,
            "version": env!("CARGO_PKG_VERSION"),
        });
        let clock: Arc<Clock> = Arc::clone(client.clock());

        let (mut run, local_before, queued, adopting) = {
            let names = names.clone();
            let device_body = device_body.clone();
            let clock = Arc::clone(&clock);
            self.db
                .write(move |conn| {
                    let tx = conn.transaction()?;
                    let row = store::load_row(&tx)?
                        .ok_or_else(|| CoreError::Other("Flectar Mail Sync is off".into()))?;
                    let mirror = store::load_mirror(&tx)?;
                    let local =
                        collections::snapshot(&tx, &names, &mirror, (device_id, device_body))?;
                    let queued = if row.adopting {
                        0
                    } else {
                        store::track(&tx, &local, &clock)?
                    };
                    let run = store::RunStore::load(&tx, &row, clock)?;
                    tx.commit()?;
                    Ok((run, local, queued, row.adopting))
                })
                .await?
        };
        let was_paused = lock(&service.status).state == DeviceSyncState::PaymentRequired;
        // Uploads the server refused wait for a pull or a new change, not
        // the next check.
        if !pull && queued == 0 && (run.outbox.is_empty() || was_paused) {
            return Ok(false);
        }

        self.set_sync_state(DeviceSyncState::Syncing);
        let report = match client.sync(&mut run).await {
            Ok(report) => report,
            Err(error) => return self.sync_failed(error).await,
        };
        tracing::debug!(?report, "Flectar Mail Sync run");

        let secrets = run.secrets.take();
        let applied = {
            let clock = Arc::clone(&clock);
            self.db
                .write(move |conn| {
                    let tx = conn.transaction()?;
                    let local_now =
                        collections::snapshot(&tx, &names, &run.mirror, (device_id, device_body))?;
                    run.save(&tx, &clock, now_ms())?;
                    let hashes = store::load_local_hashes(&tx)?;
                    let waiting: BTreeMap<RecordId, Entry> = run
                        .mirror
                        .iter()
                        .filter(|(id, _)| {
                            !hashes.contains_key(*id) && !run.outbox.contains_key(*id)
                        })
                        .filter_map(|(id, mirrored)| Some((*id, mirrored.entry.clone()?)))
                        .collect();
                    let applied = collections::apply(
                        &tx,
                        &run.incoming,
                        &waiting,
                        &local_before,
                        &local_now,
                    )?;
                    if adopting {
                        tx.execute("UPDATE device_sync SET adopting = 0 WHERE id = 1", [])?;
                    }
                    tx.commit()?;
                    Ok(applied)
                })
                .await?
        };
        if let Some(secrets) = secrets
            && let Err(error) = self.store_sync_secrets(&secrets).await
        {
            tracing::warn!(%error, "could not store new Flectar Mail Sync keys");
        }

        let mut removed = Vec::new();
        for follow_up in applied.follow_ups {
            let result = match follow_up {
                FollowUp::RemoveAccount(account_id, sync_id) => {
                    // Another record of the same mailbox may have taken this
                    // account over in the same run.
                    let still_ours = self
                        .db
                        .read(move |conn| {
                            use rusqlite::OptionalExtension;
                            Ok(conn
                                .query_row(
                                    "SELECT sync_id FROM accounts WHERE id = ?1",
                                    rusqlite::params![account_id],
                                    |row| row.get::<_, Option<String>>(0),
                                )
                                .optional()?
                                .flatten()
                                .is_some_and(|current| current == sync_id))
                        })
                        .await
                        .unwrap_or(false);
                    if !still_ours {
                        continue;
                    }
                    removed.push(account_id);
                    self.remove_account(account_id).await
                }
                FollowUp::RestartAccount(account_id) => {
                    self.restart_actor(account_id).await;
                    Ok(())
                }
            };
            if let Err(error) = result {
                tracing::warn!(%error, "could not apply an account change from another device");
            }
        }
        if applied.accounts_changed {
            self.bus.emit(CoreEvent::AccountsChanged { removed });
        }
        if applied.settings_changed {
            self.bus.emit(CoreEvent::SettingsChanged);
        }
        if !applied.threads_changed.is_empty() {
            self.bus.emit(CoreEvent::MailUpdated {
                thread_ids: applied.threads_changed,
            });
        }

        {
            let mut status = lock(&service.status);
            status.state = if report.payment_required {
                DeviceSyncState::PaymentRequired
            } else {
                DeviceSyncState::Idle
            };
            status.last_synced_at = Some(now_ms());
        }
        self.refresh_sync_devices().await;
        let billing_due = lock(&service.billing_checked)
            .is_none_or(|checked| checked.elapsed() >= BILLING_REFRESH);
        if billing_due || report.payment_required != was_paused {
            self.check_billing().await;
        }
        if adopting {
            // Upload what only this device had, now that it has the group's.
            service.wake.notify_one();
        }
        Ok(true)
    }

    async fn sync_failed<T: Default>(&self, error: SyncError) -> Result<T> {
        match error {
            SyncError::Removed | SyncError::LoginRefused => {
                self.drop_sync_state(DeviceSyncState::Removed).await?;
            }
            SyncError::GroupDeleted => {
                self.drop_sync_state(DeviceSyncState::Deleted).await?;
            }
            SyncError::PaymentRequired => {
                self.set_sync_state(DeviceSyncState::PaymentRequired);
                self.check_billing().await;
            }
            error => {
                let message = error.to_string();
                let stored = message.clone();
                let _ = self
                    .db
                    .write(move |conn| store::set_error(conn, Some(&stored)))
                    .await;
                self.set_sync_state(DeviceSyncState::Error(message));
            }
        }
        Ok(T::default())
    }

    /// Ask the server what the group pays, for the status. Failing to reach
    /// it keeps what was known.
    async fn check_billing(&self) {
        if let Err(error) = self.device_sync_refresh_billing().await {
            tracing::debug!(%error, "could not read Flectar Mail Sync billing");
        }
    }

    /// The group's devices, named by their own records.
    async fn refresh_sync_devices(&self) {
        let Some(client) = self.sync_service().client() else {
            return;
        };
        let state = client.state().await;
        let mirror = self
            .db
            .read(|conn| store::load_mirror(conn))
            .await
            .unwrap_or_default();
        let entries: BTreeMap<RecordId, Entry> = mirror
            .into_iter()
            .filter_map(|(id, mirrored)| Some((id, mirrored.entry?)))
            .collect();
        let records = collections::device_records(&entries);
        let this = client.identity().device_id();
        let description = lock(&self.sync_service().device).clone();
        let mut devices: Vec<SyncDevice> = state
            .devices
            .keys()
            .map(|id| {
                let record = records.get(id);
                let field = |name: &str| {
                    record
                        .and_then(|record| record.get(name))
                        .and_then(|value| value.as_str())
                        .map(str::to_owned)
                };
                SyncDevice {
                    id: hex(id),
                    name: if *id == this {
                        description.name.clone()
                    } else {
                        field("name").unwrap_or_default()
                    },
                    platform: if *id == this {
                        description.platform.clone()
                    } else {
                        field("platform").unwrap_or_default()
                    },
                    this_device: *id == this,
                }
            })
            .collect();
        devices.sort_by(|a, b| b.this_device.cmp(&a.this_device).then(a.name.cmp(&b.name)));
        lock(&self.sync_service().status).devices = devices;
        self.emit_sync_status();
    }

    /// This device's id in hex, when a send it schedules now can be shared
    /// with the user's other devices: sync is on, and the group may upload.
    pub(crate) fn shared_send_device(&self) -> Option<String> {
        let service = self.sync_service();
        let client = service.client()?;
        if lock(&service.status).state == DeviceSyncState::PaymentRequired {
            return None;
        }
        Some(hex(&client.identity().device_id()))
    }

    /// Sync now, for example when the app comes to the foreground.
    pub fn device_sync_now(&self) {
        self.sync_service().wake.notify_one();
    }

    /// Run one sync to the end, and move the shared scheduled sends along.
    /// Returns whether it reached the server.
    pub async fn device_sync_run(&self) -> Result<bool> {
        let reached = self.run_device_sync(true).await;
        self.drive_scheduled_sends().await;
        reached
    }

    /// What `server_url`, Flectar's by default, charges for syncing, to show
    /// before sync is turned on. `None` when it is free.
    pub async fn device_sync_offer(&self, server_url: Option<String>) -> Result<Option<SyncOffer>> {
        let server_url = server_url.unwrap_or_else(|| DEFAULT_SERVER_URL.to_owned());
        let info = Http::new(self.sync_service().http.clone(), &server_url)
            .info()
            .await
            .map_err(sync_error)?;
        Ok(info.billing.map(|offer| SyncOffer {
            trial_days: offer.trial_days,
            plans: offer.plans.into_iter().map(sync_plan).collect(),
        }))
    }

    /// Ask the server what the group pays, and report it in the status.
    pub async fn device_sync_refresh_billing(&self) -> Result<()> {
        let service = self.sync_service();
        let client = service.require_client()?;
        let billing = client.billing().await.map_err(sync_error)?;
        *lock(&service.billing_checked) = Some(tokio::time::Instant::now());
        lock(&service.status).billing = sync_billing(billing);
        self.emit_sync_status();
        Ok(())
    }

    /// A checkout page for subscribing the group to `plan`, to open in the
    /// browser. `None` when the group already pays: the status then says
    /// so.
    pub async fn device_sync_checkout(&self, plan: String) -> Result<Option<String>> {
        let client = self.sync_service().require_client()?;
        match client.checkout(&plan).await {
            Ok(url) => Ok(Some(url)),
            Err(error) if error.code() == Some(flectar_mail_sync_client::ErrorCode::Conflict) => {
                self.device_sync_refresh_billing().await?;
                Ok(None)
            }
            Err(error) => Err(sync_error(error)),
        }
    }

    /// The page that changes or cancels the subscription and lists its
    /// invoices, to open in the browser.
    pub async fn device_sync_manage_billing(&self) -> Result<String> {
        let client = self.sync_service().require_client()?;
        client.manage_billing().await.map_err(sync_error)
    }

    /// Start a group with this device. Returns the 24 recovery words to show
    /// the user once.
    pub async fn device_sync_turn_on(&self, server_url: Option<String>) -> Result<String> {
        let server_url = server_url.unwrap_or_else(|| DEFAULT_SERVER_URL.to_owned());
        let (client, code) =
            SyncClient::create_group(self.sync_service().http.clone(), &server_url)
                .await
                .map_err(sync_error)?;
        self.adopt_client(client, false).await?;
        Ok(code.to_words())
    }

    /// Join the group of a recovery code, when no other device is at hand.
    pub async fn device_sync_recover(
        &self,
        server_url: Option<String>,
        words: String,
    ) -> Result<()> {
        if flectar_mail_sync_client::RecoveryCode::from_words(&words).is_err() {
            // A mistyped word, caught before asking the server.
            return Err(sync_error(SyncError::UnknownRecoveryCode));
        }
        let server_url = server_url.unwrap_or_else(|| DEFAULT_SERVER_URL.to_owned());
        let client = SyncClient::recover(self.sync_service().http.clone(), &server_url, &words)
            .await
            .map_err(sync_error)?;
        self.adopt_client(client, true).await
    }

    /// Invite a new device. Show the code it returns; its pairing then
    /// emits [`PairingEvent::Request`] when the device asks to join.
    pub async fn device_sync_invite(&self, with_code: bool) -> Result<PairingCode> {
        let client = self.sync_service().require_client()?;
        let invitation = if with_code {
            client.invite_with_code().await
        } else {
            client.invite_with_qr().await
        }
        .map_err(sync_error)?;
        let code = PairingCode {
            pairing: self.sync_service().next_pairing(),
            qr: invitation.qr_code(),
            code: invitation.short_code().map(str::to_owned),
        };
        self.await_join_request(code.pairing, client, invitation);
        Ok(code)
    }

    /// Answer the QR code a new device shows. Returns the pairing, which
    /// emits [`PairingEvent::Request`] when the device's request arrives.
    pub async fn device_sync_answer_code(&self, qr_code: String) -> Result<u64> {
        let client = self.sync_service().require_client()?;
        let invitation = client.invite_scanned(&qr_code).await.map_err(sync_error)?;
        let pairing = self.sync_service().next_pairing();
        self.await_join_request(pairing, client, invitation);
        Ok(pairing)
    }

    fn await_join_request(&self, pairing: u64, client: Arc<SyncClient>, invitation: Invitation) {
        let decision_slot: Arc<Mutex<Option<oneshot::Sender<Decision>>>> = Arc::default();
        let waiting = Arc::clone(&decision_slot);
        let core = self.clone();
        let task = tokio::spawn(async move {
            let emit = |event| core.emit_pairing(pairing, event);
            let attempt: JoinAttempt = match invitation.wait_for_request().await {
                Ok(attempt) => attempt,
                Err(SyncError::PairingExpired) => return emit(PairingEvent::Expired),
                Err(error) => return emit(PairingEvent::Failed(error.to_string())),
            };
            let (decide, decision) = oneshot::channel();
            *lock(&waiting) = Some(decide);
            emit(PairingEvent::Request {
                words: attempt
                    .approval_words()
                    .iter()
                    .map(|w| (*w).to_owned())
                    .collect(),
            });
            match decision.await {
                Ok(Decision::Accept) => match client.accept(&invitation, &attempt).await {
                    Ok(()) => {
                        emit(PairingEvent::Added);
                        core.refresh_sync_devices().await;
                        core.sync_service().wake.notify_one();
                    }
                    Err(error) => emit(PairingEvent::Failed(error.to_string())),
                },
                Ok(Decision::Decline) | Err(_) => {
                    let _ = client.decline(&invitation).await;
                    emit(PairingEvent::Declined);
                }
            }
        });
        *lock(&self.sync_service().pairing) = Some(Pairing {
            id: pairing,
            task,
            decision: decision_slot,
        });
    }

    /// Add the device that asked to join `pairing`. Does nothing unless its
    /// request is waiting.
    pub fn device_sync_accept(&self, pairing: u64) {
        self.decide(pairing, Decision::Accept);
    }

    /// Turn down the device that asked to join `pairing`.
    pub fn device_sync_decline(&self, pairing: u64) {
        self.decide(pairing, Decision::Decline);
    }

    fn decide(&self, pairing: u64, decision: Decision) {
        let sender = lock(&self.sync_service().pairing)
            .as_ref()
            .filter(|current| current.id == pairing)
            .and_then(|current| lock(&current.decision).take());
        if let Some(sender) = sender {
            let _ = sender.send(decision);
        }
    }

    /// Stop adding a device in `pairing`, on either side. A newer pairing
    /// carries on.
    pub fn device_sync_cancel_pairing(&self, pairing: u64) {
        let ended = {
            let mut current = lock(&self.sync_service().pairing);
            if current
                .as_ref()
                .is_some_and(|current| current.id == pairing)
            {
                current.take()
            } else {
                None
            }
        };
        if let Some(ended) = ended {
            ended.task.abort();
        }
    }

    /// Join with the QR code an existing device shows. Returns the pairing,
    /// which emits [`PairingEvent::Waiting`] and then how it ended.
    pub async fn device_sync_join_scanned(&self, qr_code: String) -> Result<u64> {
        let joining =
            Joining::scanned(self.sync_service().http.clone(), &qr_code).map_err(sync_error)?;
        Ok(self.finish_join(joining))
    }

    /// Join with the short code an existing device shows. Returns the
    /// pairing, which emits [`PairingEvent::Waiting`] and then how it ended.
    pub async fn device_sync_join_with_code(
        &self,
        server_url: Option<String>,
        code: String,
    ) -> Result<u64> {
        let server_url = server_url.unwrap_or_else(|| DEFAULT_SERVER_URL.to_owned());
        let joining = Joining::with_code(self.sync_service().http.clone(), &server_url, &code)
            .await
            .map_err(sync_error)?;
        Ok(self.finish_join(joining))
    }

    /// A QR code for an existing device to scan. Its pairing emits
    /// [`PairingEvent::Waiting`] once scanned, and then how it ended.
    pub async fn device_sync_join_show_code(
        &self,
        server_url: Option<String>,
    ) -> Result<PairingCode> {
        let server_url = server_url.unwrap_or_else(|| DEFAULT_SERVER_URL.to_owned());
        let (joining, qr) = Joining::show_qr_code(self.sync_service().http.clone(), &server_url)
            .await
            .map_err(sync_error)?;
        Ok(PairingCode {
            pairing: self.finish_join(joining),
            qr: Some(qr),
            code: None,
        })
    }

    fn finish_join(&self, mut joining: Joining) -> u64 {
        let pairing = self.sync_service().next_pairing();
        let core = self.clone();
        let task = tokio::spawn(async move {
            let emit = |event| core.emit_pairing(pairing, event);
            let words = match joining.request().await {
                Ok(words) => words,
                Err(SyncError::PairingExpired) => return emit(PairingEvent::Expired),
                Err(error) => return emit(PairingEvent::Failed(error.to_string())),
            };
            emit(PairingEvent::Waiting {
                words: words.iter().map(|w| (*w).to_owned()).collect(),
            });
            match joining.finish().await {
                Ok(client) => match core.adopt_client(client, true).await {
                    Ok(()) => emit(PairingEvent::Joined),
                    Err(error) => emit(PairingEvent::Failed(error.to_string())),
                },
                Err(SyncError::Declined) => emit(PairingEvent::Declined),
                Err(SyncError::PairingExpired) => emit(PairingEvent::Expired),
                Err(error) => emit(PairingEvent::Failed(error.to_string())),
            }
        });
        *lock(&self.sync_service().pairing) = Some(Pairing {
            id: pairing,
            task,
            decision: Arc::default(),
        });
        pairing
    }

    /// Remove another device from the group; the others move to a new key
    /// it cannot read.
    pub async fn device_sync_remove_device(&self, device_id: String) -> Result<()> {
        let client = self.sync_service().require_client()?;
        let id: RecordId = (0..16)
            .map(|index| {
                device_id
                    .get(index * 2..index * 2 + 2)
                    .and_then(|b| u8::from_str_radix(b, 16).ok())
            })
            .collect::<Option<Vec<u8>>>()
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| CoreError::Other("invalid device id".into()))?;
        client.remove_device(id).await.map_err(sync_error)?;
        self.refresh_sync_devices().await;
        Ok(())
    }

    /// Replace the recovery code; the old one stops working at once. Returns
    /// the new 24 words to show the user.
    pub async fn device_sync_reset_recovery(&self) -> Result<String> {
        let client = self.sync_service().require_client()?;
        let code = client.reset_recovery().await.map_err(sync_error)?;
        Ok(code.to_words())
    }

    /// Stop syncing on this device and leave the group. The app's data
    /// stays here; the other devices keep syncing.
    pub async fn device_sync_turn_off(&self) -> Result<()> {
        if let Some(client) = self.sync_service().client() {
            let own = client.identity().device_id();
            if let Err(error) = client.remove_device(own).await {
                // The other devices can still remove this one themselves.
                tracing::warn!(%error, "could not leave the Flectar Mail Sync group");
            }
        }
        self.drop_sync_state(DeviceSyncState::Off).await
    }

    /// Delete the group and everything the server holds for it, for every
    /// device. Each device keeps its own data.
    pub async fn device_sync_delete_everywhere(&self) -> Result<()> {
        let client = self.sync_service().require_client()?;
        client.delete_group().await.map_err(sync_error)?;
        self.drop_sync_state(DeviceSyncState::Off).await
    }
}
