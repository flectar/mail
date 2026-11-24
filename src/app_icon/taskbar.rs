//! The chosen icon on the Windows taskbar, for a copy without package
//! identity.
//!
//! The taskbar draws a button from a shortcut rather than its window when a
//! shortcut describes the app: from the Start menu shortcut that carries the
//! copy's app id, and a pin, itself a shortcut, from its own icon. So the
//! executable embeds every icon (build.rs), and, as Firefox does for its own
//! icons, the shortcuts of this copy name the chosen one: its Start menu
//! shortcut, its pins and its shortcuts on the desktop, the ones that carry
//! its app id and start its executable.
//!
//! A portable copy has no Start menu shortcut, so its button shows the
//! window's icon. Its window also tells the taskbar what a new pin starts and
//! shows (relaunch properties), as Chrome's profile windows do; otherwise the
//! pin would start the executable with its first icon.

use flectar_mail_core::models::AppIcon;

/// The executable's icon resource for `icon` (build.rs): the default icon is
/// the application icon, 1, and the others follow in the order of
/// [`AppIcon`].
fn resource_id(icon: AppIcon) -> u16 {
    match icon {
        AppIcon::Orange => 1,
        AppIcon::Graphite => 2,
        AppIcon::Light => 3,
        AppIcon::Blue => 4,
        AppIcon::Green => 5,
        AppIcon::Purple => 6,
    }
}

/// How a shortcut names `icon` of the executable: the default icon by its
/// position, first, as a shortcut without an icon of its own shows it, and
/// the others by their resource id, which a negative index stands for.
fn icon_index(icon: AppIcon) -> i32 {
    match resource_id(icon) {
        1 => 0,
        id => -i32::from(id),
    }
}

/// Whether a shortcut whose icon is the one at `index` in the file `path`
/// shows the icon at `wanted` in `executable`.
fn shows_icon(path: &str, index: i32, executable: &str, wanted: i32) -> bool {
    if path.is_empty() {
        // A shortcut without an icon of its own shows its target's first.
        return wanted == 0;
    }
    path.eq_ignore_ascii_case(executable)
        && (index == wanted || (wanted == 0 && index == -i32::from(resource_id(AppIcon::Orange))))
}

/// The icon a pin of a portable copy's window takes, by resource id.
fn relaunch_icon(executable: &str, icon: AppIcon) -> String {
    format!("{executable},-{}", resource_id(icon))
}

#[cfg(target_os = "windows")]
pub(super) use system::{describe_window, forget_window, offered, show};

