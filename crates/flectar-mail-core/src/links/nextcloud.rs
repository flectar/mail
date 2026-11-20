//! Nextcloud, ownCloud, OpenCloud and ownCloud Infinite Scale servers: uploads go over
//! WebDAV, and public links come from the OCS share API, version 1, which all four serve.

use super::err;
use crate::error::Result;
use crate::files::transport::{Transport, validated_url};
use crate::net::proxy::Owner;
use serde_json::Value;
use std::path::Path;
use std::time::{Duration, Instant};
use url::Url;

/// The name the server shows for the app password a sign-in creates.
const USER_AGENT: &str = "Flectar Mail";
/// How long a sign-in in the browser has, as long as the server keeps its token.
const LOGIN_WAIT: Duration = Duration::from_secs(20 * 60);
const POLL_EVERY: Duration = Duration::from_secs(2);
/// The pieces a large file goes up in, where the server takes files in pieces: Nextcloud's
/// chunked upload takes pieces from 5 MB to 5 GB.
const CHUNK_BYTES: u64 = 10 * 1024 * 1024;
/// How long one upload request may take, a whole file or a piece of it.
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(60 * 60);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// The server's address, as `https://cloud.example.com/`, also from a pasted web or WebDAV
/// address, which names the server too.
pub(super) fn base_url(value: &str) -> Result<Url> {
    let value = value.trim();
    let value = if value.contains("://") {
        value.to_owned()
    } else {
        format!("https://{value}")
    };
    let mut url = validated_url(&value)?;
    for marker in ["/remote.php", "/index.php", "/ocs/", "/apps/", "/login"] {
        if let Some(at) = url.path().find(marker) {
            let path = url.path()[..at].to_owned();
            url.set_path(&path);
        }
    }
    url.set_query(None);
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    Ok(url)
}

/// A sign-in started in the browser: the page the user signs in on, and where the app asks
/// whether they did.
pub(super) struct Login {
    pub(super) login_url: String,
    pub(super) endpoint: String,
    pub(super) token: String,
}

/// What a finished sign-in gives: the server and an app password of the app's own.
pub(super) struct Grant {
    pub(super) server: String,
    pub(super) login_name: String,
    pub(super) app_password: String,
}

/// A storage server belongs to no account, so it goes through the app's
/// proxy.
fn anonymous_client() -> Result<reqwest::Client> {
    crate::net::proxy::http_builder(Owner::App)
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .user_agent(USER_AGENT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| err("Could not reach the server."))
}

/// Starts Nextcloud's sign-in in the browser, which gives the app a password of its own.
pub(super) async fn start_login(base: &Url) -> Result<Login> {
    let url = base
        .join("index.php/login/v2")
        .map_err(|_| err("Enter the server's address."))?;
    let response = anonymous_client()?
        .post(url)
        .send()
        .await
        .map_err(|_| err("Could not reach the server."))?;
    if !response.status().is_success() {
        return Err(err(
            "This server doesn't sign in from apps in the browser. Sign in with an app password instead.",
        ));
    }
    let body: Value = response
        .json()
        .await
        .map_err(|_| err("The server didn't answer as Nextcloud does."))?;
    let field = |value: &Value| value.as_str().map(str::to_owned);
    let (Some(login_url), Some(endpoint), Some(token)) = (
        field(&body["login"]),
        field(&body["poll"]["endpoint"]),
        field(&body["poll"]["token"]),
    ) else {
        return Err(err("The server didn't answer as Nextcloud does."));
    };
    validated_url(&login_url)?;
    validated_url(&endpoint)?;
    Ok(Login {
        login_url,
        endpoint,
        token,
    })
}

/// Waits for the user to finish signing in, for as long as the server keeps the sign-in.
pub(super) async fn finish_login(login: &Login) -> Result<Grant> {
    let client = anonymous_client()?;
    let deadline = Instant::now() + LOGIN_WAIT;
    while Instant::now() < deadline {
        let response = client
            .post(&login.endpoint)
            .form(&[("token", login.token.as_str())])
            .send()
            .await;
        match response {
            Ok(response) if response.status().is_success() => {
                let body: Value = response
                    .json()
                    .await
                    .map_err(|_| err("The server didn't answer as Nextcloud does."))?;
                let field = |key: &str| body[key].as_str().map(str::to_owned);
                return match (field("server"), field("loginName"), field("appPassword")) {
                    (Some(server), Some(login_name), Some(app_password)) => Ok(Grant {
                        server,
                        login_name,
                        app_password,
                    }),
                    _ => Err(err("The server didn't answer as Nextcloud does.")),
                };
            }
            // Not signed in yet.
            Ok(response) if response.status() == reqwest::StatusCode::NOT_FOUND => {}
            Ok(_) => return Err(err("The server refused the sign-in. Try again.")),
            // A dropped connection is retried until the sign-in runs out.
            Err(_) => {}
        }
        tokio::time::sleep(POLL_EVERY).await;
    }
    Err(err("Signing in took too long. Try again."))
}

