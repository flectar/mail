//! Connections through a proxy server.
//!
//! A connection either goes straight to its server or through one proxy, and
//! a failed proxy never turns into a direct connection: the caller gets the
//! proxy's error instead.

mod bypass;
mod config;
#[cfg(test)]
mod http_tests;
mod settings;
mod system;
mod tunnel;

pub use bypass::Bypass;
pub use config::{
    AccountOverride, AppProxy, Manual, Owner, SharedClient, generation, http_proxy, mail_routes,
    set_account, set_app, set_system_source, system_changed,
};
#[cfg(test)]
pub(crate) use config::{account_in_force, app_in_force, test_turns};
pub use settings::{AccountProxy, ManualProxy, ProxyMode, ProxySettings};
pub use system::{Environment, SystemProxies};

use serde::{Deserialize, Serialize};
use std::fmt;
use std::io;
use std::time::Duration;
use tokio::net::TcpStream;

/// What the app calls itself to proxies, as in its other HTTP requests.
pub(crate) const USER_AGENT: &str = concat!("Flectar-Mail/", env!("CARGO_PKG_VERSION"));

/// How long a proxy may take to set up a tunnel once it's connected.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

/// How long connections leave a proxy alone after it turned down their user
/// name and password. Signing in again and again, as background syncing
/// would, can lock the account the proxy checks passwords against, and
/// directory lockout counters commonly reset after 15 to 30 minutes. Another
/// user name or password, or saving the settings, ends the wait at once.
const REFUSED_HOLD_OFF: Duration = Duration::from_secs(15 * 60);

/// The kinds of proxy server a connection can go through.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProxyKind {
    /// SOCKS version 5. Server names are resolved by the proxy.
    #[default]
    Socks5,
    /// An HTTP proxy, which carries every protocol through CONNECT tunnels.
    Http,
    /// SOCKS version 4a, which older system settings can name. Server names
    /// are resolved by the proxy. Settings don't offer it.
    Socks4a,
}

/// A proxy's user name and password.
#[derive(Clone, PartialEq, Eq)]
pub struct Credentials {
    pub user: String,
    pub password: String,
}

impl fmt::Debug for Credentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Credentials")
            .field("user", &self.user)
            .finish_non_exhaustive()
    }
}

/// A proxy server and how to sign in to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProxyServer {
    pub kind: ProxyKind,
    pub host: String,
    pub port: u16,
    pub credentials: Option<Credentials>,
}

impl ProxyServer {
    /// `host:port`, with an IPv6 address in brackets.
    pub fn label(&self) -> String {
        if self.host.contains(':') && !self.host.starts_with('[') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }

    /// The proxy as reqwest takes it, with the user name and password in
    /// it. SOCKS5 names are resolved by the proxy (`socks5h`).
    pub fn url(&self) -> Option<url::Url> {
        let scheme = match self.kind {
            ProxyKind::Socks5 => "socks5h",
            ProxyKind::Http => "http",
            ProxyKind::Socks4a => "socks4a",
        };
        let mut url = url::Url::parse(&format!("{scheme}://{}", self.label())).ok()?;
        if let Some(credentials) = &self.credentials {
            url.set_username(&credentials.user).ok()?;
            url.set_password(Some(&credentials.password)).ok()?;
        }
        Some(url)
    }

    fn io_error(&self, source: io::Error) -> ProxyError {
        ProxyError::Io {
            proxy: self.label(),
            source,
        }
    }

    fn protocol(&self, detail: &str) -> ProxyError {
        ProxyError::Protocol {
            proxy: self.label(),
            detail: detail.to_owned(),
        }
    }

    fn credentials_needed(&self) -> ProxyError {
        ProxyError::CredentialsNeeded {
            proxy: self.label(),
        }
    }
}

/// Where a connection goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Route {
    /// Straight to the server.
    Direct,
    /// Through this proxy.
    Proxy(ProxyServer),
}

/// Why a connection through a proxy failed.
#[derive(Debug, thiserror::Error)]
pub enum ProxyError {
    #[error("could not connect to the proxy {proxy}: {source}")]
    Unreachable {
        proxy: String,
        #[source]
        source: io::Error,
    },
    #[error("the proxy {proxy} did not answer in time")]
    TimedOut { proxy: String },
    #[error("the proxy {proxy} needs a user name and password")]
    CredentialsNeeded { proxy: String },
    #[error("the proxy {proxy} did not accept the user name and password")]
    CredentialsRejected { proxy: String },
    /// The proxy asks for a way of signing in that isn't supported, such as
    /// Kerberos or NTLM. `offered` names the ways, when the proxy said.
    #[error(
        "the proxy {proxy} asks for a way of signing in Flectar Mail doesn't support{}",
        in_parentheses(.offered)
    )]
    UnsupportedSignIn { proxy: String, offered: String },
    #[error("the proxy {proxy} could not connect to {target}: {reason}")]
    Refused {
        proxy: String,
        target: String,
        reason: String,
    },
    #[error("the proxy {proxy} broke off the connection: {detail}")]
    Protocol { proxy: String, detail: String },
    #[error("the connection to the proxy {proxy} failed: {source}")]
    Io {
        proxy: String,
        #[source]
        source: io::Error,
    },
    /// The proxy settings can't be used as they are.
    #[error("{0}")]
    Settings(String),
}

/// ` (text)`, or nothing for no text.
fn in_parentheses(text: &str) -> String {
    if text.is_empty() {
        String::new()
    } else {
        format!(" ({text})")
    }
}

