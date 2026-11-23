//! The servers a new mail account's form fills in from its email address
//! (#478): the person's other accounts on the same domain, Gmail's servers
//! for a Gmail address, and otherwise what
//! `flectar_mail_core::accounts::autoconfig` finds for the domain. Only a
//! form whose servers are still untouched, or still show the last
//! suggestion, is filled in.

use super::*;
use flectar_mail_core::accounts::autoconfig::{self, Discovery, Source};
use flectar_mail_core::models::{
    AccountSettings, AuthKind, ConnectionSecurity, MailConnectionSettings,
};
use std::sync::atomic::{AtomicU64, Ordering};

/// How long an address has to stay unchanged before it's looked up, so
/// typing it doesn't start a lookup per character.
const SETTLE: std::time::Duration = std::time::Duration::from_millis(600);

/// Counts the form's suggestions, so a lookup for an address that has since
/// changed is dropped.
static GENERATION: AtomicU64 = AtomicU64::new(0);

#[derive(Default)]
struct Form {
    /// The servers last filled in, which the next suggestion may replace.
    servers: Option<AccountConfig>,
    /// The username last filled in, likewise.
    username: Option<String>,
    /// The JMAP address last filled in, likewise.
    jmap_url: Option<String>,
    /// The last domain whose settings were found, and those settings.
    found: Option<(String, Discovery)>,
}

thread_local! {
    static FORM: RefCell<Form> = RefCell::default();
}

/// Fills in the form's servers for `email`, or starts looking them up.
pub(super) fn suggest(
    app: &AppWindow,
    configs: &[AccountConfig],
    email: &str,
    runtime: &tokio::runtime::Handle,
) {
    let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let notice = app.global::<AccountDiscovery>();
    notice.set_confirmed(false);
    let Some(protocol) = form_protocol(app) else {
        notice.set_state("".into());
        return;
    };
    if !takes_suggestion(app, protocol) {
        // The person entered servers of their own.
        FORM.with_borrow_mut(|form| {
            *form = Form {
                found: form.found.take(),
                ..Form::default()
            }
        });
        notice.set_state("".into());
        return;
    }
    if protocol != MailProtocol::Jmap
        && let Some(config) = suggested_mail_transport(configs, email, protocol)
    {
        fill(app, &config, None);
        notice.set_state("".into());
        return;
    }
    let Some(domain) = lookup_domain(email) else {
        clear(app);
        notice.set_state("".into());
        return;
    };
    let cached = FORM.with_borrow(|form| {
        form.found
            .as_ref()
            .filter(|(found, _)| *found == domain)
            .map(|(_, discovery)| discovery.clone())
    });
    if let Some(discovery) = cached {
        apply(app, email, &domain, Some(&discovery));
        return;
    }
    // The servers of another address don't stay while this one is looked up.
    clear(app);
    notice.set_domain(domain.as_str().into());
    notice.set_state("looking".into());
    let window = app.as_weak();
    let address = email.trim().to_owned();
    runtime.spawn(async move {
        tokio::time::sleep(SETTLE).await;
        if GENERATION.load(Ordering::SeqCst) != generation {
            return;
        }
        let found = autoconfig::discover(&address).await;
        let _ = window.upgrade_in_event_loop(move |app| {
            if GENERATION.load(Ordering::SeqCst) != generation {
                return;
            }
            if let Some(discovery) = &found {
                FORM.with_borrow_mut(|form| form.found = Some((domain.clone(), discovery.clone())));
            }
            apply(&app, &address, &domain, found.as_ref());
        });
    });
}

/// The form's protocol, when its servers can be suggested.
fn form_protocol(app: &AppWindow) -> Option<MailProtocol> {
    match MailProtocol::parse(app.get_account_protocol().as_str()) {
        Some(protocol @ (MailProtocol::Imap | MailProtocol::Pop3 | MailProtocol::Jmap)) => {
            Some(protocol)
        }
        _ => None,
    }
}

fn standard_incoming_port(protocol: MailProtocol) -> &'static str {
    if protocol == MailProtocol::Pop3 {
        "995"
    } else {
        "993"
    }
}