/// What the server allows links to files to be.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Capabilities {
    pub(super) public_links: bool,
    pub(super) password_enforced: bool,
    /// The most days a link may work, where the server limits it.
    pub(super) max_expiry_days: Option<u32>,
    /// The server takes large files in pieces, as Nextcloud's chunked upload.
    pub(super) chunking: bool,
    /// Where the server makes passwords its policy accepts.
    pub(super) generate_password_url: Option<String>,
    /// What the server calls itself.
    pub(super) name: Option<String>,
}

/// A share the server made.
pub(super) struct Share {
    pub(super) id: String,
    pub(super) url: String,
    /// The day the link stops working, as the server gives it: "YYYY-MM-DD HH:MM:SS".
    pub(super) expiration: Option<String>,
}

/// One signed-in server.
pub(super) struct Client {
    transport: Transport,
    base: Url,
    /// The name WebDAV paths use, which may differ from the login name.
    user_id: String,
}

impl Client {
    pub(super) async fn connect(base: &Url, login_name: &str, app_password: &str) -> Result<Self> {
        let transport = Transport::new(Owner::App, base.as_str(), login_name, app_password)?;
        let mut client = Self {
            transport,
            base: base.clone(),
            user_id: String::new(),
        };
        let user = client.ocs("GET", "ocs/v1.php/cloud/user", &[]).await?;
        client.user_id = user["id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| err("The server didn't say who's signed in."))?
            .to_owned();
        Ok(client)
    }

    fn url(&self, path: &str) -> Result<Url> {
        self.base
            .join(path)
            .map_err(|_| err("The server's address isn't valid."))
    }

    /// An OCS call's `data`, or the server's reason it failed. Version 1 answers HTTP 200 and
    /// puts the outcome in `meta.statuscode`, 100 for success.
    async fn ocs(&self, method: &str, path: &str, form: &[(&str, &str)]) -> Result<Value> {
        let mut url = self.url(path)?;
        url.query_pairs_mut().append_pair("format", "json");
        let mut request = self
            .transport
            .request(method, url.as_str())?
            .timeout(REQUEST_TIMEOUT)
            .header("OCS-APIRequest", "true")
            .header("Accept", "application/json");
        if !form.is_empty() {
            request = request.form(form);
        }
        let response = request
            .send()
            .await
            .map_err(|_| err("Could not reach the server."))?;
        match response.status().as_u16() {
            401 => return Err(err("The server signed this app out. Connect it again.")),
            429 => {
                return Err(err(
                    "The server limits how many links can be made. Try again in a few minutes.",
                ));
            }
            _ => {}
        }
        let body: Value = response
            .json()
            .await
            .map_err(|_| err("The server didn't answer as Nextcloud does."))?;
        let code = body["ocs"]["meta"]["statuscode"].as_i64().unwrap_or(0);
        if code == 100 || code == 200 {
            return Ok(body["ocs"]["data"].clone());
        }
        let message = body["ocs"]["meta"]["message"]
            .as_str()
            .map(str::trim)
            .filter(|message| !message.is_empty())
            .unwrap_or("The server refused the request.");
        Err(err(message))
    }

    pub(super) async fn capabilities(&self) -> Result<Capabilities> {
        let data = self
            .ocs("GET", "ocs/v1.php/cloud/capabilities", &[])
            .await?;
        let capabilities = &data["capabilities"];
        let public = &capabilities["files_sharing"]["public"];
        let enforced = |value: &Value| value.as_bool().unwrap_or(false);
        let expire = &public["expire_date"];
        Ok(Capabilities {
            public_links: capabilities["files_sharing"]["api_enabled"]
                .as_bool()
                .unwrap_or(true)
                && enforced(&public["enabled"]),
            // ownCloud says which kinds of link need one; a link to read a file is one.
            password_enforced: enforced(&public["password"]["enforced"])
                || enforced(&public["password"]["enforced_for"]["read_only"]),
            max_expiry_days: enforced(&expire["enforced"])
                .then(|| {
                    expire["days"]
                        .as_u64()
                        .or_else(|| expire["days"].as_str().and_then(|days| days.parse().ok()))
                })
                .flatten()
                .and_then(|days| u32::try_from(days).ok())
                .filter(|days| *days > 0),
            chunking: enforced(&capabilities["files"]["bigfilechunking"])
                && capabilities["dav"]["chunking"].as_str() != Some("1.0"),
            generate_password_url: capabilities["password_policy"]["api"]["generate"]
                .as_str()
                .map(str::to_owned),
            name: capabilities["theming"]["name"]
                .as_str()
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_owned),
        })
    }

