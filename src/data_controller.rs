//! Account configuration transfer, verified database export, and local-data
//! reset callback wiring.
//!
//! These operations share the same sensitive-data boundary and platform file
//! picker constraints. Keeping them out of the composition root makes the
//! credential-exclusion and destructive-reset behavior reviewable together.

use super::*;
use chrono::Utc;
use flectar_mail_core::models::{
    MIN_WORKSPACE_LIST_PANE_WIDTH, PortableAccountConfig, Settings,
    normalized_workspace_list_pane_width,
};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

const MAX_ACCOUNT_BACKUP_BYTES: u64 = 1024 * 1024;
const STORAGE_CATEGORY_COUNT: usize = 5;
const STORAGE_MAIL: usize = 0;
const STORAGE_ATTACHMENTS: usize = 1;
const STORAGE_FILES: usize = 2;
const STORAGE_DATABASES: usize = 3;
const STORAGE_OTHER: usize = 4;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PortableMailProfile {
    name: String,
    color: String,
    #[serde(default)]
    account_emails: Vec<String>,
}

fn portable_account_colors(
    backup: &serde_json::Value,
    imported_emails: &HashSet<String>,
) -> Result<Option<HashMap<String, String>>, String> {
    let Some(value) = backup
        .get("preferences")
        .and_then(|preferences| preferences.get("accountColors"))
    else {
        return Ok(None);
    };
    let colors = serde_json::from_value::<HashMap<String, String>>(value.clone())
        .map_err(|error| format!("invalid account color data: {error}"))?;
    if colors.len() > 100 {
        return Err("backup contains more than 100 account colors".into());
    }
    let mut normalized = HashMap::new();
    for (email, color) in colors {
        let email = email.trim().to_ascii_lowercase();
        if !imported_emails.contains(&email) {
            return Err(format!(
                "account color references an account outside the backup: {email:?}"
            ));
        }
        let valid_color = color.len() == 7
            && color.starts_with('#')
            && color[1..].bytes().all(|byte| byte.is_ascii_hexdigit());
        if !valid_color {
            return Err(format!("account {email:?} has an invalid color"));
        }
        if normalized.insert(email.clone(), color).is_some() {
            return Err(format!("backup contains duplicate account color {email:?}"));
        }
    }
    Ok(Some(normalized))
}

fn portable_mail_profiles(
    backup: &serde_json::Value,
    imported_emails: &HashSet<String>,
) -> Result<Option<Vec<PortableMailProfile>>, String> {
    let Some(value) = backup
        .get("preferences")
        .and_then(|preferences| preferences.get("mailProfiles"))
    else {
        return Ok(None);
    };
    let profiles = serde_json::from_value::<Vec<PortableMailProfile>>(value.clone())
        .map_err(|error| format!("invalid profile data: {error}"))?;
    if profiles.len() > 50 {
        return Err("backup contains more than 50 profiles".into());
    }
    let mut assigned_emails = HashSet::new();
    let mut profile_names = HashSet::new();
    for profile in &profiles {
        let profile_name = profile.name.trim();
        if profile_name.is_empty()
            || profile_name.chars().count() > 48
            || profile_name.chars().any(char::is_control)
        {
            return Err("backup contains an invalid profile name".into());
        }
        let valid_color = profile.color.len() == 7
            && profile.color.starts_with('#')
            && profile.color[1..]
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit());
        if !valid_color {
            return Err(format!("profile {profile_name:?} has an invalid color"));
        }
        if !profile_names.insert(profile_name.to_ascii_lowercase()) {
            return Err(format!(
                "backup contains more than one profile named {:?}",
                profile.name
            ));
        }
        if profile.account_emails.len() > 100 {
            return Err(format!(
                "profile {:?} contains more than 100 accounts",
                profile.name
            ));
        }
        for email in &profile.account_emails {
            let email = email.trim().to_ascii_lowercase();
            if !imported_emails.contains(&email) {
                return Err(format!(
                    "profile {:?} references an account outside the backup",
                    profile.name
                ));
            }
            if !assigned_emails.insert(email.clone()) {
                return Err(format!(
                    "account {email:?} is assigned to more than one profile"
                ));
            }
        }
    }
    Ok(Some(profiles))
}

#[derive(Debug, Default, PartialEq, Eq)]
struct StorageScan {
    bytes: [u64; STORAGE_CATEGORY_COUNT],
    unreadable_entries: usize,
}

#[derive(Clone, Copy)]
enum StorageRoot {
    Data,
    Cache,
}

fn storage_category(root: StorageRoot, relative: &std::path::Path) -> usize {
    use std::path::Component;

    let first = relative.components().find_map(|component| match component {
        Component::Normal(value) => value.to_str(),
        _ => None,
    });
    match first {
        Some("mail") => STORAGE_MAIL,
        Some("attachments" | "draft_attachments") => STORAGE_ATTACHMENTS,
        Some("files" | "file_transfers") => STORAGE_FILES,
        _ if matches!(root, StorageRoot::Data)
            && relative
                .parent()
                .is_some_and(|parent| parent.as_os_str().is_empty())
            && relative
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    ["flectar-mail.db", "flectar-calendar.db", "flectar-files.db"]
                        .iter()
                        .any(|database| {
                            name == *database
                                || name.strip_prefix(database).is_some_and(|suffix| {
                                    matches!(suffix, "-wal" | "-shm" | "-journal")
                                })
                        })
                }) =>
        {
            STORAGE_DATABASES
        }
        _ => STORAGE_OTHER,
    }
}

fn scan_storage_tree(
    root: &std::path::Path,
    kind: StorageRoot,
    skipped_root: Option<&std::path::Path>,
    generation: &AtomicU64,
    ticket: u64,
    scan: &mut StorageScan,
) -> bool {
    let mut directories = vec![root.to_path_buf()];
    let mut visited = 0usize;
    while let Some(directory) = directories.pop() {
        if generation.load(Ordering::Acquire) != ticket {
            return false;
        }
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => {
                scan.unreadable_entries += 1;
                continue;
            }
        };
        for entry in entries {
            visited += 1;
            if visited.is_multiple_of(256) {
                if generation.load(Ordering::Acquire) != ticket {
                    return false;
                }
                std::thread::yield_now();
            }
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => {
                    scan.unreadable_entries += 1;
                    continue;
                }
            };
            let path = entry.path();
            if skipped_root.is_some_and(|skipped| path == skipped) {
                continue;
            }
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(_) => {
                    scan.unreadable_entries += 1;
                    continue;
                }
            };
            if file_type.is_dir() {
                directories.push(path);
            } else if file_type.is_file() {
                match entry.metadata() {
                    Ok(metadata) => {
                        let relative = path.strip_prefix(root).unwrap_or(&path);
                        let category = storage_category(kind, relative);
                        scan.bytes[category] = scan.bytes[category].saturating_add(metadata.len());
                    }
                    Err(_) => scan.unreadable_entries += 1,
                }
            }
        }
    }
    generation.load(Ordering::Acquire) == ticket
}

