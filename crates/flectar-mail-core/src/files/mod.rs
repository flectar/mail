//! File storage and a paged, account-scoped attachment library.
//! Remote credentials stay in the core and are bound to the configured origin.
pub mod cache;
pub mod dav;
pub mod jmap;
pub mod mail_search;
pub mod progress;
#[cfg(test)]
#[path = "tests.rs"]
mod protocol_tests;
pub mod service;
#[cfg(test)]
mod storage_tests;
pub mod store;
pub mod sync;
mod text_index;
mod transport;

use crate::{
    Core,
    accounts::credentials::{self, Slot},
    error::{CoreError, Result},
    models::AuthKind,
};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CollisionPolicy {
    #[default]
    Reject,
    Rename,
    Replace,
    Newest,
}

pub const PAGE_SIZE: usize = 100;
pub const MAX_TRANSFER: usize = 512 * 1024 * 1024;

pub(crate) fn err(message: impl Into<String>) -> CoreError {
    CoreError::Other(message.into())
}

/// Only a leaf name: never allow remote names to become local paths.
pub fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.len() > 255
        || name
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '\\'))
    {
        return Err(err(
            "Enter a file name of 1–255 bytes without slashes or control characters.",
        ));
    }
    Ok(())
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rights {
    #[serde(default)]
    pub may_read: bool,
    #[serde(default)]
    pub may_add_children: bool,
    #[serde(default)]
    pub may_rename: bool,
    #[serde(default)]
    pub may_delete: bool,
    #[serde(default)]
    pub may_modify_content: bool,
    #[serde(default)]
    pub may_share: bool,
    // Older Stalwart FileNode drafts use one write right.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub may_write: bool,
}
impl Rights {
    /// FileNode ACLs are additive and inherited by descendants.
    pub(crate) fn inherit(&mut self, parent: &Self) {
        self.may_read |= parent.may_read;
        self.may_add_children |= parent.may_add_children;
        self.may_rename |= parent.may_rename;
        self.may_delete |= parent.may_delete;
        self.may_modify_content |= parent.may_modify_content;
        self.may_share |= parent.may_share;
        self.may_write |= parent.may_write;
    }
    pub fn add(&self) -> bool {
        self.may_add_children || self.may_write
    }
    pub fn rename(&self) -> bool {
        self.may_rename || self.may_write
    }
    pub fn delete(&self) -> bool {
        self.may_delete || self.may_write
    }
    pub fn modify(&self) -> bool {
        self.may_modify_content || self.may_write
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileNode {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub blob_id: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default, rename = "type")]
    pub media_type: Option<String>,
    #[serde(default)]
    pub node_type: Option<String>,
    #[serde(default)]
    pub modified: Option<String>,
    #[serde(default)]
    pub created: Option<String>,
    #[serde(default)]
    pub accessed: Option<String>,
    #[serde(default, deserialize_with = "nullable_bool")]
    pub executable: bool,
    #[serde(default)]
    pub is_subscribed: Option<bool>,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub my_rights: Rights,
    #[serde(default)]
    pub share_with: Option<serde_json::Value>,
    /// WebDAV opaque strong validator; never manufacture one.
    #[serde(default)]
    pub etag: Option<String>,
    #[serde(default)]
    pub locked: bool,
}
fn nullable_bool<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<bool, D::Error> {
    Ok(Option::<bool>::deserialize(deserializer)?.unwrap_or(false))
}

impl FileNode {
    pub fn is_directory(&self) -> bool {
        self.node_type
            .as_deref()
            .map_or(self.blob_id.is_none(), |kind| kind == "directory")
    }
}

