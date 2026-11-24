-- App keys, the Google and Microsoft app registrations entered in Sign-in
-- settings, which Flectar Mail Sync carries to the user's other devices of
-- the same kind along with their passwords and sign-ins. They travel sealed
-- under a key derived from the group's keys; this device's settings hold
-- the keys it uses.
CREATE TABLE app_key_shares (
  provider TEXT NOT NULL CHECK (provider IN ('gmail', 'microsoft')),
  -- The kind of device the keys are for, as for sign-ins.
  family TEXT NOT NULL,
  -- The record that carries them, named from the provider and family.
  record_id BLOB CHECK (length(record_id) = 16),
  -- The keys as the group has them, sealed, as JSON {"epoch", "data"};
  -- NULL while this device has none to share.
  sealed TEXT,
  -- A keyed digest of the keys this device last shared or took on.
  fingerprint BLOB,
  -- Keys saved on this device that should replace the group's.
  fresh INTEGER NOT NULL DEFAULT 0 CHECK (fresh IN (0, 1)),
  -- Keys another device shared, sealed, waiting for this device's settings.
  arrived TEXT,
  PRIMARY KEY (provider, family)
) STRICT;
