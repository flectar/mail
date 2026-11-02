-- A full JMAP sync that stops midway resumes instead of starting over.
-- full_sync_state is the Email state taken when it began, which becomes the
-- checkpoint once every message is stored. Until then the messages it has
-- stored are listed here, so a later attempt only fetches the rest.
ALTER TABLE jmap_sync_state ADD COLUMN full_sync_state TEXT;

CREATE TABLE jmap_full_sync_stored (
  account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  email_id TEXT NOT NULL,
  PRIMARY KEY (account_id, email_id)
) WITHOUT ROWID;
