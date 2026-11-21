//! Credentials kept in one file, sealed with a key that the desktop holds.
//!
//! The Flatpak asks the Secret portal for its master secret, which the
//! desktop's keyring (GNOME Keyring or KWallet) keeps for this app alone, and
//! seals the profile's credentials with a key derived from it. This is what
//! libsecret does inside a sandbox, and it needs no access to the user's whole
//! keyring. Without that secret the file reveals nothing, and a file the key
//! cannot open is left untouched rather than replaced, so a keyring that
//! returns a different secret never costs the stored credentials.
//!
//! The file holds a format tag, a random salt and nonce, and the
//! AES-256-GCM-sealed JSON map of `"{account}:{slot}"` to secret. Every write
//! picks a new salt and nonce and replaces the file atomically.

use super::{CredentialStore, Slot};
use crate::error::{CoreError, Result};
use ring::aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use ring::hkdf::{HKDF_SHA256, Salt};
use ring::rand::{SecureRandom, SystemRandom};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Identifies the format, and is authenticated with the sealed data.
const TAG: &[u8; 8] = b"FMSEAL01";
const SALT_LEN: usize = 32;
const HEADER_LEN: usize = TAG.len() + SALT_LEN + NONCE_LEN;
const KEY_INFO: &[u8] = b"flectar-mail sealed credentials v1";
/// The portal's secret is at least this long; a shorter one is not a key.
const MIN_MASTER_LEN: usize = 16;
/// Far more than any profile's credentials; refuses to read a runaway file.
const MAX_FILE_BYTES: u64 = 1024 * 1024;

pub struct SealedFileCredentialStore {
    path: PathBuf,
    master: Vec<u8>,
    lock: Mutex<()>,
}

impl SealedFileCredentialStore {
    /// `master` is the secret the key is derived from. Its bytes are copied,
    /// so the caller can wipe its own copy.
    pub fn new(path: impl Into<PathBuf>, master: &[u8]) -> Result<Self> {
        if master.len() < MIN_MASTER_LEN {
            return Err(CoreError::CredentialStoreUnavailable(
                "the desktop returned too short a secret for sealing credentials".into(),
            ));
        }
        Ok(Self {
            path: path.into(),
            master: master.to_vec(),
            lock: Mutex::new(()),
        })
    }

    fn key(&self, salt: &[u8]) -> Result<LessSafeKey> {
        let mut bytes = [0u8; 32];
        Salt::new(HKDF_SHA256, salt)
            .extract(&self.master)
            .expand(&[KEY_INFO], &AES_256_GCM)
            .and_then(|okm| okm.fill(&mut bytes))
            .map_err(|_| sealing_failed())?;
        let key = UnboundKey::new(&AES_256_GCM, &bytes).map_err(|_| sealing_failed());
        wipe(&mut bytes);
        Ok(LessSafeKey::new(key?))
    }

    fn read(&self) -> Result<HashMap<String, String>> {
        let file = match std::fs::File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
            Err(error) => return Err(error.into()),
        };
        let mut sealed = Vec::new();
        file.take(MAX_FILE_BYTES + 1).read_to_end(&mut sealed)?;
        if sealed.len() as u64 > MAX_FILE_BYTES
            || sealed.len() < HEADER_LEN
            || &sealed[..TAG.len()] != TAG
        {
            return Err(unreadable(&self.path));
        }
        let (header, body) = sealed.split_at_mut(HEADER_LEN);
        let salt = &header[TAG.len()..TAG.len() + SALT_LEN];
        let nonce = Nonce::try_assume_unique_for_key(&header[TAG.len() + SALT_LEN..])
            .map_err(|_| unreadable(&self.path))?;
        let plain = self
            .key(salt)?
            .open_in_place(nonce, Aad::from(TAG), body)
            .map_err(|_| unreadable(&self.path))?;
        let map = serde_json::from_slice(plain).map_err(|_| unreadable(&self.path));
        wipe(plain);
        map
    }

    fn write(&self, map: &HashMap<String, String>) -> Result<()> {
        let random = SystemRandom::new();
        let mut header = [0u8; HEADER_LEN];
        header[..TAG.len()].copy_from_slice(TAG);
        random
            .fill(&mut header[TAG.len()..])
            .map_err(|_| sealing_failed())?;
        let salt = &header[TAG.len()..TAG.len() + SALT_LEN];
        let nonce = Nonce::try_assume_unique_for_key(&header[TAG.len() + SALT_LEN..])
            .map_err(|_| sealing_failed())?;
        let mut body = serde_json::to_vec(map)?;
        let sealed = self
            .key(salt)?
            .seal_in_place_append_tag(nonce, Aad::from(TAG), &mut body);
        if sealed.is_err() {
            wipe(&mut body);
            return Err(sealing_failed());
        }
        if (HEADER_LEN + body.len()) as u64 > MAX_FILE_BYTES {
            return Err(CoreError::Other(
                "sealed credential file exceeds its 1 MiB limit".into(),
            ));
        }
        replace(&self.path, &header, &body)
    }

    /// Reads, changes and writes the map while holding the lock. A file that
    /// can't be opened stops the change, so it is never overwritten.
    fn change(&self, apply: impl FnOnce(&mut HashMap<String, String>) -> bool) -> Result<()> {
        let _guard = self
            .lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut map = self.read()?;
        let changed = apply(&mut map);
        let result = if changed { self.write(&map) } else { Ok(()) };
        wipe_map(map);
        result
    }
}

