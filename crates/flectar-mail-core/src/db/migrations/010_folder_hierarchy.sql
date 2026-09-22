ALTER TABLE folders ADD COLUMN parent_id INTEGER REFERENCES folders(id) ON DELETE SET NULL;
ALTER TABLE folders ADD COLUMN selectable INTEGER NOT NULL DEFAULT 1
  CHECK (selectable IN (0, 1));
ALTER TABLE folders ADD COLUMN can_create_children INTEGER NOT NULL DEFAULT 1
  CHECK (can_create_children IN (0, 1));
ALTER TABLE folders ADD COLUMN can_rename INTEGER NOT NULL DEFAULT 1
  CHECK (can_rename IN (0, 1));
ALTER TABLE folders ADD COLUMN can_delete INTEGER NOT NULL DEFAULT 1
  CHECK (can_delete IN (0, 1));

-- JMAP advertises this independently of per-mailbox rights. IMAP has no
-- equivalent discovery flag, so its optimistic default remains true and the
-- server enforces CREATE/ACL at mutation time.
ALTER TABLE accounts ADD COLUMN can_create_top_level_mailbox INTEGER NOT NULL DEFAULT 1
  CHECK (can_create_top_level_mailbox IN (0, 1));

-- JMAP exposes authoritative rights during session/mailbox discovery. Do not
-- present optimistic mutation controls for upgraded JMAP rows before that
-- discovery has completed; IMAP retains its server-validated optimistic
-- default because the base protocol has no equivalent mandatory rights field.
UPDATE accounts
SET can_create_top_level_mailbox = 0
WHERE mail_protocol = 'jmap';

UPDATE folders
SET can_create_children = 0, can_rename = 0, can_delete = 0
WHERE account_id IN (
  SELECT id FROM accounts WHERE mail_protocol = 'jmap'
);

CREATE INDEX idx_folders_parent ON folders(account_id, parent_id);
