//! Access-token lifecycle: hands out valid tokens, refreshing behind a mutex
//! when close to expiry. Refresh tokens live in the keyring; access tokens are
//! cached in memory only. They are deliberately NOT written to the keyring:
//! nothing ever reads them back (a restart just refreshes from the refresh
//! token), and a Microsoft access-token JWT is large enough to blow past the
//! Windows Credential Manager blob limit (~2560 bytes), which failed the whole
//! store with a "secure storage error" and left the account uncredentialed.

use crate::accounts::credentials::{self, CredentialStoreHandle, Slot};
use crate::error::{CoreError, Result};
use crate::models::Provider;
use crate::oauth::providers::for_provider;
use crate::oauth::redirect::OAuthRedirectBrokerHandle;
use base64::Engine;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Clone)]
struct CachedToken {
    access_token: String,
    expires_at_ms: i64,
}

#[derive(Clone)]
pub struct TokenProvider {
    cache: Arc<Mutex<HashMap<i64, CachedToken>>>,
    /// OAuth providers can rotate refresh tokens. Serialize refresh-token
    /// grants per account so a mail refresh and a Microsoft Graph refresh can
    /// never race and persist different generations. Accounts remain fully
    /// independent, so one throttled tenant cannot block every mailbox.
    refresh_locks: Arc<Mutex<HashMap<i64, Arc<Mutex<()>>>>>,
    credentials: CredentialStoreHandle,
    oauth_platform: OAuthRedirectBrokerHandle,
}

const PLATFORM_GOOGLE_PREFIX: &str = "flectar-platform-google-v1:";
const MAX_OAUTH_TOKEN_BYTES: usize = 256 * 1024;
const MAX_OAUTH_REGISTRATION_BYTES: usize = 64 * 1024;
const MAX_TOKEN_LIFETIME_SECONDS: i64 = 366 * 24 * 60 * 60;
const OAUTH_CREDENTIAL_VERSION: u8 = 1;

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
struct OAuthCredentialBundle {
    version: u8,
    refresh_token: String,
    client_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    client_secret: Option<String>,
}

pub(super) fn validate_token_fields(
    access_token: &str,
    expires_in: Option<i64>,
    refresh_token: Option<&str>,
) -> Result<()> {
    let valid_token = |value: &str| {
        !value.is_empty()
            && value.len() <= MAX_OAUTH_TOKEN_BYTES
            && !value.chars().any(char::is_control)
    };
    if !valid_token(access_token) || refresh_token.is_some_and(|token| !valid_token(token)) {
        return Err(CoreError::Auth(
            "OAuth provider returned an invalid token".into(),
        ));
    }
    if expires_in.is_some_and(|seconds| !(1..=MAX_TOKEN_LIFETIME_SECONDS).contains(&seconds)) {
        return Err(CoreError::Auth(
            "OAuth provider returned an invalid token lifetime".into(),
        ));
    }
    Ok(())
}

fn token_expiry_ms(expires_in: Option<i64>) -> i64 {
    let usable_seconds = expires_in
        .unwrap_or(3600)
        .clamp(1, MAX_TOKEN_LIFETIME_SECONDS)
        .saturating_sub(60)
        .max(1);
    crate::models::now_ms().saturating_add(usable_seconds.saturating_mul(1000))
}

fn validate_oauth_credentials(credentials: &OAuthCredentialBundle) -> Result<()> {
    let valid = |value: &str, max_bytes: usize| {
        !value.trim().is_empty() && value.len() <= max_bytes && !value.chars().any(char::is_control)
    };
    if credentials.version != OAUTH_CREDENTIAL_VERSION
        || !valid(&credentials.refresh_token, MAX_OAUTH_TOKEN_BYTES)
        || !valid(&credentials.client_id, MAX_OAUTH_REGISTRATION_BYTES)
        || credentials
            .client_secret
            .as_deref()
            .is_some_and(|secret| !valid(secret, MAX_OAUTH_REGISTRATION_BYTES))
    {
        return Err(CoreError::Auth(
            "stored OAuth credentials are invalid; reconnect the account".into(),
        ));
    }
    Ok(())
}

fn is_missing_credential(error: &CoreError) -> bool {
    matches!(error, CoreError::Auth(message) if message == "no stored credential")
}

