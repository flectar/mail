//! macOS and iOS proxy settings, as CFNetworkCopySystemProxySettings gives
//! them: a proxy for HTTP, one for HTTPS and a SOCKS proxy, each turned on or
//! off, a list of servers that skip them, and a proxy auto-configuration
//! script, at an address, written out, or found by automatic discovery.
//! Turning them into proxies follows Chromium's reading of the same keys.

use flectar_mail_core::net::proxy::Bypass;
use url::Url;

/// A proxy server as the settings name it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Server {
    pub host: String,
    pub port: Option<u16>,
}

/// Where a proxy auto-configuration script comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Script {
    Address(String),
    Text(String),
}

/// The fixed settings and the script, read from the dictionary.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Settings {
    pub http: Option<Server>,
    pub https: Option<Server>,
    pub socks: Option<Server>,
    /// "Bypass proxy settings for these hosts and domains".
    pub exceptions: Vec<String>,
    /// "Exclude simple hostnames": names without a dot skip the proxies.
    pub exclude_simple_hostnames: bool,
    pub script: Option<Script>,
    /// "Auto proxy discovery" (WPAD), when there is no script of its own.
    pub discover: bool,
}

/// The address automatic discovery asks, as browsers ask it by name: the
/// `wpad` server of the network's own domain.
pub(super) const DISCOVERY_ADDRESS: &str = "http://wpad/wpad.dat";

impl Settings {
    /// The script to run, if any.
    pub(super) fn script(&self) -> Option<Script> {
        self.script.clone().or_else(|| {
            self.discover
                .then(|| Script::Address(DISCOVERY_ADDRESS.to_owned()))
        })
    }

    /// The proxies the fixed settings name for a connection to `url`, in the
    /// order to try them. Empty means a direct connection.
    pub(super) fn fixed(&self, url: &Url) -> Vec<Url> {
        if let Some(host) = url.host_str() {
            let mut list = self.exceptions.join(",");
            if self.exclude_simple_hostnames {
                list.push_str(",<local>");
            }
            if Bypass::parse_wildcards(&list).skips(host) {
                return Vec::new();
            }
        }
        let web = match url.scheme() {
            "http" => self.http.as_ref(),
            "https" => self.https.as_ref(),
            _ => None,
        };
        if let Some(server) = web {
            return server_url("http", server, 80).into_iter().collect();
        }
        // SOCKS serves whatever no other proxy does. Apple's SOCKS proxies
        // speak version 5.
        self.socks
            .as_ref()
            .and_then(|server| server_url("socks5", server, 1080))
            .into_iter()
            .collect()
    }
}

/// A proxy a script names, as CFNetwork reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Answer {
    Direct,
    /// `PROXY host:port`: CFNetwork reports it as an HTTP proxy for `http`
    /// URLs and an HTTPS one for `https` URLs, both reached over HTTP.
    Http(Server),
    Socks(Server),
    /// Anything else, such as an FTP proxy.
    Other,
}

/// The proxies a script's answers name, in order.
pub(super) fn from_script(answers: &[Answer]) -> Vec<Url> {
    answers
        .iter()
        .filter_map(|answer| match answer {
            Answer::Direct => Url::parse("direct://").ok(),
            Answer::Http(server) => server_url("http", server, 80),
            Answer::Socks(server) => server_url("socks5", server, 1080),
            Answer::Other => None,
        })
        .collect()
}

