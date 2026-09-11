//! Authorization-code + PKCE flow over the localhost loopback redirect.
//! The browser is opened by the host via the `open_url` callback so this crate
//! remains independent of the UI toolkit and platform launcher.

use crate::error::{CoreError, Result};
use crate::models::Provider;
use crate::oauth::providers::for_provider;
use crate::oauth::redirect::{
    LoopbackRedirectBroker, OAUTH_REDIRECT_TIMEOUT, OAuthRedirectBrokerHandle,
};
use crate::oauth::tokens::post_form;
use base64::Engine;
use sha2::Digest;

pub struct OAuthOutcome {
    pub email: String,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
    pub access_token: String,
    pub expires_in: Option<i64>,
    pub refresh_token: Option<String>,
    pub client_id: String,
    pub client_secret: Option<String>,
}

fn random_string(len: usize) -> String {
    use rand::Rng;
    let mut rng = rand::rng();
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";
    (0..len)
        .map(|_| CHARS[rng.random_range(0..CHARS.len())] as char)
        .collect()
}

#[derive(serde::Deserialize)]
struct ExchangeResponse {
    access_token: String,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
struct OAuthIdentity {
    email: String,
    display_name: Option<String>,
    avatar_url: Option<String>,
}

fn valid_google_avatar_url(value: &str) -> Option<String> {
    let url = url::Url::parse(value).ok()?;
    let host = url.host_str()?.to_ascii_lowercase();
    (url.scheme() == "https"
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
        && (host == "googleusercontent.com" || host.ends_with(".googleusercontent.com")))
    .then(|| url.into())
}

pub async fn authorize(
    provider: Provider,
    open_url: impl FnOnce(String) -> std::result::Result<(), String> + Send,
) -> Result<OAuthOutcome> {
    authorize_with(provider, &[], None, open_url).await
}

/// Like `authorize` but with additional scopes (incremental consent, e.g.
/// Google Calendar) and a login hint so re-consent lands on the right account.
pub async fn authorize_with(
    provider: Provider,
    extra_scopes: &[&str],
    login_hint: Option<&str>,
    open_url: impl FnOnce(String) -> std::result::Result<(), String> + Send,
) -> Result<OAuthOutcome> {
    authorize_with_broker(
        provider,
        extra_scopes,
        login_hint,
        std::sync::Arc::new(LoopbackRedirectBroker::default()),
        open_url,
    )
    .await
}

pub async fn authorize_with_broker(
    provider: Provider,
    extra_scopes: &[&str],
    login_hint: Option<&str>,
    redirects: OAuthRedirectBrokerHandle,
    open_url: impl FnOnce(String) -> std::result::Result<(), String> + Send,
) -> Result<OAuthOutcome> {
    let cfg = for_provider(provider)
        .ok_or_else(|| CoreError::Auth("provider does not use oauth".into()))?;
    let (client_id, client_secret) = crate::oauth::providers::resolve_credentials(provider)?;

    let mut scopes = cfg.scopes.to_vec();
    for extra in extra_scopes {
        if !scopes.contains(extra) {
            scopes.push(extra);
        }
    }

    // Google does not support custom-scheme or loopback browser redirects for
    // Android OAuth clients. The Android broker uses AuthorizationClient and
    // returns a Play-Services-managed access token instead.
    if let Some(result) = redirects.authorize_platform(provider, &scopes, true).await {
        let token = result?;
        let identity = google_identity_from_access_token(&token.access_token).await?;
        return Ok(OAuthOutcome {
            email: identity.email,
            display_name: identity.display_name,
            avatar_url: identity.avatar_url,
            access_token: token.access_token,
            expires_in: token.expires_in,
            refresh_token: Some(
                crate::oauth::tokens::platform_google_marker(&scopes)
                    .map_err(|error| CoreError::Auth(error.to_string()))?,
            ),
            client_id,
            client_secret: None,
        });
    }

    let verifier = random_string(64);
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(sha2::Sha256::digest(verifier.as_bytes()));
    let state = random_string(32);
    let nonce = random_string(32);

    // Begin listening before the browser opens. Mobile brokers can receive a
    // redirect almost immediately when an existing provider session skips UI.
    let redirect_session = redirects.begin(provider, &state).await?;
    let redirect_uri = redirect_session.redirect_uri().to_owned();

    let scopes = scopes.join(" ");
    let mut auth_url = format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&scope={}&state={}&nonce={}&code_challenge={}&code_challenge_method=S256",
        cfg.auth_url,
        urlencode(&client_id),
        urlencode(&redirect_uri),
        urlencode(&scopes),
        urlencode(&state),
        urlencode(&nonce),
        urlencode(&challenge),
    );
    if provider == Provider::Gmail {
        auth_url.push_str("&access_type=offline&prompt=consent");
    }
    if let Some(hint) = login_hint {
        auth_url.push_str(&format!("&login_hint={}", urlencode(hint)));
    } else if provider == Provider::Microsoft {
        // Without a hint, an active Microsoft SSO session signs in silently as
        // whoever it belongs to - the user never sees a login page. Force the
        // account picker so signing in is an explicit, visible choice.
        auth_url.push_str("&prompt=select_account");
    }

    tracing::info!(
        ?provider,
        %redirect_uri,
        scopes = %scopes,
        "oauth: opening browser for sign-in"
    );
    open_url(auth_url)
        .map_err(|error| CoreError::Auth(format!("could not open sign-in browser: {error}")))?;

    let code = redirect_session
        .wait(OAUTH_REDIRECT_TIMEOUT)
        .await
        .inspect_err(|e| tracing::warn!(?provider, error = %e, "oauth: no usable callback"))?;
    tracing::info!(?provider, "oauth: authorization code received");
    if code.state.as_deref() != Some(state.as_str()) {
        tracing::warn!(?provider, "oauth: state mismatch on callback");
        return Err(CoreError::Auth("oauth state mismatch".into()));
    }

