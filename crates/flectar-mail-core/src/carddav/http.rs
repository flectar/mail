//! Bounded HTTP transport for CardDAV's WebDAV methods.

use crate::error::{CoreError, Result};
use crate::http_body;

const MAX_BODY_BYTES: usize = 32 * 1024 * 1024;
const MAX_URL_BYTES: usize = 16 * 1024;

pub fn validated_url(value: &str) -> Result<url::Url> {
    if value.len() > MAX_URL_BYTES {
        return Err(super::err("server URL is too long"));
    }
    let url = url::Url::parse(value).map_err(|e| super::err(format!("invalid server URL: {e}")))?;
    let loopback = match url
        .host()
        .ok_or_else(|| super::err("server URL has no host"))?
    {
        url::Host::Domain(host) => host.eq_ignore_ascii_case("localhost"),
        url::Host::Ipv4(address) => address.is_loopback(),
        url::Host::Ipv6(address) => address.is_loopback(),
    };
    if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
        return Err(super::err(
            "CardDAV credentials require HTTPS (plain HTTP is allowed only on loopback)",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        return Err(super::err(
            "CardDAV URLs cannot contain credentials or fragments",
        ));
    }
    Ok(url)
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

#[async_trait::async_trait]
pub trait Transport: Send + Sync {
    async fn request(
        &self,
        method: &str,
        url: &str,
        depth: Option<&str>,
        headers: &[(&str, &str)],
        content_type: Option<&str>,
        body: Option<String>,
    ) -> Result<DavResponse>;
}

pub struct HttpTransport {
    client: reqwest::Client,
    username: String,
    password: String,
    origin: url::Origin,
}

impl HttpTransport {
    pub fn new(username: String, password: String, base_url: &str) -> Result<Self> {
        let base = validated_url(base_url)?;
        let origin = base.origin();
        let redirect_origin = origin.clone();
        let client = crate::http_tls::client_builder()
            .user_agent("flectar-mail-carddav/0.1")
            .timeout(std::time::Duration::from_secs(45))
            .redirect(reqwest::redirect::Policy::custom(move |attempt| {
                if attempt.previous().len() >= 5 {
                    attempt.error("too many CardDAV redirects")
                } else if attempt.url().origin() != redirect_origin {
                    attempt.error("CardDAV redirect changed origin")
                } else {
                    attempt.follow()
                }
            }))
            .build()
            .map_err(|e| super::err(format!("HTTP client: {e}")))?;
        Ok(Self {
            client,
            username,
            password,
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
        headers: &[(&str, &str)],
        content_type: Option<&str>,
        body: Option<String>,
    ) -> Result<DavResponse> {
        let method = reqwest::Method::from_bytes(method.as_bytes())
            .map_err(|_| super::err(format!("invalid HTTP method {method}")))?;
        let url = validated_url(url)?;
        if url.origin() != self.origin {
            return Err(super::err(
                "CardDAV URL changed origin; refusing to forward credentials",
            ));
        }
        let mut request = self
            .client
            .request(method, url)
            .basic_auth(&self.username, Some(&self.password));
        if let Some(depth) = depth {
            request = request.header("Depth", depth);
        }
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        if let Some(body) = body {
            request = request
                .header(
                    "Content-Type",
                    content_type.unwrap_or("application/xml; charset=utf-8"),
                )
                .body(body);
        }
        let response = request.send().await.map_err(|e| {
            if e.is_connect() || e.is_timeout() {
                CoreError::Offline
            } else {
                super::err(format!("request failed: {e}"))
            }
        })?;
        let status = response.status().as_u16();
        if status == 401 {
            return Err(CoreError::NeedsReauth);
        }
        let etag = response
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let body = http_body::text(response, MAX_BODY_BYTES, "CardDAV response body").await?;
        Ok(DavResponse { status, etag, body })
    }
}

#[cfg(test)]
pub type MockRequest = (
    String,
    String,
    Vec<(String, String)>,
    Option<String>,
    Option<String>,
    Option<String>,
);

#[cfg(test)]
pub struct MockTransport {
    pub responses: std::sync::Mutex<std::collections::VecDeque<DavResponse>>,
    pub seen: std::sync::Mutex<Vec<MockRequest>>,
}

#[cfg(test)]
impl MockTransport {
    pub fn new(responses: Vec<DavResponse>) -> Self {
        Self {
            responses: std::sync::Mutex::new(responses.into()),
            seen: Default::default(),
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
        headers: &[(&str, &str)],
        content_type: Option<&str>,
        body: Option<String>,
    ) -> Result<DavResponse> {
        self.seen.lock().unwrap().push((
            method.into(),
            url.into(),
            headers
                .iter()
                .map(|(name, value)| ((*name).into(), (*value).into()))
                .collect(),
            content_type.map(str::to_owned),
            body,
            depth.map(str::to_owned),
        ));
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| super::err("mock transport exhausted"))
    }
}
