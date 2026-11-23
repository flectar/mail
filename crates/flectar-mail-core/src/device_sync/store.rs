//! The sync state this device keeps in its database, and the store the sync
//! engine works on during one run.
//!
//! A run loads the mirror and outbox into a [`RunStore`], lets the engine
//! pull, merge and push, then saves the result and hands the app the
//! changes it must take on, in one transaction.

use super::merge;
use crate::error::{CoreError, Result};
use flectar_mail_sync_client::{
    ChainOp, Claim, Clock, Content, Entry, Mirrored, Pending, Remote, Secrets, Stamp, Store,
    StoreError,
};
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::Arc;

pub(crate) type RecordId = [u8; 16];

/// This device's row in `device_sync`.
#[derive(Clone, Debug)]
pub(crate) struct SyncRow {
    pub server_url: String,
    pub chain: Vec<ChainOp>,
    pub cursor: u64,
    pub seen: u64,
    pub clock: (u64, u32),
    pub adopting: bool,
    pub last_synced_at: Option<i64>,
    pub last_error: Option<String>,
    /// The server turned this device away, unconfirmed by the chain.
    pub turned_away: Option<Claim>,
}

fn to_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn to_u64(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

fn record_id(bytes: Vec<u8>) -> Result<RecordId> {
    bytes
        .try_into()
        .map_err(|_| CoreError::Other("a stored sync record id is not 16 bytes".into()))
}

/// The sequence number of the last change this device took on, while sync
/// is on.
pub(crate) fn cursor(conn: &Connection) -> Result<Option<u64>> {
    Ok(conn
        .query_row("SELECT cursor FROM device_sync WHERE id = 1", [], |row| {
            row.get::<_, i64>(0)
        })
        .optional()?
        .map(to_u64))
}

/// Whether this device is taking on the group's items after joining:
/// `None` while sync is off.
pub(crate) fn adopting(conn: &Connection) -> Result<Option<bool>> {
    Ok(conn
        .query_row("SELECT adopting FROM device_sync WHERE id = 1", [], |row| {
            row.get(0)
        })
        .optional()?)
}

/// Whether local changes wait for the server.
pub(crate) fn outbox_is_empty(conn: &Connection) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT NOT EXISTS (SELECT 1 FROM device_sync_outbox)",
        [],
        |row| row.get(0),
    )?)
}

pub(crate) fn load_row(conn: &Connection) -> Result<Option<SyncRow>> {
    let row = conn
        .query_row(
            "SELECT server_url, chain, cursor, seen, clock_ms, clock_counter, adopting,
                    last_synced_at, last_error, turned_away
             FROM device_sync WHERE id = 1",
            [],
            |row| {
                Ok((
                    (
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                    ),
                    row.get::<_, bool>(6)?,
                    row.get::<_, Option<i64>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<String>>(9)?,
                ))
            },
        )
        .optional()?;
    let Some((
        (server_url, chain, cursor, seen, ms, counter),
        adopting,
        last_synced_at,
        last_error,
        turned_away,
    )) = row
    else {
        return Ok(None);
    };
    Ok(Some(SyncRow {
        server_url,
        chain: serde_json::from_str(&chain)?,
        cursor: to_u64(cursor),
        seen: to_u64(seen),
        clock: (to_u64(ms), u32::try_from(counter).unwrap_or(0)),
        adopting,
        last_synced_at,
        last_error,
        turned_away: match turned_away.as_deref() {
            Some("removed") => Some(Claim::Removed),
            Some("deleted") => Some(Claim::GroupDeleted),
            _ => None,
        },
    }))
}

/// Start syncing on this device. Any state of an earlier group is dropped.
pub(crate) fn create_row(
    conn: &Connection,
    server_url: &str,
    chain: &[ChainOp],
    adopting: bool,
    now_ms: i64,
) -> Result<()> {
    clear(conn)?;
    conn.execute(
        "INSERT INTO device_sync (id, server_url, chain, adopting, created_at)
         VALUES (1, ?1, ?2, ?3, ?4)",
        params![server_url, serde_json::to_string(chain)?, adopting, now_ms],
    )?;
    Ok(())
}

/// Stop syncing on this device: the group's state goes, the app's data stays.
pub(crate) fn clear(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "DELETE FROM device_sync;
         DELETE FROM device_sync_mirror;
         DELETE FROM device_sync_outbox;
         DELETE FROM device_sync_local;",
    )?;
    Ok(())
}

