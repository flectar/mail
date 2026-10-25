//! Secrets live in the platform credential service and never in SQLite.
//! Desktop builds use the system keychain; mobile hosts inject their own
//! `CredentialStore`. Debug builds may explicitly inject a plaintext file
//! backend for isolated development containers; release builds never enable
//! that backend.

use crate::error::{CoreError, Result};
use std::borrow::Cow;
use std::sync::Arc;

#[cfg(desktop)]
mod system;
#[cfg(all(desktop, any(test, debug_assertions)))]
pub use system::DevelopmentFileCredentialStore;
#[cfg(desktop)]
pub use system::SystemCredentialStore;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Slot {
    Password,
    RefreshToken,
    AccessToken,
    /// OAuth application registration that issued this account's refresh
    /// token. Keeping it per-account prevents a later custom-app override from
    /// invalidating accounts connected with the bundled registration.
    OAuthClientId,
    OAuthClientSecret,
    /// Versioned, atomically replaced OAuth refresh token and application
    /// registration. The older individual OAuth slots remain readable for
    /// migration and are removed when an account is deleted.
    OAuthBundle,
    /// App-level AI API key (stored under account id 0).
    AiApiKey,
    /// App password of an account's one CalDAV server, from before an
    /// account could sync several. `caldav::task::move_secret` re-files it
    /// under its connection.
    AccountCaldavPassword,
    /// The app password of the CalDAV connection with this id. Google and
    /// Microsoft connections ride the account's OAuth tokens instead.
    CaldavPassword(i64),
    /// CardDAV app password. Kept separate because a user may configure
    /// different DAV providers for calendars and contacts.
    CarddavPassword,
}

impl Slot {
    /// The secret's stable name within its account.
    pub fn key(&self) -> Cow<'static, str> {
        match self {
            Slot::Password => "password".into(),
            Slot::RefreshToken => "refresh_token".into(),
            Slot::AccessToken => "access_token".into(),
            Slot::OAuthClientId => "oauth_client_id".into(),
            Slot::OAuthClientSecret => "oauth_client_secret".into(),
            Slot::OAuthBundle => "oauth_bundle".into(),
            Slot::AiApiKey => "ai_api_key".into(),
            Slot::AccountCaldavPassword => "caldav_password".into(),
            Slot::CaldavPassword(connection_id) => {
                format!("caldav_password:{connection_id}").into()
            }
            Slot::CarddavPassword => "carddav_password".into(),
        }
    }
}

/// Platform-owned secret persistence. Implementations must be durable across
/// process death and must never put secret material in SQLite or logs.
pub trait CredentialStore: Send + Sync {
    fn store(&self, account_id: i64, slot: Slot, secret: &str) -> Result<()>;
    fn load(&self, account_id: i64, slot: Slot) -> Result<String>;
    fn delete(&self, account_id: i64, slot: Slot) -> Result<()>;
    /// Remove the account's secrets. A backend that cannot enumerate its
    /// entries removes the fixed slots; callers delete each
    /// [`Slot::CaldavPassword`] with its connection first.
    fn delete_all(&self, account_id: i64) -> Result<()>;

    /// Verify that this backend can complete a write/read/delete cycle before
    /// an external OAuth grant or a remote account check begins.
    fn check_available(&self) -> Result<()> {
        static PROBE_LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        let _guard = PROBE_LOCK
            .get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let account_id = i64::MIN;
        let secret = "flectar-credential-probe-v1";
        self.store(account_id, Slot::Password, secret)
            .map_err(as_unavailable)?;
        let loaded = self.load(account_id, Slot::Password);
        let deleted = self.delete(account_id, Slot::Password);

        let loaded = loaded.map_err(as_unavailable)?;
        deleted.map_err(as_unavailable)?;
        if loaded != secret {
            return Err(CoreError::CredentialStoreUnavailable(
                "credential verification returned different data".into(),
            ));
        }
        Ok(())
    }
}

fn as_unavailable(error: CoreError) -> CoreError {
    match error {
        CoreError::CredentialStoreUnavailable(_) => error,
        other => CoreError::CredentialStoreUnavailable(other.to_string()),
    }
}

pub type CredentialStoreHandle = Arc<dyn CredentialStore>;

/// For hosts that have not integrated secure storage yet. Every operation
/// fails, so account setup reports the gap instead of persisting secrets
/// anywhere else.
#[derive(Debug, Default)]
pub struct UnavailableCredentialStore;

