//! Account configuration transfer, verified database export, and local-data
//! reset callback wiring.
//!
//! These operations share the same sensitive-data boundary and platform file
//! picker constraints. Keeping them out of the composition root makes the
//! credential-exclusion and destructive-reset behavior reviewable together.

use super::*;
use chrono::Utc;
use flectar_mail_core::models::{PortableAccountConfig, Settings};

const MAX_ACCOUNT_BACKUP_BYTES: u64 = 1024 * 1024;

fn read_account_backup(path: &std::path::Path) -> Result<Vec<u8>, String> {
    use std::io::Read;

    let file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    let mut bytes = Vec::with_capacity(MAX_ACCOUNT_BACKUP_BYTES as usize);
    file.take(MAX_ACCOUNT_BACKUP_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_ACCOUNT_BACKUP_BYTES {
        return Err("backup is larger than the 1 MB safety limit".into());
    }
    Ok(bytes)
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn pick_backup_export_path(title: String, filter: String) -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title(title)
        .set_file_name(format!(
            "flectar-mail-backup-{}.json",
            Local::now().format("%Y-%m-%d")
        ))
        .add_filter(filter, &["json"])
        .save_file()
}

#[cfg(any(target_os = "android", target_os = "ios"))]
fn pick_backup_export_path(_title: String, _filter: String) -> Option<PathBuf> {
    None
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn pick_backup_import_path(title: String, filter: String) -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title(title)
        .add_filter(filter, &["json"])
        .pick_file()
}

#[cfg(any(target_os = "android", target_os = "ios"))]
fn pick_backup_import_path(_title: String, _filter: String) -> Option<PathBuf> {
    None
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
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

#[cfg(any(target_os = "android", target_os = "ios"))]
fn pick_database_snapshot_path(_title: String) -> Option<PathBuf> {
    None
}

#[allow(clippy::too_many_arguments)]
pub(super) fn register_data_management_callbacks(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &Rc<tokio::runtime::Runtime>,
    ui_task_tx: &UiSender<UiTaskUpdate>,
    contact_state: &Rc<RefCell<ContactDirectoryState>>,
    contacts_loaded: &Rc<Cell<bool>>,
    contacts_loading: &Rc<Cell<bool>>,
    contact_load_generation: &Rc<Cell<u64>>,
    calendar_state: &Rc<RefCell<LocalCalendarState>>,
) {
    let app_weak = app.as_weak();
    let state_for_backup = Rc::clone(state);
    let runtime_for_backup = Rc::clone(runtime);
    app.on_export_account_backup(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
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
        let Some(path) = pick_backup_export_path(
            translated(
                &app,
                &UiMessage::plain("Export Flectar Mail account backup"),
            )
            .to_string(),
            translated(&app, &UiMessage::plain("Flectar Mail backup")).to_string(),
        ) else {
            #[cfg(any(target_os = "android", target_os = "ios"))]
            app.set_sync_status(UiMessage::plain(
                "The platform host does not provide document export.",
            ));
            return;
        };

        let preferences = core
            .as_ref()
            .and_then(|core| runtime_for_backup.block_on(core.load_settings()).ok())
            .map(|settings| {
                serde_json::json!({
                    "theme": settings.theme,
                    "showAvatars": settings.show_avatars,
                    "workspaceLayout": settings.workspace_layout,
                    "monochromeSidebarIcons": settings.monochrome_sidebar_icons,
                    "language": settings.language,
                    "calendarWeekStart": settings.calendar_week_start,
                    "loadRemoteImages": settings.load_remote_images,
                    "markReadOnOpen": settings.mark_read_on_open,
                    "notificationsEnabled": settings.notifications_enabled,
                    "notificationScope": settings.notification_scope,
                    "soundEnabled": settings.sound_enabled,
                    "syncIntervalMinutes": settings.sync_interval_minutes,
                    "closeToTray": settings.close_to_tray
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
        match serde_json::to_vec_pretty(&backup)
            .map_err(|error| error.to_string())
            .and_then(|bytes| std::fs::write(&path, bytes).map_err(|error| error.to_string()))
        {
            Ok(()) => app.set_sync_status(UiMessage::plain(
                "Backup exported. Passwords and OAuth tokens were not included.",
            )),
            Err(error) => {
                app.set_sync_status(UiMessage::detail("Could not export backup: {}", error))
            }
        }
    });

    let app_weak = app.as_weak();
    let state_for_database_snapshot = Rc::clone(state);
    let runtime_for_database_snapshot = Rc::clone(runtime);
    let updates_for_database_snapshot = ui_task_tx.clone();
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
        let Some(path) = pick_database_snapshot_path(
            translated(
                &app,
                &UiMessage::plain("Choose where to export the database snapshot"),
            )
            .to_string(),
        ) else {
            #[cfg(any(target_os = "android", target_os = "ios"))]
            app.set_sync_status(UiMessage::plain(
                "The platform host does not provide document export.",
            ));
            return;
        };

        app.set_sync_status(UiMessage::plain("Creating verified database snapshot…"));
        let updates = updates_for_database_snapshot.clone();
        runtime_for_database_snapshot.spawn(async move {
            let display_path = path.display().to_string();
            let message = match core.create_database_snapshot(path).await {
                Ok(_) => UiMessage::detail("Database snapshot exported to {}.", display_path),
                Err(error) => UiMessage::detail("Could not export database snapshot: {}", error),
            };
            let _ = updates
                .send(UiTaskUpdate {
                    message,
                    accounts: None,
                    calendar_connections: None,
                    calendar_error: None,
                    account_removal: None,
                    clear_account_form: false,
                    finishes_account_setup: false,
                    finishes_oauth: false,
                    close_to_tray: None,
                })
                .await;
        });
    });

    let app_weak = app.as_weak();
    let state_for_backup_import = Rc::clone(state);
    let runtime_for_backup_import = Rc::clone(runtime);
    app.on_import_account_backup(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let Some(core) = state_for_backup_import.borrow().core.clone() else {
            app.set_sync_status(UiMessage::plain(
                "Local data storage is unavailable. Retry startup.",
            ));
            return;
        };
        let Some(path) = pick_backup_import_path(
            translated(
                &app,
                &UiMessage::plain("Import Flectar Mail account backup"),
            )
            .to_string(),
            translated(&app, &UiMessage::plain("Flectar Mail backup")).to_string(),
        ) else {
            #[cfg(any(target_os = "android", target_os = "ios"))]
            app.set_sync_status(UiMessage::plain(
                "The platform host does not provide document import.",
            ));
            return;
        };

        let result = (|| -> Result<(usize, Settings, Vec<Account>, Vec<AccountConfig>), String> {
            let bytes = read_account_backup(&path)?;
            let backup: serde_json::Value =
                serde_json::from_slice(&bytes).map_err(|error| format!("invalid JSON: {error}"))?;
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

            let mut settings = runtime_for_backup_import.block_on(core.load_settings())?;
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
                if let Some(layout) = preferences
                    .get("workspaceLayout")
                    .or_else(|| preferences.get("mailLayout"))
                    .and_then(|value| value.as_str())
                    && matches!(layout, "default" | "minimal")
                {
                    settings.workspace_layout = layout.to_owned();
                }
                if let Some(language) = preferences
                    .get("language")
                    .and_then(|value| value.as_str())
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
            }

            let imported = runtime_for_backup_import
                .block_on(core.import_account_configs(configs))?;
            runtime_for_backup_import.block_on(core.save_settings(settings.clone()))?;
            let accounts = runtime_for_backup_import.block_on(core.load_accounts())?;
            let configs = runtime_for_backup_import.block_on(core.load_account_configs())?;
            Ok((imported, settings, accounts, configs))
        })();

        match result {
            Ok((imported, settings, accounts, configs)) => {
                let total = accounts.len();
                apply_language(&app, &settings.language);
                update_connected_accounts(
                    &app,
                    &state_for_backup_import,
                    &runtime_for_backup_import,
                    accounts,
                    configs,
                );
                app.set_theme_mode(
                    match settings.theme.as_str() {
                        "carbon" | "dark" => "dark",
                        "snow" | "light" => "light",
                        _ => "system",
                    }
                    .into(),
                );
                app.set_monochrome_sidebar_icons(settings.monochrome_sidebar_icons);
                app.set_show_avatars(settings.show_avatars);
                app.set_workspace_layout(settings.workspace_layout.clone().into());
                app.set_notifications_enabled(settings.notifications_enabled);
                app.set_notification_sound_enabled(settings.sound_enabled);
                app.set_notification_scope(settings.notification_scope.clone().into());
                app.set_sync_interval_minutes(settings.sync_interval_minutes as i32);
                app.set_mark_read_on_open(settings.mark_read_on_open);
                state_for_backup_import.borrow_mut().mark_read_on_open =
                    settings.mark_read_on_open;
                app.set_remote_images_enabled(
                    settings.load_remote_images && cfg!(feature = "remote-content"),
                );
                #[cfg(not(any(target_os = "android", target_os = "ios", feature = "flatpak")))]
                app.set_close_to_tray(settings.close_to_tray);
                #[cfg(any(target_os = "android", target_os = "ios", feature = "flatpak"))]
                app.set_close_to_tray(false);
                app.set_sync_status(if imported == 0 {
                    UiMessage::detail(
                        "Backup imported. All {} account configurations already existed.",
                        total,
                    )
                } else {
                    UiMessage::detail(
                        "Imported {} account configurations. Open Accounts and sign in to each one.",
                        imported,
                    )
                });
            }
            Err(error) => {
                app.set_sync_status(UiMessage::detail("Could not import backup: {}", error))
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
                        state.calendar_connections.clear();
                        state.messages.clear();
                        state.mailboxes.clear();
                        state.unified_mailboxes.clear();
                        state.total_count = 0;
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
                        contacts.matching_count = 0;
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
                    app.set_monochrome_sidebar_icons(false);
                    app.set_show_avatars(true);
                    app.set_workspace_layout("default".into());
                    app.set_notifications_enabled(true);
                    app.set_notification_sound_enabled(true);
                    app.set_notification_scope("important".into());
                    app.set_sync_interval_minutes(5);
                    app.set_mark_read_on_open(true);
                    app.set_remote_images_enabled(false);
                    app.set_close_to_tray(false);
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
