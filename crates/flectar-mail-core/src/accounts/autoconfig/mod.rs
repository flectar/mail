//! Finds a new account's server settings from its email address, the way
//! the IETF mailmaint autoconfig draft (draft-ietf-mailmaint-autoconfig)
//! orders the lookups: the mail provider's own configuration file, then
//! Thunderbird's public provider database (ISPDB). All lookups start at
//! once, and a result is used only when every lookup ranked above it has
//! failed, as Thunderbird and Thunderbird for Android do.
//!
//! A file fetched over plain HTTP could have been altered on the way, so its
//! result is marked untrusted and the user confirms it before connecting
//! (the draft's sections 5.1 and 7.3).

mod parse;
#[cfg(test)]
mod tests;

use std::{future::Future, pin::Pin, time::Duration};

use futures::{StreamExt, stream::FuturesUnordered};

use crate::models::{ConnectionSecurity, MailProtocol};
use crate::net::proxy::{self, Owner};

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
}

/// The settings found for an email address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Discovery {
    pub source: Source,
    /// False when the file came over plain HTTP, so it could have been
    /// altered on the way. The user confirms such settings first.
    pub trusted: bool,
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
        self.url.starts_with("https://")
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

/// Fetches one configuration file, or `None` when there isn't one there.
pub(crate) type Fetch<'a> = Box<
    dyn Fn(Lookup) -> Pin<Box<dyn Future<Output = Option<String>> + Send + 'a>> + Send + Sync + 'a,
>;

/// The settings for `address`, or `None` when no lookup finds a usable file.
pub async fn discover(address: &str) -> Option<Discovery> {
    let client = client().ok()?;
    let fetch: Fetch<'_> = Box::new(move |lookup| {
        let client = client.clone();
        Box::pin(async move { fetch_file(&client, &lookup).await })
    });
    discover_with(address, &fetch).await
}

pub(crate) async fn discover_with(address: &str, fetch: &Fetch<'_>) -> Option<Discovery> {
    let (local_part, domain) = split_address(address)?;
    let placeholders = parse::Placeholders {
        address: address.trim(),
        local_part: &local_part,
        domain: &domain,
    };
    let lookups = lookups(&domain);
    let mut results: Vec<Option<Option<Discovery>>> = vec![None; lookups.len()];
    let mut pending = lookups
        .iter()
        .cloned()
        .enumerate()
        .map(|(index, lookup)| {
            let trusted = lookup.trusted();
            let source = lookup.source;
            let placeholders = &placeholders;
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
