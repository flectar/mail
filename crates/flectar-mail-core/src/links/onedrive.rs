//! OneDrive, through Microsoft Graph: a file goes up by an upload session into a folder of its
//! own in the app's Mail attachments folder, and is shared by a view link anyone with it can
//! open.

use super::err;
use crate::error::Result;
use crate::net::proxy::Owner;
use serde_json::{Value, json};
use std::path::Path;
use std::time::Duration;
use url::Url;

pub(super) const API: &str = "https://graph.microsoft.com/v1.0/";
/// The pieces a file goes up in: Graph takes multiples of 320 KiB, under 60 MiB.
const CHUNK_BYTES: u64 = 32 * 320 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(60 * 60);

/// Files going up at once look for the app's folder one at a time, so the first ones don't
/// each make one.
static FOLDER_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// One account's OneDrive, signed in to with a Graph access token.
pub(super) struct OneDrive {
    http: reqwest::Client,
    api: Url,
    token: String,
}

impl OneDrive {
    /// Requests go through the proxy of `owner`, the account the OneDrive is.
    pub(super) fn new(owner: Owner, api: &str, token: String) -> Result<Self> {
        Ok(Self {
            http: crate::net::proxy::http_builder(owner)
                .connect_timeout(Duration::from_secs(10))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|_| err("Could not reach OneDrive."))?,
            api: Url::parse(api).map_err(|_| err("Could not reach OneDrive."))?,
            token,
        })
    }

    /// `me/drive/` followed by `segments`, each encoded as a path segment.
    fn url(&self, segments: &[&str]) -> Result<Url> {
        let mut url = self
            .api
            .join("me/drive/")
            .map_err(|_| err("Could not reach OneDrive."))?;
        url.path_segments_mut()
            .map_err(|_| err("Could not reach OneDrive."))?
            .pop_if_empty()
            .extend(segments);
        Ok(url)
    }

    /// A Graph call's JSON answer, or its status and error answer.
    async fn call(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<std::result::Result<Value, (u16, Value)>> {
        let response = request
            .bearer_auth(&self.token)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|_| err("Could not reach OneDrive."))?;
        let status = response.status();
        let body: Value = response.json().await.unwrap_or(Value::Null);
        if status.is_success() {
            Ok(Ok(body))
        } else {
            Ok(Err((status.as_u16(), body)))
        }
    }

    /// A Graph call's JSON answer, or what Graph said went wrong.
    async fn json(&self, request: reqwest::RequestBuilder) -> Result<Value> {
        self.call(request)
            .await?
            .map_err(|(status, body)| graph_error(status, &body))
    }

    /// Checks that the account has a OneDrive the app can reach.
    pub(super) async fn check(&self) -> Result<()> {
        let mut url = self.url(&[])?;
        url.query_pairs_mut().append_pair("$select", "id");
        match self.call(self.http.get(url)).await? {
            Ok(_) => Ok(()),
            Err((404, _)) => Err(err("This Microsoft account has no OneDrive.")),
            Err((status, body)) => Err(graph_error(status, &body)),
        }
    }

    /// The id of the folder named `name` at the top of the drive, which the app makes the
    /// first time.
    pub(super) async fn folder(&self, name: &str) -> Result<String> {
        let _looking = FOLDER_LOCK.lock().await;
        for _ in 0..2 {
            match self
                .call(self.http.get(self.url(&["root:", name])?))
                .await?
            {
                Ok(item) => return item_id(&item),
                Err((404, _)) => {}
                Err((status, body)) => return Err(graph_error(status, &body)),
            }
            let made = self
                .call(
                    self.http
                        .post(self.url(&["root", "children"])?)
                        .json(&json!({
                            "name": name,
                            "folder": {},
                            "@microsoft.graph.conflictBehavior": "fail",
                        })),
                )
                .await?;
            match made {
                Ok(item) => return item_id(&item),
                // Made elsewhere meanwhile, which the next look finds.
                Err((409, _)) => {}
                Err((status, body)) => return Err(graph_error(status, &body)),
            }
        }
        Err(err("OneDrive didn't make the folder."))
    }

    /// Makes a folder named `name` in the folder with id `parent`, and returns its id.
    pub(super) async fn make_folder(&self, parent: &str, name: &str) -> Result<String> {
        let url = self.url(&["items", parent, "children"])?;
        let item = self
            .json(self.http.post(url).json(&json!({
                "name": name,
                "folder": {},
                "@microsoft.graph.conflictBehavior": "rename",
            })))
            .await?;
        item_id(&item)
    }

    /// Uploads the file at `local` as `name` into the folder with id `folder`, and returns the
    /// new file's id and its size.
    pub(super) async fn upload(
        &self,
        local: &Path,
        folder: &str,
        name: &str,
    ) -> Result<(String, u64)> {
        let size = tokio::fs::metadata(local).await?.len();
        let parent = format!("{folder}:");
        let name = format!("{}:", file_name(name));
        let item = if size == 0 {
            // An upload session takes no empty file: it goes in one request.
            let url = self.url(&["items", &parent, &name, "content"])?;
            self.json(self.http.put(url).body(Vec::new())).await?
        } else {
            let url = self.url(&["items", &parent, &name, "createUploadSession"])?;
            let session = self
                .json(
                    self.http
                        .post(url)
                        .json(&json!({"item": {"@microsoft.graph.conflictBehavior": "rename"}})),
                )
                .await?;
            let upload_url = session["uploadUrl"]
                .as_str()
                .and_then(|value| Url::parse(value).ok())
                .filter(|url| url.scheme() == "https" || self.api.scheme() == "http")
                .ok_or_else(|| err("OneDrive didn't start the upload."))?;
            match self.send_pieces(local, size, &upload_url).await {
                Ok(item) => item,
                Err(error) => {
                    // What went up of it goes, rather than waiting for the session to end.
                    let _ = self
                        .http
                        .delete(upload_url)
                        .timeout(REQUEST_TIMEOUT)
                        .send()
                        .await;
                    return Err(error);
                }
            }
        };
        Ok((item_id(&item)?, size))
    }

    /// Sends the `size` bytes of the file at `local` to the upload session at `upload_url`,
    /// and returns the item they made.
    async fn send_pieces(&self, local: &Path, size: u64, upload_url: &Url) -> Result<Value> {
        use tokio::io::{AsyncReadExt, AsyncSeekExt};
        let mut file = tokio::fs::File::open(local).await?;
        let progress = crate::files::progress::begin(1, size);
        let mut start = 0u64;
        loop {
            let length = CHUNK_BYTES.min(size - start);
            let mut chunk = vec![0u8; usize::try_from(length).unwrap_or(usize::MAX)];
            file.read_exact(&mut chunk).await?;
            // The upload address authorizes the pieces. A token with them can be refused.
            let response = self
                .http
                .put(upload_url.clone())
                .timeout(UPLOAD_TIMEOUT)
                .header(
                    "Content-Range",
                    format!("bytes {start}-{}/{size}", start + length - 1),
                )
                .body(chunk)
                .send()
                .await
                .map_err(|_| err("The upload stopped. Try again."))?;
            let status = response.status().as_u16();
            let body: Value = response.json().await.unwrap_or(Value::Null);
            match status {
                // More to send, from where the session says.
                202 => {
                    let next = body["nextExpectedRanges"][0]
                        .as_str()
                        .and_then(|range| range.split('-').next())
                        .and_then(|first| first.parse::<u64>().ok())
                        .unwrap_or(start + length);
                    if next <= start || next >= size {
                        return Err(err("OneDrive didn't take the upload. Try again."));
                    }
                    if let Some(progress) = &progress {
                        progress.advance(next - start);
                    }
                    if next != start + length {
                        file.seek(std::io::SeekFrom::Start(next)).await?;
                    }
                    start = next;
                }
                200 | 201 => {
                    if let Some(progress) = &progress {
                        progress.advance(size - start);
                    }
                    return Ok(body);
                }
                status => return Err(graph_error(status, &body)),
            }
        }
    }

    /// Lets anyone with the link view the item with id `id`, until the end of `expires_on`
    /// (`YYYY-MM-DD`) where the account lets links expire, and returns the link with the day
    /// it expires, if it does.
    pub(super) async fn share(
        &self,
        id: &str,
        expires_on: Option<&str>,
    ) -> Result<(String, Option<String>)> {
        let url = self.url(&["items", id, "createLink"])?;
        let anyone = json!({"type": "view", "scope": "anonymous"});
        let mut body = anyone.clone();
        if let Some(date) = expires_on {
            body["expirationDateTime"] = json!(format!("{date}T23:59:59Z"));
        }
        let mut answer = self.call(self.http.post(url.clone()).json(&body)).await?;
        let mut expires_on = expires_on.map(str::to_owned);
        if answer.is_err() && expires_on.is_some() {
            // Accounts without Microsoft 365 can't make links that expire.
            answer = self.call(self.http.post(url).json(&anyone)).await?;
            expires_on = None;
        }
        let created = answer.map_err(|(status, body)| match body["error"]["code"].as_str() {
            Some("accessDenied" | "notAllowed") => {
                err("Your organization doesn't allow links that anyone can open.")
            }
            _ => graph_error(status, &body),
        })?;
        let link = created["link"]["webUrl"]
            .as_str()
            .ok_or_else(|| err("OneDrive didn't give the link."))?;
        Ok((link.to_owned(), expires_on))
    }

    /// Deletes the item with id `id`, a file or a folder with what's in it, and its links.
    pub(super) async fn delete(&self, id: &str) -> Result<()> {
        let url = self.url(&["items", id])?;
        let response = self
            .http
            .delete(url)
            .bearer_auth(&self.token)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|_| err("Could not reach OneDrive."))?;
        match response.status().as_u16() {
            200..=299 | 404 => Ok(()),
            status => {
                let body: Value = response.json().await.unwrap_or(Value::Null);
                Err(graph_error(status, &body))
            }
        }
    }
}

