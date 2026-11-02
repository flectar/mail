-- Saved contacts become entries with their own identity. Each entry stores
-- its vCard, which is authoritative for local and synced entries alike; the
-- other columns and contact_values are a projection of it for listing and
-- search. The contacts table keeps only the per-address mail history used for
-- suggestions and ranking. Converting existing data happens in Rust, in the
-- same transaction (repo::contact_entries::migrate).

-- AUTOINCREMENT keeps a deleted entry's id from being reused while an editor
-- may still refer to it.
CREATE TABLE contact_entries (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  document TEXT NOT NULL,
  revision INTEGER NOT NULL DEFAULT 1,
  is_favorite INTEGER NOT NULL DEFAULT 0 CHECK (is_favorite IN (0, 1)),
  -- Set when the entry's address book was removed before its local changes
  -- were uploaded.
  detached_unsynced INTEGER NOT NULL DEFAULT 0 CHECK (detached_unsynced IN (0, 1)),
  -- Links a contact kept after its address book was removed to the same card
  -- when the address book is connected again.
  uid TEXT,
  name TEXT NOT NULL,
  email TEXT NOT NULL,
  phone TEXT NOT NULL,
  company TEXT NOT NULL,
  job_title TEXT NOT NULL,
  website TEXT NOT NULL,
  birthday TEXT NOT NULL,
  postal_address TEXT NOT NULL,
  notes TEXT NOT NULL,
  tags TEXT NOT NULL,
  folded TEXT NOT NULL,
  photo_hash TEXT
);

-- Every EMAIL and TEL property, keyed by its line in the entry's document.
CREATE TABLE contact_values (
  contact_id INTEGER NOT NULL REFERENCES contact_entries(id) ON DELETE CASCADE,
  line INTEGER NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN ('EMAIL', 'TEL')),
  value TEXT NOT NULL,
  label TEXT NOT NULL,
  preference INTEGER NOT NULL,
  PRIMARY KEY (contact_id, line)
) WITHOUT ROWID;
CREATE INDEX idx_contact_values_value ON contact_values(kind, value COLLATE NOCASE);
CREATE INDEX idx_contact_entries_uid ON contact_entries(uid);

-- Embedded photos, shared by entries with identical images.
CREATE TABLE contact_photos (
  hash TEXT PRIMARY KEY,
  bytes BLOB NOT NULL
) WITHOUT ROWID;
CREATE INDEX idx_contact_entries_photo ON contact_entries(photo_hash)
  WHERE photo_hash IS NOT NULL;
CREATE TRIGGER contact_photo_released_by_delete AFTER DELETE ON contact_entries
WHEN OLD.photo_hash IS NOT NULL
BEGIN
  DELETE FROM contact_photos WHERE hash = OLD.photo_hash
    AND NOT EXISTS (SELECT 1 FROM contact_entries WHERE photo_hash = OLD.photo_hash);
END;
CREATE TRIGGER contact_photo_released_by_update AFTER UPDATE OF photo_hash ON contact_entries
WHEN OLD.photo_hash IS NOT NULL AND OLD.photo_hash IS NOT NEW.photo_hash
BEGIN
  DELETE FROM contact_photos WHERE hash = OLD.photo_hash
    AND NOT EXISTS (SELECT 1 FROM contact_entries WHERE photo_hash = OLD.photo_hash);
END;

ALTER TABLE carddav_addressbooks ADD COLUMN available INTEGER NOT NULL DEFAULT 1
  CHECK (available IN (0, 1));

