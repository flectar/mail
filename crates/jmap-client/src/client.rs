/*
 * Copyright Stalwart Labs LLC See the COPYING
 * file at the top-level directory of this distribution.
 *
 * Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
 * https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
 * <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
 * option. This file may not be copied, modified, or distributed
 * except according to those terms.
 */

use crate::{
    blob,
    core::{
        request::{self, Request},
        response,
        session::{Session, URLPart},
    },
    Error,
};
use ahash::AHashSet;
use base64::{engine::general_purpose, Engine};
#[cfg(feature = "blocking")]
use reqwest::blocking::{Client as HttpClient, Response};
use reqwest::{
    header::{self},
    redirect,
};
#[cfg(feature = "async")]
use reqwest::{Client as HttpClient, Response};
use serde::de::DeserializeOwned;
use std::{
    net::IpAddr,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

const DEFAULT_TIMEOUT_MS: u64 = 10 * 1000;
static USER_AGENT: &str = concat!("jmap-client/", env!("CARGO_PKG_VERSION"));

#[derive(Debug, PartialEq, Eq)]
pub enum Credentials {
    Basic(String),
    Bearer(String),
}

pub struct Client {
    session: parking_lot::Mutex<Arc<Session>>,
    session_url: String,
    api_url: String,
    session_updated: AtomicBool,
    trusted_hosts: Arc<AHashSet<String>>,

    upload_url: Vec<URLPart<blob::URLParameter>>,
    download_url: Vec<URLPart<blob::URLParameter>>,
    #[cfg(feature = "async")]
    event_source_url: Vec<URLPart<crate::event_source::URLParameter>>,

    headers: header::HeaderMap,
    default_account_id: String,
    timeout: Duration,
    pub(crate) accept_invalid_certs: bool,

    #[cfg(feature = "websockets")]
    pub(crate) authorization: String,
    #[cfg(feature = "websockets")]
    pub(crate) ws: tokio::sync::Mutex<Option<crate::client_ws::WsStream>>,
}

pub struct ClientBuilder {
    credentials: Option<Credentials>,
    trusted_hosts: AHashSet<String>,
    forwarded_for: Option<String>,
    accept_invalid_certs: bool,
    timeout: Duration,
}

impl Default for ClientBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl ClientBuilder {
    /// Creates a new `ClientBuilder`.
    ///
    /// Setting the credentials is required to connect to the JMAP API.
    pub fn new() -> Self {
        Self {
            credentials: None,
            trusted_hosts: AHashSet::new(),
            timeout: Duration::from_millis(DEFAULT_TIMEOUT_MS),
            forwarded_for: None,
            accept_invalid_certs: false,
        }
    }

    /// Set up client credentials to connect to the JMAP API.
    ///
    /// The JMAP API URL is set using the [ClientBuilder.connect()](struct.ClientBuilder.html#method.connect) method.
    ///
    /// # Bearer authentication
    /// Pass a `&str` with the API Token.
    ///
    /// ```rust
    /// Client::new().credentials("some-api-token");
    /// ```
    ///
    /// Or use the longer form by using [Credentials::bearer()](enum.Credentials.html#method.bearer).
    /// ```rust
    /// let credentials = Credentials::bearer("some-api-token");
    /// Client::new().credentials(credentials);
    /// ```
    ///
    /// # Basic authentication
    /// Pass a `(&str, &str)` tuple, with the first position containing a username and the second containing a password.
    ///
    /// **It is not suggested to use this approach in production;** instead, if possible, use [Bearer authentication](struct.ClientBuilder.html#bearer-authentication).
    ///
    /// ```rust
    /// Client::new().credentials(("user@domain.com", "password"));
    /// ```
    ///
    /// Or use the longer form by using [Credentials::basic()](enum.Credentials.html#method.basic).
    /// ```rust
    /// let credentials = Credentials::basic("user@domain.com", "password");
    /// Client::new().credentials(credentials);
    /// ```
    pub fn credentials(mut self, credentials: impl Into<Credentials>) -> Self {
        self.credentials = Some(credentials.into());
        self
    }

    /// Set a timeout for all the requests to the JMAP API.
    ///
    /// The timeout can be changed after the `Client` has been created by using [Client.set_timeout()](struct.Client.html#method.set_timeout).
    ///
    /// By default the timeout is 10 seconds.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Accepts invalid certificates for all the requests to the JMAP API.
    ///
    /// By default certificates are validated.
    ///
    /// # Warning
    /// **It is not suggested to use this approach in production;** this method should be used only for testing and as a last resort.
    ///
    /// [Read more in the reqwest docs](https://docs.rs/reqwest/latest/reqwest/struct.ClientBuilder.html#method.danger_accept_invalid_certs)
    pub fn accept_invalid_certs(mut self, accept_invalid_certs: bool) -> Self {
        self.accept_invalid_certs = accept_invalid_certs;
        self
    }

    /// Set a list of trusted hosts that will be checked when a redirect is required.
    ///
    /// The list can be changed after the `Client` has been created by using [Client.set_follow_redirects()](struct.Client.html#method.set_follow_redirects).
    ///
    /// The client will follow at most 5 redirects.
    pub fn follow_redirects(
        mut self,
        trusted_hosts: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.trusted_hosts = trusted_hosts.into_iter().map(|h| h.into()).collect();
        self
    }

    /// Set the originating IP address of the client connecting to the JMAP API.
    pub fn forwarded_for(mut self, forwarded_for: IpAddr) -> Self {
        self.forwarded_for = Some(match forwarded_for {
            IpAddr::V4(addr) => format!("for={}", addr),
            IpAddr::V6(addr) => format!("for=\"{}\"", addr),
        });
        self
    }

    /// Connects to the JMAP API Session URL.
    ///
    /// Setting up [Credentials](struct.ClientBuilder.html#method.credentials) must be done before calling this function.
    #[maybe_async::maybe_async]
    pub async fn connect(self, url: &str) -> crate::Result<Client> {
        let authorization = match self.credentials.expect("Missing credentials") {
            Credentials::Basic(s) => format!("Basic {}", s),
            Credentials::Bearer(s) => format!("Bearer {}", s),
        };
        let mut headers = header::HeaderMap::new();
        headers.insert(
            header::USER_AGENT,
            header::HeaderValue::from_static(USER_AGENT),
        );
        headers.insert(
            header::AUTHORIZATION,
            header::HeaderValue::from_str(&authorization).unwrap(),
        );
        if let Some(forwarded_for) = self.forwarded_for {
            headers.insert(
                header::FORWARDED,
                header::HeaderValue::from_str(&forwarded_for).unwrap(),
            );
        }

        let trusted_hosts = Arc::new(self.trusted_hosts);

        let trusted_hosts_ = trusted_hosts.clone();
        let session_url = format!("{}/.well-known/jmap", url);
        let session: Session = serde_json::from_slice(
            &Client::handle_error(
                http_client_builder()
                    .timeout(self.timeout)
                    .danger_accept_invalid_certs(self.accept_invalid_certs)
                    .redirect(redirect::Policy::custom(move |attempt| {
                        if attempt.previous().len() > 5 {
                            attempt.error("Too many redirects.")
                        } else if matches!( attempt.url().host_str(), Some(host) if trusted_hosts_.contains(host) )
                        {
                                attempt.follow()
                        } else {
                            let message = format!(
                                "Aborting redirect request to unknown host '{}'.",
                                attempt.url().host_str().unwrap_or("")
                            );
                            attempt.error(message)
                        }
                    }))
                    .default_headers(headers.clone())
                    .build()?
                    .get(&session_url)
                    .send()
                    .await?,
            )
            .await?
            .bytes()
            .await?,
        )?;

        let default_account_id = session
            .primary_accounts()
            .next()
            .map(|a| a.1.to_string())
            .unwrap_or_default();

        headers.insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("application/json"),
        );

        Ok(Client {
            download_url: URLPart::parse(session.download_url())?,
            upload_url: URLPart::parse(session.upload_url())?,
            #[cfg(feature = "async")]
            event_source_url: URLPart::parse(session.event_source_url())?,
            api_url: session.api_url().to_string(),
            session: parking_lot::Mutex::new(Arc::new(session)),
            session_url,
            session_updated: true.into(),
            accept_invalid_certs: self.accept_invalid_certs,
            trusted_hosts,
            #[cfg(feature = "websockets")]
            authorization,
            timeout: self.timeout,
            headers,
            default_account_id,
            #[cfg(feature = "websockets")]
            ws: None.into(),
        })
    }
}

