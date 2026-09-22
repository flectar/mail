-- Suggestions are an interaction convenience, not an imported address book.
-- Establish per-account boundaries. Sent-mail history is useful autocomplete
-- data, while incoming history must not manufacture thousands of suggestions.
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
       0,
       CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)
FROM accounts;

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

-- Favoriting is also an explicit keep action. Promote any favorite created by
-- an older build before removing addresses learned by its historical scan.
UPDATE contacts
SET is_managed = 1
WHERE is_favorite = 1 AND is_managed = 0;

-- Repair the aggregate defensively before deciding whether a legacy suggestion
-- represents a genuine send. Per-account affinity is the more specific source
-- when an interrupted older write left the global count behind.
UPDATE contacts
SET send_count = MAX(
  send_count,
  COALESCE((
    SELECT SUM(ca.send_count)
    FROM contact_accounts ca
    WHERE ca.contact_id = contacts.id
  ), 0)
);

-- One-time upgrade cleanup: retain explicit/local, favorited, live CardDAV,
-- and genuine outgoing-recipient suggestions. Remove incoming-only identities
-- that caused old inboxes to manufacture thousands of suggested people.
DELETE FROM contacts
WHERE is_managed = 0
  AND send_count = 0
  AND NOT EXISTS (
    SELECT 1 FROM carddav_objects co
    WHERE co.contact_id = contacts.id
      AND co.remote_exists = 1
      AND co.deleted = 0
  );

-- For suggestion-only rows, incoming affinity belongs to the legacy cache.
-- Preserve outbound frequency and last-known interaction so people the user
-- actually emailed autocomplete immediately. Real contacts keep all affinity.
UPDATE contacts
SET recv_count = 0
WHERE is_managed = 0
  AND NOT EXISTS (
    SELECT 1 FROM carddav_objects co
    WHERE co.contact_id = contacts.id
      AND co.remote_exists = 1
      AND co.deleted = 0
  );

UPDATE contact_accounts
SET recv_count = 0
WHERE EXISTS (
  SELECT 1 FROM contacts c
  WHERE c.id = contact_accounts.contact_id
    AND c.is_managed = 0
    AND NOT EXISTS (
      SELECT 1 FROM carddav_objects co
      WHERE co.contact_id = c.id
        AND co.remote_exists = 1
        AND co.deleted = 0
    )
);