impl Drop for SealedFileCredentialStore {
    fn drop(&mut self) {
        wipe(&mut self.master);
    }
}

impl CredentialStore for SealedFileCredentialStore {
    fn store(&self, account_id: i64, slot: Slot, secret: &str) -> Result<()> {
        self.change(|map| {
            if let Some(mut old) = map.insert(slot_key(account_id, &slot), secret.to_string()) {
                wipe_string(&mut old);
            }
            true
        })
    }

    fn load(&self, account_id: i64, slot: Slot) -> Result<String> {
        let _guard = self
            .lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut map = self.read()?;
        let secret = map.remove(&slot_key(account_id, &slot));
        wipe_map(map);
        secret.ok_or_else(|| CoreError::Auth("no stored credential".into()))
    }

    fn delete(&self, account_id: i64, slot: Slot) -> Result<()> {
        self.change(|map| match map.remove(&slot_key(account_id, &slot)) {
            Some(mut old) => {
                wipe_string(&mut old);
                true
            }
            None => false,
        })
    }

    fn delete_all(&self, account_id: i64) -> Result<()> {
        let prefix = format!("{account_id}:");
        self.change(|map| {
            let before = map.len();
            map.retain(|key, _| !key.starts_with(&prefix));
            map.len() != before
        })
    }
}

fn slot_key(account_id: i64, slot: &Slot) -> String {
    format!("{account_id}:{}", slot.key())
}

fn replace(path: &Path, header: &[u8], body: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    temporary.write_all(header)?;
    temporary.write_all(body)?;
    temporary.as_file_mut().sync_all()?;
    temporary
        .persist(path)
        .map_err(|error| CoreError::Io(error.error))?;
    Ok(())
}

fn sealing_failed() -> CoreError {
    CoreError::CredentialStoreUnavailable("credentials could not be sealed".into())
}

fn unreadable(path: &Path) -> CoreError {
    CoreError::CredentialStoreUnavailable(format!(
        "{} can't be opened with the secret the desktop keyring holds for Flectar Mail",
        path.display()
    ))
}

/// Overwrites secret bytes before their memory is freed.
fn wipe(bytes: &mut [u8]) {
    bytes.fill(0);
    std::hint::black_box(bytes);
}

fn wipe_string(secret: &mut String) {
    let mut bytes = std::mem::take(secret).into_bytes();
    wipe(&mut bytes);
}

