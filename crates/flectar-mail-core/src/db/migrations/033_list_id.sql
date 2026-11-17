-- The mailing list each message came from (its RFC 2919 List-Id), so the
-- mail of one list can be found together, as after unsubscribing from it.
ALTER TABLE messages ADD COLUMN list_id TEXT;
CREATE INDEX idx_messages_list ON messages(account_id, list_id) WHERE list_id IS NOT NULL;
