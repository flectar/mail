//! Linux desktops' proxy settings, through the desktop portal's proxy
//! resolver. It answers with the desktop's own settings, such as GNOME's or
//! KDE's, runs their proxy auto-configuration script, and serves Flatpak
//! sandboxes too, as GLib's proxy resolver asks it there. A desktop without
//! the portal leaves the environment's proxy variables, which come first
//! anyway when set.

use flectar_mail_core::net::proxy::SystemProxies;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use url::Url;
use zbus::blocking::{Connection, Proxy, connection, proxy};

/// How long the portal may take to answer: it may fetch and run a proxy
/// auto-configuration script first.
const ANSWER_TIME: Duration = Duration::from_secs(10);
/// How long a portal that is missing or failed is left alone.
const PORTAL_REST: Duration = Duration::from_secs(5 * 60);

pub(super) struct Reader {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    connection: Option<Connection>,
    failed: Option<Instant>,
}

impl Reader {
    pub(super) fn new() -> Self {
        Self {
            state: Mutex::new(State::default()),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl SystemProxies for Reader {
    fn lookup(&self, url: &Url) -> Vec<Url> {
        let connection = {
            let mut state = self.state();
            if state
                .failed
                .is_some_and(|failed| failed.elapsed() < PORTAL_REST)
            {
                return Vec::new();
            }
            match &state.connection {
                Some(connection) => connection.clone(),
                None => match connection::Builder::session()
                    .and_then(|builder| builder.method_timeout(ANSWER_TIME).build())
                {
                    Ok(connection) => state.connection.insert(connection).clone(),
                    Err(error) => {
                        tracing::debug!(%error, "no session bus to ask for the system's proxy");
                        state.failed = Some(Instant::now());
                        return Vec::new();
                    }
                },
            }
        };
        match ask(&connection, url) {
            Ok(proxies) => proxies,
            Err(error) => {
                tracing::debug!(%error, "the desktop portal didn't name a proxy");
                let mut state = self.state();
                state.failed = Some(Instant::now());
                state.connection = None;
                Vec::new()
            }
        }
    }
}

/// The portal's answer: proxy URIs such as `http://proxy:3128`,
/// `socks5://proxy:1080` or `direct://`, in the order to try them.
fn ask(connection: &Connection, url: &Url) -> zbus::Result<Vec<Url>> {
    let portal: Proxy<'_> = proxy::Builder::new(connection)
        .destination("org.freedesktop.portal.Desktop")?
        .path("/org/freedesktop/portal/desktop")?
        .interface("org.freedesktop.portal.ProxyResolver")?
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()?;
    let proxies: Vec<String> = portal.call("Lookup", &(url.as_str(),))?;
    Ok(proxies
        .iter()
        .filter_map(|proxy| Url::parse(proxy).ok())
        .collect())
}
