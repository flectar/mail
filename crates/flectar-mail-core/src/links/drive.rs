//! Google Drive: a file goes up by a resumable upload into a Mail attachments folder the app
//! makes, and is shared by a link anyone with it can open, which Drive can't make expire.

use super::err;
use crate::error::Result;
use crate::net::proxy::Owner;
use serde_json::{Value, json};
use std::path::Path;
use std::time::Duration;
use url::Url;

pub(super) const API: &str = "https://www.googleapis.com/";
/// The pieces a file goes up in: Google takes multiples of 256 KiB.
const CHUNK_BYTES: u64 = 32 * 256 * 1024;
const FOLDER_TYPE: &str = "application/vnd.google-apps.folder";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(60 * 60);

/// Files going up at once look for the app's folder one at a time, so the first ones don't
/// each make one.
static FOLDER_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// One account's Google Drive, signed in to with its access token.
pub(super) struct Drive {
    http: reqwest::Client,
    api: Url,
    token: String,
}

impl Drive {
    /// Requests go through the proxy of `owner`, the account the Drive is.
    pub(super) fn new(owner: Owner, api: &str, token: String) -> Result<Self> {
        Ok(Self {
            http: crate::net::proxy::http_builder(owner)
                .connect_timeout(Duration::from_secs(10))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|_| err("Could not reach Google Drive."))?,
            api: Url::parse(api).map_err(|_| err("Could not reach Google Drive."))?,
            token,
        })
    }

    fn url(&self, path: &str) -> Result<Url> {
        self.api
            .join(path)
            .map_err(|_| err("Could not reach Google Drive."))
    }

    /// A Drive API call's JSON answer, or what Drive said went wrong.
    async fn json(&self, request: reqwest::RequestBuilder) -> Result<Value> {
        let response = request
            .bearer_auth(&self.token)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|_| err("Could not reach Google Drive."))?;
        let status = response.status();
        let body: Value = response.json().await.unwrap_or(Value::Null);
        if status.is_success() {
            return Ok(body);
        }
        Err(drive_error(status.as_u16(), &body))
    }

    /// The id of the app's folder named `name`, if there's one. The app sees only what it
    /// made, so another folder of that name isn't it.
    pub(super) async fn find_folder(&self, name: &str) -> Result<Option<String>> {
        let mut url = self.url("drive/v3/files")?;
        url.query_pairs_mut()
            .append_pair(
                "q",
                &format!(
                    "name = '{}' and mimeType = '{FOLDER_TYPE}' and trashed = false",
                    name.replace('\\', "\\\\").replace('\'', "\\'")
                ),
            )
            .append_pair("spaces", "drive")
            .append_pair("fields", "files(id)");
        let found = self.json(self.http.get(url)).await?;
        Ok(found["files"][0]["id"].as_str().map(str::to_owned))
    }

    /// The id of the app's folder named `name`, which it makes the first time.
    pub(super) async fn folder(&self, name: &str) -> Result<String> {
        let _looking = FOLDER_LOCK.lock().await;
        if let Some(id) = self.find_folder(name).await? {
            return Ok(id);
        }
        let mut url = self.url("drive/v3/files")?;
        url.query_pairs_mut().append_pair("fields", "id");
        let made = self
            .json(
                self.http
                    .post(url)
                    .json(&json!({"name": name, "mimeType": FOLDER_TYPE})),
            )
            .await?;
        made["id"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| err("Google Drive didn't make the folder."))
    }

    /// Uploads the file at `local` as `name` into the folder with id `folder`, returning the
    /// new file's id and its size.
    pub(super) async fn upload(
        &self,
        local: &Path,
        name: &str,
        folder: &str,
    ) -> Result<(String, u64)> {
        use tokio::io::{AsyncReadExt, AsyncSeekExt};
        let size = tokio::fs::metadata(local).await?.len();
        let mut url = self.url("upload/drive/v3/files")?;
        url.query_pairs_mut()
            .append_pair("uploadType", "resumable")
            .append_pair("fields", "id");
        let response = self
            .http
            .post(url)
            .bearer_auth(&self.token)
            .timeout(REQUEST_TIMEOUT)
            .header("X-Upload-Content-Type", "application/octet-stream")
            .header("X-Upload-Content-Length", size)
            .json(&json!({"name": name, "parents": [folder]}))
            .send()
            .await
            .map_err(|_| err("Could not reach Google Drive."))?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body: Value = response.json().await.unwrap_or(Value::Null);
            return Err(drive_error(status, &body));
        }
        let session = response
            .headers()
            .get("Location")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| Url::parse(value).ok())
            .filter(|url| url.scheme() == "https" || self.api.scheme() == "http")
            .ok_or_else(|| err("Google Drive didn't start the upload."))?;
        let mut file = tokio::fs::File::open(local).await?;
        let progress = crate::files::progress::begin(1, size);
        let mut start = 0u64;
        loop {
            let length = CHUNK_BYTES.min(size - start);
            let mut chunk = vec![0u8; usize::try_from(length).unwrap_or(usize::MAX)];
            file.read_exact(&mut chunk).await?;
            let range = if size == 0 {
                "bytes */0".to_owned()
            } else {
                format!("bytes {start}-{}/{size}", start + length - 1)
            };
            // The session's address authorizes the pieces, which carry no token: one that
            // runs out during a long upload would stop it.
            let response = self
                .http
                .put(session.clone())
                .timeout(UPLOAD_TIMEOUT)
                .header("Content-Range", range)
                .body(chunk)
                .send()
                .await
                .map_err(|_| err("The upload stopped. Try again."))?;
            match response.status().as_u16() {
                // Resume Incomplete: Range says how much arrived, and none did without it.
                308 => {
                    let received = response
                        .headers()
                        .get("Range")
                        .and_then(|value| value.to_str().ok())
                        .and_then(|range| range.rsplit('-').next())
                        .and_then(|end| end.parse::<u64>().ok())
                        .map_or(0, |end| end + 1);
                    if received <= start {
                        return Err(err("Google Drive didn't take the upload. Try again."));
                    }
                    if let Some(progress) = &progress {
                        progress.advance(received - start);
                    }
                    if received != start + length {
                        file.seek(std::io::SeekFrom::Start(received)).await?;
                    }
                    start = received;
                }
                200 | 201 => {
                    if let Some(progress) = &progress {
                        progress.advance(size - start);
                    }
                    let body: Value = response
                        .json()
                        .await
                        .map_err(|_| err("Google Drive didn't say which file it made."))?;
                    let id = body["id"]
                        .as_str()
                        .ok_or_else(|| err("Google Drive didn't say which file it made."))?;
                    return Ok((id.to_owned(), size));
                }
                status => {
                    let body: Value = response.json().await.unwrap_or(Value::Null);
                    return Err(drive_error(status, &body));
                }
            }
        }
    }

    /// Lets anyone with the link read the file with id `id`, and returns the link.
    pub(super) async fn share(&self, id: &str) -> Result<String> {
        let mut url = self.url("drive/v3/files/")?;
        url.path_segments_mut()
            .map_err(|_| err("Could not reach Google Drive."))?
            .pop_if_empty()
            .push(id)
            .push("permissions");
        self.json(
            self.http
                .post(url)
                .json(&json!({"role": "reader", "type": "anyone"})),
        )
        .await?;
        let mut url = self.url("drive/v3/files/")?;
        url.path_segments_mut()
            .map_err(|_| err("Could not reach Google Drive."))?
            .pop_if_empty()
            .push(id);
        url.query_pairs_mut().append_pair("fields", "webViewLink");
        let file = self.json(self.http.get(url)).await?;
        file["webViewLink"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| err("Google Drive didn't give the link."))
    }

    /// Deletes the file with id `id`, and its link with it.
    pub(super) async fn delete(&self, id: &str) -> Result<()> {
        let mut url = self.url("drive/v3/files/")?;
        url.path_segments_mut()
            .map_err(|_| err("Could not reach Google Drive."))?
            .pop_if_empty()
            .push(id);
        let response = self
            .http
            .delete(url)
            .bearer_auth(&self.token)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|_| err("Could not reach Google Drive."))?;
        match response.status().as_u16() {
            200..=299 | 404 => Ok(()),
            status => {
                let body: Value = response.json().await.unwrap_or(Value::Null);
                Err(drive_error(status, &body))
            }
        }
    }
}

