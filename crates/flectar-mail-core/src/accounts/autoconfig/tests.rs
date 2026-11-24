use std::{collections::HashMap, sync::Arc, time::Duration};

use super::{
    DavServer, Discovery, Fetch, Lookup, MailHostOf, MailServer, Source, discover_with, lookups,
    mail_host_domains, mail_host_lookups, parse, split_address,
};
use crate::models::{ConnectionSecurity, MailProtocol};

const PROVIDER_FILE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<clientConfig version="1.1">
  <emailProvider id="example.com">
    <domain>example.com</domain>
    <displayName>Example &amp; Co</displayName>
    <incomingServer type="imap">
      <hostname>imap.example.com</hostname>
      <port>143</port>
      <socketType>STARTTLS</socketType>
      <authentication>password-cleartext</authentication>
      <username>%EMAILLOCALPART%</username>
    </incomingServer>
    <incomingServer type="imap">
      <hostname>imap.example.com</hostname>
      <port>993</port>
      <socketType>SSL</socketType>
      <authentication>password-cleartext</authentication>
      <username>%EMAILLOCALPART%</username>
    </incomingServer>
    <incomingServer type="pop3">
      <hostname>pop.example.com</hostname>
      <port>110</port>
      <socketType>plain</socketType>
      <authentication>password-cleartext</authentication>
    </incomingServer>
    <incomingServer type="exchange">
      <hostname>exchange.example.com</hostname>
    </incomingServer>
    <outgoingServer type="smtp">
      <hostname>smtp.%EMAILDOMAIN%</hostname>
      <port>587</port>
      <socketType>STARTTLS</socketType>
      <authentication>OAuth2</authentication>
      <username>%EMAILADDRESS%</username>
    </outgoingServer>
    <documentation url="https://example.com/help"><descr lang="en">Help</descr></documentation>
  </emailProvider>
  <webMail><loginPage url="https://mail.example.com/" /></webMail>
</clientConfig>"#;

const DRAFT_FILE: &str = r#"<clientConfig version="1.2">
  <emailProvider id="example.net">
    <incomingServer type="jmap">
      <url>https://jmap.example.net/.well-known/jmap</url>
      <authentication>password-cleartext</authentication>
      <username>%EMAILADDRESS%</username>
    </incomingServer>
    <incomingServer type="imap">
      <hostname>imap.example.net</hostname>
      <port>993</port>
      <socketType>SSL</socketType>
      <authentication>GSSAPI</authentication>
    </incomingServer>
  </emailProvider>
  <addressbook type="carddav">
    <url>https://dav.example.net/remote.php/dav</url>
    <authentication system="http">Basic</authentication>
    <username>%EMAILADDRESS%</username>
  </addressbook>
  <calendar type="caldav">
    <url>http://dav.example.net/remote.php/dav</url>
  </calendar>
  <chatServer type="xmpp"><url>wss://example.net/xmpp</url></chatServer>
</clientConfig>"#;

fn placeholders() -> parse::Placeholders<'static> {
    parse::Placeholders {
        address: "ada@example.com",
        local_part: "ada",
        domain: "example.com",
    }
}

#[test]
fn provider_files_give_the_usable_servers_with_placeholders_filled() {
    let config = parse::parse(PROVIDER_FILE, &placeholders()).unwrap();
    let discovery = Discovery::from_config(config, Source::Provider, true).unwrap();
    assert_eq!(discovery.provider_name.as_deref(), Some("Example & Co"));
    // TLS from the start is preferred over the file's first, STARTTLS entry.
    assert_eq!(
        discovery.imap,
        Some(MailServer {
            host: "imap.example.com".into(),
            port: 993,
            security: ConnectionSecurity::Tls,
            username: Some("ada".into()),
        })
    );
    // A server without encryption is never offered.
    assert_eq!(discovery.pop3, None);
    // OAuth2 providers take app passwords on the same server.
    assert_eq!(
        discovery.smtp,
        Some(MailServer {
            host: "smtp.example.com".into(),
            port: 587,
            security: ConnectionSecurity::Starttls,
            username: Some("ada@example.com".into()),
        })
    );
    assert_eq!(
        discovery.incoming(MailProtocol::Imap),
        discovery.imap.as_ref()
    );
}

#[test]
fn draft_files_give_jmap_and_https_address_books_only() {
    let config = parse::parse(DRAFT_FILE, &placeholders()).unwrap();
    let discovery = Discovery::from_config(config, Source::Provider, true).unwrap();
    assert_eq!(
        discovery.jmap,
        Some(DavServer {
            url: "https://jmap.example.net/.well-known/jmap".into(),
            username: Some("ada@example.com".into()),
        })
    );
    // Kerberos sign-in can't take the password the form asks for.
    assert_eq!(discovery.imap, None);
    assert_eq!(
        discovery.carddav.map(|book| book.url),
        Some("https://dav.example.net/remote.php/dav".into())
    );
    // A calendar served over plain HTTP isn't used.
    assert_eq!(discovery.caldav, None);
}