pub(crate) fn set_error(conn: &Connection, error: Option<&str>) -> Result<()> {
    conn.execute(
        "UPDATE device_sync SET last_error = ?1 WHERE id = 1",
        params![error],
    )?;
    Ok(())
}

/// The server turned this device away, saying `claim`, which the chain
/// this device verified doesn't show. A sync that gets through clears it.
pub(crate) fn set_turned_away(conn: &Connection, claim: Claim) -> Result<()> {
    let claim = match claim {
        Claim::Removed => "removed",
        Claim::GroupDeleted => "deleted",
    };
    conn.execute(
        "UPDATE device_sync SET turned_away = ?1 WHERE id = 1",
        params![claim],
    )?;
    Ok(())
}

fn stamp(ms: i64, counter: i64, device: Vec<u8>) -> Result<Stamp> {
    Ok(Stamp {
        ms: to_u64(ms),
        counter: u32::try_from(counter).unwrap_or(0),
        device: device
            .try_into()
            .map_err(|_| CoreError::Other("a stored sync stamp is damaged".into()))?,
    })
}

/// A stored version: its item, or with neither schema nor body, its
/// deletion.
fn content(collection: String, schema: Option<i64>, body: Option<String>) -> Result<Content> {
    Ok(match (schema, body) {
        (Some(schema), Some(body)) => Content::Entry(Entry {
            collection,
            schema: u32::try_from(schema).unwrap_or(0),
            body: serde_json::from_str(&body)?,
        }),
        _ => Content::Deleted { collection },
    })
}

pub(crate) fn load_mirror(conn: &Connection) -> Result<BTreeMap<RecordId, Mirrored>> {
    let mut statement = conn.prepare(
        "SELECT record_id, seq, stamp_ms, stamp_counter, stamp_device, collection, schema, body,
                compacted
         FROM device_sync_mirror",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, Vec<u8>>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, Vec<u8>>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, Option<i64>>(6)?,
            row.get::<_, Option<String>>(7)?,
            row.get::<_, bool>(8)?,
        ))
    })?;
    let mut mirror = BTreeMap::new();
    for row in rows {
        let (id, seq, ms, counter, device, collection, schema, body, compacted) = row?;
        mirror.insert(
            record_id(id)?,
            Mirrored {
                seq: to_u64(seq),
                stamp: stamp(ms, counter, device)?,
                content: content(collection, schema, body)?,
                compacted,
            },
        );
    }
    Ok(mirror)
}

pub(crate) fn load_outbox(conn: &Connection) -> Result<BTreeMap<RecordId, Pending>> {
    let mut statement = conn.prepare(
        "SELECT record_id, base_seq, stamp_ms, stamp_counter, stamp_device, collection, schema, body
         FROM device_sync_outbox",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, Vec<u8>>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, Vec<u8>>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, i64>(6)?,
            row.get::<_, Option<String>>(7)?,
        ))
    })?;
    let mut outbox = BTreeMap::new();
    for row in rows {
        let (id, base_seq, ms, counter, device, collection, schema, body) = row?;
        let record_id = record_id(id)?;
        // A deletion keeps 0 for its schema.
        let schema = body.as_ref().map(|_| schema);
        outbox.insert(
            record_id,
            Pending {
                record_id,
                base_seq: to_u64(base_seq),
                stamp: stamp(ms, counter, device)?,
                content: content(collection, schema, body)?,
            },
        );
    }
    Ok(outbox)
}

fn save_outbox_entry(conn: &Connection, pending: &Pending) -> Result<()> {
    let (schema, body) = match &pending.content {
        Content::Entry(entry) => (
            i64::from(entry.schema),
            Some(serde_json::to_string(&entry.body)?),
        ),
        Content::Deleted { .. } => (0, None),
    };
    conn.execute(
        "INSERT OR REPLACE INTO device_sync_outbox
           (record_id, base_seq, stamp_ms, stamp_counter, stamp_device, collection, schema, body)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            &pending.record_id[..],
            to_i64(pending.base_seq),
            to_i64(pending.stamp.ms),
            i64::from(pending.stamp.counter),
            &pending.stamp.device[..],
            pending.content.collection(),
            schema,
            body
        ],
    )?;
    Ok(())
}

