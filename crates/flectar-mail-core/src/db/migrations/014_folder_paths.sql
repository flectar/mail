-- A folder path is the full hierarchical name for every protocol, not only
-- for IMAP mailboxes.
ALTER TABLE folders RENAME COLUMN imap_name TO path;
