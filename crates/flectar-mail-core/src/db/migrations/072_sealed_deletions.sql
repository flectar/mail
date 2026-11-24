-- Flectar Mail Sync seals deletions as records of their own, stamped by the
-- device that made them (records version 2), so the server can't delete
-- anything on its own. Every mirrored version now has a stamp and a
-- collection, a deletion's too, so this device can refuse the version
-- before a deletion if the server serves it again, and seal the deletion
-- again for a server that lost it. A deletion also notes whether the server
-- still holds it sealed.
--
-- The deletions mirrored before came from the server's bare tombstones,
-- which prove nothing, and carry no stamp of their own or collection: they
-- go. What they deleted stays deleted here.
CREATE TABLE device_sync_mirror_sealed (
  record_id BLOB PRIMARY KEY CHECK (length(record_id) = 16),
  seq INTEGER NOT NULL CHECK (seq >= 0),
  stamp_ms INTEGER NOT NULL,
  stamp_counter INTEGER NOT NULL,
  stamp_device BLOB NOT NULL CHECK (length(stamp_device) = 16),
  -- The item's collection, or the deleted item's.
  collection TEXT NOT NULL,
  -- Both NULL once a member deleted the record.
  schema INTEGER,
  body TEXT,
  -- A deletion the server no longer holds sealed: a bare tombstone took its
  -- place, or nothing did.
  compacted INTEGER NOT NULL DEFAULT 0 CHECK (compacted IN (0, 1)),
  CHECK ((schema IS NULL) = (body IS NULL)),
  CHECK (compacted = 0 OR body IS NULL)
) WITHOUT ROWID;

INSERT INTO device_sync_mirror_sealed
  (record_id, seq, stamp_ms, stamp_counter, stamp_device, collection, schema, body)
SELECT record_id, seq, stamp_ms, stamp_counter, stamp_device, collection, schema, body
FROM device_sync_mirror
WHERE stamp_ms IS NOT NULL AND stamp_counter IS NOT NULL AND stamp_device IS NOT NULL
  AND collection IS NOT NULL AND schema IS NOT NULL AND body IS NOT NULL;

DROP TABLE device_sync_mirror;
ALTER TABLE device_sync_mirror_sealed RENAME TO device_sync_mirror;
