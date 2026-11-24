//! OpenHarmony's system HTTP proxy, read through Network Kit's native API.
//!
//! `libnet_connection.so` is a system library; the SDK ships a link-time stub.
//! The API returns the global proxy when one is set, otherwise the default
//! network's proxy. In automatic (PAC) mode Network Kit runs the script behind
//! a local proxy and returns that one, so the script applies here too; a PAC
//! address published on its own through `OH_NetConn_SetPacUrl` is not run.
//! Mail goes through only SOCKS proxies in the shared proxy layer, so this
//! reader answers for HTTP(S) only; IMAP and SMTP stay direct unless an
//! account has its own SOCKS proxy.
//!
//! The native `NetConn_HttpProxy` does not carry proxy credentials, so a
//! system proxy that asks for a password needs the manual proxy fields in
//! Settings.

use core::ffi::{CStr, c_char};
use std::net::Ipv6Addr;

use flectar_mail::flectar_mail_core::net::proxy::Bypass;
use flectar_mail::platform::{SystemProxies, configure_system_proxy};
use url::Url;

const MAX_STR: usize = 256;
const MAX_EXCLUSIONS: usize = 256;

/// The proxy configuration returned by Network Kit.
#[repr(C)]
struct NetConnHttpProxy {
    host: [c_char; MAX_STR],
    exclusion_list: [[c_char; MAX_STR]; MAX_EXCLUSIONS],
    exclusion_list_size: i32,
    port: u16,
}

// The C layout from `net_connection_type.h`: host, the exclusion array, its
// size, and the port, padded to the alignment of `i32`.
const _: () = assert!(core::mem::size_of::<NetConnHttpProxy>() == 65_800);
const _: () = assert!(core::mem::align_of::<NetConnHttpProxy>() == 4);

#[link(name = "net_connection", kind = "dylib")]
unsafe extern "C" {
    /// `OH_NetConn_GetDefaultHttpProxy`: 0 on success.
    fn OH_NetConn_GetDefaultHttpProxy(proxy: *mut NetConnHttpProxy) -> i32;
}

/// Install the system-proxy reader. Call before `flectar_mail::run`, which
/// starts the first connections. A changed system proxy is seen when the
/// shared cache's minute expires.
pub(super) fn install() {
    configure_system_proxy(Reader);
}

struct Reader;

impl SystemProxies for Reader {
    fn lookup(&self, url: &Url) -> Vec<Url> {
        // The system proxy is an HTTP proxy. Mail routes in the shared layer
        // filter HTTP proxies out; answering only for web URLs keeps the
        // intent explicit and avoids a needless lookup for IMAP/SMTP.
        if !matches!(url.scheme(), "http" | "https") {
            return Vec::new();
        }
        let (host, port, exclusions) = match system_http_proxy() {
            Ok(Some(proxy)) => proxy,
            Ok(None) => return Vec::new(),
            Err(code) => {
                tracing::warn!(code, "could not ask HarmonyOS for the proxy");
                return Vec::new();
            }
        };
        if url
            .host_str()
            .is_some_and(|host| bypass(&exclusions).skips(host))
        {
            return Vec::new();
        }
        proxy_url(&host, port).into_iter().collect()
    }
}

/// Read the current proxy from Network Kit, or the call's error code.
fn system_http_proxy() -> Result<Option<(String, u16, Vec<String>)>, i32> {
    // SAFETY: zero is a valid value for every field of the C struct, which
    // goes straight to the heap at its 64 KiB.
    let mut proxy = unsafe { Box::<NetConnHttpProxy>::new_zeroed().assume_init() };
    // SAFETY: `proxy` points at writable storage of the C layout.
    let rc = unsafe { OH_NetConn_GetDefaultHttpProxy(&mut *proxy) };
    if rc != 0 {
        return Err(rc);
    }
    let Some(host) = c_string(&proxy.host) else {
        return Ok(None);
    };
    if host.trim().is_empty() {
        return Ok(None);
    }
    let count = proxy.exclusion_list_size.clamp(0, MAX_EXCLUSIONS as i32) as usize;
    let exclusions = proxy.exclusion_list[..count]
        .iter()
        .filter_map(c_string)
        .collect();
    Ok(Some((host, proxy.port, exclusions)))
}

/// A fixed C string, read only within its array. `None` when the system filled
/// the whole array without a terminating NUL.
fn c_string(value: &[c_char; MAX_STR]) -> Option<String> {
    // SAFETY: the array is live and exactly `MAX_STR` bytes long.
    let bytes = unsafe { std::slice::from_raw_parts(value.as_ptr().cast::<u8>(), MAX_STR) };
    CStr::from_bytes_until_nul(bytes)
        .ok()
        .map(|text| text.to_string_lossy().into_owned())
}

/// The proxy as a URL. Network Kit's own HTTP client hands the host to curl,
/// which reads `https://` in front of it as a proxy reached over TLS, and a
/// port in it before the separate one. A bare IPv6 address goes in brackets.
fn proxy_url(host: &str, port: u16) -> Option<Url> {
    let host = host.trim();
    let text = if host.contains("://") {
        host.to_owned()
    } else if host.parse::<Ipv6Addr>().is_ok() {
        format!("http://[{host}]")
    } else {
        format!("http://{host}")
    };
    let mut proxy = Url::parse(&text).ok()?;
    if !matches!(proxy.scheme(), "http" | "https") || proxy.host_str().is_none() {
        return None;
    }
    if proxy.port().is_none() {
        // Without a port anywhere, there is no proxy to reach.
        if port == 0 {
            return None;
        }
        proxy.set_port(Some(port)).ok()?;
    }
    Some(proxy)
}

/// The servers that skip the proxy. Network Kit's own HTTP client (netstack's
/// `IsHostNameExcluded`) takes each comma-separated entry as a whole server
/// name in which `*` stands for any characters, so `example.com` is that
/// server alone, `*.example.com` its subdomains and `192.168.*` a network, and
/// an entry of `*` skips the proxy for every server. Windows, macOS and
/// Android write their lists the same way.
fn bypass(exclusions: &[String]) -> Bypass {
    Bypass::parse_wildcards(&exclusions.join(","))
}

#[cfg(test)]
mod tests {
    use super::{bypass, proxy_url};

    fn entries(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn exclusions_follow_the_system_rules() {
        let list = bypass(&entries(&["example.com", "*.corp.example, 192.168.*"]));
        assert!(list.skips("example.com"));
        assert!(!list.skips("www.example.com"));
        assert!(list.skips("mail.corp.example"));
        assert!(!list.skips("corp.example"));
        assert!(list.skips("192.168.1.20"));
        assert!(!list.skips("10.0.0.1"));
        assert!(bypass(&entries(&["example.com", "*"])).skips("anything.test"));
        assert!(!bypass(&[]).skips("example.com"));
    }

    #[test]
    fn the_host_may_carry_a_scheme_and_a_port() {
        for (host, port, proxy) in [
            ("proxy.corp", 3128, Some("http://proxy.corp:3128/")),
            ("https://proxy.corp", 8443, Some("https://proxy.corp:8443/")),
            ("proxy.corp:8080", 3128, Some("http://proxy.corp:8080/")),
            ("2001:db8::1", 3128, Some("http://[2001:db8::1]:3128/")),
            ("[2001:db8::1]", 3128, Some("http://[2001:db8::1]:3128/")),
            ("proxy.corp", 0, None),
            ("socks5://proxy.corp", 1080, None),
        ] {
            let url = proxy_url(host, port).map(String::from);
            assert_eq!(url.as_deref(), proxy, "{host} port {port}");
        }
    }
}
