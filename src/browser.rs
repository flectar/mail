//! Open the system browser without leaking AppImage libraries into host programs.
#[cfg(target_os = "linux")]
use std::process::{Command, Stdio};

#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
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

pub(super) fn open(url: &str) -> Result<(), String> {
    // This entry point is only for provider authorization, never a shell command.
    if !url.starts_with("https://") {
        return Err("Sign-in requires an HTTPS address".into());
    }
    #[cfg(target_os = "linux")]
    {
        let mut xdg = host_command("xdg-open");
        xdg.arg(url);
        if launch(xdg).is_ok() {
            return Ok(());
        }
        let mut gio = host_command("gio");
        gio.args(["open", url]);
        launch(gio).map_err(|_| "Could not open the default browser".into())
    }
    #[cfg(not(target_os = "linux"))]
    webbrowser::open(url).map_err(|e| e.to_string())
}

#[cfg(all(test, target_os = "linux"))]
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