fn server_url(scheme: &str, server: &Server, default_port: u16) -> Option<Url> {
    let host = server.host.trim();
    if host.is_empty() {
        return None;
    }
    let host = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    let port = server
        .port
        .filter(|port| *port != 0)
        .unwrap_or(default_port);
    let url = Url::parse(&format!("{scheme}://{host}:{port}")).ok()?;
    url.host_str()?;
    Some(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(text: &str) -> Url {
        Url::parse(text).unwrap()
    }

    fn named(proxies: &[Url]) -> Vec<&str> {
        proxies.iter().map(Url::as_str).collect()
    }

    fn server(host: &str, port: u16) -> Option<Server> {
        Some(Server {
            host: host.into(),
            port: Some(port),
        })
    }

    #[test]
    fn web_proxies_serve_the_web_and_socks_the_rest() {
        let settings = Settings {
            http: server("web.proxy", 3128),
            https: server("secure.proxy", 3129),
            socks: server("socks.proxy", 1080),
            ..Settings::default()
        };
        assert_eq!(
            named(&settings.fixed(&url("http://example.com/"))),
            ["http://web.proxy:3128/"]
        );
        assert_eq!(
            named(&settings.fixed(&url("https://example.com/"))),
            ["http://secure.proxy:3129/"]
        );
        assert_eq!(
            named(&settings.fixed(&url("imaps://imap.example.com:993"))),
            ["socks5://socks.proxy:1080"]
        );
    }

    #[test]
    fn without_a_web_proxy_the_web_goes_through_socks() {
        let settings = Settings {
            socks: server("socks.proxy", 1081),
            ..Settings::default()
        };
        assert_eq!(
            named(&settings.fixed(&url("https://example.com/"))),
            ["socks5://socks.proxy:1081"]
        );
        let none = Settings::default();
        assert!(none.fixed(&url("https://example.com/")).is_empty());
    }

    #[test]
    fn exceptions_and_simple_names_go_direct() {
        // macOS's own default list.
        let settings = Settings {
            https: server("secure.proxy", 3129),
            socks: server("socks.proxy", 1080),
            exceptions: vec![
                "*.local".into(),
                "169.254/16".into(),
                "intranet.example".into(),
            ],
            exclude_simple_hostnames: true,
            ..Settings::default()
        };
        for target in [
            "https://printer.local/",
            "https://169.254.1.2/",
            "https://intranet.example/",
            "https://wiki/",
            "imaps://localhost:993",
        ] {
            assert!(settings.fixed(&url(target)).is_empty(), "{target}");
        }
        assert!(
            !settings
                .fixed(&url("https://mail.intranet.example/"))
                .is_empty()
        );
        assert!(
            !settings
                .fixed(&url("imaps://imap.example.com:993"))
                .is_empty()
        );
    }

    #[test]
    fn a_missing_port_takes_the_usual_one() {
        let settings = Settings {
            https: Some(Server {
                host: "2001:db8::7".into(),
                port: None,
            }),
            socks: Some(Server {
                host: "socks.proxy".into(),
                port: Some(0),
            }),
            ..Settings::default()
        };
        assert_eq!(
            named(&settings.fixed(&url("https://example.com/"))),
            ["http://[2001:db8::7]/"]
        );
        assert_eq!(
            named(&settings.fixed(&url("smtp://smtp.example.com:587"))),
            ["socks5://socks.proxy:1080"]
        );
    }

    #[test]
    fn discovery_runs_only_without_a_script() {
        let discovering = Settings {
            discover: true,
            ..Settings::default()
        };
        assert_eq!(
            discovering.script(),
            Some(Script::Address(DISCOVERY_ADDRESS.into()))
        );
        let both = Settings {
            script: Some(Script::Address("http://config.example/proxy.pac".into())),
            ..discovering
        };
        assert_eq!(
            both.script(),
            Some(Script::Address("http://config.example/proxy.pac".into()))
        );
        let written = Settings {
            script: Some(Script::Text(
                "function FindProxyForURL() { return \"DIRECT\"; }".into(),
            )),
            discover: true,
            ..Settings::default()
        };
        assert!(matches!(written.script(), Some(Script::Text(_))));
        assert_eq!(Settings::default().script(), None);
    }

    #[test]
    fn script_answers_become_proxies_in_order() {
        let answers = [
            Answer::Socks(Server {
                host: "socks.proxy".into(),
                port: Some(1080),
            }),
            Answer::Other,
            Answer::Http(Server {
                host: "web.proxy".into(),
                port: Some(8080),
            }),
            Answer::Direct,
        ];
        assert_eq!(
            named(&from_script(&answers)),
            [
                "socks5://socks.proxy:1080",
                "http://web.proxy:8080/",
                "direct://"
            ]
        );
    }
}
