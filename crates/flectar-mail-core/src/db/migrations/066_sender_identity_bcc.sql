-- The Bcc a JMAP server keeps for each identity (RFC 8621 section 6), as a
-- JSON list of addresses, for the mail written from it (#252).
ALTER TABLE sender_identities ADD COLUMN bcc TEXT NOT NULL DEFAULT '[]';
