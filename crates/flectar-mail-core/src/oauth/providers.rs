use crate::error::{CoreError, Result};
use crate::models::Provider;

/// Incremental-consent scope for Google Calendar (CalDAV access). Not in
/// the default GOOGLE scopes: mail-only accounts should not be asked for it.
pub const GOOGLE_CALENDAR_SCOPE: &str = "https://www.googleapis.com/auth/calendar";

/// Incremental-consent scope (Microsoft Graph) for creating Teams meetings.
/// Kept out of the default MICROSOFT scopes: those all target
/// `outlook.office.com`, and Microsoft issues a single-resource access token,
/// so mixing a `graph.microsoft.com` scope into the mail token would break the
/// IMAP/SMTP audience. It is consented separately and redeemed for its own
/// Graph-audience token (see `TokenProvider::access_token_for_scope`).
pub const MS_ONLINE_MEETINGS_SCOPE: &str = "https://graph.microsoft.com/OnlineMeetings.ReadWrite";

/// Incremental-consent scope (Microsoft Graph) for writing events into the
/// user's Outlook / Microsoft 365 calendar, so events created in the app show
/// up in Outlook and Teams. Same single-resource caveat as the meetings scope.
pub const MS_CALENDARS_SCOPE: &str = "https://graph.microsoft.com/Calendars.ReadWrite";
use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

pub struct OAuthProviderConfig {
    pub auth_url: &'static str,
    pub token_url: &'static str,
    pub scopes: &'static [&'static str],
    /// Build-time environment variable names for the bundled desktop app.
    pub client_id_env: &'static str,
    pub client_secret_env: &'static str,
}

pub const GOOGLE: OAuthProviderConfig = OAuthProviderConfig {
    auth_url: "https://accounts.google.com/o/oauth2/v2/auth",
    token_url: "https://oauth2.googleapis.com/token",
    // Gmail's permanent-delete endpoint requires the full mail scope. Existing
    // gmail.modify grants must be reauthorized before deletion can succeed.
    scopes: &["https://mail.google.com/", "openid", "email", "profile"],
    client_id_env: "FLECTAR_GOOGLE_DESKTOP_CLIENT_ID",
    client_secret_env: "FLECTAR_GOOGLE_DESKTOP_CLIENT_SECRET",
};

pub const MICROSOFT: OAuthProviderConfig = OAuthProviderConfig {
    auth_url: "https://login.microsoftonline.com/common/oauth2/v2.0/authorize",
    token_url: "https://login.microsoftonline.com/common/oauth2/v2.0/token",
    scopes: &[
        "https://outlook.office.com/IMAP.AccessAsUser.All",
        "https://outlook.office.com/SMTP.Send",
        "offline_access",
        "openid",
        "profile",
        "email",
    ],
    client_id_env: "FLECTAR_MICROSOFT_DESKTOP_CLIENT_ID",
    // Entra public desktop clients use PKCE and never have a client secret.
    client_secret_env: "",
};

pub fn for_provider(p: Provider) -> Option<&'static OAuthProviderConfig> {
    match p {
        Provider::Gmail => Some(&GOOGLE),
        Provider::Microsoft => Some(&MICROSOFT),
        Provider::Generic => None,
    }
}

/// Validate local syntax, not the existence or consent status of a provider
/// registration. Microsoft application IDs are non-nil, hyphenated GUIDs.
pub fn validate_client_id(provider: Provider, client_id: &str) -> Result<()> {
    let id = client_id.trim();
    let valid = match provider {
        Provider::Microsoft => {
            let bytes = id.as_bytes();
            bytes.len() == 36
                && bytes.iter().enumerate().all(|(index, byte)| {
                    if matches!(index, 8 | 13 | 18 | 23) {
                        *byte == b'-'
                    } else {
                        byte.is_ascii_hexdigit()
                    }
                })
                && id != "00000000-0000-0000-0000-000000000000"
        }
        // Google client IDs are opaque. Reject our former build placeholder
        // without imposing an undocumented format on existing registrations.
        Provider::Gmail => {
            !id.is_empty()
                && !id.eq_ignore_ascii_case("test.apps.googleusercontent.com")
                && !id.chars().any(|ch| ch.is_whitespace() || ch.is_control())
        }
        Provider::Generic => false,
    };
    if valid {
        Ok(())
    } else {
        Err(CoreError::Auth(match provider {
            Provider::Microsoft => "Enter the Microsoft Application (client) ID from your app registration in Sign-in settings; it must be a nonzero GUID, not a placeholder.".into(),
            Provider::Gmail => "Enter your Google OAuth client ID in Sign-in settings; a test placeholder cannot be used for sign-in.".into(),
            Provider::Generic => "provider does not use oauth".into(),
        }))
    }
}

