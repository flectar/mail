-- "Mark messages as read when opened" was on or off. It now says when an
-- opened message counts as read: at once, after two or five seconds, or never.
-- Off becomes never.
UPDATE app_settings
   SET value = json_set(
         json_remove(value, '$.markReadOnOpen'),
         '$.markRead',
         CASE json_extract(value, '$.markReadOnOpen')
           WHEN 0 THEN 'never'
           ELSE 'immediately'
         END
       )
 WHERE key = 'settings'
   AND json_valid(value)
   AND json_type(value, '$.markReadOnOpen') IS NOT NULL;
