//! Servers reached without the proxy.
//!
//! Loopback always goes direct, as in Firefox and Chromium: a proxy can't
//! reach this computer's own services, such as a mail bridge on 127.0.0.1.
//! A bypass list adds more, in the forms `NO_PROXY` (curl) and the system
//! settings use: `example.com` or `.example.com` for a domain and its
//! subdomains, `*.example.com` for the subdomains, an address, a network
//! such as `10.0.0.0/8`, `<local>` for names without a dot, and `*` for
//! everything. Entries are separated by commas, semicolons or spaces.

use std::net::IpAddr;

/// A parsed bypass list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Bypass {
    rules: Vec<Rule>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Rule {
    Everything,
    /// Names without a dot, as Windows' `<local>`.
    SimpleNames,
    /// The domain itself and its subdomains.
    Domain(String),
    /// Only the subdomains.
    Subdomains(String),
    /// Only this name, as system settings read a name in their lists.
    Host(String),
    /// Names or addresses matching a pattern where `*` stands for any
    /// characters.
    Pattern(String),
    Network {
        address: IpAddr,
        prefix: u8,
    },
}

impl Bypass {
    pub fn parse(list: &str) -> Self {
        let rules = entries(list).filter_map(Rule::parse).collect();
        Self { rules }
    }

    /// A list as the system settings of Windows, macOS and Android write it:
    /// a name names only that server, and `*` stands for any characters, as
    /// in `*.example.com` or `intranet*`. Windows' `<-loopback>`, which
    /// takes back the loopback bypass, is ignored: loopback stays direct.
    pub fn parse_wildcards(list: &str) -> Self {
        let rules = entries(list)
            .filter(|entry| !entry.eq_ignore_ascii_case("<-loopback>"))
            .filter_map(|entry| {
                let rest = entry.strip_prefix("*.").unwrap_or(entry);
                if entry != "*"
                    && rest.contains('*')
                    && ipv4_wildcard(entry).is_none()
                    && !entry.contains(['/', ':', '<'])
                {
                    return Some(Rule::Pattern(entry.to_ascii_lowercase()));
                }
                match Rule::parse(entry)? {
                    Rule::Domain(name) if !entry.starts_with('.') => Some(Rule::Host(name)),
                    rule => Some(rule),
                }
            })
            .collect();
        Self { rules }
    }

    /// The entries of `list` that name no server, so they'd be skipped.
    pub fn unreadable(list: &str) -> Vec<String> {
        entries(list)
            .filter(|entry| Rule::parse(entry).is_none())
            .map(str::to_owned)
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Whether `host` skips the proxy: a loopback name or address, or one
    /// the list names.
    pub fn skips(&self, host: &str) -> bool {
        let host = host
            .strip_prefix('[')
            .and_then(|host| host.strip_suffix(']'))
            .unwrap_or(host)
            .trim_end_matches('.');
        let address = host.parse::<IpAddr>().ok();
        if is_loopback(host, address) {
            return true;
        }
        self.rules.iter().any(|rule| rule.matches(host, address))
    }
}

fn entries(list: &str) -> impl Iterator<Item = &str> {
    list.split([',', ';', ' ', '\t', '\n', '\r'])
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
}

/// `localhost`, its subdomains (RFC 6761 section 6.3), and loopback
/// addresses, including IPv4-mapped ones.
fn is_loopback(host: &str, address: Option<IpAddr>) -> bool {
    match address {
        Some(IpAddr::V4(address)) => address.is_loopback(),
        Some(IpAddr::V6(address)) => {
            address.is_loopback() || address.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())
        }
        None => {
            host.eq_ignore_ascii_case("localhost")
                || host
                    .len()
                    .checked_sub(".localhost".len())
                    .and_then(|start| host.get(start..))
                    .is_some_and(|tail| tail.eq_ignore_ascii_case(".localhost"))
        }
    }
}

impl Rule {
    fn parse(entry: &str) -> Option<Self> {
        if entry == "*" {
            return Some(Self::Everything);
        }
        if entry.eq_ignore_ascii_case("<local>") {
            return Some(Self::SimpleNames);
        }
        let bare = entry
            .strip_prefix('[')
            .and_then(|entry| entry.strip_suffix(']'))
            .unwrap_or(entry);
        if let Some((address, prefix)) = bare.split_once('/') {
            let address = address.trim_start_matches('[').trim_end_matches(']');
            // macOS writes networks short, as `169.254/16`.
            let address = address
                .parse::<IpAddr>()
                .ok()
                .or_else(|| short_ipv4(address))?;
            let prefix: u8 = prefix.parse().ok()?;
            let bits = if address.is_ipv4() { 32 } else { 128 };
            return (prefix <= bits).then_some(Self::Network { address, prefix });
        }
        if let Ok(address) = bare.parse::<IpAddr>() {
            let prefix = if address.is_ipv4() { 32 } else { 128 };
            return Some(Self::Network { address, prefix });
        }
        if let Some(network) = ipv4_wildcard(bare) {
            return Some(network);
        }
        let name = entry.trim_end_matches('.').to_ascii_lowercase();
        if let Some(domain) = name.strip_prefix("*.") {
            return (!domain.is_empty()).then(|| Self::Subdomains(domain.to_owned()));
        }
        // A leading dot names the domain and its subdomains, as in curl.
        let domain = name.trim_start_matches('.');
        (!domain.is_empty() && !domain.contains('*')).then(|| Self::Domain(domain.to_owned()))
    }

