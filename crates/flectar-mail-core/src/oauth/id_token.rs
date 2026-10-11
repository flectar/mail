//! Local OpenID Connect ID-token verification for desktop authorization-code
//! exchanges. Claims are not trusted until the provider signature, issuer,
//! audience, lifetime, and one-time nonce have all been checked.

use crate::error::{CoreError, Result};
use crate::models::Provider;
use base64::Engine as _;
use once_cell::sync::Lazy;
use ring::signature;
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

const GOOGLE_JWKS: &str = "https://www.googleapis.com/oauth2/v3/certs";
const MICROSOFT_JWKS: &str = "https://login.microsoftonline.com/common/discovery/v2.0/keys";
const MAX_ID_TOKEN_BYTES: usize = 64 * 1024;
const MAX_JWKS_BODY_BYTES: usize = 512 * 1024;
const MAX_JWKS_KEYS: usize = 128;
const DEFAULT_KEY_TTL: Duration = Duration::from_secs(60 * 60);
const MAX_KEY_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const CLOCK_SKEW_SECONDS: i64 = 60;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct VerifiedIdentity {
    pub email: String,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
}

#[derive(Clone, Deserialize)]
struct Jwk {
    kid: String,
    kty: String,
    n: String,
    e: String,
    #[serde(default)]
    alg: Option<String>,
    #[serde(default, rename = "use")]
    usage: Option<String>,
    #[serde(default)]
    issuer: Option<String>,
}

#[derive(Deserialize)]
struct JwkSet {
    keys: Vec<Jwk>,
}

struct CachedKeys {
    expires_at: Instant,
    keys: Arc<[Jwk]>,
}

static GOOGLE_KEY_CACHE: Lazy<Mutex<Option<CachedKeys>>> = Lazy::new(|| Mutex::new(None));
static MICROSOFT_KEY_CACHE: Lazy<Mutex<Option<CachedKeys>>> = Lazy::new(|| Mutex::new(None));

#[derive(Deserialize)]
struct JwtHeader {
    alg: String,
    kid: String,
    #[serde(default)]
    typ: Option<String>,
}

struct EncodedToken<'a> {
    header: JwtHeader,
    claims: Value,
    signed: &'a str,
    signature: &'a str,
}

pub(super) async fn verify(
    provider: Provider,
    token: &str,
    client_id: &str,
    expected_nonce: &str,
) -> Result<VerifiedIdentity> {
    let encoded = parse(token)?;
    validate_header(&encoded.header)?;
    let now = chrono::Utc::now().timestamp();
    validate_claims(provider, &encoded.claims, client_id, expected_nonce, now)?;
    verify_signature(
        provider,
        &encoded.header.kid,
        issuer(&encoded.claims)?,
        encoded.signed.as_bytes(),
        encoded.signature,
    )
    .await?;
    identity(provider, &encoded.claims)
}

fn parse(token: &str) -> Result<EncodedToken<'_>> {
    if token.is_empty() || token.len() > MAX_ID_TOKEN_BYTES {
        return Err(CoreError::Auth("identity token has an invalid size".into()));
    }
    let (signed, encoded_signature) = token
        .rsplit_once('.')
        .ok_or_else(|| CoreError::Auth("identity token is malformed".into()))?;
    let (encoded_header, encoded_claims) = signed
        .split_once('.')
        .ok_or_else(|| CoreError::Auth("identity token is malformed".into()))?;
    if encoded_header.is_empty()
        || encoded_header.len() > 8 * 1024
        || encoded_claims.is_empty()
        || encoded_claims.contains('.')
        || encoded_signature.is_empty()
        || encoded_signature.len() > 16 * 1024
    {
        return Err(CoreError::Auth("identity token is malformed".into()));
    }
    let header = decode_json(encoded_header, "header")?;
    let claims = decode_json(encoded_claims, "claims")?;
    Ok(EncodedToken {
        header,
        claims,
        signed,
        signature: encoded_signature,
    })
}

