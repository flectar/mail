//! Browser sign-in through a host's web authentication session, such as iOS's
//! ASWebAuthenticationSession.
//!
//! The session shows the provider's page in a system sheet that shares the
//! browser's sign-in state and hands the provider's redirect back to the app,
//! which may be suspended while the page is in front. That is why these hosts
//! cannot use the desktop flow's loopback listener.
//!
//! [`WebAuthentication`] is both the OAuth redirect broker and the platform
//! context's authorization handler. The session itself sits behind
//! [`SessionPresenter`], so these rules run in the unit tests on every
//! platform.

use async_trait::async_trait;
use flectar_mail_core::{
    error::{CoreError, Result},
    models::Provider,
    oauth::{
        loopback::AuthCode,
        redirect::{
            OAUTH_REDIRECT_TIMEOUT, OAuthRedirectBroker, OAuthRedirectGuard, OAuthRedirectSession,
        },
    },
};
use std::{
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicI64, Ordering},
    },
    time::Duration,
};
use tokio::sync::oneshot;

/// Microsoft's redirect for the app's iOS platform registration.
pub(crate) const MICROSOFT_REDIRECT: &str = "msauth.com.flectar.mail://auth";

/// Shows and closes the host's sign-in sessions.
pub(crate) trait SessionPresenter: Send + Sync + 'static {
    /// Show `url` until the provider redirects to `scheme`, then report the
    /// result to [`WebAuthentication::finished`] with the same `id`. Must
    /// return once the session has been handed to the host.
    fn start(&self, id: i64, url: &str, scheme: &str);
    /// Close session `id` if it is still showing.
    fn cancel(&self, id: i64);
}

/// How a session ended.
pub(crate) enum Outcome {
    /// The provider redirected to this URL.
    Redirected(String),
    /// The user dismissed the sheet.
    Cancelled,
    Failed(String),
}

struct Pending {
    transaction: i64,
    guard: OAuthRedirectGuard,
    redirect_uri: String,
    /// Whether a session shows this sign-in.
    started: bool,
    sender: oneshot::Sender<Result<AuthCode>>,
}

struct State<P> {
    presenter: P,
    /// One sign-in at a time. Sessions run inside the app, so nothing has to
    /// outlive the process.
    pending: Mutex<Option<Pending>>,
    next_transaction: AtomicI64,
}

/// A cheap handle to the host's sign-in state.
pub(crate) struct WebAuthentication<P> {
    state: Arc<State<P>>,
}

impl<P> Clone for WebAuthentication<P> {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
        }
    }
}

struct WebAuthenticationSession<P: SessionPresenter> {
    state: Arc<State<P>>,
    transaction: i64,
    redirect_uri: String,
    receiver: Option<oneshot::Receiver<Result<AuthCode>>>,
}

/// Where the provider sends the user back to.
fn redirect_uri(provider: Provider) -> Result<String> {
    match provider {
        Provider::Microsoft => Ok(MICROSOFT_REDIRECT.to_owned()),
        Provider::Gmail => {
            let (client_id, _) =
                flectar_mail_core::oauth::providers::resolve_credentials(provider)?;
            flectar_mail_core::oauth::providers::google_reversed_client_id_redirect(&client_id)
        }
        Provider::Generic => Err(CoreError::Auth("provider does not use oauth".into())),
    }
}

impl<P: SessionPresenter> WebAuthentication<P> {
    pub(crate) fn new(presenter: P) -> Self {
        Self {
            state: Arc::new(State {
                presenter,
                pending: Mutex::new(None),
                next_transaction: AtomicI64::new(1),
            }),
        }
    }

    fn begin_with(
        &self,
        redirect_uri: String,
        expected_state: &str,
    ) -> Result<WebAuthenticationSession<P>> {
        let (sender, receiver) = oneshot::channel();
        let transaction = self.state.next_transaction.fetch_add(1, Ordering::Relaxed);
        let mut pending = self
            .state
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if pending.is_some() {
            return Err(CoreError::Auth(
                "another browser authorization is already in progress".into(),
            ));
        }
        *pending = Some(Pending {
            transaction,
            guard: OAuthRedirectGuard::new(expected_state, OAUTH_REDIRECT_TIMEOUT),
            redirect_uri: redirect_uri.clone(),
            started: false,
            sender,
        });
        Ok(WebAuthenticationSession {
            state: self.state.clone(),
            transaction,
            redirect_uri,
            receiver: Some(receiver),
        })
    }

