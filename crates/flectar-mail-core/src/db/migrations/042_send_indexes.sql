-- Scheduled sends are looked up by their message, and deleting a message
-- checks the shared sends that keep it as another device's draft.
CREATE INDEX idx_pending_actions_message ON pending_actions(message_id);
CREATE INDEX idx_synced_sends_shadow ON synced_sends(shadow_id);