fn scan_storage(paths: &Paths, generation: &AtomicU64, ticket: u64) -> Option<StorageScan> {
    let mut scan = StorageScan::default();
    let nested_cache = (paths.cache_dir != paths.data_dir
        && paths.cache_dir.starts_with(&paths.data_dir))
    .then_some(paths.cache_dir.as_path());
    if !scan_storage_tree(
        &paths.data_dir,
        StorageRoot::Data,
        nested_cache,
        generation,
        ticket,
        &mut scan,
    ) {
        return None;
    }
    if paths.cache_dir != paths.data_dir
        && !scan_storage_tree(
            &paths.cache_dir,
            StorageRoot::Cache,
            paths
                .data_dir
                .starts_with(&paths.cache_dir)
                .then_some(paths.data_dir.as_path()),
            generation,
            ticket,
            &mut scan,
        )
    {
        return None;
    }
    Some(scan)
}

fn format_storage_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    const TB: f64 = GB * 1024.0;
    let bytes = bytes as f64;
    if bytes < KB {
        format!("{} B", bytes as u64)
    } else if bytes < MB {
        format!("{:.1} KB", bytes / KB)
    } else if bytes < GB {
        format!("{:.1} MB", bytes / MB)
    } else if bytes < TB {
        format!("{:.2} GB", bytes / GB)
    } else {
        format!("{:.2} TB", bytes / TB)
    }
}

fn storage_rows(scan: &StorageScan) -> Vec<StorageUsageRow> {
    let total = scan.bytes.iter().copied().sum::<u64>();
    let colors = [
        slint::Color::from_rgb_u8(0x3b, 0x82, 0xf6),
        slint::Color::from_rgb_u8(0xf5, 0x9e, 0x0b),
        slint::Color::from_rgb_u8(0x22, 0xc5, 0x5e),
        slint::Color::from_rgb_u8(0xa8, 0x55, 0xf7),
        slint::Color::from_rgb_u8(0x94, 0xa3, 0xb8),
    ];
    let kinds = ["mail", "attachments", "files", "databases", "other"];
    let mut offset = 0.0f32;
    scan.bytes
        .iter()
        .enumerate()
        .map(|(index, bytes)| {
            let fraction = if total == 0 {
                0.0
            } else {
                *bytes as f32 / total as f32
            };
            let row = StorageUsageRow {
                kind: kinds[index].into(),
                size: format_storage_size(*bytes).into(),
                fraction,
                offset,
                color: colors[index],
            };
            offset += fraction;
            row
        })
        .collect()
}

/// Outcome of restoring a Flectar account backup.
pub(super) struct BackupRestore {
    pub(super) imported: usize,
    /// Accounts in the backup that this device already had.
    pub(super) already_present: usize,
    /// The backup carried preferences, which replaced the current ones.
    pub(super) preferences: bool,
}

