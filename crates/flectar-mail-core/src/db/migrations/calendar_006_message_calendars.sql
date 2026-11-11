-- The calendar each stored message carries (its text/calendar part): an
-- invitation, its update or cancellation, or an answer to one. The card
-- above the message reads it from here, also for messages whose calendar
-- doesn't change the user's, such as answers to their own invitations and
-- mail in Junk.
CREATE TABLE message_calendars (
  message_id INTEGER PRIMARY KEY,
  account_id INTEGER NOT NULL,
  calendar TEXT NOT NULL
);
