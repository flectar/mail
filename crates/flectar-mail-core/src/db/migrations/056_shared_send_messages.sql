-- An IMAP or POP3 account keeps its drafts on the device that wrote them,
-- so a send scheduled from one shares the message itself, end-to-end
-- encrypted like every Flectar Mail Sync record, for another of the user's
-- devices to send.
-- The message as it goes out, with its Bcc header, which delivery strips.
ALTER TABLE synced_sends ADD COLUMN mime BLOB;
-- Another device's message as a draft here, while it waits to be sent.
ALTER TABLE synced_sends ADD COLUMN shadow_id INTEGER REFERENCES messages(id) ON DELETE SET NULL;
