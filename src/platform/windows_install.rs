//! How this copy of Flectar Mail is installed on Windows, as the sign-in
//! entry, the default mail app registration and the notifications each need
//! to know: whether it runs from the Microsoft Store package, and which
//! executable the setup program registered.

use std::path::PathBuf;

/// This copy's package family name, when it runs from the Microsoft Store
/// package.
#[cfg(target_os = "windows")]
pub(crate) fn package_family_name() -> Option<String> {
    use windows_sys::Win32::{
        Foundation::{APPMODEL_ERROR_NO_PACKAGE, ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS},
        Storage::Packaging::Appx::GetCurrentPackageFamilyName,
    };
    let mut length = 0_u32;
    // SAFETY: a zero length with no buffer only asks for the name's size.
    let status = unsafe { GetCurrentPackageFamilyName(&mut length, std::ptr::null_mut()) };
    if status == APPMODEL_ERROR_NO_PACKAGE || status != ERROR_INSUFFICIENT_BUFFER {
        return None;
    }
    let mut buffer = vec![0_u16; length as usize];
    // SAFETY: `buffer` holds `length` units, as passed.
    let status = unsafe { GetCurrentPackageFamilyName(&mut length, buffer.as_mut_ptr()) };
    if status != ERROR_SUCCESS {
        return None;
    }
    let end = buffer
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(buffer.len());
    Some(String::from_utf16_lossy(&buffer[..end]))
}

/// The executable a registered command line starts: its quoted first part,
/// or its first word.
pub(crate) fn command_executable(command: &str) -> Option<PathBuf> {
    let executable = match command.strip_prefix('"') {
        Some(quoted) => quoted.split_once('"')?.0,
        None => command.split_whitespace().next()?,
    };
    (!executable.is_empty()).then(|| PathBuf::from(executable))
}

/// Whether two paths lead to the same file, with names compared the way
/// Windows compares them: regardless of case.
#[cfg(target_os = "windows")]
pub(crate) fn same_file(first: &std::path::Path, second: &std::path::Path) -> bool {
    match (std::fs::canonicalize(first), std::fs::canonicalize(second)) {
        (Ok(first), Ok(second)) => first
            .as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(&second.as_os_str().to_string_lossy()),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::command_executable;
    use std::path::Path;

    #[test]
    fn a_command_starts_its_quoted_or_first_word() {
        assert_eq!(
            command_executable(
                r#""C:\Users\A B\AppData\Local\Programs\Flectar Mail\flectar-mail.exe" --toast-activated"#
            )
            .as_deref(),
            Some(Path::new(
                r"C:\Users\A B\AppData\Local\Programs\Flectar Mail\flectar-mail.exe"
            ))
        );
        assert_eq!(
            command_executable(r"C:\flectar.exe --background").as_deref(),
            Some(Path::new(r"C:\flectar.exe"))
        );
        assert_eq!(command_executable("\"unterminated"), None);
        assert_eq!(command_executable("\"\" --background"), None);
        assert_eq!(command_executable(""), None);
    }
}
