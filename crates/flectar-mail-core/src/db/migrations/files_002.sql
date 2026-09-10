ALTER TABLE content_cache ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0 CHECK(pinned IN (0,1));
ALTER TABLE operations RENAME TO operations_old;
DROP INDEX operations_pending;
CREATE TABLE operations (
 id INTEGER PRIMARY KEY AUTOINCREMENT,
 account_id INTEGER NOT NULL REFERENCES connections(account_id) ON DELETE CASCADE,
 space_id INTEGER REFERENCES spaces(id) ON DELETE SET NULL,
 action TEXT NOT NULL,
 payload_json TEXT NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('preparing','queued','running','completed','failed','uncertain','cancelled')),
 progress_bytes INTEGER NOT NULL DEFAULT 0,
 error TEXT,
 created_at INTEGER NOT NULL,
 updated_at INTEGER NOT NULL
);
INSERT INTO operations SELECT * FROM operations_old;
DROP TABLE operations_old;
CREATE INDEX operations_pending ON operations(account_id,state,id);
CREATE TABLE IF NOT EXISTS scan_collections(space_id INTEGER NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,parent_id TEXT NOT NULL,sync_token TEXT,PRIMARY KEY(space_id,parent_id));
