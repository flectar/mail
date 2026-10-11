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

pub mod get;
pub mod helpers;
pub mod query;
pub mod set;
pub mod validate;

use std::fmt::Display;

use crate::core::changes::ChangesObject;
use crate::core::Object;
use crate::Get;
use crate::Set;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SieveScript<State = Get> {
    #[serde(skip)]
    _create_id: Option<usize>,

    #[serde(skip)]
    _state: std::marker::PhantomData<State>,

    #[serde(rename = "id")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,

    #[serde(rename = "name")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    #[serde(rename = "blobId")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blob_id: Option<String>,

    #[serde(rename = "isActive")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_active: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct SetArguments {
    #[serde(rename = "onSuccessActivateScript")]
    #[serde(skip_serializing_if = "Option::is_none")]
    on_success_activate_script: Option<String>,
    #[serde(rename = "onSuccessDeactivateScript")]
    #[serde(skip_serializing_if = "Option::is_none")]
    on_success_deactivate_script: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, Copy)]
pub enum Property {
    #[serde(rename = "id")]
    Id,
    #[serde(rename = "name")]
    Name,
    #[serde(rename = "blobId")]
    BlobId,
    #[serde(rename = "isActive")]
    IsActive,
}

impl Display for Property {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Property::Id => write!(f, "id"),
            Property::Name => write!(f, "name"),
            Property::BlobId => write!(f, "blobId"),
            Property::IsActive => write!(f, "isActive"),
        }
    }
}

impl Object for SieveScript<Set> {
    type Property = Property;

    fn requires_account_id() -> bool {
        true
    }
}

impl Object for SieveScript<Get> {
    type Property = Property;

    fn requires_account_id() -> bool {
        true
    }
}

impl ChangesObject for SieveScript<Set> {
    type ChangesResponse = ();
}

impl ChangesObject for SieveScript<Get> {
    type ChangesResponse = ();
}
