-- A scheduled IMAP or POP3 message too large for its record travels as a
-- file in Flectar Mail Sync's storage, sealed under a key only records carry.
-- The file's id, key and sealed size, as JSON.
ALTER TABLE synced_sends ADD COLUMN file TEXT;
-- What other devices show while it waits: the message's headers and the
-- start of its text, without its attachments.
ALTER TABLE synced_sends ADD COLUMN preview BLOB;

-- Files nothing needs any more, until the device that stored them removes
-- them from Flectar Mail Sync's storage: their send ended or was forgotten,
-- or its message was stored again.
CREATE TABLE synced_send_files_gone (
  file TEXT PRIMARY KEY,
  -- The device that stored it, by device id in hex: only it removes it.
  origin TEXT NOT NULL
);

CREATE TRIGGER synced_send_file_replaced
AFTER UPDATE OF file ON synced_sends
WHEN OLD.file IS NOT NULL
  AND json_extract(OLD.file, '$.id') IS NOT json_extract(NEW.file, '$.id')
BEGIN
  INSERT OR IGNORE INTO synced_send_files_gone (file, origin)
  VALUES (OLD.file, OLD.origin);
END;

CREATE TRIGGER synced_send_file_forgotten
AFTER DELETE ON synced_sends
WHEN OLD.file IS NOT NULL
BEGIN
  INSERT OR IGNORE INTO synced_send_files_gone (file, origin)
  VALUES (OLD.file, OLD.origin);
END;