pub(crate) fn validate_custom_client_id(provider: Provider, client_id: &str) -> Result<()> {
    if client_id.trim().is_empty() {
        // Clearing a custom registration restores the bundled default.
        Ok(())
    } else {
        validate_client_id(provider, client_id)
    }
}

fn checked_credentials(
    provider: Provider,
    client_id: &str,
    client_secret: Option<&str>,
) -> Result<(String, Option<String>)> {
    validate_client_id(provider, client_id)?;
    let secret = client_secret
        .filter(|_| provider == Provider::Gmail)
        .map(str::trim)
        .filter(|secret| !secret.is_empty())
        .map(str::to_owned);
    Ok((client_id.trim().to_owned(), secret))
}

/// Client credentials the user entered in Settings. Refreshed on startup and
/// whenever settings change. An explicit user registration overrides the app
/// registration bundled by the build.
type ConfiguredCredentials = HashMap<Provider, (String, Option<String>)>;

fn configured() -> &'static RwLock<ConfiguredCredentials> {
    static MAP: OnceLock<RwLock<ConfiguredCredentials>> = OnceLock::new();
    MAP.get_or_init(Default::default)
}

pub fn set_configured(provider: Provider, client_id: &str, client_secret: &str) {
    // The map contains independent credential values and has no multi-field
    // transactional invariant. If an unrelated caller panics while holding
    // the lock, recovering its contents is safer than turning every future
    // account setup into another process panic.
    let mut map = configured()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let id = client_id.trim();
    if id.is_empty() {
        map.remove(&provider);
    } else {
        let secret = (provider == Provider::Gmail)
            .then(|| client_secret.trim())
            .filter(|secret| !secret.is_empty())
            .map(str::to_owned);
        map.insert(provider, (id.to_string(), secret));
    }
}

fn bundled_credentials(provider: Provider) -> Option<(String, Option<String>)> {
    #[cfg(mobile)]
    let (client_id, client_secret) = match provider {
        Provider::Gmail => (option_env!("FLECTAR_GOOGLE_ANDROID_CLIENT_ID"), None),
        Provider::Microsoft => (option_env!("FLECTAR_MICROSOFT_ANDROID_CLIENT_ID"), None),
        Provider::Generic => return None,
    };
    #[cfg(desktop)]
    let (client_id, client_secret) = match provider {
        Provider::Gmail => (
            option_env!("FLECTAR_GOOGLE_DESKTOP_CLIENT_ID"),
            option_env!("FLECTAR_GOOGLE_DESKTOP_CLIENT_SECRET"),
        ),
        Provider::Microsoft => (option_env!("FLECTAR_MICROSOFT_DESKTOP_CLIENT_ID"), None),
        Provider::Generic => return None,
    };
    // Invalid build-time defaults must not enable the sign-in buttons. Runtime
    // or user configuration may still provide a real registration.
    checked_credentials(provider, client_id?, client_secret).ok()
}

pub fn has_bundled_credentials(provider: Provider) -> bool {
    bundled_credentials(provider).is_some()
}

/// Resolve the OAuth registration used for a new authorization or token
/// refresh. User-entered settings win, followed by credentials compiled into
/// the application, followed by the documented Flectar Mail runtime variables.
pub fn resolve_credentials(provider: Provider) -> Result<(String, Option<String>)> {
    for_provider(provider).ok_or_else(|| CoreError::Auth("provider does not use oauth".into()))?;
    if let Some(entry) = configured()
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&provider)
    {
        // An invalid saved override must not silently switch registrations.
        return checked_credentials(provider, &entry.0, entry.1.as_deref());
    }
    if let Some(entry) = bundled_credentials(provider) {
        return Ok(entry);
    }

    let (client_id_env, client_secret_env) = match provider {
        Provider::Gmail => (
            "FLECTAR_MAIL_GOOGLE_CLIENT_ID",
            Some("FLECTAR_MAIL_GOOGLE_CLIENT_SECRET"),
        ),
        Provider::Microsoft => ("FLECTAR_MAIL_MS_CLIENT_ID", None),
        Provider::Generic => {
            return Err(CoreError::Auth("provider does not use oauth".into()));
        }
    };
    if let Some(id) = std::env::var(client_id_env)
        .ok()
        .filter(|id| !id.trim().is_empty())
    {
        let secret = client_secret_env
            .and_then(|name| std::env::var(name).ok())
            .filter(|secret| !secret.trim().is_empty());
        return checked_credentials(provider, &id, secret.as_deref());
    }
    Err(CoreError::Auth(format!(
        "no OAuth app configured for {}: use custom OAuth credentials in Settings → General, or build with {}",
        provider.as_str(),
        bundled_client_id_env(provider)
    )))
}

