//! Background startup snapshot and coalesced core-event ingestion.

use crate::{
    AccountPresentationSettings, AccountRow, AppTheme, AppWindow, InboxState, MessageColors,
    PAGE_SIZE,
    account_controller::apply_connected_account_rows,
    avatar_initials,
    calendar::{
        LocalCalendarAccount, LocalCalendarEvent, LocalCalendarSource, calendar_accounts,
        calendar_range_millis, calendar_sources, core_calendar_event,
    },
    mail::{self, CoreMailSource, MailCursor, MailMessage, MailboxEntry},
    mail_scope::Scope,
    mail_view_model::{refresh_list_metadata, refresh_rows_only},
    startup_metrics::StartupMetrics,
    theme::stored_color,
    ui_dispatch::UiWake,
};
use flectar_mail_core::{
    config::Paths,
    events::CoreEvent,
    models::{
        Account, AccountConfig, CalendarConnection, CardDavConnection, Settings, ThreadCursor,
        normalized_workspace_list_pane_width,
    },
};
use serde::{Deserialize, Serialize};
use slint::ComponentHandle;
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    rc::Rc,
    sync::{Arc, Mutex},
};
use tokio::io::AsyncReadExt;

// Version 4 keys account mailboxes by id instead of display name. Version 5
// stores the provider and mail protocol names of the current account model.
// Version 6 keeps message dates as timestamps instead of English text.
const WARM_START_FORMAT_VERSION: u32 = 6;
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
    pub(crate) date_ms: i64,
    pub(crate) label: String,
    pub(crate) unread: bool,
    pub(crate) starred: bool,
    pub(crate) has_attachments: bool,
    #[serde(default = "single_message")]
    pub(crate) message_count: i64,
    #[serde(default)]
    pub(crate) has_replied: bool,
    #[serde(default)]
    pub(crate) labels: Vec<i64>,
    pub(crate) sender_verification: String,
    #[serde(default)]
    pub(crate) account_uses_labels: bool,
    // Verification codes come from message bodies, which stay out of this
    // file, and file chips act on attachment ids that a stored body can
    // replace; the list shows both once the mail store answers.
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
            date_ms: message.date_ms,
            label: message.label.clone(),
            unread: message.unread,
            starred: message.starred,
            has_attachments: message.has_attachments,
            message_count: message.message_count,
            has_replied: message.has_replied,
            labels: message.labels.clone(),
            sender_verification: message.sender_verification.clone(),
            account_uses_labels: message.account_uses_labels,
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
            date_ms: message.date_ms,
            to: String::new(),
            label: message.label,
            unread: message.unread,
            starred: message.starred,
            has_attachments: message.has_attachments,
            message_count: message.message_count.max(1),
            has_replied: message.has_replied,
            is_outgoing: false,
            labels: message.labels,
            html: None,
            text: None,
            attachments: Vec::new(),
            body_pending: true,
            sender_verification: message.sender_verification,
            verification_code: None,
            account_uses_labels: message.account_uses_labels,
            first_attachment: None,
            attachment_total: 0,
        }
    }
}

const fn single_message() -> i64 {
    1
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
    #[serde(default = "selectable_mailbox")]
    pub(crate) is_selectable: bool,
    #[serde(default)]
    pub(crate) can_create_children: bool,
    #[serde(default)]
    pub(crate) can_rename: bool,
    #[serde(default)]
    pub(crate) can_delete: bool,
    pub(crate) label: String,
    /// Absent for account headings, which are not navigation targets.
    #[serde(default)]
    pub(crate) scope: Option<Scope>,
    pub(crate) context: String,
    pub(crate) detail: String,
    pub(crate) avatar: String,
    pub(crate) is_account: bool,
    pub(crate) count: String,
}

fn missing_folder_id() -> i64 {
    -1
}

