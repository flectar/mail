//! The user's Sieve scripts over JMAP (RFC 9661), on servers such as
//! Stalwart and Cyrus. The app keeps its automatic replies (#239) and rules
//! (#238) there in a script of its own, as it does over ManageSieve. Those
//! servers keep their own VacationResponse as a script too, which would turn
//! the user's filters off.

use super::client::{ConnectedClient, map_error};
use crate::error::{CoreError, Result};
use crate::sieve::Scripts;
use crate::sieve::managesieve::Script;
use futures::StreamExt;
use jmap_client::core::response::{SieveScriptGetResponse, SieveScriptSetResponse};
use jmap_client::core::session::Capabilities;
use jmap_client::core::set::{SetErrorType, SetObject};
use std::collections::HashMap;

const SIEVE_CAPABILITY: &str = "urn:ietf:params:jmap:sieve";
/// Bound for a script read back.
const MOST_BYTES: usize = 1024 * 1024;

/// An account's scripts on its JMAP server.
pub(crate) struct JmapScripts<'a> {
    connected: &'a ConnectedClient,
    /// The extensions scripts may use.
    extensions: Vec<String>,
    /// The Sieve implementation's name and version, if the server gave one.
    implementation: String,
    /// Each script's id and blob, by name, as last listed.
    known: HashMap<String, (String, String)>,
}

/// The account's scripts, when its server runs Sieve scripts.
pub(crate) fn scripts(connected: &ConnectedClient) -> Option<JmapScripts<'_>> {
    let session = connected.client.session();
    let account = session.account(&connected.account_id)?;
    let listed = account.capability(SIEVE_CAPABILITY)?;
    // RFC 9661 lists the extensions with the account. Earlier drafts, which
    // some servers still follow, listed them with the session.
    let sieve = match listed {
        Capabilities::Sieve(sieve) => sieve,
        _ => session.sieve_capabilities()?,
    };
    // The client keeps the implementation without giving it out.
    let implementation = serde_json::to_value(sieve)
        .ok()
        .and_then(|value| Some(value.get("implementation")?.as_str()?.to_owned()))
        .unwrap_or_default();
    Some(JmapScripts {
        connected,
        extensions: sieve.sieve_extensions().to_vec(),
        implementation,
        known: HashMap::new(),
    })
}

/// The account's scripts, when its server runs Sieve scripts that can send
/// automatic replies.
pub(crate) fn replies(connected: &ConnectedClient) -> Option<JmapScripts<'_>> {
    scripts(connected).filter(|scripts| scripts.has_extension("vacation"))
}

impl Scripts for JmapScripts<'_> {
    fn has_extension(&self, name: &str) -> bool {
        self.extensions
            .iter()
            .any(|extension| extension.eq_ignore_ascii_case(name))
    }

    fn implementation(&self) -> &str {
        &self.implementation
    }

    async fn list(&mut self) -> Result<Vec<Script>> {
        let mut request = self.connected.request();
        request
            .get_sieve_script()
            .account_id(&self.connected.account_id);
        let mut response = self
            .connected
            .send_single::<SieveScriptGetResponse>(request)
            .await
            .map_err(map_error)?;
        self.known.clear();
        let mut scripts = Vec::new();
        for script in response.take_list() {
            let (Some(id), Some(name), Some(blob)) = (script.id(), script.name(), script.blob_id())
            else {
                continue;
            };
            self.known
                .insert(name.to_owned(), (id.to_owned(), blob.to_owned()));
            scripts.push(Script {
                name: name.to_owned(),
                active: script.is_active(),
            });
        }
        Ok(scripts)
    }

    async fn get(&mut self, name: &str) -> Result<Option<String>> {
        let Some((_, blob)) = self.known.get(name) else {
            return Ok(None);
        };
        let url = self
            .connected
            .blob_url(blob, "script.sieve", "application/sieve");
        let response = self.connected.get(url).send().await.map_err(|error| {
            CoreError::Network(format!(
                "JMAP script download failed: {}",
                super::client::transport_failure(&error)
            ))
        })?;
        let status = response.status();
        if matches!(status.as_u16(), 401 | 403) {
            return Err(CoreError::Auth(
                "JMAP rejected credentials while downloading a filter script".into(),
            ));
        }
        if status.as_u16() == 404 {
            return Ok(None);
        }
        if !status.is_success() {
            return Err(CoreError::Jmap(format!(
                "JMAP script download returned HTTP {status}"
            )));
        }
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| {
                CoreError::Network(format!(
                    "JMAP script download failed: {}",
                    super::client::transport_failure(&error)
                ))
            })?;
            if bytes.len() + chunk.len() > MOST_BYTES {
                return Err(CoreError::Jmap("a filter script is too large".into()));
            }
            bytes.extend_from_slice(&chunk);
        }
        String::from_utf8(bytes)
            .map(Some)
            .map_err(|_| CoreError::Jmap("a filter script isn't UTF-8".into()))
    }

    async fn put(&mut self, name: &str, text: &str) -> Result<()> {
        let blob = self
            .connected
            .upload(text.as_bytes().to_vec(), "application/sieve")
            .await
            .map_err(map_error)?
            .take_blob_id();
        let mut request = self.connected.request();
        let set = request.set_sieve_script();
        set.account_id(&self.connected.account_id);
        let id = if let Some((id, _)) = self.known.get(name) {
            let id = id.clone();
            set.update(&id).blob_id(&blob);
            self.send(request).await?.updated(&id).map_err(refused)?;
            id
        } else {
            let create = set
                .create()
                .name(name)
                .blob_id(&blob)
                .create_id()
                .ok_or_else(|| CoreError::Jmap("no id to create a script with".into()))?;
            let created = self
                .send(request)
                .await?
                .created(&create)
                .map_err(refused)?;
            created
                .id()
                .map(str::to_owned)
                .ok_or_else(|| CoreError::Jmap("the server gave the script no id".into()))?
        };
        self.known.insert(name.to_owned(), (id, blob));
        Ok(())
    }

    async fn activate(&mut self, name: Option<&str>) -> Result<()> {
        let mut request = self.connected.request();
        let set = request.set_sieve_script();
        set.account_id(&self.connected.account_id);
        match name {
            Some(name) => {
                let (id, _) = self
                    .known
                    .get(name)
                    .ok_or_else(|| CoreError::NotFound(format!("filter script {name}")))?;
                set.arguments().on_success_activate_script_id(id.as_str());
            }
            None => {
                set.arguments().on_success_deactivate_script(true);
            }
        }
        self.send(request)
            .await?
            .unwrap_update_errors()
            .map_err(refused)
    }
}

impl JmapScripts<'_> {
    async fn send(
        &self,
        request: jmap_client::core::request::Request<'_>,
    ) -> Result<SieveScriptSetResponse> {
        self.connected
            .send_single::<SieveScriptSetResponse>(request)
            .await
            .map_err(map_error)
    }
}

/// The server's word on the app's script.
fn refused(error: jmap_client::Error) -> CoreError {
    match &error {
        jmap_client::Error::Set(set) => CoreError::Other(match set.error() {
            SetErrorType::OverQuota | SetErrorType::TooLarge => {
                "Your server has no room for another filter script".into()
            }
            _ => match set.description() {
                Some(said) => format!(
                    "Your server refused Flectar Mail's script. It said \u{201c}{said}\u{201d}"
                ),
                None => "Your server refused Flectar Mail's script".into(),
            },
        }),
        _ => map_error(error),
    }
}
