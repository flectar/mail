-- Newest-first queue of the Gmail messages whose bodies download ahead of
-- time (#253), as idx_messages_jmap_pending_body is JMAP's.
CREATE INDEX idx_messages_gmail_pending_body
  ON messages(account_id, date DESC, id DESC)
  WHERE gm_msgid IS NOT NULL AND body_state = 'none';
