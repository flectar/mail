//! Start Flectar Mail when the user signs in.
//!
//! The operating system's startup list is the source of truth. Users can also
//! remove or disable the entry from their desktop settings, Task Manager or
//! Login Items, so the preference is read back from there instead of being
//! mirrored in the settings database, where it could disagree with what
//! actually happens at the next sign-in. Only the Flatpak sandbox, which cannot
//! read the host's autostart directory, keeps its own record of the request.
//!
//! Entries launch the application with [`BACKGROUND_ARGUMENT`]. Startup keeps
//! the window hidden for that launch only when close-to-tray is enabled and a
//! tray is available; otherwise it opens the window as usual.
//!
//! In the Flatpak the same Background portal also decides whether the app may
//! keep running with its window closed, which is how it closes to the
//! background there: it has no tray icon.

use std::{io, path::PathBuf};

/// Marks a launch from the sign-in entry.
pub(crate) const BACKGROUND_ARGUMENT: &str = "--background";

/// Whether this installation can register a sign-in entry.
pub(crate) fn supported() -> bool {
    imp::supported()
}

/// The outcome of a successful change.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Applied {
    Done,
    /// Only the user can allow the entry, in the settings page that has been
    /// opened for them: Login Items on macOS, or Startup apps on Windows when
    /// they turned off the Microsoft Store build's startup task.
    #[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
    NeedsApproval,
}

pub(crate) fn launched_in_background() -> bool {
    is_background_launch(std::env::args_os().skip(1))
}

fn is_background_launch(mut arguments: impl Iterator<Item = std::ffi::OsString>) -> bool {
    arguments.any(|argument| argument == BACKGROUND_ARGUMENT)
}

/// A sign-in entry as the operating system currently records it.
#[derive(Debug, PartialEq, Eq)]
struct Entry {
    /// The executable the entry launches, when it was written by this module.
    executable: Option<PathBuf>,
    /// False when the user disabled the entry without removing it.
    enabled: bool,
}

pub(crate) fn is_enabled() -> io::Result<bool> {
    Ok(imp::read()?.is_some_and(|entry| entry.enabled))
}

pub(crate) fn set_enabled(enabled: bool) -> io::Result<Applied> {
    if enabled {
        imp::write(&launch_executable()?)
    } else {
        imp::remove().map(|()| Applied::Done)
    }
}

/// Re-point an enabled entry whose executable no longer exists, such as an
/// AppImage replaced by a newer file name. An entry that still launches some
/// existing installation is left alone, so running a second copy of the app
/// never silently takes over the user's sign-in entry.
pub(crate) fn repair() -> io::Result<()> {
    let Some(Entry {
        executable: Some(registered),
        enabled: true,
    }) = imp::read()?
    else {
        return Ok(());
    };
    if registered.exists() {
        return Ok(());
    }
    let current = launch_executable()?;
    if current == registered {
        return Ok(());
    }
    imp::write(&current).map(drop)
}

fn launch_executable() -> io::Result<PathBuf> {
    // An AppImage runs from a temporary mount; the entry must start the image.
    #[cfg(linux_desktop)]
    if let Some(image) = std::env::var_os("APPIMAGE")
        .map(PathBuf::from)
        .filter(|image| image.is_absolute())
    {
        return Ok(image);
    }
    let executable = std::env::current_exe()?;
    // Gatekeeper runs quarantined apps from a randomized read-only copy that
    // disappears after the app quits.
    #[cfg(target_os = "macos")]
    if executable
        .components()
        .any(|component| component.as_os_str() == "AppTranslocation")
    {
        return Err(io::Error::other(
            "move Flectar Mail to the Applications folder first",
        ));
    }
    Ok(executable)
}

/// Desktop entries are UTF-8 text.
#[cfg(all(linux_desktop, not(feature = "flatpak")))]
fn utf8(path: &std::path::Path) -> io::Result<&str> {
    path.to_str().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "the application path is not valid UTF-8",
        )
    })
}

#[cfg(all(linux_desktop, not(feature = "flatpak")))]
use xdg as imp;

