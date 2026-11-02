-- An account's calendars may come from several servers: its provider's
-- calendar plus any number of CalDAV servers, such as a shared mailbox's
-- calendar reached with the account's own sign-in. Each server becomes a
-- connection of its own, holding what the one-per-account configuration
-- row held, and every collection records the connection that syncs it.

CREATE TABLE calendar_connections (
  id INTEGER PRIMARY KEY,
  account_id INTEGER NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN ('google', 'generic', 'microsoft')),
  base_url TEXT NOT NULL,
  username TEXT NOT NULL DEFAULT '',
  principal_url TEXT,
  home_set_url TEXT,
  enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
  last_error TEXT
);
CREATE INDEX idx_calendar_connections_account ON calendar_connections(account_id);
-- Provider calendars sign in with the account's own tokens, so each exists
-- once per account.
CREATE UNIQUE INDEX idx_calendar_connections_provider
  ON calendar_connections(account_id, kind) WHERE kind <> 'generic';

INSERT INTO calendar_connections
  (account_id, kind, base_url, username, principal_url, home_set_url, enabled, last_error)
SELECT account_id, kind, base_url, COALESCE(username, ''), principal_url, home_set_url,
       enabled, last_error
FROM caldav_config
ORDER BY account_id;

-- CalDAV app passwords were kept in one keyring entry per account. Each
-- such connection waits here until the application has moved that secret
-- to the connection's own entry.
CREATE TABLE calendar_secret_moves (
  connection_id INTEGER PRIMARY KEY
    REFERENCES calendar_connections(id) ON DELETE CASCADE
);
INSERT INTO calendar_secret_moves (connection_id)
SELECT id FROM calendar_connections WHERE kind = 'generic';

-- A collection without a configured server could never sync; its events
-- stay as local events.
UPDATE calendar_events
SET calendar_id = NULL, caldav_href = NULL, etag = NULL, dirty = 0
WHERE calendar_id IN (
  SELECT id FROM calendars
  WHERE account_id NOT IN (SELECT account_id FROM calendar_connections)
);

CREATE TABLE calendars_new (
  id INTEGER PRIMARY KEY,
  account_id INTEGER NOT NULL,
  connection_id INTEGER NOT NULL REFERENCES calendar_connections(id),
  url TEXT NOT NULL,
  display_name TEXT,
  color TEXT,
  ctag TEXT,
  sync_token TEXT,
  read_only INTEGER NOT NULL DEFAULT 0 CHECK (read_only IN (0, 1)),
  enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
  is_default INTEGER NOT NULL DEFAULT 0 CHECK (is_default IN (0, 1)),
  last_synced_at INTEGER,
  UNIQUE(account_id, url)
);

INSERT INTO calendars_new (
  id, account_id, connection_id, url, display_name, color, ctag, sync_token,
  read_only, enabled, is_default, last_synced_at
)
SELECT c.id, c.account_id, k.id, c.url, c.display_name, c.color, c.ctag,
       c.sync_token, c.read_only, c.enabled, c.is_default, c.last_synced_at
FROM calendars c
JOIN calendar_connections k ON k.account_id = c.account_id;

-- Renaming the new table re-checks every trigger, and one reading the
-- dropped table would fail that check, so the event triggers over calendars
-- are recreated below as created by the baseline.
DROP TRIGGER calendar_event_account_insert;
DROP TRIGGER calendar_event_account_update;
DROP TABLE calendars;
ALTER TABLE calendars_new RENAME TO calendars;
DROP TABLE caldav_config;

CREATE UNIQUE INDEX idx_calendars_one_default
  ON calendars(account_id) WHERE is_default = 1;
CREATE INDEX idx_calendars_connection ON calendars(connection_id);

CREATE TRIGGER calendar_event_account_insert
BEFORE INSERT ON calendar_events
WHEN NEW.calendar_id IS NOT NULL AND NOT EXISTS (
  SELECT 1 FROM calendars
  WHERE id = NEW.calendar_id AND account_id = NEW.account_id
)
BEGIN
  SELECT RAISE(ABORT, 'calendar event account mismatch');
END;
CREATE TRIGGER calendar_event_account_update
BEFORE UPDATE OF calendar_id, account_id ON calendar_events
WHEN NEW.calendar_id IS NOT NULL AND NOT EXISTS (
  SELECT 1 FROM calendars
  WHERE id = NEW.calendar_id AND account_id = NEW.account_id
)
BEGIN
  SELECT RAISE(ABORT, 'calendar event account mismatch');
END;
CREATE TRIGGER calendar_owner_immutable
BEFORE UPDATE OF account_id ON calendars
WHEN NEW.account_id != OLD.account_id AND EXISTS (
  SELECT 1 FROM calendar_events WHERE calendar_id = OLD.id
)
BEGIN
  SELECT RAISE(ABORT, 'calendar owner has events');
END;

-- A collection belongs to the account that owns its connection.
CREATE TRIGGER calendar_connection_account_insert
BEFORE INSERT ON calendars
WHEN NOT EXISTS (
  SELECT 1 FROM calendar_connections
  WHERE id = NEW.connection_id AND account_id = NEW.account_id
)
BEGIN
  SELECT RAISE(ABORT, 'calendar connection account mismatch');
END;
CREATE TRIGGER calendar_connection_account_update
BEFORE UPDATE OF connection_id, account_id ON calendars
WHEN NOT EXISTS (
  SELECT 1 FROM calendar_connections
  WHERE id = NEW.connection_id AND account_id = NEW.account_id
)
BEGIN
  SELECT RAISE(ABORT, 'calendar connection account mismatch');
END;
CREATE TRIGGER calendar_connection_owner_immutable
BEFORE UPDATE OF account_id ON calendar_connections
WHEN NEW.account_id != OLD.account_id AND EXISTS (
  SELECT 1 FROM calendars WHERE connection_id = OLD.id
)
BEGIN
  SELECT RAISE(ABORT, 'calendar connection owner has calendars');
END;
