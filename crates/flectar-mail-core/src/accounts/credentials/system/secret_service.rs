//! The freedesktop Secret Service behind keyring. keyring stores items in
//! the collection behind the `default` alias, and a session without one, such
//! as a compositor started without the PAM keyring module, makes every store
//! fail. libsecret and oo7 create the collection on first use instead, and the
//! provider asks the user to choose its password; this does the same.

use dbus_secret_service::{EncryptionType, Error, SecretService};
use std::sync::{Mutex, PoisonError};

/// What libsecret names the collection it creates for the alias.
const DEFAULT_COLLECTION_LABEL: &str = "Default keyring";

const NO_PROVIDER: &str = "no keyring service is running; install or start one that provides \
                           the Secret Service, such as GNOME Keyring, KWallet or KeePassXC, and \
                           try again";
const FAILED_TO_START: &str = "the keyring service failed to start";
const LOCKED: &str = "the keyring is locked; unlock it and try again";

/// Whether keyring failed because the session has no default collection. It
/// reports the Secret Service's empty alias as a storage access failure.
pub(super) fn missing_default_collection(error: &keyring::Error) -> bool {
    matches!(
        error,
        keyring::Error::NoStorageAccess(cause)
            if matches!(cause.downcast_ref(), Some(Error::NoResult))
    )
}

/// Create the collection behind the `default` alias. The provider prompts for
/// the new keyring's password, and dismissing that prompt fails with a storage
/// access error carrying [`Error::Prompt`].
pub(super) fn create_default_collection() -> keyring::Result<()> {
    // One prompt at a time; a caller that waited finds the collection if the
    // first prompt created it.
    static CREATING: Mutex<()> = Mutex::new(());
    let _creating = CREATING.lock().unwrap_or_else(PoisonError::into_inner);
    let service = SecretService::connect(EncryptionType::Plain)
        .map_err(|error| keyring::Error::PlatformFailure(Box::new(error)))?;
    match service.get_default_collection() {
        Ok(_) => Ok(()),
        Err(Error::NoResult) => service
            .create_collection(DEFAULT_COLLECTION_LABEL, "default")
            .map(drop)
            .map_err(keyring::secret_service::decode_error),
        Err(error) => Err(keyring::secret_service::decode_error(error)),
    }
}

/// Explain a Secret Service failure that keyring passed on, keeping the D-Bus
/// error for bug reports. `None` when the Secret Service client did not raise
/// `cause`.
pub(super) fn unavailable_reason(
    cause: &(dyn std::error::Error + Send + Sync + 'static),
) -> Option<String> {
    Some(match cause.downcast_ref::<Error>()? {
        Error::Dbus(error) => {
            let name = error.name().unwrap_or_default();
            let reason = match name {
                "org.freedesktop.DBus.Error.ServiceUnknown" => NO_PROVIDER,
                // A provider that is installed but fails to start: dbus-broker
                // reports NameHasNoOwner, dbus-daemon a Spawn error.
                "org.freedesktop.DBus.Error.NameHasNoOwner" => FAILED_TO_START,
                _ if name.starts_with("org.freedesktop.DBus.Error.Spawn.") => FAILED_TO_START,
                "org.freedesktop.Secret.Error.IsLocked" => LOCKED,
                _ => "the keyring service failed",
            };
            match (error.name(), error.message()) {
                (Some(name), Some(message)) => format!("{reason} ({name}: {message})"),
                (Some(detail), None) | (None, Some(detail)) => format!("{reason} ({detail})"),
                (None, None) => reason.into(),
            }
        }
        Error::Unavailable => NO_PROVIDER.into(),
        Error::Prompt => "the keyring prompt was dismissed or could not be shown; try again and \
                          unlock or create the keyring when asked"
            .into(),
        Error::Locked => LOCKED.into(),
        Error::NoResult => "the keyring service has no default keyring; choose one as the default \
                            in your keyring manager and try again"
            .into(),
        other => format!("the keyring service failed ({other})"),
    })
}
