//! Moves credentials from one store to another as they are used.
//!
//! The Flatpak used to keep its credentials in the user's keyring through the
//! Secret Service and now keeps them in a file sealed through the Secret
//! portal. Reads try the new store first. A credential only the old store has
//! is copied over, read back, and then removed from the old store, so it moves
//! the first time anything asks for it and is never in neither. Writes and
//! removals also clear the old copy. Once the old store can't be reached, as in
//! a sandbox without access to the user's keyring, it is left alone.

use super::{CredentialStore, Slot};
use crate::error::{CoreError, Result};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub struct MigratingCredentialStore {
    current: Arc<dyn CredentialStore>,
    previous: Arc<dyn CredentialStore>,
    previous_unreachable: AtomicBool,
}

impl MigratingCredentialStore {
    pub fn new(current: Arc<dyn CredentialStore>, previous: Arc<dyn CredentialStore>) -> Self {
        Self {
            current,
            previous,
            previous_unreachable: AtomicBool::new(false),
        }
    }

    /// Runs `call` on the old store unless it has failed before. A missing
    /// credential is an answer, so only other errors mark it unreachable.
    fn previous<T>(
        &self,
        call: impl FnOnce(&dyn CredentialStore) -> Result<T>,
    ) -> Option<Result<T>> {
        if self.previous_unreachable.load(Ordering::Relaxed) {
            return None;
        }
        let result = call(self.previous.as_ref());
        if let Err(error) = &result
            && !is_absent(error)
        {
            tracing::info!(%error, "credential: the previous store is unreachable; leaving it alone");
            self.previous_unreachable.store(true, Ordering::Relaxed);
        }
        Some(result)
    }

    fn clear_previous(&self, call: impl FnOnce(&dyn CredentialStore) -> Result<()>) {
        let _ = self.previous(call);
    }
}

impl CredentialStore for MigratingCredentialStore {
    fn store(&self, account_id: i64, slot: Slot, secret: &str) -> Result<()> {
        self.current.store(account_id, slot, secret)?;
        self.clear_previous(|previous| previous.delete(account_id, slot));
        Ok(())
    }

    fn load(&self, account_id: i64, slot: Slot) -> Result<String> {
        let missing = match self.current.load(account_id, slot) {
            Err(error) if is_absent(&error) => error,
            other => return other,
        };
        let Some(Ok(secret)) = self.previous(|previous| previous.load(account_id, slot)) else {
            return Err(missing);
        };
        self.current.store(account_id, slot, &secret)?;
        if self.current.load(account_id, slot)? != secret {
            return Err(CoreError::CredentialStoreUnavailable(
                "a moved credential read back differently".into(),
            ));
        }
        self.clear_previous(|previous| previous.delete(account_id, slot));
        Ok(secret)
    }

    fn delete(&self, account_id: i64, slot: Slot) -> Result<()> {
        self.current.delete(account_id, slot)?;
        self.clear_previous(|previous| previous.delete(account_id, slot));
        Ok(())
    }

    fn delete_all(&self, account_id: i64) -> Result<()> {
        self.current.delete_all(account_id)?;
        self.clear_previous(|previous| previous.delete_all(account_id));
        Ok(())
    }
}

