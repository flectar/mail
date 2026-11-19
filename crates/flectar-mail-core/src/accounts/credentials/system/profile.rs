//! Which keyring service holds a profile's secrets.
//!
//! Secrets are named after account numbers, and every profile numbers its
//! accounts from 1, so profiles that share a keyring need services of their
//! own. The Flatpak and a native package in one Linux session are two such
//! profiles. Every profile used to name its secrets under [`SHARED_SERVICE`],
//! so they read, replaced and removed each other's.
//!
//! A profile records its service in its data folder. A new profile gets a
//! service of its own before it stores anything. A profile from before keeps
//! the shared service, where its secrets are, unless another profile from
//! before used it too. Neither can tell its own secrets from the other's
//! then, so both move to services of their own and their accounts sign in
//! again.

use super::SHARED_SERVICE;
use crate::config::Paths;
use crate::error::{CoreError, Result};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

/// A real record is about a hundred bytes.
const MAX_RECORD_BYTES: u64 = 4 * 1024;

/// What a profile's record file says.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    /// The profile's random id.
    profile: String,
    /// The service its secrets are under. A profile from before has none
    /// until it has found out whether it keeps the shared one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    service: Option<String>,
}

impl Record {
    /// A new profile, with a service of its own.
    fn own() -> Self {
        let profile = random_id();
        Self {
            service: Some(own_service(&profile)),
            profile,
        }
    }

    /// A profile from before, still to settle its service.
    fn unsettled() -> Self {
        Self {
            profile: random_id(),
            service: None,
        }
    }

    fn valid(&self) -> bool {
        self.profile.len() == 32
            && self
                .profile
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
            && self.service.as_deref().is_none_or(|service| {
                service == SHARED_SERVICE || service == own_service(&self.profile)
            })
    }
}

fn random_id() -> String {
    format!("{:032x}", rand::random::<u128>())
}

fn own_service(profile: &str) -> String {
    format!("{SHARED_SERVICE}-{profile}")
}

/// The keyring service of one profile, settled on first use.
#[derive(Debug)]
pub(super) struct ProfileService {
    record: PathBuf,
    /// The profile had no mail database when its store was made.
    new: bool,
    settled: Mutex<Option<String>>,
}

impl ProfileService {
    pub(super) fn of(paths: &Paths) -> Self {
        let record = paths.keyring_file();
        let new = !record.exists() && !paths.db_file().exists();
        // Recorded right away, so a new profile that quits before storing
        // anything doesn't look like one from before on its next start.
        if new && let Err(error) = publish(&record, &Record::own()) {
            tracing::warn!(%error, "credential: could not record the profile's keyring service");
        }
        Self {
            record,
            new,
            settled: Mutex::new(None),
        }
    }

    /// The service this profile's secrets are named under.
    pub(super) fn get(&self) -> Result<String> {
        let mut settled = self.settled.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(service) = settled.as_ref() {
            return Ok(service.clone());
        }
        let service = self.settle()?;
        *settled = Some(service.clone());
        Ok(service)
    }

    fn settle(&self) -> Result<String> {
        let mut record = match read(&self.record)? {
            Some(record) => record,
            None => {
                let first = if self.new {
                    Record::own()
                } else {
                    Record::unsettled()
                };
                publish(&self.record, &first)?;
                read(&self.record)?.ok_or_else(|| {
                    CoreError::Other(format!("{} disappeared", self.record.display()))
                })?
            }
        };
        let service = match record.service.as_deref() {
            Some(SHARED_SERVICE) if shared::kept(&record.profile)? => {
                return Ok(SHARED_SERVICE.to_owned());
            }
            Some(SHARED_SERVICE) => {
                tracing::warn!(
                    "credential: another profile used this profile's keyring service; each now \
                     has its own, so its accounts sign in again"
                );
                own_service(&record.profile)
            }
            Some(service) => return Ok(service.to_owned()),
            None if shared::claim(&record.profile)? => {
                tracing::info!("credential: this profile keeps the keyring service from before");
                SHARED_SERVICE.to_owned()
            }
            None => {
                tracing::warn!(
                    "credential: another profile used the keyring service from before; this one \
                     now has its own, so its accounts sign in again"
                );
                own_service(&record.profile)
            }
        };
        record.service = Some(service.clone());
        replace(&self.record, &record)?;
        Ok(service)
    }
}

