-- Passwords and sign-ins Flectar Mail Sync carries between the user's
-- devices. Secrets are never kept readable here: they stay sealed under a
-- key derived from the group's keys, which live in the system credential
-- store, and only the credential store holds them in the clear.

-- What this device shares for an account: its password, or a Google or
-- Microsoft sign-in for devices of one kind (`family`).
CREATE TABLE sign_in_shares (
  account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  kind TEXT NOT NULL CHECK (kind IN ('password', 'oauth')),
  family TEXT NOT NULL,
  -- The record that carries it, named from the account, kind and family.
  record_id BLOB CHECK (length(record_id) = 16),
  -- The secret sealed for the group, as JSON {"epoch", "data"}; NULL while
  -- there is nothing to share.
  sealed TEXT,
  -- A keyed digest of the secret, to see a change without keeping it.
  fingerprint BLOB,
  -- A sign-in made on this device that should replace the shared one.
  fresh INTEGER NOT NULL DEFAULT 0 CHECK (fresh IN (0, 1)),
  -- The shared sign-in no longer works here: the record says so, and a
  -- device with a working one shares it instead.
  stale INTEGER NOT NULL DEFAULT 0 CHECK (stale IN (0, 1)),
  -- When the credential store was last read for it.
  checked_at INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (account_id, kind, family)
) STRICT;

-- Secrets another device shared, waiting to go into this device's
-- credential store.
CREATE TABLE sign_in_arrivals (
  account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  kind TEXT NOT NULL CHECK (kind IN ('password', 'oauth')),
  family TEXT NOT NULL,
  record_id BLOB NOT NULL CHECK (length(record_id) = 16),
  sealed TEXT NOT NULL,
  PRIMARY KEY (account_id, kind, family)
) STRICT;
