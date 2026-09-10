//! SQLite access in WAL mode. Mail uses dedicated writer and reader threads;
//! calendar uses one serialized connection because its workload is smaller.
//! Async callers submit closures and await results, so repository code remains
//! plain synchronous rusqlite.

pub mod calendar_migrations;
pub mod files_migrations;
pub mod migrations;
pub mod repo;
pub mod snapshot;

use crate::error::{CoreError, Result};
use rusqlite::Connection;
use rusqlite::config::DbConfig;
use std::path::{Path, PathBuf};
use tokio::sync::{mpsc, oneshot};

type Job = Box<dyn FnOnce(&mut Connection) + Send + 'static>;

// Backpressure keeps a burst of sync/UI work from retaining an unbounded
// number of captured query arguments while SQLite is busy with a long write.
// The queue is deliberately much larger than normal foreground fan-out, yet
// small enough to put a deterministic ceiling on pending-job memory.
const JOB_QUEUE_CAPACITY: usize = 128;

#[derive(Clone, Copy)]
enum StoreKind {
    Mail,
    Calendar,
    Files,
}

#[derive(Clone)]
pub struct Db {
    write_tx: mpsc::Sender<Job>,
    read_tx: mpsc::Sender<Job>,
}

fn spawn_conn_thread(
    path: std::path::PathBuf,
    name: &str,
    kind: StoreKind,
    query_only: bool,
) -> Result<mpsc::Sender<Job>> {
    let (tx, mut rx) = mpsc::channel::<Job>(JOB_QUEUE_CAPACITY);
    let mut conn = open_connection(&path, kind)?;
    if query_only {
        conn.pragma_update(None, "query_only", "ON")?;
    }
    std::thread::Builder::new()
        .name(format!("flectar-mail-db-{name}"))
        .spawn(move || {
            while let Some(job) = rx.blocking_recv() {
                job(&mut conn);
            }
        })
        .map_err(CoreError::Io)?;
    Ok(tx)
}

fn open_connection(path: &Path, kind: StoreKind) -> Result<Connection> {
    let conn = Connection::open(path)?;
    // Treat the database as data, never executable configuration. Defensive
    // mode blocks dangerous schema/file pragmas and an untrusted schema keeps
    // schema objects from invoking non-innocuous application functions.
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)?;
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    // Bound each connection's private page cache. The old 64 MiB setting was
    // applied independently to the reader and writer, allowing SQLite alone
    // to retain roughly 128 MiB after a large mailbox scan. mmap remains a
    // reclaimable file-backed fast path, but its visible window is bounded.
    let (mmap_size, cache_size_kib) = match kind {
        StoreKind::Mail => (67_108_864i64, 8_192i64),
        // Calendar queries touch a tiny working set compared with mailbox/FTS
        // scans. A smaller cache avoids paying the mail profile twice.
        StoreKind::Calendar => (16_777_216i64, 2_048i64),
        StoreKind::Files => (33_554_432i64, 4_096i64),
    };
    conn.pragma_update(None, "mmap_size", mmap_size)?;
    conn.pragma_update(None, "cache_size", -cache_size_kib)?;
    // Large sorts should not compete with the renderer for resident memory.
    conn.pragma_update(None, "temp_store", "FILE")?;
    conn.busy_timeout(std::time::Duration::from_secs(10))?;
    Ok(conn)
}

fn sqlite_sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

fn quick_check(conn: &Connection, store: &str) -> Result<()> {
    let result: String = conn.pragma_query_value(None, "quick_check", |row| row.get(0))?;
    if result.eq_ignore_ascii_case("ok") {
        Ok(())
    } else {
        Err(CoreError::Other(format!(
            "{store} database quick_check failed: {result}"
        )))
    }
}

fn prepare_store(
    path: &Path,
    kind: StoreKind,
    store: &str,
    migrate: impl FnOnce(&mut Connection) -> Result<()>,
) -> Result<()> {
    // A clean final SQLite close removes WAL/journal sidecars. Their presence
    // before opening is therefore a cheap dirty-shutdown signal. Do the O(n)
    // scan only then, or after a schema change, never on every normal launch.
    let recovered_dirty_state =
        sqlite_sidecar(path, "-wal").exists() || sqlite_sidecar(path, "-journal").exists();
    let mut conn = open_connection(path, kind)?;
    let version_before: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if recovered_dirty_state {
        quick_check(&conn, store)?;
    }
    migrate(&mut conn)?;
    // SQLite 3.46+ bounds the ANALYZE work performed by optimize. The 0x10000
    // bit also considers tables that this short-lived preparation connection
    // has not queried, keeping statistics useful after large syncs/imports.
    conn.execute_batch("PRAGMA optimize=0x10002;")?;
    let version_after: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version_after != version_before {
        quick_check(&conn, store)?;
    }
    Ok(())
}

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Run migrations on a throwaway connection before the threads start.
        prepare_store(path, StoreKind::Mail, "mail", |conn| {
            migrations::run(conn)?;
            repo::contacts::backfill_folded(conn)
        })?;
        let write_tx = spawn_conn_thread(path.to_path_buf(), "writer", StoreKind::Mail, false)?;
        let read_tx = spawn_conn_thread(path.to_path_buf(), "reader", StoreKind::Mail, true)?;
        Ok(Db { write_tx, read_tx })
    }

    /// Open the physically separate calendar store.
    pub fn open_calendar(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        prepare_store(path, StoreKind::Calendar, "calendar", |conn| {
            calendar_migrations::run(conn)
        })?;
        // Calendar operations are short and low-volume. One serialized
        // connection keeps the calendar independent from mailbox/FTS writes
        // while avoiding a fourth SQLite cache and a fourth dedicated thread.
        let write_tx =
            spawn_conn_thread(path.to_path_buf(), "calendar", StoreKind::Calendar, false)?;
        Ok(Db {
            read_tx: write_tx.clone(),
            write_tx,
        })
    }

    pub fn open_files(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        prepare_store(path, StoreKind::Files, "files", files_migrations::run)?;
        let write_tx =
            spawn_conn_thread(path.to_path_buf(), "files-writer", StoreKind::Files, false)?;
        let read_tx =
            spawn_conn_thread(path.to_path_buf(), "files-reader", StoreKind::Files, true)?;
        Ok(Self { write_tx, read_tx })
    }

    async fn call<T, F>(&self, tx: &mpsc::Sender<Job>, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    {
        let (reply_tx, reply_rx) = oneshot::channel();
        tx.send(Box::new(move |conn| {
            let _ = reply_tx.send(f(conn));
        }))
        .await
        .map_err(|_| CoreError::Other("db thread gone".into()))?;
        reply_rx
            .await
            .map_err(|_| CoreError::Other("db call dropped".into()))?
    }

    /// Run a write (or transactional) closure on the writer connection.
    pub async fn write<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    {
        self.call(&self.write_tx, f).await
    }

    /// Run a read-only closure on the reader connection.
    pub async fn read<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    {
        self.call(&self.read_tx, f).await
    }
}

