-- When an account moves to another server or protocol, a mailbox is
-- recreated, or a folder disappears from an IMAP server, the mail that came
-- from a server loses its link to it. Each message waits here to be matched
-- by Message-ID with its copy on the current server. Once the sync has gone
-- through the server's mail, those still waiting are no longer there.
ALTER TABLE messages ADD COLUMN relink_pending INTEGER NOT NULL DEFAULT 0
  CHECK (relink_pending IN (0, 1));
CREATE INDEX idx_messages_relink_pending ON messages(account_id)
  WHERE relink_pending = 1;

-- Mail an earlier switch already left waits the same way: IMAP mail without
-- a UID, and JMAP mail without an Email id. The account's own drafts and the
-- sent mail only this device holds keep their place.
UPDATE messages SET relink_pending = 1
WHERE NOT EXISTS (SELECT 1 FROM drafts_meta d WHERE d.message_id = messages.id)
  AND ((uid IS NULL
        AND account_id IN (SELECT id FROM accounts WHERE mail_protocol = 'imap')
        AND (jmap_id IS NOT NULL OR is_outgoing = 0))
    OR (jmap_id IS NULL
        AND account_id IN (SELECT id FROM accounts WHERE mail_protocol = 'jmap')
        AND (uid IS NOT NULL OR is_outgoing = 0)));

-- A folder the server no longer lists. It is not synced, and goes once the
-- mail in it has been matched elsewhere or removed.
ALTER TABLE folders ADD COLUMN missing_on_server INTEGER NOT NULL DEFAULT 0
  CHECK (missing_on_server IN (0, 1));
