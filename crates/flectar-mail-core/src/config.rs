use std::path::PathBuf;

use crate::error::Result;

#[derive(Debug, Clone)]
pub struct Paths {
    /// Durable application state. Databases, raw mail, drafts, queues, and
    /// settings live here and must survive process death and cache eviction.
    pub data_dir: PathBuf,
    /// Reconstructable data such as downloaded attachment bodies and preview
    /// scratch files. Hosts may place this in an OS-managed cache directory.
    pub cache_dir: PathBuf,
}

impl Paths {
    /// Resolve desktop directories. Mobile hosts must inject their sandbox
    /// paths explicitly; silently using the process working directory is never
    /// safe on Android or iOS.
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    pub fn default_dirs() -> Result<Self> {
        let data_root = dirs::data_dir().ok_or_else(|| {
            crate::error::CoreError::Other(
                "the operating system has no application data directory".into(),
            )
        })?;
        let cache_root = dirs::cache_dir().ok_or_else(|| {
            crate::error::CoreError::Other(
                "the operating system has no application cache directory".into(),
            )
        })?;
        Ok(Self::new(
            data_root.join("flectar-mail"),
            cache_root.join("flectar-mail"),
        ))
    }

    #[cfg(target_os = "ios")]
    pub fn default_dirs() -> Result<Self> {
        let root = std::env::var_os("HOME").map(PathBuf::from).ok_or_else(|| {
            crate::error::CoreError::Other("iOS did not supply the application sandbox.".into())
        })?;
        Ok(Self::new(
            root.join("Library/Application Support/Flectar"),
            root.join("Library/Caches/Flectar"),
        ))
    }
    #[cfg(target_os = "android")]
    pub fn default_dirs() -> Result<Self> {
        Err(crate::error::CoreError::Other(
            "mobile application storage must be supplied by the platform host".into(),
        ))
    }

    pub fn new(data_dir: PathBuf, cache_dir: PathBuf) -> Self {
        Self {
            data_dir,
            cache_dir,
        }
    }

    pub fn for_tests(root: &std::path::Path) -> Self {
        Self::new(root.to_path_buf(), root.join("cache"))
    }

    pub fn db_file(&self) -> PathBuf {
        self.data_dir.join("flectar-mail.db")
    }

    /// Calendar data lives in its own SQLite database so large mailbox writes,
    /// FTS maintenance, and mail backfills never contend with calendar reads or
    /// sync bookkeeping.
    pub fn calendar_db_file(&self) -> PathBuf {
        self.data_dir.join("flectar-calendar.db")
    }

    pub fn files_db_file(&self) -> PathBuf {
        self.data_dir.join("flectar-files.db")
    }
    pub fn files_cache_dir(&self, account_id: i64) -> PathBuf {
        self.cache_dir.join("files").join(account_id.to_string())
    }
    pub fn files_staging_dir(&self, account_id: i64) -> PathBuf {
        self.data_dir
            .join("file_transfers")
            .join(account_id.to_string())
    }

    /// Small, reconstructable projection of the last visible mailbox page.
    /// The native shell may paint this before the databases finish opening;
    /// SQLite remains authoritative and replaces it during normal startup.
    pub fn warm_start_file(&self) -> PathBuf {
        self.cache_dir.join("warm-start-mailbox-v2.json")
    }

    /// Directory for raw .eml files of one account.
    pub fn mail_dir(&self, account_id: i64) -> PathBuf {
        self.data_dir.join("mail").join(account_id.to_string())
    }

    /// Directory for extracted attachment files.
    pub fn attachments_dir(&self, account_id: i64) -> PathBuf {
        self.cache_dir
            .join("attachments")
            .join(account_id.to_string())
    }

    /// Root for app-managed copies of outgoing (draft) attachments. Files
    /// picked in the composer are staged here so dispatch only ever reads
    /// files the app itself wrote, never an arbitrary caller-supplied path.
    pub fn draft_attachments_dir(&self) -> PathBuf {
        self.data_dir.join("draft_attachments")
    }

    /// Directory holding local embedding-model weights (`<models_dir>/<key>`).
    pub fn models_dir(&self) -> PathBuf {
        self.cache_dir.join("models")
    }

    pub fn temp_dir(&self) -> PathBuf {
        self.cache_dir.join("tmp")
    }

    pub fn ensure(&self) -> std::io::Result<()> {
        for directory in [
            &self.data_dir,
            &self.cache_dir,
            &self.mail_root(),
            &self.draft_attachments_dir(),
            &self.temp_dir(),
        ] {
            std::fs::create_dir_all(directory)?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.data_dir, std::fs::Permissions::from_mode(0o700))?;
            std::fs::set_permissions(&self.cache_dir, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }

    fn mail_root(&self) -> PathBuf {
        self.data_dir.join("mail")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_paths_separate_durable_and_reconstructable_state() {
        let root = std::path::Path::new("/private/app");
        let paths = Paths::new(root.join("data"), root.join("cache"));

        assert_eq!(paths.db_file(), root.join("data/flectar-mail.db"));
        assert_eq!(
            paths.calendar_db_file(),
            root.join("data/flectar-calendar.db")
        );
        assert_eq!(
            paths.warm_start_file(),
            root.join("cache/warm-start-mailbox-v2.json")
        );
        assert_eq!(paths.mail_dir(7), root.join("data/mail/7"));
        assert_eq!(
            paths.draft_attachments_dir(),
            root.join("data/draft_attachments")
        );
        assert_eq!(paths.attachments_dir(7), root.join("cache/attachments/7"));
        assert_eq!(paths.temp_dir(), root.join("cache/tmp"));
    }
}
