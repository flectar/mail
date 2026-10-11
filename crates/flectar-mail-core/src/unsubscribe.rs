//! List unsubscribe mechanics: RFC 8058 one-click HTTPS POST, with RFC 2369
//! mailto: and plain-browser fallbacks.
//!
//! One-click (RFC 8058) requires the sender to publish BOTH
//! `List-Unsubscribe: <https://…>` and
//! `List-Unsubscribe-Post: List-Unsubscribe=One-Click`; the receiver then
//! POSTs the literal body `List-Unsubscribe=One-Click` to the HTTPS URI.
//! The endpoint must complete the unsubscribe without cookies, logins or
//! redirects, so the client here carries no cookie store and treats any
//! non-2xx (including 3xx) as failure.

use crate::error::{CoreError, Result};
use crate::mime::{is_one_click_post, unsubscribe_uris};
use std::{
    net::{IpAddr, SocketAddr},
    time::Duration,
};

const MAX_UNSUBSCRIBE_URL_BYTES: usize = 16 * 1024;

/// Which mechanism the message's headers support, in preference order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnsubscribePlan {
    /// HTTPS URI with a one-click List-Unsubscribe-Post marker.
    OneClick { url: String },
    /// mailto: fallback - send an unsubscribe request message.
    Mailto {
        to: String,
        subject: String,
        body: String,
    },
    /// Only a plain web URL: the user has to finish in the browser.
    Browser { url: String },
}

/// Decide how to unsubscribe from the raw header values. None when the header
/// contains no usable URI.
pub fn plan(
    list_unsubscribe: &str,
    list_unsubscribe_post: Option<&str>,
) -> Option<UnsubscribePlan> {
    let mut uris = unsubscribe_uris(list_unsubscribe);
    let web = uris
        .clone()
        .find(|u| has_scheme_prefix(u, "https://") || has_scheme_prefix(u, "http://"));
    let one_click = list_unsubscribe_post.is_some_and(is_one_click_post);

    // RFC 8058 is https-only; a one-click marker on an http:// URI is ignored.
    if one_click && let Some(url) = web.filter(|u| has_scheme_prefix(u, "https://")) {
        return Some(UnsubscribePlan::OneClick {
            url: url.to_string(),
        });
    }

    if let Some(m) = uris.find(|u| has_scheme_prefix(u, "mailto:"))
        && let Some(p) = parse_mailto(m)
    {
        return Some(p);
    }

    web.map(|url| UnsubscribePlan::Browser {
        url: url.to_owned(),
    })
}

fn has_scheme_prefix(uri: &str, prefix: &str) -> bool {
    uri.as_bytes()
        .get(..prefix.len())
        .is_some_and(|bytes| bytes.eq_ignore_ascii_case(prefix.as_bytes()))
}

/// Parse `mailto:addr?subject=…&body=…` into a Mailto plan. Subject defaults
/// to "unsubscribe" (the RFC 2369 convention); body to the subject text, so
/// list processors that only read the body still trigger.
fn parse_mailto(uri: &str) -> Option<UnsubscribePlan> {
    let rest = &uri[uri.find(':')? + 1..];
    let (addr, query) = match rest.split_once('?') {
        Some((a, q)) => (a, Some(q)),
        None => (rest, None),
    };
    let to = percent_decode(addr.trim());
    if to.is_empty() || !to.contains('@') {
        return None;
    }
    let mut subject = None;
    let mut body = None;
    for pair in query.unwrap_or("").split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        if k.eq_ignore_ascii_case("subject") {
            subject = Some(v);
        } else if k.eq_ignore_ascii_case("body") {
            body = Some(v);
        }
    }
    let subject = subject
        .map(percent_decode)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unsubscribe".into());
    let body = body
        .map(percent_decode)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| subject.clone());
    Some(UnsubscribePlan::Mailto { to, subject, body })
}

/// Minimal percent-decoding ('+' as space, %XX bytes, lossy UTF-8) - enough
/// for mailto query values without pulling in a URL crate.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 3 <= bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                match (hi, lo) {
                    (Some(hi), Some(lo)) => {
                        out.push((hi * 16 + lo) as u8);
                        i += 3;
                    }
                    _ => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    match String::from_utf8(out) {
        Ok(decoded) => decoded,
        Err(error) => String::from_utf8_lossy(error.as_bytes()).into_owned(),
    }
}

/// Perform the RFC 8058 one-click POST. Success is a 2xx only: no redirects
/// are followed (the endpoint must not require them) and no cookies are sent.
pub async fn post_one_click(url: &str) -> Result<()> {
    let url = validated_public_https_url(url)?;
    let (addresses, resolve_host): (Vec<SocketAddr>, Option<String>) = match url.host() {
        Some(url::Host::Domain(host)) => (
            tokio::net::lookup_host((host, 443))
                .await
                .map_err(|error| CoreError::Network(format!("unsubscribe DNS lookup: {error}")))?
                .collect(),
            Some(host.to_owned()),
        ),
        Some(url::Host::Ipv4(address)) => (vec![SocketAddr::new(address.into(), 443)], None),
        Some(url::Host::Ipv6(address)) => (vec![SocketAddr::new(address.into(), 443)], None),
        None => return Err(CoreError::Network("unsubscribe URL has no host".into())),
    };
    if addresses.is_empty() || addresses.iter().any(|address| !is_public_ip(address.ip())) {
        return Err(CoreError::Network(
            "unsubscribe endpoint resolved to a non-public address".into(),
        ));
    }
    let mut client = crate::http_tls::client_builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(8))
        .timeout(Duration::from_secs(20))
        .user_agent(concat!("flectar-mail/", env!("CARGO_PKG_VERSION")));
    if let Some(host) = resolve_host {
        client = client.resolve(&host, addresses[0]);
    }
    let client = client
        .build()
        .map_err(|e| CoreError::Network(e.to_string()))?;
    let resp = client
        .post(url)
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body("List-Unsubscribe=One-Click")
        .send()
        .await
        .map_err(|e| CoreError::Network(e.to_string()))?;
    let status = resp.status();
    if status.is_success() {
        Ok(())
    } else {
        Err(CoreError::Network(format!(
            "one-click unsubscribe endpoint answered {status}"
        )))
    }
}

