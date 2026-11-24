//! The server address the CalDAV and CardDAV dialogs fill in for a new
//! connection (#478), from what
//! `flectar_mail_core::accounts::autoconfig::discover_dav` finds for the
//! account's email address. It's filled in only while the field is still
//! empty, so an address the person typed is never replaced.

use super::*;
use flectar_mail_core::accounts::autoconfig::{self, DavDiscovery, DavService, DavSource, Source};
use std::sync::atomic::{AtomicU64, Ordering};

/// Counts the lookups, so one for a dialog that has since closed or moved
/// on is dropped.
static GENERATION: AtomicU64 = AtomicU64::new(0);

pub(super) fn install(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Handle,
) {
    let lookup = app.global::<DavLookup>();
    let window = app.as_weak();
    let state = Rc::clone(state);
    let runtime = runtime.clone();
    lookup.on_look_up(move |account_id, service| {
        let Some(app) = window.upgrade() else {
            return;
        };
        let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
        let notice = app.global::<DavLookup>();
        notice.set_confirmed(false);
        let service = match service.as_str() {
            "calendar" => DavService::Calendar,
            "contacts" => DavService::Contacts,
            _ => {
                notice.set_state("".into());
                return;
            }
        };
        // Google and Microsoft accounts reach their own calendars and
        // contacts through their sign-in, so their dialogs are for other
        // servers, which the address doesn't point to.
        let email = state
            .borrow()
            .account_configs
            .iter()
            .find(|config| config.id == i64::from(account_id))
            .filter(|config| config.provider == Provider::Generic)
            .map(|config| config.email.clone());
        let Some((email, domain)) =
            email.and_then(|email| Some((email.clone(), lookup_domain(&email)?)))
        else {
            notice.set_state("".into());
            return;
        };
        notice.set_service(service_name(service).into());
        notice.set_domain(domain.as_str().into());
        notice.set_state("looking".into());
        let window = app.as_weak();
        runtime.spawn(async move {
            let found = autoconfig::discover_dav(&email, service).await;
            let _ = window.upgrade_in_event_loop(move |app| {
                if GENERATION.load(Ordering::SeqCst) == generation {
                    apply(&app, account_id, &email, service, found.as_ref());
                }
            });
        });
    });

    let window = app.as_weak();
    lookup.on_stop(move || {
        GENERATION.fetch_add(1, Ordering::SeqCst);
        if let Some(app) = window.upgrade() {
            app.global::<DavLookup>().set_state("".into());
        }
    });
}

fn service_name(service: DavService) -> &'static str {
    match service {
        DavService::Calendar => "calendar",
        DavService::Contacts => "contacts",
    }
}

/// The address's domain: lowercase, with at least one dot.
fn lookup_domain(email: &str) -> Option<String> {
    let domain = mail_domain(email)?.to_lowercase();
    domain.contains('.').then_some(domain)
}

/// Fills in what was found, when the dialog that asked is still open on a
/// new connection for `account_id` and its address is still empty.
fn apply(
    app: &AppWindow,
    account_id: i32,
    email: &str,
    service: DavService,
    found: Option<&DavDiscovery>,
) {
    let notice = app.global::<DavLookup>();
    let (open, url, username) = match service {
        DavService::Calendar => (
            app.get_caldav_account_id() == account_id && app.get_caldav_connection_id() <= 0,
            app.get_caldav_url(),
            app.get_caldav_username(),
        ),
        DavService::Contacts => (
            app.get_carddav_account_id() == account_id && !app.get_carddav_manage_existing(),
            app.get_carddav_url(),
            app.get_carddav_username(),
        ),
    };
    if !open || !url.trim().is_empty() {
        notice.set_state("".into());
        return;
    }
    let Some(found) = found else {
        notice.set_state("none".into());
        return;
    };
    // The file's sign-in name replaces the dialog's default, the address.
    let username = found
        .username
        .as_deref()
        .filter(|_| username.trim().eq_ignore_ascii_case(email.trim()));
    match service {
        DavService::Calendar => {
            app.set_caldav_url(found.url.as_str().into());
            if let Some(username) = username {
                app.set_caldav_username(username.into());
            }
        }
        DavService::Contacts => {
            app.set_carddav_url(found.url.as_str().into());
            if let Some(username) = username {
                app.set_carddav_username(username.into());
            }
        }
    }
    notice.set_url(found.url.as_str().into());
    notice.set_source(source_name(found.source).into());
    notice.set_host(found.host.as_deref().unwrap_or_default().into());
    notice.set_trusted(found.trusted);
    notice.set_confirmed(false);
    notice.set_state("found".into());
}

/// `DavLookup.source`'s name for where the address came from.
fn source_name(source: DavSource) -> &'static str {
    match source {
        DavSource::Config(Source::Provider) => "provider",
        DavSource::Config(Source::Database) => "database",
        DavSource::Config(Source::MailHost) => "mail-host",
        DavSource::Dns => "dns",
        DavSource::WellKnown => "website",
    }
}

#[cfg(test)]
mod tests {
    use super::{lookup_domain, source_name};
    use flectar_mail_core::accounts::autoconfig::{DavSource, Source};

    #[test]
    fn every_source_has_a_notice() {
        let names = [
            DavSource::Config(Source::Provider),
            DavSource::Config(Source::Database),
            DavSource::Config(Source::MailHost),
            DavSource::Dns,
            DavSource::WellKnown,
        ]
        .map(source_name);
        assert_eq!(
            names,
            ["provider", "database", "mail-host", "dns", "website"]
        );
        assert_eq!(
            lookup_domain("Ada@Example.ORG").as_deref(),
            Some("example.org")
        );
        assert_eq!(lookup_domain("ada@localhost"), None);
    }
}
