-- A reply to a message that is not stored here, such as one a mailing list
-- archive's mailto link starts, keeps the Message-ID it answers (without
-- angle brackets) for its In-Reply-To and References headers.
ALTER TABLE drafts_meta ADD COLUMN in_reply_to_header TEXT;
