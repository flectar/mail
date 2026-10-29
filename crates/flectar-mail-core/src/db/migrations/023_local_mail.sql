-- Mail can also be kept on this device without a server: imported mail goes
-- into one account whose protocol is 'local'. Its folders and messages work
-- like any account's, but it has no address, server, sign-in, or settings,
-- so those columns are empty exactly for it.

CREATE TABLE accounts_new (
  id INTEGER PRIMARY KEY,
  email TEXT UNIQUE,
  display_name TEXT,
  provider TEXT CHECK (provider IN ('generic', 'gmail', 'microsoft')),
  auth_kind TEXT CHECK (auth_kind IN ('password', 'oauth2')),
  mail_protocol TEXT NOT NULL
    CHECK (mail_protocol IN ('gmail', 'imap', 'jmap', 'pop3', 'local')),
  username TEXT,
  jmap_url TEXT NOT NULL DEFAULT '',
  jmap_account_id TEXT,
  incoming_host TEXT,
  incoming_port INTEGER CHECK (incoming_port BETWEEN 0 AND 65535),
  smtp_host TEXT,
  smtp_port INTEGER CHECK (smtp_port BETWEEN 0 AND 65535),
  sync_state TEXT NOT NULL DEFAULT 'idle',
  sync_error TEXT,
  created_at INTEGER NOT NULL,
  settings_json TEXT,
  sort_order INTEGER NOT NULL DEFAULT 0,
  avatar_url TEXT,
  can_create_top_level_mailbox INTEGER NOT NULL DEFAULT 1
    CHECK (can_create_top_level_mailbox IN (0, 1)),
  CHECK ((provider = 'gmail') = (mail_protocol = 'gmail')),
  CHECK (provider <> 'microsoft' OR mail_protocol = 'imap'),
  CHECK (mail_protocol NOT IN ('jmap', 'pop3') OR provider = 'generic'),
  CHECK ((mail_protocol = 'local') = (email IS NULL)),
  CHECK ((mail_protocol = 'local') = (provider IS NULL)),
  CHECK ((mail_protocol = 'local') = (auth_kind IS NULL)),
  CHECK ((mail_protocol = 'local') = (username IS NULL)),
  CHECK ((mail_protocol = 'local') = (incoming_host IS NULL)),
  CHECK ((mail_protocol = 'local') = (incoming_port IS NULL)),
  CHECK ((mail_protocol = 'local') = (smtp_host IS NULL)),
  CHECK ((mail_protocol = 'local') = (smtp_port IS NULL)),
  CHECK ((mail_protocol = 'local') = (settings_json IS NULL))
);

INSERT INTO accounts_new (
  id, email, display_name, provider, auth_kind, mail_protocol, username,
  jmap_url, jmap_account_id, incoming_host, incoming_port, smtp_host,
  smtp_port, sync_state, sync_error, created_at, settings_json, sort_order,
  avatar_url, can_create_top_level_mailbox
)
SELECT
  id, email, display_name, provider, auth_kind, mail_protocol, username,
  jmap_url, jmap_account_id, incoming_host, incoming_port, smtp_host,
  smtp_port, sync_state, sync_error, created_at, settings_json, sort_order,
  avatar_url, can_create_top_level_mailbox
FROM accounts;

-- Renaming the new table re-checks every view, and one reading the dropped
-- table would fail that check, so the view over accounts is recreated.
DROP VIEW directory_accounts;
DROP TABLE accounts;
ALTER TABLE accounts_new RENAME TO accounts;

CREATE INDEX idx_accounts_sort_order ON accounts(sort_order, id);
CREATE UNIQUE INDEX idx_accounts_one_local ON accounts(mail_protocol)
  WHERE mail_protocol = 'local';

CREATE TRIGGER contact_learning_account_created
AFTER INSERT ON accounts
BEGIN
  INSERT INTO contact_learning_state (account_id, outgoing_since, incoming_since)
  VALUES (
    NEW.id,
    0,
    CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)
  );
END;

-- As created by migration 013, except that contacts don't belong to mail
-- kept on this device, which has no address to write from.
CREATE VIEW directory_accounts AS
SELECT o.entry_id AS contact_id, b.account_id
FROM carddav_objects o JOIN carddav_addressbooks b ON b.id = o.addressbook_id
WHERE o.entry_id IS NOT NULL
UNION ALL
SELECT e.id, a.id FROM contact_entries e CROSS JOIN accounts a
WHERE a.mail_protocol <> 'local'
  AND NOT EXISTS (SELECT 1 FROM carddav_objects o WHERE o.entry_id = e.id)
UNION ALL
SELECT -contact_id, account_id FROM contact_accounts;
