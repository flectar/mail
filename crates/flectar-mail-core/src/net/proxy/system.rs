//! The operating system's proxy settings.
//!
//! Each host reads its platform's settings through [`SystemProxies`]. They
//! answer with proxy URLs in the form GLib and the desktop portal use:
//! `http://proxy:3128`, `socks5://proxy:1080`, `direct://`. The environment's
//! proxy variables come first when any is set, as in .NET, so a program
//! started with its own proxy settings uses them. Without either, connections
//! go direct.

use super::{Bypass, Credentials, ProxyKind, ProxyServer, Route};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use url::Url;

/// The system's proxy settings, as the host reads them.
pub trait SystemProxies: Send + Sync {
    /// The proxies the system names for a connection to `url`, in the order
    /// to try them, as URLs such as `http://proxy:3128`, `socks5://proxy:1080`
    /// or `direct://`. An empty list means a direct connection.
    ///
    /// It may take a while, as when it runs a proxy auto-configuration
    /// script. Connections ask from a thread that can wait, and answers are
    /// kept for a minute.
    fn lookup(&self, url: &Url) -> Vec<Url>;
}

impl<T: SystemProxies + ?Sized> SystemProxies for Box<T> {
    fn lookup(&self, url: &Url) -> Vec<Url> {
        (**self).lookup(url)
    }
}

/// A host's source with its answers kept, per scheme, server and port, for a
/// minute and until the proxy settings change. A platform can run a proxy
/// auto-configuration script for each server, so asking again for every
/// connection would cost time.
pub(super) struct Cached {
    source: Box<dyn SystemProxies>,
    /// Answers by `scheme://host:port`.
    entries: Mutex<HashMap<String, Kept>>,
    /// Which proxy settings are in force; see [`super::generation`].
    generation: fn() -> u64,
}

struct Kept {
    at: Instant,
    generation: u64,
    proxies: Vec<Url>,
}

impl Cached {
    /// How long an answer is kept: the system's settings can change at any
    /// time, as when the computer joins another network.
    const KEEP: Duration = Duration::from_secs(60);
    /// Answers kept at most, so a long session doesn't collect every server.
    const MOST: usize = 256;

    pub(super) fn new(source: impl SystemProxies + 'static) -> Self {
        Self {
            source: Box::new(source),
            entries: Mutex::new(HashMap::new()),
            generation: super::generation,
        }
    }
}

impl SystemProxies for Cached {
    fn lookup(&self, url: &Url) -> Vec<Url> {
        let key = format!(
            "{}://{}:{}",
            url.scheme(),
            url.host_str().unwrap_or_default(),
            url.port_or_known_default().unwrap_or_default()
        );
        let generation = (self.generation)();
        let entries = || {
            self.entries
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
        };
        if let Some(kept) = entries().get(&key)
            && kept.generation == generation
            && kept.at.elapsed() < Self::KEEP
        {
            return kept.proxies.clone();
        }
        // Asked without the lock held, and without holding up the other
        // tasks of an async worker thread.
        let proxies = blocking(|| self.source.lookup(url));
        let mut entries = entries();
        if entries.len() >= Self::MOST {
            entries.clear();
        }
        entries.insert(
            key,
            Kept {
                at: Instant::now(),
                generation,
                proxies: proxies.clone(),
            },
        );
        proxies
    }
}

/// Run `work`, which may block for a while. On a worker thread of Tokio's
/// multi-threaded runtime, the worker's other tasks move to another thread
/// meanwhile. Elsewhere, as on a thread for blocking work, it just runs.
fn blocking<T>(work: impl FnOnce() -> T) -> T {
    use tokio::runtime::{Handle, RuntimeFlavor};
    match Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(work)
        }
        _ => work(),
    }
}

/// The proxy variables of the environment, read as reqwest read them before
/// Flectar Mail had its own proxy settings, and as Go reads them: `HTTP_PROXY`
/// for `http` URLs, `HTTPS_PROXY` for `https` ones, `ALL_PROXY` for the rest
/// and as the fallback, and `NO_PROXY` for the servers they skip. Each name
/// counts in capitals or in lower case, the capitals first. A program run as
/// a CGI script ignores `HTTP_PROXY`, which a request's `Proxy:` header can
/// set (httpoxy).
#[derive(Clone, Debug, Default)]
pub struct Environment {
    http: Option<Url>,
    https: Option<Url>,
    all: Option<Url>,
    bypass: Bypass,
}

impl Environment {
    /// The process's own variables, read once: they don't change while it
    /// runs.
    pub fn read() -> &'static Self {
        static READ: std::sync::OnceLock<Environment> = std::sync::OnceLock::new();
        READ.get_or_init(|| Self::from_lookup(|name| std::env::var(name).ok()))
    }

    /// Whether a variable names a proxy. `NO_PROXY` alone doesn't count.
    fn is_set(&self) -> bool {
        self.http.is_some() || self.https.is_some() || self.all.is_some()
    }

    pub(crate) fn from_lookup(variable: impl Fn(&str) -> Option<String>) -> Self {
        let first = |names: &[&str]| {
            names
                .iter()
                .filter_map(|name| variable(name))
                .map(|value| value.trim().to_owned())
                .find(|value| !value.is_empty())
                .and_then(|value| proxy_url(&value))
        };
        let cgi = variable("REQUEST_METHOD").is_some();
        Self {
            http: if cgi {
                None
            } else {
                first(&["HTTP_PROXY", "http_proxy"])
            },
            https: first(&["HTTPS_PROXY", "https_proxy"]),
            all: first(&["ALL_PROXY", "all_proxy"]),
            bypass: Bypass::parse(
                &["NO_PROXY", "no_proxy"]
                    .iter()
                    .find_map(|name| variable(name))
                    .unwrap_or_default(),
            ),
        }
    }
}

