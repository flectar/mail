//! Native window and system-tray lifecycle wiring.
//!
//! Window close behavior, tray actions, background resource release, and
//! persistence of the close-to-tray, sign-in and window-frame preferences form
//! one lifecycle boundary. Keeping that boundary here makes the application
//! composition root independent of platform window policy.

use super::*;
use crate::tray_ui::FlectarTray;

#[cfg(target_os = "macos")]
mod macos;

/// How long the window must stay minimized before its rendering buffers are
/// released. Shorter interruptions keep the rendered message ready.
#[cfg(desktop)]
const MINIMIZED_RELEASE_DELAY: Duration = Duration::from_secs(10);
/// Let Slint destroy the native surface and process pending property changes
/// that may still own image references before returning memory.
const TRIM_DELAY: Duration = Duration::from_millis(250);
/// Coalesces the burst of startup updates applied while the window is hidden.
const HIDDEN_TRIM_DELAY: Duration = Duration::from_secs(1);

thread_local! {
    static LIFECYCLE: Lifecycle = Lifecycle::default();
}

#[derive(Default)]
struct Lifecycle {
    minimized_release: Timer,
    hidden_trim: Timer,
    /// The last minimized state reported by the platform.
    minimized: Cell<bool>,
    /// Rendering was suspended because the window stayed minimized.
    released_while_minimized: Cell<bool>,
    /// Deferred until the window is first shown after a background start.
    first_show: RefCell<Vec<Box<dyn FnOnce()>>>,
    /// Maximized and full-screen state when the window was hidden.
    hidden_state: Cell<(bool, bool)>,
}

