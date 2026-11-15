-- Drafts AI agents wrote through the local MCP server. An agent may change
-- its draft only while the draft is as it left it. The user's save removes
-- the row, and `saved_at`, the draft's date after the agent's save, catches
-- changes made any other way.
CREATE TABLE agent_drafts (
  draft_id INTEGER PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
  -- The name the agent's app gave itself.
  agent TEXT NOT NULL,
  saved_at INTEGER NOT NULL
);
