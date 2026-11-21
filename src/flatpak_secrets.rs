//! Where the Flatpak keeps credentials.
//!
//! Inside the sandbox, Flectar Mail asks the Secret portal for the secret the
//! desktop's keyring keeps for it, and seals the profile's credentials in a
//! file with a key derived from that secret. The app then needs no access to
//! the user's whole keyring, which Flathub requires. Credentials from versions
//! that used the keyring directly move into the file as they are used, while
//! that access is still granted.
//!
//! Desktops without a Secret portal backend, such as sway, keep using the
//! keyring over D-Bus, which works once the user grants the Flatpak that
//! access. The portal is asked on first use rather than at startup, because
//! the keyring may first have to be unlocked.

use flectar_mail_core::accounts::credentials::{
    CredentialStore, CredentialStoreHandle, MigratingCredentialStore, SealedFileCredentialStore,
    Slot, SystemCredentialStore,
};
use flectar_mail_core::config::Paths;
use flectar_mail_core::error::{CoreError, Result};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// What the user can run when the desktop has no Secret portal.
const GRANT_KEYRING: &str =
    "flatpak override --user --talk-name=org.freedesktop.secrets com.flectar.mail";
/// Long enough for the user to unlock the keyring when the portal asks.
const PORTAL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

pub(crate) fn credential_store(paths: &Paths) -> CredentialStoreHandle {
    Arc::new(FlatpakCredentials {
        sealed: paths.data_dir.join("credentials.sealed"),
        keyring: Arc::new(SystemCredentialStore::for_profile(paths)),
        chosen: Mutex::new(None),
    })
}

struct FlatpakCredentials {
    sealed: PathBuf,
    keyring: Arc<dyn CredentialStore>,
    chosen: Mutex<Option<Chosen>>,
}

#[derive(Clone)]
enum Chosen {
    Portal(Arc<dyn CredentialStore>),
    /// The desktop has no Secret portal, so the keyring is used directly.
    Keyring,
}

enum Retrieved {
    Secret(Vec<u8>),
    NoPortal(String),
}

impl FlatpakCredentials {
    fn chosen(&self) -> Result<Chosen> {
        let mut chosen = self
            .chosen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(chosen) = chosen.as_ref() {
            return Ok(chosen.clone());
        }
        let decided = match retrieve_secret()? {
            Retrieved::Secret(mut secret) => {
                let sealed = SealedFileCredentialStore::new(&self.sealed, &secret);
                secret.fill(0);
                std::hint::black_box(&secret);
                Chosen::Portal(Arc::new(MigratingCredentialStore::new(
                    Arc::new(sealed?),
                    self.keyring.clone(),
                )))
            }
            Retrieved::NoPortal(reason) => {
                // Credentials sealed in another session can't be opened
                // without the portal, and the keyring doesn't have them.
                if self.sealed.exists() {
                    return Err(CoreError::CredentialStoreUnavailable(format!(
                        "Flectar Mail's credentials are sealed with your desktop keyring through the Secret portal, which this session doesn't offer ({reason})"
                    )));
                }
                tracing::warn!(%reason, "credential: no Secret portal; using the keyring over D-Bus");
                Chosen::Keyring
            }
        };
        *chosen = Some(decided.clone());
        Ok(decided)
    }

    fn with<T>(&self, call: impl FnOnce(&dyn CredentialStore) -> Result<T>) -> Result<T> {
        match self.chosen()? {
            Chosen::Portal(store) => call(store.as_ref()),
            Chosen::Keyring => call(self.keyring.as_ref()).map_err(|error| match error {
                CoreError::CredentialStoreUnavailable(message) => {
                    CoreError::CredentialStoreUnavailable(format!(
                        "{message}. Your desktop has no Secret portal, so Flectar Mail needs access to the keyring: run `{GRANT_KEYRING}` and start it again"
                    ))
                }
                other => other,
            }),
        }
    }
}

impl CredentialStore for FlatpakCredentials {
    fn store(&self, account_id: i64, slot: Slot, secret: &str) -> Result<()> {
        self.with(|store| store.store(account_id, slot, secret))
    }

    fn load(&self, account_id: i64, slot: Slot) -> Result<String> {
        self.with(|store| store.load(account_id, slot))
    }

    fn delete(&self, account_id: i64, slot: Slot) -> Result<()> {
        self.with(|store| store.delete(account_id, slot))
    }

    fn delete_all(&self, account_id: i64) -> Result<()> {
        self.with(|store| store.delete_all(account_id))
    }
}

/// Asks the Secret portal for the app's secret. A desktop without the portal
/// is an answer and is remembered; any other failure, such as a keyring the
/// user didn't unlock, is retried on the next use.
fn retrieve_secret() -> Result<Retrieved> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| CoreError::CredentialStoreUnavailable(error.to_string()))?;
    let retrieved = runtime.block_on(async {
        tokio::time::timeout(PORTAL_TIMEOUT, ashpd::desktop::secret::retrieve()).await
    });
    match retrieved {
        Ok(Ok(secret)) => Ok(Retrieved::Secret(secret)),
        Ok(Err(error)) if no_portal(&error) => Ok(Retrieved::NoPortal(error.to_string())),
        Ok(Err(error)) => Err(CoreError::CredentialStoreUnavailable(format!(
            "the Secret portal didn't return Flectar Mail's secret: {error}"
        ))),
        Err(_) => Err(CoreError::CredentialStoreUnavailable(
            "the Secret portal didn't answer; unlock your keyring and try again".into(),
        )),
    }
}

fn no_portal(error: &ashpd::Error) -> bool {
    match error {
        ashpd::Error::PortalNotFound(_) => true,
        ashpd::Error::Zbus(zbus::Error::MethodError(name, _, _)) => matches!(
            name.as_str(),
            "org.freedesktop.DBus.Error.ServiceUnknown"
                | "org.freedesktop.DBus.Error.UnknownInterface"
                | "org.freedesktop.DBus.Error.UnknownMethod"
                | "org.freedesktop.DBus.Error.UnknownObject"
        ),
        _ => false,
    }
}
