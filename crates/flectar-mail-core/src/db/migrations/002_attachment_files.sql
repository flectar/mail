-- Search metadata only. Keep the file library independent of message bodies.
CREATE VIRTUAL TABLE attachment_files_fts USING fts5(
    filename, media_type, sender, subject,
    tokenize = 'unicode61 remove_diacritics 2', prefix = '2 3 4'
);
INSERT INTO attachment_files_fts(rowid, filename, media_type, sender, subject)
SELECT a.id, a.filename, a.mime_type,
       COALESCE(m.from_name,'') || ' ' || COALESCE(m.from_addr,''), m.subject
FROM attachments a JOIN messages m ON m.id = a.message_id;
CREATE TRIGGER attachment_files_insert AFTER INSERT ON attachments BEGIN
    INSERT INTO attachment_files_fts(rowid, filename, media_type, sender, subject)
    SELECT new.id, new.filename, new.mime_type,
           COALESCE(m.from_name,'') || ' ' || COALESCE(m.from_addr,''), m.subject
    FROM messages m WHERE m.id = new.message_id;
END;
CREATE TRIGGER attachment_files_delete AFTER DELETE ON attachments BEGIN
    DELETE FROM attachment_files_fts WHERE rowid = old.id;
END;
CREATE TRIGGER attachment_files_update AFTER UPDATE OF filename,mime_type,message_id ON attachments BEGIN
    DELETE FROM attachment_files_fts WHERE rowid = old.id;
    INSERT INTO attachment_files_fts(rowid, filename, media_type, sender, subject)
    SELECT new.id, new.filename, new.mime_type,
           COALESCE(m.from_name,'') || ' ' || COALESCE(m.from_addr,''), m.subject
    FROM messages m WHERE m.id = new.message_id;
END;
CREATE TRIGGER attachment_files_message_update AFTER UPDATE OF from_name,from_addr,subject ON messages BEGIN
    UPDATE attachment_files_fts
    SET sender = COALESCE(new.from_name,'') || ' ' || COALESCE(new.from_addr,''), subject = new.subject
    WHERE rowid IN (SELECT id FROM attachments WHERE message_id = new.id);
END;