#[test]
fn files_that_are_not_configurations_or_name_odd_hosts_give_nothing() {
    let placeholders = placeholders();
    for text in [
        "",
        "<html><body>Not found</body></html>",
        "<clientConfig><emailProvider>",
        "not xml at all <<<",
    ] {
        let found = parse::parse(text, &placeholders)
            .and_then(|config| Discovery::from_config(config, Source::Provider, true));
        assert_eq!(found, None, "{text:?}");
    }
    for host in [
        "imap.example.com:993",
        "https://imap.example.com",
        "imap example.com",
        "192.0.2.1",
        "localhost",
        "-bad.example.com",
    ] {
        let file = PROVIDER_FILE.replace("imap.example.com", host);
        let config = parse::parse(&file, &placeholders).unwrap();
        assert!(config.imap.is_empty(), "{host}");
    }
    let huge = format!(
        "<clientConfig>{}</clientConfig>",
        " ".repeat(parse::MAX_BYTES)
    );
    assert_eq!(parse::parse(&huge, &placeholders), None);
}

#[test]
fn lookups_follow_the_draft_order_without_the_address() {
    let urls = lookups("example.com")
        .into_iter()
        .map(|lookup| (lookup.url, lookup.source))
        .collect::<Vec<_>>();
    assert_eq!(
        urls,
        [
            (
                "https://autoconfig.example.com/.well-known/client-autoconfig.xml",
                Source::Provider
            ),
            (
                "https://autoconfig.example.com/mail/config-v1.1.xml",
                Source::Provider
            ),
            (
                "https://example.com/.well-known/autoconfig/mail/config-v1.1.xml",
                Source::Provider
            ),
            (
                "http://autoconfig.example.com/mail/config-v1.1.xml",
                Source::Provider
            ),
            (
                "http://example.com/.well-known/autoconfig/mail/config-v1.1.xml",
                Source::Provider
            ),
            (
                "https://autoconfig.thunderbird.net/v1.1/example.com",
                Source::Database
            ),
        ]
        .map(|(url, source)| (url.to_owned(), source))
    );
    assert!(urls.iter().all(|(url, _)| !url.contains('@')));
}

#[test]
fn addresses_split_into_a_local_part_and_an_ascii_domain() {
    assert_eq!(
        split_address(" Ada@Example.COM. "),
        Some(("Ada".into(), "example.com".into()))
    );
    assert_eq!(
        split_address("ada@bücher.example"),
        Some(("ada".into(), "xn--bcher-kva.example".into()))
    );
    for address in [
        "ada",
        "@example.com",
        "ada@localhost",
        "ada@[192.0.2.1]",
        "a b@example.com",
    ] {
        assert_eq!(split_address(address), None, "{address}");
    }
}

/// A fetcher that answers each URL after a delay, or not at all.
fn fake(answers: Vec<(&'static str, u64, &'static str)>) -> Fetch<'static> {
    let answers = Arc::new(
        answers
            .into_iter()
            .map(|(url, delay, body)| (url.to_owned(), (delay, body)))
            .collect::<HashMap<_, _>>(),
    );
    Box::new(move |lookup: Lookup| {
        let answer = answers.get(&lookup.url).copied();
        Box::pin(async move {
            let (delay, body) = answer?;
            tokio::time::sleep(Duration::from_millis(delay)).await;
            Some(body.to_owned())
        })
    })
}

#[tokio::test(start_paused = true)]
async fn a_higher_priority_file_wins_even_when_it_answers_later() {
    let fetch = fake(vec![
        (
            "https://autoconfig.example.com/mail/config-v1.1.xml",
            3_000,
            PROVIDER_FILE,
        ),
        (
            "https://autoconfig.thunderbird.net/v1.1/example.com",
            10,
            DRAFT_FILE,
        ),
    ]);
    let found = discover_with("ada@example.com", &fetch, &no_mail_host())
        .await
        .unwrap();
    assert_eq!(found.source, Source::Provider);
    assert!(found.trusted);
    assert_eq!(found.imap.unwrap().host, "imap.example.com");
}

#[tokio::test(start_paused = true)]
async fn files_over_plain_http_are_untrusted_and_rank_below_https() {
    let fetch = fake(vec![(
        "http://autoconfig.example.com/mail/config-v1.1.xml",
        10,
        PROVIDER_FILE,
    )]);
    let found = discover_with("ada@example.com", &fetch, &no_mail_host())
        .await
        .unwrap();
    assert_eq!(found.source, Source::Provider);
    assert!(!found.trusted, "HTTP can be altered on the way");

    let fetch = fake(vec![
        (
            "http://autoconfig.example.com/mail/config-v1.1.xml",
            10,
            PROVIDER_FILE,
        ),
        (
            "https://example.com/.well-known/autoconfig/mail/config-v1.1.xml",
            2_000,
            DRAFT_FILE,
        ),
    ]);
    let found = discover_with("ada@example.com", &fetch, &no_mail_host())
        .await
        .unwrap();
    assert!(found.trusted);
    assert!(found.jmap.is_some());
}

