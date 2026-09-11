//! WebDAV (RFC 4918), ACL (RFC 3744), quota (RFC 4331), sync (RFC 6578).
//! Failed propstats never become successful metadata or permissions.
use super::{
    FileNode, MAX_TRANSFER, Rights, err,
    transport::{Transport, validated_url},
    validate_name,
};
use crate::error::Result;
use quick_xml::{NsReader, events::Event, name::ResolveResult};
use std::{collections::HashMap, path::Path};
use url::Url;
const DAV: &str = "DAV:";

#[derive(Clone, Debug, Default)]
pub struct Property {
    pub namespace: String,
    pub name: String,
    pub text: String,
    pub children: Vec<Property>,
}
impl Property {
    fn is(&self, name: &str) -> bool {
        self.namespace == DAV && self.name == name
    }
    fn child(&self, name: &str) -> Option<&Self> {
        self.children.iter().find(|p| p.is(name))
    }
    fn value(&self, name: &str) -> Option<&str> {
        self.child(name).map(|p| p.text.trim())
    }
    fn contains(&self, name: &str) -> bool {
        self.is(name) || self.children.iter().any(|p| p.contains(name))
    }
    pub fn describe(&self) -> String {
        fn visit(p: &Property, depth: usize, out: &mut String) {
            if !p.text.trim().is_empty() {
                out.push_str(&format!(
                    "{}{}: {}\n",
                    "  ".repeat(depth),
                    p.name,
                    p.text.trim()
                ));
            }
            for child in &p.children {
                visit(child, depth + 1, out)
            }
        }
        let mut out = String::new();
        visit(self, 0, &mut out);
        out
    }
    fn xml(&self) -> String {
        format!(
            "<{} xmlns=\"{}\">{}{}</{}>",
            self.name,
            escape(&self.namespace),
            escape(&self.text),
            self.children.iter().map(Self::xml).collect::<String>(),
            self.name
        )
    }
}
pub struct Listing {
    pub nodes: Vec<FileNode>,
    pub current: FileNode,
    pub quota_available: Option<u64>,
    pub quota_used: Option<u64>,
    pub sync_token: Option<String>,
}
pub struct DavClient {
    transport: Transport,
    pub root: String,
    pub supports_locks: bool,
    pub supports_acl: bool,
    locks: HashMap<String, String>,
}
impl DavClient {
    pub async fn connect(endpoint: &str, user: &str, secret: &str) -> Result<Self> {
        let mut url = canonical_url(validated_url(endpoint)?)?;
        if url.query().is_some() {
            return Err(err("WebDAV storage URLs cannot contain a query."));
        }
        if !url.path().ends_with('/') {
            url.set_path(&format!("{}/", url.path()));
        }
        let transport = Transport::new(url.as_str(), user, secret)?;
        let response = transport
            .send(transport.request("OPTIONS", url.as_str())?)
            .await?;
        let dav = response
            .headers()
            .get("DAV")
            .and_then(|h| h.to_str().ok())
            .unwrap_or("")
            .to_owned();
        Ok(Self {
            transport,
            root: url.into(),
            supports_locks: dav.split(',').any(|s| s.trim() == "2"),
            supports_acl: dav.split(',').any(|s| s.trim() == "access-control"),
            locks: HashMap::new(),
        })
    }
    pub fn href(&self, base: &str, value: &str) -> Result<String> {
        let base = validated_url(base)?;
        let root = validated_url(&self.root)?;
        let url = canonical_url(base.join(value).map_err(|_| err("Invalid WebDAV href."))?)?;
        if url.origin() != root.origin()
            || url.query().is_some()
            || url.fragment().is_some()
            || !(url.path().starts_with(root.path())
                || url.path() == root.path().trim_end_matches('/'))
        {
            return Err(err(
                "WebDAV returned a resource outside the configured storage root.",
            ));
        }
        Ok(url.into())
    }
    pub fn child_url(&self, parent: &str, name: &str, directory: bool) -> Result<String> {
        validate_name(name)?;
        let parent = self.href(&self.root, parent)?;
        let mut url = Url::parse(&parent).map_err(|_| err("Invalid folder."))?;
        {
            let mut path = url
                .path_segments_mut()
                .map_err(|_| err("Invalid folder path."))?;
            path.pop_if_empty().push(name);
            if directory {
                path.push("");
            }
        }
        self.href(&self.root, url.as_str())
    }
    async fn xml(
        &self,
        method: &str,
        url: &str,
        depth: &str,
        body: String,
        headers: &[(&str, String)],
    ) -> Result<Property> {
        let url = self.href(&self.root, url)?;
        let mut request = self
            .transport
            .request(method, &url)?
            .header("Depth", depth)
            .header("Content-Type", "application/xml; charset=utf-8")
            .body(body);
        for (key, value) in headers {
            request = request.header(*key, value);
        }
        let response = self.transport.send(request).await?;
        let body = crate::http_body::text(response, 16 * 1024 * 1024, "WebDAV metadata").await?;
        parse(&body)
    }
    pub async fn properties(&self, url: &str) -> Result<Property> {
        self.xml("PROPFIND",url,"0","<d:propfind xmlns:d=\"DAV:\"><d:allprop/><d:include><d:acl/><d:current-user-privilege-set/><d:lockdiscovery/><d:quota-available-bytes/><d:quota-used-bytes/><d:sync-token/></d:include></d:propfind>".into(),&[]).await
    }
    pub async fn list(&self, url: &str) -> Result<Listing> {
        let url = self.href(&self.root, url)?;
        let tree=self.xml("PROPFIND",&url,"1","<d:propfind xmlns:d=\"DAV:\"><d:prop><d:displayname/><d:resourcetype/><d:getetag/><d:getcontentlength/><d:getcontenttype/><d:getlastmodified/><d:creationdate/><d:current-user-privilege-set/><d:lockdiscovery/><d:quota-available-bytes/><d:quota-used-bytes/><d:sync-token/></d:prop></d:propfind>".into(),&[]).await?;
        if !tree.is("multistatus") {
            return Err(err("Expected a WebDAV multistatus response."));
        }
        let mut listing = Listing {
            nodes: Vec::new(),
            current: FileNode::default(),
            quota_available: None,
            quota_used: None,
            sync_token: None,
        };
        let mut found = false;
        for response in tree.children.iter().filter(|p| p.is("response")) {
            let Some(href) = response.value("href") else {
                return Err(err("WebDAV response has no href."));
            };
            let href = self.href(&url, href)?;
            let props = successful_properties(response)?;
            let node = node(&href, &props);
            if href.trim_end_matches('/') == url.trim_end_matches('/') {
                listing.quota_available =
                    prop(&props, "quota-available-bytes").and_then(|v| v.text.trim().parse().ok());
                listing.quota_used =
                    prop(&props, "quota-used-bytes").and_then(|v| v.text.trim().parse().ok());
                listing.sync_token = prop(&props, "sync-token").map(|v| v.text.trim().into());
                listing.current = node;
                found = true;
            } else {
                // Stalwart assisted discovery may return grandchildren. Keep
                // navigation one level deep rather than flattening the tree.
                let prefix = format!("{}/", url.trim_end_matches('/'));
                let relative = href
                    .strip_prefix(&prefix)
                    .unwrap_or("")
                    .trim_end_matches('/');
                if !relative.is_empty() && !relative.contains('/') {
                    listing.nodes.push(node);
                }
            }
        }
        if !found {
            return Err(err("The server omitted the requested folder."));
        }
        let parent_can_remove = listing.current.my_rights.may_delete;
        for node in &mut listing.nodes {
            node.my_rights.may_delete = parent_can_remove;
            node.my_rights.may_rename = parent_can_remove && listing.current.my_rights.add();
        }
        listing.nodes.sort_by(|a, b| {
            b.is_directory()
                .cmp(&a.is_directory())
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        Ok(listing)
    }
    fn conditions(
        &self,
        node: &FileNode,
        request: reqwest::RequestBuilder,
        require_etag: bool,
    ) -> Result<reqwest::RequestBuilder> {
        let mut request = request;
        if let Some(etag) = node.etag.as_ref().filter(|v| !v.starts_with("W/")) {
            request = request.header("If-Match", etag);
        } else if require_etag {
            return Err(err(
                "The server supplied no strong ETag. Refresh; this file cannot safely be overwritten.",
            ));
        }
        // Tagged lists also submit a parent lock when creating/moving a child.
        let conditions = self
            .locks
            .iter()
            .filter(|(href, _)| {
                node.id == **href
                    || node
                        .id
                        .starts_with(&format!("{}/", href.trim_end_matches('/')))
            })
            .map(|(href, token)| format!("<{href}> (<{token}>)"))
            .collect::<Vec<_>>()
            .join(" ");
        if !conditions.is_empty() {
            request = request.header("If", conditions);
        }
        Ok(request)
    }
    async fn mutate(
        &self,
        method: &str,
        node: &FileNode,
        extra: &[(&str, String)],
        body: Option<reqwest::Body>,
        require_etag: bool,
    ) -> Result<()> {
        let url = self.href(&self.root, &node.id)?;
        let mut request =
            self.conditions(node, self.transport.request(method, &url)?, require_etag)?;
        let mut headers = reqwest::header::HeaderMap::new();
        for (key, value) in extra {
            let name = reqwest::header::HeaderName::from_bytes(key.as_bytes())
                .map_err(|_| err("Invalid mutation header."))?;
            let value = reqwest::header::HeaderValue::from_str(value)
                .map_err(|_| err("Invalid mutation header value."))?;
            headers.insert(name, value);
        }
        // Replace generated If conditions when a mutation supplies a complete
        // source/destination list. Duplicate If headers can lose preconditions.
        request = request.headers(headers);
        if let Some(body) = body {
            request = request.body(body);
        }
        let response = self.transport.send(request).await?;
        if response.status().as_u16() == 207 {
            let xml =
                crate::http_body::text(response, 16 * 1024 * 1024, "WebDAV operation").await?;
            check_multistatus(&parse(&xml)?)?;
        }
        Ok(())
    }
    pub async fn create_folder(&self, parent: &str, name: &str) -> Result<()> {
        let node = FileNode {
            id: self.child_url(parent, name, true)?,
            ..Default::default()
        };
        self.mutate(
            "MKCOL",
            &node,
            &[("If-None-Match", "*".into())],
            None,
            false,
        )
        .await
    }
    pub async fn upload(
        &self,
        parent: &str,
        name: &str,
        path: &Path,
        replace: Option<&FileNode>,
    ) -> Result<()> {
        let node = match replace {
            Some(node) => node.clone(),
            None => FileNode {
                id: self.child_url(parent, name, false)?,
                ..Default::default()
            },
        };
        let (data, length) = super::upload_body(path, MAX_TRANSFER).await?;
        let mut headers = vec![
            ("Content-Length", length.to_string()),
            ("Content-Type", crate::queue::mime_guess_from_name(name)),
        ];
        if replace.is_none() {
            headers.push(("If-None-Match", "*".into()));
        }
        self.mutate("PUT", &node, &headers, Some(data), replace.is_some())
            .await
    }
    async fn download_response(&self, node: &FileNode) -> Result<reqwest::Response> {
        if node.is_directory() {
            return Err(err("Select an individual file to download."));
        }
        let url = self.href(&self.root, &node.id)?;
        let request = self.conditions(node, self.transport.request("GET", &url)?, false)?;
        self.transport.send(request).await
    }
    pub async fn download(&self, node: &FileNode) -> Result<Vec<u8>> {
        crate::http_body::bytes(
            self.download_response(node).await?,
            MAX_TRANSFER,
            "File download",
        )
        .await
    }
    pub async fn preview(&self, node: &FileNode) -> Result<Vec<u8>> {
        crate::http_body::bytes(
            self.download_response(node).await?,
            16 * 1024 * 1024,
            "File preview",
        )
        .await
    }
    pub async fn download_to(&self, node: &FileNode, destination: &Path) -> Result<()> {
        super::save_response(destination, self.download_response(node).await?).await
    }
    pub async fn delete(&self, node: &FileNode) -> Result<()> {
        if node.id.trim_end_matches('/') == self.root.trim_end_matches('/') {
            return Err(err("The storage root cannot be deleted."));
        }
        self.mutate("DELETE", node, &[], None, !node.is_directory())
            .await
    }
    pub async fn relocate(
        &self,
        node: &FileNode,
        parent: &str,
        name: &str,
        copy: bool,
    ) -> Result<()> {
        let destination = self.child_url(parent, name, node.is_directory())?;
        if destination == node.id
            || destination.starts_with(&format!("{}/", node.id.trim_end_matches('/')))
        {
            return Err(err("A folder cannot be moved or copied into itself."));
        }
        let conditions = self
            .locks
            .iter()
            .filter(|(href, _)| {
                [node.id.as_str(), destination.as_str()].iter().any(|id| {
                    *id == href.as_str()
                        || id.starts_with(&format!("{}/", href.trim_end_matches('/')))
                })
            })
            .map(|(href, token)| format!("<{href}> (<{token}>)"))
            .collect::<Vec<_>>()
            .join(" ");
        let mut headers = vec![
            ("Destination", destination),
            ("Overwrite", "F".into()),
            ("Depth", "infinity".into()),
        ];
        if !conditions.is_empty() {
            headers.push(("If", conditions));
        }
        self.mutate(
            if copy { "COPY" } else { "MOVE" },
            node,
            &headers,
            None,
            !node.is_directory(),
        )
        .await
    }
    /// Explicit replacement carries validators for both resources. Never
    /// delete a destination first: COPY/MOVE remains one server operation.
    pub async fn relocate_with_policy(
        &self,
        node: &FileNode,
        parent: &str,
        name: &str,
        copy: bool,
        policy: super::CollisionPolicy,
    ) -> Result<()> {
        if matches!(policy, super::CollisionPolicy::Reject) {
            return self.relocate(node, parent, name, copy).await;
        }
        let listing = self.list(parent).await?;
        let Some(existing) = listing.nodes.iter().find(|n| n.name == name) else {
            return self.relocate(node, parent, name, copy).await;
        };
        if matches!(policy, super::CollisionPolicy::Rename) {
            let (stem, extension) = name
                .rsplit_once('.')
                .filter(|(stem, _)| !stem.is_empty())
                .map(|(a, b)| (a, format!(".{b}")))
                .unwrap_or((name, String::new()));
            for number in 1..=1000 {
                let candidate = format!("{stem} ({number}){extension}");
                if !listing.nodes.iter().any(|n| n.name == candidate) {
                    return self.relocate(node, parent, &candidate, copy).await;
                }
            }
            return Err(err("Could not find an unused destination name."));
        }
        if node.is_directory() || existing.is_directory() {
            return Err(err(
                "Replacing a WebDAV folder cannot be guarded by a content ETag. Choose Keep both or select a new destination.",
            ));
        }
        if existing.id == node.id {
            return Err(err("Source and destination are the same file."));
        }
        let etag = existing
            .etag
            .as_deref()
            .filter(|s| !s.starts_with("W/"))
            .ok_or_else(|| err("The destination has no strong ETag. Choose a new name."))?;
        if matches!(policy, super::CollisionPolicy::Newest) {
            let date = |n: &FileNode| {
                n.modified.as_deref().and_then(|s| {
                    chrono::DateTime::parse_from_rfc2822(s)
                        .or_else(|_| chrono::DateTime::parse_from_rfc3339(s))
                        .ok()
                })
            };
            let (Some(source), Some(target)) = (date(node), date(existing)) else {
                return Err(err("Both files need modification dates for Keep newest."));
            };
            if source <= target {
                return Err(err(
                    "The destination is newer or has the same modification date; it was kept.",
                ));
            }
        }
        let mut target_conditions = format!("[{etag}]");
        let mut others = Vec::new();
        for (href, token) in &self.locks {
            if href == &existing.id {
                target_conditions.push_str(&format!(" <{token}>"));
            } else if [node.id.as_str(), existing.id.as_str()].iter().any(|id| {
                *id == href || id.starts_with(&format!("{}/", href.trim_end_matches('/')))
            }) {
                others.push(format!("<{href}> (<{token}>)"));
            }
        }
        // ETag and lock must be in the SAME list (AND). Separate lists for a
        // resource are OR in RFC 4918 and could bypass the stale-write check.
        let conditions = format!(
            "<{}> ({target_conditions}) {}",
            existing.id,
            others.join(" ")
        );
        self.mutate(
            if copy { "COPY" } else { "MOVE" },
            node,
            &[
                ("Destination", existing.id.clone()),
                ("Overwrite", "T".into()),
                ("Depth", "infinity".into()),
                ("If", conditions),
            ],
            None,
            true,
        )
        .await
    }
    pub async fn lock(&mut self, node: &FileNode) -> Result<()> {
        self.lock_with_options(node, false, false, 300).await
    }
    pub async fn lock_with_options(
        &mut self,
        node: &FileNode,
        shared: bool,
        recursive: bool,
        seconds: u32,
    ) -> Result<()> {
        if !self.supports_locks {
            return Err(err("The server does not advertise WebDAV locking."));
        }
        let url = self.href(&self.root, &node.id)?;
        let mut request = self
            .transport
            .request("LOCK", &url)?
            .header("Timeout", format!("Second-{}", seconds.clamp(30, 3600)))
            .header("Depth", if recursive { "infinity" } else { "0" });
        if let Some(token) = self.locks.get(&url) {
            request = request.header("If", format!("(<{token}>)"));
        } else {
            request=request.header("Content-Type","application/xml").body(format!("<d:lockinfo xmlns:d=\"DAV:\"><d:lockscope><d:{}/></d:lockscope><d:locktype><d:write/></d:locktype></d:lockinfo>",if shared {"shared"} else {"exclusive"}));
        }
        let response = self.transport.send(request).await?;
        if let Some(token) = response
            .headers()
            .get("Lock-Token")
            .and_then(|h| h.to_str().ok())
        {
            self.locks.insert(
                url,
                token.trim_start_matches('<').trim_end_matches('>').into(),
            );
        } else if !self.locks.contains_key(&url) {
            return Err(err("The server omitted the lock token."));
        }
        Ok(())
    }
    pub fn owns_lock(&self, id: &str) -> bool {
        self.locks.contains_key(id)
    }
    pub async fn unlock(&mut self, node: &FileNode) -> Result<()> {
        let token = self
            .locks
            .get(&node.id)
            .ok_or_else(|| err("This session does not own the lock."))?
            .clone();
        self.mutate(
            "UNLOCK",
            node,
            &[("Lock-Token", format!("<{token}>"))],
            None,
            false,
        )
        .await?;
        self.locks.remove(&node.id);
        Ok(())
    }
    pub async fn patch_property(
        &self,
        node: &FileNode,
        namespace: &str,
        name: &str,
        value: Option<&str>,
    ) -> Result<()> {
        if name.is_empty()
            || !name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
            || namespace.len() > 1024
            || url::Url::parse(namespace).is_err()
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(err("Invalid XML property name."));
        }
        let action = if value.is_some() { "set" } else { "remove" };
        let xml = format!(
            "<d:propertyupdate xmlns:d=\"DAV:\"><d:{action}><d:prop><p:{name} xmlns:p=\"{}\">{}</p:{name}></d:prop></d:{action}></d:propertyupdate>",
            escape(namespace),
            escape(value.unwrap_or(""))
        );
        self.mutate(
            "PROPPATCH",
            node,
            &[("Content-Type", "application/xml; charset=utf-8".into())],
            Some(xml.into()),
            !node.is_directory(),
        )
        .await
    }
    /// Preserve every unrelated ACE. Never silently discard existing shares.
    pub async fn share(
        &self,
        node: &FileNode,
        principal: &str,
        write: bool,
        remove: bool,
    ) -> Result<()> {
        self.share_privileges(
            node,
            principal,
            if write { &["read", "write"] } else { &["read"] },
            remove,
        )
        .await
    }
    pub async fn share_privileges(
        &self,
        node: &FileNode,
        principal: &str,
        privileges: &[&str],
        remove: bool,
    ) -> Result<()> {
        if privileges.iter().any(|p| {
            ![
                "read",
                "write",
                "write-content",
                "write-properties",
                "bind",
                "unbind",
                "read-acl",
                "write-acl",
                "unlock",
                "read-current-user-privilege-set",
            ]
            .contains(p)
        }) {
            return Err(err("Unsupported WebDAV privilege."));
        }
        if !self.supports_acl || !node.my_rights.may_share {
            return Err(err("You cannot change sharing for this file."));
        }
        let root = validated_url(&self.root)?;
        let principal = canonical_url(
            root.join(principal)
                .map_err(|_| err("Invalid principal URL."))?,
        )?;
        if principal.origin() != root.origin()
            || !(principal.path().starts_with("/dav/pal/")
                || principal.path().starts_with("/dav/principal/"))
        {
            return Err(err("Enter a principal URL on this storage server."));
        }
        let tree = self.properties(&node.id).await?;
        let response = tree
            .children
            .iter()
            .find(|p| p.is("response"))
            .ok_or_else(|| err("Missing ACL response."))?;
        let props = successful_properties(response)?;
        let acl =
            prop(&props, "acl").ok_or_else(|| err("The server did not return the current ACL."))?;
        let mut entries = String::new();
        for ace in &acl.children {
            if !ace.is("ace") {
                continue;
            }
            let matches = ace
                .child("principal")
                .and_then(|p| p.value("href"))
                .and_then(|p| root.join(p).ok())
                .and_then(|p| canonical_url(p).ok())
                .is_some_and(|p| {
                    p.as_str().trim_end_matches('/') == principal.as_str().trim_end_matches('/')
                });
            if matches && (ace.contains("protected") || ace.contains("inherited")) {
                return Err(err("This permission is protected or inherited."));
            }
            // RFC 3744: protected/inherited ACEs must not be submitted in ACL.
            if !matches && !ace.contains("protected") && !ace.contains("inherited") {
                entries.push_str(&ace.xml());
            }
        }
        if !remove {
            let grants = privileges
                .iter()
                .map(|p| format!("<d:privilege><d:{p}/></d:privilege>"))
                .collect::<String>();
            entries.push_str(&format!("<d:ace><d:principal><d:href>{}</d:href></d:principal><d:grant>{grants}</d:grant></d:ace>",escape(principal.as_str())));
        }
        self.mutate(
            "ACL",
            node,
            &[("Content-Type", "application/xml".into())],
            Some(format!("<d:acl xmlns:d=\"DAV:\">{entries}</d:acl>").into()),
            !node.is_directory(),
        )
        .await
    }
    pub async fn sync_collection(&self, url: &str, token: &str) -> Result<Property> {
        self.xml("REPORT",url,"1",format!("<d:sync-collection xmlns:d=\"DAV:\"><d:sync-token>{}</d:sync-token><d:sync-level>1</d:sync-level><d:prop><d:getetag/></d:prop></d:sync-collection>",escape(token)),&[]).await
    }
    pub async fn collection_changed(&self, url: &str, token: &str) -> Result<(bool, String)> {
        let tree = self.sync_collection(url, token).await?;
        if !tree.is("multistatus") {
            return Err(err("Invalid collection sync response."));
        }
        let token = tree
            .value("sync-token")
            .ok_or_else(|| err("Missing collection sync token."))?
            .to_owned();
        // Reconcile changed collections with Depth 1 so permissions, locks,
        // quota and deletion semantics use the same validated listing parser.
        Ok((tree.children.iter().any(|p| p.is("response")), token))
    }
}
// Compare paths after normalizing percent-encoded segments. Servers may encode
// the account's '@' even when the configured URL does not. Encoded separators
// are rejected instead of being reinterpreted as a different hierarchy.
fn canonical_url(mut url: Url) -> Result<Url> {
    let segments = url
        .path()
        .split('/')
        .skip(1)
        .map(|part| {
            let decoded = percent_encoding::percent_decode_str(part)
                .decode_utf8()
                .map_err(|_| err("Invalid UTF-8 WebDAV path."))?
                .into_owned();
            if decoded.contains(['/', '\\']) || decoded.chars().any(char::is_control) {
                return Err(err("Invalid encoded WebDAV path segment."));
            }
            Ok(decoded)
        })
        .collect::<Result<Vec<_>>>()?;
    {
        let mut path = url
            .path_segments_mut()
            .map_err(|_| err("Invalid WebDAV path."))?;
        path.clear();
        for segment in segments {
            path.push(&segment);
        }
    }
    Ok(url)
}

fn prop<'a>(props: &'a [Property], name: &str) -> Option<&'a Property> {
    props.iter().find(|p| p.is(name))
}
fn status(value: &str) -> u16 {
    value
        .split_whitespace()
        .nth(1)
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}
fn successful_properties(response: &Property) -> Result<Vec<Property>> {
    if response
        .value("status")
        .is_some_and(|s| !(200..300).contains(&status(s)))
    {
        return Err(err("A WebDAV resource could not be loaded."));
    }
    let mut props = Vec::new();
    let mut success = false;
    for stat in response.children.iter().filter(|p| p.is("propstat")) {
        if stat
            .value("status")
            .is_some_and(|s| (200..300).contains(&status(s)))
        {
            success = true;
            if let Some(p) = stat.child("prop") {
                props.extend(p.children.clone());
            }
        }
    }
    if !success {
        return Err(err(
            "No readable properties were returned for this resource.",
        ));
    }
    Ok(props)
}
fn check_multistatus(tree: &Property) -> Result<()> {
    if !tree.is("multistatus") {
        return Err(err("Invalid WebDAV operation response."));
    }
    for response in tree.children.iter().filter(|p| p.is("response")) {
        if response
            .value("status")
            .is_some_and(|s| !(200..300).contains(&status(s)))
        {
            return Err(err(
                "Some WebDAV operations failed. Refresh before retrying.",
            ));
        }
        for stat in response.children.iter().filter(|p| p.is("propstat")) {
            if !stat
                .value("status")
                .is_some_and(|s| (200..300).contains(&status(s)))
            {
                return Err(err("The server rejected a property change."));
            }
        }
    }
    Ok(())
}
fn node(href: &str, props: &[Property]) -> FileNode {
    let value = |name| prop(props, name).map(|p| p.text.trim().to_owned());
    let has = |name| {
        prop(props, "current-user-privilege-set")
            .is_some_and(|p| p.contains(name) || p.contains("all"))
    };
    let directory = prop(props, "resourcetype").is_some_and(|p| p.contains("collection"));
    let name = value("displayname")
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            let leaf = href
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or("Files");
            percent_encoding::percent_decode_str(leaf)
                .decode_utf8_lossy()
                .into_owned()
        });
    FileNode {
        id: href.into(),
        name,
        node_type: Some(if directory { "directory" } else { "file" }.into()),
        blob_id: (!directory).then(|| href.into()),
        size: value("getcontentlength").and_then(|n| n.parse().ok()),
        media_type: value("getcontenttype"),
        modified: value("getlastmodified"),
        created: value("creationdate"),
        etag: value("getetag"),
        locked: prop(props, "lockdiscovery").is_some_and(|p| p.contains("activelock")),
        my_rights: Rights {
            may_read: has("read"),
            may_add_children: has("bind") || has("write"),
            may_delete: has("unbind") || has("write"),
            may_modify_content: has("write-content") || has("write"),
            may_share: has("write-acl"),
            ..Default::default()
        },
        ..Default::default()
    }
}
fn escape(s: &str) -> String {
    quick_xml::escape::escape(s).into_owned()
}
fn parse(xml: &str) -> Result<Property> {
    let mut reader = NsReader::from_str(xml);
    reader.config_mut().expand_empty_elements = true;
    let mut stack: Vec<Property> = Vec::new();
    let mut root = None;
    let mut count = 0usize;
    loop {
        let (ns, event) = reader
            .read_resolved_event()
            .map_err(|_| err("Malformed WebDAV XML."))?;
        let namespace = match ns {
            ResolveResult::Bound(n) => n.as_ref().to_owned(),
            _ => String::new(),
        };
        match event {
            Event::Start(e) => {
                count += 1;
                if count > 150000 || stack.len() > 64 {
                    return Err(err("WebDAV metadata exceeds safety limits."));
                }
                let p = Property {
                    namespace,
                    name: e.local_name().as_ref().to_owned(),
                    ..Default::default()
                };
                stack.push(p);
            }
            Event::End(_) => {
                let p = stack.pop().ok_or_else(|| err("Malformed XML nesting."))?;
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(p);
                } else if root.replace(p).is_some() {
                    return Err(err("Multiple XML roots."));
                }
            }
            Event::Text(e) => {
                if let Some(p) = stack.last_mut() {
                    p.text
                        .push_str(&e.xml_content(quick_xml::XmlVersion::Implicit1_0));
                }
            }
            Event::CData(e) => {
                if let Some(p) = stack.last_mut() {
                    p.text
                        .push_str(&e.xml_content(quick_xml::XmlVersion::Implicit1_0));
                }
            }
            Event::GeneralRef(e) => {
                let name = e.as_ref();
                let escaped = format!("&{name};");
                let value = quick_xml::escape::unescape(&escaped)
                    .map_err(|_| err("Unknown XML entity."))?;
                if let Some(p) = stack.last_mut() {
                    p.text.push_str(&value);
                }
            }
            Event::DocType(_) => return Err(err("WebDAV XML document types are not allowed.")),
            Event::Eof => break,
            _ => {}
        }
    }
    if !stack.is_empty() {
        return Err(err("Incomplete WebDAV XML."));
    }
    root.ok_or_else(|| err("Empty WebDAV XML."))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn namespace_and_failed_propstats_do_not_grant_rights() {
        let tree=parse("<d:response xmlns:d='DAV:' xmlns:x='urn:evil'><d:propstat><d:prop><d:displayname>A &amp; B</d:displayname><x:current-user-privilege-set><d:all/></x:current-user-privilege-set></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat><d:propstat><d:prop><d:current-user-privilege-set><d:all/></d:current-user-privilege-set></d:prop><d:status>HTTP/1.1 403 Forbidden</d:status></d:propstat></d:response>").unwrap();
        let file = node(
            "https://example.org/dav/file/me/a",
            &successful_properties(&tree).unwrap(),
        );
        assert_eq!(file.name, "A & B");
        assert!(!file.my_rights.may_share);
        assert!(!file.my_rights.may_read);
    }
    #[test]
    fn prop_patch_checks_individual_statuses() {
        assert!(check_multistatus(&parse("<multistatus xmlns='DAV:'><response><propstat><prop/><status>HTTP/1.1 424 Failed Dependency</status></propstat></response></multistatus>").unwrap()).is_err());
    }
    #[test]
    fn rejects_doctype_and_incomplete_xml() {
        assert!(parse("<!DOCTYPE a><a/>").is_err());
        assert!(parse("<a>").is_err());
    }
}