#[cfg(test)]
pub(crate) mod testutil {
    use rusqlite::{Connection, params};

    /// In-memory DB with all migrations applied.
    pub fn conn() -> Connection {
        let mut c = Connection::open_in_memory().unwrap();
        c.pragma_update(None, "foreign_keys", "ON").unwrap();
        super::migrations::run(&mut c).unwrap();
        c
    }

    /// In-memory DB with the standalone calendar schema applied.
    pub fn calendar_conn() -> Connection {
        let mut c = Connection::open_in_memory().unwrap();
        c.pragma_update(None, "foreign_keys", "ON").unwrap();
        super::calendar_migrations::run(&mut c).unwrap();
        c
    }

    /// Account 1 with an inbox folder 1, the minimum most repos need.
    pub fn seed_account(c: &Connection) {
        c.execute(
            "INSERT INTO accounts (id, email, provider, auth_kind, username,
             imap_host, imap_port, smtp_host, smtp_port, created_at)
             VALUES (1,'me@test.dev','imap','password','me','h',993,'h',587,0)",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO folders (id, account_id, imap_name, role) VALUES (1,1,'INBOX','inbox')",
            [],
        )
        .unwrap();
    }

    /// One incoming message in its own thread; returns (thread_id, message_id).
    pub fn seed_message(
        c: &Connection,
        from_addr: &str,
        subject: &str,
        is_automated: bool,
    ) -> (i64, i64) {
        c.execute(
            "INSERT INTO threads (account_id, subject_norm, unread_count, last_message_at)
             VALUES (1, ?1, 1, 1000)",
            params![subject.to_lowercase()],
        )
        .unwrap();
        let thread_id = c.last_insert_rowid();
        c.execute(
            "INSERT INTO messages (thread_id, account_id, folder_id, uid, message_id, subject,
             from_addr, date, is_read, is_automated, is_draft, is_outgoing)
             VALUES (?1, 1, 1, ?1, 'mid-' || ?1, ?2, ?3, 1000, 0, ?4, 0, 0)",
            params![thread_id, subject, from_addr, is_automated as i64],
        )
        .unwrap();
        let msg_id = c.last_insert_rowid();
        (thread_id, msg_id)
    }
}

#[cfg(test)]
mod connection_profile_tests {
    use super::Db;
    use rusqlite::config::DbConfig;

    #[tokio::test]
    async fn mail_reader_is_query_only_and_uses_the_bounded_cache_profile() {
        let temp = tempfile::tempdir().unwrap();
        let db = Db::open(&temp.path().join("mail.db")).unwrap();

        let (query_only, cache_size, defensive, trusted_schema): (i64, i64, bool, i64) = db
            .read(|conn| {
                Ok((
                    conn.pragma_query_value(None, "query_only", |row| row.get(0))?,
                    conn.pragma_query_value(None, "cache_size", |row| row.get(0))?,
                    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)?,
                    conn.pragma_query_value(None, "trusted_schema", |row| row.get(0))?,
                ))
            })
            .await
            .unwrap();

        assert_eq!(query_only, 1);
        assert_eq!(cache_size, -8_192);
        assert!(defensive);
        assert_eq!(trusted_schema, 0);
        assert!(
            db.read(|conn| {
                conn.execute("DELETE FROM app_settings", [])?;
                Ok(())
            })
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn calendar_store_uses_the_small_cache_profile() {
        let temp = tempfile::tempdir().unwrap();
        let db = Db::open_calendar(&temp.path().join("calendar.db")).unwrap();

        let cache_size: i64 = db
            .read(|conn| Ok(conn.pragma_query_value(None, "cache_size", |row| row.get(0))?))
            .await
            .unwrap();

        assert_eq!(cache_size, -2_048);
    }
}
