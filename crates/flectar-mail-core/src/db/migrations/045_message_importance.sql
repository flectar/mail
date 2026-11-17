-- How important the sender marked a message (#236): 1 high, -1 low, and 0
-- normal or unmarked, from its Importance or X-Priority header, or what a
-- draft asks for. A conversation counts its high-importance messages, which
-- the mail list marks.
ALTER TABLE messages ADD COLUMN importance INTEGER NOT NULL DEFAULT 0;
ALTER TABLE threads ADD COLUMN important_count INTEGER NOT NULL DEFAULT 0;
