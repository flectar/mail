-- How each connection's server schedules for the account (RFC 6638), as
-- last asked: whether it sends attendees' answers to organizers itself, the
-- addresses it does that for (a JSON list), and when it was asked. NULL
-- until then.
ALTER TABLE calendar_connections ADD COLUMN auto_schedule INTEGER CHECK (auto_schedule IN (0, 1));
ALTER TABLE calendar_connections ADD COLUMN schedule_addresses TEXT;
ALTER TABLE calendar_connections ADD COLUMN schedule_checked_at INTEGER;
