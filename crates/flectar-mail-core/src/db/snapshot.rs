//! Coherent online snapshots of the physically separate mail, calendar and files
//! stores. All three writer queues are held at the same coordinated gate while
//! SQLite's online-backup API copies committed pages, so no application write
//! can land between the captured database states.

use super::Db;
use crate::error::{CoreError, Result};
use rusqlite::{Connection, MAIN_DB, OpenFlags};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::mpsc;

const SNAPSHOT_FORMAT: &str = "flectar-mail-database-snapshot";
const SNAPSHOT_VERSION: u32 = 2;
const MAIL_FILE: &str = "mail.sqlite3";
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

fn staging_path(destination: &Path) -> Result<PathBuf> {
    let parent = destination.parent().ok_or_else(|| {
        CoreError::Other("database snapshot destination has no parent directory".into())
    })?;
    let name = destination.file_name().ok_or_else(|| {
        CoreError::Other("database snapshot destination has no directory name".into())
    })?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| CoreError::Other(format!("system clock before Unix epoch: {error}")))?
        .as_nanos();
    Ok(parent.join(format!(
        ".{}.partial-{}-{nonce}",
        name.to_string_lossy(),
        std::process::id()
    )))
}

#[cfg(unix)]
fn restrict_permissions(directory: &Path, files: &[&Path]) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
    for file in files {
        std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn restrict_permissions(_directory: &Path, _files: &[&Path]) -> Result<()> {
    Ok(())
}

fn publish_snapshot(staging: &Path, destination: &Path) -> Result<()> {
    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios"
    ))]
    {
        rustix::fs::renameat_with(
            rustix::fs::CWD,
            staging,
            rustix::fs::CWD,
            destination,
            rustix::fs::RenameFlags::NOREPLACE,
        )
        .map_err(std::io::Error::from)?;
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios"
    )))]
    {
        // Windows directory rename fails if the destination already exists.
        std::fs::rename(staging, destination)?;
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
    let staging = staging_path(destination)?;
    tokio::fs::create_dir(&staging).await?;
    // Restrict access before SQLite creates any files. Applying permissions
    // only after the backup would briefly expose private mail under a common
    // 022 umask.
    restrict_permissions(&staging, &[])?;

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
                restrict_permissions(&target, &[])?;
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
        restrict_permissions(
            &staging,
            &[&mail_path, &calendar_path, &files_path, &manifest_path],
        )?;
        publish_snapshot(&staging, destination)?;
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
                   id, email, provider, auth_kind, username, imap_host, imap_port,
                   smtp_host, smtp_port, created_at
                 ) VALUES (1, 'snapshot@example.test', 'imap', 'password', 'snapshot',
                           'imap.example.test', 993, 'smtp.example.test', 465, 0)",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
        calendar
            .write(|conn| {
                conn.execute(
                    "INSERT INTO calendars (id, account_id, url, display_name)
                     VALUES (1, 1, 'local://snapshot', 'Snapshot')",
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
}