fn decode_json<T: serde::de::DeserializeOwned>(encoded: &str, part: &str) -> Result<T> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| CoreError::Auth(format!("identity token {part} is malformed")))?;
    serde_json::from_slice(&bytes)
        .map_err(|_| CoreError::Auth(format!("identity token {part} is malformed")))
}

fn validate_header(header: &JwtHeader) -> Result<()> {
    if header.alg != "RS256"
        || header.kid.is_empty()
        || header.kid.len() > 256
        || header
            .typ
            .as_deref()
            .is_some_and(|value| !value.eq_ignore_ascii_case("JWT"))
    {
        return Err(CoreError::Auth(
            "identity token uses an unsupported signature".into(),
        ));
    }
    Ok(())
}

fn validate_claims(
    provider: Provider,
    claims: &Value,
    client_id: &str,
    expected_nonce: &str,
    now: i64,
) -> Result<()> {
    let audience = claims.get("aud");
    if client_id.is_empty() || !audience_contains(audience, client_id) {
        return Err(CoreError::Auth("identity token audience mismatch".into()));
    }
    let presenter = claims.get("azp").and_then(Value::as_str);
    if presenter.is_some_and(|presenter| presenter != client_id)
        || (audience
            .and_then(Value::as_array)
            .is_some_and(|values| values.len() > 1)
            && presenter != Some(client_id))
    {
        return Err(CoreError::Auth(
            "identity token authorized-party mismatch".into(),
        ));
    }
    if claims.get("nonce").and_then(Value::as_str) != Some(expected_nonce) {
        return Err(CoreError::Auth("identity token nonce mismatch".into()));
    }

    let expires_at = claims
        .get("exp")
        .and_then(Value::as_i64)
        .ok_or_else(|| CoreError::Auth("identity token has no expiry".into()))?;
    if expires_at.saturating_add(CLOCK_SKEW_SECONDS) < now {
        return Err(CoreError::Auth("identity token has expired".into()));
    }
    let subject = claims.get("sub").and_then(Value::as_str);
    if subject.is_none_or(|subject| subject.is_empty() || subject.len() > 255) {
        return Err(CoreError::Auth(
            "identity token has no valid subject".into(),
        ));
    }
    let issued_at = claims
        .get("iat")
        .and_then(Value::as_i64)
        .ok_or_else(|| CoreError::Auth("identity token has no issue time".into()))?;
    if claims
        .get("nbf")
        .and_then(Value::as_i64)
        .is_some_and(|not_before| not_before > now.saturating_add(CLOCK_SKEW_SECONDS))
        || issued_at > now.saturating_add(CLOCK_SKEW_SECONDS)
    {
        return Err(CoreError::Auth(
            "identity token is not valid at the current time".into(),
        ));
    }

    let issuer = issuer(claims)?;
    match provider {
        Provider::Gmail
            if issuer != "https://accounts.google.com" && issuer != "accounts.google.com" =>
        {
            Err(CoreError::Auth("identity token issuer mismatch".into()))
        }
        Provider::Microsoft if !valid_microsoft_issuer(issuer, claims) => {
            Err(CoreError::Auth("identity token issuer mismatch".into()))
        }
        Provider::Imap => Err(CoreError::Auth(
            "password accounts do not use identity tokens".into(),
        )),
        _ => Ok(()),
    }
}

fn issuer(claims: &Value) -> Result<&str> {
    claims
        .get("iss")
        .and_then(Value::as_str)
        .ok_or_else(|| CoreError::Auth("identity token has no issuer".into()))
}

fn audience_contains(audience: Option<&Value>, client_id: &str) -> bool {
    match audience {
        Some(Value::String(value)) => value == client_id,
        Some(Value::Array(values)) if !values.is_empty() && values.len() <= 16 => {
            values.iter().all(Value::is_string)
                && values.iter().any(|value| value.as_str() == Some(client_id))
        }
        _ => false,
    }
}

