-- JMAP header sync no longer asks for attachment lists: Stalwart can only
-- answer that by reading every message in full. A background pass lists the
-- attachments of messages that have some, and records here which blob the
-- stored list belongs to.
ALTER TABLE messages ADD COLUMN jmap_attachments_blob_id TEXT;

-- Until now header sync listed the attachments of every message it stored.
UPDATE messages SET jmap_attachments_blob_id = jmap_blob_id
  WHERE jmap_blob_id IS NOT NULL;

-- Newest-first queue of messages whose attachments still need listing.
CREATE INDEX idx_messages_jmap_attachments_pending
  ON messages(account_id, date DESC, id DESC)
  WHERE has_attachments = 1 AND body_state <> 'cached' AND jmap_blob_id IS NOT NULL
    AND jmap_attachments_blob_id IS NOT jmap_blob_id;
