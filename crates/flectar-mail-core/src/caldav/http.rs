//! Thin HTTP layer for WebDAV verbs. The `Transport` trait is the seam that
//! makes discovery/sync/push unit-testable against canned responses.

use crate::error::{CoreError, Result};
use crate::http_body;

const MAX_DAV_RESPONSE_BODY_BYTES: usize = 32 * 1024 * 1024;
const MAX_DAV_URL_BYTES: usize = 16 * 1024;

pub(super) fn validated_dav_url(value: &str) -> Result<url::Url> {
    if value.len() > MAX_DAV_URL_BYTES {
        return Err(CoreError::CalDav("calendar URL is too long".into()));
    }
    let url = url::Url::parse(value)
        .map_err(|error| CoreError::CalDav(format!("invalid calendar URL: {error}")))?;
    let loopback = match url
        .host()
        .ok_or_else(|| CoreError::CalDav("calendar URL has no host".into()))?
    {
        url::Host::Domain(host) => host.eq_ignore_ascii_case("localhost"),
        url::Host::Ipv4(address) => address.is_loopback(),
        url::Host::Ipv6(address) => address.is_loopback(),
    };
    let transport_is_safe = url.scheme() == "https" || (url.scheme() == "http" && loopback);
    if !transport_is_safe {
        return Err(CoreError::CalDav(
            "calendar credentials require HTTPS (plain HTTP is allowed only on loopback)".into(),
        ));
    }
    if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        return Err(CoreError::CalDav(
            "calendar URLs cannot contain credentials or fragments".into(),
        ));
    }
    Ok(url)
}

#[derive(Clone)]
pub enum DavAuth {
    Bearer(String),
    Basic(String, String),
}

impl std::fmt::Debug for DavAuth {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bearer(_) => formatter
                .debug_tuple("Bearer")
                .field(&"[REDACTED]")
                .finish(),
            Self::Basic(_, _) => formatter
                .debug_tuple("Basic")
                .field(&"[REDACTED]")
                .field(&"[REDACTED]")
                .finish(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct DavResponse {
    pub status: u16,
    pub etag: Option<String>,
    pub body: String,
}

impl DavResponse {
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// One WebDAV request. `depth` adds a Depth header when Some; `extra` carries
/// conditional headers (If-Match / If-None-Match).
#[async_trait::async_trait]
pub trait Transport: Send + Sync {
    async fn request(
        &self,
        method: &str,
        url: &str,
        depth: Option<&str>,
        extra: &[(&str, &str)],
        body: Option<String>,
    ) -> Result<DavResponse>;
}

pub struct HttpTransport {
    client: reqwest::Client,
    auth: DavAuth,
    origin: url::Origin,
}

impl HttpTransport {
    /// Bind credentials to one origin. Server-provided hrefs, persisted
    /// calendar URLs, and redirects can never forward them elsewhere.
    pub fn new(auth: DavAuth, base_url: &str) -> Result<Self> {
        let base_url = validated_dav_url(base_url)?;
        let origin = base_url.origin();
        let redirect_origin = origin.clone();
        let client = crate::http_tls::client_builder()
            .user_agent("flectar-mail-caldav/0.1")
            .timeout(std::time::Duration::from_secs(45))
            .redirect(reqwest::redirect::Policy::custom(move |attempt| {
                if attempt.previous().len() > 5 {
                    attempt.error("too many CalDAV redirects")
                } else if attempt.url().origin() != redirect_origin {
                    attempt.error("CalDAV redirect changed origin")
                } else {
                    attempt.follow()
                }
            }))
            .build()
            .map_err(|e| CoreError::CalDav(format!("http client: {e}")))?;
        Ok(Self {
            client,
            auth,
            origin,
        })
    }
}

#[async_trait::async_trait]
impl Transport for HttpTransport {
    async fn request(
        &self,
        method: &str,
        url: &str,
        depth: Option<&str>,
        extra: &[(&str, &str)],
        body: Option<String>,
    ) -> Result<DavResponse> {
        let method = reqwest::Method::from_bytes(method.as_bytes())
            .map_err(|_| CoreError::CalDav(format!("bad method {method}")))?;
        let url = validated_dav_url(url)?;
        if url.origin() != self.origin {
            return Err(CoreError::CalDav(
                "calendar URL changed origin; refusing to forward credentials".into(),
            ));
        }
        let mut req = self.client.request(method, url);
        req = match &self.auth {
            DavAuth::Bearer(token) => req.bearer_auth(token),
            DavAuth::Basic(user, pass) => req.basic_auth(user, Some(pass)),
        };
        if let Some(d) = depth {
            req = req.header("Depth", d);
        }
        for (k, v) in extra {
            req = req.header(*k, *v);
        }
        if let Some(b) = body {
            req = req
                .header("Content-Type", "application/xml; charset=utf-8")
                .body(b);
        }
        let resp = req.send().await.map_err(|e| {
            if e.is_connect() || e.is_timeout() {
                CoreError::Offline
            } else {
                CoreError::CalDav(format!("request: {e}"))
            }
        })?;
        let status = resp.status().as_u16();
        if status == 401 {
            return Err(CoreError::NeedsReauth);
        }
        let etag = resp
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let body =
            http_body::text(resp, MAX_DAV_RESPONSE_BODY_BYTES, "CalDAV response body").await?;
        Ok(DavResponse { status, etag, body })
    }
}

/// Canned-response transport for tests: pops responses in order and records
/// every request it saw.
#[cfg(test)]
use std::collections::HashMap;

#[cfg(test)]
pub struct MockTransport {
    pub responses: std::sync::Mutex<std::collections::VecDeque<DavResponse>>,
    pub seen: std::sync::Mutex<Vec<(String, String, Option<String>)>>, // (method, url, body)
    pub headers_seen: std::sync::Mutex<Vec<HashMap<String, String>>>,
}

#[cfg(test)]
impl MockTransport {
    pub fn new(responses: Vec<DavResponse>) -> Self {
        Self {
            responses: std::sync::Mutex::new(responses.into()),
            seen: std::sync::Mutex::new(Vec::new()),
            headers_seen: std::sync::Mutex::new(Vec::new()),
        }
    }
}

#[cfg(test)]
#[async_trait::async_trait]
impl Transport for MockTransport {
    async fn request(
        &self,
        method: &str,
        url: &str,
        depth: Option<&str>,
        extra: &[(&str, &str)],
        body: Option<String>,
    ) -> Result<DavResponse> {
        self.seen
            .lock()
            .unwrap()
            .push((method.to_string(), url.to_string(), body));
        let mut headers: HashMap<String, String> = extra
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        if let Some(d) = depth {
            headers.insert("Depth".into(), d.to_string());
        }
        self.headers_seen.lock().unwrap().push(headers);
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| CoreError::CalDav("mock transport exhausted".into()))
    }
}

#[cfg(test)]
mod url_tests {
    use super::*;

    #[test]
    fn dav_credentials_require_a_safe_url() {
        assert!(validated_dav_url("https://dav.example.test/cal/").is_ok());
        assert!(validated_dav_url("http://127.0.0.1:8080/cal/").is_ok());
        assert!(validated_dav_url("http://[::1]:8080/cal/").is_ok());
        for unsafe_url in [
            "http://dav.example.test/cal/",
            "ftp://dav.example.test/cal/",
            "https://user:secret@dav.example.test/cal/",
            "https://dav.example.test/cal/#private",
        ] {
            assert!(validated_dav_url(unsafe_url).is_err(), "{unsafe_url}");
        }
    }
}
