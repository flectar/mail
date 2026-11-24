//! Where an account's calendars or contacts are, from its email address:
//! the calendar or address book the provider's configuration names, found
//! as for the mail servers, then RFC 6764's bootstrapping. That is the
//! domain's `_caldavs` or `_carddavs` SRV record with the context path its
//! TXT record gives, and otherwise the domain's own well-known address.
//! Only TLS services are used, as RFC 6764 section 8 asks of clients that
//! support TLS.

use std::{future::Future, pin::Pin, time::Duration};

use super::{Discovery, Source, split_address};
use crate::net::dns;
use crate::net::proxy::{self, Owner};

const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
/// As many redirects as the DAV connections themselves follow.
const MAX_REDIRECTS: usize = 5;

/// The service a connection syncs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DavService {
    Calendar,
    Contacts,
}

impl DavService {
    /// The SRV service over TLS (RFC 6764 section 3, RFC 6352 section 11).
    fn srv(self) -> &'static str {
        match self {
            Self::Calendar => "_caldavs._tcp",
            Self::Contacts => "_carddavs._tcp",
        }
    }

    /// The well-known path (RFC 6764 section 5).
    fn well_known(self) -> &'static str {
        match self {
            Self::Calendar => "/.well-known/caldav",
            Self::Contacts => "/.well-known/carddav",
        }
    }
}

/// Where a calendar or contacts server's address came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DavSource {
    /// A configuration file, from where `Source` says.
    Config(Source),
    /// The domain's SRV record.
    Dns,
    /// The domain's own well-known address over HTTPS.
    WellKnown,
}

/// The server found for a calendar or contacts connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DavDiscovery {
    /// Where to connect: the service's context path, or the server's root,
    /// under which the connection finds the well-known address itself.
    pub url: String,
    /// The sign-in name, when a configuration file gives one.
    pub username: Option<String>,
    pub source: DavSource,
    /// False when the address could have been altered on the way and
    /// points to a server the domain's certificates don't vouch for, so
    /// the person confirms it before connecting.
    pub trusted: bool,
    /// The registrable domain of the server, when it's outside the
    /// address's domain: the mail host for `Config(Source::MailHost)`, or
    /// the SRV record's target.
    pub host: Option<String>,
}

type Pending<'a, T> = Pin<Box<dyn Future<Output = Option<T>> + Send + 'a>>;

/// The lookups the bootstrapping makes, apart for tests.
pub(crate) struct Lookups<'a> {
    /// The address's mail configuration.
    pub config: Box<dyn FnOnce() -> Pending<'a, Discovery> + Send + 'a>,
    /// The server an SRV name gives.
    pub service: Box<dyn Fn(String) -> Pending<'a, dns::Service> + Send + Sync + 'a>,
    /// The `path` key of a name's TXT records.
    pub path: Box<dyn Fn(String) -> Pending<'a, String> + Send + Sync + 'a>,
    /// Where an HTTPS well-known address leads, when a DAV server answers
    /// there.
    pub probe: Box<dyn Fn(String) -> Pending<'a, String> + Send + Sync + 'a>,
}

/// The server for `service` for `address`, or `None` when nothing points
/// to one.
pub async fn discover_dav(address: &str, service: DavService) -> Option<DavDiscovery> {
    let client = probe_client().ok()?;
    let owned = address.to_owned();
    let lookups = Lookups {
        config: Box::new(move || Box::pin(async move { super::discover(&owned).await })),
        service: Box::new(|name| {
            Box::pin(async move {
                let (label, domain) = split_service_name(&name)?;
                if !super::dns_allowed(domain) {
                    return None;
                }
                dns::service(label, domain).await
            })
        }),
        path: Box::new(|name| Box::pin(async move { dns::text_value(&name, "path").await })),
        probe: Box::new(move |url| {
            let client = client.clone();
            Box::pin(async move { probe(&client, &url).await })
        }),
    };
    discover_dav_with(address, service, lookups).await
}

/// The service label and the domain of an SRV name such as
/// `_caldavs._tcp.example.com`.
fn split_service_name(name: &str) -> Option<(&str, &str)> {
    let protocol_end = name.find("._tcp.")? + "._tcp".len();
    Some((&name[..protocol_end], &name[protocol_end + 1..]))
}

