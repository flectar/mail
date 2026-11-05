//! Windows: the Default Programs registration that lists an email app under
//! MAILTO in Settings > Apps > Default apps, as Thunderbird and Mailspring
//! write it. Windows has no supported way for an app to make itself the
//! default; only the user can, in that page.

use super::{Applied, Done, State, Status};
use std::{
    ffi::{OsStr, OsString},
    io,
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
    ptr::{null, null_mut},
};
use windows_sys::Win32::{
    Foundation::{
        APPMODEL_ERROR_NO_PACKAGE, ERROR_FILE_NOT_FOUND, ERROR_INSUFFICIENT_BUFFER,
        ERROR_MORE_DATA, ERROR_SUCCESS, WIN32_ERROR,
    },
    Storage::Packaging::Appx::GetCurrentPackageFamilyName,
    System::Registry::{
        HKEY, HKEY_CLASSES_ROOT, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, REG_SZ, RRF_RT_REG_SZ,
        RegDeleteKeyValueW, RegDeleteTreeW, RegGetValueW, RegSetKeyValueW,
    },
    UI::{
        Shell::{
            ASSOCF_IS_PROTOCOL, ASSOCSTR_PROGID, AssocQueryStringW, SHCNE_ASSOCCHANGED,
            SHCNF_IDLIST, SHChangeNotify, ShellExecuteW,
        },
        WindowsAndMessaging::SW_SHOWNORMAL,
    },
};

/// One email app registration under `Software\Clients\Mail`.
struct Registration {
    /// Shown in Default apps; also the `RegisteredApplications` value name,
    /// which the Settings link names.
    name: &'static str,
    prog_id: &'static str,
}

/// What platform/windows/installer.iss writes for an installed copy.
const INSTALLED: Registration = Registration {
    name: "Flectar Mail",
    prog_id: "FlectarMail.Url.mailto",
};

/// A portable copy registers under names of its own, so it never replaces
/// or removes the installed copy's registration.
const PORTABLE: Registration = Registration {
    name: "Flectar Mail (Portable)",
    prog_id: "FlectarMail.Portable.Url.mailto",
};

/// The `Application Id` in platform/windows/msix/AppxManifest.xml.in.
const PACKAGE_APPLICATION_ID: &str = "FlectarMail";
const DESCRIPTION: &str = "Native, offline-first email client";
const REGISTERED_APPLICATIONS: &str = r"Software\RegisteredApplications";
/// Windows 11 opens Default apps at an application's own page.
const FIRST_WINDOWS_11_BUILD: u32 = 22000;

impl Registration {
    fn client(&self) -> String {
        format!(r"Software\Clients\Mail\{}", self.name)
    }

    fn capabilities(&self) -> String {
        format!(r"{}\Capabilities", self.client())
    }

    fn class(&self) -> String {
        format!(r"Software\Classes\{}", self.prog_id)
    }

    fn command(&self) -> String {
        format!(r"{}\shell\open\command", self.class())
    }
}

/// How this copy of Flectar Mail is installed.
enum Copy {
    /// The Microsoft Store package; its manifest registers the handler.
    Packaged { app_user_model_id: String },
    /// Installed by the setup program, which registered it.
    Installed,
    /// Run from the portable ZIP.
    Portable,
}

pub(super) fn supported() -> bool {
    true
}

pub(super) fn state() -> io::Result<State> {
    let copy = copy()?;
    let current = default_prog_id();
    let default = match &copy {
        // Windows names a package's handler with a generated ProgID, which
        // records the application it belongs to.
        Copy::Packaged { app_user_model_id } => current
            .as_deref()
            .and_then(package_application)
            .is_some_and(|application| application == *app_user_model_id),
        Copy::Installed => current.as_deref() == Some(INSTALLED.prog_id),
        Copy::Portable => current.as_deref() == Some(PORTABLE.prog_id),
    };
    let removable = matches!(copy, Copy::Portable) && registered_executable(&PORTABLE)?.is_some();
    Ok(State {
        status: if default {
            Status::Default
        } else {
            Status::NotDefault
        },
        removable,
    })
}

/// Point a portable copy's registration at this executable when it moved, so
/// its links keep arriving. Runs once at startup, like the sign-in entry's
/// repair: with several portable copies, the one last started gets them.
pub(super) fn repair() -> io::Result<()> {
    if !matches!(copy()?, Copy::Portable) {
        return Ok(());
    }
    match registered_executable(&PORTABLE)? {
        Some(registered) if !same_file(&registered, &std::env::current_exe()?) => {
            register(&PORTABLE)
        }
        _ => Ok(()),
    }
}

pub(super) fn make_default(done: Done) {
    let result = (|| {
        let copy = copy()?;
        if matches!(copy, Copy::Portable) {
            register(&PORTABLE)?;
        }
        open_default_apps(&copy)?;
        Ok(Applied::Settings)
    })();
    done(result);
}

