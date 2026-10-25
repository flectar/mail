//! Email, calendar, account, and persistence logic shared by the native hosts.
//! The application embeds [`Core`], calls its async methods, and forwards
//! [`CoreEvent`]s to the Slint UI.

pub mod accounts;
pub mod ai;
pub mod autolabel;
pub mod caldav;
pub mod calendar;
pub mod carddav;
pub mod config;
pub mod db;
pub mod embed;
pub mod error;
pub mod events;
mod file_io;
pub mod files;
pub mod googlecal;
pub mod graph;
pub mod graphcal;
mod http_body;
pub mod imap;
pub mod jmap;
pub mod mail_security;
mod mbox;
pub mod migration;
pub mod mime;
pub mod models;
pub mod net;
pub mod oauth;
pub mod pop3;
pub mod queue;
pub mod route;
pub mod scheduler;
pub mod search;
pub mod sender_identities;
pub mod signatures;
pub mod smtp;
pub mod sync;
pub mod unsubscribe;
pub mod verification_code;

use crate::accounts::credentials::{self, CredentialStoreHandle, Slot};
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
const SEND_START_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
const SEND_STATUS_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);

#[cfg(test)]
mod inline_cid_rewrite_tests {
    use crate::mime::rewrite_cid_src_with_images;
    use base64::Engine;
    use std::collections::HashMap;

    #[test]
    fn direct_encoding_preserves_existing_data_uri_bytes() {
        for len in [0, 1, 2, 3, 4, 7, 256, 1024] {
            let bytes: Vec<u8> = (0..len).map(|index| index as u8).collect();
            let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
            let map = HashMap::from([("image".to_owned(), ("image/png".to_owned(), bytes))]);
            assert_eq!(
                rewrite_cid_src_with_images("<img src='cid:image'>", &map),
                format!("<img src=\"data:image/png;base64,{encoded}\">")
            );
        }
    }
}

fn send_start_expired(now: i64, observed_at: i64, not_before: Option<i64>) -> bool {
    let timeout_ms = i64::try_from(SEND_START_TIMEOUT.as_millis()).unwrap_or(i64::MAX);
    now >= not_before.unwrap_or(observed_at).saturating_add(timeout_ms)
}

fn normalize_mail_profile_name(value: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(CoreError::Other("profile name cannot be empty".into()));
    }
    if value.chars().count() > 48 {
        return Err(CoreError::Other(
            "profile name cannot be longer than 48 characters".into(),
        ));
    }
    if value.chars().any(char::is_control) {
        return Err(CoreError::Other(
            "profile name cannot contain control characters".into(),
        ));
    }
    Ok(value.to_owned())
}

/// The mail view only shows a profile that exists and has accounts. Drop it
/// once a deletion or membership change leaves neither.
fn retire_active_mail_profile(settings: &mut Settings) {
    let showable = settings
        .active_mail_profile
        .as_deref()
        .is_some_and(|active| {
            settings
                .mail_profiles
                .iter()
                .any(|profile| profile.id == active && !profile.account_ids.is_empty())
        });
    if !showable {
        settings.active_mail_profile = None;
    }
}

fn normalize_mail_profile_color(value: &str) -> Result<String> {
    let value = value.trim();
    let valid = value.len() == 7
        && value.starts_with('#')
        && value[1..].bytes().all(|byte| byte.is_ascii_hexdigit());
    if !valid {
        return Err(CoreError::Other(
            "profile color must use #RRGGBB format".into(),
        ));
    }
    Ok(value.to_ascii_uppercase())
}

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

/// Read the presentation state needed to lay out the first application frame.
///
/// This is a best-effort presentation bootstrap. Normal core startup remains
/// authoritative for loading and validating the complete settings model.
pub fn startup_presentation(paths: &Paths) -> models::StartupPresentation {
    db::startup_presentation(&paths.db_file())
}

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

/// Rules every account created from portable or migrated settings must
/// satisfy. Interactive setup checks the same fields as the user types them.
pub(crate) fn validate_portable_account(config: &PortableAccountConfig) -> Result<()> {
    net::tls::trusted_certificates(&config.settings.connection.trusted_certificate_pem)?;
    let email = config.email.trim();
    if email.len() > 320 || !email.contains('@') {
        return Err(CoreError::Other(format!(
            "invalid account email address: {}",
            config.email
        )));
    }
    if config.username.len() > 512
        || config.jmap_url.len() > 2048
        || config.incoming_host.len() > 253
        || config.smtp_host.len() > 253
    {
        return Err(CoreError::Other("an account field is too long".into()));
    }
    let account_kind = (config.provider, config.auth_kind);
    let supported = match config.mail_protocol {
        MailProtocol::Gmail => account_kind == (Provider::Gmail, AuthKind::Oauth2),
        MailProtocol::Imap => matches!(
            account_kind,
            (Provider::Generic, AuthKind::Password) | (Provider::Microsoft, AuthKind::Oauth2)
        ),
        MailProtocol::Jmap | MailProtocol::Pop3 => {
            account_kind == (Provider::Generic, AuthKind::Password)
        }
    };
    if !supported {
        return Err(CoreError::Other(format!(
            "unsupported sign-in type for {email}"
        )));
    }
    if config.mail_protocol != MailProtocol::Pop3
        && config.settings.keep_on_server != KeepOnServer::Always
    {
        return Err(CoreError::Other(
            "only POP3 accounts remove downloaded mail from their server".into(),
        ));
    }
    let connection = &config.settings.connection;
    if config.auth_kind == AuthKind::Oauth2
        && !(connection.incoming_security.is_encrypted() && connection.smtp_security.is_encrypted())
    {
        return Err(CoreError::Other(format!(
            "{email} signs in with OAuth, which needs encrypted connections"
        )));
    }
    let complete = match config.mail_protocol {
        // Provider presets own the server settings of OAuth accounts.
        MailProtocol::Gmail => true,
        MailProtocol::Imap if config.provider == Provider::Microsoft => true,
        MailProtocol::Imap | MailProtocol::Pop3 => {
            !config.username.trim().is_empty()
                && !config.incoming_host.trim().is_empty()
                && !config.smtp_host.trim().is_empty()
                && config.incoming_port != 0
                && config.smtp_port != 0
        }
        MailProtocol::Jmap => !config.username.trim().is_empty(),
    };
    if !complete {
        return Err(CoreError::Other(format!(
            "incomplete {} settings for {email}",
            config.mail_protocol.as_str().to_ascii_uppercase()
        )));
    }
    Ok(())
}

/// Separator between the levels of a stored folder path. JMAP paths are
/// composed by Flectar from mailbox names; IMAP servers and Gmail labels
/// report their own, and POP3 folders exist only on this device.
fn folder_path_delimiter(protocol: MailProtocol, folder: &repo::folders::Folder) -> String {
    match protocol {
        MailProtocol::Jmap => " / ".to_owned(),
        MailProtocol::Gmail | MailProtocol::Imap => {
            folder.delimiter.clone().unwrap_or_else(|| "/".into())
        }
        MailProtocol::Pop3 => repo::pop3::FOLDER_DELIMITER.to_owned(),
    }
}

/// The configuration a password account would have, for checks before it
/// exists.
fn password_account_config(args: &AddPasswordAccountArgs) -> AccountConfig {
    AccountConfig {
        id: 0,
        email: args.email.clone(),
        display_name: None,
        avatar_url: None,
        provider: Provider::Generic,
        auth_kind: AuthKind::Password,
        mail_protocol: args.mail_protocol,
        username: args.username.clone(),
        jmap_url: String::new(),
        jmap_account_id: None,
        incoming_host: args.incoming_host.clone(),
        incoming_port: args.incoming_port,
        smtp_host: args.smtp_host.clone(),
        smtp_port: args.smtp_port,
        settings: AccountSettings::new(args.connection.clone()),
    }
}

async fn check_smtp(args: &AddPasswordAccountArgs) -> Result<()> {
    smtp::test_connection(
        &password_account_config(args),
        &smtp::SmtpAuth::Password(args.password.clone()),
    )
    .await
}

/// The setup check signed in, so the first sync respects the server's
/// minimum time between sign-ins.
fn record_pop3_probe(
    conn: &rusqlite::Connection,
    account_id: i64,
    probe: sync::pop3::Probe,
) -> Result<()> {
    repo::pop3::record_sign_in(
        conn,
        account_id,
        probe.signed_in_at,
        probe.login_delay,
        probe.expire,
    )
}

fn folder_exists() -> CoreError {
    CoreError::Other("a folder with this name already exists".into())
}

/// Gmail mail is only reachable through Google sign-in.
fn gmail_needs_google_sign_in() -> CoreError {
    CoreError::Auth("Gmail accounts sign in with Google, not with a password".into())
}

/// Whether an account stored as `stored` takes the address `signed_in` that
/// its provider reported for a sign-in. Google ignores dots in Gmail
/// addresses and treats googlemail.com as gmail.com, so an imported login can
/// be another spelling of the address Google reports. Addresses that differ
/// only in case keep the stored spelling.
fn sign_in_respells(provider: Provider, stored: &str, signed_in: &str) -> Result<bool> {
    if signed_in.eq_ignore_ascii_case(stored) {
        Ok(false)
    } else if provider == Provider::Gmail && accounts::same_mailbox(signed_in, stored) {
        Ok(true)
    } else {
        // Refuse to graft another mailbox's tokens onto this account. Signing
        // in as a different address is "add account", not "reauth".
        Err(CoreError::Auth(format!(
            "signed in as {signed_in} but this account is {stored}"
        )))
    }
}

fn normalize_account_password(value: String) -> String {
    // Password controls are one-line inputs. Apply that invariant at the core
    // boundary too so clipboard line endings cannot reach protocol encoders.
    // Preserve spaces and every other character because they may be valid.
    value
        .chars()
        .filter(|character| !matches!(character, '\r' | '\n'))
        .collect()
}

#[cfg(test)]
mod sign_in_address_tests {
    use super::*;

    #[test]
    fn gmail_sign_ins_take_googles_spelling_of_the_same_mailbox() {
        use Provider::{Gmail, Microsoft};
        for (provider, stored, signed_in, respells) in [
            (Gmail, "JohnDoe@gmail.com", "johndoe@gmail.com", Some(false)),
            (Gmail, "johndoe@gmail.com", "john.doe@gmail.com", Some(true)),
            (Gmail, "J.Doe@googlemail.com", "jdoe@gmail.com", Some(true)),
            (Gmail, "johndoe@gmail.com", "jane@gmail.com", None),
            // Workspace addresses and Microsoft sign-ins keep their dots.
            (Gmail, "jane.doe@example.com", "janedoe@example.com", None),
            (Microsoft, "johndoe@gmail.com", "john.doe@gmail.com", None),
            (Microsoft, "Jo@Contoso.test", "jo@contoso.test", Some(false)),
        ] {
            assert_eq!(
                sign_in_respells(provider, stored, signed_in).ok(),
                respells,
                "{stored} signed in as {signed_in}"
            );
        }
        assert_eq!(
            sign_in_respells(Gmail, "johndoe@gmail.com", "jane@gmail.com")
                .unwrap_err()
                .to_string(),
            "auth failed: signed in as jane@gmail.com but this account is johndoe@gmail.com"
        );
    }
}

#[cfg(test)]
mod account_input_tests {
    use super::{normalize_account_email, normalize_account_password};

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