pub fn platform_google_marker(scopes: &[&str]) -> std::result::Result<String, &'static str> {
    if scopes.is_empty() || scopes.iter().any(|scope| scope.contains('\n')) {
        return Err("invalid platform OAuth scopes");
    }
    Ok(format!(
        "{PLATFORM_GOOGLE_PREFIX}{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(scopes.join("\n"))
    ))
}

fn platform_google_scopes(marker: &str) -> Option<Vec<String>> {
    let encoded = marker.strip_prefix(PLATFORM_GOOGLE_PREFIX)?;
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded)
        .ok()?;
    let decoded = String::from_utf8(decoded).ok()?;
    let scopes = decoded
        .split('\n')
        .filter(|scope| !scope.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    (!scopes.is_empty()).then_some(scopes)
}

#[derive(serde::Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    refresh_token: Option<String>,
}

impl TokenProvider {
    pub fn new(
        credentials: CredentialStoreHandle,
        oauth_platform: OAuthRedirectBrokerHandle,
    ) -> Self {
        Self {
            cache: Arc::default(),
            refresh_locks: Arc::default(),
            credentials,
            oauth_platform,
        }
    }

    /// Drop a provider-rejected access token. The next request performs a
    /// refresh-token grant behind the account's single-flight lock.
    pub async fn invalidate(&self, account_id: i64) {
        self.cache.lock().await.remove(&account_id);
    }

    /// Remove all in-memory secrets and synchronization state for an account
    /// before its durable credentials are deleted.
    pub async fn forget_account(&self, account_id: i64) {
        let refresh_lock = self.refresh_locks.lock().await.get(&account_id).cloned();
        let _refresh_guard = match refresh_lock.as_ref() {
            Some(lock) => Some(lock.lock().await),
            None => None,
        };
        self.cache.lock().await.remove(&account_id);
        self.refresh_locks.lock().await.remove(&account_id);
    }

    /// Store initial tokens after the auth-code exchange.
    pub async fn store_initial(
        &self,
        account_id: i64,
        access_token: String,
        expires_in: Option<i64>,
        refresh_token: Option<String>,
        client_id: String,
        client_secret: Option<String>,
    ) -> Result<()> {
        validate_token_fields(&access_token, expires_in, refresh_token.as_deref())?;
        let refresh_lock = self.refresh_lock(account_id).await;
        let _refresh_guard = refresh_lock.lock().await;
        let client_secret = client_secret.filter(|secret| !secret.trim().is_empty());
        let bundle = match refresh_token {
            Some(refresh_token) => OAuthCredentialBundle {
                version: OAUTH_CREDENTIAL_VERSION,
                refresh_token,
                client_id,
                client_secret,
            },
            None => {
                let existing = self
                    .oauth_credentials(account_id, None)
                    .await
                    .map_err(|error| {
                        if is_missing_credential(&error) {
                            CoreError::Auth(
                                "OAuth provider did not return a refresh token; reconnect and grant offline access"
                                    .into(),
                            )
                        } else {
                            error
                        }
                    })?;
                if existing.client_id != client_id || existing.client_secret != client_secret {
                    return Err(CoreError::Auth(
                        "OAuth provider did not return a refresh token for the active app registration; reconnect and grant offline access"
                            .into(),
                    ));
                }
                existing
            }
        };
        validate_oauth_credentials(&bundle)?;
        self.store_oauth_credentials(account_id, &bundle).await?;
        self.remove_legacy_oauth_credentials(account_id).await;
        let expires_at_ms = token_expiry_ms(expires_in);
        self.cache.lock().await.insert(
            account_id,
            CachedToken {
                access_token,
                expires_at_ms,
            },
        );
        Ok(())
    }

    /// A valid access token, refreshed if less than 5 minutes remain.
    pub async fn access_token(&self, account_id: i64, provider: Provider) -> Result<String> {
        {
            let cache = self.cache.lock().await;
            if let Some(tok) = cache.get(&account_id)
                && tok.expires_at_ms - crate::models::now_ms() > 5 * 60 * 1000
            {
                return Ok(tok.access_token.clone());
            }
        }

        let refresh_lock = self.refresh_lock(account_id).await;
        let _refresh_guard = refresh_lock.lock().await;
        // Another caller may have refreshed while this task waited for the
        // account lock. Recheck before touching the provider.
        {
            let cache = self.cache.lock().await;
            if let Some(tok) = cache.get(&account_id)
                && tok.expires_at_ms - crate::models::now_ms() > 5 * 60 * 1000
            {
                return Ok(tok.access_token.clone());
            }
        }
        let refreshed = self.refresh(account_id, provider).await?;
        self.cache
            .lock()
            .await
            .insert(account_id, refreshed.clone());
        Ok(refreshed.access_token)
    }

