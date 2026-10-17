//! Open the system browser without leaking AppImage libraries into host programs.

use std::sync::OnceLock;

/// Signature of a platform-installed opener for external addresses.
type ExternalUrlOpener = fn(&str) -> Result<(), String>;

/// Opener installed by a platform integration that cannot hand addresses to a
/// helper process (for example, a host-managed sandbox without
/// `xdg-open`-style tools). The first registration wins.
static EXTERNAL_OPENER: OnceLock<ExternalUrlOpener> = OnceLock::new();

/// Install the opener used by [`open_external`].
pub fn set_external_url_opener(opener: fn(&str) -> Result<(), String>) {
    let _ = EXTERNAL_OPENER.set(opener);
}

#[cfg(linux_desktop)]
use std::process::{Command, Stdio};

#[cfg(linux_desktop)]
fn host_command(program: &str) -> Command {
    let mut command = Command::new(program);
    restore_library_path(
        &mut command,
        std::env::var_os("FLECTAR_MAIL_BUNDLED").is_some(),
        std::env::var_os("FLECTAR_MAIL_HOST_LD_LIBRARY_PATH"),
    );
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

#[cfg(linux_desktop)]
fn restore_library_path(command: &mut Command, bundled: bool, path: Option<std::ffi::OsString>) {
    if !bundled {
        return;
    }
    match path.filter(|p| !p.is_empty()) {
        Some(path) => {
            command.env("LD_LIBRARY_PATH", path);
        }
        None => {
            command.env_remove("LD_LIBRARY_PATH");
        }
    }
}

#[cfg(linux_desktop)]
fn launch(mut command: Command) -> Result<(), String> {
    let mut child = command.spawn().map_err(|e| e.to_string())?;
    // xdg-open may exit before the browser starts, or live as long as the browser.
    // Detect immediate failures, but never block authorization on a browser exit.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => return Err(format!("browser launcher exited with {status}")),
            Err(error) => return Err(error.to_string()),
            Ok(None) if std::time::Instant::now() >= deadline => {
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                return Ok(());
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(25)),
        }
    }
}

#[cfg(linux_desktop)]
fn open_with_desktop_portal(address: &url::Url) -> Result<(), String> {
    let address = address.clone();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime.block_on(async move {
        tokio::time::timeout(std::time::Duration::from_secs(5), async move {
            let request = ashpd::desktop::open_uri::OpenFileRequest::default()
                .send_uri(&address)
                .await
                .map_err(|error| error.to_string())?;
            request.response().map_err(|error| error.to_string())
        })
        .await
        .map_err(|_| "desktop portal did not respond".to_owned())?
    })
}

pub(super) fn open(url: &str) -> Result<(), String> {
    // This entry point is only for provider authorization, never a shell command.
    let address = url::Url::parse(url).map_err(|_| "Sign-in address is invalid".to_owned())?;
    if address.scheme() != "https" || address.host_str().is_none() {
        return Err("Sign-in requires an HTTPS address".into());
    }
    let url = address.as_str();
    #[cfg(linux_desktop)]
    {
        // The portal is the reliable route from AppImages and desktop
        // containers to the user's host browser. Keep command-based fallbacks
        // for minimal desktops that do not run xdg-desktop-portal.
        if open_with_desktop_portal(&address).is_ok() {
            return Ok(());
        }
        let mut xdg = host_command("xdg-open");
        xdg.arg(url);
        if launch(xdg).is_ok() {
            return Ok(());
        }
        let mut gio = host_command("gio");
        gio.args(["open", url]);
        launch(gio).map_err(|_| "Could not open the default browser".into())
    }
    #[cfg(not(linux_desktop))]
    webbrowser::open(url).map_err(|e| e.to_string())
}

/// Open an address that only needs to reach the system browser: links clicked
/// in a mail, project pages. Unlike [`open`] — the provider authorization
/// entry point, which insists on a validated HTTPS sign-in address — this
/// passes the address through as-is and prefers a platform-installed opener
/// when one exists.
pub(super) fn open_external(url: &str) -> Result<(), String> {
    if let Some(opener) = EXTERNAL_OPENER.get() {
        return opener(url);
    }
    webbrowser::open(url).map_err(|error| error.to_string())
}

#[cfg(all(test, linux_desktop))]
mod tests {
    use super::*;
    #[test]
    fn detects_launcher_failure() {
        let mut command = Command::new("sh");
        command.args(["-c", "exit 3"]);
        assert!(launch(command).is_err());
    }
    #[test]
    fn detects_launcher_success() {
        let mut command = Command::new("sh");
        command.args(["-c", "exit 0"]);
        assert!(launch(command).is_ok());
    }
    #[test]
    fn rejects_non_https_authorization() {
        assert!(open("file:///tmp/test").is_err());
        assert!(open("--help").is_err());
        assert!(open("https://").is_err());
    }
    #[test]
    fn restores_host_libraries_only_for_bundled_builds() {
        let mut command = Command::new("xdg-open");
        restore_library_path(&mut command, false, None);
        assert_eq!(command.get_envs().count(), 0);
        restore_library_path(&mut command, true, Some("/host/lib".into()));
        assert_eq!(command.get_envs().next().unwrap().1.unwrap(), "/host/lib");
        restore_library_path(&mut command, true, Some("".into()));
        assert_eq!(command.get_envs().next().unwrap().1, None);
    }
    #[test]
    fn foreground_browser_does_not_hold_up_authorization() {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 2"]);
        let start = std::time::Instant::now();
        assert!(launch(command).is_ok());
        assert!(start.elapsed() < std::time::Duration::from_millis(1800));
    }
}
