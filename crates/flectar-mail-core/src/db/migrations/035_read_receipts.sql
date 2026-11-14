-- Read receipt requests on incoming mail (RFC 8098). `receipt_to` holds the
-- Disposition-Notification-To addresses as JSON, NULL when the message asks
-- for none. `receipt_confirm` is set when section 2.1 wants the user's
-- confirmation before a receipt goes out: no Return-Path, one that differs
-- from the receipt address, or several receipt addresses.
-- `receipt_handled` records that a receipt was sent or declined, here or on
-- another device ($MDNSent, RFC 3503). It is never cleared.
ALTER TABLE messages ADD COLUMN receipt_to TEXT;
ALTER TABLE messages ADD COLUMN receipt_confirm INTEGER NOT NULL DEFAULT 0;
ALTER TABLE messages ADD COLUMN receipt_handled INTEGER NOT NULL DEFAULT 0;
