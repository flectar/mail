use crate::{
    error::{CoreError, Result},
    models::Provider,
    oauth::loopback::{AuthCode, LoopbackServer},
};
use async_trait::async_trait;
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

/// Give people enough time to finish account selection, consent, and MFA.
pub const OAUTH_REDIRECT_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// Access token returned by a platform authorization service such as Google
/// Play Services. Native mobile authorization does not expose a reusable
/// refresh token to the application; the platform must be asked for a fresh
/// access token when this one expires.
pub struct PlatformAuthorization {
    pub access_token: String,
    pub expires_in: Option<i64>,
}

#[async_trait]
pub trait OAuthRedirectSession: Send {
    fn redirect_uri(&self) -> &str;
    async fn wait(self: Box<Self>, timeout: Duration) -> Result<AuthCode>;
}

#[async_trait]
pub trait OAuthRedirectBroker: Send + Sync {
    async fn begin(
        &self,
        provider: Provider,
        expected_state: &str,
    ) -> Result<Box<dyn OAuthRedirectSession>>;

    /// Use a platform-native authorization API when one exists. `None` means
    /// this broker only supports browser redirects. A non-interactive request
    /// must return `NeedsReauth` rather than opening account or consent UI.
    async fn authorize_platform(
        &self,
        _provider: Provider,
        _scopes: &[&str],
        _interactive: bool,
    ) -> Option<Result<PlatformAuthorization>> {
        None
    }

    /// Forward a browser callback that could not reach the local listener.
    ///
    /// This is useful when the browser and application run in different
    /// network namespaces, as can happen with containers and remote desktops.
    fn submit_redirect(&self, _uri: &str) -> Result<()> {
        Err(CoreError::Auth(
            "pasted browser callbacks are not supported on this platform".into(),
        ))
    }
}

pub type OAuthRedirectBrokerHandle = Arc<dyn OAuthRedirectBroker>;

/// Single-use, expiring verifier shared by redirect brokers. A rejected
/// callback does not consume the transaction, but the first valid callback
/// does, so browser retries cannot redeem the same request twice.
pub struct OAuthRedirectGuard {
    expected_state: String,
    expires_at: Instant,
    consumed: bool,
}

impl OAuthRedirectGuard {
    pub fn new(expected_state: impl Into<String>, lifetime: Duration) -> Self {
        Self {
            expected_state: expected_state.into(),
            expires_at: Instant::now() + lifetime,
            consumed: false,
        }
    }

    pub fn accept(&mut self, uri: &str) -> Result<AuthCode> {
        if self.consumed {
            return Err(CoreError::Auth(
                "OAuth callback is expired or already used".into(),
            ));
        }
        if Instant::now() >= self.expires_at {
            return Err(CoreError::Auth("OAuth callback has expired".into()));
        }
        let response = parse_redirect_uri(uri)?;
        if response.state.as_deref() != Some(self.expected_state.as_str()) {
            return Err(CoreError::Auth("oauth state mismatch".into()));
        }
        self.consumed = true;
        Ok(response)
    }
}

pub fn parse_redirect_uri(uri: &str) -> Result<AuthCode> {
    let uri = url::Url::parse(uri)
        .map_err(|_| CoreError::Auth("OAuth redirect URI is invalid".into()))?;
    let mut code = None;
    let mut state = None;
    let mut provider_error = None;
    for (key, value) in uri.query_pairs() {
        match key.as_ref() {
            "code" => code = Some(value.into_owned()),
            "state" => state = Some(value.into_owned()),
            "error" => provider_error = Some(value.into_owned()),
            _ => {}
        }
    }
    if let Some(error) = provider_error {
        return Err(CoreError::Auth(format!("oauth error: {error}")));
    }
    Ok(AuthCode {
        code: code.ok_or_else(|| CoreError::Auth("OAuth redirect has no code".into()))?,
        state,
    })
}

#[derive(Default)]
pub struct LoopbackRedirectBroker {
    pending: Arc<Mutex<Option<PendingRedirect>>>,
}

struct PendingRedirect {
    redirect_uri: String,
    expected_state: String,
    guard: OAuthRedirectGuard,
    sender: Option<tokio::sync::oneshot::Sender<AuthCode>>,
}

struct LoopbackRedirectSession {
    redirect_uri: String,
    server: LoopbackServer,
    manual_redirect: tokio::sync::oneshot::Receiver<AuthCode>,
    expected_state: String,
    pending: Arc<Mutex<Option<PendingRedirect>>>,
}

#[async_trait]
impl OAuthRedirectBroker for LoopbackRedirectBroker {
    async fn begin(
        &self,
        provider: Provider,
        expected_state: &str,
    ) -> Result<Box<dyn OAuthRedirectSession>> {
        let server = LoopbackServer::bind().await?;
        let redirect_uri = match provider {
            Provider::Gmail => server.ipv4_redirect_uri(),
            Provider::Microsoft => server.localhost_redirect_uri(),
            Provider::Imap => return Err(CoreError::Auth("provider does not use oauth".into())),
        };
        let (sender, manual_redirect) = tokio::sync::oneshot::channel();
        let mut pending = self
            .pending
            .lock()
            .map_err(|_| CoreError::Auth("OAuth callback state is unavailable".into()))?;
        if pending.is_some() {
            return Err(CoreError::Auth(
                "another browser authorization is already in progress".into(),
            ));
        }
        *pending = Some(PendingRedirect {
            redirect_uri: redirect_uri.clone(),
            expected_state: expected_state.to_owned(),
            guard: OAuthRedirectGuard::new(expected_state, OAUTH_REDIRECT_TIMEOUT),
            sender: Some(sender),
        });
        Ok(Box::new(LoopbackRedirectSession {
            redirect_uri,
            server,
            manual_redirect,
            expected_state: expected_state.to_owned(),
            pending: self.pending.clone(),
        }))
    }

