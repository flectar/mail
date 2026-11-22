-- The role the account's server suggests for each folder, kept apart from
-- `role`, which follows the folders the user chose by hand (#252).
ALTER TABLE folders ADD COLUMN detected_role TEXT;
UPDATE folders SET detected_role = role;
