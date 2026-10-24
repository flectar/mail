//! SQLite access in WAL mode. Mail uses dedicated writer and reader threads;
//! calendar uses one serialized connection because its workload is smaller.
//! Async callers submit closures and await results, so repository code remains
//! plain synchronous rusqlite.

pub mod calendar_migrations;
pub mod files_migrations;
pub mod migrations;
pub mod repo;
pub mod snapshot;

use crate::{
    error::{CoreError, Result},
    models::{
        AppIcon, StartupPresentation, normalized_window_frame, normalized_workspace_list_pane_width,
    },
};
use rusqlite::config::DbConfig;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use std::path::{Path, PathBuf};
use tokio::sync::{mpsc, oneshot};

type Job = Box<dyn FnOnce(Result<&mut Connection>) + Send + 'static>;

enum Command {
    Run(Job),
    Release(oneshot::Sender<()>),
    Inspect(oneshot::Sender<Result<Option<SqliteCacheUsage>>>),
}

/// SQLite's allocated page-cache bytes for one live connection. This is not
/// resident memory: mmap pages and allocator retention must be measured apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SqliteCacheUsage {
    pub cache_bytes: i64,
    pub cache_limit_kib: i64,
    pub mmap_limit_bytes: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DbConnectionCacheUsage {
    pub role: &'static str,
    pub usage: SqliteCacheUsage,
}

struct LazyStore {
    path: PathBuf,
    kind: StoreKind,
    opened: tokio::sync::OnceCell<Db>,
}

// Backpressure keeps a burst of sync/UI work from retaining an unbounded
// number of captured query arguments while SQLite is busy with a long write.
// The queue is deliberately much larger than normal foreground fan-out, yet
// small enough to bound queued job count. Captured payloads and producers
// awaiting admission need their own byte budgets.
const JOB_QUEUE_CAPACITY: usize = 128;
const MAIL_READER_CACHE_KIB: i64 = 2_048;
const MAIL_READER_MMAP_BYTES: i64 = 2_097_152;

#[derive(Clone, Copy)]
enum StoreKind {
    Mail,
    Calendar,
    Files,
}

fn configure_query_connection(conn: &Connection, kind: StoreKind) -> Result<()> {
    conn.pragma_update(None, "query_only", "ON")?;
    if matches!(kind, StoreKind::Mail) {
        // Populated FTS search can fill the reader's old 8 MiB page cache.
        // The writer keeps that capacity for sync transactions, while the
        // reader uses a smaller private cache and mapped working set for
        // list, badges, and search.
        conn.pragma_update(None, "cache_size", -MAIL_READER_CACHE_KIB)?;
        conn.pragma_update(None, "mmap_size", MAIL_READER_MMAP_BYTES)?;
    }
    Ok(())
}

#[derive(Clone)]
pub struct Db {
    write_tx: Option<mpsc::Sender<Command>>,
    read_tx: Option<mpsc::Sender<Command>>,
    lazy: Option<std::sync::Arc<LazyStore>>,
}

fn spawn_conn_thread(
    path: std::path::PathBuf,
    name: &str,
    kind: StoreKind,
    query_only: bool,
) -> Result<mpsc::Sender<Command>> {
    let (tx, mut rx) = mpsc::channel::<Command>(JOB_QUEUE_CAPACITY);
    let conn = open_connection(&path, kind)?;
    if query_only {
        configure_query_connection(&conn, kind)?;
    }
    std::thread::Builder::new()
        .name(format!("flectar-mail-db-{name}"))
        .spawn(move || {
            let mut connection = Some(conn);
            while let Some(command) = rx.blocking_recv() {
                match command {
                    Command::Release(done) => {
                        // Serialized behind earlier work: never close a connection
                        // while a transaction or result is still using it.
                        connection.take();
                        let _ = done.send(());
                    }
                    Command::Inspect(reply) => {
                        let _ = reply.send(connection.as_ref().map(sqlite_cache_usage).transpose());
                    }
                    Command::Run(job) => {
                        let result = (|| {
                            if connection.is_none() {
                                let conn = open_connection(&path, kind)?;
                                if query_only {
                                    configure_query_connection(&conn, kind)?;
                                }
                                connection = Some(conn);
                            }
                            Ok(())
                        })();
                        job(result.map(|()| connection.as_mut().unwrap()));
                    }
                }
            }
        })
        .map_err(CoreError::Io)?;
    Ok(tx)
}

