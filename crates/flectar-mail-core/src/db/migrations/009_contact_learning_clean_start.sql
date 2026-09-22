-- Suggestions are an interaction convenience, not an imported address book.
-- Establish a per-account clean-start boundary so historical backfills never
-- manufacture thousands of people from old mail.
CREATE TABLE contact_learning_state (
  account_id INTEGER PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE,
  outgoing_since INTEGER NOT NULL,
  incoming_since INTEGER NOT NULL
);

-- Successful local sends learn immediately. This durable marker makes that
-- update exactly-once even when provider submission reconciliation retries.
ALTER TABLE messages ADD COLUMN contact_learning_recorded INTEGER NOT NULL DEFAULT 0
  CHECK (contact_learning_recorded IN (0, 1));

INSERT INTO contact_learning_state (account_id, outgoing_since, incoming_since)
SELECT id,
       CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER),
       CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)
FROM accounts;

CREATE TRIGGER contact_learning_account_created
AFTER INSERT ON accounts
BEGIN
  INSERT INTO contact_learning_state (account_id, outgoing_since, incoming_since)
  VALUES (
    NEW.id,
    CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER),
    CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)
  );
END;

-- Favoriting is also an explicit keep action. Promote any favorite created by
-- an older build before removing addresses learned by its historical scan.
UPDATE contacts
SET is_managed = 1
WHERE is_favorite = 1 AND is_managed = 0;

-- One-time upgrade cleanup: retain explicit/local, favorited, and live
-- CardDAV contacts while removing legacy suggestion-only identities.
DELETE FROM contacts
WHERE is_managed = 0
  AND NOT EXISTS (
    SELECT 1 FROM carddav_objects co
    WHERE co.contact_id = contacts.id
      AND co.remote_exists = 1
      AND co.deleted = 0
  );

-- Learned affinity is part of the same legacy cache. Keep real contacts and
-- their account associations, but restart their ranking history cleanly.
UPDATE contacts
SET send_count = 0, recv_count = 0, last_interacted = NULL;

UPDATE contact_accounts
SET send_count = 0, recv_count = 0, last_interacted = NULL;
