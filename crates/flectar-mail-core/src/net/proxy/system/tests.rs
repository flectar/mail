use super::*;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

fn environment(variables: &[(&str, &str)]) -> Environment {
    let variables: HashMap<String, String> = variables
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect();
    Environment::from_lookup(|name| variables.get(name).cloned())
}

fn url(text: &str) -> Url {
    Url::parse(text).unwrap()
}

/// A source that names fixed proxies for every URL.
struct Fixed(Vec<&'static str>);

impl SystemProxies for Fixed {
    fn lookup(&self, _url: &Url) -> Vec<Url> {
        self.0.iter().map(|proxy| url(proxy)).collect()
    }
}

#[test]
fn environment_reads_variables_as_reqwest_did() {
    let env = environment(&[
        ("http_proxy", "web.proxy:3128"),
        ("HTTPS_PROXY", "http://secure.proxy:3129"),
        ("https_proxy", "http://ignored.proxy:3129"),
        ("ALL_PROXY", "socks5h://all.proxy:1080"),
        ("no_proxy", ".internal.example, 10.0.0.0/8"),
    ]);
    assert_eq!(
        env.lookup(&url("http://example.com/")),
        [url("http://web.proxy:3128")]
    );
    assert_eq!(
        env.lookup(&url("https://example.com/")),
        [url("http://secure.proxy:3129")]
    );
    assert_eq!(
        env.lookup(&url("imaps://imap.example.com:993")),
        [url("socks5h://all.proxy:1080")]
    );
    assert!(
        env.lookup(&url("imaps://imap.internal.example:993"))
            .is_empty()
    );
    assert!(env.lookup(&url("https://10.1.2.3/")).is_empty());
}

#[test]
fn a_cgi_script_ignores_http_proxy() {
    let request = url("http://example.com/");
    let env = environment(&[("HTTP_PROXY", "http://web.proxy:80")]);
    assert_eq!(env.lookup(&request), [url("http://web.proxy:80")]);
    let cgi = environment(&[
        ("HTTP_PROXY", "http://injected:80"),
        ("REQUEST_METHOD", "GET"),
    ]);
    assert!(cgi.lookup(&request).is_empty());
}

#[test]
fn mail_follows_all_proxy_but_not_the_web_proxies() {
    let env = environment(&[
        ("http_proxy", "http://web.proxy:3128"),
        ("https_proxy", "http://web.proxy:3128"),
    ]);
    assert_eq!(
        routes_from(&env, "imaps", "imap.example.com", 993),
        [Route::Direct]
    );
}

#[test]
fn mail_takes_the_systems_socks_proxies_in_order() {
    let source = Fixed(vec![
        "socks5://ana:p%40ss@socks.proxy:1080",
        "http://web.proxy",
        "socks4://old.proxy",
        "direct://",
    ]);
    assert_eq!(
        routes_from(&source, "imaps", "imap.example.com", 993),
        [
            Route::Proxy(ProxyServer {
                kind: ProxyKind::Socks5,
                host: "socks.proxy".into(),
                port: 1080,
                credentials: Some(Credentials {
                    user: "ana".into(),
                    password: "p@ss".into(),
                }),
            }),
            Route::Proxy(ProxyServer {
                kind: ProxyKind::Socks4a,
                host: "old.proxy".into(),
                port: 1080,
                credentials: None,
            }),
            Route::Direct,
        ]
    );
}

#[test]
fn mail_goes_past_the_systems_web_proxies() {
    for proxies in [
        vec!["http://web.proxy:3128"],
        vec!["https://tls.proxy:443", "http://web.proxy:3128"],
    ] {
        assert_eq!(
            routes_from(&Fixed(proxies), "imaps", "imap.example.com", 993),
            [Route::Direct]
        );
    }
}

#[test]
fn http_requests_resolve_socks_names_at_the_proxy() {
    let source = Fixed(vec!["socks5://socks.proxy"]);
    assert_eq!(
        proxy_from(&source, &url("https://example.com/")),
        Some(url("socks5h://socks.proxy:1080"))
    );
    let direct = Fixed(vec!["direct://"]);
    assert_eq!(proxy_from(&direct, &url("https://example.com/")), None);
    let none = Fixed(Vec::new());
    assert_eq!(proxy_from(&none, &url("https://example.com/")), None);
}

#[test]
fn looks_up_ipv6_servers_in_brackets() {
    struct Echo;
    impl SystemProxies for Echo {
        fn lookup(&self, url: &Url) -> Vec<Url> {
            assert_eq!(url.as_str(), "imaps://[2001:db8::7]:993");
            Vec::new()
        }
    }
    assert_eq!(
        routes_from(&Echo, "imaps", "2001:db8::7", 993),
        [Route::Direct]
    );
}

#[test]
fn the_environment_comes_before_the_system() {
    let system = Fixed(vec!["socks5://socks.proxy:1080"]);
    let socks = |host: &str| {
        Route::Proxy(ProxyServer {
            kind: ProxyKind::Socks5,
            host: host.into(),
            port: 1080,
            credentials: None,
        })
    };
    let unset = environment(&[("NO_PROXY", "intranet.example")]);
    assert_eq!(
        routes_from(
            chosen(&unset, Some(&system)),
            "imaps",
            "imap.example.com",
            993
        ),
        [socks("socks.proxy")]
    );
    // A program started with its own proxy variables uses them, for mail too.
    let set = environment(&[("https_proxy", "http://web.proxy:3128")]);
    assert_eq!(
        routes_from(
            chosen(&set, Some(&system)),
            "imaps",
            "imap.example.com",
            993
        ),
        [Route::Direct]
    );
    assert_eq!(
        proxy_from(chosen(&set, Some(&system)), &url("https://example.com/")),
        Some(url("http://web.proxy:3128"))
    );
    let all = environment(&[("ALL_PROXY", "socks5://env.proxy:1080")]);
    assert_eq!(
        routes_from(
            chosen(&all, Some(&system)),
            "imaps",
            "imap.example.com",
            993
        ),
        [socks("env.proxy")]
    );
    assert_eq!(
        routes_from(chosen(&unset, None), "imaps", "imap.example.com", 993),
        [Route::Direct]
    );
}

#[test]
fn each_socks_server_is_tried_once() {
    // GLib names a SOCKS server once per version when the settings don't say
    // which it speaks.
    let source = Fixed(vec![
        "socks5://socks.proxy:1080",
        "socks4a://socks.proxy:1080",
        "socks4://socks.proxy:1080",
    ]);
    let server = |kind| {
        Route::Proxy(ProxyServer {
            kind,
            host: "socks.proxy".into(),
            port: 1080,
            credentials: None,
        })
    };
    assert_eq!(
        routes_from(&source, "smtp", "smtp.example.com", 587),
        [server(ProxyKind::Socks5), server(ProxyKind::Socks4a)]
    );
}

/// A source that counts the times it's asked.
struct Counting(Arc<AtomicUsize>);

impl SystemProxies for Counting {
    fn lookup(&self, _url: &Url) -> Vec<Url> {
        self.0.fetch_add(1, Ordering::SeqCst);
        vec![url("socks5://socks.proxy:1080")]
    }
}

static TEST_GENERATION: AtomicU64 = AtomicU64::new(0);

#[test]
fn answers_are_kept_per_server_until_the_settings_change() {
    let asked = Arc::new(AtomicUsize::new(0));
    let cached = Cached {
        generation: || TEST_GENERATION.load(Ordering::SeqCst),
        ..Cached::new(Counting(Arc::clone(&asked)))
    };
    let lookup = |text: &str| cached.lookup(&url(text));
    lookup("imaps://imap.example.com:993");
    lookup("imaps://imap.example.com:993");
    lookup("https://imap.example.com/a");
    lookup("https://imap.example.com/b");
    assert_eq!(asked.load(Ordering::SeqCst), 2);
    lookup("smtp://smtp.example.com:587");
    assert_eq!(asked.load(Ordering::SeqCst), 3);
    TEST_GENERATION.fetch_add(1, Ordering::SeqCst);
    assert_eq!(
        lookup("imaps://imap.example.com:993"),
        [url("socks5://socks.proxy:1080")]
    );
    assert_eq!(asked.load(Ordering::SeqCst), 4);
}

/// A source that answers only once another task has run.
struct Waiting(Mutex<std::sync::mpsc::Receiver<()>>);

impl SystemProxies for Waiting {
    fn lookup(&self, _url: &Url) -> Vec<Url> {
        let signal = self.0.lock().unwrap().recv_timeout(Duration::from_secs(10));
        assert!(signal.is_ok(), "no other task ran during the lookup");
        Vec::new()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_slow_lookup_leaves_the_workers_other_tasks_running() {
    let (signal, wait) = std::sync::mpsc::channel();
    let cached = Arc::new(Cached::new(Waiting(Mutex::new(wait))));
    // With one worker, the other task runs only if the lookup gives the
    // worker up while it waits.
    let lookup = tokio::spawn(async move { cached.lookup(&url("imaps://imap.example.com:993")) });
    tokio::spawn(async move { signal.send(()).unwrap() });
    assert!(lookup.await.unwrap().is_empty());
}

#[tokio::test]
async fn lookups_work_on_a_single_threaded_runtime() {
    let asked = Arc::new(AtomicUsize::new(0));
    let cached = Cached::new(Counting(Arc::clone(&asked)));
    assert_eq!(
        cached.lookup(&url("imaps://imap.example.com:993")),
        [url("socks5://socks.proxy:1080")]
    );
    assert_eq!(asked.load(Ordering::SeqCst), 1);
}