/// Restores a Flectar Mail account backup: account settings, profiles,
/// colors, and preferences. Existing accounts are kept unchanged.
pub(super) fn restore_account_backup(
    app: &AppWindow,
    bytes: &[u8],
    core: &CoreMailSource,
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Runtime,
) -> Result<BackupRestore, String> {
    if bytes.len() as u64 > MAX_ACCOUNT_BACKUP_BYTES {
        return Err("backup is larger than the 1 MB safety limit".into());
    }
    type Restored = (BackupRestore, Settings, Vec<Account>, Vec<AccountConfig>);
    let result = (|| -> Result<Restored, String> {
        let backup: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|error| format!("invalid JSON: {error}"))?;
        if backup.get("format").and_then(|value| value.as_str())
            != Some("flectar-mail-account-backup")
        {
            return Err("this is not a Flectar Mail account backup".into());
        }
        if backup.get("version").and_then(|value| value.as_u64()) != Some(1) {
            return Err("this backup version is not supported".into());
        }
        if backup
            .get("includesCredentials")
            .and_then(|value| value.as_bool())
            == Some(true)
        {
            return Err("backups containing credentials are not accepted".into());
        }
        let configs = serde_json::from_value::<Vec<PortableAccountConfig>>(
            backup
                .get("accounts")
                .cloned()
                .ok_or_else(|| "backup has no accounts list".to_owned())?,
        )
        .map_err(|error| format!("invalid account data: {error}"))?;
        if configs.len() > 100 {
            return Err("backup contains more than 100 accounts".into());
        }
        let imported_emails = configs
            .iter()
            .map(|config| config.email.trim().to_ascii_lowercase())
            .collect::<HashSet<_>>();
        let portable_profiles = portable_mail_profiles(&backup, &imported_emails)?;
        let has_portable_profiles = portable_profiles.is_some();
        let portable_profiles = portable_profiles.unwrap_or_default();
        let portable_colors = portable_account_colors(&backup, &imported_emails)?;
        let has_portable_colors = portable_colors.is_some();
        let portable_colors = portable_colors.unwrap_or_default();

        let mut settings = runtime.block_on(core.load_settings())?;
        let has_preferences = backup.get("preferences").is_some();
        if let Some(preferences) = backup.get("preferences") {
            if let Some(theme) = preferences.get("theme").and_then(|value| value.as_str())
                && matches!(theme, "system" | "carbon" | "snow" | "dark" | "light")
            {
                settings.theme = theme.to_owned();
            }
            if let Some(enabled) = preferences
                .get("monochromeSidebarIcons")
                .and_then(|value| value.as_bool())
            {
                settings.monochrome_sidebar_icons = enabled;
            }
            if let Some(enabled) = preferences
                .get("showAvatars")
                .and_then(|value| value.as_bool())
            {
                settings.show_avatars = enabled;
            }
            if let Some(frame) = preferences
                .get("windowFrame")
                .and_then(|value| value.as_str())
                && matches!(frame, "system" | "integrated")
            {
                settings.window_frame = frame.to_owned();
            }
            if let Some(layout) = preferences
                .get("workspaceLayout")
                .or_else(|| preferences.get("mailLayout"))
                .and_then(|value| value.as_str())
                && matches!(layout, "default" | "minimal")
            {
                settings.workspace_layout = layout.to_owned();
            }
            if let Some(preference) = preferences
                .get("messageAppearance")
                .and_then(|value| value.as_str())
                .and_then(flectar_mail_core::models::MessageAppearance::parse)
            {
                settings.message_appearance = preference;
            }
            if let Some(width) = preferences
                .get("workspaceListPaneWidth")
                .and_then(|value| value.as_i64())
            {
                settings.workspace_list_pane_width = normalized_workspace_list_pane_width(width);
            }
            if let Some(language) = preferences.get("language").and_then(|value| value.as_str())
                && language.len() <= 32
            {
                settings.language = language.to_owned();
            }
            if let Some(week_start) = preferences
                .get("calendarWeekStart")
                .and_then(|value| value.as_str())
                && matches!(week_start, "sunday" | "monday")
            {
                settings.calendar_week_start = week_start.to_owned();
            }
            if let Some(enabled) = preferences
                .get("loadRemoteImages")
                .and_then(|value| value.as_bool())
            {
                settings.load_remote_images = enabled;
            }
            if let Some(enabled) = preferences
                .get("markReadOnOpen")
                .and_then(|value| value.as_bool())
            {
                settings.mark_read_on_open = enabled;
            }
            if let Some(enabled) = preferences
                .get("collectOutgoingContacts")
                .and_then(|value| value.as_bool())
            {
                settings.collect_outgoing_contacts = enabled;
            }
            if let Some(enabled) = preferences
                .get("collectIncomingContacts")
                .and_then(|value| value.as_bool())
            {
                settings.collect_incoming_contacts = enabled;
            }
            if let Some(enabled) = preferences
                .get("contactSuggestAllAccounts")
                .and_then(|value| value.as_bool())
            {
                settings.contact_suggest_all_accounts = enabled;
            }
            if let Some(enabled) = preferences
                .get("suggestLearnedContacts")
                .and_then(|value| value.as_bool())
            {
                settings.suggest_learned_contacts = enabled;
            }
            if let Some(enabled) = preferences
                .get("notificationsEnabled")
                .and_then(|value| value.as_bool())
            {
                settings.notifications_enabled = enabled;
            }
            if let Some(scope) = preferences
                .get("notificationScope")
                .and_then(|value| value.as_str())
                && matches!(scope, "important" | "all")
            {
                settings.notification_scope = scope.to_owned();
            }
            if let Some(enabled) = preferences
                .get("soundEnabled")
                .and_then(|value| value.as_bool())
            {
                settings.sound_enabled = enabled;
            }
            if let Some(minutes) = preferences
                .get("syncIntervalMinutes")
                .and_then(|value| value.as_i64())
                && matches!(minutes, 1 | 5 | 15)
            {
                settings.sync_interval_minutes = minutes;
            }
            if let Some(enabled) = preferences
                .get("closeToTray")
                .and_then(|value| value.as_bool())
            {
                settings.close_to_tray = enabled;
            }
            if let Some(enabled) = preferences
                .get("showAccountMarkers")
                .and_then(|value| value.as_bool())
            {
                settings.show_account_badges = enabled;
            }
        }

        let backup_accounts = configs.len();
        let imported = runtime.block_on(core.import_account_configs(configs))?;
        runtime.block_on(core.save_settings(settings.clone()))?;
        let accounts = runtime.block_on(core.load_accounts())?;
        let configs = runtime.block_on(core.load_account_configs())?;
        let account_ids = accounts
            .iter()
            .map(|account| (account.email.trim().to_ascii_lowercase(), account.id))
            .collect::<HashMap<_, _>>();
        if has_portable_colors {
            for email in &imported_emails {
                if let Some(account_id) = account_ids.get(email) {
                    runtime.block_on(core.set_account_color(*account_id, None))?;
                }
            }
            for (email, color) in portable_colors {
                if let Some(account_id) = account_ids.get(&email) {
                    runtime.block_on(core.set_account_color(*account_id, Some(color)))?;
                }
            }
        }
        if has_portable_profiles {
            for email in &imported_emails {
                if let Some(account_id) = account_ids.get(email) {
                    runtime.block_on(core.assign_account_profile(*account_id, None))?;
                }
            }
        }
        for profile in portable_profiles {
            let current = runtime.block_on(core.load_settings())?;
            let existing_id = current
                .mail_profiles
                .iter()
                .find(|candidate| candidate.name.eq_ignore_ascii_case(profile.name.trim()))
                .map(|candidate| candidate.id.clone());
            let (_, saved) = runtime.block_on(core.save_mail_profile(
                existing_id,
                profile.name,
                profile.color,
            ))?;
            for email in profile.account_emails {
                if let Some(account_id) = account_ids.get(&email.trim().to_ascii_lowercase()) {
                    runtime.block_on(
                        core.assign_account_profile(*account_id, Some(saved.id.clone())),
                    )?;
                }
            }
        }
        settings = runtime.block_on(core.load_settings())?;
        let restore = BackupRestore {
            imported,
            already_present: backup_accounts.saturating_sub(imported),
            preferences: has_preferences,
        };
        Ok((restore, settings, accounts, configs))
    })();

    let (restore, settings, accounts, configs) = result?;
    apply_language(app, &settings.language);
    state.borrow_mut().account_presentation = AccountPresentationSettings::from_settings(&settings);
    update_connected_accounts(app, state, runtime, accounts, configs);
    refresh_rows_only(app, state, runtime);
    app.set_theme_mode(
        match settings.theme.as_str() {
            "carbon" | "dark" => "dark",
            "snow" | "light" => "light",
            _ => "system",
        }
        .into(),
    );
    app.set_monochrome_sidebar_icons(settings.monochrome_sidebar_icons);
    app.global::<MessageColors>()
        .set_preference(settings.message_appearance.as_str().into());
    app.set_show_avatars(settings.show_avatars);
    app.set_show_account_markers(settings.show_account_badges);
    crate::window_chrome::apply_preference(
        app,
        crate::window_chrome::WindowFrame::parse(&settings.window_frame),
    );
    app.set_workspace_layout(settings.workspace_layout.clone().into());
    app.set_workspace_list_pane_width(normalized_workspace_list_pane_width(
        settings.workspace_list_pane_width,
    ) as f32);
    app.set_notifications_enabled(settings.notifications_enabled);
    app.set_notification_sound_enabled(settings.sound_enabled);
    app.set_notification_scope(settings.notification_scope.clone().into());
    app.set_sync_interval_minutes(settings.sync_interval_minutes as i32);
    app.set_mark_read_on_open(settings.mark_read_on_open);
    app.set_collect_outgoing_contacts(settings.collect_outgoing_contacts);
    app.set_collect_incoming_contacts(settings.collect_incoming_contacts);
    app.set_contact_suggest_all_accounts(settings.contact_suggest_all_accounts);
    app.set_suggest_learned_contacts(settings.suggest_learned_contacts);
    state.borrow_mut().mark_read_on_open = settings.mark_read_on_open;
    app.set_remote_images_enabled(settings.load_remote_images && cfg!(feature = "remote-content"));
    #[cfg(not(any(mobile, feature = "flatpak")))]
    app.set_close_to_tray(settings.close_to_tray);
    #[cfg(any(mobile, feature = "flatpak"))]
    app.set_close_to_tray(false);
    Ok(restore)
}

/// Hands generated bytes to the document provider through a private staging
/// file; the host copies it to the destination its picker offers.
fn export_document_bytes(
    documents: std::sync::Arc<dyn documents::DocumentProvider>,
    staging_root: PathBuf,
    bytes: Vec<u8>,
    name: String,
    dialog: documents::DocumentDialog,
) -> documents::DocumentFuture<bool> {
    Box::pin(async move {
        tokio::fs::create_dir_all(&staging_root)
            .await
            .map_err(|error| format!("creating {}: {error}", staging_root.display()))?;
        let staging = tempfile::tempdir_in(&staging_root)
            .map_err(|error| format!("creating staging in {}: {error}", staging_root.display()))?;
        let path = staging.path().join(&name);
        tokio::fs::write(&path, bytes)
            .await
            .map_err(|error| format!("writing {}: {error}", path.display()))?;
        documents.export_with(path, name, dialog).await
    })
}

