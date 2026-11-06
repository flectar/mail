-- Messages being copied or moved to a folder of another account. Each
-- stage is committed before the next starts, so a transfer resumes after a
-- restart: fetch (the source's raw message into a file of the transfer's
-- own), upload (to the target folder), and for a move, remove (the source,
-- only once the target has the copy). A transfer's row goes once it ends.
CREATE TABLE mail_transfers (
  id INTEGER PRIMARY KEY,
  source_message_id INTEGER REFERENCES messages(id) ON DELETE SET NULL,
  source_account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  target_folder_id INTEGER NOT NULL REFERENCES folders(id) ON DELETE CASCADE,
  is_move INTEGER NOT NULL,
  stage TEXT NOT NULL CHECK (stage IN ('fetch', 'upload', 'remove')),
  staged_path TEXT,
  -- What the source was when the transfer started, for the copy.
  rfc_message_id TEXT,
  seen INTEGER NOT NULL,
  flagged INTEGER NOT NULL,
  received_at INTEGER,
  attempts INTEGER NOT NULL DEFAULT 0,
  next_attempt_at INTEGER NOT NULL DEFAULT 0,
  error TEXT,
  created_at INTEGER NOT NULL
);
CREATE INDEX idx_mail_transfers_due ON mail_transfers(next_attempt_at);
