//! macOS and iOS proxy settings, read through CFNetwork, which also runs
//! proxy auto-configuration scripts. A script answers first; when there is
//! none, or it can't be fetched or run, the fixed settings apply.

use super::apple_settings::{Answer, DISCOVERY_ADDRESS, Script, Server, Settings, from_script};
use flectar_mail_core::net::proxy::SystemProxies;
use objc2_core_foundation::{
    CFArray, CFBoolean, CFDictionary, CFError, CFNumber, CFRetained, CFRunLoop, CFRunLoopSource,
    CFStreamClientContext, CFString, CFType, CFURL,
};
use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use url::Url;

/// How long a script may take to be fetched and to answer.
const SCRIPT_TIME: Duration = Duration::from_secs(15);
/// How long a script that couldn't be fetched or run is left alone: a set
/// one for a minute, and automatic discovery, which on most networks finds
/// nothing, for ten.
const SCRIPT_REST: Duration = Duration::from_secs(60);
const DISCOVERY_REST: Duration = Duration::from_secs(10 * 60);

type ScriptAnswered =
    unsafe extern "C" fn(client: *mut c_void, proxies: *const CFArray, error: *const CFError);

#[link(name = "CFNetwork", kind = "framework")]
unsafe extern "C" {
    fn CFNetworkCopySystemProxySettings() -> Option<NonNull<CFDictionary>>;
    fn CFNetworkExecuteProxyAutoConfigurationURL(
        script: &CFURL,
        target: &CFURL,
        answered: ScriptAnswered,
        context: *mut CFStreamClientContext,
    ) -> Option<NonNull<CFRunLoopSource>>;
    fn CFNetworkExecuteProxyAutoConfigurationScript(
        script: &CFString,
        target: &CFURL,
        answered: ScriptAnswered,
        context: *mut CFStreamClientContext,
    ) -> Option<NonNull<CFRunLoopSource>>;
    static kCFProxyTypeKey: &'static CFString;
    static kCFProxyHostNameKey: &'static CFString;
    static kCFProxyPortNumberKey: &'static CFString;
    static kCFProxyTypeNone: &'static CFString;
    static kCFProxyTypeHTTP: &'static CFString;
    static kCFProxyTypeHTTPS: &'static CFString;
    static kCFProxyTypeSOCKS: &'static CFString;
}

pub(super) struct Reader {
    /// The script that last couldn't be fetched or run, and when.
    failed: Mutex<Option<(Script, Instant)>>,
}

impl Reader {
    pub(super) fn new() -> Self {
        Self {
            failed: Mutex::new(None),
        }
    }

    fn resting(&self, script: &Script) -> bool {
        let failed = self
            .failed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        failed.as_ref().is_some_and(|(failed, at)| {
            let rest = match failed {
                Script::Address(address) if address == DISCOVERY_ADDRESS => DISCOVERY_REST,
                _ => SCRIPT_REST,
            };
            failed == script && at.elapsed() < rest
        })
    }

    fn remember_failure(&self, script: Script) {
        *self
            .failed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((script, Instant::now()));
    }
}

impl SystemProxies for Reader {
    fn lookup(&self, url: &Url) -> Vec<Url> {
        // CFNetwork autoreleases, and threads that ask have no pool.
        objc2::rc::autoreleasepool(|_| {
            let Some(settings) = settings() else {
                return Vec::new();
            };
            if let Some(script) = settings.script()
                && !self.resting(&script)
                && let Some(target) =
                    CFURL::from_string(None, &CFString::from_str(url.as_str()), None)
            {
                match run_script(&script, &target) {
                    Some(answers) => return from_script(&answers),
                    None => self.remember_failure(script),
                }
            }
            settings.fixed(url)
        })
    }
}

/// The settings in force, for the network in use.
fn settings() -> Option<Settings> {
    // SAFETY: the function takes nothing and returns a dictionary the
    // caller owns, or null.
    let dictionary = unsafe { CFRetained::from_raw(CFNetworkCopySystemProxySettings()?) };
    // SAFETY: the system's proxy settings have string keys.
    let dictionary: CFRetained<CFDictionary<CFString, CFType>> =
        unsafe { CFRetained::cast_unchecked(dictionary) };
    let get = |key: &str| dictionary.get(&CFString::from_str(key));
    let on = |key: &str| get(key).is_some_and(|value| flag(&value));
    let text = |key: &str| {
        get(key)
            .and_then(|value| value.downcast::<CFString>().ok())
            .map(|value| value.to_string())
            .filter(|value| !value.trim().is_empty())
    };
    let server = |kind: &str| {
        if !on(&format!("{kind}Enable")) {
            return None;
        }
        Some(Server {
            host: text(&format!("{kind}Proxy"))?,
            port: get(&format!("{kind}Port")).and_then(|value| port(&value)),
        })
    };
    let script = if on("ProxyAutoConfigEnable") {
        text("ProxyAutoConfigURLString")
            .map(Script::Address)
            .or_else(|| text("ProxyAutoConfigJavaScript").map(Script::Text))
    } else {
        None
    };
    let exceptions = get("ExceptionsList")
        .and_then(|value| value.downcast::<CFArray>().ok())
        .map(|array| {
            // SAFETY: the list holds CoreFoundation objects.
            let array: CFRetained<CFArray<CFType>> = unsafe { CFRetained::cast_unchecked(array) };
            (0..array.len())
                .filter_map(|index| array.get(index)?.downcast::<CFString>().ok())
                .map(|entry| entry.to_string())
                .collect()
        })
        .unwrap_or_default();
    Some(Settings {
        http: server("HTTP"),
        https: server("HTTPS"),
        socks: server("SOCKS"),
        exceptions,
        exclude_simple_hostnames: on("ExcludeSimpleHostnames"),
        script,
        discover: on("ProxyAutoDiscoveryEnable"),
    })
}