pub(crate) async fn discover_dav_with(
    address: &str,
    service: DavService,
    lookups: Lookups<'_>,
) -> Option<DavDiscovery> {
    let (_, domain) = split_address(address)?;
    let srv_name = format!("{}.{domain}", service.srv());
    // The configuration and the DNS record are looked up at once; the
    // provider's own file comes first.
    let (config, srv) = futures::join!((lookups.config)(), (lookups.service)(srv_name.clone()));
    if let Some(found) = config.and_then(|discovery| from_config(&discovery, service)) {
        return Some(found);
    }
    if let Some(srv) = srv {
        // A valid SRV record makes the TXT record's path the context path
        // (RFC 6764 section 6, step 3). Without one, the connection tries
        // the well-known address on that server itself.
        let path = (lookups.path)(srv_name)
            .await
            .filter(|path| path.starts_with('/') && !path.contains(char::is_whitespace));
        let port = if srv.port == 443 {
            String::new()
        } else {
            format!(":{}", srv.port)
        };
        let url = format!(
            "https://{}{port}{}",
            srv.target,
            path.as_deref().unwrap_or("/")
        );
        url::Url::parse(&url).ok()?;
        // A server in the queried domain proves itself with that domain's
        // certificate. One elsewhere could be anyone's, so the person
        // confirms it first (RFC 6764 section 8).
        let inside = in_domain(&srv.target, &domain);
        return Some(DavDiscovery {
            url,
            username: None,
            source: DavSource::Dns,
            trusted: inside,
            host: (!inside).then(|| registrable(&srv.target)),
        });
    }
    // RFC 6764's heuristic for a domain without the record: the domain
    // itself, over TLS.
    let url = (lookups.probe)(format!("https://{domain}{}", service.well_known())).await?;
    Some(DavDiscovery {
        url,
        username: None,
        source: DavSource::WellKnown,
        trusted: true,
        host: None,
    })
}

/// The calendar or address book a configuration names, over HTTPS.
fn from_config(discovery: &Discovery, service: DavService) -> Option<DavDiscovery> {
    let server = match service {
        DavService::Calendar => discovery.caldav.as_ref(),
        DavService::Contacts => discovery.carddav.as_ref(),
    }?;
    url::Url::parse(&server.url)
        .ok()
        .filter(|url| url.scheme() == "https")?;
    Some(DavDiscovery {
        url: server.url.clone(),
        username: server.username.clone(),
        source: DavSource::Config(discovery.source),
        trusted: discovery.trusted,
        host: discovery.mail_host.clone(),
    })
}

/// Whether `host` is `domain` or a name under it.
fn in_domain(host: &str, domain: &str) -> bool {
    host == domain
        || host
            .strip_suffix(domain)
            .is_some_and(|rest| rest.ends_with('.'))
}

/// The registrable domain of `host`, or the host itself.
fn registrable(host: &str) -> String {
    psl::domain_str(host).unwrap_or(host).to_owned()
}

fn probe_client() -> reqwest::Result<reqwest::Client> {
    proxy::http_builder(Owner::App)
        // Each redirect is checked and followed below.
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("Flectar-Mail/", env!("CARGO_PKG_VERSION")))
        .build()
}

/// Where the well-known address `url` leads: the context path a DAV server
/// redirects to (RFC 6764 section 5), or the server's root when it asks to
/// sign in first. `None` when no DAV server answers there. No credentials
/// are sent, and only HTTPS is followed.
async fn probe(client: &reqwest::Client, url: &str) -> Option<String> {
    let start = url::Url::parse(url).ok()?;
    let mut current = start.clone();
    let method = reqwest::Method::from_bytes(b"PROPFIND").ok()?;
    for _ in 0..=MAX_REDIRECTS {
        let response = client
            .request(method.clone(), current.clone())
            .header("Depth", "0")
            .timeout(PROBE_TIMEOUT)
            .send()
            .await
            .ok()?;
        let status = response.status();
        if status.is_redirection() {
            let location = response.headers().get(reqwest::header::LOCATION)?;
            let next = current.join(location.to_str().ok()?).ok()?;
            if next.scheme() != "https" {
                return None;
            }
            current = next;
            continue;
        }
        return answered(&start, &current, status.as_u16());
    }
    None
}