/// The hash `device_sync_local` keeps of a record body.
pub(crate) fn body_hash(entry: &Entry) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(entry.collection.as_bytes());
    hasher.update([0]);
    hasher.update(entry.schema.to_be_bytes());
    hasher.update(entry.body.to_string().as_bytes());
    hasher.finalize().into()
}

pub(crate) fn load_local_hashes(conn: &Connection) -> Result<BTreeMap<RecordId, [u8; 32]>> {
    let mut statement = conn.prepare("SELECT record_id, hash FROM device_sync_local")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?))
    })?;
    let mut hashes = BTreeMap::new();
    for row in rows {
        let (id, hash) = row?;
        let hash: [u8; 32] = hash
            .try_into()
            .map_err(|_| CoreError::Other("a stored sync hash is not 32 bytes".into()))?;
        hashes.insert(record_id(id)?, hash);
    }
    Ok(hashes)
}

pub(crate) fn set_local_hash(
    conn: &Connection,
    id: &RecordId,
    hash: Option<[u8; 32]>,
) -> Result<()> {
    match hash {
        Some(hash) => conn.execute(
            "INSERT OR REPLACE INTO device_sync_local (record_id, hash) VALUES (?1, ?2)",
            params![&id[..], &hash[..]],
        )?,
        None => conn.execute(
            "DELETE FROM device_sync_local WHERE record_id = ?1",
            params![&id[..]],
        )?,
    };
    Ok(())
}

/// The server's version of one record, if it has one.
/// When the server's version of a record was written, in ms since the epoch.
pub(crate) fn mirrored_stamp_ms(conn: &Connection, id: &RecordId) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT stamp_ms FROM device_sync_mirror WHERE record_id = ?1",
            params![&id[..]],
            |row| row.get::<_, Option<i64>>(0),
        )
        .optional()?
        .flatten())
}

pub(crate) fn mirrored_entry(conn: &Connection, id: &RecordId) -> Result<Option<Entry>> {
    let row = conn
        .query_row(
            "SELECT collection, schema, body FROM device_sync_mirror WHERE record_id = ?1",
            params![&id[..]],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .optional()?;
    Ok(match row {
        Some((collection, schema, body)) => content(collection, schema, body)?.into_entry(),
        None => None,
    })
}

/// Whether a local change to the record waits to reach the server.
pub(crate) fn has_pending(conn: &Connection, id: &RecordId) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM device_sync_outbox WHERE record_id = ?1)",
        params![&id[..]],
        |row| row.get(0),
    )?)
}

/// Record local changes: compare the app's items with the hashes of the last
/// check, and queue an upload for each one that changed or vanished.
/// Whether [`track`] has anything to look at: an item that changed since
/// the last check, or one that went.
pub(crate) fn untracked(conn: &Connection, local: &BTreeMap<RecordId, Entry>) -> Result<bool> {
    let hashes = load_local_hashes(conn)?;
    Ok(local
        .iter()
        .any(|(id, entry)| hashes.get(id) != Some(&body_hash(entry)))
        || hashes.keys().any(|id| !local.contains_key(id)))
}

pub(crate) fn track(
    conn: &Connection,
    local: &BTreeMap<RecordId, Entry>,
    clock: &Clock,
) -> Result<usize> {
    let hashes = load_local_hashes(conn)?;
    let mirror = load_mirror(conn)?;
    let outbox = load_outbox(conn)?;
    let base_seq = |id: &RecordId| mirror.get(id).map_or(0, |mirrored| mirrored.seq);
    let mut queued = 0;
    for (id, entry) in local {
        let hash = body_hash(entry);
        if hashes.get(id) == Some(&hash) {
            continue;
        }
        // A newer version wrote this record with fields this one would drop:
        // leave it alone until this device updates.
        if mirror
            .get(id)
            .and_then(|mirrored| mirrored.content.entry())
            .is_some_and(|mirrored| {
                mirrored.collection == entry.collection && mirrored.schema > entry.schema
            })
        {
            continue;
        }
        // The item may only now match what the server has, after a merge.
        let unchanged = mirror
            .get(id)
            .and_then(|mirrored| mirrored.content.entry())
            .is_some_and(|mirrored| body_hash(mirrored) == hash)
            && !outbox.contains_key(id);
        if !unchanged {
            save_outbox_entry(
                conn,
                &Pending {
                    record_id: *id,
                    base_seq: outbox.get(id).map_or_else(|| base_seq(id), |p| p.base_seq),
                    stamp: clock.now(),
                    content: Content::Entry(entry.clone()),
                },
            )?;
            queued += 1;
        }
        set_local_hash(conn, id, Some(hash))?;
    }
    for id in hashes.keys().filter(|id| !local.contains_key(*id)) {
        // The deletion names the collection of what it deletes: as the
        // server has it, or as it waits to go up.
        let collection = mirror
            .get(id)
            .map(|mirrored| mirrored.content.collection())
            .or_else(|| outbox.get(id).map(|pending| pending.content.collection()))
            .unwrap_or_default()
            .to_owned();
        save_outbox_entry(
            conn,
            &Pending {
                record_id: *id,
                base_seq: outbox.get(id).map_or_else(|| base_seq(id), |p| p.base_seq),
                stamp: clock.now(),
                content: Content::Deleted { collection },
            },
        )?;
        set_local_hash(conn, id, None)?;
        queued += 1;
    }
    Ok(queued)
}

