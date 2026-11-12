-- When the stored version of an event was written (its DTSTAMP). Two versions
-- of an invitation with the same SEQUENCE are told apart by it: the later one
-- wins (RFC 5546 section 2.1.5).
ALTER TABLE calendar_events ADD COLUMN dtstamp INTEGER;
