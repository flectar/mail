//! Background startup snapshot and coalesced core-event ingestion.

use crate::{
    AppTheme, AppWindow, PAGE_SIZE,
    calendar::{
        LocalCalendarAccount, LocalCalendarEvent, LocalCalendarSource, calendar_accounts,
        calendar_range_millis, calendar_sources, core_calendar_event,
    },
    mail::{self, CoreMailSource},
    startup_metrics::StartupMetrics,
    theme::stored_color,
    ui_dispatch::UiWake,
};
use chrono::Local;
use flectar_mail_core::{
    config::Paths,
    events::CoreEvent,
    models::{Account, AccountConfig, CalendarConnection, Settings, ThreadCursor},
};
use serde::{Deserialize, Serialize};
use slint::ComponentHandle;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tokio::io::AsyncReadExt;

const WARM_START_FORMAT_VERSION: u32 = 2;
const MAX_WARM_START_BYTES: u64 = 2 * 1024 * 1024;
const MAX_WARM_START_ACCOUNTS: usize = 64;
const MAX_WARM_START_MESSAGES: usize = 25;
const MAX_WARM_START_MAILBOXES: usize = 512;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct WarmStartMessage {
    pub(crate) id: i32,
    pub(crate) thread_id: Option<i64>,
    #[serde(default = "missing_folder_id")]
    pub(crate) account_id: i64,
    pub(crate) account: String,
    pub(crate) folder: String,
    pub(crate) sender: String,
    pub(crate) address: String,
    pub(crate) domain: String,
    pub(crate) initials: String,
    pub(crate) subject: String,
    pub(crate) preview: String,
    pub(crate) time: String,
    pub(crate) label: String,
    pub(crate) unread: bool,
    pub(crate) starred: bool,
    pub(crate) has_attachments: bool,
    #[serde(default)]
    pub(crate) has_replied: bool,
    #[serde(default)]
    pub(crate) labels: Vec<i64>,
    pub(crate) sender_verification: String,
}

impl From<&mail::MailMessage> for WarmStartMessage {
    fn from(message: &mail::MailMessage) -> Self {
        Self {
            id: message.id,
            thread_id: message.thread_id,
            account_id: message.account_id,
            account: message.account.clone(),
            folder: message.folder.clone(),
            sender: message.sender.clone(),
            address: message.address.clone(),
            domain: message.domain.clone(),
            initials: message.initials.clone(),
            subject: message.subject.clone(),
            preview: message.preview.clone(),
            time: message.time.clone(),
            label: message.label.clone(),
            unread: message.unread,
            starred: message.starred,
            has_attachments: message.has_attachments,
            has_replied: message.has_replied,
            labels: message.labels.clone(),
            sender_verification: message.sender_verification.clone(),
        }
    }
}

