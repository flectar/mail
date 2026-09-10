//! RFC 8620 method envelopes with Stalwart's FileNode extension. The extension
//! is a draft: preserve unknown metadata and negotiate both permission shapes.
use super::{
    FileNode, MAX_TRANSFER, PAGE_SIZE, err,
    transport::{Transport, expand, ordered_records},
    validate_name,
};
use crate::error::Result;
use serde_json::{Value, json};
use std::path::Path;

pub const CAPABILITY: &str = "urn:ietf:params:jmap:filenode";
const CORE: &str = "urn:ietf:params:jmap:core";
const SHARING: &str = "urn:ietf:params:jmap:principals";
const QUOTA: &str = "urn:ietf:params:jmap:quota";

#[derive(Clone, Debug)]
pub struct StorageAccount {
    pub id: String,
    pub name: String,
    pub read_only: bool,
}
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BrowseOptions {
    pub node_type: Option<String>,
    pub min_size: Option<u32>,
    pub max_size: Option<u32>,
    pub sort: String,
    pub descending: bool,
}
impl BrowseOptions {
    pub fn filtered(&self) -> bool {
        self.node_type.is_some() || self.min_size.is_some() || self.max_size.is_some()
    }
    pub fn apply_local(&self, nodes: &mut Vec<FileNode>) {
        nodes.retain(|n| {
            self.node_type
                .as_deref()
                .is_none_or(|t| (t == "directory") == n.is_directory())
                && self
                    .min_size
                    .is_none_or(|v| n.size.is_some_and(|s| s >= v as u64))
                && self
                    .max_size
                    .is_none_or(|v| n.size.is_some_and(|s| s <= v as u64))
        });
        nodes.sort_by(|a, b| {
            let order = match self.sort.as_str() {
                "size" => a.size.cmp(&b.size),
                "nodeType" => b.is_directory().cmp(&a.is_directory()),
                _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            }
            .then_with(|| a.id.cmp(&b.id));
            if self.descending {
                order.reverse()
            } else {
                order
            }
        });
    }
}
fn filename_pattern(text: &str) -> String {
    let mut pattern = String::from("*");
    for c in text.chars() {
        match c {
            '*' => pattern.push_str("[*]"),
            '?' => pattern.push_str("[?]"),
            '[' => pattern.push_str("[[]"),
            _ => pattern.push(c),
        }
    }
    pattern.push('*');
    pattern
}
pub struct FilePage {
    pub nodes: Vec<FileNode>,
    pub state: String,
    pub query_state: String,
    pub next_position: Option<usize>,
}
#[derive(Clone)]
pub struct FileClient {
    transport: Transport,
    session: Value,
    pub accounts: Vec<StorageAccount>,
    pub account_id: String,
    pub state: Option<String>,
    pub collision: super::CollisionPolicy,
    pub compare_case_insensitively: bool,
}
impl FileClient {
    pub async fn connect(
        base: &str,
        user: &str,
        secret: &str,
        preferred: Option<&str>,
    ) -> Result<Self> {
        let transport = Transport::new(base, user, secret)?;
        let session = transport
            .discover(&format!("{}/.well-known/jmap", base.trim_end_matches('/')))
            .await?;
        if session["capabilities"].get(CAPABILITY).is_none() {
            return Err(err(
                "This server does not advertise JMAP file storage. You can configure its WebDAV URL instead.",
            ));
        }
        let accounts = session["accounts"]
            .as_object()
            .ok_or_else(|| err("Invalid JMAP accounts."))?
            .iter()
            .filter(|(_, a)| a["accountCapabilities"].get(CAPABILITY).is_some())
            .map(|(id, a)| StorageAccount {
                id: id.clone(),
                name: a["name"].as_str().unwrap_or(id).into(),
                read_only: a["isReadOnly"].as_bool().unwrap_or(true),
            })
            .collect::<Vec<_>>();
        let selected = preferred
            .filter(|id| accounts.iter().any(|a| a.id == *id))
            .or_else(|| {
                session["primaryAccounts"][CAPABILITY]
                    .as_str()
                    .filter(|id| accounts.iter().any(|a| a.id == *id))
            })
            .or_else(|| accounts.first().map(|a| a.id.as_str()))
            .ok_or_else(|| err("No accessible file storage account."))?
            .to_owned();
        for key in ["apiUrl", "uploadUrl", "downloadUrl"] {
            let template = session[key]
                .as_str()
                .ok_or_else(|| err("Incomplete JMAP session."))?;
            let url = expand(
                template,
                &[
                    ("accountId", &selected),
                    ("blobId", "probe"),
                    ("name", "probe"),
                    ("type", "application/octet-stream"),
                ],
            )?;
            let _ = transport.request("GET", &url)?;
        }
        Ok(Self {
            transport,
            session,
            accounts,
            account_id: selected,
            state: None,
            collision: super::CollisionPolicy::Reject,
            compare_case_insensitively: false,
        })
    }
    pub fn capabilities(&self) -> &Value {
        &self.session["accounts"][&self.account_id]["accountCapabilities"][CAPABILITY]
    }
    pub fn can_create_root(&self) -> bool {
        self.capabilities()["mayCreateTopLevelFileNode"].as_bool() == Some(true)
            && !self.read_only()
    }
    pub fn read_only(&self) -> bool {
        self.accounts
            .iter()
            .find(|a| a.id == self.account_id)
            .is_none_or(|a| a.read_only)
    }
    pub fn modern(&self) -> bool {
        self.capabilities().get("forbiddenNameChars").is_some()
            || self.capabilities()["fileNodeQuerySortOptions"]
                .as_array()
                .is_some_and(|a| a.iter().any(|v| v == "nodeType"))
    }
    pub fn select_account(&mut self, id: &str) -> Result<()> {
        if !self.accounts.iter().any(|a| a.id == id) {
            return Err(err("File account is no longer available."));
        }
        self.account_id = id.into();
        self.state = None;
        Ok(())
    }
    fn name(&self, name: &str) -> Result<()> {
        validate_name(name)?;
        let cap = self.capabilities();
        if name.len() as u64 > cap["maxSizeFileNodeName"].as_u64().unwrap_or(255)
            || cap["forbiddenNameChars"]
                .as_str()
                .is_some_and(|chars| name.chars().any(|c| chars.contains(c)))
            || cap["forbiddenNodeNames"].as_array().is_some_and(|names| {
                names
                    .iter()
                    .any(|n| n.as_str().is_some_and(|n| n.eq_ignore_ascii_case(name)))
            })
        {
            return Err(err("The server does not allow this file name."));
        }
        Ok(())
    }
    /// Standard get/query/changes/queryChanges/set/copy envelopes share strict
    /// response correlation and per-object error handling, including HTTP 200 errors.
    async fn call(&self, method: &str, capability: &str, mut args: Value) -> Result<Value> {
        if self.session["capabilities"].get(capability).is_none() {
            return Err(err("The server does not advertise this capability."));
        }
        let account = if capability == SHARING {
            self.session["primaryAccounts"][SHARING]
                .as_str()
                .ok_or_else(|| err("No personal sharing account is available."))?
        } else if self.session["accounts"][&self.account_id]["accountCapabilities"]
            .get(capability)
            .is_some()
        {
            self.account_id.as_str()
        } else if capability == CAPABILITY {
            return Err(err("File storage account is no longer available."));
        } else {
            self.session["primaryAccounts"][capability]
                .as_str()
                .or_else(|| {
                    self.session["accounts"].as_object().and_then(|accounts| {
                        accounts
                            .iter()
                            .find(|(_, a)| a["accountCapabilities"].get(capability).is_some())
                            .map(|(id, _)| id.as_str())
                    })
                })
                .ok_or_else(|| err("No account advertises this capability."))?
        };
        args["accountId"] = json!(account);
        let api = self.session["apiUrl"]
            .as_str()
            .ok_or_else(|| err("Missing JMAP API URL."))?;
        let response =
            self.transport
                .json(self.transport.request("POST", api)?.json(
                    &json!({"using":[CORE,capability],"methodCalls":[[method,args,"files"]]}),
                ))
                .await?;
        method_result(&response, method, account)
    }
    pub async fn get(&self, ids: &[String]) -> Result<Value> {
        self.call("FileNode/get", CAPABILITY, json!({"ids":ids}))
            .await
    }
    pub async fn query(&self, filter: Value, position: usize, sort: Value) -> Result<Value> {
        self.call("FileNode/query",CAPABILITY,json!({"filter":filter,"position":position,"limit":PAGE_SIZE,"sort":sort,"calculateTotal":true})).await
    }
    pub async fn list(
        &mut self,
        parent: Option<&str>,
        text: &str,
        position: usize,
    ) -> Result<FilePage> {
        self.list_filtered(parent, text, position, None, &BrowseOptions::default())
            .await
    }
    pub async fn list_all(&mut self, position: usize) -> Result<FilePage> {
        self.list_filtered(
            None,
            "",
            position,
            Some(json!({})),
            &BrowseOptions::default(),
        )
        .await
    }
    pub async fn list_options(
        &mut self,
        parent: Option<&str>,
        text: &str,
        position: usize,
        options: &BrowseOptions,
    ) -> Result<FilePage> {
        self.list_filtered(parent, text, position, None, options)
            .await
    }
    async fn list_filtered(
        &mut self,
        parent: Option<&str>,
        text: &str,
        position: usize,
        override_filter: Option<Value>,
        options: &BrowseOptions,
    ) -> Result<FilePage> {
        let mut filter = if text.is_empty() {
            match parent {
                Some(id) => json!({"parentId":id}),
                None => {
                    if self.modern() {
                        json!({"isTopLevel":true})
                    } else {
                        json!({"hasParentId":false})
                    }
                }
            }
        } else {
            json!({"nameMatch":filename_pattern(text)})
        };
        if !text.is_empty()
            && let Some(id) = parent
        {
            filter["ancestorId"] = json!(id);
        }
        if let Some(kind) = &options.node_type {
            filter["nodeType"] = json!(kind);
        }
        if let Some(min) = options.min_size {
            filter["minSize"] = json!(min);
        }
        if let Some(max) = options.max_size {
            filter["maxSize"] = json!(max);
        }
        if let Some(value) = override_filter {
            filter = value;
        }
        let supported = self.capabilities()["fileNodeQuerySortOptions"].as_array();
        let property = if options.sort.is_empty() {
            "name"
        } else {
            &options.sort
        };
        let sort = if supported.is_some_and(|a| a.iter().any(|v| v == property)) {
            json!([{"property":property,"isAscending":!options.descending}])
        } else {
            json!([])
        };
        let query = self.query(filter, position, sort).await?;
        let ids: Vec<String> = serde_json::from_value(query["ids"].clone())?;
        if ids.len() > PAGE_SIZE {
            return Err(err("File server exceeded the requested page limit."));
        }
        let max_get = self.session["capabilities"][CORE]["maxObjectsInGet"]
            .as_u64()
            .unwrap_or(100)
            .clamp(1, 100) as usize;
        let mut nodes = Vec::new();
        let mut state = None;
        // Read a state even for an empty folder so create uses a fresh precondition.
        let chunks = if ids.is_empty() {
            vec![&ids[..]]
        } else {
            ids.chunks(max_get).collect()
        };
        for ids in chunks {
            let result = self.get(ids).await?;
            let current = result["state"]
                .as_str()
                .ok_or_else(|| err("Missing FileNode state."))?
                .to_owned();
            if state.as_ref().is_some_and(|s| s != &current) {
                return Err(err("Storage changed while loading. Refresh to try again."));
            }
            state = Some(current);
            nodes.extend(ordered_records(
                ids,
                serde_json::from_value::<Vec<FileNode>>(result["list"].clone())?,
                |node| &node.id,
            )?);
        }
        let mut ancestors = std::collections::HashMap::<String, FileNode>::new();
        for node in &nodes {
            ancestors.insert(node.id.clone(), node.clone());
        }
        for node in &mut nodes {
            let mut parent = node.parent_id.clone();
            let mut seen = std::collections::HashSet::new();
            while let Some(id) = parent {
                if !seen.insert(id.clone()) || seen.len() > 64 {
                    return Err(err("Invalid FileNode ancestor hierarchy."));
                }
                if !ancestors.contains_key(&id) {
                    let result = self.get(std::slice::from_ref(&id)).await?;
                    if result["state"].as_str() != state.as_deref() {
                        return Err(err("Storage changed while resolving permissions. Refresh."));
                    }
                    let Some(value) = result["list"].as_array().and_then(|a| a.first()) else {
                        break;
                    };
                    ancestors.insert(id.clone(), serde_json::from_value(value.clone())?);
                }
                let ancestor = &ancestors[&id];
                node.my_rights.inherit(&ancestor.my_rights);
                parent = ancestor.parent_id.clone();
            }
        }
        self.state = state;
        let total = query["total"].as_u64().map(|n| n as usize);
        let next = position + ids.len();
        Ok(FilePage {
            nodes,
            state: self.state.clone().unwrap_or_default(),
            query_state: query["queryState"].as_str().unwrap_or_default().into(),
            next_position: (!ids.is_empty() && total.map_or(ids.len() == PAGE_SIZE, |n| next < n))
                .then_some(next),
        })
    }
    /// RFC 8620 EventSource is a wake-up hint. Durable /changes and periodic
    /// reconciliation remain authoritative after disconnects or missed events.
    pub async fn wait_for_change(&self) -> Result<()> {
        use futures::StreamExt;
        let template = self.session["eventSourceUrl"]
            .as_str()
            .ok_or_else(|| err("File push is not advertised."))?;
        let url = expand(
            template,
            &[
                ("types", "FileNode"),
                ("closeafter", "state"),
                ("ping", "30"),
            ],
        )?;
        let response = self
            .transport
            .send(
                self.transport
                    .request("GET", &url)?
                    .header("Accept", "text/event-stream"),
            )
            .await?;
        if !response
            .headers()
            .get("Content-Type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("text/event-stream"))
        {
            return Err(err("Invalid file push response."));
        }
        let mut stream = response.bytes_stream();
        let mut line = Vec::new();
        while let Some(chunk) = stream.next().await {
            for byte in chunk.map_err(|_| err("File push disconnected."))? {
                if byte == b'\n' {
                    let text = String::from_utf8_lossy(&line);
                    if let Some(data) = text.trim_end().strip_prefix("data:")
                        && let Ok(event) = serde_json::from_str::<Value>(data.trim())
                        && event["@type"] == "StateChange"
                        && event["changed"]
                            .as_object()
                            .is_some_and(|a| a.values().any(|v| v.get("FileNode").is_some()))
                    {
                        return Ok(());
                    }
                    line.clear();
                } else {
                    if line.len() >= 64 * 1024 {
                        return Err(err("File push event exceeds the metadata limit."));
                    }
                    line.push(byte);
                }
            }
        }
        Err(err("File push disconnected."))
    }
    pub async fn changes(&self, since: &str) -> Result<Value> {
        self.call(
            "FileNode/changes",
            CAPABILITY,
            json!({"sinceState":since,"maxChanges":1000}),
        )
        .await
    }
    pub async fn query_changes(&self, since: &str, filter: Value, sort: Value) -> Result<Value> {
        self.call(
            "FileNode/queryChanges",
            CAPABILITY,
            json!({"sinceQueryState":since,"filter":filter,"sort":sort,"maxChanges":1000}),
        )
        .await
    }
    pub(crate) async fn rights(&self, id: &str) -> Result<super::Rights> {
        let mut rights = super::Rights::default();
        let mut next = Some(id.to_owned());
        let mut seen = std::collections::HashSet::new();
        while let Some(id) = next {
            if !seen.insert(id.clone()) || seen.len() > 64 {
                return Err(err("Invalid file hierarchy."));
            }
            let result = self.get(&[id]).await?;
            if result["state"].as_str() != self.state.as_deref() {
                return Err(err("Storage changed. Refresh before editing files."));
            }
            let Some(value) = result["list"].as_array().and_then(|a| a.first()) else {
                if seen.len() > 1 {
                    break;
                } // A shared node need not expose its ancestors.
                return Err(err("File no longer exists."));
            };
            let node: FileNode = serde_json::from_value(value.clone())?;
            rights.inherit(&node.my_rights);
            next = node.parent_id;
        }
        Ok(rights)
    }
    async fn require_destination(&self, parent: Option<&str>) -> Result<()> {
        let allowed = match parent {
            Some(parent) => self.rights(parent).await?.add(),
            None => self.can_create_root(),
        };
        if !allowed {
            return Err(err("You cannot create files in this folder."));
        }
        Ok(())
    }
    async fn validate_set_rights(&self, args: &Value) -> Result<()> {
        if let Some(create) = args["create"].as_object() {
            for node in create.values() {
                self.name(
                    node["name"]
                        .as_str()
                        .ok_or_else(|| err("File name is required."))?,
                )?;
                let parent = node["parentId"].as_str();
                if let Some(reference) = parent.and_then(|s| s.strip_prefix('#')) {
                    if !create.contains_key(reference) {
                        return Err(err("Unknown parent creation reference."));
                    }
                } else {
                    self.require_destination(parent).await?;
                }
            }
        }
        if let Some(update) = args["update"].as_object() {
            for (id, patch) in update {
                let rights = self.rights(id).await?;
                for (key, _) in patch
                    .as_object()
                    .ok_or_else(|| err("Invalid file patch."))?
                {
                    let allowed = match key.split('/').next().unwrap_or("") {
                        "shareWith" => rights.may_share,
                        "name" | "parentId" => rights.rename(),
                        "isSubscribed" => rights.may_read,
                        _ => rights.modify(),
                    };
                    if !allowed {
                        return Err(err("You do not have permission to change this file."));
                    }
                }
                if patch.get("parentId").is_some() {
                    self.require_destination(patch["parentId"].as_str()).await?;
                }
            }
        }
        if let Some(destroy) = args["destroy"].as_array() {
            for id in destroy {
                if !self
                    .rights(id.as_str().ok_or_else(|| err("Invalid file ID."))?)
                    .await?
                    .delete()
                {
                    return Err(err("You cannot delete this file."));
                }
            }
        }
        Ok(())
    }
    pub async fn set(&mut self, mut args: Value) -> Result<Value> {
        if self.read_only() {
            return Err(err("This storage account is read-only."));
        }
        self.validate_set_rights(&args).await?;
        let state = self
            .state
            .as_ref()
            .ok_or_else(|| err("Refresh storage before changing files."))?;
        args["ifInState"] = json!(state);
        if !matches!(self.collision, super::CollisionPolicy::Reject) {
            args["onExists"] = json!(self.collision);
        }
        args["compareCaseInsensitively"] = json!(self.compare_case_insensitively);
        let result = self.call("FileNode/set", CAPABILITY, args).await?;
        self.state = result["newState"].as_str().map(str::to_owned);
        Ok(result)
    }
    pub async fn create_folder(&mut self, parent: Option<&str>, name: &str) -> Result<Value> {
        self.name(name)?;
        self.set(json!({"create":{"new":{"name":name,"parentId":parent,"blobId":null}}}))
            .await
    }
    pub async fn update(&mut self, id: &str, patch: Value) -> Result<Value> {
        if let Some(name) = patch.get("name").and_then(Value::as_str) {
            self.name(name)?;
        }
        self.set(json!({"update":{id:patch}})).await
    }
    pub async fn delete(&mut self, id: &str, recursive: bool) -> Result<Value> {
        self.set(json!({"destroy":[id],"onDestroyRemoveChildren":recursive}))
            .await
    }
    /// RFC 8620 /copy is cross-account. Within one account, create nodes
    /// referencing existing blobs; creation references preserve the hierarchy.
    pub async fn copy(&mut self, id: &str, parent: Option<&str>, name: &str) -> Result<Value> {
        self.name(name)?;
        let state = self
            .state
            .clone()
            .ok_or_else(|| err("Refresh before copying files."))?;
        let source = self.get(&[id.to_owned()]).await?;
        if source["state"] != state {
            return Err(err("Storage changed. Refresh before copying."));
        }
        let root: FileNode = serde_json::from_value(
            source["list"]
                .as_array()
                .and_then(|a| a.first())
                .ok_or_else(|| err("Source file no longer exists."))?
                .clone(),
        )?;
        let limit = self.session["capabilities"][CORE]["maxObjectsInSet"]
            .as_u64()
            .unwrap_or(100)
            .min(10000) as usize;
        let mut nodes = vec![root];
        let mut index = 0;
        while index < nodes.len() {
            if nodes[index].is_directory() {
                let mut position = 0;
                loop {
                    let page = self.list(Some(&nodes[index].id), "", position).await?;
                    if page.state != state {
                        self.state = Some(state);
                        return Err(err("Storage changed. Refresh before copying."));
                    }
                    if nodes.len() + page.nodes.len() > limit {
                        return Err(err(
                            "This folder exceeds the server's batch copy limit. Copy smaller subfolders.",
                        ));
                    }
                    nodes.extend(page.nodes);
                    if let Some(next) = page.next_position {
                        position = next;
                    } else {
                        break;
                    }
                }
            }
            index += 1;
        }
        if parent.is_some_and(|parent| nodes.iter().any(|n| n.id == parent)) {
            return Err(err("A folder cannot be copied into itself."));
        }
        let ids = nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.id.clone(), format!("copy{i}")))
            .collect::<std::collections::HashMap<_, _>>();
        let mut create = serde_json::Map::new();
        for (index, node) in nodes.iter().enumerate() {
            let parent_id = if index == 0 {
                parent.map(str::to_owned)
            } else {
                node.parent_id
                    .as_ref()
                    .and_then(|id| ids.get(id))
                    .map(|id| format!("#{id}"))
            };
            let mut value = json!({"name":if index==0 {name} else {&node.name},"parentId":parent_id,"blobId":node.blob_id});
            if !node.is_directory() {
                value["type"] = json!(node.media_type);
                value["executable"] = json!(node.executable);
            }
            // Same-account copies use /set, so preserve source timestamps
            // explicitly. Otherwise Keep newest would compare a fresh default
            // timestamp and could replace a newer destination with older data.
            for (property, date) in [
                ("created", &node.created),
                ("modified", &node.modified),
                ("accessed", &node.accessed),
            ] {
                if let Some(date) = date {
                    value[property] = json!(date);
                }
            }
            if matches!(self.collision, super::CollisionPolicy::Newest) && node.modified.is_none() {
                return Err(err(
                    "Source modification time is unavailable for Keep newest.",
                ));
            }
            create.insert(format!("copy{index}"), value);
        }
        self.state = Some(state);
        self.set(json!({"create":create})).await
    }
    pub async fn copy_from_account(
        &mut self,
        from_account: &str,
        from_state: &str,
        id: &str,
        parent: Option<&str>,
        name: &str,
    ) -> Result<Value> {
        self.name(name)?;
        self.require_destination(parent).await?;
        if from_account == self.account_id {
            return self.copy(id, parent, name).await;
        }
        if self.read_only() {
            return Err(err("This storage account is read-only."));
        }
        let mut source = self.clone();
        source.select_account(from_account)?;
        source.state = Some(from_state.into());
        let root = source.get(&[id.into()]).await?;
        if root["state"].as_str() != Some(from_state) {
            return Err(err("Source storage changed. Refresh before copying."));
        }
        let node: FileNode = serde_json::from_value(
            root["list"]
                .as_array()
                .and_then(|a| a.first())
                .ok_or_else(|| err("Source file was not found."))?
                .clone(),
        )?;
        let mut nodes = vec![node];
        let mut seen = std::collections::HashSet::from([id.to_owned()]);
        let mut index = 0;
        // Gather and validate the source before changing the destination.
        while index < nodes.len() {
            self.name(&nodes[index].name)?;
            if nodes[index].is_directory() {
                let mut position = 0;
                loop {
                    let page = source.list(Some(&nodes[index].id), "", position).await?;
                    if page.state != from_state {
                        return Err(err("Source storage changed. Refresh before copying."));
                    }
                    if nodes.len() + page.nodes.len() > 1000 {
                        return Err(err(
                            "Copy at most 1,000 nodes between storage spaces at once.",
                        ));
                    }
                    for node in page.nodes {
                        if !seen.insert(node.id.clone()) {
                            return Err(err("Invalid source file hierarchy."));
                        }
                        nodes.push(node);
                    }
                    match page.next_position {
                        Some(next) => position = next,
                        None => break,
                    }
                }
            }
            index += 1;
        }
        let mut created = serde_json::Map::new();
        let initial_state = self
            .state
            .clone()
            .ok_or_else(|| err("Refresh before copying files."))?;
        for (index, node) in nodes.iter().enumerate() {
            let destination_parent = if index == 0 {
                parent.map(str::to_owned)
            } else {
                Some(
                    created
                        .get(node.parent_id.as_deref().unwrap_or(""))
                        .and_then(|v: &Value| v["id"].as_str())
                        .ok_or_else(|| err("Copied parent folder is missing."))?
                        .to_owned(),
                )
            };
            // Stalwart indexes the create map by source ID. Each folder must
            // exist before copying its children into the returned destination ID.
            let result = self.call("FileNode/copy", CAPABILITY, json!({"fromAccountId":from_account,"ifFromInState":from_state,"ifInState":self.state,"create":{&node.id:{"parentId":destination_parent,"name":if index == 0 {name} else {&node.name}}},"onSuccessDestroyOriginal":false,"onExists":if matches!(self.collision,super::CollisionPolicy::Reject){Value::Null}else{json!(self.collision)},"compareCaseInsensitively":self.compare_case_insensitively})).await
                .map_err(|e| err(format!("Copy stopped after {} items: {e}. Refresh the destination before retrying.", created.len())))?;
            let value = result["created"][&node.id].clone();
            if value["id"].as_str().is_none() {
                return Err(err(
                    "Copy response omitted the new file ID. Refresh before retrying.",
                ));
            }
            created.insert(node.id.clone(), value);
            self.state = result["newState"].as_str().map(str::to_owned);
        }
        Ok(
            json!({"accountId":self.account_id,"fromAccountId":from_account,"oldState":initial_state,"newState":self.state,"created":created}),
        )
    }

    pub async fn upload(
        &mut self,
        parent: Option<&str>,
        name: &str,
        path: &Path,
        replace: Option<&str>,
    ) -> Result<Value> {
        self.name(name)?;
        let limit = self.session["capabilities"][CORE]["maxSizeUpload"]
            .as_u64()
            .unwrap_or(MAX_TRANSFER as u64)
            .min(MAX_TRANSFER as u64) as usize;
        let (data, length) = super::upload_body(path, limit).await?;
        let url = expand(
            self.session["uploadUrl"]
                .as_str()
                .ok_or_else(|| err("Missing upload URL."))?,
            &[("accountId", &self.account_id)],
        )?;
        let uploaded = self
            .transport
            .json(
                self.transport
                    .request("POST", &url)?
                    .header("Content-Length", length)
                    .header("Content-Type", crate::queue::mime_guess_from_name(name))
                    .body(data),
            )
            .await?;
        if uploaded["accountId"] != self.account_id {
            return Err(err("Upload account mismatch."));
        }
        let blob = uploaded["blobId"]
            .as_str()
            .ok_or_else(|| err("Upload did not return a blob."))?;
        let mut value = json!({"blobId":blob,"type":uploaded["type"].as_str().unwrap_or("application/octet-stream"),"modified":chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs,true)});
        if let Some(id) = replace {
            self.update(id, value).await
        } else {
            value["name"] = json!(name);
            value["parentId"] = json!(parent);
            self.set(json!({"create":{"new":value}})).await
        }
    }
    async fn download_response(&self, node: &FileNode) -> Result<reqwest::Response> {
        let blob = node
            .blob_id
            .as_deref()
            .ok_or_else(|| err("Folders cannot be downloaded as individual files."))?;
        let url = expand(
            self.session["downloadUrl"]
                .as_str()
                .ok_or_else(|| err("Missing download URL."))?,
            &[
                ("accountId", &self.account_id),
                ("blobId", blob),
                ("name", &node.name),
                (
                    "type",
                    node.media_type
                        .as_deref()
                        .unwrap_or("application/octet-stream"),
                ),
            ],
        )?;
        self.transport
            .send(self.transport.request("GET", &url)?)
            .await
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
    pub async fn quota(&self) -> Result<Value> {
        self.call("Quota/get", QUOTA, json!({"ids":null})).await
    }
    pub async fn principals(&self, text: &str) -> Result<Value> {
        self.call(
            "Principal/query",
            SHARING,
            json!({"filter":{"text":text},"limit":100}),
        )
        .await
    }
    pub async fn resolve_principal(&self, recipient: &str) -> Result<String> {
        if !recipient.contains('@') {
            return Ok(recipient.into());
        }
        let result = self.principals(recipient).await?;
        let ids = result["ids"]
            .as_array()
            .ok_or_else(|| err("Invalid principal lookup."))?;
        let result = self
            .call("Principal/get", SHARING, json!({"ids":ids}))
            .await?;
        let matches = result["list"]
            .as_array()
            .ok_or_else(|| err("Invalid principal lookup."))?
            .iter()
            .filter(|p| {
                p["email"]
                    .as_str()
                    .is_some_and(|email| email.eq_ignore_ascii_case(recipient))
            })
            .filter_map(|p| p["id"].as_str())
            .collect::<Vec<_>>();
        if matches.len() != 1 {
            return Err(err(
                "The recipient could not be uniquely identified. Use their principal ID if directory lookup is disabled.",
            ));
        }
        Ok(matches[0].into())
    }
    pub async fn dismiss_share_notifications(&self, ids: &[String], state: &str) -> Result<Value> {
        if ids.len() > 100 {
            return Err(err("Too many sharing notifications."));
        }
        self.call(
            "ShareNotification/set",
            SHARING,
            json!({"ifInState":state,"destroy":ids}),
        )
        .await
    }
    pub async fn share_notifications(&self) -> Result<Value> {
        {
            let query = self.call("ShareNotification/query", SHARING, json!({"filter":{"objectType":"FileNode"},"sort":[{"property":"created","isAscending":false}],"limit":100})).await?;
            self.call(
                "ShareNotification/get",
                SHARING,
                json!({"ids":query["ids"]}),
            )
            .await
        }
    }
}
fn method_result(response: &Value, method: &str, account: &str) -> Result<Value> {
    let calls = response["methodResponses"]
        .as_array()
        .ok_or_else(|| err("Invalid JMAP response."))?;
    let call = calls
        .iter()
        .find(|c| c[2] == "files")
        .ok_or_else(|| err("Missing JMAP method response."))?;
    if call[0] == "error" {
        return Err(err(format!(
            "File operation failed: {}",
            call[1]["type"].as_str().unwrap_or("unknown error")
        )));
    }
    if call[0] != method || call[1]["accountId"] != account {
        return Err(err("JMAP response did not match the request."));
    }
    for key in ["notCreated", "notUpdated", "notDestroyed"] {
        if let Some(errors) = call[1][key].as_object().filter(|e| !e.is_empty()) {
            let kinds = errors
                .values()
                .filter_map(|v| v["type"].as_str())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(err(format!(
                "Some file operations failed ({kinds}). Refresh to inspect completed changes before retrying."
            )));
        }
    }
    Ok(call[1].clone())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_success_status_with_method_or_object_errors() {
        for value in [
            json!({"methodResponses":[["error",{"type":"stateMismatch"},"files"]]}),
            json!({"methodResponses":[["FileNode/set",{"accountId":"a","notUpdated":{"x":{"type":"forbidden"}}},"files"]]}),
            json!({"methodResponses":[["FileNode/set",{"accountId":"other"},"files"]]}),
        ] {
            assert!(method_result(&value, "FileNode/set", "a").is_err());
        }
    }
    #[test]
    fn opaque_template_values_are_encoded() {
        let url = expand(
            "https://example.org/{accountId}/{blobId}/{name}?type={type}",
            &[
                ("accountId", "a/b"),
                ("blobId", "#?"),
                ("name", "a b.pdf"),
                ("type", "text/plain"),
            ],
        )
        .unwrap();
        assert_eq!(
            url,
            "https://example.org/a%2Fb/%23%3F/a%20b.pdf?type=text%2Fplain"
        );
    }
}
