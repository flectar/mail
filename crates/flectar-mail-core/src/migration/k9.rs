//! Reader for Thunderbird for Android and K-9 Mail settings exports (`.k9s`).
//!
//! The export is XML written by `SettingsExporter` in
//! `thunderbird/thunderbird-android`. Passwords are only present when the
//! user opted in; they are never read. Document types are refused, so
//! external entities cannot be resolved.

use super::SourceError;
use super::model::{
    AccountCandidate, IdentityCandidate, ImportInventory, ServerCandidate, ServerProtocol,
    SignatureCandidate, SourceAuth, SourceKind, SourceRetention, SourceSecurity,
};
use quick_xml::Reader;
use quick_xml::events::Event;

const MAX_ELEMENTS: usize = 200_000;
const MAX_DEPTH: usize = 32;
const MAX_ACCOUNTS: usize = 100;
/// The format uses at most three attributes per element.
const MAX_ATTRIBUTES: usize = 16;
/// The attributes this reader uses; others are skipped without being stored.
const KNOWN_ATTRIBUTES: [&str; 5] = ["format", "version", "uuid", "key", "type"];
/// Content version from which an empty user name no longer implies "no
/// authentication" (`ServerSettingsUpgraderTo95`).
const EXPLICIT_AUTH_VERSION: u32 = 95;
/// The only container format K-9 has ever written.
const FILE_FORMAT: &str = "1";
/// Content version that made connection security mandatory; older exports
/// default to TLS (`ServerSettingsUpgraderTo92`).
const SECURITY_REQUIRED_VERSION: u32 = 92;
/// Content version from which `signatureUse` defaults to false.
const SIGNATURE_OPT_IN_VERSION: u32 = 68;

#[derive(Debug, Default)]
struct Element {
    name: String,
    attributes: Vec<(&'static str, String)>,
    text: String,
    children: Vec<Element>,
}

impl Element {
    fn attribute(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value.as_str())
    }

    fn child(&self, name: &str) -> Option<&Element> {
        self.children.iter().find(|child| child.name == name)
    }

    fn children<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Element> {
        self.children.iter().filter(move |child| child.name == name)
    }

    fn child_text(&self, name: &str) -> Option<&str> {
        self.child(name).map(|child| child.text.as_str())
    }

    /// `<value key="...">text</value>` entries of a settings container.
    fn value(&self, key: &str) -> Option<&str> {
        self.children("value")
            .find(|value| value.attribute("key") == Some(key))
            .map(|value| value.text.as_str())
    }
}

fn malformed(reason: impl Into<String>) -> SourceError {
    SourceError::Malformed {
        format: "Thunderbird for Android",
        reason: reason.into(),
    }
}