impl Client {
    #[allow(clippy::new_ret_no_self)]
    pub fn new() -> ClientBuilder {
        ClientBuilder::new()
    }

    pub fn set_timeout(&mut self, timeout: Duration) -> &mut Self {
        self.timeout = timeout;
        self
    }

    pub fn set_follow_redirects(
        &mut self,
        trusted_hosts: impl IntoIterator<Item = impl Into<String>>,
    ) -> &mut Self {
        self.trusted_hosts = Arc::new(trusted_hosts.into_iter().map(|h| h.into()).collect());
        self
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    pub fn session(&self) -> Arc<Session> {
        self.session.lock().clone()
    }

    pub fn session_url(&self) -> &str {
        &self.session_url
    }

    pub fn headers(&self) -> &header::HeaderMap {
        &self.headers
    }

    pub(crate) fn redirect_policy(&self) -> redirect::Policy {
        let trusted_hosts = self.trusted_hosts.clone();
        redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() > 5 {
                attempt.error("Too many redirects.")
            } else if matches!( attempt.url().host_str(), Some(host) if trusted_hosts.contains(host) )
            {
                attempt.follow()
            } else {
                let message = format!(
                    "Aborting redirect request to unknown host '{}'.",
                    attempt.url().host_str().unwrap_or("")
                );
                attempt.error(message)
            }
        })
    }

    #[maybe_async::maybe_async]
    pub async fn send<R>(
        &self,
        request: &request::Request<'_>,
    ) -> crate::Result<response::Response<R>>
    where
        R: DeserializeOwned,
    {
        let response: response::Response<R> = serde_json::from_slice(
            &Client::handle_error(
                http_client_builder()
                    .redirect(self.redirect_policy())
                    .danger_accept_invalid_certs(self.accept_invalid_certs)
                    .timeout(self.timeout)
                    .default_headers(self.headers.clone())
                    .build()?
                    .post(&self.api_url)
                    .body(serde_json::to_string(&request)?)
                    .send()
                    .await?,
            )
            .await?
            .bytes()
            .await?,
        )?;

        if response.session_state() != self.session.lock().state() {
            self.session_updated.store(false, Ordering::Relaxed);
        }

        Ok(response)
    }

    #[maybe_async::maybe_async]
    pub async fn refresh_session(&self) -> crate::Result<()> {
        let session: Session = serde_json::from_slice(
            &Client::handle_error(
                http_client_builder()
                    .timeout(Duration::from_millis(DEFAULT_TIMEOUT_MS))
                    .danger_accept_invalid_certs(self.accept_invalid_certs)
                    .redirect(self.redirect_policy())
                    .default_headers(self.headers.clone())
                    .build()?
                    .get(&self.session_url)
                    .send()
                    .await?,
            )
            .await?
            .bytes()
            .await?,
        )?;
        *self.session.lock() = Arc::new(session);
        self.session_updated.store(true, Ordering::Relaxed);
        Ok(())
    }

    pub fn is_session_updated(&self) -> bool {
        self.session_updated.load(Ordering::Relaxed)
    }

    pub fn set_default_account_id(&mut self, defaul_account_id: impl Into<String>) -> &mut Self {
        self.default_account_id = defaul_account_id.into();
        self
    }

    pub fn default_account_id(&self) -> &str {
        &self.default_account_id
    }

    pub fn build(&self) -> Request<'_> {
        Request::new(self)
    }

    pub fn download_url(&self) -> &[URLPart<blob::URLParameter>] {
        &self.download_url
    }

    pub fn upload_url(&self) -> &[URLPart<blob::URLParameter>] {
        &self.upload_url
    }

    #[cfg(feature = "async")]
    pub fn event_source_url(&self) -> &[URLPart<crate::event_source::URLParameter>] {
        &self.event_source_url
    }

    #[maybe_async::maybe_async]
    pub async fn handle_error(response: Response) -> crate::Result<Response> {
        if response.status().is_success() {
            Ok(response)
        } else if let Some(b"application/problem+json") = response
            .headers()
            .get(header::CONTENT_TYPE)
            .map(|h| h.as_bytes())
        {
            Err(Error::Problem(serde_json::from_slice(
                &response.bytes().await?,
            )?))
        } else {
            Err(Error::Server(format!("{}", response.status())))
        }
    }
}