-- A server resource and its synchronization state. An object with an entry
-- makes that entry a synced contact; one without an entry is a resource that
-- could not be imported. The old table is converted and dropped by Rust.
DROP INDEX idx_carddav_objects_contact;
DROP INDEX idx_carddav_objects_dirty;
ALTER TABLE carddav_objects RENAME TO legacy_carddav_objects;
CREATE TABLE carddav_objects (
  id INTEGER PRIMARY KEY,
  addressbook_id INTEGER NOT NULL REFERENCES carddav_addressbooks(id) ON DELETE CASCADE,
  entry_id INTEGER UNIQUE REFERENCES contact_entries(id) ON DELETE CASCADE,
  href TEXT NOT NULL,
  etag TEXT,
  remote_exists INTEGER NOT NULL DEFAULT 1 CHECK (remote_exists IN (0, 1)),
  dirty INTEGER NOT NULL DEFAULT 0 CHECK (dirty IN (0, 1)),
  deleted INTEGER NOT NULL DEFAULT 0 CHECK (deleted IN (0, 1)),
  -- Counts local changes. An upload acknowledgement clears `dirty` only when
  -- no change was made after the uploaded revision.
  revision INTEGER NOT NULL DEFAULT 0,
  -- The server's version while it conflicts with a pending local change.
  conflict_vcard TEXT,
  conflict_etag TEXT,
  conflict_deleted INTEGER NOT NULL DEFAULT 0 CHECK (conflict_deleted IN (0, 1)),
  last_error TEXT,
  UNIQUE (addressbook_id, href)
);
CREATE INDEX idx_carddav_objects_dirty ON carddav_objects(addressbook_id) WHERE dirty = 1;

-- Disconnecting CardDAV or removing its account deletes the address books.
-- Their entries stay on this device as local contacts, except the ones the
-- user already deleted.
CREATE TRIGGER contact_entries_detached BEFORE DELETE ON carddav_addressbooks
BEGIN
  DELETE FROM contact_entries WHERE id IN (
    SELECT entry_id FROM carddav_objects WHERE addressbook_id = OLD.id AND deleted = 1
  );
  UPDATE contact_entries SET detached_unsynced = 1 WHERE id IN (
    SELECT entry_id FROM carddav_objects
    WHERE addressbook_id = OLD.id
      AND (dirty = 1 OR conflict_vcard IS NOT NULL OR conflict_deleted = 1)
  );
END;

-- The contacts workspace lists saved entries, then mail-derived suggestions
-- for addresses no entry contains. Suggestion ids are negated history ids.
-- An entry whose deletion is waiting to upload is hidden unless the server
-- changed it meanwhile, which the user must resolve.
CREATE VIEW contact_directory AS
SELECT e.id, e.name, e.email, e.phone, e.company, e.job_title, e.website,
       e.birthday, e.postal_address, e.notes, e.tags, e.folded, e.is_favorite,
       1 AS is_managed,
       COALESCE(h.send_count, 0) AS send_count,
       COALESCE(h.recv_count, 0) AS recv_count,
       h.last_interacted
FROM contact_entries e
LEFT JOIN (
  SELECT contact_id, SUM(send_count) AS send_count, SUM(recv_count) AS recv_count,
         MAX(last_interacted) AS last_interacted
  FROM (
    SELECT DISTINCT v.contact_id, c.id, c.send_count, c.recv_count, c.last_interacted
    FROM contact_values v JOIN contacts c ON c.email = v.value
    WHERE v.kind = 'EMAIL'
  )
  GROUP BY contact_id
) h ON h.contact_id = e.id
WHERE NOT EXISTS (
  SELECT 1 FROM carddav_objects o
  WHERE o.entry_id = e.id AND o.deleted = 1
    AND o.conflict_vcard IS NULL AND o.conflict_deleted = 0
)
UNION ALL
SELECT -c.id, COALESCE(c.name, ''), c.email, '', '', '', '', '', '', '', '',
       COALESCE(c.folded, c.email), 0, 0, c.send_count, c.recv_count, c.last_interacted
FROM contacts c
WHERE NOT EXISTS (
  SELECT 1 FROM contact_values v WHERE v.kind = 'EMAIL' AND v.value = c.email COLLATE NOCASE
);

-- Accounts each directory row belongs to: a synced entry to its address
-- book's account, a local entry to every account, and a suggestion to the
-- accounts whose mail mentioned it.
CREATE VIEW directory_accounts AS
SELECT o.entry_id AS contact_id, b.account_id
FROM carddav_objects o JOIN carddav_addressbooks b ON b.id = o.addressbook_id
WHERE o.entry_id IS NOT NULL
UNION ALL
SELECT e.id, a.id FROM contact_entries e CROSS JOIN accounts a
WHERE NOT EXISTS (SELECT 1 FROM carddav_objects o WHERE o.entry_id = e.id)
UNION ALL
SELECT -contact_id, account_id FROM contact_accounts;
