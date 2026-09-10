//! Account-scoped disk content cache. Remote identifiers never become paths.
use super::{FileClient, FileNode, err};
use crate::{Core, error::Result};
use rusqlite::{OptionalExtension, params};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

pub async fn content(
    core: &Core,
    account: i64,
    space: i64,
    node: &FileNode,
    client: Option<&FileClient>,
    offline: bool,
) -> Result<PathBuf> {
    if !node.my_rights.may_read {
        return Err(err("You do not have permission to read this file."));
    }
    if node.is_directory() {
        return Err(err("Select a file."));
    }
    let validator = node
        .blob_id
        .as_deref()
        .filter(|id| !id.starts_with("http://") && !id.starts_with("https://"))
        .or(node.etag.as_deref())
        .unwrap_or("");
    let key = format!(
        "{:x}",
        Sha256::digest(format!("{space}\0{}\0{validator}", node.id).as_bytes())
    );
    let directory = core.paths.files_cache_dir(account);
    let path = directory.join(&key);
    let lookup = node.id.clone();
    let expected_key = key.clone();
    let recorded=core.files_db.read(move|c|Ok(c.query_row("SELECT byte_size FROM content_cache WHERE space_id=?1 AND remote_id=?2 AND relative_path=?3",params![space,lookup,expected_key],|r|r.get::<_,i64>(0)).optional()?)).await?;
    if !validator.is_empty()
        && tokio::fs::metadata(&path)
            .await
            .is_ok_and(|m| recorded == Some(m.len() as i64))
    {
        let remote = node.id.clone();
        core.files_db
            .write(move |c| {
                c.execute(
                    "UPDATE content_cache SET accessed_at=?3 WHERE space_id=?1 AND remote_id=?2",
                    params![space, remote, chrono::Utc::now().timestamp_millis()],
                )?;
                Ok(())
            })
            .await?;
        return Ok(path);
    }
    if offline {
        return Err(err(
            "This file is not available offline. Connect and choose Keep offline first.",
        ));
    }
    tokio::fs::create_dir_all(&directory).await?;
    // Reserve the bounded transfer maximum when the server omitted its size.
    let reserve = super::MAX_TRANSFER as i64;
    trim(core, account, CACHE_BUDGET - reserve).await?;
    // A per-core operation lock serializes duplicate downloads and cache writes.
    if tokio::fs::metadata(&path).await.is_ok() {
        tokio::fs::remove_file(&path).await?;
    }
    match client.ok_or_else(|| err("Storage is disconnected."))? {
        FileClient::Jmap(c) => c.download_to(node, &path).await?,
        FileClient::Dav(c) => c.download_to(node, &path).await?,
    }
    let size = tokio::fs::metadata(&path).await?.len() as i64;
    let remote = node.id.clone();
    let validator = validator.to_owned();
    core.files_db.write(move |c|{c.execute("INSERT INTO content_cache(space_id,remote_id,validator,relative_path,byte_size,accessed_at) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(space_id,remote_id) DO UPDATE SET validator=excluded.validator,relative_path=excluded.relative_path,byte_size=excluded.byte_size,accessed_at=excluded.accessed_at",params![space,remote,validator,key,size,chrono::Utc::now().timestamp_millis()])?;Ok(())}).await?;
    Ok(path)
}

// Two GiB per account, including explicitly pinned files. Pins survive ordinary
// eviction, but server deletion/revocation and account removal take precedence.
const CACHE_BUDGET: i64 = 2 * 1024 * 1024 * 1024;
pub async fn pin(core: &Core, space: i64, remote: String, pinned: bool) -> Result<()> {
    core.files_db
        .write(move |c| {
            c.execute(
                "UPDATE content_cache SET pinned=?3 WHERE space_id=?1 AND remote_id=?2",
                params![space, remote, pinned],
            )?;
            Ok(())
        })
        .await
}
pub(crate) async fn trim(core: &Core, account: i64, budget: i64) -> Result<()> {
    let obsolete = core.files_db.write(move |c| {
        let tx=c.transaction()?;
        let mut size: i64=tx.query_row("SELECT COALESCE(SUM(byte_size),0) FROM content_cache c JOIN spaces s ON s.id=c.space_id WHERE s.account_id=?1",[account],|r|r.get(0))?;
        let candidates={let mut q=tx.prepare("SELECT c.space_id,c.remote_id,c.relative_path,c.byte_size FROM content_cache c JOIN spaces s ON s.id=c.space_id WHERE s.account_id=?1 AND c.pinned=0 ORDER BY c.accessed_at,c.remote_id")?;q.query_map([account],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,i64>(3)?)))?.collect::<rusqlite::Result<Vec<_>>>()?};
        let mut obsolete=Vec::new();
        for (space,remote,path,bytes) in candidates {if size<=budget {break;} tx.execute("DELETE FROM content_cache WHERE space_id=?1 AND remote_id=?2",params![space,remote])?;size-=bytes;obsolete.push(path);}
        if size>budget {return Err(err("Offline files fill the 2 GiB account cache. Release an offline copy before downloading another file."));}
        tx.commit()?;Ok(obsolete)
    }).await?;
    for key in obsolete {
        if safe_key(&key) {
            let _ = tokio::fs::remove_file(core.paths.files_cache_dir(account).join(key)).await;
        }
    }
    sweep(core, account).await
}
fn safe_key(key: &str) -> bool {
    key.len() == 64 && key.bytes().all(|b| b.is_ascii_hexdigit())
}
/// Remove crash leftovers and superseded blob versions. Only our opaque names
/// are touched; no server path can escape the account cache.
pub(crate) async fn sweep(core: &Core, account: i64) -> Result<()> {
    let keys=core.files_db.read(move |c| {let mut q=c.prepare("SELECT relative_path FROM content_cache c JOIN spaces s ON s.id=c.space_id WHERE s.account_id=?1")?;Ok(q.query_map([account],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<std::collections::HashSet<_>>>()?)}).await?;
    let mut entries = match tokio::fs::read_dir(core.paths.files_cache_dir(account)).await {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    while let Some(entry) = entries.next_entry().await? {
        let key = entry.file_name().to_string_lossy().into_owned();
        if safe_key(&key) && !keys.contains(&key) && entry.file_type().await?.is_file() {
            tokio::fs::remove_file(entry.path()).await?;
        }
    }
    Ok(())
}
