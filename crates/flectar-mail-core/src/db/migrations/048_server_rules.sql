-- Where each account's rules run (#238): which of them its server runs,
-- so they work while the app is closed, and why the others don't, as last
-- checked. `written` is the digest of the rules this device last put on the
-- server, to tell them from rules another device put there.
CREATE TABLE server_rules (
  account_id INTEGER PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE,
  written TEXT,
  -- ServerRulesState, as JSON.
  state_json TEXT NOT NULL,
  -- The ids of the rules the server runs, and RuleHere for the others, as
  -- JSON.
  on_server_json TEXT NOT NULL,
  here_json TEXT NOT NULL,
  checked_at INTEGER NOT NULL
);