    #[test]
    fn pasted_password_line_breaks_are_removed_without_trimming_spaces() {
        assert_eq!(
            normalize_account_password("\r\n secret\nvalue \r\n".into()),
            " secretvalue "
        );
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
        Provider::Generic => &[],
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
    /// Remote files, sync cursors and durable transfer operations.
    pub files_db: Db,
    pub(crate) file_work_lock: Arc<tokio::sync::Mutex<()>>,
    pub bus: EventBus,
    paths: Arc<Paths>,
    tokens: TokenProvider,
    credentials: CredentialStoreHandle,
    oauth_redirects: OAuthRedirectBrokerHandle,
    handles: Arc<RwLock<HashMap<i64, AccountHandle>>>,
    cal_handles: Arc<RwLock<HashMap<i64, caldav::task::CalTaskHandle>>>,
    card_handles: Arc<RwLock<HashMap<i64, carddav::task::CardDavTaskHandle>>>,
    /// Per-attachment single-flight locks. Concurrent preview/open/save calls
    /// share one remote fetch and cannot race the same `.download.part` file.
    attachment_locks:
        Arc<tokio::sync::Mutex<HashMap<i64, std::sync::Weak<tokio::sync::Mutex<()>>>>>,
    #[cfg(feature = "local-embeddings")]
    embed: Arc<embed::EmbedState>,
    /// Fired by `cancel_oauth` to abort a pending browser sign-in (the
    /// loopback wait otherwise blocks the UI until its 15-minute timeout).
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
    #[cfg(desktop)]
    pub async fn start(paths: Paths) -> Result<Core> {
        Self::start_with_platform(
            paths,
            Arc::new(credentials::SystemCredentialStore),
            Arc::new(LoopbackRedirectBroker::default()),
            true,
        )
        .await
    }

    /// Start the core for a mail-only native shell. Mail synchronization,
    /// folder discovery, and keyword search stay fully enabled, while the
    /// optional local embedding model is left unloaded until a host that
    /// exposes semantic search asks for it.
    #[cfg(desktop)]
    pub async fn start_mail_ui(paths: Paths) -> Result<Core> {
        Self::start_mail_ui_with_credentials(paths, Arc::new(credentials::SystemCredentialStore))
            .await
    }

    /// Start a mail-only core with a host-provided credential backend. Mobile
    /// hosts use this entry point so no unsupported keyring fallback can run.
    pub async fn start_mail_ui_with_credentials(
        paths: Paths,
        credentials: CredentialStoreHandle,
    ) -> Result<Core> {
        Self::start_mail_ui_with_platform(
            paths,
            credentials,
            Arc::new(LoopbackRedirectBroker::default()),
        )
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
        // Product databases are opened by their first operation, not by Core construction.
        let calendar_db = Db::deferred_calendar(paths.calendar_db_file());
        let files_db = Db::deferred_files(paths.files_db_file());
        let bus = EventBus::new();
        let core = Core {
            db,
            calendar_db,
            files_db,
            file_work_lock: Arc::new(tokio::sync::Mutex::new(())),
            bus,
            paths: Arc::new(paths),
            tokens: TokenProvider::new(credentials.clone(), oauth_redirects.clone()),
            credentials,
            oauth_redirects,
            handles: Arc::new(RwLock::new(HashMap::new())),
            cal_handles: Arc::new(RwLock::new(HashMap::new())),
            card_handles: Arc::new(RwLock::new(HashMap::new())),
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
        if core.paths.files_db_file().exists()
            || core
                .db
                .read(|conn| {
                    Ok(conn.query_row(
                        "SELECT EXISTS(SELECT 1 FROM app_settings WHERE key LIKE 'files:%')",
                        [],
                        |r| r.get::<_, bool>(0),
                    )?)
                })
                .await?
        {
            core.recover_files().await?;
            core.files_db.release_idle_connections().await?;
        }
        let removed_staged_files = core.cleanup_orphaned_draft_files().await?;
        if removed_staged_files > 0 {
            tracing::info!(removed_staged_files, "removed orphaned draft staging files");
        }
        let removed_mail_files = core.remove_interrupted_mail_files().await?;
        if removed_mail_files > 0 {
            tracing::info!(
                removed_mail_files,
                "removed files of interrupted mail writes"
            );
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
        if core.calendar_db.is_open() {
            match core
                .calendar_db
                .write(|conn| repo::actions::recover_inflight(conn))
                .await
            {
                Ok(n) if n > 0 => tracing::info!("recovered {n} calendar action(s)"),
                _ => {}
            }
            core.calendar_db.release_idle_connections().await?;
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
        // A fresh profile has no snoozes, queued actions, mail actors or
        // calendar store to poll. Preserve startup work for existing profiles,
        // and let the scheduler activate after account setup or the first
        // calendar operation on a new profile.
        let scheduler_has_existing_work = core.paths.calendar_db_file().exists()
            || core
                .db
                .read(|conn| {
                    Ok(conn.query_row(
                        "SELECT EXISTS(SELECT 1 FROM accounts)
                             OR EXISTS(SELECT 1 FROM snoozes)
                             OR EXISTS(SELECT 1 FROM pending_actions)",
                        [],
                        |row| row.get::<_, bool>(0),
                    )?)
                })
                .await?;

        // Recovery and migration can leave both SQLite page caches warm after
        // scanning a large mailbox. Foreground startup needs only a small page
        // and badges; close these idle connections before spawning workers so
        // their recovery working set is not retained throughout the session.
        // The next read/write reopens its connection with the normal profile.
        core.db.release_idle_connections().await?;
        // Spawn actors for existing accounts - but only after the Slint UI
        // reports ready (notify_ui_ready). Actors immediately load OAuth
        // tokens from the OS keyring, and on a launch that plays the intro
        // that prompt must land after the show, not on top of it. The timeout
        // fallback keeps sync alive if no window ever reports in (tray-only
        // or a stalled startup).
        {
            let core = core.clone();
            tokio::spawn(async move {
                events::sync_diagnostic("startup_wait", None, "waiting_for_ui");
                tokio::select! {
                    _ = core.ui_ready.notified() => {}
                    _ = tokio::time::sleep(std::time::Duration::from_secs(60)) => {}
                }
                events::sync_diagnostic("startup_accounts", None, "loading");
                let configs = match core
                    .db
                    .read(|conn| repo::accounts::list_configs(conn))
                    .await
                {
                    Ok(c) => c,
                    Err(e) => {
                        events::sync_diagnostic("startup_accounts", None, e.code());
                        tracing::error!("listing accounts for startup sync: {e}");
                        return;
                    }
                };
                for cfg in configs {
                    core.spawn_actor(cfg).await;
                }
                events::sync_diagnostic("startup_workers", None, "ready");
                // Calendar sync tasks for accounts with a syncing calendar
                // connection, once older CalDAV passwords have moved to theirs.
                if core.paths.calendar_db_file().exists() {
                    core.move_calendar_secrets().await;
                    if let Ok(cal_accounts) = core
                        .calendar_db
                        .read(|conn| repo::caldav::accounts_with_enabled_connections(conn))
                        .await
                    {
                        for account_id in cal_accounts {
                            core.spawn_cal_task(account_id).await;
                        }
                    }
                }
                if let Ok(card_accounts) = core
                    .db
                    .read(|conn| repo::carddav::enabled_configs(conn))
                    .await
                {
                    for cfg in card_accounts {
                        core.spawn_card_task(cfg.account_id).await;
                    }
                    core.nudge_card(None).await;
                }
            });
        }

        scheduler::spawn(
            core.db.clone(),
            core.calendar_db.clone(),
            core.bus.clone(),
            core.handles.clone(),
            core.cal_handles.clone(),
            scheduler_has_existing_work,
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
                let (marker, has_threads, auto) =
                    c.db.read(|conn| {
                        let s = repo::settings::get(conn)?;
                        let marker: i64 = conn.query_row(
                            "SELECT COUNT(*) FROM route_cache
                             WHERE sender_domain = '__routing_backfill__'",
                            [],
                            |r| r.get(0),
                        )?;
                        let has_threads = if marker == 0 && s.auto_labels_enabled {
                            conn.query_row("SELECT EXISTS(SELECT 1 FROM threads)", [], |r| {
                                r.get::<_, bool>(0)
                            })?
                        } else {
                            false
                        };
                        Ok((marker, has_threads, s.auto_labels_enabled))
                    })
                    .await
                    .unwrap_or((1, false, false));
                if marker == 0 {
                    if auto && has_threads {
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

        // Verification codes past a message's preview reach the list without
        // the message being opened.
        {
            let c = core.clone();
            tokio::spawn(async move { c.code_body_loop().await });
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

    /// Long-lived loop: on every mail change, reads the bodies of recent
    /// received messages that mention a code their subject and preview do
    /// not show. Gmail and JMAP read bodies only when a message is opened,
    /// and IMAP's background reading can be far behind, while a code is
    /// wanted now.
    async fn code_body_loop(&self) {
        use tokio::sync::broadcast::error::RecvError;
        // Older codes have expired.
        const RECENT_MS: i64 = 24 * 60 * 60 * 1000;
        // Bodies read for one change at most, newest first.
        const LIMIT: usize = 32;
        let mut rx = self.bus.subscribe();
        loop {
            match rx.recv().await {
                Ok(CoreEvent::MailUpdated { thread_ids }) => {
                    let since = now_ms() - RECENT_MS;
                    let pending = self
                        .db
                        .read(move |conn| {
                            repo::messages::pending_code_bodies(conn, &thread_ids, since)
                        })
                        .await
                        .unwrap_or_else(|error| {
                            tracing::warn!("finding bodies for verification codes failed: {error}");
                            Vec::new()
                        });
                    for (message_id, account_id, _, _) in pending
                        .into_iter()
                        .filter(|(_, _, subject, snippet)| {
                            verification_code::mentioned(subject, snippet)
                        })
                        .take(LIMIT)
                    {
                        self.request_body(account_id, message_id).await;
                    }
                }
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
        events::sync_diagnostic("spawn_worker", Some(cfg.id), "starting");
        let handle = spawn_account(self.sync_ctx(), cfg);
        self.handles.write().await.insert(handle.account_id, handle);
    }

    /// Replace the account's mail worker with one that uses its stored
    /// configuration, after a change the running worker would not see.
    async fn restart_actor(&self, account_id: i64) {
        if let Some(handle) = self.handles.write().await.remove(&account_id) {
            handle.abort();
        }
        match self
            .db
            .read(move |conn| repo::accounts::get_config(conn, account_id))
            .await
        {
            Ok(Some(cfg)) => self.spawn_actor(cfg).await,
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(account_id, %error, "could not restart the mail worker");
            }
        }
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

    /// Keep the account's calendar task running exactly while one of its
    /// connections syncs. Runs after a change was saved, so a failed read
    /// leaves the task as it is rather than failing that change.
    async fn refresh_cal_task(&self, account_id: i64) {
        let syncing = self
            .calendar_db
            .read(move |conn| {
                Ok(repo::caldav::account_connections(conn, account_id)?
                    .iter()
                    .any(|connection| connection.enabled))
            })
            .await;
        match syncing {
            Ok(false) => {
                self.cal_handles.write().await.remove(&account_id);
            }
            Ok(true) => {
                if !self.cal_handles.read().await.contains_key(&account_id) {
                    self.spawn_cal_task(account_id).await;
                }
            }
            Err(error) => tracing::warn!(account_id, "reading calendar connections: {error}"),
        }
    }

    /// Move CalDAV passwords saved before an account could sync several
    /// servers to their connections. A locked keyring leaves the rest for the
    /// connection's first sync.
    async fn move_calendar_secrets(&self) {
        let pending = match self
            .calendar_db
            .read(|conn| repo::caldav::pending_secret_moves(conn))
            .await
        {
            Ok(pending) => pending,
            Err(error) => {
                tracing::warn!("listing calendar passwords to move: {error}");
                return;
            }
        };
        for (connection_id, account_id) in pending {
            if let Err(error) = caldav::task::move_secret(
                &self.calendar_db,
                self.credentials.clone(),
                connection_id,
                account_id,
            )
            .await
            {
                tracing::warn!(connection_id, "moving calendar password: {error}");
            }
        }
    }

    /// Delete a CalDAV connection's password, including one still waiting
    /// under its account from before connections had their own.
    async fn delete_connection_password(
        &self,
        connection: &repo::caldav::ConnectionConfig,
    ) -> Result<()> {
        if connection.kind != CalendarConnectionKind::Generic {
            return Ok(());
        }
        let connection_id = connection.id;
        let account_id = connection.account_id;
        credentials::delete_async(
            self.credentials.clone(),
            account_id,
            Slot::CaldavPassword(connection_id),
        )
        .await?;
        let pending = self
            .calendar_db
            .read(move |conn| repo::caldav::secret_move_pending(conn, connection_id))
            .await?;
        if pending {
            credentials::delete_async(
                self.credentials.clone(),
                account_id,
                Slot::AccountCaldavPassword,
            )
            .await?;
        }
        Ok(())
    }

    /// Delete the passwords of every CalDAV connection of an account. The
    /// keyring cannot list entries, so they are found through their
    /// connections, before those go.
    async fn delete_calendar_passwords(&self, account_id: i64) -> Result<()> {
        let connections = self
            .calendar_db
            .read(move |conn| repo::caldav::account_connections(conn, account_id))
            .await?;
        for connection in &connections {
            self.delete_connection_password(connection).await?;
        }
        Ok(())
    }

    async fn spawn_card_task(&self, account_id: i64) {
        let handle = carddav::task::spawn(
            self.db.clone(),
            self.bus.clone(),
            self.credentials.clone(),
            account_id,
        );
        if let Some(previous) = self.card_handles.write().await.insert(account_id, handle) {
            previous.abort();
        }
    }

    async fn nudge_card(&self, account_id: Option<i64>) {
        let handles = self.card_handles.read().await;
        for handle in handles
            .values()
            .filter(|handle| account_id.is_none_or(|id| id == handle.account_id))
        {
            handle.nudge();
        }
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
            validate_portable_account(config)?;
        }

        self.db
            .write(move |conn| {
                let transaction = conn.transaction()?;
                let mut imported = 0usize;
                for config in &configs {
                    if repo::accounts::find_by_mailbox(&transaction, config.email.trim())?.is_some()
                    {
                        continue;
                    }
                    repo::accounts::insert_with_sync_state(
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
                            incoming_host: config.incoming_host.trim(),
                            incoming_port: config.incoming_port,
                            smtp_host: config.smtp_host.trim(),
                            smtp_port: config.smtp_port,
                        },
                        &config.settings,
                        "needs_reauth",
                    )?;
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
                match config.mail_protocol {
                    MailProtocol::Gmail => repo::gmail::expire_history(&transaction, account_id)?,
                    MailProtocol::Jmap => {
                        transaction.execute(
                            "UPDATE jmap_sync_state SET email_state = NULL WHERE account_id = ?1",
                            rusqlite::params![account_id],
                        )?;
                    }
                    MailProtocol::Imap => {
                        repo::folders::reopen_backfill_for_account(&transaction, account_id)?;
                    }
                    // The next sync weighs skipped server messages against
                    // the new boundary.
                    MailProtocol::Pop3 => {}
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

    /// Override SMTP Sent filing for providers with configurable auto-save.
    pub async fn set_account_sent_copy_policy(
        &self,
        account_id: i64,
        policy: SentCopyPolicy,
    ) -> Result<()> {
        let settings = self
            .db
            .write(move |conn| {
                let tx = conn.transaction()?;
                let mut config = repo::accounts::get_config(&tx, account_id)?
                    .ok_or_else(|| CoreError::NotFound(format!("account {account_id}")))?;
                match config.mail_protocol {
                    MailProtocol::Gmail | MailProtocol::Imap | MailProtocol::Jmap => {}
                    MailProtocol::Pop3 => {
                        return Err(CoreError::Other(
                            "POP3 accounts keep sent mail on this device".into(),
                        ));
                    }
                }
                config.settings.sent_copy_policy = policy;
                repo::accounts::set_settings(&tx, account_id, &config.settings)?;
                tx.commit()?;
                Ok(config.settings)
            })
            .await?;
        if let Some(handle) = self.handles.read().await.get(&account_id).cloned() {
            handle.update_settings(settings);
        }
        self.bus.emit(CoreEvent::MailUpdated { thread_ids: vec![] });
        Ok(())
    }

    /// Choose how long a POP3 account leaves downloaded mail on its server.
    /// The account's next sync applies the choice.
    pub async fn set_account_keep_on_server(
        &self,
        account_id: i64,
        keep: KeepOnServer,
    ) -> Result<()> {
        let settings = self
            .db
            .write(move |conn| {
                let tx = conn.transaction()?;
                let mut config = repo::accounts::get_config(&tx, account_id)?
                    .ok_or_else(|| CoreError::NotFound(format!("account {account_id}")))?;
                match config.mail_protocol {
                    MailProtocol::Pop3 => {}
                    MailProtocol::Gmail | MailProtocol::Imap | MailProtocol::Jmap => {
                        return Err(CoreError::Other(
                            "only POP3 accounts remove downloaded mail from their server".into(),
                        ));
                    }
                }
                config.settings.keep_on_server = keep;
                repo::accounts::set_settings(&tx, account_id, &config.settings)?;
                tx.commit()?;
                Ok(config.settings)
            })
            .await?;
        if let Some(handle) = self.handles.read().await.get(&account_id).cloned() {
            handle.update_settings(settings);
        }
        self.bus.emit(CoreEvent::MailUpdated { thread_ids: vec![] });
        Ok(())
    }

    pub async fn test_connection(&self, args: &AddPasswordAccountArgs) -> ConnectionTestResult {
        let mut args = args.clone();
        args.password = normalize_account_password(args.password);
        let result = match args.mail_protocol {
            MailProtocol::Jmap => crate::jmap::client::connect_with(
                &args.email,
                &args.username,
                &args.password,
                &args.jmap_url,
                None,
            )
            .await
            .map(|_| ())
            .map_err(|error| error.to_client_json()),
            MailProtocol::Imap => self
                .check_imap_smtp(&args)
                .await
                .map_err(|error| error.to_string()),
            MailProtocol::Pop3 => self
                .check_pop3_smtp(&args)
                .await
                .map(drop)
                .map_err(|error| error.to_string()),
            MailProtocol::Gmail => Err(gmail_needs_google_sign_in().to_string()),
        };
        match result {
            Ok(()) => ConnectionTestResult {
                ok: true,
                error: None,
            },
            Err(error) => ConnectionTestResult {
                ok: false,
                error: Some(error),
            },
        }
    }

    async fn check_imap_smtp(&self, args: &AddPasswordAccountArgs) -> Result<()> {
        let creds = imap::ImapCredentials::Password {
            user: args.username.clone(),
            password: args.password.clone(),
        };
        let mut session = imap::connect_with_settings(
            &args.incoming_host,
            args.incoming_port,
            creds,
            &args.connection,
        )
        .await?;
        let mailbox_result = imap::select(&mut session, "INBOX").await.map(|_| ());
        imap::logout(session).await;
        mailbox_result?;
        check_smtp(args).await
    }

    /// Returns what the POP3 server announced, which the first sync has to
    /// respect.
    async fn check_pop3_smtp(&self, args: &AddPasswordAccountArgs) -> Result<sync::pop3::Probe> {
        let probe = sync::pop3::probe(&password_account_config(args), &args.password).await?;
        check_smtp(args).await?;
        Ok(probe)
    }

    pub async fn add_account_password(&self, args: AddPasswordAccountArgs) -> Result<Account> {
        net::tls::trusted_certificates(&args.connection.trusted_certificate_pem)?;
        let email = normalize_account_email(&args.email)
            .ok_or_else(|| CoreError::Auth("enter a valid email address".into()))?;
        let mut args = AddPasswordAccountArgs {
            email,
            display_name: args
                .display_name
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty()),
            username: args.username.trim().to_owned(),
            password: normalize_account_password(args.password),
            mail_protocol: args.mail_protocol,
            jmap_url: args.jmap_url.trim().to_owned(),
            incoming_host: args.incoming_host.trim().to_owned(),
            incoming_port: args.incoming_port,
            smtp_host: args.smtp_host.trim().to_owned(),
            smtp_port: args.smtp_port,
            connection: args.connection,
        };
        if args.username.is_empty() {
            args.username = args.email.clone();
        }
        if args.password.is_empty() {
            return Err(CoreError::Auth("email and password are required".into()));
        }
        if args.username.len() > 512
            || args.jmap_url.len() > 2048
            || args.incoming_host.len() > 253
            || args.smtp_host.len() > 253
        {
            return Err(CoreError::Auth("an account field is too long".into()));
        }
        match args.mail_protocol {
            MailProtocol::Imap | MailProtocol::Pop3 => {
                if args.incoming_host.is_empty()
                    || args.smtp_host.is_empty()
                    || args.incoming_port == 0
                    || args.smtp_port == 0
                {
                    return Err(CoreError::Auth(format!(
                        "{} and SMTP server hosts and ports are required",
                        args.mail_protocol.as_str().to_ascii_uppercase()
                    )));
                }
            }
            MailProtocol::Jmap => {
                args.jmap_url =
                    crate::jmap::client::normalize_base_url(&args.jmap_url, &args.email)?;
            }
            MailProtocol::Gmail => return Err(gmail_needs_google_sign_in()),
        }

        credentials::check_available_async(self.credentials.clone()).await?;
        self.recover_incomplete_account_credentials().await?;

        let existing = self
            .db
            .read({
                let email = args.email.clone();
                move |conn| repo::accounts::find_by_mailbox(conn, &email)
            })
            .await?;
        // A Gmail account is edited under the spelling it was added with:
        // the update below would take another spelling for another user.
        if let Some(existing) = &existing
            && !existing.email.eq_ignore_ascii_case(&args.email)
        {
            return Err(CoreError::Auth(format!(
                "{} is already added",
                existing.email
            )));
        }
        // POP3 mail exists only on this device, so it can't move to or from
        // a protocol that keeps mail on the server.
        if let Some(existing) = &existing
            && (existing.mail_protocol == MailProtocol::Pop3)
                != (args.mail_protocol == MailProtocol::Pop3)
        {
            return Err(CoreError::Auth(
                "an account can't switch between POP3 and IMAP or JMAP".into(),
            ));
        }

        // Verify credentials before storing anything. For JMAP, also pin the
        // exact Mail account selected from the authenticated Session so a
        // later reconnect can never drift to a different shared account.
        let mut pop3_probe = None;
        let jmap_account_id = match args.mail_protocol {
            MailProtocol::Jmap => Some(
                crate::jmap::client::connect_with(
                    &args.email,
                    &args.username,
                    &args.password,
                    &args.jmap_url,
                    None,
                )
                .await?
                .account_id,
            ),
            MailProtocol::Imap => {
                self.check_imap_smtp(&args).await?;
                None
            }
            MailProtocol::Pop3 => {
                pop3_probe = Some(self.check_pop3_smtp(&args).await?);
                None
            }
            MailProtocol::Gmail => return Err(gmail_needs_google_sign_in()),
        };
        let is_new_account = existing.is_none();

        let id = if let Some(existing) = existing {
            if existing.provider != Provider::Generic || existing.auth_kind != AuthKind::Password {
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
                            provider: Provider::Generic,
                            auth_kind: AuthKind::Password,
                            mail_protocol: a.mail_protocol,
                            username: &a.username,
                            jmap_url: &a.jmap_url,
                            jmap_account_id: remote_account.as_deref(),
                            incoming_host: &a.incoming_host,
                            incoming_port: a.incoming_port,
                            smtp_host: &a.smtp_host,
                            smtp_port: a.smtp_port,
                        },
                    )?;
                    let mut settings = repo::accounts::get_config(&transaction, id)?
                        .ok_or_else(|| CoreError::NotFound("account".into()))?
                        .settings;
                    settings.connection = a.connection.clone();
                    repo::accounts::set_settings(&transaction, id, &settings)?;
                    if let Some(probe) = pop3_probe {
                        record_pop3_probe(&transaction, id, probe)?;
                    }
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
                    let transaction = conn.transaction()?;
                    let id = repo::accounts::insert_with_sync_state(
                        &transaction,
                        &repo::accounts::NewAccount {
                            email: &a.email,
                            display_name: a.display_name.as_deref(),
                            avatar_url: None,
                            provider: Provider::Generic,
                            auth_kind: AuthKind::Password,
                            mail_protocol: a.mail_protocol,
                            username: &a.username,
                            jmap_url: &a.jmap_url,
                            jmap_account_id: remote_account.as_deref(),
                            incoming_host: &a.incoming_host,
                            incoming_port: a.incoming_port,
                            smtp_host: &a.smtp_host,
                            smtp_port: a.smtp_port,
                        },
                        &AccountSettings::new(a.connection.clone()),
                        repo::accounts::CREDENTIAL_SETUP_STATE,
                    )?;
                    if let Some(probe) = pop3_probe {
                        record_pop3_probe(&transaction, id, probe)?;
                    }
                    transaction.commit()?;
                    Ok(id)
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
                self.rollback_incomplete_account_credentials(id).await;
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

        if is_new_account
            && let Err(error) = self
                .db
                .write(move |conn| repo::accounts::set_sync_state(conn, id, "idle"))
                .await
        {
            self.rollback_incomplete_account_credentials(id).await;
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
        events::sync_diagnostic("ui_ready", None, "notified");
        self.ui_ready.notify_one();
    }

    /// Finish the rollback for an account whose process stopped after
    /// reserving its database id but before committing credentials. These rows
    /// are excluded from account lists and actor startup.
    async fn recover_incomplete_account_credentials(&self) -> Result<()> {
        let account_ids = self
            .db
            .read(|conn| repo::accounts::credential_setup_ids(conn))
            .await?;
        for account_id in account_ids {
            self.tokens.forget_account(account_id).await;
            credentials::delete_all_async(self.credentials.clone(), account_id).await?;
            self.db
                .write(move |conn| repo::accounts::delete(conn, account_id))
                .await?;
            tracing::info!(
                account_id,
                "rolled back incomplete account credential setup"
            );
        }
        Ok(())
    }

    /// Compensate for a failed cross-store credential commit. Credentials are
    /// deleted before the database id is released so a later account cannot
    /// inherit an orphaned token. A cleanup failure keeps the row in its hidden
    /// setup state for the next recovery attempt.
    async fn rollback_incomplete_account_credentials(&self, account_id: i64) {
        self.tokens.forget_account(account_id).await;
        if let Err(error) =
            credentials::delete_all_async(self.credentials.clone(), account_id).await
        {
            tracing::error!(
                account_id,
                %error,
                "could not remove credentials after account setup failed"
            );
            return;
        }
        if let Err(error) = self
            .db
            .write(move |conn| repo::accounts::delete(conn, account_id))
            .await
        {
            tracing::error!(
                account_id,
                %error,
                "could not remove incomplete account row"
            );
        }
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
                    self.attach_google_calendar(account.id).await?;
                }
                Provider::Microsoft => {
                    // The initial consent already included Calendars.ReadWrite;
                    // this retrieves its Graph-audience token without showing a
                    // second prompt, then discovers Outlook calendars.
                    self.connect_microsoft_calendar(account.id, open_url)
                        .await?;
                }
                Provider::Generic => {}
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
        let servers = match provider {
            Provider::Gmail => &accounts::providers::GMAIL,
            Provider::Microsoft => &accounts::providers::MICROSOFT,
            Provider::Generic => return Err(CoreError::Auth("not an oauth provider".into())),
        };
        credentials::check_available_async(self.credentials.clone()).await?;
        self.recover_incomplete_account_credentials().await?;

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
        let email = outcome.email.clone();
        let existing = self
            .db
            .read({
                let email = email.clone();
                move |conn| repo::accounts::find_by_mailbox(conn, &email)
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
            return self.reconnect(existing, outcome).await;
        }

        let display_name = outcome.display_name.clone();
        let avatar_url = outcome.avatar_url.clone();
        let id = self
            .db
            .write(move |conn| {
                repo::accounts::insert_with_sync_state(
                    conn,
                    &repo::accounts::NewAccount {
                        email: &email,
                        display_name: display_name.as_deref(),
                        avatar_url: avatar_url.as_deref(),
                        provider,
                        auth_kind: AuthKind::Oauth2,
                        mail_protocol: servers.mail_protocol,
                        username: &email,
                        jmap_url: "",
                        jmap_account_id: None,
                        incoming_host: servers.incoming_host,
                        incoming_port: servers.incoming_port,
                        smtp_host: servers.smtp_host,
                        smtp_port: servers.smtp_port,
                    },
                    &AccountSettings::new(servers.connection()),
                    repo::accounts::CREDENTIAL_SETUP_STATE,
                )
            })
            .await?;

        if let Err(error) = self
            .tokens
            .store_initial(
                id,
                outcome.access_token,
                outcome.expires_in,
                outcome.refresh_token,
                outcome.client_id,
                outcome.client_secret,
            )
            .await
        {
            self.rollback_incomplete_account_credentials(id).await;
            return Err(error);
        }

        if let Err(error) = self
            .db
            .write(move |conn| repo::accounts::set_sync_state(conn, id, "idle"))
            .await
        {
            self.rollback_incomplete_account_credentials(id).await;
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
            Provider::Generic => return Err(CoreError::Auth("not an oauth provider".into())),
        };
        credentials::check_available_async(self.credentials.clone()).await?;
        let outcome = tokio::select! {
            // Hint the provider at the account being repaired so the browser
            // preselects it instead of silently reusing whatever session is
            // active (which then fails the mailbox check).
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
        self.reconnect(account, outcome).await
    }

    /// Store a completed sign-in for an existing account and resume its sync,
    /// which waits paused while the account needs a sign-in.
    async fn reconnect(
        &self,
        account: Account,
        outcome: oauth::flow::OAuthOutcome,
    ) -> Result<Account> {
        let account_id = account.id;
        self.store_sign_in(&account, outcome).await?;

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

    /// Store the tokens and profile of a sign-in for an existing account.
    /// When the provider spells the account's address differently, such as
    /// Gmail's dotted form of an imported login, the account takes that
    /// spelling and its mail worker restarts with it.
    async fn store_sign_in(
        &self,
        account: &Account,
        outcome: oauth::flow::OAuthOutcome,
    ) -> Result<()> {
        let account_id = account.id;
        let respelled = sign_in_respells(account.provider, &account.email, &outcome.email)?;
        if respelled {
            let email = outcome.email.clone();
            self.db
                .write(move |conn| {
                    let transaction = conn.transaction()?;
                    repo::accounts::set_signed_in_address(&transaction, account_id, &email)?;
                    transaction.commit()?;
                    Ok(())
                })
                .await?;
        }

        let display_name = outcome.display_name.clone();
        let avatar_url = outcome.avatar_url.clone();
        let stored = async {
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
            self.db
                .write(move |conn| {
                    repo::accounts::update_oauth_identity(
                        conn,
                        account_id,
                        display_name.as_deref(),
                        avatar_url.as_deref(),
                    )
                })
                .await
        }
        .await;
        // Only an account that never signed in can have another spelling of
        // its Gmail address, so its worker waits paused with no work in
        // flight. It restarts with the new spelling even when the tokens
        // could not be stored.
        if respelled {
            self.restart_actor(account_id).await;
        }
        stored
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
                    "DELETE FROM calendar_connections WHERE account_id = ?1",
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
        let _files_guard = self.file_work_lock.lock().await;
        if let Some(h) = self.handles.write().await.remove(&account_id) {
            h.abort();
        }
        self.cal_handles.write().await.remove(&account_id);
        if let Some(handle) = self.card_handles.write().await.remove(&account_id) {
            handle.abort();
        }
        self.tokens.forget_account(account_id).await;
        self.delete_calendar_passwords(account_id).await?;
        self.purge_calendar_account(account_id).await?;
        self.files_db
            .write(move |conn| {
                conn.execute("DELETE FROM connections WHERE account_id=?1", [account_id])?;
                Ok(())
            })
            .await?;
        credentials::delete_all_async(self.credentials.clone(), account_id).await?;
        self.db
            .write(move |conn| {
                conn.execute(
                    "DELETE FROM app_settings WHERE key = ?1",
                    [format!("files:{account_id}")],
                )?;
                let mut settings = repo::settings::get(conn)?;
                for profile in &mut settings.mail_profiles {
                    profile.account_ids.retain(|id| *id != account_id);
                }
                retire_active_mail_profile(&mut settings);
                let account_key = account_id.to_string();
                settings.account_colors.remove(&account_key);
                settings.account_short_names.remove(&account_key);
                settings.account_themes.remove(&account_key);
                settings.signature_defaults.remove(&account_key);
                settings
                    .signature_list
                    .retain(|signature| signature.account_id != account_id);
                repo::settings::set(conn, &settings)?;
                repo::accounts::delete(conn, account_id)
            })
            .await?;
        for directory in [
            self.paths.mail_dir(account_id),
            self.paths.attachments_dir(account_id),
            self.paths.files_cache_dir(account_id),
            self.paths.files_staging_dir(account_id),
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

        if !self.paths.calendar_db_file().exists() {
            return Ok(());
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
                     UNION SELECT account_id FROM calendar_connections WHERE account_id != 0
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
                if let Err(error) = self.delete_calendar_passwords(account_id).await {
                    tracing::warn!(account_id, "orphaned calendar passwords stay: {error}");
                }
                self.purge_calendar_account(account_id).await?;
                self.files_db
                    .write(move |conn| {
                        conn.execute("DELETE FROM connections WHERE account_id=?1", [account_id])?;
                        Ok(())
                    })
                    .await?;
            }
        }
        let detached = self.detach_orphaned_calendar_message_links().await?;
        if detached > 0 {
            tracing::info!(detached, "detached orphaned calendar message links");
        }
        Ok(())
    }

    /// Remove what interrupted mail writes left behind: temporary files, and
    /// POP3 originals nothing refers to because their download stopped
    /// before it was committed. That session never sent QUIT, so the server
    /// still has those messages. Runs before any account worker writes.
    async fn remove_interrupted_mail_files(&self) -> Result<usize> {
        let mut accounts = match tokio::fs::read_dir(self.paths.mail_root()).await {
            Ok(accounts) => accounts,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(error.into()),
        };
        let stored: HashSet<std::ffi::OsString> = self
            .db
            .read(|conn| {
                let mut statement =
                    conn.prepare("SELECT raw_path FROM messages WHERE raw_path LIKE '%' || ?1")?;
                let paths = statement
                    .query_map([sync::pop3::ORIGINAL_SUFFIX], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(paths
                    .iter()
                    .filter_map(|path| std::path::Path::new(path).file_name())
                    .map(std::ffi::OsStr::to_owned)
                    .collect())
            })
            .await?;
        let mut removed = 0;
        while let Some(account) = accounts.next_entry().await? {
            if !account.file_type().await?.is_dir() {
                continue;
            }
            removed += crate::file_io::remove_partial_writes(account.path()).await?;
            let mut files = tokio::fs::read_dir(account.path()).await?;
            while let Some(file) = files.next_entry().await? {
                let name = file.file_name();
                if name
                    .to_str()
                    .is_some_and(|name| name.ends_with(sync::pop3::ORIGINAL_SUFFIX))
                    && !stored.contains(&name)
                    && file.file_type().await?.is_file()
                {
                    tokio::fs::remove_file(file.path()).await?;
                    removed += 1;
                }
            }
        }
        Ok(removed)
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
                     WHERE EXISTS (
                         SELECT 1 FROM messages m
                         WHERE m.id = draft_attachments.draft_id AND m.is_draft = 0
                     )",
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
                    "DELETE FROM contact_entries;
                     DELETE FROM contacts;
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
                     DELETE FROM calendar_connections;",
                )?;
                Ok(())
            })
            .await?;

        self.files_db
            .write(|c| {
                c.execute("DELETE FROM connections", [])?;
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

    /// Export coherent, integrity-checked copies of all three SQLite stores plus a
    /// versioned manifest. Credentials remain in the platform keyring and are
    /// intentionally outside this database snapshot.
    pub async fn create_database_snapshot(
        &self,
        destination: std::path::PathBuf,
    ) -> Result<DatabaseSnapshotManifest> {
        self.check_export_destination(&destination).await?;
        let _files_guard = self.file_work_lock.lock().await;
        db::snapshot::create(
            &self.db,
            &self.calendar_db,
            &self.files_db,
            Some(self.paths.data_dir.join("file_transfers")),
            &destination,
        )
        .await
    }

    /// Exports go outside the app's own directories, where nothing the app
    /// cleans up can take them away, and never replace an earlier export.
    async fn check_export_destination(&self, destination: &std::path::Path) -> Result<()> {
        let parent = destination
            .parent()
            .ok_or_else(|| CoreError::Other("the destination has no parent directory".into()))?;
        let canonical_parent = tokio::fs::canonicalize(parent).await.map_err(|error| {
            CoreError::Other(format!(
                "resolving the destination {}: {error}",
                parent.display()
            ))
        })?;
        let data_dir = tokio::fs::canonicalize(&self.paths.data_dir).await?;
        let cache_dir = tokio::fs::canonicalize(&self.paths.cache_dir).await?;
        if canonical_parent.starts_with(&data_dir) || canonical_parent.starts_with(&cache_dir) {
            return Err(CoreError::Other(
                "exports must go outside the application data and cache directories".into(),
            ));
        }
        if tokio::fs::try_exists(destination).await? {
            return Err(CoreError::Other(format!(
                "{} already exists",
                destination.display()
            )));
        }
        Ok(())
    }

    /// Write a POP3 account's mail to a new directory as one mbox file per
    /// folder, so mail that exists only on this device can be kept before it
    /// is removed. The directory must not exist yet.
    pub async fn export_account_mail(
        &self,
        account_id: i64,
        destination: std::path::PathBuf,
    ) -> Result<MailExport> {
        let config = self.folder_account_config(account_id).await?;
        match config.mail_protocol {
            MailProtocol::Pop3 => {}
            MailProtocol::Gmail | MailProtocol::Imap | MailProtocol::Jmap => {
                return Err(CoreError::Other(
                    "only POP3 accounts keep their mail on this device alone".into(),
                ));
            }
        }
        self.check_export_destination(&destination).await?;
        let staging = crate::file_io::staging_directory(&destination)?;
        tokio::fs::create_dir(&staging).await?;
        let result = async {
            crate::file_io::restrict_permissions(&staging, &[])?;
            let export = self.write_mail_export(account_id, &staging).await?;
            crate::file_io::publish_directory(&staging, &destination)?;
            Ok(export)
        }
        .await;
        if result.is_err() {
            let _ = tokio::fs::remove_dir_all(&staging).await;
        }
        result
    }

    async fn write_mail_export(
        &self,
        account_id: i64,
        directory: &std::path::Path,
    ) -> Result<MailExport> {
        use tokio::io::AsyncWriteExt;
        let folders = self
            .db
            .read(move |conn| repo::folders::list(conn, Some(account_id)))
            .await?;
        let mut names = HashSet::new();
        let mut export = MailExport::default();
        for folder in folders {
            let folder_id = folder.id;
            let messages = self
                .db
                .read(move |conn| repo::messages::in_folder_for_export(conn, folder_id))
                .await?;
            if messages.is_empty() {
                continue;
            }
            let path = directory.join(mbox::file_name(&folder.path, &mut names));
            let mut file = tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .await?;
            for message in messages {
                let original = match &message.raw_path {
                    Some(raw_path) => self
                        .read_original(
                            account_id,
                            message.id,
                            raw_path,
                            MAX_CACHED_MESSAGE_BYTES,
                            "exported message",
                        )
                        .await
                        .inspect_err(|error| {
                            tracing::warn!(message_id = message.id, %error, "message left out of a mail export");
                        })
                        .ok(),
                    None if message.is_draft => Some(self.draft_message(message.id).await?),
                    None => None,
                };
                let Some(original) = original else {
                    export.missing += 1;
                    continue;
                };
                file.write_all(mbox::separator(&message.from, message.date).as_bytes())
                    .await?;
                file.write_all(&mbox::entry(&original)).await?;
                export.messages += 1;
            }
            // The account may be removed right after its export.
            file.sync_all().await?;
            crate::file_io::restrict_permissions(directory, &[&path])?;
            export.folders += 1;
        }
        Ok(export)
    }

    /// A draft as a message file. Drafts have no original until they are
    /// sent.
    async fn draft_message(&self, draft_id: i64) -> Result<Vec<u8>> {
        let (detail, bcc, staged) = self
            .db
            .read(move |conn| {
                let detail = repo::messages::detail(conn, draft_id)?;
                let bcc: String = conn.query_row(
                    "SELECT bcc_json FROM messages WHERE id = ?1",
                    [draft_id],
                    |row| row.get(0),
                )?;
                let mut statement = conn.prepare(
                    "SELECT file_path, filename FROM draft_attachments WHERE draft_id = ?1",
                )?;
                let staged = statement
                    .query_map([draft_id], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok((detail, serde_json::from_str::<Vec<Address>>(&bcc)?, staged))
            })
            .await?;
        // Like sending, only ever read files from the app's own staging area.
        let staging_root = tokio::fs::canonicalize(self.paths.draft_attachments_dir())
            .await
            .ok();
        let mut attachments = Vec::new();
        let mut budget = MAX_DRAFT_ATTACHMENT_BYTES;
        for (path, filename) in staged {
            let path = tokio::fs::canonicalize(&path).await?;
            if !staging_root
                .as_ref()
                .is_some_and(|root| path.starts_with(root))
            {
                return Err(CoreError::Other(format!(
                    "attachment {filename}: refusing to read a file outside the staging area"
                )));
            }
            let bytes = crate::file_io::read(&path, budget, "draft attachment").await?;
            budget -= bytes.len();
            attachments.push(crate::mime::OutgoingAttachment {
                mime_type: crate::queue::mime_guess_from_name(&filename),
                filename,
                bytes,
            });
        }
        let domain = detail
            .from
            .email
            .split('@')
            .nth(1)
            .unwrap_or("localhost")
            .to_owned();
        let (_, raw) = crate::mime::build_message_owned(crate::mime::OutgoingMessage {
            from: detail.from.clone(),
            reply_to: None,
            to: &detail.to,
            cc: &detail.cc,
            bcc: &bcc,
            subject: &detail.subject,
            body_text: detail.text_body.as_deref().unwrap_or(""),
            body_html: detail.html_body.as_deref(),
            in_reply_to: None,
            references: &[],
            message_id: None,
            message_id_domain: &domain,
            attachments,
        })?;
        Ok(raw)
    }

    pub async fn sync_now(&self, account_id: Option<i64>) -> Result<()> {
        events::sync_diagnostic("refresh", account_id, "requested");
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
        if receivers.is_empty() {
            events::sync_diagnostic("refresh", account_id, "no_workers");
        }
        for receiver in receivers {
            let result =
                match tokio::time::timeout(std::time::Duration::from_secs(45), receiver).await {
                    Err(_) => {
                        events::sync_diagnostic("refresh", account_id, "timeout");
                        return Err(CoreError::Other("Inbox sync timed out".into()));
                    }
                    Ok(Err(_)) => {
                        events::sync_diagnostic("refresh", account_id, "worker_stopped");
                        return Err(CoreError::Other("sync actor stopped".into()));
                    }
                    Ok(Ok(result)) => result,
                };
            if let Err(error) = result {
                events::sync_diagnostic("refresh", account_id, "worker_error");
                return Err(CoreError::Other(error));
            }
        }
        events::sync_diagnostic("refresh", account_id, "complete");
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
        self.list_threads_where(
            view,
            split_id,
            account_id,
            None,
            label_id,
            folder_id,
            page.into(),
        )
        .await
    }

    /// `list_threads` across a set of accounts, such as a mail profile's,
    /// instead of one account or all of them. An empty set lists nothing.
    pub async fn list_threads_for_accounts(
        &self,
        view: View,
        split_id: Option<i64>,
        account_ids: Vec<i64>,
        label_id: Option<i64>,
        folder_id: Option<i64>,
        page: impl Into<ThreadPageRequest>,
    ) -> Result<ThreadPage> {
        self.list_threads_where(
            view,
            split_id,
            None,
            Some(account_ids),
            label_id,
            folder_id,
            page.into(),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn list_threads_where(
        &self,
        view: View,
        split_id: Option<i64>,
        account_id: Option<i64>,
        account_ids: Option<Vec<i64>>,
        label_id: Option<i64>,
        folder_id: Option<i64>,
        page: ThreadPageRequest,
    ) -> Result<ThreadPage> {
        use repo::threads::TabFilter;
        let ThreadPageRequest { cursor, limit } = page;
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
                        account_ids,
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
                        account_ids: None,
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

    /// Load the lightweight message sequence for a conversation. Bodies,
    /// inline images, and attachment rows are deliberately omitted so a long
    /// thread can be opened without materializing every rich message.
    pub async fn get_thread_outline(&self, thread_id: i64) -> Result<ThreadDetail> {
        self.db
            .read(move |conn| {
                let thread = repo::threads::get_summary(conn, thread_id)?
                    .ok_or_else(|| CoreError::NotFound(format!("thread {thread_id}")))?;
                let messages = repo::messages::list_outline_for_thread(conn, thread_id)?;
                Ok(ThreadDetail { thread, messages })
            })
            .await
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
            reply_to: None,
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
        let raw = crate::mail_security::Gpg::default()
            .protect(&raw, &cfg.settings.security, &cfg.email, out.to)
            .await?;
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
        let fallback = Arc::new(html);
        let worker_html = Arc::clone(&fallback);
        let rewritten = tokio::task::spawn_blocking(move || {
            let mut map = std::collections::HashMap::new();
            for (content_id, mime, bytes) in fetched {
                map.insert(
                    crate::mime::normalize_cid(&content_id),
                    (
                        mime.unwrap_or_else(|| "application/octet-stream".to_string()),
                        bytes,
                    ),
                );
            }
            crate::mime::rewrite_cid_src_with_images(&worker_html, &map)
        })
        .await;
        rewritten
            .unwrap_or_else(|_| Arc::try_unwrap(fallback).unwrap_or_else(|html| (*html).clone()))
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

        // Only messages with fetched images need rewriting. Move the others
        // straight to the result so their HTML is never copied for this worker.
        let fetched_ids: HashSet<i64> = fetched.iter().map(|(id, _, _, _)| *id).collect();
        let mut to_rewrite = Vec::new();
        for (id, html) in need {
            if fetched_ids.contains(&id) {
                to_rewrite.push((id, html));
            } else {
                out.insert(id, html);
            }
        }

        // Extraction + base64 of (possibly large) image parts is CPU-bound.
        let fallback = Arc::new(to_rewrite);
        let worker_need = Arc::clone(&fallback);
        let rewritten = tokio::task::spawn_blocking(move || {
            let mut maps: HashMap<i64, HashMap<String, (String, Vec<u8>)>> = HashMap::new();
            for (message_id, content_id, mime, bytes) in fetched {
                maps.entry(message_id).or_default().insert(
                    crate::mime::normalize_cid(&content_id),
                    (
                        mime.unwrap_or_else(|| "application/octet-stream".to_string()),
                        bytes,
                    ),
                );
            }
            worker_need
                .iter()
                .map(|(id, html)| match maps.get(id) {
                    Some(map) => {
                        let html = crate::mime::rewrite_cid_src_with_images(html, map);
                        (*id, html)
                    }
                    None => (*id, html.clone()),
                })
                .collect::<Vec<_>>()
        })
        .await;
        let rewritten = rewritten
            .unwrap_or_else(|_| Arc::try_unwrap(fallback).unwrap_or_else(|html| (*html).clone()));
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
            Some(folder_id) => Some(self.folder_for_child(account_id, folder_id).await?),
            None => None,
        };
        match config.mail_protocol {
            MailProtocol::Gmail => {
                if name.contains('/') {
                    return Err(CoreError::Other(
                        "folder names cannot contain the hierarchy separator \"/\"".into(),
                    ));
                }
                let full_name = parent
                    .as_ref()
                    .map(|parent| format!("{}/{}", parent.path, name))
                    .unwrap_or(name);
                sync::gmail::create_user_folder(&self.sync_ctx(), &config, &full_name).await?;
            }
            MailProtocol::Jmap => {
                let secret =
                    credentials::load_async(self.credentials.clone(), config.id, Slot::Password)
                        .await?;
                let connected = jmap::client::connect(&config, &secret).await?;
                if parent.is_none() && !connected.may_create_top_level_mailbox {
                    return Err(CoreError::Other(
                        "the server does not allow creating top-level mailboxes".into(),
                    ));
                }
                let parent_remote = parent.as_ref().and_then(|folder| folder.jmap_id.clone());
                let parent_local_id = parent.as_ref().map(|folder| folder.id);
                let mailbox = connected
                    .mailbox_create(&name, parent_remote, jmap_client::mailbox::Role::None)
                    .await
                    .map_err(jmap::client::map_error)?;
                let rights = mailbox.my_rights();
                let permissions = repo::folders::FolderPermissions {
                    can_create_children: rights.is_some_and(|rights| rights.may_create_child()),
                    can_rename: rights.is_some_and(|rights| rights.may_rename()),
                    can_delete: rights.is_some_and(|rights| rights.may_delete()),
                };
                let remote_id = mailbox
                    .id()
                    .ok_or_else(|| CoreError::Jmap("Mailbox/set returned no id".into()))?
                    .to_owned();
                let display_path = parent
                    .as_ref()
                    .map(|parent| format!("{} / {name}", parent.path))
                    .unwrap_or(name);
                self.db
                    .write(move |conn| {
                        let folder_id = repo::folders::upsert_jmap(
                            conn,
                            account_id,
                            &remote_id,
                            &display_path,
                            None,
                        )?;
                        repo::folders::set_parent(conn, folder_id, parent_local_id)?;
                        repo::folders::set_permissions(conn, folder_id, permissions)?;
                        Ok(())
                    })
                    .await?;
            }
            MailProtocol::Imap => {
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
                let parent_local_id = parent.as_ref().map(|folder| folder.id);
                let remote_name = parent
                    .as_ref()
                    .map(|parent| format!("{}{delimiter}{encoded_leaf}", parent.path))
                    .unwrap_or(encoded_leaf);
                let mut session = self.connect_folder_imap(&config).await?;
                imap::create_folder(&mut session, &remote_name).await?;
                imap::logout(session).await;
                self.db
                    .write(move |conn| {
                        let folder_id = repo::folders::upsert(
                            conn,
                            account_id,
                            &remote_name,
                            Some(&delimiter),
                            None,
                        )?;
                        repo::folders::set_parent(conn, folder_id, parent_local_id)?;
                        Ok(())
                    })
                    .await?;
            }
            MailProtocol::Pop3 => {
                let delimiter = repo::pop3::FOLDER_DELIMITER;
                if name.contains(delimiter) {
                    return Err(CoreError::Other(format!(
                        "folder names cannot contain the hierarchy separator {delimiter:?}"
                    )));
                }
                let parent_local_id = parent.as_ref().map(|folder| folder.id);
                let path = parent
                    .as_ref()
                    .map(|parent| format!("{}{delimiter}{name}", parent.path))
                    .unwrap_or(name);
                self.db
                    .write(move |conn| {
                        let tx = conn.transaction()?;
                        if repo::folders::path_exists(&tx, account_id, &path)? {
                            return Err(folder_exists());
                        }
                        let folder_id =
                            repo::folders::upsert(&tx, account_id, &path, Some(delimiter), None)?;
                        repo::folders::set_parent(&tx, folder_id, parent_local_id)?;
                        tx.commit()?;
                        Ok(())
                    })
                    .await?;
            }
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
        if !folder.can_rename {
            return Err(CoreError::Other(
                "the server does not allow renaming this folder".into(),
            ));
        }
        let account_id = folder.account_id;
        let config = self.folder_account_config(account_id).await?;
        let delimiter = folder_path_delimiter(config.mail_protocol, &folder);
        if name.contains(&delimiter) {
            return Err(CoreError::Other(format!(
                "folder names cannot contain the hierarchy separator {delimiter:?}"
            )));
        }
        let old_name = folder.path.clone();
        let parent_prefix = old_name.rsplit_once(&delimiter).map(|(parent, _)| parent);
        match config.mail_protocol {
            MailProtocol::Gmail => {
                let full_name = parent_prefix
                    .map(|parent| format!("{parent}{delimiter}{name}"))
                    .unwrap_or(name);
                sync::gmail::rename_user_folder(&self.sync_ctx(), &config, folder_id, &full_name)
                    .await?;
            }
            MailProtocol::Jmap => {
                let secret =
                    credentials::load_async(self.credentials.clone(), config.id, Slot::Password)
                        .await?;
                let connected = jmap::client::connect(&config, &secret).await?;
                let remote_id = folder
                    .jmap_id
                    .as_deref()
                    .ok_or_else(|| CoreError::NotFound(format!("remote folder {folder_id}")))?;
                connected
                    .mailbox_rename(remote_id, &name)
                    .await
                    .map_err(jmap::client::map_error)?;
                let new_name = parent_prefix
                    .map(|parent| format!("{parent}{delimiter}{name}"))
                    .unwrap_or(name);
                self.db
                    .write(move |conn| {
                        repo::folders::rename_tree(
                            conn, account_id, &old_name, &new_name, &delimiter,
                        )
                    })
                    .await?;
            }
            MailProtocol::Imap => {
                let encoded = imap::encode_mailbox_name(&name);
                let new_name = parent_prefix
                    .map(|parent| format!("{parent}{delimiter}{encoded}"))
                    .unwrap_or(encoded);
                let mut session = self.connect_folder_imap(&config).await?;
                imap::rename_folder(&mut session, &old_name, &new_name).await?;
                imap::logout(session).await;
                self.db
                    .write(move |conn| {
                        repo::folders::rename_tree(
                            conn, account_id, &old_name, &new_name, &delimiter,
                        )
                    })
                    .await?;
            }
            MailProtocol::Pop3 => {
                let new_name = parent_prefix
                    .map(|parent| format!("{parent}{delimiter}{name}"))
                    .unwrap_or(name);
                self.db
                    .write(move |conn| {
                        let tx = conn.transaction()?;
                        if new_name != old_name
                            && repo::folders::path_exists(&tx, account_id, &new_name)?
                        {
                            return Err(folder_exists());
                        }
                        repo::folders::rename_tree(
                            &tx, account_id, &old_name, &new_name, &delimiter,
                        )?;
                        tx.commit()?;
                        Ok(())
                    })
                    .await?;
            }
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
        if !folder.can_delete {
            return Err(CoreError::Other(
                "the server does not allow deleting this folder".into(),
            ));
        }
        let account_id = folder.account_id;
        let config = self.folder_account_config(account_id).await?;
        let delimiter = folder_path_delimiter(config.mail_protocol, &folder);
        let prefix = format!("{}{delimiter}", folder.path);
        let mut tree = self
            .db
            .read(move |conn| {
                Ok(repo::folders::list(conn, Some(account_id))?
                    .into_iter()
                    .filter(|candidate| {
                        candidate.id == folder_id || candidate.path.starts_with(&prefix)
                    })
                    .collect::<Vec<_>>())
            })
            .await?;
        tree.sort_by_key(|candidate| std::cmp::Reverse(candidate.path.len()));
        match config.mail_protocol {
            MailProtocol::Gmail => {
                for candidate in &tree {
                    sync::gmail::delete_user_folder(&self.sync_ctx(), &config, candidate.id)
                        .await?;
                }
            }
            MailProtocol::Jmap => {
                let secret =
                    credentials::load_async(self.credentials.clone(), config.id, Slot::Password)
                        .await?;
                let connected = jmap::client::connect(&config, &secret).await?;
                for candidate in &tree {
                    let remote_id = candidate.jmap_id.as_deref().ok_or_else(|| {
                        CoreError::NotFound(format!("remote folder {}", candidate.id))
                    })?;
                    connected
                        .mailbox_destroy(remote_id, true)
                        .await
                        .map_err(jmap::client::map_error)?;
                }
                sync::delete_folder_tree(&self.sync_ctx(), account_id, folder.path, delimiter)
                    .await?;
            }
            MailProtocol::Imap => {
                let mut session = self.connect_folder_imap(&config).await?;
                for candidate in &tree {
                    imap::delete_folder(&mut session, &candidate.path).await?;
                }
                imap::logout(session).await;
                sync::delete_folder_tree(&self.sync_ctx(), account_id, folder.path, delimiter)
                    .await?;
            }
            // The folders and their mail exist only here.
            MailProtocol::Pop3 => {
                sync::delete_folder_tree(&self.sync_ctx(), account_id, folder.path, delimiter)
                    .await?;
            }
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

    async fn folder_for_child(
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
        if !folder.can_create_children {
            return Err(CoreError::Other(
                "the server does not allow subfolders inside this folder".into(),
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
        imap::connect_with_settings(
            &config.incoming_host,
            config.incoming_port,
            credentials,
            &config.settings.connection,
        )
        .await
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

    /// Queue a provider-side purge of Trash for one account or all accounts.
    /// The provider enumerates the whole mailbox, including mail outside the
    /// local history window and messages not loaded in the UI.
    pub async fn empty_trash(&self, account_id: Option<i64>) -> Result<ActionResult> {
        let queued = self
            .db
            .write(move |conn| {
                let tx = conn.transaction()?;
                let mut stmt = tx.prepare(
                    "SELECT account_id, id FROM folders WHERE role = 'trash'
                 AND (?1 IS NULL OR account_id = ?1)",
                )?;
                let folders = stmt
                    .query_map(rusqlite::params![account_id], |row| {
                        Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                drop(stmt);
                if folders.is_empty() {
                    return Err(CoreError::NotFound(
                        "Trash folder is not available yet".into(),
                    ));
                }
                let mut queued = Vec::new();
                for (account_id, folder_id) in folders {
                    let active: bool = tx.query_row(
                        "SELECT EXISTS(SELECT 1 FROM pending_actions
                     WHERE account_id = ?1 AND kind = 'empty_trash'
                       AND state IN ('pending','inflight'))",
                        rusqlite::params![account_id],
                        |row| row.get(0),
                    )?;
                    if !active {
                        let action_id = repo::actions::enqueue(
                            &tx,
                            account_id,
                            "empty_trash",
                            None,
                            None,
                            &serde_json::json!({"folderId": folder_id}),
                            None,
                        )?;
                        queued.push((action_id, account_id));
                    }
                }
                tx.commit()?;
                Ok(queued)
            })
            .await?;
        for &(_, account_id) in &queued {
            self.nudge(Some(account_id), || SyncCmd::RunActions).await;
        }
        Ok(ActionResult {
            action_ids: queued.into_iter().map(|(id, _)| id).collect(),
        })
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

    /// Wait for one explicitly requested send to finish.
    ///
    /// The pending-action queue remains the durable owner of delivery, but an
    /// interactive composer needs a stronger answer than "the action was
    /// queued". If the first delivery attempt fails, cancel its scheduled
    /// retry before returning the error so an editable preserved draft cannot
    /// also be delivered later behind the user's back.
    pub async fn wait_for_send(&self, action_id: i64) -> Result<()> {
        let observed_at = now_ms();
        loop {
            let status = self
                .db
                .read(move |conn| {
                    Ok(conn
                        .query_row(
                            "SELECT kind, state, last_error, not_before
                             FROM pending_actions WHERE id = ?1",
                            rusqlite::params![action_id],
                            |row| {
                                Ok((
                                    row.get::<_, String>(0)?,
                                    row.get::<_, String>(1)?,
                                    row.get::<_, Option<String>>(2)?,
                                    row.get::<_, Option<i64>>(3)?,
                                ))
                            },
                        )
                        .optional()?)
                })
                .await?;
            let Some((kind, state, last_error, not_before)) = status else {
                return Err(CoreError::NotFound("send action".into()));
            };
            if kind != "send" {
                return Err(CoreError::Other(format!(
                    "action {action_id} is not a send action"
                )));
            }

            match state.as_str() {
                "done" => return Ok(()),
                "failed" => {
                    return Err(CoreError::Other(
                        last_error.unwrap_or_else(|| "message delivery failed".into()),
                    ));
                }
                "cancelled" => {
                    return Err(CoreError::Other("message delivery was cancelled".into()));
                }
                // A retryable failure has already been written back to the
                // pending row. There is a backoff before another claim, so the
                // cancellation normally wins immediately; if another worker
                // did claim it, keep observing that definitive attempt.
                "pending" if last_error.is_some() => {
                    if self.cancel_send(action_id).await? {
                        return Err(CoreError::Other(last_error.unwrap()));
                    }
                }
                "pending" if send_start_expired(now_ms(), observed_at, not_before) => {
                    if self.cancel_send(action_id).await? {
                        return Err(CoreError::Smtp(format!(
                            "message delivery did not start within {} seconds of its scheduled time",
                            SEND_START_TIMEOUT.as_secs()
                        )));
                    }
                }
                "pending" | "inflight" => {}
                other => {
                    return Err(CoreError::Other(format!(
                        "send action entered an unknown state: {other}"
                    )));
                }
            }

            tokio::time::sleep(SEND_STATUS_POLL_INTERVAL).await;
        }
    }

    pub async fn save_draft(&self, args: SaveDraftArgs) -> Result<i64> {
        // Resolve the visible From address independently of the authenticated
        // account identity. Only provider-discovered, verified identities can
        // cross this boundary; the canonical provider display name is stored.
        let sender = self
            .resolve_sender_identity(
                args.account_id,
                args.from.as_ref().map(|address| address.email.as_str()),
            )
            .await?
            .address();
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

                let mail_protocol: MailProtocol = tx.query_row(
                    "SELECT mail_protocol FROM accounts WHERE id = ?1",
                    rusqlite::params![args.account_id],
                    |row| row.get(0),
                )?;
                let gmail = mail_protocol == MailProtocol::Gmail;
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
                        if gmail { "DRAFT" } else { "Drafts" },
                        Some("/"),
                        Some(roles::DRAFTS),
                    )?,
                };
                let gmail_all_folder = if gmail {
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
                                    bcc_json = ?5, date = ?6, from_name = ?7, from_addr = ?8
                              WHERE id = ?1 AND account_id = ?9 AND is_draft = 1",
                            rusqlite::params![
                                id,
                                args.subject,
                                serde_json::to_string(&args.to)?,
                                serde_json::to_string(&args.cc)?,
                                serde_json::to_string(&args.bcc)?,
                                now_ms(),
                                sender.name,
                                sender.email,
                                args.account_id,
                            ],
                        )?;
                        if updated != 1 {
                            return Err(CoreError::NotFound("draft".into()));
                        }
                        id
                    }
                    None => {
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
                            from: Some(sender.clone()),
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
                let security = repo::accounts::get_config(&tx, args.account_id)?.ok_or_else(|| CoreError::NotFound("account".into()))?.settings.security;
                // Protected drafts stay local until explicit Send. Even incomplete
                // recipient lists must never cause a plaintext server upload.
                let uploads_drafts = match mail_protocol {
                    MailProtocol::Gmail | MailProtocol::Jmap => true,
                    MailProtocol::Imap | MailProtocol::Pop3 => false,
                };
                let sync_remote_account = if !security.enabled() && uploads_drafts {
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
                        "SELECT m.account_id, m.from_name, m.from_addr,
                                m.to_json, m.cc_json, m.bcc_json, m.subject,
                                COALESCE(b.text_body, ''), b.html_body,
                                COALESCE(dm.mode, 'new'), dm.in_reply_to_message_id,
                                m.is_draft
                         FROM messages m
                         LEFT JOIN message_bodies b ON b.message_id = m.id
                         LEFT JOIN drafts_meta dm ON dm.message_id = m.id
                         WHERE m.id = ?1",
                        rusqlite::params![draft_id],
                        |row| {
                            let is_draft = row.get::<_, i64>(11)? != 0;
                            let from_name = row.get::<_, Option<String>>(1)?;
                            let from_email = row.get::<_, Option<String>>(2)?;
                            Ok((
                                SaveDraftArgs {
                                    draft_id: Some(draft_id),
                                    account_id: row.get(0)?,
                                    from: from_email.map(|email| Address {
                                        name: from_name,
                                        email,
                                    }),
                                    to: repo::parse_json_column(&row.get::<_, String>(3)?, 3)?,
                                    cc: repo::parse_json_column(&row.get::<_, String>(4)?, 4)?,
                                    bcc: repo::parse_json_column(&row.get::<_, String>(5)?, 5)?,
                                    subject: row.get(6)?,
                                    body_text: row.get(7)?,
                                    body_html: row.get(8)?,
                                    mode: row.get(9)?,
                                    in_reply_to_message_id: row.get(10)?,
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
                                COALESCE(f.role, ''), m.gm_msgid, m.gmail_draft_id,
                                a.mail_protocol, m.jmap_id
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
                                row.get::<_, Option<String>>(6)?,
                                row.get::<_, Option<String>>(7)?,
                                row.get::<_, MailProtocol>(8)?,
                                row.get::<_, Option<String>>(9)?,
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
                match row.8 {
                    MailProtocol::Jmap => {
                        conn.execute(
                            "UPDATE pending_actions SET state='cancelled',finished_at=?2
                             WHERE message_id=?1 AND kind='save_draft'
                               AND state IN ('pending','inflight')",
                            rusqlite::params![draft_id, now_ms()],
                        )?;
                        if let Some(remote_id) = row.9.as_deref() {
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
                    }
                    MailProtocol::Gmail => {
                        // Cancel any queued autosaves before deleting the local
                        // row. If a remote resource already exists, retain its
                        // opaque ids in a message-independent delete action.
                        conn.execute(
                            "UPDATE pending_actions SET state = 'cancelled', finished_at = ?2
                             WHERE message_id = ?1 AND kind = 'save_draft'
                               AND state = 'pending'",
                            rusqlite::params![draft_id, now_ms()],
                        )?;
                        if row.6.is_some() || row.7.is_some() {
                            repo::actions::enqueue(
                                conn,
                                row.0,
                                "delete_draft",
                                None,
                                row.1,
                                &serde_json::json!({
                                    "gmailMessageId": row.6,
                                    "gmailDraftId": row.7,
                                }),
                                None,
                            )?;
                            nudge_account = Some(row.0);
                        }
                        staged_paths = repo::messages::take_draft_attachment_paths(conn, draft_id)?;
                        repo::messages::delete(conn, draft_id)?;
                    }
                    MailProtocol::Imap if row.3.is_some() && row.5 != roles::TRASH => {
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
                    }
                    MailProtocol::Imap | MailProtocol::Pop3 => {
                        // Purely local drafts have no remote copy to preserve.
                        staged_paths = repo::messages::take_draft_attachment_paths(conn, draft_id)?;
                        repo::messages::delete(conn, draft_id)?;
                    }
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
        let draft_id = args.draft_id;
        let (policy, sender, recipients) = self
            .db
            .read(move |conn| {
                let detail = repo::messages::detail(conn, draft_id)?;
                let config = repo::accounts::get_config(conn, detail.account_id)?
                    .ok_or_else(|| CoreError::NotFound("account".into()))?;
                let bcc: String = conn.query_row(
                    "SELECT bcc_json FROM messages WHERE id=?1 AND is_draft=1",
                    [draft_id],
                    |r| r.get(0),
                )?;
                let mut recipients = detail.to;
                recipients.extend(detail.cc);
                recipients.extend(serde_json::from_str::<Vec<Address>>(&bcc)?);
                let sender = detail.from.email;
                if repo::sender_identities::get_verified(conn, detail.account_id, &sender)?
                    .is_none()
                {
                    return Err(CoreError::Other(format!(
                        "{sender} is not an authorized sender identity for this account"
                    )));
                }
                Ok((config.settings.security, sender, recipients))
            })
            .await?;
        crate::mail_security::Gpg::default()
            .preflight(&policy, &sender, &recipients)
            .await?;
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
                let payload = serde_json::json!({ "draftId": draft_id, "mailSecurity": policy });
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
            draft_id,
            dispatch_at,
        })
    }

    /// Read a downloaded message's original file. A POP3 account keeps the
    /// only copy of its mail here, so when the file has gone missing it is
    /// downloaded again while the server still has the message.
    pub(crate) async fn read_original(
        &self,
        account_id: i64,
        message_id: i64,
        path: &str,
        max_bytes: usize,
        context: &'static str,
    ) -> Result<Vec<u8>> {
        let error = match crate::file_io::read(path, max_bytes, context).await {
            Ok(raw) => return Ok(raw),
            Err(error) => error,
        };
        let protocol = self
            .db
            .read(move |conn| {
                repo::accounts::get_config(conn, account_id)?
                    .map(|config| config.mail_protocol)
                    .ok_or_else(|| CoreError::NotFound(format!("account {account_id}")))
            })
            .await?;
        let missing = !tokio::fs::try_exists(path).await.unwrap_or(true);
        match protocol {
            MailProtocol::Pop3 if missing => {}
            MailProtocol::Pop3 | MailProtocol::Gmail | MailProtocol::Imap | MailProtocol::Jmap => {
                return Err(error);
            }
        }
        let handle = self
            .handles
            .read()
            .await
            .get(&account_id)
            .cloned()
            .ok_or_else(|| CoreError::NotFound(format!("account {account_id}")))?;
        let restored = handle.restore_message(message_id).await?;
        crate::file_io::read(restored, max_bytes, context).await
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
        let (message_id, part_id, imap_section, filename, mime_type, file_path, jmap_blob_id) = self
            .db
            .read(move |conn| {
                conn.query_row(
                    "SELECT message_id, part_id, imap_section, filename, mime_type, file_path, jmap_blob_id
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
                            r.get::<_, Option<String>>(6)?,
                        ))
                    },
                )
                .map_err(Into::into)
            })
            .await?;

        if let Some(path) = file_path
            && tokio::fs::metadata(&path).await.is_ok()
        {
            if let Err(error) = self
                .index_attachment_text(attachment_id, std::path::Path::new(&path))
                .await
            {
                tracing::debug!(%error,"attachment text indexing deferred");
            }
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
        let legacy = match (
            row.raw_path.as_ref().filter(|_| jmap_blob_id.is_none()),
            part_id.as_deref(),
        ) {
            (Some(raw_path), Some(part_id)) => {
                match self
                    .read_original(
                        row.account_id,
                        message_id,
                        raw_path,
                        MAX_CACHED_MESSAGE_BYTES,
                        "cached message",
                    )
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
        if let Err(error) = self.index_attachment_text(attachment_id, &path).await {
            tracing::debug!(%error,"attachment text indexing deferred");
        }
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
                let settings = repo::settings::get(conn)?;
                let account_id = if settings.contact_suggest_all_accounts {
                    None
                } else {
                    account_id
                };
                repo::contacts::autocomplete(
                    conn,
                    &prefix,
                    account_id,
                    settings.suggest_learned_contacts,
                    limit.clamp(1, 50),
                )
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
            .read(move |conn| {
                let settings = repo::settings::get(conn)?;
                repo::contacts::suggest(
                    conn,
                    &text,
                    None,
                    settings.suggest_learned_contacts,
                    limit.clamp(1, 20),
                )
            })
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
        suggestions_only: bool,
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
                    suggestions_only,
                    cursor.as_ref(),
                    limit.clamp(1, 100),
                )
            })
            .await
    }

    /// Clear local mail-derived people data without touching manual or
    /// CardDAV-backed contacts.
    pub async fn clear_contact_suggestions(&self) -> Result<usize> {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let removed = self
            .db
            .write(move |conn| {
                let tx = conn.transaction()?;
                let removed = repo::contacts::clear_suggestions(&tx, now_ms)?;
                tx.commit()?;
                Ok(removed)
            })
            .await?;
        self.bus.emit(CoreEvent::ContactsUpdated { account_id: 0 });
        Ok(removed)
    }

    pub async fn contact_photo(&self, hash: String) -> Result<Option<Vec<u8>>> {
        self.db
            .read(move |conn| repo::contact_entries::photo_bytes(conn, &hash))
            .await
    }

    pub async fn resolve_contact_conflict(
        &self,
        id: i64,
        revision: i64,
        keep_local: bool,
    ) -> Result<()> {
        self.db
            .write(move |conn| {
                let tx = conn.transaction()?;
                repo::carddav::resolve_conflict(&tx, id, revision, keep_local)?;
                tx.commit()?;
                Ok(())
            })
            .await?;
        self.bus.emit(CoreEvent::ContactsUpdated { account_id: 0 });
        self.nudge_card(None).await;
        Ok(())
    }

    /// Save a contact. A new contact, or a promoted suggestion, created for a
    /// single account is also uploaded to that account's address book.
    pub async fn save_contact(&self, record: ContactRecord) -> Result<ContactRecord> {
        let saved = self
            .db
            .write(move |conn| {
                let tx = conn.transaction()?;
                let id = repo::contact_entries::save(&tx, &record)?;
                if let [account_id] = record.account_ids[..]
                    && record.id <= 0
                {
                    repo::carddav::attach_new_contact(&tx, account_id, id)?;
                }
                let saved = repo::contacts::get_record(&tx, id)?
                    .ok_or_else(|| CoreError::Other("Contact no longer exists".into()))?;
                tx.commit()?;
                Ok(saved)
            })
            .await?;
        self.nudge_card(None).await;
        Ok(saved)
    }

    /// Delete a saved contact as of `revision`, or forget a suggestion.
    pub async fn delete_contact(&self, id: i64, revision: i64) -> Result<()> {
        self.db
            .write(move |conn| {
                let tx = conn.transaction()?;
                repo::contact_entries::delete(&tx, id, revision)?;
                tx.commit()?;
                Ok(())
            })
            .await?;
        self.nudge_card(None).await;
        Ok(())
    }

    /// Connect an RFC 6352 CardDAV service. Discovery is also the connection
    /// test, so neither configuration nor credentials survive a failed probe.
    pub async fn connect_carddav(&self, args: ConnectCardDavArgs) -> Result<Vec<AddressBook>> {
        let account_id = args.account_id;
        if self
            .db
            .read(move |conn| repo::accounts::get(conn, account_id))
            .await?
            .is_none()
        {
            return Err(CoreError::NotFound(format!("account {account_id}")));
        }
        let mut base_url = args.url.trim().to_owned();
        if base_url.is_empty() {
            return Err(CoreError::CardDav("server URL is required".into()));
        }
        if !base_url.contains("://") {
            base_url = format!("https://{base_url}");
        }
        if args.password.is_empty() {
            return Err(CoreError::CardDav("app password is required".into()));
        }
        credentials::check_available_async(self.credentials.clone()).await?;
        let transport = carddav::http::HttpTransport::new(
            args.username.clone(),
            args.password.clone(),
            &base_url,
        )?;
        let discovered = carddav::discovery::discover(&transport, &base_url).await?;

        let previous_password =
            credentials::load_async(self.credentials.clone(), account_id, Slot::CarddavPassword)
                .await
                .ok();
        credentials::store_async(
            self.credentials.clone(),
            account_id,
            Slot::CarddavPassword,
            args.password,
        )
        .await?;
        let config = repo::carddav::CardDavConfig {
            account_id,
            base_url,
            username: args.username,
            principal_url: discovered.principal_url,
            home_set_url: discovered.home_set_url,
            enabled: true,
            last_error: None,
        };
        let books = discovered.addressbooks;
        let persisted = self
            .db
            .write(move |conn| {
                let tx = conn.transaction()?;
                repo::carddav::upsert_config(&tx, &config)?;
                let mut first = None;
                let urls = books
                    .iter()
                    .map(|book| book.url.clone())
                    .collect::<HashSet<_>>();
                for book in books {
                    let id = repo::carddav::upsert_addressbook(
                        &tx,
                        account_id,
                        &book.url,
                        book.display_name.as_deref(),
                        book.read_only,
                    )?;
                    first.get_or_insert(id);
                }
                repo::carddav::retain_addressbooks(&tx, account_id, &urls)?;
                if let Some(id) = first {
                    repo::carddav::ensure_default(&tx, account_id, id)?;
                }
                let result = repo::carddav::list_addressbooks(&tx, Some(account_id))?;
                tx.commit()?;
                Ok(result)
            })
            .await;
        let books = match persisted {
            Ok(books) => books,
            Err(error) => {
                if let Some(password) = previous_password {
                    let _ = credentials::store_async(
                        self.credentials.clone(),
                        account_id,
                        Slot::CarddavPassword,
                        password,
                    )
                    .await;
                } else {
                    let _ = credentials::delete_async(
                        self.credentials.clone(),
                        account_id,
                        Slot::CarddavPassword,
                    )
                    .await;
                }
                return Err(error);
            }
        };
        self.spawn_card_task(account_id).await;
        self.nudge_card(Some(account_id)).await;
        Ok(books)
    }

    pub async fn list_carddav_connections(&self) -> Result<Vec<CardDavConnection>> {
        self.db
            .read(|conn| repo::carddav::list_connections(conn))
            .await
    }

    pub async fn list_addressbooks(&self, account_id: Option<i64>) -> Result<Vec<AddressBook>> {
        self.db
            .read(move |conn| repo::carddav::list_addressbooks(conn, account_id))
            .await
    }

    pub async fn set_account_carddav_enabled(&self, account_id: i64, enabled: bool) -> Result<()> {
        let found = self
            .db
            .write(move |conn| repo::carddav::set_enabled(conn, account_id, enabled))
            .await?;
        if !found {
            return Err(CoreError::NotFound("CardDAV connection".into()));
        }
        if enabled {
            if !self.card_handles.read().await.contains_key(&account_id) {
                self.spawn_card_task(account_id).await;
            }
            self.nudge_card(Some(account_id)).await;
        } else {
            if let Some(handle) = self.card_handles.write().await.remove(&account_id) {
                handle.abort();
            }
        }
        Ok(())
    }

    pub async fn disconnect_carddav(&self, account_id: i64) -> Result<()> {
        if let Some(handle) = self.card_handles.write().await.remove(&account_id) {
            handle.abort();
        }
        self.db
            .write(move |conn| {
                let tx = conn.transaction()?;
                repo::carddav::disconnect(&tx, account_id)?;
                tx.commit()?;
                Ok(())
            })
            .await?;
        let _ =
            credentials::delete_async(self.credentials.clone(), account_id, Slot::CarddavPassword)
                .await;
        self.bus.emit(CoreEvent::ContactsUpdated { account_id });
        Ok(())
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
        // calendar connection: the sync task pushes the event to its calendar
        // (and a second direct POST would duplicate the event in Outlook).
        if account
            .as_ref()
            .is_some_and(|account| account.provider == Provider::Microsoft)
        {
            let has_connection = self
                .calendar_db
                .read(move |conn| repo::caldav::account_has_connections(conn, account_id))
                .await?;
            if !has_connection && let Err(e) = self.push_event_to_graph(account_id, &args).await {
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

    /// Add a CalDAV server to an account, or change one of its CalDAV
    /// connections. The address is a server's, or one calendar's own address
    /// such as a shared mailbox's calendar reached with the account's sign-in.
    /// Events created in its calendars use the account's address as
    /// organizer. Discovery doubles as the connection test, so nothing
    /// persists on failure, and calendars another connection of the account
    /// already syncs stay with it. The app password goes to the keyring; an
    /// empty one keeps the saved password of the connection being changed.
    pub async fn connect_caldav(&self, args: ConnectCaldavArgs) -> Result<CalendarConnection> {
        let account_id = args.account_id;
        self.db
            .read(move |conn| repo::accounts::get(conn, account_id))
            .await?
            .ok_or_else(|| CoreError::NotFound("account".into()))?;
        let existing = match args.connection_id {
            Some(connection_id) => Some(
                self.calendar_db
                    .read(move |conn| repo::caldav::get_connection(conn, connection_id))
                    .await?
                    .filter(|connection| {
                        connection.account_id == account_id
                            && connection.kind == CalendarConnectionKind::Generic
                    })
                    .ok_or_else(|| CoreError::NotFound("calendar connection".into()))?,
            ),
            None => None,
        };
        let url = args.url.trim();
        if url.is_empty() {
            return Err(CoreError::CalDav("server URL is required".into()));
        }
        let url = if url.contains("://") {
            url.to_owned()
        } else {
            format!("https://{url}")
        };
        let username = args.username.trim().to_owned();
        let submitted_password = args.password.filter(|password| !password.is_empty());

        credentials::check_available_async(self.credentials.clone()).await?;
        let password = match (&submitted_password, &existing) {
            (Some(password), _) => password.clone(),
            // A saved password only goes back to the server it was saved for,
            // so a mistyped address cannot receive it.
            (None, Some(connection)) if !same_origin(&connection.base_url, &url) => {
                return Err(CoreError::CalDav(
                    "enter the app password again for a different server".into(),
                ));
            }
            (None, Some(connection)) => {
                caldav::task::connection_password(
                    &self.calendar_db,
                    self.credentials.clone(),
                    connection,
                )
                .await?
            }
            (None, None) => return Err(CoreError::CalDav("password is required".into())),
        };

        let transport =
            caldav::HttpTransport::new(caldav::DavAuth::Basic(username.clone(), password), &url)?;
        // Google answers for one calendar at its own address without always
        // naming it a calendar collection.
        let discovery = if caldav::google_calendar_id(&url).is_some() {
            caldav::discovery::discover_known_collection(&transport, &url).await?
        } else {
            caldav::discovery::discover(&transport, &url).await?
        };

        let editing = existing.as_ref().map(|connection| connection.id);
        let taken: HashSet<String> = self
            .calendar_db
            .read(move |conn| repo::caldav::list_calendars(conn, Some(account_id)))
            .await?
            .into_iter()
            .filter(|calendar| Some(calendar.connection_id) != editing)
            .map(|calendar| caldav::collection_identity(&calendar.url))
            .collect();
        let calendars: Vec<caldav::discovery::DiscoveredCalendar> = discovery
            .calendars
            .into_iter()
            .filter(|calendar| !taken.contains(&caldav::collection_identity(&calendar.url)))
            .collect();
        if calendars.is_empty() {
            return Err(CoreError::CalDav(
                "this account already syncs these calendars".into(),
            ));
        }

        let mut connection = repo::caldav::ConnectionConfig {
            id: editing.unwrap_or_default(),
            account_id,
            kind: CalendarConnectionKind::Generic,
            base_url: url,
            username,
            principal_url: discovery.principal_url,
            home_set_url: Some(discovery.home_set_url),
            enabled: true,
            last_error: None,
        };
        let connection_id = match editing {
            Some(connection_id) => {
                if let Some(password) = submitted_password {
                    caldav::task::save_password(
                        &self.calendar_db,
                        self.credentials.clone(),
                        connection_id,
                        account_id,
                        password,
                    )
                    .await?;
                }
                self.calendar_db
                    .write(move |conn| {
                        let tx = conn.transaction()?;
                        repo::caldav::update_connection(&tx, &connection)?;
                        store_discovered_calendars(&tx, &connection, &calendars)?;
                        tx.commit()?;
                        Ok(())
                    })
                    .await?;
                connection_id
            }
            None => {
                // A new connection waits, paused, until its password is saved.
                connection.enabled = false;
                let connection_id = self
                    .calendar_db
                    .write(move |conn| {
                        let tx = conn.transaction()?;
                        connection.id = repo::caldav::insert_connection(&tx, &connection)?;
                        store_discovered_calendars(&tx, &connection, &calendars)?;
                        tx.commit()?;
                        Ok(connection.id)
                    })
                    .await?;
                let saved = caldav::task::save_password(
                    &self.calendar_db,
                    self.credentials.clone(),
                    connection_id,
                    account_id,
                    submitted_password.unwrap_or_default(),
                )
                .await;
                if let Err(error) = saved {
                    self.calendar_db
                        .write(move |conn| {
                            let tx = conn.transaction()?;
                            repo::caldav::remove_connection(&tx, connection_id)?;
                            repo::caldav::ensure_default_calendar(&tx, account_id, None)?;
                            tx.commit()?;
                            Ok(())
                        })
                        .await?;
                    return Err(error);
                }
                self.calendar_db
                    .write(move |conn| {
                        repo::caldav::set_connection_enabled(conn, connection_id, true).map(|_| ())
                    })
                    .await?;
                connection_id
            }
        };

        self.refresh_cal_task(account_id).await;
        self.nudge_cal(Some(account_id)).await;
        self.bus.emit(CoreEvent::CalendarUpdated { account_id });
        self.calendar_connection(connection_id).await
    }

    async fn calendar_connection(&self, connection_id: i64) -> Result<CalendarConnection> {
        self.calendar_db
            .read(|conn| repo::caldav::list_connections(conn))
            .await?
            .into_iter()
            .find(|connection| connection.id == connection_id)
            .ok_or_else(|| CoreError::NotFound("calendar connection".into()))
    }

    /// Sync the account's Google calendars over CalDAV with its own OAuth
    /// tokens; the caller must have completed the calendar-scope consent.
    /// The primary collection is checked first so a disabled CalDAV API is
    /// reported immediately; Calendar API metadata then supplies every
    /// secondary, shared and special calendar id, since CalDAV itself only
    /// knows the one id embedded in its collection URL.
    async fn attach_google_calendar(&self, account_id: i64) -> Result<Vec<Calendar>> {
        let email = self
            .db
            .read(move |conn| Ok(repo::accounts::get(conn, account_id)?.map(|a| a.email)))
            .await?
            .ok_or_else(|| CoreError::NotFound("account".into()))?;
        let base_url = caldav::google_calendar_url(&email)?;
        let token = self
            .tokens
            .access_token(account_id, Provider::Gmail)
            .await?;
        let transport =
            caldav::HttpTransport::new(caldav::DavAuth::Bearer(token.clone()), &base_url)?;
        caldav::discovery::discover_known_collection(&transport, &base_url).await?;
        let listed = googlecal::list_calendars(&token).await?;
        if listed.is_empty() {
            return Err(CoreError::CalDav("no Google calendars found".into()));
        }

        let out = self
            .calendar_db
            .write(move |conn| {
                let tx = conn.transaction()?;
                let mut connection = match repo::caldav::provider_connection(
                    &tx,
                    account_id,
                    CalendarConnectionKind::Google,
                )? {
                    Some(connection) => connection,
                    None => {
                        let mut connection = repo::caldav::ConnectionConfig {
                            id: 0,
                            account_id,
                            kind: CalendarConnectionKind::Google,
                            base_url: base_url.clone(),
                            username: String::new(),
                            principal_url: None,
                            home_set_url: None,
                            enabled: true,
                            last_error: None,
                        };
                        connection.id = repo::caldav::insert_connection(&tx, &connection)?;
                        connection
                    }
                };
                connection.home_set_url = Some(base_url.clone());
                connection.base_url = base_url;
                connection.enabled = true;
                repo::caldav::update_connection(&tx, &connection)?;
                let calendars = googlecal::reconcile_calendars(&tx, &connection, &listed)?;
                tx.commit()?;
                Ok(calendars)
            })
            .await?;

        self.refresh_cal_task(account_id).await;
        self.nudge_cal(Some(account_id)).await;
        self.bus.emit(CoreEvent::CalendarUpdated { account_id });
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

        credentials::check_available_async(self.credentials.clone()).await?;

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
        self.store_sign_in(&account, outcome).await?;

        self.attach_google_calendar(account_id).await
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
                credentials::check_available_async(self.credentials.clone()).await?;
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
                self.store_sign_in(&account, outcome).await?;
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
    /// persists them under the account's Microsoft connection; the account's
    /// calendar task then pulls via calendarView delta.
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

        let out = self
            .calendar_db
            .write(move |conn| {
                let tx = conn.transaction()?;
                let mut connection = match repo::caldav::provider_connection(
                    &tx,
                    account_id,
                    CalendarConnectionKind::Microsoft,
                )? {
                    Some(connection) => connection,
                    None => {
                        let mut connection = repo::caldav::ConnectionConfig {
                            id: 0,
                            account_id,
                            kind: CalendarConnectionKind::Microsoft,
                            base_url: graph::GRAPH_BASE.to_string(),
                            username: String::new(),
                            principal_url: None,
                            home_set_url: None,
                            enabled: true,
                            last_error: None,
                        };
                        connection.id = repo::caldav::insert_connection(&tx, &connection)?;
                        connection
                    }
                };
                connection.enabled = true;
                repo::caldav::update_connection(&tx, &connection)?;
                let mut listed = HashSet::new();
                let mut default_id = None;
                for c in &discovered {
                    let id = repo::caldav::upsert_calendar(
                        &tx,
                        &connection,
                        &c.id,
                        c.name.as_deref(),
                        c.hex_color.as_deref(),
                        !c.can_edit,
                    )?;
                    if c.is_default {
                        default_id.get_or_insert(id);
                    }
                    listed.insert(c.id.clone());
                }
                repo::caldav::retain_calendars(&tx, connection.id, &listed)?;
                // Keep an existing default if one is set; else Outlook's
                // default calendar (or the first writable one) wins.
                repo::caldav::ensure_default_calendar(&tx, account_id, default_id)?;
                let list = repo::caldav::connection_calendars(&tx, connection.id)?;
                tx.commit()?;
                Ok(list)
            })
            .await?;

        self.refresh_cal_task(account_id).await;
        self.nudge_cal(Some(account_id)).await;
        self.bus.emit(CoreEvent::CalendarUpdated { account_id });
        Ok(out)
    }

    /// Stop syncing a calendar connection and forget its calendars here (see
    /// `repo::caldav::forget_calendar` for the events that stay). Nothing
    /// changes on the server. The password goes first: a connection that
    /// outlives it only asks for a new one, while a password outliving its
    /// connection could never be found again.
    pub async fn remove_calendar_connection(&self, connection_id: i64) -> Result<()> {
        let connection = self
            .calendar_db
            .read(move |conn| repo::caldav::get_connection(conn, connection_id))
            .await?
            .ok_or_else(|| CoreError::NotFound("calendar connection".into()))?;
        let account_id = connection.account_id;
        self.delete_connection_password(&connection).await?;
        self.calendar_db
            .write(move |conn| {
                let tx = conn.transaction()?;
                repo::caldav::remove_connection(&tx, connection_id)?;
                repo::caldav::ensure_default_calendar(&tx, account_id, None)?;
                tx.commit()?;
                Ok(())
            })
            .await?;
        self.refresh_cal_task(account_id).await;
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
            .read(|conn| repo::caldav::list_connections(conn))
            .await
    }

    /// Pause/resume one calendar connection without deleting local events,
    /// discovered collections, sync tokens, or credentials.
    pub async fn set_calendar_connection_enabled(
        &self,
        connection_id: i64,
        enabled: bool,
    ) -> Result<()> {
        let account_id = self
            .calendar_db
            .write(move |conn| {
                let connection = repo::caldav::get_connection(conn, connection_id)?
                    .ok_or_else(|| CoreError::NotFound("calendar connection".into()))?;
                repo::caldav::set_connection_enabled(conn, connection_id, enabled)?;
                Ok(connection.account_id)
            })
            .await?;
        self.refresh_cal_task(account_id).await;
        if enabled {
            self.nudge_cal(Some(account_id)).await;
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

    /// Queue a calendar write for the account's calendar task, when the
    /// account has a calendar connection. No-op otherwise (purely local
    /// calendars).
    async fn enqueue_cal_push(&self, event_id: i64, account_id: i64, kind: &str) -> Result<()> {
        let kind = kind.to_string();
        self.calendar_db
            .write(move |conn| {
                if !repo::caldav::account_has_connections(conn, account_id)? {
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
                from: None,
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
        Ok(self
            .search_chronological_page(query, account_id, None, limit)
            .await?
            .threads)
    }

    /// Cursor-paginated chronological search for the native mail list.
    pub async fn search_chronological_page(
        &self,
        query: String,
        account_id: Option<i64>,
        cursor: Option<models::SearchCursor>,
        limit: i64,
    ) -> Result<models::SearchPage> {
        let mut parsed = search::parse(&query);
        parsed.account_id = account_id;
        self.search_chronological_parsed(parsed, cursor, limit)
            .await
    }

    /// `search_chronological_page` across a set of accounts, such as a mail
    /// profile's. An empty set matches nothing.
    pub async fn search_chronological_page_for_accounts(
        &self,
        query: String,
        account_ids: Vec<i64>,
        cursor: Option<models::SearchCursor>,
        limit: i64,
    ) -> Result<models::SearchPage> {
        let mut parsed = search::parse(&query);
        parsed.account_ids = Some(account_ids);
        self.search_chronological_parsed(parsed, cursor, limit)
            .await
    }

    async fn search_chronological_parsed(
        &self,
        parsed: search::ParsedQuery,
        cursor: Option<models::SearchCursor>,
        limit: i64,
    ) -> Result<models::SearchPage> {
        let limit = limit.clamp(1, 100);
        self.db
            .read(move |conn| repo::search::chronological_page(conn, &parsed, cursor, limit))
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
        owner_account_id: Option<i64>,
    ) -> Result<Label> {
        let (label, accounts) = self
            .db
            .write(move |conn| {
                let tx = conn.transaction()?;
                if let Some(account_id) = owner_account_id
                    && repo::accounts::get(&tx, account_id)?.is_none()
                {
                    return Err(CoreError::NotFound(format!("account {account_id}")));
                }
                let previous = id
                    .map(|label_id| repo::labels::get(&tx, label_id))
                    .transpose()?
                    .flatten();
                let cascade_descendants = previous
                    .as_ref()
                    .and_then(|label| label.owner_account_id)
                    .map(|account_id| repo::accounts::get(&tx, account_id))
                    .transpose()?
                    .flatten()
                    .is_some_and(|account| account.provider == Provider::Gmail);
                let (label, mut changed_labels) = repo::labels::save_with_descendants(
                    &tx,
                    id,
                    &name,
                    &color,
                    position,
                    owner_account_id,
                    cascade_descendants,
                )?;
                // Descendants are already deepest-first; update them before
                // their root to avoid temporary Gmail name collisions.
                changed_labels.push(label.clone());
                let mut accounts = Vec::new();
                if id.is_none()
                    && let Some(account_id) = label.owner_account_id
                    && repo::accounts::get(&tx, account_id)?
                        .is_some_and(|account| account.provider == Provider::Gmail)
                {
                    repo::actions::enqueue(
                        &tx,
                        account_id,
                        "gmail_label_create",
                        None,
                        None,
                        &serde_json::json!({ "labelId": label.id }),
                        None,
                    )?;
                    accounts.push(account_id);
                }
                for changed in changed_labels {
                    let mappings = {
                        let mut stmt = tx.prepare(
                            "SELECT account_id, provider_id FROM gmail_labels
                             WHERE local_label_id = ?1",
                        )?;
                        stmt.query_map(rusqlite::params![changed.id], |row| {
                            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                        })?
                        .collect::<rusqlite::Result<Vec<_>>>()?
                    };
                    for (account_id, provider_id) in mappings {
                        repo::actions::enqueue(
                            &tx,
                            account_id,
                            "gmail_label_update",
                            None,
                            None,
                            &serde_json::json!({
                                "providerId": provider_id,
                                "name": changed.name,
                                "color": changed.color,
                            }),
                            None,
                        )?;
                        accounts.push(account_id);
                    }
                }
                accounts.sort_unstable();
                accounts.dedup();
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
                let root = repo::labels::get(&tx, id)?;
                let delete_subtree = root
                    .as_ref()
                    .and_then(|label| label.owner_account_id)
                    .map(|account_id| repo::accounts::get(&tx, account_id))
                    .transpose()?
                    .flatten()
                    .is_some_and(|account| account.provider == Provider::Gmail);
                let labels = match root {
                    Some(root) if delete_subtree => repo::labels::subtree(&tx, &root)?,
                    Some(root) => vec![root],
                    None => Vec::new(),
                };
                let mut accounts = Vec::new();
                for label in &labels {
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
                    for (account_id, provider_id) in mappings {
                        repo::actions::enqueue(
                            &tx,
                            account_id,
                            "gmail_label_delete",
                            None,
                            None,
                            &serde_json::json!({ "providerId": provider_id }),
                            None,
                        )?;
                        accounts.push(account_id);
                    }
                    if delete_subtree && let Some(account_id) = label.owner_account_id {
                        // If creation has not reached Gmail yet, cancelling it
                        // is both faster and avoids manufacturing a remote
                        // label only to delete it immediately.
                        tx.execute(
                            "UPDATE pending_actions
                             SET state='cancelled', finished_at=?3
                             WHERE account_id=?1 AND kind='gmail_label_create'
                               AND state='pending'
                               AND json_extract(payload, '$.labelId')=?2",
                            rusqlite::params![account_id, label.id, now_ms()],
                        )?;
                        accounts.push(account_id);
                    }
                }
                for label in labels {
                    repo::labels::delete(&tx, label.id)?;
                }
                accounts.sort_unstable();
                accounts.dedup();
                tx.commit()?;
                Ok(accounts)
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
        let oauth_settings = settings.clone();
        let now_ms = chrono::Utc::now().timestamp_millis();
        self.db
            .write(move |conn| {
                let tx = conn.transaction()?;
                let previous = repo::settings::get(&tx)?;
                // Allow unrelated settings to be saved even if an older build
                // persisted invalid OAuth keys; validate any newly supplied ID.
                if settings.google_client_id != previous.google_client_id {
                    oauth::providers::validate_custom_client_id(
                        Provider::Gmail,
                        &settings.google_client_id,
                    )?;
                }
                if settings.ms_client_id != previous.ms_client_id {
                    oauth::providers::validate_custom_client_id(
                        Provider::Microsoft,
                        &settings.ms_client_id,
                    )?;
                }
                repo::contacts::advance_learning_boundaries(
                    &tx,
                    !previous.collect_outgoing_contacts && settings.collect_outgoing_contacts,
                    !previous.collect_incoming_contacts && settings.collect_incoming_contacts,
                    now_ms,
                )?;
                repo::settings::set(&tx, &settings)?;
                tx.commit()?;
                Ok(())
            })
            .await?;
        // Keep the resolver in the same order as persisted writes.
        apply_oauth_settings(&oauth_settings);
        Ok(())
    }

    /// Create or update a local mail profile without touching provider state.
    /// The settings read and write share the database writer so an unrelated
    /// preference cannot be lost to a stale read/modify/write cycle.
    pub async fn save_mail_profile(
        &self,
        profile_id: Option<String>,
        name: String,
        color: String,
    ) -> Result<(Settings, MailProfile)> {
        let name = normalize_mail_profile_name(&name)?;
        let color = normalize_mail_profile_color(&color)?;
        self.db
            .write(move |conn| {
                let mut settings = repo::settings::get(conn)?;
                let requested_id = profile_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|id| !id.is_empty());
                let duplicate = settings.mail_profiles.iter().any(|profile| {
                    Some(profile.id.as_str()) != requested_id
                        && profile.name.eq_ignore_ascii_case(&name)
                });
                if duplicate {
                    return Err(CoreError::Other(format!(
                        "a profile named {name:?} already exists"
                    )));
                }

                let profile = if let Some(id) = requested_id {
                    let profile = settings
                        .mail_profiles
                        .iter_mut()
                        .find(|profile| profile.id == id)
                        .ok_or_else(|| CoreError::NotFound(format!("mail profile {id}")))?;
                    profile.name = name;
                    profile.color = color;
                    profile.clone()
                } else {
                    if settings.mail_profiles.len() >= 50 {
                        return Err(CoreError::Other(
                            "no more than 50 mail profiles can be created".into(),
                        ));
                    }
                    let base_id = format!(
                        "profile-{}",
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_micros()
                    );
                    let mut id = base_id.clone();
                    let mut suffix = 2u32;
                    while settings
                        .mail_profiles
                        .iter()
                        .any(|profile| profile.id == id)
                    {
                        id = format!("{base_id}-{suffix}");
                        suffix += 1;
                    }
                    let profile = MailProfile {
                        id,
                        name,
                        color,
                        account_ids: Vec::new(),
                    };
                    settings.mail_profiles.push(profile.clone());
                    profile
                };
                repo::settings::set(conn, &settings)?;
                Ok((settings, profile))
            })
            .await
    }

    /// Assign an account to one profile, or remove its profile assignment.
    pub async fn assign_account_profile(
        &self,
        account_id: i64,
        profile_id: Option<String>,
    ) -> Result<Settings> {
        self.db
            .write(move |conn| {
                if repo::accounts::get(conn, account_id)?.is_none() {
                    return Err(CoreError::NotFound(format!("account {account_id}")));
                }
                let mut settings = repo::settings::get(conn)?;
                let profile_id = profile_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|id| !id.is_empty());
                if let Some(id) = profile_id
                    && !settings
                        .mail_profiles
                        .iter()
                        .any(|profile| profile.id == id)
                {
                    return Err(CoreError::NotFound(format!("mail profile {id}")));
                }
                for profile in &mut settings.mail_profiles {
                    profile.account_ids.retain(|id| *id != account_id);
                }
                if let Some(id) = profile_id {
                    let profile = settings
                        .mail_profiles
                        .iter_mut()
                        .find(|profile| profile.id == id)
                        .expect("profile existence checked above");
                    profile.account_ids.push(account_id);
                    profile.account_ids.sort_unstable();
                }
                retire_active_mail_profile(&mut settings);
                repo::settings::set(conn, &settings)?;
                Ok(settings)
            })
            .await
    }

    /// Set an account-specific marker color, or restore its generated color.
    /// Choosing an individual color also leaves any profile in the same
    /// settings write so the visible color has one unambiguous owner.
    pub async fn set_account_color(
        &self,
        account_id: i64,
        color: Option<String>,
    ) -> Result<Settings> {
        let color = color
            .map(|color| normalize_mail_profile_color(&color))
            .transpose()?;
        self.db
            .write(move |conn| {
                if repo::accounts::get(conn, account_id)?.is_none() {
                    return Err(CoreError::NotFound(format!("account {account_id}")));
                }
                let mut settings = repo::settings::get(conn)?;
                for profile in &mut settings.mail_profiles {
                    profile.account_ids.retain(|id| *id != account_id);
                }
                retire_active_mail_profile(&mut settings);
                let key = account_id.to_string();
                if let Some(color) = color {
                    settings.account_colors.insert(key, color);
                } else {
                    settings.account_colors.remove(&key);
                }
                repo::settings::set(conn, &settings)?;
                Ok(settings)
            })
            .await
    }

    /// Delete one local profile. Connected accounts and provider data remain.
    pub async fn delete_mail_profile(&self, profile_id: String) -> Result<Settings> {
        let profile_id = profile_id.trim().to_owned();
        self.db
            .write(move |conn| {
                let mut settings = repo::settings::get(conn)?;
                let before = settings.mail_profiles.len();
                settings
                    .mail_profiles
                    .retain(|profile| profile.id != profile_id);
                if settings.mail_profiles.len() == before {
                    return Err(CoreError::NotFound(format!("mail profile {profile_id}")));
                }
                retire_active_mail_profile(&mut settings);
                repo::settings::set(conn, &settings)?;
                Ok(settings)
            })
            .await
    }

    /// Show only one profile's accounts in the mail view, or every account
    /// with `None`. The profile must have at least one account.
    pub async fn set_active_mail_profile(&self, profile_id: Option<String>) -> Result<Settings> {
        let profile_id = profile_id
            .map(|id| id.trim().to_owned())
            .filter(|id| !id.is_empty());
        self.db
            .write(move |conn| {
                let mut settings = repo::settings::get(conn)?;
                if let Some(id) = &profile_id {
                    let profile = settings
                        .mail_profiles
                        .iter()
                        .find(|profile| &profile.id == id)
                        .ok_or_else(|| CoreError::NotFound(format!("mail profile {id}")))?;
                    if profile.account_ids.is_empty() {
                        return Err(CoreError::Other(format!(
                            "mail profile {:?} has no accounts",
                            profile.name
                        )));
                    }
                }
                settings.active_mail_profile = profile_id;
                repo::settings::set(conn, &settings)?;
                Ok(settings)
            })
            .await
    }

    pub async fn set_show_account_badges(&self, enabled: bool) -> Result<Settings> {
        self.db
            .write(move |conn| {
                let mut settings = repo::settings::get(conn)?;
                settings.show_account_badges = enabled;
                repo::settings::set(conn, &settings)?;
                Ok(settings)
            })
            .await
    }

    pub async fn set_group_mail_by_date(&self, enabled: bool) -> Result<()> {
        self.db
            .write(move |conn| {
                let mut settings = repo::settings::get(conn)?;
                settings.group_mail_by_date = enabled;
                repo::settings::set(conn, &settings)
            })
            .await
    }

    pub async fn set_time_format(&self, format: TimeFormat) -> Result<()> {
        self.db
            .write(move |conn| {
                let mut settings = repo::settings::get(conn)?;
                settings.time_format = format;
                repo::settings::set(conn, &settings)
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
        // Reject a bad registration before either provider is written. Empty
        // IDs remain the explicit way to clear a custom registration.
        if let Some((id, _)) = &google {
            oauth::providers::validate_custom_client_id(Provider::Gmail, id)?;
        }
        if let Some(id) = &microsoft {
            oauth::providers::validate_custom_client_id(Provider::Microsoft, id)?;
        }
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
        for handle in self.card_handles.read().await.values() {
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

/// Whether two addresses share scheme, host and port.
fn same_origin(left: &str, right: &str) -> bool {
    match (url::Url::parse(left), url::Url::parse(right)) {
        (Ok(left), Ok(right)) => left.origin() == right.origin(),
        _ => false,
    }
}

/// Store what a CalDAV discovery found for `connection`: its collections
/// are exactly these now, and the account keeps or gains a default calendar.
fn store_discovered_calendars(
    conn: &rusqlite::Connection,
    connection: &repo::caldav::ConnectionConfig,
    calendars: &[caldav::discovery::DiscoveredCalendar],
) -> Result<()> {
    let mut urls = HashSet::new();
    let mut first = None;
    for calendar in calendars {
        let id = repo::caldav::upsert_calendar(
            conn,
            connection,
            &calendar.url,
            calendar.display_name.as_deref(),
            calendar.color.as_deref(),
            false,
        )?;
        first.get_or_insert(id);
        urls.insert(calendar.url.clone());
    }
    repo::caldav::retain_calendars(conn, connection.id, &urls)?;
    repo::caldav::ensure_default_calendar(conn, connection.account_id, first)
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

    let (account_id, mail_protocol): (i64, MailProtocol) = tx.query_row(
        "SELECT t.account_id, a.mail_protocol
         FROM threads t JOIN accounts a ON a.id = t.account_id
         WHERE t.id = ?1",
        rusqlite::params![thread_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let multi_mailbox = mail_protocol.has_multi_mailbox_messages();

    // Most thread actions intentionally ignore drafts. Archive and Trash are
    // exceptions: users can file or discard a draft from the Drafts view, and
    // excluding it here made the optimistic removal snap back on reconciliation.
    let include_drafts = matches!(
        kind,
        ActionKind::Archive | ActionKind::Trash | ActionKind::DeletePermanently
    );
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
        ActionKind::DeletePermanently => {
            for (id, f, u, _r, _s, role) in &msgs {
                let in_trash = role == roles::TRASH
                    || tx.query_row(
                        "SELECT EXISTS(SELECT 1 FROM message_folders mf
                     JOIN folders folder ON folder.id = mf.folder_id
                     WHERE mf.message_id = ?1 AND folder.role = 'trash')",
                        rusqlite::params![id],
                        |row| row.get::<_, bool>(0),
                    )?;
                if !in_trash {
                    continue;
                }
                let already_queued: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM pending_actions
                     WHERE message_id = ?1 AND kind = 'delete_permanently'
                       AND state IN ('pending','inflight'))",
                    rusqlite::params![id],
                    |row| row.get(0),
                )?;
                if already_queued {
                    continue;
                }
                let aid = repo::actions::enqueue(
                    &tx,
                    account_id,
                    kind.as_str(),
                    Some(*id),
                    Some(thread_id),
                    &serde_json::json!({"folderId": f, "uid": u}),
                    None,
                )?;
                out.push((aid, account_id));
            }
        }
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
            let target_folder = repo::folders::get(&tx, target)?
                .ok_or_else(|| CoreError::NotFound(format!("folder {target}")))?;
            if target_folder.account_id != account_id {
                return Err(CoreError::Other(
                    "messages cannot be moved between accounts".into(),
                ));
            }
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
            if let Some(owner_account_id) = label.owner_account_id
                && owner_account_id != account_id
            {
                return Err(CoreError::Other(format!(
                    "label '{}' belongs to another account",
                    label.name
                )));
            }
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
                if label.is_auto || label.owner_account_id.is_none() {
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

#[cfg(test)]
mod move_action_validation_tests {
    use super::*;

    #[tokio::test]
    async fn empty_trash_does_not_remove_mail_added_during_provider_purge() {
        let tmp = tempfile::tempdir().unwrap();
        let core = Core::start(Paths::for_tests(tmp.path())).await.unwrap();
        let old_id = core
            .db
            .write(|conn| {
                db::testutil::seed_account(conn);
                conn.execute(
                    "INSERT INTO folders (id, account_id, path, role)
                     VALUES (2, 1, 'Trash', 'trash')",
                    [],
                )?;
                let (_, id) = db::testutil::seed_message(conn, "sender@test.dev", "old", false);
                conn.execute(
                    "UPDATE messages SET folder_id = 2 WHERE id = ?1",
                    rusqlite::params![id],
                )?;
                Ok(id)
            })
            .await
            .unwrap();
        let ctx = core.sync_ctx();
        let snapshot = sync::snapshot_trash_ids(&ctx, 1).await.unwrap();
        let new_id = core
            .db
            .write(|conn| {
                let (_, id) = db::testutil::seed_message(conn, "sender@test.dev", "new", false);
                conn.execute(
                    "UPDATE messages SET folder_id = 2 WHERE id = ?1",
                    rusqlite::params![id],
                )?;
                Ok(id)
            })
            .await
            .unwrap();
        sync::finish_empty_trash(&ctx, 1, snapshot).await.unwrap();
        let (old, new) = core
            .db
            .read(move |conn| {
                Ok((
                    repo::messages::get_row(conn, old_id)?,
                    repo::messages::get_row(conn, new_id)?,
                ))
            })
            .await
            .unwrap();
        assert!(old.is_none());
        assert!(new.is_some());
    }

    #[tokio::test]
    async fn local_draft_purge_cleans_attachment_but_remote_move_waits_for_trash_uid() {
        let tmp = tempfile::tempdir().unwrap();
        let core = Core::start(Paths::for_tests(tmp.path())).await.unwrap();
        let staged_path = core
            .paths
            .draft_attachments_dir()
            .join("test")
            .join("file.txt");
        std::fs::create_dir_all(staged_path.parent().unwrap()).unwrap();
        std::fs::write(&staged_path, b"attachment").unwrap();
        let staged_path_string = staged_path.to_string_lossy().into_owned();
        let draft_id = core
            .db
            .write(move |conn| {
                db::testutil::seed_account(conn);
                conn.execute(
                    "INSERT INTO folders (id, account_id, path, role)
                     VALUES (2, 1, 'Trash', 'trash')",
                    [],
                )?;
                let (_, draft_id) =
                    db::testutil::seed_message(conn, "sender@test.dev", "draft", false);
                conn.execute(
                    "UPDATE messages SET is_draft = 1, uid = NULL, folder_id = 2 WHERE id = ?1",
                    rusqlite::params![draft_id],
                )?;
                conn.execute(
                    "INSERT INTO drafts_meta (message_id) VALUES (?1)",
                    rusqlite::params![draft_id],
                )?;
                conn.execute(
                    "INSERT INTO draft_attachments (draft_id, file_path, filename)
                     VALUES (?1, ?2, 'file.txt')",
                    rusqlite::params![draft_id, staged_path_string],
                )?;
                Ok(draft_id)
            })
            .await
            .unwrap();
        let ctx = core.sync_ctx();
        assert!(sync::is_local_only_draft(&ctx, draft_id).await.unwrap());
        let unlinked_remote_draft = core
            .db
            .write(|conn| {
                let (_, id) = db::testutil::seed_message(conn, "sender@test.dev", "remote", false);
                conn.execute(
                    "UPDATE messages SET is_draft = 1, uid = NULL, folder_id = 2 WHERE id = ?1",
                    rusqlite::params![id],
                )?;
                Ok(id)
            })
            .await
            .unwrap();
        assert!(
            !sync::is_local_only_draft(&ctx, unlinked_remote_draft)
                .await
                .unwrap()
        );
        core.db
            .write(move |conn| {
                repo::actions::enqueue(
                    conn,
                    1,
                    "trash",
                    Some(draft_id),
                    None,
                    &serde_json::json!({"srcUid": 42}),
                    None,
                )?;
                Ok(())
            })
            .await
            .unwrap();
        assert!(!sync::is_local_only_draft(&ctx, draft_id).await.unwrap());
        sync::finish_permanent_delete(&ctx, Some(draft_id))
            .await
            .unwrap();
        assert!(!staged_path.exists());
        assert!(
            core.db
                .read(move |conn| repo::messages::get_row(conn, draft_id))
                .await
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn permanent_delete_queues_only_trash_messages_and_keeps_them_until_confirmed() {
        let mut conn = db::testutil::conn();
        db::testutil::seed_account(&conn);
        conn.execute(
            "INSERT INTO folders (id, account_id, path, role)
                      VALUES (2, 1, 'Trash', 'trash')",
            [],
        )
        .unwrap();
        let (thread_id, inbox_id) =
            db::testutil::seed_message(&conn, "sender@test.dev", "conversation", false);
        let (_, trash_id) =
            db::testutil::seed_message(&conn, "sender@test.dev", "old reply", false);
        conn.execute(
            "UPDATE messages SET thread_id = ?1, folder_id = 2, uid = 42 WHERE id = ?2",
            rusqlite::params![thread_id, trash_id],
        )
        .unwrap();

        let actions =
            apply_thread_action(&mut conn, thread_id, ActionKind::DeletePermanently, None).unwrap();
        assert_eq!(actions.len(), 1);
        let action = repo::actions::get(&conn, actions[0].0).unwrap().unwrap();
        assert_eq!(action.message_id, Some(trash_id));
        assert_eq!(action.kind, "delete_permanently");
        assert!(repo::messages::get_row(&conn, trash_id).unwrap().is_some());
        assert!(repo::messages::get_row(&conn, inbox_id).unwrap().is_some());
        assert!(
            apply_thread_action(&mut conn, thread_id, ActionKind::DeletePermanently, None)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn move_rejects_a_folder_owned_by_another_account_without_local_mutation() {
        let mut conn = db::testutil::conn();
        db::testutil::seed_account(&conn);
        conn.execute(
            "INSERT INTO accounts (id, email, provider, auth_kind, username,
             incoming_host, incoming_port, smtp_host, smtp_port, created_at, mail_protocol, settings_json)
             VALUES (2,'other@test.dev','generic','password','other','h',993,'h',587,0,'imap','{\"connection\":{\"incomingSecurity\":\"tls\",\"smtpSecurity\":\"tls\"}}')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO folders (id, account_id, path, role)
             VALUES (2, 2, 'Archive', 'archive')",
            [],
        )
        .unwrap();
        let (thread_id, message_id) =
            db::testutil::seed_message(&conn, "sender@test.dev", "cross account", false);
        let before = repo::messages::get_row(&conn, message_id).unwrap().unwrap();

        let error = apply_thread_action(
            &mut conn,
            thread_id,
            ActionKind::Move,
            Some(&ActionParams {
                wake_at: None,
                target_folder_id: Some(2),
                label_id: None,
            }),
        )
        .unwrap_err();

        assert!(error.to_string().contains("between accounts"));
        let after = repo::messages::get_row(&conn, message_id).unwrap().unwrap();
        assert_eq!(after.folder_id, before.folder_id);
        assert_eq!(after.uid, before.uid);
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM pending_actions", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn label_action_rejects_a_label_owned_by_another_account() {
        let mut conn = db::testutil::conn();
        db::testutil::seed_account(&conn);
        conn.execute(
            "INSERT INTO accounts (id, email, provider, auth_kind, username,
             incoming_host, incoming_port, smtp_host, smtp_port, created_at, mail_protocol, settings_json)
             VALUES (2,'other@test.dev','generic','password','other','h',993,'h',587,0,'imap','{\"connection\":{\"incomingSecurity\":\"tls\",\"smtpSecurity\":\"tls\"}}')",
            [],
        )
        .unwrap();
        let (thread_id, message_id) =
            db::testutil::seed_message(&conn, "sender@test.dev", "cross account label", false);
        let label = repo::labels::save(&conn, None, "Travel", "#123456", 0, Some(2)).unwrap();

        let error = apply_thread_action(
            &mut conn,
            thread_id,
            ActionKind::AddLabel,
            Some(&ActionParams {
                wake_at: None,
                target_folder_id: None,
                label_id: Some(label.id),
            }),
        )
        .unwrap_err();

        assert!(error.to_string().contains("another account"));
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM message_labels WHERE message_id = ?1",
                [message_id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
            0
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM pending_actions", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn global_label_action_stays_local() {
        let mut conn = db::testutil::conn();
        db::testutil::seed_account(&conn);
        let (thread_id, message_id) =
            db::testutil::seed_message(&conn, "sender@test.dev", "global label", false);
        let label = repo::labels::save(&conn, None, "Follow up", "#123456", 0, None).unwrap();

        let actions = apply_thread_action(
            &mut conn,
            thread_id,
            ActionKind::AddLabel,
            Some(&ActionParams {
                wake_at: None,
                target_folder_id: None,
                label_id: Some(label.id),
            }),
        )
        .unwrap();

        assert!(actions.is_empty());
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM message_labels
                  WHERE message_id = ?1 AND label_id = ?2",
                rusqlite::params![message_id, label.id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
            1
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM pending_actions", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
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
                    let protocol: MailProtocol = tx.query_row(
                        "SELECT a.mail_protocol
                         FROM messages m JOIN accounts a ON a.id = m.account_id
                         WHERE m.id = ?1",
                        rusqlite::params![mid],
                        |row| row.get(0),
                    )?;
                    if was_pending {
                        // Remote never changed; restore the original mapping.
                        repo::messages::set_uid_and_folder(&tx, mid, src, src_uid)?;
                    } else {
                        // Remote moved; move it back. The message's UID there
                        // is known once that mailbox has synced, and the move
                        // looks it up otherwise.
                        let current_uid = repo::messages::get_row(&tx, mid)?
                            .filter(|row| row.folder_id == cur_folder)
                            .and_then(|row| row.uid);
                        repo::messages::set_uid_and_folder(&tx, mid, src, None)?;
                        let payload = serde_json::json!({
                            "srcFolderId": cur_folder,
                            "srcUid": current_uid,
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
                    if protocol.has_multi_mailbox_messages() {
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
mod startup_connection_tests {
    use super::*;

    #[tokio::test]
    async fn startup_releases_recovery_connections_before_foreground_reads() {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::for_tests(temp.path());
        let db = Db::open(&paths.db_file()).unwrap();
        db.write(|conn| {
            conn.execute(
                "INSERT INTO route_cache(sender_domain, route_key) VALUES('__routing_backfill__', '1')",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
        db.release_idle_connections().await.unwrap();
        drop(db);
        let core = Core::start_mail_ui(paths).await.unwrap();

        assert!(
            core.db
                .cache_usage()
                .await
                .unwrap()
                .iter()
                .all(|connection| connection.role != "writer")
        );
        core.get_settings().await.unwrap();
        let usage = core.db.cache_usage().await.unwrap();
        assert_eq!(usage.len(), 1);
        assert_eq!(usage[0].role, "reader");
    }
}

#[cfg(test)]
mod draft_staging_tests {
    use super::*;

    #[tokio::test]
    async fn cleanup_scans_staged_attachments_and_keeps_live_draft_references() {
        let temp = tempfile::tempdir().unwrap();
        let core = Core::start_mail_ui(Paths::for_tests(temp.path()))
            .await
            .unwrap();
        let (sent_id, draft_id) = core
            .db
            .write(|conn| {
                db::testutil::seed_account(conn);
                let (_, sent_id) = db::testutil::seed_message(conn, "me@test.dev", "sent", false);
                let (_, draft_id) = db::testutil::seed_message(conn, "me@test.dev", "draft", false);
                conn.execute("UPDATE messages SET is_draft = 1 WHERE id = ?1", [draft_id])?;
                for (message_id, name) in [(sent_id, "sent"), (draft_id, "draft")] {
                    conn.execute(
                        "INSERT INTO draft_attachments (draft_id, file_path, filename)
                         VALUES (?1, ?2, ?2)",
                        rusqlite::params![message_id, name],
                    )?;
                }
                Ok((sent_id, draft_id))
            })
            .await
            .unwrap();

        core.cleanup_orphaned_draft_files().await.unwrap();
        let retained = core
            .db
            .read(|conn| {
                let mut statement =
                    conn.prepare("SELECT draft_id FROM draft_attachments ORDER BY draft_id")?;
                Ok(statement
                    .query_map([], |row| row.get::<_, i64>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?)
            })
            .await
            .unwrap();
        assert_ne!(sent_id, draft_id);
        assert_eq!(retained, vec![draft_id]);
    }

    fn draft_args(
        draft_id: Option<i64>,
        source: &std::path::Path,
        filename: &str,
    ) -> SaveDraftArgs {
        SaveDraftArgs {
            draft_id,
            account_id: 1,
            from: None,
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
        let staged_path = core
            .paths
            .draft_attachments_dir()
            .join("cancelled-send")
            .join("file.txt");
        std::fs::create_dir_all(staged_path.parent().unwrap()).unwrap();
        std::fs::write(&staged_path, b"attachment").unwrap();
        let stored_path = staged_path.to_string_lossy().into_owned();
        core.db
            .write(move |conn| {
                conn.execute(
                    "INSERT INTO draft_attachments (draft_id, file_path, filename)
                     VALUES (?1, ?2, 'file.txt')",
                    rusqlite::params![draft_id, stored_path],
                )?;
                Ok(())
            })
            .await
            .unwrap();

        core.delete_draft(draft_id).await.unwrap();
        assert!(!staged_path.exists());

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
mod send_confirmation_tests {
    use super::*;

    #[test]
    fn send_start_timeout_begins_at_scheduled_delivery() {
        let observed_at = 1_000;
        let scheduled_at = 121_000;
        assert!(!send_start_expired(61_000, observed_at, Some(scheduled_at)));
        assert!(!send_start_expired(
            180_999,
            observed_at,
            Some(scheduled_at)
        ));
        assert!(send_start_expired(181_000, observed_at, Some(scheduled_at)));
        assert!(send_start_expired(61_000, observed_at, None));
    }

    async fn send_action() -> (Core, tempfile::TempDir, i64) {
        let temp = tempfile::tempdir().unwrap();
        let core = Core::start_mail_ui(Paths::for_tests(temp.path()))
            .await
            .unwrap();
        let action_id = core
            .db
            .write(|conn| {
                db::testutil::seed_account(conn);
                repo::actions::enqueue(
                    conn,
                    1,
                    "send",
                    None,
                    None,
                    &serde_json::json!({ "draftId": 1 }),
                    None,
                )
            })
            .await
            .unwrap();
        (core, temp, action_id)
    }

    #[tokio::test]
    async fn confirmed_send_returns_only_after_done_state() {
        let (core, _temp, action_id) = send_action().await;
        let waiting_core = core.clone();
        let waiter = tokio::spawn(async move { waiting_core.wait_for_send(action_id).await });
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(!waiter.is_finished());

        core.db
            .write(move |conn| repo::actions::set_state(conn, action_id, "done", None))
            .await
            .unwrap();

        tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn failed_attempt_is_cancelled_before_returning_the_error() {
        let (core, _temp, action_id) = send_action().await;
        core.db
            .write(move |conn| {
                repo::actions::bump_attempt(
                    conn,
                    action_id,
                    now_ms() + 60_000,
                    "SMTP relay rejected the message",
                )
            })
            .await
            .unwrap();

        let error = core.wait_for_send(action_id).await.unwrap_err();
        assert!(error.to_string().contains("SMTP relay rejected"));
        core.db
            .read(move |conn| {
                assert_eq!(
                    repo::actions::get(conn, action_id)?.map(|action| action.state),
                    Some("cancelled".into())
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
        assert!(!core.paths.calendar_db_file().exists());
        assert!(!core.paths.files_db_file().exists());
        assert!(!core.calendar_db.is_open());
        assert!(!core.files_db.is_open());
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
                    "INSERT INTO contacts (email, name, send_count)
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
                    "INSERT INTO calendar_connections (id, account_id, kind, base_url)
                     VALUES (1, 1, 'generic', 'https://dav.example.test/')",
                    [],
                )?;
                conn.execute(
                    "INSERT INTO calendars (id, account_id, connection_id, url, display_name)
                     VALUES (1, 1, 1, 'local://primary', 'Primary')",
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
        assert_eq!(
            mail_contract,
            (
                crate::db::migrations::LATEST_VERSION,
                1,
                false,
                "ok".to_owned()
            )
        );

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
        assert_eq!(
            calendar_contract,
            (
                crate::db::calendar_migrations::LATEST_VERSION,
                1,
                false,
                "ok".to_owned()
            )
        );
    }
}

#[cfg(test)]
mod account_backup_import_tests {
    use super::*;

    fn portable_imap(email: &str) -> PortableAccountConfig {
        PortableAccountConfig {
            email: email.into(),
            display_name: Some("Portable account".into()),
            provider: Provider::Generic,
            auth_kind: AuthKind::Password,
            mail_protocol: MailProtocol::Imap,
            username: email.into(),
            jmap_url: String::new(),
            incoming_host: "imap.example.com".into(),
            incoming_port: 993,
            smtp_host: "smtp.example.com".into(),
            smtp_port: 465,
            settings: crate::db::testutil::account_settings(),
        }
    }

    #[test]
    fn only_password_accounts_may_connect_without_encryption() {
        let mut plain = portable_imap("plain@example.com");
        plain.settings.connection.incoming_security = ConnectionSecurity::None;
        plain.settings.connection.smtp_security = ConnectionSecurity::None;
        validate_portable_account(&plain).unwrap();

        let mut microsoft = portable_imap("oauth@example.com");
        microsoft.provider = Provider::Microsoft;
        microsoft.auth_kind = AuthKind::Oauth2;
        validate_portable_account(&microsoft).unwrap();
        microsoft.settings.connection.smtp_security = ConnectionSecurity::None;
        assert!(validate_portable_account(&microsoft).is_err());
    }

    #[test]
    fn pop3_accounts_import_only_with_password_sign_in() {
        let mut pop3 = portable_imap("pop3@example.com");
        pop3.mail_protocol = MailProtocol::Pop3;
        pop3.incoming_port = 995;
        validate_portable_account(&pop3).unwrap();

        let mut oauth = pop3.clone();
        oauth.provider = Provider::Microsoft;
        oauth.auth_kind = AuthKind::Oauth2;
        assert!(validate_portable_account(&oauth).is_err());

        // Only POP3 accounts remove mail from their server.
        pop3.settings.keep_on_server = KeepOnServer::UntilDownloaded;
        validate_portable_account(&pop3).unwrap();
        let mut imap = portable_imap("imap@example.com");
        imap.settings.keep_on_server = KeepOnServer::UntilDownloaded;
        assert!(validate_portable_account(&imap).is_err());

        pop3.incoming_host.clear();
        assert!(validate_portable_account(&pop3).is_err());
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
        assert_eq!(configs[0].incoming_host, "imap.example.com");
    }

    #[tokio::test]
    async fn imported_gmail_configuration_merges_by_mailbox() {
        let temp = tempfile::tempdir().unwrap();
        let core = Core::start_mail_ui(Paths::for_tests(temp.path()))
            .await
            .unwrap();
        let gmail = |email: &str| PortableAccountConfig {
            provider: Provider::Gmail,
            auth_kind: AuthKind::Oauth2,
            mail_protocol: MailProtocol::Gmail,
            ..portable_imap(email)
        };

        assert_eq!(
            core.import_account_configs(vec![
                gmail("jane.doe@gmail.com"),
                gmail("JaneDoe@googlemail.com"),
            ])
            .await
            .unwrap(),
            1
        );
        assert_eq!(
            core.import_account_configs(vec![gmail("janedoe@gmail.com")])
                .await
                .unwrap(),
            0
        );
        let accounts = core.list_accounts().await.unwrap();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].email, "jane.doe@gmail.com");
    }
}

#[cfg(test)]
mod local_data_reset_tests {
    use super::*;

    async fn start_test_core(paths: &Paths) -> Core {
        Core::start_mail_ui_with_credentials(
            paths.clone(),
            Arc::new(credentials::DevelopmentFileCredentialStore::new(
                paths.data_dir.join("test-credentials.json"),
            )),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn delete_all_local_data_keeps_databases_open_and_empties_profile() {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::for_tests(temp.path());
        let core = start_test_core(&paths).await;
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
        let core = start_test_core(&paths).await;
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

#[cfg(test)]
mod move_undo_tests {
    use super::*;
    use crate::db::testutil;

    fn mailboxes(conn: &rusqlite::Connection, message_id: i64) -> Vec<i64> {
        conn.prepare(
            "SELECT folder_id FROM message_folders WHERE message_id = ?1 ORDER BY folder_id",
        )
        .unwrap()
        .query_map([message_id], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
    }

    #[test]
    fn undoing_a_completed_jmap_move_restores_its_mailbox_membership() {
        let mut conn = testutil::conn();
        testutil::seed_account(&conn);
        conn.execute_batch(
            "UPDATE accounts SET mail_protocol = 'jmap' WHERE id = 1;
             UPDATE folders SET jmap_id = 'inbox' WHERE id = 1;
             INSERT INTO folders (id, account_id, path, role, jmap_id)
               VALUES (2, 1, 'Archive', 'archive', 'archive');",
        )
        .unwrap();
        let (thread_id, message_id) =
            testutil::seed_message(&conn, "a@example.com", "Plans", false);
        // The archive already reached the server.
        conn.execute(
            "UPDATE messages SET folder_id = 2, uid = NULL WHERE id = ?1",
            [message_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO message_folders (message_id, folder_id) VALUES (?1, 2)",
            [message_id],
        )
        .unwrap();
        let action_id = repo::actions::enqueue(
            &conn,
            1,
            "archive",
            Some(message_id),
            Some(thread_id),
            &serde_json::json!({"srcFolderId": 1, "srcUid": null, "targetFolderId": 2}),
            None,
        )
        .unwrap();
        repo::actions::set_state(&conn, action_id, "done", None).unwrap();
        let action = repo::actions::get(&conn, action_id).unwrap().unwrap();

        revert_action(&mut conn, &action).unwrap();

        let folder: i64 = conn
            .query_row(
                "SELECT folder_id FROM messages WHERE id = ?1",
                [message_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(folder, 1);
        assert_eq!(mailboxes(&conn, message_id), [1]);
    }
}

#[cfg(test)]
mod calendar_connection_tests {
    use super::*;
    use base64::Engine;
    use std::sync::Mutex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A loopback CalDAV server with one calendar collection per path, each
    /// accepting only its own credentials. It stores uploaded events and
    /// records every request as (method, path, credentials).
    #[derive(Default)]
    struct DavServer {
        // (collection path, display name, "user:password")
        collections: Mutex<Vec<(String, String, String)>>,
        requests: Mutex<Vec<(String, String, Option<String>)>>,
    }

    impl DavServer {
        fn set_secret(&self, collection: &str, secret: &str) {
            for entry in self.collections.lock().unwrap().iter_mut() {
                if entry.0 == collection {
                    entry.2 = secret.to_owned();
                }
            }
        }

        fn uploads(&self) -> Vec<(String, Option<String>)> {
            self.requests
                .lock()
                .unwrap()
                .iter()
                .filter(|(method, _, _)| method == "PUT")
                .map(|(_, path, credentials)| (path.clone(), credentials.clone()))
                .collect()
        }

        fn respond(&self, method: &str, path: &str, credentials: Option<&str>) -> String {
            self.requests.lock().unwrap().push((
                method.to_owned(),
                path.to_owned(),
                credentials.map(str::to_owned),
            ));
            let collections = self.collections.lock().unwrap();
            let Some((collection, name, secret)) = collections
                .iter()
                .find(|(collection, _, _)| {
                    path.trim_end_matches('/') == collection.trim_end_matches('/')
                        || path.starts_with(collection.as_str())
                })
                .cloned()
            else {
                return "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".into();
            };
            drop(collections);
            if credentials != Some(secret.as_str()) {
                return "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n".into();
            }
            let body = match method {
                "PUT" => {
                    return "HTTP/1.1 201 Created\r\nETag: \"1\"\r\nContent-Length: 0\r\n\r\n"
                        .into();
                }
                "PROPFIND" => format!(
                    r#"<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:response><d:href>{collection}</d:href><d:propstat><d:prop><d:resourcetype><d:collection/><c:calendar/></d:resourcetype><d:displayname>{name}</d:displayname></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#
                ),
                _ => r#"<d:multistatus xmlns:d="DAV:"></d:multistatus>"#.to_owned(),
            };
            format!(
                "HTTP/1.1 207 Multi-Status\r\nContent-Type: application/xml\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
        }
    }

    async fn dav_server(collections: &[(&str, &str, &str)]) -> (String, Arc<DavServer>) {
        let server = Arc::new(DavServer::default());
        *server.collections.lock().unwrap() = collections
            .iter()
            .map(|(path, name, secret)| {
                ((*path).to_owned(), (*name).to_owned(), (*secret).to_owned())
            })
            .collect();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let shared = server.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let server = shared.clone();
                tokio::spawn(async move {
                    let mut buffer = Vec::new();
                    loop {
                        let head_end = loop {
                            if let Some(end) =
                                buffer.windows(4).position(|window| window == b"\r\n\r\n")
                            {
                                break end + 4;
                            }
                            let mut chunk = [0u8; 4096];
                            match stream.read(&mut chunk).await {
                                Ok(0) | Err(_) => return,
                                Ok(read) => buffer.extend_from_slice(&chunk[..read]),
                            }
                        };
                        let head = String::from_utf8_lossy(&buffer[..head_end]).to_string();
                        let header = |name: &str| {
                            head.lines().find_map(|line| {
                                let (key, value) = line.split_once(':')?;
                                key.eq_ignore_ascii_case(name)
                                    .then(|| value.trim().to_owned())
                            })
                        };
                        let length: usize = header("content-length")
                            .and_then(|value| value.parse().ok())
                            .unwrap_or(0);
                        while buffer.len() < head_end + length {
                            let mut chunk = [0u8; 4096];
                            match stream.read(&mut chunk).await {
                                Ok(0) | Err(_) => return,
                                Ok(read) => buffer.extend_from_slice(&chunk[..read]),
                            }
                        }
                        buffer.drain(..head_end + length);
                        let mut request_line = head.lines().next().unwrap_or_default().split(' ');
                        let method = request_line.next().unwrap_or_default().to_owned();
                        let path = request_line.next().unwrap_or_default().to_owned();
                        let credentials = header("authorization")
                            .and_then(|value| value.strip_prefix("Basic ").map(str::to_owned))
                            .and_then(|value| {
                                base64::engine::general_purpose::STANDARD.decode(value).ok()
                            })
                            .map(|bytes| String::from_utf8_lossy(&bytes).to_string());
                        let response = server.respond(&method, &path, credentials.as_deref());
                        if stream.write_all(response.as_bytes()).await.is_err() {
                            return;
                        }
                    }
                });
            }
        });
        (format!("http://{address}"), server)
    }

    async fn start_test_core(paths: &Paths) -> Core {
        let core = Core::start_mail_ui_with_credentials(
            paths.clone(),
            Arc::new(credentials::DevelopmentFileCredentialStore::new(
                paths.data_dir.join("test-credentials.json"),
            )),
        )
        .await
        .unwrap();
        core.db
            .write(|conn| {
                db::testutil::seed_account(conn);
                Ok(())
            })
            .await
            .unwrap();
        core
    }

    fn caldav(connection_id: Option<i64>, url: &str, password: Option<&str>) -> ConnectCaldavArgs {
        ConnectCaldavArgs {
            account_id: 1,
            connection_id,
            url: url.to_owned(),
            username: " me ".to_owned(),
            password: password.map(str::to_owned),
        }
    }

    /// Wait for the account's calendar task to reach a state.
    async fn eventually(mut done: impl AsyncFnMut() -> bool) {
        for _ in 0..200 {
            if done().await {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        panic!("the calendar task never reached the expected state");
    }

    #[tokio::test]
    async fn an_account_adds_changes_and_removes_several_caldav_servers() {
        let (server, dav) = dav_server(&[
            ("/dav/own/", "Me", "me:own-password"),
            ("/dav/shared/", "Team", "me:shared-password"),
            ("/dav/third/", "Third", "me:third-password"),
        ])
        .await;
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::for_tests(temp.path());
        let core = start_test_core(&paths).await;

        let own = core
            .connect_caldav(caldav(
                None,
                &format!("{server}/dav/own/"),
                Some("own-password"),
            ))
            .await
            .unwrap();
        let shared = core
            .connect_caldav(caldav(
                None,
                &format!("{server}/dav/shared/"),
                Some("shared-password"),
            ))
            .await
            .unwrap();
        assert_eq!(
            (own.calendars.clone(), shared.calendars.clone()),
            (vec!["Me".to_owned()], vec!["Team".to_owned()])
        );
        assert_eq!(shared.username, "me");
        assert!(own.enabled && shared.enabled);
        assert!(core.cal_handles.read().await.contains_key(&1));

        // One calendar syncs through one connection per account, however
        // its address is written.
        let duplicate = core
            .connect_caldav(caldav(
                None,
                &format!("{server}/dav/shared"),
                Some("shared-password"),
            ))
            .await
            .err()
            .unwrap()
            .to_string();
        assert!(duplicate.contains("already syncs"), "{duplicate}");
        // A wrong password never creates a connection.
        assert!(matches!(
            core.connect_caldav(caldav(None, &format!("{server}/dav/third/"), Some("wrong")))
                .await,
            Err(CoreError::NeedsReauth)
        ));
        assert_eq!(core.list_calendar_connections().await.unwrap().len(), 2);

        // Changing a connection without a new password keeps the saved one;
        // the server accepts nothing else.
        let changed = core
            .connect_caldav(caldav(
                Some(shared.id),
                &format!("{server}/dav/shared/"),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(changed.id, shared.id);
        // Moving it to another server needs the password again; the new
        // server never sees the saved one.
        let (moved, elsewhere) =
            dav_server(&[("/dav/shared/", "Team", "me:shared-password")]).await;
        let refused = core
            .connect_caldav(caldav(
                Some(shared.id),
                &format!("{moved}/dav/shared/"),
                None,
            ))
            .await
            .err()
            .unwrap()
            .to_string();
        assert!(refused.contains("different server"), "{refused}");
        assert!(elsewhere.requests.lock().unwrap().is_empty());
        // A connection belongs to its account and its kind of server.
        assert!(
            core.connect_caldav(ConnectCaldavArgs {
                account_id: 2,
                ..caldav(Some(shared.id), &format!("{server}/dav/shared/"), None)
            })
            .await
            .is_err()
        );

        let calendars = core.list_calendars(Some(1)).await.unwrap();
        let default = calendars
            .iter()
            .find(|calendar| calendar.is_default)
            .unwrap();
        assert_eq!(default.connection_id, own.id);
        let team = calendars
            .iter()
            .find(|calendar| calendar.connection_id == shared.id)
            .unwrap()
            .id;

        // An event created in the shared calendar goes to that server only,
        // signed in as that connection.
        core.create_event(CreateEventArgs {
            account_id: 1,
            calendar_id: Some(team),
            summary: "Front desk rota".into(),
            description: None,
            location: None,
            join_url: None,
            starts_at: 1_800_000_000_000,
            ends_at: 1_800_003_600_000,
            all_day: false,
            attendees: Vec::new(),
        })
        .await
        .unwrap();
        eventually(async || !dav.uploads().is_empty()).await;
        let uploads = dav.uploads();
        assert_eq!(uploads.len(), 1);
        assert!(uploads[0].0.starts_with("/dav/shared/"), "{uploads:?}");
        assert_eq!(uploads[0].1.as_deref(), Some("me:shared-password"));

        // A server that stops accepting its password marks only its own
        // connection; the other keeps syncing.
        dav.set_secret("/dav/shared/", "me:rotated");
        core.calendar_sync_now(Some(1)).await;
        eventually(async || {
            let connections = core.list_calendar_connections().await.unwrap();
            connections
                .iter()
                .any(|c| c.id == shared.id && c.last_error.as_deref() == Some("needs_reauth"))
        })
        .await;
        let connections = core.list_calendar_connections().await.unwrap();
        let own_state = connections.iter().find(|c| c.id == own.id).unwrap();
        assert_eq!(own_state.last_error, None);

        core.remove_calendar_connection(own.id).await.unwrap();
        let calendars = core.list_calendars(Some(1)).await.unwrap();
        assert_eq!(calendars.len(), 1);
        assert!(calendars[0].is_default, "the account keeps a default");
        assert!(
            credentials::load_async(core.credentials.clone(), 1, Slot::CaldavPassword(own.id))
                .await
                .is_err()
        );
        assert_eq!(
            credentials::load_async(core.credentials.clone(), 1, Slot::CaldavPassword(shared.id))
                .await
                .unwrap(),
            "shared-password"
        );

        core.set_calendar_connection_enabled(shared.id, false)
            .await
            .unwrap();
        assert!(!core.cal_handles.read().await.contains_key(&1));
        core.set_calendar_connection_enabled(shared.id, true)
            .await
            .unwrap();
        assert!(core.cal_handles.read().await.contains_key(&1));

        // Removing the account takes the connection passwords along, which
        // the system keyring could not list by itself.
        core.remove_account(1).await.unwrap();
        assert!(core.list_calendar_connections().await.unwrap().is_empty());
        assert!(
            credentials::load_async(core.credentials.clone(), 1, Slot::CaldavPassword(shared.id))
                .await
                .is_err()
        );
    }
}
