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
pub mod set;

use std::fmt::Display;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::core::changes::ChangesObject;
use crate::core::set::list_not_set;
use crate::core::Object;
use crate::{DataType, Get, Set};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushSubscription<State = Get> {
    #[serde(skip)]
    _create_id: Option<usize>,

    #[serde(skip)]
    _state: std::marker::PhantomData<State>,

    #[serde(rename = "id")]
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,

    #[serde(rename = "deviceClientId")]
    #[serde(skip_serializing_if = "Option::is_none")]
    device_client_id: Option<String>,

    #[serde(rename = "url")]
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,

    #[serde(rename = "keys")]
    #[serde(skip_serializing_if = "Option::is_none")]
    keys: Option<Keys>,

    #[serde(rename = "verificationCode")]
    #[serde(skip_serializing_if = "Option::is_none")]
    verification_code: Option<String>,

    #[serde(rename = "expires")]
    #[serde(skip_serializing_if = "Option::is_none")]
    expires: Option<DateTime<Utc>>,

    #[serde(rename = "types")]
    #[serde(skip_serializing_if = "list_not_set")]
    types: Option<Vec<DataType>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, Copy)]
pub enum Property {
    #[serde(rename = "id")]
    Id,
    #[serde(rename = "deviceClientId")]
    DeviceClientId,
    #[serde(rename = "url")]
    Url,
    #[serde(rename = "keys")]
    Keys,
    #[serde(rename = "verificationCode")]
    VerificationCode,
    #[serde(rename = "expires")]
    Expires,
    #[serde(rename = "types")]
    Types,
}

impl Display for Property {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Property::Id => write!(f, "id"),
            Property::DeviceClientId => write!(f, "deviceClientId"),
            Property::Url => write!(f, "url"),
            Property::Keys => write!(f, "keys"),
            Property::VerificationCode => write!(f, "verificationCode"),
            Property::Expires => write!(f, "expires"),
            Property::Types => write!(f, "types"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Keys {
    p256dh: String,
    auth: String,
}

impl Object for PushSubscription<Set> {
    type Property = Property;

    fn requires_account_id() -> bool {
        false
    }
}

impl Object for PushSubscription<Get> {
    type Property = Property;

    fn requires_account_id() -> bool {
        false
    }
}

impl ChangesObject for PushSubscription<Set> {
    type ChangesResponse = ();
}

impl ChangesObject for PushSubscription<Get> {
    type ChangesResponse = ();
}