/// XDG autostart entries, honored by GNOME, KDE Plasma, Xfce, Cinnamon and
/// other desktops that follow the Desktop Application Autostart
/// Specification.
#[cfg(all(linux_desktop, not(feature = "flatpak")))]
mod xdg {
    use super::{Applied, BACKGROUND_ARGUMENT, Entry, utf8};
    use std::{
        io::{self, Write},
        path::{Path, PathBuf},
    };

    const FILE_NAME: &str = "com.flectar.mail.desktop";
    /// Records the launched executable without parsing the quoted `Exec` line.
    const EXECUTABLE_KEY: &str = "X-Flectar-Executable";

    fn entry_path() -> io::Result<PathBuf> {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .filter(|path| path.is_absolute())
                    .map(|home| home.join(".config"))
            })
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "neither XDG_CONFIG_HOME nor HOME is set",
                )
            })?;
        Ok(config.join("autostart").join(FILE_NAME))
    }

    pub(super) fn supported() -> bool {
        true
    }

    pub(super) fn read() -> io::Result<Option<Entry>> {
        read_from(&entry_path()?)
    }

    pub(super) fn write(executable: &Path) -> io::Result<Applied> {
        write_to(&entry_path()?, executable)?;
        Ok(Applied::Done)
    }

    pub(super) fn remove() -> io::Result<()> {
        remove_from(&entry_path()?)
    }

    pub(super) fn read_from(path: &Path) -> io::Result<Option<Entry>> {
        let contents = match std::fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let mut in_main_group = false;
        let mut entry = Entry {
            executable: None,
            enabled: true,
        };
        for line in contents.lines().map(str::trim) {
            if line.starts_with('[') {
                in_main_group = line == "[Desktop Entry]";
                continue;
            }
            let Some((key, value)) = line.split_once('=').filter(|_| in_main_group) else {
                continue;
            };
            match (key.trim_end(), value.trim_start()) {
                ("Hidden", "true") | ("X-GNOME-Autostart-enabled", "false") => {
                    entry.enabled = false;
                }
                (EXECUTABLE_KEY, value) => {
                    entry.executable = Some(PathBuf::from(unescape_string(value)));
                }
                _ => {}
            }
        }
        Ok(Some(entry))
    }

    pub(super) fn write_to(path: &Path, executable: &Path) -> io::Result<()> {
        let executable = utf8(executable)?;
        let directory = path
            .parent()
            .ok_or_else(|| io::Error::other("the autostart path has no parent"))?;
        std::fs::create_dir_all(directory)?;
        let contents = format!(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=Flectar Mail\n\
             Comment=Start Flectar Mail when you sign in\n\
             Icon=com.flectar.mail\n\
             TryExec={try_exec}\n\
             Exec={exec} {BACKGROUND_ARGUMENT}\n\
             Terminal=false\n\
             X-GNOME-Autostart-enabled=true\n\
             {EXECUTABLE_KEY}={try_exec}\n",
            try_exec = escape_string(executable),
            exec = escape_string(&quote_exec_argument(executable)),
        );
        // Replace atomically so a crash never leaves a truncated entry that
        // the session manager would try to launch.
        let mut file = tempfile::NamedTempFile::new_in(directory)?;
        file.write_all(contents.as_bytes())?;
        file.as_file().sync_all()?;
        file.persist(path).map_err(|error| error.error)?;
        Ok(())
    }

    pub(super) fn remove_from(path: &Path) -> io::Result<()> {
        match std::fs::remove_file(path) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }

    /// Quote one `Exec` argument. Reserved characters inside double quotes
    /// are backslash-escaped, and `%` is doubled so it is not read as a field
    /// code. The general string escaping is applied afterwards.
    fn quote_exec_argument(argument: &str) -> String {
        let mut quoted = String::with_capacity(argument.len() + 2);
        quoted.push('"');
        for character in argument.chars() {
            match character {
                '"' | '`' | '$' | '\\' => {
                    quoted.push('\\');
                    quoted.push(character);
                }
                '%' => quoted.push_str("%%"),
                _ => quoted.push(character),
            }
        }
        quoted.push('"');
        quoted
    }

    fn escape_string(value: &str) -> String {
        let mut escaped = String::with_capacity(value.len());
        for character in value.chars() {
            match character {
                '\\' => escaped.push_str("\\\\"),
                '\n' => escaped.push_str("\\n"),
                '\t' => escaped.push_str("\\t"),
                '\r' => escaped.push_str("\\r"),
                _ => escaped.push(character),
            }
        }
        escaped
    }

    fn unescape_string(value: &str) -> String {
        let mut unescaped = String::with_capacity(value.len());
        let mut characters = value.chars();
        while let Some(character) = characters.next() {
            if character != '\\' {
                unescaped.push(character);
                continue;
            }
            match characters.next() {
                Some('n') => unescaped.push('\n'),
                Some('t') => unescaped.push('\t'),
                Some('r') => unescaped.push('\r'),
                Some('s') => unescaped.push(' '),
                Some(other) => unescaped.push(other),
                None => unescaped.push('\\'),
            }
        }
        unescaped
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn entry_round_trips_paths_that_need_quoting() {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("autostart").join(FILE_NAME);
            let executable = Path::new("/home/a b/50% \"$HOME\"\\Flectar.AppImage");

            assert_eq!(read_from(&path).unwrap(), None);
            write_to(&path, executable).unwrap();

            let contents = std::fs::read_to_string(&path).unwrap();
            assert!(contents.contains(
                "Exec=\"/home/a b/50%% \\\\\"\\\\$HOME\\\\\"\\\\\\\\Flectar.AppImage\" --background\n"
            ));
            assert_eq!(
                read_from(&path).unwrap(),
                Some(Entry {
                    executable: Some(executable.to_path_buf()),
                    enabled: true,
                })
            );

            remove_from(&path).unwrap();
            assert_eq!(read_from(&path).unwrap(), None);
            remove_from(&path).unwrap();
        }

        #[test]
        fn desktop_settings_can_disable_an_entry_without_removing_it() {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join(FILE_NAME);
            for disabled in ["Hidden=true", "X-GNOME-Autostart-enabled=false"] {
                std::fs::write(
                    &path,
                    format!("[Desktop Entry]\nExec=flectar-mail\n{disabled}\n"),
                )
                .unwrap();
                let entry = read_from(&path).unwrap().unwrap();
                assert!(!entry.enabled, "{disabled}");
                assert_eq!(entry.executable, None);
            }

            // Keys outside the main group do not describe the entry.
            std::fs::write(
                &path,
                "[Desktop Entry]\nExec=flectar-mail\n[Desktop Action x]\nHidden=true\n",
            )
            .unwrap();
            assert!(read_from(&path).unwrap().unwrap().enabled);
        }
    }
}

