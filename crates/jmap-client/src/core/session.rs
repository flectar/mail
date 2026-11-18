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
    email::{MailCapabilities, SubmissionCapabilities},
    URI,
};
use ahash::AHashMap;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    #[serde(rename = "capabilities")]
    capabilities: AHashMap<String, Capabilities>,

    #[serde(rename = "accounts")]
    accounts: AHashMap<String, Account>,

    #[serde(rename = "primaryAccounts")]
    primary_accounts: AHashMap<String, String>,

    #[serde(rename = "username")]
    username: String,

    #[serde(rename = "apiUrl")]
    api_url: String,

    #[serde(rename = "downloadUrl")]
    download_url: String,

    #[serde(rename = "uploadUrl")]
    upload_url: String,

    #[serde(rename = "eventSourceUrl")]
    event_source_url: String,

    #[serde(rename = "state")]
    state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    #[serde(rename = "name")]
    name: String,

    #[serde(rename = "isPersonal")]
    is_personal: bool,

    #[serde(rename = "isReadOnly")]
    is_read_only: bool,

    #[serde(rename = "accountCapabilities")]
    account_capabilities: AHashMap<String, Capabilities>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Capabilities {
    Core(CoreCapabilities),
    Mail(MailCapabilities),
    Submission(SubmissionCapabilities),
    WebSocket(WebSocketCapabilities),
    Sieve(SieveCapabilities),
    Empty(EmptyCapabilities),
    Other(serde_json::Value),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreCapabilities {
    #[serde(rename = "maxSizeUpload")]
    max_size_upload: usize,

    #[serde(rename = "maxConcurrentUpload")]
    max_concurrent_upload: usize,

    #[serde(rename = "maxSizeRequest")]
    max_size_request: usize,

    #[serde(rename = "maxConcurrentRequests")]
    max_concurrent_requests: usize,

    #[serde(rename = "maxCallsInRequest")]
    max_calls_in_request: usize,

    #[serde(rename = "maxObjectsInGet")]
    max_objects_in_get: usize,

    #[serde(rename = "maxObjectsInSet")]
    max_objects_in_set: usize,

    #[serde(rename = "collationAlgorithms")]
    collation_algorithms: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSocketCapabilities {
    #[serde(rename = "url")]
    url: String,
    #[serde(rename = "supportsPush")]
    supports_push: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SieveCapabilities {
    #[serde(rename = "implementation")]
    implementation: Option<String>,
    #[serde(rename = "maxSizeScriptName")]
    max_script_name: Option<usize>,
    #[serde(rename = "maxSizeScript")]
    max_script_size: Option<usize>,
    #[serde(rename = "maxNumberScripts")]
    max_scripts: Option<usize>,
    #[serde(rename = "maxNumberRedirects")]
    max_redirects: Option<usize>,
    #[serde(rename = "sieveExtensions")]
    extensions: Vec<String>,
    #[serde(rename = "notificationMethods")]
    notification_methods: Option<Vec<String>>,
    #[serde(rename = "externalLists")]
    ext_lists: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmptyCapabilities {}

impl Session {
    pub fn capabilities(&self) -> impl Iterator<Item = &String> {
        self.capabilities.keys()
    }

    pub fn capability(&self, capability: impl AsRef<str>) -> Option<&Capabilities> {
        self.capabilities.get(capability.as_ref())
    }

    pub fn has_capability(&self, capability: impl AsRef<str>) -> bool {
        self.capabilities.contains_key(capability.as_ref())
    }

    pub fn websocket_capabilities(&self) -> Option<&WebSocketCapabilities> {
        self.capabilities
            .get(URI::WebSocket.as_ref())
            .and_then(|v| match v {
                Capabilities::WebSocket(capabilities) => Some(capabilities),
                _ => None,
            })
    }

    pub fn core_capabilities(&self) -> Option<&CoreCapabilities> {
        self.capabilities
            .get(URI::Core.as_ref())
            .and_then(|v| match v {
                Capabilities::Core(capabilities) => Some(capabilities),
                _ => None,
            })
    }

    pub fn mail_capabilities(&self) -> Option<&MailCapabilities> {
        self.capabilities
            .get(URI::Mail.as_ref())
            .and_then(|v| match v {
                Capabilities::Mail(capabilities) => Some(capabilities),
                _ => None,
            })
    }

    pub fn submission_capabilities(&self) -> Option<&SubmissionCapabilities> {
        self.capabilities
            .get(URI::Submission.as_ref())
            .and_then(|v| match v {
                Capabilities::Submission(capabilities) => Some(capabilities),
                _ => None,
            })
    }

    pub fn sieve_capabilities(&self) -> Option<&SieveCapabilities> {
        self.capabilities
            .get(URI::Sieve.as_ref())
            .and_then(|v| match v {
                Capabilities::Sieve(capabilities) => Some(capabilities),
                _ => None,
            })
    }

    pub fn accounts(&self) -> impl Iterator<Item = &String> {
        self.accounts.keys()
    }

    pub fn account(&self, account: &str) -> Option<&Account> {
        self.accounts.get(account)
    }

    pub fn primary_accounts(&self) -> impl Iterator<Item = (&String, &String)> {
        self.primary_accounts.iter()
    }

    pub fn username(&self) -> &str {
        &self.username
    }

    pub fn api_url(&self) -> &str {
        &self.api_url
    }

    pub fn download_url(&self) -> &str {
        &self.download_url
    }

    pub fn upload_url(&self) -> &str {
        &self.upload_url
    }

    pub fn event_source_url(&self) -> &str {
        &self.event_source_url
    }

    pub fn state(&self) -> &str {
        &self.state
    }
}

impl Account {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn is_personal(&self) -> bool {
        self.is_personal
    }

    pub fn is_read_only(&self) -> bool {
        self.is_read_only
    }

    pub fn capabilities(&self) -> impl Iterator<Item = &String> {
        self.account_capabilities.keys()
    }

    pub fn capability(&self, capability: &str) -> Option<&Capabilities> {
        self.account_capabilities.get(capability)
    }
}

impl CoreCapabilities {
    pub fn max_size_upload(&self) -> usize {
        self.max_size_upload
    }

    pub fn max_concurrent_upload(&self) -> usize {
        self.max_concurrent_upload
    }

    pub fn max_size_request(&self) -> usize {
        self.max_size_request
    }

    pub fn max_concurrent_requests(&self) -> usize {
        self.max_concurrent_requests
    }

    pub fn max_calls_in_request(&self) -> usize {
        self.max_calls_in_request
    }

    pub fn max_objects_in_get(&self) -> usize {
        self.max_objects_in_get
    }

    pub fn max_objects_in_set(&self) -> usize {
        self.max_objects_in_set
    }

    pub fn collation_algorithms(&self) -> &[String] {
        &self.collation_algorithms
    }
}

impl WebSocketCapabilities {
    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn supports_push(&self) -> bool {
        self.supports_push
    }
}

impl SieveCapabilities {
    pub fn max_script_name_size(&self) -> usize {
        self.max_script_name.unwrap_or(512)
    }

    pub fn max_script_size(&self) -> Option<usize> {
        self.max_script_size
    }

    pub fn max_number_scripts(&self) -> Option<usize> {
        self.max_scripts
    }

    pub fn max_number_redirects(&self) -> Option<usize> {
        self.max_redirects
    }

    pub fn sieve_extensions(&self) -> &[String] {
        &self.extensions
    }

    pub fn notification_methods(&self) -> Option<&[String]> {
        self.notification_methods.as_deref()
    }

    pub fn external_lists(&self) -> Option<&[String]> {
        self.ext_lists.as_deref()
    }
}

pub trait URLParser: Sized {
    fn parse(value: &str) -> Option<Self>;
}

pub enum URLPart<T: URLParser> {
    Value(String),
    Parameter(T),
}

impl<T: URLParser> URLPart<T> {
    pub fn parse(url: &str) -> crate::Result<Vec<URLPart<T>>> {
        let mut parts = Vec::new();
        let mut buf = String::with_capacity(url.len());
        let mut in_parameter = false;

        for ch in url.chars() {
            match ch {
                '{' => {
                    if !buf.is_empty() {
                        parts.push(URLPart::Value(buf.clone()));
                        buf.clear();
                    }
                    in_parameter = true;
                }
                '}' => {
                    if in_parameter && !buf.is_empty() {
                        parts.push(URLPart::Parameter(T::parse(&buf).ok_or_else(|| {
                            crate::Error::Internal(format!(
                                "Invalid parameter '{}' in URL: {}",
                                buf, url
                            ))
                        })?));
                        buf.clear();
                    } else {
                        return Err(crate::Error::Internal(format!("Invalid URL: {}", url)));
                    }
                    in_parameter = false;
                }
                _ => {
                    buf.push(ch);
                }
            }
        }

        if !buf.is_empty() {
            if !in_parameter {
                parts.push(URLPart::Value(buf.clone()));
            } else {
                return Err(crate::Error::Internal(format!("Invalid URL: {}", url)));
            }
        }

        Ok(parts)
    }
}