    /// Mint an access token for a *specific* resource scope (e.g. Microsoft
    /// Graph), separate from the cached mail token. Microsoft issues
    /// single-resource tokens, so the mail token cannot be reused against
    /// `graph.microsoft.com`; this runs a dedicated refresh-token grant with an
    /// explicit `scope` and returns the resulting (Graph-audience) token
    /// without disturbing the mail cache or the stored mail access token.
    ///
    /// The refresh token may be rotated by this grant; the new one is persisted
    /// (it stays multi-resource, so mail refresh keeps working). A rejected
    /// grant usually means the extra scope was never consented, surfaced as
    /// `NeedsReauth` so the caller can trigger incremental consent.
    pub async fn access_token_for_scope(
        &self,
        account_id: i64,
        provider: Provider,
        scope: &str,
    ) -> Result<String> {
        let refresh_lock = self.refresh_lock(account_id).await;
        let _refresh_guard = refresh_lock.lock().await;
        let cfg =
            for_provider(provider).ok_or_else(|| CoreError::Auth("not an oauth account".into()))?;
        let mut oauth = self.oauth_credentials(account_id, Some(provider)).await?;

        let mut form = vec![
            ("grant_type".to_string(), "refresh_token".to_string()),
            ("refresh_token".to_string(), oauth.refresh_token.clone()),
            ("client_id".to_string(), oauth.client_id.clone()),
            // offline_access keeps a (rotated) refresh token coming back.
            ("scope".to_string(), format!("{scope} offline_access")),
        ];
        if let Some(cs) = oauth.client_secret.clone() {
            form.push(("client_secret".to_string(), cs));
        }

        let body = post_form(cfg.token_url, &form).await?;
        let tok: TokenResponse = serde_json::from_str(&body).map_err(|_| {
            // invalid_grant / consent_required => the scope was never granted.
            if token_error_requires_reauth(&body) {
                CoreError::NeedsReauth
            } else if token_error_is_transient(&body) {
                CoreError::Network("OAuth token service is temporarily unavailable".into())
            } else {
                CoreError::Auth(format!(
                    "scoped token request failed: {}",
                    crate::http_body::single_line_excerpt(&body, 512)
                ))
            }
        })?;
        validate_token_fields(
            &tok.access_token,
            tok.expires_in,
            tok.refresh_token.as_deref(),
        )?;

        // Do NOT overwrite Slot::AccessToken (that is the mail token, a
        // different audience). Only persist the rotated refresh token.
        if let Some(rt) = tok.refresh_token {
            oauth.refresh_token = rt;
            self.store_oauth_credentials(account_id, &oauth).await?;
        }
        Ok(tok.access_token)
    }

    async fn refresh(&self, account_id: i64, provider: Provider) -> Result<CachedToken> {
        let cfg =
            for_provider(provider).ok_or_else(|| CoreError::Auth("not an oauth account".into()))?;
        let mut oauth = self.oauth_credentials(account_id, Some(provider)).await?;

        if provider == Provider::Gmail
            && let Some(scopes) = platform_google_scopes(&oauth.refresh_token)
        {
            let scope_refs = scopes.iter().map(String::as_str).collect::<Vec<_>>();
            let result = self
                .oauth_platform
                .authorize_platform(provider, &scope_refs, false)
                .await
                .ok_or_else(|| {
                    CoreError::Auth("platform-managed Google authorization is unavailable".into())
                })??;
            validate_token_fields(&result.access_token, result.expires_in, None)?;
            return Ok(CachedToken {
                access_token: result.access_token,
                expires_at_ms: token_expiry_ms(result.expires_in),
            });
        }

        let mut form = vec![
            ("grant_type".to_string(), "refresh_token".to_string()),
            ("refresh_token".to_string(), oauth.refresh_token.clone()),
            ("client_id".to_string(), oauth.client_id.clone()),
        ];
        // The Microsoft refresh token may cover both Outlook and Graph
        // resources. Ask explicitly for the mail scopes so the resulting
        // access token always has the audience expected by IMAP and SMTP.
        if provider == Provider::Microsoft {
            form.push(("scope".to_string(), cfg.scopes.join(" ")));
        }
        if let Some(cs) = oauth.client_secret.clone() {
            form.push(("client_secret".to_string(), cs));
        }

        let body = post_form(cfg.token_url, &form).await?;
        let tok: TokenResponse = serde_json::from_str(&body).map_err(|_| {
            if token_error_requires_reauth(&body) {
                CoreError::NeedsReauth
            } else if token_error_is_transient(&body) {
                CoreError::Network("OAuth token service is temporarily unavailable".into())
            } else {
                CoreError::Auth(format!(
                    "token refresh failed: {}",
                    crate::http_body::single_line_excerpt(&body, 512)
                ))
            }
        })?;
        validate_token_fields(
            &tok.access_token,
            tok.expires_in,
            tok.refresh_token.as_deref(),
        )?;

        if let Some(rt) = tok.refresh_token {
            oauth.refresh_token = rt;
            self.store_oauth_credentials(account_id, &oauth).await?;
        }
        Ok(CachedToken {
            access_token: tok.access_token,
            expires_at_ms: token_expiry_ms(tok.expires_in),
        })
    }

