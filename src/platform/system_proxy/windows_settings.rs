//! Windows' fixed proxy settings, as Internet Options keep them: a proxy list
//! such as `proxy:3128` or `http=proxy:3128;https=proxy:3128;socks=socks:1080`,
//! and a list of servers that skip it, with `*` wildcards and `<local>`.
//! (WINHTTP_PROXY_INFO and WinINet "Enabling Internet Functionality".)

use flectar_mail_core::net::proxy::Bypass;
use url::Url;

/// The proxies Windows' fixed settings name for a connection to `url`, in
/// the order to try them. Empty means a direct connection.
pub(super) fn fixed(url: &Url, list: &str, bypass: &str) -> Vec<Url> {
    if let Some(host) = url.host_str()
        && Bypass::parse_wildcards(bypass).skips(host)
    {
        return Vec::new();
    }
    let mut every = None;
    let mut http = None;
    let mut https = None;
    let mut socks = None;
    for entry in list
        .split([';', ' ', '\t', '\r', '\n'])
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
    {
        match entry.split_once('=') {
            Some((scheme, server)) if scheme.eq_ignore_ascii_case("http") => {
                http.get_or_insert(server);
            }
            Some((scheme, server)) if scheme.eq_ignore_ascii_case("https") => {
                https.get_or_insert(server);
            }
            Some((scheme, server)) if scheme.eq_ignore_ascii_case("socks") => {
                socks.get_or_insert(server);
            }
            // FTP and anything else this app never connects to.
            Some(_) => {}
            // A proxy for every protocol, from "Use the same proxy server for
            // all protocols".
            None => {
                every.get_or_insert(entry);
            }
        }
    }
    let web = match url.scheme() {
        "http" => http.or(every),
        "https" => https.or(every),
        _ => None,
    };
    if let Some(server) = web {
        return server_url("http", server, 80).into_iter().collect();
    }
    // SOCKS serves whatever no other entry does. Windows doesn't say which
    // version, so version 5 is tried first and then 4a, as GLib does.
    socks.map(socks_urls).unwrap_or_default()
}

/// The proxies a WinHTTP auto-configuration result names, as in
/// `proxy:8080;proxy2:8080`. WinHTTP leaves out the script's SOCKS proxies
/// and what follows a `DIRECT`, so these are HTTP proxies, unless an entry
/// says otherwise in the documented `scheme=server` form.
pub(super) fn from_script(list: &str) -> Vec<Url> {
    list.split([';', ' ', '\t', '\r', '\n'])
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .flat_map(|entry| match entry.split_once('=') {
            Some((scheme, server)) if scheme.eq_ignore_ascii_case("socks") => socks_urls(server),
            Some((_, server)) => server_url("http", server, 80).into_iter().collect(),
            None => server_url("http", entry, 80).into_iter().collect(),
        })
        .collect()
}

/// What a proxy auto-configuration script is asked about. WinHTTP runs
/// scripts for the URLs of HTTP requests only, so a mail server is asked
/// about as `https://host:port/`.
pub(super) fn script_target(url: &Url) -> Url {
    if matches!(url.scheme(), "http" | "https") {
        return url.clone();
    }
    let host = match url.host() {
        Some(url::Host::Ipv6(address)) => format!("[{address}]"),
        Some(host) => host.to_string(),
        None => return url.clone(),
    };
    let port = url.port().unwrap_or(443);
    Url::parse(&format!("https://{host}:{port}/")).unwrap_or_else(|_| url.clone())
}

/// `socks5://` and `socks4a://` URLs for a SOCKS server.
pub(super) fn socks_urls(server: &str) -> Vec<Url> {
    ["socks5", "socks4a"]
        .into_iter()
        .filter_map(|scheme| server_url(scheme, server, 1080))
        .collect()
}

/// A proxy URL for `server`, written `host`, `host:port`, `[v6]:port` or
/// with a `scheme://` in front, which goes.
fn server_url(scheme: &str, server: &str, default_port: u16) -> Option<Url> {
    let server = server
        .split_once("://")
        .map_or(server, |(_, rest)| rest)
        .trim_end_matches('/');
    if server.is_empty() {
        return None;
    }
    let mut url = Url::parse(&format!("{scheme}://{server}")).ok()?;
    if url.port().is_none() {
        url.set_port(Some(default_port)).ok()?;
    }
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

    #[test]
    fn one_proxy_serves_the_web_but_not_mail() {
        let list = "proxy.example:3128";
        assert_eq!(
            named(&fixed(&url("https://example.com/"), list, "")),
            ["http://proxy.example:3128/"]
        );
        assert_eq!(
            named(&fixed(&url("http://example.com/"), list, "")),
            ["http://proxy.example:3128/"]
        );
        assert!(fixed(&url("imaps://imap.example.com:993"), list, "").is_empty());
    }

    #[test]
    fn proxies_per_protocol() {
        let list = "http=web.proxy:80;https=secure.proxy:3129;ftp=ftp.proxy:21;socks=socks.proxy";
        assert_eq!(
            named(&fixed(&url("http://example.com/"), list, "")),
            ["http://web.proxy/"]
        );
        assert_eq!(
            named(&fixed(&url("https://example.com/"), list, "")),
            ["http://secure.proxy:3129/"]
        );
        assert_eq!(
            named(&fixed(&url("imaps://imap.example.com:993"), list, "")),
            ["socks5://socks.proxy:1080", "socks4a://socks.proxy:1080"]
        );
    }

    #[test]
    fn socks_serves_the_web_without_an_http_proxy() {
        assert_eq!(
            named(&fixed(
                &url("https://example.com/"),
                "socks=socks.proxy:1080",
                ""
            )),
            ["socks5://socks.proxy:1080", "socks4a://socks.proxy:1080"]
        );
    }

    #[test]
    fn bypassed_servers_go_direct() {
        let list = "proxy.example:3128";
        let bypass = "*.internal.example;<local>;10.*";
        for target in [
            "https://wiki.internal.example/",
            "https://intranet/",
            "https://10.1.2.3/",
            "https://localhost/",
        ] {
            assert!(fixed(&url(target), list, bypass).is_empty(), "{target}");
        }
        assert!(!fixed(&url("https://example.com/"), list, bypass).is_empty());
    }

    #[test]
    fn script_results_are_http_proxies() {
        assert_eq!(
            named(&from_script("proxy1:8080; proxy2:8080")),
            ["http://proxy1:8080/", "http://proxy2:8080/"]
        );
        assert_eq!(
            named(&from_script(
                "https=secure.proxy:3129;socks=socks.proxy:1081"
            )),
            [
                "http://secure.proxy:3129/",
                "socks5://socks.proxy:1081",
                "socks4a://socks.proxy:1081"
            ]
        );
        assert!(from_script("").is_empty());
    }

    #[test]
    fn scripts_are_asked_about_mail_servers_as_https() {
        let target = |text: &str| script_target(&url(text)).to_string();
        assert_eq!(
            target("imaps://imap.example.com:993"),
            "https://imap.example.com:993/"
        );
        assert_eq!(
            target("smtp://[2001:db8::7]:587"),
            "https://[2001:db8::7]:587/"
        );
        assert_eq!(target("https://example.com/a?b"), "https://example.com/a?b");
    }

    #[test]
    fn accepts_a_scheme_and_ipv6_in_the_list() {
        assert_eq!(
            named(&fixed(
                &url("https://example.com/"),
                "https=http://[2001:db8::7]:3128",
                ""
            )),
            ["http://[2001:db8::7]:3128/"]
        );
    }
}