#[cfg(target_os = "windows")]
use windows_startup as imp;

/// Windows keeps a packaged app's registry writes private to its package, so
/// the Microsoft Store build uses the startup task that its manifest
/// declares. The installer and portable builds use the `Run` key.
#[cfg(target_os = "windows")]
mod windows_startup {
    use super::{Applied, Entry, registry, startup_task};
    use std::{io, path::Path};

    pub(super) fn supported() -> bool {
        true
    }

    pub(super) fn read() -> io::Result<Option<Entry>> {
        if packaged() {
            startup_task::read().map(Some)
        } else {
            registry::read()
        }
    }

    pub(super) fn write(executable: &Path) -> io::Result<Applied> {
        if packaged() {
            startup_task::enable()
        } else {
            registry::write(executable)
        }
    }

    pub(super) fn remove() -> io::Result<()> {
        if packaged() {
            startup_task::disable()
        } else {
            registry::remove()
        }
    }

    /// Whether the process has package identity, as the Store build does.
    fn packaged() -> bool {
        use windows_sys::Win32::{
            Foundation::ERROR_INSUFFICIENT_BUFFER,
            Storage::Packaging::Appx::GetCurrentPackageFullName,
        };
        let mut length = 0;
        // SAFETY: a zero length with no buffer only asks for the name's size,
        // which an unpackaged process does not have.
        let status = unsafe { GetCurrentPackageFullName(&mut length, std::ptr::null_mut()) };
        status == ERROR_INSUFFICIENT_BUFFER
    }
}

