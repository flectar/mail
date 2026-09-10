//! Durable projections and operation history. Every projection update and its
//! sync cursor commit in the same SQLite transaction.
use super::{ConnectionSettings, FileNode};
use crate::{Core, db::Db, error::Result};
use rusqlite::{OptionalExtension, params};

#[derive(Clone, Debug)]
pub struct Operation {
    pub id: i64,
    pub action: String,
    pub state: String,
    pub error: String,
    pub bytes: u64,
    pub description: String,
}
pub async fn operations(db: &Db, account: i64) -> Result<Vec<Operation>> {
    db.read(move |c| {
        let mut query=c.prepare("SELECT id,action,state,COALESCE(error,''),progress_bytes,payload_json FROM operations WHERE account_id=?1 ORDER BY id DESC LIMIT 100")?;
        let rows=query.query_map([account],|r|{
            let payload=r.get::<_,String>(5)?;
            let payload=serde_json::from_str::<serde_json::Value>(&payload).unwrap_or_default();
            let name=payload["stagedName"].as_str().or_else(||payload["node"]["name"].as_str()).or_else(||payload["a"].as_str()).unwrap_or("");
            let folder=payload["history"].as_array().into_iter().flatten().filter_map(|n|n["name"].as_str()).collect::<Vec<_>>().join(" / ");
            Ok(Operation{id:r.get(0)?,action:r.get(1)?,state:r.get(2)?,error:r.get(3)?,bytes:r.get::<_,i64>(4)?.max(0) as u64,description:format!("/{folder} · {name}")})
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }).await
}

impl Core {
    pub(crate) async fn recover_files(&self) -> Result<()> {
        let accounts = self
            .list_account_configs()
            .await?
            .into_iter()
            .map(|a| a.id)
            .collect::<Vec<_>>();
        // Migrate the earlier branch's settings idempotently. The original is
        // deleted only after the destination commit succeeds.
        let legacy = self
            .db
            .read(|c| {
                let mut q =
                    c.prepare("SELECT key,value FROM app_settings WHERE key LIKE 'files:%'")?;
                Ok(
                    q.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                        .collect::<rusqlite::Result<Vec<_>>>()?,
                )
            })
            .await?;
        for (key, value) in legacy {
            if let Some(id) = key
                .strip_prefix("files:")
                .and_then(|v| v.parse::<i64>().ok())
                .filter(|id| accounts.contains(id))
            {
                let _: ConnectionSettings = serde_json::from_str(&value)?;
                self.files_db.write(move |c| { c.execute("INSERT OR IGNORE INTO connections(account_id,settings_json,updated_at) VALUES(?1,?2,?3)",params![id,value,chrono::Utc::now().timestamp_millis()])?; Ok(()) }).await?;
            }
            self.db
                .write(move |c| {
                    c.execute("DELETE FROM app_settings WHERE key=?1", [key])?;
                    Ok(())
                })
                .await?;
        }
        self.files_db.write(move |c| {
            let tx=c.transaction()?;
            let ids={let mut q=tx.prepare("SELECT account_id FROM connections")?;q.query_map([],|r| r.get::<_,i64>(0))?.collect::<rusqlite::Result<Vec<_>>>()?};
            for id in ids { if !accounts.contains(&id) { tx.execute("DELETE FROM connections WHERE account_id=?1",[id])?; } }
            tx.execute("UPDATE operations SET state='uncertain',error='Interrupted during a server request. Refresh and reconcile before retrying.',updated_at=?1 WHERE state='running'",[chrono::Utc::now().timestamp_millis()])?;
            tx.execute("UPDATE operations SET state='failed',error='Interrupted while staging an upload. Select the source again.' WHERE state='preparing'",[])?;
            tx.commit()?; Ok(())
        }).await?;
        self.prune_file_staging().await
    }
    async fn prune_file_staging(&self) -> Result<()> {
        let pending=self.files_db.read(|c| {
            let mut q=c.prepare("SELECT account_id,id FROM operations WHERE state IN ('queued','running','uncertain')")?;
            Ok(q.query_map([],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?)))?.collect::<rusqlite::Result<std::collections::HashSet<_>>>()?)
        }).await?;
        let mut accounts =
            match tokio::fs::read_dir(self.paths.data_dir.join("file_transfers")).await {
                Ok(e) => e,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(e) => return Err(e.into()),
            };
        while let Some(account) = accounts.next_entry().await? {
            if !account.file_type().await?.is_dir() {
                continue;
            }
            let Ok(account_id) = account.file_name().to_string_lossy().parse::<i64>() else {
                continue;
            };
            let mut jobs = tokio::fs::read_dir(account.path()).await?;
            while let Some(job) = jobs.next_entry().await? {
                if !job.file_type().await?.is_dir() {
                    continue;
                }
                let Ok(id) = job.file_name().to_string_lossy().parse::<i64>() else {
                    continue;
                };
                if !pending.contains(&(account_id, id)) {
                    tokio::fs::remove_dir_all(job.path()).await?;
                }
            }
        }
        Ok(())
    }
}

