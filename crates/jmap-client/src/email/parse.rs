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

use serde::{Deserialize, Serialize};

use super::{BodyProperty, Email, Property};
use crate::{core::RequestParams, Error};
use ahash::AHashMap;

#[derive(Debug, Clone, Serialize)]
pub struct EmailParseRequest {
    #[serde(rename = "accountId")]
    account_id: String,

    #[serde(rename = "blobIds")]
    blob_ids: Vec<String>,

    #[serde(rename = "properties")]
    #[serde(skip_serializing_if = "Option::is_none")]
    properties: Option<Vec<Property>>,

    #[serde(rename = "bodyProperties")]
    #[serde(skip_serializing_if = "Option::is_none")]
    body_properties: Option<Vec<BodyProperty>>,

    #[serde(rename = "fetchTextBodyValues")]
    #[serde(skip_serializing_if = "Option::is_none")]
    fetch_text_body_values: Option<bool>,

    #[serde(rename = "fetchHTMLBodyValues")]
    #[serde(skip_serializing_if = "Option::is_none")]
    fetch_html_body_values: Option<bool>,

    #[serde(rename = "fetchAllBodyValues")]
    #[serde(skip_serializing_if = "Option::is_none")]
    fetch_all_body_values: Option<bool>,

    #[serde(rename = "maxBodyValueBytes")]
    #[serde(skip_serializing_if = "Option::is_none")]
    max_body_value_bytes: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EmailParseResponse {
    #[serde(rename = "accountId")]
    account_id: String,

    #[serde(rename = "parsed")]
    parsed: Option<AHashMap<String, Email>>,

    #[serde(rename = "notParsable")]
    not_parsable: Option<Vec<String>>,

    #[serde(rename = "notFound")]
    not_found: Option<Vec<String>>,
}

impl EmailParseRequest {
    pub fn new(params: RequestParams) -> Self {
        EmailParseRequest {
            account_id: params.account_id,
            blob_ids: Vec::new(),
            properties: None,
            body_properties: None,
            fetch_text_body_values: None,
            fetch_html_body_values: None,
            fetch_all_body_values: None,
            max_body_value_bytes: None,
        }
    }

    pub fn blob_ids<U, V>(&mut self, blob_ids: U) -> &mut Self
    where
        U: IntoIterator<Item = V>,
        V: Into<String>,
    {
        self.blob_ids = blob_ids.into_iter().map(|v| v.into()).collect();
        self
    }

    pub fn properties(&mut self, properties: impl IntoIterator<Item = Property>) -> &mut Self {
        self.properties = Some(properties.into_iter().collect());
        self
    }

    pub fn body_properties(
        &mut self,
        body_properties: impl IntoIterator<Item = BodyProperty>,
    ) -> &mut Self {
        self.body_properties = Some(body_properties.into_iter().collect());
        self
    }

    pub fn fetch_text_body_values(&mut self, fetch_text_body_values: bool) -> &mut Self {
        self.fetch_text_body_values = fetch_text_body_values.into();
        self
    }

    pub fn fetch_html_body_values(&mut self, fetch_html_body_values: bool) -> &mut Self {
        self.fetch_html_body_values = fetch_html_body_values.into();
        self
    }

    pub fn fetch_all_body_values(&mut self, fetch_all_body_values: bool) -> &mut Self {
        self.fetch_all_body_values = fetch_all_body_values.into();
        self
    }

    pub fn max_body_value_bytes(&mut self, max_body_value_bytes: usize) -> &mut Self {
        self.max_body_value_bytes = max_body_value_bytes.into();
        self
    }
}

impl EmailParseResponse {
    pub fn account_id(&self) -> &str {
        &self.account_id
    }

    pub fn parsed(&mut self, blob_id: &str) -> crate::Result<Email> {
        if let Some(result) = self.parsed.as_mut().and_then(|r| r.remove(blob_id)) {
            Ok(result)
        } else if self
            .not_parsable
            .as_ref()
            .map(|np| np.iter().any(|id| id == blob_id))
            .unwrap_or(false)
        {
            Err(Error::Internal(format!(
                "blobId {} is not parsable.",
                blob_id
            )))
        } else {
            Err(Error::Internal(format!("blobId {} not found.", blob_id)))
        }
    }

    pub fn parsed_list(&self) -> Option<impl Iterator<Item = (&String, &Email)>> {
        self.parsed.as_ref().map(|map| map.iter())
    }

    pub fn not_parsable(&self) -> Option<&[String]> {
        self.not_parsable.as_deref()
    }

    pub fn not_found(&self) -> Option<&[String]> {
        self.not_found.as_deref()
    }
}
