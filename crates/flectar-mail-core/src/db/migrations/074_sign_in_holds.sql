-- Accounts this device had before it joined a sync group. Their passwords
-- and sign-ins stay on this device until the user shares them on the Sync
-- page: a code typed in at someone else's request could have joined a group
-- that isn't the user's. Accounts added here after joining share as the
-- group's setting says.
CREATE TABLE sign_in_holds (
  account_id INTEGER PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE
) STRICT;
