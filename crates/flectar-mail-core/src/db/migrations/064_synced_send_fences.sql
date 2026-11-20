-- The device that may have handed a shared send's message to the mail
-- server without the result being known to the others yet. A device
-- writes it, through the sync server, before it sends; until it is
-- cleared, another device sends the message only after checking that it
-- didn't go out. Synced.
ALTER TABLE synced_sends ADD COLUMN sending TEXT;
