//! The application icon chosen in Settings.
//!
//! The window icon and the in-app logos bind to the `AppIcon` Slint global,
//! which is all Windows and X11 window managers need. The other places that
//! show the icon are outside Slint's reach:
//!
//! - the tray is a separate Slint compilation unit and gets its image here;
//! - macOS draws the Dock and app switcher icon from the application, not the
//!   window;
//! - Linux launchers, and Wayland taskbars, look the icon up in the icon
//!   theme by the desktop entry's name. A copy in the user's own icon theme
//!   takes precedence over the installed one;
//! - mobile home screens switch to icons bundled with the app, through the
//!   platform layer.
//!
//! Flatpak can neither show a tray nor write to the host's icon theme, so the
//! choice is not offered there.

use crate::{AppIconVariant, AppWindow, tray_ui::FlectarTray};
use flectar_mail_core::models;
use slint::ComponentHandle;
use std::cell::RefCell;

pub(crate) const AVAILABLE: bool = cfg!(not(feature = "flatpak"));

/// Lets Settings describe where this platform shows the icon.
#[cfg(desktop)]
const PLATFORM: &str = crate::window_chrome::PLATFORM;
#[cfg(mobile)]
const PLATFORM: &str = crate::platform::NAME;

thread_local! {
    static TRAY: RefCell<Option<slint::Weak<FlectarTray>>> = const { RefCell::new(None) };
}

pub(crate) fn variant(icon: models::AppIcon) -> AppIconVariant {
    match icon {
        models::AppIcon::Orange => AppIconVariant::Orange,
        models::AppIcon::Graphite => AppIconVariant::Graphite,
        models::AppIcon::Light => AppIconVariant::Light,
        models::AppIcon::Blue => AppIconVariant::Blue,
        models::AppIcon::Green => AppIconVariant::Green,
        models::AppIcon::Purple => AppIconVariant::Purple,
    }
}

pub(crate) fn choice(variant: AppIconVariant) -> models::AppIcon {
    match variant {
        AppIconVariant::Orange => models::AppIcon::Orange,
        AppIconVariant::Graphite => models::AppIcon::Graphite,
        AppIconVariant::Light => models::AppIcon::Light,
        AppIconVariant::Blue => models::AppIcon::Blue,
        AppIconVariant::Green => models::AppIcon::Green,
        AppIconVariant::Purple => models::AppIcon::Purple,
    }
}

/// Show `icon` everywhere the platform displays it. Call it before the window
/// is first shown so the first frame already carries the chosen icon.
pub(crate) fn apply(app: &AppWindow, icon: models::AppIcon) {
    let icon = if AVAILABLE {
        icon
    } else {
        models::AppIcon::default()
    };
    let global = app.global::<crate::AppIcon>();
    global.set_available(AVAILABLE);
    global.set_platform(PLATFORM.into());
    global.set_variant(variant(icon));
    TRAY.with(|tray| {
        if let Some(tray) = tray.borrow().as_ref().and_then(slint::Weak::upgrade) {
            tray.set_image(global.invoke_tray_image(variant(icon)));
        }
    });
    #[cfg(target_os = "macos")]
    dock::show(icon);
    #[cfg(all(linux_desktop, not(feature = "flatpak")))]
    launcher::request(icon);
    #[cfg(mobile)]
    crate::platform::show_app_icon(icon);
}

/// Keep the tray's image in step with the chosen icon.
pub(crate) fn attach_tray(app: &AppWindow, tray: &FlectarTray) {
    let global = app.global::<crate::AppIcon>();
    tray.set_image(global.invoke_tray_image(global.get_variant()));
    TRAY.with(|slot| *slot.borrow_mut() = Some(tray.as_weak()));
}

/// The 512 px rendering of a non-default icon, for the Dock and the Linux
/// icon theme. The default icon is the installed one.
#[cfg(any(target_os = "macos", all(linux_desktop, not(feature = "flatpak"))))]
fn png_512(icon: models::AppIcon) -> Option<&'static [u8]> {
    match icon {
        models::AppIcon::Orange => None,
        models::AppIcon::Graphite => Some(include_bytes!(
            "../resources/app-icon/variants/graphite-512.png"
        )),
        models::AppIcon::Light => Some(include_bytes!(
            "../resources/app-icon/variants/light-512.png"
        )),
        models::AppIcon::Blue => Some(include_bytes!(
            "../resources/app-icon/variants/blue-512.png"
        )),
        models::AppIcon::Green => Some(include_bytes!(
            "../resources/app-icon/variants/green-512.png"
        )),
        models::AppIcon::Purple => Some(include_bytes!(
            "../resources/app-icon/variants/purple-512.png"
        )),
    }
}

