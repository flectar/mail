-- Sidebar badges in lists without conversations read every unread message.
-- This index holds only those, with the columns the badges read, so a badge
-- refresh never visits mail that is read.
CREATE INDEX IF NOT EXISTS idx_messages_unread
    ON messages(account_id, folder_id, thread_id, is_starred)
    WHERE is_read = 0 AND is_draft = 0;
