//! The proxy settings in force, for every connection the app makes.
//!
//! Connections ask here as they open, so a change applies to the next
//! connection without rebuilding clients. Settings and accounts write here
//! when they load or change; see `Core::apply_proxy_settings`.

use super::system::{self, SystemProxies};
use super::{Bypass, Credentials, ManualProxy, ProxyServer, Route};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

/// Whose connection this is: one account's, or the app's own, such as
/// remote images or the update check.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Owner {
    App,
    Account(i64),
}

/// The app's proxy setting, with its password.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum AppProxy {
    #[default]
    System,
    Direct,
    Manual(Manual),
}

/// A proxy typed in by hand, ready to use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manual {
    pub server: ProxyServer,
    pub bypass: Bypass,
}

impl Manual {
    /// `proxy` with the password from the keyring.
    pub fn new(proxy: &ManualProxy, password: Option<String>) -> Self {
        let user = proxy.user.trim();
        let credentials = (!user.is_empty()).then(|| Credentials {
            user: user.to_owned(),
            password: password.unwrap_or_default(),
        });
        Self {
            server: ProxyServer {
                kind: proxy.kind,
                host: proxy.host.trim().to_owned(),
                port: proxy.port,
                credentials,
            },
            bypass: Bypass::parse(&proxy.bypass),
        }
    }

    fn route(&self, host: &str) -> Route {
        if self.bypass.skips(host) {
            Route::Direct
        } else {
            Route::Proxy(self.server.clone())
        }
    }
}

/// One account's choice when it isn't the app's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AccountOverride {
    Direct,
    Own(Manual),
}

#[derive(Default)]
struct Config {
    app: AppProxy,
    accounts: HashMap<i64, AccountOverride>,
}

fn config() -> &'static RwLock<Arc<Config>> {
    static CONFIG: OnceLock<RwLock<Arc<Config>>> = OnceLock::new();
    CONFIG.get_or_init(Default::default)
}

fn current() -> Arc<Config> {
    config()
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

/// Counts the changes, so clients built before one can be told apart.
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// Which proxy settings are in force: it changes whenever they do.
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Acquire)
}

fn update(change: impl FnOnce(&mut Config)) {
    #[cfg(test)]
    let _turn = test_turns::wait();
    let mut guard = config()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut next = Config {
        app: guard.app.clone(),
        accounts: guard.accounts.clone(),
    };
    change(&mut next);
    if next.app != guard.app || next.accounts != guard.accounts {
        *guard = Arc::new(next);
        GENERATION.fetch_add(1, Ordering::AcqRel);
    }
}

/// The proxy settings belong to the process, which the core's tests share,
/// and every core applies its own as it starts. A test that checks the
/// settings in force takes the turn, and other tests' changes wait until it's
/// done. Changes made on the thread that holds the turn, by the test and its
/// own core, go ahead.
#[cfg(test)]
pub(crate) mod test_turns {
    use std::cell::Cell;
    use std::sync::{PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

    static TURN: RwLock<()> = RwLock::new(());

    thread_local! {
        static HOLDING: Cell<bool> = const { Cell::new(false) };
    }

    /// The turn, held until it's dropped.
    pub(crate) struct Turn {
        _guard: RwLockWriteGuard<'static, ()>,
    }

    impl Drop for Turn {
        fn drop(&mut self) {
            HOLDING.set(false);
        }
    }

    /// Take the turn, waiting for another test that holds it.
    pub(crate) fn take() -> Turn {
        let guard = TURN.write().unwrap_or_else(PoisonError::into_inner);
        HOLDING.set(true);
        Turn { _guard: guard }
    }

    /// Wait for the turn to be free, unless this thread holds it.
    pub(super) fn wait() -> Option<RwLockReadGuard<'static, ()>> {
        (!HOLDING.get()).then(|| TURN.read().unwrap_or_else(PoisonError::into_inner))
    }
}

/// The app's proxy in force, for tests.
#[cfg(test)]
pub(crate) fn app_in_force() -> AppProxy {
    current().app.clone()
}

/// An account's own choice in force, for tests.
#[cfg(test)]
pub(crate) fn account_in_force(account_id: i64) -> Option<AccountOverride> {
    current().accounts.get(&account_id).cloned()
}

/// Use `proxy` for the app's own connections and every account that
/// follows the app.
pub fn set_app(proxy: AppProxy) {
    update(|config| config.app = proxy);
}

