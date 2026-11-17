-- Read receipts that came back for sent mail (RFC 8098), by the Message-ID of
-- the message they report on and the recipient they report for. They live
-- apart from the receipt messages, so deleting a receipt keeps what it said,
-- and one that arrives before its sent message has synced waits here for it.
-- `recipient_name` is the receipt sender's name, when the receipt came from
-- the address it reports for. `reported_at` is the receipt's Date, in
-- milliseconds.
CREATE TABLE returned_receipts (
  original_message_id TEXT NOT NULL,
  recipient TEXT NOT NULL COLLATE NOCASE,
  account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  recipient_name TEXT,
  disposition TEXT NOT NULL
    CHECK (disposition IN ('displayed', 'deleted', 'dispatched', 'processed')),
  reported_at INTEGER NOT NULL,
  PRIMARY KEY (original_message_id, recipient, account_id)
) WITHOUT ROWID;
