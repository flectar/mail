//! Reads a mail autoconfig file, the `clientConfig` XML that Thunderbird's
//! autoconfig defined (version 1.1) and the IETF mailmaint autoconfig draft
//! extends (version 1.2). Elements and server types this app doesn't use are
//! skipped, as the draft requires.

use quick_xml::{Reader, events::Event};

use super::{Config, DavServer, MailServer};
use crate::models::ConnectionSecurity;

/// Autoconfig files are a few kilobytes. Anything much larger isn't one.
pub(super) const MAX_BYTES: usize = 256 * 1024;
const MAX_DEPTH: usize = 16;
const MAX_ELEMENTS: usize = 4_096;

#[derive(Debug, Default)]
struct Element {
    name: String,
    attributes: Vec<(String, String)>,
    text: String,
    children: Vec<Element>,
}

impl Element {
    fn attribute(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    fn children<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Element> + 'a {
        self.children
            .iter()
            .filter(move |child| child.name.eq_ignore_ascii_case(name))
    }

    fn child_text<'a>(&'a self, name: &'a str) -> Option<&'a str> {
        self.children(name)
            .map(|child| child.text.trim())
            .find(|text| !text.is_empty())
    }
}

/// The email address a file is read for, which fills its placeholders.
pub(super) struct Placeholders<'a> {
    pub address: &'a str,
    pub local_part: &'a str,
    pub domain: &'a str,
}

impl Placeholders<'_> {
    fn fill(&self, value: &str) -> String {
        value
            .replace("%EMAILADDRESS%", self.address)
            .replace("%EMAILLOCALPART%", self.local_part)
            .replace("%EMAILDOMAIN%", self.domain)
    }
}

/// The servers a file lists that this app can use, in the file's order of
/// preference. `None` when the text isn't a `clientConfig` file.
pub(super) fn parse(xml: &str, placeholders: &Placeholders<'_>) -> Option<Config> {
    let root = tree(xml)?;
    if !root.name.eq_ignore_ascii_case("clientConfig") {
        return None;
    }
    let mut config = Config::default();
    for provider in root.children("emailProvider") {
        if config.provider_name.is_none() {
            config.provider_name = provider
                .child_text("displayName")
                .map(|name| name.chars().take(80).collect());
        }
        for server in provider.children("incomingServer") {
            let kind = server.attribute("type").unwrap_or_default();
            if kind.eq_ignore_ascii_case("imap") {
                config.imap.extend(mail_server(server, placeholders));
            } else if kind.eq_ignore_ascii_case("pop3") {
                config.pop3.extend(mail_server(server, placeholders));
            } else if kind.eq_ignore_ascii_case("jmap") {
                config.jmap.extend(url_server(server, placeholders, false));
            }
        }
        for server in provider.children("outgoingServer") {
            if server
                .attribute("type")
                .is_some_and(|kind| kind.eq_ignore_ascii_case("smtp"))
            {
                config.smtp.extend(mail_server(server, placeholders));
            }
        }
    }
    // The draft puts address books and calendars beside `emailProvider`;
    // Thunderbird's 1.1 format spells them `addressBook` and `calendar`.
    for book in root.children("addressbook") {
        if book
            .attribute("type")
            .is_some_and(|kind| kind.eq_ignore_ascii_case("carddav"))
        {
            config.carddav.extend(url_server(book, placeholders, true));
        }
    }
    for calendar in root.children("calendar") {
        if calendar
            .attribute("type")
            .is_some_and(|kind| kind.eq_ignore_ascii_case("caldav"))
        {
            config
                .caldav
                .extend(url_server(calendar, placeholders, true));
        }
    }
    Some(config)
}

fn mail_server(server: &Element, placeholders: &Placeholders<'_>) -> Option<MailServer> {
    if !password_sign_in(server) {
        return None;
    }
    let host = placeholders.fill(server.child_text("hostname")?);
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    if !is_host_name(&host) {
        return None;
    }
    let port = server
        .child_text("port")?
        .parse::<u16>()
        .ok()
        .filter(|port| *port > 0)?;
    // A server without encryption is never offered (RFC 8314 section 5.1).
    let security = match server.child_text("socketType")? {
        socket if socket.eq_ignore_ascii_case("SSL") => ConnectionSecurity::Tls,
        socket if socket.eq_ignore_ascii_case("STARTTLS") => ConnectionSecurity::Starttls,
        _ => return None,
    };
    let username = server
        .child_text("username")
        .map(|name| placeholders.fill(name).trim().to_owned())
        .filter(|name| !name.is_empty() && !name.contains(['\r', '\n']));
    Some(MailServer {
        host,
        port,
        security,
        username,
    })
}

