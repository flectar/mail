-- Unread messages of a folder that are only on the server: older than the
-- account's mail history, or not downloaded yet. A sync sets it when it has
-- caught up with the folder, from the server's count of unread messages and
-- those kept here, so mail read here before the server heard of it never
-- shows as missing.
ALTER TABLE folders ADD COLUMN server_only_unseen INTEGER;

-- The Mailbox state a JMAP account's counts were last brought up to, so a
-- sync asks only for the counts of mailboxes that changed since.
ALTER TABLE jmap_sync_state ADD COLUMN count_state TEXT;

-- The history id a Gmail account's Inbox count was last read at.
ALTER TABLE gmail_sync_state ADD COLUMN count_history_id TEXT;