fn is_absent(error: &CoreError) -> bool {
    matches!(error, CoreError::Auth(message) if message == "no stored credential")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Memory {
        map: Mutex<HashMap<(i64, Slot), String>>,
        broken: AtomicBool,
        /// Reads work, writes fail, like a full disk.
        read_only: AtomicBool,
        /// Writes keep something other than what they were given.
        corrupting: AtomicBool,
        calls: Mutex<u32>,
    }

    impl Memory {
        fn with(entries: &[(i64, Slot, &str)]) -> Arc<Self> {
            let memory = Self::default();
            for (account, slot, secret) in entries {
                memory
                    .map
                    .lock()
                    .unwrap()
                    .insert((*account, *slot), secret.to_string());
            }
            Arc::new(memory)
        }

        fn get(&self, account: i64, slot: Slot) -> Option<String> {
            self.map.lock().unwrap().get(&(account, slot)).cloned()
        }

        fn check(&self) -> Result<()> {
            *self.calls.lock().unwrap() += 1;
            if self.broken.load(Ordering::Relaxed) {
                return Err(CoreError::CredentialStoreUnavailable("no access".into()));
            }
            Ok(())
        }
    }

    impl CredentialStore for Memory {
        fn store(&self, account_id: i64, slot: Slot, secret: &str) -> Result<()> {
            self.check()?;
            if self.read_only.load(Ordering::Relaxed) {
                return Err(CoreError::Other("disk full".into()));
            }
            let secret = if self.corrupting.load(Ordering::Relaxed) {
                &secret[1..]
            } else {
                secret
            };
            self.map
                .lock()
                .unwrap()
                .insert((account_id, slot), secret.into());
            Ok(())
        }
        fn load(&self, account_id: i64, slot: Slot) -> Result<String> {
            self.check()?;
            self.get(account_id, slot)
                .ok_or_else(|| CoreError::Auth("no stored credential".into()))
        }
        fn delete(&self, account_id: i64, slot: Slot) -> Result<()> {
            self.check()?;
            self.map.lock().unwrap().remove(&(account_id, slot));
            Ok(())
        }
        fn delete_all(&self, account_id: i64) -> Result<()> {
            self.check()?;
            self.map
                .lock()
                .unwrap()
                .retain(|(account, _), _| *account != account_id);
            Ok(())
        }
    }

    #[test]
    fn a_credential_moves_the_first_time_it_is_read() {
        let current = Memory::with(&[]);
        let previous = Memory::with(&[(1, Slot::Password, "old"), (2, Slot::Password, "other")]);
        let store = MigratingCredentialStore::new(current.clone(), previous.clone());
        assert_eq!(store.load(1, Slot::Password).unwrap(), "old");
        assert_eq!(current.get(1, Slot::Password).as_deref(), Some("old"));
        assert_eq!(previous.get(1, Slot::Password), None);
        // Untouched credentials stay where they are until asked for.
        assert_eq!(previous.get(2, Slot::Password).as_deref(), Some("other"));
        assert!(matches!(
            store.load(3, Slot::Password),
            Err(CoreError::Auth(_))
        ));
    }

    #[test]
    fn the_new_store_wins_and_writes_clear_the_old_copy() {
        let current = Memory::with(&[(1, Slot::Password, "new")]);
        let previous = Memory::with(&[
            (1, Slot::Password, "stale"),
            (1, Slot::RefreshToken, "token"),
        ]);
        let store = MigratingCredentialStore::new(current.clone(), previous.clone());
        assert_eq!(store.load(1, Slot::Password).unwrap(), "new");
        store.store(1, Slot::RefreshToken, "fresh").unwrap();
        assert_eq!(previous.get(1, Slot::RefreshToken), None);
        store.delete_all(1).unwrap();
        assert_eq!(previous.get(1, Slot::Password), None);
        assert_eq!(current.get(1, Slot::Password), None);
    }

    #[test]
    fn a_failed_move_keeps_the_old_copy() {
        for failure in ["unreachable", "read only", "corrupting"] {
            let current = Memory::with(&[]);
            let previous = Memory::with(&[(1, Slot::Password, "old")]);
            match failure {
                "unreachable" => current.broken.store(true, Ordering::Relaxed),
                "read only" => current.read_only.store(true, Ordering::Relaxed),
                _ => current.corrupting.store(true, Ordering::Relaxed),
            }
            let store = MigratingCredentialStore::new(current.clone(), previous.clone());
            let error = store.load(1, Slot::Password).unwrap_err();
            // The cause comes back, not a missing credential.
            assert!(!is_absent(&error), "{failure}: {error}");
            assert_eq!(
                previous.get(1, Slot::Password).as_deref(),
                Some("old"),
                "{failure}"
            );
        }
    }

    #[test]
    fn an_unreachable_old_store_is_asked_once() {
        let current = Memory::with(&[(1, Slot::Password, "new")]);
        let previous = Memory::with(&[]);
        previous.broken.store(true, Ordering::Relaxed);
        let store = MigratingCredentialStore::new(current.clone(), previous.clone());
        assert!(matches!(
            store.load(2, Slot::Password),
            Err(CoreError::Auth(_))
        ));
        store.store(1, Slot::Password, "newer").unwrap();
        store.delete(1, Slot::Password).unwrap();
        assert!(matches!(
            store.load(3, Slot::Password),
            Err(CoreError::Auth(_))
        ));
        assert_eq!(*previous.calls.lock().unwrap(), 1);
        store.check_available().unwrap();
    }
}