fn sqlite_cache_usage(conn: &Connection) -> Result<SqliteCacheUsage> {
    let mut cache_bytes = 0i64;
    let mut highwater = 0i64;
    // SAFETY: This runs on the connection's dedicated thread with an exclusive
    // borrow of its owner. SQLite only writes the two output integers here;
    // neither the connection nor its handle escapes this call.
    let code = unsafe {
        rusqlite::ffi::sqlite3_db_status64(
            conn.handle(),
            rusqlite::ffi::SQLITE_DBSTATUS_CACHE_USED,
            &mut cache_bytes,
            &mut highwater,
            0,
        )
    };
    if code != rusqlite::ffi::SQLITE_OK {
        return Err(CoreError::Other(format!(
            "SQLite cache diagnostic failed with code {code}"
        )));
    }
    let cache_size: i64 = conn.pragma_query_value(None, "cache_size", |row| row.get(0))?;
    let cache_limit_kib = if cache_size < 0 {
        cache_size.saturating_neg()
    } else {
        let page_size: i64 = conn.pragma_query_value(None, "page_size", |row| row.get(0))?;
        cache_size.saturating_mul(page_size).saturating_add(1023) / 1024
    };
    Ok(SqliteCacheUsage {
        cache_bytes,
        cache_limit_kib,
        mmap_limit_bytes: conn.pragma_query_value(None, "mmap_size", |row| row.get(0))?,
    })
}

fn open_connection(path: &Path, kind: StoreKind) -> Result<Connection> {
    let conn = Connection::open(path)?;
    conn.busy_timeout(std::time::Duration::from_secs(10))?;
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
    // to retain roughly 128 MiB after a large mailbox scan. The mail reader
    // gets a smaller override in configure_query_connection; the writer keeps
    // capacity for sync transactions. mmap remains a reclaimable file-backed
    // fast path, but touched pages still appear in process RSS.
    let (mmap_size, cache_size_kib) = match kind {
        StoreKind::Mail => (8_388_608i64, 8_192i64),
        // Calendar queries touch a tiny working set compared with mailbox/FTS
        // scans. A smaller cache avoids paying the mail profile twice.
        StoreKind::Calendar => (0i64, 2_048i64),
        StoreKind::Files => (0i64, 2_048i64),
    };
    conn.pragma_update(None, "mmap_size", mmap_size)?;
    conn.pragma_update(None, "cache_size", -cache_size_kib)?;
    // Large sorts should not compete with the renderer for resident memory.
    conn.pragma_update(None, "temp_store", "FILE")?;
    Ok(conn)
}

/// Best-effort read of the presentation state needed for the first frame.
///
/// Normal startup remains responsible for opening, migrating, and validating
/// the complete settings blob. This deliberately performs one zero-wait,
/// read-only query so presentation state can be applied before the window is
/// shown without putting database startup or mailbox loading on that path.
pub(crate) fn startup_presentation(path: &Path) -> StartupPresentation {
    let Some(value) = startup_settings_value(path) else {
        return StartupPresentation::default();
    };
    StartupPresentation {
        workspace_list_pane_width: value
            .get("workspaceListPaneWidth")
            .and_then(serde_json::Value::as_i64)
            .map(normalized_workspace_list_pane_width),
        window_frame: value
            .get("windowFrame")
            .and_then(serde_json::Value::as_str)
            .map(|frame| normalized_window_frame(frame).to_owned()),
        close_to_tray: value
            .get("closeToTray")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or_default(),
        app_icon: value
            .get("appIcon")
            .and_then(serde_json::Value::as_str)
            .and_then(AppIcon::parse)
            .unwrap_or_default(),
    }
}

