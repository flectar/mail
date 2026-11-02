-- The one-time code a received message asks its reader to enter, such as a
-- sign-in code, offered for copying in the message list. NULL when it holds
-- none. The conversion after this step looks through the stored mail.
ALTER TABLE messages ADD COLUMN verification_code TEXT;
