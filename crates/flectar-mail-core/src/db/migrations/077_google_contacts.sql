-- Google Contacts sync over CardDAV signs in with the Gmail account's own
-- OAuth tokens instead of an app password (#423).
ALTER TABLE carddav_config ADD COLUMN kind TEXT NOT NULL DEFAULT 'generic'
  CHECK (kind IN ('generic', 'google'));
