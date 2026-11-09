//! Integrated window frame and window controls.
//!
//! The "system" frame leaves decorations to the platform. The "integrated"
//! frame removes the title bar, draws the window controls over the top
//! corners of the application's own toolbars, and lets empty toolbar space
//! move the window. Each platform keeps the window-manager behavior users
//! already rely on:
//!
//! - Windows keeps its native frame, so DWM still provides the shadow, rounded
//!   corners, Aero Snap and the system menu. Only the caption is removed, and
//!   the drawn caption buttons are reported to Windows through non-client
//!   hit-testing, which also enables the Snap Layouts flyout.
//! - macOS keeps the native traffic lights over a transparent, full-size
//!   title bar and follows the system's title-bar double-click preference.
//! - Linux draws GTK-style controls in the order configured for GTK and KDE
//!   windows, follows the desktop's title-bar click actions, and asks the
//!   window manager for its window menu.

use crate::{AppWindow, WindowChrome};
use flectar_mail_core::models::normalized_window_frame;
use slint::ComponentHandle;
#[cfg(desktop)]
use slint::winit_030::{EventResult, WinitWindowAccessor, winit};
use std::cell::Cell;

#[cfg(linux_desktop)]
mod activation;
#[cfg(linux_desktop)]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;
/// The unread count over the taskbar button of a copy without package
/// identity, which draws its own.
#[cfg(target_os = "windows")]
mod taskbar_badge;
#[cfg(any(test, target_os = "windows"))]
mod badge_pixels;

/// Show the unread count on the main window's taskbar button, as a copy
/// without package identity must: Windows' own badges need one.
#[cfg(target_os = "windows")]
pub(crate) fn set_taskbar_badge(badge: Option<(u32, String)>) {
    windows::set_taskbar_badge(badge);
}

/// The integrated frame is the default; unknown saved values fall back to the
/// system frame so a window can never start without any controls.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum WindowFrame {
    System,
    #[default]
    Integrated,
}

impl WindowFrame {
    pub(crate) fn parse(value: &str) -> Self {
        match normalized_window_frame(value) {
            "integrated" => Self::Integrated,
            _ => Self::System,
        }
    }

    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Integrated => "integrated",
        }
    }
}

/// What a title-bar gesture does, in the vocabulary GNOME and KWin share.
/// Linux follows every configurable action; Windows and macOS only map the
/// gestures their own title bars support.
#[cfg_attr(not(linux_desktop), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TitleBarAction {
    ToggleMaximize,
    Minimize,
    Close,
    Menu,
    Lower,
}

/// Platform name shared with the window controls; empty where the platform
/// always owns the window frame.
pub(crate) const PLATFORM: &str = if cfg!(target_os = "windows") {
    "windows"
} else if cfg!(target_os = "macos") {
    "macos"
} else if cfg!(linux_desktop) {
    "linux"
} else {
    ""
};

thread_local! {
    static STARTUP_FRAME: Cell<WindowFrame> = const { Cell::new(WindowFrame::System) };
    /// Set while a composer window is being created.
    #[cfg(desktop)]
    static CREATING_COMPOSER: Cell<bool> = const { Cell::new(false) };
}

/// Native window attributes for the main window, applied by the backend
/// selector before the window is created. Composer windows keep the
/// platform's own frame.
#[cfg(any(target_os = "windows", target_os = "macos"))]
#[cfg_attr(target_os = "windows", allow(unused_variables))]
pub(crate) fn window_attributes(
    startup: WindowFrame,
) -> impl Fn(winit::window::WindowAttributes) -> winit::window::WindowAttributes + 'static {
    move |attributes| {
        if CREATING_COMPOSER.get() {
            #[cfg(target_os = "windows")]
            let attributes = windows::composer_window_attributes(attributes);
            return attributes;
        }
        #[cfg(target_os = "windows")]
        let attributes = windows::window_attributes(attributes);
        #[cfg(target_os = "macos")]
        let attributes = macos::window_attributes(attributes, startup);
        attributes
    }
}