/// A record change the app must take on after a run.
#[derive(Clone, Debug)]
pub(crate) struct Incoming {
    /// The record as this device knew it, to find what a deletion removes.
    pub previous: Option<Entry>,
    /// `None` deletes the item.
    pub next: Option<Entry>,
}

/// The engine's store for one run, in memory.
pub(crate) struct RunStore {
    pub cursor: u64,
    pub seen: u64,
    pub mirror: BTreeMap<RecordId, Mirrored>,
    pub outbox: BTreeMap<RecordId, Pending>,
    pub incoming: BTreeMap<RecordId, Incoming>,
    pub chain: Option<Vec<ChainOp>>,
    pub secrets: Option<Secrets>,
    clock: Arc<Clock>,
    /// The mirror and outbox as loaded: [`RunStore::save`] writes only what
    /// the run changed.
    loaded_mirror: BTreeMap<RecordId, Mirrored>,
    loaded_outbox: BTreeMap<RecordId, Pending>,
}

impl RunStore {
    pub fn load(conn: &Connection, row: &SyncRow, clock: Arc<Clock>) -> Result<Self> {
        let mirror = load_mirror(conn)?;
        let outbox = load_outbox(conn)?;
        Ok(Self {
            cursor: row.cursor,
            seen: row.seen,
            loaded_mirror: mirror.clone(),
            loaded_outbox: outbox.clone(),
            mirror,
            outbox,
            incoming: BTreeMap::new(),
            chain: None,
            secrets: None,
            clock,
        })
    }

    fn take(&mut self, id: RecordId, previous: Option<Entry>, next: Option<Entry>) {
        let previous = self
            .incoming
            .remove(&id)
            .map_or(previous, |earlier| earlier.previous);
        self.incoming.insert(id, Incoming { previous, next });
    }

    /// Save what the run changed. The app's items are updated separately,
    /// in the same transaction, from [`RunStore::incoming`].
    pub fn save(&self, conn: &Connection, clock: &Clock, now_ms: i64) -> Result<()> {
        let floor = clock.floor();
        conn.execute(
            "UPDATE device_sync SET cursor = ?1, seen = ?2, clock_ms = ?3, clock_counter = ?4,
                    last_synced_at = ?5, last_error = NULL, turned_away = NULL
             WHERE id = 1",
            params![
                to_i64(self.cursor),
                to_i64(self.seen),
                to_i64(floor.ms),
                i64::from(floor.counter),
                now_ms
            ],
        )?;
        if let Some(chain) = &self.chain {
            conn.execute(
                "UPDATE device_sync SET chain = ?1 WHERE id = 1",
                params![serde_json::to_string(chain)?],
            )?;
        }
        // Only the rows the run changed: most runs take on a few records of
        // many.
        let mut insert = conn.prepare(
            "INSERT OR REPLACE INTO device_sync_mirror
               (record_id, seq, stamp_ms, stamp_counter, stamp_device, collection, schema, body,
                compacted)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )?;
        let mut remove = conn.prepare("DELETE FROM device_sync_mirror WHERE record_id = ?1")?;
        for id in self.loaded_mirror.keys() {
            if !self.mirror.contains_key(id) {
                remove.execute(params![&id[..]])?;
            }
        }
        for (id, mirrored) in &self.mirror {
            if self.loaded_mirror.get(id) == Some(mirrored) {
                continue;
            }
            let (schema, body) = match &mirrored.content {
                Content::Entry(entry) => (
                    Some(i64::from(entry.schema)),
                    Some(serde_json::to_string(&entry.body)?),
                ),
                Content::Deleted { .. } => (None, None),
            };
            insert.execute(params![
                &id[..],
                to_i64(mirrored.seq),
                to_i64(mirrored.stamp.ms),
                i64::from(mirrored.stamp.counter),
                &mirrored.stamp.device[..],
                mirrored.content.collection(),
                schema,
                body,
                mirrored.compacted
            ])?;
        }
        let mut remove = conn.prepare("DELETE FROM device_sync_outbox WHERE record_id = ?1")?;
        for id in self.loaded_outbox.keys() {
            if !self.outbox.contains_key(id) {
                remove.execute(params![&id[..]])?;
            }
        }
        for pending in self.outbox.values() {
            if self.loaded_outbox.get(&pending.record_id) == Some(pending) {
                continue;
            }
            save_outbox_entry(conn, pending)?;
        }
        Ok(())
    }
}

