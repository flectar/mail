//! Coherent online snapshots of the physically separate mail, calendar and files
//! stores. All three writer queues are held at the same coordinated gate while
//! SQLite's online-backup API copies committed pages, so no application write
//! can land between the captured database states.

use super::Db;
use crate::error::{CoreError, Result};
use crate::file_io;
use rusqlite::{Connection, MAIN_DB, OpenFlags};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::mpsc;

const SNAPSHOT_FORMAT: &str = "flectar-mail-database-snapshot";
const SNAPSHOT_VERSION: u32 = 3;
const MAIL_FILE: &str = "mail.sqlite3";
/// Originals of mail that exists only on this device, as
/// `mail/<account id>/<file>`.
const DEVICE_MAIL_DIR: &str = "mail";
const CALENDAR_FILE: &str = "calendar.sqlite3";
const FILES_FILE: &str = "files.sqlite3";
const MANIFEST_FILE: &str = "manifest.json";
const GATE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const BACKUP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60 * 60);

fn coordination_error(phase: &str, error: impl std::fmt::Display) -> CoreError {
    CoreError::Other(format!(
        "database snapshot {phase} coordination failed: {error}"
    ))
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseSnapshotStore {
    pub file: String,
    pub schema_version: i64,
    pub sqlite_version: String,
    pub page_count: i64,
    pub page_size: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseSnapshotManifest {
    pub format: String,
    pub version: u32,
    pub created_at: String,
    pub core_version: String,
    pub mail: DatabaseSnapshotStore,
    pub calendar: DatabaseSnapshotStore,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<DatabaseSnapshotStore>,
    #[serde(default)]
    pub file_transfer_payloads: bool,
    /// The snapshot holds the originals of mail that exists only on this
    /// device (POP3 accounts) in [`DEVICE_MAIL_DIR`].
    #[serde(default)]
    pub device_mail_payloads: bool,
}

/// Copy the originals of mail that exists only on this device into the
/// snapshot, as the snapshot's own mail database references them. A file
/// removed since the snapshot was taken belonged to a message deleted since,
/// and is left out. Returns whether there were any.
async fn copy_device_mail(snapshot_db: &Path, staging: &Path) -> Result<bool> {
    let db = snapshot_db.to_owned();
    let originals = tokio::task::spawn_blocking(move || -> Result<Vec<(i64, String)>> {
        let conn = Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let mut statement = conn.prepare(
            "SELECT m.account_id, m.raw_path
             FROM messages m JOIN accounts a ON a.id = m.account_id
             WHERE a.mail_protocol = 'pop3' AND m.raw_path IS NOT NULL",
        )?;
        let rows = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    })
    .await
    .map_err(|error| CoreError::Other(error.to_string()))??;
    if originals.is_empty() {
        return Ok(false);
    }
    let root = staging.join(DEVICE_MAIL_DIR);
    for (account_id, original) in originals {
        let source = Path::new(&original);
        let Some(name) = source.file_name() else {
            continue;
        };
        let directory = root.join(account_id.to_string());
        if !tokio::fs::try_exists(&directory).await? {
            tokio::fs::create_dir_all(&directory).await?;
            file_io::restrict_permissions(&root, &[])?;
            file_io::restrict_permissions(&directory, &[])?;
        }
        let target = directory.join(name);
        match crate::file_io::copy(
            source,
            &target,
            crate::MAX_CACHED_MESSAGE_BYTES,
            "snapshot mail",
        )
        .await
        {
            Ok(_) => file_io::restrict_permissions(&directory, &[&target])?,
            Err(_) if !tokio::fs::try_exists(source).await? => {}
            Err(error) => return Err(error),
        }
    }
    Ok(true)
}

fn snapshot_store(
    conn: &Connection,
    destination: &Path,
    file: &str,
) -> Result<DatabaseSnapshotStore> {
    let schema_version = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let sqlite_version = conn.query_row("SELECT sqlite_version()", [], |row| row.get(0))?;
    let page_count = conn.pragma_query_value(None, "page_count", |row| row.get(0))?;
    let page_size = conn.pragma_query_value(None, "page_size", |row| row.get(0))?;
    conn.backup(MAIN_DB, destination, None)?;
    Ok(DatabaseSnapshotStore {
        file: file.to_owned(),
        schema_version,
        sqlite_version,
        page_count,
        page_size,
    })
}

fn verify_store(path: &Path, expected: &DatabaseSnapshotStore, store: &str) -> Result<()> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let schema: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if schema != expected.schema_version {
        return Err(CoreError::Other(format!(
            "{store} snapshot schema changed during verification: expected {}, found {schema}",
            expected.schema_version
        )));
    }
    let page_count: i64 = conn.pragma_query_value(None, "page_count", |row| row.get(0))?;
    let page_size: i64 = conn.pragma_query_value(None, "page_size", |row| row.get(0))?;
    if page_count != expected.page_count || page_size != expected.page_size {
        return Err(CoreError::Other(format!(
            "{store} snapshot page geometry changed during verification: expected {} pages of {} bytes, found {page_count} pages of {page_size} bytes",
            expected.page_count, expected.page_size
        )));
    }
    let integrity: String = conn.pragma_query_value(None, "integrity_check", |row| row.get(0))?;
    if !integrity.eq_ignore_ascii_case("ok") {
        return Err(CoreError::Other(format!(
            "{store} snapshot integrity_check failed: {integrity}"
        )));
    }
    let foreign_key_violation: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_foreign_key_check)",
        [],
        |row| row.get(0),
    )?;
    if foreign_key_violation {
        return Err(CoreError::Other(format!(
            "{store} snapshot contains a foreign-key violation"
        )));
    }
    Ok(())
}