#[tokio::test(start_paused = true)]
async fn the_database_is_used_when_the_provider_has_no_file() {
    let fetch = fake(vec![
        (
            "https://autoconfig.example.com/mail/config-v1.1.xml",
            10,
            "<html>404</html>",
        ),
        (
            "https://autoconfig.thunderbird.net/v1.1/example.com",
            500,
            PROVIDER_FILE,
        ),
    ]);
    let found = discover_with("ada@example.com", &fetch, &no_mail_host())
        .await
        .unwrap();
    assert_eq!(found.source, Source::Database);
    assert!(found.trusted);

    let fetch = fake(Vec::new());
    assert_eq!(
        discover_with("ada@example.com", &fetch, &no_mail_host()).await,
        None
    );
    assert_eq!(
        discover_with("not an address", &fetch, &no_mail_host()).await,
        None
    );
}

fn no_mail_host() -> MailHostOf<'static> {
    Box::new(|_| Box::pin(async { None }))
}

/// A resolver that names `host` as every domain's mail server, and counts
/// the questions.
fn mail_host(
    host: &'static str,
    asked: Arc<std::sync::atomic::AtomicUsize>,
) -> MailHostOf<'static> {
    Box::new(move |_| {
        asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async move { Some(host.to_owned()) })
    })
}

#[test]
fn mail_servers_are_looked_up_by_their_registrable_domain() {
    let domains = |host| mail_host_domains(host).unwrap();
    assert_eq!(
        domains("aspmx.l.google.com"),
        ("l.google.com".into(), "google.com".into())
    );
    assert_eq!(
        domains("mx.example.co.uk"),
        ("example.co.uk".into(), "example.co.uk".into())
    );
    assert_eq!(
        domains("mx.example.com"),
        ("example.com".into(), "example.com".into())
    );
    // A host that is itself a public suffix has no registrable domain.
    assert_eq!(mail_host_domains("co.uk"), None);

    let urls = mail_host_lookups("premium.europe.example.com", "example.com")
        .into_iter()
        .map(|lookup| {
            assert_eq!(lookup.source, Source::MailHost);
            assert!(!lookup.trusted(), "DNS answers can be forged");
            lookup.url
        })
        .collect::<Vec<_>>();
    assert_eq!(
        urls,
        [
            "https://autoconfig.premium.europe.example.com/.well-known/client-autoconfig.xml",
            "https://autoconfig.example.com/.well-known/client-autoconfig.xml",
            "https://autoconfig.premium.europe.example.com/mail/config-v1.1.xml",
            "https://autoconfig.example.com/mail/config-v1.1.xml",
            "https://autoconfig.thunderbird.net/v1.1/premium.europe.example.com",
            "https://autoconfig.thunderbird.net/v1.1/example.com",
        ]
    );
    assert_eq!(mail_host_lookups("example.com", "example.com").len(), 3);
}

#[tokio::test(start_paused = true)]
async fn a_domain_hosted_elsewhere_gets_its_host_s_settings_untrusted() {
    let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let fetch = fake(vec![(
        "https://autoconfig.thunderbird.net/v1.1/google.com",
        10,
        PROVIDER_FILE,
    )]);
    let found = discover_with(
        "ada@family.example",
        &fetch,
        &mail_host("aspmx.l.google.com", asked.clone()),
    )
    .await
    .unwrap();
    assert_eq!(found.source, Source::MailHost);
    assert!(!found.trusted);
    assert_eq!(found.mail_host.as_deref(), Some("google.com"));
    assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn dns_is_asked_only_when_the_domain_itself_has_nothing() {
    let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let fetch = fake(vec![
        (
            "https://autoconfig.example.com/mail/config-v1.1.xml",
            10,
            PROVIDER_FILE,
        ),
        (
            "https://autoconfig.thunderbird.net/v1.1/hoster.example",
            10,
            DRAFT_FILE,
        ),
    ]);
    let found = discover_with(
        "ada@example.com",
        &fetch,
        &mail_host("mx.hoster.example", asked.clone()),
    )
    .await
    .unwrap();
    assert_eq!(found.source, Source::Provider);
    assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 0);

    // A mail server under the address's own domain adds nothing to look up:
    // the domain's own lookups already asked there.
    let fetch = fake(Vec::new());
    let found = discover_with(
        "ada@example.net",
        &fetch,
        &mail_host("mx.example.net", asked.clone()),
    )
    .await;
    assert_eq!(found, None);
    assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 1);
}
