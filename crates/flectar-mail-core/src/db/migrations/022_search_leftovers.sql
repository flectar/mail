-- Removing an account left the search entries of its messages behind, and a
-- message stored later can be given one of their ids.
DELETE FROM messages_fts WHERE rowid NOT IN (SELECT id FROM messages);
