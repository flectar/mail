//! The tray icon: the chosen app icon, with the unread count on it where the
//! tray shows the app's own picture, and the count in its tooltip.
//!
//! The count follows the one on the app icon, so the same setting turns it
//! off. It matters most on Windows, where the taskbar loses its count with
//! the window that carries it, so the tray is all that shows it while the
//! window is closed. Windows draws the counts of packaged apps at the top
//! right of their icons, so the tray's goes there, in their colours. macOS
//! keeps the count on the Dock, where it stays while the window is closed.
//!
//! Most people never turn the tray on, so it is drawn, and follows the
//! desktop's colours, only once it is.

use crate::{AppWindow, tray_ui::FlectarTray};
use slint::ComponentHandle;
use std::cell::{Cell, RefCell};

#[cfg(target_os = "windows")]
mod windows;

struct Tray {
    handle: slint::Weak<FlectarTray>,
    /// The chosen icon's tray picture, under the count.
    icon: slint::Image,
}

thread_local! {
    static TRAY: RefCell<Option<Tray>> = const { RefCell::new(None) };
    /// Unread mail the app icon counts, 0 for none, kept whether or not a
    /// tray is there to show it yet.
    static UNREAD: Cell<u32> = const { Cell::new(0) };
    /// Whether that counts conversations, as the mail list shows them,
    /// rather than messages.
    static CONVERSATIONS: Cell<bool> = const { Cell::new(false) };
    /// The tray follows the desktop's colours and scale, from the first time
    /// it is on.
    static FOLLOWING: Cell<bool> = const { Cell::new(false) };
}

/// Show the chosen icon and the count in `tray` from now on.
pub(crate) fn attach(app: &AppWindow, tray: &FlectarTray) {
    let icon = app.global::<crate::AppIcon>();
    TRAY.with(|slot| {
        *slot.borrow_mut() = Some(Tray {
            handle: tray.as_weak(),
            icon: icon.invoke_tray_image(icon.get_variant()),
        });
    });
    tray.on_shown(shown);
    if tray.get_enabled() {
        shown();
    } else {
        refresh();
    }
}

/// Show `icon`, the tray picture of a newly chosen app icon.
pub(crate) fn set_icon(icon: slint::Image) {
    TRAY.with(|tray| {
        if let Some(tray) = tray.borrow_mut().as_mut() {
            tray.icon = icon;
        }
    });
    refresh();
}

/// Show `count` unread conversations, or messages, or none.
pub(crate) fn show_unread(count: Option<u32>, conversations: bool) {
    UNREAD.set(count.unwrap_or(0));
    CONVERSATIONS.set(conversations);
    refresh();
}

/// The tray turned on, or was on as it was attached.
fn shown() {
    if !FOLLOWING.replace(true) {
        #[cfg(target_os = "windows")]
        windows::follow_changes();
    }
    refresh();
}

/// Put the count on the tray, and the picture while the tray is on.
fn refresh() {
    let Some((tray, icon)) = TRAY.with(|tray| {
        let tray = tray.borrow();
        let tray = tray.as_ref()?;
        Some((tray.handle.upgrade()?, tray.icon.clone()))
    }) else {
        return;
    };
    let unread = UNREAD.get();
    tray.set_unread(i32::try_from(unread).unwrap_or(i32::MAX));
    tray.set_conversations(CONVERSATIONS.get());
    // A tray that is off is drawn when it turns on.
    if tray.get_enabled() {
        tray.set_image(picture(&icon, unread).unwrap_or(icon));
    }
}

/// `icon` with `unread` on it, where the tray shows the app's own picture.
fn picture(icon: &slint::Image, unread: u32) -> Option<slint::Image> {
    #[cfg(target_os = "windows")]
    return windows::picture(icon, unread);
    #[allow(unreachable_code)]
    {
        let _ = (icon, unread);
        None
    }
}
