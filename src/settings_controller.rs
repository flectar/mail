//! Settings preference callback wiring.
//!
//! These callbacks share one lifecycle and persistence boundary: update the
//! session immediately where needed, then write through the core when it is
//! available. Keeping them together prevents the application composition root
//! from accumulating one closure per setting.

use super::*;
use flectar_mail_core::models::CustomTheme;

pub(super) fn register_settings_preference_callbacks(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &Rc<tokio::runtime::Runtime>,
) {
    let app_weak = app.as_weak();
    let state_for_notifications = Rc::clone(state);
    let runtime_for_notifications = Rc::clone(runtime);
    app.on_save_notification_settings(move |enabled, sound, scope| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let Some(core) = state_for_notifications.borrow().core.clone() else {
            app.set_sync_status(UiMessage::plain(
                "Notification preferences are active for this session.",
            ));
            return;
        };
        match runtime_for_notifications.block_on(core.set_notification_settings(
            enabled,
            sound,
            scope.as_str(),
        )) {
            Ok(()) => app.set_sync_status(UiMessage::plain("Notification preferences saved.")),
            Err(error) => {
                app.set_sync_status(UiMessage::detail("Could not save notifications: {}", error))
            }
        }
    });

    let app_weak = app.as_weak();
    let state_for_sync_interval = Rc::clone(state);
    let runtime_for_sync_interval = Rc::clone(runtime);
    app.on_save_sync_interval(move |minutes| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let Some(core) = state_for_sync_interval.borrow().core.clone() else {
            app.set_sync_status(UiMessage::plain(
                "Sync interval is active for this session.",
            ));
            return;
        };
        match runtime_for_sync_interval.block_on(core.set_sync_interval_minutes(i64::from(minutes)))
        {
            Ok(()) => app.set_sync_status(UiMessage::detail(
                "Sync interval set to {} minutes.",
                minutes,
            )),
            Err(error) => {
                app.set_sync_status(UiMessage::detail("Could not save sync interval: {}", error))
            }
        }
    });

    let app_weak = app.as_weak();
    let state_for_read_on_open = Rc::clone(state);
    let runtime_for_read_on_open = Rc::clone(runtime);
    app.on_save_mark_read_on_open(move |enabled| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        state_for_read_on_open.borrow_mut().mark_read_on_open = enabled;
        let Some(core) = state_for_read_on_open.borrow().core.clone() else {
            app.set_sync_status(UiMessage::plain(
                "Message-opening preference updated for this session.",
            ));
            return;
        };
        match runtime_for_read_on_open.block_on(core.set_mark_read_on_open(enabled)) {
            Ok(()) => app.set_sync_status(UiMessage::plain("Message-opening preference saved.")),
            Err(error) => app.set_sync_status(UiMessage::detail(
                "Could not save message-opening preference: {}",
                error,
            )),
        }
    });

    let app_weak = app.as_weak();
    let state_for_theme = Rc::clone(state);
    let runtime_for_theme = Rc::clone(runtime);
    app.on_save_theme(move |theme| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let Some(core) = state_for_theme.borrow().core.clone() else {
            app.set_sync_status(UiMessage::plain("Theme updated for this session."));
            return;
        };
        match runtime_for_theme.block_on(core.set_theme(&theme)) {
            Ok(()) => app.set_sync_status(UiMessage::plain("Theme preference saved.")),
            Err(error) => app.set_sync_status(UiMessage::detail("Could not save theme: {}", error)),
        }
    });

    let app_weak = app.as_weak();
    let state_for_theme_preset = Rc::clone(state);
    let runtime_for_theme_preset = Rc::clone(runtime);
    app.on_save_theme_preset(move |preset| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let Some(core) = state_for_theme_preset.borrow().core.clone() else {
            app.set_sync_status(UiMessage::plain("Theme palette updated for this session."));
            return;
        };
        match runtime_for_theme_preset.block_on(core.set_theme_preset(preset.as_str())) {
            Ok(()) => app.set_sync_status(UiMessage::plain("Theme palette saved.")),
            Err(error) => {
                app.set_sync_status(UiMessage::detail("Could not save theme palette: {}", error))
            }
        }
    });

    let app_weak = app.as_weak();
    let state_for_custom_theme = Rc::clone(state);
    let runtime_for_custom_theme = Rc::clone(runtime);
    app.on_save_custom_theme(
        move |light_primary,
              light_page,
              light_surface,
              light_text,
              light_border,
              dark_primary,
              dark_page,
              dark_surface,
              dark_text,
              dark_border| {
            let Some(app) = app_weak.upgrade() else {
                return false;
            };
            let custom_theme = CustomTheme {
                light_primary: theme::color_to_hex(light_primary),
                light_page_background: theme::color_to_hex(light_page),
                light_surface: theme::color_to_hex(light_surface),
                light_text: theme::color_to_hex(light_text),
                light_border: theme::color_to_hex(light_border),
                dark_primary: theme::color_to_hex(dark_primary),
                dark_page_background: theme::color_to_hex(dark_page),
                dark_surface: theme::color_to_hex(dark_surface),
                dark_text: theme::color_to_hex(dark_text),
                dark_border: theme::color_to_hex(dark_border),
            };
            let Some(core) = state_for_custom_theme.borrow().core.clone() else {
                app.set_sync_status(UiMessage::plain("Custom theme updated for this session."));
                return false;
            };
            match runtime_for_custom_theme.block_on(core.set_custom_theme(custom_theme)) {
                Ok(()) => {
                    app.set_sync_status(UiMessage::plain("Custom theme saved."));
                    true
                }
                Err(error) => {
                    app.set_sync_status(UiMessage::detail(
                        "Could not save custom theme: {}",
                        error,
                    ));
                    false
                }
            }
        },
    );

    let app_weak = app.as_weak();
    let state_for_language = Rc::clone(state);
    let runtime_for_language = Rc::clone(runtime);
    app.on_save_language(move |language| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        apply_language(&app, language.as_str());
        let Some(core) = state_for_language.borrow().core.clone() else {
            app.set_sync_status(UiMessage::plain("Language updated for this session."));
            return;
        };
        match runtime_for_language.block_on(core.set_language(language.as_str())) {
            Ok(()) => app.set_sync_status(UiMessage::plain("Language preference saved.")),
            Err(error) => {
                app.set_sync_status(UiMessage::detail("Could not save language: {}", error))
            }
        }
    });

    let app_weak = app.as_weak();
    let state_for_sidebar_icons = Rc::clone(state);
    let runtime_for_sidebar_icons = Rc::clone(runtime);
    app.on_save_sidebar_icon_style(move |monochrome| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let Some(core) = state_for_sidebar_icons.borrow().core.clone() else {
            app.set_sync_status(UiMessage::plain(
                "Sidebar icon style updated for this session.",
            ));
            return;
        };
        match runtime_for_sidebar_icons.block_on(core.set_monochrome_sidebar_icons(monochrome)) {
            Ok(()) => app.set_sync_status(UiMessage::plain("Sidebar icon preference saved.")),
            Err(error) => app.set_sync_status(UiMessage::detail(
                "Could not save sidebar icon preference: {}",
                error,
            )),
        }
    });

    let app_weak = app.as_weak();
    let state_for_avatars = Rc::clone(state);
    let runtime_for_avatars = Rc::clone(runtime);
    app.on_save_avatar_visibility(move |enabled| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let Some(core) = state_for_avatars.borrow().core.clone() else {
            app.set_sync_status(UiMessage::plain(
                "Avatar preference updated for this session.",
            ));
            return;
        };
        match runtime_for_avatars.block_on(core.set_show_avatars(enabled)) {
            Ok(()) => app.set_sync_status(UiMessage::plain("Avatar preference saved.")),
            Err(error) => app.set_sync_status(UiMessage::detail(
                "Could not save avatar preference: {}",
                error,
            )),
        }
    });

    let app_weak = app.as_weak();
    let state_for_workspace_layout = Rc::clone(state);
    let runtime_for_workspace_layout = Rc::clone(runtime);
    app.on_save_workspace_layout(move |layout| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let Some(core) = state_for_workspace_layout.borrow().core.clone() else {
            app.set_sync_status(UiMessage::plain(
                "Workspace layout updated for this session.",
            ));
            return;
        };
        match runtime_for_workspace_layout.block_on(core.set_workspace_layout(layout.as_str())) {
            Ok(()) => app.set_sync_status(UiMessage::plain("Workspace layout preference saved.")),
            Err(error) => app.set_sync_status(UiMessage::detail(
                "Could not save workspace layout: {}",
                error,
            )),
        }
    });
}

