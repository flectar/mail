-- POP3 accounts download mail into folders that exist only on this device.
-- A POP3 server identifies its messages by UIDL alone, so every message the
-- account has seen on the server is recorded here: downloaded ones are never
-- fetched twice, and deleting a message here never brings it back.
CREATE TABLE pop3_uidls (
  account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  uidl TEXT NOT NULL CHECK (length(uidl) BETWEEN 1 AND 70),
  -- 'stored': downloaded; message_id empties once the message is deleted
  -- here. 'matched': the account already had the message, such as the Sent
  -- copy of mail sent to oneself, so the server's copy wasn't kept.
  -- 'skipped': older than the account's mail history when it was listed.
  -- 'failed': retried from retry_at, never when that is NULL.
  state TEXT NOT NULL CHECK (state IN ('stored', 'matched', 'skipped', 'failed')),
  message_id INTEGER REFERENCES messages(id) ON DELETE SET NULL,
  size INTEGER NOT NULL CHECK (size >= 0),
  -- The message's Date, once known from its headers.
  sent_at INTEGER,
  stored_at INTEGER,
  -- When a complete listing first lacked this message. The row is kept for
  -- a while in case the server lists it again.
  missing_since INTEGER,
  attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
  retry_at INTEGER,
  last_error TEXT,
  PRIMARY KEY (account_id, uidl),
  CHECK (state NOT IN ('stored', 'matched') OR stored_at IS NOT NULL)
) WITHOUT ROWID;

CREATE INDEX pop3_uidls_message ON pop3_uidls(message_id)
  WHERE message_id IS NOT NULL;

CREATE TABLE pop3_sync_state (
  account_id INTEGER PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE,
  -- When the first complete download finished. Mail found after that is new
  -- mail and may notify.
  first_synced_at INTEGER,
  -- The last accepted sign-in and the server's minimum time between
  -- sign-ins (RFC 2449 LOGIN-DELAY).
  signed_in_at INTEGER,
  login_delay_secs INTEGER CHECK (login_delay_secs >= 0),
  -- How long the server keeps retrieved mail (RFC 2449 EXPIRE): NULL when it
  -- doesn't say, 'never', or a number of days.
  expire TEXT CHECK (
    expire IS NULL OR expire = 'never'
    OR (length(expire) BETWEEN 1 AND 10 AND expire NOT GLOB '*[^0-9]*')
  )
);