impl Store for RunStore {
    fn cursor(&mut self) -> std::result::Result<u64, StoreError> {
        Ok(self.cursor)
    }

    fn seen(&mut self) -> std::result::Result<u64, StoreError> {
        Ok(self.seen)
    }

    fn set_seen(&mut self, seq: u64) -> std::result::Result<(), StoreError> {
        self.seen = seq;
        Ok(())
    }

    fn mirrored(
        &mut self,
        record_id: &RecordId,
    ) -> std::result::Result<Option<Mirrored>, StoreError> {
        Ok(self.mirror.get(record_id).cloned())
    }

    fn mirrored_ids(&mut self) -> std::result::Result<Vec<RecordId>, StoreError> {
        Ok(self.mirror.keys().copied().collect())
    }

    fn apply(&mut self, changes: &[Remote], cursor: u64) -> std::result::Result<(), StoreError> {
        for change in changes {
            let id = change.record_id;
            let base_entry = self
                .mirror
                .get(&id)
                .and_then(|mirrored| mirrored.content.entry().cloned());
            self.mirror.insert(
                id,
                Mirrored {
                    seq: change.seq,
                    stamp: change.stamp,
                    content: change.content.clone(),
                    compacted: false,
                },
            );
            let pending = self.outbox.get(&id).cloned();
            match (&change.content, pending) {
                // Another device sealed its deletion, which wins over local
                // edits.
                (Content::Deleted { .. }, _) => {
                    self.outbox.remove(&id);
                    self.take(id, base_entry, None);
                }
                (Content::Entry(remote), None) => self.take(id, base_entry, Some(remote.clone())),
                // A local deletion wins over the other device's edit. It
                // must be newer than the edit, or the other devices would
                // refuse it as a rollback.
                (
                    Content::Entry(_),
                    Some(Pending {
                        content: Content::Deleted { .. },
                        ..
                    }),
                ) => {
                    if let Some(pending) = self.outbox.get_mut(&id) {
                        pending.base_seq = change.seq;
                        if pending.stamp < change.stamp {
                            pending.stamp = self.clock.now();
                        }
                    }
                }
                (
                    Content::Entry(remote),
                    Some(Pending {
                        content: Content::Entry(local),
                        stamp,
                        ..
                    }),
                ) => {
                    let remote_newer = change.stamp > stamp;
                    if local.collection != remote.collection {
                        self.outbox.remove(&id);
                        self.take(id, base_entry, Some(remote.clone()));
                        continue;
                    }
                    let base_body = base_entry
                        .as_ref()
                        .filter(|base| base.collection == remote.collection)
                        .map(|base| &base.body);
                    let mut body = merge::merge(base_body, &local.body, &remote.body, remote_newer);
                    if remote.collection == super::collections::SCHEDULED {
                        super::scheduled::settle(base_body, &local.body, &remote.body, &mut body);
                    }
                    if body == remote.body {
                        self.outbox.remove(&id);
                        self.take(id, base_entry, Some(remote.clone()));
                    } else {
                        let merged = Entry {
                            collection: remote.collection.clone(),
                            schema: local.schema.max(remote.schema),
                            body,
                        };
                        // Newer than the version it was merged with, since the
                        // clock has seen that version's stamp.
                        self.outbox.insert(
                            id,
                            Pending {
                                record_id: id,
                                base_seq: change.seq,
                                stamp: self.clock.now(),
                                content: Content::Entry(merged.clone()),
                            },
                        );
                        self.take(id, base_entry, Some(merged));
                    }
                }
            }
        }
        self.cursor = cursor;
        Ok(())
    }