pub async fn space(
    db: &Db,
    account: i64,
    remote: String,
    name: String,
    protocol: &str,
) -> Result<i64> {
    let protocol = protocol.to_owned();
    db.write(move |c| {
        c.execute("INSERT INTO spaces(account_id,remote_id,name,protocol) VALUES(?1,?2,?3,?4) ON CONFLICT(account_id,remote_id) DO UPDATE SET name=excluded.name",params![account,remote,name,protocol])?;

        Ok(c.query_row("SELECT id FROM spaces WHERE account_id=?1 AND remote_id=?2",params![account,remote],|r|r.get(0))?)
    }).await
}
pub(crate) fn upsert(c: &rusqlite::Connection, space: i64, node: &FileNode) -> Result<()> {
    c.execute("INSERT INTO nodes(space_id,remote_id,parent_id,name,media_type,node_json) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(space_id,remote_id) DO UPDATE SET parent_id=excluded.parent_id,name=excluded.name,media_type=excluded.media_type,node_json=excluded.node_json",params![space,node.id,node.parent_id,node.name,node.media_type,serde_json::to_string(node)?])?;
    Ok(())
}
pub async fn replace_collection(
    db: &Db,
    space: i64,
    parent: Option<String>,
    nodes: Vec<FileNode>,
    state: Option<String>,
    query: Option<String>,
    token: Option<String>,
) -> Result<()> {
    db.write(move |c| {
        let tx=c.transaction()?;
        // Apply moves before pruning. A promoted child must survive removal of
        // its former parent, while deleted/revoked subtrees must leave search
        // and the offline cache immediately after a complete folder refresh.
        let ids = serde_json::to_string(&nodes.iter().map(|n| &n.id).collect::<Vec<_>>())?;
        for node in nodes { upsert(&tx,space,&node)?; }
        let removed = "WITH RECURSIVE removed(remote_id) AS (
            SELECT remote_id FROM nodes WHERE space_id=?1 AND parent_id IS ?2
              AND remote_id NOT IN (SELECT value FROM json_each(?3))
            UNION
            SELECT n.remote_id FROM nodes n JOIN removed r ON n.parent_id=r.remote_id WHERE n.space_id=?1
        )";
        for (table, key) in [("content_cache", "remote_id"), ("collections", "parent_id"), ("nodes", "remote_id")] {
            tx.execute(&format!("{removed} DELETE FROM {table} WHERE space_id=?1 AND {key} IN (SELECT remote_id FROM removed)"), params![space,parent,ids])?;
        }
        tx.execute("INSERT INTO collections(space_id,parent_id,sync_token,query_state,complete,synced_at) VALUES(?1,?2,?3,?4,1,?5) ON CONFLICT(space_id,parent_id) DO UPDATE SET sync_token=excluded.sync_token,query_state=excluded.query_state,complete=1,synced_at=excluded.synced_at",params![space,parent.unwrap_or_default(),token,query,chrono::Utc::now().timestamp_millis()])?;
        let _=state; // Only a complete account scan owns the global sync cursor.
        tx.commit()?; Ok(())
    }).await
}
pub async fn cached(
    db: &Db,
    space: i64,
    parent: Option<String>,
    query: String,
) -> Result<Vec<FileNode>> {
    let query = super::attachment_match_query(&query);
    db.read(move |c| {
        let mut q=c.prepare("SELECT node_json FROM nodes WHERE space_id=?1 AND (?3!='' OR parent_id IS ?2) AND (?3='' OR id IN (SELECT rowid FROM nodes_fts WHERE nodes_fts MATCH ?3)) ORDER BY name,remote_id LIMIT 10000")?;
        let json=q.query_map(params![space,parent,query],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        json.into_iter().map(|v|Ok(serde_json::from_str(&v)?)).collect()
    }).await
}
pub async fn selected_space(db: &Db, account: i64) -> Result<Option<i64>> {
    db.read(move |c|Ok(c.query_row("SELECT s.id FROM spaces s JOIN connections c ON c.account_id=s.account_id AND c.selected_space=s.remote_id WHERE s.account_id=?1",[account],|r|r.get(0)).optional()?)).await
}
pub async fn begin(
    db: &Db,
    account: i64,
    space: Option<i64>,
    action: String,
    payload: String,
) -> Result<i64> {
    db.write(move |c| {let now=chrono::Utc::now().timestamp_millis();c.execute("INSERT INTO operations(account_id,space_id,action,payload_json,state,created_at,updated_at) VALUES(?1,?2,?3,?4,'queued',?5,?5)",params![account,space,action,payload,now])?;Ok(c.last_insert_rowid())}).await
}
pub async fn finish(db: &Db, id: i64, state: &str, error: Option<String>) -> Result<()> {
    let state = state.to_owned();
    let bytes = super::progress::bytes().min(i64::MAX as u64) as i64;
    db.write(move |c| {
        c.execute(
            "UPDATE operations SET state=?2,error=?3,updated_at=?4,progress_bytes=MAX(progress_bytes,?5) WHERE id=?1",
            params![id, state, error, chrono::Utc::now().timestamp_millis(),bytes],
        )?;
        Ok(())
    })
    .await
}
pub async fn activity(db: &Db, account: i64) -> Result<String> {
    db.read(move |c| {
        let mut q=c.prepare("SELECT id,action,state,COALESCE(error,'') FROM operations WHERE account_id=?1 ORDER BY id DESC LIMIT 100")?;
        let rows=q.query_map([account],|r|Ok(format!("#{} · {} · {}\n{}",r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows.join("\n\n"))
    }).await
}