fn read(file: &Path) -> Result<Option<Record>> {
    use std::io::Read;

    let handle = match std::fs::File::open(file) {
        Ok(handle) => handle,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut bytes = Vec::new();
    handle.take(MAX_RECORD_BYTES + 1).read_to_end(&mut bytes)?;
    serde_json::from_slice::<Record>(&bytes)
        .ok()
        .filter(|record| bytes.len() as u64 <= MAX_RECORD_BYTES && record.valid())
        .map(Some)
        .ok_or_else(|| CoreError::Other(format!("{} is damaged", file.display())))
}

/// Write `record` unless the profile has one already, which another start
/// may have written meanwhile.
fn publish(file: &Path, record: &Record) -> Result<()> {
    let failed = match temporary(file, record)?.persist_noclobber(file) {
        Ok(_) => return Ok(()),
        Err(failed) => failed,
    };
    if file.exists() {
        return Ok(());
    }
    // eCryptfs, NFS and ntfs-3g can't move without replacing; the name is
    // still free, so a plain move does the same.
    failed
        .file
        .persist(file)
        .map(drop)
        .map_err(|failed| failed.error.into())
}

fn replace(file: &Path, record: &Record) -> Result<()> {
    temporary(file, record)?
        .persist(file)
        .map(drop)
        .map_err(|failed| failed.error.into())
}

fn temporary(file: &Path, record: &Record) -> Result<tempfile::NamedTempFile> {
    let directory = file
        .parent()
        .filter(|directory| !directory.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(directory)?;
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    serde_json::to_writer(temporary.as_file_mut(), record)?;
    temporary.as_file().sync_all()?;
    Ok(temporary)
}

/// The Secret Service is one keyring for every profile in the session, and
/// an entry there names the profile that keeps the shared service.
#[cfg(secret_service)]
mod shared {
    use super::super::{SHARED_SERVICE, is_absent, secret_service, set_password};
    use crate::error::Result;

    /// What the entry holds once a second profile from before turned up.
    const CONTESTED: &str = "contested";

    fn entry() -> Result<keyring::Entry> {
        keyring::Entry::new(SHARED_SERVICE, "profile").map_err(Into::into)
    }

    /// Whether `profile`, one from before, keeps the shared service.
    pub(super) fn claim(profile: &str) -> Result<bool> {
        let entry = entry()?;
        match entry.get_password() {
            // An earlier claim of this profile that couldn't be recorded.
            Ok(owner) if owner == profile => Ok(true),
            Ok(owner) => contest(&entry, &owner),
            // No keyring yet, so no secrets from before either.
            Err(error) if secret_service::missing_default_collection(&error) => Ok(false),
            Err(error) if is_absent(&error) => {
                set_password(&entry, profile)?;
                // Another profile may have claimed it at the same moment.
                match entry.get_password() {
                    Ok(owner) if owner == profile => Ok(true),
                    Ok(owner) => contest(&entry, &owner),
                    Err(error) => Err(error.into()),
                }
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Whether `profile`, which claimed the shared service, still keeps it.
    /// Only reading, as on every start: a keyring that was emptied, or that
    /// hides its items while locked, has nobody else's claim.
    pub(super) fn kept(profile: &str) -> Result<bool> {
        let entry = entry()?;
        match entry.get_password() {
            Ok(owner) if owner == profile => Ok(true),
            Ok(owner) => contest(&entry, &owner),
            Err(error) if is_absent(&error) => Ok(true),
            Err(error) => Err(error.into()),
        }
    }

    /// Another profile from before used the shared service as well, so
    /// neither keeps it.
    fn contest(entry: &keyring::Entry, owner: &str) -> Result<bool> {
        if owner != CONTESTED {
            set_password(entry, CONTESTED)?;
        }
        Ok(false)
    }
}

/// On Windows and macOS every build used the same folder per user, so a
/// profile from before is the only one. A later one, such as a Store
/// package's own, is new and has a service of its own.
#[cfg(not(secret_service))]
mod shared {
    use crate::error::Result;

    pub(super) fn claim(_profile: &str) -> Result<bool> {
        Ok(true)
    }

    pub(super) fn kept(_profile: &str) -> Result<bool> {
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(directory: &Path, database: bool) -> Paths {
        let paths = Paths::for_tests(directory);
        if database {
            std::fs::write(paths.db_file(), b"").unwrap();
        }
        paths
    }

    #[test]
    fn a_new_profile_gets_a_service_of_its_own_right_away() {
        let directory = tempfile::tempdir().unwrap();
        let paths = profile(directory.path(), false);
        let service = ProfileService::of(&paths);
        let recorded = read(&paths.keyring_file()).unwrap().unwrap();
        assert_eq!(
            recorded.service.as_deref(),
            Some(own_service(&recorded.profile).as_str())
        );
        assert_eq!(service.get().unwrap(), own_service(&recorded.profile));

        // The next start reads the same record.
        std::fs::write(paths.db_file(), b"").unwrap();
        assert_eq!(
            ProfileService::of(&paths).get().unwrap(),
            own_service(&recorded.profile)
        );
    }

    #[test]
    fn a_profile_from_before_is_settled_on_first_use() {
        let directory = tempfile::tempdir().unwrap();
        let paths = profile(directory.path(), true);
        ProfileService::of(&paths);
        assert!(read(&paths.keyring_file()).unwrap().is_none());
    }

    #[test]
    fn a_recorded_service_is_kept() {
        let directory = tempfile::tempdir().unwrap();
        let paths = profile(directory.path(), true);
        let recorded = Record::own();
        publish(&paths.keyring_file(), &recorded).unwrap();
        assert_eq!(
            ProfileService::of(&paths).get().unwrap(),
            recorded.service.unwrap()
        );
    }

    #[test]
    fn publishing_keeps_a_record_written_first() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("keyring.json");
        let first = Record::own();
        publish(&file, &first).unwrap();
        publish(&file, &Record::own()).unwrap();
        assert_eq!(read(&file).unwrap(), Some(first));
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn records_are_strict() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("keyring.json");
        let id = "0123456789abcdef0123456789abcdef";
        for valid in [
            format!(r#"{{"profile":"{id}"}}"#),
            format!(r#"{{"profile":"{id}","service":"flectar-mail"}}"#),
            format!(r#"{{"profile":"{id}","service":"flectar-mail-{id}"}}"#),
        ] {
            std::fs::write(&file, valid).unwrap();
            assert!(read(&file).unwrap().is_some());
        }
        for damaged in [
            String::new(),
            "not json".to_owned(),
            format!(r#"{{"profile":"{}"}}"#, id.to_uppercase()),
            format!(r#"{{"profile":"{id}0"}}"#),
            // Another profile's service.
            format!(
                r#"{{"profile":"{id}","service":"flectar-mail-{}"}}"#,
                "f".repeat(32)
            ),
            format!(r#"{{"profile":"{id}","service":"other"}}"#),
            format!(r#"{{"profile":"{id}","extra":true}}"#),
            format!(r#"{}{{"profile":"{id}"}}"#, " ".repeat(5000)),
        ] {
            std::fs::write(&file, damaged).unwrap();
            assert!(read(&file).is_err());
        }
    }
}
