-- An account names its service (provider) and its mail protocol separately.
-- Generic servers previously stored the provider 'imap', and Gmail accounts
-- stored the protocol 'imap' although they always synchronize through the
-- Gmail API. The incoming server columns serve every incoming protocol, and
-- 'pop3' is admitted now so a POP3 engine needs no second rebuild of this
-- table.
--
-- Connection security becomes explicit. The former 'auto' mode, which was
-- also what a missing value meant, is resolved with the exact port rule it
-- applied at connection time: IMAP used STARTTLS on 143 and TLS otherwise;
-- SMTP used TLS on 465 and STARTTLS otherwise.

CREATE TABLE accounts_new (
  id INTEGER PRIMARY KEY,
  email TEXT NOT NULL UNIQUE,
  display_name TEXT,
  provider TEXT NOT NULL CHECK (provider IN ('generic', 'gmail', 'microsoft')),
  auth_kind TEXT NOT NULL CHECK (auth_kind IN ('password', 'oauth2')),
  mail_protocol TEXT NOT NULL
    CHECK (mail_protocol IN ('gmail', 'imap', 'jmap', 'pop3')),
  username TEXT NOT NULL,
  jmap_url TEXT NOT NULL DEFAULT '',
  jmap_account_id TEXT,
  incoming_host TEXT NOT NULL,
  incoming_port INTEGER NOT NULL CHECK (incoming_port BETWEEN 0 AND 65535),
  smtp_host TEXT NOT NULL,
  smtp_port INTEGER NOT NULL CHECK (smtp_port BETWEEN 0 AND 65535),
  sync_state TEXT NOT NULL DEFAULT 'idle',
  sync_error TEXT,
  created_at INTEGER NOT NULL,
  settings_json TEXT NOT NULL,
  sort_order INTEGER NOT NULL DEFAULT 0,
  avatar_url TEXT,
  can_create_top_level_mailbox INTEGER NOT NULL DEFAULT 1
    CHECK (can_create_top_level_mailbox IN (0, 1)),
  CHECK ((provider = 'gmail') = (mail_protocol = 'gmail')),
  CHECK (provider <> 'microsoft' OR mail_protocol = 'imap'),
  CHECK (mail_protocol NOT IN ('jmap', 'pop3') OR provider = 'generic')
);

INSERT INTO accounts_new (
  id, email, display_name, provider, auth_kind, mail_protocol, username,
  jmap_url, jmap_account_id, incoming_host, incoming_port, smtp_host,
  smtp_port, sync_state, sync_error, created_at, settings_json, sort_order,
  avatar_url, can_create_top_level_mailbox
)
SELECT
  id,
  email,
  display_name,
  CASE provider WHEN 'imap' THEN 'generic' ELSE provider END,
  auth_kind,
  CASE WHEN provider = 'gmail' THEN 'gmail' ELSE mail_protocol END,
  username,
  jmap_url,
  jmap_account_id,
  imap_host,
  imap_port,
  smtp_host,
  smtp_port,
  sync_state,
  sync_error,
  created_at,
  CASE
    WHEN json_valid(settings_json) AND json_type(settings_json) = 'object' THEN
      json_set(
        settings_json,
        '$.connection',
        json_object(
          'incomingSecurity',
          CASE json_extract(settings_json, '$.connection.imapSecurity')
            WHEN 'tls' THEN 'tls'
            WHEN 'starttls' THEN 'starttls'
            ELSE CASE WHEN imap_port = 143 THEN 'starttls' ELSE 'tls' END
          END,
          'smtpSecurity',
          CASE json_extract(settings_json, '$.connection.smtpSecurity')
            WHEN 'tls' THEN 'tls'
            WHEN 'starttls' THEN 'starttls'
            ELSE CASE WHEN smtp_port = 465 THEN 'tls' ELSE 'starttls' END
          END,
          'trustedCertificatePem',
          COALESCE(json_extract(settings_json, '$.connection.trustedCertificatePem'), '')
        )
      )
    ELSE settings_json
  END,
  sort_order,
  avatar_url,
  can_create_top_level_mailbox
FROM accounts;

-- Renaming the new table re-checks every view, and one reading the dropped
-- table would fail that check, so the view over accounts is recreated.
DROP VIEW directory_accounts;
DROP TABLE accounts;
ALTER TABLE accounts_new RENAME TO accounts;

CREATE INDEX idx_accounts_sort_order ON accounts(sort_order, id);

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

-- As created by migration 013.
CREATE VIEW directory_accounts AS
SELECT o.entry_id AS contact_id, b.account_id
FROM carddav_objects o JOIN carddav_addressbooks b ON b.id = o.addressbook_id
WHERE o.entry_id IS NOT NULL
UNION ALL
SELECT e.id, a.id FROM contact_entries e CROSS JOIN accounts a
WHERE NOT EXISTS (SELECT 1 FROM carddav_objects o WHERE o.entry_id = e.id)
UNION ALL
SELECT -contact_id, account_id FROM contact_accounts;