/// The Microsoft Store package's startup task. Windows lists it under
/// Startup apps in Settings and Task Manager, and launches the packaged
/// executable with the task's `--background` parameter.
#[cfg(target_os = "windows")]
mod startup_task {
    use super::{Applied, Entry};
    use std::io;
    use windows::{
        ApplicationModel::{StartupTask, StartupTaskState},
        core::h,
    };

    fn task() -> io::Result<StartupTask> {
        // The TaskId that the Store package manifest declares.
        Ok(StartupTask::GetAsync(h!("FlectarMailStartup"))?.join()?)
    }

    pub(super) fn read() -> io::Result<Entry> {
        let state = task()?.State()?;
        Ok(Entry {
            // The package always launches its own executable.
            executable: None,
            enabled: matches!(
                state,
                StartupTaskState::Enabled | StartupTaskState::EnabledByPolicy
            ),
        })
    }

    pub(super) fn enable() -> io::Result<Applied> {
        let task = task()?;
        let mut state = task.State()?;
        if state == StartupTaskState::Disabled {
            // Packaged desktop apps enable their task without a prompt.
            state = task.RequestEnableAsync()?.join()?;
        }
        match state {
            StartupTaskState::Enabled | StartupTaskState::EnabledByPolicy => Ok(Applied::Done),
            // Apps cannot override a user who turned the task off; take them
            // to the switch instead.
            StartupTaskState::DisabledByUser => {
                open_startup_settings()?;
                Ok(Applied::NeedsApproval)
            }
            _ => Err(io::Error::other(
                "an administrator has turned off startup apps",
            )),
        }
    }

    pub(super) fn disable() -> io::Result<()> {
        Ok(task()?.Disable()?)
    }

    fn open_startup_settings() -> io::Result<()> {
        use windows_sys::Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL};
        let page: Vec<u16> = "ms-settings:startupapps\0".encode_utf16().collect();
        // SAFETY: `page` is a live, NUL-terminated UTF-16 string, and every
        // other pointer argument may be null.
        let result = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                std::ptr::null(),
                page.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        // Values above 32 report success.
        if result as usize > 32 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

/// The per-user `Run` key, which Task Manager's Startup apps page lists and
/// can disable through the adjacent `StartupApproved` key.
#[cfg(target_os = "windows")]
mod registry {
    use super::{Applied, BACKGROUND_ARGUMENT, Entry};
    use std::{
        ffi::{OsStr, OsString},
        io,
        os::windows::ffi::{OsStrExt, OsStringExt},
        path::{Path, PathBuf},
        ptr::null_mut,
    };
    use windows_sys::Win32::{
        Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR},
        System::Registry::{
            HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_BINARY, RRF_RT_REG_SZ, RegDeleteKeyValueW,
            RegGetValueW, RegSetKeyValueW,
        },
    };

    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const APPROVED_KEY: &str =
        r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
    const VALUE_NAME: &str = "Flectar Mail";