    /// Show the provider's sign-in page in a session. This is the platform
    /// context's authorization handler, called with the flow's checked
    /// HTTPS address.
    pub(crate) fn authorize(&self, url: &str) -> std::result::Result<(), String> {
        let (transaction, scheme) = {
            let mut pending = self
                .state
                .pending
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let pending = pending
                .as_mut()
                .ok_or_else(|| "no sign-in is waiting for this page".to_owned())?;
            if pending.started {
                return Err("the sign-in page is already open".into());
            }
            // The flow names the redirect it waits for in the address.
            let redirect = url::Url::parse(url)
                .map_err(|_| "the sign-in address is invalid".to_owned())?
                .query_pairs()
                .find(|(key, _)| key == "redirect_uri")
                .map(|(_, value)| value.into_owned());
            if redirect.as_deref() != Some(pending.redirect_uri.as_str()) {
                return Err("the sign-in address does not match the waiting sign-in".into());
            }
            let scheme = url::Url::parse(&pending.redirect_uri)
                .map_err(|_| "the sign-in redirect is invalid".to_owned())?
                .scheme()
                .to_owned();
            pending.started = true;
            (pending.transaction, scheme)
        };
        self.state.presenter.start(transaction, url, &scheme);
        Ok(())
    }

    /// Report how session `id` ended. Sessions the flow has given up on are
    /// ignored.
    pub(crate) fn finished(&self, id: i64, outcome: Outcome) {
        let mut pending = self
            .state
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if !pending
            .as_ref()
            .is_some_and(|pending| pending.transaction == id)
        {
            return;
        }
        let Some(mut current) = pending.take() else {
            return;
        };
        let result = match outcome {
            Outcome::Redirected(uri) => accept(&mut current, &uri),
            Outcome::Cancelled => Err(CoreError::Auth("sign-in was cancelled".into())),
            Outcome::Failed(error) => Err(CoreError::Auth(format!("sign-in failed: {error}"))),
        };
        let _ = current.sender.send(result);
    }
}

/// A session returns any URL in the redirect's scheme; the rest must match
/// the redirect too.
fn accept(pending: &mut Pending, uri: &str) -> Result<AuthCode> {
    if uri.split(['?', '#']).next() != Some(pending.redirect_uri.as_str()) {
        return Err(CoreError::Auth(
            "sign-in returned to an unexpected address".into(),
        ));
    }
    pending.guard.accept(uri)
}

#[async_trait]
impl<P: SessionPresenter> OAuthRedirectBroker for WebAuthentication<P> {
    async fn begin(
        &self,
        provider: Provider,
        expected_state: &str,
    ) -> Result<Box<dyn OAuthRedirectSession>> {
        Ok(Box::new(
            self.begin_with(redirect_uri(provider)?, expected_state)?,
        ))
    }
}

#[async_trait]
impl<P: SessionPresenter> OAuthRedirectSession for WebAuthenticationSession<P> {
    fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    async fn wait(mut self: Box<Self>, timeout: Duration) -> Result<AuthCode> {
        let receiver = self
            .receiver
            .take()
            .ok_or_else(|| CoreError::Auth("sign-in already finished".into()))?;
        match tokio::time::timeout(timeout, receiver).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(CoreError::Auth("sign-in was cancelled".into())),
            Err(_) => Err(CoreError::Auth("sign-in timed out".into())),
        }
    }
}

