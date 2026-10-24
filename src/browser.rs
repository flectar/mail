//! Host URL dispatch, with separate policies for mail links and authorization.

use std::sync::Arc;

type OpenUrl = dyn Fn(&str) -> Result<(), String> + Send + Sync;

/// The URL service carried by [`crate::PlatformContext`].
///
/// The default uses the system browser, preferring Linux's desktop portal for
/// authorization. Hosts without desktop launchers supply a callback with
/// [`Self::new`]. Clones share that callback's state.
#[derive(Clone, Default)]
pub struct ExternalUrlOpener {
    handler: Option<Arc<OpenUrl>>,
}

impl ExternalUrlOpener {
    /// Use a host callback for both external links and browser authorization.
    ///
    /// Mail links call this on the application event loop; authorization calls
    /// it on a worker thread. Hosts must marshal native API calls to their
    /// required thread. Return promptly after handing the URL to the host,
    /// without waiting for the browser to close. Errors are shown to the user;
    /// a failed host callback never falls back to a desktop launcher.
    pub fn new(opener: impl Fn(&str) -> Result<(), String> + Send + Sync + 'static) -> Self {
        Self {
            handler: Some(Arc::new(opener)),
        }
    }

    pub(super) fn open_external(&self, url: &str) -> Result<(), String> {
        match &self.handler {
            Some(opener) => opener(url),
            None => webbrowser::open(url).map_err(|error| error.to_string()),
        }
    }

    pub(super) fn open_authorization(&self, url: &str) -> Result<(), String> {
        let address = url::Url::parse(url).map_err(|_| "Sign-in address is invalid".to_owned())?;
        if address.scheme() != "https" || address.host_str().is_none() {
            return Err("Sign-in requires an HTTPS address".into());
        }
        match &self.handler {
            Some(opener) => opener(address.as_str()),
            None => open_system_authorization(&address),
        }
    }
}

#[cfg(linux_desktop)]
use std::process::{Command, Stdio};

#[cfg(linux_desktop)]
fn quiet_command(program: &str) -> Command {
    let mut command = Command::new(program);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
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

fn open_system_authorization(address: &url::Url) -> Result<(), String> {
    let url = address.as_str();
    #[cfg(linux_desktop)]
    {
        // The portal is the reliable route from AppImages and desktop
        // containers to the user's host browser. Keep command-based fallbacks
        // for minimal desktops that do not run xdg-desktop-portal.
        if open_with_desktop_portal(address).is_ok() {
            return Ok(());
        }
        let mut xdg = quiet_command("xdg-open");
        xdg.arg(url);
        if launch(xdg).is_ok() {
            return Ok(());
        }
        let mut gio = quiet_command("gio");
        gio.args(["open", url]);
        launch(gio).map_err(|_| "Could not open the default browser".into())
    }
    #[cfg(not(linux_desktop))]
    webbrowser::open(url).map_err(|e| e.to_string())
}

#[cfg(test)]
mod dispatch_tests {
    use super::*;
    use crate::mail_render_projection::open_email_link;
    use std::sync::Mutex;

    #[test]
    fn mail_links_preserve_allowed_addresses_and_reject_other_schemes() {
        let addresses = Arc::new(Mutex::new(Vec::new()));
        let recorded = addresses.clone();
        let opener = ExternalUrlOpener::new(move |url| {
            recorded.lock().unwrap().push(url.to_owned());
            Ok(())
        });
        let allowed = [
            "http://example.com/path?q=1#part",
            "https://example.com/a%20b?x=1&y=2",
            "HTTPS://example.com/",
            "mailto:person@example.com?subject=Hello%20there",
            "tel:+123456789",
        ];
        for url in allowed {
            open_email_link(&opener, &format!("  {url}\n")).unwrap();
        }
        for url in [
            "",
            "example.com",
            "/relative/path",
            "#fragment",
            "--help",
            "javascript:alert(1)",
            "data:text/html,hello",
            "file:///tmp/message.html",
            "intent://example.com",
        ] {
            assert!(open_email_link(&opener, url).is_err(), "{url}");
        }
        assert_eq!(*addresses.lock().unwrap(), allowed);
    }

    #[test]
    fn authorization_validates_before_dispatch_for_default_and_custom_openers() {
        let custom = ExternalUrlOpener::new(|_| panic!("invalid authorization reached the host"));
        for opener in [ExternalUrlOpener::default(), custom] {
            for url in [
                "http://example.com/",
                "file:///tmp/test",
                "mailto:person@example.com",
                "javascript:alert(1)",
                "--help",
                "https://",
                "https://[invalid]/",
                "/relative/path",
            ] {
                assert!(opener.open_authorization(url).is_err(), "{url}");
            }
        }
    }

    #[test]
    fn authorization_and_mail_share_the_host_across_threads() {
        let addresses = Arc::new(Mutex::new(Vec::new()));
        let recorded = addresses.clone();
        let opener = ExternalUrlOpener::new(move |url| {
            recorded.lock().unwrap().push(url.to_owned());
            Ok(())
        });
        open_email_link(&opener, "https://example.com/mail").unwrap();
        let worker_opener = opener.clone();
        std::thread::spawn(move || {
            worker_opener
                .open_authorization("HTTPS://EXAMPLE.COM/sign-in?state=a%2Bb&code_challenge=c")
        })
        .join()
        .unwrap()
        .unwrap();
        assert_eq!(
            *addresses.lock().unwrap(),
            [
                "https://example.com/mail",
                "https://example.com/sign-in?state=a%2Bb&code_challenge=c",
            ]
        );
    }

    #[test]
    fn host_errors_propagate_without_fallback_or_global_registration() {
        let first = ExternalUrlOpener::new(|_| Err("first host unavailable".into()));
        let second = ExternalUrlOpener::new(|_| Err("second host unavailable".into()));
        for (opener, expected) in [
            (first, "first host unavailable"),
            (second, "second host unavailable"),
        ] {
            assert_eq!(
                open_email_link(&opener, "https://example.com/"),
                Err(expected.to_owned())
            );
            assert_eq!(
                opener.open_authorization("https://example.com/"),
                Err(expected.to_owned())
            );
        }
    }
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
    fn foreground_browser_does_not_hold_up_authorization() {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 2"]);
        let start = std::time::Instant::now();
        assert!(launch(command).is_ok());
        assert!(start.elapsed() < std::time::Duration::from_millis(1800));
    }
}
