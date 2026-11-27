//! Namespace-insensitive parsing and request bodies for CardDAV multistatus XML.

use crate::error::Result;
use quick_xml::{Reader, events::Event};

const MAX_ITEMS: usize = 50_000;
const MAX_DEPTH: usize = 128;

#[derive(Debug, Clone, Default)]
pub struct Item {
    pub href: String,
    pub status: u16,
    pub etag: Option<String>,
    pub address_data: Option<String>,
    pub display_name: Option<String>,
    pub principal: Option<String>,
    pub home_set: Option<String>,
    pub is_addressbook: bool,
    /// `None` when the server omitted privilege information.
    pub can_write: Option<bool>,
    pub ctag: Option<String>,
    pub sync_token: Option<String>,
    /// Where new members go by POST (RFC 5995), when the server says.
    pub add_member: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Multistatus {
    pub items: Vec<Item>,
    pub sync_token: Option<String>,
}

fn local(value: &str) -> std::borrow::Cow<'_, str> {
    let name = value.rsplit(':').next().unwrap_or("");
    if name.bytes().any(|byte| byte.is_ascii_uppercase()) {
        std::borrow::Cow::Owned(name.to_ascii_lowercase())
    } else {
        std::borrow::Cow::Borrowed(name)
    }
}
fn status(value: &str) -> u16 {
    value
        .split_whitespace()
        .nth(1)
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

fn merge_props(item: &mut Item, props: Item) {
    if props.etag.is_some() {
        item.etag = props.etag;
    }
    if props.address_data.is_some() {
        item.address_data = props.address_data;
    }
    if props.display_name.is_some() {
        item.display_name = props.display_name;
    }
    if props.principal.is_some() {
        item.principal = props.principal;
    }
    if props.home_set.is_some() {
        item.home_set = props.home_set;
    }
    item.is_addressbook |= props.is_addressbook;
    if props.can_write.is_some() {
        item.can_write = props.can_write;
    }
    if props.ctag.is_some() {
        item.ctag = props.ctag;
    }
    if props.sync_token.is_some() {
        item.sync_token = props.sync_token;
    }
    if props.add_member.is_some() {
        item.add_member = props.add_member;
    }
}

pub fn parse(body: &str) -> Result<Multistatus> {
    let mut reader = Reader::from_str(body);
    reader.config_mut().trim_text(false);
    let mut out = Multistatus::default();
    let mut current: Option<Item> = None;
    let mut props: Option<Item> = None;
    let mut path = Vec::<String>::new();
    let mut text = String::new();
    let mut href_target: Option<&str> = None;
    let mut prop_status = None;
    let mut direct_status = None;
    let mut successful_propstat = false;
    let mut first_prop_error = None;
    let mut buf = Vec::new();
    loop {
        match reader
            .read_event_into(&mut buf)
            .map_err(|e| super::err(format!("XML: {e}")))?
        {
            Event::Start(e) => {
                if path.len() >= MAX_DEPTH {
                    return Err(super::err("XML nesting limit exceeded"));
                }
                let qualified_name = e.name();
                let name = local(qualified_name.as_ref());
                match name.as_ref() {
                    "response" => {
                        current = Some(Item::default());
                        direct_status = None;
                        successful_propstat = false;
                        first_prop_error = None;
                    }
                    "propstat" => {
                        props = Some(Item::default());
                        prop_status = None;
                    }
                    "current-user-principal" => href_target = Some("principal"),
                    "addressbook-home-set" => href_target = Some("home"),
                    "add-member" => href_target = Some("add-member"),
                    "current-user-privilege-set" => {
                        if let Some(item) = props.as_mut().or(current.as_mut()) {
                            item.can_write = Some(false);
                        }
                    }
                    "write" | "write-content" | "all"
                        if path
                            .iter()
                            .any(|value| value == "current-user-privilege-set") =>
                    {
                        if let Some(item) = props.as_mut().or(current.as_mut()) {
                            item.can_write = Some(true);
                        }
                    }
                    "addressbook" if path.last().is_some_and(|v| v == "resourcetype") => {
                        if let Some(item) = props.as_mut().or(current.as_mut()) {
                            item.is_addressbook = true;
                        }
                    }
                    _ => {}
                }
                path.push(name.into_owned());
                text.clear();
            }
            Event::Empty(e) => {
                let qualified_name = e.name();
                let name = local(qualified_name.as_ref());
                if name == "addressbook"
                    && path.last().is_some_and(|v| v == "resourcetype")
                    && let Some(item) = props.as_mut().or(current.as_mut())
                {
                    item.is_addressbook = true;
                }
                if matches!(name.as_ref(), "write" | "write-content" | "all")
                    && path
                        .iter()
                        .any(|value| value == "current-user-privilege-set")
                    && let Some(item) = props.as_mut().or(current.as_mut())
                {
                    item.can_write = Some(true);
                }
            }
            Event::Text(value) => text.push_str(&value.xml10_content()),
            Event::CData(value) => text.push_str(&value.into_inner()),
            Event::GeneralRef(value) => {
                if let Some(ch) = value
                    .resolve_char_ref()
                    .map_err(|e| super::err(format!("XML reference: {e}")))?
                {
                    text.push(ch);
                } else if let Some(entity) =
                    quick_xml::escape::resolve_predefined_entity(value.as_ref())
                {
                    text.push_str(entity);
                } else {
                    return Err(super::err("unknown XML entity"));
                }
            }
            Event::End(e) => {
                let qualified_name = e.name();
                let name = local(qualified_name.as_ref());
                path.pop();
                let value = text.trim();
                match name.as_ref() {
                    "response" => {
                        if let Some(mut item) = current.take() {
                            if out.items.len() >= MAX_ITEMS {
                                return Err(super::err("CardDAV item limit exceeded"));
                            }
                            item.status = direct_status.unwrap_or_else(|| {
                                if successful_propstat {
                                    200
                                } else {
                                    first_prop_error.unwrap_or(200)
                                }
                            });
                            out.items.push(item);
                        }
                    }
                    "propstat" => {
                        let code = prop_status.unwrap_or(0);
                        if (200..300).contains(&code) {
                            successful_propstat = true;
                            if let (Some(item), Some(props)) = (current.as_mut(), props.take()) {
                                merge_props(item, props);
                            }
                        } else {
                            if first_prop_error.is_none() && code != 0 {
                                first_prop_error = Some(code);
                            }
                            props = None;
                        }
                        prop_status = None;
                    }
                    "href" => {
                        let in_propstat = props.is_some();
                        if let Some(item) = props.as_mut().or(current.as_mut()) {
                            match href_target {
                                Some("principal") => item.principal = Some(value.to_owned()),
                                Some("home") => item.home_set = Some(value.to_owned()),
                                Some("add-member") => item.add_member = Some(value.to_owned()),
                                _ if !in_propstat && item.href.is_empty() => {
                                    item.href = value.to_owned()
                                }
                                _ => {}
                            }
                        }
                    }
                    "current-user-principal" | "addressbook-home-set" | "add-member" => {
                        href_target = None
                    }
                    "status" => {
                        let code = status(value);
                        if path.last().is_some_and(|v| v == "response") {
                            direct_status = Some(code);
                        } else if path.iter().any(|part| part == "propstat") {
                            prop_status = Some(code);
                        }
                    }
                    "getetag" => {
                        if let Some(item) = props.as_mut().or(current.as_mut()) {
                            item.etag = Some(value.to_owned());
                        }
                    }
                    "address-data" => {
                        if let Some(item) = props.as_mut().or(current.as_mut()) {
                            item.address_data = Some(value.to_owned());
                        }
                    }
                    "displayname" => {
                        if !value.is_empty()
                            && let Some(item) = props.as_mut().or(current.as_mut())
                        {
                            item.display_name = Some(value.to_owned());
                        }
                    }
                    "getctag" => {
                        if let Some(item) = props.as_mut().or(current.as_mut()) {
                            item.ctag = Some(value.to_owned());
                        }
                    }
                    "sync-token" => {
                        if let Some(item) = props.as_mut().or(current.as_mut()) {
                            item.sync_token = Some(value.to_owned());
                        } else {
                            out.sync_token = Some(value.to_owned());
                        }
                    }
                    _ => {}
                }
                text.clear();
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}

fn escaped_len(value: &str) -> usize {
    value
        .chars()
        .map(|character| match character {
            '&' => 5,
            '<' | '>' => 4,
            '"' => 6,
            _ => character.len_utf8(),
        })
        .sum()
}

fn append_escaped(output: &mut String, value: &str) {
    for character in value.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '"' => output.push_str("&quot;"),
            _ => output.push(character),
        }
    }
}

pub fn principal() -> String {
    r#"<?xml version="1.0"?><d:propfind xmlns:d="DAV:"><d:prop><d:current-user-principal/></d:prop></d:propfind>"#.into()
}
pub fn home_set() -> String {
    r#"<?xml version="1.0"?><d:propfind xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:carddav"><d:prop><c:addressbook-home-set/></d:prop></d:propfind>"#.into()
}
pub fn collections() -> String {
    r#"<?xml version="1.0"?><d:propfind xmlns:d="DAV:" xmlns:cs="http://calendarserver.org/ns/"><d:prop><d:resourcetype/><d:displayname/><cs:getctag/><d:sync-token/><d:current-user-privilege-set/></d:prop></d:propfind>"#.into()
}
pub fn state() -> String {
    r#"<?xml version="1.0"?><d:propfind xmlns:d="DAV:" xmlns:cs="http://calendarserver.org/ns/"><d:prop><cs:getctag/><d:sync-token/></d:prop></d:propfind>"#.into()
}
/// The ETag of every member, for a PROPFIND with Depth 1.
pub fn etags() -> String {
    r#"<?xml version="1.0"?><d:propfind xmlns:d="DAV:"><d:prop><d:getetag/></d:prop></d:propfind>"#
        .into()
}
/// Where the collection takes new members by POST (RFC 5995).
pub fn add_member() -> String {
    r#"<?xml version="1.0"?><d:propfind xmlns:d="DAV:"><d:prop><d:add-member/></d:prop></d:propfind>"#.into()
}
pub fn sync_collection(token: &str) -> String {
    const PREFIX: &str = r#"<?xml version="1.0"?><d:sync-collection xmlns:d="DAV:"><d:sync-token>"#;
    const SUFFIX: &str = r#"</d:sync-token><d:sync-level>1</d:sync-level><d:prop><d:getetag/></d:prop></d:sync-collection>"#;
    let mut output = String::with_capacity(PREFIX.len() + escaped_len(token) + SUFFIX.len());
    output.push_str(PREFIX);
    append_escaped(&mut output, token);
    output.push_str(SUFFIX);
    output
}
pub fn addressbook_query() -> String {
    // FN is required by both vCard 3.0 and 4.0. Querying it lists every valid
    // address object, including contacts without an email address that the
    // app retains as unprojected DAV objects.
    r#"<?xml version="1.0"?><c:addressbook-query xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:carddav"><d:prop><d:getetag/></d:prop><c:filter><c:prop-filter name="FN"/></c:filter></c:addressbook-query>"#.into()
}
pub fn multiget(hrefs: &[String]) -> String {
    const PREFIX: &str = r#"<?xml version="1.0"?><c:addressbook-multiget xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:carddav"><d:prop><d:getetag/><c:address-data/></d:prop>"#;
    const SUFFIX: &str = r#"</c:addressbook-multiget>"#;
    let capacity = PREFIX.len()
        + SUFFIX.len()
        + hrefs
            .iter()
            .map(|href| "<d:href></d:href>".len() + escaped_len(href))
            .sum::<usize>();
    let mut output = String::with_capacity(capacity);
    output.push_str(PREFIX);
    for href in hrefs {
        output.push_str("<d:href>");
        append_escaped(&mut output, href);
        output.push_str("</d:href>");
    }
    output.push_str(SUFFIX);
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignores_properties_from_failed_propstats() {
        let response = r#"<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:carddav">
          <d:response><d:href>/book/</d:href>
            <d:propstat><d:prop><d:getetag>bad</d:getetag><d:current-user-privilege-set/></d:prop><d:status>HTTP/1.1 404 Not Found</d:status></d:propstat>
            <d:propstat><d:prop><d:resourcetype><d:collection/><c:addressbook/></d:resourcetype><d:getetag>good</d:getetag></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>
          </d:response></d:multistatus>"#;
        let item = parse(response).unwrap().items.remove(0);
        assert_eq!(item.status, 200);
        assert_eq!(item.etag.as_deref(), Some("good"));
        assert_eq!(item.can_write, None);
        assert!(item.is_addressbook);
    }

    #[test]
    fn requests_server_native_vcard_format() {
        let body = multiget(&["/book/a.vcf".into()]);
        assert!(body.contains("<c:address-data/>"));
        assert!(!body.contains("version=\"4.0\""));
    }

    #[test]
    fn reads_where_a_collection_takes_new_members() {
        let response = r#"<d:multistatus xmlns:d="DAV:"><d:response><d:href>/book/</d:href>
            <d:propstat><d:prop><d:add-member><d:href>/book/?add-member</d:href></d:add-member></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>
          </d:response></d:multistatus>"#;
        let item = parse(response).unwrap().items.remove(0);
        assert_eq!(item.href, "/book/");
        assert_eq!(item.add_member.as_deref(), Some("/book/?add-member"));
    }
}
