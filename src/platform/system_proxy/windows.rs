//! Windows' proxy settings: the user's Internet Options, read through WinHTTP,
//! which also runs their proxy auto-configuration script, found at a set
//! address or by automatic detection (WPAD). Microsoft's sequence: read the
//! settings, run the script when there is one, and use the fixed proxy list
//! when there is none or it can't be run.

use super::windows_settings::{fixed, from_script, script_target};
use flectar_mail_core::net::proxy::SystemProxies;
use std::ffi::c_void;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use url::Url;
use windows_sys::Win32::Foundation::{GetLastError, GlobalFree};
use windows_sys::Win32::Networking::WinHttp::{
    ERROR_WINHTTP_AUTODETECTION_FAILED, ERROR_WINHTTP_BAD_AUTO_PROXY_SCRIPT,
    ERROR_WINHTTP_LOGIN_FAILURE, ERROR_WINHTTP_UNABLE_TO_DOWNLOAD_SCRIPT,
    WINHTTP_ACCESS_TYPE_NAMED_PROXY, WINHTTP_ACCESS_TYPE_NO_PROXY, WINHTTP_AUTO_DETECT_TYPE_DHCP,
    WINHTTP_AUTO_DETECT_TYPE_DNS_A, WINHTTP_AUTOPROXY_AUTO_DETECT, WINHTTP_AUTOPROXY_CONFIG_URL,
    WINHTTP_AUTOPROXY_OPTIONS, WINHTTP_CURRENT_USER_IE_PROXY_CONFIG, WINHTTP_PROXY_INFO,
    WinHttpCloseHandle, WinHttpGetIEProxyConfigForCurrentUser, WinHttpGetProxyForUrl, WinHttpOpen,
};

/// How long automatic detection is left alone after it found no script. It
/// can take seconds, and on most home networks, where Windows has it on by
/// default, there is nothing to find.
const DETECTION_REST: Duration = Duration::from_secs(10 * 60);
/// How long a set script that couldn't be fetched or run is left alone.
/// Each try can take seconds.
const SCRIPT_REST: Duration = Duration::from_secs(60);

pub(super) struct Reader {
    script: Mutex<Script>,
}

#[derive(Default)]
struct Script {
    /// The WinHTTP session scripts run in, which keeps them between lookups.
    session: Option<Session>,
    /// When automatic detection last found nothing.
    detection_failed: Option<Instant>,
    /// The set script that last couldn't be fetched or run, and when.
    script_failed: Option<(String, Instant)>,
}

/// A WinHTTP session handle.
struct Session(*mut c_void);

// SAFETY: WinHTTP session handles can be used from any thread.
unsafe impl Send for Session {}

impl Drop for Session {
    fn drop(&mut self) {
        // SAFETY: the handle came from WinHttpOpen and is closed once.
        unsafe { WinHttpCloseHandle(self.0) };
    }
}

/// The user's settings, as WinHTTP reads them.
struct Settings {
    auto_detect: bool,
    script: Option<String>,
    proxy: Option<String>,
    bypass: Option<String>,
}

impl Reader {
    pub(super) fn new() -> Self {
        Self {
            script: Mutex::new(Script::default()),
        }
    }