fn valid_microsoft_issuer(value: &str, claims: &Value) -> bool {
    let Some(tenant) = claims.get("tid").and_then(Value::as_str) else {
        return false;
    };
    if !valid_tenant_id(tenant) {
        return false;
    }
    let Ok(url) = url::Url::parse(value) else {
        return false;
    };
    let Some(mut segments) = url.path_segments() else {
        return false;
    };
    url.scheme() == "https"
        && url.host_str() == Some("login.microsoftonline.com")
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && segments.next() == Some(tenant)
        && segments.next() == Some("v2.0")
        && segments.next().is_none()
}

fn valid_tenant_id(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

fn identity(provider: Provider, claims: &Value) -> Result<VerifiedIdentity> {
    if provider == Provider::Gmail
        && claims.get("email_verified").and_then(Value::as_bool) != Some(true)
    {
        return Err(CoreError::Auth(
            "Google did not verify the mailbox email address".into(),
        ));
    }
    let claim_names: &[&str] = match provider {
        Provider::Gmail => &["email"],
        Provider::Microsoft => &["preferred_username", "email", "upn"],
        Provider::Imap => &[],
    };
    let email = claim_names
        .iter()
        .find_map(|name| claims[*name].as_str().and_then(valid_mailbox))
        .ok_or_else(|| {
            CoreError::Auth(format!(
                "{} did not return the mailbox email address",
                provider.as_str()
            ))
        })?;
    let display_name = claims
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty() && name.chars().count() <= 512)
        .map(str::to_owned);
    let avatar_url = (provider == Provider::Gmail)
        .then(|| claims.get("picture").and_then(Value::as_str))
        .flatten()
        .filter(|value| value.len() <= 4 * 1024)
        .map(str::to_owned);
    Ok(VerifiedIdentity {
        email,
        display_name,
        avatar_url,
    })
}

pub(super) fn valid_mailbox(value: &str) -> Option<String> {
    let value = value.trim();
    let (local, domain) = value.split_once('@')?;
    (value.len() <= 320
        && !local.is_empty()
        && local.len() <= 64
        && !domain.is_empty()
        && domain.len() <= 255
        && !domain.contains('@')
        && !value.chars().any(char::is_whitespace)
        && !value.chars().any(char::is_control))
    .then(|| value.to_owned())
}

async fn verify_signature(
    provider: Provider,
    kid: &str,
    token_issuer: &str,
    signed: &[u8],
    encoded_signature: &str,
) -> Result<()> {
    let signature_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded_signature)
        .map_err(|_| CoreError::Auth("identity token signature is malformed".into()))?;
    if signature_bytes.len() > 1024 {
        return Err(CoreError::Auth(
            "identity token signature is malformed".into(),
        ));
    }
    let mut keys = signing_keys(provider, false).await?;
    let mut key = matching_key(provider, &keys, kid, token_issuer);
    if key.is_none() {
        keys = signing_keys(provider, true).await?;
        key = matching_key(provider, &keys, kid, token_issuer);
    }
    let key = key.ok_or_else(|| CoreError::Auth("identity token signing key is unknown".into()))?;
    let modulus = decode_key_part(&key.n, 1024)?;
    let exponent = decode_key_part(&key.e, 8)?;
    signature::RsaPublicKeyComponents {
        n: &modulus,
        e: &exponent,
    }
    .verify(
        &signature::RSA_PKCS1_2048_8192_SHA256,
        signed,
        &signature_bytes,
    )
    .map_err(|_| CoreError::Auth("identity token signature is invalid".into()))
}

