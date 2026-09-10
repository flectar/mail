//! Read-only JMAP Mail attachment search across server history. Results remain
//! transient, so changing a Files search never widens Mail's synchronization
//! window or inserts duplicate messages into its projection.
use super::{
    AttachmentFile, PAGE_SIZE, err,
    transport::{Transport, expand, ordered_records},
};
use crate::{
    Core,
    accounts::credentials::{self, Slot},
    error::Result,
    models::AuthKind,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
const MAIL: &str = "urn:ietf:params:jmap:mail";
#[derive(Clone, Debug)]
pub struct RemoteAttachment {
    pub account: String,
    pub email: String,
    pub blob: String,
}
pub struct MailSearch {
    transport: Transport,
    session: Value,
    account: String,
    local_account: i64,
}
pub struct Page {
    pub files: Vec<AttachmentFile>,
    pub next: Option<usize>,
    pub state: String,
}
impl Core {
    pub async fn connect_attachment_search(&self, account: i64) -> Result<MailSearch> {
        let config = self
            .list_account_configs()
            .await?
            .into_iter()
            .find(|a| a.id == account)
            .ok_or_else(|| err("Account was removed."))?;
        if config.auth_kind != AuthKind::Password {
            return Err(err(
                "Server attachment search requires a JMAP password or application-password account.",
            ));
        }
        let secret =
            credentials::load_async(self.credentials.clone(), account, Slot::Password).await?;
        let base = crate::jmap::client::normalize_base_url(&config.jmap_url, &config.email)?;
        let transport = Transport::new(
            &base,
            if config.username.is_empty() {
                &config.email
            } else {
                &config.username
            },
            &secret,
        )?;
        let session = transport
            .discover(&format!("{base}/.well-known/jmap"))
            .await?;
        MailSearch::from_session(
            transport,
            session,
            account,
            config.jmap_account_id.as_deref(),
        )
    }
    pub async fn attachment_file_content(&self, file: &AttachmentFile) -> Result<PathBuf> {
        let Some(remote) = &file.remote else {
            return Ok(PathBuf::from(self.get_attachment(file.id).await?));
        };
        if !self
            .list_account_configs()
            .await?
            .iter()
            .any(|a| a.id == file.account_id)
        {
            return Err(err("The account was removed."));
        }
        // Resolve to the existing Mail attachment cache whenever synchronized.
        let account = file.account_id;
        let email = remote.email.clone();
        let blob = remote.blob.clone();
        let local=self.db.read(move |c| {use rusqlite::OptionalExtension;Ok(c.query_row("SELECT a.id FROM attachments a JOIN messages m ON m.id=a.message_id WHERE m.account_id=?1 AND m.jmap_id=?2 AND a.jmap_blob_id=?3",rusqlite::params![account,email,blob],|r|r.get::<_,i64>(0)).optional()?)}).await?;
        if let Some(id) = local {
            return Ok(PathBuf::from(self.get_attachment(id).await?));
        }
        let key = format!(
            "{:x}",
            Sha256::digest(format!("{}\0{}", remote.account, remote.blob))
        );
        let directory = self.paths.attachments_dir(file.account_id).join("remote");
        tokio::fs::create_dir_all(&directory).await?;
        let path = directory.join(key);
        if tokio::fs::try_exists(&path).await? {
            return Ok(path);
        }
        let client = self.connect_attachment_search(file.account_id).await?;
        if client.account != remote.account {
            return Err(err("The mail account changed. Repeat the server search."));
        }
        // Remote search downloads share Mail's cache directory. Bound this
        // reconstructable area and remove oldest entries before another transfer.
        let mut entries = tokio::fs::read_dir(&directory).await?;
        let mut cached = Vec::new();
        let mut total = 0u64;
        while let Some(entry) = entries.next_entry().await? {
            if !entry.file_type().await?.is_file() {
                continue;
            }
            let metadata = entry.metadata().await?;
            total = total.saturating_add(metadata.len());
            cached.push((metadata.modified().ok(), entry.path(), metadata.len()));
        }
        cached.sort_by_key(|(time, _, _)| *time);
        for (_, old, size) in cached {
            if total <= 1536 * 1024 * 1024 {
                break;
            }
            tokio::fs::remove_file(old).await?;
            total = total.saturating_sub(size);
        }
        let url = expand(
            client.session["downloadUrl"]
                .as_str()
                .ok_or_else(|| err("Missing mail download URL."))?,
            &[
                ("accountId", &remote.account),
                ("blobId", &remote.blob),
                ("name", &file.filename),
                ("type", &file.media_type),
            ],
        )?;
        let response = client
            .transport
            .send(client.transport.request("GET", &url)?)
            .await?;
        super::save_response(&path, response).await?;
        Ok(path)
    }
}
impl MailSearch {
    fn from_session(
        transport: Transport,
        session: Value,
        local_account: i64,
        preferred: Option<&str>,
    ) -> Result<Self> {
        if session["capabilities"].get(MAIL).is_none() {
            return Err(err("This server does not advertise JMAP Mail."));
        }
        let account = preferred
            .or_else(|| session["primaryAccounts"][MAIL].as_str())
            .ok_or_else(|| err("No mail account is available."))?
            .to_owned();
        if session["accounts"][&account]["accountCapabilities"]
            .get(MAIL)
            .is_none()
        {
            return Err(err("The selected account does not support JMAP Mail."));
        }
        let api = session["apiUrl"]
            .as_str()
            .ok_or_else(|| err("Missing mail API URL."))?;
        let _ = transport.request("POST", api)?;
        Ok(Self {
            transport,
            session,
            account,
            local_account,
        })
    }
    async fn call(&self, method: &str, mut args: Value) -> Result<Value> {
        args["accountId"] = json!(self.account);
        let api = self.session["apiUrl"]
            .as_str()
            .ok_or_else(|| err("Missing mail API URL."))?;
        let response=self.transport.json(self.transport.request("POST",api)?.json(&json!({"using":["urn:ietf:params:jmap:core",MAIL],"methodCalls":[[method,args,"attachments"]]}))).await?;
        let row = response["methodResponses"]
            .as_array()
            .and_then(|rows| rows.iter().find(|r| r[2] == "attachments"))
            .ok_or_else(|| err("Missing mail search response."))?;
        if row[0] != method || row[1]["accountId"] != self.account {
            return Err(err(format!(
                "Mail search failed: {}",
                row[1]["type"].as_str().unwrap_or("invalid response")
            )));
        }
        Ok(row[1].clone())
    }
    /// RFC 8621 text includes indexed attachment text supported by the server.
    /// All attachments of matching messages are returned; no claim is made
    /// that every individual attachment contains the query.
    pub async fn search(&self, text: &str, position: usize) -> Result<Page> {
        if text.len() > 1024 {
            return Err(err("Search is too long."));
        }
        let mut filter = json!({"hasAttachment":true});
        if !text.is_empty() {
            filter["text"] = json!(text);
        }
        let query=self.call("Email/query",json!({"filter":filter,"position":position,"limit":PAGE_SIZE,"sort":[{"property":"receivedAt","isAscending":false}],"collapseThreads":false})).await?;
        let ids: Vec<String> = serde_json::from_value(query["ids"].clone())
            .map_err(|_| err("Invalid mail search IDs."))?;
        if ids.len() > PAGE_SIZE {
            return Err(err("Mail server exceeded the requested page limit."));
        }
        let state = query["queryState"]
            .as_str()
            .ok_or_else(|| err("Missing mail query state."))?
            .to_owned();
        let mut files = Vec::new();
        let max = self.session["capabilities"]["urn:ietf:params:jmap:core"]["maxObjectsInGet"]
            .as_u64()
            .unwrap_or(100)
            .clamp(1, 100) as usize;
        for chunk in ids.chunks(max) {
            let result=self.call("Email/get",json!({"ids":chunk,"properties":["id","from","subject","receivedAt","attachments"]})).await?;
            let list = result["list"]
                .as_array()
                .ok_or_else(|| err("Invalid mail search results."))?;
            for email in ordered_records(chunk, list.clone(), |email| {
                email["id"].as_str().unwrap_or("")
            })? {
                let id = email["id"]
                    .as_str()
                    .ok_or_else(|| err("Missing email identifier."))?;
                let sender = email["from"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|a| {
                        format!(
                            "{} {}",
                            a["name"].as_str().unwrap_or(""),
                            a["email"].as_str().unwrap_or("")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                for part in email["attachments"].as_array().into_iter().flatten() {
                    let Some(blob) = part["blobId"].as_str() else {
                        continue;
                    };
                    let name = part["name"].as_str().unwrap_or("attachment");
                    if part["disposition"] == "inline"
                        && part["name"].as_str().is_none_or(str::is_empty)
                    {
                        continue;
                    }
                    files.push(AttachmentFile {
                        id: 0,
                        account_id: self.local_account,
                        thread_id: None,
                        filename: name.into(),
                        media_type: part["type"]
                            .as_str()
                            .unwrap_or("application/octet-stream")
                            .into(),
                        size: part["size"].as_u64().unwrap_or(0),
                        sender: sender.clone(),
                        subject: email["subject"].as_str().unwrap_or("").into(),
                        date: email["receivedAt"]
                            .as_str()
                            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                            .map(|d| d.timestamp_millis())
                            .unwrap_or(0),
                        cached: false,
                        remote: Some(RemoteAttachment {
                            account: self.account.clone(),
                            email: id.into(),
                            blob: blob.into(),
                        }),
                    });
                }
            }
        }
        Ok(Page {
            files,
            next: (ids.len() == PAGE_SIZE).then_some(position + ids.len()),
            state,
        })
    }
}