pub(super) fn create_and_register_window_lifecycle(
    app: &AppWindow,
) -> Result<Option<FlectarTray>, slint::PlatformError> {
    #[cfg(not(any(mobile, feature = "flatpak")))]
    let tray = Some(FlectarTray::new()?);
    #[cfg(any(mobile, feature = "flatpak"))]
    let tray: Option<FlectarTray> = None;

    #[cfg(any(mobile, feature = "flatpak"))]
    app.set_close_to_tray(false);

    if let Some(tray) = tray.as_ref() {
        tray.set_enabled(app.get_close_to_tray());
        crate::app_icon::attach_tray(app, tray);

        let app_for_tray = app.as_weak();
        tray.on_show_window(move || {
            if let Some(app) = app_for_tray.upgrade() {
                activate(&app, None);
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

    // Opening the running app from the Dock or Finder shows its window.
    #[cfg(target_os = "macos")]
    {
        let app = app.as_weak();
        slint::Timer::single_shot(Duration::ZERO, move || {
            macos::on_reopen(move || {
                if let Some(app) = app.upgrade() {
                    activate(&app, None);
                }
            });
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
            prepare_for_tray(&app);
            slint::CloseRequestResponse::HideWindow
        } else {
            let _ = slint::quit_event_loop();
            slint::CloseRequestResponse::HideWindow
        }
    });

    Ok(tray)
}

pub(super) fn prepare_for_tray(app: &AppWindow) {
    let window = app.window();
    let state = (window.is_maximized(), window.is_fullscreen());
    LIFECYCLE.with(|lifecycle| {
        lifecycle.minimized_release.stop();
        lifecycle.released_while_minimized.set(false);
        lifecycle.hidden_state.set(state);
    });
    app.invoke_suspend_rendering(true);
    slint::Timer::single_shot(TRIM_DELAY, release_free_memory);
}

pub(super) fn show_from_tray(app: &AppWindow) {
    crate::window_chrome::before_show();
    if let Err(error) = app.show() {
        eprintln!("could not restore window: {error}");
        return;
    }
    let (maximized, fullscreen) = LIFECYCLE.with(|lifecycle| {
        lifecycle.minimized.set(false);
        lifecycle.hidden_state.take()
    });
    restore_window_state(app, maximized, fullscreen);
    app.invoke_suspend_rendering(false);
    app.window().request_redraw();
    for hook in LIFECYCLE.with(|lifecycle| lifecycle.first_show.take()) {
        hook();
    }
}

/// The recreated native window starts out normal, while Slint still records
/// the old state and so ignores a request to set it again. Apply it to the
/// native window directly; Slint follows the resulting resize.
fn restore_window_state(app: &AppWindow, maximized: bool, fullscreen: bool) {
    #[cfg(desktop)]
    if maximized || fullscreen {
        use slint::winit_030::{WinitWindowAccessor, winit::window::Fullscreen};
        let app = app.as_weak();
        // The native window is created again after `show` returns.
        let _ = slint::spawn_local(async move {
            let Some(app) = app.upgrade() else {
                return;
            };
            let Ok(window) = app.window().winit_window().await else {
                return;
            };
            if fullscreen {
                window.set_fullscreen(Some(Fullscreen::Borderless(None)));
            } else {
                window.set_maximized(true);
            }
        });
    }
    #[cfg(mobile)]
    let _ = (app, maximized, fullscreen);
}

/// Bring the window to the user from the tray or another launch: show it if
/// it is hidden, restore it if it is minimized, and ask for focus with the
/// launcher's activation token when one was forwarded.
pub(super) fn activate(app: &AppWindow, activation_token: Option<String>) {
    let window = app.window();
    if !window.is_visible() {
        show_from_tray(app);
    } else if window.is_minimized() {
        window.set_minimized(false);
    }
    #[cfg(desktop)]
    crate::window_chrome::focus(app, activation_token);
    #[cfg(mobile)]
    let _ = activation_token;
}

/// Return memory freed by startup work that ran while the window is hidden,
/// as after a start in the tray. Hiding the window trims on its own.
pub(super) fn trim_while_hidden(app: &AppWindow) {
    if app.window().is_visible() {
        return;
    }
    LIFECYCLE.with(|lifecycle| {
        lifecycle.hidden_trim.start(
            slint::TimerMode::SingleShot,
            HIDDEN_TRIM_DELAY,
            release_free_memory,
        );
    });
}

/// Run `hook` once the window is visible: now, or when a background start
/// first shows it.
pub(super) fn when_first_shown(app: &AppWindow, hook: impl FnOnce() + 'static) {
    if app.window().is_visible() {
        hook();
    } else {
        LIFECYCLE.with(|lifecycle| lifecycle.first_show.borrow_mut().push(Box::new(hook)));
    }
}

/// Follow platform minimize reports. A window that stays minimized releases
/// the same rendering buffers as one closed to the tray, and renders them
/// again as soon as it is restored. Platforms that cannot report minimization
/// (Wayland) keep everything rendered.
#[cfg(desktop)]
pub(crate) fn minimized_changed(app: &AppWindow, minimized: bool) {
    let changed = LIFECYCLE.with(|lifecycle| lifecycle.minimized.replace(minimized) != minimized);
    if !changed {
        return;
    }
    if minimized {
        let app = app.as_weak();
        LIFECYCLE.with(|lifecycle| {
            lifecycle.minimized_release.start(
                slint::TimerMode::SingleShot,
                MINIMIZED_RELEASE_DELAY,
                move || {
                    let Some(app) = app.upgrade() else {
                        return;
                    };
                    // Hiding to the tray already released everything.
                    if !LIFECYCLE.with(|lifecycle| lifecycle.minimized.get())
                        || !app.window().is_visible()
                        || app.get_render_suspended()
                    {
                        return;
                    }
                    LIFECYCLE.with(|lifecycle| lifecycle.released_while_minimized.set(true));
                    app.invoke_suspend_rendering(true);
                    slint::Timer::single_shot(TRIM_DELAY, release_free_memory);
                },
            );
        });
        return;
    }
    let resume = LIFECYCLE.with(|lifecycle| {
        lifecycle.minimized_release.stop();
        lifecycle.released_while_minimized.replace(false)
    });
    if resume {
        app.invoke_suspend_rendering(false);
        app.window().request_redraw();
    }
}

/// Whether the window was last reported minimized. Lets the platform event
/// filter skip minimize queries on ordinary frames and resizes.
#[cfg(desktop)]
pub(crate) fn reported_minimized() -> bool {
    LIFECYCLE.with(|lifecycle| lifecycle.minimized.get())
}

/// Whether a background start can rely on a tray icon to bring the window
/// back. Linux needs a StatusNotifier host, which some desktops (GNOME without
/// an extension) do not provide; Windows and macOS always have one.
#[cfg(desktop)]
pub(super) fn tray_host_available(runtime: &tokio::runtime::Runtime) -> bool {
    if cfg!(feature = "flatpak") {
        return false;
    }
    #[cfg(linux_desktop)]
    return runtime.block_on(status_notifier_host_registered());
    #[cfg(not(linux_desktop))]
    {
        let _ = runtime;
        true
    }
}

#[cfg(linux_desktop)]
async fn status_notifier_host_registered() -> bool {
    use ashpd::zbus;
    let query = async {
        let connection = zbus::Connection::session().await.ok()?;
        let watcher = zbus::Proxy::new(
            &connection,
            "org.kde.StatusNotifierWatcher",
            "/StatusNotifierWatcher",
            "org.kde.StatusNotifierWatcher",
        )
        .await
        .ok()?;
        watcher
            .get_property::<bool>("IsStatusNotifierHostRegistered")
            .await
            .ok()
    };
    matches!(
        tokio::time::timeout(Duration::from_secs(1), query).await,
        Ok(Some(true))
    )
}

/// Return already-free allocator pages to the operating system at an explicit
/// idle boundary. Working caches are never purged periodically.
fn release_free_memory() {
    #[cfg(glibc)]
    // SAFETY: malloc_trim only releases free heap pages; no pointers change.
    unsafe {
        libc::malloc_trim(0);
    }
    #[cfg(target_os = "macos")]
    {
        unsafe extern "C" {
            fn malloc_zone_pressure_relief(zone: *mut std::ffi::c_void, goal: usize) -> usize;
        }
        // SAFETY: a null zone asks every malloc zone to release free pages.
        unsafe {
            malloc_zone_pressure_relief(std::ptr::null_mut(), 0);
        }
    }
    #[cfg(target_os = "windows")]
    {
        use windows_sys::Win32::System::{
            Memory::{GetProcessHeap, HeapCompact, SetProcessWorkingSetSizeEx},
            Threading::GetCurrentProcess,
        };
        // SAFETY: both calls act on this process's own heap and working set.
        // Trimming moves idle pages to the standby list, which is what Task
        // Manager reports; pages touched later by sync fault back cheaply.
        unsafe {
            HeapCompact(GetProcessHeap(), 0);
            SetProcessWorkingSetSizeEx(GetCurrentProcess(), usize::MAX, usize::MAX, 0);
        }
    }
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
        if let Some(app) = app_weak.upgrade() {
            app.set_settings_open(!app.get_settings_open());
        }
    });
    // The entry can also change in the system's own startup settings, so
    // read it again whenever Settings appears: from the rail, a tab or a
    // shortcut into a settings page.
    #[cfg(desktop)]
    {
        let launch_at_login = LaunchAtLogin::register(app, runtime);
        let app_weak = app.as_weak();
        app.on_settings_shown(move || {
            if let Some(app) = app_weak.upgrade() {
                launch_at_login.refresh(&app, false);
            }
        });
    }

    let app_weak = app.as_weak();
    let state_for_frame = Rc::clone(state);
    let runtime_for_frame = Rc::clone(runtime);
    app.global::<WindowChrome>().on_choose(move |integrated| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let frame = if integrated {
            window_chrome::WindowFrame::Integrated
        } else {
            window_chrome::WindowFrame::System
        };
        window_chrome::apply_preference(&app, frame);
        let Some(core) = state_for_frame.borrow().core.clone() else {
            app.set_sync_status(UiMessage::plain(
                "Window frame updated for this session.",
            ));
            return;
        };
        match runtime_for_frame.block_on(core.set_window_frame(frame.key())) {
            Ok(()) => app.set_sync_status(UiMessage::plain("Window frame preference saved.")),
            Err(error) => app.set_sync_status(UiMessage::detail(
                "Could not save window frame preference: {}",
                error,
            )),
        }
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
        if cfg!(any(mobile, feature = "flatpak")) {
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
                    carddav_connections: None,
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
                    carddav_connections: None,
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

/// The "Start automatically" switch. The operating system's startup list is
/// read and written on the blocking pool; one lock and a request counter keep
/// overlapping toggles and refreshes from applying out of order.
#[cfg(desktop)]
#[derive(Clone)]
struct LaunchAtLogin {
    supported: bool,
    runtime: tokio::runtime::Handle,
    lock: Arc<Mutex<()>>,
    requests: Arc<std::sync::atomic::AtomicU64>,
}

#[cfg(desktop)]
impl LaunchAtLogin {
    fn register(app: &AppWindow, runtime: &tokio::runtime::Runtime) -> Self {
        let this = Self {
            supported: crate::autostart::supported(),
            runtime: runtime.handle().clone(),
            lock: Arc::default(),
            requests: Arc::default(),
        };
        app.set_launch_at_login_available(this.supported);
        if !this.supported {
            return this;
        }
        this.refresh(app, true);
        let toggle = this.clone();
        let app_weak = app.as_weak();
        app.on_set_launch_at_login(move |enabled| {
            if app_weak.upgrade().is_some() {
                toggle.set(app_weak.clone(), enabled);
            }
        });
        this
    }

    /// Read the entry into the switch. At startup, also re-point an entry
    /// whose executable was moved or replaced.
    fn refresh(&self, app: &AppWindow, repair: bool) {
        if !self.supported {
            return;
        }
        let this = self.clone();
        let app = app.as_weak();
        self.runtime.spawn_blocking(move || {
            use std::sync::atomic::Ordering;
            let _guard = this.lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let requests = this.requests.load(Ordering::SeqCst);
            if repair && let Err(error) = crate::autostart::repair() {
                eprintln!("could not update the sign-in entry: {error}");
            }
            match crate::autostart::is_enabled() {
                // A toggle requested meanwhile reports its own result.
                Ok(enabled) if this.requests.load(Ordering::SeqCst) == requests => {
                    let _ = app.upgrade_in_event_loop(move |app| app.set_launch_at_login(enabled));
                }
                Ok(_) => {}
                Err(error) => eprintln!("could not read the sign-in entry: {error}"),
            }
        });
    }

    fn set(&self, app: slint::Weak<AppWindow>, enabled: bool) {
        use std::sync::atomic::Ordering;
        let request = self.requests.fetch_add(1, Ordering::SeqCst) + 1;
        let this = self.clone();
        self.runtime.spawn_blocking(move || {
            let _guard = this.lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            // Only the latest request is applied; it supersedes this one.
            if this.requests.load(Ordering::SeqCst) != request {
                return;
            }
            use crate::autostart::Applied;
            let result = crate::autostart::set_enabled(enabled).map_err(|error| error.to_string());
            let actual = crate::autostart::is_enabled()
                .unwrap_or(enabled == matches!(result, Ok(Applied::Done)));
            let _ = app.upgrade_in_event_loop(move |app| {
                app.set_launch_at_login(actual);
                app.set_sync_status(match result {
                    Ok(Applied::Done) if enabled => {
                        UiMessage::plain("Flectar Mail will start when you sign in.")
                    }
                    Ok(Applied::Done) => UiMessage::plain(
                        "Flectar Mail will no longer start when you sign in.",
                    ),
                    Ok(Applied::NeedsApproval) => UiMessage::plain(
                        "Allow Flectar Mail under Login Items in System Settings to finish.",
                    ),
                    Err(error) => {
                        UiMessage::detail("Could not change startup behavior: {}", error)
                    }
                });
            });
        });
    }
}