pub(super) fn remove() -> io::Result<()> {
    if !matches!(copy()?, Copy::Portable) {
        return Ok(());
    }
    delete_value(REGISTERED_APPLICATIONS, PORTABLE.name)?;
    delete_tree(&PORTABLE.client())?;
    delete_tree(&PORTABLE.class())?;
    associations_changed();
    Ok(())
}

fn copy() -> io::Result<Copy> {
    if let Some(family) = package_family_name() {
        return Ok(Copy::Packaged {
            app_user_model_id: format!("{family}!{PACKAGE_APPLICATION_ID}"),
        });
    }
    let executable = std::env::current_exe()?;
    let installed = registered_executable(&INSTALLED)?
        .is_some_and(|registered| same_file(&registered, &executable));
    Ok(if installed {
        Copy::Installed
    } else {
        Copy::Portable
    })
}

/// The layout platform/windows/installer.iss writes, for this executable.
fn register(registration: &Registration) -> io::Result<()> {
    let executable = std::env::current_exe()?;
    let quoted = |suffix: &str| {
        // Windows paths cannot contain quotes, so quoting is unambiguous.
        let mut value = OsString::from("\"");
        value.push(&executable);
        value.push("\"");
        value.push(suffix);
        value
    };
    let icon = quoted(",0");
    let command = quoted(&format!(" {} \"%1\"", crate::mailto_links::MAILTO_OPTION));
    let label = OsStr::new("Flectar Mail email link");
    set_value(&registration.class(), None, label)?;
    set_value(&registration.class(), Some("FriendlyTypeName"), label)?;
    set_value(
        &format!(r"{}\DefaultIcon", registration.class()),
        None,
        &icon,
    )?;
    set_value(&registration.command(), None, &command)?;
    let capabilities = registration.capabilities();
    set_value(
        &capabilities,
        Some("ApplicationName"),
        OsStr::new(registration.name),
    )?;
    set_value(
        &capabilities,
        Some("ApplicationDescription"),
        OsStr::new(DESCRIPTION),
    )?;
    set_value(&capabilities, Some("ApplicationIcon"), &icon)?;
    set_value(
        &format!(r"{capabilities}\URLAssociations"),
        Some("mailto"),
        OsStr::new(registration.prog_id),
    )?;
    set_value(
        REGISTERED_APPLICATIONS,
        Some(registration.name),
        OsStr::new(&capabilities),
    )?;
    associations_changed();
    Ok(())
}

/// The executable a registration's command starts.
fn registered_executable(registration: &Registration) -> io::Result<Option<PathBuf>> {
    let Some(command) = get_value(HKEY_CURRENT_USER, &registration.command(), None)? else {
        return Ok(None);
    };
    let command = command.to_string_lossy();
    Ok(command
        .strip_prefix('"')
        .and_then(|quoted| quoted.split_once('"'))
        .map(|(executable, _)| PathBuf::from(executable)))
}

fn same_file(first: &Path, second: &Path) -> bool {
    match (std::fs::canonicalize(first), std::fs::canonicalize(second)) {
        (Ok(first), Ok(second)) => first
            .as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(&second.as_os_str().to_string_lossy()),
        _ => false,
    }
}

/// The ProgID that opens mail links for the current user, honoring their
/// choice in Default apps.
fn default_prog_id() -> Option<String> {
    let scheme = wide(OsStr::new("mailto"));
    let mut buffer = vec![0_u16; 512];
    let mut length = buffer.len() as u32;
    // SAFETY: `scheme` is NUL-terminated and `buffer` holds `length` units.
    let result = unsafe {
        AssocQueryStringW(
            ASSOCF_IS_PROTOCOL,
            ASSOCSTR_PROGID,
            scheme.as_ptr(),
            null(),
            buffer.as_mut_ptr(),
            &mut length,
        )
    };
    if result != 0 {
        return None;
    }
    let value = from_wide(&buffer[..(length as usize).min(buffer.len())]);
    Some(value.to_string_lossy().into_owned()).filter(|value| !value.is_empty())
}

/// The application a packaged handler's ProgID belongs to.
fn package_application(prog_id: &str) -> Option<String> {
    let subkey = format!(r"{prog_id}\Application");
    get_value(HKEY_CLASSES_ROOT, &subkey, Some("AppUserModelID"))
        .ok()
        .flatten()
        .map(|value| value.to_string_lossy().into_owned())
}

fn package_family_name() -> Option<String> {
    let mut length = 0_u32;
    // SAFETY: a zero length with no buffer only asks for the name's size.
    let status = unsafe { GetCurrentPackageFamilyName(&mut length, null_mut()) };
    if status == APPMODEL_ERROR_NO_PACKAGE || status != ERROR_INSUFFICIENT_BUFFER {
        return None;
    }
    let mut buffer = vec![0_u16; length as usize];
    // SAFETY: `buffer` holds `length` units, as passed.
    let status = unsafe { GetCurrentPackageFamilyName(&mut length, buffer.as_mut_ptr()) };
    (status == ERROR_SUCCESS).then(|| from_wide(&buffer).to_string_lossy().into_owned())
}

