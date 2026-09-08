//! Email, calendar, account, and persistence logic shared by the native hosts.
//! The application embeds [`Core`], calls its async methods, and forwards
//! [`CoreEvent`]s to the Slint UI.

pub mod accounts;
pub mod ai;
pub mod autolabel;
pub mod caldav;
pub mod calendar;
pub mod config;
pub mod db;
pub mod embed;
pub mod error;
pub mod events;
mod file_io;
pub mod googlecal;
pub mod graph;
pub mod graphcal;
mod http_body;
pub mod imap;
pub mod jmap;
pub mod mime;
pub mod models;
pub mod oauth;
pub mod queue;
pub mod route;
pub mod scheduler;
pub mod search;
pub mod smtp;
pub mod sync;
pub mod unsubscribe;

use crate::accounts::credentials::{self, CredentialStoreHandle, Slot, SystemCredentialStore};
use crate::config::Paths;
use crate::db::Db;
use crate::db::repo;
#[cfg(feature = "local-embeddings")]
use crate::embed::Embedder;
use crate::error::{CoreError, Result};
use crate::events::{CoreEvent, EventBus};
use crate::models::*;
use crate::oauth::redirect::{LoopbackRedirectBroker, OAuthRedirectBrokerHandle};
use crate::oauth::tokens::TokenProvider;
use crate::sync::engine::{AccountHandle, SyncCmd, SyncCtx, spawn_account};
use rusqlite::OptionalExtension;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::RwLock;

const MAX_INLINE_CID_BYTES_PER_IMAGE: usize = 8 * 1024 * 1024;
const MAX_INLINE_CID_BYTES_PER_MESSAGE: usize = 16 * 1024 * 1024;
const MAX_INLINE_CID_BYTES_PER_THREAD: usize = 32 * 1024 * 1024;
const MAX_INLINE_CID_IMAGES_PER_MESSAGE: usize = 32;
const MAX_DRAFT_ATTACHMENTS: usize = 32;
const MAX_DRAFT_ATTACHMENT_BYTES: usize = 25 * 1024 * 1024;
const MAX_DRAFT_BODY_BYTES: usize = 8 * 1024 * 1024;
const MAX_CACHED_MESSAGE_BYTES: usize = 128 * 1024 * 1024;
const MAX_CACHED_HEADER_BYTES: usize = 256 * 1024;

fn validate_folder_leaf(value: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(CoreError::Other("folder name cannot be empty".into()));
    }
    if value.chars().count() > 255 {
        return Err(CoreError::Other(
            "folder name cannot be longer than 255 characters".into(),
        ));
    }
    if value.chars().any(char::is_control) {
        return Err(CoreError::Other(
            "folder name cannot contain control characters".into(),
        ));
    }
    Ok(value.to_owned())
}

pub use crate::db::repo::notifications::{NotificationOutboxItem, RoutedTab};
pub use crate::db::snapshot::DatabaseSnapshotManifest;

fn normalize_account_email(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() || value.len() > 320 || value.chars().any(char::is_whitespace) {
        return None;
    }
    let (local, domain) = value.rsplit_once('@')?;
    if local.is_empty()
        || domain.is_empty()
        || local.contains('@')
        || domain.contains('@')
        || domain.starts_with('.')
        || domain.ends_with('.')
    {
        return None;
    }
    Some(format!("{local}@{}", domain.to_lowercase()))
}

#[cfg(test)]
mod account_email_tests {
    use super::normalize_account_email;

    #[test]
    fn account_email_preserves_local_part_and_normalizes_domain() {
        assert_eq!(
            normalize_account_email(" User.Name@EXAMPLE.TEST ").as_deref(),
            Some("User.Name@example.test")
        );
        assert!(normalize_account_email("missing-domain@").is_none());
        assert!(normalize_account_email("two@@example.test").is_none());
        assert!(normalize_account_email("space @example.test").is_none());
    }
}

/// An AI feature, each routed to a configurable model tier.
#[derive(Clone, Copy)]
enum Scenario {
    /// Ask-your-inbox agentic Q&A.
    Ask,
    /// Drafting / rewriting replies.
    Draft,
    /// Thread summaries.
    Summarize,
    /// Learning the user's writing voice.
    Voice,
    /// Palette natural-language commands (tiny prompt, latency-sensitive).
    Command,
    /// One-tap quick-reply chips (tiny output, latency-sensitive).
    QuickReply,
    /// Sorting incoming mail into categories at sync (short prompt, per-email).
    Categorize,
}

impl Scenario {
    fn as_str(self) -> &'static str {
        match self {
            Scenario::Ask => "ask",
            Scenario::Draft => "draft",
            Scenario::Summarize => "summarize",
            Scenario::Voice => "voice",
            Scenario::Command => "command",
            Scenario::QuickReply => "quick_reply",
            Scenario::Categorize => "automation",
        }
    }
}

/// Resolve the canonical model id for a scenario's configured tier.
fn resolve_ai_model(settings: &Settings, scenario: Scenario) -> String {
    let tier = match scenario {
        Scenario::Ask => settings.ai_tier_ask.as_str(),
        Scenario::Draft => settings.ai_tier_draft.as_str(),
        Scenario::Summarize => settings.ai_tier_summarize.as_str(),
        Scenario::Voice => settings.ai_tier_voice.as_str(),
        Scenario::Categorize => settings.ai_tier_categorize.as_str(),
        // Palette parsing and reply chips want the fastest model available.
        Scenario::Command | Scenario::QuickReply => "instant",
    };
    match tier {
        "instant" => &settings.ai_model_instant,
        "cheap" => &settings.ai_model_cheap,
        "intelligent" => &settings.ai_model_intelligent,
        _ => &settings.ai_model_intelligent,
    }
    .clone()
}

fn oauth_calendar_scopes(provider: Provider, enabled: bool) -> &'static [&'static str] {
    if !enabled {
        return &[];
    }
    match provider {
        Provider::Gmail => &[oauth::providers::GOOGLE_CALENDAR_SCOPE],
        Provider::Microsoft => &[
            oauth::providers::MS_ONLINE_MEETINGS_SCOPE,
            oauth::providers::MS_CALENDARS_SCOPE,
        ],
        Provider::Imap => &[],
    }
}

#[cfg(test)]
mod calendar_oauth_scope_tests {
    use super::*;

    #[test]
    fn mail_only_sign_in_never_requests_calendar_permissions() {
        assert!(oauth_calendar_scopes(Provider::Gmail, false).is_empty());
        assert!(oauth_calendar_scopes(Provider::Microsoft, false).is_empty());
    }

    #[test]
    fn opt_in_requests_the_provider_specific_calendar_permissions() {
        assert_eq!(
            oauth_calendar_scopes(Provider::Gmail, true),
            &[oauth::providers::GOOGLE_CALENDAR_SCOPE]
        );
        let microsoft = oauth_calendar_scopes(Provider::Microsoft, true);
        assert!(microsoft.contains(&oauth::providers::MS_CALENDARS_SCOPE));
        assert!(microsoft.contains(&oauth::providers::MS_ONLINE_MEETINGS_SCOPE));
    }
}

#[derive(Clone)]
pub struct ThreadPageRequest {
    pub cursor: Option<ThreadCursor>,
    pub limit: i64,
}

impl From<(Option<ThreadCursor>, i64)> for ThreadPageRequest {
    fn from((cursor, limit): (Option<ThreadCursor>, i64)) -> Self {
        Self { cursor, limit }
    }
}

#[derive(Clone)]
pub struct Core {
    /// Mail, contacts, folders, and mail-side durable actions.
    pub db: Db,
    /// Events and provider calendar sync state. Kept physically separate from
    /// the mail store so mailbox backfills cannot block the calendar UI.
    pub calendar_db: Db,
    pub bus: EventBus,
    paths: Arc<Paths>,
    tokens: TokenProvider,
    credentials: CredentialStoreHandle,
    oauth_redirects: OAuthRedirectBrokerHandle,
    handles: Arc<RwLock<HashMap<i64, AccountHandle>>>,
    cal_handles: Arc<RwLock<HashMap<i64, caldav::task::CalTaskHandle>>>,
    /// Per-attachment single-flight locks. Concurrent preview/open/save calls
    /// share one remote fetch and cannot race the same `.download.part` file.
    attachment_locks:
        Arc<tokio::sync::Mutex<HashMap<i64, std::sync::Weak<tokio::sync::Mutex<()>>>>>,
    #[cfg(feature = "local-embeddings")]
    embed: Arc<embed::EmbedState>,
    /// Fired by `cancel_oauth` to abort a pending browser sign-in (the
    /// loopback wait otherwise blocks the UI until its 5-minute timeout).
    oauth_cancel: Arc<tokio::sync::Notify>,
    /// Fired by `notify_ui_ready` once the Slint UI has finished its startup
    /// show (the first-run intro). Account actors wait for this before
    /// touching OAuth tokens, so the OS keyring prompt never lands on top of
    /// the intro. A timeout fallback keeps tray-only/headless syncing alive.
    ui_ready: Arc<tokio::sync::Notify>,
    /// Serializes AI routing passes so the background router and an inline
    /// re-sort never classify the same 'pending' threads twice.
    ai_router_lock: Arc<tokio::sync::Mutex<()>>,
}

impl Core {
    pub async fn start(paths: Paths) -> Result<Core> {
        Self::start_with_platform(
            paths,
            Arc::new(SystemCredentialStore),
            Arc::new(LoopbackRedirectBroker),
            true,
        )
        .await
    }

    /// Start the core for a mail-only native shell. Mail synchronization,
    /// folder discovery, and keyword search stay fully enabled, while the
    /// optional local embedding model is left unloaded until a host that
    /// exposes semantic search asks for it.
    pub async fn start_mail_ui(paths: Paths) -> Result<Core> {
        Self::start_mail_ui_with_credentials(paths, Arc::new(SystemCredentialStore)).await
    }

    /// Start a mail-only core with a host-provided credential backend. Mobile
    /// hosts use this entry point so no unsupported keyring fallback can run.
    pub async fn start_mail_ui_with_credentials(
        paths: Paths,
        credentials: CredentialStoreHandle,
    ) -> Result<Core> {
        Self::start_mail_ui_with_platform(paths, credentials, Arc::new(LoopbackRedirectBroker))
            .await
    }

    pub async fn start_mail_ui_with_platform(
        paths: Paths,
        credentials: CredentialStoreHandle,
        oauth_redirects: OAuthRedirectBrokerHandle,
    ) -> Result<Core> {
        Self::start_with_platform(paths, credentials, oauth_redirects, false).await
    }

    async fn start_with_platform(
        paths: Paths,
        credentials: CredentialStoreHandle,
        oauth_redirects: OAuthRedirectBrokerHandle,
        enable_embeddings: bool,
    ) -> Result<Core> {
        tracing::info!(
            data_dir = %paths.data_dir.display(),
            cache_dir = %paths.cache_dir.display(),
            "core startup: resolved application storage"
        );
        paths.ensure().map_err(|error| {
            CoreError::Other(format!(
                "creating application directories {} and {}: {error}",
                paths.data_dir.display(),
                paths.cache_dir.display()
            ))
        })?;
        let mail_db_path = paths.db_file();
        tracing::info!(database = %mail_db_path.display(), "core startup: opening mail database");
        let db = Db::open(&mail_db_path).map_err(|error| {
            CoreError::Other(format!(
                "opening mail database {}: {error}",
                mail_db_path.display()
            ))
        })?;
        let calendar_db_path = paths.calendar_db_file();
        tracing::info!(database = %calendar_db_path.display(), "core startup: opening calendar database");
        let calendar_db = Db::open_calendar(&calendar_db_path).map_err(|error| {
            CoreError::Other(format!(
                "opening calendar database {}: {error}",
                calendar_db_path.display()
            ))
        })?;
        tracing::info!("core startup: databases opened and migrated");
        let bus = EventBus::new();
        let core = Core {
            db,
            calendar_db,
            bus,
            paths: Arc::new(paths),
            tokens: TokenProvider::new(credentials.clone(), oauth_redirects.clone()),
            credentials,
            oauth_redirects,
            handles: Arc::new(RwLock::new(HashMap::new())),
            cal_handles: Arc::new(RwLock::new(HashMap::new())),
            attachment_locks: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            #[cfg(feature = "local-embeddings")]
            embed: Arc::new(embed::EmbedState::new()),
            oauth_cancel: Arc::new(tokio::sync::Notify::new()),
            ui_ready: Arc::new(tokio::sync::Notify::new()),
            ai_router_lock: Arc::new(tokio::sync::Mutex::new(())),
        };

        // Finish any destructive operation interrupted between the mail and
        // calendar stores, then remove calendar rows that predate the durable
        // operation journal and no longer have a mail account owner.
        core.recover_cross_store_state().await?;
        let removed_staged_files = core.cleanup_orphaned_draft_files().await?;
        if removed_staged_files > 0 {
            tracing::info!(removed_staged_files, "removed orphaned draft staging files");
        }

        // Make saved OAuth app registrations available before any actor
        // needs a token refresh.
        let settings = core.db.read(|conn| repo::settings::get(conn)).await?;
        apply_oauth_settings(&settings);

        // Recover any actions orphaned mid-flight by a previous crash/kill, so a
        // send that was executing when the app died retries instead of sticking
        // on "Sending…" forever.
        match core
            .db
            .write(|conn| repo::actions::recover_inflight(conn))
            .await
        {
            Ok(n) if n > 0 => tracing::info!("recovered {n} orphaned in-flight action(s)"),
            _ => {}
        }
        match core
            .calendar_db
            .write(|conn| repo::actions::recover_inflight(conn))
            .await
        {
            Ok(n) if n > 0 => tracing::info!("recovered {n} calendar action(s)"),
            _ => {}
        }
        match core
            .db
            .write(|conn| {
                Ok(conn.execute(
                    "UPDATE messages SET body_state = 'none' WHERE body_state = 'fetching'",
                    [],
                )?)
            })
            .await
        {
            Ok(n) if n > 0 => tracing::info!("recovered {n} orphaned content fetch(es)"),
            _ => {}
        }
        // Spawn actors for existing accounts - but only after the Slint UI
        // reports ready (notify_ui_ready). Actors immediately load OAuth
        // tokens from the OS keyring, and on a launch that plays the intro
        // that prompt must land after the show, not on top of it. The timeout
        // fallback keeps sync alive if no window ever reports in (tray-only
        // or a stalled startup).
        {
            let core = core.clone();
            tokio::spawn(async move {
                tokio::select! {
                    _ = core.ui_ready.notified() => {}
                    _ = tokio::time::sleep(std::time::Duration::from_secs(60)) => {}
                }
                let configs = match core
                    .db
                    .read(|conn| repo::accounts::list_configs(conn))
                    .await
                {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::error!("listing accounts for startup sync: {e}");
                        return;
                    }
                };
                for cfg in configs {
                    core.spawn_actor(cfg).await;
                }
                // Calendar sync tasks for accounts with a connected CalDAV server.
                if let Ok(cal_accounts) = core
                    .calendar_db
                    .read(|conn| repo::caldav::all_configs(conn))
                    .await
                {
                    for cfg in cal_accounts {
                        core.spawn_cal_task(cfg.account_id).await;
                    }
                }
            });
        }

        scheduler::spawn(
            core.db.clone(),
            core.calendar_db.clone(),
            core.bus.clone(),
            core.handles.clone(),
            core.cal_handles.clone(),
        );

        #[cfg(feature = "local-embeddings")]
        if enable_embeddings {
            // Make the bundled default model available for offline first run,
            // then start the background embedding worker. Hosts that only
            // expose mailbox sync should not pay the memory cost of a local
            // BERT model they cannot use.
            core.provision_bundled_model().await;
            embed::worker::spawn(core.db.clone(), core.embed.clone(), core.paths.clone());
        }
        #[cfg(not(feature = "local-embeddings"))]
        let _ = enable_embeddings;

        // One-shot routing backfill: on a fresh 007 install or an upgrade to the
        // 014 routing column, resolve every existing thread's tab once. Guarded
        // by a marker row so it never repeats (preserved across reroute_all).
        {
            let c = core.clone();
            tokio::spawn(async move {
                let (marker, threads, auto) =
                    c.db.read(|conn| {
                        let s = repo::settings::get(conn)?;
                        let marker: i64 = conn.query_row(
                            "SELECT COUNT(*) FROM route_cache
                             WHERE sender_domain = '__routing_backfill__'",
                            [],
                            |r| r.get(0),
                        )?;
                        let threads: i64 =
                            conn.query_row("SELECT COUNT(*) FROM threads", [], |r| r.get(0))?;
                        Ok((marker, threads, s.auto_labels_enabled))
                    })
                    .await
                    .unwrap_or((1, 0, false));
                if marker == 0 {
                    if auto && threads > 0 {
                        match c.reroute_all().await {
                            Ok(n) => tracing::info!("routing backfill resolved {n} threads"),
                            Err(e) => tracing::warn!("routing backfill failed: {e}"),
                        }
                    }
                    let _ =
                        c.db.write(|conn| {
                            conn.execute(
                                "INSERT OR REPLACE INTO route_cache (sender_domain, route_key)
                                 VALUES ('__routing_backfill__', '1')",
                                [],
                            )?;
                            Ok(())
                        })
                        .await;
                }
            });
        }

        // Background AI router: drains threads marked 'pending' into a category
        // whenever mail changes. Decoupled from sync so LLM latency never blocks
        // the sync loop.
        {
            let c = core.clone();
            tokio::spawn(async move { c.ai_router_loop().await });
        }

