-- What an AI agent changed in each activity log entry, so the user can undo
-- it from the log (#237). Moves and marks are queued actions, which are kept
-- for a week after they're done. Label changes are listed one by one, since
-- local labels change without a queued action.
CREATE TABLE agent_activity_actions (
  activity_id INTEGER NOT NULL REFERENCES agent_activity(id) ON DELETE CASCADE,
  action_id INTEGER NOT NULL REFERENCES pending_actions(id) ON DELETE CASCADE,
  PRIMARY KEY (activity_id, action_id)
);
CREATE INDEX idx_agent_activity_actions_action ON agent_activity_actions(action_id);
CREATE TABLE agent_activity_labels (
  activity_id INTEGER NOT NULL REFERENCES agent_activity(id) ON DELETE CASCADE,
  message_id INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
  label_id INTEGER NOT NULL REFERENCES labels(id) ON DELETE CASCADE,
  -- 1 when the agent added the label, 0 when it removed it.
  added INTEGER NOT NULL,
  PRIMARY KEY (activity_id, message_id, label_id)
);
CREATE INDEX idx_agent_activity_labels_message ON agent_activity_labels(message_id);
CREATE INDEX idx_agent_activity_labels_label ON agent_activity_labels(label_id);
-- When the user undid the entry's changes.
ALTER TABLE agent_activity ADD COLUMN undone_at INTEGER;