fn url_server(server: &Element, placeholders: &Placeholders<'_>, dav: bool) -> Option<DavServer> {
    if !password_sign_in(server) {
        return None;
    }
    let url = server
        .child_text("url")
        .or_else(|| dav.then(|| server.child_text("serverURL")).flatten())?;
    let url = url::Url::parse(placeholders.fill(url).trim()).ok()?;
    if url.scheme() != "https" || url.host_str().is_none_or(|host| !is_host_name(host)) {
        return None;
    }
    if !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    let username = server
        .child_text("username")
        .map(|name| placeholders.fill(name).trim().to_owned())
        .filter(|name| !name.is_empty() && !name.contains(['\r', '\n']));
    Some(DavServer {
        url: url.to_string(),
        username,
    })
}

/// Whether the server can take a password, the only sign-in this form
/// offers. Providers that list only OAuth2, such as Fastmail and Yahoo, take
/// app passwords on the same servers, so OAuth2 counts. A server listing no
/// method takes the protocol's own password sign-in.
fn password_sign_in(server: &Element) -> bool {
    let mut methods = server
        .children("authentication")
        .map(|method| method.text.trim())
        .peekable();
    methods.peek().is_none()
        || methods.any(|method| {
            [
                "password-cleartext",
                "password-encrypted",
                "plain",
                "secure",
                "Basic",
                "OAuth2",
            ]
            .iter()
            .any(|known| method.eq_ignore_ascii_case(known))
        })
}

/// A DNS host name, the only kind a file may name: no address literals,
/// ports, paths or spaces.
fn is_host_name(host: &str) -> bool {
    host.len() <= 253
        && host.contains('.')
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || !c.is_ascii())
        })
        && host
            .rsplit('.')
            .next()
            .is_some_and(|tld| !tld.chars().all(|c| c.is_ascii_digit()))
}

fn local_name(name: &str) -> String {
    name.rsplit(':').next().unwrap_or_default().to_owned()
}

/// The document as a tree, within the size limits above.
fn tree(xml: &str) -> Option<Element> {
    if xml.len() > MAX_BYTES {
        return None;
    }
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut stack: Vec<Element> = vec![Element::default()];
    let mut elements = 0;
    let mut buf = Vec::new();
    loop {
        let event = reader.read_event_into(&mut buf).ok()?;
        let empty = matches!(event, Event::Empty(_));
        match event {
            Event::Start(start) | Event::Empty(start) => {
                elements += 1;
                if elements > MAX_ELEMENTS || stack.len() > MAX_DEPTH {
                    return None;
                }
                let mut element = Element {
                    name: local_name(start.name().as_ref()),
                    ..Element::default()
                };
                for attribute in start.attributes().flatten() {
                    let value = attribute
                        .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                        .ok()?
                        .into_owned();
                    element
                        .attributes
                        .push((local_name(attribute.key.as_ref()), value));
                }
                if empty {
                    stack.last_mut()?.children.push(element);
                } else {
                    stack.push(element);
                }
            }
            Event::End(_) => {
                let element = stack.pop()?;
                stack.last_mut()?.children.push(element);
            }
            Event::Text(text) => stack.last_mut()?.text.push_str(&text.xml10_content()),
            Event::CData(text) => stack.last_mut()?.text.push_str(&text.into_inner()),
            Event::GeneralRef(reference) => {
                let text = &mut stack.last_mut()?.text;
                if let Some(ch) = reference.resolve_char_ref().ok()? {
                    text.push(ch);
                } else {
                    text.push_str(quick_xml::escape::resolve_predefined_entity(
                        reference.as_ref(),
                    )?);
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    if stack.len() != 1 {
        return None;
    }
    stack.pop()?.children.into_iter().next()
}