    fn pending(&mut self, limit: usize) -> std::result::Result<Vec<Pending>, StoreError> {
        let mut pending: Vec<Pending> = self.outbox.values().cloned().collect();
        pending.sort_by_key(|change| change.stamp);
        pending.truncate(limit);
        Ok(pending)
    }

    fn committed(&mut self, committed: &[(Pending, u64)]) -> std::result::Result<(), StoreError> {
        for (change, seq) in committed {
            self.mirror.insert(
                change.record_id,
                Mirrored {
                    seq: *seq,
                    stamp: change.stamp,
                    content: change.content.clone(),
                    compacted: false,
                },
            );
            match self.outbox.get_mut(&change.record_id) {
                Some(newer) if newer.stamp != change.stamp => newer.base_seq = *seq,
                _ => {
                    self.outbox.remove(&change.record_id);
                }
            }
        }
        Ok(())
    }

    fn discard(&mut self, pending: &Pending) -> std::result::Result<(), StoreError> {
        if self
            .outbox
            .get(&pending.record_id)
            .is_some_and(|stored| stored.stamp == pending.stamp)
        {
            self.outbox.remove(&pending.record_id);
        }
        Ok(())
    }

    fn requeue(
        &mut self,
        record_id: &RecordId,
        base_seq: u64,
    ) -> std::result::Result<(), StoreError> {
        if let Some(pending) = self.outbox.get_mut(record_id) {
            pending.base_seq = base_seq;
            return Ok(());
        }
        if let Some(mirrored) = self.mirror.get(record_id) {
            self.outbox.insert(
                *record_id,
                Pending {
                    record_id: *record_id,
                    base_seq,
                    stamp: mirrored.stamp,
                    content: mirrored.content.clone(),
                },
            );
        }
        Ok(())
    }

    fn compacted(&mut self, record_id: &RecordId, seq: u64) -> std::result::Result<(), StoreError> {
        let Some(mirrored) = self.mirror.get_mut(record_id) else {
            return Ok(());
        };
        if !mirrored.content.is_deleted() {
            return Ok(());
        }
        mirrored.seq = seq;
        mirrored.compacted = true;
        if let Some(pending) = self.outbox.get_mut(record_id) {
            pending.base_seq = seq;
        }
        Ok(())
    }

    fn compactable(
        &mut self,
        before_ms: u64,
        limit: usize,
    ) -> std::result::Result<Vec<(RecordId, u64)>, StoreError> {
        Ok(self
            .mirror
            .iter()
            .filter(|(id, mirrored)| {
                mirrored.content.is_deleted()
                    && !mirrored.compacted
                    && mirrored.seq > 0
                    && mirrored.stamp.ms < before_ms
                    && !self.outbox.contains_key(*id)
            })
            .take(limit)
            .map(|(id, mirrored)| (*id, mirrored.seq))
            .collect())
    }

    fn save_chain(&mut self, chain: &[ChainOp]) -> std::result::Result<(), StoreError> {
        self.chain = Some(chain.to_vec());
        Ok(())
    }

