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

use crate::Method;

use super::{request::ResultReference, Object, RequestParams};

pub trait GetObject: Object {
    type GetArguments: Default;
}

#[derive(Debug, Clone, Serialize)]
pub struct GetRequest<O: GetObject> {
    #[serde(skip)]
    method: (Method, usize),

    #[serde(rename = "accountId")]
    #[serde(skip_serializing_if = "Option::is_none")]
    account_id: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    ids: Option<Vec<String>>,

    #[serde(rename = "#ids")]
    #[serde(skip_deserializing)]
    #[serde(skip_serializing_if = "Option::is_none")]
    ids_ref: Option<ResultReference>,

    #[serde(skip_serializing_if = "Option::is_none")]
    properties: Option<Vec<O::Property>>,

    #[serde(rename = "#properties")]
    #[serde(skip_deserializing)]
    #[serde(skip_serializing_if = "Option::is_none")]
    properties_ref: Option<ResultReference>,

    #[serde(flatten)]
    arguments: O::GetArguments,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GetResponse<T> {
    #[serde(rename = "accountId")]
    account_id: Option<String>,

    state: String,

    list: Vec<T>,

    #[serde(rename = "notFound")]
    not_found: Vec<String>,
}

impl<O: GetObject> GetRequest<O> {
    pub fn new(params: RequestParams) -> Self {
        GetRequest {
            account_id: if O::requires_account_id() {
                params.account_id.into()
            } else {
                None
            },
            method: (params.method, params.call_id),
            ids: None,
            ids_ref: None,
            properties: None,
            properties_ref: None,
            arguments: O::GetArguments::default(),
        }
    }

    pub fn account_id(&mut self, account_id: impl Into<String>) -> &mut Self {
        if O::requires_account_id() {
            self.account_id = Some(account_id.into());
        }
        self
    }

    pub fn ids<U, V>(&mut self, ids: U) -> &mut Self
    where
        U: IntoIterator<Item = V>,
        V: Into<String>,
    {
        self.ids = Some(ids.into_iter().map(|v| v.into()).collect());
        self.ids_ref = None;
        self
    }

    pub fn ids_ref(&mut self, reference: ResultReference) -> &mut Self {
        self.ids_ref = reference.into();
        self.ids = None;
        self
    }

    pub fn properties(&mut self, properties: impl IntoIterator<Item = O::Property>) -> &mut Self {
        self.properties = Some(properties.into_iter().collect());
        self.properties_ref = None;
        self
    }

    pub fn properties_ref(&mut self, reference: ResultReference) -> &mut Self {
        self.properties_ref = Some(reference);
        self.properties = None;
        self
    }

    pub fn arguments(&mut self) -> &mut O::GetArguments {
        &mut self.arguments
    }

    pub fn result_reference(&self, property: O::Property) -> ResultReference {
        ResultReference::new(
            self.method.0,
            self.method.1,
            format!("/list/*/{}", property),
        )
    }
}

impl<O> GetResponse<O> {
    pub fn account_id(&self) -> Option<&str> {
        self.account_id.as_deref()
    }

    pub fn state(&self) -> &str {
        &self.state
    }

    pub fn take_state(&mut self) -> String {
        std::mem::take(&mut self.state)
    }

    pub fn list(&self) -> &[O] {
        &self.list
    }

    pub fn not_found(&self) -> &[String] {
        &self.not_found
    }

    pub fn take_list(&mut self) -> Vec<O> {
        std::mem::take(&mut self.list)
    }
    pub fn pop(&mut self) -> Option<O> {
        self.list.pop()
    }

    pub fn take_not_found(&mut self) -> Vec<String> {
        std::mem::take(&mut self.not_found)
    }
}
