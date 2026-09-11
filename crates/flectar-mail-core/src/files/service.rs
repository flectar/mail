//! Protocol-independent file workspace, owned by the core. Native hosts only
//! choose paths, render previews and dispatch commands; no Slint dependency.
use super::{self as files, AttachmentFile, ConnectionSettings, FileClient, FileNode, PAGE_SIZE};
use crate::Core;
use serde_json::json;
use std::path::{Path, PathBuf};
type Result<T> = std::result::Result<T, String>;
pub enum Output {
    Operations(Vec<super::store::Operation>),
    Status(String),
    Preview(Preview),
}
pub enum Preview {
    Text(String),
    Data(Vec<u8>, String),
}
#[derive(Clone)]
pub enum Entry {
    Attachment(AttachmentFile),
    Remote(FileNode),
}
#[derive(Default)]
pub struct FilesService {
    pub browse: super::jmap::BrowseOptions,
    pub collision: super::CollisionPolicy,
    pub case_insensitive: bool,
    pub offline: bool,
    pub cached_space: Option<i64>,
    pub cached_spaces: Vec<(i64, String, String)>,
    pub active_operation: Option<i64>,
    pub accounts: Vec<(i64, String)>,
    pub account: usize,
    pub settings: ConnectionSettings,
    pub attachments: bool,
    pub server_attachments: bool,
    pub mail_search: Option<super::mail_search::MailSearch>,
    pub client: Option<FileClient>,
    pub history: Vec<FileNode>,
    pub entries: Vec<Entry>,
    pub query: String,
    pub next: Option<usize>,
    pub before: Option<i64>,
    pub query_state: Option<String>,
    pub can_create: bool,
    pub quota: String,
    pub notification_ids: Vec<String>,
    pub notification_state: Option<String>,
}
impl FilesService {
    pub fn account_id(&self) -> Option<i64> {
        self.account
            .checked_sub(1)
            .and_then(|i| self.accounts.get(i))
            .map(|a| a.0)
    }
    pub fn parent(&self) -> Option<String> {
        self.history.last().map(|n| n.id.clone())
    }
    pub fn selected(&self, index: i32) -> Result<Entry> {
        usize::try_from(index)
            .ok()
            .and_then(|i| self.entries.get(i))
            .cloned()
            .ok_or_else(|| "Select a file first.".into())
    }
    async fn init(&mut self, core: &Core) -> Result<()> {
        let configs = core
            .list_account_configs()
            .await
            .map_err(|e| e.to_string())?;
        let prior = self.account_id();
        self.accounts = configs.into_iter().map(|a| (a.id, a.email)).collect();
        self.account = prior
            .and_then(|id| self.accounts.iter().position(|a| a.0 == id))
            .map(|i| i + 1)
            .unwrap_or(0);
        let selected = self.account_id();
        self.cached_spaces = core
            .files_db
            .read(move |c| {
                let mut q = c.prepare(
                    "SELECT id,name,remote_id FROM spaces WHERE account_id=?1 ORDER BY remote_id",
                )?;
                Ok(
                    q.query_map([selected], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                        .collect::<rusqlite::Result<Vec<_>>>()?,
                )
            })
            .await
            .map_err(|e| e.to_string())?;
        if prior.is_some() && self.account == 0 {
            self.cached_space = None;
            self.offline = false;
            self.mail_search = None;
            self.client = None;
            self.history.clear();
            self.entries.clear();
        }
        Ok(())
    }
    async fn settings(&mut self, core: &Core) -> Result<()> {
        self.settings = if let Some(id) = self.account_id() {
            core.file_connection_settings(id)
                .await
                .map_err(|e| e.to_string())?
        } else {
            ConnectionSettings::default()
        };
        Ok(())
    }
    async fn connect(&mut self, core: &Core) -> Result<()> {
        let id = self
            .account_id()
            .ok_or_else(|| "Select an account to browse server storage.".to_string())?;
        self.client = None;
        self.can_create = false;
        let mut client = core
            .connect_files(id, &self.settings)
            .await
            .map_err(|e| e.to_string())?;
        core.save_file_connection_settings(id, self.settings.clone())
            .await
            .map_err(|e| e.to_string())?;
        if let (Some(space), FileClient::Jmap(c)) = (
            super::store::selected_space(&core.files_db, id)
                .await
                .map_err(|e| e.to_string())?,
            &mut client,
        ) {
            let remote = core
                .files_db
                .read(move |db| {
                    Ok(
                        db.query_row("SELECT remote_id FROM spaces WHERE id=?1", [space], |r| {
                            r.get::<_, String>(0)
                        })?,
                    )
                })
                .await
                .map_err(|e| e.to_string())?;
            c.select_account(&remote).map_err(|e| e.to_string())?;
        }
        self.client = Some(client);
        self.offline = false;
        Ok(())
    }
    pub async fn execute(
        &mut self,
        core: &Core,
        action: &str,
        a: &str,
        b: &str,
        selected: i32,
        local_path: Option<PathBuf>,
    ) -> Result<Output> {
        // Mail metadata reads must remain responsive during a large remote
        // tree scan. They use Mail's own SQLite queue and do not touch file
        // content or remote mutations. Other commands share the lifecycle gate.
        let metadata_only = (self.attachments
            && b != "storage"
            && matches!(
                action,
                "load"
                    | "account"
                    | "account-id"
                    | "search"
                    | "refresh"
                    | "more"
                    | "attachment-source"
            ))
            || (matches!(action, "account" | "account-id") && b == "attachments");
        let _files_guard = if metadata_only {
            None
        } else {
            Some(core.file_work_lock.lock().await)
        };
        if !metadata_only {
            core.files_db.write(|c|{c.execute("UPDATE operations SET state='failed',error='Upload staging was interrupted. Select the source again.' WHERE state='preparing'",[])?;Ok(())}).await.map_err(|e|e.to_string())?;
        }
        if let Some(id) = self.active_operation.take() {
            super::store::finish(
                &core.files_db,
                id,
                "uncertain",
                Some(
                    "Cancelled while contacting storage. Inspect the destination before retrying."
                        .into(),
                ),
            )
            .await
            .map_err(|e| e.to_string())?;
        }
        if matches!(
            action,
            "transfers" | "retry-operation" | "cancel-operation" | "resolve-operation"
        ) {
            let account = self.account_id().ok_or("Select an account.")?;
            if action != "transfers" {
                let id = a.parse::<i64>().map_err(|_| "Invalid operation.")?;
                let (kind,state,payload,space)=core.files_db.read(move |c|Ok(c.query_row("SELECT action,state,payload_json,space_id FROM operations WHERE id=?1 AND account_id=?2",rusqlite::params![id,account],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,Option<i64>>(3)?)))?)).await.map_err(|e|e.to_string())?;
                if action == "retry-operation" {
                    if state != "queued" {
                        return Err("Only operations that have not contacted the server may be resumed. Inspect uncertain changes and mark them reviewed.".into());
                    }
                    let payload: serde_json::Value =
                        serde_json::from_str(&payload).map_err(|e| e.to_string())?;
                    let mut pending = FilesService {
                        accounts: self.accounts.clone(),
                        account: self.account,
                        settings: self.settings.clone(),
                        collision: serde_json::from_value(payload["collision"].clone())
                            .unwrap_or_default(),
                        case_insensitive: payload["caseInsensitive"].as_bool().unwrap_or(false),
                        attachments: false,
                        ..Default::default()
                    };
                    pending.connect(core).await?;
                    if let (Some(space), Some(FileClient::Jmap(client))) =
                        (space, pending.client.as_mut())
                    {
                        let remote = core
                            .files_db
                            .read(move |c| {
                                Ok(c.query_row(
                                    "SELECT remote_id FROM spaces WHERE id=?1",
                                    [space],
                                    |r| r.get::<_, String>(0),
                                )?)
                            })
                            .await
                            .map_err(|e| e.to_string())?;
                        client.select_account(&remote).map_err(|e| e.to_string())?;
                        let current = client
                            .list(None, "", 0)
                            .await
                            .map_err(|e| e.to_string())?
                            .state;
                        if payload["state"].as_str().is_some_and(|s| s != current) {
                            return Err("Storage changed since this operation was queued. Cancel it and repeat the action from a refreshed view.".into());
                        }
                    }
                    pending.history = serde_json::from_value(payload["history"].clone())
                        .map_err(|e| e.to_string())?;
                    pending.cached_space = space;
                    if !payload["node"].is_null() {
                        pending.entries.push(Entry::Remote(
                            serde_json::from_value(payload["node"].clone())
                                .map_err(|e| e.to_string())?,
                        ));
                    }
                    pending.can_create = true; // Current protocol permissions/preconditions are checked below.
                    let path = payload["stagedName"]
                        .as_str()
                        .map(|name| {
                            files::validate_name(name).map_err(|e| e.to_string())?;
                            Ok::<_, String>(
                                core.paths
                                    .files_staging_dir(account)
                                    .join(id.to_string())
                                    .join(name),
                            )
                        })
                        .transpose()?;
                    if matches!(kind.as_str(), "upload" | "replace") && path.is_none() {
                        return Err("The queued upload has no complete staging data. Cancel it and select the source again.".into());
                    }
                    if let Some(FileClient::Jmap(c)) = &mut pending.client {
                        c.collision = serde_json::from_value(payload["collision"].clone())
                            .unwrap_or_default();
                        c.compare_case_insensitively =
                            payload["caseInsensitive"].as_bool().unwrap_or(false);
                    }
                    super::store::finish(&core.files_db, id, "running", None)
                        .await
                        .map_err(|e| e.to_string())?;
                    self.active_operation = Some(id);
                    let result = perform(
                        &mut pending,
                        core,
                        &kind,
                        payload["a"].as_str().unwrap_or(""),
                        payload["b"].as_str().unwrap_or(""),
                        0,
                        path,
                    )
                    .await;
                    super::store::finish(
                        &core.files_db,
                        id,
                        if result.is_ok() {
                            "completed"
                        } else {
                            "uncertain"
                        },
                        result.as_ref().err().cloned(),
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                    self.active_operation = None;
                    result?;
                } else {
                    if state == "running" {
                        return Err("Cancel the active operation first.".into());
                    }
                    super::store::finish(
                        &core.files_db,
                        id,
                        if action == "resolve-operation" {
                            "completed"
                        } else {
                            "cancelled"
                        },
                        None,
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                }
                let _ = tokio::fs::remove_dir_all(
                    core.paths.files_staging_dir(account).join(id.to_string()),
                )
                .await;
            }
            return Ok(Output::Operations(
                super::store::operations(&core.files_db, account)
                    .await
                    .map_err(|e| e.to_string())?,
            ));
        }
        if let Some(FileClient::Jmap(c)) = &mut self.client {
            c.collision = if matches!(action, "folder" | "rename" | "move" | "copy" | "copy-space")
            {
                self.collision
            } else {
                super::CollisionPolicy::Reject
            };
            c.compare_case_insensitively = self.case_insensitive;
        }
        let mutation = matches!(
            action,
            "upload"
                | "replace"
                | "folder"
                | "rename"
                | "move"
                | "copy"
                | "copy-space"
                | "delete"
                | "share"
                | "unshare"
                | "metadata"
                | "remove-property"
                | "lock"
                | "unlock"
        );
        let mut local_path = local_path;
        let job = if mutation {
            self.init(core).await?;
            let account = self.account_id().ok_or("Select an account.")?;
            core.save_file_connection_settings(account, self.settings.clone())
                .await
                .map_err(|e| e.to_string())?;
            let node = self.selected(selected).ok().and_then(|e| match e {
                Entry::Remote(n) => Some(n),
                _ => None,
            });
            let mut state = match &self.client {
                Some(FileClient::Jmap(c)) => c.state.clone(),
                _ => None,
            };
            if state.is_none()
                && !self.settings.webdav
                && let Some(space) = self.cached_space
            {
                state = core
                    .files_db
                    .read(move |c| {
                        Ok(
                            c.query_row("SELECT state FROM spaces WHERE id=?1", [space], |r| {
                                r.get::<_, Option<String>>(0)
                            })?,
                        )
                    })
                    .await
                    .map_err(|e| e.to_string())?;
            }
            let payload = json!({"a":a,"b":b,"node":node,"history":self.history,"state":state,"collision":self.collision,"caseInsensitive":self.case_insensitive});
            let id = super::store::begin(
                &core.files_db,
                account,
                self.cached_space,
                action.into(),
                payload.to_string(),
            )
            .await
            .map_err(|e| e.to_string())?;
            if matches!(action, "upload" | "replace") {
                super::store::finish(&core.files_db, id, "preparing", None)
                    .await
                    .map_err(|e| e.to_string())?;
                let staged: Result<PathBuf> = async {
                    let source = local_path.as_ref().ok_or("Choose an upload file.")?;
                    let directory = core.paths.files_staging_dir(account).join(id.to_string());
                    tokio::fs::create_dir_all(&directory)
                        .await
                        .map_err(|e| e.to_string())?;
                    let destination =
                        directory.join(source.file_name().ok_or("Invalid file name.")?);
                    files::save_cached_file(source, &destination)
                        .await
                        .map_err(|e| e.to_string())?;
                    Ok(destination)
                }
                .await;
                let destination = match staged {
                    Ok(path) => path,
                    Err(error) => {
                        super::store::finish(&core.files_db, id, "failed", Some(error.clone()))
                            .await
                            .map_err(|e| e.to_string())?;
                        return Err(error);
                    }
                };
                local_path = Some(destination.clone());
                let mut payload = payload;
                payload["stagedName"] = json!(
                    destination
                        .file_name()
                        .and_then(|v| v.to_str())
                        .ok_or("File name must be valid Unicode.")?
                );
                core.files_db
                    .write(move |c| {
                        c.execute(
                            "UPDATE operations SET payload_json=?2,state='queued' WHERE id=?1",
                            rusqlite::params![id, payload.to_string()],
                        )?;
                        Ok(())
                    })
                    .await
                    .map_err(|e| e.to_string())?;
            }
            if self.offline {
                return Ok(Output::Status(format!(
                    "Queued operation #{id}. Reconnect and review it in Transfers."
                )));
            }
            super::store::finish(&core.files_db, id, "running", None)
                .await
                .map_err(|e| e.to_string())?;
            self.active_operation = Some(id);
            Some(id)
        } else {
            None
        };
        let result = perform(self, core, action, a, b, selected, local_path).await;
        if let Some(id) = job {
            super::store::finish(
                &core.files_db,
                id,
                if result.is_ok() {
                    "completed"
                } else {
                    "uncertain"
                },
                result.as_ref().err().cloned(),
            )
            .await
            .map_err(|e| e.to_string())?;
            self.active_operation = None;
            if result.is_ok()
                && let Some(account) = self.account_id()
            {
                let _ = tokio::fs::remove_dir_all(
                    core.paths.files_staging_dir(account).join(id.to_string()),
                )
                .await;
            }
        }
        result
    }
    async fn refresh(&mut self, core: &Core, more: bool) -> Result<()> {
        let result = self.refresh_remote(core, more).await;
        if result.is_ok() {
            self.offline = false;
            return result;
        }
        let network_failure = result.as_ref().err().is_some_and(|e| {
            e.contains("Could not reach file storage")
                || e.contains("Could not discover file storage")
                || e.contains("timed out")
        });
        if !self.attachments && network_failure {
            let account = self.account_id().ok_or("Select an account.")?;
            if let Some(space) = super::store::selected_space(&core.files_db, account)
                .await
                .map_err(|e| e.to_string())?
            {
                self.cached_space = Some(space);
                core.files_db.write(move |c|{c.execute("UPDATE connections SET selected_space=(SELECT remote_id FROM spaces WHERE id=?2) WHERE account_id=?1",rusqlite::params![account,space])?;Ok(())}).await.map_err(|e|e.to_string())?;
                let mut nodes =
                    super::store::cached(&core.files_db, space, self.parent(), self.query.clone())
                        .await
                        .map_err(|e| e.to_string())?;
                self.browse.apply_local(&mut nodes);
                self.entries = nodes.into_iter().map(Entry::Remote).collect();
                self.offline = true;
                self.next = None;
                self.can_create =
                    self.query.is_empty() && self.history.last().is_some_and(|n| n.my_rights.add());
                self.quota =
                    "Offline · cached metadata; permissions are rechecked before changes.".into();
                return Ok(());
            }
        }
        result
    }
    async fn refresh_remote(&mut self, core: &Core, more: bool) -> Result<()> {
        if !more {
            self.entries.clear();
            self.next = None;
            self.before = None;
            self.query_state = None;
        }
        if self.attachments && self.server_attachments {
            if self.mail_search.is_none() {
                self.mail_search = Some(
                    core.connect_attachment_search(
                        self.account_id()
                            .ok_or("Select one account to search server mail.")?,
                    )
                    .await
                    .map_err(|e| e.to_string())?,
                );
            }
            let page = self
                .mail_search
                .as_ref()
                .unwrap()
                .search(&self.query, if more { self.next.unwrap_or(0) } else { 0 })
                .await
                .map_err(|e| e.to_string())?;
            if more && self.query_state.as_ref().is_some_and(|s| s != &page.state) {
                return Err("Mail changed during search. Refresh before loading more.".into());
            }
            self.query_state = Some(page.state);
            self.next = page.next;
            self.entries
                .extend(page.files.into_iter().map(Entry::Attachment));
            self.can_create = false;
        } else if self.attachments {
            let mut rows = core
                .attachment_files(
                    self.account_id(),
                    self.query.clone(),
                    if more { self.before } else { None },
                )
                .await
                .map_err(|e| e.to_string())?;
            let has_more = rows.len() > PAGE_SIZE;
            rows.truncate(PAGE_SIZE);
            self.before = rows.last().map(|a| a.id);
            self.next = has_more.then_some(0);
            self.entries.extend(rows.into_iter().map(Entry::Attachment));
            self.can_create = false;
        } else {
            if self.client.is_none() {
                self.connect(core).await?;
            }
            let parent = self.parent();
            let account = self.account_id().ok_or("Select an account.")?;
            let (remote, name, protocol) = match self.client.as_ref().unwrap() {
                FileClient::Jmap(c) => (
                    c.account_id.clone(),
                    c.accounts
                        .iter()
                        .find(|a| a.id == c.account_id)
                        .map(|a| a.name.clone())
                        .unwrap_or_default(),
                    "jmap",
                ),
                FileClient::Dav(c) => (c.root.clone(), c.root.clone(), "dav"),
            };
            let space = super::store::space(&core.files_db, account, remote, name, protocol)
                .await
                .map_err(|e| e.to_string())?;
            self.cached_space = Some(space);
            core.files_db.write(move |c|{c.execute("UPDATE connections SET selected_space=(SELECT remote_id FROM spaces WHERE id=?2) WHERE account_id=?1",rusqlite::params![account,space])?;Ok(())}).await.map_err(|e|e.to_string())?;
            match self.client.as_mut().unwrap() {
                FileClient::Jmap(client) => {
                    let page = client
                        .list_options(
                            parent.as_deref(),
                            &self.query,
                            if more { self.next.unwrap_or(0) } else { 0 },
                            &self.browse,
                        )
                        .await
                        .map_err(|e| e.to_string())?;
                    if more
                        && self
                            .query_state
                            .as_ref()
                            .is_some_and(|state| state != &page.query_state)
                    {
                        return Err(
                            "Storage changed during pagination. Refresh before loading more."
                                .into(),
                        );
                    }
                    self.query_state = Some(page.query_state.clone());
                    self.next = page.next_position;
                    self.can_create = self.query.is_empty()
                        && !client.read_only()
                        && self
                            .history
                            .last()
                            .map_or(client.can_create_root(), |p| p.my_rights.add());
                    self.entries
                        .extend(page.nodes.into_iter().map(Entry::Remote));
                    if self.query.is_empty() && !self.browse.filtered() && self.next.is_none() {
                        let nodes = self
                            .entries
                            .iter()
                            .filter_map(|e| match e {
                                Entry::Remote(n) => Some(n.clone()),
                                _ => None,
                            })
                            .collect();
                        super::store::replace_collection(
                            &core.files_db,
                            space,
                            parent.clone(),
                            nodes,
                            Some(page.state),
                            Some(page.query_state),
                            None,
                        )
                        .await
                        .map_err(|e| e.to_string())?;
                    }
                    if !more {
                        self.quota = client
                            .quota()
                            .await
                            .ok()
                            .and_then(|v| v["list"].as_array().cloned())
                            .map(|items| {
                                items
                                    .iter()
                                    .map(|q| {
                                        format!(
                                            "{}: {} / {} {}",
                                            q["name"].as_str().unwrap_or("Storage"),
                                            q["used"],
                                            q["hardLimit"],
                                            q["unit"].as_str().unwrap_or("")
                                        )
                                    })
                                    .collect::<Vec<_>>()
                                    .join("\n")
                            })
                            .unwrap_or_default();
                    }
                }
                FileClient::Dav(client) => {
                    let url = parent.as_deref().unwrap_or(&client.root);
                    let mut listing = client.list(url).await.map_err(|e| e.to_string())?;
                    self.can_create = self.query.is_empty() && listing.current.my_rights.add();
                    let cache_parent = parent.clone();
                    let mut nodes = listing.nodes.clone();
                    for node in &mut nodes {
                        node.parent_id = cache_parent.clone();
                    }
                    super::store::replace_collection(
                        &core.files_db,
                        space,
                        cache_parent,
                        nodes,
                        None,
                        None,
                        listing.sync_token.clone(),
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                    self.quota = match (listing.quota_used, listing.quota_available) {
                        (Some(used), Some(available)) => {
                            format!("{} used · {} available", size(used), size(available))
                        }
                        _ => String::new(),
                    };
                    self.browse.apply_local(&mut listing.nodes);
                    let tokens = self.query.to_lowercase();
                    // DAV has no standard full-text search implemented by
                    // Stalwart. Label this explicitly as current-folder search.
                    self.entries = listing
                        .nodes
                        .into_iter()
                        .filter(|n| {
                            tokens.split_whitespace().all(|s| {
                                format!("{} {}", n.name, n.media_type.as_deref().unwrap_or(""))
                                    .to_lowercase()
                                    .contains(s)
                            })
                        })
                        .map(Entry::Remote)
                        .collect();
                    self.next = None;
                }
            }
        }
        Ok(())
    }
    async fn destination(&mut self, path: &str) -> Result<Option<String>> {
        if path.is_empty() {
            return Ok(self.parent());
        }
        if !path.starts_with('/') {
            return Err(
                "Use a folder path starting with /, or leave it empty for the current folder."
                    .into(),
            );
        }
        let segments = path
            .split('/')
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>();
        if segments.len() > 64 {
            return Err("Destination path is too deep.".into());
        }
        match self.client.as_mut().ok_or("Storage is disconnected.")? {
            FileClient::Dav(client) => {
                let mut url = client.root.clone();
                for segment in segments {
                    url = client
                        .child_url(&url, segment, true)
                        .map_err(|e| e.to_string())?;
                }
                let listing = client.list(&url).await.map_err(|e| e.to_string())?;
                if !listing.current.my_rights.add() {
                    return Err("You cannot add files in this folder.".into());
                }
                Ok(Some(url))
            }
            FileClient::Jmap(client) => {
                let mut parent = None;
                for segment in segments {
                    let mut position = 0;
                    let mut found = None;
                    loop {
                        let page = client
                            .list(parent.as_deref(), "", position)
                            .await
                            .map_err(|e| e.to_string())?;
                        if let Some(node) = page
                            .nodes
                            .into_iter()
                            .find(|n| n.name == segment && n.is_directory())
                        {
                            found = Some(node);
                            break;
                        }
                        if let Some(next) = page.next_position {
                            position = next;
                        } else {
                            break;
                        }
                    }
                    let node = found
                        .ok_or_else(|| format!("Destination folder ‘{segment}’ was not found."))?;
                    if !node.my_rights.add() {
                        return Err("You cannot add files in this folder.".into());
                    }
                    parent = Some(node.id);
                }
                Ok(parent)
            }
        }
    }
}

async fn perform(
    dir: &mut FilesService,
    core: &Core,
    action: &str,
    a: &str,
    b: &str,
    selected: i32,
    local_path: Option<PathBuf>,
) -> Result<Output> {
    // Account removal invalidates an authenticated session before any action.
    let old_account = dir.account_id();
    dir.init(core).await?;
    if old_account.is_some()
        && dir.account_id().is_none()
        && !matches!(action, "account" | "account-id")
    {
        return Err("The selected account was removed. Select another account.".into());
    }
    let mut status = String::new();
    match action {
        "load" => {
            dir.init(core).await?;
            dir.settings(core).await?;
        }
        "account" | "account-id" => {
            dir.cached_space = None;
            dir.offline = false;
            let index = if action == "account-id" {
                let id = a.parse::<i64>().map_err(|_| "Invalid account.")?;
                dir.accounts
                    .iter()
                    .position(|account| account.0 == id)
                    .map(|index| index + 1)
                    .ok_or("Account no longer exists.")?
            } else {
                a.parse::<usize>().map_err(|_| "Invalid account.")?
            };
            if index > dir.accounts.len() {
                return Err("Account no longer exists.".into());
            }
            dir.account = index;
            if matches!(b, "attachments" | "storage") {
                dir.attachments = b == "attachments";
            }
            dir.client = None;
            dir.mail_search = None;
            dir.history.clear();
            dir.entries.clear();
            dir.can_create = false;
            dir.query.clear();
            dir.settings(core).await?;
        }
        "attachment-source" => {
            dir.server_attachments = a == "server";
            dir.query_state = None;
            dir.query.clear();
        }
        "scope" => {
            dir.attachments = a == "attachments";
            dir.query.clear();
        }
        "connect" => {
            dir.attachments = false;
            dir.history.clear();
            dir.entries.clear();
            dir.cached_space = None;
            dir.settings = ConnectionSettings {
                endpoint: a.into(),
                webdav: b == "dav",
            };
            dir.connect(core).await?;
            core.save_file_connection_settings(
                dir.account_id().ok_or("Select an account.")?,
                dir.settings.clone(),
            )
            .await
            .map_err(|e| e.to_string())?;
        }
        "space" if dir.offline => {
            let index = a.parse::<usize>().map_err(|_| "Invalid storage space.")?;
            let (id, _, remote) = dir
                .cached_spaces
                .get(index)
                .cloned()
                .ok_or("Storage space is unavailable offline.")?;
            let account = dir.account_id().ok_or("Select an account.")?;
            core.files_db
                .write(move |c| {
                    c.execute(
                        "UPDATE connections SET selected_space=?2 WHERE account_id=?1",
                        rusqlite::params![account, remote],
                    )?;
                    Ok(())
                })
                .await
                .map_err(|e| e.to_string())?;
            dir.cached_space = Some(id);
            dir.client = None;
            dir.history.clear();
            dir.query.clear();
        }
        "space" => {
            let FileClient::Jmap(client) = dir.client.as_mut().ok_or("Storage is disconnected.")?
            else {
                return Err("Shared WebDAV spaces use their own connection URL.".into());
            };
            let index = a.parse::<usize>().map_err(|_| "Invalid storage space.")?;
            let id = client
                .accounts
                .get(index)
                .ok_or("Storage space is unavailable.")?
                .id
                .clone();
            client.select_account(&id).map_err(|e| e.to_string())?;
            dir.history.clear();
            dir.query.clear();
        }
        "index-text" => {
            let count = core
                .index_cached_attachment_text(dir.account_id())
                .await
                .map_err(|e| e.to_string())?;
            status = format!(
                "Indexed {count} cached text attachments. Text, JSON and XML up to 1 MiB are supported; other formats remain searchable by metadata."
            );
        }
        "filter" => {
            let mut options: super::jmap::BrowseOptions =
                serde_json::from_str(a).map_err(|_| "Invalid file filters.")?;
            if !["name", "size", "nodeType"].contains(&options.sort.as_str()) {
                return Err("Unsupported file sorting.".into());
            }
            if options
                .node_type
                .as_deref()
                .is_some_and(|s| !["file", "directory"].contains(&s))
            {
                return Err("Unsupported file kind.".into());
            }
            if options
                .min_size
                .zip(options.max_size)
                .is_some_and(|(a, b)| a > b)
            {
                return Err("Minimum size exceeds maximum size.".into());
            }
            if options.node_type.as_deref() == Some("directory") {
                options.min_size = None;
                options.max_size = None;
            }
            dir.browse = options;
        }
        "search" => {
            if a.len() > 1024 {
                return Err("Search is too long.".into());
            }
            dir.query = a.into();
        }
        "up" => {
            dir.history.pop();
            dir.query.clear();
        }
        "enter" => {
            let Entry::Remote(node) = dir.selected(selected)? else {
                return Err("Select a folder.".into());
            };
            if !node.is_directory() {
                return Err("Select a folder.".into());
            }
            if dir.history.len() >= 64 {
                return Err("Folder path is too deep.".into());
            }
            dir.history.push(node);
            dir.query.clear();
        }
        "notifications" | "clear-notifications" => {
            let Some(FileClient::Jmap(client)) = dir.client.as_ref() else {
                return Err("Sharing notifications require JMAP storage.".into());
            };
            if action == "clear-notifications" && !dir.notification_ids.is_empty() {
                client
                    .dismiss_share_notifications(
                        &dir.notification_ids,
                        dir.notification_state
                            .as_deref()
                            .ok_or("Reload sharing activity before clearing it.")?,
                    )
                    .await
                    .map_err(|e| e.to_string())?;
            }
            let result = client
                .share_notifications()
                .await
                .map_err(|e| e.to_string())?;
            dir.notification_state = result["state"].as_str().map(str::to_owned);
            dir.notification_ids = result["list"]
                .as_array()
                .ok_or("Invalid sharing notifications.")?
                .iter()
                .filter_map(|n| n["id"].as_str().map(str::to_owned))
                .collect();
            let messages = result["list"]
                .as_array()
                .ok_or("Invalid sharing notifications.")?
                .iter()
                .map(|n| {
                    format!(
                        "{}\n{} · {}\n{}",
                        n["name"].as_str().unwrap_or("File"),
                        n["changedBy"]["name"].as_str().unwrap_or(""),
                        n["created"].as_str().unwrap_or(""),
                        if n["newRights"].is_null() {
                            "Access removed"
                        } else {
                            "Permissions updated"
                        }
                    )
                })
                .collect::<Vec<_>>();
            return Ok(Output::Preview(Preview::Text(if messages.is_empty() {
                "No sharing notifications.".into()
            } else {
                messages.join("\n\n")
            })));
        }
        "poll" => {
            if dir.attachments {
                return Ok(Output::Status(String::new()));
            }
            if let Some(FileClient::Jmap(client)) = dir.client.as_ref()
                && let Some(state) = client.state.as_deref()
                && let Ok(changes) = client.changes(state).await
                && ["created", "updated", "destroyed"]
                    .iter()
                    .all(|k| changes[*k].as_array().is_some_and(Vec::is_empty))
            {
                return Ok(Output::Status(format!("{} items", dir.entries.len())));
            }
        }
        "preview" | "download" | "details" | "keep-offline" | "release-offline" => {
            let entry = dir.selected(selected)?;
            if action == "details" {
                let mut text = details(&entry);
                if let (Entry::Remote(node), Some(FileClient::Dav(c))) = (&entry, &dir.client)
                    && !dir.offline
                {
                    text.push_str("\n\nWebDAV properties:\n");
                    text.push_str(
                        &c.properties(&node.id)
                            .await
                            .map_err(|e| e.to_string())?
                            .describe(),
                    );
                }
                return Ok(Output::Preview(Preview::Text(text)));
            }
            if action == "release-offline" {
                let Entry::Remote(node) = &entry else {
                    return Err("Attachment caching is managed by Mail.".into());
                };
                super::cache::pin(
                    core,
                    dir.cached_space.ok_or("Refresh storage.")?,
                    node.id.clone(),
                    false,
                )
                .await
                .map_err(|e| e.to_string())?;
                return Ok(Output::Status(
                    "Offline copy may now be removed when cache space is needed.".into(),
                ));
            }
            if action == "keep-offline" {
                match &entry {
                    Entry::Attachment(a) => {
                        core.attachment_file_content(a)
                            .await
                            .map_err(|e| e.to_string())?;
                    }
                    Entry::Remote(n) => {
                        super::cache::content(
                            core,
                            dir.account_id().ok_or("Select an account.")?,
                            dir.cached_space.ok_or("Refresh storage.")?,
                            n,
                            dir.client.as_ref(),
                            dir.offline,
                        )
                        .await
                        .map_err(|e| e.to_string())?;
                    }
                }
                if let Entry::Remote(node) = &entry {
                    super::cache::pin(
                        core,
                        dir.cached_space.ok_or("Refresh storage.")?,
                        node.id.clone(),
                        true,
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                }
                return Ok(Output::Status("File is available offline.".into()));
            }
            let destination = if action == "download" {
                Some(local_path.ok_or("Choose a download destination.")?)
            } else {
                None
            };
            if let Some(path) = destination {
                match &entry {
                    Entry::Attachment(a) => {
                        let source = core
                            .attachment_file_content(a)
                            .await
                            .map_err(|e| e.to_string())?;
                        files::save_cached_file(Path::new(&source), &path)
                            .await
                            .map_err(|e| e.to_string())?;
                    }
                    Entry::Remote(n) => {
                        let source = super::cache::content(
                            core,
                            dir.account_id().ok_or("Select an account.")?,
                            dir.cached_space.ok_or("Refresh storage.")?,
                            n,
                            dir.client.as_ref(),
                            dir.offline,
                        )
                        .await
                        .map_err(|e| e.to_string())?;
                        files::save_cached_file(&source, &path)
                            .await
                            .map_err(|e| e.to_string())?
                    }
                }
                return Ok(Output::Status("File downloaded.".into()));
            }
            let data = match &entry {
                Entry::Attachment(a) => {
                    let path = core
                        .attachment_file_content(a)
                        .await
                        .map_err(|e| e.to_string())?;
                    read_bounded(Path::new(&path), 16 * 1024 * 1024).await?
                }
                Entry::Remote(n) => {
                    if n.size.is_some_and(|n| n > 16 * 1024 * 1024) {
                        return Ok(Output::Preview(Preview::Text(
                            "This file is too large to preview. Download it to view locally."
                                .into(),
                        )));
                    }
                    let path = super::cache::content(
                        core,
                        dir.account_id().ok_or("Select an account.")?,
                        dir.cached_space.ok_or("Refresh storage.")?,
                        n,
                        dir.client.as_ref(),
                        dir.offline,
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                    read_bounded(&path, 16 * 1024 * 1024).await?
                }
            };
            let media = match entry {
                Entry::Attachment(a) => a.media_type,
                Entry::Remote(n) => n.media_type.unwrap_or_default(),
            };
            return Ok(Output::Preview(Preview::Data(data, media)));
        }
        "folder" => {
            if !dir.can_create {
                return Err("You cannot create files in this folder.".into());
            }
            let parent = dir.parent();
            match dir.client.as_mut().ok_or("Storage is disconnected.")? {
                FileClient::Jmap(c) => {
                    c.create_folder(parent.as_deref(), a)
                        .await
                        .map_err(|e| e.to_string())?;
                }
                FileClient::Dav(c) => {
                    c.create_folder(parent.as_deref().unwrap_or(&c.root), a)
                        .await
                        .map_err(|e| e.to_string())?;
                }
            }
            status = "Folder created.".into();
        }
        "upload" | "replace" => {
            let node = if action == "replace" {
                match dir.selected(selected)? {
                    Entry::Remote(n) if n.my_rights.modify() && !n.is_directory() => Some(n),
                    _ => return Err("This file cannot be replaced.".into()),
                }
            } else {
                if !dir.can_create {
                    return Err("You cannot upload into this folder.".into());
                }
                None
            };
            let path = local_path.ok_or("Choose a file to upload.")?;
            let name = node.as_ref().map(|n| n.name.clone()).unwrap_or_else(|| {
                safe_name(&path.file_name().unwrap_or_default().to_string_lossy())
            });
            let parent = dir.parent();
            match dir.client.as_mut().ok_or("Storage is disconnected.")? {
                FileClient::Jmap(c) => {
                    c.upload(
                        parent.as_deref(),
                        &name,
                        &path,
                        node.as_ref().map(|n| n.id.as_str()),
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                }
                FileClient::Dav(c) => c
                    .upload(
                        parent.as_deref().unwrap_or(&c.root),
                        &name,
                        &path,
                        node.as_ref(),
                    )
                    .await
                    .map_err(|e| e.to_string())?,
            }
            status = "File uploaded.".into();
        }
        "copy-space" => {
            let Entry::Remote(node) = dir.selected(selected)? else {
                return Err("Select a storage file or folder.".into());
            };
            if !node.my_rights.may_read {
                return Err("You cannot read this file.".into());
            }
            let (space, path) = b.split_once('|').ok_or("Invalid destination.")?;
            let index = space
                .parse::<usize>()
                .map_err(|_| "Invalid storage space.")?;
            let Some(FileClient::Jmap(source)) = dir.client.as_ref() else {
                return Err("Copy between storage spaces requires JMAP.".into());
            };
            let target = source
                .accounts
                .get(index)
                .ok_or("Storage space is unavailable.")?
                .id
                .clone();
            let source_id = source.account_id.clone();
            let source_state = source.state.clone().ok_or("Refresh before copying.")?;
            if target == source_id {
                let parent = dir.destination(path).await?;
                let Some(FileClient::Jmap(client)) = dir.client.as_mut() else {
                    unreachable!()
                };
                client.state = Some(source_state);
                client
                    .copy(&node.id, parent.as_deref(), a)
                    .await
                    .map_err(|e| e.to_string())?;
            } else {
                let client = core
                    .connect_files(dir.account_id().ok_or("Select an account.")?, &dir.settings)
                    .await
                    .map_err(|e| e.to_string())?;
                let mut destination = FilesService {
                    client: Some(client),
                    ..Default::default()
                };
                let Some(FileClient::Jmap(client)) = destination.client.as_mut() else {
                    return Err("JMAP connection required.".into());
                };
                client.select_account(&target).map_err(|e| e.to_string())?;
                client.list(None, "", 0).await.map_err(|e| e.to_string())?;
                let parent = destination.destination(path).await?;
                let Some(FileClient::Jmap(client)) = destination.client.as_mut() else {
                    unreachable!()
                };
                client.collision = dir.collision;
                client.compare_case_insensitively = dir.case_insensitive;
                client
                    .copy_from_account(&source_id, &source_state, &node.id, parent.as_deref(), a)
                    .await
                    .map_err(|e| e.to_string())?;
            }
            status = "File copied to the selected storage space.".into();
        }
        "rename" | "move" | "copy" | "delete" | "share" | "unshare" | "lock" | "unlock"
        | "metadata" | "remove-property" => {
            let Entry::Remote(node) = dir.selected(selected)? else {
                return Err("Email attachments cannot be changed in storage.".into());
            };
            match action {
                "metadata" | "remove-property" if !node.my_rights.modify() => {
                    return Err("You cannot change this file’s properties.".into());
                }
                "rename" | "move" if !node.my_rights.rename() => {
                    return Err("You cannot move or rename this file.".into());
                }
                "delete" if !node.my_rights.delete() => {
                    return Err("You cannot delete this file.".into());
                }
                "share" | "unshare" if !node.my_rights.may_share => {
                    return Err("You cannot change sharing for this file.".into());
                }
                _ => {}
            }
            // Resolving the destination must not replace the optimistic state
            // from the displayed source list with a newer state.
            let old_state = match dir.client.as_ref() {
                Some(FileClient::Jmap(c)) => c.state.clone(),
                _ => None,
            };
            let parent = if matches!(action, "move" | "copy") {
                dir.destination(b).await?
            } else {
                dir.parent()
            };
            match dir.client.as_mut().ok_or("Storage is disconnected.")? {
                FileClient::Jmap(c) => {
                    c.state = old_state;
                    match action {
                        "metadata"=> {
                            let dates=a.split('|').collect::<Vec<_>>();
                            if dates.len()!=3 {return Err("Invalid dates.".into());}
                            let mut patch=json!({});
                            if !node.is_directory() {patch["executable"]=json!(b.contains('x'));}
                            for (key,date) in ["created","modified","accessed"].iter().zip(dates) {
                                if !date.is_empty() {
                                    let parsed=chrono::DateTime::parse_from_rfc3339(date).map_err(|_|format!("Enter a valid {key} date with its UTC offset."))?;
                                    patch[*key]=json!(parsed.with_timezone(&chrono::Utc).to_rfc3339_opts(chrono::SecondsFormat::Secs,true));
                                }
                            }
                            c.update(&node.id,patch).await
                        }
                        "rename"=>{c.update(&node.id,json!({"name":a})).await},
                        "move"=>{c.update(&node.id,json!({"name":a,"parentId":parent})).await},
                        "copy"=>{c.copy(&node.id,parent.as_deref(),a).await},
                        "delete"=>{c.delete(&node.id,true).await},
                        "share"|"unshare"=> {
                            if a.is_empty() || a.len()>256 {return Err("Enter a recipient account ID.".into());}
                            let recipient=c.resolve_principal(a).await.map_err(|e|e.to_string())?;
                            let key=format!("shareWith/{}",recipient.replace('~',"~0").replace('/',"~1"));
                            let rights=if action=="unshare" {serde_json::Value::Null} else if c.modern() {json!({"mayRead":true,"mayAddChildren":b.contains('a'),"mayRename":b.contains('r'),"mayDelete":b.contains('d'),"mayModifyContent":b.contains('w'),"mayShare":b.contains('s')})} else {json!({"mayRead":true,"mayWrite":b.chars().any(|c|"ward".contains(c)),"mayShare":b.contains('s')})};
                            c.update(&node.id,json!({key:rights})).await
                        }
                        _=>return Err("Locks are available through WebDAV.".into())
                    }.map_err(|e|e.to_string())?;
                }
                FileClient::Dav(c) => match action {
                    "rename" | "move" | "copy" => {
                        c.relocate_with_policy(
                            &node,
                            parent.as_deref().unwrap_or(&c.root),
                            a,
                            action == "copy",
                            dir.collision,
                        )
                        .await
                    }
                    "delete" => c.delete(&node).await,
                    "metadata" | "remove-property" => {
                        let (namespace, name) = b
                            .split_once('|')
                            .unwrap_or(("urn:flectar:files", "description"));
                        c.patch_property(
                            &node,
                            namespace,
                            name,
                            if action == "remove-property" {
                                None
                            } else {
                                Some(a)
                            },
                        )
                        .await
                    }
                    "share" | "unshare" => {
                        let mut privileges = vec!["read"];
                        for (flag, right) in [
                            ('w', "write-content"),
                            ('p', "write-properties"),
                            ('a', "bind"),
                            ('d', "unbind"),
                            ('s', "write-acl"),
                        ] {
                            if b.contains(flag) {
                                privileges.push(right);
                            }
                        }
                        c.share_privileges(&node, a, &privileges, action == "unshare")
                            .await
                    }
                    "lock" => {
                        c.lock_with_options(
                            &node,
                            b.contains('s'),
                            b.contains('r'),
                            a.parse().unwrap_or(300),
                        )
                        .await
                    }
                    "unlock" => c.unlock(&node).await,
                    _ => unreachable!(),
                }
                .map_err(|e| e.to_string())?,
            }
            status = "File updated.".into();
        }
        "refresh" | "more" => {}
        _ => return Err("Unknown file action.".into()),
    }
    dir.refresh(core, action == "more").await?;
    if status.is_empty() {
        status = if dir.attachments {
            format!("{} attachments · synchronized mail", dir.entries.len())
        } else if dir.settings.webdav {
            format!("{} items · search covers this folder", dir.entries.len())
        } else {
            format!("{} items", dir.entries.len())
        };
    }
    Ok(Output::Status(status))
}
async fn read_bounded(path: &Path, limit: usize) -> Result<Vec<u8>> {
    use tokio::io::AsyncReadExt;
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|e| e.to_string())?;
    let mut data = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut data)
        .await
        .map_err(|e| e.to_string())?;
    if data.len() > limit {
        return Err("File exceeds the transfer limit.".into());
    }
    Ok(data)
}
fn size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024. * 1024.))
    }
}
fn safe_name(name: &str) -> String {
    if files::validate_name(name).is_ok() {
        name.into()
    } else {
        "download".into()
    }
}
fn details(entry: &Entry) -> String {
    match entry {
        Entry::Attachment(a) => format!(
            "{}\n{} · {}\n\nFrom: {}\nSubject: {}\n\n{}",
            a.filename,
            a.media_type,
            size(a.size),
            a.sender,
            a.subject,
            if a.cached {
                "Available offline"
            } else {
                "Downloads on demand"
            }
        ),
        Entry::Remote(n) => format!(
            "{}\n{} · {}\n\nCreated: {}\nModified: {}\nAccessed: {}\nRole: {}\nExecutable: {}\nSubscribed: {}\n\nSharing:\n{}",
            n.name,
            n.media_type.as_deref().unwrap_or("Folder"),
            n.size.map(size).unwrap_or_default(),
            n.created.as_deref().unwrap_or("—"),
            n.modified.as_deref().unwrap_or("—"),
            n.accessed.as_deref().unwrap_or("—"),
            n.role.as_deref().unwrap_or("—"),
            n.executable,
            n.is_subscribed.unwrap_or(true),
            n.share_with
                .as_ref()
                .map(|v| serde_json::to_string_pretty(v).unwrap_or_default())
                .unwrap_or_else(|| "No sharing details returned".into())
        ),
    }
}
