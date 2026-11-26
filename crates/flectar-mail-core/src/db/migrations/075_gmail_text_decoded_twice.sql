-- Gmail hands a part's text over inline converted to UTF-8 while its header
-- still names the charset the sender used, and bodies read from their parts
-- were decoded with that charset a second time, which garbles every
-- character beyond ASCII (#537). Those bodies download again; the one kept
-- shows until then. A body read from its original was decoded once, and
-- drafts keep what was written here.
UPDATE messages SET body_state = 'none', mime_plan_json = NULL
WHERE body_state = 'cached'
  AND raw_path IS NULL
  AND is_draft = 0
  AND gm_msgid IS NOT NULL
  AND account_id IN (SELECT id FROM accounts WHERE mail_protocol = 'gmail')
  AND id IN (
    SELECT message_id FROM message_bodies
    WHERE length(CAST(text_body AS BLOB)) <> length(text_body)
       OR length(CAST(html_body AS BLOB)) <> length(html_body)
  );