    pub(super) fn read() -> io::Result<Option<Entry>> {
        let Some(command) = get_value(RUN_KEY, RRF_RT_REG_SZ)? else {
            return Ok(None);
        };
        // Task Manager stores an odd first byte for disabled entries.
        let enabled = get_value(APPROVED_KEY, RRF_RT_REG_BINARY)?
            .is_none_or(|flags| flags.first().is_none_or(|flags| flags & 1 == 0));
        let command = OsString::from_wide(
            &command
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&unit| u16::from_ne_bytes(unit))
                .take_while(|&unit| unit != 0)
                .collect::<Vec<_>>(),
        );
        Ok(Some(Entry {
            executable: command_executable(&command.to_string_lossy()),
            enabled,
        }))
    }

    pub(super) fn write(executable: &Path) -> io::Result<Applied> {
        let command = command_line(executable);
        let data = wide(&command);
        // SAFETY: every pointer refers to a live, NUL-terminated UTF-16
        // buffer, and the byte count covers `data` including its terminator.
        check(unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                wide(OsStr::new(RUN_KEY)).as_ptr(),
                wide(OsStr::new(VALUE_NAME)).as_ptr(),
                REG_SZ,
                data.as_ptr().cast(),
                u32::try_from(data.len() * 2).map_err(io::Error::other)?,
            )
        })?;
        // Re-enabling from the app must also clear a Task Manager override.
        delete_value(APPROVED_KEY)?;
        Ok(Applied::Done)
    }

    pub(super) fn remove() -> io::Result<()> {
        delete_value(RUN_KEY)?;
        delete_value(APPROVED_KEY)
    }

    fn command_line(executable: &Path) -> OsString {
        // Windows paths cannot contain quotes, so quoting is unambiguous.
        let mut command = OsString::from("\"");
        command.push(executable);
        command.push("\" ");
        command.push(BACKGROUND_ARGUMENT);
        command
    }

    fn command_executable(command: &str) -> Option<PathBuf> {
        let executable = match command.strip_prefix('"') {
            Some(quoted) => quoted.split_once('"')?.0,
            None => command.split_whitespace().next()?,
        };
        (!executable.is_empty()).then(|| PathBuf::from(executable))
    }

    fn get_value(subkey: &str, kind: u32) -> io::Result<Option<Vec<u8>>> {
        let subkey = wide(OsStr::new(subkey));
        let name = wide(OsStr::new(VALUE_NAME));
        loop {
            let mut size = 0_u32;
            // SAFETY: a null data pointer asks only for the required size.
            let status = unsafe {
                RegGetValueW(
                    HKEY_CURRENT_USER,
                    subkey.as_ptr(),
                    name.as_ptr(),
                    kind,
                    null_mut(),
                    null_mut(),
                    &mut size,
                )
            };
            if status == ERROR_FILE_NOT_FOUND {
                return Ok(None);
            }
            check(status)?;
            let mut data = vec![0_u8; size as usize];
            // SAFETY: `data` is writable for `size` bytes, as passed.
            let status = unsafe {
                RegGetValueW(
                    HKEY_CURRENT_USER,
                    subkey.as_ptr(),
                    name.as_ptr(),
                    kind,
                    null_mut(),
                    data.as_mut_ptr().cast(),
                    &mut size,
                )
            };
            match status {
                ERROR_FILE_NOT_FOUND => return Ok(None),
                // The value grew between the two calls; ask again.
                windows_sys::Win32::Foundation::ERROR_MORE_DATA => continue,
                status => check(status)?,
            }
            data.truncate(size as usize);
            return Ok(Some(data));
        }
    }

    fn delete_value(subkey: &str) -> io::Result<()> {
        // SAFETY: both strings are live, NUL-terminated UTF-16 buffers.
        let status = unsafe {
            RegDeleteKeyValueW(
                HKEY_CURRENT_USER,
                wide(OsStr::new(subkey)).as_ptr(),
                wide(OsStr::new(VALUE_NAME)).as_ptr(),
            )
        };
        if status == ERROR_FILE_NOT_FOUND {
            return Ok(());
        }
        check(status)
    }

    fn check(status: WIN32_ERROR) -> io::Result<()> {
        if status == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(status as i32))
        }
    }

    fn wide(value: &OsStr) -> Vec<u16> {
        value.encode_wide().chain(Some(0)).collect()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn command_line_round_trips_the_executable() {
            let executable = Path::new(r"C:\Users\A B\AppData\Local\Programs\Flectar Mail\flectar-mail.exe");
            let command = command_line(executable);
            assert_eq!(
                command.to_str().unwrap(),
                format!("\"{}\" --background", executable.display())
            );
            assert_eq!(
                command_executable(command.to_str().unwrap()).as_deref(),
                Some(executable)
            );
            assert_eq!(
                command_executable(r"C:\flectar.exe --background").as_deref(),
                Some(Path::new(r"C:\flectar.exe"))
            );
            assert_eq!(command_executable("\"unterminated"), None);
        }
    }
}

