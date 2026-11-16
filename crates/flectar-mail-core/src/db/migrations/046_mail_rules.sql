-- Rules that sort new mail as it arrives in an Inbox (#238), tried in
-- position order.
CREATE TABLE mail_rules (
  id INTEGER PRIMARY KEY,
  -- The account whose mail a rule sorts, or NULL for every account's.
  account_id INTEGER REFERENCES accounts(id) ON DELETE CASCADE,
  position INTEGER NOT NULL,
  name TEXT NOT NULL,
  enabled INTEGER NOT NULL,
  match_all INTEGER NOT NULL,
  -- RuleCondition and RuleAction lists, as JSON.
  conditions_json TEXT NOT NULL,
  actions_json TEXT NOT NULL,
  stop INTEGER NOT NULL,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE INDEX idx_mail_rules_position ON mail_rules(position, id);
