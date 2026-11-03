//! Account secrets in an Apple Keychain, as the iOS host stores them.
//!
//! Each account keeps its secrets as generic-password items under its own
//! service, `com.flectar.mail.account.<id>`, with the slot as the item's
//! account name. Removing an account therefore deletes one service. Items are
//! readable once the device has been unlocked after starting up, so future
//! background sync can reach them, and never leave the device: like the
//! Android Keystore, a backup restored onto another device asks the user to
//! sign in again instead of carrying passwords and tokens along.
//!
//! The Keychain itself sits behind [`Keychain`], so these rules run in the
//! unit tests on every platform.

use flectar_mail_core::accounts::credentials::{CredentialStore, Slot};
use flectar_mail_core::error::{CoreError, Result};
use std::sync::{Mutex, PoisonError};

/// `errSecItemNotFound`
pub(crate) const ITEM_NOT_FOUND: i32 = -25300;
/// `errSecDuplicateItem`
pub(crate) const DUPLICATE_ITEM: i32 = -25299;
/// `errSecInteractionNotAllowed`: the device has not been unlocked since it
/// started.
pub(crate) const INTERACTION_NOT_ALLOWED: i32 = -25308;

/// A failed Keychain call: its `OSStatus` and the system's description.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct KeychainError {
    pub(crate) status: i32,
    pub(crate) message: String,
}

/// Generic-password operations on the app's Keychain. Implementations create
/// and update items as readable after the first unlock and only on this
/// device.
pub(crate) trait Keychain: Send + Sync {
    fn read(&self, service: &str, account: &str) -> std::result::Result<Vec<u8>, KeychainError>;
    fn add(
        &self,
        service: &str,
        account: &str,
        secret: &[u8],
    ) -> std::result::Result<(), KeychainError>;
    fn update(
        &self,
        service: &str,
        account: &str,
        secret: &[u8],
    ) -> std::result::Result<(), KeychainError>;
    fn delete(&self, service: &str, account: &str) -> std::result::Result<(), KeychainError>;
    /// Delete every item of `service`.
    fn delete_service(&self, service: &str) -> std::result::Result<(), KeychainError>;
}

pub(crate) struct KeychainCredentialStore<K> {
    keychain: K,
    /// Serializes this process's writes, so a store never races its own
    /// update and add.
    transaction: Mutex<()>,
}

impl<K: Keychain> KeychainCredentialStore<K> {
    pub(crate) fn new(keychain: K) -> Self {
        Self {
            keychain,
            transaction: Mutex::new(()),
        }
    }
}

fn service(account_id: i64) -> String {
    format!("com.flectar.mail.account.{account_id}")
}

fn unavailable(error: KeychainError) -> CoreError {
    if error.status == INTERACTION_NOT_ALLOWED {
        return CoreError::CredentialStoreUnavailable(
            "this device has not been unlocked since it restarted; unlock it and try again".into(),
        );
    }
    CoreError::Keyring(format!(
        "iOS Keychain error {}: {}",
        error.status, error.message
    ))
}

/// Deleting an item that is already gone succeeds.
fn deleted(result: std::result::Result<(), KeychainError>) -> Result<()> {
    match result {
        Err(error) if error.status != ITEM_NOT_FOUND => Err(unavailable(error)),
        _ => Ok(()),
    }
}

impl<K: Keychain> CredentialStore for KeychainCredentialStore<K> {
    fn store(&self, account_id: i64, slot: Slot, secret: &str) -> Result<()> {
        let _transaction = self
            .transaction
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let (service, account) = (service(account_id), slot.key());
        let secret = secret.as_bytes();
        // Updating first keeps an existing item's identity; only a missing
        // item is added.
        let result = match self.keychain.update(&service, &account, secret) {
            Err(error) if error.status == ITEM_NOT_FOUND => {
                match self.keychain.add(&service, &account, secret) {
                    // Another process added it in between.
                    Err(error) if error.status == DUPLICATE_ITEM => {
                        self.keychain.update(&service, &account, secret)
                    }
                    result => result,
                }
            }
            result => result,
        };
        result.map_err(unavailable)
    }