impl From<WarmStartMessage> for mail::MailMessage {
    fn from(message: WarmStartMessage) -> Self {
        Self {
            id: message.id,
            thread_id: message.thread_id,
            account_id: message.account_id,
            account: message.account,
            folder: message.folder,
            sender: message.sender,
            address: message.address,
            domain: message.domain,
            initials: message.initials,
            subject: message.subject,
            preview: message.preview,
            time: message.time,
            to: String::new(),
            label: message.label,
            unread: message.unread,
            starred: message.starred,
            has_attachments: message.has_attachments,
            has_replied: message.has_replied,
            labels: message.labels,
            html: None,
            text: None,
            body_pending: true,
            sender_verification: message.sender_verification,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct WarmStartMailbox {
    pub(crate) account_id: i64,
    #[serde(default = "missing_folder_id")]
    pub(crate) folder_id: i64,
    #[serde(default = "missing_folder_id")]
    pub(crate) parent_folder_id: i64,
    #[serde(default)]
    pub(crate) depth: usize,
    #[serde(default)]
    pub(crate) has_children: bool,
    #[serde(default)]
    pub(crate) is_standard: bool,
    pub(crate) label: String,
    pub(crate) scope: String,
    pub(crate) context: String,
    pub(crate) detail: String,
    pub(crate) avatar: String,
    pub(crate) is_account: bool,
    pub(crate) count: String,
}

fn missing_folder_id() -> i64 {
    -1
}

impl From<&mail::MailboxEntry> for WarmStartMailbox {
    fn from(mailbox: &mail::MailboxEntry) -> Self {
        Self {
            account_id: mailbox.account_id,
            folder_id: mailbox.folder_id,
            parent_folder_id: mailbox.parent_folder_id,
            depth: mailbox.depth,
            has_children: mailbox.has_children,
            is_standard: mailbox.is_standard,
            label: mailbox.label.clone(),
            scope: mailbox.scope.clone(),
            context: mailbox.context.clone(),
            detail: mailbox.detail.clone(),
            avatar: mailbox.avatar.clone(),
            is_account: mailbox.is_account,
            count: mailbox.count.clone(),
        }
    }
}

impl From<WarmStartMailbox> for mail::MailboxEntry {
    fn from(mailbox: WarmStartMailbox) -> Self {
        Self {
            account_id: mailbox.account_id,
            folder_id: mailbox.folder_id,
            parent_folder_id: mailbox.parent_folder_id,
            depth: mailbox.depth,
            has_children: mailbox.has_children,
            is_standard: mailbox.is_standard,
            label: mailbox.label,
            scope: mailbox.scope,
            context: mailbox.context,
            detail: mailbox.detail,
            avatar: mailbox.avatar,
            is_account: mailbox.is_account,
            count: mailbox.count,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct WarmStartSnapshot {
    format_version: u32,
    pub(crate) saved_at_ms: i64,
    pub(crate) scope: String,
    pub(crate) total_count: usize,
    pub(crate) inbox_count: usize,
    pub(crate) next_cursor: Option<ThreadCursor>,
    pub(crate) accounts: Vec<Account>,
    pub(crate) messages: Vec<WarmStartMessage>,
    pub(crate) mailboxes: Vec<WarmStartMailbox>,
    pub(crate) unified_mailboxes: Vec<WarmStartMailbox>,
}

pub(crate) struct WarmStartProjection<'a> {
    pub(crate) scope: &'a str,
    pub(crate) total_count: usize,
    pub(crate) inbox_count: usize,
    pub(crate) next_cursor: Option<ThreadCursor>,
    pub(crate) accounts: &'a [Account],
    pub(crate) messages: &'a [mail::MailMessage],
    pub(crate) mailboxes: &'a [mail::MailboxEntry],
    pub(crate) unified_mailboxes: &'a [mail::MailboxEntry],
}

impl WarmStartSnapshot {
    pub(crate) fn capture(projection: WarmStartProjection<'_>) -> Self {
        Self {
            format_version: WARM_START_FORMAT_VERSION,
            saved_at_ms: chrono::Utc::now().timestamp_millis(),
            scope: projection.scope.to_owned(),
            total_count: projection.total_count,
            inbox_count: projection.inbox_count,
            next_cursor: projection.next_cursor,
            accounts: projection
                .accounts
                .iter()
                .take(MAX_WARM_START_ACCOUNTS)
                .cloned()
                .collect(),
            messages: projection
                .messages
                .iter()
                .take(MAX_WARM_START_MESSAGES)
                .map(WarmStartMessage::from)
                .collect(),
            mailboxes: projection
                .mailboxes
                .iter()
                .take(MAX_WARM_START_MAILBOXES)
                .map(WarmStartMailbox::from)
                .collect(),
            unified_mailboxes: projection
                .unified_mailboxes
                .iter()
                .take(MAX_WARM_START_MAILBOXES)
                .map(WarmStartMailbox::from)
                .collect(),
        }
    }

    fn is_valid(&self) -> bool {
        self.format_version == WARM_START_FORMAT_VERSION
            && !self.accounts.is_empty()
            && self.accounts.len() <= MAX_WARM_START_ACCOUNTS
            && self.messages.len() <= MAX_WARM_START_MESSAGES
            && self.mailboxes.len() <= MAX_WARM_START_MAILBOXES
            && self.unified_mailboxes.len() <= MAX_WARM_START_MAILBOXES
            && !self.scope.trim().is_empty()
    }
}

#[derive(Clone)]
enum WarmStartCacheCommand {
    Save(Arc<WarmStartSnapshot>),
    Clear,
}

#[derive(Clone)]
pub(crate) struct WarmStartCacheWriter {
    tx: tokio::sync::watch::Sender<Option<WarmStartCacheCommand>>,
}

impl WarmStartCacheWriter {
    pub(crate) fn spawn(runtime: &tokio::runtime::Runtime, path: PathBuf) -> Self {
        // A cache file is a projection, not a journal. `watch` retains exactly
        // the newest command while a slow disk write is in flight, placing a
        // hard one-snapshot bound on queued memory.
        let (tx, mut rx) = tokio::sync::watch::channel(None);
        runtime.spawn(async move {
            while rx.changed().await.is_ok() {
                let Some(command) = rx.borrow_and_update().clone() else {
                    continue;
                };
                let result = match command {
                    WarmStartCacheCommand::Save(snapshot) => {
                        write_warm_start_snapshot(&path, &snapshot).await
                    }
                    WarmStartCacheCommand::Clear => clear_warm_start_snapshot(&path).await,
                };
                if let Err(error) = result {
                    tracing::warn!(error = %error, "warm-start mailbox cache update failed");
                }
            }
        });
        Self { tx }
    }

    pub(crate) fn save(&self, snapshot: WarmStartSnapshot) {
        self.tx
            .send_replace(Some(WarmStartCacheCommand::Save(Arc::new(snapshot))));
    }

    pub(crate) fn clear(&self) {
        self.tx.send_replace(Some(WarmStartCacheCommand::Clear));
    }
}

pub(crate) async fn load_warm_start_snapshot(path: &Path) -> Option<WarmStartSnapshot> {
    let file = tokio::fs::File::open(path).await.ok()?;
    let mut bytes = Vec::with_capacity(MAX_WARM_START_BYTES as usize);
    if file
        .take(MAX_WARM_START_BYTES + 1)
        .read_to_end(&mut bytes)
        .await
        .is_err()
        || bytes.len() as u64 > MAX_WARM_START_BYTES
    {
        let _ = clear_warm_start_snapshot(path).await;
        return None;
    }
    let snapshot = serde_json::from_slice::<WarmStartSnapshot>(&bytes).ok();
    match snapshot.filter(WarmStartSnapshot::is_valid) {
        Some(snapshot) => Some(snapshot),
        None => {
            let _ = clear_warm_start_snapshot(path).await;
            None
        }
    }
}

async fn write_warm_start_snapshot(
    path: &Path,
    snapshot: &WarmStartSnapshot,
) -> Result<(), String> {
    let bytes = serde_json::to_vec(snapshot).map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_WARM_START_BYTES {
        return Err("warm-start mailbox cache exceeded its size limit".into());
    }
    let parent = path
        .parent()
        .ok_or_else(|| "warm-start mailbox cache has no parent directory".to_owned())?;
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|error| error.to_string())?;
    let temporary = path.with_extension(format!("json.{}.tmp", std::process::id()));
    tokio::fs::write(&temporary, bytes)
        .await
        .map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600))
            .await
            .map_err(|error| error.to_string())?;
    }
    #[cfg(windows)]
    if tokio::fs::try_exists(path).await.unwrap_or(false) {
        tokio::fs::remove_file(path)
            .await
            .map_err(|error| error.to_string())?;
    }
    tokio::fs::rename(&temporary, path)
        .await
        .map_err(|error| error.to_string())
}

async fn clear_warm_start_snapshot(path: &Path) -> Result<(), String> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

pub(crate) struct StartupSnapshot {
    pub(crate) core: CoreMailSource,
    pub(crate) scope: String,
    pub(crate) page: Option<mail::MailPage>,
    pub(crate) remote_images_enabled: bool,
    pub(crate) accounts: Vec<Account>,
    pub(crate) account_configs: Vec<AccountConfig>,
    pub(crate) settings: Option<Settings>,
}

pub(crate) struct StartupCalendarSnapshot {
    pub(crate) calendar_connections: Vec<CalendarConnection>,
    pub(crate) calendar_events: Vec<LocalCalendarEvent>,
    pub(crate) calendar_accounts: Vec<LocalCalendarAccount>,
    pub(crate) calendar_sources: Vec<LocalCalendarSource>,
}

pub(crate) enum StartupUpdate {
    Warm {
        snapshot: Box<WarmStartSnapshot>,
        applied: tokio::sync::oneshot::Sender<()>,
    },
    Ready(Result<Box<StartupSnapshot>, String>),
    MailMetadata(Result<mail::MailMetadata, String>),
    Calendar(StartupCalendarSnapshot),
}

const MAX_PENDING_THREAD_UPDATES: usize = 2_048;

/// Cross-thread core events are collapsed to the latest meaningful state.
/// A massive backfill can therefore never grow an unbounded UI queue or make
/// one Slint callback drain thousands of historical notifications.
#[derive(Default)]
pub(crate) struct PendingCoreUpdates {
    pub(crate) changed_threads: HashSet<i64>,
    pub(crate) all_mail_changed: bool,
    pub(crate) account_states: HashMap<i64, (String, Option<String>)>,
    pub(crate) calendar_changed: bool,
}

impl PendingCoreUpdates {
    fn record_threads(&mut self, thread_ids: Vec<i64>) {
        if self.all_mail_changed {
            return;
        }
        if thread_ids.is_empty()
            || self.changed_threads.len().saturating_add(thread_ids.len())
                > MAX_PENDING_THREAD_UPDATES
        {
            self.changed_threads.clear();
            self.all_mail_changed = true;
            return;
        }
        self.changed_threads.extend(thread_ids);
    }
}

pub(crate) async fn load_startup_snapshot(
    paths: Paths,
    credentials: flectar_mail_core::accounts::credentials::CredentialStoreHandle,
    oauth_redirects: flectar_mail_core::oauth::redirect::OAuthRedirectBrokerHandle,
    preferred_scope: &str,
    metrics: &StartupMetrics,
) -> Result<StartupSnapshot, String> {
    let core = CoreMailSource::start(paths, credentials, oauth_redirects).await?;
    metrics.emit("core_opened", serde_json::Value::Null);
    let (mail, configs, settings) = tokio::join!(
        // Counts walk every mailbox and can dominate startup on a large local
        // store. The first useful frame only needs one bounded page; sidebar
        // totals arrive as a separate progressive update.
        core.load_startup_page(preferred_scope, PAGE_SIZE as i64),
        core.load_account_configs(),
        core.load_settings(),
    );

    let (accounts, scope, page) =
        mail.unwrap_or_else(|_| (Vec::new(), "Unified Inbox".to_owned(), None));
    let settings = settings.ok();
    metrics.emit(
        "startup_queries_loaded",
        serde_json::json!({
            "accounts": accounts.len(),
            "has_page": page.is_some(),
        }),
    );

    Ok(StartupSnapshot {
        core,
        scope,
        page,
        remote_images_enabled: settings
            .as_ref()
            .is_some_and(|settings| settings.load_remote_images),
        accounts,
        account_configs: configs.unwrap_or_default(),
        settings,
    })
}

pub(crate) async fn load_startup_mail_metadata(
    core: &CoreMailSource,
    scope: &str,
) -> Result<mail::MailMetadata, String> {
    core.load_mail_metadata(scope).await
}

pub(crate) async fn load_startup_calendar_snapshot(
    core: &CoreMailSource,
    accounts: &[Account],
) -> StartupCalendarSnapshot {
    let calendar_range = calendar_range_millis(Local::now().date_naive());
    let (connections, events, calendars) = tokio::join!(
        core.load_calendar_connections(),
        async {
            match calendar_range {
                Ok((start_ms, end_ms)) => core.load_events(start_ms, end_ms).await,
                Err(error) => Err(error),
            }
        },
        core.load_calendars(None),
    );

    StartupCalendarSnapshot {
        calendar_connections: connections.unwrap_or_default(),
        calendar_events: events
            .unwrap_or_default()
            .into_iter()
            .map(core_calendar_event)
            .collect(),
        calendar_accounts: calendar_accounts(accounts),
        calendar_sources: calendar_sources(calendars.unwrap_or_default()),
    }
}

pub(crate) fn spawn_core_event_listener(
    runtime: &tokio::runtime::Runtime,
    core: CoreMailSource,
    pending: Arc<Mutex<PendingCoreUpdates>>,
    wake: UiWake,
) {
    let mut events = core.subscribe_events();
    runtime.spawn(async move {
        use tokio::sync::broadcast::error::RecvError;

        loop {
            let should_wake = match events.recv().await {
                Ok(CoreEvent::MailUpdated { thread_ids }) => {
                    pending
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .record_threads(thread_ids);
                    true
                }
                Ok(CoreEvent::AccountState {
                    account_id,
                    sync_state,
                    sync_error,
                }) => {
                    pending
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .account_states
                        .insert(account_id, (sync_state, sync_error));
                    true
                }
                Ok(CoreEvent::CalendarUpdated { .. })
                | Ok(CoreEvent::CalendarEventsAdded { .. }) => {
                    pending
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .calendar_changed = true;
                    true
                }
                Err(RecvError::Lagged(_)) => {
                    let mut pending = pending
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    pending.changed_threads.clear();
                    pending.all_mail_changed = true;
                    true
                }
                Ok(_) => false,
                Err(RecvError::Closed) => break,
            };
            if should_wake {
                wake.wake();
            }
        }
    });
}

pub(crate) fn apply_settings(app: &AppWindow, settings: &Settings) {
    crate::apply_language(app, &settings.language);
    app.set_custom_oauth_configured(
        !settings.google_client_id.trim().is_empty() || !settings.ms_client_id.trim().is_empty(),
    );
    app.set_google_client_id(settings.google_client_id.clone().into());
    app.set_google_client_secret(settings.google_client_secret.clone().into());
    app.set_ms_client_id(settings.ms_client_id.clone().into());
    app.set_notifications_enabled(settings.notifications_enabled);
    app.set_notification_sound_enabled(settings.sound_enabled);
    app.set_notification_scope(settings.notification_scope.clone().into());
    app.set_sync_interval_minutes(match settings.sync_interval_minutes {
        1 | 5 | 15 => settings.sync_interval_minutes as i32,
        _ => 5,
    });
    app.set_mark_read_on_open(settings.mark_read_on_open);
    app.set_close_to_tray(settings.close_to_tray);
    app.set_monochrome_sidebar_icons(settings.monochrome_sidebar_icons);
    app.set_show_avatars(settings.show_avatars);
    app.set_workspace_layout(
        match settings.workspace_layout.as_str() {
            "minimal" => "minimal",
            _ => "default",
        }
        .into(),
    );
    app.set_theme_mode(
        match settings.theme.as_str() {
            "carbon" | "dark" => "dark",
            "snow" | "light" => "light",
            _ => "system",
        }
        .into(),
    );

    let theme = app.global::<AppTheme>();
    theme.set_preset(
        match settings.theme_preset.as_str() {
            "teal" | "green" | "purple" | "custom" => settings.theme_preset.as_str(),
            _ => "default",
        }
        .into(),
    );
    let custom = &settings.custom_theme;
    theme.set_custom_light_primary(stored_color(&custom.light_primary, "#0969DA"));
    theme.set_custom_light_page_bg(stored_color(&custom.light_page_background, "#F2F2F0"));
    theme.set_custom_light_surface(stored_color(&custom.light_surface, "#FFFFFF"));
    theme.set_custom_light_text(stored_color(&custom.light_text, "#202120"));
    theme.set_custom_light_border(stored_color(&custom.light_border, "#D9D9D6"));
    theme.set_custom_dark_primary(stored_color(&custom.dark_primary, "#0969DA"));
    theme.set_custom_dark_page_bg(stored_color(&custom.dark_page_background, "#111213"));
    theme.set_custom_dark_surface(stored_color(&custom.dark_surface, "#18191A"));
    theme.set_custom_dark_text(stored_color(&custom.dark_text, "#F3F3F2"));
    theme.set_custom_dark_border(stored_color(&custom.dark_border, "#3A3B3C"));
}

#[cfg(test)]
mod warm_start_tests {
    use super::*;
    use flectar_mail_core::models::{AuthKind, MailProtocol, Provider};

