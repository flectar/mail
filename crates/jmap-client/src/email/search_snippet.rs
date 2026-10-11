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

use crate::core::{query::Filter, request::ResultReference, RequestParams};

#[derive(Deserialize, Clone, Debug)]
pub struct SearchSnippet {
    #[serde(rename = "emailId")]
    email_id: String,
    subject: Option<String>,
    preview: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchSnippetGetRequest {
    #[serde(rename = "accountId")]
    account_id: String,

    #[serde(rename = "filter")]
    #[serde(skip_serializing_if = "Option::is_none")]
    filter: Option<Filter<super::query::Filter>>,

    #[serde(rename = "emailIds")]
    #[serde(skip_serializing_if = "Option::is_none")]
    email_ids: Option<Vec<String>>,

    #[serde(rename = "#emailIds")]
    #[serde(skip_serializing_if = "Option::is_none")]
    email_ids_ref: Option<ResultReference>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SearchSnippetGetResponse {
    #[serde(rename = "accountId")]
    account_id: String,

    #[serde(rename = "list")]
    list: Vec<SearchSnippet>,

    #[serde(rename = "notFound")]
    not_found: Option<Vec<String>>,
}

impl SearchSnippetGetRequest {
    pub fn new(params: RequestParams) -> Self {
        SearchSnippetGetRequest {
            account_id: params.account_id,
            filter: None,
            email_ids: None,
            email_ids_ref: None,
        }
    }

    pub fn filter(&mut self, filter: impl Into<Filter<super::query::Filter>>) -> &mut Self {
        self.filter = Some(filter.into());
        self
    }

    pub fn email_id(&mut self, email_id: impl Into<String>) -> &mut Self {
        self.email_ids
            .get_or_insert_with(Vec::new)
            .push(email_id.into());
        self
    }

    pub fn email_ids(
        &mut self,
        email_ids: impl IntoIterator<Item = impl Into<String>>,
    ) -> &mut Self {
        self.email_ids
            .get_or_insert_with(Vec::new)
            .extend(email_ids.into_iter().map(|id| id.into()));
        self
    }

    pub fn email_ids_ref(&mut self, reference: ResultReference) -> &mut Self {
        self.email_ids_ref = reference.into();
        self.email_ids = None;
        self
    }
}

impl SearchSnippet {
    pub fn email_id(&self) -> &str {
        &self.email_id
    }

    pub fn subject(&self) -> Option<&str> {
        self.subject.as_deref()
    }

    pub fn preview(&self) -> Option<&str> {
        self.preview.as_deref()
    }
}

impl SearchSnippetGetResponse {
    pub fn account_id(&self) -> &str {
        &self.account_id
    }

    pub fn snippet(&self, id: &str) -> Option<&SearchSnippet> {
        self.list.iter().find(|snippet| snippet.email_id == id)
    }

    pub fn list(&self) -> &[SearchSnippet] {
        &self.list
    }

    pub fn not_found(&self) -> Option<&[String]> {
        self.not_found.as_deref()
    }
}