/// Whether the form's servers are still untouched or show the last
/// suggestion, so a new suggestion may replace them.
fn takes_suggestion(app: &AppWindow, protocol: MailProtocol) -> bool {
    if protocol == MailProtocol::Jmap {
        let url = app.get_jmap_url();
        return url.trim().is_empty()
            || FORM.with_borrow(|form| form.jmap_url.as_deref() == Some(url.as_str()));
    }
    let shows_last = FORM.with_borrow(|form| {
        form.servers.as_ref().is_some_and(|config| {
            app.get_incoming_host().as_str() == config.incoming_host
                && app.get_incoming_port().as_str() == config.incoming_port.to_string()
                && app.get_smtp_host().as_str() == config.smtp_host
                && app.get_smtp_port().as_str() == config.smtp_port.to_string()
                && app.get_incoming_security().as_str()
                    == config.settings.connection.incoming_security.as_str()
                && app.get_smtp_security().as_str()
                    == config.settings.connection.smtp_security.as_str()
                && app.get_trusted_certificate_pem().as_str()
                    == config.settings.connection.trusted_certificate_pem
        })
    });
    shows_last
        || (app.get_incoming_host().trim().is_empty()
            && app.get_smtp_host().trim().is_empty()
            && [
                standard_incoming_port(MailProtocol::Imap),
                standard_incoming_port(MailProtocol::Pop3),
            ]
            .contains(&app.get_incoming_port().as_str())
            && app.get_smtp_port().as_str() == "465"
            && app.get_incoming_security().as_str() == "tls"
            && app.get_smtp_security().as_str() == "tls"
            && app.get_trusted_certificate_pem().trim().is_empty())
}

/// The address's domain to look up: lowercase, with at least one dot.
fn lookup_domain(email: &str) -> Option<String> {
    let domain = mail_domain(email)?.to_lowercase();
    domain.contains('.').then_some(domain)
}

/// Shows what was found for `domain` and fills the form in with it.
fn apply(app: &AppWindow, email: &str, domain: &str, found: Option<&Discovery>) {
    let notice = app.global::<AccountDiscovery>();
    notice.set_domain(domain.into());
    let Some(protocol) = form_protocol(app) else {
        notice.set_state("".into());
        return;
    };
    if !takes_suggestion(app, protocol) {
        notice.set_state("".into());
        return;
    }
    let Some(discovery) = found else {
        notice.set_state("none".into());
        return;
    };
    let filled = if protocol == MailProtocol::Jmap {
        discovery.jmap.as_ref().is_some_and(|jmap| {
            app.set_jmap_url(jmap.url.as_str().into());
            FORM.with_borrow_mut(|form| form.jmap_url = Some(jmap.url.clone()));
            fill_username(app, email, jmap.username.as_deref());
            true
        })
    } else {
        // A provider with only POP3 gets a POP3 form, as the person hasn't
        // chosen servers of their own yet.
        let protocol = if discovery.incoming(protocol).is_none()
            && protocol == MailProtocol::Imap
            && discovery.pop3.is_some()
        {
            app.set_account_protocol("pop3".into());
            MailProtocol::Pop3
        } else {
            protocol
        };
        transport(email, protocol, discovery).is_some_and(|(config, username)| {
            fill(app, &config, username.as_deref());
            true
        })
    };
    if !filled {
        notice.set_state("none".into());
        return;
    }
    notice.set_source(
        match discovery.source {
            Source::Provider => "provider",
            Source::Database => "database",
            Source::MailHost => "mail-host",
        }
        .into(),
    );
    notice.set_mail_host(discovery.mail_host.as_deref().unwrap_or_default().into());
    notice.set_trusted(discovery.trusted);
    notice.set_confirmed(false);
    notice.set_state("found".into());
}

/// The form's servers for `protocol` from what was found, with the sign-in
/// name the file gives.
fn transport(
    email: &str,
    protocol: MailProtocol,
    discovery: &Discovery,
) -> Option<(AccountConfig, Option<String>)> {
    let incoming = discovery.incoming(protocol)?;
    let smtp = discovery.smtp.as_ref();
    let config = AccountConfig {
        id: 0,
        email: email.trim().into(),
        display_name: None,
        name: None,
        avatar_url: None,
        provider: Provider::Generic,
        auth_kind: AuthKind::Password,
        mail_protocol: protocol,
        username: String::new(),
        jmap_url: String::new(),
        jmap_account_id: None,
        incoming_host: incoming.host.clone(),
        incoming_port: incoming.port,
        smtp_host: smtp.map(|server| server.host.clone()).unwrap_or_default(),
        smtp_port: smtp.map_or(465, |server| server.port),
        settings: AccountSettings::new(MailConnectionSettings::new(
            incoming.security,
            smtp.map_or(ConnectionSecurity::Tls, |server| server.security),
        )),
    };
    Some((config, incoming.username.clone()))
}