#[cfg(target_os = "macos")]
mod dock {
    use flectar_mail_core::models::AppIcon;
    use objc2::{AnyThread, MainThreadMarker};
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::NSData;
    use std::cell::Cell;

    thread_local! {
        /// The bundle's icon shows until the application sets another.
        static SHOWN: Cell<AppIcon> = const { Cell::new(AppIcon::Orange) };
    }

    pub(super) fn show(icon: AppIcon) {
        let Some(main_thread) = MainThreadMarker::new() else {
            return;
        };
        if SHOWN.get() == icon {
            return;
        }
        let image = match super::png_512(icon) {
            Some(png) => match NSImage::initWithData(NSImage::alloc(), &NSData::with_bytes(png)) {
                Some(image) => Some(image),
                None => {
                    eprintln!("could not decode the {} Dock icon", icon.as_str());
                    return;
                }
            },
            None => None,
        };
        // SAFETY: AppKit documents nil as restoring the bundle's icon.
        unsafe {
            NSApplication::sharedApplication(main_thread).setApplicationIconImage(image.as_deref());
        }
        SHOWN.set(icon);
    }
}

/// Copies of the chosen icon in the user's hicolor icon theme, under the name
/// the desktop entry uses. Only files whose contents are one of the bundled
/// variants are ever replaced or removed, so an icon the user put there
/// themselves is left alone.
#[cfg(all(linux_desktop, not(feature = "flatpak")))]
mod launcher {
    use flectar_mail_core::models::AppIcon;
    use std::{
        io::{self, Write},
        path::{Path, PathBuf},
        sync::{Mutex, PoisonError},
        time::SystemTime,
    };

    /// Where the theme looks for the installed icon, matching the scalable
    /// and 512 px copies the packages install.
    const SLOTS: [&str; 2] = [
        "scalable/apps/com.flectar.mail.svg",
        "512x512/apps/com.flectar.mail.png",
    ];

    /// The latest icon to write; a single change in flight applies whichever
    /// icon was chosen last.
    static REQUESTED: Mutex<Option<AppIcon>> = Mutex::new(None);
    static WRITING: Mutex<()> = Mutex::new(());

    pub(super) fn request(icon: AppIcon) {
        // Tests must not rewrite the developer's own launcher icon.
        if cfg!(test) {
            return;
        }
        *REQUESTED.lock().unwrap_or_else(PoisonError::into_inner) = Some(icon);
        let spawned = std::thread::Builder::new()
            .name("launcher-icon".into())
            .spawn(|| {
                let _writing = WRITING.lock().unwrap_or_else(PoisonError::into_inner);
                let Some(icon) = REQUESTED
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take()
                else {
                    return;
                };
                if let Err(error) = hicolor_dir().and_then(|theme| sync(&theme, icon)) {
                    eprintln!("could not update the launcher icon: {error}");
                }
            });
        if let Err(error) = spawned {
            eprintln!("could not update the launcher icon: {error}");
        }
    }

