-- When the account's last complete sync started: everything its server had
-- received by then is stored here. Mail that arrives after it, while no copy
-- of the app is following the account, is what a check in the background
-- tells the user about.
ALTER TABLE accounts ADD COLUMN caught_up_at INTEGER;
