//! The "Default email app" row in Settings: whether `mailto:` links from
//! browsers and other apps open in Flectar Mail, and a way to choose it.
//!
//! No platform lets an application take email links over by itself, and this
//! only acts when the user asks:
//!
//! - Windows lets only the user choose, in Settings > Apps > Default apps.
//!   The installer and the Store package register Flectar Mail there; a
//!   portable copy registers itself for the current user when asked, under
//!   names of its own, and can remove that again. Choosing it opens Flectar
//!   Mail's page of Default apps (Windows 11) or Default apps (Windows 10).
//! - macOS asks the user to confirm the change.
//! - Linux desktops keep the choice in the user's `mimeapps.list`, which this
//!   writes as GNOME Settings and KDE System Settings do. A Flatpak cannot
//!   change it, as no portal offers that, so the row points to the system
//!   settings instead, as Flathub's Thunderbird does.

use crate::{
    AppWindow, DefaultEmailApp,
    ui_message::{AppWindowMessages, UiMessage},
};
use slint::ComponentHandle;
use std::{
    io,
    sync::{Arc, Mutex, PoisonError},
};

#[cfg(target_os = "macos")]
#[path = "default_email_app/macos.rs"]
mod platform;
#[cfg(target_os = "windows")]
#[path = "default_email_app/windows.rs"]
mod platform;
#[cfg(all(linux_desktop, not(feature = "flatpak")))]
#[path = "default_email_app/xdg.rs"]
mod platform;
#[cfg(all(linux_desktop, feature = "flatpak"))]
mod platform {
    //! The sandbox sees neither the host's choice nor a way to change it.
    use super::{Applied, Done, State, Status};
    use std::io;

    pub(super) fn supported() -> bool {
        true
    }

    pub(super) fn state() -> io::Result<State> {
        Ok(State {
            status: Status::SystemSettings,
            removable: false,
        })
    }

    pub(super) fn make_default(done: Done) {
        done(Ok(Applied::Unchanged));
    }

    pub(super) fn remove() -> io::Result<()> {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Status {
    // Each platform reports only some of these.
    /// Mail links open in Flectar Mail.
    #[cfg_attr(all(linux_desktop, feature = "flatpak"), allow(dead_code))]
    Default,
    /// Another app or none opens them; the user can choose Flectar Mail.
    #[cfg_attr(all(linux_desktop, feature = "flatpak"), allow(dead_code))]
    NotDefault,
    /// Only the system settings can tell and change it (Flatpak).
    #[cfg_attr(not(all(linux_desktop, feature = "flatpak")), allow(dead_code))]
    SystemSettings,
    /// This copy is not in the applications menu, so the desktop cannot
    /// start it for a link (an AppImage that was not added there).
    #[cfg_attr(not(all(linux_desktop, not(feature = "flatpak"))), allow(dead_code))]
    NotInstalled,
}

impl Status {
    /// The value of `DefaultEmailApp.status` in Slint.
    fn key(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::NotDefault => "not-default",
            Self::SystemSettings => "system-settings",
            Self::NotInstalled => "not-installed",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct State {
    status: Status,
    /// A portable Windows copy registered itself and can take that back.
    removable: bool,
}

/// What choosing Flectar Mail did.
#[derive(Debug, PartialEq, Eq)]
enum Applied {
    /// It is the default now.
    #[cfg_attr(
        not(any(target_os = "macos", all(linux_desktop, not(feature = "flatpak")))),
        allow(dead_code)
    )]
    Done,
    /// The system settings opened for the user to finish there.
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    Settings,
    /// Nothing changed: the user kept the current app.
    #[cfg_attr(
        not(any(target_os = "macos", all(linux_desktop, feature = "flatpak"))),
        allow(dead_code)
    )]
    Unchanged,
}

/// Runs once the change is applied, on whichever thread finished it.
type Done = Box<dyn FnOnce(io::Result<Applied>) + Send>;

/// Reading and changing the default touches the registry or files, so it
/// runs on the blocking pool, one request at a time.
#[derive(Clone)]
pub(crate) struct DefaultEmailAppRow {
    runtime: tokio::runtime::Handle,
    lock: Arc<Mutex<()>>,
}

impl DefaultEmailAppRow {
    /// Show the row where the platform supports it, with its current state.
    pub(crate) fn register(app: &AppWindow, runtime: &tokio::runtime::Runtime) -> Option<Self> {
        if !platform::supported() {
            return None;
        }
        let row = Self {
            runtime: runtime.handle().clone(),
            lock: Arc::default(),
        };
        let ui = app.global::<DefaultEmailApp>();
        let this = row.clone();
        let weak = app.as_weak();
        ui.on_make_default(move || this.make_default(weak.clone()));
        let this = row.clone();
        let weak = app.as_weak();
        ui.on_remove(move || this.remove(weak.clone()));
        row.refresh(app);
        Some(row)
    }

    /// Read the current default again: it also changes in system settings.
    pub(crate) fn refresh(&self, app: &AppWindow) {
        let app = app.as_weak();
        let lock = Arc::clone(&self.lock);
        self.runtime.spawn_blocking(move || {
            let _guard = lock.lock().unwrap_or_else(PoisonError::into_inner);
            show(&app, platform::state());
        });
    }

    fn make_default(&self, app: slint::Weak<AppWindow>) {
        let Some(window) = app.upgrade() else {
            return;
        };
        window.global::<DefaultEmailApp>().set_busy(true);
        let lock = Arc::clone(&self.lock);
        self.runtime.spawn_blocking(move || {
            let _guard = lock.lock().unwrap_or_else(PoisonError::into_inner);
            let finished = app.clone();
            platform::make_default(Box::new(move |result| {
                let state = platform::state();
                let message = match result {
                    Ok(Applied::Done) => Some(UiMessage::plain(
                        "Flectar Mail is now your default email app.",
                    )),
                    Ok(Applied::Settings) => Some(UiMessage::plain(
                        "Choose Flectar Mail for MAILTO in Windows Settings to finish.",
                    )),
                    Ok(Applied::Unchanged) => None,
                    Err(error) => Some(UiMessage::detail(
                        "Could not change the default email app: {}",
                        error,
                    )),
                };
                show(&finished, state);
                let _ = finished.upgrade_in_event_loop(move |app| {
                    app.global::<DefaultEmailApp>().set_busy(false);
                    if let Some(message) = message {
                        app.set_sync_status(message);
                    }
                });
            }));
        });
    }

    fn remove(&self, app: slint::Weak<AppWindow>) {
        let lock = Arc::clone(&self.lock);
        self.runtime.spawn_blocking(move || {
            let _guard = lock.lock().unwrap_or_else(PoisonError::into_inner);
            let result = platform::remove();
            show(&app, platform::state());
            let _ = app.upgrade_in_event_loop(move |app| {
                app.set_sync_status(match result {
                    Ok(()) => {
                        UiMessage::plain("Windows no longer offers this copy for email links.")
                    }
                    Err(error) => {
                        UiMessage::detail("Could not change the default email app: {}", error)
                    }
                });
            });
        });
    }
}

fn show(app: &slint::Weak<AppWindow>, state: io::Result<State>) {
    match state {
        Ok(state) => {
            let _ = app.upgrade_in_event_loop(move |app| {
                let ui = app.global::<DefaultEmailApp>();
                ui.set_status(state.status.key().into());
                ui.set_removable(state.removable);
            });
        }
        Err(error) => eprintln!("could not read the default email app: {error}"),
    }
}