    fn save_secrets(&mut self, secrets: &Secrets) -> std::result::Result<(), StoreError> {
        self.secrets = Some(secrets.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testutil;
    use serde_json::json;

    fn pref(value: &str) -> Entry {
        Entry {
            collection: "pref".into(),
            schema: 1,
            body: json!({"key": "theme", "value": value}),
        }
    }

    #[test]
    fn a_run_saves_only_the_records_it_changed() {
        let conn = testutil::conn();
        create_row(&conn, "https://sync.test", &[], false, 1).unwrap();
        let (kept, changed, sent) = ([1u8; 16], [2u8; 16], [3u8; 16]);
        for id in [kept, changed] {
            conn.execute(
                "INSERT INTO device_sync_mirror
                   (record_id, seq, stamp_ms, stamp_counter, stamp_device, collection, schema, body)
                 VALUES (?1, 1, 1, 0, ?2, 'pref', 1, '{}')",
                params![&id[..], &[7u8; 16][..]],
            )
            .unwrap();
        }
        let stamp = Stamp {
            ms: 1,
            counter: 0,
            device: [7; 16],
        };
        save_outbox_entry(
            &conn,
            &Pending {
                record_id: sent,
                base_seq: 0,
                stamp,
                content: Content::Entry(pref("carbon")),
            },
        )
        .unwrap();
        let clock = Arc::new(Clock::new([7; 16], None));
        let row = load_row(&conn).unwrap().unwrap();
        let mut run = RunStore::load(&conn, &row, Arc::clone(&clock)).unwrap();
        // A record the run leaves alone is not written back over.
        conn.execute(
            "UPDATE device_sync_mirror SET seq = 9 WHERE record_id = ?1",
            params![&kept[..]],
        )
        .unwrap();
        run.mirror.get_mut(&changed).unwrap().seq = 5;
        run.outbox.remove(&sent);
        run.save(&conn, &clock, 2).unwrap();
        let mirror = load_mirror(&conn).unwrap();
        assert_eq!(mirror[&changed].seq, 5);
        assert_eq!(mirror[&kept].seq, 9);
        assert!(load_outbox(&conn).unwrap().is_empty());
    }

    #[test]
    fn a_check_sees_what_changed_since_the_last() {
        let conn = testutil::conn();
        create_row(&conn, "https://sync.test", &[], false, 1).unwrap();
        let clock = Clock::new([7; 16], None);
        let id = [4u8; 16];
        let mut local = BTreeMap::from([(id, pref("carbon"))]);
        assert!(untracked(&conn, &local).unwrap());
        track(&conn, &local, &clock).unwrap();
        assert!(!untracked(&conn, &local).unwrap());
        local.insert(id, pref("snow"));
        assert!(untracked(&conn, &local).unwrap());
        track(&conn, &local, &clock).unwrap();
        local.clear();
        assert!(untracked(&conn, &local).unwrap(), "a deletion counts too");
    }

    fn stamp(ms: u64, device: u8) -> Stamp {
        Stamp {
            ms,
            counter: 0,
            device: [device; 16],
        }
    }

    fn deleted() -> Content {
        Content::Deleted {
            collection: "pref".into(),
        }
    }

    /// A run over a device that mirrors `id` at seq 3 as `content`.
    fn run_with(conn: &Connection, id: RecordId, content: Content) -> (RunStore, Arc<Clock>) {
        create_row(conn, "https://sync.test", &[], false, 1).unwrap();
        let clock = Arc::new(Clock::new([7; 16], None));
        let row = load_row(conn).unwrap().unwrap();
        let mut run = RunStore::load(conn, &row, Arc::clone(&clock)).unwrap();
        run.mirror.insert(
            id,
            Mirrored {
                seq: 3,
                stamp: stamp(10, 8),
                content,
                compacted: false,
            },
        );
        (run, clock)
    }

    #[test]
    fn a_deletion_keeps_its_stamp_and_collection() {
        let conn = testutil::conn();
        let id = [4u8; 16];
        let (mut run, clock) = run_with(&conn, id, Content::Entry(pref("carbon")));
        run.apply(
            &[Remote {
                record_id: id,
                seq: 5,
                stamp: stamp(20, 8),
                content: deleted(),
            }],
            5,
        )
        .unwrap();
        assert!(run.incoming[&id].next.is_none());
        run.save(&conn, &clock, 2).unwrap();
        let mirrored = &load_mirror(&conn).unwrap()[&id];
        assert_eq!(
            (
                mirrored.seq,
                mirrored.stamp,
                &mirrored.content,
                mirrored.compacted
            ),
            (5, stamp(20, 8), &deleted(), false)
        );

        // The server holds a bare tombstone in its place: the deletion stands.
        let row = load_row(&conn).unwrap().unwrap();
        let mut run = RunStore::load(&conn, &row, Arc::clone(&clock)).unwrap();
        run.compacted(&id, 6).unwrap();
        run.save(&conn, &clock, 3).unwrap();
        let mirrored = &load_mirror(&conn).unwrap()[&id];
        assert_eq!((mirrored.seq, mirrored.stamp), (6, stamp(20, 8)));
        assert!(mirrored.compacted && mirrored.content.is_deleted());
    }

    #[test]
    fn a_local_deletion_that_wins_is_newer_than_the_edit_it_beat() {
        let conn = testutil::conn();
        let id = [4u8; 16];
        let (mut run, clock) = run_with(&conn, id, Content::Entry(pref("carbon")));
        run.outbox.insert(
            id,
            Pending {
                record_id: id,
                base_seq: 3,
                stamp: stamp(15, 7),
                content: deleted(),
            },
        );
        // The engine's clock observes every stamp it opens.
        let edit = stamp(30, 8);
        clock.observe(&edit);
        run.apply(
            &[Remote {
                record_id: id,
                seq: 6,
                stamp: edit,
                content: Content::Entry(pref("snow")),
            }],
            6,
        )
        .unwrap();
        let pending = &run.outbox[&id];
        assert!(pending.content.is_deleted());
        assert_eq!(pending.base_seq, 6);
        assert!(pending.stamp > edit, "or the others refuse it");
        assert!(run.incoming.is_empty(), "the item stays deleted here");
    }

    #[test]
    fn what_goes_back_up_is_the_mirrored_version_deletions_too() {
        let conn = testutil::conn();
        let (kept, gone) = ([4u8; 16], [5u8; 16]);
        let (mut run, _) = run_with(&conn, kept, Content::Entry(pref("carbon")));
        run.mirror.insert(
            gone,
            Mirrored {
                seq: 4,
                stamp: stamp(12, 9),
                content: deleted(),
                compacted: false,
            },
        );
        run.requeue(&kept, 8).unwrap();
        run.requeue(&gone, 9).unwrap();
        assert_eq!(
            run.outbox[&kept],
            Pending {
                record_id: kept,
                base_seq: 8,
                stamp: stamp(10, 8),
                content: Content::Entry(pref("carbon")),
            }
        );
        assert_eq!(
            run.outbox[&gone],
            Pending {
                record_id: gone,
                base_seq: 9,
                stamp: stamp(12, 9),
                content: deleted(),
            }
        );
        assert!(run.incoming.is_empty(), "nothing changes here");
    }

    #[test]
    fn only_old_sealed_deletions_are_compacted() {
        let conn = testutil::conn();
        let (live, old, recent, compacted, waiting) =
            ([1u8; 16], [2u8; 16], [3u8; 16], [4u8; 16], [5u8; 16]);
        let (mut run, _) = run_with(&conn, live, Content::Entry(pref("carbon")));
        for (id, ms, done) in [
            (old, 10, false),
            (recent, 500, false),
            (compacted, 10, true),
            (waiting, 10, false),
        ] {
            run.mirror.insert(
                id,
                Mirrored {
                    seq: 2,
                    stamp: stamp(ms, 9),
                    content: deleted(),
                    compacted: done,
                },
            );
        }
        run.outbox.insert(
            waiting,
            Pending {
                record_id: waiting,
                base_seq: 2,
                stamp: stamp(600, 7),
                content: Content::Entry(pref("snow")),
            },
        );
        assert_eq!(run.compactable(100, 10).unwrap(), vec![(old, 2)]);
    }

    #[test]
    fn a_local_deletion_names_what_it_deletes() {
        let conn = testutil::conn();
        create_row(&conn, "https://sync.test", &[], false, 1).unwrap();
        let clock = Clock::new([7; 16], None);
        let id = [4u8; 16];
        track(&conn, &BTreeMap::from([(id, pref("carbon"))]), &clock).unwrap();
        track(&conn, &BTreeMap::new(), &clock).unwrap();
        assert_eq!(load_outbox(&conn).unwrap()[&id].content, deleted());
    }

    #[test]
    fn a_sync_that_gets_through_ends_a_server_s_refusal() {
        let conn = testutil::conn();
        create_row(&conn, "https://sync.test", &[], false, 1).unwrap();
        assert_eq!(load_row(&conn).unwrap().unwrap().turned_away, None);
        set_turned_away(&conn, Claim::GroupDeleted).unwrap();
        let row = load_row(&conn).unwrap().unwrap();
        assert_eq!(row.turned_away, Some(Claim::GroupDeleted));
        set_turned_away(&conn, Claim::Removed).unwrap();
        let row = load_row(&conn).unwrap().unwrap();
        assert_eq!(row.turned_away, Some(Claim::Removed));
        let clock = Arc::new(Clock::new([7; 16], None));
        let run = RunStore::load(&conn, &row, Arc::clone(&clock)).unwrap();
        run.save(&conn, &clock, 2).unwrap();
        assert_eq!(load_row(&conn).unwrap().unwrap().turned_away, None);
    }
}
