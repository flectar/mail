-- Attachment identities and text stay in the mail store.
ALTER TABLE attachments ADD COLUMN jmap_blob_id TEXT;
CREATE VIRTUAL TABLE attachment_text_fts USING fts5(
 content, tokenize='unicode61 remove_diacritics 2',prefix='2 3 4'
);
CREATE TRIGGER attachment_text_delete AFTER DELETE ON attachments BEGIN
 DELETE FROM attachment_text_fts WHERE rowid=old.id;
END;
CREATE TRIGGER attachment_text_invalidate AFTER UPDATE OF jmap_blob_id,part_id,imap_section ON attachments BEGIN
 DELETE FROM attachment_text_fts WHERE rowid=old.id;
END;
-- Existing accounts need one header reconciliation to populate part metadata.
UPDATE jmap_sync_state SET email_state=NULL;