    async fn oauth_credentials(
        &self,
        account_id: i64,
        provider: Option<Provider>,
    ) -> Result<OAuthCredentialBundle> {
        match credentials::load_async(self.credentials.clone(), account_id, Slot::OAuthBundle).await
        {
            Ok(encoded) => {
                let bundle: OAuthCredentialBundle =
                    serde_json::from_str(&encoded).map_err(|_| {
                        CoreError::Auth(
                            "stored OAuth credentials are invalid; reconnect the account".into(),
                        )
                    })?;
                validate_oauth_credentials(&bundle)?;
                return Ok(bundle);
            }
            Err(error) if is_missing_credential(&error) => {}
            Err(error) => return Err(error),
        }

        // Migrate accounts created before OAuth credentials were committed as
        // one value. Missing legacy registration entries use the app's current
        // provider registration, matching the previous behavior.
        let refresh_token =
            credentials::load_async(self.credentials.clone(), account_id, Slot::RefreshToken)
                .await?;
        let (client_id, fallback_client_secret) = match credentials::load_async(
            self.credentials.clone(),
            account_id,
            Slot::OAuthClientId,
        )
        .await
        {
            Ok(client_id) if !client_id.trim().is_empty() => (client_id, None),
            Ok(_) => {
                let provider = provider.ok_or_else(|| {
                    CoreError::Auth("OAuth account registration is missing".into())
                })?;
                crate::oauth::providers::resolve_credentials(provider)?
            }
            Err(error) if is_missing_credential(&error) => {
                let provider = provider.ok_or_else(|| {
                    CoreError::Auth("OAuth account registration is missing".into())
                })?;
                crate::oauth::providers::resolve_credentials(provider)?
            }
            Err(error) => return Err(error),
        };
        let client_secret = match credentials::load_async(
            self.credentials.clone(),
            account_id,
            Slot::OAuthClientSecret,
        )
        .await
        {
            Ok(secret) if !secret.trim().is_empty() => Some(secret),
            Ok(_) => fallback_client_secret,
            Err(error) if is_missing_credential(&error) => fallback_client_secret,
            Err(error) => return Err(error),
        };
        let bundle = OAuthCredentialBundle {
            version: OAUTH_CREDENTIAL_VERSION,
            refresh_token,
            client_id,
            client_secret,
        };
        validate_oauth_credentials(&bundle)?;
        self.store_oauth_credentials(account_id, &bundle).await?;
        self.remove_legacy_oauth_credentials(account_id).await;
        Ok(bundle)
    }

    async fn store_oauth_credentials(
        &self,
        account_id: i64,
        bundle: &OAuthCredentialBundle,
    ) -> Result<()> {
        let encoded = serde_json::to_string(bundle)?;
        credentials::store_async(
            self.credentials.clone(),
            account_id,
            Slot::OAuthBundle,
            encoded,
        )
        .await
    }

    async fn remove_legacy_oauth_credentials(&self, account_id: i64) {
        for slot in [
            Slot::RefreshToken,
            Slot::OAuthClientId,
            Slot::OAuthClientSecret,
        ] {
            if let Err(error) =
                credentials::delete_async(self.credentials.clone(), account_id, slot).await
            {
                tracing::warn!(
                    account_id,
                    slot = slot.as_str(),
                    %error,
                    "could not remove migrated OAuth credential"
                );
            }
        }
    }