/// Create a composer window. The winit backend applies the attributes hook
/// while the component creates its window, so the hook can tell the two
/// kinds apart: only the main window gets the integrated frame.
#[cfg(desktop)]
pub(crate) fn composer_window<T>(
    create: impl FnOnce() -> Result<T, slint::PlatformError>,
) -> Result<T, slint::PlatformError> {
    CREATING_COMPOSER.set(true);
    let window = create();
    CREATING_COMPOSER.set(false);
    window
}

/// Prepare platform hooks that must observe native window creation. Call
/// after selecting the backend and before showing the main window.
pub(crate) fn prepare(startup: WindowFrame) {
    STARTUP_FRAME.set(startup);
    #[cfg(target_os = "windows")]
    windows::prepare(startup);
}

pub(crate) fn register(app: &AppWindow, runtime: &tokio::runtime::Handle) {
    let chrome = app.global::<WindowChrome>();
    chrome.set_platform(PLATFORM.into());

    let weak = app.as_weak();
    chrome.on_minimize(move || {
        if let Some(app) = weak.upgrade() {
            app.window().set_minimized(true);
        }
    });
    let weak = app.as_weak();
    chrome.on_toggle_maximize(move || {
        if let Some(app) = weak.upgrade() {
            toggle_maximized(app.window());
        }
    });
    let weak = app.as_weak();
    chrome.on_close(move || {
        if let Some(app) = weak.upgrade() {
            close(app.window());
        }
    });

    #[cfg(linux_desktop)]
    linux::register(app, runtime);
    #[cfg(target_os = "windows")]
    windows::register(app);
    #[cfg(target_os = "macos")]
    macos::register(app);
    #[cfg(not(linux_desktop))]
    let _ = runtime;
    apply_preference(app, STARTUP_FRAME.get());

    #[cfg(desktop)]
    register_desktop_events(app);
}

/// Control+Q alone, the key `Shortcuts.quit` lists.
#[cfg(linux_desktop)]
pub(crate) fn is_quit(
    key: &winit::keyboard::Key,
    modifiers: winit::keyboard::ModifiersState,
) -> bool {
    matches!(key, winit::keyboard::Key::Character(text) if text.eq_ignore_ascii_case("q"))
        && modifiers == winit::keyboard::ModifiersState::CONTROL
}

/// Quit for Control+Q pressed in any of the app's windows, as GNOME and KDE
/// have it. Messages being written go to Drafts as the app ends; only the
/// calendar editor's unsaved changes keep it from quitting. Control+Q is the
/// only way out of a Flatpak running in the background where the desktop
/// lists no background apps. Returns whether it quit.
#[cfg(linux_desktop)]
pub(crate) fn quit_key(
    app: &AppWindow,
    event: &winit::event::KeyEvent,
    modifiers: winit::keyboard::ModifiersState,
) -> bool {
    let quits = event.state.is_pressed()
        && !event.repeat
        && is_quit(&event.logical_key, modifiers)
        && !app.get_calendar_editor_open();
    if quits {
        crate::window_controller::quit(app);
    }
    quits
}

