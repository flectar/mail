//! The operating system's request to reduce motion (#231). While the Reduce
//! motion setting is System, `MotionSettings.system-reduced` follows Windows'
//! animation effects, macOS' Reduce motion, the desktop portal's reduced
//! motion or GNOME's animations switch on Linux, Android's Remove animations,
//! iOS' Reduce Motion, and HarmonyOS' animation reduction.

use crate::{AppWindow, MotionSettings};
use slint::ComponentHandle;
#[cfg(any(target_os = "windows", mobile))]
use std::cell::RefCell;

#[cfg(any(target_os = "windows", mobile))]
thread_local! {
    /// The window to update when the system reports a change later.
    static APP: RefCell<Option<slint::Weak<AppWindow>>> = const { RefCell::new(None) };
}

/// Read the system setting now and follow its changes.
pub(crate) fn watch(app: &AppWindow, runtime: &tokio::runtime::Handle) {
    #[cfg(any(target_os = "windows", mobile))]
    {
        APP.with_borrow_mut(|weak| *weak = Some(app.as_weak()));
        publish(app, system_reduced());
    }
    #[cfg(mobile)]
    crate::platform::watch_reduce_motion();
    #[cfg(target_os = "macos")]
    {
        publish(app, macos::reduced());
        macos::observe(app.as_weak());
    }
    #[cfg(linux_desktop)]
    linux::watch(app.as_weak(), runtime);
    let _ = (app, runtime);
}

fn publish(app: &AppWindow, reduced: bool) {
    app.global::<MotionSettings>().set_system_reduced(reduced);
}

#[cfg(target_os = "windows")]
fn system_reduced() -> bool {
    windows::reduced()
}

#[cfg(mobile)]
fn system_reduced() -> bool {
    crate::platform::reduce_motion()
}

/// Read the setting again and update the interface. UI thread only.
#[cfg(any(target_os = "windows", mobile))]
fn refresh() {
    let app = APP.with_borrow(|weak| weak.as_ref().and_then(slint::Weak::upgrade));
    if let Some(app) = app {
        publish(&app, system_reduced());
        // Phones report coming back to the front here too, when the system's
        // text size may have changed as well.
        #[cfg(mobile)]
        crate::text_size::refresh(&app);
    }
}

/// Windows tells every top-level window when its animation setting changes,
/// and the main window's subclass passes that on.
#[cfg(target_os = "windows")]
pub(crate) fn system_changed() {
    // The window procedure is mid-dispatch, so update the interface after it.
    slint::Timer::single_shot(std::time::Duration::ZERO, refresh);
}

/// The phone reported a change, or the app came back to the front, where the
/// user may have changed it in the system settings. Any thread may call it.
#[cfg(mobile)]
pub(crate) fn system_changed() {
    let _ = slint::invoke_from_event_loop(refresh);
}

#[cfg(target_os = "windows")]
mod windows {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SPI_GETCLIENTAREAANIMATION, SystemParametersInfoW,
    };

    /// Settings > Accessibility > Visual effects > Animation effects is off.
    pub(super) fn reduced() -> bool {
        let mut animations: i32 = 1;
        // SAFETY: SPI_GETCLIENTAREAANIMATION writes a BOOL through the pointer.
        let read = unsafe {
            SystemParametersInfoW(
                SPI_GETCLIENTAREAANIMATION,
                0,
                (&mut animations as *mut i32).cast(),
                0,
            )
        };
        read != 0 && animations == 0
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use crate::AppWindow;
    use block2::RcBlock;
    use objc2_app_kit::{NSWorkspace, NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification};
    use objc2_foundation::NSNotification;
    use std::ptr::NonNull;

    /// System Settings > Accessibility > Display > Reduce motion.
    pub(super) fn reduced() -> bool {
        NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion()
    }

    /// AppKit posts the change on the workspace's own notification center.
    pub(super) fn observe(weak: slint::Weak<AppWindow>) {
        let block = RcBlock::new(move |_: NonNull<NSNotification>| {
            let reduced = reduced();
            let _ = weak.upgrade_in_event_loop(move |app| super::publish(&app, reduced));
        });
        let workspace = NSWorkspace::sharedWorkspace();
        // SAFETY: the name is AppKit's own constant and the block runs on
        // whichever thread posts, which only hands the value to the event loop.
        let observer = unsafe {
            workspace
                .notificationCenter()
                .addObserverForName_object_queue_usingBlock(
                    Some(NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification),
                    None,
                    None,
                    &block,
                )
        };
        // The observer lasts as long as the app.
        std::mem::forget(observer);
    }
}

#[cfg(linux_desktop)]
mod linux {
    use crate::AppWindow;
    use futures_util::StreamExt;
    use std::time::Duration;

    const PORTAL_TIMEOUT: Duration = Duration::from_secs(3);
    const APPEARANCE: &str = "org.freedesktop.appearance";
    const REDUCED_MOTION: &str = "reduced-motion";
    const GNOME_INTERFACE: &str = "org.gnome.desktop.interface";
    const ENABLE_ANIMATIONS: &str = "enable-animations";

    /// The portal's reduced motion (xdg-desktop-portal 1.21), and GNOME's
    /// animations switch, which GNOME 49 and earlier offer as Reduce
    /// animation and the GNOME and GTK portals expose. Either asks for less
    /// motion, as Qt reads them.
    #[derive(Clone, Copy, Default)]
    struct Sources {
        reduced_motion: bool,
        animations_off: bool,
    }

    impl Sources {
        fn reduced(self) -> bool {
            self.reduced_motion || self.animations_off
        }
    }

    pub(super) fn watch(weak: slint::Weak<AppWindow>, runtime: &tokio::runtime::Handle) {
        runtime.spawn(async move {
            let Ok(Ok(settings)) =
                tokio::time::timeout(PORTAL_TIMEOUT, ashpd::desktop::settings::Settings::new())
                    .await
            else {
                return;
            };
            let mut sources = Sources::default();
            if let Ok(Ok(value)) = tokio::time::timeout(
                PORTAL_TIMEOUT,
                settings.read::<u32>(APPEARANCE, REDUCED_MOTION),
            )
            .await
            {
                sources.reduced_motion = value == 1;
            }
            if let Ok(Ok(value)) = tokio::time::timeout(
                PORTAL_TIMEOUT,
                settings.read::<bool>(GNOME_INTERFACE, ENABLE_ANIMATIONS),
            )
            .await
            {
                sources.animations_off = !value;
            }
            publish(&weak, sources.reduced());

            let Ok(mut changes) = settings.receive_setting_changed().await else {
                return;
            };
            while let Some(setting) = changes.next().await {
                let before = sources.reduced();
                match (setting.namespace(), setting.key(), &**setting.value()) {
                    (APPEARANCE, REDUCED_MOTION, ashpd::zvariant::Value::U32(value)) => {
                        sources.reduced_motion = *value == 1;
                    }
                    (GNOME_INTERFACE, ENABLE_ANIMATIONS, ashpd::zvariant::Value::Bool(value)) => {
                        sources.animations_off = !*value;
                    }
                    _ => continue,
                }
                if sources.reduced() != before {
                    publish(&weak, sources.reduced());
                }
            }
        });
    }

    fn publish(weak: &slint::Weak<AppWindow>, reduced: bool) {
        let _ = weak.upgrade_in_event_loop(move |app| super::publish(&app, reduced));
    }
}
