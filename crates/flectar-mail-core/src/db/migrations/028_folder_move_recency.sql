-- When mail was last moved into each folder, so the Move to picker can offer
-- the folders the user files into first.
ALTER TABLE folders ADD COLUMN moved_to_at INTEGER;