/// The id of the item Graph answered with.
fn item_id(item: &Value) -> Result<String> {
    item["id"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| err("OneDrive didn't say which file it made."))
}

/// `name` as OneDrive takes it, which refuses some characters in names, and names that end in
/// a dot or a space.
fn file_name(name: &str) -> String {
    let name = name
        .chars()
        .map(|character| match character {
            '"' | '*' | ':' | '<' | '>' | '?' | '/' | '\\' | '|' => '_',
            character if character.is_control() => '_',
            character => character,
        })
        .collect::<String>();
    let kept = name.trim_end_matches(['.', ' ']).trim_start();
    if kept.is_empty() {
        "file".to_owned()
    } else {
        kept.to_owned()
    }
}

/// What Graph's error answer means for the sender.
fn graph_error(status: u16, body: &Value) -> crate::error::CoreError {
    let code = body["error"]["code"].as_str().unwrap_or_default();
    match (status, code) {
        (401, _) => {
            err("Microsoft signed this app out of OneDrive. Turn OneDrive on again in Settings.")
        }
        (403, "accessDenied") => err(
            "Microsoft didn't let this app reach your OneDrive. Turn OneDrive on again in Settings, and allow it.",
        ),
        (507, _) | (_, "quotaLimitReached") => {
            err("There's no room left in your OneDrive for this file.")
        }
        _ => match body["error"]["message"].as_str().map(str::trim) {
            Some(message) if !message.is_empty() => err(message),
            _ => err("OneDrive refused the request."),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_onedrive_refuses_is_made_one_it_takes() {
        assert_eq!(file_name("Q3: plan?.pdf"), "Q3_ plan_.pdf");
        assert_eq!(file_name("a|b<c>d*\"e\\f.txt"), "a_b_c_d__e_f.txt");
        assert_eq!(file_name(" notes. . "), "notes");
        assert_eq!(file_name("..."), "file");
        assert_eq!(
            file_name("Informe 2026 – España.docx"),
            "Informe 2026 – España.docx"
        );
    }

    #[test]
    fn graph_errors_say_what_to_do() {
        let error = |status, code: &str| {
            graph_error(
                status,
                &json!({"error": {"code": code, "message": "Graph says"}}),
            )
            .to_string()
        };
        assert!(error(401, "InvalidAuthenticationToken").contains("Turn OneDrive on again"));
        assert!(error(403, "accessDenied").contains("didn't let this app reach your OneDrive"));
        assert!(error(507, "quotaLimitReached").contains("no room left"));
        assert_eq!(error(400, "invalidRequest"), "Graph says");
    }
}