    fn hicolor_dir() -> io::Result<PathBuf> {
        let data = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .filter(|path| path.is_absolute())
                    .map(|home| home.join(".local/share"))
            })
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "neither XDG_DATA_HOME nor HOME is set",
                )
            })?;
        Ok(data.join("icons/hicolor"))
    }

    fn files(icon: AppIcon) -> Option<[&'static [u8]; 2]> {
        let svg: &[u8] = match icon {
            AppIcon::Orange => return None,
            AppIcon::Graphite => include_bytes!("../resources/app-icon/variants/graphite.svg"),
            AppIcon::Light => include_bytes!("../resources/app-icon/variants/light.svg"),
            AppIcon::Blue => include_bytes!("../resources/app-icon/variants/blue.svg"),
            AppIcon::Green => include_bytes!("../resources/app-icon/variants/green.svg"),
            AppIcon::Purple => include_bytes!("../resources/app-icon/variants/purple.svg"),
        };
        Some([svg, super::png_512(icon)?])
    }

    fn bundled(slot: usize, contents: &[u8]) -> bool {
        AppIcon::ALL
            .into_iter()
            .filter_map(files)
            .any(|files| files[slot] == contents)
    }

    /// Make `theme` show `icon`. Returns whether anything changed.
    pub(super) fn sync(theme: &Path, icon: AppIcon) -> io::Result<bool> {
        let wanted = files(icon);
        let mut changed = false;
        for (slot, relative) in SLOTS.into_iter().enumerate() {
            let path = theme.join(relative);
            let current = match std::fs::read(&path) {
                Ok(contents) => Some(contents),
                Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                Err(error) => return Err(error),
            };
            if current
                .as_deref()
                .is_some_and(|contents| !bundled(slot, contents))
            {
                eprintln!("keeping the user's own icon at {}", path.display());
                continue;
            }
            match (wanted, current) {
                (Some(files), current) if current.as_deref() != Some(files[slot]) => {
                    write(&path, files[slot])?;
                    changed = true;
                }
                (None, Some(_)) => {
                    std::fs::remove_file(&path)?;
                    changed = true;
                }
                _ => {}
            }
        }
        if changed {
            // GTK, GNOME Shell and KDE notice theme changes, and ignore a
            // stale icon cache, by the theme directory's modification time.
            std::fs::File::open(theme)?.set_modified(SystemTime::now())?;
        }
        Ok(changed)
    }

    fn write(path: &Path, contents: &[u8]) -> io::Result<()> {
        let directory = path
            .parent()
            .ok_or_else(|| io::Error::other("the icon path has no parent"))?;
        std::fs::create_dir_all(directory)?;
        // Replace atomically so the theme never sees a truncated icon.
        let mut file = tempfile::NamedTempFile::new_in(directory)?;
        file.write_all(contents)?;
        file.as_file().sync_all()?;
        file.persist(path).map_err(|error| error.error)?;
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn contents(theme: &Path, slot: usize) -> Option<Vec<u8>> {
            std::fs::read(theme.join(SLOTS[slot])).ok()
        }

        #[test]
        fn choosing_icons_writes_replaces_and_removes_the_theme_copies() {
            let directory = tempfile::tempdir().unwrap();
            let theme = directory.path().join("icons/hicolor");

            assert!(!sync(&theme, AppIcon::Orange).unwrap());
            assert!(!theme.exists());

            assert!(sync(&theme, AppIcon::Graphite).unwrap());
            let graphite = files(AppIcon::Graphite).unwrap();
            for (slot, file) in graphite.into_iter().enumerate() {
                assert_eq!(contents(&theme, slot).as_deref(), Some(file));
            }
            assert!(!sync(&theme, AppIcon::Graphite).unwrap());

            assert!(sync(&theme, AppIcon::Purple).unwrap());
            let purple = files(AppIcon::Purple).unwrap();
            for (slot, file) in purple.into_iter().enumerate() {
                assert_eq!(contents(&theme, slot).as_deref(), Some(file));
            }

            assert!(sync(&theme, AppIcon::Orange).unwrap());
            for slot in 0..SLOTS.len() {
                assert_eq!(contents(&theme, slot), None);
            }
        }

        #[test]
        fn the_users_own_icon_is_never_replaced_or_removed() {
            let directory = tempfile::tempdir().unwrap();
            let theme = directory.path().join("icons/hicolor");
            let own = theme.join(SLOTS[0]);
            std::fs::create_dir_all(own.parent().unwrap()).unwrap();
            std::fs::write(&own, b"<svg>mine</svg>").unwrap();

            assert!(sync(&theme, AppIcon::Blue).unwrap());
            assert_eq!(
                contents(&theme, 0).as_deref(),
                Some(&b"<svg>mine</svg>"[..])
            );
            assert_eq!(
                contents(&theme, 1).as_deref(),
                Some(files(AppIcon::Blue).unwrap()[1])
            );

            assert!(sync(&theme, AppIcon::Orange).unwrap());
            assert_eq!(
                contents(&theme, 0).as_deref(),
                Some(&b"<svg>mine</svg>"[..])
            );
            assert_eq!(contents(&theme, 1), None);
        }

        #[test]
        fn changes_mark_the_theme_directory_as_modified() {
            let directory = tempfile::tempdir().unwrap();
            let theme = directory.path().join("icons/hicolor");
            // With the size directories in place, only the explicit update
            // changes the theme directory itself.
            for relative in SLOTS {
                std::fs::create_dir_all(theme.join(relative).parent().unwrap()).unwrap();
            }
            let old = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
            let modified = || std::fs::metadata(&theme).unwrap().modified().unwrap();

            for icon in [AppIcon::Green, AppIcon::Orange] {
                std::fs::File::open(&theme)
                    .unwrap()
                    .set_modified(old)
                    .unwrap();
                assert!(sync(&theme, icon).unwrap());
                assert!(modified() > old, "{icon:?}");
            }

            std::fs::File::open(&theme)
                .unwrap()
                .set_modified(old)
                .unwrap();
            assert!(!sync(&theme, AppIcon::Orange).unwrap());
            assert_eq!(modified(), old);
        }

        #[test]
        fn every_variant_has_distinct_files() {
            let variants: Vec<_> = AppIcon::ALL.into_iter().filter_map(files).collect();
            assert_eq!(variants.len(), AppIcon::ALL.len() - 1);
            for (index, first) in variants.iter().enumerate() {
                for second in &variants[index + 1..] {
                    assert_ne!(first[0], second[0]);
                    assert_ne!(first[1], second[1]);
                }
            }
        }
    }
}

