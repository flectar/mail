-- Send later with Flectar Mail Sync: any of the user's devices sends a
-- scheduled message when the one that scheduled it is off. The message
-- itself waits in the account's Drafts on the mail server, found by its
-- Message-ID; this table is what syncs, along with which device claimed
-- the send, so only one sends it.
CREATE TABLE synced_sends (
  sync_id TEXT PRIMARY KEY
    CHECK (length(sync_id) = 32 AND sync_id NOT GLOB '*[^0-9a-f]*'),
  account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  -- The draft's Message-ID, without angle brackets.
  message_id TEXT NOT NULL,
  send_at INTEGER NOT NULL,
  -- The device that scheduled it, by device id in hex.
  origin TEXT NOT NULL,
  -- The draft's version on the mail server, once the device that
  -- scheduled it uploaded it: other devices send only that version.
  draft_ref TEXT,
  state TEXT NOT NULL DEFAULT 'scheduled'
    CHECK (state IN ('scheduled', 'sent', 'cancelled', 'failed')),
  -- Why the device that tried could not send it.
  error TEXT,
  -- The device that may send it, and until when its claim holds.
  claimed_by TEXT,
  claim_until INTEGER,
  -- The send this device queued for it, once it holds or makes one.
  action_id INTEGER REFERENCES pending_actions(id) ON DELETE SET NULL,
  -- Until when this device may send it: the end of its claim once the
  -- server took the claim. Never synced.
  may_send_until INTEGER,
  updated_at INTEGER NOT NULL
);
CREATE INDEX idx_synced_sends_message ON synced_sends(account_id, message_id);
CREATE INDEX idx_synced_sends_action ON synced_sends(action_id);
