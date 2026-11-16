-- Where rules stopped for a message's body (#238): the rule to go on from,
-- the notification held back meanwhile and the headers fetched with the
-- message, as JSON. NULL when no rule waits.
ALTER TABLE messages ADD COLUMN rules_waiting TEXT;
CREATE INDEX idx_messages_rules_waiting ON messages(account_id)
  WHERE rules_waiting IS NOT NULL;