#[cfg(all(test, desktop, not(feature = "flatpak")))]
mod tests {
    use super::*;
    use i_slint_core::accessibility::{AccessibilityAction, AccessibleStringProperty};
    use i_slint_core::item_tree::ItemRc;
    use i_slint_core::items::AccessibleRole;
    use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
    use std::{cell::Cell, ops::ControlFlow, rc::Rc};

    struct Headless(Rc<MinimalSoftwareWindow>);
    impl slint::platform::Platform for Headless {
        fn create_window_adapter(
            &self,
        ) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
            Ok(self.0.clone())
        }
    }

    /// The picker's choices, in order, with their checked state.
    fn choices(app: &AppWindow) -> Vec<(String, bool, ItemRc)> {
        let window = i_slint_core::window::WindowInner::from_pub(app.window());
        let root = ItemRc::new_root(window.component());
        let mut radios = Vec::new();
        root.visit_descendants(|item| {
            if item.accessible_role() == AccessibleRole::RadioButton && item.is_visible() {
                let label = item.accessible_string_property(AccessibleStringProperty::Label);
                let checked = item.accessible_string_property(AccessibleStringProperty::Checked);
                radios.push((
                    label.unwrap_or_default().to_string(),
                    checked.as_deref() == Some("true"),
                    item.clone(),
                ));
            }
            ControlFlow::<()>::Continue(())
        });
        // Theme cards come first and share some color names.
        let first = radios
            .iter()
            .position(|(label, ..)| label == "Orange")
            .expect("the app icon picker");
        radios.drain(..first);
        radios.truncate(models::AppIcon::ALL.len());
        radios
    }

    #[test]
    fn settings_show_the_chosen_icon_and_offer_every_variant() {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(Headless(window))).unwrap();
        let app = AppWindow::new().unwrap();
        app.window().set_size(slint::PhysicalSize::new(1320, 1100));
        let global = app.global::<crate::AppIcon>();
        for icon in models::AppIcon::ALL {
            assert_eq!(choice(variant(icon)), icon);
            let window_icon = global.invoke_window_image(variant(icon)).size();
            let tray_icon = global.invoke_tray_image(variant(icon)).size();
            assert_eq!(
                (window_icon.width, window_icon.height),
                (256, 256),
                "{icon:?}"
            );
            assert_eq!((tray_icon.width, tray_icon.height), (64, 64), "{icon:?}");
        }

        apply(&app, models::AppIcon::Graphite);
        assert!(global.get_available());
        assert_eq!(global.get_variant(), AppIconVariant::Graphite);
        let chosen = Rc::new(Cell::new(None));
        let target = Rc::clone(&chosen);
        global.on_choose(move |variant| target.set(Some(variant)));
        app.set_settings_tab("General".into());
        app.set_settings_open(true);
        app.show().unwrap();
        slint::platform::update_timers_and_animations();

        let choices = choices(&app);
        let labels: Vec<_> = choices.iter().map(|(label, ..)| label.as_str()).collect();
        assert_eq!(
            labels,
            ["Orange", "Graphite", "Light", "Blue", "Green", "Purple"]
        );
        let checked: Vec<_> = choices
            .iter()
            .filter(|(_, checked, _)| *checked)
            .map(|(label, ..)| label.as_str())
            .collect();
        assert_eq!(checked, ["Graphite"]);

        choices[3]
            .2
            .accessible_action(&AccessibilityAction::Default);
        assert_eq!(chosen.get(), Some(AppIconVariant::Blue));
    }
}