impl From<ProxyError> for io::Error {
    fn from(error: ProxyError) -> Self {
        let kind = match &error {
            ProxyError::Unreachable { source, .. } | ProxyError::Io { source, .. } => source.kind(),
            ProxyError::TimedOut { .. } => io::ErrorKind::TimedOut,
            ProxyError::CredentialsNeeded { .. }
            | ProxyError::CredentialsRejected { .. }
            | ProxyError::UnsupportedSignIn { .. } => io::ErrorKind::PermissionDenied,
            ProxyError::Refused { .. } => io::ErrorKind::ConnectionRefused,
            ProxyError::Protocol { .. } => io::ErrorKind::InvalidData,
            ProxyError::Settings(_) => io::ErrorKind::InvalidInput,
        };
        io::Error::new(kind, error)
    }
}

/// Whether `host` is a server name or address a proxy can be asked for, as
/// the proxy's own name must be.
pub fn check_server_name(host: &str) -> Result<(), ProxyError> {
    tunnel::check_name(host)
}

/// The proxy error behind an I/O error, if a proxy caused it.
pub fn proxy_error(error: &io::Error) -> Option<&ProxyError> {
    error.get_ref()?.downcast_ref()
}

/// Open a TCP connection to `host`:`port` along `route`. Through a proxy,
/// the stream carries the tunnel, so the protocol and its TLS run on it as
/// they would on a direct connection.
pub async fn connect(route: &Route, host: &str, port: u16) -> io::Result<TcpStream> {
    let server = match route {
        Route::Direct => return super::connect_tcp(host, port).await,
        Route::Proxy(server) => server,
    };
    if refused::recently(server) {
        return Err(ProxyError::CredentialsRejected {
            proxy: server.label(),
        }
        .into());
    }
    let mut stream = super::connect_tcp(&server.host, server.port)
        .await
        .map_err(|source| ProxyError::Unreachable {
            proxy: server.label(),
            source,
        })?;
    let opened = tokio::time::timeout(
        super::timeouts::scaled(HANDSHAKE_TIMEOUT),
        tunnel::open(&mut stream, server, host, port),
    )
    .await
    .map_err(|_| ProxyError::TimedOut {
        proxy: server.label(),
    })?;
    match opened {
        Ok(()) => {
            refused::forget(server);
            Ok(stream)
        }
        Err(error) => {
            if matches!(error, ProxyError::CredentialsRejected { .. }) {
                refused::remember(server);
            }
            Err(error.into())
        }
    }
}

/// Proxies that turned down their user name and password a moment ago. A
/// proxy counts with its user name and password, so other ones are tried at
/// once.
mod refused {
    use super::{ProxyServer, REFUSED_HOLD_OFF};
    use std::sync::Mutex;
    use std::time::Instant;

    static REFUSED: Mutex<Vec<(ProxyServer, Instant)>> = Mutex::new(Vec::new());

    fn with<T>(work: impl FnOnce(&mut Vec<(ProxyServer, Instant)>) -> T) -> T {
        let mut refused = REFUSED
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        refused.retain(|(_, at)| at.elapsed() < REFUSED_HOLD_OFF);
        work(&mut refused)
    }

    pub(super) fn recently(server: &ProxyServer) -> bool {
        server.credentials.is_some()
            && with(|refused| refused.iter().any(|(known, _)| known == server))
    }

    pub(super) fn remember(server: &ProxyServer) {
        with(|refused| {
            refused.retain(|(known, _)| known != server);
            refused.push((server.clone(), Instant::now()));
        });
    }

    pub(super) fn forget(server: &ProxyServer) {
        with(|refused| refused.retain(|(known, _)| known != server));
    }

    pub(super) fn forget_all() {
        with(Vec::clear);
    }
}

/// Try proxies that turned a sign-in down again at once, as after the user
/// saved the settings.
pub fn try_refused_proxies_again() {
    refused::forget_all();
}

/// A reqwest client builder for `owner`, whose connections go through the
/// proxy `owner`'s settings choose and check certificates as mail
/// connections do. The proxy is chosen again for every new connection, so a
/// change in Settings applies without a new client.
pub fn http_builder(owner: Owner) -> reqwest::ClientBuilder {
    crate::net::tls::client_builder().proxy(reqwest_proxy(owner))
}

/// The proxy rule of `owner`'s HTTP clients, for clients built elsewhere.
pub fn reqwest_proxy(owner: Owner) -> reqwest::Proxy {
    reqwest::Proxy::custom(move |url| http_proxy(owner, url))
}

/// Open a mail protocol connection of `owner` to `host`:`port`, through the
/// proxy its settings choose. `scheme` names the protocol to the system's
/// settings, as in `imaps` or `smtp`. When the system lists several proxies,
/// each is tried in turn, and a direct connection only when the list says so.
pub async fn connect_for(
    owner: Owner,
    scheme: &str,
    host: &str,
    port: u16,
) -> io::Result<TcpStream> {
    let routes = mail_routes(owner, scheme, host, port);
    // When every route fails, the first one's error goes back: the route the
    // settings prefer, as with the addresses of a server.
    let mut first_error = None;
    for route in &routes {
        match connect(route, host, port).await {
            Ok(stream) => return Ok(stream),
            Err(error) => {
                first_error.get_or_insert(error);
            }
        }
    }
    Err(first_error.unwrap_or_else(|| {
        ProxyError::Settings(
            "the system's proxy settings name a kind of proxy Flectar Mail can't use for mail"
                .into(),
        )
        .into()
    }))
}
