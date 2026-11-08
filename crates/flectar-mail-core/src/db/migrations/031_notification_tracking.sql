-- A shown new-mail notification stays tracked, so it can be withdrawn once its
-- message is read, moved or deleted, here or on another device. Deleting the
-- message or its account leaves the row, with the id the notification was
-- shown under, until the notification is withdrawn.
CREATE TABLE notification_outbox_next (
  id INTEGER PRIMARY KEY,
  account_id INTEGER NOT NULL,
  message_id INTEGER UNIQUE REFERENCES messages(id) ON DELETE SET NULL,
  notified_message_id INTEGER NOT NULL,
  thread_id INTEGER REFERENCES threads(id) ON DELETE SET NULL,
  sender_name TEXT,
  sender_addr TEXT,
  subject TEXT NOT NULL DEFAULT '',
  -- Mail from a person rather than a robot: only that plays a sound.
  audible INTEGER NOT NULL DEFAULT 0 CHECK (audible IN (0, 1)),
  state TEXT NOT NULL DEFAULT 'pending'
    CHECK (state IN ('pending','delivering','delivered','suppressed','withdrawn')),
  -- Shown as part of its account's "new messages" summary rather than on its
  -- own.
  in_summary INTEGER NOT NULL DEFAULT 0 CHECK (in_summary IN (0, 1)),
  attempts INTEGER NOT NULL DEFAULT 0,
  not_before INTEGER,
  created_at INTEGER NOT NULL,
  claimed_at INTEGER,
  delivered_at INTEGER,
  suppressed_at INTEGER,
  suppression_reason TEXT,
  withdrawn_at INTEGER,
  last_error TEXT
);

-- Until now nothing showed notifications on most platforms, so the queue holds
-- mail nobody was told about. Settle it rather than announcing it all at once
-- on the next launch.
INSERT INTO notification_outbox_next (
  id, account_id, message_id, notified_message_id, thread_id, sender_name,
  sender_addr, subject, state, attempts, not_before, created_at, claimed_at,
  delivered_at, suppressed_at, suppression_reason, last_error
)
SELECT id, account_id, message_id, message_id, thread_id, sender_name,
       sender_addr, subject,
       CASE WHEN state IN ('pending', 'delivering') THEN 'suppressed' ELSE state END,
       attempts,
       CASE WHEN state IN ('pending', 'delivering') THEN NULL ELSE not_before END,
       created_at, claimed_at, delivered_at,
       CASE WHEN state IN ('pending', 'delivering')
            THEN CAST(strftime('%s', 'now') AS INTEGER) * 1000
            ELSE suppressed_at END,
       CASE WHEN state IN ('pending', 'delivering')
            THEN 'queued before notifications were shown'
            ELSE suppression_reason END,
       last_error
FROM notification_outbox;

DROP TABLE notification_outbox;
ALTER TABLE notification_outbox_next RENAME TO notification_outbox;
CREATE INDEX idx_notification_outbox_due
  ON notification_outbox(state, not_before, created_at);
