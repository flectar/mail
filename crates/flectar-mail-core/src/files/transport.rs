use super::err;
use crate::error::Result;
use reqwest::{Client, Method, RequestBuilder, Response};
use url::Url;

#[derive(Clone)]
pub struct Transport {
    http: Client,
    origin: url::Origin,
    user: String,
    secret: String,
}
impl Transport {
    pub fn new(endpoint: &str, user: &str, secret: &str) -> Result<Self> {
        let url = validated_url(endpoint)?;
        Ok(Self {
            http: Client::builder()
                .connect_timeout(std::time::Duration::from_secs(10))
                .timeout(std::time::Duration::from_secs(120))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|_| err("Could not initialize file transport."))?,
            origin: url.origin(),
            user: user.into(),
            secret: secret.into(),
        })
    }
    pub fn request(&self, method: &str, url: &str) -> Result<RequestBuilder> {
        let url = validated_url(url)?;
        if url.origin() != self.origin {
            return Err(err(
                "Storage endpoint changed origin; credentials were not sent.",
            ));
        }
        Ok(self
            .http
            .request(
                Method::from_bytes(method.as_bytes()).map_err(|_| err("Invalid HTTP method."))?,
                url,
            )
            .basic_auth(&self.user, Some(&self.secret)))
    }
    /// RFC 8620 discovery can redirect; credentials stay on the original origin.
    /// Mutations deliberately do not redirect or retry.
    pub async fn discover(&self, endpoint: &str) -> Result<serde_json::Value> {
        let mut url = validated_url(endpoint)?;
        for _ in 0..6 {
            let response = self
                .request("GET", url.as_str())?
                .send()
                .await
                .map_err(|_| err("Could not discover file storage."))?;
            if response.status().is_redirection() {
                let location = response
                    .headers()
                    .get("Location")
                    .and_then(|h| h.to_str().ok())
                    .ok_or_else(|| err("Invalid discovery redirect."))?;
                url = url
                    .join(location)
                    .map_err(|_| err("Invalid discovery redirect URL."))?;
                let _ = self.request("GET", url.as_str())?;
                continue;
            }
            if !response.status().is_success() {
                return Err(err(format!(
                    "File discovery failed (HTTP {}).",
                    response.status().as_u16()
                )));
            }
            let mut session: serde_json::Value = serde_json::from_slice(
                &crate::http_body::bytes(response, 16 * 1024 * 1024, "File session").await?,
            )?;
            // Resolve relative templates against the final discovery location,
            // including after redirects. Mail search and file push use these too.
            for key in ["apiUrl", "uploadUrl", "downloadUrl", "eventSourceUrl"] {
                if let Some(template) = session[key].as_str() {
                    let resolved = url
                        .join(template)
                        .map_err(|_| err("Invalid session endpoint."))?
                        .to_string()
                        .replace("%7B", "{")
                        .replace("%7D", "}");
                    session[key] = resolved.into();
                }
            }
            return Ok(session);
        }
        Err(err("Too many file discovery redirects."))
    }
    pub async fn send(&self, request: RequestBuilder) -> Result<Response> {
        let response = request.send().await.map_err(|e| {
            if e.is_timeout() {
                err("File request timed out. Refresh before retrying a change.")
            } else {
                err("Could not reach file storage.")
            }
        })?;
        if !response.status().is_success() {
            return Err(err(match response.status().as_u16() {
                401 => "Storage authentication expired. Reconnect the account.",
                403 => "You do not have permission for this file operation.",
                404 => "File storage or the requested file was not found.",
                409 => "The destination or file hierarchy conflicts with this operation.",
                412 => "The file changed on the server. Refresh before retrying.",
                423 => "The file is locked. Refresh or release your lock before editing.",
                413 | 507 => "The file exceeds the server size limit or storage quota.",
                429 => "Too many requests. Wait before refreshing.",
                _ => "The file server rejected the request. Refresh before retrying.",
            }));
        }
        Ok(response)
    }
    pub async fn json(&self, request: RequestBuilder) -> Result<serde_json::Value> {
        let response = self.send(request).await?;
        let bytes = crate::http_body::bytes(response, 16 * 1024 * 1024, "File metadata").await?;
        Ok(serde_json::from_slice(&bytes)?)
    }
}

/// RFC 8620 /get may return records in any order. Preserve the /query order
/// across pages, allowing records deleted between query and get to be absent.
pub(super) fn ordered_records<T>(
    ids: &[String],
    records: Vec<T>,
    id: impl Fn(&T) -> &str,
) -> Result<Vec<T>> {
    let requested = ids
        .iter()
        .map(String::as_str)
        .collect::<std::collections::HashSet<_>>();
    let mut by_id = std::collections::HashMap::with_capacity(records.len());
    for record in records {
        let key = id(&record).to_owned();
        if !requested.contains(key.as_str()) || by_id.insert(key, record).is_some() {
            return Err(err("JMAP returned an unexpected or duplicate record."));
        }
    }
    Ok(ids.iter().filter_map(|id| by_id.remove(id)).collect())
}
pub fn validated_url(value: &str) -> Result<Url> {
    let url = Url::parse(value).map_err(|_| err("Enter a valid HTTPS storage URL."))?;
    let local = match url.host() {
        Some(url::Host::Domain(h)) => h == "localhost",
        Some(url::Host::Ipv4(a)) => a.is_loopback(),
        Some(url::Host::Ipv6(a)) => a.is_loopback(),
        _ => false,
    };
    if value.len() > 16384
        || !(url.scheme() == "https" || url.scheme() == "http" && local)
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(err(
            "Storage requires HTTPS and a URL without credentials or fragments (HTTP is allowed on loopback).",
        ));
    }
    Ok(url)
}
/// RFC 6570 simple expansion: encode even '/' in opaque identifiers.
pub fn expand(template: &str, values: &[(&str, &str)]) -> Result<String> {
    let mut out = template.to_owned();
    for (key, value) in values {
        let encoded: String = url::form_urlencoded::byte_serialize(value.as_bytes())
            .collect::<String>()
            .replace('+', "%20");
        out = out.replace(&format!("{{{key}}}"), &encoded);
    }
    if out.contains(['{', '}']) {
        return Err(err("Unsupported storage URL template."));
    }
    validated_url(&out)?;
    Ok(out)
}