    fn matches(&self, host: &str, address: Option<IpAddr>) -> bool {
        match self {
            Self::Everything => true,
            Self::SimpleNames => address.is_none() && !host.contains('.'),
            Self::Domain(domain) => {
                address.is_none()
                    && (host.eq_ignore_ascii_case(domain) || is_subdomain(host, domain))
            }
            Self::Subdomains(domain) => address.is_none() && is_subdomain(host, domain),
            Self::Host(name) => address.is_none() && host.eq_ignore_ascii_case(name),
            Self::Pattern(pattern) => wildcard_match(pattern, host),
            Self::Network {
                address: network,
                prefix,
            } => address.is_some_and(|address| in_network(address, *network, *prefix)),
        }
    }
}

/// An IPv4 address with its trailing zeros left out, as in `169.254`.
fn short_ipv4(text: &str) -> Option<IpAddr> {
    let mut octets = [0u8; 4];
    let mut count = 0;
    for part in text.split('.') {
        *octets.get_mut(count)? = part.parse().ok()?;
        count += 1;
    }
    (count > 0).then(|| IpAddr::from(octets))
}

/// Windows writes networks as `192.168.*`.
fn ipv4_wildcard(entry: &str) -> Option<Rule> {
    let leading = entry.strip_suffix(".*")?;
    let mut octets = [0u8; 4];
    let mut count = 0;
    for part in leading.split('.') {
        *octets.get_mut(count)? = part.parse().ok()?;
        count += 1;
    }
    (count < 4).then(|| Rule::Network {
        address: IpAddr::from(octets),
        prefix: 8 * count as u8,
    })
}

/// Whether `text` matches `pattern`, where `*` stands for any characters,
/// ignoring ASCII case.
fn wildcard_match(pattern: &str, text: &str) -> bool {
    let (pattern, text) = (pattern.as_bytes(), text.as_bytes());
    let (mut at_pattern, mut at_text) = (0, 0);
    // The last `*` seen, and where in the text its match ends for now.
    let mut star = None;
    while at_text < text.len() {
        match pattern.get(at_pattern) {
            Some(b'*') => {
                star = Some((at_pattern, at_text));
                at_pattern += 1;
            }
            Some(byte) if byte.eq_ignore_ascii_case(&text[at_text]) => {
                at_pattern += 1;
                at_text += 1;
            }
            _ => {
                // Let the last `*` take one more character, and go on.
                let Some((star_at, matched_to)) = star else {
                    return false;
                };
                star = Some((star_at, matched_to + 1));
                at_pattern = star_at + 1;
                at_text = matched_to + 1;
            }
        }
    }
    pattern[at_pattern..].iter().all(|&byte| byte == b'*')
}

/// Whether `host` ends with `.domain`.
fn is_subdomain(host: &str, domain: &str) -> bool {
    host.len() > domain.len() + 1
        && host.as_bytes()[host.len() - domain.len() - 1] == b'.'
        && host[host.len() - domain.len()..].eq_ignore_ascii_case(domain)
}