fn backup_dialog(app: &AppWindow, title: UiMessage) -> documents::DocumentDialog {
    documents::DocumentDialog {
        title: translated(app, &title).to_string(),
        filter: Some((
            translated(app, &UiMessage::plain("Flectar Mail backup")).to_string(),
            &["json"],
        )),
        directory: None,
    }
}

#[cfg(desktop)]
fn pick_database_snapshot_path(title: String) -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title(title)
        .pick_folder()
        .map(|directory| {
            directory.join(format!(
                "flectar-mail-snapshot-{}",
                Local::now().format("%Y-%m-%d-%H%M%S")
            ))
        })
}

/// Mobile hosts export single files from their app-private storage, so the
/// verified snapshot is staged beside the data directory (Core refuses
/// destinations inside the data and cache roots) and archived as one zip.
#[cfg(mobile)]
fn snapshot_staging_root(paths: &Paths) -> Result<PathBuf, String> {
    Ok(paths
        .data_dir
        .parent()
        .ok_or("the data directory has no parent for snapshot staging")?
        .join("snapshot-staging"))
}

/// A zipped snapshot waiting in staging for the export picker.
#[cfg(mobile)]
struct StagedSnapshot {
    root: PathBuf,
    archive: PathBuf,
    name: String,
    /// Keeps the startup cleanup out of the staging root until it is removed.
    _staging: tokio::sync::OwnedMutexGuard<()>,
}

#[cfg(mobile)]
impl StagedSnapshot {
    async fn remove(self) {
        let _ = tokio::fs::remove_dir_all(&self.root).await;
    }
}

#[cfg(mobile)]
async fn stage_database_snapshot(
    core: CoreMailSource,
    paths: Paths,
    staging: Arc<tokio::sync::Mutex<()>>,
) -> Result<StagedSnapshot, String> {
    let staging = staging.lock_owned().await;
    let root = snapshot_staging_root(&paths)?;
    // The root only ever holds this export, so clearing it also removes what
    // an interrupted export left behind.
    let _ = tokio::fs::remove_dir_all(&root).await;
    let staged = async {
        tokio::fs::create_dir_all(&root)
            .await
            .map_err(|error| format!("creating {}: {error}", root.display()))?;
        let name = format!(
            "flectar-mail-snapshot-{}",
            Local::now().format("%Y-%m-%d-%H%M%S")
        );
        let directory = root.join(&name);
        core.create_database_snapshot(directory.clone())
            .await
            .map_err(|error| error.to_string())?;
        let name = format!("{name}.zip");
        let archive = root.join(&name);
        let archive_for_zip = archive.clone();
        tokio::task::spawn_blocking(move || {
            archive_snapshot_directory(
                &directory,
                &archive_for_zip,
                flectar_mail_core::files::MAX_TRANSFER as u64,
            )
        })
        .await
        .map_err(|error| error.to_string())??;
        Ok((archive, name))
    }
    .await;
    match staged {
        Ok((archive, name)) => Ok(StagedSnapshot {
            root,
            archive,
            name,
            _staging: staging,
        }),
        Err(error) => {
            let _ = tokio::fs::remove_dir_all(&root).await;
            Err(error)
        }
    }
}