impl UnavailableCredentialStore {
    fn error() -> CoreError {
        CoreError::CredentialStoreUnavailable(
            "this platform host does not provide secure credential storage".into(),
        )
    }
}

impl CredentialStore for UnavailableCredentialStore {
    fn store(&self, _: i64, _: Slot, _: &str) -> Result<()> {
        Err(Self::error())
    }

    fn load(&self, _: i64, _: Slot) -> Result<String> {
        Err(Self::error())
    }

    fn delete(&self, _: i64, _: Slot) -> Result<()> {
        Err(Self::error())
    }

    fn delete_all(&self, _: i64) -> Result<()> {
        Err(Self::error())
    }
}

pub async fn store_async(
    store: CredentialStoreHandle,
    account_id: i64,
    slot: Slot,
    secret: String,
) -> Result<()> {
    tokio::task::spawn_blocking(move || store.store(account_id, slot, &secret))
        .await
        .map_err(|e| CoreError::Other(e.to_string()))?
}

pub async fn load_async(
    store: CredentialStoreHandle,
    account_id: i64,
    slot: Slot,
) -> Result<String> {
    tokio::task::spawn_blocking(move || store.load(account_id, slot))
        .await
        .map_err(|e| CoreError::Other(e.to_string()))?
}

pub async fn delete_all_async(store: CredentialStoreHandle, account_id: i64) -> Result<()> {
    tokio::task::spawn_blocking(move || store.delete_all(account_id))
        .await
        .map_err(|e| CoreError::Other(e.to_string()))?
}

pub async fn delete_async(store: CredentialStoreHandle, account_id: i64, slot: Slot) -> Result<()> {
    tokio::task::spawn_blocking(move || store.delete(account_id, slot))
        .await
        .map_err(|e| CoreError::Other(e.to_string()))?
}

pub async fn check_available_async(store: CredentialStoreHandle) -> Result<()> {
    tokio::task::spawn_blocking(move || store.check_available())
        .await
        .map_err(|e| CoreError::Other(e.to_string()))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct MemoryStore(std::sync::Mutex<std::collections::HashMap<(i64, Slot), String>>);

    impl CredentialStore for MemoryStore {
        fn store(&self, account_id: i64, slot: Slot, secret: &str) -> Result<()> {
            self.0
                .lock()
                .unwrap()
                .insert((account_id, slot), secret.into());
            Ok(())
        }

        fn load(&self, account_id: i64, slot: Slot) -> Result<String> {
            self.0
                .lock()
                .unwrap()
                .get(&(account_id, slot))
                .cloned()
                .ok_or_else(|| CoreError::Auth("no stored credential".into()))
        }

        fn delete(&self, account_id: i64, slot: Slot) -> Result<()> {
            self.0.lock().unwrap().remove(&(account_id, slot));
            Ok(())
        }

        fn delete_all(&self, account_id: i64) -> Result<()> {
            self.0
                .lock()
                .unwrap()
                .retain(|(id, _), _| *id != account_id);
            Ok(())
        }
    }

    #[tokio::test]
    async fn unavailable_store_reports_the_missing_backend() {
        let store: CredentialStoreHandle = Arc::new(UnavailableCredentialStore);
        assert!(matches!(
            check_available_async(store.clone()).await,
            Err(CoreError::CredentialStoreUnavailable(_))
        ));
        assert!(
            store_async(store.clone(), 1, Slot::Password, "secret".into())
                .await
                .is_err()
        );
        assert!(load_async(store, 1, Slot::Password).await.is_err());
    }

    #[tokio::test]
    async fn injected_store_contract_round_trips_and_deletes_an_account() {
        let memory = Arc::new(MemoryStore::default());
        let store: CredentialStoreHandle = memory.clone();
        check_available_async(store.clone()).await.unwrap();
        assert!(memory.0.lock().unwrap().is_empty());
        store_async(store.clone(), 9, Slot::Password, "secret".into())
            .await
            .unwrap();
        store_async(store.clone(), 10, Slot::Password, "other".into())
            .await
            .unwrap();
        assert_eq!(
            load_async(store.clone(), 9, Slot::Password).await.unwrap(),
            "secret"
        );
        delete_all_async(store.clone(), 9).await.unwrap();
        assert!(load_async(store.clone(), 9, Slot::Password).await.is_err());
        assert_eq!(
            load_async(store, 10, Slot::Password).await.unwrap(),
            "other"
        );
    }
}
