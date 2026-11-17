-- Rules put mail in a tab of the Inbox (#238), in place of split rules, which
-- no release of the app could make. A conversation keeps the tab a rule or the
-- user put it in through every later sort.
ALTER TABLE threads ADD COLUMN pinned_tab TEXT;
UPDATE threads SET routed_tab = NULL WHERE routed_tab LIKE 'split:%';
DELETE FROM route_cache WHERE route_key LIKE 'split:%';
DROP TABLE split_rules;
