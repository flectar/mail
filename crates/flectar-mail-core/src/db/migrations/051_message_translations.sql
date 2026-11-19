-- Translations of messages (#240), kept on the device for some days so a
-- message translated again needs no request, and kept out of search. One
-- per message, language and service, as a translation from one service
-- isn't another's. A translation is of the HTML body when the message has
-- one, else of the text body.
CREATE TABLE message_translations (
  message_id INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
  target TEXT NOT NULL,
  service TEXT NOT NULL,
  -- The language the service found the message in, when it said.
  source TEXT,
  html TEXT,
  text TEXT,
  -- Milliseconds since the epoch.
  created_at INTEGER NOT NULL,
  PRIMARY KEY (message_id, target, service)
) WITHOUT ROWID;
CREATE INDEX idx_message_translations_created ON message_translations(created_at);