/// What lookups ask: the environment when one of its variables names a
/// proxy, the host's source otherwise, and the empty environment without one.
fn chosen<'a>(
    environment: &'a Environment,
    host: Option<&'a dyn SystemProxies>,
) -> &'a dyn SystemProxies {
    match host {
        Some(host) if !environment.is_set() => host,
        _ => environment,
    }
}

impl SystemProxies for Environment {
    fn lookup(&self, url: &Url) -> Vec<Url> {
        let skipped = url.host_str().is_some_and(|host| self.bypass.skips(host));
        let proxy = match url.scheme() {
            "http" => self.http.as_ref().or(self.all.as_ref()),
            "https" => self.https.as_ref().or(self.all.as_ref()),
            _ => self.all.as_ref(),
        };
        match proxy {
            Some(proxy) if !skipped => vec![proxy.clone()],
            _ => Vec::new(),
        }
    }
}

/// A proxy variable's value: a URL, or `host:port`, which curl takes as an
/// HTTP proxy.
fn proxy_url(value: &str) -> Option<Url> {
    if value.contains("://") {
        Url::parse(value).ok()
    } else {
        Url::parse(&format!("http://{value}")).ok()
    }
}

/// The routes to try for a mail protocol connection when the app follows
/// the system's settings.
pub(super) fn mail_routes(
    source: Option<&dyn SystemProxies>,
    scheme: &str,
    host: &str,
    port: u16,
) -> Vec<Route> {
    routes_from(chosen(Environment::read(), source), scheme, host, port)
}

fn routes_from(source: &dyn SystemProxies, scheme: &str, host: &str, port: u16) -> Vec<Route> {
    let Some(url) = target_url(scheme, host, port) else {
        return vec![Route::Direct];
    };
    let mut routes = Vec::new();
    for route in source.lookup(&url).iter().filter_map(mail_route) {
        // Systems that don't say which SOCKS version a server speaks name it
        // once for each, and two of those versions go the same way here.
        if !routes.contains(&route) {
            routes.push(route);
        }
    }
    if routes.is_empty() {
        // No proxy for mail: no SOCKS proxy, or no proxy at all.
        return vec![Route::Direct];
    }
    routes
}

/// What mail does with a proxy the system names. Mail goes through the
/// system's SOCKS proxies, as in Thunderbird, and not through its HTTP ones,
/// which are set up for the web: they commonly allow tunnels only to the
/// HTTPS port (Squid's default), so a mail server that works without them
/// would fail through them. An HTTP proxy for mail is chosen in Settings.
fn mail_route(proxy: &Url) -> Option<Route> {
    let kind = match proxy.scheme() {
        "direct" => return Some(Route::Direct),
        "socks" | "socks5" | "socks5h" => ProxyKind::Socks5,
        "socks4" | "socks4a" => ProxyKind::Socks4a,
        _ => return None,
    };
    server(proxy, kind).map(Route::Proxy)
}

/// The proxy for an HTTP request when the app follows the system's settings.
pub(super) fn http_proxy(source: Option<&dyn SystemProxies>, url: &Url) -> Option<Url> {
    proxy_from(chosen(Environment::read(), source), url)
}

fn proxy_from(source: &dyn SystemProxies, url: &Url) -> Option<Url> {
    let proxy = source.lookup(url).into_iter().next()?;
    let mut proxy = match proxy.scheme() {
        "direct" => return None,
        // Names are resolved by the proxy, as for mail connections.
        "socks" | "socks5" => replace_scheme(&proxy, "socks5h")?,
        _ => proxy,
    };
    if proxy.port().is_none() {
        let port = default_port(proxy.scheme())?;
        proxy.set_port(Some(port)).ok()?;
    }
    Some(proxy)
}

fn replace_scheme(url: &Url, scheme: &str) -> Option<Url> {
    let rest = url.as_str().split_once("://")?.1;
    Url::parse(&format!("{scheme}://{rest}")).ok()
}

fn default_port(scheme: &str) -> Option<u16> {
    match scheme {
        "http" => Some(80),
        "https" => Some(443),
        "socks" | "socks4" | "socks4a" | "socks5" | "socks5h" => Some(1080),
        _ => None,
    }
}

/// The URL a mail connection is looked up by, such as `imaps://host:993`.
fn target_url(scheme: &str, host: &str, port: u16) -> Option<Url> {
    let host = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    Url::parse(&format!("{scheme}://{host}:{port}")).ok()
}

fn server(proxy: &Url, kind: ProxyKind) -> Option<ProxyServer> {
    let host = proxy
        .host_str()?
        .trim_start_matches('[')
        .trim_end_matches(']');
    let port = proxy.port().or_else(|| default_port(proxy.scheme()))?;
    let user = percent_decode(proxy.username());
    let credentials = (!user.is_empty()).then(|| Credentials {
        user,
        password: percent_decode(proxy.password().unwrap_or_default()),
    });
    Some(ProxyServer {
        kind,
        host: host.to_owned(),
        port,
        credentials,
    })
}

fn percent_decode(text: &str) -> String {
    percent_encoding::percent_decode_str(text)
        .decode_utf8_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests;
