-- When each message went into its folder (#238), so that Trash and Junk can
-- be emptied of what has sat there for some days. IMAP's INTERNALDATE and
-- JMAP's receivedAt keep the time a message arrived through every move, so
-- the app keeps its own: on the message for its folder, and on each folder
-- it is in besides. Mail already here counts from now, so that nothing goes
-- at once. Milliseconds since the epoch.
ALTER TABLE messages ADD COLUMN folder_since INTEGER;
ALTER TABLE message_folders ADD COLUMN since INTEGER;
UPDATE messages SET folder_since = CAST(strftime('%s', 'now') AS INTEGER) * 1000;
UPDATE message_folders SET since = CAST(strftime('%s', 'now') AS INTEGER) * 1000;
CREATE TRIGGER messages_folder_since_insert AFTER INSERT ON messages
WHEN NEW.folder_since IS NULL
BEGIN
  UPDATE messages SET folder_since = CAST(strftime('%s', 'now') AS INTEGER) * 1000
  WHERE id = NEW.id;
END;
CREATE TRIGGER messages_folder_since_move AFTER UPDATE OF folder_id ON messages
WHEN NEW.folder_id IS NOT OLD.folder_id
BEGIN
  UPDATE messages SET folder_since = CAST(strftime('%s', 'now') AS INTEGER) * 1000
  WHERE id = NEW.id;
END;
CREATE TRIGGER message_folders_since_insert AFTER INSERT ON message_folders
WHEN NEW.since IS NULL
BEGIN
  UPDATE message_folders SET since = CAST(strftime('%s', 'now') AS INTEGER) * 1000
  WHERE message_id = NEW.message_id AND folder_id = NEW.folder_id;
END;