    let mut form = vec![
        ("grant_type".to_string(), "authorization_code".to_string()),
        ("code".to_string(), code.code),
        ("redirect_uri".to_string(), redirect_uri),
        ("client_id".to_string(), client_id.clone()),
        ("code_verifier".to_string(), verifier),
    ];
    if let Some(cs) = client_secret.as_ref() {
        form.push(("client_secret".to_string(), cs.clone()));
    }
    // Microsoft issues single-resource access tokens. When the consent spans
    // more than one resource (e.g. the Graph Teams scope added at sign-in
    // alongside the outlook.office.com mail scopes), redeem the code
    // explicitly for the MAIL resource so the returned token's audience is
    // unambiguous. The refresh token stays multi-resource, so the extra
    // consented scopes are redeemed on demand later (see
    // `TokenProvider::access_token_for_scope`). Google has no such constraint
    // and is left to return a token covering every granted scope.
    if provider == Provider::Microsoft && !extra_scopes.is_empty() {
        form.push(("scope".to_string(), cfg.scopes.join(" ")));
    }

    let body = post_form(cfg.token_url, &form).await.inspect_err(
        |e| tracing::warn!(?provider, error = %e, "oauth: token endpoint unreachable"),
    )?;
    let tok: ExchangeResponse = serde_json::from_str(&body).map_err(|_| {
        let detail = crate::http_body::single_line_excerpt(&body, 512);
        tracing::warn!(?provider, response = %detail, "oauth: token exchange rejected");
        CoreError::Auth(format!("token exchange failed: {detail}"))
    })?;
    super::tokens::validate_token_fields(
        &tok.access_token,
        tok.expires_in,
        tok.refresh_token.as_deref(),
    )?;
    tracing::info!(
        ?provider,
        has_refresh_token = tok.refresh_token.is_some(),
        expires_in = ?tok.expires_in,
        "oauth: token exchange succeeded"
    );

    let id_token = tok.id_token.as_deref().ok_or_else(|| {
        CoreError::Auth(format!(
            "{} did not return an identity token",
            provider.as_str()
        ))
    })?;
    let verified = super::id_token::verify(provider, id_token, &client_id, &nonce).await?;
    // Microsoft profile photos require a separate Graph token and are
    // deliberately left to the initials fallback. Google picture claims are
    // provider-scoped again before they reach the UI image loader.
    let identity = OAuthIdentity {
        email: verified.email,
        display_name: verified.display_name,
        avatar_url: verified
            .avatar_url
            .and_then(|url| valid_google_avatar_url(&url)),
    };

    if tok.refresh_token.is_none() {
        let message = match provider {
            Provider::Gmail => {
                "Google did not return an offline refresh token; revoke Flectar Mail access and reconnect"
            }
            Provider::Microsoft => {
                "Microsoft did not return an offline refresh token; reconnect and grant access"
            }
            Provider::Imap => unreachable!(),
        };
        return Err(CoreError::Auth(message.into()));
    }

    Ok(OAuthOutcome {
        email: identity.email,
        display_name: identity.display_name,
        avatar_url: identity.avatar_url,
        access_token: tok.access_token,
        expires_in: tok.expires_in,
        refresh_token: tok.refresh_token,
        client_id,
        client_secret,
    })
}

async fn google_identity_from_access_token(access_token: &str) -> Result<OAuthIdentity> {
    const MAX_USERINFO_BODY_BYTES: usize = 1024 * 1024;
    #[derive(serde::Deserialize)]
    struct UserInfo {
        email: String,
        email_verified: bool,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        picture: Option<String>,
    }

    static HTTP: once_cell::sync::Lazy<std::result::Result<reqwest::Client, String>> =
        once_cell::sync::Lazy::new(|| {
            reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(10))
                .timeout(std::time::Duration::from_secs(30))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|error| format!("Google identity HTTP client: {error}"))
        });
    let http = HTTP
        .as_ref()
        .map_err(|message| CoreError::Network(message.clone()))?;
    let response = http
        .get("https://openidconnect.googleapis.com/v1/userinfo")
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|error| CoreError::Network(format!("Google identity request failed: {error}")))?;
    if !response.status().is_success() {
        return Err(CoreError::Auth(format!(
            "Google identity request was rejected (HTTP {})",
            response.status()
        )));
    }
    let body = crate::http_body::bytes(
        response,
        MAX_USERINFO_BODY_BYTES,
        "Google identity response",
    )
    .await?;
    let info: UserInfo = serde_json::from_slice(&body).map_err(|error| {
        CoreError::Auth(format!("Google identity response was invalid: {error}"))
    })?;
    if !info.email_verified {
        return Err(CoreError::Auth(
            "Google did not verify the mailbox email address".into(),
        ));
    }
    let email = super::id_token::valid_mailbox(&info.email)
        .ok_or_else(|| CoreError::Auth("Google did not return the mailbox email address".into()))?;
    Ok(OAuthIdentity {
        email,
        display_name: info
            .name
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty() && name.chars().count() <= 512),
        avatar_url: info
            .picture
            .filter(|url| url.len() <= 4 * 1024)
            .and_then(|url| valid_google_avatar_url(&url)),
    })
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn google_identity_rejects_untrusted_picture_hosts() {
        assert!(valid_google_avatar_url("https://example.com/tracker.png").is_none());
        assert!(valid_google_avatar_url("https://lh3.googleusercontent.com/a/profile").is_some());
        assert!(
            valid_google_avatar_url("https://user:secret@lh3.googleusercontent.com/a").is_none()
        );
    }
}