fn fill(app: &AppWindow, config: &AccountConfig, username: Option<&str>) {
    app.set_incoming_host(config.incoming_host.as_str().into());
    app.set_incoming_port(config.incoming_port.to_string().into());
    app.set_smtp_host(config.smtp_host.as_str().into());
    app.set_smtp_port(config.smtp_port.to_string().into());
    app.set_incoming_security(config.settings.connection.incoming_security.as_str().into());
    app.set_smtp_security(config.settings.connection.smtp_security.as_str().into());
    app.set_trusted_certificate_pem(
        config
            .settings
            .connection
            .trusted_certificate_pem
            .as_str()
            .into(),
    );
    FORM.with_borrow_mut(|form| form.servers = Some(config.clone()));
    fill_username(app, &config.email, username);
}

/// Fills in the file's sign-in name when it isn't the address itself, the
/// form's default, and the person hasn't typed one.
fn fill_username(app: &AppWindow, email: &str, username: Option<&str>) {
    let username = username
        .filter(|name| !name.eq_ignore_ascii_case(email.trim()))
        .map(str::to_owned);
    let current = app.get_account_username();
    let ours = FORM.with_borrow(|form| form.username.as_deref() == Some(current.as_str()));
    if current.is_empty() || ours {
        app.set_account_username(username.as_deref().unwrap_or_default().into());
        FORM.with_borrow_mut(|form| form.username = username);
    }
}

/// Takes back the servers and username last filled in, if the form still
/// shows them.
fn clear(app: &AppWindow) {
    let Some(protocol) = form_protocol(app) else {
        return;
    };
    let shows_last = FORM.with_borrow(|form| form.servers.is_some() || form.jmap_url.is_some())
        && takes_suggestion(app, protocol);
    if shows_last {
        if protocol == MailProtocol::Jmap {
            app.set_jmap_url("".into());
        } else {
            app.set_incoming_host("".into());
            app.set_incoming_port(standard_incoming_port(protocol).into());
            app.set_smtp_host("".into());
            app.set_smtp_port("465".into());
            app.set_incoming_security("tls".into());
            app.set_smtp_security("tls".into());
            app.set_trusted_certificate_pem("".into());
        }
        fill_username(app, "", None);
    }
    FORM.with_borrow_mut(|form| {
        form.servers = None;
        form.jmap_url = None;
    });
}

#[cfg(test)]
mod tests {
    use super::{lookup_domain, transport};
    use flectar_mail_core::accounts::autoconfig::{Discovery, MailServer, Source};
    use flectar_mail_core::models::{ConnectionSecurity, MailProtocol};

    fn server(host: &str, port: u16, security: ConnectionSecurity) -> MailServer {
        MailServer {
            host: host.into(),
            port,
            security,
            username: None,
        }
    }

    #[test]
    fn addresses_are_looked_up_by_their_lowercase_domain() {
        assert_eq!(
            lookup_domain(" Ada@Example.ORG ").as_deref(),
            Some("example.org")
        );
        assert_eq!(lookup_domain("ada@localhost"), None);
        assert_eq!(lookup_domain("ada"), None);
        assert_eq!(lookup_domain("ada@example.org."), None);
    }

    #[test]
    fn found_servers_become_the_form_s_servers_for_its_protocol() {
        let discovery = Discovery {
            source: Source::Provider,
            trusted: true,
            mail_host: None,
            provider_name: None,
            imap: Some(MailServer {
                username: Some("ada".into()),
                ..server("imap.example.org", 993, ConnectionSecurity::Tls)
            }),
            pop3: None,
            smtp: Some(server(
                "smtp.example.org",
                587,
                ConnectionSecurity::Starttls,
            )),
            jmap: None,
            caldav: None,
            carddav: None,
        };
        let (config, username) =
            transport("ada@example.org", MailProtocol::Imap, &discovery).unwrap();
        assert_eq!(
            (config.incoming_host.as_str(), config.incoming_port),
            ("imap.example.org", 993)
        );
        assert_eq!(
            (config.smtp_host.as_str(), config.smtp_port),
            ("smtp.example.org", 587)
        );
        assert_eq!(
            config.settings.connection.incoming_security,
            ConnectionSecurity::Tls
        );
        assert_eq!(
            config.settings.connection.smtp_security,
            ConnectionSecurity::Starttls
        );
        assert_eq!(username.as_deref(), Some("ada"));
        // Nothing for a protocol the provider doesn't offer.
        assert!(transport("ada@example.org", MailProtocol::Pop3, &discovery).is_none());
    }
}
