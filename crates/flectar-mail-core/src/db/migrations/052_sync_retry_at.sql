-- When an account's sync asks its server again after the server asked to
-- wait (#251), in milliseconds since the epoch, so the app can say when
-- sync resumes. NULL unless the account is waiting.
ALTER TABLE accounts ADD COLUMN sync_retry_at INTEGER;