impl Credentials {
    pub fn basic(username: &str, password: &str) -> Self {
        Credentials::Basic(general_purpose::STANDARD.encode(format!("{}:{}", username, password)))
    }

    pub fn bearer(token: impl Into<String>) -> Self {
        Credentials::Bearer(token.into())
    }
}

impl From<&str> for Credentials {
    fn from(s: &str) -> Self {
        Credentials::bearer(s.to_string())
    }
}

impl From<String> for Credentials {
    fn from(s: String) -> Self {
        Credentials::bearer(s)
    }
}

impl From<(&str, &str)> for Credentials {
    fn from((username, password): (&str, &str)) -> Self {
        Credentials::basic(username, password)
    }
}

impl From<(String, String)> for Credentials {
    fn from((username, password): (String, String)) -> Self {
        Credentials::basic(&username, &password)
    }
}

#[cfg(test)]
mod tests {
    use crate::core::response::{Response, TaggedMethodResponse};

    #[test]
    fn test_deserialize() {
        let _r: Response<TaggedMethodResponse> = serde_json::from_slice(
            br#"{"sessionState": "123", "methodResponses": [[ "Email/query", {
                "accountId": "A1",
                "queryState": "abcdefg",
                "canCalculateChanges": true,
                "position": 0,
                "total": 101,
                "ids": [ "msg1023", "msg223", "msg110", "msg93", "msg91",
                    "msg38", "msg36", "msg33", "msg11", "msg1" ]
            }, "t0" ],
            [ "Email/get", {
                "accountId": "A1",
                "state": "123456",
                "list": [{
                    "id": "msg1023",
                    "threadId": "trd194"
                }, {
                    "id": "msg223",
                    "threadId": "trd114"
                }
                ],
                "notFound": []
            }, "t1" ],
            [ "Thread/get", {
                "accountId": "A1",
                "state": "123456",
                "list": [{
                    "id": "trd194",
                    "emailIds": [ "msg1020", "msg1021", "msg1023" ]
                }, {
                    "id": "trd114",
                    "emailIds": [ "msg201", "msg223" ]
                }
                ],
                "notFound": []
            }, "t2" ]]}"#,
        )
        .unwrap();
    }
}

// HarmonyOS apps use explicit embedded trust roots. Desktop keeps its existing
// platform verification. This port-only helper avoids desktop certificate
// discovery paths without changing the public JMAP API or request behavior.
fn http_client_builder() -> reqwest::ClientBuilder {
    let builder = HttpClient::builder();
    #[cfg(target_env = "ohos")]
    {
        static CONFIG: std::sync::LazyLock<rustls::ClientConfig> = std::sync::LazyLock::new(|| {
            let mut roots = rustls::RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .expect("ring supports the standard TLS protocol versions")
            .with_root_certificates(roots)
            .with_no_client_auth()
        });
        builder.tls_backend_preconfigured(CONFIG.clone())
    }
    #[cfg(not(target_env = "ohos"))]
    {
        builder
    }
}
