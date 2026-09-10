CREATE TABLE connections (
 account_id INTEGER PRIMARY KEY,
 settings_json TEXT NOT NULL,
 selected_space TEXT,
 updated_at INTEGER NOT NULL
);
-- One connection selects one authoritative protocol. Switching protocols
-- invalidates its projection; DAV hrefs and JMAP IDs are never guessed equal.
CREATE TABLE spaces (
 id INTEGER PRIMARY KEY AUTOINCREMENT,
 account_id INTEGER NOT NULL REFERENCES connections(account_id) ON DELETE CASCADE,
 remote_id TEXT NOT NULL,
 name TEXT NOT NULL,
 protocol TEXT NOT NULL CHECK(protocol IN ('jmap','dav')),
 state TEXT,
 last_synced_at INTEGER,
 UNIQUE(account_id, remote_id)
);
CREATE TABLE nodes (
 id INTEGER PRIMARY KEY,
 space_id INTEGER NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
 remote_id TEXT NOT NULL,
 parent_id TEXT,
 name TEXT NOT NULL,
 media_type TEXT,
 node_json TEXT NOT NULL,
 UNIQUE(space_id, remote_id)
);
CREATE INDEX nodes_parent ON nodes(space_id,parent_id,name);
CREATE VIRTUAL TABLE nodes_fts USING fts5(name,media_type,tokenize='unicode61 remove_diacritics 2',prefix='2 3 4');
CREATE TRIGGER nodes_insert AFTER INSERT ON nodes BEGIN
 INSERT INTO nodes_fts(rowid,name,media_type) VALUES(new.id,new.name,new.media_type);
END;
CREATE TRIGGER nodes_update AFTER UPDATE ON nodes BEGIN
 DELETE FROM nodes_fts WHERE rowid=old.id;
 INSERT INTO nodes_fts(rowid,name,media_type) VALUES(new.id,new.name,new.media_type);
END;
CREATE TRIGGER nodes_delete AFTER DELETE ON nodes BEGIN
 DELETE FROM nodes_fts WHERE rowid=old.id;
END;
CREATE TABLE collections (
 space_id INTEGER NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
 parent_id TEXT NOT NULL,
 sync_token TEXT,
 query_state TEXT,
 complete INTEGER NOT NULL DEFAULT 0,
 synced_at INTEGER NOT NULL,
 PRIMARY KEY(space_id,parent_id)
);
CREATE TABLE operations (
 id INTEGER PRIMARY KEY AUTOINCREMENT,
 account_id INTEGER NOT NULL REFERENCES connections(account_id) ON DELETE CASCADE,
 space_id INTEGER REFERENCES spaces(id) ON DELETE CASCADE,
 action TEXT NOT NULL,
 payload_json TEXT NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('queued','running','completed','failed','uncertain','cancelled')),
 progress_bytes INTEGER NOT NULL DEFAULT 0,
 error TEXT,
 created_at INTEGER NOT NULL,
 updated_at INTEGER NOT NULL
);
CREATE INDEX operations_pending ON operations(account_id,state,id);
CREATE TABLE content_cache (
 space_id INTEGER NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
 remote_id TEXT NOT NULL,
 validator TEXT NOT NULL,
 relative_path TEXT NOT NULL,
 byte_size INTEGER NOT NULL,
 accessed_at INTEGER NOT NULL,
 PRIMARY KEY(space_id,remote_id)
);

CREATE TABLE scan_nodes(space_id INTEGER NOT NULL REFERENCES spaces(id) ON DELETE CASCADE, remote_id TEXT NOT NULL, node_json TEXT NOT NULL, PRIMARY KEY(space_id,remote_id));

CREATE TABLE scan_collections(space_id INTEGER NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,parent_id TEXT NOT NULL,sync_token TEXT,PRIMARY KEY(space_id,parent_id));