fn selectable_mailbox() -> bool {
    true
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
            is_selectable: mailbox.is_selectable,
            can_create_children: mailbox.can_create_children,
            can_rename: mailbox.can_rename,
            can_delete: mailbox.can_delete,
            label: mailbox.label.clone(),
            scope: mailbox.scope,
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
            is_selectable: mailbox.is_selectable,
            can_create_children: mailbox.can_create_children,
            can_rename: mailbox.can_rename,
            can_delete: mailbox.can_delete,
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
    pub(crate) scope: Scope,
    pub(crate) inbox_count: usize,
    pub(crate) next_cursor: Option<ThreadCursor>,
    pub(crate) accounts: Vec<Account>,
    /// The active profile's accounts, which the saved rows show.
    #[serde(default)]
    pub(crate) profile_accounts: Option<Vec<i64>>,
    pub(crate) messages: Vec<WarmStartMessage>,
    pub(crate) mailboxes: Vec<WarmStartMailbox>,
    pub(crate) unified_mailboxes: Vec<WarmStartMailbox>,
}

pub(crate) struct WarmStartProjection<'a> {
    pub(crate) scope: Scope,
    pub(crate) inbox_count: usize,
    pub(crate) next_cursor: Option<ThreadCursor>,
    pub(crate) accounts: &'a [Account],
    pub(crate) profile_accounts: Option<&'a [i64]>,
    pub(crate) messages: &'a [mail::MailMessage],
    pub(crate) mailboxes: &'a [mail::MailboxEntry],
    pub(crate) unified_mailboxes: &'a [mail::MailboxEntry],
}

