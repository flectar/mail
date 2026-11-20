//! The tray icon: the chosen app icon, with the unread count on it where the
//! tray shows the app's own picture, and the count in its tooltip.
//!
//! The count follows the one on the app icon, so the same setting turns it
//! off. It matters most on Windows, where the taskbar loses its count with
//! the window that carries it, so the tray is all that shows it while the
//! window is closed. Windows draws the counts of packaged apps at the top
//! right of their icons, and so does KDE Plasma, so the tray's goes there,
//! in their colours. macOS keeps the count on the Dock, where it stays while
//! the window is closed.
//!
//! Most people never turn the tray on, so it is drawn, and follows the
//! desktop's colours, only once it is.

use crate::{AppWindow, tray_ui::FlectarTray};
use slint::ComponentHandle;
use std::cell::{Cell, RefCell};

#[cfg(all(linux_desktop, not(feature = "flatpak")))]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

struct Tray {
    handle: slint::Weak<FlectarTray>,
    /// The chosen icon's tray picture, under the count.
    icon: slint::Image,
    /// Where the Linux tray follows the desktop's accent colour from.
    #[cfg(all(linux_desktop, not(feature = "flatpak")))]
    runtime: tokio::runtime::Handle,
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
pub(crate) fn attach(app: &AppWindow, tray: &FlectarTray, runtime: &tokio::runtime::Handle) {
    let icon = app.global::<crate::AppIcon>();
    TRAY.with(|slot| {
        *slot.borrow_mut() = Some(Tray {
            handle: tray.as_weak(),
            icon: icon.invoke_tray_image(icon.get_variant()),
            #[cfg(all(linux_desktop, not(feature = "flatpak")))]
            runtime: runtime.clone(),
        });
    });
    #[cfg(not(all(linux_desktop, not(feature = "flatpak"))))]
    let _ = runtime;
    // macOS shows a menu bar item's name as text beside its icon, so the item
    // goes without one there, and VoiceOver gets the name from macos.rs.
    tray.set_named(!cfg!(target_os = "macos"));
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
        #[cfg(all(linux_desktop, not(feature = "flatpak")))]
        TRAY.with(|tray| {
            if let Some(tray) = tray.borrow().as_ref() {
                linux::follow_accent(&tray.runtime);
            }
        });
    }
    // Its item may only now exist.
    #[cfg(target_os = "macos")]
    macos::name_the_item();
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
    #[cfg(all(linux_desktop, not(feature = "flatpak")))]
    return linux::picture(icon, unread);
    #[allow(unreachable_code)]
    {
        let _ = (icon, unread);
        None
    }
}

#[cfg(all(test, linux_desktop, not(feature = "flatpak")))]
mod tests {
    use super::*;
    use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
    use slint::platform::{Platform, WindowAdapter};
    use slint::{Rgba8Pixel, SharedPixelBuffer};
    use std::rc::Rc;

    struct Headless(Rc<MinimalSoftwareWindow>);

    impl Platform for Headless {
        fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
            Ok(self.0.clone())
        }
    }

    #[test]
    fn the_count_goes_on_the_tray_icon_and_comes_off_again() {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(Headless(window))).unwrap();
        // The window loads the typeface the count is drawn in.
        let app = AppWindow::new().unwrap();
        let tray = FlectarTray::new().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        tray.set_enabled(true);
        attach(&app, &tray, runtime.handle());
        let plain = tray.get_image().to_rgba8().unwrap();

        show_unread(Some(3), true);
        assert_eq!(tray.get_unread(), 3);
        assert!(tray.get_conversations());
        let counted = tray.get_image().to_rgba8().unwrap();
        assert_eq!(
            (counted.width(), counted.height()),
            (plain.width(), plain.height())
        );
        let at = |image: &SharedPixelBuffer<Rgba8Pixel>, x: u32, y: u32| {
            image.as_slice()[(y * image.width() + x) as usize]
        };
        // The disc covers the top right corner; the bottom left stays the icon.
        assert_ne!(at(&counted, 52, 12), at(&plain, 52, 12));
        assert_eq!(at(&counted, 6, 58), at(&plain, 6, 58));
        // Its digit is drawn, black on the default blue.
        let ink = (32..64)
            .flat_map(|x| (0..48).map(move |y| (x, y)))
            .filter(|&(x, y)| {
                let pixel = at(&counted, x, y);
                pixel.a > 200 && pixel.r < 40 && pixel.g < 40 && pixel.b < 40
            })
            .count();
        assert!(ink > 20, "{ink} pixels of the digit");

        show_unread(None, true);
        assert_eq!(tray.get_unread(), 0);
        assert_eq!(
            tray.get_image().to_rgba8().unwrap().as_bytes(),
            plain.as_bytes()
        );

        // A tray that is off keeps its picture until it turns on.
        tray.set_enabled(false);
        show_unread(Some(5), false);
        assert_eq!(tray.get_unread(), 5);
        assert_eq!(
            tray.get_image().to_rgba8().unwrap().as_bytes(),
            plain.as_bytes()
        );
    }
}
