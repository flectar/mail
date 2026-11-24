//! The mail server (MX) a domain names, asked of the device's own resolver,
//! as mail apps do when a provider publishes no configuration of its own.
//! Each platform's resolver API keeps the device's DNS settings: private or
//! encrypted DNS, VPN and per-domain rules. Answers aren't authenticated, so
//! whatever is derived from them is only offered for the person to confirm.

use std::time::Duration;

#[cfg(android_resolver)]
mod android;
#[cfg(apple_resolver)]
mod apple;
#[cfg(res_query)]
mod res_query;
#[cfg(test)]
mod tests;
#[cfg(windows)]
mod windows;

const TIMEOUT: Duration = Duration::from_secs(5);
const TYPE_MX: u16 = 15;
const CLASS_IN: u16 = 1;

/// The host of `domain`'s most preferred mail server, in lowercase. `None`
/// when the lookup fails, times out, or the domain takes no mail.
pub(crate) async fn mail_host(domain: &str) -> Option<String> {
    let domain = domain.trim().trim_end_matches('.').to_ascii_lowercase();
    if !is_host_name(&domain) {
        return None;
    }
    let lookup = tokio::task::spawn_blocking(move || exchanges(&domain));
    let exchanges = tokio::time::timeout(TIMEOUT, lookup).await.ok()?.ok()??;
    preferred(exchanges)
}

// Each platform's resolver: the domain's mail servers with their
// preferences. Blocks.
#[cfg(android_resolver)]
use android::exchanges;
#[cfg(apple_resolver)]
use apple::exchanges;
#[cfg(res_query)]
use res_query::exchanges;
#[cfg(windows)]
use windows::exchanges;

#[cfg(not(any(res_query, android_resolver, apple_resolver, windows)))]
fn exchanges(_domain: &str) -> Option<Vec<(u16, String)>> {
    None
}

/// The most preferred server. A single "." means the domain takes no mail
/// (RFC 7505).
fn preferred(mut exchanges: Vec<(u16, String)>) -> Option<String> {
    exchanges.sort();
    let (_, host) = exchanges.into_iter().next()?;
    is_host_name(&host).then_some(host)
}

/// A DNS host name in lowercase ASCII, with at least two labels.
pub(crate) fn is_host_name(host: &str) -> bool {
    host.len() <= 253
        && host.contains('.')
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        })
}

/// The MX records in a DNS response message (RFC 1035 section 4.1).
#[cfg_attr(not(any(res_query, android_resolver, test)), allow(dead_code))]
fn exchanges_in_message(message: &[u8]) -> Option<Vec<(u16, String)>> {
    let header = message.get(..12)?;
    // A response, with no error.
    if header[2] & 0x80 == 0 || header[3] & 0x0f != 0 {
        return None;
    }
    let questions = u16::from_be_bytes([header[4], header[5]]);
    let answers = u16::from_be_bytes([header[6], header[7]]);
    let mut at = 12;
    for _ in 0..questions {
        at = read_name(message, at)?.1 + 4;
    }
    let mut exchanges = Vec::new();
    for _ in 0..answers {
        let (_, after_name) = read_name(message, at)?;
        let fixed = message.get(after_name..after_name + 10)?;
        let kind = u16::from_be_bytes([fixed[0], fixed[1]]);
        let class = u16::from_be_bytes([fixed[2], fixed[3]]);
        let length = usize::from(u16::from_be_bytes([fixed[8], fixed[9]]));
        let data = after_name + 10;
        message.get(data..data + length)?;
        if kind == TYPE_MX && class == CLASS_IN && length >= 3 {
            let preference = u16::from_be_bytes([message[data], message[data + 1]]);
            let (host, _) = read_name(message, data + 2)?;
            exchanges.push((preference, host));
        }
        at = data + length;
    }
    Some(exchanges)
}

/// An MX record's data on its own, as Apple's resolver hands it over: the
/// preference, then the host with no compression.
#[cfg_attr(not(any(apple_resolver, test)), allow(dead_code))]
fn exchange_in_record(data: &[u8]) -> Option<(u16, String)> {
    let preference = u16::from_be_bytes([*data.first()?, *data.get(1)?]);
    let (host, end) = read_name(data, 2)?;
    (end == data.len()).then_some((preference, host))
}

/// The domain name at `at` (RFC 1035 section 4.1.4), in lowercase, and where
/// the name ends in the message. The root name is "".
fn read_name(message: &[u8], mut at: usize) -> Option<(String, usize)> {
    let mut labels: Vec<String> = Vec::new();
    let mut end = None;
    let mut length = 0;
    for _ in 0..128 {
        let byte = *message.get(at)?;
        match byte & 0xc0 {
            0x00 if byte == 0 => {
                let name = labels.join(".");
                return Some((name, end.unwrap_or(at + 1)));
            }
            0x00 => {
                let label = message.get(at + 1..at + 1 + usize::from(byte))?;
                length += label.len() + 1;
                if length > 255 || !label.is_ascii() {
                    return None;
                }
                labels.push(String::from_utf8_lossy(label).to_ascii_lowercase());
                at += 1 + label.len();
            }
            0xc0 => {
                let target =
                    usize::from(u16::from_be_bytes([byte, *message.get(at + 1)?]) & 0x3fff);
                end.get_or_insert(at + 2);
                // Pointers only point back, so a loop can't go on for ever.
                if target >= at {
                    return None;
                }
                at = target;
            }
            _ => return None,
        }
    }
    None
}
