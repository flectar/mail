CREATE TABLE sender_identities (
  account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  email TEXT NOT NULL COLLATE NOCASE,
  display_name TEXT,
  reply_to_email TEXT,
  is_primary INTEGER NOT NULL DEFAULT 0 CHECK (is_primary IN (0, 1)),
  is_provider_default INTEGER NOT NULL DEFAULT 0
    CHECK (is_provider_default IN (0, 1)),
  verification_status TEXT NOT NULL DEFAULT 'accepted'
    CHECK (verification_status IN ('accepted', 'pending')),
  last_synced_at INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (account_id, email)
) WITHOUT ROWID;
CREATE INDEX idx_sender_identities_default
  ON sender_identities(account_id, is_provider_default DESC, is_primary DESC);

-- Every provider can safely expose its authenticated address. Provider
-- adapters replace/extend these rows after remote discovery.
INSERT INTO sender_identities (
  account_id, email, display_name, is_primary, is_provider_default,
  verification_status, last_synced_at
)
SELECT id, email, display_name, 1, 1, 'accepted', created_at FROM accounts;