/// Give an account its own choice, or `None` to follow the app.
pub fn set_account(account_id: i64, proxy: Option<AccountOverride>) {
    update(|config| match proxy {
        Some(proxy) => {
            config.accounts.insert(account_id, proxy);
        }
        None => {
            config.accounts.remove(&account_id);
        }
    });
}

/// What a connection from `owner` follows, apart from the system's settings.
enum Choice {
    Route(Route),
    System,
}

impl Config {
    fn choice(&self, owner: Owner, host: &str) -> Choice {
        // Loopback never goes through a proxy, whatever the settings say.
        if Bypass::default().skips(host) {
            return Choice::Route(Route::Direct);
        }
        if let Owner::Account(id) = owner {
            match self.accounts.get(&id) {
                Some(AccountOverride::Direct) => return Choice::Route(Route::Direct),
                Some(AccountOverride::Own(manual)) => return Choice::Route(manual.route(host)),
                None => {}
            }
        }
        match &self.app {
            AppProxy::Direct => Choice::Route(Route::Direct),
            AppProxy::Manual(manual) => Choice::Route(manual.route(host)),
            AppProxy::System => Choice::System,
        }
    }

    fn mail_routes(&self, owner: Owner, scheme: &str, host: &str, port: u16) -> Vec<Route> {
        match self.choice(owner, host) {
            Choice::Route(route) => vec![route],
            Choice::System => system::mail_routes(system_source().as_deref(), scheme, host, port),
        }
    }

    fn http_proxy(&self, owner: Owner, url: &url::Url) -> Option<url::Url> {
        let host = url.host_str()?;
        match self.choice(owner, host) {
            Choice::Route(Route::Direct) => None,
            Choice::Route(Route::Proxy(server)) => server.url(),
            Choice::System => system::http_proxy(system_source().as_deref(), url),
        }
    }
}

/// The routes to try, in order, for a mail protocol connection of `owner`
/// to `host`:`port`. `scheme` names the protocol to the system's settings,
/// as in `imap`, `imaps`, `pop3s`, `smtp` or `sieve`.
pub fn mail_routes(owner: Owner, scheme: &str, host: &str, port: u16) -> Vec<Route> {
    current().mail_routes(owner, scheme, host, port)
}

/// The proxy for an HTTP request of `owner` to `url`, as the URL reqwest
/// connects through, or `None` to connect directly.
pub fn http_proxy(owner: Owner, url: &url::Url) -> Option<url::Url> {
    current().http_proxy(owner, url)
}

fn system_slot() -> &'static RwLock<Option<Arc<dyn SystemProxies>>> {
    static SYSTEM: OnceLock<RwLock<Option<Arc<dyn SystemProxies>>>> = OnceLock::new();
    SYSTEM.get_or_init(Default::default)
}

