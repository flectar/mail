//! Finds a new account's server settings from its email address, the way
//! the IETF mailmaint autoconfig draft (draft-ietf-mailmaint-autoconfig)
//! orders the lookups: the mail provider's own configuration file, then
//! Thunderbird's public provider database (ISPDB). All lookups start at
//! once, and a result is used only when every lookup ranked above it has
//! failed, as Thunderbird and Thunderbird for Android do.
//!
//! When neither has the domain, the domain's mail server (MX) points to the
//! provider that hosts its mail, and that provider's configuration is looked
//! up the same way (the draft's section 4.3).
//!
//! A file fetched over plain HTTP could have been altered on the way, and a
//! DNS answer could be forged, so such results are marked untrusted and the
//! user confirms them before connecting (the draft's sections 5.1, 7.2 and
//! 7.3).

mod parse;
#[cfg(test)]
mod tests;

use std::{future::Future, pin::Pin, time::Duration};

use futures::{StreamExt, stream::FuturesUnordered};

use crate::models::{ConnectionSecurity, MailProtocol};
use crate::net::dns;
use crate::net::proxy::{self, Owner, Route};

/// Thunderbird's public provider database, as Thunderbird for Android, KMail
/// and Evolution query it.
const ISPDB: &str = "https://autoconfig.thunderbird.net/v1.1/";
const PROVIDER_TIMEOUT: Duration = Duration::from_secs(5);
const DATABASE_TIMEOUT: Duration = Duration::from_secs(10);

/// A mail server a configuration file names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MailServer {
    pub host: String,
    pub port: u16,
    pub security: ConnectionSecurity,
    /// The sign-in name, when the file gives one.
    pub username: Option<String>,
}

/// A JMAP, CalDAV or CardDAV service a configuration file names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DavServer {
    pub url: String,
    pub username: Option<String>,
}

/// Every usable server one file lists, in its order of preference.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Config {
    provider_name: Option<String>,
    imap: Vec<MailServer>,
    pop3: Vec<MailServer>,
    smtp: Vec<MailServer>,
    jmap: Vec<DavServer>,
    caldav: Vec<DavServer>,
    carddav: Vec<DavServer>,
}

/// Where the settings came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// The provider's own configuration file.
    Provider,
    /// Thunderbird's provider database.
    Database,
    /// The configuration of the provider whose server takes the domain's
    /// mail, found through the domain's MX record.
    MailHost,
}

/// The settings found for an email address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Discovery {
    pub source: Source,
    /// False when the file came over plain HTTP or was found through DNS,
    /// so it could have been altered on the way. The user confirms such
    /// settings first.
    pub trusted: bool,
    /// For `Source::MailHost`, the registrable domain of the server that
    /// takes the address's mail.
    pub mail_host: Option<String>,
    pub provider_name: Option<String>,
    pub imap: Option<MailServer>,
    pub pop3: Option<MailServer>,
    pub smtp: Option<MailServer>,
    pub jmap: Option<DavServer>,
    pub caldav: Option<DavServer>,
    pub carddav: Option<DavServer>,
}

impl Discovery {
    /// The incoming server for `protocol`.
    pub fn incoming(&self, protocol: MailProtocol) -> Option<&MailServer> {
        match protocol {
            MailProtocol::Imap => self.imap.as_ref(),
            MailProtocol::Pop3 => self.pop3.as_ref(),
            _ => None,
        }
    }

    fn from_config(config: Config, source: Source, trusted: bool) -> Option<Self> {
        let discovery = Self {
            source,
            trusted,
            mail_host: None,
            provider_name: config.provider_name,
            imap: preferred(config.imap),
            pop3: preferred(config.pop3),
            smtp: preferred(config.smtp),
            jmap: config.jmap.into_iter().next(),
            caldav: config.caldav.into_iter().next(),
            carddav: config.carddav.into_iter().next(),
        };
        (discovery.imap.is_some() || discovery.pop3.is_some() || discovery.jmap.is_some())
            .then_some(discovery)
    }
}

/// The provider's first server with TLS from the start, else its first with
/// STARTTLS: mail clients should prefer implicit TLS (RFC 8314 section 3).
fn preferred(servers: Vec<MailServer>) -> Option<MailServer> {
    let tls = servers
        .iter()
        .position(|server| server.security == ConnectionSecurity::Tls);
    let index = tls.unwrap_or(0);
    servers.into_iter().nth(index)
}