#[derive(Clone, Debug)]
pub struct AttachmentFile {
    pub remote: Option<mail_search::RemoteAttachment>,
    pub id: i64,
    pub account_id: i64,
    pub thread_id: Option<i64>,
    pub filename: String,
    pub media_type: String,
    pub size: u64,
    pub sender: String,
    pub subject: String,
    pub date: i64,
    pub cached: bool,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ConnectionSettings {
    pub endpoint: String,
    pub webdav: bool,
}

fn attachment_match_query(query: &str) -> String {
    query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .take(32)
        .map(|s| format!("\"{s}\"*"))
        .collect::<Vec<_>>()
        .join(" AND ")
}

fn attachment_page(
    conn: &rusqlite::Connection,
    account: Option<i64>,
    query: &str,
    before: Option<i64>,
) -> Result<Vec<AttachmentFile>> {
    let mut stmt = conn.prepare(
                "SELECT a.id, m.account_id, m.thread_id, COALESCE(NULLIF(a.filename,''),'attachment'),
                 COALESCE(a.mime_type,'application/octet-stream'), COALESCE(a.size,0),
                 trim(COALESCE(m.from_name,'') || ' ' || COALESCE(m.from_addr,'')), m.subject, m.date,
                 a.file_path IS NOT NULL FROM attachments a JOIN messages m ON m.id=a.message_id
                 WHERE (?1 IS NULL OR m.account_id=?1) AND (?2 IS NULL OR a.id < ?2)
                 AND (a.is_inline=0 OR length(COALESCE(a.filename,''))>0)
                 AND (?3='' OR a.id IN (SELECT rowid FROM attachment_files_fts WHERE attachment_files_fts MATCH ?3) OR a.id IN (SELECT rowid FROM attachment_text_fts WHERE attachment_text_fts MATCH ?3))
                 ORDER BY a.id DESC LIMIT ?4")?;
    Ok(stmt
        .query_map(
            params![account, before, query, (PAGE_SIZE + 1) as i64],
            |r| {
                Ok(AttachmentFile {
                    remote: None,
                    id: r.get(0)?,
                    account_id: r.get(1)?,
                    thread_id: r.get(2)?,
                    filename: r.get(3)?,
                    media_type: r.get(4)?,
                    size: r.get::<_, i64>(5)?.max(0) as u64,
                    sender: r.get(6)?,
                    subject: r.get(7)?,
                    date: r.get(8)?,
                    cached: r.get(9)?,
                })
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

impl Core {
    /// SQL pagination never hydrates message bodies or marks messages read.
    /// Includes named inline parts, but hides unnamed signature/CID images.
    pub async fn attachment_files(
        &self,
        account: Option<i64>,
        query: String,
        before: Option<i64>,
    ) -> Result<Vec<AttachmentFile>> {
        if query.len() > 1024 {
            return Err(err("Search is too long."));
        }
        let query = attachment_match_query(&query);
        self.db
            .read(move |conn| attachment_page(conn, account, &query, before))
            .await
    }
    pub async fn file_connection_settings(&self, account: i64) -> Result<ConnectionSettings> {
        self.files_db
            .read(move |conn| {
                let value: Option<String> = conn
                    .query_row(
                        "SELECT settings_json FROM connections WHERE account_id=?1",
                        [account],
                        |r| r.get(0),
                    )
                    .optional()?;
                Ok(value
                    .map(|v| serde_json::from_str(&v))
                    .transpose()?
                    .unwrap_or_default())
            })
            .await
    }
    pub async fn save_file_connection_settings(
        &self,
        account: i64,
        settings: ConnectionSettings,
    ) -> Result<()> {
        let value = serde_json::to_string(&settings)?;
        self.files_db.write(move |conn| {
            let tx=conn.transaction()?;
            let old:Option<String>=tx.query_row("SELECT settings_json FROM connections WHERE account_id=?1",[account],|r|r.get(0)).optional()?;
            if old.as_ref().is_some_and(|old|old!=&value) {
                tx.execute("UPDATE operations SET state='cancelled',error='Storage connection changed. Select the source and destination again.' WHERE account_id=?1 AND state IN ('preparing','queued')",[account])?;
                tx.execute("DELETE FROM spaces WHERE account_id=?1",[account])?;
                tx.execute("UPDATE connections SET selected_space=NULL WHERE account_id=?1",[account])?;
            }
            tx.execute("INSERT INTO connections(account_id,settings_json,updated_at) VALUES(?1,?2,?3) ON CONFLICT(account_id) DO UPDATE SET settings_json=excluded.settings_json,updated_at=excluded.updated_at",params![account,value,chrono::Utc::now().timestamp_millis()])?;
            tx.commit()?; Ok(())
        }).await
    }
    pub async fn connect_files(
        &self,
        account: i64,
        settings: &ConnectionSettings,
    ) -> Result<FileClient> {
        let config = self
            .list_account_configs()
            .await?
            .into_iter()
            .find(|a| a.id == account)
            .ok_or_else(|| err("Account was removed."))?;
        if config.auth_kind != AuthKind::Password {
            return Err(err(
                "Connect storage using a Stalwart password or application-password account.",
            ));
        }
        let secret =
            credentials::load_async(self.credentials.clone(), account, Slot::Password).await?;
        let user = if config.username.is_empty() {
            &config.email
        } else {
            &config.username
        };
        if settings.webdav {
            Ok(FileClient::Dav(
                dav::DavClient::connect(&settings.endpoint, user, &secret).await?,
            ))
        } else {
            let endpoint = if settings.endpoint.is_empty() {
                &config.jmap_url
            } else {
                &settings.endpoint
            };
            let base = crate::jmap::client::normalize_base_url(endpoint, &config.email)?;
            Ok(FileClient::Jmap(
                jmap::FileClient::connect(&base, user, &secret, config.jmap_account_id.as_deref())
                    .await?,
            ))
        }
    }
}

pub enum FileClient {
    Jmap(jmap::FileClient),
    Dav(dav::DavClient),
}

/// Save via a private sibling temporary file. A failed/cancelled transfer cannot
/// truncate an existing destination; persist_noclobber also closes the TOCTOU race.
pub async fn save_bytes(destination: &Path, bytes: &[u8]) -> Result<()> {
    let parent = destination
        .parent()
        .ok_or_else(|| err("Invalid destination."))?;
    let temp = tempfile::NamedTempFile::new_in(parent)?;
    tokio::fs::write(temp.path(), bytes).await?;
    tokio::fs::File::open(temp.path()).await?.sync_all().await?;
    temp.persist_noclobber(destination)
        .map_err(|e| err(format!("Could not save file: {}", e.error)))?;
    Ok(())
}

/// Stream a selected file with a fixed byte length. The opened handle pins the
/// source; a file growing during upload cannot exceed the advertised boundary.
pub(crate) async fn upload_body(path: &Path, limit: usize) -> Result<(reqwest::Body, u64)> {
    use tokio::io::AsyncReadExt;
    let file = tokio::fs::File::open(path).await?;
    let metadata = file.metadata().await?;
    if !metadata.is_file() || metadata.len() > limit as u64 {
        return Err(err(
            "Select a regular file within the storage upload limit.",
        ));
    }
    let length = metadata.len();
    let progress = progress::begin(1, length);
    let stream =
        futures::stream::try_unfold((file.take(length), 0_u64), move |(mut file, read)| {
            let progress = progress.clone();
            async move {
                if read == length {
                    return Ok::<_, std::io::Error>(None);
                }
                let mut chunk = vec![0_u8; 64 * 1024];
                let count = file.read(&mut chunk).await?;
                if count == 0 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "Upload file changed while reading",
                    ));
                }
                if let Some(p) = &progress {
                    p.advance(count as u64);
                }
                chunk.truncate(count);
                Ok(Some((chunk, (file, read + count as u64))))
            }
        });
    Ok((reqwest::Body::wrap_stream(stream), length))
}

pub(crate) async fn save_response(
    destination: &Path,
    mut response: reqwest::Response,
) -> Result<()> {
    use tokio::io::AsyncWriteExt;
    if response
        .content_length()
        .is_some_and(|n| n > MAX_TRANSFER as u64)
    {
        return Err(err("File exceeds the download limit."));
    }
    let temp = tempfile::NamedTempFile::new_in(
        destination
            .parent()
            .ok_or_else(|| err("Invalid destination."))?,
    )?;
    let mut file = tokio::fs::File::from_std(temp.reopen()?);
    let progress = progress::begin(2, response.content_length().unwrap_or(0));
    let mut total = 0usize;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| err("Download interrupted."))?
    {
        if chunk.len() > MAX_TRANSFER.saturating_sub(total) {
            return Err(err("File exceeds the download limit."));
        }
        file.write_all(&chunk).await?;
        if let Some(p) = &progress {
            p.advance(chunk.len() as u64);
        }
        total += chunk.len();
    }
    file.sync_all().await?;
    drop(file);
    temp.persist_noclobber(destination)
        .map_err(|e| err(format!("Could not save file: {}", e.error)))?;
    Ok(())
}
/// Attachment downloads use the existing single-flight cache, then copy via a
/// sibling temporary file so cancellation cannot leave a partial destination.
pub async fn save_cached_file(source: &Path, destination: &Path) -> Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let metadata = tokio::fs::metadata(source).await?;
    if !metadata.is_file() {
        return Err(err("Select a regular file."));
    }
    let progress = progress::begin(3, metadata.len());
    let mut source = tokio::fs::File::open(source)
        .await?
        .take(MAX_TRANSFER as u64 + 1);
    let temp = tempfile::NamedTempFile::new_in(
        destination
            .parent()
            .ok_or_else(|| err("Invalid destination."))?,
    )?;
    let mut file = tokio::fs::File::from_std(temp.reopen()?);
    let mut count = 0;
    let mut buffer = vec![0; 64 * 1024];
    loop {
        let read = source.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read]).await?;
        count += read as u64;
        if let Some(p) = &progress {
            p.advance(read as u64);
        }
    }
    if count > MAX_TRANSFER as u64 {
        return Err(err("File exceeds the download limit."));
    }
    file.flush().await?;
    file.sync_all().await?;
    drop(file);
    temp.persist_noclobber(destination)
        .map_err(|e| err(format!("Could not save file: {}", e.error)))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_path_names() {
        for name in ["", ".", "..", "../secret", "a/b", "a\\b", "a\n"] {
            assert!(validate_name(name).is_err());
        }
        assert!(validate_name("Résumé & budget.pdf").is_ok());
    }
    #[tokio::test]
    async fn save_does_not_clobber() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file");
        save_bytes(&path, b"original").await.unwrap();
        assert!(save_bytes(&path, b"replacement").await.is_err());
        assert_eq!(std::fs::read(path).unwrap(), b"original");
    }
}
