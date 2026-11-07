//! The sync state this device keeps in its database, and the store the sync
//! engine works on during one run.
//!
//! A run loads the mirror and outbox into a [`RunStore`], lets the engine
//! pull, merge and push, then saves the result and hands the app the
//! changes it must take on, in one transaction.

use super::merge;
use crate::error::{CoreError, Result};
use flectar_mail_sync_client::{
    ChainOp, Clock, Entry, Mirrored, Pending, Remote, Secrets, Stamp, Store, StoreError,
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

pub(crate) fn load_row(conn: &Connection) -> Result<Option<SyncRow>> {
    let row = conn
        .query_row(
            "SELECT server_url, chain, cursor, seen, clock_ms, clock_counter, adopting,
                    last_synced_at, last_error
             FROM device_sync WHERE id = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, bool>(6)?,
                    row.get::<_, Option<i64>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                ))
            },
        )
        .optional()?;
    let Some((server_url, chain, cursor, seen, ms, counter, adopting, last_synced_at, last_error)) =
        row
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

fn stamp(ms: Option<i64>, counter: Option<i64>, device: Option<Vec<u8>>) -> Option<Stamp> {
    Some(Stamp {
        ms: to_u64(ms?),
        counter: u32::try_from(counter?).unwrap_or(0),
        device: device?.try_into().ok()?,
    })
}

fn entry(collection: Option<String>, schema: Option<i64>, body: Option<String>) -> Option<Entry> {
    Some(Entry {
        collection: collection?,
        schema: u32::try_from(schema?).unwrap_or(0),
        body: serde_json::from_str(&body?).ok()?,
    })
}