    async fn refresh_lock(&self, account_id: i64) -> Arc<Mutex<()>> {
        self.refresh_locks
            .lock()
            .await
            .entry(account_id)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }
}

#[cfg(test)]
mod platform_marker_tests {
    use super::*;

    #[test]
    fn platform_google_marker_round_trips_scopes() {
        let marker = platform_google_marker(&["openid", "scope/a"]).unwrap();
        assert_eq!(
            platform_google_scopes(&marker).unwrap(),
            vec!["openid".to_owned(), "scope/a".to_owned()]
        );
    }
}

/// Bounded OAuth form POST. Gmail's REST provider already brings in reqwest;
/// using it here gives token exchange robust HTTP framing, connection pooling,
/// proxy support and a hard deadline instead of waiting forever on a silent
/// identity endpoint.
pub async fn post_form(url: &str, form: &[(String, String)]) -> Result<String> {
    const MAX_OAUTH_RESPONSE_BODY_BYTES: usize = 1024 * 1024;
    static HTTP: once_cell::sync::Lazy<std::result::Result<reqwest::Client, String>> =
        once_cell::sync::Lazy::new(|| {
            reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(15))
                .timeout(std::time::Duration::from_secs(30))
                .redirect(reqwest::redirect::Policy::none())
                .user_agent(concat!("Flectar-Mail/", env!("CARGO_PKG_VERSION")))
                .build()
                .map_err(|error| format!("OAuth HTTP client configuration failed: {error}"))
        });
    let http = HTTP
        .as_ref()
        .map_err(|message| CoreError::Network(message.clone()))?;
    let response = http.post(url).form(form).send().await.map_err(|error| {
        if error.is_timeout() || error.is_connect() {
            CoreError::Offline
        } else {
            CoreError::Network(format!("OAuth token request failed: {error}"))
        }
    })?;
    let status = response.status();
    let body = crate::http_body::text(
        response,
        MAX_OAUTH_RESPONSE_BODY_BYTES,
        "OAuth token response",
    )
    .await?;
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
        return Err(CoreError::Network(format!(
            "OAuth token service returned {status}"
        )));
    }
    if token_error_is_transient(&body) {
        return Err(CoreError::Network(
            "OAuth token service is temporarily unavailable".into(),
        ));
    }
    Ok(body)
}