#[cfg(desktop)]
fn register_desktop_events(app: &AppWindow) {
    let chrome = app.global::<WindowChrome>();
    let weak = app.as_weak();
    chrome.on_title_bar_gesture(move |gesture, x, y| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        if let Some(action) = title_bar_action(&app, &gesture) {
            perform(&app, action, x, y);
        }
    });

    // Slint keeps one winit event filter per window; this module owns it.
    let weak = app.as_weak();
    let modifiers = Cell::new(winit::keyboard::ModifiersState::empty());
    app.window().on_winit_window_event(move |_, event| {
        let Some(app) = weak.upgrade() else {
            return EventResult::Propagate;
        };
        // Input, resizing and focus changes leave freed memory behind; return
        // it once the window goes still. Repaints don't count: a blinking text
        // cursor would keep the countdown from ever ending.
        if !matches!(event, winit::event::WindowEvent::RedrawRequested) {
            crate::memory::release_free_heap_when_idle();
        }
        #[cfg(linux_desktop)]
        if let winit::event::WindowEvent::KeyboardInput { event, .. } = event
            && quit_key(&app, event, modifiers.get())
        {
            return EventResult::PreventDefault;
        }
        match event {
            winit::event::WindowEvent::ModifiersChanged(state) => modifiers.set(state.state()),
            // Tab shortcuts work wherever focus is, as in a browser, except in
            // the composer and the calendar editor, which keep their keys.
            winit::event::WindowEvent::KeyboardInput { event, .. }
                if event.state.is_pressed()
                    && app.global::<crate::TabsUi>().get_enabled()
                    && !app.global::<crate::ComposerUi>().get_open()
                    && !app.get_calendar_editor_open() =>
            {
                if let Some(command) = crate::tabs::shortcut(&event.logical_key, modifiers.get()) {
                    app.global::<crate::TabsUi>()
                        .invoke_command(command.key().into());
                    return EventResult::PreventDefault;
                }
            }
            winit::event::WindowEvent::Focused(focused) => {
                let chrome = app.global::<WindowChrome>();
                chrome.set_active(*focused);
                if *focused {
                    crate::default_email_app::window_focused(&app);
                    crate::notifications::ui::refresh_availability();
                }
                #[cfg(linux_desktop)]
                if *focused && chrome.get_integrated() {
                    linux::refresh(&app);
                }
                #[cfg(target_os = "macos")]
                macos::refresh(&app);
                report_minimized(&app);
            }
            winit::event::WindowEvent::Resized(size) => {
                // AppKit lays the title bar out again on resize and full-screen
                // transitions, which moves the traffic lights back.
                #[cfg(target_os = "macos")]
                macos::refresh(&app);
                // Windows reports minimizing as a resize to zero.
                if size.width == 0
                    || size.height == 0
                    || crate::window_controller::reported_minimized()
                {
                    report_minimized(&app);
                }
            }
            winit::event::WindowEvent::Occluded(_) => report_minimized(&app),
            // X11 may restore a window without another event; its first frame
            // is the earliest reliable signal.
            winit::event::WindowEvent::RedrawRequested
                if crate::window_controller::reported_minimized() =>
            {
                report_minimized(&app);
            }
            _ => {}
        }
        EventResult::Propagate
    });
}

/// Forward the platform's minimized state to the window lifecycle. Wayland
/// cannot report it, so nothing is released there.
#[cfg(desktop)]
fn report_minimized(app: &AppWindow) {
    if let Some(minimized) = app
        .window()
        .with_winit_window(|window| window.is_minimized())
        .flatten()
    {
        crate::window_controller::minimized_changed(app, minimized);
    }
}

/// Apply a saved preference. Every desktop platform switches immediately.
pub(crate) fn apply_preference(app: &AppWindow, frame: WindowFrame) {
    let chrome = app.global::<WindowChrome>();
    chrome.set_preference(frame.key().into());
    let integrated = !PLATFORM.is_empty() && frame == WindowFrame::Integrated;
    chrome.set_integrated(integrated);
    #[cfg(linux_desktop)]
    if integrated {
        linux::refresh(app);
    }
    #[cfg(target_os = "windows")]
    windows::set_enabled(integrated);
    #[cfg(target_os = "macos")]
    macos::set_integrated(app, integrated);
}

#[cfg(desktop)]
#[cfg_attr(not(linux_desktop), allow(unused_variables))]
fn title_bar_action(app: &AppWindow, gesture: &str) -> Option<TitleBarAction> {
    #[cfg(linux_desktop)]
    return linux::title_bar_action(app, gesture);
    #[cfg(target_os = "windows")]
    return windows::title_bar_action(gesture);
    #[cfg(target_os = "macos")]
    return macos::title_bar_action(gesture);
    #[allow(unreachable_code)]
    None
}

