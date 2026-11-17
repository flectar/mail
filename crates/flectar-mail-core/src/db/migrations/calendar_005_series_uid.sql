-- The series an occurrence synced from Exchange belongs to, by the UID its
-- invitation carried, so that a recurring meeting known from mail meets its
-- occurrences there (MS-OXCICAL keeps that UID inside Exchange's own id).
ALTER TABLE calendar_events ADD COLUMN series_uid TEXT;
CREATE INDEX idx_events_series_uid ON calendar_events(account_id, series_uid)
  WHERE series_uid IS NOT NULL;
