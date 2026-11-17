-- Automatic replies an account's server sends while the user is away
-- (#239), as last read from the server or saved to it, so the app shows
-- them without asking the server first.
CREATE TABLE auto_replies (
  account_id INTEGER PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE,
  enabled INTEGER NOT NULL,
  subject TEXT NOT NULL,
  text TEXT NOT NULL,
  starts_at INTEGER,
  ends_at INTEGER,
  contacts_only INTEGER NOT NULL,
  domain_only INTEGER NOT NULL,
  -- When the app last saved them. It then uses the provider's permission to
  -- change mail settings, which a new sign-in asks for again.
  saved_at INTEGER,
  read_at INTEGER NOT NULL
);