/// Both entry points share one asynchronous persistence path and busy state.
pub(super) fn register_oauth_settings_callbacks(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &Rc<tokio::runtime::Runtime>,
) {
    let weak = app.as_weak();
    let single_state = Rc::clone(state);
    let single_runtime = Rc::clone(runtime);
    app.on_save_oauth_app(move |provider, client_id, client_secret| {
        let Some(app) = weak.upgrade() else { return };
        let (google, microsoft) = match provider.as_str() {
            "gmail" => (
                Some((client_id.to_string(), client_secret.to_string())),
                None,
            ),
            "microsoft" => (None, Some(client_id.to_string())),
            _ => return,
        };
        save_oauth_settings(
            &app,
            &single_state,
            &single_runtime,
            google,
            microsoft,
            false,
        );
    });

    let weak = app.as_weak();
    let state = Rc::clone(state);
    let runtime = Rc::clone(runtime);
    app.on_save_oauth_apps(move |google_id, google_secret, microsoft_id| {
        let Some(app) = weak.upgrade() else { return };
        save_oauth_settings(
            &app,
            &state,
            &runtime,
            Some((google_id.to_string(), google_secret.to_string())),
            Some(microsoft_id.to_string()),
            true,
        );
    });
}

fn save_oauth_settings(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Runtime,
    google: Option<(String, String)>,
    microsoft: Option<String>,
    close_dialog: bool,
) {
    if app.get_oauth_settings_saving() || app.get_oauth_in_progress() {
        return;
    }
    let Some(core) = state.borrow().core.clone() else {
        let message = UiMessage::plain("Local mail data is unavailable. Retry startup.");
        app.set_sync_status(message.clone());
        app.set_oauth_settings_error(ui_message::translated(app, &message));
        return;
    };
    let update_google = google.is_some();
    let update_microsoft = microsoft.is_some();
    app.set_oauth_settings_error("".into());
    app.set_oauth_settings_saving(true);
    let weak = app.as_weak();
    runtime.spawn(async move {
        let result = core.set_oauth_apps(google, microsoft).await;
        let _ = weak.upgrade_in_event_loop(move |app| {
            app.set_oauth_settings_saving(false);
            match result {
                Ok(settings) => {
                    if update_google {
                        app.set_google_client_id(settings.google_client_id.clone().into());
                        app.set_google_client_secret(settings.google_client_secret.clone().into());
                    }
                    if update_microsoft {
                        app.set_ms_client_id(settings.ms_client_id.clone().into());
                    }
                    app.set_custom_oauth_configured(
                        !settings.google_client_id.is_empty() || !settings.ms_client_id.is_empty(),
                    );
                    startup::refresh_oauth_availability(&app);
                    app.set_sync_status(UiMessage::plain("OAuth app keys saved."));
                    if close_dialog {
                        app.set_oauth_setup_open(false);
                    }
                }
                Err(error) => {
                    let message = UiMessage::detail("OAuth settings failed: {}", error);
                    app.set_sync_status(message.clone());
                    app.set_oauth_settings_error(ui_message::translated(&app, &message));
                }
            }
        });
    });
}