impl WarmStartSnapshot {
    pub(crate) fn capture(projection: WarmStartProjection<'_>) -> Self {
        Self {
            format_version: WARM_START_FORMAT_VERSION,
            saved_at_ms: chrono::Utc::now().timestamp_millis(),
            scope: projection.scope,
            inbox_count: projection.inbox_count,
            next_cursor: projection.next_cursor,
            accounts: projection
                .accounts
                .iter()
                .take(MAX_WARM_START_ACCOUNTS)
                .cloned()
                .collect(),
            profile_accounts: projection.profile_accounts.map(<[i64]>::to_vec),
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
    }
}

pub(crate) fn apply_warm_start_snapshot(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Runtime,
    snapshot: WarmStartSnapshot,
) {
    // The cache contains account summaries, not connection settings. Project
    // these directly: the full settings projection requires AccountConfig and
    // would filter every cached account out before the database has opened.
    let account_rows = snapshot
        .accounts
        .iter()
        .filter_map(|account| {
            let name = account
                .display_name
                .as_ref()
                .filter(|name| !name.trim().is_empty())
                .unwrap_or(&account.email);
            Some(AccountRow {
                id: i32::try_from(account.id).ok()?,
                drag_key: account.id.to_string().into(),
                name: name.as_str().into(),
                email: account.email.as_str().into(),
                provider: account.provider.as_str().into(),
                mail_protocol: account.mail_protocol.as_str().into(),
                status: account.sync_state.as_str().into(),
                sync_error: account.sync_error.as_deref().unwrap_or_default().into(),
                initials: avatar_initials(name).into(),
                ..Default::default()
            })
        })
        .collect();
    {
        let mut state = state.borrow_mut();
        // Core remains unset: cached rows are only a presentation until local
        // storage is ready, and the startup guard keeps actions inert.
        state.using_core = true;
        // The saved rows show the profile's accounts; so do the sidebar and
        // scope until settings load.
        state.profile_accounts = snapshot.profile_accounts;
        let _ = state.set_connected_accounts(snapshot.accounts);
        state.set_scope(snapshot.scope);
        state.inbox_count = snapshot.inbox_count;
        state.next_cursor = snapshot.next_cursor.map(MailCursor::Thread);
        state.messages = snapshot
            .messages
            .into_iter()
            .map(MailMessage::from)
            .collect();
        state.mailboxes = snapshot
            .mailboxes
            .into_iter()
            .map(MailboxEntry::from)
            .collect();
        state.unified_mailboxes = snapshot
            .unified_mailboxes
            .into_iter()
            .map(MailboxEntry::from)
            .collect();
        state.preview_closed = true;
    }
    // The mailbox is a conditional element keyed on Slint's account model.
    // Publishing only the cached rows leaves that entire view uninstantiated.
    apply_connected_account_rows(app, account_rows);
    refresh_rows_only(app, state, runtime);
    refresh_list_metadata(app, state);
    app.set_startup_hydrated(true);
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
    pub(crate) scope: Scope,
    pub(crate) page: Option<mail::MailPage>,
    pub(crate) remote_images_enabled: bool,
    pub(crate) accounts: Vec<Account>,
    pub(crate) account_configs: Vec<AccountConfig>,
    pub(crate) settings: Option<Settings>,
}

pub(crate) struct StartupCalendarSnapshot {
    pub(crate) calendar_connections: Vec<CalendarConnection>,
    pub(crate) carddav_connections: Vec<CardDavConnection>,
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
    Calendar {
        generation: u64,
        month: chrono::NaiveDate,
        snapshot: StartupCalendarSnapshot,
    },
    Connections {
        calendar: Vec<CalendarConnection>,
        contacts: Vec<CardDavConnection>,
    },
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
    pub(crate) action_error: Option<String>,
    pub(crate) calendar_changed: bool,
    pub(crate) contacts_changed: bool,
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
    preferred_scope: Scope,
    metrics: &StartupMetrics,
) -> Result<StartupSnapshot, String> {
    let core = CoreMailSource::start(paths, credentials, oauth_redirects).await?;
    metrics.emit("core_opened", serde_json::Value::Null);
    // The first page shows only the active profile's accounts.
    let settings = core.load_settings().await.ok();
    let _ = core.set_profile_accounts(settings.as_ref().and_then(|settings| {
        AccountPresentationSettings::from_settings(settings).active_profile_accounts()
    }));
    let (mail, configs) = tokio::join!(
        // Counts walk every mailbox and can dominate startup on a large local
        // store. The first useful frame only needs one bounded page; sidebar
        // totals arrive as a separate progressive update.
        core.load_startup_page(preferred_scope, PAGE_SIZE as i64),
        core.load_account_configs(),
    );

    let (accounts, scope, page) =
        mail.unwrap_or_else(|_| (Vec::new(), Scope::UNIFIED_INBOX, None));
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
    scope: Scope,
) -> Result<mail::MailMetadata, String> {
    core.load_mail_metadata(scope).await
}

pub(crate) async fn load_startup_calendar_snapshot(
    core: &CoreMailSource,
    accounts: &[Account],
    visible_month: chrono::NaiveDate,
) -> StartupCalendarSnapshot {
    let calendar_range = calendar_range_millis(visible_month);
    let (connections, carddav_connections, events, calendars) = tokio::join!(
        core.load_calendar_connections(),
        core.load_carddav_connections(),
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
        carddav_connections: carddav_connections.unwrap_or_default(),
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
                Ok(CoreEvent::ActionState {
                    state,
                    error: Some(error),
                    ..
                }) if state == "failed" => {
                    pending
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .action_error = Some(error);
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
                Ok(CoreEvent::ContactsUpdated { .. }) => {
                    pending
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .contacts_changed = true;
                    true
                }
                Err(RecvError::Lagged(_)) => {
                    let mut pending = pending
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    pending.changed_threads.clear();
                    pending.all_mail_changed = true;
                    pending.calendar_changed = true;
                    pending.contacts_changed = true;
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

pub(crate) fn refresh_oauth_availability(app: &AppWindow) {
    use flectar_mail_core::{models::Provider, oauth::providers::resolve_credentials};
    app.set_google_oauth_available(resolve_credentials(Provider::Gmail).is_ok());
    app.set_microsoft_oauth_available(resolve_credentials(Provider::Microsoft).is_ok());
}

pub(crate) fn apply_settings(app: &AppWindow, settings: &Settings) {
    app.global::<MessageColors>()
        .set_preference(settings.message_appearance.as_str().into());
    refresh_oauth_availability(app);
    app.set_time_format(settings.time_format.as_str().into());
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
    app.set_collect_outgoing_contacts(settings.collect_outgoing_contacts);
    app.set_collect_incoming_contacts(settings.collect_incoming_contacts);
    app.set_contact_suggest_all_accounts(settings.contact_suggest_all_accounts);
    app.set_suggest_learned_contacts(settings.suggest_learned_contacts);
    app.set_close_to_tray(settings.close_to_tray && !cfg!(feature = "flatpak"));
    crate::window_chrome::apply_preference(
        app,
        crate::window_chrome::WindowFrame::parse(&settings.window_frame),
    );
    app.set_monochrome_sidebar_icons(settings.monochrome_sidebar_icons);
    crate::app_icon::apply(app, settings.app_icon);
    app.set_show_avatars(settings.show_avatars);
    app.set_group_mail_by_date(settings.group_mail_by_date);
    app.set_show_account_markers(settings.show_account_badges);
    app.set_workspace_layout(
        match settings.workspace_layout.as_str() {
            "minimal" => "minimal",
            _ => "default",
        }
        .into(),
    );
    app.set_workspace_list_pane_width(normalized_workspace_list_pane_width(
        settings.workspace_list_pane_width,
    ) as f32);
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
    use slint::platform::{
        Platform, WindowAdapter,
        software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
    };
    use slint::{Model, Rgb8Pixel};
    use std::{cell::Cell, time::Duration};

    struct Headless(Rc<MinimalSoftwareWindow>, Rc<Cell<Duration>>);

    impl Platform for Headless {
        fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
            Ok(self.0.clone())
        }

        fn duration_since_start(&self) -> Duration {
            self.1.get()
        }
    }

    const PERSON_INBOX: Scope = Scope::Account {
        account_id: 7,
        mailbox: crate::StandardMailbox::Inbox,
    };

    /// A headless window with a controlled clock and an empty inbox state.
    struct Harness {
        window: Rc<MinimalSoftwareWindow>,
        clock: Rc<Cell<Duration>>,
        app: AppWindow,
        runtime: tokio::runtime::Runtime,
        state: Rc<RefCell<InboxState>>,
        _cache: tempfile::TempDir,
    }

    impl Harness {
        fn new() -> Self {
            let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
            let clock = Rc::new(Cell::new(Duration::ZERO));
            slint::platform::set_platform(Box::new(Headless(window.clone(), clock.clone())))
                .unwrap();
            let app = AppWindow::new().unwrap();
            app.window().set_size(slint::PhysicalSize::new(1320, 800));
            app.show().unwrap();
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let cache = tempfile::tempdir().unwrap();
            let (favicons, _) = crate::bounded_ui_channel();
            let (avatars, _) = crate::bounded_ui_channel();
            let state = InboxState::empty(
                None,
                crate::UiSender::new(favicons, UiWake::new(app.as_weak(), |_| {})),
                None,
                crate::UiSender::new(avatars, UiWake::new(app.as_weak(), |_| {})),
                false,
                false,
                WarmStartCacheWriter::spawn(&runtime, cache.path().join("warm.json")),
            );
            app.set_emails(state.email_rows.clone().into());
            app.set_mail_list_entries(state.mail_list_entries.clone().into());
            app.set_sidebar_rows(state.sidebar_rows.clone().into());
            Self {
                window,
                clock,
                app,
                runtime,
                state: Rc::new(RefCell::new(state)),
                _cache: cache,
            }
        }

        fn apply_single_account_snapshot(&self) {
            apply_warm_start_snapshot(
                &self.app,
                &self.state,
                &self.runtime,
                single_account_snapshot(),
            );
        }

        /// Paint a settled frame, saved as `<name>.png` when
        /// `FLECTAR_STARTUP_REVIEW_DIR` is set.
        fn render(&self, name: &str) -> Vec<Rgb8Pixel> {
            let size = self.app.window().size();
            let mut pixels = vec![Rgb8Pixel::default(); (size.width * size.height) as usize];
            // Evaluate bindings before advancing the clock: the compact drawer
            // starts its animation on the first frame. Clicks and snapshots
            // must see the settled layout even when PNG output is disabled.
            for elapsed in [Duration::ZERO, Duration::from_secs(1)] {
                self.clock.set(self.clock.get() + elapsed);
                slint::platform::update_timers_and_animations();
                self.app.window().request_redraw();
                self.window.draw_if_needed(|renderer| {
                    renderer.render(&mut pixels, size.width as usize);
                });
            }
            if let Some(directory) = std::env::var_os("FLECTAR_STARTUP_REVIEW_DIR") {
                let directory = PathBuf::from(directory);
                std::fs::create_dir_all(&directory).unwrap();
                let bytes: Vec<u8> = pixels.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
                image::save_buffer(
                    directory.join(format!("{name}.png")),
                    &bytes,
                    size.width,
                    size.height,
                    image::ColorType::Rgb8,
                )
                .unwrap();
            }
            pixels
        }

        fn click(&self, x: f32, y: f32) {
            let position = slint::LogicalPosition::new(x, y);
            let button = slint::platform::PointerEventButton::Left;
            self.app
                .window()
                .dispatch_event(slint::platform::WindowEvent::PointerPressed { position, button });
            self.app
                .window()
                .dispatch_event(slint::platform::WindowEvent::PointerReleased { position, button });
        }

        fn shows_unified_inbox(&self) -> bool {
            self.state
                .borrow()
                .sidebar_rows
                .iter()
                .any(|row| row.kind == crate::SidebarRowKind::UnifiedInbox)
        }
    }

    /// One account whose cache was written while the unified inbox was
    /// selected, with every standard mailbox discovered.
    fn single_account_snapshot() -> WarmStartSnapshot {
        let mut mailboxes = vec![mail::MailboxEntry {
            is_account: true,
            is_standard: false,
            label: "Person".into(),
            scope: None,
            count: String::new(),
            ..mailbox()
        }];
        for (index, mailbox_kind) in crate::StandardMailbox::ALL.into_iter().enumerate() {
            mailboxes.push(mail::MailboxEntry {
                label: mailbox_kind.label().into(),
                scope: Some(Scope::Account {
                    account_id: 7,
                    mailbox: mailbox_kind,
                }),
                folder_id: index as i64 + 1,
                count: if index == 0 {
                    "3".into()
                } else {
                    String::new()
                },
                ..mailbox()
            });
        }
        WarmStartSnapshot::capture(WarmStartProjection {
            scope: Scope::UNIFIED_INBOX,
            inbox_count: 3,
            next_cursor: None,
            accounts: &[account()],
            profile_accounts: None,
            messages: &[message()],
            mailboxes: &mailboxes,
            unified_mailboxes: &[],
        })
    }

    #[test]
    fn warm_start_paints_accounts_and_rows_before_core_ready() {
        let harness = Harness::new();
        let app = &harness.app;
        let mut loading_frames = Vec::new();
        for theme in ["light", "dark"] {
            app.set_theme_mode(theme.into());
            let frame = harness.render(&format!("loading-{theme}"));
            assert!(
                frame.iter().any(|pixel| *pixel != frame[0]),
                "account discovery must display a loading view instead of a blank window"
            );
            loading_frames.push(frame);
        }

        harness.apply_single_account_snapshot();

        let state = &harness.state;
        assert!(app.get_startup_hydrated());
        assert!(!app.get_startup_ready());
        assert!(state.borrow().core.is_none());
        assert!(state.borrow().messages[0].html.is_none());
        assert_eq!(app.get_connected_accounts().row_count(), 1);
        assert_eq!(app.get_connected_accounts().row_data(0).unwrap().id, 7);
        assert_eq!(app.get_emails().row_count(), 1);
        assert_eq!(
            app.get_emails().row_data(0).unwrap().subject,
            "Cached subject"
        );
        // The cached unified selection resolves to the only account's inbox,
        // which the sidebar lists first, without a unified section.
        assert_eq!(state.borrow().scope, PERSON_INBOX);
        assert_eq!(app.get_selected_scope(), PERSON_INBOX.to_string());
        assert_eq!(app.get_selected_scope_title(), "Person / Inbox");
        let rows = state.borrow().sidebar_rows.iter().collect::<Vec<_>>();
        assert_eq!(rows[0].kind, crate::SidebarRowKind::Account);
        assert!(!harness.shows_unified_inbox());
        assert_eq!(
            rows.iter()
                .filter(|row| row.kind == crate::SidebarRowKind::Folder)
                .count(),
            crate::StandardMailbox::ALL.len()
        );
        for (index, theme) in ["light", "dark"].into_iter().enumerate() {
            app.set_theme_mode(theme.into());
            assert_ne!(
                harness.render(&format!("cached-{theme}")),
                loading_frames[index]
            );
        }
        app.window().set_size(slint::PhysicalSize::new(390, 844));
        harness.render("cached-compact");
        assert!(
            !app.get_startup_ready(),
            "rendering cached mail must not unlock storage actions"
        );
    }

    /// The projected account folders must be usable in the compact drawer as
    /// well as the desktop sidebar once startup unlocks the shell.
    #[test]
    fn single_account_inbox_is_selectable_in_compact_and_desktop_layouts() {
        let harness = Harness::new();
        let app = &harness.app;
        harness.apply_single_account_snapshot();
        app.set_startup_ready(true);
        app.global::<crate::MotionSettings>().set_enabled(false);
        let selected = Rc::new(RefCell::new(String::new()));
        let clicked = selected.clone();
        app.on_select_scope(move |scope| *clicked.borrow_mut() = scope.to_string());
        app.window().set_size(slint::PhysicalSize::new(390, 844));
        for theme in ["light", "dark"] {
            app.set_theme_mode(theme.into());
            harness.render(&format!("ready-compact-{theme}"));
            harness.click(31., 29.);
            harness.render(&format!("ready-compact-drawer-{theme}"));
            selected.borrow_mut().clear();
            harness.click(140., 165.);
            assert_eq!(selected.borrow().parse(), Ok(PERSON_INBOX));
            app.window().set_size(slint::PhysicalSize::new(1320, 800));
            harness.render(&format!("ready-desktop-{theme}"));
            selected.borrow_mut().clear();
            harness.click(140., 209.);
            assert_eq!(selected.borrow().parse(), Ok(PERSON_INBOX));
            app.window().set_size(slint::PhysicalSize::new(390, 844));
        }
    }

    #[test]
    fn account_inventory_controls_the_unified_section_and_visible_selection() {
        let harness = Harness::new();
        harness.apply_single_account_snapshot();
        let state = &harness.state;
        // A second account counts even before its folders arrive, and even
        // when offline. Adding it keeps the account's own selection.
        let mut offline = account();
        offline.id = 8;
        offline.sync_state = "error".into();
        offline.sync_error = Some("Offline".into());
        let both = vec![account(), offline];
        assert!(!state.borrow_mut().set_connected_accounts(both.clone()));
        // Searching folders must not redefine that inventory.
        for query in ["", "Inbox", ""] {
            state.borrow_mut().folder_filter = query.into();
            crate::refresh_sidebar(state);
            assert_eq!(harness.shows_unified_inbox(), query.is_empty());
            assert_eq!(state.borrow().scope, PERSON_INBOX);
        }

        // A unified selection that would disappear moves to the survivor.
        state.borrow_mut().set_scope(Scope::Unified(crate::StandardMailbox::Sent));
        assert!(state.borrow_mut().set_connected_accounts(vec![account()]));
        assert_eq!(
            state.borrow().scope,
            Scope::Account {
                account_id: 7,
                mailbox: crate::StandardMailbox::Sent,
            }
        );
        crate::refresh_sidebar(state);
        assert!(!harness.shows_unified_inbox());
        assert!(!state.borrow_mut().set_connected_accounts(both));
        crate::refresh_sidebar(state);
        assert!(harness.shows_unified_inbox());
    }

    fn account() -> Account {
        Account {
            id: 7,
            email: "person@example.com".into(),
            display_name: Some("Person".into()),
            avatar_url: None,
            provider: Provider::Generic,
            auth_kind: AuthKind::Password,
            mail_protocol: MailProtocol::Imap,
            sync_state: "idle".into(),
            sync_error: None,
            can_create_top_level_mailbox: true,
            server_keeps_mail_days: None,
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
            date_ms: 1_700_000_000_000,
            to: "secret-recipient@example.com".into(),
            label: "UNREAD".into(),
            unread: true,
            starred: false,
            has_attachments: false,
            message_count: 2,
            has_replied: true,
            is_outgoing: false,
            labels: Vec::new(),
            html: Some("<p>body must not enter warm cache</p>".into()),
            text: Some("Plain body must not enter warm cache".into()),
            attachments: Vec::new(),
            body_pending: false,
            sender_verification: "domain".into(),
            verification_code: Some("48291375".into()),
            account_uses_labels: true,
            first_attachment: Some(flectar_mail_core::models::AttachmentMeta {
                id: 3,
                filename: Some("cached-file-name.pdf".into()),
                mime_type: Some("application/pdf".into()),
                size: Some(42),
                is_inline: false,
            }),
            attachment_total: 1,
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
            is_selectable: true,
            can_create_children: true,
            can_rename: false,
            can_delete: false,
            label: "Inbox".into(),
            scope: Some(PERSON_INBOX),
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
            scope: PERSON_INBOX,
            inbox_count: 3,
            next_cursor: Some(ThreadCursor {
                last_message_at: 123,
                thread_id: 7,
            }),
            accounts: &accounts,
            profile_accounts: Some(&[7]),
            messages: &messages,
            mailboxes: &mailboxes,
            unified_mailboxes: &[],
        });

        write_warm_start_snapshot(&path, &snapshot).await.unwrap();
        let bytes = tokio::fs::read(&path).await.unwrap();
        let json = String::from_utf8(bytes).unwrap();
        assert!(!json.contains("body must not enter"));
        assert!(!json.contains("secret-recipient"));
        assert!(!json.contains("48291375"));
        assert!(!json.contains("cached-file-name"));

        let restored = load_warm_start_snapshot(&path).await.unwrap();
        assert_eq!(restored.scope, PERSON_INBOX);
        assert_eq!(restored.profile_accounts, Some(vec![7]));
        assert_eq!(restored.messages.len(), 1);
        let restored_message = mail::MailMessage::from(restored.messages[0].clone());
        assert_eq!(restored_message.subject, "Cached subject");
        assert!(restored_message.has_replied);
        assert!(restored_message.html.is_none());
        assert!(restored_message.body_pending);
        assert!(restored_message.verification_code.is_none());
        assert!(restored_message.account_uses_labels);
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

    /// Version 3 keyed account mailboxes by display name; such a cache is a
    /// miss rather than a selection of the wrong mailbox.
    #[tokio::test]
    async fn previous_format_warm_start_is_ignored_and_removed() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("warm.json");
        let mut json = serde_json::to_value(single_account_snapshot()).unwrap();
        json["format_version"] = 3.into();
        json["scope"] = "Person / Inbox".into();
        tokio::fs::write(&path, json.to_string()).await.unwrap();

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
            scope: Scope::UNIFIED_INBOX,
            inbox_count: messages.len(),
            next_cursor: None,
            accounts: &accounts,
            profile_accounts: None,
            messages: &messages,
            mailboxes: &mailboxes,
            unified_mailboxes: &[],
        });

        assert_eq!(snapshot.messages.len(), MAX_WARM_START_MESSAGES);
    }

    #[test]
    fn benchmark_warm_cache_fixture_matches_the_current_format() {
        let snapshot: WarmStartSnapshot = serde_json::from_str(include_str!(
            "../resources/benchmarks/warm-cache/warm-start-mailbox-v6.json"
        ))
        .unwrap();

        assert!(snapshot.is_valid());
        assert_eq!(snapshot.messages.len(), 3);
        // Caches written before profiles filtered mail show every account.
        assert_eq!(snapshot.profile_accounts, None);
    }
}
