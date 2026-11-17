-- Senders the user blocked: mail from them that sync stores in an inbox goes
-- to Spam. The list is kept on this device for every account, since the
-- providers' own block lists need settings permissions.
CREATE TABLE blocked_senders (
  -- Lowercase, as compared with LOWER(messages.from_addr).
  address TEXT PRIMARY KEY NOT NULL,
  blocked_at INTEGER NOT NULL
);
