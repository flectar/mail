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

use crate::core::set::{list_not_set, map_not_set, string_not_set};
use ahash::AHashMap;
use serde::{Deserialize, Serialize};
use std::fmt::Display;

use crate::{
    core::{changes::ChangesObject, Object},
    Get, Set,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Principal<State = Get> {
    #[serde(skip)]
    _create_id: Option<usize>,

    #[serde(skip)]
    _state: std::marker::PhantomData<State>,

    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,

    #[serde(rename = "type")]
    #[serde(skip_serializing_if = "Option::is_none")]
    ptype: Option<Type>,

    #[serde(skip_serializing_if = "string_not_set")]
    name: Option<String>,

    #[serde(skip_serializing_if = "string_not_set")]
    description: Option<String>,

    #[serde(skip_serializing_if = "string_not_set")]
    email: Option<String>,

    #[serde(skip_serializing_if = "string_not_set")]
    timezone: Option<String>,

    #[serde(skip_serializing_if = "list_not_set")]
    capabilities: Option<Vec<String>>,

    #[serde(skip_serializing_if = "list_not_set")]
    aliases: Option<Vec<String>>,

    #[serde(skip_serializing_if = "string_not_set")]
    secret: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    dkim: Option<DKIM>,

    #[serde(skip_serializing_if = "Option::is_none")]
    quota: Option<u32>,

    #[serde(skip_serializing_if = "string_not_set")]
    picture: Option<String>,

    #[serde(skip_serializing_if = "list_not_set")]
    members: Option<Vec<String>>,

    #[serde(skip_serializing_if = "map_not_set")]
    acl: Option<AHashMap<String, Vec<ACL>>>,

    #[serde(flatten)]
    #[serde(skip_deserializing)]
    #[serde(skip_serializing_if = "Option::is_none")]
    property_patch: Option<AHashMap<String, bool>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, Copy)]
pub enum Property {
    #[serde(rename = "id")]
    Id = 0,
    #[serde(rename = "type")]
    Type = 1,
    #[serde(rename = "name")]
    Name = 2,
    #[serde(rename = "description")]
    Description = 3,
    #[serde(rename = "email")]
    Email = 4,
    #[serde(rename = "timezone")]
    Timezone = 5,
    #[serde(rename = "capabilities")]
    Capabilities = 6,
    #[serde(rename = "aliases")]
    Aliases = 7,
    #[serde(rename = "secret")]
    Secret = 8,
    #[serde(rename = "dkim")]
    DKIM = 9,
    #[serde(rename = "quota")]
    Quota = 10,
    #[serde(rename = "picture")]
    Picture = 11,
    #[serde(rename = "members")]
    Members = 12,
    #[serde(rename = "shareWith")]
    ShareWith = 13,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, Copy)]
pub enum ACL {
    #[serde(rename = "mayRename")]
    Rename = 1,
    #[serde(rename = "mayDelete")]
    Delete = 2,
    #[serde(rename = "mayReadItems")]
    ReadItems = 3,
    #[serde(rename = "mayAddItems")]
    AddItems = 4,
    #[serde(rename = "maySetKeywords")]
    SetKeywords = 5,
    #[serde(rename = "mayRemoveItems")]
    RemoveItems = 6,
    #[serde(rename = "mayCreateChild")]
    CreateChild = 7,
    #[serde(rename = "mayShare")]
    Administer = 8,
    #[serde(rename = "maySubmit")]
    Submit = 10,
    #[serde(rename = "maySetSeen")]
    SetSeen = 11,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Type {
    #[serde(rename = "individual")]
    Individual,
    #[serde(rename = "group")]
    Group,
    #[serde(rename = "resource")]
    Resource,
    #[serde(rename = "location")]
    Location,
    #[serde(rename = "domain")]
    Domain,
    #[serde(rename = "list")]
    List,
    #[serde(rename = "other")]
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DKIM {
    #[serde(rename = "dkimSelector")]
    dkim_selector: Option<String>,
    #[serde(rename = "dkimExpiration")]
    dkim_expiration: Option<i64>,
}

impl DKIM {
    pub fn new(dkim_selector: Option<impl Into<String>>, dkim_expiration: Option<i64>) -> DKIM {
        DKIM {
            dkim_selector: dkim_selector.map(Into::into),
            dkim_expiration,
        }
    }

    pub fn selector(&self) -> Option<&str> {
        self.dkim_selector.as_deref()
    }

    pub fn expiration(&self) -> Option<i64> {
        self.dkim_expiration
    }
}

impl Display for Property {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Property::Id => write!(f, "id"),
            Property::Type => write!(f, "type"),
            Property::Name => write!(f, "name"),
            Property::Description => write!(f, "description"),
            Property::Email => write!(f, "email"),
            Property::Timezone => write!(f, "timezone"),
            Property::Capabilities => write!(f, "capabilities"),
            Property::Aliases => write!(f, "aliases"),
            Property::Secret => write!(f, "secret"),
            Property::DKIM => write!(f, "dkim"),
            Property::Quota => write!(f, "quota"),
            Property::Picture => write!(f, "picture"),
            Property::Members => write!(f, "members"),
            Property::ShareWith => write!(f, "shareWith"),
        }
    }
}

impl Display for ACL {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ACL::Rename => write!(f, "rename"),
            ACL::Delete => write!(f, "delete"),
            ACL::ReadItems => write!(f, "readItems"),
            ACL::AddItems => write!(f, "addItems"),
            ACL::SetKeywords => write!(f, "setKeywords"),
            ACL::RemoveItems => write!(f, "removeItems"),
            ACL::CreateChild => write!(f, "createChild"),
            ACL::Administer => write!(f, "administer"),
            ACL::Submit => write!(f, "submit"),
            ACL::SetSeen => write!(f, "setSeen"),
        }
    }
}

impl Object for Principal<Set> {
    type Property = Property;

    fn requires_account_id() -> bool {
        true
    }
}

impl Object for Principal<Get> {
    type Property = Property;

    fn requires_account_id() -> bool {
        true
    }
}

impl ChangesObject for Principal<Set> {
    type ChangesResponse = ();
}

impl ChangesObject for Principal<Get> {
    type ChangesResponse = ();
}
