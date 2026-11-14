-- Mail lists shown oldest first, by the app's key for each list. A list of
-- one account, folder or label goes with it, so a later one that takes the
-- same id starts newest first.
CREATE TABLE oldest_first_lists (
  list_key TEXT PRIMARY KEY NOT NULL,
  account_id INTEGER REFERENCES accounts(id) ON DELETE CASCADE,
  folder_id INTEGER REFERENCES folders(id) ON DELETE CASCADE,
  label_id INTEGER REFERENCES labels(id) ON DELETE CASCADE
) WITHOUT ROWID;
