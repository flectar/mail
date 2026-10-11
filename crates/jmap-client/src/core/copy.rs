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

use ahash::AHashMap;
use serde::{Deserialize, Serialize};

use crate::Error;

use super::{
    set::{SetError, SetObject},
    RequestParams,
};

#[derive(Debug, Clone, Serialize)]
pub struct CopyRequest<O: SetObject> {
    #[serde(rename = "fromAccountId")]
    from_account_id: String,

    #[serde(rename = "ifFromInState")]
    #[serde(skip_serializing_if = "Option::is_none")]
    if_from_in_state: Option<String>,

    #[serde(rename = "accountId")]
    account_id: String,

    #[serde(rename = "ifInState")]
    #[serde(skip_serializing_if = "Option::is_none")]
    if_in_state: Option<String>,

    #[serde(rename = "create")]
    create: AHashMap<String, O>,

    #[serde(rename = "onSuccessDestroyOriginal")]
    on_success_destroy_original: bool,

    #[serde(rename = "destroyFromIfInState")]
    #[serde(skip_serializing_if = "Option::is_none")]
    destroy_from_if_in_state: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CopyResponse<O: SetObject> {
    #[serde(rename = "fromAccountId")]
    from_account_id: String,

    #[serde(rename = "accountId")]
    account_id: String,

    #[serde(rename = "oldState")]
    old_state: Option<String>,

    #[serde(rename = "newState")]
    new_state: String,

    #[serde(rename = "created")]
    created: Option<AHashMap<String, O>>,

    #[serde(rename = "notCreated")]
    not_created: Option<AHashMap<String, SetError<O::Property>>>,
}

impl<T: SetObject> CopyRequest<T> {
    pub fn new(params: RequestParams, from_account_id: String) -> Self {
        CopyRequest {
            from_account_id,
            if_from_in_state: None,
            account_id: params.account_id,
            if_in_state: None,
            create: AHashMap::new(),
            on_success_destroy_original: false,
            destroy_from_if_in_state: None,
        }
    }

    pub fn account_id(&mut self, account_id: impl Into<String>) -> &mut Self {
        self.account_id = account_id.into();
        self
    }

    pub fn if_from_in_state(&mut self, if_from_in_state: impl Into<String>) -> &mut Self {
        self.if_from_in_state = Some(if_from_in_state.into());
        self
    }

    pub fn if_in_state(&mut self, if_in_state: impl Into<String>) -> &mut Self {
        self.if_in_state = Some(if_in_state.into());
        self
    }

    pub fn create(&mut self, id: impl Into<String>) -> &mut T {
        let id = id.into();
        let mut object = T::new(None);
        object.set_id(Some(id.clone()));
        self.create.insert(id.clone(), object);
        self.create.get_mut(&id).unwrap()
    }

    pub fn on_success_destroy_original(&mut self, on_success_destroy_original: bool) -> &mut Self {
        self.on_success_destroy_original = on_success_destroy_original;
        self
    }

    pub fn destroy_from_if_in_state(
        &mut self,
        destroy_from_if_in_state: impl Into<String>,
    ) -> &mut Self {
        self.destroy_from_if_in_state = Some(destroy_from_if_in_state.into());
        self
    }
}

impl<O: SetObject> CopyResponse<O> {
    pub fn from_account_id(&self) -> &str {
        &self.from_account_id
    }

    pub fn account_id(&self) -> &str {
        &self.account_id
    }

    pub fn old_state(&self) -> Option<&str> {
        self.old_state.as_deref()
    }

    pub fn new_state(&self) -> &str {
        &self.new_state
    }

    pub fn created(&mut self, id: &str) -> crate::Result<O> {
        if let Some(result) = self.created.as_mut().and_then(|r| r.remove(id)) {
            Ok(result)
        } else if let Some(error) = self.not_created.as_mut().and_then(|r| r.remove(id)) {
            Err(error.to_string_error().into())
        } else {
            Err(Error::Internal(format!("Id {} not found.", id)))
        }
    }

    pub fn take_created(&mut self) -> Option<Vec<O>> {
        self.created
            .take()
            .map(|map| map.into_iter().map(|(_, v)| v).collect())
    }

    pub fn created_ids(&self) -> Option<impl Iterator<Item = &String>> {
        self.created.as_ref().map(|map| map.keys())
    }

    pub fn not_created_ids(&self) -> Option<impl Iterator<Item = &String>> {
        self.not_created.as_ref().map(|map| map.keys())
    }
}