fn token_error_requires_reauth(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    [
        "invalid_grant",
        "interaction_required",
        "consent_required",
        "aadsts65001",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn token_error_is_transient(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    lower.contains("temporarily_unavailable") || lower.contains("server_error")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    #[tokio::test]
    async fn initial_oauth_tokens_persist_in_development_container_store() {
        let directory = tempfile::tempdir().unwrap();
        let credentials: CredentialStoreHandle =
            Arc::new(credentials::DevelopmentFileCredentialStore::new(
                directory.path().join("credentials.json"),
            ));
        let provider = TokenProvider::new(
            credentials.clone(),
            Arc::new(crate::oauth::redirect::LoopbackRedirectBroker::default()),
        );

        provider
            .store_initial(
                17,
                "access-token".into(),
                Some(3600),
                Some("refresh-token".into()),
                "oauth-client".into(),
                None,
            )
            .await
            .unwrap();

        let encoded = credentials::load_async(credentials.clone(), 17, Slot::OAuthBundle)
            .await
            .unwrap();
        let bundle: OAuthCredentialBundle = serde_json::from_str(&encoded).unwrap();
        assert_eq!(bundle.version, OAUTH_CREDENTIAL_VERSION);
        assert_eq!(bundle.refresh_token, "refresh-token");
        assert_eq!(bundle.client_id, "oauth-client");
        assert!(bundle.client_secret.is_none());
        assert!(
            credentials::load_async(credentials.clone(), 17, Slot::RefreshToken)
                .await
                .is_err()
        );
        assert_eq!(
            provider.access_token(17, Provider::Gmail).await.unwrap(),
            "access-token"
        );
    }

    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    #[tokio::test]
    async fn legacy_oauth_entries_migrate_to_one_atomic_bundle() {
        let directory = tempfile::tempdir().unwrap();
        let credentials: CredentialStoreHandle =
            Arc::new(credentials::DevelopmentFileCredentialStore::new(
                directory.path().join("credentials.json"),
            ));
        credentials::store_async(
            credentials.clone(),
            23,
            Slot::RefreshToken,
            "legacy-refresh".into(),
        )
        .await
        .unwrap();
        credentials::store_async(
            credentials.clone(),
            23,
            Slot::OAuthClientId,
            "legacy-client".into(),
        )
        .await
        .unwrap();
        credentials::store_async(
            credentials.clone(),
            23,
            Slot::OAuthClientSecret,
            "legacy-secret".into(),
        )
        .await
        .unwrap();
        let provider = TokenProvider::new(
            credentials.clone(),
            Arc::new(crate::oauth::redirect::LoopbackRedirectBroker::default()),
        );

        let bundle = provider
            .oauth_credentials(23, Some(Provider::Gmail))
            .await
            .unwrap();
        assert_eq!(bundle.refresh_token, "legacy-refresh");
        assert_eq!(bundle.client_id, "legacy-client");
        assert_eq!(bundle.client_secret.as_deref(), Some("legacy-secret"));
        assert!(
            credentials::load_async(credentials.clone(), 23, Slot::OAuthBundle)
                .await
                .is_ok()
        );
        for slot in [
            Slot::RefreshToken,
            Slot::OAuthClientId,
            Slot::OAuthClientSecret,
        ] {
            assert!(
                credentials::load_async(credentials.clone(), 23, slot)
                    .await
                    .is_err()
            );
        }
    }

    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    #[tokio::test]
    async fn missing_refresh_token_never_creates_or_mixes_registrations() {
        let directory = tempfile::tempdir().unwrap();
        let credentials: CredentialStoreHandle =
            Arc::new(credentials::DevelopmentFileCredentialStore::new(
                directory.path().join("credentials.json"),
            ));
        let provider = TokenProvider::new(
            credentials.clone(),
            Arc::new(crate::oauth::redirect::LoopbackRedirectBroker::default()),
        );

        assert!(
            provider
                .store_initial(
                    31,
                    "access-one".into(),
                    Some(3600),
                    None,
                    "client-one".into(),
                    None,
                )
                .await
                .is_err()
        );
        assert!(
            credentials::load_async(credentials.clone(), 31, Slot::OAuthBundle)
                .await
                .is_err()
        );

        provider
            .store_initial(
                31,
                "access-two".into(),
                Some(3600),
                Some("refresh-one".into()),
                "client-one".into(),
                None,
            )
            .await
            .unwrap();
        provider
            .store_initial(
                31,
                "access-three".into(),
                Some(3600),
                None,
                "client-one".into(),
                None,
            )
            .await
            .unwrap();
        assert!(
            provider
                .store_initial(
                    31,
                    "access-four".into(),
                    Some(3600),
                    None,
                    "different-client".into(),
                    None,
                )
                .await
                .is_err()
        );
        let bundle = provider
            .oauth_credentials(31, Some(Provider::Gmail))
            .await
            .unwrap();
        assert_eq!(bundle.refresh_token, "refresh-one");
        assert_eq!(bundle.client_id, "client-one");
    }

    #[test]
    fn provider_tokens_and_lifetimes_are_hard_bounded() {
        assert!(validate_token_fields("access", Some(3600), Some("refresh")).is_ok());
        assert!(validate_token_fields("", Some(3600), None).is_err());
        assert!(validate_token_fields("access\nheader", Some(3600), None).is_err());
        assert!(validate_token_fields("access", Some(0), None).is_err());
        assert!(
            validate_token_fields("access", Some(MAX_TOKEN_LIFETIME_SECONDS + 1), None).is_err()
        );
        assert!(
            validate_token_fields(&"x".repeat(MAX_OAUTH_TOKEN_BYTES + 1), Some(3600), None)
                .is_err()
        );
    }

    #[test]
    fn microsoft_consent_and_rotated_grant_errors_require_reauth() {
        assert!(token_error_requires_reauth(
            r#"{"error":"invalid_grant","error_description":"AADSTS700082"}"#
        ));
        assert!(token_error_requires_reauth(
            r#"{"error":"interaction_required","error_description":"AADSTS65001"}"#
        ));
        assert!(!token_error_requires_reauth(
            r#"{"error":"invalid_client"}"#
        ));
    }

    #[test]
    fn provider_outages_are_retryable() {
        assert!(token_error_is_transient(
            r#"{"error":"temporarily_unavailable"}"#
        ));
        assert!(token_error_is_transient(r#"{"error":"server_error"}"#));
        assert!(!token_error_is_transient(r#"{"error":"invalid_client"}"#));
    }
}
