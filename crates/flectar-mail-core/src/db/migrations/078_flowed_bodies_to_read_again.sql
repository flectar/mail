-- Bodies kept before text sent as format=flowed (RFC 3676) was read back
-- into its paragraphs. Each is read again when it is next opened: from its
-- original kept on this device, or else from the server. A flowed line ends
-- in a space, so only text with such a line can be one of them. Drafts keep
-- what was written here.
INSERT OR IGNORE INTO bodies_to_read_again (message_id)
SELECT b.message_id
FROM message_bodies b
JOIN messages m ON m.id = b.message_id
WHERE m.body_state = 'cached'
  AND m.is_draft = 0
  AND (instr(b.text_body, ' ' || char(13)) > 0 OR instr(b.text_body, ' ' || char(10)) > 0);