    fn account() -> Account {
        Account {
            id: 7,
            email: "person@example.com".into(),
            display_name: Some("Person".into()),
            avatar_url: None,
            provider: Provider::Imap,
            auth_kind: AuthKind::Password,
            mail_protocol: MailProtocol::Imap,
            sync_state: "idle".into(),
            sync_error: None,
        }
    }

    fn message() -> mail::MailMessage {
        mail::MailMessage {
            id: 9,
            thread_id: Some(19),
            account_id: 7,
            account: "Person".into(),
            folder: "Inbox".into(),
            sender: "Sender".into(),
            address: "sender@example.net".into(),
            domain: "example.net".into(),
            initials: "S".into(),
            subject: "Cached subject".into(),
            preview: "Cached preview".into(),
            time: "Today".into(),
            to: "secret-recipient@example.com".into(),
            label: "UNREAD".into(),
            unread: true,
            starred: false,
            has_attachments: false,
            has_replied: true,
            labels: Vec::new(),
            html: Some("<p>body must not enter warm cache</p>".into()),
            text: Some("Plain body must not enter warm cache".into()),
            body_pending: false,
            sender_verification: "domain".into(),
        }
    }

    fn mailbox() -> mail::MailboxEntry {
        mail::MailboxEntry {
            account_id: 7,
            folder_id: 1,
            parent_folder_id: -1,
            depth: 0,
            has_children: false,
            is_standard: true,
            label: "Inbox".into(),
            scope: "Person / Inbox".into(),
            context: "Person".into(),
            detail: "person@example.com".into(),
            avatar: "P".into(),
            is_account: false,
            count: "3".into(),
        }
    }