/// Open Default apps for the user to choose Flectar Mail. Windows 11 opens
/// it at Flectar Mail's own page; Windows 10 only has the whole list, as
/// Firefox also does.
fn open_default_apps(copy: &Copy) -> io::Result<()> {
    let page = if windows_build().is_some_and(|build| build >= FIRST_WINDOWS_11_BUILD) {
        match copy {
            Copy::Packaged { app_user_model_id } => {
                format!("ms-settings:defaultapps?registeredAUMID={app_user_model_id}")
            }
            Copy::Installed => settings_link(INSTALLED.name),
            Copy::Portable => settings_link(PORTABLE.name),
        }
    } else {
        "ms-settings:defaultapps".to_owned()
    };
    let page = wide(OsStr::new(&page));
    // SAFETY: `page` is NUL-terminated and every other pointer may be null.
    let result = unsafe {
        ShellExecuteW(
            null_mut(),
            null(),
            page.as_ptr(),
            null(),
            null(),
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

/// The documented link takes the `RegisteredApplications` value name,
/// URI-escaped.
fn settings_link(name: &str) -> String {
    format!(
        "ms-settings:defaultapps?registeredAppUser={}",
        name.replace(' ', "%20")
    )
}

fn windows_build() -> Option<u32> {
    get_value(
        HKEY_LOCAL_MACHINE,
        r"SOFTWARE\Microsoft\Windows NT\CurrentVersion",
        Some("CurrentBuildNumber"),
    )
    .ok()
    .flatten()?
    .to_string_lossy()
    .trim()
    .parse()
    .ok()
}

/// Let Explorer and Settings see changed handlers, as SHChangeNotify asks of
/// every program that registers one.
fn associations_changed() {
    // SAFETY: this event takes no items.
    unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED as i32, SHCNF_IDLIST, null(), null()) };
}

fn get_value(root: HKEY, subkey: &str, name: Option<&str>) -> io::Result<Option<OsString>> {
    let subkey = wide(OsStr::new(subkey));
    let name = name.map(|name| wide(OsStr::new(name)));
    let name = name.as_ref().map_or(null(), |name| name.as_ptr());
    loop {
        let mut size = 0_u32;
        // SAFETY: a null data pointer asks only for the required size.
        let status = unsafe {
            RegGetValueW(
                root,
                subkey.as_ptr(),
                name,
                RRF_RT_REG_SZ,
                null_mut(),
                null_mut(),
                &mut size,
            )
        };
        if status == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        check(status)?;
        let mut data = vec![0_u16; (size as usize).div_ceil(2)];
        // SAFETY: `data` is writable for `size` bytes, as passed.
        let status = unsafe {
            RegGetValueW(
                root,
                subkey.as_ptr(),
                name,
                RRF_RT_REG_SZ,
                null_mut(),
                data.as_mut_ptr().cast(),
                &mut size,
            )
        };
        match status {
            ERROR_FILE_NOT_FOUND => return Ok(None),
            // The value grew between the two calls; ask again.
            ERROR_MORE_DATA => continue,
            status => check(status)?,
        }
        data.truncate((size as usize).div_ceil(2));
        return Ok(Some(from_wide(&data)));
    }
}

fn set_value(subkey: &str, name: Option<&str>, value: &OsStr) -> io::Result<()> {
    let subkey = wide(OsStr::new(subkey));
    let name = name.map(|name| wide(OsStr::new(name)));
    let data = wide(value);
    // SAFETY: every pointer refers to a live, NUL-terminated UTF-16 buffer
    // or is null for the default value, and the byte count covers `data`
    // including its terminator.
    check(unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            name.as_ref().map_or(null(), |name| name.as_ptr()),
            REG_SZ,
            data.as_ptr().cast(),
            u32::try_from(data.len() * 2).map_err(io::Error::other)?,
        )
    })
}

fn delete_value(subkey: &str, name: &str) -> io::Result<()> {
    // SAFETY: both strings are live, NUL-terminated UTF-16 buffers.
    let status = unsafe {
        RegDeleteKeyValueW(
            HKEY_CURRENT_USER,
            wide(OsStr::new(subkey)).as_ptr(),
            wide(OsStr::new(name)).as_ptr(),
        )
    };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(());
    }
    check(status)
}

fn delete_tree(subkey: &str) -> io::Result<()> {
    // SAFETY: the subkey is a live, NUL-terminated UTF-16 buffer.
    let status = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, wide(OsStr::new(subkey)).as_ptr()) };
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

/// UTF-16 up to the first NUL.
fn from_wide(value: &[u16]) -> OsString {
    let end = value
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(value.len());
    OsString::from_wide(&value[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_links_name_the_registration() {
        assert_eq!(
            settings_link(INSTALLED.name),
            "ms-settings:defaultapps?registeredAppUser=Flectar%20Mail"
        );
        assert_eq!(
            settings_link(PORTABLE.name),
            "ms-settings:defaultapps?registeredAppUser=Flectar%20Mail%20(Portable)"
        );
    }
}
