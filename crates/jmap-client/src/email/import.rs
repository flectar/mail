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
    core::{
        request::ResultReference,
        set::{from_timestamp, SetError},
        RequestParams,
    },
    Error,
};
use ahash::AHashMap;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{Email, Property};

#[derive(Debug, Clone, Serialize)]
pub struct EmailImportRequest {
    #[serde(rename = "accountId")]
    account_id: String,

    #[serde(rename = "ifInState")]
    #[serde(skip_serializing_if = "Option::is_none")]
    if_in_state: Option<String>,

    emails: AHashMap<String, EmailImport>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EmailImport {
    #[serde(skip)]
    create_id: usize,

    #[serde(rename = "blobId")]
    blob_id: String,

    #[serde(rename = "mailboxIds")]
    #[serde(skip_serializing_if = "Option::is_none")]
    mailbox_ids: Option<AHashMap<String, bool>>,

    #[serde(rename = "#mailboxIds")]
    #[serde(skip_deserializing)]
    #[serde(skip_serializing_if = "Option::is_none")]
    mailbox_ids_ref: Option<ResultReference>,

    #[serde(rename = "keywords")]
    keywords: AHashMap<String, bool>,

    #[serde(rename = "receivedAt")]
    #[serde(skip_serializing_if = "Option::is_none")]
    received_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EmailImportResponse {
    #[serde(rename = "accountId")]
    account_id: String,

    #[serde(rename = "oldState")]
    old_state: Option<String>,

    #[serde(rename = "newState")]
    new_state: String,

    #[serde(rename = "created")]
    created: Option<AHashMap<String, Email>>,

    #[serde(rename = "notCreated")]
    not_created: Option<AHashMap<String, SetError<Property>>>,
}

impl EmailImportRequest {
    pub fn new(params: RequestParams) -> Self {
        EmailImportRequest {
            account_id: params.account_id,
            if_in_state: None,
            emails: AHashMap::new(),
        }
    }

    pub fn account_id(&mut self, account_id: impl Into<String>) -> &mut Self {
        self.account_id = account_id.into();
        self
    }

    pub fn if_in_state(&mut self, if_in_state: impl Into<String>) -> &mut Self {
        self.if_in_state = Some(if_in_state.into());
        self
    }

    pub fn email(&mut self, blob_id: impl Into<String>) -> &mut EmailImport {
        let create_id = self.emails.len();
        let create_id_str = format!("i{}", create_id);
        self.emails.insert(
            create_id_str.clone(),
            EmailImport::new(blob_id.into(), create_id),
        );
        self.emails.get_mut(&create_id_str).unwrap()
    }
}

impl EmailImport {
    fn new(blob_id: String, create_id: usize) -> Self {
        EmailImport {
            create_id,
            blob_id,
            mailbox_ids: None,
            mailbox_ids_ref: None,
            keywords: AHashMap::new(),
            received_at: None,
        }
    }

    pub fn mailbox_ids<T, U>(&mut self, mailbox_ids: T) -> &mut Self
    where
        T: IntoIterator<Item = U>,
        U: Into<String>,
    {
        self.mailbox_ids = Some(mailbox_ids.into_iter().map(|s| (s.into(), true)).collect());
        self.mailbox_ids_ref = None;
        self
    }

    pub fn mailbox_ids_ref(&mut self, reference: ResultReference) -> &mut Self {
        self.mailbox_ids_ref = reference.into();
        self.mailbox_ids = None;
        self
    }

    pub fn keywords<T, U>(&mut self, keywords: T) -> &mut Self
    where
        T: IntoIterator<Item = U>,
        U: Into<String>,
    {
        self.keywords = keywords.into_iter().map(|s| (s.into(), true)).collect();
        self
    }

    pub fn received_at(&mut self, received_at: i64) -> &mut Self {
        self.received_at = Some(from_timestamp(received_at));
        self
    }

    pub fn create_id(&self) -> String {
        format!("i{}", self.create_id)
    }
}

impl EmailImportResponse {
    pub fn account_id(&self) -> &str {
        &self.account_id
    }

    pub fn old_state(&self) -> Option<&str> {
        self.old_state.as_deref()
    }

    pub fn new_state(&self) -> &str {
        &self.new_state
    }

    pub fn take_new_state(&mut self) -> String {
        std::mem::take(&mut self.new_state)
    }

    pub fn created(&mut self, id: &str) -> crate::Result<Email> {
        if let Some(result) = self.created.as_mut().and_then(|r| r.remove(id)) {
            Ok(result)
        } else if let Some(error) = self.not_created.as_mut().and_then(|r| r.remove(id)) {
            Err(error.to_string_error().into())
        } else {
            Err(Error::Internal(format!("Id {} not found.", id)))
        }
    }

    pub fn created_ids(&self) -> Option<impl Iterator<Item = &String>> {
        self.created.as_ref().map(|map| map.keys())
    }

    pub fn not_created_ids(&self) -> Option<impl Iterator<Item = &String>> {
        self.not_created.as_ref().map(|map| map.keys())
    }
}
