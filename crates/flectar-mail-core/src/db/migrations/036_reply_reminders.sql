-- "Remind me if no reply": unless someone replies to a sent message by a
-- time the user chose, its conversation comes back to the top of the inbox.
-- A reminder names the message by its Message-ID, which every device finds,
-- so it follows the user to every device through Flectar Mail Sync.
CREATE TABLE reply_reminders (
  sync_id TEXT PRIMARY KEY
    CHECK (length(sync_id) = 32 AND sync_id NOT GLOB '*[^0-9a-f]*'),
  account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  -- The sent message's Message-ID, as messages.message_id holds it.
  message_id TEXT NOT NULL,
  remind_at INTEGER NOT NULL,
  -- When the time came here without a reply: the conversation shows at the
  -- top of the inbox until a reply arrives or the user dismisses it.
  due_at INTEGER,
  -- The conversation here, once the message has arrived.
  thread_id INTEGER REFERENCES threads(id) ON DELETE SET NULL
);
CREATE INDEX idx_reply_reminders_thread ON reply_reminders(thread_id);
CREATE UNIQUE INDEX idx_reply_reminders_message ON reply_reminders(account_id, message_id);

-- A conversation that came back, from a snooze or a reminder, lists as if
-- its newest message arrived then.
ALTER TABLE threads ADD COLUMN resurfaced_at INTEGER;