    /// A password the server's policy accepts, where it makes them.
    pub(super) async fn generate_password(&self, url: &str) -> Option<String> {
        let url = validated_url(url).ok()?;
        let response = self
            .transport
            .request("GET", url.as_str())
            .ok()?
            .timeout(REQUEST_TIMEOUT)
            .header("OCS-APIRequest", "true")
            .header("Accept", "application/json")
            .send()
            .await
            .ok()?;
        let body: Value = response.json().await.ok()?;
        body["ocs"]["data"]["password"]
            .as_str()
            .filter(|password| !password.is_empty())
            .map(str::to_owned)
    }

    fn dav_url(&self, path: &str) -> Result<Url> {
        let mut url = self.url("remote.php/dav/files/")?;
        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|_| err("The server's address isn't valid."))?;
            segments.pop_if_empty().push(&self.user_id);
            for segment in path.split('/').filter(|segment| !segment.is_empty()) {
                segments.push(segment);
            }
        }
        Ok(url)
    }

    /// Makes the folder at `path`, which may be there already.
    async fn make_folder(&self, path: &str) -> Result<()> {
        let url = self.dav_url(path)?;
        let response = self
            .transport
            .request("MKCOL", url.as_str())?
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|_| err("Could not reach the server."))?;
        match response.status().as_u16() {
            // 405: the folder is there.
            201 | 405 => Ok(()),
            status => Err(dav_error(status)),
        }
    }

    /// Uploads the file at `local` as `name` in the folder at `folder`, which it makes, and
    /// returns its size.
    pub(super) async fn upload(
        &self,
        local: &Path,
        folder: &str,
        name: &str,
        chunking: bool,
    ) -> Result<u64> {
        let mut path = String::new();
        for segment in folder.split('/').filter(|segment| !segment.is_empty()) {
            path.push('/');
            path.push_str(segment);
            self.make_folder(&path).await?;
        }
        let target = self.dav_url(&format!("{folder}/{name}"))?;
        let size = tokio::fs::metadata(local).await?.len();
        if chunking && size > CHUNK_BYTES {
            self.upload_in_chunks(local, size, &target).await?;
        } else {
            let (body, length) = crate::files::upload_body(local, usize::MAX).await?;
            let response = self
                .transport
                .request("PUT", target.as_str())?
                .timeout(UPLOAD_TIMEOUT)
                .header("Content-Type", "application/octet-stream")
                .header("Content-Length", length)
                .body(body)
                .send()
                .await
                .map_err(|_| err("The upload stopped. Try again."))?;
            if !response.status().is_success() {
                return Err(dav_error(response.status().as_u16()));
            }
        }
        Ok(size)
    }

    /// Nextcloud's chunked upload: the pieces go into an upload folder of their own, and
    /// moving its `.file` puts the whole file at `target`.
    async fn upload_in_chunks(&self, local: &Path, size: u64, target: &Url) -> Result<()> {
        use tokio::io::AsyncReadExt;
        let mut uploads = self.url("remote.php/dav/uploads/")?;
        let id = format!("flectar-{}", super::random_token(16));
        uploads
            .path_segments_mut()
            .map_err(|_| err("The server's address isn't valid."))?
            .pop_if_empty()
            .push(&self.user_id)
            .push(&id);
        let destination = target.as_str().to_owned();
        let total = size.to_string();
        let response = self
            .transport
            .request("MKCOL", uploads.as_str())?
            .timeout(REQUEST_TIMEOUT)
            .header("Destination", &destination)
            .send()
            .await
            .map_err(|_| err("Could not reach the server."))?;
        if !response.status().is_success() {
            return Err(dav_error(response.status().as_u16()));
        }
        let result = async {
            let mut file = tokio::fs::File::open(local).await?;
            let progress = crate::files::progress::begin(1, size);
            let mut sent = 0u64;
            let mut number = 1u32;
            while sent < size {
                let length = CHUNK_BYTES.min(size - sent);
                let mut chunk = vec![0u8; usize::try_from(length).unwrap_or(usize::MAX)];
                file.read_exact(&mut chunk).await?;
                let mut url = uploads.clone();
                url.path_segments_mut()
                    .map_err(|_| err("The server's address isn't valid."))?
                    .push(&format!("{number:05}"));
                let response = self
                    .transport
                    .request("PUT", url.as_str())?
                    .timeout(UPLOAD_TIMEOUT)
                    .header("Destination", &destination)
                    .header("OC-Total-Length", &total)
                    .body(chunk)
                    .send()
                    .await
                    .map_err(|_| err("The upload stopped. Try again."))?;
                if !response.status().is_success() {
                    return Err(dav_error(response.status().as_u16()));
                }
                sent += length;
                number += 1;
                if let Some(progress) = &progress {
                    progress.advance(length);
                }
            }
            let mut assembled = uploads.clone();
            assembled
                .path_segments_mut()
                .map_err(|_| err("The server's address isn't valid."))?
                .push(".file");
            let response = self
                .transport
                .request("MOVE", assembled.as_str())?
                .timeout(UPLOAD_TIMEOUT)
                .header("Destination", &destination)
                .header("OC-Total-Length", &total)
                .send()
                .await
                .map_err(|_| err("The upload stopped. Try again."))?;
            if !response.status().is_success() {
                return Err(dav_error(response.status().as_u16()));
            }
            Ok(())
        }
        .await;
        if result.is_err() {
            // An unfinished upload's pieces go, as the server would after a day.
            if let Ok(request) = self.transport.request("DELETE", uploads.as_str()) {
                let _ = request.timeout(REQUEST_TIMEOUT).send().await;
            }
        }
        result
    }

    /// Shares the file at `path` by a public link that only reads it, working until
    /// `expire_date` (`YYYY-MM-DD`) and asking for `password`, if they're given.
    pub(super) async fn share(
        &self,
        path: &str,
        expire_date: Option<&str>,
        password: Option<&str>,
    ) -> Result<Share> {
        let mut form = vec![("path", path), ("shareType", "3"), ("permissions", "1")];
        if let Some(date) = expire_date {
            form.push(("expireDate", date));
        }
        if let Some(password) = password {
            form.push(("password", password));
        }
        let data = self
            .ocs("POST", "ocs/v1.php/apps/files_sharing/api/v1/shares", &form)
            .await?;
        let id = match &data["id"] {
            Value::String(id) => id.clone(),
            Value::Number(id) => id.to_string(),
            _ => return Err(err("The server didn't say which link it made.")),
        };
        let url = data["url"]
            .as_str()
            .ok_or_else(|| err("The server didn't give the link."))?;
        validated_url(url)?;
        Ok(Share {
            id,
            url: url.to_owned(),
            expiration: data["expiration"]
                .as_str()
                .filter(|date| !date.is_empty())
                .map(str::to_owned),
        })
    }

    /// Takes back the share with `id`.
    pub(super) async fn unshare(&self, id: &str) -> Result<()> {
        let path = format!(
            "ocs/v1.php/apps/files_sharing/api/v1/shares/{}",
            url::form_urlencoded::byte_serialize(id.as_bytes()).collect::<String>()
        );
        self.ocs("DELETE", &path, &[]).await.map(|_| ())
    }

    /// Deletes the folder at `path` with what's in it.
    pub(super) async fn delete(&self, path: &str) -> Result<()> {
        let url = self.dav_url(path)?;
        let response = self
            .transport
            .request("DELETE", url.as_str())?
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|_| err("Could not reach the server."))?;
        match response.status().as_u16() {
            200..=299 | 404 => Ok(()),
            status => Err(dav_error(status)),
        }
    }
}

fn dav_error(status: u16) -> crate::error::CoreError {
    err(match status {
        401 => "The server signed this app out. Connect it again.",
        403 => "The server doesn't allow this upload.",
        413 => "The file is bigger than the server takes.",
        423 => "The folder is locked on the server.",
        507 => "There's no room left on the server for this file.",
        _ => "The server refused the upload.",
    })
}