    /// The proxies the script names for `url`, or `None` when there is no
    /// script, or it couldn't be found or run, for the fixed settings to
    /// apply, as Windows does.
    fn run_script(&self, url: &Url, settings: &Settings) -> Option<Vec<Url>> {
        let mut script = self
            .script
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let detect = settings.auto_detect
            && script
                .detection_failed
                .is_none_or(|failed| failed.elapsed() >= DETECTION_REST);
        let set_script = settings.script.as_deref().filter(|address| {
            script
                .script_failed
                .as_ref()
                .is_none_or(|(failed, at)| failed != address || at.elapsed() >= SCRIPT_REST)
        });
        if !detect && set_script.is_none() {
            return None;
        }
        if script.session.is_none() {
            let agent = wide("Flectar Mail");
            // SAFETY: the agent name is a live NUL-terminated string, and the
            // null proxy name and bypass list are what NO_PROXY takes.
            let session = unsafe {
                WinHttpOpen(
                    agent.as_ptr(),
                    WINHTTP_ACCESS_TYPE_NO_PROXY,
                    std::ptr::null(),
                    std::ptr::null(),
                    0,
                )
            };
            if session.is_null() {
                return None;
            }
            script.session = Some(Session(session));
        }
        let session = script.session.as_ref()?.0;
        let address = set_script.map(wide);
        let mut options = WINHTTP_AUTOPROXY_OPTIONS::default();
        if let Some(address) = &address {
            options.dwFlags |= WINHTTP_AUTOPROXY_CONFIG_URL;
            options.lpszAutoConfigUrl = address.as_ptr();
        }
        if detect {
            // Detection comes first when both are on, as in Internet Options.
            options.dwFlags |= WINHTTP_AUTOPROXY_AUTO_DETECT;
            options.dwAutoDetectFlags =
                WINHTTP_AUTO_DETECT_TYPE_DHCP | WINHTTP_AUTO_DETECT_TYPE_DNS_A;
        }
        let target = wide(script_target(url).as_str());
        let mut info = WINHTTP_PROXY_INFO::default();
        // SAFETY: the session is open, the target and script address are live
        // NUL-terminated strings, and `options` and `info` are live
        // structures of the types the call takes.
        let ask = |options: &mut WINHTTP_AUTOPROXY_OPTIONS, info: &mut WINHTTP_PROXY_INFO| unsafe {
            if WinHttpGetProxyForUrl(session, target.as_ptr(), options, info) != 0 {
                Ok(())
            } else {
                Err(GetLastError())
            }
        };
        let mut answer = ask(&mut options, &mut info);
        if answer == Err(ERROR_WINHTTP_LOGIN_FAILURE) {
            // Microsoft's advice: sign in to the script's server as the user
            // only when it asks, since signing in costs time.
            options.fAutoLogonIfChallenged = 1;
            answer = ask(&mut options, &mut info);
        }
        if let Err(error) = answer {
            if detect && error == ERROR_WINHTTP_AUTODETECTION_FAILED {
                script.detection_failed = Some(Instant::now());
            }
            if let Some(address) = set_script
                && matches!(
                    error,
                    ERROR_WINHTTP_UNABLE_TO_DOWNLOAD_SCRIPT | ERROR_WINHTTP_BAD_AUTO_PROXY_SCRIPT
                )
            {
                script.script_failed = Some((address.to_owned(), Instant::now()));
            }
            return None;
        }
        drop(script);
        // SAFETY: on success the strings belong to the caller.
        let (proxy, _bypass) = unsafe { (take(info.lpszProxy), take(info.lpszProxyBypass)) };
        if info.dwAccessType != WINHTTP_ACCESS_TYPE_NAMED_PROXY {
            return Some(Vec::new());
        }
        Some(from_script(&proxy.unwrap_or_default()))
    }
}

impl SystemProxies for Reader {
    fn lookup(&self, url: &Url) -> Vec<Url> {
        let Some(settings) = settings() else {
            return Vec::new();
        };
        if let Some(proxies) = self.run_script(url, &settings) {
            return proxies;
        }
        match &settings.proxy {
            Some(list) => fixed(url, list, settings.bypass.as_deref().unwrap_or_default()),
            None => Vec::new(),
        }
    }
}

/// The current user's settings, for the connection in use.
fn settings() -> Option<Settings> {
    let mut config = WINHTTP_CURRENT_USER_IE_PROXY_CONFIG::default();
    // SAFETY: `config` is a live structure; the strings it gets are freed
    // below with GlobalFree, as the function asks.
    if unsafe { WinHttpGetIEProxyConfigForCurrentUser(&mut config) } == 0 {
        return None;
    }
    // SAFETY: each string came from the call above and is taken once.
    let (script, proxy, bypass) = unsafe {
        (
            take(config.lpszAutoConfigUrl),
            take(config.lpszProxy),
            take(config.lpszProxyBypass),
        )
    };
    Some(Settings {
        auto_detect: config.fAutoDetect != 0,
        script,
        proxy,
        bypass,
    })
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

/// A string WinHTTP allocated, read and then freed.
///
/// SAFETY: `text` is null or a NUL-terminated string allocated with
/// GlobalAlloc that nothing else frees.
unsafe fn take(text: *mut u16) -> Option<String> {
    if text.is_null() {
        return None;
    }
    let mut length = 0;
    // SAFETY: the string ends with a NUL.
    while unsafe { *text.add(length) } != 0 {
        length += 1;
    }
    // SAFETY: `length` units were just read.
    let read = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, length) });
    // SAFETY: allocated with GlobalAlloc, as WinHTTP documents.
    unsafe { GlobalFree(text.cast()) };
    Some(read)
}
