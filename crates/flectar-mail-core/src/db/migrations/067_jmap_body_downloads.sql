-- Newest-first queue of the JMAP messages whose bodies download ahead of time
-- (#253), as idx_messages_pending_body_page is IMAP's.
CREATE INDEX idx_messages_jmap_pending_body
  ON messages(account_id, date DESC, id DESC)
  WHERE jmap_blob_id IS NOT NULL AND body_state = 'none';