/// One place a configuration file may be, in priority order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Lookup {
    pub url: String,
    pub source: Source,
    pub timeout: Duration,
}

impl Lookup {
    fn trusted(&self) -> bool {
        self.source != Source::MailHost && self.url.starts_with("https://")
    }
}

/// Where to look for `domain`'s configuration, highest priority first: the
/// draft's URLs 1.1 to 1.4, Thunderbird's well-known URL over HTTP as
/// Thunderbird and Thunderbird for Android still try it, then the database.
/// The email address isn't sent: the draft makes it optional, and servers
/// must answer without it.
pub(crate) fn lookups(domain: &str) -> Vec<Lookup> {
    let provider = |url: String| Lookup {
        url,
        source: Source::Provider,
        timeout: PROVIDER_TIMEOUT,
    };
    vec![
        provider(format!(
            "https://autoconfig.{domain}/.well-known/client-autoconfig.xml"
        )),
        provider(format!("https://autoconfig.{domain}/mail/config-v1.1.xml")),
        provider(format!(
            "https://{domain}/.well-known/autoconfig/mail/config-v1.1.xml"
        )),
        provider(format!("http://autoconfig.{domain}/mail/config-v1.1.xml")),
        provider(format!(
            "http://{domain}/.well-known/autoconfig/mail/config-v1.1.xml"
        )),
        Lookup {
            url: format!("{ISPDB}{domain}"),
            source: Source::Database,
            timeout: DATABASE_TIMEOUT,
        },
    ]
}

/// Where to look for the configuration of the provider whose mail server is
/// under `full` (the server's name without its first label) and `base` (its
/// registrable domain): the draft's URLs 3.1 to 3.6, all over HTTPS.
pub(crate) fn mail_host_lookups(full: &str, base: &str) -> Vec<Lookup> {
    let lookup = |url: String, timeout| Lookup {
        url,
        source: Source::MailHost,
        timeout,
    };
    let mut lookups = Vec::new();
    for path in [".well-known/client-autoconfig.xml", "mail/config-v1.1.xml"] {
        for domain in [full, base] {
            lookups.push(lookup(
                format!("https://autoconfig.{domain}/{path}"),
                PROVIDER_TIMEOUT,
            ));
        }
    }
    for domain in [full, base] {
        lookups.push(lookup(format!("{ISPDB}{domain}"), DATABASE_TIMEOUT));
    }
    lookups.dedup();
    lookups
}

/// The domains to look up for the mail server `host`: the host without its
/// first label, when that is longer than the registrable domain, and the
/// registrable domain, which the Public Suffix List gives, so a lookup
/// never goes to a name under a public suffix such as `co.uk`.
pub(crate) fn mail_host_domains(host: &str) -> Option<(String, String)> {
    let base = psl::domain_str(host)?.to_owned();
    let full = host
        .split_once('.')
        .map(|(_, rest)| rest)
        .filter(|rest| rest.len() > base.len())
        .unwrap_or(&base)
        .to_owned();
    Some((full, base))
}

/// Whether the domain's mail server may be asked of the device's DNS. A
/// proxy can't answer MX questions, so with one in the way the lookup would
/// reveal the domain outside it, and the step is skipped.
fn dns_allowed(domain: &str) -> bool {
    let direct_mail = proxy::mail_routes(Owner::App, "imaps", domain, 993)
        .iter()
        .all(|route| matches!(route, Route::Direct));
    let direct_web = url::Url::parse(&format!("https://autoconfig.{domain}/"))
        .is_ok_and(|url| proxy::http_proxy(Owner::App, &url).is_none());
    direct_mail && direct_web
}

/// Fetches one configuration file, or `None` when there isn't one there.
pub(crate) type Fetch<'a> = Box<
    dyn Fn(Lookup) -> Pin<Box<dyn Future<Output = Option<String>> + Send + 'a>> + Send + Sync + 'a,
>;

/// The mail server a domain names in DNS, or `None`.
pub(crate) type MailHostOf<'a> = Box<
    dyn Fn(String) -> Pin<Box<dyn Future<Output = Option<String>> + Send + 'a>> + Send + Sync + 'a,
>;

