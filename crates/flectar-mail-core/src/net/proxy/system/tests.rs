use super::*;
use std::collections::HashMap;

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
        mail_routes(Some(&env), "imaps", "imap.example.com", 993),
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
        mail_routes(Some(&source), "imaps", "imap.example.com", 993),
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
            mail_routes(Some(&Fixed(proxies)), "imaps", "imap.example.com", 993),
            [Route::Direct]
        );
    }
}

#[test]
fn http_requests_resolve_socks_names_at_the_proxy() {
    let source = Fixed(vec!["socks5://socks.proxy"]);
    assert_eq!(
        http_proxy(Some(&source), &url("https://example.com/")),
        Some(url("socks5h://socks.proxy:1080"))
    );
    let direct = Fixed(vec!["direct://"]);
    assert_eq!(
        http_proxy(Some(&direct), &url("https://example.com/")),
        None
    );
    let none = Fixed(Vec::new());
    assert_eq!(http_proxy(Some(&none), &url("https://example.com/")), None);
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
        mail_routes(Some(&Echo), "imaps", "2001:db8::7", 993),
        [Route::Direct]
    );
}