fn in_network(address: IpAddr, network: IpAddr, prefix: u8) -> bool {
    let (address, network, bits) = match (address, network) {
        (IpAddr::V4(address), IpAddr::V4(network)) => (
            u128::from(address.to_bits()),
            u128::from(network.to_bits()),
            32,
        ),
        (IpAddr::V6(address), IpAddr::V6(network)) => (address.to_bits(), network.to_bits(), 128),
        (IpAddr::V6(address), IpAddr::V4(network)) => match address.to_ipv4_mapped() {
            Some(address) => (
                u128::from(address.to_bits()),
                u128::from(network.to_bits()),
                32,
            ),
            None => return false,
        },
        (IpAddr::V4(_), IpAddr::V6(_)) => return false,
    };
    let prefix = u32::from(prefix);
    if prefix == 0 {
        return true;
    }
    let shift = bits - prefix;
    address >> shift == network >> shift
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_always_skips_the_proxy() {
        let none = Bypass::default();
        for host in [
            "localhost",
            "LocalHost.",
            "imap.localhost",
            "127.0.0.1",
            "127.8.9.10",
            "::1",
            "[::1]",
            "::ffff:127.0.0.1",
        ] {
            assert!(none.skips(host), "{host}");
        }
        for host in [
            "localhost.example",
            "notlocalhost",
            "10.0.0.1",
            "::2",
            "example.com",
        ] {
            assert!(!none.skips(host), "{host}");
        }
    }

    #[test]
    fn domains_cover_their_subdomains() {
        let list = Bypass::parse("example.com, .corp.example;internal.test");
        for host in [
            "example.com",
            "imap.example.com",
            "Mail.Example.COM.",
            "corp.example",
            "a.b.corp.example",
            "internal.test",
        ] {
            assert!(list.skips(host), "{host}");
        }
        for host in ["notexample.com", "example.com.evil", "example.org", "test"] {
            assert!(!list.skips(host), "{host}");
        }
    }

    #[test]
    fn a_star_prefix_covers_only_subdomains() {
        let list = Bypass::parse("*.example.com");
        assert!(list.skips("imap.example.com"));
        assert!(!list.skips("example.com"));
    }

    #[test]
    fn addresses_and_networks() {
        let list = Bypass::parse("192.168.0.0/16 10.1.2.3 2001:db8::/32 [fd00::1]");
        for host in [
            "192.168.4.5",
            "10.1.2.3",
            "2001:db8:1::5",
            "[2001:db8::7]",
            "fd00::1",
            "::ffff:192.168.9.9",
        ] {
            assert!(list.skips(host), "{host}");
        }
        for host in ["192.169.0.1", "10.1.2.4", "2001:db9::1", "fd00::2"] {
            assert!(!list.skips(host), "{host}");
        }
    }

    #[test]
    fn local_names_and_everything() {
        let local = Bypass::parse("<local>");
        assert!(local.skips("intranet"));
        assert!(!local.skips("intranet.example"));
        assert!(!local.skips("10.0.0.1"));
        let all = Bypass::parse("*");
        assert!(all.skips("example.com"));
        assert!(all.skips("10.0.0.1"));
    }

    #[test]
    fn system_lists_name_hosts_exactly() {
        let list =
            Bypass::parse_wildcards("intranet.example;*.corp.example;<local>;<-loopback>;10.*");
        assert!(list.skips("intranet.example"));
        assert!(!list.skips("mail.intranet.example"));
        assert!(list.skips("mail.corp.example"));
        assert!(!list.skips("corp.example"));
        assert!(list.skips("printer"));
        assert!(list.skips("10.1.2.3"));
        assert!(list.skips("localhost"));
    }

    #[test]
    fn system_lists_take_wildcards_anywhere() {
        let list = Bypass::parse_wildcards("*example.com;wiki*;192.168.*.1;*.lab.*");
        assert!(list.skips("example.com"));
        assert!(list.skips("myexample.com"));
        assert!(list.skips("mail.EXAMPLE.com"));
        assert!(!list.skips("example.org"));
        assert!(list.skips("wiki"));
        assert!(list.skips("wiki.intranet"));
        assert!(!list.skips("mywiki"));
        assert!(list.skips("192.168.7.1"));
        assert!(!list.skips("192.168.7.2"));
        assert!(list.skips("printer.lab.example"));
        assert!(!list.skips("lab.example"));
    }

    #[test]
    fn wildcards_match_like_globs() {
        for (pattern, text, expected) in [
            ("*", "", true),
            ("a*", "a", true),
            ("a*c", "abbbc", true),
            ("a*c", "abbbd", false),
            ("*a*a*", "banana", true),
            ("*a*a*a*a", "banana", false),
            ("a**b", "ab", true),
            ("", "", true),
            ("", "a", false),
        ] {
            assert_eq!(wildcard_match(pattern, text), expected, "{pattern} {text}");
        }
    }

    #[test]
    fn reads_short_macos_networks() {
        let list = Bypass::parse("169.254/16, 192.168.1/24, *.local");
        assert!(list.skips("169.254.10.20"));
        assert!(list.skips("192.168.1.9"));
        assert!(!list.skips("192.168.2.9"));
        assert!(list.skips("printer.local"));
    }

    #[test]
    fn reads_windows_network_wildcards() {
        let list = Bypass::parse("192.168.*;10.*");
        assert!(list.skips("192.168.7.1"));
        assert!(list.skips("10.200.0.1"));
        assert!(!list.skips("192.169.0.1"));
        assert!(!list.skips("11.0.0.1"));
    }

    #[test]
    fn ignores_entries_it_cannot_read() {
        let list = "10.0.0.0/40, *, ., *., 1.2.3.4.*, 300.*, a*b.example";
        assert_eq!(Bypass::parse(list).rules, [Rule::Everything]);
        assert_eq!(
            Bypass::unreadable(list),
            [
                "10.0.0.0/40",
                ".",
                "*.",
                "1.2.3.4.*",
                "300.*",
                "a*b.example"
            ]
        );
        assert!(Bypass::parse(" ,; ").is_empty());
        assert!(Bypass::unreadable("localhost, .example.com; 10.0.0.0/8").is_empty());
    }
}
