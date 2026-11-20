-- Why this device could not send another device's shared send, which it
-- then leaves to the others, instead of failing it on every device. Never
-- synced.
ALTER TABLE synced_sends ADD COLUMN failed_here TEXT;