#[cfg(target_os = "macos")]
use login_item as imp;

/// A launch agent inside the application bundle, registered through
/// `SMAppService`. macOS lists it under Login Items attributed to Flectar
/// Mail, may ask the user to approve it, and reports when the user turns it
/// off there. `scripts/package-macos.sh` installs
/// `platform/macos/com.flectar.mail.login.plist` into
/// `Contents/Library/LaunchAgents`, so it can only be registered from a
/// bundle.
#[cfg(target_os = "macos")]
mod login_item {
    use super::{Applied, Entry};
    use objc2::rc::Retained;
    use objc2_foundation::NSString;
    use objc2_service_management::{SMAppService, SMAppServiceStatus};
    use std::{io, path::Path};

    const AGENT: &str = "com.flectar.mail.login.plist";

    fn service() -> Retained<SMAppService> {
        // SAFETY: the name refers to a property list in the bundle's
        // Contents/Library/LaunchAgents directory.
        unsafe { SMAppService::agentServiceWithPlistName(&NSString::from_str(AGENT)) }
    }

    fn status() -> SMAppServiceStatus {
        // SAFETY: a status query on a valid service object.
        unsafe { service().status() }
    }

    pub(super) fn supported() -> bool {
        let Ok(executable) = std::env::current_exe() else {
            return false;
        };
        executable
            .parent()
            .filter(|macos| macos.ends_with("Contents/MacOS"))
            .and_then(Path::parent)
            .is_some_and(|contents| contents.join("Library/LaunchAgents").join(AGENT).is_file())
    }

    pub(super) fn read() -> io::Result<Option<Entry>> {
        // Approval pending means it will not run at the next sign-in yet.
        Ok((status() == SMAppServiceStatus::Enabled).then_some(Entry {
            executable: None,
            enabled: true,
        }))
    }

    pub(super) fn write(_executable: &Path) -> io::Result<Applied> {
        let service = service();
        // SAFETY: registration of a valid service object; failures are
        // returned as NSError.
        unsafe { service.registerAndReturnError() }.map_err(error)?;
        // SAFETY: as above.
        if unsafe { service.status() } == SMAppServiceStatus::RequiresApproval {
            // SAFETY: opens System Settings; no arguments.
            unsafe { SMAppService::openSystemSettingsLoginItems() };
            return Ok(Applied::NeedsApproval);
        }
        Ok(Applied::Done)
    }

    pub(super) fn remove() -> io::Result<()> {
        let status = status();
        if status == SMAppServiceStatus::NotRegistered || status == SMAppServiceStatus::NotFound {
            return Ok(());
        }
        // SAFETY: unregistration of a valid service object; failures are
        // returned as NSError.
        unsafe { service().unregisterAndReturnError() }.map_err(error)
    }

    fn error(error: Retained<objc2_foundation::NSError>) -> io::Error {
        io::Error::other(error.localizedDescription().to_string())
    }
}

#[cfg(all(linux_desktop, feature = "flatpak"))]
use portal as imp;

/// Whether the desktop lets the Flatpak keep running with its window closed.
/// Called on the blocking pool; the sign-in entry stays as it is.
#[cfg(all(linux_desktop, feature = "flatpak"))]
pub(crate) fn allow_background() -> io::Result<bool> {
    portal::allow_background()
}

/// Tell the desktop what the Flatpak does while its window is closed. GNOME
/// shows it under the app in Background Apps.
#[cfg(all(linux_desktop, feature = "flatpak"))]
pub(crate) async fn set_background_status(message: String) {
    portal::set_status(message).await;
}

/// The Background portal writes the host's autostart entry on the sandbox's
/// behalf. It offers no way to read that entry back, so the last granted
/// request is recorded in the data directory.
#[cfg(all(linux_desktop, feature = "flatpak"))]
mod portal {
    use super::{Applied, BACKGROUND_ARGUMENT, Entry};
    use ashpd::desktop::{
        ResponseError,
        background::{Background, BackgroundProxy},
    };
    use std::{
        io,
        path::{Path, PathBuf},
        sync::{Mutex, PoisonError},
    };

