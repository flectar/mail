-- Email templates follow the user to every device through Flectar Mail
-- Sync. Local integer ids differ between devices, so each template gets an
-- id that is the same everywhere.
ALTER TABLE snippets ADD COLUMN sync_id TEXT
  CHECK (sync_id IS NULL OR (length(sync_id) = 32 AND sync_id NOT GLOB '*[^0-9a-f]*'));
UPDATE snippets SET sync_id = lower(hex(randomblob(16)));
CREATE UNIQUE INDEX idx_snippets_sync_id ON snippets(sync_id) WHERE sync_id IS NOT NULL;

-- New templates get one too, unless the insert brought the id of a template
-- another device made.
CREATE TRIGGER snippets_sync_id
AFTER INSERT ON snippets
WHEN NEW.sync_id IS NULL
BEGIN
  UPDATE snippets SET sync_id = lower(hex(randomblob(16))) WHERE id = NEW.id;
END;
