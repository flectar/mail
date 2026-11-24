//! Records a domain publishes, asked of the device's own resolver: the
//! mail server (MX) mail apps look for when a provider publishes no
//! configuration of its own, and the calendar and contacts services (SRV,
//! with a TXT path) of RFC 6764. Each platform's resolver API keeps the
//! device's DNS settings: private or encrypted DNS, VPN and per-domain
//! rules. Answers aren't authenticated, so whatever is derived from them is
//! only offered for the person to confirm, or checked against TLS.

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
const TYPE_TXT: u16 = 16;
const TYPE_SRV: u16 = 33;
const CLASS_IN: u16 = 1;

/// One answer record of the kinds asked for here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Record {
    Mx {
        preference: u16,
        host: String,
    },
    Srv(Service),
    /// The record's character strings (RFC 1035 section 3.3.14).
    Txt(Vec<Vec<u8>>),
}

/// A server an SRV record names (RFC 2782).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Service {
    pub priority: u16,
    pub weight: u16,
    pub port: u16,
    /// The host, in lowercase. "" when the service isn't offered.
    pub target: String,
}

/// The records of `kind` that `name` has, or `None` when the lookup fails,
/// times out or finds none.
async fn query(name: &str, kind: u16) -> Option<Vec<Record>> {
    let name = name.trim().trim_end_matches('.').to_ascii_lowercase();
    if !is_host_name(&name) {
        return None;
    }
    let lookup = tokio::task::spawn_blocking(move || records(&name, kind));
    tokio::time::timeout(TIMEOUT, lookup).await.ok()?.ok()?
}

/// The host of `domain`'s most preferred mail server, in lowercase. `None`
/// when the lookup fails, times out, or the domain takes no mail.
pub(crate) async fn mail_host(domain: &str) -> Option<String> {
    let exchanges = query(domain, TYPE_MX)
        .await?
        .into_iter()
        .filter_map(|record| match record {
            Record::Mx { preference, host } => Some((preference, host)),
            _ => None,
        })
        .collect();
    preferred(exchanges)
}

/// The server to use for `service` (such as `_caldavs._tcp`) under
/// `domain`, chosen by priority and weight as RFC 2782 says. `None` when
/// there is none, or the domain says it doesn't offer the service.
pub(crate) async fn service(service: &str, domain: &str) -> Option<Service> {
    let services = query(&format!("{service}.{domain}"), TYPE_SRV)
        .await?
        .into_iter()
        .filter_map(|record| match record {
            Record::Srv(service) => Some(service),
            _ => None,
        })
        .collect();
    chosen(services, rand::random())
}

/// The value of `key` in the TXT records of `name`, in DNS-SD's key=value
/// form (RFC 6763 section 6), from the first string that has the key.
pub(crate) async fn text_value(name: &str, key: &str) -> Option<String> {
    query(name, TYPE_TXT)
        .await?
        .into_iter()
        .filter_map(|record| match record {
            Record::Txt(strings) => Some(strings),
            _ => None,
        })
        .flatten()
        .find_map(|string| {
            let equals = string.iter().position(|&byte| byte == b'=')?;
            if !string[..equals].eq_ignore_ascii_case(key.as_bytes()) {
                return None;
            }
            String::from_utf8(string[equals + 1..].to_vec()).ok()
        })
}

// Each platform's resolver: the answer records of one kind for a name.
// Blocks.
#[cfg(android_resolver)]
use android::records;
#[cfg(apple_resolver)]
use apple::records;
#[cfg(res_query)]
use res_query::records;
#[cfg(windows)]
use windows::records;

#[cfg(not(any(res_query, android_resolver, apple_resolver, windows)))]
fn records(_name: &str, _kind: u16) -> Option<Vec<Record>> {
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

/// Of the servers an SRV lookup gave, the one to try (RFC 2782): one with
/// the lowest priority, picked by weight with `random`. A single "."
/// target means the service isn't offered.
fn chosen(mut services: Vec<Service>, random: u32) -> Option<Service> {
    if let [only] = services.as_slice()
        && only.target.is_empty()
    {
        return None;
    }
    services.retain(|service| is_host_name(&service.target));
    let priority = services.iter().map(|service| service.priority).min()?;
    services.retain(|service| service.priority == priority);
    // Weight 0 first, then each server takes its share of the total.
    services.sort_by_key(|service| service.weight != 0);
    let total: u32 = services
        .iter()
        .map(|service| u32::from(service.weight))
        .sum();
    let pick = if total == 0 { 0 } else { random % (total + 1) };
    let mut running = 0;
    services.into_iter().find(|service| {
        running += u32::from(service.weight);
        running >= pick
    })
}

/// The answer records of `kind` in a DNS response message (RFC 1035
/// section 4.1).
#[cfg_attr(not(any(res_query, android_resolver, test)), allow(dead_code))]
fn records_in_message(message: &[u8], kind: u16) -> Option<Vec<Record>> {
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
    let mut records = Vec::new();
    for _ in 0..answers {
        let (_, after_name) = read_name(message, at)?;
        let fixed = message.get(after_name..after_name + 10)?;
        let found = u16::from_be_bytes([fixed[0], fixed[1]]);
        let class = u16::from_be_bytes([fixed[2], fixed[3]]);
        let length = usize::from(u16::from_be_bytes([fixed[8], fixed[9]]));
        let data = after_name + 10;
        message.get(data..data + length)?;
        if found == kind && class == CLASS_IN {
            // A record that doesn't parse is skipped, as resolvers do.
            records.extend(record_at(message, kind, data, data + length));
        }
        at = data + length;
    }
    Some(records)
}

/// A record's data on its own, as Apple's resolver hands it over, with its
/// names uncompressed.
#[cfg_attr(not(any(apple_resolver, test)), allow(dead_code))]
fn record_in_data(kind: u16, data: &[u8]) -> Option<Record> {
    record_at(data, kind, 0, data.len())
}

/// The record of `kind` whose data runs from `start` to `end` in `message`.
fn record_at(message: &[u8], kind: u16, start: usize, end: usize) -> Option<Record> {
    let data = message.get(start..end)?;
    let number = |at: usize| Some(u16::from_be_bytes([*data.get(at)?, *data.get(at + 1)?]));
    // A name inside the data has to end where the data does.
    let name_at = |at: usize| {
        let (name, after) = read_name(message, start + at)?;
        (after == end).then_some(name)
    };
    match kind {
        TYPE_MX => Some(Record::Mx {
            preference: number(0)?,
            host: name_at(2)?,
        }),
        TYPE_SRV => Some(Record::Srv(Service {
            priority: number(0)?,
            weight: number(2)?,
            port: number(4)?,
            target: name_at(6)?,
        })),
        TYPE_TXT => {
            let mut strings = Vec::new();
            let mut at = 0;
            while at < data.len() {
                let length = usize::from(data[at]);
                strings.push(data.get(at + 1..at + 1 + length)?.to_vec());
                at += 1 + length;
            }
            Some(Record::Txt(strings))
        }
        _ => None,
    }
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