    const SIGN_IN_REASON: &str = "Start Flectar Mail when you sign in";
    const BACKGROUND_REASON: &str = "Check for new mail while the window is closed";

    /// Every request also rewrites the sign-in entry from the record, so one
    /// must not run between another's reading and writing it.
    static REQUESTS: Mutex<()> = Mutex::new(());

    fn record() -> io::Result<PathBuf> {
        let paths = flectar_mail_core::config::Paths::default_dirs()
            .map_err(|error| io::Error::other(error.to_string()))?;
        Ok(paths.data_dir.join("launch-at-login"))
    }

    pub(super) fn supported() -> bool {
        true
    }

    pub(super) fn read() -> io::Result<Option<Entry>> {
        Ok(record()?.is_file().then_some(Entry {
            executable: None,
            enabled: true,
        }))
    }

    /// Ask to run in the background, starting at sign-in or not; `None` when
    /// the desktop refuses. Called on the blocking pool, outside any async
    /// task.
    fn request(reason: &str, auto_start: bool) -> io::Result<Option<Background>> {
        let request = Background::request()
            .reason(reason)
            .auto_start(auto_start)
            .command(["flectar-mail", BACKGROUND_ARGUMENT])
            .dbus_activatable(false);
        let response =
            tokio::runtime::Handle::current().block_on(async { request.send().await?.response() });
        match response {
            Ok(background) => Ok(Some(background)),
            // The user, or a permission they set earlier, said no.
            Err(ashpd::Error::Response(ResponseError::Cancelled)) => Ok(None),
            Err(error) => Err(io::Error::other(error.to_string())),
        }
    }

    pub(super) fn allow_background() -> io::Result<bool> {
        let _requests = REQUESTS.lock().unwrap_or_else(PoisonError::into_inner);
        let auto_start = read()?.is_some();
        Ok(request(BACKGROUND_REASON, auto_start)?
            .is_some_and(|background| background.run_in_background()))
    }

    /// Portals before version 2 have no status; the desktop then lists the
    /// app by its name alone.
    pub(super) async fn set_status(message: String) {
        if let Ok(proxy) = BackgroundProxy::new().await {
            let _ = proxy.set_status(&message).await;
        }
    }

    pub(super) fn write(_executable: &Path) -> io::Result<Applied> {
        let _requests = REQUESTS.lock().unwrap_or_else(PoisonError::into_inner);
        if !request(SIGN_IN_REASON, true)?.is_some_and(|background| background.auto_start()) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the desktop did not allow starting at sign-in",
            ));
        }
        let record = record()?;
        if let Some(directory) = record.parent() {
            std::fs::create_dir_all(directory)?;
        }
        std::fs::write(record, b"")?;
        Ok(Applied::Done)
    }

    pub(super) fn remove() -> io::Result<()> {
        let _requests = REQUESTS.lock().unwrap_or_else(PoisonError::into_inner);
        // A refused request leaves the entry in place.
        if request(SIGN_IN_REASON, false)?.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the desktop did not allow changing the sign-in entry",
            ));
        }
        match std::fs::remove_file(record()?) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }
}

#[cfg(not(any(linux_desktop, target_os = "windows", target_os = "macos")))]
mod imp {
    use super::{Applied, Entry};
    use std::{io, path::Path};

    pub(super) fn supported() -> bool {
        false
    }

    pub(super) fn read() -> io::Result<Option<Entry>> {
        Ok(None)
    }

    pub(super) fn write(_executable: &Path) -> io::Result<Applied> {
        Err(io::ErrorKind::Unsupported.into())
    }

    pub(super) fn remove() -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_exact_background_argument_requests_a_hidden_start() {
        let launch = |arguments: &[&str]| {
            is_background_launch(arguments.iter().map(std::ffi::OsString::from))
        };
        assert!(launch(&["--background"]));
        assert!(launch(&["--verbose", "--background"]));
        assert!(!launch(&[]));
        assert!(!launch(&["--background=1", "background"]));
    }
}
