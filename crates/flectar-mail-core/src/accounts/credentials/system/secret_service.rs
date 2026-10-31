//! The freedesktop Secret Service behind keyring. keyring passes its client's
//! errors on as platform failures; this names their cause, such as no keyring
//! service running at all, so the user knows what to fix.

use dbus_secret_service::Error;

const NO_PROVIDER: &str = "no keyring service is running; install or start one that provides \
                           the Secret Service, such as GNOME Keyring, KWallet or KeePassXC, and \
                           try again";
const FAILED_TO_START: &str = "the keyring service failed to start";
const LOCKED: &str = "the keyring is locked; unlock it and try again";

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
