//! Bounded reads for app-managed and user-selected files.

use crate::error::{CoreError, Result};
use std::path::Path;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub(crate) async fn read(
    path: impl AsRef<Path>,
    max_bytes: usize,
    context: &'static str,
) -> Result<Vec<u8>> {
    let mut file = tokio::fs::File::open(path.as_ref())
        .await
        .map_err(|error| CoreError::Other(format!("{context}: {error}")))?;
    let size = file
        .metadata()
        .await
        .map_err(|error| CoreError::Other(format!("{context}: {error}")))?
        .len();
    if size > max_bytes as u64 {
        return Err(too_large(context, max_bytes));
    }

    let mut bytes = Vec::with_capacity(usize::try_from(size).unwrap_or(max_bytes).min(max_bytes));
    let mut chunk = [0_u8; 16 * 1024];
    loop {
        let read = file
            .read(&mut chunk)
            .await
            .map_err(|error| CoreError::Other(format!("{context}: {error}")))?;
        if read == 0 {
            return Ok(bytes);
        }
        append_bounded(&mut bytes, &chunk[..read], max_bytes, context)?;
    }
}

pub(crate) async fn copy(
    source: impl AsRef<Path>,
    destination: impl AsRef<Path>,
    max_bytes: usize,
    context: &'static str,
) -> Result<usize> {
    let mut source = tokio::fs::File::open(source.as_ref())
        .await
        .map_err(|error| CoreError::Other(format!("{context}: {error}")))?;
    let size = source
        .metadata()
        .await
        .map_err(|error| CoreError::Other(format!("{context}: {error}")))?
        .len();
    if size > max_bytes as u64 {
        return Err(too_large(context, max_bytes));
    }
    let destination_path = destination.as_ref();
    let mut destination = tokio::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(destination_path)
        .await
        .map_err(|error| CoreError::Other(format!("{context}: {error}")))?;

    let result = async {
        let mut written = 0usize;
        let mut chunk = [0_u8; 16 * 1024];
        loop {
            let read = source
                .read(&mut chunk)
                .await
                .map_err(|error| CoreError::Other(format!("{context}: {error}")))?;
            if read == 0 {
                destination
                    .flush()
                    .await
                    .map_err(|error| CoreError::Other(format!("{context}: {error}")))?;
                return Ok(written);
            }
            if read > max_bytes.saturating_sub(written) {
                return Err(too_large(context, max_bytes));
            }
            destination
                .write_all(&chunk[..read])
                .await
                .map_err(|error| CoreError::Other(format!("{context}: {error}")))?;
            written += read;
        }
    }
    .await;
    if result.is_err() {
        drop(destination);
        let _ = tokio::fs::remove_file(destination_path).await;
    }
    result
}

/// Write a complete app-managed file beside its destination and publish it
/// with one rename. A crash or short write can leave at most an unreferenced
/// temporary file, never a truncated path already referenced by SQLite.
pub(crate) async fn write_atomic(
    path: impl AsRef<Path>,
    bytes: &[u8],
    context: &'static str,
) -> Result<()> {
    write_replacing(path.as_ref(), bytes, context, false).await
}

/// [`write_atomic`] for data that exists nowhere else: the file and its
/// directory entry are on stable storage when this returns, so a crash or
/// power loss right after can't lose it.
pub(crate) async fn write_durable(
    path: impl AsRef<Path>,
    bytes: &[u8],
    context: &'static str,
) -> Result<()> {
    write_replacing(path.as_ref(), bytes, context, true).await
}

/// Temporary files of interrupted [`write_atomic`] and [`write_durable`]
/// calls, which nothing references.
fn is_partial_write(name: &str) -> bool {
    name.starts_with('.') && name.ends_with(".partial")
}

/// Remove what interrupted writes left in `dir`, returning how many files.
/// Call it only while nothing writes there.
pub(crate) async fn remove_partial_writes(dir: impl AsRef<Path>) -> Result<usize> {
    let mut entries = match tokio::fs::read_dir(dir.as_ref()).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error.into()),
    };
    let mut removed = 0;
    while let Some(entry) = entries.next_entry().await? {
        if entry.file_name().to_str().is_some_and(is_partial_write)
            && entry.file_type().await?.is_file()
        {
            tokio::fs::remove_file(entry.path()).await?;
            removed += 1;
        }
    }
    Ok(removed)
}