/// Create a new snapshot directory atomically. The destination must not
/// already exist, which prevents an export from overwriting an older backup.
pub async fn create(
    mail_db: &Db,
    calendar_db: &Db,
    files_db: &Db,
    payload_root: Option<PathBuf>,
    destination: &Path,
) -> Result<DatabaseSnapshotManifest> {
    if destination.exists() {
        return Err(CoreError::Other(format!(
            "snapshot destination already exists: {}",
            destination.display()
        )));
    }
    let parent = destination.parent().ok_or_else(|| {
        CoreError::Other("database snapshot destination has no parent directory".into())
    })?;
    tokio::fs::create_dir_all(parent).await?;
    let staging = file_io::staging_directory(destination)?;
    tokio::fs::create_dir(&staging).await?;
    // Restrict access before SQLite creates any files. Applying permissions
    // only after the backup would briefly expose private mail under a common
    // 022 umask.
    file_io::restrict_permissions(&staging, &[])?;

    let mail_path = staging.join(MAIL_FILE);
    let calendar_path = staging.join(CALENDAR_FILE);
    let files_path = staging.join(FILES_FILE);
    let (ready_tx, ready_rx) = mpsc::channel::<()>();
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let mut starts = Vec::new();
    let mut releases = Vec::new();
    let mut jobs = Vec::new();
    for (db, path, name) in [
        (mail_db.clone(), mail_path.clone(), MAIL_FILE),
        (calendar_db.clone(), calendar_path.clone(), CALENDAR_FILE),
        (files_db.clone(), files_path.clone(), FILES_FILE),
    ] {
        let ready = ready_tx.clone();
        let done = done_tx.clone();
        let (start_tx, start_rx) = mpsc::sync_channel::<()>(0);
        let (release_tx, release_rx) = mpsc::sync_channel::<()>(0);
        starts.push(start_tx);
        releases.push(release_tx);
        jobs.push(async move {
            db.write(move |conn| {
                ready.send(()).map_err(|e| coordination_error("ready", e))?;
                start_rx
                    .recv_timeout(GATE_TIMEOUT)
                    .map_err(|e| coordination_error("start", e))?;
                let result = snapshot_store(conn, &path, name);
                done.send(())
                    .map_err(|e| coordination_error("completion", e))?;
                release_rx
                    .recv_timeout(BACKUP_TIMEOUT)
                    .map_err(|e| coordination_error("release", e))?;
                result
            })
            .await
        });
    }
    drop(ready_tx);
    drop(done_tx);
    let coordinator = tokio::task::spawn_blocking(move || -> Result<()> {
        for _ in 0..3 {
            ready_rx
                .recv_timeout(GATE_TIMEOUT)
                .map_err(|e| coordination_error("ready", e))?;
        }
        for start in starts {
            start.send(()).map_err(|e| coordination_error("start", e))?;
        }
        for _ in 0..3 {
            done_rx
                .recv_timeout(BACKUP_TIMEOUT)
                .map_err(|e| coordination_error("completion", e))?;
        }
        for release in releases {
            release
                .send(())
                .map_err(|e| coordination_error("release", e))?;
        }
        Ok(())
    });
    let (stores, coordination) = tokio::join!(futures::future::join_all(jobs), coordinator);
    let mut stores = stores.into_iter();
    let mail = stores.next().unwrap();
    let calendar = stores.next().unwrap();
    let files = stores.next().unwrap();

    let result = async {
        coordination.map_err(|error| coordination_error("task", error))??;
        let mail = mail?;
        let calendar = calendar?;
        let files = files?;
        verify_store(&files_path, &files, "files")?;
        verify_store(&mail_path, &mail, "mail")?;
        verify_store(&calendar_path, &calendar, "calendar")?;
        let manifest = DatabaseSnapshotManifest {
            format: SNAPSHOT_FORMAT.to_owned(),
            version: SNAPSHOT_VERSION,
            created_at: chrono::Utc::now().to_rfc3339(),
            core_version: env!("CARGO_PKG_VERSION").to_owned(),
            mail,
            calendar,
            files: Some(files),
            file_transfer_payloads: payload_root.is_some(),
            device_mail_payloads: copy_device_mail(&mail_path, &staging).await?,
        };
        if let Some(source) = payload_root
            && tokio::fs::try_exists(&source).await?
        {
            let mut queue = std::collections::VecDeque::from([(
                source,
                staging.join("file_transfers"),
                0usize,
            )]);
            while let Some((source, target, depth)) = queue.pop_front() {
                if depth > 3 {
                    return Err(CoreError::Other(
                        "Invalid transfer staging hierarchy.".into(),
                    ));
                }
                tokio::fs::create_dir_all(&target).await?;
                file_io::restrict_permissions(&target, &[])?;
                let mut entries = tokio::fs::read_dir(source).await?;
                while let Some(entry) = entries.next_entry().await? {
                    let kind = entry.file_type().await?;
                    let path = target.join(entry.file_name());
                    if kind.is_dir() {
                        queue.push_back((entry.path(), path, depth + 1));
                    } else if kind.is_file() {
                        crate::files::save_cached_file(&entry.path(), &path).await?;
                    } else {
                        return Err(CoreError::Other(
                            "Unexpected link in transfer staging.".into(),
                        ));
                    }
                }
            }
        }
        let manifest_path = staging.join(MANIFEST_FILE);
        tokio::fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest)?).await?;
        file_io::restrict_permissions(
            &staging,
            &[&mail_path, &calendar_path, &files_path, &manifest_path],
        )?;
        file_io::publish_directory(&staging, destination)?;
        Ok(manifest)
    }
    .await;

    if result.is_err() {
        let _ = tokio::fs::remove_dir_all(&staging).await;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{calendar_migrations, migrations};

    #[tokio::test]
    async fn snapshot_is_complete_verified_and_never_overwrites() {
        let source = tempfile::tempdir().unwrap();
        let output = tempfile::tempdir().unwrap();
        let mail = Db::open(&source.path().join("mail.db")).unwrap();
        let calendar = Db::open_calendar(&source.path().join("calendar.db")).unwrap();
        mail.write(|conn| {
            conn.execute(
                "INSERT INTO accounts (
                   id, email, provider, auth_kind, username, incoming_host, incoming_port,
                   smtp_host, smtp_port, created_at, mail_protocol, settings_json
                 ) VALUES (1, 'snapshot@example.test', 'generic', 'password', 'snapshot',
                           'imap.example.test', 993, 'smtp.example.test', 465, 0, 'imap', '{\"connection\":{\"incomingSecurity\":\"tls\",\"smtpSecurity\":\"tls\"}}')",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
        calendar
            .write(|conn| {
                conn.execute(
                    "INSERT INTO calendar_connections (id, account_id, kind, base_url)
                     VALUES (1, 1, 'generic', 'https://dav.example.test/')",
                    [],
                )?;
                conn.execute(
                    "INSERT INTO calendars (id, account_id, connection_id, url, display_name)
                     VALUES (1, 1, 1, 'local://snapshot', 'Snapshot')",
                    [],
                )?;
                Ok(())
            })
            .await
            .unwrap();

        let files = Db::open_files(&source.path().join("files.db")).unwrap();
        let destination = output.path().join("snapshot");
        let manifest = create(&mail, &calendar, &files, None, &destination)
            .await
            .unwrap();
        assert_eq!(manifest.mail.schema_version, migrations::LATEST_VERSION);
        assert_eq!(
            manifest.files.as_ref().unwrap().schema_version,
            crate::db::files_migrations::LATEST_VERSION
        );
        assert_eq!(
            manifest.calendar.schema_version,
            calendar_migrations::LATEST_VERSION
        );

        let stored: DatabaseSnapshotManifest =
            serde_json::from_slice(&std::fs::read(destination.join(MANIFEST_FILE)).unwrap())
                .unwrap();
        assert_eq!(stored, manifest);
        let copied_mail = Connection::open(destination.join(MAIL_FILE)).unwrap();
        let copied_calendar = Connection::open(destination.join(CALENDAR_FILE)).unwrap();
        assert_eq!(
            copied_mail
                .query_row("SELECT COUNT(*) FROM accounts", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            copied_calendar
                .query_row("SELECT COUNT(*) FROM calendars", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );

        let error = create(&mail, &calendar, &files, None, &destination)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("already exists"));
    }

    #[tokio::test]
    async fn snapshots_carry_the_originals_of_mail_kept_only_on_this_device() {
        let source = tempfile::tempdir().unwrap();
        let output = tempfile::tempdir().unwrap();
        let mail = Db::open(&source.path().join("mail.db")).unwrap();
        let calendar = Db::open_calendar(&source.path().join("calendar.db")).unwrap();
        let files = Db::open_files(&source.path().join("files.db")).unwrap();
        let kept = source.path().join("kept.pop3.eml");
        let deleted = source.path().join("deleted.pop3.eml");
        let server_copy = source.path().join("cache.eml");
        std::fs::write(&kept, b"Subject: kept\r\n\r\nbody\r\n").unwrap();
        std::fs::write(&server_copy, b"Subject: cached\r\n\r\n").unwrap();
        let paths =
            [kept.clone(), deleted, server_copy].map(|path| path.to_string_lossy().into_owned());
        mail.write(move |conn| {
            for (id, protocol) in [(1, "pop3"), (2, "imap")] {
                conn.execute(
                    "INSERT INTO accounts (
                       id, email, provider, auth_kind, username, incoming_host, incoming_port,
                       smtp_host, smtp_port, created_at, mail_protocol, settings_json
                     ) VALUES (?1, ?1 || '@example.test', 'generic', 'password', 'me', 'h', 995,
                               'h', 465, 0, ?2, '{\"connection\":{\"incomingSecurity\":\"tls\",\"smtpSecurity\":\"tls\"}}')",
                    rusqlite::params![id, protocol],
                )?;
                conn.execute(
                    "INSERT INTO folders (id, account_id, path, role) VALUES (?1, ?1, 'Inbox', 'inbox')",
                    [id],
                )?;
            }
            for (account, path) in [(1, &paths[0]), (1, &paths[1]), (2, &paths[2])] {
                conn.execute(
                    "INSERT INTO messages (account_id, folder_id, date, raw_path)
                     VALUES (?1, ?1, 0, ?2)",
                    rusqlite::params![account, path],
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();

        let destination = output.path().join("snapshot");
        let manifest = create(&mail, &calendar, &files, None, &destination)
            .await
            .unwrap();
        assert!(manifest.device_mail_payloads);
        let copied = destination.join(DEVICE_MAIL_DIR).join("1");
        assert_eq!(
            std::fs::read(copied.join("kept.pop3.eml")).unwrap(),
            std::fs::read(&kept).unwrap()
        );
        assert_eq!(std::fs::read_dir(&copied).unwrap().count(), 1);
        assert!(!destination.join(DEVICE_MAIL_DIR).join("2").exists());
    }
}
