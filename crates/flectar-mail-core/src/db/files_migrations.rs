//! Independent, versioned metadata store for remote files. Mail attachments
//! remain owned by the mail database and are never copied into this schema.
use crate::error::{CoreError, Result};
use rusqlite::Connection;
const MIGRATIONS: &[&str] = &[
    include_str!("migrations/files_001.sql"),
    include_str!("migrations/files_002.sql"),
];
pub const LATEST_VERSION: i64 = MIGRATIONS.len() as i64;
pub fn run(conn: &mut Connection) -> Result<()> {
    let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if version > LATEST_VERSION {
        return Err(CoreError::Other(
            "Files database was created by a newer application.".into(),
        ));
    }
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(version as usize) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", (index + 1) as i64)?;
        tx.commit()?;
    }
    Ok(())
}