fn system_source() -> Option<Arc<dyn SystemProxies>> {
    system_slot()
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

/// Read the system's proxy settings through `source`, which the host
/// provides. Its answers are kept for a minute. Without one, only the
/// environment's proxy variables count.
pub fn set_system_source(source: impl SystemProxies + 'static) {
    let source: Arc<dyn SystemProxies> = Arc::new(system::Cached::new(source));
    *system_slot()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(source);
    GENERATION.fetch_add(1, Ordering::AcqRel);
}

/// Tell connections that the system's proxy settings changed, as when the
/// computer joins another network: answers kept from before go, and so do
/// HTTP clients with their idle connections.
pub fn system_changed() {
    GENERATION.fetch_add(1, Ordering::AcqRel);
}

/// An HTTP client shared by the requests of each owner, built again after the
/// proxy settings change. The client's idle connections go with it, so none
/// opened before a change is used after it.
pub struct SharedClient {
    build: fn(reqwest::ClientBuilder) -> reqwest::ClientBuilder,
    /// Each owner's client and the generation it was built in. Few owners
    /// make requests, so a list is enough.
    clients: Mutex<Vec<(Owner, u64, reqwest::Client)>>,
}

impl SharedClient {
    /// `build` sets everything apart from the proxy, such as timeouts.
    pub const fn new(build: fn(reqwest::ClientBuilder) -> reqwest::ClientBuilder) -> Self {
        Self {
            build,
            clients: Mutex::new(Vec::new()),
        }
    }

    pub fn get(&self, owner: Owner) -> reqwest::Result<reqwest::Client> {
        let generation = generation();
        let mut clients = self
            .clients
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Clients built before a change go, with their connections.
        clients.retain(|(_, built, _)| *built == generation);
        if let Some((_, _, client)) = clients.iter().find(|(known, ..)| *known == owner) {
            return Ok(client.clone());
        }
        let client = (self.build)(super::http_builder(owner)).build()?;
        clients.push((owner, generation, client.clone()));
        Ok(client)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::proxy::ProxyKind;

    fn manual(kind: ProxyKind, host: &str, user: &str, bypass: &str) -> Manual {
        Manual::new(
            &ManualProxy {
                kind,
                host: host.into(),
                port: 1080,
                user: user.into(),
                bypass: bypass.into(),
            },
            Some("s3cret".into()),
        )
    }

    fn url(text: &str) -> url::Url {
        url::Url::parse(text).unwrap()
    }

    #[test]
    fn owners_follow_the_app_or_their_own_choice() {
        let socks = manual(ProxyKind::Socks5, "socks.proxy", "ana", "intranet.example");
        let config = Config {
            app: AppProxy::Manual(socks.clone()),
            accounts: HashMap::from([
                (7, AccountOverride::Direct),
                (
                    8,
                    AccountOverride::Own(manual(ProxyKind::Http, "own.proxy", "", "")),
                ),
            ]),
        };

        let through_socks = Route::Proxy(socks.server.clone());
        assert_eq!(
            config.mail_routes(Owner::App, "imaps", "imap.example.com", 993),
            std::slice::from_ref(&through_socks)
        );
        assert_eq!(
            config.mail_routes(Owner::Account(1), "smtp", "smtp.example.com", 587),
            [through_socks]
        );
        assert_eq!(
            config.mail_routes(Owner::Account(1), "imaps", "mail.intranet.example", 993),
            [Route::Direct]
        );
        assert_eq!(
            config.mail_routes(Owner::Account(7), "imaps", "imap.example.com", 993),
            [Route::Direct]
        );
        assert_eq!(
            config.http_proxy(Owner::App, &url("https://example.com/")),
            Some(url("socks5h://ana:s3cret@socks.proxy:1080"))
        );
        assert_eq!(
            config.http_proxy(Owner::Account(7), &url("https://example.com/")),
            None
        );
        assert_eq!(
            config.http_proxy(Owner::Account(8), &url("https://example.com/")),
            Some(url("http://own.proxy:1080"))
        );
    }

    #[test]
    fn other_tests_wait_while_one_holds_the_turn() {
        let turn = test_turns::take();
        set_app(AppProxy::Direct);
        let changed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let other = std::thread::spawn({
            let changed = Arc::clone(&changed);
            move || {
                // Another test's core, applying its settings as it starts.
                set_app(AppProxy::System);
                changed.store(true, Ordering::SeqCst);
            }
        });
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(!changed.load(Ordering::SeqCst));
        assert_eq!(app_in_force(), AppProxy::Direct);
        drop(turn);
        other.join().unwrap();
        assert!(changed.load(Ordering::SeqCst));
    }

    #[test]
    fn loopback_never_goes_through_a_proxy() {
        let config = Config {
            app: AppProxy::Manual(manual(ProxyKind::Socks5, "socks.proxy", "", "")),
            accounts: HashMap::from([(
                8,
                AccountOverride::Own(manual(ProxyKind::Http, "own.proxy", "", "")),
            )]),
        };
        assert_eq!(
            config.http_proxy(Owner::App, &url("http://127.0.0.1:8080/")),
            None
        );
        assert_eq!(
            config.mail_routes(Owner::Account(8), "imap", "localhost", 1143),
            [Route::Direct]
        );
    }

    #[test]
    fn credentials_go_into_the_proxy_url_encoded() {
        let config = Config {
            app: AppProxy::Manual(Manual::new(
                &ManualProxy {
                    kind: ProxyKind::Http,
                    host: "proxy.example".into(),
                    port: 3128,
                    user: "ana@example.com".into(),
                    bypass: String::new(),
                },
                Some("p:w/d".into()),
            )),
            accounts: HashMap::new(),
        };
        let proxy = config
            .http_proxy(Owner::App, &url("https://example.com/"))
            .unwrap();
        assert_eq!(
            proxy.as_str(),
            "http://ana%40example.com:p%3Aw%2Fd@proxy.example:3128/"
        );
    }
}