#[cfg(desktop)]
fn perform(app: &AppWindow, action: TitleBarAction, x: f32, y: f32) {
    let window = app.window();
    match action {
        TitleBarAction::ToggleMaximize => toggle_maximized(window),
        TitleBarAction::Minimize => window.set_minimized(true),
        TitleBarAction::Close => close(window),
        TitleBarAction::Menu => show_window_menu(window, x, y),
        TitleBarAction::Lower => {
            #[cfg(linux_desktop)]
            linux::lower(window);
        }
    }
}

/// Open the window manager's window menu at a window-relative position.
#[cfg(desktop)]
fn show_window_menu(window: &slint::Window, x: f32, y: f32) {
    // Winit implements the window menu on Windows and Wayland.
    #[cfg(linux_desktop)]
    if linux::show_x11_window_menu(window) {
        return;
    }
    let scale = window.scale_factor();
    window.with_winit_window(|window| {
        window.show_window_menu(winit::dpi::PhysicalPosition::new(
            f64::from(x * scale),
            f64::from(y * scale),
        ));
    });
}

fn toggle_maximized(window: &slint::Window) {
    window.set_maximized(!window.is_maximized());
}

/// Keep the launcher's activation token and clear it from the environment.
///
/// # Safety
///
/// Changes the environment: no other thread may be running.
#[cfg(desktop)]
pub(crate) unsafe fn take_launch_activation_token() {
    // SAFETY: forwarded from the caller.
    #[cfg(linux_desktop)]
    unsafe {
        activation::take_launch_token()
    };
}

/// The activation token this process was launched with, for its first window
/// or to forward to an already running instance.
#[cfg(desktop)]
pub(crate) fn launch_activation_token() -> Option<String> {
    #[cfg(linux_desktop)]
    return activation::launch_token();
    #[cfg(not(linux_desktop))]
    None
}

/// The launch token as the environment variable it arrived in, so a process
/// that replaces this one can still use it.
#[cfg(desktop)]
pub(crate) fn launch_activation_variable() -> Option<(&'static str, String)> {
    #[cfg(linux_desktop)]
    return activation::launch_token_variable();
    #[cfg(not(linux_desktop))]
    None
}

/// Ask the platform to focus the main window, using a launcher's activation
/// token where the desktop requires one. A window shown from the tray is
/// created again first, so this waits for the native window.
#[cfg(desktop)]
pub(crate) fn focus(app: &AppWindow, activation_token: Option<String>) {
    let app = app.as_weak();
    let _ = slint::spawn_local(async move {
        let Some(app) = app.upgrade() else {
            return;
        };
        let Ok(window) = app.window().winit_window().await else {
            return;
        };
        #[cfg(linux_desktop)]
        if let Some(token) = activation_token.as_deref()
            && activation::activate(&window, token)
        {
            return;
        }
        #[cfg(not(linux_desktop))]
        let _ = activation_token;
        window.focus_window();
    });
}

/// Prepare platform hooks before the main window is shown again. Hiding it
/// destroys the native window, and Slint creates the new one without the
/// attributes hook passed to the backend selector.
pub(crate) fn before_show() {
    #[cfg(target_os = "windows")]
    windows::rearm();
}

/// Follow the same path as the platform close button so tray and
/// back-navigation policies apply unchanged.
fn close(window: &slint::Window) {
    window.dispatch_event(slint::platform::WindowEvent::CloseRequested);
}

#[cfg(all(test, linux_desktop))]
mod tests {
    use super::is_quit;
    use super::winit::keyboard::{Key, ModifiersState, NamedKey};

    #[test]
    fn only_control_q_quits() {
        let control = ModifiersState::CONTROL;
        let q = Key::Character("q".into());
        assert!(is_quit(&q, control));
        assert!(is_quit(&Key::Character("Q".into()), control));
        assert!(!is_quit(&q, ModifiersState::empty()));
        assert!(!is_quit(&q, control | ModifiersState::SHIFT));
        assert!(!is_quit(&q, control | ModifiersState::ALT));
        assert!(!is_quit(&Key::Character("w".into()), control));
        assert!(!is_quit(&Key::Named(NamedKey::Escape), control));
    }
}
