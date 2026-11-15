-- What AI agents did through the local MCP server, for the activity log in
-- Settings. Entries name the tool, account and folder and count the items,
-- but never hold subjects, addresses or message text. Only the newest
-- entries are kept.
CREATE TABLE agent_activity (
  id INTEGER PRIMARY KEY,
  occurred_at INTEGER NOT NULL,
  -- The name the agent's app gave itself.
  agent TEXT NOT NULL,
  tool TEXT NOT NULL,
  account_id INTEGER REFERENCES accounts(id) ON DELETE SET NULL,
  folder_id INTEGER REFERENCES folders(id) ON DELETE SET NULL,
  items INTEGER NOT NULL DEFAULT 0,
  -- 'done', 'refused' (outside the agent's access) or 'failed'.
  outcome TEXT NOT NULL
);
CREATE INDEX idx_agent_activity_occurred ON agent_activity(occurred_at);