    #[tokio::test]
    async fn warm_start_round_trip_excludes_bodies_and_recipient_details() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("warm.json");
        let accounts = [account()];
        let messages = [message()];
        let mailboxes = [mailbox()];
        let snapshot = WarmStartSnapshot::capture(WarmStartProjection {
            scope: "Person / Inbox",
            total_count: 3,
            inbox_count: 3,
            next_cursor: Some(ThreadCursor {
                last_message_at: 123,
                thread_id: 7,
            }),
            accounts: &accounts,
            messages: &messages,
            mailboxes: &mailboxes,
            unified_mailboxes: &[],
        });

        write_warm_start_snapshot(&path, &snapshot).await.unwrap();
        let bytes = tokio::fs::read(&path).await.unwrap();
        let json = String::from_utf8(bytes).unwrap();
        assert!(!json.contains("body must not enter"));
        assert!(!json.contains("secret-recipient"));

        let restored = load_warm_start_snapshot(&path).await.unwrap();
        assert_eq!(restored.scope, "Person / Inbox");
        assert_eq!(restored.messages.len(), 1);
        let restored_message = mail::MailMessage::from(restored.messages[0].clone());
        assert_eq!(restored_message.subject, "Cached subject");
        assert!(restored_message.has_replied);
        assert!(restored_message.html.is_none());
        assert!(restored_message.body_pending);
    }