        Ok(core)
    }

    /// Long-lived loop: on every mail change, drain the AI routing queue in
    /// bounded batches. Serial (one broadcast receiver), so batches never
    /// overlap; the self-emitted refresh settles after one empty pass.
    async fn ai_router_loop(&self) {
        use tokio::sync::broadcast::error::RecvError;
        let mut rx = self.bus.subscribe();
        loop {
            match rx.recv().await {
                Ok(CoreEvent::MailUpdated { .. }) => loop {
                    match self.classify_pending(50).await {
                        Ok(0) => break,
                        Ok(_) => continue,
                        Err(e) => {
                            tracing::warn!("ai routing pass failed: {e}");
                            break;
                        }
                    }
                },
                Ok(_) => {}
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            }
        }
    }

    /// Copy the installer-bundled default model into the data dir if it isn't
    /// there yet, so semantic search works with no network on first launch.
    /// Best-effort: if the resource is missing (e.g. dev builds), the worker
    /// falls back to downloading on demand.
    #[cfg(feature = "local-embeddings")]
    async fn provision_bundled_model(&self) {
        let models_dir = self.paths.models_dir();
        let spec = embed::spec_or_default(embed::DEFAULT_MODEL);
        if embed::model_present(&models_dir, spec.key) {
            return;
        }
        if let Some(src) = bundled_model_dir(spec.key) {
            let dst = embed::model_dir(&models_dir, spec.key);
            if let Err(e) = copy_model_files(&src, &dst, spec).await {
                tracing::warn!("bundled model copy failed: {e}");
            }
        }
    }

    fn sync_ctx(&self) -> SyncCtx {
        SyncCtx {
            db: self.db.clone(),
            calendar_db: self.calendar_db.clone(),
            bus: self.bus.clone(),
            paths: self.paths.clone(),
            tokens: self.tokens.clone(),
            credentials: self.credentials.clone(),
        }
    }

    async fn spawn_actor(&self, cfg: AccountConfig) {
        let handle = spawn_account(self.sync_ctx(), cfg);
        self.handles.write().await.insert(handle.account_id, handle);
    }

    async fn spawn_cal_task(&self, account_id: i64) {
        let handle = caldav::task::spawn(
            self.calendar_db.clone(),
            self.db.clone(),
            self.bus.clone(),
            self.tokens.clone(),
            self.credentials.clone(),
            account_id,
        );
        self.cal_handles.write().await.insert(account_id, handle);
    }

    async fn nudge_cal(&self, account_id: Option<i64>) {
        let handles = self.cal_handles.read().await;
        match account_id {
            Some(id) => {
                if let Some(h) = handles.get(&id) {
                    h.nudge();
                }
            }
            None => {
                for h in handles.values() {
                    h.nudge();
                }
            }
        }
    }

    async fn nudge(&self, account_id: Option<i64>, cmd_for: impl Fn() -> SyncCmd) {
        let handles = self.handles.read().await;
        match account_id {
            Some(id) => {
                if let Some(h) = handles.get(&id) {
                    h.send(cmd_for());
                }
            }
            None => {
                for h in handles.values() {
                    h.send(cmd_for());
                }
            }
        }
    }

    pub async fn list_accounts(&self) -> Result<Vec<Account>> {
        self.db.read(|conn| repo::accounts::list(conn)).await
    }

    /// Native settings surfaces need the editable server fields alongside the
    /// public account summary. Credentials remain in the OS keyring and are
    /// deliberately not included.
    pub async fn list_account_configs(&self) -> Result<Vec<AccountConfig>> {
        self.db
            .read(|conn| repo::accounts::list_configs(conn))
            .await
    }

    /// Import non-secret account metadata from a portable backup. Existing
    /// addresses are preserved, and imported accounts remain paused until the
    /// user authenticates on this device.
    pub async fn import_account_configs(
        &self,
        configs: Vec<PortableAccountConfig>,
    ) -> Result<usize> {
        for config in &configs {
            let email = config.email.trim();
            if email.len() > 320 || !email.contains('@') {
                return Err(CoreError::Other(format!(
                    "backup contains an invalid email address: {}",
                    config.email
                )));
            }
            if config.username.len() > 512
                || config.jmap_url.len() > 2048
                || config.imap_host.len() > 253
                || config.smtp_host.len() > 253
            {
                return Err(CoreError::Other(
                    "backup contains an account field that is too long".into(),
                ));
            }
            let valid_pair = matches!(
                (config.provider, config.auth_kind),
                (Provider::Imap, AuthKind::Password)
                    | (Provider::Gmail, AuthKind::Oauth2)
                    | (Provider::Microsoft, AuthKind::Oauth2)
            );
            if !valid_pair {
                return Err(CoreError::Other(format!(
                    "backup contains an unsupported sign-in type for {email}"
                )));
            }
            if config.provider == Provider::Imap
                && config.mail_protocol == MailProtocol::Imap
                && (config.username.trim().is_empty()
                    || config.imap_host.trim().is_empty()
                    || config.smtp_host.trim().is_empty()
                    || config.imap_port == 0
                    || config.smtp_port == 0)
            {
                return Err(CoreError::Other(format!(
                    "backup contains incomplete IMAP settings for {email}"
                )));
            }
            if config.provider == Provider::Imap
                && config.mail_protocol == MailProtocol::Jmap
                && config.username.trim().is_empty()
            {
                return Err(CoreError::Other(format!(
                    "backup contains incomplete JMAP settings for {email}"
                )));
            }
        }

        self.db
            .write(move |conn| {
                let transaction = conn.transaction()?;
                let mut imported = 0usize;
                for config in &configs {
                    if repo::accounts::find_by_email(&transaction, config.email.trim())?.is_some() {
                        continue;
                    }
                    let account_id = repo::accounts::insert(
                        &transaction,
                        &repo::accounts::NewAccount {
                            email: config.email.trim(),
                            display_name: config.display_name.as_deref(),
                            avatar_url: None,
                            provider: config.provider,
                            auth_kind: config.auth_kind,
                            mail_protocol: config.mail_protocol,
                            username: config.username.trim(),
                            jmap_url: config.jmap_url.trim(),
                            jmap_account_id: None,
                            imap_host: config.imap_host.trim(),
                            imap_port: config.imap_port,
                            smtp_host: config.smtp_host.trim(),
                            smtp_port: config.smtp_port,
                        },
                    )?;
                    repo::accounts::set_settings(&transaction, account_id, &config.settings)?;
                    repo::accounts::set_sync_state(&transaction, account_id, "needs_reauth")?;
                    imported += 1;
                }
                transaction.commit()?;
                Ok(imported)
            })
            .await
    }

    pub async fn reorder_account(&self, source_id: i64, target_id: i64, after: bool) -> Result<()> {
        self.db
            .write(move |conn| repo::accounts::reorder(conn, source_id, target_id, after))
            .await?;
        // Account order also determines the account/folder sidebar order.
        // An empty MailUpdated is the core's existing global-view invalidation
        // signal and lets every host refresh that structure immediately.
        self.bus.emit(CoreEvent::MailUpdated { thread_ids: vec![] });
        Ok(())
    }

    /// Change one account's background history download boundary. Provider
    /// checkpoints are reopened, but existing local messages are retained.
    pub async fn set_account_mail_history(
        &self,
        account_id: i64,
        mail_history: MailHistory,
    ) -> Result<()> {
        let updated = self
            .db
            .write(move |conn| {
                let transaction = conn.transaction()?;
                let mut config = repo::accounts::get_config(&transaction, account_id)?
                    .ok_or_else(|| CoreError::NotFound(format!("account {account_id}")))?;
                if config.settings.mail_history == mail_history {
                    transaction.commit()?;
                    return Ok(None);
                }

                config.settings.mail_history = mail_history;
                repo::accounts::set_settings(&transaction, account_id, &config.settings)?;
                match (config.provider, config.mail_protocol) {
                    (Provider::Gmail, _) => repo::gmail::expire_history(&transaction, account_id)?,
                    (_, MailProtocol::Jmap) => {
                        transaction.execute(
                            "UPDATE jmap_sync_state SET email_state = NULL WHERE account_id = ?1",
                            rusqlite::params![account_id],
                        )?;
                    }
                    (Provider::Imap | Provider::Microsoft, MailProtocol::Imap) => {
                        repo::folders::reopen_backfill_for_account(&transaction, account_id)?;
                    }
                }
                transaction.commit()?;
                Ok(Some(config.settings))
            })
            .await?;

        if let Some(settings) = updated {
            if let Some(handle) = self.handles.read().await.get(&account_id).cloned() {
                handle.update_settings(settings);
            }
            self.bus.emit(CoreEvent::MailUpdated { thread_ids: vec![] });
        }
        Ok(())
    }

    pub async fn test_connection(&self, args: &AddPasswordAccountArgs) -> ConnectionTestResult {
        if args.mail_protocol == MailProtocol::Jmap {
            return match crate::jmap::client::connect_with(
                &args.email,
                &args.username,
                &args.password,
                &args.jmap_url,
                None,
            )
            .await
            {
                Ok(_) => ConnectionTestResult {
                    ok: true,
                    error: None,
                },
                Err(error) => ConnectionTestResult {
                    ok: false,
                    error: Some(error.to_client_json()),
                },
            };
        }
        let creds = imap::ImapCredentials::Password {
            user: args.username.clone(),
            password: args.password.clone(),
        };
        match imap::connect(&args.imap_host, args.imap_port, creds).await {
            Ok(session) => {
                imap::logout(session).await;
                ConnectionTestResult {
                    ok: true,
                    error: None,
                }
            }
            Err(e) => ConnectionTestResult {
                ok: false,
                error: Some(e.to_client_json()),
            },
        }
    }

    pub async fn add_account_password(&self, args: AddPasswordAccountArgs) -> Result<Account> {
        let email = normalize_account_email(&args.email)
            .ok_or_else(|| CoreError::Auth("enter a valid email address".into()))?;
        let mut args = AddPasswordAccountArgs {
            email,
            display_name: args
                .display_name
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty()),
            username: args.username.trim().to_owned(),
            password: args.password,
            mail_protocol: args.mail_protocol,
            jmap_url: args.jmap_url.trim().to_owned(),
            imap_host: args.imap_host.trim().to_owned(),
            imap_port: args.imap_port,
            smtp_host: args.smtp_host.trim().to_owned(),
            smtp_port: args.smtp_port,
        };
        if args.username.is_empty() {
            args.username = args.email.clone();
        }
        if args.password.is_empty() {
            return Err(CoreError::Auth("email and password are required".into()));
        }
        if args.username.len() > 512
            || args.jmap_url.len() > 2048
            || args.imap_host.len() > 253
            || args.smtp_host.len() > 253
        {
            return Err(CoreError::Auth("an account field is too long".into()));
        }
        if args.mail_protocol == MailProtocol::Imap
            && (args.imap_host.is_empty()
                || args.smtp_host.is_empty()
                || args.imap_port == 0
                || args.smtp_port == 0)
        {
            return Err(CoreError::Auth(
                "IMAP and SMTP server hosts and ports are required".into(),
            ));
        }
        let args = if args.mail_protocol == MailProtocol::Jmap {
            AddPasswordAccountArgs {
                jmap_url: crate::jmap::client::normalize_base_url(&args.jmap_url, &args.email)?,
                ..args
            }
        } else {
            args
        };

        // Verify credentials before storing anything. For JMAP, also pin the
        // exact Mail account selected from the authenticated Session so a
        // later reconnect can never drift to a different shared account.
        let jmap_account_id = if args.mail_protocol == MailProtocol::Jmap {
            Some(
                crate::jmap::client::connect_with(
                    &args.email,
                    &args.username,
                    &args.password,
                    &args.jmap_url,
                    None,
                )
                .await?
                .account_id,
            )
        } else {
            let probe = self.test_connection(&args).await;
            if !probe.ok {
                return Err(CoreError::Auth(
                    probe.error.unwrap_or_else(|| "connection failed".into()),
                ));
            }
            None
        };

        let existing = self
            .db
            .read({
                let email = args.email.clone();
                move |conn| repo::accounts::find_by_email(conn, &email)
            })
            .await?;
        let is_new_account = existing.is_none();

        let id = if let Some(existing) = existing {
            if existing.provider != Provider::Imap || existing.auth_kind != AuthKind::Password {
                return Err(CoreError::Auth(format!(
                    "{} is already connected through {}; reconnect it through that provider",
                    existing.email,
                    existing.provider.as_str()
                )));
            }

            // Stop every worker before changing the persisted remote identity.
            // Otherwise a request already in flight against the old server
            // could commit its result after the reset transaction below.
            if let Some(handle) = self.handles.write().await.remove(&existing.id) {
                handle.abort();
            }

            let a = args.clone();
            let remote_account = jmap_account_id.clone();
            let id = existing.id;
            let update_result = self
                .db
                .write(move |conn| {
                    let transaction = conn.transaction()?;
                    repo::accounts::update_password(
                        &transaction,
                        id,
                        &repo::accounts::NewAccount {
                            email: &a.email,
                            display_name: a.display_name.as_deref(),
                            avatar_url: None,
                            provider: Provider::Imap,
                            auth_kind: AuthKind::Password,
                            mail_protocol: a.mail_protocol,
                            username: &a.username,
                            jmap_url: &a.jmap_url,
                            jmap_account_id: remote_account.as_deref(),
                            imap_host: &a.imap_host,
                            imap_port: a.imap_port,
                            smtp_host: &a.smtp_host,
                            smtp_port: a.smtp_port,
                        },
                    )?;
                    transaction.commit()?;
                    Ok(())
                })
                .await;
            if let Err(error) = update_result {
                if let Ok(Some(cfg)) = self
                    .db
                    .read(move |conn| repo::accounts::get_config(conn, id))
                    .await
                {
                    self.spawn_actor(cfg).await;
                }
                return Err(error);
            }

            id
        } else {
            let a = args.clone();
            let remote_account = jmap_account_id.clone();
            self.db
                .write(move |conn| {
                    repo::accounts::insert(
                        conn,
                        &repo::accounts::NewAccount {
                            email: &a.email,
                            display_name: a.display_name.as_deref(),
                            avatar_url: None,
                            provider: Provider::Imap,
                            auth_kind: AuthKind::Password,
                            mail_protocol: a.mail_protocol,
                            username: &a.username,
                            jmap_url: &a.jmap_url,
                            jmap_account_id: remote_account.as_deref(),
                            imap_host: &a.imap_host,
                            imap_port: a.imap_port,
                            smtp_host: &a.smtp_host,
                            smtp_port: a.smtp_port,
                        },
                    )
                })
                .await?
        };

        if let Err(error) = credentials::store_async(
            self.credentials.clone(),
            id,
            Slot::Password,
            args.password.clone(),
        )
        .await
        {
            // A newly inserted account without its password can never sync.
            // Roll it back atomically from the user's point of view.
            if is_new_account {
                let _ = self
                    .db
                    .write(move |conn| repo::accounts::delete(conn, id))
                    .await;
            } else if let Ok(Some(cfg)) = self
                .db
                .read(move |conn| repo::accounts::get_config(conn, id))
                .await
            {
                // Reconnect editing stops the previous workers before the
                // identity transaction. If keyring persistence then fails,
                // restore an actor so the account visibly enters its normal
                // authentication error state instead of remaining inert until
                // the next app restart.
                self.spawn_actor(cfg).await;
            }
            return Err(error);
        }

        let cfg = self
            .db
            .read(move |conn| repo::accounts::get_config(conn, id))
            .await?
            .ok_or_else(|| CoreError::NotFound("account".into()))?;
        self.spawn_actor(cfg).await;

        self.db
            .read(move |conn| repo::accounts::get(conn, id))
            .await?
            .ok_or_else(|| CoreError::NotFound("account".into()))
    }

    /// Abort any sign-in currently waiting on the browser redirect.
    pub fn cancel_oauth(&self) {
        tracing::info!("oauth: sign-in cancelled by user");
        self.oauth_cancel.notify_waiters();
    }

    /// The Slint host calls this once its startup show (the first-run intro) is
    /// out of the way - or immediately when there is no show. Releases the
    /// deferred account-actor spawn in [`Core::start`], which is what first
    /// touches the OS keyring. `notify_one` stores a permit, so the order of
    /// caller vs. waiter never matters.
    pub fn notify_ui_ready(&self) {
        self.ui_ready.notify_one();
    }

    pub async fn start_oauth(
        &self,
        provider: Provider,
        open_url: impl FnOnce(String) -> std::result::Result<(), String> + Send,
    ) -> Result<Account> {
        self.start_oauth_with_scopes(provider, &[], open_url).await
    }

    /// Connect mail and, when explicitly requested in account settings,
    /// include calendar consent in the same browser flow. Mail-only setup does
    /// not ask for Google Calendar or Microsoft Graph calendar permissions.
    pub async fn start_oauth_with_calendar<F>(
        &self,
        provider: Provider,
        connect_calendar: bool,
        open_url: F,
    ) -> Result<Account>
    where
        F: Fn(String) -> std::result::Result<(), String> + Clone + Send + 'static,
    {
        let consent_extra = oauth_calendar_scopes(provider, connect_calendar);
        let account = self
            .start_oauth_with_scopes(provider, consent_extra, open_url.clone())
            .await?;
        if connect_calendar {
            match provider {
                Provider::Gmail => {
                    self.connect_calendar(ConnectCalendarArgs {
                        account_id: account.id,
                        kind: "google".into(),
                        url: None,
                        username: None,
                        password: None,
                    })
                    .await?;
                }
                Provider::Microsoft => {
                    // The initial consent already included Calendars.ReadWrite;
                    // this retrieves its Graph-audience token without showing a
                    // second prompt, then discovers Outlook calendars.
                    self.connect_microsoft_calendar(account.id, open_url)
                        .await?;
                }
                Provider::Imap => {}
            }
        }
        Ok(account)
    }

    async fn start_oauth_with_scopes(
        &self,
        provider: Provider,
        consent_extra: &[&str],
        open_url: impl FnOnce(String) -> std::result::Result<(), String> + Send,
    ) -> Result<Account> {
        let outcome = tokio::select! {
            r = oauth::flow::authorize_with_broker(
                provider,
                consent_extra,
                None,
                self.oauth_redirects.clone(),
                open_url,
            ) => r?,
            _ = self.oauth_cancel.notified() => {
                return Err(CoreError::Auth("sign-in cancelled".into()));
            }
        };
        let servers = match provider {
            Provider::Gmail => &accounts::providers::GMAIL,
            Provider::Microsoft => &accounts::providers::MICROSOFT,
            Provider::Imap => return Err(CoreError::Auth("not an oauth provider".into())),
        };

        let email = outcome.email.clone();
        let existing = self
            .db
            .read({
                let email = email.clone();
                move |conn| repo::accounts::find_by_email(conn, &email)
            })
            .await?;
        if let Some(existing) = existing {
            if existing.provider != provider {
                return Err(CoreError::Auth(format!(
                    "{} is already connected through {}; use that provider to reconnect it",
                    existing.email,
                    existing.provider.as_str()
                )));
            }

            let id = existing.id;
            let display_name = outcome.display_name.clone();
            let avatar_url = outcome.avatar_url.clone();
            self.db
                .write(move |conn| {
                    repo::accounts::update_oauth_identity(
                        conn,
                        id,
                        display_name.as_deref(),
                        avatar_url.as_deref(),
                    )
                })
                .await?;
            self.tokens
                .store_initial(
                    id,
                    outcome.access_token,
                    outcome.expires_in,
                    outcome.refresh_token,
                    outcome.client_id,
                    outcome.client_secret,
                )
                .await?;
            self.db
                .write(move |conn| repo::accounts::set_sync_state(conn, id, "idle"))
                .await?;

            if let Some(handle) = self.handles.read().await.get(&id).cloned() {
                handle.send(SyncCmd::SyncNow { complete: None });
            } else {
                let cfg = self
                    .db
                    .read(move |conn| repo::accounts::get_config(conn, id))
                    .await?
                    .ok_or_else(|| CoreError::NotFound("account".into()))?;
                self.spawn_actor(cfg).await;
            }

            return self
                .db
                .read(move |conn| repo::accounts::get(conn, id))
                .await?
                .ok_or_else(|| CoreError::NotFound("account".into()));
        }

        let display_name = outcome.display_name.clone();
        let avatar_url = outcome.avatar_url.clone();
        let id = self
            .db
            .write(move |conn| {
                repo::accounts::insert(
                    conn,
                    &repo::accounts::NewAccount {
                        email: &email,
                        display_name: display_name.as_deref(),
                        avatar_url: avatar_url.as_deref(),
                        provider,
                        auth_kind: AuthKind::Oauth2,
                        mail_protocol: MailProtocol::Imap,
                        username: &email,
                        jmap_url: "",
                        jmap_account_id: None,
                        imap_host: servers.imap_host,
                        imap_port: servers.imap_port,
                        smtp_host: servers.smtp_host,
                        smtp_port: servers.smtp_port,
                    },
                )
            })
            .await?;

        self.tokens
            .store_initial(
                id,
                outcome.access_token,
                outcome.expires_in,
                outcome.refresh_token,
                outcome.client_id,
                outcome.client_secret,
            )
            .await?;

        let cfg = self
            .db
            .read(move |conn| repo::accounts::get_config(conn, id))
            .await?
            .ok_or_else(|| CoreError::NotFound("account".into()))?;
        self.spawn_actor(cfg).await;

        self.db
            .read(move |conn| repo::accounts::get(conn, id))
            .await?
            .ok_or_else(|| CoreError::NotFound("account".into()))
    }

    /// Re-run the OAuth consent for an existing account whose refresh token was
    /// revoked/expired (state `needs_reauth`) and swap in fresh tokens in place.
    /// Unlike `start_oauth` this never inserts a row: it updates the existing
    /// account's credentials and nudges its (paused) actor to reconnect.
    pub async fn reauth_account(
        &self,
        account_id: i64,
        open_url: impl FnOnce(String) -> std::result::Result<(), String> + Send,
    ) -> Result<Account> {
        let account = self
            .db
            .read(move |conn| repo::accounts::get(conn, account_id))
            .await?
            .ok_or_else(|| CoreError::NotFound("account".into()))?;
        // Only OAuth mailboxes reauth through the browser; password/IMAP
        // accounts recover by re-entering credentials in the account editor.
        let consent_extra: &[&str] = match account.provider {
            Provider::Microsoft => &[
                oauth::providers::MS_ONLINE_MEETINGS_SCOPE,
                oauth::providers::MS_CALENDARS_SCOPE,
            ],
            Provider::Gmail => &[],
            Provider::Imap => return Err(CoreError::Auth("not an oauth provider".into())),
        };
        let outcome = tokio::select! {
            // Hint the provider at the account being repaired so the browser
            // preselects it instead of silently reusing whatever session is
            // active (which then fails the email match below).
            r = oauth::flow::authorize_with_broker(
                account.provider,
                consent_extra,
                Some(&account.email),
                self.oauth_redirects.clone(),
                open_url,
            ) => r?,
            _ = self.oauth_cancel.notified() => {
                return Err(CoreError::Auth("sign-in cancelled".into()));
            }
        };
        // Refuse to graft another mailbox's tokens onto this account. Signing in
        // as a different address is "add account", not "reauth".
        if !outcome.email.eq_ignore_ascii_case(&account.email) {
            return Err(CoreError::Auth(format!(
                "signed in as {} but this account is {}",
                outcome.email, account.email
            )));
        }

        let display_name = outcome.display_name.clone();
        let avatar_url = outcome.avatar_url.clone();
        self.db
            .write(move |conn| {
                repo::accounts::update_oauth_identity(
                    conn,
                    account_id,
                    display_name.as_deref(),
                    avatar_url.as_deref(),
                )
            })
            .await?;

        self.tokens
            .store_initial(
                account_id,
                outcome.access_token,
                outcome.expires_in,
                outcome.refresh_token,
                outcome.client_id,
                outcome.client_secret,
            )
            .await?;

        // Clear needs_reauth and wake the actor. The reauth pause loop retries
        // its connect on the next SyncNow; if no actor is live (e.g. reauth
        // right after launch), spawn one so the fresh tokens take effect.
        self.db
            .write(move |conn| repo::accounts::set_sync_state(conn, account_id, "idle"))
            .await?;
        let has_handle = self.handles.read().await.contains_key(&account_id);
        if has_handle {
            if let Some(handle) = self.handles.read().await.get(&account_id) {
                handle.send(SyncCmd::SyncNow { complete: None });
            }
        } else {
            let cfg = self
                .db
                .read(move |conn| repo::accounts::get_config(conn, account_id))
                .await?
                .ok_or_else(|| CoreError::NotFound("account".into()))?;
            self.spawn_actor(cfg).await;
        }

        self.db
            .read(move |conn| repo::accounts::get(conn, account_id))
            .await?
            .ok_or_else(|| CoreError::NotFound("account".into()))
    }

    async fn purge_calendar_account(&self, account_id: i64) -> Result<()> {
        self.calendar_db
            .write(move |conn| {
                let tx = conn.transaction()?;
                tx.execute(
                    "DELETE FROM pending_actions WHERE account_id = ?1",
                    rusqlite::params![account_id],
                )?;
                tx.execute(
                    "DELETE FROM calendar_events WHERE account_id = ?1",
                    rusqlite::params![account_id],
                )?;
                tx.execute(
                    "DELETE FROM calendars WHERE account_id = ?1",
                    rusqlite::params![account_id],
                )?;
                tx.execute(
                    "DELETE FROM caldav_config WHERE account_id = ?1",
                    rusqlite::params![account_id],
                )?;
                tx.commit()?;
                Ok(())
            })
            .await
    }

    /// Recreate the old same-database `ON DELETE SET NULL` behavior for mail
    /// invite links now that calendar data has its own SQLite file. Calendar
    /// rows remain useful after their source message is deleted, so detach the
    /// optional reference rather than deleting the event.
    async fn detach_orphaned_calendar_message_links(&self) -> Result<usize> {
        let referenced_message_ids = self
            .calendar_db
            .read(|conn| {
                let mut statement = conn.prepare(
                    "SELECT DISTINCT message_id FROM calendar_events
                     WHERE message_id IS NOT NULL ORDER BY message_id",
                )?;
                Ok(statement
                    .query_map([], |row| row.get::<_, i64>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?)
            })
            .await?;
        if referenced_message_ids.is_empty() {
            return Ok(0);
        }

        // SQLite builds in the supported package set accept at least 999 bind
        // parameters. Stay well below that limit and reuse one mail-actor
        // roundtrip so even a large calendar does not block startup on N
        // cross-thread queries.
        let ids_for_lookup = referenced_message_ids.clone();
        let existing_message_ids = self
            .db
            .read(move |conn| {
                let mut existing = HashSet::new();
                for chunk in ids_for_lookup.chunks(500) {
                    let placeholders = std::iter::repeat_n("?", chunk.len())
                        .collect::<Vec<_>>()
                        .join(",");
                    let sql = format!("SELECT id FROM messages WHERE id IN ({placeholders})");
                    let mut statement = conn.prepare(&sql)?;
                    existing.extend(
                        statement
                            .query_map(rusqlite::params_from_iter(chunk), |row| {
                                row.get::<_, i64>(0)
                            })?
                            .collect::<rusqlite::Result<Vec<_>>>()?,
                    );
                }
                Ok(existing)
            })
            .await?;
        let orphaned = referenced_message_ids
            .into_iter()
            .filter(|message_id| !existing_message_ids.contains(message_id))
            .collect::<Vec<_>>();
        if orphaned.is_empty() {
            return Ok(0);
        }
        let detached = orphaned.len();
        self.calendar_db
            .write(move |conn| {
                let tx = conn.transaction()?;
                for message_id in orphaned {
                    tx.execute(
                        "UPDATE calendar_events SET message_id = NULL WHERE message_id = ?1",
                        rusqlite::params![message_id],
                    )?;
                }
                tx.commit()?;
                Ok(())
            })
            .await?;
        Ok(detached)
    }

    async fn complete_account_removal(&self, account_id: i64) -> Result<()> {
        if let Some(h) = self.handles.write().await.remove(&account_id) {
            h.abort();
        }
        self.cal_handles.write().await.remove(&account_id);
        self.tokens.forget_account(account_id).await;
        self.purge_calendar_account(account_id).await?;
        credentials::delete_all_async(self.credentials.clone(), account_id).await?;
        self.db
            .write(move |conn| repo::accounts::delete(conn, account_id))
            .await?;
        for directory in [
            self.paths.mail_dir(account_id),
            self.paths.attachments_dir(account_id),
        ] {
            match tokio::fs::remove_dir_all(directory).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(CoreError::Io(error)),
            }
        }
        self.db
            .write(move |conn| {
                conn.execute(
                    "DELETE FROM cross_store_operations
                     WHERE kind = 'remove_account' AND account_id = ?1",
                    rusqlite::params![account_id],
                )?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    async fn recover_cross_store_state(&self) -> Result<()> {
        let pending = self
            .db
            .read(|conn| {
                let mut statement = conn.prepare(
                    "SELECT account_id FROM cross_store_operations
                     WHERE kind = 'remove_account' ORDER BY created_at, account_id",
                )?;
                Ok(statement
                    .query_map([], |row| row.get::<_, i64>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?)
            })
            .await?;
        for account_id in pending {
            tracing::warn!(account_id, "finishing interrupted account removal");
            self.complete_account_removal(account_id).await?;
        }

        let valid_accounts = self
            .db
            .read(|conn| {
                let mut statement = conn.prepare("SELECT id FROM accounts")?;
                Ok(statement
                    .query_map([], |row| row.get::<_, i64>(0))?
                    .collect::<rusqlite::Result<HashSet<_>>>()?)
            })
            .await?;
        let calendar_accounts = self
            .calendar_db
            .read(|conn| {
                let mut statement = conn.prepare(
                    "SELECT account_id FROM calendar_events WHERE account_id != 0
                     UNION SELECT account_id FROM calendars WHERE account_id != 0
                     UNION SELECT account_id FROM caldav_config WHERE account_id != 0
                     UNION SELECT account_id FROM pending_actions WHERE account_id != 0",
                )?;
                Ok(statement
                    .query_map([], |row| row.get::<_, i64>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?)
            })
            .await?;
        for account_id in calendar_accounts {
            if !valid_accounts.contains(&account_id) {
                tracing::warn!(account_id, "removing orphaned calendar account data");
                self.purge_calendar_account(account_id).await?;
            }
        }
        let detached = self.detach_orphaned_calendar_message_links().await?;
        if detached > 0 {
            tracing::info!(detached, "detached orphaned calendar message links");
        }
        Ok(())
    }

    async fn cleanup_orphaned_draft_files(&self) -> Result<usize> {
        let (stale_paths, referenced_paths) = self
            .db
            .write(|conn| {
                let stale_paths = {
                    let mut statement = conn.prepare(
                        "SELECT da.file_path FROM draft_attachments da
                         JOIN messages m ON m.id = da.draft_id
                         WHERE m.is_draft = 0",
                    )?;
                    statement
                        .query_map([], |row| row.get::<_, String>(0))?
                        .collect::<rusqlite::Result<Vec<_>>>()?
                };
                conn.execute(
                    "DELETE FROM draft_attachments
                     WHERE draft_id IN (SELECT id FROM messages WHERE is_draft = 0)",
                    [],
                )?;
                let referenced_paths = {
                    let mut statement = conn.prepare("SELECT file_path FROM draft_attachments")?;
                    statement
                        .query_map([], |row| row.get::<_, String>(0))?
                        .collect::<rusqlite::Result<Vec<_>>>()?
                };
                Ok((stale_paths, referenced_paths))
            })
            .await?;

        let root = self.paths.draft_attachments_dir();
        let mut removed = 0usize;
        for path in stale_paths {
            if std::path::Path::new(&path).exists() {
                remove_staged_attachment(&root, &path).await;
                removed += 1;
            }
        }
        let Ok(canonical_root) = tokio::fs::canonicalize(&root).await else {
            return Ok(removed);
        };
        let mut referenced = HashSet::new();
        for path in referenced_paths {
            if let Ok(path) = tokio::fs::canonicalize(path).await
                && path.starts_with(&canonical_root)
            {
                referenced.insert(path);
            }
        }

        let mut directories = tokio::fs::read_dir(&canonical_root).await?;
        while let Some(directory) = directories.next_entry().await? {
            let file_type = directory.file_type().await?;
            if !file_type.is_dir() || file_type.is_symlink() {
                continue;
            }
            let mut entries = tokio::fs::read_dir(directory.path()).await?;
            while let Some(entry) = entries.next_entry().await? {
                let file_type = entry.file_type().await?;
                if !file_type.is_file() && !file_type.is_symlink() {
                    continue;
                }
                let path = entry.path();
                let is_referenced = tokio::fs::canonicalize(&path)
                    .await
                    .is_ok_and(|path| referenced.contains(&path));
                if !is_referenced && tokio::fs::remove_file(path).await.is_ok() {
                    removed += 1;
                }
            }
            let _ = tokio::fs::remove_dir(directory.path()).await;
        }
        Ok(removed)
    }

    pub async fn remove_account(&self, account_id: i64) -> Result<()> {
        let created_at = chrono::Utc::now().timestamp_millis();
        self.db
            .write(move |conn| {
                conn.execute(
                    "INSERT INTO cross_store_operations (kind, account_id, created_at)
                     VALUES ('remove_account', ?1, ?2)
                     ON CONFLICT(kind, account_id) DO NOTHING",
                    rusqlite::params![account_id, created_at],
                )?;
                Ok(())
            })
            .await?;
        self.complete_account_removal(account_id).await
    }

    /// Remove every user-owned record and secret while keeping the live
    /// database connections usable. This is the in-app equivalent of a fresh
    /// profile; it deliberately does not touch anything on provider servers.
    pub async fn delete_all_local_data(&self) -> Result<()> {
        let account_ids = self
            .db
            .read(|conn| {
                let mut statement = conn.prepare("SELECT id FROM accounts ORDER BY id")?;
                Ok(statement
                    .query_map([], |row| row.get::<_, i64>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?)
            })
            .await?;

        for account_id in account_ids {
            self.remove_account(account_id).await?;
        }

        // Account cascades remove mail/folders/actions. These tables also hold
        // profile-level data, so clear them explicitly for a genuine reset.
        self.db
            .write(|conn| {
                conn.execute_batch(
                    "DELETE FROM contacts;
                     DELETE FROM snippets;
                     DELETE FROM split_rules;
                     DELETE FROM route_cache;
                     DELETE FROM ai_usage_events;
                     DELETE FROM app_settings;",
                )?;
                Ok(())
            })
            .await?;
        self.calendar_db
            .write(|conn| {
                conn.execute_batch(
                    "DELETE FROM pending_actions;
                     DELETE FROM calendar_events;
                     DELETE FROM calendars;
                     DELETE FROM caldav_config;",
                )?;
                Ok(())
            })
            .await?;

        // Account id 0 owns app-level secrets such as the optional AI key.
        credentials::delete_all_async(self.credentials.clone(), 0).await?;
        let _ = tokio::fs::remove_dir_all(self.paths.draft_attachments_dir()).await;
        let _ = tokio::fs::remove_dir_all(self.paths.models_dir()).await;
        self.bus.emit(CoreEvent::MailUpdated { thread_ids: vec![] });
        self.bus.emit(CoreEvent::CalendarUpdated { account_id: 0 });
        Ok(())
    }

    /// Export coherent, integrity-checked copies of both SQLite stores plus a
    /// versioned manifest. Credentials remain in the platform keyring and are
    /// intentionally outside this database snapshot.
    pub async fn create_database_snapshot(
        &self,
        destination: std::path::PathBuf,
    ) -> Result<DatabaseSnapshotManifest> {
        let parent = destination.parent().ok_or_else(|| {
            CoreError::Other("database snapshot destination has no parent directory".into())
        })?;
        let canonical_parent = tokio::fs::canonicalize(parent).await.map_err(|error| {
            CoreError::Other(format!(
                "resolving snapshot destination {}: {error}",
                parent.display()
            ))
        })?;
        let data_dir = tokio::fs::canonicalize(&self.paths.data_dir).await?;
        let cache_dir = tokio::fs::canonicalize(&self.paths.cache_dir).await?;
        if canonical_parent.starts_with(&data_dir) || canonical_parent.starts_with(&cache_dir) {
            return Err(CoreError::Other(
                "database snapshots must be exported outside the application data and cache directories"
                    .into(),
            ));
        }
        db::snapshot::create(&self.db, &self.calendar_db, &destination).await
    }

    pub async fn sync_now(&self, account_id: Option<i64>) -> Result<()> {
        let receivers = {
            let handles = self.handles.read().await;
            match account_id {
                Some(id) => handles
                    .get(&id)
                    .map(|handle| vec![handle.sync_now()])
                    .ok_or_else(|| CoreError::NotFound(format!("account {id}")))?,
                None => handles.values().map(AccountHandle::sync_now).collect(),
            }
        };
        for receiver in receivers {
            let result = tokio::time::timeout(std::time::Duration::from_secs(45), receiver)
                .await
                .map_err(|_| CoreError::Other("Inbox sync timed out".into()))?
                .map_err(|_| CoreError::Other("sync actor stopped".into()))?;
            result.map_err(CoreError::Other)?;
        }
        Ok(())
    }

    pub async fn get_sync_status(&self) -> Result<Vec<SyncStatus>> {
        let accounts = self.db.read(|conn| repo::accounts::list(conn)).await?;
        let mut statuses = Vec::with_capacity(accounts.len());
        for account in accounts {
            statuses.push(sync::engine::status_for_account(&self.db, account.id).await?);
        }
        Ok(statuses)
    }

    pub async fn list_threads(
        &self,
        view: View,
        split_id: Option<i64>,
        account_id: Option<i64>,
        label_id: Option<i64>,
        folder_id: Option<i64>,
        page: impl Into<ThreadPageRequest>,
    ) -> Result<ThreadPage> {
        use repo::threads::TabFilter;
        let ThreadPageRequest { cursor, limit } = page.into();
        // A category label filters to its single resolved tab; a manual label is
        // a cross-cutting filter. Split conventions: -1 = Important, -2 = Other,
        // positive ids = custom split tabs.
        let tab = if let Some(lid) = label_id {
            let is_auto = self
                .db
                .read(move |conn| Ok(repo::labels::get(conn, lid)?.map(|l| l.is_auto)))
                .await?
                .unwrap_or(false);
            Some(if is_auto {
                TabFilter::AutoLabel(lid)
            } else {
                TabFilter::ManualLabel(lid)
            })
        } else {
            match split_id {
                Some(-1) => Some(TabFilter::Important),
                Some(-2) => Some(TabFilter::Other),
                Some(id) if id > 0 => Some(TabFilter::Split(id)),
                _ => None,
            }
        };
        self.db
            .read(move |conn| {
                repo::threads::list(
                    conn,
                    &repo::threads::ListArgs {
                        view,
                        tab,
                        account_id,
                        folder_id,
                        cursor,
                        limit: limit.clamp(1, 200),
                    },
                )
            })
            .await
    }

    /// Exact count for the standard mailbox/folder scopes used by native
    /// shells. This shares the list query's predicates but executes one scalar
    /// SQL query instead of fetching every summary through cursor pagination.
    pub async fn count_threads(
        &self,
        view: View,
        account_id: Option<i64>,
        folder_id: Option<i64>,
    ) -> Result<usize> {
        self.count_threads_filtered(view, None, account_id, None, folder_id)
            .await
    }

    /// Exact count for any native sidebar scope, including Important/Other
    /// and manual or automatic labels.
    pub async fn count_threads_filtered(
        &self,
        view: View,
        split_id: Option<i64>,
        account_id: Option<i64>,
        label_id: Option<i64>,
        folder_id: Option<i64>,
    ) -> Result<usize> {
        use repo::threads::TabFilter;
        let tab = if let Some(label_id) = label_id {
            let is_auto = self
                .db
                .read(move |conn| Ok(repo::labels::get(conn, label_id)?.map(|l| l.is_auto)))
                .await?
                .unwrap_or(false);
            Some(if is_auto {
                TabFilter::AutoLabel(label_id)
            } else {
                TabFilter::ManualLabel(label_id)
            })
        } else {
            match split_id {
                Some(-1) => Some(TabFilter::Important),
                Some(-2) => Some(TabFilter::Other),
                Some(id) if id > 0 => Some(TabFilter::Split(id)),
                _ => None,
            }
        };
        self.db
            .read(move |conn| {
                repo::threads::count(
                    conn,
                    &repo::threads::ListArgs {
                        view,
                        tab,
                        account_id,
                        folder_id,
                        cursor: None,
                        limit: 1,
                    },
                )
            })
            .await
    }

    /// Load only the message displayed by a single-message thread preview.
    /// Conversation consumers can still request the complete thread separately.
    pub async fn get_latest_thread_body(&self, thread_id: i64) -> Result<MessageDetail> {
        let id = self
            .db
            .read(move |conn| repo::messages::latest_in_thread(conn, thread_id))
            .await?
            .ok_or_else(|| CoreError::NotFound(format!("thread {thread_id}")))?;
        self.load_body(id, Some(thread_id)).await
    }

    pub async fn get_thread(&self, thread_id: i64) -> Result<ThreadDetail> {
        let t0 = std::time::Instant::now();
        let mut detail = self
            .db
            .read(move |conn| {
                let thread = repo::threads::get_summary(conn, thread_id)?
                    .ok_or_else(|| CoreError::NotFound(format!("thread {thread_id}")))?;
                let messages = repo::messages::list_for_thread(conn, thread_id)?;
                Ok(ThreadDetail { thread, messages })
            })
            .await?;
        let t_db = t0.elapsed();
        // Resolve embedded cid: images to data: URIs for the native HTML
        // renderer, which cannot fetch cid: URLs. Batched: one DB read and one
        // encode pass for the whole thread.
        let pairs: Vec<(i64, String)> = detail
            .messages
            .iter_mut()
            .filter_map(|m| m.html_body.take().map(|h| (m.id, h)))
            .collect();
        let mut resolved = self.inline_cid_images_batch(thread_id, pairs).await;
        for m in &mut detail.messages {
            if let Some(html) = resolved.remove(&m.id) {
                m.html_body = Some(html);
            }
        }
        let t_cid = t0.elapsed() - t_db;
        // Kick off priority fetches for unfetched bodies, newest first. The
        // conditional database claim in `request_body` deduplicates repeated
        // thread opens while the request is queued or in flight.
        for m in detail.messages.iter().rev() {
            if m.body_state == "none" {
                self.request_body(m.account_id, m.id).await;
            }
        }
        let html_bytes: usize = detail
            .messages
            .iter()
            .filter_map(|m| m.html_body.as_ref().map(String::len))
            .sum();
        tracing::debug!(
            "get_thread {thread_id}: {} msgs, {html_bytes}B html, db {t_db:?}, cid {t_cid:?}, total {:?}",
            detail.messages.len(),
            t0.elapsed()
        );
        Ok(detail)
    }

    async fn request_body(&self, account_id: i64, message_id: i64) {
        let claimed = self
            .db
            .write(move |conn| repo::messages::begin_body_fetch(conn, message_id))
            .await
            .unwrap_or(false);
        if !claimed {
            return;
        }
        let sent = self
            .handles
            .read()
            .await
            .get(&account_id)
            .is_some_and(|handle| handle.send(SyncCmd::FetchBody { message_id }));
        if !sent {
            let _ = self
                .db
                .write(move |conn| repo::messages::cancel_body_fetch(conn, message_id))
                .await;
        }
    }

    pub async fn get_body(&self, message_id: i64) -> Result<MessageDetail> {
        self.load_body(message_id, None).await
    }

    async fn load_body(
        &self,
        message_id: i64,
        preview_thread: Option<i64>,
    ) -> Result<MessageDetail> {
        let (mut detail, cached_raw_path) = self
            .db
            .read(move |conn| {
                let detail = repo::messages::detail(conn, message_id)?;
                let cached_raw_path = if detail.sender_verification.is_verified() {
                    None
                } else {
                    repo::messages::get_row(conn, message_id)?.and_then(|row| row.raw_path)
                };
                Ok((detail, cached_raw_path))
            })
            .await?;
        // Previously downloaded messages may already have raw headers on disk.
        // Recover receiver-stamped sender authentication lazily when opened
        // instead of forcing a mailbox-wide header re-download.
        if let Some(raw_path) = cached_raw_path
            && let Ok(raw) =
                crate::file_io::read_headers(raw_path, MAX_CACHED_HEADER_BYTES, "cached headers")
                    .await
            && let Ok(headers) = crate::mime::parse_header_block(&raw)
            && headers.sender_verification.is_verified()
        {
            let verification = headers.sender_verification;
            self.db
                .write(move |conn| {
                    repo::messages::set_sender_verification(conn, message_id, verification)
                })
                .await?;
            detail.sender_verification = verification;
        }
        if detail.body_state == "none" {
            self.request_body(detail.account_id, message_id).await;
        }
        if let Some(html) = detail.html_body.take() {
            detail.html_body = if let Some(thread_id) = preview_thread {
                // Render cached content immediately; missing inline attachments
                // arrive through MailUpdated instead of delaying text on network I/O.
                self.inline_cid_images_batch(thread_id, vec![(message_id, html)])
                    .await
                    .remove(&message_id)
            } else {
                Some(self.inline_cid_images(message_id, html).await)
            };
        }
        Ok(detail)
    }

    /// Newest message of `thread_id` that can be unsubscribed from, or None
    /// when the thread genuinely offers no list unsubscribe.
    ///
    /// The stored column alone is not enough to answer this: mail synced before
    /// List-Unsubscribe joined the IMAP header fetch has NULL there even when
    /// the cached raw message carries the header. So on a miss, re-read the
    /// raws we already have on disk (newest first) and persist the first header
    /// found, rather than telling the user there is no unsubscribe link.
    pub async fn thread_unsubscribe_message(&self, thread_id: i64) -> Result<Option<i64>> {
        if let Some(id) = self
            .db
            .read(move |conn| repo::messages::thread_unsubscribe_message(conn, thread_id))
            .await?
        {
            return Ok(Some(id));
        }
        let candidates = self
            .db
            .read(move |conn| repo::messages::thread_unsubscribe_candidates(conn, thread_id))
            .await?;
        for (id, raw_path) in candidates {
            let Ok(raw) =
                crate::file_io::read_headers(&raw_path, MAX_CACHED_HEADER_BYTES, "cached headers")
                    .await
            else {
                continue;
            };
            let Ok(headers) = crate::mime::parse_header_block(&raw) else {
                continue;
            };
            let Some(list_unsubscribe) = headers.list_unsubscribe else {
                continue;
            };
            let post = headers.list_unsubscribe_post;
            self.db
                .write(move |conn| {
                    repo::messages::set_list_unsubscribe(
                        conn,
                        id,
                        &list_unsubscribe,
                        post.as_deref(),
                    )
                })
                .await?;
            tracing::info!(
                message_id = id,
                thread_id,
                "recovered List-Unsubscribe from raw"
            );
            return Ok(Some(id));
        }
        Ok(None)
    }

    /// Unsubscribe from the list a message came from, preferring the RFC 8058
    /// one-click HTTPS POST, then the RFC 2369 mailto: (sent immediately over
    /// SMTP), and finally handing the URL back for the browser. The returned
    /// outcome states what actually happened so the UI can toast honestly.
    pub async fn unsubscribe_message(&self, message_id: i64) -> Result<UnsubscribeOutcome> {
        let detail = self
            .db
            .read(move |conn| repo::messages::detail(conn, message_id))
            .await?;
        let raw = detail
            .list_unsubscribe
            .ok_or_else(|| CoreError::NotFound("message has no List-Unsubscribe header".into()))?;
        let plan = unsubscribe::plan(&raw, detail.list_unsubscribe_post.as_deref())
            .ok_or_else(|| CoreError::Other("List-Unsubscribe header has no usable URI".into()))?;
        match plan {
            unsubscribe::UnsubscribePlan::OneClick { url } => {
                match unsubscribe::post_one_click(&url).await {
                    Ok(()) => {
                        tracing::info!(message_id, %url, "one-click unsubscribe accepted");
                        Ok(UnsubscribeOutcome::OneClick)
                    }
                    // The endpoint exists but didn't take the POST (blocked
                    // bots, expired token, outage): let the user finish there.
                    Err(e) => {
                        tracing::warn!(message_id, %url, error = %e, "one-click unsubscribe failed; deferring to browser");
                        Ok(UnsubscribeOutcome::NeedsBrowser { url })
                    }
                }
            }
            unsubscribe::UnsubscribePlan::Mailto { to, subject, body } => {
                self.send_unsubscribe_mail(detail.account_id, &to, &subject, &body)
                    .await?;
                tracing::info!(message_id, %to, "mailto unsubscribe sent");
                Ok(UnsubscribeOutcome::MailtoSent)
            }
            unsubscribe::UnsubscribePlan::Browser { url } => {
                Ok(UnsubscribeOutcome::NeedsBrowser { url })
            }
        }
    }

    /// Build and send the tiny unsubscribe-request message a mailto: List-
    /// Unsubscribe asks for. Sent directly (not through the pending-action
    /// queue): the user expects an immediate, definite answer, and there is no
    /// draft to reconcile.
    async fn send_unsubscribe_mail(
        &self,
        account_id: i64,
        to: &str,
        subject: &str,
        body: &str,
    ) -> Result<()> {
        let cfg = self
            .db
            .read(move |conn| repo::accounts::get_config(conn, account_id))
            .await?
            .ok_or_else(|| CoreError::NotFound(format!("account {account_id}")))?;
        let from = Address {
            name: cfg.display_name.clone(),
            email: cfg.email.clone(),
        };
        let domain = cfg
            .email
            .split('@')
            .nth(1)
            .unwrap_or("localhost")
            .to_string();
        let recipients = [Address {
            name: None,
            email: to.to_string(),
        }];
        let out = crate::mime::OutgoingMessage {
            from,
            to: &recipients,
            cc: &[],
            bcc: &[],
            subject,
            body_text: body,
            body_html: None,
            in_reply_to: None,
            references: &[],
            message_id: None,
            message_id_domain: &domain,
            attachments: Vec::new(),
        };
        let (_msg_id, raw) = crate::mime::build_message(&out)?;
        let auth = match cfg.auth_kind {
            AuthKind::Password => crate::smtp::SmtpAuth::Password(
                credentials::load_async(self.credentials.clone(), cfg.id, Slot::Password).await?,
            ),
            AuthKind::Oauth2 => crate::smtp::SmtpAuth::XOAuth2(
                self.tokens.access_token(cfg.id, cfg.provider).await?,
            ),
        };
        crate::smtp::send_raw(&cfg, &auth, &cfg.email, &[to.to_string()], &raw).await
    }

    /// Rewrite `src="cid:…"` references in a message body to `data:` URIs built
    /// from its inline attachments so embedded images render in the native
    /// HTML renderer. No-op when the body has no cid: references. On any
    /// failure the original HTML is returned unchanged.
    async fn inline_cid_images(&self, message_id: i64, html: String) -> String {
        if !html.contains("cid:") {
            return html;
        }
        let referenced = crate::mime::referenced_cids(&html);
        if referenced.is_empty() {
            return html;
        }
        let atts = self
            .db
            .read(move |conn| {
                let mut stmt = conn.prepare(
                    "SELECT id, content_id, mime_type, size, file_path FROM attachments
                     WHERE message_id = ?1 AND content_id IS NOT NULL
                       AND (part_id IS NOT NULL OR imap_section IS NOT NULL)",
                )?;
                let rows = stmt
                    .query_map(rusqlite::params![message_id], |r| {
                        Ok((
                            r.get::<_, i64>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, Option<String>>(2)?,
                            r.get::<_, Option<i64>>(3)?,
                            r.get::<_, Option<String>>(4)?,
                        ))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .await;
        let atts = match atts {
            Ok(a) if !a.is_empty() => a,
            _ => return html,
        };
        let mut fetched = Vec::new();
        let mut remaining = MAX_INLINE_CID_BYTES_PER_MESSAGE;
        for (attachment_id, content_id, mime, declared_size, file_path) in atts {
            if fetched.len() == MAX_INLINE_CID_IMAGES_PER_MESSAGE || remaining == 0 {
                break;
            }
            let normalized = crate::mime::normalize_cid(&content_id);
            if !referenced.contains(&normalized)
                || mime
                    .as_deref()
                    .is_some_and(|value| !value.starts_with("image/"))
            {
                continue;
            }
            let max_bytes = MAX_INLINE_CID_BYTES_PER_IMAGE.min(remaining);
            let declared_size = declared_size.and_then(|size| usize::try_from(size).ok());
            if declared_size.is_some_and(|size| size > max_bytes)
                || (file_path.is_none() && declared_size.is_none())
            {
                continue;
            }
            let Ok(path) = self.get_attachment(attachment_id).await else {
                continue;
            };
            let Ok(bytes) = crate::file_io::read(path, max_bytes, "inline image").await else {
                continue;
            };
            remaining -= bytes.len();
            fetched.push((content_id, mime, bytes));
        }
        if fetched.is_empty() {
            return html;
        }
        // Extraction + base64 of (possibly large) image parts is CPU-bound.
        let fallback = html.clone();
        tokio::task::spawn_blocking(move || {
            use base64::Engine;
            let mut map = std::collections::HashMap::new();
            for (content_id, mime, bytes) in fetched {
                let mime = mime.unwrap_or_else(|| "application/octet-stream".to_string());
                let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                map.insert(
                    crate::mime::normalize_cid(&content_id),
                    format!("data:{mime};base64,{b64}"),
                );
            }
            crate::mime::rewrite_cid_src(&html, &map)
        })
        .await
        .unwrap_or(fallback)
    }

    /// Batch variant of `inline_cid_images` for the thread-open path: one DB
    /// read and one encode pass for the whole thread. Only attachments already
    /// on disk are inlined; missing ones are downloaded in the background (a
    /// `MailUpdated` event refreshes the thread when they land) so opening a
    /// thread never waits on the network. Returns every input id mapped to its
    /// (possibly rewritten) HTML.
    async fn inline_cid_images_batch(
        &self,
        thread_id: i64,
        pairs: Vec<(i64, String)>,
    ) -> std::collections::HashMap<i64, String> {
        use std::collections::HashMap;
        let mut out: HashMap<i64, String> = HashMap::new();
        let mut need: Vec<(i64, String)> = Vec::new();
        let mut referenced: HashMap<i64, HashSet<String>> = HashMap::new();
        for (id, html) in pairs {
            if html.contains("cid:") {
                let cids = crate::mime::referenced_cids(&html);
                if cids.is_empty() {
                    out.insert(id, html);
                } else {
                    referenced.insert(id, cids);
                    need.push((id, html));
                }
            } else {
                out.insert(id, html);
            }
        }
        if need.is_empty() {
            return out;
        }

        let ids: Vec<i64> = need.iter().map(|(id, _)| *id).collect();
        let atts = self
            .db
            .read(move |conn| {
                let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
                let sql = format!(
                    "SELECT message_id, id, content_id, mime_type, file_path, size FROM attachments
                     WHERE message_id IN ({placeholders}) AND content_id IS NOT NULL
                       AND (part_id IS NOT NULL OR imap_section IS NOT NULL)"
                );
                let mut stmt = conn.prepare(&sql)?;
                let rows = stmt
                    .query_map(rusqlite::params_from_iter(ids.iter()), |r| {
                        Ok((
                            r.get::<_, i64>(0)?,
                            r.get::<_, i64>(1)?,
                            r.get::<_, String>(2)?,
                            r.get::<_, Option<String>>(3)?,
                            r.get::<_, Option<String>>(4)?,
                            r.get::<_, Option<i64>>(5)?,
                        ))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .await
            .unwrap_or_default();
        if atts.is_empty() {
            out.extend(need);
            return out;
        }

        let mut fetched: Vec<(i64, String, Option<String>, Vec<u8>)> = Vec::new();
        let mut missing: Vec<i64> = Vec::new();
        let mut thread_remaining = MAX_INLINE_CID_BYTES_PER_THREAD;
        let mut message_remaining: HashMap<i64, usize> = HashMap::new();
        let mut message_counts: HashMap<i64, usize> = HashMap::new();
        for (message_id, attachment_id, content_id, mime, file_path, declared_size) in atts {
            let normalized = crate::mime::normalize_cid(&content_id);
            if !referenced
                .get(&message_id)
                .is_some_and(|cids| cids.contains(&normalized))
                || mime
                    .as_deref()
                    .is_some_and(|value| !value.starts_with("image/"))
            {
                continue;
            }
            let count = message_counts.entry(message_id).or_default();
            if *count == MAX_INLINE_CID_IMAGES_PER_MESSAGE || thread_remaining == 0 {
                continue;
            }
            let message_remaining = message_remaining
                .entry(message_id)
                .or_insert(MAX_INLINE_CID_BYTES_PER_MESSAGE);
            let max_bytes = MAX_INLINE_CID_BYTES_PER_IMAGE
                .min(*message_remaining)
                .min(thread_remaining);
            let declared_size = declared_size.and_then(|size| usize::try_from(size).ok());
            if max_bytes == 0 || declared_size.is_some_and(|size| size > max_bytes) {
                continue;
            }
            let bytes = match file_path {
                Some(path) => crate::file_io::read(path, max_bytes, "inline image")
                    .await
                    .ok(),
                None => None,
            };
            match bytes {
                Some(bytes) => {
                    *count += 1;
                    *message_remaining -= bytes.len();
                    thread_remaining -= bytes.len();
                    fetched.push((message_id, content_id, mime, bytes));
                }
                None => {
                    let Some(size) = declared_size else { continue };
                    *count += 1;
                    *message_remaining -= size;
                    thread_remaining -= size;
                    missing.push(attachment_id);
                }
            }
        }
        if !missing.is_empty() {
            let core = self.clone();
            tokio::spawn(async move {
                let mut any = false;
                for id in missing {
                    any |= core.get_attachment(id).await.is_ok();
                }
                if any {
                    core.bus.emit(CoreEvent::MailUpdated {
                        thread_ids: vec![thread_id],
                    });
                }
            });
        }
        if fetched.is_empty() {
            out.extend(need);
            return out;
        }

        // Extraction + base64 of (possibly large) image parts is CPU-bound.
        let fallback = need.clone();
        let rewritten = tokio::task::spawn_blocking(move || {
            use base64::Engine;
            let mut maps: HashMap<i64, HashMap<String, String>> = HashMap::new();
            for (message_id, content_id, mime, bytes) in fetched {
                let mime = mime.unwrap_or_else(|| "application/octet-stream".to_string());
                let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                maps.entry(message_id).or_default().insert(
                    crate::mime::normalize_cid(&content_id),
                    format!("data:{mime};base64,{b64}"),
                );
            }
            need.into_iter()
                .map(|(id, html)| match maps.get(&id) {
                    Some(map) => {
                        let html = crate::mime::rewrite_cid_src(&html, map);
                        (id, html)
                    }
                    None => (id, html),
                })
                .collect::<Vec<_>>()
        })
        .await
        .unwrap_or(fallback);
        out.extend(rewritten);
        out
    }

    pub async fn list_folders(&self, account_id: Option<i64>) -> Result<Vec<FolderInfo>> {
        self.db
            .read(move |conn| repo::folders::list_info(conn, account_id))
            .await
    }

    pub async fn create_folder(
        &self,
        account_id: i64,
        parent_folder_id: Option<i64>,
        name: String,
    ) -> Result<()> {
        let name = validate_folder_leaf(&name)?;
        let config = self.folder_account_config(account_id).await?;
        let parent = match parent_folder_id {
            Some(folder_id) => Some(self.editable_folder(account_id, folder_id).await?),
            None => None,
        };
        if config.provider == Provider::Gmail {
            if name.contains('/') {
                return Err(CoreError::Other(
                    "folder names cannot contain the hierarchy separator \"/\"".into(),
                ));
            }
            let full_name = parent
                .as_ref()
                .map(|parent| format!("{}/{}", parent.imap_name, name))
                .unwrap_or(name);
            sync::gmail::create_user_folder(&self.sync_ctx(), &config, &full_name).await?;
        } else if config.mail_protocol == MailProtocol::Jmap {
            let secret =
                credentials::load_async(self.credentials.clone(), config.id, Slot::Password)
                    .await?;
            let connected = jmap::client::connect(&config, &secret).await?;
            let parent_remote = parent.as_ref().and_then(|folder| folder.jmap_id.clone());
            let mailbox = connected
                .client
                .mailbox_create(&name, parent_remote, jmap_client::mailbox::Role::None)
                .await
                .map_err(jmap::client::map_error)?;
            let remote_id = mailbox
                .id()
                .ok_or_else(|| CoreError::Jmap("Mailbox/set returned no id".into()))?
                .to_owned();
            let display_path = parent
                .as_ref()
                .map(|parent| format!("{} / {name}", parent.imap_name))
                .unwrap_or(name);
            self.db
                .write(move |conn| {
                    repo::folders::upsert_jmap(conn, account_id, &remote_id, &display_path, None)?;
                    Ok(())
                })
                .await?;
        } else {
            let delimiter = match parent.as_ref().and_then(|folder| folder.delimiter.clone()) {
                Some(delimiter) if !delimiter.is_empty() => delimiter,
                _ => {
                    self.db
                        .read(move |conn| {
                            Ok(repo::folders::list(conn, Some(account_id))?
                                .into_iter()
                                .find_map(|folder| {
                                    folder.delimiter.filter(|value| !value.is_empty())
                                })
                                .unwrap_or_else(|| "/".into()))
                        })
                        .await?
                }
            };
            if name.contains(&delimiter) {
                return Err(CoreError::Other(format!(
                    "folder names cannot contain the hierarchy separator {delimiter:?}"
                )));
            }
            let encoded_leaf = imap::encode_mailbox_name(&name);
            let remote_name = parent
                .as_ref()
                .map(|parent| format!("{}{delimiter}{encoded_leaf}", parent.imap_name))
                .unwrap_or(encoded_leaf);
            let mut session = self.connect_folder_imap(&config).await?;
            imap::create_folder(&mut session, &remote_name).await?;
            imap::logout(session).await;
            self.db
                .write(move |conn| {
                    repo::folders::upsert(conn, account_id, &remote_name, Some(&delimiter), None)?;
                    Ok(())
                })
                .await?;
        }
        self.bus.emit(CoreEvent::MailUpdated { thread_ids: vec![] });
        Ok(())
    }

    pub async fn rename_folder(&self, folder_id: i64, name: String) -> Result<()> {
        let name = validate_folder_leaf(&name)?;
        let folder = self
            .db
            .read(move |conn| repo::folders::get(conn, folder_id))
            .await?
            .ok_or_else(|| CoreError::NotFound(format!("folder {folder_id}")))?;
        if folder.role.is_some() {
            return Err(CoreError::Other("system folders cannot be renamed".into()));
        }
        let account_id = folder.account_id;
        let config = self.folder_account_config(account_id).await?;
        let delimiter = if config.mail_protocol == MailProtocol::Jmap {
            " / ".to_owned()
        } else {
            folder.delimiter.clone().unwrap_or_else(|| "/".into())
        };
        if name.contains(&delimiter) {
            return Err(CoreError::Other(format!(
                "folder names cannot contain the hierarchy separator {delimiter:?}"
            )));
        }
        let old_name = folder.imap_name.clone();
        let parent_prefix = old_name.rsplit_once(&delimiter).map(|(parent, _)| parent);
        if config.provider == Provider::Gmail {
            let full_name = parent_prefix
                .map(|parent| format!("{parent}{delimiter}{name}"))
                .unwrap_or(name);
            sync::gmail::rename_user_folder(&self.sync_ctx(), &config, folder_id, &full_name)
                .await?;
        } else if config.mail_protocol == MailProtocol::Jmap {
            let secret =
                credentials::load_async(self.credentials.clone(), config.id, Slot::Password)
                    .await?;
            let connected = jmap::client::connect(&config, &secret).await?;
            let remote_id = folder
                .jmap_id
                .as_deref()
                .ok_or_else(|| CoreError::NotFound(format!("remote folder {folder_id}")))?;
            connected
                .client
                .mailbox_rename(remote_id, &name)
                .await
                .map_err(jmap::client::map_error)?;
            let new_name = parent_prefix
                .map(|parent| format!("{parent}{delimiter}{name}"))
                .unwrap_or(name);
            self.db
                .write(move |conn| {
                    repo::folders::rename_tree(conn, account_id, &old_name, &new_name, &delimiter)
                })
                .await?;
        } else {
            let encoded = imap::encode_mailbox_name(&name);
            let new_name = parent_prefix
                .map(|parent| format!("{parent}{delimiter}{encoded}"))
                .unwrap_or(encoded);
            let mut session = self.connect_folder_imap(&config).await?;
            imap::rename_folder(&mut session, &old_name, &new_name).await?;
            imap::logout(session).await;
            self.db
                .write(move |conn| {
                    repo::folders::rename_tree(conn, account_id, &old_name, &new_name, &delimiter)
                })
                .await?;
        }
        self.bus.emit(CoreEvent::MailUpdated { thread_ids: vec![] });
        Ok(())
    }

    pub async fn delete_folder(&self, folder_id: i64) -> Result<()> {
        let folder = self
            .db
            .read(move |conn| repo::folders::get(conn, folder_id))
            .await?
            .ok_or_else(|| CoreError::NotFound(format!("folder {folder_id}")))?;
        if folder.role.is_some() {
            return Err(CoreError::Other("system folders cannot be deleted".into()));
        }
        let account_id = folder.account_id;
        let config = self.folder_account_config(account_id).await?;
        let delimiter = if config.mail_protocol == MailProtocol::Jmap {
            " / ".to_owned()
        } else {
            folder.delimiter.clone().unwrap_or_else(|| "/".into())
        };
        let prefix = format!("{}{delimiter}", folder.imap_name);
        let mut tree = self
            .db
            .read(move |conn| {
                Ok(repo::folders::list(conn, Some(account_id))?
                    .into_iter()
                    .filter(|candidate| {
                        candidate.id == folder_id || candidate.imap_name.starts_with(&prefix)
                    })
                    .collect::<Vec<_>>())
            })
            .await?;
        tree.sort_by_key(|candidate| std::cmp::Reverse(candidate.imap_name.len()));
        if config.provider == Provider::Gmail {
            for candidate in &tree {
                sync::gmail::delete_user_folder(&self.sync_ctx(), &config, candidate.id).await?;
            }
        } else if config.mail_protocol == MailProtocol::Jmap {
            let secret =
                credentials::load_async(self.credentials.clone(), config.id, Slot::Password)
                    .await?;
            let connected = jmap::client::connect(&config, &secret).await?;
            for candidate in &tree {
                let remote_id = candidate.jmap_id.as_deref().ok_or_else(|| {
                    CoreError::NotFound(format!("remote folder {}", candidate.id))
                })?;
                connected
                    .client
                    .mailbox_destroy(remote_id, true)
                    .await
                    .map_err(jmap::client::map_error)?;
            }
            let root_name = folder.imap_name;
            self.db
                .write(move |conn| {
                    repo::folders::delete_tree(conn, account_id, &root_name, &delimiter)
                })
                .await?;
        } else {
            let mut session = self.connect_folder_imap(&config).await?;
            for candidate in &tree {
                imap::delete_folder(&mut session, &candidate.imap_name).await?;
            }
            imap::logout(session).await;
            let root_name = folder.imap_name;
            self.db
                .write(move |conn| {
                    repo::folders::delete_tree(conn, account_id, &root_name, &delimiter)
                })
                .await?;
        }
        self.bus.emit(CoreEvent::MailUpdated { thread_ids: vec![] });
        Ok(())
    }

    async fn folder_account_config(&self, account_id: i64) -> Result<AccountConfig> {
        self.db
            .read(move |conn| repo::accounts::get_config(conn, account_id))
            .await?
            .ok_or_else(|| CoreError::NotFound(format!("account {account_id}")))
    }

    async fn editable_folder(
        &self,
        account_id: i64,
        folder_id: i64,
    ) -> Result<repo::folders::Folder> {
        let folder = self
            .db
            .read(move |conn| repo::folders::get(conn, folder_id))
            .await?
            .ok_or_else(|| CoreError::NotFound(format!("folder {folder_id}")))?;
        if folder.account_id != account_id {
            return Err(CoreError::Other(
                "parent folder belongs to another account".into(),
            ));
        }
        if folder.role.is_some() {
            return Err(CoreError::Other(
                "system folders cannot contain subfolders here".into(),
            ));
        }
        Ok(folder)
    }

    async fn connect_folder_imap(&self, config: &AccountConfig) -> Result<imap::Session> {
        let credentials = match config.auth_kind {
            AuthKind::Password => imap::ImapCredentials::Password {
                user: config.username.clone(),
                password: credentials::load_async(
                    self.credentials.clone(),
                    config.id,
                    Slot::Password,
                )
                .await?,
            },
            AuthKind::Oauth2 => imap::ImapCredentials::XOAuth2 {
                user: config.username.clone(),
                access_token: self.tokens.access_token(config.id, config.provider).await?,
            },
        };
        imap::connect(&config.imap_host, config.imap_port, credentials).await
    }

    pub async fn perform_action(&self, args: PerformActionArgs) -> Result<ActionResult> {
        let mut action_ids: Vec<i64> = Vec::new();
        let mut touched_accounts: Vec<i64> = Vec::new();

        for thread_id in args.thread_ids.clone() {
            let kind = args.kind;
            let params = args.params.clone();
            let ids = self
                .db
                .write(move |conn| apply_thread_action(conn, thread_id, kind, params.as_ref()))
                .await?;
            for (aid, account_id) in ids {
                action_ids.push(aid);
                if !touched_accounts.contains(&account_id) {
                    touched_accounts.push(account_id);
                }
            }
        }

        self.bus.emit(CoreEvent::MailUpdated {
            thread_ids: args.thread_ids.clone(),
        });
        for acc in touched_accounts {
            self.nudge(Some(acc), || SyncCmd::RunActions).await;
        }
        Ok(ActionResult { action_ids })
    }

    pub async fn undo_last(&self) -> Result<bool> {
        let cutoff = now_ms() - 30_000;
        let last = self
            .db
            .read(move |conn| repo::actions::last_undoable(conn, cutoff))
            .await?;
        let Some(last) = last else { return Ok(false) };

        // Undo the whole gesture: same kind, created within 150ms of it.
        let (kind, created) = (last.kind.clone(), last.created_at);
        let undone_threads = self
            .db
            .write(move |conn| {
                let mut stmt = conn.prepare(
                    "SELECT id FROM pending_actions
                     WHERE kind = ?1 AND ABS(created_at - ?2) <= 150
                       AND state IN ('pending','inflight','done')",
                )?;
                let ids = stmt
                    .query_map(rusqlite::params![kind, created], |r| r.get::<_, i64>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                drop(stmt);
                let mut threads = Vec::new();
                for id in ids {
                    if let Some(action) = repo::actions::get(conn, id)?
                        && let Some(tid) = revert_action(conn, &action)?
                        && !threads.contains(&tid)
                    {
                        threads.push(tid);
                    }
                }
                Ok(threads)
            })
            .await?;

        if !undone_threads.is_empty() {
            self.bus.emit(CoreEvent::MailUpdated {
                thread_ids: undone_threads,
            });
        }
        self.nudge(None, || SyncCmd::RunActions).await;
        Ok(true)
    }

    pub async fn cancel_send(&self, action_id: i64) -> Result<bool> {
        self.db
            .write(move |conn| repo::actions::try_cancel(conn, action_id))
            .await
    }

    /// "Send now": make a queued send due immediately (skip the remaining undo
    /// window) and nudge its actor. Returns false if it was already sent or
    /// cancelled.
    pub async fn send_now(&self, action_id: i64) -> Result<bool> {
        let account_id = self
            .db
            .write(move |conn| repo::actions::expedite(conn, action_id))
            .await?;
        match account_id {
            Some(account_id) => {
                self.nudge(Some(account_id), || SyncCmd::RunActions).await;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    pub async fn save_draft(&self, args: SaveDraftArgs) -> Result<i64> {
        // Stage every attachment into an app-managed dir up front, so the paths
        // persisted to `draft_attachments` (and later read at dispatch) are
        // always files the app itself copied - never an arbitrary path handed
        // in by the composer. Snapshotting here also fixes the sent bytes at
        // compose time rather than whatever the source file becomes later.
        if args.attachments.len() > MAX_DRAFT_ATTACHMENTS {
            return Err(CoreError::Other(format!(
                "a draft can contain at most {MAX_DRAFT_ATTACHMENTS} attachments"
            )));
        }
        let body_bytes = args
            .body_text
            .len()
            .saturating_add(args.body_html.as_ref().map_or(0, String::len));
        if body_bytes > MAX_DRAFT_BODY_BYTES {
            return Err(CoreError::Other(format!(
                "draft body exceeds the {} MiB safety limit",
                MAX_DRAFT_BODY_BYTES / (1024 * 1024)
            )));
        }
        let staging_root = self.paths.draft_attachments_dir();
        let reusable_paths = if let Some(draft_id) = args.draft_id {
            self.db
                .read(move |conn| {
                    let mut statement = conn
                        .prepare("SELECT file_path FROM draft_attachments WHERE draft_id = ?1")?;
                    Ok(statement
                        .query_map(rusqlite::params![draft_id], |row| row.get::<_, String>(0))?
                        .collect::<rusqlite::Result<Vec<_>>>()?)
                })
                .await?
        } else {
            Vec::new()
        };
        let mut reusable_canonical_paths = HashSet::new();
        for path in reusable_paths {
            if let Ok(path) = tokio::fs::canonicalize(path).await {
                reusable_canonical_paths.insert(path);
            }
        }
        let mut staged: Vec<crate::models::DraftAttachmentIn> =
            Vec::with_capacity(args.attachments.len());
        let mut created_paths: Vec<String> = Vec::new();
        let mut staged_bytes = 0usize;
        for att in &args.attachments {
            let remaining = MAX_DRAFT_ATTACHMENT_BYTES.saturating_sub(staged_bytes);
            let staged_attachment = stage_draft_attachment(
                &staging_root,
                &att.file_path,
                &att.filename,
                remaining,
                &reusable_canonical_paths,
            )
            .await;
            let (file_path, file_size, created) = match staged_attachment {
                Ok(value) => value,
                Err(error) => {
                    for path in &created_paths {
                        remove_staged_attachment(&staging_root, path).await;
                    }
                    return Err(error);
                }
            };
            if created {
                created_paths.push(file_path.clone());
            }
            staged_bytes += file_size;
            staged.push(crate::models::DraftAttachmentIn {
                file_path,
                filename: att.filename.clone(),
            });
        }
        let retained_paths: HashSet<String> = staged
            .iter()
            .map(|attachment| attachment.file_path.clone())
            .collect();
        let save_result = self
            .db
            .write(move |conn| {
                let tx = conn.transaction()?;

                let (provider, mail_protocol): (String, String) = tx.query_row(
                    "SELECT provider,mail_protocol FROM accounts WHERE id = ?1",
                    rusqlite::params![args.account_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?;
                // A user can compose immediately after OAuth, before the first
                // provider page has created folders. Give drafts a valid local
                // home up front; Gmail's names match the native label/synthetic
                // folders that label reconciliation will reuse.
                let drafts_folder = match repo::folders::by_role(
                    &tx,
                    args.account_id,
                    roles::DRAFTS,
                )? {
                    Some(folder) => folder.id,
                    None => repo::folders::upsert(
                        &tx,
                        args.account_id,
                        if provider == "gmail" { "DRAFT" } else { "Drafts" },
                        Some("/"),
                        Some(roles::DRAFTS),
                    )?,
                };
                let gmail_all_folder = if provider == "gmail" {
                    Some(match repo::folders::by_role(&tx, args.account_id, roles::ALL)? {
                        Some(folder) => folder.id,
                        None => repo::folders::upsert(
                            &tx,
                            args.account_id,
                            "[Gmail]/All Mail",
                            Some("/"),
                            Some(roles::ALL),
                        )?,
                    })
                } else {
                    None
                };

                // Thread: replies join the parent's thread.
                let thread_id = if let Some(parent_id) = args.in_reply_to_message_id {
                    repo::messages::get_row(&tx, parent_id)?.and_then(|r| r.thread_id)
                } else {
                    None
                };

                let draft_id = match args.draft_id {
                    Some(id) => {
                        let updated = tx.execute(
                            "UPDATE messages SET subject = ?2, to_json = ?3, cc_json = ?4,
                                    bcc_json = ?5, date = ?6 WHERE id = ?1 AND is_draft = 1",
                            rusqlite::params![
                                id,
                                args.subject,
                                serde_json::to_string(&args.to)?,
                                serde_json::to_string(&args.cc)?,
                                serde_json::to_string(&args.bcc)?,
                                now_ms(),
                            ],
                        )?;
                        if updated != 1 {
                            return Err(CoreError::NotFound("draft".into()));
                        }
                        id
                    }
                    None => {
                        let account_email: String = tx.query_row(
                            "SELECT email FROM accounts WHERE id = ?1",
                            rusqlite::params![args.account_id],
                            |r| r.get(0),
                        )?;
                        let tid = match thread_id {
                            Some(t) => t,
                            None => repo::threads::create(
                                &tx,
                                args.account_id,
                                None,
                                &crate::mime::normalize_subject(&args.subject),
                            )?,
                        };
                        let nm = repo::messages::NewMessage {
                            account_id: args.account_id,
                            folder_id: drafts_folder,
                            uid: None,
                            message_id: None,
                            gm_msgid: None,
                            gm_thrid: None,
                            subject: args.subject.clone(),
                            from: Some(Address {
                                name: None,
                                email: account_email,
                            }),
                            to: args.to.clone(),
                            cc: args.cc.clone(),
                            bcc: args.bcc.clone(),
                            date: now_ms(),
                            internal_date: None,
                            is_read: true,
                            is_starred: false,
                            is_draft: true,
                            is_outgoing: true,
                            is_automated: false,
                            has_attachments: false,
                            size: None,
                            snippet: crate::mime::make_snippet(&args.body_text),
                            references: Vec::new(),
                            list_unsubscribe: None,
                            list_unsubscribe_post: None,
                            sender_addr: None,
                            sender_verification: Default::default(),
                        };
                        let id = repo::messages::insert(&tx, &nm, tid)?;
                        tx.execute(
                            "INSERT INTO drafts_meta (message_id, mode, in_reply_to_message_id)
                             VALUES (?1, ?2, ?3)",
                            rusqlite::params![id, args.mode, args.in_reply_to_message_id],
                        )?;
                        id
                    }
                };

                tx.execute(
                    "INSERT INTO message_bodies (message_id, text_body, html_body) VALUES (?1, ?2, ?3)
                     ON CONFLICT(message_id) DO UPDATE SET text_body = excluded.text_body,
                                                           html_body = excluded.html_body",
                    rusqlite::params![draft_id, args.body_text, args.body_html],
                )?;
                tx.execute(
                    "UPDATE messages SET body_state = 'cached', snippet = ?2 WHERE id = ?1",
                    rusqlite::params![draft_id, crate::mime::make_snippet(&args.body_text)],
                )?;
                // Staged outgoing attachments: replace on every save.
                let old_paths = {
                    let mut statement = tx.prepare(
                        "SELECT file_path FROM draft_attachments WHERE draft_id = ?1",
                    )?;
                    statement
                        .query_map(rusqlite::params![draft_id], |row| row.get::<_, String>(0))?
                        .collect::<rusqlite::Result<Vec<_>>>()?
                };
                tx.execute(
                    "DELETE FROM draft_attachments WHERE draft_id = ?1",
                    rusqlite::params![draft_id],
                )?;
                for att in &staged {
                    tx.execute(
                        "INSERT INTO draft_attachments (draft_id, file_path, filename) VALUES (?1,?2,?3)",
                        rusqlite::params![draft_id, att.file_path, att.filename],
                    )?;
                }
                tx.execute(
                    "UPDATE messages SET has_attachments = ?2 WHERE id = ?1",
                    rusqlite::params![draft_id, (!staged.is_empty()) as i64],
                )?;
                if let Some(all_folder) = gmail_all_folder {
                    repo::gmail::set_message_folders(
                        &tx,
                        draft_id,
                        &[drafts_folder, all_folder],
                    )?;
                }
                let thread_id =
                    repo::messages::get_row(&tx, draft_id)?.and_then(|r| r.thread_id);
                if let Some(tid) = thread_id {
                    repo::threads::recompute(&tx, tid)?;
                }
                repo::search::index_message(&tx, draft_id)?;
                let sync_remote_account = if provider == "gmail" || mail_protocol == "jmap" {
                    tx.execute(
                        "UPDATE pending_actions SET state='cancelled',finished_at=?2
                         WHERE message_id=?1 AND kind='save_draft' AND state='pending'",
                        rusqlite::params![draft_id, now_ms()],
                    )?;
                    repo::actions::enqueue(
                        &tx,
                        args.account_id,
                        "save_draft",
                        Some(draft_id),
                        thread_id,
                        &serde_json::json!({ "draftId": draft_id }),
                        None,
                    )?;
                    Some(args.account_id)
                } else {
                    None
                };
                tx.commit()?;
                Ok((draft_id, thread_id, sync_remote_account, old_paths))
            })
            .await;
        let (draft_id, thread_id, sync_remote_account, old_paths) = match save_result {
            Ok(value) => value,
            Err(error) => {
                for path in &created_paths {
                    remove_staged_attachment(&staging_root, path).await;
                }
                return Err(error);
            }
        };
        for path in old_paths {
            if !retained_paths.contains(&path) {
                remove_staged_attachment(&staging_root, &path).await;
            }
        }
        if let Some(thread_id) = thread_id {
            self.bus.emit(CoreEvent::MailUpdated {
                thread_ids: vec![thread_id],
            });
        }
        if let Some(account_id) = sync_remote_account {
            self.nudge(Some(account_id), || SyncCmd::RunActions).await;
        }
        Ok(draft_id)
    }

    /// Load the complete editable state for a saved draft. Message detail omits
    /// Bcc and app-managed attachment paths, so reopening from the rendered
    /// message alone would silently lose both on the next save.
    pub async fn get_draft(&self, draft_id: i64) -> Result<SaveDraftArgs> {
        let (mut draft, remote_attachments) = self
            .db
            .read(move |conn| {
                let mut draft = conn
                    .query_row(
                        "SELECT m.account_id, m.to_json, m.cc_json, m.bcc_json, m.subject,
                                COALESCE(b.text_body, ''), b.html_body,
                                COALESCE(dm.mode, 'new'), dm.in_reply_to_message_id,
                                m.is_draft
                         FROM messages m
                         LEFT JOIN message_bodies b ON b.message_id = m.id
                         LEFT JOIN drafts_meta dm ON dm.message_id = m.id
                         WHERE m.id = ?1",
                        rusqlite::params![draft_id],
                        |row| {
                            let is_draft = row.get::<_, i64>(9)? != 0;
                            Ok((
                                SaveDraftArgs {
                                    draft_id: Some(draft_id),
                                    account_id: row.get(0)?,
                                    to: repo::parse_json_column(&row.get::<_, String>(1)?, 1)?,
                                    cc: repo::parse_json_column(&row.get::<_, String>(2)?, 2)?,
                                    bcc: repo::parse_json_column(&row.get::<_, String>(3)?, 3)?,
                                    subject: row.get(4)?,
                                    body_text: row.get(5)?,
                                    body_html: row.get(6)?,
                                    mode: row.get(7)?,
                                    in_reply_to_message_id: row.get(8)?,
                                    attachments: Vec::new(),
                                },
                                is_draft,
                            ))
                        },
                    )
                    .optional()?
                    .ok_or_else(|| CoreError::NotFound("draft".into()))?;
                if !draft.1 {
                    return Err(CoreError::NotFound("draft".into()));
                }

                let mut stmt = conn.prepare(
                    "SELECT file_path, filename
                     FROM draft_attachments WHERE draft_id = ?1 ORDER BY id",
                )?;
                draft.0.attachments = stmt
                    .query_map(rusqlite::params![draft_id], |row| {
                        Ok(crate::models::DraftAttachmentIn {
                            file_path: row.get(0)?,
                            filename: row.get(1)?,
                        })
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                let remote_attachments = if draft.0.attachments.is_empty() {
                    let mut stmt = conn.prepare(
                        "SELECT id, COALESCE(filename, '')
                         FROM attachments
                         WHERE message_id = ?1 AND is_inline = 0
                         ORDER BY id",
                    )?;
                    stmt.query_map(rusqlite::params![draft_id], |row| {
                        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?
                } else {
                    Vec::new()
                };
                Ok((draft.0, remote_attachments))
            })
            .await?;

        // Synced provider drafts have attachment descriptors, not composer
        // staging rows. Materialize those parts through the account's priority
        // reader so the composer shows them and the next save snapshots the
        // exact bytes into its managed staging directory.
        for (attachment_id, filename) in remote_attachments {
            let file_path = self.get_attachment(attachment_id).await?;
            draft.attachments.push(crate::models::DraftAttachmentIn {
                file_path,
                filename: if filename.is_empty() {
                    format!("attachment-{attachment_id}")
                } else {
                    crate::mime::decode_encoded_words(&filename)
                },
            });
        }
        Ok(draft)
    }

    pub async fn delete_draft(&self, draft_id: i64) -> Result<()> {
        let (thread_id, nudge_account, staged_paths) = self
            .db
            .write(move |conn| {
                let row = conn
                    .query_row(
                        "SELECT m.account_id, m.thread_id, m.folder_id, m.uid, m.is_draft,
                                COALESCE(f.role, ''), a.provider,
                                m.gm_msgid, m.gmail_draft_id, a.mail_protocol, m.jmap_id
                         FROM messages m LEFT JOIN folders f ON f.id = m.folder_id
                         JOIN accounts a ON a.id = m.account_id
                         WHERE m.id = ?1",
                        rusqlite::params![draft_id],
                        |row| {
                            Ok((
                                row.get::<_, i64>(0)?,
                                row.get::<_, Option<i64>>(1)?,
                                row.get::<_, Option<i64>>(2)?,
                                row.get::<_, Option<i64>>(3)?,
                                row.get::<_, i64>(4)? != 0,
                                row.get::<_, String>(5)?,
                                row.get::<_, String>(6)?,
                                row.get::<_, Option<String>>(7)?,
                                row.get::<_, Option<String>>(8)?,
                                row.get::<_, String>(9)?,
                                row.get::<_, Option<String>>(10)?,
                            ))
                        },
                    )
                    .optional()?
                    .ok_or_else(|| CoreError::NotFound("draft".into()))?;
                if !row.4 {
                    return Err(CoreError::NotFound("draft".into()));
                }

                let send_inflight = conn.query_row(
                    "SELECT EXISTS(
                       SELECT 1 FROM pending_actions
                       WHERE message_id=?1 AND kind='send' AND state='inflight'
                     )",
                    rusqlite::params![draft_id],
                    |query_row| query_row.get::<_, bool>(0),
                )?;
                if send_inflight {
                    return Err(CoreError::Other(
                        "the draft is currently being submitted and cannot be deleted".into(),
                    ));
                }
                conn.execute(
                    "UPDATE pending_actions SET state='cancelled',finished_at=?2
                     WHERE message_id=?1 AND kind='send' AND state='pending'",
                    rusqlite::params![draft_id, now_ms()],
                )?;

                let mut nudge_account = None;
                let mut staged_paths = Vec::new();
                if row.9 == "jmap" {
                    conn.execute(
                        "UPDATE pending_actions SET state='cancelled',finished_at=?2
                         WHERE message_id=?1 AND kind='save_draft'
                           AND state IN ('pending','inflight')",
                        rusqlite::params![draft_id, now_ms()],
                    )?;
                    if let Some(remote_id) = row.10.as_deref() {
                        repo::actions::enqueue(
                            conn,
                            row.0,
                            "delete_draft",
                            None,
                            row.1,
                            &serde_json::json!({ "jmapEmailId": remote_id }),
                            None,
                        )?;
                        nudge_account = Some(row.0);
                    }
                    staged_paths = repo::messages::take_draft_attachment_paths(conn, draft_id)?;
                    repo::messages::delete(conn, draft_id)?;
                } else if row.6 == "gmail" {
                    // Cancel any queued autosaves before deleting the local
                    // row. If a remote resource already exists, retain its
                    // opaque ids in a message-independent delete action.
                    conn.execute(
                        "UPDATE pending_actions SET state = 'cancelled', finished_at = ?2
                         WHERE message_id = ?1 AND kind = 'save_draft'
                           AND state = 'pending'",
                        rusqlite::params![draft_id, now_ms()],
                    )?;
                    if row.7.is_some() || row.8.is_some() {
                        repo::actions::enqueue(
                            conn,
                            row.0,
                            "delete_draft",
                            None,
                            row.1,
                            &serde_json::json!({
                                "gmailMessageId": row.7,
                                "gmailDraftId": row.8,
                            }),
                            None,
                        )?;
                        nudge_account = Some(row.0);
                    }
                    staged_paths = repo::messages::take_draft_attachment_paths(conn, draft_id)?;
                    repo::messages::delete(conn, draft_id)?;
                } else if row.3.is_some() && row.5 != roles::TRASH {
                    // This draft exists on IMAP. Move it optimistically and
                    // queue the matching remote move so the next sync cannot
                    // resurrect the local row in Drafts.
                    let trash = repo::folders::by_role(conn, row.0, roles::TRASH)?
                        .ok_or_else(|| CoreError::NotFound("no trash folder".into()))?;
                    let payload = serde_json::json!({
                        "srcFolderId": row.2,
                        "srcUid": row.3,
                        "targetFolderId": trash.id,
                    });
                    repo::messages::set_uid_and_folder(conn, draft_id, trash.id, None)?;
                    repo::actions::enqueue(
                        conn,
                        row.0,
                        "trash",
                        Some(draft_id),
                        row.1,
                        &payload,
                        None,
                    )?;
                    nudge_account = Some(row.0);
                } else {
                    // Purely local drafts have no remote copy to preserve.
                    staged_paths = repo::messages::take_draft_attachment_paths(conn, draft_id)?;
                    repo::messages::delete(conn, draft_id)?;
                }
                if let Some(tid) = row.1 {
                    repo::threads::recompute(conn, tid)?;
                }
                Ok((row.1, nudge_account, staged_paths))
            })
            .await?;
        for path in staged_paths {
            remove_staged_attachment(&self.paths.draft_attachments_dir(), &path).await;
        }
        if let Some(thread_id) = thread_id {
            self.bus.emit(CoreEvent::MailUpdated {
                thread_ids: vec![thread_id],
            });
        }
        if let Some(account_id) = nudge_account {
            self.nudge(Some(account_id), || SyncCmd::RunActions).await;
        }
        Ok(())
    }

    pub async fn queue_send(&self, args: QueueSendArgs) -> Result<QueueSendResult> {
        let settings = self.db.read(|conn| repo::settings::get(conn)).await?;
        let dispatch_at = args
            .send_at
            .unwrap_or_else(|| now_ms() + settings.undo_send_seconds * 1000);

        let draft_id = args.draft_id;
        let (action_id, account_id) = self
            .db
            .write(move |conn| {
                let row = repo::messages::get_row(conn, draft_id)?
                    .ok_or_else(|| CoreError::NotFound("draft".into()))?;
                conn.execute(
                    "UPDATE pending_actions SET state = 'cancelled', finished_at = ?2
                     WHERE message_id = ?1 AND kind = 'save_draft' AND state = 'pending'",
                    rusqlite::params![draft_id, now_ms()],
                )?;
                let payload = serde_json::json!({ "draftId": draft_id });
                let aid = repo::actions::enqueue(
                    conn,
                    row.account_id,
                    "send",
                    Some(draft_id),
                    row.thread_id,
                    &payload,
                    Some(dispatch_at),
                )?;
                Ok((aid, row.account_id))
            })
            .await?;

        // Fire exactly when the send comes due instead of waiting for the
        // scheduler's next tick (up to TICK_SECS of extra slop, which made even
        // an immediate send feel sluggish). The scheduler still covers restarts
        // and any timer this task misses. Only armed for near-term sends; far
        // future "send later" relies on the scheduler so we don't hold a task
        // sleeping for hours.
        let delay_ms = (dispatch_at - now_ms()).max(0);
        if delay_ms <= 15 * 60 * 1000 {
            let core = self.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms as u64)).await;
                core.nudge(Some(account_id), || SyncCmd::RunActions).await;
            });
        }
        Ok(QueueSendResult {
            action_id,
            dispatch_at,
        })
    }

    /// Return a stable cached attachment path. Legacy messages extract from
    /// their raw MIME; selectively-cached messages download only the requested
    /// IMAP section through the priority reader.
    pub async fn get_attachment(&self, attachment_id: i64) -> Result<String> {
        let fetch_lock = {
            let mut locks = self.attachment_locks.lock().await;
            match locks.get(&attachment_id).and_then(std::sync::Weak::upgrade) {
                Some(lock) => lock,
                None => {
                    if locks.len() >= 64 {
                        locks.retain(|_, lock| lock.strong_count() > 0);
                    }
                    let lock = Arc::new(tokio::sync::Mutex::new(()));
                    locks.insert(attachment_id, Arc::downgrade(&lock));
                    lock
                }
            }
        };
        let _fetch_guard = fetch_lock.lock().await;

        // This check deliberately happens after acquiring the single-flight
        // lock: a concurrent caller may just have populated `file_path`.
        let (message_id, part_id, imap_section, filename, mime_type, file_path) = self
            .db
            .read(move |conn| {
                conn.query_row(
                    "SELECT message_id, part_id, imap_section, filename, mime_type, file_path
                     FROM attachments WHERE id = ?1",
                    rusqlite::params![attachment_id],
                    |r| {
                        Ok((
                            r.get::<_, i64>(0)?,
                            r.get::<_, Option<String>>(1)?,
                            r.get::<_, Option<String>>(2)?,
                            r.get::<_, Option<String>>(3)?,
                            r.get::<_, Option<String>>(4)?,
                            r.get::<_, Option<String>>(5)?,
                        ))
                    },
                )
                .map_err(Into::into)
            })
            .await?;

        if let Some(path) = file_path
            && tokio::fs::metadata(&path).await.is_ok()
        {
            return Ok(path);
        }

        let row = self
            .db
            .read(move |conn| repo::messages::get_row(conn, message_id))
            .await?
            .ok_or_else(|| CoreError::NotFound("message".into()))?;

        // Prefer the legacy raw cache when it is healthy. If its file was
        // removed/corrupted but this row also has an IMAP section, safely fall
        // through to the remote section instead of making the attachment
        // permanently inaccessible because of a stale `raw_path`.
        let legacy = match (row.raw_path.as_ref(), part_id.as_deref()) {
            (Some(raw_path), Some(part_id)) => {
                match crate::file_io::read(raw_path, MAX_CACHED_MESSAGE_BYTES, "cached message")
                    .await
                {
                    Ok(raw) => match crate::mime::extract_attachment(&raw, part_id) {
                        Ok(value) => Some(value),
                        Err(error) if imap_section.is_none() => return Err(error),
                        Err(_) => None,
                    },
                    Err(error) if imap_section.is_none() => return Err(error),
                    Err(_) => None,
                }
            }
            (Some(_), None) if imap_section.is_none() => {
                return Err(CoreError::NotFound("attachment part".into()));
            }
            _ => None,
        };
        let (bytes, parsed_name) = if let Some(value) = legacy {
            value
        } else {
            let handle = self
                .handles
                .read()
                .await
                .get(&row.account_id)
                .cloned()
                .ok_or_else(|| CoreError::NotFound(format!("account {}", row.account_id)))?;
            (handle.fetch_attachment(attachment_id).await?, None)
        };

        let mut safe_name = safe_filename(
            &filename
                .map(|name| crate::mime::decode_encoded_words(&name))
                .or(parsed_name)
                .unwrap_or_else(|| format!("attachment-{attachment_id}")),
        );
        // Invites and other body parts often carry no filename; give the temp
        // file an extension from its MIME type so opening it hands off to the
        // right app (a `text/calendar` part becomes `*.ics`, not a bare name
        // the OS opens in a text editor or refuses to open at all).
        if std::path::Path::new(&safe_name).extension().is_none()
            && let Some(ext) = mime_type.as_deref().and_then(ext_for_mime)
        {
            safe_name.push('.');
            safe_name.push_str(ext);
        }
        let dir = self
            .paths
            .attachments_dir(row.account_id)
            .join(attachment_id.to_string());
        tokio::fs::create_dir_all(&dir).await?;
        let path = dir.join(&safe_name);
        crate::file_io::write_atomic(&path, &bytes, "attachment cache").await?;

        let path_str = path.to_string_lossy().to_string();
        let p = path_str.clone();
        self.db
            .write(move |conn| {
                conn.execute(
                    "UPDATE attachments SET file_path = ?2 WHERE id = ?1",
                    rusqlite::params![attachment_id, p],
                )?;
                Ok(())
            })
            .await?;
        Ok(path_str)
    }

    /// Extract an attachment and write it to a caller-chosen destination (the
    /// "download" / save-as path). Reuses `get_attachment` so extraction and
    /// filename handling stay in one place.
    pub async fn save_attachment(&self, attachment_id: i64, dest: String) -> Result<()> {
        let src = self.get_attachment(attachment_id).await?;
        tokio::fs::copy(&src, &dest).await?;
        Ok(())
    }

    /// Compose "To" autocomplete. `account_id` Some scopes suggestions to the
    /// sending account's own contacts; None returns contacts across all accounts
    /// (the "show contacts from all accounts" setting).
    pub async fn list_contacts(
        &self,
        prefix: String,
        account_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<Address>> {
        self.db
            .read(move |conn| {
                repo::contacts::autocomplete(conn, &prefix, account_id, limit.clamp(1, 50))
            })
            .await
    }

    /// Contact suggestions for the search screen: every query token must match
    /// (accent-insensitive), ranked by how much the user emails that contact.
    /// Search operators are stripped first so "from:x be" still suggests on "be".
    pub async fn suggest_contacts(
        &self,
        query: String,
        limit: i64,
    ) -> Result<Vec<ContactSuggestion>> {
        let text = search::parse(&query).text;
        self.db
            .read(move |conn| repo::contacts::suggest(conn, &text, None, limit.clamp(1, 20)))
            .await
    }

    /// Full records for the contacts workspace (including an empty-query list).
    pub async fn list_contact_records(
        &self,
        query: String,
        limit: i64,
    ) -> Result<Vec<ContactRecord>> {
        self.db
            .read(move |conn| repo::contacts::list_records(conn, &query, limit))
            .await
    }

    /// One strict page for the dedicated contacts workspace. Filtering lives
    /// in SQLite so infinite scrolling never needs to preload the directory.
    pub async fn list_contact_record_page(
        &self,
        query: String,
        account_id: Option<i64>,
        favorites_only: bool,
        cursor: Option<ContactRecordCursor>,
        limit: i64,
    ) -> Result<ContactRecordPage> {
        self.db
            .read(move |conn| {
                repo::contacts::list_record_page(
                    conn,
                    &query,
                    account_id,
                    favorites_only,
                    cursor.as_ref(),
                    limit.clamp(1, 100),
                )
            })
            .await
    }

    pub async fn save_contact(&self, record: ContactRecord) -> Result<ContactRecord> {
        let now_ms = chrono::Utc::now().timestamp_millis();
        self.db
            .write(move |conn| repo::contacts::save_record(conn, &record, now_ms))
            .await
    }

    pub async fn delete_contact(&self, id: i64) -> Result<()> {
        self.db
            .write(move |conn| repo::contacts::delete_record(conn, id))
            .await
    }

    pub async fn list_events(&self, start_ms: i64, end_ms: i64) -> Result<Vec<CalendarEvent>> {
        let (mut events, masters) = self
            .calendar_db
            .read(move |conn| {
                Ok((
                    repo::calendar::list_range(conn, start_ms, end_ms)?,
                    repo::calendar::recurring_masters(conn, end_ms)?,
                ))
            })
            .await?;

        // Expand recurring series into concrete occurrences. Occurrences keep
        // the master's row id (edits/deletes address the whole series in v1);
        // unsupported rules fall back to the master row alone.
        for m in masters {
            let Some(rrule) = m.event.rrule.clone() else {
                continue;
            };
            let duration = m
                .event
                .ends_at
                .map(|e| e - m.event.starts_at)
                .unwrap_or(1_800_000);
            let Some(occs) = caldav::rrule::expand(
                &rrule,
                m.event.starts_at,
                duration,
                m.event.all_day,
                m.ical_raw.as_deref(),
                start_ms,
                end_ms,
            ) else {
                continue; // unsupported rule: the master entry stands alone
            };
            events.retain(|e| e.id != m.event.id);
            for occ in occs {
                let mut e = m.event.clone();
                e.starts_at = occ.start;
                e.ends_at = Some(occ.end);
                events.push(e);
            }
        }
        events.sort_by_key(|e| e.starts_at);
        Ok(events)
    }

    /// Invite events carried by one message (the thread invite card).
    pub async fn events_for_message(&self, message_id: i64) -> Result<Vec<CalendarEvent>> {
        self.calendar_db
            .read(move |conn| repo::calendar::for_message(conn, message_id))
            .await
    }

    /// Create a meeting. The event lands on the local calendar immediately;
    /// if it has attendees, an invite email with an ICS (METHOD:REQUEST) is
    /// drafted and queued through the normal send pipeline (undo window,
    /// offline queueing, sent-folder append all apply).
    pub async fn create_event(&self, args: CreateEventArgs) -> Result<CalendarEvent> {
        if args.summary.trim().is_empty() {
            return Err(CoreError::Other("event needs a title".into()));
        }
        if args.ends_at <= args.starts_at {
            return Err(CoreError::Other("event must end after it starts".into()));
        }
        let account_id = args.account_id;
        let account = if account_id == 0 {
            None
        } else {
            Some(
                self.db
                    .read(move |conn| repo::accounts::get(conn, account_id))
                    .await?
                    .ok_or_else(|| CoreError::NotFound("account".into()))?,
            )
        };
        if account.is_none() && !args.attendees.is_empty() {
            return Err(CoreError::Other(
                "connect a mail account before inviting attendees".into(),
            ));
        }

        if let Some(calendar_id) = args.calendar_id {
            let cal = self
                .calendar_db
                .read(move |conn| repo::caldav::get_calendar(conn, calendar_id))
                .await?
                .ok_or_else(|| CoreError::NotFound("calendar".into()))?;
            if cal.account_id != account_id {
                return Err(CoreError::Other(
                    "calendar belongs to another account".into(),
                ));
            }
            if cal.read_only {
                return Err(CoreError::Other("calendar is read-only".into()));
            }
            if !cal.enabled {
                return Err(CoreError::Other("calendar is disabled".into()));
            }
        }

        let uid = format!("{}-{}@flectar-mail", now_ms(), crate::mime::rand_token());
        let attendees: Vec<EventAttendee> = args
            .attendees
            .iter()
            .map(|a| EventAttendee {
                email: a.email.clone(),
                name: a.name.clone(),
                partstat: Some("NEEDS-ACTION".into()),
            })
            .collect();

        let ev = args.clone();
        let organizer_email = account
            .as_ref()
            .map(|a| a.email.clone())
            .unwrap_or_default();
        let uid_for_db = uid.clone();
        let event_id = self
            .calendar_db
            .write(move |conn| {
                repo::calendar::insert_local(
                    conn,
                    account_id,
                    ev.calendar_id,
                    &uid_for_db,
                    ev.summary.trim(),
                    ev.location.as_deref(),
                    ev.description.as_deref(),
                    ev.join_url.as_deref(),
                    &organizer_email,
                    &attendees,
                    ev.starts_at,
                    ev.ends_at,
                    ev.all_day,
                )
            })
            .await?;

        if !args.attendees.is_empty() {
            let Some(account) = account.as_ref() else {
                return Err(CoreError::Other(
                    "connect a mail account before inviting attendees".into(),
                ));
            };
            let organizer = Address {
                name: account.display_name.clone(),
                email: account.email.clone(),
            };
            let ics = calendar::build_request_ics(&calendar::InviteSpec {
                uid: &uid,
                sequence: 0,
                summary: args.summary.trim(),
                description: args.description.as_deref(),
                location: args.location.as_deref(),
                join_url: args.join_url.as_deref(),
                organizer: &organizer,
                attendees: &args.attendees,
                starts_at_ms: args.starts_at,
                ends_at_ms: args.ends_at,
                dtstamp_ms: now_ms(),
            });
            let body_text = invite_body_text(&args);
            self.send_calendar_mail(
                account_id,
                args.attendees.clone(),
                format!("Invitation: {}", args.summary.trim()),
                body_text,
                &ics,
            )
            .await?;
        }

        self.enqueue_cal_push(event_id, account_id, "cal_put")
            .await?;

        // Microsoft accounts have no CalDAV endpoint, so also write the event
        // into their Outlook / Microsoft 365 calendar via Graph - that is what
        // Teams and Outlook show. Best-effort: a Graph failure (e.g. the
        // account predates the calendar consent) must not fail event creation,
        // which already succeeded locally. Skipped when the account has a
        // connected calendar: the sync task's push already writes it (and a
        // second direct POST would duplicate the event in Outlook).
        if account
            .as_ref()
            .is_some_and(|account| account.provider == Provider::Microsoft)
        {
            let has_cal_config = self
                .calendar_db
                .read(move |conn| repo::caldav::get_config(conn, account_id))
                .await?
                .is_some();
            if !has_cal_config && let Err(e) = self.push_event_to_graph(account_id, &args).await {
                tracing::warn!(error = %e, "graph: could not sync event to Outlook calendar");
            }
        }

        self.calendar_db
            .read(move |conn| repo::calendar::get(conn, event_id))
            .await?
            .ok_or_else(|| CoreError::NotFound("event".into()))
    }

    /// Write a just-created event into the Microsoft 365 calendar via Graph.
    async fn push_event_to_graph(&self, account_id: i64, args: &CreateEventArgs) -> Result<()> {
        let token = self
            .tokens
            .access_token_for_scope(
                account_id,
                Provider::Microsoft,
                oauth::providers::MS_CALENDARS_SCOPE,
            )
            .await?;
        // Fold the join link into the body so it rides along in Outlook/Teams.
        let body_html = match (&args.description, &args.join_url) {
            (d, Some(url)) => Some(format!(
                "{}<p><a href=\"{}\">Join the meeting</a></p>",
                d.as_deref().unwrap_or(""),
                url
            )),
            (Some(d), None) if !d.trim().is_empty() => Some(d.clone()),
            _ => None,
        };
        let attendees = args
            .attendees
            .iter()
            .map(|a| graph::GraphAttendee {
                email: a.email.clone(),
                name: a.name.clone(),
            })
            .collect();
        graph::create_calendar_event(
            &token,
            None,
            &graph::GraphEvent {
                subject: args.summary.trim(),
                body_html,
                location: args.location.as_deref(),
                start_ms: args.starts_at,
                end_ms: args.ends_at,
                all_day: args.all_day,
                attendees,
            },
        )
        .await
        .map(|_| ())
    }

    /// Answer an invite: store our response and email an ICS METHOD:REPLY to
    /// the organizer (the standard-compliant path every calendar understands).
    pub async fn rsvp_event(&self, args: RsvpEventArgs) -> Result<CalendarEvent> {
        let partstat = match args.response.as_str() {
            "accepted" => "ACCEPTED",
            "tentative" => "TENTATIVE",
            "declined" => "DECLINED",
            _ => return Err(CoreError::Other("invalid RSVP response".into())),
        };
        let event_id = args.event_id;
        let (ev, uid_seq) = self
            .calendar_db
            .read(move |conn| {
                Ok((
                    repo::calendar::get(conn, event_id)?,
                    repo::calendar::uid_and_sequence(conn, event_id)?,
                ))
            })
            .await?;
        let ev = ev.ok_or_else(|| CoreError::NotFound("event".into()))?;
        let (uid, sequence) = uid_seq.ok_or_else(|| CoreError::NotFound("event".into()))?;

        let account_id = ev.account_id;
        let account = self
            .db
            .read(move |conn| repo::accounts::get(conn, account_id))
            .await?
            .ok_or_else(|| CoreError::NotFound("account".into()))?;

        // Reply goes to the organizer; without one there is nobody to notify,
        // but we still record the response locally.
        if let Some(organizer) = ev.organizer.clone().filter(|o| !o.is_empty()) {
            let me = Address {
                name: account.display_name.clone(),
                email: account.email.clone(),
            };
            let ics = calendar::build_reply_ics(&calendar::ReplySpec {
                uid: &uid,
                sequence,
                summary: ev.summary.as_deref(),
                partstat,
                organizer_email: &organizer,
                attendee: &me,
                starts_at_ms: ev.starts_at,
                ends_at_ms: ev.ends_at,
                dtstamp_ms: now_ms(),
            });
            let verb = match partstat {
                "ACCEPTED" => "Accepted",
                "TENTATIVE" => "Tentative",
                _ => "Declined",
            };
            let title = ev.summary.clone().unwrap_or_else(|| "(no title)".into());
            self.send_calendar_mail(
                account_id,
                vec![Address {
                    name: None,
                    email: organizer,
                }],
                format!("{verb}: {title}"),
                format!(
                    "{} has responded {} to: {title}",
                    account.email,
                    verb.to_lowercase()
                ),
                &ics,
            )
            .await?;
        }

        self.calendar_db
            .write(move |conn| repo::calendar::set_rsvp(conn, event_id, partstat))
            .await?;
        // CalDAV-backed invites also sync the PARTSTAT change to the server.
        self.enqueue_cal_push(event_id, ev.account_id, "cal_put")
            .await?;
        self.calendar_db
            .read(move |conn| repo::calendar::get(conn, event_id))
            .await?
            .ok_or_else(|| CoreError::NotFound("event".into()))
    }

    /// Edit an event we organize. The change is applied locally at once,
    /// attendees get an updated REQUEST ICS (bumped SEQUENCE) when `notify`,
    /// and CalDAV-backed events are flagged for the next push.
    pub async fn update_event(&self, args: UpdateEventArgs) -> Result<CalendarEvent> {
        if args.summary.trim().is_empty() {
            return Err(CoreError::Other("event needs a title".into()));
        }
        if args.ends_at <= args.starts_at {
            return Err(CoreError::Other("event must end after it starts".into()));
        }
        let event_id = args.event_id;
        let existing = self
            .calendar_db
            .read(move |conn| repo::calendar::get(conn, event_id))
            .await?
            .ok_or_else(|| CoreError::NotFound("event".into()))?;
        if !existing.is_local {
            return Err(CoreError::Other(
                "only events you organize can be edited".into(),
            ));
        }

        // Preserve responses attendees already gave; new addresses start out
        // NEEDS-ACTION.
        let attendees: Vec<EventAttendee> = args
            .attendees
            .iter()
            .map(|a| EventAttendee {
                email: a.email.clone(),
                name: a.name.clone(),
                partstat: existing
                    .attendees
                    .iter()
                    .find(|old| old.email.eq_ignore_ascii_case(&a.email))
                    .and_then(|old| old.partstat.clone())
                    .or_else(|| Some("NEEDS-ACTION".into())),
            })
            .collect();

        let args_for_db = args.clone();
        let atts_for_db = attendees.clone();
        self.calendar_db
            .write(move |conn| {
                repo::calendar::update_local_fields(conn, &args_for_db, &atts_for_db)
            })
            .await?;

        if args.notify && !args.attendees.is_empty() {
            let account_id = existing.account_id;
            let account = self
                .db
                .read(move |conn| repo::accounts::get(conn, account_id))
                .await?
                .ok_or_else(|| CoreError::NotFound("account".into()))?;
            let (uid, sequence) = self
                .calendar_db
                .read(move |conn| repo::calendar::uid_and_sequence(conn, event_id))
                .await?
                .ok_or_else(|| CoreError::NotFound("event".into()))?;
            let organizer = Address {
                name: account.display_name.clone(),
                email: account.email.clone(),
            };
            let ics = calendar::build_request_ics(&calendar::InviteSpec {
                uid: &uid,
                sequence,
                summary: args.summary.trim(),
                description: args.description.as_deref(),
                location: args.location.as_deref(),
                join_url: args.join_url.as_deref(),
                organizer: &organizer,
                attendees: &args.attendees,
                starts_at_ms: args.starts_at,
                ends_at_ms: args.ends_at,
                dtstamp_ms: now_ms(),
            });
            let body = invite_body_text(&CreateEventArgs {
                account_id,
                calendar_id: None,
                summary: args.summary.clone(),
                description: args.description.clone(),
                location: args.location.clone(),
                join_url: args.join_url.clone(),
                starts_at: args.starts_at,
                ends_at: args.ends_at,
                all_day: args.all_day,
                attendees: args.attendees.clone(),
            });
            self.send_calendar_mail(
                account_id,
                args.attendees.clone(),
                format!("Updated invitation: {}", args.summary.trim()),
                body,
                &ics,
            )
            .await?;
        }

        self.enqueue_cal_push(event_id, existing.account_id, "cal_put")
            .await?;
        self.calendar_db
            .read(move |conn| repo::calendar::get(conn, event_id))
            .await?
            .ok_or_else(|| CoreError::NotFound("event".into()))
    }

    /// Delete an event. Organized events with attendees email a METHOD:CANCEL
    /// when `notify`; CalDAV-backed rows become tombstones deleted at the next
    /// push, purely local rows disappear immediately.
    pub async fn delete_event(&self, event_id: i64, notify: bool) -> Result<()> {
        let ev = self
            .calendar_db
            .read(move |conn| repo::calendar::get(conn, event_id))
            .await?
            .ok_or_else(|| CoreError::NotFound("event".into()))?;

        if notify && ev.is_local && !ev.attendees.is_empty() {
            let account_id = ev.account_id;
            let account = self
                .db
                .read(move |conn| repo::accounts::get(conn, account_id))
                .await?
                .ok_or_else(|| CoreError::NotFound("account".into()))?;
            let (uid, sequence) = self
                .calendar_db
                .read(move |conn| repo::calendar::uid_and_sequence(conn, event_id))
                .await?
                .ok_or_else(|| CoreError::NotFound("event".into()))?;
            let organizer = Address {
                name: account.display_name.clone(),
                email: account.email.clone(),
            };
            let to: Vec<Address> = ev
                .attendees
                .iter()
                .map(|a| Address {
                    name: a.name.clone(),
                    email: a.email.clone(),
                })
                .collect();
            let title = ev.summary.clone().unwrap_or_else(|| "(no title)".into());
            let ics = calendar::build_cancel_ics(&calendar::InviteSpec {
                uid: &uid,
                sequence: sequence + 1,
                summary: &title,
                description: None,
                location: ev.location.as_deref(),
                join_url: None,
                organizer: &organizer,
                attendees: &to,
                starts_at_ms: ev.starts_at,
                ends_at_ms: ev.ends_at.unwrap_or(ev.starts_at + 1_800_000),
                dtstamp_ms: now_ms(),
            });
            self.send_calendar_mail(
                account_id,
                to,
                format!("Cancelled: {title}"),
                format!("{title} has been cancelled."),
                &ics,
            )
            .await?;
        }

        // CalDAV rows need the server-side DELETE; keep a tombstone and let
        // the push path finish the job. Mail-invite rows keep a terminal
        // tombstone - purging them would let the next re-parse of the
        // archived invitation email resurrect the event. Purely local rows
        // go right away.
        if ev.calendar_id.is_some() {
            self.calendar_db
                .write(move |conn| repo::calendar::mark_deleted(conn, event_id))
                .await?;
            self.enqueue_cal_push(event_id, ev.account_id, "cal_delete")
                .await?;
        } else if ev.message_id.is_some() {
            self.calendar_db
                .write(move |conn| repo::calendar::mark_deleted(conn, event_id))
                .await?;
        } else {
            self.calendar_db
                .write(move |conn| repo::calendar::hard_delete(conn, event_id))
                .await?;
        }
        Ok(())
    }

    /// Connect a calendar server to an account. Generic servers store the app
    /// password in the keyring; Google reuses the account's OAuth tokens (the
    /// caller must have completed the calendar-scope re-consent first).
    /// Discovery doubles as the connection test - nothing persists on failure.
    pub async fn connect_calendar(&self, args: ConnectCalendarArgs) -> Result<Vec<Calendar>> {
        let account_id = args.account_id;
        let kind = if args.kind == "google" {
            "google"
        } else {
            "generic"
        };
        let base_url = match kind {
            "google" => {
                let calendar_id = self
                    .db
                    .read(move |conn| {
                        Ok(repo::accounts::get(conn, account_id)?.map(|account| account.email))
                    })
                    .await?
                    .ok_or_else(|| CoreError::NotFound("account".into()))?;
                caldav::google_calendar_url(&calendar_id)?
            }
            _ => {
                let url = args
                    .url
                    .clone()
                    .filter(|u| !u.trim().is_empty())
                    .ok_or_else(|| CoreError::CalDav("server URL is required".into()))?;
                let mut url = url.trim().to_string();
                if !url.contains("://") {
                    url = format!("https://{url}");
                }
                url
            }
        };

        // Build auth without persisting anything yet.
        let auth = match kind {
            "google" => caldav::DavAuth::Bearer(
                self.tokens
                    .access_token(account_id, Provider::Gmail)
                    .await?,
            ),
            _ => {
                let user = args.username.clone().unwrap_or_default();
                let pass = args
                    .password
                    .clone()
                    .filter(|p| !p.is_empty())
                    .ok_or_else(|| CoreError::CalDav("password is required".into()))?;
                caldav::DavAuth::Basic(user, pass)
            }
        };
        let google_access_token = match &auth {
            caldav::DavAuth::Bearer(token) if kind == "google" => Some(token.clone()),
            _ => None,
        };
        let transport = caldav::HttpTransport::new(auth, &base_url)?;
        let (discovery, google_calendars) = if kind == "google" {
            // Validate the primary DAV collection so a disabled CalDAV API is
            // reported immediately. Calendar API metadata then supplies every
            // secondary/shared/special calendar id; CalDAV itself only knows
            // about the one id embedded in its collection URL.
            let primary =
                caldav::discovery::discover_known_collection(&transport, &base_url).await?;
            let access_token = google_access_token.as_deref().ok_or_else(|| {
                CoreError::CalDav("Google calendar authentication was not initialized".into())
            })?;
            let listed = googlecal::list_calendars(access_token).await?;
            if listed.is_empty() {
                return Err(CoreError::CalDav("no Google calendars found".into()));
            }
            (primary, Some(listed))
        } else {
            (
                caldav::discovery::discover(&transport, &base_url).await?,
                None,
            )
        };

        // Persist: keyring first, then config + collections.
        if kind == "generic"
            && let Some(pass) = args.password.clone()
        {
            credentials::store_async(
                self.credentials.clone(),
                account_id,
                Slot::CaldavPassword,
                pass,
            )
            .await?;
        }
        let cfg = repo::caldav::CaldavConfig {
            account_id,
            kind: kind.to_string(),
            base_url,
            username: args.username.clone(),
            principal_url: discovery.principal_url.clone(),
            home_set_url: Some(discovery.home_set_url.clone()),
            enabled: true,
            last_error: None,
        };
        let calendars = discovery.calendars.clone();
        let out = self
            .calendar_db
            .write(move |conn| {
                let tx = conn.transaction()?;
                repo::caldav::upsert_config(&tx, &cfg)?;
                if let Some(google_calendars) = &google_calendars {
                    googlecal::reconcile_calendars(&tx, account_id, google_calendars)?;
                } else {
                    let mut first_id = None;
                    for c in &calendars {
                        let id = repo::caldav::upsert_calendar(
                            &tx,
                            account_id,
                            &c.url,
                            c.display_name.as_deref(),
                            c.color.as_deref(),
                            false,
                        )?;
                        first_id.get_or_insert(id);
                    }
                    if let Some(id) = first_id {
                        // Keep an existing default if one is set; else first wins.
                        let has_default: i64 = tx.query_row(
                            "SELECT COUNT(*) FROM calendars WHERE account_id = ?1 AND is_default = 1",
                            rusqlite::params![account_id],
                            |r| r.get(0),
                        )?;
                        if has_default == 0 {
                            repo::caldav::set_default_calendar(&tx, account_id, id)?;
                        }
                    }
                }
                let list = repo::caldav::list_calendars(&tx, Some(account_id))?;
                tx.commit()?;
                Ok(list)
            })
            .await?;

        self.spawn_cal_task(account_id).await;
        self.nudge_cal(Some(account_id)).await;
        Ok(out)
    }

    /// Google calendar connection: re-run the OAuth consent with the
    /// calendar scope added (scopes are fixed at consent time, so the account
    /// must re-consent) and swap in the widened tokens, then discover.
    pub async fn connect_google_calendar(
        &self,
        account_id: i64,
        open_url: impl FnOnce(String) -> std::result::Result<(), String> + Send,
    ) -> Result<Vec<Calendar>> {
        let account = self
            .db
            .read(move |conn| repo::accounts::get(conn, account_id))
            .await?
            .ok_or_else(|| CoreError::NotFound("account".into()))?;
        if account.provider != Provider::Gmail {
            return Err(CoreError::CalDav(
                "google calendar needs a Gmail account".into(),
            ));
        }

        let outcome = tokio::select! {
            r = oauth::flow::authorize_with_broker(
                Provider::Gmail,
                &[oauth::providers::GOOGLE_CALENDAR_SCOPE],
                Some(&account.email),
                self.oauth_redirects.clone(),
                open_url,
            ) => r?,
            _ = self.oauth_cancel.notified() => {
                return Err(CoreError::Auth("sign-in cancelled".into()));
            }
        };
        if !outcome.email.eq_ignore_ascii_case(&account.email) {
            return Err(CoreError::Auth(format!(
                "consent was granted for {} - expected {}",
                outcome.email, account.email
            )));
        }
        self.tokens
            .store_initial(
                account_id,
                outcome.access_token,
                outcome.expires_in,
                outcome.refresh_token,
                outcome.client_id,
                outcome.client_secret,
            )
            .await?;

        self.connect_calendar(ConnectCalendarArgs {
            account_id,
            kind: "google".into(),
            url: None,
            username: None,
            password: None,
        })
        .await
    }

    /// Mint a Graph token for one extra scope, widening consent in the
    /// browser when the scope was never granted (incremental consent, like
    /// `connect_google_calendar`). `open_url` opens the consent page.
    async fn graph_token_with_consent(
        &self,
        account_id: i64,
        scope: &str,
        open_url: impl FnOnce(String) -> std::result::Result<(), String> + Send,
    ) -> Result<String> {
        let account = self
            .db
            .read(move |conn| repo::accounts::get(conn, account_id))
            .await?
            .ok_or_else(|| CoreError::NotFound("account".into()))?;
        if account.provider != Provider::Microsoft {
            return Err(CoreError::Auth("needs a Microsoft account".into()));
        }

        let extra_scopes = [scope];
        match self
            .tokens
            .access_token_for_scope(account_id, Provider::Microsoft, scope)
            .await
        {
            Ok(t) => Ok(t),
            // Scope not yet consented (or refresh token stale): widen consent
            // in the browser, then mint the Graph token again.
            Err(CoreError::NeedsReauth) => {
                let outcome = tokio::select! {
                    r = oauth::flow::authorize_with_broker(
                        Provider::Microsoft,
                        &extra_scopes,
                        Some(&account.email),
                        self.oauth_redirects.clone(),
                        open_url,
                    ) => r?,
                    _ = self.oauth_cancel.notified() => {
                        return Err(CoreError::Auth("sign-in cancelled".into()));
                    }
                };
                if !outcome.email.eq_ignore_ascii_case(&account.email) {
                    return Err(CoreError::Auth(format!(
                        "consent was granted for {} - expected {}",
                        outcome.email, account.email
                    )));
                }
                self.tokens
                    .store_initial(
                        account_id,
                        outcome.access_token,
                        outcome.expires_in,
                        outcome.refresh_token,
                        outcome.client_id,
                        outcome.client_secret,
                    )
                    .await?;
                self.tokens
                    .access_token_for_scope(account_id, Provider::Microsoft, scope)
                    .await
            }
            Err(e) => Err(e),
        }
    }

    /// Create a Microsoft Teams online meeting for a Microsoft account and
    /// return its join URL (for insertion into a compose draft).
    ///
    /// Needs a Graph-scoped token; the first attempt may trigger an
    /// incremental re-consent in the browser.
    pub async fn create_teams_meeting(
        &self,
        account_id: i64,
        subject: &str,
        start_ms: i64,
        end_ms: i64,
        open_url: impl FnOnce(String) -> std::result::Result<(), String> + Send,
    ) -> Result<graph::OnlineMeeting> {
        let token = self
            .graph_token_with_consent(
                account_id,
                oauth::providers::MS_ONLINE_MEETINGS_SCOPE,
                open_url,
            )
            .await?;
        graph::create_online_meeting(&token, subject, start_ms, end_ms).await
    }

    /// Microsoft calendar connection: Outlook / Microsoft 365 has no CalDAV
    /// endpoint, so the calendar syncs via Graph. Widens consent to
    /// `Calendars.ReadWrite` when needed, lists the account's calendars and
    /// persists them under a "microsoft"-kind config; the shared calendar
    /// task then pulls via calendarView delta.
    pub async fn connect_microsoft_calendar(
        &self,
        account_id: i64,
        open_url: impl FnOnce(String) -> std::result::Result<(), String> + Send,
    ) -> Result<Vec<Calendar>> {
        let token = self
            .graph_token_with_consent(account_id, oauth::providers::MS_CALENDARS_SCOPE, open_url)
            .await?;
        let discovered = graph::list_calendars(&token).await?;
        if discovered.is_empty() {
            return Err(CoreError::CalDav("no calendars found".into()));
        }

        let cfg = repo::caldav::CaldavConfig {
            account_id,
            kind: "microsoft".to_string(),
            base_url: graph::GRAPH_BASE.to_string(),
            username: None,
            principal_url: None,
            home_set_url: None,
            enabled: true,
            last_error: None,
        };
        let out = self
            .calendar_db
            .write(move |conn| {
                let tx = conn.transaction()?;
                repo::caldav::upsert_config(&tx, &cfg)?;
                let mut default_id = None;
                let mut first_id = None;
                for c in &discovered {
                    let id = repo::caldav::upsert_calendar(
                        &tx,
                        account_id,
                        &c.id,
                        c.name.as_deref(),
                        c.hex_color.as_deref(),
                        !c.can_edit,
                    )?;
                    first_id.get_or_insert(id);
                    if c.is_default {
                        default_id.get_or_insert(id);
                    }
                }
                if let Some(id) = default_id.or(first_id) {
                    // Keep an existing default if one is set; else Outlook's
                    // default calendar (or the first) wins.
                    let has_default: i64 = tx.query_row(
                        "SELECT COUNT(*) FROM calendars WHERE account_id = ?1 AND is_default = 1",
                        rusqlite::params![account_id],
                        |r| r.get(0),
                    )?;
                    if has_default == 0 {
                        repo::caldav::set_default_calendar(&tx, account_id, id)?;
                    }
                }
                let list = repo::caldav::list_calendars(&tx, Some(account_id))?;
                tx.commit()?;
                Ok(list)
            })
            .await?;

        self.spawn_cal_task(account_id).await;
        self.nudge_cal(Some(account_id)).await;
        Ok(out)
    }

    /// Disconnect the calendar server: events stay locally, sync bookkeeping
    /// is cleared, credentials removed.
    pub async fn disconnect_calendar(&self, account_id: i64) -> Result<()> {
        self.cal_handles.write().await.remove(&account_id);
        self.calendar_db
            .write(move |conn| repo::caldav::delete_config(conn, account_id))
            .await?;
        let _ = credentials::store_async(
            self.credentials.clone(),
            account_id,
            Slot::CaldavPassword,
            String::new(),
        )
        .await;
        self.bus.emit(CoreEvent::CalendarUpdated { account_id });
        Ok(())
    }

    pub async fn list_calendars(&self, account_id: Option<i64>) -> Result<Vec<Calendar>> {
        self.calendar_db
            .read(move |conn| repo::caldav::list_calendars(conn, account_id))
            .await
    }

    pub async fn list_calendar_connections(&self) -> Result<Vec<CalendarConnection>> {
        self.calendar_db
            .read(|conn| {
                Ok(repo::caldav::list_configs(conn)?
                    .into_iter()
                    .map(|cfg| CalendarConnection {
                        account_id: cfg.account_id,
                        kind: cfg.kind,
                        enabled: cfg.enabled,
                        last_error: cfg.last_error,
                    })
                    .collect())
            })
            .await
    }

    /// Pause/resume a connected calendar without deleting local events,
    /// discovered collections, sync tokens, or credentials.
    pub async fn set_account_calendar_enabled(&self, account_id: i64, enabled: bool) -> Result<()> {
        let found = self
            .calendar_db
            .write(move |conn| repo::caldav::set_config_enabled(conn, account_id, enabled))
            .await?;
        if !found {
            return Err(CoreError::NotFound("calendar connection".into()));
        }
        if enabled {
            if !self.cal_handles.read().await.contains_key(&account_id) {
                self.spawn_cal_task(account_id).await;
            }
            self.nudge_cal(Some(account_id)).await;
        } else {
            self.cal_handles.write().await.remove(&account_id);
        }
        self.bus.emit(CoreEvent::CalendarUpdated { account_id });
        Ok(())
    }

    pub async fn set_calendar_enabled(&self, calendar_id: i64, enabled: bool) -> Result<()> {
        self.calendar_db
            .write(move |conn| repo::caldav::set_calendar_enabled(conn, calendar_id, enabled))
            .await?;
        let cal = self
            .calendar_db
            .read(move |conn| repo::caldav::get_calendar(conn, calendar_id))
            .await?;
        if let Some(cal) = cal {
            if enabled {
                self.nudge_cal(Some(cal.account_id)).await;
            }
            self.bus.emit(CoreEvent::CalendarUpdated {
                account_id: cal.account_id,
            });
        }
        Ok(())
    }

    /// User override of the calendar's display color (`#RRGGBB`). None
    /// clears the override so the next discovery restores the server color.
    pub async fn set_calendar_color(&self, calendar_id: i64, color: Option<String>) -> Result<()> {
        if let Some(c) = &color {
            let valid = c.len() == 7
                && c.starts_with('#')
                && c[1..].chars().all(|ch| ch.is_ascii_hexdigit());
            if !valid {
                return Err(CoreError::Other("invalid color".into()));
            }
        }
        let cal = self
            .calendar_db
            .read(move |conn| repo::caldav::get_calendar(conn, calendar_id))
            .await?
            .ok_or_else(|| CoreError::NotFound("calendar".into()))?;
        self.calendar_db
            .write(move |conn| {
                repo::caldav::set_calendar_color(conn, calendar_id, color.as_deref())
            })
            .await?;
        self.bus.emit(CoreEvent::CalendarUpdated {
            account_id: cal.account_id,
        });
        Ok(())
    }

    /// Make this calendar the account's target for newly created events.
    pub async fn set_default_calendar(&self, calendar_id: i64) -> Result<()> {
        let cal = self
            .calendar_db
            .read(move |conn| repo::caldav::get_calendar(conn, calendar_id))
            .await?
            .ok_or_else(|| CoreError::NotFound("calendar".into()))?;
        if cal.read_only {
            return Err(CoreError::Other("calendar is read-only".into()));
        }
        let account_id = cal.account_id;
        self.calendar_db
            .write(move |conn| repo::caldav::set_default_calendar(conn, account_id, calendar_id))
            .await?;
        self.bus.emit(CoreEvent::CalendarUpdated { account_id });
        Ok(())
    }

    pub async fn calendar_sync_now(&self, account_id: Option<i64>) {
        self.nudge_cal(account_id).await;
    }

    /// Queue a CalDAV write for the account's calendar task, when the account
    /// has one configured. No-op otherwise (purely local calendars).
    async fn enqueue_cal_push(&self, event_id: i64, account_id: i64, kind: &str) -> Result<()> {
        let kind = kind.to_string();
        self.calendar_db
            .write(move |conn| {
                if repo::caldav::get_config(conn, account_id)?.is_none() {
                    return Ok(());
                }
                // Dirty guards the row against being clobbered by a pull that
                // runs between now and the push.
                if kind == "cal_put" {
                    repo::calendar::mark_dirty(conn, event_id)?;
                }
                let payload = serde_json::json!({ "eventId": event_id });
                repo::actions::enqueue(conn, account_id, &kind, None, None, &payload, None)?;
                Ok(())
            })
            .await?;
        self.nudge_cal(Some(account_id)).await;
        Ok(())
    }

    /// Draft + queue an email carrying an ICS part, through the normal send
    /// pipeline. The ICS is staged like an attachment.
    async fn send_calendar_mail(
        &self,
        account_id: i64,
        to: Vec<Address>,
        subject: String,
        body_text: String,
        ics: &str,
    ) -> Result<()> {
        let tmp_dir = self.paths.temp_dir();
        tokio::fs::create_dir_all(&tmp_dir).await?;
        let tmp_path = tmp_dir.join(format!("invite-{}.ics", crate::mime::rand_token()));
        tokio::fs::write(&tmp_path, ics.as_bytes()).await?;

        let draft_id = self
            .save_draft(SaveDraftArgs {
                draft_id: None,
                account_id,
                to,
                cc: Vec::new(),
                bcc: Vec::new(),
                subject,
                body_text,
                body_html: None,
                mode: "new".into(),
                in_reply_to_message_id: None,
                attachments: vec![DraftAttachmentIn {
                    file_path: tmp_path.to_string_lossy().into_owned(),
                    filename: "invite.ics".into(),
                }],
            })
            .await?;
        // The draft staged its own copy; the temp file can go.
        let _ = tokio::fs::remove_file(&tmp_path).await;
        self.queue_send(QueueSendArgs {
            draft_id,
            send_at: None,
        })
        .await?;
        Ok(())
    }

    async fn ai_config(&self, scenario: Scenario) -> Result<ai::AiConfig> {
        let settings = self.db.read(|conn| repo::settings::get(conn)).await?;
        self.ai_config_from(&settings, scenario).await
    }

    /// Build an [`ai::AiConfig`] for `scenario` from already-loaded settings,
    /// picking the model for the scenario's tier (all tiers share the base URL
    /// and stored API key).
    async fn ai_config_from(
        &self,
        settings: &Settings,
        scenario: Scenario,
    ) -> Result<ai::AiConfig> {
        let api_key =
            match credentials::load_async(self.credentials.clone(), 0, Slot::AiApiKey).await {
                Ok(k) => k,
                // Local endpoints (LM Studio, Ollama over http://) need no key;
                // hosted ones do, so fail early with a pointer to Settings.
                Err(_) if settings.ai_base_url.starts_with("http://") => String::new(),
                Err(_) => return Err(CoreError::AiNotConfigured),
            };
        Ok(ai::AiConfig {
            base_url: settings.ai_base_url.clone(),
            model: resolve_ai_model(settings, scenario),
            api_key,
            language: ai::ui_language_name(&settings.language).map(str::to_string),
            usage_sink: {
                let db = self.db.clone();
                let scenario = scenario.as_str().to_string();
                Some(Arc::new(move |usage: ai::AiUsage| {
                    let db = db.clone();
                    let scenario = scenario.clone();
                    if let Ok(handle) = tokio::runtime::Handle::try_current() {
                        handle.spawn(async move {
                            let result = db
                                .write(move |conn| {
                                    repo::ai_usage::record(
                                        conn,
                                        &repo::ai_usage::AiUsageRecord {
                                            occurred_at: now_ms(),
                                            model: &usage.model,
                                            scenario: &scenario,
                                            prompt_tokens: usage.prompt_tokens,
                                            completion_tokens: usage.completion_tokens,
                                            total_tokens: usage.total_tokens,
                                            exact: usage.exact,
                                        },
                                    )
                                })
                                .await;
                            if let Err(e) = result {
                                tracing::warn!("recording ai usage: {e}");
                            }
                        });
                    }
                }))
            },
        })
    }

    pub async fn ai_usage_stats(&self) -> Result<AiUsageStats> {
        self.db.read(|conn| repo::ai_usage::stats(conn)).await
    }

    pub async fn email_stats(&self) -> Result<EmailStats> {
        self.db.read(|conn| repo::email_stats::stats(conn)).await
    }

    /// Translate a plain-language automation request into a validated action
    /// plan. This only previews behavior; it never executes mailbox actions.
    pub async fn ai_plan_automation(&self, prompt: String) -> Result<AiAutomationPlan> {
        let prompt = prompt.trim();
        if prompt.is_empty() {
            return Ok(AiAutomationPlan {
                issues: vec![
                    "Describe the emails to match and what Flectar Mail should do.".into(),
                ],
                ..AiAutomationPlan::default()
            });
        }
        let (settings, labels, splits) = self
            .db
            .read(|conn| {
                Ok((
                    repo::settings::get(conn)?,
                    repo::labels::list(conn)?,
                    repo::splits::list(conn)?,
                ))
            })
            .await?;
        let cfg = self.ai_config_from(&settings, Scenario::Categorize).await?;
        let messages = route::automation_planner_prompt(prompt, &labels, &splits);
        let output = ai::chat(&cfg, messages).await?;
        Ok(route::validate_automation_plan(
            route::parse_automation_plan(&output),
            &labels,
            &splits,
        ))
    }

    pub async fn set_ai_key(&self, api_key: String) -> Result<()> {
        if api_key.trim().is_empty() {
            credentials::delete_all_async(self.credentials.clone(), 0).await?;
            return Ok(());
        }
        credentials::store_async(
            self.credentials.clone(),
            0,
            Slot::AiApiKey,
            api_key.trim().to_string(),
        )
        .await
    }

    pub async fn ai_status(&self) -> Result<AiStatus> {
        let settings = self.db.read(|conn| repo::settings::get(conn)).await?;
        let configured = credentials::load_async(self.credentials.clone(), 0, Slot::AiApiKey)
            .await
            .is_ok()
            || settings.ai_base_url.starts_with("http://");
        Ok(AiStatus {
            configured,
            model_instant: settings.ai_model_instant,
            model_cheap: settings.ai_model_cheap,
            model_intelligent: settings.ai_model_intelligent,
            base_url: settings.ai_base_url,
        })
    }

    /// Model ids from the configured endpoint. Works keyless on OpenRouter,
    /// so this is available before an API key is saved.
    pub async fn ai_list_models(&self) -> Result<Vec<String>> {
        let settings = self.db.read(|conn| repo::settings::get(conn)).await?;
        let api_key = credentials::load_async(self.credentials.clone(), 0, Slot::AiApiKey)
            .await
            .unwrap_or_default();
        ai::list_models(&settings.ai_base_url, &api_key).await
    }

    /// Parse a natural-language palette query ("meeting tomorrow 8pm ...")
    /// into a structured intent the UI can execute.
    pub async fn ai_command(&self, query: String) -> Result<AiIntent> {
        let cfg = self.ai_config(Scenario::Command).await?;
        ai::intent(&cfg, &query).await
    }

    pub async fn ai_summarize(&self, thread_id: i64) -> Result<crate::models::AiThreadSummary> {
        let cfg = self.ai_config(Scenario::Summarize).await?;
        let detail = self.get_thread(thread_id).await?;
        let context = ai::thread_context(&detail.messages, 24_000);
        let bus = self.bus.clone();
        ai::summarize_thread_stream(&cfg, &detail.thread.subject, &context, move |delta| {
            bus.emit(CoreEvent::AiSummaryDelta {
                thread_id,
                delta: delta.to_owned(),
            });
        })
        .await
    }

    /// Up to 3 short one-tap reply suggestions grounded in the thread, shown
    /// as chips in an empty reply composer. Runs on the instant tier: the
    /// chips are only useful if they appear before the user starts typing.
    pub async fn ai_quick_replies(&self, thread_id: i64) -> Result<Vec<String>> {
        let settings = self.db.read(|conn| repo::settings::get(conn)).await?;
        let cfg = self.ai_config_from(&settings, Scenario::QuickReply).await?;
        let detail = self.get_thread(thread_id).await?;
        let context = ai::thread_context(&detail.messages, 12_000);
        // Match the user's learned voice when voice drafting is enabled, same as
        // full AI drafts; otherwise pass an empty profile for the neutral prompt.
        let profile = if settings.voice_drafting {
            settings.voice_profile.as_str()
        } else {
            ""
        };
        ai::quick_replies(&cfg, &detail.thread.subject, &context, profile).await
    }

    /// Draft or rewrite email body text. With a thread, the reply is grounded
    /// in its content; without, it's freeform writing from the instruction.
    /// When `voice` (or the persisted setting) is on, the draft imitates the
    /// user's learned writing style and their similar past sent emails.
    pub async fn ai_draft(
        &self,
        thread_id: Option<i64>,
        reply_to_message_id: Option<i64>,
        instruction: String,
        sender_name: String,
        voice: Option<bool>,
        has_signature: bool,
    ) -> Result<String> {
        let settings = self.db.read(|conn| repo::settings::get(conn)).await?;
        let cfg = self.ai_config_from(&settings, Scenario::Draft).await?;
        let use_voice = voice.unwrap_or(settings.voice_drafting);

        let (subject, context, reply_target) = match thread_id {
            Some(tid) => {
                let detail = self.get_thread(tid).await?;
                (
                    detail.thread.subject.clone(),
                    ai::thread_context(&detail.messages, 24_000),
                    ai::reply_target_line(&detail.messages, reply_to_message_id),
                )
            }
            None => (String::new(), String::new(), String::new()),
        };

        if use_voice {
            let query = format!("{subject}\n{instruction}");
            let examples = self.voice_examples(&query, 3).await.unwrap_or_default();
            return ai::chat(
                &cfg,
                ai::apply_language(
                    ai::draft_prompt_voiced(
                        &subject,
                        &context,
                        &reply_target,
                        &instruction,
                        ai::VoiceDraftContext {
                            sender_name: &sender_name,
                            profile: &settings.voice_profile,
                            examples: &examples,
                            has_signature,
                        },
                    ),
                    &cfg,
                ),
            )
            .await;
        }

        ai::chat(
            &cfg,
            ai::apply_language(
                ai::draft_prompt(
                    &subject,
                    &context,
                    &reply_target,
                    &instruction,
                    &sender_name,
                    has_signature,
                ),
                &cfg,
            ),
        )
        .await
    }

    /// Copy-edit a draft body (plain text or simple HTML) without changing
    /// meaning, tone, or language. Returns the corrected draft.
    pub async fn ai_proofread(&self, body: String) -> Result<String> {
        let cfg = self.ai_config(Scenario::Draft).await?;
        ai::chat(&cfg, ai::proofread_prompt(&body)).await
    }

    /// Generate a clean email signature for an account from its name and
    /// address. Returns plain text with line breaks for the caller to render.
    pub async fn ai_signature(&self, name: String, email: String) -> Result<String> {
        let cfg = self.ai_config(Scenario::Draft).await?;
        ai::chat(
            &cfg,
            ai::apply_language(ai::signature_prompt(&name, &email), &cfg),
        )
        .await
    }

    /// Distill the user's writing voice from their sent mail and persist it as
    /// a style profile. Returns the profile text.
    pub async fn ai_learn_voice(&self) -> Result<String> {
        let cfg = self.ai_config(Scenario::Voice).await?;
        let samples = self
            .db
            .read(|conn| {
                let rows = repo::messages::list_sent_bodies(conn, None, 30)?;
                Ok::<_, CoreError>(
                    rows.into_iter()
                        .map(|(_, subj, body)| format!("Subject: {subj}\n{body}"))
                        .collect::<Vec<_>>(),
                )
            })
            .await?;
        if samples.is_empty() {
            return Err(CoreError::Other(
                "No sent emails to learn from yet. Send or sync some mail first.".into(),
            ));
        }
        let profile = ai::chat(&cfg, ai::voice_profile_prompt(&samples)).await?;

        let p = profile.clone();
        let now = now_ms();
        self.db
            .write(move |conn| {
                let mut s = repo::settings::get(conn)?;
                s.voice_profile = p;
                s.voice_learned_at = now;
                repo::settings::set(conn, &s)
            })
            .await?;
        Ok(profile)
    }

    /// Up to `k` (incoming → the user's reply) exchanges from their sent mail
    /// most relevant to `query`, for few-shot voice imitation. Prefers semantic
    /// retrieval; falls back to recent sent mail when the index is empty.
    async fn voice_examples(&self, query: &str, k: usize) -> Result<Vec<(String, String)>> {
        let hits = self.vector_hits(query, 40).await.unwrap_or_default();
        let hit_ids: Vec<i64> = hits.iter().map(|(id, _)| *id).collect();
        self.db
            .read(move |conn| {
                let sent_ids = repo::messages::filter_sent(conn, &hit_ids)?;
                let mut out: Vec<(String, String)> = Vec::new();
                for mid in sent_ids {
                    if out.len() >= k {
                        break;
                    }
                    if let Some(pair) = build_example_pair(conn, mid)? {
                        out.push(pair);
                    }
                }
                if out.is_empty() {
                    // No index / no similar sent mail: use recent sent as exemplars.
                    for (_, subject, body) in
                        repo::messages::list_sent_bodies(conn, None, k as i64)?
                    {
                        out.push((format!("(Compose a new email. Subject: {subject})"), body));
                    }
                }
                Ok::<_, CoreError>(out)
            })
            .await
    }

    pub async fn search(
        &self,
        query: String,
        account_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<ThreadSummary>> {
        let mut parsed = search::parse(&query);
        // Scope every branch (lexical, semantic, operator filters) to the UI's
        // active account when one is selected; None searches all accounts.
        parsed.account_id = account_id;
        let limit = limit.clamp(1, 100);
        let t0 = std::time::Instant::now();

        // The lexical DB read and the semantic branch (a CPU-bound model
        // forward pass + KNN) are independent - run them concurrently so
        // latency is max(lexical, semantic), not their sum. Semantic is
        // best-effort and skipped for queries too short to carry meaning.
        let lex_fut = {
            let q = parsed.clone();
            self.db.read(move |conn| {
                repo::search::lexical_thread_ids(conn, &q, repo::search::candidate_cap(limit))
            })
        };
        let vec_fut = async {
            if parsed.text.chars().count() < 3 {
                Vec::new()
            } else {
                self.vector_hits(&parsed.text, 200)
                    .await
                    .unwrap_or_default()
            }
        };
        let (lexical, vec_hits) = tokio::join!(lex_fut, vec_fut);
        let lexical = lexical?;
        let t_branches = t0.elapsed();

        let parsed2 = parsed.clone();
        let out = self
            .db
            .read(move |conn| repo::search::fuse(conn, &parsed2, lexical, &vec_hits, limit))
            .await;
        tracing::debug!(
            "search '{}': branches {:?}, fuse+hydrate {:?}",
            parsed.text,
            t_branches,
            t0.elapsed() - t_branches
        );
        out
    }

    /// Search for a chronological mail timeline. This keeps relevance-ranked
    /// search available to assistants and retrieval callers while giving the
    /// interactive mail list the newest matching threads first.
    pub async fn search_chronological(
        &self,
        query: String,
        account_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<ThreadSummary>> {
        let mut parsed = search::parse(&query);
        parsed.account_id = account_id;
        let limit = limit.clamp(1, 100);
        self.db
            .read(move |conn| repo::search::chronological(conn, &parsed, limit))
            .await
    }

    /// Embed `text` as a query and return the top-`k` (message_id, score) hits
    /// from the in-memory index. Empty when no local model is loaded. Query
    /// embeddings are cached, so repeated or backspaced-over queries skip the
    /// model forward pass entirely.
    #[cfg(feature = "local-embeddings")]
    async fn vector_hits(&self, text: &str, k: usize) -> Result<Vec<(i64, f32)>> {
        let Some(embedder) = self.embed.embedder().await else {
            return Ok(Vec::new());
        };
        let qv = match self.embed.cached_query(text).await {
            Some(v) => v,
            None => {
                let t = text.to_string();
                let v = tokio::task::spawn_blocking(move || embedder.embed_query(&t))
                    .await
                    .map_err(|e| CoreError::Other(format!("embed query join: {e}")))??;
                self.embed.cache_query(text.to_string(), v.clone()).await;
                v
            }
        };
        let idx = self.embed.index.read().await;
        Ok(idx.search(&qv, k))
    }

    #[cfg(not(feature = "local-embeddings"))]
    async fn vector_hits(&self, _text: &str, _k: usize) -> Result<Vec<(i64, f32)>> {
        Ok(Vec::new())
    }

    /// Pre-compute and cache the query embedding for `query` while the user is
    /// still typing, so the search that fires when they pause skips the model
    /// forward pass. Best-effort: no-ops when the model isn't loaded, the
    /// query is too short for the semantic branch, or it's already cached.
    #[cfg(feature = "local-embeddings")]
    pub async fn warm_query_embedding(&self, query: String) {
        let parsed = search::parse(&query);
        if parsed.text.chars().count() < 3 {
            return;
        }
        let Some(embedder) = self.embed.embedder().await else {
            return;
        };
        if self.embed.cached_query(&parsed.text).await.is_some() {
            return;
        }
        let t = parsed.text.clone();
        if let Ok(Ok(v)) = tokio::task::spawn_blocking(move || embedder.embed_query(&t)).await {
            self.embed.cache_query(parsed.text, v).await;
        }
    }

    #[cfg(not(feature = "local-embeddings"))]
    pub async fn warm_query_embedding(&self, _query: String) {}

    pub async fn embedding_status(&self) -> Result<EmbeddingStatus> {
        let settings = self.db.read(|conn| repo::settings::get(conn)).await?;
        let enabled = settings.embedding_backend == "local";
        let model = settings.embedding_model.clone();
        let model_id = embed::spec_or_default(&model).key.to_string();
        let m = model_id.clone();
        let (total, embedded, pending) = self
            .db
            .read(move |conn| repo::embeddings::counts(conn, &m))
            .await?;
        #[cfg(feature = "local-embeddings")]
        let ready = self.embed.embedder().await.is_some()
            && *self.embed.active_model.read().await == model_id;
        #[cfg(not(feature = "local-embeddings"))]
        let ready = false;
        Ok(EmbeddingStatus {
            enabled,
            model: model_id,
            total,
            embedded,
            pending,
            ready,
        })
    }

    /// Requeue the whole mailbox for (re-)embedding. The worker drains it.
    pub async fn semantic_reindex(&self) -> Result<i64> {
        let n = self
            .db
            .write(|conn| repo::embeddings::mark_all_pending(conn))
            .await?;
        Ok(n as i64)
    }

    /// RAG: answer a natural-language question grounded in the most relevant
    /// messages, returning the answer plus its source citations.
    /// Answer a question about the mailbox. Seeds with a semantic-search RAG
    /// pass, then hands the model a `search_inbox` tool so it can reformulate
    /// queries and dig deeper on its own before answering. Falls back to a plain
    /// one-shot RAG answer if the model/endpoint doesn't support tool calling.
    pub async fn ai_ask(&self, question: String, request_id: String) -> Result<AskResult> {
        const MAX_ROUNDS: usize = 4;
        let cfg = self.ai_config(Scenario::Ask).await?;

        // RAG seed: the model always starts from the best hybrid matches.
        let mut sources = self.retrieve_search(&question, 8).await?;
        if sources.is_empty() {
            return Ok(AskResult {
                answer: "I couldn't find anything relevant in your mailbox. \
                         Make sure semantic search is enabled and indexing has finished."
                    .into(),
                citations: Vec::new(),
            });
        }
        // Surface the seed sources immediately; more are emitted as the model searches.
        self.emit_ask_citations(&request_id, &sources);

        let mut initial_context = String::new();
        for (i, m) in sources.iter().enumerate() {
            initial_context.push_str(&ai::format_excerpt(i + 1, m));
        }
        let ask_system = format!("{}{}", ai::AGENTIC_ASK_SYSTEM, ai::language_directive(&cfg));
        let mut messages: Vec<serde_json::Value> = vec![
            serde_json::json!({ "role": "system", "content": ask_system }),
            serde_json::json!({
                "role": "user",
                "content": format!("Emails:\n\n{initial_context}\nQuestion: {question}"),
            }),
        ];
        let tools = ai::search_inbox_tool();

        // Agentic loop: let the model call search_inbox until it answers or we
        // hit the round cap. `answer = Some` means the model produced text.
        let mut answer: Option<String> = None;
        for round in 0..MAX_ROUNDS {
            match ai::chat_tools(&cfg, messages.clone(), tools.clone()).await {
                Ok(ai::ChatStep::Content(text)) => {
                    answer = Some(text);
                    break;
                }
                Ok(ai::ChatStep::Tools { assistant, calls }) => {
                    messages.push(assistant);
                    for call in calls {
                        let (result, added) = if call.name == "search_inbox" {
                            self.run_search_inbox(&call.arguments, &mut sources).await
                        } else {
                            (format!("Unknown tool: {}", call.name), 0)
                        };
                        messages.push(serde_json::json!({
                            "role": "tool",
                            "tool_call_id": call.id,
                            "content": result,
                        }));
                        if added > 0 {
                            self.emit_ask_citations(&request_id, &sources);
                        }
                    }
                }
                Err(e) => {
                    // Any tool-round failure (no tool support, a provider hiccup,
                    // a malformed tool reply) shouldn't sink the whole Ask - we
                    // already have grounded sources, so fall through to a plain
                    // streamed answer over them instead of erroring out.
                    tracing::warn!("ai_ask tool round {round} failed, using plain fallback: {e}");
                    break;
                }
            }
        }

        let answer = match answer {
            // The agentic path answered directly (chat_tools is non-streaming, so
            // emit its text as one delta). Empty answers fall through to the
            // streamed fallback below rather than settling on a blank result.
            Some(text) if !text.trim().is_empty() => {
                self.bus.emit(CoreEvent::AskDelta {
                    request_id: request_id.clone(),
                    delta: text.clone(),
                });
                text
            }
            _ => {
                // Cap reached while still searching, a tool-less model, or a
                // mid-loop failure: force a final streamed answer over everything
                // gathered, tools off. Reasoning is streamed on its own channel.
                messages.push(serde_json::json!({
                    "role": "system",
                    "content": "Now answer the user's question using ONLY the numbered excerpts \
                                above. Cite them like [1]. If the answer isn't there, say you \
                                couldn't find it. Answer in the user's language, concisely; light \
                                Markdown is fine, no preamble.",
                }));
                let (bus_a, rid_a) = (self.bus.clone(), request_id.clone());
                let (bus_r, rid_r) = (self.bus.clone(), request_id.clone());
                let (answer, _reasoning) = ai::chat_stream_json_split(
                    &cfg,
                    messages,
                    move |delta| {
                        bus_a.emit(CoreEvent::AskDelta {
                            request_id: rid_a.clone(),
                            delta: delta.to_string(),
                        });
                    },
                    move |delta| {
                        bus_r.emit(CoreEvent::AskReasoning {
                            request_id: rid_r.clone(),
                            delta: delta.to_string(),
                        });
                    },
                )
                .await?;
                answer
            }
        };
        // Never settle on a blank answer - the model thought but produced no
        // user-facing text.
        let answer = if answer.trim().is_empty() {
            "I couldn't find an answer to that in your mailbox.".to_string()
        } else {
            answer
        };
        self.bus.emit(CoreEvent::AskDone { request_id });

        Ok(AskResult {
            answer,
            citations: Self::ask_citations(&sources),
        })
    }

    /// Operator-aware hybrid retrieval (semantic RAG fused with `from:`/`to:`/
    /// `subject:`/`is:`/`has:` keyword filters) hydrated to message details for
    /// citation. Powers both the Ask RAG seed and the agentic `search_inbox`
    /// tool, so the model can search by meaning, sender, recipient, and more.
    async fn retrieve_search(&self, query: &str, k: usize) -> Result<Vec<MessageDetail>> {
        let parsed = crate::search::parse(query);
        // Semantic branch only carries meaning for queries of a few chars+.
        let vec_hits = if parsed.text.chars().count() >= 3 {
            self.vector_hits(&parsed.text, 200)
                .await
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let k = k as i64;
        self.db
            .read(move |conn| {
                let ids = repo::search::message_hits(conn, &parsed, &vec_hits, k)?;
                let mut out = Vec::new();
                for id in ids {
                    if let Ok(d) = repo::messages::detail(conn, id) {
                        out.push(d);
                    }
                }
                Ok::<_, CoreError>(out)
            })
            .await
    }

    /// Execute a `search_inbox` tool call: run the search, append any *new*
    /// messages to `sources` with stable citation numbers, and return the
    /// excerpt block for the model plus how many new sources were added.
    async fn run_search_inbox(
        &self,
        arguments: &str,
        sources: &mut Vec<MessageDetail>,
    ) -> (String, usize) {
        let args: serde_json::Value =
            serde_json::from_str(arguments).unwrap_or_else(|_| serde_json::json!({}));
        let query = args["query"].as_str().unwrap_or("").trim().to_string();
        if query.is_empty() {
            return ("(empty query - nothing searched)".into(), 0);
        }
        if sources.len() >= 24 {
            return (
                "Source limit reached; answer with what you already have.".into(),
                0,
            );
        }
        let limit = args["limit"].as_u64().unwrap_or(6).clamp(1, 8) as usize;
        let details = self
            .retrieve_search(&query, limit)
            .await
            .unwrap_or_default();

        let mut block = String::new();
        let mut added = 0;
        for d in details {
            if sources.iter().any(|s| s.id == d.id) {
                continue; // already cited under an earlier number
            }
            block.push_str(&ai::format_excerpt(sources.len() + 1, &d));
            sources.push(d);
            added += 1;
            if sources.len() >= 24 {
                break;
            }
        }
        let text = if added == 0 {
            format!("No new results for \"{query}\".")
        } else {
            format!("Results for \"{query}\":\n\n{block}")
        };
        (text, added)
    }

    fn emit_ask_citations(&self, request_id: &str, sources: &[MessageDetail]) {
        self.bus.emit(CoreEvent::AskCitations {
            request_id: request_id.to_string(),
            citations: Self::ask_citations(sources),
        });
    }

    fn ask_citations(sources: &[MessageDetail]) -> Vec<AskCitation> {
        sources
            .iter()
            .map(|d| AskCitation {
                message_id: d.id,
                thread_id: d.thread_id,
                subject: d.subject.clone(),
                from: d.from.name.clone().unwrap_or_else(|| d.from.email.clone()),
                date: d.date,
                snippet: d.snippet.clone(),
            })
            .collect()
    }

    pub async fn list_snippets(&self) -> Result<Vec<Snippet>> {
        self.db.read(|conn| repo::snippets::list(conn)).await
    }

    pub async fn save_snippet(
        &self,
        id: Option<i64>,
        name: String,
        shortcut: Option<String>,
        subject: Option<String>,
        body_text: String,
    ) -> Result<Snippet> {
        self.db
            .write(move |conn| {
                repo::snippets::save(
                    conn,
                    id,
                    &name,
                    shortcut.as_deref(),
                    subject.as_deref(),
                    &body_text,
                )
            })
            .await
    }

    pub async fn delete_snippet(&self, id: i64) -> Result<()> {
        self.db
            .write(move |conn| repo::snippets::delete(conn, id))
            .await
    }

    pub async fn use_snippet(&self, id: i64) -> Result<()> {
        self.db
            .write(move |conn| repo::snippets::bump_usage(conn, id))
            .await
    }

    pub async fn list_splits(&self) -> Result<Vec<SplitRule>> {
        self.db.read(|conn| repo::splits::list(conn)).await
    }

    /// First split rule (in position order) that matches a thread, if any.
    /// `threads.routed_tab` can't answer this for rules with a `target` (they
    /// write the target's route key, not `split:<id>`), so re-run the matcher.
    pub async fn find_matching_split(&self, thread_id: i64) -> Result<Option<SplitRule>> {
        self.db
            .read(move |conn| {
                for rule in repo::splits::list(conn)? {
                    if route::split_matches(conn, thread_id, &rule.query)? {
                        return Ok(Some(rule));
                    }
                }
                Ok(None)
            })
            .await
    }

    pub async fn save_split(
        &self,
        id: Option<i64>,
        name: String,
        position: i64,
        query: SplitRuleQuery,
        target: Option<String>,
    ) -> Result<SplitRule> {
        let saved = self
            .db
            .write(move |conn| {
                repo::splits::save(conn, id, &name, position, &query, target.as_deref())
            })
            .await?;
        // Rules changed: re-resolve every thread and drop the AI cache so edited
        // routing takes effect. AI (if any) runs in the background afterwards.
        self.reroute_all_sync().await?;
        Ok(saved)
    }

    pub async fn delete_split(&self, id: i64) -> Result<()> {
        self.db
            .write(move |conn| repo::splits::delete(conn, id))
            .await?;
        // A deleted rule's threads must fall back to the AI/heuristic/defaults.
        self.reroute_all_sync().await?;
        Ok(())
    }

    /// Persist a single shared tab order across custom splits and auto-label
    /// tabs. `order` lists every reorderable tab top-to-bottom as `(kind, id)`
    /// where `kind` is `"split"` or `"label"`; its index becomes the row's
    /// `position`. Built-in Important/Other are pinned and never included.
    ///
    /// Only re-resolves routing when the splits' relative priority actually
    /// changed (moving an auto-label around can't affect first-match-wins), so a
    /// pure label move stays cheap.
    pub async fn reorder_tabs(&self, order: Vec<(String, i64)>) -> Result<()> {
        let split_order_changed = self
            .db
            .write(move |conn| {
                let tx = conn.transaction()?;
                let split_ids = |tx: &rusqlite::Transaction| -> rusqlite::Result<Vec<i64>> {
                    tx.prepare("SELECT id FROM split_rules ORDER BY position, id")?
                        .query_map([], |r| r.get(0))?
                        .collect()
                };
                let before = split_ids(&tx)?;
                for (i, (kind, id)) in order.iter().enumerate() {
                    let pos = i as i64;
                    match kind.as_str() {
                        "split" => tx.execute(
                            "UPDATE split_rules SET position = ?2 WHERE id = ?1",
                            rusqlite::params![id, pos],
                        )?,
                        "label" => tx.execute(
                            "UPDATE labels SET position = ?2 WHERE id = ?1",
                            rusqlite::params![id, pos],
                        )?,
                        _ => 0,
                    };
                }
                let after = split_ids(&tx)?;
                tx.commit()?;
                Ok(before != after)
            })
            .await?;
        if split_order_changed {
            self.reroute_all_sync().await?;
        } else {
            self.bus.emit(CoreEvent::MailUpdated { thread_ids: vec![] });
        }
        Ok(())
    }

    pub async fn list_labels(&self) -> Result<Vec<Label>> {
        self.db.read(|conn| repo::labels::list(conn)).await
    }

    /// Explicitly place one thread in a built-in or automatic category. This
    /// is used by native drag-and-drop targets; `apply_tab` also keeps the
    /// visible auto-label chip in sync with the exclusive route.
    pub async fn route_thread_to_tab(&self, thread_id: i64, target: String) -> Result<()> {
        self.db
            .write(move |conn| {
                let valid = match target.as_str() {
                    "important" | "other" => true,
                    value if value.starts_with("label:") => {
                        let Some(id) = value
                            .strip_prefix("label:")
                            .and_then(|id| id.parse::<i64>().ok())
                        else {
                            return Err(CoreError::Other(
                                "invalid mail category destination".into(),
                            ));
                        };
                        repo::labels::get(conn, id)?.is_some_and(|label| label.is_auto)
                    }
                    _ => false,
                };
                if !valid {
                    return Err(CoreError::Other("invalid mail category destination".into()));
                }
                route::apply_tab(conn, thread_id, Some(&target))
            })
            .await?;
        self.bus.emit(CoreEvent::MailUpdated {
            thread_ids: vec![thread_id],
        });
        Ok(())
    }

    pub async fn save_label(
        &self,
        id: Option<i64>,
        name: String,
        color: String,
        position: i64,
    ) -> Result<Label> {
        let (label, accounts) = self
            .db
            .write(move |conn| {
                let tx = conn.transaction()?;
                let label = repo::labels::save(&tx, id, &name, &color, position)?;
                let mappings = {
                    let mut stmt = tx.prepare(
                        "SELECT account_id, provider_id FROM gmail_labels
                         WHERE local_label_id = ?1",
                    )?;
                    stmt.query_map(rusqlite::params![label.id], |row| {
                        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?
                };
                let mut accounts = Vec::new();
                for (account_id, provider_id) in mappings {
                    repo::actions::enqueue(
                        &tx,
                        account_id,
                        "gmail_label_update",
                        None,
                        None,
                        &serde_json::json!({
                            "providerId": provider_id,
                            "name": label.name,
                            "color": label.color,
                        }),
                        None,
                    )?;
                    accounts.push(account_id);
                }
                tx.commit()?;
                Ok((label, accounts))
            })
            .await?;
        for account_id in accounts {
            self.nudge(Some(account_id), || SyncCmd::RunActions).await;
        }
        Ok(label)
    }

    pub async fn delete_label(&self, id: i64) -> Result<()> {
        let accounts = self
            .db
            .write(move |conn| {
                let tx = conn.transaction()?;
                let mappings = {
                    let mut stmt = tx.prepare(
                        "SELECT account_id, provider_id FROM gmail_labels
                         WHERE local_label_id = ?1",
                    )?;
                    stmt.query_map(rusqlite::params![id], |row| {
                        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?
                };
                for (account_id, provider_id) in &mappings {
                    repo::actions::enqueue(
                        &tx,
                        *account_id,
                        "gmail_label_delete",
                        None,
                        None,
                        &serde_json::json!({ "providerId": provider_id }),
                        None,
                    )?;
                }
                repo::labels::delete(&tx, id)?;
                tx.commit()?;
                Ok(mappings
                    .into_iter()
                    .map(|(account_id, _)| account_id)
                    .collect::<Vec<_>>())
            })
            .await?;
        for account_id in accounts {
            self.nudge(Some(account_id), || SyncCmd::RunActions).await;
        }
        Ok(())
    }

    pub async fn restore_auto_labels(&self) -> Result<i64> {
        let restored = self
            .db
            .write(|conn| repo::labels::restore_auto_defaults(conn))
            .await?;
        if restored > 0 {
            self.reroute_all_sync().await?;
        }
        Ok(restored)
    }

    /// Re-run routing over all stored mail. Legacy alias kept for the existing
    /// "Relabel" action; delegates to [`Core::reroute_all`].
    /// Re-resolve every thread's tab from scratch and drain the AI queue, so the
    /// caller sees the final state. Used by the explicit "Re-sort" action and the
    /// one-shot startup backfill.
    pub async fn reroute_all(&self) -> Result<i64> {
        let n = self.reroute_all_sync().await?;
        // Drain the AI queue now so the caller sees the final state.
        while self.classify_pending(200).await? > 0 {}
        Ok(n)
    }

    /// Deterministic re-route only (rules + heuristic, or mark 'pending'); the
    /// background router handles the AI pass. Fast enough to run on every rule
    /// edit without blocking on model calls.
    async fn reroute_all_sync(&self) -> Result<i64> {
        let n = self
            .db
            .write(|conn| {
                let tx = conn.transaction()?;
                // Keep the one-shot backfill marker; drop real sender decisions
                // so an edited prompt/rules re-evaluates.
                tx.execute(
                    "DELETE FROM route_cache WHERE sender_domain <> '__routing_backfill__'",
                    [],
                )?;
                tx.execute(
                    "DELETE FROM message_labels WHERE label_id IN
                     (SELECT id FROM labels WHERE is_auto = 1)",
                    [],
                )?;
                tx.execute("UPDATE threads SET routed_tab = NULL", [])?;
                tx.execute(
                    "UPDATE messages SET local_subject_prefix = '', local_body_note = ''",
                    [],
                )?;
                let settings = repo::settings::get(&tx)?;
                let mut n = 0i64;
                if settings.auto_labels_enabled {
                    let splits = repo::splits::list(&tx)?;
                    let ids: Vec<i64> = {
                        let mut stmt = tx.prepare("SELECT id FROM threads")?;

                        stmt.query_map([], |r| r.get(0))?
                            .collect::<rusqlite::Result<Vec<_>>>()?
                    };
                    for id in ids {
                        route::route_thread_deterministic(
                            &tx,
                            &splits,
                            settings.ai_categorize,
                            id,
                        )?;
                        n += 1;
                    }
                }
                tx.commit()?;
                Ok(n)
            })
            .await?;
        // Routing changed across many threads; a blanket refresh is fine here.
        // The emit also wakes the background AI router to handle any 'pending'.
        self.bus.emit(CoreEvent::MailUpdated { thread_ids: vec![] });
        Ok(n)
    }

    /// Classify up to `limit` pending threads. With no custom automations this
    /// preserves the low-cost sender-domain category cache. Compound rules are
    /// evaluated per message (their conditions may depend on subject/body), and
    /// only their preconfigured action allow-lists are executed.
    pub async fn classify_pending(&self, limit: i64) -> Result<i64> {
        let _guard = self.ai_router_lock.lock().await;
        let settings = self.db.read(|conn| repo::settings::get(conn)).await?;
        if !settings.ai_categorize {
            return Ok(0);
        }
        let cfg = match self.ai_config_from(&settings, Scenario::Categorize).await {
            Ok(c) => c,
            // No key: leave threads 'pending' (still shown in Important/Other).
            Err(_) => return Ok(0),
        };

        let (jobs, cache) = self
            .db
            .read(move |conn| {
                Ok((
                    route::pending_threads(conn, limit)?,
                    route::load_cache(conn)?,
                ))
            })
            .await?;
        if jobs.is_empty() {
            return Ok(0);
        }

        let prompt = settings.ai_category_prompt.clone();
        let rules: Vec<AiAutomationRule> = settings
            .ai_automation_rules
            .iter()
            .filter(|rule| rule.enabled && !rule.actions.is_empty())
            .cloned()
            .collect();
        let compound = !rules.is_empty();
        // Resolved (thread id, category keyword, configured actions).
        let mut resolved: Vec<(i64, Option<String>, Vec<AiAutomationAction>)> = Vec::new();
        let mut decided: std::collections::HashMap<String, Option<autolabel::Category>> =
            std::collections::HashMap::new();
        let mut cache_writes: Vec<(String, String)> = Vec::new();

        for (tid, facts) in jobs {
            let domain = route::sender_domain(&facts.from_addr);
            if compound {
                let msgs = route::automation_prompt(
                    &prompt,
                    &rules,
                    &facts.from_addr,
                    &facts.subject,
                    &facts.snippet,
                );
                match ai::chat(&cfg, msgs).await {
                    Ok(out) => {
                        let decision = route::parse_automation_decision(&out);
                        let selected: std::collections::HashSet<&str> =
                            decision.rule_ids.iter().map(String::as_str).collect();
                        // Preserve the user's rule/action order, not the model's
                        // output order, so compound behavior is deterministic.
                        let actions = rules
                            .iter()
                            .filter(|rule| selected.contains(rule.id.as_str()))
                            .flat_map(|rule| rule.actions.iter().cloned())
                            .collect();
                        resolved.push((
                            tid,
                            decision.category.map(|c| c.keyword().to_string()),
                            actions,
                        ));
                    }
                    Err(e) => {
                        tracing::warn!("ai automation failed for thread {tid}: {e}");
                    }
                }
                continue;
            }

            let category: Option<autolabel::Category> = if let Some(hit) = cache.get(&domain) {
                // Cache stores the category keyword, "" = no category.
                autolabel::Category::from_keyword(hit)
            } else if let Some(d) = decided.get(&domain) {
                *d
            } else {
                let msgs = route::category_prompt(
                    &prompt,
                    &facts.from_addr,
                    &facts.subject,
                    &facts.snippet,
                );
                match ai::chat(&cfg, msgs).await {
                    Ok(out) => {
                        let cat = route::parse_category(&out);
                        decided.insert(domain.clone(), cat);
                        // Persist the decision keyed by domain ("" = no category).
                        let key = cat.map(|c| c.keyword().to_string()).unwrap_or_default();
                        cache_writes.push((domain.clone(), key));
                        cat
                    }
                    // Transient failure: leave this thread 'pending' so it retries
                    // on the next pass, and don't poison the cache for the sender.
                    Err(e) => {
                        tracing::warn!("ai categorize failed for thread {tid}: {e}");
                        continue;
                    }
                }
            };
            resolved.push((tid, category.map(|c| c.keyword().to_string()), Vec::new()));
        }

        let action_plans: Vec<(i64, Vec<AiAutomationAction>)> = resolved
            .iter()
            .map(|(tid, _, actions)| (*tid, actions.clone()))
            .collect();
        // One write pass: apply category/tab routing and local annotations.
        let count = self
            .db
            .write(move |conn| {
                use rusqlite::OptionalExtension;
                for (domain, key) in &cache_writes {
                    route::cache_put(conn, domain, key)?;
                }
                let mut count = 0i64;
                for (tid, kw, actions) in &resolved {
                    let key = match kw {
                        Some(k) => {
                            let id: Option<i64> = conn
                                .query_row(
                                    "SELECT id FROM labels WHERE keyword = ?1 AND is_auto = 1",
                                    rusqlite::params![k],
                                    |r| r.get(0),
                                )
                                .optional()?;
                            id.map(|id| format!("label:{id}"))
                        }
                        None => None,
                    };
                    route::apply_tab(conn, *tid, key.as_deref())?;

                    let mut prefixes = Vec::new();
                    let mut notes = Vec::new();
                    for action in actions {
                        match action.kind.as_str() {
                            "route_to" if valid_automation_route(conn, &action.value)? => {
                                route::apply_tab(conn, *tid, Some(&action.value))?;
                            }
                            "subject_prefix" if !action.value.trim().is_empty() => {
                                prefixes.push(action.value.as_str());
                            }
                            "body_note" if !action.value.trim().is_empty() => {
                                notes.push(action.value.trim());
                            }
                            _ => {}
                        }
                    }
                    if !prefixes.is_empty() || !notes.is_empty() {
                        conn.execute(
                            "UPDATE messages
                             SET local_subject_prefix = ?2, local_body_note = ?3
                             WHERE id = (SELECT id FROM messages
                                         WHERE thread_id = ?1 AND is_outgoing = 0 AND is_draft = 0
                                         ORDER BY date DESC LIMIT 1)",
                            rusqlite::params![tid, prefixes.concat(), notes.join("\n\n")],
                        )?;
                    }
                    count += 1;
                }
                Ok(count)
            })
            .await?;

        // Threads that just gained a label may now satisfy a label-based split
        // rule, so re-check their split routing once the labels are written.
        let relabeled: Vec<i64> = action_plans
            .iter()
            .filter(|(_, actions)| actions.iter().any(|a| a.kind == "add_label"))
            .map(|(tid, _)| *tid)
            .collect();

        // Mailbox mutations reuse the normal optimistic + queued action path,
        // so IMAP state, retries, and UI refreshes behave exactly like a click.
        for (thread_id, actions) in action_plans {
            for action in actions {
                let (kind, params) = match action.kind.as_str() {
                    "mark_read" => (Some(ActionKind::MarkRead), None),
                    "star" => (Some(ActionKind::Star), None),
                    "archive" => (Some(ActionKind::Archive), None),
                    "trash" => (Some(ActionKind::Trash), None),
                    "add_label" | "remove_label" => {
                        let Some(label_id) = action.value.parse::<i64>().ok() else {
                            continue;
                        };
                        let kind = if action.kind == "add_label" {
                            ActionKind::AddLabel
                        } else {
                            ActionKind::RemoveLabel
                        };
                        (
                            Some(kind),
                            Some(ActionParams {
                                wake_at: None,
                                target_folder_id: None,
                                label_id: Some(label_id),
                            }),
                        )
                    }
                    _ => (None, None),
                };
                let Some(kind) = kind else { continue };
                if let Err(e) = self
                    .perform_action(PerformActionArgs {
                        kind,
                        thread_ids: vec![thread_id],
                        params,
                    })
                    .await
                {
                    tracing::warn!(
                        "automation action {} failed for thread {thread_id}: {e}",
                        action.kind
                    );
                }
            }
        }
        if !relabeled.is_empty() {
            self.db
                .write(move |conn| {
                    let splits = repo::splits::list(conn)?;
                    for tid in relabeled {
                        route::reapply_splits_only(conn, &splits, tid)?;
                    }
                    Ok(())
                })
                .await?;
        }
        self.bus.emit(CoreEvent::MailUpdated { thread_ids: vec![] });
        Ok(count)
    }

    /// Exact unread counts for every split tab and sidebar row in one call.
    pub async fn unread_counts(&self, account_id: Option<i64>) -> Result<UnreadCounts> {
        self.db
            .read(move |conn| {
                let splits = repo::splits::list(conn)?;
                let labels = repo::labels::list(conn)?;
                repo::counts::unread_counts(conn, account_id, &splits, &labels)
            })
            .await
    }

    /// Native sidebar badges for all accounts, computed in one grouped query.
    pub async fn mailbox_badge_counts(&self) -> Result<Vec<MailboxBadgeCounts>> {
        self.db
            .read(|conn| repo::counts::mailbox_badge_counts(conn))
            .await
    }

    pub async fn get_settings(&self) -> Result<Settings> {
        self.db.read(|conn| repo::settings::get(conn)).await
    }

    pub async fn set_settings(&self, settings: Settings) -> Result<()> {
        self.db
            .write(move |conn| {
                repo::settings::set(conn, &settings)?;
                // Keep the resolver in the same order as persisted writes.
                apply_oauth_settings(&settings);
                Ok(())
            })
            .await
    }

    /// Persist one or both OAuth registrations in a single settings write.
    /// Read on the writer thread so unrelated preferences aren't overwritten
    /// by a stale settings snapshot while this operation waits for the DB.
    pub async fn set_oauth_apps(
        &self,
        google: Option<(String, String)>,
        microsoft: Option<String>,
    ) -> Result<Settings> {
        self.db
            .write(move |conn| {
                let mut settings = repo::settings::get(conn)?;
                if let Some((id, secret)) = google {
                    settings.google_client_id = id.trim().to_owned();
                    settings.google_client_secret = if settings.google_client_id.is_empty() {
                        String::new()
                    } else {
                        secret.trim().to_owned()
                    };
                }
                if let Some(id) = microsoft {
                    settings.ms_client_id = id.trim().to_owned();
                    // Public desktop clients use PKCE, without a secret.
                    settings.ms_client_secret.clear();
                }
                repo::settings::set(conn, &settings)?;
                apply_oauth_settings(&settings);
                Ok(settings)
            })
            .await
    }

    pub async fn set_sync_interval_minutes(&self, minutes: i64) -> Result<()> {
        if !matches!(minutes, 1 | 5 | 15) {
            return Err(CoreError::Other(format!(
                "unsupported sync interval: {minutes} minutes"
            )));
        }
        let mut settings = self.get_settings().await?;
        settings.sync_interval_minutes = minutes;
        self.set_settings(settings).await?;

        // Wake every live provider so the newly persisted cadence takes effect
        // now instead of after its previous timeout expires.
        for handle in self.handles.read().await.values() {
            handle.send(SyncCmd::SyncNow { complete: None });
        }
        for handle in self.cal_handles.read().await.values() {
            handle.nudge();
        }
        Ok(())
    }

    /// Return notifications that are ready for the native host to dispatch.
    /// Eligibility is decided by sync before enqueueing; this API only exposes
    /// durable delivery state to the host process.
    pub async fn due_notifications(&self, limit: i64) -> Result<Vec<NotificationOutboxItem>> {
        let now = now_ms();
        let limit = limit.clamp(1, 100);
        self.db
            .read(move |conn| repo::notifications::list_due(conn, now, limit))
            .await
    }

    /// Resolve which inbox tab a thread lands in, for notification-scope
    /// filtering. `RoutedTab::Pending` means AI classification is still in
    /// flight and the host should wait before deciding.
    pub async fn notification_thread_tab(&self, thread_id: i64) -> Result<Option<RoutedTab>> {
        self.db
            .read(move |conn| repo::notifications::resolve_tab(conn, thread_id))
            .await
    }

    /// Defer a pending notification's next dispatch without consuming a delivery
    /// attempt, used while waiting for its thread's tab to resolve.
    pub async fn defer_notification_delivery(&self, id: i64, not_before: i64) -> Result<bool> {
        self.db
            .write(move |conn| repo::notifications::defer(conn, id, not_before))
            .await
    }

    /// Atomically claim one due notification. A false result means another
    /// dispatcher or a state transition won the race.
    pub async fn claim_notification_delivery(&self, id: i64) -> Result<bool> {
        let now = now_ms();
        self.db
            .write(move |conn| repo::notifications::try_claim(conn, id, now))
            .await
    }

    pub async fn mark_notification_delivered(&self, id: i64) -> Result<bool> {
        let now = now_ms();
        self.db
            .write(move |conn| repo::notifications::mark_delivered(conn, id, now))
            .await
    }

    pub async fn suppress_notification_delivery(
        &self,
        id: i64,
        reason: impl Into<String>,
    ) -> Result<bool> {
        let now = now_ms();
        let reason = reason.into();
        self.db
            .write(move |conn| repo::notifications::mark_suppressed(conn, id, now, &reason))
            .await
    }

    /// Return a claimed notification to the pending queue after a bounded
    /// delay. The cap prevents a bad caller from making a row disappear for an
    /// unreasonable amount of time.
    pub async fn retry_notification_delivery(
        &self,
        id: i64,
        delay_ms: i64,
        error: impl Into<String>,
    ) -> Result<bool> {
        const MAX_DELAY_MS: i64 = 24 * 60 * 60 * 1_000;
        let retry_at = now_ms().saturating_add(delay_ms.clamp(0, MAX_DELAY_MS));
        let error = error.into();
        self.db
            .write(move |conn| repo::notifications::retry(conn, id, retry_at, &error))
            .await
    }

    /// Recover rows left in `delivering` by a process crash. Native delivery is
    /// necessarily at-least-once because the OS send and SQLite commit cannot
    /// share a transaction.
    pub async fn recover_notification_deliveries(&self) -> Result<usize> {
        let now = now_ms();
        self.db
            .write(move |conn| repo::notifications::recover_delivering(conn, now))
            .await
    }
}

/// Build a (incoming → the user's reply) example from one of their sent
/// messages: the reply is its body, the incoming side is the message it
/// replied to (the prior message in its thread), or a synthetic prompt if it
/// started the thread. Returns None if the sent body is empty.
fn build_example_pair(
    conn: &rusqlite::Connection,
    sent_id: i64,
) -> Result<Option<(String, String)>> {
    let sent = repo::messages::detail(conn, sent_id)?;
    let reply = sent.text_body.clone().unwrap_or_default();
    if reply.trim().is_empty() {
        return Ok(None);
    }
    let msgs = repo::messages::list_for_thread(conn, sent.thread_id)?;
    let mut incoming: Option<&MessageDetail> = None;
    for m in &msgs {
        if m.id == sent_id {
            break;
        }
        incoming = Some(m);
    }
    let incoming_text = match incoming {
        Some(m) => {
            let body = m.text_body.clone().unwrap_or_else(|| m.snippet.clone());
            format!("Subject: {}\nFrom: {}\n{}", m.subject, m.from.email, body)
        }
        None => format!("(Compose a new email. Subject: {})", sent.subject),
    };
    Ok(Some((incoming_text, reply)))
}

/// Locate model files bundled by a native host. The host sets
/// `FLECTAR_MAIL_RESOURCE_DIR` to the platform-specific resource directory.
#[cfg(feature = "local-embeddings")]
fn bundled_model_dir(key: &str) -> Option<std::path::PathBuf> {
    let base = std::env::var_os("FLECTAR_MAIL_RESOURCE_DIR")?;
    let dir = std::path::PathBuf::from(base).join("models").join(key);
    dir.join("model.safetensors").exists().then_some(dir)
}

#[cfg(feature = "local-embeddings")]
async fn copy_model_files(
    src: &std::path::Path,
    dst: &std::path::Path,
    spec: &'static embed::ModelSpec,
) -> Result<()> {
    let verified_source = src.to_path_buf();
    tokio::task::spawn_blocking(move || embed::verify_model_files(&verified_source, spec))
        .await
        .map_err(|error| CoreError::Other(format!("bundled model verification task: {error}")))??;

    let parent = dst
        .parent()
        .ok_or_else(|| CoreError::Other("bundled model destination has no parent".into()))?;
    tokio::fs::create_dir_all(parent).await?;
    let nonce = rand::random::<u64>();
    let staging = parent.join(format!(".{}-{nonce:016x}.bundled", spec.key));
    let backup = parent.join(format!(".{}-{nonce:016x}.replaced", spec.key));
    tokio::fs::create_dir(&staging).await?;
    for artifact in spec.artifacts {
        if let Err(error) = crate::file_io::copy(
            src.join(artifact.filename),
            staging.join(artifact.filename),
            usize::try_from(artifact.bytes).unwrap_or(usize::MAX),
            "bundled model artifact",
        )
        .await
        {
            let _ = tokio::fs::remove_dir_all(&staging).await;
            return Err(error);
        }
    }
    let verified_staging = staging.clone();
    let verification = match tokio::task::spawn_blocking(move || {
        embed::verify_model_files(&verified_staging, spec)
    })
    .await
    {
        Ok(result) => result,
        Err(error) => Err(CoreError::Other(format!(
            "bundled model verification task: {error}"
        ))),
    };
    if let Err(error) = verification {
        let _ = tokio::fs::remove_dir_all(&staging).await;
        return Err(error);
    }

    let had_existing = tokio::fs::symlink_metadata(dst).await.is_ok();
    if had_existing && let Err(error) = tokio::fs::rename(dst, &backup).await {
        let _ = tokio::fs::remove_dir_all(&staging).await;
        return Err(error.into());
    }
    if let Err(error) = tokio::fs::rename(&staging, dst).await {
        if had_existing {
            let _ = tokio::fs::rename(&backup, dst).await;
        }
        let _ = tokio::fs::remove_dir_all(&staging).await;
        return Err(error.into());
    }
    if had_existing {
        let _ = tokio::fs::remove_dir_all(backup).await;
    }
    Ok(())
}

/// Reduce an untrusted filename to a single, benign path component: strip
/// separators/NUL/control chars, leading dots and spaces, and cap the length.
/// A file extension for a MIME type, used to name an on-disk attachment when
/// the message gave it no filename. Without a recognizable extension the OS
/// can't route "Open in app" to the right handler (e.g. a `text/calendar`
/// invite written as a bare `attachment-3` never reaches the calendar app).
fn ext_for_mime(mime: &str) -> Option<&'static str> {
    Some(
        match mime
            .split(';')
            .next()
            .unwrap_or(mime)
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "text/calendar" => "ics",
            "application/pdf" => "pdf",
            "text/plain" => "txt",
            "text/html" => "html",
            "text/csv" => "csv",
            "application/json" => "json",
            "application/zip" => "zip",
            "image/png" => "png",
            "image/jpeg" => "jpg",
            "image/gif" => "gif",
            "image/webp" => "webp",
            "image/svg+xml" => "svg",
            "message/rfc822" => "eml",
            _ => return None,
        },
    )
}

fn safe_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c == '/' || c == '\\' || c == '\0' || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches(['.', ' ']);
    let base = if trimmed.is_empty() {
        "attachment"
    } else {
        trimmed
    };
    base.chars().take(200).collect()
}

/// Copy a composer-picked file into the app-managed draft-attachment staging
/// area and return the staged absolute path. Only a path already owned by this
/// same saved draft is reused; another draft's staged file is copied so later
/// replacement cannot delete a shared path. This guarantees dispatch only ever
/// reads files the app itself wrote, closing an arbitrary local-file
/// read/exfiltration path through the `save_draft` API.
async fn stage_draft_attachment(
    root: &std::path::Path,
    src: &str,
    filename: &str,
    max_bytes: usize,
    reusable_paths: &HashSet<std::path::PathBuf>,
) -> Result<(String, usize, bool)> {
    tokio::fs::create_dir_all(root).await?;
    let root = tokio::fs::canonicalize(root).await?;
    let canon_src = tokio::fs::canonicalize(src)
        .await
        .map_err(|e| CoreError::Other(format!("attachment {filename}: {e}")))?;
    if canon_src.starts_with(&root) && reusable_paths.contains(&canon_src) {
        // Already staged (e.g. re-saving a draft reloaded from the DB).
        let size = tokio::fs::metadata(&canon_src).await?.len();
        let size = usize::try_from(size).map_err(|_| {
            CoreError::Other(format!(
                "attachment {filename} is too large for this platform"
            ))
        })?;
        if size > max_bytes {
            return Err(CoreError::Other(format!(
                "draft attachments exceed the {} MiB safety limit",
                MAX_DRAFT_ATTACHMENT_BYTES / (1024 * 1024)
            )));
        }
        return Ok((canon_src.to_string_lossy().into_owned(), size, false));
    }
    let sub = root.join(crate::mime::rand_token());
    tokio::fs::create_dir_all(&sub).await?;
    let dst = sub.join(safe_filename(filename));
    let size = match crate::file_io::copy(&canon_src, &dst, max_bytes, "draft attachment").await {
        Ok(size) => size,
        Err(error) => {
            let _ = tokio::fs::remove_dir(&sub).await;
            return Err(error);
        }
    };
    Ok((dst.to_string_lossy().into_owned(), size, true))
}

pub(crate) async fn remove_staged_attachment(root: &std::path::Path, path: &str) {
    let (Ok(root), Ok(path)) = (
        tokio::fs::canonicalize(root).await,
        tokio::fs::canonicalize(path).await,
    ) else {
        return;
    };
    if !path.starts_with(&root) || path == root {
        return;
    }
    let parent = path.parent().map(std::path::Path::to_path_buf);
    let _ = tokio::fs::remove_file(&path).await;
    if let Some(parent) = parent.filter(|parent| parent.starts_with(&root) && parent != &root) {
        let _ = tokio::fs::remove_dir(parent).await;
    }
}

/// Plain-text body for an outgoing invite email (the ICS carries the real
/// event; this is what non-calendar clients show).
fn invite_body_text(args: &CreateEventArgs) -> String {
    use chrono::TimeZone;
    let fmt = |ms: i64| {
        chrono::Local
            .timestamp_millis_opt(ms)
            .earliest()
            .map(|dt| {
                if args.all_day {
                    dt.format("%a, %b %e, %Y").to_string()
                } else {
                    dt.format("%a, %b %e, %Y at %H:%M").to_string()
                }
            })
            .unwrap_or_default()
    };
    let mut out = format!(
        "You are invited: {}\n\nWhen: {} - {}\n",
        args.summary.trim(),
        fmt(args.starts_at),
        fmt(args.ends_at)
    );
    if let Some(loc) = args.location.as_deref().filter(|l| !l.is_empty()) {
        out.push_str(&format!("Where: {loc}\n"));
    }
    if let Some(url) = args.join_url.as_deref().filter(|u| !u.is_empty()) {
        out.push_str(&format!("Join: {url}\n"));
    }
    if let Some(desc) = args.description.as_deref().filter(|d| !d.is_empty()) {
        out.push_str(&format!("\n{desc}\n"));
    }
    out
}

/// Push user-entered OAuth app registrations into the resolver.
fn apply_oauth_settings(settings: &Settings) {
    oauth::providers::set_configured(
        Provider::Gmail,
        &settings.google_client_id,
        &settings.google_client_secret,
    );
    oauth::providers::set_configured(
        Provider::Microsoft,
        &settings.ms_client_id,
        &settings.ms_client_secret,
    );
}

/// Validate a model-selected route against current local rows. The model only
/// receives configured values, but this protects stale/deleted targets too.
fn valid_automation_route(conn: &rusqlite::Connection, target: &str) -> Result<bool> {
    if matches!(target, "important" | "other") {
        return Ok(true);
    }
    let (table, id) = if let Some(id) = target.strip_prefix("split:") {
        ("split_rules", id)
    } else if let Some(id) = target.strip_prefix("label:") {
        ("labels", id)
    } else {
        return Ok(false);
    };
    let Some(id) = id.parse::<i64>().ok() else {
        return Ok(false);
    };
    let sql = format!("SELECT EXISTS (SELECT 1 FROM {table} WHERE id = ?1)");
    Ok(conn.query_row(&sql, rusqlite::params![id], |row| row.get::<_, i64>(0))? != 0)
}

/// Optimistic local mutation + enqueue, in one transaction.
/// Returns (action_id, account_id) pairs.
fn apply_thread_action(
    conn: &mut rusqlite::Connection,
    thread_id: i64,
    kind: ActionKind,
    params: Option<&ActionParams>,
) -> Result<Vec<(i64, i64)>> {
    let tx = conn.transaction()?;
    let mut out: Vec<(i64, i64)> = Vec::new();

    let (account_id, account_provider, mail_protocol): (i64, String, String) = tx.query_row(
        "SELECT t.account_id, a.provider, a.mail_protocol
         FROM threads t JOIN accounts a ON a.id = t.account_id
         WHERE t.id = ?1",
        rusqlite::params![thread_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let gmail = account_provider == "gmail";
    let multi_mailbox = gmail || mail_protocol == "jmap";

    // Most thread actions intentionally ignore drafts. Archive and Trash are
    // exceptions: users can file or discard a draft from the Drafts view, and
    // excluding it here made the optimistic removal snap back on reconciliation.
    let include_drafts = matches!(kind, ActionKind::Archive | ActionKind::Trash);
    let mut stmt = tx.prepare(
        "SELECT m.id, m.folder_id, m.uid, m.is_read, m.is_starred, COALESCE(f.role,'')
         FROM messages m LEFT JOIN folders f ON f.id = m.folder_id
         WHERE m.thread_id = ?1 AND (m.is_draft = 0 OR ?2 = 1)",
    )?;
    #[allow(clippy::type_complexity)]
    let msgs: Vec<(i64, Option<i64>, Option<i64>, bool, bool, String)> = stmt
        .query_map(rusqlite::params![thread_id, include_drafts as i64], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get::<_, i64>(3)? != 0,
                r.get::<_, i64>(4)? != 0,
                r.get(5)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);

    fn folder_of(tx: &rusqlite::Transaction, account_id: i64, role: &str) -> Result<Option<i64>> {
        Ok(repo::folders::by_role(tx, account_id, role)?.map(|f| f.id))
    }

    #[allow(clippy::too_many_arguments)]
    fn enqueue_move(
        tx: &rusqlite::Transaction,
        account_id: i64,
        thread_id: i64,
        msg_id: i64,
        src_folder: Option<i64>,
        src_uid: Option<i64>,
        target: i64,
        kind_str: &str,
        multi_mailbox: bool,
    ) -> Result<i64> {
        let payload = serde_json::json!({
            "srcFolderId": src_folder,
            "srcUid": src_uid,
            "targetFolderId": target,
        });
        repo::messages::set_uid_and_folder(tx, msg_id, target, None)?;
        if multi_mailbox {
            if let Some(src_folder) = src_folder {
                tx.execute(
                    "DELETE FROM message_folders WHERE message_id = ?1 AND folder_id = ?2",
                    rusqlite::params![msg_id, src_folder],
                )?;
            }
            tx.execute(
                "INSERT OR IGNORE INTO message_folders (message_id, folder_id)
                 VALUES (?1, ?2)",
                rusqlite::params![msg_id, target],
            )?;
        }
        let aid = repo::actions::enqueue(
            tx,
            account_id,
            kind_str,
            Some(msg_id),
            Some(thread_id),
            &payload,
            None,
        )?;
        Ok(aid)
    }

    match kind {
        ActionKind::MarkRead | ActionKind::MarkUnread => {
            let target_read = kind == ActionKind::MarkRead;
            for (id, _f, _u, is_read, _s, _role) in &msgs {
                if *is_read != target_read {
                    repo::messages::set_read(&tx, *id, target_read)?;
                    let payload = serde_json::json!({});
                    let aid = repo::actions::enqueue(
                        &tx,
                        account_id,
                        kind.as_str(),
                        Some(*id),
                        Some(thread_id),
                        &payload,
                        None,
                    )?;
                    out.push((aid, account_id));
                }
            }
        }
        ActionKind::Star => {
            // Star the latest message only (thread-level star).
            if let Some((id, _f, _u, _r, is_starred, _role)) = msgs.iter().max_by_key(|m| m.0) {
                if *is_starred {
                    repo::threads::recompute(&tx, thread_id)?;
                    tx.commit()?;
                    return Ok(out);
                }
                repo::messages::set_starred(&tx, *id, true)?;
                let aid = repo::actions::enqueue(
                    &tx,
                    account_id,
                    "star",
                    Some(*id),
                    Some(thread_id),
                    &serde_json::json!({}),
                    None,
                )?;
                out.push((aid, account_id));
            }
        }
        ActionKind::Unstar => {
            for (id, _f, _u, _r, is_starred, _role) in &msgs {
                if *is_starred {
                    repo::messages::set_starred(&tx, *id, false)?;
                    let aid = repo::actions::enqueue(
                        &tx,
                        account_id,
                        "unstar",
                        Some(*id),
                        Some(thread_id),
                        &serde_json::json!({}),
                        None,
                    )?;
                    out.push((aid, account_id));
                }
            }
        }
        ActionKind::Archive | ActionKind::Trash | ActionKind::Spam => {
            let (target_role, kind_str) = match kind {
                ActionKind::Archive => (roles::ARCHIVE, "archive"),
                ActionKind::Trash => (roles::TRASH, "trash"),
                _ => (roles::SPAM, "spam"),
            };
            // Gmail-style fallback: archiving with no Archive folder moves to All Mail.
            let target = folder_of(&tx, account_id, target_role)?
                .or(if kind == ActionKind::Archive {
                    folder_of(&tx, account_id, roles::ALL)?
                } else {
                    None
                })
                .ok_or_else(|| CoreError::NotFound(format!("no {target_role} folder")))?;
            for (id, f, u, _r, _s, role) in &msgs {
                let movable = match kind {
                    ActionKind::Archive => role == roles::INBOX || role == roles::DRAFTS,
                    _ => role != target_role && !role.is_empty(),
                };
                if movable && f.is_some() {
                    let aid = enqueue_move(
                        &tx,
                        account_id,
                        thread_id,
                        *id,
                        *f,
                        *u,
                        target,
                        kind_str,
                        multi_mailbox,
                    )?;
                    out.push((aid, account_id));
                }
            }
            // Archiving also clears snooze.
            repo::snoozes::clear(&tx, thread_id)?;
        }
        ActionKind::Unarchive | ActionKind::NotSpam => {
            let target = folder_of(&tx, account_id, roles::INBOX)?
                .ok_or_else(|| CoreError::NotFound("no inbox folder".into()))?;
            let from_role = if kind == ActionKind::Unarchive {
                roles::ARCHIVE
            } else {
                roles::SPAM
            };
            for (id, f, u, _r, _s, role) in &msgs {
                if (role == from_role || (kind == ActionKind::Unarchive && role == roles::ALL))
                    && f.is_some()
                {
                    let aid = enqueue_move(
                        &tx,
                        account_id,
                        thread_id,
                        *id,
                        *f,
                        *u,
                        target,
                        kind.as_str(),
                        multi_mailbox,
                    )?;
                    out.push((aid, account_id));
                }
            }
        }
        ActionKind::Move => {
            let target = params
                .and_then(|p| p.target_folder_id)
                .ok_or_else(|| CoreError::Other("move requires targetFolderId".into()))?;
            for (id, f, u, _r, _s, _role) in &msgs {
                if f.is_some() && *f != Some(target) {
                    let aid = enqueue_move(
                        &tx,
                        account_id,
                        thread_id,
                        *id,
                        *f,
                        *u,
                        target,
                        "move",
                        multi_mailbox,
                    )?;
                    out.push((aid, account_id));
                }
            }
        }
        ActionKind::Snooze => {
            let wake_at = params
                .and_then(|p| p.wake_at)
                .ok_or_else(|| CoreError::Other("snooze requires wakeAt".into()))?;
            let orig = msgs.iter().find_map(|(_, f, ..)| *f);
            repo::snoozes::set(&tx, thread_id, account_id, wake_at, orig)?;
            let aid = repo::actions::enqueue(
                &tx,
                account_id,
                "snooze",
                None,
                Some(thread_id),
                &serde_json::json!({ "wakeAt": wake_at }),
                None,
            )?;
            out.push((aid, account_id));
        }
        ActionKind::Unsnooze => {
            repo::snoozes::clear(&tx, thread_id)?;
            let aid = repo::actions::enqueue(
                &tx,
                account_id,
                "unsnooze",
                None,
                Some(thread_id),
                &serde_json::json!({}),
                None,
            )?;
            out.push((aid, account_id));
        }
        ActionKind::AddLabel | ActionKind::RemoveLabel => {
            let label_id = params
                .and_then(|p| p.label_id)
                .ok_or_else(|| CoreError::Other("label action requires labelId".into()))?;
            let label = repo::labels::get(&tx, label_id)?
                .ok_or_else(|| CoreError::NotFound(format!("label {label_id}")))?;
            let add = kind == ActionKind::AddLabel;
            let payload = serde_json::json!({ "labelId": label_id, "keyword": label.keyword });
            for (id, ..) in &msgs {
                if add {
                    repo::labels::add_to_message(&tx, *id, label_id)?;
                } else {
                    repo::labels::remove_from_message(&tx, *id, label_id)?;
                }
                // Auto labels are local-only: mutate membership but never push
                // their keyword to IMAP (server reconcile also skips them).
                if label.is_auto {
                    continue;
                }
                let aid = repo::actions::enqueue(
                    &tx,
                    account_id,
                    kind.as_str(),
                    Some(*id),
                    Some(thread_id),
                    &payload,
                    None,
                )?;
                out.push((aid, account_id));
            }
        }
        ActionKind::Send => {
            return Err(CoreError::Other("use queue_send for sending".into()));
        }
    }

    repo::threads::recompute(&tx, thread_id)?;
    tx.commit()?;
    Ok(out)
}

/// Inverse of an action: cancel if pending, revert the local mutation, and
/// enqueue a compensating remote action when the original already ran.
/// Returns the affected thread id.
fn revert_action(
    conn: &mut rusqlite::Connection,
    action: &repo::actions::PendingAction,
) -> Result<Option<i64>> {
    let was_pending = repo::actions::try_cancel(conn, action.id)?;
    let tx = conn.transaction()?;
    let thread_id = action.thread_id;

    match action.kind.as_str() {
        "mark_read" | "mark_unread" => {
            if let Some(mid) = action.message_id {
                repo::messages::set_read(&tx, mid, action.kind == "mark_unread")?;
                if !was_pending {
                    let inverse = if action.kind == "mark_read" {
                        "mark_unread"
                    } else {
                        "mark_read"
                    };
                    repo::actions::enqueue(
                        &tx,
                        action.account_id,
                        inverse,
                        Some(mid),
                        thread_id,
                        &serde_json::json!({}),
                        None,
                    )?;
                }
            }
        }
        "star" | "unstar" => {
            if let Some(mid) = action.message_id {
                repo::messages::set_starred(&tx, mid, action.kind == "unstar")?;
                if !was_pending {
                    let inverse = if action.kind == "star" {
                        "unstar"
                    } else {
                        "star"
                    };
                    repo::actions::enqueue(
                        &tx,
                        action.account_id,
                        inverse,
                        Some(mid),
                        thread_id,
                        &serde_json::json!({}),
                        None,
                    )?;
                }
            }
        }
        "archive" | "trash" | "spam" | "move" | "unarchive" | "not_spam" => {
            if let Some(mid) = action.message_id {
                let src_folder = action.payload["srcFolderId"].as_i64();
                let src_uid = action.payload["srcUid"].as_i64();
                let cur_folder = action.payload["targetFolderId"].as_i64();
                if let Some(src) = src_folder {
                    let gmail: bool = tx.query_row(
                        "SELECT a.provider = 'gmail'
                         FROM messages m JOIN accounts a ON a.id = m.account_id
                         WHERE m.id = ?1",
                        rusqlite::params![mid],
                        |row| row.get(0),
                    )?;
                    if was_pending {
                        // Remote never changed; restore the original mapping.
                        repo::messages::set_uid_and_folder(&tx, mid, src, src_uid)?;
                    } else {
                        // Remote moved; move it back.
                        repo::messages::set_uid_and_folder(&tx, mid, src, None)?;
                        let payload = serde_json::json!({
                            "srcFolderId": cur_folder,
                            "srcUid": serde_json::Value::Null,
                            "targetFolderId": src,
                        });
                        repo::actions::enqueue(
                            &tx,
                            action.account_id,
                            "move",
                            Some(mid),
                            thread_id,
                            &payload,
                            None,
                        )?;
                    }
                    if gmail {
                        if let Some(current) = cur_folder {
                            tx.execute(
                                "DELETE FROM message_folders
                                 WHERE message_id = ?1 AND folder_id = ?2",
                                rusqlite::params![mid, current],
                            )?;
                        }
                        tx.execute(
                            "INSERT OR IGNORE INTO message_folders (message_id, folder_id)
                             VALUES (?1, ?2)",
                            rusqlite::params![mid, src],
                        )?;
                    }
                }
            }
        }
        "add_label" | "remove_label" => {
            if let (Some(mid), Some(label_id)) =
                (action.message_id, action.payload["labelId"].as_i64())
            {
                let was_add = action.kind == "add_label";
                // Restore the local membership to its pre-action state.
                if was_add {
                    repo::labels::remove_from_message(&tx, mid, label_id)?;
                } else {
                    repo::labels::add_to_message(&tx, mid, label_id)?;
                }
                if !was_pending {
                    // Remote already got the keyword; enqueue the inverse push.
                    let inverse = if was_add { "remove_label" } else { "add_label" };
                    repo::actions::enqueue(
                        &tx,
                        action.account_id,
                        inverse,
                        Some(mid),
                        thread_id,
                        &action.payload,
                        None,
                    )?;
                }
            }
        }
        "snooze" => {
            if let Some(tid) = thread_id {
                repo::snoozes::clear(&tx, tid)?;
            }
        }
        "unsnooze" => { /* nothing sensible to restore */ }
        "send" => { /* cancel already handled if pending; sent mail can't be unsent */ }
        _ => {}
    }

    if let Some(tid) = thread_id {
        repo::threads::recompute(&tx, tid)?;
    }
    tx.commit()?;
    Ok(thread_id)
}

#[cfg(test)]
mod ai_model_routing_tests {
    use super::*;

    #[test]
    fn fresh_settings_have_explicit_models_for_every_tier() {
        let settings = Settings::default();
        assert!(!settings.ai_model_instant.trim().is_empty());
        assert!(!settings.ai_model_cheap.trim().is_empty());
        assert!(!settings.ai_model_intelligent.trim().is_empty());
        assert_eq!(
            resolve_ai_model(&settings, Scenario::Ask),
            settings.ai_model_intelligent
        );
        assert_eq!(
            resolve_ai_model(&settings, Scenario::Summarize),
            settings.ai_model_instant
        );
    }

    #[test]
    fn each_scenario_uses_its_tier_model() {
        let s = Settings {
            ai_model_instant: "fast".into(),
            ai_model_cheap: "mid".into(),
            ai_model_intelligent: "smart".into(),
            ..Settings::default()
        };
        // Defaults: ask/draft -> intelligent, summarize -> instant, voice -> cheap.
        assert_eq!(resolve_ai_model(&s, Scenario::Ask), "smart");
        assert_eq!(resolve_ai_model(&s, Scenario::Draft), "smart");
        assert_eq!(resolve_ai_model(&s, Scenario::Summarize), "fast");
        assert_eq!(resolve_ai_model(&s, Scenario::Voice), "mid");
    }

    #[test]
    fn scenario_can_be_repointed_to_another_tier() {
        let s = Settings {
            ai_model_instant: "fast".into(),
            ai_tier_ask: "instant".into(), // route Ask to the instant tier
            ..Settings::default()
        };
        assert_eq!(resolve_ai_model(&s, Scenario::Ask), "fast");
    }

    #[test]
    fn unknown_tier_uses_the_intelligent_model() {
        let s = Settings {
            ai_model_intelligent: "safe-default".into(),
            ai_tier_ask: "unknown".into(),
            ..Settings::default()
        };
        assert_eq!(resolve_ai_model(&s, Scenario::Ask), "safe-default");
    }
}

#[cfg(test)]
mod attachment_ext_tests {
    use super::*;

    #[test]
    fn calendar_mime_maps_to_ics() {
        assert_eq!(ext_for_mime("text/calendar"), Some("ics"));
        // MIME parameters (e.g. method=REQUEST) and casing are tolerated.
        assert_eq!(ext_for_mime("text/calendar; method=REQUEST"), Some("ics"));
        assert_eq!(ext_for_mime("TEXT/CALENDAR"), Some("ics"));
    }

    #[test]
    fn unknown_mime_has_no_extension() {
        assert_eq!(ext_for_mime("application/x-unknown"), None);
    }
}

#[cfg(test)]
mod draft_staging_tests {
    use super::*;

    fn draft_args(
        draft_id: Option<i64>,
        source: &std::path::Path,
        filename: &str,
    ) -> SaveDraftArgs {
        SaveDraftArgs {
            draft_id,
            account_id: 1,
            to: Vec::new(),
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "bounded draft".into(),
            body_text: "body".into(),
            body_html: None,
            mode: "new".into(),
            in_reply_to_message_id: None,
            attachments: vec![DraftAttachmentIn {
                file_path: source.to_string_lossy().into_owned(),
                filename: filename.into(),
            }],
        }
    }

    #[tokio::test]
    async fn failed_and_replaced_drafts_do_not_leak_staged_files() {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::for_tests(temp.path());
        let core = Core::start_mail_ui(paths.clone()).await.unwrap();
        core.db
            .write(|conn| {
                db::testutil::seed_account(conn);
                Ok(())
            })
            .await
            .unwrap();
        let first_source = temp.path().join("first.txt");
        let second_source = temp.path().join("second.txt");
        tokio::fs::write(&first_source, b"first").await.unwrap();
        tokio::fs::write(&second_source, b"second").await.unwrap();

        assert!(
            core.save_draft(draft_args(Some(999_999), &first_source, "first.txt"))
                .await
                .is_err()
        );
        assert_eq!(
            std::fs::read_dir(paths.draft_attachments_dir())
                .unwrap()
                .count(),
            0
        );

        let draft_id = core
            .save_draft(draft_args(None, &first_source, "first.txt"))
            .await
            .unwrap();
        let first_staged = core
            .db
            .read(move |conn| {
                Ok(conn.query_row(
                    "SELECT file_path FROM draft_attachments WHERE draft_id = ?1",
                    rusqlite::params![draft_id],
                    |row| row.get::<_, String>(0),
                )?)
            })
            .await
            .unwrap();
        assert!(std::path::Path::new(&first_staged).exists());

        core.save_draft(draft_args(Some(draft_id), &second_source, "second.txt"))
            .await
            .unwrap();
        let second_staged = core
            .db
            .read(move |conn| {
                Ok(conn.query_row(
                    "SELECT file_path FROM draft_attachments WHERE draft_id = ?1",
                    rusqlite::params![draft_id],
                    |row| row.get::<_, String>(0),
                )?)
            })
            .await
            .unwrap();
        assert_ne!(first_staged, second_staged);
        assert!(!std::path::Path::new(&first_staged).exists());
        assert_eq!(std::fs::read(second_staged).unwrap(), b"second");
    }
}

#[cfg(test)]
mod draft_action_race_tests {
    use super::*;

    async fn jmap_draft_with_send(state: &str) -> (Core, tempfile::TempDir, i64, i64) {
        let temp = tempfile::tempdir().unwrap();
        let core = Core::start_mail_ui(Paths::for_tests(temp.path()))
            .await
            .unwrap();
        let state = state.to_owned();
        let (draft_id, action_id) = core
            .db
            .write(move |conn| {
                db::testutil::seed_account(conn);
                conn.execute(
                    "UPDATE accounts SET mail_protocol='jmap',jmap_url='https://mail.test.dev' WHERE id=1",
                    [],
                )?;
                conn.execute("UPDATE folders SET role='drafts' WHERE id=1", [])?;
                let (thread_id, draft_id) =
                    db::testutil::seed_message(conn, "me@test.dev", "Draft", false);
                conn.execute(
                    "UPDATE messages SET is_draft=1,is_outgoing=1,uid=NULL WHERE id=?1",
                    rusqlite::params![draft_id],
                )?;
                let action_id = repo::actions::enqueue(
                    conn,
                    1,
                    "send",
                    Some(draft_id),
                    Some(thread_id),
                    &serde_json::json!({ "draftId": draft_id }),
                    Some(now_ms() + 60_000),
                )?;
                repo::actions::set_state(conn, action_id, &state, None)?;
                Ok((draft_id, action_id))
            })
            .await
            .unwrap();
        (core, temp, draft_id, action_id)
    }

    #[tokio::test]
    async fn deleting_a_jmap_draft_cancels_a_queued_send() {
        let (core, _temp, draft_id, action_id) = jmap_draft_with_send("pending").await;

        core.delete_draft(draft_id).await.unwrap();

        core.db
            .read(move |conn| {
                assert!(repo::messages::get_row(conn, draft_id)?.is_none());
                assert_eq!(
                    repo::actions::get(conn, action_id)?.map(|action| action.state),
                    Some("cancelled".into())
                );
                Ok(())
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn deleting_a_draft_during_submission_is_rejected_without_data_loss() {
        let (core, _temp, draft_id, action_id) = jmap_draft_with_send("inflight").await;

        let error = core.delete_draft(draft_id).await.unwrap_err();
        assert!(error.to_string().contains("currently being submitted"));

        core.db
            .read(move |conn| {
                assert!(repo::messages::get_row(conn, draft_id)?.is_some());
                assert_eq!(
                    repo::actions::get(conn, action_id)?.map(|action| action.state),
                    Some("inflight".into())
                );
                Ok(())
            })
            .await
            .unwrap();
    }
}

#[cfg(test)]
mod thread_read_tests {
    use super::*;

    #[tokio::test]
    async fn fresh_profile_keeps_mail_contacts_and_calendar_independent() {
        let temp = tempfile::tempdir().unwrap();
        let core = Core::start_mail_ui(Paths::for_tests(temp.path()))
            .await
            .unwrap();
        let (thread_id, message_id) = core
            .db
            .write(|conn| {
                db::testutil::seed_account(conn);
                let (thread_id, message_id) =
                    db::testutil::seed_message(conn, "sender@example.com", "Rich message", false);
                repo::messages::store_body(
                    conn,
                    message_id,
                    Some("Plain alternative"),
                    Some("<html><body><strong>Rich HTML body</strong></body></html>"),
                    None,
                    false,
                    Some("Rich HTML body"),
                )?;
                repo::threads::recompute(conn, thread_id)?;
                conn.execute(
                    "INSERT INTO contacts (email, name, is_managed)
                     VALUES ('person@example.com', 'Person', 1)",
                    [],
                )?;
                Ok((thread_id, message_id))
            })
            .await
            .unwrap();

        let detail = core.get_thread(thread_id).await.unwrap();

        assert_eq!(detail.messages.len(), 1);
        assert_eq!(detail.messages[0].id, message_id);
        assert_eq!(
            detail.messages[0].html_body.as_deref(),
            Some("<html><body><strong>Rich HTML body</strong></body></html>")
        );

        core.calendar_db
            .write(|conn| {
                conn.execute(
                    "INSERT INTO calendars (id, account_id, url, display_name)
                     VALUES (1, 1, 'local://primary', 'Primary')",
                    [],
                )?;
                conn.execute(
                    "INSERT INTO calendar_events (
                       account_id, ical_uid, summary, starts_at, calendar_id, is_local
                     ) VALUES (1, 'fresh-event', 'Calendar event', 1000, 1, 1)",
                    [],
                )?;
                Ok(())
            })
            .await
            .unwrap();

        let mail_contract = core
            .db
            .read(|conn| {
                let version: i64 =
                    conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
                let contacts: i64 =
                    conn.query_row("SELECT COUNT(*) FROM contacts", [], |row| row.get(0))?;
                let has_calendar: bool = conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name = 'calendar_events')",
                    [],
                    |row| row.get(0),
                )?;
                let integrity: String =
                    conn.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
                Ok((version, contacts, has_calendar, integrity))
            })
            .await
            .unwrap();
        assert_eq!(mail_contract, (1, 1, false, "ok".to_owned()));

        let calendar_contract = core
            .calendar_db
            .read(|conn| {
                let version: i64 =
                    conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
                let events: i64 =
                    conn.query_row("SELECT COUNT(*) FROM calendar_events", [], |row| row.get(0))?;
                let has_messages: bool = conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name = 'messages')",
                    [],
                    |row| row.get(0),
                )?;
                let integrity: String =
                    conn.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
                Ok((version, events, has_messages, integrity))
            })
            .await
            .unwrap();
        assert_eq!(calendar_contract, (1, 1, false, "ok".to_owned()));
    }
}

#[cfg(test)]
mod account_backup_import_tests {
    use super::*;

    fn portable_imap(email: &str) -> PortableAccountConfig {
        PortableAccountConfig {
            email: email.into(),
            display_name: Some("Portable account".into()),
            provider: Provider::Imap,
            auth_kind: AuthKind::Password,
            mail_protocol: MailProtocol::Imap,
            username: email.into(),
            jmap_url: String::new(),
            imap_host: "imap.example.com".into(),
            imap_port: 993,
            smtp_host: "smtp.example.com".into(),
            smtp_port: 465,
            settings: AccountSettings::default(),
        }
    }

    #[tokio::test]
    async fn imported_configuration_requires_reauthentication_and_merges_by_email() {
        let temp = tempfile::tempdir().unwrap();
        let core = Core::start_mail_ui(Paths::for_tests(temp.path()))
            .await
            .unwrap();

        assert_eq!(
            core.import_account_configs(vec![portable_imap("person@example.com")])
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            core.import_account_configs(vec![portable_imap("PERSON@example.com")])
                .await
                .unwrap(),
            0
        );

        let accounts = core.list_accounts().await.unwrap();
        core.set_account_mail_history(accounts[0].id, MailHistory::OneYear)
            .await
            .unwrap();
        assert_eq!(
            core.list_account_configs().await.unwrap()[0]
                .settings
                .mail_history,
            MailHistory::OneYear
        );
        let configs = core.list_account_configs().await.unwrap();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].sync_state, "needs_reauth");
        assert_eq!(configs[0].imap_host, "imap.example.com");
    }
}

#[cfg(test)]
mod local_data_reset_tests {
    use super::*;

    #[tokio::test]
    async fn delete_all_local_data_keeps_databases_open_and_empties_profile() {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::for_tests(temp.path());
        let core = Core::start_mail_ui(paths.clone()).await.unwrap();
        core.db
            .write(|conn| {
                db::testutil::seed_account(conn);
                db::testutil::seed_message(conn, "sender@example.com", "Reset me", false);
                conn.execute(
                    "INSERT INTO contacts (email, name) VALUES ('friend@example.com', 'Friend')",
                    [],
                )?;
                conn.execute(
                    "INSERT INTO snippets (name, body_text) VALUES ('Hello', 'Hi')",
                    [],
                )?;
                repo::settings::set(
                    conn,
                    &Settings {
                        theme: "carbon".into(),
                        ..Settings::default()
                    },
                )?;
                Ok(())
            })
            .await
            .unwrap();
        core.calendar_db
            .write(|conn| {
                conn.execute(
                    "INSERT INTO calendar_events
                     (account_id, ical_uid, starts_at, is_local)
                     VALUES (1, 'event-1', 1000, 1)",
                    [],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        std::fs::create_dir_all(paths.models_dir()).unwrap();
        std::fs::write(paths.models_dir().join("model.bin"), b"model").unwrap();

        core.delete_all_local_data().await.unwrap();

        let counts = core
            .db
            .read(|conn| {
                Ok((
                    conn.query_row("SELECT COUNT(*) FROM accounts", [], |row| {
                        row.get::<_, i64>(0)
                    })?,
                    conn.query_row("SELECT COUNT(*) FROM contacts", [], |row| {
                        row.get::<_, i64>(0)
                    })?,
                    conn.query_row("SELECT COUNT(*) FROM snippets", [], |row| {
                        row.get::<_, i64>(0)
                    })?,
                    conn.query_row("SELECT COUNT(*) FROM app_settings", [], |row| {
                        row.get::<_, i64>(0)
                    })?,
                ))
            })
            .await
            .unwrap();
        assert_eq!(counts, (0, 0, 0, 0));
        let calendar_count = core
            .calendar_db
            .read(|conn| {
                Ok(
                    conn.query_row("SELECT COUNT(*) FROM calendar_events", [], |row| {
                        row.get::<_, i64>(0)
                    })?,
                )
            })
            .await
            .unwrap();
        assert_eq!(calendar_count, 0);
        assert!(!paths.models_dir().exists());
        assert!(core.list_accounts().await.unwrap().is_empty());
        assert_eq!(core.get_settings().await.unwrap().theme, "system");
    }

    #[tokio::test]
    async fn startup_recovery_finishes_interrupted_removal_and_cleans_orphans() {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::for_tests(temp.path());
        let core = Core::start_mail_ui(paths.clone()).await.unwrap();
        core.db
            .write(|conn| {
                db::testutil::seed_account(conn);
                conn.execute(
                    "INSERT INTO cross_store_operations (kind, account_id, created_at)
                     VALUES ('remove_account', 1, 1000)",
                    [],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        core.calendar_db
            .write(|conn| {
                conn.execute(
                    "INSERT INTO calendar_events
                     (account_id, ical_uid, starts_at, is_local)
                     VALUES (1, 'pending-delete', 1000, 1),
                            (99, 'legacy-orphan', 1000, 1)",
                    [],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        std::fs::create_dir_all(paths.mail_dir(1)).unwrap();
        std::fs::create_dir_all(paths.attachments_dir(1)).unwrap();

        core.recover_cross_store_state().await.unwrap();

        let (accounts, operations) = core
            .db
            .read(|conn| {
                Ok((
                    conn.query_row("SELECT COUNT(*) FROM accounts", [], |row| {
                        row.get::<_, i64>(0)
                    })?,
                    conn.query_row("SELECT COUNT(*) FROM cross_store_operations", [], |row| {
                        row.get::<_, i64>(0)
                    })?,
                ))
            })
            .await
            .unwrap();
        let calendar_events = core
            .calendar_db
            .read(|conn| {
                Ok(
                    conn.query_row("SELECT COUNT(*) FROM calendar_events", [], |row| {
                        row.get::<_, i64>(0)
                    })?,
                )
            })
            .await
            .unwrap();
        assert_eq!((accounts, operations, calendar_events), (0, 0, 0));
        assert!(!paths.mail_dir(1).exists());
        assert!(!paths.attachments_dir(1).exists());
    }

    #[tokio::test]
    async fn startup_recovery_detaches_only_missing_mail_message_links() {
        let temp = tempfile::tempdir().unwrap();
        let core = Core::start_mail_ui(Paths::for_tests(temp.path()))
            .await
            .unwrap();
        let existing_message_id =
            core.db
                .write(|conn| {
                    db::testutil::seed_account(conn);
                    Ok(db::testutil::seed_message(
                        conn,
                        "sender@example.com",
                        "Calendar invite",
                        false,
                    )
                    .1)
                })
                .await
                .unwrap();
        let missing_message_id = existing_message_id + 10_000;
        core.calendar_db
            .write(move |conn| {
                conn.execute(
                    "INSERT INTO calendar_events
                     (account_id, message_id, ical_uid, starts_at, is_local)
                     VALUES (1, ?1, 'linked-existing', 1000, 1),
                            (1, ?2, 'linked-missing', 2000, 1)",
                    rusqlite::params![existing_message_id, missing_message_id],
                )?;
                Ok(())
            })
            .await
            .unwrap();

        assert_eq!(
            core.detach_orphaned_calendar_message_links().await.unwrap(),
            1
        );

        let links = core
            .calendar_db
            .read(|conn| {
                let mut statement = conn.prepare(
                    "SELECT ical_uid, message_id FROM calendar_events ORDER BY ical_uid",
                )?;
                Ok(statement
                    .query_map([], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, Option<i64>>(1)?))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?)
            })
            .await
            .unwrap();
        assert_eq!(
            links,
            vec![
                ("linked-existing".to_owned(), Some(existing_message_id)),
                ("linked-missing".to_owned(), None),
            ]
        );
    }
}