async fn write_replacing(
    path: &Path,
    bytes: &[u8],
    context: &'static str,
    durable: bool,
) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| CoreError::Other(format!("{context}: destination has no parent")))?;
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|error| CoreError::Other(format!("{context}: {error}")))?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("file");
    let temporary = parent.join(format!(".{name}.{:016x}.partial", rand::random::<u64>()));
    let mut file = tokio::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .await
        .map_err(|error| CoreError::Other(format!("{context}: {error}")))?;
    let result = async {
        file.write_all(bytes)
            .await
            .map_err(|error| CoreError::Other(format!("{context}: {error}")))?;
        file.flush()
            .await
            .map_err(|error| CoreError::Other(format!("{context}: {error}")))?;
        if durable {
            file.sync_all()
                .await
                .map_err(|error| CoreError::Other(format!("{context}: {error}")))?;
        }
        drop(file);
        #[cfg(windows)]
        if tokio::fs::symlink_metadata(path).await.is_ok() {
            tokio::fs::remove_file(path)
                .await
                .map_err(|error| CoreError::Other(format!("{context}: {error}")))?;
        }
        tokio::fs::rename(&temporary, path)
            .await
            .map_err(|error| CoreError::Other(format!("{context}: {error}")))?;
        // The rename is only durable once the directory is. Windows can't
        // open a directory for this, and NTFS journals the rename itself.
        #[cfg(unix)]
        if durable {
            let directory = tokio::fs::File::open(parent)
                .await
                .map_err(|error| CoreError::Other(format!("{context}: {error}")))?;
            directory
                .sync_all()
                .await
                .map_err(|error| CoreError::Other(format!("{context}: {error}")))?;
        }
        Ok(())
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&temporary).await;
    }
    result
}

