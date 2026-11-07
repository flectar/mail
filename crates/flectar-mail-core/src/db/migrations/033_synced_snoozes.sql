-- Snoozes follow the user to every device through Flectar Mail Sync. A
-- conversation's local id differs between devices, so each shared snooze
-- names it by an anchor every device can find: the provider's thread id
-- where the account has one, and the newest message's Message-ID.
-- `snoozes` keeps doing the hiding here; this table is what syncs.
CREATE TABLE synced_snoozes (
  sync_id TEXT PRIMARY KEY
    CHECK (length(sync_id) = 32 AND sync_id NOT GLOB '*[^0-9a-f]*'),
  account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  anchor TEXT NOT NULL,
  wake_at INTEGER NOT NULL,
  -- The conversation here, once its mail has arrived.
  thread_id INTEGER REFERENCES threads(id) ON DELETE SET NULL
);
CREATE INDEX idx_synced_snoozes_thread ON synced_snoozes(thread_id);
