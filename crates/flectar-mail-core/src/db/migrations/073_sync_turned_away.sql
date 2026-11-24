-- The sync server turned this device away, saying it was removed from its
-- group ('removed') or the group was deleted ('deleted'), and the chain this
-- device verified shows neither. A removed device can no longer read the
-- chain, so this is all it hears of a removal made on another device, and
-- also what a server that lies would say. The device keeps its keys and
-- everything it syncs, shows that sync stopped, and asks the server again
-- now and then; a sync that gets through clears it.
ALTER TABLE device_sync ADD COLUMN turned_away TEXT
  CHECK (turned_away IS NULL OR turned_away IN ('removed', 'deleted'));
