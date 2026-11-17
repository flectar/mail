-- A mail account's own cloud storage, Google Drive or OneDrive, sends files by link through
-- the account's sign-in. `kind` is 'nextcloud', 'google_drive' or 'onedrive', and
-- `account_id` names the account for the last two. An account has one at most.
ALTER TABLE link_services ADD COLUMN kind TEXT NOT NULL DEFAULT 'nextcloud';
ALTER TABLE link_services ADD COLUMN account_id INTEGER REFERENCES accounts(id) ON DELETE CASCADE;
CREATE UNIQUE INDEX link_services_account ON link_services(account_id)
    WHERE account_id IS NOT NULL;