/// Archives a snapshot directory without compression: the stores are dense
/// SQLite files plus already-compressed attachments. Symlinks are skipped so
/// nothing outside the snapshot enters the archive. Hosts transfer at most
/// `limit` bytes, so a larger snapshot fails before the archive is written.
#[cfg(any(mobile, test))]
fn archive_snapshot_directory(
    directory: &std::path::Path,
    archive: &std::path::Path,
    limit: u64,
) -> Result<(), String> {
    /// Archive name, source path and, for files, the size.
    type Entry = (String, PathBuf, Option<u64>);
    fn collect(
        root: &std::path::Path,
        directory: &std::path::Path,
        entries: &mut Vec<Entry>,
    ) -> Result<(), String> {
        let mut children = std::fs::read_dir(directory)
            .map_err(|error| format!("reading {}: {error}", directory.display()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        children.sort_by_key(|entry| entry.file_name());
        for entry in children {
            let path = entry.path();
            let kind = entry.file_type().map_err(|error| error.to_string())?;
            let name = path
                .strip_prefix(root)
                .map_err(|error| error.to_string())?
                .to_string_lossy()
                .replace('\\', "/");
            if kind.is_dir() {
                entries.push((format!("{name}/"), path.clone(), None));
                collect(root, &path, entries)?;
            } else if kind.is_file() {
                let size = entry.metadata().map_err(|error| error.to_string())?.len();
                entries.push((name, path, Some(size)));
            }
        }
        Ok(())
    }
    let too_large = |size| {
        format!(
            "the snapshot is {}, more than the {} this device can export",
            format_storage_size(size),
            format_storage_size(limit)
        )
    };
    let mut entries = Vec::new();
    collect(directory, directory, &mut entries)?;
    let size = entries.iter().filter_map(|(_, _, size)| *size).sum::<u64>();
    if size > limit {
        return Err(too_large(size));
    }
    let file = std::fs::File::create(archive)
        .map_err(|error| format!("creating {}: {error}", archive.display()))?;
    let mut zip = zip::ZipWriter::new(file);
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (name, path, size) in entries {
        let Some(size) = size else {
            zip.add_directory(name, options)
                .map_err(|error| error.to_string())?;
            continue;
        };
        let mut source = std::fs::File::open(&path)
            .map_err(|error| format!("reading {}: {error}", path.display()))?;
        // Stores past 4 GiB need ZIP64 entries.
        zip.start_file(name, options.large_file(size >= u64::from(u32::MAX)))
            .map_err(|error| error.to_string())?;
        std::io::copy(&mut source, &mut zip).map_err(|error| error.to_string())?;
    }
    let size = zip
        .finish()
        .and_then(|file| Ok(file.metadata()?.len()))
        .map_err(|error| error.to_string())?;
    // Entry headers can still push the archive past the limit.
    if size > limit {
        return Err(too_large(size));
    }
    Ok(())
}

/// Clears the in-progress flag when the snapshot task ends, even by panic.
struct SnapshotRunning(Arc<AtomicBool>);

impl Drop for SnapshotRunning {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn register_data_management_callbacks(
    app: &AppWindow,
    paths: &Paths,
    documents: &std::sync::Arc<dyn documents::DocumentProvider>,
    state: &Rc<RefCell<InboxState>>,
    runtime: &Rc<tokio::runtime::Runtime>,
    // Only the desktop snapshot export reports through the task channel.
    #[cfg_attr(mobile, allow(unused_variables))] ui_task_tx: &UiSender<UiTaskUpdate>,
    contact_state: &Rc<RefCell<ContactDirectoryState>>,
    contacts_loaded: &Rc<Cell<bool>>,
    contacts_loading: &Rc<Cell<bool>>,
    contact_load_generation: &Rc<Cell<u64>>,
    calendar_state: &Rc<RefCell<LocalCalendarState>>,
) {
    // Directory walking stays completely dormant until the Storage page asks
    // for it. A dedicated, cancellable worker avoids consuming either the UI
    // thread or Tokio's two workers used for interactive mail operations.
    let storage_generation = Arc::new(AtomicU64::new(0));
    let cancel_generation = Arc::clone(&storage_generation);
    let cancel_app = app.as_weak();
    app.on_cancel_storage_usage(move || {
        cancel_generation.fetch_add(1, Ordering::AcqRel);
        if let Some(app) = cancel_app.upgrade() {
            app.set_storage_loading(false);
        }
    });

    let load_generation = Arc::clone(&storage_generation);
    let storage_paths = paths.clone();
    let storage_app = app.as_weak();
    app.on_load_storage_usage(move || {
        let Some(app) = storage_app.upgrade() else {
            return;
        };
        if app.get_storage_loading() || !app.get_settings_open() || app.get_settings_tab() != "Data"
        {
            return;
        }
        let ticket = load_generation.fetch_add(1, Ordering::AcqRel) + 1;
        app.set_storage_loading(true);
        app.set_storage_error("".into());
        let worker_generation = Arc::clone(&load_generation);
        let result_generation = Arc::clone(&load_generation);
        let paths = storage_paths.clone();
        let weak = app.as_weak();
        let spawn = std::thread::Builder::new()
            .name("flectar-storage-scan".into())
            .spawn(move || {
                let Some(scan) = scan_storage(&paths, &worker_generation, ticket) else {
                    return;
                };
                let total = scan.bytes.iter().copied().sum::<u64>();
                let rows = storage_rows(&scan);
                let incomplete = scan.unreadable_entries > 0;
                let _ = weak.upgrade_in_event_loop(move |app| {
                    if result_generation.load(Ordering::Acquire) != ticket {
                        return;
                    }
                    app.set_storage_total(format_storage_size(total).into());
                    app.set_storage_usage(ModelRc::new(VecModel::from(rows)));
                    app.set_storage_error(if incomplete {
                        "incomplete".into()
                    } else {
                        "".into()
                    });
                    app.set_storage_loaded(true);
                    app.set_storage_loading(false);
                });
            });
        if spawn.is_err() {
            app.set_storage_loading(false);
            app.set_storage_error("failed".into());
        }
    });

    let app_weak = app.as_weak();
    let state_for_backup = Rc::clone(state);
    let runtime_for_backup = Rc::clone(runtime);
    let documents_for_backup = documents.clone();
    let paths_for_backup = paths.clone();
    let exporting_backup = Rc::new(Cell::new(false));
    app.on_export_account_backup(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        // The dialog does not block the window; a second click while it is
        // open would start a second export.
        if exporting_backup.get() {
            return;
        }
        let (core, configs) = {
            let state = state_for_backup.borrow();
            (state.core.clone(), state.account_configs.clone())
        };
        if configs.is_empty() {
            app.set_sync_status(UiMessage::plain(
                "Connect an account before creating a backup.",
            ));
            return;
        }
        let preferences = core
            .as_ref()
            .and_then(|core| runtime_for_backup.block_on(core.load_settings()).ok())
            .map(|settings| {
                let mail_profiles = settings
                    .mail_profiles
                    .iter()
                    .map(|profile| PortableMailProfile {
                        name: profile.name.clone(),
                        color: profile.color.clone(),
                        account_emails: configs
                            .iter()
                            .filter(|account| profile.account_ids.contains(&account.id))
                            .map(|account| account.email.clone())
                            .collect(),
                    })
                    .collect::<Vec<_>>();
                let account_colors = configs
                    .iter()
                    .filter_map(|account| {
                        settings
                            .account_colors
                            .get(&account.id.to_string())
                            .map(|color| (account.email.clone(), color.clone()))
                    })
                    .collect::<HashMap<_, _>>();
                serde_json::json!({
                    "theme": settings.theme,
                    "messageAppearance": settings.message_appearance,
                    "showAvatars": settings.show_avatars,
                    "workspaceLayout": settings.workspace_layout,
                    "workspaceListPaneWidth": settings.workspace_list_pane_width,
                    "monochromeSidebarIcons": settings.monochrome_sidebar_icons,
                    "language": settings.language,
                    "calendarWeekStart": settings.calendar_week_start,
                    "loadRemoteImages": settings.load_remote_images,
                    "markReadOnOpen": settings.mark_read_on_open,
                    "collectOutgoingContacts": settings.collect_outgoing_contacts,
                    "collectIncomingContacts": settings.collect_incoming_contacts,
                    "contactSuggestAllAccounts": settings.contact_suggest_all_accounts,
                    "suggestLearnedContacts": settings.suggest_learned_contacts,
                    "notificationsEnabled": settings.notifications_enabled,
                    "notificationScope": settings.notification_scope,
                    "soundEnabled": settings.sound_enabled,
                    "syncIntervalMinutes": settings.sync_interval_minutes,
                    "closeToTray": settings.close_to_tray,
                    "windowFrame": settings.window_frame,
                    "showAccountMarkers": settings.show_account_badges,
                    "accountColors": account_colors,
                    "mailProfiles": mail_profiles
                })
            })
            .unwrap_or_else(|| serde_json::json!({}));
        let accounts = configs
            .iter()
            .map(|account| {
                serde_json::json!({
                    "email": account.email,
                    "displayName": account.display_name,
                    "provider": account.provider.as_str(),
                    "authKind": account.auth_kind.as_str(),
                    "mailProtocol": account.mail_protocol.as_str(),
                    "username": account.username,
                    "jmapUrl": account.jmap_url,
                    "imapHost": account.imap_host,
                    "imapPort": account.imap_port,
                    "smtpHost": account.smtp_host,
                    "smtpPort": account.smtp_port,
                    "settings": account.settings
                })
            })
            .collect::<Vec<_>>();
        let backup = serde_json::json!({
            "format": "flectar-mail-account-backup",
            "version": 1,
            "createdAt": Utc::now().to_rfc3339(),
            "includesCredentials": false,
            "accounts": accounts,
            "preferences": preferences
        });
        let bytes = match serde_json::to_vec_pretty(&backup) {
            Ok(bytes) => bytes,
            Err(error) => {
                app.set_sync_status(UiMessage::detail("Could not export backup: {}", error));
                return;
            }
        };
        let name = format!(
            "flectar-mail-backup-{}.json",
            Local::now().format("%Y-%m-%d")
        );
        let dialog = backup_dialog(&app, UiMessage::plain("Export Flectar Mail account backup"));
        let request = export_document_bytes(
            documents_for_backup.clone(),
            paths_for_backup.temp_dir(),
            bytes,
            name,
            dialog,
        );
        let app_weak = app.as_weak();
        let exporting = Rc::clone(&exporting_backup);
        exporting.set(true);
        documents::run_request(&runtime_for_backup, request, move |result| {
            exporting.set(false);
            let Some(app) = app_weak.upgrade() else {
                return;
            };
            match result {
                Ok(true) => app.set_sync_status(UiMessage::plain(
                    "Backup exported. Passwords and OAuth tokens were not included.",
                )),
                Ok(false) => {}
                Err(error) => {
                    app.set_sync_status(UiMessage::detail("Could not export backup: {}", error))
                }
            }
        });
    });

    let app_weak = app.as_weak();
    let state_for_database_snapshot = Rc::clone(state);
    let runtime_for_database_snapshot = Rc::clone(runtime);
    #[cfg(desktop)]
    let updates_for_database_snapshot = ui_task_tx.clone();
    #[cfg(mobile)]
    let documents_for_database_snapshot = documents.clone();
    #[cfg(mobile)]
    let paths_for_database_snapshot = paths.clone();
    #[cfg(mobile)]
    let snapshot_staging = Arc::new(tokio::sync::Mutex::new(()));
    // A killed export leaves a full copy of the stores in staging; reclaim it
    // now instead of at the next export.
    #[cfg(mobile)]
    if let Ok(root) = snapshot_staging_root(paths) {
        let staging = Arc::clone(&snapshot_staging);
        runtime.spawn(async move {
            let _staging = staging.lock().await;
            let _ = tokio::fs::remove_dir_all(root).await;
        });
    }
    let database_snapshot_running = Arc::new(AtomicBool::new(false));
    app.on_export_database_snapshot(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let Some(core) = state_for_database_snapshot.borrow().core.clone() else {
            app.set_sync_status(UiMessage::plain(
                "Local data storage is unavailable. Retry startup.",
            ));
            return;
        };
        if database_snapshot_running.load(Ordering::Acquire) {
            app.set_sync_status(UiMessage::plain("Creating verified database snapshot…"));
            return;
        }
        #[cfg(desktop)]
        let Some(path) = pick_database_snapshot_path(
            translated(
                &app,
                &UiMessage::plain("Choose where to export the database snapshot"),
            )
            .to_string(),
        ) else {
            return;
        };

        database_snapshot_running.store(true, Ordering::Release);
        app.set_sync_status(UiMessage::plain("Creating verified database snapshot…"));
        let running = SnapshotRunning(Arc::clone(&database_snapshot_running));
        #[cfg(desktop)]
        {
            let updates = updates_for_database_snapshot.clone();
            runtime_for_database_snapshot.spawn(async move {
                let display_path = path.display().to_string();
                let message = match core.create_database_snapshot(path).await {
                    Ok(_) => UiMessage::detail("Database snapshot exported to {}.", display_path),
                    Err(error) => {
                        UiMessage::detail("Could not export database snapshot: {}", error)
                    }
                };
                drop(running);
                let _ = updates
                    .send(UiTaskUpdate {
                        message,
                        accounts: None,
                        calendar_connections: None,
                        carddav_connections: None,
                        calendar_error: None,
                        account_removal: None,
                        clear_account_form: false,
                        finishes_account_setup: false,
                        finishes_oauth: false,
                        close_to_tray: None,
                    })
                    .await;
            });
        }
        // Staging runs on the runtime; only the export picker goes through
        // the shared document request path.
        #[cfg(mobile)]
        {
            let staged = runtime_for_database_snapshot.spawn(stage_database_snapshot(
                core,
                paths_for_database_snapshot.clone(),
                Arc::clone(&snapshot_staging),
            ));
            let abort_staging = staged.abort_handle();
            let app_weak = app.as_weak();
            let documents = documents_for_database_snapshot.clone();
            let runtime = Rc::clone(&runtime_for_database_snapshot);
            let spawned = slint::spawn_local(async move {
                let snapshot = match staged.await.unwrap_or_else(|error| Err(error.to_string())) {
                    Ok(snapshot) => snapshot,
                    Err(error) => {
                        drop(running);
                        if let Some(app) = app_weak.upgrade() {
                            app.set_sync_status(UiMessage::detail(
                                "Could not export database snapshot: {}",
                                error,
                            ));
                        }
                        return;
                    }
                };
                let request = documents.export(snapshot.archive.clone(), snapshot.name.clone());
                let cleanup = runtime.handle().clone();
                documents::run_request(&runtime, request, move |exported| {
                    let name = snapshot.name.clone();
                    cleanup.spawn(snapshot.remove());
                    drop(running);
                    let Some(app) = app_weak.upgrade() else {
                        return;
                    };
                    app.set_sync_status(match exported {
                        Ok(true) => UiMessage::detail("Database snapshot exported to {}.", name),
                        // Cancelled at the picker: return the status line to idle.
                        Ok(false) => UiMessage::plain("Local data ready."),
                        Err(error) => {
                            UiMessage::detail("Could not export database snapshot: {}", error)
                        }
                    });
                });
            });
            if let Err(error) = spawned {
                abort_staging.abort();
                app.set_sync_status(UiMessage::detail(
                    "Could not export database snapshot: {}",
                    error,
                ));
            }
        }
    });

    let (data_reset_raw_tx, data_reset_rx) = bounded_ui_channel::<Result<(), String>>();
    let data_reset_tx = UiSender::new(
        data_reset_raw_tx,
        UiWake::new(app.as_weak(), |app| app.invoke_drain_data_reset_updates()),
    );
    let data_reset_rx = Rc::new(RefCell::new(data_reset_rx));
    let data_reset_app = app.as_weak();
    let data_reset_state = Rc::clone(state);
    let data_reset_contacts = Rc::clone(contact_state);
    let data_reset_contacts_loaded = Rc::clone(contacts_loaded);
    let data_reset_contacts_loading = Rc::clone(contacts_loading);
    let data_reset_contact_generation = Rc::clone(contact_load_generation);
    let data_reset_calendar = Rc::clone(calendar_state);
    let data_reset_runtime = Rc::clone(runtime);
    app.on_drain_data_reset_updates(move || {
        while let Ok(update) = data_reset_rx.borrow_mut().try_recv() {
            let Some(app) = data_reset_app.upgrade() else {
                return;
            };
            match update {
                Ok(()) => {
                    {
                        let mut state = data_reset_state.borrow_mut();
                        state.connected_accounts.clear();
                        state.account_configs.clear();
                        state.account_presentation = AccountPresentationSettings::default();
                        state.calendar_connections.clear();
                        state.messages.clear();
                        state.mailboxes.clear();
                        state.unified_mailboxes.clear();
                        state.inbox_count = 0;
                        state.next_cursor = None;
                        state.selected_id = None;
                        state.rendered_id = None;
                        state.using_core = true;
                        state.mark_read_on_open = true;
                    }
                    refresh_connected_accounts(&app, &data_reset_state);
                    let _ = render_current(&app, &data_reset_state, &data_reset_runtime);
                    {
                        let mut contacts = data_reset_contacts.borrow_mut();
                        contacts.contacts.clear();
                        contacts.selected_id = None;
                        contacts.query.clear();
                        contacts.scope = "All contacts".to_owned();
                        contacts.page = 1;
                        contacts.next_cursor = None;
                        contacts.total_count = 0;
                        contacts.favorite_count = 0;
                        contacts.account_counts.clear();
                        contacts.editing_new = false;
                        contacts.using_core = true;
                    }
                    data_reset_contact_generation
                        .set(data_reset_contact_generation.get().wrapping_add(1));
                    data_reset_contacts_loaded.set(true);
                    data_reset_contacts_loading.set(false);
                    app.set_contact_loading_more(false);
                    app.set_contact_list_revision(app.get_contact_list_revision().wrapping_add(1));
                    apply_contact_directory(&app, &data_reset_contacts);
                    let today = Local::now().date_naive();
                    {
                        let mut calendar = data_reset_calendar.borrow_mut();
                        calendar.events.clear();
                        calendar.selected_date = today;
                        calendar.visible_month = first_of_month(today);
                    }
                    apply_calendar(&app, &data_reset_calendar.borrow(), today);
                    app.set_theme_mode("system".into());
                    app.global::<MessageColors>().set_preference("system".into());
                    app.set_monochrome_sidebar_icons(false);
                    app.set_show_avatars(true);
                    app.set_show_account_markers(false);
                    app.set_workspace_layout("default".into());
                    app.set_workspace_list_pane_width(MIN_WORKSPACE_LIST_PANE_WIDTH as f32);
                    app.set_notifications_enabled(true);
                    app.set_notification_sound_enabled(true);
                    app.set_notification_scope("important".into());
                    app.set_sync_interval_minutes(5);
                    app.set_mark_read_on_open(true);
                    app.set_remote_images_enabled(false);
                    app.set_close_to_tray(false);
                    crate::window_chrome::apply_preference(
                        &app,
                        crate::window_chrome::WindowFrame::System,
                    );
                    app.global::<TabsUi>().invoke_reset();
                    app.invoke_cancel_storage_usage();
                    app.set_storage_loaded(false);
                    app.set_storage_usage(ModelRc::default());
                    app.invoke_load_storage_usage();
                    app.set_sync_status(UiMessage::plain(
                        "All local Flectar Mail data was deleted.",
                    ));
                }
                Err(error) => app.set_sync_status(UiMessage::detail(
                    "Could not delete all local data: {}",
                    error,
                )),
            }
        }
    });

    let app_weak = app.as_weak();
    let state_for_data_reset = Rc::clone(state);
    let runtime_for_data_reset = Rc::clone(runtime);
    app.on_delete_all_data(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let Some(core) = state_for_data_reset.borrow().core.clone() else {
            app.set_sync_status(UiMessage::plain(
                "Local data storage is unavailable. Retry startup.",
            ));
            return;
        };
        app.set_sync_status(UiMessage::plain("Deleting local accounts and cached data…"));
        let updates = data_reset_tx.clone();
        runtime_for_data_reset.spawn(async move {
            let result = core.delete_all_local_data().await;
            let _ = updates.send(result).await;
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_scan_lifetime_follows_settings_visibility() {
        use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
        struct Headless(Rc<MinimalSoftwareWindow>);
        impl slint::platform::Platform for Headless {
            fn create_window_adapter(
                &self,
            ) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
                Ok(self.0.clone())
            }
        }
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(Headless(window))).unwrap();
        let app = AppWindow::new().unwrap();
        let calls = Rc::new(RefCell::new(Vec::new()));
        let loads = calls.clone();
        app.on_load_storage_usage(move || loads.borrow_mut().push("load"));
        let cancellations = calls.clone();
        app.on_cancel_storage_usage(move || cancellations.borrow_mut().push("cancel"));
        slint::platform::update_timers_and_animations();
        calls.borrow_mut().clear();

        app.set_settings_open(true);
        slint::platform::update_timers_and_animations();
        assert!(calls.borrow().is_empty());
        app.set_settings_tab("Data".into());
        slint::platform::update_timers_and_animations();
        app.set_settings_open(false);
        slint::platform::update_timers_and_animations();
        app.set_settings_open(true);
        slint::platform::update_timers_and_animations();
        app.set_settings_tab("About".into());
        slint::platform::update_timers_and_animations();
        assert_eq!(*calls.borrow(), ["load", "cancel", "load", "cancel"]);
    }

    #[test]
    fn storage_scan_counts_app_data_by_category_and_honors_cancellation() {
        let root = tempfile::tempdir().unwrap();
        let paths = Paths::new(root.path().join("data"), root.path().join("cache"));
        for directory in [
            paths.mail_dir(1),
            paths.attachments_dir(1),
            paths.draft_attachments_dir(),
            paths.files_cache_dir(1),
        ] {
            std::fs::create_dir_all(directory).unwrap();
        }
        std::fs::write(paths.mail_dir(1).join("message.eml"), [0; 11]).unwrap();
        std::fs::write(paths.attachments_dir(1).join("received.bin"), [0; 13]).unwrap();
        std::fs::write(paths.draft_attachments_dir().join("draft.bin"), [0; 17]).unwrap();
        std::fs::write(paths.files_cache_dir(1).join("offline.bin"), [0; 19]).unwrap();
        std::fs::write(paths.db_file(), [0; 23]).unwrap();
        std::fs::write(paths.data_dir.join("settings.json"), [0; 29]).unwrap();
        std::fs::write(paths.warm_start_file(), [0; 31]).unwrap();

        let generation = AtomicU64::new(7);
        let scan = scan_storage(&paths, &generation, 7).unwrap();
        assert_eq!(scan.bytes, [11, 30, 19, 23, 60]);
        assert_eq!(scan.unreadable_entries, 0);
        assert!(scan_storage(&paths, &generation, 6).is_none());
    }

    #[test]
    fn storage_scan_counts_overlapping_roots_once() {
        for nesting in 0..3 {
            let root = tempfile::tempdir().unwrap();
            let outer = root.path().join("outer");
            let inner = outer.join("inner");
            let paths = match nesting {
                0 => Paths::new(outer, inner),
                1 => Paths::new(inner, outer),
                _ => Paths::new(outer.clone(), outer),
            };
            std::fs::create_dir_all(paths.mail_dir(1)).unwrap();
            std::fs::create_dir_all(paths.attachments_dir(1)).unwrap();
            std::fs::write(paths.mail_dir(1).join("mail.eml"), [0; 11]).unwrap();
            std::fs::write(paths.attachments_dir(1).join("attachment"), [0; 13]).unwrap();
            std::fs::write(paths.db_file(), [0; 17]).unwrap();
            let scan = scan_storage(&paths, &AtomicU64::new(0), 0).unwrap();
            assert_eq!(scan.bytes, [11, 13, 0, 17, 0]);
        }
    }

    #[cfg(unix)]
    #[test]
    fn storage_scan_skips_symlinks_and_cycles() {
        let root = tempfile::tempdir().unwrap();
        let paths = Paths::for_tests(root.path());
        std::fs::create_dir_all(paths.mail_dir(1)).unwrap();
        let file = paths.mail_dir(1).join("mail.eml");
        std::fs::write(&file, [0; 11]).unwrap();
        std::os::unix::fs::symlink(&file, paths.data_dir.join("alias")).unwrap();
        std::os::unix::fs::symlink(&paths.data_dir, paths.data_dir.join("cycle")).unwrap();
        let scan = scan_storage(&paths, &AtomicU64::new(0), 0).unwrap();
        assert_eq!(scan.bytes, [11, 0, 0, 0, 0]);
        assert_eq!(scan.unreadable_entries, 0);
    }

    #[test]
    fn storage_database_category_requires_exact_database_or_sidecar() {
        for name in [
            "flectar-mail.db",
            "flectar-calendar.db-wal",
            "flectar-files.db-shm",
            "flectar-mail.db-journal",
        ] {
            assert_eq!(
                storage_category(StorageRoot::Data, name.as_ref()),
                STORAGE_DATABASES
            );
        }
        for name in [
            "flectar-mail.db.backup",
            "flectar-files.db-old",
            "other/flectar-mail.db",
        ] {
            assert_eq!(
                storage_category(StorageRoot::Data, name.as_ref()),
                STORAGE_OTHER
            );
        }
    }

    #[test]
    fn storage_rows_are_finite_for_empty_and_uneven_usage() {
        let empty = storage_rows(&StorageScan::default());
        assert!(
            empty
                .iter()
                .all(|row| row.fraction == 0.0 && row.offset == 0.0)
        );
        let scan = StorageScan {
            bytes: [1, 0, 999, 0, 0],
            unreadable_entries: 0,
        };
        let rows = storage_rows(&scan);
        assert!((rows[0].fraction - 0.001).abs() < 0.000001);
        assert!((rows[2].offset - 0.001).abs() < 0.000001);
        assert!((rows[4].offset - 1.0).abs() < 0.000001);
    }

    #[test]
    fn storage_size_uses_binary_units() {
        assert_eq!(format_storage_size(0), "0 B");
        assert_eq!(format_storage_size(1536), "1.5 KB");
        assert_eq!(format_storage_size(3 * 1024 * 1024), "3.0 MB");
        assert_eq!(format_storage_size(5 * 1024 * 1024 * 1024), "5.00 GB");
    }

    #[test]
    fn portable_profiles_distinguish_legacy_backups_from_explicit_unassignment() {
        let emails = HashSet::from(["alice@example.org".to_owned()]);
        let legacy = serde_json::json!({ "preferences": {} });
        assert!(portable_mail_profiles(&legacy, &emails).unwrap().is_none());

        let explicit_empty = serde_json::json!({
            "preferences": { "mailProfiles": [] }
        });
        assert_eq!(
            portable_mail_profiles(&explicit_empty, &emails)
                .unwrap()
                .unwrap()
                .len(),
            0
        );
    }

    #[test]
    fn portable_profiles_reject_cross_backup_and_duplicate_membership() {
        let emails = HashSet::from(["alice@example.org".to_owned(), "bob@example.org".to_owned()]);
        let duplicate = serde_json::json!({
            "preferences": { "mailProfiles": [
                { "name": "Work", "color": "#3B82F6", "accountEmails": ["alice@example.org"] },
                { "name": "Personal", "color": "#EA580C", "accountEmails": ["ALICE@example.org"] }
            ] }
        });
        assert!(portable_mail_profiles(&duplicate, &emails).is_err());

        let outside = serde_json::json!({
            "preferences": { "mailProfiles": [
                { "name": "Work", "color": "#3B82F6", "accountEmails": ["mallory@example.org"] }
            ] }
        });
        assert!(portable_mail_profiles(&outside, &emails).is_err());
    }

    #[test]
    fn portable_account_colors_are_optional_validated_and_normalized() {
        let emails = HashSet::from(["work@example.com".to_owned()]);
        let legacy = serde_json::json!({ "preferences": {} });
        assert!(portable_account_colors(&legacy, &emails).unwrap().is_none());

        let valid = serde_json::json!({
            "preferences": {
                "accountColors": { " Work@Example.com ": "#f97316" }
            }
        });
        let colors = portable_account_colors(&valid, &emails).unwrap().unwrap();
        assert_eq!(
            colors.get("work@example.com").map(String::as_str),
            Some("#f97316")
        );

        let invalid = serde_json::json!({
            "preferences": {
                "accountColors": { "work@example.com": "orange" }
            }
        });
        assert!(portable_account_colors(&invalid, &emails).is_err());

        let outside = serde_json::json!({
            "preferences": {
                "accountColors": { "other@example.com": "#000000" }
            }
        });
        assert!(portable_account_colors(&outside, &emails).is_err());
    }

    #[test]
    fn snapshot_archive_keeps_the_tree_and_skips_symlinks() {
        let staging = tempfile::tempdir().unwrap();
        let snapshot = staging.path().join("snapshot");
        std::fs::create_dir_all(snapshot.join("file_transfers")).unwrap();
        std::fs::write(snapshot.join("mail.sqlite3"), b"mail").unwrap();
        std::fs::write(snapshot.join("file_transfers/part"), b"part").unwrap();
        let outside = staging.path().join("outside");
        std::fs::write(&outside, b"private").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, snapshot.join("link")).unwrap();
        let archive = staging.path().join("snapshot.zip");

        archive_snapshot_directory(&snapshot, &archive, u64::MAX).unwrap();

        let mut zip = zip::ZipArchive::new(std::fs::File::open(&archive).unwrap()).unwrap();
        let mut names = zip
            .file_names()
            .map(|name| name.unwrap().into_owned())
            .collect::<Vec<_>>();
        names.sort();
        assert_eq!(
            names,
            ["file_transfers/", "file_transfers/part", "mail.sqlite3"]
        );
        let mut contents = String::new();
        std::io::Read::read_to_string(&mut zip.by_name("mail.sqlite3").unwrap(), &mut contents)
            .unwrap();
        assert_eq!(contents, "mail");
    }

    #[test]
    fn snapshot_archive_refuses_stores_over_the_transfer_limit() {
        let staging = tempfile::tempdir().unwrap();
        let snapshot = staging.path().join("snapshot");
        std::fs::create_dir_all(&snapshot).unwrap();
        std::fs::write(snapshot.join("mail.sqlite3"), [0; 64]).unwrap();
        let archive = staging.path().join("snapshot.zip");

        let error = archive_snapshot_directory(&snapshot, &archive, 63).unwrap_err();

        assert!(error.contains("64 B"), "{error}");
        assert!(!archive.exists());
        // Headers alone push an archive of a store at the limit past it.
        assert!(archive_snapshot_directory(&snapshot, &archive, 64).is_err());
        archive_snapshot_directory(&snapshot, &archive, 4096).unwrap();
    }
}