fn startup_settings_value(path: &Path) -> Option<serde_json::Value> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    conn.busy_timeout(std::time::Duration::ZERO).ok()?;
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)
        .ok()?;
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false)
        .ok()?;

    let json = conn
        .query_row(
            "SELECT value FROM app_settings WHERE key = 'settings'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .ok()??;
    serde_json::from_str::<serde_json::Value>(&json).ok()
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
    fn deferred(path: PathBuf, kind: StoreKind) -> Self {
        Self {
            write_tx: None,
            read_tx: None,
            lazy: Some(std::sync::Arc::new(LazyStore {
                path,
                kind,
                opened: tokio::sync::OnceCell::new(),
            })),
        }
    }

    pub fn deferred_calendar(path: PathBuf) -> Self {
        Self::deferred(path, StoreKind::Calendar)
    }
    pub fn deferred_files(path: PathBuf) -> Self {
        Self::deferred(path, StoreKind::Files)
    }

    pub fn is_open(&self) -> bool {
        self.lazy
            .as_ref()
            .is_none_or(|store| store.opened.get().is_some())
    }

    async fn ready(&self) -> Result<&Self> {
        if let Some(store) = &self.lazy {
            if store.opened.get().is_none() {
                // Keep initialization alive if its first UI caller is cancelled.
                // Otherwise a second caller could race the abandoned migration.
                let initializing = store.clone();
                tokio::spawn(async move {
                    initializing
                        .opened
                        .get_or_try_init(|| async {
                            let path = initializing.path.clone();
                            let kind = initializing.kind;
                            tokio::task::spawn_blocking(move || match kind {
                                StoreKind::Calendar => Self::open_calendar(&path),
                                StoreKind::Files => Self::open_files(&path),
                                StoreKind::Mail => Self::open(&path),
                            })
                            .await
                            .map_err(|error| CoreError::Other(error.to_string()))?
                        })
                        .await
                        .map(|_| ())
                })
                .await
                .map_err(|error| CoreError::Other(error.to_string()))??;
            }
            Ok(store.opened.get().unwrap())
        } else {
            Ok(self)
        }
    }

    /// Release resident connections without opening an unused product store.
    /// A subsequent job reopens its connection with the normal profile.
    pub async fn release_idle_connections(&self) -> Result<()> {
        let db = match &self.lazy {
            Some(store) => match store.opened.get() {
                Some(db) => db,
                None => return Ok(()),
            },
            None => self,
        };
        let writer = db.write_tx.as_ref().unwrap();
        let reader = db.read_tx.as_ref().unwrap();
        for tx in [
            Some(writer),
            (!reader.same_channel(writer)).then_some(reader),
        ]
        .into_iter()
        .flatten()
        {
            let (done, wait) = oneshot::channel();
            tx.send(Command::Release(done))
                .await
                .map_err(|_| CoreError::Other("db thread gone".into()))?;
            wait.await
                .map_err(|_| CoreError::Other("db thread gone".into()))?;
        }
        Ok(())
    }

    /// Inspect currently live connections without opening a deferred store or
    /// reopening a connection released by `release_idle_connections`.
    /// SQLite reports allocated page-cache bytes, not mapped resident pages.
    pub async fn cache_usage(&self) -> Result<Vec<DbConnectionCacheUsage>> {
        let db = match &self.lazy {
            Some(store) => match store.opened.get() {
                Some(db) => db,
                None => return Ok(Vec::new()),
            },
            None => self,
        };
        let writer = db.write_tx.as_ref().unwrap();
        let reader = db.read_tx.as_ref().unwrap();
        let mut usage = Vec::with_capacity(2);
        for (role, tx) in [
            ("writer", Some(writer)),
            ("reader", (!reader.same_channel(writer)).then_some(reader)),
        ]
        .into_iter()
        .filter_map(|(role, tx)| tx.map(|tx| (role, tx)))
        {
            let (reply, wait) = oneshot::channel();
            tx.send(Command::Inspect(reply))
                .await
                .map_err(|_| CoreError::Other("db thread gone".into()))?;
            if let Some(usage_for_connection) = wait
                .await
                .map_err(|_| CoreError::Other("db thread gone".into()))??
            {
                usage.push(DbConnectionCacheUsage {
                    role,
                    usage: usage_for_connection,
                });
            }
        }
        Ok(usage)
    }

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
        Ok(Db {
            write_tx: Some(write_tx),
            read_tx: Some(read_tx),
            lazy: None,
        })
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
            read_tx: Some(write_tx.clone()),
            write_tx: Some(write_tx),
            lazy: None,
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
        Ok(Self {
            write_tx: Some(write_tx),
            read_tx: Some(read_tx),
            lazy: None,
        })
    }

    async fn call<T, F>(&self, tx: &mpsc::Sender<Command>, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    {
        let (reply_tx, reply_rx) = oneshot::channel();
        tx.send(Command::Run(Box::new(move |conn| {
            let _ = reply_tx.send(conn.and_then(f));
        })))
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
        let db = self.ready().await?;
        db.call(db.write_tx.as_ref().unwrap(), f).await
    }

    /// Run `f` in one transaction whose commit has reached stable storage when
    /// this returns. The store normally commits with `synchronous=NORMAL`,
    /// which a power loss can undo; this is for commits that let a server
    /// drop its own copy of the data.
    pub async fn write_durable<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&rusqlite::Transaction) -> Result<T> + Send + 'static,
    {
        self.write(move |conn| {
            // The level can only change outside a transaction.
            let previous: i64 = conn.pragma_query_value(None, "synchronous", |row| row.get(0))?;
            conn.pragma_update(None, "synchronous", "FULL")?;
            let result = conn.transaction().map_err(Into::into).and_then(|tx| {
                let value = f(&tx)?;
                tx.commit()?;
                Ok(value)
            });
            // Once the commit stands, failing to restore the usual level only
            // costs speed; reporting an error would misstate the commit.
            if let Err(error) = conn.pragma_update(None, "synchronous", previous) {
                tracing::warn!(%error, "could not restore the database sync level");
            }
            result
        })
        .await
    }

    /// Run a read-only closure on the reader connection.
    pub async fn read<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    {
        let db = self.ready().await?;
        db.call(db.read_tx.as_ref().unwrap(), f).await
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

    /// Settings of a test account that connects with TLS.
    pub fn account_settings() -> crate::models::AccountSettings {
        use crate::models::{AccountSettings, ConnectionSecurity, MailConnectionSettings};
        AccountSettings::new(MailConnectionSettings::new(
            ConnectionSecurity::Tls,
            ConnectionSecurity::Tls,
        ))
    }

    /// Account 1 with an inbox folder 1, the minimum most repos need.
    pub fn seed_account(c: &Connection) {
        c.execute(
            "INSERT INTO accounts (id, email, provider, auth_kind, username,
             incoming_host, incoming_port, smtp_host, smtp_port, created_at, mail_protocol, settings_json)
             VALUES (1,'me@test.dev','generic','password','me','h',993,'h',587,0,'imap','{\"connection\":{\"incomingSecurity\":\"tls\",\"smtpSecurity\":\"tls\"}}')",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO folders (id, account_id, path, role) VALUES (1,1,'INBOX','inbox')",
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
    async fn cache_diagnostic_tracks_live_connections_without_reopening_them() {
        let temp = tempfile::tempdir().unwrap();
        let deferred = Db::deferred_files(temp.path().join("files.db"));
        assert!(deferred.cache_usage().await.unwrap().is_empty());
        assert!(!temp.path().join("files.db").exists());

        let mail = Db::open(&temp.path().join("mail.db")).unwrap();
        let usage = mail.cache_usage().await.unwrap();
        assert_eq!(usage.len(), 2);
        assert_eq!(usage[0].role, "writer");
        assert_eq!(usage[1].role, "reader");
        for connection in usage {
            assert!(connection.usage.cache_bytes > 0);
            assert_eq!(
                connection.usage.cache_limit_kib,
                if connection.role == "reader" {
                    2_048
                } else {
                    8_192
                }
            );
            assert_eq!(
                connection.usage.mmap_limit_bytes,
                if connection.role == "reader" {
                    2_097_152
                } else {
                    8_388_608
                }
            );
        }
        mail.release_idle_connections().await.unwrap();
        assert!(mail.cache_usage().await.unwrap().is_empty());

        let calendar = Db::deferred_calendar(temp.path().join("calendar.db"));
        assert!(calendar.cache_usage().await.unwrap().is_empty());
        calendar.read(|_| Ok(())).await.unwrap();
        let usage = calendar.cache_usage().await.unwrap();
        assert_eq!(usage.len(), 1);
        assert_eq!(usage[0].usage.cache_limit_kib, 2_048);
        assert_eq!(usage[0].usage.mmap_limit_bytes, 0);
    }

    #[tokio::test]
    async fn deferred_store_opens_once_and_releases_without_losing_writes() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("files.db");
        let db = Db::deferred_files(path.clone());
        db.release_idle_connections().await.unwrap();
        assert!(!path.exists());
        assert!(!db.is_open());
        let (left, right) = tokio::join!(
            db.read(|conn| Ok(conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))?)),
            db.read(|conn| Ok(conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))?)),
        );
        assert_eq!(left.unwrap(), right.unwrap());
        db.write(|conn| {
            conn.execute_batch(
                "CREATE TABLE lifecycle_test(value INTEGER); INSERT INTO lifecycle_test VALUES(7);",
            )?;
            Ok(())
        })
        .await
        .unwrap();
        db.release_idle_connections().await.unwrap();
        let value = db
            .read(|conn| {
                Ok(conn.query_row("SELECT value FROM lifecycle_test", [], |r| {
                    r.get::<_, i64>(0)
                })?)
            })
            .await
            .unwrap();
        assert_eq!(value, 7);
        assert!(
            db.read(|conn| {
                conn.execute("DELETE FROM lifecycle_test", [])?;
                Ok(())
            })
            .await
            .is_err()
        );
        db.write(|conn| {
            conn.execute("UPDATE lifecycle_test SET value=9", [])?;
            Ok(())
        })
        .await
        .unwrap();
        assert_eq!(
            db.read(
                |conn| Ok(conn.query_row("SELECT value FROM lifecycle_test", [], |r| r
                    .get::<_, i64>(0))?)
            )
            .await
            .unwrap(),
            9
        );
    }

    #[tokio::test]
    async fn deferred_calendar_retries_after_an_open_failure() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("calendar.db");
        std::fs::create_dir(&path).unwrap();
        let db = Db::deferred_calendar(path.clone());
        assert!(db.read(|_| Ok(())).await.is_err());
        std::fs::remove_dir(&path).unwrap();
        db.read(|conn| {
            assert!(conn.is_autocommit());
            Ok(())
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn mail_reader_is_query_only_and_uses_the_bounded_cache_profile() {
        let temp = tempfile::tempdir().unwrap();
        let db = Db::open(&temp.path().join("mail.db")).unwrap();

        let (query_only, mmap_size, cache_size, defensive, trusted_schema): (
            i64,
            i64,
            i64,
            bool,
            i64,
        ) = db
            .read(|conn| {
                Ok((
                    conn.pragma_query_value(None, "query_only", |row| row.get(0))?,
                    conn.pragma_query_value(None, "mmap_size", |row| row.get(0))?,
                    conn.pragma_query_value(None, "cache_size", |row| row.get(0))?,
                    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)?,
                    conn.pragma_query_value(None, "trusted_schema", |row| row.get(0))?,
                ))
            })
            .await
            .unwrap();

        assert_eq!(query_only, 1);
        assert_eq!(mmap_size, 2_097_152);
        assert_eq!(cache_size, -2_048);
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

#[cfg(test)]
mod startup_preference_tests {
    use super::startup_presentation;
    use crate::models::{AppIcon, MIN_WORKSPACE_LIST_PANE_WIDTH};
    use rusqlite::Connection;

    #[test]
    fn startup_width_is_read_without_opening_the_full_store() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("mail.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE app_settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO app_settings (key, value)
             VALUES ('settings', '{\"workspaceListPaneWidth\": 672}');",
        )
        .unwrap();
        drop(conn);

        assert_eq!(
            startup_presentation(&path).workspace_list_pane_width,
            Some(672)
        );
    }

    #[test]
    fn startup_width_is_bounded_and_missing_state_is_non_fatal() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("missing.db");
        assert_eq!(startup_presentation(&missing), Default::default());

        let path = temp.path().join("mail.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE app_settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO app_settings (key, value)
             VALUES ('settings', '{\"workspaceListPaneWidth\": 120}');",
        )
        .unwrap();
        drop(conn);

        assert_eq!(
            startup_presentation(&path).workspace_list_pane_width,
            Some(MIN_WORKSPACE_LIST_PANE_WIDTH)
        );
    }

    #[test]
    fn startup_window_frame_is_normalized() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("mail.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE app_settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO app_settings (key, value)
             VALUES ('settings', '{\"windowFrame\": \"integrated\"}');",
        )
        .unwrap();
        assert_eq!(
            startup_presentation(&path).window_frame.as_deref(),
            Some("integrated")
        );

        conn.execute(
            "UPDATE app_settings SET value = '{\"windowFrame\": \"future\"}'",
            [],
        )
        .unwrap();
        assert_eq!(
            startup_presentation(&path).window_frame.as_deref(),
            Some("system")
        );
    }

    #[test]
    fn startup_close_to_tray_requires_an_explicit_boolean() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("mail.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE app_settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO app_settings (key, value)
             VALUES ('settings', '{\"closeToTray\": true}');",
        )
        .unwrap();
        assert!(startup_presentation(&path).close_to_tray);

        for value in [
            "{}",
            "{\"closeToTray\": \"true\"}",
            "{\"closeToTray\": false}",
        ] {
            conn.execute("UPDATE app_settings SET value = ?1", [value])
                .unwrap();
            assert!(!startup_presentation(&path).close_to_tray, "{value}");
        }
    }

    #[test]
    fn startup_app_icon_falls_back_to_the_default() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("mail.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE app_settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO app_settings (key, value)
             VALUES ('settings', '{\"appIcon\": \"graphite\"}');",
        )
        .unwrap();
        assert_eq!(startup_presentation(&path).app_icon, AppIcon::Graphite);

        for value in ["{}", "{\"appIcon\": \"future\"}", "{\"appIcon\": 2}"] {
            conn.execute("UPDATE app_settings SET value = ?1", [value])
                .unwrap();
            assert_eq!(
                startup_presentation(&path).app_icon,
                AppIcon::Orange,
                "{value}"
            );
        }
    }
}