fn matching_key<'a>(
    provider: Provider,
    keys: &'a [Jwk],
    kid: &str,
    token_issuer: &str,
) -> Option<&'a Jwk> {
    keys.iter().find(|key| {
        key.kid == kid
            && key.kty == "RSA"
            && key.alg.as_deref().is_none_or(|value| value == "RS256")
            && key.usage.as_deref().is_none_or(|value| value == "sig")
            && key_issuer_matches(provider, key.issuer.as_deref(), token_issuer)
    })
}

fn key_issuer_matches(provider: Provider, key_issuer: Option<&str>, token_issuer: &str) -> bool {
    if provider != Provider::Microsoft {
        return true;
    }
    key_issuer.is_none_or(|issuer| {
        issuer == token_issuer
            || (microsoft_tenant_from_issuer(token_issuer).is_some()
                && issuer == "https://login.microsoftonline.com/{tenantid}/v2.0")
    })
}

fn microsoft_tenant_from_issuer(issuer: &str) -> Option<&str> {
    issuer
        .strip_prefix("https://login.microsoftonline.com/")?
        .strip_suffix("/v2.0")
        .filter(|tenant| valid_tenant_id(tenant))
}

fn decode_key_part(encoded: &str, max_bytes: usize) -> Result<Vec<u8>> {
    if encoded.is_empty() || encoded.len() > max_bytes.saturating_mul(2) {
        return Err(CoreError::Auth(
            "identity provider returned an invalid signing key".into(),
        ));
    }
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| CoreError::Auth("identity provider returned an invalid signing key".into()))?;
    if bytes.is_empty() || bytes.len() > max_bytes {
        return Err(CoreError::Auth(
            "identity provider returned an invalid signing key".into(),
        ));
    }
    Ok(bytes)
}

async fn signing_keys(provider: Provider, force_refresh: bool) -> Result<Arc<[Jwk]>> {
    let cache = match provider {
        Provider::Gmail => &*GOOGLE_KEY_CACHE,
        Provider::Microsoft => &*MICROSOFT_KEY_CACHE,
        Provider::Imap => {
            return Err(CoreError::Auth(
                "password accounts do not use identity tokens".into(),
            ));
        }
    };
    let mut cache = cache.lock().await;
    if !force_refresh
        && let Some(cached) = cache.as_ref()
        && cached.expires_at > Instant::now()
    {
        return Ok(cached.keys.clone());
    }

    let url = match provider {
        Provider::Gmail => GOOGLE_JWKS,
        Provider::Microsoft => MICROSOFT_JWKS,
        Provider::Imap => unreachable!(),
    };
    let response = oidc_http_client()?.get(url).send().await.map_err(|error| {
        if error.is_timeout() || error.is_connect() {
            CoreError::Offline
        } else {
            CoreError::Network(format!("identity signing-key request failed: {error}"))
        }
    })?;
    if !response.status().is_success() {
        return Err(CoreError::Network(format!(
            "identity signing-key service returned {}",
            response.status()
        )));
    }
    let ttl = cache_ttl(response.headers());
    let body = crate::http_body::bytes(
        response,
        MAX_JWKS_BODY_BYTES,
        "identity signing-key response",
    )
    .await?;
    let set: JwkSet = serde_json::from_slice(&body)
        .map_err(|_| CoreError::Auth("identity provider returned invalid signing keys".into()))?;
    if set.keys.is_empty()
        || set.keys.len() > MAX_JWKS_KEYS
        || set.keys.iter().any(|key| {
            key.kid.is_empty() || key.kid.len() > 256 || key.n.len() > 16 * 1024 || key.e.len() > 64
        })
    {
        return Err(CoreError::Auth(
            "identity provider returned invalid signing keys".into(),
        ));
    }
    let keys: Arc<[Jwk]> = set.keys.into();
    *cache = Some(CachedKeys {
        expires_at: Instant::now() + ttl,
        keys: keys.clone(),
    });
    Ok(keys)
}