/// What Drive's error answer means for the sender.
fn drive_error(status: u16, body: &Value) -> crate::error::CoreError {
    let reasons = body["error"]["errors"]
        .as_array()
        .map(|errors| {
            errors
                .iter()
                .filter_map(|error| error["reason"].as_str())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if status == 401 {
        return err("Google signed this app out of Drive. Turn Google Drive on again in Settings.");
    }
    let scope_missing = body["error"]["details"].as_array().is_some_and(|details| {
        details
            .iter()
            .any(|detail| detail["reason"] == "ACCESS_TOKEN_SCOPE_INSUFFICIENT")
    });
    if scope_missing || reasons.contains(&"insufficientPermissions") {
        return err(
            "Google didn't let this app reach your Drive. Turn Google Drive on again in Settings, and allow it.",
        );
    }
    if reasons.contains(&"accessNotConfigured") {
        return err(
            "The Google Drive API is off for this app's Google Cloud project. Turn it on there, then try again.",
        );
    }
    // A Workspace organization's sharing settings refuse the link: "ACL change not allowed."
    if reasons.contains(&"invalidSharingRequest") {
        return err("Your organization doesn't allow links that anyone can open.");
    }
    if reasons.contains(&"domainPolicy") {
        return err("Your organization doesn't let apps reach Google Drive.");
    }
    if reasons.contains(&"storageQuotaExceeded") {
        return err("There's no room left in your Google Drive for this file.");
    }
    match body["error"]["message"].as_str().map(str::trim) {
        Some(message) if !message.is_empty() => err(message),
        _ => err("Google Drive refused the request."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn error(status: u16, reason: &str, message: &str) -> String {
        drive_error(
            status,
            &json!({"error": {"errors": [{"reason": reason, "message": message}], "code": status, "message": message}}),
        )
        .to_string()
    }

    #[test]
    fn drive_errors_say_what_to_do() {
        assert!(
            error(403, "insufficientPermissions", "Insufficient Permission")
                .contains("Turn Google Drive on again in Settings, and allow it")
        );
        let scope_missing = drive_error(
            403,
            &json!({"error": {"code": 403, "message": "Request had insufficient authentication scopes.",
                "details": [{"reason": "ACCESS_TOKEN_SCOPE_INSUFFICIENT"}]}}),
        );
        assert!(scope_missing.to_string().contains("and allow it"));
        assert!(
            error(
                403,
                "accessNotConfigured",
                "Google Drive API has not been used in project 1"
            )
            .contains("The Google Drive API is off")
        );
        assert!(
            error(
                403,
                "domainPolicy",
                "The domain administrators have disabled Drive apps."
            )
            .contains("doesn't let apps reach Google Drive")
        );
        assert!(
            error(
                403,
                "storageQuotaExceeded",
                "The user's Drive storage quota has been exceeded."
            )
            .contains("no room left")
        );
        assert_eq!(
            error(403, "userRateLimitExceeded", "Slow down"),
            "Slow down"
        );
    }
}
