-- What the server last reported for each IMAP folder: STATUS MESSAGES and
-- UNSEEN, next to the UIDNEXT and UIDVALIDITY already kept. A folder that
-- still reports all four has had nothing arrive, leave or change its read
-- state since, so a sync need not open it.
ALTER TABLE folders ADD COLUMN server_messages INTEGER;
ALTER TABLE folders ADD COLUMN server_unseen INTEGER;