impl<P: SessionPresenter> Drop for WebAuthenticationSession<P> {
    /// Forget the sign-in however the flow ends, including when the page
    /// never opened, and close a sheet that is still showing.
    fn drop(&mut self) {
        let mut pending = self
            .state
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if !pending
            .as_ref()
            .is_some_and(|pending| pending.transaction == self.transaction)
        {
            return;
        }
        if let Some(Pending { started: true, .. }) = pending.take() {
            self.state.presenter.cancel(self.transaction);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records the sessions the sign-in asks for.
    #[derive(Default)]
    struct Sheets {
        started: Mutex<Vec<(i64, String, String)>>,
        cancelled: Mutex<Vec<i64>>,
    }

    impl SessionPresenter for Arc<Sheets> {
        fn start(&self, id: i64, url: &str, scheme: &str) {
            self.started
                .lock()
                .unwrap()
                .push((id, url.to_owned(), scheme.to_owned()));
        }

        fn cancel(&self, id: i64) {
            self.cancelled.lock().unwrap().push(id);
        }
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
    }

    fn sign_in() -> (WebAuthentication<Arc<Sheets>>, Arc<Sheets>) {
        let sheets = Arc::new(Sheets::default());
        (WebAuthentication::new(sheets.clone()), sheets)
    }

    fn address(redirect_uri: &str) -> String {
        let redirect: String =
            url::form_urlencoded::byte_serialize(redirect_uri.as_bytes()).collect();
        format!("https://login.example.com/authorize?client_id=c&redirect_uri={redirect}&state=s")
    }

    #[test]
    fn a_redirect_from_the_session_completes_the_sign_in() {
        let (broker, sheets) = sign_in();
        let session = broker.begin_with(MICROSOFT_REDIRECT.into(), "s").unwrap();
        broker.authorize(&address(MICROSOFT_REDIRECT)).unwrap();
        let (id, _, scheme) = sheets.started.lock().unwrap()[0].clone();
        assert_eq!(scheme, "msauth.com.flectar.mail");
        broker.finished(
            id,
            Outcome::Redirected(format!("{MICROSOFT_REDIRECT}?code=c0de&state=s")),
        );
        let code = runtime()
            .block_on(Box::new(session).wait(Duration::from_secs(5)))
            .unwrap();
        assert_eq!(code.code, "c0de");
        // The finished session needs no cancelling, and a new sign-in can start.
        assert!(sheets.cancelled.lock().unwrap().is_empty());
        broker.begin_with(MICROSOFT_REDIRECT.into(), "t").unwrap();
    }

    #[test]
    fn cancelled_failed_and_forged_sessions_end_the_sign_in() {
        let wrong_state = format!("{MICROSOFT_REDIRECT}?code=c0de&state=other");
        let wrong_address = "msauth.com.flectar.mail://elsewhere?code=c0de&state=s".to_owned();
        for (outcome, error) in [
            (Outcome::Cancelled, "cancelled"),
            (Outcome::Failed("no network".into()), "no network"),
            (Outcome::Redirected(wrong_state), "state mismatch"),
            (Outcome::Redirected(wrong_address), "unexpected address"),
        ] {
            let (broker, sheets) = sign_in();
            let session = broker.begin_with(MICROSOFT_REDIRECT.into(), "s").unwrap();
            broker.authorize(&address(MICROSOFT_REDIRECT)).unwrap();
            let id = sheets.started.lock().unwrap()[0].0;
            broker.finished(id, outcome);
            let result = runtime().block_on(Box::new(session).wait(Duration::from_secs(5)));
            assert!(
                matches!(&result, Err(CoreError::Auth(message)) if message.contains(error)),
                "{error}: {:?}",
                result.err()
            );
        }
    }

    #[test]
    fn a_timeout_closes_the_sheet_and_ignores_its_late_result() {
        let (broker, sheets) = sign_in();
        let session = broker.begin_with(MICROSOFT_REDIRECT.into(), "s").unwrap();
        broker.authorize(&address(MICROSOFT_REDIRECT)).unwrap();
        let id = sheets.started.lock().unwrap()[0].0;
        let result = runtime().block_on(Box::new(session).wait(Duration::from_millis(10)));
        assert!(matches!(result, Err(CoreError::Auth(message)) if message.contains("timed out")));
        assert_eq!(*sheets.cancelled.lock().unwrap(), [id]);
        // The next sign-in is not disturbed by the old session's report.
        let next = broker.begin_with(MICROSOFT_REDIRECT.into(), "t").unwrap();
        broker.finished(id, Outcome::Cancelled);
        broker.authorize(&address(MICROSOFT_REDIRECT)).unwrap();
        let next_id = sheets.started.lock().unwrap()[1].0;
        assert_ne!(next_id, id);
        broker.finished(
            next_id,
            Outcome::Redirected(format!("{MICROSOFT_REDIRECT}?code=n&state=t")),
        );
        let code = runtime()
            .block_on(Box::new(next).wait(Duration::from_secs(5)))
            .unwrap();
        assert_eq!(code.code, "n");
    }

    #[test]
    fn a_sign_in_whose_page_never_opened_is_released() {
        let (broker, sheets) = sign_in();
        let session = broker.begin_with(MICROSOFT_REDIRECT.into(), "s").unwrap();
        assert!(
            broker
                .begin_with(MICROSOFT_REDIRECT.into(), "t")
                .is_err_and(|error| error.to_string().contains("already in progress"))
        );
        drop(session);
        assert!(sheets.cancelled.lock().unwrap().is_empty());
        broker.begin_with(MICROSOFT_REDIRECT.into(), "t").unwrap();
    }

    #[test]
    fn only_the_waiting_sign_in_opens_and_only_once() {
        let (broker, sheets) = sign_in();
        assert!(broker.authorize(&address(MICROSOFT_REDIRECT)).is_err());
        let _session = broker.begin_with(MICROSOFT_REDIRECT.into(), "s").unwrap();
        for url in [
            address("msauth.com.flectar.mail://other"),
            "https://login.example.com/authorize?client_id=c&state=s".to_owned(),
            "not a url".to_owned(),
        ] {
            assert!(broker.authorize(&url).is_err(), "{url}");
        }
        broker.authorize(&address(MICROSOFT_REDIRECT)).unwrap();
        assert!(broker.authorize(&address(MICROSOFT_REDIRECT)).is_err());
        assert_eq!(sheets.started.lock().unwrap().len(), 1);
    }
}