/// The settings for `address`, or `None` when no lookup finds a usable file.
pub async fn discover(address: &str) -> Option<Discovery> {
    let client = client().ok()?;
    let fetch: Fetch<'_> = Box::new(move |lookup| {
        let client = client.clone();
        Box::pin(async move { fetch_file(&client, &lookup).await })
    });
    let mail_host: MailHostOf<'_> = Box::new(|domain| {
        Box::pin(async move {
            if dns_allowed(&domain) {
                dns::mail_host(&domain).await
            } else {
                None
            }
        })
    });
    discover_with(address, &fetch, &mail_host).await
}

pub(crate) async fn discover_with(
    address: &str,
    fetch: &Fetch<'_>,
    mail_host: &MailHostOf<'_>,
) -> Option<Discovery> {
    let (local_part, domain) = split_address(address)?;
    let placeholders = parse::Placeholders {
        address: address.trim(),
        local_part: &local_part,
        domain: &domain,
    };
    if let Some(found) = first_found(&lookups(&domain), fetch, &placeholders).await {
        return Some(found);
    }
    // Only when nothing above was found (the draft's section 4.3).
    let host = mail_host(domain.clone()).await?;
    let (full, base) = mail_host_domains(&host)?;
    // The domain's own lookups already covered a server under it.
    if base == domain {
        return None;
    }
    let mut found = first_found(&mail_host_lookups(&full, &base), fetch, &placeholders).await?;
    found.mail_host = Some(base);
    Some(found)
}

/// The best file `lookups` find: a result counts once every lookup ranked
/// above it has failed.
async fn first_found(
    lookups: &[Lookup],
    fetch: &Fetch<'_>,
    placeholders: &parse::Placeholders<'_>,
) -> Option<Discovery> {
    let mut results: Vec<Option<Option<Discovery>>> = vec![None; lookups.len()];
    let mut pending = lookups
        .iter()
        .cloned()
        .enumerate()
        .map(|(index, lookup)| {
            let trusted = lookup.trusted();
            let source = lookup.source;
            async move {
                let found = fetch(lookup)
                    .await
                    .and_then(|xml| parse::parse(&xml, placeholders))
                    .and_then(|config| Discovery::from_config(config, source, trusted));
                (index, found)
            }
        })
        .collect::<FuturesUnordered<_>>();
    while let Some((index, found)) = pending.next().await {
        results[index] = Some(found);
        // The best result so far wins once nothing ranked above it is left.
        for result in &results {
            match result {
                None => break,
                Some(Some(discovery)) => return Some(discovery.clone()),
                Some(None) => {}
            }
        }
    }
    None
}

/// The address's local part and its domain in lowercase ASCII (IDNA).
fn split_address(address: &str) -> Option<(String, String)> {
    let (local, domain) = address.trim().rsplit_once('@')?;
    if local.is_empty() || local.contains(char::is_whitespace) {
        return None;
    }
    let domain = url::Host::parse(domain.trim().trim_end_matches('.')).ok()?;
    match domain {
        url::Host::Domain(domain) if domain.contains('.') => Some((local.to_owned(), domain)),
        _ => None,
    }
}

fn client() -> reqwest::Result<reqwest::Client> {
    proxy::http_builder(Owner::App)
        // A lookup that starts on HTTPS never follows a redirect to plain
        // HTTP, so its file stays trusted. One that starts on HTTP is
        // untrusted wherever it ends.
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            let started_on_http = attempt
                .previous()
                .first()
                .is_some_and(|first| first.scheme() == "http");
            if attempt.previous().len() < 5
                && (attempt.url().scheme() == "https" || started_on_http)
            {
                attempt.follow()
            } else {
                attempt.stop()
            }
        }))
        .user_agent(concat!("Flectar-Mail/", env!("CARGO_PKG_VERSION")))
        .build()
}

async fn fetch_file(client: &reqwest::Client, lookup: &Lookup) -> Option<String> {
    let response = client
        .get(&lookup.url)
        .timeout(lookup.timeout)
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    if response
        .content_length()
        .is_some_and(|length| length > parse::MAX_BYTES as u64)
    {
        return None;
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        body.extend_from_slice(&chunk.ok()?);
        if body.len() > parse::MAX_BYTES {
            return None;
        }
    }
    String::from_utf8(body).ok()
}