    fn load(&self, account_id: i64, slot: Slot) -> Result<String> {
        match self.keychain.read(&service(account_id), &slot.key()) {
            Ok(secret) => String::from_utf8(secret).map_err(|_| {
                CoreError::Auth("stored credentials are damaged; sign in again".into())
            }),
            Err(error) if error.status == ITEM_NOT_FOUND => {
                Err(CoreError::Auth("no stored credential".into()))
            }
            Err(error) => Err(unavailable(error)),
        }
    }

    fn delete(&self, account_id: i64, slot: Slot) -> Result<()> {
        let _transaction = self
            .transaction
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        deleted(self.keychain.delete(&service(account_id), &slot.key()))
    }

    fn delete_all(&self, account_id: i64) -> Result<()> {
        let _transaction = self
            .transaction
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        deleted(self.keychain.delete_service(&service(account_id)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// An in-memory Keychain that records calls and can fail on demand.
    #[derive(Default)]
    struct FakeKeychain {
        items: Mutex<BTreeMap<(String, String), Vec<u8>>>,
        calls: Mutex<Vec<&'static str>>,
        /// Fail every call with this status.
        failure: Option<i32>,
        /// Report a duplicate on the next add, as when another process adds
        /// the item between our update and add.
        race: Mutex<bool>,
    }

    impl FakeKeychain {
        fn call(&self, name: &'static str) -> std::result::Result<(), KeychainError> {
            self.calls.lock().unwrap().push(name);
            match self.failure {
                Some(status) => Err(error(status)),
                None => Ok(()),
            }
        }
    }

    fn error(status: i32) -> KeychainError {
        KeychainError {
            status,
            message: format!("status {status}"),
        }
    }

    fn key(service: &str, account: &str) -> (String, String) {
        (service.to_owned(), account.to_owned())
    }

    impl Keychain for FakeKeychain {
        fn read(
            &self,
            service: &str,
            account: &str,
        ) -> std::result::Result<Vec<u8>, KeychainError> {
            self.call("read")?;
            let items = self.items.lock().unwrap();
            items
                .get(&key(service, account))
                .cloned()
                .ok_or_else(|| error(ITEM_NOT_FOUND))
        }

        fn add(
            &self,
            service: &str,
            account: &str,
            secret: &[u8],
        ) -> std::result::Result<(), KeychainError> {
            self.call("add")?;
            let mut items = self.items.lock().unwrap();
            if std::mem::take(&mut *self.race.lock().unwrap()) {
                items.insert(key(service, account), b"from another process".to_vec());
            }
            if items.contains_key(&key(service, account)) {
                return Err(error(DUPLICATE_ITEM));
            }
            items.insert(key(service, account), secret.to_vec());
            Ok(())
        }

        fn update(
            &self,
            service: &str,
            account: &str,
            secret: &[u8],
        ) -> std::result::Result<(), KeychainError> {
            self.call("update")?;
            let mut items = self.items.lock().unwrap();
            let item = items
                .get_mut(&key(service, account))
                .ok_or_else(|| error(ITEM_NOT_FOUND))?;
            *item = secret.to_vec();
            Ok(())
        }

        fn delete(&self, service: &str, account: &str) -> std::result::Result<(), KeychainError> {
            self.call("delete")?;
            self.items
                .lock()
                .unwrap()
                .remove(&key(service, account))
                .map(drop)
                .ok_or_else(|| error(ITEM_NOT_FOUND))
        }

        fn delete_service(&self, service: &str) -> std::result::Result<(), KeychainError> {
            self.call("delete_service")?;
            let mut items = self.items.lock().unwrap();
            let before = items.len();
            items.retain(|(item_service, _), _| item_service != service);
            if items.len() == before {
                Err(error(ITEM_NOT_FOUND))
            } else {
                Ok(())
            }
        }
    }

    fn store(keychain: FakeKeychain) -> KeychainCredentialStore<FakeKeychain> {
        KeychainCredentialStore::new(keychain)
    }

    #[test]
    fn secrets_round_trip_per_account_service() {
        let store = store(FakeKeychain::default());
        store.store(7, Slot::Password, "first").unwrap();
        store.store(7, Slot::Password, "second").unwrap();
        store.store(70, Slot::Password, "other account").unwrap();
        store.store(7, Slot::CaldavPassword(3), "calendar").unwrap();
        assert_eq!(store.load(7, Slot::Password).unwrap(), "second");
        assert_eq!(store.load(70, Slot::Password).unwrap(), "other account");
        assert_eq!(store.load(7, Slot::CaldavPassword(3)).unwrap(), "calendar");
        let items = store.keychain.items.lock().unwrap();
        assert!(items.contains_key(&key("com.flectar.mail.account.7", "password")));
        assert!(items.contains_key(&key("com.flectar.mail.account.7", "caldav_password:3")));
        // A new item is added once; a later store updates it in place.
        drop(items);
        let calls = store.keychain.calls.lock().unwrap().clone();
        assert_eq!(&calls[..3], ["update", "add", "update"]);
    }

    #[test]
    fn a_concurrent_add_is_updated_instead_of_failing() {
        let keychain = FakeKeychain::default();
        *keychain.race.lock().unwrap() = true;
        let store = store(keychain);
        store.store(1, Slot::RefreshToken, "token").unwrap();
        assert_eq!(store.load(1, Slot::RefreshToken).unwrap(), "token");
        assert_eq!(
            store.keychain.calls.lock().unwrap()[..3],
            ["update", "add", "update"]
        );
    }

    #[test]
    fn missing_and_damaged_secrets_ask_the_user_to_sign_in() {
        let store = store(FakeKeychain::default());
        assert!(matches!(
            store.load(1, Slot::Password),
            Err(CoreError::Auth(message)) if message == "no stored credential"
        ));
        store.keychain.items.lock().unwrap().insert(
            key("com.flectar.mail.account.1", "password"),
            vec![0xff, 0xfe],
        );
        assert!(matches!(
            store.load(1, Slot::Password),
            Err(CoreError::Auth(message)) if message.contains("damaged")
        ));
    }

    #[test]
    fn deleting_is_idempotent_and_scoped_to_one_account() {
        let store = store(FakeKeychain::default());
        store.delete(1, Slot::Password).unwrap();
        store.delete_all(1).unwrap();
        store.store(1, Slot::Password, "a").unwrap();
        store.store(1, Slot::OAuthBundle, "b").unwrap();
        store.store(11, Slot::Password, "c").unwrap();
        store.delete(1, Slot::Password).unwrap();
        assert!(store.load(1, Slot::Password).is_err());
        store.delete_all(1).unwrap();
        assert!(store.load(1, Slot::OAuthBundle).is_err());
        assert_eq!(store.load(11, Slot::Password).unwrap(), "c");
    }

    #[test]
    fn keychain_failures_report_why() {
        let locked = store(FakeKeychain {
            failure: Some(INTERACTION_NOT_ALLOWED),
            ..FakeKeychain::default()
        });
        for result in [
            locked.store(1, Slot::Password, "secret"),
            locked.load(1, Slot::Password).map(drop),
            locked.delete(1, Slot::Password),
            locked.delete_all(1),
        ] {
            assert!(matches!(
                result,
                Err(CoreError::CredentialStoreUnavailable(message)) if message.contains("unlock")
            ));
        }
        // The availability probe that runs before account setup fails too.
        assert!(locked.check_available().is_err());

        let failing = store(FakeKeychain {
            failure: Some(-34018),
            ..FakeKeychain::default()
        });
        assert!(matches!(
            failing.store(1, Slot::Password, "secret"),
            Err(CoreError::Keyring(message)) if message.contains("-34018")
        ));
    }

    #[test]
    fn the_availability_probe_leaves_nothing_behind() {
        let store = store(FakeKeychain::default());
        store.check_available().unwrap();
        assert!(store.keychain.items.lock().unwrap().is_empty());
    }
}