fn wipe_map(map: HashMap<String, String>) {
    for (_, mut secret) in map {
        wipe_string(&mut secret);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MASTER: &[u8] = b"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn store(dir: &Path) -> SealedFileCredentialStore {
        SealedFileCredentialStore::new(dir.join("credentials.sealed"), MASTER).unwrap()
    }

    #[test]
    fn round_trips_and_deletes_one_account() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        assert!(matches!(
            store.load(1, Slot::Password),
            Err(CoreError::Auth(_))
        ));
        store.store(1, Slot::Password, "one").unwrap();
        store.store(1, Slot::RefreshToken, "token").unwrap();
        store.store(2, Slot::Password, "two").unwrap();
        store.store(1, Slot::Password, "replaced").unwrap();
        assert_eq!(store.load(1, Slot::Password).unwrap(), "replaced");
        store.delete(1, Slot::RefreshToken).unwrap();
        assert!(matches!(
            store.load(1, Slot::RefreshToken),
            Err(CoreError::Auth(_))
        ));
        store.delete_all(1).unwrap();
        assert!(matches!(
            store.load(1, Slot::Password),
            Err(CoreError::Auth(_))
        ));
        assert_eq!(store.load(2, Slot::Password).unwrap(), "two");
        store.check_available().unwrap();
        // A second store with the same secret, as after a restart, reads it.
        let reopened =
            SealedFileCredentialStore::new(dir.path().join("credentials.sealed"), MASTER).unwrap();
        assert_eq!(reopened.load(2, Slot::Password).unwrap(), "two");
    }

    #[test]
    fn the_file_never_holds_a_secret_in_the_clear() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        store.store(7, Slot::Password, "hunter2-plaintext").unwrap();
        let bytes = std::fs::read(dir.path().join("credentials.sealed")).unwrap();
        assert_eq!(&bytes[..TAG.len()], TAG);
        let text = String::from_utf8_lossy(&bytes);
        assert!(!text.contains("hunter2"));
        assert!(!text.contains("7:password"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join("credentials.sealed"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // Each write uses a new salt and nonce.
        store.store(7, Slot::Password, "hunter2-plaintext").unwrap();
        let again = std::fs::read(dir.path().join("credentials.sealed")).unwrap();
        assert_ne!(bytes[TAG.len()..HEADER_LEN], again[TAG.len()..HEADER_LEN]);
    }

    #[test]
    fn another_secret_cannot_read_or_replace_the_file() {
        let dir = tempfile::tempdir().unwrap();
        store(dir.path()).store(1, Slot::Password, "kept").unwrap();
        let before = std::fs::read(dir.path().join("credentials.sealed")).unwrap();
        let other = SealedFileCredentialStore::new(
            dir.path().join("credentials.sealed"),
            b"fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210",
        )
        .unwrap();
        assert!(matches!(
            other.load(1, Slot::Password),
            Err(CoreError::CredentialStoreUnavailable(_))
        ));
        assert!(other.store(2, Slot::Password, "new").is_err());
        assert!(other.delete_all(1).is_err());
        assert_eq!(
            std::fs::read(dir.path().join("credentials.sealed")).unwrap(),
            before
        );
        assert_eq!(store(dir.path()).load(1, Slot::Password).unwrap(), "kept");
    }

    #[test]
    fn tampered_or_foreign_files_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        store.store(1, Slot::Password, "secret").unwrap();
        let path = dir.path().join("credentials.sealed");
        let mut bytes = std::fs::read(&path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        std::fs::write(&path, &bytes).unwrap();
        assert!(matches!(
            store.load(1, Slot::Password),
            Err(CoreError::CredentialStoreUnavailable(_))
        ));
        for foreign in [&b"{}"[..], &b"FMSEAL01short"[..], &[0u8; 200][..]] {
            std::fs::write(&path, foreign).unwrap();
            assert!(matches!(
                store.load(1, Slot::Password),
                Err(CoreError::CredentialStoreUnavailable(_))
            ));
            assert!(store.store(1, Slot::Password, "x").is_err());
            assert_eq!(std::fs::read(&path).unwrap(), foreign);
        }
    }

    #[test]
    fn removing_what_is_absent_leaves_the_file_alone() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        store.store(1, Slot::Password, "secret").unwrap();
        let path = dir.path().join("credentials.sealed");
        let before = std::fs::read(&path).unwrap();
        store.delete(1, Slot::RefreshToken).unwrap();
        store.delete_all(2).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), before);
        // Nothing stored yet, so nothing is written either.
        let empty =
            SealedFileCredentialStore::new(dir.path().join("empty.sealed"), MASTER).unwrap();
        empty.delete_all(1).unwrap();
        assert!(!dir.path().join("empty.sealed").exists());
    }

    #[test]
    fn a_short_secret_is_not_a_key() {
        let dir = tempfile::tempdir().unwrap();
        assert!(SealedFileCredentialStore::new(dir.path().join("c"), b"short").is_err());
    }
}