fn validated_public_https_url(value: &str) -> Result<url::Url> {
    if value.len() > MAX_UNSUBSCRIBE_URL_BYTES {
        return Err(CoreError::Network("unsubscribe URL is too long".into()));
    }
    let url = url::Url::parse(value)
        .map_err(|error| CoreError::Network(format!("invalid unsubscribe URL: {error}")))?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(CoreError::Network(
            "one-click unsubscribe requires a credential-free HTTPS URL on port 443".into(),
        ));
    }
    let direct_address = match url.host() {
        Some(url::Host::Ipv4(address)) => Some(address.into()),
        Some(url::Host::Ipv6(address)) => Some(address.into()),
        _ => None,
    };
    if direct_address.is_some_and(|address| !is_public_ip(address)) {
        return Err(CoreError::Network(
            "unsubscribe endpoint uses a non-public address".into(),
        ));
    }
    Ok(url)
}

fn is_public_ip(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => {
            let octets = address.octets();
            !(address.is_private()
                || address.is_loopback()
                || address.is_link_local()
                || address.is_multicast()
                || address.is_unspecified()
                || address.is_broadcast()
                || address.is_documentation()
                || octets[0] == 0
                || (octets[0] == 100 && (64..=127).contains(&octets[1]))
                || (octets[0] == 192 && octets[1] == 0 && octets[2] == 0)
                || (octets[0] == 198 && matches!(octets[1], 18 | 19))
                || octets[0] >= 240)
        }
        IpAddr::V6(address) => {
            !(address.is_loopback()
                || address.is_unique_local()
                || address.is_unicast_link_local()
                || address.is_multicast()
                || address.is_unspecified()
                || address.segments()[..2] == [0x2001, 0x0db8]
                || address.segments()[0] & 0xffc0 == 0xfec0)
                && address
                    .to_ipv4_mapped()
                    .is_none_or(|mapped| is_public_ip(mapped.into()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_click_needs_post_header_and_https() {
        let p = plan(
            "<https://x.example/u?t=1>, <mailto:u@x.example>",
            Some("List-Unsubscribe=One-Click"),
        );
        assert_eq!(
            p,
            Some(UnsubscribePlan::OneClick {
                url: "https://x.example/u?t=1".into()
            })
        );

        // Same header without the POST marker: mailto wins over browser.
        let p = plan("<https://x.example/u?t=1>, <mailto:u@x.example>", None);
        assert!(matches!(p, Some(UnsubscribePlan::Mailto { .. })));

        // One-click marker on a plain http URL is ignored.
        let p = plan("<http://x.example/u>", Some("List-Unsubscribe=One-Click"));
        assert_eq!(
            p,
            Some(UnsubscribePlan::Browser {
                url: "http://x.example/u".into()
            })
        );
    }

    #[test]
    fn one_click_rejects_local_and_credentialed_endpoints() {
        assert!(validated_public_https_url("https://example.com/u").is_ok());
        for value in [
            "https://127.0.0.1/u",
            "https://[::1]/u",
            "https://169.254.169.254/latest/meta-data",
            "https://user:secret@example.com/u",
            "https://example.com:444/u",
            "https://example.com/u#fragment",
            "http://example.com/u",
        ] {
            assert!(validated_public_https_url(value).is_err(), "{value}");
        }
    }

    #[test]
    fn mailto_parses_subject_and_body() {
        let p = plan(
            "<mailto:leave@list.example?subject=unsub%20me&body=please+go>",
            None,
        );
        assert_eq!(
            p,
            Some(UnsubscribePlan::Mailto {
                to: "leave@list.example".into(),
                subject: "unsub me".into(),
                body: "please go".into(),
            })
        );
    }

    #[test]
    fn mailto_defaults() {
        let p = plan("<mailto:leave@list.example>", None);
        assert_eq!(
            p,
            Some(UnsubscribePlan::Mailto {
                to: "leave@list.example".into(),
                subject: "unsubscribe".into(),
                body: "unsubscribe".into(),
            })
        );
    }

    #[test]
    fn browser_fallback_and_empty() {
        assert_eq!(
            plan("<https://x.example/u>", None),
            Some(UnsubscribePlan::Browser {
                url: "https://x.example/u".into()
            })
        );
        assert_eq!(plan("nothing useful", None), None);
        // Bare (unbracketed) value from a sloppy sender still parses.
        assert!(matches!(
            plan("https://x.example/u", Some("List-Unsubscribe=One-Click")),
            Some(UnsubscribePlan::OneClick { .. })
        ));
    }

    #[test]
    fn percent_decode_edge_cases() {
        assert_eq!(percent_decode("a%2Bb+c"), "a+b c");
        assert_eq!(percent_decode("bad%2"), "bad%2");
        assert_eq!(percent_decode("%zz"), "%zz");
    }
}