/// A hidden working directory beside `destination`, for building a
/// directory that [`publish_directory`] then puts in place whole.
pub(crate) fn staging_directory(destination: &Path) -> Result<std::path::PathBuf> {
    let parent = destination
        .parent()
        .ok_or_else(|| CoreError::Other("the destination has no parent directory".into()))?;
    let name = destination
        .file_name()
        .ok_or_else(|| CoreError::Other("the destination has no directory name".into()))?;
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

/// Move a finished staging directory to `destination`, which must not exist,
/// so an export never replaces an earlier one.
pub(crate) fn publish_directory(staging: &Path, destination: &Path) -> Result<()> {
    #[cfg(rename_noreplace)]
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
    #[cfg(not(rename_noreplace))]
    {
        // Windows directory rename fails if the destination already exists.
        std::fs::rename(staging, destination)?;
    }
    Ok(())
}

/// Limit a directory and the listed files in it to their owner.
#[cfg(unix)]
pub(crate) fn restrict_permissions(directory: &Path, files: &[&Path]) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
    for file in files {
        std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn restrict_permissions(_directory: &Path, _files: &[&Path]) -> Result<()> {
    Ok(())
}

/// Read only the RFC-style header prefix of a potentially large cached file.
pub(crate) async fn read_headers(
    path: impl AsRef<Path>,
    max_bytes: usize,
    context: &'static str,
) -> Result<Vec<u8>> {
    let mut file = tokio::fs::File::open(path.as_ref())
        .await
        .map_err(|error| CoreError::Other(format!("{context}: {error}")))?;
    let mut bytes = Vec::with_capacity((16 * 1024).min(max_bytes));
    let mut chunk = [0_u8; 16 * 1024];
    while bytes.len() < max_bytes {
        let remaining = max_bytes - bytes.len();
        let read_capacity = remaining.min(chunk.len());
        let read = file
            .read(&mut chunk[..read_capacity])
            .await
            .map_err(|error| CoreError::Other(format!("{context}: {error}")))?;
        if read == 0 {
            break;
        }
        append_bounded(&mut bytes, &chunk[..read], max_bytes, context)?;
        if let Some(end) = header_end(&bytes) {
            bytes.truncate(end);
            break;
        }
    }
    Ok(bytes)
}

// A file may grow after metadata is sampled. Keep buffer growth within the
// caller's ceiling instead of Vec's implicit capacity doubling.
fn append_bounded(
    bytes: &mut Vec<u8>,
    chunk: &[u8],
    max_bytes: usize,
    context: &'static str,
) -> Result<()> {
    if chunk.len() > max_bytes.saturating_sub(bytes.len()) {
        return Err(too_large(context, max_bytes));
    }
    let required = bytes.len() + chunk.len();
    if required > bytes.capacity() {
        let target = required
            .max(bytes.capacity().saturating_mul(2))
            .min(max_bytes);
        bytes.reserve_exact(target - bytes.len());
    }
    bytes.extend_from_slice(chunk);
    Ok(())
}

fn header_end(bytes: &[u8]) -> Option<usize> {
    bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|position| position + 4)
        .or_else(|| {
            bytes
                .windows(2)
                .position(|window| window == b"\n\n")
                .map(|position| position + 2)
        })
}

fn too_large(context: &'static str, max_bytes: usize) -> CoreError {
    CoreError::Other(format!(
        "{context} exceeded the {max_bytes}-byte file limit"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn growing_file_buffers_stay_within_the_ceiling() {
        let mut bytes = Vec::with_capacity(12);
        append_bounded(&mut bytes, &[1; 12], 20, "growing file").unwrap();
        append_bounded(&mut bytes, &[2; 6], 20, "growing file").unwrap();
        assert_eq!(bytes.len(), 18);
        assert_eq!(bytes.capacity(), 20);
        assert!(append_bounded(&mut bytes, &[3; 3], 20, "growing file").is_err());
        assert_eq!(bytes.len(), 18);
        assert!(bytes.capacity() <= 20);
    }

    #[tokio::test]
    async fn file_read_stops_at_the_hard_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bounded.bin");
        tokio::fs::write(&path, [1, 2, 3, 4]).await.unwrap();
        assert_eq!(read(&path, 4, "test file").await.unwrap(), [1, 2, 3, 4]);
        assert!(read(&path, 3, "test file").await.is_err());
    }

    #[tokio::test]
    async fn file_copy_removes_an_over_limit_partial() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.bin");
        let destination = dir.path().join("destination.bin");
        tokio::fs::write(&source, [1, 2, 3, 4]).await.unwrap();
        assert!(copy(&source, &destination, 3, "test copy").await.is_err());
        assert!(!destination.exists());

        assert_eq!(
            copy(&source, &destination, 4, "test copy").await.unwrap(),
            4
        );
        assert_eq!(tokio::fs::read(destination).await.unwrap(), [1, 2, 3, 4]);
    }

    #[tokio::test]
    async fn durable_writes_replace_whole_files_and_leave_no_partials() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mail").join("message.eml");
        write_durable(&path, b"first", "test write").await.unwrap();
        write_durable(&path, b"second", "test write").await.unwrap();
        assert_eq!(tokio::fs::read(&path).await.unwrap(), b"second");
        let mut names = Vec::new();
        let mut entries = tokio::fs::read_dir(path.parent().unwrap()).await.unwrap();
        while let Some(entry) = entries.next_entry().await.unwrap() {
            names.push(entry.file_name().into_string().unwrap());
        }
        assert_eq!(names, ["message.eml"]);
    }

    #[tokio::test]
    async fn interrupted_writes_are_swept_without_touching_real_files() {
        let dir = tempfile::tempdir().unwrap();
        let kept = ["message.eml", ".hidden", "notes.partial"];
        for name in kept {
            tokio::fs::write(dir.path().join(name), b"x").await.unwrap();
        }
        tokio::fs::write(dir.path().join(".message.eml.00ab.partial"), b"x")
            .await
            .unwrap();
        tokio::fs::create_dir(dir.path().join(".folder.partial"))
            .await
            .unwrap();
        assert_eq!(remove_partial_writes(dir.path()).await.unwrap(), 1);
        for name in kept.iter().chain([&".folder.partial"]) {
            assert!(dir.path().join(name).exists(), "{name}");
        }
        assert!(!dir.path().join(".message.eml.00ab.partial").exists());
        assert_eq!(
            remove_partial_writes(dir.path().join("missing"))
                .await
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn small_header_limits_also_bound_initial_capacity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("message.eml");
        tokio::fs::write(&path, b"X: a\n\nbody").await.unwrap();
        for limit in [0, 1, 7, 1024, 16 * 1024] {
            let bytes = read_headers(&path, limit, "small header").await.unwrap();
            assert!(bytes.len() <= limit);
            assert!(bytes.capacity() <= limit);
        }
    }

    #[tokio::test]
    async fn header_read_does_not_load_the_body() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("message.eml");
        tokio::fs::write(&path, b"Subject: hello\r\nX-Test: yes\r\n\r\nlarge body")
            .await
            .unwrap();
        assert_eq!(
            read_headers(&path, 1024, "test headers").await.unwrap(),
            b"Subject: hello\r\nX-Test: yes\r\n\r\n"
        );
    }

    #[tokio::test]
    async fn atomic_write_replaces_only_with_complete_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.bin");
        write_atomic(&path, b"first", "test atomic write")
            .await
            .unwrap();
        write_atomic(&path, b"second", "test atomic write")
            .await
            .unwrap();
        assert_eq!(tokio::fs::read(&path).await.unwrap(), b"second");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