fn parse_tree(bytes: &[u8]) -> Result<Element, SourceError> {
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(false);
    let mut stack: Vec<Element> = Vec::new();
    let mut root = None;
    let mut count = 0usize;
    let mut buffer = Vec::new();
    loop {
        let event = reader
            .read_event_into(&mut buffer)
            .map_err(|error| malformed(error.to_string()))?;
        let empty = matches!(event, Event::Empty(_));
        match event {
            Event::Start(start) | Event::Empty(start) => {
                count += 1;
                if count > MAX_ELEMENTS || stack.len() >= MAX_DEPTH {
                    return Err(malformed("the document exceeds safety limits"));
                }
                let mut element = Element {
                    name: start.local_name().as_ref().to_owned(),
                    ..Element::default()
                };
                for (index, attribute) in start.attributes().enumerate() {
                    if index == MAX_ATTRIBUTES {
                        return Err(malformed("the document exceeds safety limits"));
                    }
                    let attribute = attribute.map_err(|error| malformed(error.to_string()))?;
                    let Some(name) = KNOWN_ATTRIBUTES
                        .into_iter()
                        .find(|name| *name == attribute.key.local_name().as_ref())
                    else {
                        continue;
                    };
                    let value = attribute
                        .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                        .map_err(|error| malformed(error.to_string()))?;
                    element.attributes.push((name, value.into_owned()));
                }
                if empty {
                    attach(&mut stack, &mut root, element)?;
                } else {
                    stack.push(element);
                }
            }
            Event::End(_) => {
                let element = stack
                    .pop()
                    .ok_or_else(|| malformed("unbalanced elements"))?;
                attach(&mut stack, &mut root, element)?;
            }
            Event::Text(text) => {
                if let Some(element) = stack.last_mut() {
                    element
                        .text
                        .push_str(&text.xml_content(quick_xml::XmlVersion::Implicit1_0));
                }
            }
            Event::CData(text) => {
                if let Some(element) = stack.last_mut() {
                    element
                        .text
                        .push_str(&text.xml_content(quick_xml::XmlVersion::Implicit1_0));
                }
            }
            Event::GeneralRef(reference) => {
                let escaped = format!("&{};", reference.as_ref());
                let value = quick_xml::escape::unescape(&escaped)
                    .map_err(|_| malformed("unknown XML entity"))?;
                if let Some(element) = stack.last_mut() {
                    element.text.push_str(&value);
                }
            }
            Event::DocType(_) => return Err(malformed("document types are not allowed")),
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    if !stack.is_empty() {
        return Err(malformed("the document is incomplete"));
    }
    root.ok_or_else(|| malformed("the document is empty"))
}

fn attach(
    stack: &mut [Element],
    root: &mut Option<Element>,
    element: Element,
) -> Result<(), SourceError> {
    if let Some(parent) = stack.last_mut() {
        parent.children.push(element);
        Ok(())
    } else if root.replace(element).is_some() {
        Err(malformed("the document has more than one root"))
    } else {
        Ok(())
    }
}

pub fn parse(bytes: &[u8]) -> Result<ImportInventory, SourceError> {
    let root = parse_tree(bytes)?;
    if root.name != "k9settings" {
        return Err(SourceError::UnknownFormat);
    }
    let format = root.attribute("format").unwrap_or_default();
    if format != FILE_FORMAT {
        return Err(SourceError::UnsupportedVersion(format.to_owned()));
    }
    let version = root.attribute("version").map(str::to_owned);
    // The content version selects legacy defaults. Current exports declare
    // it; treat an unreadable one as current so nothing is assumed secure.
    let content_version = version
        .as_deref()
        .and_then(|version| version.trim().parse::<u32>().ok())
        .unwrap_or(u32::MAX);
    let accounts = root
        .child("accounts")
        .map(|accounts| accounts.children("account").collect::<Vec<_>>())
        .unwrap_or_default();
    if accounts.len() > MAX_ACCOUNTS {
        return Err(malformed(format!(
            "the export contains more than {MAX_ACCOUNTS} accounts"
        )));
    }
    Ok(ImportInventory {
        source: SourceKind::K9Settings,
        source_version: version,
        accounts: accounts
            .into_iter()
            .map(|element| account(element, content_version))
            .collect(),
        // K-9 keeps its mail in its own database, which exports leave out.
        mail: Vec::new(),
        other_entries: Vec::new(),
    })
}

fn account(element: &Element, version: u32) -> AccountCandidate {
    let settings = element.child("settings");
    let identities = element
        .child("identities")
        .map(|identities| {
            identities
                .children("identity")
                .map(|identity| identity_candidate(identity, version))
                .collect()
        })
        .unwrap_or_default();
    let incoming = element
        .child("incoming-server")
        .map(|server| server_candidate(server, version));
    // POP3 accounts keep mail on the server, or remove it once it is
    // deleted in the app. An absent policy is the default, NEVER.
    let retention = incoming
        .as_ref()
        .filter(|server| server.protocol == ServerProtocol::Pop3)
        .map(|_| SourceRetention {
            keep: true,
            remove_after_days: None,
            remove_deleted: settings.and_then(|settings| settings.value("deletePolicy"))
                == Some("DELETE"),
        });
    AccountCandidate {
        source_id: element.attribute("uuid").unwrap_or_default().to_owned(),
        label: element.child_text("name").map(str::to_owned),
        identities,
        incoming,
        outgoing: element
            .child("outgoing-server")
            .map(|server| server_candidate(server, version)),
        color: settings
            .and_then(|settings| settings.value("chipColor"))
            .and_then(color),
        retention,
    }
}

fn identity_candidate(element: &Element, version: u32) -> IdentityCandidate {
    let settings = element.child("settings");
    let setting = |key| settings.and_then(|settings| settings.value(key));
    // K-9 keeps the text even while the signature is switched off.
    let used = match setting("signatureUse") {
        Some(value) => value == "true",
        None => version < SIGNATURE_OPT_IN_VERSION,
    };
    let signature = setting("signature")
        .filter(|text| used && !text.trim().is_empty())
        .map(|text| {
            if setting("signatureIsHtml") == Some("true") {
                SignatureCandidate::Html(text.to_owned())
            } else {
                SignatureCandidate::Text(text.to_owned())
            }
        });
    IdentityCandidate {
        email: element.child_text("email").unwrap_or_default().to_owned(),
        display_name: element.child_text("name").map(str::to_owned),
        reply_to: setting("replyTo")
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned),
        signature,
        separate_outgoing_server: false,
    }
}

fn server_candidate(element: &Element, version: u32) -> ServerCandidate {
    let text = |name| element.child_text(name).unwrap_or_default().trim();
    let username = element.child_text("username").unwrap_or_default();
    let security = match text("connection-security") {
        "SSL_TLS_REQUIRED" => SourceSecurity::Tls,
        "STARTTLS_REQUIRED" => SourceSecurity::Starttls,
        "NONE" => SourceSecurity::None,
        // Upgrader 92 maps the optional modes to required ones and any
        // other old value to TLS.
        "SSL_TLS_OPTIONAL" => SourceSecurity::Tls,
        "STARTTLS_OPTIONAL" => SourceSecurity::Starttls,
        _ if version < SECURITY_REQUIRED_VERSION => SourceSecurity::Tls,
        _ => SourceSecurity::Unknown,
    };
    let auth = match text("authentication-type") {
        // Upgrader 95: in older exports no user name means no authentication.
        _ if username.trim().is_empty() && version < EXPLICIT_AUTH_VERSION => SourceAuth::None,
        "PLAIN" | "LOGIN" => SourceAuth::Password,
        "CRAM_MD5" => SourceAuth::EncryptedPassword,
        "AUTOMATIC" => SourceAuth::Automatic,
        "XOAUTH2" => SourceAuth::OAuth2,
        "EXTERNAL" => SourceAuth::ClientCertificate,
        "NONE" => SourceAuth::None,
        _ => SourceAuth::Unknown,
    };
    ServerCandidate {
        protocol: match element.attribute("type").unwrap_or_default() {
            protocol if protocol.eq_ignore_ascii_case("imap") => ServerProtocol::Imap,
            protocol if protocol.eq_ignore_ascii_case("pop3") => ServerProtocol::Pop3,
            protocol if protocol.eq_ignore_ascii_case("smtp") => ServerProtocol::Smtp,
            _ => ServerProtocol::Other,
        },
        host: text("host").to_owned(),
        port: text("port").parse().ok().filter(|port| *port != 0),
        security,
        auth,
        username: username.to_owned(),
    }
}

/// `chipColor` is `#rrggbb` in current exports. Older ones stored a signed
/// 32-bit Android ARGB integer.
fn color(value: &str) -> Option<String> {
    let value = value.trim();
    if let Some(hex) = value.strip_prefix('#') {
        return (hex.len() == 6 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .then(|| format!("#{}", hex.to_ascii_uppercase()));
    }
    let argb = value.parse::<i64>().ok()?;
    Some(format!("#{:06X}", argb & 0xFF_FF_FF))
}
