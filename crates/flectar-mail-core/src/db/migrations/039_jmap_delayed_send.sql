-- The longest a JMAP account's server holds a submission before sending
-- it, in seconds (RFC 8621 maxDelayedSend), as the last session said: 0
-- when it holds none. A scheduled message the server can hold, into the
-- account's Scheduled mailbox, goes out even with every device off.
ALTER TABLE accounts ADD COLUMN jmap_max_delayed_send INTEGER NOT NULL DEFAULT 0
  CHECK (jmap_max_delayed_send >= 0);