pub(crate) fn load_mirror(conn: &Connection) -> Result<BTreeMap<RecordId, Mirrored>> {
    let mut statement = conn.prepare(
        "SELECT record_id, seq, stamp_ms, stamp_counter, stamp_device, collection, schema, body
         FROM device_sync_mirror",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, Vec<u8>>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, Option<i64>>(2)?,
            row.get::<_, Option<i64>>(3)?,
            row.get::<_, Option<Vec<u8>>>(4)?,
            row.get::<_, Option<String>>(5)?,
            row.get::<_, Option<i64>>(6)?,
            row.get::<_, Option<String>>(7)?,
        ))
    })?;
    let mut mirror = BTreeMap::new();
    for row in rows {
        let (id, seq, ms, counter, device, collection, schema, body) = row?;
        mirror.insert(
            record_id(id)?,
            Mirrored {
                seq: to_u64(seq),
                stamp: stamp(ms, counter, device),
                entry: entry(collection, schema, body),
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
        let Some(stamp) = stamp(Some(ms), Some(counter), Some(device)) else {
            continue;
        };
        let entry = match body {
            Some(body) => Some(Entry {
                collection,
                schema: u32::try_from(schema).unwrap_or(0),
                body: serde_json::from_str(&body)?,
            }),
            None => None,
        };
        outbox.insert(
            record_id,
            Pending {
                record_id,
                base_seq: to_u64(base_seq),
                stamp,
                entry,
            },
        );
    }
    Ok(outbox)
}

fn save_outbox_entry(conn: &Connection, pending: &Pending, collection: &str) -> Result<()> {
    let (schema, body) = match &pending.entry {
        Some(entry) => (
            i64::from(entry.schema),
            Some(serde_json::to_string(&entry.body)?),
        ),
        None => (0, None),
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
            collection,
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
pub(crate) fn mirrored_entry(conn: &Connection, id: &RecordId) -> Result<Option<Entry>> {
    let row = conn
        .query_row(
            "SELECT collection, schema, body FROM device_sync_mirror WHERE record_id = ?1",
            params![&id[..]],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .optional()?;
    Ok(row.and_then(|(collection, schema, body)| entry(collection, schema, body)))
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
            .and_then(|mirrored| mirrored.entry.as_ref())
            .is_some_and(|mirrored| {
                mirrored.collection == entry.collection && mirrored.schema > entry.schema
            })
        {
            continue;
        }
        // The item may only now match what the server has, after a merge.
        let unchanged = mirror
            .get(id)
            .and_then(|mirrored| mirrored.entry.as_ref())
            .is_some_and(|mirrored| body_hash(mirrored) == hash)
            && !outbox.contains_key(id);
        if !unchanged {
            save_outbox_entry(
                conn,
                &Pending {
                    record_id: *id,
                    base_seq: outbox.get(id).map_or_else(|| base_seq(id), |p| p.base_seq),
                    stamp: clock.now(),
                    entry: Some(entry.clone()),
                },
                &entry.collection,
            )?;
            queued += 1;
        }
        set_local_hash(conn, id, Some(hash))?;
    }
    for id in hashes.keys().filter(|id| !local.contains_key(*id)) {
        let collection = mirror
            .get(id)
            .and_then(|mirrored| mirrored.entry.as_ref())
            .map(|entry| entry.collection.clone())
            .unwrap_or_default();
        save_outbox_entry(
            conn,
            &Pending {
                record_id: *id,
                base_seq: outbox.get(id).map_or_else(|| base_seq(id), |p| p.base_seq),
                stamp: clock.now(),
                entry: None,
            },
            &collection,
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
}

impl RunStore {
    pub fn load(conn: &Connection, row: &SyncRow, clock: Arc<Clock>) -> Result<Self> {
        Ok(Self {
            cursor: row.cursor,
            seen: row.seen,
            mirror: load_mirror(conn)?,
            outbox: load_outbox(conn)?,
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
                    last_synced_at = ?5, last_error = NULL
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
        conn.execute("DELETE FROM device_sync_mirror", [])?;
        let mut insert = conn.prepare(
            "INSERT INTO device_sync_mirror
               (record_id, seq, stamp_ms, stamp_counter, stamp_device, collection, schema, body)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?;
        for (id, mirrored) in &self.mirror {
            let (collection, schema, body) = match &mirrored.entry {
                Some(entry) => (
                    Some(entry.collection.as_str()),
                    Some(i64::from(entry.schema)),
                    Some(serde_json::to_string(&entry.body)?),
                ),
                None => (None, None, None),
            };
            insert.execute(params![
                &id[..],
                to_i64(mirrored.seq),
                mirrored.stamp.map(|stamp| to_i64(stamp.ms)),
                mirrored.stamp.map(|stamp| i64::from(stamp.counter)),
                mirrored.stamp.map(|stamp| stamp.device.to_vec()),
                collection,
                schema,
                body
            ])?;
        }
        conn.execute("DELETE FROM device_sync_outbox", [])?;
        for pending in self.outbox.values() {
            let collection = pending
                .entry
                .as_ref()
                .map(|entry| entry.collection.clone())
                .or_else(|| {
                    self.mirror
                        .get(&pending.record_id)
                        .and_then(|mirrored| mirrored.entry.as_ref())
                        .map(|entry| entry.collection.clone())
                })
                .unwrap_or_default();
            save_outbox_entry(conn, pending, &collection)?;
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
            let base = self.mirror.get(&id).cloned();
            let base_entry = base.as_ref().and_then(|mirrored| mirrored.entry.clone());
            self.mirror.insert(
                id,
                Mirrored {
                    seq: change.seq,
                    stamp: change
                        .stamp
                        .or(base.as_ref().and_then(|mirrored| mirrored.stamp)),
                    entry: change.entry.clone(),
                },
            );
            let pending = self.outbox.get(&id).cloned();
            match (&change.entry, pending) {
                // A deletion from another device wins over local edits.
                (None, _) => {
                    self.outbox.remove(&id);
                    self.take(id, base_entry, None);
                }
                (Some(remote), None) => self.take(id, base_entry, Some(remote.clone())),
                // A local deletion wins over the other device's edit.
                (Some(_), Some(Pending { entry: None, .. })) => {
                    if let Some(pending) = self.outbox.get_mut(&id) {
                        pending.base_seq = change.seq;
                    }
                }
                (
                    Some(remote),
                    Some(Pending {
                        entry: Some(local),
                        stamp,
                        ..
                    }),
                ) => {
                    let remote_newer = change.stamp.is_none_or(|remote_stamp| remote_stamp > stamp);
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
                                entry: Some(merged.clone()),
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
                    stamp: Some(change.stamp),
                    entry: change.entry.clone(),
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

    fn forget(&mut self, record_ids: &[RecordId]) -> std::result::Result<(), StoreError> {
        for id in record_ids {
            let previous = self.mirror.remove(id).and_then(|mirrored| mirrored.entry);
            // Deleted elsewhere while this device was away: the deletion wins
            // over local edits; items created here and never uploaded stay.
            if self
                .outbox
                .get(id)
                .is_none_or(|pending| pending.base_seq > 0)
            {
                self.outbox.remove(id);
                self.take(*id, previous, None);
            }
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
        if let Some(mirrored) = self.mirror.get(record_id)
            && let Some(stamp) = mirrored.stamp
        {
            self.outbox.insert(
                *record_id,
                Pending {
                    record_id: *record_id,
                    base_seq,
                    stamp,
                    entry: mirrored.entry.clone(),
                },
            );
        }
        Ok(())
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
