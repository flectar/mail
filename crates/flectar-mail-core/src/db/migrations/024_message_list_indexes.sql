-- Mail lists without conversations page through messages newest first, like
-- the conversation timeline on threads: within an account, and across all
-- of them.
CREATE INDEX IF NOT EXISTS idx_messages_account_recent
    ON messages(account_id, date DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_messages_recent
    ON messages(date DESC, id DESC);
