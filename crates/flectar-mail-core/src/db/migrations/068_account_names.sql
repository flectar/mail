-- The name the user gave an account, such as "Work" or "Shop". Lists of
-- accounts show it instead of the address, and people the account writes to
-- never see it. NULL shows the provider's name for the account, else its
-- address. Synced.
ALTER TABLE accounts ADD COLUMN name TEXT CHECK (name IS NULL OR length(name) > 0);
