//! Rebuildable disk projections. A scan stages pages in SQLite, then publishes
//! the complete tree and cursor atomically. Readers keep the last good tree
//! throughout a failed scan; memory is bounded to a protocol page.
use super::{FileClient, FileNode, store};
use crate::{Core, error::Result};
use rusqlite::{OptionalExtension, params};

pub async fn account(core: &Core, account: i64) -> Result<()> {
    let _guard = core.file_work_lock.lock().await;
    let settings = core.file_connection_settings(account).await?;
    let mut client = core.connect_files(account, &settings).await?;
    match &mut client {
        FileClient::Jmap(c) => {
            // A successful session is authoritative about accessible accounts.
            let available = c.accounts.iter().map(|a| a.id.clone()).collect::<Vec<_>>();
            core.files_db.write(move |db|{let tx=db.transaction()?;
                let ids={let mut q=tx.prepare("SELECT id,remote_id FROM spaces WHERE account_id=?1 AND protocol='jmap'")?;q.query_map([account],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?};
                for (id,remote) in ids {if !available.contains(&remote) {
                    tx.execute("UPDATE operations SET state='cancelled',error='Access to this storage space was revoked.' WHERE space_id=?1 AND state IN ('preparing','queued')",[id])?;
                    tx.execute("DELETE FROM spaces WHERE id=?1",[id])?;
                }}tx.commit()?;Ok(())}).await?;
            super::cache::sweep(core, account).await?;
            for remote in c.accounts.clone() {
                c.select_account(&remote.id)?;
                let space =
                    store::space(&core.files_db, account, remote.id, remote.name, "jmap").await?;
                let since = core
                    .files_db
                    .read(move |db| {
                        Ok(
                            db.query_row("SELECT state FROM spaces WHERE id=?1", [space], |r| {
                                r.get::<_, Option<String>>(0)
                            })?,
                        )
                    })
                    .await?;
                if let Some(since) = since
                    && let Ok(changes) = c.changes(&since).await
                {
                    if ["created", "updated", "destroyed"]
                        .iter()
                        .all(|key| changes[*key].as_array().is_some_and(Vec::is_empty))
                    {
                        continue;
                    }
                    if changes["hasMoreChanges"].as_bool() == Some(false)
                        && apply_jmap_delta(core, c, space, &changes).await?
                    {
                        continue;
                    }
                }
                begin(core, space).await?;
                let mut position = 0;
                let mut state = None;
                let mut query = None;
                loop {
                    let page = c.list_all(position).await?;
                    if state.as_ref().is_some_and(|s| s != &page.state)
                        || query.as_ref().is_some_and(|s| s != &page.query_state)
                    {
                        return Err(super::err(
                            "Storage changed during synchronization; the last complete cache was preserved.",
                        ));
                    }
                    state = Some(page.state);
                    query = Some(page.query_state);
                    stage(core, space, page.nodes).await?;
                    match page.next_position {
                        Some(next) if next > position => position = next,
                        None => break,
                        _ => return Err(super::err("Invalid file pagination.")),
                    }
                }
                publish(core, space, state).await?;
            }
        }
        FileClient::Dav(c) => {
            let space = store::space(
                &core.files_db,
                account,
                c.root.clone(),
                c.root.clone(),
                "dav",
            )
            .await?;
            begin(core, space).await?;
            let mut pending =
                std::collections::VecDeque::from([(c.root.clone(), None::<String>, 0usize)]);
            let mut visited = std::collections::HashSet::new();
            while let Some((url, parent, depth)) = pending.pop_front() {
                if depth > 64 || !visited.insert(url.clone()) {
                    return Err(super::err("Invalid WebDAV folder hierarchy."));
                }
                let key = parent.clone().unwrap_or_default();
                let lookup = key.clone();
                let previous=core.files_db.read(move |db|Ok(db.query_row("SELECT sync_token FROM collections WHERE space_id=?1 AND parent_id=?2",params![space,lookup],|r|r.get::<_,Option<String>>(0)).optional()?.flatten())).await?;
                let unchanged = if let Some(previous) = previous {
                    match c.collection_changed(&url, &previous).await {
                        Ok((false, token)) => Some(token),
                        _ => None,
                    }
                } else {
                    None
                };
                let (mut nodes, token) = if let Some(token) = unchanged {
                    let parent = parent.clone();
                    let nodes = core
                        .files_db
                        .read(move |db| {
                            let mut q = db.prepare(
                                "SELECT node_json FROM nodes WHERE space_id=?1 AND parent_id IS ?2",
                            )?;
                            let rows = q
                                .query_map(params![space, parent], |r| r.get::<_, String>(0))?
                                .collect::<rusqlite::Result<Vec<_>>>()?;
                            rows.into_iter()
                                .map(|s| Ok(serde_json::from_str(&s)?))
                                .collect::<Result<Vec<FileNode>>>()
                        })
                        .await?;
                    (nodes, Some(token))
                } else {
                    let listing = c.list(&url).await?;
                    (listing.nodes, listing.sync_token)
                };
                for node in &mut nodes {
                    node.parent_id = parent.clone();
                    if node.is_directory() {
                        pending.push_back((node.id.clone(), Some(node.id.clone()), depth + 1));
                    }
                }
                stage(core, space, nodes).await?;
                core.files_db.write(move |db|{db.execute("INSERT OR REPLACE INTO scan_collections(space_id,parent_id,sync_token) VALUES(?1,?2,?3)",params![space,key,token])?;Ok(())}).await?;
            }
            publish(core, space, None).await?;
        }
    }
    super::cache::sweep(core, account).await?;
    Ok(())
}
async fn apply_jmap_delta(
    core: &Core,
    client: &mut super::jmap::FileClient,
    space: i64,
    changes: &serde_json::Value,
) -> Result<bool> {
    let Some(state) = changes["newState"].as_str() else {
        return Ok(false);
    };
    let mut nodes = Vec::new();
    client.state = Some(state.into());
    for id in ["created", "updated"]
        .iter()
        .flat_map(|key| changes[*key].as_array().into_iter().flatten())
        .filter_map(|id| id.as_str())
    {
        let result = client.get(&[id.into()]).await?;
        if result["state"].as_str() != Some(state) {
            return Ok(false);
        }
        let Some(value) = result["list"].as_array().and_then(|v| v.first()) else {
            return Ok(false);
        };
        let mut node: FileNode = serde_json::from_value(value.clone())?;
        if node.is_directory() {
            return Ok(false);
        } // Sharing on a directory affects descendants.
        node.my_rights = client.rights(id).await?;
        nodes.push(node);
    }
    let destroyed = changes["destroyed"]
        .as_array()
        .ok_or_else(|| super::err("Invalid file changes."))?
        .iter()
        .filter_map(|id| id.as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    let state = state.to_owned();
    core.files_db
        .write(move |db| {
            let tx = db.transaction()?;
            for id in &destroyed {
                let directory: Option<String> = tx
                    .query_row(
                        "SELECT node_json FROM nodes WHERE space_id=?1 AND remote_id=?2",
                        params![space, id],
                        |r| r.get(0),
                    )
                    .optional()?;
                if directory.is_some_and(|s| {
                    serde_json::from_str::<FileNode>(&s).is_ok_and(|n| n.is_directory())
                }) {
                    return Ok(false);
                }
            }
            for id in destroyed {
                tx.execute(
                    "DELETE FROM content_cache WHERE space_id=?1 AND remote_id=?2",
                    params![space, id],
                )?;
                tx.execute(
                    "DELETE FROM nodes WHERE space_id=?1 AND remote_id=?2",
                    params![space, id],
                )?;
            }
            for node in nodes {
                store::upsert(&tx, space, &node)?;
            }
            tx.execute(
                "UPDATE spaces SET state=?2,last_synced_at=?3 WHERE id=?1",
                params![space, state, chrono::Utc::now().timestamp_millis()],
            )?;
            tx.commit()?;
            Ok(true)
        })
        .await
}
async fn begin(core: &Core, space: i64) -> Result<()> {
    core.files_db
        .write(move |c| {
            c.execute("DELETE FROM scan_nodes WHERE space_id=?1", [space])?;
            c.execute("DELETE FROM scan_collections WHERE space_id=?1", [space])?;
            Ok(())
        })
        .await
}
async fn stage(core: &Core, space: i64, nodes: Vec<FileNode>) -> Result<()> {
    core.files_db.write(move |c|{let tx=c.transaction()?;for node in nodes {tx.execute("INSERT INTO scan_nodes(space_id,remote_id,node_json) VALUES(?1,?2,?3) ON CONFLICT(space_id,remote_id) DO UPDATE SET node_json=excluded.node_json",params![space,node.id,serde_json::to_string(&node)?])?;}tx.commit()?;Ok(())}).await
}
async fn publish(core: &Core, space: i64, state: Option<String>) -> Result<()> {
    core.files_db.write(move |c|{
        let tx=c.transaction()?;
        tx.execute("DELETE FROM nodes WHERE space_id=?1",[space])?;
        tx.execute("INSERT INTO nodes(space_id,remote_id,parent_id,name,media_type,node_json) SELECT space_id,remote_id,json_extract(node_json,'$.parentId'),json_extract(node_json,'$.name'),json_extract(node_json,'$.type'),node_json FROM scan_nodes WHERE space_id=?1",[space])?;
        tx.execute("DELETE FROM content_cache WHERE space_id=?1 AND remote_id NOT IN (SELECT remote_id FROM nodes WHERE space_id=?1)",[space])?;
        tx.execute("UPDATE spaces SET state=?2,last_synced_at=?3 WHERE id=?1",params![space,state,chrono::Utc::now().timestamp_millis()])?;
        tx.execute("DELETE FROM collections WHERE space_id=?1",[space])?;
        tx.execute("INSERT INTO collections(space_id,parent_id,sync_token,complete,synced_at) SELECT space_id,parent_id,sync_token,1,?2 FROM scan_collections WHERE space_id=?1",params![space,chrono::Utc::now().timestamp_millis()])?;
        tx.execute("DELETE FROM scan_nodes WHERE space_id=?1",[space])?;
        tx.execute("DELETE FROM scan_collections WHERE space_id=?1",[space])?;
        tx.commit()?;Ok(())
    }).await
}

/// One cancellable worker for all configured file accounts. The host owns its
/// task handle so profile replacement cannot keep the old profile alive.
pub async fn run(core: Core) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let (wake, mut events) = tokio::sync::mpsc::channel::<i64>(16);
    let mut watches = tokio::task::JoinSet::new();
    let mut handles = std::collections::HashMap::<i64, tokio::task::AbortHandle>::new();
    loop {
        tokio::select! {
            _=interval.tick()=>{
                let accounts=core.files_db.read(|c|{let mut q=c.prepare("SELECT account_id FROM connections")?;Ok(q.query_map([],|r|r.get::<_,i64>(0))?.collect::<rusqlite::Result<Vec<_>>>()?)}).await;
                if let Ok(accounts)=accounts {
                    handles.retain(|id,handle|{if !accounts.contains(id){handle.abort();false}else{true}});
                    for id in accounts {
                        handles.entry(id).or_insert_with(||{
                            let core=core.clone();let wake=wake.clone();
                            watches.spawn(async move {
                                loop {
                                    let result=async {
                                        let settings=core.file_connection_settings(id).await?;
                                        let FileClient::Jmap(client)=core.connect_files(id,&settings).await? else {return Err(super::err("WebDAV uses collection polling."));};
                                        client.wait_for_change().await
                                    }.await;
                                    if result.is_ok(){if wake.send(id).await.is_err(){return;}tokio::time::sleep(std::time::Duration::from_secs(1)).await;}
                                    else {tokio::time::sleep(std::time::Duration::from_secs(60)).await;}
                                }
                            })
                        });
                        if let Err(error)=account(&core,id).await {tracing::debug!(account_id=id,%error,"file synchronization deferred");}
                    }
                    while watches.try_join_next().is_some() {}
                }
            },
            Some(id)=events.recv()=>{
                if handles.contains_key(&id)&& let Err(error)=account(&core,id).await {tracing::debug!(account_id=id,%error,"file push reconciliation deferred");}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn interrupted_scan_keeps_committed_tree_and_cursor_together() {
        let root = tempfile::tempdir().unwrap();
        let core = Core::start_mail_ui(crate::config::Paths::for_tests(root.path()))
            .await
            .unwrap();
        core.files_db
            .write(|c| {
                c.execute(
                    "INSERT INTO connections(account_id,settings_json,updated_at) VALUES(1,'{}',0)",
                    [],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let space = store::space(
            &core.files_db,
            1,
            "personal".into(),
            "Personal".into(),
            "jmap",
        )
        .await
        .unwrap();
        begin(&core, space).await.unwrap();
        stage(
            &core,
            space,
            vec![FileNode {
                id: "a".into(),
                name: "Original".into(),
                ..Default::default()
            }],
        )
        .await
        .unwrap();
        publish(&core, space, Some("s1".into())).await.unwrap();
        begin(&core, space).await.unwrap();
        stage(
            &core,
            space,
            vec![FileNode {
                id: "b".into(),
                name: "Incomplete".into(),
                ..Default::default()
            }],
        )
        .await
        .unwrap();
        assert_eq!(
            store::cached(&core.files_db, space, None, "".into())
                .await
                .unwrap()[0]
                .id,
            "a"
        );
        assert_eq!(
            core.files_db
                .read(move |c| Ok(c.query_row(
                    "SELECT state FROM spaces WHERE id=?1",
                    [space],
                    |r| r.get::<_, String>(0)
                )?))
                .await
                .unwrap(),
            "s1"
        );
        // A restart discards incomplete staging before accepting new pages.
        begin(&core, space).await.unwrap();
        stage(
            &core,
            space,
            vec![FileNode {
                id: "c".into(),
                name: "Complete".into(),
                ..Default::default()
            }],
        )
        .await
        .unwrap();
        publish(&core, space, Some("s2".into())).await.unwrap();
        let nodes = store::cached(&core.files_db, space, None, "".into())
            .await
            .unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].id, "c");
    }
}
