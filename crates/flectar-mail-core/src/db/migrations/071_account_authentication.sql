-- How many of each account's incoming messages carried an
-- Authentication-Results field. A server that stamps nearly every message
-- it receives with its own is one whose topmost field is its own. On one
-- that adds none, the topmost field is whatever the sender wrote, and no
-- result in it can be trusted (RFC 8601 sections 5 and 7.1).
CREATE TABLE account_authentication (
  account_id INTEGER PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE,
  messages INTEGER NOT NULL DEFAULT 0,
  stamped INTEGER NOT NULL DEFAULT 0
);