fn bundled_client_id_env(provider: Provider) -> &'static str {
    #[cfg(mobile)]
    return match provider {
        Provider::Gmail => "FLECTAR_GOOGLE_ANDROID_CLIENT_ID",
        Provider::Microsoft => "FLECTAR_MICROSOFT_ANDROID_CLIENT_ID",
        Provider::Generic => "",
    };
    #[cfg(desktop)]
    match provider {
        Provider::Gmail => GOOGLE.client_id_env,
        Provider::Microsoft => MICROSOFT.client_id_env,
        Provider::Generic => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_nil_and_malformed_microsoft_app_ids() {
        for id in [
            "",
            " ",
            "00000000-0000-0000-0000-000000000000",
            "00000000000000000000000000000000",
            "ms-id",
            "11111111-2222-4333-8444-55555555555z",
            "111111112222-4333-8444-555555555555",
            "{11111111-2222-4333-8444-555555555555}",
            "11111111-2222-4333-8444-555555555555?x=1",
        ] {
            assert!(
                validate_client_id(Provider::Microsoft, id).is_err(),
                "{id:?}"
            );
            assert!(checked_credentials(Provider::Microsoft, id, None).is_err());
        }
        for id in [
            "11111111-2222-4333-8444-555555555555",
            "ABCDEF01-2345-4678-9ABC-DEF012345678",
            "  abcdef01-2345-4678-9abc-def012345678  ",
        ] {
            let (actual, secret) =
                checked_credentials(Provider::Microsoft, id, Some("ignored")).unwrap();
            assert_eq!(actual, id.trim());
            assert!(secret.is_none());
        }
        assert!(validate_custom_client_id(Provider::Microsoft, "  ").is_ok());
    }

    #[test]
    fn rejects_legacy_google_placeholder_and_preserves_opaque_ids() {
        for id in [
            "",
            "test.apps.googleusercontent.com",
            " TEST.apps.googleusercontent.com ",
            "id with spaces",
        ] {
            assert!(checked_credentials(Provider::Gmail, id, None).is_err());
        }
        assert_eq!(
            checked_credentials(Provider::Gmail, " custom-google-id ", Some(" secret ")).unwrap(),
            ("custom-google-id".into(), Some("secret".into()))
        );
    }

    #[test]
    fn resolve_uses_configured_values_and_clears() {
        // Explicit settings always win, including in a release build that has
        // an official registration compiled into it.
        set_configured(
            Provider::Microsoft,
            "  11111111-2222-4333-8444-555555555555  ",
            "",
        );
        let (id, secret) = resolve_credentials(Provider::Microsoft).unwrap();
        assert_eq!(id, "11111111-2222-4333-8444-555555555555");
        assert_eq!(secret, None);

        set_configured(
            Provider::Microsoft,
            "11111111-2222-4333-8444-555555555555",
            "  ignored-secret ",
        );
        let (_, secret) = resolve_credentials(Provider::Microsoft).unwrap();
        assert_eq!(secret, None);

        set_configured(Provider::Gmail, "google-id", "  s3cret ");
        let (_, secret) = resolve_credentials(Provider::Gmail).unwrap();
        assert_eq!(secret.as_deref(), Some("s3cret"));
        set_configured(Provider::Gmail, "", "");

        // Even if a valid default is bundled, an invalid explicit override
        // cannot authorize under that default's different app registration.
        set_configured(
            Provider::Microsoft,
            "00000000-0000-0000-0000-000000000000",
            "",
        );
        assert!(resolve_credentials(Provider::Microsoft).is_err());

        // Empty id clears the custom registration. A bundled registration may
        // then become active, so only verify that the custom value is gone.
        set_configured(Provider::Microsoft, "", "whatever");
        assert_ne!(
            resolve_credentials(Provider::Microsoft)
                .ok()
                .map(|entry| entry.0),
            Some("11111111-2222-4333-8444-555555555555".to_owned())
        );
    }

    #[test]
    fn resolve_rejects_non_oauth_provider() {
        assert!(resolve_credentials(Provider::Generic).is_err());
    }
}