#[cfg(target_os = "windows")]
mod system {
    use super::{AppIcon, icon_index, relaunch_icon, shows_icon};
    use crate::notifications::Unpackaged;
    use crate::platform::windows_install::same_file;
    use std::path::{Path, PathBuf};
    use std::sync::{OnceLock, mpsc};
    use windows::Win32::Foundation::{HWND, MAX_PATH, PROPERTYKEY};
    use windows::Win32::Storage::EnhancedStorage::{
        PKEY_AppUserModel_ID, PKEY_AppUserModel_RelaunchCommand,
        PKEY_AppUserModel_RelaunchDisplayNameResource, PKEY_AppUserModel_RelaunchIconResource,
    };
    use windows::Win32::System::Com::StructuredStorage::{
        PROPVARIANT, PropVariantClear, PropVariantToStringAlloc,
    };
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
        CoTaskMemFree, IPersistFile, STGM, STGM_READ, STGM_READWRITE,
    };
    use windows::Win32::System::Variant::VT_LPWSTR;
    use windows::Win32::UI::Shell::PropertiesSystem::{
        IPropertyStore, SHGetPropertyStoreForWindow,
    };
    use windows::Win32::UI::Shell::{
        FOLDERID_Desktop, FOLDERID_Programs, FOLDERID_RoamingAppData, IShellLinkW, KF_FLAG_DEFAULT,
        SHCNE_ASSOCCHANGED, SHCNE_UPDATEITEM, SHCNF_IDLIST, SHCNF_PATHW, SHChangeNotify,
        SHGetKnownFolderPath, SHStrDupW, ShellLink,
    };
    use windows::core::{GUID, HSTRING, Interface};

    /// Where the taskbar keeps the user's pins, under the roaming app data.
    const PINS: &str = r"Microsoft\Internet Explorer\Quick Launch\User Pinned\TaskBar";

    /// This copy, unless it runs from the Store package, whose manifest
    /// names the icons of its shortcuts and taskbar button.
    fn copy() -> Option<&'static Unpackaged> {
        static COPY: OnceLock<Option<Unpackaged>> = OnceLock::new();
        COPY.get_or_init(crate::notifications::unpackaged).as_ref()
    }

    /// Whether the taskbar can show the chosen icon: not for the Store
    /// package, which therefore doesn't offer the choice.
    pub(in crate::app_icon) fn offered() -> bool {
        copy().is_some()
    }

    /// Name `icon` in this copy's shortcuts. It happens off the UI thread,
    /// since shortcuts are files and Explorer answers in its own time, and
    /// only the latest choice is applied.
    pub(in crate::app_icon) fn show(icon: AppIcon) {
        static WORKER: OnceLock<Option<mpsc::Sender<AppIcon>>> = OnceLock::new();
        let worker = WORKER.get_or_init(|| {
            let app_id = copy()?.app_id;
            let (sender, choices) = mpsc::channel();
            let started = std::thread::Builder::new()
                .name("flectar-app-icon".into())
                .spawn(move || {
                    // SAFETY: this thread's own apartment, kept for its life.
                    if let Err(error) =
                        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.ok()
                    {
                        tracing::warn!(%error, "app icon: COM is unavailable");
                        return;
                    }
                    while let Ok(mut icon) = choices.recv() {
                        while let Ok(later) = choices.try_recv() {
                            icon = later;
                        }
                        name_icon(app_id, icon);
                    }
                });
            if let Err(error) = started {
                tracing::warn!(%error, "app icon: could not start the shortcut thread");
                return None;
            }
            Some(sender)
        });
        if let Some(worker) = worker {
            let _ = worker.send(icon);
        }
    }

    /// Point every shortcut of this copy at `icon`.
    fn name_icon(app_id: &str, icon: AppIcon) {
        let Ok(executable) = std::env::current_exe() else {
            return;
        };
        let index = icon_index(icon);
        let mut changed = false;
        for folder in shortcut_folders() {
            let Ok(entries) = std::fs::read_dir(&folder) else {
                continue;
            };
            for path in entries.flatten().map(|entry| entry.path()) {
                if !path
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("lnk"))
                {
                    continue;
                }
                // SAFETY: this thread initialized COM.
                match unsafe { name_icon_in(&path, app_id, &executable, index) } {
                    Ok(updated) => changed |= updated,
                    Err(error) => tracing::debug!(
                        %error,
                        shortcut = %path.file_name().unwrap_or_default().to_string_lossy(),
                        "app icon: could not update a shortcut"
                    ),
                }
            }
        }
        if changed {
            // SAFETY: an event without items, after which Explorer reloads
            // the icons it shows.
            unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None) };
        }
    }

    /// The desktop and Start menu folders of the user, and their pins. The
    /// Start menu folder of all users belongs to them, and isn't changed.
    fn shortcut_folders() -> Vec<PathBuf> {
        let mut folders = [&FOLDERID_Desktop, &FOLDERID_Programs]
            .into_iter()
            .filter_map(known_folder)
            .collect::<Vec<_>>();
        folders.extend(known_folder(&FOLDERID_RoamingAppData).map(|roaming| roaming.join(PINS)));
        folders
    }

    fn known_folder(id: &GUID) -> Option<PathBuf> {
        // SAFETY: a known folder's id; the path the call allocates is freed
        // below.
        let path = unsafe { SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None) }.ok()?;
        // SAFETY: a NUL-terminated string from the call above.
        let folder = unsafe { path.to_string() }.ok().map(PathBuf::from);
        // SAFETY: allocated by SHGetKnownFolderPath with the task allocator.
        unsafe { CoTaskMemFree(Some(path.0.cast())) };
        folder
    }

    fn open_shortcut(path: &HSTRING, mode: STGM) -> windows::core::Result<IShellLinkW> {
        // SAFETY: COM is initialized on the calling thread.
        let link: IShellLinkW =
            unsafe { CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER) }?;
        // SAFETY: a NUL-terminated path.
        unsafe { link.cast::<IPersistFile>()?.Load(path, mode) }?;
        Ok(link)
    }

    /// Point the shortcut at `path` at the icon at `index` in `executable`,
    /// when it is one of this copy's: it carries `app_id` and starts
    /// `executable`. Tells whether it changed.
    ///
    /// # Safety
    ///
    /// COM must be initialized on the calling thread.
    unsafe fn name_icon_in(
        path: &Path,
        app_id: &str,
        executable: &Path,
        index: i32,
    ) -> windows::core::Result<bool> {
        let path = HSTRING::from(path.as_os_str());
        let link = open_shortcut(&path, STGM_READ)?;
        // SAFETY: COM is initialized, as the caller ensures.
        if unsafe { string_property(&link.cast()?, &PKEY_AppUserModel_ID) }.as_deref()
            != Some(app_id)
        {
            return Ok(false);
        }
        let mut target = [0_u16; MAX_PATH as usize];
        // SAFETY: the buffer's length goes with it; no find data is wanted.
        unsafe { link.GetPath(&mut target, std::ptr::null_mut(), 0) }?;
        if !same_file(Path::new(&wide(&target)), executable) {
            return Ok(false);
        }
        let mut location = [0_u16; MAX_PATH as usize];
        let mut current = 0;
        // SAFETY: the buffer's length goes with it.
        unsafe { link.GetIconLocation(&mut location, &mut current) }?;
        let executable = executable.to_string_lossy();
        if shows_icon(&wide(&location), current, &executable, index) {
            return Ok(false);
        }
        // Opened again to be written, as only this copy's shortcuts are.
        let link = open_shortcut(&path, STGM_READWRITE)?;
        // SAFETY: a NUL-terminated path, and the shortcut's file name.
        unsafe {
            link.SetIconLocation(&HSTRING::from(executable.as_ref()), index)?;
            link.cast::<IPersistFile>()?.Save(&path, true)?;
            SHChangeNotify(
                SHCNE_UPDATEITEM,
                SHCNF_PATHW,
                Some(path.as_ptr().cast()),
                None,
            );
        }
        Ok(true)
    }

    /// A string property of a shortcut, empty when it has none.
    ///
    /// # Safety
    ///
    /// COM must be initialized on the calling thread.
    unsafe fn string_property(store: &IPropertyStore, key: &PROPERTYKEY) -> Option<String> {
        // SAFETY: a valid key; the value is cleared below.
        let mut value = unsafe { store.GetValue(key) }.ok()?;
        // SAFETY: the value read above.
        let text = unsafe { PropVariantToStringAlloc(&value) };
        // SAFETY: the value read above, cleared once.
        let _ = unsafe { PropVariantClear(&mut value) };
        let text = text.ok()?;
        // SAFETY: a NUL-terminated string from PropVariantToStringAlloc.
        let string = unsafe { text.to_string() }.ok();
        // SAFETY: allocated by PropVariantToStringAlloc with the task allocator.
        unsafe { CoTaskMemFree(Some(text.0.cast())) };
        string
    }

    /// The text up to the end of a NUL-terminated buffer.
    fn wide(buffer: &[u16]) -> String {
        let end = buffer
            .iter()
            .position(|&unit| unit == 0)
            .unwrap_or(buffer.len());
        String::from_utf16_lossy(&buffer[..end])
    }

    /// Tell the taskbar what a pin of `window` starts and shows, for a
    /// portable copy: it has no Start menu shortcut that a pin would copy.
    /// The app id goes last, since the taskbar takes the others when it
    /// changes. Called on the window's thread.
    pub(in crate::app_icon) fn describe_window(
        window: windows_sys::Win32::Foundation::HWND,
        icon: AppIcon,
    ) {
        let Some(copy) = copy().filter(|copy| copy.portable) else {
            return;
        };
        let Ok(executable) = std::env::current_exe() else {
            return;
        };
        let executable = executable.to_string_lossy();
        let properties = [
            (
                &PKEY_AppUserModel_RelaunchCommand,
                Some(format!("\"{executable}\"")),
            ),
            (
                &PKEY_AppUserModel_RelaunchDisplayNameResource,
                Some("Flectar Mail".to_owned()),
            ),
            (
                &PKEY_AppUserModel_RelaunchIconResource,
                Some(relaunch_icon(&executable, icon)),
            ),
            (&PKEY_AppUserModel_ID, Some(copy.app_id.to_owned())),
        ];
        // SAFETY: a window of this thread, whose apartment winit initialized.
        if let Err(error) = unsafe { set_window_properties(window, &properties) } {
            tracing::debug!(%error, "app icon: could not describe the window to the taskbar");
        }
    }

    /// Take back what [`describe_window`] told the taskbar, before `window`
    /// is destroyed, as Windows asks.
    pub(in crate::app_icon) fn forget_window(window: windows_sys::Win32::Foundation::HWND) {
        if !copy().is_some_and(|copy| copy.portable) {
            return;
        }
        let properties = [
            (&PKEY_AppUserModel_ID, None),
            (&PKEY_AppUserModel_RelaunchCommand, None),
            (&PKEY_AppUserModel_RelaunchDisplayNameResource, None),
            (&PKEY_AppUserModel_RelaunchIconResource, None),
        ];
        // SAFETY: a window of this thread, still alive.
        if let Err(error) = unsafe { set_window_properties(window, &properties) } {
            tracing::debug!(%error, "app icon: could not clear the window's taskbar properties");
        }
    }

    /// Set each property of `window` to its string, or clear it.
    ///
    /// # Safety
    ///
    /// `window` must be a live window of this thread, and COM initialized on
    /// it.
    unsafe fn set_window_properties(
        window: windows_sys::Win32::Foundation::HWND,
        properties: &[(&PROPERTYKEY, Option<String>)],
    ) -> windows::core::Result<()> {
        // SAFETY: a live window, as the caller ensures.
        let store: IPropertyStore = unsafe { SHGetPropertyStoreForWindow(HWND(window)) }?;
        for (key, value) in properties {
            let mut variant = match value {
                // SAFETY: COM is initialized; the string is freed with the
                // variant.
                Some(text) => unsafe { string_variant(text) }?,
                None => PROPVARIANT::default(),
            };
            // SAFETY: a valid key and value.
            let result = unsafe { store.SetValue(*key, &variant) };
            // SAFETY: the value made above, cleared once.
            let _ = unsafe { PropVariantClear(&mut variant) };
            result?;
        }
        // SAFETY: the store read above.
        unsafe { store.Commit() }
    }

    /// A string value as the shell's properties take it: VT_LPWSTR, in
    /// memory of the task allocator, which PropVariantClear frees.
    ///
    /// # Safety
    ///
    /// COM must be initialized on the calling thread.
    unsafe fn string_variant(text: &str) -> windows::core::Result<PROPVARIANT> {
        // SAFETY: a NUL-terminated string, copied with the task allocator.
        let copy = unsafe { SHStrDupW(&HSTRING::from(text)) }?;
        let mut value = PROPVARIANT::default();
        // SAFETY: the variant takes the copy, as a string variant does.
        unsafe {
            let inner = &mut *value.Anonymous.Anonymous;
            inner.vt = VT_LPWSTR;
            inner.Anonymous.pwszVal = copy;
        }
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ICONS: [AppIcon; 6] = [
        AppIcon::Orange,
        AppIcon::Graphite,
        AppIcon::Light,
        AppIcon::Blue,
        AppIcon::Green,
        AppIcon::Purple,
    ];

    #[test]
    fn the_executable_embeds_every_icon_under_its_id() {
        let build = include_str!("../../build.rs");
        assert!(build.contains(r#".set_icon("resources/app-icon/flectar-mail.ico")"#));
        for icon in ICONS.into_iter().skip(1) {
            let name = format!("{icon:?}").to_lowercase();
            assert!(
                build.contains(&format!("({}, \"{name}\")", resource_id(icon))),
                "build.rs embeds {name} as resource {}",
                resource_id(icon)
            );
        }
    }

    #[test]
    fn shortcuts_name_the_default_icon_by_position_and_the_others_by_id() {
        let indices = ICONS.map(icon_index);
        assert_eq!(indices, [0, -2, -3, -4, -5, -6]);
        let executable = r"C:\Users\Ada\AppData\Local\Programs\Flectar Mail\flectar-mail.exe";
        assert_eq!(
            relaunch_icon(executable, AppIcon::Green),
            format!("{executable},-5")
        );
        assert_eq!(
            relaunch_icon(executable, AppIcon::Orange),
            format!("{executable},-1")
        );
    }

    #[test]
    fn a_shortcut_already_showing_the_icon_is_left_alone() {
        let executable = r"C:\Programs\Flectar Mail\flectar-mail.exe";
        // As the setup program writes it: no icon of its own.
        assert!(shows_icon("", 0, executable, 0));
        assert!(!shows_icon("", 0, executable, -5));
        assert!(shows_icon(executable, 0, executable, 0));
        assert!(shows_icon(executable, -1, executable, 0));
        assert!(shows_icon(&executable.to_uppercase(), -5, executable, -5));
        assert!(!shows_icon(executable, -4, executable, -5));
        assert!(!shows_icon(
            r"C:\Other\flectar-mail.exe",
            -5,
            executable,
            -5
        ));
    }
}