fn cache_ttl(headers: &reqwest::header::HeaderMap) -> Duration {
    headers
        .get(reqwest::header::CACHE_CONTROL)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            value.split(',').find_map(|directive| {
                directive
                    .trim()
                    .strip_prefix("max-age=")
                    .and_then(|seconds| seconds.parse::<u64>().ok())
            })
        })
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_KEY_TTL)
        .min(MAX_KEY_TTL)
}

fn oidc_http_client() -> Result<&'static reqwest::Client> {
    static HTTP: Lazy<std::result::Result<reqwest::Client, String>> = Lazy::new(|| {
        crate::http_tls::client_builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("Flectar-Mail/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| format!("identity HTTP client configuration failed: {error}"))
    });
    HTTP.as_ref()
        .map_err(|message| CoreError::Network(message.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn google_claims(now: i64) -> Value {
        serde_json::json!({
            "iss": "https://accounts.google.com",
            "aud": "desktop-client",
            "exp": now + 3600,
            "iat": now,
            "nonce": "one-time",
            "sub": "google-subject",
            "email": "person@gmail.com",
            "email_verified": true,
            "name": "Ada Lovelace",
            "picture": "https://lh3.googleusercontent.com/a/profile"
        })
    }

    #[test]
    fn claims_require_issuer_audience_lifetime_and_nonce() {
        let now = 1_800_000_000;
        let claims = google_claims(now);
        validate_claims(Provider::Gmail, &claims, "desktop-client", "one-time", now).unwrap();
        assert!(
            validate_claims(Provider::Gmail, &claims, "other-client", "one-time", now).is_err()
        );
        assert!(
            validate_claims(Provider::Gmail, &claims, "desktop-client", "replayed", now).is_err()
        );
        assert!(
            validate_claims(
                Provider::Gmail,
                &claims,
                "desktop-client",
                "one-time",
                now + 4000
            )
            .is_err()
        );
    }

    #[test]
    fn microsoft_issuer_must_match_the_tenant() {
        let tenant = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
        let claims = serde_json::json!({
            "iss": format!("https://login.microsoftonline.com/{tenant}/v2.0"),
            "tid": tenant,
            "aud": "desktop-client",
            "exp": 1_900_000_000_i64,
            "iat": 1_800_000_000_i64,
            "nonce": "one-time",
            "sub": "microsoft-subject"
        });
        assert!(
            validate_claims(
                Provider::Microsoft,
                &claims,
                "desktop-client",
                "one-time",
                1_800_000_000
            )
            .is_ok()
        );
        let mut mismatched = claims;
        mismatched["tid"] = Value::String("ffffffff-bbbb-4ccc-8ddd-eeeeeeeeeeee".into());
        assert!(
            validate_claims(
                Provider::Microsoft,
                &mismatched,
                "desktop-client",
                "one-time",
                1_800_000_000
            )
            .is_err()
        );
    }

    #[test]
    fn verified_identity_requires_a_real_verified_mailbox() {
        let claims = google_claims(1_800_000_000);
        assert_eq!(
            identity(Provider::Gmail, &claims).unwrap(),
            VerifiedIdentity {
                email: "person@gmail.com".into(),
                display_name: Some("Ada Lovelace".into()),
                avatar_url: Some("https://lh3.googleusercontent.com/a/profile".into()),
            }
        );
        let mut unverified = claims;
        unverified["email_verified"] = Value::Bool(false);
        assert!(identity(Provider::Gmail, &unverified).is_err());

        let microsoft = serde_json::json!({
            "preferred_username": "+1 555 0100",
            "email": "person@outlook.com"
        });
        assert_eq!(
            identity(Provider::Microsoft, &microsoft).unwrap().email,
            "person@outlook.com"
        );
    }

    #[test]
    fn unsigned_and_none_algorithm_tokens_are_rejected() {
        let header = JwtHeader {
            alg: "none".into(),
            kid: "key".into(),
            typ: Some("JWT".into()),
        };
        assert!(validate_header(&header).is_err());
        assert!(parse("header.claims").is_err());
    }
}
