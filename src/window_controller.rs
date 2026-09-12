//! Native window and system-tray lifecycle wiring.
//!
//! Window close behavior, tray actions, and persistence of the close-to-tray
//! preference form one lifecycle boundary. Keeping that boundary here makes
//! the application composition root independent of platform window policy.

use super::*;
use crate::tray_ui::FlectarTray;

pub(super) fn create_and_register_window_lifecycle(
    app: &AppWindow,
) -> Result<Option<FlectarTray>, slint::PlatformError> {
    #[cfg(not(any(target_os = "android", target_os = "ios", feature = "flatpak")))]
    let tray = Some(FlectarTray::new()?);
    #[cfg(any(target_os = "android", target_os = "ios", feature = "flatpak"))]
    let tray: Option<FlectarTray> = None;

    #[cfg(any(target_os = "android", target_os = "ios", feature = "flatpak"))]
    app.set_close_to_tray(false);

    if let Some(tray) = tray.as_ref() {
        tray.set_enabled(app.get_close_to_tray());

        let app_for_tray = app.as_weak();
        tray.on_show_window(move || {
            if let Some(app) = app_for_tray.upgrade() {
                let _ = app.show();
            }
        });
        let app_for_tray_quit = app.as_weak();
        tray.on_quit_app(move || {
            if let Some(app) = app_for_tray_quit.upgrade() {
                let _ = app.hide();
            }
            let _ = slint::quit_event_loop();
        });
    }

    let app_for_close = app.as_weak();
    app.window().on_close_requested(move || {
        let Some(app) = app_for_close.upgrade() else {
            return slint::CloseRequestResponse::HideWindow;
        };
        if app.invoke_system_back_requested() {
            return slint::CloseRequestResponse::KeepWindowShown;
        }
        if app.get_close_to_tray() {
            slint::CloseRequestResponse::HideWindow
        } else {
            let _ = slint::quit_event_loop();
            slint::CloseRequestResponse::HideWindow
        }
    });

    Ok(tray)
}

pub(super) fn register_window_preference_callbacks(
    app: &AppWindow,
    tray: Option<&FlectarTray>,
    state: &Rc<RefCell<InboxState>>,
    runtime: &Rc<tokio::runtime::Runtime>,
    ui_task_tx: &UiSender<UiTaskUpdate>,
) {
    let app_weak = app.as_weak();
    app.on_toggle_settings(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        app.set_settings_open(!app.get_settings_open());
    });

    let app_weak = app.as_weak();
    let tray_for_close_setting = tray.map(FlectarTray::as_weak);
    let state_for_close_setting = Rc::clone(state);
    let runtime_for_close_setting = Rc::clone(runtime);
    let ui_task_tx_for_close_setting = ui_task_tx.clone();
    app.on_set_close_to_tray(move |enabled| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        if cfg!(any(target_os = "android", target_os = "ios", feature = "flatpak")) {
            app.set_close_to_tray(false);
            app.set_sync_status(UiMessage::plain(
                "System tray behavior is unavailable in this package.",
            ));
            return;
        }
        app.set_close_to_tray(enabled);
        if let Some(tray) = tray_for_close_setting
            .as_ref()
            .and_then(slint::Weak::upgrade)
        {
            tray.set_enabled(enabled);
        }

        let Some(core) = state_for_close_setting.borrow().core.clone() else {
            app.set_sync_status(UiMessage::plain(
                "Close-to-tray is active for this session.",
            ));
            return;
        };
        app.set_sync_status(UiMessage::plain("Saving close behavior…"));
        let updates = ui_task_tx_for_close_setting.clone();
        runtime_for_close_setting.spawn(async move {
            let update = match core.set_close_to_tray(enabled).await {
                Ok(()) => UiTaskUpdate {
                    message: if enabled {
                        UiMessage::plain(
                            "Closing the window will now keep Flectar Mail in the tray.",
                        )
                    } else {
                        UiMessage::plain("Closing the window will now quit Flectar Mail.")
                    },
                    accounts: None,
                    calendar_connections: None,
                    calendar_error: None,
                    account_removal: None,
                    clear_account_form: false,
                    finishes_oauth: false,
                    finishes_account_setup: false,
                    close_to_tray: None,
                },
                Err(error) => UiTaskUpdate {
                    message: UiMessage::detail("Could not save close behavior: {}", error),
                    accounts: None,
                    calendar_connections: None,
                    calendar_error: None,
                    account_removal: None,
                    clear_account_form: false,
                    finishes_oauth: false,
                    finishes_account_setup: false,
                    // Restore both the setting and tray visibility if the
                    // preference could not be persisted.
                    close_to_tray: Some(!enabled),
                },
            };
            let _ = updates.send(update).await;
        });
    });
}