/// What a probe that ended at `current` with `status` found. A DAV
/// server answers PROPFIND with a multistatus, or asks to sign in first,
/// in which case it may only redirect once signed in.
fn answered(start: &url::Url, current: &url::Url, status: u16) -> Option<String> {
    if status != 207 && status != 401 {
        return None;
    }
    if current == start {
        let mut root = start.clone();
        root.set_path("/");
        return Some(root.to_string());
    }
    Some(current.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::autoconfig::DavServer;

    fn discovery(source: Source, trusted: bool) -> Discovery {
        Discovery {
            source,
            trusted,
            mail_host: None,
            provider_name: None,
            imap: None,
            pop3: None,
            smtp: None,
            jmap: None,
            caldav: Some(DavServer {
                url: "https://dav.example.com/cal/".into(),
                username: Some("ada".into()),
            }),
            carddav: None,
        }
    }

    fn lookups(
        config: Option<Discovery>,
        srv: Option<(&'static str, u16)>,
        path: Option<&'static str>,
        probe: Option<&'static str>,
    ) -> Lookups<'static> {
        Lookups {
            config: Box::new(move || Box::pin(async move { config })),
            service: Box::new(move |name| {
                Box::pin(async move {
                    assert!(name.starts_with("_ca"), "{name}");
                    srv.map(|(target, port)| dns::Service {
                        priority: 0,
                        weight: 0,
                        port,
                        target: target.into(),
                    })
                })
            }),
            path: Box::new(move |_| Box::pin(async move { path.map(str::to_owned) })),
            probe: Box::new(move |url| {
                Box::pin(async move {
                    assert_eq!(url, "https://example.com/.well-known/carddav");
                    probe.map(str::to_owned)
                })
            }),
        }
    }

    #[tokio::test]
    async fn the_providers_configuration_comes_first() {
        let found = discover_dav_with(
            "ada@example.com",
            DavService::Calendar,
            lookups(
                Some(discovery(Source::Provider, true)),
                Some(("other.example.net", 443)),
                None,
                None,
            ),
        )
        .await
        .unwrap();
        assert_eq!(found.url, "https://dav.example.com/cal/");
        assert_eq!(found.username.as_deref(), Some("ada"));
        assert_eq!(found.source, DavSource::Config(Source::Provider));
        assert!(found.trusted);
    }

    #[tokio::test]
    async fn a_configuration_without_this_service_falls_through_to_dns() {
        let found = discover_dav_with(
            "ada@example.com",
            DavService::Contacts,
            lookups(
                Some(discovery(Source::Provider, true)),
                Some(("dav.example.com", 8443)),
                Some("/dav/addressbooks/"),
                None,
            ),
        )
        .await
        .unwrap();
        assert_eq!(found.url, "https://dav.example.com:8443/dav/addressbooks/");
        assert_eq!(found.source, DavSource::Dns);
        assert!(found.trusted, "a server in the domain");
        assert_eq!(found.host, None);
    }

    #[tokio::test]
    async fn a_server_outside_the_domain_needs_confirming() {
        let found = discover_dav_with(
            "ada@example.com",
            DavService::Contacts,
            lookups(
                None,
                Some(("dav.hoster.co.uk", 443)),
                Some("not a path"),
                None,
            ),
        )
        .await
        .unwrap();
        assert_eq!(found.url, "https://dav.hoster.co.uk/");
        assert!(!found.trusted);
        assert_eq!(found.host.as_deref(), Some("hoster.co.uk"));
        // A name that only ends in the domain's letters isn't under it.
        assert!(!in_domain("evilexample.com", "example.com"));
        assert!(in_domain("dav.example.com", "example.com"));
    }

    #[tokio::test]
    async fn without_a_record_the_domains_well_known_address_is_probed() {
        let found = discover_dav_with(
            "ada@example.com",
            DavService::Contacts,
            lookups(
                None,
                None,
                None,
                Some("https://cloud.example.com/remote.php/dav/"),
            ),
        )
        .await
        .unwrap();
        assert_eq!(found.url, "https://cloud.example.com/remote.php/dav/");
        assert_eq!(found.source, DavSource::WellKnown);
        assert!(found.trusted);
        assert_eq!(
            discover_dav_with(
                "ada@example.com",
                DavService::Contacts,
                lookups(None, None, None, None)
            )
            .await,
            None
        );
    }

    #[tokio::test]
    async fn a_configuration_over_http_or_from_the_mail_host_stays_untrusted() {
        let mut from_host = discovery(Source::MailHost, false);
        from_host.mail_host = Some("hoster.example".into());
        let found = discover_dav_with(
            "ada@example.com",
            DavService::Calendar,
            lookups(Some(from_host), None, None, None),
        )
        .await
        .unwrap();
        assert!(!found.trusted);
        assert_eq!(found.host.as_deref(), Some("hoster.example"));
    }

    #[test]
    fn a_probe_finds_a_server_that_answers_or_asks_to_sign_in() {
        let start = url::Url::parse("https://example.com/.well-known/caldav").unwrap();
        let moved = url::Url::parse("https://cal.example.com/dav/").unwrap();
        assert_eq!(
            answered(&start, &moved, 401).as_deref(),
            Some("https://cal.example.com/dav/")
        );
        assert_eq!(
            answered(&start, &start, 401).as_deref(),
            Some("https://example.com/"),
            "redirects only after signing in"
        );
        assert_eq!(answered(&start, &moved, 200), None, "a web page");
        assert_eq!(answered(&start, &start, 404), None);
    }

    #[test]
    fn service_names_split_into_label_and_domain() {
        assert_eq!(
            split_service_name("_carddavs._tcp.example.co.uk"),
            Some(("_carddavs._tcp", "example.co.uk"))
        );
        assert_eq!(split_service_name("example.com"), None);
    }
}
