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

use reqwest::header::CONTENT_TYPE;
use serde::Deserialize;

use crate::{client::Client, core::session::URLPart};

#[cfg(feature = "blocking")]
use reqwest::blocking::Client as HttpClient;
#[cfg(feature = "async")]
use reqwest::Client as HttpClient;

#[derive(Debug, Deserialize)]
pub struct UploadResponse {
    #[serde(rename = "accountId")]
    account_id: String,

    #[serde(rename = "blobId")]
    blob_id: String,

    #[serde(rename = "type")]
    type_: String,

    #[serde(rename = "size")]
    size: usize,
}

impl Client {
    #[maybe_async::maybe_async]
    pub async fn upload(
        &self,
        account_id: Option<&str>,
        blob: Vec<u8>,
        content_type: Option<&str>,
    ) -> crate::Result<UploadResponse> {
        let account_id = account_id.unwrap_or_else(|| self.default_account_id());
        let mut upload_url =
            String::with_capacity(self.session().upload_url().len() + account_id.len());

        for part in self.upload_url() {
            match part {
                URLPart::Value(value) => {
                    upload_url.push_str(value);
                }
                URLPart::Parameter(param) => {
                    if let super::URLParameter::AccountId = param {
                        upload_url.push_str(account_id);
                    }
                }
            }
        }

        serde_json::from_slice::<UploadResponse>(
            &Client::handle_error(
                HttpClient::builder()
                    .timeout(self.timeout())
                    .danger_accept_invalid_certs(self.accept_invalid_certs)
                    .redirect(self.redirect_policy())
                    .default_headers(self.headers().clone())
                    .build()?
                    .post(upload_url)
                    .header(
                        CONTENT_TYPE,
                        content_type.unwrap_or("application/octet-stream"),
                    )
                    .body(blob)
                    .send()
                    .await?,
            )
            .await?
            .bytes()
            .await?,
        )
        .map_err(|err| err.into())
    }
}

impl UploadResponse {
    pub fn account_id(&self) -> &str {
        &self.account_id
    }

    pub fn blob_id(&self) -> &str {
        &self.blob_id
    }

    pub fn content_type(&self) -> &str {
        &self.type_
    }

    pub fn size(&self) -> usize {
        self.size
    }

    pub fn take_blob_id(&mut self) -> String {
        std::mem::take(&mut self.blob_id)
    }
}