    fn submit_redirect(&self, uri: &str) -> Result<()> {
        let mut pending = self
            .pending
            .lock()
            .map_err(|_| CoreError::Auth("OAuth callback state is unavailable".into()))?;
        let pending = pending
            .as_mut()
            .ok_or_else(|| CoreError::Auth("no browser authorization is in progress".into()))?;
        if !same_redirect_endpoint(uri, &pending.redirect_uri) {
            return Err(CoreError::Auth(
                "paste the complete localhost address from the final browser page".into(),
            ));
        }
        let code = pending.guard.accept(uri)?;
        let sender = pending
            .sender
            .take()
            .ok_or_else(|| CoreError::Auth("OAuth callback is expired or already used".into()))?;
        sender
            .send(code)
            .map_err(|_| CoreError::Auth("browser authorization is no longer active".into()))
    }
}

#[async_trait]
impl OAuthRedirectSession for LoopbackRedirectSession {
    fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    async fn wait(mut self: Box<Self>, timeout: Duration) -> Result<AuthCode> {
        tokio::select! {
            result = self.server.wait_for_code(timeout) => result,
            result = tokio::time::timeout(timeout, &mut self.manual_redirect) => {
                result
                    .map_err(|_| CoreError::Auth("sign-in timed out".into()))?
                    .map_err(|_| CoreError::Auth("browser authorization is no longer active".into()))
            }
        }
    }
}

impl Drop for LoopbackRedirectSession {
    fn drop(&mut self) {
        let Ok(mut pending) = self.pending.lock() else {
            return;
        };
        if pending
            .as_ref()
            .is_some_and(|pending| pending.expected_state == self.expected_state)
        {
            *pending = None;
        }
    }
}

fn same_redirect_endpoint(candidate: &str, expected: &str) -> bool {
    let Ok(candidate) = url::Url::parse(candidate.trim()) else {
        return false;
    };
    let Ok(expected) = url::Url::parse(expected) else {
        return false;
    };
    candidate.scheme() == expected.scheme()
        && candidate.host_str() == expected.host_str()
        && candidate.port_or_known_default() == expected.port_or_known_default()
        && candidate.path() == expected.path()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirect_parser_decodes_code_and_state_and_rejects_denial() {
        assert_eq!(
            parse_redirect_uri("com.flectar.mail.oauth://callback?code=a%2Fb&state=ok").unwrap(),
            AuthCode {
                code: "a/b".into(),
                state: Some("ok".into())
            }
        );
        assert!(
            parse_redirect_uri("com.flectar.mail.oauth://callback?error=access_denied&state=ok")
                .is_err()
        );
    }

    #[test]
    fn redirect_guard_rejects_wrong_state_expiry_and_duplicate_delivery() {
        let mut guard = OAuthRedirectGuard::new("right", Duration::from_secs(300));
        assert!(
            guard
                .accept("com.flectar.mail.oauth://callback?code=nope&state=wrong")
                .unwrap_err()
                .to_string()
                .contains("state mismatch")
        );
        assert_eq!(
            guard
                .accept("com.flectar.mail.oauth://callback?code=usable&state=right")
                .unwrap()
                .code,
            "usable"
        );
        assert!(
            guard
                .accept("com.flectar.mail.oauth://callback?code=again&state=right")
                .unwrap_err()
                .to_string()
                .contains("already used")
        );

        let mut expired = OAuthRedirectGuard::new("right", Duration::ZERO);
        assert!(
            expired
                .accept("com.flectar.mail.oauth://callback?code=late&state=right")
                .unwrap_err()
                .to_string()
                .contains("expired")
        );
    }

    #[tokio::test]
    async fn loopback_broker_accepts_a_callback_pasted_from_the_error_page() {
        let broker = LoopbackRedirectBroker::default();
        let session = broker.begin(Provider::Gmail, "right").await.unwrap();
        let callback = format!(
            "{}?state=right&iss=https%3A%2F%2Faccounts.google.com&code=a%2Fb",
            session.redirect_uri()
        );

        broker.submit_redirect(&callback).unwrap();
        assert_eq!(
            session.wait(Duration::from_secs(1)).await.unwrap(),
            AuthCode {
                code: "a/b".into(),
                state: Some("right".into()),
            }
        );
    }

    #[tokio::test]
    async fn pasted_callback_must_match_the_active_listener_and_state() {
        let broker = LoopbackRedirectBroker::default();
        let session = broker.begin(Provider::Gmail, "right").await.unwrap();
        assert!(
            broker
                .submit_redirect("http://127.0.0.1:1/?state=right&code=nope")
                .unwrap_err()
                .to_string()
                .contains("complete localhost address")
        );
        let wrong_state = format!("{}?state=wrong&code=nope", session.redirect_uri());
        assert!(
            broker
                .submit_redirect(&wrong_state)
                .unwrap_err()
                .to_string()
                .contains("state mismatch")
        );
    }
}
