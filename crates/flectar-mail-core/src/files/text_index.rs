//! Bounded local text extraction. Never downloads attachments just to index
//! them, executes a document, or renders active HTML/SVG content.
use crate::{Core, error::Result};
use rusqlite::{OptionalExtension, params};
use std::path::Path;
const MAX_TEXT_BYTES: usize = 1024 * 1024;
#[derive(PartialEq)]
struct TextSource {
    message: i64,
    mime: String,
    blob: Option<String>,
    part: Option<String>,
    section: Option<String>,
    path: Option<String>,
}
fn source(c: &rusqlite::Connection, id: i64) -> rusqlite::Result<Option<TextSource>> {
    c.query_row(
        "SELECT message_id,COALESCE(mime_type,''),jmap_blob_id,part_id,imap_section,file_path FROM attachments WHERE id=?1",
        [id], |r| Ok(TextSource {
            message: r.get(0)?, mime: r.get(1)?, blob: r.get(2)?,
            part: r.get(3)?, section: r.get(4)?, path: r.get(5)?,
        }),
    ).optional()
}
fn store_text(
    c: &mut rusqlite::Connection,
    id: i64,
    expected: TextSource,
    text: String,
) -> Result<bool> {
    let tx = c.transaction()?;
    // Reading the file yields to synchronization/account removal. Never put
    // old bytes into the index of a replaced attachment or a reused row ID.
    if source(&tx, id)?.as_ref() != Some(&expected) {
        return Ok(false);
    }
    tx.execute("DELETE FROM attachment_text_fts WHERE rowid=?1", [id])?;
    tx.execute(
        "INSERT INTO attachment_text_fts(rowid,content) VALUES(?1,?2)",
        params![id, text],
    )?;
    tx.commit()?;
    Ok(true)
}
fn supported(mime: &str) -> bool {
    mime.starts_with("text/")
        || matches!(
            mime,
            "application/json" | "application/xml" | "application/javascript"
        )
        || mime.ends_with("+json")
        || mime.ends_with("+xml")
}
impl Core {
    pub(crate) async fn index_attachment_text(&self, id: i64, path: &Path) -> Result<bool> {
        let Some(expected) = self.db.read(move |c| Ok(source(c, id)?)).await? else {
            return Ok(false);
        };
        if !supported(&expected.mime) || expected.path.as_deref() != path.to_str() {
            return Ok(false);
        }
        let bytes = match crate::file_io::read(path, MAX_TEXT_BYTES, "attachment text").await {
            Ok(bytes) => bytes,
            Err(_) => return Ok(false),
        };
        let (encoding, skip) =
            encoding_rs::Encoding::for_bom(&bytes).unwrap_or((encoding_rs::UTF_8, 0));
        let (text, _, errors) = encoding.decode(&bytes[skip..]);
        if errors || text.contains('\0') {
            return Ok(false);
        }
        let text = text.into_owned();
        self.db
            .write(move |c| store_text(c, id, expected, text))
            .await
    }

    pub async fn index_cached_attachment_text(&self, account: Option<i64>) -> Result<usize> {
        let mut after = 0;
        let mut count = 0;
        loop {
            let rows=self.db.read(move |c| {let mut q=c.prepare("SELECT a.id,a.file_path FROM attachments a JOIN messages m ON m.id=a.message_id WHERE a.id>?1 AND a.file_path IS NOT NULL AND (?2 IS NULL OR m.account_id=?2) ORDER BY a.id LIMIT 100")?;Ok(q.query_map(params![after,account],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?)}).await?;
            if rows.is_empty() {
                break;
            }
            for (id, path) in rows {
                after = id;
                count += usize::from(self.index_attachment_text(id, Path::new(&path)).await?);
            }
        }
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn text_search_is_scoped_and_removed_with_attachment() {
        let root = tempfile::tempdir().unwrap();
        let core = Core::start_mail_ui(crate::config::Paths::for_tests(root.path()))
            .await
            .unwrap();
        core.db.write(|c|{
            crate::db::testutil::seed_account(c);
            c.execute("INSERT OR IGNORE INTO folders(id,account_id,imap_name) VALUES(1,1,'INBOX')",[])?;
            c.execute("INSERT INTO messages(id,account_id,folder_id,subject,from_addr,date) VALUES(1,1,1,'Meeting','sender@test.dev',0)",[])?;
            c.execute("INSERT INTO attachments(id,message_id,filename,mime_type,size,is_inline) VALUES(1,1,'notes.txt','text/plain',20,0),(2,1,'document.pdf','application/pdf',20,0)",[])?;
            Ok(())
        }).await.unwrap();
        let path = root.path().join("text");
        tokio::fs::write(&path, "Café project azimuth")
            .await
            .unwrap();
        let cached = path.to_string_lossy().into_owned();
        core.db
            .write(move |c| {
                c.execute("UPDATE attachments SET file_path=?1", [cached])?;
                Ok(())
            })
            .await
            .unwrap();
        let stale_path = root.path().join("superseded-copy");
        tokio::fs::write(&stale_path, "staleprivateword")
            .await
            .unwrap();
        assert!(!core.index_attachment_text(1, &stale_path).await.unwrap());
        assert!(core.index_attachment_text(1, &path).await.unwrap());
        assert!(!core.index_attachment_text(2, &path).await.unwrap());
        core.db
            .write(|c| {
                let stale = source(c, 1)?.unwrap();
                c.execute(
                    "UPDATE attachments SET jmap_blob_id='replacement' WHERE id=1",
                    [],
                )?;
                assert!(!store_text(c, 1, stale, "staleprivateword".into())?);
                assert_eq!(
                    c.query_row("SELECT count(*) FROM attachment_text_fts", [], |r| r
                        .get::<_, i64>(0))?,
                    0
                );
                Ok(())
            })
            .await
            .unwrap();
        assert!(core.index_attachment_text(1, &path).await.unwrap());
        assert_eq!(
            core.attachment_files(Some(1), "azimuth".into(), None)
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(
            core.attachment_files(Some(2), "azimuth".into(), None)
                .await
                .unwrap()
                .is_empty()
        );
        core.db
            .write(|c| {
                c.execute("DELETE FROM attachments WHERE id=1", [])?;
                Ok(())
            })
            .await
            .unwrap();
        assert!(
            core.attachment_files(None, "azimuth".into(), None)
                .await
                .unwrap()
                .is_empty()
        );
    }
}