/// A switch in the settings: a number, 1 for on, or a boolean.
fn flag(value: &CFType) -> bool {
    if let Some(number) = value.downcast_ref::<CFNumber>() {
        return number.as_i64().is_some_and(|number| number != 0);
    }
    value
        .downcast_ref::<CFBoolean>()
        .is_some_and(CFBoolean::as_bool)
}

fn port(value: &CFType) -> Option<u16> {
    let number = value.downcast_ref::<CFNumber>()?.as_i64()?;
    u16::try_from(number).ok()
}

/// What a script's run gave back.
struct Outcome {
    done: Cell<bool>,
    answers: RefCell<Option<Vec<Answer>>>,
}

/// The answers of `script` for `target`, or `None` when it couldn't be
/// fetched or run in time.
fn run_script(script: &Script, target: &CFURL) -> Option<Vec<Answer>> {
    // Chromium found CFNetwork's script runner crashing when two threads use
    // it at once, so scripts run one at a time.
    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());
    let _turn = ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let outcome = Outcome {
        done: Cell::new(false),
        answers: RefCell::new(None),
    };
    let mut context = CFStreamClientContext {
        version: 0,
        info: std::ptr::from_ref(&outcome).cast_mut().cast(),
        retain: None,
        release: None,
        copyDescription: None,
    };
    let source = match script {
        Script::Address(address) => {
            let address = CFURL::from_string(None, &CFString::from_str(address), None)?;
            // SAFETY: both URLs are live, and `context` points at `outcome`,
            // which outlives the run loop source: it's invalidated below.
            unsafe {
                CFNetworkExecuteProxyAutoConfigurationURL(
                    &address,
                    target,
                    script_answered,
                    &mut context,
                )
            }
        }
        Script::Text(text) => {
            // SAFETY: as above.
            unsafe {
                CFNetworkExecuteProxyAutoConfigurationScript(
                    &CFString::from_str(text),
                    target,
                    script_answered,
                    &mut context,
                )
            }
        }
    }?;
    // SAFETY: the caller owns the run loop source CFNetwork returns.
    let source = unsafe { CFRetained::from_raw(source) };
    // A mode of its own, so nothing else scheduled on this thread's run loop
    // runs meanwhile.
    let mode = CFString::from_str("com.flectar.mail.ProxyAutoConfiguration");
    if let Some(run_loop) = CFRunLoop::current() {
        run_loop.add_source(Some(&*source), Some(&*mode));
        let deadline = Instant::now() + SCRIPT_TIME;
        while !outcome.done.get() {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            CFRunLoop::run_in_mode(Some(&*mode), left.as_secs_f64(), true);
        }
        run_loop.remove_source(Some(&*source), Some(&*mode));
    }
    // After this the callback never runs, so `outcome` can go.
    source.invalidate();
    outcome.answers.into_inner()
}

/// CFNetwork's callback with a script's answers or its error.
unsafe extern "C" fn script_answered(
    client: *mut c_void,
    proxies: *const CFArray,
    error: *const CFError,
) {
    // SAFETY: `client` is the `Outcome` run_script passed, alive until the
    // run loop source is invalidated.
    let outcome = unsafe { &*client.cast::<Outcome>() };
    // SAFETY: CFNetwork passes a live array, or null with an error.
    let proxies = unsafe { proxies.as_ref() };
    if error.is_null()
        && let Some(proxies) = proxies
    {
        *outcome.answers.borrow_mut() = Some(answers(proxies));
    }
    outcome.done.set(true);
    if let Some(run_loop) = CFRunLoop::current() {
        run_loop.stop();
    }
}

/// A script's answers, from CFNetwork's dictionaries.
fn answers(proxies: &CFArray) -> Vec<Answer> {
    // SAFETY: the array holds proxy dictionaries with string keys.
    let proxies: &CFArray<CFDictionary<CFString, CFType>> =
        unsafe { &*std::ptr::from_ref(proxies).cast() };
    (0..proxies.len())
        .filter_map(|index| proxies.get(index))
        .map(|proxy| {
            // SAFETY: the constants are CFNetwork's, set before any call.
            let (type_key, host_key, port_key) =
                unsafe { (kCFProxyTypeKey, kCFProxyHostNameKey, kCFProxyPortNumberKey) };
            let Some(kind) = proxy
                .get(type_key)
                .and_then(|kind| kind.downcast::<CFString>().ok())
            else {
                return Answer::Other;
            };
            let server = || Server {
                host: proxy
                    .get(host_key)
                    .and_then(|host| host.downcast::<CFString>().ok())
                    .map(|host| host.to_string())
                    .unwrap_or_default(),
                port: proxy.get(port_key).and_then(|value| port(&value)),
            };
            // SAFETY: as above.
            let (none, http, https, socks) = unsafe {
                (
                    kCFProxyTypeNone,
                    kCFProxyTypeHTTP,
                    kCFProxyTypeHTTPS,
                    kCFProxyTypeSOCKS,
                )
            };
            if *kind == *none {
                Answer::Direct
            } else if *kind == *http || *kind == *https {
                Answer::Http(server())
            } else if *kind == *socks {
                Answer::Socks(server())
            } else {
                Answer::Other
            }
        })
        .collect()
}
