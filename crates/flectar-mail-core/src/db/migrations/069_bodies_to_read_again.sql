-- Messages whose kept body was read before the body showed the messages a
-- message carries (#455). Each is read again when it is next opened: from
-- its original kept on this device, or else from the server.
CREATE TABLE bodies_to_read_again (
  message_id INTEGER PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE
);

INSERT INTO bodies_to_read_again (message_id)
SELECT DISTINCT a.message_id
FROM attachments a
JOIN messages m ON m.id = a.message_id
WHERE m.body_state = 'cached'
  AND lower(trim(a.mime_type)) IN ('message/rfc822', 'message/global');
