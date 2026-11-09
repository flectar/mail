-- Flectar Sync keeps this device's setup the same on the user's other
-- devices through an end-to-end encrypted server. Mail, contacts and
-- calendars keep syncing with the mail provider, not through it.

-- Accounts get an id that is the same on every device. Local integer ids
-- differ between devices, so synced references use this instead. The local
-- mail account never syncs, so it has none.
ALTER TABLE accounts ADD COLUMN sync_id TEXT
  CHECK (sync_id IS NULL OR (length(sync_id) = 32 AND sync_id NOT GLOB '*[^0-9a-f]*'));
UPDATE accounts SET sync_id = lower(hex(randomblob(16))) WHERE mail_protocol <> 'local';
CREATE UNIQUE INDEX idx_accounts_sync_id ON accounts(sync_id) WHERE sync_id IS NOT NULL;

-- New accounts get one too, unless the insert brought the id of an account
-- another device added.
CREATE TRIGGER accounts_sync_id
AFTER INSERT ON accounts
WHEN NEW.sync_id IS NULL AND NEW.mail_protocol <> 'local'
BEGIN
  UPDATE accounts SET sync_id = lower(hex(randomblob(16))) WHERE id = NEW.id;
END;

-- Whether this device syncs, and where it is in its group. One row at most:
-- the device's keys live in the platform credential store, not here.
CREATE TABLE device_sync (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  server_url TEXT NOT NULL,
  -- The group's signed membership chain, as this device verified it (JSON).
  chain TEXT NOT NULL,
  -- Server changes up to this sequence number are applied.
  cursor INTEGER NOT NULL DEFAULT 0 CHECK (cursor >= 0),
  -- The highest sequence number the server is known to have reached.
  seen INTEGER NOT NULL DEFAULT 0 CHECK (seen >= 0),
  -- The latest clock stamp, so stamps keep increasing across restarts.
  clock_ms INTEGER NOT NULL DEFAULT 0,
  clock_counter INTEGER NOT NULL DEFAULT 0,
  -- A device that just joined reads the group before it uploads anything,
  -- so it takes the group's preferences instead of overwriting them.
  adopting INTEGER NOT NULL DEFAULT 0 CHECK (adopting IN (0, 1)),
  created_at INTEGER NOT NULL,
  last_synced_at INTEGER,
  last_error TEXT
);

-- The last server version of each record: the base of three-way merges.
-- Records of collections or schemas this version does not know are kept
-- unread, so they survive until a newer version reads them.
CREATE TABLE device_sync_mirror (
  record_id BLOB PRIMARY KEY CHECK (length(record_id) = 16),
  seq INTEGER NOT NULL CHECK (seq >= 0),
  stamp_ms INTEGER,
  stamp_counter INTEGER,
  stamp_device BLOB CHECK (stamp_device IS NULL OR length(stamp_device) = 16),
  collection TEXT,
  schema INTEGER,
  -- JSON; NULL once the record is deleted.
  body TEXT,
  CHECK ((stamp_ms IS NULL) = (stamp_device IS NULL))
) WITHOUT ROWID;

-- Local changes not yet on the server, one per record: the newest.
CREATE TABLE device_sync_outbox (
  record_id BLOB PRIMARY KEY CHECK (length(record_id) = 16),
  base_seq INTEGER NOT NULL CHECK (base_seq >= 0),
  stamp_ms INTEGER NOT NULL,
  stamp_counter INTEGER NOT NULL,
  stamp_device BLOB NOT NULL CHECK (length(stamp_device) = 16),
  collection TEXT NOT NULL,
  schema INTEGER NOT NULL,
  -- JSON; NULL deletes the record.
  body TEXT
) WITHOUT ROWID;

-- What each synced item looked like here at the last check, as a hash of
-- its record body. A changed hash is a local edit; a vanished item, a local
-- deletion. Items this device never had are not listed, so they are never
-- deleted by mistake.
CREATE TABLE device_sync_local (
  record_id BLOB PRIMARY KEY CHECK (length(record_id) = 16),
  hash BLOB NOT NULL CHECK (length(hash) = 32)
) WITHOUT ROWID;
