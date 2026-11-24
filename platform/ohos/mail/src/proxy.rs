//! OpenHarmony's system HTTP proxy, read through Network Kit's native API.
//!
//! `libnet_connection.so` is a system library; the SDK ships a link-time stub.
//! The API returns the global proxy when one is set, otherwise the default
//! network's proxy. Mail goes through only SOCKS proxies in the shared proxy
//! layer, so this reader answers for HTTP(S) only; IMAP and SMTP stay direct
//! unless an account has its own SOCKS proxy.
//!
//! The native `NetConn_HttpProxy` does not carry proxy credentials and the
//! PAC script is not evaluated here. Authenticated or PAC proxies can be set
//! with the manual proxy fields in Settings.

use core::ffi::{CStr, c_char};
use std::net::IpAddr;

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

#[link(name = "net_connection")]
unsafe extern "C" {
    /// `OH_NetConn_GetDefaultHttpProxy`: 0 on success.
    fn OH_NetConn_GetDefaultHttpProxy(proxy: *mut NetConnHttpProxy) -> i32;
}

/// Install the system-proxy reader. Call before `flectar_mail::run`, which
/// starts the first connections. A changed system proxy is seen when the
/// shared cache's minute expires.
pub(super) fn install() {
    configure_system_proxy(Reader);
    match system_http_proxy() {
        Ok(Some((host, port, exclusions))) => slint_ohos_backend::ohos_info!(
            "mail: system HTTP proxy {host}:{port} ({} exclusions)",
            exclusions.len()
        ),
        Ok(None) => slint_ohos_backend::ohos_info!("mail: no system HTTP proxy"),
        Err(rc) => {
            slint_ohos_backend::ohos_error!("mail: system HTTP proxy read failed: {rc}")
        }
    }
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
        let Some((host, port, exclusions)) = system_http_proxy().ok().flatten() else {
            return Vec::new();
        };
        if url
            .host_str()
            .is_some_and(|host| excluded(host, &exclusions))
        {
            return Vec::new();
        }
        let host = if host.contains(':') && !host.starts_with('[') {
            format!("[{host}]")
        } else {
            host
        };
        let Ok(proxy) = Url::parse(&format!("http://{host}:{port}")) else {
            return Vec::new();
        };
        vec![proxy]
    }
}

/// Read the current proxy from Network Kit, or the call's error code.
fn system_http_proxy() -> Result<Option<(String, u16, Vec<String>)>, i32> {
    // SAFETY: the struct is zeroed before the call, and the API fills it.
    let mut proxy = Box::new(unsafe { core::mem::zeroed::<NetConnHttpProxy>() });
    // SAFETY: `proxy` points at writable storage of the C layout.
    let rc = unsafe { OH_NetConn_GetDefaultHttpProxy(&mut *proxy) };
    if rc != 0 {
        return Err(rc);
    }
    let Some(host) = c_string(&proxy.host) else {
        return Ok(None);
    };
    if host.is_empty() || proxy.port == 0 {
        return Ok(None);
    }
    let count = proxy.exclusion_list_size.clamp(0, MAX_EXCLUSIONS as i32) as usize;
    let mut exclusions = Vec::with_capacity(count);
    for index in 0..count {
        // SAFETY: `index` is within the fixed exclusion array.
        let Some(text) = c_string(&proxy.exclusion_list[index]) else {
            continue;
        };
        for entry in text.split(',') {
            let entry = entry.trim();
            if !entry.is_empty() {
                exclusions.push(entry.to_owned());
            }
        }
    }
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

/// Whether the system's exclusion list covers `host`.
///
/// The rules are Network Kit's: an entry matches the host itself or any
/// subdomain of it, names compare without case, protocol prefixes and ports
/// are ignored, IP entries match the whole address, and a lone `*` disables
/// the proxy for every host. A `*` mixed with other entries does not take
/// effect, as in the system's own matching.
fn excluded(host: &str, exclusions: &[String]) -> bool {
    let host = normalize_host(host);
    if host.is_empty() {
        return false;
    }
    let host_ip = host.parse::<IpAddr>().ok();
    let mut has_entry = false;
    let mut all_wildcard = true;
    let mut matched = false;
    for entry in exclusions {
        let entry = normalize_host(entry);
        if entry.is_empty() {
            continue;
        }
        has_entry = true;
        if entry == "*" {
            continue;
        }
        all_wildcard = false;
        let entry_ip = entry.parse::<IpAddr>().ok();
        if let Some(host_ip) = host_ip {
            // An IP host matches only an equal address; a domain entry never
            // covers it, and a partial address never matches.
            if entry_ip == Some(host_ip) {
                matched = true;
            }
            continue;
        }
        if entry_ip.is_some() {
            // An IP entry names one address, not a domain suffix.
            continue;
        }
        if host == entry || host.ends_with(&format!(".{entry}")) {
            matched = true;
        }
    }
    matched || (has_entry && all_wildcard)
}

/// A host or exclusion entry without case, a protocol prefix, a path, a port,
/// a bracketed IPv6 wrapper, a trailing dot, or surrounding space.
fn normalize_host(value: &str) -> String {
    let value = value.trim();
    let value = value.split_once("://").map_or(value, |(_, rest)| rest);
    let value = value.split(['/', '?', '#']).next().unwrap_or(value);
    let value = if let Some(rest) = value.strip_prefix('[') {
        rest.find(']').map_or(rest, |end| &rest[..end])
    } else {
        match value.rsplit_once(':') {
            Some((host, port))
                if !host.contains(':')
                    && !port.is_empty()
                    && port.chars().all(|c| c.is_ascii_digit()) =>
            {
                host
            }
            _ => value,
        }
    };
    value.trim_end_matches('.').to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::excluded;

    fn entries(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn matches_domains_and_their_subdomains() {
        let list = entries(&["example.com", "ample.com"]);
        assert!(excluded("example.com", &list));
        assert!(excluded("mail.example.com", &list));
        assert!(excluded("AMPLE.COM", &list));
        assert!(excluded("www.ample.com", &list));
        assert!(!excluded("notexample.com", &list));
        assert!(!excluded("example.com.org", &list));
    }

    #[test]
    fn a_lone_wildcard_bypasses_every_host() {
        let list = entries(&["*"]);
        assert!(excluded("anything.test", &list));
        // Mixed with names, the system's wildcard does not take effect.
        let list = entries(&["*", "example.com"]);
        assert!(excluded("example.com", &list));
        assert!(!excluded("anything.test", &list));
    }

    #[test]
    fn matches_ips_by_address_and_not_by_prefix() {
        let list = entries(&["10.0.0.1", "2001:0db8:0:0:0:0:0:1"]);
        assert!(excluded("10.0.0.1", &list));
        assert!(!excluded("10.0.0.2", &list));
        assert!(!excluded("10.0.0.10", &list));
        assert!(excluded("2001:db8::1", &list));
        let list = entries(&["192.168.1"]);
        assert!(!excluded("192.168.1.2", &list));
        let list = entries(&["192.168.1.2"]);
        assert!(!excluded("host.192.168.1.2", &list));
    }

    #[test]
    fn ignores_protocols_ports_paths_and_trailing_dots() {
        let list = entries(&[
            "https://[::1]:8080/x",
            "http://corp.test:443/path",
            "example.com.",
        ]);
        assert!(excluded("::1", &list));
        assert!(excluded("host.corp.test", &list));
        assert!(excluded("example.com", &list));
        assert!(!excluded("other.test", &list));
    }
}