    #[tokio::test]
    async fn invalid_warm_start_is_ignored_and_removed() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("warm.json");
        tokio::fs::write(&path, br#"{"format_version":999}"#)
            .await
            .unwrap();

        assert!(load_warm_start_snapshot(&path).await.is_none());
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn warm_start_projection_is_bounded_to_the_visible_page() {
        let messages = (0..100)
            .map(|id| {
                let mut value = message();
                value.id = id;
                value
            })
            .collect::<Vec<_>>();
        let accounts = [account()];
        let mailboxes = [mailbox()];
        let snapshot = WarmStartSnapshot::capture(WarmStartProjection {
            scope: "Unified Inbox",
            total_count: messages.len(),
            inbox_count: messages.len(),
            next_cursor: None,
            accounts: &accounts,
            messages: &messages,
            mailboxes: &mailboxes,
            unified_mailboxes: &[],
        });

        assert_eq!(snapshot.messages.len(), MAX_WARM_START_MESSAGES);
    }

    #[test]
    fn benchmark_warm_cache_fixture_matches_the_current_format() {
        let snapshot: WarmStartSnapshot = serde_json::from_str(include_str!(
            "../resources/benchmarks/warm-cache/warm-start-mailbox-v2.json"
        ))
        .unwrap();

        assert!(snapshot.is_valid());
        assert_eq!(snapshot.messages.len(), 3);
    }
}
