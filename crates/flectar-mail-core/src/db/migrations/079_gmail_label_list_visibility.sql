-- How Gmail's label list shows each label (#253), its labelListVisibility:
-- 'labelShow', 'labelShowIfUnread' or 'labelHide'. NULL until the labels are
-- listed again, which shows them all, as Gmail does a label without one.
ALTER TABLE gmail_labels ADD COLUMN list_visibility TEXT;
